//! What a config change means. Given a path, the operation, and the content (or symlink
//! target) before and after, produce a few readable lines such as
//! `[connection] wifi.powersave: 2 → 3`, `enabled foo.service (wanted by multi-user.target)`,
//! `user osreflogwl added (uid 976, shell /sbin/nologin)` or `rewritten, content unchanged`.
//!
//! This code parses content any local user may influence, so osreflogd runs it in a
//! sandboxed worker process (no filesystem, a seccomp allowlist, no capabilities, memory and
//! CPU limits). It must never panic on any input, and its output is bounded.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use serde::{Deserialize, Serialize};

/// At most this many lines per change.
pub const MAX_LINES: usize = 12;
/// At most this many characters per line.
pub const MAX_LINE: usize = 200;
/// Inputs larger than this are summarized by size only.
pub const MAX_INPUT: usize = 1 << 20;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Item {
    pub path: String,
    /// `create`, `modify`, `delete`, `rename` or `attr`.
    pub op: String,
    pub before: Option<String>,
    pub after: Option<String>,
    pub before_target: Option<String>,
    pub after_target: Option<String>,
}

/// Value names whose values are never shown.
const SECRET_KEYS: &[&str] = &[
    "psk",
    "password",
    "passwd",
    "secret",
    "token",
    "private-key",
    "private_key",
    "apikey",
    "api_key",
    "api-key",
];

fn redact(key: &str, value: &str) -> String {
    let k = key.to_ascii_lowercase();
    if SECRET_KEYS.iter().any(|s| k.contains(s)) {
        "***".to_owned()
    } else {
        value.to_owned()
    }
}

fn clip(s: &str) -> String {
    let one: String = s
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    if one.chars().count() > MAX_LINE {
        let cut: String = one.chars().take(MAX_LINE - 1).collect();
        format!("{cut}…")
    } else {
        one
    }
}

fn finish(mut lines: Vec<String>) -> Vec<String> {
    let total = lines.len();
    lines.truncate(MAX_LINES);
    if total > MAX_LINES {
        lines.pop();
        lines.push(format!("… and {} more", total - (MAX_LINES - 1)));
    }
    lines.into_iter().map(|l| clip(&l)).collect()
}

/// Summarizes one change. Never panics; at most [`MAX_LINES`] lines of at most [`MAX_LINE`]
/// characters.
#[must_use]
pub fn summarize(item: &Item) -> Vec<String> {
    if let Some(lines) = by_path(item) {
        return finish(lines);
    }
    let (before, after) = (item.before.as_deref(), item.after.as_deref());
    if before.is_some_and(|b| b.len() > MAX_INPUT) || after.is_some_and(|a| a.len() > MAX_INPUT) {
        return finish(vec!["content too large to summarize".into()]);
    }
    let lines = match (item.op.as_str(), before, after) {
        (_, Some(b), Some(a)) if b == a => vec!["rewritten, content unchanged".into()],
        ("delete", _, _) => vec![match before {
            Some(b) => format!("removed ({} lines)", b.lines().count()),
            None => "removed".into(),
        }],
        (_, None, Some(a)) => {
            let mut v = vec![format!("new file ({} lines)", a.lines().count())];
            v.extend(
                content_diff(&item.path, "", a)
                    .into_iter()
                    .take(MAX_LINES - 1),
            );
            v
        }
        (_, Some(b), Some(a)) => content_diff(&item.path, b, a),
        _ => Vec::new(),
    };
    finish(lines)
}

/// Meaning from the path and symlink targets alone.
fn by_path(item: &Item) -> Option<Vec<String>> {
    let p = item.path.as_str();
    // Unit enablement: /etc/systemd/{system,user}/<target>.<wants|requires|upholds>/<unit>
    if let Some(rest) = p
        .strip_prefix("/etc/systemd/system/")
        .or_else(|| p.strip_prefix("/etc/systemd/user/"))
        && let Some((dir, unit)) = rest.split_once('/')
        && !unit.contains('/')
    {
        for (suffix, verb) in [
            (".wants", "wanted"),
            (".requires", "required"),
            (".upholds", "upheld"),
        ] {
            if let Some(target) = dir.strip_suffix(suffix) {
                return Some(vec![match item.op.as_str() {
                    "delete" => format!("disabled {unit} (no longer {verb} by {target})"),
                    _ => format!("enabled {unit} ({verb} by {target})"),
                }]);
            }
        }
    }
    if p.starts_with("/etc/systemd/")
        && (item.after_target.as_deref() == Some("/dev/null")
            || item.before_target.as_deref() == Some("/dev/null"))
    {
        let unit = p.rsplit('/').next().unwrap_or(p);
        return Some(vec![if item.after_target.as_deref() == Some("/dev/null") {
            format!("masked {unit}")
        } else {
            format!("unmasked {unit}")
        }]);
    }
    if p == "/etc/localtime" && (item.before_target.is_some() || item.after_target.is_some()) {
        let zone = |t: &Option<String>| {
            t.as_deref().map_or_else(
                || "?".to_owned(),
                |t| t.split("zoneinfo/").nth(1).unwrap_or(t).to_owned(),
            )
        };
        let (b, a) = (zone(&item.before_target), zone(&item.after_target));
        return Some(vec![if b == a {
            format!("time zone {a} (link rewritten)")
        } else {
            format!("time zone: {b} → {a}")
        }]);
    }
    if let Some(name) = p.strip_prefix("/etc/alternatives/")
        && item.after_target.is_some()
    {
        return Some(vec![format!(
            "alternative {name} → {}",
            item.after_target.as_deref().unwrap_or("?")
        )]);
    }
    match (&item.before_target, &item.after_target) {
        (Some(b), Some(a)) if b == a => Some(vec![format!("link → {a} (rewritten, unchanged)")]),
        (Some(b), Some(a)) => Some(vec![format!("link: {b} → {a}")]),
        (None, Some(a)) => Some(vec![format!("link → {a}")]),
        (Some(b), None) if item.op == "delete" => Some(vec![format!("link removed (was → {b})")]),
        _ => None,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Format {
    Passwd,
    Group,
    Fstab,
    Ini,
    Kv,
    Json,
    Lines,
}

const INI_SUFFIXES: &[&str] = &[
    ".service",
    ".socket",
    ".timer",
    ".target",
    ".mount",
    ".automount",
    ".path",
    ".slice",
    ".scope",
    ".network",
    ".netdev",
    ".link",
    ".desktop",
    ".repo",
    ".ini",
    ".nmconnection",
];

// The path is lowercased first, so suffix checks are already case-insensitive.
#[allow(clippy::case_sensitive_file_extension_comparisons)]
fn format_of(path: &str, sample: &str) -> Format {
    match path {
        "/etc/passwd" | "/etc/passwd-" => return Format::Passwd,
        "/etc/group" | "/etc/group-" => return Format::Group,
        "/etc/fstab" => return Format::Fstab,
        _ => {}
    }
    let lower = path.to_ascii_lowercase();
    if lower.ends_with(".json") {
        return Format::Json;
    }
    let has_section = sample.lines().any(|l| {
        let t = l.trim();
        t.starts_with('[') && t.ends_with(']') && t.len() > 2
    });
    if INI_SUFFIXES.iter().any(|s| lower.ends_with(s)) || (has_section && !lower.ends_with(".toml"))
    {
        return Format::Ini;
    }
    let meaningful: Vec<&str> = sample
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .collect();
    let kv = meaningful
        .iter()
        .filter(|l| {
            l.split_once('=').is_some_and(|(k, _)| {
                !k.trim().is_empty() && !k.trim().contains(char::is_whitespace)
            })
        })
        .count();
    if !meaningful.is_empty() && kv * 10 >= meaningful.len() * 8 {
        return Format::Kv;
    }
    Format::Lines
}

fn content_diff(path: &str, before: &str, after: &str) -> Vec<String> {
    let sample = if after.is_empty() { before } else { after };
    match format_of(path, sample) {
        Format::Passwd => passwd_diff(before, after),
        Format::Group => group_diff(before, after),
        Format::Fstab => fstab_diff(before, after),
        Format::Ini => map_diff(&ini(before), &ini(after)),
        Format::Kv => map_diff(&kv(before), &kv(after)),
        Format::Json => match (json(before), json(after)) {
            (Some(b), Some(a)) => map_diff(&b, &a),
            _ => lines_diff(before, after),
        },
        Format::Lines => lines_diff(before, after),
    }
}

fn strip_comment(line: &str) -> &str {
    let t = line.trim();
    if t.starts_with('#') || t.starts_with(';') {
        ""
    } else {
        t
    }
}

/// INI/keyfile/systemd: `[section] key` → value. Repeated keys (systemd allows them) are
/// joined with newlines in order.
fn ini(text: &str) -> BTreeMap<String, String> {
    let mut out: BTreeMap<String, String> = BTreeMap::new();
    let mut section = String::new();
    for line in text.lines() {
        let t = strip_comment(line);
        if t.is_empty() {
            continue;
        }
        if t.starts_with('[') && t.ends_with(']') {
            t[1..t.len() - 1].trim().clone_into(&mut section);
            continue;
        }
        let (k, v) = t
            .split_once('=')
            .map_or((t, ""), |(k, v)| (k.trim(), v.trim()));
        let key = if section.is_empty() {
            k.to_owned()
        } else {
            format!("[{section}] {k}")
        };
        out.entry(key)
            .and_modify(|e| {
                e.push('\n');
                e.push_str(v);
            })
            .or_insert_with(|| v.to_owned());
    }
    out
}

/// `KEY=value` files (sysctl.d, os-release, environment, /etc/default/*). Quotes kept as is.
fn kv(text: &str) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for line in text.lines() {
        let t = strip_comment(line);
        let t = t.strip_prefix("export ").unwrap_or(t);
        if let Some((k, v)) = t.split_once('=') {
            out.insert(k.trim().to_owned(), v.trim().to_owned());
        } else if !t.is_empty() {
            out.insert(t.to_owned(), String::new());
        }
    }
    out
}

fn json(text: &str) -> Option<BTreeMap<String, String>> {
    fn walk(prefix: &str, v: &serde_json::Value, out: &mut BTreeMap<String, String>, depth: usize) {
        if out.len() > 10_000 || depth > 64 {
            return;
        }
        match v {
            serde_json::Value::Object(m) => {
                for (k, v) in m {
                    let p = if prefix.is_empty() {
                        k.clone()
                    } else {
                        format!("{prefix}.{k}")
                    };
                    walk(&p, v, out, depth + 1);
                }
            }
            serde_json::Value::Array(a) => {
                for (i, v) in a.iter().enumerate() {
                    walk(&format!("{prefix}[{i}]"), v, out, depth + 1);
                }
            }
            other => {
                out.insert(prefix.to_owned(), other.to_string());
            }
        }
    }
    if text.trim().is_empty() {
        return Some(BTreeMap::new());
    }
    let v: serde_json::Value = serde_json::from_str(text).ok()?;
    let mut out = BTreeMap::new();
    walk("", &v, &mut out, 0);
    Some(out)
}

fn short(v: &str) -> String {
    let one = v.replace('\n', " | ");
    if one.is_empty() {
        "(empty)".to_owned()
    } else {
        one
    }
}

fn map_diff(before: &BTreeMap<String, String>, after: &BTreeMap<String, String>) -> Vec<String> {
    let mut out = Vec::new();
    for (k, a) in after {
        match before.get(k) {
            None => out.push(format!("+ {k} = {}", short(&redact(k, a)))),
            Some(b) if b != a => out.push(format!(
                "{k}: {} → {}",
                short(&redact(k, b)),
                short(&redact(k, a))
            )),
            Some(_) => {}
        }
    }
    for (k, b) in before {
        if !after.contains_key(k) {
            out.push(format!("- {k} (was {})", short(&redact(k, b))));
        }
    }
    if out.is_empty() {
        out.push("rewritten, no setting changed".into());
    }
    out
}

/// passwd: `name:x:uid:gid:gecos:home:shell`.
fn passwd_diff(before: &str, after: &str) -> Vec<String> {
    fn parse(text: &str) -> BTreeMap<String, Vec<String>> {
        text.lines()
            .filter_map(|l| {
                let f: Vec<String> = l.split(':').map(str::to_owned).collect();
                (f.len() >= 7 && !f[0].is_empty()).then(|| (f[0].clone(), f))
            })
            .collect()
    }
    let (b, a) = (parse(before), parse(after));
    let mut out = Vec::new();
    for (name, f) in &a {
        match b.get(name) {
            None => out.push(format!(
                "user {name} added (uid {}, home {}, shell {})",
                f[2], f[5], f[6]
            )),
            Some(old) if old != f => {
                let mut changes = Vec::new();
                for (i, label) in [
                    (2, "uid"),
                    (3, "gid"),
                    (4, "gecos"),
                    (5, "home"),
                    (6, "shell"),
                ] {
                    if old.get(i) != f.get(i) {
                        changes.push(format!(
                            "{label} {} → {}",
                            old.get(i).map_or("", String::as_str),
                            f.get(i).map_or("", String::as_str)
                        ));
                    }
                }
                out.push(format!("user {name}: {}", changes.join(", ")));
            }
            Some(_) => {}
        }
    }
    for name in b.keys() {
        if !a.contains_key(name) {
            out.push(format!("user {name} removed"));
        }
    }
    if out.is_empty() {
        out.push("rewritten, no user changed".into());
    }
    out
}

/// group: `name:x:gid:member,member`.
fn group_diff(before: &str, after: &str) -> Vec<String> {
    fn parse(text: &str) -> BTreeMap<String, (String, BTreeSet<String>)> {
        text.lines()
            .filter_map(|l| {
                let f: Vec<&str> = l.split(':').collect();
                (f.len() >= 4 && !f[0].is_empty()).then(|| {
                    let members = f[3]
                        .split(',')
                        .filter(|m| !m.is_empty())
                        .map(str::to_owned)
                        .collect();
                    (f[0].to_owned(), (f[2].to_owned(), members))
                })
            })
            .collect()
    }
    let (b, a) = (parse(before), parse(after));
    let mut out = Vec::new();
    for (name, (gid, members)) in &a {
        match b.get(name) {
            None => out.push(format!("group {name} added (gid {gid})")),
            Some((old_gid, old_members)) => {
                if old_gid != gid {
                    out.push(format!("group {name}: gid {old_gid} → {gid}"));
                }
                for m in members.difference(old_members) {
                    out.push(format!("group {name}: + member {m}"));
                }
                for m in old_members.difference(members) {
                    out.push(format!("group {name}: - member {m}"));
                }
            }
        }
    }
    for name in b.keys() {
        if !a.contains_key(name) {
            out.push(format!("group {name} removed"));
        }
    }
    if out.is_empty() {
        out.push("rewritten, no group changed".into());
    }
    out
}

/// fstab: entries keyed by mount point.
fn fstab_diff(before: &str, after: &str) -> Vec<String> {
    fn parse(text: &str) -> BTreeMap<String, String> {
        text.lines()
            .filter_map(|l| {
                let t = strip_comment(l);
                let f: Vec<&str> = t.split_whitespace().collect();
                (f.len() >= 3).then(|| (f[1].to_owned(), f.join(" ")))
            })
            .collect()
    }
    let (b, a) = (parse(before), parse(after));
    let mut out = Vec::new();
    for (mp, e) in &a {
        match b.get(mp) {
            None => out.push(format!("+ mount {mp}: {e}")),
            Some(old) if old != e => out.push(format!("mount {mp}: {old} → {e}")),
            Some(_) => {}
        }
    }
    for mp in b.keys() {
        if !a.contains_key(mp) {
            out.push(format!("- mount {mp}"));
        }
    }
    if out.is_empty() {
        out.push("rewritten, no mount changed".into());
    }
    out
}

/// Fallback: which non-blank lines were added and removed (as sets, order ignored).
fn lines_diff(before: &str, after: &str) -> Vec<String> {
    let set = |t: &str| -> BTreeSet<String> {
        t.lines()
            .map(strip_comment)
            .filter(|l| !l.is_empty())
            .map(str::to_owned)
            .collect()
    };
    let (b, a) = (set(before), set(after));
    let added: Vec<&String> = a.difference(&b).collect();
    let removed: Vec<&String> = b.difference(&a).collect();
    if added.is_empty() && removed.is_empty() {
        return vec!["rewritten, same lines (order or blank lines differ)".into()];
    }
    let mut out = Vec::new();
    let mut head = String::new();
    let _ = write!(head, "+{} -{} lines", added.len(), removed.len());
    out.push(head);
    out.extend(added.iter().map(|l| format!("+ {l}")));
    out.extend(removed.iter().map(|l| format!("- {l}")));
    out
}

/// Summaries for a batch, as the worker protocol carries them.
#[must_use]
pub fn summarize_all(items: &[Item]) -> Vec<Vec<String>> {
    items.iter().map(summarize).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(path: &str, op: &str, before: Option<&str>, after: Option<&str>) -> Item {
        Item {
            path: path.into(),
            op: op.into(),
            before: before.map(Into::into),
            after: after.map(Into::into),
            ..Item::default()
        }
    }

    #[test]
    fn unchanged_rewrite() {
        assert_eq!(
            summarize(&item(
                "/etc/passwd",
                "modify",
                Some("a:x:1:1::/:/bin/sh\n"),
                Some("a:x:1:1::/:/bin/sh\n")
            )),
            ["rewritten, content unchanged"]
        );
    }

    #[test]
    fn systemd_unit_and_dropin() {
        let before = "[Service]\nType=oneshot\nExecStart=/bin/true\n";
        let after = "[Service]\nType=simple\nExecStart=/bin/true\nEnvironment=OSREFLOG=1\n";
        assert_eq!(
            summarize(&item(
                "/etc/systemd/system/x.service",
                "modify",
                Some(before),
                Some(after)
            )),
            [
                "+ [Service] Environment = OSREFLOG=1",
                "[Service] Type: oneshot → simple"
            ]
        );
    }

    #[test]
    fn networkmanager_keyfile_hides_secrets() {
        let before = "[wifi]\npowersave=2\n[wifi-security]\npsk=hunter2\n";
        let after = "[wifi]\npowersave=3\n[wifi-security]\npsk=hunter3\n";
        let s = summarize(&item(
            "/etc/NetworkManager/conf.d/wifi.conf",
            "modify",
            Some(before),
            Some(after),
        ));
        assert_eq!(
            s,
            ["[wifi-security] psk: *** → ***", "[wifi] powersave: 2 → 3"]
        );
    }

    #[test]
    fn sysctl_and_os_release_as_key_value() {
        let s = summarize(&item(
            "/etc/sysctl.d/99-x.conf",
            "modify",
            Some("vm.swappiness = 60\n"),
            Some("vm.swappiness = 10\nkernel.panic = 5\n"),
        ));
        assert_eq!(s, ["+ kernel.panic = 5", "vm.swappiness: 60 → 10"]);
    }

    #[test]
    fn passwd_and_group() {
        let before = "root:x:0:0:root:/root:/bin/bash\n";
        let after = "root:x:0:0:root:/root:/bin/zsh\nosreflogwl:x:976:976::/:/sbin/nologin\n";
        assert_eq!(
            summarize(&item("/etc/passwd", "modify", Some(before), Some(after))),
            [
                "user osreflogwl added (uid 976, home /, shell /sbin/nologin)",
                "user root: shell /bin/bash → /bin/zsh"
            ]
        );
        let s = summarize(&item(
            "/etc/group",
            "modify",
            Some("wheel:x:10:khaled\n"),
            Some("wheel:x:10:khaled,bob\ndocker:x:973:\n"),
        ));
        assert_eq!(
            s,
            ["group docker added (gid 973)", "group wheel: + member bob"]
        );
    }

    #[test]
    fn fstab_by_mount_point() {
        let b = "UUID=1 / btrfs subvol=root 0 0\n";
        let a = "UUID=1 / btrfs subvol=root,noatime 0 0\nUUID=2 /data ext4 defaults 0 2\n";
        assert_eq!(
            summarize(&item("/etc/fstab", "modify", Some(b), Some(a))),
            [
                "mount /: UUID=1 / btrfs subvol=root 0 0 → UUID=1 / btrfs subvol=root,noatime 0 0",
                "+ mount /data: UUID=2 /data ext4 defaults 0 2"
            ]
        );
    }

    #[test]
    fn json_flattened() {
        let s = summarize(&item(
            "/etc/containers/policy.json",
            "modify",
            Some(r#"{"default":[{"type":"insecure"}]}"#),
            Some(r#"{"default":[{"type":"reject"}]}"#),
        ));
        assert_eq!(s, [r#"default[0].type: "insecure" → "reject""#]);
    }

    #[test]
    fn path_semantics() {
        assert_eq!(
            summarize(&item(
                "/etc/systemd/system/multi-user.target.wants/foo.service",
                "create",
                None,
                None
            )),
            ["enabled foo.service (wanted by multi-user.target)"]
        );
        assert_eq!(
            summarize(&item(
                "/etc/systemd/system/multi-user.target.wants/foo.service",
                "delete",
                None,
                None
            )),
            ["disabled foo.service (no longer wanted by multi-user.target)"]
        );
        let mut tz = item("/etc/localtime", "modify", None, None);
        tz.before_target = Some("../usr/share/zoneinfo/Asia/Beirut".into());
        tz.after_target = Some("../usr/share/zoneinfo/UTC".into());
        assert_eq!(summarize(&tz), ["time zone: Asia/Beirut → UTC"]);
        let mut mask = item("/etc/systemd/system/foo.service", "create", None, None);
        mask.after_target = Some("/dev/null".into());
        assert_eq!(summarize(&mask), ["masked foo.service"]);
    }

    #[test]
    fn creates_deletes_and_fallback() {
        assert_eq!(
            summarize(&item("/etc/foo.conf", "delete", Some("a\nb\n"), None)),
            ["removed (2 lines)"]
        );
        let s = summarize(&item(
            "/etc/ssh/sshd_config.d/99.conf",
            "create",
            None,
            Some("# hi\nPermitRootLogin no\n"),
        ));
        assert_eq!(
            s,
            ["new file (2 lines)", "+1 -0 lines", "+ PermitRootLogin no"]
        );
    }

    #[test]
    fn output_is_bounded() {
        let after = (0..500).fold(String::new(), |mut s, i| {
            let _ = writeln!(s, "k{i}=v");
            s
        });
        let s = summarize(&item("/etc/x.env", "modify", Some(""), Some(&after)));
        assert_eq!(s.len(), MAX_LINES);
        assert!(s.last().unwrap().starts_with("… and"));
        let long = format!("k={}\n", "x".repeat(5000));
        assert!(
            summarize(&item("/etc/y.env", "modify", Some("k=1\n"), Some(&long)))
                .iter()
                .all(|l| l.chars().count() <= MAX_LINE)
        );
    }

    proptest::proptest! {
        #![proptest_config(proptest::prelude::ProptestConfig::with_cases(2000))]
        #[test]
        fn never_panics_and_stays_bounded(path in "/etc/[a-z./-]{0,20}", op in "create|modify|delete|rename|attr", before in proptest::option::of(".{0,400}"), after in proptest::option::of(".{0,400}")) {
            let it = Item { path, op, before, after, before_target: None, after_target: None };
            let s = summarize(&it);
            proptest::prop_assert!(s.len() <= MAX_LINES);
            proptest::prop_assert!(s.iter().all(|l| l.chars().count() <= MAX_LINE));
        }
    }
}
