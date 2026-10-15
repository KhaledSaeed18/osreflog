//! Process lineage: who every pid is, and who started it, kept from the moment it forks until
//! a grace period after it exits. Fed by `bpf/lineage.bpf.c` through a ring buffer and seeded
//! from `/proc` at startup for processes that already exist.

use std::collections::HashMap;
use std::fs;
use std::time::{Duration, Instant};

use anyhow::Context;
use aya::maps::{Array, MapData, RingBuf};
use aya::programs::BtfTracePoint;
use aya::{Btf, Ebpf, include_bytes_aligned};

const OBJECT: &[u8] = include_bytes_aligned!(concat!(env!("OUT_DIR"), "/lineage.bpf.o"));

/// How long an exited process stays known: its file events may still be in the fanotify
/// queue, and its children may outlive it.
pub const EXIT_GRACE: Duration = Duration::from_secs(30);
/// Bound on remembered processes (live ones are never evicted by this bound alone).
const MAX_PROCS: usize = 65_536;
const MAX_CHAIN: usize = 64;

// `struct event` in bpf/lineage.bpf.c.
const EVENT_LEN: usize = 568;
const OFF_KIND: usize = 0;
const OFF_PID: usize = 4;
const OFF_PPID: usize = 8;
const OFF_UID: usize = 12;
const OFF_CGROUP: usize = 24;
const OFF_COMM: usize = 32;
const OFF_FILENAME: usize = 48;
const OFF_ARGS_LEN: usize = 304;
const OFF_ARGS: usize = 312;
const KIND_FORK: u8 = 1;
const KIND_EXEC: u8 = 2;
const KIND_EXIT: u8 = 3;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Proc {
    pub pid: u32,
    pub ppid: u32,
    pub comm: String,
    pub exe: String,
    pub args: Vec<String>,
    pub uid: u32,
    /// cgroup v2 path, e.g. `/system.slice/tlp.service`; empty when unknown.
    pub cgroup: String,
    pub cgroup_id: u64,
    pub exited: Option<Instant>,
}

/// One decoded ring-buffer record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record {
    pub kind: u8,
    pub pid: u32,
    pub ppid: u32,
    pub uid: u32,
    pub cgroup_id: u64,
    pub comm: String,
    pub filename: String,
    pub args: Vec<String>,
}

fn cstr(bytes: &[u8]) -> String {
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..end]).into_owned()
}

fn le_u32(b: &[u8], at: usize) -> u32 {
    b.get(at..at + 4)
        .and_then(|s| s.try_into().ok())
        .map_or(0, u32::from_ne_bytes)
}

fn le_u64(b: &[u8], at: usize) -> u64 {
    b.get(at..at + 8)
        .and_then(|s| s.try_into().ok())
        .map_or(0, u64::from_ne_bytes)
}

/// Splits a NUL-separated argv (possibly cut short) into arguments.
#[must_use]
pub fn split_args(raw: &[u8]) -> Vec<String> {
    raw.split(|&b| b == 0)
        .filter(|a| !a.is_empty())
        .map(|a| String::from_utf8_lossy(a).into_owned())
        .collect()
}

/// Decodes a ring-buffer record; `None` if it's too short.
#[must_use]
pub fn decode(b: &[u8]) -> Option<Record> {
    if b.len() < EVENT_LEN {
        return None;
    }
    let args_len = (le_u32(b, OFF_ARGS_LEN) as usize).min(EVENT_LEN - OFF_ARGS);
    Some(Record {
        kind: b[OFF_KIND],
        pid: le_u32(b, OFF_PID),
        ppid: le_u32(b, OFF_PPID),
        uid: le_u32(b, OFF_UID),
        cgroup_id: le_u64(b, OFF_CGROUP),
        comm: cstr(&b[OFF_COMM..OFF_FILENAME]),
        filename: cstr(&b[OFF_FILENAME..OFF_ARGS_LEN]),
        args: split_args(&b[OFF_ARGS..OFF_ARGS + args_len]),
    })
}

/// A process's cgroup v2 path from `/proc/<pid>/cgroup`.
#[must_use]
pub fn proc_cgroup(pid: u32) -> Option<String> {
    let text = fs::read_to_string(format!("/proc/{pid}/cgroup")).ok()?;
    text.lines()
        .find_map(|l| l.strip_prefix("0::"))
        .map(str::to_owned)
}

/// Everything known about live and recently exited processes.
#[derive(Debug, Default)]
pub struct ProcTable {
    procs: HashMap<u32, Proc>,
}

impl ProcTable {
    #[must_use]
    pub fn get(&self, pid: u32) -> Option<&Proc> {
        self.procs.get(&pid)
    }

    pub fn insert(&mut self, p: Proc) {
        self.procs.insert(p.pid, p);
    }

    /// Applies one ring-buffer record. `cgroup_of` reads a live process's cgroup path.
    pub fn apply(&mut self, r: Record, now: Instant, cgroup_of: impl Fn(u32) -> Option<String>) {
        match r.kind {
            KIND_FORK => {
                let parent = self.procs.get(&r.ppid).cloned();
                let child = Proc {
                    pid: r.pid,
                    ppid: r.ppid,
                    comm: r.comm,
                    exe: parent.as_ref().map(|p| p.exe.clone()).unwrap_or_default(),
                    args: parent.as_ref().map(|p| p.args.clone()).unwrap_or_default(),
                    uid: r.uid,
                    cgroup: parent
                        .as_ref()
                        .map(|p| p.cgroup.clone())
                        .unwrap_or_default(),
                    cgroup_id: r.cgroup_id,
                    exited: None,
                };
                self.procs.insert(r.pid, child);
            }
            KIND_EXEC => {
                let ppid = if r.ppid != 0 {
                    r.ppid
                } else {
                    self.procs.get(&r.pid).map_or(0, |p| p.ppid)
                };
                let inherited = self
                    .procs
                    .get(&r.pid)
                    .map(|p| p.cgroup.clone())
                    .unwrap_or_default();
                let cgroup = cgroup_of(r.pid).unwrap_or(inherited);
                self.procs.insert(
                    r.pid,
                    Proc {
                        pid: r.pid,
                        ppid,
                        comm: r.comm,
                        exe: r.filename,
                        args: r.args,
                        uid: r.uid,
                        cgroup,
                        cgroup_id: r.cgroup_id,
                        exited: None,
                    },
                );
            }
            KIND_EXIT => {
                if let Some(p) = self.procs.get_mut(&r.pid) {
                    p.exited = Some(now);
                }
            }
            _ => {}
        }
    }

    /// The process and its ancestors, nearest first, up to (not including) pid 0.
    #[must_use]
    pub fn chain(&self, pid: u32) -> Vec<&Proc> {
        let mut out = Vec::new();
        let mut cur = pid;
        while cur != 0 && out.len() < MAX_CHAIN {
            let Some(p) = self.procs.get(&cur) else { break };
            out.push(p);
            if p.ppid == cur {
                break;
            }
            cur = p.ppid;
        }
        out
    }

    /// Forgets processes that exited more than [`EXIT_GRACE`] ago (and, past the size bound,
    /// the oldest exited ones).
    pub fn gc(&mut self, now: Instant) {
        self.procs.retain(|_, p| {
            p.exited
                .is_none_or(|t| now.saturating_duration_since(t) < EXIT_GRACE)
        });
        if self.procs.len() > MAX_PROCS {
            let mut exited: Vec<(u32, Instant)> = self
                .procs
                .values()
                .filter_map(|p| p.exited.map(|t| (p.pid, t)))
                .collect();
            exited.sort_by_key(|(_, t)| *t);
            for (pid, _) in exited.into_iter().take(self.procs.len() - MAX_PROCS) {
                self.procs.remove(&pid);
            }
        }
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.procs.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.procs.is_empty()
    }

    /// Reads one process from `/proc` (used to seed the table and for unknown pids).
    #[must_use]
    pub fn read_proc(pid: u32) -> Option<Proc> {
        let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        // comm is in parentheses and may contain spaces or parentheses itself.
        let open = stat.find('(')?;
        let close = stat.rfind(')')?;
        let comm = stat[open + 1..close].to_owned();
        let parent: u32 = stat[close + 2..].split(' ').nth(1)?.parse().ok()?;
        let status = fs::read_to_string(format!("/proc/{pid}/status")).unwrap_or_default();
        let uid = status
            .lines()
            .find_map(|l| l.strip_prefix("Uid:"))
            .and_then(|v| v.split_whitespace().next())
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);
        Some(Proc {
            pid,
            ppid: parent,
            comm,
            exe: fs::read_link(format!("/proc/{pid}/exe"))
                .map(|p| p.display().to_string())
                .unwrap_or_default(),
            args: fs::read(format!("/proc/{pid}/cmdline"))
                .map(|b| split_args(&b))
                .unwrap_or_default(),
            uid,
            cgroup: proc_cgroup(pid).unwrap_or_default(),
            cgroup_id: 0,
            exited: None,
        })
    }

    /// Adds every process currently in `/proc`.
    pub fn seed(&mut self) {
        let Ok(dir) = fs::read_dir("/proc") else {
            return;
        };
        for entry in dir.flatten() {
            if let Some(pid) = entry
                .file_name()
                .to_str()
                .and_then(|s| s.parse::<u32>().ok())
                && let Some(p) = Self::read_proc(pid)
            {
                self.procs.insert(pid, p);
            }
        }
    }
}

/// The loaded eBPF program and its ring buffer.
pub struct Lineage {
    // Field order: the ring buffer and maps before the object that owns the programs.
    ring: RingBuf<MapData>,
    drops: Array<MapData, u64>,
    _ebpf: Ebpf,
}

impl Lineage {
    /// Loads and attaches the three tracepoints. Needs `CAP_BPF` and `CAP_PERFMON` (root).
    ///
    /// # Errors
    /// If the kernel lacks BTF or refuses the program.
    pub fn load() -> anyhow::Result<Self> {
        let mut ebpf = Ebpf::load(OBJECT).context("loading the lineage object")?;
        let btf = Btf::from_sys_fs().context("reading kernel BTF")?;
        for (name, tracepoint) in [
            ("on_fork", "sched_process_fork"),
            ("on_exec", "sched_process_exec"),
            ("on_exit", "sched_process_exit"),
        ] {
            let program: &mut BtfTracePoint = ebpf
                .program_mut(name)
                .with_context(|| format!("{name} missing from the object"))?
                .try_into()?;
            program
                .load(tracepoint, &btf)
                .with_context(|| format!("loading {name}"))?;
            program
                .attach()
                .with_context(|| format!("attaching {name}"))?;
        }
        let ring = RingBuf::try_from(ebpf.take_map("events").context("events map missing")?)?;
        let drops = Array::try_from(ebpf.take_map("drops").context("drops map missing")?)?;
        Ok(Self {
            ring,
            drops,
            _ebpf: ebpf,
        })
    }

    /// The ring buffer's fd, to poll on.
    #[must_use]
    pub fn fd(&self) -> std::os::fd::BorrowedFd<'_> {
        use std::os::fd::AsFd;
        self.ring.as_fd()
    }

    /// Applies every pending record to `table`.
    pub fn drain(&mut self, table: &mut ProcTable) -> usize {
        let now = Instant::now();
        let mut n = 0;
        while let Some(item) = self.ring.next() {
            if let Some(r) = decode(&item) {
                table.apply(r, now, proc_cgroup);
                n += 1;
            }
        }
        n
    }

    /// Records the kernel couldn't hand over because the ring buffer was full.
    #[must_use]
    pub fn drops(&self) -> u64 {
        self.drops.get(&0, 0).unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(
        kind: u8,
        pid: u32,
        parent: u32,
        comm: &str,
        filename: &str,
        args: &[&str],
    ) -> Vec<u8> {
        let mut b = vec![0u8; EVENT_LEN];
        b[OFF_KIND] = kind;
        b[OFF_PID..OFF_PID + 4].copy_from_slice(&pid.to_ne_bytes());
        b[OFF_PPID..OFF_PPID + 4].copy_from_slice(&parent.to_ne_bytes());
        b[OFF_UID..OFF_UID + 4].copy_from_slice(&1000u32.to_ne_bytes());
        b[OFF_COMM..OFF_COMM + comm.len()].copy_from_slice(comm.as_bytes());
        b[OFF_FILENAME..OFF_FILENAME + filename.len()].copy_from_slice(filename.as_bytes());
        let joined: Vec<u8> = args
            .iter()
            .flat_map(|a| a.bytes().chain(std::iter::once(0)))
            .collect();
        b[OFF_ARGS..OFF_ARGS + joined.len()].copy_from_slice(&joined);
        b[OFF_ARGS_LEN..OFF_ARGS_LEN + 4]
            .copy_from_slice(&u32::try_from(joined.len()).unwrap().to_ne_bytes());
        b
    }

    fn shell() -> Proc {
        Proc {
            pid: 100,
            ppid: 1,
            comm: "bash".into(),
            exe: "/usr/bin/bash".into(),
            args: vec!["bash".into()],
            uid: 1000,
            cgroup: "/user.slice/user-1000.slice/session-3.scope".into(),
            cgroup_id: 9,
            exited: None,
        }
    }

    #[test]
    fn fork_exec_exit_builds_a_chain() {
        let mut t = ProcTable::default();
        t.insert(shell());
        let now = Instant::now();
        let none = |_| None;
        t.apply(
            decode(&record(KIND_FORK, 200, 100, "bash", "", &[])).unwrap(),
            now,
            none,
        );
        assert_eq!(
            t.get(200).unwrap().exe,
            "/usr/bin/bash",
            "a fork inherits the parent's program"
        );
        t.apply(
            decode(&record(
                KIND_EXEC,
                200,
                100,
                "sed",
                "/usr/bin/sed",
                &["sed", "-i", "s/a/b/", "/etc/x"],
            ))
            .unwrap(),
            now,
            none,
        );
        let p = t.get(200).unwrap();
        assert_eq!((p.comm.as_str(), p.exe.as_str()), ("sed", "/usr/bin/sed"));
        assert_eq!(p.args, ["sed", "-i", "s/a/b/", "/etc/x"]);
        assert_eq!(
            p.cgroup, "/user.slice/user-1000.slice/session-3.scope",
            "inherited when /proc can't tell"
        );
        let chain: Vec<u32> = t.chain(200).iter().map(|p| p.pid).collect();
        assert_eq!(chain, [200, 100]);

        t.apply(
            decode(&record(KIND_EXIT, 200, 100, "sed", "", &[])).unwrap(),
            now,
            none,
        );
        t.gc(now + Duration::from_secs(10));
        assert!(t.get(200).is_some(), "kept during the grace period");
        t.gc(now + EXIT_GRACE + Duration::from_secs(1));
        assert!(t.get(200).is_none());
        assert!(t.get(100).is_some(), "live processes stay");
    }

    #[test]
    fn decode_rejects_short_records_and_bounds_args() {
        assert!(decode(&[0u8; 100]).is_none());
        let mut b = record(KIND_EXEC, 1, 0, "x", "/x", &["a"]);
        b[OFF_ARGS_LEN..OFF_ARGS_LEN + 4].copy_from_slice(&9999u32.to_ne_bytes());
        assert_eq!(decode(&b).unwrap().args, ["a"]);
    }

    #[test]
    fn seeds_from_proc() {
        let me = ProcTable::read_proc(std::process::id()).unwrap();
        assert_eq!(me.pid, std::process::id());
        assert!(me.exe.contains("osreflogd") || !me.exe.is_empty());
        let mut t = ProcTable::default();
        t.seed();
        assert!(t.len() > 10);
        assert_ne!(t.chain(std::process::id()).len(), 0);
    }
}
