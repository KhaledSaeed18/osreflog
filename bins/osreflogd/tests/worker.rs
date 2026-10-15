//! The summarizer worker and its sandbox, run as real processes (no root needed: Landlock and
//! seccomp are unprivileged with no-new-privileges).
#![allow(clippy::unwrap_used)]

use std::io::Write;
use std::os::unix::process::ExitStatusExt;
use std::process::{Command, Stdio};

const EXE: &str = env!("CARGO_BIN_EXE_osreflogd");

#[test]
fn sandbox_kills_a_file_open() {
    let status = Command::new(EXE)
        .arg("--sandbox-selftest")
        .status()
        .unwrap();
    assert_eq!(
        status.code(),
        None,
        "the process must not exit normally: {status:?}"
    );
    assert_eq!(
        status.signal(),
        Some(libc_sigsys()),
        "killed by seccomp (SIGSYS)"
    );
}

fn libc_sigsys() -> i32 {
    31
}

#[test]
fn worker_summarizes_through_the_sandbox() {
    let mut child = Command::new(EXE)
        .arg("--summarize-worker")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let request = r#"{"items":[
        {"path":"/etc/sysctl.d/99-x.conf","op":"modify","before":"vm.swappiness = 60\n","after":"vm.swappiness = 10\n","before_target":null,"after_target":null},
        {"path":"/etc/passwd","op":"modify","before":"a:x:1:1::/:/bin/sh\n","after":"a:x:1:1::/:/bin/sh\n","before_target":null,"after_target":null}
    ]}"#;
    child
        .stdin
        .take()
        .unwrap()
        .write_all(request.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(
        out.status.success(),
        "{:?} {}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["summaries"][0][0], "vm.swappiness: 60 → 10");
    assert_eq!(v["summaries"][1][0], "rewritten, content unchanged");
}

#[test]
fn worker_rejects_garbage_without_crashing() {
    let mut child = Command::new(EXE)
        .arg("--summarize-worker")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"\x00\xffnot json")
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert_eq!(out.status.code(), Some(4));
}
