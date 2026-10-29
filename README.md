# cozy

A Wayland desktop toolkit for Linux, written in Rust. It holds a terminal and
the small programs that surround one: a status bar, an app launcher, a
wallpaper setter, a screenshot tool, a clipboard manager, a file browser, a
system monitor, and a portal backend.

The goal is a session that runs on this workspace's binaries alone. The
Wayland protocol, D-Bus, the VT parser and the software renderer are written
in Rust in this workspace, with no C libraries and no GPU. `io_uring`, the
lock-free rings, and the JSON, TOML and PNG codecs are the author's own crates
on crates.io or GitHub. The plan and its rules are in [`PLAN.md`](PLAN.md).

## Programs

| Binary | What it is |
|---|---|
| `terminal` | The terminal emulator |
| `statusbar` | A layer-shell status bar with a tray, Wi-Fi, Bluetooth and volume panels |
| `launcher` | A keyboard-driven app launcher |
| `wallpaper` | Sets a PNG wallpaper on every output |
| `shot` | Screenshots through `zwlr_screencopy`; replaces `slurp \| grim` |
| `clipboard` | Clipboard history through `zwlr_data_control` |
| `files` | A file browser |
| `monitor` | A system monitor drawn as tiles |
| `portal` | An `org.freedesktop.impl.portal.*` backend; today only the Settings interface |
| `open` | An `xdg-open` replacement |
| `canvas` | An infinite-canvas diagram tool |
| `xdg-user-dir`, `xdg-user-dirs-update` | Replacements for the `xdg-user-dirs` tools |

## Requirements

- Linux with `io_uring`.
- A Wayland compositor. `statusbar`, `launcher`, `wallpaper`, `shot` and
  `clipboard` use wlroots protocols (layer shell, screencopy, data control),
  which niri provides. The author runs niri.
- Rust 1.89 or newer.

## Build and install

```sh
cargo build --release --workspace --bins
scripts/install.sh
```

`scripts/install.sh` installs the binaries into `$CARGO_HOME/bin`, or into
`$INSTALL_ROOT/bin` if you set `INSTALL_ROOT`.

The PNG decoder comes from [`ixiptla`](https://github.com/42Pupusas/ixiptla),
a git dependency. Everything else comes from crates.io.

## Configuration

The terminal reads `$XDG_CONFIG_HOME/terminal/config.toml`.

## Tests and benchmarks

```sh
cargo test --workspace
cargo bench -p tty-benches
```

How the terminal compares with Ghostty on `vtebench`, including the raw data
and the limits of that comparison, is in
[`docs/vtebench/README.md`](docs/vtebench/README.md).

## Layout

- `bins/` holds the programs above.
- `crates/` holds the libraries they share, one responsibility per crate.
- `benches/` holds the benchmark and delivery-test crates.
- `docs/` holds design notes and the `vtebench` results.

## License

MIT. See [`LICENSE`](LICENSE).
