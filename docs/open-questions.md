# Open questions

Each question is answered by a measurement or decision in the phase noted, then moved to "Resolved" with a link to the ADR or journal entry where it was recorded.

## Open

| # | Question | Candidates / notes | Decide in |
| - | -------- | ------------------ | --------- |
| Q1 | Live-capture scope strategy | (a) recursive inode marks on scope trees (default in [capture.md](./capture.md)); (b) a filesystem mark with in-kernel filtering, if fanotify BPF filtering exists in 7.2; (c) BPF LSM path hooks as the primary source. Measure event rates with `fatrace` and the `/etc` mark count | P1 |
| Q2 | Private top-level mount | `PrivateMounts=yes` + mounting `subvolid=5` in the daemon's namespace vs a dedicated mount unit with restrictive perms. Must stay invisible to `/`, `locate`, backup tools and the user | P1 |
| Q3 | Space accounting | btrfs qgroups (exact, but a known performance cost), simple quotas (squota, kernel ≥ 6.7: cheaper, needs enabling on the fs), or an estimate from send-stream extents. Need "how much does checkpoint X cost" | P2 |
| Q4 | dnf5 hook coverage | Does the `pre_transaction` action block dnf until we return? Does GNOME Software (PackageKit + dnf5 backend) load libdnf5 plugins? What about offline updates (`system-update.target`)? Anything not hooked becomes an `offline` cause | P1 |
| Q5 | Daemon start ordering | How early can osreflogd start (`DefaultDependencies=no`)? Needs `/var/lib/osreflog` mounted and BPF available. Whatever happens before that is caught by the boot-time diff | P3 |
| Q6 | systemd enablement attribution | System bus monitor overhead and reliability for `EnableUnitFiles`; alternative: correlate pid 1's writes with recent `systemctl` execs from lineage | P4 |
| Q7 | VM tier boot path | dracut module and cmdline for virtiofs root with Fedora's modular `virtio_fs`; one cached initramfs per kernel version; boot time and RSS | P1 |
| Q8 | Cross-mount reflink | `FICLONE` from a snapshot under the private top-level mount into `/` (same filesystem, different mount points) | P1 |
| Q9 | Replay soundness | When a cause has no blob for a path, how often does replay from `cB` differ from the true state? Instrument in the seeded suite | P6 |
| Q10 | Flatpak | System Flatpaks live in an OSTree repo under `/var/lib/flatpak`. Attribute via `flatpak` lineage + ostree refs diff, or treat as out of scope for v1? | P4 |
| Q11 | License | Proposed: GPL-3.0-or-later, same as drainscope | P0 review |
| Q12 | Ledger schema evolution | Migrations with `user_version`; the hash chain must cover the logical record, not the SQLite row layout | P2 |

## Resolved

| # | Question | Answer | Where |
| - | -------- | ------ | ----- |
| R1 | Project name | **osreflog** | [ADR 0009](./decisions/0009-name-osreflog.md), [naming/](./naming/) |
| R2 | Language and core libraries | Rust; aya, zbus, rusqlite (drainscope's stack) | [ADR 0001](./decisions/0001-rust-drainscope-stack.md) |
