# Threat model and privacy

osreflogd is a root daemon that watches the system's config files, records every command that changes them, parses their contents, can write files as root (undo), and can change the next boot (metal bisect). That makes it a high-value target. Its ledger is also a detailed diary of the machine.

## Assets

| Asset | Why it matters |
| ----- | -------------- |
| System integrity | Undo and bisect can write as root and alter boot |
| Bootability | A bad probe entry or initramfs could leave the machine unbootable |
| Secrets in config | Wi-Fi PSKs (`/etc/NetworkManager/system-connections`), `/etc/shadow`, SSH host keys, VPN configs, tokens |
| Activity history | Which commands ran, when, by whom: privacy-sensitive even without secrets |
| Ledger integrity | Bisect and undo trust it; a forged ledger could steer undo |

## Adversaries

| Adversary | Can | Goal |
| --------- | --- | ---- |
| A1: local unprivileged user/process | write their own files, call D-Bus, create files in world-writable dirs | read secrets via the ledger, trick undo into writing root files, crash the daemon, hide its own changes |
| A2: malicious file content | content in a scoped file that the parser will read (e.g. a package's config, a user's `~/.config`) | exploit a parser bug |
| A3: event flood | generate huge numbers of fs events or processes | exhaust memory, drop events, hide a change in the noise |
| A4: root attacker | anything | out of scope to *prevent*; in scope to *detect after the fact* (tamper-evidence) |

## Mitigations

**Secrets (A1, privacy)**
- A denylist, never stored as content, only "changed" + hash: `/etc/shadow`, `/etc/gshadow`, `/etc/NetworkManager/system-connections/*`, `/etc/ssh/ssh_host_*_key`, `/etc/pki/**/private/*`, `*.key`, `/etc/wireguard/*`, `/etc/crypttab`, `/etc/security/opasswd`.
- Heuristic: a root-owned file with mode `0600`/`0400` is treated as secret unless allowlisted.
- Key-level redaction in semantic diffs: `psk=`, `password=`, `secret=`, `token=`, `private-key=`.
- argv redaction: secret flags (`--password x`, `--token=x`), `NAME_TOKEN=x` arguments, URL passwords, and the value after a positional key that names a secret (`dconf write …/api-key x`, `gsettings set org.app password x`, `nmcli … wifi-sec.psk x`). Full argv is not stored for `command` causes, only the top command plus a redacted argv.
- The ledger is `root:osreflog 0640`, D-Bus reads are polkit-gated, and nothing leaves the machine. There is no network code in osreflogd at all.

**Undo as a root write primitive (A1)**
- Undo only targets paths that the ledger recorded for that cause, and only in scope.
- Every file operation is `openat2` with `RESOLVE_BENEATH | RESOLVE_NO_SYMLINKS | RESOLVE_NO_MAGICLINKS`, relative to a pinned dirfd for the scope root. No path-string opens.
- 3-way check, and refusal on conflict ([undo.md](./undo.md)).
- polkit `auth_admin` (not `_keep`) for `--apply` on package causes and boot config.

**Parsers (A2)**
- All content parsing runs in a summarize worker (`osreflogd --summarize-worker`, built in P4): a separate process that, before reading any input, sets resource limits (512 MiB address space, 5 s CPU, no file writes, no new processes), drops every capability, sets no-new-privileges, enters a Landlock domain with every filesystem and network right handled and none granted, and installs a seccomp allowlist that kills the process on anything else (read, write, memory, signals for its own abort, exit). Content goes in over a pipe as JSON; summaries come out. The daemon gives it 10 s.
- Size caps per file (1 MiB) and per summary (12 lines of 200 characters).
- The semantic crate has no unsafe code. A stable-toolchain harness (`fuzz/`, `scripts/p4/fuzz.sh`) fuzzes every parser and differ, including the send-stream parser, the rpm header parser and the fanotify event parser, which read kernel or root-written data. It checks bounded output, bounded CPU time and round trips, not only panics. rpm version comparison is also tested differentially against rpm's own.

**User scope**
- `osreflog-user` runs as the user and writes only the user's own ledger (`~/.local/state/osreflog/user.db`, directory `0700`). Nothing about one user's settings reaches the root daemon or other users ([ADR 0010](./decisions/0010-user-scope-own-ledger.md)).
- Values of keys whose names contain `password`, `psk`, `secret`, `token`, `api-key`, `private`… are shown as `***`, both in summaries and in the recorded command line. Values are cut at 100 characters.
- The user ledger is only as trustworthy as the user's account: it is a record for the user, not evidence for the admin.

**Floods (A3)**
- Bounded per-actor memory in the cause builder; coalesce repeated writes to the same path within a cause.
- Lineage cache bounded (LRU, with exited processes evicted first).
- On overload, drop *content* capture before dropping *events*, and record a gap. The send-stream diff at the next checkpoint guarantees no change is lost from the state record, only its attribution.
- A per-uid rate threshold: a user generating more than N changes/s in scope gets a single summarised cause, flagged as "flood".

**Boot safety (metal bisect)**
- One-shot entries only (`grub2-reboot`), the default entry never modified, and probe roots are clones.
- The boot gate checklist must pass in a VM first ([roadmap](./roadmap.md)).
- `osreflog doctor --boot` verifies that no probe entry is left behind and the default entry is intact.

**Ledger integrity (A4)**
- Hash chain: each committed cause stores `H(prev_hash ‖ cause_record)`. `osreflog verify` walks the chain and cross-checks against snapshots (a snapshot's send-stream diff must match the ledger's changes for that interval).
- This is tamper-evident, not tamper-proof: a root attacker can rewrite the chain. Optional later: periodically sign the head with a key held in the TPM, or copy the head hash off-machine.

**The daemon itself**
- systemd hardening: `ProtectHome=read-only` (system daemon never needs to read `$HOME` content), `ProtectKernelTunables=`, `ProtectClock=`, `RestrictAddressFamilies=AF_UNIX AF_NETLINK`, `IPAddressDeny=any`, `MemoryMax=`, `CPUQuota=`, and a capability bounding set limited to what fanotify, BPF, btrfs ioctls and mounts need (`CAP_SYS_ADMIN`, `CAP_BPF`, `CAP_PERFMON`, `CAP_DAC_READ_SEARCH`, `CAP_FOWNER`, `CAP_CHOWN`, `CAP_SETFCAP` for undo).
- SELinux policy module (`osreflogd_t`), following the urljail experience; written in P8, with denials collected throughout development.

## Operating rules

1. Read-only features run on the real machine from P2. Anything that writes to `/` waits for the write gate; anything that touches boot waits for the boot gate.
2. Every new semantic differ gets a fuzz target before it's enabled.
3. Every new secret-bearing path found during dogfooding goes into the denylist and the testbed in the same change.
