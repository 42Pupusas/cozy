#!/bin/sh
# Wide-glyph / CJK / emoji stress: floods the screen with double-width
# characters, exercising text_layout glyph rasterization + atlas growth and
# the Cell::wide/spacer double-width path in crates/grid.
#
# Falls back to a hardcoded CJK string if `python3` isn't available.
#
# Run inside the terminal window you want to stress. Ctrl+C to stop.
set -eu
LINES="${1:-50000}"

if command -v python3 >/dev/null 2>&1; then
    python3 -c "
import sys
lines = $LINES
line = '国际化测试字符宽度渲染' * 4
for _ in range(lines):
    sys.stdout.write(line + '\n')
"
else
    line="国际化测试字符宽度渲染国际化测试字符宽度渲染国际化测试字符宽度渲染"
    i=0
    while [ "$i" -lt "$LINES" ]; do
        echo "$line"
        i=$((i + 1))
    done
fi
