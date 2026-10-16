//! What the window shows, as plain data: causes grouped by day, a cause's lines, the status
//! line. No GTK here, so it is tested on its own; the window only lays it out.

use std::fmt::Write as _;

use osreflog_model::{Cause, CauseDetail, LiveChange, Status};

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
        let _ = write!(
            s,
            " · {}",
            plural(c.package_paths, "package file", "package files")
        );
    }
    s
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
    if c.package_paths > 0 {
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
    match &s.latest {
        Some(c) => format!(
            "{live} · {} · last checkpoint {} ({})",
            plural(s.checkpoints, "checkpoint", "checkpoints"),
            local(c.created_ms),
            c.reason
        ),
        None => format!("{live} · no checkpoint yet"),
    }
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
    }
}
