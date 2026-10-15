# 0011: The write gate's matrix runs in a contained scratch root, not a VM

- Status: Accepted
- Date: 2026-10-15

## Context
The write gate (roadmap) says the undo engine must pass its test matrix "in a VM" before any code writes to the live `/`. The point is containment: if the engine resolved a path wrongly, the damage should land somewhere disposable. On this machine a VM means downloading a Fedora image (~500 MB at ~100 KiB/s) and running it in 7 GB of RAM. P6 (bisect) builds VM infrastructure anyway.

## Decision
For the gate, the matrix runs as root against a **writable btrfs snapshot of the live root** (the scratch root), inside a `systemd-run` scope with `ProtectSystem=strict`, `ProtectHome=read-only` and only the scratch root writable. The engine takes a pinned root directory fd and does every operation relative to it with `openat2(RESOLVE_BENEATH | RESOLVE_NO_SYMLINKS | RESOLVE_NO_MAGICLINKS | RESOLVE_NO_XDEV)`, so the scratch root runs exactly the code that runs on `/`. The matrix runs again in a VM once P6 exists.

## Alternatives considered
- **A VM now:** the download and the RAM, for containment the mount sandbox already gives (for file writes, which is all the engine does).
- **Unprivileged tests in a tempdir only:** they can't test ownership, SELinux labels, `security.*` xattrs or bind mounts, which is where root-only bugs hide. They exist as well, for the logic.

## Consequences
- Real files, labels and ownership, at no download cost; the snapshot costs nothing on btrfs.
- What a scratch root can't cover: pid 1 and dconf (undo delegates those anyway), and a kernel without `openat2` (5.6+; Fedora has it).
