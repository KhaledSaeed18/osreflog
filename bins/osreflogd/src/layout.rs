//! Where osreflog keeps things on the tracked btrfs filesystem, and the private mount of its
//! top level that makes them reachable.
//!
//! ```text
//! top level (subvolid=5), mounted at /run/osreflog/top in osreflogd's namespace only
//! ├── root                     the tracked subvolume (mounted at /)
//! └── osreflog/
//!     ├── ledger/              own subvolume: rolling back / never rolls back history
//!     ├── snapshots/c000042    read-only checkpoints
//!     └── tmp/                 scratch: database copies, live snapshots for @{now}
//! ```

use std::fs::{self, File};
use std::os::fd::{AsFd, BorrowedFd};
use std::path::{Path, PathBuf};

use osreflog_btrfs::ioctl;
use rustix::mount::{MountFlags, mount};

pub const DEFAULT_TOP: &str = "/run/osreflog/top";
const BASE: &str = "osreflog";

#[derive(Debug, thiserror::Error)]
pub enum LayoutError {
    #[error("{0} is not a mount point")]
    NotMounted(String),
    #[error("{0} is {1}, not btrfs")]
    NotBtrfs(String, String),
    #[error(
        "{0} is the btrfs top level itself; osreflog tracks a subvolume such as Fedora's `root`"
    )]
    TopLevel(String),
    #[error(
        "refusing to mount the btrfs top level in the host's mount namespace: run osreflogd \
         under systemd (PrivateMounts=yes) or `unshare -m --propagation private`"
    )]
    SharedNamespace,
    #[error("{what}: {source}")]
    Io {
        what: String,
        source: std::io::Error,
    },
}

fn io<E: Into<std::io::Error>>(what: impl Into<String>) -> impl FnOnce(E) -> LayoutError {
    let what = what.into();
    move |e| LayoutError::Io {
        what,
        source: e.into(),
    }
}

/// One line of `/proc/self/mountinfo`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MountEntry {
    /// The path inside the filesystem that is mounted (`/root` for Fedora's `/`).
    pub fs_root: String,
    pub mount_point: String,
    pub fs_type: String,
    pub source: String,
}

fn unescape(s: &str) -> String {
    // mountinfo escapes space, tab, newline and backslash as \ooo.
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\'
            && i + 3 < b.len()
            && b[i + 1..i + 4].iter().all(|c| (b'0'..=b'7').contains(c))
        {
            let v = (b[i + 1] - b'0') * 64 + (b[i + 2] - b'0') * 8 + (b[i + 3] - b'0');
            out.push(v);
            i += 4;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Parses mountinfo text.
#[must_use]
pub fn parse_mountinfo(text: &str) -> Vec<MountEntry> {
    text.lines()
        .filter_map(|line| {
            let (left, right) = line.split_once(" - ")?;
            let l: Vec<&str> = left.split(' ').collect();
            let r: Vec<&str> = right.split(' ').collect();
            Some(MountEntry {
                fs_root: unescape(l.get(3)?),
                mount_point: unescape(l.get(4)?),
                fs_type: (*r.first()?).to_owned(),
                source: unescape(r.get(1)?),
            })
        })
        .collect()
}

/// The mount at exactly `path` (the last one, if stacked).
///
/// # Errors
/// If mountinfo can't be read or nothing is mounted there.
pub fn mount_at(path: &Path) -> Result<MountEntry, LayoutError> {
    let text =
        fs::read_to_string("/proc/self/mountinfo").map_err(io("reading /proc/self/mountinfo"))?;
    let want = path.to_string_lossy();
    parse_mountinfo(&text)
        .into_iter()
        .rfind(|m| m.mount_point == want)
        .ok_or_else(|| LayoutError::NotMounted(want.into_owned()))
}

/// Whether this process has its own mount namespace (not PID 1's).
///
/// # Errors
/// If the namespace links can't be read.
pub fn in_private_namespace() -> Result<bool, LayoutError> {
    let own = fs::read_link("/proc/self/ns/mnt").map_err(io("reading /proc/self/ns/mnt"))?;
    let init = fs::read_link("/proc/1/ns/mnt").map_err(io("reading /proc/1/ns/mnt"))?;
    Ok(own != init)
}

fn open_dir(path: &Path) -> Result<File, LayoutError> {
    File::open(path).map_err(io(format!("opening {}", path.display())))
}

pub struct Layout {
    /// The tracked mount point, normally `/`.
    pub root: PathBuf,
    pub device: String,
    /// The tracked subvolume's path from the top level (`root` on Fedora).
    pub subvolume: String,
    pub top: PathBuf,
    root_dir: File,
    snapshots_dir: File,
    tmp_dir: File,
}

impl Layout {
    /// Finds the btrfs subvolume mounted at `root`, mounts its filesystem's top level at
    /// `top` (in this process's private namespace) and creates osreflog's directories.
    ///
    /// # Errors
    /// If `root` isn't a btrfs subvolume mount, the namespace is PID 1's, or any mount or
    /// filesystem operation fails.
    pub fn open(root: &Path, top: &Path) -> Result<Self, LayoutError> {
        let root = fs::canonicalize(root).map_err(io(format!("resolving {}", root.display())))?;
        let entry = mount_at(&root)?;
        if entry.fs_type != "btrfs" {
            return Err(LayoutError::NotBtrfs(
                root.display().to_string(),
                entry.fs_type,
            ));
        }
        let subvolume = entry.fs_root.trim_start_matches('/').to_owned();
        if subvolume.is_empty() {
            return Err(LayoutError::TopLevel(root.display().to_string()));
        }
        if !in_private_namespace()? {
            return Err(LayoutError::SharedNamespace);
        }

        fs::create_dir_all(top).map_err(io(format!("creating {}", top.display())))?;
        let mounted = mount_at(top).is_ok_and(|m| m.source == entry.source && m.fs_root == "/");
        if !mounted {
            mount(
                entry.source.as_str(),
                top,
                "btrfs",
                MountFlags::NOSUID | MountFlags::NODEV | MountFlags::NOEXEC | MountFlags::NOATIME,
                c"subvolid=5",
            )
            .map_err(io(format!(
                "mounting the top level of {} at {}",
                entry.source,
                top.display()
            )))?;
        }

        let base = top.join(BASE);
        fs::create_dir_all(base.join("snapshots")).map_err(io("creating osreflog/snapshots"))?;
        fs::create_dir_all(base.join("tmp")).map_err(io("creating osreflog/tmp"))?;
        if !base.join("ledger").exists() {
            let base_dir = open_dir(&base)?;
            ioctl::create_subvolume(base_dir.as_fd(), "ledger")
                .map_err(io("creating the ledger subvolume"))?;
        }

        let layout = Self {
            root_dir: open_dir(&top.join(&subvolume))?,
            snapshots_dir: open_dir(&base.join("snapshots"))?,
            tmp_dir: open_dir(&base.join("tmp"))?,
            root,
            device: entry.source,
            subvolume,
            top: top.to_owned(),
        };
        layout.clean_tmp();
        Ok(layout)
    }

    /// Removes leftovers in `tmp/` from an earlier run (live snapshots, database copies).
    fn clean_tmp(&self) {
        let Ok(entries) = fs::read_dir(self.tmp()) else {
            return;
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if ioctl::delete_subvolume(self.tmp_dir.as_fd(), &name).is_err() {
                let _ = fs::remove_dir_all(entry.path());
            }
        }
    }

    #[must_use]
    pub fn base(&self) -> PathBuf {
        self.top.join(BASE)
    }

    #[must_use]
    pub fn ledger_path(&self) -> PathBuf {
        self.base().join("ledger").join("ledger.sqlite")
    }

    #[must_use]
    pub fn snapshot_path(&self, name: &str) -> PathBuf {
        self.base().join("snapshots").join(name)
    }

    #[must_use]
    pub fn tmp(&self) -> PathBuf {
        self.base().join("tmp")
    }

    #[must_use]
    pub fn root_fd(&self) -> BorrowedFd<'_> {
        self.root_dir.as_fd()
    }

    #[must_use]
    pub fn snapshots_fd(&self) -> BorrowedFd<'_> {
        self.snapshots_dir.as_fd()
    }

    #[must_use]
    pub fn tmp_fd(&self) -> BorrowedFd<'_> {
        self.tmp_dir.as_fd()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
23 1 0:22 /root / rw,relatime shared:1 - btrfs /dev/sda3 rw,seclabel,compress=zstd:1,subvolid=256,subvol=/root
24 23 0:22 /home /home rw,relatime shared:2 - btrfs /dev/sda3 rw,seclabel,subvolid=257,subvol=/home
25 23 8:2 / /boot rw,relatime shared:3 - ext4 /dev/sda2 rw,seclabel
26 23 0:50 / /mnt/my\\040disk rw - fuseblk /dev/sdb1 rw
";

    #[test]
    fn parses_mountinfo() {
        let m = parse_mountinfo(SAMPLE);
        assert_eq!(m.len(), 4);
        assert_eq!(
            m[0],
            MountEntry {
                fs_root: "/root".into(),
                mount_point: "/".into(),
                fs_type: "btrfs".into(),
                source: "/dev/sda3".into()
            }
        );
        assert_eq!(m[2].fs_type, "ext4");
        assert_eq!(m[3].mount_point, "/mnt/my disk");
    }
}
