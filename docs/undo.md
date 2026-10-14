# Undo

`osreflog undo <#cause | change-id | path@ref>` reverts one cause or one change on the live system. It's the only feature that writes to `/` (bisect writes only to clones), so it ships after the write gate ([roadmap](./roadmap.md)).

## Rules

1. **Dry run first.** `undo` without `--apply` prints the exact semantic diff it would apply. `--apply` requires polkit admin.
2. **3-way check per object.** With `B` = the ledger's before, `A` = the ledger's after, and `C` = current:
   - `C == A` → restore `B`.
   - `C == B` → already undone, nothing to do.
   - otherwise → **conflict**. Refuse that object and show all three. For keyfile/INI formats, offer a key-level merge (undo only the keys the cause changed, if those keys are still at `A`).
3. **Checkpoint before.** A `pre-undo` checkpoint is taken, so the undo itself can be undone.
4. **Recorded.** The undo is an `undo` cause linking the cause it reverts.

## Per object type

| Object | How it's restored |
| ------ | ----------------- |
| Text config with a blob | write `B` to a temp file in the same dir, set owner/mode/xattrs/SELinux label from the ledger, `fsync`, `renameat2(RENAME_EXCHANGE)` then unlink, all via `openat2(RESOLVE_BENEATH \| RESOLVE_NO_SYMLINKS)` |
| File without a blob | reflink (`FICLONE`) from the newest checkpoint that has version `B`, then the same metadata steps (verified in P1: FICLONE works across mount points of the same filesystem) |
| Deleted file | same as above, from the checkpoint before deletion |
| Created file | move to `/var/lib/osreflog/attic/` (recoverable), never unlink directly |
| Unit enablement | `systemctl enable/disable` via the systemd D-Bus API (pid 1 owns those symlinks) |
| dconf key | through the user agent → `dconf write` with the old GVariant, or `reset` if it was unset |
| Package cause | **delegate**: `dnf5 history undo <id>` (shown, then run with the user's confirmation). osreflog never rewrites `/usr` by hand. |
| `/boot` config | only BLS entries and `grubenv`, with `grub2-editenv` for the latter; never kernel images |

## What undo will never do

- Restore a file on the secrets denylist (it has no content for those anyway).
- Write outside the paths recorded for that cause.
- Roll back a whole subvolume. Whole-system rollback is Snapper's job, and a btrfs rollback of `/` would also roll back logs, VM images and Docker data, which is rarely what the user wants.
