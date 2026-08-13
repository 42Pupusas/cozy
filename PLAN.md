# Plan

Status: **rewritten 2026-07-30**, to match the workspace as it is.

The first plan described one terminal emulator that did not exist yet. That
terminal exists. Eight more binaries exist beside it. This document therefore
changes subject.

Sibling documents:

- `docs/portal-scoping.md` — the portal backend's own delivery plan. It stays
  authoritative for portal internals.
- `docs/filebrowser-scoping.md` — the file browser's scoping note.
- `DESIGN_GUIDE.md` — visual language.

---

## 1. Goal

Replace `xdg-desktop-portal` and the `xdg-utils` command-line tools with our
own implementations.

The desktop session must run on this workspace's binaries alone. No GTK portal
backend. No `xdg-open`. No shell-outs to other projects' tools.

Two constraints bound the goal:

- We replace only what a real local caller needs. We do not chase Flatpak
  completeness.
- We keep the rollback path. Each interface routes independently.

### Non-goals

| Area | Reason |
|---|---|
| `ScreenCast`, `RemoteDesktop`, `Camera` | These need PipeWire. |
| `Secret` | A secret store is security-sensitive work with no portal payoff. |
| `Documents` | This needs a FUSE filesystem. It only helps sandboxed apps. |
| GPU rendering | The software renderer is fast enough. See section 6. |

---

## 2. Rules

These rules did not change. They still hold.

- **Zero external dependencies.** Justify every crate. Prefer our own tooling
  crates. Only `divan` (in `benches/`) comes from crates.io today.
- **Rust only.** No C glue. No bindgen. No `build.rs` that calls system
  libraries.
- **No async runtimes.** Threads communicate over ring buffers. Blocking
  happens in the kernel, or on a ring's wait path.
- **Message passing, not shared state.** Use `io_uring` at the kernel
  boundary, and `quetzalcoatl` rings inside a process.
- **One responsibility per crate, one responsibility per file.**
- **No free functions.** Every piece of logic belongs to a struct, an enum, or
  a trait implementation.
- **Full clippy.** `pedantic`, `nursery`, `perf`, `style`. Doc lints are the
  only allowed exceptions. `undocumented_unsafe_blocks` is a warning, not a
  convention.
- **Prove each step.** Build, test, and lint every feature combination before
  you start the next step.

---

## 3. What exists

45 packages: 9 binaries, 32 libraries, 4 bench crates.

### 3.1 Binaries

| Binary | Replaces | State |
|---|---|---|
| `terminal` | alacritty, ghostty | Daily use |
| `statusbar` | waybar | Daily use. Owns the `StatusNotifierWatcher` tray |
| `launcher` | wofi, rofi | Daily use |
| `wallpaper` | swaybg | Daily use |
| `shot` | `slurp \| grim` | Daily use |
| `clipboard` | `wl-paste --watch cliphist` | Daily use |
| `files` | nautilus | Working |
| `monitor` | htop | Working |
| `portal` | xdg-desktop-portal-gtk | Settings interface only. Not routed |

### 3.2 Library layers

**Layer 0 — no intra-workspace dependencies.**
`font`, `input`, `text_layout`, `xkb`, `waker`, `platform-wire`,
`platform-sys`, `localtime`, `diskstat`, `tlog`, `widget`, `fsmodel`,
`launcher`, `mime`, `clipboard`, `fileops`, `inotify-events`,
`headless-stress`, `config`, `wallpaper`, `sysmon`.

**Layer 1 — platform.**
`platform-sys` + `platform-wire` + `platform-draw` compose into `platform`.
`platform` owns the Wayland connection, the shm swap chain, toplevel and
layer-shell surfaces, the cursor, the clipboard, and damage-tracked present.
Every graphical binary sits on it.

**Layer 2 — the D-Bus stack.**

```text
dbus              client: SASL, calls, Value, Unmarshaller, Marshaller, signals
 ├─ dbus-server   registry, router, introspection, properties, name_watcher,
 │  │             serve, dispatcher, server_loop
 │  └─ bins/portal   SettingsService, PortalLoop, ConfigWatcher
 ├─ portal-core   pure domain logic, no bus, no Wayland
 └─ statusbar     client only. It must never gain an edge to dbus-server
```

`dbus-server` is a separate crate, not a Cargo feature. The dependency edge
must stay visible in `Cargo.toml`.

### 3.3 Drift from the first plan

Record these, so nobody chases a crate that does not exist.

| First plan said | Reality |
|---|---|
| `crates/renderer` | Never created. Drawing lives in `platform-draw` and `platform`. |
| `crates/pty` uses `forkpty` | Correct, and it works. |
| GPU backend behind a feature flag | No EGL exists anywhere. Section 6 explains why. |
| `crates/text_layout` is a milestone | Done, and in use. |
| Milestones M0 to M9 | All done, except the GPU part of M6/M7. |

---

## 4. The three gates

Everything interactive in the portal waits behind these. Do them in order.

### Gate 1 — file descriptor passing

Add `dbus::fds::FdPassing`.

- `SCM_RIGHTS` send and receive over the bus socket.
- The `UNIX_FDS` header field.
- The `h` type: encode and decode an index into the descriptor array.

Test it with a socket pair. Cover several descriptors in one message. Cover
cleanup after an error. Do not expose any portal method that returns a
descriptor before these tests pass.

This gate blocks `Screenshot` and `FileChooser`, which are the two interfaces
that matter most. It needs no compositor. Do it first.

### Gate 2 — the unified event pump

`PortalLoop` polls two descriptors today: the bus socket and an inotify watch.
It must also drive a Wayland connection.

Requirements:

- The daemon answers new bus calls while a dialog is on screen.
- The daemon closes request and session objects when a caller loses its name.
  `dbus_server::NameWatcher` already reports that event.

### Gate 3 — the dialog shell

Add `crates/portal-ui`, and add the missing pieces of `portal-core`.

`portal-core` gains:

- `request_handle.rs` — request object paths and token validation.
- `session_handle.rs` — session lifetime and the `Closed` rules.
- `response_code.rs` — success, cancelled, and error semantics.
- `options/` — per-interface option-dictionary validation.

`portal-ui` gains:

- `dialog_shell.rs` — the modal Wayland surface, and its focus rules.
- `access_dialog.rs` — a yes/no prompt.
- `file_chooser.rs` — an adapter over `fsmodel`, `widget`, `fileops`, `mime`.

Accept unparented overlays at first. Add `xdg_foreign` parent handles only
after the shell is reliable.

---

## 5. Delivery sequence

Each step must build, test, and lint clean before the next step starts.

### Phase A — collect the finished work

1. **Route `Settings`.** Add one line to `portals.conf`:
   `org.freedesktop.impl.portal.Settings=tijaax`. Restart the session
   components. Prove the rollback once, by reverting that line.
   The code, the tests, and the on-disk artifacts are already done. Until this
   step happens, none of that work carries load.
2. **Write `bins/open`.** A real `xdg-open` replacement over `mime` and
   `launcher`. Both cores already work.
3. **Delete the shell-outs.** Remove the `xdg-open` fallback from
   `bins/files/src/launch.rs`. Remove the `xdg-open` call from
   `bins/terminal`'s link handler. Point both at `bins/open`'s core.
4. **Write the compatibility matrix.** Record, per interface: the advertised
   version, the real caller that exercises it, the options it accepts, and the
   backend it falls back to. `docs/portal-scoping.md` step 1 asked for this
   and it does not exist yet.

### Phase B — the gates

5. Gate 1: `dbus::fds::FdPassing`.
6. Gate 2: the unified event pump.
7. Gate 3: `portal-ui` and the rest of `portal-core`.

### Phase C — interfaces

Each interface routes on its own. Each one adapts a crate that already works.

8. **`Access`.** The first interactive interface. It only needs the yes/no
   dialog, so it proves the shell with the least surface.
9. **`Screenshot`.** Adapt `bins/shot` and `crates/screencopy`. Needs Gate 1.
10. **`AppChooser` and `OpenURI`.** Adapt `launcher` and `mime`. Prove the
    default-application path, cancellation, and multiple URIs against real GTK
    and Qt callers.
11. **`FileChooser`.** Adapt `bins/files`. Support open, save, multiple
    selection, filters, choices, and cancellation. Needs Gate 1. Advertise a
    high interface version only after the tests cover it.
12. **Small interfaces.** Add `Inhibit`, `Lockdown`, `Account`, `Email`, and
    `Print` only when a local caller proves a need. Keep the old route for
    everything else.

### Phase D — the remaining xdg utilities

Each one is a thin binary over a crate that already exists.

| Tool | Needs |
|---|---|
| `xdg-mime` | `crates/mime` covers queries. Add the write path that sets a default application. Then add a CLI. |
| `xdg-user-dirs` | `bins/files/src/user_dirs.rs` reads `user-dirs.dirs`. Add the writer and the `-update` behaviour. |
| `xdg-settings` | Get and set the default browser, and the default URL scheme handler. |
| `xdg-desktop-menu` | Install and uninstall `.desktop` entries. |
| `xdg-icon-resource` | Install and uninstall icon files. |

The end state removes the `xdg-utils` package from the machine.

### Phase E — only after daily use

13. **Notifications.** Scope this on its own. `statusbar`'s urgency segment
    reads niri's window state. It is not a D-Bus notification server. Do not
    quietly turn it into one.
14. **Frontend ownership.** Consider owning `org.freedesktop.portal.Desktop`
    itself only after every backend interface survives daily use. This is
    optional. It is not a measure of success.

---

## 6. Terminal work that remains

The terminal is finished for daily use. These items stay open, and none of
them blocks the desktop goal.

- **Sixel graphics.** `vt_parser` consumes DCS payloads and discards them.
- **Kitty image protocol.** Nothing exists.
- **Ligatures.** This needs `GSUB` table parsing in `crates/font`.
- **Tabs and multiplexing.** Nothing exists.
- **GPU rendering.** Deliberately deferred, not abandoned. The software
  renderer meets the frame budget, and the SIMD paths are already optimised in
  dev builds. Revisit only if a measurement shows a real problem.

---

## 7. Verification gate

Run these before you call any step complete.

1. Run the changed crate's own tests.
2. Run `cargo build` and `cargo test` for every crate you touched, and for
   every crate downstream of them.
3. Run clippy on the same set.
4. Run `cargo check --workspace --all-targets`.
5. Inspect `cargo graph --report`. Confirm `dbus`'s client-only dependents
   never gain an edge to `dbus-server`.
6. For a routed portal interface, test it against a real client. Then perform
   the rollback once, to prove recovery works.
