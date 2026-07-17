#!/bin/sh
# Resize storm: rapidly resizes a target window, stressing
# TermWindow::resize (crates/platform/src/window.rs). The pool is now grown
# in place (ftruncate + mremap + wl_shm_pool.resize) and shrunk lazily rather
# than torn down every frame, so this script is the way to confirm a resize
# flood no longer thrashes the shm pool (plus the grid reflow in the parser
# thread still runs each step).
#
# Backends, tried in order:
#   * niri      -> niri msg  (native Wayland)
#   * Hyprland  -> hyprctl   (native Wayland)
#   * Sway/wlroots -> swaymsg (native Wayland)
#   * X11/XWayland -> xdotool
# The window is matched by its Wayland app_id (TermWindow sets a fixed
# `forfun.terminal` app_id), which — unlike the title — the shell can't
# overwrite, so we never accidentally grab whatever terminal launched us. The
# X11/xdotool fallback still matches by title (XWayland has no app_id).
#
# Usage: run from a DIFFERENT terminal than the one being stressed.
#   ./resize_storm.sh                     # match the default app_id
#   ./resize_storm.sh forfun.terminal 500 # explicit app_id + iterations
set -eu
APP_ID="${1:-forfun.terminal}"
ITERATIONS="${2:-200}"
# xdotool has no app_id; on X11 we match the window title instead. Strip a
# leading `forfun.` so the default still finds a window titled "terminal".
TITLE="${APP_ID##*.}"

# The w/h sweep is shared by every backend: a small, cyclic set of sizes so the
# window keeps growing and shrinking across the pool's lazy-shrink threshold.
size_for() {
    # $1 = iteration index; sets globals W and H.
    W=$(( 400 + ($1 % 5) * 100 ))
    H=$(( 300 + ($1 % 3) * 80 ))
}

storm_niri() {
    # niri identifies windows by a numeric id; look it up by app_id from the
    # JSON window list. Prefer jq; fall back to a grep/sed scrape.
    if command -v jq >/dev/null 2>&1; then
        id=$(niri msg --json windows \
            | jq -r --arg a "$APP_ID" \
                'map(select(.app_id == $a)) | .[0].id // empty')
    else
        # Objects are on one JSON blob; split on braces and grep the pair.
        id=$(niri msg --json windows \
            | tr '}' '\n' \
            | grep -F "\"app_id\":\"$APP_ID\"" \
            | head -n1 \
            | grep -o '"id":[0-9]*' \
            | head -n1 \
            | cut -d: -f2)
    fi
    if [ -z "$id" ]; then
        echo "resize_storm: niri msg found no window with app_id '$APP_ID'" >&2
        exit 1
    fi
    # Float it so the scrollable-tiling layout doesn't clamp our sizes.
    niri msg action move-window-to-floating --id "$id" >/dev/null 2>&1 || true
    i=0
    while [ "$i" -lt "$ITERATIONS" ]; do
        size_for "$i"
        # SizeChange 'fixed': a bare pixel count sets an exact logical size.
        niri msg action set-window-width --id "$id" "$W" >/dev/null
        niri msg action set-window-height --id "$id" "$H" >/dev/null
        i=$((i + 1))
    done
}

storm_hyprland() {
    # Resolve the window address from the client list (JSON). Prefer jq; fall
    # back to a grep/sed scrape so the script still works without jq.
    if command -v jq >/dev/null 2>&1; then
        addr=$(hyprctl -j clients \
            | jq -r --arg a "$APP_ID" \
                'map(select(.class == $a)) | .[0].address // empty')
    else
        addr=$(hyprctl clients \
            | awk -v a="$APP_ID" '
                /^Window / { w = $2 }
                $1 == "class:" && $2 == a { print w; exit }')
        case "$addr" in Window*) addr=${addr#Window } ;; esac
    fi
    if [ -z "$addr" ]; then
        echo "resize_storm: hyprctl found no window with class (app_id) '$APP_ID'" >&2
        exit 1
    fi
    # Float it first so the tiling layout doesn't clamp our sizes.
    hyprctl dispatch setfloating "address:0x${addr#0x}" >/dev/null 2>&1 || true
    i=0
    while [ "$i" -lt "$ITERATIONS" ]; do
        size_for "$i"
        hyprctl dispatch resizewindowpixel \
            "exact $W $H,address:0x${addr#0x}" >/dev/null
        i=$((i + 1))
    done
}

storm_sway() {
    # Sway can only freely resize floating windows, so enable floating on the
    # match first, then drive `resize set` through the same criteria selector.
    # Sway calls the Wayland app_id `app_id` in its criteria language.
    if ! swaymsg -t get_tree | grep -q "\"app_id\": \"$APP_ID\""; then
        echo "resize_storm: swaymsg found no window with app_id '$APP_ID'" >&2
        exit 1
    fi
    swaymsg "[app_id=\"$APP_ID\"] floating enable" >/dev/null 2>&1 || true
    i=0
    while [ "$i" -lt "$ITERATIONS" ]; do
        size_for "$i"
        swaymsg "[app_id=\"$APP_ID\"] resize set width ${W}px height ${H}px" >/dev/null
        i=$((i + 1))
    done
}

storm_xdotool() {
    win=$(xdotool search --name "$TITLE" | head -n1)
    if [ -z "$win" ]; then
        echo "resize_storm: no window matching '$TITLE' found" >&2
        exit 1
    fi
    i=0
    while [ "$i" -lt "$ITERATIONS" ]; do
        size_for "$i"
        xdotool windowsize "$win" "$W" "$H"
        i=$((i + 1))
    done
}

# Prefer the native compositor control matching the running session, then fall
# back to X11/XWayland. On Wayland, NIRI_SOCKET / HYPRLAND_INSTANCE_SIGNATURE /
# SWAYSOCK tell us which compositor is live without guessing.
if [ -n "${NIRI_SOCKET:-}" ] && command -v niri >/dev/null 2>&1; then
    storm_niri
elif [ -n "${HYPRLAND_INSTANCE_SIGNATURE:-}" ] && command -v hyprctl >/dev/null 2>&1; then
    storm_hyprland
elif [ -n "${SWAYSOCK:-}" ] && command -v swaymsg >/dev/null 2>&1; then
    storm_sway
elif command -v niri >/dev/null 2>&1; then
    storm_niri
elif command -v hyprctl >/dev/null 2>&1; then
    storm_hyprland
elif command -v swaymsg >/dev/null 2>&1; then
    storm_sway
elif command -v xdotool >/dev/null 2>&1; then
    storm_xdotool
else
    echo "resize_storm: need niri (niri msg), hyprctl (Hyprland), swaymsg (Sway), or xdotool (X11/XWayland)." >&2
    echo "  On another Wayland compositor, drag-resize the window by hand instead." >&2
    exit 1
fi
