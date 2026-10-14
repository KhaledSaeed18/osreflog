//! Reads this machine's own rpmdb and dnf5 history (read-only copies) and compares with
//! `rpm -qa`. Skipped where there is no rpm database.
#![allow(clippy::unwrap_used)]

use std::path::Path;
use std::process::Command;

#[test]
fn rpmdb_matches_rpm_qa() {
    if !Path::new("/usr/lib/sysimage/rpm/rpmdb.sqlite").exists() {
        eprintln!("skipping: no rpmdb");
        return;
    }
    let scratch = tempfile::tempdir().unwrap();
    let start = std::time::Instant::now();
    let pkgs = osreflog_rpm::packages(Path::new("/"), scratch.path()).unwrap();
    let elapsed = start.elapsed();
    let out = Command::new("rpm")
        .args([
            "-qa",
            "--qf",
            "%{NAME}-%|EPOCH?{%{EPOCH}:}:{}|%{VERSION}-%{RELEASE}.%{ARCH}\\n",
        ])
        .output()
        .unwrap();
    let mut want: Vec<String> = String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect();
    let mut got: Vec<String> = pkgs.iter().map(osreflog_rpm::Package::nevra).collect();
    // rpm prints epoch 0 when set explicitly; nevra() hides zero epochs like dnf.
    for w in &mut want {
        *w = w.replace("-0:", "-");
    }
    want.sort();
    got.sort();
    assert_eq!(got.len(), want.len());
    assert_eq!(got, want);
    eprintln!("{} packages in {elapsed:?}", got.len());
}

#[test]
fn history_has_transactions() {
    if !Path::new("/usr/lib/sysimage/libdnf5/transaction_history.sqlite").exists() {
        eprintln!("skipping: no dnf5 history");
        return;
    }
    let scratch = tempfile::tempdir().unwrap();
    let all = osreflog_rpm::transactions(Path::new("/"), scratch.path(), 0, i64::MAX).unwrap();
    assert_ne!(all.len(), 0);
    let last = all.last().unwrap();
    eprintln!(
        "{} transactions; last #{} {:?} by uid {}",
        all.len(),
        last.id,
        last.description,
        last.user_id
    );
}
