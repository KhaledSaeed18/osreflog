# Changelog

What changed in each release, for people running osreflog. The design and the measurements
behind each change are in [docs/journal/](docs/journal/). The release workflow takes a
release's notes from its section here.

## 0.2.0 (2026-10-17)

The app grows up, and the project gets its release machinery.

### App

- The row of what's open (a cause, its undo, a checkpoint's diff, the bisect) is marked in the
  sidebar.
- Search the causes by label, kind, process, command or `#id` (Ctrl+F; Escape clears it).
- Pages open with a header: an icon, a title kept to two lines, one line of facts, and what can
  be done (Undo… is in a cause's header). Commands are in monospace.
- Line diffs are colored: removed lines red, added lines green. A probe's verdict icon is green
  or red.
- Animated loading, shown only when an answer takes more than 200 ms; pages fade in and start at
  the top; a running bisect's page refreshes in place.
- Toasts when a verdict is sent, a probe armed again, a verdict taken back, an undo done, and
  when the daemon answers again after it was down. F5 or Ctrl+R refreshes now.
- Long commands no longer wrap to three lines in the lists; the full text is in the tooltip.
- Needs libadwaita 1.6 or newer.

### Hardening

- `osreflogd` and the boot doctor refuse the system call groups they never use (clock setting,
  ptrace and perf events, kernel modules, raw I/O, reboot, swap, obsolete calls). systemd's
  exposure score: 3.6 → 2.4 and 4.2 → 2.2.
- The settings agent (`osreflog-user`) is sandboxed: read-only system and home except its own
  ledger, no network, no capabilities, a system call allow-list. Exposure 8.0 → 1.0.
- A new package, `osreflog-selinux`: the ledger, blobs and attic in `/var/lib/osreflog`, and the
  runtime directories, under SELinux types of their own, so confined services can't read the
  history. Installed with `osreflog` wherever SELinux is in use.
- `MALLOC_ARENA_MAX=2` for the daemon, which held 108 MB after a day with several large diffs:
  glibc keeps a malloc arena per thread and rarely returns what's freed in it. The hardening
  check measures the difference.

### Project

- The README is the project's front page: git's tools mapped to osreflog's, a tour with real
  output, screenshots in light and dark, what keeps it safe, and the measured numbers.
- CI on every push, on Fedora 44: build, clippy with warnings as errors, the tests, a 30-second
  run of every fuzz target, the units' systemd exposure (it may only get better), the D-Bus and
  polkit policies, the desktop entry and metainfo, the spec against the workspace, and
  `cargo deny`; the SELinux module compiles.
- Releases are built by CI from the tag: the source tarball, the SRPM and the RPMs, attached to
  the GitHub release.
- `scripts/screenshots.sh` captures the README's screenshots from real history, off-screen.
- The AppStream metainfo lists the screenshots.

## 0.1.0 (2026-10-16)

The first release, [in COPR](https://copr.fedorainfracloud.org/coprs/khaledsaeed18/osreflog/)
for Fedora 44.

- `osreflogd` records every change to the system with its cause: a dnf transaction, a systemd
  unit, a terminal command, an app, a daemon acting for the D-Bus caller who asked it. btrfs
  checkpoints around every dnf transaction and at boot; a hash-chained ledger.
- The CLI: `log`, `diff`, `causes`, `show`, `why`, `checkpoint`, `undo` (a dry run unless
  `--apply`), `user` (your own dconf/GSettings changes), `bisect` in containers, VMs and reboots
  of this machine, `verify`, `doctor`.
- The GNOME app: the timeline of causes, what changed since any checkpoint, undo with a preview,
  and the bisect wizard.
- Two packages, `osreflog` and `osreflog-app`; the services follow Fedora's presets and are
  enabled by hand.
