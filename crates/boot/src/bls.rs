//! Boot Loader Specification type #1 entries (`/boot/loader/entries/*.conf`), as Fedora's
//! GRUB `blscfg` reads them: one `key value` per line, keys may repeat (`initrd`).

use std::fmt::Write as _;

/// An entry, its lines in order. Comments and blank lines are kept so a rendered entry
/// differs from its source only where it was changed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    lines: Vec<Line>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Line {
    Field(String, String),
    Other(String),
}

impl Entry {
    #[must_use]
    pub fn parse(text: &str) -> Entry {
        let lines = text
            .lines()
            .map(|l| {
                let t = l.trim();
                if t.is_empty() || t.starts_with('#') {
                    return Line::Other(l.to_owned());
                }
                match t.split_once(char::is_whitespace) {
                    Some((k, v)) => Line::Field(k.to_owned(), v.trim().to_owned()),
                    None => Line::Field(t.to_owned(), String::new()),
                }
            })
            .collect();
        Entry { lines }
    }

    /// The first value of `key`.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&str> {
        self.lines.iter().find_map(|l| match l {
            Line::Field(k, v) if k == key => Some(v.as_str()),
            _ => None,
        })
    }

    /// Every value of `key`, in order (`initrd` may repeat, and one line may list several).
    #[must_use]
    pub fn get_all(&self, key: &str) -> Vec<&str> {
        self.lines
            .iter()
            .filter_map(|l| match l {
                Line::Field(k, v) if k == key => Some(v.split_whitespace()),
                _ => None,
            })
            .flatten()
            .collect()
    }

    /// Replaces every `key` line with one (where the first was), or appends it.
    pub fn set(&mut self, key: &str, value: &str) {
        let mut seen = false;
        self.lines.retain_mut(|l| match l {
            Line::Field(k, v) if k == key => {
                if seen {
                    return false;
                }
                seen = true;
                value.clone_into(v);
                true
            }
            _ => true,
        });
        if !seen {
            self.lines
                .push(Line::Field(key.to_owned(), value.to_owned()));
        }
    }

    pub fn remove(&mut self, key: &str) {
        self.lines
            .retain(|l| !matches!(l, Line::Field(k, _) if k == key));
    }

    #[must_use]
    pub fn render(&self) -> String {
        let mut out = String::new();
        for l in &self.lines {
            match l {
                Line::Field(k, v) if v.is_empty() => {
                    let _ = writeln!(out, "{k}");
                }
                Line::Field(k, v) => {
                    let _ = writeln!(out, "{k} {v}");
                }
                Line::Other(s) => {
                    let _ = writeln!(out, "{s}");
                }
            }
        }
        out
    }
}

/// The kernel command line of a probe: the default entry's, with the root subvolume replaced
/// and osreflog's own options added. `rootflags` keeps its other mount options (`compress=`…);
/// a `subvolid=` is dropped, since it would name the default's subvolume. Without `rootflags`
/// (root mounted with the filesystem's default subvolume), one is added.
#[must_use]
pub fn probe_options(default: &str, subvol: &str, extra: &[String]) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut had_rootflags = false;
    for word in default.split_whitespace() {
        if let Some(flags) = word.strip_prefix("rootflags=") {
            had_rootflags = true;
            let mut kept: Vec<String> = flags
                .split(',')
                .filter(|f| {
                    !f.is_empty() && !f.starts_with("subvol=") && !f.starts_with("subvolid=")
                })
                .map(str::to_owned)
                .collect();
            kept.insert(0, format!("subvol={subvol}"));
            out.push(format!("rootflags={}", kept.join(",")));
        } else if extra
            .iter()
            .any(|e| e.split('=').next() == word.split('=').next())
        {
            // Replaced by ours (a `panic=` of the default's, an old `osreflog.probe=`).
        } else {
            out.push(word.to_owned());
        }
    }
    if !had_rootflags {
        out.push(format!("rootflags=subvol={subvol}"));
    }
    out.extend(extra.iter().cloned());
    out.join(" ")
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    const FEDORA: &str = "\
title Fedora Linux (7.2.9-200.fc44.x86_64) 44 (Workstation Edition)
version 7.2.9-200.fc44.x86_64
linux /vmlinuz-7.2.9-200.fc44.x86_64
initrd /initramfs-7.2.9-200.fc44.x86_64.img
options root=UUID=7b7a13e0 ro rootflags=subvol=root rhgb quiet
grub_users $grub_users
grub_arg --unrestricted
grub_class fedora
";

    #[test]
    fn round_trip_is_exact() {
        assert_eq!(Entry::parse(FEDORA).render(), FEDORA);
        let odd = "# comment\n\ntitle  two  spaces\nversion 1\n";
        let e = Entry::parse(odd);
        assert_eq!(e.get("title"), Some("two  spaces"));
        assert_eq!(e.render(), "# comment\n\ntitle two  spaces\nversion 1\n");
    }

    #[test]
    fn set_replaces_repeated_keys_in_place() {
        let mut e = Entry::parse("linux /a\ninitrd /ucode.img\ninitrd /b.img\noptions x\n");
        e.set("initrd", "/osreflog/c.img");
        assert_eq!(e.render(), "linux /a\ninitrd /osreflog/c.img\noptions x\n");
        e.set("sort-key", "z");
        assert!(e.render().ends_with("sort-key z\n"));
        e.remove("options");
        assert_eq!(e.get("options"), None);
    }

    #[test]
    fn probe_options_swap_the_subvolume_only() {
        let extra = vec!["osreflog.probe=3".to_owned(), "panic=10".to_owned()];
        assert_eq!(
            probe_options(
                "root=UUID=7b7a ro rootflags=subvol=root,compress=zstd:1 rhgb quiet",
                "osreflog/probes/p3",
                &extra
            ),
            "root=UUID=7b7a ro rootflags=subvol=osreflog/probes/p3,compress=zstd:1 rhgb quiet \
             osreflog.probe=3 panic=10"
        );
        assert_eq!(
            probe_options("root=UUID=7b7a rootflags=subvolid=256 panic=0", "p", &extra),
            "root=UUID=7b7a rootflags=subvol=p osreflog.probe=3 panic=10"
        );
        assert_eq!(
            probe_options("root=UUID=7b7a ro", "p", &[]),
            "root=UUID=7b7a ro rootflags=subvol=p"
        );
    }
}
