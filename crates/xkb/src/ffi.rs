//! Raw `libxkbcommon` bindings, loaded at runtime via `dlopen(3)`.
//!
//! The workspace rule (see `PLAN.md`) is "no build.rs shelling out to system
//! libraries, no bindgen" — so instead of linking `-lxkbcommon` at build
//! time (which would need a build script to locate the `.so`/headers and
//! would make the binary refuse to start at all on a system without the
//! library), we `dlopen` it lazily, exactly like `winit`/`smithay` do via
//! the `xkbcommon-dl` crate. If it's missing, [`Lib::open`] returns `Err`
//! and callers fall back to the physical-keycode stub (see `crates/platform`).
//!
//! Only the handful of entry points we actually need are declared — this is
//! not a general xkbcommon binding.

#![allow(non_camel_case_types)]

use std::ffi::{CStr, c_char, c_int, c_void};

pub type xkb_context = c_void;
pub type xkb_keymap = c_void;
pub type xkb_state = c_void;
pub type xkb_keycode_t = u32;
pub type xkb_keysym_t = u32;
pub type xkb_mod_index_t = u32;
pub type xkb_mod_mask_t = u32;
pub type xkb_layout_index_t = u32;

/// `enum xkb_context_flags` — mirrors the platform crate's convention of a
/// real `#[repr] enum` per closed C-enum set (see e.g.
/// `platform::protocol::SeatCapability`) rather than a loose group of
/// `const`s. We only ever pass the no-flags variant.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[repr(i32)]
pub enum ContextFlags {
    NoFlags = 0,
}

/// `enum xkb_keymap_format`. `TextV1` is the only wire format Wayland
/// compositors send (`wl_keyboard.keymap`'s `format` argument is documented
/// as always `XKB_V1`).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[repr(i32)]
pub enum KeymapFormat {
    TextV1 = 1,
}

/// `enum xkb_keymap_compile_flags`.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[repr(i32)]
pub enum KeymapCompileFlags {
    NoFlags = 0,
}

/// `enum xkb_state_component` — the bits `xkb_state_update_mask` /
/// `xkb_state_mod_index_is_active` accept. We only ever test the
/// *effective* (derived) mod mask ourselves (`xkb_state_update_mask` takes
/// the raw depressed/latched/locked masks straight from
/// `wl_keyboard.modifiers` without decomposing them on our side), so that's
/// the only variant named here — same one-bit-per-variant idiom as
/// `platform::protocol::SeatCapability`.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum StateComponent {
    ModsEffective = 1 << 3,
}

type FnContextNew = unsafe extern "C" fn(ContextFlags) -> *mut xkb_context;
type FnContextUnref = unsafe extern "C" fn(*mut xkb_context);
type FnKeymapNewFromString = unsafe extern "C" fn(
    *mut xkb_context,
    *const c_char,
    KeymapFormat,
    KeymapCompileFlags,
) -> *mut xkb_keymap;
type FnKeymapUnref = unsafe extern "C" fn(*mut xkb_keymap);
type FnStateNew = unsafe extern "C" fn(*mut xkb_keymap) -> *mut xkb_state;
type FnStateUnref = unsafe extern "C" fn(*mut xkb_state);
type FnStateUpdateMask = unsafe extern "C" fn(
    *mut xkb_state,
    xkb_mod_mask_t,
    xkb_mod_mask_t,
    xkb_mod_mask_t,
    xkb_layout_index_t,
    xkb_layout_index_t,
    xkb_layout_index_t,
) -> u32;
type FnStateKeyGetOneSym = unsafe extern "C" fn(*mut xkb_state, xkb_keycode_t) -> xkb_keysym_t;
type FnStateKeyGetUtf32 = unsafe extern "C" fn(*mut xkb_state, xkb_keycode_t) -> u32;
type FnStateModIndexIsActive =
    unsafe extern "C" fn(*mut xkb_state, xkb_mod_index_t, StateComponent) -> c_int;
type FnKeymapModGetIndex = unsafe extern "C" fn(*mut xkb_keymap, *const c_char) -> xkb_mod_index_t;

/// The subset of `libxkbcommon`'s C ABI we call, resolved once via `dlopen`
/// + `dlsym` and cached for the process lifetime.
pub struct Lib {
    pub context_new: FnContextNew,
    pub context_unref: FnContextUnref,
    pub keymap_new_from_string: FnKeymapNewFromString,
    pub keymap_unref: FnKeymapUnref,
    pub state_new: FnStateNew,
    pub state_unref: FnStateUnref,
    pub state_update_mask: FnStateUpdateMask,
    pub state_key_get_one_sym: FnStateKeyGetOneSym,
    pub state_key_get_utf32: FnStateKeyGetUtf32,
    pub state_mod_index_is_active: FnStateModIndexIsActive,
    pub keymap_mod_get_index: FnKeymapModGetIndex,
    // Kept alive for the process lifetime -- every function pointer above
    // points into this mapping, so dropping it would dangle them. We never
    // actually dlclose: `Lib` lives in a `OnceLock` (see `open()`), matching
    // how `xkbcommon-dl` itself never closes the library either.
    _handle: *mut c_void,
}

// SAFETY: the resolved function pointers are plain C functions with no
// thread-local state of their own; libxkbcommon's context/keymap/state
// objects are what carry state, and each is confined to one owner in our
// wrapper (`Keymap`/`State` are not `Sync`). `Lib` itself just holds function
// pointers + an opaque handle, none of which point at thread-local data.
unsafe impl Send for Lib {}
unsafe impl Sync for Lib {}

const RTLD_NOW: c_int = 2;

unsafe extern "C" {
    fn dlopen(filename: *const c_char, flag: c_int) -> *mut c_void;
    fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
    fn dlerror() -> *mut c_char;
}

/// Fetch one symbol by name, transmuting to the caller-specified function
/// pointer type. Returns `None` (after draining `dlerror`) if missing.
///
/// # Safety
/// `T` must exactly match the C signature of `sym` in the loaded library.
unsafe fn sym<T: Copy>(handle: *mut c_void, name: &CStr) -> Option<T> {
    // SAFETY: `handle` is a live dlopen handle for the caller's lifetime;
    // `name` is NUL-terminated by construction (`CStr`).
    let ptr = unsafe { dlsym(handle, name.as_ptr()) };
    if ptr.is_null() {
        // SAFETY: dlerror() takes no arguments; the returned pointer (if
        // any) is a static/thread-local string libc owns, we don't free it.
        unsafe {
            dlerror();
        }
        return None;
    }
    // SAFETY: `T` is a function-pointer-sized type (guaranteed by the
    // `Copy` bound this module only ever instantiates with `unsafe extern
    // "C" fn(...)` types), and the caller-provided signature is asserted to
    // match by this function's own safety contract.
    Some(unsafe { std::mem::transmute_copy::<*mut c_void, T>(&ptr) })
}

impl Lib {
    /// `dlopen("libxkbcommon.so.0")` and resolve every symbol we need.
    ///
    /// Tries the versioned soname first (what every distro package installs,
    /// per the shared-lib ABI convention), then the unversioned
    /// `libxkbcommon.so` (dev-package symlink, present on some systems).
    pub fn open() -> Result<Self, String> {
        let handle = ["libxkbcommon.so.0\0", "libxkbcommon.so\0"]
            .into_iter()
            .find_map(|name| {
                // SAFETY: `name` is a static NUL-terminated string literal.
                let h = unsafe { dlopen(name.as_ptr().cast(), RTLD_NOW) };
                if h.is_null() { None } else { Some(h) }
            })
            .ok_or_else(|| "dlopen(libxkbcommon.so.0) failed: library not found".to_string())?;

        macro_rules! resolve {
            ($name:literal) => {{
                let cname = concat!($name, "\0");
                // SAFETY: `handle` is the live handle just opened above;
                // `cname` is NUL-terminated by construction.
                unsafe {
                    sym(
                        handle,
                        CStr::from_bytes_with_nul_unchecked(cname.as_bytes()),
                    )
                }
                .ok_or_else(|| format!("libxkbcommon: missing symbol `{}`", $name))?
            }};
        }

        Ok(Self {
            context_new: resolve!("xkb_context_new"),
            context_unref: resolve!("xkb_context_unref"),
            keymap_new_from_string: resolve!("xkb_keymap_new_from_string"),
            keymap_unref: resolve!("xkb_keymap_unref"),
            state_new: resolve!("xkb_state_new"),
            state_unref: resolve!("xkb_state_unref"),
            state_update_mask: resolve!("xkb_state_update_mask"),
            state_key_get_one_sym: resolve!("xkb_state_key_get_one_sym"),
            state_key_get_utf32: resolve!("xkb_state_key_get_utf32"),
            state_mod_index_is_active: resolve!("xkb_state_mod_index_is_active"),
            keymap_mod_get_index: resolve!("xkb_keymap_mod_get_index"),
            _handle: handle,
        })
    }
}
