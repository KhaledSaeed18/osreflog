//! A long-running randomized harness for every parser and differ that reads input osreflog
//! doesn't control. It runs on the stable toolchain without instrumentation: inputs are
//! mutations of real seeds (files under `/etc`, the P1 send streams, rpm headers and fanotify
//! events built by the crates' own builders) plus random bytes. Every target checks more than
//! "no panic": output stays bounded, time per input stays bounded, and formats with a round
//! trip or a reconstruction check it.
//!
//! ```text
//! osreflog-fuzz <target> [--secs N] [--seed S] [--out DIR]
//! osreflog-fuzz replay <target> <file>
//! osreflog-fuzz vercmp-pairs <n> [--seed S]     # pairs for the differential check against rpm
//! ```
//!
//! Runs are deterministic for a seed, so a crash that kills the process (an abort) is
//! reproduced by rerunning with the logged seed; large inputs are also written to
//! `<out>/<target>.current` before they run.

#[path = "../../bins/osreflog/src/textdiff.rs"]
mod textdiff;

use std::cmp::Ordering;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant};

use osreflog_btrfs::stream::MAGIC;
use osreflog_btrfs::{ChangeSetBuilder, StreamReader, crc32c};
use osreflog_fanotify::event::{self, Fid, Handle};
use osreflog_rpm::Package;
use osreflog_rpm::vercmp::rpmvercmp;
use osreflog_semantic::{Item, MAX_LINE, MAX_LINES};

/// Larger inputs are not generated.
const MAX_INPUT: usize = 256 << 10;
/// An input taking longer than this is a finding.
const SLOW: Duration = Duration::from_millis(500);
/// Inputs at least this large are written out before they run.
const PERSIST_FROM: usize = 16 << 10;
const MAX_FINDINGS: usize = 25;
const STREAM_HEADER: usize = 17;
const CMD_HEADER: usize = 10;

struct Target {
    name: &'static str,
    seeds: fn() -> Vec<Vec<u8>>,
    generate: fn(&mut Gen) -> Vec<u8>,
    check: fn(&[u8]) -> Result<(), String>,
}

const TARGETS: &[Target] = &[
    Target {
        name: "semantic",
        seeds: semantic_seeds,
        generate: semantic_gen,
        check: semantic_check,
    },
    Target {
        name: "stream",
        seeds: stream_seeds,
        generate: stream_gen,
        check: stream_check,
    },
    Target {
        name: "rpm-header",
        seeds: rpm_seeds,
        generate: Gen::mutated,
        check: rpm_header_check,
    },
    Target {
        name: "rpm-diff",
        seeds: rpm_diff_seeds,
        generate: rpm_diff_gen,
        check: rpm_diff_check,
    },
    Target {
        name: "vercmp",
        seeds: vercmp_seeds,
        generate: vercmp_gen,
        check: vercmp_check,
    },
    Target {
        name: "fanotify",
        seeds: fanotify_seeds,
        generate: Gen::mutated,
        check: fanotify_check,
    },
    Target {
        name: "refs",
        seeds: refs_seeds,
        generate: Gen::mutated,
        check: refs_check,
    },
    Target {
        name: "textdiff",
        seeds: textdiff_seeds,
        generate: textdiff_gen,
        check: textdiff_check,
    },
];

// ---------------------------------------------------------------------------------------
// Mutation

struct Gen {
    rng: fastrand::Rng,
    seeds: Vec<Vec<u8>>,
}

const TOKENS: &[&[u8]] = &[
    b"=",
    b" = ",
    b"[",
    b"]",
    b"[section]\n",
    b"\n",
    b"\r\n",
    b"#",
    b":",
    b"::",
    b"0:0:",
    b"\"",
    b"{\"a\":",
    b"}",
    b"[]",
    b"\\",
    b"\\\n",
    b"\t",
    b"\0",
    "é".as_bytes(),
    "\u{feff}".as_bytes(),
    "\u{10ffff}".as_bytes(),
    "→".as_bytes(),
    b"\xff\xfe",
    b"999999999999999999999",
    b"-1",
    b"4294967295",
    b"~",
    b"^",
    b"/dev/null",
    b"zoneinfo/",
    b"@{",
];

const INTS: &[u64] = &[
    0,
    1,
    2,
    0x7f,
    0x80,
    0xff,
    0x100,
    0x7fff,
    0xffff,
    0x7fff_ffff,
    0xffff_ffff,
    u64::MAX,
];

impl Gen {
    fn new(seed: u64, seeds: Vec<Vec<u8>>) -> Self {
        Self {
            rng: fastrand::Rng::with_seed(seed),
            seeds,
        }
    }

    fn seed(&mut self) -> Vec<u8> {
        if self.seeds.is_empty() || self.rng.u8(..) < 8 {
            let n = self.rng.usize(..64);
            return (0..n).map(|_| self.rng.u8(..)).collect();
        }
        self.seeds[self.rng.usize(..self.seeds.len())].clone()
    }

    fn mutated(&mut self) -> Vec<u8> {
        let s = self.seed();
        self.mutate(s)
    }

    /// One to eight random edits: bit flips, interesting bytes, integers and tokens,
    /// insertions, deletions, duplicated ranges, splices from other seeds, truncation.
    fn mutate(&mut self, mut d: Vec<u8>) -> Vec<u8> {
        let edits = 1 + self.rng.usize(..8);
        for _ in 0..edits {
            let at = if d.is_empty() {
                0
            } else {
                self.rng.usize(..d.len())
            };
            match self.rng.u8(..10) {
                0 if !d.is_empty() => d[at] ^= 1 << self.rng.u8(..8),
                1 if !d.is_empty() => d[at] = self.rng.u8(..),
                2 if !d.is_empty() => {
                    let v = INTS[self.rng.usize(..INTS.len())];
                    let width = [1usize, 2, 4, 8][self.rng.usize(..4)];
                    let bytes = if self.rng.bool() {
                        v.to_le_bytes()
                    } else {
                        v.to_be_bytes()
                    };
                    let src = if self.rng.bool() {
                        &bytes[..width]
                    } else {
                        &bytes[8 - width..]
                    };
                    for (i, b) in src.iter().enumerate() {
                        if let Some(slot) = d.get_mut(at + i) {
                            *slot = *b;
                        }
                    }
                }
                3 => {
                    let t = TOKENS[self.rng.usize(..TOKENS.len())];
                    d.splice(at..at, t.iter().copied());
                }
                4 => {
                    let n = 1 + self.rng.usize(..16);
                    let bytes: Vec<u8> = (0..n).map(|_| self.rng.u8(..)).collect();
                    d.splice(at..at, bytes);
                }
                5 if !d.is_empty() => {
                    let end = (at + 1 + self.rng.usize(..64)).min(d.len());
                    d.drain(at..end);
                }
                6 if !d.is_empty() => {
                    let end = (at + 1 + self.rng.usize(..256)).min(d.len());
                    let chunk = d[at..end].to_vec();
                    let to = self.rng.usize(..=d.len());
                    d.splice(to..to, chunk);
                }
                7 if !self.seeds.is_empty() => {
                    let other = &self.seeds[self.rng.usize(..self.seeds.len())];
                    if !other.is_empty() {
                        let from = self.rng.usize(..other.len());
                        let end = (from + 1 + self.rng.usize(..512)).min(other.len());
                        let chunk = other[from..end].to_vec();
                        d.splice(at..at, chunk);
                    }
                }
                8 if !d.is_empty() => d.truncate(at),
                _ => {
                    // Swap two lines: reorders entries without breaking their syntax.
                    let lines: Vec<usize> = d
                        .iter()
                        .enumerate()
                        .filter(|(_, b)| **b == b'\n')
                        .map(|(i, _)| i)
                        .collect();
                    if lines.len() >= 2 {
                        let i = self.rng.usize(..lines.len() - 1);
                        let (a, b) = (lines[i] + 1, lines[i + 1] + 1);
                        if b <= d.len() {
                            let line = d[a..b].to_vec();
                            d.drain(a..b);
                            let to = lines[self.rng.usize(..=i)].min(d.len());
                            d.splice(to..to, line);
                        }
                    }
                }
            }
        }
        d.truncate(MAX_INPUT);
        d
    }
}

fn text(d: &[u8]) -> String {
    String::from_utf8_lossy(d).into_owned()
}

// ---------------------------------------------------------------------------------------
// Seeds from the machine

fn etc_files(limit: usize) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut dirs = vec![PathBuf::from("/etc")];
    while let Some(dir) = dirs.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in entries.flatten() {
            let Ok(ft) = e.file_type() else { continue };
            let path = e.path();
            if ft.is_dir() {
                dirs.push(path);
            } else if ft.is_file()
                && e.metadata().is_ok_and(|m| m.len() <= 64 << 10)
                && let Ok(text) = std::fs::read_to_string(&path)
            {
                out.push((path.to_string_lossy().into_owned(), text));
                if out.len() >= limit {
                    return out;
                }
            }
        }
    }
    out
}

fn files_in(dir: &Path, suffix: &str) -> Vec<Vec<u8>> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().ends_with(suffix))
        .filter_map(|e| std::fs::read(e.path()).ok())
        .filter(|b| b.len() <= MAX_INPUT)
        .collect()
}

// ---------------------------------------------------------------------------------------
// semantic: the summarizers run by the sandboxed worker

const PATHS: &[&str] = &[
    "/etc/passwd",
    "/etc/group",
    "/etc/passwd-",
    "/etc/fstab",
    "/etc/hosts",
    "/etc/x.json",
    "/etc/x.toml",
    "/etc/sysctl.d/99-x.conf",
    "/etc/systemd/system/a.service",
    "/etc/systemd/system/a.service.d/b.conf",
    "/etc/systemd/system/multi-user.target.wants/a.service",
    "/etc/systemd/user/default.target.upholds/a.service",
    "/etc/systemd/system/x/y/z",
    "/etc/localtime",
    "/etc/alternatives/java",
    "/etc/NetworkManager/system-connections/w.nmconnection",
    "/etc/yum.repos.d/f.REPO",
    "/",
    "",
    "/etc/systemd/",
];
const OPS: &[&str] = &["create", "modify", "delete", "rename", "attr", "", "x"];
const LINK_TARGETS: &[&str] = &[
    "/dev/null",
    "/usr/share/zoneinfo/Europe/Berlin",
    "../usr/share/zoneinfo/UTC",
    "zoneinfo/zoneinfo/",
    "zoneinfo/",
    "",
    "/usr/lib/jvm/java-21/bin/java",
];

fn semantic_seeds() -> Vec<Vec<u8>> {
    let mut seeds: Vec<Vec<u8>> = etc_files(3000)
        .into_iter()
        .map(|(p, t)| [p.into_bytes(), vec![0], t.into_bytes()].concat())
        .collect();
    for (p, t) in [
        (
            "/etc/passwd",
            "root:x:0:0:root:/root:/bin/bash\nbin:x:1:1:bin:/bin:/sbin/nologin\n",
        ),
        ("/etc/group", "root:x:0:\nwheel:x:10:alice,bob\n"),
        (
            "/etc/fstab",
            "UUID=1 / btrfs subvol=root,compress=zstd:1 0 0\n",
        ),
        ("/etc/x.json", "{\"a\": [1, {\"b\": null}], \"c\": \"d\"}"),
        (
            "/etc/a.ini",
            "[main]\nkey = value\n# comment\n[other]\nk=v\n",
        ),
    ] {
        seeds.push([p.as_bytes(), &[0], t.as_bytes()].concat());
    }
    seeds
}

fn split_seed(seed: &[u8]) -> (&[u8], &[u8]) {
    match seed.iter().position(|b| *b == 0) {
        Some(i) => (&seed[..i], &seed[i + 1..]),
        None => (b"", seed),
    }
}

fn semantic_gen(g: &mut Gen) -> Vec<u8> {
    let seed = g.seed();
    let (path, content) = split_seed(&seed);
    let (path, content) = (String::from_utf8_lossy(path).into_owned(), content.to_vec());
    let path = if g.rng.u8(..10) < 6 {
        path
    } else {
        PATHS[g.rng.usize(..PATHS.len())].to_owned()
    };
    let before = (g.rng.u8(..10) > 0).then(|| {
        let m = g.mutate(content.clone());
        text(&m)
    });
    let after = (g.rng.u8(..10) > 0).then(|| {
        let base = match (&before, g.rng.u8(..4)) {
            (Some(b), 0..=2) => b.clone().into_bytes(),
            _ => g.mutated(),
        };
        let m = g.mutate(base);
        text(&m)
    });
    let item = Item {
        path,
        op: OPS[g.rng.usize(..OPS.len())].to_owned(),
        before,
        after,
        before_target: link_target(g),
        after_target: link_target(g),
    };
    serde_json::to_vec(&item).unwrap_or_default()
}

fn link_target(g: &mut Gen) -> Option<String> {
    match g.rng.u8(..10) {
        0..=6 => None,
        7 | 8 => Some(LINK_TARGETS[g.rng.usize(..LINK_TARGETS.len())].to_owned()),
        _ => {
            let m = g.mutated();
            Some(text(&m))
        }
    }
}

fn semantic_check(input: &[u8]) -> Result<(), String> {
    let Ok(item) = serde_json::from_slice::<Item>(input) else {
        return Ok(());
    };
    let lines = osreflog_semantic::summarize(&item);
    if lines.len() > MAX_LINES {
        return Err(format!("{} lines, more than {MAX_LINES}", lines.len()));
    }
    for l in &lines {
        if l.chars().count() > MAX_LINE {
            return Err(format!("line of {} chars: {l:?}", l.chars().count()));
        }
        if l.chars().any(char::is_control) {
            return Err(format!("control character in {l:?}"));
        }
    }
    if osreflog_semantic::summarize(&item) != lines {
        return Err("not deterministic".into());
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------
// stream: the btrfs send-stream parser and change-set builder

fn stream_seeds() -> Vec<Vec<u8>> {
    let mut seeds = files_in(
        Path::new(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../crates/btrfs/tests/fixtures"
        )),
        ".stream",
    );
    seeds.extend(files_in(
        Path::new("/var/tmp/osreflog-p1/results"),
        ".stream",
    ));
    seeds
}

/// Rewrites every command's CRC so the checking parser gets past it.
fn fix_crcs(d: &mut [u8]) {
    let mut pos = STREAM_HEADER;
    while pos + CMD_HEADER <= d.len() {
        let len = u32::from_le_bytes([d[pos], d[pos + 1], d[pos + 2], d[pos + 3]]) as usize;
        let end = pos + CMD_HEADER + len;
        if end > d.len() {
            return;
        }
        d[pos + 6..pos + 10].fill(0);
        let crc = crc32c::update(
            crc32c::update(0, &d[pos..pos + CMD_HEADER]),
            &d[pos + CMD_HEADER..end],
        );
        d[pos + 6..pos + 10].copy_from_slice(&crc.to_le_bytes());
        pos = end;
    }
}

fn stream_gen(g: &mut Gen) -> Vec<u8> {
    let seed = g.seed();
    let mut d = g.mutate(seed.clone());
    if g.rng.u8(..10) < 8 && seed.len() >= STREAM_HEADER && d.len() >= STREAM_HEADER {
        d[..STREAM_HEADER].copy_from_slice(&seed[..STREAM_HEADER]);
    }
    if g.rng.bool() {
        fix_crcs(&mut d);
    }
    d
}

fn stream_check(input: &[u8]) -> Result<(), String> {
    let checked = osreflog_btrfs::diff_stream(input).ok();
    let Ok(mut reader) = StreamReader::new(input) else {
        return Ok(());
    };
    reader.skip_crc();
    let mut builder = ChangeSetBuilder::new();
    let mut commands = 0usize;
    let bound = input.len() / CMD_HEADER + 1;
    loop {
        match reader.next_command() {
            Ok(Some(c)) => {
                commands += 1;
                if commands > bound {
                    return Err(format!("{commands} commands from {} bytes", input.len()));
                }
                if builder.apply(&c).is_err() {
                    return Ok(());
                }
            }
            Ok(None) => break,
            Err(_) => return Ok(()),
        }
    }
    let (changes, _) = builder.finish();
    if let Some(d) = checked
        && d.changes.len() != changes.len()
    {
        return Err(format!(
            "CRC-checked parse gave {} changes, unchecked {}",
            d.changes.len(),
            changes.len()
        ));
    }
    if input.starts_with(MAGIC) && changes.iter().any(|c| c.path.is_empty()) {
        return Err("a change with an empty path".into());
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------
// rpm: header parser, package-set diff, version comparison

fn rpm_seeds() -> Vec<Vec<u8>> {
    use osreflog_rpm::header::build;
    vec![
        build("bash", None, "5.2.37", "1.fc44", "x86_64"),
        build("kernel-core", Some(1), "7.2.9", "200.fc44", "x86_64"),
        build("gpg-pubkey", None, "abc", "def", ""),
        build("", Some(u32::MAX), "", "", "noarch"),
    ]
}

// Every target has the same signature.
#[allow(clippy::unnecessary_wraps)]
fn rpm_header_check(input: &[u8]) -> Result<(), String> {
    if let Ok(p) = osreflog_rpm::header::parse(input) {
        let _ = (p.evr(), p.nevra());
    }
    Ok(())
}

const NAMES: &[&str] = &["kernel", "bash", "glibc", "a"];
const ARCHES: &[&str] = &["x86_64", "i686", "noarch"];
const VERSION_PARTS: &[&str] = &[
    "0",
    "1",
    "01",
    "2",
    "10",
    "1.0",
    "1.00",
    "a",
    "b",
    "~rc1",
    "^git",
    "~",
    "^",
    ".",
    "_",
    "+",
    "fc44",
    "el9",
    "999999999999999999999",
];

fn version(g: &mut Gen) -> String {
    let n = 1 + g.rng.usize(..4);
    (0..n)
        .map(|_| VERSION_PARTS[g.rng.usize(..VERSION_PARTS.len())])
        .collect()
}

fn package(g: &mut Gen) -> Package {
    Package {
        name: NAMES[g.rng.usize(..NAMES.len())].to_owned(),
        epoch: [None, Some(0), Some(1), Some(u32::MAX)][g.rng.usize(..4)],
        version: version(g),
        release: version(g),
        arch: ARCHES[g.rng.usize(..ARCHES.len())].to_owned(),
    }
}

fn rpm_diff_seeds() -> Vec<Vec<u8>> {
    Vec::new()
}

/// Two package lists as text: one `name epoch version release arch` per line, `--` between.
fn rpm_diff_gen(g: &mut Gen) -> Vec<u8> {
    let mut s = String::new();
    let old: Vec<Package> = (0..g.rng.usize(..8)).map(|_| package(g)).collect();
    let mut new: Vec<Package> = old.iter().filter(|_| g.rng.bool()).cloned().collect();
    new.extend((0..g.rng.usize(..8)).map(|_| package(g)));
    for (i, list) in [old, new].iter().enumerate() {
        if i == 1 {
            s.push_str("--\n");
        }
        for p in list {
            let e = p.epoch.map_or("-".to_owned(), |e| e.to_string());
            let _ = writeln!(s, "{} {e} {} {} {}", p.name, p.version, p.release, p.arch);
        }
    }
    s.into_bytes()
}

fn parse_lists(input: &[u8]) -> Option<(Vec<Package>, Vec<Package>)> {
    let text = std::str::from_utf8(input).ok()?;
    let (a, b) = text.split_once("--\n")?;
    let parse = |t: &str| -> Option<Vec<Package>> {
        t.lines()
            .map(|l| {
                let f: Vec<&str> = l.split(' ').collect();
                let [name, epoch, version, release, arch] = f[..] else {
                    return None;
                };
                Some(Package {
                    name: name.into(),
                    epoch: epoch.parse().ok(),
                    version: version.into(),
                    release: release.into(),
                    arch: arch.into(),
                })
            })
            .collect()
    };
    Some((parse(a)?, parse(b)?))
}

/// Every package that left or arrived is accounted for exactly once, as the `from` or the
/// `to` of a change for its name and arch, and nothing else is.
fn rpm_diff_check(input: &[u8]) -> Result<(), String> {
    let Some((old, new)) = parse_lists(input) else {
        return Ok(());
    };
    let changes = osreflog_rpm::diff(&old, &new);
    let mut left: Vec<(String, String, String)> = old
        .iter()
        .filter(|p| !new.contains(p))
        .map(|p| (p.name.clone(), p.arch.clone(), p.evr()))
        .collect();
    let mut arrived: Vec<(String, String, String)> = new
        .iter()
        .filter(|p| !old.contains(p))
        .map(|p| (p.name.clone(), p.arch.clone(), p.evr()))
        .collect();
    let mut from: Vec<(String, String, String)> = Vec::new();
    let mut to: Vec<(String, String, String)> = Vec::new();
    for c in &changes {
        match (c.action.as_str(), &c.from_evr, &c.to_evr) {
            ("install", None, Some(t)) => to.push((c.name.clone(), c.arch.clone(), t.clone())),
            ("remove", Some(f), None) => from.push((c.name.clone(), c.arch.clone(), f.clone())),
            ("upgrade" | "downgrade", Some(f), Some(t)) => {
                from.push((c.name.clone(), c.arch.clone(), f.clone()));
                to.push((c.name.clone(), c.arch.clone(), t.clone()));
            }
            _ => return Err(format!("malformed change {c:?}")),
        }
    }
    for v in [&mut left, &mut arrived, &mut from, &mut to] {
        v.sort();
    }
    if left != from || arrived != to {
        return Err(format!(
            "left {left:?} arrived {arrived:?}, changes {changes:?}"
        ));
    }
    Ok(())
}

fn vercmp_seeds() -> Vec<Vec<u8>> {
    Vec::new()
}

fn vercmp_gen(g: &mut Gen) -> Vec<u8> {
    let mut parts: Vec<String> = (0..3).map(|_| version(g)).collect();
    if g.rng.u8(..4) == 0 {
        let i = g.rng.usize(..3);
        let m = g.mutate(parts[i].clone().into_bytes());
        parts[i] = text(&m).replace('\0', "");
    }
    parts.join("\0").into_bytes()
}

fn vercmp_check(input: &[u8]) -> Result<(), String> {
    let text = String::from_utf8_lossy(input);
    let v: Vec<&str> = text.split('\0').collect();
    let [a, b, c] = v[..] else { return Ok(()) };
    if rpmvercmp(a, a) != Ordering::Equal {
        return Err(format!("{a:?} is not equal to itself"));
    }
    if rpmvercmp(a, b) != rpmvercmp(b, a).reverse() {
        return Err(format!("not antisymmetric: {a:?} {b:?}"));
    }
    if rpmvercmp(a, b).is_le() && rpmvercmp(b, c).is_le() && rpmvercmp(a, c).is_gt() {
        return Err(format!(
            "not transitive: {a:?} <= {b:?} <= {c:?} but {a:?} > {c:?}"
        ));
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------
// fanotify: the event-buffer parser

fn fanotify_seeds() -> Vec<Vec<u8>> {
    let fid = |info_type, name: Option<&[u8]>| Fid {
        info_type,
        fsid: [7, -1],
        handle: Handle {
            handle_type: 1,
            bytes: vec![1, 2, 3, 4, 5, 6, 7, 8],
        },
        name: name.map(<[u8]>::to_vec),
    };
    let one = event::build(
        event::FAN_CREATE | event::FAN_ONDIR,
        42,
        &[fid(event::INFO_DFID_NAME, Some(b"etc"))],
    );
    let rename = event::build(
        event::FAN_RENAME,
        7,
        &[
            fid(event::INFO_OLD_DFID_NAME, Some(b"a.tmp")),
            fid(event::INFO_NEW_DFID_NAME, Some(b"a")),
        ],
    );
    let plain = event::build(event::FAN_CLOSE_WRITE, 1, &[fid(event::INFO_FID, None)]);
    vec![
        one.clone(),
        rename.clone(),
        plain.clone(),
        [one, rename, plain].concat(),
        event::build(event::FAN_Q_OVERFLOW, -1, &[]),
    ]
}

/// A parsed event, rebuilt, parses to itself.
fn fanotify_check(input: &[u8]) -> Result<(), String> {
    let Ok(events) = event::parse(input) else {
        return Ok(());
    };
    for e in events {
        if e.fids
            .iter()
            .any(|f| f.name.as_ref().is_some_and(|n| n.contains(&0)))
        {
            return Err("a name with a NUL in it".into());
        }
        let again = event::parse(&event::build(e.mask, e.pid, &e.fids))
            .map_err(|err| format!("rebuilt event doesn't parse: {err:?}"))?;
        if again != [e.clone()] {
            return Err(format!("round trip changed {e:?} into {again:?}"));
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------
// refs: checkpoint references (the D-Bus service parses them for any local caller)

fn refs_seeds() -> Vec<Vec<u8>> {
    [
        "c42",
        "@{1}",
        "@{now}",
        "now",
        "@{boot}",
        "@{boot-1}",
        "@{ms:1760000000000}",
    ]
    .iter()
    .map(|s| s.as_bytes().to_vec())
    .collect()
}

// Every target has the same signature.
#[allow(clippy::unnecessary_wraps)]
fn refs_check(input: &[u8]) -> Result<(), String> {
    let _ = osreflog_ledger::refs::parse(&String::from_utf8_lossy(input));
    Ok(())
}

// ---------------------------------------------------------------------------------------
// textdiff: the CLI's line diff

fn textdiff_seeds() -> Vec<Vec<u8>> {
    etc_files(500)
        .into_iter()
        .map(|(_, t)| t.into_bytes())
        .collect()
}

fn textdiff_gen(g: &mut Gen) -> Vec<u8> {
    let before = g.seed();
    let after = if g.rng.u8(..4) == 0 {
        g.mutated()
    } else {
        g.mutate(before.clone())
    };
    [before, vec![0], after].concat()
}

fn textdiff_check(input: &[u8]) -> Result<(), String> {
    let (b, a) = split_seed(input);
    let (before, after) = (String::from_utf8_lossy(b), String::from_utf8_lossy(a));
    let (la, lb): (Vec<&str>, Vec<&str>) = (before.lines().collect(), after.lines().collect());
    let edits = textdiff::diff_lines(&la, &lb);
    let side = |skip: textdiff::Edit| -> Vec<&str> {
        edits
            .iter()
            .filter(|(e, _)| *e != skip)
            .map(|(_, l)| *l)
            .collect()
    };
    if side(textdiff::Edit::Add) != la || side(textdiff::Edit::Del) != lb {
        return Err("the edit script doesn't rebuild both sides".into());
    }
    let u = textdiff::unified(&before, &after, 3);
    if u.lines()
        .any(|l| !matches!(l.chars().next(), Some(' ' | '-' | '+' | '@') | None))
    {
        return Err("a unified line without a mark".into());
    }
    if (la == lb) != u.is_empty() {
        return Err("empty diff iff equal".into());
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------
// Running

fn panic_message(p: &Box<dyn std::any::Any + Send>) -> String {
    p.downcast_ref::<&str>()
        .map(ToString::to_string)
        .or_else(|| p.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "panic".into())
}

fn run_one(t: &Target, input: &[u8]) -> (Result<(), String>, Duration) {
    let started = Instant::now();
    let r = std::panic::catch_unwind(|| (t.check)(input))
        .unwrap_or_else(|p| Err(format!("panic: {}", panic_message(&p))));
    let took = started.elapsed();
    match r {
        Ok(()) if took > SLOW => (Err(format!("slow: {} ms", took.as_millis())), took),
        r => (r, took),
    }
}

#[derive(Default)]
struct Stats {
    execs: u64,
    findings: usize,
    slowest: Duration,
    largest: usize,
}

fn fuzz(t: &Target, secs: u64, seed: u64, out: &Path) -> std::io::Result<Stats> {
    std::fs::create_dir_all(out)?;
    let seeds = (t.seeds)();
    println!(
        "{}: seed {seed}, {} seeds, {secs} s, findings in {}",
        t.name,
        seeds.len(),
        out.display()
    );
    let mut g = Gen::new(seed, seeds);
    let current = out.join(format!("{}.current", t.name));
    let started = Instant::now();
    let deadline = started + Duration::from_secs(secs);
    let mut next_report = started + Duration::from_secs(60);
    let mut stats = Stats::default();
    while Instant::now() < deadline {
        let input = (t.generate)(&mut g);
        if input.len() >= PERSIST_FROM {
            std::fs::write(&current, &input)?;
        }
        let (r, took) = run_one(t, &input);
        stats.execs += 1;
        stats.slowest = stats.slowest.max(took);
        stats.largest = stats.largest.max(input.len());
        if let Err(why) = r {
            stats.findings += 1;
            println!("FINDING {} #{}: {why}", t.name, stats.findings);
            if stats.findings <= MAX_FINDINGS {
                let base = out.join(format!("{}-{}", t.name, stats.findings));
                std::fs::write(base.with_extension("bin"), &input)?;
                std::fs::write(base.with_extension("txt"), &why)?;
            }
        }
        let now = Instant::now();
        if now >= next_report {
            next_report = now + Duration::from_secs(60);
            report(t.name, &stats, started.elapsed());
        }
    }
    report(t.name, &stats, started.elapsed());
    let _ = std::fs::remove_file(&current);
    Ok(stats)
}

// Rates are for display only.
#[allow(clippy::cast_precision_loss)]
fn report(name: &str, s: &Stats, elapsed: Duration) {
    println!(
        "{name}: {:>6} s  {:>11} execs  {:>8.0}/s  slowest {:>4} ms  largest {:>6} B  findings {}",
        elapsed.as_secs(),
        s.execs,
        s.execs as f64 / elapsed.as_secs_f64().max(0.001),
        s.slowest.as_millis(),
        s.largest,
        s.findings
    );
}

fn target(name: &str) -> Option<&'static Target> {
    TARGETS.iter().find(|t| t.name == name)
}

fn usage() -> ExitCode {
    let names: Vec<&str> = TARGETS.iter().map(|t| t.name).collect();
    eprintln!(
        "usage: osreflog-fuzz <target> [--secs N] [--seed S] [--out DIR]\n       osreflog-fuzz replay <target> <file>\n       osreflog-fuzz vercmp-pairs <n> [--seed S]\ntargets: {}",
        names.join(", ")
    );
    ExitCode::from(2)
}

fn flag(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1).cloned())
}

/// Caps the address space, so a runaway allocation fails here instead of on the machine.
fn limit_memory() {
    use rustix::process::{Resource, Rlimit, setrlimit};
    let gib = Some(2u64 << 30);
    let _ = setrlimit(
        Resource::As,
        Rlimit {
            current: gib,
            maximum: gib,
        },
    );
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    std::panic::set_hook(Box::new(|_| {}));
    limit_memory();
    let seed = flag(&args, "--seed")
        .and_then(|s| s.parse().ok())
        .unwrap_or_else(|| fastrand::u64(..));
    match args.first().map(String::as_str) {
        Some("replay") => {
            let (Some(t), Some(file)) = (args.get(1).and_then(|n| target(n)), args.get(2)) else {
                return usage();
            };
            let Ok(input) = std::fs::read(file) else {
                eprintln!("can't read {file}");
                return ExitCode::from(2);
            };
            let (r, took) = run_one(t, &input);
            println!("{}: {r:?} in {} ms", t.name, took.as_millis());
            if r.is_ok() {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            }
        }
        Some("vercmp-pairs") => {
            let count: usize = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(10_000);
            let mut g = Gen::new(seed, Vec::new());
            for _ in 0..count {
                let (left, right) = (version(&mut g), version(&mut g));
                let r = match rpmvercmp(&left, &right) {
                    Ordering::Less => -1,
                    Ordering::Equal => 0,
                    Ordering::Greater => 1,
                };
                println!("{left}\t{right}\t{r}");
            }
            ExitCode::SUCCESS
        }
        Some(name) => {
            let Some(t) = target(name) else {
                return usage();
            };
            let secs = flag(&args, "--secs")
                .and_then(|s| s.parse().ok())
                .unwrap_or(60);
            let out = PathBuf::from(flag(&args, "--out").unwrap_or_else(|| "fuzz-out".into()));
            match fuzz(t, secs, seed, &out) {
                Ok(s) if s.findings == 0 => ExitCode::SUCCESS,
                Ok(_) => ExitCode::FAILURE,
                Err(e) => {
                    eprintln!("{}: {e}", t.name);
                    ExitCode::from(2)
                }
            }
        }
        None => usage(),
    }
}
