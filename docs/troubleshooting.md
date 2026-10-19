# Troubleshooting

Start with `osreflog doctor`: it checks the layout, the dnf hook, the daemon and the ledger, and
says what to do about anything wrong. The daemon's own account of things is in its journal:

```bash
journalctl -u osreflogd -b
```

Each entry below is a symptom, why it happens, and what to do.

## The daemon

**"osreflog isn't running" in the app, or `D-Bus: … ServiceUnknown` from the CLI.**
`osreflogd` isn't started; after an install from COPR it's off until enabled once.

```bash
sudo systemctl enable --now osreflogd.service
```

The app notices within five seconds. If the unit fails, `journalctl -u osreflogd -b` says why.

**`/ is ext4, not btrfs` (or xfs, …).** osreflog needs `/` on a btrfs subvolume; there is no
fallback. **`/ is the btrfs top level`:** `/` must be a subvolume (Fedora's `root`), not
subvolume 5 itself, so snapshots can live beside it.

**`Snapper is installed` / `Timeshift is installed` (a warning).** Two tools snapshotting the same
`/` is unsupported: retention and rollbacks would fight. Use one.

**Reading works at the desktop but not over ssh.** Reading the history is allowed to the active
local session only (polkit `…read`); an ssh session is not one. Use `sudo osreflog …` there:
root may call everything.

**A password prompt for every undo.** By design: an undo writes to the system, so polkit asks
each time (`auth_admin`). Checkpoints and bisect remember the answer for a few minutes
(`auth_admin_keep`).

## Recording

**dnf prints `osreflog: no pre-transaction checkpoint: osreflogd is not running`.**
The transaction goes ahead (the hook never blocks or fails dnf), without its checkpoints. Nothing
is lost: the next checkpoint's diff has the whole change, and the dnf history import names the
transaction. Start the daemon so the next one is bracketed.

**An offline update from GNOME Software ("Restart & Install").** It runs in its own boot target;
`osreflogd` is wanted by `system-update.target` too and starts before the offline transaction,
so the update gets its checkpoints like any other.

**"Not watched" windows in a diff.** Times when the daemon wasn't running (a restart, an update,
the time between shutdown and the next boot). The state witness still sees every change in them;
they just have no recorded cause. The app and `osreflog diff` list each window.

**A cause of kind `unknown`.** A write whose process exited before its lineage could be read, or
one no rule claims (a process with no unit, session or package transaction). Its process name,
pid and command are still shown when known.

**Thousands of "noise" or "package" changes.** Package transactions write many files, and a boot
updates access times. Views hide package-owned paths, caches, logs and times-only changes by
default; `--all` (or **Include package files and noise** in the app) shows them.

**`osreflog verify` fails.** A ledger record no longer matches the hash chain: the database was
changed outside osreflogd, or the disk lost a write. Nothing is repaired automatically. Keep a
copy of `/var/lib/osreflog/` and [open an issue](https://github.com/KhaledSaeed18/osreflog/issues)
with the `verify` output.

## Undo

**`conflict, left alone: …`.** The path changed again after the cause you're undoing, so putting
back the old version would throw that newer change away. Undo never does that. Look at the
newer change (`osreflog why <path>`), then edit the file by hand, or undo the newer cause first.

**`refused as a secret`.** Files osreflog never stores (`/etc/shadow`, private keys, Wi-Fi and
VPN credentials) can't be put back: there's no content to put back. Restore them from a backup.

**A package transaction can't be undone path by path.** It's delegated: undo shows the
`dnf5 history undo <id>` command to run, so dnf removes or reinstalls the packages itself.

**Where did the replaced file go?** Into the attic, `/var/lib/osreflog/attic/<time>-c<cause>/`,
never deleted. The undo is a cause of its own: undoing it puts the change back.

## Bisect

**`not reproduced: the test passes on the bad end`.** The test says "good" on the checkpoint you
called bad, so there's nothing to search. Run the test by hand on the live system first. Common
reasons: the test depends on something a container doesn't have:
- `systemctl is-enabled …` in a container probe prints nothing (no systemd runs there): use
  `systemctl --root=/ is-enabled …`, or turn on booting systemd in the container (`--boot`).
- Kernel modules, drivers, early boot: a VM probe at least (`--needs kernel`).
- Wi-Fi, GPU, suspend: this machine's hardware (`--needs hardware`, reboots into each copy).

**VM probes fail to start.** They need `qemu-kvm-core`, `virtiofsd` and `dracut`
(`sudo dnf install qemu-kvm-core virtiofsd dracut`), KVM (`/dev/kvm`), and about 600 MiB of free
memory per probe.

**`approximate` on a probe.** A replayed path had no stored content (larger than 1 MiB, binary,
or written before the daemon saw it), so the copy used the nearest state the snapshots have.
The result names those paths; treat that probe's verdict with care.

**After a probe on real hardware, the machine didn't come back to the normal system.** The
probe boots once; the next boot is always the default entry. If a probe hangs, power-cycle the
machine. If anything looks off at the GRUB menu, see [Recovery](recovery.md), which also says
how to boot the default entry by hand. `osreflog-boot-doctor` cleans up after every boot.

**"the default boot entry changed" when arming a probe.** Something rewrote the default boot
entry since the bisect started (a kernel update, tuned). osreflog refuses to arm a probe it
can't check; start the bisect again.

## Space

Snapshots share data with `/`, so each costs only what changed after it. Retention keeps every
checkpoint for 48 hours, then one a day for 14 days and one a week for 8 weeks; the ledger is
small and kept. `osreflog status` says how many snapshots are kept and how much they hold that
`/` no longer has (deleted files, replaced content), as a range:

```
space      the kept snapshots hold between 310 MiB and 1.2 GiB that / no longer has
```

The low end is held for sure; the high end is what may be, if older snapshots still share it.
A big upgrade replaces hundreds of MiB of files at once, and the snapshot before it keeps the old
ones until retention lets it go.

**`doctor` warns that the kept snapshots hold more than is free.** Keep fewer: lower the numbers
in `/etc/osreflog/retention.conf` (every setting is there, commented out, with its default),
then `sudo systemctl restart osreflogd`. Snapshots that fall outside the new limits go after the
next checkpoint; their history stays in the ledger, only `diff`s and bisects that need their
files can't use them any more. A setting the daemon can't read is named in
`journalctl -u osreflogd -b`, and the defaults are used.

The range comes from the diffs osreflog computes anyway, at no extra cost; diffs from before
0.3.0 didn't measure it, so until those snapshots are pruned the line says which checkpoint it
counts from. For the exact figure, `btrfs filesystem usage /` before and after
`sudo btrfs subvolume delete` of a snapshot (slow, and a deleted snapshot doesn't come back).

## Still stuck

[Open an issue](https://github.com/KhaledSaeed18/osreflog/issues) with the output of
`osreflog doctor`, `osreflog status` and `journalctl -u osreflogd -b --no-pager | tail -100`.
