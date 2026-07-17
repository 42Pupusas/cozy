//! The Wayland objects we speak to, as typed enums.
//!
//! Wayland's catalogue of interfaces lives in XML files (`wayland.xml`,
//! `xdg-shell.xml`). Bindings generators turn those into code; since we're
//! doing it by hand, we transcribe only the requests/events we actually use.
//!
//! Each interface gets two `#[repr(u16)]` enums — one for the requests we send,
//! one for the events we receive — so the enum *discriminant is the wire
//! opcode*. Opcodes are the 0-based index of a request (or event) within its
//! interface, in declaration order, matching the stable protocol versions.
//!
//! This buys us:
//!   * **type safety** — you can't pass a `wl_shm` opcode where a `wl_surface`
//!     one is expected; each interface's requests are a distinct type.
//!   * **exhaustive matching** — the event loop matches on a real enum, and an
//!     unrecognised opcode decodes to `None` rather than a silent wrong branch.
//!
//! Some variants are transcribed ahead of being wired up (events we don't yet
//! dispatch on); `#![allow(dead_code)]` keeps this a complete reference table.
#![allow(dead_code)]

/// A request enum whose discriminant is its wire opcode. Implemented for every
/// `*Request` enum so [`MessageBuilder::new`](crate::wire::MessageBuilder::new)
/// can accept any of them directly.
pub trait Opcode: Copy {
    fn opcode(self) -> u16;
}

/// An event enum that can be decoded from a wire opcode. Implemented for every
/// `*Event` enum so a generic decoder (e.g. `Event::event::<E>()`) works for any
/// of them.
pub trait FromOpcode: Sized {
    fn from_opcode(op: u16) -> Option<Self>;
}

/// Generates a `#[repr(u16)]` request enum plus its [`Opcode`] impl.
macro_rules! requests {
    ($(#[$m:meta])* $name:ident {
        $($(#[$vm:meta])* $variant:ident = $op:expr),+ $(,)?
    }) => {
        $(#[$m])*
        #[derive(Copy, Clone, Debug, PartialEq, Eq)]
        #[repr(u16)]
        pub enum $name {
            $($(#[$vm])* $variant = $op),+
        }
        impl Opcode for $name {
            fn opcode(self) -> u16 {
                self as u16
            }
        }
    };
}

/// Generates a `#[repr(u16)]` event enum plus `from_opcode`, which decodes a
/// raw wire opcode into the enum (returning `None` for opcodes we don't model).
macro_rules! events {
    ($(#[$m:meta])* $name:ident {
        $($(#[$vm:meta])* $variant:ident = $op:expr),+ $(,)?
    }) => {
        $(#[$m])*
        #[derive(Copy, Clone, Debug, PartialEq, Eq)]
        #[repr(u16)]
        pub enum $name {
            $($(#[$vm])* $variant = $op),+
        }
        impl $name {
            /// Decode a wire opcode, or `None` if it isn't one we recognise.
            pub fn from_opcode(op: u16) -> Option<Self> {
                match op {
                    $($op => Some(Self::$variant),)+
                    _ => None,
                }
            }
        }
        impl FromOpcode for $name {
            fn from_opcode(op: u16) -> Option<Self> {
                Self::from_opcode(op)
            }
        }
    };
}

/// `wl_display` is always object id 1; it's the root of everything.
pub const DISPLAY_ID: u32 = 1;

requests! {
    /// `wl_display` requests.
    DisplayRequest {
        Sync = 0,
        GetRegistry = 1,
    }
}
events! {
    /// `wl_display` events.
    DisplayEvent {
        /// A request referenced an invalid object — fatal protocol error.
        Error = 0,
        /// The server has deleted the object with the given id.
        DeleteId = 1,
    }
}

requests! {
    /// `wl_registry` requests — binds the compositor's global singletons.
    RegistryRequest {
        Bind = 0,
    }
}
events! {
    /// `wl_registry` events.
    RegistryEvent {
        /// A global is available: `name`, `interface`, `version`.
        Global = 0,
        /// A previously announced global went away.
        GlobalRemove = 1,
    }
}

requests! {
    /// `wl_compositor` requests — the factory for surfaces.
    CompositorRequest {
        CreateSurface = 0,
    }
}

requests! {
    /// `wl_surface` requests — a rectangle of pixels the compositor composites.
    SurfaceRequest {
        Destroy = 0,
        Attach = 1,
        Damage = 2,
        /// Request a one-shot frame-throttling callback (a `wl_callback` whose
        /// `done` event fires when the compositor is ready for the next frame).
        Frame = 3,
        /// Set the buffer scale (integer, ≥1). 1 = 1:1, 2 = HiDPI @ 2x.
        SetBufferScale = 8,
        Commit = 6,
    }
}

requests! {
    /// `wl_buffer` requests.
    BufferRequest {
        /// Destroy the buffer object (its id becomes reusable once the server
        /// acknowledges with `wl_display.delete_id`).
        Destroy = 0,
    }
}
events! {
    /// `wl_buffer` events.
    BufferEvent {
        /// The compositor is done reading this buffer; we may reuse it.
        Release = 0,
    }
}

events! {
    /// `wl_callback` events (used for `wl_surface.frame` throttling).
    CallbackEvent {
        /// The callback has fired: `callback_data` (a timestamp we ignore).
        Done = 0,
    }
}

requests! {
    /// `wl_shm` requests — the shared-memory buffer factory.
    ShmRequest {
        CreatePool = 0,
    }
}
events! {
    /// `wl_shm` events.
    ShmEvent {
        /// The compositor advertises a supported pixel format.
        Format = 0,
    }
}

requests! {
    /// `wl_shm_pool` requests — carves buffers out of a shared-memory pool.
    ShmPoolRequest {
        CreateBuffer = 0,
        /// Destroy the pool and free the fd (v3+).
        Destroy = 1,
        /// Grow the pool to a new (larger) byte size after the backing fd has
        /// been `ftruncate`d. The compositor re-maps to match; the size may
        /// only ever increase.
        Resize = 2,
    }
}

requests! {
    /// `xdg_wm_base` requests — the shell that turns surfaces into windows.
    WmBaseRequest {
        GetXdgSurface = 2,
        Pong = 3,
    }
}
events! {
    /// `xdg_wm_base` events.
    WmBaseEvent {
        /// Liveness check; we must `Pong` with the same serial or get killed.
        Ping = 0,
    }
}

requests! {
    /// `xdg_surface` requests — bridges a `wl_surface` to xdg-shell roles.
    XdgSurfaceRequest {
        Destroy = 0,
        GetToplevel = 1,
        AckConfigure = 4,
    }
}
events! {
    /// `xdg_surface` events.
    XdgSurfaceEvent {
        /// The compositor proposes a configuration; ack it, then commit.
        Configure = 0,
    }
}

requests! {
    /// `xdg_toplevel` requests — a top-level application window.
    ToplevelRequest {
        Destroy = 0,
        SetTitle = 2,
        /// Stable, machine-facing identifier (app_id) the compositor uses for
        /// window rules / matching — distinct from the human-facing title,
        /// which apps overwrite at runtime via OSC.
        SetAppId = 3,
    }
}

requests! {
    /// `zxdg_decoration_manager_v1` requests — the SSD factory.
    DecorationManagerRequest {
        /// Create a decoration object for a toplevel, then `set_mode` on it.
        GetToplevelDecoration = 1,
    }
}

requests! {
    /// `zxdg_toplevel_decoration_v1` requests.
    ToplevelDecorationRequest {
        SetMode = 1,
    }
}

/// `zxdg_toplevel_decoration_v1` mode values (argument to `set_mode`).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum DecorationMode {
    /// Client draws its own decorations.
    ClientSide = 1,
    /// Compositor draws the titlebar/border — what we want.
    ServerSide = 2,
}
events! {
    /// `xdg_toplevel` events.
    ToplevelEvent {
        /// Suggested new size and states.
        Configure = 0,
        /// The user (or compositor) asked the window to close.
        Close = 1,
    }
}

events! {
    /// `wl_output` events. We only care about `scale` (HiDPI/fractional-ish
    /// integer scale factor) — `geometry`/`mode`/`done`/`name`/`description`
    /// are transcribed for completeness but unused.
    OutputEvent {
        /// Physical geometry: position, physical size, subpixel layout, make,
        /// model, transform.
        Geometry = 0,
        /// A display mode (resolution/refresh); may fire multiple times.
        Mode = 1,
        /// Marks the end of a burst of `geometry`/`mode`/`scale` events sent
        /// atomically (so a reader can wait for `done` before acting).
        Done = 2,
        /// Integer buffer scale factor this output expects (1 = normal,
        /// 2 = HiDPI @ 2x, ...). This is the one we actually read.
        Scale = 3,
    }
}

requests! {
    /// `wl_seat` requests — an input-device group (pointer/keyboard/touch).
    /// Touch (`get_touch` = 2) comes when we add that device.
    SeatRequest {
        GetPointer = 0,
        GetKeyboard = 1,
    }
}
events! {
    /// `wl_seat` events.
    SeatEvent {
        /// Bitmask of available device capabilities (see [`SeatCapability`]).
        Capabilities = 0,
        /// A human-readable seat name.
        Name = 1,
    }
}

requests! {
    /// `wl_pointer` requests.
    PointerRequest {
        /// Set the cursor surface: `serial`, `surface` (or 0 to hide), `hotspot_x`,
        /// `hotspot_y` (both `i32` in surface-local coords).
        SetCursor = 0,
        /// Release the pointer object (v3+); harmless to skip for our purposes.
        Release = 1,
    }
}
events! {
    /// `wl_pointer` events. Coordinates on `Enter`/`Motion` are `wl_fixed`
    /// (24.8 fixed-point `i32`); divide by 256 — see [`fixed_to_i32`].
    PointerEvent {
        /// Pointer entered a surface: `serial`, `surface`, `x`, `y`.
        Enter = 0,
        /// Pointer left a surface: `serial`, `surface`.
        Leave = 1,
        /// Pointer moved: `time`, `x`, `y`.
        Motion = 2,
        /// Button changed: `serial`, `time`, `button`, `state`.
        Button = 3,
        /// Scroll/axis event: `time`, `axis`, `value`.
        Axis = 4,
    }
}

/// Mouse buttons, by their Linux evdev codes (as `wl_pointer.button` reports
/// them in its `button` argument).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum PointerButton {
    Left = 0x110,
    Right = 0x111,
    Middle = 0x112,
}

impl PointerButton {
    /// Identify a button from its raw evdev code, or `None` if we don't model it.
    pub fn from_code(code: u32) -> Option<Self> {
        match code {
            0x110 => Some(PointerButton::Left),
            0x111 => Some(PointerButton::Right),
            0x112 => Some(PointerButton::Middle),
            _ => None,
        }
    }
}

/// Whether a `wl_pointer.button` event is a press or a release (its `state` arg).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum ButtonState {
    Released = 0,
    Pressed = 1,
}

impl ButtonState {
    pub fn from_raw(state: u32) -> Option<Self> {
        match state {
            0 => Some(ButtonState::Released),
            1 => Some(ButtonState::Pressed),
            _ => None,
        }
    }
}

requests! {
    /// `wl_keyboard` requests.
    KeyboardRequest {
        /// Release the keyboard object (v3+); harmless to skip for our purposes.
        Release = 0,
    }
}

requests! {
    /// `wl_data_device_manager` requests — clipboard/DnD factory.
    DataDeviceManagerRequest {
        /// Create a data source (data we offer to other clients).
        CreateDataSource = 0,
        /// Get the per-seat data device.
        GetDataDevice = 1,
    }
}
requests! {
    /// `wl_data_device` requests.
    DataDeviceRequest {
        /// Start a drag-and-drop (unused here).
        StartDrag = 0,
        /// Set the clipboard selection to our data source: `serial`, `source`.
        SetSelection = 1,
        /// Release the device (v2+).
        Release = 2,
    }
}
events! {
    /// `wl_data_device` events.
    DataDeviceEvent {
        /// A new drag offer appeared (unused here).
        DataOffer = 0,
        /// Keyboard focus entered during DnD (unused here).
        Enter = 1,
        /// DnD focus left (unused here).
        Leave = 2,
        /// DnD motion (unused here).
        Motion = 3,
        /// DnD drop (unused here).
        Drop = 4,
        /// The clipboard selection changed: `id` (a `wl_data_offer`, or 0).
        Selection = 5,
    }
}
requests! {
    /// `wl_data_source` requests — data we offer to other clients (our copy).
    DataSourceRequest {
        /// Advertise one MIME type we can provide: `mime_type` (string).
        Offer = 0,
        /// Destroy the source.
        Destroy = 1,
        /// Set the allowed DnD actions (v3+; unused for plain clipboard).
        SetActions = 2,
    }
}
events! {
    /// `wl_data_source` events.
    DataSourceEvent {
        /// A destination accepted a MIME type (drag target; unused here).
        Target = 0,
        /// A client requests the data: `mime_type` (string), `fd` — we write
        /// the selection to `fd` and close it. The fd rides in ancillary data.
        Send = 1,
        /// The selection was replaced by another client; destroy our source.
        Cancelled = 2,
        /// DnD drop performed (unused here).
        DndDropPerformed = 3,
        /// DnD finished (unused here).
        DndFinished = 4,
        /// Action offered (unused here).
        Action = 5,
    }
}
requests! {
    /// `wl_data_offer` requests — data another client offers us (our paste).
    DataOfferRequest {
        /// Accept a MIME type (drag; for clipboard we can send a null serial):
        /// `serial`, `mime_type`.
        Accept = 0,
        /// Ask the source to transfer: `mime_type`, `fd` — we read from `fd`.
        Receive = 1,
        /// Destroy the offer.
        Destroy = 2,
        /// Finish a DnD offer (unused here).
        Finish = 3,
        /// Set actions (unused here).
        SetActions = 4,
    }
}
events! {
    /// `wl_data_offer` events.
    DataOfferEvent {
        /// The source advertises one MIME type it can provide: `mime_type`.
        Offer = 0,
        /// Source actions (v3+; unused).
        SourceActions = 1,
        /// Offered action (v3+; unused).
        Action = 2,
    }
}
events! {
    /// `wl_keyboard` events.
    KeyboardEvent {
        /// The XKB keymap, as `format`, an `fd`, and a `size`. The fd rides in
        /// ancillary data — the one keyboard event that carries one. We don't
        /// parse it (we map raw evdev codes directly), so we close the fd and
        /// move on.
        Keymap = 0,
        /// Keyboard focus entered a surface: `serial`, `surface`, `keys` (an
        /// array of the evdev codes already held down).
        Enter = 1,
        /// Keyboard focus left a surface: `serial`, `surface`.
        Leave = 2,
        /// A key changed: `serial`, `time`, `key` (evdev code), `state`.
        Key = 3,
        /// Modifier/group state changed: `serial`, `depressed`, `latched`,
        /// `locked`, `group`. We don't track modifiers yet.
        Modifiers = 4,
        /// The compositor's key-repeat parameters: `rate` (keys/sec, 0 disables
        /// repeat) and `delay` (ms before the first repeat). `wl_keyboard` v4+.
        RepeatInfo = 5,
    }
}

/// Whether a `wl_keyboard.key` event is a press or a release (its `state` arg).
/// Same wire values as [`ButtonState`], but a distinct type so a key state can't
/// be passed where a pointer-button state is expected.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum KeyState {
    Released = 0,
    Pressed = 1,
}

impl KeyState {
    pub fn from_raw(state: u32) -> Option<Self> {
        match state {
            0 => Some(KeyState::Released),
            1 => Some(KeyState::Pressed),
            _ => None,
        }
    }
}

/// The keys we act on, by their Linux evdev codes — exactly what a
/// `wl_keyboard.key` event reports in its `key` argument (the raw kernel
/// keycode; the +8 XKB offset applies only to the keymap, not the wire field).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum Key {
    // Row 1: number row
    Backtick = 41,
    N1 = 2,
    N2 = 3,
    N3 = 4,
    N4 = 5,
    N5 = 6,
    N6 = 7,
    N7 = 8,
    N8 = 9,
    N9 = 10,
    N0 = 11,
    Minus = 12,
    Equal = 13,
    Backspace = 14,
    // Row 2: QWERTY
    Tab = 15,
    Q = 16,
    W = 17,
    E = 18,
    R = 19,
    T = 20,
    Y = 21,
    U = 22,
    I = 23,
    O = 24,
    P = 25,
    LBracket = 26,
    RBracket = 27,
    Backslash = 43,
    // Row 3: home row
    CapsLock = 58,
    A = 30,
    S = 31,
    D = 32,
    F = 33,
    G = 34,
    H = 35,
    J = 36,
    K = 37,
    L = 38,
    Semicolon = 39,
    Apostrophe = 40,
    Enter = 28,
    // Row 4: bottom row
    LShift = 42,
    Z = 44,
    X = 45,
    C = 46,
    V = 47,
    B = 48,
    N = 49,
    M = 50,
    Comma = 51,
    Dot = 52,
    Slash = 53,
    RShift = 54,
    // Modifiers / special
    LCtrl = 29,
    LMeta = 125,
    LAlt = 56,
    Space = 57,
    RAlt = 100,
    RMeta = 126,
    RCtrl = 97,
    // Navigation cluster
    Esc = 1,
    F1 = 59,
    F2 = 60,
    F3 = 61,
    F4 = 62,
    F5 = 63,
    F6 = 64,
    F7 = 65,
    F8 = 66,
    F9 = 67,
    F10 = 68,
    F11 = 87,
    F12 = 88,
    Insert = 110,
    Delete = 111,
    Home = 102,
    End = 107,
    PageUp = 104,
    PageDown = 109,
    Up = 103,
    Down = 108,
    Left = 105,
    Right = 106,
}

impl Key {
    /// Identify a key from its raw evdev code, or `None` if we don't recognise it.
    #[allow(clippy::too_many_lines)]
    pub fn from_code(code: u32) -> Option<Self> {
        match code {
            1 => Some(Key::Esc),
            2 => Some(Key::N1),
            3 => Some(Key::N2),
            4 => Some(Key::N3),
            5 => Some(Key::N4),
            6 => Some(Key::N5),
            7 => Some(Key::N6),
            8 => Some(Key::N7),
            9 => Some(Key::N8),
            10 => Some(Key::N9),
            11 => Some(Key::N0),
            12 => Some(Key::Minus),
            13 => Some(Key::Equal),
            14 => Some(Key::Backspace),
            15 => Some(Key::Tab),
            16 => Some(Key::Q),
            17 => Some(Key::W),
            18 => Some(Key::E),
            19 => Some(Key::R),
            20 => Some(Key::T),
            21 => Some(Key::Y),
            22 => Some(Key::U),
            23 => Some(Key::I),
            24 => Some(Key::O),
            25 => Some(Key::P),
            26 => Some(Key::LBracket),
            27 => Some(Key::RBracket),
            28 => Some(Key::Enter),
            29 => Some(Key::LCtrl),
            30 => Some(Key::A),
            31 => Some(Key::S),
            32 => Some(Key::D),
            33 => Some(Key::F),
            34 => Some(Key::G),
            35 => Some(Key::H),
            36 => Some(Key::J),
            37 => Some(Key::K),
            38 => Some(Key::L),
            39 => Some(Key::Semicolon),
            40 => Some(Key::Apostrophe),
            41 => Some(Key::Backtick),
            42 => Some(Key::LShift),
            43 => Some(Key::Backslash),
            44 => Some(Key::Z),
            45 => Some(Key::X),
            46 => Some(Key::C),
            47 => Some(Key::V),
            48 => Some(Key::B),
            49 => Some(Key::N),
            50 => Some(Key::M),
            51 => Some(Key::Comma),
            52 => Some(Key::Dot),
            53 => Some(Key::Slash),
            54 => Some(Key::RShift),
            56 => Some(Key::LAlt),
            57 => Some(Key::Space),
            58 => Some(Key::CapsLock),
            59 => Some(Key::F1),
            60 => Some(Key::F2),
            61 => Some(Key::F3),
            62 => Some(Key::F4),
            63 => Some(Key::F5),
            64 => Some(Key::F6),
            65 => Some(Key::F7),
            66 => Some(Key::F8),
            67 => Some(Key::F9),
            68 => Some(Key::F10),
            87 => Some(Key::F11),
            88 => Some(Key::F12),
            97 => Some(Key::RCtrl),
            100 => Some(Key::RAlt),
            102 => Some(Key::Home),
            103 => Some(Key::Up),
            104 => Some(Key::PageUp),
            105 => Some(Key::Left),
            106 => Some(Key::Right),
            107 => Some(Key::End),
            108 => Some(Key::Down),
            109 => Some(Key::PageDown),
            110 => Some(Key::Insert),
            111 => Some(Key::Delete),
            125 => Some(Key::LMeta),
            126 => Some(Key::RMeta),
            _ => None,
        }
    }

    /// Modifier keys (Ctrl/Alt/Shift/Meta) never auto-repeat — holding them
    /// must not synthesize a stream of key events.
    pub fn is_modifier(self) -> bool {
        matches!(
            self,
            Key::LCtrl
                | Key::RCtrl
                | Key::LAlt
                | Key::RAlt
                | Key::LShift
                | Key::RShift
                | Key::LMeta
                | Key::RMeta
        )
    }
}

/// `wl_seat` capability bits (from the `capabilities` event bitmask).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum SeatCapability {
    Pointer = 1,
    Keyboard = 2,
    Touch = 4,
}

impl SeatCapability {
    /// Whether `bitmask` (from the capabilities event) includes this capability.
    pub fn in_mask(self, bitmask: u32) -> bool {
        bitmask & (self as u32) != 0
    }
}

/// Keyboard modifier state, decoded from `wl_keyboard.modifiers`.
///
/// The depressed/latched/locked fields are bitmasks of the XKB modifier
/// indices (not Linux evdev keycodes). We track the union — any modifier
/// that's depressed, latched, *or* locked is considered active.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct Modifiers {
    /// Raw depressed modifier bitmask.
    pub depressed: u32,
    /// Raw latched modifier bitmask (e.g. one-shot Shift).
    pub latched: u32,
    /// Raw locked modifier bitmask (e.g. Caps Lock).
    pub locked: u32,
    /// Layout group index.
    pub group: u32,
}

/// A `wl_fixed` value: signed 24.8 fixed-point. Pointer coordinates arrive in
/// this format; the newtype keeps them from being read as plain pixels by
/// accident — you must go through [`to_pixels`](Fixed::to_pixels).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Fixed(pub i32);

impl Fixed {
    /// Whole pixels, truncating the 8-bit fraction.
    pub fn to_pixels(self) -> i32 {
        self.0 / 256
    }
}

/// A `wl_shm` pixel format. Discriminant is the on-wire format code.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum Format {
    /// 32-bit ARGB, alpha honoured.
    Argb8888 = 0,
    /// 32-bit RGB, the high byte ignored. What we draw into.
    Xrgb8888 = 1,
}

impl Format {
    pub fn code(self) -> u32 {
        self as u32
    }
}

/// A global interface we bind by name, with the maximum version we support.
pub struct Interface {
    pub name: &'static str,
    pub version: u32,
}

pub const WL_COMPOSITOR: Interface = Interface {
    name: "wl_compositor",
    version: 4,
};
pub const WL_SHM: Interface = Interface {
    name: "wl_shm",
    version: 1,
};
pub const XDG_WM_BASE: Interface = Interface {
    name: "xdg_wm_base",
    version: 1,
};
pub const WL_SEAT: Interface = Interface {
    name: "wl_seat",
    // v4+ so the keyboard delivers `repeat_info` (key-repeat rate/delay). The
    // keyboard object inherits the seat's bound version.
    version: 4,
};
pub const WL_SUBCOMPOSITOR: Interface = Interface {
    name: "wl_subcompositor",
    version: 1,
};
pub const ZWP_LINUX_DMABUF_V1: Interface = Interface {
    name: "zwp_linux_dmabuf_v1",
    // v3 adds modifier event; v4 adds feedback (we skip for now).
    version: 3,
};
pub const ZXDG_DECORATION_MANAGER_V1: Interface = Interface {
    name: "zxdg_decoration_manager_v1",
    version: 1,
};
pub const WL_OUTPUT: Interface = Interface {
    name: "wl_output",
    // v2 adds `done`/`scale` events, which is the whole reason we bind this.
    version: 2,
};
pub const WL_DATA_DEVICE_MANAGER: Interface = Interface {
    name: "wl_data_device_manager",
    // v1 has create_data_source/get_data_device — all plain clipboard needs.
    version: 1,
};

requests! {
    /// `wl_subcompositor` requests — creates child surfaces (popups, menus,
    /// combo-box dropdowns) that the compositor composites together.
    SubcompositorRequest {
        /// Create a subsurface for `surface`, parented to `parent`.
        GetSubsurface = 1,
    }
}

requests! {
    /// `wl_subsurface` requests — positions and layers a child surface within
    /// its parent. Subsurfaces are a separate surface chain that commits
    /// independently or in sync with the parent.
    SubsurfaceRequest {
        /// Set the child's position relative to the parent, in surface-local
        /// pixels (before buffer scale).
        SetPosition = 0,
        /// Stack the child just above `sibling` in z-order.
        PlaceAbove = 1,
        /// Stack the child just below `sibling` in z-order.
        PlaceBelow = 2,
        /// Sync mode: the child's commits are cached until the parent commits
        /// (no partial updates — good for popups that should appear atomically).
        SetSync = 4,
        /// Desync mode: the child commits independently (good for live
        /// animation / video overlays).
        SetDesync = 5,
    }
}

// ── zwp_linux_dmabuf_v1 ──────────────────────────────────────────────────

requests! {
    /// `zwp_linux_dmabuf_v1` requests — factory for dmabuf-based wl_buffers.
    DmabufRequest {
        /// Unbind the factory (created objects remain valid).
        Destroy = 0,
        /// Create a temporary params object for building a dmabuf buffer.
        CreateParams = 1,
    }
}
events! {
    /// `zwp_linux_dmabuf_v1` events.
    DmabufEvent {
        /// Advertised DRM_FORMAT code (deprecated in v4).
        Format = 0,
        /// Advertised modifier for a format (since v3, deprecated in v4).
        Modifier = 3,
    }
}

requests! {
    /// `zwp_linux_buffer_params_v1` requests — parameters for creating a
    /// dmabuf-based wl_buffer.
    DmabufParamsRequest {
        /// Delete this object, used or not.
        Destroy = 0,
        /// Add one dmabuf fd to the set. `fd` travels out-of-band via SCM_RIGHTS.
        Add = 1,
        /// Request asynchronous wl_buffer creation. New buffer id delivered via
        /// the `created` event.
        Create = 2,
        /// Request immediate wl_buffer creation. Pre-allocated `new_id`; no
        /// `created` event on success, `failed` on failure.
        CreateImmed = 3,
    }
}
events! {
    /// `zwp_linux_buffer_params_v1` events.
    DmabufParamsEvent {
        /// `Create` succeeded — carries the new wl_buffer id.
        Created = 0,
        /// Buffer creation failed (compositor can't import dmabuf).
        Failed = 1,
    }
}

/// DRM fourcc codes for dmabuf buffer creation.
pub mod drm_format {
    /// `DRM_FORMAT_XRGB8888` — 32-bit RGB, top byte unused.
    pub const XRGB8888: u32 = 0x34325258;
    /// `DRM_FORMAT_ARGB8888` — 32-bit ARGB, alpha in top byte.
    pub const ARGB8888: u32 = 0x34325241;
}

/// `DRM_FORMAT_MOD_LINEAR` — no tiling/compression.
pub const DRM_FORMAT_MOD_LINEAR_HI: u32 = 0;
pub const DRM_FORMAT_MOD_LINEAR_LO: u32 = 0;

#[cfg(test)]
mod tests {
    use super::*;

    /// Every evdev code `Key::from_code` is documented to recognise. Kept as
    /// an explicit list (rather than re-deriving it from the enum) so the
    /// test independently double-checks the match arms actually cover what
    /// the doc comment/enum promises.
    const VALID_KEY_CODES: &[u32] = &[
        1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25,
        26, 27, 28, 29, 30, 31, 32, 33, 34, 35, 36, 37, 38, 39, 40, 41, 42, 43, 44, 45, 46, 47, 48,
        49, 50, 51, 52, 53, 54, 56, 57, 58, 59, 60, 61, 62, 63, 64, 65, 66, 67, 68, 87, 88, 97,
        100, 102, 103, 104, 105, 106, 107, 108, 109, 110, 111, 125, 126,
    ];

    #[test]
    fn key_from_code_recognises_every_documented_code_and_roundtrips() {
        for &code in VALID_KEY_CODES {
            let key = Key::from_code(code)
                .unwrap_or_else(|| panic!("code {code} should decode to a Key"));
            assert_eq!(
                key as u32, code,
                "Key discriminant must equal its evdev code"
            );
        }
    }

    #[test]
    fn key_from_code_rejects_unknown_codes() {
        // A sampling of codes that fall in the gaps of the evdev keyboard
        // range (KP keys, media keys, codes past the last-known F12) plus
        // the two boundary sentinels 0 and u32::MAX.
        for code in [
            0,
            55,
            69,
            86,
            89,
            96,
            98,
            99,
            101,
            112,
            124,
            127,
            200,
            1000,
            u32::MAX,
        ] {
            assert_eq!(Key::from_code(code), None, "code {code} should not decode");
        }
    }

    #[test]
    fn key_from_code_covers_every_code_in_0_to_200_consistently() {
        // Exhaustive sweep of the low range: anything not in our explicit
        // valid-code list must come back None, and anything in it must
        // roundtrip. Catches accidental extra/missing match arms in one shot.
        for code in 0..=200u32 {
            let expected_some = VALID_KEY_CODES.contains(&code);
            assert_eq!(
                Key::from_code(code).is_some(),
                expected_some,
                "code {code} disagreed with expected validity"
            );
        }
    }

    #[test]
    fn key_is_modifier_true_only_for_the_eight_modifier_keys() {
        let modifiers = [
            Key::LCtrl,
            Key::RCtrl,
            Key::LAlt,
            Key::RAlt,
            Key::LShift,
            Key::RShift,
            Key::LMeta,
            Key::RMeta,
        ];
        for &k in &modifiers {
            assert!(k.is_modifier(), "{k:?} should be a modifier");
        }
        // A representative sample of non-modifier keys.
        for &k in &[Key::A, Key::Enter, Key::Space, Key::F1, Key::Up, Key::Esc] {
            assert!(!k.is_modifier(), "{k:?} should not be a modifier");
        }
    }

    #[test]
    fn pointer_button_from_code_known_and_unknown() {
        assert_eq!(PointerButton::from_code(0x110), Some(PointerButton::Left));
        assert_eq!(PointerButton::from_code(0x111), Some(PointerButton::Right));
        assert_eq!(PointerButton::from_code(0x112), Some(PointerButton::Middle));
        assert_eq!(PointerButton::from_code(0x113), None);
        assert_eq!(PointerButton::from_code(0), None);
    }

    #[test]
    fn button_state_from_raw_known_and_unknown() {
        assert_eq!(ButtonState::from_raw(0), Some(ButtonState::Released));
        assert_eq!(ButtonState::from_raw(1), Some(ButtonState::Pressed));
        assert_eq!(ButtonState::from_raw(2), None);
    }

    #[test]
    fn key_state_from_raw_known_and_unknown() {
        assert_eq!(KeyState::from_raw(0), Some(KeyState::Released));
        assert_eq!(KeyState::from_raw(1), Some(KeyState::Pressed));
        assert_eq!(KeyState::from_raw(2), None);
    }

    #[test]
    fn seat_capability_in_mask_checks_individual_bits() {
        let mask = SeatCapability::Pointer as u32 | SeatCapability::Touch as u32;
        assert!(SeatCapability::Pointer.in_mask(mask));
        assert!(!SeatCapability::Keyboard.in_mask(mask));
        assert!(SeatCapability::Touch.in_mask(mask));
        assert!(!SeatCapability::Pointer.in_mask(0));
    }

    #[test]
    fn fixed_to_pixels_truncates_toward_zero() {
        assert_eq!(Fixed(256).to_pixels(), 1);
        assert_eq!(Fixed(0).to_pixels(), 0);
        assert_eq!(Fixed(255).to_pixels(), 0);
        assert_eq!(Fixed(-256).to_pixels(), -1);
        // Truncation, not floor: -1 pixel worth minus a fraction still
        // truncates toward zero under integer division.
        assert_eq!(Fixed(-255).to_pixels(), 0);
        assert_eq!(Fixed(1024).to_pixels(), 4);
    }

    #[test]
    fn format_code_matches_wire_value() {
        assert_eq!(Format::Argb8888.code(), 0);
        assert_eq!(Format::Xrgb8888.code(), 1);
    }
}
