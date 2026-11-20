#!/bin/bash
# P8 exit, amended: this laptop is a development machine nobody uses day to day, so a week of
# real history never builds up on it. Instead, run this once in each of a few development
# sessions (on different days, so the history spans them) and the app has a history made by
# real tools to show and screenshot:
#
#   sudo scripts/p8/history.sh 2>&1 | tee -a perf-out/history.log     # as root
#   scripts/p4/user-workload.sh                                        # then as your user
#
# One run, between two checkpoints noted "scripted history, run N start/end":
#   1. dnf upgrade --refresh: the machine's real pending updates (skip with --no-upgrade);
#   2. a package installed and removed with dnf (cowsay);
#   3. a Flatpak app installed and removed (org.gnome.clocks from flathub, skipped offline, when
#      it is already installed, or when its runtime isn't): one counted cause each way;
#   4. the P3 workload: ~22 admin tools change /etc, each change reverted;
#   5. the P5 undo workload: changes found by cause and undone with `osreflog undo`;
#   6. doctor and status.
# Everything except the upgrade is left as it was. Takes ~10 min plus the upgrade.
set -uo pipefail
[ "$(id -u)" -eq 0 ] || { echo "run as root: sudo $0" >&2; exit 1; }
REPO=$(cd "$(dirname "$0")/../.." && pwd)
cd "$REPO"
UPGRADE=1
[ "${1:-}" = --no-upgrade ] && UPGRADE=0
STATE=/var/lib/osreflog-history
mkdir -p "$STATE"
RUN=$(( $(cat "$STATE/runs" 2>/dev/null || echo 0) + 1 ))
APP=org.gnome.clocks
failed=0
step() { # NAME COMMAND...
    local name=$1; shift
    printf '\n=== [%s] %s\n' "$(date '+%F %T')" "$name"
    if "$@"; then echo "--- $name: ok"; else echo "--- $name: FAILED"; failed=$((failed + 1)); fi
    sleep 3   # causes close after 2 s of quiet
}
checkpoint() { osreflog checkpoint -m "scripted history, run $RUN $1"; }

had_cowsay() { rpm -q cowsay >/dev/null 2>&1; }
dnf_pair() {
    if had_cowsay; then echo "cowsay already installed: skipped"; return 0; fi
    dnf install -y -q cowsay && sleep 3 && dnf remove -y -q cowsay
}
flatpak_pair() {
    if flatpak info --system "$APP" >/dev/null 2>&1; then echo "$APP already installed: skipped"; return 0; fi
    local runtime
    runtime=$(flatpak remote-info --system flathub "$APP" 2>/dev/null | sed -n 's/^ *Runtime: //p')
    if [ -z "$runtime" ]; then echo "flathub unreachable: skipped"; return 0; fi
    # Uninstalling the app leaves its runtime behind (GNOME's is ~1 GB with its GL extension),
    # so the step only runs when that runtime is already here.
    if ! flatpak info --system "$runtime" >/dev/null 2>&1; then
        echo "$runtime not installed: skipped (it would stay behind)"; return 0
    fi
    flatpak install -y --noninteractive --system flathub "$APP" && sleep 3 \
        && flatpak uninstall -y --noninteractive --system "$APP"
}

echo "scripted history, run $RUN ($(osreflog --version))"
step "checkpoint (start)" checkpoint start
[ "$UPGRADE" = 1 ] && step "dnf upgrade" dnf upgrade --refresh -y
step "dnf install and remove" dnf_pair
step "flatpak install and remove" flatpak_pair
step "P3 workload (admin tools)" scripts/p3/workload.sh
step "P5 undo workload" scripts/p5/undo-workload.sh
step "checkpoint (end)" checkpoint end
step "doctor" osreflog doctor
osreflog status
echo "$RUN" >"$STATE/runs"

printf '\nscripted history, run %s: ' "$RUN"
if [ "$failed" -eq 0 ]; then echo "all ok"; else echo "$failed step(s) FAILED"; fi
echo "Next, as your user: scripts/p4/user-workload.sh"
[ "$failed" -eq 0 ]
