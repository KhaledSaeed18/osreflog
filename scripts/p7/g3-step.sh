#!/bin/bash
# P7 G3 in the test VM: a metal probe prepared like the real thing (its own fstab, a throwaway
# home, osreflogd kept out, the dnf hook gone, a boot record), booted once, and read back.
#
#   cargo build --release -p osreflog-boot --example bootprobe              # as your user
#   sudo scripts/p7/testvm.sh flow scripts/p7/g3-step.sh target/release/examples/bootprobe
#
# State lives on /boot (ext4, shared by the default and the probe): /home is exactly what the
# probe must not share.
#   boot 1 (default): snapshot / and home as probe 2; prepare it; reuse /boot's kernel; arm
#   boot 2 (probe):   / and /home are the probe's; osreflogd skipped by its condition, not
#                     failed; no dnf hook; boot record written; nothing failed; write to /home
#   boot 3 (default): the probe's /home write isn't in the real home; boot record read from
#                     the clone; disarm and delete both subvolumes
set -uo pipefail
S=/boot/.testvm-g3
REC=$S/record.json
mkdir -p "$S"
stage=$(cat "$S/stage" 2>/dev/null || echo 1)
fail() { echo "FAIL $*"; exit 1; }
next() { echo "$1" >"$S/stage"; sync; systemctl reboot; exit 0; }
cmdline=$(cat /proc/cmdline)
dev=$(findmnt -no SOURCE / | sed 's/\[.*\]$//')
top() { mkdir -p /run/top; mountpoint -q /run/top || mount -o subvolid=5 "$dev" /run/top; }
P=/run/top/osreflog/probes

echo "g3 stage $stage, root $(findmnt -no SOURCE /), home $(findmnt -no SOURCE /home)"
case "$stage" in
1)
    [[ $cmdline != *osreflog.probe=* ]] || fail "stage 1 booted a probe"
    rm -f /home/.g3-probe-wrote
    bootprobe record "$REC" || fail "record"
    top
    mkdir -p "$P"
    btrfs subvolume snapshot / "$P/p2" >/dev/null || fail "snapshot /"
    btrfs subvolume snapshot /run/top/home "$P/p2-home" >/dev/null || fail "snapshot home"
    bootprobe prep 2 osreflog/probes/p2 "$P/p2" osreflog/probes/p2-home || fail "prep"
    echo "probe fstab: $(grep -v '^#' "$P/p2/etc/fstab" | grep -E ' / | /home ' | tr '\n' '|')"
    bootprobe prepare "$REC" 2 osreflog/probes/p2 "$P/p2" || fail "kernel and entry"
    bootprobe arm "$REC" || fail "arm"
    next 2
    ;;
2)
    [[ $cmdline == *osreflog.probe=2* ]] || fail "stage 2 is not probe 2: $cmdline"
    [[ $(findmnt -no OPTIONS /) == *subvol=/osreflog/probes/p2,* || $(findmnt -no OPTIONS /) == *subvol=/osreflog/probes/p2 ]] \
        || fail "/ is not the probe: $(findmnt -no OPTIONS /)"
    [[ $(findmnt -no OPTIONS /home) == *subvol=/osreflog/probes/p2-home* ]] \
        || fail "/home is not the throwaway: $(findmnt -no OPTIONS /home)"
    echo "osreflogd: $(systemctl show -p ActiveState -p ConditionResult --value osreflogd | tr '\n' ' ')"
    [ "$(systemctl show -p ConditionResult --value osreflogd)" = no ] || fail "osreflogd's condition didn't stop it"
    systemctl is-failed -q osreflogd && fail "osreflogd failed instead of being skipped"
    [ ! -e /etc/dnf/libdnf5-plugins/actions.d/osreflog.actions ] || fail "dnf hook still in the probe"
    systemctl is-active -q osreflog-metal-probe.service || fail "the probe unit didn't run: $(systemctl status osreflog-metal-probe.service --no-pager 2>&1 | tail -3)"
    echo "boot record: $(tr '\n' ' ' </var/lib/osreflog-probe/booted)"
    echo "motd: $(cut -c1-60 /etc/motd.d/osreflog-probe)…"
    echo "labels: $(ls -Z /etc/systemd/system/osreflog-metal-probe.service /etc/fstab /var/lib/osreflog-probe/booted | awk '{print $1}' | tr '\n' ' ')"
    failed=$(systemctl --failed --plain --no-legend | awk '{print $1}' | tr '\n' ' ')
    echo "failed units: ${failed:-none}"
    [ -z "$failed" ] || fail "units failed in the probe"
    echo "written in the probe" >/home/.g3-probe-wrote
    bootprobe verify "$REC" || fail "verify in the probe"
    next 3
    ;;
3)
    [[ $cmdline != *osreflog.probe=* ]] || fail "stage 3 booted the probe again"
    [ ! -e /home/.g3-probe-wrote ] || fail "the probe wrote into the real /home"
    systemctl is-active -q osreflogd || fail "osreflogd not running on the default"
    top
    [ -e "$P/p2-home/.g3-probe-wrote" ] || fail "the probe's write isn't in its throwaway home either"
    bootprobe booted "$P/p2" 2 || fail "no boot record in the clone"
    bootprobe verify "$REC" || fail "verify after the probe"
    bootprobe disarm || fail "disarm"
    btrfs subvolume delete "$P/p2" "$P/p2-home" >/dev/null || fail "delete probe"
    left=$(bootprobe leftovers)
    [ "$left" = '{"entry":false,"next_entry":null,"files":[]}' ] || fail "left on /boot: $left"
    rm -rf "$S" /usr/local/libexec/osreflog-testvm-step
    echo DONE
    ;;
esac
