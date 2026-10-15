//! Content of changed config files, kept so `why` can show what changed inside them.
//!
//! Only small text files are kept (≤ 1 MiB, no NUL bytes). Secrets never are: denylisted
//! paths, root-owned files nobody else may read, and anything holding a private key are
//! refused before reading or before storing. Blobs are deflated and named by the BLAKE3 of
//! their content: `blobs/ab/abcdef….z`.

use std::fs::{self, File};
use std::io::{Read, Write};
use std::os::fd::BorrowedFd;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use flate2::Compression;
use flate2::read::DeflateDecoder;
use flate2::write::DeflateEncoder;
use rustix::fs::{Mode, OFlags, openat};

pub const MAX_BLOB: u64 = 1 << 20;

pub use osreflog_model::secrets::{is_secret_mode, is_secret_path};

/// Whether content looks like a secret (private keys).
#[must_use]
pub fn is_secret_content(content: &[u8]) -> bool {
    const MARKERS: &[&[u8]] = &[b"PRIVATE KEY-----", b"BEGIN OPENSSH PRIVATE KEY"];
    MARKERS
        .iter()
        .any(|m| content.windows(m.len()).any(|w| w == *m))
}

pub struct Blobs {
    dir: PathBuf,
}

impl Blobs {
    /// # Errors
    /// If the directory can't be created.
    pub fn new(dir: &Path) -> std::io::Result<Self> {
        fs::create_dir_all(dir)?;
        Ok(Self {
            dir: dir.to_owned(),
        })
    }

    fn path_of(&self, hash: &str) -> Option<PathBuf> {
        if hash.len() != 64 || !hash.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        Some(self.dir.join(&hash[..2]).join(format!("{hash}.z")))
    }

    /// Stores `content` (already vetted) and returns its hash.
    ///
    /// # Errors
    /// On write errors.
    pub fn store(&self, content: &[u8]) -> std::io::Result<String> {
        let hash = blake3::hash(content).to_hex().to_string();
        let Some(path) = self.path_of(&hash) else {
            return Err(std::io::Error::other("bad hash"));
        };
        if path.exists() {
            return Ok(hash);
        }
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let tmp = path.with_extension("tmp");
        let mut enc = DeflateEncoder::new(File::create(&tmp)?, Compression::default());
        enc.write_all(content)?;
        enc.finish()?.sync_all()?;
        fs::rename(&tmp, &path)?;
        Ok(hash)
    }

    /// Reads, vets and stores the regular file `name` in the directory `dir` (never
    /// following symlinks). `path` is its full path, for the secret rules. `None` when the
    /// file is gone, not regular, too big, binary or secret.
    #[must_use]
    pub fn capture(
        &self,
        dir: BorrowedFd<'_>,
        name: &std::ffi::OsStr,
        path: &str,
    ) -> Option<(String, u64)> {
        if is_secret_path(path) {
            return None;
        }
        let fd = openat(
            dir,
            name,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK | OFlags::NOCTTY,
            Mode::empty(),
        )
        .ok()?;
        let mut file = File::from(fd);
        let meta = file.metadata().ok()?;
        if !meta.is_file() || meta.len() > MAX_BLOB || is_secret_mode(meta.uid(), meta.mode()) {
            return None;
        }
        let mut content = Vec::with_capacity(usize::try_from(meta.len()).unwrap_or(0));
        Read::by_ref(&mut file)
            .take(MAX_BLOB + 1)
            .read_to_end(&mut content)
            .ok()?;
        if content.len() as u64 > MAX_BLOB || content.contains(&0) || is_secret_content(&content) {
            return None;
        }
        let size = content.len() as u64;
        self.store(&content).ok().map(|h| (h, size))
    }

    /// A stored blob's content.
    #[must_use]
    pub fn read(&self, hash: &str) -> Option<Vec<u8>> {
        let file = File::open(self.path_of(hash)?).ok()?;
        let mut out = Vec::new();
        DeflateDecoder::new(file)
            .take(MAX_BLOB + 1)
            .read_to_end(&mut out)
            .ok()?;
        Some(out)
    }
}

/// Reads a text file directly (e.g. from a snapshot) under the same rules as [`Blobs::capture`].
#[must_use]
pub fn read_text(file: &Path, logical_path: &str) -> Option<String> {
    if is_secret_path(logical_path) {
        return None;
    }
    let meta = fs::symlink_metadata(file).ok()?;
    if !meta.is_file() || meta.len() > MAX_BLOB || is_secret_mode(meta.uid(), meta.mode()) {
        return None;
    }
    let content = fs::read(file).ok()?;
    if content.contains(&0) || is_secret_content(&content) {
        return None;
    }
    String::from_utf8(content).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::fd::AsFd;

    #[test]
    fn secret_rules() {
        for p in [
            "/etc/shadow",
            "/etc/gshadow-",
            "/etc/NetworkManager/system-connections/home.nmconnection",
            "/etc/ssh/ssh_host_ed25519_key",
            "/etc/pki/tls/private/x.pem",
            "/opt/app/server.key",
        ] {
            assert!(is_secret_path(p), "{p}");
        }
        for p in [
            "/etc/hosts",
            "/etc/ssh/ssh_host_ed25519_key.pub",
            "/etc/ssh/sshd_config",
        ] {
            assert!(!is_secret_path(p), "{p}");
        }
        assert!(is_secret_mode(0, 0o100_600));
        assert!(!is_secret_mode(0, 0o100_644));
        assert!(
            !is_secret_mode(0, 0o100_440),
            "sudoers-like group-readable files are kept"
        );
        assert!(!is_secret_mode(1000, 0o100_600));
        assert!(is_secret_content(b"x\n-----BEGIN RSA PRIVATE KEY-----\n"));
        assert!(!is_secret_content(b"127.0.0.1 localhost\n"));
    }

    #[test]
    fn capture_store_and_read() {
        let dir = tempfile::tempdir().unwrap();
        let blobs = Blobs::new(&dir.path().join("blobs")).unwrap();
        let etc = dir.path().join("etc");
        fs::create_dir(&etc).unwrap();
        fs::write(etc.join("hosts"), "127.0.0.1 localhost\n").unwrap();
        fs::write(etc.join("bin"), b"a\0b").unwrap();
        std::os::unix::fs::symlink("/etc/passwd", etc.join("link")).unwrap();
        let d = File::open(&etc).unwrap();

        let (hash, size) = blobs
            .capture(d.as_fd(), "hosts".as_ref(), "/etc/hosts")
            .unwrap();
        assert_eq!(size, 20);
        assert_eq!(blobs.read(&hash).unwrap(), b"127.0.0.1 localhost\n");
        assert_eq!(
            blobs
                .capture(d.as_fd(), "hosts".as_ref(), "/etc/hosts")
                .unwrap()
                .0,
            hash,
            "content-addressed"
        );
        assert!(
            blobs
                .capture(d.as_fd(), "bin".as_ref(), "/etc/bin")
                .is_none(),
            "binary"
        );
        assert!(
            blobs
                .capture(d.as_fd(), "link".as_ref(), "/etc/link")
                .is_none(),
            "symlinks are never followed"
        );
        assert!(
            blobs
                .capture(d.as_fd(), "hosts".as_ref(), "/etc/shadow")
                .is_none(),
            "secret by path"
        );
        assert!(
            blobs
                .capture(d.as_fd(), "missing".as_ref(), "/etc/missing")
                .is_none()
        );
        assert!(
            blobs.read("../../etc/passwd").is_none(),
            "hashes are validated"
        );
        assert_eq!(
            read_text(&etc.join("hosts"), "/etc/hosts").as_deref(),
            Some("127.0.0.1 localhost\n")
        );
    }
}
