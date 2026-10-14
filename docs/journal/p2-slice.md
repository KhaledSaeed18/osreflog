# P2: Vertical slice, state witness only

- Status: **in progress**
- Started: 2026-10-15
- Roadmap: [P2](../roadmap.md#p2-vertical-slice-state-witness-only-next)
- Exit: after a real package transaction, `osreflog diff @{1}..@{0}` shows the package changes and changed files in < 2 s, and the daemon idles at < 0.3 % CPU.

## Plan

The whole read-only path: checkpoints, the send-stream diff, packages, the ledger, the D-Bus service and the CLI. There's no live witness yet (that's P3), so every interval is reported as state-witness-only.

| Crate | What | Unsafe |
| ----- | ---- | ------ |
| `osreflog-model` | serde types shared by daemon and CLI: checkpoints, intervals, changes, packages, path classes | no |
| `osreflog-btrfs` | CRC32C, send-stream (v1/v2) parser, change-set builder with rename tracking, ioctls (snapshot, delete, subvolume info, send) | **only `ioctl.rs`** |
| `osreflog-rpm` | rpm header parser, `rpmvercmp`, rpmdb and dnf5 history readers (from a snapshot), package diff | no |
| `osreflog-ledger` | SQLite schema v1, hash chain, refs (`c42`, `@{1}`, `@{now}`, `@{2026-10-15 10:00}`), retention policy | no |
| `osreflogd` | layout + private top-level mount, engine, D-Bus service with polkit | no (relies on systemd `PrivateMounts=yes`, or `unshare -m` in dev, and refuses to run in PID 1's mount namespace) |
| `osreflog` | CLI: `status`, `log`, `diff`, `checkpoint`, `verify`, `hook`, `doctor` | no |

Decisions taken while planning:

- **ioctls, not the `btrfs` CLI**, for snapshot, delete, subvolume info and send. The structs were checked against `/usr/include/linux/btrfs.h` with a C program: `btrfs_ioctl_vol_args_v2` is 4096 bytes, `btrfs_ioctl_send_args` 72, `btrfs_ioctl_get_subvol_info_args` 504. All `unsafe` lives in one module.
- **Send is streamed**: the ioctl runs on a thread writing into a pipe; the parser consumes the read end, so an 18 MB stream is never buffered.
- **The parser is validated against P1's real streams**: its command counts must match `btrfs receive --dump` for the same stream.
- **rpmdb and dnf history are read from copies** of the snapshot's SQLite files (with their `-wal`), because SQLite needs to write `-shm` and the snapshot is read-only. On btrfs the copy is a reflink, so it's instant.
- **The daemon never unshares by itself.** It checks that its mount namespace differs from PID 1's and refuses otherwise, so the top-level mount can never leak into the host's namespace.
- **The diff is computed after the hook returns.** The post-transaction hook replies once the snapshot exists, and the interval is computed in the background, so dnf isn't held for the send.
- **D-Bus payloads are JSON strings** for P2, the same JSON the CLI prints with `--json`. Typed D-Bus structs can come with the GNOME app.
- **Root-only integration tests** run against a loopback btrfs image (`scripts/p2/it.sh`), never against the real disk. They also produce the committed send-stream fixtures.

## Log

### 2026-10-15: code complete, waiting for the root run

Built: six crates, 46 tests (all pass), zero clippy warnings at `pedantic`.

- **Parser validated on real kernel streams.** Against both P1 streams, every CRC verifies (so the CRC convention is right: raw CRC-32C, seed 0, no final inversion), and the command counts match `btrfs receive --dump` exactly. No orphan name survives into a change set.
- **Change sets are much smaller than raw paths.** For the 16-package install, 176,981 commands became 34,854 changes (P1's naive path count was 59,773); the one-package install became 38. Two rules do this:
  - directories whose only change is their times, and something inside them changed, are dropped;
  - a file *replaced in place* (deleted, then created at the same path, which is how rpm and editors save) is reported as a **modify**, not a delete plus a create. This was found while writing the integration test.
- **rpmdb reader matches `rpm -qa`** exactly for all 2,154 installed packages (2.35 s including a 134 MB copy to tmpfs; inside the daemon the copy is a reflink). rpm's own `rpmvercmp` test vectors pass.
- **Package sets are reused.** When the send stream shows no change under `/usr/lib/sysimage/rpm/`, the package set is linked to the previous checkpoint's instead of being read again.
- **The ledger's hash chain stores each record next to its hash**, so `verify` detects both an edited chain and edited tables. Unit tests tamper with each and check that `verify` catches it.
- **The dnf hook's stdout is a command channel.** In the actions plugin's plain mode, every stdout line is interpreted: `stop=<msg>` aborts the transaction, and unknown lines are logged as errors. The hook therefore prints only `log.INFO=`/`log.WARNING=` lines with control characters removed, and human-facing warnings go to stderr.
- **Namespace guard, changed.** Comparing `/proc/self/ns/mnt` with PID 1's needs ptrace access to PID 1, which the hardened unit drops. The daemon now checks what actually matters: whether a mount at `/run/osreflog/top` would propagate. In the host namespace `/run` is `shared:14` here, so it refuses; under `PrivateMounts=yes` or `unshare --propagation private` the tag is absent, so it proceeds.

The remaining exit check needs root, in one run: `sudo scripts/p2/verify.sh`. It runs the loopback test, installs, makes a real transaction (`dnf remove figlet`), and measures diff latency and idle cost.
