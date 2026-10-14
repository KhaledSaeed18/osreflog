# 0002: Two witnesses: live capture + snapshot diff

- Status: Accepted
- Date: 2026-10-14

## Context
Live capture alone loses changes (daemon down, queue overflow, offline updates, a live USB). Snapshot diffs alone know *what* but never *who*. urljail already uses the "two witnesses" principle (guest CDP vs host eBPF), and it worked well.

## Decision
Every change is reported with the witnesses that saw it. The **state witness** (btrfs send-stream diff between checkpoints) is the source of truth for *what*; the **live witness** (fanotify + eBPF lineage) is the source of truth for *who*. Reconciliation runs at every checkpoint: confirmed (both), `offline` (state only), transient (live only).

## Alternatives considered
- **Live only (auditd-style):** cheaper, but gaps are silent.
- **Snapshots only (Snapper-style):** no attribution, so no causes, which rules out cause-level bisect.

## Consequences
- Attribution coverage becomes a measurable number (a success criterion).
- Checkpoints are needed even when the user doesn't care about rollback. They're cheap on btrfs.
