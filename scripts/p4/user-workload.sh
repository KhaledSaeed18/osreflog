#!/bin/bash
# P4d: settings changes made the ways people make them, each checked in `osreflog user`.
# Run as your user (not root) with the agent running:
#   systemctl --user enable --now osreflog-user
#   scripts/p4/user-workload.sh
# Everything it changes is reverted; test keys live under /org/osreflog/workload/.
set -euo pipefail
[ "$(id -u)" -ne 0 ] || { echo "run as your user, not root" >&2; exit 1; }
systemctl --user is-active --quiet osreflog-user || {
    echo "osreflog-user isn't running: systemctl --user enable --now osreflog-user" >&2
    exit 1
}
OSREFLOG=$(command -v osreflog || echo "$(dirname "$0")/../../target/release/osreflog")
DIR=/org/osreflog/workload/
START_MS=$(date +%s%3N)
step() { printf '  %s\n' "$*"; }

echo "workload"
CLOCK=$(gsettings get org.gnome.desktop.interface clock-show-seconds)
FLIPPED=$([ "$CLOCK" = true ] && echo false || echo true)
step "gsettings from a shell: clock-show-seconds $CLOCK -> $FLIPPED"
gsettings set org.gnome.desktop.interface clock-show-seconds "$FLIPPED"
sleep 2.5
step "gsettings from a shell: back to $CLOCK"
gsettings set org.gnome.desktop.interface clock-show-seconds "$CLOCK"

step "an app (started the way GNOME starts apps) writes a key"
systemd-run --user --wait --quiet --unit="app-gnome-org.osreflog.Probe-$$" \
    dconf write "${DIR}theme" "'dark'"

step "dconf load: several keys in one write"
printf "[/]\na=1\nb='x'\n\n[sub]\nc=true\n" | dconf load "$DIR"

step "a key whose value must not be shown"
dconf write "${DIR}api-key" "'s3cret-value'"
sleep 2.5

step "dconf reset -f: the whole directory at once"
dconf reset -f "$DIR"
sleep 3

echo
"$OSREFLOG" user "$DIR" -n 20 --json >"${TMPDIR:-/tmp}/osreflog-user-workload.json"
"$OSREFLOG" user /org/gnome/desktop/interface/clock-show-seconds -n 4 --json \
    >"${TMPDIR:-/tmp}/osreflog-user-clock.json"
python3 -I - "$START_MS" "${TMPDIR:-/tmp}/osreflog-user-workload.json" \
    "${TMPDIR:-/tmp}/osreflog-user-clock.json" "$CLOCK" "$FLIPPED" <<'EOF'
import json, sys

start, wl, clock_file, clock, flipped = sys.argv[1:]
start = int(start)
causes = [d for d in json.load(open(wl)) + json.load(open(clock_file))
          if d["cause"]["started_ms"] >= start]
failed = 0

def check(ok, what):
    global failed
    print(("  ok   " if ok else "  FAIL ") + what)
    failed += not ok

def find(key):
    return [(d["cause"], c) for d in causes for c in d["changes"]
            if c["path"] == "dconf:" + key]

clock_key = "/org/gnome/desktop/interface/clock-show-seconds"
hits = find(clock_key)
check(any(c["kind"] == "command" and c["label"].startswith("gsettings set")
          and ch["summary"] == [f"{clock} → {flipped}"] for c, ch in hits),
      f"gsettings from a shell: a command cause, '{clock} → {flipped}'")
check(any(ch["summary"] == [f"{flipped} → {clock}"] for _, ch in hits),
      f"the revert: '{flipped} → {clock}'")

theme = find("/org/osreflog/workload/theme")
check(any(c["kind"] == "app" and c["label"] == "org.osreflog.Probe"
          and ch["summary"] == ["set to 'dark'"] for c, ch in theme),
      "an app's write: credited to the app (org.osreflog.Probe), 'set to 'dark''")

load = [d for d in causes if d["cause"]["label"].startswith("dconf load")]
keys = sorted(c["path"] for d in load for c in d["changes"])
want = ["dconf:/org/osreflog/workload/a", "dconf:/org/osreflog/workload/b",
        "dconf:/org/osreflog/workload/sub/c"]
check(len(load) == 1 and keys == want, f"dconf load: one cause with its 3 keys ({keys})")

secret = find("/org/osreflog/workload/api-key")
check(any(ch["summary"] == ["set to ***"] for _, ch in secret), "api-key: value shown as ***")
check("s3cret-value" not in json.dumps(causes), "the secret value appears nowhere")

reset = [d for d in causes if d["cause"]["label"].startswith("dconf reset")]
deleted = sorted(c["path"] for d in reset for c in d["changes"] if c["op"] == "delete")
check(len(reset) == 1 and len(deleted) == 5, f"dconf reset -f: one cause, 5 keys removed ({len(deleted)})")
check(all(c["cause"]["actor"]["via"].startswith("dconf-service") for c in causes),
      "every cause records that dconf-service wrote it")

print(f"\n{len(causes)} causes checked, {failed} failed" if failed else f"\n{len(causes)} causes checked, all ok")
sys.exit(1 if failed else 0)
EOF
