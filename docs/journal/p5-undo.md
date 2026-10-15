# P5: The write gate and undo

- Status: **in progress**
- Started: 2026-10-15
- Roadmap: [WRITE GATE and P5](../roadmap.md#write-gate)
- Exit: the write gate's checklist passes; the undo matrix passes on the real machine for the workload's scenarios; no conflict is ever silently overwritten.

## Plan

| Slice | What |
| ----- | ---- |
| **P5a** engine | `crates/undo`: applies a plan of steps (path, expected current state A, state to restore B) against a pinned root directory fd. Every open is `openat2` with `RESOLVE_BENEATH \| RESOLVE_NO_SYMLINKS \| RESOLVE_NO_MAGICLINKS \| RESOLVE_NO_XDEV`, and every other call is `*at` on a parent fd opened that way. No unsafe code. |
| **P5b** planner + worker | osreflogd turns a cause into a plan (B from blobs or the snapshot before the cause, metadata from snapshots); `osreflogd --undo-worker` applies it with capabilities cut to `CAP_CHOWN CAP_FOWNER CAP_DAC_OVERRIDE` and a Landlock domain that can write only beneath the scope directories and the attic |
| **P5c** interface | D-Bus `UndoPlan` (read) and `Undo` (new polkit action `…undo`, `auth_admin`, never kept), pre-undo checkpoint verified before every apply; `osreflog undo #id` is a dry run, `--apply` applies exactly the plan that was shown (by digest); the live witness records the undo as an `undo` cause, so an undo can itself be undone |
| **P5d** delegations | dnf causes: `dnf5 history undo <transaction>` shown, run only with `--apply` (dnf asks again); dconf: `osreflog user undo #id` runs as the user (`dconf write` / `dconf reset`) with the same 3-way rule; unit enablement: the symlinks are restored like any symlink, then systemd is reloaded |
| **P5e** gate tests | the matrix (below) and the A1/A3 tests, as root, contained |
| **P5f** live | an undo workload on the real machine: make changes with real tools, undo each, check the files, the conflicts and the undo of an undo |

### The engine's rules

- **One step per path.** A step is: path, `expect` (A, what the cause left: absent, a file with a BLAKE3 hash, a symlink target, a directory), `restore` (B: absent, or a file with content and metadata, a symlink, a directory). A rename X→Y becomes two steps (Y back to absent, X back to its content).
- **3-way, per step, at apply time.** Current C is read through the pinned parent fd. `C == B`: already undone, skipped. `C != A`: **conflict**, skipped and reported with all three. Otherwise apply.
- **Race-checked swaps.** New content goes into a temp file in the same directory (owner, mode, xattrs set, `fsync`), then `renameat2(RENAME_EXCHANGE)` swaps it with the current file. The file that was swapped out is hashed again: if it no longer matches A (someone wrote between the check and the swap), the swap is reversed and the step is a conflict. A restore over a missing path uses `RENAME_NOREPLACE`, so it can't overwrite something created in the meantime.
- **Nothing is deleted.** What undo removes (a created file, the replaced version) moves to `/var/lib/osreflog/attic/<undo>/`, on the same filesystem.
- **Scope.** Only config paths (`/etc`, `/opt`, `/usr/local`) recorded for that cause; never a secret (by path, or a root `0600` file); never a file capability (`security.capability` would grant privilege); a created directory is removed only if empty.

### Not in P5

- **BLS entries and `grubenv`:** `/boot` is ext4 on this machine, outside the btrfs witness, so nothing records how it changed. Boot config comes with metal bisect (P7), which has to manage boot entries anyway.
- **Large files:** B travels inside the plan, up to 8 MiB per file. Config files are far smaller; reflinking from a snapshot (R8) stays possible if it's ever needed.

## The write gate, amended

The roadmap asks for the undo matrix to pass **in a VM**. That needs a Fedora image (~500 MB at ~100 KiB/s) and a VM in 7 GB of RAM. P6 builds that anyway, and the matrix will run there again. For the gate, [ADR 0011](../decisions/0011-gate-in-a-contained-scratch-root.md) replaces the VM with a **contained scratch root**:

- a writable btrfs snapshot of the live root, so real files, real SELinux labels, real ownership;
- the test runs as root under `systemd-run` with `ProtectSystem=strict`, `ProtectHome=read-only` and only the scratch root writable, so a resolution bug that escaped the root dirfd would hit a read-only filesystem, not the machine.

The engine only ever writes relative to the root fd it's given, so the scratch root exercises exactly the code that runs on `/`.

### The matrix

Restores: modify, create (to the attic), delete (recreated with owner, mode, xattrs and SELinux label), rename, symlink retarget, symlink created and deleted, directory created (empty / not empty) and deleted. 3-way: already undone, conflict (content changed since), conflict during the swap (race), restore over a path that reappeared. Attacks on resolution: a parent directory swapped for a symlink (before and during the run), `..` and absolute paths in a plan, a magic link (`/proc/self/fd/…`) as a component, a bind mount inside the root, a FIFO or device where a file was. Refusals: out-of-scope path, secret, file capability.
