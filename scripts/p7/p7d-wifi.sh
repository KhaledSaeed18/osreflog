#!/bin/bash
# P7d, the P7 exit: a hardware-specific regression bisected on this laptop, end to end, with
# metal probes. The regression is seeded: NetworkManager's Wi-Fi power save, off in the good
# state (wifi.powersave = 2), switched on by one of four real causes (wifi.powersave = 3). Only
# the real Wi-Fi card can tell: `iw dev <wlan> get power_save` once the connection is up. A
# container or a VM has no answer, so this is metal-only.
#
#   cargo build --release && sudo scripts/install-dev.sh
#   sudo scripts/p7/p7d-wifi.sh start [--hands-free]
#       seeds the causes, takes the two checkpoints, starts `bisect start --tier metal`
#   ...then, for each probe: reboot; in the probe run  sudo osreflog-p7d-check  (it gives the
#       verdict); reboot. Back on your normal system osreflog arms the next probe; reboot
#       again, until `osreflog bisect status` shows the result.
#   sudo scripts/p7/p7d-wifi.sh finish
#       checks the result, removes everything this script added, puts power save back
#
# --hands-free also installs two units so nobody has to be there: in a probe, the check runs
# once Wi-Fi is up, gives the verdict and reboots; on the default system, once osreflog has armed
# the next probe, it reboots into it. At most 12 reboots, then they stop. Use it only after one
# probe by hand has worked on this machine.
set -uo pipefail
[ "$(id -u)" -eq 0 ] || { echo "run with sudo" >&2; exit 1; }
STATE=/var/lib/osreflog-p7d
CONF=/etc/NetworkManager/conf.d/90-osreflog-p7d.conf
EXTRA=/etc/NetworkManager/conf.d/91-osreflog-p7d-extra.conf
CHECK=/usr/local/bin/osreflog-p7d-check
NEXT=/usr/local/bin/osreflog-p7d-next
AUTO_UNIT=/etc/systemd/system/osreflog-p7d-auto.service
NEXT_UNIT=/etc/systemd/system/osreflog-p7d-next.service
settle() { sleep 3; }     # a cause closes after 2 s of quiet
checkpoint() { osreflog checkpoint -m "$1" --json | python3 -I -c 'import json,sys; print(json.load(sys.stdin)["name"])'; }
cause_of() { osreflog why "$1" -n 1 --json | python3 -I -c 'import json,sys; e=json.load(sys.stdin); print(e[0]["cause"]["id"] if e else "")'; }
die() { echo "p7d: $*" >&2; exit 1; }

wlan() { iw dev 2>/dev/null | awk '/Interface/ {print $2; exit}'; }

install_check() {
    local dev=$1
    cat >"$CHECK" <<EOF
#!/bin/sh
# osreflog P7d: in a bisect probe, judge Wi-Fi power save on $dev and give the verdict.
dev=$dev
state=
for i in \$(seq 90); do
    state=\$(nmcli -t -f DEVICE,STATE dev 2>/dev/null | grep "^\$dev:" | cut -d: -f2)
    [ "\$state" = connected ] && break
    sleep 1
done
ps=\$(iw dev "\$dev" get power_save 2>&1)
echo "\$dev: \$ps (connection: \$state)"
case "\$ps" in
    *": off"*) v=good ;;
    *": on"*) v=bad ;;
    *) v=skip ;;
esac
[ "\$state" = connected ] || v=skip
echo "verdict: \$v"
exec osreflog-verdict "\$v"
EOF
    chmod 0755 "$CHECK"
    restorecon "$CHECK" 2>/dev/null || true
}

install_hands_free() {
    cat >"$AUTO_UNIT" <<'EOF'
[Unit]
Description=osreflog P7d: in a probe, judge Wi-Fi power save, give the verdict, reboot
ConditionKernelCommandLine=osreflog.probe
Wants=network-online.target
After=network-online.target NetworkManager.service

[Service]
Type=oneshot
ExecStart=/usr/local/bin/osreflog-p7d-check
ExecStartPost=/usr/bin/sleep 10
ExecStartPost=/usr/bin/systemctl reboot

[Install]
WantedBy=multi-user.target
EOF
    cat >"$NEXT" <<EOF
#!/bin/sh
# osreflog P7d: on the default system, reboot into the next armed probe (at most 12 times).
n=\$(cat $STATE/reboots 2>/dev/null || echo 0)
for i in \$(seq 300); do
    s=\$(osreflog bisect status --json 2>/dev/null)
    r=\$(printf '%s' "\$s" | python3 -I -c 'import json,sys; s=json.load(sys.stdin) or {}; w=s.get("waiting") or {}; print("done" if not s.get("running") else ("reboot" if w.get("reboot") else "wait"))' 2>/dev/null)
    case "\$r" in
        done) echo "the bisect is over: sudo scripts/p7/p7d-wifi.sh finish"; exit 0 ;;
        reboot)
            [ "\$n" -lt 12 ] || { echo "12 reboots: stopping here"; exit 1; }
            echo \$((n + 1)) > $STATE/reboots
            sleep 20
            exec systemctl reboot ;;
    esac
    sleep 2
done
echo "no armed probe after 10 minutes: osreflog bisect status"
exit 1
EOF
    chmod 0755 "$NEXT"
    cat >"$NEXT_UNIT" <<'EOF'
[Unit]
Description=osreflog P7d: reboot into the next armed probe
ConditionKernelCommandLine=!osreflog.probe
ConditionPathExists=!/var/lib/osreflog-probe/manual
After=osreflogd.service multi-user.target

[Service]
Type=oneshot
ExecStart=/usr/local/bin/osreflog-p7d-next

[Install]
WantedBy=multi-user.target
EOF
    restorecon "$NEXT" "$AUTO_UNIT" "$NEXT_UNIT" 2>/dev/null || true
    systemctl daemon-reload
    systemctl enable -q osreflog-p7d-auto.service osreflog-p7d-next.service
}

start() {
    local hands_free=${1:-}
    command -v osreflog >/dev/null || die "osreflog isn't installed: sudo scripts/install-dev.sh"
    systemctl is-active -q osreflog-boot-doctor.service || systemctl is-enabled -q osreflog-boot-doctor.service \
        || die "osreflog-boot-doctor.service isn't installed: sudo scripts/install-dev.sh"
    local dev; dev=$(wlan)
    [ -n "$dev" ] || die "no Wi-Fi interface"
    nmcli -t -f DEVICE,STATE dev | grep -q "^$dev:connected" || die "$dev isn't connected"
    [ ! -e "$STATE" ] || die "$STATE exists: an earlier run? sudo $0 finish"
    osreflog bisect status --json | grep -q '"running":true' && die "a bisect is running: osreflog bisect status"
    mkdir -p "$STATE"
    echo "$dev" >"$STATE/dev"
    iw dev "$dev" get power_save >"$STATE/power_save.before"

    # The helpers go in before the good checkpoint, so every probe has them.
    install_check "$dev"
    if [ "$hands_free" = --hands-free ]; then install_hands_free; echo "hands-free units installed"; fi
    printf '# osreflog P7d seed\n[connection]\nwifi.powersave = 2\n' >"$CONF"; settle
    local good bad
    good=$(checkpoint "p7d: good, Wi-Fi power save off") || die "checkpoint"
    printf '# osreflog P7d seed: harmless\n[logging]\nlevel=INFO\n' >"$EXTRA"; settle
    sed -i 's/^wifi.powersave = 2$/wifi.powersave = 3/' "$CONF"; settle
    cause_of "$CONF" >"$STATE/expected"
    printf '# osreflog P7d seed: harmless, again\n[logging]\nlevel=INFO\ndomains=WIFI:INFO\n' >"$EXTRA"; settle
    bad=$(checkpoint "p7d: bad, Wi-Fi power save on") || die "checkpoint"
    echo "good $good, bad $bad; power save went on with cause #$(cat "$STATE/expected")"
    [ -n "$(cat "$STATE/expected")" ] || die "the live witness didn't record the cause: is osreflogd capturing? (osreflog doctor)"
    echo "$good $bad" >"$STATE/checkpoints"
    osreflog bisect start --good "$good" --bad "$bad" --tier metal --no-reboot
    echo
    if [ "$hands_free" = --hands-free ]; then
        echo "hands-free: rebooting into the first probe in 20 s (Ctrl-C to stop; sudo $0 finish to clean up)"
        echo 1 >"$STATE/reboots"
        sleep 20
        systemctl reboot
    else
        echo "now: reboot; in the probe run  sudo osreflog-p7d-check  then reboot; repeat until"
        echo "osreflog bisect status shows the result; then  sudo $0 finish"
    fi
}

finish() {
    local status found expected
    status=$(osreflog bisect status --json)
    osreflog bisect log
    found=$(printf '%s' "$status" | python3 -I -c 'import json,sys; s=json.load(sys.stdin) or {}; f=s.get("finding") or {}; print(f.get("culprit", {}).get("id", ""))')
    expected=$(cat "$STATE/expected" 2>/dev/null)
    echo
    if [ -n "$found" ] && [ "$found" = "$expected" ]; then
        echo "P7d: found #$found, the cause that switched Wi-Fi power save on"
        rc=0
    else
        echo "P7d: found '#$found', expected #$expected"
        rc=1
    fi
    printf '%s' "$status" | grep -q '"running":true' && osreflog bisect reset
    systemctl disable -q osreflog-p7d-auto.service osreflog-p7d-next.service 2>/dev/null
    rm -f "$CONF" "$EXTRA" "$CHECK" "$NEXT" "$AUTO_UNIT" "$NEXT_UNIT"
    systemctl daemon-reload
    nmcli general reload conf 2>/dev/null || true
    local dev; dev=$(cat "$STATE/dev" 2>/dev/null)
    [ -n "$dev" ] && nmcli device reapply "$dev" >/dev/null 2>&1
    echo "power save before: $(cat "$STATE/power_save.before" 2>/dev/null); now: $(iw dev "$dev" get power_save 2>&1)"
    rm -rf "$STATE"
    echo "cleaned up (the two checkpoints stay in the history)"
    return $rc
}

case "${1:-}" in
    start) start "${2:-}" ;;
    finish) finish ;;
    *) sed -n '2,22p' "$0" | sed 's/^# \{0,1\}//'; exit 2 ;;
esac
