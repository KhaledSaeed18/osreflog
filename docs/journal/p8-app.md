# P8: GNOME app, packaging, hardening

- Status: **pre-flight done, first slice planned** (P7 closed 2026-10-16)
- Started: 2026-10-16
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

## Pre-flight, second pass (2026-10-16, after P7)

- **To install** (one transaction): `sudo dnf install --setopt=install_weak_deps=False gtk4-devel libadwaita-devel`. dnf resolves **68 packages, 29 MiB to download, 164 MiB on disk**, and upgrades `glib2` 2.88.3 → 2.88.4 (the headers need the matching library). 163 GiB free on `/`. Nothing else: `blueprint-compiler`, `mock` and meson can wait for packaging.
- **A pending offline update.** dnf warns that installing anything now invalidates "a pending offline transaction initiated by dnf5daemon-server": GNOME Software has downloaded an update for "Restart & Install" (not yet triggered: no `/system-update`). That is exactly the case **Q4** has waited for since P2 (does the dnf hook run in an offline update, and is osreflogd up?). Proposed order: apply that update first from GNOME Software, check what osreflog recorded for it (`osreflog log`, `osreflog causes`), then install the headers.
- **The API the app needs exists.** Read methods (`Status`, `Log`, `Causes`, `Show`, `Diff`, `Why`, `BisectStatus`) are polkit `…read`, allowed for the active session without a password; `Checkpoint`, `Undo`, `Bisect*` ask through polkit as they do for the CLI. The app needs no new privileged code.

## Plan: the first slice (P8a, read-only)

**Goal:** a GNOME app that shows this machine's real history: a timeline of causes, and for each the files it changed with their semantic summaries, before and after. No writes, no password prompts.

| Piece | What |
| ----- | ---- |
| `crates/client` | The `zbus` proxy moves out of the CLI into a shared crate, with typed calls that return `osreflog_model` types (`causes() -> Vec<Cause>`, `show(id) -> CauseDetail`, `status() -> Status`, …). The CLI switches to it, so the app and the CLI can't drift apart |
| `bins/osreflog-app` | gtk4-rs + libadwaita-rs. `AdwApplicationWindow` with an `AdwNavigationSplitView`: the sidebar lists causes newest first, grouped by day (time, kind, label, how many paths); the content shows the selected cause (`Show`): actor, command or unit, each changed path with its semantic summary and the before/after text the daemon already serves (secrets never stored, so never shown). A header line from `Status`: live witness on or off, last checkpoint, gaps |
| Async | D-Bus calls on a small tokio runtime in a background thread (the workspace's `zbus` uses tokio), results handed to the GTK main loop over a channel; the window never blocks on the daemon. New causes: a poll every few seconds while the window is visible (the daemon has no change signal yet; adding one is a later slice) |
| Daemon down | An `AdwStatusPage` "osreflog isn't running" with the `systemctl` line to start it, instead of an empty list |
| Tests | The pure parts (grouping by day, labels, the summary lines) as unit tests in the app crate; the window itself checked by running it here against the real daemon |

**Exit for P8a:** the app opens on this laptop, lists the real causes of the last week, and shows any cause's changes; it works with the daemon stopped (says so) and started again (recovers).

**Later slices**, one at a time: **P8b** "what changed since…" (`Diff` between two checkpoints, with a picker); **P8c** undo with the dry-run preview (`UndoPlan`, then `Undo` by digest, polkit asks); **P8d** the bisect wizard (`BisectRun` / `BisectStatus` / `BisectVerdict` / `BisectRearm`, metal included); **P8e** packaging (RPM spec, desktop file, metainfo, icon; local `rpmbuild`, then COPR); **P8f** hardening (system call filters, the SELinux policy module, the transient units' narrowest settings); the user scope (dconf history from `osreflog-user`) where it fits.

**Decided here (the owner can overrule):** gtk4-rs and libadwaita-rs, code-built widgets (no Blueprint), a D-Bus client only. Crate versions are pinned to what builds against GTK 4.22 and libadwaita 1.9 when the slice starts.
