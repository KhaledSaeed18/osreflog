#!/bin/bash
# osreflog P3 workload: ~20 real admin tools change /etc on this machine, every change is
# reverted, and osreflog has to explain each one. Replaces "a week of normal use" on a
# dev-only machine (see docs/journal/p3-live.md).
#
#   sudo scripts/p3/workload.sh
#
# Checkpoints: "workload start" → apply → "workload applied" → revert → "workload reverted".
# The middle checkpoint matters: reverted changes cancel out in a start..end diff.
# The revert also runs on any error or Ctrl-C. Brief side effects while it runs (~2 min):
# the clock shows UTC, tuned runs the powersave profile, a dummy NetworkManager connection
# exists (never activated). Results: /var/tmp/osreflog-p3w/results.
set -euo pipefail
[ "$(id -u)" -eq 0 ] || { echo "run with sudo" >&2; exit 1; }
HERE=$(cd "$(dirname "$0")" && pwd)
R=/var/tmp/osreflog-p3w/results
W=/etc/osreflog-workload
UNIT=osreflog-workload.service
ORIG_TZ=$(timedatectl show -p Timezone --value)
ORIG_PRETTY=$(hostnamectl --pretty)
ORIG_PROFILE=$(tuned-adm active | sed -n 's/^Current active profile: //p')
HAD_FIGLET=$(rpm -q figlet >/dev/null 2>&1 && echo 1 || echo 0)
rm -rf "$R"; mkdir -p "$R"
step() { printf '\033[1m[%s] %s\033[0m\n' "$(date +%H:%M:%S)" "$*"; sleep 2.5; }

revert() {
    set +e
    [ -n "$ORIG_TZ" ] && timedatectl set-timezone "$ORIG_TZ"
    hostnamectl set-hostname --pretty "$ORIG_PRETTY"
    [ -n "$ORIG_PROFILE" ] && tuned-adm profile "$ORIG_PROFILE"
    firewall-cmd --permanent --delete-zone=osreflogwl >/dev/null 2>&1
    # firewalld keeps a backup of a deleted zone (osreflog found this on the first run).
    rm -f /etc/firewalld/zones/osreflogwl.xml.old
    nmcli connection delete osreflog-workload >/dev/null 2>&1
    getent passwd osreflogwl >/dev/null && userdel osreflogwl
    getent group osreflogwlg >/dev/null && groupdel osreflogwlg
    alternatives --remove osreflog-alt /usr/bin/true 2>/dev/null
    rm -f /etc/sudoers.d/osreflog-workload /etc/cron.d/osreflog-workload \
        /etc/sysctl.d/99-osreflog-workload.conf /etc/modprobe.d/osreflog-workload.conf \
        /etc/ssh/sshd_config.d/99-osreflog-workload.conf
    rm -rf /etc/systemd/journald.conf.d/osreflog-workload.conf
    rmdir /etc/systemd/journald.conf.d 2>/dev/null
    if [ -f /etc/dconf/db/local.d/00-osreflog-workload ]; then
        rm -f /etc/dconf/db/local.d/00-osreflog-workload
        dconf update
    fi
    if [ -f "/etc/systemd/system/$UNIT" ]; then
        systemctl disable "$UNIT" 2>/dev/null
        rm -rf "/etc/systemd/system/$UNIT" "/etc/systemd/system/$UNIT.d"
        systemctl daemon-reload
    fi
    [ -e "$W/frozen.conf" ] && chattr -i "$W/frozen.conf"
    rm -rf "$W"
    [ "$HAD_FIGLET" = 0 ] && rpm -q figlet >/dev/null 2>&1 && dnf remove -y figlet >> "$R/dnf.log" 2>&1
    set -e
}
trap revert EXIT

osreflog status > "$R/status-before.txt"
grep -q '^live       watching' "$R/status-before.txt" || { cat "$R/status-before.txt"; echo "the live witness is not running" >&2; exit 1; }
osreflog checkpoint -m "workload start" --json > "$R/cp-start.json"

step "timedatectl set-timezone UTC (systemd-timedated writes /etc/localtime)"
timedatectl set-timezone UTC
step "hostnamectl --pretty (systemd-hostnamed writes /etc/machine-info)"
hostnamectl set-hostname --pretty "osreflog workload"
step "tuned-adm profile powersave"
tuned-adm profile powersave
step "firewall-cmd --permanent --new-zone"
firewall-cmd --permanent --new-zone=osreflogwl >/dev/null
step "nmcli connection add (dummy, never activated)"
nmcli connection add type dummy ifname osrflwl0 con-name osreflog-workload autoconnect no \
    ipv4.method disabled ipv6.method disabled >/dev/null
step "useradd and groupadd (passwd, group, shadow, gshadow)"
useradd -r -M -s /sbin/nologin osreflogwl
groupadd osreflogwlg
step "alternatives --install"
alternatives --install /usr/local/bin/osreflog-alt osreflog-alt /usr/bin/true 10
step "sudoers drop-in, checked by visudo, installed with install(1)"
tmp=$(mktemp); printf '# osreflog workload\nDefaults:osreflogwl !lecture\n' > "$tmp"
visudo -cqf "$tmp" && install -m 0440 "$tmp" /etc/sudoers.d/osreflog-workload; rm -f "$tmp"
step "cron.d drop-in with tee"
echo '# osreflog workload: never runs' | tee /etc/cron.d/osreflog-workload >/dev/null
step "sysctl.d drop-in (not applied)"
printf '# osreflog workload\nvm.osreflog_not_a_real_key = 1\n' > /etc/sysctl.d/99-osreflog-workload.conf
step "modprobe.d drop-in with cp"
tmp=$(mktemp); echo '# osreflog workload' > "$tmp"; cp "$tmp" /etc/modprobe.d/osreflog-workload.conf; rm -f "$tmp"
step "journald.conf.d drop-in with install -D"
tmp=$(mktemp); printf '[Journal]\n#Storage=auto\n' > "$tmp"
install -D -m 0644 "$tmp" /etc/systemd/journald.conf.d/osreflog-workload.conf; rm -f "$tmp"
step "sshd_config.d drop-in from python (sshd not reloaded)"
python3 -I -c 'open("/etc/ssh/sshd_config.d/99-osreflog-workload.conf", "w").write("# osreflog workload\n")'
step "dconf system default + dconf update"
printf '[org/gnome/desktop/interface]\n# osreflog workload\n' > /etc/dconf/db/local.d/00-osreflog-workload
dconf update
step "a unit: file, daemon-reload, enable, systemctl edit --stdin"
printf '[Unit]\nDescription=osreflog workload\n[Service]\nType=oneshot\nExecStart=/bin/true\n[Install]\nWantedBy=multi-user.target\n' > "/etc/systemd/system/$UNIT"
systemctl daemon-reload
systemctl enable "$UNIT" 2>/dev/null
printf '[Service]\nEnvironment=OSREFLOG=1\n' | systemctl edit --stdin --drop-in=osreflog "$UNIT"
step "editors and file tools in $W"
mkdir -p "$W"
printf 'a = 1\nb = 2\n' > "$W/sed.conf"; sleep 2.5
sed -i 's/a = 1/a = 10/' "$W/sed.conf"; sleep 2.5
printf 'x = 1\n' > "$W/perl.conf"; sleep 2.5
perl -pi -e 's/x = 1/x = 2/' "$W/perl.conf"; sleep 2.5
printf 'v = 1\n' > "$W/vi.conf"; sleep 2.5
vi -es -c '%s/v = 1/v = 2/' -c 'wq' "$W/vi.conf"; sleep 2.5
tar -C "$W" -xf <(tar -C /etc -cf - hostname); sleep 2.5
rsync -a /etc/os-release "$W/os-release.copy"; sleep 2.5
chmod 600 "$W/sed.conf"; chmod 644 "$W/sed.conf"; sleep 2.5
ln -s sed.conf "$W/link.conf"; sleep 2.5
mv "$W/perl.conf" "$W/perl-renamed.conf"; sleep 2.5
setfacl -m u:nobody:r "$W/vi.conf"; sleep 2.5
printf 'frozen\n' > "$W/frozen.conf"; chattr +i "$W/frozen.conf"
step "dnf install figlet"
[ "$HAD_FIGLET" = 0 ] && dnf install -y --setopt=install_weak_deps=False figlet > "$R/dnf.log" 2>&1

sleep 3
osreflog checkpoint -m "workload applied" --json > "$R/cp-applied.json"
osreflog why /etc/shadow --json > "$R/why-shadow.json"
osreflog why /etc/NetworkManager/system-connections/osreflog-workload.nmconnection --json > "$R/why-nm.json"
osreflog why /etc/passwd > "$R/why-passwd.txt"
osreflog why "$W/sed.conf" > "$R/why-sed.txt"

printf '\033[1m[%s] reverting everything\033[0m\n' "$(date +%H:%M:%S)"
trap - EXIT
revert
sleep 3
osreflog checkpoint -m "workload reverted" --json > "$R/cp-reverted.json"

for _ in $(seq 90); do
    [ "$(osreflog status --json | python3 -I -c 'import json,sys; print(json.load(sys.stdin)["pending_diffs"])')" = 0 ] && break
    sleep 1
done
name() { python3 -I -c 'import json,sys; print(json.load(open(sys.argv[1]))["name"])' "$1"; }
s=$(name "$R/cp-start.json"); a=$(name "$R/cp-applied.json"); v=$(name "$R/cp-reverted.json")
osreflog diff "$s..$a" --all --json > "$R/diff-apply.json"
osreflog diff "$a..$v" --all --json > "$R/diff-revert.json"
osreflog diff "$s..$v" --all --json > "$R/diff-net.json"
osreflog diff "$s..$a" > "$R/diff-apply.txt"
osreflog diff "$a..$v" > "$R/diff-revert.txt"
osreflog causes -n 80 > "$R/causes.txt"
osreflog status > "$R/status-after.txt"
osreflog verify > "$R/verify.txt"
chown -R "${SUDO_UID:-0}:${SUDO_GID:-0}" /var/tmp/osreflog-p3w
python3 -I "$HERE/check_workload.py" "$R"
