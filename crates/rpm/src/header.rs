//! The parts of an rpm header osreflog needs (name, epoch, version, release, arch), parsed
//! from the blob rpm stores in `rpmdb.sqlite`: big-endian `il` (entry count), `dl` (data
//! length), `il` 16-byte entries (tag, type, offset, count), then the data store.

use crate::RpmError;

const TAG_NAME: u32 = 1000;
const TAG_VERSION: u32 = 1001;
const TAG_RELEASE: u32 = 1002;
const TAG_EPOCH: u32 = 1003;
const TAG_ARCH: u32 = 1022;
const TYPE_INT32: u32 = 4;
const TYPE_STRING: u32 = 6;
const MAX_ENTRIES: usize = 1 << 20;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Package {
    pub name: String,
    pub epoch: Option<u32>,
    pub version: String,
    pub release: String,
    /// `(none)` for packages without an arch (gpg-pubkey).
    pub arch: String,
}

impl Package {
    /// `[epoch:]version-release`, epoch shown only when non-zero (as dnf does).
    #[must_use]
    pub fn evr(&self) -> String {
        match self.epoch {
            Some(e) if e != 0 => format!("{e}:{}-{}", self.version, self.release),
            _ => format!("{}-{}", self.version, self.release),
        }
    }

    #[must_use]
    pub fn nevra(&self) -> String {
        format!("{}-{}.{}", self.name, self.evr(), self.arch)
    }
}

fn be32(b: &[u8], at: usize) -> Option<u32> {
    b.get(at..at + 4)
        .map(|s| u32::from_be_bytes([s[0], s[1], s[2], s[3]]))
}

/// Parses the package identity out of a header blob.
///
/// # Errors
/// If the blob is truncated or the identity tags are missing or mistyped.
pub fn parse(blob: &[u8]) -> Result<Package, RpmError> {
    let bad = |what: &'static str| RpmError::Header(what);
    let il = be32(blob, 0).ok_or(bad("truncated"))? as usize;
    let dl = be32(blob, 4).ok_or(bad("truncated"))? as usize;
    if il > MAX_ENTRIES {
        return Err(bad("too many entries"));
    }
    let store_start = 8 + il * 16;
    let store = blob
        .get(store_start..store_start + dl)
        .ok_or(bad("data store out of bounds"))?;

    let mut name = None;
    let mut version = None;
    let mut release = None;
    let mut epoch = None;
    let mut arch = None;
    for n in 0..il {
        let e = 8 + n * 16;
        let tag = be32(blob, e).ok_or(bad("truncated entry"))?;
        let ty = be32(blob, e + 4).ok_or(bad("truncated entry"))?;
        let off = be32(blob, e + 8).ok_or(bad("truncated entry"))? as usize;
        let string = || -> Result<String, RpmError> {
            if ty != TYPE_STRING {
                return Err(bad("identity tag is not a string"));
            }
            let rest = store.get(off..).ok_or(bad("string offset out of bounds"))?;
            let end = rest
                .iter()
                .position(|&b| b == 0)
                .ok_or(bad("unterminated string"))?;
            Ok(String::from_utf8_lossy(&rest[..end]).into_owned())
        };
        match tag {
            TAG_NAME => name = Some(string()?),
            TAG_VERSION => version = Some(string()?),
            TAG_RELEASE => release = Some(string()?),
            TAG_ARCH => arch = Some(string()?),
            TAG_EPOCH => {
                if ty != TYPE_INT32 {
                    return Err(bad("epoch is not an int32"));
                }
                epoch = Some(be32(store, off).ok_or(bad("epoch out of bounds"))?);
            }
            _ => {}
        }
    }
    Ok(Package {
        name: name.ok_or(bad("no name"))?,
        epoch,
        version: version.ok_or(bad("no version"))?,
        release: release.ok_or(bad("no release"))?,
        arch: arch.unwrap_or_else(|| "(none)".to_owned()),
    })
}

/// Builds a minimal header blob (tests here and in other crates).
#[must_use]
pub fn build(name: &str, epoch: Option<u32>, version: &str, release: &str, arch: &str) -> Vec<u8> {
    let mut entries: Vec<(u32, u32, Vec<u8>)> = vec![
        (TAG_NAME, TYPE_STRING, [name.as_bytes(), b"\0"].concat()),
        (
            TAG_VERSION,
            TYPE_STRING,
            [version.as_bytes(), b"\0"].concat(),
        ),
        (
            TAG_RELEASE,
            TYPE_STRING,
            [release.as_bytes(), b"\0"].concat(),
        ),
        (TAG_ARCH, TYPE_STRING, [arch.as_bytes(), b"\0"].concat()),
    ];
    if let Some(e) = epoch {
        entries.push((TAG_EPOCH, TYPE_INT32, e.to_be_bytes().to_vec()));
    }
    let mut index = Vec::new();
    let mut store = Vec::new();
    for (tag, ty, data) in entries {
        while ty == TYPE_INT32 && store.len() % 4 != 0 {
            store.push(0);
        }
        index.extend_from_slice(&tag.to_be_bytes());
        index.extend_from_slice(&ty.to_be_bytes());
        index.extend_from_slice(&u32::try_from(store.len()).unwrap_or(0).to_be_bytes());
        index.extend_from_slice(&1u32.to_be_bytes());
        store.extend_from_slice(&data);
    }
    let il = u32::try_from(index.len() / 16).unwrap_or(0);
    let dl = u32::try_from(store.len()).unwrap_or(0);
    [
        il.to_be_bytes().as_slice(),
        &dl.to_be_bytes(),
        &index,
        &store,
    ]
    .concat()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let p = parse(&build("bash", None, "5.3.9", "3.fc44", "x86_64")).unwrap();
        assert_eq!(p.nevra(), "bash-5.3.9-3.fc44.x86_64");
        let p = parse(&build(
            "openssl-devel",
            Some(1),
            "3.5.9",
            "1.fc44",
            "x86_64",
        ))
        .unwrap();
        assert_eq!(p.evr(), "1:3.5.9-1.fc44");
    }

    #[test]
    fn rejects_garbage() {
        assert!(parse(&[0, 0, 0, 1]).is_err());
        let mut blob = build("a", None, "1", "1", "noarch");
        blob.truncate(blob.len() - 3);
        assert!(parse(&blob).is_err());
    }
}
