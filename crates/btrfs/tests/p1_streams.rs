#![allow(clippy::unwrap_used)]
//! Cross-checks the parser against real streams saved by the P1 measurement run, when they
//! exist on this machine (`/var/tmp/osreflog-p1/results`): every command `btrfs receive
//! --dump` printed must be parsed, with the same name, and no orphan name may survive.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::BufReader;
use std::path::Path;

use osreflog_btrfs::{StreamReader, diff_stream, is_orphan_name};

const DIR: &str = "/var/tmp/osreflog-p1/results";

fn dump_counts(dump: &Path) -> BTreeMap<String, u64> {
    let mut counts = BTreeMap::new();
    for line in std::fs::read_to_string(dump).unwrap().lines() {
        if let Some(name) = line.split_whitespace().next() {
            *counts.entry(name.to_owned()).or_default() += 1;
        }
    }
    counts
}

fn check(tag: &str) {
    let stream = Path::new(DIR).join(format!("send-{tag}.nodata.stream"));
    let dump = Path::new(DIR).join(format!("dump-{tag}.txt"));
    if !stream.exists() || !dump.exists() {
        eprintln!("skipping {tag}: P1 results not on this machine");
        return;
    }
    let mut reader = StreamReader::new(BufReader::new(File::open(&stream).unwrap())).unwrap();
    let mut counts = BTreeMap::<String, u64>::new();
    while let Some(c) = reader.next_command().unwrap() {
        if c.name() != "end" {
            *counts.entry(c.name().to_owned()).or_default() += 1;
        }
    }
    assert_eq!(
        counts,
        dump_counts(&dump),
        "{tag}: command counts differ from btrfs receive --dump"
    );

    let diff = diff_stream(BufReader::new(File::open(&stream).unwrap())).unwrap();
    let orphans: Vec<_> = diff
        .changes
        .iter()
        .filter(|c| is_orphan_name(&c.path))
        .map(|c| c.path.clone())
        .collect();
    assert!(
        orphans.is_empty(),
        "{tag}: orphan names left in the change set: {orphans:?}"
    );
    eprintln!(
        "{tag}: v{} {} commands -> {} changes, {} bytes",
        diff.version,
        diff.info.commands,
        diff.changes.len(),
        diff.changed_bytes
    );
}

#[test]
fn hook_stream_matches_dump() {
    check("hook");
}

#[test]
fn install_stream_matches_dump() {
    check("install");
}
