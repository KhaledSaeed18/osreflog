# 0011: The write gate's matrix runs in a contained scratch root, not a VM

- Status: Accepted
- Date: 2026-10-15

## Context
The write gate (roadmap) says the undo engine must pass its test matrix "in a VM" before any code writes to the live `/`. The point is containment: if the engine resolved a path wrongly, the damage should land somewhere disposable. osreflog's VM path (boot a snapshot's own kernel with the snapshot as root over virtiofs; no separate image to download) is P6's `vm` runner, which doesn't exist yet; building it first would have put P6 ahead of P5. (An earlier version of this ADR said a VM needed a ~500 MB image download. It doesn't: P1 already booted a snapshot this way.)

## Decision
For the gate, the matrix runs as root against a **writable btrfs snapshot of the live root** (the scratch root), inside a `systemd-run` scope with `ProtectSystem=strict`, `ProtectHome=read-only` and only the scratch root writable. The engine takes a pinned root directory fd and does every operation relative to it with `openat2(RESOLVE_BENEATH | RESOLVE_NO_SYMLINKS | RESOLVE_NO_MAGICLINKS | RESOLVE_NO_XDEV)`, so the scratch root runs exactly the code that runs on `/`. The matrix runs again in a VM once P6 exists.

## Alternatives considered
- **A VM now:** building P6's `vm` runner ahead of P5, and a 512 MiB guest beside the desktop, for containment the mount sandbox already gives (for file writes, which is all the engine does).
- **Unprivileged tests in a tempdir only:** they can't test ownership, SELinux labels, `security.*` xattrs or bind mounts, which is where root-only bugs hide. They exist as well, for the logic.

## Consequences
- Real files, labels and ownership, at no download cost; the snapshot costs nothing on btrfs.
- What a scratch root can't cover: pid 1 and dconf (undo delegates those anyway), and a kernel without `openat2` (5.6+; Fedora has it).
