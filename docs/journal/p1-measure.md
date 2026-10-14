# P1: Measure by hand

- Status: **closed**
- Started: 2026-10-14
- Closed: 2026-10-15
- Roadmap: [P1](../roadmap.md#p1-measure-by-hand-closed-2026-10-15)
- Exit: reproducible `scripts/p1/` set plus this entry with numbers; Q1, Q2, Q4, Q7 answered. **Met:** one root script ([scripts/p1/run.sh](../../scripts/p1/run.sh)) ran every experiment in a private mount namespace and cleaned up after itself. Q1, Q2, Q7 and Q8 are resolved, Q4 is answered for dnf and PackageKit, and Q3 has data. Aggregated output is in [data/p1-results.md](./data/p1-results.md).

## How it ran

| Run | Command | What |
| --- | ------- | ---- |
| main | `sudo scripts/p1/run.sh` | all stages; 20 min fatrace baseline with the laptop in normal use |
| follow-up 1 | `sudo env VM_MEM=512M scripts/p1/run.sh only nspawn vm` | nspawn probe with result file; 512 MiB VM (panicked, see below) |
| follow-up 2 | `sudo env VM_MEM=512M DRACUT_MODE=hostonly scripts/p1/run.sh only vm` | 512 MiB VM with a small initramfs |

Everything lived under `osreflog-p1/` at the btrfs top level and was deleted at the end of each run. The system kept these packages: `fatrace`, `bpftrace` (plus bcc, clang21-libs, kernel-devel as weak deps), `libdnf5-plugin-actions`, `hyperfine`, `figlet`. **SELinux: 0 AVC denials** (everything ran as unconfined root).

## Results by question

### Q1: live-capture scope strategy → one filesystem mark, filter in userspace

fatrace (fanotify filesystem marks on every real mount, events `W + D <>`) over 1,200 s of normal use:

| Measure | Value |
| ------- | ----- |
| All events | 10,906: **9.1/s** average, peak 177/s |
| Of which `W` (modify) | 8,721 (80 %). The design subscribes to close-write instead, which leaves ~2/s |
| `/var` | 6.2/s (rsyslogd, systemd-journald, postgres, drainscope's daemon) |
| `/home/*/.config`, `.local` | 2.8/s (mostly the Claude desktop app) |
| In the default scopes (`/etc`, `/boot`, `/usr/local`, `/opt`, selected `/var/lib`) | **7 events in 20 min**, all NetworkManager |
| Writer had exited before fatrace read `/proc/<pid>/exe` | 2.05 % of events |

**Decision:** a single `FAN_MARK_FILESYSTEM` on the btrfs superblock (covers `root` and `home` subvolumes, new directories automatically), with scope filtering in userspace. That replaces recursive inode marks. It also makes live capture of `/usr` affordable, so rpm writes are attributed directly rather than only through the dnf bracket. Caveat: the baseline had no heavy browsing; re-measure under browser load in P3 and keep the in-kernel filter question parked for then. See [ADR 0003](../decisions/0003-fanotify-plus-ebpf-lineage.md).

**New finding:** fatrace ran inside the private namespace where the top level is mounted, and **every path came back through that mount** (`/run/osreflog-p1/top/root/var/...`, `/run/osreflog-p1/top/home/...`). In the daemon, path strings from `/proc/self/fd` are therefore ambiguous. Events must be resolved from the directory file handle (`FAN_REPORT_DFID_NAME`) with `open_by_handle_at` against a mount fd chosen per subvolume. Recorded in [capture.md](../capture.md).

### Q2: private top-level mount → works

`mount -o subvolid=5` inside `unshare -m --propagation private`: visible in the script's namespace (1 mount), **not visible in PID 1's namespace (0)**. Snapshots created there never appeared in `/`. [ADR 0004](../decisions/0004-snapshot-layout.md) is now Accepted.

### Q3: space accounting → partly answered, stays open

- Quotas are **not enabled** on this filesystem (`btrfs qgroup show`: "quotas not enabled").
- `btrfs filesystem du -s` on two snapshots took **23.4 s**: far too slow for routine use. Exclusive sizes: `s-a` 62.6 MiB, `s-b` 3.3 MiB.
- The send stream gives a cheap upper bound for free: 552 MB of full send data for the 435 MiB install, 9.6 MB for a one-package install.

P2 starts with send-stream estimates and evaluates simple quotas (squota) on a loopback btrfs before ever enabling anything on the real disk.

### Q4: dnf5 hook coverage → dnf and PackageKit both block on the hook

Actions file: `pre_transaction::::hook.sh pre ${pid}` / `post_transaction::::hook.sh post ${pid}`. The hook snapshots `/` from its own private mount namespace; the pre hook then sleeps 3 s to make ordering unambiguous.

| Transaction | Hook parent | Pre snapshot | Post snapshot | First package file born after pre hook returned |
| ----------- | ----------- | ------------ | ------------- | ------------------------------------------------ |
| `dnf install hyperfine` | `dnf` | 918 ms | 80 ms | `/usr/bin/hyperfine` **+770 ms** |
| `pkcon install figlet` | `packagekitd` | 412 ms | 174 ms | `/usr/bin/figlet` **+565 ms** |

Both package managers wait for the pre hook before rpm writes anything, and both load libdnf5 plugins. Still open: **dnf5daemon-server**. dnf warned that it was invalidating "a pending offline transaction initiated by dnf5daemon-server", which means GNOME Software on Fedora 44 prepares offline updates through dnf5daemon, applied at boot by `system-update.target`. Those are the cases to test in P2. Until then, the boot-time diff covers them as `offline` causes.

### Q7: VM tier boot path → works in 512 MiB with a host-only initramfs

| Initramfs | Size | dracut time | Guest RAM | Result | qemu peak RSS |
| --------- | ---- | ----------- | --------- | ------ | ------------- |
| `--no-hostonly --add virtiofs` | 254 MiB | 41 s | 768 MiB | `running` at 36.6 s, 0 failed units | 1,082 MiB |
| same | 254 MiB | (cached) | 512 MiB | **kernel panic** unpacking the initramfs (out of memory) | – |
| `--hostonly --omit plymouth --add virtiofs --add-drivers "virtiofs virtio_pci virtio_ring"` | **37 MiB** | 13 s | **512 MiB** | **`running` at 34.2 s, 0 failed units** | **615 MiB** |

Boot: direct kernel boot of the snapshot's own `/usr/lib/modules/<ver>/vmlinuz`, `root=virtiofs:rootfs` (dracut's `74virtiofs` module), `selinux=0`, an emptied `/etc/fstab` in the clone, `-nic none`, free-page reporting balloon. virtiofsd peaked at 4 MiB. The probe unit wrote its verdict to `/var/tmp/p1-result` inside the clone, and the host read it back after poweroff.

### Q8: cross-mount reflink → works

`cp --reflink=always` from a snapshot under the private top-level mount into `/var/tmp` (the root mount) succeeded: FICLONE works across mount points of the same filesystem. Undo can restore from snapshots without copying data.

## Other measurements

**Snapshots**

| Operation | Time |
| --------- | ---- |
| Read-only snapshot of `/`, idle system | 53–173 ms (5 runs) |
| Snapshot right **after** the 435 MiB install | **5,252 ms**: btrfs flushes the transaction's dirty data first |
| Pre-transaction snapshot from the dnf hook | 412–918 ms |
| Subvolume delete | 3 ms (cleanup happens later in the btrfs cleaner thread) |

The post-transaction checkpoint costs time in proportion to what the transaction just wrote. It runs in `post_transaction`, after rpm has finished, so it delays dnf's exit but never the package writes.

**Send-stream diff** (`btrfs send --no-data --proto 2 -p old new`)

| Interval | Commands | Stream | Time | Full send (with data) | `find-new` |
| -------- | -------- | ------ | ---- | --------------------- | ---------- |
| 16-package install (435 MiB) | 176,980 | 18.2 MB | 3.7 s (+0.5 s `receive --dump`) | 552 MB in 6.7 s | 20,888 paths, **missed all 75 deletions** |
| 1-package install | 459 | 41 KB | 50 ms | 9.6 MB | 33 paths, missed all 9 deletions |

- New files appear first under orphan names `o<inode>-<gen>-0` and are then renamed into place. The parser must track renames (expected in ADR 0005, now confirmed).
- The rpmdb copied out of each snapshot gave exactly the 16 (and 1) added packages.
- Noise: in the one-package transaction, 273 of 459 commands were under `/var` (dnf caches, logs, rpm database). The cause view needs a default noise filter for `/var/cache`, `/var/log` and the package database files, while the state record keeps everything.
- 3.7 s for a large transaction is over the 2 s target for `osreflog diff`. Decision: compute the diff **eagerly** after each checkpoint and store it in the ledger, so `diff` reads a cached result.

**Process lifetimes** (bpftrace, exec → main-thread exit)

| Window | Exits | < 1 ms | < 10 ms | < 100 ms |
| ------ | ----- | ------ | ------- | -------- |
| During `dnf install hyperfine` | 87 | 20 (23 %) | 58 (67 %) | 71 (82 %) |
| 20 min baseline | 626 | 33 (5 %) | 441 (70 %) | 482 (77 %) |

Most actors live for single-digit milliseconds. Reading `/proc` after the event loses some of them (2 % in the fatrace run). This confirms the eBPF lineage cache with an exit grace period.

**nspawn tier**

| Probe | Time | Result |
| ----- | ---- | ------ |
| `--ephemeral --pipe -- rpm -q bash` | 374–549 ms | correct output |
| `--ephemeral -b` to `is-system-running`, then poweroff | 26–27 s | `degraded`: only `auditd.service` and `audit-rules.service` failed (no audit in containers; expected) |

`--console=pipe` captured nothing from the booted container. The probe now writes its result to a bind-mounted file, which is also the design for bisect verdicts.

## Gotchas

1. **Slow network.** The 16-package install downloaded 101 MiB at 60–120 KiB/s and took 32 min, mostly for bpftrace's weak dependencies (`kernel-devel` alone 74 MiB). Use `--setopt=install_weak_deps=False` for tooling in later phases.
2. **dnf invalidated GNOME Software's pending offline update.** It's harmless (GNOME Software prepares it again), but osreflog's own tooling must never do that silently.
3. **Never edit a running bash script.** bash reads scripts lazily; changing `run.sh` mid-run would have made the root process continue at a shifted byte offset. Edits go to a copy and are applied after the run.
4. **Paths through private mounts:** see Q1.
5. **Probe unit type:** a `Type=oneshot` probe in the boot transaction would make `systemctl is-system-running --wait` wait for itself. `Type=exec` avoids it.
6. **Probes need `/etc/fstab` neutralized** (bind an empty file in nspawn; truncate it in the VM clone). Otherwise systemd waits for this host's disks by UUID.
7. `sudo VAR=value cmd` is refused for variables outside `env_keep`; `sudo env VAR=value cmd` works.

## What changed in the design

- [capture.md](../capture.md): one filesystem mark instead of recursive inode marks; `/usr` is live-captured too; event resolution by file handle, never by path string.
- [ADR 0003](../decisions/0003-fanotify-plus-ebpf-lineage.md) accepted with that amendment; [ADR 0004](../decisions/0004-snapshot-layout.md) and [ADR 0005](../decisions/0005-send-stream-diff.md) confirmed.
- [architecture.md](../architecture.md): eager diff after each checkpoint; noise filter; post-transaction checkpoint cost; dnf5daemon/offline path noted.
- [bisect.md](../bisect.md): VM tier uses a host-only virtiofs initramfs and 512 MiB; result files instead of consoles; expected container failures (audit).
- [open-questions.md](../open-questions.md): Q1, Q2, Q7, Q8 resolved; Q3 and Q4 narrowed.
