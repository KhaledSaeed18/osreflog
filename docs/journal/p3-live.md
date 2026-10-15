# P3: Live witness and causes

- Status: **in progress**
- Started: 2026-10-15
- Roadmap: [P3](../roadmap.md#p3-live-witness-and-causes-in-progress)
- Exit: the testbed passes; one week of dogfooding gives ≥ 95 % attribution of `/etc` changes, with the misses explained here.

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
