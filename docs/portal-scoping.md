# Plan: a selective, native portal backend

Status: **implementation plan** — refreshed 2026-07-29.

## 0. Decision and boundary

We will replace selected `xdg-desktop-portal` **backend** interfaces first by
shipping one native `org.freedesktop.impl.portal.*` daemon. The existing
`xdg-desktop-portal` frontend continues to own client identity, request/session
bookkeeping, permission policy, and routing while `portals.conf` routes only the
interfaces we have proved.

This is deliberately not an all-or-nothing replacement. Per-interface routing
makes a regression reversible by changing configuration and restarting the user
session; claiming `org.freedesktop.portal.Desktop` directly does not.

**Target:** parity for this machine's actual unsandboxed applications, rather
than a general-purpose, Flatpak-complete portal implementation.

**Explicit non-goals for this project:**

- `ScreenCast`, `RemoteDesktop`, and `Camera`: keep the existing backend (or
  compositor implementation) because they require PipeWire protocol support.
- `Secret`: keep gnome-keyring; a secret store is security-sensitive work with
  no portal payoff.
- `Documents`: do not implement unless Flatpak is actively used. It requires a
  FUSE document filesystem and only matters for sandboxed apps.
- `GlobalShortcuts`, `InputCapture`, `Usb`, and other niche interfaces: defer
  until a real local caller proves a need.

The end state can remove GTK-backed dialogs and most portal-related processes
from the session without pretending that PipeWire or a secret store are small
follow-up tasks.

## 1. Current assets and remaining gates

The workspace now has the reusable product pieces that were missing when the
original scoping note was written:

| Need | Workspace asset | State |
|---|---|---|
| File selection UI and model | `bins/files`, `fsmodel`, `widget` | Working; list and grid views exist |
| File operations | `fileops` | Working headless core |
| MIME/default-app resolution | `mime`, `launcher` | Working core |
| App selection and launching | `launcher` | Working |
| Screenshot selection | `screencopy`, `bins/shot` | Working |
| Wayland, canvas, input, text | `platform`, `font`, `xkb`, `input` | Working |
| D-Bus client and limited watcher support | `dbus` | Working, but not a portal server foundation |

The hard gate is still `crates/dbus`. It has generic **decoding** (`Value` /
`Unmarshaller`) and enough client/watcher support to own a name, receive
messages, and make blocking calls. It does **not** yet provide:

1. Generic outbound encoding for arbitrary portal signatures, notably `a{sv}`,
   arrays, structs, variants, byte arrays, and handles.
2. A reusable object registry, method router, standard error replies, or
   `org.freedesktop.DBus.Introspectable` support.
3. Server-side `Properties.GetAll` / `PropertiesChanged` support.
4. Unix FD transport (`SCM_RIGHTS`, `UNIX_FDS` header field, and `h` indexes).
5. A non-blocking event model which can service multiple bus callers while a
   Wayland dialog is open, plus request cleanup when the caller disconnects.

`request_name` and `recv_message` prove a useful low-level capability, but the
statusbar's hand-wired watcher must not be mistaken for the reusable portal
server layer above.

## 2. Architecture

Keep responsibilities narrow and reuse the headless file-browser crates instead
of cloning their policy into the portal daemon.

```text
crates/dbus/
  marshal.rs                 Generic Value -> wire encoder (done)
  fds.rs                     Unix-FD send/receive and header accounting

crates/dbus-server/          Depends on crates/dbus; re-exports what downstream
                             interface crates need from it (Value, Reply,
                             Marshaller/Unmarshaller) so they only take one
                             dependency, not two separately versioned ones.
  registry.rs                Object lifetime and lookup (done)
  router.rs                  Signature-checked method dispatch (done)
  introspection.rs           Introspectable XML responses
  properties.rs              GetAll and PropertiesChanged
  name_watcher.rs            Caller death -> request/session cleanup

crates/portal-core/
  request_handle.rs          Request paths and token validation
  session_handle.rs          Session lifecycle and Closed rules
  response_code.rs           Success/cancelled/error response semantics
  options/                   Per-interface option-dictionary validation

crates/portal-ui/
  dialog_shell.rs            Wayland modal-shell lifecycle and focus
  access_dialog.rs           Yes/no access prompt
  file_chooser.rs            Adapter over fsmodel/widget/fileops/mime

bins/portal/
  main.rs                    Thin wiring: bus + Wayland pump, routing, config
```

`portal-core` must be pure protocol/domain logic with exhaustive unit tests.
`portal-ui` owns presentation only. `bins/portal` owns no policy beyond wiring
and must remain thin.

The server-side object registry, method router, introspection, and properties
support live in a **separate `crates/dbus-server` crate** that depends on
`crates/dbus`, not behind a Cargo feature on `dbus` itself. `crates/dbus` stays
a lean client library -- exactly what `statusbar`'s `NetworkManager`/bluez/
logind wrappers need and nothing else -- and "does this binary link the portal
server machinery" becomes a visible `Cargo.toml` dependency edge instead of a
feature flag someone has to remember to check. `fds.rs` stays in `crates/dbus`
for now since fd passing is a transport-level concern any client or server
might eventually need; revisit if that stops being true.

## 3. Delivery sequence

Every numbered step is independently buildable, tested, clippy-clean, and
revertible before the following step begins.

### Foundation

1. **Inventory local requirements.** Record whether Flatpak is installed/in
   use, which portal interfaces local applications call, the installed niri
   screencast implementation, and the current `portals.conf` routing. Convert
   that into a checked-in compatibility matrix.
2. **Marshaller.** Add `dbus::marshal::Marshaller`, mirroring `Unmarshaller`.
   Start with all D-Bus primitive types, recursive containers, variants, and
   `a{sv}`. Golden-wire tests must round-trip through `Unmarshaller` and cover
   alignment at every nesting boundary.
3. **Server core.** Add `crates/dbus-server`'s registry, router, introspection,
   argument validation, standard D-Bus error returns, and Properties support.
   Refactor the statusbar watcher only if it reduces duplication without mixing
   its service policy into generic code.
4. **Scratch service.** Build a small test daemon on a scratch well-known name;
   exercise introspection, a method call, `GetAll`, malformed arguments, and a
   signal from an external D-Bus client before registering any portal interface.
5. **Settings backend.** Implement `impl.portal.Settings` backed by
   `crates/config`. Route only Settings via a `.portal` file and `portals.conf`.
   This validates the real frontend path without UI or FD complexity.

### Interactive and FD work

6. **FD passing.** Add `dbus::fds::FdPassing`, `UNIX_FDS`, and `h` value
   encoding/decoding with focused socket-pair tests. Do not expose an FD-returning
   portal method until these tests cover multiple descriptors and error cleanup.
7. **Unified event pump.** Introduce the daemon-owned bus/Wayland event pump.
   It must continue accepting/responding to calls while a dialog is visible and
   must close request/session objects when a caller loses its name.
8. **Portal UI shell.** Extract a focused modal dialog shell from the proven
   launcher/shot patterns. Initially accept unparented overlays; add
   `xdg_foreign` parent-handle support only after the shell is reliable.
9. **Access and Screenshot.** Implement `impl.portal.Access` and Screenshot
   (adapting `bins/shot`) as the first interactive interfaces.

### Application-facing interfaces

10. **AppChooser and OpenURI.** Reuse `launcher` and `mime`; prove default-app,
    cancellation, and multi-URI behavior against real GTK/Qt callers.
11. **FileChooser.** Adapt `fsmodel`, `widget`, `mime`, and `fileops` into
    `portal-ui`; support open/save, multiple selection, filters, choices, and
    robust cancellation before advertising a high interface version.
12. **Small interfaces.** Add only locally-needed minimal implementations for
    Inhibit, Lockdown, Account, Email, and Print; otherwise leave routing to the
    established backend.
13. **Notifications (optional).** Scope this separately. `statusbar` urgency is
    not a D-Bus notification server; do not silently turn it into one.

### Only after daily-use proof

14. **Evaluate frontend ownership.** Consider owning
    `org.freedesktop.portal.Desktop` only after every selected backend interface
    is exercised in daily use with a documented rollback path. It is optional,
    not a milestone required for success.

## 4. Compatibility and rollback contract

- Develop every interface first on a scratch bus name.
- Ship a `.portal` descriptor and route **one interface at a time** in
  `portals.conf`.
- Maintain a compatibility matrix containing interface, advertised version,
  caller/test client, known options, and fallback backend.
- Report only versions whose methods, properties, options, signals, and
  cancellation semantics have tests.
- Preserve the existing backend route for every unimplemented interface.
- A rollback is a configuration change, not code surgery: restore that single
  interface's prior provider and restart the portal/session components.

## 5. Verification gate for each change

Before declaring any increment complete:

1. Run focused unit/integration tests for the changed crate.
2. Run `cargo build` and `cargo test` for every crate touched (`dbus`,
   `dbus-server`, and any downstream crate), plus FD support once that lands.
3. Run clippy for the same crates.
4. Run workspace `cargo check --workspace --all-targets` and inspect the Cargo
   dependency graph for cycles and dead-state smells -- in particular confirm
   `dbus`'s own dependents (e.g. `statusbar`) never gain a transitive edge to
   `dbus-server`.
5. For a routed interface, validate against a real portal client and then make
   the per-interface rollback change once to prove recovery works.

## 6. First implementation task

Start with step 1's compatibility matrix and then step 2: a standalone,
well-tested `dbus::marshal::Marshaller`. No portal daemon, UI, or interface code
should be started before the marshaller can encode and round-trip the nested
values portal replies require.

## 7. Bottom line

The workspace is ready to begin a native portal **backend** project: the file
chooser's actual domain/UI dependencies are no longer hypothetical. The next
risk is concentrated in `dbus`, so we should retire that risk in small, generic,
well-tested infrastructure slices. A selective backend replacement is feasible,
useful, and safe to deploy; a complete PipeWire/FUSE/secret-store replacement is
intentionally outside this plan.
