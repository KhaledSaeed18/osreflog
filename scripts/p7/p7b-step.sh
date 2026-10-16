#!/bin/bash
# P7b in the test VM: a whole metal bisect through the real daemon and CLI. The VM gets this
# build; real causes (one adds a bad line) between two checkpoints; `osreflog bisect start
# --tier metal`; then every probe is a reboot of the VM into it, where this script plays the
# person (looks at the config, gives the verdict inside the probe, reboots), and every return
# to the default lets the daemon read the verdict and arm the next probe.
#
#   cargo build --release
#   sudo scripts/p7/testvm.sh flow scripts/p7/p7b-step.sh target/release/osreflogd \
#       target/release/osreflog data/systemd/osreflogd.service \
#       data/systemd/osreflog-boot-doctor.service
#
# State on /boot. Boots: the default (install, seed, start, reboot), then per probe the probe
# (verdict, reboot) and the default (the daemon moves on, reboot), until the bisect ends.
set -uo pipefail
S=/boot/.testvm-p7b
mkdir -p "$S"
DIR=/etc/osreflog-seed
fail() { echo "FAIL $*"; osreflog bisect log 2>&1 | sed 's/^/  | /'; exit 1; }
reboot_now() { sync; systemctl reboot; exit 0; }
cmdline=$(cat /proc/cmdline)
status() { osreflog bisect status --json 2>/dev/null; }
field() { python3 -I -c "import json,sys; s=json.load(sys.stdin) or {}; $1"; }
settle() { sleep 3; }
checkpoint() { osreflog checkpoint -m "$1" --json | python3 -I -c 'import json,sys; print(json.load(sys.stdin)["name"])'; }
cause_of() { osreflog why "$1" -n 1 --json | python3 -I -c 'import json,sys; e=json.load(sys.stdin); print(e[0]["cause"]["id"] if e else "")'; }
boots=$(( $(cat "$S/boots" 2>/dev/null || echo 0) + 1 ))
echo "$boots" >"$S/boots"
[ "$boots" -le 20 ] || fail "20 boots and no result"

if [[ $cmdline == *osreflog.probe=* ]]; then
    n=$(sed 's/.*osreflog\.probe=\([0-9]*\).*/\1/' <<<"$cmdline")
    echo "p7b boot $boots: in probe $n, / is $(findmnt -no SOURCE /), /home is $(findmnt -no SOURCE /home)"
    systemctl is-active -q osreflogd && fail "osreflogd runs in the probe"
    [ "$(cat /var/lib/osreflog-probe/manual 2>/dev/null)" = "$n" ] || fail "no marker for probe $n"
    if grep -q 'mode = broken' "$DIR/app.conf" 2>/dev/null; then v=bad; else v=good; fi
    echo "probe $n: $v ($(grep -c . "$DIR/app.conf" 2>/dev/null) lines in app.conf)"
    osreflog bisect "$v" --no-reboot | sed 's/^/  | /'
    [ "$(cat /var/lib/osreflog-probe/verdict)" = "$v" ] || fail "the verdict wasn't written in the probe"
    reboot_now
fi

echo "p7b boot $boots: the default"
if [ ! -e "$S/started" ]; then
    L=/usr/local/libexec/osreflog
    cp /usr/local/bin/osreflogd "$L/.osreflogd.new" && mv "$L/.osreflogd.new" "$L/osreflogd"
    # flow put this build's CLI at /usr/local/bin/osreflog already, and the unit files next to it.
    install -m 0644 /usr/local/bin/osreflogd.service /usr/local/bin/osreflog-boot-doctor.service /etc/systemd/system/
    restorecon "$L/osreflogd" /usr/local/bin/osreflog /etc/systemd/system/osreflogd.service /etc/systemd/system/osreflog-boot-doctor.service
    systemctl daemon-reload
    systemctl enable -q osreflog-boot-doctor.service
    systemctl restart osreflogd.service
    sleep 2
    systemctl is-active -q osreflogd || fail "osreflogd didn't start: $(journalctl -u osreflogd -n 5 -o cat)"
    osreflog bisect reset >/dev/null 2>&1

    echo "seeding: four causes, the third adds a bad line"
    rm -rf "$DIR"; mkdir -p "$DIR"; printf 'port = 80\nworkers = 4\n' >"$DIR/app.conf"; settle
    good=$(checkpoint "p7b: good") || fail "checkpoint"
    echo "# tuned" >>"$DIR/app.conf"; settle
    sed -i 's/workers = 4/workers = 8/' "$DIR/app.conf"; settle
    echo "mode = broken" >>"$DIR/app.conf"; settle
    cause_of "$DIR/app.conf" >"$S/expected"
    echo "timeout = 30" >>"$DIR/app.conf"; settle
    bad=$(checkpoint "p7b: bad") || fail "checkpoint"
    echo "good $good, bad $bad, the bad line came with cause #$(cat "$S/expected")"
    [ -n "$(cat "$S/expected")" ] || fail "the live witness didn't record the bad line's cause"
    osreflog bisect start --good "$good" --bad "$bad" --tier metal --no-reboot | sed 's/^/  | /'
    touch "$S/started"
fi

# Back on the default: let the daemon read the verdict and arm the next probe (or finish).
for _ in $(seq 180); do
    s=$(status)
    running=$(field 'print(s.get("running", False))' <<<"$s")
    reboot=$(field 'w=s.get("waiting") or {}; print(w.get("reboot", False))' <<<"$s")
    if [ "$running" != True ] || [ "$reboot" = True ]; then break; fi
    sleep 1
done
echo "boot doctor: $(journalctl -b -u osreflog-boot-doctor.service -o cat --no-pager | grep '^  ' | tr '\n' '|')"
osreflog bisect status | tail -4 | sed 's/^/  | /'
if [ "$running" = True ] && [ "$reboot" = True ]; then
    n=$(field 'print(s["waiting"]["probe"])' <<<"$s")
    [ -e /boot/loader/entries/osreflog-probe.conf ] || fail "probe $n waits for a reboot but isn't armed"
    grep -q "next_entry=osreflog-probe" <(grub2-editenv list) || fail "probe $n not armed in grubenv"
    echo "rebooting into probe $n"
    reboot_now
fi
[ "$running" = True ] && fail "the bisect waits but not for a reboot: $(field 'print(s.get("waiting"))' <<<"$s")"

found=$(field 'f=s.get("finding") or {}; print(f.get("culprit", {}).get("id", ""))' <<<"$s")
expected=$(cat "$S/expected")
osreflog bisect log | sed 's/^/  | /'
[ "$found" = "$expected" ] || fail "found '#$found', expected #$expected: $(field 'print(s.get("error") or s.get("finding"))' <<<"$s")"
[ -z "$(grub2-editenv list | grep 'next_entry=osreflog')" ] || fail "next_entry left armed"
[ ! -e /boot/loader/entries/osreflog-probe.conf ] || fail "the probe entry is left"
echo "found #$found in $(field 'print(len(s["probes"]))' <<<"$s") probes, $boots boots"
rm -rf "$S" "$DIR" /usr/local/libexec/osreflog-testvm-step
echo DONE
