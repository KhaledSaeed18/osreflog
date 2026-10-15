//! What to put into a clone of the good checkpoint so it shows the state after cause `k`.
//!
//! Each path takes the version left by the last cause `≤ k` that touched it: that cause's
//! blob when it recorded one (exact), else the bad checkpoint's version. The bad
//! checkpoint's version is exact when no later cause in the interval touched the path (it's
//! then what that cause left), and **approximate** otherwise.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

/// One thing a cause did to a path, as the ledger has it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Touch {
    pub path: String,
    /// `create`, `modify`, `delete`, `rename` or `attr`.
    pub op: String,
    /// The old path of a rename.
    pub from: Option<String>,
    pub is_dir: bool,
    /// The content after the change, when the live witness recorded it.
    pub blob: Option<String>,
    /// For a symlink: where it points after the change.
    pub target: Option<String>,
}

/// A cause, with what it touched, in the order the causes happened.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CauseTouches {
    pub id: i64,
    pub touches: Vec<Touch>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "do", rename_all = "snake_case")]
pub enum Action {
    Remove,
    /// Write this blob's content (owner, mode and label from the bad checkpoint if it has
    /// the file, else from the good one, else root's defaults).
    Blob {
        hash: String,
    },
    Symlink {
        target: String,
    },
    Dir,
    /// Copy (reflink) the bad checkpoint's version.
    FromBad,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Op {
    pub path: String,
    #[serde(flatten)]
    pub action: Action,
    pub cause: i64,
    /// False when a later cause also touched the path, so the bad checkpoint's version may
    /// include its edits.
    pub exact: bool,
}

/// The ops for the state after the first `k` causes (`0` is the good checkpoint as is).
/// Removals come first, deepest path first; then directories and files, shallowest first.
#[must_use]
pub fn plan(causes: &[CauseTouches], k: usize) -> Vec<Op> {
    let k = k.min(causes.len());
    // The last index of each cause that touched each path, over the whole interval.
    let mut last_toucher: BTreeMap<&str, usize> = BTreeMap::new();
    for (i, c) in causes.iter().enumerate() {
        for t in &c.touches {
            last_toucher.insert(&t.path, i);
            if let Some(from) = &t.from {
                last_toucher.insert(from, i);
            }
        }
    }
    let mut state: BTreeMap<String, Op> = BTreeMap::new();
    for (i, c) in causes[..k].iter().enumerate() {
        for t in &c.touches {
            if let (true, Some(from)) = (t.op == "rename", &t.from) {
                state.insert(
                    from.clone(),
                    Op {
                        path: from.clone(),
                        action: Action::Remove,
                        cause: c.id,
                        exact: true,
                    },
                );
            }
            let later = last_toucher.get(t.path.as_str()).is_some_and(|&l| l > i);
            let (action, exact) = if t.op == "delete" {
                (Action::Remove, true)
            } else if t.is_dir {
                (Action::Dir, true)
            } else if let Some(target) = &t.target {
                (
                    Action::Symlink {
                        target: target.clone(),
                    },
                    true,
                )
            } else if let Some(hash) = &t.blob {
                (Action::Blob { hash: hash.clone() }, true)
            } else {
                (Action::FromBad, !later)
            };
            // An attribute-only change keeps an earlier content decision for the path.
            if t.op == "attr" && state.contains_key(&t.path) {
                continue;
            }
            state.insert(
                t.path.clone(),
                Op {
                    path: t.path.clone(),
                    action,
                    cause: c.id,
                    exact,
                },
            );
        }
    }
    let depth = |p: &str| p.matches('/').count();
    let (mut removals, mut rest): (Vec<Op>, Vec<Op>) = state
        .into_values()
        .partition(|o| o.action == Action::Remove);
    removals.sort_by(|a, b| {
        depth(&b.path)
            .cmp(&depth(&a.path))
            .then(a.path.cmp(&b.path))
    });
    rest.sort_by(|a, b| {
        depth(&a.path)
            .cmp(&depth(&b.path))
            .then(a.path.cmp(&b.path))
    });
    removals.into_iter().chain(rest).collect()
}

/// The causes whose ops would be approximate in a plan up to `k`.
#[must_use]
pub fn approximations(ops: &[Op]) -> BTreeSet<String> {
    ops.iter()
        .filter(|o| !o.exact)
        .map(|o| o.path.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(path: &str, op: &str, blob: Option<&str>) -> Touch {
        Touch {
            path: path.into(),
            op: op.into(),
            from: None,
            is_dir: false,
            blob: blob.map(Into::into),
            target: None,
        }
    }

    fn causes() -> Vec<CauseTouches> {
        vec![
            CauseTouches {
                id: 10,
                touches: vec![
                    t("/etc/a.conf", "modify", Some("A1")),
                    t("/etc/big.bin", "modify", None),
                    t("/etc/shared.conf", "modify", None),
                ],
            },
            CauseTouches {
                id: 11,
                touches: vec![
                    t("/etc/d", "create", None),
                    t("/etc/d/x.conf", "create", Some("X")),
                    t("/etc/old.conf", "delete", None),
                ],
            },
            CauseTouches {
                id: 12,
                touches: vec![
                    t("/etc/a.conf", "modify", Some("A2")),
                    t("/etc/shared.conf", "modify", None),
                ],
            },
        ]
    }

    #[test]
    fn state_after_k_takes_each_paths_last_version() {
        let c = causes();
        assert_eq!(plan(&c, 0), Vec::new());
        let p1 = plan(&c, 1);
        let a = p1.iter().find(|o| o.path == "/etc/a.conf").unwrap();
        assert_eq!(
            (&a.action, a.exact),
            (&Action::Blob { hash: "A1".into() }, true)
        );
        let big = p1.iter().find(|o| o.path == "/etc/big.bin").unwrap();
        assert!(
            big.exact,
            "no later cause touched it: the bad checkpoint has what #10 left"
        );
        let shared = p1.iter().find(|o| o.path == "/etc/shared.conf").unwrap();
        assert!(!shared.exact, "#12 touched it later: approximate");
        assert_eq!(approximations(&p1).len(), 1);

        let p3 = plan(&c, 3);
        let a = p3.iter().find(|o| o.path == "/etc/a.conf").unwrap();
        assert_eq!(a.action, Action::Blob { hash: "A2".into() });
        assert!(approximations(&p3).is_empty());
    }

    #[test]
    fn removals_first_then_parents_before_children() {
        let mut c = causes();
        c[1].touches[0].is_dir = true;
        let order: Vec<String> = plan(&c, 2).into_iter().map(|o| o.path).collect();
        let pos = |p: &str| order.iter().position(|o| *o == p).unwrap();
        assert_eq!(pos("/etc/old.conf"), 0, "{order:?}");
        assert!(pos("/etc/d") < pos("/etc/d/x.conf"));
    }

    #[test]
    fn a_rename_removes_the_old_name() {
        let c = vec![CauseTouches {
            id: 1,
            touches: vec![Touch {
                from: Some("/etc/a".into()),
                ..t("/etc/b", "rename", Some("B"))
            }],
        }];
        let p = plan(&c, 1);
        assert_eq!(
            p[0],
            Op {
                path: "/etc/a".into(),
                action: Action::Remove,
                cause: 1,
                exact: true
            }
        );
        assert_eq!(p[1].action, Action::Blob { hash: "B".into() });
    }
}
