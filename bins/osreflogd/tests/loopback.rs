//! End-to-end engine test on a throwaway btrfs filesystem. Root only, never on a real disk:
//! `scripts/p2/it.sh` creates a loopback image, mounts its `root` subvolume, and runs this
//! binary with `--ignored` inside a private mount namespace. Without
//! `OSREFLOG_LOOPBACK_ROOT` every test here is a no-op.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::too_many_lines)]

use std::fs;
use std::os::fd::AsFd;
use std::path::{Path, PathBuf};

use osreflog_btrfs::ioctl;
use osreflog_ledger::Ledger;
use osreflog_ledger::retention::Policy;
use osreflog_model::{Class, Endpoint};
use osreflogd::engine::Engine;
use osreflogd::layout::Layout;
use rusqlite::Connection;

fn root() -> Option<PathBuf> {
    std::env::var_os("OSREFLOG_LOOPBACK_ROOT").map(PathBuf::from)
}

fn write(root: &Path, rel: &str, content: &str) {
    let p = root.join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, content).unwrap();
}

fn rpmdb(root: &Path, pkgs: &[(&str, &str)]) {
    let dir = root.join("usr/lib/sysimage/rpm");
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join("rpmdb.sqlite");
    let _ = fs::remove_file(&path);
    let c = Connection::open(&path).unwrap();
    c.execute_batch("CREATE TABLE Packages (hnum INTEGER PRIMARY KEY, blob BLOB NOT NULL)")
        .unwrap();
    for (name, version) in pkgs {
        let blob = osreflog_rpm::header::build(name, None, version, "1.fc44", "x86_64");
        c.execute("INSERT INTO Packages (blob) VALUES (?1)", [blob])
            .unwrap();
    }
}

fn history(root: &Path, rows: &[(i64, i64, &str)]) {
    let dir = root.join("usr/lib/sysimage/libdnf5");
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join("transaction_history.sqlite");
    let c = Connection::open(&path).unwrap();
    c.execute_batch(
        "CREATE TABLE IF NOT EXISTS trans_state (id INTEGER PRIMARY KEY, name TEXT);
         INSERT OR IGNORE INTO trans_state VALUES (2, 'Ok');
         CREATE TABLE IF NOT EXISTS trans (id INTEGER PRIMARY KEY, dt_begin INTEGER NOT NULL, dt_end INTEGER,
            rpmdb_version_begin TEXT, rpmdb_version_end TEXT, releasever TEXT NOT NULL, user_id INTEGER NOT NULL,
            description TEXT, comment TEXT, state_id INTEGER)",
    )
    .unwrap();
    for (id, at, desc) in rows {
        c.execute(
            "INSERT OR REPLACE INTO trans (id, dt_begin, dt_end, releasever, user_id, description, state_id) VALUES (?1, ?2, ?2, '44', 1000, ?3, 2)",
            rusqlite::params![id, at, desc],
        )
        .unwrap();
    }
}

const MIB: usize = 1 << 20;

fn now_s() -> i64 {
    osreflogd::engine::now_ms() / 1000
}

#[test]
#[ignore = "root only: run through scripts/p2/it.sh"]
fn checkpoints_diffs_packages_and_pruning() {
    let Some(root) = root() else { return };
    let top =
        PathBuf::from(std::env::var_os("OSREFLOG_LOOPBACK_TOP").expect("OSREFLOG_LOOPBACK_TOP"));

    // A small "system".
    write(&root, "etc/hosts", "127.0.0.1 localhost\n");
    write(&root, "etc/old.conf", "gone soon\n");
    write(&root, "opt/app/config", "a=1\n");
    write(&root, "usr/bin/tool", "v1\n");
    write(&root, "var/log/dnf5.log", "start\n");
    rpmdb(&root, &[("bash", "5.3.9"), ("figlet", "2.2.5")]);
    history(&root, &[(1, 1_000, "dnf install bash")]);
    // For the space report (noise paths, so the default listing below doesn't show them).
    fs::create_dir_all(root.join("var/cache/it")).unwrap();
    fs::write(root.join("var/cache/it/big.bin"), vec![7u8; 4 * MIB]).unwrap();
    fs::write(root.join("var/cache/it/db.bin"), vec![1u8; 2 * MIB]).unwrap();

    let layout = Layout::open(&root, &top).unwrap();
    let ledger = Ledger::open(&layout.ledger_path()).unwrap();
    let mut engine = Engine::new(layout, ledger).unwrap();

    let c1 = engine
        .checkpoint("pre-dnf", None, Some(std::process::id()))
        .unwrap();
    assert_eq!(c1.name, "c000001");

    // A "transaction": config edits, a package upgrade, an install, a removal.
    write(&root, "etc/hosts", "127.0.0.1 localhost\n10.0.0.2 nas\n");
    write(&root, "etc/new.conf", "x=1\n");
    fs::remove_file(root.join("etc/old.conf")).unwrap();
    fs::rename(root.join("opt/app"), root.join("opt/app2")).unwrap();
    write(&root, "usr/bin/tool", "v2\n");
    write(&root, "var/log/dnf5.log", "start\nmore\n");
    rpmdb(&root, &[("bash", "5.3.10"), ("hyperfine", "1.20")]);
    history(&root, &[(2, now_s(), "dnf upgrade bash")]);
    // 4 MiB deleted, 1 MiB overwritten in place, 1 MiB appended: 5 MiB only c1 holds.
    fs::remove_file(root.join("var/cache/it/big.bin")).unwrap();
    {
        use std::io::{Seek, SeekFrom, Write};
        let mut f = fs::OpenOptions::new()
            .write(true)
            .open(root.join("var/cache/it/db.bin"))
            .unwrap();
        f.write_all(&vec![2u8; MIB]).unwrap();
        f.seek(SeekFrom::End(0)).unwrap();
        f.write_all(&vec![3u8; MIB]).unwrap();
        f.sync_all().unwrap();
    }

    let c2 = engine.checkpoint("post-dnf", None, None).unwrap();
    assert_eq!(engine.compute_pending().unwrap(), 1);
    let space = engine.status().unwrap().space.expect("measured");
    eprintln!("space: {space:?}");
    assert!(
        space.complete && space.at_least == space.at_most,
        "{space:?}"
    );
    let five = 5 * MIB as u64;
    assert!(
        (five..five + 100 * 1024).contains(&space.at_least),
        "c1 alone holds 5 MiB and a few small files, not {} bytes",
        space.at_least
    );

    let r = engine.diff("@{1}", "@{0}", false).unwrap();
    assert!(r.cached);
    let shown: Vec<(String, String)> = r
        .changes
        .iter()
        .map(|c| (c.op.clone(), c.path.clone()))
        .collect();
    eprintln!("{shown:#?}");
    for want in [
        ("modify", "/etc/hosts"),
        ("create", "/etc/new.conf"),
        ("delete", "/etc/old.conf"),
        ("rename", "/opt/app2"),
    ] {
        assert!(
            shown.contains(&(want.0.to_owned(), want.1.to_owned())),
            "missing {want:?}"
        );
    }
    assert!(
        !shown.contains(&("modify".to_owned(), "/opt".to_owned())),
        "the renamed directory's parent only had its times changed"
    );
    assert!(
        r.changes
            .iter()
            .all(|c| !matches!(c.class, Class::Package | Class::Noise))
    );
    assert!(
        r.hidden >= 2,
        "usr/bin/tool and the log are hidden by default"
    );
    let pk: Vec<String> = r
        .packages
        .iter()
        .map(|p| format!("{} {}", p.action, p.name))
        .collect();
    assert_eq!(pk, ["upgrade bash", "remove figlet", "install hyperfine"]);
    assert_eq!(r.transactions.len(), 1, "{:?}", r.transactions);
    assert_eq!(r.transactions[0].description, "dnf upgrade bash");

    let all = engine.diff("c1", "c2", true).unwrap();
    assert!(
        all.changes
            .iter()
            .any(|c| c.path == "/usr/bin/tool" && c.class == Class::Package)
    );

    // Since the newest checkpoint, against the live system.
    write(&root, "etc/hosts", "changed again\n");
    let live = engine.diff("@{0}", "@{now}", false).unwrap();
    assert!(matches!(live.to, Endpoint::Live { .. }));
    assert_eq!(
        live.changes
            .iter()
            .map(|c| c.path.as_str())
            .collect::<Vec<_>>(),
        ["/etc/hosts"]
    );
    assert!(live.packages.is_empty(), "rpmdb unchanged since c2");

    // Retention that keeps only the newest: c1's snapshot goes, its history stays.
    let _c3 = engine
        .checkpoint("manual", Some("before pruning".into()), None)
        .unwrap();
    engine.policy = Policy {
        keep_all_ms: 0,
        daily_days: 0,
        weekly_weeks: 0,
    };
    engine.compute_pending().unwrap();
    let log = engine.log(10).unwrap();
    let pruned: Vec<&str> = log
        .iter()
        .filter(|e| e.checkpoint.pruned_ms.is_some())
        .map(|e| e.checkpoint.name.as_str())
        .collect();
    assert_eq!(pruned, ["c000002", "c000001"]);
    assert!(!engine.layout().snapshot_path("c000001").exists());
    let again = engine.diff("c1", "c2", false).unwrap();
    assert!(again.cached, "pruned snapshots keep their stored intervals");
    assert!(
        engine.diff("c1", "c3", false).is_err(),
        "no stored interval and no snapshot"
    );

    let v = engine.verify().unwrap();
    assert!(v.ok, "{v:?}");
    let s = engine.status().unwrap();
    assert_eq!(
        (s.checkpoints, s.snapshots_kept, s.pending_diffs),
        (3, 1, 0)
    );
    eprintln!("status: {s:?}");
    let _ = c2;
}

/// Writes a send stream between two snapshots of a tiny tree, for the parser's committed
/// regression test (`crates/btrfs/tests/fixtures`).
#[test]
#[ignore = "root only: run through scripts/p2/it.sh"]
fn write_send_stream_fixture() {
    let (Some(root), Some(out)) = (root(), std::env::var_os("OSREFLOG_FIXTURES")) else {
        return;
    };
    let top = PathBuf::from(std::env::var_os("OSREFLOG_LOOPBACK_TOP").unwrap());
    let base = top.join("fixture");
    fs::create_dir_all(&base).unwrap();
    let base_dir = fs::File::open(&base).unwrap();
    let root_dir = fs::File::open(&root).unwrap();

    let tree = root.join("fixture-tree");
    let _ = fs::remove_dir_all(&tree);
    write(&tree, "keep/same.txt", "same\n");
    write(&tree, "edit.txt", "before\n");
    write(&tree, "gone.txt", "bye\n");
    write(&tree, "dir/inner.txt", "inner\n");
    ioctl::create_snapshot(root_dir.as_fd(), base_dir.as_fd(), "a", true).unwrap();

    write(&tree, "edit.txt", "after, longer\n");
    fs::remove_file(tree.join("gone.txt")).unwrap();
    write(&tree, "new.txt", "hello\n");
    fs::rename(tree.join("dir"), tree.join("moved")).unwrap();
    std::os::unix::fs::symlink("new.txt", tree.join("link")).unwrap();
    ioctl::create_snapshot(root_dir.as_fd(), base_dir.as_fd(), "b", true).unwrap();

    let a = fs::File::open(base.join("a")).unwrap();
    let parent = ioctl::subvolume_info(a.as_fd()).unwrap().treeid;
    let b = std::os::fd::OwnedFd::from(fs::File::open(base.join("b")).unwrap());
    let mut stream = ioctl::send(b, Some(parent), true).unwrap();
    let mut bytes = Vec::new();
    std::io::Read::read_to_end(&mut stream, &mut bytes).unwrap();
    stream.finish().unwrap();
    let out = PathBuf::from(out).join("small.nodata.stream");
    fs::write(&out, &bytes).unwrap();
    eprintln!("wrote {} ({} bytes)", out.display(), bytes.len());

    for name in ["a", "b"] {
        ioctl::delete_subvolume(base_dir.as_fd(), name).unwrap();
    }
    fs::remove_dir_all(&tree).unwrap();
}
