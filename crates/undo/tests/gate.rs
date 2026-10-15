//! The write gate's matrix: what only root can test. Run by `scripts/p5/gate.sh`, which
//! makes a writable btrfs snapshot of `/` (the scratch root) and runs these tests in a
//! `systemd-run` sandbox where everything but the scratch area is read-only (ADR 0011).
//!
//! Environment: `OSREFLOG_GATE_ROOT` (the scratch root), `OSREFLOG_GATE_OUTSIDE` (a writable
//! directory outside it, where an escape would land), `OSREFLOG_DAEMON` (the osreflogd
//! binary, for the real worker). Without them every test returns at once.

#![allow(clippy::unwrap_used)]

use std::io::Write as _;
use std::os::unix::fs::{MetadataExt, symlink};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use osreflog_undo::apply::NOT_BENEATH;
use osreflog_undo::plan::{hash, to_hex};
use osreflog_undo::{Meta, Outcome, Plan, Report, Root, State, Step, Version, apply};

struct Gate {
    root: PathBuf,
    outside: PathBuf,
    daemon: PathBuf,
}

fn gate() -> Option<Gate> {
    let var = |n| std::env::var_os(n).map(PathBuf::from);
    Some(Gate {
        root: var("OSREFLOG_GATE_ROOT")?,
        outside: var("OSREFLOG_GATE_OUTSIDE")?,
        daemon: var("OSREFLOG_DAEMON")?,
    })
}

impl Gate {
    /// A fresh directory for one test, `/etc/osreflog-gate/<name>` in the scratch root.
    fn area(&self, name: &str) -> (String, PathBuf) {
        let logical = format!("/etc/osreflog-gate/{name}");
        let real = self.root.join(logical.trim_start_matches('/'));
        let _ = std::fs::remove_dir_all(&real);
        std::fs::create_dir_all(&real).unwrap();
        (logical, real)
    }

    fn plan(steps: Vec<Step>) -> Plan {
        Plan {
            undo_of: 1,
            attic: format!("/var/lib/osreflog/attic/gate-{}", std::process::id()),
            steps,
        }
    }

    /// Applies through the real worker: `osreflogd --undo-worker`, sandbox and all.
    fn worker(&self, plan: &Plan) -> Report {
        std::fs::create_dir_all(self.root.join("var/lib/osreflog/attic")).unwrap();
        let mut child = Command::new(&self.daemon)
            .arg("--undo-worker")
            .arg(&self.root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(&serde_json::to_vec(plan).unwrap())
            .unwrap();
        let out = child.wait_with_output().unwrap();
        assert!(
            out.status.success(),
            "worker: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        serde_json::from_slice(&out.stdout).unwrap()
    }
}

fn xattr(path: &Path, name: &str) -> Option<Vec<u8>> {
    let mut buf = vec![0u8; 1024];
    let n = rustix::fs::lgetxattr(path, name, &mut buf[..]).ok()?;
    buf.truncate(n);
    Some(buf)
}

fn label(l: &str) -> (String, String) {
    let mut v = l.as_bytes().to_vec();
    v.push(0);
    ("security.selinux".into(), to_hex(&v))
}

fn file(content: &str) -> State {
    State::File {
        hash: hash(content.as_bytes()),
    }
}

#[test]
#[ignore = "root only: scripts/p5/gate.sh"]
fn owner_mode_xattrs_and_labels_are_restored_by_the_worker() {
    let Some(g) = gate() else { return };
    let (dir, real) = g.area("meta");
    std::fs::write(real.join("owned.conf"), "new\n").unwrap();
    let before = Meta {
        uid: 0,
        gid: 10,
        mode: 0o640,
        xattrs: vec![
            ("user.osreflog".into(), to_hex(b"kept")),
            label("system_u:object_r:httpd_config_t:s0"),
        ],
    };
    let deleted = Meta {
        uid: 0,
        gid: 0,
        mode: 0o644,
        xattrs: vec![label("system_u:object_r:net_conf_t:s0")],
    };
    let report = g.worker(&Gate::plan(vec![
        Step {
            path: format!("{dir}/owned.conf"),
            expect: file("new\n"),
            restore: Version::file(b"old\n", before),
        },
        Step {
            path: format!("{dir}/deleted.conf"),
            expect: State::Absent,
            restore: Version::file(b"was\n", deleted),
        },
    ]));
    for s in &report.steps {
        assert_eq!(
            s.outcome,
            Outcome::Restored {
                warnings: Vec::new()
            },
            "{}",
            s.path
        );
    }
    let owned = real.join("owned.conf");
    let m = std::fs::metadata(&owned).unwrap();
    assert_eq!((m.uid(), m.gid(), m.mode() & 0o7777), (0, 10, 0o640));
    assert_eq!(std::fs::read_to_string(&owned).unwrap(), "old\n");
    assert_eq!(
        xattr(&owned, "user.osreflog").as_deref(),
        Some(&b"kept"[..])
    );
    assert_eq!(
        xattr(&owned, "security.selinux").as_deref(),
        Some(&b"system_u:object_r:httpd_config_t:s0\0"[..])
    );
    assert_eq!(
        xattr(&real.join("deleted.conf"), "security.selinux").as_deref(),
        Some(&b"system_u:object_r:net_conf_t:s0\0"[..])
    );
    // The replaced version is in the attic.
    let attic = g
        .root
        .join(report.attic.trim_start_matches('/'))
        .join(dir.trim_start_matches('/'))
        .join("owned.conf");
    assert_eq!(std::fs::read_to_string(attic).unwrap(), "new\n");
}

#[test]
#[ignore = "root only: scripts/p5/gate.sh"]
fn the_workers_sandbox_confines_it() {
    let Some(g) = gate() else { return };
    let out = Command::new(&g.daemon)
        .arg("--undo-sandbox-selftest")
        .arg(&g.root)
        .output()
        .unwrap();
    print!("{}", String::from_utf8_lossy(&out.stdout));
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
}

#[test]
#[ignore = "root only: scripts/p5/gate.sh"]
fn a_bind_mount_inside_the_root_is_not_crossed() {
    let Some(g) = gate() else { return };
    let (dir, real) = g.area("bind");
    std::fs::create_dir_all(real.join("a")).unwrap();
    std::fs::create_dir_all(real.join("b")).unwrap();
    rustix::mount::mount_bind(real.join("a"), real.join("b")).unwrap();
    let report = apply(
        &Root::open(&g.root).unwrap(),
        &Gate::plan(vec![Step {
            path: format!("{dir}/b/x.conf"),
            expect: State::Absent,
            restore: Version::file(b"x\n", Meta::default()),
        }]),
    );
    rustix::mount::unmount(real.join("b"), rustix::mount::UnmountFlags::DETACH).unwrap();
    assert!(
        matches!(&report.steps[0].outcome, Outcome::Failed { error } if error.starts_with(NOT_BENEATH)),
        "{report:?}"
    );
    assert!(!real.join("a/x.conf").exists());
}

#[test]
#[ignore = "root only: scripts/p5/gate.sh"]
fn proc_and_its_magic_links_are_not_followed() {
    let Some(g) = gate() else { return };
    let (dir, real) = g.area("proc");
    let mnt = real.join("proc");
    std::fs::create_dir_all(&mnt).unwrap();
    rustix::mount::mount(
        "proc",
        &mnt,
        "proc",
        rustix::mount::MountFlags::empty(),
        None::<&std::ffi::CStr>,
    )
    .unwrap();
    // Through the plan: /proc mounted inside the root is another mount.
    let report = apply(
        &Root::open(&g.root).unwrap(),
        &Gate::plan(vec![Step {
            path: format!("{dir}/proc/self/root/etc/osreflog-escape.conf"),
            expect: State::Absent,
            restore: Version::file(b"x\n", Meta::default()),
        }]),
    );
    // Directly: a root at /proc can't resolve its magic links either.
    let procroot = Root::open(&mnt).unwrap();
    let magic = procroot.dir("self/root/etc");
    let thread = procroot.dir("thread-self/fd");
    rustix::mount::unmount(&mnt, rustix::mount::UnmountFlags::DETACH).unwrap();
    assert!(
        matches!(&report.steps[0].outcome, Outcome::Failed { error } if error.starts_with(NOT_BENEATH)),
        "{report:?}"
    );
    assert!(magic.is_err() && thread.is_err());
    assert!(!g.root.join("etc/osreflog-escape.conf").exists());
}

#[test]
#[ignore = "root only: scripts/p5/gate.sh"]
fn a_parent_swapped_for_a_symlink_mid_run_never_escapes() {
    let Some(g) = gate() else { return };
    let (dir, real) = g.area("race");
    let outside = g.outside.join("race");
    let _ = std::fs::remove_dir_all(&outside);
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::create_dir(real.join("d")).unwrap();
    symlink(&outside, real.join("alt")).unwrap();
    // Swap the directory and the symlink pointing outside, as fast as possible.
    let stop = Arc::new(AtomicBool::new(false));
    let swapper = {
        let (stop, real) = (Arc::clone(&stop), real.clone());
        std::thread::spawn(move || {
            let mut swaps = 0u64;
            let parent = rustix::fs::open(
                &real,
                rustix::fs::OFlags::RDONLY | rustix::fs::OFlags::DIRECTORY,
                rustix::fs::Mode::empty(),
            )
            .unwrap();
            while !stop.load(Ordering::Relaxed) {
                rustix::fs::renameat_with(
                    &parent,
                    "d",
                    &parent,
                    "alt",
                    rustix::fs::RenameFlags::EXCHANGE,
                )
                .unwrap();
                swaps += 1;
            }
            swaps
        })
    };
    let root = Root::open(&g.root).unwrap();
    let (mut restored, mut refused) = (0, 0);
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut i = 0;
    while Instant::now() < deadline {
        let report = apply(
            &root,
            &Gate::plan(vec![Step {
                path: format!("{dir}/d/x{i}.conf"),
                expect: State::Absent,
                restore: Version::file(b"x\n", Meta::default()),
            }]),
        );
        match &report.steps[0].outcome {
            Outcome::Restored { .. } => restored += 1,
            _ => refused += 1,
        }
        i += 1;
    }
    stop.store(true, Ordering::Relaxed);
    let swaps = swapper.join().unwrap();
    let escaped: Vec<_> = std::fs::read_dir(&outside).unwrap().flatten().collect();
    println!(
        "{i} runs, {restored} restored, {refused} refused, {swaps} swaps, {} escaped",
        escaped.len()
    );
    assert!(escaped.is_empty(), "wrote outside the root: {escaped:?}");
    assert!(restored > 0 && refused > 0, "both outcomes exercised");
}

#[test]
#[ignore = "root only: scripts/p5/gate.sh"]
fn secrets_capabilities_and_setuid_are_refused_even_as_root() {
    let Some(g) = gate() else { return };
    let (dir, real) = g.area("refuse");
    std::fs::write(real.join("tool"), "new\n").unwrap();
    let cap = Meta {
        uid: 0,
        gid: 0,
        mode: 0o755,
        xattrs: vec![("security.capability".into(), to_hex(&[1, 0, 0, 2]))],
    };
    let suid = Meta {
        uid: 0,
        gid: 0,
        mode: 0o4755,
        xattrs: Vec::new(),
    };
    let report = g.worker(&Gate::plan(vec![
        Step {
            path: "/etc/shadow".into(),
            expect: State::Absent,
            restore: Version::file(b"root::0:0:99999:7:::\n", Meta::default()),
        },
        Step {
            path: format!("{dir}/tool"),
            expect: file("new\n"),
            restore: Version::file(b"old\n", cap),
        },
        Step {
            path: format!("{dir}/suid"),
            expect: State::Absent,
            restore: Version::file(b"x\n", suid),
        },
    ]));
    assert!(
        report
            .steps
            .iter()
            .all(|s| matches!(s.outcome, Outcome::Refused { .. })),
        "{report:?}"
    );
    assert_eq!(std::fs::read_to_string(real.join("tool")).unwrap(), "new\n");
    assert!(xattr(&real.join("tool"), "security.capability").is_none());
}
