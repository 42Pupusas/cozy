#!/bin/sh
# Wide-glyph stress: floods the screen with double-width characters,
# exercising text_layout glyph rasterization + atlas growth and the
# Cell::wide/spacer double-width path in crates/grid.
#
# Uses wide-presentation emoji rather than CJK ideographs: this terminal only
# ships a Latin-mono primary font + an emoji fallback by default (see the
# `[font]` docs in the config, and `scripts/stress/README.md`'s
# glyph_render_check section) -- no CJK-covering font is wired up, so CJK
# text here would just paint .notdef/tofu boxes instead of actually
# exercising rasterization. Emoji are double-width per UAX #11 (see
# text_layout::CellWidth::is_wide) and are guaranteed covered as long as
# `font.emoji_path` is set, so this still stresses the same wide-cell path
# while always rendering real ink.
#
# Falls back to a hardcoded emoji string if `python3` isn't available.
#
# Run inside the terminal window you want to stress. Ctrl+C to stop.
set -eu
LINES="${1:-50000}"

if command -v python3 >/dev/null 2>&1; then
    python3 -c "
import sys
lines = $LINES
line = '😀😁😂🤣😃😄😅😆😉😊' * 4
for _ in range(lines):
    sys.stdout.write(line + '\n')
"
else
    line="😀😁😂🤣😃😄😅😆😉😊😀😁😂🤣😃😄😅😆😉😊😀😁😂🤣😃😄😅😆😉😊"
    i=0
    while [ "$i" -lt "$LINES" ]; do
        echo "$line"
        i=$((i + 1))
    done
fi
