# Performance

What osreflog costs the machine it watches. Measured on the development laptop (i7-8550U, 7 GiB
of RAM, Fedora 44, `/` on btrfs with `compress=zstd:1` on a SATA disk), with the date and the
script for each number so it can be measured again.

## At rest

| | | Measured |
|---|---|---|
| osreflogd, nothing happening | **0.08 %** of one CPU (5 ticks in 60 s) | 2026-10-17, `/proc/<pid>/stat` |
| osreflog-user, nothing happening | about 0.1 % of one CPU, 6 MB | 2026-10-17, `scripts/perf/soak.py` |
| osreflogd memory, just started | 41 MB | P3, after start with the live witness |
| osreflogd memory, after a day of use | **108 MB** anonymous (see below) | 2026-10-17 |
| Live witness volume | about 9 fanotify events/s system-wide; most are out of scope | P1, P3 |

**The 108 MB.** After a day with several large diffs (tens of thousands of changed paths each,
computed on the daemon's blocking thread pool), osreflogd held 108 MB, where P2 measured 26 MB
before the live witness existed. The likely reason is glibc keeping a malloc arena per thread
and rarely returning what was freed in it; 0.2.0 sets `MALLOC_ARENA_MAX=2` in the unit, and
`scripts/p8/hardening-check.sh` measures the daemon around two large diffs to confirm it. The
unit's ceiling is `MemoryMax=768M`.

With two arenas (2026-10-17, `scripts/p8/hardening-check.sh`): 56 MB after the testbed, the undo
gate and workload and five bisects, and **79 MB** after two more diffs from `c000170` to now. That
is less than the 108 MB of the day before, but not the same day's work, so it doesn't prove the
cause; the 48-hour soak shows whether it keeps growing.

## Around a package transaction

From the P2 exit run (`scripts/p2/verify.sh`, a real `dnf remove figlet`):

| | |
|---|---|
| Pre-transaction checkpoint (dnf waits for it) | 742 ms |
| Post-transaction checkpoint | 135 ms |
| Boot checkpoint | 129 ms |
| The interval's diff, in the background | 5.6 s, mostly reading the 134 MB rpmdb from the snapshot |
| `osreflog diff @{1}..@{0}` afterwards | **6 ms**, from the ledger |

dnf waits for the two checkpoints, under a second together; the diff runs in the background
after dnf has finished.

## Diffs on demand

A diff between two checkpoints that aren't neighbours is computed from the send stream the first
time it's asked for: 4 s for a busy interval (P8b), and up to about 10 s for a day with 30,000
changed paths (`c000136..@{now}`, 2026-10-17), most of it reading the stream. Neighbouring intervals
are computed once after each checkpoint and cached.

## Under load

`scripts/p5/flood.sh`, as an ordinary user: 100,000 processes and 300,000 file create/delete
pairs as fast as the machine could make them. osreflogd went from 55 to 104 MB, the lineage
table stopped at its 65,536-process cap, the fanotify queue never overflowed, and
`osreflog status` answered in 19 ms afterwards.

## Bisect

| Probe | Time | Memory |
|---|---|---|
| Container (`systemd-nspawn`, a test command) | 0.3 to 0.9 s | |
| VM booting the snapshot's own kernel | 12.7 to 14.0 s | qemu 428 to 436 MiB |
| A reboot of this machine into a snapshot | one boot, plus 22 s to prepare the first probe's kernel and initramfs | |

The VM's initramfs is built once per kernel (15.6 s for 7.2.9) and cached. Probes run in units
of their own, outside the daemon's memory limit.

## Disk

Snapshots share their data with `/`: each costs what changed after it. Retention keeps every
checkpoint for 48 hours, one a day for 14 days and one a week for 8 weeks
(`/etc/osreflog/retention.conf`); the ledger is kept (1,361 chain entries after four days on
this machine).

What the kept snapshots hold is reported by `status` and checked by `doctor`, from the diffs:
each interval records the bytes its older snapshot keeps that the newer side lost (deleted
files and files replaced by a rename, at their allocated size; content written over in place,
by the ranges written below the old end of the file; truncated tails). Appends release nothing,
so a growing log costs nothing here. Every byte leaves `/` in exactly one interval, so the sum
over intervals that start at a kept snapshot is held for sure, and the sum over all intervals
since the oldest kept snapshot is the most that can be. Measuring costs one `openat2` and one
`fstat` per changed file in the older snapshot, beside a diff that reads the whole send stream.
Reflinks and compression aren't accounted for. On a loopback filesystem, 4 MiB deleted, 1 MiB
written over and 1 MiB appended read as 5 MiB held (`scripts/p2/it.sh`).

Quotas would give exact numbers but stay off: `btrfs fi du` took 23 s for two snapshots in P1,
full qgroups slow down every snapshot deletion, and simple quotas (squota) charge an extent to
the subvolume that wrote it, so they can't say what a snapshot holds alone.

## Over days

The first soak, started 2026-10-17 18:35, measured nothing it set out to: the hardening check
reinstalled and restarted osreflogd an hour in, and the machine was shut down after 6.1 h,
which ended the user unit. Its numbers are two daemons glued together (memory "falling" from
116 MB to 41 MB was the restart, not memory coming back).

The soak now survives both. Samples carry wall-clock times and are appended to one CSV; a
restarted process starts a new segment; time with the machine off isn't counted; and the run
is a user unit enabled at login that carries on where it stopped:

```bash
scripts/perf/soak-unit.sh start     # 48 h sampled, into perf-out/soak.csv
scripts/perf/soak-unit.sh status    # the table from the samples so far
```

The second run started 2026-10-20 02:09, a few hours after a boot (osreflogd at 15 MB). Its
table goes here when it ends.
