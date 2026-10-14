# 0003: fanotify for events, eBPF for process lineage

- Status: Proposed. P1 measures event rates and confirms (open question Q1)
- Date: 2026-10-14

## Context
We need the changed object, the operation, and the actor's identity and ancestry, including for processes that exit within microseconds (`sed -i`, rpm scriptlets).

## Decision
- **fanotify** with recursive inode marks on scope trees, `FAN_REPORT_DFID_NAME_TARGET | FAN_REPORT_PIDFD`, `FAN_CLOSE_WRITE` as the content trigger.
- **eBPF** tracepoints `sched_process_fork/exec/exit` → ring buffer → a userspace pid+starttime lineage cache with an exit grace period.

## Alternatives considered
- **BPF LSM path hooks only:** synchronous, in the actor's context, perfect attribution, and BPF LSM is active on this host. But path reconstruction in BPF is verifier-sensitive and a bug drops events silently. Kept as a P1 measurement and a possible later replacement.
- **inotify:** no pid, no file handles.
- **auditd watches:** gives attribution, but needs auditd rules, a different log pipeline, and has known overhead.
- **fanotify alone:** loses the identity of short-lived actors.

## Consequences
- Two kernel interfaces to keep in sync, joined by pid + start time.
- `/usr` isn't live-watched; it relies on dnf brackets + the state witness.
