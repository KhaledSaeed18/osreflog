//! What a bisect session is asked to do and what it reports, shared by the daemon and the CLI.

use serde::{Deserialize, Serialize};

use crate::search::Verdict;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Tier {
    /// A container on the probe: userspace (binaries, libraries, config, units).
    Nspawn,
    /// A VM booting the probe's own kernel: also kernel, modules, early boot.
    Vm,
    /// The machine itself, rebooted once into the probe: also hardware, firmware, the GPU,
    /// Wi-Fi. Verdicts by hand only (`bisect start`).
    Metal,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Args {
    /// References, as for `osreflog diff`: `c42`, `@{1}`, `@{boot-1}`, …
    pub good: String,
    pub bad: String,
    pub tier: Tier,
    /// nspawn: boot systemd in the container and run the test as a unit.
    #[serde(default)]
    pub boot: bool,
    /// The test, a shell script run as root inside the probe. Exit 0 good, 125 skip, 1–127 bad.
    pub test: String,
    /// Per probe; default 120 s (nspawn), 240 s (vm).
    #[serde(default)]
    pub timeout_s: Option<u64>,
    /// Verdicts from a person (`bisect start`), not from a test (`bisect run`): each probe
    /// waits, booted, until someone says good, bad or skip. `test` is unused.
    #[serde(default)]
    pub manual: bool,
}

/// A probe waiting for a person's verdict.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Waiting {
    pub probe: u32,
    /// As in [`ProbeRecord::what`].
    pub what: String,
    pub since_ms: i64,
    /// How to look at it, and how to answer.
    pub how: Vec<String>,
    /// A metal probe: armed for the next boot, waiting for a reboot into it.
    #[serde(default)]
    pub reboot: bool,
}

/// One probe and what it said.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProbeRecord {
    /// `checkpoint c000041`, or `after #452 (3 of 7 causes)`.
    pub what: String,
    pub verdict: Verdict,
    pub ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub peak_rss_kib: Option<u64>,
    /// Paths whose replayed version was approximate in this probe.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub approximate: Vec<String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub detail: String,
    /// The end of the test's output in this probe.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub output: String,
}

/// A cause, as a result names it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Culprit {
    pub id: i64,
    pub kind: String,
    pub label: String,
    pub started_ms: i64,
    /// The paths it changed (at most a few dozen, for display).
    pub paths: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "found", rename_all = "snake_case")]
pub enum Finding {
    /// The first bad cause.
    Cause {
        culprit: Culprit,
        /// The last good and first bad checkpoints around it.
        between: (String, String),
        /// Approximate replays in the two probes that decided it: if any, say so.
        approximate: Vec<String>,
    },
    /// Several candidates: every probe between them was skipped.
    Causes { candidates: Vec<Culprit> },
    /// The first bad checkpoint, when no recorded cause in its interval explains the change.
    Checkpoint {
        last_good: String,
        first_bad: String,
        why: String,
    },
    /// The test passes on the bad end: nothing to bisect in this tier.
    NotReproduced { detail: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Status {
    pub args: Args,
    pub running: bool,
    pub started_ms: i64,
    /// Human-readable progress, one line per step.
    pub log: Vec<String>,
    pub probes: Vec<ProbeRecord>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finding: Option<Finding>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// A manual session's probe that waits for a verdict.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub waiting: Option<Waiting>,
}

/// What a test needs from a probe, for `--tier auto`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Need {
    /// The kernel, its modules, early boot: a VM at least.
    Kernel,
    /// This machine's hardware or firmware (Wi-Fi, GPU, suspend): only a metal probe.
    Hardware,
}

impl Tier {
    /// The cheapest tier that has everything `needs` asks for. `manual`: a person gives the
    /// verdicts (`bisect start`), where a VM isn't offered (nothing to enter) and kernel needs
    /// go to metal. A hardware need without a person has no tier: a metal probe can't judge
    /// itself.
    ///
    /// # Errors
    /// The reason, when no tier fits.
    pub fn auto(needs: &[Need], manual: bool) -> Result<Tier, String> {
        let hardware = needs.contains(&Need::Hardware);
        let kernel = needs.contains(&Need::Kernel);
        match (manual, hardware, kernel) {
            (false, true, _) => Err(
                "a test that needs this machine's hardware needs a person to judge it: osreflog bisect start --tier metal"
                    .into(),
            ),
            (false, false, true) => Ok(Tier::Vm),
            (false | true, false, false) => Ok(Tier::Nspawn),
            (true, _, _) => Ok(Tier::Metal),
        }
    }

    /// The next tier up, to try when a regression doesn't reproduce in this one.
    #[must_use]
    pub fn next(self, manual: bool) -> Option<Tier> {
        match (self, manual) {
            (Tier::Nspawn, false) => Some(Tier::Vm),
            (Tier::Nspawn | Tier::Vm, true) => Some(Tier::Metal),
            // From a VM, metal needs a person: suggested as bisect start instead.
            (Tier::Vm, false) | (Tier::Metal, _) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_picks_the_cheapest_tier_that_fits() {
        assert_eq!(Tier::auto(&[], false), Ok(Tier::Nspawn));
        assert_eq!(Tier::auto(&[Need::Kernel], false), Ok(Tier::Vm));
        assert!(Tier::auto(&[Need::Hardware], false).is_err());
        assert!(Tier::auto(&[Need::Kernel, Need::Hardware], false).is_err());
        assert_eq!(Tier::auto(&[], true), Ok(Tier::Nspawn));
        assert_eq!(Tier::auto(&[Need::Kernel], true), Ok(Tier::Metal));
        assert_eq!(Tier::auto(&[Need::Hardware], true), Ok(Tier::Metal));
    }

    #[test]
    fn next_tier_up() {
        assert_eq!(Tier::Nspawn.next(false), Some(Tier::Vm));
        assert_eq!(Tier::Vm.next(false), None);
        assert_eq!(Tier::Nspawn.next(true), Some(Tier::Metal));
        assert_eq!(Tier::Metal.next(true), None);
    }

    #[test]
    fn metal_serializes_lowercase() {
        assert_eq!(serde_json::to_string(&Tier::Metal).unwrap(), "\"metal\"");
        assert_eq!(
            serde_json::to_string(&Need::Hardware).unwrap(),
            "\"hardware\""
        );
    }
}
