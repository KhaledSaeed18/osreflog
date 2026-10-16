//! osreflog: see what changed on this system, and when. A thin client of osreflogd.

mod bisect;
mod doctor;
mod format;
mod time;
mod user_undo;

use std::process::ExitCode;
use std::time::Duration;

use anyhow::{Context, bail};
use clap::{Parser, Subcommand};
use osreflog_model::{
    Cause, CauseDetail, Checkpoint, DiffReport, LogEntry, Status, VerifyReport, WhyEntry,
};

pub(crate) use osreflog_client::OsreflogProxy;

/// How long the dnf hook waits for the daemon before giving up (and letting dnf continue).
const HOOK_TIMEOUT: Duration = Duration::from_secs(120);

#[derive(Parser)]
#[command(
    version,
    about = "A reflog for your Linux system: see what changed, and when."
)]
struct Cli {
    #[command(subcommand)]
    command: Cmd,
    /// Talk to a daemon on the session bus (development only).
    #[arg(long, global = true, hide = true)]
    session_bus: bool,
}

#[derive(Subcommand)]
enum Cmd {
    /// Daemon and ledger status.
    Status {
        #[arg(long)]
        json: bool,
    },
    /// Checkpoints, newest first, with what changed since the one before.
    Log {
        /// How many checkpoints to show.
        #[arg(short = 'n', long, default_value_t = 20)]
        limit: u32,
        #[arg(long)]
        json: bool,
    },
    /// What changed between two checkpoints: `A..B`, `A..` (to now) or `A`.
    ///
    /// References: c42, @{0} (newest), @{1}, @{now}, @{boot}, @{boot-1}, or any date
    /// `date -d` understands, such as @{yesterday} or @{2 hours ago}.
    Diff {
        /// The range; default `@{0}..@{now}` (since the newest checkpoint).
        range: Option<String>,
        /// Include package-owned files and noise (caches, logs, package databases).
        #[arg(long)]
        all: bool,
        /// Only the summary counts.
        #[arg(long)]
        stat: bool,
        #[arg(long)]
        json: bool,
    },
    /// Take a checkpoint now (asks for admin authentication).
    Checkpoint {
        /// A note to remember it by.
        #[arg(short = 'm', long)]
        note: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Re-check the ledger's hash chain against its records.
    /// Causes the live witness recorded, newest first: who changed what.
    Causes {
        #[arg(short = 'n', long, default_value_t = 20)]
        limit: u32,
        #[arg(long)]
        json: bool,
    },
    /// One cause with every change it made (`osreflog show 12` or `show #12`).
    Show {
        id: String,
        #[arg(long)]
        json: bool,
    },
    /// Why a path changed: who changed it, when, and what changed inside it.
    Why {
        path: std::path::PathBuf,
        /// How many changes to show, newest first.
        #[arg(short = 'n', long, default_value_t = 5)]
        limit: u32,
        #[arg(long)]
        json: bool,
    },
    Verify {
        #[arg(long)]
        json: bool,
    },
    /// Undo a cause: put back what was there before it, path by path. Without --apply it
    /// only shows what it would do. A path that changed since is a conflict and is left
    /// alone; what undo replaces goes to /var/lib/osreflog/attic, never deleted.
    Undo {
        /// The cause, e.g. 42 or #42.
        id: String,
        /// Apply it (admin authentication; a pre-undo checkpoint is taken first).
        #[arg(long)]
        apply: bool,
        /// A cause from your own settings history (`osreflog user`), undone with dconf as you.
        #[arg(long)]
        user: bool,
        #[arg(long)]
        json: bool,
    },
    /// Your own settings changes (dconf/GSettings), newest first, with the app or command
    /// that made each. Recorded by the `osreflog-user` agent in your own ledger.
    User {
        /// Only keys under this path, e.g. /org/gnome/desktop/ or one key.
        key: Option<String>,
        #[arg(short = 'n', long, default_value_t = 20)]
        limit: u32,
        #[arg(long)]
        json: bool,
    },
    /// Find which cause broke something: binary search over checkpoints, then causes, running
    /// a test in throwaway copies of the system (containers or VMs).
    Bisect {
        #[command(subcommand)]
        action: bisect::BisectCmd,
    },
    /// Check the installation.
    Doctor {
        /// Instead: after metal bisect probes, put /boot back and remove clones nothing waits
        /// for (as root; this also runs at every boot).
        #[arg(long)]
        boot: bool,
        /// With --boot: only say what would be removed.
        #[arg(long, requires = "boot")]
        dry_run: bool,
    },
    /// Called by the dnf5 actions plugin; never fails the transaction.
    #[command(hide = true)]
    Hook { phase: String, pid: u32 },
}

fn split_range(range: Option<&str>) -> (String, String) {
    let (from, to) = match range {
        None => ("@{0}", "@{now}"),
        Some(r) => match r.split_once("..") {
            Some((a, b)) => (
                if a.is_empty() { "@{0}" } else { a },
                if b.is_empty() { "@{now}" } else { b },
            ),
            None => (r, "@{now}"),
        },
    };
    (time::expand_ref(from), time::expand_ref(to))
}

/// Turns a D-Bus method error into its message.
pub(crate) fn explain(e: zbus::Error) -> anyhow::Error {
    anyhow::Error::new(osreflog_client::explain(e))
}

async fn proxy(session: bool) -> anyhow::Result<OsreflogProxy<'static>> {
    let connection = if session {
        zbus::Connection::session().await
    } else {
        zbus::Connection::system().await
    }
    .context("connecting to D-Bus")?;
    Ok(OsreflogProxy::new(&connection).await?)
}

/// A message as one line, safe to follow `log.<LEVEL>=` in the actions plugin's protocol.
fn one_line(msg: &str) -> String {
    msg.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

fn warn_hook(msg: &str) {
    println!("log.WARNING={}", one_line(msg));
    eprintln!("{msg}");
}

fn print_json(raw: &str) -> anyhow::Result<()> {
    let value: serde_json::Value = serde_json::from_str(raw)?;
    println!("{}", serde_json::to_string_pretty(&value)?);
    Ok(())
}

/// `osreflog undo`: the preview, then (with `--apply`) the undo of exactly that plan. A
/// package transaction is handed to `dnf5 history undo`, which asks for confirmation itself.
async fn undo(p: &OsreflogProxy<'_>, id: i64, apply: bool, json: bool) -> anyhow::Result<ExitCode> {
    let raw = p.undo_plan(id).await.map_err(explain)?;
    let preview: osreflog_undo::Preview = serde_json::from_str(&raw)?;
    if !apply {
        if json {
            print_json(&raw)?;
        } else {
            format::undo_preview(&preview);
        }
        return Ok(ExitCode::SUCCESS);
    }
    if let Some(osreflog_undo::Delegation::Dnf { transaction, .. }) = &preview.delegated {
        format::undo_preview(&preview);
        if !rustix::process::geteuid().is_root() {
            bail!("run it as root: sudo dnf5 history undo {transaction}");
        }
        let status = std::process::Command::new("dnf5")
            .args(["history", "undo", &transaction.to_string()])
            .status()
            .context("running dnf5")?;
        return Ok(if status.success() {
            ExitCode::SUCCESS
        } else {
            ExitCode::FAILURE
        });
    }
    if !json {
        format::undo_preview(&preview);
        println!();
    }
    if preview.to_restore() == 0 {
        if !json {
            println!("nothing to apply.");
        }
        return Ok(ExitCode::SUCCESS);
    }
    let raw = p.undo(id, &preview.digest).await.map_err(explain)?;
    let applied: osreflog_undo::Applied = serde_json::from_str(&raw)?;
    if json {
        print_json(&raw)?;
    } else {
        format::undo_applied(&applied);
    }
    let clean = applied.report.steps.iter().all(|s| {
        matches!(
            s.outcome,
            osreflog_undo::Outcome::Restored { .. } | osreflog_undo::Outcome::AlreadyUndone
        )
    });
    Ok(if clean {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}

/// The dnf5 actions hook. Never blocks or fails a package transaction because of osreflog.
async fn hook(session: bool, phase: &str, pid: u32) {
    let result = tokio::time::timeout(HOOK_TIMEOUT, async {
        proxy(session)
            .await?
            .hook(phase, pid)
            .await
            .map_err(explain)
    })
    .await;
    // In the actions plugin's plain mode every stdout line is a command (`stop=...` would
    // abort the transaction), so stdout carries only `log.<LEVEL>=` lines.
    match result {
        Ok(Ok(raw)) => {
            if let Ok(c) = serde_json::from_str::<Checkpoint>(&raw) {
                let msg = format!("osreflog: checkpoint {} ({})", c.name, c.reason);
                println!("log.INFO={}", one_line(&msg));
            }
        }
        Ok(Err(e)) => warn_hook(&format!(
            "osreflog: no {phase}-transaction checkpoint: {e:#}"
        )),
        Err(_) => warn_hook(&format!(
            "osreflog: no {phase}-transaction checkpoint: osreflogd did not answer in time"
        )),
    }
}

/// Causes from the user's own ledger whose dconf keys fall under `key`, newest first.
fn user(key: Option<&str>, limit: u32, json: bool) -> anyhow::Result<ExitCode> {
    let path =
        osreflog_ledger::user_ledger_path().context("neither HOME nor XDG_STATE_HOME is set")?;
    if !path.exists() {
        println!(
            "no user ledger at {} yet; start the agent with\n  systemctl --user enable --now osreflog-user",
            path.display()
        );
        return Ok(ExitCode::SUCCESS);
    }
    let ledger = osreflog_ledger::Ledger::open(&path)
        .with_context(|| format!("opening {}", path.display()))?;
    let prefix = format!("dconf:{}", key.unwrap_or("/"));
    let mut found: Vec<CauseDetail> = Vec::new();
    for cause in ledger.causes(100_000, None)? {
        if found.len() >= limit as usize {
            break;
        }
        let summaries = ledger.summaries(cause.id)?;
        let changes: Vec<_> = ledger
            .cause_changes(cause.id)?
            .into_iter()
            .filter(|c| c.path.starts_with(&prefix))
            .map(|mut c| {
                c.summary = summaries.get(&c.path).cloned().unwrap_or_default();
                c
            })
            .collect();
        if !changes.is_empty() {
            found.push(CauseDetail { cause, changes });
        }
    }
    if json {
        println!("{}", serde_json::to_string_pretty(&found)?);
    } else if found.is_empty() {
        println!(
            "no settings changes recorded{}",
            key.map(|k| format!(" under {k}")).unwrap_or_default()
        );
    } else {
        for (i, d) in found.iter().enumerate() {
            if i > 0 {
                println!();
            }
            format::show(d);
        }
    }
    Ok(ExitCode::SUCCESS)
}

// One arm per subcommand: long, but flat.
#[allow(clippy::too_many_lines)]
async fn run(cli: Cli) -> anyhow::Result<ExitCode> {
    match cli.command {
        Cmd::Hook { phase, pid } => {
            hook(cli.session_bus, &phase, pid).await;
            Ok(ExitCode::SUCCESS)
        }
        Cmd::Doctor {
            boot: true,
            dry_run,
        } => doctor::boot(dry_run),
        Cmd::Doctor { .. } => doctor::run(cli.session_bus).await,
        // Inside a bisect probe osreflogd doesn't run: the verdict is a file there.
        Cmd::Bisect { action } if bisect::in_probe(&action) => bisect::verdict_here(&action),
        Cmd::User { key, limit, json } => user(key.as_deref(), limit, json),
        Cmd::Undo {
            id,
            apply,
            user: true,
            json,
        } => user_undo::run(
            id.trim_start_matches('#')
                .parse()
                .context("a cause id, like 12 or #12")?,
            apply,
            json,
        ),
        command => {
            let p = proxy(cli.session_bus).await?;
            match command {
                Cmd::Status { json } => {
                    let raw = p.status().await.map_err(explain)?;
                    if json {
                        print_json(&raw)?;
                    } else {
                        format::status(&serde_json::from_str::<Status>(&raw)?);
                    }
                }
                Cmd::Log { limit, json } => {
                    let raw = p.log(limit).await.map_err(explain)?;
                    if json {
                        print_json(&raw)?;
                    } else {
                        format::log(&serde_json::from_str::<Vec<LogEntry>>(&raw)?);
                    }
                }
                Cmd::Diff {
                    range,
                    all,
                    stat,
                    json,
                } => {
                    let (from, to) = split_range(range.as_deref());
                    let raw = p.diff(&from, &to, all).await.map_err(explain)?;
                    if json {
                        print_json(&raw)?;
                    } else {
                        format::diff(&serde_json::from_str::<DiffReport>(&raw)?, stat);
                    }
                }
                Cmd::Checkpoint { note, json } => {
                    let raw = p
                        .checkpoint(note.as_deref().unwrap_or(""))
                        .await
                        .map_err(explain)?;
                    if json {
                        print_json(&raw)?;
                    } else {
                        let c: Checkpoint = serde_json::from_str(&raw)?;
                        println!(
                            "checkpoint {} taken at {}",
                            c.name,
                            time::local(c.created_ms)
                        );
                    }
                }
                Cmd::Verify { json } => {
                    let raw = p.verify().await.map_err(explain)?;
                    let v: VerifyReport = serde_json::from_str(&raw)?;
                    if json {
                        print_json(&raw)?;
                    } else if v.ok {
                        println!("ledger ok: {} chain entries verified", v.entries);
                    } else {
                        println!(
                            "LEDGER MISMATCH at entry {}: {}",
                            v.first_bad.unwrap_or(0),
                            v.detail.as_deref().unwrap_or("?")
                        );
                    }
                    if !v.ok {
                        return Ok(ExitCode::FAILURE);
                    }
                }
                Cmd::Causes { limit, json } => {
                    let raw = p.causes(limit).await.map_err(explain)?;
                    if json {
                        print_json(&raw)?;
                    } else {
                        format::causes(&serde_json::from_str::<Vec<Cause>>(&raw)?);
                    }
                }
                Cmd::Show { id, json } => {
                    let id: i64 = id
                        .trim_start_matches('#')
                        .parse()
                        .context("a cause id, like 12 or #12")?;
                    let raw = p.show(id).await.map_err(explain)?;
                    if json {
                        print_json(&raw)?;
                    } else {
                        format::show(&serde_json::from_str::<CauseDetail>(&raw)?);
                    }
                }
                Cmd::Why { path, limit, json } => {
                    let abs = if path.is_absolute() {
                        path
                    } else {
                        std::env::current_dir()?.join(path)
                    };
                    let path = abs.to_string_lossy().into_owned();
                    let raw = p.why(&path, limit).await.map_err(explain)?;
                    if json {
                        print_json(&raw)?;
                    } else {
                        format::why(&path, &serde_json::from_str::<Vec<WhyEntry>>(&raw)?);
                    }
                }
                Cmd::Undo {
                    id, apply, json, ..
                } => {
                    let id: i64 = id
                        .trim_start_matches('#')
                        .parse()
                        .context("a cause id, like 12 or #12")?;
                    return undo(&p, id, apply, json).await;
                }
                Cmd::Bisect { action } => return bisect::run(&p, action).await,
                Cmd::Hook { .. } | Cmd::Doctor { .. } | Cmd::User { .. } => bail!("unreachable"),
            }
            Ok(ExitCode::SUCCESS)
        }
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(r) => r,
        Err(e) => {
            eprintln!("osreflog: {e}");
            return ExitCode::FAILURE;
        }
    };
    match runtime.block_on(run(cli)) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("osreflog: {e:#}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::split_range;

    #[test]
    fn hook_messages_stay_on_one_line() {
        assert_eq!(super::one_line("a\nstop=x\rb"), "a stop=x b");
    }

    #[test]
    fn ranges() {
        let s = |r: Option<&str>| split_range(r);
        assert_eq!(s(None), ("@{0}".into(), "@{now}".into()));
        assert_eq!(s(Some("c3..c5")), ("c3".into(), "c5".into()));
        assert_eq!(s(Some("@{1}..")), ("@{1}".into(), "@{now}".into()));
        assert_eq!(s(Some("..c5")), ("@{0}".into(), "c5".into()));
        assert_eq!(s(Some("c3")), ("c3".into(), "@{now}".into()));
    }
}
