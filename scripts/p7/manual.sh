#!/bin/bash
# P7a: a bisect with verdicts by hand, on this machine. Real causes (harmless ones and one bad
# line in a config file) between two checkpoints; `osreflog bisect start`; then, for each probe
# (a booted container), this script plays the person: it looks into the container and gives
# the verdict, alternately inside the probe (`osreflog bisect bad` run in the container) and on
# the host. Half-way, osreflogd is restarted while a probe waits: the session must carry on.
# Everything it changes lives under /etc/osreflog-seed and is removed at the end.
#
#   cargo build --release && sudo scripts/install-dev.sh      # this build's daemon and CLI
#   sudo scripts/p7/manual.sh
set -uo pipefail
[ "$(id -u)" -eq 0 ] || { echo "run with sudo" >&2; exit 1; }
DIR=/etc/osreflog-seed
FAILED=0
ok() { echo "  ok   $*"; }
bad() { echo "  FAIL $*"; FAILED=$((FAILED + 1)); }
settle() { sleep 3; }     # a cause closes after 2 s of quiet
checkpoint() { osreflog checkpoint -m "$1" --json | python3 -I -c 'import json,sys; print(json.load(sys.stdin)["name"])'; }
cause_of() { osreflog why "$1" -n 1 --json | python3 -I -c 'import json,sys; e=json.load(sys.stdin); print(e[0]["cause"]["id"] if e else "")'; }
status() { osreflog bisect status --json; }
field() { python3 -I -c "import json,sys; s=json.load(sys.stdin) or {}; $1"; }
cleanup() { osreflog bisect reset >/dev/null 2>&1; rm -rf "$DIR"; true; }
trap cleanup EXIT
cleanup

echo "== seeding: four causes, the third adds a bad line"
mkdir -p "$DIR"; printf 'port = 80\nworkers = 4\n' >"$DIR/app.conf"; settle
GOOD=$(checkpoint "p7a: good")
echo "# tuned" >>"$DIR/app.conf"; settle
sed -i 's/workers = 4/workers = 8/' "$DIR/app.conf"; settle
echo "mode = broken" >>"$DIR/app.conf"; settle
BADCAUSE=$(cause_of "$DIR/app.conf")
echo "timeout = 30" >>"$DIR/app.conf"; settle
BAD=$(checkpoint "p7a: bad")
echo "  good $GOOD, bad $BAD, the bad line came with cause #$BADCAUSE"

echo "== osreflog bisect start --good $GOOD --bad $BAD"
t0=$SECONDS
osreflog bisect start --good "$GOOD" --bad "$BAD" | sed 's/^/  | /'

# Plays the person until the bisect ends.
i=0
restarted=0
while :; do
    s=$(status)
    running=$(field 'print(s.get("running", False))' <<<"$s")
    [ "$running" = True ] || break
    n=$(field 'w=s.get("waiting"); print(w["probe"] if w else "")' <<<"$s")
    if [ -z "$n" ]; then sleep 1; continue; fi
    m=osreflog-probe-$n
    # Wait until the container's own systemd answers.
    for _ in $(seq 120); do
        systemd-run -M "$m" --wait -q true 2>/dev/null && break
        sleep 1
    done
    if systemd-run -M "$m" --wait -q --pipe /bin/sh -c "grep -q 'mode = broken' $DIR/app.conf" 2>/dev/null; then
        v=bad
    else
        v=good
    fi
    i=$((i + 1))
    if [ "$restarted" = 0 ] && [ "$i" = 2 ]; then
        echo "  -- restarting osreflogd while probe $n waits"
        systemctl restart osreflogd.service
        sleep 2
        after=$(status | field 'w=s.get("waiting"); print(w["probe"] if w else "")')
        if [ "$after" = "$n" ]; then ok "after the restart probe $n still waits"; else bad "after the restart: waiting '$after', expected $n"; fi
        restarted=1
    fi
    if [ $((i % 2)) = 1 ]; then
        echo "  -- probe $n: $v, given inside the container"
        systemd-run -M "$m" --wait -q --pipe /usr/local/bin/osreflog bisect "$v" | sed 's/^/     | /'
        # The daemon picks it up within a second; wait until this probe no longer waits.
        for _ in $(seq 30); do
            w=$(status | field 'w=s.get("waiting"); print(w["probe"] if w else "")')
            [ "$w" != "$n" ] && break
            sleep 1
        done
    else
        echo "  -- probe $n: $v, given on the host"
        osreflog bisect "$v" | sed 's/^/     | /'
    fi
done

s=$(status)
found=$(field 'f=s.get("finding") or {}; print(f.get("culprit", {}).get("id", ""))' <<<"$s")
probes=$(field 'print(len(s.get("probes", [])))' <<<"$s")
echo "== finished in $((SECONDS - t0)) s, $probes probes"
osreflog bisect log | sed 's/^/  | /'
if [ "$found" = "$BADCAUSE" ]; then ok "found #$found"; else bad "found '#$found', expected #$BADCAUSE: $(field 'print(s.get("error") or s.get("finding"))' <<<"$s")"; fi
[ "$restarted" = 1 ] && ok "survived an osreflogd restart" || bad "never restarted osreflogd"
left=$(machinectl list --no-legend | grep -c osreflog-probe || true)
[ "$left" = 0 ] && ok "no probe container left" || bad "$left probe container(s) left"
echo
[ "$FAILED" = 0 ] && echo "P7a manual bisect: all ok" || echo "P7a manual bisect: $FAILED failure(s)"
exit "$FAILED"
