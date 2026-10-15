# P6: Bisect (nspawn and vm)

- Status: **in progress**
- Started: 2026-10-15
- Roadmap: [P6](../roadmap.md#p6-bisect-nspawn--vm)
- Exit: the seeded suite finds the right cause in ≤ ⌈log₂ n⌉ + 1 probes; an nspawn probe takes < 5 s; a vm probe takes < 30 s and stays within 768 MiB.

## Plan

| Slice | What |
| ----- | ---- |
| **P6a** search | `crates/bisect`, pure: the two-level search (checkpoints, then causes inside the last good→bad interval) as a state machine that asks for one probe at a time and handles skips; the replay plan for "state after cause k" with exact/approximate flags. Unit-tested with simulated oracles, including the probe-count bound |
| **P6b** probes | osreflogd builds a probe: a writable clone of a checkpoint in `osreflog/probes/`, then replay of causes 1..k (content from the cause's blob when exact, reflinked from the bad checkpoint otherwise), `/etc/fstab` emptied, the test and a probe unit copied in |
| **P6c** runners | `osreflogd --probe-runner`, started by the daemon as a transient systemd unit (its own memory limit, outside the daemon's 768 MiB): mounts the probe in its own private namespace, then **nspawn** (the test as a command, or `--boot`) or **vm** (the probe's own kernel, a cached virtiofs initramfs per kernel version, sandboxed virtiofsd, 512 MiB guest, a minimal boot target that runs the test and powers off, a memory guard before starting). The verdict comes back as a file in the probe |
| **P6d** interface | D-Bus `BisectRun` / `BisectStatus` / `BisectReset` (polkit `…bisect`, `auth_admin_keep`), one session at a time, a log the CLI follows: `osreflog bisect run --good A --bad B [--tier nspawn\|vm] [--boot] -- <test>`, `bisect status`, `bisect reset` |
| **P6e** seeded suite | real causes on the real machine with one breaking change among harmless ones: a bad config line, a masked unit, a sysctl drop-in, a module option (vm), a package install that breaks the test, a change split across checkpoints; each bisected, checked for the right cause and the probe count; probe times and qemu RSS measured |

### Decisions

- **Replay is per cause, from the ledger.** A cause's paths are its live-witness records plus the interval's state-witness paths attributed to it (package paths of a dnf transaction). For "state after k", each path takes the version left by the last cause ≤ k that touched it: its blob when that cause recorded one (exact); the bad checkpoint's version when no later cause in the interval touched the path (exact for the state at the interval's end, which is what that cause left); otherwise the bad checkpoint's version is marked **approximate**. Paths no cause explains stay as they were in the good checkpoint. This is Q9's instrument: every result lists its approximations.
- **The runner is outside the daemon.** A VM needs ~615 MiB; the daemon's unit is capped at 768 MiB and has no network namespace to share. `systemd-run` starts each probe as its own unit, and the runner re-mounts the btrfs top level privately (the same propagation guard as the daemon) to reach the probe.
- **Fast VM probes.** P1 measured 34 s to `multi-user.target`. A probe boots `osreflog-probe.target` (basic system + the test unit) instead, which skips the desktop and most services. `--boot` in nspawn runs the same target.
- **Automatic bisect only.** `bisect start / good / bad / skip` (verdicts by hand) belong to the metal tier, P7.
