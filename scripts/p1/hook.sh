#!/bin/bash
# dnf5 actions hook for P1: take a read-only snapshot of / in a private mount namespace.
#   hook.sh pre|post <dnf-pid>
# Logs one TSV line to /var/tmp/osreflog-p1/hook.log:
#   phase  dnf_pid  parent_comm  start_ms  snapshot  snapshot_ms  end_ms
# The pre hook sleeps 3 s after the snapshot so ordering is unambiguous: if rpm wrote
# package files before this hook returned, their birth times fall inside the sleep.
set -u
phase=$1; dnfpid=$2
WORK=/var/tmp/osreflog-p1
start=$(date +%s%3N)
name="hook-$phase-$dnfpid"
parent=$(cat "/proc/$PPID/comm" 2>/dev/null || echo '?')

snap_ms=$(unshare -m --propagation private bash -c '
    set -e
    dev=$(findmnt -n -o SOURCE --target / | sed "s/\[.*//")
    sub=$(findmnt -n -o FSROOT --target / | sed "s|^/||")
    top=$(mktemp -d /run/osreflog-p1-hook.XXXX)
    mount -o subvolid=5 "$dev" "$top"
    [ -d "$top/osreflog-p1" ] || btrfs subvolume create "$top/osreflog-p1" >/dev/null
    t0=$(date +%s%3N)
    btrfs subvolume snapshot -r "$top/$sub" "$top/osreflog-p1/'"$name"'" >/dev/null
    t1=$(date +%s%3N)
    umount "$top"; rmdir "$top"
    echo $((t1 - t0))
' 2>>"$WORK/hook.err") || snap_ms="error"

[ "$phase" = pre ] && sleep 3
end=$(date +%s%3N)
printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\n' "$phase" "$dnfpid" "$parent" "$start" "$name" "$snap_ms" "$end" >> "$WORK/hook.log"
exit 0
