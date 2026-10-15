//! The engine: checkpoints, the diffs between them, package sets, retention. Everything here
//! is blocking; the D-Bus layer runs it on tokio's blocking pool.
//!
//! Two locks: `snap_lock` serializes snapshot creation and deletion, `compute_lock`
//! serializes diffs. A dnf hook therefore never waits for a diff in progress, only for
//! another snapshot.

use std::fs::File;
use std::io::{BufReader, Read};
use std::os::fd::{AsFd, OwnedFd};
use std::path::Path;
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use osreflog_btrfs::{Kind, Op, flags, ioctl};
use osreflog_ledger::refs::{self, RefError, Resolved};
use osreflog_ledger::retention::{self, Policy};
use osreflog_ledger::{Interval, Ledger, LedgerError};
use osreflog_model::{
    Change, Checkpoint, Class, ClassCounts, DiffReport, Endpoint, LogEntry, PackageChange, Status,
    Transaction, VerifyReport, classify, reason,
};
use osreflog_rpm::Package;

use crate::layout::Layout;

const RPMDB_PREFIX: &str = "/usr/lib/sysimage/rpm/";
const HISTORY_PREFIX: &str = "/usr/lib/sysimage/libdnf5/";
/// Slack after the newer checkpoint when matching dnf transactions (seconds): dnf records
/// the end time after the post-transaction hook has run.
const TRANSACTION_SLACK_S: i64 = 5;
const SCHEDULED_EVERY_MS: i64 = 24 * 3600 * 1000;

#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    #[error(transparent)]
    Ledger(#[from] LedgerError),
    #[error(transparent)]
    Ref(#[from] RefError),
    #[error("send stream: {0}")]
    Stream(#[from] osreflog_btrfs::StreamError),
    #[error("packages: {0}")]
    Rpm(#[from] osreflog_rpm::RpmError),
    #[error("{what}: {source}")]
    Io {
        what: String,
        source: std::io::Error,
    },
    #[error("{0}")]
    Invalid(String),
}

pub type Result<T> = std::result::Result<T, EngineError>;

fn io(what: impl Into<String>) -> impl FnOnce(std::io::Error) -> EngineError {
    let what = what.into();
    move |source| EngineError::Io { what, source }
}

#[must_use]
pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

fn boot_id() -> String {
    std::fs::read_to_string("/proc/sys/kernel/random/boot_id")
        .map_or_else(|_| "unknown".to_owned(), |s| s.trim().to_owned())
}

fn comm_of(pid: u32) -> Option<String> {
    std::fs::read_to_string(format!("/proc/{pid}/comm"))
        .ok()
        .map(|s| s.trim().to_owned())
}

fn ms_of(d: std::time::Duration) -> i64 {
    i64::try_from(d.as_millis()).unwrap_or(i64::MAX)
}

/// A reader that counts bytes, so the stream size is known without buffering it.
struct Counting<R> {
    inner: R,
    bytes: u64,
}

impl<R: Read> Read for Counting<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.bytes += n as u64;
        Ok(n)
    }
}

type PackageTuple = (String, Option<u32>, String, String, String);

fn encode_packages(pkgs: &[Package]) -> Vec<u8> {
    let tuples: Vec<PackageTuple> = pkgs
        .iter()
        .map(|p| {
            (
                p.name.clone(),
                p.epoch,
                p.version.clone(),
                p.release.clone(),
                p.arch.clone(),
            )
        })
        .collect();
    serde_json::to_vec(&tuples).unwrap_or_default()
}

fn decode_packages(data: &[u8]) -> Option<Vec<Package>> {
    let tuples: Vec<PackageTuple> = serde_json::from_slice(data).ok()?;
    Some(
        tuples
            .into_iter()
            .map(|(name, epoch, version, release, arch)| Package {
                name,
                epoch,
                version,
                release,
                arch,
            })
            .collect(),
    )
}

fn model_change(c: osreflog_btrfs::Change) -> Change {
    let (op, from) = match c.op {
        Op::Create => ("create", None),
        Op::Modify => ("modify", None),
        Op::Delete => ("delete", None),
        Op::Rename { from } => ("rename", Some(from)),
    };
    let mut what = Vec::new();
    for (bit, name) in [
        (flags::CONTENT, "content"),
        (flags::MODE, "mode"),
        (flags::OWNER, "owner"),
        (flags::XATTR, "xattr"),
        (flags::TIMES, "times"),
        (flags::FILEATTR, "fileattr"),
    ] {
        if c.flags & bit != 0 {
            what.push(name.to_owned());
        }
    }
    let kind = if c.kind == Kind::Unknown && c.flags & flags::CONTENT != 0 {
        Kind::File
    } else {
        c.kind
    };
    Change {
        class: classify(&c.path),
        path: c.path,
        op: op.to_owned(),
        from,
        kind: kind.as_str().to_owned(),
        what,
        bytes: c.bytes,
    }
}

/// The newer side of a diff being computed.
struct Target<'a> {
    root: &'a Path,
    fd: OwnedFd,
    created_ms: i64,
    /// The checkpoint, when it is one (its package set is then stored).
    checkpoint_id: Option<i64>,
}

pub struct Engine {
    layout: Layout,
    ledger: Mutex<Ledger>,
    boot_id: String,
    snap_lock: Mutex<()>,
    compute_lock: Mutex<()>,
    pub policy: Policy,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Engine {
    /// # Errors
    /// If the ledger belongs to another filesystem (its recorded root subvolume differs).
    pub fn new(layout: Layout, ledger: Ledger) -> Result<Self> {
        let info =
            ioctl::subvolume_info(layout.root_fd()).map_err(io("reading the root subvolume"))?;
        let uuid = info
            .uuid
            .iter()
            .fold(String::with_capacity(32), |mut s, b| {
                use std::fmt::Write;
                let _ = write!(s, "{b:02x}");
                s
            });
        match ledger.meta("root_uuid")? {
            Some(known) if known != uuid => {
                return Err(EngineError::Invalid(format!(
                    "this ledger belongs to subvolume {known}, but {} is {uuid}",
                    layout.root.display()
                )));
            }
            Some(_) => {}
            None => {
                ledger.set_meta("root_uuid", &uuid)?;
                ledger.set_meta("subvolume", &layout.subvolume)?;
            }
        }
        Ok(Self {
            layout,
            ledger: Mutex::new(ledger),
            boot_id: boot_id(),
            snap_lock: Mutex::new(()),
            compute_lock: Mutex::new(()),
            policy: Policy::default(),
        })
    }

    #[must_use]
    pub fn layout(&self) -> &Layout {
        &self.layout
    }

    /// Takes a read-only snapshot of the tracked subvolume and records it.
    ///
    /// # Errors
    /// On snapshot or ledger failures (a snapshot without a ledger row is deleted again).
    pub fn checkpoint(
        &self,
        why: &str,
        note: Option<String>,
        pm_pid: Option<u32>,
    ) -> Result<Checkpoint> {
        let _snap = lock(&self.snap_lock);
        let id = lock(&self.ledger).next_checkpoint_id()?;
        let name = format!("c{id:06}");
        let started = Instant::now();
        ioctl::create_snapshot(
            self.layout.root_fd(),
            self.layout.snapshots_fd(),
            &name,
            true,
        )
        .map_err(io(format!(
            "snapshotting {} as {name}",
            self.layout.root.display()
        )))?;
        let snap =
            File::open(self.layout.snapshot_path(&name)).map_err(io(format!("opening {name}")))?;
        let info = ioctl::subvolume_info(snap.as_fd()).map_err(io(format!("reading {name}")))?;
        let checkpoint = Checkpoint {
            id,
            name: name.clone(),
            created_ms: now_ms(),
            reason: why.to_owned(),
            generation: info.generation,
            boot_id: self.boot_id.clone(),
            note,
            pm_pid,
            pm_comm: pm_pid.and_then(comm_of),
            pinned: false,
            pruned_ms: None,
        };
        if let Err(e) = lock(&self.ledger).add_checkpoint(&checkpoint) {
            let _ = ioctl::delete_subvolume(self.layout.snapshots_fd(), &name);
            return Err(e.into());
        }
        let ms = ms_of(started.elapsed());
        tracing::info!(id, reason = why, ms, "checkpoint {name} ({why}) in {ms} ms");
        Ok(checkpoint)
    }

    /// Takes a `boot` checkpoint unless one exists for this boot.
    ///
    /// # Errors
    /// As [`Engine::checkpoint`].
    pub fn boot_checkpoint_if_needed(&self) -> Result<Option<Checkpoint>> {
        let known = lock(&self.ledger)
            .checkpoints()?
            .iter()
            .any(|c| c.boot_id == self.boot_id && c.reason == reason::BOOT);
        if known {
            return Ok(None);
        }
        self.checkpoint(reason::BOOT, None, None).map(Some)
    }

    /// Takes a `scheduled` checkpoint if the newest one is older than a day.
    ///
    /// # Errors
    /// As [`Engine::checkpoint`].
    pub fn scheduled_checkpoint_if_due(&self) -> Result<Option<Checkpoint>> {
        let latest = lock(&self.ledger).latest()?;
        if latest.is_some_and(|c| now_ms() - c.created_ms < SCHEDULED_EVERY_MS) {
            return Ok(None);
        }
        self.checkpoint(reason::SCHEDULED, None, None).map(Some)
    }

    fn package_set_of(&self, checkpoint: &Checkpoint) -> Result<Option<(i64, Vec<Package>)>> {
        if let Some((owner, data)) = lock(&self.ledger).package_set(checkpoint.id)? {
            return Ok(decode_packages(&data).map(|p| (owner, p)));
        }
        if checkpoint.pruned_ms.is_some() {
            return Ok(None);
        }
        let pkgs = osreflog_rpm::packages(
            &self.layout.snapshot_path(&checkpoint.name),
            &self.layout.tmp(),
        )?;
        lock(&self.ledger).store_package_set(checkpoint.id, Some(&encode_packages(&pkgs)), None)?;
        Ok(Some((checkpoint.id, pkgs)))
    }

    /// Computes the interval from checkpoint `from` to `to` (not stored).
    fn compute(&self, from: &Checkpoint, to: &Target<'_>) -> Result<Interval> {
        let started = Instant::now();
        let from_dir = File::open(self.layout.snapshot_path(&from.name))
            .map_err(io(format!("opening {}", from.name)))?;
        let parent = ioctl::subvolume_info(from_dir.as_fd())
            .map_err(io(format!("reading {}", from.name)))?;
        let fd = to
            .fd
            .try_clone()
            .map_err(io("duplicating the snapshot fd"))?;
        let stream =
            ioctl::send(fd, Some(parent.treeid), true).map_err(io("starting btrfs send"))?;
        let mut counting = Counting {
            inner: BufReader::with_capacity(1 << 20, stream),
            bytes: 0,
        };
        let diff = osreflog_btrfs::diff_stream(&mut counting)?;
        let stream_bytes = counting.bytes;
        counting
            .inner
            .into_inner()
            .finish()
            .map_err(io("btrfs send"))?;

        let changes: Vec<Change> = diff.changes.into_iter().map(model_change).collect();
        let rpm_changed = changes.iter().any(|c| c.path.starts_with(RPMDB_PREFIX));
        let history_changed = changes.iter().any(|c| c.path.starts_with(HISTORY_PREFIX));

        let mut packages: Vec<PackageChange> = Vec::new();
        let old = self.package_set_of(from)?;
        if rpm_changed || old.is_none() {
            let new = osreflog_rpm::packages(to.root, &self.layout.tmp())?;
            if let Some((_, old)) = &old {
                packages = osreflog_rpm::diff(old, &new);
            }
            if let Some(id) = to.checkpoint_id {
                lock(&self.ledger).store_package_set(id, Some(&encode_packages(&new)), None)?;
            }
        } else if let (Some(id), Some((owner, _))) = (to.checkpoint_id, &old) {
            lock(&self.ledger).store_package_set(id, None, Some(*owner))?;
        }

        let transactions = if rpm_changed || history_changed {
            self.transactions_between(from, to)?
        } else {
            Vec::new()
        };

        Ok(Interval {
            from_id: from.id,
            to_id: to.checkpoint_id.unwrap_or(0),
            computed_ms: now_ms(),
            duration_ms: ms_of(started.elapsed()),
            stream_bytes,
            commands: diff.info.commands,
            changed_bytes: diff.changed_bytes,
            changes,
            packages,
            transactions,
        })
    }

    /// The dnf transactions of an interval: those in the newer side's history that the older
    /// snapshot's history doesn't already show as finished. dnf's whole-second times can't
    /// separate a transaction from a checkpoint taken in the same second; the histories can.
    /// Without the older snapshot (pruned), the time window alone decides.
    fn transactions_between(&self, from: &Checkpoint, to: &Target<'_>) -> Result<Vec<Transaction>> {
        let from_s = from.created_ms / 1000;
        let mut found = osreflog_rpm::transactions(
            to.root,
            &self.layout.tmp(),
            from_s,
            to.created_ms / 1000 + TRANSACTION_SLACK_S,
        )?;
        if from.pruned_ms.is_none() {
            let before = osreflog_rpm::transactions(
                &self.layout.snapshot_path(&from.name),
                &self.layout.tmp(),
                from_s,
                i64::MAX,
            )?;
            let finished: std::collections::HashSet<i64> = before
                .iter()
                .filter(|t| t.end.is_some())
                .map(|t| t.id)
                .collect();
            found.retain(|t| !finished.contains(&t.id));
        } else {
            found.retain(|t| t.end.is_none_or(|end| end > from_s));
        }
        Ok(found)
    }

    fn compute_between(&self, from: &Checkpoint, to: &Checkpoint) -> Result<Interval> {
        let root = self.layout.snapshot_path(&to.name);
        let fd = OwnedFd::from(File::open(&root).map_err(io(format!("opening {}", to.name)))?);
        self.compute(
            from,
            &Target {
                root: &root,
                fd,
                created_ms: to.created_ms,
                checkpoint_id: Some(to.id),
            },
        )
    }

    /// Computes and stores every interval still missing between consecutive checkpoints,
    /// then applies retention. Returns how many intervals were computed.
    ///
    /// # Errors
    /// On the first failing interval.
    pub fn compute_pending(&self) -> Result<usize> {
        let _compute = lock(&self.compute_lock);
        let pending = lock(&self.ledger).pending_intervals()?;
        for (from_id, to_id) in &pending {
            let (from, to) = {
                let l = lock(&self.ledger);
                (l.checkpoint(*from_id)?, l.checkpoint(*to_id)?)
            };
            let (Some(from), Some(to)) = (from, to) else {
                continue;
            };
            let iv = self.compute_between(&from, &to)?;
            tracing::info!(
                from = from.id,
                to = to.id,
                changes = iv.changes.len(),
                packages = iv.packages.len(),
                ms = iv.duration_ms,
                "interval {}..{}: {} changes, {} package changes, in {} ms",
                from.name,
                to.name,
                iv.changes.len(),
                iv.packages.len(),
                iv.duration_ms
            );
            lock(&self.ledger).store_interval(&iv)?;
        }
        self.prune_locked()?;
        Ok(pending.len())
    }

    /// Deletes snapshots retention no longer keeps. Caller holds `compute_lock`, so no diff
    /// is reading them; intervals still pending are never pruned.
    fn prune_locked(&self) -> Result<Vec<i64>> {
        let _snap = lock(&self.snap_lock);
        let (checkpoints, pending) = {
            let l = lock(&self.ledger);
            (l.checkpoints()?, l.pending_intervals()?)
        };
        let busy: std::collections::HashSet<i64> =
            pending.iter().flat_map(|(a, b)| [*a, *b]).collect();
        let mut pruned = Vec::new();
        for id in retention::to_prune(&checkpoints, now_ms(), self.policy) {
            if busy.contains(&id) {
                continue;
            }
            let Some(c) = checkpoints.iter().find(|c| c.id == id) else {
                continue;
            };
            match ioctl::delete_subvolume(self.layout.snapshots_fd(), &c.name) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(io(format!("deleting {}", c.name))(e)),
            }
            lock(&self.ledger).mark_pruned(id, now_ms())?;
            tracing::info!(id, "pruned the snapshot of {}", c.name);
            pruned.push(id);
        }
        Ok(pruned)
    }

    fn resolve(&self, r: &str) -> Result<Resolved> {
        let parsed = refs::parse(r)?;
        Ok(refs::resolve(&lock(&self.ledger), parsed)?)
    }

    /// The diff between two references. `all` includes package-owned and noise paths.
    ///
    /// # Errors
    /// If a reference doesn't resolve, the older side is `@{now}`, a needed snapshot was
    /// pruned, or computing fails.
    pub fn diff(&self, from: &str, to: &str, all: bool) -> Result<DiffReport> {
        let (from, to) = (self.resolve(from)?, self.resolve(to)?);
        let Resolved::Checkpoint(from) = from else {
            return Err(EngineError::Invalid(
                "@{now} can only be the newer side of a diff".into(),
            ));
        };
        let (interval, to_endpoint, cached) = match to {
            Resolved::Checkpoint(to) => {
                if to.id == from.id {
                    return Err(EngineError::Invalid(
                        "both sides are the same checkpoint".into(),
                    ));
                }
                let (from, to) = if from.id < to.id {
                    (from, to)
                } else {
                    (to, from)
                };
                let stored = lock(&self.ledger).interval(from.id, to.id)?;
                let (iv, cached) = if let Some(iv) = stored {
                    (iv, true)
                } else {
                    for c in [&from, &to] {
                        if c.pruned_ms.is_some() {
                            return Err(EngineError::Invalid(format!(
                                "the snapshot of {} was pruned; diff consecutive checkpoints (osreflog log) instead",
                                c.name
                            )));
                        }
                    }
                    let _compute = lock(&self.compute_lock);
                    let iv = self.compute_between(&from, &to)?;
                    lock(&self.ledger).store_interval(&iv)?;
                    (iv, false)
                };
                let from_ep = Endpoint::Checkpoint(from);
                return Ok(report(from_ep, Endpoint::Checkpoint(to), iv, cached, all));
            }
            Resolved::Live => {
                if from.pruned_ms.is_some() {
                    return Err(EngineError::Invalid(format!(
                        "the snapshot of {} was pruned",
                        from.name
                    )));
                }
                let _compute = lock(&self.compute_lock);
                let taken = now_ms();
                let name = format!("live-{taken}");
                {
                    let _snap = lock(&self.snap_lock);
                    ioctl::create_snapshot(
                        self.layout.root_fd(),
                        self.layout.tmp_fd(),
                        &name,
                        true,
                    )
                    .map_err(io("snapshotting the live system"))?;
                }
                let root = self.layout.tmp().join(&name);
                let result = File::open(&root)
                    .map_err(io("opening the live snapshot"))
                    .and_then(|f| {
                        self.compute(
                            &from,
                            &Target {
                                root: &root,
                                fd: OwnedFd::from(f),
                                created_ms: taken,
                                checkpoint_id: None,
                            },
                        )
                    });
                let _ = ioctl::delete_subvolume(self.layout.tmp_fd(), &name);
                (result?, Endpoint::Live { taken_ms: taken }, false)
            }
        };
        Ok(report(
            Endpoint::Checkpoint(from),
            to_endpoint,
            interval,
            cached,
            all,
        ))
    }

    /// The newest `limit` checkpoints with their interval summaries, newest first.
    ///
    /// # Errors
    /// On ledger errors.
    pub fn log(&self, limit: usize) -> Result<Vec<LogEntry>> {
        let l = lock(&self.ledger);
        let mut out = Vec::new();
        for c in l.checkpoints()?.into_iter().rev().take(limit) {
            let interval = l.summary_ending_at(c.id)?;
            out.push(LogEntry {
                checkpoint: c,
                interval,
            });
        }
        Ok(out)
    }

    /// # Errors
    /// On ledger errors.
    pub fn status(&self) -> Result<Status> {
        let l = lock(&self.ledger);
        let all = l.checkpoints()?;
        Ok(Status {
            version: env!("CARGO_PKG_VERSION").to_owned(),
            root: self.layout.root.display().to_string(),
            device: self.layout.device.clone(),
            subvolume: self.layout.subvolume.clone(),
            checkpoints: all.len() as u64,
            snapshots_kept: all.iter().filter(|c| c.pruned_ms.is_none()).count() as u64,
            latest: all.last().cloned(),
            chain_entries: l.chain_len()?,
            pending_diffs: l.pending_intervals()?.len() as u64,
        })
    }

    /// # Errors
    /// On ledger errors.
    pub fn verify(&self) -> Result<VerifyReport> {
        Ok(lock(&self.ledger).verify()?)
    }
}

/// Package-owned and noise paths are hidden by default, and so are files whose only change is
/// their times (mostly `relatime` access-time updates on files that were read or run).
fn shown_by_default(c: &Change) -> bool {
    let times_only = c.op == "modify" && c.what.len() == 1 && c.what[0] == "times";
    !matches!(c.class, Class::Package | Class::Noise) && !times_only
}

fn report(from: Endpoint, to: Endpoint, iv: Interval, cached: bool, all: bool) -> DiffReport {
    let mut counts = ClassCounts::default();
    for c in &iv.changes {
        counts.add(c.class);
    }
    let total = iv.changes.len() as u64;
    let changes: Vec<Change> = iv
        .changes
        .into_iter()
        .filter(|c| all || shown_by_default(c))
        .collect();
    let hidden = total - changes.len() as u64;
    DiffReport {
        from,
        to,
        witnesses: vec!["state".to_owned()],
        cached,
        compute_ms: u64::try_from(iv.duration_ms).unwrap_or(0),
        stream_bytes: iv.stream_bytes,
        commands: iv.commands,
        changed_bytes: iv.changed_bytes,
        counts,
        packages: iv.packages,
        transactions: iv.transactions,
        changes,
        hidden,
    }
}
