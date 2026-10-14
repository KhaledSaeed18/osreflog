# Open questions

Each question is answered by a measurement or decision in the phase noted, then moved to "Resolved" with a link to the ADR or journal entry where it was recorded.

## Open

| # | Question | Candidates / notes | Decide in |
| - | -------- | ------------------ | --------- |
| Q3 | Space accounting | P1: quotas are off on this filesystem, and `btrfs fi du` took 23 s for two snapshots, too slow. Start with send-stream estimates; evaluate simple quotas (squota) on a loopback btrfs before enabling anything on the real disk | P2 |
| Q4 | dnf5 hook coverage (remaining part) | P1 verified that `dnf` and `packagekitd` both block on the pre hook. Still open: GNOME Software's offline updates via dnf5daemon-server, applied at boot by `system-update.target`. Does the hook run there, and is osreflogd up yet? Anything not hooked becomes an `offline` cause | P2 |
| Q5 | Daemon start ordering | How early can osreflogd start (`DefaultDependencies=no`)? Needs `/var/lib/osreflog` mounted and BPF available. Whatever happens before that is caught by the boot-time diff | P3 |
| Q6 | systemd enablement attribution | System bus monitor overhead and reliability for `EnableUnitFiles`; alternative: correlate pid 1's writes with recent `systemctl` execs from lineage | P4 |
| Q9 | Replay soundness | When a cause has no blob for a path, how often does replay from `cB` differ from the true state? Instrument in the seeded suite | P6 |
| Q12 | Ledger schema evolution | Migrations with `user_version`; the hash chain must cover the logical record, not the SQLite row layout | P2 |

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
