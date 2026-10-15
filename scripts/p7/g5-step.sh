#!/bin/bash
# P7, the boot gate (G5) in the test VM: the metal flow probe after probe, the default checked
# after every one, a probe that hangs and one that panics both back on the default, and the
# boot doctor cleaning up after each.
#
#   cargo build --release && cargo build --release -p osreflog-boot --example bootprobe
#   sudo scripts/p7/testvm.sh flow scripts/p7/g5-step.sh target/release/osreflogd \
#       target/release/osreflog target/release/examples/bootprobe data/systemd/osreflog-boot-doctor.service
#
# One stage per boot; state on /boot. Each probe is made the way the metal tier will make it:
# record the default, snapshot / and home, prepare the clone, plan and install the kernel, write
# the entry, write pending.json, arm. The verdict is a file the probe writes into itself (what
# `sudo osreflog bisect good|bad` will do in P7a), read from the clone back on the default.
#   1 default  install this build; probe 1 (this kernel, reused from /boot)
#   2 probe 1  boot record, default intact from inside; verdict "bad"
#   3 default  doctor at boot kept p1; verdict read back; p1 released; probe 2 (7.2.8: the
#              probe's own kernel, installed in /boot/osreflog, initramfs from the clone)
#   4 probe 2  Secure Boot with the copied 7.2.8 image; verdict "good"
#   5 default  verdict read back; probe 3, which hangs in early boot (the host resets the VM)
#   6 default  after the reset: the default, not the probe again; no boot record; probe 4,
#              which panics (sysrq c) and reboots itself through panic=10
#   7 default  the default again; nothing left anywhere
set -uo pipefail
S=/boot/.testvm-g5
REC=$S/record.json
mkdir -p "$S"
stage=$(cat "$S/stage" 2>/dev/null || echo 1)
fail() { echo "FAIL $*"; exit 1; }
next() { echo "$1" >"$S/stage"; sync; systemctl reboot; exit 0; }
cmdline=$(cat /proc/cmdline)
dev=$(findmnt -no SOURCE / | sed 's/\[.*\]$//')
top() { mkdir -p /run/top; mountpoint -q /run/top || mount -o subvolid=5 "$dev" /run/top; }
P=/run/top/osreflog/probes
clean='{"entry":false,"next_entry":null,"files":[]}'
on_default() {
    [[ $cmdline != *osreflog.probe=* ]] || fail "stage $stage booted a probe: $cmdline"
    [[ $(findmnt -no OPTIONS /) == *subvol=/root* ]] || fail "/ is not root"
}
in_probe() {
    [[ $cmdline == *osreflog.probe=$1\ * || $cmdline == *osreflog.probe=$1 ]] || fail "stage $stage is not probe $1: $cmdline"
    [[ $(findmnt -no OPTIONS /) == *subvol=/osreflog/probes/p$1* ]] || fail "/ is not p$1"
    [[ $(findmnt -no OPTIONS /home) == *subvol=/osreflog/probes/p$1-home* ]] || fail "/home is not p$1-home"
    systemctl is-active -q osreflog-metal-probe.service || fail "no boot record unit"
    bootprobe verify "$REC" || fail "the default changed (seen from probe $1)"
}
doctor_at_boot() {
    [ "$(systemctl show -p Result --value osreflog-boot-doctor)" = success ] || fail "the boot doctor failed at boot"
    journalctl -b -u osreflog-boot-doctor.service -o cat --no-pager | grep '^  ' | sed 's/^/  | /'
}
after_probe() { # n: back on the default after probe n
    on_default
    doctor_at_boot
    [ "$(bootprobe leftovers)" = "$clean" ] || fail "/boot not clean after probe $1: $(bootprobe leftovers)"
    bootprobe verify "$REC" || fail "the default changed after probe $1"
    top
}
release() { # n: the bisect has the verdict: drop pending, let the doctor delete the clones
    rm -f "$P/pending.json"
    osreflog doctor --boot | grep -q "removed clone p$1" || fail "p$1 not deleted"
}
make_probe() { # n [--install]: everything up to and including arming
    local n=$1
    bootprobe record "$REC" >/dev/null || fail "record"
    top
    mkdir -p "$P"
    btrfs subvolume snapshot / "$P/p$n" >/dev/null || fail "snapshot p$n"
    btrfs subvolume snapshot /run/top/home "$P/p$n-home" >/dev/null || fail "snapshot p$n-home"
    bootprobe prep "$n" "osreflog/probes/p$n" "$P/p$n" "osreflog/probes/p$n-home" >/dev/null || fail "prep p$n"
    shift
    "${HOOK:-true}" "$P/p$n"
    bootprobe prepare "$REC" "$n" "osreflog/probes/p$n" "$P/p$n" "$@" | sed 's/^/  /' || fail "kernel and entry p$n"
    printf '{"n":%s,"clone":"p%s","home":"p%s-home","record":%s}\n' "$n" "$n" "$n" "$(cat "$REC")" >"$P/pending.json"
    bootprobe arm "$REC" >/dev/null || fail "arm p$n"
    echo "probe $n armed"
}
verdict_of() { cat "$P/p$1/var/lib/osreflog-probe/verdict" 2>/dev/null; }

# Probe hooks: what makes probe 2 older, probe 3 hang, probe 4 panic.
older_kernel() { rm -rf "$1/usr/lib/modules/7.2.9-200.fc44.x86_64"; }
hang() {
    cat >"$1/etc/systemd/system/osreflog-g5-hang.service" <<'EOF'
[Unit]
Description=G5: hang the boot
DefaultDependencies=no
Before=sysinit.target
[Service]
Type=oneshot
ExecStart=/bin/sh -c 'echo "TESTVM RESET-ME (probe hangs)" >/dev/console; exec sleep infinity'
TimeoutStartSec=infinity
[Install]
WantedBy=sysinit.target
EOF
    mkdir -p "$1/etc/systemd/system/sysinit.target.wants"
    ln -sf /etc/systemd/system/osreflog-g5-hang.service "$1/etc/systemd/system/sysinit.target.wants/"
}
panic() {
    cat >"$1/etc/systemd/system/osreflog-g5-panic.service" <<'EOF'
[Unit]
Description=G5: crash the kernel
DefaultDependencies=no
After=local-fs.target
[Service]
Type=oneshot
ExecStart=/bin/sh -c 'echo "TESTVM probe 4 crashing the kernel" >/dev/console; sleep 1; echo c >/proc/sysrq-trigger'
[Install]
WantedBy=sysinit.target
EOF
    mkdir -p "$1/etc/systemd/system/sysinit.target.wants"
    ln -sf /etc/systemd/system/osreflog-g5-panic.service "$1/etc/systemd/system/sysinit.target.wants/"
}

echo "g5 stage $stage"
case "$stage" in
1)
    on_default
    L=/usr/local/libexec/osreflog
    cp /usr/local/bin/osreflogd "$L/.osreflogd.new" && mv "$L/.osreflogd.new" "$L/osreflogd"
    install -m 0644 /usr/local/bin/osreflog-boot-doctor.service /etc/systemd/system/
    restorecon "$L/osreflogd" /etc/systemd/system/osreflog-boot-doctor.service
    systemctl daemon-reload && systemctl enable -q osreflog-boot-doctor.service || fail "enable the doctor"
    make_probe 1
    next 2
    ;;
2)
    in_probe 1
    echo "probe 1: kernel $(uname -r); verdict bad"
    echo bad >/var/lib/osreflog-probe/verdict
    next 3
    ;;
3)
    after_probe 1
    [ "$(verdict_of 1)" = bad ] || fail "probe 1's verdict not read back: $(verdict_of 1)"
    echo "probe 1 verdict read back: bad"
    release 1
    HOOK=older_kernel make_probe 2
    next 4
    ;;
4)
    in_probe 2
    [ "$(uname -r)" = 7.2.8-200.fc44.x86_64 ] || fail "probe 2 runs $(uname -r), not its own 7.2.8"
    echo "probe 2: kernel $(uname -r), $(mokutil --sb-state | head -1); verdict good"
    echo good >/var/lib/osreflog-probe/verdict
    next 5
    ;;
5)
    after_probe 2
    [ "$(verdict_of 2)" = good ] || fail "probe 2's verdict not read back"
    echo "probe 2 verdict read back: good"
    release 2
    HOOK=hang make_probe 3
    next 6
    ;;
6)
    after_probe 3
    [ -z "$(bootprobe booted "$P/p3" 3 2>/dev/null)" ] || fail "probe 3 has a boot record, but it hung"
    journalctl -b -u osreflog-boot-doctor.service -o cat --no-pager | grep -q 'probe 3 has no boot record' \
        || fail "the doctor didn't report probe 3 as never booted"
    echo "probe 3 hung, was reset, and the default came back"
    release 3
    HOOK=panic make_probe 4
    next 7
    ;;
7)
    after_probe 4
    echo "probe 4 panicked, rebooted itself (panic=10), and the default came back"
    release 4
    [ "$(bootprobe leftovers)" = "$clean" ] || fail "left on /boot"
    [ -z "$(ls "$P" 2>/dev/null)" ] || fail "left in probes: $(ls "$P")"
    bootprobe verify "$REC" || fail "verify at the end"
    rm -rf "$S" /usr/local/libexec/osreflog-testvm-step
    echo DONE
    ;;
esac
