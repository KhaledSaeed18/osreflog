//! Types shared by osreflogd and the osreflog CLI. They travel over D-Bus as JSON, the same
//! JSON the CLI prints with `--json`.

use serde::{Deserialize, Serialize};

/// The daemon's D-Bus names and polkit actions.
pub mod bus {
    pub const NAME: &str = "io.github.khaledsaeed18.osreflog1";
    pub const PATH: &str = "/io/github/khaledsaeed18/osreflog1";
    pub const INTERFACE: &str = "io.github.khaledsaeed18.osreflog1";
    pub const ERROR_PREFIX: &str = "io.github.khaledsaeed18.osreflog1.Error";
    /// Reading history: active local sessions.
    pub const ACTION_READ: &str = "io.github.khaledsaeed18.osreflog.read";
    /// Taking a checkpoint by hand: admin authentication, kept for a while.
    pub const ACTION_CHECKPOINT: &str = "io.github.khaledsaeed18.osreflog.checkpoint";
}

/// Why a checkpoint was taken.
pub mod reason {
    pub const PRE_DNF: &str = "pre-dnf";
    pub const POST_DNF: &str = "post-dnf";
    pub const BOOT: &str = "boot";
    pub const SCHEDULED: &str = "scheduled";
    pub const MANUAL: &str = "manual";
    /// A temporary snapshot of the live system for `@{now}`; never stored.
    pub const LIVE: &str = "live";
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Checkpoint {
    pub id: i64,
    /// Subvolume name under `osreflog/snapshots/`.
    pub name: String,
    /// Unix time in milliseconds.
    pub created_ms: i64,
    pub reason: String,
    /// btrfs generation of the snapshot.
    pub generation: u64,
    pub boot_id: String,
    pub note: Option<String>,
    /// The package manager process behind a `pre-dnf`/`post-dnf` checkpoint.
    pub pm_pid: Option<u32>,
    pub pm_comm: Option<String>,
    pub pinned: bool,
    /// When the snapshot was deleted by retention; its history stays in the ledger.
    pub pruned_ms: Option<i64>,
}

/// Where a path lives, which decides how prominently a change is shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Class {
    /// `/etc`, `/opt`, `/usr/local`: configuration and hand-installed software.
    Config,
    /// `/var` (except noise), `/root`, `/srv`: state that shapes behaviour.
    State,
    /// `/usr` outside `/usr/local`: package-owned.
    Package,
    /// Caches, logs, temporary files and the package databases themselves.
    Noise,
    Other,
}

impl Class {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Class::Config => "config",
            Class::State => "state",
            Class::Package => "package",
            Class::Noise => "noise",
            Class::Other => "other",
        }
    }

    /// The inverse of [`Class::as_str`].
    #[must_use]
    pub fn parse(s: &str) -> Option<Class> {
        Some(match s {
            "config" => Class::Config,
            "state" => Class::State,
            "package" => Class::Package,
            "noise" => Class::Noise,
            "other" => Class::Other,
            _ => return None,
        })
    }
}

const NOISE: &[&str] = &[
    "/var/cache/",
    "/var/log/",
    "/var/tmp/",
    "/tmp/",
    "/var/lib/dnf/",
    "/var/lib/PackageKit/",
    "/var/lib/systemd/coredump/",
    "/usr/lib/sysimage/rpm/",
    "/usr/lib/sysimage/libdnf5/",
];
const CONFIG: &[&str] = &["/etc/", "/opt/", "/usr/local/", "/boot/"];
const STATE: &[&str] = &["/var/", "/root/", "/srv/"];

/// Classifies an absolute path (as it appears in a change set).
#[must_use]
pub fn classify(path: &str) -> Class {
    let under = |prefixes: &[&str]| {
        prefixes
            .iter()
            .any(|p| path.starts_with(p) || path == p.trim_end_matches('/'))
    };
    if under(NOISE) {
        Class::Noise
    } else if under(CONFIG) {
        Class::Config
    } else if path.starts_with("/usr/") || path == "/usr" {
        Class::Package
    } else if under(STATE) {
        Class::State
    } else {
        Class::Other
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Change {
    pub path: String,
    /// `create`, `modify`, `delete` or `rename`.
    pub op: String,
    /// The old path of a `rename`.
    pub from: Option<String>,
    /// `file`, `dir`, `symlink`, `special` or `unknown`.
    pub kind: String,
    pub class: Class,
    /// What changed: `content`, `mode`, `owner`, `xattr`, `times`, `fileattr`.
    pub what: Vec<String>,
    pub bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackageChange {
    pub name: String,
    pub arch: String,
    /// `install`, `remove`, `upgrade` or `downgrade`.
    pub action: String,
    pub from_evr: Option<String>,
    pub to_evr: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Transaction {
    /// dnf5's transaction id.
    pub id: i64,
    pub begin: i64,
    pub end: Option<i64>,
    pub user_id: u32,
    /// The command line, when dnf recorded one (`PackageKit` leaves it empty).
    pub description: String,
    pub state: String,
}

/// Counts of changes by class, before filtering.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClassCounts {
    pub config: u64,
    pub state: u64,
    pub package: u64,
    pub noise: u64,
    pub other: u64,
}

impl ClassCounts {
    pub fn add(&mut self, class: Class) {
        match class {
            Class::Config => self.config += 1,
            Class::State => self.state += 1,
            Class::Package => self.package += 1,
            Class::Noise => self.noise += 1,
            Class::Other => self.other += 1,
        }
    }

    #[must_use]
    pub fn total(&self) -> u64 {
        self.config + self.state + self.package + self.noise + self.other
    }
}

/// One side of a diff: a checkpoint, or the live system.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Endpoint {
    Checkpoint(Checkpoint),
    Live { taken_ms: i64 },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiffReport {
    pub from: Endpoint,
    pub to: Endpoint,
    /// Which witnesses saw these changes; `["state"]` until the live witness exists (P3).
    pub witnesses: Vec<String>,
    /// Served from the ledger rather than computed for this request.
    pub cached: bool,
    pub compute_ms: u64,
    pub stream_bytes: u64,
    pub commands: u64,
    pub changed_bytes: u64,
    pub counts: ClassCounts,
    pub packages: Vec<PackageChange>,
    pub transactions: Vec<Transaction>,
    /// The changes requested: by default everything except `package` and `noise` paths and
    /// times-only modifications.
    pub changes: Vec<Change>,
    /// How many changes were left out by the class filter.
    pub hidden: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogEntry {
    pub checkpoint: Checkpoint,
    /// Summary of the interval from the previous checkpoint, when computed.
    pub interval: Option<IntervalSummary>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IntervalSummary {
    pub from_id: i64,
    pub counts: ClassCounts,
    pub packages: u64,
    pub transactions: Vec<Transaction>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Status {
    pub version: String,
    /// The tracked mount, normally `/`.
    pub root: String,
    pub device: String,
    pub subvolume: String,
    pub checkpoints: u64,
    pub snapshots_kept: u64,
    pub latest: Option<Checkpoint>,
    pub chain_entries: u64,
    /// Intervals still waiting for their diff.
    pub pending_diffs: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerifyReport {
    pub entries: u64,
    pub ok: bool,
    /// The first chain entry that doesn't match its record, if any.
    pub first_bad: Option<u64>,
    pub detail: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classification() {
        assert_eq!(classify("/etc/hosts"), Class::Config);
        assert_eq!(classify("/etc"), Class::Config);
        assert_eq!(classify("/usr/local/bin/tool"), Class::Config);
        assert_eq!(classify("/usr/bin/bash"), Class::Package);
        assert_eq!(classify("/usr/lib/sysimage/rpm/rpmdb.sqlite"), Class::Noise);
        assert_eq!(classify("/var/log/messages"), Class::Noise);
        assert_eq!(classify("/var/lib/NetworkManager/timestamps"), Class::State);
        assert_eq!(classify("/variable"), Class::Other);
        assert_eq!(classify("/home"), Class::Other);
    }

    #[test]
    fn class_round_trips() {
        for c in [
            Class::Config,
            Class::State,
            Class::Package,
            Class::Noise,
            Class::Other,
        ] {
            assert_eq!(Class::parse(c.as_str()), Some(c));
            assert_eq!(
                serde_json::to_string(&c).unwrap(),
                format!("\"{}\"", c.as_str())
            );
        }
    }
}
