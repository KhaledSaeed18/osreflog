#!/bin/bash
# P7, boot gate G1: a test VM laid out like this laptop, for the metal flow to run in before it
# ever runs on real hardware. Built from a fresh osreflog checkpoint, no downloads:
#
#   GPT disk image (sparse raw, in its own subvolume osreflog/testvm at the btrfs top level)
#   ├── 1  ESP, vfat      Fedora's shim and GRUB, copied from this machine's ESP
#   ├── 2  /boot, ext4    one Fedora-signed kernel, a host-only initramfs with virtio drivers,
#   │                     this machine's GRUB config with the image's UUIDs, one BLS entry
#   └── 3  btrfs          subvolumes `root` (the checkpoint, copied with labels and xattrs) and
#                         `home` (empty)
#
# booted by QEMU with OVMF and Secure Boot on, so shim checks the kernel as on the laptop.
#
#   sudo scripts/p7/testvm.sh build    # checkpoint → image (replaces an existing image)
#   sudo scripts/p7/testvm.sh check    # boot headless, report what the VM sees, power off
#   sudo scripts/p7/testvm.sh run      # boot on this terminal (serial console; Ctrl-A X quits)
#   sudo scripts/p7/testvm.sh log      # the last boot's serial log
#   sudo scripts/p7/testvm.sh delete   # remove the image
#
# Left out of the copy: container and VM storage, caches, logs, temp files, Wi-Fi/VPN profiles
# and SSH host keys. The VM boots multi-user.target (systemd.unit= on its cmdline), logs root in
# on the serial console, and has no network. TESTVM_MEM (MiB, default 1536) sizes the guest.
set -euo pipefail
[ "$(id -u)" -eq 0 ] || { echo "run with sudo" >&2; exit 1; }
if [ -z "${OSREFLOG_P7_NS:-}" ]; then
    # The btrfs top level is mounted in a private namespace only, as osreflogd does.
    exec unshare -m --propagation private env OSREFLOG_P7_NS=1 "$0" "$@"
fi

MODE=${1:-}
MEM=${TESTVM_MEM:-1536}
OVMF=/usr/share/edk2/ovmf
TOP=/run/osreflog-testvm/top
DEV=$(findmnt -no SOURCE -T / | sed 's/\[.*\]$//')
mkdir -p "$TOP"
mountpoint -q "$TOP" || mount -o subvolid=5 "$DEV" "$TOP"
AREA=$TOP/osreflog/testvm
DISK=$AREA/disk.raw
VARS=$AREA/OVMF_VARS.fd
LOG=$AREA/serial.log
MON=$AREA/monitor.sock

die() { echo "testvm: $*" >&2; exit 1; }

build() {
    command -v osreflog >/dev/null || die "osreflog is not installed (scripts/install-dev.sh)"
    local kver=${TESTVM_KVER:-$(uname -r)}
    [ -f "/boot/vmlinuz-$kver" ] || die "/boot/vmlinuz-$kver missing"

    local name snap
    name=$(osreflog checkpoint -m "p7: test VM image" --json \
        | python3 -I -c 'import json,sys; print(json.load(sys.stdin)["name"])')
    snap=$TOP/osreflog/snapshots/$name
    [ -d "$snap/usr/lib/modules/$kver" ] || die "$name has no modules for $kver"
    echo "checkpoint $name, kernel $kver"

    if [ -d "$AREA" ]; then btrfs subvolume delete "$AREA" >/dev/null; fi
    btrfs subvolume create "$AREA" >/dev/null
    chattr +C "$AREA"                                   # no CoW under a VM disk
    truncate -s 40G "$DISK"
    sfdisk -q "$DISK" <<'EOF'
label: gpt
size=600MiB, type=C12A7328-F81F-11D2-BA4B-00A0C93EC93B, name="EFI System Partition"
size=1GiB, type=0FC63DAF-8483-4772-8E79-3D69D8477DE4, name="boot"
type=0FC63DAF-8483-4772-8E79-3D69D8477DE4, name="root"
EOF

    local loop W=/run/osreflog-testvm/img
    loop=$(losetup -fP --show "$DISK")
    # shellcheck disable=SC2064
    trap "umount -R $W 2>/dev/null; losetup -d $loop" EXIT
    mkfs.vfat -F 32 -n EFI "${loop}p1" >/dev/null
    mkfs.ext4 -q -L boot "${loop}p2"
    mkfs.btrfs -q -L fedora_testvm "${loop}p3"
    local esp_uuid boot_uuid root_uuid
    esp_uuid=$(blkid -s UUID -o value "${loop}p1")
    boot_uuid=$(blkid -s UUID -o value "${loop}p2")
    root_uuid=$(blkid -s UUID -o value "${loop}p3")

    mkdir -p "$W"
    mount "${loop}p3" "$W"
    btrfs subvolume create "$W/root" >/dev/null
    btrfs subvolume create "$W/home" >/dev/null
    umount "$W"
    mount -o subvol=root,compress=zstd:1 "${loop}p3" "$W"
    local R=$W

    echo "copying the checkpoint (labels, ACLs, xattrs, hard links)…"
    local t0=$SECONDS
    rsync -aHAXS --numeric-ids --one-file-system \
        --exclude='/var/lib/docker/*' --exclude='/var/lib/containers/*' \
        --exclude='/var/lib/libvirt/images/*' --exclude='/var/lib/flatpak/*' \
        --exclude='/var/lib/systemd/coredump/*' --exclude='/var/cache/*' \
        --exclude='/var/log/journal/*' --exclude='/var/tmp/*' --exclude='/tmp/*' \
        --exclude='/etc/NetworkManager/system-connections/*' --exclude='/etc/ssh/ssh_host_*' \
        "$snap/" "$R/"
    echo "copied in $((SECONDS - t0)) s, $(df -h --output=used "$R" | tail -1 | tr -d ' ') used in the image"

    cat >"$R/etc/fstab" <<EOF
# osreflog test VM (scripts/p7/testvm.sh)
UUID=$root_uuid / btrfs subvol=root,compress=zstd:1 0 0
UUID=$boot_uuid /boot ext4 defaults 1 2
UUID=$esp_uuid /boot/efi vfat umask=0077,shortname=winnt 0 2
UUID=$root_uuid /home btrfs subvol=home,compress=zstd:1 0 0
EOF

    mkdir -p "$R/etc/systemd/system/serial-getty@ttyS0.service.d"
    cat >"$R/etc/systemd/system/serial-getty@ttyS0.service.d/autologin.conf" <<'EOF'
[Service]
ExecStart=
ExecStart=-/usr/sbin/agetty -o '-p -f -- \\u' --noclear --autologin root --keep-baud 115200,57600,38400,9600 - $TERM
EOF
    # What `check` reads back: one TESTVM line per fact, on the serial console.
    cat >"$R/usr/local/libexec/osreflog-testvm-check" <<'EOF'
#!/bin/bash
state=$(systemctl is-system-running --wait 2>/dev/null)
say() { echo "TESTVM $*"; }
say "kernel $(uname -r)"
say "cmdline $(cat /proc/cmdline)"
say "secureboot $(mokutil --sb-state 2>&1 | head -1)"
say "lockdown $(cat /sys/kernel/security/lockdown 2>&1)"
say "selinux $(getenforce)"
for m in / /boot /boot/efi /home; do say "mount $m $(findmnt -no SOURCE,FSTYPE,OPTIONS "$m")"; done
say "grubenv $(grub2-editenv list 2>&1 | tr '\n' ' ')"
say "entries $(ls /boot/loader/entries | tr '\n' ' ')"
say "osreflogd $(systemctl is-active osreflogd)"
say "boot $(systemd-analyze 2>/dev/null | head -1)"
say "state $state"
systemctl --failed --plain --no-legend | awk '{print "TESTVM failed " $1}'
echo TESTVM-READY
EOF
    chmod 0755 "$R/usr/local/libexec/osreflog-testvm-check"
    cat >"$R/etc/systemd/system/osreflog-testvm-check.service" <<'EOF'
[Unit]
Description=osreflog test VM: report state on the console
After=multi-user.target

[Service]
# exec, not oneshot: the boot must finish without waiting for this unit, which waits for the boot.
Type=exec
ExecStart=/usr/local/libexec/osreflog-testvm-check
StandardOutput=journal+console

[Install]
WantedBy=multi-user.target
EOF
    ln -sf /etc/systemd/system/osreflog-testvm-check.service \
        "$R/etc/systemd/system/multi-user.target.wants/osreflog-testvm-check.service"

    # /boot: one kernel, its initramfs, this machine's GRUB config with the image's UUIDs.
    mount "${loop}p2" "$R/boot"
    local f
    for f in vmlinuz config System.map; do
        if [ -e "/boot/$f-$kver" ]; then cp -a "/boot/$f-$kver" "$R/boot/"; fi
    done
    if [ -f "/boot/.vmlinuz-$kver.hmac" ]; then cp -a "/boot/.vmlinuz-$kver.hmac" "$R/boot/"; fi
    echo "building a host-only initramfs with virtio drivers…"
    dracut -q -f --hostonly --no-hostonly-cmdline --omit plymouth \
        --add-drivers "virtio_blk virtio_pci" --kver "$kver" "$R/boot/initramfs-$kver.img"
    rsync -a --exclude=grubenv /boot/grub2/ "$R/boot/grub2/"
    local host_root host_boot host_esp
    host_root=$(findmnt -no UUID /)
    host_boot=$(findmnt -no UUID /boot)
    host_esp=$(findmnt -no UUID /boot/efi)
    sed -i -e "s/$host_root/$root_uuid/g" -e "s/$host_boot/$boot_uuid/g" \
        -e "s/$host_esp/$esp_uuid/g" "$R/boot/grub2/grub.cfg"
    local mid entry
    mid=$(cat "$R/etc/machine-id")
    entry=$mid-$kver
    mkdir -p "$R/boot/loader/entries"
    cat >"$R/boot/loader/entries/$entry.conf" <<EOF
title Fedora Linux ($kver) osreflog test VM
version $kver
linux /vmlinuz-$kver
initrd /initramfs-$kver.img
options root=UUID=$root_uuid ro rootflags=subvol=root console=tty0 console=ttyS0,115200 systemd.unit=multi-user.target
grub_users \$grub_users
grub_arg --unrestricted
grub_class fedora
EOF
    grub2-editenv "$R/boot/grub2/grubenv" create
    grub2-editenv "$R/boot/grub2/grubenv" set "saved_entry=$entry"

    # Labels for what this script wrote (rsync kept the rest). ESP is vfat: no labels.
    local fc=$R/etc/selinux/targeted/contexts/files/file_contexts
    if ! setfiles -r "$R" "$fc" "$R/etc/fstab" "$R/etc/systemd/system" \
        "$R/usr/local/libexec/osreflog-testvm-check" "$R/boot" 2>&1; then
        echo "setfiles failed: the VM relabels on its first boot"
        touch "$R/.autorelabel"
    fi

    # ESP: shim, GRUB, fallback; the stub grub.cfg finds the image's /boot.
    mkdir -p "$R/boot/efi"
    mount "${loop}p1" "$R/boot/efi"
    mkdir -p "$R/boot/efi/EFI"
    cp -r /boot/efi/EFI/fedora /boot/efi/EFI/BOOT "$R/boot/efi/EFI/"
    cat >"$R/boot/efi/EFI/fedora/grub.cfg" <<EOF
search --no-floppy --fs-uuid --set=dev $boot_uuid
set prefix=(\$dev)/grub2
export \$prefix
configfile \$prefix/grub.cfg
EOF
    sync
    umount -R "$W"
    losetup -d "$loop"
    trap - EXIT

    cp "$OVMF/OVMF_VARS.secboot.fd" "$VARS"
    echo "$name $kver $entry" >"$AREA/source"
    echo "image: $(du -h "$DISK" | cut -f1) used of 40G; boot it with: sudo $0 check"
}

# Memory guard: the host has ~7 GiB and runs a desktop; never push it into heavy swapping.
ready() {
    [ -f "$DISK" ] || die "no image: sudo $0 build"
    local avail
    avail=$(awk '/MemAvailable/ {print int($2 / 1024)}' /proc/meminfo)
    [ "$avail" -ge $((MEM + 256)) ] || die "only $avail MiB available, the VM needs $MEM + 256"
}

# exec: `qemu … &` then leaves $! pointing at qemu itself, not a subshell.
qemu() {
    exec qemu-system-x86_64 -name osreflog-testvm \
        -machine q35,smm=on,accel=kvm -cpu host -smp 2 -m "$MEM" \
        -global driver=cfi.pflash01,property=secure,value=on \
        -drive if=pflash,format=raw,unit=0,readonly=on,file="$OVMF/OVMF_CODE.secboot.fd" \
        -drive if=pflash,format=raw,unit=1,file="$VARS" \
        -drive if=virtio,format=raw,file="$DISK" \
        -nic none "$@"
}

check() {
    ready
    rm -f "$LOG" "$MON"
    local t0=$SECONDS timeout=${TESTVM_TIMEOUT:-600}
    qemu -display none -serial "file:$LOG" -monitor "unix:$MON,server,nowait" &
    local pid=$!
    # shellcheck disable=SC2064
    trap "kill $pid 2>/dev/null" EXIT                 # never leave the VM running behind us
    echo "booting (pid $pid, serial log $LOG)…"
    until grep -q TESTVM-READY "$LOG" 2>/dev/null; do
        kill -0 "$pid" 2>/dev/null || { tail -40 "$LOG"; die "qemu exited before the VM reported"; }
        [ $((SECONDS - t0)) -lt "$timeout" ] || { tail -40 "$LOG"; kill "$pid"; die "no report in $timeout s"; }
        sleep 2
    done
    echo "reported after $((SECONDS - t0)) s; qemu RSS $(awk '/VmHWM/ {print int($2/1024) " MiB peak"}' "/proc/$pid/status")"
    # Console control sequences can come before a line's text: match anywhere, not at ^.
    local log
    log=$(tr -d '\r' <"$LOG" | grep -ao 'TESTVM .*' || true)
    sed 's/^TESTVM /  /' <<<"$log"
    python3 -I -c 'import socket,sys; s=socket.socket(socket.AF_UNIX); s.connect(sys.argv[1]); s.sendall(b"system_powerdown\n")' "$MON"
    for _ in $(seq 60); do kill -0 "$pid" 2>/dev/null || break; sleep 1; done
    if kill -0 "$pid" 2>/dev/null; then echo "  (no clean poweroff in 60 s: stopped)"; kill "$pid"; fi
    wait "$pid" 2>/dev/null || true

    local fails=0 entry
    entry=$(cut -d' ' -f3 "$AREA/source")
    report() { if grep -qE "^TESTVM $2" <<<"$log"; then echo "ok   $1"; else echo "FAIL $1"; fails=$((fails + 1)); fi; }
    report "Secure Boot on in the VM" "secureboot SecureBoot enabled"
    report "/ is the image's root subvolume" "mount / .*subvol=/root"
    report "/boot is ext4" "mount /boot [^ ]+ ext4 "
    report "the BLS entry is in place" "entries .*$entry\\.conf"
    report "system running (or degraded: failed units listed above)" "state (running|degraded)"
    if [ "$fails" -ne 0 ]; then
        echo "--- last 60 lines of the serial log"
        tr -d '\r' <"$LOG" | tail -60
        exit 1
    fi
}

case "$MODE" in
    build) build ;;
    check) check ;;
    run) ready; qemu -nographic ;;
    log) tr -d '\r' <"$LOG" ;;
    delete) btrfs subvolume delete "$AREA" >/dev/null && echo "deleted $AREA" ;;
    *) sed -n '2,25p' "$0" | sed 's/^# \{0,1\}//'; exit 2 ;;
esac
