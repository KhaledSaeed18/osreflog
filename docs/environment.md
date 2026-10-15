# Environment

Facts about the development host, collected 2026-10-14, updated after P1 and at P7 pre-flight (2026-10-15). Re-check them at the start of each phase.

| Item | Value | Why it matters |
| ---- | ----- | -------------- |
| OS | Fedora 44 Workstation, GNOME Shell 50.5 | target platform |
| Kernel | 7.2.9-200.fc44 (also 7.2.8, 7.1.6 installed) | fanotify features, BPF; several kernels give the VM tier real version changes to bisect |
| CPU | i7-8550U, 4c/8t, VT-x | KVM available for the vm tier |
| RAM | 7.1 GiB + 7.1 GiB zram; ~1.5 GiB available with the desktop running, ~3.8 GiB already in zram | vm probes run at 512 MiB (615 MiB qemu RSS in P1), one at a time; daemon RSS budget < 60 MB |
| Disk | `/dev/sda3` btrfs, 236 GiB, 50 GiB used | plenty of room for snapshots; still bound and report it |
| Layout | one btrfs fs: subvol `root` → `/`, `home` → `/home`, both `compress=zstd:1`; no separate `/var` subvolume | snapshots of `root` include `/var/log`, `/var/lib/docker`, `/var/lib/libvirt`. That's one reason undo is per-change, not a whole-subvolume rollback |
| `/boot` | separate ext4 (2.0 GiB, 1.3 GiB free at P7 pre-flight); `/boot/efi` vfat; GRUB2 2.12 with BLS entries, shim 16.1 | `/boot` is outside snapshots: track its config as blobs; metal probes need a kernel in `/boot` |
| Secure Boot | **enabled**; kernel lockdown `integrity` (P7 pre-flight) | metal probes can only boot Fedora-signed kernels; the boot gate's test VM runs OVMF with Secure Boot |
| Kernel images | `/usr/lib/modules/<ver>/vmlinuz` present (inside the root subvolume) | the vm tier can boot each snapshot's own kernel |
| Kernel config | `CONFIG_FANOTIFY=y`, `CONFIG_FANOTIFY_ACCESS_PERMISSIONS=y`, `CONFIG_BPF_LSM=y`, `CONFIG_DEBUG_INFO_BTF=y`, `CONFIG_SECURITY_PATH=y`, `CONFIG_VIRTIO_FS=m`, `CONFIG_9P_FS=m` | fanotify + CO-RE eBPF + BPF LSM all available; virtiofs is a module, so the vm tier needs an initramfs |
| Active LSMs | `lockdown,capability,yama,selinux,bpf,landlock,ipe,ima,evm` | BPF LSM usable; Landlock available for the parser sandbox; SELinux enforcing |
| fanotify | `max_user_marks` 67,456 (unprivileged) | enough for `~/.config` in the user agent; root uses `FAN_UNLIMITED_MARKS` |
| Dir counts | `/etc` 478 dirs, `/usr` 17,965 dirs | no longer matters for marks: P1 chose one filesystem-wide mark |
| fanotify load | 9.1 events/s system-wide, peak 177/s, over 20 min of normal use (P1) | one filesystem mark + userspace filtering is cheap |
| dnf | dnf5 5.4.6; history in `/usr/lib/sysimage/libdnf5/transaction_history.sqlite`; `libdnf5-plugin-actions` 5.4.6 installed in P1 | the pre-transaction checkpoint hook; `dnf` and `packagekitd` both block on it |
| GNOME Software | PackageKit 1.4.0 for installs; offline updates prepared by **dnf5daemon-server** | the offline path is the remaining Q4 case |
| RPM | rpm 6.0.2; db `/usr/lib/sysimage/rpm/rpmdb.sqlite` (`/var/lib/rpm` is a symlink) | the package diff reads it from each snapshot |
| Snapshot tools | `btrfs-progs` 7.1 installed; `snapper` 0.13 and `btrfs-assistant` 2.2 available, not installed | don't install Snapper on the dev machine during development, to avoid two tools managing snapshots |
| systemd | 259.9 | nspawn tier, mount namespaces, hardening options |
| Virtualization | `qemu-kvm` 10.2.2, `virtiofsd` 1.14.0, libvirt installed (`virtqemud.socket` enabled) | vm tier, using QEMU directly, not libvirt |
| Toolchains | rustup present but **no default toolchain** (projects pin via `rust-toolchain.toml`); Go 1.27.2; `bpftool` 7.6; `fatrace` 0.19.1, `bpftrace` 0.24.2, `hyperfine` 1.20 (P1) | osreflog pins `stable` + rustfmt/clippy like drainscope |
| Network | ~60–125 KiB/s in P1; one Fedora mirror stalled under 1 KB/s | install tooling with `--setopt=install_weak_deps=False`; avoid large downloads in scripts |
| Quotas | btrfs quotas **not enabled** | space accounting can't rely on qgroups (Q3) |
| User | in `wheel`, `docker` | polkit admin auth works via wheel |
| osreflog (dev install, P2) | `osreflogd` running from `/usr/local/libexec/osreflog/`, CLI in `/usr/local/bin`, dnf hook active since 2026-10-15; snapshots and ledger under `osreflog/` at the btrfs top level | every dnf/PackageKit transaction on this machine is now bracketed by checkpoints; `sudo scripts/install-dev.sh --uninstall` removes it and keeps the history |

## Expected friction

- **Memory.** A vm probe beside a GNOME session pushes into zram. The memory guard in the vm runner is not optional. P1: a 512 MiB guest needs a host-only initramfs (37 MiB); the generic one (254 MiB) panics while unpacking.
- **SELinux.** P1 produced **no AVC denials**, but everything ran as unconfined root. Expect denials once osreflogd runs in its own domain (mounting subvolid=5, BLS entries, virtiofsd on arbitrary subvolumes, fanotify). Collect them (`ausearch -m avc -ts recent`) for the P8 policy.
- **Docker on btrfs** creates many nested subvolumes under `/var/lib/docker`. They're excluded from root snapshots automatically (nested subvolumes aren't included), which is good, but they show up in subvolume listings. Filter them out.
- **drainscope and urljail run on the same machine.** Note their background load when measuring the overhead criteria.
