# Installing osreflog

## What it needs

- **Fedora 44 or 45** (or rawhide), x86_64, with `/` on a **btrfs subvolume**: Fedora
  Workstation's default layout since Fedora 33 (`root` mounted at `/`, `home` at `/home`).
  `osreflog doctor` checks this first.
- **dnf5** with its actions plugin (`libdnf5-plugin-actions`, pulled in by the package): the hook
  that takes a checkpoint before and after every package transaction.
- **For bisect:** `systemd-container` for container probes; `qemu-kvm-core`, `virtiofsd` and
  `dracut` for VM probes (the package recommends them); GRUB with BLS entries for probes on real
  hardware.
- **Not Snapper or Timeshift on the same `/`.** Two tools managing snapshots of one subvolume is
  unsupported; `doctor` warns when either is installed.

## From COPR

```bash
sudo dnf copr enable khaledsaeed18/osreflog
sudo dnf install osreflog osreflog-app
```

Fedora's presets leave new services off, so turn them on once:

```bash
sudo systemctl enable --now osreflogd.service osreflog-boot-doctor.service
systemctl --user enable --now osreflog-user.service
```

- `osreflogd` is the daemon: it watches changes, takes checkpoints and serves the CLI and the app
  over D-Bus.
- `osreflog-boot-doctor` runs once at every boot: it checks the default boot entry and cleans up
  after a bisect probe on real hardware.
- `osreflog-user` records your own settings changes (dconf/GSettings) in a ledger of your own; run
  it in each account you want covered.

Then check:

```bash
osreflog doctor
```

```
osreflog doctor
  ok    / is btrfs subvolume /root
  ok    dnf5 actions plugin installed
  ok    checkpoint hook installed (/etc/dnf/libdnf5-plugins/actions.d/osreflog.actions)
  ok    osreflogd 0.2.0 running, 1 checkpoints, 1 snapshots kept
  ok    ledger chain verified (3 entries)
```

The app is **osreflog** in the app grid. Updates arrive with `dnf upgrade`; the daemon restarts
at the end of the upgrade's transaction, not halfway through it.

## The first hour

- `osreflog log` lists checkpoints: one at every boot, one a day, and two around every dnf
  transaction. `sudo osreflog checkpoint -m "before trying X"` takes one by hand.
- `osreflog causes` lists what changed the system, cause by cause, newest first.
- `osreflog why /etc/some/file` shows every recorded change to a file.
- Reading the history needs no password for the person at the machine; taking a checkpoint,
  undoing and bisecting ask through polkit.

## Release RPMs

Each [GitHub release](https://github.com/KhaledSaeed18/osreflog/releases) carries the RPMs built
on Fedora 44, the SRPM, the source tarball and a `SHA256SUMS` file:

```bash
sha256sum -c SHA256SUMS --ignore-missing
sudo dnf install ./osreflog-*.x86_64.rpm ./osreflog-app-*.x86_64.rpm
```

COPR is the better way for most people: it updates by itself.

## From source

What the build needs, on Fedora:

```bash
sudo dnf install rust cargo gcc clang libbpf-devel kernel-headers gtk4-devel libadwaita-devel
```

(or Rust from rustup, 1.88 or newer). Then:

```bash
cargo build --release
sudo scripts/install-dev.sh
osreflog doctor
```

`install-dev.sh` installs the daemon, CLI, user agent, units, D-Bus and polkit policies and the
dnf hook under `/usr/local` and `/etc`, which take precedence over a packaged install: run
`sudo scripts/install-dev.sh --uninstall` before going back to the packages. The app runs from
the tree: `cargo run --release -p osreflog-app`.

## Removing it

```bash
sudo systemctl disable --now osreflogd.service osreflog-boot-doctor.service
systemctl --user disable --now osreflog-user.service
sudo dnf remove osreflog osreflog-app
```

(or `sudo scripts/install-dev.sh --uninstall` for a source install.)

**The history stays.** Snapshots and the ledger live at the btrfs top level, next to `root` and
`home`, in `osreflog/`: removing the package never deletes them, and installing again picks
them up. To delete them for good:

```bash
sudo mount -o subvolid=5 /dev/<your btrfs device> /mnt
sudo btrfs subvolume delete /mnt/osreflog/snapshots/*
sudo btrfs subvolume delete /mnt/osreflog/probes/*      # if there are any
sudo btrfs subvolume delete /mnt/osreflog/ledger
sudo rm -r /mnt/osreflog
sudo umount /mnt
```

Your own settings history is in `~/.local/state/osreflog/`.
