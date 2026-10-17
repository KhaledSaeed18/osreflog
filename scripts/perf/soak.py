#!/usr/bin/python3 -I
"""What osreflog costs over hours or days of normal use: CPU and memory of osreflogd and
osreflog-user, and how much they recorded, sampled every INTERVAL seconds. No root.

    scripts/perf/soak.py SECS [INTERVAL] [OUT.csv]
    systemd-run --user --unit=osreflog-soak scripts/perf/soak.py 172800 60   # 48 h, detached

Writes one CSV row per sample and, at the end (or on SIGTERM/SIGINT), a summary in Markdown
next to it (OUT.md) for docs/performance.md. A process that restarts mid-run starts a new
segment: its CPU time counts from zero again, and the summary adds the segments up.
"""

import csv
import os
import re
import signal
import subprocess
import sys
import time

TICK = os.sysconf("SC_CLK_TCK")
PAGE = os.sysconf("SC_PAGE_SIZE")
NAMES = ("osreflogd", "osreflog-user")


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


def main():
    secs = int(sys.argv[1]) if len(sys.argv) > 1 else 3600
    interval = int(sys.argv[2]) if len(sys.argv) > 2 else 60
    path = sys.argv[3] if len(sys.argv) > 3 else f"soak-{int(time.time())}.csv"
    stop = []
    signal.signal(signal.SIGTERM, lambda *_: stop.append(1))
    signal.signal(signal.SIGINT, lambda *_: stop.append(1))

    cols = ["t", "process", "pid", "start", "cpu_ticks", "rss", "hwm", "checkpoints", "snapshots",
            "chain", "events", "changes"]
    began = time.time()
    rows = []
    with open(path, "w", newline="") as f:
        w = csv.writer(f)
        w.writerow(cols)
        while not stop and time.time() - began < secs:
            now = time.time()
            counters = status()
            for name in NAMES:
                for pid in pids(name):
                    s = sample(pid)
                    if s is None:
                        continue
                    row = [round(now - began), name, pid, *s] + [counters.get(k, "") for k in cols[7:]]
                    w.writerow(row)
                    rows.append(dict(zip(cols, row)))
            f.flush()
            for _ in range(interval):
                if stop:
                    break
                time.sleep(1)
    summarize(rows, path, began, interval)


def summarize(rows, path, began, interval):
    hours = (time.time() - began) / 3600
    lines = [
        f"Soak test: {hours:.1f} h, a sample every {interval} s, from "
        f"{time.strftime('%Y-%m-%d %H:%M', time.localtime(began))} "
        f"(`scripts/perf/soak.py`, raw samples `{os.path.basename(path)}`).",
        "",
        "| Process | Average CPU (of one CPU) | Busiest interval | Memory at the start | Memory at the end | Peak |",
        "|---|---|---|---|---|---|",
    ]
    for name in NAMES:
        mine = [r for r in rows if r["process"] == name]
        if not mine:
            lines.append(f"| {name} | not running | | | | |")
            continue
        busy_ticks, busy_secs, worst = 0, 0, 0.0
        for a, b in zip(mine, mine[1:]):
            if a["pid"] != b["pid"] or a["start"] != b["start"]:
                continue  # restarted: a new segment
            dt = b["t"] - a["t"]
            dc = b["cpu_ticks"] - a["cpu_ticks"]
            if dt > 0:
                busy_ticks += dc
                busy_secs += dt
                worst = max(worst, dc / TICK / dt)
        avg = busy_ticks / TICK / busy_secs if busy_secs else 0.0
        mb = lambda b: f"{b / 1048576:.0f} MB"
        peak = max(r["hwm"] for r in mine)
        lines.append(
            f"| {name} | {avg:.2%} | {worst:.1%} | {mb(mine[0]['rss'])} | {mb(mine[-1]['rss'])} | {mb(peak)} |"
        )
    first = next((r for r in rows if r["checkpoints"] != ""), None)
    last = next((r for r in reversed(rows) if r["checkpoints"] != ""), None)
    if first and last:
        lines += [
            "",
            f"Recorded meanwhile: {last['checkpoints'] - first['checkpoints']} checkpoints, "
            f"{last['chain'] - first['chain']} ledger entries"
            + (f", {last['changes'] - first['changes']} live changes" if last["changes"] != "" and first["changes"] != "" else "")
            + ".",
        ]
    out = os.path.splitext(path)[0] + ".md"
    with open(out, "w") as f:
        f.write("\n".join(lines) + "\n")
    print("\n".join(lines))


if __name__ == "__main__":
    main()
