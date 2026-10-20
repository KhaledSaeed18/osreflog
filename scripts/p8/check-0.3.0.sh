#!/bin/bash
# Everything 0.3.0 changed that needs root to check, in one run, before the release:
#
#   cargo build --release && cargo test --release -p osreflog-undo --no-run \
#       && cargo test -p osreflogd --test loopback --no-run                    # as your user
#   sudo scripts/p8/check-0.3.0.sh 2>&1 | tee perf-out/check-0.3.0.log
#
# 1. the loopback engine test (scripts/p2/it.sh): the ledger's schema 7, and the space report
#    on a real btrfs (4 MiB deleted + 1 MiB written over + 1 MiB appended = 5 MiB held);
# 2. the worker domains (scripts/p8/selinux-workers.sh): installs the module and this build,
#    runs the testbed and the live undo workload, collects the denials;
# 3. the seeded bisect suite (scripts/p6/seeded.sh), since the undo and summary workers now
#    start through a different path;
# 4. status and doctor with the space line, and the retention the daemon read.
#
# The soak (scripts/perf/soak-unit.sh) keeps running: the restart is a new segment in it.
# After this, one reboot, then as your user:
#   journalctl -b -u osreflogd -o short-monotonic | head
# should show "live witness watching" before "checkpoint … (boot)".
set -uo pipefail
[ "$(id -u)" -eq 0 ] || { echo "run as root: sudo $0" >&2; exit 1; }
REPO=$(cd "$(dirname "$0")/../.." && pwd)
cd "$REPO"
failed=0
step() {
    local name=$1
    shift
    echo "=== $name"
    if "$@"; then
        echo "--- $name: ok"
    else
        echo "--- $name: FAILED ($?)"
        failed=1
    fi
}
since=$(date '+%Y-%m-%d %H:%M:%S')
# The testbed's causes are labeled with this script's command line (the session's top command).
export OSREFLOG_CHECK_PARENT=check-0.3.0
step "loopback engine test (schema 7, space)" scripts/p2/it.sh
step "worker domains" scripts/p8/selinux-workers.sh
step "seeded bisect" scripts/p6/seeded.sh
echo "=== status and doctor"
osreflog status
osreflog doctor || failed=1
journalctl -u osreflogd --since "$since" --no-pager | grep -E "retention|worker started as|SELinux refused" | sort -u
echo
[ $failed -eq 0 ] && echo "0.3.0 root checks: every check passed" || echo "0.3.0 root checks: a check FAILED"
exit $failed
