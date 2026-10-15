#!/bin/bash
# P7 G2 in the test VM: one probe armed, booted once, and the default back after it.
#
#   cargo build --release -p osreflog-boot --example bootprobe              # as your user
#   sudo scripts/p7/testvm.sh flow scripts/p7/g2-step.sh target/release/examples/bootprobe
#
# Runs once per boot inside the VM (after the boot report); its state lives in /home, which
# the default and the probe share. Prints facts, then DONE or FAIL; each boot reboots itself.
#   boot 1 (default): record the default; snapshot / as osreflog/probes/p1; install the kernel
#                     into /boot/osreflog (dracut from the probe); write the entry; arm; reboot
#   boot 2 (probe):   the probe's cmdline and root; GRUB has cleared next_entry; default intact
#   boot 3 (default): default again; disarm; delete the probe; nothing left on /boot
set -uo pipefail
S=/home/.testvm
REC=$S/record.json
mkdir -p "$S"
stage=$(cat "$S/stage" 2>/dev/null || echo 1)
fail() { echo "FAIL $*"; exit 1; }
say() { echo "$*"; }
next() { echo "$1" >"$S/stage"; sync; systemctl reboot; exit 0; }
cmdline=$(cat /proc/cmdline)
dev=$(findmnt -no SOURCE / | sed 's/\[.*\]$//')
top() { mkdir -p /run/top; mountpoint -q /run/top || mount -o subvolid=5 "$dev" /run/top; }

say "g2 stage $stage, root $(findmnt -no SOURCE /)"
case "$stage" in
1)
    [[ $cmdline != *osreflog.probe=* ]] || fail "stage 1 booted a probe"
    bootprobe record "$REC" || fail "record"
    top
    mkdir -p /run/top/osreflog/probes
    btrfs subvolume snapshot / /run/top/osreflog/probes/p1 >/dev/null || fail "snapshot"
    echo p1 >/run/top/osreflog/probes/p1/etc/osreflog-probe-marker
    bootprobe prepare "$REC" 1 osreflog/probes/p1 /run/top/osreflog/probes/p1 --install || fail "prepare"
    say "probe entry: $(grep -E '^(linux|initrd|options)' /boot/loader/entries/osreflog-probe.conf | tr '\n' '|')"
    say "boot/osreflog: $(ls -s /boot/osreflog | tr '\n' ' ')"
    bootprobe arm "$REC" || fail "arm"
    say "grubenv armed: $(grub2-editenv list | tr '\n' ' ')"
    bootprobe verify "$REC" || fail "verify after arm"
    next 2
    ;;
2)
    [[ $cmdline == *osreflog.probe=1* ]] || fail "stage 2 is not the probe: $cmdline"
    [[ $cmdline == *rootflags=subvol=osreflog/probes/p1* ]] || fail "probe cmdline without its subvolume"
    [[ $(findmnt -no OPTIONS /) == *subvol=/osreflog/probes/p1* ]] || fail "/ is not the probe: $(findmnt -no OPTIONS /)"
    [ "$(cat /etc/osreflog-probe-marker 2>/dev/null)" = p1 ] || fail "no probe marker in /etc"
    say "probe kernel $(uname -r), initrd from $(grep -o 'initrd [^ ]*' /boot/loader/entries/osreflog-probe.conf)"
    say "grubenv in probe: $(grub2-editenv list | tr '\n' ' ')"
    grub2-editenv list | grep -q '^next_entry=.' && fail "next_entry still set after the probe booted"
    bootprobe verify "$REC" || fail "verify in the probe"
    say "probe failed units: $(systemctl --failed --plain --no-legend | awk '{print $1}' | tr '\n' ' ')"
    next 3
    ;;
3)
    [[ $cmdline != *osreflog.probe=* ]] || fail "stage 3 booted the probe again"
    [[ $(findmnt -no OPTIONS /) == *subvol=/root* ]] || fail "/ is not root"
    bootprobe verify "$REC" || fail "verify after the probe"
    bootprobe disarm || fail "disarm"
    top
    btrfs subvolume delete /run/top/osreflog/probes/p1 >/dev/null || fail "delete probe"
    left=$(bootprobe leftovers)
    say "leftovers: $left"
    [ "$left" = '{"entry":false,"next_entry":null,"files":[]}' ] || fail "something left on /boot"
    bootprobe verify "$REC" || fail "verify after cleanup"
    rm -f /usr/local/libexec/osreflog-testvm-step
    echo done >"$S/stage"
    say DONE
    ;;
*) say "no stage $stage" ;;
esac
