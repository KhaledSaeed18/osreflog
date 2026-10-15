# Architecture

## Components

| Component | Runs as | Language | Responsibility |
| --------- | ------- | -------- | -------------- |
| `osreflogd` | root, systemd service, hardened | Rust | Live capture, cause assembly, checkpoints, diffing, ledger, bisect orchestration, undo |
| `osreflog` | user | Rust | CLI: `log`, `show`, `diff`, `why`, `blame`, `bisect`, `undo`, `checkpoint`, `doctor` |
| `osreflog-user` | user, systemd user service | Rust | Optional user scope: dconf (GSettings) writes, credited to the app or command that asked, in the user's own ledger ([ADR 0010](./decisions/0010-user-scope-own-ledger.md)) |
| summarize worker | `osreflogd --summarize-worker`, sandboxed, no capabilities | Rust | Turns untrusted file content into semantic summaries (rlimits, Landlock, seccomp allowlist) |
| eBPF programs | kernel | Rust (aya-ebpf) | Process lineage: fork/exec/exit tracepoints → ring buffer |
| dnf5 hook | runs inside dnf5 | actions file + CLI call | Synchronous pre/post-transaction checkpoints |
| GNOME app | user | Rust, GTK4/libadwaita | Timeline, cause details, bisect wizard (late phase) |

Same stack as drainscope where possible: **aya** (eBPF), **zbus** (D-Bus), **rusqlite** (ledger), so knowledge and patterns carry over.

## The data model

```
Checkpoint ──< Cause >── Change
    │            │          │
 btrfs ro     actor +     path/key + op + before/after blob ids
 snapshot     kind        + witness flags (live, state)
```

- **Change**: one mutation of one object. Objects are files (`/etc/...`), boot config files (`/boot/loader/entries/...`), packages (NEVRA), dconf keys, and unit enablement. Ops are `create`, `modify`, `delete`, `rename`, `attr`. Each change records which witnesses saw it.
- **Cause**: a group of changes with one actor and one reason. Kinds:

  | Kind | How it's recognized |
  | ---- | ------------------- |
  | `dnf` | dnf5 actions hook brackets the transaction; metadata from `transaction_history.sqlite` |
  | `rpm-scriptlet` | lineage descends from an rpm process inside a `dnf` cause; folded into that cause |
  | `unit` | actor's cgroup is a system unit (`tlp.service`, `fwupd.service`, `NetworkManager.service`) |
  | `command` | actor's lineage reaches a login session / terminal scope; records the top command (e.g. `sudo vim …`) |
  | `app-setting` | dconf `Change` call attributed to the sending app (user scope) |
  | `offline` | change seen only by the state witness: daemon down, offline update, live USB |
  | `undo` | osreflog's own undo, linking the cause it reverts |

  Causes are closed by a quiet period (default 2 s with no further changes from the same actor) or by an explicit bracket (dnf hook).
- **Checkpoint**: a read-only btrfs snapshot of the root subvolume plus the ledger position at that moment. Tagged with *why* it was taken: `pre-dnf`, `post-dnf`, `boot-ok`, `scheduled`, `manual`, `pre-undo`.

Refs use git's reflog syntax:

| Ref | Meaning |
| --- | ------- |
| `#2215` | cause 2215 |
| `c42` | checkpoint 42 |
| `@{now}` | live system |
| `@{1}`, `@{2}` | one, two checkpoints back |
| `@{boot-1}` | the last known-good boot before the current one |
| `@{yesterday}`, `@{2026-10-12 14:00}` | the state at that time (nearest checkpoint + replayed causes) |

## Lifecycle of a change

```
 process writes /etc/foo.conf
        │
        ├─► fanotify event (dir fid + name, pid)  ─┐
        │                                          ├─► cause builder: pid → lineage → cgroup/unit/session
        └─► eBPF exec/fork lineage (already cached)┘        │
                                                             ▼
                                         open cause (actor, kind), append change
                                                             │
                     on close-write: read file (O_NOFOLLOW) → blob store (zstd, BLAKE3)
                                                             │
                     on cause close: ledger commit (one SQLite txn, hash-chain entry)
                                                             │
 next checkpoint:  btrfs send --no-data -p prev  →  structural diff  →  reconcile:
                     state ∩ live = confirmed · state only = `offline` cause · live only = transient
```

"Live only" changes are expected: a file created and deleted between two checkpoints exists only in the live record. They are kept but flagged as transient.

## Checkpoints and storage layout

Fedora's layout (this host): one btrfs filesystem, subvolumes `root` → `/` and `home` → `/home`. `/boot` is a separate ext4 partition, so it is **not** covered by snapshots.

```
btrfs top level (subvolid=5, never mounted in the user's namespace)
├── root                      → /
├── home                      → /home
└── osreflog/
    ├── ledger                → /var/lib/osreflog   (own subvolume, so rolling back root never rolls back history)
    ├── snapshots/c0001 …     read-only snapshots of `root`
    └── probes/p0007          writable throwaway clones for bisect (deleted after the probe)
```

- `osreflogd` mounts the top level **privately in its own mount namespace** (`MountFlags=` / `PrivateMounts=` in the unit), so snapshots never show up in the user's `/`, `locate` or backup tools ([ADR 0004](./decisions/0004-snapshot-layout.md)).
- `/boot` config (BLS entries, `grubenv`, `grub.cfg`) is tracked as content blobs in the ledger, because snapshots can't cover it. Kernel images are package-owned and come back through dnf.
- Blob store: content-addressed by BLAKE3 and zstd-compressed. Only for text-like config files under live-capture scopes, < 1 MiB, and not on the secrets denylist ([threat-model.md](./threat-model.md)).

Checkpoint cost (P1): 53–173 ms on an idle system, 0.4–0.9 s from the dnf pre-transaction hook, and **5.3 s right after a 435 MiB transaction**, because btrfs flushes the transaction's dirty data first. The post-transaction checkpoint runs after rpm has finished, so it delays dnf's exit but never the package writes.

**Noise filter.** Even a one-package transaction touched 273 paths under `/var` (dnf caches, logs, the rpm database). Views hide package-owned paths, `/var/cache`, `/var/log`, the package databases, and files whose only change is their times (`relatime` access-time updates) by default; the state record keeps everything, and `--all` shows it.

**dnf transactions per interval** are those in the newer snapshot's history that the older snapshot's history doesn't already show as finished. dnf records whole seconds, which can't separate a transaction from a checkpoint taken in the same second; the two histories can. When the older snapshot was pruned, a time window decides.

Retention (defaults, configurable): keep every checkpoint for 48 h, daily for 14 days, weekly for 8 weeks, plus pinned ones (`boot-ok` linked to a bisect, user-pinned). The ledger is kept forever (it's small); blobs are pruned once no remaining checkpoint or cause needs them.

## Diffing

1. **Structural** (what): `btrfs send --no-data -p <older> <newer>` produces a command stream (`mkfile`, `rename`, `unlink`, `update_extent`, `chmod`, `chown`, `set_xattr`, …) without file data. osreflog parses this stream itself (protocol v2) rather than running a full rsync-style scan. See [ADR 0005](./decisions/0005-send-stream-diff.md). The diff is computed **eagerly** right after each checkpoint and cached in the ledger: a large transaction took 3.7 s to send in P1, too slow to run on demand.
2. **Package** (which packages): diff the RPM database (`/usr/lib/sysimage/rpm/rpmdb.sqlite`) between the two snapshots, and pull transaction metadata from `/usr/lib/sysimage/libdnf5/transaction_history.sqlite`. Files owned by a changed package fold into that package's cause instead of being listed one by one.
3. **Semantic** (what it means): per-format differs in the sandboxed `osreflog-parse` worker:

   | Format | Examples | Diff unit |
   | ------ | -------- | --------- |
   | INI / keyfile | NetworkManager, systemd units, `.desktop`, `/etc/dnf` | section.key |
   | systemd effective config | unit + drop-ins merged | effective property |
   | Unit enablement | symlinks in `*.wants/`, `*.requires/` | unit enabled/disabled per target |
   | passwd/group/shadow | `/etc/passwd`, `/etc/group` | user/group field; shadow → "password changed", never the hash |
   | key=value | `sysctl.d`, `modprobe.d`, `/etc/default/grub`, `/etc/os-release` | key |
   | fstab / crypttab | `/etc/fstab` | mount entry |
   | BLS entries | `/boot/loader/entries/*.conf` | kernel, options, initrd |
   | JSON / TOML / YAML | various | path |
   | dconf | user db, `/etc/dconf/db/*.d` | schema path / key, typed GVariant |
   | SELinux | booleans, local modules, fcontext | name |
   | fallback | anything text | unified diff |
   | binary | anything else | size + hash + `file(1)` type |

## Bisect

See [bisect.md](./bisect.md). The daemon builds a probe state (writable clone of a checkpoint, plus replayed causes), runs the user's test in the chosen tier, records the verdict, and deletes the clone.

## Undo

See [undo.md](./undo.md). It's a 3-way check (ledger "before", ledger "after", current file); it restores content from blobs or via reflink from a snapshot; packages go through `dnf5 history undo`. The undo is recorded as an `undo` cause, and a `pre-undo` checkpoint is taken first.

## Interfaces

- **D-Bus** system service `io.github.khaledsaeed18.osreflog` (zbus). Read methods (log, show, diff) are allowed for members of the `osreflog` group or via polkit `…read`. Mutating methods (`Checkpoint`, `Undo`, `Bisect*`) require polkit `…admin` (auth_admin_keep). See [ADR 0008](./decisions/0008-dbus-polkit.md).
- **CLI** is a thin D-Bus client with git-like porcelain; `--json` on every read command.
- **dnf5 hook**: an actions file in `/etc/dnf/libdnf5-plugins/actions.d/osreflog.actions` calls `osreflog hook pre ${pid}` / `osreflog hook post ${pid}` (`enabled=host-only`, so `--installroot` transactions are skipped). In the plugin's plain mode every stdout line of the hook is a command (`stop=<msg>` would abort the transaction), so the hook prints only `log.INFO=`/`log.WARNING=` lines, never fails, and gives up after 120 s. P1 verified that both `dnf` and `packagekitd` (GNOME Software's PackageKit path) wait for the pre hook before rpm writes anything. GNOME Software's offline updates go through dnf5daemon-server and are applied at boot; until that path is tested (P2), they show up as `offline` causes from the boot-time diff.

## Repo layout (planned)

```
osreflog/
├── crates/
│   ├── osreflogd/        daemon: wiring, D-Bus, systemd notify
│   ├── capture/          fanotify reader, scope marks, overflow handling
│   ├── lineage/          aya loader + userspace pid→lineage cache
│   ├── lineage-ebpf/     aya-ebpf programs (fork/exec/exit tracepoints)
│   ├── causes/           cause assembly rules
│   ├── btrfs/            ioctls: snapshot, subvol, send stream parser, FICLONE
│   ├── ledger/           SQLite schema, hash chain, blob store, refs
│   ├── diff/             structural + package diffs; semantic differs live in parse/
│   ├── parse/            osreflog-parse worker (sandboxed)
│   ├── bisect/           probe planner, nspawn / vm / metal runners
│   ├── undo/             3-way restore, dnf delegation
│   ├── cli/              osreflog
│   └── user-agent/       osreflog-user (dconf monitor)
├── data/                 systemd units, polkit policy, D-Bus config, dnf actions file
├── selinux/              policy module (osreflogd_t)
├── testbed/              scenario suite: scripted changes in a VM, expected ledger
├── docs/
└── xtask/
```
