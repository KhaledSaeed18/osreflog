//! Keeping secrets out of what osreflog records about a process.

/// Words that make a key's value secret, when they're in its last segment.
const SECRET_KEY_WORDS: &[&str] = &[
    "password",
    "passwd",
    "passphrase",
    "psk",
    "secret",
    "token",
    "api-key",
    "apikey",
    "api_key",
    "private-key",
    "private_key",
    "credential",
];

/// Whether a positional argument is a key naming a secret (its last `/`, `.` or `:` segment
/// contains a secret word), so the argument after it is that secret.
fn names_secret(arg: &str) -> bool {
    if arg.starts_with('-') || arg.contains('=') || arg.contains(char::is_whitespace) {
        return false;
    }
    let last = arg
        .rsplit(['/', '.', ':'])
        .next()
        .unwrap_or(arg)
        .to_ascii_lowercase();
    SECRET_KEY_WORDS.iter().any(|w| last.contains(w))
}

/// Hides secret values in a command line: `--password=x`, `--token x`, `FOO_TOKEN=x`,
/// `https://user:pass@host`, and the value after a key that names a secret, as in
/// `dconf write /app/api-key x`, `gsettings set org.app password x` or
/// `nmcli connection modify home wifi-sec.psk x`.
#[must_use]
pub fn args(args: &[String]) -> Vec<String> {
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
        if names_secret(a) {
            out.push(a.clone());
            hide_next = true;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redaction() {
        let r = |a: &[&str]| args(&a.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>());
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
            r(&["dconf", "write", "/org/app/api-key", "'v'"]),
            ["dconf", "write", "/org/app/api-key", "***"]
        );
        assert_eq!(
            r(&["gsettings", "set", "org.app", "smtp-password", "v"]),
            ["gsettings", "set", "org.app", "smtp-password", "***"]
        );
        assert_eq!(
            r(&[
                "nmcli",
                "c",
                "modify",
                "home",
                "wifi-sec.psk",
                "v",
                "ipv4.method",
                "auto"
            ]),
            [
                "nmcli",
                "c",
                "modify",
                "home",
                "wifi-sec.psk",
                "***",
                "ipv4.method",
                "auto"
            ]
        );
        assert_eq!(
            r(&[
                "gsettings",
                "set",
                "org.gnome.desktop.peripherals.keyboard",
                "delay",
                "300"
            ]),
            [
                "gsettings",
                "set",
                "org.gnome.desktop.peripherals.keyboard",
                "delay",
                "300"
            ]
        );
        assert_eq!(
            r(&["sed", "-i", "s/a/b/", "/etc/hosts"]),
            ["sed", "-i", "s/a/b/", "/etc/hosts"]
        );
    }
}
