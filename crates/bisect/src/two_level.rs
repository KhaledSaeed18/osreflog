//! The whole bisect as one resumable state machine: the bad end first, then checkpoints, then
//! the causes inside the last good→bad checkpoint interval. It asks for one probe at a time
//! ([`TwoLevel::next`]) and takes that probe's verdict ([`TwoLevel::record`]), so the probe can
//! run in the same call (`bisect run`) or come back reboots later from a person (`bisect
//! start` / `good` / `bad` / `skip`). It is plain data: serialized between steps, it is the
//! session.
//!
//! Checkpoints are indices into the session's list (0 the good end, `n` the bad end); causes
//! are counted (`k` = the state after the first `k` causes of the interval).

use serde::{Deserialize, Serialize};

use crate::search::{Outcome, Search, Verdict};

/// What the machine needs next.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "step", rename_all = "snake_case")]
pub enum Step {
    /// Probe checkpoint `index`. `bad_end`: the first probe, which must fail in this tier.
    Checkpoint {
        index: usize,
        bad_end: bool,
    },
    /// Tell the machine how many causes lie between checkpoints `from` and `to`
    /// ([`TwoLevel::causes`]).
    Causes {
        from: usize,
        to: usize,
    },
    /// Probe the state after the first `k` of the interval's `n` causes, built on `from`.
    AfterCauses {
        from: usize,
        to: usize,
        k: usize,
        n: usize,
    },
    Done(Answer),
}

/// How a bisect ended, in indices; the session turns it into names and causes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "answer", rename_all = "snake_case")]
pub enum Answer {
    /// The test passes on the bad end in this tier.
    NotReproduced,
    /// The bad end couldn't be tested (skip).
    BadEndUntestable,
    /// Every checkpoint between these two was skipped.
    Checkpoints { last_good: usize, first_bad: usize },
    /// The first bad checkpoint, and no recorded cause in its interval.
    NoCauses { from: usize, to: usize },
    /// With every recorded cause replayed the test still passes: something unrecorded broke it.
    NotARecordedCause { from: usize, to: usize },
    /// The `k`-th cause (1-based) of the interval `from..to` is the first bad one.
    Cause { from: usize, to: usize, k: usize },
    /// Every cause probe between these was skipped: the culprit is one of causes
    /// `last_good + 1 ..= first_bad`.
    CauseRange {
        from: usize,
        to: usize,
        last_good: usize,
        first_bad: usize,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "phase", rename_all = "snake_case")]
enum Phase {
    BadEnd,
    Checkpoints {
        search: Search,
    },
    NeedCauses {
        from: usize,
        to: usize,
    },
    Causes {
        from: usize,
        to: usize,
        n: usize,
        search: Search,
        /// "After all causes" was assumed bad; once the search would end on it, it's tested.
        tested_end: bool,
    },
    Done {
        answer: Answer,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TwoLevel {
    /// The bad end's index: checkpoints are `0..=last`.
    last: usize,
    phase: Phase,
    /// Every verdict recorded, with the step it answered.
    pub history: Vec<(Step, Verdict)>,
    /// How many causes the interval had, once [`Self::causes`] said: replaying the history
    /// needs it again.
    #[serde(default)]
    causes: Option<usize>,
}

impl TwoLevel {
    /// A bisect over checkpoints `0..=last` (`last >= 1`): 0 good, `last` the bad end.
    #[must_use]
    pub fn new(last: usize) -> Self {
        Self {
            last: last.max(1),
            phase: Phase::BadEnd,
            history: Vec::new(),
            causes: None,
        }
    }

    #[must_use]
    pub fn next(&self) -> Step {
        match &self.phase {
            Phase::BadEnd => Step::Checkpoint {
                index: self.last,
                bad_end: true,
            },
            Phase::Checkpoints { search } => match search.next() {
                Some(index) => Step::Checkpoint {
                    index,
                    bad_end: false,
                },
                // Only reached when record() didn't move on; kept total for safety.
                None => Step::Done(checkpoint_answer(search)),
            },
            Phase::NeedCauses { from, to } => Step::Causes {
                from: *from,
                to: *to,
            },
            Phase::Causes {
                from,
                to,
                n,
                search,
                tested_end,
            } => match search.next() {
                Some(k) => Step::AfterCauses {
                    from: *from,
                    to: *to,
                    k,
                    n: *n,
                },
                None if search.bounds().1 == *n && !tested_end => Step::AfterCauses {
                    from: *from,
                    to: *to,
                    k: *n,
                    n: *n,
                },
                None => Step::Done(cause_answer(*from, *to, search)),
            },
            Phase::Done { answer } => Step::Done(answer.clone()),
        }
    }

    #[must_use]
    pub fn is_done(&self) -> bool {
        matches!(self.phase, Phase::Done { .. })
    }

    /// The verdict for the probe [`Self::next`] asked for. Ignored when no probe is asked for.
    pub fn record(&mut self, verdict: Verdict) {
        let step = self.next();
        if !matches!(step, Step::Checkpoint { .. } | Step::AfterCauses { .. }) {
            return;
        }
        self.history.push((step.clone(), verdict));
        self.phase = match (std::mem::replace(&mut self.phase, Phase::BadEnd), step) {
            (Phase::BadEnd, _) => match verdict {
                Verdict::Good => done(Answer::NotReproduced),
                Verdict::Skip => done(Answer::BadEndUntestable),
                Verdict::Bad => after_checkpoints(Search::new(self.last)),
            },
            (Phase::Checkpoints { mut search }, Step::Checkpoint { index, .. }) => {
                search.record(index, verdict);
                after_checkpoints(search)
            }
            (
                Phase::Causes {
                    from,
                    to,
                    n,
                    mut search,
                    tested_end,
                },
                Step::AfterCauses { k, .. },
            ) => {
                let end = k == n && search.next().is_none();
                if end && verdict == Verdict::Good {
                    done(Answer::NotARecordedCause { from, to })
                } else {
                    search.record(k, verdict);
                    let phase = Phase::Causes {
                        from,
                        to,
                        n,
                        search,
                        tested_end: tested_end || end,
                    };
                    settle(phase)
                }
            }
            (phase, _) => phase,
        };
    }

    /// Takes back the last verdict (a person's mistake): the search is rebuilt from the verdicts
    /// before it, so [`Self::next`] asks for that probe again. Returns what was taken back;
    /// `None` when there is no verdict yet. Works on a finished search too.
    pub fn take_back(&mut self) -> Option<(Step, Verdict)> {
        // The cause count: kept since it was given, or read off any cause probe in the history
        // (a session saved before the count was kept has only that).
        let causes = self.causes.or_else(|| {
            self.history.iter().find_map(|(step, _)| match step {
                Step::AfterCauses { n, .. } => Some(*n),
                _ => None,
            })
        });
        let last = self.history.pop()?;
        let mut m = TwoLevel::new(self.last);
        for (step, verdict) in &self.history {
            if let (Step::Causes { .. }, Some(n)) = (m.next(), causes) {
                m.causes(n);
            }
            debug_assert_eq!(
                &m.next(),
                step,
                "replaying the verdicts asked a different step"
            );
            m.record(*verdict);
        }
        if let (Step::Causes { .. }, Some(n)) = (m.next(), causes) {
            m.causes(n);
        }
        // The cause count stays known even if the replay stopped before asking for it.
        m.causes = causes;
        *self = m;
        Some(last)
    }

    /// The number of causes between the checkpoints [`Step::Causes`] named.
    pub fn causes(&mut self, n: usize) {
        if let Phase::NeedCauses { from, to } = self.phase {
            self.causes = Some(n);
            self.phase = if n == 0 {
                done(Answer::NoCauses { from, to })
            } else {
                settle(Phase::Causes {
                    from,
                    to,
                    n,
                    search: Search::new(n),
                    tested_end: false,
                })
            };
        }
    }
}

fn done(answer: Answer) -> Phase {
    Phase::Done { answer }
}

fn checkpoint_answer(search: &Search) -> Answer {
    match search.outcome() {
        // A first bad checkpoint means causes are needed; next() never reports that as done.
        Outcome::FirstBad { position } => Answer::NoCauses {
            from: position - 1,
            to: position,
        },
        Outcome::Ambiguous {
            last_good,
            first_bad,
        } => Answer::Checkpoints {
            last_good,
            first_bad,
        },
    }
}

fn after_checkpoints(search: Search) -> Phase {
    if search.next().is_some() {
        return Phase::Checkpoints { search };
    }
    match search.outcome() {
        Outcome::FirstBad { position } => Phase::NeedCauses {
            from: position - 1,
            to: position,
        },
        Outcome::Ambiguous {
            last_good,
            first_bad,
        } => done(Answer::Checkpoints {
            last_good,
            first_bad,
        }),
    }
}

/// A cause phase that has nothing left to ask becomes its answer.
fn settle(phase: Phase) -> Phase {
    if let Phase::Causes {
        from,
        to,
        n,
        search,
        tested_end,
    } = &phase
        && search.next().is_none()
        && (search.bounds().1 != *n || *tested_end)
    {
        return done(cause_answer(*from, *to, search));
    }
    phase
}

fn cause_answer(from: usize, to: usize, search: &Search) -> Answer {
    match search.outcome() {
        Outcome::FirstBad { position } => Answer::Cause {
            from,
            to,
            k: position,
        },
        Outcome::Ambiguous {
            last_good,
            first_bad,
        } => Answer::CauseRange {
            from,
            to,
            last_good,
            first_bad,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Runs a whole bisect against oracles: checkpoint `i` is bad from `cp_bad` on, and inside
    /// the interval the state after `k` causes is bad from `cause_bad` on (`None`: never).
    /// Every step goes through a JSON round trip, as a session resumed after a reboot does.
    fn drive(
        last: usize,
        cp_bad: usize,
        causes: usize,
        cause_bad: Option<usize>,
    ) -> (Answer, usize) {
        let mut m = TwoLevel::new(last);
        let mut probes = 0;
        loop {
            m = serde_json::from_str(&serde_json::to_string(&m).unwrap()).unwrap();
            match m.next() {
                Step::Checkpoint { index, .. } => {
                    probes += 1;
                    m.record(if index >= cp_bad {
                        Verdict::Bad
                    } else {
                        Verdict::Good
                    });
                }
                Step::Causes { from, to } => {
                    assert_eq!((from, to), (cp_bad - 1, cp_bad));
                    m.causes(causes);
                }
                Step::AfterCauses { k, n, .. } => {
                    assert_eq!(n, causes);
                    probes += 1;
                    let bad = cause_bad.is_some_and(|b| k >= b);
                    m.record(if bad { Verdict::Bad } else { Verdict::Good });
                }
                Step::Done(a) => return (a, probes),
            }
        }
    }

    fn log2_ceil(n: usize) -> usize {
        if n <= 1 {
            0
        } else {
            (usize::BITS - (n - 1).leading_zeros()) as usize
        }
    }

    #[test]
    fn finds_every_cause_within_the_bound() {
        for last in 1..=12 {
            for cp_bad in 1..=last {
                for causes in 1..=12 {
                    for cause_bad in 1..=causes {
                        let (a, probes) = drive(last, cp_bad, causes, Some(cause_bad));
                        assert_eq!(
                            a,
                            Answer::Cause {
                                from: cp_bad - 1,
                                to: cp_bad,
                                k: cause_bad
                            },
                            "last {last} cp_bad {cp_bad} causes {causes} cause_bad {cause_bad}"
                        );
                        // The bad end, the checkpoint search, the cause search, and at most
                        // one probe of "after all causes".
                        assert!(
                            probes <= 1 + log2_ceil(last) + log2_ceil(causes) + 1,
                            "{probes}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn ends_without_a_cause_when_it_should() {
        // No causes in the interval.
        assert_eq!(drive(4, 3, 0, None).0, Answer::NoCauses { from: 2, to: 3 });
        // Every cause replayed and still good.
        assert_eq!(
            drive(4, 3, 5, None).0,
            Answer::NotARecordedCause { from: 2, to: 3 }
        );
    }

    #[test]
    fn a_verdict_taken_back_is_asked_again() {
        // The P8d slip: three causes, probe 2 (after 1 cause) wrongly said bad.
        let mut m = TwoLevel::new(1);
        m.record(Verdict::Bad);
        m.causes(3);
        let asked = m.next();
        assert!(matches!(
            asked,
            Step::AfterCauses { k: 1, .. } | Step::AfterCauses { k: 2, .. }
        ));
        m.record(if matches!(asked, Step::AfterCauses { k: 1, .. }) {
            Verdict::Bad
        } else {
            Verdict::Good
        });
        let wrong_answer = m.next();
        // Take it back: the same probe is asked again, and the right verdict finds the cause.
        let back = m.take_back().unwrap();
        assert_eq!(back.0, asked);
        assert_eq!(m.next(), asked);
        assert_eq!(m.history.len(), 1);
        let json = serde_json::to_string(&m).unwrap();
        let mut m: TwoLevel = serde_json::from_str(&json).unwrap();
        m.record(if matches!(asked, Step::AfterCauses { k: 1, .. }) {
            Verdict::Good
        } else {
            Verdict::Bad
        });
        while let Step::AfterCauses { k, .. } = m.next() {
            m.record(if k >= 2 { Verdict::Bad } else { Verdict::Good });
        }
        assert_eq!(
            m.next(),
            Step::Done(Answer::Cause {
                from: 0,
                to: 1,
                k: 2
            })
        );
        assert_ne!(m.next(), wrong_answer);

        // Back to the very start, through the cause count.
        while m.take_back().is_some() {}
        assert_eq!(
            m.next(),
            Step::Checkpoint {
                index: 1,
                bad_end: true
            }
        );
        assert!(m.take_back().is_none());
    }

    /// The P8d session on the laptop: saved before the cause count was kept, so its JSON has
    /// no `causes`. Taking back probe 3 must ask after #780 again, not lose the good verdict for
    /// #779 and ask that one.
    #[test]
    fn a_session_saved_without_the_cause_count_takes_back_right() {
        let mut m = TwoLevel::new(1);
        m.record(Verdict::Bad);
        m.causes(3);
        m.record(Verdict::Good); // after 1 of 3
        m.record(Verdict::Bad); // after 2 of 3
        assert_eq!(
            m.next(),
            Step::Done(Answer::Cause {
                from: 0,
                to: 1,
                k: 2
            })
        );
        let mut json: serde_json::Value = serde_json::to_value(&m).unwrap();
        json.as_object_mut().unwrap().remove("causes");
        let mut old: TwoLevel = serde_json::from_value(json).unwrap();
        let (step, _) = old.take_back().unwrap();
        assert_eq!(
            step,
            Step::AfterCauses {
                from: 0,
                to: 1,
                k: 2,
                n: 3
            }
        );
        assert_eq!(old.next(), step);
        assert_eq!(old.history.len(), 2, "the good verdict for #779 is kept");
    }

    #[test]
    fn taking_back_every_verdict_replays_any_search() {
        // For every culprit position: answer right, take everything back one by one, and each
        // time the step asked again is the one that verdict answered.
        for last in 1..=6 {
            for cp_bad in 1..=last {
                for causes in 0..=5 {
                    let mut m = TwoLevel::new(last);
                    let mut steps = Vec::new();
                    loop {
                        match m.next() {
                            Step::Checkpoint { index, .. } => {
                                steps.push(m.next());
                                m.record(if index >= cp_bad {
                                    Verdict::Bad
                                } else {
                                    Verdict::Good
                                });
                            }
                            Step::Causes { .. } => m.causes(causes),
                            Step::AfterCauses { k, .. } => {
                                steps.push(m.next());
                                m.record(if k >= causes.max(1) {
                                    Verdict::Bad
                                } else {
                                    Verdict::Good
                                });
                            }
                            Step::Done(_) => break,
                        }
                    }
                    while let Some((step, _)) = m.take_back() {
                        assert_eq!(Some(&step), steps.last());
                        assert_eq!(m.next(), step);
                        steps.pop();
                    }
                    assert_eq!(steps, Vec::<Step>::new());
                }
            }
        }
    }

    #[test]
    fn the_bad_end_decides_first() {
        let mut m = TwoLevel::new(5);
        assert_eq!(
            m.next(),
            Step::Checkpoint {
                index: 5,
                bad_end: true
            }
        );
        m.record(Verdict::Good);
        assert_eq!(m.next(), Step::Done(Answer::NotReproduced));
        let mut m = TwoLevel::new(5);
        m.record(Verdict::Skip);
        assert_eq!(m.next(), Step::Done(Answer::BadEndUntestable));
        assert!(m.is_done());
        m.record(Verdict::Bad); // nothing asked: ignored
        assert_eq!(m.history.len(), 1);
    }

    #[test]
    fn skips_give_ranges() {
        // Checkpoints: bad end bad, then every inner checkpoint skipped.
        let mut m = TwoLevel::new(4);
        m.record(Verdict::Bad);
        while let Step::Checkpoint { .. } = m.next() {
            m.record(Verdict::Skip);
        }
        assert_eq!(
            m.next(),
            Step::Done(Answer::Checkpoints {
                last_good: 0,
                first_bad: 4
            })
        );

        // Causes: two checkpoints, then every cause probe skipped but "after all" bad.
        let mut m = TwoLevel::new(1);
        m.record(Verdict::Bad);
        assert_eq!(m.next(), Step::Causes { from: 0, to: 1 });
        m.causes(4);
        loop {
            match m.next() {
                Step::AfterCauses { k, n, .. } => {
                    m.record(if k == n { Verdict::Bad } else { Verdict::Skip });
                }
                Step::Done(a) => {
                    assert_eq!(
                        a,
                        Answer::CauseRange {
                            from: 0,
                            to: 1,
                            last_good: 0,
                            first_bad: 4
                        }
                    );
                    break;
                }
                s => panic!("{s:?}"),
            }
        }
    }

    #[test]
    fn a_single_interval_goes_straight_to_causes() {
        let mut m = TwoLevel::new(1);
        m.record(Verdict::Bad);
        assert_eq!(m.next(), Step::Causes { from: 0, to: 1 });
        m.causes(1);
        // One cause: the search has no inner position, so "after all causes" is tested.
        assert_eq!(
            m.next(),
            Step::AfterCauses {
                from: 0,
                to: 1,
                k: 1,
                n: 1
            }
        );
        m.record(Verdict::Bad);
        assert_eq!(
            m.next(),
            Step::Done(Answer::Cause {
                from: 0,
                to: 1,
                k: 1
            })
        );
    }
}
