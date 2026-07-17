#!/bin/sh
# Dense full-screen redraw stress: every line touches every column with
# alternating SGR colors, forcing a full-row blit_cell pass every line.
# Stresses crates/platform/src/render.rs's blit_snapshot/blit_cell path.
#
# Run inside the terminal window you want to stress. Ctrl+C to stop.
set -eu
COLS="${COLUMNS:-80}"
LINES="${1:-100000}"

i=0
while [ "$i" -lt "$LINES" ]; do
    c=$(( (i % 6) + 41 ))
    printf '\033[%sm' "$c"
    awk -v n="$COLS" 'BEGIN { for (i = 0; i < n; i++) printf "%d", i % 10; print "" }'
    i=$((i + 1))
done
printf '\033[0m\n'
