# Scoping: replacing `xdg-desktop-portal` (+ its tool ecosystem) with one workspace bin

Status: **scoping only** — no code written. Written 2026-07-28.

## 0. What we'd actually be replacing

`xdg-desktop-portal` on this machine is not one thing, it is a stack:

| Layer | Bus name | Binary here | What it does |
|---|---|---|---|
| Frontend | `org.freedesktop.portal.Desktop` | `/usr/lib/xdg-desktop-portal` | Object/session lifecycle, app-id detection, permission checks, routes to a backend |
| Backend | `org.freedesktop.impl.portal.*` | `xdg-desktop-portal-gtk`, `-gnome` | The actual UI (file dialogs, permission prompts) |
| Documents | `org.freedesktop.portal.Documents` | same frontend binary | A FUSE fs at `/run/user/$UID/doc` |
| Permission store | `org.freedesktop.impl.portal.PermissionStore` | separate | GVariant-backed permission DB |

Local config (`/usr/share/xdg-desktop-portal/niri-portals.conf`):

```
[preferred]
default=gnome;gtk;
org.freedesktop.impl.portal.Access=gtk;
org.freedesktop.impl.portal.Notification=gtk;
org.freedesktop.impl.portal.Secret=gnome-keyring;
```

So today: niri session → frontend → gnome/gtk backends. That's ~3 daemons plus GTK
pulled into the session just to draw a file dialog.

"Corresponding tools" I'm reading as the `xdg-utils` script pile: `xdg-open`,
`xdg-mime`, `xdg-settings`, `xdg-user-dir(s)`, `xdg-email`, `xdg-screensaver`.
Those are POSIX shell scripts; they're the *easy* part and share a mime/desktop-entry
core with the portal itself.

## 1. The strategic insight: two possible shapes

**Shape A — replace the backend only.** We own `org.freedesktop.impl.portal.*`,
ship a `.portal` file, and point `niri-portals.conf` at us per-interface. The real
frontend keeps doing lifecycle, app-id, permissions, and routing.

**Shape B — replace the whole frontend.** We own `org.freedesktop.portal.Desktop`
directly and skip the impl layer entirely (a single-user, non-Flatpak machine has
no meaningful permission boundary to enforce, so the frontend's main job is moot).

**Recommendation: start at A, migrate to B.** Reasons:

- `portals.conf` gives us **per-interface rollback**. Break `FileChooser`? Flip one
  line, no rebuild, session still works. Shape B is all-or-nothing from day one and
  a bug means "no app can open a file".
- The backend interfaces are the ones that need UI, which is the actual work. The
  frontend is bookkeeping we can absorb later once the UI is proven.
- We can ship value on week one (Settings, Screenshot) without touching anything else.

Shape B's payoff is real though — killing the frontend drops the permission store,
the app-id sandbox detection, and one daemon. Worth doing *after* the interfaces land.

## 2. What we already have (this is the good news)

| Need | Existing asset | State |
|---|---|---|
| D-Bus client, SASL EXTERNAL, io_uring transport | `crates/dbus` | Solid, in production via statusbar |
| D-Bus **server** side (own a name, answer calls, emit signals) | `crates/dbus` `request_name` / `send_reply` / `send_signal` / `recv_message` / `add_match` | **Already proven** by `statusbar::tray`'s `StatusNotifierWatcher` |
| Generic D-Bus decoding of arbitrary signatures | `dbus::value::Unmarshaller` | Recursive-descent, fully generic |
| Wayland connection, wire codec | `crates/platform` (`conn`, `wire`, `protocol`) | Mature, dual-ring io_uring |
| SCM_RIGHTS fd passing | `platform::sys::{OutgoingMessage, IncomingControl}` | Done — but for the *Wayland* socket |
| Layer-shell windows | `statusbar::bar_window`, `bins/launcher/launcher_window` | Two working implementations |
| Software canvas + text + font | `platform::draw::Canvas`, `statusbar::text`, `crates/font` | Working |
| Keyboard input / xkb | `crates/input`, `crates/xkb` | Working |
| `.desktop` parsing, app index, fuzzy search, `Exec=` spawn | `crates/launcher` | Working — this is 80% of AppChooser and xdg-open |
| Icon theme lookup | `launcher::icon_lookup` | hicolor + pixmaps, PNG only |
| Screen capture | `crates/screencopy` + `bins/shot` | **Full interactive region picker already exists** |
| Wallpaper setting | `crates/wallpaper` | Working |
| Idle inhibit / session control | `dbus::logind` | Partial (brightness); session object path resolution done |
| PNG encode/decode | `ixiptla-core` | Working |
| Config | `crates/config` (shelby) | Working |

That is a genuinely large head start. The portal is mostly "D-Bus object plumbing +
a file dialog", and we have industrial-grade versions of both halves' hard parts.

## 3. What's missing — the honest gap list

### 3.1 `crates/dbus` gaps (the real long pole, and it blocks everything)

The current crate is a **client** that grew just enough server support for one
specific watcher. Portal work needs it to become a proper bidirectional peer.

1. **Outbound value encoder.** `message::BodyArg` currently has exactly six variants:
   `Str`, `ObjectPath`, `U32`, `Bool`, `VariantBool`, `VariantU32`. Every portal
   method returns `a{sv}` — nested arrays, dicts, variants, struct, `ay`, `h`.
   We need a `Marshaller` that is the mirror of the existing `Unmarshaller`:
   generic over signature, recursive. **This is the single most important missing
   piece and everything else waits on it.** → new `crates/dbus/src/marshal.rs`.

2. **Unix fd passing on the bus socket.** `OpenFile`/`OpenPipe`/`ScreenCast` return
   file descriptors: header field `UNIX_FDS` (code 9, not in `message::field_code`)
   plus `SCM_RIGHTS` on `sendmsg`/`recvmsg`, and the `h` type in the body being an
   *index* into the fd array. The cmsg assembly logic exists in `platform::sys` but
   is Wayland-specific and not shared. → new `crates/dbus/src/fds.rs`, possibly
   lifting a shared cmsg struct out of `platform::sys`.

3. **Object registry + method dispatch.** Today `tray.rs` hand-matches
   `(path, interface, member)` in an if-chain. A portal serves dozens of methods
   across ~15 interfaces plus dynamically-created
   `/org/freedesktop/portal/desktop/request/<sender>/<token>` objects. Needs a real
   router. → new `crates/dbus/src/server/` module tree: `ObjectRegistry`,
   `MethodCallRouter`, `PropertyBag`.

4. **`org.freedesktop.DBus.Introspectable`.** GTK/Qt/GLib clients introspect before
   calling. Missing introspection = mysterious client-side failures.
   → `crates/dbus/src/server/introspection.rs`.

5. **`Properties.GetAll` server-side + `PropertiesChanged` emission.**

6. **`NameOwnerChanged` tracking.** When a client dies mid-dialog we must close its
   requests/sessions or we leak windows and objects forever. Requires a match rule
   plus a per-sender registry.

7. **Concurrency model.** `DbusConn` is strictly blocking-single-call. A portal must
   serve a second client while a file dialog is open, and must pump a Wayland event
   loop simultaneously. Both sockets are pollable fds and both already speak
   io_uring, so this should be **one unified io_uring loop over both fds** rather
   than threads — but that loop does not exist yet and is nontrivial design work.

8. **Signature-driven arg validation.** Rejecting malformed calls with a proper
   `org.freedesktop.DBus.Error.InvalidArgs` rather than desyncing.

Realistically items 1+2+3 alone are a substantial chunk of work before a single
portal method can return a useful value.

### 3.2 Missing domain logic

- **MIME resolution.** Nothing here parses `mimeapps.list`, `shared-mime-info`
  globs/magic, or `defaults.list`. Needed by `OpenURI`, `xdg-open`, `xdg-mime`.
  `crates/launcher`'s `DesktopEntry` gets us `MimeType=` but not the association
  database or the glob matcher. → new `crates/mime`.
- **A file browser model.** Directory listing, sorting, filter application
  (portal filters are `(sas)` glob/mime pairs), navigation, hidden-file rules,
  multi-select, save-mode name entry. Pure logic, headlessly testable.
  → new `crates/filebrowser`.
- **A modal dialog window abstraction.** We have two layer-shell windows, but both
  are bespoke. A portal dialog needs: focus/keyboard grab, Esc-to-cancel, parenting
  to the requesting window, scrolling list, text entry. → new `crates/portal-ui`,
  factored out of `launcher_window` + `shot`'s `selection_overlay`.
- **Notification server.** `statusbar::notifications` is *not* a notification server —
  it polls niri IPC for `is_urgent` windows. A real `org.freedesktop.Notifications`
  + `portal.Notification` implementation (popup surfaces, actions, timeouts, hints,
  image-data) is from-scratch work.
- **`parent_window` handles.** Portal clients pass `"wayland:<handle>"` from
  `xdg_foreign`. To parent modals correctly we'd need `zxdg_importer_v2` support in
  `crates/platform` (not currently in `protocol.rs`). Degradable: an unparented
  layer-shell overlay works, just less pretty.

### 3.3 The genuinely hard tier

- **`ScreenCast` / `RemoteDesktop` / `Camera` → PipeWire.** These return a PipeWire
  node id and require client and portal to share a PipeWire connection. There is
  **no PipeWire client anywhere in this workspace**, and PipeWire's native protocol
  (SPA pods, buffer/format negotiation, dmabuf handoff) is comparable in size to the
  Wayland client we already wrote — arguably larger, and far less well specified.
  **Recommendation: do not attempt.** Leave these three routed to the existing
  backend in `portals.conf`. Costs us nothing architecturally, since `portals.conf`
  is per-interface.
  - *To verify:* recent niri implements `impl.portal.ScreenCast` itself; if so the
    gnome backend may already be bypassed and this is even more clearly not our
    problem.
- **`Documents` (FUSE).** Only meaningful for Flatpak. If no Flatpak on this box →
  **skip entirely**. Confirm before committing.
- **`Secret`.** Delegated to gnome-keyring today. Reimplementing a secret store is a
  separate project with real security stakes. **Skip, keep delegating.**
- **`GlobalShortcuts`, `InputCapture`, `Usb`.** Niche; skip until something asks.

### 3.4 Cross-cutting risks

- **App identity is meaningless without a sandbox.** The real frontend derives app-id
  from `/.flatpak-info` or snap cgroups. Unsandboxed callers are all "unconfined".
  This is a *simplification* for us (no permission store needed) but means we cannot
  claim feature parity for a Flatpak-using machine.
- **Clients are picky about reported interface `version`.** GTK's
  `GtkFileChooserNative` and Qt's XDG platform theme branch on version properties.
  Report only versions we actually satisfy, and expect a debugging tail here.
- **Bricking risk.** Owning `org.freedesktop.portal.Desktop` badly = no app can open
  a file, screenshot, or check dark mode. Shape A + per-interface `portals.conf` is
  the mitigation; also develop against a scratch bus name first.

## 4. Proposed crate layout

Respecting the workspace rules (SRP, one file one responsibility, no free functions,
pure core / thin compositor+bus shell — the `crates/screencopy` and `crates/launcher`
pattern):

```
crates/dbus/src/marshal.rs        Marshaller — Value -> wire bytes (mirror of Unmarshaller)
crates/dbus/src/fds.rs            FdPassing — SCM_RIGHTS + UNIX_FDS header field
crates/dbus/src/server/registry.rs      ObjectRegistry
crates/dbus/src/server/router.rs        MethodCallRouter
crates/dbus/src/server/introspection.rs Introspection (XML generation)
crates/dbus/src/server/name_watcher.rs  NameWatcher (NameOwnerChanged -> cleanup)

crates/portal-core/               Pure protocol logic, zero I/O:
    request_handle.rs             RequestHandle — object path derivation, token rules
    session_handle.rs             SessionHandle — lifecycle, Closed emission rules
    response_code.rs              ResponseCode (0 success / 1 cancelled / 2 other)
    options/*.rs                  Per-interface option-dict parse + validate
    (this is where the bulk of the unit tests live)

crates/mime/                      mimeapps.list, glob/magic matching, default resolution
crates/filebrowser/               Directory model, sorting, filters, save-name logic
crates/portal-ui/                 Modal dialog shell, extracted from launcher_window
                                  + shot's selection_overlay
crates/notify/                    Notification server logic + popup surface

bins/portal/                      The daemon. Wiring only: one io_uring loop over
                                  the bus fd + the Wayland fd, routing to the above.
bins/xdgtools/                    Multicall: xdg-open / xdg-mime / xdg-settings /
                                  xdg-user-dir, all on crates/mime + crates/launcher.
```

`bins/portal` should stay as thin as `bins/shot` is — wiring, not logic.

## 5. Suggested order (each step independently shippable + revertible)

| # | Step | Blocks on | Size | Payoff |
|---|---|---|---|---|
| 1 | `dbus::marshal::Marshaller` + tests | — | M | Unblocks literally everything |
| 2 | `dbus::server::{ObjectRegistry, MethodCallRouter, Introspection}` | 1 | M | Ditto; also lets `tray.rs` drop its if-chain |
| 3 | `impl.portal.Settings` (dark mode / accent from `crates/config`) | 1,2 | **S** | Immediate visible win; kills a GTK dep for theming |
| 4 | `dbus::fds::FdPassing` (SCM_RIGHTS + `UNIX_FDS`) | 1 | M | Gate for every fd-returning method |
| 5 | Unified io_uring loop over bus fd + Wayland fd | — | M/L | Needed before any UI-bearing interface |
| 6 | `impl.portal.Screenshot` — wrap `bins/shot` | 4,5 | **S** | We already have the whole picker |
| 7 | `crates/portal-ui` extracted from launcher/shot | 5 | M | Shared dialog shell |
| 8 | `impl.portal.Access` (yes/no modal) | 7 | S | Simplest UI interface, proves the shell |
| 9 | `crates/mime` + `bins/xdgtools` | — | M | Independent; replaces the shell scripts |
| 10 | `impl.portal.AppChooser` | 7,9 | S | It's `bins/launcher` with a filter |
| 11 | `impl.portal.OpenURI` | 9 | S | |
| 12 | `crates/filebrowser` (pure) | — | M | Independent, heavily testable |
| 13 | `impl.portal.FileChooser` | 4,7,12 | **L** | The one that matters most to real apps |
| 14 | `Inhibit`/`Lockdown`/`Account`/`Email`/`Print` (stub or minimal) | 2 | S each | Long tail |
| 15 | `crates/notify` + `Notification` | 7 | L | Also replaces any separate notif daemon |
| 16 | Shape A → B: own `portal.Desktop`, drop the impl layer | 1–14 | L | Kills the last daemon |
| — | `ScreenCast`/`RemoteDesktop`/`Camera` | PipeWire client | XL | **Out of scope — keep delegating** |
| — | `Documents` (FUSE), `Secret` | — | — | **Out of scope** |

Steps 1, 2, 9, 12 have no Wayland dependency at all and are pure-logic +
fully unit-testable — good places to start, and they parallelize cleanly.

## 6. Open questions to settle before starting

1. Is Flatpak in use on this machine? If no → `Documents`, the permission store, and
   app-id detection all vanish from scope, which is a large simplification.
2. Does the installed niri already implement `impl.portal.ScreenCast` natively?
   If yes, screencast never touches our code path and delegation is free.
3. Do we want a notification daemon at all, or is `statusbar`'s niri-IPC urgency
   indicator sufficient? Step 15 is large and might simply be unwanted.
4. Should `crates/dbus`'s server half be a separate crate (`crates/dbus-server`) to
   keep `crates/dbus` lean for the statusbar, or a feature-gated module? Workspace
   convention favours gating the *field* + thin wrapper methods, so a `server`
   feature on `crates/dbus` is probably right — but check `cargo graph` for cycle
   smells either way.
5. Target: parity with what *this* machine's apps actually call, or general
   correctness? Parity is maybe 30% of the work. Recommend parity.

## 7. Bottom line

- **Feasible and unusually well-supported by what's already here** — the Wayland
  client, canvas, font, input, screencopy, desktop-entry parsing, and a working
  D-Bus server are all in place.
- **The gating work is not the portal, it's `crates/dbus`.** A generic outbound
  marshaller, fd passing, and object dispatch are prerequisites for everything and
  don't exist yet. Budget for that first; don't start UI work until it's solid.
- **Full replacement is not realistic** because of PipeWire. Aim for "replace
  everything except ScreenCast/RemoteDesktop/Camera/Secret", which is genuinely
  achievable and would still let us drop the GTK backend entirely.
- **Do it as a backend first.** Per-interface `portals.conf` routing turns a
  potentially session-bricking rewrite into a sequence of safe, reversible steps.
