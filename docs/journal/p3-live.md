# P3: Live witness and causes

- Status: **in progress**
- Started: 2026-10-15
- Roadmap: [P3](../roadmap.md#p3-live-witness-and-causes-next)
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
- **Paths are resolved per subvolume by fsid.** fanotify reports a per-subvolume fsid on btrfs; each subvolume of interest (`root` → `/`) maps to its own mount fd for `open_by_handle_at`. Events from other subvolumes (`home`, the ledger, snapshots) are dropped before any path resolution.
- **Deflate instead of zstd** for blobs: flate2 is already in the cargo cache; zstd would be a C build plus a download. The blob format records its codec, so this can change later.
- **The kernel's default fanotify queue (16,384 events), not an unlimited one.** An overflow becomes a recorded *gap*, and the state witness still proves what changed in that window. Bounded memory beats completeness here, because the state witness backs it up.
- **Attribution isn't hashed into the P2 interval records**: it's appended to the chain as its own `attribution` record per interval, so ledgers written by P2 still verify.
- **The testbed runs on the real machine against the installed daemon**, in a scratch area (`/etc/osreflog-testbed/`, a dummy unit, one small package). A VM testbed comes with the bisect infrastructure (P6), which has to boot VMs anyway.

## Log
