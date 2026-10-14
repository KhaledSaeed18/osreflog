# Bisect

`osreflog bisect` finds the first cause after which a test fails, in the spirit of `git bisect run` and Mozilla's `mozregression`.

```
osreflog bisect run --good @{boot-3} --bad @{now} [--tier nspawn|vm|metal] -- <test command>
osreflog bisect start --good c40 --bad c47 --tier metal      # manual verdicts
osreflog bisect good | bad | skip
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

The test runs **inside** the probe, as root of the probe (it is a throwaway clone), with the test file copied in read-only. Examples:
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
| `nspawn` | `systemd-nspawn --ephemeral` (or a clone + `--directory`) on the probe; optional `--boot` to run systemd inside | userspace: binaries, libraries, config parsing, units that start in a container | ~seconds, tens of MB |
| `vm` | QEMU/KVM, direct kernel boot of the probe's **own** kernel (`/usr/lib/modules/<ver>/vmlinuz` lives inside the snapshot on Fedora), root on **virtiofs** shared from the probe subvolume, headless, serial console + vsock for test I/O | kernel version, modules, boot sequence, systemd ordering, generic drivers (virtio) | ~10–30 s boot, 768 MiB RAM, 2 vCPU |
| `metal` | one-time reboot of the real machine into a writable clone of the probe; you give the verdict after boot | anything hardware-specific: Wi-Fi, GPU, suspend, firmware interaction | minutes per probe + your attention |

**Tier choice.** `--tier auto` (default) picks the cheapest tier whose capabilities match the test's declared needs (`--needs kernel`, `--needs hardware`), and suggests trying a cheaper tier first. A regression that doesn't reproduce in the chosen tier is reported as "good in vm on both ends: try metal" rather than a false answer.

### VM tier details

- The kernel and modules come from the probe; the initramfs is built once per kernel version with `dracut --no-hostonly --add virtiofs` and cached in the ledger subvolume _(verify the dracut module name and that root=virtiofs tag boot works with Fedora's modular `virtio_fs`)_.
- `virtiofsd` (1.14 on this host) runs sandboxed, sharing only the probe subvolume.
- No network by default (`-nic none`); `--net user` opt-in for tests that need it.
- Units known to fail in a VM (e.g. hardware-specific services) are masked in the probe by a small drop-in set, listed in the bisect log.
- Budget: one probe at a time. 768 MiB guest RAM fits beside a GNOME session with ~1.5 GiB available on this host. If `MemAvailable` drops below a threshold, the probe waits.

### Metal tier details (behind the boot gate)

1. Build the probe clone at `osreflog/probes/pN`.
2. Make sure its kernel is bootable: if `/boot` already has `vmlinuz-<ver>` (Fedora keeps 3 kernels), reuse it. Otherwise copy `vmlinuz` from the probe and generate an initramfs into `/boot/osreflog/`.
3. Write a **temporary BLS entry** `/boot/loader/entries/osreflog-probe.conf` with `rootflags=subvol=osreflog/probes/pN` and `osreflog.probe=N`.
4. Arm it for **one boot only** (`grub2-reboot <id>`). The default entry is untouched, so a failed probe boot falls back to your normal system on the next power cycle _(verify: grub2-reboot with BLS ids, and greenboot-style fallback if the probe hangs)_.
5. After the probe boots, a oneshot unit shows "osreflog bisect probe N: run your check, then `osreflog bisect good|bad`". The verdict is written to the ledger subvolume, which every probe mounts at `/var/lib/osreflog`.
6. Rebooting returns to the default entry; the daemon deletes the probe entry and clone and continues the bisect.

The boot gate ([roadmap](./roadmap.md)) must pass before step 3 is ever run on real hardware.

## What a bisect result looks like

```
first bad cause: #2215  2026-10-13 10:13  unit: tlp.service
  /etc/NetworkManager/conf.d/wifi.conf   wifi.powersave 2 → 3
probes: 6 (4 checkpoint, 2 cause) · tier: metal · approximations: none
skipped: c44 (boot failed in probe: unrelated, see log)
next: osreflog undo #2215 --dry-run
```
