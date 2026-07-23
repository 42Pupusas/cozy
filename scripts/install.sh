#!/bin/sh
# Install (or update) the three workspace binaries into ~/.cargo/bin.
#
# We use `cargo install --path` rather than a hand-rolled `cp` so updates are
# a single idempotent command: it builds in release, drops the binary into
# CARGO_HOME/bin (default ~/.cargo/bin, already on PATH for any Rust user),
# and `--force` overwrites the previous version in place. Re-run this after a
# `git pull` to update everything.
#
#   terminal   -- the Wayland terminal      (bins/terminal)
#   statusbar  -- the niri status bar        (bins/statusbar, crate statusbar-bin)
#   monitor    -- the system monitor         (bins/monitor)
#
# niri launches `terminal` (Mod+T) and `statusbar` (spawn-at-startup) by their
# ~/.cargo/bin path -- see ~/.config/niri/config.kdl.
set -eu

# Resolve the repo root from this script's location so it works from anywhere.
repo_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$repo_root"

for pkg in bins/terminal bins/statusbar bins/monitor; do
    echo ">> cargo install --path $pkg --force"
    cargo install --path "$pkg" --force
done

echo
echo "Installed into ${CARGO_HOME:-$HOME/.cargo}/bin:"
cargo install --list | grep -E '^(terminal|statusbar|monitor) ' || true
