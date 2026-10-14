//! osreflog: see what changed on this system, and when. A thin client of osreflogd.

mod doctor;
mod format;
mod time;

use std::process::ExitCode;
use std::time::Duration;

use anyhow::{Context, bail};
use clap::{Parser, Subcommand};
use osreflog_model::{Checkpoint, DiffReport, LogEntry, Status, VerifyReport};

#[zbus::proxy(
    interface = "io.github.khaledsaeed18.osreflog1",
    default_service = "io.github.khaledsaeed18.osreflog1",
    default_path = "/io/github/khaledsaeed18/osreflog1"
)]
trait Osreflog {
    fn status(&self) -> zbus::Result<String>;
    fn log(&self, limit: u32) -> zbus::Result<String>;
    fn diff(&self, from: &str, to: &str, all: bool) -> zbus::Result<String>;
    fn verify(&self) -> zbus::Result<String>;
    fn checkpoint(&self, note: &str) -> zbus::Result<String>;
    fn hook(&self, phase: &str, pid: u32) -> zbus::Result<String>;
}

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
    Verify {
        #[arg(long)]
        json: bool,
    },
    /// Check the installation.
    Doctor,
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
fn explain(e: zbus::Error) -> anyhow::Error {
    match e {
        zbus::Error::MethodError(name, Some(msg), _)
            if name.as_str().starts_with(osreflog_model::bus::ERROR_PREFIX) =>
        {
            anyhow::anyhow!("{msg}")
        }
        zbus::Error::MethodError(name, _, _)
            if name.as_str() == "org.freedesktop.DBus.Error.ServiceUnknown" =>
        {
            anyhow::anyhow!("osreflogd is not running (systemctl status osreflogd)")
        }
        other => anyhow::Error::new(other),
    }
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

async fn run(cli: Cli) -> anyhow::Result<ExitCode> {
    match cli.command {
        Cmd::Hook { phase, pid } => {
            hook(cli.session_bus, &phase, pid).await;
            Ok(ExitCode::SUCCESS)
        }
        Cmd::Doctor => doctor::run(cli.session_bus).await,
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
                Cmd::Hook { .. } | Cmd::Doctor => bail!("unreachable"),
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
