//! Types shared by osreflogd and the osreflog CLI. They travel over D-Bus as JSON, the same
//! JSON the CLI prints with `--json`.

use serde::{Deserialize, Serialize};

/// The daemon's D-Bus names and polkit actions.
pub mod redact;
pub mod secrets;

pub mod bus {
    pub const NAME: &str = "io.github.khaledsaeed18.osreflog1";
    pub const PATH: &str = "/io/github/khaledsaeed18/osreflog1";
    pub const INTERFACE: &str = "io.github.khaledsaeed18.osreflog1";
    pub const ERROR_PREFIX: &str = "io.github.khaledsaeed18.osreflog1.Error";
    /// Reading history: active local sessions.
    pub const ACTION_READ: &str = "io.github.khaledsaeed18.osreflog.read";
    /// Taking a checkpoint by hand: admin authentication, kept for a while.
    pub const ACTION_CHECKPOINT: &str = "io.github.khaledsaeed18.osreflog.checkpoint";
    /// Undoing a cause: admin authentication every time, never kept.
    pub const ACTION_UNDO: &str = "io.github.khaledsaeed18.osreflog.undo";
    /// Running a bisect (builds probes, boots VMs): admin authentication, kept for a while.
    pub const ACTION_BISECT: &str = "io.github.khaledsaeed18.osreflog.bisect";
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
    // State rewritten on a timer, not changes anyone makes (P3: rsyslog alone made 19 of 27
    // causes in a two-minute testbed).
    "/var/lib/rsyslog/",
    "/var/lib/upower/",
    "/var/lib/systemd/timers/",
    "/var/lib/systemd/timesync/",
    "/var/lib/chrony/",
    "/var/lib/logrotate/",
    "/var/lib/NetworkManager/timestamps",
    "/var/lib/systemd/random-seed",
    // Container engines' image and layer stores: their own databases, kept open and written
    // in place (so only the state witness sees them), and changed by pulls and builds, not by
    // configuration (P4: dockerd's identity cache was the one unexplained change).
    "/var/lib/docker/",
    "/var/lib/containers/",
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

/// A short reference to a cause, attached to the changes it explains.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CauseRef {
    pub id: i64,
    pub kind: String,
    pub label: String,
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
    /// The cause the live witness saw making this change, when it saw it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cause: Option<CauseRef>,
    /// `both` (live and state witness), `bracket` (inside a package transaction the live
    /// witness bracketed), or `state` (only the snapshot diff saw it). `None` before P3.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub witness: Option<String>,
    /// What the change means, from its cause's semantic summary (display only).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub summary: Vec<String>,
}

impl Change {
    /// Only the times changed: mostly `relatime` access-time updates on files that were read
    /// or run. No fanotify event marks them, and they aren't changes worth explaining.
    #[must_use]
    pub fn times_only(&self) -> bool {
        self.op == "modify" && self.what.len() == 1 && self.what[0] == "times"
    }
}

/// The process a cause is about.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Actor {
    pub pid: u32,
    pub comm: String,
    pub exe: String,
    pub uid: u32,
    /// cgroup v2 path, e.g. `/system.slice/tlp.service`.
    pub cgroup: String,
    /// The command line that explains the change (redacted), e.g. `sudo sed -i ... /etc/hosts`.
    pub command: Vec<String>,
    /// For a change a daemon made because this actor asked over D-Bus: the daemon and the
    /// method, e.g. `firewalld.service (org.fedoraproject.FirewallD1.config.addZone2)`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub via: Option<String>,
}

/// A group of changes with one actor and one reason, as the live witness saw it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cause {
    pub id: i64,
    /// `dnf`, `unit`, `command`, `app` or `unknown`.
    pub kind: String,
    pub label: String,
    pub actor: Actor,
    pub started_ms: i64,
    pub ended_ms: i64,
    pub changes: u64,
    /// Package-owned paths written inside a dnf transaction, counted rather than listed.
    pub package_paths: u64,
}

/// One change as the live witness saw it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LiveChange {
    pub ts_ms: i64,
    pub path: String,
    /// `create`, `modify`, `delete`, `rename` or `attr`.
    pub op: String,
    pub from: Option<String>,
    /// `file` or `dir`.
    pub kind: String,
    /// BLAKE3 of the content after the change, when captured.
    pub blob: Option<String>,
    pub size: Option<u64>,
    /// The process that made this change, e.g. `sed (pid 1406613)` (P3 schema v3).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor: Option<String>,
    /// For a symlink: where it points after the change (P4 schema v4).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    /// What the change means, when summarized (filled in for display; not stored here).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub summary: Vec<String>,
}

/// A time window in which the live witness may have missed changes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Gap {
    pub id: i64,
    pub start_ms: i64,
    pub end_ms: i64,
    pub reason: String,
}

/// How much of a diff the live witness explains.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Coverage {
    /// Changes in `config` paths, and how many have a cause.
    pub config: u64,
    pub config_attributed: u64,
    /// The same over every class except `noise`.
    pub all: u64,
    pub all_attributed: u64,
}

/// One answer to "why did this path change?".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WhyEntry {
    pub cause: Cause,
    pub change: LiveChange,
    /// Content before and after, when both are known, text and not secret.
    pub before: Option<String>,
    pub after: Option<String>,
}

/// A cause with every change it made.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CauseDetail {
    pub cause: Cause,
    pub changes: Vec<LiveChange>,
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

/// Counts of changes by class, before filtering. A change of the times alone counts as noise:
/// they are mostly `relatime` access times on files a boot read (P8 pre-flight: 2,582 "package
/// changes" between two boots were all of these).
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

    /// Counts `c` under its class, or as noise when only its times changed.
    pub fn add_change(&mut self, c: &Change) {
        self.add(if c.times_only() {
            Class::Noise
        } else {
            c.class
        });
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
    /// How many changes the live witness explains (empty before P3).
    #[serde(default)]
    pub coverage: Coverage,
    /// Windows inside this diff where the live witness may have missed changes.
    #[serde(default)]
    pub gaps: Vec<Gap>,
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

/// The live witness's counters since the daemon started.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LiveStatus {
    pub running: bool,
    /// fanotify events read.
    pub events: u64,
    /// Changes recorded into causes.
    pub recorded: u64,
    /// Events on other subvolumes or noise paths, dropped early.
    pub out_of_scope: u64,
    /// Events whose directory could no longer be resolved.
    pub unresolved: u64,
    pub overflows: u64,
    pub ring_drops: u64,
    pub open_causes: u64,
    pub processes: u64,
    /// Causes credited to a D-Bus caller instead of the daemon that wrote (P4).
    #[serde(default)]
    pub delegated: u64,
    /// Changes not attributed because open causes held too many (threat model A3); the state
    /// witness still records them.
    #[serde(default)]
    pub dropped: u64,
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
    /// The live witness, when it's running (P3).
    #[serde(default)]
    pub live: Option<LiveStatus>,
    /// What the kept snapshots hold on disk, when the diffs say (0.3.0 on).
    #[serde(default)]
    pub space: Option<Space>,
}

/// Space held by the kept snapshots: data they still have that `/` no longer does (deleted
/// files, replaced content), from the diffs between checkpoints. Each piece of data leaves
/// `/` in exactly one interval, so what left in an interval starting at a kept snapshot is
/// surely held (`at_least`), and what left in any interval since the oldest kept one may be
/// (`at_most`). Reflinks and compression aren't accounted for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Space {
    pub at_least: u64,
    pub at_most: u64,
    /// The checkpoint the bounds start at: the oldest kept snapshot, or a later one when the
    /// diffs before it were computed by an older osreflog, which didn't measure this.
    pub since: String,
    /// `since` is the oldest kept snapshot: the bounds cover everything kept.
    pub complete: bool,
}

impl Space {
    /// "between 210 MiB and 1.4 GiB", or one size when the bounds meet.
    #[must_use]
    pub fn range_text(&self) -> String {
        if bytes_text(self.at_least) == bytes_text(self.at_most) {
            bytes_text(self.at_most)
        } else {
            format!(
                "between {} and {}",
                bytes_text(self.at_least),
                bytes_text(self.at_most)
            )
        }
    }
}

/// A size in binary units: "512 B", "12 KiB", "3.4 GiB".
#[must_use]
pub fn bytes_text(b: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    #[allow(clippy::cast_precision_loss)] // two significant digits are shown
    let mut v = b as f64;
    let mut unit = 0;
    while v >= 1024.0 && unit < UNITS.len() - 1 {
        v /= 1024.0;
        unit += 1;
    }
    if unit == 0 || v >= 10.0 {
        format!("{v:.0} {}", UNITS[unit])
    } else {
        format!("{v:.1} {}", UNITS[unit])
    }
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
    fn sizes_read_as_people_say_them() {
        assert_eq!(bytes_text(512), "512 B");
        assert_eq!(bytes_text(12 * 1024), "12 KiB");
        assert_eq!(bytes_text(3_650_000_000), "3.4 GiB");
        let s = Space {
            at_least: 210 << 20,
            at_most: 1_500_000_000,
            since: "c000001".into(),
            complete: true,
        };
        assert_eq!(s.range_text(), "between 210 MiB and 1.4 GiB");
        assert_eq!(
            Space {
                at_least: 1 << 30,
                ..s.clone()
            }
            .range_text(),
            "between 1.0 GiB and 1.4 GiB"
        );
        assert_eq!(
            Space {
                at_least: 0,
                at_most: 0,
                ..s.clone()
            }
            .range_text(),
            "0 B"
        );
    }

    #[test]
    fn classification() {
        assert_eq!(classify("/etc/hosts"), Class::Config);
        assert_eq!(classify("/etc"), Class::Config);
        assert_eq!(classify("/usr/local/bin/tool"), Class::Config);
        assert_eq!(classify("/usr/bin/bash"), Class::Package);
        assert_eq!(classify("/usr/lib/sysimage/rpm/rpmdb.sqlite"), Class::Noise);
        assert_eq!(classify("/var/log/messages"), Class::Noise);
        assert_eq!(
            classify("/var/lib/NetworkManager/NetworkManager.state"),
            Class::State
        );
        assert_eq!(classify("/var/lib/NetworkManager/timestamps"), Class::Noise);
        assert_eq!(classify("/var/lib/rsyslog/imjournal.state"), Class::Noise);
        assert_eq!(
            classify("/var/lib/docker/image/identity-cache.db"),
            Class::Noise
        );
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
