//! Packages inside a snapshot: the rpm database and dnf5's transaction history, both read
//! from copies (SQLite needs to write `-shm`, and snapshots are read-only), and the diff
//! between two package sets.

pub mod header;
pub mod vercmp;

use std::collections::BTreeMap;
use std::path::Path;

use osreflog_model::{PackageChange, Transaction};
use rusqlite::{Connection, OpenFlags};

pub use header::Package;

/// Where the databases live inside a Fedora root.
pub const RPMDB_DIR: &str = "usr/lib/sysimage/rpm";
pub const RPMDB_FILE: &str = "rpmdb.sqlite";
pub const HISTORY_DIR: &str = "usr/lib/sysimage/libdnf5";
pub const HISTORY_FILE: &str = "transaction_history.sqlite";

#[derive(Debug, thiserror::Error)]
pub enum RpmError {
    #[error("rpm header: {0}")]
    Header(&'static str),
    #[error("copying {path}: {source}")]
    Copy {
        path: String,
        source: std::io::Error,
    },
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("{0}")]
    Io(#[from] std::io::Error),
}

/// Copies `dir/file` and its `-wal` into `scratch` and opens the copy read-only. On btrfs the
/// copy is a reflink. The WAL must come along: it may hold the latest transactions.
fn open_copy(dir: &Path, file: &str, scratch: &Path) -> Result<Connection, RpmError> {
    for suffix in ["", "-wal"] {
        let name = format!("{file}{suffix}");
        let src = dir.join(&name);
        if suffix.is_empty() || src.exists() {
            std::fs::copy(&src, scratch.join(&name)).map_err(|source| RpmError::Copy {
                path: src.display().to_string(),
                source,
            })?;
        }
    }
    // Read-write open of the private copy, so SQLite can replay the WAL; nothing is written
    // back to the snapshot.
    let conn = Connection::open_with_flags(
        scratch.join(file),
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    Ok(conn)
}

/// Every installed package recorded in the rpmdb under `root` (a snapshot or `/`).
///
/// # Errors
/// If the database can't be copied or read, or a header doesn't parse.
pub fn packages(root: &Path, scratch: &Path) -> Result<Vec<Package>, RpmError> {
    let tmp = tempfile::tempdir_in(scratch)?;
    let conn = open_copy(&root.join(RPMDB_DIR), RPMDB_FILE, tmp.path())?;
    let mut stmt = conn.prepare("SELECT blob FROM Packages")?;
    let mut rows = stmt.query([])?;
    let mut out = Vec::new();
    while let Some(row) = rows.next()? {
        let blob: Vec<u8> = row.get(0)?;
        out.push(header::parse(&blob)?);
    }
    out.sort();
    Ok(out)
}

/// dnf5 transactions under `root` that had not ended before `from_s` and had begun by `to_s`
/// (Unix seconds, as dnf records them). Second resolution makes this a superset at the
/// edges; callers narrow it with the older snapshot's own history when they can.
///
/// # Errors
/// If the history database can't be copied or read. A root without one yields no
/// transactions.
pub fn transactions(
    root: &Path,
    scratch: &Path,
    from_s: i64,
    to_s: i64,
) -> Result<Vec<Transaction>, RpmError> {
    let dir = root.join(HISTORY_DIR);
    if !dir.join(HISTORY_FILE).exists() {
        return Ok(Vec::new());
    }
    let tmp = tempfile::tempdir_in(scratch)?;
    let conn = open_copy(&dir, HISTORY_FILE, tmp.path())?;
    let mut stmt = conn.prepare(
        "SELECT t.id, t.dt_begin, t.dt_end, t.user_id, COALESCE(t.description, ''), COALESCE(s.name, '')
         FROM trans t LEFT JOIN trans_state s ON s.id = t.state_id
         WHERE (t.dt_end IS NULL OR t.dt_end >= ?1) AND t.dt_begin <= ?2 ORDER BY t.id",
    )?;
    let rows = stmt.query_map([from_s, to_s], |r| {
        Ok(Transaction {
            id: r.get(0)?,
            begin: r.get(1)?,
            end: r.get(2)?,
            user_id: r.get(3)?,
            description: r.get(4)?,
            state: r.get(5)?,
        })
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// What changed between two package sets. Per name and arch, removed and added versions are
/// paired oldest-first into upgrades or downgrades; the rest are installs or removals (so a
/// new kernel next to the old ones is an install).
#[must_use]
pub fn diff(old: &[Package], new: &[Package]) -> Vec<PackageChange> {
    type Key = (String, String);
    let index = |pkgs: &[Package]| {
        let mut m: BTreeMap<Key, Vec<Package>> = BTreeMap::new();
        for p in pkgs {
            m.entry((p.name.clone(), p.arch.clone()))
                .or_default()
                .push(p.clone());
        }
        m
    };
    let (old, new) = (index(old), index(new));
    let mut keys: Vec<&Key> = old.keys().chain(new.keys()).collect();
    keys.sort();
    keys.dedup();

    let cmp = |a: &Package, b: &Package| {
        vercmp::evrcmp(
            (a.epoch, &a.version, &a.release),
            (b.epoch, &b.version, &b.release),
        )
    };
    let mut out = Vec::new();
    for key in keys {
        let before = old.get(key).map_or(&[][..], Vec::as_slice);
        let after = new.get(key).map_or(&[][..], Vec::as_slice);
        let mut removed: Vec<&Package> = before.iter().filter(|p| !after.contains(p)).collect();
        let mut added: Vec<&Package> = after.iter().filter(|p| !before.contains(p)).collect();
        removed.sort_by(|a, b| cmp(a, b));
        added.sort_by(|a, b| cmp(a, b));
        let change = |action: &str, from: Option<&Package>, to: Option<&Package>| PackageChange {
            name: key.0.clone(),
            arch: key.1.clone(),
            action: action.to_owned(),
            from_evr: from.map(Package::evr),
            to_evr: to.map(Package::evr),
        };
        // Pair only when the counts match; otherwise it's installonly (kernels) or a
        // multilib shuffle, better shown as installs and removals.
        if removed.len() == added.len() {
            for (r, a) in removed.iter().zip(&added) {
                let action = if cmp(a, r).is_gt() {
                    "upgrade"
                } else {
                    "downgrade"
                };
                out.push(change(action, Some(r), Some(a)));
            }
        } else {
            out.extend(removed.iter().map(|r| change("remove", Some(r), None)));
            out.extend(added.iter().map(|a| change("install", None, Some(a))));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(name: &str, version: &str) -> Package {
        Package {
            name: name.into(),
            epoch: None,
            version: version.into(),
            release: "1.fc44".into(),
            arch: "x86_64".into(),
        }
    }

    fn actions(cs: &[PackageChange]) -> Vec<String> {
        cs.iter()
            .map(|c| format!("{} {} {:?}->{:?}", c.action, c.name, c.from_evr, c.to_evr))
            .collect()
    }

    #[test]
    fn install_remove_upgrade_downgrade() {
        let old = [p("a", "1"), p("b", "2"), p("c", "3")];
        let new = [p("a", "2"), p("b", "1"), p("d", "1")];
        assert_eq!(
            actions(&diff(&old, &new)),
            [
                "upgrade a Some(\"1-1.fc44\")->Some(\"2-1.fc44\")",
                "downgrade b Some(\"2-1.fc44\")->Some(\"1-1.fc44\")",
                "remove c Some(\"3-1.fc44\")->None",
                "install d None->Some(\"1-1.fc44\")",
            ]
        );
    }

    #[test]
    fn installonly_kernel_is_install_plus_remove() {
        let old = [
            p("kernel", "7.1.6"),
            p("kernel", "7.2.8"),
            p("kernel", "7.2.9"),
        ];
        let new = [
            p("kernel", "7.2.8"),
            p("kernel", "7.2.9"),
            p("kernel", "7.2.10"),
        ];
        // Same count: paired as an upgrade from the removed one to the added one.
        assert_eq!(
            actions(&diff(&old, &new)),
            ["upgrade kernel Some(\"7.1.6-1.fc44\")->Some(\"7.2.10-1.fc44\")"]
        );
        let new = [
            p("kernel", "7.1.6"),
            p("kernel", "7.2.8"),
            p("kernel", "7.2.9"),
            p("kernel", "7.2.10"),
        ];
        assert_eq!(
            actions(&diff(&old, &new)),
            ["install kernel None->Some(\"7.2.10-1.fc44\")"]
        );
    }

    #[test]
    fn unchanged_sets_have_no_diff() {
        let set = [p("a", "1")];
        assert_eq!(diff(&set, &set), Vec::<PackageChange>::new());
    }
}
