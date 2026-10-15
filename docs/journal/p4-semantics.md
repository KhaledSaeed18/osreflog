# P4: Callers, semantics and the user scope

- Status: **in progress**
- Started: 2026-10-15
- Roadmap: [P4](../roadmap.md#p4-semantic-diffs-and-user-scope-next)
- Exit: every differ fuzzed for at least 1 h with no crashes; readable semantic summaries for the testbed and workload scenarios; D-Bus-delegated writes credited to the tool that asked.

## Plan

Four slices, each tested on its own and then on the real machine with the P3 testbed and workload.

| Slice | What | Notes |
| ----- | ---- | ----- |
| **P4a** caller attribution | Watch method calls on the system bus (`BecomeMonitor`), resolve the caller's and the destination's pids, and when a daemon's cause closes, credit it to the client that asked shortly before. Recorded as `via: firewalld.service (org.fedoraproject.FirewallD1.config.addZone2)` | P3 found six of ~22 admin tools write through a daemon |
| **P4b** semantic summaries | Per format: keyfile/INI, key=value, passwd/group, fstab, unit enablement (from the symlink path), symlink targets, JSON; plus "rewritten, content unchanged". Computed when a cause closes, in a **sandboxed worker process** (no filesystem access via Landlock, a seccomp allowlist, no capabilities, memory and CPU limits), stored with the cause | the parsers read files any local user may influence (threat model A2) |
| **P4c** fuzzing | A long-running randomized harness per differ on the stable toolchain (mutations of real `/etc` files plus random bytes), ≥ 1 h each | cargo-fuzz needs nightly and libFuzzer: too heavy on this connection; the harness checks the same properties (no panic, bounded time and output) |
| **P4d** user scope | `osreflog-user`, a systemd user service that monitors dconf writes on the session bus and reports them with the app that made them | GNOME settings changes are the desktop's most common "what changed?" |

### P4a design

- A **separate monitor connection** (a monitor can only receive). Match rule: method calls. Filtered in code: no `org.freedesktop.DBus`, no polkit, no osreflog, and no members that read (`Get…`, `List…`, `Introspect`, `Can…`, `Is…`, `Check…`, case-insensitive), which would otherwise make false matches likely.
- For each kept call, look up the **caller's pid** right away (the caller waits for its reply, so it's still alive), and the **destination owner's pid**, retrying for ~2.5 s because bus activation may still be starting the daemon.
- When a `unit` or `unknown` cause closes, find a call whose destination owner pid is the cause's writer pid, made at most 10 s before the cause started, and choose the latest. The cause is reclassified from the **caller's** process chain (so `firewall-cmd` run from a shell becomes a `command` cause), and `actor.via` records the daemon and method. The per-change process stays the daemon, which is what actually wrote the file.

## Log
