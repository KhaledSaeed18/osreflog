//! The pure half of the user agent: reading `dconf dump` output, turning a `Notify` into
//! key paths, comparing values, naming the app behind a process, and grouping changes into
//! causes. Everything here is unit-tested without a session bus.

use std::collections::BTreeMap;

/// Values longer than this are cut when summarized.
const MAX_VALUE: usize = 100;
/// A cause closes after this long without a change from the same process.
pub const QUIET_MS: i64 = 2_000;

/// Key names whose values are never shown (the same rule as the system summaries).
const SECRET_WORDS: &[&str] = &[
    "psk", "password", "passwd", "secret", "token", "private", "apikey", "api-key", "api_key",
];

/// Key path to value text (`GVariant` text format, as `dconf read` prints it).
pub type Values = BTreeMap<String, String>;

/// Parses `dconf dump <dir>` output into full key paths. `dir` is the dumped directory,
/// ending in `/`; sections are relative to it (`[/]` is the directory itself).
#[must_use]
pub fn parse_dump(dir: &str, text: &str) -> Values {
    let mut out = Values::new();
    let mut section: Option<String> = None;
    for line in text.lines() {
        if line.is_empty() {
            continue;
        }
        if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            section = Some(if name == "/" {
                dir.to_owned()
            } else {
                format!("{dir}{}/", name.trim_matches('/'))
            });
        } else if let (Some(s), Some((key, value))) = (&section, line.split_once('=')) {
            out.insert(format!("{s}{key}"), value.to_owned());
        }
    }
    out
}

/// The paths a `Notify(prefix, keys)` names: `prefix` + each key, or `prefix` alone. A path
/// ending in `/` is a whole directory (a reset or a bulk write).
#[must_use]
pub fn notified_paths(prefix: &str, keys: &[String]) -> Vec<String> {
    if keys.is_empty() {
        return vec![prefix.to_owned()];
    }
    keys.iter().map(|k| format!("{prefix}{k}")).collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValueChange {
    pub key: String,
    pub old: Option<String>,
    pub new: Option<String>,
}

/// What differs between the cached values and the current ones under `path` (a key, or a
/// directory ending in `/`). Updates `cache` to the current values.
pub fn apply(cache: &mut Values, path: &str, current: &Values) -> Vec<ValueChange> {
    let in_scope = |k: &str| {
        if path.ends_with('/') {
            k.starts_with(path)
        } else {
            k == path
        }
    };
    let mut keys: Vec<String> = cache.keys().filter(|k| in_scope(k)).cloned().collect();
    keys.extend(current.keys().filter(|k| in_scope(k)).cloned());
    keys.sort();
    keys.dedup();
    let mut out = Vec::new();
    for key in keys {
        let old = cache.get(&key).cloned();
        let new = current.get(&key).cloned();
        if old == new {
            continue;
        }
        match &new {
            Some(v) => cache.insert(key.clone(), v.clone()),
            None => cache.remove(&key),
        };
        out.push(ValueChange { key, old, new });
    }
    out
}

/// Whether a key's name marks its value as secret: never shown, never stored.
#[must_use]
pub fn is_secret_key(key: &str) -> bool {
    let name = key.rsplit('/').next().unwrap_or(key).to_ascii_lowercase();
    SECRET_WORDS.iter().any(|w| name.contains(w))
}

fn shown(key: &str, value: &str) -> String {
    if is_secret_key(key) {
        return "***".to_owned();
    }
    let one: String = value
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    if one.chars().count() > MAX_VALUE {
        format!("{}…", one.chars().take(MAX_VALUE - 1).collect::<String>())
    } else {
        one
    }
}

impl ValueChange {
    /// `create`, `modify` or `delete`, as for files.
    #[must_use]
    pub fn op(&self) -> &'static str {
        match (&self.old, &self.new) {
            (None, _) => "create",
            (_, None) => "delete",
            _ => "modify",
        }
    }

    #[must_use]
    pub fn summary(&self) -> String {
        match (&self.old, &self.new) {
            (Some(o), Some(n)) => format!("{} → {}", shown(&self.key, o), shown(&self.key, n)),
            (None, Some(n)) => format!("set to {}", shown(&self.key, n)),
            (Some(o), None) => format!("reset (was {})", shown(&self.key, o)),
            (None, None) => "unchanged".to_owned(),
        }
    }
}

/// The application a process belongs to, from its cgroup: GNOME and systemd start apps in
/// `app-[<launcher>-]<app id>-<random>.scope` or `app-[<launcher>-]<app id>[@<random>].service`.
#[must_use]
pub fn app_of_cgroup(cgroup: &str) -> Option<String> {
    let unit = cgroup.rsplit('/').next()?;
    let name = unit
        .strip_suffix(".scope")
        .or_else(|| unit.strip_suffix(".service"))?
        .strip_prefix("app-")?;
    let name = name.split('@').next().unwrap_or(name);
    let name = match name.rsplit_once('-') {
        Some((head, tail)) if !tail.is_empty() && tail.bytes().all(|b| b.is_ascii_digit()) => head,
        _ => name,
    };
    let name = ["gnome-", "flatpak-", "kde-", "dbus-:1.", "dbus-"]
        .iter()
        .find_map(|p| name.strip_prefix(p))
        .unwrap_or(name);
    let name = name.replace("\\x2d", "-");
    (!name.is_empty()).then_some(name)
}

/// Value changes grouped by the process that asked, closed after [`QUIET_MS`].
#[derive(Debug)]
pub struct Groups<A> {
    open: Vec<Group<A>>,
}

impl<A> Default for Groups<A> {
    fn default() -> Self {
        Self { open: Vec::new() }
    }
}

#[derive(Debug)]
pub struct Group<A> {
    pub pid: u32,
    pub actor: A,
    pub started_ms: i64,
    pub last_ms: i64,
    pub changes: Vec<(i64, ValueChange)>,
}

impl<A> Groups<A> {
    pub fn add(&mut self, pid: u32, actor: A, ts_ms: i64, changes: Vec<ValueChange>) {
        if changes.is_empty() {
            return;
        }
        if let Some(g) = self.open.iter_mut().find(|g| g.pid == pid) {
            g.last_ms = ts_ms;
            for c in changes {
                // The latest value of a key wins; its first old value is kept.
                if let Some((t, prev)) = g.changes.iter_mut().find(|(_, p)| p.key == c.key) {
                    *t = ts_ms;
                    prev.new = c.new;
                } else {
                    g.changes.push((ts_ms, c));
                }
            }
            return;
        }
        self.open.push(Group {
            pid,
            actor,
            started_ms: ts_ms,
            last_ms: ts_ms,
            changes: changes.into_iter().map(|c| (ts_ms, c)).collect(),
        });
    }

    /// Groups quiet since before `now_ms - QUIET_MS`, with changes that cancelled out dropped.
    pub fn close(&mut self, now_ms: i64) -> Vec<Group<A>> {
        let (done, open): (Vec<_>, Vec<_>) = std::mem::take(&mut self.open)
            .into_iter()
            .partition(|g| now_ms - g.last_ms >= QUIET_MS);
        self.open = open;
        done.into_iter()
            .map(|mut g| {
                g.changes.retain(|(_, c)| c.old != c.new);
                g
            })
            .filter(|g| !g.changes.is_empty())
            .collect()
    }

    #[cfg(test)]
    fn is_empty(&self) -> bool {
        self.open.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DUMP: &str = "[/]\nroot-key=1\n\n[org/gnome/desktop/interface]\ncolor-scheme='prefer-dark'\nclock-show-seconds=true\n\n[org/gnome/Ptyxis]\nwindow-size=(uint32 150, uint32 30)\n";

    #[test]
    fn dump_keys_are_full_paths() {
        let v = parse_dump("/", DUMP);
        assert_eq!(v["/root-key"], "1");
        assert_eq!(
            v["/org/gnome/desktop/interface/color-scheme"],
            "'prefer-dark'"
        );
        assert_eq!(
            v["/org/gnome/Ptyxis/window-size"],
            "(uint32 150, uint32 30)"
        );
        let sub = parse_dump("/org/gnome/", "[desktop/interface]\na=1\n[/]\nb=2\n");
        assert_eq!(sub["/org/gnome/desktop/interface/a"], "1");
        assert_eq!(sub["/org/gnome/b"], "2");
    }

    #[test]
    fn notify_paths() {
        assert_eq!(notified_paths("/a/b", &[String::new()]), ["/a/b"]);
        assert_eq!(
            notified_paths("/a/", &["x".into(), "y/z".into()]),
            ["/a/x", "/a/y/z"]
        );
        assert_eq!(notified_paths("/a/", &[]), ["/a/"]);
    }

    #[test]
    fn key_and_directory_changes() {
        let mut cache = parse_dump("/", DUMP);
        let key = "/org/gnome/desktop/interface/color-scheme";
        let now = Values::from([(key.to_owned(), "'default'".to_owned())]);
        let c = apply(&mut cache, key, &now);
        assert_eq!(c.len(), 1);
        assert_eq!(
            (c[0].op(), c[0].summary().as_str()),
            ("modify", "'prefer-dark' → 'default'")
        );
        assert!(
            apply(&mut cache, key, &now).is_empty(),
            "rewritten with the same value"
        );
        // A directory reset removes every key under it.
        let c = apply(&mut cache, "/org/gnome/desktop/interface/", &Values::new());
        assert_eq!(c.len(), 2);
        assert!(c.iter().all(|c| c.op() == "delete"));
        assert_eq!(c[0].summary(), "reset (was true)");
        assert!(
            cache
                .keys()
                .all(|k| !k.starts_with("/org/gnome/desktop/interface/"))
        );
        let c = apply(
            &mut cache,
            "/x/new",
            &Values::from([("/x/new".into(), "'v'".into())]),
        );
        assert_eq!(
            (c[0].op(), c[0].summary().as_str()),
            ("create", "set to 'v'")
        );
    }

    #[test]
    fn secrets_and_long_values_are_not_shown() {
        let c = ValueChange {
            key: "/org/app/api-key".into(),
            old: None,
            new: Some("'abc'".into()),
        };
        assert_eq!(c.summary(), "set to ***");
        let c = ValueChange {
            key: "/org/app/list".into(),
            old: Some("x".repeat(300)),
            new: Some("'a\nb'".into()),
        };
        let s = c.summary();
        assert!(s.chars().count() < 120 && s.ends_with("'a b'"), "{s}");
    }

    #[test]
    fn apps_from_cgroups() {
        let app = |c: &str| app_of_cgroup(c);
        let base = "/user.slice/user-1000.slice/user@1000.service/app.slice/";
        assert_eq!(
            app(&format!("{base}app-com.anthropic.Claude-1090428.scope")).as_deref(),
            Some("com.anthropic.Claude")
        );
        assert_eq!(
            app(&format!("{base}app-gnome-org.gnome.Settings-4242.scope")).as_deref(),
            Some("org.gnome.Settings")
        );
        assert_eq!(
            app(&format!("{base}app-flatpak-org.mozilla.firefox-77.scope")).as_deref(),
            Some("org.mozilla.firefox")
        );
        assert_eq!(
            app(&format!("{base}app-org.gnome.Ptyxis@abc.service")).as_deref(),
            Some("org.gnome.Ptyxis")
        );
        assert_eq!(
            app(&format!("{base}app-gnome-my\\x2dapp-12.scope")).as_deref(),
            Some("my-app")
        );
        assert_eq!(app("/user.slice/user-1000.slice/session-2.scope"), None);
    }

    #[test]
    fn groups_coalesce_per_process_and_drop_round_trips() {
        let mut g: Groups<&str> = Groups::default();
        let ch = |key: &str, old: Option<&str>, new: Option<&str>| ValueChange {
            key: key.into(),
            old: old.map(Into::into),
            new: new.map(Into::into),
        };
        g.add(10, "settings", 1_000, vec![ch("/a", Some("1"), Some("2"))]);
        g.add(
            10,
            "settings",
            1_500,
            vec![ch("/a", Some("2"), Some("3")), ch("/b", None, Some("x"))],
        );
        g.add(11, "gsettings", 1_600, vec![ch("/c", Some("1"), Some("2"))]);
        g.add(11, "gsettings", 1_700, vec![ch("/c", Some("2"), Some("1"))]);
        assert!(g.close(2_000).is_empty(), "still open");
        let done = g.close(3_800);
        assert_eq!(done.len(), 1, "pid 11 changed /c and back: nothing left");
        assert_eq!(done[0].changes.len(), 2);
        assert_eq!(done[0].changes[0].1, ch("/a", Some("1"), Some("3")));
        assert!(g.is_empty());
    }
}
