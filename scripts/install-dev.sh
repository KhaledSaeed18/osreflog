#!/bin/bash
# Installs a development build of osreflog on this machine (until there's an RPM in COPR).
#
#   cargo build --release            # as your user
#   sudo scripts/install-dev.sh      # installs and starts osreflogd
#   sudo scripts/install-dev.sh --uninstall
#
# Uninstalling removes the programs, units, policies and dnf hook. It keeps the history: the
# snapshots and ledger under `osreflog/` at the btrfs top level (see the message it prints).
set -euo pipefail
[ "$(id -u)" -eq 0 ] || { echo "run with sudo" >&2; exit 1; }
REPO=$(cd "$(dirname "$0")/.." && pwd)

LIBEXEC=/usr/local/libexec/osreflog
BIN=/usr/local/bin/osreflog
UNIT=/etc/systemd/system/osreflogd.service
BOOT_UNIT=/etc/systemd/system/osreflog-boot-doctor.service
USER_UNIT=/etc/systemd/user/osreflog-user.service
DBUS=/etc/dbus-1/system.d/io.github.khaledsaeed18.osreflog1.conf
POLKIT=/usr/share/polkit-1/actions/io.github.khaledsaeed18.osreflog.policy
ACTIONS=/etc/dnf/libdnf5-plugins/actions.d/osreflog.actions

reload_bus() {
    systemctl reload dbus-broker.service 2>/dev/null || systemctl reload dbus.service 2>/dev/null || true
}

if [ "${1:-}" = --uninstall ]; then
    rm -f "$ACTIONS"
    systemctl disable --now osreflogd.service 2>/dev/null || true
    systemctl disable osreflog-boot-doctor.service 2>/dev/null || true
    rm -f "$UNIT" "$BOOT_UNIT" "$USER_UNIT" "$DBUS" "$POLKIT" "$BIN"
    rm -rf "$LIBEXEC"
    systemctl daemon-reload
    reload_bus
    echo "osreflog uninstalled. History kept at the btrfs top level in osreflog/;"
    echo "to delete it: mount -o subvolid=5 <device> /mnt, then btrfs subvolume delete each"
    echo "/mnt/osreflog/snapshots/* and /mnt/osreflog/ledger, and rm -r /mnt/osreflog."
    exit 0
fi

for f in "$REPO/target/release/osreflogd" "$REPO/target/release/osreflog" "$REPO/target/release/osreflog-user"; do
    [ -x "$f" ] || { echo "missing $f: run cargo build --release first" >&2; exit 1; }
done
if ! rpm -q libdnf5-plugin-actions >/dev/null; then
    dnf install -y --setopt=install_weak_deps=False libdnf5-plugin-actions
fi

install -d -m 0755 "$LIBEXEC"
install -m 0755 "$REPO/target/release/osreflogd" "$LIBEXEC/osreflogd"
install -m 0755 "$REPO/target/release/osreflog-user" "$LIBEXEC/osreflog-user"
install -m 0755 "$REPO/target/release/osreflog" "$BIN"
install -m 0644 "$REPO/data/systemd/osreflogd.service" "$UNIT"
install -m 0644 "$REPO/data/systemd/osreflog-boot-doctor.service" "$BOOT_UNIT"
install -d -m 0755 "$(dirname "$USER_UNIT")"
install -m 0644 "$REPO/data/systemd/osreflog-user.service" "$USER_UNIT"
install -m 0644 "$REPO/data/dbus/io.github.khaledsaeed18.osreflog1.conf" "$DBUS"
install -m 0644 "$REPO/data/polkit/io.github.khaledsaeed18.osreflog.policy" "$POLKIT"
restorecon -R "$LIBEXEC" "$BIN" "$UNIT" "$BOOT_UNIT" "$USER_UNIT" "$DBUS" "$POLKIT" 2>/dev/null || true

systemctl daemon-reload
reload_bus
systemctl enable osreflogd.service
# After every boot of the default system: /boot back as it was after a metal probe (P7).
systemctl enable osreflog-boot-doctor.service
systemctl restart osreflogd.service
# The hook goes in last: from here on every dnf transaction is bracketed by checkpoints.
install -m 0644 "$REPO/data/dnf/osreflog.actions" "$ACTIONS"
restorecon "$ACTIONS" 2>/dev/null || true
systemctl --no-pager --lines=5 status osreflogd.service || true
echo "osreflog installed. Try: osreflog doctor; osreflog log"
echo "For your settings history, as your user: systemctl --user enable --now osreflog-user"
