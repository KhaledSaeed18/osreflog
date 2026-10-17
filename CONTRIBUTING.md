# Contributing

Bug reports, ideas, measurements from machines osreflog hasn't seen (another btrfs layout, LUKS,
NVMe, AMD) and pull requests are all welcome. Before changing code, skim
[Architecture](docs/architecture.md) and the [threat model](docs/threat-model.md): osreflog runs
as root and, when asked, writes to the system, so a few rules below aren't negotiable.

## The tree

| Path | What |
|---|---|
| `bins/osreflogd` | The daemon: live witness (fanotify, eBPF lineage in `bpf/`), checkpoints, causes, D-Bus service, undo and bisect workers |
| `bins/osreflog` | The CLI |
| `bins/osreflog-user` | The per-user agent for dconf/GSettings changes |
| `bins/osreflog-app` | The GNOME app (gtk4-rs, libadwaita); `view.rs` is everything it shows as plain, tested data |
| `crates/btrfs` | Snapshot and subvolume ioctls, the send-stream parser |
| `crates/ledger` | SQLite schema and migrations, the hash chain, refs, retention |
| `crates/semantic` | What a config change means: keyfile/INI, key=value, passwd, fstab, JSON, units, symlinks |
| `crates/undo` | The undo engine: plans, `openat2`-confined writes, the attic, line diffs |
| `crates/bisect` | The two-level search (checkpoints, then causes) and its session |
| `crates/boot` | BLS entries and grubenv for probes on real hardware |
| `crates/rpm`, `fanotify`, `model`, `client` | rpmdb reading and version comparison, fanotify records, shared types, the D-Bus client |
| `fuzz/` | A stable-toolchain fuzz harness for every parser and differ |
| `data/` | Units, D-Bus and polkit policies, the dnf actions file, the app's desktop file, metainfo and icons |
| `scripts/` | Install, packaging, screenshots, and each phase's checks (`p2/` … `p8/`, `perf/`) |
| `docs/` | Design, decisions (`decisions/`, ADRs), one journal entry per phase (`journal/`) |

## Build and test

On Fedora:

```bash
sudo dnf install rust cargo gcc clang libbpf-devel kernel-headers gtk4-devel libadwaita-devel
cargo build --workspace
cargo test --workspace --exclude osreflog-fuzz
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all
```

Rust from rustup works too (1.88 or newer). The tests need no root and no btrfs: fixtures come
from a loopback btrfs image, recorded once.

To try a change for real, install it and run the app from the tree:

```bash
cargo build --release
sudo scripts/install-dev.sh
cargo run --release -p osreflog-app
```

The app can open a page at start (`OSREFLOG_APP_OPEN=search:p8c,cause:774`, also `since:`,
`undo:`, `bisect:session`, `bisect:new`) and render itself to a PNG
(`OSREFLOG_APP_SCREENSHOT`); `scripts/screenshots.sh` uses both for the README.

## The checks that need root

Each phase left a script that checks its claims against the installed daemon on a real machine.
They are listed with their last results in [Validation](docs/validation.md). Run the ones your
change touches, as root, and say in the pull request what they printed:

- capture and causes: `scripts/p3/testbed.sh`, `scripts/p3/workload.sh`
- undo: `scripts/p5/gate.sh` (always, for any change under `crates/undo` or the undo worker),
  `scripts/p5/undo-workload.sh`
- bisect: `scripts/p6/seeded.sh`; probes on real hardware: `scripts/p7/testvm.sh` first, in a VM
- packaging: `scripts/p8/coprvm.sh` (a fresh Fedora VM, installs from COPR)
- parsers and differs: `scripts/p4/fuzz.sh 600` (no root)

## CI

Every push runs [`.github/workflows/ci.yml`](.github/workflows/ci.yml) on Fedora 44: fmt, clippy
with warnings as errors, the tests, a 30-second run of every fuzz target, the units' systemd
exposure (which may only go down), the D-Bus and polkit policies, the desktop file and metainfo,
the spec against the workspace (`scripts/dist.sh --check`) and `cargo deny`.

## Rules

- **Read-only unless asked.** Only undo writes to the system, only after a dry run shown to the
  person, and only through `crates/undo`'s confined writes. Anything that adds a write path goes
  through the write gate's checks again.
- **Never store a secret.** A new kind of secret-bearing file goes into the denylist and the
  testbed in the same change ([threat model](docs/threat-model.md)).
- **Never block dnf.** The hook may fail; the package transaction may not.
- **The default boot entry is sacred.** Anything touching `/boot` keeps the boot gate's
  guarantees and is tested in the test VM before real hardware.
- **`unsafe` stays where it is.** The workspace denies it; three modules opt back in for system
  calls with no safe wrapper: the btrfs ioctls (`crates/btrfs`), fanotify (`crates/fanotify`'s
  `sys`) and the daemon's sandbox (`bins/osreflogd/src/sandbox.rs`). A new one needs a reason in
  the pull request.
- **No `unwrap` or `expect`** outside tests; clippy's pedantic lints are on.
- **Measure, then claim.** A number in the README or the docs comes with the script that
  measured it.

## Changes

- One change per commit, the subject in the form `area: what changed` (`app: …`, `undo: …`,
  `docs: …`), in plain words.
- A change of design gets an ADR in `docs/decisions/`; a phase's findings go in its journal
  entry, including what broke on the way.
- User-visible changes get a line in [CHANGELOG.md](CHANGELOG.md) under the next version.

## Releases

1. Bump `version` in `Cargo.toml`, `Version` and `%changelog` in `packaging/osreflog.spec`, and
   a `<release>` in the metainfo; write the version's section in `CHANGELOG.md`.
2. `scripts/dist.sh --check`, then commit and wait for CI.
3. `git tag -a vX.Y.Z -m "osreflog X.Y.Z" && git push origin vX.Y.Z`: the release workflow
   builds the tarball, SRPM and RPMs on Fedora 44 and publishes the GitHub release with the
   changelog section as its notes.
4. COPR: `copr-cli build khaledsaeed18/osreflog osreflog-X.Y.Z-1.fc44.src.rpm` with the SRPM from
   the release (Fedora 44, 45 and rawhide).

## License

osreflog is [GPL-3.0-or-later](LICENSE); contributions are under the same license.
