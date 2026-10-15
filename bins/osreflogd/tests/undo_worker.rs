//! The undo worker fails closed: without the capabilities it keeps (an ordinary user has
//! none), it exits before reading a plan. Its real runs are in the write gate's matrix
//! (`scripts/p5/gate.sh`), as root.

#![allow(clippy::unwrap_used)]

use std::io::Write as _;
use std::process::{Command, Stdio};

#[test]
fn without_capabilities_the_worker_refuses_to_start() {
    if rustix::process::geteuid().is_root() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("etc")).unwrap();
    std::fs::write(dir.path().join("etc/a.conf"), "new\n").unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_osreflogd"))
        .arg("--undo-worker")
        .arg(dir.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let plan = r#"{"undo_of":1,"attic":"/var/lib/osreflog/attic/t","steps":[{"path":"/etc/a.conf","expect":{"type":"absent"},"restore":{"type":"absent"}}]}"#;
    let _ = child.stdin.take().unwrap().write_all(plan.as_bytes());
    let out = child.wait_with_output().unwrap();
    assert_eq!(
        out.status.code(),
        Some(2),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(out.stdout, b"");
    assert_eq!(
        std::fs::read_to_string(dir.path().join("etc/a.conf")).unwrap(),
        "new\n"
    );
}
