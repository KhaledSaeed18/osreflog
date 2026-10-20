# P8: GNOME app, packaging, hardening

- Status: **P8a–P8e done** (P8e installed from COPR on a fresh Fedora 44); P8f (hardening) next
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
- **Retested on the laptop** with the fix installed: a by-hand bisect from the app, bad / good / bad: #780; **Take back the last verdict**: "Bad for after #780 taken back: that probe is asked again", the same probe (after #780) asked again, Bad: #780 again, 3 probes counted.

### 2026-10-16: P8e, packaging

- **`data/` names the packaged paths**: the units run `/usr/libexec/osreflog/osreflogd` and `osreflog-user`, the dnf hook calls `/usr/bin/osreflog`. `scripts/install-dev.sh` and the P7 VM scripts rewrite them to `/usr/local` as they install.
- **`data/app/`**: the desktop file, AppStream metainfo (release 0.1.0) and the icon (a clock with a counter-clockwise arrow, the newest point orange) with its symbolic version; `desktop-file-validate` and `appstreamcli validate` pass.
- **`packaging/osreflog.spec`**: two packages. `osreflog` has the daemon, boot doctor, user agent, CLI, units, D-Bus and polkit policies and the dnf actions file (`%config(noreplace)`); it Requires libdnf5-plugin-actions and grub2-tools-minimal and Recommends dracut, systemd-container, qemu-kvm-core and virtiofsd. `osreflog-app` has the GTK app. The build is offline, from vendored crates; `%check` validates the desktop file and metainfo and runs the workspace tests. The units follow Fedora's presets on install (enabled by hand) and restart at the end of an upgrade's rpm transaction, not halfway through it.
- **`scripts/dist.sh [--rpm]`**: from a clean tree, the source tarball (git tree, vendored crates, `LICENSE.dependencies`) and the SRPM, and with `--rpm` the binary RPMs built with rustup's toolchain. It refuses to build when the spec's Version or License disagrees with the workspace; the License is computed from the crates the four binaries link, plus SQLite's blessing (bundled by rusqlite).
- **Local build**: both packages, the tests passing in `%check`.
- **COPR** ([khaledsaeed18/osreflog](https://copr.fedorainfracloud.org/coprs/khaledsaeed18/osreflog/), fedora-44-x86_64, Fedora's Rust 1.99): the first build failed in `%check`. `seeds_from_proc` wanted more than 10 processes in `/proc`, and mock's PID namespace has a handful. It now checks only that the table has the test's own process. Every test then passed in a PID and network namespace of their own (`unshare --pid --net`, as mock runs them), and build 11160993 succeeded.
- **Installed from COPR on a fresh Fedora 44** (2026-10-17, `scripts/p8/coprvm.sh`: Fedora's Cloud Base 44 image, btrfs with `root`, `home`, `var` and `boot` subvolumes, cloud-init, two boots). `dnf copr enable` and `dnf install osreflog osreflog-app` worked and `rpm -V` was clean. The units were disabled by the presets, as expected, and enabled by hand. The doctor gave 5 ok. `dnf install figlet` got its pre-dnf and post-dnf checkpoints and a cause with 360 package paths. `systemd-analyze security` rated osreflogd 3.6 ("OK"). After the reboot, the boot doctor and osreflogd started at boot and took the boot checkpoint. `dnf remove figlet` got its own pair of checkpoints. `verify` passed on 21 chain entries, and the doctor again gave 5 ok. The journal had one warning, the expected witness gap across the reboot. On the way, two bugs in the harness, not osreflog: a login prompt on ttyS0 hung up the test's writes to the same port (the test now writes to a virtio port of its own), and the test unit's `After=cloud-final.service` made an ordering cycle at the second boot.
- **P8 exit**: installable from COPR on a fresh Fedora 44 VM with a clean doctor: **met**. The app showing a real week of history: still to come (the laptop's history keeps growing).

### 2026-10-17: the timeline's rough edges

- **Long commands** no longer wrap to three lines: cause and checkpoint rows keep their title and subtitle to one line each, ellipsized, with the whole text in the row's tooltip.
- **The status line** in the sidebar's title is now short ("recording · 139 checkpoints"); the full line (last checkpoint, its time and reason) is its tooltip.
- **Search**: a search box above the causes keeps the ones whose label, kind, process name, command or `#id` holds every word typed, ignoring case; "No matching causes" when none does. The filter holds across the 5 s refresh. `view::matches`, `view::status_short`: 9 tests in all. Development hook: `OSREFLOG_APP_OPEN=search:<text>`.
- **Seen against the real daemon** (Broadway): `search:dnf` listed the dnf transactions and the `sudo sh -c dnf install …` commands, one line each; `search:zzqq` showed the empty page, still there after a refresh.

### 2026-10-17: a pass over the look and feel

- **Where you are**: the sidebar row of what the content side shows (a cause, its undo, a checkpoint's diff, the bisect session) is tinted with the accent color, kept across refreshes and searches.
- **Loading**: libadwaita's animated spinner (`AdwSpinnerPaintable`, so the app needs libadwaita 1.6; the spec says so) instead of the static `content-loading` icon, on the sidebar's first read and on the content side. Picking a cause shows the loading page only when the answer takes more than 200 ms, so a cause read in a few ms no longer flashes; on a narrow window the content slides in at once, with the loading page.
- **Motion**: the sidebar and content stacks crossfade; a new page fades in (180 ms) and starts at the top instead of the last page's scroll position. A running bisect's page, refreshed every second, neither fades nor jumps.
- **Page headers**: every page opens with an icon (tinted green for an undo or bisect that succeeded, amber for one that needs you or went wrong), a title kept to two lines (the whole of it in the tooltip) and one line of facts ("#843 · unit · 1 change · 2026-10-17 17:49:05"). The heading used to be the cause's whole command, three lines of bold. The header bar says only "#843". **Undo…** sits in the cause's header. The command is in monospace, and "Kind", already in the header, is gone from the facts.
- **Spacing**: 24 px between a page's groups (18 before), 24 px at the top, 36 at the bottom; buttons sit in rows of their own (Open #780 and Take back the last verdict side by side).
- **Feedback**: toasts for a verdict sent, a probe armed again, a verdict taken back, an undo done, and the daemon answering again after it was down.
- **Keys**: Ctrl+F goes to the causes and their search; Escape clears it; F5 or Ctrl+R refreshes now.
- **Bisect**: the session's header says "Found #780 in 3 probes" (the finding below names it); the sidebar row keeps to two lines each.
- **Seen** (Broadway, against the real daemon): a cause with its highlighted row, an undo preview, a diff's spinner then its page, a finished bisect; no GTK warnings. Not seen: the narrow window (Broadway's window can't be resized from the browser) and the keys and toasts (it drops input); for the owner's desktop.

### 2026-10-17: the README as the project's front page, with screenshots

- **README** rewritten around what osreflog is for a reader who doesn't know it yet: git's tools for the whole system (a table from `git reflog`, `diff`, `log -p`, `show`, `revert`, `bisect run` to their osreflog commands), a tour with real output (`why`, `diff`, `bisect log`), the app with four screenshots in light and dark, "safe by construction" (dry runs, conflicts left alone, the attic, checkpoint first, throwaway probes, secrets never stored, the hash chain), how it works, the numbers measured in the phase journals, install and scope. The old status line (P7) and the "design target, not working yet" section, most of which works now, are gone.
- **Screenshots** from this laptop's real history, reproducible: `scripts/screenshots.sh` runs the app on an off-screen Broadway display with `OSREFLOG_APP_SCREENSHOT` (render the window to a PNG at 2×, then quit) and `ADW_DEBUG_COLOR_SCHEME` for light and dark. `OSREFLOG_APP_OPEN` takes several items now (`search:p8c,cause:774`). Animations are off in that mode: with no browser attached Broadway's frame clock doesn't run, and the first capture stayed on the fading loading page. The metainfo lists the screenshots (GNOME Software shows them).
- **Found while capturing:** line diffs in previews were plain text; removed lines are red, added lines green and hunk headers dimmed now (`view::line_tone`), and a probe's verdict icon is green or red.
- **Found while measuring:** the daemon at rest uses 0.08 % of one CPU (5 ticks in 60 s), but holds 108 MB of anonymous memory after a day with several large diffs (P2 measured 26 MB before the live witness and the app). That goes to the performance work: what holds it, and whether it comes back.

### 2026-10-17: P8f, hardening

- **System call filters.** `osreflogd` and the boot doctor refuse `@clock @cpu-emulation @debug @module @obsolete @raw-io @reboot @swap` with `EPERM`. An allow-list (`@system-service`) is too narrow for a daemon that mounts, sets owners and capabilities, and sets its workers' rlimits; the undo and summary workers keep their own allow-list filter and Landlock on top. `@debug` is safe to refuse because the lineage program attaches through `bpf()` links (`tp_btf`), not perf events. Exposure: osreflogd 3.6 → **2.4**, the boot doctor 4.2 → **2.2**.
- **`UMask=` left out** for the daemon: the undo engine creates intermediate directories with a fixed 0755 and restores modes explicitly only on files, so a umask could leave a restored directory 0700. Worth doing once the write gate has a case for it.
- **The user agent sandboxed**, tested as a user drop-in before it went into `data/`: read-only system and home except `~/.local/state/osreflog`, private /tmp and devices, no network, no capabilities, `@system-service` minus `@privileged @resources`, `UMask=0077`. In a user unit these imply `PrivateUsers=yes`; reading your own processes' `/proc` still works. `scripts/p4/user-workload.sh`: **8/8**. Exposure 8.0 → **1.0** (offline; 0.8 for the running unit). `IPAddressDeny=` came out again: a user manager can't apply it.
- **`MALLOC_ARENA_MAX=2`** in osreflogd's unit, for the 108 MB found earlier today: a hypothesis (glibc's per-thread arenas on the blocking pool) until measured.
- **`osreflog-selinux`** (`data/selinux/`): types for the ledger, blobs and attic (`osreflog_var_lib_t`) and the runtime directories (`osreflog_runtime_t`), so confined services can't read them, and `osreflog_read_lib_files()` for a policy that needs to. The daemon stays `unconfined_service_t`: a domain for a process that reads every file and, on undo, writes any of them would mostly be a label. A narrow domain for the undo and summary workers (one writes, one parses untrusted content) is the meaningful next step. The module compiles here and in CI; the spec installs it with Fedora's `%selinux_modules_install` and relabels.
- **CI**'s ratchet now holds the new scores, the user unit included, and compiles the module.

**Runbook (root, on the laptop):**

```bash
cargo build --release && cargo test --release -p osreflog-undo --no-run
sudo scripts/p8/hardening-check.sh 2>&1 | tee perf-out/hardening-check.log
```

It installs the build, then runs the doctor, the P3 testbed (capture and lineage under the filter), the P5 write gate and live undo, the P6 seeded bisect (containers and a VM), the boot doctor, the daemon's memory around two large diffs, and lists anything the filter refused. Afterwards, the SELinux module from the release RPM: `sudo dnf install ./osreflog-selinux-0.2.0-1.fc44.noarch.rpm`, then `ls -Z /var/lib/osreflog` should show `osreflog_var_lib_t`, and the testbed again.

### 2026-10-17: the hardening check, as root

`sudo scripts/p8/hardening-check.sh` on the laptop (the units from main, the daemon binary from 2026-10-16, whose code hasn't changed since): install ok; the units carry the filter and `MALLOC_ARENA_MAX=2`; doctor ok; **write gate ok; live undo ok; seeded bisect 5/5** (nspawn and the VM); the boot doctor ok; and **nothing refused** by the filter in the journal. So `@debug` (ptrace, perf events) is safe to refuse: the lineage program loaded and attached.

- **One failure, in the harness:** the testbed's check wants a command cause labeled with `sed` or `testbed`, and run under the wrapper the session's top command is `sudo scripts/p8/hardening-check.sh`. Lineage was right (a `command` cause, coverage 9/9). The wrapper now says who it is (`OSREFLOG_CHECK_PARENT`) and the check accepts that; on the same results: 16/16, 0 failed.
- **Memory with two arenas:** 56 MB after all of the above, 79 MB after two more large diffs (`c000170..`). Lower than yesterday's 108 MB, but not the same work; the soak decides.

### 2026-10-20: the soak, again

- **The first soak measured two daemons and a shutdown.** The hardening check reinstalled and restarted osreflogd an hour into it, and the machine was shut down at 00:42 the next night, which stopped the user unit after 6.1 h. The summary glued the two daemons together: memory "fell" from 116 MB to 41 MB (the restart) and the live counter went negative (−15094, the new daemon's count minus the old one's).
- **`scripts/perf/soak.py`** now samples with wall-clock times and appends to one CSV, so a run started again with the same file carries on; it counts only the time it sampled (a gap longer than three intervals is the machine off), splits each process's samples into runs by pid and start time, adds CPU and live counters up per run, and reports memory an hour into the longest run and at its end. `--summary` prints the table from the samples so far. Read again this way, the first run says 223 live changes.
- **`scripts/perf/soak-unit.sh start`** installs the soak as a user unit enabled at login, which stops for good once the time is sampled (a `.done` file next to the CSV). The second run started 2026-10-20 02:09, a few hours after a boot, with osreflogd at 15 MB.

### 2026-10-20: Q3, what the snapshots hold

- **Measured from the diffs, without quotas.** The send-stream builder now keeps, for each object that existed before the interval, what the interval took from it (`Release`): all of it (deleted, or replaced by a new file renamed over it, which is how rpm and editors write), or the byte ranges written over in place (merged, at most 64 per file, then one span) and the size it was truncated to. The engine looks each one up in the older snapshot (`openat2` beneath it, no symlinks, no crossing mounts, then `fstat`): regular files only, a deleted name of a file with other links counts nothing, writes past the old end count nothing, and nothing counts more than the old file's allocated size. The sum is the interval's `released_bytes`, stored in a table of its own (ledger schema 6), not chained, since it's an estimate.
- **Bounds.** Every byte leaves `/` in exactly one interval, and it's in that interval's older snapshot. So what left in an interval starting at a kept snapshot is held for sure, and what left in any interval since the oldest kept snapshot is the most that can be held (some of it was written and removed between pruned checkpoints). `ledger::space::bounds` walks back from the newest checkpoint along the closest stored intervals; it stops at the first interval computed before this version, and says from which checkpoint it counted.
- **Reported** by `osreflog status` (a `space` line), the app's status tooltip, and `osreflog doctor`, which warns when the snapshots surely hold more than is free on `/`.
- **Retention is configurable**, so the warning has something to do about it: `/etc/osreflog/retention.conf` (`keep_all_hours`, `daily_days`, `weekly_weeks`), shipped with the defaults commented out, read at start; a wrong file is named in the journal and the defaults are used.
- **Quotas stay off**: full qgroups make every snapshot deletion slower, and simple quotas charge an extent to the subvolume that wrote it, so a snapshot never "holds" anything in their accounting. Q3 is resolved as R12.
- **Tests**: the builder's releases (in-place ranges with an append, a rename over, a delete, a truncation, a file under a moved directory), the range merge and its bound, the bounds walk (kept and pruned starts, older intervals, a hand-asked diff beside the eager ones), the size text, the retention file. The loopback test (`sudo scripts/p2/it.sh`, root) checks that 4 MiB deleted, 1 MiB written over and 1 MiB appended read as 5 MiB held: still to run.

### 2026-10-20: Q5, how early the daemon starts

- **Where it starts now**: right after `basic.target`, once D-Bus is up: on the last boot `osreflogd.service` started 6.7 s into userspace (kernel monotonic 10.97 s), about 1 s after dbus-broker. It can't go much earlier: `Type=dbus` needs the bus, the ledger needs `/var` and the BPF program needs `/sys/fs/bpf`; with `DefaultDependencies=no` it would race sysinit's own writes for a second at most. What boot changes before the daemon is up is in the boot checkpoint's diff anyway (the state witness), just without a cause.
- **What it did after starting cost more**: the boot checkpoint took 6657 ms (4467 ms the boot before; 60 ms to 2.7 s during the day), because creating a snapshot waits for btrfs to commit everything boot has written so far. The live witness started only after it, so for about 7 s of every boot nothing was attributed, and the gap record covered it.
- **Change**: the live witness starts first, then the boot checkpoint, then the bus name. Changes during the checkpoint now get causes; the gap ends when watching starts (about 0.3 s after the daemon starts, as on a restart). The bus name still waits for the checkpoint, so an offline update's `pre-dnf` checkpoint still comes after the boot one. Q5 is resolved as R13.
- **To see on the laptop** (root: install, then a reboot): `journalctl -b -u osreflogd -o short-monotonic` shows "live witness watching" before "checkpoint … (boot)".

### 2026-10-20: SELinux domains for the workers

- **Two domains** in `osreflog-selinux` (module 0.3.0): `osreflog_summary_t` for the summarizing worker (it parses content any local user can influence: pipes to the daemon, the loader and libraries, its own `/proc`, its sandbox's limits and capability drop, nothing else) and `osreflog_undo_t` for the undo worker (manage and relabel files and directories that aren't security policy, symlinks too, the attic, `chown`/`fowner`/`dac_override`, `setfscreate`; no network, no exec, no modules).
- **How a worker gets there.** Both are this same executable, so a transition on exec by file type can't tell a worker from the daemon, and relabeling the daemon's binary would change how systemd starts it. Instead osreflogd asks at exec: between fork and exec the child writes the worker's context (the daemon's own, with the type swapped) to `/proc/thread-self/attr/exec`. The policy lets only `unconfined_service_t` transition into them, with `nnp_transition` since the unit sets `NoNewPrivileges=`, entering from `bin_t` (packaged, `/usr/libexec`) or `usr_t` (development, `/usr/local/libexec`).
- **Never in the way**: without SELinux or without the module the context is invalid, the write fails and the worker runs as before; if the policy refuses the transition the exec fails, the spawn is tried again without it, and the daemon says so once and stops asking until it restarts. The first worker of each kind logs the context it runs in ("worker started as …").
- **Permissive** for now (`permissive` in the module): they log what they'd be denied and are denied nothing, so a missing rule can't break summaries or undo. The module compiles here (and in CI); written blind, it needs one run as root: `sudo scripts/p8/selinux-workers.sh` installs the module and the build, runs the testbed (summaries) and the live undo workload (undo worker), checks the journal for both domains, and collects the denials into `perf-out/selinux-avc.log` with `audit2allow`'s reading. Then the rules get what's missing, and the domains enforce.
- **Not done**: a domain for the daemon itself, for the reason given in P8f.

### 2026-10-20: Flatpak, as R4 said

- **Found by reading the ledger as a dataset** (the research note, next): of 43,183 path changes recorded in five days, 41,297 were in `/var/lib/flatpak` (appstream refreshes by the system helper and `flatpak update`), stored row by row as "unknown" causes with no explanation. R4 (2026-10-14) had decided they would be "recognized and summarised as one opaque `flatpak` cause, never diffed", and the README says so, but nothing did it.
- **Now**: the live witness gives every write under `/var/lib/flatpak/` to one `flatpak` cause, labeled with the first writer's name, and counts the paths without listing them (the same counter as a transaction's package paths); `show` and the app say "written in Flatpak's installation, not listed: `flatpak history` has the details". The state witness classes the directory as noise, so diffs hide it by default.
- **And units in sub-slices**: the same reading showed `cupsd`'s writes as "unknown" causes. Its cgroup is `/system.slice/system-cups.slice/cups.service`, and the unit was taken only from the component right under `system.slice`, so every templated unit (`system-getty.slice/getty@tty1.service`, `system-systemd\x2dcoredump.slice/…`) fell through as well. `system_unit` now skips sub-slices.

### 2026-10-20: what changes, the first reading

- **`scripts/research/month.py`** reads the ledger through the CLI as the desktop user (no root), leaves out osreflog's own tests and development installs, counts Flatpak apart, reapplies today's noise list to older records (read from the model's source), and prints an anonymised summary: who changes the system, what churns, checkpoints and packages.
- **[docs/research/what-changes.md](../research/what-changes.md)**: 4.6 days, 342 causes from the system and its user (74 a day), 77% of them services rewriting their own files; uresourced, tuned-ppd and cupsd at the top; 63% of paths changed once. The reading found the Flatpak flood and the units in sub-slices (above), and two more timers (DHCP leases, abrt's cursor), now noise. The month's reading replaces these tables from 2026-11-15.

### 2026-10-20: no UMask for the daemon, after all

P8f left `UMask=` out because the undo engine relied on the umask for some directory modes. Looked at again: what the daemon keeps is already private without it (the ledger and blobs live on the btrfs top level, mounted only in the daemon's namespace; the attic and `/run/osreflog` are 0700), while a umask of 0077 would also apply to what it makes for others to read: the probe clones' files, the cached VM initramfs, the files it hands nspawn and qemu. It would protect nothing new and risk the bisect tiers, so the unit stays without it.

### 2026-10-20: runbook for 0.3.0 (root, on the laptop)

Everything above that needs root, in one script, before 0.3.0 is tagged. The binaries are built (release, the undo gate's test binary, the loopback test):

```bash
sudo scripts/p8/check-0.3.0.sh 2>&1 | tee perf-out/check-0.3.0.log
```

It runs the loopback engine test (schema 6 and the 5 MiB space check), installs the SELinux module and this build and runs the testbed and the live undo workload with the workers in their domains (denials into `perf-out/selinux-avc.log`), the seeded bisect suite, then status and doctor. Then one reboot, and `journalctl -b -u osreflogd -o short-monotonic | head` should show the live witness watching before the boot checkpoint. The soak keeps running through all of it (the restarts are segments of their own); `scripts/perf/soak-unit.sh status` shows how far it is.

### 2026-10-20: 0.3.0's root checks, first run

`sudo scripts/p8/check-0.3.0.sh` (log in `perf-out/check-0.3.0.log`):

- **Loopback engine test: 2/2.** Schema 6 from scratch; the space report after 4 MiB deleted, 1 MiB written over and 1 MiB appended: at least = at most = **5,263,389 bytes**, 5 MiB and the 20 KiB of small files the scenario also replaces; after pruning to one snapshot, 0.
- **The module** built, installed and loaded; `/var/lib/osreflog` is `osreflog_var_lib_t`; doctor ok.
- **Both workers ran in their domains**: "worker started as system_u:system_r:osreflog_summary_t:s0" two seconds after the restart, and `osreflog_undo_t` for the live undo workload, which passed (15 checks, including the dnf undo). The script still said "no worker ran in it": `journalctl … | grep -q` under `pipefail` fails when grep stops reading early. It reads the journal once now.
- **One denial**, twice: `relabelto` by `osreflog_undo_t` on its temporary file with `unconfined_u:object_r:etc_t:s0`. Not a missing type rule (`files_relabel_non_security_files` has it) but the identity constraint: a restored label keeps the SELinux user the file had, and only exempt domains may set another user than their own. `domain_obj_id_change_exemption(osreflog_undo_t)`. The summarizer was denied nothing. With that, both domains enforce; the next run turns dontaudit rules off (`semodule -DB`) so a denial can't hide.
- **Testbed: 15/16**, the one failure the harness again: the sed cause's label was the outer script's command line (`sudo scripts/p8/check-0.3.0.sh`), two wrappers up. `OSREFLOG_CHECK_PARENT` is a comma-separated list now, each wrapper adding itself.
- **Seeded bisect: 5/5** with the workers' new start path.

Still to run: `sudo scripts/p8/selinux-workers.sh` with the domains enforcing, then a reboot for the boot order.

### 2026-10-20: the worker domains, enforcing

`sudo scripts/p8/selinux-workers.sh` with both domains enforcing and dontaudit rules off for the run: module installed, ledger labeled, doctor ok, **testbed 16/16** (the stacked `OSREFLOG_CHECK_PARENT` works), **live undo all ok** (restored files keep their labels, the attic, the dnf delegation), the summarizer as `osreflog_summary_t` and the undo worker as `osreflog_undo_t`, and **no denial** for either, in the audit log or the journal.

### 2026-10-20: 3.3 GiB, to be read

After the root checks, `osreflog status` said the kept snapshots hold 3.3 GiB that `/` no longer has, counting from c000174 (the boot checkpoint, the first interval measured). The diffs since then wrote about 900 MiB in all. Released can exceed written (deleting writes nothing), but nothing deleted that much in sight: the Flatpak appstream refresh in c000174..c000175 is 121 MiB, mostly hardlinks to its repository, which count nothing. A likelier part is the SELinux policy store (`/var/lib/selinux`, 1,790 files replaced in c000174..c000175): every `semodule` transaction writes a new copy of the store and deletes the old, and the checks ran six of them. Whether 3.3 GiB is all real can't be read from the CLI, so:

- `osreflog diff A..B` now says what the older side holds alone (`released_bytes` in the report), and the daemon logs, for each interval, how much it released and its five largest contributors;
- `sudo scripts/p8/space-intervals.sh` prints every stored interval's written and released bytes from the ledger, read-only, to see which intervals the sum comes from.
- **Read** (`perf-out/space-intervals.log`): 54 intervals, 3.79 GiB in all, and the pattern gives it away. Intervals 15 to 20 s apart that wrote 2 to 6 MiB each "released" 20 to 40 MiB, every one. The constant was journald's `system.journal`: written in place every interval in hundreds of small, scattered places (its header, hash tables, entry arrays and the new entries at the end), and past 64 ranges per file the builder merged them all into one span from the first to the last, nearly the whole 30–40 MiB file. The intervals around dnf transactions (about 150 MiB each) are mostly real: dnf's and PackageKit's metadata caches replaced whole.
- **Fixed**: up to 4,096 ranges per file are kept (64 KiB at most); past that the ranges are dropped and the bytes written count instead, which can't be more than was written. Ranges count in whole 4 KiB blocks (btrfs copies blocks, not bytes), without counting a shared block twice. A test writes 16 bytes in 1,000 places of a 40 MiB file: 1,000 blocks, not 40 MiB; and in 10,000 places: the 160,000 bytes written.
- **Ledger schema 7** forgets the releases stored so far (only this laptop has any); the space report counts again from the next interval.
- **Loopback again** with the fix (`perf-out/it.log`): 2/2, held 5,275,648 bytes: the 5 MiB, and the small files now counted in whole blocks (eight). The fixed build is installed (osreflogd restarted 2026-10-20 10:18, the summarizer again in `osreflog_summary_t`); the space line comes back after the next checkpoint.
