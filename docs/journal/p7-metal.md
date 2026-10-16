# P7: The boot gate and metal bisect

- Status: **boot gate passed 2026-10-16**; P7 proper (P7a–P7d) next
- Started: 2026-10-15
- Roadmap: [BOOT GATE and P7](../roadmap.md#boot-gate-passed-2026-10-16)
- Exit: the boot gate's checklist passes in a test VM; then one real hardware-specific regression (seeded, e.g. a Wi-Fi powersave config) bisected on this laptop end to end.

## Pre-flight (2026-10-15)

Checked before writing any code, as an ordinary user (no sudo in the session).

### What's in place

| Item | Found | For |
| ---- | ----- | --- |
| Tree | `main` clean and in sync with `origin`; `cargo test --workspace` passes | starting point |
| osreflogd | active since 20:30, dev install in `/usr/local` | probes are built by the running daemon |
| Boot loader | GRUB 2.12 (`grub2-efi-x64` 2.12-66), BLS entries, `shim-x64` 16.1 | `grub2-reboot` one-shot entries |
| Boot tools | `grub2-reboot`, `grub2-editenv`, `grub2-set-default`, `grubby`, `kernel-install`, `dracut` all installed | writing and arming the probe entry |
| `/boot` | ext4 on `/dev/sda2`, 2.0 GiB, **1.3 GiB free**; kernels 7.1.6, 7.2.8, 7.2.9 + rescue, initramfs 46–50 MiB each | room for a probe initramfs or two under `/boot/osreflog/`; `grubenv` on ext4, so GRUB can clear `next_entry` itself |
| Current cmdline | `root=UUID=7b7a13e0… ro rootflags=subvol=root rhgb quiet` | a probe entry only changes `rootflags=subvol=` and adds `osreflog.probe=N` |
| Virtualization | `/dev/kvm`, `kvm_intel nested=Y`, `qemu-system-x86_64` 10.2.2, `virtiofsd` 1.14 in `/usr/libexec`, `edk2-ovmf` with `OVMF_CODE.secboot.fd` + `OVMF_VARS.secboot.fd` | the gate's test VM, booted with UEFI and Secure Boot like the laptop |
| Disk | 175 GiB free on the btrfs | a test-VM disk image |
| Fedora boot hooks | `grub-boot-success.timer` (user), `grub-boot-indeterminate.service`; no greenboot | GRUB's own boot-success flag; see "Fallback" below |

### What changes the plan

1. **Secure Boot is on** (`mokutil --sb-state`: enabled) and the kernel is in lockdown **integrity** mode. shim only lets GRUB start kernels signed by Fedora. Probe kernels are Fedora's own `vmlinuz` (from `/boot`, or copied out of the snapshot's `/usr/lib/modules/<ver>/`), so they are signed and boot; an initramfs is not checked by shim/GRUB, so a generated one is fine. A locally built kernel could never be a metal probe. The test VM must run OVMF with Secure Boot on, or it doesn't test the same path.
2. **A probe that boots the full system would start osreflogd inside it.** The snapshot has `osreflogd.service` enabled and the dnf actions hook installed. Inside a probe, the daemon would capture, checkpoint and write to the same ledger as the real system. P6 probes never boot that far (`osreflog-probe.target`); a metal probe boots `graphical.target`. The probe must mask `osreflogd.service` and remove the dnf hook in the clone, and a small probe-side unit takes over (record "booted", show the verdict prompt).
3. **`/etc/fstab` can't just be emptied** as P6 does. A metal probe needs `/boot`, `/boot/efi` and `/home`, and the ledger at `/var/lib/osreflog` (bisect.md, step 5). The `/` line says `subvol=root`. The probe's fstab is rewritten instead: `/` takes the probe's subvolume (or drops the `subvol=` option), the ledger subvolume is added, the rest kept (`/mnt/external` already has `nofail`). Whether `systemd-remount-fs` accepts the rewritten `/` line is tested in the VM.
4. **`/home` in a probe.** Mounting the real `/home` means a probe's GNOME session writes to the real `~/.config` and dconf, which would leak probe state into the real system. Proposed: each probe mounts a writable snapshot of `home`, deleted with the probe. Decision needed (below).
5. **Probes can't live in `osreflog/tmp/`.** P6 clears `tmp/` when the daemon starts, and a metal probe survives two reboots. Metal probes go in `osreflog/probes/pN`, as bisect.md already says, and `doctor --boot` owns their cleanup.
6. **Root access.** The session has no sudo, so every root step (installing the daemon, writing `/boot`, `grub2-reboot`, running the gate) goes through scripts the owner runs, as in P5 and P6: `sudo scripts/p7/…`.
7. **Memory.** 2.2 GiB available with 4.5 GiB in swap at the time of the check. The test VM gets 1–1.5 GiB and boots to `multi-user.target`; the GNOME parts of the probe flow (the verdict prompt) are tested on metal.

### Fallback after a hung or panicked probe

`grub2-reboot` sets `next_entry` in `/boot/grub2/grubenv`, and GRUB clears it as it boots that entry. So a probe that hangs falls back to the default entry on the next power cycle without any help from the probe. To be verified in the test VM (gate item 3), with Secure Boot on. Two cheap additions: `panic=10` on the probe's cmdline (a kernel panic reboots by itself, into the default), and a systemd `JobTimeoutSec=` on the probe's boot target with `JobTimeoutAction=reboot-force`.

## Plan

| Slice | What |
| ----- | ---- |
| **G1** test VM | `scripts/p7/testvm.sh`: a raw disk image laid out like the laptop (GPT: ESP vfat, `/boot` ext4, btrfs with `root` and `home`), populated without downloads: `root` received from a recent read-only checkpoint, `/boot` with one Fedora-signed kernel and a host-only initramfs, the ESP with Fedora's shim and GRUB. OVMF with Secure Boot, 1–1.5 GiB, serial console, `multi-user.target`. Booted and checked before osreflog touches it |
| **G2** boot entries | osreflogd (new module): probe kernel lookup (reuse `/boot/vmlinuz-<ver>`, otherwise copy it and build an initramfs into `/boot/osreflog/`), the BLS entry `osreflog-probe.conf`, `grub2-reboot`, and a record of the default entry (`saved_entry` + checksum of its entry file) checked before and after every probe |
| **G3** probe prep | metal probes in `osreflog/probes/pN`; fstab rewritten (item 3); `osreflogd` masked and the dnf hook removed in the clone; the probe-side unit; `/home` per the decision below |
| **G4** doctor | `osreflog doctor --boot`: removes leftover probe entries, initramfs and clones when no bisect is waiting for them, and checks the default entry; run at every boot by a oneshot unit |
| **G5** gate run | `scripts/p7/gate.sh` inside the test VM: full metal flow, default untouched after every probe, a hung probe (stopped at the GRUB menu, then a hard reset) and a panicked probe both come back to the default, `doctor --boot` cleans up; recovery note in the docs |
| **P7a** verdicts | `bisect start / good / bad / skip / log` (manual verdicts, which P6 left for this phase), the session kept across reboots in the ledger |
| **P7b** metal tier | `--tier metal`: probe → arm → reboot (asked, never automatic without `--yes`) → verdict in the probe → reboot → continue |
| **P7c** auto | `--tier auto` with `--needs kernel\|hardware` |
| **P7d** exit | seeded Wi-Fi powersave regression bisected on the laptop |

## Decisions (2026-10-15)

The owner took both proposals:
- **Test VM disk:** built from a checkpoint of this machine, kept in its own subvolume `osreflog/testvm` at the top level and deleted at the end of the gate. Wi-Fi/VPN profiles and SSH host keys are left out of the copy; the rest of `/etc` goes in.
- **`/home` in a metal probe:** a throwaway writable snapshot of `home`, deleted with the probe.

## Log

### 2026-10-15: G1, the test VM

`scripts/p7/testvm.sh build | check | run | log | delete`. The image is a sparse raw GPT disk in a no-CoW subvolume: an ESP with this machine's shim and GRUB, an ext4 `/boot` with one Fedora-signed kernel, a host-only initramfs plus `virtio_blk virtio_pci`, the host's GRUB config with the image's UUIDs and one BLS entry, and a btrfs with `root` (rsync `-aHAXS` from the checkpoint, so SELinux labels, ACLs and file capabilities survive) and an empty `home`. Booted by QEMU with `OVMF_CODE.secboot.fd`, SMM, the Secure Boot variable store, `-nic none`, 1536 MiB.

| | |
| - | - |
| Source | checkpoint c000105, kernel 7.2.9-200.fc44 |
| Copy | 7.4 GiB in 368 s (Docker, Flatpak, libvirt images, caches, journals left out); image 7.6 GiB of a 40 GiB sparse file |
| Boot | shim → GRUB menu (5 s, visible on the serial console) → kernel; `Startup finished in 1.3 s (kernel) + 2.6 s (initrd) + 9.5 s (userspace) = 13.4 s`; report 24 s after QEMU started |
| In the VM | Secure Boot enabled, lockdown `integrity`, SELinux enforcing (no relabel needed), `/` = `vda3[/root]`, `/boot` ext4, `/boot/efi` vfat, `/home` = `vda3[/home]`, state `running` with **0 failed units**, osreflogd active |
| grubenv | `saved_entry=<machine-id>-7.2.9…`; Fedora's `grub-boot-indeterminate.service` wrote `boot_indeterminate=0`, so the guest writes `/boot/grub2/grubenv` as the laptop does |
| Memory | qemu peak RSS 977–1170 MiB for a 1536 MiB guest |

The first `check` lost its report: the console prefixes each line (`[ 13.37] osreflog-testvm-check[1097]: TESTVM …`), the script matched at `^`, `pipefail` ended it silently, and qemu was left running. The check now matches anywhere, kills qemu on any exit and prints the serial log's tail on a failure. GRUB also logged `no such device: F034-AABF`: the host's grub.cfg names the ESP's UUID as well; the next build replaces it too.

The menu staying visible matters for G5: a "hung probe" is simulated by stopping there and resetting.

### 2026-10-15: G2, the probe's boot entry

`crates/boot` (17 tests, a stand-in `/boot` written with the real `grub2-editenv`):
- **Default record**: `saved_entry`, BLAKE3 of its entry file, its kernel and initramfs paths. Checked before writing the probe entry, before and after arming, and after every boot. Refused up front: no `saved_entry` (the default would be "the first entry", which a probe entry could become), a `prev_saved_entry`, `saved_entry` naming the probe, an id with `/`, and a `/boot` inside the root filesystem.
- **Probe entry** `osreflog-probe.conf`: the default's entry with the probe's title, kernel and initramfs, `rootflags=subvol=osreflog/probes/pN` (other mount options kept, `subvolid=` dropped), `osreflog.probe=N` and `panic=10`; `$kernelopts`-style variables expanded from grubenv. Written as `.osreflog-probe.conf.tmp` (which `blscfg` ignores), synced, renamed.
- **Arming** is `grub2-editenv … set next_entry=osreflog-probe`, read back. Not `grub2-reboot`: it also copies a leftover `prev_saved_entry` into `saved_entry`, which changes the default, and runs `grub2-probe` on the disk. Disarming clears `next_entry` only when it names the probe.
- **Kernel**: the probe's newest kernel; `/boot`'s copy reused when the image is identical and an initramfs exists, otherwise copied to `/boot/osreflog/` with a host-only initramfs built by `dracut --sysroot <probe>`, so the probe boots with its own module options.

`scripts/p7/testvm.sh flow` installs a step script that runs after the report at every boot (state in `/home`, which default and probe share). `scripts/p7/g2-step.sh`, three boots:

| Boot | What it showed |
| ---- | -------------- |
| 1, default | default recorded; `/` snapshotted as `osreflog/probes/p1`; kernel copied and initramfs built from the probe in **22 s** (41 MiB + 18 MiB in `/boot/osreflog`); entry written; armed: `next_entry=osreflog-probe`, `saved_entry` unchanged, default verified |
| 2, probe | GRUB booted `/osreflog/vmlinuz-…` **under Secure Boot** (a copied Fedora-signed image passes shim); `/` = `vda3[/osreflog/probes/p1]`, the marker written into the probe is there; GRUB had already cleared `next_entry` (left as `next_entry=`, empty); default verified from inside the probe; boot 14.3 s |
| 3, default | the default again without anything being done; default verified, disarmed, probe deleted, nothing left on `/boot`, default verified once more |

Findings:
- **The probe's fstab says `subvol=root` and nothing failed.** `systemd-remount-fs` remounted `/` with the options of the subvolume actually mounted (`subvol=/osreflog/probes/p1`). The fstab rewrite in G3 is still needed for the ledger and for honesty, but a stale `/` line doesn't break the boot.
- **osreflogd failed to start in the probe** (the only failed unit), as predicted: the probe has the daemon enabled. G3 masks it in the clone.
- The host side reported FAIL although the VM printed DONE: the report was kept in `$LINES`, which bash (checkwinsize) resets to the terminal's height after an external command. Renamed; the VM-side results above are from the serial log.

### 2026-10-15: G3, the probe's own root

`osreflog_boot::probe::prepare` turns a clone into probe N (22 tests in the crate):
- **fstab**: `/` on `osreflog/probes/pN`, `/home` on `osreflog/probes/pN-home`, a writable snapshot of the home subvolume, when `/home` is a subvolume of the same btrfs (otherwise reported: `InRoot`, or `Real` for another filesystem, which a probe would mount for real). Each changed line keeps the original as a `# was:` comment, so preparing twice, or preparing a clone of a probe, starts from the system's own lines.
- **Kept out**: osreflogd and the user agent get a drop-in with `ConditionKernelCommandLine=!osreflog.probe`, and the shipped units now carry the same condition (a hand-booted clone can't record into the real history either). The dnf actions hook is removed from the clone.
- **`osreflog-metal-probe.service`** (`ConditionKernelCommandLine=osreflog.probe=N`) writes `/var/lib/osreflog-probe/booted`; the daemon reads it from the clone after the reboot, which tells "the probe booted" from "it fell back to the default". A record from an earlier probe of the same state is removed first.
- **What the user sees**: `/etc/motd.d/osreflog-probe` and an autostart notification: this boot is probe N, a throwaway copy (its /home too); verdict with `sudo osreflog bisect good|bad|skip`; reboot to return.
- **Writes beneath the clone only**: `openat2` with `RESOLVE_BENEATH | NO_SYMLINKS | NO_MAGICLINKS | NO_XDEV`, one component at a time, files replaced through a temporary name. A test points `/etc/motd.d` in the clone at a directory outside it: refused, nothing written there. (P6's `prepare` for nspawn/VM probes writes with plain paths; worth the same treatment.)

**Design change: probes don't mount the ledger.** bisect.md had every probe mount the ledger subvolume at `/var/lib/osreflog` for the verdict. A probe is a state under suspicion; giving it the real history read-write isn't needed. The verdict comes back as the boot record does: a file in the clone, read once the default has booted again.

`scripts/p7/g3-step.sh` (state on `/boot`, the one thing probe and default share besides the ESP):

| Boot | What it showed |
| ---- | -------------- |
| 1, default | `/` and `home` snapshotted as p2 and p2-home; fstab rewritten as above; `/boot`'s kernel reused (identical image, 10 ms); armed |
| 2, probe | `/` = `p2`, `/home` = `p2-home`; osreflogd `inactive`, condition `no` (skipped, not failed); dnf hook gone; boot record written; labels right without any relabel (`systemd_unit_file_t`, `etc_t`, `var_lib_t`: new files take their directory's type); **0 failed units**, state `running`; a file written to `/home` |
| 3, default | the file is not in the real `/home` (it is in p2-home); boot record read from the clone (`probe=2`, kernel 7.2.9); default verified; disarmed; both subvolumes deleted |

The host side now reports `ok   g3-step.sh` (the `$LINES` fix from G2 confirmed).

### 2026-10-16: G4, the boot doctor

`osreflogd --boot-doctor [--dry-run]` (`bins/osreflogd/src/bootdoctor.rs`, 7 tests), run by `osreflog-boot-doctor.service` at every boot of the default system (`ConditionKernelCommandLine=!osreflog.probe`, `Before=osreflogd.service`, `PrivateMounts=yes`, only `/boot` writable) and by `sudo osreflog doctor --boot [--dry-run]`:
1. **`/boot` is always cleaned**: the probe entry, `/boot/osreflog/`, and a `next_entry` naming the probe. On the default system a probe is either done or didn't boot; either way nothing should boot it again unasked. Someone else's `next_entry` is reported and left.
2. **The default entry** is checked against the record taken when the probe was armed, or, with nothing pending, checked to be one fixed entry. A difference is a problem (exit 1, in the journal).
3. **Clones** in `osreflog/probes/` are deleted unless `pending.json` names them. That file is the contract with the bisect (P7b): it writes `{n, clone, home, record}` before arming and removes it once it has the verdict. An unreadable `pending.json` keeps every clone, since deleting is the one step that can't be undone.

`scripts/p7/g4-step.sh`, with this build installed in the VM:

| Boot | What it showed |
| ---- | -------------- |
| 1, default | an orphan clone p9; probe 3 and its home prepared with kernel files in `/boot/osreflog`; armed; `pending.json` written |
| 2, probe | the boot doctor didn't run (`ConditionResult=no`) |
| 3, default | the doctor had run at boot (`success`, exit 0): removed the probe entry and 2 kernel files; default untouched since probe 3 was armed; "probe 3 booted (kernel 7.2.9…)"; removed p9, kept p3 and p3-home. Then by hand: probe 3 re-armed, `--dry-run` changed nothing; the real run disarmed it ("did not boot, next_entry still set"); with `pending.json` gone, p3 and p3-home were deleted |

Found on the way: `osreflog doctor --boot` first ran the doctor through `systemd-run --pipe --wait`, and from a service that failed with "Failed to start transient service unit: Connection reset by peer", P6's finding #1 again. As root, systemd-run talks to systemd's private socket, and that connection is reset when the caller is a service. The CLI now runs the doctor under `unshare --mount --propagation private` (it refuses a mount that would propagate either way); the boot-time unit keeps the full sandbox. `testvm.sh flow` now starts from a clean VM (no probe entry, `next_entry`, test state or clones left by an earlier run that stopped half-way).

### 2026-10-16: G5, the gate run

`scripts/p7/g5-step.sh`: four probes in a row, each made as the metal tier will make it (record the default; snapshot `/` and home; prepare the clone; plan and install the kernel; entry; `pending.json`; arm), the verdict written by the probe into itself and read back from the clone on the default. `testvm.sh` now gives a VM that prints `RESET-ME` a hard reset (`system_reset`, the power button) five seconds later.

| Probe | What it did | Back on the default |
| ----- | ----------- | ------------------- |
| 1 | this kernel, reused from `/boot`; `/` and `/home` its own; default verified from inside; verdict `bad` | doctor at boot removed the entry, default untouched since arming, "probe 1 booted", clone kept until released; verdict `bad` read back; released, clones deleted |
| 2 | its own **7.2.8** (newer modules removed from the clone): image copied to `/boot/osreflog`, initramfs built from the clone (22 s); booted under **Secure Boot**; verdict `good` | doctor removed the entry and both kernel files; verdict `good` read back |
| 3 | **hangs** in early boot (a unit before `sysinit.target` that never ends); the host reset the VM | the next boot was the default (GRUB had cleared `next_entry` when it started the probe); "probe 3 has no boot record: it never booted" |
| 4 | **crashes the kernel** (`echo c > /proc/sysrq-trigger`, "Kernel panic - not syncing: sysrq triggered crash") | `panic=10` rebooted it with no help, into the default |

After every probe: the default entry verified against the record taken at arming, `/boot` clean, the doctor's run at boot `success`. At the end nothing was left on `/boot` or in `osreflog/probes/`. Every default boot reached `running` in 11.6–13.2 s. Host: `ok   g5-step.sh`.

One thing for P7b: probe 4 has a boot record although it crashed, because the record unit runs after `local-fs.target`, before the crash. A boot record means the probe's root mounted, not that its boot finished. The bisect should treat a probe without a verdict as **skip** (with "it crashed or was reset" when there is a boot record and "it never booted" when there isn't), never as good or bad.

## The boot gate, passed

All five items hold in the test VM ([roadmap](../roadmap.md#boot-gate-passed-2026-10-16)); [recovery.md](../recovery.md) says how to get back by hand. The test VM image stays in `osreflog/testvm` for P7a/P7b development (`sudo scripts/p7/testvm.sh delete` removes it).

What hasn't run on this laptop yet: anything that writes to the real `/boot`. Before P7d's real bisect, `install-dev.sh` puts `osreflog-boot-doctor.service` in place, and the first metal probe on the laptop is armed by hand and watched.
