//! `osreflog doctor`: is everything in place?

use std::path::Path;
use std::process::ExitCode;

use osreflog_model::{Status, VerifyReport};

const ACTIONS_FILE: &str = "/etc/dnf/libdnf5-plugins/actions.d/osreflog.actions";
const ACTIONS_PLUGIN: &str = "/usr/lib64/libdnf5/plugins/actions.so";

struct Report {
    failed: bool,
    warnings: u32,
}

impl Report {
    #[allow(clippy::unused_self)]
    fn ok(&self, what: &str) {
        println!("  ok    {what}");
    }

    fn warn(&mut self, what: &str) {
        println!("  warn  {what}");
        self.warnings += 1;
    }

    fn fail(&mut self, what: &str) {
        println!("  FAIL  {what}");
        self.failed = true;
    }
}

fn root_is_btrfs() -> Option<(String, String)> {
    let text = std::fs::read_to_string("/proc/self/mountinfo").ok()?;
    text.lines().rev().find_map(|line| {
        let (left, right) = line.split_once(" - ")?;
        let l: Vec<&str> = left.split(' ').collect();
        let r: Vec<&str> = right.split(' ').collect();
        if l.get(4) != Some(&"/") {
            return None;
        }
        Some(((*r.first()?).to_owned(), (*l.get(3)?).to_owned()))
    })
}

/// Bytes free for unprivileged writes on the filesystem holding `path`.
fn free_bytes(path: &str) -> Option<u64> {
    let st = rustix::fs::statvfs(path).ok()?;
    Some(st.f_bavail.saturating_mul(st.f_frsize))
}

/// The kept snapshots against the free space: a warning when they surely hold more than is
/// left, so pruning (or unpinning) would give back more than / has.
fn space(r: &mut Report, sp: &osreflog_model::Space, free: Option<u64>) {
    use osreflog_model::bytes_text;
    let since = if sp.complete {
        String::new()
    } else {
        format!(", counting changes since {}", sp.since)
    };
    match free {
        Some(free) if sp.at_least > free => r.warn(&format!(
            "the kept snapshots hold at least {} that / no longer has, more than the {} free{since}; see docs/troubleshooting.md",
            bytes_text(sp.at_least),
            bytes_text(free)
        )),
        Some(free) => r.ok(&format!(
            "the kept snapshots hold {} that / no longer has ({} free){since}",
            sp.range_text(),
            bytes_text(free)
        )),
        None => r.ok(&format!(
            "the kept snapshots hold {} that / no longer has{since}",
            sp.range_text()
        )),
    }
}

pub async fn run(session: bool) -> anyhow::Result<ExitCode> {
    let mut r = Report {
        failed: false,
        warnings: 0,
    };
    println!("osreflog doctor");

    match root_is_btrfs() {
        Some((fs, sub)) if fs == "btrfs" && sub != "/" => {
            r.ok(&format!("/ is btrfs subvolume {sub}"));
        }
        Some((fs, _)) if fs == "btrfs" => {
            r.fail("/ is the btrfs top level; osreflog needs / on a subvolume");
        }
        Some((fs, _)) => r.fail(&format!("/ is {fs}, not btrfs")),
        None => r.fail("could not read /proc/self/mountinfo"),
    }

    if Path::new(ACTIONS_PLUGIN).exists() {
        r.ok("dnf5 actions plugin installed");
    } else {
        r.fail("dnf5 actions plugin missing: sudo dnf install libdnf5-plugin-actions");
    }
    if Path::new(ACTIONS_FILE).exists() {
        r.ok(&format!("checkpoint hook installed ({ACTIONS_FILE})"));
    } else {
        r.fail(&format!("checkpoint hook missing: {ACTIONS_FILE}"));
    }
    for (path, tool) in [
        ("/usr/bin/snapper", "Snapper"),
        ("/usr/bin/timeshift", "Timeshift"),
    ] {
        if Path::new(path).exists() {
            r.warn(&format!(
                "{tool} is installed: two tools managing snapshots of / is unsupported"
            ));
        }
    }

    match crate::proxy(session).await {
        Err(e) => r.fail(&format!("D-Bus: {e:#}")),
        Ok(p) => {
            match p.status().await.map_err(crate::explain) {
                Ok(raw) => match serde_json::from_str::<Status>(&raw) {
                    Ok(s) => {
                        r.ok(&format!(
                            "osreflogd {} running, {} checkpoints, {} snapshots kept",
                            s.version, s.checkpoints, s.snapshots_kept
                        ));
                        if s.pending_diffs > 0 {
                            r.warn(&format!(
                                "{} diffs pending (computed in the background)",
                                s.pending_diffs
                            ));
                        }
                        if let Some(sp) = &s.space {
                            space(&mut r, sp, free_bytes("/"));
                        }
                    }
                    Err(e) => r.fail(&format!("unexpected status reply: {e}")),
                },
                Err(e) => r.fail(&format!("osreflogd: {e:#}")),
            }
            match p.verify().await.map_err(crate::explain) {
                Ok(raw) => match serde_json::from_str::<VerifyReport>(&raw) {
                    Ok(v) if v.ok => {
                        r.ok(&format!("ledger chain verified ({} entries)", v.entries));
                    }
                    Ok(v) => r.fail(&format!(
                        "ledger chain mismatch: {}",
                        v.detail.unwrap_or_default()
                    )),
                    Err(e) => r.fail(&format!("unexpected verify reply: {e}")),
                },
                Err(e) => r.fail(&format!("verify: {e:#}")),
            }
        }
    }
    if r.warnings > 0 && !r.failed {
        println!("{} warning(s)", r.warnings);
    }
    Ok(if r.failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    })
}

/// `osreflog doctor --boot`: runs osreflogd's boot doctor in a private mount namespace (the
/// btrfs top level is mounted there only) and passes its report and exit code through.
/// Not through `systemd-run`: as root it talks to systemd's private socket, and from a
/// service that connection is reset (P6, and again in P7 G4), while `unshare` works anywhere.
/// The boot-time unit keeps the full sandbox.
pub fn boot(dry_run: bool) -> anyhow::Result<ExitCode> {
    if !rustix::process::geteuid().is_root() {
        anyhow::bail!("doctor --boot changes /boot and deletes probe subvolumes: run it with sudo");
    }
    let daemon = std::env::current_exe()
        .ok()
        .and_then(|exe| Some(exe.parent()?.parent()?.join("libexec/osreflog/osreflogd")))
        .filter(|p| p.is_file())
        .unwrap_or_else(|| Path::new("/usr/libexec/osreflog/osreflogd").to_owned());
    let mut cmd = std::process::Command::new("unshare");
    cmd.args(["--mount", "--propagation", "private"])
        .arg(&daemon)
        .arg("--boot-doctor");
    if dry_run {
        cmd.arg("--dry-run");
    }
    let status = cmd.status()?;
    Ok(match status.code() {
        Some(0) => ExitCode::SUCCESS,
        Some(c) => ExitCode::from(u8::try_from(c).unwrap_or(1)),
        None => ExitCode::FAILURE,
    })
}
