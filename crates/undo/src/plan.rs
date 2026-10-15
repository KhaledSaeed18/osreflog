//! What an undo plan is made of, and the checks every plan passes before anything is touched.

use std::fmt;

use osreflog_model::secrets::{is_secret_mode, is_secret_path};
use serde::{Deserialize, Serialize};

/// Paths undo may restore: config, never package-owned files or state.
pub const SCOPE: &[&str] = &["/etc/", "/opt/", "/usr/local/"];
/// Where undo moves what it removes.
pub const ATTIC: &str = "/var/lib/osreflog/attic/";
/// The largest file a plan carries.
pub const MAX_FILE: usize = 8 << 20;
pub const MAX_STEPS: usize = 10_000;

/// What is at a path, as far as the 3-way check cares: its type and its content.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum State {
    Absent,
    /// A regular file, by the BLAKE3 hash of its content.
    File {
        hash: String,
    },
    Symlink {
        target: String,
    },
    Dir,
    /// A FIFO, socket or device node: never expected, never touched.
    Other,
}

impl fmt::Display for State {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Absent => f.write_str("absent"),
            Self::File { hash } => write!(f, "file {}", &hash[..hash.len().min(12)]),
            Self::Symlink { target } => write!(f, "symlink → {target}"),
            Self::Dir => f.write_str("directory"),
            Self::Other => f.write_str("special file"),
        }
    }
}

/// Owner, permission bits and extended attributes.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Meta {
    pub uid: u32,
    pub gid: u32,
    /// Permission bits only (`0o7777` at most).
    pub mode: u32,
    /// Name and value, the value as hex. `security.selinux` is how labels travel.
    #[serde(default)]
    pub xattrs: Vec<(String, String)>,
}

/// What a step puts back.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Version {
    Absent,
    File {
        /// The content, as hex.
        content: String,
        meta: Meta,
    },
    Symlink {
        target: String,
        uid: u32,
        gid: u32,
    },
    Dir {
        meta: Meta,
    },
}

impl Version {
    /// A file version from raw content.
    #[must_use]
    pub fn file(content: &[u8], meta: Meta) -> Self {
        Self::File {
            content: to_hex(content),
            meta,
        }
    }

    /// The state this version leaves at its path.
    #[must_use]
    pub fn state(&self) -> State {
        match self {
            Self::Absent => State::Absent,
            Self::File { content, .. } => State::File {
                hash: from_hex(content).map_or_else(String::new, |c| hash(&c)),
            },
            Self::Symlink { target, .. } => State::Symlink {
                target: target.clone(),
            },
            Self::Dir { .. } => State::Dir,
        }
    }
}

/// One path: what the cause left there, and what was there before.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Step {
    pub path: String,
    /// A: the state the cause left. The current state must match it.
    pub expect: State,
    /// B: the state before the cause.
    pub restore: Version,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Plan {
    /// The cause this plan reverts.
    pub undo_of: i64,
    /// Where removed objects go, e.g. `/var/lib/osreflog/attic/20261015-154000-c42`.
    pub attic: String,
    pub steps: Vec<Step>,
}

/// The BLAKE3 hash of `content`, as osreflog's blobs name it.
#[must_use]
pub fn hash(content: &[u8]) -> String {
    blake3::hash(content).to_hex().to_string()
}

#[must_use]
pub fn to_hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push(char::from(DIGITS[usize::from(b >> 4)]));
        s.push(char::from(DIGITS[usize::from(b & 0xf)]));
    }
    s
}

#[must_use]
pub fn from_hex(s: &str) -> Option<Vec<u8>> {
    let digit = |c: u8| match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        _ => None,
    };
    if !s.len().is_multiple_of(2) {
        return None;
    }
    s.as_bytes()
        .chunks(2)
        .map(|p| Some(digit(p[0])? << 4 | digit(p[1])?))
        .collect()
}

/// Whether `path` is absolute and normal: no empty, `.` or `..` components, no trailing
/// slash, no NUL.
#[must_use]
pub fn is_normal(path: &str) -> bool {
    path.len() > 1
        && path.len() < 4096
        && path.starts_with('/')
        && !path.ends_with('/')
        && !path.contains('\0')
        && path[1..]
            .split('/')
            .all(|c| !c.is_empty() && c != "." && c != ".." && c.len() < 256)
}

/// Why a step may not run at all, whatever is on disk.
#[must_use]
pub fn refusal(step: &Step) -> Option<String> {
    let p = step.path.as_str();
    if !is_normal(p) {
        return Some("not a normal absolute path".into());
    }
    if !SCOPE.iter().any(|s| p.starts_with(s)) {
        return Some(format!("outside undo's scope ({})", SCOPE.join(", ")));
    }
    if is_secret_path(p) {
        return Some("a secret: restore it by hand from a snapshot".into());
    }
    let meta = match &step.restore {
        Version::File { content, meta } => {
            if content.len() / 2 > MAX_FILE {
                return Some(format!("larger than {} MiB", MAX_FILE >> 20));
            }
            if from_hex(content).is_none() {
                return Some("content is not hex".into());
            }
            Some(meta)
        }
        Version::Dir { meta } => Some(meta),
        Version::Absent | Version::Symlink { .. } => None,
    };
    if let Some(m) = meta {
        if m.mode & !0o7777 != 0 {
            return Some("mode has bits beyond permissions".into());
        }
        if matches!(step.restore, Version::File { .. }) && is_secret_mode(m.uid, m.mode) {
            return Some("a secret (root-only file): restore it by hand from a snapshot".into());
        }
        if m.mode & 0o6000 != 0 && matches!(step.restore, Version::File { .. }) {
            return Some("set-user-ID or set-group-ID: restore it by hand".into());
        }
        if m.xattrs.iter().any(|(n, _)| n == "security.capability") {
            return Some("carries a file capability: restore it by hand".into());
        }
        if m.xattrs
            .iter()
            .any(|(n, v)| n.contains('\0') || from_hex(v).is_none())
        {
            return Some("a malformed extended attribute".into());
        }
    }
    None
}

/// Checks the plan as a whole.
///
/// # Errors
/// If the attic is outside [`ATTIC`], the plan is too long, or a path appears twice.
pub fn check(plan: &Plan) -> Result<(), String> {
    if !plan.attic.starts_with(ATTIC) || !is_normal(&plan.attic) {
        return Err(format!("attic {:?} is not under {ATTIC}", plan.attic));
    }
    if plan.steps.len() > MAX_STEPS {
        return Err(format!("{} steps, more than {MAX_STEPS}", plan.steps.len()));
    }
    let mut paths: Vec<&str> = plan.steps.iter().map(|s| s.path.as_str()).collect();
    paths.sort_unstable();
    if let Some(&[a, _]) = paths.windows(2).find(|w| w[0] == w[1]) {
        return Err(format!("{a} appears twice"));
    }
    Ok(())
}

/// The order steps run in: removals deepest first (children before their directory), then
/// restores shallowest first (a directory before its children).
#[must_use]
pub fn ordered(steps: &[Step]) -> Vec<&Step> {
    let depth = |s: &Step| s.path.matches('/').count();
    let (mut removals, mut restores): (Vec<&Step>, Vec<&Step>) = steps
        .iter()
        .partition(|s| matches!(s.restore, Version::Absent));
    removals.sort_by(|a, b| depth(b).cmp(&depth(a)).then(a.path.cmp(&b.path)));
    restores.sort_by(|a, b| depth(a).cmp(&depth(b)).then(a.path.cmp(&b.path)));
    removals.into_iter().chain(restores).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn step(path: &str, restore: Version) -> Step {
        Step {
            path: path.into(),
            expect: State::Absent,
            restore,
        }
    }

    fn meta(uid: u32, mode: u32) -> Meta {
        Meta {
            uid,
            gid: 0,
            mode,
            xattrs: Vec::new(),
        }
    }

    #[test]
    fn hex_round_trips() {
        let b = [0u8, 1, 0x7f, 0x80, 0xff];
        assert_eq!(to_hex(&b), "00017f80ff");
        assert_eq!(from_hex(&to_hex(&b)).as_deref(), Some(&b[..]));
        assert!(from_hex("abc").is_none() && from_hex("zz").is_none() && from_hex("AB").is_none());
    }

    #[test]
    fn normal_paths() {
        for p in ["/etc/hosts", "/etc/a b/c", "/usr/local/bin/x"] {
            assert!(is_normal(p), "{p}");
        }
        for p in [
            "etc/hosts",
            "/etc/../root",
            "/etc/./x",
            "/etc//x",
            "/etc/",
            "/",
            "/etc/x\0",
        ] {
            assert!(!is_normal(p), "{p:?}");
        }
    }

    #[test]
    fn refusals() {
        let file = |m| Version::file(b"x", m);
        assert!(refusal(&step("/etc/hosts", file(meta(0, 0o644)))).is_none());
        assert!(
            refusal(&step("/var/lib/x", file(meta(0, 0o644))))
                .unwrap()
                .contains("scope")
        );
        assert!(
            refusal(&step("/usr/bin/x", Version::Absent))
                .unwrap()
                .contains("scope")
        );
        assert!(refusal(&step("/etc/../root/x", Version::Absent)).is_some());
        assert!(
            refusal(&step("/etc/shadow", file(meta(0, 0o644))))
                .unwrap()
                .contains("secret")
        );
        assert!(
            refusal(&step("/etc/app.conf", file(meta(0, 0o600))))
                .unwrap()
                .contains("secret")
        );
        assert!(refusal(&step("/etc/app.conf", file(meta(1000, 0o600)))).is_none());
        assert!(
            refusal(&step("/usr/local/bin/x", file(meta(0, 0o4755))))
                .unwrap()
                .contains("set-user-ID")
        );
        let mut cap = meta(0, 0o755);
        cap.xattrs.push(("security.capability".into(), "01".into()));
        assert!(
            refusal(&step("/usr/local/bin/x", file(cap)))
                .unwrap()
                .contains("capability")
        );
        assert!(
            refusal(&step("/etc/x", file(meta(0, 0o10_644))))
                .unwrap()
                .contains("bits")
        );
    }

    #[test]
    fn plan_checks_and_order() {
        let mut plan = Plan {
            undo_of: 1,
            attic: "/var/lib/osreflog/attic/t".into(),
            steps: vec![
                step("/etc/d", Version::Absent),
                step("/etc/d/f", Version::Absent),
                step("/etc/n/m", Version::file(b"", meta(0, 0o644))),
                step(
                    "/etc/n",
                    Version::Dir {
                        meta: meta(0, 0o755),
                    },
                ),
            ],
        };
        assert!(check(&plan).is_ok());
        let order: Vec<&str> = ordered(&plan.steps)
            .iter()
            .map(|s| s.path.as_str())
            .collect();
        assert_eq!(order, ["/etc/d/f", "/etc/d", "/etc/n", "/etc/n/m"]);
        plan.steps.push(step("/etc/d", Version::Absent));
        assert!(check(&plan).unwrap_err().contains("twice"));
        plan.attic = "/tmp/attic".into();
        assert!(check(&plan).is_err());
    }

    #[test]
    fn version_states() {
        assert_eq!(
            Version::file(b"hello", Meta::default()).state(),
            State::File {
                hash: hash(b"hello")
            }
        );
        assert_eq!(Version::Absent.state(), State::Absent);
    }
}
