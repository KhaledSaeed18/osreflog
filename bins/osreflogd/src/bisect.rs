//! A bisect session: the two-level search over checkpoints, then causes
//! ([`osreflog_bisect::TwoLevel`]), driving one probe at a time. Probes are built here (the
//! daemon has the btrfs top level) and run as transient systemd units by
//! `osreflogd --probe-runner` (see `runner.rs`). One session at a time.
//!
//! - **Automatic** (`bisect run`): each probe runs the test and gives its verdict at once; the
//!   session lives in memory.
//! - **Manual** (`bisect start`): each probe is a booted container (P7a; a reboot of the
//!   machine with `--tier metal`, P7b) that waits for a person's verdict. The verdict is a file
//!   in the probe: written there by `osreflog bisect good|bad|skip` inside the probe, or by the
//!   daemon for a verdict given on the host. The whole session (the search, the probes so far,
//!   the one waiting) is saved in the ledger after every step and resumed when the daemon
//!   starts, so it survives restarts and reboots.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use osreflog_bisect::{
    Answer, Args, CauseTouches, Culprit, Finding, ProbeRecord, Status, Step, Tier, Touch, TwoLevel,
    Verdict, Waiting, replay,
};
use osreflog_ledger::refs::Resolved;
use osreflog_model::{Checkpoint, Class, classify};
use serde::{Deserialize, Serialize};

use crate::blobs::Blobs;
use crate::bootdoctor::{PENDING, Pending};
use crate::engine::{Engine, EngineError, now_ms};
use crate::probe;
use crate::runner::{FLAG, RESULT_FILE, RunResult, Spec};

type Result<T> = std::result::Result<T, EngineError>;

/// Guest RAM for a VM probe (P1: a 512 MiB guest boots with the host-only initramfs).
const VM_MEM_MIB: u64 = 512;
const MAX_CULPRIT_PATHS: usize = 40;
/// The ledger `meta` key holding a manual session.
const SAVED_KEY: &str = "bisect.session";
/// A manual container probe may run this long before its unit is stopped.
const MANUAL_MAX_S: u64 = 7 * 24 * 3600;

/// Milliseconds as seconds with one decimal.
fn secs(ms: u64) -> String {
    format!("{}.{} s", ms / 1000, ms % 1000 / 100)
}

fn invalid(msg: impl Into<String>) -> EngineError {
    EngineError::Invalid(msg.into())
}

/// What the D-Bus side shares with the session's thread.
#[derive(Default)]
pub struct Inner {
    status: Mutex<Option<Status>>,
    /// Set by `bisect reset` to make a manual session's thread clean up and stop.
    cancel: AtomicBool,
}

pub type Shared = Arc<Inner>;

fn lock(s: &Shared) -> std::sync::MutexGuard<'_, Option<Status>> {
    s.status.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The current or last session, as clients see it.
#[must_use]
pub fn status(shared: &Shared) -> Option<Status> {
    lock(shared).clone()
}

/// The manual probe waiting for a verdict.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Current {
    n: u32,
    /// The clone's name in `osreflog/probes/`.
    clone: String,
    unit: String,
    machine: String,
    what: String,
    approximate: Vec<String>,
    since_ms: i64,
}

/// Everything a session is, saved between steps (manual) or kept in memory (automatic).
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Saved {
    status: Status,
    machine: TwoLevel,
    /// Checkpoint names by index: 0 the good end, last the bad end.
    checkpoints: Vec<String>,
    /// The causes of the interval the cause search runs in, once it does.
    culprits: Vec<Culprit>,
    /// Approximate paths of the cause probe after `k` causes.
    approx_at: BTreeMap<usize, Vec<String>>,
    current: Option<Current>,
    probes_made: u32,
}

struct Session<'a> {
    engine: &'a Engine,
    shared: Shared,
    saved: Saved,
    /// The interval's touches, recomputed from the ledger when a resumed session needs them.
    touches: Option<Vec<CauseTouches>>,
}

impl Session<'_> {
    fn manual(&self) -> bool {
        self.saved.status.args.manual
    }

    fn log(&mut self, line: impl Into<String>) {
        let line = line.into();
        tracing::info!("bisect: {line}");
        self.saved.status.log.push(line);
        self.publish();
    }

    /// Shows the session to clients and, for a manual one, saves it.
    fn publish(&self) {
        *lock(&self.shared) = Some(self.saved.status.clone());
        if self.manual() {
            match serde_json::to_string(&self.saved) {
                Ok(json) => {
                    if let Err(e) = self.engine.ledger().set_meta(SAVED_KEY, &json) {
                        tracing::error!("saving the bisect session: {e}");
                    }
                }
                Err(e) => tracing::error!("saving the bisect session: {e}"),
            }
        }
    }

    fn checkpoint(&self, r: &str) -> Result<Checkpoint> {
        match self.engine.resolve(r)? {
            Resolved::Checkpoint(c) if c.pruned_ms.is_none() => Ok(c),
            Resolved::Checkpoint(c) => Err(invalid(format!("{} was pruned", c.name))),
            Resolved::Live => Err(invalid(
                "bisect needs checkpoints on both ends; take one first: osreflog checkpoint",
            )),
        }
    }

    fn at(&self, index: usize) -> Result<Checkpoint> {
        let name = self
            .saved
            .checkpoints
            .get(index)
            .ok_or_else(|| invalid("checkpoint index out of range"))?
            .clone();
        self.checkpoint(&name)
    }

    /// Resolves the ends and lists the checkpoints between them.
    fn plan(&mut self) -> Result<()> {
        let args = self.saved.status.args.clone();
        let good = self.checkpoint(&args.good)?;
        let bad = self.checkpoint(&args.bad)?;
        if good.id >= bad.id {
            return Err(invalid(format!(
                "{} isn't older than {}",
                good.name, bad.name
            )));
        }
        let cps: Vec<String> = self
            .engine
            .ledger()
            .checkpoints()?
            .into_iter()
            .filter(|c| c.pruned_ms.is_none() && c.id >= good.id && c.id <= bad.id)
            .map(|c| c.name)
            .collect();
        self.log(format!(
            "good {} … bad {}: {} checkpoints, tier {:?}{}",
            good.name,
            bad.name,
            cps.len(),
            args.tier,
            if args.manual {
                ", verdicts by hand"
            } else {
                ""
            }
        ));
        self.saved.machine = TwoLevel::new(cps.len() - 1);
        self.saved.checkpoints = cps;
        Ok(())
    }

    /// Runs the search until it is done or (manual) a probe waits for a verdict.
    fn drive(&mut self) -> Result<()> {
        loop {
            match self.saved.machine.next() {
                Step::Checkpoint { index, bad_end } => {
                    let cp = self.at(index)?;
                    let what = if bad_end {
                        format!("checkpoint {} (the bad end)", cp.name)
                    } else {
                        format!("checkpoint {}", cp.name)
                    };
                    if !self.probe(what, &cp, None)? {
                        return Ok(());
                    }
                }
                Step::Causes { from, to } => {
                    let (from, to) = (self.at(from)?, self.at(to)?);
                    self.log(format!(
                        "first bad checkpoint {} (after {})",
                        to.name, from.name
                    ));
                    let causes = self.interval_causes(&from, &to)?;
                    if !causes.is_empty() {
                        self.log(format!(
                            "{} causes between {} and {}",
                            causes.len(),
                            from.name,
                            to.name
                        ));
                    }
                    self.saved.culprits = causes.iter().map(|(c, _)| c.clone()).collect();
                    self.touches = Some(causes.into_iter().map(|(_, t)| t).collect());
                    self.saved.machine.causes(self.saved.culprits.len());
                    self.publish();
                }
                Step::AfterCauses { from, to, k, n } => {
                    let (from, to) = (self.at(from)?, self.at(to)?);
                    if self.touches.is_none() {
                        let causes = self.interval_causes(&from, &to)?;
                        self.touches = Some(causes.into_iter().map(|(_, t)| t).collect());
                    }
                    let touches = self.touches.clone().unwrap_or_default();
                    let ops = replay::plan(&touches, k);
                    let id = self.saved.culprits.get(k - 1).map_or(0, |c| c.id);
                    let label = format!("after #{id} ({k} of {n} causes)");
                    if !self.probe(label, &from, Some((&to, &ops)))? {
                        return Ok(());
                    }
                }
                Step::Done(answer) => {
                    self.finish(&answer);
                    return Ok(());
                }
            }
        }
    }

    /// Builds one probe: a clone of `base` with `ops` replayed from `bad`. Automatic: runs it
    /// and records its verdict (true). Manual: leaves it booted and waiting (false).
    fn probe(
        &mut self,
        what: String,
        base: &Checkpoint,
        replay: Option<(&Checkpoint, &[osreflog_bisect::Op])>,
    ) -> Result<bool> {
        self.saved.probes_made += 1;
        let n = self.saved.probes_made;
        let manual = self.manual();
        let name = if manual {
            format!("p{n}")
        } else {
            format!("probe-{}-{n}", std::process::id())
        };
        let p = probe::clone_checkpoint(self.engine, &base.name, &name, manual)
            .map_err(|e| invalid(format!("cloning {}: {e}", base.name)))?;
        let mut approximate = Vec::new();
        if let Some((bad, ops)) = replay {
            let blobs =
                Blobs::new(&self.engine.blob_dir()).map_err(|e| invalid(format!("blobs: {e}")))?;
            let bad_path = self.engine.layout().snapshot_path(&bad.name);
            let r = probe::replay(&p.path(), &bad_path, &blobs, ops)
                .map_err(|e| invalid(format!("replaying into {name}: {e}")))?;
            if !r.missing.is_empty() {
                let some: Vec<String> = r.missing.iter().take(3).cloned().collect();
                self.log(format!(
                    "  {} path(s) couldn't be replayed (gone by {}): {}",
                    r.missing.len(),
                    bad.name,
                    some.join(", ")
                ));
            }
            approximate = r.approximate;
        }
        if let Step::AfterCauses { k, .. } = self.saved.machine.next() {
            self.saved.approx_at.insert(k, approximate.clone());
        }
        if manual {
            let path = p.path();
            let top_relative = p.top_relative();
            let clone = p.keep();
            self.wait_on(n, clone, &path, top_relative, what, approximate)?;
            return Ok(false);
        }
        probe::prepare(&p.path(), &self.saved.status.args.test)
            .map_err(|e| invalid(format!("preparing {name}: {e}")))?;
        let result = self.run(&p);
        let rec = ProbeRecord {
            what,
            verdict: result.verdict.unwrap_or(Verdict::Skip),
            ms: result.ms,
            peak_rss_kib: result.peak_rss_kib,
            approximate,
            detail: result.detail.clone(),
            output: result.output.clone(),
        };
        let mut line = format!("  {} → {:?} in {}", rec.what, rec.verdict, secs(rec.ms));
        if let Some(kib) = rec.peak_rss_kib {
            let _ = write!(line, ", qemu {} MiB", kib / 1024);
        }
        if let Some(ms) = result.initrd_ms {
            let _ = write!(
                line,
                " (initramfs for {} built in {})",
                result.kernel.as_deref().unwrap_or("?"),
                secs(ms)
            );
        }
        if !rec.detail.is_empty() {
            let _ = write!(line, ": {}", rec.detail);
        }
        self.log(line);
        self.record(rec);
        Ok(true)
    }

    fn record(&mut self, rec: ProbeRecord) {
        self.saved.machine.record(rec.verdict);
        self.saved.status.probes.push(rec);
        self.publish();
    }

    /// A manual probe: prepared as a container, booted, and waiting.
    fn wait_on(
        &mut self,
        n: u32,
        clone: String,
        path: &std::path::Path,
        top_relative: String,
        what: String,
        approximate: Vec<String>,
    ) -> Result<()> {
        let fail = |e: &dyn std::fmt::Display| invalid(format!("preparing probe {n}: {e}"));
        osreflog_boot::probe::prepare_container(path, n).map_err(|e| fail(&e))?;
        // The boot doctor keeps a clone only while pending.json names it.
        let pending = Pending {
            n,
            clone: clone.clone(),
            home: None,
            record: None,
        };
        let pending_json = serde_json::to_vec(&pending).map_err(|e| fail(&e))?;
        osreflog_boot::write_synced(&self.engine.layout().probes().join(PENDING), &pending_json)
            .map_err(|e| fail(&e))?;
        let machine = format!("osreflog-probe-{n}");
        let unit = format!("{machine}.service");
        let cur = Current {
            n,
            clone,
            unit,
            machine,
            what,
            approximate,
            since_ms: now_ms(),
        };
        self.start_container(&cur, top_relative)?;
        self.saved.status.waiting = Some(Waiting {
            probe: n,
            what: cur.what.clone(),
            since_ms: cur.since_ms,
            how: vec![
                format!("look into it:  sudo machinectl shell {}", cur.machine),
                "then, in there or here:  sudo osreflog bisect good | bad | skip".into(),
            ],
        });
        let line = format!(
            "probe {n}: {} is booting as container {}",
            cur.what, cur.machine
        );
        self.saved.current = Some(cur);
        self.log(line);
        Ok(())
    }

    fn start_container(&self, cur: &Current, top_relative: String) -> Result<()> {
        let spec = Spec {
            device: self.engine.layout().device.clone(),
            probe: top_relative,
            tier: Tier::Nspawn,
            boot: true,
            timeout_s: MANUAL_MAX_S,
            mem_mib: 0,
            manual: true,
            machine: cur.machine.clone(),
        };
        let exe = std::env::current_exe().map_err(|e| invalid(format!("the runner: {e}")))?;
        let spec_json = serde_json::to_string(&spec).map_err(|e| invalid(e.to_string()))?;
        block_on(start_unit(&cur.unit, exe, &spec_json, 1024, MANUAL_MAX_S))
            .map_err(|e| invalid(format!("starting {}: {e}", cur.unit)))
    }

    /// The verdict for the waiting probe: the container stopped, the clone deleted, the
    /// search moved on.
    fn give(&mut self, verdict: Verdict) {
        let Some(cur) = self.saved.current.take() else {
            return;
        };
        let ms = u64::try_from(now_ms() - cur.since_ms).unwrap_or(0);
        // A few seconds for `osreflog bisect …` inside the probe to say it was recorded, then
        // the container shuts down; its clone can only go once it has.
        std::thread::sleep(Duration::from_secs(3));
        let _ = block_on(async {
            stop_unit(&cur.unit).await?;
            wait_gone(&cur.unit, Duration::from_secs(60)).await
        });
        probe::delete_kept(self.engine, &cur.clone);
        let _ = std::fs::remove_file(self.engine.layout().probes().join(PENDING));
        self.saved.status.waiting = None;
        self.log(format!(
            "  {} → {verdict:?} (by hand, after {})",
            cur.what,
            secs(ms)
        ));
        self.record(ProbeRecord {
            what: cur.what,
            verdict,
            ms,
            peak_rss_kib: None,
            approximate: cur.approximate,
            detail: String::new(),
            output: String::new(),
        });
    }

    /// Ends the session with what the search found.
    fn finish(&mut self, answer: &Answer) {
        let name = |i: usize| self.saved.checkpoints.get(i).cloned().unwrap_or_default();
        let tier = self.saved.status.args.tier;
        let result = match *answer {
            Answer::NotReproduced => Ok(Finding::NotReproduced {
                detail: if self.manual() {
                    format!(
                        "you said good on the bad end {}",
                        name(self.saved.checkpoints.len() - 1)
                    )
                } else {
                    format!(
                        "the test passes on {} in the {tier:?} tier; try another tier",
                        name(self.saved.checkpoints.len() - 1)
                    )
                },
            }),
            Answer::BadEndUntestable => Err(format!(
                "the bad end {} couldn't be tested: {}",
                name(self.saved.checkpoints.len() - 1),
                self.saved
                    .status
                    .probes
                    .last()
                    .map(|p| p.detail.clone())
                    .unwrap_or_default()
            )),
            Answer::Checkpoints {
                last_good,
                first_bad,
            } => Ok(Finding::Checkpoint {
                last_good: name(last_good),
                first_bad: name(first_bad),
                why: "every checkpoint between them was skipped".into(),
            }),
            Answer::NoCauses { from, to } => Ok(Finding::Checkpoint {
                last_good: name(from),
                first_bad: name(to),
                why: "no recorded cause in between: see osreflog diff for what changed".into(),
            }),
            Answer::NotARecordedCause { from, to } => Ok(Finding::Checkpoint {
                last_good: name(from),
                first_bad: name(to),
                why: format!(
                    "the test passes with all {} recorded causes replayed: what broke it isn't a recorded cause (see osreflog diff {}..{})",
                    self.saved.culprits.len(),
                    name(from),
                    name(to)
                ),
            }),
            Answer::Cause { from, to, k } => {
                let mut approximate = self.saved.approx_at.get(&k).cloned().unwrap_or_default();
                approximate.extend(
                    self.saved
                        .approx_at
                        .get(&(k - 1))
                        .cloned()
                        .unwrap_or_default(),
                );
                approximate.sort();
                approximate.dedup();
                match self.saved.culprits.get(k - 1) {
                    Some(c) => Ok(Finding::Cause {
                        culprit: c.clone(),
                        between: (name(from), name(to)),
                        approximate,
                    }),
                    None => Err("the cause list changed under the session".into()),
                }
            }
            Answer::CauseRange {
                last_good,
                first_bad,
                ..
            } => Ok(Finding::Causes {
                candidates: self
                    .saved
                    .culprits
                    .get(last_good..first_bad)
                    .map(<[Culprit]>::to_vec)
                    .unwrap_or_default(),
            }),
        };
        let s = &mut self.saved.status;
        s.running = false;
        s.waiting = None;
        match result {
            Ok(f) => s.finding = Some(f),
            Err(e) => s.error = Some(e),
        }
        self.publish();
    }

    fn fail(&mut self, e: &EngineError) {
        let s = &mut self.saved.status;
        s.running = false;
        s.waiting = None;
        s.error = Some(e.to_string());
        self.publish();
    }

    fn run(&self, p: &probe::Probe<'_>) -> RunResult {
        let args = &self.saved.status.args;
        let (timeout_s, mem_mib) = match args.tier {
            Tier::Nspawn => (args.timeout_s.unwrap_or(120), 0),
            Tier::Vm => (args.timeout_s.unwrap_or(240), VM_MEM_MIB),
            Tier::Metal => (0, 0),
        };
        let spec = Spec {
            device: self.engine.layout().device.clone(),
            probe: p.top_relative(),
            tier: args.tier,
            boot: args.boot,
            timeout_s,
            mem_mib,
            manual: false,
            machine: String::new(),
        };
        let fail = |detail: String| RunResult {
            detail,
            ..RunResult::default()
        };
        let (Ok(exe), Ok(spec_json)) = (std::env::current_exe(), serde_json::to_string(&spec))
        else {
            return fail("can't start the runner".into());
        };
        let unit = format!("osreflog-{}.service", p.name);
        let result_file = p.path().join(probe::PROBE_DIR).join(RESULT_FILE);
        let _ = std::fs::remove_file(&result_file);
        let deadline = Duration::from_secs(timeout_s + 600);
        let ran = block_on(async {
            start_unit(&unit, exe, &spec_json, mem_mib, timeout_s + 600).await?;
            wait_gone(&unit, deadline).await
        });
        if let Err(e) = ran {
            return fail(format!("running {unit}: {e}"));
        }
        match std::fs::read(&result_file) {
            Ok(json) => serde_json::from_slice(&json)
                .unwrap_or_else(|e| fail(format!("the runner's result didn't parse: {e}"))),
            Err(_) => fail(format!(
                "the runner left no result: journalctl -u {unit} has its output"
            )),
        }
    }

    /// The causes of the interval `from..to` in order, with what each touched.
    fn interval_causes(
        &self,
        from: &Checkpoint,
        to: &Checkpoint,
    ) -> Result<Vec<(Culprit, CauseTouches)>> {
        self.engine.compute_pending()?;
        let l = self.engine.ledger();
        let interval = l.interval(from.id, to.id)?;
        let live = l.live_changes_between(from.created_ms, to.created_ms)?;
        let mut by_cause: BTreeMap<i64, Vec<Touch>> = BTreeMap::new();
        for (cause, c) in live {
            if classify(&c.path) == Class::Noise {
                continue;
            }
            by_cause.entry(cause).or_default().push(Touch {
                path: c.path,
                op: c.op,
                from: c.from,
                is_dir: c.kind == "dir",
                blob: c.blob,
                target: c.target,
            });
        }
        // Paths the state witness attributed to a cause without a live record of their own:
        // the package-owned files of a dnf transaction.
        if let Some(iv) = interval {
            for ch in iv.changes {
                let Some(cause) = ch.cause else { continue };
                if ch.class == Class::Noise {
                    continue;
                }
                let touches = by_cause.entry(cause.id).or_default();
                if !touches.iter().any(|t| t.path == ch.path) {
                    touches.push(Touch {
                        path: ch.path,
                        op: ch.op,
                        from: ch.from,
                        is_dir: ch.kind == "dir",
                        blob: None,
                        target: None,
                    });
                }
            }
        }
        let mut out = Vec::new();
        for (id, touches) in by_cause {
            let Some(c) = l.cause(id)? else { continue };
            let culprit = Culprit {
                id,
                kind: c.kind,
                label: c.label,
                started_ms: c.started_ms,
                paths: touches
                    .iter()
                    .take(MAX_CULPRIT_PATHS)
                    .map(|t| t.path.clone())
                    .collect(),
            };
            out.push((culprit, CauseTouches { id, touches }));
        }
        out.sort_by_key(|(c, _)| (c.started_ms, c.id));
        Ok(out)
    }
}

fn block_on<F: std::future::Future>(f: F) -> F::Output {
    match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt.block_on(f),
        Err(e) => panic!("a tokio runtime for the bisect thread: {e}"),
    }
}

fn manager(
    method: &'static str,
) -> (
    Option<&'static str>,
    &'static str,
    Option<&'static str>,
    &'static str,
) {
    (
        Some("org.freedesktop.systemd1"),
        "/org/freedesktop/systemd1",
        Some("org.freedesktop.systemd1.Manager"),
        method,
    )
}

/// Starts the runner as a transient unit through systemd's D-Bus API. `PrivateMounts`: the
/// runner mounts the btrfs top level in its own namespace; `Delegate`: nspawn `--keep-unit`
/// makes sub-cgroups for the container.
async fn start_unit(
    unit: &str,
    exe: std::path::PathBuf,
    spec_json: &str,
    mem_mib: u64,
    max_s: u64,
) -> zbus::Result<()> {
    use zbus::zvariant::Value;
    let conn = zbus::Connection::system().await?;
    let exe = exe.to_string_lossy().into_owned();
    let exec: Vec<(String, Vec<String>, bool)> = vec![(
        exe.clone(),
        vec![exe, FLAG.to_owned(), spec_json.to_owned()],
        false,
    )];
    let props: Vec<(&str, Value<'_>)> = vec![
        ("Description", Value::from("osreflog bisect probe")),
        ("ExecStart", Value::from(exec)),
        ("PrivateMounts", Value::from(true)),
        ("Delegate", Value::from(true)),
        ("Nice", Value::from(10i32)),
        ("MemoryMax", Value::from((mem_mib + 1024) * 1024 * 1024)),
        ("RuntimeMaxUSec", Value::from(max_s * 1_000_000)),
        ("CollectMode", Value::from("inactive-or-failed")),
    ];
    let aux: Vec<(&str, Vec<(&str, Value<'_>)>)> = Vec::new();
    let (d, path, iface, method) = manager("StartTransientUnit");
    conn.call_method(d, path, iface, method, &(unit, "fail", props, aux))
        .await?;
    Ok(())
}

/// Whether `unit` is loaded and not yet inactive or failed.
async fn unit_active(unit: &str) -> zbus::Result<bool> {
    let conn = zbus::Connection::system().await?;
    let (d, path, iface, method) = manager("GetUnit");
    let Ok(reply) = conn.call_method(d, path, iface, method, &(unit,)).await else {
        return Ok(false);
    };
    let unit_path: zbus::zvariant::OwnedObjectPath = reply.body().deserialize()?;
    let state = conn
        .call_method(
            Some("org.freedesktop.systemd1"),
            unit_path.as_str(),
            Some("org.freedesktop.DBus.Properties"),
            "Get",
            &("org.freedesktop.systemd1.Unit", "ActiveState"),
        )
        .await?;
    let state: zbus::zvariant::OwnedValue = state.body().deserialize()?;
    let state = String::try_from(state).unwrap_or_default();
    Ok(!matches!(state.as_str(), "inactive" | "failed"))
}

/// Waits until `unit` has ended (and been collected), stopping it after `deadline`.
async fn wait_gone(unit: &str, deadline: Duration) -> zbus::Result<()> {
    let started = std::time::Instant::now();
    loop {
        tokio::time::sleep(Duration::from_millis(500)).await;
        if !unit_active(unit).await? {
            return Ok(());
        }
        if started.elapsed() > deadline {
            return stop_unit(unit).await;
        }
    }
}

async fn stop_unit(unit: &str) -> zbus::Result<()> {
    let conn = zbus::Connection::system().await?;
    let (d, path, iface, method) = manager("StopUnit");
    let _ = conn
        .call_method(d, path, iface, method, &(unit, "replace"))
        .await;
    Ok(())
}

fn new_status(args: Args) -> Status {
    Status {
        args,
        running: true,
        started_ms: now_ms(),
        log: Vec::new(),
        probes: Vec::new(),
        finding: None,
        error: None,
        waiting: None,
    }
}

/// Starts a session in the background.
///
/// # Errors
/// If one is already running, or a manual session can't plan its first probe.
pub fn start(engine: Arc<Engine>, shared: &Shared, args: Args) -> Result<Status> {
    {
        let mut s = lock(shared);
        if s.as_ref().is_some_and(|s| s.running) {
            return Err(invalid(
                "a bisect is already running: osreflog bisect status",
            ));
        }
        if !args.manual && args.test.trim().is_empty() {
            return Err(invalid("no test given"));
        }
        if args.manual && args.tier != Tier::Nspawn {
            return Err(invalid(
                "verdicts by hand need --tier nspawn (a container you enter) or, from P7b, --tier metal",
            ));
        }
        *s = Some(new_status(args.clone()));
    }
    shared.cancel.store(false, Ordering::Relaxed);
    let saved = Saved {
        status: new_status(args),
        machine: TwoLevel::new(1),
        checkpoints: Vec::new(),
        culprits: Vec::new(),
        approx_at: BTreeMap::new(),
        current: None,
        probes_made: 0,
    };
    spawn(engine, shared, saved, true)?;
    status(shared).ok_or_else(|| invalid("the bisect vanished"))
}

/// Resumes a manual session saved in the ledger (after a restart or a reboot).
#[must_use]
pub fn resume(engine: &Arc<Engine>) -> Shared {
    let shared = Shared::default();
    let saved = match engine.ledger().meta(SAVED_KEY) {
        Ok(Some(json)) if !json.is_empty() => serde_json::from_str::<Saved>(&json),
        _ => return shared,
    };
    let saved = match saved {
        Ok(s) => s,
        Err(e) => {
            tracing::error!("the saved bisect session didn't parse, ignored: {e}");
            return shared;
        }
    };
    *lock(&shared) = Some(saved.status.clone());
    if saved.status.running {
        tracing::info!(
            probes = saved.status.probes.len(),
            "resuming the manual bisect"
        );
        if let Err(e) = spawn(Arc::clone(engine), &shared, saved, false) {
            tracing::error!("resuming the bisect: {e}");
        }
    }
    shared
}

fn spawn(engine: Arc<Engine>, shared: &Shared, saved: Saved, fresh: bool) -> Result<()> {
    let shared2 = Arc::clone(shared);
    let spawned = std::thread::Builder::new()
        .name("bisect".into())
        .spawn(move || {
            let mut session = Session {
                engine: &engine,
                shared: Arc::clone(&shared2),
                saved,
                touches: None,
            };
            if fresh {
                if let Err(e) = session.plan() {
                    session.fail(&e);
                    return;
                }
            } else if let Some(cur) = session.saved.current.clone() {
                // After a restart or a reboot: boot the waiting probe again unless its verdict
                // is already there or its container still runs.
                let path = probe::kept_path(&engine, &cur.clone);
                if osreflog_boot::probe::read_verdict(&path).is_none()
                    && !block_on(unit_active(&cur.unit)).unwrap_or(false)
                {
                    let rel = format!("osreflog/probes/{}", cur.clone);
                    match session.start_container(&cur, rel) {
                        Ok(()) => {
                            session.log(format!("probe {} booted again after a restart", cur.n));
                        }
                        Err(e) => {
                            session.log(format!("probe {} couldn't be booted again: {e}", cur.n));
                        }
                    }
                }
            }
            if session.manual() {
                manual_loop(&mut session);
            } else if let Err(e) = session.drive() {
                session.fail(&e);
            }
        });
    if let Err(e) = spawned {
        *lock(shared) = None;
        return Err(invalid(format!("starting the bisect: {e}")));
    }
    Ok(())
}

/// A manual session's thread: drive to the next waiting probe, wait for its verdict file,
/// give it, repeat; or clean up when reset.
fn manual_loop(session: &mut Session<'_>) {
    let mut stopped_said = false;
    loop {
        if session.shared.cancel.load(Ordering::Relaxed) {
            if let Some(cur) = session.saved.current.take() {
                let _ = block_on(stop_unit(&cur.unit));
                probe::delete_kept(session.engine, &cur.clone);
                let _ = std::fs::remove_file(session.engine.layout().probes().join(PENDING));
            }
            let _ = session.engine.ledger().set_meta(SAVED_KEY, "");
            *lock(&session.shared) = None;
            return;
        }
        let Some(cur) = session.saved.current.clone() else {
            if !session.saved.status.running {
                return;
            }
            if let Err(e) = session.drive() {
                session.fail(&e);
                return;
            }
            if session.saved.current.is_none() {
                return; // done
            }
            stopped_said = false;
            continue;
        };
        let path = probe::kept_path(session.engine, &cur.clone);
        if let Some(v) = osreflog_boot::probe::read_verdict(&path) {
            let verdict = match v.as_str() {
                "good" => Verdict::Good,
                "bad" => Verdict::Bad,
                _ => Verdict::Skip,
            };
            session.give(verdict);
            continue;
        }
        if !stopped_said && !block_on(unit_active(&cur.unit)).unwrap_or(true) {
            stopped_said = true;
            session.log(format!(
                "probe {}'s container stopped without a verdict: give it here, sudo osreflog bisect good | bad | skip",
                cur.n
            ));
        }
        std::thread::sleep(Duration::from_secs(1));
    }
}

/// A verdict given on the host for the waiting manual probe: written into the probe, where the
/// session's thread picks it up as it would one given inside.
///
/// # Errors
/// If no manual probe is waiting, or the verdict can't be written.
pub fn verdict(engine: &Engine, shared: &Shared, verdict: &str) -> Result<Status> {
    let Some(s) = status(shared) else {
        return Err(invalid("no bisect: osreflog bisect start"));
    };
    let Some(w) = s.waiting.as_ref() else {
        return Err(invalid(if s.running {
            "no probe is waiting for a verdict yet: osreflog bisect status"
        } else {
            "the bisect is over: osreflog bisect status"
        }));
    };
    let path = probe::kept_path(engine, &format!("p{}", w.probe));
    osreflog_boot::probe::write_verdict(&path, verdict)
        .map_err(|e| invalid(format!("recording the verdict: {e}")))?;
    Ok(s)
}

/// Forgets the session. A manual one is stopped first (its container, its clone, the saved
/// state); an automatic one that is still running is refused.
///
/// # Errors
/// If an automatic bisect is running.
pub fn reset(engine: &Engine, shared: &Shared) -> Result<()> {
    let Some(s) = status(shared) else {
        let _ = engine.ledger().set_meta(SAVED_KEY, "");
        return Ok(());
    };
    if !s.running {
        *lock(shared) = None;
        let _ = engine.ledger().set_meta(SAVED_KEY, "");
        return Ok(());
    }
    if !s.args.manual {
        return Err(invalid("a bisect is running; wait for it to finish"));
    }
    shared.cancel.store(true, Ordering::Relaxed);
    for _ in 0..100 {
        if status(shared).is_none() {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Err(invalid("the bisect didn't stop in 10 s; try again"))
}
