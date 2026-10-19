#!/usr/bin/python3 -I
"""What osreflog costs over hours or days of normal use: CPU and memory of osreflogd and
osreflog-user, and how much they recorded, sampled every INTERVAL seconds. No root.

    scripts/perf/soak.py SECS [INTERVAL] [OUT.csv]
    scripts/perf/soak.py --summary OUT.csv [INTERVAL]   # the table from the samples so far
    scripts/perf/soak-unit.sh start                     # 48 h that survive reboots

The samples carry wall-clock times and are appended to OUT.csv, so a run stopped by a
shutdown or a logout picks up where it left when started again with the same OUT.csv: SECS
counts the time sampled, not the time the machine was off. A gap longer than three intervals
is the machine off (or the run stopped); a process that restarts starts a new segment, its CPU
time and live counters from zero again, and the summary adds the segments up.

At the end (or on SIGTERM/SIGINT) a summary in Markdown goes next to the CSV (OUT.md), for
docs/performance.md.
"""

import csv
import os
import re
import signal
import subprocess
import sys
import time

TICK = os.sysconf("SC_CLK_TCK")
NAMES = ("osreflogd", "osreflog-user")
COLS = ["t", "process", "pid", "start", "cpu_ticks", "rss", "hwm", "checkpoints", "snapshots",
        "chain", "events", "changes"]


def pids(name):
    out = []
    for d in os.listdir("/proc"):
        if not d.isdigit():
            continue
        try:
            with open(f"/proc/{d}/comm") as f:
                if f.read().strip() == name[:15]:
                    out.append(int(d))
        except OSError:
            pass
    return out


def sample(pid):
    """(start ticks, cpu ticks, rss bytes, peak rss bytes) for one process, or None."""
    try:
        with open(f"/proc/{pid}/stat") as f:
            fields = f.read().rsplit(")", 1)[1].split()
        with open(f"/proc/{pid}/status") as f:
            status = f.read()
    except OSError:
        return None
    # Fields after the comm: state is [0], utime [11], stime [12], starttime [19].
    cpu = int(fields[11]) + int(fields[12])
    start = int(fields[19])

    def kib(key):
        m = re.search(rf"^{key}:\s+(\d+) kB", status, re.M)
        return int(m.group(1)) * 1024 if m else 0

    return start, cpu, kib("VmRSS"), kib("VmHWM")


def status():
    """Counters from `osreflog status`: checkpoints, chain entries, live changes recorded."""
    try:
        out = subprocess.run(["osreflog", "status"], capture_output=True, text=True, timeout=30).stdout
    except (OSError, subprocess.TimeoutExpired):
        return {}
    found = {}
    for key, pattern in (
        ("checkpoints", r"checkpoints\s+(\d+)"),
        ("snapshots", r"\((\d+) snapshots kept\)"),
        ("chain", r"ledger\s+(\d+) chain entries"),
        ("events", r"watching: (\d+) events"),
        ("changes", r"(\d+) changes recorded"),
    ):
        m = re.search(pattern, out)
        if m:
            found[key] = int(m.group(1))
    return found


def load(path):
    """The samples already in OUT.csv, as dicts of ints (empty strings stay strings)."""
    try:
        with open(path, newline="") as f:
            r = csv.DictReader(f)
            if r.fieldnames != COLS:
                return None
            rows = []
            for row in r:
                rows.append({k: (v if k == "process" or v == "" else int(v)) for k, v in row.items()})
            return rows
    except FileNotFoundError:
        return []


def sampled(times, interval):
    """Seconds covered by samples, leaving out gaps longer than three intervals."""
    times = sorted(set(times))
    return sum(b - a for a, b in zip(times, times[1:]) if b - a <= 3 * interval)


def main():
    if len(sys.argv) > 2 and sys.argv[1] == "--summary":
        path = sys.argv[2]
        interval = int(sys.argv[3]) if len(sys.argv) > 3 else 60
        rows = load(path)
        if not rows:
            sys.exit(f"{path}: no samples in this format")
        summarize(rows, path, interval)
        return
    secs = int(sys.argv[1]) if len(sys.argv) > 1 else 3600
    interval = int(sys.argv[2]) if len(sys.argv) > 2 else 60
    path = sys.argv[3] if len(sys.argv) > 3 else f"soak-{int(time.time())}.csv"
    stop = []
    signal.signal(signal.SIGTERM, lambda *_: stop.append(1))
    signal.signal(signal.SIGINT, lambda *_: stop.append(1))

    rows = load(path)
    if rows is None:
        sys.exit(f"{path}: not a soak CSV of this version; pick another file")
    times = [r["t"] for r in rows]
    with open(path, "a", newline="") as f:
        w = csv.writer(f)
        if not rows:
            w.writerow(COLS)
        while not stop and sampled(times, interval) < secs:
            now = int(time.time())
            counters = status()
            for name in NAMES:
                for pid in pids(name):
                    s = sample(pid)
                    if s is None:
                        continue
                    row = [now, name, pid, *s] + [counters.get(k, "") for k in COLS[7:]]
                    w.writerow(row)
                    rows.append(dict(zip(COLS, row)))
            times.append(now)
            f.flush()
            for _ in range(interval):
                if stop:
                    break
                time.sleep(1)
    summarize(rows, path, interval)
    if not stop:
        # Done: a unit started with Restart= or at login has nothing left to do.
        open(os.path.splitext(path)[0] + ".done", "w").close()


def segments(rows):
    """Consecutive runs of one process (same pid and start time), in time order."""
    out = []
    for r in sorted(rows, key=lambda r: r["t"]):
        if out and out[-1][-1]["pid"] == r["pid"] and out[-1][-1]["start"] == r["start"]:
            out[-1].append(r)
        else:
            out.append([r])
    return out


def summarize(rows, path, interval):
    mb = lambda b: f"{b / 1048576:.0f} MB"
    times = sorted({r["t"] for r in rows})
    hours = sampled(times, interval) / 3600
    span = (times[-1] - times[0]) / 3600 if times else 0
    gaps = sum(1 for a, b in zip(times, times[1:]) if b - a > 3 * interval)
    lines = [
        f"Soak test: {hours:.1f} h sampled over {span:.1f} h, a sample every {interval} s, from "
        f"{time.strftime('%Y-%m-%d %H:%M', time.localtime(times[0]))} to "
        f"{time.strftime('%Y-%m-%d %H:%M', time.localtime(times[-1]))}"
        + (f", {gaps} gap{'s' if gaps != 1 else ''} with the machine off or the run stopped" if gaps else "")
        + f" (`scripts/perf/soak.py`, raw samples `{os.path.basename(path)}`).",
        "",
        "| Process | Runs | Average CPU (of one CPU) | Busiest interval | Memory after 1 h | Memory at the end | Peak |",
        "|---|---|---|---|---|---|---|",
    ]
    for name in NAMES:
        segs = segments([r for r in rows if r["process"] == name])
        if not segs:
            lines.append(f"| {name} | not running | | | | | |")
            continue
        busy_ticks, busy_secs, worst = 0, 0, 0.0
        for seg in segs:
            for a, b in zip(seg, seg[1:]):
                dt = b["t"] - a["t"]
                if dt <= 0 or dt > 3 * interval:
                    continue  # the machine was suspended or off
                dc = b["cpu_ticks"] - a["cpu_ticks"]
                busy_ticks += dc
                busy_secs += dt
                worst = max(worst, dc / TICK / dt)
        avg = busy_ticks / TICK / busy_secs if busy_secs else 0.0
        # The longest run says the most about growth: its memory an hour in and at its end.
        longest = max(segs, key=lambda s: s[-1]["t"] - s[0]["t"])
        hour = next((r for r in longest if r["t"] - longest[0]["t"] >= 3600), longest[0])
        peak = max(r["hwm"] for r in rows if r["process"] == name)
        lines.append(
            f"| {name} | {len(segs)} | {avg:.2%} | {worst:.1%} | {mb(hour['rss'])} | {mb(longest[-1]['rss'])} | {mb(peak)} |"
        )
    # Checkpoints and chain entries only grow; the live counters belong to one daemon run.
    daemon = [r for r in rows if r["process"] == "osreflogd" and r["checkpoints"] != ""]
    if daemon:
        first, last = daemon[0], daemon[-1]
        changes = 0
        for seg in segments(daemon):
            withc = [r for r in seg if r["changes"] != ""]
            if withc:
                changes += withc[-1]["changes"] - withc[0]["changes"]
        lines += [
            "",
            f"Recorded meanwhile: {last['checkpoints'] - first['checkpoints']} checkpoints, "
            f"{last['chain'] - first['chain']} ledger entries, {changes} live changes.",
        ]
    out = os.path.splitext(path)[0] + ".md"
    with open(out, "w") as f:
        f.write("\n".join(lines) + "\n")
    print("\n".join(lines))


if __name__ == "__main__":
    main()
