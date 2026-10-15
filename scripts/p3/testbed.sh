#!/bin/bash
# osreflog P3 testbed: scripted changes on the real machine, in a scratch area, checked
# against the causes osreflogd records. Needs the installed daemon (scripts/install-dev.sh).
#
#   sudo scripts/p3/testbed.sh
#
# Touches only /etc/osreflog-testbed/, a dummy unit osreflog-testbed.service, and the figlet
# package (installed, then removed). Results: /var/tmp/osreflog-p3/results.
set -euo pipefail
[ "$(id -u)" -eq 0 ] || { echo "run with sudo" >&2; exit 1; }
HERE=$(cd "$(dirname "$0")" && pwd)
R=/var/tmp/osreflog-p3/results
T=/etc/osreflog-testbed
UNIT=/etc/systemd/system/osreflog-testbed.service
mkdir -p "$R"
say() { printf '\n\033[1m[p3 %s] %s\033[0m\n' "$(date +%H:%M:%S)" "$*"; }
cleanup() {
    systemctl disable osreflog-testbed.service >/dev/null 2>&1 || true
    rm -f "$UNIT"
    rm -rf "$T"
    systemctl daemon-reload
}
trap cleanup EXIT

osreflog status > "$R/status-before.txt"
grep -q '^live       watching' "$R/status-before.txt" || { cat "$R/status-before.txt"; echo "the live witness is not running" >&2; exit 1; }
osreflog checkpoint -m "testbed start" --json > "$R/cp-start.json"

say "1 sed -i from a shell"
mkdir -p "$T"
printf 'mode = old\nkeep = 1\n' > "$T/sed.conf"
sleep 3   # a separate cause from the sed below
sed -i 's/mode = old/mode = new/' "$T/sed.conf"

say "2 editor-style save (temp file renamed over the target)"
printf 'a = 1\n' > "$T/edit.conf"
sleep 3
python3 -I -c '
import os
d = "/etc/osreflog-testbed"
with open(os.path.join(d, ".edit.conf.tmp"), "w") as f:
    f.write("a = 2\n")
os.rename(os.path.join(d, ".edit.conf.tmp"), os.path.join(d, "edit.conf"))
'

say "3 a systemd service writes"
systemd-run --quiet --wait --unit osreflog-testbed-writer --property=Type=oneshot \
    /bin/sh -c "echo 'from a unit' > $T/unit.conf"

say "4 systemctl enable (pid 1 writes the symlink)"
printf '[Unit]\nDescription=osreflog testbed\n[Service]\nType=oneshot\nExecStart=/bin/true\n[Install]\nWantedBy=multi-user.target\n' > "$UNIT"
systemctl daemon-reload
systemctl enable osreflog-testbed.service 2>/dev/null

say "5 package transaction"
dnf install -y --setopt=install_weak_deps=False figlet > "$R/dnf-install.log" 2>&1
dnf remove -y figlet > "$R/dnf-remove.log" 2>&1

say "6 secrets are never kept"
printf 'not really secret\n' > "$T/server.key"
install -m 0600 /dev/null "$T/private.conf"
printf 'token = 123\n' > "$T/private.conf"

say "7 created and deleted at once (transient)"
sh -c "echo x > $T/transient.tmp; rm -f $T/transient.tmp"

sleep 3
osreflog checkpoint -m "testbed done" --json > "$R/cp-done.json"

say "checking"
for _ in $(seq 60); do
    [ "$(osreflog status --json | python3 -I -c 'import json,sys; print(json.load(sys.stdin)["pending_diffs"])')" = 0 ] && break
    sleep 1
done
start=$(python3 -I -c 'import json; print(json.load(open("'"$R"'/cp-start.json"))["name"])')
done_cp=$(python3 -I -c 'import json; print(json.load(open("'"$R"'/cp-done.json"))["name"])')
osreflog diff "$start..$done_cp" --all --json > "$R/diff.json"
osreflog diff "$start..$done_cp" > "$R/diff.txt"
osreflog causes -n 50 --json > "$R/causes.json"
osreflog causes -n 50 > "$R/causes.txt"
osreflog why "$T/sed.conf" --json > "$R/why-sed.json"
osreflog why "$T/sed.conf" > "$R/why-sed.txt"
osreflog why "$T/edit.conf" > "$R/why-edit.txt"
osreflog why "$T/server.key" --json > "$R/why-server.key.json"
osreflog why "$T/private.conf" --json > "$R/why-private.conf.json"
osreflog status > "$R/status-after.txt"
osreflog verify > "$R/verify.txt"
journalctl -u osreflogd -b --no-pager -o cat | tail -40 > "$R/journal.txt"
chown -R "${SUDO_UID:-0}:${SUDO_GID:-0}" /var/tmp/osreflog-p3
python3 -I "$HERE/check.py" "$R"
