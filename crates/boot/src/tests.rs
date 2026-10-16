#![allow(clippy::unwrap_used)]

//! A stand-in `/boot` in a temporary directory, written with the real `grub2-editenv`.

use super::*;

const ID: &str = "48239e7648cb40b7a34190254fe5587c-7.2.9-200.fc44.x86_64";
const V: &str = "7.2.9-200.fc44.x86_64";

struct Fake {
    _dir: tempfile::TempDir,
    boot: Boot,
}

fn editenv() -> Option<PathBuf> {
    ["/usr/bin/grub2-editenv", "/usr/bin/grub-editenv"]
        .into_iter()
        .map(PathBuf::from)
        .find(|p| p.is_file())
}

/// A Fedora-like `/boot`: one default entry, its kernel and initramfs, `saved_entry` set.
/// `None` when grub2-editenv isn't installed (the test is then skipped).
fn fake(options: &str) -> Option<Fake> {
    let editenv = editenv()?;
    let dir = tempfile::tempdir().unwrap();
    let mut boot = Boot::new(dir.path());
    boot.editenv = editenv;
    std::fs::create_dir_all(boot.entries_dir()).unwrap();
    std::fs::create_dir_all(dir.path().join("grub2")).unwrap();
    std::fs::write(
        boot.entries_dir().join(format!("{ID}.conf")),
        format!(
            "title Fedora Linux ({V}) 44\nversion {V}\nlinux /vmlinuz-{V}\n\
             initrd /initramfs-{V}.img\noptions {options}\ngrub_users $grub_users\n\
             grub_arg --unrestricted\ngrub_class fedora\n"
        ),
    )
    .unwrap();
    std::fs::write(dir.path().join(format!("vmlinuz-{V}")), "kernel").unwrap();
    std::fs::write(dir.path().join(format!("initramfs-{V}.img")), "initrd").unwrap();
    let env = boot.grubenv_path();
    grubenv::edit(&boot.editenv, &env, &["create"]).unwrap();
    grubenv::edit(
        &boot.editenv,
        &env,
        &["set", &format!("saved_entry={ID}"), "boot_success=0"],
    )
    .unwrap();
    Some(Fake { _dir: dir, boot })
}

fn reuse() -> kernel::Plan {
    kernel::Plan::Reuse { version: V.into() }
}

#[test]
fn a_whole_probe_leaves_the_default_untouched() {
    let Some(f) = fake("root=UUID=7b7a ro rootflags=subvol=root rhgb quiet") else {
        return;
    };
    let b = &f.boot;
    let rec = b.default_record().unwrap();
    assert_eq!(rec.saved_entry, ID);
    assert_eq!(rec.linux, format!("/vmlinuz-{V}"));
    assert_eq!(rec.initrd, vec![format!("/initramfs-{V}.img")]);
    let default_before = std::fs::read(b.entries_dir().join(format!("{ID}.conf"))).unwrap();

    let path = b
        .write_probe_entry(&rec, 3, "osreflog/probes/p3", &reuse())
        .unwrap();
    let probe = bls::Entry::parse(&std::fs::read_to_string(&path).unwrap());
    assert_eq!(
        probe.get("title"),
        Some(format!("osreflog bisect probe 3 ({V})").as_str())
    );
    assert_eq!(probe.get("linux"), Some(format!("/vmlinuz-{V}").as_str()));
    assert_eq!(
        probe.get("options"),
        Some(
            "root=UUID=7b7a ro rootflags=subvol=osreflog/probes/p3 rhgb quiet osreflog.probe=3 panic=10"
        )
    );
    assert_eq!(probe.get("grub_arg"), Some("--unrestricted"));
    assert!(!b.entries_dir().join(".osreflog-probe.conf.tmp").exists());

    b.arm(&rec).unwrap();
    let env = b.env().unwrap();
    assert_eq!(env["next_entry"], PROBE_ID);
    assert_eq!(env["saved_entry"], ID);
    b.verify(&rec).unwrap();

    let left = b.disarm().unwrap();
    assert!(left.entry);
    assert_eq!(left.next_entry.as_deref(), Some(PROBE_ID));
    assert_eq!(b.leftovers().unwrap(), Leftovers::default());
    b.verify(&rec).unwrap();
    assert_eq!(b.default_record().unwrap(), rec);
    assert_eq!(
        std::fs::read(b.entries_dir().join(format!("{ID}.conf"))).unwrap(),
        default_before
    );
    // The block is still a valid 1024-byte block (GRUB rewrites it in place at boot).
    assert_eq!(std::fs::metadata(b.grubenv_path()).unwrap().len(), 1024);
}

#[test]
fn an_installed_kernel_lives_under_boot_osreflog_and_is_cleaned_up() {
    let Some(f) = fake("root=UUID=7b7a ro rootflags=subvol=root") else {
        return;
    };
    let b = &f.boot;
    let rec = b.default_record().unwrap();
    let plan = kernel::Plan::Install {
        version: "7.1.6-201.fc44.x86_64".into(),
        vmlinuz: PathBuf::from("/nonexistent"),
    };
    let (img, initrd) = plan.install_paths(&b.root).unwrap();
    write_synced(&img, b"k").unwrap();
    write_synced(&initrd, b"i").unwrap();
    let path = b
        .write_probe_entry(&rec, 1, "osreflog/probes/p1", &plan)
        .unwrap();
    let probe = bls::Entry::parse(&std::fs::read_to_string(path).unwrap());
    assert_eq!(
        probe.get("linux"),
        Some("/osreflog/vmlinuz-7.1.6-201.fc44.x86_64")
    );
    assert_eq!(
        probe.get("initrd"),
        Some("/osreflog/initramfs-7.1.6-201.fc44.x86_64.img")
    );
    assert_eq!(probe.get("version"), Some("7.1.6-201.fc44.x86_64"));
    assert_eq!(b.leftovers().unwrap().files.len(), 2);
    b.disarm().unwrap();
    assert!(!b.root.join("osreflog").exists());
    b.verify(&rec).unwrap();
}

#[test]
fn kernelopts_are_expanded_from_grubenv() {
    let Some(f) = fake("$kernelopts $tuned_params") else {
        return;
    };
    let b = &f.boot;
    grubenv::edit(
        &b.editenv,
        &b.grubenv_path(),
        &["set", "kernelopts=root=UUID=7b7a ro rootflags=subvol=root"],
    )
    .unwrap();
    let rec = b.default_record().unwrap();
    let path = b
        .write_probe_entry(&rec, 2, "osreflog/probes/p2", &reuse())
        .unwrap();
    let probe = bls::Entry::parse(&std::fs::read_to_string(path).unwrap());
    assert_eq!(
        probe.get("options"),
        Some("root=UUID=7b7a ro rootflags=subvol=osreflog/probes/p2 osreflog.probe=2 panic=10")
    );
}

#[test]
fn refuses_a_default_that_isnt_one_fixed_entry() {
    let Some(f) = fake("ro") else { return };
    let b = &f.boot;
    let env = b.grubenv_path();

    grubenv::edit(&b.editenv, &env, &["set", "prev_saved_entry=old"]).unwrap();
    assert!(matches!(b.default_record(), Err(BootError::Refused(_))));
    grubenv::edit(&b.editenv, &env, &["unset", "prev_saved_entry"]).unwrap();

    grubenv::edit(&b.editenv, &env, &["unset", "saved_entry"]).unwrap();
    assert!(matches!(b.default_record(), Err(BootError::Refused(_))));

    grubenv::edit(
        &b.editenv,
        &env,
        &["set", &format!("saved_entry={PROBE_ID}")],
    )
    .unwrap();
    assert!(matches!(b.default_record(), Err(BootError::Refused(_))));

    grubenv::edit(&b.editenv, &env, &["set", "saved_entry=../../etc/passwd"]).unwrap();
    assert!(matches!(b.default_record(), Err(BootError::Invalid(_))));

    grubenv::edit(&b.editenv, &env, &["set", "saved_entry=missing"]).unwrap();
    assert!(matches!(b.default_record(), Err(BootError::Io { .. })));

    // /boot inside the root filesystem.
    grubenv::edit(&b.editenv, &env, &["set", &format!("saved_entry={ID}")]).unwrap();
    let p = b.entries_dir().join(format!("{ID}.conf"));
    let text = std::fs::read_to_string(&p)
        .unwrap()
        .replace("linux /vmlinuz", "linux /boot/vmlinuz");
    std::fs::write(&p, text).unwrap();
    assert!(matches!(b.default_record(), Err(BootError::Refused(_))));
}

#[test]
fn verify_catches_every_change_to_the_default() {
    let Some(f) = fake("ro rootflags=subvol=root") else {
        return;
    };
    let b = &f.boot;
    let env = b.grubenv_path();
    let rec = b.default_record().unwrap();
    let conf = b.entries_dir().join(format!("{ID}.conf"));
    let original = std::fs::read(&conf).unwrap();

    // The entry's content.
    let mut changed = original.clone();
    changed.extend_from_slice(b"# edited\n");
    std::fs::write(&conf, &changed).unwrap();
    assert!(matches!(b.verify(&rec), Err(BootError::DefaultChanged(_))));
    std::fs::write(&conf, &original).unwrap();
    b.verify(&rec).unwrap();

    // saved_entry pointing elsewhere.
    std::fs::write(b.entries_dir().join("other.conf"), "linux /vmlinuz-x\n").unwrap();
    grubenv::edit(&b.editenv, &env, &["set", "saved_entry=other"]).unwrap();
    assert!(matches!(b.verify(&rec), Err(BootError::DefaultChanged(_))));
    grubenv::edit(&b.editenv, &env, &["set", &format!("saved_entry={ID}")]).unwrap();

    // Its initramfs gone.
    let initrd = b.root.join(format!("initramfs-{V}.img"));
    std::fs::rename(&initrd, b.root.join("moved")).unwrap();
    assert!(matches!(b.verify(&rec), Err(BootError::DefaultChanged(_))));
    std::fs::rename(b.root.join("moved"), &initrd).unwrap();

    // Someone else's one-shot entry: not ours to arm over, and not ours to clear.
    grubenv::edit(&b.editenv, &env, &["set", "next_entry=someone-else"]).unwrap();
    assert!(matches!(b.verify(&rec), Err(BootError::DefaultChanged(_))));
    b.write_probe_entry(&rec, 1, "p", &reuse()).unwrap_err();
    b.disarm().unwrap();
    assert_eq!(b.env().unwrap()["next_entry"], "someone-else");
}

#[test]
fn arm_needs_an_entry() {
    let Some(f) = fake("ro") else { return };
    let rec = f.boot.default_record().unwrap();
    assert!(f.boot.arm(&rec).is_err());
    assert!(!f.boot.env().unwrap().contains_key("next_entry"));
}

#[test]
fn expand_is_grub_like() {
    let env: BTreeMap<String, String> = [
        ("a".to_owned(), "x y".to_owned()),
        ("b_2".to_owned(), "z".to_owned()),
    ]
    .into();
    assert_eq!(expand("$a ${b_2}w $missing end", &env), "x y zw end");
    assert_eq!(expand("cost=$ 5", &env), "cost=$ 5");
    assert_eq!(expand("plain", &env), "plain");
}

/// This laptop's real entries (P7d): tuned adds `$tuned_initrd` to `initrd` and `$tuned_params`
/// to `options`. Neither is a file; the variables expand from grubenv (often to nothing).
#[test]
fn tuned_variables_in_a_fedora_entry() {
    let Some(f) = fake("root=UUID=7b7a ro rootflags=subvol=root rhgb quiet $tuned_params") else {
        return;
    };
    let b = &f.boot;
    let p = b.entries_dir().join(format!("{ID}.conf"));
    let text = std::fs::read_to_string(&p).unwrap().replace(
        &format!("initrd /initramfs-{V}.img"),
        &format!("initrd /initramfs-{V}.img $tuned_initrd"),
    );
    std::fs::write(&p, text).unwrap();
    grubenv::edit(
        &b.editenv,
        &b.grubenv_path(),
        &["set", "tuned_params=", "tuned_initrd="],
    )
    .unwrap();

    let rec = b.default_record().unwrap();
    assert_eq!(rec.initrd, vec![format!("/initramfs-{V}.img")]);
    b.verify(&rec).unwrap();
    let path = b
        .write_probe_entry(&rec, 1, "osreflog/probes/p1", &reuse())
        .unwrap();
    let probe = bls::Entry::parse(&std::fs::read_to_string(path).unwrap());
    assert_eq!(
        probe.get("options"),
        Some(
            "root=UUID=7b7a ro rootflags=subvol=osreflog/probes/p1 rhgb quiet osreflog.probe=1 panic=10"
        )
    );
    b.arm(&rec).unwrap();
    b.verify(&rec).unwrap();
}
