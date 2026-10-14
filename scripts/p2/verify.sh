#!/bin/bash
# P2 exit check, one sudo run:
#   1. the loopback integration test (stops here if it fails)
#   2. install the development build (scripts/install-dev.sh) and start osreflogd
#   3. a real package transaction (`dnf remove figlet`, the P1 test package: no download)
#   4. time `osreflog diff @{1}..@{0}` once its interval is computed
#   5. osreflogd's CPU over 60 idle seconds, and its memory
# Build first, as your user:
#   cargo build --release && cargo test -p osreflogd --test loopback --no-run
# Results: /var/tmp/osreflog-p2/results
set -euo pipefail
[ "$(id -u)" -eq 0 ] || { echo "run with sudo" >&2; exit 1; }
REPO=$(cd "$(dirname "$0")/../.." && pwd)
R=/var/tmp/osreflog-p2/results
mkdir -p "$R"
say() { printf '\n\033[1m[p2 %s] %s\033[0m\n' "$(date +%H:%M:%S)" "$*"; }
ms() { date +%s%3N; }

say "1/5 loopback integration test"
"$REPO/scripts/p2/it.sh" 2>&1 | tee "$R/it.log"

say "2/5 install"
"$REPO/scripts/install-dev.sh" 2>&1 | tee "$R/install.log"
sleep 2
osreflog doctor 2>&1 | tee "$R/doctor-before.txt" || true

say "3/5 real transaction: dnf remove figlet"
if rpm -q figlet >/dev/null; then
    t0=$(ms); dnf remove -y figlet 2>&1 | tee "$R/dnf.log"; t1=$(ms)
    echo "dnf_remove_ms $((t1 - t0))" > "$R/metrics.txt"
else
    echo "figlet not installed; using a manual checkpoint pair instead" | tee "$R/dnf.log"
    osreflog checkpoint -m "p2 verify (1)" >/dev/null
    touch /etc/osreflog-p2-marker
    osreflog checkpoint -m "p2 verify (2)" >/dev/null
    rm -f /etc/osreflog-p2-marker
fi

say "4/5 diff latency"
for _ in $(seq 120); do
    pending=$(osreflog status --json | python3 -I -c 'import json,sys; print(json.load(sys.stdin)["pending_diffs"])')
    [ "$pending" = 0 ] && break
    sleep 1
done
t0=$(ms); osreflog diff '@{1}..@{0}' > "$R/diff.txt"; t1=$(ms)
echo "diff_cached_ms $((t1 - t0))" >> "$R/metrics.txt"
t0=$(ms); osreflog diff '@{1}..@{0}' --all --json > "$R/diff-all.json"; t1=$(ms)
echo "diff_all_json_ms $((t1 - t0))" >> "$R/metrics.txt"
t0=$(ms); osreflog diff > "$R/diff-live.txt"; t1=$(ms)
echo "diff_live_ms $((t1 - t0))" >> "$R/metrics.txt"
osreflog log > "$R/log.txt"
osreflog status > "$R/status.txt"
osreflog verify > "$R/verify.txt" || true
cat "$R/diff.txt"

say "5/5 idle cost over 60 s"
pid=$(systemctl show -p MainPID --value osreflogd.service)
tck=$(getconf CLK_TCK)
cpu() { awk '{print $14 + $15}' "/proc/$pid/stat"; }
c0=$(cpu); sleep 60; c1=$(cpu)
echo "idle_cpu_percent $(python3 -I -c "print(round(($c1 - $c0) / $tck / 60 * 100, 3))")" >> "$R/metrics.txt"
grep -E 'VmRSS|VmHWM' "/proc/$pid/status" >> "$R/metrics.txt"
journalctl -u osreflogd -b --no-pager -o short-precise > "$R/journal.txt" || true
ausearch -m avc -ts today > "$R/avc-today.txt" 2>&1 || true
osreflog doctor > "$R/doctor-after.txt" 2>&1 || true

chown -R "${SUDO_UID:-0}:${SUDO_GID:-0}" /var/tmp/osreflog-p2
say "done"
cat "$R/metrics.txt"
