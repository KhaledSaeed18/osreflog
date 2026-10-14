# P1 results (/var/tmp/osreflog-p1)

## Stages

| Stage | rc | seconds |
| ----- | -- | ------- |
| env | 0 | 0.0 |
| install | 0 | 1949.9 |
| hook | 0 | 66.8 |
| diff | 0 | 19.7 |
| space | 0 | 23.9 |
| reflink | 0 | 0.0 |
| visibility | 0 | 0.0 |
| nspawn | 0 | 28.3 |
| vm | 1 | 86.4 |
| fatrace | 0 | 1203.4 |

## Metrics

| Key | Value |
| --- | ----- |
| `snapshot_ms.s-a` | 63 |
| `snapshot_ms.s-b` | 5252 |
| `hook_stage_start_epoch` | 1792013696 |
| `dnf_install_hyperfine_ms` | 29678 |
| `pkcon_install_figlet_ms` | 32149 |
| `gen.s-a` | 45594 |
| `gen.s-b` | 45657 |
| `send_nodata_ms.install` | 3726 |
| `send_proto.install` | 2 |
| `send_nodata_bytes.install` | 18230739 |
| `dump_ms.install` | 534 |
| `send_full_ms.install` | 6666 |
| `send_full_bytes.install` | 551691828 |
| `findnew_ms.install` | 293 |
| `gen.hook-pre-1116000` | 45658 |
| `gen.hook-post-1116000` | 45660 |
| `send_nodata_ms.hook` | 50 |
| `send_proto.hook` | 2 |
| `send_nodata_bytes.hook` | 41150 |
| `dump_ms.hook` | 5 |
| `send_full_ms.hook` | 98 |
| `send_full_bytes.hook` | 9581362 |
| `findnew_ms.hook` | 93 |
| `snapshot_ms.t1` | 173 |
| `snapshot_ms.t2` | 75 |
| `snapshot_ms.t3` | 53 |
| `snapshot_ms.t4` | 65 |
| `snapshot_ms.t5` | 69 |
| `subvol_delete_ms.t1` | 3 |
| `subvol_delete_ms.t2` | 3 |
| `subvol_delete_ms.t3` | 3 |
| `subvol_delete_ms.t4` | 3 |
| `subvol_delete_ms.t5` | 3 |
| `fi_du_ms` | 23424 |
| `reflink_cross_mount` | ok |
| `top_mount_visible_in_pid1_ns` | 0 |
| `top_mount_visible_in_own_ns` | 1 |
| `nspawn_cmd_ms.1` | 535 |
| `nspawn_cmd_ms.2` | 374 |
| `nspawn_cmd_ms.3` | 419 |
| `nspawn_boot_ms` | 26902 |
| `vm_kernel` | 7.2.9-200.fc44.x86_64 |
| `dracut_ms` | 41367 |
| `initrd_bytes` | 266618407 |
| `vm_wall_ms` | 43927 |
| `fatrace_secs_actual` | 1200 |

## dnf5 actions hook

| phase | dnf pid | parent | snapshot ms | hook total ms |
| --- | --- | --- | --- | --- |
| pre | 1116000 | dnf | 918 | 3972 |
| post | 1116000 | dnf | 80 | 98 |
| pre | 1115858 | packagekitd | 412 | 3438 |
| post | 1115858 | packagekitd | 174 | 201 |

`/usr/bin/hyperfine` born -71 ms relative to the pre hook's end (second resolution): rpm wrote it **after** the hook returned.

Invocations whose parent is not dnf5: 2 (packagekitd).

## Send-stream diff: install

59773 distinct paths, 176980 commands.

| Command | Count |
| ------- | ----- |
| `utimes` | 26851 |
| `rename` | 24775 |
| `set_xattr` | 24767 |
| `chown` | 24761 |
| `update_extent` | 23129 |
| `chmod` | 19282 |
| `mkfile` | 15421 |
| `link` | 8355 |
| `symlink` | 5479 |
| `mkdir` | 3861 |
| `clone` | 205 |
| `unlink` | 59 |
| `truncate` | 18 |
| `rmdir` | 16 |
| `snapshot` | 1 |

Top path prefixes:

| Prefix | Commands |
| ------ | -------- |
| `/s-b/usr` | 122380 |
| `/s-b/var` | 4977 |
| `/s-b/etc` | 50 |
| `/s-b/o840243-38920-0` | 5 |
| `/s-b/o840246-38920-0` | 5 |
| `/s-b/o840249-38920-0` | 5 |
| `/s-b/o873383-40351-0` | 5 |
| `/s-b/o840185-38919-0` | 4 |
| `/s-b/o840197-38920-0` | 4 |
| `/s-b/o840203-38920-0` | 4 |
| `/s-b/o873389-40351-0` | 4 |
| `/s-b/o840177-38919-0` | 3 |
| `/s-b/o840188-38920-0` | 3 |
| `/s-b/o840180-38919-0` | 2 |
| `/s-b/o840191-38920-0` | 2 |

`find-new` reported 20888 distinct paths; it cannot see the 75 unlink/rmdir targets in the send stream (75 of them absent from find-new).

rpmdb diff: 16 lines
    > bcc-0.35.0-5.fc44.x86_64
    > bcc-tools-0.35.0-5.fc44.x86_64
    > bison-3.8.2-15.fc44.x86_64
    > bpftrace-0.24.2-3.fc44.x86_64
    > clang21-libs-21.1.8-5.fc44.x86_64
    > clang21-resource-filesystem-21.1.8-5.fc44.x86_64
    > compiler-rt21-21.1.8-5.fc44.x86_64
    > fatrace-0.19.1-3.fc44.x86_64
    > flex-2.6.4-24.fc44.x86_64
    > kernel-devel-7.2.10-200.fc44.x86_64
    > libdnf5-plugin-actions-5.4.6.0-1.fc44.x86_64
    > libomp21-21.1.8-5.fc44.x86_64
    > libomp21-devel-21.1.8-5.fc44.x86_64
    > openssl-devel-1:3.5.9-1.fc44.x86_64
    > python3-bcc-0.35.0-5.fc44.noarch
    > python3-netaddr-1.3.0-11.fc44.noarch

## Send-stream diff: hook

76 distinct paths, 459 commands.

| Command | Count |
| ------- | ----- |
| `update_extent` | 281 |
| `utimes` | 53 |
| `rename` | 23 |
| `set_xattr` | 23 |
| `chown` | 23 |
| `chmod` | 22 |
| `mkfile` | 20 |
| `unlink` | 9 |
| `mkdir` | 2 |
| `snapshot` | 1 |
| `clone` | 1 |
| `symlink` | 1 |

Top path prefixes:

| Prefix | Commands |
| ------ | -------- |
| `/hook-post-1116000/var` | 273 |
| `/hook-post-1116000/usr` | 132 |
| `/hook-post-1116000/etc` | 7 |
| `/hook-post-1116000/o955292-45658-0` | 2 |
| `/hook-post-1116000/o955293-45658-0` | 2 |
| `/hook-post-1116000/o955294-45658-0` | 2 |
| `/hook-post-1116000/o955295-45658-0` | 2 |
| `/hook-post-1116000/o955296-45658-0` | 2 |
| `/hook-post-1116000/o955297-45658-0` | 2 |
| `/hook-post-1116000/o955298-45658-0` | 2 |
| `/hook-post-1116000/o955299-45658-0` | 2 |
| `/hook-post-1116000/o955300-45658-0` | 2 |
| `/hook-post-1116000/o955301-45658-0` | 2 |
| `/hook-post-1116000/o955302-45658-0` | 2 |
| `/hook-post-1116000/o955303-45658-0` | 2 |

`find-new` reported 33 distinct paths; it cannot see the 9 unlink/rmdir targets in the send stream (9 of them absent from find-new).

rpmdb diff: 1 lines
    > hyperfine-1.20.0-2.fc44.x86_64

## Process lifetimes: during `dnf install hyperfine`

| Counter | Value |
| ------- | ----- |
| forks | 269 |
| execs | 100 |
| exits | 87 |
| lt_1ms | 20 |
| lt_10ms | 58 |
| lt_100ms | 71 |

Top exec'd commands: `ps` 14, `tar` 12, `date` 12, `sh` 7, `git` 5, `sleep` 4, `sed` 4, `gpg` 4, `findmnt` 4, `cat` 3, `bash` 3, `16` 3, `unshare` 2, `umount` 2, `rmdir` 2

Lifetime histogram (µs):
```
[256, 512)            17 |@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@|
[512, 1K)              3 |@@@@@@@@@                                           |
[1K, 2K)              14 |@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@          |
[2K, 4K)              13 |@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@             |
[4K, 8K)              10 |@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@                      |
[8K, 16K)              4 |@@@@@@@@@@@@                                        |
[16K, 32K)             3 |@@@@@@@@@                                           |
[32K, 64K)             4 |@@@@@@@@@@@@                                        |
[64K, 128K)            6 |@@@@@@@@@@@@@@@@@@                                  |
[128K, 256K)           1 |@@@                                                 |
[256K, 512K)           1 |@@@                                                 |
[512K, 1M)             6 |@@@@@@@@@@@@@@@@@@                                  |
[1M, 2M)               1 |@@@                                                 |
[2M, 4M)               2 |@@@@@@                                              |
[4M, 8M)               0 |                                                    |
[8M, 16M)              1 |@@@                                                 |
[16M, 32M)             1 |@@@                                                 |
```

## Process lifetimes: baseline

| Counter | Value |
| ------- | ----- |
| forks | 4352 |
| execs | 635 |
| exits | 626 |
| lt_1ms | 33 |
| lt_10ms | 441 |
| lt_100ms | 482 |

Top exec'd commands: `ps` 189, `sleep` 126, `claude` 114, `pkla-check-auth` 37, `git` 28, `date` 24, `systemd-userwor` 11, `mkdir` 9, `chronyc` 9, `bash` 9, `99-nvme-nbft-co` 9, `20-chrony-onoff` 9, `20-chrony-dhcp` 9, `04-iscsi` 9, `tail` 6

Lifetime histogram (µs):
```
[256, 512)            15 |@@@                                                 |
[512, 1K)             18 |@@@@                                                |
[1K, 2K)              60 |@@@@@@@@@@@@@@                                      |
[2K, 4K)              65 |@@@@@@@@@@@@@@@@                                    |
[4K, 8K)             209 |@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@|
[8K, 16K)             87 |@@@@@@@@@@@@@@@@@@@@@                               |
[16K, 32K)             7 |@                                                   |
[32K, 64K)            20 |@@@@                                                |
[64K, 128K)            1 |                                                    |
[128K, 256K)           0 |                                                    |
[256K, 512K)           0 |                                                    |
[512K, 1M)             5 |@                                                   |
[1M, 2M)               0 |                                                    |
[2M, 4M)               0 |                                                    |
[4M, 8M)               1 |                                                    |
[8M, 16M)            107 |@@@@@@@@@@@@@@@@@@@@@@@@@@                          |
[16M, 32M)             0 |                                                    |
[32M, 64M)            21 |@@@@@                                               |
[64M, 128M)            0 |                                                    |
[128M, 256M)           0 |                                                    |
[256M, 512M)           8 |@                                                   |
[512M, 1G)             1 |                                                    |
[1G, 2G)               1 |                                                    |
```

## Probe boot: vm

Probe line: `[   36.634361] sh[1270]: P1-PROBE state=running uptime=36.63`

Failed units (0): none

## VM memory

qemu max RSS: 1082 MiB

virtiofsd peak RSS: 4 MiB

## fatrace baseline (filesystem marks on every real mount)

10906 events in 1200 s: **9.1/s** average, peak 177/s.

In the default live-capture scopes (/etc/, /boot/, /usr/local/, /opt/, /var/lib/NetworkManager/, /var/lib/bluetooth/, /var/lib/fwupd/, /var/lib/AccountsService/): **7** events (21/hour).

Actor not resolvable by fatrace: comm missing in 10 (0.09 %), exe missing in 224 (2.05 %). Events without a path: 20.

| Type | Count |
| ---- | ----- |
| `W` | 8721 |
| `CW` | 628 |
| `+` | 581 |
| `<>` | 511 |
| `CWO` | 281 |
| `D` | 66 |
| `WO` | 59 |
| `<` | 19 |
| `>` | 19 |
| `RCWO` | 17 |
| `+<>` | 4 |

| Bucket | Events | /s |
| ------ | ------ | -- |
| `/var/` | 7492 | 6.24 |
| `/home/*/.config` | 1745 | 1.45 |
| `/home/*/.local` | 1593 | 1.33 |
| `/home/*/.claude` | 42 | 0.04 |
| `(no path)` | 20 | 0.02 |
| `/home/*/(files)` | 11 | 0.01 |
| `/home/*/.claude.json` | 2 | 0.00 |
| `/home/*/.cache` | 1 | 0.00 |

Top processes, all events: `rsyslogd` 6209, `claude-desktop` 1758, `drainscope-daem` 1306, `systemd-journal` 1072, `postgres` 230, `tee` 115, `upowerd` 61, `claude` 48, `redis-server` 47, `NetworkManager` 13, `auditd` 12, `?` 10

Top processes, in-scope events: `NetworkManager` 7

Paths remapped from the private top-level mount: 10886

JSON keys seen: comm, device, exe, gid, inode, path, pid, timestamp, types, uid

## Other files

reflink: ok; top mount visible in pid 1 ns: 0

qgroup:
```
ERROR: can't list qgroups: quotas not enabled
```

fi du:
```
Total   Exclusive  Set shared  Filename
  13.24GiB    62.55MiB     8.90GiB  /run/osreflog-p1/top/osreflog-p1/s-a
  13.67GiB     3.25MiB     9.12GiB  /run/osreflog-p1/top/osreflog-p1/s-b
```

pkcon:
```
Transaction:	Resolving
Status: 	Waiting in queue
Status: 	Starting
Transaction:	Installing
Status: 	Waiting in queue
Status: 	Waiting for authentication
Status: 	Waiting in queue
Status: 	Starting
Status: 	Querying
Status: 	Downloading packages
Percentage:	0
Percentage:	80
Percentage:	100
Status: 	Running
Status: 	Installing packages
Package:	figlet_2.2.5-39.fc44.x86_64
Status: 	Finished
Results:
Installed    figlet-2.2.5-39.fc44.x86_64 (fedora)
```

AVC records today: 0

## Follow-up run followup-20261015-010206

| Key | Value |
| --- | ----- |
| `snapshot_ms.s-b` | 94 |
| `nspawn_cmd_ms.1` | 549 |
| `nspawn_cmd_ms.2` | 450 |
| `nspawn_cmd_ms.3` | 424 |
| `nspawn_boot_ms` | 26113 |
| `vm_kernel` | 7.2.9-200.fc44.x86_64 |
| `initrd_bytes` | 266618407 |
| `vm_wall_ms` | 4804 |
| `vm_mem` | 512M |

nspawn-probe-result.txt: `P1-PROBE state=degraded uptime=213109.26 failed=audit-rules.service auditd.service `

vm-probe-result.txt: `no result file`

Maximum resident set size (kbytes): 710392

## Follow-up run followup-20261015-010413

| Key | Value |
| --- | ----- |
| `snapshot_ms.s-b` | 83 |
| `vm_kernel` | 7.2.9-200.fc44.x86_64 |
| `dracut_mode` | hostonly |
| `dracut_ms` | 13386 |
| `initrd_bytes` | 38859587 |
| `vm_wall_ms` | 38957 |
| `vm_mem` | 512M |

vm-probe-result.txt: `P1-PROBE state=running uptime=34.15 failed=`

Maximum resident set size (kbytes): 629872
