#!/bin/bash
# P8 exit: osreflog installed from COPR on a fresh Fedora 44, as the README says to, in a VM
# made from Fedora's own Cloud Base image (btrfs root, cloud-init), with network.
#
#   scripts/p8/coprvm.sh          # as your user: needs /dev/kvm, not root
#
# The image (~560 MB) is downloaded once to ~/.cache/osreflog-coprvm and checked against
# Fedora's signed CHECKSUM; each run boots a fresh copy-on-write overlay of it. Two boots:
#   1  dnf copr enable, dnf install osreflog osreflog-app, rpm -V, enable the units,
#      osreflog doctor, a dnf transaction (checkpoints around it), the unit's exposure
#   2  osreflogd up at boot, the boot doctor ran, the boot checkpoint, a second transaction,
#      warnings in osreflogd's journal; then the VM powers off
# What the test reports is in target/coprvm/guest.log (printed as it comes), the console in
# serial.log.
set -euo pipefail
REPO=$(cd "$(dirname "$0")/../.." && pwd)
REL=44
IMAGES=https://download.fedoraproject.org/pub/fedora/linux/releases/$REL/Cloud/x86_64/images
CACHE=${XDG_CACHE_HOME:-$HOME/.cache}/osreflog-coprvm
WORK=$REPO/target/coprvm
PORT=${COPRVM_PORT:-8763}
die() { echo "coprvm: $*" >&2; exit 1; }
[ -w /dev/kvm ] || die "no access to /dev/kvm"
mkdir -p "$CACHE" "$WORK"

# The image, checked against the CHECKSUM file signed with Fedora $REL's key (this machine's copy).
sums=$CACHE/CHECKSUM
listing=$(curl -fsSL "$IMAGES/")
name=$(grep -oE "Fedora-Cloud-Base-Generic-$REL-[0-9.]+\.x86_64\.qcow2" <<<"$listing" | head -1)
check=$(grep -oE "Fedora-Cloud-$REL-[0-9.]+-x86_64-CHECKSUM" <<<"$listing" | head -1)
[ -n "$name" ] && [ -n "$check" ] || die "no Generic qcow2 image listed at $IMAGES"
curl -fsSL -o "$sums" "$IMAGES/$check"
key=/etc/pki/rpm-gpg/RPM-GPG-KEY-fedora-$REL-primary
[ -r "$key" ] || die "no $key to check the CHECKSUM signature with"
gpg --dearmor <"$key" >"$CACHE/fedora.gpg"
gpgv --keyring "$CACHE/fedora.gpg" "$sums" 2>/dev/null || die "the CHECKSUM signature doesn't verify"
want=$(sed -n "s/^SHA256 ($name) = //p" "$sums")
base=$CACHE/$name
if [ ! -f "$base" ] || [ "$(sha256sum "$base" | cut -d' ' -f1)" != "$want" ]; then
    echo "downloading $name…"
    curl -fL --progress-bar -o "$base.part" "$IMAGES/$name"
    [ "$(sha256sum "$base.part" | cut -d' ' -f1)" = "$want" ] || die "$name doesn't match its checksum"
    mv "$base.part" "$base"
fi

rm -rf "$WORK"/*
qemu-img create -q -f qcow2 -b "$base" -F qcow2 "$WORK/disk.qcow2" 20G

# cloud-init (NoCloud, served over HTTP to the guest at 10.0.2.2): the test as a unit that runs
# after cloud-init at every boot, one stage per boot.
mkdir -p "$WORK/seed"
printf 'instance-id: osreflog-coprvm-%s\nlocal-hostname: coprvm\n' "$(date +%s)" >"$WORK/seed/meta-data"
cat >"$WORK/seed/user-data" <<'EOF'
#cloud-config
write_files:
  - path: /usr/local/sbin/coprvm-step
    permissions: "0755"
    content: |
      #!/bin/bash
      # Its own virtio port: a login prompt on ttyS0 hangs up everything else writing there.
      out=/dev/virtio-ports/coprvm
      for _ in $(seq 30); do [ -e $out ] && break; sleep 1; done
      exec > >(sed -u 's/^/COPRVM /' | tee -a /var/log/coprvm.log >$out) 2>&1
      S=/var/lib/coprvm-stage
      stage=$(cat $S 2>/dev/null || echo 1)
      fail() { echo "FAIL $*"; }
      run() { echo "\$ $*"; "$@"; local rc=$?; echo "(exit $rc)"; return $rc; }
      echo "stage $stage, $(uname -r), $(findmnt -no SOURCE,FSTYPE /)"
      case $stage in
      1)
          run btrfs subvolume list /
          rpm -q dnf5-plugins >/dev/null || run dnf -y -q install dnf5-plugins
          run dnf -y copr enable khaledsaeed18/osreflog || fail "copr enable"
          run dnf -y -q install osreflog osreflog-app || fail "dnf install"
          run rpm -q osreflog osreflog-app
          run rpm -V osreflog osreflog-app || fail "rpm -V"
          echo "presets: osreflogd $(systemctl is-enabled osreflogd), doctor $(systemctl is-enabled osreflog-boot-doctor)"
          run systemctl enable --now osreflogd.service osreflog-boot-doctor.service || fail "enable"
          systemctl is-active -q osreflogd || fail "osreflogd not active"
          run osreflog status
          run osreflog doctor || fail "doctor"
          run dnf -y -q install figlet || fail "dnf install figlet"
          sleep 5
          run osreflog log
          osreflog log | grep -q 'post-dnf' || fail "no post-dnf checkpoint"
          run osreflog causes -n 5
          echo "exposure: $(systemd-analyze security --no-pager osreflogd.service | tail -1)"
          echo 2 >$S; sync
          echo "rebooting"
          systemctl reboot
          ;;
      2)
          systemctl is-active -q osreflogd || fail "osreflogd not active after the reboot"
          echo "boot doctor: $(systemctl show -p Result --value osreflog-boot-doctor)"
          [ "$(systemctl show -p Result --value osreflog-boot-doctor)" = success ] || fail "boot doctor"
          run osreflog log
          osreflog log | grep -q 'boot' || fail "no boot checkpoint"
          run dnf -y -q remove figlet || fail "dnf remove"
          sleep 5
          run osreflog log
          run osreflog causes -n 5
          run osreflog verify || fail "verify"
          run osreflog doctor || fail "doctor"
          echo "osreflogd warnings: $(journalctl -b -u osreflogd -p warning -o cat --no-pager | wc -l)"
          journalctl -b -u osreflogd -p warning -o cat --no-pager | head -20
          echo DONE
          echo 3 >$S; sync
          systemctl poweroff
          ;;
      esac
  - path: /etc/systemd/system/coprvm-step.service
    content: |
      [Unit]
      After=multi-user.target network-online.target
      Wants=network-online.target
      [Service]
      Type=exec
      ExecStart=/usr/local/sbin/coprvm-step
      [Install]
      WantedBy=multi-user.target
runcmd:
  - [systemctl, daemon-reload]
  - [systemctl, enable, coprvm-step.service]
  - [systemctl, start, --no-block, coprvm-step.service]
EOF
python3 -m http.server -b 127.0.0.1 -d "$WORK/seed" "$PORT" >"$WORK/http.log" 2>&1 &
http=$!
trap 'kill $http 2>/dev/null' EXIT
sleep 1
kill -0 $http 2>/dev/null || die "port $PORT is taken (set COPRVM_PORT)"

echo "booting (test log: $WORK/guest.log; up to 40 minutes)…"
: >"$WORK/guest.log"
tail -F -n +1 "$WORK/guest.log" 2>/dev/null &
tailer=$!
trap 'kill $http $tailer 2>/dev/null' EXIT
timeout 2400 qemu-system-x86_64 -machine q35,accel=kvm -cpu host -smp 4 -m 3072 \
    -drive file="$WORK/disk.qcow2",if=virtio \
    -nic user,model=virtio-net-pci \
    -smbios "type=1,serial=ds=nocloud;s=http://10.0.2.2:$PORT/" \
    -device virtio-serial-pci -chardev file,id=out,path="$WORK/guest.log" \
    -device virtserialport,chardev=out,name=coprvm \
    -display none -serial file:"$WORK/serial.log" -monitor none \
    || echo "coprvm: qemu exited with $? (124: the 40-minute timeout)"
sleep 1
grep -q '^COPRVM DONE' "$WORK/guest.log" || die "the guest didn't finish: see $WORK/guest.log and serial.log"
if grep '^COPRVM FAIL' "$WORK/guest.log"; then
    die "failed checks above"
fi
echo "coprvm: all checks passed"
