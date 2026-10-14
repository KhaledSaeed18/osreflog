# Architecture Decision Records

One file per decision: context, decision, alternatives, consequences. Superseded ADRs stay, with their status updated.

| # | Decision | Status |
| - | -------- | ------ |
| [0001](./0001-rust-drainscope-stack.md) | Rust, with drainscope's stack (aya, zbus, rusqlite) | Accepted |
| [0002](./0002-two-witnesses.md) | Two witnesses: live capture + snapshot diff | Accepted |
| [0003](./0003-fanotify-plus-ebpf-lineage.md) | fanotify for events, eBPF for process lineage | Proposed (P1 measures) |
| [0004](./0004-snapshot-layout.md) | Snapshots as top-level siblings, ledger in its own subvolume | Proposed (P1 verifies) |
| [0005](./0005-send-stream-diff.md) | btrfs send stream (no-data) as the structural diff | Accepted |
| [0006](./0006-cause-level-bisect-three-tiers.md) | Cause-level bisect with nspawn / vm / metal tiers | Accepted |
| [0007](./0007-undo-delegates-packages.md) | Undo restores files itself, delegates packages to dnf5 | Accepted |
| [0008](./0008-dbus-polkit.md) | D-Bus system service + polkit | Accepted |
| [0009](./0009-name-osreflog.md) | Name: osreflog | Accepted |
