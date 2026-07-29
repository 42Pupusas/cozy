//! A `zwlr_layer_shell_v1` launcher window: a small, unanchored (thus
//! compositor-centered), keyboard-exclusive overlay surface.
//!
//! Structurally this is `statusbar::BarWindow`'s sibling -- same
//! `platform::surface::SurfaceBootstrap` connection bootstrap and
//! `platform::present::PresentState` double-buffered shm swap chain -- but
//! with three differences a status bar has no use for:
//!   * **no anchor**: leaving every `Anchor` bit unset tells the compositor
//!     to center the surface on the output instead of docking it to an
//!     edge, which is what a launcher wants.
//!   * **`KeyboardInteractivity::Exclusive`**: a bar never takes keyboard
//!     focus; a launcher needs to grab it immediately so typing lands in
//!     the search box with no click required.
//!   * **real XKB keyboard input**: wired exactly like
//!     `platform::TermWindow` (`crate::xkb::Translator` fed from
//!     `wl_keyboard.keymap`/`modifiers`, `platform::key_repeat`-equivalent
//!     auto-repeat) -- a bar has no keyboard at all.

use platform::conn::{Sender, WindowSender};
use platform::draw::Canvas;
use platform::event::RawEvent;
use platform::id::{CompositorId, KeyboardId, LayerShellId, OutputId, RegistryId, SeatId, ShmId};
use platform::layer::{LayerSurfaceConfig, LayerSurfaceHandle};
use platform::present::{PresentAttempt, PresentState};
use platform::protocol::{
    self as proto, Format, Key, KeyState, KeyboardEvent, KeyboardInteractivity, Layer,
    LayerSurfaceEvent, SeatCapability, SeatEvent, SeatRequest,
};
// Two different notions of "key" meet in this file, so neither is left to
// be inferred from context: `proto::Key` is a *physical* evdev key (what
// the wire reports, what auto-repeat tracks), while `DecodedKey` is what
// XKB says that press *means* on the user's actual layout.
use platform::toplevel::keysym::Key as DecodedKey;
use platform::wire::MessageBuilder;
use platform::{Error, Result};
use quetzalcoatl::capacity::Capacity;
use quetzalcoatl::mpsc;
use quetzalcoatl::spsc::Consumer;

use platform::surface::{self, Frame};

const EVENT_RING_CAP: usize = 128;
const APP_ID: &str = "launcher";

/// One resolved keyboard event a frame's caller needs to react to: text
/// input, navigation, or a request to close/launch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LauncherKey {
    Char(char),
    Backspace,
    Enter,
    Esc,
    Up,
    Down,
    CtrlDigit(u8),
}

impl LauncherKey {
    /// Narrow a decoded [`Key`] to the subset a search box acts on.
    ///
    /// Deliberately total over the keys we care about and `None` for the
    /// rest: a launcher ignores F-keys, `Insert`, `PageUp` and friends rather
    /// than guessing at a meaning for them. `CtrlDigit` is absent here
    /// because it is a *chord*, resolved from modifier state before the
    /// keysym is decoded -- see `LauncherWindow::resolve_key`.
    #[must_use]
    pub const fn from_key(key: Option<DecodedKey>) -> Option<Self> {
        match key {
            Some(DecodedKey::Char(c)) => Some(Self::Char(c)),
            Some(DecodedKey::Backspace) => Some(Self::Backspace),
            Some(DecodedKey::Enter) => Some(Self::Enter),
            Some(DecodedKey::Escape) => Some(Self::Esc),
            Some(DecodedKey::Up) => Some(Self::Up),
            Some(DecodedKey::Down) => Some(Self::Down),
            _ => None,
        }
    }
}

/// A centered, keyboard-grabbing layer-shell overlay window.
pub struct LauncherWindow {
    sender: Sender,
    request_rx: mpsc::Consumer<platform::conn::WindowRequest>,
    window_tx: WindowSender,
    events: Consumer<RawEvent>,
    #[allow(dead_code)]
    reader: Option<std::thread::JoinHandle<Result<()>>>,
    /// Tracks whether the reader thread still exists. The launcher drains
    /// its ring with the non-blocking `pop()`, so without this an exited
    /// reader looks exactly like an idle one and the overlay would linger
    /// after the compositor went away.
    reader_health: platform::ReaderHealth,
    wake: waker::Waker,

    #[allow(dead_code)]
    compositor: CompositorId,
    #[allow(dead_code)]
    layer_shell: LayerShellId,
    layer: LayerSurfaceHandle,

    scale: i32,
    width: i32,
    height: i32,

    seat: SeatId,
    keyboard: Option<KeyboardId>,
    xkb: Option<xkb::Translator>,
    awaiting_keymap_fd: Option<usize>,
    key_repeat: platform::key_repeat::KeyRepeatState,
    /// What the currently-held key resolved to, so auto-repeat can re-emit
    /// it verbatim instead of re-deriving it from the physical keycode.
    /// Kept in lockstep with `key_repeat`'s held key.
    held_resolved: Option<LauncherKey>,

    present: PresentState,
    backbuffer: Vec<u32>,
    background: platform::draw::Color,
    /// Alpha applied to background pixels, `0-255` (`255` = opaque). Same
    /// role as `statusbar::BarWindow`'s `bg_alpha` -- fixed for the
    /// window's whole lifetime from the `opacity` passed to
    /// [`LauncherWindow::new`], since it determines which `wl_shm` format
    /// (`Argb8888` vs `Xrgb8888`) we negotiate at construction.
    bg_alpha: u8,

    pending_keys: Vec<LauncherKey>,
    configured: bool,
    closing: bool,
}

struct Globals {
    compositor: CompositorId,
    shm: ShmId,
    layer_shell: LayerShellId,
    seat: SeatId,
}

#[derive(Default)]
struct GlobalsBuilder {
    compositor: Option<CompositorId>,
    shm: Option<ShmId>,
    layer_shell: Option<LayerShellId>,
    seat: Option<SeatId>,
}

impl surface::GlobalCollector for GlobalsBuilder {
    fn is_complete(&self) -> bool {
        self.compositor.is_some()
            && self.shm.is_some()
            && self.layer_shell.is_some()
            && self.seat.is_some()
    }

    fn bind_global(
        &mut self,
        sender: &mut Sender,
        registry: RegistryId,
        name: u32,
        interface: &str,
        version: u32,
    ) -> Result<()> {
        match interface {
            s if s == proto::WL_COMPOSITOR.name => Self::bind_if_missing(
                &mut self.compositor,
                sender,
                registry,
                name,
                interface,
                version,
                proto::WL_COMPOSITOR.version,
            ),
            s if s == proto::WL_SHM.name => Self::bind_if_missing(
                &mut self.shm,
                sender,
                registry,
                name,
                interface,
                version,
                proto::WL_SHM.version,
            ),
            s if s == proto::ZWLR_LAYER_SHELL_V1.name => Self::bind_if_missing(
                &mut self.layer_shell,
                sender,
                registry,
                name,
                interface,
                version,
                proto::ZWLR_LAYER_SHELL_V1.version,
            ),
            s if s == proto::WL_SEAT.name => Self::bind_if_missing(
                &mut self.seat,
                sender,
                registry,
                name,
                interface,
                version,
                proto::WL_SEAT.version,
            ),
            _ => Ok(()),
        }
    }
}

impl GlobalsBuilder {
    fn finish(self) -> Result<Globals> {
        Ok(Globals {
            compositor: self.compositor.ok_or(Error::ReaderStopped)?,
            shm: self.shm.ok_or(Error::ReaderStopped)?,
            layer_shell: self.layer_shell.ok_or(Error::ReaderStopped)?,
            seat: self.seat.ok_or(Error::ReaderStopped)?,
        })
    }
}

impl Drop for LauncherWindow {
    fn drop(&mut self) {
        let _ = self.layer.destroy(&mut self.sender);
    }
}

impl LauncherWindow {
    /// Open a centered, keyboard-exclusive overlay `width`x`height` logical
    /// pixels (scaled to physical pixels by the output's buffer scale),
    /// with the given background `opacity` (`0.0` fully transparent ..
    /// `1.0` opaque -- mirrors `config::Config::launcher_opacity`, same
    /// convention as `statusbar::BarWindow::new`'s `opacity` parameter).
    pub fn new(width: i32, height: i32, opacity: f64, wake: waker::Waker) -> Result<Self> {
        let (mut sender, repeat_timer, mut events, reader_handle) =
            surface::SurfaceBootstrap::connect(EVENT_RING_CAP, "launcher-wayland-reader", &wake)?;

        let Globals {
            compositor,
            shm,
            layer_shell,
            seat,
        } = Self::discover(&mut sender, &mut events)?;

        // Unlike the bar (which probes `wl_output.scale`), the launcher
        // doesn't need a specific output up front -- it starts at scale 1
        // and self-corrects once the first `configure` reports otherwise
        // via `resize` (mirrors how `TermWindow` handles a runtime scale
        // change: there isn't one, this is just "assume 1 until proven
        // otherwise").
        let scale = 1;
        let px_w = width * scale;
        let px_h = height * scale;

        // Same deliberate, reviewed float->int cast as
        // `statusbar::BarWindow::new`'s identical conversion --
        // `.clamp(0.0, 1.0)` before multiplying guarantees the result is in
        // `0.0..=255.0`, so truncation/sign-loss can't actually happen.
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let bg_alpha = (opacity.clamp(0.0, 1.0) * 255.0).round() as u8;

        let (frames, shm_mem, pool) =
            Frame::create_pair(&mut sender, shm, px_w, px_h, Format::Argb8888)?;

        // No anchor: leaving every anchor bit unset means the compositor
        // centers the surface on the output, exactly the "type to search"
        // floating bar every desktop launcher uses.
        let layer = LayerSurfaceHandle::create(
            &mut sender,
            compositor,
            layer_shell,
            &LayerSurfaceConfig {
                layer: Layer::Overlay,
                namespace: APP_ID,
                output: OutputId(0),
                buffer_scale: Some(scale),
                anchor: None,
                size: Some((width, height)),
                exclusive_zone: None,
                margin: None,
                keyboard_interactivity: KeyboardInteractivity::Exclusive,
            },
        )?;

        let (req_tx, req_rx) =
            mpsc::RingBuffer::<platform::conn::WindowRequest>::new(Capacity::exact(64)).split();
        let (id_start, id_end) = sender.alloc_id_block(64);
        let window_tx = WindowSender {
            tx: req_tx,
            next_id: id_start,
            id_end,
            free_ids: Vec::new(),
        };

        Ok(Self {
            sender,
            request_rx: req_rx,
            window_tx,
            events,
            reader: Some(reader_handle),
            reader_health: platform::ReaderHealth::new(),
            wake,
            compositor,
            layer_shell,
            layer,
            scale,
            width: px_w,
            height: px_h,
            seat,
            keyboard: None,
            xkb: None,
            awaiting_keymap_fd: None,
            key_repeat: platform::key_repeat::KeyRepeatState::new(repeat_timer),
            held_resolved: None,
            present: PresentState::new(frames, shm_mem, pool, Format::Argb8888, 1),
            backbuffer: vec![0; usize::try_from(px_w * px_h).unwrap_or(0)],
            background: platform::draw::Color::rgb(0x1e, 0x1e, 0x2e),
            bg_alpha,
            pending_keys: Vec::new(),
            configured: false,
            closing: false,
        })
    }

    #[must_use]
    pub const fn scale(&self) -> i32 {
        self.scale
    }

    pub fn set_background(&mut self, color: platform::draw::Color) {
        self.background = color;
        self.request_redraw();
    }

    /// Mark the frame dirty *and* wake a blocked `wait` immediately.
    ///
    /// Marking dirty alone isn't enough: `wait` parks on the Wayland
    /// reader's wakeup eventfd, which only fires for genuine protocol
    /// events. Without this signal, a redraw requested right before the
    /// run loop's `wait(1000)` (e.g. every keystroke, since `main.rs`
    /// handles keys *then* waits) would sit unpainted for up to a second --
    /// or until the next real Wayland event happened to arrive -- which is
    /// exactly the "laggy typing" symptom: each character only appeared
    /// once you typed the *next* one, or after a long stall.
    pub fn request_redraw(&mut self) {
        self.present.mark_dirty();
        self.wake.signal();
    }

    /// Drain every resolved key event queued since the last call.
    pub fn take_keys(&mut self) -> Vec<LauncherKey> {
        std::mem::take(&mut self.pending_keys)
    }

    pub fn wait(&self, timeout_ms: i32) {
        let _ = self.wake.wait(timeout_ms);
        self.wake.drain();
    }

    /// Process pending events and present if dirty. Returns `false` once
    /// the surface is destroyed / the connection is lost.
    pub fn tick(&mut self, mut paint: impl FnMut(&mut Canvas)) -> Result<bool> {
        if self.closing {
            return Ok(false);
        }
        self.sender
            .drain_requests(&mut self.request_rx)
            .map_err(Error::Conn)?;

        while let Some(ev) = self.events.pop() {
            self.handle_event(&ev)?;
            if self.closing {
                return Ok(false);
            }
        }

        // Drain first, *then* ask whether the reader is gone, so a final
        // `wl_display.error` pushed on the way out is still handled above
        // rather than discarded in favour of a silent exit.
        if !self
            .reader_health
            .observe(self.events.is_closed(), self.events.is_empty())
            .is_alive()
        {
            self.closing = true;
            return Ok(false);
        }

        if self.configured
            && let PresentAttempt::Ready(slot) = self.present.begin_present()
        {
            self.do_present(slot, &mut paint);
        }
        Ok(true)
    }

    fn handle_event(&mut self, ev: &RawEvent) -> Result<()> {
        if let RawEvent::Fd(fds) = ev {
            self.handle_keymap_fd(fds);
            return Ok(());
        }
        if matches!(ev, RawEvent::Repeat) {
            self.handle_key_repeat_tick();
            return Ok(());
        }

        match self.present.route_common(ev, &mut self.window_tx) {
            surface::Routed::Fatal(e) => return Err(e),
            surface::Routed::Handled => return Ok(()),
            surface::Routed::Passthrough => {}
        }

        if ev.is(self.seat) && ev.event::<SeatEvent>() == Some(SeatEvent::Capabilities) {
            self.handle_seat_capabilities(ev);
            return Ok(());
        }

        if let Some(keyboard) = self.keyboard
            && ev.is(keyboard)
        {
            self.dispatch_keyboard_event(ev);
            return Ok(());
        }

        if ev.is(self.layer.layer_surface) {
            self.handle_layer_surface_event(ev);
        }
        Ok(())
    }

    fn handle_keymap_fd(&mut self, fds: &[std::os::fd::OwnedFd]) {
        use std::os::fd::AsFd as _;
        let Some(fd) = fds.first() else {
            return;
        };
        if let Some(size) = self.awaiting_keymap_fd.take() {
            match xkb::Translator::from_keymap_fd(fd.as_fd(), size) {
                Ok(t) => self.xkb = Some(t),
                Err(e) => eprintln!("launcher: XKB keymap translation unavailable: {e}"),
            }
        }
    }

    fn handle_seat_capabilities(&mut self, ev: &RawEvent) {
        let caps = ev.arg(0);
        if self.keyboard.is_none() && SeatCapability::Keyboard.in_mask(caps) {
            let keyboard: KeyboardId = self.window_tx.alloc_id();
            self.window_tx
                .request(MessageBuilder::new(self.seat, SeatRequest::GetKeyboard).new_id(keyboard));
            self.keyboard = Some(keyboard);
        }
    }

    fn dispatch_keyboard_event(&mut self, ev: &RawEvent) {
        match ev.event::<KeyboardEvent>() {
            Some(KeyboardEvent::Keymap) => {
                self.awaiting_keymap_fd = Some(ev.arg(1) as usize);
            }
            Some(KeyboardEvent::Key) => self.handle_key(ev),
            Some(KeyboardEvent::RepeatInfo) => {
                self.key_repeat.set_repeat_info(ev.arg(0), ev.arg(1));
            }
            Some(KeyboardEvent::Modifiers) => {
                if let Some(xkb) = &mut self.xkb {
                    let _ = xkb.update_mask(ev.arg(1), ev.arg(2), ev.arg(3), ev.arg(4));
                }
            }
            Some(KeyboardEvent::Leave) => {
                self.held_resolved = None;
                self.key_repeat.on_leave();
            }
            _ => {}
        }
    }

    /// Resolve one `wl_keyboard.key` press/release: XKB gives us both the
    /// keysym (for navigation keys) and, separately, the UTF-32 codepoint
    /// (for everything the user actually types) -- see `crate::xkb`'s
    /// `utf32`. Falls back to nothing (no physical-keycode text fallback
    /// makes sense without XKB: a launcher's search box needs real text,
    /// not just US-QWERTY guesses) when XKB isn't available.
    fn handle_key(&mut self, ev: &RawEvent) {
        let evdev_code = ev.arg(2);
        let state = KeyState::from_raw(ev.arg(3));
        let Some(KeyState::Pressed) = state else {
            if let Some(k) = Key::from_code(evdev_code) {
                // Clear the stashed resolution in lockstep with the timer,
                // so a late tick cannot resurrect a released key.
                if self.key_repeat.held_key() == Some(k) {
                    self.held_resolved = None;
                }
                self.key_repeat.on_key_release(k);
            }
            return;
        };

        let Some(resolved) = self.resolve_key(evdev_code) else {
            return;
        };
        self.pending_keys.push(resolved);
        if let Some(k) = Key::from_code(evdev_code)
            && !k.is_modifier()
        {
            // Stash what this press *meant*, not just which physical key it
            // was, so auto-repeat can reproduce it without re-decoding.
            self.held_resolved = Some(resolved);
            self.key_repeat.on_key_press(k);
        }
    }

    /// Resolve one press through the shared keysym decoder.
    ///
    /// The keysym constants and the named-key match used to be private to
    /// this file -- one of four copies of the same X11 table in the
    /// workspace. They now come from [`platform::toplevel::keysym`], which
    /// checks named keys *before* text and so cannot fall into the trap
    /// this file was one reordering away from: Return/Tab/Backspace/Escape
    /// each carry a legacy control codepoint, so a text-first decoder types
    /// a carriage return into the search box when you press Enter.
    fn resolve_key(&self, evdev_code: u32) -> Option<LauncherKey> {
        let xkb = self.xkb.as_ref()?;
        let sym = xkb.keysym(evdev_code);

        // Ctrl+digit is checked first: it is a chord this window binds to
        // "launch the Nth result", so it must win over the digit's text.
        if xkb.control_active()
            && let Some(digit) = Self::digit_for_keysym(sym)
        {
            return Some(LauncherKey::CtrlDigit(digit));
        }

        LauncherKey::from_key(DecodedKey::decode(sym, xkb.utf32(evdev_code)))
    }

    /// Auto-repeat: re-emit whatever the *original* press resolved to.
    ///
    /// This previously re-derived a character from the physical keycode via
    /// a hardcoded US-QWERTY table, because `KeyRepeatState` remembers only
    /// the physical `protocol::Key` and the evdev code is long gone by the
    /// time the timer fires. That made held-key repeat type the wrong
    /// letter on every non-US layout -- press `a` on AZERTY and the first
    /// character came from XKB (correct) while every repeat came from the
    /// QWERTY table (wrong).
    ///
    /// Remembering the resolved key at press time sidesteps the re-decode
    /// entirely, so repeat is correct on every layout by construction.
    fn handle_key_repeat_tick(&mut self) {
        if self.key_repeat.held_key().is_none() {
            return;
        }
        if let Some(resolved) = self.held_resolved {
            self.pending_keys.push(resolved);
        }
    }

    const fn digit_for_keysym(sym: u32) -> Option<u8> {
        match sym {
            0x30 => Some(0),
            0x31 => Some(1),
            0x32 => Some(2),
            0x33 => Some(3),
            0x34 => Some(4),
            0x35 => Some(5),
            0x36 => Some(6),
            0x37 => Some(7),
            0x38 => Some(8),
            0x39 => Some(9),
            _ => None,
        }
    }

    fn handle_layer_surface_event(&mut self, ev: &RawEvent) {
        match ev.event::<LayerSurfaceEvent>() {
            Some(LayerSurfaceEvent::Configure) => {
                let serial = ev.arg(0);
                self.layer.ack_configure_queued(&self.window_tx, serial);
                self.configured = true;
                self.present.mark_dirty();
            }
            Some(LayerSurfaceEvent::Closed) => {
                self.closing = true;
            }
            None => {}
        }
    }

    fn pack_bg(&self) -> u32 {
        surface::SurfaceBootstrap::pack_bg(self.background, self.bg_alpha)
    }

    fn do_present(&mut self, slot: usize, paint: &mut impl FnMut(&mut Canvas)) {
        {
            let bg_packed = self.pack_bg();
            for p in &mut self.backbuffer {
                *p = bg_packed;
            }
            // Hand the painter a canvas that knows this window is
            // translucent, so its `fill_rect`/`fill_round_rect` calls pack
            // for the premultiplied Argb8888 backbuffer instead of writing a
            // zero alpha byte (an invisible fill).
            let mut canvas = Canvas::new(&mut self.backbuffer, self.width, self.height)
                .with_format(platform::SurfaceFormat::with_bg_alpha(self.bg_alpha));
            paint(&mut canvas);
        }

        let (dst, _stale) = self.present.slot_pixels_and_stale(slot);
        dst.copy_from_slice(&self.backbuffer);

        surface::SurfacePresenter::commit_full_frame(
            &mut self.present,
            &mut self.window_tx,
            self.layer.surface,
            slot,
            self.width,
            self.height,
        );
    }

    fn discover(sender: &mut Sender, events: &mut Consumer<RawEvent>) -> Result<Globals> {
        let mut globals = GlobalsBuilder::default();
        surface::RegistryDiscovery::collect(sender, events, &mut globals)?;
        globals.finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use platform::toplevel::keysym::sym;

    #[test]
    fn text_and_navigation_narrow_to_launcher_keys() {
        assert_eq!(
            LauncherKey::from_key(DecodedKey::decode(0x61, u32::from('a'))),
            Some(LauncherKey::Char('a'))
        );
        assert_eq!(
            LauncherKey::from_key(DecodedKey::decode(sym::UP, 0)),
            Some(LauncherKey::Up)
        );
        assert_eq!(
            LauncherKey::from_key(DecodedKey::decode(sym::DOWN, 0)),
            Some(LauncherKey::Down)
        );
        assert_eq!(
            LauncherKey::from_key(DecodedKey::decode(sym::ESCAPE, 0x1b)),
            Some(LauncherKey::Esc)
        );
    }

    /// The trap the shared decoder exists to close. Enter, Backspace and
    /// Escape all report a legacy control codepoint alongside their keysym;
    /// decoding text first would insert U+000D into the search query
    /// instead of launching the selected entry.
    #[test]
    fn keys_carrying_a_control_codepoint_stay_actions_not_text() {
        assert_eq!(
            LauncherKey::from_key(DecodedKey::decode(sym::RETURN, 0x0d)),
            Some(LauncherKey::Enter)
        );
        assert_eq!(
            LauncherKey::from_key(DecodedKey::decode(sym::BACKSPACE, 0x08)),
            Some(LauncherKey::Backspace)
        );
    }

    /// The keypad's Enter is a distinct keysym; the old private table did
    /// list it, and dropping it here would be a silent regression.
    #[test]
    fn the_keypad_enter_still_launches() {
        assert_eq!(
            LauncherKey::from_key(DecodedKey::decode(sym::KP_ENTER, 0)),
            Some(LauncherKey::Enter)
        );
    }

    /// Non-US layouts are the reason the repeat path was rewritten: the
    /// decoder must carry whatever character XKB reports, not a QWERTY
    /// guess derived from the physical keycode.
    #[test]
    fn a_non_ascii_character_survives_decoding() {
        assert_eq!(
            LauncherKey::from_key(DecodedKey::decode(0, u32::from('é'))),
            Some(LauncherKey::Char('é'))
        );
    }

    /// Keys a search box has no meaning for are dropped rather than
    /// invented into something.
    #[test]
    fn irrelevant_keys_are_ignored() {
        assert_eq!(LauncherKey::from_key(DecodedKey::decode(sym::F1, 0)), None);
        assert_eq!(
            LauncherKey::from_key(DecodedKey::decode(sym::INSERT, 0)),
            None
        );
        // A bare modifier: real keysym, no symbol, no text.
        assert_eq!(LauncherKey::from_key(DecodedKey::decode(0xffe1, 0)), None);
        assert_eq!(LauncherKey::from_key(None), None);
    }

    #[test]
    fn ctrl_digits_map_across_the_whole_row() {
        assert_eq!(LauncherWindow::digit_for_keysym(0x30), Some(0));
        assert_eq!(LauncherWindow::digit_for_keysym(0x39), Some(9));
        assert_eq!(LauncherWindow::digit_for_keysym(0x2f), None);
        assert_eq!(LauncherWindow::digit_for_keysym(0x3a), None);
    }
}
