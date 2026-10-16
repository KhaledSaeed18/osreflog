//! Human-readable output.

use std::fmt::Write as _;

use osreflog_model::{
    Actor, Cause, CauseDetail, Change, ClassCounts, DiffReport, Endpoint, LiveChange, LogEntry,
    Status, Transaction, WhyEntry,
};

use crate::time::local;

fn counts_line(c: &ClassCounts) -> String {
    let mut parts = Vec::new();
    for (n, label) in [
        (c.config, "config"),
        (c.state, "state"),
        (c.other, "other"),
        (c.package, "package"),
        (c.noise, "noise"),
    ] {
        if n > 0 {
            parts.push(format!("{n} {label}"));
        }
    }
    if parts.is_empty() {
        "no changes".to_owned()
    } else {
        parts.join(" · ")
    }
}

fn transaction_line(t: &Transaction) -> String {
    let what = if t.description.is_empty() {
        "(no command line: PackageKit or GNOME Software)"
    } else {
        &t.description
    };
    format!(
        "dnf #{} {what} (uid {}, {})",
        t.id,
        t.user_id,
        t.state.to_lowercase()
    )
}

pub fn status(s: &Status) {
    println!("osreflogd {}", s.version);
    println!(
        "tracking   {} (subvolume {} on {})",
        s.root, s.subvolume, s.device
    );
    println!(
        "checkpoints {} ({} snapshots kept)",
        s.checkpoints, s.snapshots_kept
    );
    if let Some(c) = &s.latest {
        println!(
            "latest     {} {} ({})",
            c.name,
            local(c.created_ms),
            c.reason
        );
    }
    println!("ledger     {} chain entries", s.chain_entries);
    if s.pending_diffs > 0 {
        println!("pending    {} diffs being computed", s.pending_diffs);
    }
    if let Some(l) = &s.live {
        println!(
            "live       watching: {} events, {} changes recorded, {} out of scope, {} open causes, {} processes known",
            l.events, l.recorded, l.out_of_scope, l.open_causes, l.processes
        );
        if l.delegated > 0 {
            println!(
                "           {} causes credited to the D-Bus caller instead of the daemon",
                l.delegated
            );
        }
        if l.dropped > 0 {
            println!(
                "           {} changes not attributed (too many at once; snapshots still have them)",
                l.dropped
            );
        }
        if l.overflows > 0 || l.ring_drops > 0 || l.unresolved > 0 {
            println!(
                "           {} queue overflows, {} lineage drops, {} unresolved",
                l.overflows, l.ring_drops, l.unresolved
            );
        }
    } else {
        println!("live       not running (state witness only)");
    }
}

pub fn log(entries: &[LogEntry]) {
    if entries.is_empty() {
        println!("no checkpoints yet");
        return;
    }
    for e in entries {
        let c = &e.checkpoint;
        let mut line = format!("{}  {}  {:<9}", c.name, local(c.created_ms), c.reason);
        if let Some(comm) = &c.pm_comm {
            let _ = write!(line, " via {comm}");
        }
        if let Some(note) = &c.note {
            let _ = write!(line, "  \"{note}\"");
        }
        if c.pruned_ms.is_some() {
            line.push_str("  [pruned]");
        }
        println!("{line}");
        match &e.interval {
            Some(iv) => {
                let mut summary = counts_line(&iv.counts);
                if iv.packages > 0 {
                    summary = format!(
                        "{} package change{} · {summary}",
                        iv.packages,
                        if iv.packages == 1 { "" } else { "s" }
                    );
                }
                println!("           since c{:06}: {summary}", iv.from_id);
                for t in &iv.transactions {
                    println!("           {}", transaction_line(t));
                }
            }
            None if e.checkpoint.id > 1 => println!("           (diff not computed yet)"),
            None => {}
        }
    }
}

fn endpoint(e: &Endpoint) -> String {
    match e {
        Endpoint::Checkpoint(c) => format!("{} ({}, {})", c.name, c.reason, local(c.created_ms)),
        Endpoint::Live { taken_ms } => format!("now ({})", local(*taken_ms)),
    }
}

fn change_line(c: &Change) -> String {
    let symbol = match c.op.as_str() {
        "create" => "+",
        "delete" => "-",
        "rename" => ">",
        _ => "~",
    };
    let path = match &c.from {
        Some(from) => format!("{from} -> {}", c.path),
        None => c.path.clone(),
    };
    let detail = if c.what.is_empty() || c.op == "create" || c.op == "delete" {
        c.kind.clone()
    } else {
        format!("{} {}", c.kind, c.what.join(","))
    };
    let by = match (&c.cause, c.witness.as_deref()) {
        (Some(cause), _) => format!("  by #{} {}: {}", cause.id, cause.kind, cause.label),
        (None, Some("state")) => "  (not seen live)".to_owned(),
        _ => String::new(),
    };
    format!("  {symbol} {path}  [{} {detail}]{by}", c.class.as_str())
}

pub fn diff(r: &DiffReport, stat_only: bool) {
    println!("from {}", endpoint(&r.from));
    println!("to   {}", endpoint(&r.to));
    println!(
        "     {} · seen by: {} · {} commands, {} KiB stream, {} ms{}",
        counts_line(&r.counts),
        r.witnesses.join("+"),
        r.commands,
        r.stream_bytes / 1024,
        r.compute_ms,
        if r.cached { ", cached" } else { "" }
    );
    for t in &r.transactions {
        println!("     {}", transaction_line(t));
    }
    if r.coverage.all > 0 {
        println!(
            "     explained by the live witness: {}/{} config changes, {}/{} overall",
            r.coverage.config_attributed,
            r.coverage.config,
            r.coverage.all_attributed,
            r.coverage.all
        );
    }
    for g in &r.gaps {
        println!(
            "     gap {} .. {}: {}",
            local(g.start_ms),
            local(g.end_ms),
            g.reason
        );
    }
    if !r.packages.is_empty() {
        println!("\npackages ({}):", r.packages.len());
        for p in &r.packages {
            let evr = match (&p.from_evr, &p.to_evr) {
                (Some(a), Some(b)) => format!("{a} -> {b}"),
                (Some(a), None) => a.clone(),
                (None, Some(b)) => b.clone(),
                (None, None) => String::new(),
            };
            println!("  {:<9} {}.{}  {evr}", p.action, p.name, p.arch);
        }
    }
    if stat_only {
        return;
    }
    if !r.changes.is_empty() {
        println!("\nfiles ({}):", r.changes.len());
        for c in &r.changes {
            println!("{}", change_line(c));
            for line in &c.summary {
                println!("      · {line}");
            }
        }
    }
    if r.hidden > 0 {
        println!(
            "\n{} package-owned, noise and times-only paths hidden (--all to show)",
            r.hidden
        );
    }
}

fn cause_line(c: &Cause) -> String {
    let mut line = format!(
        "#{:<5} {}  {:<8} {}",
        c.id,
        local(c.started_ms),
        c.kind,
        c.label
    );
    let mut counts = Vec::new();
    if c.changes > 0 {
        counts.push(format!(
            "{} change{}",
            c.changes,
            if c.changes == 1 { "" } else { "s" }
        ));
    }
    if c.package_paths > 0 {
        counts.push(format!("{} package paths", c.package_paths));
    }
    if !counts.is_empty() {
        let _ = write!(line, "  ({})", counts.join(", "));
    }
    line
}

pub fn causes(list: &[Cause]) {
    if list.is_empty() {
        println!("no causes recorded yet (the live witness records changes from now on)");
        return;
    }
    for c in list {
        println!("{}", cause_line(c));
    }
}

fn actor_line(a: &Actor) -> String {
    let mut s = format!(
        "{} (pid {}, uid {})",
        if a.exe.is_empty() { &a.comm } else { &a.exe },
        a.pid,
        a.uid
    );
    if !a.cgroup.is_empty() {
        let _ = write!(s, " in {}", a.cgroup);
    }
    s
}

fn live_line(c: &LiveChange) -> String {
    let symbol = match c.op.as_str() {
        "create" => "+",
        "delete" => "-",
        "rename" => ">",
        "attr" => "*",
        _ => "~",
    };
    let path = match &c.from {
        Some(from) => format!("{from} -> {}", c.path),
        None => c.path.clone(),
    };
    let content = if c.blob.is_some() {
        "  [content kept]"
    } else {
        ""
    };
    format!("  {symbol} {}  {path}{content}", local(c.ts_ms))
}

pub fn show(d: &CauseDetail) {
    let c = &d.cause;
    println!("{}", cause_line(c));
    println!("  actor    {}", actor_line(&c.actor));
    if !c.actor.command.is_empty() {
        println!("  command  {}", c.actor.command.join(" "));
    }
    if let Some(via) = &c.actor.via {
        println!("  via      {via}");
    }
    println!(
        "  window   {} .. {}",
        local(c.started_ms),
        local(c.ended_ms)
    );
    if c.package_paths > 0 {
        println!(
            "  plus {} package-owned paths written inside the transaction",
            c.package_paths
        );
    }
    for ch in &d.changes {
        println!("{}", live_line(ch));
        for line in &ch.summary {
            println!("        · {line}");
        }
    }
}

pub fn why(path: &str, entries: &[WhyEntry]) {
    if entries.is_empty() {
        println!("{path}: no changes recorded by the live witness");
        return;
    }
    for (i, e) in entries.iter().enumerate() {
        if i > 0 {
            println!();
        }
        println!("{}", live_line(&e.change).trim_start());
        println!("  by #{} {}: {}", e.cause.id, e.cause.kind, e.cause.label);
        if let Some(via) = &e.cause.actor.via {
            println!("  via {via}");
        }
        match &e.change.actor {
            Some(actor) => println!("  process {actor}"),
            None => println!("  actor {}", actor_line(&e.cause.actor)),
        }
        for line in &e.change.summary {
            println!("  · {line}");
        }
        match (&e.before, &e.after) {
            (Some(before), Some(after)) => {
                let d = osreflog_undo::textdiff::unified(before, after, 3);
                if d.is_empty() {
                    println!("  (content unchanged)");
                } else {
                    for line in d.lines() {
                        println!("  {line}");
                    }
                }
            }
            (None, Some(_)) => println!("  (no earlier content to compare with)"),
            (_, None) if e.change.op == "delete" => {}
            _ => println!("  (content not kept: secret, binary, too large, or not a config path)"),
        }
    }
}

fn short(s: &osreflog_undo::State) -> String {
    s.to_string()
}

/// `osreflog undo` without `--apply`: what would happen to each path.
pub fn undo_preview(p: &osreflog_undo::Preview) {
    use osreflog_undo::Verdict;
    println!("undo {}", cause_line(&p.cause));
    if let Some(osreflog_undo::Delegation::Dnf {
        transaction,
        description,
    }) = &p.delegated
    {
        println!("  a package transaction: dnf undoes it, with the rpm database and scriptlets");
        if !description.is_empty() {
            println!("  transaction {transaction}: {description}");
        }
        println!(
            "  run: sudo dnf5 history undo {transaction}   (or: sudo osreflog undo {} --apply)",
            p.cause.id
        );
        return;
    }
    if p.steps.is_empty() && p.skipped.is_empty() {
        println!("  nothing recorded for this cause");
    }
    for step in &p.steps {
        let (mark, what) = match &step.verdict {
            Verdict::Restore => (
                "~",
                match &step.restore {
                    osreflog_undo::State::Absent => "remove (to the attic)".to_owned(),
                    r => format!("restore {}", short(r)),
                },
            ),
            Verdict::AlreadyUndone => ("=", "already as it was before".to_owned()),
            Verdict::Conflict => (
                "!",
                match &step.current {
                    Ok(c) => format!(
                        "conflict: it changed after this cause (now {}, the cause left {})",
                        short(c),
                        short(&step.expect)
                    ),
                    Err(e) => format!("conflict: unreadable ({e})"),
                },
            ),
            Verdict::Refused { reason } => ("x", format!("refused: {reason}")),
        };
        println!("  {mark} {}  {what}", step.path);
        if step.verdict == Verdict::Restore
            && let (Some(now), Some(then)) = (&step.current_text, &step.restore_text)
        {
            for line in osreflog_undo::textdiff::unified(now, then, 2)
                .lines()
                .take(40)
            {
                println!("      {line}");
            }
        }
    }
    if !p.skipped.is_empty() {
        println!("  left alone:");
        for s in &p.skipped {
            println!("    {}: {}", s.path, s.reason);
        }
    }
    let n = p.to_restore();
    if n > 0 {
        println!(
            "dry run: nothing changed. {n} path{} to restore: sudo osreflog undo {} --apply",
            if n == 1 { "" } else { "s" },
            p.cause.id
        );
    }
}

/// The result of `osreflog undo --apply`.
pub fn undo_applied(a: &osreflog_undo::Applied) {
    use osreflog_undo::Outcome;
    println!(
        "pre-undo checkpoint {} (c{}), verified",
        a.pre_undo.name, a.pre_undo.id
    );
    for s in &a.report.steps {
        let line = match &s.outcome {
            Outcome::Restored { warnings } if warnings.is_empty() => "restored".to_owned(),
            Outcome::Restored { warnings } => format!("restored ({})", warnings.join("; ")),
            Outcome::AlreadyUndone => "already as it was before".to_owned(),
            Outcome::Conflict { current, reason } => {
                format!("conflict, left alone: {reason} (now {current})")
            }
            Outcome::Refused { reason } => format!("refused: {reason}"),
            Outcome::Failed { error } => format!("failed: {error}"),
        };
        println!("  {}  {line}", s.path);
    }
    println!("replaced versions are in {}", a.report.attic);
    if a.reloaded_systemd {
        println!("systemd reloaded (unit files or enablement links changed)");
    }
    if let Some(id) = a.undo_cause {
        println!("recorded as #{id}; `osreflog undo {id}` puts the change back");
    }
}
