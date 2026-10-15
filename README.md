# osreflog

**A reflog for your Linux system: see what changed, who changed it, and bisect the change that broke it.**

> **Status: P4 done; undo (P5) is next, behind the write gate.** osreflog runs on a real Fedora 44 machine. Every change to `/etc` and friends gets a cause: the command, service, package transaction or app that made it, including tools that ask a daemon over D-Bus (`timedatectl`, `firewall-cmd`, `systemctl enable`…). `osreflog why <path>` shows who changed a file and what it means (`user osreflogwl added`, `time zone: UTC → Asia/Beirut`, `enabled foo.service`), computed in a sandboxed worker. `osreflog user` does the same for your own GNOME settings. Bisect and undo come later. See the [roadmap](docs/roadmap.md).

"It worked last week." Your Wi-Fi drops, suspend stops working, or an app won't start, and something changed in between. A package update, a config edit, a GNOME setting, a firmware update, a command you ran at 2 a.m. Linux can take snapshots of your system (Snapper, Timeshift), but none of them can tell you *what* changed between two snapshots, *what caused* each change, or *which one* broke things.

osreflog records every change to your system's state together with its cause. It can rebuild the system as it was at any point in that history and bisect it the way `git bisect` searches commits.

## What works today

Real output after `sudo dnf remove figlet`, with osreflog installed:

```
$ osreflog diff @{1}..@{0}
from c000002 (pre-dnf, 2026-10-15 10:04:17)
to   c000003 (post-dnf, 2026-10-15 10:04:22)
     1 config · 1 state · 77 package · 32 noise · seen by: state · 733 commands, 94 KiB stream, 5638 ms, cached
     dnf #27 dnf remove -y figlet (uid 1000, ok)

packages (1):
  remove    figlet.x86_64  2.2.5-39.fc44

files (2):
  ~ /etc/ld.so.cache  [config file content,mode,owner,xattr,times]
  ~ /var/lib/rsyslog/imjournal.state  [state file content,mode,owner,xattr,times]

109 package-owned and noise paths hidden (--all to show)
```

`osreflog log` lists checkpoints with what changed since the one before. `osreflog diff` with no range shows what changed since the newest checkpoint. References work the way git's reflog does: `c42`, `@{1}`, `@{now}`, `@{boot-1}`, `@{yesterday}`. Measured on this laptop: the cached diff takes 6 ms, osreflog adds under a second to a dnf transaction, and the idle daemon uses 0.0 % CPU and 26 MB of RAM ([P2 journal](docs/journal/p2-slice.md)).

Development install (Fedora on btrfs):

```
cargo build --release
sudo scripts/install-dev.sh      # --uninstall to remove; history is kept
osreflog doctor
```

## Where it's going

The design target, not working yet:

```
$ osreflog log --since yesterday
#2214  10:12  dnf upgrade (12 packages)            by khaledsaeed18 via sudo
#2215  10:13  /etc/NetworkManager/conf.d/wifi.conf  by tlp.service
#2216  14:02  org.gnome.desktop.interface           by gnome-control-center

$ osreflog why /etc/NetworkManager/conf.d/wifi.conf
-wifi.powersave = 2
+wifi.powersave = 3
 changed 10:13 by /usr/sbin/tlp (tlp.service), during cause #2214

$ osreflog bisect run --good @{boot-3} --bad @{now} -- ./wifi-ok.sh
probe 1/4  #2211  good    (nspawn, 3.1 s)
...
first bad cause: #2215  /etc/NetworkManager/conf.d/wifi.conf by tlp.service

$ osreflog undo #2215 --dry-run
```

## How it works

```
                  ┌────────────────────── osreflogd (root, Rust) ──────────────────────┐
 live witness ──► │ fanotify (fs-wide mark) + eBPF exec/fork lineage ──► cause builder │
                  │ dnf5 actions hook · dnf5 history db · systemd units · D-Bus        │
                  │                              │                                     │
 state witness ─► │ btrfs checkpoints (read-only snapshots) ──► send-stream diff       │
                  │                              ▼                                     │
                  │        ledger (SQLite, hash-chained) + content blobs (zstd)         │
                  └────────┬──────────────────────┬──────────────────────┬─────────────┘
                           │ D-Bus + polkit       │                      │
                     osreflog CLI          bisect probes:         GNOME app (later)
                                         nspawn · KVM VM · metal
```

- **Two witnesses.** A live witness records *who* changed each file while it happens (fanotify, plus eBPF to trace each change back to the process and package transaction behind it). A state witness proves *what* changed between btrfs snapshots, using the snapshot comparison stream that `btrfs send --no-data` produces. When the two disagree, that's a finding: for example, a change made while the daemon wasn't running.
- **Causes, not just files.** Changes are grouped into causes: a dnf transaction, a systemd service, a terminal command, an app writing a setting. You bisect, blame and undo whole causes.
- **Bisect at cause level.** Snapshots are coarse. The ledger can rebuild the states *between* snapshots (the state after cause k), so bisect narrows down to a single cause, not just a single snapshot.
- **Three probe tiers.** An ephemeral `systemd-nspawn` container on a snapshot (seconds), a KVM VM that boots the snapshot's own kernel over virtiofs (tens of seconds), or a one-shot reboot into the snapshot on real hardware (minutes; for hardware-only regressions).

Built on: **btrfs** (snapshots, send streams, reflinks), **fanotify**, **eBPF** (aya), **dnf5/RPM**, **systemd** (nspawn, units, mount namespaces), **KVM/QEMU + virtiofs**, **D-Bus + polkit**, **dconf**.

## Scope

Fedora Workstation on btrfs (the default since Fedora 33). It needs a single btrfs filesystem for `/`. Immutable systems such as Silverblue and NixOS already keep their own history, so they are out of scope.

## Documentation

Start at [docs/README.md](docs/README.md).

## License

[GPL-3.0-or-later](LICENSE)
