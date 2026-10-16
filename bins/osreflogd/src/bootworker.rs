//! `osreflogd --boot-worker '<job>'`: the one step of a metal bisect that writes `/boot`,
//! started by the daemon as its own transient unit (`PrivateMounts=yes`, outside the daemon's
//! sandbox, where `/boot` is read-only). It mounts the btrfs top level privately to reach the
//! prepared clone, then:
//!
//! - **arm**: records the default entry, plans the probe's kernel (reused from `/boot`, or
//!   copied to `/boot/osreflog/` with an initramfs built from the clone), writes the probe's BLS
//!   entry, writes `pending.json` (so the boot doctor keeps the clones and checks the default),
//!   and arms the entry for the next boot only. On any failure it disarms again.
//! - **disarm**: removes what an arm left on `/boot` (a verdict given before the reboot, a
//!   reset).
//!
//! Its result is a file the daemon reads once the unit has ended: `arm.json` in the clone's
//! `/var/lib/osreflog-probe/` (arm), or `disarm.json` in `osreflog/probes/` (disarm).

use std::path::{Path, PathBuf};
use std::time::Instant;

use osreflog_boot::{Boot, DefaultRecord, kernel};
use rustix::mount::{MountFlags, UnmountFlags, mount, unmount};
use serde::{Deserialize, Serialize};

use crate::bootdoctor::{PENDING, PROBES, Pending};

pub const FLAG: &str = "--boot-worker";
pub const ARM_RESULT: &str = "var/lib/osreflog-probe/arm.json";
pub const DISARM_RESULT: &str = "disarm.json";
const RUN_DIR: &str = "/run/osreflog-boot-worker";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "job", rename_all = "snake_case")]
pub enum Job {
    Arm {
        device: String,
        n: u32,
        /// The clone's name in `osreflog/probes/`, and its throwaway home's.
        clone: String,
        home: Option<String>,
    },
    Disarm {
        device: String,
    },
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Armed {
    /// The default entry as it was when the probe was armed.
    pub record: Option<DefaultRecord>,
    pub kernel: String,
    /// The kernel was copied to `/boot/osreflog/` (not reused from `/boot`).
    pub installed: bool,
    pub ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Arms probe `n` (clone `clone` in `probes`) on `boot`: everything above, testable on a
/// stand-in `/boot` with a stand-in dracut.
///
/// # Errors
/// What went wrong, after `/boot` was put back.
pub fn arm(
    boot: &Boot,
    probes: &Path,
    n: u32,
    clone: &str,
    home: Option<&str>,
    dracut: &Path,
) -> Result<Armed, String> {
    let started = Instant::now();
    let rec = boot.default_record().map_err(|e| e.to_string())?;
    let clone_path = probes.join(clone);
    let armed = (|| {
        let plan = kernel::plan(&boot.root, &clone_path).map_err(|e| e.to_string())?;
        kernel::install(&plan, &boot.root, &clone_path, dracut).map_err(|e| e.to_string())?;
        boot.write_probe_entry(&rec, n, &format!("{PROBES}/{clone}"), &plan)
            .map_err(|e| e.to_string())?;
        let pending = Pending {
            n,
            clone: clone.to_owned(),
            home: home.map(str::to_owned),
            record: Some(rec.clone()),
        };
        let json = serde_json::to_vec(&pending).map_err(|e| e.to_string())?;
        osreflog_boot::write_synced(&probes.join(PENDING), &json).map_err(|e| e.to_string())?;
        boot.arm(&rec).map_err(|e| e.to_string())?;
        Ok::<_, String>(Armed {
            record: Some(rec.clone()),
            kernel: plan.version().to_owned(),
            installed: matches!(plan, kernel::Plan::Install { .. }),
            ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
            error: None,
        })
    })();
    if armed.is_err() {
        let _ = boot.disarm();
    }
    armed
}

fn mount_top(device: &str) -> Result<PathBuf, String> {
    let top = PathBuf::from(RUN_DIR).join(std::process::id().to_string());
    std::fs::create_dir_all(&top).map_err(|e| format!("creating {}: {e}", top.display()))?;
    if crate::layout::would_propagate(&top).map_err(|e| e.to_string())? {
        return Err(
            "refusing to mount where it would propagate (run me with PrivateMounts=yes)".into(),
        );
    }
    mount(
        device,
        &top,
        "btrfs",
        MountFlags::NOSUID | MountFlags::NODEV | MountFlags::NOATIME,
        c"subvolid=5",
    )
    .map_err(|e| format!("mounting the top level of {device}: {e}"))?;
    Ok(top)
}

/// Entry point. Exit 0 when the result file was written (success or not is inside it).
#[must_use]
pub fn main(job: &str) -> i32 {
    let Ok(job) = serde_json::from_str::<Job>(job) else {
        eprintln!("bad boot job");
        return 2;
    };
    let device = match &job {
        Job::Arm { device, .. } | Job::Disarm { device } => device.clone(),
    };
    let top = match mount_top(&device) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("osreflog boot worker: {e}");
            return 2;
        }
    };
    let probes = top.join(PROBES);
    let boot = Boot::new("/boot");
    let code = match job {
        Job::Arm { n, clone, home, .. } => {
            let result = arm(
                &boot,
                &probes,
                n,
                &clone,
                home.as_deref(),
                Path::new("dracut"),
            )
            .unwrap_or_else(|e| Armed {
                error: Some(e),
                ..Armed::default()
            });
            match serde_json::to_string(&result) {
                Ok(json) => write_result(&probes.join(&clone), ARM_RESULT, &json),
                Err(_) => 1,
            }
        }
        Job::Disarm { .. } => {
            let result = boot.disarm().map_or_else(
                |e| serde_json::json!({ "error": e.to_string() }).to_string(),
                |left| serde_json::to_string(&left).unwrap_or_default(),
            );
            match osreflog_boot::write_synced(&probes.join(DISARM_RESULT), result.as_bytes()) {
                Ok(()) => 0,
                Err(_) => 1,
            }
        }
    };
    let _ = unmount(&top, UnmountFlags::DETACH);
    let _ = std::fs::remove_dir(&top);
    code
}

fn write_result(clone: &Path, rel: &str, json: &str) -> i32 {
    match osreflog_undo::beneath::Beneath::open(clone).and_then(|b| b.write(rel, json, 0o644)) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("osreflog boot worker: writing the result: {e}");
            1
        }
    }
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
        bin: PathBuf,
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
        std::fs::write(boot.root.join("vmlinuz-7.2.9"), "k929").unwrap();
        std::fs::write(boot.root.join("initramfs-7.2.9.img"), "i").unwrap();
        let env = boot.grubenv_path();
        osreflog_boot::grubenv::edit(&boot.editenv, &env, &["create"]).unwrap();
        osreflog_boot::grubenv::edit(&boot.editenv, &env, &["set", &format!("saved_entry={ID}")])
            .unwrap();
        let probes = dir.path().join("probes");
        let bin = dir.path().join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        Some(Fake {
            _dir: dir,
            boot,
            probes,
            bin,
        })
    }

    fn clone_with_kernel(f: &Fake, name: &str, version: &str, image: &str) {
        let d = f.probes.join(name).join("usr/lib/modules").join(version);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("vmlinuz"), image).unwrap();
    }

    fn dracut(f: &Fake, works: bool) -> PathBuf {
        use std::os::unix::fs::PermissionsExt as _;
        let p = f.bin.join(if works { "dracut" } else { "dracut-broken" });
        let body = if works {
            "#!/bin/sh\nfor a; do last=$a; done\necho initramfs > \"$last\"\n"
        } else {
            "#!/bin/sh\necho 'dracut: broken' >&2\nexit 1\n"
        };
        std::fs::write(&p, body).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        p
    }

    #[test]
    fn arm_reuses_the_installed_kernel_and_records_pending() {
        let Some(f) = fake() else { return };
        clone_with_kernel(&f, "p1", "7.2.9", "k929");
        let a = arm(
            &f.boot,
            &f.probes,
            1,
            "p1",
            Some("p1-home"),
            &dracut(&f, true),
        )
        .unwrap();
        assert_eq!(a.kernel, "7.2.9");
        assert!(!a.installed);
        assert_eq!(a.record.as_ref().unwrap().saved_entry, ID);
        let env = f.boot.env().unwrap();
        assert_eq!(env["next_entry"], osreflog_boot::PROBE_ID);
        assert_eq!(env["saved_entry"], ID);
        let p: Pending =
            serde_json::from_slice(&std::fs::read(f.probes.join(PENDING)).unwrap()).unwrap();
        assert_eq!(
            (p.n, p.clone.as_str(), p.home.as_deref()),
            (1, "p1", Some("p1-home"))
        );
        assert_eq!(p.record, a.record);
        let entry = std::fs::read_to_string(f.boot.probe_entry_path()).unwrap();
        assert!(
            entry.contains("rootflags=subvol=osreflog/probes/p1"),
            "{entry}"
        );
        f.boot.verify(a.record.as_ref().unwrap()).unwrap();
    }

    #[test]
    fn arm_installs_an_older_kernel_from_the_clone() {
        let Some(f) = fake() else { return };
        clone_with_kernel(&f, "p2", "7.2.8", "k928");
        let a = arm(&f.boot, &f.probes, 2, "p2", None, &dracut(&f, true)).unwrap();
        assert_eq!(a.kernel, "7.2.8");
        assert!(a.installed);
        assert_eq!(
            std::fs::read_to_string(f.boot.root.join("osreflog/vmlinuz-7.2.8")).unwrap(),
            "k928"
        );
        assert!(f.boot.root.join("osreflog/initramfs-7.2.8.img").is_file());
    }

    #[test]
    fn a_failed_arm_leaves_boot_as_it_was() {
        let Some(f) = fake() else { return };
        clone_with_kernel(&f, "p3", "7.2.8", "k928");
        let e = arm(&f.boot, &f.probes, 3, "p3", None, &dracut(&f, false)).unwrap_err();
        assert!(e.contains("dracut"), "{e}");
        assert_eq!(
            f.boot.leftovers().unwrap(),
            osreflog_boot::Leftovers::default()
        );
        assert!(!f.probes.join(PENDING).exists());

        // No kernel in the clone at all.
        std::fs::create_dir_all(f.probes.join("p4")).unwrap();
        assert!(arm(&f.boot, &f.probes, 4, "p4", None, &dracut(&f, true)).is_err());
        assert_eq!(
            f.boot.leftovers().unwrap(),
            osreflog_boot::Leftovers::default()
        );
    }

    #[test]
    fn arm_refuses_a_default_that_isnt_pinned() {
        let Some(f) = fake() else { return };
        clone_with_kernel(&f, "p5", "7.2.9", "k929");
        osreflog_boot::grubenv::edit(
            &f.boot.editenv,
            &f.boot.grubenv_path(),
            &["unset", "saved_entry"],
        )
        .unwrap();
        let e = arm(&f.boot, &f.probes, 5, "p5", None, &dracut(&f, true)).unwrap_err();
        assert!(e.contains("saved_entry"), "{e}");
        assert!(!f.boot.probe_entry_path().exists());
    }

    #[test]
    fn jobs_round_trip() {
        let j = Job::Arm {
            device: "/dev/sda3".into(),
            n: 2,
            clone: "p2".into(),
            home: Some("p2-home".into()),
        };
        let s = serde_json::to_string(&j).unwrap();
        assert!(s.contains("\"job\":\"arm\""), "{s}");
        assert_eq!(serde_json::from_str::<Job>(&s).unwrap(), j);
    }
}
