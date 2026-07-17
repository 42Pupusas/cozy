#!/bin/sh
# 24-bit truecolor gradient stress: every cell gets a distinct RGB SGR
# sequence (fg and bg), forcing per-cell Attrs churn and the RGB (non-palette)
# path in Theme::effective / render.rs.
#
# Run inside the terminal window you want to stress. Ctrl+C to stop.
set -eu
COLS="${COLUMNS:-80}"
ROWS="${1:-20000}"

r=0
while [ "$r" -lt "$ROWS" ]; do
    awk -v n="$COLS" -v row="$r" 'BEGIN {
        for (c = 0; c < n; c++) {
            fr = (c * 255 / n);
            fg_g = ((row * 3) % 255);
            fb = ((c + row) % 255);
            br = (255 - fr);
            bg_g = (row % 255);
            bb = (c % 255);
            printf "\033[38;2;%d;%d;%d;48;2;%d;%d;%dm#", fr, fg_g, fb, br, bg_g, bb;
        }
        print "\033[0m";
    }'
    r=$((r + 1))
done
