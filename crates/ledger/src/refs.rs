//! Checkpoint references, in git's reflog spirit:
//!
//! | Ref | Meaning |
//! | --- | ------- |
//! | `c42`, `c000042` | checkpoint 42 |
//! | `@{0}`, `@{1}` | newest checkpoint, the one before it, ... |
//! | `@{now}`, `now` | the live system |
//! | `@{boot}`, `@{boot-1}` | the newest `boot` checkpoint, the one before it, ... |
//! | `@{ms:1792013726071}` | the newest checkpoint at or before that time (the CLI turns `@{yesterday}` and other `date -d` phrases into this form) |

use osreflog_model::Checkpoint;

use crate::{Ledger, LedgerError};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ref {
    Id(i64),
    Back(u32),
    Now,
    Boot(u32),
    AtMs(i64),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolved {
    Checkpoint(Checkpoint),
    Live,
}

#[derive(Debug, thiserror::Error)]
pub enum RefError {
    #[error(
        "not a checkpoint reference: {0:?} (try c42, @{{1}}, @{{now}}, @{{boot-1}}, @{{yesterday}})"
    )]
    Syntax(String),
    #[error("{0} doesn't name an existing checkpoint")]
    NotFound(String),
    #[error(transparent)]
    Ledger(#[from] LedgerError),
}

/// Parses one reference.
///
/// # Errors
/// If `s` is not one of the forms above.
pub fn parse(s: &str) -> Result<Ref, RefError> {
    let syntax = || RefError::Syntax(s.to_owned());
    let s = s.trim();
    if s == "now" {
        return Ok(Ref::Now);
    }
    if let Some(n) = s.strip_prefix('c') {
        if !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()) {
            return n.parse().map(Ref::Id).map_err(|_| syntax());
        }
        return Err(syntax());
    }
    let inner = s
        .strip_prefix("@{")
        .and_then(|r| r.strip_suffix('}'))
        .ok_or_else(syntax)?;
    if inner == "now" {
        return Ok(Ref::Now);
    }
    if inner == "boot" {
        return Ok(Ref::Boot(0));
    }
    if let Some(n) = inner.strip_prefix("boot-") {
        return n.parse().map(Ref::Boot).map_err(|_| syntax());
    }
    if let Some(ms) = inner.strip_prefix("ms:") {
        return ms.parse().map(Ref::AtMs).map_err(|_| syntax());
    }
    inner.parse().map(Ref::Back).map_err(|_| syntax())
}

/// Finds what a reference points at.
///
/// # Errors
/// If it points at no checkpoint, or on ledger errors.
pub fn resolve(ledger: &Ledger, r: Ref) -> Result<Resolved, RefError> {
    let all = ledger.checkpoints()?;
    let found = match r {
        Ref::Now => return Ok(Resolved::Live),
        Ref::Id(id) => all.into_iter().find(|c| c.id == id),
        Ref::Back(n) => all.into_iter().rev().nth(n as usize),
        Ref::Boot(n) => all
            .into_iter()
            .rev()
            .filter(|c| c.reason == osreflog_model::reason::BOOT)
            .nth(n as usize),
        Ref::AtMs(ms) => all.into_iter().rev().find(|c| c.created_ms <= ms),
    };
    found
        .map(Resolved::Checkpoint)
        .ok_or_else(|| RefError::NotFound(describe(r)))
}

fn describe(r: Ref) -> String {
    match r {
        Ref::Id(id) => format!("c{id}"),
        Ref::Back(n) => format!("@{{{n}}}"),
        Ref::Now => "@{now}".into(),
        Ref::Boot(0) => "@{boot}".into(),
        Ref::Boot(n) => format!("@{{boot-{n}}}"),
        Ref::AtMs(ms) => format!("@{{ms:{ms}}}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::cp;

    #[test]
    fn parsing() {
        assert_eq!(parse("c42").unwrap(), Ref::Id(42));
        assert_eq!(parse("c000042").unwrap(), Ref::Id(42));
        assert_eq!(parse("@{0}").unwrap(), Ref::Back(0));
        assert_eq!(parse("@{12}").unwrap(), Ref::Back(12));
        assert_eq!(parse("@{now}").unwrap(), Ref::Now);
        assert_eq!(parse("now").unwrap(), Ref::Now);
        assert_eq!(parse("@{boot}").unwrap(), Ref::Boot(0));
        assert_eq!(parse("@{boot-2}").unwrap(), Ref::Boot(2));
        assert_eq!(
            parse("@{ms:1792013726071}").unwrap(),
            Ref::AtMs(1_792_013_726_071)
        );
        for bad in [
            "",
            "c",
            "cx",
            "@{",
            "@{-1}",
            "@{yesterday}",
            "HEAD",
            "@{boot-x}",
        ] {
            assert!(parse(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn resolving() {
        let mut l = Ledger::open_in_memory().unwrap();
        l.add_checkpoint(&cp(1, 1000, "boot")).unwrap();
        l.add_checkpoint(&cp(2, 2000, "pre-dnf")).unwrap();
        l.add_checkpoint(&cp(3, 3000, "boot")).unwrap();
        let id = |r: &str| match resolve(&l, parse(r).unwrap()).unwrap() {
            Resolved::Checkpoint(c) => c.id,
            Resolved::Live => -1,
        };
        assert_eq!(id("@{0}"), 3);
        assert_eq!(id("@{2}"), 1);
        assert_eq!(id("c2"), 2);
        assert_eq!(id("@{now}"), -1);
        assert_eq!(id("@{boot}"), 3);
        assert_eq!(id("@{boot-1}"), 1);
        assert_eq!(id("@{ms:2500}"), 2);
        assert!(resolve(&l, parse("@{3}").unwrap()).is_err());
        assert!(resolve(&l, parse("@{ms:10}").unwrap()).is_err());
    }
}
