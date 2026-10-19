//! What the kept snapshots hold on disk, bounded from the bytes each interval released (see
//! [`Space`]). Walks back from the newest checkpoint along stored intervals to the oldest
//! kept snapshot, or as far as intervals that measured it go.

use std::collections::{HashMap, HashSet};

use osreflog_model::{Checkpoint, Space};

/// The bounds, from the checkpoints and every interval's `(from, to, released)`; `None` when
/// no interval since the oldest kept snapshot measured what it released.
#[must_use]
pub fn bounds(checkpoints: &[Checkpoint], released: &[(i64, i64, Option<u64>)]) -> Option<Space> {
    let kept: HashSet<i64> = checkpoints
        .iter()
        .filter(|c| c.pruned_ms.is_none())
        .map(|c| c.id)
        .collect();
    let oldest_kept = *kept.iter().min()?;
    let latest = checkpoints.iter().map(|c| c.id).max()?;
    let names: HashMap<i64, &str> = checkpoints
        .iter()
        .map(|c| (c.id, c.name.as_str()))
        .collect();
    let mut ending: HashMap<i64, Vec<(i64, u64)>> = HashMap::new();
    for &(from, to, bytes) in released {
        if let Some(bytes) = bytes
            && from < to
        {
            ending.entry(to).or_default().push((from, bytes));
        }
    }

    let (mut at_least, mut at_most) = (0u64, 0u64);
    let mut cur = latest;
    while cur > oldest_kept {
        // The closest older end: the finest split of the time between the two.
        let Some(&(from, bytes)) = ending
            .get(&cur)
            .and_then(|v| v.iter().max_by_key(|(f, _)| *f))
        else {
            break;
        };
        if kept.contains(&from) {
            at_least += bytes;
        }
        at_most += bytes;
        cur = from;
    }
    if cur == latest && latest != oldest_kept {
        return None;
    }
    Some(Space {
        at_least,
        at_most,
        since: names
            .get(&cur)
            .map_or_else(String::new, |n| (*n).to_owned()),
        complete: cur <= oldest_kept,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cp(id: i64, kept: bool) -> Checkpoint {
        Checkpoint {
            id,
            name: format!("c{id:06}"),
            created_ms: id * 1000,
            reason: "manual".into(),
            generation: 0,
            boot_id: String::new(),
            note: None,
            pm_pid: None,
            pm_comm: None,
            pinned: false,
            pruned_ms: (!kept).then_some(1),
        }
    }

    #[test]
    fn kept_starts_are_sure_pruned_ones_may_be() {
        // c1 kept, c2 pruned, c3 kept (newest).
        let cps = [cp(1, true), cp(2, false), cp(3, true)];
        let rel = [(1, 2, Some(100)), (2, 3, Some(50))];
        let s = bounds(&cps, &rel).unwrap();
        assert_eq!((s.at_least, s.at_most), (100, 150));
        assert_eq!(s.since, "c000001");
        assert!(s.complete);
    }

    #[test]
    fn stops_at_intervals_from_before_the_measurement() {
        let cps = [cp(1, true), cp(2, true), cp(3, true)];
        let rel = [(1, 2, None), (2, 3, Some(7))];
        let s = bounds(&cps, &rel).unwrap();
        assert_eq!((s.at_least, s.at_most), (7, 7));
        assert_eq!(s.since, "c000002");
        assert!(!s.complete);
    }

    #[test]
    fn nothing_measured_is_unknown_one_snapshot_holds_nothing() {
        let cps = [cp(1, true), cp(2, true)];
        assert_eq!(bounds(&cps, &[(1, 2, None)]), None);
        let one = [cp(1, false), cp(2, true)];
        let s = bounds(&one, &[]).unwrap();
        assert_eq!((s.at_least, s.at_most, s.complete), (0, 0, true));
    }

    #[test]
    fn prefers_the_closest_older_end() {
        let cps = [cp(1, true), cp(2, true), cp(3, true)];
        // A diff asked for by hand (1..3) next to the eager ones.
        let rel = [(1, 3, Some(500)), (1, 2, Some(10)), (2, 3, Some(20))];
        let s = bounds(&cps, &rel).unwrap();
        assert_eq!((s.at_least, s.at_most), (30, 30));
    }
}
