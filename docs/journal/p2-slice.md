# P2: Vertical slice, state witness only

- Status: **closed**
- Started: 2026-10-15
- Closed: 2026-10-15
- Roadmap: [P2](../roadmap.md#p2-vertical-slice-state-witness-only-closed-2026-10-15)
- Exit: after a real package transaction, `osreflog diff @{1}..@{0}` shows the package changes and changed files in < 2 s, and the daemon idles at < 0.3 % CPU. **Met:** after a real `dnf remove figlet`, the diff showed the package removal, its dnf transaction and the changed files in **6 ms** (cached); over 60 idle seconds the daemon used **0.0 %** CPU with a 26 MB RSS.

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
- **The daemon never unshares by itself.** It refuses to mount the top level anywhere a mount could propagate to the host (see the log below for how that check ended up).
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

### 2026-10-15: verification on the real system (`sudo scripts/p2/verify.sh`)

**First run: the daemon refused to start under systemd.** Its new propagation check treated any `shared:` tag as dangerous. Measured with a transient unit (`systemd-run --user -p PrivateMounts=yes`): inside `PrivateMounts=yes`, `/run` is `shared:936 master:14`, a *new* peer group that is a slave of the host's group 14. Mounts there reach only the namespace's own copies. The rule is now "refuse if `shared` without `master`", plus the PID 1 namespace comparison when that link is readable, with a unit test on the measured tags. The script stopped before installing the dnf hook (`set -e`), so no package transaction ran without a daemon.

Second run, everything passed:

| Check | Result |
| ----- | ------ |
| Loopback integration test | 2/2 (engine end to end; fixture written) |
| `osreflog doctor` after install | 5/5 ok |
| Real transaction | `dnf remove figlet`: 8.5 s including dnf's own metadata load |
| Hook cost | pre-transaction checkpoint **742 ms**, post-transaction **135 ms**, boot checkpoint 129 ms (journal fields) |
| Interval computation (background) | boot → pre-dnf 2.4 s; pre → post 5.6 s (includes reading the 134 MB rpmdb from the post snapshot) |
| `osreflog diff @{1}..@{0}` | **6 ms** (served from the ledger), also with `--all --json` |
| `osreflog diff` (live, `@{0}..@{now}`) | 1.7 s: temporary snapshot, send, parse, delete |
| Idle CPU over 60 s | **0.0 %** |
| Memory | 26 MB RSS idle, 27 MB peak in that window; 130 MB peak over the whole run (rpmdb parsing) |
| Ledger | chain verified |
| SELinux | 0 denials from osreflog (42 today, all from `chronyc`) |
| Reading as an unprivileged user | works through polkit (`allow_active=yes`) |

What the real diff showed for `dnf remove figlet`:

```
from c000002 (pre-dnf, 2026-10-15 10:04:17)
to   c000003 (post-dnf, 2026-10-15 10:04:22)
     1 config · 1 state · 77 package · 32 noise · seen by: state · 733 commands, 94 KiB stream, 5638 ms, cached
     dnf #27 dnf remove -y figlet (uid 1000, ok)

packages (1):
  remove    figlet.x86_64  2.2.5-39.fc44

files (2):
  ~ /etc/ld.so.cache  [config file content,mode,owner,xattr,times]
  ~ /var/lib/rsyslog/imjournal.state  [state file content,mode,owner,xattr,times]

109 package-owned and noise paths hidden (--all to show)
```

`/etc/ld.so.cache` is the package's `ldconfig` scriptlet at work, and dnf recorded uid 1000 (the user behind `sudo`). osreflog also recorded **its own installation**: between the boot checkpoint and the first pre-dnf checkpoint, `osreflog.actions` appears as created.

Bugs found by the real run, fixed and covered by tests:

1. **A renamed directory's parent showed as modified.** The kernel sends `utimes` for a renamed directory; the times-only parent rule only counted children with non-time flags. A rename now counts as a change inside the parent.
2. **A dnf transaction showed up in the next interval too.** dnf records whole seconds, and the post-dnf checkpoint was taken in the same second the transaction ended. Transactions are now matched by history: those in the newer snapshot's history that the older snapshot's history doesn't already show as finished. The time window is only the fallback for pruned snapshots.
3. **Times-only changes were noise.** `relatime` access-time updates (executed binaries, read configs) appeared as modifications. Files whose only change is their times are now hidden by default (`--all` shows them).
4. **Journal lines said only `checkpoint`.** tracing-journald puts fields in `F_*` journal fields that `journalctl` doesn't print by default; messages now carry the values in their text.
5. **Reinstalling didn't restart the daemon.** `install-dev.sh` now restarts it.

Also seen: the live diff right after `sudo sh -c ...` showed systemd-logind writing resource drop-ins under `/etc/systemd/system.control/user-0.slice.d/` for root's session. That's a real change to `/etc` that nothing else would show.

## What changed in the design

- [architecture.md](../architecture.md): the hook's stdout protocol; transactions matched by history; times-only changes hidden by default; the propagation guard.
- [open-questions.md](../open-questions.md): Q12 resolved (`user_version` migrations, the chain hashes records rather than rows); Q3 narrowed (changed bytes are recorded per interval, squota still unevaluated); Q4 unchanged (the GNOME Software offline-update path is still untested).

## Left for later phases

- Space: per-checkpoint exclusive size (Q3). Intervals record changed bytes; nothing reports them yet.
- `/etc/ld.so.cache` shows `mode,owner,xattr` because it was replaced in place: a no-data stream can't tell whether the new inode's metadata differs from the old one. P4's semantic differs can compare against the older snapshot.
- Diffing two non-consecutive checkpoints whose snapshots were pruned isn't supported yet (it needs merging stored intervals).
- The interval with a package change spends most of its 5.6 s reading the 134 MB rpmdb. Reading only changed headers, or caching by rpmdb checksum, is an optimization for later.
