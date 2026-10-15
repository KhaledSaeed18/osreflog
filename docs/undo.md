# Undo

`osreflog undo <#cause>` reverts one cause on the live system, path by path. It's the only feature that writes to `/` (bisect writes only to clones), so it ships after the write gate ([roadmap](./roadmap.md)).

## Rules

1. **Dry run first.** `undo` without `--apply` prints what it would do to each path, with a diff for text. `--apply` requires polkit admin authentication every time, and applies exactly the plan that was shown (the daemon compares the plan's digest).
2. **3-way check per object.** With `B` = the ledger's before, `A` = the ledger's after, and `C` = current:
   - `C == A` → restore `B`.
   - `C == B` → already undone, nothing to do.
   - otherwise → **conflict**. Refuse that object and show what's there and what the cause left. (A key-level merge for keyfile/INI formats is a possible later addition.)
3. **Checkpoint before.** A `pre-undo` checkpoint is taken, so the undo itself can be undone.
4. **Recorded.** The undo is an `undo` cause linking the cause it reverts.

## Per object type

| Object | How it's restored |
| ------ | ----------------- |
| Changed file | write `B` to a temp file in the same directory, set owner, mode, xattrs and SELinux label (from the snapshot before the cause), `fsync`, then `renameat2(RENAME_EXCHANGE)`; the swapped-out file must still be `A`, else it's swapped back and the path is a conflict; the replaced version goes to the attic |
| File without a recorded blob | the same, with `B` read from the newest snapshot before the cause (plans carry content up to 8 MiB; a reflink from the snapshot stays possible for larger files) |
| Deleted file | recreated the same way, with `RENAME_NOREPLACE` so it can't overwrite something that appeared meanwhile |
| Created file or directory | moved to `/var/lib/osreflog/attic/<undo>/` (a directory only if empty), never unlinked |
| Symlink | a new link under a temp name, owner set, then the same swap |
| Unit enablement | the `.wants/` or mask link is restored like any symlink, then systemd is reloaded over D-Bus (what `systemctl enable` itself does) |
| dconf key | `osreflog undo --user #id`, as the user: `dconf write` with the old value, or `dconf reset`, if the key still has the value the cause left |
| Package cause | **delegate**: shows `dnf5 history undo <transaction>`; `--apply` runs it, and dnf asks for confirmation. osreflog never rewrites `/usr` by hand. |
| `/boot` config | not yet: `/boot` is ext4 on Fedora, outside the btrfs witness, so nothing records it. With metal bisect (P7). |

Every directory on the way is opened with `openat2` and `RESOLVE_BENEATH | RESOLVE_NO_SYMLINKS | RESOLVE_NO_MAGICLINKS | RESOLVE_NO_XDEV` relative to the root, and every other call is a `*at` call on that directory's fd. The writes happen in `osreflogd --undo-worker`, which keeps only `CAP_CHOWN`, `CAP_FOWNER` and `CAP_DAC_OVERRIDE` and can only reach the scope directories and the attic (Landlock). The live witness records the worker's writes as an `undo of #N` cause, so an undo can be undone.

## What undo will never do

- Restore a secret (the denylist, or a root-only file), a file capability, or a set-user-ID/set-group-ID bit.
- Write outside the paths recorded for that cause, or outside `/etc`, `/opt` and `/usr/local`.
- Overwrite a path that changed after the cause.
- Roll back a whole subvolume. Whole-system rollback is Snapper's job, and a btrfs rollback of `/` would also roll back logs, VM images and Docker data, which is rarely what the user wants.
