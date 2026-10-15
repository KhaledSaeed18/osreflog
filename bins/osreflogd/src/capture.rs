//! The live witness: a thread that reads fanotify events for the tracked filesystem and the
//! eBPF lineage ring buffer, works out who made each change, and groups changes into causes.
//!
//! Each loop drains the ring buffer *before* reading file events: an actor's exec record is
//! queued before any write it makes, so its lineage is known when its events arrive.

use std::collections::HashMap;
use std::ffi::OsStr;
use std::fs::File;
use std::os::fd::{AsFd, AsRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use osreflog_fanotify::event::{
    FAN_ATTRIB, FAN_CLOSE_WRITE, FAN_CREATE, FAN_DELETE, FAN_RENAME, INFO_DFID_NAME,
    INFO_NEW_DFID_NAME, INFO_OLD_DFID_NAME,
};
use osreflog_fanotify::{Fid, Group, MASK, open_by_handle};
use osreflog_model::{Class, LiveChange, LiveStatus, classify as class_of};
use rustix::event::{PollFd, PollFlags, poll};

use crate::blobs::Blobs;
use crate::causes::{self, Brackets, Builder, OpenCause};
use crate::engine::{Engine, now_ms};
use crate::lineage::{Lineage, ProcTable};

const TICK: Duration = Duration::from_millis(500);
const GC_EVERY: Duration = Duration::from_secs(5);
const HEARTBEAT_EVERY: Duration = Duration::from_secs(60);
const DIR_CACHE_MAX: usize = 8192;
const READ_BUF: usize = 256 * 1024;

/// State shared between the capture thread, the engine and the D-Bus service.
#[derive(Default)]
pub struct LiveState {
    pub brackets: Mutex<Brackets>,
    /// Every change up to this time (ms) has been stored as part of a closed cause.
    pub flushed_ms: AtomicI64,
    pub running: AtomicBool,
    pub stop: AtomicBool,
    pub stats: Mutex<LiveStatus>,
}

impl LiveState {
    pub fn lock_brackets(&self) -> std::sync::MutexGuard<'_, Brackets> {
        self.brackets.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn stats(&self) -> std::sync::MutexGuard<'_, LiveStatus> {
        self.stats.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Waits until changes up to `ms` are stored, or `timeout` passes. Returns whether they
    /// are (always true when the live witness isn't running: there is nothing to wait for).
    #[must_use]
    pub fn wait_flushed(&self, ms: i64, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        while self.running.load(Ordering::Relaxed) && self.flushed_ms.load(Ordering::Relaxed) < ms {
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        true
    }

    /// A package transaction began (dnf hook, pre).
    pub fn bracket_open(&self, pid: u32, label: String) {
        let mut b = self.lock_brackets();
        b.ended.remove(&pid);
        b.active.insert(pid, label);
    }

    /// A package transaction ended (dnf hook, post).
    pub fn bracket_close(&self, pid: u32) {
        let mut b = self.lock_brackets();
        b.active.remove(&pid);
        b.ended.insert(pid);
    }
}

fn op_of(mask: u64) -> Option<&'static str> {
    if mask & FAN_DELETE != 0 {
        Some("delete")
    } else if mask & FAN_RENAME != 0 {
        Some("rename")
    } else if mask & FAN_CREATE != 0 {
        Some("create")
    } else if mask & FAN_CLOSE_WRITE != 0 {
        Some("modify")
    } else if mask & FAN_ATTRIB != 0 {
        Some("attr")
    } else {
        None
    }
}

/// Resolves directory handles to paths, with a small cache.
/// Resolves directory handles to logical paths, with a small cache.
///
/// The mark is on the whole btrfs filesystem through the top-level mount (a filesystem mark
/// through a subvolume such as `/` fails with EXDEV), so handles are opened against the
/// top-level mount and come back as `<top>/<subvolume>/...`. Paths under the tracked
/// subvolume's prefix become `/...`; everything else (other subvolumes: `home`, the ledger,
/// snapshots) is out of scope.
struct Resolver {
    mount: File,
    /// `<top>/<subvolume>`, e.g. `/run/osreflog/top/root`.
    prefix: String,
    cache: HashMap<(i32, Vec<u8>), String>,
}

impl Resolver {
    /// The logical path for a path seen through the top-level mount, if it's in scope.
    fn logical(&self, seen: &str) -> Option<String> {
        let rest = seen.strip_prefix(&self.prefix)?;
        if rest.is_empty() {
            Some("/".to_owned())
        } else if rest.starts_with('/') {
            Some(rest.to_owned())
        } else {
            None
        }
    }

    /// The directory's open fd and logical path, for a record on the tracked subvolume.
    fn dir(&mut self, fid: &Fid) -> Option<(OwnedFd, String)> {
        let fd = open_by_handle(
            self.mount.as_fd(),
            &fid.handle,
            libc::O_PATH | libc::O_DIRECTORY,
        )
        .ok()?;
        let key = (fid.handle.handle_type, fid.handle.bytes.clone());
        if let Some(p) = self.cache.get(&key) {
            return Some((fd, p.clone()));
        }
        let seen = std::fs::read_link(format!("/proc/self/fd/{}", fd.as_raw_fd())).ok()?;
        let seen = seen.to_str()?;
        if seen.ends_with(" (deleted)") {
            return None;
        }
        let path = self.logical(seen)?;
        if self.cache.len() >= DIR_CACHE_MAX {
            self.cache.clear();
        }
        self.cache.insert(key, path.clone());
        Some((fd, path))
    }

    fn full(&mut self, fid: &Fid) -> Option<(OwnedFd, String, Vec<u8>)> {
        let (fd, dir) = self.dir(fid)?;
        let name = fid.name.clone().unwrap_or_default();
        let path = if name.is_empty() || name == b"." {
            dir
        } else if dir == "/" {
            format!("/{}", String::from_utf8_lossy(&name))
        } else {
            format!("{dir}/{}", String::from_utf8_lossy(&name))
        };
        Some((fd, path, name))
    }
}

struct Capture {
    engine: Arc<Engine>,
    live: Arc<LiveState>,
    group: Group,
    lineage: Lineage,
    table: ProcTable,
    resolver: Resolver,
    builder: Builder,
    blobs: Blobs,
    self_pid: u32,
    last_event_ms: i64,
    ring_drops_seen: u64,
}

impl Capture {
    fn store(&self, closed: Vec<OpenCause>) {
        for c in closed {
            if c.changes.is_empty() && c.cause.package_paths == 0 {
                continue;
            }
            if let Err(e) = self.engine.store_cause(&c.cause, &c.changes) {
                tracing::error!(err = %e, "storing a cause failed");
            }
        }
    }

    fn handle(&mut self, ev: &osreflog_fanotify::Event, now: Instant) {
        let ts = now_ms();
        if ev.is_overflow() {
            self.live.stats().overflows += 1;
            if let Err(e) = self
                .engine
                .add_gap(self.last_event_ms, ts, "fanotify queue overflow")
            {
                tracing::error!(err = %e, "recording a gap failed");
            }
            return;
        }
        self.last_event_ms = ts;
        self.live.stats().events += 1;
        let pid = u32::try_from(ev.pid).unwrap_or(0);
        if pid == self.self_pid {
            return;
        }
        let Some(op) = op_of(ev.mask) else { return };
        let (target, old) = if op == "rename" {
            (ev.fid(INFO_NEW_DFID_NAME), ev.fid(INFO_OLD_DFID_NAME))
        } else {
            (ev.fid(INFO_DFID_NAME), None)
        };
        let Some(target) = target else { return };
        if ev.is_dir() && matches!(op, "rename" | "delete") {
            self.resolver.cache.clear();
        }
        let Some((dirfd, path, name)) = self.resolver.full(target) else {
            // Another subvolume, or a directory that's already gone.
            self.live.stats().out_of_scope += 1;
            return;
        };
        let from = old.and_then(|o| self.resolver.full(o)).map(|(_, p, _)| p);
        let class = class_of(&path);
        if class == Class::Noise {
            self.live.stats().out_of_scope += 1;
            return;
        }

        if self.table.chain(pid).is_empty()
            && let Some(p) = ProcTable::read_proc(pid)
        {
            self.table.insert(p);
        }
        let classified = {
            let chain = self.table.chain(pid);
            causes::classify(&chain, &self.live.lock_brackets())
        };
        if class == Class::Package && classified.kind == "dnf" {
            self.builder.count_package_path(&classified, ts, now);
            return;
        }

        let is_dir = ev.is_dir();
        let wants_content =
            class == Class::Config && !is_dir && (ev.mask & FAN_CLOSE_WRITE != 0 || op == "rename");
        let (blob, size) = if wants_content {
            self.blobs
                .capture(dirfd.as_fd(), OsStr::from_bytes(&name), &path)
                .map_or((None, None), |(h, s)| (Some(h), Some(s)))
        } else {
            (None, None)
        };
        let change = LiveChange {
            ts_ms: ts,
            path,
            op: op.to_owned(),
            from,
            kind: if is_dir { "dir" } else { "file" }.to_owned(),
            blob,
            size,
            actor: self
                .table
                .get(pid)
                .map(|p| format!("{} (pid {pid})", p.comm)),
        };
        self.builder.add(&classified, change, now);
        self.live.stats().recorded += 1;
    }

    fn tick(&mut self, now: Instant) {
        let closed = {
            let brackets = self.live.lock_brackets().clone();
            self.builder.take_closed(now, &brackets)
        };
        if !closed.is_empty() {
            let finished: Vec<u32> = closed
                .iter()
                .filter_map(|c| match c.key {
                    causes::Key::Package(pid) => Some(pid),
                    _ => None,
                })
                .collect();
            self.store(closed);
            let mut b = self.live.lock_brackets();
            for pid in finished {
                b.ended.remove(&pid);
            }
        }
        // A transaction that wrote nothing in scope still has to stop being "ended".
        {
            let mut b = self.live.lock_brackets();
            let open: Vec<u32> = b
                .ended
                .iter()
                .copied()
                .filter(|pid| !self.builder.has_package(*pid))
                .collect();
            for pid in open {
                b.ended.remove(&pid);
            }
        }
        let flushed = self.builder.oldest_open_ms().map_or_else(now_ms, |t| t - 1);
        self.live.flushed_ms.store(flushed, Ordering::Relaxed);
    }

    fn housekeeping(&mut self, now: Instant) {
        self.table.gc(now);
        let drops = self.lineage.drops();
        if drops > self.ring_drops_seen {
            let ts = now_ms();
            let _ = self.engine.add_gap(
                ts - i64::try_from(GC_EVERY.as_millis()).unwrap_or(0),
                ts,
                "lineage ring buffer full",
            );
            self.ring_drops_seen = drops;
        }
        let mut s = self.live.stats();
        s.ring_drops = drops;
        s.open_causes = self.builder.open_count() as u64;
        s.processes = self.table.len() as u64;
    }

    fn run(mut self) {
        let mut buf = vec![0u8; READ_BUF];
        let mut last_gc = Instant::now();
        let mut last_beat = Instant::now()
            .checked_sub(HEARTBEAT_EVERY)
            .unwrap_or_else(Instant::now);
        while !self.live.stop.load(Ordering::Relaxed) {
            {
                let mut fds = [
                    PollFd::new(&self.group, PollFlags::IN),
                    PollFd::from_borrowed_fd(self.lineage.fd(), PollFlags::IN),
                ];
                let timeout = rustix::event::Timespec {
                    tv_sec: 0,
                    tv_nsec: i64::try_from(TICK.as_nanos()).unwrap_or(0),
                };
                let _ = poll(&mut fds, Some(&timeout));
            }
            self.lineage.drain(&mut self.table);
            let now = Instant::now();
            loop {
                match self.group.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => match osreflog_fanotify::parse(&buf[..n]) {
                        Ok(events) => {
                            for ev in &events {
                                self.handle(ev, now);
                            }
                        }
                        Err(e) => tracing::warn!(?e, "malformed fanotify buffer"),
                    },
                    Err(e) => {
                        tracing::error!(err = %e, "reading fanotify events");
                        break;
                    }
                }
            }
            self.tick(now);
            if now.duration_since(last_gc) >= GC_EVERY {
                self.housekeeping(now);
                last_gc = now;
            }
            if now.duration_since(last_beat) >= HEARTBEAT_EVERY {
                self.engine.heartbeat();
                last_beat = now;
            }
        }
        let rest = self.builder.take_all();
        self.store(rest);
        self.live.flushed_ms.store(now_ms(), Ordering::Relaxed);
        self.live.running.store(false, Ordering::Relaxed);
        tracing::info!("live witness stopped");
    }
}

/// Starts the live witness for the filesystem mounted at `root`. Needs root (fanotify
/// filesystem marks, `open_by_handle_at`, eBPF).
///
/// # Errors
/// If fanotify or the eBPF program can't be set up; the daemon then runs with the state
/// witness only.
pub fn spawn(
    engine: Arc<Engine>,
    live: Arc<LiveState>,
    top: &Path,
    subvolume: &str,
    blob_dir: &Path,
) -> anyhow::Result<JoinHandle<()>> {
    use anyhow::Context;
    let group = Group::new().context("creating the fanotify group")?;
    group
        .mark_filesystem(top, MASK)
        .with_context(|| format!("marking the filesystem through {}", top.display()))?;
    let lineage = Lineage::load()?;
    let mut table = ProcTable::default();
    table.seed();
    let blobs = Blobs::new(blob_dir).context("creating the blob store")?;
    let resolver = Resolver {
        mount: File::open(top).context("opening the top-level mount")?,
        prefix: format!(
            "{}/{}",
            top.display().to_string().trim_end_matches('/'),
            subvolume
        ),
        cache: HashMap::new(),
    };
    tracing::info!(
        processes = table.len(),
        "live witness watching subvolume {subvolume} through {}",
        top.display()
    );
    live.flushed_ms.store(now_ms(), Ordering::Relaxed);
    live.running.store(true, Ordering::Relaxed);
    live.stats().running = true;
    let capture = Capture {
        engine,
        live,
        group,
        lineage,
        table,
        resolver,
        builder: Builder::default(),
        blobs,
        self_pid: std::process::id(),
        last_event_ms: now_ms(),
        ring_drops_seen: 0,
    };
    Ok(std::thread::Builder::new()
        .name("live-witness".into())
        .spawn(move || capture.run())?)
}

/// For tests and `osreflogd watch`: where blobs live under a ledger directory.
#[must_use]
pub fn blob_dir(ledger_dir: &Path) -> PathBuf {
    ledger_dir.join("blobs")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_tracked_subvolume_is_in_scope() {
        let r = Resolver {
            mount: File::open("/").unwrap(),
            prefix: "/run/osreflog/top/root".into(),
            cache: HashMap::new(),
        };
        assert_eq!(
            r.logical("/run/osreflog/top/root/etc/hosts").as_deref(),
            Some("/etc/hosts")
        );
        assert_eq!(r.logical("/run/osreflog/top/root").as_deref(), Some("/"));
        assert_eq!(
            r.logical("/run/osreflog/top/rootfs/etc"),
            None,
            "a sibling with a longer name"
        );
        assert_eq!(r.logical("/run/osreflog/top/home/user/.bashrc"), None);
        assert_eq!(
            r.logical("/run/osreflog/top/osreflog/ledger/ledger.sqlite"),
            None
        );
    }

    #[test]
    fn operations_by_precedence() {
        assert_eq!(op_of(FAN_CREATE | FAN_CLOSE_WRITE), Some("create"));
        assert_eq!(op_of(FAN_CLOSE_WRITE | FAN_ATTRIB), Some("modify"));
        assert_eq!(op_of(FAN_DELETE | FAN_ATTRIB), Some("delete"));
        assert_eq!(op_of(FAN_RENAME), Some("rename"));
        assert_eq!(op_of(0), None);
    }
}
