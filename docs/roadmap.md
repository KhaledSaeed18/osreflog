# Roadmap

Status: **P1 in progress** (started 2026-10-14). P0 closed 2026-10-14. Each phase ends with a written note in `docs/journal/` (created when Phase 1 starts): what worked, what broke, and what changed in the design.

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

## P1: Measure by hand (current)
Learn the primitives with existing tools before writing Rust.
- `btrfs subvolume snapshot -r` by hand from a private mount of the top level (`mount -o subvolid=5` in `unshare -m`); time it
- `btrfs send --no-data -p` between two snapshots around a `dnf upgrade`; save the stream; read it with `btrfs receive --dump`
- `fatrace -f CWDO` (fanotify) for a normal working hour: events/s for `/etc`, for the whole filesystem, and with Firefox running. This answers Q1.
- `bpftrace` one-liners on `sched_process_exec/exit`: exec rate, lifetimes of rpm scriptlets during an update
- Install `libdnf5-plugin-actions`; a `pre_transaction` action that sleeps 5 s. Confirm dnf blocks on it, and check whether GNOME Software/PackageKit loads the plugin (Q4)
- `systemd-nspawn --ephemeral -D <snapshot>` with a test command; `--boot` into a clone
- Boot a clone in QEMU: snapshot kernel + `dracut --no-hostonly` initramfs + virtiofs root; measure boot time and RSS
- `cp --reflink` from a snapshot into `/` (cross-mount FICLONE)

**Exit:** a reproducible `scripts/p1/` set plus a journal entry with numbers. Q1, Q2, Q4 and Q7 answered, and the (verify) markers in the docs updated.

## P2: Vertical slice: state witness only
The whole read-only path, with no live capture yet.
- Cargo workspace, `osreflogd` + `osreflog` skeletons, D-Bus API (read methods), polkit policy
- `btrfs` crate: snapshot/subvolume ioctls, send-stream (v2) parser with fixture streams from P1
- Ledger: SQLite schema, checkpoints, refs (`@{1}`, `c42`), hash chain
- dnf5 actions hook → `pre-dnf`/`post-dnf` checkpoints; rpmdb diff + dnf history import
- `osreflog checkpoint`, `log`, `diff @{1}..@{0}` (structural + packages; unified diff fallback)
- Retention policy and `osreflog doctor` (layout, space, hook installed)

**Exit:** after a real `dnf upgrade`, `osreflog diff @{1}..@{0}` shows packages and changed files in < 2 s; the daemon idles at < 0.3 % CPU.

## P3: Live witness and causes
- `capture` crate: fanotify with scoped recursive marks, dir handles, PIDFD
- `lineage` + `lineage-ebpf` (aya): fork/exec/exit ring buffer, pid+starttime cache with grace period
- Cause builder rules (dnf bracket, unit, session/command, unknown); quiet-period closing
- Blob store (BLAKE3 + zstd) with secrets denylist and argv redaction
- Reconciliation at each checkpoint: confirmed / `offline` / transient; gap records
- `osreflog why <path>`, `show #id`, `blame <path>` (line → cause, for text configs)
- testbed: scripted scenarios in a VM (`sed -i`, `systemctl enable`, `dnf install`, `nmcli` change, editor save-by-rename) with expected causes

**Exit:** the testbed passes; one week of dogfooding gives ≥ 95 % attribution of `/etc` changes, with the misses explained in the journal.

## P4: Semantic diffs and user scope
- `osreflog-parse` sandboxed worker (seccomp + Landlock + cgroup limits)
- Differs: keyfile/INI, systemd effective config + enablement, passwd/group, key=value, fstab, BLS, JSON/TOML/YAML, dconf, SELinux bits; a fuzz target for each
- `osreflog-user`: dconf `Change` monitoring with app attribution; `~/.config` inode marks
- systemd `EnableUnitFiles` caller attribution via a system bus monitor

**Exit:** every differ is fuzzed for at least 1 h with no crashes; `osreflog log` shows readable semantic summaries for the testbed scenarios.

## WRITE GATE
Before any code writes to the live `/`:
- [ ] The undo engine passes its full test matrix in a VM (conflicts, symlink tricks, SELinux labels, xattrs, renames, deleted files)
- [ ] `openat2` resolution tests: symlink swap races, `..`, magic links (`/proc/self/fd`), bind mounts
- [ ] A `pre-undo` checkpoint is taken and verified before every apply
- [ ] Threat-model mitigations for A1/A3 implemented and tested
- [ ] Dry run is the default everywhere

## P5: Undo
- 3-way undo for files, deletions, creations (attic), unit enablement, dconf keys, BLS/grubenv
- Package causes delegated to `dnf5 history undo` with confirmation
- `osreflog undo --dry-run / --apply`; undo of an undo

**Exit:** the undo matrix passes on the real machine for the testbed scenarios; no conflict is ever silently overwritten.

## P6: Bisect (nspawn + vm)
- Probe planner: checkpoint bisect, then cause bisect with replay (reflink from `cB` or blobs); approximation flags
- `nspawn` runner (ephemeral, `--boot` option, test copy-in, timeout)
- `vm` runner: QEMU direct kernel boot from the probe, cached dracut virtiofs initramfs, sandboxed virtiofsd, vsock/serial result channel, memory guard
- Seeded-regression suite: 6–10 scenarios (bad config line, package downgrade breaking a binary, masked unit, kernel module option, sysctl, SELinux boolean)
- `bisect run / start / good / bad / skip / log / reset`

**Exit:** the seeded suite finds the right cause in ≤ ⌈log₂ n⌉ + 1 probes; an nspawn probe takes < 5 s; a vm probe takes < 30 s and stays within 768 MiB.

## BOOT GATE
Before any probe boots on real hardware:
- [ ] The full metal flow works inside a **nested** test VM (a VM whose disk is a Fedora btrfs install, using the same GRUB/BLS)
- [ ] The default entry is verified untouched after every probe (checksum of `grubenv` saved_entry + entry file)
- [ ] A hung or panicked probe returns to the default on the next power cycle (tested in the VM)
- [ ] `osreflog doctor --boot` cleans up leftovers and is run automatically at every boot
- [ ] A recovery note in the docs: how to boot the default entry by hand from the GRUB menu

## P7: Metal bisect
- Probe kernel/initramfs preparation, temporary BLS entry, `grub2-reboot` one-shot
- Probe-side oneshot unit and verdict prompt; resume the bisect after reboot
- `--tier auto` with capability hints (`--needs kernel|hardware`)

**Exit:** one real hardware-specific regression (seeded, e.g. a Wi-Fi powersave config) bisected on this laptop end to end.

## P8: GNOME app, packaging, hardening
- GTK4/libadwaita app: timeline of causes, cause detail with semantic diffs, "what changed since…" picker, bisect wizard (nspawn/vm), undo with dry-run preview
- SELinux policy module; systemd hardening audit (`systemd-analyze security`)
- COPR packaging (like drainscope), RPM with the dnf actions file, units, polkit, D-Bus config
- Docs: user guide, design write-up, the "what changes in a month" research note

**Exit:** installable from COPR on a fresh Fedora 44 VM; `osreflog doctor` is clean; the app shows a real week of history.
