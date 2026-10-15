# P6: Bisect (nspawn and vm)

- Status: **closed**
- Started: 2026-10-15
- Closed: 2026-10-15
- Roadmap: [P6](../roadmap.md#p6-bisect-nspawn--vm)
- Exit: the seeded suite finds the right cause in ≤ ⌈log₂ n⌉ + 1 probes; an nspawn probe takes < 5 s; a vm probe takes < 30 s and stays within 768 MiB. **Met:** 5/5 scenarios found the right cause within the bound; nspawn probes 0.3–0.9 s; vm probes 12.7–14.0 s with qemu at 424–436 MiB.

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

## Log

### 2026-10-15: built

- **`crates/bisect`** (pure): the search state machine (tested exhaustively: every culprit position for every n up to 200 is found within ⌈log₂ n⌉ probes; skips give an honest "one of these" range), the replay plan with exact/approximate flags, and the session types shared by the daemon and the CLI.
- **Probes** (`osreflogd/src/probe.rs`): a writable clone of a checkpoint in `osreflog/tmp/` (cleared at start), replay through `openat2` beneath the clone (a test proves a symlinked directory in the clone can't lead a write outside it), `/etc/fstab` emptied, the test and the probe target and service installed.
- **Runner** (`osreflogd --probe-runner`): its own transient unit (`PrivateMounts`, `Delegate`, its own memory limit), mounts the top level privately behind the same propagation guard as the daemon, then nspawn (`--keep-unit --private-network`, the test as a command or `--boot`) or a VM (the probe's newest kernel, a cached host-only virtiofs initramfs, `--sysroot` when the host no longer has that kernel, virtiofsd, 512 MiB guest, a memory guard, `osreflog-probe.target`). It writes its result into the probe for the daemon to read.
- **Session** (`osreflogd/src/bisect.rs`): checks the bad end first, then checkpoints, then causes. Each cause's paths are its live records plus the interval's attributed paths (a dnf transaction's package files), so package causes replay as a unit. If "after all causes" decides the answer, it's tested too: when it passes, the result says no recorded cause explains the change. D-Bus `BisectRun`/`BisectStatus`/`BisectReset` with a new polkit action; `osreflog bisect run|status|reset` follows the log and prints the finding, with each probe's verdict, time, qemu memory and test output.

### 2026-10-15: what the machine found

1. **`systemd-run` from inside the daemon** was refused ("Connection reset by peer") before any probe could start. Rather than loosen the daemon's sandbox, the daemon now starts the probe unit through systemd's D-Bus API (`StartTransientUnit`, on the bus it already uses), waits for the unit to end, and reads the runner's result from the probe.
2. **The VM never powered off.** `SuccessAction=poweroff-force` on the probe service didn't stop the guest, so the first VM probe ran into its 240 s timeout (with the verdict already recorded). The probe service now calls `systemctl poweroff -ff` itself, as P1's probe did, and the runner stops the VM 5 s after the result appears whatever happens. VM probes went from 240 s to 13–14 s.
3. **Probes now keep the test's output.** Two "not reproduced" answers couldn't be explained without it; with it, both were explained at once.

Three seeded scenarios were wrong at first, and bisect said so correctly each time ("not reproduced: the test passes on the bad end"), which is the answer it's meant to give:
- `systemctl is-enabled` in an nspawn command probe prints nothing: no systemd runs there. `systemctl --root=/` reads the files ([bisect.md](../bisect.md) now says so).
- The test unit lived in `/etc/systemd/system`, where `systemctl mask` refuses to replace a unit file; the mask never happened. It lives in `/usr/local/lib/systemd/system` now.
- `options dummy numdummies=3` was overridden by systemd's own `/usr/lib/modprobe.d/systemd.conf` (`numdummies=0`), read later. The probe's output showed `insmod … numdummies=3 numdummies=0`.

### Results

`scripts/p6/seeded.sh`, as root against the installed daemon:

| Scenario | Tier | Causes in the interval | Probes | Found | Slowest probe |
| -------- | ---- | ---------------------- | ------ | ----- | ------------- |
| a bad line in a config file, 4 checkpoints | nspawn | 3 | 1 end + 2 checkpoint + 2 cause | **#486** ✓ | 0.9 s |
| a unit masked (pid 1 writes the link) | nspawn | 4 | 1 end + 2 cause | **#525** ✓ | 0.4 s |
| a sysctl drop-in | nspawn | 4 | 1 end + 2 cause | **#498** ✓ | 0.5 s |
| `dnf install figlet` (its own pre/post checkpoints) | nspawn | 1 | 1 end + 2 checkpoint + 1 cause | **#501** ✓ | 0.4 s |
| a kernel module option | vm | 3 | 1 end + 2 cause | **#535** ✓ | 14.0 s, qemu 436 MiB |

Every cause search stayed within ⌈log₂ n⌉ + 1. The initramfs for 7.2.9 built once in 15.6 s and is cached. config, sysctl and package ran on the build before the two VM fixes above; nspawn command probes use neither the probe unit nor its poweroff, so those results stand.

**Q9 (replay soundness):** no approximate replay in any scenario: every replayed config path had a blob, and the package paths had no later cause. The approximation flag is exercised by unit tests only so far; it stays an open measurement for real-world use.

## What changed in the design

- [bisect.md](../bisect.md): probes start through systemd's D-Bus API; the probe powers itself off; the test's output is kept; `systemctl --root=/` in nspawn command probes; automatic bisect only (manual verdicts come with metal, P7).
- The probe count is ⌈log₂ checkpoints⌉ + ⌈log₂ causes⌉, plus one probe of the bad end that also catches "not reproduced in this tier".
