#!/usr/bin/python3 -I
"""Summarise a P1 run (/var/tmp/osreflog-p1) as Markdown on stdout.

Runs unprivileged after run.sh has chowned the results. Raw fatrace paths are reduced to
aggregate counts here; no individual path under /home is printed.
"""
import collections
import json
import os
import re
import sys

WORK = sys.argv[1] if len(sys.argv) > 1 else "/var/tmp/osreflog-p1"
R = os.path.join(WORK, "results")
RAW = os.path.join(WORK, "raw")

SCOPES = ["/etc/", "/boot/", "/usr/local/", "/opt/", "/var/lib/NetworkManager/",
          "/var/lib/bluetooth/", "/var/lib/fwupd/", "/var/lib/AccountsService/"]


def read(name, default=""):
    try:
        with open(os.path.join(R, name), errors="replace") as f:
            return f.read()
    except OSError:
        return default


def metrics():
    m = collections.OrderedDict()
    for line in read("metrics.tsv").splitlines():
        if "\t" in line:
            k, v = line.split("\t", 1)
            m[k] = v
    return m


def section(title):
    print(f"\n## {title}\n")


def stages():
    section("Stages")
    print("| Stage | rc | seconds |\n| ----- | -- | ------- |")
    for line in read("stages.tsv").splitlines():
        name, rc, msec = line.split("\t")
        print(f"| {name} | {rc} | {int(msec) / 1000:.1f} |")


def metric_table(m):
    section("Metrics")
    print("| Key | Value |\n| --- | ----- |")
    for k, v in m.items():
        print(f"| `{k}` | {v} |")


def hook():
    section("dnf5 actions hook")
    rows = [l.split("\t") for l in read("hook.log").splitlines() if l.count("\t") == 6]
    if not rows:
        print("No hook invocations logged.")
        return
    print("| phase | dnf pid | parent | snapshot ms | hook total ms |\n| --- | --- | --- | --- | --- |")
    for phase, pid, parent, start, name, snap_ms, end in rows:
        print(f"| {phase} | {pid} | {parent} | {snap_ms} | {int(end) - int(start)} |")
    birth = re.search(r"birth=(\d+)", read("hyperfine-birth.txt"))
    pre = [r for r in rows if r[0] == "pre"]
    if birth and pre:
        b_ms = int(birth.group(1)) * 1000
        pre_end = int(pre[0][6])
        verdict = "after" if b_ms >= pre_end - 999 else "BEFORE"
        print(f"\n`/usr/bin/hyperfine` born {b_ms - pre_end:+d} ms relative to the pre hook's end "
              f"(second resolution): rpm wrote it **{verdict}** the hook returned.")
    pk = [r for r in rows if r[2] not in ("dnf5", "dnf")]
    print(f"\nInvocations whose parent is not dnf5: {len(pk)} "
          f"({', '.join(sorted({r[2] for r in pk})) or 'none'}).")
    err = read("hook.err").strip()
    if err:
        print(f"\nhook.err:\n```\n{err[:2000]}\n```")


DUMP_RE = re.compile(r"^(\S+)\s+(\S+)")


def dump(tag):
    text = read(f"dump-{tag}.txt")
    if not text:
        return
    section(f"Send-stream diff: {tag}")
    ops = collections.Counter()
    paths = set()
    tops = collections.Counter()
    deleted = set()
    for line in text.splitlines():
        mt = DUMP_RE.match(line)
        if not mt:
            continue
        op, path = mt.group(1), mt.group(2)
        ops[op] += 1
        if op in ("snapshot", "subvol"):
            continue
        p = path.split("/", 1)[1] if "/" in path else path
        p = "/" + p.replace("\\ ", " ")
        paths.add(p)
        tops["/".join(p.split("/")[:3])] += 1
        if op in ("unlink", "rmdir"):
            deleted.add(p)
    print(f"{len(paths)} distinct paths, {sum(ops.values())} commands.\n")
    print("| Command | Count |\n| ------- | ----- |")
    for op, n in ops.most_common():
        print(f"| `{op}` | {n} |")
    print("\nTop path prefixes:\n\n| Prefix | Commands |\n| ------ | -------- |")
    for t, n in tops.most_common(15):
        print(f"| `{t}` | {n} |")
    fn = read(f"findnew-{tag}.txt")
    fpaths = set()
    for line in fn.splitlines():
        if line.startswith("inode"):
            fpaths.add("/" + line.rsplit(" ", 1)[-1].lstrip("/"))
    print(f"\n`find-new` reported {len(fpaths)} distinct paths; it cannot see the "
          f"{len(deleted)} unlink/rmdir targets in the send stream "
          f"({len(deleted - fpaths)} of them absent from find-new).")
    rd = [l for l in read(f"rpmdiff-{tag}.txt").splitlines() if l[:1] in "<>"]
    print(f"\nrpmdb diff: {len(rd)} lines")
    for l in rd[:25]:
        print(f"    {l}")


BT_COUNT = re.compile(r"^@(\w+): (\d+)$", re.M)


def bpf(name, label):
    text = read(name)
    if not text:
        return
    section(f"Process lifetimes: {label}")
    counts = dict((k, int(v)) for k, v in BT_COUNT.findall(text))
    print("| Counter | Value |\n| ------- | ----- |")
    for k in ("forks", "execs", "exits", "lt_1ms", "lt_10ms", "lt_100ms"):
        if k in counts:
            print(f"| {k} | {counts[k]} |")
    comms = re.findall(r"^@exec_comm\[(.*?)\]: (\d+)$", text, re.M)
    comms = sorted(((int(n), c) for c, n in comms), reverse=True)[:15]
    if comms:
        print("\nTop exec'd commands: " + ", ".join(f"`{c}` {n}" for n, c in comms))
    hist = re.search(r"@life_us:\s*\n(.*?)(?:\n\n|\Z)", text, re.S)
    if hist:
        print(f"\nLifetime histogram (µs):\n```\n{hist.group(1).rstrip()}\n```")


def probe_log(name, label):
    text = read(name)
    if not text:
        return
    section(f"Probe boot: {label}")
    probe = [l.strip() for l in text.splitlines() if "P1-PROBE" in l]
    failed = sorted({l.split("P1-FAILED", 1)[1].split()[0] for l in text.splitlines() if "P1-FAILED" in l})
    print(f"Probe line: `{probe[0] if probe else 'not reached'}`\n")
    print(f"Failed units ({len(failed)}): " + (", ".join(f"`{u}`" for u in failed) or "none"))
    tail = [l for l in text.splitlines() if l.strip()][-8:]
    if not probe:
        print("\nLast lines:\n```\n" + "\n".join(tail) + "\n```")


def vm_mem():
    t = read("vm-time.txt")
    m = re.search(r"Maximum resident set size \(kbytes\): (\d+)", t)
    h = re.search(r"VmHWM:\s+(\d+)", read("virtiofsd-hwm.txt"))
    if m or h:
        section("VM memory")
        if m:
            print(f"qemu max RSS: {int(m.group(1)) / 1024:.0f} MiB")
        if h:
            print(f"\nvirtiofsd peak RSS: {int(h.group(1)) / 1024:.0f} MiB")


def fatrace(m):
    path = os.path.join(RAW, "fatrace.jsonl")
    if not os.path.exists(path):
        return
    section("fatrace baseline (filesystem marks on every real mount)")
    secs = max(1, int(m.get("fatrace_secs_actual", "1")))
    total = 0
    types = collections.Counter()
    buckets = collections.Counter()
    scoped = 0
    scoped_comm = collections.Counter()
    comm_all = collections.Counter()
    unresolved_comm = 0
    unresolved_exe = 0
    no_path = 0
    per_sec = collections.Counter()
    keys = collections.Counter()
    remapped = 0
    with open(path, errors="replace") as f:
        for line in f:
            try:
                e = json.loads(line)
            except ValueError:
                continue
            total += 1
            keys.update(e.keys())
            t = e.get("types") or e.get("type") or "?"
            types[t] += 1
            comm = e.get("comm") or ""
            if not comm:
                unresolved_comm += 1
            if not e.get("exe"):
                unresolved_exe += 1
            comm_all[comm or "?"] += 1
            ts = e.get("timestamp")
            if isinstance(ts, (int, float)):
                per_sec[int(ts)] += 1
            p = e.get("path")
            if p:
                # fatrace ran in run.sh's private mount namespace, where the btrfs top level
                # is mounted at /run/osreflog-p1/top: event paths resolve through that mount.
                for pre, repl in (("/run/osreflog-p1/top/root/", "/"),
                                  ("/run/osreflog-p1/top/home/", "/home/")):
                    if p.startswith(pre):
                        p = repl + p[len(pre):]
                        remapped += 1
                        break
            if not p:
                no_path += 1
                buckets["(no path)"] += 1
                continue
            if p.startswith("/home/"):
                parts = p.split("/")
                top = parts[3].split(".tmp")[0] if len(parts) > 3 else ""
                b = "/home/*/" + (top if top.startswith(".") else "(files)")
            else:
                b = "/".join(p.split("/")[:2]) + "/"
            buckets[b] += 1
            if any(p.startswith(s) for s in SCOPES):
                scoped += 1
                scoped_comm[comm or "?"] += 1
    peak = max(per_sec.values()) if per_sec else 0
    print(f"{total} events in {secs} s: **{total / secs:.1f}/s** average, peak {peak}/s.\n")
    print(f"In the default live-capture scopes ({', '.join(SCOPES)}): **{scoped}** events "
          f"({scoped / secs * 3600:.0f}/hour).\n")
    print(f"Actor not resolvable by fatrace: comm missing in {unresolved_comm} "
          f"({100 * unresolved_comm / max(1, total):.2f} %), exe missing in {unresolved_exe} "
          f"({100 * unresolved_exe / max(1, total):.2f} %). Events without a path: {no_path}.\n")
    print("| Type | Count |\n| ---- | ----- |")
    for t, n in types.most_common():
        print(f"| `{t}` | {n} |")
    print("\n| Bucket | Events | /s |\n| ------ | ------ | -- |")
    for b, n in buckets.most_common(20):
        print(f"| `{b}` | {n} | {n / secs:.2f} |")
    print("\nTop processes, all events: " + ", ".join(f"`{c}` {n}" for c, n in comm_all.most_common(12)))
    print("\nTop processes, in-scope events: " + (", ".join(f"`{c}` {n}" for c, n in scoped_comm.most_common(12)) or "none"))
    print(f"\nPaths remapped from the private top-level mount: {remapped}")
    print(f"\nJSON keys seen: {', '.join(sorted(keys))}")


def main():
    m = metrics()
    print(f"# P1 results ({WORK})")
    stages()
    metric_table(m)
    hook()
    dump("install")
    dump("hook")
    bpf("exec-during-dnf.txt", "during `dnf install hyperfine`")
    bpf("exec-baseline.txt", "baseline")
    probe_log("nspawn-boot.log", "nspawn")
    probe_log("vm-console.log", "vm")
    vm_mem()
    fatrace(m)
    print(f"\n## Other files\n\nreflink: {m.get('reflink_cross_mount')}; "
          f"top mount visible in pid 1 ns: {m.get('top_mount_visible_in_pid1_ns')}")
    print(f"\nqgroup:\n```\n{read('qgroup.txt').strip()[:500]}\n```")
    print(f"\nfi du:\n```\n{read('fi-du.txt').strip()[:800]}\n```")
    print(f"\npkcon:\n```\n{read('pkcon.txt').strip()[-1200:]}\n```")
    avc = read("avc-today.txt")
    print(f"\nAVC records today: {avc.count('type=AVC')}")


if __name__ == "__main__":
    main()
