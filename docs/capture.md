# Live capture and attribution

The live witness answers *who* and *why* at the moment of a change. It must work even for short-lived actors such as `sed -i`, rpm scriptlets, and `systemctl enable`, which have often exited by the time userspace reads the event.

## Two event sources

| Source | Gives | Misses |
| ------ | ----- | ------ |
| **fanotify** | which object changed (dir file handle + name), op, pid of the actor | who the pid *was* if it already exited; anything during overflow |
| **eBPF lineage** (tracepoints `sched_process_fork`, `sched_process_exec`, `sched_process_exit`) | for every pid: exe, argv (truncated, redacted later), ppid, cgroup id, uid, session, start time | nothing about files |

The daemon joins them: fanotify's pid (+ start time, to defeat pid reuse) → the lineage cache → the chain of ancestors up to a unit, a session or a dnf transaction. Exited pids stay in the cache for a grace period (default 30 s), so events that arrive late still resolve.

Why not one source? See [ADR 0003](./decisions/0003-fanotify-plus-ebpf-lineage.md). In P1, 23 % of processes during a dnf transaction lived under 1 ms and 67 % under 10 ms, and fatrace failed to read the exe for 2 % of events because the writer had already exited. In short:
- fanotify is a stable ABI built for exactly this. It reports directory entry events with file handles, and the kernel coalesces events. It doesn't keep the actor alive or describe it.
- eBPF on exec/fork is small and robust, and gives full lineage even for processes that exit in microseconds.
- BPF LSM path hooks (`path_unlink`, `path_rename`, `file_open` with write mode) could do both in one place, synchronously, in the actor's context. BPF LSM is active on this host (`lsm=…,bpf,…`). It stays the **alternative** we measure in P1 rather than the default: it ties correctness to verifier-sensitive path reconstruction in BPF, and a bug there loses events silently.

## Marks and scopes

**One filesystem mark, filtered in userspace** (decided in P1, [journal](./journal/p1-measure.md#q1-live-capture-scope-strategy--one-filesystem-mark-filter-in-userspace)). `/` and `/home` share one btrfs filesystem, so a single `FAN_MARK_FILESYSTEM` on its superblock sees both subvolumes, and new directories need no extra marks. Measured on this laptop over 20 minutes of normal use: 9.1 events/s for the whole filesystem, 80 % of them `FAN_MODIFY`, which we don't subscribe to. The remaining rate is a few events per second, cheap to filter in userspace. `/boot` and `/boot/efi` are separate filesystems and get their own marks.

Scopes decide what is *recorded*, not what is marked:

| Scope | Recorded as |
| ----- | ----------- |
| `/etc`, `/usr/local`, `/opt`, `/var/lib/{NetworkManager,bluetooth,fwupd,AccountsService}` (configurable) | changes with content blobs |
| `/boot/loader/entries`, `/boot/grub2`, `/boot/efi/EFI/fedora` | changes with content blobs (snapshots don't cover `/boot`) |
| `/usr` | changes without content, folded into the `dnf` cause when inside a bracket. A `/usr` write *outside* a dnf cause is a finding and is shown on its own |
| `/var/cache`, `/var/log`, the rpm/libdnf5 databases, `/home` | ignored by the system daemon (noise, or the user agent's job). Still covered by the state witness |

Events: `FAN_CREATE | FAN_DELETE | FAN_MOVED_FROM | FAN_MOVED_TO | FAN_ATTRIB | FAN_CLOSE_WRITE | FAN_ONDIR`, init flags `FAN_REPORT_DFID_NAME_TARGET | FAN_REPORT_PIDFD | FAN_UNLIMITED_QUEUE`. `FAN_CLOSE_WRITE` (not `FAN_MODIFY`) is the content trigger: one event per write session, and the file is complete when we read it.

**Mark through the top-level mount** (P3). The kernel refuses `FAN_MARK_FILESYSTEM` with FID reporting through a btrfs *subvolume* (EXDEV: a subvolume has its own fsid, so a superblock mark through one is ambiguous). osreflogd marks the filesystem through its private top-level mount instead and resolves handles against it. Paths under `<top>/<tracked subvolume>/` map to `/...`; everything else (other subvolumes, the ledger, snapshots) is out of scope.

**Resolve by handle, never by path string.** In P1, fatrace ran in a private mount namespace with the btrfs top level mounted, and every event path came back through that mount (`/run/…/top/root/etc/…`). osreflogd has the same layout, so it resolves each event from its directory file handle with `open_by_handle_at`, against a mount fd chosen per subvolume (the `root` subvolume as `/`, `home` as `/home`), and derives the logical path from that.

Re-measure under heavy browser load in P3. If userspace filtering ever becomes the bottleneck, the fallback is BPF LSM hooks with in-kernel filtering (see ADR 0003).

## Building causes

Rules, in priority order, applied to the actor's lineage (nearest ancestor wins):

1. **Inside a dnf bracket**: the hook recorded `pre-transaction` with the package manager's pid (`dnf` or `packagekitd`; both run libdnf5 plugins, verified in P1). Any descendant (rpm, scriptlets, `systemctl preset`, `dracut`, `grub2-mkconfig`) → the `dnf` cause. Writes to `/usr` are expected and folded in.
2. **osreflogd itself** (undo, probe setup) → the `undo` cause, or ignored for probe clones (they live outside `/`).
3. **System unit**: the actor's cgroup path is `/system.slice/<unit>` → a `unit` cause per unit, closed after a quiet period.
4. **Login session / user scope**: lineage reaches `session-N.scope` or a terminal app scope (`app-…-<terminal>-….scope`, the same cgroup naming drainscope uses) → a `command` cause, labelled with the outermost command under the shell (e.g. `sudo vim /etc/hosts`). `sudo`/`pkexec`/`run0` record both the invoking user and the target user.
5. **Nothing matched** → an `unknown` cause with the raw lineage, so the user still sees the exe and parent chain.

`systemctl enable` is a special case worth noting: the writer of the symlinks in `/etc/systemd/system/*.wants` is **pid 1** (via D-Bus), not `systemctl`. Attribution needs the D-Bus caller. Plan: monitor `org.freedesktop.systemd1.Manager` `EnableUnitFiles`/`DisableUnitFiles` method calls on the system bus (`BecomeMonitor`, root) and attach the caller's pid/lineage _(verify: monitor overhead and match-rule precision)_.

## User scope (optional, `osreflog-user`)

- **dconf**: settings writes go through `dconf-service` on the session bus. The user agent becomes a monitor for `ca.desrt.dconf.Writer.Change` calls; the call's sender identifies the app (unique name → pid → cgroup → app ID). The value written is decoded from GVariant. _(verify: `BecomeMonitor` on the session bus as the same user; GNOME 50 dconf write path)_
- **`~/.config`**: unprivileged fanotify allows inode marks only, and the per-user mark limit on this host is 67,456. That's enough for `~/.config` (verify size), not for `$HOME`.
- User-scope data goes to the same ledger through the daemon's D-Bus API, tagged with the uid. The daemon never reads `$HOME` itself.

## Overflow and gaps

- `FAN_Q_OVERFLOW` (only possible if the queue is bounded) or a lineage ring-buffer drop → record a **gap** with a time window. The next checkpoint's send-stream diff covers the gap: any change in that window becomes `offline`/unattributed and the report says why.
- The daemon starts early (`DefaultDependencies=no`, `Before=sysinit.target` _(verify feasibility)_). Changes made before it starts (initramfs, offline updates via `system-update.target`) are caught by the boot-time checkpoint diff.
- On shutdown it writes a `stop` marker. A missing marker means a crash or power loss, recorded as a gap.

## Content capture

On `FAN_CLOSE_WRITE` or `FAN_MOVED_TO` for an in-scope regular file:

1. Open it with `openat2(dirfd, name, RESOLVE_NO_SYMLINKS | RESOLVE_BENEATH)`, where `dirfd` comes from the event's directory handle (`open_by_handle_at`). Never follow a path string.
2. If it's < 1 MiB, looks like text, and isn't on the secrets denylist → zstd blob, BLAKE3 id. Otherwise store size + hash + mode only.
3. The "before" content is the previous blob for that path, or for the first sighting, the file as it is in the latest checkpoint (read through the private top-level mount).

A file can change again before we read it. The blob is then the *later* content. That's accepted and flagged (`raced=true`), because the state witness has the final truth.
