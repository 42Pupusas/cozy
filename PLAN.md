# Plan

Status: **rewritten 2026-07-30**, to match the workspace as it is.

The first plan described one terminal emulator that did not exist yet. That
terminal exists. Eight more binaries exist beside it. This document therefore
changes subject.

The work runs across several agents at once. Section 4 partitions the
workspace by file ownership, section 5 details each track, and section 8 is
the contract every agent follows. Read section 8 first.

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

## 4. How we parallelize

The work splits across agents. Agents do not coordinate. Therefore the plan
assigns **files**, not tasks.

### 4.1 The ownership rule

Each track below lists the paths it owns. An agent writes only inside its own
paths.

If an agent needs a change outside its paths, it must stop and report the
need. It must not make the change. Two agents editing one file produces a
conflict that neither can see.

### 4.2 Sole-owner files

These files carry cross-cutting change. Exactly one track owns each.

| Path | Owner | Reason |
|---|---|---|
| `crates/dbus/**` | Track 1 | Gate 1 adds a `Value::UnixFd` variant. That breaks every exhaustive match in the crate. |
| `crates/dbus-server/**` | Track 1 | It re-exports `dbus` types, so it moves with them. |
| `bins/portal/**` | Track 2 | The daemon wires every interface together. |
| `crates/platform/**` | nobody | Frozen for this plan. See 4.5. |

### 4.3 The prerequisites

Two extractions unblock several tracks at once. Do them first, alone, and land
them before the tracks that depend on them start.

**P1 — extract `crates/opener`.**
`bins/files/src/handlers.rs` holds `Handlers`, the association-resolution
policy. `bins/files/src/launch.rs` holds `Opener`. Both are pure logic over
`mime` and `launcher`, and both are trapped inside a binary.

`bins/open` needs them. The portal's `OpenURI` needs them. Move both files to
a new `crates/opener`, and leave the `xdg-open` fallback exactly where it is
for now. `Opener` has one call site (`bins/files/src/main.rs:155`), so this is
a small, mechanical change.

**P2 — add `Value::UnixFd`.**
The first half of Gate 1. Add the variant, the `h` signature character, and
the marshal and unmarshal arms. Do not add socket transport yet. This lands
the breaking change to `crates/dbus` early, so Track 1 stops blocking anybody.

### 4.4 The tracks

After P1 and P2 land, these run **concurrently**.

| Track | Owns | Depends on | Blocked by |
|---|---|---|---|
| 1. Bus transport | `crates/dbus/**`, `crates/dbus-server/**` | P2 | — |
| 2. Portal daemon | `bins/portal/**`, `crates/portal-core/**` | — | Track 1, for FD methods only |
| 3. Dialog shell | `crates/portal-ui/**` (new) | — | — |
| 4. xdg CLIs | `bins/open/**`, `bins/xdg-*/**` (new) | P1 | — |
| 5. Write paths | `crates/mime/**`, `crates/fsmodel/user_dirs` | — | — |
| 6. Rollout | `docs/**`, `bins/portal/data/**` | — | — |
| 7. Terminal | `crates/vt_parser/**`, `crates/font/**`, `bins/terminal/**` | — | — |

Track 7 shares nothing with tracks 1 to 6. It runs at any time.

### 4.5 Frozen paths

Do not restructure `crates/platform` during this plan. Six binaries depend on
it. A refactor there stalls every track at once.

The `TermWindow` and `Toplevel` convergence stays deferred. `docs/` records
why: `TermWindow` tracks damage per cell row, and the generic painter repaints
the whole window. The naive port is a performance regression.

---

## 5. Track detail

Each numbered item is one agent's assignment. Each must build, test, and lint
clean on its own.

### Track 1 — bus transport

Owns `crates/dbus` and `crates/dbus-server`.

1. **`dbus::fds::FdPassing`.** Send and receive `SCM_RIGHTS` over the bus
   socket. Add the `UNIX_FDS` header field. Encode and decode the `h` index
   into the descriptor array.
   Test with a socket pair. Cover several descriptors in one message. Cover
   cleanup after an error.
   Needs no compositor and no bus daemon.
2. **Descriptor lifetime.** Close received descriptors when a message drops.
   Prove it: open many, drop them, and count the process's open descriptors.

### Track 2 — portal daemon

Owns `bins/portal` and `crates/portal-core`.

3. **`portal-core` handles.** Add `request_handle.rs`, `session_handle.rs`,
   and `response_code.rs`. These are pure domain types: object paths, token
   validation, the `Closed` rules, and the response codes.
   This needs no bus and no UI. It starts immediately.
4. **`options/`.** Per-interface option-dictionary validation, one module per
   interface.
5. **The unified pump.** `PortalLoop` polls the bus socket and an inotify
   watch today. Add the Wayland connection. The daemon must answer new calls
   while a dialog is open, and must close requests when a caller loses its
   name. `dbus_server::NameWatcher` already reports that event.

### Track 3 — dialog shell

Owns a new `crates/portal-ui`. Depends on `platform` and `widget`, and changes
neither.

6. **`dialog_shell.rs`.** The modal Wayland surface and its focus rules.
   Accept unparented overlays. Add `xdg_foreign` parent handles only after the
   shell is reliable.
7. **`access_dialog.rs`.** A yes/no prompt. The smallest real dialog, so it
   proves the shell.
8. **`file_chooser.rs`.** An adapter over `fsmodel`, `widget`, `fileops`, and
   `mime`. Open, save, multiple selection, filters, and cancellation.

This track builds against a test harness, not against the daemon. Track 2
wires it in later.

### Track 4 — xdg command-line tools

Owns new binaries. Each is a thin shell over a crate that already works.

9. **`bins/open`.** The `xdg-open` replacement, over `crates/opener`.
10. **Delete the shell-outs.** Remove the fallback from `crates/opener`.
    Remove the `xdg-open` call in `bins/terminal`'s link handler.
    Do this only after step 9 proves itself in daily use.
11. **`bins/xdg-mime`.** Query and set the default application.
    Needs Track 5's write path.
12. **`bins/xdg-user-dirs`.** Read, write, and the `-update` behaviour.
13. **`bins/xdg-settings`.** The default browser and URL scheme handlers.
14. **`bins/xdg-desktop-menu` and `bins/xdg-icon-resource`.** Install and
    uninstall `.desktop` entries and icon files.

The end state removes the `xdg-utils` package from the machine.

### Track 5 — write paths

Owns the write half of two read-only crates.

15. **`mime` write path.** `crates/mime` answers queries today. Add setting a
    default application, which means writing `mimeapps.list`.
16. **`user_dirs` write path.** `bins/files/src/user_dirs.rs` reads
    `user-dirs.dirs`. Move it to a crate and add the writer.

### Track 6 — rollout

Owns documentation and the portal's data files. Writes no logic.

17. **Route `Settings`.** Add one line to `portals.conf`:
    `org.freedesktop.impl.portal.Settings=tijaax`. Restart the session
    components. Prove the rollback once, by reverting that line.
    The code, the tests, and the artifacts are done. Until this step happens,
    none of that work carries load.
18. **The compatibility matrix.** Record, per interface: the advertised
    version, the real caller that exercises it, the options it accepts, and
    the fallback backend. `docs/portal-scoping.md` step 1 asked for this, and
    it does not exist.
19. **Route each new interface**, one at a time, as its track finishes.

### Track 7 — terminal

Owns the terminal's own crates. Independent of everything above.

20. **Sixel graphics.** `vt_parser` consumes DCS payloads and discards them.
21. **Kitty image protocol.**
22. **Ligatures.** This needs `GSUB` parsing in `crates/font`.
23. **Tabs and multiplexing.**

---

## 5b. Interface assembly

An interface needs pieces from several tracks. These are integration steps,
and Track 2 owns them. Each waits for its parts.

| Interface | Needs | Adapts |
|---|---|---|
| `Access` | Track 3 step 7 | — |
| `Screenshot` | Track 1, Track 3 step 6 | `bins/shot`, `screencopy` |
| `OpenURI`, `AppChooser` | P1, Track 3 step 6 | `opener`, `launcher` |
| `FileChooser` | Track 1, Track 3 step 8 | `bins/files` |

Add `Inhibit`, `Lockdown`, `Account`, `Email`, and `Print` only when a real
local caller proves a need. Keep the old route for everything else.

---

## 5c. Deferred

- **Notifications.** Scope this on its own. `statusbar`'s urgency segment
  reads niri's window state. It is not a D-Bus notification server. Do not
  quietly turn it into one.
- **Frontend ownership.** Consider owning `org.freedesktop.portal.Desktop`
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

Step 4 is the shared check. It is the one that catches another agent's
breakage. Run it, but do not repair what you find outside your own paths.
Report it instead.

---

## 8. The agent contract

One agent takes one track. Read this before you start.

### 8.1 Boundaries

- Write only inside your track's paths, as listed in section 4.4.
- Do not edit `crates/platform`. Section 4.5 explains why.
- Do not edit another track's crate, even to fix an obvious bug. Report it.
- Add a dependency edge only within your own crates. A new edge into a crate
  you do not own is a request, not a change.

### 8.2 When you are blocked

Stop. Report the block. Name the file, the reason, and the track that owns it.

Do not work around a block by copying the code you needed. A second copy of
someone else's logic is worse than waiting.

### 8.3 Landing work

- One commit per numbered step. Do not batch steps.
- Run section 7 before every commit.
- A step that adds a public type also adds its tests, in the same commit.

### 8.4 The house rules still apply

Section 2 is not advisory. In particular:

- No free functions. Every piece of logic belongs to a struct, an enum, or a
  trait implementation.
- One responsibility per file. A new responsibility means a new module file,
  not a new struct in an existing one.
- No prose comments. Comment only what the code cannot say.
- Feature-gated code gates the field, and gives call sites thin wrapper
  methods. Never scatter `#[cfg]` through logic.
