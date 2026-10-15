# P5: The write gate and undo

- Status: **closed**
- Started: 2026-10-15
- Closed: 2026-10-15
- Roadmap: [WRITE GATE and P5](../roadmap.md#write-gate)
- Exit: the write gate's checklist passes; the undo matrix passes on the real machine for the workload's scenarios; no conflict is ever silently overwritten. **Met:** gate 16/16 as root under the daemon's own sandbox (0 escapes in a symlink-swap race); live undo workload 15/15 (a conflict left alone, an undo undone); flood test bounded.

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

## Log

### 2026-10-15: the engine, the worker, the interface

- **`crates/undo`**, no unsafe code: plan types and checks (`plan.rs`), the operations relative to a pinned root fd (`apply.rs`), and what the CLI shows (`preview.rs`). 16 unprivileged tests, including one that stages a write between the 3-way check and the swap through a test-only hook: the swap is reversed and the step is a conflict.
- **Planner** (`osreflogd/src/undo.rs`): per recorded change, A from the record (blob hash, symlink target, directory, absent) or, when no content was recorded, from the current file if nothing could have changed it since (no later record, no gap); B from the newest record since the path last went away, else the newest snapshot before the cause; owner, mode, xattrs and label from that snapshot. A move becomes two steps. Package-owned paths, state, the attic and secrets are listed as left alone, with the reason.
- **Worker** (`osreflogd --undo-worker`): opens the root and the directories it may write, keeps only `CAP_CHOWN`, `CAP_FOWNER`, `CAP_DAC_OVERRIDE`, sets no-new-privileges and limits, and enters a Landlock domain that allows the scope directories and the attic only. `--undo-sandbox-selftest` proves it: writing in `etc` works; writing `var/tmp`, reading `usr` and executing anything fail.
- **Interface:** `UndoPlan(id)` (read action) and `Undo(id, digest)` (new action `…undo`, `auth_admin`, never kept). `Undo` recomputes the plan and refuses if its digest isn't the one shown, takes a `pre-undo` checkpoint, verifies the ledger and the snapshot, plans again (it must still match), then runs the worker, reloads systemd if unit files or links changed, and returns the cause the live witness recorded for the undo. `osreflog undo #id` is the dry run with diffs; `--apply` applies; `--user` undoes a settings change with `dconf` as the user (ledger v5 keeps raw values for that, never a secret key's).
- **The undo is a cause.** The daemon registers the worker's pid; the cause builder labels its writes `undo of #N`. Undoing that cause puts the change back.
- **A3:** at most 256 causes open at once (more merge into one `flood` cause per uid) and 100,000 changes held across them (past that, changes are counted as `dropped` in `osreflog status`; the state witness still records them).

### 2026-10-15: what the gate and the real machine found

Five runs to green. Each failure was caught before a wrong write, and each fix is in:

1. **`pam_namespace`:** the sudo session had a private `/var/tmp`, so `systemd-run` couldn't see the scratch area there. It moved to `/var/cache/osreflog-gate`.
2. **The tests, not the engine:** files restored with `Meta::default()` (root, mode 0000) are secrets by osreflog's own rule and were refused before any path was resolved, so three resolution tests tested nothing. They now restore ordinary `0644` files. The first race run was 1.98 M refusals and 0 restores; the passing one exercised both.
3. **`RestrictSUIDSGID=` blocks `openat2`.** The gate passed, then every undo on the installed daemon failed with `ENOSYS`: systemd implements that setting with a seccomp filter that fails `openat2` outright, because it can't inspect the mode inside `struct open_how`. The engine has no fallback to `openat` by design, so it refused everything (nothing was written). The setting is gone from the unit; undo refuses set-user-ID and set-group-ID bits itself, tested as root. **The gate now runs under every hardening directive of the daemon's unit**, read from the unit file, which immediately showed the second gap:
4. **`CAP_CHOWN`** was missing from the unit's bounding set, so the worker couldn't have kept it. Added.
5. **Delegation timing:** a dnf cause undone seconds after the transaction found no transaction, because the interval that records it is computed in the background. The preview now computes pending intervals first.

Also found by reading the kernel: `renameat2(RENAME_EXCHANGE)` emits **two** move events, one each way. The live witness took the second for a real move and would have recorded the undo itself wrongly (so an undo couldn't be undone). A rename that mirrors the previous one from the same process is now the second half of an exchange and is ignored.

### Results

**Write gate** (`scripts/p5/gate.sh`, root, SELinux enforcing, kernel 7.2.9, inside `osreflogd.service`'s 20 hardening settings plus a writable scratch area): the engine's 10 tests and the matrix's 6 pass.

| Matrix test | Result |
| ----------- | ------ |
| owner, mode, `user.*` xattr and SELinux label (`httpd_config_t`, `net_conf_t`) restored by the real worker | restored exactly; replaced version in the attic |
| the worker's sandbox | write in `etc` ok; write `var/tmp`, read `usr`, exec: refused |
| a bind mount inside the root | `EXDEV`: not crossed |
| `/proc` mounted inside the root, and its magic links | refused both ways |
| a parent directory swapped with a symlink to outside, 3 s | 3,541 runs: 375 restored inside, 3,166 refused, 19,771 swaps, **0 escaped** |
| `/etc/shadow`, a file capability, a set-user-ID file | refused, even as root |

**Live undo workload** (`scripts/p5/undo-workload.sh`, the installed daemon): **15/15**.

| Change | Undo |
| ------ | ---- |
| `sed -i` | restored; recorded as an `undo` cause |
| an editor's save (temp file renamed over) | restored |
| a created file | moved to the attic |
| a deleted file (`0640 root:wheel`, `etc_t`) | back with the same owner, mode and label |
| `ln -sfn` | symlink points back |
| `systemctl enable` (pid 1 writes the link) | link removed, systemd reloaded, unit disabled |
| a file changed again after the cause | **conflict, left alone** (both hashes shown) |
| the undo from the first row | undone: the `sed` change is back |
| a `*.key` file | refused as a secret |
| `dnf install figlet` | delegated: `dnf5 history undo 46`, which removed it |

The ledger verified all 711 entries afterwards.

**Flood** (`scripts/p5/flood.sh`, as an ordinary user): 100,000 processes and 300,000 file create/delete pairs. osreflogd went from 55 to 104 MB (limit 768 MB), the lineage table stopped at its 65,536 cap, the fanotify queue never overflowed, and `osreflog status` answered in 19 ms afterwards. An ordinary user can't write in-scope config at all, so the cause caps are covered by a unit test rather than this run.

**User scope:** `osreflog undo --user` against real dconf: a restore, already undone, a conflict, and a secret key refused (its value is never kept).

## The write gate

- [x] The undo matrix passes, in a contained scratch root under the daemon's own sandbox ([ADR 0011](../decisions/0011-gate-in-a-contained-scratch-root.md); again in a VM in P6)
- [x] `openat2` resolution tests: symlink swap race, `..`, magic links, bind mounts
- [x] A verified `pre-undo` checkpoint before every apply
- [x] A1: only paths the cause recorded, in scope, through a pinned fd, 3-way, `auth_admin` every time; A3: bounded causes, changes and lineage, tested
- [x] Dry run is the default

## What changed in the design

- [ADR 0011](../decisions/0011-gate-in-a-contained-scratch-root.md): the gate's matrix in a contained scratch root instead of a VM.
- [undo.md](../undo.md): how restores work as built (exchange-and-verify, the attic, plans by digest, user scope, unit enablement by link plus reload).
- The daemon's unit: no `RestrictSUIDSGID=` (it blocks `openat2`), `CAP_CHOWN` added.
- Boot config (BLS, `grubenv`) moves to P7: `/boot` is ext4 here and nothing records it.
