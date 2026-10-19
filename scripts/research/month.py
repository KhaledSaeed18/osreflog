#!/usr/bin/python3 -I
"""What changes on this system: the ledger read as a dataset, for docs/research/.

    scripts/research/month.py [--days N] [--out FILE.md]

Reads through the CLI (`osreflog causes`, `show`, `log`, as your user: reading the history is
allowed at the desktop), so it needs no root and sees what the app sees. Prints Markdown.

Anonymised: no host or user names, no command lines (a command cause is named by its program),
no file contents; paths are kept, with anything under a home directory, a UUID or a number that
looks like an id generalised. Causes made by osreflog's own test scripts (scratch paths under
/etc/osreflog-*, commands from scripts/p*) are counted apart, so the rest is the system's own.
"""

import collections
import json
import os
import re
import subprocess
import sys
import time

DAY_MS = 86_400_000


def cli(*args):
    out = subprocess.run(["osreflog", *args, "--json"], capture_output=True, text=True, check=True)
    return json.loads(out.stdout)


def generalise(path):
    path = re.sub(r"^/home/[^/]+", "/home/…", path)
    path = re.sub(r"[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}", "<uuid>", path)
    path = re.sub(r"[0-9a-f]{32,}", "<id>", path)
    path = re.sub(r"\d{5,}", "<n>", path)
    return path


def area(path):
    """The first two levels: /etc/NetworkManager, /var/lib/dnf …"""
    parts = path.split("/")
    return "/".join(parts[:3]) if len(parts) > 3 else path


def is_test(cause, changes):
    cmd = " ".join(cause["actor"].get("command") or [])
    if re.search(r"scripts/p\d|testbed|workload|hardening-check|selinux-workers", cmd):
        return True
    if "osreflog-" in cause.get("label", "") and cause["kind"] == "command":
        return True
    # Scratch files of the tests, and osreflog's own development installs.
    return bool(changes) and all("osreflog" in c["path"] for c in changes)


FLATPAK = "/var/lib/flatpak/"
REPO = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))


def noise_prefixes():
    """Today's noise paths, from the model's source, so older records read as new ones do
    (rsyslog's state file was recorded for a few hours before it became noise)."""
    with open(os.path.join(REPO, "crates/model/src/lib.rs")) as f:
        src = f.read()
    block = re.search(r"const NOISE: &\[&str\] = &\[(.*?)\];", src, re.S).group(1)
    return [p for p in re.findall(r'"([^"]+)"', block)] + [FLATPAK]


def unit_of(cgroup):
    """As osreflogd 0.3.0 reads a cgroup: the unit past any sub-slices of system.slice."""
    rest = cgroup.removeprefix("/system.slice/") if cgroup.startswith("/system.slice/") else None
    if rest is None:
        return None
    unit = next((c for c in rest.split("/") if not c.endswith(".slice")), "")
    return unit if unit.endswith((".service", ".scope")) else None


def who(cause):
    if cause["kind"] == "flatpak":
        return "flatpak"
    if cause["kind"] == "unit":
        return cause["label"]
    # Recorded before 0.3.0 as "unknown": a unit in a sub-slice.
    if cause["kind"] == "unknown" and (unit := unit_of(cause["actor"].get("cgroup", ""))):
        return unit
    if cause["kind"] == "dnf":
        return "dnf"
    if cause["kind"] == "command":
        return f"{cause['actor'].get('comm') or '?'} (a command)"
    if cause["kind"] == "unknown":
        return f"{cause['actor'].get('comm') or '?'} (no unit, no shell)"
    return cause["kind"]


def table(header, rows):
    out = ["| " + " | ".join(header) + " |", "|" + "---|" * len(header)]
    out += ["| " + " | ".join(str(c) for c in r) + " |" for r in rows]
    return "\n".join(out)


def main():
    days = None
    out_path = None
    args = sys.argv[1:]
    while args:
        a = args.pop(0)
        if a == "--days":
            days = int(args.pop(0))
        elif a == "--out":
            out_path = args.pop(0)
        else:
            sys.exit(__doc__)

    causes = cli("causes", "-n", "1000000")
    log = cli("log", "-n", "1000000")
    now = int(time.time() * 1000)
    if days:
        causes = [c for c in causes if c["started_ms"] >= now - days * DAY_MS]
        log = [e for e in log if e["checkpoint"]["created_ms"] >= now - days * DAY_MS]
    if not causes:
        sys.exit("no causes in the ledger for that span")

    detail = {}
    for c in causes:
        try:
            detail[c["id"]] = cli("show", str(c["id"]))["changes"]
        except subprocess.CalledProcessError:
            detail[c["id"]] = []
    tests = {c["id"] for c in causes if is_test(c, detail[c["id"]])}
    # Flatpak's own installation: one opaque cause since 0.3.0 (R4), listed path by path
    # before. Counted apart, so its appstream refreshes don't drown everything else.
    flatpak_paths = 0
    noise = tuple(noise_prefixes())
    noise_dropped = 0
    for c in causes:
        if c["kind"] == "flatpak":
            flatpak_paths += c["package_paths"]
        kept = [ch for ch in detail[c["id"]] if not ch["path"].startswith(FLATPAK)]
        flatpak_paths += len(detail[c["id"]]) - len(kept)
        quiet = [ch for ch in kept if not ch["path"].startswith(noise)]
        noise_dropped += len(kept) - len(quiet)
        detail[c["id"]] = quiet
    flatpak_causes = {c["id"] for c in causes if c["kind"] == "flatpak"}
    empty = {c["id"] for c in causes if not detail[c["id"]] and c["kind"] != "flatpak"
             and c["id"] not in tests}
    system = [c for c in causes
              if c["id"] not in tests and c["id"] not in flatpak_causes and c["id"] not in empty]
    first = min(c["started_ms"] for c in causes)
    last = max(c["ended_ms"] for c in causes)
    span_days = max((last - first) / DAY_MS, 1 / 24)

    lines = [
        f"Span: {time.strftime('%Y-%m-%d', time.localtime(first / 1000))} to "
        f"{time.strftime('%Y-%m-%d', time.localtime(last / 1000))} ({span_days:.1f} days). "
        f"{len(causes)} causes, {len(tests)} of them made by osreflog's own tests and left out "
        f"below; {len(flatpak_causes) + len(empty)} with nothing left once Flatpak's "
        f"installation ({flatpak_paths} paths, counted apart) and paths that are noise today "
        f"({noise_dropped}) are taken out; {len(system)} from the system and the people using "
        f"it, {len(system) / span_days:.1f} a day.",
        "",
        "### Who changes the system",
        "",
    ]
    def kind(c):
        return "unit" if c["kind"] == "unknown" and unit_of(c["actor"].get("cgroup", "")) else c["kind"]

    kinds = collections.Counter(kind(c) for c in system)
    changes_by_kind = collections.Counter()
    for c in system:
        changes_by_kind[kind(c)] += len(detail[c["id"]])
    lines.append(table(
        ["Kind", "Causes", "Changes", "Share of causes"],
        [(k, n, changes_by_kind[k], f"{n / len(system):.0%}") for k, n in kinds.most_common()],
    ))
    lines += ["", "The busiest, by causes:", ""]
    actors = collections.Counter(who(c) for c in system)
    actor_changes = collections.Counter()
    for c in system:
        actor_changes[who(c)] += len(detail[c["id"]])
    lines.append(table(
        ["Who", "Causes", "Changes"],
        [(a, n, actor_changes[a]) for a, n in actors.most_common(15)],
    ))

    paths = collections.Counter()
    areas = collections.Counter()
    ops = collections.Counter()
    for c in system:
        seen = set()
        for ch in detail[c["id"]]:
            p = generalise(ch["path"])
            ops[ch["op"]] += 1
            if p not in seen:
                paths[p] += 1
                seen.add(p)
                areas[area(p)] += 1
    lines += ["", "### What churns", "", "Paths changed by the most causes:", ""]
    lines.append(table(["Path", "Causes"], paths.most_common(20)))
    lines += ["", "By area (first two levels of the path), counting each cause once per path:", ""]
    lines.append(table(["Area", "Path changes"], areas.most_common(15)))
    lines += ["", "Operations: " + ", ".join(f"{n} {op}" for op, n in ops.most_common()) + "."]
    once = sum(1 for n in paths.values() if n == 1)
    lines += [
        "",
        f"{len(paths)} distinct paths; {once} ({once / max(len(paths), 1):.0%}) changed by one cause "
        f"only, {sum(1 for n in paths.values() if n >= 10)} by ten or more.",
    ]

    reasons = collections.Counter(e["checkpoint"]["reason"] for e in log)
    transactions = sum(len(e["interval"]["transactions"]) for e in log if e.get("interval"))
    packages = sum(e["interval"]["packages"] for e in log if e.get("interval"))
    lines += [
        "",
        "### Checkpoints and packages",
        "",
        f"{len(log)} checkpoints: " + ", ".join(f"{n} {r}" for r, n in reasons.most_common()) + ". "
        f"{transactions} dnf transactions changed {packages} packages.",
    ]

    hours = collections.Counter(time.localtime(c["started_ms"] / 1000).tm_hour for c in system)
    busiest = ", ".join(f"{h:02d}:00 ({n})" for h, n in hours.most_common(3))
    lines += ["", f"Busiest hours of the day (local time, causes started): {busiest}."]

    text = "\n".join(lines) + "\n"
    if out_path:
        with open(out_path, "w") as f:
            f.write(text)
    print(text)


if __name__ == "__main__":
    main()
