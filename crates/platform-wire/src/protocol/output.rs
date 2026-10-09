use super::Interface;

pub const WL_OUTPUT: Interface = Interface {
    name: "wl_output",
    // v4 adds the stable `name` event used by per-output appearance rules.
    version: 4,
};

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
        /// 2 = `HiDPI` @ 2x, ...). This is the one we actually read.
        Scale = 3,
        /// Stable compositor-provided connector name, introduced by v4.
        /// Wallpaper rules use this value for swaybg-compatible `--output`.
        Name = 4,
        /// Human-facing output description, also introduced by v4.
        Description = 5,
    }
}
