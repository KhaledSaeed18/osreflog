#!/bin/bash
# The 48-hour soak as a user unit that comes back after a reboot or a logout and carries on
# where it stopped (scripts/perf/soak.py appends to the same CSV and counts only the time it
# sampled). No root.
#
#   scripts/perf/soak-unit.sh start     # install, enable at login, start
#   scripts/perf/soak-unit.sh status    # the unit, and the table from the samples so far
#   scripts/perf/soak-unit.sh remove    # stop, disable, delete the unit (the samples stay)
#
# SOAK_SECS (172800) and SOAK_OUT (perf-out/soak.csv) change the length and the file.
set -euo pipefail

repo=$(cd "$(dirname "$0")/../.." && pwd)
secs=${SOAK_SECS:-172800}
out=${SOAK_OUT:-$repo/perf-out/soak.csv}
unit=osreflog-soak.service
dir=${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user

case ${1:-} in
start)
    mkdir -p "$dir" "$(dirname "$out")"
    cat >"$dir/$unit" <<UNIT
[Unit]
Description=osreflog soak test ($secs s sampled, into $out)
# Written by soak.py when the time is sampled: nothing left to do.
ConditionPathExists=!${out%.csv}.done

[Service]
ExecStart=$repo/scripts/perf/soak.py $secs 60 $out
Restart=on-failure
RestartSec=60

[Install]
WantedBy=default.target
UNIT
    systemctl --user daemon-reload
    systemctl --user enable --now "$unit"
    systemctl --user --no-pager status "$unit" | head -5
    ;;
status)
    systemctl --user --no-pager status "$unit" | head -5 || true
    [ -s "$out" ] && "$repo/scripts/perf/soak.py" --summary "$out"
    [ -e "${out%.csv}.done" ] && echo "done: the table is in ${out%.csv}.md"
    ;;
remove)
    systemctl --user disable --now "$unit" || true
    rm -f "$dir/$unit"
    systemctl --user daemon-reload
    ;;
*)
    sed -n '2,12p' "$0" | sed 's/^# \{0,1\}//'
    exit 2
    ;;
esac
