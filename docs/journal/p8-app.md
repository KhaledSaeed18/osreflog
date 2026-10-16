# P8: GNOME app, packaging, hardening

- Status: **pre-flight only** (P7 isn't closed: P7b's VM run and P7d on the laptop come first)
- Roadmap: [P8](../roadmap.md#p8-gnome-app-packaging-hardening)
- Exit: installable from COPR on a fresh Fedora 44 VM; `osreflog doctor` is clean; the app shows a real week of history.

## Pre-flight (2026-10-16, overnight, no root)

| Item | Found | For |
| ---- | ----- | --- |
| GTK runtime | `gtk4` 4.22.5, `libadwaita` 1.9.4 installed | the app runs here |
| GTK headers | `gtk4-devel`, `libadwaita-devel` 4.22.5 / 1.9.4 **not installed**, available in `updates`; no `glib-2.0.pc` either | gtk4-rs / libadwaita-rs build: `sudo dnf install --setopt=install_weak_deps=False gtk4-devel libadwaita-devel` (small, but the network is slow: P1 measured 60–125 KiB/s) |
| UI files | `blueprint-compiler` not installed | optional: plain GtkBuilder XML or code-built widgets need nothing |
| Packaging | `rpm-build` 6.0.2, `copr-cli` 2.7 installed, `~/.config/copr` present; `mock` not installed | a local `rpmbuild` first; mock for clean-room builds, then COPR (like drainscope) |
| Build system | `meson` 1.11.2 | only if the app's data files (desktop file, icons, metainfo) go through meson; cargo alone can do without |

### Unit hardening (`systemd-analyze security --offline=true`, as a user)

| Unit | Exposure | Notes |
| ---- | -------- | ----- |
| `osreflogd.service` | **3.6 OK** | runs as root by design (btrfs ioctls, fanotify, BPF); P8's work is a system call filter and the SELinux policy |
| `osreflog-boot-doctor.service` | **4.2 OK** | root, writes only `/boot` |
| `osreflog-user.service` | 8.0 "EXPOSED" | analyzed offline as if it were a system unit run as root; as a user unit most of these options don't apply (no capabilities to drop). Re-check it with `systemctl --user` semantics in P8 |

The two transient units osreflogd starts (the probe runner and the boot worker) aren't in this table: they're defined in code (`bisect.rs`, `start_unit`) with `PrivateMounts`, `Delegate`, a memory cap and a runtime cap, and run unconfined because nspawn, qemu and `/boot` writes need it. P8 should give them the narrowest set each still works with, and test each tier after.

### What P8 needs decided first

- **App stack:** gtk4-rs + libadwaita-rs (same language as the rest, and the D-Bus client types already exist in `crates/model` and `crates/bisect`), talking to osreflogd over the same system-bus API as the CLI. No second privileged component: the app is a D-Bus client and polkit asks for the password.
- **What it shows first:** the timeline of causes and a cause's semantic diff, both already served (`Log`, `Show`, `Diff`). The bisect wizard can drive `BisectRun` / `BisectStatus` / `BisectVerdict` / `BisectRearm` as the CLI does; metal probes need nothing new on the app's side.
