//! Turning attributed file events into causes: one actor, one reason, many changes.
//!
//! Classification walks the actor's process chain (nearest first):
//!
//! 1. inside a package transaction (a dnf/PackageKit pid bracketed by the dnf hook) → `dnf`
//! 2. the actor's cgroup is a system service or scope → `unit`, one per unit
//! 3. a shell is an ancestor → `command`, keyed by the top command under that shell (so
//!    `sudo sed -i ...` and everything it starts is one cause)
//! 4. in a user app scope without a shell → `app`
//! 5. anything else → `unknown`, keyed by executable
//!
//! Causes close after [`QUIET`] without changes; a `dnf` cause closes when its transaction's
//! post hook has run.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use osreflog_model::{Actor, Cause, LiveChange};

use crate::lineage::Proc;

pub const QUIET: Duration = Duration::from_secs(2);
const SHELLS: &[&str] = &[
    "bash", "zsh", "fish", "sh", "dash", "ksh", "tcsh", "csh", "nu", "xonsh", "elvish",
];
const MAX_LABEL: usize = 160;
const MAX_CHANGES_PER_CAUSE: usize = 20_000;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Key {
    Package(u32),
    Unit(String),
    Command(u32),
    App(String),
    Unknown(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Classified {
    pub key: Key,
    pub kind: &'static str,
    pub label: String,
    pub actor: Actor,
}

/// Package transactions in progress (by the package manager's pid) and finished ones whose
/// causes may now close.
#[derive(Debug, Default, Clone)]
pub struct Brackets {
    pub active: HashMap<u32, String>,
    pub ended: HashSet<u32>,
}

/// Hides secret values in a command line: `--password=x`, `--token x`, `FOO_TOKEN=x`,
/// `https://user:pass@host`.
#[must_use]
pub fn redact(args: &[String]) -> Vec<String> {
    const FLAGS: &[&str] = &[
        "password", "passwd", "pass", "token", "secret", "api-key", "apikey", "key", "auth",
    ];
    const NAMES: &[&str] = &["PASS", "TOKEN", "SECRET", "KEY", "AUTH", "CREDENTIAL"];
    let mut out = Vec::with_capacity(args.len());
    let mut hide_next = false;
    for a in args {
        if hide_next {
            out.push("***".to_owned());
            hide_next = false;
            continue;
        }
        let bare = a.trim_start_matches('-');
        let is_flag = a.starts_with('-') && bare.len() < a.len();
        if is_flag {
            let (name, value) = bare
                .split_once('=')
                .map_or((bare, None), |(n, v)| (n, Some(v)));
            if FLAGS.iter().any(|f| name.eq_ignore_ascii_case(f)) {
                if value.is_some() {
                    out.push(format!("{}=***", &a[..a.len() - bare.len() + name.len()]));
                } else {
                    out.push(a.clone());
                    hide_next = true;
                }
                continue;
            }
        }
        if let Some((name, _)) = a.split_once('=')
            && !name.is_empty()
            && !name.starts_with('-')
            && NAMES.iter().any(|n| name.to_ascii_uppercase().contains(n))
        {
            out.push(format!("{name}=***"));
            continue;
        }
        if let Some(scheme_end) = a.find("://")
            && let Some(at) = a[scheme_end + 3..].find('@')
        {
            let userinfo = &a[scheme_end + 3..scheme_end + 3 + at];
            if let Some((user, _)) = userinfo.split_once(':') {
                out.push(format!(
                    "{}{}:***{}",
                    &a[..scheme_end + 3],
                    user,
                    &a[scheme_end + 3 + at..]
                ));
                continue;
            }
        }
        out.push(a.clone());
    }
    out
}

fn label_of(args: &[String], fallback: &str) -> String {
    let joined = if args.is_empty() {
        fallback.to_owned()
    } else {
        args.join(" ")
    };
    if joined.chars().count() > MAX_LABEL {
        let cut: String = joined.chars().take(MAX_LABEL - 1).collect();
        format!("{cut}…")
    } else {
        joined
    }
}

fn actor_of(p: &Proc, command: Vec<String>) -> Actor {
    Actor {
        pid: p.pid,
        comm: p.comm.clone(),
        exe: p.exe.clone(),
        uid: p.uid,
        cgroup: p.cgroup.clone(),
        command,
        via: None,
    }
}

/// The system unit (`foo.service`, `run-r1.scope`) a cgroup path belongs to, if any.
/// Unit suffixes are case-sensitive.
#[must_use]
#[allow(clippy::case_sensitive_file_extension_comparisons)]
pub fn system_unit(cgroup: &str) -> Option<String> {
    let rest = cgroup.strip_prefix("/system.slice/")?;
    let unit = rest.split('/').next()?;
    (unit.ends_with(".service") || unit.ends_with(".scope")).then(|| unit.to_owned())
}

/// The app id of a user app scope (`app-gnome-org.gnome.TextEditor-1234.scope` →
/// `org.gnome.TextEditor`), if any.
#[must_use]
#[allow(clippy::case_sensitive_file_extension_comparisons)]
pub fn app_id(cgroup: &str) -> Option<String> {
    let scope = cgroup
        .rsplit('/')
        .find(|c| c.starts_with("app-") && (c.ends_with(".scope") || c.ends_with(".service")))?;
    let inner = scope.strip_prefix("app-")?.rsplit_once('.')?.0;
    // app-<launcher>-<app id>-<random>: drop the launcher prefix and the random suffix.
    let mut parts: Vec<&str> = inner.split('-').collect();
    if parts.len() > 2 {
        parts.remove(0);
        parts.pop();
    }
    Some(parts.join("-"))
}

/// Classifies an event by its actor's process chain (nearest first).
#[must_use]
pub fn classify(chain: &[&Proc], brackets: &Brackets) -> Classified {
    let Some(actor) = chain.first() else {
        return Classified {
            key: Key::Unknown("?".into()),
            kind: "unknown",
            label: "unknown process".into(),
            actor: Actor::default(),
        };
    };
    if let Some(pm) = chain
        .iter()
        .find(|p| brackets.active.contains_key(&p.pid) || brackets.ended.contains(&p.pid))
    {
        let command = redact(&pm.args);
        return Classified {
            key: Key::Package(pm.pid),
            kind: "dnf",
            label: label_of(&command, &pm.comm),
            actor: actor_of(pm, command),
        };
    }
    if let Some(unit) = system_unit(&actor.cgroup) {
        return Classified {
            key: Key::Unit(unit.clone()),
            kind: "unit",
            label: unit,
            actor: actor_of(actor, redact(&actor.args)),
        };
    }
    // The top command under the nearest shell: the chain element whose parent is a shell.
    if let Some(i) = chain.iter().position(|p| SHELLS.contains(&p.comm.as_str())) {
        let top = if i == 0 { chain[0] } else { chain[i - 1] };
        let command = redact(&top.args);
        return Classified {
            key: Key::Command(top.pid),
            kind: "command",
            label: label_of(&command, &top.comm),
            actor: actor_of(actor, command),
        };
    }
    if let Some(app) = app_id(&actor.cgroup) {
        return Classified {
            key: Key::App(app.clone()),
            kind: "app",
            label: app,
            actor: actor_of(actor, redact(&actor.args)),
        };
    }
    let exe = if actor.exe.is_empty() {
        actor.comm.clone()
    } else {
        actor.exe.clone()
    };
    Classified {
        key: Key::Unknown(exe.clone()),
        kind: "unknown",
        label: exe,
        actor: actor_of(actor, redact(&actor.args)),
    }
}

#[derive(Debug, Clone)]
pub struct OpenCause {
    pub key: Key,
    pub cause: Cause,
    pub changes: Vec<LiveChange>,
    last: Instant,
}

/// Open causes, keyed by [`Key`].
#[derive(Debug, Default)]
pub struct Builder {
    open: HashMap<Key, OpenCause>,
}

/// Merges `new` into the change already recorded for the same path in this cause.
fn coalesce(existing: &mut Vec<LiveChange>, new: LiveChange) {
    if let Some(i) = existing.iter().rposition(|c| c.path == new.path) {
        let old = &existing[i];
        let merged_op = match (old.op.as_str(), new.op.as_str()) {
            ("create", "delete") => {
                existing.remove(i);
                return;
            }
            ("create", _) => "create",
            (_, "delete") => "delete",
            ("rename", "modify" | "attr") => "rename",
            (_, "modify") | ("modify", _) => "modify",
            (_, op) => op,
        };
        let from = old.from.clone().or_else(|| new.from.clone());
        let blob = new.blob.clone().or_else(|| old.blob.clone());
        let size = new.size.or(old.size);
        existing[i] = LiveChange {
            op: merged_op.to_owned(),
            from,
            blob,
            size,
            ..new
        };
        return;
    }
    // A rename of something this cause created (an editor's temp file) replaces the target.
    if new.op == "rename"
        && let Some(src) = &new.from
        && let Some(i) = existing
            .iter()
            .rposition(|c| &c.path == src && c.op == "create")
    {
        existing.remove(i);
        existing.push(LiveChange {
            op: "modify".to_owned(),
            from: None,
            ..new
        });
        return;
    }
    existing.push(new);
}

impl Builder {
    /// Records a change for its cause.
    pub fn add(&mut self, c: &Classified, change: LiveChange, now: Instant) {
        let ts = change.ts_ms;
        let open = self.open.entry(c.key.clone()).or_insert_with(|| OpenCause {
            key: c.key.clone(),
            cause: Cause {
                id: 0,
                kind: c.kind.to_owned(),
                label: c.label.clone(),
                actor: c.actor.clone(),
                started_ms: ts,
                ended_ms: ts,
                changes: 0,
                package_paths: 0,
            },
            changes: Vec::new(),
            last: now,
        });
        open.last = now;
        open.cause.ended_ms = open.cause.ended_ms.max(ts);
        if open.changes.len() < MAX_CHANGES_PER_CAUSE {
            coalesce(&mut open.changes, change);
        }
    }

    /// Counts a package-owned path written inside a transaction, without listing it.
    pub fn count_package_path(&mut self, c: &Classified, ts_ms: i64, now: Instant) {
        let open = self.open.entry(c.key.clone()).or_insert_with(|| OpenCause {
            key: c.key.clone(),
            cause: Cause {
                id: 0,
                kind: c.kind.to_owned(),
                label: c.label.clone(),
                actor: c.actor.clone(),
                started_ms: ts_ms,
                ended_ms: ts_ms,
                changes: 0,
                package_paths: 0,
            },
            changes: Vec::new(),
            last: now,
        });
        open.last = now;
        open.cause.ended_ms = open.cause.ended_ms.max(ts_ms);
        open.cause.package_paths += 1;
    }

    /// Takes the causes ready to store: quiet for [`QUIET`], or (for `dnf`) whose
    /// transaction has ended and that have been quiet briefly.
    pub fn take_closed(&mut self, now: Instant, brackets: &Brackets) -> Vec<OpenCause> {
        let ready: Vec<Key> = self
            .open
            .iter()
            .filter(|(k, c)| {
                let quiet = now.saturating_duration_since(c.last);
                match k {
                    Key::Package(pid) => {
                        brackets.ended.contains(pid) && quiet >= Duration::from_millis(300)
                    }
                    _ => quiet >= QUIET,
                }
            })
            .map(|(k, _)| k.clone())
            .collect();
        ready
            .into_iter()
            .filter_map(|k| self.open.remove(&k))
            .collect()
    }

    /// Takes every open cause (shutdown).
    pub fn take_all(&mut self) -> Vec<OpenCause> {
        self.open.drain().map(|(_, c)| c).collect()
    }

    /// The start of the oldest open cause: everything before it has been stored.
    #[must_use]
    pub fn oldest_open_ms(&self) -> Option<i64> {
        self.open.values().map(|c| c.cause.started_ms).min()
    }

    /// Whether a package transaction's cause is still open.
    #[must_use]
    pub fn has_package(&self, pid: u32) -> bool {
        self.open.contains_key(&Key::Package(pid))
    }

    #[must_use]
    pub fn open_count(&self) -> usize {
        self.open.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(pid: u32, parent: u32, comm: &str, args: &[&str], cgroup: &str) -> Proc {
        Proc {
            pid,
            ppid: parent,
            comm: comm.into(),
            exe: format!("/usr/bin/{comm}"),
            args: args.iter().map(|s| (*s).to_owned()).collect(),
            uid: 0,
            cgroup: cgroup.into(),
            cgroup_id: 0,
            exited: None,
            seen_ms: 0,
        }
    }

    fn change(path: &str, op: &str, ts: i64) -> LiveChange {
        LiveChange {
            ts_ms: ts,
            path: path.into(),
            op: op.into(),
            from: None,
            kind: "file".into(),
            blob: None,
            size: None,
            actor: None,
        }
    }

    const SESSION: &str = "/user.slice/user-1000.slice/session-3.scope";

    #[test]
    fn sudo_sed_in_a_terminal_is_one_command() {
        let shell = p(100, 1, "bash", &["bash"], SESSION);
        let sudo = p(
            200,
            100,
            "sudo",
            &["sudo", "sed", "-i", "s/a/b/", "/etc/hosts"],
            SESSION,
        );
        let sed = p(
            201,
            200,
            "sed",
            &["sed", "-i", "s/a/b/", "/etc/hosts"],
            SESSION,
        );
        let c = classify(&[&sed, &sudo, &shell], &Brackets::default());
        assert_eq!(c.kind, "command");
        assert_eq!(c.key, Key::Command(200));
        assert_eq!(c.label, "sudo sed -i s/a/b/ /etc/hosts");
        assert_eq!(c.actor.comm, "sed");
    }

    #[test]
    fn services_and_scopes_are_units() {
        let tlp = p(
            50,
            1,
            "tlp",
            &["/usr/sbin/tlp", "start"],
            "/system.slice/tlp.service",
        );
        assert_eq!(
            classify(&[&tlp], &Brackets::default()).key,
            Key::Unit("tlp.service".into())
        );
        assert_eq!(
            system_unit("/system.slice/run-r9f.scope").as_deref(),
            Some("run-r9f.scope")
        );
        assert_eq!(
            system_unit("/user.slice/user-1000.slice/session-3.scope"),
            None
        );
    }

    #[test]
    fn package_bracket_wins() {
        let dnf = p(
            300,
            100,
            "dnf",
            &["dnf", "install", "-y", "figlet"],
            SESSION,
        );
        let rpm_script = p(301, 300, "sh", &["/bin/sh", "-c", "ldconfig"], SESSION);
        let ldconfig = p(302, 301, "ldconfig", &["ldconfig"], SESSION);
        let mut b = Brackets::default();
        b.active.insert(300, "pre".into());
        let c = classify(&[&ldconfig, &rpm_script, &dnf], &b);
        assert_eq!((c.kind, c.key.clone()), ("dnf", Key::Package(300)));
        assert_eq!(c.label, "dnf install -y figlet");
    }

    #[test]
    fn apps_and_unknowns() {
        let editor = p(
            400,
            1,
            "gnome-text-edit",
            &["gnome-text-editor"],
            "/user.slice/user-1000.slice/user@1000.service/app.slice/app-gnome-org.gnome.TextEditor-4242.scope",
        );
        let c = classify(&[&editor], &Brackets::default());
        assert_eq!((c.kind, c.label.as_str()), ("app", "org.gnome.TextEditor"));
        let orphan = p(500, 1, "mystery", &[], "/");
        assert_eq!(classify(&[&orphan], &Brackets::default()).kind, "unknown");
        assert_eq!(classify(&[], &Brackets::default()).kind, "unknown");
    }

    #[test]
    fn redaction() {
        let r = |a: &[&str]| redact(&a.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>());
        assert_eq!(
            r(&["nmcli", "--password", "hunter2", "x"]),
            ["nmcli", "--password", "***", "x"]
        );
        assert_eq!(r(&["tool", "--token=abc"]), ["tool", "--token=***"]);
        assert_eq!(
            r(&["env", "GITHUB_TOKEN=abc", "PATH=/bin"]),
            ["env", "GITHUB_TOKEN=***", "PATH=/bin"]
        );
        assert_eq!(
            r(&["curl", "https://me:pw@example.com/x"]),
            ["curl", "https://me:***@example.com/x"]
        );
        assert_eq!(
            r(&["sed", "-i", "s/a/b/", "/etc/hosts"]),
            ["sed", "-i", "s/a/b/", "/etc/hosts"]
        );
    }

    #[test]
    fn coalescing_and_editor_saves() {
        let mut v = Vec::new();
        coalesce(&mut v, change("/etc/a", "create", 1));
        coalesce(&mut v, change("/etc/a", "modify", 2));
        assert_eq!(v[0].op, "create");
        coalesce(&mut v, change("/etc/a", "delete", 3));
        assert!(
            v.is_empty(),
            "created and deleted in one cause leaves nothing"
        );

        // vim: write .hosts.swp-like temp, rename over the target.
        coalesce(&mut v, change("/etc/.hosts.tmp", "create", 1));
        coalesce(&mut v, change("/etc/.hosts.tmp", "modify", 2));
        let mut rename = change("/etc/hosts", "rename", 3);
        rename.from = Some("/etc/.hosts.tmp".into());
        coalesce(&mut v, rename);
        assert_eq!(v.len(), 1);
        assert_eq!(
            (v[0].path.as_str(), v[0].op.as_str(), v[0].from.as_deref()),
            ("/etc/hosts", "modify", None)
        );
    }

    #[test]
    fn causes_close_after_quiet_and_dnf_after_its_bracket() {
        let shell = p(100, 1, "bash", &["bash"], SESSION);
        let touch = p(200, 100, "touch", &["touch", "/etc/x"], SESSION);
        let dnf = p(300, 100, "dnf", &["dnf", "remove", "figlet"], SESSION);
        let mut b = Brackets::default();
        b.active.insert(300, String::new());
        let mut builder = Builder::default();
        let t0 = Instant::now();
        builder.add(
            &classify(&[&touch, &shell], &b),
            change("/etc/x", "create", 10),
            t0,
        );
        builder.add(
            &classify(&[&dnf], &b),
            change("/etc/ld.so.cache", "modify", 11),
            t0,
        );
        builder.count_package_path(&classify(&[&dnf], &b), 12, t0);
        assert_eq!(builder.oldest_open_ms(), Some(10));
        assert!(
            builder
                .take_closed(t0 + Duration::from_secs(1), &b)
                .is_empty()
        );
        let closed = builder.take_closed(t0 + QUIET, &b);
        assert_eq!(
            closed.len(),
            1,
            "the command closes, the transaction is still open"
        );
        assert_eq!(closed[0].cause.kind, "command");

        b.active.remove(&300);
        b.ended.insert(300);
        let closed = builder.take_closed(t0 + QUIET, &b);
        assert_eq!((closed.len(), closed[0].cause.package_paths), (1, 1));
        assert_eq!(builder.open_count(), 0);
    }
}
