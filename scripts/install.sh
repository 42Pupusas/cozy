#!/bin/sh
# Install (or update) every workspace binary into cargo's bin directory.
#
# We use `cargo install --path` rather than a hand-rolled `cp` for two
# reasons. It is idempotent -- one command builds in release, drops the binary
# into the install root, and `--force` overwrites the previous version -- and,
# importantly, cargo installs by writing a temporary file and `rename(2)`-ing
# it into place. Renaming over a *running* executable is fine on Linux, while
# copying onto one fails with ETXTBSY ("Text file busy"). A plain `cp`-based
# installer therefore breaks precisely for the long-lived binaries here
# (statusbar, wallpaper, clipboard) that are always running when you update.
#
# Binaries installed (package name -> binary name where they differ):
#
#   terminal                   -- the Wayland terminal        (bins/terminal)
#   statusbar-bin  -> statusbar -- the niri status bar         (bins/statusbar)
#   monitor                    -- the system monitor          (bins/monitor)
#   launcher-bin   -> launcher  -- the app launcher            (bins/launcher)
#   clipboard-bin  -> clipboard -- the clipboard daemon        (bins/clipboard)
#   wallpaper-bin  -> wallpaper -- the wallpaper setter        (bins/wallpaper)
#   shot-bin       -> shot      -- the screenshot utility      (bins/shot)
#
# All seven matter to a niri session: ~/.config/niri/config.kdl spawns
# statusbar, wallpaper and clipboard at startup, and binds terminal, launcher,
# monitor and shot to keys. Every one of those references the install root by
# absolute path, so a binary missing from here is a silently broken keybind or
# a stale daemon -- which is why this installs the whole set rather than the
# four it used to cover.
#
# `launcher` additionally needs `terminal` on PATH: it shells out to a bare
# `terminal -e <cmd>` (or whatever $TERMINAL names) to run Terminal=true
# .desktop entries, so an out-of-date `terminal` makes that wrapping look
# broken even when crates/launcher's own code is current.
#
# The install root is cargo's own: $CARGO_HOME/bin, falling back to the
# ~/.cargo/bin default when CARGO_HOME is unset. On this machine CARGO_HOME is
# /var/cache/cuarentaydos/cargo, which is exactly the path niri's config
# spawns from -- so the default is already correct and no hardcoding is
# needed. Override with INSTALL_ROOT=/somewhere to install elsewhere (the
# binaries land in $INSTALL_ROOT/bin, matching cargo's own --root layout).
set -eu

# Resolve the repo root from this script's location so it works from anywhere.
repo_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$repo_root"

cargo_home=${CARGO_HOME:-$HOME/.cargo}
install_root=${INSTALL_ROOT:-$cargo_home}
bin_dir="$install_root/bin"

echo "Installing workspace binaries into $bin_dir"
echo

# Build everything once up front. `cargo install` would otherwise recompile
# per package; sharing one workspace build makes repeat runs near-instant and,
# more usefully, fails fast on a compile error before any binary is replaced.
echo ">> cargo build --release --workspace --bins"
cargo build --release --workspace --bins
echo

for pkg in bins/terminal bins/statusbar bins/monitor bins/launcher \
           bins/clipboard bins/wallpaper bins/shot; do
    echo ">> cargo install --path $pkg --force"
    cargo install --path "$pkg" --force --root "$install_root"
done

# Clean up leftovers from the era when binaries were deployed by copying:
# replacing a running executable with `cp` fails, so the old inode was moved
# aside to <name>.old first. Nothing launches these and they only confuse
# `ls`, so sweep them now that cargo's rename-based install makes the whole
# dance unnecessary.
stale=$(find "$bin_dir" -maxdepth 1 -name '*.old*' -type f 2>/dev/null || true)
if [ -n "$stale" ]; then
    echo
    echo "Removing stale copy-install leftovers:"
    echo "$stale" | sed 's/^/  /'
    echo "$stale" | xargs -r rm -f
fi

echo
echo "Installed into $bin_dir:"
for bin in terminal statusbar monitor launcher clipboard wallpaper shot; do
    if [ -x "$bin_dir/$bin" ]; then
        echo "  ok      $bin"
    else
        echo "  MISSING $bin"
    fi
done

# Long-lived binaries keep running the code they started with: cargo's rename
# leaves the old inode alive for any existing process. Say so explicitly --
# "I installed the fix but nothing changed" is otherwise an easy trap.
echo
echo "Note: running processes still use the previous build until restarted"
echo "      (statusbar, wallpaper, clipboard are spawned at niri startup)."
