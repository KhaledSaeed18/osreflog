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
checkpoint for 48 hours, one a day for 14 days and one a week for 8 weeks; the ledger is kept
(1,361 chain entries after four days on this machine). How much space the kept snapshots hold is
not reported yet ([open question Q3](open-questions.md)): `btrfs fi du` took 23 s for two
snapshots in P1, too slow to run on every `status`, and simple quotas are still to be evaluated.

## Over days

A 48-hour soak test of normal use started on 2026-10-17 18:35:

```bash
systemd-run --user --unit=osreflog-soak scripts/perf/soak.py 172800 60 perf-out/soak-48h.csv
```

It samples both processes' CPU time and memory every minute, with what the daemon recorded
meanwhile, and writes a summary table next to the CSV. Its results go here when it ends.
