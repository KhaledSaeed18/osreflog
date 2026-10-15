#!/bin/bash
# P5: undo on the real machine. Each scenario changes something with a real tool, finds the
# cause osreflog recorded, shows the dry run, applies the undo, and checks the result.
# Everything lives under /etc/osreflog-undo (plus one test unit) and is removed at the end.
#
#   sudo scripts/p5/undo-workload.sh        (with osreflogd installed and running)
set -uo pipefail
[ "$(id -u)" -eq 0 ] || { echo "run with sudo" >&2; exit 1; }
DIR=/etc/osreflog-undo
UNIT=osreflog-undo-test.service
FAILED=0
ok() { echo "  ok   $*"; }
bad() { echo "  FAIL $*"; FAILED=$((FAILED + 1)); }
check() { if eval "$1"; then ok "$2"; else bad "$2"; fi; }
settle() { sleep 3; }   # causes close after 2 s of quiet
# The newest cause that changed $1.
cause_of() {
    osreflog why "$1" -n 1 --json | python3 -I -c 'import json,sys; e=json.load(sys.stdin); print(e[0]["cause"]["id"] if e else "")'
}
undo() { osreflog undo "$1" --apply; }
meta() { stat -c '%a %U %G' "$1" 2>/dev/null; ls -Z "$1" 2>/dev/null | cut -d' ' -f1; }

cleanup() {
    systemctl disable "$UNIT" >/dev/null 2>&1
    rm -f "/etc/systemd/system/$UNIT"
    systemctl daemon-reload
    rm -rf "$DIR"
}
cleanup
echo "setup"
mkdir -p "$DIR"
printf 'mode = old\nkeep = yes\n' >"$DIR/sed.conf"
printf 'answer = 41\n' >"$DIR/editor.conf"
printf 'precious\n' >"$DIR/deleted.conf"
chmod 0640 "$DIR/deleted.conf"; chgrp wheel "$DIR/deleted.conf"
printf 'a\n' >"$DIR/target-a"; printf 'b\n' >"$DIR/target-b"
ln -s target-a "$DIR/link"
printf 'v1\n' >"$DIR/conflict.conf"
printf '[Unit]\nDescription=osreflog undo test\n[Service]\nType=oneshot\nExecStart=/bin/true\n[Install]\nWantedBy=multi-user.target\n' >"/etc/systemd/system/$UNIT"
systemctl daemon-reload
settle
osreflog checkpoint -m "undo workload setup" >/dev/null
DELETED_META=$(meta "$DIR/deleted.conf")

echo; echo "1. sed -i"
sed -i 's/mode = old/mode = new/' "$DIR/sed.conf"; settle
ID=$(cause_of "$DIR/sed.conf")
osreflog undo "$ID"
undo "$ID"
check 'grep -q "mode = old" $DIR/sed.conf && grep -q "keep = yes" $DIR/sed.conf' "sed -i undone (#$ID)"
settle
UNDO1=$(cause_of "$DIR/sed.conf")
check '[ "$(osreflog show $UNDO1 --json | python3 -I -c "import json,sys; print(json.load(sys.stdin)[\"cause\"][\"kind\"])")" = undo ]' "the undo is recorded as an undo cause (#$UNDO1)"

echo; echo "2. an editor's save (temp file renamed over)"
python3 -I -c "
import os
p='$DIR/editor.conf'
open(p+'.tmp','w').write('answer = 42\n')
os.rename(p+'.tmp', p)"
settle
ID=$(cause_of "$DIR/editor.conf")
undo "$ID" >/dev/null
check 'grep -q "answer = 41" $DIR/editor.conf' "editor save undone (#$ID)"

echo; echo "3. a created file"
install -m 0644 /dev/null "$DIR/created.conf"; echo "new" >"$DIR/created.conf"; settle
ID=$(cause_of "$DIR/created.conf")
undo "$ID" >/dev/null
check '[ ! -e $DIR/created.conf ]' "created file removed (#$ID)"
check 'find /var/lib/osreflog/attic -path "*etc/osreflog-undo/created.conf" | grep -q .' "and kept in the attic"

echo; echo "4. a deleted file (owner, mode and label come back)"
rm "$DIR/deleted.conf"; settle
ID=$(cause_of "$DIR/deleted.conf")
undo "$ID" >/dev/null
check '[ "$(cat $DIR/deleted.conf 2>/dev/null)" = precious ]' "deleted file restored (#$ID)"
check '[ "$(meta $DIR/deleted.conf)" = "$DELETED_META" ]' "with its owner, mode and label ($(meta "$DIR/deleted.conf" | tr '\n' ' '))"

echo; echo "5. a symlink retargeted"
ln -sfn target-b "$DIR/link"; settle
ID=$(cause_of "$DIR/link")
undo "$ID" >/dev/null
check '[ "$(readlink $DIR/link)" = target-a ]' "symlink points back at target-a (#$ID)"

echo; echo "6. systemctl enable (pid 1 writes the link)"
systemctl enable "$UNIT" 2>/dev/null; settle
ID=$(cause_of "/etc/systemd/system/multi-user.target.wants/$UNIT")
undo "$ID"
check '[ "$(systemctl is-enabled $UNIT 2>/dev/null)" = disabled ]' "unit disabled again, systemd reloaded (#$ID)"

echo; echo "7. a conflict: changed again after the cause"
echo v2 >"$DIR/conflict.conf"; settle
ID=$(cause_of "$DIR/conflict.conf")
echo v3 >"$DIR/conflict.conf"; settle
osreflog undo "$ID"
osreflog undo "$ID" --apply >/dev/null 2>&1
check '[ "$(cat $DIR/conflict.conf)" = v3 ]' "conflict left alone: v3 kept (#$ID)"

echo; echo "8. undo of an undo"
undo "$UNDO1" >/dev/null
check 'grep -q "mode = new" $DIR/sed.conf' "undoing #$UNDO1 puts the sed change back"

echo; echo "9. refusals and delegations"
echo "not a real key" >"$DIR/test.key"; settle
ID=$(cause_of "$DIR/test.key")
check 'osreflog undo $ID --json | grep -q "a secret"' "a secret path is refused (#$ID)"
dnf install -y -q --setopt=install_weak_deps=False figlet >/dev/null 2>&1; settle
ID=$(osreflog causes -n 10 --json | python3 -I -c 'import json,sys; print(next(c["id"] for c in json.load(sys.stdin) if c["kind"]=="dnf"))')
TX=$(dnf5 history list 2>/dev/null | awk 'NR>1 && $1 ~ /^[0-9]+$/ {print $1; exit}')
osreflog undo "$ID"
check 'osreflog undo $ID --json | grep -q "\"transaction\": *$TX\|\"transaction\":$TX"' "dnf cause delegated to dnf5 history undo $TX (#$ID)"
dnf5 history undo -y -q "$TX" >/dev/null 2>&1
check '! rpm -q figlet >/dev/null' "and dnf5 history undo $TX removed figlet"

echo; echo "cleanup"
cleanup
echo
osreflog verify | tail -1
if [ "$FAILED" -eq 0 ]; then echo "undo workload: all ok"; else echo "undo workload: $FAILED failed"; fi
exit "$FAILED"
