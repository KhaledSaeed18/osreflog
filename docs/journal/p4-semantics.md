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

### 2026-10-15: P4a, D-Bus caller attribution, done

First real run (workload): **5 of 6** delegated tools credited to the caller (`timedatectl`, `hostnamectl`, `tuned-adm`, `firewall-cmd`, `nmcli`). The bus monitor works inside the hardened unit (`PrivateNetwork=yes` still allows the system bus socket).

Two gaps, both fixed:

1. **`systemctl` as root never touches the bus.** It talks to pid 1 over its private socket, `/run/systemd/private`, which no bus monitor can see. For writes by pid 1, osreflog now looks in the lineage table for the latest `systemctl` with a writing verb (`enable`, `disable`, `mask`, `edit`, `set-default`, …) that ran in the window. The lineage table records each process's exec time for this. That's Q6's own alternative, which turned out to be required rather than optional.
2. **`rm -rf` of a directory lost its entries' paths.** The file deletions arrive before the directory deletion, but by the time they're processed the directory can't be reopened by handle. The path cache had the answer, but the resolver opened the handle first, and every directory deletion cleared the whole cache. The cache is now consulted first (an fd is opened only when content is needed), and a directory deletion or rename forgets only that directory's subtree.

Second run: **10/10 checks, 86/86 config changes and 236/236 non-noise changes explained, all six delegated tools credited to the caller**, with 12 causes credited through D-Bus. The firewalld `.old` leftover no longer appears, since the workload's revert removes it.

A delegated cause, as `osreflog show` prints it:

```
#257   2026-10-15 12:00:58  command  sudo sh -c …/install-dev.sh && sleep 3 && …/workload.sh  (1 change)
  actor    /usr/sbin/firewall-cmd (pid 1497119, uid 0) in /user.slice/user-0.slice/session-c620.scope
  via      firewalld.service (org.fedoraproject.FirewallD1.config.zone.remove)
  > 2026-10-15 12:00:58  /etc/firewalld/zones/osreflogwl.xml -> /etc/firewalld/zones/osreflogwl.xml.old  [content kept]
```
