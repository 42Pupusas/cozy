#!/bin/sh
# Resize storm: rapidly resizes a target window, stressing
# TermWindow::resize (crates/platform/src/window.rs), which tears down and
# recreates the whole shm pool + both frame buffers on every resize, plus
# grid reflow in the parser thread.
#
# Needs `wmctrl` or `xdotool` (X11/XWayland) -- most Wayland compositors
# don't expose programmatic resize for arbitrary windows, so on pure Wayland
# you may need to just drag-resize the window by hand instead.
#
# Usage: run from a DIFFERENT terminal than the one being stressed.
#   ./resize_storm.sh "terminal"     # match by window title/class
set -eu
TITLE="${1:-terminal}"
ITERATIONS="${2:-200}"

if command -v xdotool >/dev/null 2>&1; then
    win=$(xdotool search --name "$TITLE" | head -n1)
    if [ -z "$win" ]; then
        echo "resize_storm: no window matching '$TITLE' found" >&2
        exit 1
    fi
    i=0
    while [ "$i" -lt "$ITERATIONS" ]; do
        w=$(( 400 + (i % 5) * 100 ))
        h=$(( 300 + (i % 3) * 80 ))
        xdotool windowsize "$win" "$w" "$h"
        i=$((i + 1))
    done
elif command -v wmctrl >/dev/null 2>&1; then
    echo "resize_storm: wmctrl found but per-frame resize needs xdotool for speed; install xdotool" >&2
    exit 1
else
    echo "resize_storm: needs xdotool (X11/XWayland only). On pure Wayland, drag-resize by hand instead." >&2
    exit 1
fi
