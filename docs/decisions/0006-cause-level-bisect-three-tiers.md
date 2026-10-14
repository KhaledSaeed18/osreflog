# 0006: Cause-level bisect with nspawn / vm / metal tiers

- Status: Accepted
- Date: 2026-10-14

## Context
Snapshot-level bisect narrows a regression down to "something in this 6-hour window", which often holds a 40-package upgrade plus config edits. Different regressions also need different environments to reproduce.

## Decision
Bisect checkpoints first, then causes within the final interval, rebuilding "state after cause k" by replaying causes onto a writable clone of the good checkpoint. Probes run in the cheapest tier that can reproduce the problem: `nspawn` → `vm` (the snapshot's own kernel, virtiofs root) → `metal` (one-shot BLS entry booting a clone).

## Alternatives considered
- **Checkpoint-only bisect:** simpler, but the answer is too coarse to act on.
- **VM only:** can't reproduce hardware regressions, which are a large share of real desktop breakage (Wi-Fi, GPU, suspend).
- **Metal only:** a reboot per probe makes the common userspace cases needlessly slow.

## Consequences
- Replay correctness is a research question (Q9); approximate replays are flagged in the result.
- The metal tier needs the boot gate and the most careful code in the project.
