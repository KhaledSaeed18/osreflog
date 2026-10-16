# Open questions

Each question is answered by a measurement or decision in the phase noted, then moved to "Resolved" with a link to the ADR or journal entry where it was recorded.

## Open

| # | Question | Candidates / notes | Decide in |
| - | -------- | ------------------ | --------- |
| Q3 | Space accounting | P1: quotas are off on this filesystem, and `btrfs fi du` took 23 s for two snapshots, too slow. P2 records changed bytes per interval from the send stream, but nothing reports them yet. Still to do: report them, and evaluate simple quotas (squota) on a loopback btrfs before enabling anything on the real disk | P5 (before retention gets aggressive) |
| Q5 | Daemon start ordering | How early can osreflogd start (`DefaultDependencies=no`)? Needs `/var/lib/osreflog` mounted and BPF available. Whatever happens before that is caught by the boot-time diff | P3 |
| Q9 | Replay soundness | When a cause has no blob for a path, how often does replay from `cB` differ from the true state? Instrumented in P6 (each probe and result lists its approximate paths); the seeded suite had none, so real-world data is still needed | P7+ |

## Resolved

| # | Question | Answer | Where |
| - | -------- | ------ | ----- |
| R1 | Project name | **osreflog** | [ADR 0009](./decisions/0009-name-osreflog.md), [naming/](./naming/) |
| R2 | Language and core libraries | Rust; aya, zbus, rusqlite (drainscope's stack) | [ADR 0001](./decisions/0001-rust-drainscope-stack.md) |
| R3 | License | GPL-3.0-or-later, same as drainscope (decided 2026-10-14) | [LICENSE](../LICENSE) |
| R4 | Flatpak in v1 | Out of scope for v1: Flatpak has its own history and rollback (`flatpak history`, `flatpak update --commit`). Writes under `/var/lib/flatpak` are recognized and summarised as one opaque `flatpak` cause, never diffed. Revisit after P6 | this table (2026-10-14) |
| R5 | Q1: live-capture scope strategy | One `FAN_MARK_FILESYSTEM` on the btrfs superblock, scope filtering in userspace, resolution by file handle. 9.1 events/s system-wide in P1 | [ADR 0003](./decisions/0003-fanotify-plus-ebpf-lineage.md), [journal](./journal/p1-measure.md) |
| R6 | Q2: private top-level mount | `mount -o subvolid=5` in a private mount namespace; invisible in PID 1's namespace | [ADR 0004](./decisions/0004-snapshot-layout.md), [journal](./journal/p1-measure.md) |
| R7 | Q7: VM tier boot path | Host-only dracut image with virtiofs (37 MiB), `root=virtiofs:<tag>`, 512 MiB guest, 34 s to `running` | [bisect.md](./bisect.md), [journal](./journal/p1-measure.md) |
| R8 | Q8: cross-mount reflink | Works: FICLONE from the private top-level mount into `/` | [undo.md](./undo.md), [journal](./journal/p1-measure.md) |
| R9 | Q12: ledger schema evolution | `PRAGMA user_version` migrations; a newer schema is refused. The chain hashes JSON records, not SQLite rows, so layout changes don't break it | [ledger crate](../crates/ledger/src/lib.rs), [P2 journal](./journal/p2-slice.md) |
| R10 | Q6: attribution of D-Bus-delegated writes | Bus monitor (`BecomeMonitor`) for method calls; credit a daemon's cause to the caller whose call it received in the 10 s before. For pid 1, the latest `systemctl` with a writing verb from the lineage table, because systemctl as root uses pid 1's private socket, not the bus | [P4 journal](./journal/p4-semantics.md) |
| R11 | Q4: dnf5 hook in offline updates (GNOME Software) | The hook runs in `dnf5-offline-transaction.service`, but osreflogd wasn't up: its unit is wanted by `multi-user.target`, and an offline update boots `system-update.target` ("osreflog: no pre-transaction checkpoint: osreflogd is not running"). The update still reached the history through the next boot checkpoint's diff and the dnf history import ("dnf #52 dnf5daemon-server", 44 package changes). Fix: osreflogd is wanted by `system-update.target` too and ordered before the offline transaction; to be confirmed on the next offline update | [P8 journal](./journal/p8-app.md) |
