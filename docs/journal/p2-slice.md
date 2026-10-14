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
