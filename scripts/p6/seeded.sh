#!/bin/bash
# P6: the seeded-regression suite. Each scenario makes a chain of real causes on this machine
# (harmless ones and one that breaks a test), with checkpoints along the way, then asks
# `osreflog bisect run` to find the breaking one, and checks the answer, the probe count and
# the probe times. Everything it changes lives under /etc/osreflog-seed (plus one test unit,
# a sysctl.d and a modprobe.d drop-in, and figlet) and is removed at the end.
#
#   sudo scripts/p6/seeded.sh [SCENARIO...]     (osreflogd installed and running)
#   scenarios: config unit sysctl package module
set -uo pipefail
[ "$(id -u)" -eq 0 ] || { echo "run with sudo" >&2; exit 1; }
DIR=/etc/osreflog-seed
UNIT=osreflog-seed.service
FAILED=0
SUMMARY=()
ok() { echo "  ok   $*"; }
bad() { echo "  FAIL $*"; FAILED=$((FAILED + 1)); }
settle() { sleep 3; }     # a cause closes after 2 s of quiet
checkpoint() { osreflog checkpoint -m "$1" --json | python3 -I -c 'import json,sys; print(json.load(sys.stdin)["name"])'; }
cause_of() { osreflog why "$1" -n 1 --json | python3 -I -c 'import json,sys; e=json.load(sys.stdin); print(e[0]["cause"]["id"] if e else "")'; }

cleanup() {
    systemctl unmask "$UNIT" >/dev/null 2>&1
    rm -f "/etc/systemd/system/$UNIT" /etc/sysctl.d/90-osreflog-seed.conf /etc/modprobe.d/osreflog-seed.conf
    systemctl daemon-reload
    rm -rf "$DIR"
    rpm -q figlet >/dev/null 2>&1 && dnf remove -y -q figlet >/dev/null 2>&1
    true
}

# bisect NAME TIER EXPECTED_CAUSE GOOD BAD TEST...
bisect() {
    local name=$1 tier=$2 expected=$3 good=$4 bad=$5; shift 5
    echo "  bisect --good $good --bad $bad --tier $tier -- $*"
    local out
    out=$(osreflog bisect run --good "$good" --bad "$bad" --tier "$tier" --json -- "$@" 2>&1)
    OSREFLOG_BISECT_OUT="$out" python3 -I - "$name" "$expected" <<'PY'
import json, math, os, sys
name, expected = sys.argv[1], int(sys.argv[2] or 0)
raw = os.environ["OSREFLOG_BISECT_OUT"]
try:
    s = json.loads(raw)
except ValueError:
    print(f"  FAIL {name}: no result: {raw[-400:]}"); sys.exit(1)
for line in s["log"]:
    print("   ", line)
f = s.get("finding") or {}
probes = s["probes"]
cause_probes = [p for p in probes if p["what"].startswith("after")]
cp_probes = [p for p in probes if p["what"].startswith("checkpoint") and "bad end" not in p["what"]]
n_causes = next((int(l.split()[0]) for l in s["log"] if " causes between " in l), 0)
n_cps = next((int(l.split(": ")[1].split()[0]) for l in s["log"] if " checkpoints, tier " in l), 2)
bound = math.ceil(math.log2(max(n_causes, 1))) + 1
worst = max((p["ms"] for p in probes), default=0)
rss = max((p.get("peak_rss_kib", 0) for p in probes), default=0) // 1024
found = f.get("culprit", {}).get("id")
ok = f.get("found") == "cause" and found == expected
print(f"  {'ok  ' if ok else 'FAIL'} {name}: found #{found} (expected #{expected})")
fits = len(cause_probes) <= bound
print(f"  {'ok  ' if fits else 'FAIL'} {name}: {len(cause_probes)} cause probes for {n_causes} causes (bound ⌈log₂ n⌉+1 = {bound}), "
      f"{len(cp_probes)} checkpoint probes for {n_cps} checkpoints, 1 bad-end check")
if f.get("approximate"):
    print(f"  info {name}: approximate replays: {f['approximate']}")
print(f"  info {name}: slowest probe {worst/1000:.1f} s" + (f", qemu peak {rss} MiB" if rss else ""))
with open("/run/osreflog-seeded.tsv", "a") as t:
    t.write(f"{name}\t{ok and fits}\t{len(probes)}\t{n_causes}\t{worst}\t{rss}\n")
sys.exit(0 if ok and fits else 1)
PY
    [ $? -eq 0 ] || FAILED=$((FAILED + 1))
}

scenario_config() {
    echo; echo "== config: a bad line in a config file (nspawn), with checkpoints in between"
    mkdir -p "$DIR"; printf 'port = 80\nworkers = 4\n' >"$DIR/app.conf"; settle
    local good; good=$(checkpoint "seed config: good")
    printf 'theme = dark\n' >"$DIR/ui.conf"; settle
    sed -i 's/workers = 4/workers = 8/' "$DIR/app.conf"; settle
    checkpoint "seed config: middle 1" >/dev/null
    printf 'level = info\n' >"$DIR/log.conf"; settle
    sed -i 's/^port = 80$/port = eighty/' "$DIR/app.conf"; settle
    local culprit; culprit=$(cause_of "$DIR/app.conf")
    printf 'theme = light\n' >"$DIR/ui.conf"; settle
    checkpoint "seed config: middle 2" >/dev/null
    printf 'level = debug\n' >"$DIR/log.conf"; settle
    touch "$DIR/marker"; settle
    local bad; bad=$(checkpoint "seed config: bad")
    bisect config nspawn "$culprit" "$good" "$bad" "grep -qE '^port = [0-9]+$' $DIR/app.conf"
}

scenario_unit() {
    echo; echo "== unit: a masked unit (nspawn; pid 1 writes the link)"
    # No systemd runs in an nspawn command probe: systemctl reads the files with --root=/.
    mkdir -p "$DIR"
    printf '[Unit]\nDescription=osreflog seed\n[Service]\nType=oneshot\nExecStart=/bin/true\n[Install]\nWantedBy=multi-user.target\n' >"/etc/systemd/system/$UNIT"
    systemctl daemon-reload; settle
    local good; good=$(checkpoint "seed unit: good")
    printf 'a\n' >"$DIR/unit-a"; settle
    systemctl mask "$UNIT" 2>/dev/null; settle
    local culprit; culprit=$(cause_of "/etc/systemd/system/$UNIT")
    printf 'b\n' >"$DIR/unit-b"; settle
    printf 'c\n' >"$DIR/unit-c"; settle
    local bad; bad=$(checkpoint "seed unit: bad")
    bisect unit nspawn "$culprit" "$good" "$bad" "test \"\$(systemctl --root=/ is-enabled $UNIT 2>/dev/null)\" != masked"
}

scenario_sysctl() {
    echo; echo "== sysctl: a drop-in (nspawn)"
    mkdir -p "$DIR"
    local good; good=$(checkpoint "seed sysctl: good")
    printf 'x\n' >"$DIR/s1"; settle
    printf 'y\n' >"$DIR/s2"; settle
    printf 'vm.swappiness = 100\n' >/etc/sysctl.d/90-osreflog-seed.conf; settle
    local culprit; culprit=$(cause_of /etc/sysctl.d/90-osreflog-seed.conf)
    printf 'z\n' >"$DIR/s3"; settle
    local bad; bad=$(checkpoint "seed sysctl: bad")
    bisect sysctl nspawn "$culprit" "$good" "$bad" "! grep -rqsE 'vm.swappiness *= *100' /etc/sysctl.d"
}

scenario_package() {
    echo; echo "== package: a dnf transaction (nspawn; replayed from the bad checkpoint as a unit)"
    mkdir -p "$DIR"
    local good; good=$(checkpoint "seed package: good")
    printf 'p\n' >"$DIR/p1"; settle
    dnf install -y -q --setopt=install_weak_deps=False figlet >/dev/null 2>&1; settle; settle
    local culprit
    culprit=$(osreflog causes -n 15 --json | python3 -I -c 'import json,sys; print(next(c["id"] for c in json.load(sys.stdin) if c["kind"]=="dnf"))')
    printf 'q\n' >"$DIR/p2"; settle
    local bad; bad=$(checkpoint "seed package: bad")
    bisect package nspawn "$culprit" "$good" "$bad" "! test -x /usr/bin/figlet"
}

scenario_module() {
    echo; echo "== module: a kernel module option (vm: only a booted kernel reads it)"
    # numdummies=3 makes dummy0..dummy2 (the default is one); the parameter isn't in sysfs.
    mkdir -p "$DIR"
    local good; good=$(checkpoint "seed module: good")
    printf 'm1\n' >"$DIR/m1"; settle
    printf 'options dummy numdummies=3\n' >/etc/modprobe.d/osreflog-seed.conf; settle
    local culprit; culprit=$(cause_of /etc/modprobe.d/osreflog-seed.conf)
    printf 'm2\n' >"$DIR/m2"; settle
    local bad; bad=$(checkpoint "seed module: bad")
    bisect module vm "$culprit" "$good" "$bad" "modprobe dummy && ! ip link show dummy2 >/dev/null 2>&1"
}

pgrep -x osreflogd >/dev/null || { echo "osreflogd isn't running" >&2; exit 1; }
cleanup
rm -f /run/osreflog-seeded.tsv
osreflog bisect reset >/dev/null 2>&1
for s in "${@:-config unit sysctl package module}"; do
    for one in $s; do "scenario_$one"; done
done
cleanup
echo
echo "scenario  ok  probes  causes  slowest-ms  qemu-MiB"
column -t /run/osreflog-seeded.tsv 2>/dev/null || cat /run/osreflog-seeded.tsv
echo
if [ "$FAILED" -eq 0 ]; then echo "seeded suite: all ok"; else echo "seeded suite: $FAILED failed"; fi
exit "$FAILED"
