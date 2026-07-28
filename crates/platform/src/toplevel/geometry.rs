//! Size bookkeeping for an xdg-shell toplevel: the logical/physical split,
//! and coalescing a drag's flood of configures into one resize per frame.
//!
//! # Why this is its own module
//!
//! In `TermWindow` this logic is smeared across three places -- a `scale`
//! field, a `pending_resize: Option<(i32, i32)>`, and an
//! `apply_pending_resize` that also touches the shm pool and the grid. That
//! makes the *arithmetic* untestable without a compositor, even though it is
//! pure integer math with genuinely tricky edges (xdg sizes are logical,
//! buffers are physical, and a zero from the compositor means "you choose").
//!
//! Here it is a plain value type with no Wayland in sight, so every edge can
//! be pinned by a unit test.

/// A size in **logical** (surface-local) pixels -- the coordinate space
/// xdg-shell always speaks in, regardless of buffer scale.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LogicalSize {
    pub width: i32,
    pub height: i32,
}

/// A size in **physical** pixels -- what our shm buffers are actually
/// allocated in.
///
/// A distinct type from [`LogicalSize`] rather than a bare `(i32, i32)`
/// because mixing the two is silent and catastrophic on a `HiDPI` display: the
/// window renders at half size into a quarter of its buffer, and nothing
/// type-checks it for you.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PhysicalSize {
    pub width: i32,
    pub height: i32,
}

impl PhysicalSize {
    /// Width as a `usize`, with a negative value read as zero.
    ///
    /// `Geometry` clamps sizes non-negative, but this type is public and
    /// constructible by anyone, so the conversion refuses to reinterpret a
    /// negative as an enormous positive -- which is how a bad size becomes
    /// a bad allocation rather than an honest empty one.
    #[must_use]
    pub fn width_usize(&self) -> usize {
        // `try_from` rather than a guarded `as`: the conversion itself
        // rejects negatives, so there is no cast that could be wrong if the
        // guard were later edited away.
        usize::try_from(self.width).unwrap_or(0)
    }

    /// Height as a `usize`, with a negative value read as zero.
    #[must_use]
    pub fn height_usize(&self) -> usize {
        usize::try_from(self.height).unwrap_or(0)
    }

    /// Total pixel count.
    #[must_use]
    pub fn pixel_count(&self) -> usize {
        self.width_usize().saturating_mul(self.height_usize())
    }

    /// Bytes needed for an ARGB/XRGB buffer of this size, 4 bytes per pixel.
    ///
    /// Saturating throughout: a wrap here would hand back a *small* byte
    /// count for a huge window, and the renderer would then write past the
    /// allocation. Saturating turns that into a failed allocation instead.
    #[must_use]
    pub fn byte_len(&self) -> usize {
        self.pixel_count().saturating_mul(4)
    }

    /// Bytes per row.
    #[must_use]
    pub const fn stride(&self) -> i32 {
        self.width.saturating_mul(4)
    }
}

/// Tracks a toplevel's current size, its output scale, and any size the
/// compositor has proposed but that we haven't acted on yet.
///
/// The scale is fixed for the window's lifetime, matching `TermWindow`:
/// Wayland has no live output-scale-change event we handle, so a runtime DPI
/// change would need a `wl_surface.enter` lookup we don't do.
#[derive(Debug, Clone, Copy)]
pub struct Geometry {
    /// Current committed size, in physical pixels.
    current: PhysicalSize,
    /// Buffer scale (1 = normal DPI, 2 = `HiDPI` @2x).
    scale: i32,
    /// Smallest size we will ever shrink to, in physical pixels. A window
    /// of zero area has no valid shm buffer, and several compositors will
    /// happily propose one while a user drags a corner past the edge.
    min: PhysicalSize,
    /// Latest proposal from `xdg_toplevel.configure`, not yet applied. A
    /// resize drag emits these dozens of times a second, so we keep only the
    /// newest and apply it once per frame.
    pending: Option<PhysicalSize>,
}

impl Geometry {
    /// A geometry starting at `initial` physical pixels on an output of
    /// `scale`.
    ///
    /// `scale` is clamped to at least 1: a compositor that reports 0 (or a
    /// probe that failed) must not make every subsequent size collapse to
    /// nothing.
    #[must_use]
    pub const fn new(initial: PhysicalSize, scale: i32) -> Self {
        Self {
            current: initial,
            scale: if scale < 1 { 1 } else { scale },
            min: PhysicalSize {
                width: 1,
                height: 1,
            },
            pending: None,
        }
    }

    /// Set the smallest size this window may be resized to, in physical
    /// pixels.
    #[must_use]
    pub const fn with_min(mut self, min: PhysicalSize) -> Self {
        self.min = PhysicalSize {
            width: if min.width < 1 { 1 } else { min.width },
            height: if min.height < 1 { 1 } else { min.height },
        };
        self
    }

    /// The committed physical size.
    #[must_use]
    pub const fn physical(&self) -> PhysicalSize {
        self.current
    }

    /// The committed size in logical pixels, for requests that speak
    /// surface-local coordinates.
    #[must_use]
    pub const fn logical(&self) -> LogicalSize {
        LogicalSize {
            width: self.current.width / self.scale,
            height: self.current.height / self.scale,
        }
    }

    /// The output buffer scale.
    #[must_use]
    pub const fn scale(&self) -> i32 {
        self.scale
    }

    /// Convert a logical size from the compositor into physical pixels.
    #[must_use]
    pub const fn to_physical(&self, logical: LogicalSize) -> PhysicalSize {
        PhysicalSize {
            width: logical.width * self.scale,
            height: logical.height * self.scale,
        }
    }

    /// Record a size proposed by `xdg_toplevel.configure`.
    ///
    /// A zero width or height is the compositor saying "pick your own size
    /// for that axis" -- it is **not** a request to collapse. We answer by
    /// keeping the current extent on that axis, which is what every real
    /// toolkit does; treating it literally produces a zero-area buffer and
    /// an immediate protocol error.
    pub const fn propose(&mut self, logical: LogicalSize) {
        let mut physical = self.to_physical(logical);
        if logical.width <= 0 {
            physical.width = self.current.width;
        }
        if logical.height <= 0 {
            physical.height = self.current.height;
        }
        if physical.width < self.min.width {
            physical.width = self.min.width;
        }
        if physical.height < self.min.height {
            physical.height = self.min.height;
        }
        // A proposal identical to the committed size is dropped rather than
        // queued: compositors re-send configure for state changes (focus,
        // tiling) that don't alter the size, and reallocating the shm pool
        // for those would make every focus change flicker.
        if physical.width == self.current.width && physical.height == self.current.height {
            self.pending = None;
            return;
        }
        self.pending = Some(physical);
    }

    /// Whether a proposal is waiting to be applied.
    #[must_use]
    pub const fn has_pending(&self) -> bool {
        self.pending.is_some()
    }

    /// Take the pending proposal and make it current, if there is one.
    ///
    /// Returns the new size only when it actually changed, so the caller can
    /// use `Some` as its "reallocate the buffers" signal without a second
    /// comparison.
    pub const fn apply_pending(&mut self) -> Option<PhysicalSize> {
        let Some(next) = self.pending.take() else {
            return None;
        };
        if next.width == self.current.width && next.height == self.current.height {
            return None;
        }
        self.current = next;
        Some(next)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 800x600 window at scale 1.
    fn geom() -> Geometry {
        Geometry::new(
            PhysicalSize {
                width: 800,
                height: 600,
            },
            1,
        )
    }

    #[test]
    fn a_fresh_geometry_has_nothing_pending() {
        assert!(!geom().has_pending());
    }

    #[test]
    fn logical_and_physical_agree_at_scale_one() {
        let g = geom();
        assert_eq!(g.physical().width, 800);
        assert_eq!(g.logical().width, 800);
    }

    #[test]
    fn hidpi_doubles_physical_pixels() {
        // The whole reason the two types are distinct.
        let g = Geometry::new(
            PhysicalSize {
                width: 1600,
                height: 1200,
            },
            2,
        );
        assert_eq!(
            g.logical(),
            LogicalSize {
                width: 800,
                height: 600
            }
        );
        assert_eq!(
            g.to_physical(LogicalSize {
                width: 400,
                height: 300
            }),
            PhysicalSize {
                width: 800,
                height: 600
            }
        );
    }

    #[test]
    fn a_zero_scale_is_clamped_so_sizes_do_not_collapse() {
        // A failed output-scale probe must not divide everything by zero.
        let g = Geometry::new(
            PhysicalSize {
                width: 800,
                height: 600,
            },
            0,
        );
        assert_eq!(g.scale(), 1);
        assert_eq!(g.logical().width, 800);
    }

    #[test]
    fn a_proposal_is_queued_not_applied_immediately() {
        let mut g = geom();
        g.propose(LogicalSize {
            width: 1024,
            height: 768,
        });
        assert!(g.has_pending());
        // Still the old size until the frame applies it.
        assert_eq!(g.physical().width, 800);
    }

    #[test]
    fn applying_a_proposal_commits_it() {
        let mut g = geom();
        g.propose(LogicalSize {
            width: 1024,
            height: 768,
        });
        assert_eq!(
            g.apply_pending(),
            Some(PhysicalSize {
                width: 1024,
                height: 768
            })
        );
        assert_eq!(g.physical().width, 1024);
        assert!(!g.has_pending());
    }

    #[test]
    fn a_drag_storm_collapses_to_the_last_proposal() {
        // The reason `pending` is an Option and not a queue: a resize drag
        // emits dozens of configures per frame and we must allocate once.
        let mut g = geom();
        for w in 801..=900 {
            g.propose(LogicalSize {
                width: w,
                height: 600,
            });
        }
        assert_eq!(
            g.apply_pending(),
            Some(PhysicalSize {
                width: 900,
                height: 600
            })
        );
        assert!(!g.has_pending());
    }

    #[test]
    fn applying_nothing_yields_none() {
        let mut g = geom();
        assert_eq!(g.apply_pending(), None);
    }

    #[test]
    fn a_zero_axis_means_choose_your_own_not_collapse() {
        // Compositors send 0 to mean "you decide". Taken literally this
        // makes a zero-area buffer and an instant protocol error.
        let mut g = geom();
        g.propose(LogicalSize {
            width: 0,
            height: 0,
        });
        assert!(!g.has_pending(), "0x0 must not resize us at all");
    }

    #[test]
    fn one_zero_axis_keeps_that_axis_and_honours_the_other() {
        let mut g = geom();
        g.propose(LogicalSize {
            width: 0,
            height: 900,
        });
        assert_eq!(
            g.apply_pending(),
            Some(PhysicalSize {
                width: 800,
                height: 900
            })
        );
    }

    #[test]
    fn a_negative_size_is_treated_like_zero() {
        let mut g = geom();
        g.propose(LogicalSize {
            width: -50,
            height: 600,
        });
        assert!(!g.has_pending());
    }

    #[test]
    fn a_redundant_proposal_does_not_queue_a_resize() {
        // Compositors re-send configure on focus/tiling changes with an
        // unchanged size; reallocating for those would flicker.
        let mut g = geom();
        g.propose(LogicalSize {
            width: 800,
            height: 600,
        });
        assert!(!g.has_pending());
    }

    #[test]
    fn a_proposal_that_cancels_an_earlier_one_clears_the_pending_resize() {
        let mut g = geom();
        g.propose(LogicalSize {
            width: 1024,
            height: 768,
        });
        assert!(g.has_pending());
        // The user drags back to exactly where they started.
        g.propose(LogicalSize {
            width: 800,
            height: 600,
        });
        assert!(!g.has_pending());
        assert_eq!(g.apply_pending(), None);
    }

    #[test]
    fn a_minimum_size_is_enforced() {
        let mut g = geom().with_min(PhysicalSize {
            width: 200,
            height: 150,
        });
        g.propose(LogicalSize {
            width: 10,
            height: 10,
        });
        assert_eq!(
            g.apply_pending(),
            Some(PhysicalSize {
                width: 200,
                height: 150
            })
        );
    }

    #[test]
    fn a_minimum_below_one_pixel_is_clamped() {
        let mut g = geom().with_min(PhysicalSize {
            width: 0,
            height: -5,
        });
        g.propose(LogicalSize {
            width: 1,
            height: 1,
        });
        let applied = g.apply_pending().expect("resize");
        assert!(applied.width >= 1 && applied.height >= 1);
    }

    #[test]
    fn buffer_arithmetic_matches_four_bytes_per_pixel() {
        let s = PhysicalSize {
            width: 100,
            height: 50,
        };
        assert_eq!(s.stride(), 400);
        assert_eq!(s.pixel_count(), 5_000);
        assert_eq!(s.byte_len(), 20_000);
    }

    #[test]
    fn a_negative_size_reads_as_empty_not_enormous() {
        // The dangerous conversion: `-1 as usize` is `usize::MAX`, which
        // would turn a nonsense size into a catastrophic allocation.
        let s = PhysicalSize {
            width: -1,
            height: 100,
        };
        assert_eq!(s.width_usize(), 0);
        assert_eq!(s.pixel_count(), 0);
        assert_eq!(s.byte_len(), 0);
    }

    #[test]
    fn a_pathological_size_never_wraps_to_a_small_allocation() {
        // The danger is a *wrap*, not the magnitude: a huge window whose
        // byte count silently became small would produce an allocation the
        // renderer then writes past -- a heap overflow rather than an
        // honest failed allocation. On 64-bit this product happens to fit,
        // so assert the invariant (stays enormous) rather than a specific
        // saturated constant.
        let s = PhysicalSize {
            width: i32::MAX,
            height: i32::MAX,
        };
        assert!(
            s.byte_len() >= (i32::MAX as usize),
            "byte_len must never wrap below the width alone"
        );
    }
}
