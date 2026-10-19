//! What the window shows, as plain data: causes grouped by day, a cause's lines, the status
//! line. No GTK here, so it is tested on its own; the window only lays it out.

use std::fmt::Write as _;

use osreflog_model::{
    Cause, CauseDetail, Change, Class, ClassCounts, DiffReport, Endpoint, LiveChange, LogEntry,
    Status,
};

/// One cause in the timeline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub id: i64,
    pub time: String,
    pub title: String,
    pub subtitle: String,
    pub icon: &'static str,
}

/// Causes of one day, newest first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Day {
    pub label: String,
    pub rows: Vec<Row>,
}

/// A symbolic icon for a kind of cause.
#[must_use]
pub fn kind_icon(kind: &str) -> &'static str {
    match kind {
        "dnf" => "system-software-install-symbolic",
        "unit" => "emblem-system-symbolic",
        "command" => "utilities-terminal-symbolic",
        "app" => "application-x-executable-symbolic",
        "undo" => "edit-undo-symbolic",
        _ => "dialog-question-symbolic",
    }
}

fn plural(n: u64, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// The second line of a cause's row: its kind and how much it changed.
#[must_use]
pub fn subtitle(c: &Cause) -> String {
    let mut s = format!("{} · {}", c.kind, plural(c.changes, "change", "changes"));
    if c.package_paths > 0 {
        let (one, many) = if c.kind == "flatpak" {
            ("Flatpak file", "Flatpak files")
        } else {
            ("package file", "package files")
        };
        let _ = write!(s, " · {}", plural(c.package_paths, one, many));
    }
    s
}

/// The style class for one line of a line diff: "error" for a removed line, "success" for an
/// added one, "dim-label" for a hunk header; none for anything else.
#[must_use]
pub fn line_tone(line: &str) -> Option<&'static str> {
    if line.starts_with("@@") {
        Some("dim-label")
    } else if line.starts_with('-') && !line.starts_with("---") {
        Some("error")
    } else if line.starts_with('+') && !line.starts_with("+++") {
        Some("success")
    } else {
        None
    }
}

/// Whether a cause matches what was typed in the search: every word of `query`, ignoring case,
/// is in its label, kind, process name, command or `#id`. An empty query matches everything.
#[must_use]
pub fn matches(c: &Cause, query: &str) -> bool {
    let hay = format!(
        "#{} {} {} {} {}",
        c.id,
        c.label,
        c.kind,
        c.actor.comm,
        c.actor.command.join(" ")
    )
    .to_lowercase();
    query
        .to_lowercase()
        .split_whitespace()
        .all(|w| hay.contains(w))
}

/// Groups `causes` (newest first) by local day. `local(ms)` gives the day ("2026-10-16") and
/// the time ("14:33") of a timestamp; `today` and `yesterday` are day strings in the same form.
pub fn days(
    causes: &[Cause],
    local: impl Fn(i64) -> (String, String),
    today: &str,
    yesterday: &str,
) -> Vec<Day> {
    let mut out: Vec<(String, Day)> = Vec::new();
    for c in causes {
        let (day, time) = local(c.started_ms);
        let row = Row {
            id: c.id,
            time,
            title: c.label.clone(),
            subtitle: subtitle(c),
            icon: kind_icon(&c.kind),
        };
        match out.last_mut() {
            Some((d, group)) if *d == day => group.rows.push(row),
            _ => {
                let label = if day == today {
                    "Today".to_owned()
                } else if day == yesterday {
                    "Yesterday".to_owned()
                } else {
                    day.clone()
                };
                out.push((
                    day,
                    Day {
                        label,
                        rows: vec![row],
                    },
                ));
            }
        }
    }
    out.into_iter().map(|(_, d)| d).collect()
}

/// The facts above a cause's changes, as (label, value) pairs.
pub fn facts(d: &CauseDetail, local: impl Fn(i64) -> String) -> Vec<(&'static str, String)> {
    let c = &d.cause;
    let a = &c.actor;
    let mut f = vec![("Kind", c.kind.clone())];
    let who = if a.pid == 0 {
        a.comm.clone()
    } else {
        format!("{} (pid {}, uid {})", a.comm, a.pid, a.uid)
    };
    if !who.is_empty() {
        f.push(("Process", who));
    }
    if !a.command.is_empty() {
        f.push(("Command", a.command.join(" ")));
    }
    if let Some(via) = &a.via {
        f.push(("Via", via.clone()));
    }
    if !a.cgroup.is_empty() {
        f.push(("Cgroup", a.cgroup.clone()));
    }
    let (from, to) = (local(c.started_ms), local(c.ended_ms));
    f.push((
        "When",
        if from == to {
            from
        } else {
            format!("{from} – {to}")
        },
    ));
    if c.package_paths > 0 && c.kind == "flatpak" {
        f.push((
            "Flatpak files",
            format!(
                "{} written in Flatpak's installation, not listed (flatpak history has them)",
                c.package_paths
            ),
        ));
    } else if c.package_paths > 0 {
        f.push((
            "Package files",
            format!(
                "{} written inside the transaction, not listed",
                c.package_paths
            ),
        ));
    }
    f
}

/// A change's title: what happened to which path.
#[must_use]
pub fn change_title(ch: &LiveChange) -> String {
    let path = match (&ch.from, ch.op.as_str()) {
        (Some(from), "rename") => format!("{from} → {}", ch.path),
        _ => ch.path.clone(),
    };
    match &ch.target {
        Some(t) => format!("{path} → {t}"),
        None => path,
    }
}

/// A change's subtitle: the operation, what kind of file, how big it is now.
#[must_use]
pub fn change_subtitle(ch: &LiveChange) -> String {
    let op = match ch.op.as_str() {
        "create" => "created",
        "modify" => "changed",
        "delete" => "deleted",
        "rename" => "renamed",
        "attr" => "attributes changed",
        other => other,
    };
    let mut s = format!(
        "{op} {}",
        if ch.kind == "dir" {
            "directory"
        } else {
            "file"
        }
    );
    if let Some(actor) = &ch.actor {
        let _ = write!(s, " by {actor}");
    }
    if let Some(size) = ch.size {
        let _ = write!(s, " · {}", plural(size, "byte", "bytes"));
    }
    s
}

/// The line under the window's title: what the daemon is watching.
#[must_use]
pub fn status_line(s: &Status, local: impl Fn(i64) -> String) -> String {
    let live = match &s.live {
        Some(l) if l.running => "recording changes".to_owned(),
        _ => "not recording live changes (checkpoints only)".to_owned(),
    };
    let space = s.space.as_ref().map_or_else(String::new, |sp| {
        format!(
            " · snapshots hold {}{}",
            sp.range_text(),
            if sp.complete {
                ""
            } else {
                " (measured since 0.3.0)"
            }
        )
    });
    match &s.latest {
        Some(c) => format!(
            "{live} · {} · last checkpoint {} ({}){space}",
            plural(s.checkpoints, "checkpoint", "checkpoints"),
            local(c.created_ms),
            c.reason
        ),
        None => format!("{live} · no checkpoint yet"),
    }
}

/// The status in a few words, for the sidebar's title, where the full line doesn't fit.
#[must_use]
pub fn status_short(s: &Status) -> String {
    let live = match &s.live {
        Some(l) if l.running => "recording",
        _ => "checkpoints only",
    };
    format!(
        "{live} · {}",
        plural(s.checkpoints, "checkpoint", "checkpoints")
    )
}

/// "86 config · 33 state · 12267 package", leaving out classes with nothing in them.
#[must_use]
pub fn counts_line(c: &ClassCounts) -> String {
    let parts: Vec<String> = [
        (c.config, "config"),
        (c.state, "state"),
        (c.package, "package"),
        (c.other, "other"),
        (c.noise, "noise"),
    ]
    .iter()
    .filter(|(n, _)| *n > 0)
    .map(|(n, what)| format!("{n} {what}"))
    .collect();
    if parts.is_empty() {
        "no changes".to_owned()
    } else {
        parts.join(" · ")
    }
}

/// A symbolic icon for why a checkpoint was taken.
#[must_use]
pub fn reason_icon(reason: &str) -> &'static str {
    match reason {
        "boot" => "system-reboot-symbolic",
        "pre-dnf" | "post-dnf" => "system-software-install-symbolic",
        "manual" => "bookmark-new-symbolic",
        "pre-undo" => "edit-undo-symbolic",
        _ => "document-save-symbolic",
    }
}

/// A checkpoint in the list: what it is and what changed in the interval before it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckpointRow {
    pub name: String,
    pub time: String,
    pub title: String,
    pub subtitle: String,
    pub icon: &'static str,
}

/// Checkpoints (newest first) grouped by local day, as [`days`] does for causes.
pub fn checkpoint_days(
    entries: &[LogEntry],
    local: impl Fn(i64) -> (String, String),
    today: &str,
    yesterday: &str,
) -> Vec<(String, Vec<CheckpointRow>)> {
    let mut out: Vec<(String, String, Vec<CheckpointRow>)> = Vec::new();
    for e in entries {
        let c = &e.checkpoint;
        let (day, time) = local(c.created_ms);
        let mut title = format!("{}  {}", c.name, c.reason);
        if let Some(note) = c.note.as_deref().filter(|n| !n.is_empty()) {
            let _ = write!(title, " · {note}");
        } else if let Some(pm) = &c.pm_comm {
            let _ = write!(title, " · {pm}");
        }
        let subtitle = match &e.interval {
            Some(iv) => {
                let mut s = format!("before it: {}", counts_line(&iv.counts));
                for t in &iv.transactions {
                    let _ = write!(s, " · dnf #{}", t.id);
                }
                s
            }
            None if c.pruned_ms.is_some() => "snapshot pruned".to_owned(),
            None => "no diff yet".to_owned(),
        };
        let row = CheckpointRow {
            name: c.name.clone(),
            time,
            title,
            subtitle,
            icon: reason_icon(&c.reason),
        };
        match out.last_mut() {
            Some((d, _, rows)) if *d == day => rows.push(row),
            _ => {
                let label = if day == today {
                    "Today".to_owned()
                } else if day == yesterday {
                    "Yesterday".to_owned()
                } else {
                    day.clone()
                };
                out.push((day, label, vec![row]));
            }
        }
    }
    out.into_iter().map(|(_, l, r)| (l, r)).collect()
}

/// One side of a diff, as a person reads it.
#[must_use]
pub fn endpoint_label(e: &Endpoint, local: impl Fn(i64) -> String) -> String {
    match e {
        Endpoint::Checkpoint(c) => format!("{} ({}, {})", c.name, c.reason, local(c.created_ms)),
        Endpoint::Live { taken_ms } => format!("now ({})", local(*taken_ms)),
    }
}

/// The facts above a diff's files, as (label, value) pairs.
pub fn diff_facts(r: &DiffReport, local: impl Fn(i64) -> String) -> Vec<(&'static str, String)> {
    let mut f = vec![
        ("From", endpoint_label(&r.from, &local)),
        ("To", endpoint_label(&r.to, &local)),
        ("Changes", counts_line(&r.counts)),
    ];
    let cov = &r.coverage;
    if cov.all > 0 {
        f.push((
            "Explained",
            format!(
                "{}/{} config changes, {}/{} overall have a recorded cause",
                cov.config_attributed, cov.config, cov.all_attributed, cov.all
            ),
        ));
    }
    for t in &r.transactions {
        f.push((
            "Package transaction",
            format!("dnf #{} {}", t.id, t.description),
        ));
    }
    for g in &r.gaps {
        f.push((
            "Not watched",
            format!("{} – {}: {}", local(g.start_ms), local(g.end_ms), g.reason),
        ));
    }
    f
}

/// A package change: "upgrade firefox 157.0.1-1.fc44 → 158.0-1.fc44".
#[must_use]
pub fn package_line(p: &osreflog_model::PackageChange) -> String {
    match (&p.from_evr, &p.to_evr) {
        (Some(a), Some(b)) => format!("{} {} {a} → {b}", p.action, p.name),
        (None, Some(b)) => format!("{} {} {b}", p.action, p.name),
        (Some(a), None) => format!("{} {} {a}", p.action, p.name),
        (None, None) => format!("{} {}", p.action, p.name),
    }
}

/// A changed file in a diff.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffRow {
    pub title: String,
    pub subtitle: String,
    /// The cause that made it, when one was recorded: (id, "unit tlp.service").
    pub cause: Option<(i64, String)>,
    pub summary: Vec<String>,
}

fn diff_row(c: &Change) -> DiffRow {
    let title = match (&c.from, c.op.as_str()) {
        (Some(from), "rename") => format!("{from} → {}", c.path),
        _ => c.path.clone(),
    };
    let op = match c.op.as_str() {
        "create" => "created",
        "modify" => "changed",
        "delete" => "deleted",
        "rename" => "renamed",
        other => other,
    };
    let mut subtitle = format!("{op} {}", c.kind);
    if c.op == "modify" && !c.what.is_empty() {
        let _ = write!(subtitle, " ({})", c.what.join(", "));
    }
    match &c.cause {
        Some(cause) => {
            let _ = write!(
                subtitle,
                " · by #{} {} {}",
                cause.id, cause.kind, cause.label
            );
        }
        None if c.witness.as_deref() == Some("state") => {
            subtitle.push_str(" · no recorded cause");
        }
        None => {}
    }
    DiffRow {
        title,
        subtitle,
        cause: c
            .cause
            .as_ref()
            .map(|r| (r.id, format!("{} {}", r.kind, r.label))),
        summary: c.summary.clone(),
    }
}

/// A diff's changed files, grouped by class in the order a person cares about.
#[must_use]
pub fn diff_groups(r: &DiffReport) -> Vec<(&'static str, Vec<DiffRow>)> {
    let order = [
        (Class::Config, "Configuration"),
        (Class::State, "State"),
        (Class::Other, "Elsewhere"),
        (Class::Package, "Package files"),
        (Class::Noise, "Noise"),
    ];
    order
        .iter()
        .filter_map(|(class, title)| {
            let rows: Vec<DiffRow> = r
                .changes
                .iter()
                .filter(|c| c.class == *class)
                .map(diff_row)
                .collect();
            (!rows.is_empty()).then_some((*title, rows))
        })
        .collect()
}

/// One path of an undo preview: what undo would do with it, and the text diff when it's a
/// text file being put back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UndoRow {
    pub path: String,
    pub what: String,
    /// `restore`, `same`, `conflict` or `refused`, for the row's icon and style.
    pub verdict: &'static str,
    pub diff: Option<String>,
}

/// An undo preview as rows, in the order the daemon gave them.
#[must_use]
pub fn undo_rows(p: &osreflog_undo::Preview) -> Vec<UndoRow> {
    use osreflog_undo::{State, Verdict};
    p.steps
        .iter()
        .map(|step| {
            let (verdict, what) = match &step.verdict {
                Verdict::Restore => (
                    "restore",
                    match &step.restore {
                        State::Absent => "remove it (it goes to the attic)".to_owned(),
                        r => format!("put back the {r} it was before"),
                    },
                ),
                Verdict::AlreadyUndone => ("same", "already as it was before".to_owned()),
                Verdict::Conflict => (
                    "conflict",
                    match &step.current {
                        Ok(c) => format!(
                            "changed after this cause (now {c}, the cause left {}): left alone",
                            step.expect
                        ),
                        Err(e) => format!("can't be read ({e}): left alone"),
                    },
                ),
                Verdict::Refused { reason } => ("refused", format!("never undone: {reason}")),
            };
            let diff = match (&step.verdict, &step.current_text, &step.restore_text) {
                (Verdict::Restore, Some(now), Some(then)) => Some(
                    osreflog_undo::textdiff::unified(now, then, 2)
                        .lines()
                        .take(60)
                        .collect::<Vec<_>>()
                        .join("\n"),
                ),
                _ => None,
            };
            UndoRow {
                path: step.path.clone(),
                what,
                verdict,
                diff: diff.filter(|d| !d.is_empty()),
            }
        })
        .collect()
}

/// The line above an undo preview's paths.
#[must_use]
pub fn undo_summary(p: &osreflog_undo::Preview) -> String {
    use osreflog_undo::Verdict;
    if let Some(osreflog_undo::Delegation::Dnf { transaction, .. }) = &p.delegated {
        return format!(
            "A package transaction: dnf undoes it, with the package database and scriptlets. Run  sudo dnf5 history undo {transaction}  in a terminal."
        );
    }
    let count = |v: fn(&Verdict) -> bool| p.steps.iter().filter(|s| v(&s.verdict)).count();
    let restore = p.to_restore();
    let conflicts = count(|v| *v == Verdict::Conflict);
    let refused = count(|v| matches!(v, Verdict::Refused { .. }));
    let same = count(|v| *v == Verdict::AlreadyUndone);
    if p.steps.is_empty() {
        return "Nothing recorded for this cause can be undone.".to_owned();
    }
    let mut parts = if restore > 0 {
        vec![format!("{restore} to put back")]
    } else {
        Vec::new()
    };
    if conflicts > 0 {
        parts.push(format!("{conflicts} changed since (left alone)"));
    }
    if refused > 0 {
        parts.push(format!("{refused} never undone"));
    }
    if same > 0 {
        parts.push(format!("{same} already as before"));
    }
    if restore == 0 {
        return format!("Nothing to put back: {}.", parts.join(", "));
    }
    format!("Nothing has changed yet: {}.", parts.join(", "))
}

/// What an applied undo did, path by path: (path, outcome, ok).
#[must_use]
pub fn applied_rows(a: &osreflog_undo::Applied) -> Vec<(String, String, bool)> {
    use osreflog_undo::Outcome;
    a.report
        .steps
        .iter()
        .map(|s| {
            let (line, ok) = match &s.outcome {
                Outcome::Restored { warnings } if warnings.is_empty() => {
                    ("put back".to_owned(), true)
                }
                Outcome::Restored { warnings } => {
                    (format!("put back ({})", warnings.join("; ")), true)
                }
                Outcome::AlreadyUndone => ("already as it was before".to_owned(), true),
                Outcome::Conflict { current, reason } => {
                    (format!("left alone: {reason} (now {current})"), false)
                }
                Outcome::Refused { reason } => (format!("refused: {reason}"), false),
                Outcome::Failed { error } => (format!("failed: {error}"), false),
            };
            (s.path.clone(), line, ok)
        })
        .collect()
}

/// The tier a person picks in the bisect form.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TierChoice {
    Auto,
    Nspawn,
    Vm,
    Metal,
}

/// The bisect form, as filled in: one field per row of the form, switches included.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BisectForm {
    pub good: String,
    pub bad: String,
    /// A person gives the verdicts (`bisect start`), not a test (`bisect run`).
    pub manual: bool,
    pub tier: TierChoice,
    pub needs_kernel: bool,
    pub needs_hardware: bool,
    /// nspawn, automatic: boot systemd in the container.
    pub boot: bool,
    /// The test, for an automatic bisect: a shell snippet, or a script starting with `#!`.
    pub test: String,
}

/// The daemon's arguments for a form, or what's wrong with it.
///
/// # Errors
/// A sentence for the person: no good end, no test, a tier that can't do it.
pub fn bisect_args(f: &BisectForm) -> Result<osreflog_bisect::Args, String> {
    use osreflog_bisect::{Need, Tier};
    if f.good.is_empty() {
        return Err("Pick a checkpoint where things still worked.".into());
    }
    if f.good == f.bad {
        return Err("The good and bad ends are the same checkpoint.".into());
    }
    let mut needs = Vec::new();
    if f.needs_kernel {
        needs.push(Need::Kernel);
    }
    if f.needs_hardware {
        needs.push(Need::Hardware);
    }
    let tier = match f.tier {
        TierChoice::Auto => Tier::auto(&needs, f.manual)?,
        TierChoice::Nspawn => Tier::Nspawn,
        TierChoice::Vm if f.manual => {
            return Err("Verdicts by hand need a container you enter, or a reboot (metal).".into());
        }
        TierChoice::Vm => Tier::Vm,
        TierChoice::Metal if !f.manual => {
            return Err("A metal probe can't judge itself: give the verdicts by hand.".into());
        }
        TierChoice::Metal => Tier::Metal,
    };
    let test = f.test.trim();
    if !f.manual && test.is_empty() {
        return Err("An automatic bisect needs a test: exit 0 good, 125 skip, 1–127 bad.".into());
    }
    let script = if f.manual {
        String::new()
    } else if test.starts_with("#!") {
        format!("{test}\n")
    } else {
        format!("#!/bin/sh\n{test}\n")
    };
    Ok(osreflog_bisect::Args {
        good: f.good.clone(),
        bad: f.bad.clone(),
        tier,
        boot: f.boot && !f.manual && tier == Tier::Nspawn,
        test: script,
        timeout_s: None,
        manual: f.manual,
    })
}

fn tier_word(t: osreflog_bisect::Tier) -> &'static str {
    match t {
        osreflog_bisect::Tier::Nspawn => "containers",
        osreflog_bisect::Tier::Vm => "VMs",
        osreflog_bisect::Tier::Metal => "reboots (metal)",
    }
}

/// "c000128 … c000129, reboots (metal), verdicts by hand".
#[must_use]
pub fn session_title(s: &osreflog_bisect::Status) -> String {
    format!(
        "{} … {}, {}{}",
        s.args.good,
        s.args.bad,
        tier_word(s.args.tier),
        if s.args.manual {
            ", verdicts by hand"
        } else {
            ""
        }
    )
}

/// Where a session stands, in a few words.
#[must_use]
pub fn session_state(s: &osreflog_bisect::Status) -> String {
    use osreflog_bisect::Finding;
    if let Some(w) = &s.waiting {
        return if w.reboot {
            format!("Probe {} is armed: reboot into it", w.probe)
        } else {
            format!("Probe {} waits for your verdict", w.probe)
        };
    }
    if s.running {
        return format!("Running: {} probes so far", s.probes.len());
    }
    if let Some(e) = &s.error {
        return format!("Failed: {e}");
    }
    match &s.finding {
        Some(Finding::Cause { culprit, .. }) => format!("Found #{} {}", culprit.id, culprit.label),
        Some(Finding::Causes { candidates }) => format!("One of {} causes", candidates.len()),
        Some(Finding::Checkpoint { first_bad, .. }) => format!("First bad checkpoint {first_bad}"),
        Some(Finding::NotReproduced { .. }) => "Not reproduced".to_owned(),
        None => "Finished".to_owned(),
    }
}

/// A probe as a row: (icon, title, subtitle, output).
#[must_use]
pub fn probe_rows(s: &osreflog_bisect::Status) -> Vec<(&'static str, String, String, String)> {
    use osreflog_bisect::Verdict;
    s.probes
        .iter()
        .map(|p| {
            let (icon, word) = match p.verdict {
                Verdict::Good => ("object-select-symbolic", "good"),
                Verdict::Bad => ("process-stop-symbolic", "bad"),
                Verdict::Skip => ("media-skip-forward-symbolic", "skipped"),
            };
            let mut sub = format!("{word} · {}.{} s", p.ms / 1000, p.ms % 1000 / 100);
            if let Some(kib) = p.peak_rss_kib {
                let _ = write!(sub, " · qemu {} MiB", kib / 1024);
            }
            if !p.approximate.is_empty() {
                let _ = write!(
                    sub,
                    " · {} approximate",
                    plural(p.approximate.len() as u64, "path", "paths")
                );
            }
            if !p.detail.is_empty() {
                let _ = write!(sub, " · {}", p.detail);
            }
            (icon, p.what.clone(), sub, p.output.clone())
        })
        .collect()
}

/// What a finished session found: a title, lines under it, and the cause to open, if one.
#[must_use]
pub fn finding(s: &osreflog_bisect::Status) -> Option<(String, Vec<String>, Option<i64>)> {
    use osreflog_bisect::Finding;
    if let Some(e) = &s.error {
        return Some(("The bisect failed".into(), vec![e.clone()], None));
    }
    Some(match s.finding.as_ref()? {
        Finding::Cause {
            culprit,
            between,
            approximate,
        } => {
            let mut lines = vec![
                format!("{} {}", culprit.kind, culprit.label),
                format!("between {} (good) and {} (bad)", between.0, between.1),
            ];
            lines.extend(culprit.paths.iter().take(10).cloned());
            if !approximate.is_empty() {
                lines.push(format!("approximate replays: {}", approximate.join(", ")));
            }
            (
                format!("First bad cause: #{}", culprit.id),
                lines,
                Some(culprit.id),
            )
        }
        Finding::Causes { candidates } => (
            "One of these causes".into(),
            candidates
                .iter()
                .map(|c| format!("#{} {} {}", c.id, c.kind, c.label))
                .collect(),
            None,
        ),
        Finding::Checkpoint {
            last_good,
            first_bad,
            why,
        } => (
            format!("First bad checkpoint: {first_bad}"),
            vec![format!("last good {last_good}"), why.clone()],
            None,
        ),
        Finding::NotReproduced { detail } => {
            let next = s.args.tier.next(s.args.manual).map_or(String::new(), |t| {
                format!("Try again with {}.", tier_word(t))
            });
            ("Not reproduced".into(), vec![detail.clone(), next], None)
        }
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use osreflog_model::{Actor, LiveStatus};

    use super::*;

    fn cause(id: i64, ms: i64, kind: &str, changes: u64, pkg: u64) -> Cause {
        Cause {
            id,
            kind: kind.into(),
            label: format!("cause {id}"),
            actor: Actor::default(),
            started_ms: ms,
            ended_ms: ms,
            changes,
            package_paths: pkg,
        }
    }

    /// Days are 1000 ms long here: day "d0" holds 0..999, "d1" 1000..1999, and so on.
    fn local(ms: i64) -> (String, String) {
        (format!("d{}", ms / 1000), format!("t{}", ms % 1000))
    }

    #[test]
    fn causes_group_by_day_newest_first() {
        let c = [
            cause(9, 2500, "command", 1, 0),
            cause(8, 2100, "dnf", 3, 120),
            cause(7, 1900, "unit", 2, 0),
            cause(6, 100, "weird", 1, 0),
        ];
        let d = days(&c, local, "d2", "d1");
        assert_eq!(d.len(), 3);
        assert_eq!(d[0].label, "Today");
        assert_eq!(d[0].rows.iter().map(|r| r.id).collect::<Vec<_>>(), [9, 8]);
        assert_eq!(d[1].label, "Yesterday");
        assert_eq!(d[2].label, "d0");
        assert_eq!(d[0].rows[1].subtitle, "dnf · 3 changes · 120 package files");
        assert_eq!(d[0].rows[0].subtitle, "command · 1 change");
        assert_eq!(d[0].rows[1].icon, "system-software-install-symbolic");
        assert_eq!(d[2].rows[0].icon, "dialog-question-symbolic");
        assert_eq!(d[0].rows[0].time, "t500");
        assert_eq!(days(&[], local, "d2", "d1"), Vec::<Day>::new());
    }

    #[test]
    fn a_cause_reads_as_facts_and_changes() {
        let mut c = cause(1, 1000, "command", 2, 0);
        c.ended_ms = 2000;
        c.actor = Actor {
            pid: 42,
            comm: "sed".into(),
            uid: 0,
            cgroup: "/user.slice".into(),
            command: vec![
                "sudo".into(),
                "sed".into(),
                "-i".into(),
                "/etc/hosts".into(),
            ],
            via: None,
            ..Actor::default()
        };
        let d = CauseDetail {
            cause: c,
            changes: vec![],
        };
        let f = facts(&d, |ms| format!("at {ms}"));
        assert_eq!(f[1], ("Process", "sed (pid 42, uid 0)".into()));
        assert_eq!(f[2], ("Command", "sudo sed -i /etc/hosts".into()));
        assert_eq!(f.last().unwrap(), &("When", "at 1000 – at 2000".into()));

        let ch = LiveChange {
            ts_ms: 1,
            path: "/etc/b".into(),
            op: "rename".into(),
            from: Some("/etc/a".into()),
            kind: "file".into(),
            blob: None,
            size: Some(1),
            actor: Some("mv (pid 7)".into()),
            target: None,
            summary: vec![],
        };
        assert_eq!(change_title(&ch), "/etc/a → /etc/b");
        assert_eq!(change_subtitle(&ch), "renamed file by mv (pid 7) · 1 byte");
        let link = LiveChange {
            op: "create".into(),
            from: None,
            path: "/etc/x".into(),
            target: Some("/dev/null".into()),
            size: None,
            actor: None,
            ..ch
        };
        assert_eq!(change_title(&link), "/etc/x → /dev/null");
        assert_eq!(change_subtitle(&link), "created file");
    }

    fn checkpoint(
        id: i64,
        ms: i64,
        reason: &str,
        note: Option<&str>,
    ) -> osreflog_model::Checkpoint {
        osreflog_model::Checkpoint {
            id,
            name: format!("c{id:06}"),
            created_ms: ms,
            reason: reason.into(),
            generation: 1,
            boot_id: String::new(),
            note: note.map(str::to_owned),
            pm_pid: None,
            pm_comm: None,
            pinned: false,
            pruned_ms: None,
        }
    }

    #[test]
    fn checkpoints_read_with_their_interval() {
        let entries = vec![
            LogEntry {
                checkpoint: checkpoint(3, 2200, "post-dnf", None),
                interval: Some(osreflog_model::IntervalSummary {
                    from_id: 2,
                    counts: ClassCounts {
                        config: 2,
                        package: 120,
                        noise: 4,
                        ..ClassCounts::default()
                    },
                    packages: 1,
                    transactions: vec![osreflog_model::Transaction {
                        id: 53,
                        begin: 0,
                        end: None,
                        user_id: 1000,
                        description: "dnf install figlet".into(),
                        state: "Ok".into(),
                    }],
                }),
            },
            LogEntry {
                checkpoint: checkpoint(2, 2100, "manual", Some("before the update")),
                interval: None,
            },
            LogEntry {
                checkpoint: checkpoint(1, 900, "boot", None),
                interval: None,
            },
        ];
        let d = checkpoint_days(&entries, local, "d2", "d1");
        assert_eq!(d.len(), 2);
        assert_eq!(d[0].0, "Today");
        assert_eq!(d[0].1[0].title, "c000003  post-dnf");
        assert_eq!(
            d[0].1[0].subtitle,
            "before it: 2 config · 120 package · 4 noise · dnf #53"
        );
        assert_eq!(d[0].1[1].title, "c000002  manual · before the update");
        assert_eq!(d[0].1[1].subtitle, "no diff yet");
        assert_eq!(d[1].0, "d0");
        assert_eq!(d[1].1[0].icon, "system-reboot-symbolic");
        assert_eq!(counts_line(&ClassCounts::default()), "no changes");
    }

    #[test]
    fn a_diff_reads_as_facts_and_grouped_files() {
        let change = |path: &str, class: Class, cause: Option<i64>| Change {
            path: path.into(),
            op: "modify".into(),
            from: None,
            kind: "file".into(),
            class,
            what: vec!["content".into()],
            bytes: 10,
            cause: cause.map(|id| osreflog_model::CauseRef {
                id,
                kind: "unit".into(),
                label: "tlp.service".into(),
            }),
            witness: Some(if cause.is_some() { "both" } else { "state" }.into()),
            summary: vec!["wifi.powersave 2 → 3".into()],
        };
        let r = DiffReport {
            from: Endpoint::Checkpoint(checkpoint(1, 1000, "boot", None)),
            to: Endpoint::Live { taken_ms: 2000 },
            witnesses: vec!["state".into(), "live".into()],
            cached: false,
            compute_ms: 1,
            stream_bytes: 1,
            commands: 1,
            changed_bytes: 1,
            counts: ClassCounts {
                config: 1,
                state: 1,
                ..ClassCounts::default()
            },
            packages: vec![osreflog_model::PackageChange {
                name: "firefox".into(),
                arch: "x86_64".into(),
                action: "upgrade".into(),
                from_evr: Some("157.0.1-1.fc44".into()),
                to_evr: Some("158.0-1.fc44".into()),
            }],
            transactions: vec![],
            changes: vec![
                change("/var/lib/x", Class::State, None),
                change(
                    "/etc/NetworkManager/conf.d/wifi.conf",
                    Class::Config,
                    Some(7),
                ),
            ],
            hidden: 0,
            coverage: osreflog_model::Coverage {
                config: 1,
                config_attributed: 1,
                all: 2,
                all_attributed: 1,
            },
            gaps: vec![osreflog_model::Gap {
                id: 1,
                start_ms: 1100,
                end_ms: 1200,
                reason: "osreflogd was stopped".into(),
            }],
        };
        let f = diff_facts(&r, |ms| format!("t{ms}"));
        assert_eq!(f[0], ("From", "c000001 (boot, t1000)".into()));
        assert_eq!(f[1], ("To", "now (t2000)".into()));
        assert_eq!(
            f[3].1,
            "1/1 config changes, 1/2 overall have a recorded cause"
        );
        assert_eq!(
            f[4],
            ("Not watched", "t1100 – t1200: osreflogd was stopped".into())
        );
        assert_eq!(
            package_line(&r.packages[0]),
            "upgrade firefox 157.0.1-1.fc44 → 158.0-1.fc44"
        );
        let g = diff_groups(&r);
        assert_eq!(
            g.iter().map(|(t, _)| *t).collect::<Vec<_>>(),
            ["Configuration", "State"]
        );
        assert_eq!(
            g[0].1[0].subtitle,
            "changed file (content) · by #7 unit tlp.service"
        );
        assert_eq!(g[0].1[0].cause, Some((7, "unit tlp.service".into())));
        assert_eq!(
            g[1].1[0].subtitle,
            "changed file (content) · no recorded cause"
        );
    }

    fn preview(steps: Vec<osreflog_undo::preview::PreviewStep>) -> osreflog_undo::Preview {
        osreflog_undo::Preview {
            cause: cause(5, 1000, "command", steps.len() as u64, 0),
            digest: "d".into(),
            steps,
            skipped: vec![],
            delegated: None,
        }
    }

    #[test]
    fn an_undo_preview_says_what_would_happen() {
        use osreflog_undo::State;
        use osreflog_undo::preview::{PreviewStep, Verdict};
        let file = |h: &str| State::File { hash: h.into() };
        let p = preview(vec![
            PreviewStep {
                path: "/etc/a.conf".into(),
                expect: file("aaaa"),
                restore: file("bbbb"),
                current: Ok(file("aaaa")),
                verdict: Verdict::Restore,
                current_text: Some("x = 2\n".into()),
                restore_text: Some("x = 1\n".into()),
            },
            PreviewStep {
                path: "/etc/new.conf".into(),
                expect: file("cccc"),
                restore: State::Absent,
                current: Ok(file("cccc")),
                verdict: Verdict::Restore,
                current_text: None,
                restore_text: None,
            },
            PreviewStep {
                path: "/etc/b.conf".into(),
                expect: file("dddd"),
                restore: file("eeee"),
                current: Ok(file("ffff")),
                verdict: Verdict::Conflict,
                current_text: None,
                restore_text: None,
            },
        ]);
        let rows = undo_rows(&p);
        assert_eq!(rows[0].verdict, "restore");
        assert!(
            rows[0].diff.as_deref().unwrap().contains("-x = 2"),
            "{:?}",
            rows[0].diff
        );
        assert!(rows[0].diff.as_deref().unwrap().contains("+x = 1"));
        assert_eq!(rows[1].what, "remove it (it goes to the attic)");
        assert_eq!(rows[2].verdict, "conflict");
        assert!(rows[2].what.starts_with("changed after this cause"));
        assert_eq!(
            undo_summary(&p),
            "Nothing has changed yet: 2 to put back, 1 changed since (left alone)."
        );
        let mut dnf = preview(vec![]);
        dnf.delegated = Some(osreflog_undo::Delegation::Dnf {
            transaction: 53,
            description: String::new(),
        });
        assert!(undo_summary(&dnf).contains("sudo dnf5 history undo 53"));
        assert_eq!(
            undo_summary(&preview(vec![])),
            "Nothing recorded for this cause can be undone."
        );
    }

    fn form() -> BisectForm {
        BisectForm {
            good: "c000128".into(),
            bad: "c000129".into(),
            manual: false,
            tier: TierChoice::Auto,
            needs_kernel: false,
            needs_hardware: false,
            boot: false,
            test: "grep -q 'port = 80' /etc/app.conf".into(),
        }
    }

    #[test]
    fn the_form_becomes_the_daemons_arguments() {
        use osreflog_bisect::Tier;
        let a = bisect_args(&form()).unwrap();
        assert_eq!(a.tier, Tier::Nspawn);
        assert_eq!(a.test, "#!/bin/sh\ngrep -q 'port = 80' /etc/app.conf\n");
        assert!(!a.manual);
        let a = bisect_args(&BisectForm {
            needs_kernel: true,
            ..form()
        })
        .unwrap();
        assert_eq!(a.tier, Tier::Vm);
        assert!(
            bisect_args(&BisectForm {
                needs_hardware: true,
                ..form()
            })
            .is_err()
        );
        assert!(
            bisect_args(&BisectForm {
                test: " ".into(),
                ..form()
            })
            .is_err()
        );
        assert!(
            bisect_args(&BisectForm {
                tier: TierChoice::Metal,
                ..form()
            })
            .is_err()
        );
        assert!(
            bisect_args(&BisectForm {
                bad: "c000128".into(),
                ..form()
            })
            .is_err()
        );
        let a = bisect_args(&BisectForm {
            test: "#!/bin/bash\ntrue".into(),
            ..form()
        })
        .unwrap();
        assert_eq!(a.test, "#!/bin/bash\ntrue\n");

        let by_hand = BisectForm {
            manual: true,
            test: String::new(),
            needs_hardware: true,
            ..form()
        };
        let a = bisect_args(&by_hand).unwrap();
        assert_eq!((a.tier, a.manual, a.test.as_str()), (Tier::Metal, true, ""));
        assert!(
            bisect_args(&BisectForm {
                tier: TierChoice::Vm,
                ..by_hand.clone()
            })
            .is_err()
        );
        let a = bisect_args(&BisectForm {
            needs_hardware: false,
            ..by_hand
        })
        .unwrap();
        assert_eq!(a.tier, Tier::Nspawn);
    }

    #[test]
    fn a_session_reads_as_state_probes_and_finding() {
        use osreflog_bisect::{Culprit, Finding, ProbeRecord, Status, Verdict, Waiting};
        let args = bisect_args(&BisectForm {
            manual: true,
            test: String::new(),
            needs_hardware: true,
            ..form()
        })
        .unwrap();
        let mut s = Status {
            args,
            running: true,
            started_ms: 0,
            log: vec![],
            probes: vec![ProbeRecord {
                what: "checkpoint c000129 (the bad end)".into(),
                verdict: Verdict::Bad,
                ms: 1_082_800,
                peak_rss_kib: None,
                approximate: vec![],
                detail: String::new(),
                output: String::new(),
            }],
            finding: None,
            error: None,
            waiting: Some(Waiting {
                probe: 2,
                what: "after #679".into(),
                since_ms: 0,
                how: vec![],
                reboot: true,
            }),
        };
        assert_eq!(
            session_title(&s),
            "c000128 … c000129, reboots (metal), verdicts by hand"
        );
        assert_eq!(session_state(&s), "Probe 2 is armed: reboot into it");
        let rows = probe_rows(&s);
        assert_eq!(rows[0].0, "process-stop-symbolic");
        assert_eq!(rows[0].2, "bad · 1082.8 s");
        assert_eq!(finding(&s), None);

        s.waiting = None;
        s.running = false;
        s.finding = Some(Finding::Cause {
            culprit: Culprit {
                id: 680,
                kind: "command".into(),
                label: "p7d-wifi.sh start".into(),
                started_ms: 0,
                paths: vec!["/etc/NetworkManager/conf.d/90-osreflog-p7d.conf".into()],
            },
            between: ("c000128".into(), "c000129".into()),
            approximate: vec![],
        });
        assert_eq!(session_state(&s), "Found #680 p7d-wifi.sh start");
        let (title, lines, open) = finding(&s).unwrap();
        assert_eq!(title, "First bad cause: #680");
        assert_eq!(lines[1], "between c000128 (good) and c000129 (bad)");
        assert_eq!(open, Some(680));

        s.finding = Some(Finding::NotReproduced {
            detail: "you said good on the bad end".into(),
        });
        s.args.tier = osreflog_bisect::Tier::Nspawn;
        let (_, lines, _) = finding(&s).unwrap();
        assert_eq!(lines[1], "Try again with reboots (metal).");
    }

    #[test]
    fn status_says_whether_it_records() {
        let mut s = Status {
            version: "0.1.0".into(),
            root: "/".into(),
            device: "/dev/sda3".into(),
            subvolume: "root".into(),
            checkpoints: 1,
            snapshots_kept: 1,
            latest: None,
            chain_entries: 0,
            pending_diffs: 0,
            live: None,
            space: None,
        };
        assert_eq!(
            status_line(&s, |_| String::new()),
            "not recording live changes (checkpoints only) · no checkpoint yet"
        );
        s.live = Some(LiveStatus {
            running: true,
            ..LiveStatus::default()
        });
        assert!(status_line(&s, |_| String::new()).starts_with("recording changes"));
        assert_eq!(status_short(&s), "recording · 1 checkpoint");
        s.live = None;
        s.checkpoints = 140;
        assert_eq!(status_short(&s), "checkpoints only · 140 checkpoints");
    }

    #[test]
    fn diff_lines_get_their_tone() {
        assert_eq!(line_tone("@@ -1,1 +1,1 @@"), Some("dim-label"));
        assert_eq!(line_tone("-x = 1"), Some("error"));
        assert_eq!(line_tone("+ mode = broken"), Some("success"));
        assert_eq!(line_tone("--- a"), None);
        assert_eq!(line_tone("x: 1 → 2"), None);
    }

    #[test]
    fn search_matches_every_word_ignoring_case() {
        let mut c = cause(42, 0, "command", 1, 0);
        c.label = "sed -i s/x/y/ /etc/foo.conf".into();
        c.actor.comm = "sed".into();
        c.actor.command = vec!["sed".into(), "-i".into(), "/etc/foo.conf".into()];
        assert!(matches(&c, ""));
        assert!(matches(&c, "  "));
        assert!(matches(&c, "FOO.conf"));
        assert!(matches(&c, "command sed"));
        assert!(matches(&c, "#42"));
        assert!(!matches(&c, "#4 dnf"));
        assert!(!matches(&c, "bar"));
    }
}
