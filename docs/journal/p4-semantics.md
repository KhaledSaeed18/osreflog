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

### 2026-10-15: P4b, semantic summaries in a sandboxed worker, done

The summarizers live in `crates/semantic` (keyfile/INI, key=value, passwd and group, fstab, JSON, plain lines, unit enablement from the symlink path, masking, `/etc/localtime`, alternatives, other symlink targets, "rewritten, content unchanged"). They run in a **worker process**: osreflogd re-executes itself with `--summarize-worker`, and before reading anything the worker sets rlimits (512 MiB, 5 s CPU, no file writes, no new processes), drops every capability, sets no-new-privileges, enters a Landlock domain that handles every filesystem and network right and grants none, and installs a seccomp allowlist that kills the process on anything else. Three integration tests pin it: a file open inside the sandbox dies with SIGSYS, a real summary goes through, and garbage input exits cleanly. Summaries are computed when a cause closes, on a separate thread, and stored with the cause (ledger schema v4); they aren't part of the hash chain, since they can be recomputed.

From the workload, as `osreflog why` prints them:

| Path | Summary |
| ---- | ------- |
| `/etc/localtime` | `time zone: UTC → Asia/Beirut` |
| `/etc/passwd` | `user osreflogwl added (uid 961, home /home/osreflogwl, shell /sbin/nologin)` |
| `/etc/group` | `group osreflogwl added (gid 961)`, `group osreflogwlg added (gid 1001)` |
| `…/multi-user.target.wants/osreflog-workload.service` | `enabled osreflog-workload.service (wanted by multi-user.target)` |
| `…/osreflog-workload.service.d/osreflog.conf` | `new file (2 lines)`, `+ [Service] Environment = OSREFLOG=1` |
| `/etc/sysctl.d/99-osreflog-workload.conf` | `new file (2 lines)`, `+ vm.osreflog_not_a_real_key = 1` |

The paths that differ between the workload's start and its revert (passwd, group, the dconf databases, `ld.so.cache`, `/etc/localtime` re-created by timedated) now read as `rewritten, content unchanged` instead of looking like changes.

**What "before" means took three fixes**, each found by the workload:

1. **A temp file renamed over a path is a replace, not a move.** Editors, `useradd` and `systemctl edit` write `x.tmp` and rename it over `x`. The cause builder used to keep the rename and compare the result with the temp file's (empty) history. Now a rename over a path created in the same cause becomes a `modify` of the target, compared with the target's own previous content.
2. **Spaces around `=` and comments.** Detection of key=value files required `key=value` with no spaces, and comment lines counted as content lines. Both fixed, with tests.
3. **"Before" stops where the path last went away.** The workload creates the same drop-in on every run and deletes it in its revert. The previous content lookup crossed that delete and found the previous run's identical file, so the new file read as "rewritten, content unchanged". The lookup now stops at the last delete or rename-away of the path or any directory above it, and ignores snapshots older than that.

Workload after the fixes: **16/16 checks**, including six summary checks.

### 2026-10-15: P4d, the user scope, done

`osreflog-user` is a systemd user service. Every GSettings write goes through one daemon, so one monitor sees them all. The client calls `ca.desrt.dconf.Writer.Change` on the session bus, dconf-service replies with a tag, writes the database, and emits `Notify(prefix, keys, tag)`. The agent monitors all three. It reads the caller's process (pid, exe, cmdline, cgroup) when the call arrives, since the caller is still waiting for its reply. It pairs the reply's tag with the `Notify`, then compares the new values (`dconf read`, or `dconf dump` for a directory) with a cache built from `dconf dump /` at start. Changes from one process within 2 s form one cause, and a key changed and changed back within that window is dropped.

**Where it goes: the user's own ledger** (`~/.local/state/osreflog/user.db`, directory `0700`), not the system daemon. [ADR 0010](../decisions/0010-user-scope-own-ledger.md) has the reasons: the daemon's read action is open to any active session, it couldn't verify a user's claims anyway, and it would be a new root-daemon write method callable by every user. `osreflog user [KEY]` reads the user ledger directly and prints causes in the same format as `osreflog show`.

**Who made the change.** A process whose cgroup is an app scope (`app-[launcher-]<app id>-<n>.scope` or `.service`) is credited to that app. A shell among its ancestors overrides that: the first test credited a `dconf write` typed in a terminal to the terminal app (here, this desktop app's own scope). Now a shell-run write is a `command` cause labeled with its command line.

**A secret in the command line.** The first workload run found `dconf write …/api-key 's3cret-value'` recorded verbatim as the cause's command, even though the summary said `set to ***`. The system side had the same gap for positional keys (`nmcli connection modify home wifi-sec.psk …`). Argv redaction moved to `osreflog_model::redact` and now also hides the argument after a key whose last `/`, `.` or `:` segment names a secret. Both osreflogd and osreflog-user use it.

**User workload** (`scripts/p4/user-workload.sh`, run as the user against the installed agent): **8/8 checks**.

| Write | Recorded as |
| ----- | ----------- |
| `gsettings set … clock-show-seconds` from a shell, then back | two `command` causes, `false → true` and `true → false` |
| `dconf write` from a process started as `app-gnome-org.osreflog.Probe-….service` | `app: org.osreflog.Probe`, `set to 'dark'` |
| `dconf load` of three keys | one cause with three changes |
| `dconf write …/api-key` | `set to ***`; the value appears nowhere, including the command line |
| `dconf reset -f` of the directory | one cause, five keys `reset (was …)` |

Every cause also records `via dconf-service (ca.desrt.dconf.Writer.Change)`.

**Also from this run:** the system workload's one unexplained non-noise change was `/var/lib/docker/image/identity-cache.db`. dockerd keeps it open and writes it in place, and the live witness listens for close-after-write (by design: per-write events would flood). The state witness saw it, as it should. Container engines' stores (`/var/lib/docker/`, `/var/lib/containers/`) are now noise, like the package databases.
