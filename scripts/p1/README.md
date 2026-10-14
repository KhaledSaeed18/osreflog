# P1 scripts: measure by hand

One root script runs every P1 experiment in its own private mount namespace and writes numbers to `/var/tmp/osreflog-p1/results`. See [docs/journal/p1-measure.md](../../docs/journal/p1-measure.md) for what each stage answers and what came out.

```
sudo scripts/p1/run.sh            # all stages, about 30 min (20 of them a passive baseline)
sudo scripts/p1/run.sh cleanup    # remove P1 subvolumes and the dnf hook, if a run was interrupted
```

| File | Purpose |
| ---- | ------- |
| `run.sh` | Stages: env, install, hook, diff, space, reflink, visibility, nspawn, vm, fatrace, then cleanup |
| `hook.sh` | dnf5 actions hook: snapshots `/` read-only from a private mount namespace, logs timings |
| `exec-lifetime.bt` | bpftrace: exec/fork counts and process lifetimes |
| `p1-probe.service` | Probe unit for nspawn/VM boots: reports boot state and failed units, then powers off |

What it changes on the system:
- installs `fatrace`, `bpftrace`, `libdnf5-plugin-actions`, `hyperfine` (dnf) and `figlet` (PackageKit)
- creates btrfs subvolumes under the top level in `osreflog-p1/`, deleted at the end (`KEEP=1` keeps them)
- adds `/etc/dnf/libdnf5-plugins/actions.d/osreflog-p1.actions` for the hook stage only

Raw fatrace output (`/var/tmp/osreflog-p1/raw`) contains paths from your home directory. It stays local; only aggregated numbers go into the journal.
