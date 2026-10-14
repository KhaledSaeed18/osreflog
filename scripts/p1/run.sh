#!/bin/bash
# osreflog P1: measure the primitives by hand.
#
#   sudo scripts/p1/run.sh            # all stages (about 30 min, most of it the fatrace baseline)
#   sudo scripts/p1/run.sh cleanup    # delete every P1 subvolume and the dnf hook
#   sudo VM_MEM=512M scripts/p1/run.sh only nspawn vm   # re-run selected stages into results/followup-*
#
# Env: FATRACE_SECS (default 1200), VM_MEM (default 768M), DRACUT_MODE (generic|hostonly),
#      KEEP=1 (keep subvolumes at the end).
# Raw output:  /var/tmp/osreflog-p1/raw      (may contain personal paths; never committed)
# Results:     /var/tmp/osreflog-p1/results  (timings, dumps, logs; curated into docs/journal/)
#
# Safety: everything is created under the btrfs top level in osreflog-p1/, mounted only
# inside this script's private mount namespace. The live root is never modified except by
# the package installs (fatrace, bpftrace, libdnf5-plugin-actions, hyperfine, figlet) and a
# temporary dnf actions file that is removed at the end of the hook stage.
set -uo pipefail

[ "$(id -u)" -eq 0 ] || { echo "run with sudo" >&2; exit 1; }

# Re-exec in a private mount namespace so the top-level mount is invisible to the system.
if [ -z "${OSREFLOG_P1_NS:-}" ]; then
    exec unshare -m --propagation private env OSREFLOG_P1_NS=1 "$0" "$@"
fi

SRC=$(cd "$(dirname "$0")" && pwd)
WORK=/var/tmp/osreflog-p1
R=$WORK/results
RAW=$WORK/raw
BIN=$WORK/bin
CACHE=$WORK/cache
TOP=/run/osreflog-p1/top
DEV=$(findmnt -n -o SOURCE --target / | sed 's/\[.*//')
ROOTSUB=$(findmnt -n -o FSROOT --target / | sed 's|^/||')
EXP=$TOP/osreflog-p1
ACTIONS=/etc/dnf/libdnf5-plugins/actions.d/osreflog-p1.actions
FATRACE_SECS=${FATRACE_SECS:-1200}
VM_MEM=${VM_MEM:-768M}
OWNER=${SUDO_USER:-root}

mkdir -p "$R" "$RAW" "$BIN" "$CACHE" "$TOP"
chmod 700 "$RAW"

say()  { printf '\n\033[1m[p1 %s] %s\033[0m\n' "$(date +%H:%M:%S)" "$*"; }
ms()   { date +%s%3N; }
rec()  { printf '%s\t%s\n' "$1" "$2" >> "$R/metrics.tsv"; }   # key<TAB>value

mount_top() {
    mountpoint -q "$TOP" || mount -o subvolid=5 "$DEV" "$TOP"
    [ -d "$EXP" ] || btrfs subvolume create "$EXP" >/dev/null
}

snap() {   # snap <name> : read-only snapshot of the root subvolume, timed
    local t0 t1
    t0=$(ms); btrfs subvolume snapshot -r "$TOP/$ROOTSUB" "$EXP/$1" >/dev/null; t1=$(ms)
    rec "snapshot_ms.$1" $((t1 - t0))
}

gen_of() { btrfs subvolume show "$1" | awk '/^[[:space:]]*Generation:/{print $2; exit}'; }

run_stage() {   # run_stage <name> : runs stage_<name> in a subshell with -e, records status
    local name=$1 t0 t1 rc
    say "stage $name"
    t0=$(ms)
    ( set -e; "stage_$name" ) 2>&1 | tee "$R/stage-$name.log"
    rc=${PIPESTATUS[0]}
    t1=$(ms)
    printf '%s\t%s\t%s\n' "$name" "$rc" $((t1 - t0)) >> "$R/stages.tsv"
    [ "$rc" -eq 0 ] || say "stage $name FAILED (rc=$rc), continuing"
}

# ---------------------------------------------------------------- stages

stage_env() {
    uname -r > "$R/uname.txt"
    btrfs --version > "$R/btrfs-version.txt"
    btrfs filesystem show "$DEV" > "$R/fs-show.txt" 2>&1 || true
    btrfs filesystem usage -T "$TOP" > "$R/fs-usage.txt" 2>&1 || true
    btrfs subvolume list -t "$TOP" | wc -l > "$R/subvol-count-before.txt"
    btrfs qgroup show "$TOP" > "$R/qgroup.txt" 2>&1 || true
    cat /proc/meminfo | head -5 > "$R/meminfo-start.txt"
    echo "dev=$DEV rootsub=$ROOTSUB"
}

stage_install() {
    # First real use of the design: snapshot, package transaction, snapshot.
    snap s-a
    rpm -q fatrace bpftrace libdnf5-plugin-actions hyperfine figlet > "$R/pkgs-before.txt" 2>&1 || true
    dnf install -y fatrace bpftrace libdnf5-plugin-actions
    snap s-b
}

stage_hook() {
    # dnf5 actions hook that snapshots in its own mount namespace, the way osreflogd will.
    install -m 0755 -o root -g root "$SRC/hook.sh" "$BIN/hook.sh"
    install -m 0644 -o root -g root /dev/stdin "$ACTIONS" <<EOF
pre_transaction::::$BIN/hook.sh pre \${pid}
post_transaction::::$BIN/hook.sh post \${pid}
EOF
    local bt
    rec hook_stage_start_epoch "$(date +%s)"
    bpftrace "$SRC/exec-lifetime.bt" -o "$R/exec-during-dnf.txt" &
    bt=$!
    sleep 3   # let bpftrace attach
    local t0 t1
    t0=$(ms); dnf install -y hyperfine; t1=$(ms)
    rec dnf_install_hyperfine_ms $((t1 - t0))
    kill -INT "$bt" 2>/dev/null || true; wait "$bt" || true

    # Ordering evidence: did rpm write files before the pre hook returned?
    stat -c '%n birth=%W' /usr/bin/hyperfine > "$R/hyperfine-birth.txt"

    # PackageKit path (GNOME Software uses it): does it load libdnf5 plugins?
    if command -v pkcon >/dev/null; then
        t0=$(ms); timeout 600 pkcon install -y figlet > "$R/pkcon.txt" 2>&1 || echo "pkcon rc=$?" >> "$R/pkcon.txt"; t1=$(ms)
        rec pkcon_install_figlet_ms $((t1 - t0))
    fi
    rm -f "$ACTIONS"
    dnf5 history list > "$R/dnf-history.txt" 2>&1 || true
    ausearch -m avc -ts today > "$R/avc-today.txt" 2>&1 || true
    cp "$WORK/hook.log" "$R/hook.log" 2>/dev/null || true
}

diff_pair() {   # diff_pair <tag> <old> <new>
    local tag=$1 old=$EXP/$2 new=$EXP/$3 g t0 t1 proto
    g=$(gen_of "$old")
    rec "gen.$2" "$g"
    rec "gen.$3" "$(gen_of "$new")"

    for proto in 2 1; do
        t0=$(ms)
        if btrfs send -q --proto "$proto" --no-data -p "$old" -f "$R/send-$tag.nodata.stream" "$new" 2> "$R/send-$tag.err"; then
            t1=$(ms); rec "send_nodata_ms.$tag" $((t1 - t0)); rec "send_proto.$tag" "$proto"; break
        fi
    done
    rec "send_nodata_bytes.$tag" "$(stat -c %s "$R/send-$tag.nodata.stream")"
    t0=$(ms); btrfs receive --dump -f "$R/send-$tag.nodata.stream" > "$R/dump-$tag.txt"; t1=$(ms)
    rec "dump_ms.$tag" $((t1 - t0))

    t0=$(ms); btrfs send -q -p "$old" "$new" | wc -c > "$R/send-$tag.full.bytes"; t1=$(ms)
    rec "send_full_ms.$tag" $((t1 - t0))
    rec "send_full_bytes.$tag" "$(cat "$R/send-$tag.full.bytes")"

    t0=$(ms); btrfs subvolume find-new "$new" "$g" > "$R/findnew-$tag.txt"; t1=$(ms)
    rec "findnew_ms.$tag" $((t1 - t0))

    # Package diff from each snapshot's own rpmdb (copied: sqlite wants to write -shm).
    local side
    for side in "$2" "$3"; do
        rm -rf "$WORK/rpmdb-$side"; mkdir -p "$WORK/rpmdb-$side"
        cp -a "$EXP/$side/usr/lib/sysimage/rpm/." "$WORK/rpmdb-$side/"
        rpm --dbpath "$WORK/rpmdb-$side" -qa --qf '%{NEVRA}\n' | sort > "$R/rpm-$side.txt"
    done
    diff "$R/rpm-$2.txt" "$R/rpm-$3.txt" > "$R/rpmdiff-$tag.txt" || true
    cp "$EXP/$3/usr/lib/sysimage/libdnf5/transaction_history.sqlite" "$R/dnf-history-$3.sqlite" 2>/dev/null || true
}

stage_diff() {
    diff_pair install s-a s-b
    local pre post
    pre=$(awk -F'\t' '$1=="pre"{print $5}' "$WORK/hook.log" 2>/dev/null | head -1)
    post=$(awk -F'\t' '$1=="post"{print $5}' "$WORK/hook.log" 2>/dev/null | head -1)
    if [ -n "$pre" ] && [ -n "$post" ]; then diff_pair hook "$pre" "$post"; else echo "no hook snapshots found"; fi
}

stage_space() {
    local i t0 t1
    for i in 1 2 3 4 5; do snap "t$i"; done
    for i in 1 2 3 4 5; do
        t0=$(ms); btrfs subvolume delete "$EXP/t$i" >/dev/null; t1=$(ms); rec "subvol_delete_ms.t$i" $((t1 - t0))
    done
    t0=$(ms); btrfs filesystem du -s "$EXP/s-a" "$EXP/s-b" > "$R/fi-du.txt" 2>&1 || true; t1=$(ms)
    rec fi_du_ms $((t1 - t0))
}

stage_reflink() {   # Q8: FICLONE from a snapshot (top-level mount) into / (root mount), same fs
    rm -f "$WORK/reflink-test"
    if cp --reflink=always "$EXP/s-b/etc/os-release" "$WORK/reflink-test" 2> "$R/reflink.err"; then
        rec reflink_cross_mount ok
    else
        rec reflink_cross_mount "fail: $(head -1 "$R/reflink.err")"
    fi
    rm -f "$WORK/reflink-test"
}

stage_visibility() {   # Q2: is the top-level mount visible outside our namespace?
    local n
    n=$(nsenter -t 1 -m -- findmnt -rn -o TARGET | grep -c osreflog || true)
    rec top_mount_visible_in_pid1_ns "$n"
    n=$(findmnt -rn -o TARGET | grep -c osreflog || true)
    rec top_mount_visible_in_own_ns "$n"
}

ensure_sb() { [ -d "$EXP/s-b" ] || snap s-b; }

stage_nspawn() {
    ensure_sb
    install -m 0644 /dev/null "$BIN/empty-fstab"
    install -m 0644 "$SRC/p1-probe.service" "$BIN/p1-probe.service"
    local i t0 t1
    for i in 1 2 3; do
        t0=$(ms)
        systemd-nspawn --ephemeral -q --register=no --private-network -D "$EXP/s-b" --pipe \
            -- /usr/bin/rpm -q bash > "$R/nspawn-cmd-$i.txt" 2>&1 || echo "rc=$?" >> "$R/nspawn-cmd-$i.txt"
        t1=$(ms); rec "nspawn_cmd_ms.$i" $((t1 - t0))
    done
    rm -rf "$WORK/nspawn-out"; mkdir -p "$WORK/nspawn-out"
    t0=$(ms)
    timeout 240 systemd-nspawn --ephemeral -q --register=no --private-network -D "$EXP/s-b" -b \
        --console=pipe \
        --bind="$WORK/nspawn-out:/run/p1-out" \
        --bind-ro="$BIN/empty-fstab:/etc/fstab" \
        --bind-ro="$BIN/p1-probe.service:/etc/systemd/system/p1-probe.service" \
        -- systemd.unit=multi-user.target systemd.wants=p1-probe.service \
        < /dev/null > "$R/nspawn-boot.log" 2>&1 || echo "nspawn boot rc=$?" >> "$R/nspawn-boot.log"
    t1=$(ms); rec nspawn_boot_ms $((t1 - t0))
    cat "$WORK/nspawn-out/result" > "$R/nspawn-probe-result.txt" 2>/dev/null || echo "no result file" > "$R/nspawn-probe-result.txt"
}

stage_vm() {
    local kv clone initrd sock vfs t0 t1
    ensure_sb
    kv=$(ls "$EXP/s-b/usr/lib/modules" | sort -V | tail -1)
    rec vm_kernel "$kv"
    clone=$EXP/vm-probe
    [ -d "$clone" ] && btrfs subvolume delete "$clone" >/dev/null
    btrfs subvolume snapshot "$EXP/s-b" "$clone" >/dev/null
    : > "$clone/etc/fstab"
    install -m 0644 "$SRC/p1-probe.service" "$clone/etc/systemd/system/p1-probe.service"

    # generic: --no-hostonly, every driver (254 MiB here; panics unpacking in a 512 MiB guest)
    # hostonly: this host's drivers + virtio/virtiofs, no plymouth
    local mode=${DRACUT_MODE:-generic} args
    case $mode in
        generic)  args=(--no-hostonly) ;;
        hostonly) args=(--hostonly --omit plymouth --add-drivers "virtiofs virtio_pci virtio_ring") ;;
        *) echo "unknown DRACUT_MODE $mode"; return 1 ;;
    esac
    rec dracut_mode "$mode"
    initrd=$CACHE/initrd-virtiofs-$mode-$kv.img
    [ "$mode" = generic ] && [ -s "$CACHE/initrd-virtiofs-$kv.img" ] && mv "$CACHE/initrd-virtiofs-$kv.img" "$initrd"
    if [ ! -s "$initrd" ]; then
        t0=$(ms)
        dracut -q "${args[@]}" --no-hostonly-cmdline --add virtiofs --kver "$kv" -f "$initrd"
        t1=$(ms); rec dracut_ms $((t1 - t0))
    fi
    rec initrd_bytes "$(stat -c %s "$initrd")"

    sock=/run/osreflog-p1/vfs.sock; rm -f "$sock"
    /usr/libexec/virtiofsd --socket-path="$sock" --shared-dir="$clone" --cache=auto --xattr \
        > "$R/virtiofsd.log" 2>&1 &
    vfs=$!
    for _ in $(seq 50); do [ -S "$sock" ] && break; sleep 0.1; done
    ( while kill -0 "$vfs" 2>/dev/null; do grep -h VmHWM "/proc/$vfs/status" > "$R/virtiofsd-hwm.txt" 2>/dev/null; sleep 1; done ) &

    t0=$(ms)
    /usr/bin/time -v -o "$R/vm-time.txt" timeout 300 /usr/bin/qemu-kvm \
        -M q35,accel=kvm -cpu host -smp 2 -m "$VM_MEM" \
        -object memory-backend-memfd,id=mem,size="$VM_MEM",share=on -numa node,memdev=mem \
        -device virtio-balloon-pci,free-page-reporting=on \
        -kernel "$clone/usr/lib/modules/$kv/vmlinuz" -initrd "$initrd" \
        -append "root=virtiofs:rootfs rw console=ttyS0 selinux=0 systemd.unit=multi-user.target systemd.wants=p1-probe.service panic=-1 rd.emergency=poweroff" \
        -chardev socket,id=vfs,path="$sock" -device vhost-user-fs-pci,chardev=vfs,tag=rootfs \
        -display none -serial file:"$R/vm-console.log" -monitor none -nic none -no-reboot \
        < /dev/null || echo "qemu rc=$?" >> "$R/vm-time.txt"
    t1=$(ms); rec vm_wall_ms $((t1 - t0)); rec vm_mem "$VM_MEM"
    cat "$clone/var/tmp/p1-result" > "$R/vm-probe-result.txt" 2>/dev/null || echo "no result file" > "$R/vm-probe-result.txt"
    kill "$vfs" 2>/dev/null || true; wait "$vfs" 2>/dev/null || true
    btrfs subvolume delete "$clone" >/dev/null
}

stage_fatrace() {
    say "baseline capture for $FATRACE_SECS s: keep using the laptop normally (browser, editor, terminal)"
    cat /proc/meminfo | head -5 > "$R/meminfo-baseline.txt"
    bpftrace "$SRC/exec-lifetime.bt" -o "$R/exec-baseline.txt" &
    local bt=$! t0 t1
    sleep 3
    t0=$(ms)
    fatrace -f 'W+D<' -j -t -t -u -e -s "$FATRACE_SECS" -o "$RAW/fatrace.jsonl" || true
    t1=$(ms)
    kill -INT "$bt" 2>/dev/null || true; wait "$bt" || true
    rec fatrace_secs_actual $(((t1 - t0) / 1000))
}

cleanup() {
    say "cleanup"
    rm -f "$ACTIONS"
    mount_top
    if [ -d "$EXP" ]; then
        btrfs subvolume list -o "$EXP" | awk '{print $NF}' | sed 's|^osreflog-p1/||' | while read -r s; do
            case "$s" in ''|*/*) continue ;; esac
            btrfs subvolume delete "$EXP/$s" >/dev/null && echo "deleted $s"
        done
        btrfs subvolume delete "$EXP" >/dev/null && echo "deleted osreflog-p1"
    fi
    umount "$TOP" 2>/dev/null || true
}

# ---------------------------------------------------------------- main

if [ "${1:-}" = cleanup ]; then cleanup; exit 0; fi

if [ "${1:-}" = only ]; then
    # Follow-up runs: `run.sh only nspawn vm` writes to results/<timestamp>/ instead.
    shift
    R=$R/followup-$(date +%Y%m%d-%H%M%S); mkdir -p "$R"
    : > "$R/metrics.tsv"; : > "$R/stages.tsv"
    mount_top
    for s in "$@"; do run_stage "$s"; done
else
    : > "$R/metrics.tsv"; : > "$R/stages.tsv"; : > "$WORK/hook.log"
    mount_top
    for s in env install hook diff space reflink visibility nspawn vm fatrace; do
        run_stage "$s"
    done
fi
[ "${KEEP:-0}" = 1 ] || cleanup
cp "$WORK/hook.log" "$WORK/hook.err" "$R/" 2>/dev/null || true
chown -R "$OWNER": "$R" "$RAW"
say "done. results in $R"
