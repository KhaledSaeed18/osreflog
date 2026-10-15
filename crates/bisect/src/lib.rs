//! Bisect, the pure part. [`search`] finds the first bad position over checkpoints or causes,
//! one probe at a time; [`replay`] says how to build the state after the first `k` causes of
//! an interval from a clone of its good checkpoint. The daemon does the I/O.

pub mod replay;
pub mod search;
pub mod session;

pub use replay::{Action, CauseTouches, Op, Touch};
pub use search::{Outcome, Search, Verdict, expected_probes};
pub use session::{Args, Culprit, Finding, ProbeRecord, Status, Tier};
