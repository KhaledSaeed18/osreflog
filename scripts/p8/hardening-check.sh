#!/bin/bash
# P8f: checks that the hardened units (system call filters, MALLOC_ARENA_MAX) leave every part
# of osreflogd working, on this machine, with the build in target/release installed first.
#
#   cargo build --release && cargo test --release -p osreflog-undo --no-run   # as your user
#   sudo scripts/p8/hardening-check.sh 2>&1 | tee perf-out/hardening-check.log
#
# Runs, in order: install-dev, doctor, the P3 testbed (fanotify + eBPF lineage under the new
# filter), the write gate and the live undo workload (P5), the seeded bisect suite (P6:
# containers and a VM), the boot doctor once, then the daemon's memory around two large diffs,
# and finally anything the filter refused (EPERM in the journal). A line per step; the exit
# status is non-zero if any step failed.
set -uo pipefail
[ "$(id -u)" -eq 0 ] || { echo "run as root: sudo $0" >&2; exit 1; }
REPO=$(cd "$(dirname "$0")/../.." && pwd)
cd "$REPO"
since=$(date '+%Y-%m-%d %H:%M:%S')
failed=0
step() {
    local name=$1
    shift
    echo "== $name"
    if "$@"; then
        echo "-- $name: ok"
    else
        echo "-- $name: FAILED ($?)"
        failed=1
    fi
}
rss() { awk '/^VmRSS/ {print $2 / 1024 " MB"}' "/proc/$(pidof osreflogd)/status"; }

step "install the build" scripts/install-dev.sh
step "units carry the hardening" bash -c '
    systemctl show osreflogd -p SystemCallFilter | grep -q "SystemCallFilter=." &&
    systemctl show osreflogd -p Environment | grep -q MALLOC_ARENA_MAX=2'
step "doctor" osreflog doctor
step "P3 testbed (capture and lineage)" scripts/p3/testbed.sh
step "P5 write gate" scripts/p5/gate.sh
step "P5 live undo" scripts/p5/undo-workload.sh
step "P6 seeded bisect" scripts/p6/seeded.sh
step "boot doctor" systemctl start osreflog-boot-doctor.service

echo "== memory around two large diffs"
# The fourth newest checkpoint: its snapshot is still kept, and the diff to now is a big one.
oldest=$(osreflog log 2>/dev/null | awk '/^c[0-9]/ && ++n == 4 {print $1}')
echo "daemon before: $(rss)"
osreflog diff "$oldest.." >/dev/null
osreflog diff "$oldest.." --all >/dev/null
sleep 5
echo "daemon after two diffs from $oldest to now: $(rss)"

echo "== refused by the filter since $since"
if journalctl -u osreflogd -u osreflog-boot-doctor --since "$since" --no-pager |
    grep -iE "operation not permitted|EPERM|seccomp"; then
    echo "-- something was refused: read the lines above"
    failed=1
else
    echo "-- nothing refused"
fi

echo
[ $failed -eq 0 ] && echo "P8f hardening: every check passed" || echo "P8f hardening: a check FAILED"
exit $failed
