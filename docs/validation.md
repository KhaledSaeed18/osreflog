# Validation

Every claim osreflog makes, the check that backs it, the last result, and the command to run it
again. The details of each run, what failed first and why, are in the [journal](journal/) entry
linked from each section.

Most of these checks run as root against the installed daemon, on the development laptop
(i7-8550U, 7 GiB of RAM, Fedora 44 Workstation, kernel 7.2, `/` on btrfs, `compress=zstd:1`, on a SATA disk). They live in `scripts/`,
named after the phase that added them, and print a pass/fail line per check. CI runs the parts
that need no root on every push ([below](#on-every-push)).

## Summary

| Claim | Check | Result |
|---|---|---|
| A package transaction's files and packages show in a diff in under 2 s | `scripts/p2/verify.sh` | **6 ms** (cached); 5/5 doctor checks |
| The send-stream parser reads real kernel streams | unit tests against P1's streams | every CRC verifies; command counts equal `btrfs receive --dump` |
| The rpmdb reader agrees with rpm | unit tests + `rpm -qa` | all 2,154 installed packages match; rpm's `rpmvercmp` vectors pass |
| Changes are tied to their cause | `scripts/p3/testbed.sh`, `scripts/p3/workload.sh` | testbed **16/16**; workload **87/87** config changes (100 %) |
| Tools that ask a daemon over D-Bus get the credit | `scripts/p3/workload.sh` (P4 run) | **6/6** delegated tools credited to the caller |
| Your own settings changes are recorded | `scripts/p4/user-workload.sh` | **8/8** |
| Parsers and differs survive hostile input | `scripts/p4/fuzz.sh 3600` | 8 targets × 1 h, ~3.05 billion inputs, **0 findings** |
| Undo can't be tricked into writing outside its target | `scripts/p5/gate.sh` | write gate **16/16**; symlink-swap race: 3,541 runs, **0 escapes** |
| Undo restores what it says and never overwrites a later change | `scripts/p5/undo-workload.sh` | **15/15**, including a refused conflict and an undo of an undo |
| A flood of processes and files doesn't sink the daemon | `scripts/p5/flood.sh` | 100,000 processes, 300,000 files: 55 → 104 MB, no queue overflow |
| Bisect finds the cause in ⌈log₂ n⌉ + 1 probes | `scripts/p6/seeded.sh` | **5/5** seeded regressions; nspawn ≤ 0.9 s, VM ≤ 14 s at ≤ 436 MiB |
| A probe on real hardware never touches the default boot | `scripts/p7/testvm.sh`, `g2…g5-step.sh` | the boot gate: 4 probes, a hang and a panic in a Secure Boot VM, default untouched |
| Metal bisect works on this laptop | `scripts/p7/p7d-wifi.sh` | Wi-Fi power save regression found in **3 probes**, 6 reboots |
| It installs from COPR on a fresh system | `scripts/p8/coprvm.sh` | fresh Fedora 44 Cloud VM, two boots: every check passed |
| The hardened units leave everything working | `scripts/p8/hardening-check.sh` | install, doctor, testbed 16/16, write gate, live undo, 5/5 bisects, boot doctor: all pass; **nothing refused** by the filter |
| What the kept snapshots hold is reported within bounds (0.3.0) | unit tests; `scripts/p2/it.sh` | unit tests pass; the loopback check (5 MiB held) **not yet run** as root |
| The workers run in their own SELinux domains (0.3.0) | `scripts/p8/selinux-workers.sh` | module compiles (here and in CI); **not yet run** as root |
| The history is tamper-evident | `osreflog verify` | the chain verified after every run above (711 entries after P5's) |

## The state witness (P2)

What changed between two checkpoints, from btrfs alone. [Journal](journal/p2-slice.md).

- **Run:** `sudo scripts/p2/verify.sh`: a loopback btrfs integration test, then install, a real
  `dnf remove figlet`, and the timings.
- **Result:** loopback test 2/2; doctor 5/5; pre-transaction checkpoint 742 ms, post 135 ms;
  `osreflog diff @{1}..@{0}` 6 ms from the ledger; the interval itself computed in the background
  in 5.6 s (mostly reading the 134 MB rpmdb).

## The live witness and causes (P3, P4)

Who changed each file, as it happens. [P3 journal](journal/p3-live.md),
[P4 journal](journal/p4-semantics.md).

- **Run:** `sudo scripts/p3/testbed.sh` (16 scripted scenarios: `sed -i`, an editor's save by
  rename, `systemctl enable`, a unit file, `dnf install`, an `nmcli` change, a secret file) and
  `sudo scripts/p3/workload.sh` (real admin tools, then `scripts/p3/coverage.py`).
- **Result:** testbed 16/16; workload 87/87 config changes and 239/239 non-noise changes have
  their cause; with D-Bus caller attribution (P4), 86/86 and 236/236, and all six tools that ask a
  daemon to write (among them `timedatectl`, `hostnamectl`, `tuned-adm`, `firewall-cmd` and
  `nmcli`) credited to the caller.
- **Misses that are by design:** changes to access times only raise no fanotify event; they
  count as noise, and the state witness still sees them.

## Hostile input (P4)

Every parser and differ fuzzed with a stable-toolchain harness (`fuzz/`): send streams, rpm
headers, rpmdb diffs, version comparison, fanotify records, refs, semantic summaries, line diffs.

- **Run:** `scripts/p4/fuzz.sh [SECS]`, all targets in parallel at the lowest priority, then
  200,000 version pairs compared with rpm's own `rpmvercmp` (Python bindings).
- **Result:** 1 h per target, about 3.05 billion inputs, 0 crashes, 0 inputs over 500 ms of CPU,
  0 mismatches with rpm. CI runs every target for 30 s on each push.

## Undo (write gate, P5)

osreflog's first writes to the system, gated before they shipped. [Journal](journal/p5-undo.md),
[ADR 0011](decisions/0011-gate-in-a-contained-scratch-root.md).

- **Run:** `sudo scripts/p5/gate.sh` (the undo engine in a contained scratch root under the
  daemon's own sandbox), `sudo scripts/p5/undo-workload.sh` (the installed daemon, live),
  `scripts/p5/flood.sh` (as an ordinary user).
- **Gate, 16/16:** conflicts, deletions, creations, renames, owner, mode, xattrs and SELinux
  labels restored exactly; bind mounts not crossed; `/proc` magic links refused; `/etc/shadow`,
  file capabilities and set-user-ID files refused even as root; a parent directory swapped with
  a symlink to outside for 3 s: 3,541 runs, 375 restored inside, 3,166 refused, **0 escaped**.
- **Live, 15/15:** every change kind put back, recorded as an `undo` cause; a file changed again
  after the cause left alone as a conflict; the undo itself undone; a `*.key` file refused; a dnf
  transaction delegated to `dnf5 history undo`.

## Bisect (P6, P7)

[P6 journal](journal/p6-bisect.md), [P7 journal](journal/p7-metal.md).

- **Run:** `sudo scripts/p6/seeded.sh`.

| Seeded regression | Tier | Probes | Found | Slowest probe |
|---|---|---|---|---|
| A bad line in a config file, 4 checkpoints | nspawn | 1 end + 2 checkpoint + 2 cause | ✓ | 0.9 s |
| A unit masked | nspawn | 1 end + 2 cause | ✓ | 0.4 s |
| A sysctl drop-in | nspawn | 1 end + 2 cause | ✓ | 0.5 s |
| `dnf install` | nspawn | 1 end + 2 checkpoint + 1 cause | ✓ | 0.4 s |
| A kernel module option | VM | 1 end + 2 cause | ✓ | 14.0 s, qemu 436 MiB |

- **Boot gate:** in a nested test VM with OVMF and Secure Boot (`scripts/p7/testvm.sh`), four
  probes in a row, one on its own older kernel; a probe stuck in early boot and hard-reset; a
  kernel panic rebooting itself. The default entry (`grubenv`'s `saved_entry` and the entry file)
  was checksummed unchanged after each, and the boot doctor cleaned up every leftover.
- **Metal, on the laptop:** NetworkManager's Wi-Fi power save switched on by one of three causes,
  found (#680) in 3 probes and 6 reboots, the default entry untouched.

## Packaging (P8)

[Journal](journal/p8-app.md).

- **Run:** `sudo scripts/p8/coprvm.sh`: a fresh Fedora 44 Cloud image on btrfs, cloud-init,
  `dnf copr enable` and `dnf install osreflog osreflog-app`, then two boots.
- **Result:** `rpm -V` clean; doctor 5/5 before and after the reboot; `dnf install figlet` and
  `dnf remove figlet` each bracketed by checkpoints with their cause; the boot checkpoint taken by
  the units at boot; `verify` passed on 21 entries; one warning in the journal, the expected gap
  across the reboot.

## On every push

[`.github/workflows/ci.yml`](../.github/workflows/ci.yml), in a Fedora 44 container:

- `cargo fmt --check`; `cargo clippy` with warnings as errors (pedantic lints, no `unwrap`, no
  `expect`); the workspace's tests, as COPR's `%check` runs them (no root, no btrfs).
- Every fuzz target for 30 s, and the version comparison against rpm.
- The units' systemd exposure, which may only go down (osreflogd 2.4, the boot doctor 2.2 and the
  user agent 1.0 today).
- The SELinux module compiles.
- The D-Bus and polkit policies, the desktop entry and the AppStream metainfo.
- The spec parses, and its Version and License agree with the workspace and the crates the
  binaries link (`scripts/dist.sh --check`).
- `cargo deny`: advisories, licenses, sources.

## Not validated yet

Said here so nobody has to find out the hard way.

- **A week of real history.** The app has shown a few days of this laptop's history; the P8 exit
  asks for a week.
- **Long-running cost.** The first soak ended after 6.1 h (a restart, then a shutdown); the
  second, which survives both, started 2026-10-20 and needs 48 h sampled
  ([performance](performance.md#over-days)).
- **0.3.0's root checks.** The space report against a real filesystem (`sudo scripts/p2/it.sh`),
  the worker domains (`sudo scripts/p8/selinux-workers.sh`), and the live witness starting before
  the boot checkpoint (a reboot after the install) are written and wait for a run as root.
- **Replay soundness (Q9).** No seeded scenario needed an approximate replay; how often one
  differs from the true state needs real-world data.
- **Other hardware and layouts.** One laptop, one btrfs layout (Fedora's default). No AMD, no
  NVMe-specific numbers, no LUKS-on-btrfs check yet.
- **SELinux confinement.** `osreflog-selinux` gives the ledger and runtime files types of their
  own, but osreflogd itself runs unconfined (`unconfined_service_t`): a domain for a daemon that
  reads every file and, on undo, writes any of them would mostly be a label. The summary and undo
  workers have domains of their own (0.3.0), permissive until `sudo scripts/p8/selinux-workers.sh`
  has collected their denials on a real machine and the rules cover them.
