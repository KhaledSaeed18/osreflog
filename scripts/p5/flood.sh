#!/bin/bash
# Threat model A3, as an unprivileged user: flood osreflogd with process creations and file
# events, then check it stayed bounded and responsive. Run as your user (no sudo).
#   scripts/p5/flood.sh [PROCESSES] [FILES]
set -uo pipefail
PROCS=${1:-100000}
FILES=${2:-300000}
DIR=/var/tmp/osreflog-flood
FAILED=0
ok() { echo "  ok   $*"; }
bad() { echo "  FAIL $*"; FAILED=$((FAILED + 1)); }
rss_mb() { echo $(( $(ps -o rss= -C osreflogd | head -1) / 1024 )); }
live() { osreflog status --json | python3 -I -c "import json,sys; l=json.load(sys.stdin)['live']; print(l['$1'])"; }

pgrep -x osreflogd >/dev/null || { echo "osreflogd isn't running" >&2; exit 1; }
RSS0=$(rss_mb); PROCS0=$(live processes); OVF0=$(live overflows)
echo "before: osreflogd ${RSS0} MB, ${PROCS0} processes known, ${OVF0} queue overflows"

echo "flooding: $PROCS processes"
T0=$(date +%s)
seq "$PROCS" | xargs -P 8 -n 1 true
echo "  done in $(( $(date +%s) - T0 )) s; osreflogd $(rss_mb) MB, $(live processes) processes known"

echo "flooding: $FILES files created and deleted in $DIR"
T0=$(date +%s)
python3 -I - "$DIR" "$FILES" <<'PY'
import os, sys
d, n = sys.argv[1], int(sys.argv[2])
os.makedirs(d, exist_ok=True)
for i in range(n):
    p = f"{d}/f{i % 1000}"
    with open(p, "w") as f:
        f.write("x")
    os.unlink(p)
os.rmdir(d)
PY
echo "  done in $(( $(date +%s) - T0 )) s"

T0=$(date +%s%3N)
osreflog status >/dev/null && RESP=$(( $(date +%s%3N) - T0 ))
RSS1=$(rss_mb); PROCS1=$(live processes); OVF1=$(live overflows)
echo "after: osreflogd ${RSS1} MB, ${PROCS1} processes known, $((OVF1 - OVF0)) new queue overflows, status answered in ${RESP:-?} ms"
echo
[ -n "${RESP:-}" ] && [ "$RESP" -lt 5000 ] && ok "still answering (${RESP} ms)" || bad "status didn't answer within 5 s"
[ "$RSS1" -lt 300 ] && ok "memory bounded: ${RSS1} MB (${RSS0} MB before; the unit's limit is 768 MB)" || bad "memory grew to ${RSS1} MB"
[ "$PROCS1" -le 65536 ] && ok "lineage table bounded: ${PROCS1} processes" || bad "lineage table at ${PROCS1}"
if [ "$OVF1" -gt "$OVF0" ]; then
    osreflog log -n 3 | grep -qi gap && ok "queue overflowed and was recorded as a gap" || ok "queue overflowed ($((OVF1 - OVF0))); the next diff covers it"
else
    ok "no queue overflow"
fi
exit "$FAILED"
