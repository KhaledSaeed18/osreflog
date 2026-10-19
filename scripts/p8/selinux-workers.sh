#!/bin/bash
# The workers' SELinux domains (0.3.0): installs the osreflog-selinux module built from
# data/selinux and the build in target/release, runs the work that starts both workers, and
# lists what SELinux denied them (dontaudit rules off for the run, so nothing is hidden). The
# domains enforce since 0.3.0's first run of this (one denial, then fixed); any denial here is a
# rule missing.
#
#   cargo build --release && cargo test --release -p osreflog-undo --no-run   # as your user
#   sudo scripts/p8/selinux-workers.sh 2>&1 | tee perf-out/selinux-workers.log
#
# Writes the raw denials to perf-out/selinux-avc.log and audit2allow's reading of them to
# perf-out/selinux-avc.te. The exit status is non-zero if a step failed or a worker didn't run
# in its domain.
set -uo pipefail
[ "$(id -u)" -eq 0 ] || { echo "run as root: sudo $0" >&2; exit 1; }
REPO=$(cd "$(dirname "$0")/../.." && pwd)
cd "$REPO"
OUT=$REPO/perf-out
mkdir -p "$OUT"
since_audit=$(date '+%m/%d/%Y %H:%M:%S')
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

[ "$(getenforce)" = Enforcing ] || echo "note: SELinux is $(getenforce), not Enforcing"
build=$(mktemp -d)
# dontaudit rules hide some denials even from the log; off for this run, so a domain that
# enforces can't be refused anything unseen. Rebuilt with them at the end.
semodule -DB
trap 'semodule -B; rm -rf "$build"' EXIT
cp data/selinux/osreflog.{te,fc,if} "$build/"
step "build the module" make -s -C "$build" -f /usr/share/selinux/devel/Makefile osreflog.pp
step "install the module" semodule -i "$build/osreflog.pp"
step "install the build" scripts/install-dev.sh
step "label osreflog's files" restorecon -R /var/lib/osreflog /run/osreflog
step "the ledger has its type" bash -c 'ls -Zd /var/lib/osreflog | grep -q osreflog_var_lib_t'
step "the module is loaded" bash -c 'semodule -l | grep -qx osreflog'
step "doctor" osreflog doctor
# The testbed's causes are summarized (the summarizer); the live undo workload undoes some.
step "P3 testbed (summaries)" env OSREFLOG_CHECK_PARENT="${OSREFLOG_CHECK_PARENT:+$OSREFLOG_CHECK_PARENT,}selinux-workers" scripts/p3/testbed.sh
step "P5 live undo (the undo worker)" scripts/p5/undo-workload.sh

echo "== the domains the workers ran in"
# Read once into a variable: `journalctl | grep -q` fails under pipefail when grep stops early.
log=$(journalctl -u osreflogd --since "$since" --no-pager)
grep "worker started as" <<<"$log" | sort -u
for domain in osreflog_summary_t osreflog_undo_t; do
    if grep -q "worker started as .*:$domain:" <<<"$log"; then
        echo "-- $domain: ok"
    else
        echo "-- $domain: no worker ran in it"
        failed=1
    fi
done
if grep "SELinux refused" <<<"$log"; then
    failed=1
fi

echo "== what SELinux would have denied them"
ausearch -m AVC,USER_AVC,SELINUX_ERR -ts "$since_audit" 2>/dev/null |
    grep -E "osreflog_(summary|undo)_t" >"$OUT/selinux-avc.log"
if [ -s "$OUT/selinux-avc.log" ]; then
    grep -oE 'denied +\{[^}]*\} for .* scontext=[^ ]* tcontext=[^ ]* tclass=[^ ]*' "$OUT/selinux-avc.log" |
        sed -E 's/ (pid|ino|dev|path|name|comm)=[^ ]*//g' | sort | uniq -c | sort -rn
    audit2allow -i "$OUT/selinux-avc.log" >"$OUT/selinux-avc.te" 2>/dev/null || true
    echo "-- $(wc -l <"$OUT/selinux-avc.log") denials, in perf-out/selinux-avc.log; rules in perf-out/selinux-avc.te"
else
    echo "-- none"
fi

echo
[ $failed -eq 0 ] && echo "SELinux workers: every check passed" || echo "SELinux workers: a check FAILED"
exit $failed
