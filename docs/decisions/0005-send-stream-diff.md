# 0005: btrfs send stream (no-data) as the structural diff

- Status: Accepted
- Date: 2026-10-14

## Context
Comparing two snapshots of a ~50 GiB root by walking both trees is slow. `btrfs subvolume find-new` lists files with new extents, but misses deletions, renames and metadata-only changes.

## Decision
Run `BTRFS_IOC_SEND` with `BTRFS_SEND_FLAG_NO_FILE_DATA` and a parent snapshot, and parse the resulting command stream (protocol v2) in our own `btrfs` crate. Each command (`mkfile`, `mkdir`, `rename`, `link`, `unlink`, `rmdir`, `update_extent`, `truncate`, `chmod`, `chown`, `utimes`, `set_xattr`, `remove_xattr`, `fileattr`) maps to a change op.

## Alternatives considered
- **Shell out to `btrfs send | btrfs receive --dump`:** good for P1 exploration; text parsing is fragile and slower.
- **Tree walk with mtime/size/hash:** slow, and a misleading mtime causes false negatives.
- **find-new:** incomplete (see above).

## Consequences
- Both snapshots must be read-only (they are).
- Requires `CAP_SYS_ADMIN`, which is already needed.
- The stream gives paths at the end of the interval; renames inside the interval are resolved by the parser's path tracking.
