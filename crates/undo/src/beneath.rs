//! Writes into a directory tree that may hold hostile symlinks (a probe's clone of the live
//! system): every path is resolved from one pinned root fd under [`RESOLVE`], directories are
//! made one component at a time, and files are written to a temporary name and renamed.

use std::fs::File;
use std::io::{self, Write as _};
use std::os::fd::{AsFd as _, OwnedFd};
use std::path::{Path, PathBuf};

use rustix::fs::{AtFlags, Dir, Mode, OFlags, fchmod, mkdirat, openat2, unlinkat};
use rustix::io::Errno;

use crate::apply::RESOLVE;

#[derive(Debug)]
pub enum BeneathError {
    /// The root itself could not be opened.
    Open { path: PathBuf, source: io::Error },
    /// A path beneath the root was refused (a symlink or mount point on the way) or failed.
    Refused(String),
}

impl std::fmt::Display for BeneathError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BeneathError::Open { path, source } => write!(f, "{}: {source}", path.display()),
            BeneathError::Refused(what) => f.write_str(what),
        }
    }
}

impl std::error::Error for BeneathError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            BeneathError::Open { source, .. } => Some(source),
            BeneathError::Refused(_) => None,
        }
    }
}

impl From<BeneathError> for io::Error {
    fn from(e: BeneathError) -> io::Error {
        match e {
            BeneathError::Open { source, .. } => source,
            BeneathError::Refused(what) => io::Error::other(what),
        }
    }
}

/// A root directory, opened for writes beneath it.
pub struct Beneath(OwnedFd);

impl Beneath {
    /// # Errors
    /// [`BeneathError::Open`] if `root` isn't a directory that can be opened.
    pub fn open(root: &Path) -> Result<Beneath, BeneathError> {
        let fd = rustix::fs::open(
            root,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(|e| BeneathError::Open {
            path: root.to_owned(),
            source: e.into(),
        })?;
        Ok(Beneath(fd))
    }

    fn err(rel: &str, e: Errno) -> BeneathError {
        let what = if e == Errno::LOOP || e == Errno::XDEV {
            format!("{rel}: a symlink or mount point in the probe's path, refused")
        } else {
            format!("{rel}: {e}")
        };
        BeneathError::Refused(what)
    }

    /// Opens (creating as needed) the directory `rel`, one component at a time.
    ///
    /// # Errors
    /// [`BeneathError::Refused`] on a symlink or mount point on the way, or a failed `mkdir`.
    pub fn dir(&self, rel: &str) -> Result<OwnedFd, BeneathError> {
        let mut path = String::new();
        for part in rel.split('/').filter(|p| !p.is_empty()) {
            if !path.is_empty() {
                path.push('/');
            }
            path.push_str(part);
            match mkdirat(self.0.as_fd(), path.as_str(), Mode::from_raw_mode(0o755)) {
                Ok(()) | Err(Errno::EXIST) => {}
                Err(e) => return Err(Self::err(&path, e)),
            }
        }
        self.open_dir(rel)
    }

    fn open_dir(&self, rel: &str) -> Result<OwnedFd, BeneathError> {
        openat2(
            self.0.as_fd(),
            if rel.is_empty() { "." } else { rel },
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
            Mode::empty(),
            RESOLVE,
        )
        .map_err(|e| Self::err(rel, e))
    }

    fn split(rel: &str) -> (&str, &str) {
        rel.rsplit_once('/').unwrap_or(("", rel))
    }

    /// Replaces `rel` with `data` (a new inode: a hard link elsewhere keeps the old content).
    ///
    /// # Errors
    /// [`BeneathError::Refused`] on a symlink or mount point on the way, or a failed write.
    pub fn write(&self, rel: &str, data: &str, mode: u32) -> Result<(), BeneathError> {
        let (parent, name) = Self::split(rel);
        let dir = self.dir(parent)?;
        let tmp = format!(".{name}.osreflog-tmp");
        let _ = unlinkat(dir.as_fd(), tmp.as_str(), AtFlags::empty());
        let fd = openat2(
            dir.as_fd(),
            tmp.as_str(),
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::CLOEXEC,
            Mode::from_raw_mode(mode),
            RESOLVE,
        )
        .map_err(|e| Self::err(rel, e))?;
        // The umask must not decide the mode (a probe's test has to stay executable).
        fchmod(&fd, Mode::from_raw_mode(mode)).map_err(|e| Self::err(rel, e))?;
        let mut f = File::from(fd);
        f.write_all(data.as_bytes())
            .and_then(|()| f.sync_all())
            .map_err(|e| BeneathError::Refused(format!("{rel}: {e}")))?;
        rustix::fs::renameat(dir.as_fd(), tmp.as_str(), dir.as_fd(), name)
            .map_err(|e| Self::err(rel, e))
    }

    /// Removes the file `rel`; `false` if it wasn't there.
    ///
    /// # Errors
    /// [`BeneathError::Refused`] on a symlink or mount point on the way, or a failed unlink.
    pub fn remove(&self, rel: &str) -> Result<bool, BeneathError> {
        let (parent, name) = Self::split(rel);
        let Some(dir) = self.parent(parent)? else {
            return Ok(false);
        };
        match unlinkat(dir.as_fd(), name, AtFlags::empty()) {
            Ok(()) => Ok(true),
            Err(Errno::NOENT) => Ok(false),
            Err(e) => Err(Self::err(rel, e)),
        }
    }

    /// Removes `rel` and everything under it (symlinks in it are removed, not followed);
    /// `false` if it wasn't there.
    ///
    /// # Errors
    /// [`BeneathError::Refused`] on a symlink or mount point on the way, or a failed unlink.
    pub fn remove_all(&self, rel: &str) -> Result<bool, BeneathError> {
        let (parent, name) = Self::split(rel);
        let Some(dir) = self.parent(parent)? else {
            return Ok(false);
        };
        match unlinkat(dir.as_fd(), name, AtFlags::empty()) {
            Ok(()) => return Ok(true),
            Err(Errno::NOENT) => return Ok(false),
            Err(Errno::ISDIR) => {}
            Err(e) => return Err(Self::err(rel, e)),
        }
        Self::empty(&dir, name, rel)?;
        unlinkat(dir.as_fd(), name, AtFlags::REMOVEDIR).map_err(|e| Self::err(rel, e))?;
        Ok(true)
    }

    /// Empties the directory `name` in `parent`, depth first.
    fn empty(parent: &OwnedFd, name: &str, rel: &str) -> Result<(), BeneathError> {
        let fd = openat2(
            parent.as_fd(),
            name,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
            Mode::empty(),
            RESOLVE,
        )
        .map_err(|e| Self::err(rel, e))?;
        let names: Vec<String> = Dir::read_from(&fd)
            .map_err(|e| Self::err(rel, e))?
            .filter_map(Result::ok)
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n != "." && n != "..")
            .collect();
        for n in names {
            let child = format!("{rel}/{n}");
            match unlinkat(fd.as_fd(), n.as_str(), AtFlags::empty()) {
                Ok(()) | Err(Errno::NOENT) => {}
                Err(Errno::ISDIR) => {
                    Self::empty(&fd, &n, &child)?;
                    unlinkat(fd.as_fd(), n.as_str(), AtFlags::REMOVEDIR)
                        .map_err(|e| Self::err(&child, e))?;
                }
                Err(e) => return Err(Self::err(&child, e)),
            }
        }
        Ok(())
    }

    /// The directory `rel`, or `None` if it doesn't exist.
    fn parent(&self, rel: &str) -> Result<Option<OwnedFd>, BeneathError> {
        match self.open_dir(rel) {
            Ok(d) => Ok(Some(d)),
            Err(_) if !self.exists_dir(rel) => Ok(None),
            Err(e) => Err(e),
        }
    }

    fn exists_dir(&self, rel: &str) -> bool {
        rustix::fs::statat(self.0.as_fd(), rel, AtFlags::SYMLINK_NOFOLLOW).is_ok()
    }

    /// Makes `rel` a symlink to `target` (which is stored, never followed here).
    ///
    /// # Errors
    /// [`BeneathError::Refused`] on a symlink or mount point on the way, or a failed symlink.
    pub fn symlink(&self, target: &str, rel: &str) -> Result<(), BeneathError> {
        let (parent, name) = Self::split(rel);
        let dir = self.dir(parent)?;
        let _ = unlinkat(dir.as_fd(), name, AtFlags::empty());
        rustix::fs::symlinkat(target, dir.as_fd(), name).map_err(|e| Self::err(rel, e))
    }

    /// Reads the file `rel`.
    ///
    /// # Errors
    /// [`BeneathError::Refused`] on a symlink or mount point on the way, or a failed read.
    pub fn read(&self, rel: &str) -> Result<String, BeneathError> {
        let fd = openat2(
            self.0.as_fd(),
            rel,
            OFlags::RDONLY | OFlags::CLOEXEC,
            Mode::empty(),
            RESOLVE,
        )
        .map_err(|e| Self::err(rel, e))?;
        std::io::read_to_string(File::from(fd))
            .map_err(|e| BeneathError::Refused(format!("{rel}: {e}")))
    }
}
