//! Which kernel a metal probe boots, and where it comes from.
//!
//! The probe's newest kernel (`/usr/lib/modules/<ver>/vmlinuz`, inside the snapshot on
//! Fedora). When `/boot` still has the same image (Fedora keeps three kernels), the probe
//! reuses it and its initramfs, which is what that state really booted with. Otherwise the
//! image is copied to `/boot/osreflog/` and the caller builds an initramfs next to it from
//! the probe. Both are Fedora-signed images: shim refuses anything else under Secure Boot,
//! and nothing here signs.

use std::path::{Path, PathBuf};

use crate::BootError;

/// Where osreflog's own kernel files go, relative to `/boot`.
pub const OWN_DIR: &str = "osreflog";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Plan {
    /// `/boot/vmlinuz-<ver>` is the probe's image, and `/boot/initramfs-<ver>.img` exists.
    Reuse { version: String },
    /// Copy `vmlinuz` into `/boot/osreflog/` and build an initramfs there.
    Install { version: String, vmlinuz: PathBuf },
}

impl Plan {
    #[must_use]
    pub fn version(&self) -> &str {
        match self {
            Plan::Reuse { version } | Plan::Install { version, .. } => version,
        }
    }

    /// The `linux` and `initrd` paths for the BLS entry (relative to the `/boot` partition).
    #[must_use]
    pub fn bls_paths(&self) -> (String, String) {
        match self {
            Plan::Reuse { version } => (
                format!("/vmlinuz-{version}"),
                format!("/initramfs-{version}.img"),
            ),
            Plan::Install { version, .. } => (
                format!("/{OWN_DIR}/vmlinuz-{version}"),
                format!("/{OWN_DIR}/initramfs-{version}.img"),
            ),
        }
    }

    /// For [`Plan::Install`], the files to create under `boot`: the image and the initramfs.
    #[must_use]
    pub fn install_paths(&self, boot: &Path) -> Option<(PathBuf, PathBuf)> {
        match self {
            Plan::Reuse { .. } => None,
            Plan::Install { version, .. } => {
                let dir = boot.join(OWN_DIR);
                Some((
                    dir.join(format!("vmlinuz-{version}")),
                    dir.join(format!("initramfs-{version}.img")),
                ))
            }
        }
    }
}

/// The probe's kernel versions, newest last (rpm version order).
#[must_use]
pub fn versions(probe: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(probe.join("usr/lib/modules"))
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().join("vmlinuz").is_file())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    v.sort_by(|a, b| osreflog_rpm::vercmp::rpmvercmp(a, b));
    v
}

fn hash(path: &Path) -> Result<blake3::Hash, BootError> {
    let mut h = blake3::Hasher::new();
    h.update_reader(std::fs::File::open(path).map_err(|e| BootError::io(path, e))?)
        .map_err(|e| BootError::io(path, e))?;
    Ok(h.finalize())
}

/// Plans the kernel for a probe rooted at `probe`, booted from `boot`.
///
/// # Errors
/// If the probe has no kernel, or an image can't be read.
pub fn plan(boot: &Path, probe: &Path) -> Result<Plan, BootError> {
    let version = versions(probe)
        .pop()
        .ok_or_else(|| BootError::Invalid("the probe has no kernel in /usr/lib/modules".into()))?;
    let vmlinuz = probe.join("usr/lib/modules").join(&version).join("vmlinuz");
    let installed = boot.join(format!("vmlinuz-{version}"));
    if installed.is_file()
        && boot.join(format!("initramfs-{version}.img")).is_file()
        && hash(&installed)? == hash(&vmlinuz)?
    {
        return Ok(Plan::Reuse { version });
    }
    Ok(Plan::Install { version, vmlinuz })
}

/// For [`Plan::Install`]: copies the image into `/boot/osreflog/` and builds a host-only
/// initramfs next to it from the probe's own modules and `/etc` (`dracut --sysroot`), so the
/// probe boots with its own module options. Both go through temporary names and are synced;
/// a [`Plan::Reuse`] needs nothing.
///
/// # Errors
/// If a copy or dracut fails; the partial files are removed.
pub fn install(plan: &Plan, boot: &Path, probe: &Path, dracut: &Path) -> Result<(), BootError> {
    let (Plan::Install { version, vmlinuz }, Some((image, initrd))) =
        (plan, plan.install_paths(boot))
    else {
        return Ok(());
    };
    let data = std::fs::read(vmlinuz).map_err(|e| BootError::io(vmlinuz, e))?;
    crate::write_synced(&image, &data)?;
    let tmp = initrd.with_extension("img.tmp");
    let out = std::process::Command::new(dracut)
        .args([
            "-q",
            "-f",
            "--hostonly",
            "--no-hostonly-cmdline",
            "--kver",
            version,
        ])
        .arg("--sysroot")
        .arg(probe)
        .arg(&tmp)
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|e| BootError::io(dracut, e))?;
    if !out.status.success() || !tmp.is_file() {
        let _ = std::fs::remove_file(&tmp);
        let _ = std::fs::remove_file(&image);
        let err = String::from_utf8_lossy(&out.stderr);
        let last: Vec<&str> = err.lines().rev().take(3).collect();
        return Err(BootError::Tool(format!(
            "dracut for {version}: {}",
            last.into_iter().rev().collect::<Vec<_>>().join(" / ")
        )));
    }
    std::fs::File::open(&tmp)
        .and_then(|f| f.sync_all())
        .map_err(|e| BootError::io(&tmp, e))?;
    std::fs::rename(&tmp, &initrd).map_err(|e| BootError::io(&initrd, e))?;
    std::fs::File::open(boot.join(OWN_DIR))
        .and_then(|d| d.sync_all())
        .map_err(|e| BootError::io(boot, e))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    fn kernel(probe: &Path, v: &str, image: &str) {
        let d = probe.join("usr/lib/modules").join(v);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("vmlinuz"), image).unwrap();
    }

    #[test]
    fn newest_is_by_rpm_order() {
        let t = tempfile::tempdir().unwrap();
        kernel(t.path(), "7.2.9-200.fc44.x86_64", "a");
        kernel(t.path(), "7.10.1-100.fc44.x86_64", "b");
        kernel(t.path(), "7.2.10-200.fc44.x86_64", "c");
        std::fs::create_dir_all(t.path().join("usr/lib/modules/7.99-extra")).unwrap(); // no image
        assert_eq!(versions(t.path()).last().unwrap(), "7.10.1-100.fc44.x86_64");
    }

    #[test]
    fn reuses_only_the_same_image_with_an_initramfs() {
        let boot = tempfile::tempdir().unwrap();
        let probe = tempfile::tempdir().unwrap();
        let v = "7.2.9-200.fc44.x86_64";
        kernel(probe.path(), v, "image");

        // Not in /boot at all.
        let p = plan(boot.path(), probe.path()).unwrap();
        assert!(matches!(p, Plan::Install { .. }));
        assert_eq!(
            p.bls_paths(),
            (
                format!("/osreflog/vmlinuz-{v}"),
                format!("/osreflog/initramfs-{v}.img")
            )
        );
        assert_eq!(
            p.install_paths(boot.path()).unwrap().1,
            boot.path().join(format!("osreflog/initramfs-{v}.img"))
        );

        // Same version, different image (a rebuilt kernel): not reused.
        std::fs::write(boot.path().join(format!("vmlinuz-{v}")), "other").unwrap();
        std::fs::write(boot.path().join(format!("initramfs-{v}.img")), "i").unwrap();
        assert!(matches!(
            plan(boot.path(), probe.path()).unwrap(),
            Plan::Install { .. }
        ));

        // Same image and an initramfs: reused.
        std::fs::write(boot.path().join(format!("vmlinuz-{v}")), "image").unwrap();
        let p = plan(boot.path(), probe.path()).unwrap();
        assert_eq!(p, Plan::Reuse { version: v.into() });
        assert_eq!(p.bls_paths().0, format!("/vmlinuz-{v}"));

        // Same image, initramfs gone: install.
        std::fs::remove_file(boot.path().join(format!("initramfs-{v}.img"))).unwrap();
        assert!(matches!(
            plan(boot.path(), probe.path()).unwrap(),
            Plan::Install { .. }
        ));
    }

    /// A stand-in dracut: writes its last argument, or fails when told to.
    fn fake_dracut(dir: &Path, fail: bool) -> PathBuf {
        use std::os::unix::fs::PermissionsExt as _;
        let p = dir.join(if fail { "dracut-fail" } else { "dracut" });
        let body = if fail {
            "#!/bin/sh\necho 'dracut: no modules' >&2\nexit 1\n"
        } else {
            "#!/bin/sh\nfor a; do last=$a; done\necho \"$*\" > \"$last\"\n"
        };
        std::fs::write(&p, body).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        p
    }

    #[test]
    fn install_copies_the_image_and_builds_from_the_probe() {
        let boot = tempfile::tempdir().unwrap();
        let probe = tempfile::tempdir().unwrap();
        let bin = tempfile::tempdir().unwrap();
        let v = "7.1.6-201.fc44.x86_64";
        kernel(probe.path(), v, "signed image");
        let p = plan(boot.path(), probe.path()).unwrap();

        install(
            &p,
            boot.path(),
            probe.path(),
            &fake_dracut(bin.path(), true),
        )
        .unwrap_err();
        assert!(
            std::fs::read_dir(boot.path().join(OWN_DIR))
                .unwrap()
                .next()
                .is_none()
        );

        install(
            &p,
            boot.path(),
            probe.path(),
            &fake_dracut(bin.path(), false),
        )
        .unwrap();
        let (image, initrd) = p.install_paths(boot.path()).unwrap();
        assert_eq!(std::fs::read_to_string(image).unwrap(), "signed image");
        let args = std::fs::read_to_string(initrd).unwrap();
        assert!(args.contains(&format!("--kver {v} --sysroot {}", probe.path().display())));
        assert_eq!(
            std::fs::read_dir(boot.path().join(OWN_DIR))
                .unwrap()
                .count(),
            2
        );

        // Reuse: nothing to do, dracut not even run.
        let reuse = Plan::Reuse { version: v.into() };
        install(&reuse, boot.path(), probe.path(), Path::new("/nonexistent")).unwrap();
    }

    #[test]
    fn no_kernel_is_an_error() {
        let boot = tempfile::tempdir().unwrap();
        let probe = tempfile::tempdir().unwrap();
        assert!(plan(boot.path(), probe.path()).is_err());
    }
}
