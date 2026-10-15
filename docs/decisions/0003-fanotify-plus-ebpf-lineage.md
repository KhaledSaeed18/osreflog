# 0003: fanotify for events, eBPF for process lineage

- Status: Accepted 2026-10-15 after P1, amended: one filesystem mark instead of recursive inode marks
- Date: 2026-10-14

## Context
We need the changed object, the operation, and the actor's identity and ancestry, including for processes that exit within microseconds (`sed -i`, rpm scriptlets).

## Decision
- **fanotify** with one `FAN_MARK_FILESYSTEM` on the btrfs superblock (plus `/boot`, `/boot/efi`), `FAN_REPORT_DFID_NAME_TARGET | FAN_REPORT_PIDFD`, `FAN_CLOSE_WRITE` as the content trigger, scope filtering in userspace. Events are resolved by file handle, not path string.
- P1 numbers ([journal](../journal/p1-measure.md)): 9.1 events/s system-wide over 20 min of normal use (80 % `FAN_MODIFY`, not subscribed); 2 % of events had already lost their writer by the time userspace looked.
- **eBPF** tracepoints `sched_process_fork/exec/exit` → ring buffer → a userspace pid+starttime lineage cache with an exit grace period.

## Alternatives considered
- **BPF LSM path hooks only:** synchronous, in the actor's context, perfect attribution, and BPF LSM is active on this host. But path reconstruction in BPF is verifier-sensitive and a bug drops events silently. Kept as a P1 measurement and a possible later replacement.
- **inotify:** no pid, no file handles.
- **auditd watches:** gives attribution, but needs auditd rules, a different log pipeline, and has known overhead.
- **fanotify alone:** loses the identity of short-lived actors.

## Consequences
- Two kernel interfaces to keep in sync, joined by pid + start time.
- `/usr` is live-captured too (without content), so rpm writes outside a dnf bracket stand out.
- P3: the mark goes through the btrfs top-level mount, because a filesystem mark through a subvolume fails with EXDEV. The eBPF side is C (`bpf/lineage.bpf.c`), as amended in ADR 0001.
- The original plan (recursive inode marks per scope tree) is dropped: more code, more marks, and new directories race with mark creation.
