#!/usr/bin/python3 -I
"""P3 exit metric: how many config changes the live witness explained, over every interval it
watched (intervals before it started have no attribution and are skipped).

    scripts/p3/coverage.py            # as your user; needs osreflogd running
"""
import json
import subprocess


def osreflog(*args):
    out = subprocess.run(["osreflog", *args, "--json"], check=True, capture_output=True, text=True)
    return json.loads(out.stdout)


totals = {"config": 0, "config_attributed": 0, "all": 0, "all_attributed": 0}
missed = []
intervals = 0
for entry in osreflog("log", "-n", "10000"):
    iv = entry.get("interval")
    if not iv:
        continue
    to = entry["checkpoint"]["id"]
    d = osreflog("diff", f"c{iv['from_id']}..c{to}", "--all")
    if d["coverage"]["all"] == 0:
        continue
    intervals += 1
    for k in totals:
        totals[k] += d["coverage"][k]
    missed += [
        (f"c{iv['from_id']}..c{to}", c["path"])
        for c in d["changes"]
        if c["class"] == "config" and c.get("witness") == "state" and not (c["op"] == "modify" and c["what"] == ["times"])
    ]

pct = 100 * totals["config_attributed"] / totals["config"] if totals["config"] else 100.0
print(f"intervals watched by the live witness: {intervals}")
print(f"config changes explained: {totals['config_attributed']}/{totals['config']} ({pct:.1f} %), target >= 95 %")
print(f"all non-noise changes explained: {totals['all_attributed']}/{totals['all']}")
if missed:
    print("\nconfig changes without a cause:")
    for rng, path in missed:
        print(f"  {rng}  {path}")
