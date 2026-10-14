# Vision

> **osreflog**: a reflog for your Linux system. See what changed, who changed it, and bisect the change that broke it.

## The problem

A desktop Linux system changes all the time: package updates, firmware updates, config edits by tools and by hand, settings written by apps, services enabled by installers. When something breaks, the user knows one thing: *it worked before*. Answering "what changed since then?" today means:

- `dnf history` covers packages only, and says nothing about which package mattered.
- `journalctl` shows events, but not state.
- Snapper and Timeshift take snapshots, but a snapshot only says "here's the whole system at time T". Diffing two snapshots means comparing hundreds of thousands of files, and the diff says *what*, never *who* or *why*.
- etckeeper versions `/etc` in git, but only at package-transaction boundaries, with no attribution for changes made between them.
- auditd can attribute file writes, but it records events, not state. It can't rebuild or undo anything.

There's also no equivalent of `git bisect`: no way to say "here is a test, find the first change that made it fail".

## What osreflog is

A Linux-native system service that:

1. **Records** every change to system state together with its **cause**: the package transaction, service, command or app that made it.
2. **Checkpoints** the system with btrfs read-only snapshots: before every package transaction, at known-good boots, on a schedule, on demand.
3. **Proves** what changed between checkpoints with btrfs's own change stream, and reconciles that with the live record.
4. **Explains** changes as semantic diffs: "`wifi.powersave` 2 → 3", not a raw byte diff.
5. **Bisects** regressions down to a single cause, by rebuilding intermediate states and testing them in a container, a VM, or a real reboot.
6. **Undoes** one cause or one change, safely, without rolling back the whole system.

## Goals

- **G1: Two witnesses.** Every change is seen by the live witness (fanotify + eBPF, at the moment it happens) *and* by the state witness (btrfs send-stream diff between checkpoints). Gaps and disagreements are reported, never smoothed over.
- **G2: Causes, not files.** The unit the user sees is a cause: one dnf transaction, one `tlp.service` run, one `sudo vim`, one app's settings write. Files hang under causes.
- **G3: Bisect is the headline.** Given a test and a range, find the first bad *cause* in about log₂(n) probes, starting with the cheapest probe tier that can reproduce the problem.
- **G4: Never make things worse.** The design is observe-first: nothing writes to the live root until the write gate (see [roadmap](./roadmap.md)) passes. Undo is a 3-way merge that refuses on conflict; metal bisect always boots a disposable clone, one time only, with the normal entry as fallback.
- **G5: Cheap enough to forget.** Idle CPU and memory low enough to leave on a 7 GB laptop indefinitely; snapshot space bounded and visible.
- **G6: Private by default.** The ledger is a log of your machine's life. Secrets are never stored, command lines are redacted, and the ledger stays local and root-only.
- **G7: Learn the real primitives.** btrfs ioctls and send streams, fanotify, eBPF, mount namespaces, nspawn, KVM/virtiofs, BLS boot entries, polkit, all built by hand.

## Non-goals (for now)

- **Not a backup tool.** Snapshots live on the same disk. If the disk dies, osreflog dies with it. It complements backups.
- **Not a security monitor.** It records changes made by root, but a root attacker can disable it. The ledger is tamper-*evident* (hash chain), not tamper-proof.
- **Not for user data.** It tracks system state (`/etc`, packages, units, boot config) and, optionally, *settings* (dconf, `~/.config`). Documents, photos and browser profiles are out of scope.
- **Not for immutable distros.** rpm-ostree, NixOS and similar systems already have generations.
- **Not filesystem-agnostic.** btrfs only. ext4/xfs on LVM would need another snapshot backend (`snapm`); that's possible later behind a trait, but not designed for now.
- **Not a replacement for dnf.** Package-level undo delegates to `dnf5 history undo`.

## Success criteria

| Milestone | Criterion |
| --------- | --------- |
| Slice | After `dnf upgrade`, `osreflog diff @{1}..@{0}` lists package changes and changed files, from the send stream, in < 2 s |
| Attribution | Over one week of normal use, ≥ 95 % of changed paths under `/etc` are attributed to a cause (measured by reconciliation) |
| Overhead | Daemon idle < 0.3 % of one CPU, RSS < 60 MB; pre-transaction checkpoint adds < 1 s to a dnf transaction |
| Bisect | On the seeded-regression suite, the first bad cause is found in ≤ ⌈log₂ n⌉ + 1 probes; an nspawn probe takes < 5 s |
| Undo | Undo of a file or setting change preserves owner, mode, xattrs and SELinux label, and refuses on a 3-way conflict |
| Honesty | Every report states which witness saw each change and lists gaps (daemon down, queue overflow) |

## Principles

1. **Observe before you touch.** Read-only features ship and prove themselves before anything writes to `/`.
2. **History is evidence.** Record what was observed, how it was observed, and what wasn't seen.
3. **Cheapest probe first.** A container probe that can answer the question beats a VM; a VM beats a reboot.
4. **Delegate what others own.** Packages are dnf's job; boot entries follow BLS and GRUB's own tools.
5. **Use git's vocabulary on purpose.** `log`, `show`, `diff`, `blame`, `bisect`, `@{…}` refs: users already know these words.

## Research angles

- **What changes on a desktop Linux system in a month?** The ledger is a dataset: how many causes, from whom, which paths churn. Publishable as a write-up with an anonymised summary.
- **Attribution coverage.** How often can a change be traced to a cause, and why does it fail when it can't (short-lived processes, queue overflows, offline updates)?
- **Soundness of cause-level bisect.** When does a rebuilt "state after cause k" differ from the state that really existed, and does that difference ever change a bisect verdict?
