//! Safe wrapper over `libxkbcommon`, driven by the `wl_keyboard.keymap`
//! blob every Wayland compositor sends.
//!
//! ## Why this exists
//!
//! `crates/platform` used to map raw Linux evdev scancodes straight to
//! logical keys (`KEY_CAPSLOCK` always meant "`CapsLock`", full stop). That's
//! wrong: compositors like niri let users **remap** the keyboard by
//! compiling a custom XKB keymap (`input.keyboard.xkb` in niri's config —
//! e.g. swapping Esc and `CapsLock`) and shipping it to every client over
//! exactly the `wl_keyboard.keymap` event we were throwing away. Without
//! parsing that blob we always saw the physical key, never the user's
//! remapped one — invisible to every app running in the terminal, most
//! noticeable in TUIs (nvim) that lean on Esc/CapsLock.
//!
//! ## What this crate does
//!
//! 1. [`Translator::from_keymap_fd`] takes the `(fd, size)` pair from
//!    `wl_keyboard.keymap`, `mmap`s it read-only, and hands the text to
//!    `xkb_keymap_new_from_string`.
//! 2. [`Translator::update_mask`] feeds `wl_keyboard.modifiers`
//!    (`depressed`/`latched`/`locked`/`group`) straight through to
//!    `xkb_state_update_mask` — no bit-twiddling on our side; XKB owns the
//!    modifier semantics.
//! 3. [`Translator::keysym`] maps `(evdev keycode, current state)` →
//!    keysym, exactly as `xkb_state_key_get_one_sym` says it should look
//!    *right now*, remaps and all.
//!
//! Everything is behind `dlopen` (see [`ffi`]) — a system without
//! `libxkbcommon.so` (unusual on a Wayland box, but not impossible) gets a
//! constructor error, and the caller falls back to the physical-keycode
//! path rather than failing to start.

mod ffi;

use std::ffi::c_char;
use std::os::fd::{AsRawFd, BorrowedFd};
use std::ptr::NonNull;

/// Any failure standing up the XKB translation path.
#[derive(Debug)]
pub enum XkbError {
    /// `dlopen`/`dlsym` couldn't find `libxkbcommon` or a symbol in it.
    LibraryUnavailable(String),
    /// `mmap`ing the compositor's keymap fd failed.
    Mmap(std::io::Error),
    /// The keymap text wasn't valid UTF-8 (the wire format is meant to be
    /// ASCII/UTF-8 XKB text, so this means a malformed compositor payload).
    InvalidUtf8,
    /// `xkb_context_new` / `xkb_keymap_new_from_string` / `xkb_state_new`
    /// returned `NULL` — the keymap text itself failed to compile.
    CompileFailed(&'static str),
}

impl std::fmt::Display for XkbError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::LibraryUnavailable(e) => write!(f, "libxkbcommon unavailable: {e}"),
            Self::Mmap(e) => write!(f, "mmap of keymap fd failed: {e}"),
            Self::InvalidUtf8 => write!(f, "compositor keymap was not valid UTF-8"),
            Self::CompileFailed(what) => write!(f, "{what} returned NULL"),
        }
    }
}

impl std::error::Error for XkbError {}

/// A `xkb_context` — the library's top-level handle. Owns nothing keymap-
/// specific; exists only because every `xkb_keymap_new_*` call needs one.
struct Context(NonNull<ffi::xkb_context>);

impl Context {
    fn new() -> Result<Self, XkbError> {
        let lib = ffi::Lib::global().map_err(XkbError::LibraryUnavailable)?;
        // SAFETY: `context_new` is a plain constructor; `NoFlags` is the
        // only value the header defines for a fresh context.
        let raw = unsafe { (lib.context_new)(ffi::ContextFlags::NoFlags) };
        NonNull::new(raw)
            .map(Self)
            .ok_or(XkbError::CompileFailed("xkb_context_new"))
    }
}

impl Drop for Context {
    fn drop(&mut self) {
        if let Ok(lib) = ffi::Lib::global() {
            // SAFETY: `self.0` was returned by `xkb_context_new` and is
            // dropped exactly once (no other owner holds this pointer).
            unsafe { (lib.context_unref)(self.0.as_ptr()) };
        }
    }
}

/// A compiled keymap: the result of parsing one `wl_keyboard.keymap` blob.
struct Keymap {
    ptr: NonNull<ffi::xkb_keymap>,
    // Kept alive: `xkb_keymap` doesn't outlive the `xkb_context` it was
    // compiled from touching internal refcounts, but our own `Context`
    // must not drop before every `Keymap`/`State` built from it does.
    _ctx: Context,
}

impl Keymap {
    fn compile(ctx: Context, text: &str) -> Result<Self, XkbError> {
        let lib = ffi::Lib::global().map_err(XkbError::LibraryUnavailable)?;
        // xkb_keymap_new_from_string wants a NUL-terminated C string.
        let mut owned = String::with_capacity(text.len() + 1);
        owned.push_str(text);
        owned.push('\0');
        // SAFETY: `owned` is NUL-terminated and lives for the whole call;
        // `ctx.0` is a live context; the format/flags constants are the
        // values the header defines for "classic XKB text, no flags".
        let raw = unsafe {
            (lib.keymap_new_from_string)(
                ctx.0.as_ptr(),
                owned.as_ptr().cast::<c_char>(),
                ffi::KeymapFormat::TextV1,
                ffi::KeymapCompileFlags::NoFlags,
            )
        };
        NonNull::new(raw)
            .map(|ptr| Self { ptr, _ctx: ctx })
            .ok_or(XkbError::CompileFailed("xkb_keymap_new_from_string"))
    }
}

impl Drop for Keymap {
    fn drop(&mut self) {
        if let Ok(lib) = ffi::Lib::global() {
            // SAFETY: `self.ptr` came from `xkb_keymap_new_from_string` and
            // is dropped exactly once.
            unsafe { (lib.keymap_unref)(self.ptr.as_ptr()) };
        }
    }
}

/// The live modifier/layout state machine for one keyboard.
struct State {
    ptr: NonNull<ffi::xkb_state>,
    _keymap: Keymap,
    /// Cached modifier indices, looked up once so modifier queries do not
    /// repeatedly resolve names through libxkbcommon on every key event.
    caps_mod_index: ffi::xkb_mod_index_t,
    ctrl_mod_index: ffi::xkb_mod_index_t,
}

/// XKB's canonical name for the `CapsLock` modifier (`XKB_MOD_NAME_CAPS`
/// in the C headers) -- resolved once per `State` and cached.
const XKB_MOD_NAME_CAPS: &str = "Lock\0";
const XKB_MOD_NAME_CTRL: &str = "Control\0";

impl State {
    fn new(keymap: Keymap) -> Result<Self, XkbError> {
        let lib = ffi::Lib::global().map_err(XkbError::LibraryUnavailable)?;
        // SAFETY: `keymap.ptr` is a live, successfully-compiled keymap.
        let raw = unsafe { (lib.state_new)(keymap.ptr.as_ptr()) };
        let ptr = NonNull::new(raw).ok_or(XkbError::CompileFailed("xkb_state_new"))?;
        // SAFETY: `keymap.ptr` is live; `XKB_MOD_NAME_CAPS` is NUL-terminated.
        let caps_mod_index = unsafe {
            (lib.keymap_mod_get_index)(
                keymap.ptr.as_ptr(),
                XKB_MOD_NAME_CAPS.as_ptr().cast::<c_char>(),
            )
        };
        // SAFETY: `keymap.ptr` is live; `XKB_MOD_NAME_CTRL` is NUL-terminated.
        let ctrl_mod_index = unsafe {
            (lib.keymap_mod_get_index)(
                keymap.ptr.as_ptr(),
                XKB_MOD_NAME_CTRL.as_ptr().cast::<c_char>(),
            )
        };
        Ok(Self {
            ptr,
            _keymap: keymap,
            caps_mod_index,
            ctrl_mod_index,
        })
    }
}

impl Drop for State {
    fn drop(&mut self) {
        if let Ok(lib) = ffi::Lib::global() {
            // SAFETY: `self.ptr` came from `xkb_state_new` and is dropped
            // exactly once.
            unsafe { (lib.state_unref)(self.ptr.as_ptr()) };
        }
    }
}

/// The evdev→XKB keycode offset. XKB keycodes are evdev codes shifted up by
/// 8 (a historical `XFree86` accommodation for extra core-protocol keycodes) —
/// documented directly in `wl_keyboard.keymap`'s protocol description.
const EVDEV_XKB_OFFSET: u32 = 8;

/// Drives one keyboard's XKB state across the `wl_keyboard.keymap` +
/// `wl_keyboard.key` + `wl_keyboard.modifiers` event trio.
///
/// One `Translator` per keyboard (in practice, per `Window` — we only ever
/// see one physical keyboard). Rebuilding it (`from_keymap_fd`) on a new
/// `Keymap` event (rare — layout hot-swaps) is cheap and always correct;
/// there's no incremental-update path in libxkbcommon for that anyway.
pub struct Translator {
    state: State,
}

impl Translator {
    /// Parse a `wl_keyboard.keymap` blob: `fd` is the ancillary-data file
    /// descriptor the event carried, `size` is its `size` argument (the
    /// mapping includes a trailing NUL the protocol guarantees).
    ///
    /// # Errors
    /// See [`XkbError`] — library missing, mmap failure, invalid UTF-8, or
    /// the keymap text failing to compile.
    pub fn from_keymap_fd(fd: BorrowedFd<'_>, size: usize) -> Result<Self, XkbError> {
        let text = Self::mmap_keymap(fd, size)?;
        let ctx = Context::new()?;
        let keymap = Keymap::compile(ctx, &text)?;
        let state = State::new(keymap)?;
        Ok(Self { state })
    }

    /// Feed a `wl_keyboard.modifiers` event straight through — no bitmask
    /// translation on our side, XKB owns what depressed/latched/locked mean
    /// for whatever keymap is loaded.
    pub fn update_mask(
        &mut self,
        depressed: u32,
        latched: u32,
        locked: u32,
        group: u32,
    ) -> Result<(), XkbError> {
        let lib = ffi::Lib::global().map_err(XkbError::LibraryUnavailable)?;
        // SAFETY: `self.state.ptr` is a live state; the mask/group args are
        // passed through verbatim from the compositor, exactly the shape
        // `xkb_state_update_mask` expects (depressed/latched/locked mods,
        // then the matching triple for the layout group).
        unsafe {
            (lib.state_update_mask)(
                self.state.ptr.as_ptr(),
                depressed,
                latched,
                locked,
                group,
                0,
                0,
            );
        }
        Ok(())
    }

    /// Resolve one evdev keycode (as `wl_keyboard.key` reports it) to the
    /// keysym XKB says it currently produces, honoring the compositor's
    /// remaps and the live modifier/layout state.
    ///
    /// Returns `0` (`XKB_KEY_NoSymbol`) for keys with no symbol at this
    /// state (e.g. an unassigned physical key).
    #[must_use]
    pub fn keysym(&self, evdev_code: u32) -> u32 {
        let Ok(lib) = ffi::Lib::global() else {
            return 0;
        };
        let keycode = evdev_code + EVDEV_XKB_OFFSET;
        // SAFETY: `self.state.ptr` is a live state; any `xkb_keycode_t` is a
        // valid argument (out-of-range codes just yield NoSymbol, per spec).
        unsafe { (lib.state_key_get_one_sym)(self.state.ptr.as_ptr(), keycode) }
    }

    /// Like [`Self::keysym`] but resolved to the Unicode codepoint XKB says
    /// this key produces as text (`0` if it produces none — a bare
    /// modifier, a dead key mid-compose, a functional key, …).
    #[must_use]
    pub fn utf32(&self, evdev_code: u32) -> u32 {
        let Ok(lib) = ffi::Lib::global() else {
            return 0;
        };
        let keycode = evdev_code + EVDEV_XKB_OFFSET;
        // SAFETY: same as `keysym` above.
        unsafe { (lib.state_key_get_utf32)(self.state.ptr.as_ptr(), keycode) }
    }

    /// Whether `CapsLock` is currently latched on, per the *effective*
    /// modifier state (so a remapped keymap where some other physical key
    /// drives `CapsLock` is still reported correctly).
    #[must_use]
    pub fn control_active(&self) -> bool {
        let Ok(lib) = ffi::Lib::global() else {
            return false;
        };
        if self.state.ctrl_mod_index == u32::MAX {
            return false;
        }
        // SAFETY: `self.state.ptr` is live; `ctrl_mod_index` was resolved
        // against this exact keymap in `State::new`.
        let active = unsafe {
            (lib.state_mod_index_is_active)(
                self.state.ptr.as_ptr(),
                self.state.ctrl_mod_index,
                ffi::StateComponent::ModsEffective,
            )
        };
        active != 0
    }

    /// Whether `CapsLock` is currently latched on, per the *effective*
    /// modifier state (so a remapped keymap where some other physical key
    /// drives `CapsLock` is still reported correctly).
    #[must_use]
    pub fn caps_lock_active(&self) -> bool {
        let Ok(lib) = ffi::Lib::global() else {
            return false;
        };
        if self.state.caps_mod_index == u32::MAX {
            return false; // XKB_MOD_INVALID: this keymap has no "Lock" mod.
        }
        // SAFETY: `self.state.ptr` is live; `caps_mod_index` was resolved
        // against this exact keymap in `State::new`.
        let active = unsafe {
            (lib.state_mod_index_is_active)(
                self.state.ptr.as_ptr(),
                self.state.caps_mod_index,
                ffi::StateComponent::ModsEffective,
            )
        };
        active != 0
    }
}

impl Translator {
    /// `mmap` the compositor's keymap fd read-only and decode it as UTF-8.
    /// A private associated fn on `Translator` (rather than a free
    /// function) since it's the first step of `Translator::from_keymap_fd`
    /// and has no other caller -- pure fd-in, string-out, so it's still
    /// easy to unit test without a real XKB context.
    fn mmap_keymap(fd: BorrowedFd<'_>, size: usize) -> Result<String, XkbError> {
        if size == 0 {
            return Err(XkbError::Mmap(std::io::Error::other("zero-size keymap")));
        }
        // SAFETY: standard read-only, private, fixed-size mapping of a fd
        // the caller owns for at least this call's duration (`BorrowedFd`).
        let ptr = unsafe {
            libc_mmap(
                std::ptr::null_mut(),
                size,
                PROT_READ,
                MAP_PRIVATE,
                fd.as_raw_fd(),
                0,
            )
        };
        if ptr == MAP_FAILED {
            return Err(XkbError::Mmap(std::io::Error::last_os_error()));
        }
        // SAFETY: `ptr` is a valid mapping of `size` bytes just established
        // above; the compositor guarantees a NUL-terminated string within
        // it, so we search for that terminator rather than trust the raw
        // slice end (matches the protocol's documented "keymap.size
        // includes the NUL").
        let text = unsafe {
            let slice = std::slice::from_raw_parts(ptr.cast::<u8>(), size);
            let nul_at = slice.iter().position(|&b| b == 0).unwrap_or(size);
            std::str::from_utf8(&slice[..nul_at])
                .map(str::to_owned)
                .map_err(|_| XkbError::InvalidUtf8)
        };
        // SAFETY: unmapping the same ptr/size pair mmap returned.
        unsafe {
            libc_munmap(ptr, size);
        }
        text
    }
}

const PROT_READ: i32 = 0x1;
const MAP_PRIVATE: i32 = 0x2;
const MAP_FAILED: *mut std::ffi::c_void = usize::MAX as *mut std::ffi::c_void;

unsafe extern "C" {
    #[link_name = "mmap"]
    fn libc_mmap(
        addr: *mut std::ffi::c_void,
        len: usize,
        prot: i32,
        flags: i32,
        fd: i32,
        offset: i64,
    ) -> *mut std::ffi::c_void;
    #[link_name = "munmap"]
    fn libc_munmap(addr: *mut std::ffi::c_void, len: usize) -> i32;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::os::fd::AsFd;

    /// A tiny but complete XKB keymap: US layout, `Esc` swapped onto
    /// physical `CapsLock` (evdev 58) and vice versa -- exactly the kind of
    /// remap niri's `input.keyboard.xkb` config produces, so tests exercise
    /// the real bug this crate fixes.
    const SWAPPED_KEYMAP: &str = r#"xkb_keymap {
  xkb_keycodes "swap" {
    minimum = 8;
    maximum = 255;
    <ESC> = 9;
    <CAPS> = 66;
  };
  xkb_types "(unnamed)" { include "complete" };
  xkb_compat "(unnamed)" { include "complete" };
  xkb_symbols "swap" {
    key <ESC> { [ Caps_Lock ] };
    key <CAPS> { [ Escape ] };
  };
  xkb_geometry "(unnamed)" { include "pc(pc105)" };
};
"#;

    unsafe extern "C" {
        fn memfd_create(name: *const c_char, flags: u32) -> i32;
    }

    fn write_keymap_fd(text: &str) -> std::os::fd::OwnedFd {
        use std::os::fd::FromRawFd;
        // memfd_create gives us a real fd backing real memory, exactly the
        // shape a `wl_keyboard.keymap` event's ancillary fd has.
        //
        // SAFETY: the name is a static NUL-terminated C string and the flags
        // are zero, which is the call's documented minimal form.
        let raw = unsafe { memfd_create(c"xkb-test".as_ptr(), 0) };
        assert!(raw >= 0, "memfd_create failed");
        // SAFETY: `raw` is a fresh, valid, unowned descriptor from the call
        // above, checked non-negative, so ownership transfers exactly once.
        let owned = unsafe { std::os::fd::OwnedFd::from_raw_fd(raw) };
        let mut file = std::fs::File::from(owned.try_clone().expect("dup memfd"));
        file.write_all(text.as_bytes()).unwrap();
        file.write_all(b"\0").unwrap();
        owned
    }

    #[test]
    fn mmap_keymap_round_trips_text() {
        let fd = write_keymap_fd("hello xkb\0padding-after-nul-ignored");
        let text =
            Translator::mmap_keymap(fd.as_fd(), "hello xkb\0padding-after-nul-ignored".len() + 1)
                .expect("mmap_keymap should succeed");
        assert_eq!(text, "hello xkb");
    }

    #[test]
    fn mmap_keymap_rejects_zero_size() {
        let fd = write_keymap_fd("x");
        assert!(matches!(
            Translator::mmap_keymap(fd.as_fd(), 0),
            Err(XkbError::Mmap(_))
        ));
    }

    #[test]
    fn translator_resolves_niri_style_swapped_esc_capslock() {
        // Skipped (not failed) on a system without libxkbcommon.so -- CI
        // environments should have it (it's a near-universal Wayland/X11
        // runtime dep), but this keeps the suite from red-lining somewhere
        // exotic that doesn't.
        // Physical Esc (evdev 1) now produces Caps_Lock's keysym...
        const XKB_KEY_ESCAPE: u32 = 0xff1b;
        const XKB_KEY_CAPS_LOCK: u32 = 0xffe5;

        let fd = write_keymap_fd(SWAPPED_KEYMAP);
        let size = SWAPPED_KEYMAP.len() + 1;
        let Ok(mut t) = Translator::from_keymap_fd(fd.as_fd(), size) else {
            eprintln!("skipping: libxkbcommon unavailable or keymap rejected");
            return;
        };
        t.update_mask(0, 0, 0, 0).unwrap();

        assert_eq!(
            t.keysym(1),
            XKB_KEY_CAPS_LOCK,
            "physical Esc must resolve to Caps_Lock under the swapped keymap"
        );
        // ...and physical CapsLock (evdev 58) now produces Escape's.
        assert_eq!(
            t.keysym(58),
            XKB_KEY_ESCAPE,
            "physical CapsLock must resolve to Escape under the swapped keymap"
        );
    }

    #[test]
    fn translator_rejects_garbage_keymap_text() {
        let fd = write_keymap_fd("not an xkb keymap at all {{{");
        let size = "not an xkb keymap at all {{{".len() + 1;
        match Translator::from_keymap_fd(fd.as_fd(), size) {
            Err(XkbError::CompileFailed(_) | XkbError::LibraryUnavailable(_)) => {}
            Err(e) => panic!("unexpected error variant: {e}"),
            Ok(_) => panic!("garbage keymap text must not compile"),
        }
    }
}
