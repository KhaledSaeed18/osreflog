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
- argv redaction: common secret flags (`--password`, `-p<value>` for known tools, `TOKEN=` env-style args); full argv is not stored for `command` causes, only the top command plus a redacted argv.
- The ledger is `root:osreflog 0640`, D-Bus reads are polkit-gated, and nothing leaves the machine. There is no network code in osreflogd at all.

**Undo as a root write primitive (A1)**
- Undo only targets paths that the ledger recorded for that cause, and only in scope.
- Every file operation is `openat2` with `RESOLVE_BENEATH | RESOLVE_NO_SYMLINKS | RESOLVE_NO_MAGICLINKS`, relative to a pinned dirfd for the scope root. No path-string opens.
- 3-way check, and refusal on conflict ([undo.md](./undo.md)).
- polkit `auth_admin` (not `_keep`) for `--apply` on package causes and boot config.

**Parsers (A2)**
- All content parsing runs in `osreflog-parse`: a separate process with no capabilities, `PR_SET_NO_NEW_PRIVS`, a seccomp allowlist (read/write on inherited pipes, mmap, exit), a Landlock ruleset with no filesystem access at all, and memory and CPU limits via its own cgroup. Content goes in over a pipe; a structured diff comes out.
- Size caps per file (1 MiB) and per diff (output truncated, marked).
- Rust + `#![forbid(unsafe_code)]` in `parse/`; fuzz each differ (cargo-fuzz) from P4 on.

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
