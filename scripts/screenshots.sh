#!/bin/bash
# Captures the README's screenshots of osreflog-app from this machine's real history, in light
# and dark, at twice the window's size: docs/images/screenshots/<name>-{light,dark}.png.
#
#   scripts/screenshots.sh            # all of them
#   scripts/screenshots.sh cause      # one
#
# Runs the app on GTK's Broadway backend (an off-screen display), so it needs no desktop and
# works over ssh. osreflogd must be running; no root. The pages to open are picked below: when
# the history moves on, pick causes and checkpoints that still tell the story.
set -euo pipefail
cd "$(dirname "$0")/.."

out=docs/images/screenshots
display=:9
declare -A pages=(
	[cause]="search:p8c,cause:774"
	[undo]="search:p8c,undo:775"
	[since]="since:c000136"
	[bisect]="bisect:session"
)
# How long each page takes to be ready (a diff to now reads two snapshots).
declare -A wait=([since]=10)

cargo build -q -p osreflog-app
mkdir -p "$out"
gtk4-broadwayd "$display" >/dev/null 2>&1 &
broadway=$!
trap 'kill $broadway 2>/dev/null' EXIT
sleep 1

names=("$@")
[ ${#names[@]} -gt 0 ] || names=(cause undo since bisect)
for name in "${names[@]}"; do
	open=${pages[$name]:?unknown screenshot: $name}
	for scheme in light dark; do
		file="$out/$name-$scheme.png"
		GDK_BACKEND=broadway BROADWAY_DISPLAY=$display \
			ADW_DEBUG_COLOR_SCHEME=prefer-$scheme \
			OSREFLOG_APP_OPEN="$open" \
			OSREFLOG_APP_SCREENSHOT="$file" \
			OSREFLOG_APP_SCREENSHOT_AFTER="${wait[$name]:-4}" \
			OSREFLOG_APP_SCREENSHOT_SCALE=2 \
			timeout 60 target/debug/osreflog-app
		echo "$file"
	done
done
