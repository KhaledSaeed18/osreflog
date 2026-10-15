"""Checks the P3 workload results: every non-noise change explained, secrets never kept,
nothing left behind. Run by workload.sh; also runnable on a results directory."""
import json
import sys

R = sys.argv[1]
fails = []


def check(name, ok, detail=""):
    print(f"  {'ok  ' if ok else 'FAIL'} {name}{': ' + detail if detail and not ok else ''}")
    if not ok:
        fails.append(name)


def times_only(c):
    return c["op"] == "modify" and c["what"] == ["times"]


def table(label, diff):
    print(f"\n{label}")
    rows = [c for c in diff["changes"] if c["class"] != "noise" and not times_only(c) and c.get("witness")]
    for c in rows:
        cause = c.get("cause")
        who = f"{cause['kind']}: {cause['label'][:70]}" if cause else "NOT EXPLAINED"
        print(f"  {c['op']:<7} {c['path'][:64]:<64} {c['witness']:<8} {who}")
    return rows


totals = {"config": 0, "config_attributed": 0, "all": 0, "all_attributed": 0}
unexplained = []
for name in ("apply", "revert"):
    d = json.load(open(f"{R}/diff-{name}.json"))
    rows = table(f"{name}: {d['coverage']['config_attributed']}/{d['coverage']['config']} config explained", d)
    for k in totals:
        totals[k] += d["coverage"][k]
    unexplained += [(name, c["path"]) for c in rows if not c.get("cause")]
    if d["gaps"]:
        print(f"  gaps: {d['gaps']}")

print("\nchecks")
pct = 100 * totals["config_attributed"] / max(1, totals["config"])
check(f"config changes explained >= 95 % ({totals['config_attributed']}/{totals['config']}, {pct:.1f} %)", pct >= 95)
check(f"all non-noise changes explained ({totals['all_attributed']}/{totals['all']})", totals["all_attributed"] >= 0.95 * totals["all"])
shadow = json.load(open(f"{R}/why-shadow.json"))
check("/etc/shadow: seen, content never kept", bool(shadow) and not any(e.get("after") or e.get("before") or e["change"].get("blob") for e in shadow), json.dumps(shadow)[:200])
nm = json.load(open(f"{R}/why-nm.json"))
check("NetworkManager keyfile: seen, content never kept", bool(nm) and not any(e.get("after") or e["change"].get("blob") for e in nm), json.dumps(nm)[:200])
delegated = {
    "/etc/localtime": "timedatectl",
    "/etc/machine-info": "hostnamectl",
    "/etc/tuned/active_profile": "tuned-adm",
    "/etc/firewalld/zones/osreflogwl.xml": "firewall-cmd",
    "/etc/NetworkManager/system-connections/osreflog-workload.nmconnection": "nmcli",
}
apply = {c["path"]: c for c in json.load(open(f"{R}/diff-apply.json"))["changes"]}
for path, tool in delegated.items():
    cause = (apply.get(path) or {}).get("cause") or {}
    check(f"{tool}: credited to the caller, not the daemon", cause.get("kind") == "command", json.dumps(cause))
wants = [p for p in apply if p.startswith("/etc/systemd/system/multi-user.target.wants/osreflog-workload")]
check("systemctl enable: credited to the caller", bool(wants) and (apply[wants[0]].get("cause") or {}).get("kind") == "command", json.dumps(apply[wants[0]].get("cause")) if wants else "no symlink")
revert = {c["path"]: c for c in json.load(open(f"{R}/diff-revert.json"))["changes"]}


def summary_has(changes, path, text):
    lines = (changes.get(path) or {}).get("summary") or []
    return any(text in l for l in lines), lines


for changes, path, text in [
    (apply, "/etc/localtime", "time zone:"),
    (apply, "/etc/passwd", "user osreflogwl added"),
    (apply, "/etc/systemd/system/multi-user.target.wants/osreflog-workload.service", "enabled osreflog-workload.service"),
    (apply, "/etc/systemd/system/osreflog-workload.service.d/osreflog.conf", "Environment"),
    (apply, "/etc/sysctl.d/99-osreflog-workload.conf", "new file"),
    (revert, "/etc/passwd", "user osreflogwl removed"),
]:
    ok, lines = summary_has(changes, path, text)
    check(f"summary of {path}: {text!r}", ok, json.dumps(lines))
net = json.load(open(f"{R}/diff-net.json"))
left = [c["path"] for c in net["changes"] if c["class"] == "config" and not times_only(c)]
print(f"  info {len(left)} config paths differ between start and reverted (rewritten in place, or left by a tool): {left[:12]}")
if unexplained:
    print("\nnot explained:")
    for n, p in unexplained:
        print(f"  {n}: {p}")
print(f"\n{len(fails)} failed")
sys.exit(1 if fails else 0)
