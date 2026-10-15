//! The undo engine. A [`Plan`] lists, per path, what a cause left there (A) and what was
//! there before (B). [`apply`] checks each path's current state (C) against both: `C == B`
//! is already undone, `C != A` is a conflict and is left alone, and otherwise B goes back,
//! through a race-checked swap. What it removes goes to the attic; nothing is deleted.
//!
//! See `docs/undo.md` and the P5 journal for the rules and the test matrix.

pub mod apply;
pub mod plan;

pub use apply::{Outcome, Report, Root, StepReport, apply, current};
pub use plan::{Meta, Plan, State, Step, Version};
