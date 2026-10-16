//! `osreflogd --boot-doctor`: after every boot of the default system, put `/boot` back as it
//! was and remove probe clones nothing is waiting for. Run by `osreflog-boot-doctor.service`
//! (never inside a probe) and by `osreflog doctor --boot`.
//!
//! - `/boot`: anything a probe left (its BLS entry, `/boot/osreflog/`, a `next_entry` naming
//!   the probe) is removed, always. If this boot is the default, a probe is either done or
//!   didn't boot; either way nothing should boot it again unasked. Someone else's
//!   `next_entry` is reported and left alone.
//! - The default entry: checked against the record taken when the probe was armed (the
//!   `pending` file), or, with no probe pending, checked to be one fixed entry.
//! - Clones under `osreflog/probes/`: kept when `pending.json` names them (the bisect still
//!   has to read the probe's boot record and verdict), deleted otherwise. An unreadable
//!   `pending.json` keeps every clone: deleting is the one step that can't be taken back.

use std::os::fd::AsFd as _;
use std::path::{Path, PathBuf};

use osreflog_boot::{Boot, BootError, DefaultRecord, probe};
use rustix::mount::{MountFlags, UnmountFlags, mount, unmount};
use serde::{Deserialize, Serialize};

pub const FLAG: &str = "--boot-doctor";
/// Probe clones, from the btrfs top level.
pub const PROBES: &str = "osreflog/probes";
pub const PENDING: &str = "pending.json";
const RUN_DIR: &str = "/run/osreflog-boot-doctor";

/// A metal probe armed and not yet read back: written by the bisect before it arms, removed
/// when it has the verdict.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pending {
    pub n: u32,
    /// The clone's name in `osreflog/probes/`, and its throwaway home's.
    pub clone: String,
    #[serde(default)]
    pub home: Option<String>,
    /// The default entry when the probe was armed; none for a probe that doesn't boot the
    /// machine (a manual container probe).
    #[serde(default)]
    pub record: Option<DefaultRecord>,
}

#[derive(Debug, Default)]
pub struct Report {
    pub lines: Vec<String>,
    /// Things a person must look at: the default changed, a clone couldn't be deleted.
    pub problems: Vec<String>,
}

impl Report {
    fn say(&mut self, s: impl Into<String>) {
        self.lines.push(s.into());
    }

    fn problem(&mut self, s: impl Into<String>) {
        let s = s.into();
        self.lines.push(format!("PROBLEM: {s}"));
        self.problems.push(s);
    }
}

/// Reads `pending.json`: `Ok(None)` when there is none, `Err` when it can't be trusted.
fn pending(probes: &Path) -> Result<Option<Pending>, String> {
    match std::fs::read(probes.join(PENDING)) {
        Ok(data) => serde_json::from_slice(&data)
            .map(Some)
            .map_err(|e| format!("{PENDING} is unreadable ({e}); every clone kept")),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("{PENDING}: {e}; every clone kept")),
    }
}

fn is_name(s: &str) -> bool {
    !s.is_empty() && !s.contains('/') && !s.starts_with('.')
}

/// Step 1: everything osreflog left on `/boot` goes, whatever the bisect's state.
fn clean_boot(boot: &Boot, dry_run: bool, r: &mut Report) {
    let would = if dry_run { "would remove" } else { "removed" };
    match boot.leftovers() {
        Ok(left) => {
            if left.next_entry.as_deref() == Some(osreflog_boot::PROBE_ID) {
                r.say(format!(
                    "the armed probe did not boot (next_entry still set): {}",
                    if dry_run {
                        "would disarm it"
                    } else {
                        "disarming"
                    }
                ));
            } else if let Some(other) = &left.next_entry {
                r.say(format!("next_entry={other} is not osreflog's: left alone"));
            }
            if left.entry
                || !left.files.is_empty()
                || left.next_entry.as_deref() == Some(osreflog_boot::PROBE_ID)
            {
                // "the probe entry and 2 kernel files", naming only what is there.
                let mut what = Vec::new();
                if left.entry {
                    what.push("the probe entry".to_owned());
                }
                if !left.files.is_empty() {
                    what.push(format!(
                        "{} kernel file(s) in /boot/osreflog",
                        left.files.len()
                    ));
                }
                if left.next_entry.as_deref() == Some(osreflog_boot::PROBE_ID) {
                    what.push("next_entry".to_owned());
                }
                let what = what.join(", ");
                if dry_run {
                    r.say(format!("{would} {what}"));
                } else {
                    match boot.disarm() {
                        Ok(_) => r.say(format!("{would} {what}")),
                        Err(e) => r.problem(format!("cleaning /boot: {e}")),
                    }
                }
            } else {
                r.say("/boot: nothing of osreflog's");
            }
        }
        Err(e) => r.problem(format!("reading /boot: {e}")),
    }
}

/// The check itself, on `boot` and the probes directory `probes` (inside the top-level mount).
/// `delete` removes a subvolume `name` in `probes`. With `dry_run`, it only says what it would
/// do.
pub fn check(
    boot: &Boot,
    probes: &Path,
    dry_run: bool,
    mut delete: impl FnMut(&Path, &str) -> std::io::Result<()>,
) -> Report {
    let mut r = Report::default();
    let would = if dry_run { "would remove" } else { "removed" };

    clean_boot(boot, dry_run, &mut r);

    // 2. The default entry.
    let pending = pending(probes);
    match &pending {
        Ok(Some(Pending {
            n,
            record: Some(rec),
            ..
        })) => match boot.verify(rec) {
            Ok(()) => r.say(format!(
                "default {} untouched since probe {n} was armed",
                rec.saved_entry
            )),
            Err(e) => r.problem(format!("{e} (since probe {n} was armed)")),
        },
        Ok(_) | Err(_) => match boot.default_record() {
            Ok(rec) => r.say(format!("default {}", rec.saved_entry)),
            Err(BootError::Refused(why)) => {
                r.say(format!("metal probes would be refused here: {why}"));
            }
            Err(e) => r.problem(format!("the default entry: {e}")),
        },
    }

    // 3. Clones.
    let keep: Vec<String> = match &pending {
        Ok(Some(p)) => {
            if p.record.is_none() {
                r.say(format!("probe {} (a container) waits for a verdict", p.n));
            } else if is_name(&p.clone) {
                match probe::booted(&probes.join(&p.clone), p.n) {
                    Some(b) => r.say(format!(
                        "probe {} booted (kernel {}); its result waits for the bisect",
                        p.n, b.kernel
                    )),
                    None => r.say(format!("probe {} has no boot record: it never booted", p.n)),
                }
            }
            std::iter::once(p.clone.clone())
                .chain(p.home.clone())
                .collect()
        }
        Ok(None) => Vec::new(),
        Err(e) => {
            r.problem(e.clone());
            return r;
        }
    };
    let mut names: Vec<String> = std::fs::read_dir(probes)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| is_name(n) && !keep.contains(n))
        .collect();
    names.sort();
    for name in names {
        if dry_run {
            r.say(format!("{would} clone {name}"));
        } else {
            match delete(probes, &name) {
                Ok(()) => r.say(format!("{would} clone {name}")),
                Err(e) => r.problem(format!("deleting clone {name}: {e}")),
            }
        }
    }
    r
}

/// The device of the btrfs filesystem mounted at `/`.
fn root_device() -> Result<String, String> {
    crate::layout::mount_at(Path::new("/"))
        .map_err(|e| e.to_string())
        .and_then(|m| {
            if m.fs_type == "btrfs" {
                Ok(m.source)
            } else {
                Err(format!("/ is {}, not btrfs", m.fs_type))
            }
        })
}

/// Entry point: mounts the top level privately, checks, prints the report. Exit 0 when there
/// was nothing for a person to look at, 1 otherwise, 2 when it couldn't run.
#[must_use]
pub fn main(args: &[String]) -> i32 {
    let dry_run = args.iter().any(|a| a == "--dry-run");
    let device = match root_device() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("osreflog boot doctor: {e}");
            return 2;
        }
    };
    let top = PathBuf::from(RUN_DIR).join("top");
    if let Err(e) = std::fs::create_dir_all(&top) {
        eprintln!("osreflog boot doctor: creating {}: {e}", top.display());
        return 2;
    }
    match crate::layout::would_propagate(&top) {
        Ok(false) => {}
        Ok(true) => {
            eprintln!(
                "osreflog boot doctor: refusing to mount where it would propagate (run me with PrivateMounts=yes)"
            );
            return 2;
        }
        Err(e) => {
            eprintln!("osreflog boot doctor: {e}");
            return 2;
        }
    }
    if let Err(e) = mount(
        device.as_str(),
        &top,
        "btrfs",
        MountFlags::NOSUID | MountFlags::NODEV | MountFlags::NOEXEC | MountFlags::NOATIME,
        c"subvolid=5",
    ) {
        eprintln!("osreflog boot doctor: mounting the top level of {device}: {e}");
        return 2;
    }
    let report = check(
        &Boot::new("/boot"),
        &top.join(PROBES),
        dry_run,
        |dir, name| {
            let d = std::fs::File::open(dir)?;
            osreflog_btrfs::ioctl::delete_subvolume(d.as_fd(), name)
        },
    );
    let _ = unmount(&top, UnmountFlags::DETACH);
    println!(
        "osreflog boot doctor{}",
        if dry_run { " (dry run)" } else { "" }
    );
    for l in &report.lines {
        println!("  {l}");
    }
    i32::from(!report.problems.is_empty())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    const ID: &str = "abc-7.2.9";

    struct Fake {
        _dir: tempfile::TempDir,
        boot: Boot,
        probes: PathBuf,
    }

    fn fake() -> Option<Fake> {
        let editenv = PathBuf::from("/usr/bin/grub2-editenv");
        if !editenv.is_file() {
            return None;
        }
        let dir = tempfile::tempdir().unwrap();
        let mut boot = Boot::new(dir.path().join("boot"));
        boot.editenv = editenv;
        std::fs::create_dir_all(boot.entries_dir()).unwrap();
        std::fs::create_dir_all(boot.root.join("grub2")).unwrap();
        std::fs::write(
            boot.entries_dir().join(format!("{ID}.conf")),
            "linux /vmlinuz-7.2.9\ninitrd /initramfs-7.2.9.img\noptions root=UUID=x ro rootflags=subvol=root\n",
        )
        .unwrap();
        std::fs::write(boot.root.join("vmlinuz-7.2.9"), "k").unwrap();
        std::fs::write(boot.root.join("initramfs-7.2.9.img"), "i").unwrap();
        let env = boot.grubenv_path();
        osreflog_boot::grubenv::edit(&boot.editenv, &env, &["create"]).unwrap();
        osreflog_boot::grubenv::edit(&boot.editenv, &env, &["set", &format!("saved_entry={ID}")])
            .unwrap();
        let probes = dir.path().join("probes");
        std::fs::create_dir_all(&probes).unwrap();
        Some(Fake {
            _dir: dir,
            boot,
            probes,
        })
    }

    /// Arms probe `n` as the bisect would: clone dirs, kernel files, entry, `next_entry`, `pending.json`.
    #[allow(clippy::many_single_char_names)]
    fn arm(f: &Fake, n: u32) -> Pending {
        let clone = format!("p{n}");
        std::fs::create_dir_all(f.probes.join(&clone).join("etc")).unwrap();
        std::fs::create_dir_all(f.probes.join(format!("{clone}-home"))).unwrap();
        let rec = f.boot.default_record().unwrap();
        let plan = osreflog_boot::kernel::Plan::Install {
            version: "7.1.6".into(),
            vmlinuz: PathBuf::new(),
        };
        let (k, i) = plan.install_paths(&f.boot.root).unwrap();
        osreflog_boot::write_synced(&k, b"k").unwrap();
        osreflog_boot::write_synced(&i, b"i").unwrap();
        f.boot
            .write_probe_entry(&rec, n, &format!("osreflog/probes/{clone}"), &plan)
            .unwrap();
        f.boot.arm(&rec).unwrap();
        let p = Pending {
            n,
            clone: clone.clone(),
            home: Some(format!("{clone}-home")),
            record: Some(rec),
        };
        std::fs::write(f.probes.join(PENDING), serde_json::to_vec(&p).unwrap()).unwrap();
        p
    }

    fn deleter(deleted: &mut Vec<String>) -> impl FnMut(&Path, &str) -> std::io::Result<()> + '_ {
        |dir, name| {
            std::fs::remove_dir_all(dir.join(name))?;
            deleted.push(name.to_owned());
            Ok(())
        }
    }

    #[test]
    fn a_probe_that_never_booted_is_disarmed_and_its_clone_kept() {
        let Some(f) = fake() else { return };
        arm(&f, 3);
        std::fs::create_dir_all(f.probes.join("p1")).unwrap(); // an older, forgotten clone
        let mut deleted = Vec::new();
        let r = check(&f.boot, &f.probes, false, deleter(&mut deleted));
        assert!(r.problems.is_empty(), "{:?}", r.lines);
        assert!(
            r.lines.iter().any(|l| l.contains("did not boot")),
            "{:?}",
            r.lines
        );
        assert!(
            r.lines.iter().any(|l| l.contains("never booted")),
            "{:?}",
            r.lines
        );
        assert_eq!(deleted, vec!["p1"]);
        assert_eq!(
            f.boot.leftovers().unwrap(),
            osreflog_boot::Leftovers::default()
        );
        assert!(f.probes.join("p3").is_dir() && f.probes.join("p3-home").is_dir());
    }

    #[test]
    fn a_probe_that_booted_is_reported_and_kept_until_the_bisect_reads_it() {
        let Some(f) = fake() else { return };
        let p = arm(&f, 4);
        // GRUB cleared next_entry as it booted the probe; the probe wrote its record.
        osreflog_boot::grubenv::edit(
            &f.boot.editenv,
            &f.boot.grubenv_path(),
            &["set", "next_entry="],
        )
        .unwrap();
        let dir = f.probes.join(&p.clone).join(probe::PROBE_DIR);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("booted"), "probe=4\nbooted=1\nkernel=7.1.6\n").unwrap();
        let mut deleted = Vec::new();
        let r = check(&f.boot, &f.probes, false, deleter(&mut deleted));
        assert!(r.problems.is_empty(), "{:?}", r.lines);
        assert!(
            r.lines
                .iter()
                .any(|l| l.contains("probe 4 booted (kernel 7.1.6)")),
            "{:?}",
            r.lines
        );
        assert_eq!(deleted, Vec::<String>::new());
        assert!(!f.boot.probe_entry_path().exists());
    }

    #[test]
    fn with_nothing_pending_every_clone_goes() {
        let Some(f) = fake() else { return };
        arm(&f, 5);
        std::fs::remove_file(f.probes.join(PENDING)).unwrap();
        let mut deleted = Vec::new();
        let r = check(&f.boot, &f.probes, false, deleter(&mut deleted));
        assert!(r.problems.is_empty(), "{:?}", r.lines);
        assert_eq!(deleted, vec!["p5", "p5-home"]);
        assert!(
            r.lines.iter().any(|l| l == &format!("default {ID}")),
            "{:?}",
            r.lines
        );
    }

    #[test]
    fn a_dry_run_changes_nothing() {
        let Some(f) = fake() else { return };
        arm(&f, 6);
        std::fs::remove_file(f.probes.join(PENDING)).unwrap();
        let before = f.boot.leftovers().unwrap();
        let r = check(&f.boot, &f.probes, true, |_, _| {
            panic!("deleted in a dry run")
        });
        assert!(
            r.lines.iter().any(|l| l == "would remove clone p6"),
            "{:?}",
            r.lines
        );
        assert_eq!(f.boot.leftovers().unwrap(), before);
    }

    #[test]
    fn a_changed_default_is_a_problem() {
        let Some(f) = fake() else { return };
        arm(&f, 7);
        std::fs::write(
            f.boot.entries_dir().join(format!("{ID}.conf")),
            "linux /vmlinuz-7.2.9\n# edited\n",
        )
        .unwrap();
        let r = check(&f.boot, &f.probes, false, |_, _| Ok(()));
        assert_eq!(r.problems.len(), 1, "{:?}", r.lines);
        assert!(r.problems[0].contains("since probe 7 was armed"));
    }

    #[test]
    fn an_unreadable_pending_file_keeps_every_clone() {
        let Some(f) = fake() else { return };
        arm(&f, 8);
        std::fs::write(f.probes.join(PENDING), "{not json").unwrap();
        let r = check(&f.boot, &f.probes, false, |_, _| {
            panic!("deleted with an unreadable pending file")
        });
        assert_eq!(r.problems.len(), 1, "{:?}", r.lines);
        // /boot is still cleaned: nothing should boot a probe unasked.
        assert!(!f.boot.probe_entry_path().exists());
    }

    #[test]
    fn someone_elses_one_shot_entry_is_left_alone() {
        let Some(f) = fake() else { return };
        osreflog_boot::grubenv::edit(
            &f.boot.editenv,
            &f.boot.grubenv_path(),
            &["set", "next_entry=other"],
        )
        .unwrap();
        let r = check(&f.boot, &f.probes, false, |_, _| Ok(()));
        assert!(
            r.lines.iter().any(|l| l.contains("not osreflog's")),
            "{:?}",
            r.lines
        );
        assert_eq!(f.boot.env().unwrap()["next_entry"], "other");
    }
}
