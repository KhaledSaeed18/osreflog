# Roadmap

Status: **P8 (GNOME app, packaging, hardening) in progress: P8a (the history browser), P8b ("what changed since…"), P8c (undo with the preview) and P8d (the bisect wizard) done.** P0 closed 2026-10-14; P1 to P6 and the write gate closed 2026-10-15; the boot gate passed and P7 closed 2026-10-16. Each phase ends with a written note in `docs/journal/` (created when Phase 1 starts): what worked, what broke, and what changed in the design.

The order is deliberate: **measure before designing in stone, read-only before writing, VM before metal, the GUI last.**

```
 P0 ──► P1 ──► P2 ──► P3 ──► P4 ──► [WRITE GATE] ──► P5 ──► P6 ──► [BOOT GATE] ──► P7 ──► P8
 docs   measure slice  capture semantics             undo   bisect                 metal   app+pkg
        by hand  (state) (live)  + user scope               nspawn+vm              bisect
```

---

## P0: Planning (closed 2026-10-14)
- [x] Idea, constraints, host facts ([environment.md](./environment.md))
- [x] Name chosen ([naming/](./naming/))
- [x] Vision, architecture, capture, bisect, undo, threat model drafts
- [x] Review all docs; resolve or park every item in [open-questions.md](./open-questions.md). The owner delegated decisions; Q10 resolved, the rest are P1+ measurements

**Exit:** docs reviewed; Phase 1 checklist agreed.

## P1: Measure by hand (closed 2026-10-15)
Learn the primitives with existing tools before writing Rust.
- `btrfs subvolume snapshot -r` by hand from a private mount of the top level (`mount -o subvolid=5` in `unshare -m`); time it
- `btrfs send --no-data -p` between two snapshots around a `dnf upgrade`; save the stream; read it with `btrfs receive --dump`
- `fatrace -f CWDO` (fanotify) for a normal working hour: events/s for `/etc`, for the whole filesystem, and with Firefox running. This answers Q1.
- `bpftrace` one-liners on `sched_process_exec/exit`: exec rate, lifetimes of rpm scriptlets during an update
- Install `libdnf5-plugin-actions`; a `pre_transaction` action that sleeps 5 s. Confirm dnf blocks on it, and check whether GNOME Software/PackageKit loads the plugin (Q4)
- `systemd-nspawn --ephemeral -D <snapshot>` with a test command; `--boot` into a clone
- Boot a clone in QEMU: snapshot kernel + `dracut --no-hostonly` initramfs + virtiofs root; measure boot time and RSS
- `cp --reflink` from a snapshot into `/` (cross-mount FICLONE)

**Exit:** a reproducible `scripts/p1/` set plus a journal entry with numbers. Q1, Q2, Q4 and Q7 answered, and the (verify) markers in the docs updated. **Met** ([journal](./journal/p1-measure.md)): Q1, Q2, Q7, Q8 resolved; Q4 answered for dnf and PackageKit, with the offline-update path moved to P2.

## P2: Vertical slice: state witness only (closed 2026-10-15)
The whole read-only path, with no live capture yet.
- Cargo workspace, `osreflogd` + `osreflog` skeletons, D-Bus API (read methods), polkit policy
- `btrfs` crate: snapshot/subvolume ioctls, send-stream (v2) parser with fixture streams from P1
- Ledger: SQLite schema, checkpoints, refs (`@{1}`, `c42`), hash chain
- dnf5 actions hook → `pre-dnf`/`post-dnf` checkpoints; rpmdb diff + dnf history import
- `osreflog checkpoint`, `log`, `diff @{1}..@{0}` (structural + packages; unified diff fallback)
- Diff computed eagerly after each checkpoint and cached in the ledger (P1: 3.7 s to send a large transaction); default noise filter for `/var/cache`, `/var/log`, package databases
- Test fixtures from a loopback btrfs image (deterministic send streams), never from the real disk
- Q4 remainder: GNOME Software offline updates via dnf5daemon-server; Q3: send-stream size estimates, squota evaluated on the loopback image
- Retention policy and `osreflog doctor` (layout, space, hook installed)

**Exit:** after a real `dnf upgrade`, `osreflog diff @{1}..@{0}` shows packages and changed files in < 2 s; the daemon idles at < 0.3 % CPU. **Met** ([journal](./journal/p2-slice.md)) with a real `dnf remove`: 6 ms, 0.0 % idle CPU, 26 MB RSS. Moved on: Q3 (space reporting) and Q4's offline-update path; diffs between pruned, non-consecutive checkpoints.

## P3: Live witness and causes (closed 2026-10-15)
- `capture` crate: one fanotify filesystem mark, scope filtering in userspace, resolution by dir handle (`open_by_handle_at`), PIDFD; re-measure event rates under heavy browser load
- `lineage` + `lineage-ebpf` (aya): fork/exec/exit ring buffer, pid+starttime cache with grace period
- Cause builder rules (dnf bracket, unit, session/command, unknown); quiet-period closing
- Blob store (BLAKE3 + zstd) with secrets denylist and argv redaction
- Reconciliation at each checkpoint: confirmed / `offline` / transient; gap records
- `osreflog why <path>`, `show #id`, `blame <path>` (line → cause, for text configs)
- testbed: scripted scenarios in a VM (`sed -i`, `systemctl enable`, `dnf install`, `nmcli` change, editor save-by-rename) with expected causes

**Exit:** the testbed passes; one week of dogfooding gives ≥ 95 % attribution of `/etc` changes, with the misses explained in the journal. **Amended and met** ([journal](./journal/p3-live.md)): the machine is dev-only, so a scripted workload of ~22 real admin tools replaced the week: 87/87 config changes explained, testbed 16/16.

## P4: Semantic diffs and user scope (closed 2026-10-15)
- **First:** attribute D-Bus-delegated writes to the caller (timedatectl, hostnamectl, tuned-adm, firewall-cmd, nmcli, systemctl: P3 found six such tools). Monitor method calls on the system bus and join them to the daemon's writes
- `osreflog-parse` sandboxed worker (seccomp + Landlock + cgroup limits)
- Differs: keyfile/INI, systemd effective config + enablement, passwd/group, key=value, fstab, BLS, JSON/TOML/YAML, dconf, SELinux bits; a fuzz target for each
- `osreflog-user`: dconf `Change` monitoring with app attribution; `~/.config` inode marks
- systemd `EnableUnitFiles` caller attribution via a system bus monitor

**Exit:** every differ is fuzzed for at least 1 h with no crashes; `osreflog log` shows readable semantic summaries for the testbed scenarios. **Met** ([journal](./journal/p4-semantics.md)): 8 fuzz targets × 1 h, ~3.05 billion inputs, 0 findings, plus a differential check of version comparison against rpm (200,000 pairs, 0 mismatches); D-Bus-delegated writes credited to the caller for all six tools; workload 16/16 and user workload 8/8. Deferred: `~/.config` marks and the TOML/YAML/SELinux/effective-config differs to P8, the BLS differ to P5.

## WRITE GATE (passed 2026-10-15)
Before any code writes to the live `/`:
- [x] The undo engine passes its full test matrix ~~in a VM~~ in a contained scratch root under the daemon's own sandbox ([ADR 0011](./decisions/0011-gate-in-a-contained-scratch-root.md); it runs again in a VM in P6) (conflicts, symlink tricks, SELinux labels, xattrs, renames, deleted files)
- [x] `openat2` resolution tests: symlink swap races, `..`, magic links (`/proc/self/fd`), bind mounts
- [x] A `pre-undo` checkpoint is taken and verified before every apply
- [x] Threat-model mitigations for A1/A3 implemented and tested
- [x] Dry run is the default everywhere

## P5: Undo (closed 2026-10-15)
- 3-way undo for files, deletions, creations (attic), unit enablement, dconf keys, ~~BLS/grubenv~~ (moved to P7: `/boot` is outside the btrfs witness)
- Package causes delegated to `dnf5 history undo` with confirmation
- `osreflog undo --dry-run / --apply`; undo of an undo

**Exit:** the undo matrix passes on the real machine for the testbed scenarios; no conflict is ever silently overwritten. **Met** ([journal](./journal/p5-undo.md)): gate 16/16 as root (0 escapes in a symlink-swap race), live undo workload 15/15 including a refused conflict and an undo of an undo, flood bounded.

## P6: Bisect (nspawn + vm) (closed 2026-10-15)
- Probe planner: checkpoint bisect, then cause bisect with replay (reflink from `cB` or blobs); approximation flags
- `nspawn` runner (ephemeral, `--boot` option, test copy-in, timeout)
- `vm` runner: QEMU direct kernel boot from the probe, cached dracut virtiofs initramfs, sandboxed virtiofsd, vsock/serial result channel, memory guard
- Seeded-regression suite: 6–10 scenarios (bad config line, package downgrade breaking a binary, masked unit, kernel module option, sysctl, SELinux boolean)
- `bisect run / start / good / bad / skip / log / reset`

**Exit:** the seeded suite finds the right cause in ≤ ⌈log₂ n⌉ + 1 probes; an nspawn probe takes < 5 s; a vm probe takes < 30 s and stays within 768 MiB. **Met** ([journal](./journal/p6-bisect.md)): 5/5 scenarios (config line, masked unit, sysctl, dnf transaction, kernel module option in a VM) found the right cause within the bound; nspawn probes ≤ 0.9 s, vm probes ≤ 14 s at ≤ 436 MiB. Manual verdicts (`start/good/bad/skip`) move to P7 with the metal tier; the SELinux-boolean scenario wasn't built (neither tier runs SELinux).

## BOOT GATE (passed 2026-10-16)
Before any probe boots on real hardware ([journal](./journal/p7-metal.md)):
- [x] The full metal flow works inside a **nested** test VM (a VM whose disk is a Fedora btrfs install, using the same GRUB/BLS): built from a checkpoint of this machine, OVMF with Secure Boot; four probes in a row, including one on its own older kernel installed in `/boot/osreflog`
- [x] The default entry is verified untouched after every probe (checksum of `grubenv` saved_entry + entry file)
- [x] A hung or panicked probe returns to the default on the next power cycle (tested in the VM): a probe stuck in early boot and hard-reset, and a kernel crash rebooting itself through `panic=10`
- [x] `osreflog doctor --boot` cleans up leftovers and is run automatically at every boot (`osreflog-boot-doctor.service`)
- [x] A recovery note in the docs: how to boot the default entry by hand from the GRUB menu ([recovery.md](./recovery.md))

## P7: Metal bisect (closed 2026-10-16)
- [x] Probe kernel/initramfs preparation, temporary BLS entry, ~~`grub2-reboot`~~ `grub2-editenv set next_entry` one-shot (boot gate, G2)
- [x] Probe-side oneshot unit and verdict prompt (G3); manual verdicts (`bisect start / good / bad / skip / log`), the session saved in the ledger and resumed (P7a, with container probes)
- [x] `--tier metal`: arm, reboot, verdict in the probe, reboot, continue (P7b: a metal bisect in the test VM found the right cause in 3 probes, 7 boots)
- [x] `--tier auto` with capability hints (`--needs kernel|hardware`) (P7c)

**Exit:** one real hardware-specific regression (seeded, e.g. a Wi-Fi powersave config) bisected on this laptop end to end. **Met** ([journal](./journal/p7-metal.md)): NetworkManager's Wi-Fi power save switched on by one of three causes, found (#680) in 3 metal probes, 6 reboots, the default entry untouched throughout.

## P8: GNOME app, packaging, hardening
- GTK4/libadwaita app: timeline of causes, cause detail with semantic diffs, "what changed since…" picker, bisect wizard (nspawn/vm), undo with dry-run preview
- SELinux policy module; systemd hardening audit (`systemd-analyze security`)
- COPR packaging (like drainscope), RPM with the dnf actions file, units, polkit, D-Bus config
- Docs: user guide, design write-up, the "what changes in a month" research note

**Exit:** installable from COPR on a fresh Fedora 44 VM; `osreflog doctor` is clean; the app shows a real week of history.
