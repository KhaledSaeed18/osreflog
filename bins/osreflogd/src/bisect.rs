//! A bisect session: the two-level search over checkpoints, then causes, driving one probe at
//! a time. Probes are built here (the daemon has the btrfs top level) and run as transient
//! systemd units by `osreflogd --probe-runner` (see `runner.rs`). One session at a time; its
//! status is kept in memory for `osreflog bisect status`.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::sync::{Arc, Mutex, PoisonError};

use osreflog_bisect::{
    Args, CauseTouches, Culprit, Finding, Outcome, ProbeRecord, Search, Status, Tier, Touch,
    Verdict, replay,
};
use osreflog_ledger::refs::Resolved;
use osreflog_model::{Checkpoint, Class, classify};

use crate::blobs::Blobs;
use crate::engine::{Engine, EngineError, now_ms};
use crate::probe;
use crate::runner::{FLAG, RESULT_FILE, RunResult, Spec};

type Result<T> = std::result::Result<T, EngineError>;

/// Guest RAM for a VM probe (P1: a 512 MiB guest boots with the host-only initramfs).
const VM_MEM_MIB: u64 = 512;
const MAX_CULPRIT_PATHS: usize = 40;

/// Milliseconds as seconds with one decimal.
fn secs(ms: u64) -> String {
    format!("{}.{} s", ms / 1000, ms % 1000 / 100)
}

fn invalid(msg: impl Into<String>) -> EngineError {
    EngineError::Invalid(msg.into())
}

pub type Shared = Arc<Mutex<Option<Status>>>;

fn lock(s: &Shared) -> std::sync::MutexGuard<'_, Option<Status>> {
    s.lock().unwrap_or_else(PoisonError::into_inner)
}

/// One running session, as the worker thread sees it.
struct Session<'a> {
    engine: &'a Engine,
    shared: Shared,
    args: Args,
    probes_made: u32,
}

impl Session<'_> {
    fn log(&self, line: impl Into<String>) {
        let line = line.into();
        tracing::info!("bisect: {line}");
        if let Some(s) = lock(&self.shared).as_mut() {
            s.log.push(line);
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

    /// Builds, runs and removes one probe: a clone of `base`, with `ops` replayed from `bad`.
    fn probe(
        &mut self,
        what: String,
        base: &Checkpoint,
        replay: Option<(&Checkpoint, &[osreflog_bisect::Op])>,
    ) -> Result<ProbeRecord> {
        self.probes_made += 1;
        let name = format!("probe-{}-{}", std::process::id(), self.probes_made);
        let p = probe::clone_checkpoint(self.engine, &base.name, &name)
            .map_err(|e| invalid(format!("cloning {}: {e}", base.name)))?;
        let mut approximate = Vec::new();
        if let Some((bad, ops)) = replay {
            let blobs =
                Blobs::new(&self.engine.blob_dir()).map_err(|e| invalid(format!("blobs: {e}")))?;
            let bad_path = self.engine.layout().snapshot_path(&bad.name);
            let r = probe::replay(&p.path(), &bad_path, &blobs, ops)
                .map_err(|e| invalid(format!("replaying into {name}: {e}")))?;
            if !r.missing.is_empty() {
                self.log(format!(
                    "  {} path(s) couldn't be replayed (gone by {}): {}",
                    r.missing.len(),
                    bad.name,
                    r.missing
                        .iter()
                        .take(3)
                        .cloned()
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
            approximate = r.approximate;
        }
        probe::prepare(&p.path(), &self.args.test)
            .map_err(|e| invalid(format!("preparing {name}: {e}")))?;
        let result = self.run(&p);
        let verdict = result.verdict.unwrap_or(Verdict::Skip);
        let rec = ProbeRecord {
            what,
            verdict,
            ms: result.ms,
            peak_rss_kib: result.peak_rss_kib,
            approximate,
            detail: result.detail.clone(),
        };
        let mut line = format!("  {} → {verdict:?} in {}", rec.what, secs(rec.ms));
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
        if let Some(s) = lock(&self.shared).as_mut() {
            s.probes.push(rec.clone());
        }
        Ok(rec)
    }

    fn run(&self, p: &probe::Probe<'_>) -> RunResult {
        let (timeout_s, mem_mib) = match self.args.tier {
            Tier::Nspawn => (self.args.timeout_s.unwrap_or(120), 0),
            Tier::Vm => (self.args.timeout_s.unwrap_or(240), VM_MEM_MIB),
        };
        let spec = Spec {
            device: self.engine.layout().device.clone(),
            probe: p.top_relative(),
            tier: self.args.tier,
            boot: self.args.boot,
            timeout_s,
            mem_mib,
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
        let deadline = std::time::Duration::from_secs(timeout_s + 600);
        let rt = match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(rt) => rt,
            Err(e) => return fail(format!("runtime: {e}")),
        };
        let started = rt.block_on(start_unit(
            &unit, exe, &spec_json, mem_mib, timeout_s, deadline,
        ));
        if let Err(e) = started {
            return fail(format!("starting {unit}: {e}"));
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

    fn run_search(&mut self) -> Result<Finding> {
        let good = self.checkpoint(&self.args.good.clone())?;
        let bad = self.checkpoint(&self.args.bad.clone())?;
        if good.id >= bad.id {
            return Err(invalid(format!(
                "{} isn't older than {}",
                good.name, bad.name
            )));
        }
        let cps: Vec<Checkpoint> = self
            .engine
            .ledger()
            .checkpoints()?
            .into_iter()
            .filter(|c| c.pruned_ms.is_none() && c.id >= good.id && c.id <= bad.id)
            .collect();
        self.log(format!(
            "good {} … bad {}: {} checkpoints, tier {:?}",
            good.name,
            bad.name,
            cps.len(),
            self.args.tier
        ));

        // The bad end must fail in this tier, or there's nothing to find.
        let end = self.probe(format!("checkpoint {} (the bad end)", bad.name), &bad, None)?;
        match end.verdict {
            Verdict::Good => {
                return Ok(Finding::NotReproduced {
                    detail: format!(
                        "the test passes on {} in the {:?} tier; try another tier",
                        bad.name, self.args.tier
                    ),
                });
            }
            Verdict::Skip => {
                return Err(invalid(format!(
                    "the bad end {} couldn't be tested: {}",
                    bad.name, end.detail
                )));
            }
            Verdict::Bad => {}
        }

        // Level 1: checkpoints.
        let mut search = Search::new(cps.len() - 1);
        while let Some(i) = search.next() {
            let rec = self.probe(format!("checkpoint {}", cps[i].name), &cps[i], None)?;
            search.record(i, rec.verdict);
        }
        let (from, to) = match search.outcome() {
            Outcome::FirstBad { position } => (&cps[position - 1], &cps[position]),
            Outcome::Ambiguous {
                last_good,
                first_bad,
            } => {
                return Ok(Finding::Checkpoint {
                    last_good: cps[last_good].name.clone(),
                    first_bad: cps[first_bad].name.clone(),
                    why: "every checkpoint between them was skipped".into(),
                });
            }
        };
        self.log(format!(
            "first bad checkpoint {} (after {})",
            to.name, from.name
        ));

        self.cause_search(from, to)
    }

    /// Level 2: the causes between the last good and the first bad checkpoint, replayed onto
    /// the good one.
    fn cause_search(&mut self, from: &Checkpoint, to: &Checkpoint) -> Result<Finding> {
        let causes = self.interval_causes(from, to)?;
        if causes.is_empty() {
            return Ok(Finding::Checkpoint {
                last_good: from.name.clone(),
                first_bad: to.name.clone(),
                why: "no recorded cause in between: see osreflog diff for what changed".into(),
            });
        }
        self.log(format!(
            "{} causes between {} and {}",
            causes.len(),
            from.name,
            to.name
        ));
        let touches: Vec<CauseTouches> = causes.iter().map(|(_, t)| t.clone()).collect();
        let n = causes.len();
        let mut search = Search::new(n);
        let mut tested_end = false;
        let mut approx_at: BTreeMap<usize, Vec<String>> = BTreeMap::new();
        loop {
            let k = match search.next() {
                Some(k) => k,
                // "After all causes" was assumed bad; if it decides the answer, test it.
                None if search.bounds().1 == n && !tested_end && n > 0 => {
                    tested_end = true;
                    n
                }
                None => break,
            };
            let ops = replay::plan(&touches, k);
            let label = format!("after #{} ({k} of {n} causes)", causes[k - 1].0.id);
            let rec = self.probe(label, from, Some((to, &ops)))?;
            approx_at.insert(k, rec.approximate.clone());
            if k == n && rec.verdict == Verdict::Good {
                return Ok(Finding::Checkpoint {
                    last_good: from.name.clone(),
                    first_bad: to.name.clone(),
                    why: format!(
                        "the test passes with all {n} recorded causes replayed: what broke it isn't a recorded cause (see osreflog diff {}..{})",
                        from.name, to.name
                    ),
                });
            }
            search.record(k, rec.verdict);
        }
        Ok(match search.outcome() {
            Outcome::FirstBad { position } => {
                let mut approximate = approx_at.get(&position).cloned().unwrap_or_default();
                approximate.extend(approx_at.get(&(position - 1)).cloned().unwrap_or_default());
                approximate.sort();
                approximate.dedup();
                Finding::Cause {
                    culprit: causes[position - 1].0.clone(),
                    between: (from.name.clone(), to.name.clone()),
                    approximate,
                }
            }
            Outcome::Ambiguous {
                last_good,
                first_bad,
            } => Finding::Causes {
                candidates: causes[last_good..first_bad]
                    .iter()
                    .map(|(c, _)| c.clone())
                    .collect(),
            },
        })
    }
}

/// Starts the runner as a transient unit through systemd's D-Bus API and waits until the unit
/// is gone. `PrivateMounts`: the runner mounts the btrfs top level in its own namespace;
/// `Delegate`: nspawn `--keep-unit` makes sub-cgroups for the container.
async fn start_unit(
    unit: &str,
    exe: std::path::PathBuf,
    spec_json: &str,
    mem_mib: u64,
    timeout_s: u64,
    deadline: std::time::Duration,
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
        ("RuntimeMaxUSec", Value::from((timeout_s + 600) * 1_000_000)),
        ("CollectMode", Value::from("inactive-or-failed")),
    ];
    let aux: Vec<(&str, Vec<(&str, Value<'_>)>)> = Vec::new();
    let manager = |method: &'static str| {
        (
            Some("org.freedesktop.systemd1"),
            "/org/freedesktop/systemd1",
            Some("org.freedesktop.systemd1.Manager"),
            method,
        )
    };
    let (d, path, iface, method) = manager("StartTransientUnit");
    conn.call_method(d, path, iface, method, &(unit, "fail", props, aux))
        .await?;
    // Wait until the unit has ended (and been collected).
    let started = std::time::Instant::now();
    loop {
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        let (d, path, iface, method) = manager("GetUnit");
        let Ok(reply) = conn.call_method(d, path, iface, method, &(unit,)).await else {
            return Ok(());
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
        if matches!(state.as_str(), "inactive" | "failed") {
            return Ok(());
        }
        if started.elapsed() > deadline {
            let (d, path, iface, method) = manager("StopUnit");
            let _ = conn
                .call_method(d, path, iface, method, &(unit, "replace"))
                .await;
            return Ok(());
        }
    }
}

/// Starts a session in the background.
///
/// # Errors
/// If one is already running.
pub fn start(engine: Arc<Engine>, shared: &Shared, args: Args) -> Result<Status> {
    {
        let mut s = lock(shared);
        if s.as_ref().is_some_and(|s| s.running) {
            return Err(invalid(
                "a bisect is already running: osreflog bisect status",
            ));
        }
        if args.test.trim().is_empty() {
            return Err(invalid("no test given"));
        }
        *s = Some(Status {
            args: args.clone(),
            running: true,
            started_ms: now_ms(),
            log: Vec::new(),
            probes: Vec::new(),
            finding: None,
            error: None,
        });
    }
    let shared2 = Arc::clone(shared);
    let spawned = std::thread::Builder::new()
        .name("bisect".into())
        .spawn(move || {
            let mut session = Session {
                engine: &engine,
                shared: Arc::clone(&shared2),
                args,
                probes_made: 0,
            };
            let result = session.run_search();
            if let Some(s) = lock(&shared2).as_mut() {
                s.running = false;
                match result {
                    Ok(f) => s.finding = Some(f),
                    Err(e) => s.error = Some(e.to_string()),
                }
            }
        });
    if let Err(e) = spawned {
        let mut s = lock(shared);
        *s = None;
        return Err(invalid(format!("starting the bisect: {e}")));
    }
    lock(shared)
        .clone()
        .ok_or_else(|| invalid("the bisect vanished"))
}
