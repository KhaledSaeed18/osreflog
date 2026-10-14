# osreflog

**A reflog for your Linux system: see what changed, who changed it, and bisect the change that broke it.**

> **Status: Phase 1, measuring by hand.** Design docs and P1 experiment scripts only; no Rust code yet. The commands below are the design target, not a working tool. See the [roadmap](docs/roadmap.md).

"It worked last week." Your Wi-Fi drops, suspend stops working, or an app won't start, and something changed in between. A package update, a config edit, a GNOME setting, a firmware update, a command you ran at 2 a.m. Linux can take snapshots of your system (Snapper, Timeshift), but none of them can tell you *what* changed between two snapshots, *what caused* each change, or *which one* broke things.

osreflog records every change to your system's state together with its cause. It can rebuild the system as it was at any point in that history and bisect it the way `git bisect` searches commits.

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
 live witness ──► │ fanotify (scoped marks) + eBPF exec/fork lineage ──► cause builder │
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
