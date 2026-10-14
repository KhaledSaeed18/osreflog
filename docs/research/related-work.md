# Related work

What exists, what each does well, and the gap osreflog fills.

| Tool | What it does | What it lacks for this problem |
| ---- | ------------ | ------------------------------ |
| **Snapper** (+ dnf plugin, btrfs-assistant, snapper-gui) | btrfs/LVM snapshots, pre/post pairs around package transactions, timeline, `snapper diff`, rollback | No attribution beyond "this dnf command". Diffs are file lists. No bisect. Rollback is whole-subvolume |
| **Timeshift** (Linux Mint) | rsync or btrfs system snapshots, simple restore UI | No diff semantics, no causes, no bisect |
| **snapm** (+ Boom) | snapshot *sets* across volumes, bootable snapshots via Boom entries | Same: state only, no attribution. Its bootable-snapshot entries are prior art for the metal tier |
| **etckeeper** | `/etc` in git, committed around package transactions | `/etc` only; changes between transactions are lumped together without attribution; no packages, no boot config |
| **NixOS generations / rpm-ostree / bootc** | immutable or declarative systems with bootable generations | The answer by design, but only for systems built that way. osreflog targets the mutable Fedora Workstation most people run |
| **auditd** (`-w path -p wa`) | kernel-level attribution of writes | Events without state: no content, no diff, no undo, no bisect; a log rather than a model |
| **fatrace** | fanotify-based CLI that prints file events with pid/comm | A great P1 measurement tool; no lineage, no persistence |
| **Tetragon / Tracee / Falco** | eBPF security observability, file-integrity rules | Built for detection on servers and in clusters, not for desktop history or bisect |
| **Windows System Restore / "Last Known Good Configuration"** | restore points before installs; a boot option for the last config that booted | Coarse, no explanation of what changed; "last known good" inspires `@{boot-1}` |
| **macOS Time Machine** | hourly local snapshots, browsable history | User data first; no system-change attribution |
| **git bisect / mozregression / kernel ktest** | binary search over versions with a test | The model for osreflog's bisect UX and exit-code contract; they work on builds and commits, not on a running OS's history |

## Prior art to read before P2 and P6

- btrfs send stream format v2 (btrfs-progs `Documentation/dev/dev-send-stream.rst`) and `common/send-stream.c`
- fanotify(7), fanotify_init(2), fanotify_mark(2), especially `FAN_REPORT_DFID_NAME_TARGET` and `FAN_REPORT_PIDFD`
- `libdnf5` actions plugin documentation (`man libdnf5-actions`)
- systemd-nspawn(1) `--ephemeral`, `--volatile`; systemd.exec(5) sandboxing options
- Boom / snapm design notes (bootable snapshots on Fedora/RHEL)
- mozregression's bisection strategy (handling skips and build gaps)
