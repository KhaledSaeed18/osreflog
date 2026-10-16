# P8: GNOME app, packaging, hardening

- Status: **P8a–P8d done**; P8e (packaging) next
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
- **Seen against the real daemon**: c000134 → now: 74 config · 2 state · 11,966 package · 171 noise; "28/28 config changes, 9816/9816 overall have a recorded cause"; dnf #53; the two windows osreflogd wasn't watching today; 68 packages. GTK's Broadway backend drops clicks and scrolls, so the page was opened with `OSREFLOG_APP_OPEN=since:c000134` (a development hook, also `cause:<id>`). On the owner's desktop (Wayland, 2026-10-16) the checkpoint list, the Until choice, the switch and the cause buttons all work. **P8b done.**

### 2026-10-16: P8c, undo from the app

- `crates/client` has typed `undo_plan` and `undo`; the CLI's line diff moved to `osreflog_undo::textdiff`, so the app shows the same previews.
- A cause's page has **Undo…**: the dry-run preview (`UndoPlan`; nothing written) lists each path with what undo would do (put back, with the text diff for a text file; remove to the attic; already as before; changed since, left alone; never undone), the paths undo never touches, and for a package transaction the `dnf5 history undo` command to run. **Undo N changes…** asks once more (a checkpoint first, every path checked again as it's written, replaced versions to the attic, the undo itself undoable), then sends `Undo(id, digest)`: exactly the plan shown, and polkit asks for an administrator's password. The result page shows the pre-undo checkpoint, the attic, each path's outcome, and opens the undo's own cause.
- `view.rs`: the preview rows, the summary line ("Nothing has changed yet: 2 to put back, 1 changed since (left alone)", or "Nothing to put back: …"), the result rows: 6 tests.
- **Checked:** the preview of #765 (`cupsd`) matched `osreflog undo 765` (two conflicts: cupsd rewrote both files since). Then, on the owner's desktop, a test change (#773 created `/etc/osreflog-p8c.conf` with `x = 1`, #774 `sed` made it `x = 2`) undone from the app: GNOME's polkit dialog asked for the password, the result page showed it put back, and from the CLI: the file holds `x = 1`, checkpoint c000136 "pre-undo, before undoing #774" was taken first, and the undo is cause #775 ("x: 2 → 1").

### 2026-10-16: P8d, the bisect wizard

- `crates/client`: typed `bisect_run`, `bisect_status`, `bisect_verdict`, `bisect_rearm`, `bisect_reset`.
- The sidebar has a third view, **Bisect**: the current or last session (its range, tier and state) and **New bisect…**. The **form**: good and bad checkpoints, who decides (a test, or the person), how copies run (whatever the test needs, containers, VMs with their own kernel, reboots of this machine), what the test needs (the kernel, this machine's hardware), booting systemd in the container, and the test command with the exit-code contract. `view::bisect_args` turns it into the daemon's `Args` and refuses what can't work (a metal probe judged by a test, a VM judged by hand, no test, the same checkpoint twice).
- The **session page** follows the session every second while it runs: the probe that waits with its how-to lines and **Good / Bad / Skip** (one click per probe: the buttons go insensitive at the first), **Reboot now…** for an armed metal probe (asks first, then `systemctl reboot`), **Arm it again** for one that came back without a verdict; what was found, with **Open #id**; every probe with its verdict, time, qemu memory, approximations and output; the log; **Stop this bisect…** / **Forget it**. A refused action (polkit, a busy session) is said in a dialog.
- `view.rs`: the form's rules, the session's title, state, probe rows and finding: 8 tests in all.
- **On the owner's desktop**, with a seeded regression (c000137 "p8d: good" … c000138 "p8d: bad"; #779 `# tuned`, **#780 `mode = broken`**, #781 `timeout = 30`):
  - **automatic** from the app (test `! grep -q 'mode = broken' …`): bad end bad, after #779 good, after #780 bad: **#780**, probes of 0.3–0.4 s;
  - **by hand** from the app (containers): the first attempt took a mistaken **Bad** on probe 2 and ended, as it must, on #779; run again with the right verdicts: bad, good, bad: **#780** in 3 probes.
- Fixed on the way: `Gtk-CRITICAL gtk_widget_is_ancestor` on every page rebuild (a clicked button removed while focused: focus now leaves the page first); a double click giving two verdicts (once harmless, but a slower one could land on the next probe); the "good" icon (`emblem-ok-symbolic` isn't in Adwaita: `object-select-symbolic`); "Checkpoints" cut off in the switcher (narrow policy, sidebar at least 380 px).
- **A gap the mistaken verdict showed:** a verdict can't be taken back. `git bisect` replays its log for that; here the session keeps every verdict, so the search can be rebuilt without the last one.

### 2026-10-16: taking back a verdict

- `TwoLevel::take_back` drops the last verdict and rebuilds the search from the ones before it, so the probe it answered is asked again; a finished search reopens. Tested on the P8d slip and, for every culprit position up to 6 checkpoints × 5 causes, by taking every verdict back one at a time.
- osreflogd: `BisectUndoVerdict` (polkit `…bisect`). In a running manual session the waiting probe is released first (container stopped, `/boot` disarmed, clones deleted); a finished one is reloaded from the ledger and resumed. Automatic sessions are refused (their verdicts came from a test). CLI: `osreflog bisect undo-verdict`; the app: **Take back the last verdict** on a manual session's page.
- **Found on the laptop:** the first take-back, on the P8d session saved by the daemon before this change, asked about #779 again instead of #780. That save had no cause count, so the replay couldn't answer "how many causes?" and dropped the good verdict for #779; the person's Bad then went to the wrong probe and the bisect ended on #779. Fixed: the count is read off any cause probe in the history when it wasn't kept. Test: `a_session_saved_without_the_cause_count_takes_back_right`. Nothing on the system was touched; probes are throwaway copies.
