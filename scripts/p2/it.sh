#!/bin/bash
# osreflog P2 integration test on a throwaway loopback btrfs image (never the real disk).
#
#   cargo test -p osreflogd --test loopback --no-run   # as your user: builds the test binary
#   sudo scripts/p2/it.sh                               # runs it as root
#
# Creates a 1 GiB image in /var/tmp/osreflog-it, mounts its `root` subvolume in a private
# mount namespace, runs the engine end to end against it, writes the send-stream fixture into
# crates/btrfs/tests/fixtures, then unmounts and deletes the image.
set -euo pipefail
[ "$(id -u)" -eq 0 ] || { echo "run with sudo" >&2; exit 1; }
if [ -z "${OSREFLOG_IT_NS:-}" ]; then
    exec unshare -m --propagation private env OSREFLOG_IT_NS=1 "$0" "$@"
fi

REPO=$(cd "$(dirname "$0")/../.." && pwd)
BIN=$(find "$REPO/target/debug/deps" -maxdepth 1 -type f -executable -name 'loopback-*' -printf '%T@ %p\n' | sort -rn | head -1 | cut -d' ' -f2)
[ -n "$BIN" ] || { echo "build it first: cargo test -p osreflogd --test loopback --no-run" >&2; exit 1; }
WORK=/var/tmp/osreflog-it
RUN=/run/osreflog-it
IMG=$WORK/btrfs.img
FIXTURES=$REPO/crates/btrfs/tests/fixtures

cleanup() {
    umount -R "$RUN/top" 2>/dev/null || true
    umount "$RUN/root" 2>/dev/null || true
    umount "$RUN/fs" 2>/dev/null || true
    rm -f "$IMG"
}
trap cleanup EXIT

mkdir -p "$WORK" "$RUN/fs" "$RUN/root" "$RUN/top" "$FIXTURES"
truncate -s 1G "$IMG"
mkfs.btrfs -q -f "$IMG"
mount -o loop "$IMG" "$RUN/fs"
LOOP=$(findmnt -n -o SOURCE "$RUN/fs")
btrfs subvolume create "$RUN/fs/root" >/dev/null
mount -o subvol=root "$LOOP" "$RUN/root"
echo "image $IMG on $LOOP, root subvolume at $RUN/root"

OSREFLOG_LOOPBACK_ROOT=$RUN/root OSREFLOG_LOOPBACK_TOP=$RUN/top OSREFLOG_FIXTURES=$FIXTURES \
    "$BIN" --ignored --test-threads=1 --nocapture

chown -R "${SUDO_UID:-0}:${SUDO_GID:-0}" "$FIXTURES"
echo "loopback integration test passed"
