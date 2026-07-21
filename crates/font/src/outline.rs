//! TrueType `glyf` outline decoder.
//!
//! Produces a flat list of [`Verb`]s (move/line/curve/close) from the raw
//! glyph bytecode. Composite glyphs are recursively expanded.

use crate::parse::{ByteSliceExt, ParseError};
use crate::tables::{GlyphId, Loca};

// ── Geometry ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Point {
    pub x: f32,
    pub y: f32,
}

impl Point {
    #[must_use]
    pub const fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }
    #[must_use]
    pub fn lerp(self, other: Self, t: f32) -> Self {
        Self {
            x: (other.x - self.x).mul_add(t, self.x),
            y: (other.y - self.y).mul_add(t, self.y),
        }
    }
}

/// Decoded outline verb.
#[derive(Debug, Clone, Copy)]
pub enum Verb {
    MoveTo(Point),
    LineTo(Point),
    /// Quadratic Bézier: control + endpoint.
    QuadTo(Point, Point),
    Close,
}

/// A fully-decoded glyph outline (may be empty for space/NUL).
#[derive(Debug, Default, Clone)]
pub struct Outline {
    pub verbs: Vec<Verb>,
    /// Bounding box in font units.
    pub x_min: i16,
    pub y_min: i16,
    pub x_max: i16,
    pub y_max: i16,
}

impl Outline {
    const fn new(x_min: i16, y_min: i16, x_max: i16, y_max: i16) -> Self {
        Self {
            verbs: Vec::new(),
            x_min,
            y_min,
            x_max,
            y_max,
        }
    }

    /// Decode the outline for glyph `gid` from the `glyf` table bytes.
    /// `glyf_data` is the entire `glyf` table; the glyph itself lives at
    /// `glyf_data[glyph_off..glyph_off + glyph_len]`.
    ///
    /// `loca` resolves component glyph ids for composite glyphs (e.g. an
    /// accented letter, or an `i`/`j` whose dot is a separate component from
    /// its dotless stem) — without it those glyphs would decode to nothing.
    ///
    /// Was a free `decode_glyph(glyf_data, ..)` function -- moved onto
    /// `Outline` since it's the type being produced, mirroring the
    /// `T::parse(bytes)` convention used by the sibling `tables` module.
    pub fn decode(
        glyf_data: &[u8],
        glyph_off: usize,
        glyph_len: usize,
        loca: &Loca<'_>,
        depth: u8,
    ) -> Result<Self, ParseError> {
        if depth > 8 {
            return Ok(Self::default());
        } // guard infinite recursion

        let d = &glyf_data[glyph_off..glyph_off + glyph_len];
        if d.len() < 10 {
            return Ok(Self::default());
        }

        let num_contours = d.i16_at(0).ok_or(ParseError::TooShort)?;
        let x_min = d.i16_at(2).ok_or(ParseError::TooShort)?;
        let y_min = d.i16_at(4).ok_or(ParseError::TooShort)?;
        let x_max = d.i16_at(6).ok_or(ParseError::TooShort)?;
        let y_max = d.i16_at(8).ok_or(ParseError::TooShort)?;

        if num_contours >= 0 {
            // Checked non-negative above, so the sign-preserving cast is exact.
            Self::decode_simple(
                d,
                num_contours.cast_unsigned() as usize,
                x_min,
                y_min,
                x_max,
                y_max,
            )
        } else {
            Self::decode_composite(glyf_data, d, loca, depth, x_min, y_min, x_max, y_max)
        }
    }

    /// Decode a simple (non-composite) glyph's contours.
    ///
    /// Was a free `decode_simple(d, ..)` function -- moved onto `Outline`
    /// alongside its sibling `decode`/`decode_composite`.
    fn decode_simple(
        d: &[u8],
        num_contours: usize,
        x_min: i16,
        y_min: i16,
        x_max: i16,
        y_max: i16,
    ) -> Result<Self, ParseError> {
        if num_contours == 0 {
            return Ok(Self::default());
        }

        // End-point indices of each contour (relative to 0).
        let end_pts_off = 10usize;
        let total_pts = d
            .u16_at(end_pts_off + (num_contours - 1) * 2)
            .ok_or(ParseError::TooShort)? as usize
            + 1;

        let instruction_len_off = end_pts_off + num_contours * 2;
        let instruction_len = d.u16_at(instruction_len_off).ok_or(ParseError::TooShort)? as usize;

        let mut cursor = instruction_len_off + 2 + instruction_len;

        // ── Flags ──────────────────────────────────────────────────────────
        let mut flags = Vec::with_capacity(total_pts);
        while flags.len() < total_pts {
            let f = d.u8_at(cursor).ok_or(ParseError::TooShort)?;
            cursor += 1;
            let repeat = if f & REPEAT != 0 {
                let n = d.u8_at(cursor).ok_or(ParseError::TooShort)?;
                cursor += 1;
                n as usize
            } else {
                0
            };
            flags.push(f);
            for _ in 0..repeat {
                flags.push(f);
            }
        }

        // ── X coordinates ─────────────────────────────────────────────────
        let mut xs = Vec::with_capacity(total_pts);
        let mut x_acc: i32 = 0;
        for &f in &flags {
            let dx = if f & X_SHORT != 0 {
                let v = i32::from(d.u8_at(cursor).ok_or(ParseError::TooShort)?);
                cursor += 1;
                if f & X_SAME_OR_POS != 0 { v } else { -v }
            } else if f & X_SAME_OR_POS != 0 {
                0
            } else {
                let v = i32::from(d.i16_at(cursor).ok_or(ParseError::TooShort)?);
                cursor += 2;
                v
            };
            x_acc += dx;
            // A well-formed glyf table's accumulated coordinates never leave
            // FUnits range (i16); a malformed font could overflow that, so
            // clamp defensively rather than wrapping into a bogus point.
            xs.push(i16::try_from(x_acc).unwrap_or(if x_acc > 0 { i16::MAX } else { i16::MIN }));
        }

        // ── Y coordinates ─────────────────────────────────────────────────
        let mut ys = Vec::with_capacity(total_pts);
        let mut y_acc: i32 = 0;
        for &f in &flags {
            let dy = if f & Y_SHORT != 0 {
                let v = i32::from(d.u8_at(cursor).ok_or(ParseError::TooShort)?);
                cursor += 1;
                if f & Y_SAME_OR_POS != 0 { v } else { -v }
            } else if f & Y_SAME_OR_POS != 0 {
                0
            } else {
                let v = i32::from(d.i16_at(cursor).ok_or(ParseError::TooShort)?);
                cursor += 2;
                v
            };
            y_acc += dy;
            // Same defensive clamp as the x-coordinate accumulator above.
            ys.push(i16::try_from(y_acc).unwrap_or(if y_acc > 0 { i16::MAX } else { i16::MIN }));
        }

        // ── Build verbs ───────────────────────────────────────────────────
        let mut outline = Self::new(x_min, y_min, x_max, y_max);
        let points = GlyphPoints { xs: &xs, ys: &ys };
        let mut pt_idx = 0usize;

        for c in 0..num_contours {
            let end = d.u16_at(end_pts_off + c * 2).ok_or(ParseError::TooShort)? as usize;
            outline.emit_contour(&flags, &points, pt_idx, end);
            pt_idx = end + 1;
        }

        Ok(outline)
    }

    /// Emit TrueType quadratic contour verbs onto `self`, handling implied
    /// on-curve midpoints.
    ///
    /// Was a free `emit_contour(outline: &mut Outline, ..)` function --
    /// moved onto `Outline` since it only ever mutates one.
    fn emit_contour(&mut self, flags: &[u8], points: &GlyphPoints<'_>, start: usize, end: usize) {
        let count = end - start + 1;
        if count == 0 {
            return;
        }

        // Find first on-curve point (or use midpoint if all off-curve).
        let first_on = (start..=end).find(|&i| flags.is_on_curve(i));
        let start_pt = first_on.map_or_else(
            || points.point(start).lerp(points.point(start + 1), 0.5),
            |i| points.point(i),
        );

        self.verbs.push(Verb::MoveTo(start_pt));

        // Walk the contour.
        let mut pending_off: Option<Point> = None;
        let rotation_start = first_on.unwrap_or(start);

        for step in 0..count {
            let idx = start + (rotation_start - start + step) % count;
            let p = points.point(idx);
            if flags.is_on_curve(idx) {
                if let Some(ctrl) = pending_off.take() {
                    self.verbs.push(Verb::QuadTo(ctrl, p));
                } else if step > 0 {
                    self.verbs.push(Verb::LineTo(p));
                }
            } else if let Some(prev_ctrl) = pending_off.replace(p) {
                // Two consecutive off-curve → implied midpoint.
                let mid = prev_ctrl.lerp(p, 0.5);
                self.verbs.push(Verb::QuadTo(prev_ctrl, mid));
            }
        }
        // Close: flush any remaining off-curve.
        if let Some(ctrl) = pending_off {
            self.verbs.push(Verb::QuadTo(ctrl, start_pt));
        } else {
            self.verbs.push(Verb::LineTo(start_pt));
        }
        self.verbs.push(Verb::Close);
    }

    /// Decode a composite glyph by recursively expanding + translating its
    /// component glyphs.
    ///
    /// Was a free `decode_composite(glyf_data, ..)` function -- moved onto
    /// `Outline` alongside its sibling `decode`/`decode_simple`.
    ///
    /// The four `x_min`/`y_min`/`x_max`/`y_max` bounds are the glyph's own
    /// `glyf` table header fields, read once by the caller and threaded
    /// through verbatim -- bundling them into a bounds struct would just
    /// move the four fields to a call-site literal instead of removing them.
    #[allow(clippy::too_many_arguments)]
    fn decode_composite(
        glyf_data: &[u8],
        d: &[u8],
        loca: &Loca<'_>,
        depth: u8,
        x_min: i16,
        y_min: i16,
        x_max: i16,
        y_max: i16,
    ) -> Result<Self, ParseError> {
        let mut outline = Self::new(x_min, y_min, x_max, y_max);
        let mut cursor = 10usize;

        loop {
            let flags = d.u16_at(cursor).ok_or(ParseError::TooShort)?;
            cursor += 2;
            let glyph_index = GlyphId(d.u16_at(cursor).ok_or(ParseError::TooShort)?);
            cursor += 2;

            // Translation arguments.
            let (dx, dy) = if flags & ARG_1_AND_2_ARE_WORDS != 0 {
                let dx = f32::from(d.i16_at(cursor).ok_or(ParseError::TooShort)?);
                let dy = f32::from(d.i16_at(cursor + 2).ok_or(ParseError::TooShort)?);
                cursor += 4;
                (dx, dy)
            } else {
                let dx = f32::from(d.u8_at(cursor).ok_or(ParseError::TooShort)?.cast_signed());
                let dy = f32::from(
                    d.u8_at(cursor + 1)
                        .ok_or(ParseError::TooShort)?
                        .cast_signed(),
                );
                cursor += 2;
                (dx, dy)
            };

            // Skip scale/2x2 (we apply only translation for now — sufficient for
            // the terminal use-case where we load a single upright monospace font).
            if flags & WE_HAVE_A_SCALE != 0 {
                cursor += 2;
            } else if flags & WE_HAVE_AN_X_AND_Y != 0 {
                cursor += 4;
            } else if flags & WE_HAVE_A_TWO_BY_TWO != 0 {
                cursor += 8;
            }

            // Recursively decode the component glyph via `loca`.
            let component = Self::find_component(glyf_data, loca, glyph_index, depth);
            // Translate and append.
            for verb in component.verbs {
                outline.verbs.push(verb.translate(dx, dy));
            }

            if flags & MORE_COMPONENTS == 0 {
                break;
            }
        }

        Ok(outline)
    }

    /// Resolve a composite glyph's referenced component by id.
    ///
    /// Was a free `find_component(glyf_data, ..)` function -- moved onto
    /// `Outline` since it produces one. Previously a stub returning an empty
    /// outline unconditionally (composite glyphs — accented letters, and in
    /// some fonts even plain `i`/`j` — silently vanished); now actually
    /// resolves the component's byte range via `loca` and recurses.
    fn find_component(glyf_data: &[u8], loca: &Loca<'_>, gid: GlyphId, depth: u8) -> Self {
        let Some((off, len)) = loca.glyph_range(gid) else {
            return Self::default();
        };
        Self::decode(glyf_data, off, len, loca, depth + 1).unwrap_or_default()
    }
}

// ── Flag bits ─────────────────────────────────────────────────────────────────

const ON_CURVE: u8 = 0x01;
const X_SHORT: u8 = 0x02;
const Y_SHORT: u8 = 0x04;
const REPEAT: u8 = 0x08;
const X_SAME_OR_POS: u8 = 0x10;
const Y_SAME_OR_POS: u8 = 0x20;

// Composite flag bits
const ARG_1_AND_2_ARE_WORDS: u16 = 0x0001;
const MORE_COMPONENTS: u16 = 0x0020;
const WE_HAVE_A_SCALE: u16 = 0x0008;
const WE_HAVE_AN_X_AND_Y: u16 = 0x0040;
const WE_HAVE_A_TWO_BY_TWO: u16 = 0x0080;

/// Parallel x/y coordinate arrays for a simple glyph's points, bundled so
/// point lookup reads as a method call instead of a free `pt(xs, ys, i)`
/// function.
struct GlyphPoints<'a> {
    xs: &'a [i16],
    ys: &'a [i16],
}

impl GlyphPoints<'_> {
    fn point(&self, i: usize) -> Point {
        Point::new(f32::from(self.xs[i]), f32::from(self.ys[i]))
    }
}

/// Extends glyph outline flag bytes with on-curve testing, as a method
/// instead of a free `on(flags, i)` function.
trait FlagsExt {
    fn is_on_curve(&self, i: usize) -> bool;
}

impl FlagsExt for [u8] {
    fn is_on_curve(&self, i: usize) -> bool {
        self[i] & ON_CURVE != 0
    }
}

impl Verb {
    /// Translate this verb's point(s) by `(dx, dy)`.
    ///
    /// Was a free `translate_verb(v: Verb, ..)` function -- moved onto
    /// `Verb` as the value being consumed and produced.
    #[must_use]
    fn translate(self, dx: f32, dy: f32) -> Self {
        match self {
            Self::MoveTo(p) => Self::MoveTo(Point::new(p.x + dx, p.y + dy)),
            Self::LineTo(p) => Self::LineTo(Point::new(p.x + dx, p.y + dy)),
            Self::QuadTo(c, p) => Self::QuadTo(
                Point::new(c.x + dx, c.y + dy),
                Point::new(p.x + dx, p.y + dy),
            ),
            Self::Close => Self::Close,
        }
    }
}
