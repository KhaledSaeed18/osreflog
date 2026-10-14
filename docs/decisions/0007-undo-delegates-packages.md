# 0007: Undo restores files itself, delegates packages to dnf5

- Status: Accepted
- Date: 2026-10-14

## Context
Reverting a package transaction by restoring files would leave the RPM database, scriptlet side effects, and dependencies inconsistent.

## Decision
osreflog restores config files, unit enablement, dconf keys and boot config itself (3-way, `openat2`, metadata-preserving). For `dnf` causes it shows and runs `dnf5 history undo <id>` instead.

## Alternatives considered
- **Whole-subvolume rollback:** also rolls back logs, VM images, containers and later unrelated changes. That's the user's choice through Snapper, not ours.
- **File-level package revert:** breaks the rpmdb.

## Consequences
- Package undo depends on old packages still being downloadable or cached. dnf reports that; osreflog surfaces it.
