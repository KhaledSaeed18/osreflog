#!/bin/bash
# The write gate (P5): the undo matrix as root, in a contained scratch root (ADR 0011).
#
#   cargo build --release && cargo test --release -p osreflog-undo --no-run   # as your user
#   sudo scripts/p5/gate.sh
#
# The scratch root is a writable btrfs snapshot of / under /var/cache/osreflog-gate. The tests
# run in a systemd-run sandbox where everything else is read-only (ProtectSystem=strict,
# ProtectHome=read-only), so a resolution bug would fail on a read-only filesystem instead of
# writing to the machine. The snapshot is deleted at the end.
set -euo pipefail
[ "$(id -u)" -eq 0 ] || { echo "run with sudo" >&2; exit 1; }
REPO=$(cd "$(dirname "$0")/../.." && pwd)
# Not under /var/tmp: a sudo session may have a private /var/tmp (pam_namespace) that
# systemd services never see. /var/cache is shared, and osreflog classes it as noise.
AREA=/var/cache/osreflog-gate

newest() { find "$REPO/target/release/deps" -maxdepth 1 -type f -executable -name "$1-*" -printf '%T@ %p\n' | sort -n | tail -1 | cut -d' ' -f2; }
GATE=$(newest gate)
ENGINE=$(newest engine)
DAEMON=$REPO/target/release/osreflogd
for f in "$GATE" "$ENGINE" "$DAEMON"; do
    [ -n "$f" ] && [ -x "$f" ] || { echo "missing test binaries: see the header of $0" >&2; exit 1; }
done

cleanup() {
    btrfs subvolume delete "$AREA/root" >/dev/null 2>&1 || true
    rm -rf "$AREA"
}
trap cleanup EXIT
cleanup
mkdir -p "$AREA/outside" "$AREA/bin"
# Services may not execute files labeled as home content: copy them out, label them bin_t.
install -m 0755 "$GATE" "$AREA/bin/gate"
install -m 0755 "$ENGINE" "$AREA/bin/engine"
install -m 0755 "$DAEMON" "$AREA/bin/osreflogd"
chcon -t bin_t "$AREA"/bin/* 2>/dev/null || true
btrfs subvolume snapshot / "$AREA/root" >/dev/null
echo "scratch root: a writable snapshot of / at $AREA/root"
echo "kernel $(uname -r), SELinux $(getenforce)"

# The daemon's own sandbox: every hardening directive of its unit, so the matrix runs under
# exactly the restrictions the undo worker has (P5 found RestrictSUIDSGID= blocking openat2
# only on the real machine). Then the scratch area is made writable again.
HARDENING=()
while IFS= read -r line; do
    HARDENING+=(-p "$line")
done < <(sed -n '/^\[Service\]/,/^\[/p' "$REPO/data/systemd/osreflogd.service" \
    | grep -E '^(Protect|Restrict|Private|LockPersonality|MemoryDenyWriteExecute|SystemCallArchitectures|SystemCallFilter|NoNewPrivileges|CapabilityBoundingSet|IPAddressDeny)[A-Za-z]*=')
echo "sandbox: osreflogd.service's $(( ${#HARDENING[@]} / 2 )) hardening settings, scratch area writable"
run() {
    local unit=$1; shift
    if ! systemd-run --wait --pipe --collect --quiet --unit="$unit" \
        "${HARDENING[@]}" \
        -p ProtectSystem=strict -p ProtectHome=read-only -p ReadWritePaths="$AREA" \
        -p PrivateTmp=yes -p PrivateNetwork=yes \
        -E OSREFLOG_GATE_ROOT="$AREA/root" -E OSREFLOG_GATE_OUTSIDE="$AREA/outside" \
        -E OSREFLOG_DAEMON="$AREA/bin/osreflogd" \
        "$@"; then
        journalctl -u "$unit" -n 15 --no-pager -o cat | grep -v "^$" | sed 's/^/  journal: /'
        return 1
    fi
}
status=0
echo; echo "== the engine's tests, as root"
run osreflog-gate-engine "$AREA/bin/engine" --test-threads=1 || status=1
echo; echo "== the gate matrix, as root"
run osreflog-gate-matrix "$AREA/bin/gate" --ignored --test-threads=1 --nocapture || status=1
echo
if [ "$(find "$AREA/outside" -type f | wc -l)" -ne 0 ]; then
    echo "FAIL: files appeared outside the scratch root:"; find "$AREA/outside" -type f
    status=1
fi
[ $status -eq 0 ] && echo "write gate matrix: passed" || echo "write gate matrix: FAILED"
exit $status
