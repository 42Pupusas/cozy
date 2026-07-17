#!/bin/sh
# Alt-screen thrash: rapidly toggles the alternate screen buffer (smcup/rmcup,
# DECSET ?1049), exercising grid mode switches and forcing a full-screen
# dirty snapshot on every flip.
#
# Run inside the terminal window you want to stress. Ctrl+C to stop.
set -eu
ITERATIONS="${1:-5000}"

i=0
while [ "$i" -lt "$ITERATIONS" ]; do
    printf '\033[?1049h'   # enter alt screen
    printf '\033[H\033[2Jframe %d' "$i"
    printf '\033[?1049l'   # leave alt screen
    i=$((i + 1))
done
