//! The engine against a scratch root in a temp dir, as an ordinary user. Ownership, `SELinux`
//! labels, bind mounts and magic links need root: those are in the gate matrix
//! (`scripts/p5/gate.sh`).

#![allow(clippy::unwrap_used)]
use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
use std::path::{Path, PathBuf};

use osreflog_undo::plan::hash;
use osreflog_undo::{Meta, Outcome, Plan, Root, State, Step, Version, apply};

struct Scratch {
    _dir: tempfile::TempDir,
    root: PathBuf,
}

impl Scratch {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("root");
        fs::create_dir_all(root.join("etc")).unwrap();
        Self { _dir: dir, root }
    }

    fn at(&self, path: &str) -> PathBuf {
        self.root.join(path.trim_start_matches('/'))
    }

    fn write(&self, path: &str, content: &str) {
        let p = self.at(path);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, content).unwrap();
    }

    fn read(&self, path: &str) -> Option<String> {
        fs::read_to_string(self.at(path)).ok()
    }

    fn run(&self, steps: Vec<Step>) -> Vec<Outcome> {
        let plan = Plan {
            undo_of: 7,
            attic: "/var/lib/osreflog/attic/t".into(),
            steps,
        };
        let root = Root::open(&self.root).unwrap();
        let report = apply(&root, &plan);
        // Report order is execution order; tests list steps in that order.
        report.steps.into_iter().map(|s| s.outcome).collect()
    }
}

fn me(mode: u32) -> Meta {
    let m = fs::metadata("/proc/self").unwrap();
    Meta {
        uid: m.uid(),
        gid: m.gid(),
        mode,
        xattrs: Vec::new(),
    }
}

fn file(content: &str) -> State {
    State::File {
        hash: hash(content.as_bytes()),
    }
}

fn step(path: &str, expect: State, restore: Version) -> Step {
    Step {
        path: path.into(),
        expect,
        restore,
    }
}

fn restored(o: &Outcome) -> bool {
    matches!(o, Outcome::Restored { warnings } if warnings.is_empty())
}

#[test]
fn modify_is_restored_and_the_newer_version_kept_in_the_attic() {
    let s = Scratch::new();
    s.write("/etc/a.conf", "new\n");
    let out = s.run(vec![step(
        "/etc/a.conf",
        file("new\n"),
        Version::file(b"old\n", me(0o640)),
    )]);
    assert!(restored(&out[0]), "{out:?}");
    assert_eq!(s.read("/etc/a.conf").as_deref(), Some("old\n"));
    let mode = fs::metadata(s.at("/etc/a.conf"))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o7777, 0o640);
    assert_eq!(
        s.read("/var/lib/osreflog/attic/t/etc/a.conf").as_deref(),
        Some("new\n")
    );
    // No temporary file left behind.
    let names: Vec<_> = fs::read_dir(s.at("/etc"))
        .unwrap()
        .flatten()
        .map(|e| e.file_name())
        .collect();
    assert_eq!(names.len(), 1, "{names:?}");
}

#[test]
fn already_undone_and_conflicts_are_left_alone() {
    let s = Scratch::new();
    s.write("/etc/same.conf", "old\n");
    s.write("/etc/edited.conf", "edited by someone else\n");
    let out = s.run(vec![
        step(
            "/etc/edited.conf",
            file("new\n"),
            Version::file(b"old\n", me(0o644)),
        ),
        step(
            "/etc/same.conf",
            file("new\n"),
            Version::file(b"old\n", me(0o644)),
        ),
    ]);
    assert_eq!(
        out[0],
        Outcome::Conflict {
            current: file("edited by someone else\n"),
            reason: "it changed after this cause".into()
        }
    );
    assert_eq!(out[1], Outcome::AlreadyUndone);
    assert_eq!(
        s.read("/etc/edited.conf").as_deref(),
        Some("edited by someone else\n")
    );
}

#[test]
fn created_file_goes_to_the_attic_and_deleted_file_comes_back() {
    let s = Scratch::new();
    s.write("/etc/created.conf", "x\n");
    let out = s.run(vec![
        step("/etc/created.conf", file("x\n"), Version::Absent),
        step(
            "/etc/deleted.conf",
            State::Absent,
            Version::file(b"was\n", me(0o600)),
        ),
    ]);
    assert!(out.iter().all(restored), "{out:?}");
    assert!(s.read("/etc/created.conf").is_none());
    assert_eq!(
        s.read("/var/lib/osreflog/attic/t/etc/created.conf")
            .as_deref(),
        Some("x\n")
    );
    assert_eq!(s.read("/etc/deleted.conf").as_deref(), Some("was\n"));
}

#[test]
fn reappeared_path_is_a_conflict() {
    let s = Scratch::new();
    s.write("/etc/back.conf", "someone recreated it\n");
    let out = s.run(vec![step(
        "/etc/back.conf",
        State::Absent,
        Version::file(b"was\n", me(0o644)),
    )]);
    assert!(matches!(out[0], Outcome::Conflict { .. }), "{out:?}");
    assert_eq!(
        s.read("/etc/back.conf").as_deref(),
        Some("someone recreated it\n")
    );
}

#[test]
fn symlinks_are_retargeted_never_followed() {
    let s = Scratch::new();
    s.write("/etc/zones/a", "A\n");
    s.write("/etc/zones/b", "B\n");
    symlink("zones/b", s.at("/etc/localtime")).unwrap();
    let me = me(0);
    let out = s.run(vec![step(
        "/etc/localtime",
        State::Symlink {
            target: "zones/b".into(),
        },
        Version::Symlink {
            target: "zones/a".into(),
            uid: me.uid,
            gid: me.gid,
        },
    )]);
    assert!(restored(&out[0]), "{out:?}");
    assert_eq!(
        fs::read_link(s.at("/etc/localtime")).unwrap(),
        Path::new("zones/a")
    );
    // The files the links point at were never touched.
    assert_eq!(s.read("/etc/zones/b").as_deref(), Some("B\n"));
}

#[test]
fn directories_created_and_deleted() {
    let s = Scratch::new();
    fs::create_dir_all(s.at("/etc/empty.d")).unwrap();
    s.write("/etc/full.d/other.conf", "not from this cause\n");
    let out = s.run(vec![
        step("/etc/empty.d", State::Dir, Version::Absent),
        step("/etc/full.d", State::Dir, Version::Absent),
        step(
            "/etc/gone.d",
            State::Absent,
            Version::Dir { meta: me(0o750) },
        ),
        step(
            "/etc/gone.d/x.conf",
            State::Absent,
            Version::file(b"x\n", me(0o644)),
        ),
    ]);
    assert!(restored(&out[0]), "{out:?}");
    assert!(
        matches!(&out[1], Outcome::Conflict { reason, .. } if reason.contains("isn't empty")),
        "{out:?}"
    );
    assert!(restored(&out[2]) && restored(&out[3]), "{out:?}");
    assert!(!s.at("/etc/empty.d").exists());
    assert_eq!(
        s.read("/etc/full.d/other.conf").as_deref(),
        Some("not from this cause\n")
    );
    assert_eq!(
        fs::metadata(s.at("/etc/gone.d"))
            .unwrap()
            .permissions()
            .mode()
            & 0o7777,
        0o750
    );
    assert_eq!(s.read("/etc/gone.d/x.conf").as_deref(), Some("x\n"));
}

#[test]
fn a_symlink_on_the_way_is_never_followed() {
    let s = Scratch::new();
    let outside = s.root.parent().unwrap().join("outside");
    fs::create_dir_all(&outside).unwrap();
    fs::write(outside.join("x.conf"), "outside\n").unwrap();
    // /etc/sub -> a directory outside the root (absolute), /etc/rel -> ../../outside.
    symlink(&outside, s.at("/etc/sub")).unwrap();
    symlink("../../outside", s.at("/etc/rel")).unwrap();
    let out = s.run(vec![
        step(
            "/etc/rel/x.conf",
            file("outside\n"),
            Version::file(b"pwned\n", me(0o644)),
        ),
        step(
            "/etc/sub/x.conf",
            file("outside\n"),
            Version::file(b"pwned\n", me(0o644)),
        ),
    ]);
    for o in &out {
        assert!(
            matches!(o, Outcome::Failed { error } if error.starts_with(osreflog_undo::apply::NOT_BENEATH)),
            "{o:?}"
        );
    }
    assert_eq!(
        fs::read_to_string(outside.join("x.conf")).unwrap(),
        "outside\n"
    );
}

#[test]
fn the_root_itself_may_not_be_escaped_by_the_plan() {
    let s = Scratch::new();
    let out = s.run(vec![
        step("/etc/../x", State::Absent, Version::file(b"x", me(0o644))),
        step(
            "/etc/a/../../x",
            State::Absent,
            Version::file(b"x", me(0o644)),
        ),
        step("/var/lib/x", State::Absent, Version::file(b"x", me(0o644))),
        step("/usr/bin/x", State::Absent, Version::file(b"x", me(0o755))),
    ]);
    assert!(
        out.iter().all(|o| matches!(o, Outcome::Refused { .. })),
        "{out:?}"
    );
    assert!(!s.root.parent().unwrap().join("x").exists());
}

#[test]
fn a_fifo_where_a_file_was_is_never_opened() {
    let s = Scratch::new();
    rustix::fs::mknodat(
        rustix::fs::CWD,
        s.at("/etc/pipe.conf"),
        rustix::fs::FileType::Fifo,
        rustix::fs::Mode::from_raw_mode(0o644),
        0,
    )
    .unwrap();
    let out = s.run(vec![step(
        "/etc/pipe.conf",
        file("x\n"),
        Version::file(b"was\n", me(0o644)),
    )]);
    assert!(
        matches!(
            &out[0],
            Outcome::Conflict {
                current: State::Other,
                ..
            }
        ),
        "{out:?}"
    );
}

#[test]
fn attic_outside_its_place_refuses_the_whole_plan() {
    let s = Scratch::new();
    s.write("/etc/a.conf", "new\n");
    let plan = Plan {
        undo_of: 1,
        attic: "/etc".into(),
        steps: vec![step("/etc/a.conf", file("new\n"), Version::Absent)],
    };
    let report = apply(&Root::open(&s.root).unwrap(), &plan);
    assert!(matches!(report.steps[0].outcome, Outcome::Refused { .. }));
    assert_eq!(s.read("/etc/a.conf").as_deref(), Some("new\n"));
}
