//! Parsing fanotify events with FID information records, as returned by `read(2)` on a group
//! created with `FAN_REPORT_DFID_NAME`. Safe code over bytes; the layout follows
//! `<linux/fanotify.h>`:
//!
//! ```text
//! fanotify_event_metadata  u32 event_len, u8 vers, u8 reserved, u16 metadata_len,
//!                          u64 mask, i32 fd, i32 pid                       (24 bytes)
//! info header              u8 info_type, u8 pad, u16 len
//! fid info                 header, fsid (2 x i32), u32 handle_bytes, i32 handle_type,
//!                          handle bytes, [NUL-terminated name for *_DFID_NAME types]
//! ```

pub const METADATA_LEN: usize = 24;
pub const FANOTIFY_METADATA_VERSION: u8 = 3;

pub const FAN_ATTRIB: u64 = 0x0000_0004;
pub const FAN_CLOSE_WRITE: u64 = 0x0000_0008;
pub const FAN_CREATE: u64 = 0x0000_0100;
pub const FAN_DELETE: u64 = 0x0000_0200;
pub const FAN_Q_OVERFLOW: u64 = 0x0000_4000;
pub const FAN_ONDIR: u64 = 0x4000_0000;
pub const FAN_RENAME: u64 = 0x1000_0000;

pub const INFO_FID: u8 = 1;
pub const INFO_DFID_NAME: u8 = 2;
pub const INFO_DFID: u8 = 3;
pub const INFO_OLD_DFID_NAME: u8 = 10;
pub const INFO_NEW_DFID_NAME: u8 = 12;

/// A file handle as the kernel reports it, enough to reopen the object with
/// `open_by_handle_at`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Handle {
    pub handle_type: i32,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fid {
    pub info_type: u8,
    pub fsid: [i32; 2],
    pub handle: Handle,
    /// The entry name inside the directory, for `*_DFID_NAME` records ("." for the
    /// directory itself).
    pub name: Option<Vec<u8>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    pub mask: u64,
    pub pid: i32,
    pub fids: Vec<Fid>,
}

impl Event {
    /// The record of a given type (e.g. [`INFO_DFID_NAME`]).
    #[must_use]
    pub fn fid(&self, info_type: u8) -> Option<&Fid> {
        self.fids.iter().find(|f| f.info_type == info_type)
    }

    #[must_use]
    pub fn is_dir(&self) -> bool {
        self.mask & FAN_ONDIR != 0
    }

    #[must_use]
    pub fn is_overflow(&self) -> bool {
        self.mask & FAN_Q_OVERFLOW != 0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseError {
    Truncated(usize),
    Version(u8),
    BadLength(usize),
}

fn u16_at(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_ne_bytes(b.get(at..at + 2)?.try_into().ok()?))
}
fn u32_at(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_ne_bytes(b.get(at..at + 4)?.try_into().ok()?))
}
fn i32_at(b: &[u8], at: usize) -> Option<i32> {
    Some(i32::from_ne_bytes(b.get(at..at + 4)?.try_into().ok()?))
}
fn u64_at(b: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_ne_bytes(b.get(at..at + 8)?.try_into().ok()?))
}

fn parse_fid(info_type: u8, rec: &[u8]) -> Option<Fid> {
    // rec starts after the 4-byte info header.
    let fsid = [i32_at(rec, 0)?, i32_at(rec, 4)?];
    let handle_bytes = u32_at(rec, 8)? as usize;
    let handle_type = i32_at(rec, 12)?;
    let handle = rec.get(16..16 + handle_bytes)?.to_vec();
    let name = match info_type {
        INFO_DFID_NAME | INFO_OLD_DFID_NAME | INFO_NEW_DFID_NAME => {
            let rest = rec.get(16 + handle_bytes..)?;
            let end = rest.iter().position(|&b| b == 0)?;
            Some(rest[..end].to_vec())
        }
        _ => None,
    };
    Some(Fid {
        info_type,
        fsid,
        handle: Handle {
            handle_type,
            bytes: handle,
        },
        name,
    })
}

/// Parses every event in `buf` (one `read(2)` worth).
///
/// # Errors
/// On a truncated or malformed buffer.
pub fn parse(buf: &[u8]) -> Result<Vec<Event>, ParseError> {
    let mut out = Vec::new();
    let mut pos = 0;
    while pos < buf.len() {
        let ev = &buf[pos..];
        let len = u32_at(ev, 0).ok_or(ParseError::Truncated(pos))? as usize;
        let vers = *ev.get(4).ok_or(ParseError::Truncated(pos))?;
        let meta_len = u16_at(ev, 6).ok_or(ParseError::Truncated(pos))? as usize;
        if vers != FANOTIFY_METADATA_VERSION {
            return Err(ParseError::Version(vers));
        }
        if len < METADATA_LEN || meta_len < METADATA_LEN || meta_len > len || len > ev.len() {
            return Err(ParseError::BadLength(pos));
        }
        let mask = u64_at(ev, 8).ok_or(ParseError::Truncated(pos))?;
        let pid = i32_at(ev, 20).ok_or(ParseError::Truncated(pos))?;
        let mut fids = Vec::new();
        let mut ipos = meta_len;
        while ipos + 4 <= len {
            let info_type = ev[ipos];
            let info_len = u16_at(ev, ipos + 2).ok_or(ParseError::Truncated(pos))? as usize;
            if info_len < 4 || ipos + info_len > len {
                return Err(ParseError::BadLength(pos + ipos));
            }
            if matches!(
                info_type,
                INFO_FID | INFO_DFID_NAME | INFO_DFID | INFO_OLD_DFID_NAME | INFO_NEW_DFID_NAME
            ) && let Some(fid) = parse_fid(info_type, &ev[ipos + 4..ipos + info_len])
            {
                fids.push(fid);
            }
            ipos += info_len;
        }
        out.push(Event { mask, pid, fids });
        pos += len;
    }
    Ok(out)
}

/// Builds an event buffer the way the kernel lays it out (tests here and elsewhere).
#[must_use]
pub fn build(mask: u64, pid: i32, fids: &[Fid]) -> Vec<u8> {
    let mut infos = Vec::new();
    for f in fids {
        let mut rec = Vec::new();
        rec.extend_from_slice(&f.fsid[0].to_ne_bytes());
        rec.extend_from_slice(&f.fsid[1].to_ne_bytes());
        rec.extend_from_slice(
            &u32::try_from(f.handle.bytes.len())
                .unwrap_or(0)
                .to_ne_bytes(),
        );
        rec.extend_from_slice(&f.handle.handle_type.to_ne_bytes());
        rec.extend_from_slice(&f.handle.bytes);
        if let Some(name) = &f.name {
            rec.extend_from_slice(name);
            rec.push(0);
        }
        while (rec.len() + 4) % 4 != 0 {
            rec.push(0);
        }
        infos.push(f.info_type);
        infos.push(0);
        infos.extend_from_slice(&u16::try_from(rec.len() + 4).unwrap_or(0).to_ne_bytes());
        infos.extend_from_slice(&rec);
    }
    let len = u32::try_from(METADATA_LEN + infos.len()).unwrap_or(0);
    let mut ev = Vec::new();
    ev.extend_from_slice(&len.to_ne_bytes());
    ev.push(FANOTIFY_METADATA_VERSION);
    ev.push(0);
    ev.extend_from_slice(&u16::try_from(METADATA_LEN).unwrap_or(0).to_ne_bytes());
    ev.extend_from_slice(&mask.to_ne_bytes());
    ev.extend_from_slice(&(-1i32).to_ne_bytes()); // FAN_NOFD
    ev.extend_from_slice(&pid.to_ne_bytes());
    ev.extend_from_slice(&infos);
    ev
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dfid(info_type: u8, handle: &[u8], name: &str) -> Fid {
        Fid {
            info_type,
            fsid: [7, 9],
            handle: Handle {
                handle_type: 0x4d,
                bytes: handle.to_vec(),
            },
            name: Some(name.as_bytes().to_vec()),
        }
    }

    #[test]
    fn parses_close_write_and_rename() {
        let mut buf = build(
            FAN_CLOSE_WRITE,
            1234,
            &[dfid(INFO_DFID_NAME, &[1, 2, 3, 4, 5, 6, 7, 8], "hosts")],
        );
        buf.extend(build(
            FAN_RENAME,
            55,
            &[
                dfid(INFO_OLD_DFID_NAME, &[1; 8], "o257-1-0"),
                dfid(INFO_NEW_DFID_NAME, &[2; 12], "hosts"),
            ],
        ));
        let events = parse(&buf).unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].pid, 1234);
        let f = events[0].fid(INFO_DFID_NAME).unwrap();
        assert_eq!((f.fsid, f.name.as_deref()), ([7, 9], Some(&b"hosts"[..])));
        assert_eq!(f.handle.bytes, [1, 2, 3, 4, 5, 6, 7, 8]);
        assert_eq!(
            events[1]
                .fid(INFO_NEW_DFID_NAME)
                .unwrap()
                .handle
                .bytes
                .len(),
            12
        );
        assert_eq!(
            events[1].fid(INFO_OLD_DFID_NAME).unwrap().name.as_deref(),
            Some(&b"o257-1-0"[..])
        );
    }

    #[test]
    fn overflow_has_no_records() {
        let events = parse(&build(FAN_Q_OVERFLOW, -1, &[])).unwrap();
        assert!(events[0].is_overflow());
        assert_eq!(events[0].fids, []);
    }

    #[test]
    fn rejects_malformed_buffers() {
        let buf = build(
            FAN_CREATE | FAN_ONDIR,
            1,
            &[dfid(INFO_DFID_NAME, &[1; 8], "d")],
        );
        assert!(matches!(
            parse(&buf[..10]),
            Err(ParseError::Truncated(_) | ParseError::BadLength(_))
        ));
        let mut bad = buf.clone();
        bad[4] = 2;
        assert_eq!(parse(&bad), Err(ParseError::Version(2)));
        let mut bad = buf;
        bad[0] = 200; // longer than the buffer
        assert!(matches!(parse(&bad), Err(ParseError::BadLength(_))));
    }
}
