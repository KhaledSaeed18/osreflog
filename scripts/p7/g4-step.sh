#!/bin/bash
# P7 G4 in the test VM: the boot doctor, at every boot and by hand.
#
#   cargo build --release && cargo build --release -p osreflog-boot --example bootprobe
#   sudo scripts/p7/testvm.sh flow scripts/p7/g4-step.sh target/release/osreflogd \
#       target/release/osreflog target/release/examples/bootprobe data/systemd/osreflog-boot-doctor.service
#
#   boot 1 (default): install this build's osreflogd, CLI and boot-doctor unit; an orphan clone
#                     p9; probe 3 (+ home) prepared with its own kernel files, armed, and
#                     recorded in pending.json as the bisect will
#   boot 2 (probe):   the boot doctor did not run here
#   boot 3 (default): the boot doctor ran at boot: /boot clean, p9 deleted, p3 and p3-home kept
#                     (pending), "probe 3 booted"; then by hand: a dry run changes nothing;
#                     an armed probe that never booted is disarmed; without pending.json the
#                     clones go
set -uo pipefail
S=/boot/.testvm-g4
REC=$S/record.json
mkdir -p "$S"
stage=$(cat "$S/stage" 2>/dev/null || echo 1)
fail() { echo "FAIL $*"; exit 1; }
next() { echo "$1" >"$S/stage"; sync; systemctl reboot; exit 0; }
cmdline=$(cat /proc/cmdline)
dev=$(findmnt -no SOURCE / | sed 's/\[.*\]$//')
top() { mkdir -p /run/top; mountpoint -q /run/top || mount -o subvolid=5 "$dev" /run/top; }
P=/run/top/osreflog/probes
clones() { ls "$P" 2>/dev/null | tr '\n' ' '; }
doctor_log() { journalctl -b -u osreflog-boot-doctor.service -o cat --no-pager | grep -v '^Starting\|^Finished\|^osreflog-boot-doctor' ; }
arm_probe() { # n clone [--install]
    bootprobe prepare "$REC" "$1" "osreflog/probes/$2" "$P/$2" ${3:-} >/dev/null || fail "prepare $2"
    bootprobe arm "$REC" >/dev/null || fail "arm $2"
}

echo "g4 stage $stage"
case "$stage" in
1)
    L=/usr/local/libexec/osreflog
    cp /usr/local/bin/osreflogd "$L/.osreflogd.new" && mv "$L/.osreflogd.new" "$L/osreflogd"
    install -m 0644 /usr/local/bin/osreflog-boot-doctor.service /etc/systemd/system/
    # data/ names the packaged paths; this build lives in /usr/local in the VM.
    sed -i 's|/usr/libexec/osreflog/|/usr/local/libexec/osreflog/|g' /etc/systemd/system/osreflog*.service
    restorecon "$L/osreflogd" /etc/systemd/system/osreflog-boot-doctor.service
    systemctl daemon-reload
    systemctl enable -q osreflog-boot-doctor.service || fail "enable"
    bootprobe record "$REC" || fail "record"
    top
    mkdir -p "$P"
    btrfs subvolume snapshot / "$P/p9" >/dev/null || fail "snapshot p9"
    btrfs subvolume snapshot / "$P/p3" >/dev/null || fail "snapshot p3"
    btrfs subvolume snapshot /run/top/home "$P/p3-home" >/dev/null || fail "snapshot p3-home"
    bootprobe prep 3 osreflog/probes/p3 "$P/p3" osreflog/probes/p3-home >/dev/null || fail "prep"
    arm_probe 3 p3 --install
    printf '{"n":3,"clone":"p3","home":"p3-home","record":%s}\n' "$(cat "$REC")" >"$P/pending.json"
    echo "armed probe 3; clones: $(clones); /boot/osreflog: $(ls /boot/osreflog | tr '\n' ' ')"
    next 2
    ;;
2)
    [[ $cmdline == *osreflog.probe=3* ]] || fail "stage 2 is not probe 3"
    echo "boot doctor in the probe: $(systemctl show -p ConditionResult -p ActiveState --value osreflog-boot-doctor | tr '\n' ' ')"
    [ "$(systemctl show -p ConditionResult --value osreflog-boot-doctor)" = no ] || fail "the boot doctor ran in the probe"
    next 3
    ;;
3)
    [[ $cmdline != *osreflog.probe=* ]] || fail "stage 3 booted a probe"
    echo "boot doctor at this boot: $(systemctl show -p Result -p ExecMainStatus --value osreflog-boot-doctor | tr '\n' ' ')"
    doctor_log | sed 's/^/  | /'
    doctor_log | grep -q 'probe 3 booted' || fail "the boot doctor didn't see probe 3's boot"
    top
    c=$(clones)
    echo "clones after boot: $c"
    [[ $c == *p3\ * && $c == *p3-home* && $c != *p9* ]] || fail "wrong clones kept: $c"
    [ "$(bootprobe leftovers)" = '{"entry":false,"next_entry":null,"files":[]}' ] || fail "/boot not clean: $(bootprobe leftovers)"
    bootprobe verify "$REC" || fail "verify"

    # An armed probe that never booted: disarmed by hand, clones kept while pending.
    arm_probe 3 p3
    osreflog doctor --boot --dry-run | sed 's/^/  | /'
    [ "$(bootprobe leftovers | grep -c '"next_entry":"osreflog-probe"')" = 1 ] || fail "the dry run changed /boot"
    osreflog doctor --boot | sed 's/^/  | /'
    [ "$(bootprobe leftovers)" = '{"entry":false,"next_entry":null,"files":[]}' ] || fail "not disarmed"
    [[ $(clones) == *p3\ * ]] || fail "p3 deleted while pending"

    # Nothing pending: the clones go.
    rm -f "$P/pending.json"
    osreflog doctor --boot | sed 's/^/  | /'
    [ -z "$(clones)" ] || fail "clones left: $(clones)"
    bootprobe verify "$REC" || fail "verify at the end"
    rm -rf "$S" /usr/local/libexec/osreflog-testvm-step
    echo DONE
    ;;
esac
