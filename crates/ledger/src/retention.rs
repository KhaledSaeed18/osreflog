//! Which checkpoint snapshots to delete. History is never deleted: a pruned checkpoint keeps
//! its ledger rows and intervals; only its snapshot goes.
//!
//! Kept: pinned checkpoints, the newest one, everything younger than `keep_all_ms`, the newest
//! per UTC day for `daily_days` days, and the newest per week for `weekly_weeks` weeks.

use osreflog_model::Checkpoint;

const DAY_MS: i64 = 86_400_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Policy {
    pub keep_all_ms: i64,
    pub daily_days: i64,
    pub weekly_weeks: i64,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            keep_all_ms: 2 * DAY_MS,
            daily_days: 14,
            weekly_weeks: 8,
        }
    }
}

/// Where the daemon reads its retention from, when the file exists.
pub const CONFIG_PATH: &str = "/etc/osreflog/retention.conf";

impl Policy {
    /// Reads `key = value` lines (`keep_all_hours`, `daily_days`, `weekly_weeks`; `#` starts a
    /// comment) over the defaults.
    ///
    /// # Errors
    /// On an unknown key or a value that isn't a whole number (with its line number).
    pub fn parse(text: &str) -> Result<Self, String> {
        let mut p = Self::default();
        for (n, line) in text.lines().enumerate() {
            let line = line.split('#').next().unwrap_or("").trim();
            if line.is_empty() {
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                return Err(format!("line {}: expected key = value", n + 1));
            };
            let value: i64 = value
                .trim()
                .parse()
                .ok()
                .filter(|v| *v >= 0)
                .ok_or_else(|| format!("line {}: {} is not a whole number", n + 1, value.trim()))?;
            match key.trim() {
                "keep_all_hours" => p.keep_all_ms = value.saturating_mul(3_600_000),
                "daily_days" => p.daily_days = value,
                "weekly_weeks" => p.weekly_weeks = value,
                other => return Err(format!("line {}: unknown setting {other}", n + 1)),
            }
        }
        Ok(p)
    }
}

/// Ids of checkpoints whose snapshots should be deleted now.
#[must_use]
pub fn to_prune(checkpoints: &[Checkpoint], now_ms: i64, policy: Policy) -> Vec<i64> {
    let live: Vec<&Checkpoint> = checkpoints
        .iter()
        .filter(|c| c.pruned_ms.is_none())
        .collect();
    let newest = live.iter().map(|c| c.id).max();
    let mut keep = std::collections::HashSet::new();
    let mut newest_in = std::collections::HashMap::<(u8, i64), &Checkpoint>::new();
    for c in &live {
        let age = now_ms - c.created_ms;
        if c.pinned || Some(c.id) == newest || age < policy.keep_all_ms {
            keep.insert(c.id);
        }
        let day = c.created_ms.div_euclid(DAY_MS);
        if age < policy.daily_days * DAY_MS {
            let slot = newest_in.entry((0, day)).or_insert(c);
            if c.created_ms > slot.created_ms {
                *slot = c;
            }
        }
        if age < policy.weekly_weeks * 7 * DAY_MS {
            let slot = newest_in.entry((1, day.div_euclid(7))).or_insert(c);
            if c.created_ms > slot.created_ms {
                *slot = c;
            }
        }
    }
    keep.extend(newest_in.values().map(|c| c.id));
    let mut out: Vec<i64> = live
        .iter()
        .map(|c| c.id)
        .filter(|id| !keep.contains(id))
        .collect();
    out.sort_unstable();
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::cp;

    #[test]
    fn keeps_recent_daily_weekly_and_pinned() {
        let now = 100 * DAY_MS;
        // Days ago in hundredths, to stay in integers.
        let at = |hundredths: i64| now - hundredths * DAY_MS / 100;
        let mut cps = vec![
            cp(1, at(9000), "boot"),    // older than 8 weeks: pruned
            cp(2, at(4000), "boot"),    // within 8 weeks: weekly keep
            cp(3, at(4020), "boot"),    // same week, older: pruned
            cp(4, at(990), "boot"),     // daily keep (UTC day 90)
            cp(5, at(995), "boot"),     // same UTC day, older: pruned
            cp(6, at(100), "pre-dnf"),  // within 48 h
            cp(7, at(110), "post-dnf"), // within 48 h
            cp(8, at(9500), "manual"),  // pinned
        ];
        cps[7].pinned = true;
        cps.push({
            let mut c = cp(9, at(9900), "boot");
            c.pruned_ms = Some(1);
            c
        });
        assert_eq!(to_prune(&cps, now, Policy::default()), [1, 3, 5]);
    }

    #[test]
    fn newest_is_always_kept() {
        let cps = [cp(1, 0, "boot")];
        assert_eq!(
            to_prune(&cps, 1000 * DAY_MS, Policy::default()),
            Vec::<i64>::new()
        );
    }

    #[test]
    fn the_config_file_overrides_the_defaults() {
        let p = Policy::parse("# fewer snapshots\nkeep_all_hours = 12\n\nweekly_weeks=2 # two\n")
            .unwrap();
        assert_eq!(
            p,
            Policy {
                keep_all_ms: 12 * 3_600_000,
                daily_days: 14,
                weekly_weeks: 2
            }
        );
        assert_eq!(Policy::parse("").unwrap(), Policy::default());
        assert!(
            Policy::parse("daily = 3")
                .unwrap_err()
                .contains("unknown setting daily")
        );
        assert!(
            Policy::parse("\ndaily_days = -1")
                .unwrap_err()
                .starts_with("line 2")
        );
    }
}
