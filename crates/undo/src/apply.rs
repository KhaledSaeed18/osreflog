//! Applying a plan to a root directory. Everything happens relative to one pinned directory
//! fd: each parent directory is opened with `openat2` under [`RESOLVE`], and every other call
//! is a `*at` call on that parent fd with a single final name. No path string is ever
//! resolved from `/`, and no symlink is ever followed.

use std::fs::File;
use std::io::{self, Write as _};
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::path::Path;
use std::sync::atomic::{AtomicU32, Ordering};

use rustix::fs::{
    AtFlags, Dir, FileType, Mode, OFlags, RenameFlags, ResolveFlags, XattrFlags, chownat, fchmod,
    fchown, fstat, fsync, mkdirat, openat2, readlinkat, renameat_with, statat, symlinkat, unlinkat,
};
use rustix::io::Errno;
use rustix::process::{Gid, Uid};
use serde::{Deserialize, Serialize};

use crate::plan::{self, Meta, Plan, State, Step, Version};

/// How every directory on the way is resolved: never above the root, never through a
/// symlink or a magic link (`/proc/self/fd/…`), never onto another mount.
pub const RESOLVE: ResolveFlags = ResolveFlags::BENEATH
    .union(ResolveFlags::NO_SYMLINKS)
    .union(ResolveFlags::NO_MAGICLINKS)
    .union(ResolveFlags::NO_XDEV);

const TMP_PREFIX: &str = ".osreflog-undo.";
/// Errors meaning `openat2` refused the way to a path: a symlink, a magic link, a mount, or a
/// file where a directory should be.
const UNRESOLVED: &[Errno] = &[Errno::LOOP, Errno::NOTDIR, Errno::XDEV];
pub const NOT_BENEATH: &str =
    "not resolved beneath the root (a symlink, mount or file on the way): left alone";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum Outcome {
    /// Put back; what was there went to the attic.
    Restored {
        warnings: Vec<String>,
    },
    /// The path is already as it was before the cause.
    AlreadyUndone,
    /// The path changed since the cause: left alone.
    Conflict {
        current: State,
        reason: String,
    },
    /// Never done, whatever is on disk.
    Refused {
        reason: String,
    },
    Failed {
        error: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StepReport {
    pub path: String,
    pub expect: State,
    pub restore: State,
    #[serde(flatten)]
    pub outcome: Outcome,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Report {
    pub undo_of: i64,
    pub attic: String,
    pub steps: Vec<StepReport>,
}

impl Report {
    #[must_use]
    pub fn restored(&self) -> usize {
        self.count(|o| matches!(o, Outcome::Restored { .. }))
    }

    #[must_use]
    pub fn conflicts(&self) -> usize {
        self.count(|o| matches!(o, Outcome::Conflict { .. }))
    }

    fn count(&self, f: impl Fn(&Outcome) -> bool) -> usize {
        self.steps.iter().filter(|s| f(&s.outcome)).count()
    }
}

/// The directory everything is relative to.
pub struct Root {
    fd: OwnedFd,
}

/// Why a step stopped short of restoring.
enum Stop {
    Already,
    Conflict(State, String),
    Error(io::Error),
}

impl From<io::Error> for Stop {
    fn from(e: io::Error) -> Self {
        Self::Error(e)
    }
}

impl From<Errno> for Stop {
    fn from(e: Errno) -> Self {
        Self::Error(e.into())
    }
}

fn dir_flags() -> OFlags {
    OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC | OFlags::NOFOLLOW
}

fn tmp_name() -> String {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    format!(
        "{TMP_PREFIX}{}.{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    )
}

/// Splits `/a/b/c` into (`a/b`, `c`); the parent of a top-level name is `.`.
fn split(path: &str) -> (&str, &str) {
    let rel = path.trim_start_matches('/');
    match rel.rsplit_once('/') {
        Some((parent, name)) => (parent, name),
        None => (".", rel),
    }
}

impl Root {
    /// Opens the root. The path itself is trusted (it's the daemon's own mount of the
    /// subvolume, or a test's scratch root); everything beneath it is not.
    ///
    /// # Errors
    /// If it can't be opened as a directory.
    pub fn open(path: &Path) -> io::Result<Self> {
        let fd = rustix::fs::open(path, dir_flags(), Mode::empty())?;
        Ok(Self { fd })
    }

    /// Opens a directory beneath the root (`.` is the root itself).
    ///
    /// # Errors
    /// `ENOENT`, `ELOOP` (a symlink on the way), `EXDEV` (a mount or `..` above the root), …
    pub fn dir(&self, rel: &str) -> io::Result<OwnedFd> {
        Ok(openat2(&self.fd, rel, dir_flags(), Mode::empty(), RESOLVE)?)
    }

    /// Opens a directory beneath the root, creating missing components (mode `0700`).
    fn make_dirs(&self, rel: &str) -> io::Result<OwnedFd> {
        let mut fd = self.dir(".")?;
        for c in rel.split('/').filter(|c| !c.is_empty()) {
            match mkdirat(&fd, c, Mode::from_raw_mode(0o700)) {
                Ok(()) | Err(Errno::EXIST) => {}
                Err(e) => return Err(e.into()),
            }
            fd = openat2(&fd, c, dir_flags(), Mode::empty(), RESOLVE)?;
        }
        Ok(fd)
    }
}

/// What is at `name` in `parent`, without following it.
///
/// # Errors
/// On I/O errors, or `EAGAIN` if the object changed while it was being read.
pub fn state_at(parent: BorrowedFd<'_>, name: &str) -> io::Result<State> {
    let st = match statat(parent, name, AtFlags::SYMLINK_NOFOLLOW) {
        Ok(st) => st,
        Err(Errno::NOENT) => return Ok(State::Absent),
        Err(e) => return Err(e.into()),
    };
    Ok(match FileType::from_raw_mode(st.st_mode) {
        FileType::Directory => State::Dir,
        FileType::Symlink => State::Symlink {
            target: readlinkat(parent, name, Vec::new())?
                .to_string_lossy()
                .into_owned(),
        },
        FileType::RegularFile => {
            // O_NONBLOCK: if a FIFO took the file's place since the stat, opening it must
            // not hang. The fstat below catches the swap either way.
            let fd = openat2(
                parent,
                name,
                OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
                Mode::empty(),
                RESOLVE,
            )?;
            let now = fstat(&fd)?;
            if now.st_ino != st.st_ino
                || FileType::from_raw_mode(now.st_mode) != FileType::RegularFile
            {
                return Err(Errno::AGAIN.into());
            }
            let mut hasher = blake3::Hasher::new();
            io::copy(&mut File::from(fd), &mut hasher)?;
            State::File {
                hash: hasher.finalize().to_hex().to_string(),
            }
        }
        _ => State::Other,
    })
}

fn set_meta(fd: BorrowedFd<'_>, meta: &Meta, warnings: &mut Vec<String>) -> io::Result<()> {
    fchown(
        fd,
        Some(Uid::from_raw(meta.uid)),
        Some(Gid::from_raw(meta.gid)),
    )?;
    fchmod(fd, Mode::from_raw_mode(meta.mode))?;
    for (name, value) in &meta.xattrs {
        let value = plan::from_hex(value).unwrap_or_default();
        if let Err(e) = rustix::fs::fsetxattr(fd, name.as_str(), &value, XattrFlags::empty()) {
            warnings.push(format!("{name} not restored: {e}"));
        }
    }
    Ok(())
}

/// Writes `version` (a file or a symlink) under a temporary name in `parent`.
fn create_tmp(
    parent: BorrowedFd<'_>,
    version: &Version,
    warnings: &mut Vec<String>,
) -> io::Result<String> {
    let tmp = tmp_name();
    match version {
        Version::File { content, meta } => {
            let fd = openat2(
                parent,
                tmp.as_str(),
                OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::from_raw_mode(0o600),
                RESOLVE,
            )?;
            let written = (|| -> io::Result<()> {
                let mut f = File::from(fd);
                f.write_all(&plan::from_hex(content).unwrap_or_default())?;
                set_meta(f.as_fd(), meta, warnings)?;
                fsync(&f)?;
                Ok(())
            })();
            if let Err(e) = written {
                let _ = unlinkat(parent, tmp.as_str(), AtFlags::empty());
                return Err(e);
            }
        }
        Version::Symlink { target, uid, gid } => {
            symlinkat(target.as_str(), parent, tmp.as_str())?;
            if let Err(e) = chownat(
                parent,
                tmp.as_str(),
                Some(Uid::from_raw(*uid)),
                Some(Gid::from_raw(*gid)),
                AtFlags::SYMLINK_NOFOLLOW,
            ) {
                let _ = unlinkat(parent, tmp.as_str(), AtFlags::empty());
                return Err(e.into());
            }
        }
        Version::Absent | Version::Dir { .. } => {
            return Err(io::Error::other("not a file or symlink"));
        }
    }
    Ok(tmp)
}

fn is_empty_dir(parent: BorrowedFd<'_>, name: &str) -> io::Result<bool> {
    let fd = openat2(parent, name, dir_flags(), Mode::empty(), RESOLVE)?;
    for entry in Dir::read_from(&fd)? {
        let entry = entry?;
        let n = entry.file_name().to_bytes();
        if n != b"." && n != b".." {
            return Ok(false);
        }
    }
    Ok(true)
}

struct Applier<'a> {
    root: &'a Root,
    attic_rel: String,
    /// Runs between the 3-way check and the swap (tests stage races here).
    #[cfg(test)]
    before_swap: Option<fn(BorrowedFd<'_>, &str)>,
}

impl Applier<'_> {
    /// The attic directory mirroring `parent_rel`, created on first use.
    fn attic_dir(&self, parent_rel: &str) -> io::Result<OwnedFd> {
        let rel = if parent_rel == "." {
            self.attic_rel.clone()
        } else {
            format!("{}/{parent_rel}", self.attic_rel)
        };
        self.root.make_dirs(&rel)
    }

    /// Moves `name` (which must be `expect`) into the attic, checking it afterwards: if it
    /// changed on the way, it goes back and the step is a conflict.
    fn to_attic(
        &self,
        parent: BorrowedFd<'_>,
        parent_rel: &str,
        name: &str,
        expect: &State,
    ) -> Result<(), Stop> {
        if *expect == State::Dir && !is_empty_dir(parent, name)? {
            return Err(Stop::Conflict(
                State::Dir,
                "the directory isn't empty: other files were added to it".into(),
            ));
        }
        let attic = self.attic_dir(parent_rel)?;
        renameat_with(parent, name, &attic, name, RenameFlags::NOREPLACE)?;
        let moved = state_at(attic.as_fd(), name)?;
        if moved != *expect {
            renameat_with(&attic, name, parent, name, RenameFlags::NOREPLACE)?;
            return Err(Stop::Conflict(
                moved,
                "changed while it was being moved".into(),
            ));
        }
        Ok(())
    }

    fn apply(&self, step: &Step) -> Result<Vec<String>, Stop> {
        let (parent_rel, name) = split(&step.path);
        let parent = self
            .root
            .dir(parent_rel)
            .map_err(|e| match e.raw_os_error() {
                Some(n) if n == Errno::NOENT.raw_os_error() => {
                    Stop::Conflict(State::Absent, "its directory doesn't exist".into())
                }
                Some(n) if UNRESOLVED.iter().any(|e| e.raw_os_error() == n) => {
                    Stop::Error(io::Error::other(format!("{NOT_BENEATH} ({e})")))
                }
                _ => Stop::Error(e),
            })?;
        let parent = parent.as_fd();
        let current = state_at(parent, name)?;
        if current == step.restore.state() {
            return Err(Stop::Already);
        }
        if current != step.expect || current == State::Other {
            return Err(Stop::Conflict(
                current,
                "it changed after this cause".into(),
            ));
        }
        let mut warnings = Vec::new();
        match &step.restore {
            Version::Absent => self.to_attic(parent, parent_rel, name, &current)?,
            Version::Dir { meta } => {
                if current != State::Absent {
                    self.to_attic(parent, parent_rel, name, &current)?;
                }
                mkdirat(parent, name, Mode::from_raw_mode(0o700))?;
                let fd = openat2(parent, name, dir_flags(), Mode::empty(), RESOLVE)?;
                set_meta(fd.as_fd(), meta, &mut warnings)?;
            }
            Version::File { .. } | Version::Symlink { .. } => {
                let tmp = create_tmp(parent, &step.restore, &mut warnings)?;
                let placed = self.place(parent, parent_rel, name, &tmp, &current);
                if placed.is_err() {
                    let _ = unlinkat(parent, tmp.as_str(), AtFlags::empty());
                }
                placed?;
            }
        }
        fsync(parent)?;
        Ok(warnings)
    }

    /// Puts the temporary `tmp` at `name`, where `current` (already checked against A) is.
    fn place(
        &self,
        parent: BorrowedFd<'_>,
        parent_rel: &str,
        name: &str,
        tmp: &str,
        current: &State,
    ) -> Result<(), Stop> {
        match current {
            State::Absent => match renameat_with(parent, tmp, parent, name, RenameFlags::NOREPLACE)
            {
                Err(Errno::EXIST) => Err(Stop::Conflict(
                    state_at(parent, name).unwrap_or(State::Other),
                    "it appeared while undo ran".into(),
                )),
                r => Ok(r?),
            },
            State::Dir => {
                self.to_attic(parent, parent_rel, name, current)?;
                Ok(renameat_with(
                    parent,
                    tmp,
                    parent,
                    name,
                    RenameFlags::NOREPLACE,
                )?)
            }
            State::File { .. } | State::Symlink { .. } => {
                // Swap, then check what was swapped out: if it isn't A any more, someone
                // wrote it between the check and the swap, so swap back.
                #[cfg(test)]
                if let Some(hook) = self.before_swap {
                    hook(parent, name);
                }
                renameat_with(parent, tmp, parent, name, RenameFlags::EXCHANGE)?;
                let old = state_at(parent, tmp)?;
                if old != *current {
                    renameat_with(parent, tmp, parent, name, RenameFlags::EXCHANGE)?;
                    return Err(Stop::Conflict(old, "it changed while undo ran".into()));
                }
                let attic = self.attic_dir(parent_rel)?;
                renameat_with(parent, tmp, &attic, name, RenameFlags::NOREPLACE)?;
                Ok(())
            }
            State::Other => Err(Stop::Conflict(State::Other, "a special file".into())),
        }
    }
}

/// Applies `plan` beneath `root`. Every step is checked and reported on its own; a refused,
/// conflicting or failing step doesn't stop the others.
#[must_use]
pub fn apply(root: &Root, plan: &Plan) -> Report {
    run(
        &Applier {
            root,
            attic_rel: plan.attic.trim_start_matches('/').to_owned(),
            #[cfg(test)]
            before_swap: None,
        },
        plan,
    )
}

fn run(applier: &Applier<'_>, plan: &Plan) -> Report {
    let report = |s: &Step, outcome| StepReport {
        path: s.path.clone(),
        expect: s.expect.clone(),
        restore: s.restore.state(),
        outcome,
    };
    let mut out = Report {
        undo_of: plan.undo_of,
        attic: plan.attic.clone(),
        steps: Vec::new(),
    };
    if let Err(reason) = plan::check(plan) {
        out.steps = plan
            .steps
            .iter()
            .map(|s| {
                report(
                    s,
                    Outcome::Refused {
                        reason: reason.clone(),
                    },
                )
            })
            .collect();
        return out;
    }
    for step in plan::ordered(&plan.steps) {
        let outcome = if let Some(reason) = plan::refusal(step) {
            Outcome::Refused { reason }
        } else {
            match applier.apply(step) {
                Ok(warnings) => Outcome::Restored { warnings },
                Err(Stop::Already) => Outcome::AlreadyUndone,
                Err(Stop::Conflict(current, reason)) => Outcome::Conflict { current, reason },
                Err(Stop::Error(e)) => Outcome::Failed {
                    error: e.to_string(),
                },
            }
        };
        out.steps.push(report(step, outcome));
    }
    out
}

/// The current state of each step's path, for a dry run. Unreadable paths are reported as
/// errors.
#[must_use]
pub fn current(root: &Root, plan: &Plan) -> Vec<(String, Result<State, String>)> {
    plan.steps
        .iter()
        .map(|s| {
            let (parent_rel, name) = split(&s.path);
            let state = match root.dir(parent_rel) {
                Ok(p) => state_at(p.as_fd(), name).map_err(|e| e.to_string()),
                Err(e) if e.raw_os_error() == Some(Errno::NOENT.raw_os_error()) => {
                    Ok(State::Absent)
                }
                Err(e) => Err(e.to_string()),
            };
            (s.path.clone(), state)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    fn sneak(parent: BorrowedFd<'_>, name: &str) {
        let fd = openat2(
            parent,
            name,
            OFlags::WRONLY | OFlags::TRUNC,
            Mode::empty(),
            RESOLVE,
        )
        .unwrap();
        File::from(fd).write_all(b"written meanwhile\n").unwrap();
    }

    #[test]
    fn a_write_between_the_check_and_the_swap_is_swapped_back() {
        let dir = tempfile::tempdir().unwrap();
        let etc = dir.path().join("etc");
        std::fs::create_dir(&etc).unwrap();
        std::fs::write(etc.join("a.conf"), "new\n").unwrap();
        let meta = std::fs::metadata(&etc).unwrap();
        let plan = Plan {
            undo_of: 1,
            attic: "/var/lib/osreflog/attic/t".into(),
            steps: vec![Step {
                path: "/etc/a.conf".into(),
                expect: State::File {
                    hash: plan::hash(b"new\n"),
                },
                restore: Version::file(
                    b"old\n",
                    Meta {
                        uid: std::os::unix::fs::MetadataExt::uid(&meta),
                        gid: std::os::unix::fs::MetadataExt::gid(&meta),
                        mode: 0o644,
                        xattrs: Vec::new(),
                    },
                ),
            }],
        };
        let root = Root::open(dir.path()).unwrap();
        let report = run(
            &Applier {
                root: &root,
                attic_rel: plan.attic[1..].to_owned(),
                before_swap: Some(sneak),
            },
            &plan,
        );
        assert_eq!(
            report.steps[0].outcome,
            Outcome::Conflict {
                current: State::File {
                    hash: plan::hash(b"written meanwhile\n")
                },
                reason: "it changed while undo ran".into(),
            }
        );
        assert_eq!(
            std::fs::read_to_string(etc.join("a.conf")).unwrap(),
            "written meanwhile\n"
        );
        assert_eq!(
            std::fs::read_dir(&etc).unwrap().count(),
            1,
            "the temp file is gone"
        );
    }
}
