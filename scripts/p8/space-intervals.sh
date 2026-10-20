#!/bin/bash
# What each stored interval released (the bytes its older snapshot holds that the newer side
# lost), from the ledger, read-only: for reading the space report's sum interval by interval.
#
#   sudo scripts/p8/space-intervals.sh 2>&1 | tee perf-out/space-intervals.log
#
# Mounts the btrfs top level read-only in a private mount namespace (as osreflogd does) and
# reads osreflog/ledger/ledger.sqlite there; touches nothing.
set -euo pipefail
[ "$(id -u)" -eq 0 ] || { echo "run as root: sudo $0" >&2; exit 1; }
if [ -z "${OSREFLOG_NS:-}" ]; then
    exec unshare -m --propagation private env OSREFLOG_NS=1 "$0" "$@"
fi
dev=$(findmnt -n -o SOURCE / | sed 's/\[.*//')
top=$(mktemp -d)
mount -o ro,subvolid=5 "$dev" "$top"
trap 'umount "$top"; rmdir "$top"' EXIT
python3 -I - "$top/osreflog/ledger/ledger.sqlite" <<'PY'
import sqlite3, sys, time
db = sqlite3.connect(f"file:{sys.argv[1]}?mode=ro", uri=True)
rows = db.execute("""
    SELECT f.name, t.name, t.created_ms, t.reason, i.changed_bytes, s.released_bytes,
           f.pruned_ms IS NULL
    FROM intervals i
    JOIN checkpoints f ON f.id = i.from_id
    JOIN checkpoints t ON t.id = i.to_id
    JOIN interval_space s ON s.interval_id = i.id
    ORDER BY i.to_id, i.from_id""").fetchall()
mib = lambda b: f"{b / 1048576:9.1f}"
print(f"{'interval':<20} {'to at':<17} {'reason':<9} {'written MiB':>12} {'released MiB':>13}  from kept")
total = 0
for f, t, ms, reason, written, released, kept in rows:
    total += released
    at = time.strftime("%m-%d %H:%M:%S", time.localtime(ms / 1000))
    print(f"{f + '..' + t:<20} {at:<17} {reason:<9} {mib(written):>12} {mib(released):>13}  {'yes' if kept else 'no'}")
print(f"{len(rows)} intervals measured, {total / 1073741824:.2f} GiB released in all")
PY
