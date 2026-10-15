//! Building bisect probes: a writable clone of a checkpoint, the replay of causes onto it, and
//! the test and the probe unit copied in. Probes live in `osreflog/tmp/`, which the daemon
//! clears at start, so a crash never leaves one behind for long.
//!
//! Replay writes only inside the clone, through `openat2` beneath its root (the clone holds
//! whatever the system held, symlinks included).

use std::fs::File;
use std::io::{self, Write as _};
use std::os::fd::{AsFd as _, BorrowedFd, OwnedFd};
use std::path::{Path, PathBuf};

use osreflog_bisect::{Action, Op};
use osreflog_btrfs::ioctl;
use osreflog_undo::Root;
use osreflog_undo::plan::{Meta, from_hex};
use rustix::fs::{AtFlags, Mode, OFlags, ResolveFlags, XattrFlags, mkdirat, openat2, symlinkat};
use rustix::io::Errno;
use serde::{Deserialize, Serialize};

use crate::blobs::Blobs;
use crate::engine::Engine;
use crate::undo::meta_of;

const RESOLVE: ResolveFlags = osreflog_undo::apply::RESOLVE;
/// Where the test, its output and its exit code live inside a probe.
pub const PROBE_DIR: &str = "var/lib/osreflog-probe";

const TARGET_UNIT: &str = "\
[Unit]
Description=osreflog bisect probe
Requires=basic.target osreflog-probe.service
After=basic.target osreflog-probe.service
AllowIsolate=yes
";

const SERVICE_UNIT: &str = "\
[Unit]
Description=osreflog bisect probe: run the test, record its exit code, power off
After=basic.target
DefaultDependencies=no
Requires=basic.target

[Service]
Type=oneshot
StandardOutput=journal+console
ExecStart=/bin/sh -c '/var/lib/osreflog-probe/test > /var/lib/osreflog-probe/output 2>&1; echo $? > /var/lib/osreflog-probe/result; sync'
SuccessAction=poweroff-force
FailureAction=poweroff-force
TimeoutStartSec=infinity
";

/// What replaying ops into a probe did.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Replayed {
    pub ops: usize,
    /// Paths whose version came from the bad checkpoint although a later cause touched them.
    pub approximate: Vec<String>,
    /// Paths the bad checkpoint no longer has and no blob could supply: left as they were.
    pub missing: Vec<String>,
}

fn io_other(e: impl std::fmt::Display) -> io::Error {
    io::Error::other(e.to_string())
}

/// A probe on disk: a writable subvolume in `osreflog/tmp/`.
pub struct Probe<'a> {
    engine: &'a Engine,
    pub name: String,
}

impl Probe<'_> {
    #[must_use]
    pub fn path(&self) -> PathBuf {
        self.engine.layout().tmp().join(&self.name)
    }

    /// The probe's path from the btrfs top level, for the runner's own mount.
    #[must_use]
    pub fn top_relative(&self) -> String {
        format!("osreflog/tmp/{}", self.name)
    }
}

impl Drop for Probe<'_> {
    fn drop(&mut self) {
        if let Err(e) = ioctl::delete_subvolume(self.engine.layout().tmp_fd(), &self.name) {
            tracing::warn!(probe = %self.name, "deleting the probe failed: {e}");
        }
    }
}

/// A writable clone of checkpoint snapshot `snapshot` named `name`.
///
/// # Errors
/// If the snapshot can't be opened or cloned.
pub fn clone_checkpoint<'a>(
    engine: &'a Engine,
    snapshot: &str,
    name: &str,
) -> io::Result<Probe<'a>> {
    let layout = engine.layout();
    let _ = ioctl::delete_subvolume(layout.tmp_fd(), name);
    let src = File::open(layout.snapshot_path(snapshot))?;
    ioctl::create_snapshot(src.as_fd(), layout.tmp_fd(), name, false)?;
    Ok(Probe {
        engine,
        name: name.to_owned(),
    })
}

fn split(path: &str) -> (&str, &str) {
    let rel = path.trim_start_matches('/');
    rel.rsplit_once('/').unwrap_or((".", rel))
}

fn parent(root: &Root, path: &str) -> io::Result<(OwnedFd, String)> {
    let (dir, name) = split(path);
    Ok((root.dir(dir)?, name.to_owned()))
}

/// Removes whatever is at `name` (a file, a link, or a directory tree).
fn remove(dir: BorrowedFd<'_>, name: &str, real: &Path) -> io::Result<()> {
    match rustix::fs::unlinkat(dir, name, AtFlags::empty()) {
        Ok(()) | Err(Errno::NOENT) => Ok(()),
        Err(Errno::ISDIR) => match rustix::fs::unlinkat(dir, name, AtFlags::REMOVEDIR) {
            Ok(()) | Err(Errno::NOENT) => Ok(()),
            // A non-empty directory: its parent was resolved beneath the root, and
            // remove_dir_all doesn't follow symlinks inside it.
            Err(Errno::NOTEMPTY) => std::fs::remove_dir_all(real),
            Err(e) => Err(e.into()),
        },
        Err(e) => Err(e.into()),
    }
}

fn set_meta(fd: BorrowedFd<'_>, meta: &Meta) {
    let _ = rustix::fs::fchown(
        fd,
        Some(rustix::process::Uid::from_raw(meta.uid)),
        Some(rustix::process::Gid::from_raw(meta.gid)),
    );
    let _ = rustix::fs::fchmod(fd, Mode::from_raw_mode(meta.mode));
    for (name, value) in &meta.xattrs {
        if let Some(v) = from_hex(value) {
            let _ = rustix::fs::fsetxattr(fd, name.as_str(), &v, XattrFlags::empty());
        }
    }
}

fn create_file(dir: BorrowedFd<'_>, name: &str) -> io::Result<File> {
    let fd = openat2(
        dir,
        name,
        OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::from_raw_mode(0o600),
        RESOLVE,
    )?;
    Ok(File::from(fd))
}

/// Applies replay ops to the probe at `probe`, taking versions from the bad checkpoint
/// `bad` and from the blob store.
///
/// # Errors
/// If the probe's root can't be opened. A single path that can't be replayed is listed in
/// `missing` instead.
pub fn replay(probe: &Path, bad: &Path, blobs: &Blobs, ops: &[Op]) -> io::Result<Replayed> {
    let root = Root::open(probe)?;
    let mut out = Replayed::default();
    for op in ops {
        out.ops += 1;
        if !op.exact {
            out.approximate.push(op.path.clone());
        }
        if let Err(e) = replay_one(&root, probe, bad, blobs, op) {
            tracing::debug!(path = %op.path, "replay: {e}");
            out.missing.push(op.path.clone());
        }
    }
    Ok(out)
}

fn replay_one(root: &Root, probe: &Path, bad: &Path, blobs: &Blobs, op: &Op) -> io::Result<()> {
    let rel = op.path.trim_start_matches('/');
    let (dir, name) = parent(root, &op.path)?;
    let dir = dir.as_fd();
    let in_bad = bad.join(rel);
    let bad_meta = std::fs::symlink_metadata(&in_bad).ok();
    let meta = meta_of(&in_bad)
        .or_else(|| meta_of(&probe.join(rel)))
        .unwrap_or(Meta {
            uid: 0,
            gid: 0,
            mode: 0o644,
            xattrs: Vec::new(),
        });
    match &op.action {
        Action::Remove => remove(dir, &name, &probe.join(rel))?,
        Action::Dir => match mkdirat(dir, name.as_str(), Mode::from_raw_mode(0o755)) {
            Ok(()) | Err(Errno::EXIST) => {
                let fd = openat2(
                    dir,
                    name.as_str(),
                    OFlags::RDONLY | OFlags::DIRECTORY,
                    Mode::empty(),
                    RESOLVE,
                )?;
                set_meta(fd.as_fd(), &meta);
            }
            Err(e) => return Err(e.into()),
        },
        Action::Symlink { target } => {
            remove(dir, &name, &probe.join(rel))?;
            symlinkat(target.as_str(), dir, name.as_str())?;
        }
        Action::Blob { hash } => {
            let content = blobs.read(hash).ok_or_else(|| io_other("blob missing"))?;
            remove(dir, &name, &probe.join(rel))?;
            let mut f = create_file(dir, &name)?;
            f.write_all(&content)?;
            set_meta(f.as_fd(), &meta);
        }
        Action::FromBad => {
            let Some(m) = bad_meta else {
                return Err(io_other("not in the bad checkpoint"));
            };
            remove(dir, &name, &probe.join(rel))?;
            if m.is_dir() {
                mkdirat(dir, name.as_str(), Mode::from_raw_mode(0o755))?;
                let fd = openat2(
                    dir,
                    name.as_str(),
                    OFlags::RDONLY | OFlags::DIRECTORY,
                    Mode::empty(),
                    RESOLVE,
                )?;
                set_meta(fd.as_fd(), &meta);
            } else if m.file_type().is_symlink() {
                symlinkat(std::fs::read_link(&in_bad)?.as_os_str(), dir, name.as_str())?;
            } else if m.is_file() {
                let src = rustix::fs::open(
                    &in_bad,
                    OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                    Mode::empty(),
                )?;
                let mut dst = create_file(dir, &name)?;
                // A reflink costs nothing on btrfs (checked in P1); copy if it's refused.
                if rustix::fs::ioctl_ficlone(&dst, &src).is_err() {
                    io::copy(&mut File::from(src), &mut dst)?;
                }
                set_meta(dst.as_fd(), &meta);
            } else {
                return Err(io_other("a special file"));
            }
        }
    }
    Ok(())
}

/// Prepares a probe to run: `/etc/fstab` emptied (or systemd waits for this machine's disks
/// by UUID), the test copied in, and the probe target and service installed.
///
/// # Errors
/// On any write failure.
pub fn prepare(probe: &Path, test: &str) -> io::Result<()> {
    std::fs::write(probe.join("etc/fstab"), "")?;
    let dir = probe.join(PROBE_DIR);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir)?;
    let test_path = dir.join("test");
    std::fs::write(&test_path, test)?;
    std::fs::set_permissions(
        &test_path,
        std::os::unix::fs::PermissionsExt::from_mode(0o755),
    )?;
    let units = probe.join("etc/systemd/system");
    std::fs::create_dir_all(&units)?;
    std::fs::write(units.join("osreflog-probe.target"), TARGET_UNIT)?;
    std::fs::write(units.join("osreflog-probe.service"), SERVICE_UNIT)?;
    Ok(())
}

/// The newest kernel version a probe has in `/usr/lib/modules`.
#[must_use]
pub fn newest_kernel(probe: &Path) -> Option<String> {
    let mut versions: Vec<String> = std::fs::read_dir(probe.join("usr/lib/modules"))
        .ok()?
        .flatten()
        .filter(|e| e.path().join("vmlinuz").is_file())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    versions.sort_by(|a, b| osreflog_rpm::vercmp::rpmvercmp(a, b));
    versions.pop()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    fn op(path: &str, action: Action, exact: bool) -> Op {
        Op {
            path: path.into(),
            action,
            cause: 1,
            exact,
        }
    }

    #[test]
    fn replay_applies_every_action_and_stays_inside_the_probe() {
        let tmp = tempfile::tempdir().unwrap();
        let (probe, bad, outside) = (
            tmp.path().join("probe"),
            tmp.path().join("bad"),
            tmp.path().join("outside"),
        );
        for d in [&probe, &bad, &outside] {
            std::fs::create_dir_all(d.join("etc")).unwrap();
        }
        std::fs::write(probe.join("etc/gone.conf"), "old\n").unwrap();
        std::fs::create_dir_all(probe.join("etc/gone.d/sub")).unwrap();
        std::fs::write(probe.join("etc/gone.d/sub/x"), "x").unwrap();
        std::fs::write(bad.join("etc/later.conf"), "from the bad checkpoint\n").unwrap();
        std::fs::create_dir_all(bad.join("etc/new.d")).unwrap();
        std::os::unix::fs::symlink("zones/b", bad.join("etc/link")).unwrap();
        // A symlinked directory in the probe pointing outside it.
        std::os::unix::fs::symlink(&outside, probe.join("etc/escape")).unwrap();
        let blobs = Blobs::new(&tmp.path().join("blobs")).unwrap();
        let hash = blobs.store(b"from the blob\n").unwrap();

        let ops = vec![
            op("/etc/gone.conf", Action::Remove, true),
            op("/etc/gone.d", Action::Remove, true),
            op("/etc/new.d", Action::Dir, true),
            op("/etc/blob.conf", Action::Blob { hash }, true),
            op("/etc/later.conf", Action::FromBad, false),
            op("/etc/link", Action::FromBad, true),
            op("/etc/vanished.conf", Action::FromBad, true),
            op(
                "/etc/escape/pwned.conf",
                Action::Blob {
                    hash: "0".repeat(64),
                },
                true,
            ),
            op("/etc/escape/pwned2.conf", Action::FromBad, true),
        ];
        let r = replay(&probe, &bad, &blobs, &ops).unwrap();
        assert_eq!(r.ops, 9);
        assert_eq!(r.approximate, ["/etc/later.conf"]);
        assert_eq!(
            r.missing,
            [
                "/etc/vanished.conf",
                "/etc/escape/pwned.conf",
                "/etc/escape/pwned2.conf"
            ]
        );
        assert!(!probe.join("etc/gone.conf").exists() && !probe.join("etc/gone.d").exists());
        assert!(probe.join("etc/new.d").is_dir());
        assert_eq!(
            std::fs::read_to_string(probe.join("etc/blob.conf")).unwrap(),
            "from the blob\n"
        );
        assert_eq!(
            std::fs::read_to_string(probe.join("etc/later.conf")).unwrap(),
            "from the bad checkpoint\n"
        );
        assert_eq!(
            std::fs::read_link(probe.join("etc/link")).unwrap(),
            Path::new("zones/b")
        );
        assert_eq!(
            std::fs::read_dir(&outside).unwrap().count(),
            1,
            "only its own etc/"
        );
        assert_eq!(std::fs::read_dir(outside.join("etc")).unwrap().count(), 0);
    }

    #[test]
    fn prepare_installs_the_probe() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path();
        std::fs::create_dir_all(p.join("etc")).unwrap();
        std::fs::write(p.join("etc/fstab"), "UUID=x / btrfs defaults 0 0\n").unwrap();
        prepare(p, "#!/bin/sh\nexit 0\n").unwrap();
        assert_eq!(std::fs::read_to_string(p.join("etc/fstab")).unwrap(), "");
        let mode = std::os::unix::fs::PermissionsExt::mode(
            &std::fs::metadata(p.join(PROBE_DIR).join("test"))
                .unwrap()
                .permissions(),
        );
        assert_eq!(mode & 0o777, 0o755);
        assert!(p.join("etc/systemd/system/osreflog-probe.target").is_file());
        std::fs::create_dir_all(p.join("usr/lib/modules/7.2.10-200.fc44.x86_64")).unwrap();
        std::fs::create_dir_all(p.join("usr/lib/modules/7.2.9-200.fc44.x86_64")).unwrap();
        for v in ["7.2.10-200.fc44.x86_64", "7.2.9-200.fc44.x86_64"] {
            std::fs::write(p.join("usr/lib/modules").join(v).join("vmlinuz"), "").unwrap();
        }
        assert_eq!(newest_kernel(p).as_deref(), Some("7.2.10-200.fc44.x86_64"));
    }
}
