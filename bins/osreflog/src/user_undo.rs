//! `osreflog undo --user`: undoing a settings change from the user's own ledger. It runs as
//! the user, with `dconf`, and follows the same 3-way rule as the system undo: a key is put
//! back only if it still has the value the cause left.

use std::process::{Command, ExitCode};

use anyhow::{Context, bail};
use serde::Serialize;

#[derive(Debug, Serialize)]
struct Step {
    key: String,
    /// The value before the cause (`None`: unset), which undo puts back.
    restore: Option<String>,
    /// The value the cause left.
    expect: Option<String>,
    current: Option<String>,
    verdict: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
}

fn dconf_read(key: &str) -> anyhow::Result<Option<String>> {
    let out = Command::new("dconf")
        .args(["read", key])
        .output()
        .context("running dconf")?;
    if !out.status.success() {
        bail!(
            "dconf read {key}: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    let v = String::from_utf8_lossy(&out.stdout)
        .trim_end_matches('\n')
        .to_owned();
    Ok((!v.is_empty()).then_some(v))
}

fn shown(v: Option<&str>) -> String {
    match v {
        None => "unset".to_owned(),
        Some(v) if v.chars().count() > 80 => format!("{}…", v.chars().take(79).collect::<String>()),
        Some(v) => v.to_owned(),
    }
}

/// One step per key the cause changed, judged against the current value.
fn plan(ledger: &osreflog_ledger::Ledger, id: i64) -> anyhow::Result<Vec<Step>> {
    let values = ledger.dconf_values(id)?;
    let mut steps = Vec::new();
    for change in ledger.cause_changes(id)? {
        let Some(key) = change.path.strip_prefix("dconf:") else {
            continue;
        };
        let Some((_, old, new)) = values.iter().find(|(k, _, _)| k == key) else {
            steps.push(Step {
                key: key.to_owned(),
                restore: None,
                expect: None,
                current: None,
                verdict: "refused",
                reason: Some("its value wasn't kept (a secret key, or recorded before P5)".into()),
            });
            continue;
        };
        let current = dconf_read(key)?;
        let verdict = if current == *old {
            "already_undone"
        } else if current != *new {
            "conflict"
        } else {
            "restore"
        };
        steps.push(Step {
            key: key.to_owned(),
            restore: old.clone(),
            expect: new.clone(),
            current,
            verdict,
            reason: None,
        });
    }
    Ok(steps)
}

fn print(cause: &osreflog_model::Cause, steps: &[Step]) {
    println!("undo #{} {}  {}", cause.id, cause.kind, cause.label);
    for s in steps {
        let line = match s.verdict {
            "restore" => format!(
                "~ {}  {} → {}",
                s.key,
                shown(s.current.as_deref()),
                shown(s.restore.as_deref())
            ),
            "already_undone" => format!("= {}  already as it was before", s.key),
            "conflict" => format!(
                "! {}  conflict: changed since (now {}, the cause left {})",
                s.key,
                shown(s.current.as_deref()),
                shown(s.expect.as_deref())
            ),
            _ => format!(
                "x {}  refused: {}",
                s.key,
                s.reason.as_deref().unwrap_or("")
            ),
        };
        println!("  {line}");
    }
}

/// # Errors
/// If the user ledger or the cause doesn't exist, or dconf fails.
pub fn run(id: i64, apply: bool, json: bool) -> anyhow::Result<ExitCode> {
    let path =
        osreflog_ledger::user_ledger_path().context("neither HOME nor XDG_STATE_HOME is set")?;
    if !path.exists() {
        bail!(
            "no user ledger at {}: is osreflog-user running?",
            path.display()
        );
    }
    let ledger = osreflog_ledger::Ledger::open(&path)?;
    let Some(cause) = ledger.cause(id)? else {
        bail!("no cause #{id} in your settings history (osreflog user lists them)");
    };
    let steps = plan(&ledger, id)?;
    if !json {
        print(&cause, &steps);
    }
    let todo: Vec<&Step> = steps.iter().filter(|s| s.verdict == "restore").collect();
    if !apply {
        if json {
            println!("{}", serde_json::to_string_pretty(&steps)?);
        } else if !todo.is_empty() {
            println!("dry run: nothing changed. To apply: osreflog undo --user {id} --apply");
        }
        return Ok(ExitCode::SUCCESS);
    }
    let mut failed = 0;
    for s in &todo {
        // Check again right before writing: dconf has no compare-and-swap.
        if dconf_read(&s.key)? != s.expect {
            println!("  ! {}  changed meanwhile: left alone", s.key);
            failed += 1;
            continue;
        }
        let status = match &s.restore {
            Some(v) => Command::new("dconf").args(["write", &s.key, v]).status(),
            None => Command::new("dconf").args(["reset", &s.key]).status(),
        }
        .context("running dconf")?;
        if status.success() {
            println!("  restored {}", s.key);
        } else {
            println!("  failed {}", s.key);
            failed += 1;
        }
    }
    if json {
        println!("{}", serde_json::to_string_pretty(&steps)?);
    }
    let clean = failed == 0
        && steps
            .iter()
            .all(|s| matches!(s.verdict, "restore" | "already_undone"));
    Ok(if clean {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}
