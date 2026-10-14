# 0004: Snapshots as top-level siblings, ledger in its own subvolume

- Status: Proposed. P1 verifies the private mount and cross-mount reflink (Q2, Q8)
- Date: 2026-10-14

## Context
Fedora mounts `subvol=root` at `/` and `subvol=home` at `/home`; the top level (subvolid 5) isn't mounted. Snapper's convention is a nested `/.snapshots` subvolume, which makes snapshots visible in `/` (indexers, backups, `du`). The ledger must survive any rollback of `/`.

## Decision
- Create `osreflog/ledger`, `osreflog/snapshots/`, and `osreflog/probes/` at the **top level**, as siblings of `root` and `home`.
- `osreflogd` mounts the top level only inside its own private mount namespace.
- The ledger subvolume is mounted at `/var/lib/osreflog` by a mount unit, so every probe clone (which carries that unit) sees the same ledger.

## Alternatives considered
- **Nested `/.snapshots` (Snapper layout):** simple, and compatible with snapper tools, but visible everywhere and tied to `/`'s lifetime.
- **Reuse Snapper as the backend:** less code, but its config model and timeline don't know about causes, and two snapshot managers would conflict.

## Consequences
- Coexisting with Snapper or Timeshift on the same machine is unsupported; `doctor` warns.
- Probes boot with the real ledger mounted, so a probe must never be able to corrupt it: probe-side access goes only through a narrow verdict file, not direct SQLite access.
