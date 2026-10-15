//! What `osreflog undo` shows before anything is applied, and what it reports after.

use osreflog_model::{Cause, Checkpoint};
use serde::{Deserialize, Serialize};

use crate::apply::Report;
use crate::plan::{Plan, State};

/// What undo would do with one path, judged against the current state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "verdict", rename_all = "snake_case")]
pub enum Verdict {
    Restore,
    AlreadyUndone,
    Conflict,
    Refused { reason: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreviewStep {
    pub path: String,
    /// A: what the cause left.
    pub expect: State,
    /// B: what undo puts back.
    pub restore: State,
    /// C: what's there now (or why it couldn't be read).
    pub current: Result<State, String>,
    #[serde(flatten)]
    pub verdict: Verdict,
    /// Current and restored content, for text files, so the client can show a diff.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub restore_text: Option<String>,
}

/// A path of the cause that undo leaves alone, and why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Skipped {
    pub path: String,
    pub reason: String,
}

/// Work osreflog hands to the tool that owns it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "tool", rename_all = "lowercase")]
pub enum Delegation {
    /// A package transaction: `dnf5 history undo <transaction>`.
    Dnf {
        transaction: i64,
        description: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Preview {
    pub cause: Cause,
    /// BLAKE3 of the plan: `--apply` passes it back, so exactly what was shown is applied.
    pub digest: String,
    pub steps: Vec<PreviewStep>,
    pub skipped: Vec<Skipped>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delegated: Option<Delegation>,
}

impl Preview {
    #[must_use]
    pub fn to_restore(&self) -> usize {
        self.steps
            .iter()
            .filter(|s| s.verdict == Verdict::Restore)
            .count()
    }
}

/// The result of an applied undo.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Applied {
    /// Taken and verified before anything was written.
    pub pre_undo: Checkpoint,
    pub report: Report,
    /// Whether systemd was reloaded because unit files or enablement links changed.
    pub reloaded_systemd: bool,
    /// The cause the live witness recorded for this undo (undo it to redo the change).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub undo_cause: Option<i64>,
}

/// The digest of a plan, content included.
#[must_use]
pub fn digest(plan: &Plan) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(&plan.undo_of.to_le_bytes());
    for step in &plan.steps {
        hasher.update(step.path.as_bytes());
        hasher.update(&[0]);
        hasher.update(format!("{:?}\0{:?}\0", step.expect, step.restore).as_bytes());
    }
    hasher.finalize().to_hex().to_string()
}
