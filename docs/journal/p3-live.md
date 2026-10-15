# P3: Live witness and causes

- Status: **closed**
- Started: 2026-10-15
- Closed: 2026-10-15
- Roadmap: [P3](../roadmap.md#p3-live-witness-and-causes-closed-2026-10-15)
- Exit (amended, see the log): the testbed passes, and a scripted workload of real admin tools gives ≥ 95 % attribution of config changes, with every miss explained. **Met:** testbed 16/16; workload **87/87 config changes (100 %)** and 239/239 non-noise changes explained, across ~22 tools.

## Plan

The live witness adds *who* to P2's *what*. Every change the state witness proves gets a cause when the live witness saw it happen, and the coverage is measured, not assumed.

| Piece | Where | Unsafe |
| ----- | ----- | ------ |
| fanotify: init, filesystem mark, event parser, `open_by_handle_at` | `crates/fanotify` | only `sys.rs` (FFI) |
| Process lineage: fork/exec/exit from eBPF (`tp_btf`), ring buffer, pid cache seeded from `/proc` | `bins/osreflogd/bpf/lineage.bpf.c` + `src/lineage.rs` | no (aya) |
| Capture thread: polls fanotify and the ring buffer, resolves paths and actors, filters scopes | `src/capture.rs` | no |
| Cause builder: dnf bracket → unit → command in a session → unknown; closed after 2 s of quiet | `src/causes.rs` (pure, unit-tested) | no |
| Blob store: content of in-scope text files ≤ 1 MiB, deflate (flate2), BLAKE3-addressed, secrets never stored | `src/blobs.rs` | no |
| Ledger schema v2: causes, live changes, gaps, attribution of interval changes | `crates/ledger` | no |
| Reconciliation at each interval: `both` (attributed) or `state` only; coverage per class | engine | no |
| CLI: `causes`, `show #id`, `why <path>`, diff lines with their cause | `bins/osreflog` | no |

Decisions taken while planning:

- **eBPF in C, compiled by clang in `build.rs`, loaded with aya**, exactly as drainscope does. ADR 0001 said aya-ebpf (kernel-side Rust); that needs a nightly toolchain and `bpf-linker`, hundreds of MB on this connection, for no gain at this size. clang 22 and libbpf-devel are already installed.
- **Tracepoints checked against this kernel's BTF:** `sched_process_fork(parent, child)`, `sched_process_exec(task, old_pid, bprm)`, `sched_process_exit(task, group_dead)`. On 7.2 the exit tracepoint has a `group_dead` argument; the program reads only the task and checks `pid == tgid`, so it also loads on older kernels.
- ~~**Paths are resolved per subvolume by fsid.**~~ Superseded on the first real start (EXDEV, see the log): the mark and the resolution go through the btrfs top-level mount, and scope is decided by path prefix.
- **Deflate instead of zstd** for blobs: flate2 is already in the cargo cache; zstd would be a C build plus a download. The blob format records its codec, so this can change later.
- **The kernel's default fanotify queue (16,384 events), not an unlimited one.** An overflow becomes a recorded *gap*, and the state witness still proves what changed in that window. Bounded memory beats completeness here, because the state witness backs it up.
- **Attribution isn't hashed into the P2 interval records**: it's appended to the chain as its own `attribution` record per interval, so ledgers written by P2 still verify.
- **The testbed runs on the real machine against the installed daemon**, in a scratch area (`/etc/osreflog-testbed/`, a dummy unit, one small package). A VM testbed comes with the bisect infrastructure (P6), which has to boot VMs anyway.

## Log

### 2026-10-15: built, installed, testbed passing; dogfooding starts

Built: the fanotify crate, the lineage eBPF program and table, causes, blobs, the capture thread, ledger schema v2 and v3, attribution, `causes`/`show`/`why`, and the testbed (`scripts/p3/testbed.sh`). 70 tests pass, with zero clippy warnings at `pedantic`.

**First real start: EXDEV.** `fanotify_mark(FAN_MARK_FILESYSTEM)` with FID reporting failed with *Invalid cross-device link* through `/`. The kernel refuses a filesystem-wide mark through a btrfs **subvolume**: a subvolume has its own fsid, so a superblock mark through one is ambiguous. P1's fatrace didn't hit this because it marks mounts without file handles. Fix: mark through the **top-level mount** osreflogd already has (`/run/osreflog/top`) and resolve handles against it. Paths come back as `/run/osreflog/top/root/...`, exactly as P1 saw. The resolver keeps only paths under `<top>/<tracked subvolume>/` and maps them to `/...`. `home`, the ledger and the snapshots fall outside that prefix. A unit test pins the prefix rule, so `rootfs` is not `root`.

**Testbed on the real machine: 16/16 checks, on the first run with a running witness.**

| Scenario | Cause recorded |
| -------- | -------------- |
| `sed -i` from a shell script | `command`, labelled with the command typed at the interactive shell; the process (`sed (pid …)`) is recorded per change |
| editor save (temp file renamed over the target) | attributed, shown as a modify of the target |
| a write from `systemd-run --unit osreflog-testbed-writer` | `unit: osreflog-testbed-writer.service` |
| `systemctl enable` | seen; attributed to `unknown: systemd`, because pid 1 writes the symlink (Q6, as predicted) |
| `dnf install` / `dnf remove figlet` | two `dnf` causes, labelled with dnf's own command line; 480 and 74 package-owned paths counted rather than listed |
| `server.key`, a root `0600` file | seen; content never kept (path rule and mode rule) |
| created and deleted at once | no change recorded |
| `why sed.conf` | the real content diff: `-mode = old` / `+mode = new` |

**What the first run taught, fixed the same day:**

1. **Times-only changes are invisible to any witness.** The four "unexplained" config changes were all `relatime` access-time updates, which raise no fanotify event. They're now outside attribution and coverage, as they already were outside the default view; one `Change::times_only()` rule covers all three places. Coverage went from 13/17 to **9/9 config, 9/9 overall** on the rerun.
2. **Timer-driven state is noise.** rsyslog rewrites `/var/lib/rsyslog/imjournal.state` every few seconds: 19 of 27 causes in the first two-minute run. That and similar files (upower history, systemd timers and timesync, chrony, logrotate, NetworkManager timestamps, random-seed) are now noise.
3. **Each change records its own process** (ledger schema v3, `live_changes.actor`). Before, `why` named the cause's first process; for a file created by a script it said `mkdir`. Old rows have no actor and fall back to the cause's.
4. **A created path has no "before".** `why` compared a re-created file with the deleted file's last content. Fixed.

**Measured on the second run:** 5,188 fanotify events, 4,270 out of scope (other subvolumes, noise), 40 changes recorded into causes, 566 processes known, daemon at 41 MB RSS right after start. The restart was recorded as a gap ("osreflogd was stopped"). The ledger verified 165 chain entries.

**Attribution so far** (`scripts/p3/coverage.py`, all intervals the live witness watched): 47/51 config changes. The 4 misses are the times-only updates above, stored before the fix; with the fix in the running daemon they no longer count.

**Next: the exit measurement.** Use the machine normally until **2026-10-22** with osreflogd running, then run `scripts/p3/coverage.py`. The phase closes at ≥ 95 % of config changes explained, with every miss explained in this journal.

### 2026-10-15: the exit criterion changes, and the workload passes

**Why the criterion changed.** "A week of dogfooding" assumed this laptop was a daily machine. It isn't: the owner works on macOS, and this Fedora laptop is for development only. A quiet week would mostly record rsyslog and `dnf makecache` and prove little. The replacement is a **scripted workload of real admin tools**, each change made by the tool itself and reverted right after (`scripts/p3/workload.sh`). That has far more breadth than a week of light use, and it's repeatable. Passive coverage still accumulates whenever the machine is used (`scripts/p3/coverage.py` reads all of it).

**The workload** (checkpoints `workload start` → apply → `workload applied` → revert → `workload reverted`; the middle one matters because reverted changes cancel out in a start..end diff):

| Tool | What it wrote | Cause recorded |
| ---- | ------------- | -------------- |
| `timedatectl set-timezone` | `/etc/localtime` | `unit: systemd-timedated.service` |
| `hostnamectl --pretty` | `/etc/machine-info` | `unit: systemd-hostnamed.service` |
| `tuned-adm profile` | `/etc/tuned/{active_profile,profile_mode,ppd_base_profile}` | `unit: tuned.service`, `unit: tuned-ppd.service` |
| `firewall-cmd --permanent --new-zone` | `/etc/firewalld/zones/osreflogwl.xml` | `unit: firewalld.service` |
| `nmcli connection add` | NetworkManager keyfile (content never kept) | `unit: NetworkManager.service` |
| `useradd`, `groupadd`, `userdel`, `groupdel` | passwd, group, shadow, gshadow and their `-` backups (shadow content never kept) | `command` |
| `alternatives --install/--remove` | `/etc/alternatives/…`, `/var/lib/alternatives/…`, `/usr/local/bin/…` | `command` |
| `visudo` + `install`, `tee`, `printf >`, `cp`, `install -D`, `python3` | sudoers.d, cron.d, sysctl.d, modprobe.d, journald.conf.d, sshd_config.d drop-ins | `command` |
| `dconf update` | the compiled `/etc/dconf/db/*` databases | `command` |
| unit file, `systemctl edit --stdin` | the unit and its drop-in | `command` |
| `systemctl enable` | the `.wants/` symlink | `unknown: systemd` (pid 1) |
| `sed -i`, `perl -pi`, `vi -es`, `tar -x`, `rsync`, `chmod`, `ln -s`, `mv`, `setfacl`, `chattr +i` | files in `/etc/osreflog-workload` | `command` |
| `dnf install` / `dnf remove figlet` | `/etc/ld.so.cache` + 291 / 74 package paths | `dnf` (package paths as `bracket`) |

Background causes seen in the same window: `plocate-updatedb.service` and `unbound-anchor.service`, both real.

**Finding: D-Bus delegation is common.** Six of the tools don't write their files; they ask a daemon over D-Bus (timedated, hostnamed, tuned, firewalld, NetworkManager, systemd). osreflog attributes those changes to the daemon. That's true but only half the answer: "firewalld wrote it because `firewall-cmd` asked". Q6 expected this only for `systemctl enable`; it's a general pattern, and attributing the caller is now the first P4 item.

**Finding: osreflog caught a tool's leftover.** After the revert, firewalld leaves `/etc/firewalld/zones/osreflogwl.xml.old`, a backup it writes when deleting a zone. The workload's revert now removes it. The other paths that differ between start and end are files rewritten in place with the same content (passwd, group, the dconf databases, tuned state, `/etc/localtime` recreated by timedated, `ld.so.cache`). Telling "rewritten, unchanged" from "changed" is P4's semantic diff.

**Numbers:** 4,504 fanotify events during the run, 3,224 out of scope, 389 changes recorded into causes, 0 open causes at the end, 930 processes in the lineage table. Ledger: 241 chain entries, verified.

## What changed in the design

- [capture.md](../capture.md): the mark goes through the btrfs top-level mount (EXDEV through a subvolume); scope is decided by path prefix.
- [ADR 0001](../decisions/0001-rust-drainscope-stack.md) amended: eBPF programs in C, as in drainscope. [ADR 0003](../decisions/0003-fanotify-plus-ebpf-lineage.md) notes the top-level mark.
- [open-questions.md](../open-questions.md): Q6 widened from `systemctl enable` to D-Bus delegation in general, moved to the start of P4.
- [roadmap.md](../roadmap.md): P3's exit criterion amended (workload instead of a week of dogfooding), P4 gains caller attribution for D-Bus-delegated writes.
