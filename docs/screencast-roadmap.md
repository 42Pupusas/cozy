# Roadmap: native ScreenCast

Status: **roadmap, not scheduled.** Nothing here is started.

Parent documents: `PLAN.md` (workspace goal and rules) and
`docs/portal-scoping.md` (portal internals).

## 1. Goal

Provide `org.freedesktop.impl.portal.ScreenCast` from this workspace, with
cozy producing the PipeWire stream itself.

Motivation: independence from other projects' code, control of the stack, and
learning. Screen sharing for video calls is the practical use.

## 2. Constraints

- Pure Rust, no FFI, no `libpipewire`, no bindgen.
- No async runtime.
- Every step is its own crate with unit tests, and ends in a check against
  this machine.

## 3. Why not niri's Mutter API

niri implements `org.gnome.Mutter.ScreenCast` and creates the PipeWire node
itself. A backend that calls it is small: `CreateSession`, `RecordMonitor`,
`RecordWindow`, `Start`, and the `PipeWireStreamAdded` signal. It is verified
present on this machine (`busctl --user list`).

It is rejected as the end state because cozy would not produce the stream.
It remains a valid first implementation behind the `CaptureSource` trait in
section 7.

## 4. What native capture costs

| Piece | Source of truth | Repo state |
|---|---|---|
| SPA POD encode and decode | PipeWire docs, `spa/pod` headers | none |
| PipeWire native protocol | `docs.pipewire.org/page_native_protocol.html` | none |
| fd passing | `crates/scm-rights` | exists |
| `ClientNode` and activation record | PipeWire source for the installed version | none |
| Output capture | `crates/screencopy` (`zwlr_screencopy`) | exists |
| Window capture | `ext-image-copy-capture-v1`, `ext-foreign-toplevel-list-v1` | none |
| Wayland wire code | `crates/platform-wire` | exists |
| Portal interface | `org.freedesktop.impl.portal.ScreenCast` v6 | none |
| Source picker | `crates/portal-ui` | none |

## 5. Build order

1. **`spa-pod`.** Encoder and decoder for Int, Long, Float, Double, Bool, Id,
   String, Bytes, Fd, Rectangle, Fraction, Array, Struct, Object, Choice.
   Pure byte logic. Golden-byte tests.
2. **`pw-native`.** Message header, framing, sequence numbers, fd transport,
   and the Core, Registry and Client objects.
   Check: connect to `pipewire-0`, send `Hello`, list the registry, compare
   with `pw-dump`.
3. **`pw-client-node`.** Create a node through the `client-node` factory.
   Handle `Transport`, `SetIO`, `SetActivation`, `PortSetParam`.
   Check: an output node with a video format appears in `pw-dump`.
4. **Buffers and the data path.** memfd-backed buffers announced through
   `UseBuffers` and `PortBuffers`. Wake on the transport eventfd, fill a
   buffer, set the IO status, signal the peer eventfd.
   Check: a synthetic test pattern that OBS or a browser plays.
5. **Capture.** `ext-image-copy-capture-v1` and
   `ext-foreign-toplevel-list-v1`, feeding step 4. `zwlr_screencopy` covers
   outputs until then.
   Check: the real screen as the stream.
6. **Portal wiring.** `ScreenCast` in `bins/portal`, the picker, session
   lifetime, `pipewire-serial`.

## 6. Risks

| Risk | Detail |
|---|---|
| Activation record layout | Shared-memory C struct. The docs call it "not yet ABI stable". Match it field by field against the installed PipeWire source. A wrong offset hangs the graph. |
| Version dependence | Pin to the installed PipeWire and test against it. |
| Consumer compatibility | Unverified: whether Chromium and Zoom accept memfd buffers without DMA-BUF. |
| Session manager permissions | Unverified: whether wireplumber lets the portal's remote connection see a node cozy created. |
| Lost niri features | `block-out-from "screencast"`, `is-window-cast-target` and the Dynamic Cast Target act only on niri's own cast path. `block-out-from "screen-capture"` still applies to protocol capture. |
| CPU cost | Shared-memory capture copies every frame. Unmeasured. DMA-BUF is a later optimization. |

## 7. Structure that keeps both options

`portal-core` defines a `CaptureSource` trait. Two implementations:

- `MutterCastSource`, calling niri's API. Small, works today.
- `NativeCastSource`, steps 1 to 5 above.

Session handling, the picker and routing are written once against the trait.
Native replaces Mutter without touching them.

## 8. Open checks before step 1

- Which capture protocols niri advertises. `wayland-info` is not installed, so
  write a small Rust example that lists registry globals through `platform`.
- That `pw-dump` is available for the step 2 and 3 checks.
- The installed PipeWire version, and where its headers or source can be read.

## 9. Interim state

Screen sharing is broken until one of these lands: this roadmap, the Mutter
implementation of step 7, or a third-party backend. `portals.conf` still names
`wlr` for `ScreenCast` and `Screenshot`, and `xdg-desktop-portal-wlr` is not
installed.
