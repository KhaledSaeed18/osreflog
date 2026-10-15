//! Undo, the daemon's half: turning a cause into a plan, previewing it against the live
//! system, and applying it through a sandboxed worker after a verified `pre-undo`
//! checkpoint. The engine in `osreflog_undo` does the writing; see `docs/undo.md`.

use std::collections::BTreeSet;
use std::io::{Read as _, Write as _};
use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use osreflog_model::{Cause, Class, LiveChange, classify};
use osreflog_undo::plan::{self, ATTIC, MAX_FILE};
use osreflog_undo::{
    Applied, Delegation, Meta, Plan, Preview, PreviewStep, Root, Skipped, State, Step, Verdict,
    Version,
};
use rustix::fs::{OFlags, fgetxattr, flistxattr};

use crate::blobs::Blobs;
use crate::engine::{Engine, EngineError, now_ms};

pub const WORKER_FLAG: &str = "--undo-worker";
const WORKER_DEADLINE: Duration = Duration::from_secs(120);
/// Text shown in a preview diff, at most.
const MAX_TEXT: u64 = 256 << 10;

type Result<T> = std::result::Result<T, EngineError>;

fn invalid(msg: impl Into<String>) -> EngineError {
    EngineError::Invalid(msg.into())
}

/// Owner, permission bits and extended attributes of a file or directory (not a symlink).
fn meta_of(path: &Path) -> Option<Meta> {
    let m = std::fs::symlink_metadata(path).ok()?;
    if m.file_type().is_symlink() {
        return None;
    }
    let fd = rustix::fs::open(
        path,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
        rustix::fs::Mode::empty(),
    )
    .ok()?;
    let mut xattrs = Vec::new();
    let mut names = vec![0u8; 4096];
    if let Ok(n) = flistxattr(&fd, &mut names[..]) {
        for name in names[..n].split(|b| *b == 0).filter(|n| !n.is_empty()) {
            let Ok(name) = std::str::from_utf8(name) else {
                continue;
            };
            let mut value = vec![0u8; 4096];
            if let Ok(len) = fgetxattr(&fd, name, &mut value[..]) {
                xattrs.push((name.to_owned(), plan::to_hex(&value[..len])));
            }
        }
    }
    Some(Meta {
        uid: m.uid(),
        gid: m.gid(),
        mode: m.permissions().mode() & 0o7777,
        xattrs,
    })
}

fn default_meta(mode: u32) -> Meta {
    Meta {
        uid: 0,
        gid: 0,
        mode,
        xattrs: Vec::new(),
    }
}

/// The live root as the daemon sees it: the tracked subvolume through its private mount of
/// the top level.
fn live_root(engine: &Engine) -> PathBuf {
    let l = engine.layout();
    l.top.join(&l.subvolume)
}

/// A, from the record alone: `None` when the cause didn't record the content it left.
fn recorded_after(c: &LiveChange) -> Option<State> {
    if c.op == "delete" {
        return Some(State::Absent);
    }
    if c.kind == "dir" {
        return Some(State::Dir);
    }
    if let Some(t) = &c.target {
        return Some(State::Symlink { target: t.clone() });
    }
    c.blob.clone().map(|hash| State::File { hash })
}

struct Planner<'a> {
    engine: &'a Engine,
    blobs: Blobs,
    root: Root,
    root_path: PathBuf,
    cause: Cause,
}

impl Planner<'_> {
    /// B: what was at `path` before a change at `ts_ms` by a cause that started at
    /// `started_ms`, from the newest record since the path last went away, else the newest
    /// snapshot from before the cause.
    fn before(&self, path: &str, ts_ms: i64, is_dir: bool) -> std::result::Result<Version, String> {
        let (blob, target, gone, checkpoints) = {
            let l = self.engine.ledger();
            (
                l.previous_blob(path, ts_ms).map_err(|e| e.to_string())?,
                l.previous_target(path, ts_ms).map_err(|e| e.to_string())?,
                l.last_gone(path, ts_ms).map_err(|e| e.to_string())?,
                l.checkpoints().map_err(|e| e.to_string())?,
            )
        };
        let snapshot = checkpoints
            .iter()
            .rev()
            .find(|c| c.pruned_ms.is_none() && c.created_ms < self.cause.started_ms)
            .filter(|c| gone.is_none_or(|g| c.created_ms > g))
            .map(|c| {
                self.engine
                    .layout()
                    .snapshot_path(&c.name)
                    .join(path.trim_start_matches('/'))
            });
        let snap_meta = snapshot
            .as_ref()
            .and_then(|p| std::fs::symlink_metadata(p).ok());
        if is_dir || snap_meta.as_ref().is_some_and(std::fs::Metadata::is_dir) {
            let meta = snapshot
                .as_deref()
                .and_then(meta_of)
                .unwrap_or_else(|| default_meta(0o755));
            return Ok(Version::Dir { meta });
        }
        let snap_link = snap_meta
            .as_ref()
            .filter(|m| m.file_type().is_symlink())
            .and_then(|_| std::fs::read_link(snapshot.as_ref()?).ok())
            .map(|t| t.to_string_lossy().into_owned());
        if let Some(target) = target.or(snap_link) {
            let (uid, gid) = snap_meta.as_ref().map_or((0, 0), |m| (m.uid(), m.gid()));
            return Ok(Version::Symlink { target, uid, gid });
        }
        let content = match blob {
            Some(h) => self.blobs.read(&h),
            None => snapshot.as_ref().and_then(|p| {
                let m = snap_meta.as_ref()?;
                (m.is_file() && m.len() <= MAX_FILE as u64)
                    .then(|| std::fs::read(p).ok())
                    .flatten()
            }),
        };
        let Some(content) = content else {
            return Err("no earlier content recorded for it".into());
        };
        // Owner, mode and label: from the snapshot, else from the file as it is now (a
        // modify keeps them), else root's defaults.
        let meta = snapshot
            .as_deref()
            .filter(|_| snap_meta.as_ref().is_some_and(std::fs::Metadata::is_file))
            .and_then(meta_of)
            .or_else(|| meta_of(&self.root_path.join(path.trim_start_matches('/'))))
            .unwrap_or_else(|| default_meta(0o644));
        Ok(Version::file(&content, meta))
    }

    /// A for a change whose content wasn't recorded: what's there now, if nothing could have
    /// changed it since (no later record for the path, no gap in the watch).
    fn after_unrecorded(&self, c: &LiveChange) -> std::result::Result<State, String> {
        let l = self.engine.ledger();
        let later: Vec<i64> = l
            .history(&c.path, 20)
            .map_err(|e| e.to_string())?
            .into_iter()
            .filter(|(cause, ch)| cause.id != self.cause.id && ch.ts_ms > c.ts_ms)
            .map(|(cause, _)| cause.id)
            .collect();
        if let Some(id) = later.first() {
            return Err(format!(
                "its content wasn't recorded and #{id} changed it since: undo #{id} first"
            ));
        }
        if !l
            .gaps_between(c.ts_ms, now_ms())
            .map_err(|e| e.to_string())?
            .is_empty()
        {
            return Err(
                "its content wasn't recorded and osreflogd wasn't watching for part of the time since"
                    .into(),
            );
        }
        drop(l);
        let probe = Plan {
            undo_of: 0,
            attic: String::new(),
            steps: vec![Step {
                path: c.path.clone(),
                expect: State::Absent,
                restore: Version::Absent,
            }],
        };
        osreflog_undo::current(&self.root, &probe)
            .pop()
            .map_or_else(|| Err("unreadable".into()), |(_, s)| s)
    }

    fn plan(&self, changes: &[LiveChange]) -> (Plan, Vec<Skipped>) {
        let mut steps = Vec::new();
        let mut skipped = Vec::new();
        let mut skip = |path: &str, reason: String| {
            skipped.push(Skipped {
                path: path.to_owned(),
                reason,
            });
        };
        let paths: BTreeSet<&str> = changes.iter().map(|c| c.path.as_str()).collect();
        for c in changes {
            match classify(&c.path) {
                Class::Config => {}
                Class::Package => {
                    skip(
                        &c.path,
                        "package-owned: undo the package transaction instead".into(),
                    );
                    continue;
                }
                _ if c.path.starts_with(ATTIC.trim_end_matches('/')) => {
                    skip(&c.path, "the attic: kept".into());
                    continue;
                }
                _ => {
                    skip(
                        &c.path,
                        "not config: undo restores /etc, /opt and /usr/local".into(),
                    );
                    continue;
                }
            }
            let expect = match recorded_after(c).map_or_else(|| self.after_unrecorded(c), Ok) {
                Ok(s) => s,
                Err(reason) => {
                    skip(&c.path, reason);
                    continue;
                }
            };
            let restore = match c.op.as_str() {
                "create" | "rename" => Ok(Version::Absent),
                _ => self.before(&c.path, c.ts_ms, c.kind == "dir"),
            };
            match restore {
                Ok(restore) => steps.push(Step {
                    path: c.path.clone(),
                    expect,
                    restore,
                }),
                Err(reason) => skip(&c.path, reason),
            }
            // A move also puts the old name back, unless the cause changed that path too.
            if c.op == "rename"
                && let Some(from) = c.from.as_deref()
                && !paths.contains(from)
                && classify(from) == Class::Config
            {
                match self.before(from, c.ts_ms, c.kind == "dir") {
                    Ok(restore) => steps.push(Step {
                        path: from.to_owned(),
                        expect: State::Absent,
                        restore,
                    }),
                    Err(reason) => skip(from, reason),
                }
            }
        }
        let attic = format!("{ATTIC}{}-c{}", now_ms(), self.cause.id);
        (
            Plan {
                undo_of: self.cause.id,
                attic,
                steps,
            },
            skipped,
        )
    }
}

fn text_of(path: &Path) -> Option<String> {
    let m = std::fs::symlink_metadata(path).ok()?;
    if !m.is_file() || m.len() > MAX_TEXT || crate::blobs::is_secret_mode(m.uid(), m.mode()) {
        return None;
    }
    String::from_utf8(std::fs::read(path).ok()?).ok()
}

fn preview_step(root_path: &Path, step: &Step, current: Result<State>) -> PreviewStep {
    let current = current.map_err(|e| e.to_string());
    let verdict = if let Some(reason) = plan::refusal(step) {
        Verdict::Refused { reason }
    } else {
        match &current {
            Ok(c) if *c == step.restore.state() => Verdict::AlreadyUndone,
            Ok(c) if *c == step.expect && *c != State::Other => Verdict::Restore,
            _ => Verdict::Conflict,
        }
    };
    let secret = osreflog_model::secrets::is_secret_path(&step.path);
    let restore_text = match &step.restore {
        Version::File { content, meta }
            if !secret && !osreflog_model::secrets::is_secret_mode(meta.uid, meta.mode) =>
        {
            plan::from_hex(content).and_then(|b| String::from_utf8(b).ok())
        }
        _ => None,
    };
    PreviewStep {
        path: step.path.clone(),
        expect: step.expect.clone(),
        restore: step.restore.state(),
        current,
        verdict,
        current_text: if secret {
            None
        } else {
            text_of(&root_path.join(step.path.trim_start_matches('/')))
        },
        restore_text,
    }
}

/// The plan for undoing cause `id`, and its preview against the live system.
///
/// # Errors
/// If there's no such cause, or on ledger and filesystem errors.
pub fn preview(engine: &Engine, id: i64) -> Result<(Plan, Preview)> {
    let (cause, changes) = {
        let l = engine.ledger();
        let cause = l
            .cause(id)?
            .ok_or_else(|| invalid(format!("no cause #{id}")))?;
        (cause, l.cause_changes(id)?)
    };
    let root_path = live_root(engine);
    let root = Root::open(&root_path).map_err(|e| invalid(format!("opening the root: {e}")))?;
    let blobs = Blobs::new(&engine.blob_dir()).map_err(|e| invalid(format!("blobs: {e}")))?;
    if cause.kind == "dnf" {
        let tx = engine
            .ledger()
            .transaction_near(cause.started_ms / 1000 - 5, cause.ended_ms / 1000 + 60)?;
        let plan = Plan {
            undo_of: id,
            attic: String::new(),
            steps: Vec::new(),
        };
        let preview = Preview {
            digest: osreflog_undo::preview::digest(&plan),
            delegated: tx.map(|t| Delegation::Dnf {
                transaction: t.id,
                description: t.description,
            }),
            skipped: if changes.is_empty() {
                Vec::new()
            } else {
                vec![Skipped {
                    path: format!("{} paths", changes.len() as u64 + cause.package_paths),
                    reason:
                        "a package transaction: dnf undoes it, with the rpm database and scriptlets"
                            .into(),
                }]
            },
            steps: Vec::new(),
            cause,
        };
        return Ok((plan, preview));
    }
    let planner = Planner {
        engine,
        blobs,
        root,
        root_path: root_path.clone(),
        cause: cause.clone(),
    };
    let (plan, skipped) = planner.plan(&changes);
    let currents = osreflog_undo::current(&planner.root, &plan);
    let steps = plan
        .steps
        .iter()
        .zip(currents)
        .map(|(step, (_, current))| preview_step(&root_path, step, current.map_err(invalid)))
        .collect();
    let preview = Preview {
        digest: osreflog_undo::preview::digest(&plan),
        cause,
        steps,
        skipped,
        delegated: None,
    };
    Ok((plan, preview))
}

/// Applies the undo of cause `id`, if its plan is still the one with `digest`: takes and
/// verifies a `pre-undo` checkpoint, plans again (it must not have changed), then runs the
/// worker.
///
/// # Errors
/// If the plan changed, has nothing to restore, the checkpoint can't be verified, or the
/// worker fails.
pub fn apply(engine: &Engine, id: i64, digest: &str) -> Result<Applied> {
    let (_, shown) = preview(engine, id)?;
    if shown.digest != digest {
        return Err(invalid(format!(
            "the plan for #{id} changed since it was shown: run `osreflog undo {id}` again"
        )));
    }
    if shown.delegated.is_some() {
        return Err(invalid(format!(
            "#{id} is a package transaction: run the dnf command `osreflog undo {id}` shows"
        )));
    }
    if shown.to_restore() == 0 {
        return Err(invalid(format!("nothing in #{id} to restore")));
    }
    let pre = engine.checkpoint("pre-undo", Some(format!("before undoing #{id}")), None)?;
    let verified = engine.verify()?;
    let snapshot = engine.layout().snapshot_path(&pre.name);
    if !verified.ok || !snapshot.join("etc").is_dir() {
        return Err(invalid(format!(
            "the pre-undo checkpoint {} couldn't be verified ({}); nothing was changed",
            pre.name,
            verified
                .detail
                .unwrap_or_else(|| "snapshot unreadable".into())
        )));
    }
    let (plan, again) = preview(engine, id)?;
    if again.digest != digest {
        return Err(invalid(format!(
            "#{id} changed while the pre-undo checkpoint was taken: run `osreflog undo {id}` again"
        )));
    }
    let root_path = live_root(engine);
    let attic_base = root_path.join(ATTIC.trim_start_matches('/'));
    std::fs::create_dir_all(&attic_base)
        .and_then(|()| {
            std::fs::set_permissions(&attic_base, std::fs::Permissions::from_mode(0o700))
        })
        .map_err(|e| invalid(format!("creating the attic: {e}")))?;
    let report = run_worker(engine, &root_path, &plan)?;
    let reloaded_systemd = report.steps.iter().any(|s| {
        matches!(s.outcome, osreflog_undo::Outcome::Restored { .. })
            && s.path.starts_with("/etc/systemd/")
    });
    Ok(Applied {
        pre_undo: pre,
        report,
        reloaded_systemd,
    })
}

fn run_worker(engine: &Engine, root: &Path, plan: &Plan) -> Result<osreflog_undo::Report> {
    let exe = std::env::current_exe().map_err(|e| invalid(format!("current_exe: {e}")))?;
    let input = serde_json::to_vec(plan).map_err(|e| invalid(e.to_string()))?;
    let mut child = Command::new(exe)
        .arg(WORKER_FLAG)
        .arg(root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| invalid(format!("starting the undo worker: {e}")))?;
    let pid = child.id();
    engine.live.lock_brackets().undo.insert(pid, plan.undo_of);
    let result = (|| {
        if let Some(mut stdin) = child.stdin.take() {
            stdin
                .write_all(&input)
                .map_err(|e| invalid(format!("sending the plan: {e}")))?;
        }
        let deadline = Instant::now() + WORKER_DEADLINE;
        loop {
            if child
                .try_wait()
                .map_err(|e| invalid(e.to_string()))?
                .is_some()
            {
                break;
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                return Err(invalid("the undo worker didn't finish in time"));
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let mut out = Vec::new();
        let mut err = String::new();
        if let Some(mut s) = child.stdout.take() {
            let _ = s.read_to_end(&mut out);
        }
        if let Some(mut s) = child.stderr.take() {
            let _ = s.read_to_string(&mut err);
        }
        serde_json::from_slice(&out)
            .map_err(|_| invalid(format!("the undo worker failed: {}", err.trim())))
    })();
    // Keep the pid known until the live witness has stored the worker's changes, so they
    // are recorded as an `undo` cause.
    let _ = engine.live.wait_flushed(now_ms(), Duration::from_secs(10));
    engine.live.lock_brackets().undo.remove(&pid);
    result
}

/// The worker: open the root and the directories it may write, enter the sandbox, then read
/// a plan from stdin and write the report to stdout. Exit 2: the sandbox failed (nothing was
/// read); 4: the plan didn't parse.
#[must_use]
pub fn worker_main(root: &Path) -> i32 {
    let Ok(r) = Root::open(root) else {
        eprintln!("can't open {}", root.display());
        return 2;
    };
    let mut writable = Vec::new();
    for dir in plan::SCOPE
        .iter()
        .chain(std::iter::once(&ATTIC))
        .map(|d| d.trim_matches('/'))
    {
        if let Ok(fd) = r.dir(dir) {
            writable.push(fd);
        }
    }
    let borrowed: Vec<_> = writable.iter().map(std::os::fd::AsFd::as_fd).collect();
    if let Err(e) = crate::sandbox::enter_undo(&borrowed) {
        eprintln!("sandbox: {e}");
        return 2;
    }
    let mut input = Vec::new();
    if std::io::stdin().read_to_end(&mut input).is_err() {
        return 4;
    }
    let Ok(plan) = serde_json::from_slice::<Plan>(&input) else {
        eprintln!("the plan didn't parse");
        return 4;
    };
    let report = osreflog_undo::apply(&r, &plan);
    let mut stdout = std::io::stdout().lock();
    if serde_json::to_writer(&mut stdout, &report).is_err() || stdout.flush().is_err() {
        return 1;
    }
    0
}
