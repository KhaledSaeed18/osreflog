# P8: GNOME app, packaging, hardening

- Status: **P8a done; P8b built** (needs a click-through on the real desktop)
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

### 2026-10-16: the offline update (Q4) and a noise fix

GNOME Software's offline update ran (kernel 7.2.9 → 7.2.10, 7.1.6 removed, Firefox 158, drainscope 1.0.0, 44 packages in all) before the GTK headers went in.

- **Q4 answered.** In the offline boot, `dnf5` ran the actions hook: "osreflog: no pre-transaction checkpoint: osreflogd is not running". osreflogd is wanted by `multi-user.target` only, and an offline update boots `system-update.target`. So no checkpoints bracketed the update and no live causes were recorded for it, but nothing was lost: the next boot checkpoint (c000133) has the whole update in its diff and the dnf history import names it ("dnf #52 dnf5daemon-server (uid 0, ok)"). Fix: `osreflogd.service` is now also `WantedBy=system-update.target` and `Before=dnf5-offline-transaction.service packagekit-offline-update.service`. It takes effect at the next `install-dev.sh` (which re-enables the unit) and is confirmed by the next offline update. [open-questions.md](../open-questions.md) R11.
- **2,582 "package changes" between two boots were access times.** The probe-day boot checkpoints counted 693 and 2,582 package changes with no package transaction. All were `modify` with only `times` changed: `relatime` access times on files a boot reads (libwacom data, GStreamer plugins, Python modules, icons). Coverage already left them out (`Change::times_only`); the class counts didn't. `ClassCounts::add_change` and the ledger's summary query now count a times-only change as noise, so old intervals read right too. Test: `times_alone_count_as_noise`.

### 2026-10-16: P8a, the history browser

- `gtk4-devel` and `libadwaita-devel` installed (70 packages with the `glib2` upgrade), and `install-dev.sh` re-run: osreflogd is now linked into `system-update.target.wants` (the Q4 fix is live).
- **`crates/client`**: the osreflogd proxy moved out of the CLI, with typed read calls (`status`, `causes`, `show`, `log`, `diff`) and a `ClientError` that tells "the daemon isn't running" from everything else. The CLI uses it.
- **`bins/osreflog-app`** (gtk4-rs 0.11, libadwaita 0.9): `AdwNavigationSplitView` with the timeline (causes by local day, newest first; kind icon, label, change count, package files, time) and the selected cause (process, command, D-Bus caller, cgroup, time window, package files; each change with what happened, by which process, its size, and its summary lines in an expander). The window title carries the daemon's status line. D-Bus calls run on a tokio thread of their own; the GTK loop awaits their replies over a channel. It asks for new causes every 5 s and rebuilds the timeline only when they changed. `view.rs` holds everything the window shows as plain data: 3 tests.
- **Seen running against the real daemon** (GTK's Broadway backend, viewed in a browser): today's causes listed; #751 `uresourced.service` with its D-Bus caller (`systemd … SetUnitProperties`) and 8 changed files; #752, the `dnf install` of the GTK headers, with 31,422 package files counted and its one live change (`/etc/ld.so.cache` by `ldconfig`). With `systemctl stop osreflogd` the timeline gave way to "osreflog isn't running" within a refresh; with `start` it came back by itself, a new cause (systemd, from the restart) already at the top.

**P8a exit met.** Rough edges for later slices: long commands wrap to three lines in the timeline; the status line is cut off in a narrow sidebar; no search or filter yet.

### 2026-10-16: P8b, "what changed since…"

- The sidebar has two views (an `AdwViewSwitcher`): **Causes** and **Checkpoints**. The checkpoint list (from `Log`, 300 newest) is grouped by day; each row has the reason (boot, pre-/post-dnf, manual, pre-undo), the note or the package manager, and what changed in the interval before it ("before it: 77 config · 1848 state · … · dnf #52").
- Selecting a checkpoint asks `Diff(cN, @{now})` and shows "Comparing cN with now…" while it runs (4 s for a busy interval). The diff page: an **Until** choice (now, or any newer checkpoint, so any two can be compared); **Include package files and noise** (`all`); the facts (from, to, counts, how much the live witness explains, package transactions, every window osreflogd wasn't watching); the package changes; then the files by class (configuration, state, elsewhere, then package files and noise when asked), each with its summary lines and, when a cause made it, a button to that cause. At most 400 files per group are listed; the rest are counted.
- A reply for anything but the current selection is dropped, so a slow diff can't overwrite a cause picked meanwhile.
- **Found on the way:** libadwaita rows parsed a command's `&&` as markup ("Entity did not end with a semicolon"), whichever order the builder was given `use-markup` and the text. Rows are now made with setters in a fixed order (`set_use_markup(false)`, then the text): 0 warnings on a diff of 74 config files and on a cause whose command has `&&`, and the text shows as written.
- `view.rs` gained the checkpoint rows, the diff facts, the package lines and the file groups: 5 tests.
- **Seen against the real daemon**: c000134 → now: 74 config · 2 state · 11,966 package · 171 noise; "28/28 config changes, 9816/9816 overall have a recorded cause"; dnf #53; the two windows osreflogd wasn't watching today; 68 packages. GTK's Broadway backend drops clicks and scrolls, so the page was opened with `OSREFLOG_APP_OPEN=since:c000134` (a development hook, also `cause:<id>`). Still to see on the real desktop: the Until choice, the switch, the cause buttons, scrolling a long diff.
