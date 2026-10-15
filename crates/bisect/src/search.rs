//! Binary search for the first bad position, one probe at a time, with skips.
//!
//! Positions are `0..=n`. Position 0 is known good and `n` is taken as bad. The search asks
//! for one position at a time ([`Search::next`]), takes its verdict ([`Search::record`]), and
//! ends when the first bad position is pinned down or every remaining candidate was skipped.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Verdict {
    Good,
    Bad,
    /// The state can't be tested (exit 125, a timeout, a probe that didn't start).
    Skip,
}

impl Verdict {
    /// `git bisect run`'s exit-code contract: 0 good, 125 skip, 1–127 bad, anything else
    /// (a signal, a crash of the harness) skip.
    #[must_use]
    pub fn from_exit(code: i32) -> Self {
        match code {
            0 => Self::Good,
            125 => Self::Skip,
            1..=127 => Self::Bad,
            _ => Self::Skip,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum Outcome {
    /// The first bad position.
    FirstBad { position: usize },
    /// Every position between `last_good` and `first_bad` was skipped: the culprit is one of
    /// `last_good + 1 ..= first_bad`.
    Ambiguous { last_good: usize, first_bad: usize },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Search {
    good: usize,
    bad: usize,
    skipped: BTreeSet<usize>,
    pub probes: Vec<(usize, Verdict)>,
}

impl Search {
    /// A search over `0..=n`: 0 good, `n` bad.
    #[must_use]
    pub fn new(n: usize) -> Self {
        Self {
            good: 0,
            bad: n,
            skipped: BTreeSet::new(),
            probes: Vec::new(),
        }
    }

    /// The next position to probe: the untested candidate nearest the middle, or `None` when
    /// the search is over.
    #[must_use]
    pub fn next(&self) -> Option<usize> {
        let mid = self.good + (self.bad - self.good) / 2;
        (self.good + 1..self.bad)
            .filter(|p| !self.skipped.contains(p))
            .min_by_key(|p| (p.abs_diff(mid), *p))
    }

    pub fn record(&mut self, position: usize, verdict: Verdict) {
        self.probes.push((position, verdict));
        match verdict {
            Verdict::Good if position > self.good && position < self.bad => self.good = position,
            Verdict::Bad if position > self.good && position < self.bad => self.bad = position,
            Verdict::Skip => {
                self.skipped.insert(position);
            }
            _ => {}
        }
    }

    /// The answer, once [`Search::next`] returns `None`.
    #[must_use]
    pub fn outcome(&self) -> Outcome {
        if self.bad - self.good == 1 {
            Outcome::FirstBad { position: self.bad }
        } else {
            Outcome::Ambiguous {
                last_good: self.good,
                first_bad: self.bad,
            }
        }
    }

    #[must_use]
    pub fn bounds(&self) -> (usize, usize) {
        (self.good, self.bad)
    }
}

/// ⌈log₂ n⌉: the probes a search over `n` candidates needs without skips.
#[must_use]
pub fn expected_probes(n: usize) -> usize {
    if n <= 1 {
        0
    } else {
        (usize::BITS - (n - 1).leading_zeros()) as usize
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Runs a search over `0..=n` where positions `>= culprit` are bad and `skips` can't be
    /// tested.
    fn run(n: usize, culprit: usize, skips: &[usize]) -> Search {
        let mut s = Search::new(n);
        while let Some(p) = s.next() {
            let v = if skips.contains(&p) {
                Verdict::Skip
            } else if p >= culprit {
                Verdict::Bad
            } else {
                Verdict::Good
            };
            s.record(p, v);
        }
        s
    }

    #[test]
    fn finds_every_culprit_within_the_bound() {
        for n in 1..=200 {
            for culprit in 1..=n {
                let s = run(n, culprit, &[]);
                assert_eq!(
                    s.outcome(),
                    Outcome::FirstBad { position: culprit },
                    "n={n}"
                );
                assert!(
                    s.probes.len() <= expected_probes(n),
                    "n={n} culprit={culprit}: {} probes",
                    s.probes.len()
                );
            }
        }
    }

    #[test]
    fn skips_are_stepped_around() {
        // A skip away from the culprit costs nothing.
        assert_eq!(
            run(16, 9, &[12]).outcome(),
            Outcome::FirstBad { position: 9 }
        );
        // 8 untestable, 7 good, 9 bad: the culprit is 8 or 9, and the search says so.
        assert_eq!(
            run(16, 9, &[8]).outcome(),
            Outcome::Ambiguous {
                last_good: 7,
                first_bad: 9
            }
        );
        assert_eq!(
            run(16, 9, &[8, 9, 10]).outcome(),
            Outcome::Ambiguous {
                last_good: 7,
                first_bad: 11
            }
        );
    }

    #[test]
    fn exit_codes() {
        assert_eq!(Verdict::from_exit(0), Verdict::Good);
        assert_eq!(Verdict::from_exit(1), Verdict::Bad);
        assert_eq!(Verdict::from_exit(127), Verdict::Bad);
        assert_eq!(Verdict::from_exit(125), Verdict::Skip);
        assert_eq!(Verdict::from_exit(-9), Verdict::Skip);
        assert_eq!(expected_probes(1), 0);
        assert_eq!(expected_probes(2), 1);
        assert_eq!(expected_probes(8), 3);
        assert_eq!(expected_probes(9), 4);
    }
}
