#!/bin/sh
# Raw throughput / scroll stress: floods stdout as fast as possible.
# Stresses vt_parser line handling + grid scroll + the parser->ui
# DirtySnapshot ring (RING_DIRTY = 16 in bins/terminal/src/main.rs).
#
# Run inside the terminal window you want to stress. Ctrl+C to stop.
set -eu
COUNT="${1:-2000000}"
seq 1 "$COUNT"
