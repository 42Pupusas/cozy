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
use platform::id::{
    CompositorId, KeyboardId, LayerShellId, LayerSurfaceId, ObjectId, OutputId, RegistryId, SeatId,
    ShmId, SurfaceId,
};
use platform::present::{PresentAttempt, PresentState};
use platform::protocol::{
    self as proto, CompositorRequest, DisplayEvent, DisplayRequest, Format, Key, KeyState,
    KeyboardEvent, KeyboardInteractivity, Layer, LayerShellRequest, LayerSurfaceEvent,
    LayerSurfaceRequest, RegistryEvent, SeatCapability, SeatEvent, SeatRequest, SurfaceRequest,
};
use platform::wire::{MessageBuilder, WireEvent};
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
}

/// A centered, keyboard-grabbing layer-shell overlay window.
pub struct LauncherWindow {
    sender: Sender,
    request_rx: mpsc::Consumer<platform::conn::WindowRequest>,
    window_tx: WindowSender,
    events: Consumer<RawEvent>,
    #[allow(dead_code)]
    reader: Option<std::thread::JoinHandle<Result<()>>>,
    wake: waker::Waker,

    #[allow(dead_code)]
    compositor: CompositorId,
    #[allow(dead_code)]
    layer_shell: LayerShellId,
    surface: SurfaceId,
    layer_surface: LayerSurfaceId,

    scale: i32,
    width: i32,
    height: i32,

    seat: SeatId,
    keyboard: Option<KeyboardId>,
    xkb: Option<xkb::Translator>,
    awaiting_keymap_fd: Option<usize>,
    key_repeat: platform::key_repeat::KeyRepeatState,

    present: PresentState,
    backbuffer: Vec<u32>,
    background: platform::draw::Color,

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

impl GlobalsBuilder {
    const fn has_every_required_global(&self) -> bool {
        self.compositor.is_some() && self.shm.is_some() && self.layer_shell.is_some() && self.seat.is_some()
    }

    fn bind_named_global(
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

    fn bind_if_missing<T: ObjectId>(
        slot: &mut Option<T>,
        sender: &mut Sender,
        registry: RegistryId,
        name: u32,
        interface: &str,
        advertised_version: u32,
        known_version: u32,
    ) -> Result<()> {
        if slot.is_some() {
            return Ok(());
        }
        let v = known_version.min(advertised_version);
        *slot = Some(surface::SurfaceBootstrap::bind(
            sender, registry, name, interface, v,
        )?);
        Ok(())
    }

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
        let _ = self.sender.request(MessageBuilder::new(
            self.layer_surface,
            LayerSurfaceRequest::Destroy,
        ));
        let _ = self
            .sender
            .request(MessageBuilder::new(self.surface, SurfaceRequest::Destroy));
    }
}

impl LauncherWindow {
    /// Open a centered, keyboard-exclusive overlay `width`x`height` logical
    /// pixels (scaled to physical pixels by the output's buffer scale).
    pub fn new(width: i32, height: i32, wake: waker::Waker) -> Result<Self> {
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

        let (frames, shm_mem, pool) =
            Frame::create_pair(&mut sender, shm, px_w, px_h, Format::Argb8888)?;

        let surface: SurfaceId = sender.alloc_id();
        sender.request(
            MessageBuilder::new(compositor, CompositorRequest::CreateSurface).new_id(surface),
        )?;
        sender.request(MessageBuilder::new(surface, SurfaceRequest::SetBufferScale).i32(scale))?;

        let layer_surface: LayerSurfaceId = sender.alloc_id();
        sender.request(
            MessageBuilder::new(layer_shell, LayerShellRequest::GetLayerSurface)
                .new_id(layer_surface)
                .object(surface)
                .object(OutputId(0))
                .u32(Layer::Overlay as u32)
                .string(APP_ID),
        )?;

        // No SetAnchor call: leaving every anchor bit unset means the
        // compositor centers the surface on the output, exactly the "type
        // to search" floating bar every desktop launcher uses.
        sender.request(
            MessageBuilder::new(layer_surface, LayerSurfaceRequest::SetSize)
                .i32(width)
                .i32(height),
        )?;
        sender.request(
            MessageBuilder::new(layer_surface, LayerSurfaceRequest::SetKeyboardInteractivity)
                .u32(KeyboardInteractivity::Exclusive as u32),
        )?;
        sender.request(MessageBuilder::new(surface, SurfaceRequest::Commit))?;

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
            wake,
            compositor,
            layer_shell,
            surface,
            layer_surface,
            scale,
            width: px_w,
            height: px_h,
            seat,
            keyboard: None,
            xkb: None,
            awaiting_keymap_fd: None,
            key_repeat: platform::key_repeat::KeyRepeatState::new(repeat_timer),
            present: PresentState::new(frames, shm_mem, pool, Format::Argb8888, 1),
            backbuffer: vec![
                0;
                usize::try_from(px_w * px_h).unwrap_or(0)
            ],
            background: platform::draw::Color::rgb(0x1e, 0x1e, 0x2e),
            pending_keys: Vec::new(),
            configured: false,
            closing: false,
        })
    }

    #[must_use]
    pub const fn scale(&self) -> i32 {
        self.scale
    }

    pub const fn set_background(&mut self, color: platform::draw::Color) {
        self.background = color;
        self.present.mark_dirty();
    }

    pub const fn request_redraw(&mut self) {
        self.present.mark_dirty();
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

        if ev.is(self.layer_surface) {
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
            Some(KeyboardEvent::Leave) => self.key_repeat.on_leave(),
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
            self.key_repeat.on_key_press(k);
        }
    }

    fn resolve_key(&self, evdev_code: u32) -> Option<LauncherKey> {
        // XKB keysym constants for the handful of non-text keys a search
        // box cares about (module-level, not local -- clippy's
        // items_after_statements flags consts declared after a let binding
        // above them).
        const XKB_KEY_BACKSPACE: u32 = 0xff08;
        const XKB_KEY_RETURN: u32 = 0xff0d;
        const XKB_KEY_KP_ENTER: u32 = 0xff8d;
        const XKB_KEY_ESCAPE: u32 = 0xff1b;
        const XKB_KEY_UP: u32 = 0xff52;
        const XKB_KEY_DOWN: u32 = 0xff54;

        let Some(xkb) = &self.xkb else {
            return None;
        };

        let sym = xkb.keysym(evdev_code);
        match sym {
            XKB_KEY_BACKSPACE => return Some(LauncherKey::Backspace),
            XKB_KEY_RETURN | XKB_KEY_KP_ENTER => return Some(LauncherKey::Enter),
            XKB_KEY_ESCAPE => return Some(LauncherKey::Esc),
            XKB_KEY_UP => return Some(LauncherKey::Up),
            XKB_KEY_DOWN => return Some(LauncherKey::Down),
            _ => {}
        }

        let cp = xkb.utf32(evdev_code);
        if cp == 0 {
            return None;
        }
        char::from_u32(cp).map(LauncherKey::Char)
    }

    fn handle_key_repeat_tick(&mut self) {
        let Some(key) = self.key_repeat.held_key() else {
            return;
        };
        // Re-resolve via the last-known evdev code is unavailable here (the
        // repeat timer only knows the logical `Key`, not the original
        // evdev code) -- so auto-repeat re-emits the same logical key by
        // reusing `Key::from_code`'s inverse mapping isn't available
        // either. Simplest correct behavior for a search box: re-derive
        // straight from the physical `Key` via the same ASCII table
        // `platform::keymap::PhysicalKey` uses, since letters/digits are
        // what overwhelmingly get held-and-repeated while typing.
        if let Some(ch) = Self::key_to_ascii(key) {
            self.pending_keys.push(LauncherKey::Char(ch));
        } else {
            match key {
                Key::Backspace => self.pending_keys.push(LauncherKey::Backspace),
                Key::Enter => self.pending_keys.push(LauncherKey::Enter),
                _ => {}
            }
        }
    }

    /// Best-effort physical-keycode fallback for auto-repeat's synthesized
    /// presses (US-QWERTY only, no XKB remap applied -- see
    /// `handle_key_repeat_tick`'s doc for why the real per-keystroke XKB
    /// path can't be reused here).
    const fn key_to_ascii(key: Key) -> Option<char> {
        Some(match key {
            Key::A => 'a',
            Key::B => 'b',
            Key::C => 'c',
            Key::D => 'd',
            Key::E => 'e',
            Key::F => 'f',
            Key::G => 'g',
            Key::H => 'h',
            Key::I => 'i',
            Key::J => 'j',
            Key::K => 'k',
            Key::L => 'l',
            Key::M => 'm',
            Key::N => 'n',
            Key::O => 'o',
            Key::P => 'p',
            Key::Q => 'q',
            Key::R => 'r',
            Key::S => 's',
            Key::T => 't',
            Key::U => 'u',
            Key::V => 'v',
            Key::W => 'w',
            Key::X => 'x',
            Key::Y => 'y',
            Key::Z => 'z',
            Key::N0 => '0',
            Key::N1 => '1',
            Key::N2 => '2',
            Key::N3 => '3',
            Key::N4 => '4',
            Key::N5 => '5',
            Key::N6 => '6',
            Key::N7 => '7',
            Key::N8 => '8',
            Key::N9 => '9',
            Key::Space => ' ',
            Key::Minus => '-',
            _ => return None,
        })
    }

    fn handle_layer_surface_event(&mut self, ev: &RawEvent) {
        match ev.event::<LayerSurfaceEvent>() {
            Some(LayerSurfaceEvent::Configure) => {
                let serial = ev.arg(0);
                self.window_tx.request(
                    MessageBuilder::new(self.layer_surface, LayerSurfaceRequest::AckConfigure)
                        .u32(serial),
                );
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
        surface::SurfaceBootstrap::pack_bg(self.background, 255)
    }

    fn do_present(&mut self, slot: usize, paint: &mut impl FnMut(&mut Canvas)) {
        {
            let bg_packed = self.pack_bg();
            for p in &mut self.backbuffer {
                *p = bg_packed;
            }
            let mut canvas = Canvas::new(&mut self.backbuffer, self.width, self.height);
            paint(&mut canvas);
        }

        let (dst, _stale) = self.present.slot_pixels_and_stale(slot);
        dst.copy_from_slice(&self.backbuffer);

        let buffer = self.present.frame_buffer(slot);
        self.window_tx.request(
            MessageBuilder::new(self.surface, SurfaceRequest::Attach)
                .object(buffer)
                .i32(0)
                .i32(0),
        );
        self.window_tx.request(
            MessageBuilder::new(self.surface, SurfaceRequest::Damage)
                .i32(0)
                .i32(0)
                .i32(self.width)
                .i32(self.height),
        );
        let cb: platform::id::CallbackId = self.window_tx.alloc_id();
        self.window_tx
            .request(MessageBuilder::new(self.surface, SurfaceRequest::Frame).new_id(cb));
        self.window_tx
            .request(MessageBuilder::new(self.surface, SurfaceRequest::Commit));
        self.present.finish_present(cb);
    }

    fn discover(sender: &mut Sender, events: &mut Consumer<RawEvent>) -> Result<Globals> {
        let registry: RegistryId = sender.alloc_id();
        sender.request(
            MessageBuilder::new(platform::id::DisplayId(1), DisplayRequest::GetRegistry)
                .new_id(registry),
        )?;

        let mut globals = GlobalsBuilder::default();
        while !globals.has_every_required_global() {
            let event = events.pop_block().ok_or(Error::ReaderStopped)?;
            let RawEvent::Bytes(bytes) = event else {
                continue;
            };
            let Some((mut ev, _)) = WireEvent::parse(&bytes) else {
                continue;
            };

            if ev.object_id == platform::id::DisplayId(1).raw()
                && DisplayEvent::from_opcode(ev.opcode) == Some(DisplayEvent::Error)
            {
                return Err(Error::Protocol {
                    object: ev.u32(),
                    code: ev.u32(),
                    message: ev.string(),
                });
            }

            if ev.object_id == registry.raw()
                && RegistryEvent::from_opcode(ev.opcode) == Some(RegistryEvent::Global)
            {
                let name = ev.u32();
                let interface = ev.string();
                let version = ev.u32();
                globals.bind_named_global(sender, registry, name, &interface, version)?;
            }
        }
        globals.finish()
    }
}
