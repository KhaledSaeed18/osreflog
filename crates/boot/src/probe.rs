//! A metal probe's own root: what changes in the clone so it boots as a throwaway copy of the
//! system that can't touch the real one.
//!
//! - `/etc/fstab`: `/` mounts the probe's subvolume and `/home` a throwaway snapshot of the
//!   home subvolume, so the probe's desktop session can't write to the real `~/.config`.
//! - osreflogd and the user agent get `ConditionKernelCommandLine=!osreflog.probe`: inside a
//!   probe they'd capture and checkpoint into the real history. The dnf hook goes too.
//! - `osreflog-metal-probe.service` records that the probe booted (`/var/lib/osreflog-probe/
//!   booted`, read back from the clone after the reboot), and the probe says what it is at
//!   login (motd) and in the desktop (a notification).
//!
//! Nothing of the real system is mounted read-write in a probe except the `/boot` and ESP
//! lines kept from fstab. The verdict comes back the way the boot record does: a file in the
//! clone, read by the daemon once the default has booted again.
//!
//! Every write goes through `openat2` beneath the clone with symlinks refused: the clone holds
//! whatever the system held, and an absolute symlink in it would otherwise lead a write out of
//! the clone and into the real root.

use std::path::Path;

use osreflog_undo::beneath::Beneath;
use serde::{Deserialize, Serialize};

use crate::BootError;

/// Inside a probe: the boot record and, later, the verdict.
pub const PROBE_DIR: &str = "var/lib/osreflog-probe";
pub const UNIT: &str = "osreflog-metal-probe.service";
/// Units that must not run in a probe, with the drop-in that stops them.
const KEPT_OUT: [&str; 2] = [
    "etc/systemd/system/osreflogd.service.d",
    "etc/systemd/user/osreflog-user.service.d",
];
const DNF_HOOK: &str = "etc/dnf/libdnf5-plugins/actions.d/osreflog.actions";
const DROP_IN: &str = "\
# osreflog: never in a bisect probe (it would record into the real history). A booted
# container shares the host's kernel command line, so the marker file says it too.
[Unit]
ConditionKernelCommandLine=!osreflog.probe
ConditionPathExists=!/var/lib/osreflog-probe/manual
";
/// In every probe that waits for a person's verdict (metal, or a booted container): its number.
/// `osreflog bisect good|bad|skip` run inside sees it and writes [`VERDICT`] instead of asking
/// the daemon.
pub const MARKER: &str = "var/lib/osreflog-probe/manual";
/// The verdict, `good`, `bad` or `skip`, written inside the probe (by the CLI in the probe, or
/// by the daemon for a verdict given on the host) and read back by the daemon.
pub const VERDICT: &str = "var/lib/osreflog-probe/verdict";

/// Where a probe's `/home` comes from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Home {
    /// `/home` is a subvolume of the same btrfs (`from`); the probe mounts a snapshot of it.
    Throwaway { from: String },
    /// No `/home` line: it's a directory of `/`, so it is in the probe's clone already.
    InRoot,
    /// `/home` is another filesystem: the probe would mount the real one.
    Real { spec: String },
}

/// One fstab line's fields, or `None` for a comment or blank line.
fn fields(line: &str) -> Option<Vec<&str>> {
    let t = line.trim();
    if t.is_empty() || t.starts_with('#') {
        return None;
    }
    Some(t.split_whitespace().collect())
}

fn subvol_of(options: &str) -> Option<&str> {
    options
        .split(',')
        .find_map(|o| o.strip_prefix("subvol="))
        .map(|s| s.trim_start_matches('/'))
}

/// Where `/home` comes from, by the system's fstab.
#[must_use]
pub fn home_of(fstab: &str) -> Home {
    let lines: Vec<Vec<&str>> = fstab.lines().filter_map(fields).collect();
    let root = lines.iter().find(|f| f.get(1) == Some(&"/"));
    let Some(home) = lines.iter().find(|f| f.get(1) == Some(&"/home")) else {
        return Home::InRoot;
    };
    let same_btrfs =
        root.is_some_and(|r| r.first() == home.first()) && home.get(2) == Some(&"btrfs");
    match home.get(3).and_then(|o| subvol_of(o)) {
        Some(from) if same_btrfs && !from.is_empty() => Home::Throwaway {
            from: from.to_owned(),
        },
        _ => Home::Real {
            spec: home.first().copied().unwrap_or_default().to_owned(),
        },
    }
}

fn with_subvol(options: &str, subvol: &str) -> String {
    let mut kept: Vec<&str> = options
        .split(',')
        .filter(|o| {
            !o.is_empty()
                && *o != "defaults"
                && !o.starts_with("subvol=")
                && !o.starts_with("subvolid=")
        })
        .collect();
    let s = format!("subvol={subvol}");
    kept.insert(0, &s);
    kept.join(",")
}

/// The probe's fstab: `/` on `root_subvol`, `/home` on `home_subvol` when it's a throwaway
/// snapshot; every other line as it was (the original is kept as a comment above a changed
/// line). Errors when there's no btrfs `/` line to point at the probe.
///
/// # Errors
/// If fstab has no btrfs `/` line.
pub fn rewrite_fstab(
    fstab: &str,
    root_subvol: &str,
    home_subvol: Option<&str>,
) -> Result<String, BootError> {
    use std::fmt::Write as _;
    let mut out = format!("{FSTAB_HEADER}\n");
    let mut had_root = false;
    for line in original_fstab(fstab).lines() {
        let target = match fields(line) {
            Some(f) if f.len() >= 4 && f[1] == "/" && f[2] == "btrfs" => {
                had_root = true;
                Some((f, root_subvol))
            }
            Some(f) if f.len() >= 4 && f[1] == "/home" && f[2] == "btrfs" => {
                home_subvol.map(|h| (f, h))
            }
            _ => None,
        };
        if let Some((f, subvol)) = target {
            let mut f: Vec<String> = f.iter().map(|s| (*s).to_owned()).collect();
            f[3] = with_subvol(&f[3], subvol);
            let _ = writeln!(out, "{WAS}{}\n{}", line.trim(), f.join(" "));
        } else {
            let _ = writeln!(out, "{line}");
        }
    }
    if had_root {
        Ok(out)
    } else {
        Err(BootError::Refused(
            "fstab has no btrfs / line to point at the probe".into(),
        ))
    }
}

const FSTAB_HEADER: &str =
    "# osreflog bisect probe: / and /home point at the probe's own subvolumes";
const WAS: &str = "# was: ";

/// The fstab before [`rewrite_fstab`] (itself when it was never rewritten): a probe prepared
/// twice, or a clone of a probe, starts from the system's own lines.
#[must_use]
pub fn original_fstab(fstab: &str) -> String {
    let mut out = String::new();
    let mut lines = fstab.lines();
    while let Some(line) = lines.next() {
        if line == FSTAB_HEADER {
            continue;
        }
        if let Some(orig) = line.strip_prefix(WAS) {
            out.push_str(orig);
            out.push('\n');
            lines.next(); // the rewritten line
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

/// What the probe needs to know about itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Prep {
    pub n: u32,
    /// From the btrfs top level, as in `rootflags=subvol=`.
    pub root_subvol: String,
    /// The throwaway home's subvolume, when [`Home::Throwaway`].
    pub home_subvol: Option<String>,
}

fn unit(n: u32) -> String {
    format!(
        "\
[Unit]
Description=osreflog bisect probe {n}: record that the probe booted
ConditionKernelCommandLine=osreflog.probe={n}
After=local-fs.target

[Service]
Type=oneshot
RemainAfterExit=yes
# $$ and %% are systemd's escapes for $ and %.
ExecStart=/bin/sh -c '{{ echo probe={n}; echo booted=$$(date +%%s); echo kernel=$$(uname -r); }} > /{PROBE_DIR}/booted; sync'

[Install]
WantedBy=multi-user.target
"
    )
}

fn message(n: u32) -> String {
    format!(
        "This boot is osreflog bisect probe {n}: a throwaway copy of your system (its /home too). \
         Check what you are bisecting, then run: sudo osreflog bisect good, bad or skip. \
         Rebooting returns to your normal system."
    )
}

fn autostart(n: u32) -> String {
    format!(
        "[Desktop Entry]\nType=Application\nName=osreflog bisect probe\nNoDisplay=true\n\
         Exec=notify-send --urgency=critical --app-name=osreflog \"osreflog bisect probe {n}\" \"{}\"\n",
        message(n).replace('"', "'")
    )
}

/// Turns the clone at `clone` into probe `prep.n`. Returns where its `/home` comes from.
///
/// # Errors
/// [`BootError::Refused`] when a path in the clone is a symlink or a mount point, or fstab has
/// no btrfs `/`; otherwise if a write fails.
pub fn prepare(clone: &Path, prep: &Prep) -> Result<Home, BootError> {
    let root = Beneath::open(clone)?;
    let fstab = root.read("etc/fstab")?;
    let home = home_of(&original_fstab(&fstab));
    let home_subvol = match &home {
        Home::Throwaway { .. } => prep.home_subvol.as_deref(),
        _ => None,
    };
    root.write(
        "etc/fstab",
        &rewrite_fstab(&fstab, &prep.root_subvol, home_subvol)?,
        0o644,
    )?;
    for dir in KEPT_OUT {
        root.write(&format!("{dir}/zz-osreflog-probe.conf"), DROP_IN, 0o644)?;
    }
    root.remove(DNF_HOOK)?;
    // A record from an earlier probe of the same state must not be mistaken for this boot's.
    root.remove(&format!("{PROBE_DIR}/booted"))?;
    root.remove(VERDICT)?;
    root.dir(PROBE_DIR)?;
    root.write(MARKER, &format!("{}\n", prep.n), 0o644)?;
    root.write(&format!("etc/systemd/system/{UNIT}"), &unit(prep.n), 0o644)?;
    root.symlink(
        &format!("/etc/systemd/system/{UNIT}"),
        &format!("etc/systemd/system/multi-user.target.wants/{UNIT}"),
    )?;
    root.write(
        "etc/motd.d/osreflog-probe",
        &format!("{}\n", message(prep.n)),
        0o644,
    )?;
    root.write(
        "etc/xdg/autostart/osreflog-probe.desktop",
        &autostart(prep.n),
        0o644,
    )?;
    Ok(home)
}

/// Turns the clone at `clone` into probe `n` booted as a container (`systemd-nspawn --boot`)
/// for a person to look into: an empty fstab (the container has no disks), osreflog and the
/// dnf hook kept out as in a metal probe, the marker, and a login message.
///
/// # Errors
/// [`BootError::Refused`] when a path in the clone is a symlink or a mount point; otherwise
/// if a write fails.
pub fn prepare_container(clone: &Path, n: u32) -> Result<(), BootError> {
    let root = Beneath::open(clone)?;
    root.write("etc/fstab", "", 0o644)?;
    for dir in KEPT_OUT {
        root.write(&format!("{dir}/zz-osreflog-probe.conf"), DROP_IN, 0o644)?;
    }
    root.remove(DNF_HOOK)?;
    root.remove(VERDICT)?;
    root.dir(PROBE_DIR)?;
    root.write(MARKER, &format!("{n}\n"), 0o644)?;
    root.write(
        "etc/motd.d/osreflog-probe",
        &format!(
            "This is osreflog bisect probe {n}: a container on a throwaway copy of your system. \
             Check what you are bisecting, then run: osreflog bisect good, bad or skip (as root).\n"
        ),
        0o644,
    )?;
    Ok(())
}

/// The verdict written in the clone, if any (`good`, `bad` or `skip`).
#[must_use]
pub fn read_verdict(clone: &Path) -> Option<String> {
    let v = Beneath::open(clone).ok()?.read(VERDICT).ok()?;
    let v = v.trim();
    matches!(v, "good" | "bad" | "skip").then(|| v.to_owned())
}

/// Writes a verdict given on the host into the clone, where the probe's own would be.
///
/// # Errors
/// If `verdict` isn't one, or the write is refused.
pub fn write_verdict(clone: &Path, verdict: &str) -> Result<(), BootError> {
    if !matches!(verdict, "good" | "bad" | "skip") {
        return Err(BootError::Invalid(format!("not a verdict: {verdict}")));
    }
    let root = Beneath::open(clone)?;
    root.dir(PROBE_DIR)?;
    root.write(VERDICT, &format!("{verdict}\n"), 0o644)?;
    Ok(())
}

/// What the probe recorded when it booted, read from the clone after the reboot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Booted {
    pub probe: u32,
    /// Seconds since the epoch.
    pub at: u64,
    pub kernel: String,
}

/// The clone's boot record, if probe `n` booted (an absent or foreign record is `None`).
#[must_use]
pub fn booted(clone: &Path, n: u32) -> Option<Booted> {
    let text = Beneath::open(clone)
        .ok()?
        .read(&format!("{PROBE_DIR}/booted"))
        .ok()?;
    let get = |k: &str| {
        text.lines()
            .find_map(|l| l.strip_prefix(k).and_then(|r| r.strip_prefix('=')))
            .map(str::to_owned)
    };
    let b = Booted {
        probe: get("probe")?.parse().ok()?,
        at: get("booted")?.parse().ok()?,
        kernel: get("kernel")?,
    };
    (b.probe == n).then_some(b)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    const FSTAB: &str = "\
#
# /etc/fstab
#
UUID=7b7a / btrfs subvol=root,compress=zstd:1 0 0
UUID=58cd /boot ext4 defaults 1 2
UUID=F034 /boot/efi vfat umask=0077,shortname=winnt 0 2
UUID=7b7a /home btrfs subvol=home,compress=zstd:1 0 0
UUID=78F6 /mnt/external ntfs-3g defaults,nofail 0 0
";

    #[test]
    fn home_kinds() {
        assert_eq!(
            home_of(FSTAB),
            Home::Throwaway {
                from: "home".into()
            }
        );
        assert_eq!(home_of("UUID=a / btrfs subvol=root 0 0\n"), Home::InRoot);
        assert_eq!(
            home_of("UUID=a / btrfs subvol=root 0 0\nUUID=b /home ext4 defaults 0 0\n"),
            Home::Real {
                spec: "UUID=b".into()
            }
        );
        assert_eq!(
            home_of("UUID=a / btrfs subvol=root 0 0\nUUID=a /home btrfs subvol=/@home 0 0\n"),
            Home::Throwaway {
                from: "@home".into()
            }
        );
    }

    #[test]
    fn fstab_points_root_and_home_at_the_probe() {
        let out =
            rewrite_fstab(FSTAB, "osreflog/probes/p4", Some("osreflog/probes/p4-home")).unwrap();
        assert!(
            out.contains("\nUUID=7b7a / btrfs subvol=osreflog/probes/p4,compress=zstd:1 0 0\n")
        );
        assert!(out.contains(
            "\nUUID=7b7a /home btrfs subvol=osreflog/probes/p4-home,compress=zstd:1 0 0\n"
        ));
        assert!(out.contains("\n# was: UUID=7b7a / btrfs subvol=root,compress=zstd:1 0 0\n"));
        assert!(out.contains("\nUUID=58cd /boot ext4 defaults 1 2\n"));
        assert!(out.contains("\nUUID=78F6 /mnt/external ntfs-3g defaults,nofail 0 0\n"));
        // Without a throwaway home, /home stays as it was.
        let out = rewrite_fstab(FSTAB, "p", None).unwrap();
        assert!(out.contains("\nUUID=7b7a /home btrfs subvol=home,compress=zstd:1 0 0\n"));
        assert_eq!(
            with_subvol("defaults,subvolid=256,noatime", "p"),
            "subvol=p,noatime"
        );
        assert!(rewrite_fstab("UUID=a / ext4 defaults 0 0\n", "p", None).is_err());
    }

    fn clone_with(fstab: &str) -> tempfile::TempDir {
        let t = tempfile::tempdir().unwrap();
        for d in [
            "etc/systemd/system/multi-user.target.wants",
            "etc/dnf/libdnf5-plugins/actions.d",
            "var/lib",
        ] {
            std::fs::create_dir_all(t.path().join(d)).unwrap();
        }
        std::fs::write(t.path().join("etc/fstab"), fstab).unwrap();
        std::fs::write(t.path().join(DNF_HOOK), "hook").unwrap();
        t
    }

    fn prep() -> Prep {
        Prep {
            n: 7,
            root_subvol: "osreflog/probes/p7".into(),
            home_subvol: Some("osreflog/probes/p7-home".into()),
        }
    }

    #[test]
    fn prepare_makes_a_throwaway_probe() {
        let t = clone_with(FSTAB);
        let p = t.path();
        std::fs::create_dir_all(p.join(PROBE_DIR)).unwrap();
        std::fs::write(
            p.join(PROBE_DIR).join("booted"),
            "probe=7\nbooted=1\nkernel=old\n",
        )
        .unwrap();
        assert_eq!(
            prepare(p, &prep()).unwrap(),
            Home::Throwaway {
                from: "home".into()
            }
        );

        let fstab = std::fs::read_to_string(p.join("etc/fstab")).unwrap();
        assert!(fstab.contains("/ btrfs subvol=osreflog/probes/p7,"));
        assert!(fstab.contains("/home btrfs subvol=osreflog/probes/p7-home,"));
        for d in KEPT_OUT {
            let drop_in =
                std::fs::read_to_string(p.join(d).join("zz-osreflog-probe.conf")).unwrap();
            assert!(drop_in.contains("ConditionKernelCommandLine=!osreflog.probe"));
        }
        assert!(!p.join(DNF_HOOK).exists());
        assert!(booted(p, 7).is_none(), "an old boot record is cleared");
        let unit_text = std::fs::read_to_string(p.join("etc/systemd/system").join(UNIT)).unwrap();
        assert!(unit_text.contains("ConditionKernelCommandLine=osreflog.probe=7\n"));
        assert!(unit_text.contains("booted=$$(date +%%s)"));
        assert_eq!(
            std::fs::read_link(
                p.join("etc/systemd/system/multi-user.target.wants")
                    .join(UNIT)
            )
            .unwrap(),
            Path::new("/etc/systemd/system").join(UNIT)
        );
        assert!(
            std::fs::read_to_string(p.join("etc/motd.d/osreflog-probe"))
                .unwrap()
                .contains("probe 7")
        );
        let desktop =
            std::fs::read_to_string(p.join("etc/xdg/autostart/osreflog-probe.desktop")).unwrap();
        assert!(desktop.contains("Exec=notify-send"));
        assert_eq!(desktop.matches('"').count(), 4, "{desktop}");
        assert!(p.join(PROBE_DIR).is_dir());
        assert_eq!(std::fs::read_to_string(p.join(MARKER)).unwrap(), "7\n");

        // Prepared twice (a retried probe): the same result.
        let first = std::fs::read_to_string(p.join("etc/fstab")).unwrap();
        assert_eq!(original_fstab(&first), FSTAB);
        assert_eq!(
            prepare(p, &prep()).unwrap(),
            Home::Throwaway {
                from: "home".into()
            }
        );
        assert_eq!(std::fs::read_to_string(p.join("etc/fstab")).unwrap(), first);
        assert_eq!(
            std::fs::read_to_string(p.join("etc/fstab"))
                .unwrap()
                .matches("# was:")
                .count(),
            2
        );
    }

    #[test]
    fn a_symlink_in_the_clone_cannot_lead_a_write_outside() {
        let outside = tempfile::tempdir().unwrap();
        let t = clone_with(FSTAB);
        // /etc/motd.d -> a directory outside the clone.
        std::os::unix::fs::symlink(outside.path(), t.path().join("etc/motd.d")).unwrap();
        let e = prepare(t.path(), &prep()).unwrap_err();
        assert!(matches!(e, BootError::Refused(_)), "{e}");
        assert_eq!(std::fs::read_dir(outside.path()).unwrap().count(), 0);

        // fstab itself a symlink: refused before anything is written.
        let t = clone_with(FSTAB);
        std::fs::remove_file(t.path().join("etc/fstab")).unwrap();
        std::os::unix::fs::symlink("/etc/fstab", t.path().join("etc/fstab")).unwrap();
        assert!(matches!(
            prepare(t.path(), &prep()),
            Err(BootError::Refused(_))
        ));
    }

    #[test]
    fn a_container_probe_has_no_disks_and_takes_verdicts() {
        let t = clone_with(FSTAB);
        let p = t.path();
        std::fs::create_dir_all(p.join(PROBE_DIR)).unwrap();
        std::fs::write(p.join(VERDICT), "bad\n").unwrap(); // from an earlier probe of this state
        prepare_container(p, 5).unwrap();
        assert_eq!(std::fs::read_to_string(p.join("etc/fstab")).unwrap(), "");
        assert_eq!(std::fs::read_to_string(p.join(MARKER)).unwrap(), "5\n");
        assert!(!p.join(DNF_HOOK).exists());
        let drop_in =
            std::fs::read_to_string(p.join(KEPT_OUT[0]).join("zz-osreflog-probe.conf")).unwrap();
        assert!(drop_in.contains("ConditionPathExists=!/var/lib/osreflog-probe/manual"));
        assert_eq!(read_verdict(p), None);
        write_verdict(p, "skip").unwrap();
        assert_eq!(read_verdict(p).as_deref(), Some("skip"));
        assert!(write_verdict(p, "maybe").is_err());
        std::fs::write(p.join(VERDICT), "whatever\n").unwrap();
        assert_eq!(read_verdict(p), None);
    }

    #[test]
    fn boot_record_is_for_this_probe_only() {
        let t = clone_with(FSTAB);
        std::fs::create_dir_all(t.path().join(PROBE_DIR)).unwrap();
        std::fs::write(
            t.path().join(PROBE_DIR).join("booted"),
            "probe=7\nbooted=1792084187\nkernel=7.2.9-200.fc44.x86_64\n",
        )
        .unwrap();
        assert_eq!(
            booted(t.path(), 7),
            Some(Booted {
                probe: 7,
                at: 1_792_084_187,
                kernel: "7.2.9-200.fc44.x86_64".into()
            })
        );
        assert_eq!(booted(t.path(), 8), None);
    }
}
