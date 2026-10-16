# Bisect

`osreflog bisect` finds the first cause after which a test fails, in the spirit of `git bisect run` and Mozilla's `mozregression`.

```
osreflog bisect run --good @{boot-3} --bad @{now} [--tier auto|nspawn|vm] [--needs kernel] -- <test command>
osreflog bisect start --good c40 --bad c47 [--tier auto|nspawn|metal] [--needs hardware]   # verdicts by hand
osreflog bisect good | bad | skip [--yes|--no-reboot]          # on the host, or as root inside the probe
osreflog bisect rearm                                          # a metal probe that didn't boot
osreflog bisect log | reset
```

## The test contract

Same exit-code contract as `git bisect run`, so existing habits transfer:

| Exit code | Verdict |
| --------- | ------- |
| 0 | good |
| 1–124, 126, 127 | bad |
| 125 | skip (this state can't be tested) |
| timeout / probe failed to start | skip, with the reason logged |

The test runs **inside** the probe, as root of the probe (it is a throwaway clone), copied in as `/var/lib/osreflog-probe/test`; its output is kept and shown with each probe. In an nspawn **command** probe no systemd is running: use `systemctl --root=/ …` to read unit state from the files, or `--boot`. Examples:
- `systemctl is-active --wait NetworkManager && nmcli -t general status | grep -q connected` (vm tier)
- `gnome-shell --version` / `flatpak run --command=true org.foo.App` (nspawn tier)
- a script that loads a kernel module and checks `dmesg` (vm tier)

## Two levels: checkpoints, then causes

1. **Checkpoint bisect.** Binary search over the read-only snapshots between good and bad. Each probe is a writable clone of a snapshot (`btrfs subvolume snapshot` with no `-r`, instant).
2. **Cause bisect** inside the last good→bad checkpoint interval. The ledger knows the ordered causes between the two checkpoints. The probe for "state after cause k" is built as:
   - a writable clone of the good checkpoint `cG`, then
   - replay causes `1..k`: for each changed path, reflink (`FICLONE`) the post-change version from the bad checkpoint `cB` or from the blob store, then apply renames/deletes/attrs from the send stream.

   Because causes are chronological, "state after cause k" is a state that really existed, up to concurrent noise such as logs and caches. Package causes are replayed as a unit: every path touched by that dnf transaction, plus its rpmdb, so the probe's package database stays consistent.

   **Soundness caveat** (a research track): a cause's final file content in `cB` may include later edits by other causes to the same path. Replay uses the per-cause blob when one exists (exact) and falls back to `cB`'s version (approximate). Approximate replays are marked, and if one decides the verdict, the result says so.

Probe count: ⌈log₂(checkpoints)⌉ + ⌈log₂(causes in interval)⌉, versus ⌈log₂(all causes)⌉ for an ideal search. The extra probe or two buys cheap checkpoint probes for the coarse search.

## Probe tiers

| Tier | What it is | Can reproduce | Cost on this laptop |
| ---- | ---------- | ------------- | ------------------- |
| `nspawn` | `systemd-nspawn --ephemeral --private-network` (or a clone + `--directory`) on the probe; optional `--boot` to run systemd inside | userspace: binaries, libraries, config parsing, units that start in a container | P1: 0.4–0.55 s for a command, 26 s for a full boot + poweroff |
| `vm` | QEMU/KVM, direct kernel boot of the probe's **own** kernel (`/usr/lib/modules/<ver>/vmlinuz` lives inside the snapshot on Fedora), root on **virtiofs** shared from the probe subvolume, headless, result file read back from the clone | kernel version, modules, boot sequence, systemd ordering, generic drivers (virtio) | P1: 34 s to `running`, 512 MiB guest, 615 MiB qemu RSS, 2 vCPU |
| `metal` | one-time reboot of the real machine into a writable clone of the probe; you give the verdict after boot | anything hardware-specific: Wi-Fi, GPU, suspend, firmware interaction | minutes per probe + your attention |

**Tier choice.** `--tier auto` (the default) picks the cheapest tier that has the test's declared needs: `bisect run` takes nspawn, or vm with `--needs kernel`, and refuses `--needs hardware` (a metal probe needs a person); `bisect start` takes nspawn, or metal with `--needs kernel` or `--needs hardware`. A regression that doesn't reproduce in the chosen tier ends as "not reproduced", with the next tier up to try (nspawn → vm → `bisect start --tier metal`), never a false answer.

### VM tier details

- The kernel and modules come from the probe. The initramfs is built once per kernel version with `dracut --hostonly --no-hostonly-cmdline --omit plymouth --add virtiofs --add-drivers "virtiofs virtio_pci virtio_ring"` and cached in the ledger subvolume. Cmdline: `root=virtiofs:<tag> rw console=ttyS0 selinux=0`. P1 measured a 37 MiB image built in 13 s; the generic `--no-hostonly` image is 254 MiB and panics while unpacking in a 512 MiB guest ([journal](./journal/p1-measure.md#q7-vm-tier-boot-path--works-in-512-mib-with-a-host-only-initramfs)).
- `virtiofsd` (1.14 on this host) runs sandboxed, sharing only the probe subvolume (4 MiB RSS in P1).
- No network by default (`-nic none`); `--net user` opt-in for tests that need it.
- Every probe (both tiers) gets `/etc/fstab` neutralized, or systemd waits for this host's disks by UUID. Units known to fail in a probe are listed in the bisect log and don't count as test failures: in P1 the VM had none, and the container failed only `auditd.service` and `audit-rules.service` (no audit in containers).
- The verdict comes back through a **file** inside the probe, not the console (`--console=pipe` captured nothing from a booted container in P1). The probe service writes the exit code, syncs and runs `systemctl poweroff -ff` itself (P6: `SuccessAction=` didn't power the VM off), and the runner stops the VM 5 s after the result appears in any case.
- Each probe runs as a transient unit that osreflogd starts through systemd's D-Bus API (`StartTransientUnit`, with `PrivateMounts`, `Delegate` and its own memory limit), outside the daemon's sandbox and memory cap. Measured in P6: nspawn command probes 0.3–0.9 s, VM probes 12.7–14.0 s with qemu at 424–436 MiB.
- Budget: one probe at a time, **512 MiB** guest RAM with a free-page-reporting balloon. P1: `running` at 34.2 s with 0 failed units, qemu peak RSS 615 MiB. If `MemAvailable` drops below a threshold, the probe waits.

### Metal tier details

`osreflog bisect start --good A --bad B --tier metal` (verdicts by hand; `bisect run` refuses metal, since a probe can't judge itself). For each probe:

1. **Clone** the probe's state at `osreflog/probes/pN` (and replay causes into it, as for any probe); snapshot the home subvolume as `osreflog/probes/pN-home` when `/home` is a subvolume of the same btrfs.
2. **Prepare** the clone (P7 G3): its fstab mounts `pN` at `/` and `pN-home` at `/home`; osreflogd and the user agent don't start (`ConditionKernelCommandLine=!osreflog.probe`, `ConditionPathExists=!/var/lib/osreflog-probe/manual`); the dnf hook is removed; a oneshot unit records the boot; `/usr/local/bin/osreflog-verdict` gives the verdict whatever osreflog version this state has; a login message and a desktop notification say what this boot is.
3. **Arm** (`osreflogd --boot-worker`, its own transient unit, the only step that writes `/boot`): record the default entry (`saved_entry` + BLAKE3 of its entry file; refused when `saved_entry` isn't set, or a `prev_saved_entry` is), reuse `/boot/vmlinuz-<ver>` when it's the same image or copy the probe's `vmlinuz` and build its initramfs from the clone into `/boot/osreflog/`, write `/boot/loader/entries/osreflog-probe.conf` (the default's entry with the probe's kernel, `rootflags=subvol=osreflog/probes/pN`, `osreflog.probe=N`, `panic=10`), write `osreflog/probes/pending.json`, and set `next_entry=osreflog-probe` with `grub2-editenv` (not `grub2-reboot`, which can rewrite `saved_entry`). GRUB boots it once and clears `next_entry` as it does.
4. **Reboot**: the CLI asks (`--yes` reboots, `--no-reboot` doesn't ask). In the probe: check, then `sudo osreflog-verdict good|bad|skip` (or `sudo osreflog bisect …` with a new enough CLI), and reboot.
5. **Back on the default**: `osreflog-boot-doctor.service` removes the entry and kernel files and checks the default against the record; osreflogd resumes the saved session, reads the verdict from the clone, deletes the clones and arms the next probe. Without a verdict it says whether the probe booted (its boot record) or not, and waits: give the verdict on the host, or `osreflog bisect rearm`. It never skips or re-arms on its own.

A verdict given on the host before rebooting disarms `/boot` through the boot worker; so does `bisect reset`. The boot gate ([roadmap](./roadmap.md)) passed in a Secure Boot test VM before any of this ran on hardware. Getting back by hand: [recovery.md](./recovery.md).

## What a bisect result looks like

```
first bad cause: #2215  2026-10-13 10:13  unit: tlp.service
  /etc/NetworkManager/conf.d/wifi.conf   wifi.powersave 2 → 3
probes: 6 (4 checkpoint, 2 cause) · tier: metal · approximations: none
skipped: c44 (boot failed in probe: unrelated, see log)
next: osreflog undo #2215 --dry-run
```
