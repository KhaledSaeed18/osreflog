//! `osreflog bisect`: start a session in the daemon, follow its log, show what it found.

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use anyhow::{Context, bail};
use clap::{Subcommand, ValueEnum};
use osreflog_bisect::{Args, Finding, Status, Tier};

use crate::OsreflogProxy;
use crate::time::local;

#[derive(Clone, Copy, ValueEnum)]
pub enum TierArg {
    /// A container: binaries, libraries, config, units (seconds per probe).
    Nspawn,
    /// A VM booting the probe's own kernel: also kernel, modules, boot (tens of seconds).
    Vm,
}

#[derive(Subcommand)]
pub enum BisectCmd {
    /// Find the first cause after which a test fails, between two checkpoints. The test runs
    /// as root inside throwaway copies of the system: exit 0 good, 125 skip, 1–127 bad.
    ///
    /// osreflog bisect run --good c40 --bad c47 -- grep -q 'mode = old' /etc/app.conf
    Run {
        /// A checkpoint where the test passes (c40, @{3}, @{boot-1}, …).
        #[arg(long)]
        good: String,
        /// A checkpoint where it fails; default the newest.
        #[arg(long, default_value = "@{0}")]
        bad: String,
        #[arg(long, value_enum, default_value = "nspawn")]
        tier: TierArg,
        /// nspawn: boot systemd in the container and run the test as a unit.
        #[arg(long)]
        boot: bool,
        /// Seconds per probe.
        #[arg(long)]
        timeout: Option<u64>,
        /// A script file to run as the test, instead of a command.
        #[arg(long, conflicts_with = "command")]
        script: Option<PathBuf>,
        /// The test command (after --).
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        command: Vec<String>,
        #[arg(long)]
        json: bool,
    },
    /// Start a bisect where you give the verdicts: each probe boots as a container on a copy
    /// of the system at that point; look into it, then say good, bad or skip. The session
    /// survives restarts of osreflogd and reboots.
    ///
    /// osreflog bisect start --good c40 --bad c47
    Start {
        /// A checkpoint where things worked (c40, @{3}, @{boot-1}, …).
        #[arg(long)]
        good: String,
        /// A checkpoint where they don't; default the newest.
        #[arg(long, default_value = "@{0}")]
        bad: String,
    },
    /// The probe that waits is good (run it on the host, or as root inside the probe).
    Good,
    /// The probe that waits is bad.
    Bad,
    /// The probe that waits can't be judged.
    Skip,
    /// Every probe so far and its verdict.
    Log,
    /// The current or last bisect.
    Status {
        #[arg(long)]
        json: bool,
    },
    /// Forget the last bisect.
    Reset,
}

fn quote(word: &str) -> String {
    if !word.is_empty()
        && word
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_./=:,@%+".contains(&b))
    {
        word.to_owned()
    } else {
        format!("'{}'", word.replace('\'', r"'\''"))
    }
}

/// The test as a script: a file's content, one shell snippet, or a quoted command.
fn test_script(script: Option<&PathBuf>, command: &[String]) -> anyhow::Result<String> {
    if let Some(path) = script {
        return std::fs::read_to_string(path)
            .with_context(|| format!("reading {}", path.display()));
    }
    let body = match command {
        [] => bail!("give a test command after --, or --script FILE"),
        [snippet] if snippet.contains(' ') => snippet.clone(),
        words => words.iter().map(|w| quote(w)).collect::<Vec<_>>().join(" "),
    };
    Ok(format!("#!/bin/sh\n{body}\n"))
}

fn finding(status: &Status) {
    let probes = &status.probes;
    let count = |p: &str| probes.iter().filter(|r| r.what.starts_with(p)).count();
    match &status.finding {
        Some(Finding::Cause {
            culprit,
            between,
            approximate,
        }) => {
            println!(
                "first bad cause: #{}  {}  {}  {}",
                culprit.id,
                local(culprit.started_ms),
                culprit.kind,
                culprit.label
            );
            for p in culprit.paths.iter().take(10) {
                println!("  {p}");
            }
            if culprit.paths.len() > 10 {
                println!("  … and more: osreflog show {}", culprit.id);
            }
            println!("between {} (good) and {} (bad)", between.0, between.1);
            println!(
                "probes: {} ({} on checkpoints, {} on causes) · tier {:?} · approximations: {}",
                probes.len(),
                count("checkpoint"),
                count("after"),
                status.args.tier,
                if approximate.is_empty() {
                    "none".to_owned()
                } else {
                    approximate.join(", ")
                }
            );
            println!("next: osreflog show {0} · osreflog undo {0}", culprit.id);
        }
        Some(Finding::Causes { candidates }) => {
            println!("one of these causes (the probes between them couldn't be tested):");
            for c in candidates {
                println!(
                    "  #{}  {}  {}  {}",
                    c.id,
                    local(c.started_ms),
                    c.kind,
                    c.label
                );
            }
        }
        Some(Finding::Checkpoint {
            last_good,
            first_bad,
            why,
        }) => {
            println!("first bad checkpoint: {first_bad} (last good {last_good})");
            println!("  {why}");
        }
        Some(Finding::NotReproduced { detail }) => println!("not reproduced: {detail}"),
        None => {}
    }
    if let Some(e) = &status.error {
        println!("bisect failed: {e}");
    }
}

/// Inside a manual probe (a container or, from P7b, a metal boot), `/var/lib/osreflog-probe/
/// manual` says which one; osreflogd doesn't run there.
const PROBE_MARKER: &str = "/var/lib/osreflog-probe/manual";
const PROBE_VERDICT: &str = "/var/lib/osreflog-probe/verdict";

fn verdict_word(cmd: &BisectCmd) -> Option<&'static str> {
    match cmd {
        BisectCmd::Good => Some("good"),
        BisectCmd::Bad => Some("bad"),
        BisectCmd::Skip => Some("skip"),
        _ => None,
    }
}

/// Whether this is a verdict given inside a probe.
#[must_use]
pub fn in_probe(cmd: &BisectCmd) -> bool {
    verdict_word(cmd).is_some() && std::path::Path::new(PROBE_MARKER).is_file()
}

/// A verdict inside a probe: written where the daemon reads it once it sees the probe again
/// (at once for a container, after the reboot for a metal probe).
///
/// # Errors
/// Without root, or if the file can't be written.
pub fn verdict_here(cmd: &BisectCmd) -> anyhow::Result<ExitCode> {
    let Some(word) = verdict_word(cmd) else {
        bail!("not a verdict")
    };
    if !rustix::process::geteuid().is_root() {
        bail!("this is a bisect probe: give the verdict as root (sudo osreflog bisect {word})");
    }
    let n = std::fs::read_to_string(PROBE_MARKER).unwrap_or_default();
    let tmp = format!("{PROBE_VERDICT}.tmp");
    std::fs::write(&tmp, format!("{word}\n")).context("writing the verdict")?;
    std::fs::rename(&tmp, PROBE_VERDICT).context("writing the verdict")?;
    rustix::fs::sync();
    println!("probe {}: {word}, recorded.", n.trim());
    if std::path::Path::new("/run/systemd/container").exists() {
        println!("osreflog stops this container and goes on with the next probe in a moment.");
    } else {
        println!("Reboot to go back to your normal system; osreflog goes on from there.");
    }
    Ok(ExitCode::SUCCESS)
}

fn print_waiting(status: &Status) {
    if let Some(w) = &status.waiting {
        println!();
        println!("probe {} waits for your verdict: {}", w.probe, w.what);
        for h in &w.how {
            println!("  {h}");
        }
    }
}

/// Follows the session's log until it ends or a probe other than `after` waits for a verdict.
async fn follow(p: &OsreflogProxy<'_>, after: Option<u32>) -> anyhow::Result<ExitCode> {
    let mut shown: Option<usize> = None;
    loop {
        let raw = p.bisect_status().await.map_err(crate::explain)?;
        let Some(status) = serde_json::from_str::<Option<Status>>(&raw)? else {
            bail!("the bisect disappeared")
        };
        let from = shown.unwrap_or(if after.is_some() { status.log.len() } else { 0 });
        for line in &status.log[from.min(status.log.len())..] {
            println!("{line}");
        }
        shown = Some(status.log.len());
        if !status.running {
            println!();
            finding(&status);
            let found = matches!(status.finding, Some(Finding::Cause { .. }));
            return Ok(if found {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            });
        }
        if let Some(w) = &status.waiting
            && Some(w.probe) != after
        {
            print_waiting(&status);
            return Ok(ExitCode::SUCCESS);
        }
        tokio::time::sleep(Duration::from_millis(700)).await;
    }
}

fn log(status: &Status) {
    println!(
        "bisect {} … {} (tier {:?}{}), started {}",
        status.args.good,
        status.args.bad,
        status.args.tier,
        if status.args.manual { ", by hand" } else { "" },
        local(status.started_ms)
    );
    for (i, r) in status.probes.iter().enumerate() {
        println!(
            "  {:>2}  {:<5} {}",
            i + 1,
            format!("{:?}", r.verdict).to_lowercase(),
            r.what
        );
    }
    if let Some(w) = &status.waiting {
        println!(
            "  {:>2}  …     {} (waiting)",
            status.probes.len() + 1,
            w.what
        );
    }
}

/// # Errors
/// On D-Bus or argument errors.
// One arm per subcommand: long, but flat.
#[allow(clippy::too_many_lines)]
pub async fn run(p: &OsreflogProxy<'_>, cmd: BisectCmd) -> anyhow::Result<ExitCode> {
    match cmd {
        BisectCmd::Run {
            good,
            bad,
            tier,
            boot,
            timeout,
            script,
            command,
            json,
        } => {
            let args = Args {
                good: crate::time::expand_ref(&good),
                bad: crate::time::expand_ref(&bad),
                tier: match tier {
                    TierArg::Nspawn => Tier::Nspawn,
                    TierArg::Vm => Tier::Vm,
                },
                boot,
                test: test_script(script.as_ref(), &command)?,
                timeout_s: timeout,
                manual: false,
            };
            p.bisect_run(&serde_json::to_string(&args)?)
                .await
                .map_err(crate::explain)?;
            let mut shown = 0;
            loop {
                let raw = p.bisect_status().await.map_err(crate::explain)?;
                let status: Option<Status> = serde_json::from_str(&raw)?;
                let Some(status) = status else {
                    bail!("the bisect disappeared")
                };
                if !json {
                    for line in &status.log[shown.min(status.log.len())..] {
                        println!("{line}");
                    }
                }
                shown = status.log.len();
                if !status.running {
                    if json {
                        println!("{}", serde_json::to_string_pretty(&status)?);
                    } else {
                        println!();
                        finding(&status);
                    }
                    let found = matches!(status.finding, Some(Finding::Cause { .. }));
                    return Ok(if found {
                        ExitCode::SUCCESS
                    } else {
                        ExitCode::FAILURE
                    });
                }
                tokio::time::sleep(Duration::from_millis(700)).await;
            }
        }
        BisectCmd::Status { json } => {
            let raw = p.bisect_status().await.map_err(crate::explain)?;
            let status: Option<Status> = serde_json::from_str(&raw)?;
            match (json, status) {
                (true, _) => println!("{raw}"),
                (false, None) => println!("no bisect"),
                (false, Some(s)) => {
                    println!(
                        "bisect {} … {} (tier {:?}), {}",
                        s.args.good,
                        s.args.bad,
                        s.args.tier,
                        if s.running { "running" } else { "finished" }
                    );
                    for line in &s.log {
                        println!("{line}");
                    }
                    if s.running {
                        print_waiting(&s);
                    } else {
                        println!();
                        finding(&s);
                    }
                }
            }
            Ok(ExitCode::SUCCESS)
        }
        BisectCmd::Start { good, bad } => {
            let args = Args {
                good: crate::time::expand_ref(&good),
                bad: crate::time::expand_ref(&bad),
                tier: Tier::Nspawn,
                boot: true,
                test: String::new(),
                timeout_s: None,
                manual: true,
            };
            p.bisect_run(&serde_json::to_string(&args)?)
                .await
                .map_err(crate::explain)?;
            follow(p, None).await
        }
        BisectCmd::Good | BisectCmd::Bad | BisectCmd::Skip => {
            let word = verdict_word(&cmd).unwrap_or("skip");
            let raw = p.bisect_verdict(word).await.map_err(crate::explain)?;
            let before: Option<Status> = serde_json::from_str(&raw)?;
            let after = before.and_then(|s| s.waiting.map(|w| w.probe));
            follow(p, after).await
        }
        BisectCmd::Log => {
            let raw = p.bisect_status().await.map_err(crate::explain)?;
            match serde_json::from_str::<Option<Status>>(&raw)? {
                None => println!("no bisect"),
                Some(s) => {
                    log(&s);
                    if !s.running {
                        println!();
                        finding(&s);
                    }
                }
            }
            Ok(ExitCode::SUCCESS)
        }
        BisectCmd::Reset => {
            p.bisect_reset().await.map_err(crate::explain)?;
            println!("bisect forgotten");
            Ok(ExitCode::SUCCESS)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tests_become_scripts() {
        let cmd = |w: &[&str]| {
            test_script(None, &w.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>()).unwrap()
        };
        assert_eq!(
            cmd(&["grep", "-q", "mode = old", "/etc/a.conf"]),
            "#!/bin/sh\ngrep -q 'mode = old' /etc/a.conf\n"
        );
        assert_eq!(
            cmd(&["test -f /etc/x && ! grep -q y /etc/x"]),
            "#!/bin/sh\ntest -f /etc/x && ! grep -q y /etc/x\n"
        );
        assert_eq!(cmd(&["echo", "it's"]), "#!/bin/sh\necho 'it'\\''s'\n");
        assert!(test_script(None, &[]).is_err());
    }
}
