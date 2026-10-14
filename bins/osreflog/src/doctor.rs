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
