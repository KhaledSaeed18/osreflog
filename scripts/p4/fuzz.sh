#!/bin/sh
# P4c: runs every osreflog-fuzz target in parallel, at the lowest priority, for SECS seconds
# each (default 3600), then the differential vercmp check against rpm's own.
#   scripts/p4/fuzz.sh [SECS] [OUT]
# Logs and findings go to OUT (default fuzz-out/); the exit status is non-zero on any finding.
set -eu
SECS=${1:-3600}
REPO=$(cd "$(dirname "$0")/../.." && pwd)
OUT=${2:-$REPO/fuzz-out}
BIN=$REPO/target/release/osreflog-fuzz
mkdir -p "$OUT"
cargo build --offline --release -q -p osreflog-fuzz --manifest-path "$REPO/Cargo.toml"
TARGETS="semantic stream rpm-header rpm-diff vercmp fanotify refs textdiff"
for t in $TARGETS; do
    nice -n 19 "$BIN" "$t" --secs "$SECS" --out "$OUT" >"$OUT/$t.log" 2>&1 &
done
wait
"$BIN" vercmp-pairs 200000 >"$OUT/vercmp-pairs.tsv"
python3 -I "$REPO/scripts/p4/vercmp_vs_rpm.py" "$OUT/vercmp-pairs.tsv" | tee "$OUT/vercmp-vs-rpm.log"
status=0
for t in $TARGETS; do
    tail -n 1 "$OUT/$t.log"
    grep -q 'findings 0$' "$OUT/$t.log" || status=1
done
grep -q ' 0 mismatches' "$OUT/vercmp-vs-rpm.log" || status=1
exit $status
