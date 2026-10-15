"""Checks the P3 testbed results (run by testbed.sh; also runnable by hand on a results dir)."""
import json
import sys

R = sys.argv[1]
diff = json.load(open(f"{R}/diff.json"))
causes = json.load(open(f"{R}/causes.json"))
why = json.load(open(f"{R}/why-sed.json"))
T = "/etc/osreflog-testbed"
by_path = {c["path"]: c for c in diff["changes"]}
failures = []


def check(name, ok, detail=""):
    print(f"  {'ok  ' if ok else 'FAIL'} {name}{': ' + detail if detail and not ok else ''}")
    if not ok:
        failures.append(name)


def cause_of(path):
    c = by_path.get(path)
    return (c or {}).get("cause") or {}


print("testbed checks")
check("diff has the live witness", "live" in diff["witnesses"], str(diff["witnesses"]))
sed = cause_of(f"{T}/sed.conf")
check("sed.conf: a command cause", sed.get("kind") == "command", json.dumps(by_path.get(f"{T}/sed.conf")))
check("sed.conf: label is the command", "sed" in sed.get("label", "") or "testbed" in sed.get("label", ""), sed.get("label", ""))
check("edit.conf: attributed", bool(cause_of(f"{T}/edit.conf")), json.dumps(by_path.get(f"{T}/edit.conf")))
unit = cause_of(f"{T}/unit.conf")
check("unit.conf: the writer unit", unit.get("kind") == "unit" and "osreflog-testbed-writer" in unit.get("label", ""), json.dumps(unit))
wants = [p for p in by_path if p.startswith("/etc/systemd/system/multi-user.target.wants/osreflog-testbed")]
check("systemctl enable: symlink seen", bool(wants), "")
if wants:
    print(f"       (enable symlink attributed to: {cause_of(wants[0]) or 'nothing (pid 1, see Q6)'})")
check("transient file left no change", f"{T}/transient.tmp" not in by_path)
dnf = [c for c in causes if c["kind"] == "dnf"]
check("dnf transactions are causes", len(dnf) >= 2, f"{len(dnf)} dnf causes")
check("dnf counted package paths", any(c["package_paths"] > 0 for c in dnf))
pk = [p for p in diff["packages"] if p["name"] == "figlet"]
check("figlet installed and removed: no net package change", not pk, json.dumps(pk))
for name in ("server.key", "private.conf"):
    entries = json.load(open(f"{R}/why-{name}.json"))
    leaked = [e for e in entries if e.get("after") or e.get("before") or e["change"].get("blob")]
    check(f"{name}: seen but content never kept", bool(entries) and not leaked, json.dumps(entries)[:300])
check("why sed.conf has before/after content", bool(why) and why[0].get("before") is not None and why[0].get("after") is not None, json.dumps(why[:1])[:300])
if why and why[0].get("after"):
    check("why sed.conf shows the new value", "mode = new" in why[0]["after"])
cov = diff["coverage"]
check("coverage reported", cov["config"] > 0, json.dumps(cov))
print(f"\ncoverage: {cov['config_attributed']}/{cov['config']} config changes, {cov['all_attributed']}/{cov['all']} overall")
unexplained = [c["path"] for c in diff["changes"] if c.get("witness") == "state" and c["class"] == "config"]
if unexplained:
    print("config changes the live witness did not explain:")
    for p in unexplained:
        print(f"  {p}")
print(f"\n{len(failures)} failed")
sys.exit(1 if failures else 0)
