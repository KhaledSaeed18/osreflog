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
/// At most this many causes open at once; changes for new ones beyond it merge into one
/// `flood` cause per uid (threat model A3).
const MAX_OPEN: usize = 256;
/// At most this many changes held across all open causes. Past it, changes are counted out:
/// the state witness still records them, without a cause.
const MAX_HELD: usize = 100_000;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Key {
    Undo(u32),
    /// Changes from more concurrent causes than the builder keeps open, by uid.
    Flood(u32),
    Package(u32),
    Unit(String),
    Command(u32),
    App(String),
    Unknown(String),
    /// Writes in Flatpak's system installation, counted and not listed (R4).
    Flatpak,
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
    /// Running undo workers: pid → the cause they revert.
    pub undo: HashMap<u32, i64>,
}

pub use osreflog_model::redact::args as redact;

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
    // Past any sub-slices: templated units sit in one of their own (`system-getty.slice/
    // getty@tty1.service`), and so do some plain ones (`system-cups.slice/cups.service`).
    let unit = rest.split('/').find(|c| !c.ends_with(".slice"))?;
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
    if let Some((pid, of)) = chain
        .iter()
        .find_map(|p| brackets.undo.get(&p.pid).map(|of| (p.pid, *of)))
    {
        return Classified {
            key: Key::Undo(pid),
            kind: "undo",
            label: format!("undo of #{of}"),
            actor: actor_of(actor, redact(&actor.args)),
        };
    }
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

/// The one cause for writes under [`osreflog_model::FLATPAK`], whoever made them: Flatpak
/// has its own history (`flatpak history`) and rollback, so osreflog only says it happened.
#[must_use]
pub fn flatpak(c: Classified) -> Classified {
    Classified {
        key: Key::Flatpak,
        kind: "flatpak",
        label: format!("flatpak ({})", c.actor.comm),
        actor: c.actor,
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
    /// Changes held across all open causes.
    held: usize,
    /// Changes not kept because of [`MAX_HELD`] or [`MAX_CHANGES_PER_CAUSE`].
    pub dropped: u64,
}

/// Merges `new` into the change already recorded for the same path in this cause.
fn coalesce(existing: &mut Vec<LiveChange>, mut new: LiveChange) {
    // A rename moves an object away from its old name: whatever this cause recorded there is
    // superseded. If it created the source (a temp file such as `/etc/passwd+` or an
    // editor's `.foo.swp`), the rename *replaces* the target rather than moving a file.
    let mut replaces = false;
    if new.op == "rename"
        && let Some(src) = new.from.clone()
        && let Some(i) = existing.iter().rposition(|c| c.path == src)
    {
        replaces = existing[i].op == "create";
        if replaces {
            // Moving an earlier rename further keeps its original name; a created temp has none.
            new.from = None;
        } else {
            new.from = existing[i].from.clone().or(new.from);
        }
        existing.remove(i);
    }
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
            // A rename onto a path this cause already changed writes over it (temp file).
            (_, "rename" | "modify") | ("modify", _) => "modify",
            (_, op) => op,
        };
        // A replace has no old name; a moved file that is then modified keeps its old name.
        let from = if new.op == "rename" {
            None
        } else {
            old.from.clone()
        };
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
    if replaces {
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
        let flood;
        let c = if self.open.len() >= MAX_OPEN && !self.open.contains_key(&c.key) {
            let uid = c.actor.uid;
            flood = Classified {
                key: Key::Flood(uid),
                kind: "flood",
                label: format!("more than {MAX_OPEN} causes at once (uid {uid}), merged"),
                actor: c.actor.clone(),
            };
            &flood
        } else {
            c
        };
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
        if open.changes.len() < MAX_CHANGES_PER_CAUSE && self.held < MAX_HELD {
            let before = open.changes.len();
            coalesce(&mut open.changes, change);
            // Coalescing may also shrink the list (a path created and deleted again).
            self.held = (self.held + open.changes.len()).saturating_sub(before);
        } else {
            self.dropped += 1;
        }
    }

    /// Counts a path without listing it: a package-owned path written inside a transaction,
    /// or a write in Flatpak's installation.
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
        let closed: Vec<OpenCause> = ready
            .into_iter()
            .filter_map(|k| self.open.remove(&k))
            .collect();
        self.recount();
        closed
    }

    /// Takes every open cause (shutdown).
    pub fn take_all(&mut self) -> Vec<OpenCause> {
        self.held = 0;
        self.open.drain().map(|(_, c)| c).collect()
    }

    fn recount(&mut self) {
        self.held = self.open.values().map(|c| c.changes.len()).sum();
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
            target: None,
            summary: Vec::new(),
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
        // Units in sub-slices were "unknown" causes until 0.3.0 (cupsd, every template).
        for (cgroup, unit) in [
            (
                "/system.slice/system-cups.slice/cups.service",
                "cups.service",
            ),
            (
                "/system.slice/system-getty.slice/getty@tty1.service",
                "getty@tty1.service",
            ),
            (
                "/system.slice/a.slice/b.slice/x.service/payload",
                "x.service",
            ),
        ] {
            assert_eq!(system_unit(cgroup).as_deref(), Some(unit), "{cgroup}");
        }
        assert_eq!(system_unit("/system.slice/system-cups.slice"), None);
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
    fn a_flood_of_causes_merges_and_stays_bounded() {
        let mut b = Builder::default();
        let now = Instant::now();
        for pid in 0..(u32::try_from(MAX_OPEN).unwrap() + 500) {
            let proc = p(10_000 + pid, 1, "touch", &["touch"], SESSION);
            let c = Classified {
                key: Key::Command(10_000 + pid),
                kind: "command",
                label: format!("touch {pid}"),
                actor: actor_of(&proc, Vec::new()),
            };
            b.add(
                &c,
                change(&format!("/var/x/{pid}"), "create", i64::from(pid)),
                now,
            );
        }
        assert_eq!(
            b.open_count(),
            MAX_OPEN + 1,
            "the rest merged into one flood cause"
        );
        let flood = b.open.get(&Key::Flood(0)).unwrap();
        assert_eq!(
            (flood.cause.kind.as_str(), flood.changes.len()),
            ("flood", 500)
        );
        // The global budget: past it, changes are counted, not held.
        let proc = p(9, 1, "touch", &["touch"], SESSION);
        let c = Classified {
            key: Key::Command(9),
            kind: "command",
            label: "touch".into(),
            actor: actor_of(&proc, Vec::new()),
        };
        b.held = MAX_HELD;
        b.add(&c, change("/var/x/over", "create", 1), now);
        assert_eq!(b.dropped, 1);
        let closed = b.take_closed(now + Duration::from_secs(10), &Brackets::default());
        assert_eq!(closed.len(), MAX_OPEN + 1);
        assert_eq!(b.held, 0);
    }

    #[test]
    fn an_undo_workers_writes_are_an_undo_cause() {
        let daemon = p(
            10,
            1,
            "osreflogd",
            &["osreflogd"],
            "/system.slice/osreflogd.service",
        );
        let worker = p(
            11,
            10,
            "osreflogd",
            &["osreflogd", "--undo-worker", "/run/osreflog/top/root"],
            "/system.slice/osreflogd.service",
        );
        let mut b = Brackets::default();
        assert_eq!(classify(&[&worker, &daemon], &b).kind, "unit");
        b.undo.insert(11, 42);
        let c = classify(&[&worker, &daemon], &b);
        assert_eq!((c.kind, c.key.clone()), ("undo", Key::Undo(11)));
        assert_eq!(c.label, "undo of #42");
    }

    #[test]
    fn flatpak_writes_are_one_cause_whoever_made_them() {
        let helper = p(
            700,
            1,
            "flatpak-system-",
            &["/usr/libexec/flatpak-system-helper"],
            "/system.slice/flatpak-system-helper.service",
        );
        let shell = p(
            800,
            1,
            "bash",
            &["bash"],
            "/user.slice/user-1000.slice/session-2.scope",
        );
        let cli = p(801, 800, "flatpak", &["flatpak", "update"], &shell.cgroup);
        let a = flatpak(classify(&[&helper], &Brackets::default()));
        let b = flatpak(classify(&[&cli, &shell], &Brackets::default()));
        assert_eq!((a.kind, &a.key), ("flatpak", &Key::Flatpak));
        assert_eq!(a.key, b.key);
        assert_eq!(a.label, "flatpak (flatpak-system-)");
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
    fn useradd_style_replace_keeps_no_temp_and_no_old_name() {
        // shadow-utils: touch /etc/passwd (backup link), write /etc/passwd+, rename it over.
        let mut v = Vec::new();
        coalesce(&mut v, change("/etc/passwd", "attr", 1));
        coalesce(&mut v, change("/etc/passwd+", "create", 2));
        coalesce(&mut v, change("/etc/passwd+", "modify", 3));
        let mut rename = change("/etc/passwd", "rename", 4);
        rename.from = Some("/etc/passwd+".into());
        coalesce(&mut v, rename);
        assert_eq!(v.len(), 1, "{v:?}");
        assert_eq!(
            (v[0].path.as_str(), v[0].op.as_str(), v[0].from.as_deref()),
            ("/etc/passwd", "modify", None)
        );
    }

    #[test]
    fn a_real_move_keeps_its_old_name() {
        let mut v = Vec::new();
        let mut mv = change("/etc/b.conf", "rename", 1);
        mv.from = Some("/etc/a.conf".into());
        coalesce(&mut v, mv);
        coalesce(&mut v, change("/etc/b.conf", "modify", 2));
        assert_eq!(
            (v[0].op.as_str(), v[0].from.as_deref()),
            ("rename", Some("/etc/a.conf"))
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
