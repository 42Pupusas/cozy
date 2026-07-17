#!/bin/sh
# Generates a large text blob and copies it to the system clipboard, so you
# can paste it into the terminal (Ctrl+Shift+V) to stress the bracketed-paste
# input path: TermWindow's clipboard offer -> InputEvent::Paste ->
# bins/terminal/src/main.rs's InputMsg ring -> pty write.
#
# Needs wl-copy (wl-clipboard) on Wayland.
set -eu
LINES="${1:-100000}"

tmp=$(mktemp)
trap 'rm -f "$tmp"' EXIT
seq 1 "$LINES" > "$tmp"

if command -v wl-copy >/dev/null 2>&1; then
    wl-copy < "$tmp"
    echo "paste_flood: $LINES lines copied to clipboard -- paste with Ctrl+Shift+V" >&2
elif command -v xclip >/dev/null 2>&1; then
    xclip -selection clipboard < "$tmp"
    echo "paste_flood: $LINES lines copied to clipboard (xclip) -- paste with Ctrl+Shift+V" >&2
else
    echo "paste_flood: needs wl-copy (wl-clipboard) or xclip" >&2
    exit 1
fi
