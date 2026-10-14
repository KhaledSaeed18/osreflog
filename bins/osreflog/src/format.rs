//! Human-readable output.

use std::fmt::Write as _;

use osreflog_model::{Change, ClassCounts, DiffReport, Endpoint, LogEntry, Status, Transaction};

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
    format!("  {symbol} {path}  [{} {detail}]", c.class.as_str())
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
        }
    }
    if r.hidden > 0 {
        println!(
            "\n{} package-owned and noise paths hidden (--all to show)",
            r.hidden
        );
    }
}
