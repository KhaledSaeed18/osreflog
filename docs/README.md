# osreflog docs

> A reflog for your Linux system: see what changed, who changed it, and bisect the change that broke it.

**Status:** P3 (live witness) next; P2 closed 2026-10-15 ([journal](./journal/p2-slice.md)): the read-only slice runs on the real machine. Last updated 2026-10-15.

## Reading order

| # | Doc | What's in it |
| - | --- | ------------ |
| 1 | [vision.md](./vision.md) | Problem, goals, non-goals, success criteria, principles, research angles |
| 2 | [architecture.md](./architecture.md) | Components, data model (change/cause/checkpoint), refs, storage layout, diffing, interfaces, repo layout |
| 3 | [capture.md](./capture.md) | Live witness: fanotify + eBPF lineage, scopes, cause rules, user scope, gaps |
| 4 | [bisect.md](./bisect.md) | Test contract, checkpoint → cause bisect, nspawn / vm / metal tiers |
| 5 | [undo.md](./undo.md) | 3-way undo per object type, what undo never does |
| 6 | [threat-model.md](./threat-model.md) | Assets, adversaries, secrets handling, mitigations, operating rules |
| 7 | [roadmap.md](./roadmap.md) | Phases P0–P8, the write gate and the boot gate |
| 8 | [environment.md](./environment.md) | This host's facts and expected friction |
| 9 | [open-questions.md](./open-questions.md) | Open questions with the phase that answers each, and resolved ones |

## Folders

| Folder | Contents |
| ------ | -------- |
| [decisions/](./decisions/) | Architecture Decision Records (ADRs) |
| [research/](./research/) | [Related work](./research/related-work.md); research tracks are added as they start |
| [naming/](./naming/) | The name-check report that led to "osreflog" |
| [journal/](./journal/) | One entry per phase: what worked, what broke, what changed |

## Conventions

- Items marked _(verify)_ are believed true but must be confirmed on this host, usually in P1.
- Decisions go in `decisions/` as ADRs; questions go in `open-questions.md` until decided.

## Glossary

| Term | Meaning |
| ---- | ------- |
| Change | One mutation of one object (file, package, dconf key, unit enablement) |
| Cause | A group of changes with one actor and reason: a dnf transaction, a unit, a command, an app |
| Checkpoint | A read-only btrfs snapshot of `/` + the ledger position, tagged with why it was taken |
| Ledger | The SQLite history of causes, changes, checkpoints and gaps, hash-chained |
| Live witness | fanotify + eBPF lineage: sees changes as they happen and knows who made them |
| State witness | btrfs send-stream diff between checkpoints: knows exactly what changed |
| Gap | A time window where the live witness may have missed changes (daemon down, overflow) |
| Probe | A throwaway writable clone used to test one state during bisect |
| Tier | Where a probe runs: `nspawn`, `vm`, or `metal` |
| Write gate / boot gate | Checklists that must pass before code writes to `/` or touches boot |
