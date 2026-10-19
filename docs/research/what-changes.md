# What changes on a desktop Linux system

The ledger is a dataset: every change to `/` the live witness saw, with the process behind it.
This note reads it to answer the question in the [vision](../vision.md): how many changes, from
whom, and which paths churn. It also checks osreflog itself: whatever floods the history or
lands in the wrong place shows up here first.

**First reading: 4.6 days**, 2026-10-15 to 2026-10-20, on the development laptop (Fedora 44
Workstation, one user, GNOME). The title promises a month. The same script runs again once the
history holds one (from 2026-11-15), and its tables replace these.

## How it was read

```bash
scripts/research/month.py --days 30 --out perf-out/month.md
```

It reads through the CLI (`causes`, `show`, `log`), as the desktop user, without root. It is
anonymised: no host or user names, no command lines (a command cause is named by its program),
no file contents, and UUIDs, long ids and numbers in paths are generalised. Three things are
left out of the counts:

- **osreflog's own tests and installs**: causes whose command is one of the test scripts
  (`scripts/p*`), and causes whose every path has "osreflog" in it (the testbed's scratch
  files, development installs). There were 478 of them out of 996, half the history, on a
  laptop where osreflog is being built.
- **Flatpak's installation**, counted apart (see the first finding).
- **Paths that are noise today**, read from the model's own list, so the first hours, recorded
  before some of those rules existed, read as they would now.

## What it found

342 causes from the system and the people using it, **74 a day**, making 1,266 changes to 244
distinct paths.

| Kind | Causes | Share |
|---|---|---|
| A system service (`unit`) | 265 | 77% |
| A package transaction (`dnf`) | 31 | 9% |
| A process with no unit and no shell above it (`unknown`) | 26 | 8% |
| A command typed in a shell (`command`) | 20 | 6% |

Most of what changes a desktop system is the system: services rewriting their own files. What
a person does directly (commands, package installs) is a sixth of the causes on this machine,
and fewer on one where nobody is developing system software.

The busiest writers:

| Who | Causes | What it writes |
|---|---|---|
| uresourced | 61 | Eight resource drop-ins in `/etc/systemd/system.control/` each time root's user slice starts (every `sudo` session here) |
| tuned-ppd | 38 | `/etc/tuned/*` and `/etc/modprobe.d/tuned.conf` on every power profile switch (unplugging the charger) |
| cupsd | 35 | `/etc/cups/subscriptions.conf` and its backup on its own schedule |
| abrt-journal-core | 31 | Its state in `/var/lib/abrt` |
| dnf | 31 | 143 packages over 33 transactions |
| crond / anacron | 16 | Its run stamps in `/var/spool/anacron` |

Churn is concentrated: 63% of paths changed once in the 4.6 days, and 32 paths changed ten
times or more; `/etc/systemd` alone is 608 of the 1,266 changes. Changes are mostly rewrites in
place (963 modifies, 105 attribute changes, 102 deletes, 96 creates).

## What it changed in osreflog

Reading the history this way found three things that the tests didn't:

1. **Flatpak drowned everything.** Before any filtering, 41,297 of the 43,183 recorded path
   changes (96%) were in `/var/lib/flatpak`: appstream refreshes by the system helper and
   `flatpak update`, stored row by row as "unknown" causes. The design had decided otherwise
   (R4: one opaque `flatpak` cause, never diffed), and the README said so, but nothing did it.
   Since 0.3.0, writes there are one `flatpak` cause that counts its paths without listing
   them, and noise in diffs.
2. **Services in a slice of their own were "unknown".** `cupsd` runs in
   `system.slice/system-cups.slice/cups.service`, and every templated unit (`getty@tty1`) sits
   in such a slice; only units directly under `system.slice` were recognised. Fixed in 0.3.0.
3. **Two more timers.** NetworkManager's DHCP lease renewals (50 causes) and abrt's journal
   cursor (42) are state rewritten on a schedule, like rsyslog's cursor and chrony's drift file
   already were; they are noise since 0.3.0.

Left as they are, on purpose: uresourced's drop-ins and tuned's profile files are real
configuration with a real cause (P7's metal bisect found a power-save regression in exactly
those tuned files), and cupsd's subscriptions file is configuration CUPS chose to keep in
`/etc`. A history that hid them would hide the one time they matter. If they are too loud, the
answer is in how they're shown (collapsing repeated causes from one unit with the same effect)
rather than in not recording them.

## Limits

- 4.6 days, not a month, on one development laptop; half the raw history was osreflog's own
  testing, and telling it apart is a heuristic.
- The live witness doesn't see what happens while the daemon is stopped (boots, upgrades of
  osreflog itself); the state witness records those changes, without a cause, and this note
  doesn't count them.
- Counts are causes and paths, not bytes; what the changes cost on disk is in
  [performance](../performance.md#disk).
