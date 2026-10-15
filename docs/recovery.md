# Recovery: getting back to your normal system after a metal probe

A metal bisect probe (`osreflog bisect … --tier metal`) reboots this machine **once** into a throwaway copy of an earlier state. Your normal boot entry is never changed, so in almost every case the way back is simply to **reboot**. This page is for when that isn't enough, or you want to check by hand.

## How a probe boot works

- osreflog adds one extra entry, **"osreflog bisect probe N (…)"**, as `/boot/loader/entries/osreflog-probe.conf`, and possibly a kernel and initramfs in `/boot/osreflog/`.
- It asks GRUB to boot that entry **for the next boot only** (`next_entry=osreflog-probe` in `/boot/grub2/grubenv`). GRUB clears `next_entry` as soon as it starts the probe, before the probe's kernel even loads.
- Your default entry (`saved_entry` in grubenv) and its file are never written. osreflog records both before arming and checks them after every step.
- The probe's kernel line has `panic=10`: a kernel panic reboots by itself after 10 seconds, into your default.

So whatever the probe does (hangs, panics, fails to mount, starts a broken desktop), the **next** boot is your normal system.

## The probe hangs

Hold the power button until the machine turns off, then turn it on again. GRUB already cleared the one-shot entry when it started the probe, so this boot is your default. This was tested in the boot gate's VM (a probe stuck in early boot, then a hard reset).

## You're at the GRUB menu and want to be sure

Fedora hides the GRUB menu after a successful boot. To show it, press **Esc** (or hold **Shift**, or press **F8**) right after the firmware logo. Then:

- Your normal entries are the usual **"Fedora Linux (…)"** ones. Pick the top one (or whichever you normally boot) and press Enter.
- **"osreflog bisect probe N"** is the probe. Booting it by mistake is harmless: it is a copy, its `/home` is a copy too, and osreflog itself doesn't run in it. Reboot to leave.

If a menu entry looks wrong, press `e` on it to see its kernel line without changing anything; `Esc` leaves the editor. A normal entry has `rootflags=subvol=root`; a probe has `rootflags=subvol=osreflog/probes/pN` and `osreflog.probe=N`.

## After you're back

At every boot of the default system, `osreflog-boot-doctor.service` removes the probe entry, the files in `/boot/osreflog/`, a leftover `next_entry` that names the probe, and probe copies no bisect is waiting for, and it checks the default entry. To see what it did:

```bash
journalctl -b -u osreflog-boot-doctor.service -o cat
```

To run it again, or see what it would do:

```bash
sudo osreflog doctor --boot --dry-run
sudo osreflog doctor --boot
```

## By hand, without osreflog

If osreflog isn't installed any more or doesn't run, this is everything a probe can leave on `/boot`:

```bash
sudo grub2-editenv list                              # saved_entry is your default; next_entry should be absent or empty
sudo grub2-editenv - unset next_entry                # only if it says next_entry=osreflog-probe
sudo rm -f /boot/loader/entries/osreflog-probe.conf
sudo rm -rf /boot/osreflog
```

The probe copies live at the btrfs top level, outside your mounted system, in `osreflog/probes/` (`pN` and `pN-home`). They take space but do nothing. To delete them:

```bash
sudo mount -o subvolid=5 /dev/sda3 /mnt                # your btrfs device: findmnt -no SOURCE /
sudo btrfs subvolume delete /mnt/osreflog/probes/p*
sudo rm -f /mnt/osreflog/probes/pending.json
sudo umount /mnt
```

## If the default entry itself doesn't boot

osreflog never writes it, and the boot doctor reports a changed default as a problem. If something else broke it, Fedora's **rescue** entry at the bottom of the GRUB menu boots with a generic initramfs. Once in, `sudo grubby --default-kernel` and `sudo grub2-editenv list` show what the default is; `sudo grub2-set-default <entry id>` sets it (ids are the file names in `/boot/loader/entries/` without `.conf`).
