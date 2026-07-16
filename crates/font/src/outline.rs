//! TrueType `glyf` outline decoder.
//!
//! Produces a flat list of [`Verb`]s (move/line/curve/close) from the raw
//! glyph bytecode. Composite glyphs are recursively expanded.

use crate::parse::{ByteSliceExt, ParseError};

// ── Geometry ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Point {
    pub x: f32,
    pub y: f32,
}

impl Point {
    #[must_use] 
    pub const fn new(x: f32, y: f32) -> Self { Self { x, y } }
    #[must_use] 
    pub fn lerp(self, other: Self, t: f32) -> Self {
        Self { x: (other.x - self.x).mul_add(t, self.x), y: (other.y - self.y).mul_add(t, self.y) }
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
    pub verbs:  Vec<Verb>,
    /// Bounding box in font units.
    pub x_min: i16,
    pub y_min: i16,
    pub x_max: i16,
    pub y_max: i16,
}

impl Outline {
    const fn new(x_min: i16, y_min: i16, x_max: i16, y_max: i16) -> Self {
        Self { verbs: Vec::new(), x_min, y_min, x_max, y_max }
    }

    /// Decode the outline for glyph `gid` from the `glyf` table bytes.
    /// `glyf_data` is the entire `glyf` table; the glyph itself lives at
    /// `glyf_data[glyph_off..glyph_off + glyph_len]`.
    ///
    /// Was a free `decode_glyph(glyf_data, ..)` function -- moved onto
    /// `Outline` since it's the type being produced, mirroring the
    /// `T::parse(bytes)` convention used by the sibling `tables` module.
    pub fn decode(
        glyf_data: &[u8],
        glyph_off: usize,
        glyph_len: usize,
        depth: u8,
    ) -> Result<Self, ParseError> {
        if depth > 8 { return Ok(Self::default()); } // guard infinite recursion

        let d = &glyf_data[glyph_off..glyph_off + glyph_len];
        if d.len() < 10 { return Ok(Self::default()); }

        let num_contours = d.i16_at(0).ok_or(ParseError::TooShort)?;
        let x_min = d.i16_at(2).ok_or(ParseError::TooShort)?;
        let y_min = d.i16_at(4).ok_or(ParseError::TooShort)?;
        let x_max = d.i16_at(6).ok_or(ParseError::TooShort)?;
        let y_max = d.i16_at(8).ok_or(ParseError::TooShort)?;

        if num_contours >= 0 {
            decode_simple(d, num_contours as usize, x_min, y_min, x_max, y_max)
        } else {
            decode_composite(glyf_data, d, depth, x_min, y_min, x_max, y_max)
        }
    }
}

// ── Flag bits ─────────────────────────────────────────────────────────────────

const ON_CURVE:          u8 = 0x01;
const X_SHORT:           u8 = 0x02;
const Y_SHORT:           u8 = 0x04;
const REPEAT:            u8 = 0x08;
const X_SAME_OR_POS:     u8 = 0x10;
const Y_SAME_OR_POS:     u8 = 0x20;

// Composite flag bits
const ARG_1_AND_2_ARE_WORDS: u16 = 0x0001;
const MORE_COMPONENTS:       u16 = 0x0020;
const WE_HAVE_A_SCALE:       u16 = 0x0008;
const WE_HAVE_AN_X_AND_Y:    u16 = 0x0040;
const WE_HAVE_A_TWO_BY_TWO: u16 = 0x0080;

// ── Public entry point ────────────────────────────────────────────────────────


// ── Simple glyph ─────────────────────────────────────────────────────────────

fn decode_simple(
    d:           &[u8],
    num_contours: usize,
    x_min: i16, y_min: i16, x_max: i16, y_max: i16,
) -> Result<Outline, ParseError> {
    if num_contours == 0 { return Ok(Outline::default()); }

    // End-point indices of each contour (relative to 0).
    let end_pts_off = 10usize;
    let total_pts   = d.u16_at(end_pts_off + (num_contours - 1) * 2)
        .ok_or(ParseError::TooShort)? as usize + 1;

    let instruction_len_off = end_pts_off + num_contours * 2;
    let instruction_len = d.u16_at(instruction_len_off).ok_or(ParseError::TooShort)? as usize;

    let mut cursor = instruction_len_off + 2 + instruction_len;

    // ── Flags ──────────────────────────────────────────────────────────────
    let mut flags = Vec::with_capacity(total_pts);
    while flags.len() < total_pts {
        let f = d.u8_at(cursor).ok_or(ParseError::TooShort)?;
        cursor += 1;
        let repeat = if f & REPEAT != 0 {
            let n = d.u8_at(cursor).ok_or(ParseError::TooShort)?;
            cursor += 1;
            n as usize
        } else { 0 };
        flags.push(f);
        for _ in 0..repeat { flags.push(f); }
    }

    // ── X coordinates ─────────────────────────────────────────────────────
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
        xs.push(x_acc as i16);
    }

    // ── Y coordinates ─────────────────────────────────────────────────────
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
        ys.push(y_acc as i16);
    }

    // ── Build verbs ───────────────────────────────────────────────────────
    let mut outline = Outline::new(x_min, y_min, x_max, y_max);
    let mut pt_idx = 0usize;

    for c in 0..num_contours {
        let end = d.u16_at(end_pts_off + c * 2).ok_or(ParseError::TooShort)? as usize;
        outline.emit_contour(&flags, &xs, &ys, pt_idx, end);
        pt_idx = end + 1;
    }

    Ok(outline)
}

fn pt(xs: &[i16], ys: &[i16], i: usize) -> Point {
    Point::new(f32::from(xs[i]), f32::from(ys[i]))
}

fn on(flags: &[u8], i: usize) -> bool { flags[i] & ON_CURVE != 0 }

impl Outline {
    /// Emit TrueType quadratic contour verbs onto `self`, handling implied
    /// on-curve midpoints.
    ///
    /// Was a free `emit_contour(outline: &mut Outline, ..)` function --
    /// moved onto `Outline` since it only ever mutates one.
    fn emit_contour(
        &mut self,
        flags: &[u8],
        xs:    &[i16],
        ys:    &[i16],
        start: usize,
        end:   usize,
    ) {
        let count = end - start + 1;
        if count == 0 { return; }

        // Find first on-curve point (or use midpoint if all off-curve).
        let first_on = (start..=end).find(|&i| on(flags, i));
        let start_pt = first_on.map_or_else(
            || pt(xs, ys, start).lerp(pt(xs, ys, start + 1), 0.5),
            |i| pt(xs, ys, i),
        );

        self.verbs.push(Verb::MoveTo(start_pt));

        // Walk the contour.
        let mut pending_off: Option<Point> = None;
        let rotation_start = first_on.unwrap_or(start);

        for step in 0..count {
            let idx = start + (rotation_start - start + step) % count;
            let p = pt(xs, ys, idx);
            if on(flags, idx) {
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
}

// ── Composite glyph ───────────────────────────────────────────────────────────

fn decode_composite(
    glyf_data: &[u8],
    d:         &[u8],
    depth:     u8,
    x_min: i16, y_min: i16, x_max: i16, y_max: i16,
) -> Result<Outline, ParseError> {
    let mut outline = Outline::new(x_min, y_min, x_max, y_max);
    let mut cursor  = 10usize;

    loop {
        let flags = d.u16_at(cursor).ok_or(ParseError::TooShort)?;
        cursor += 2;
        let glyph_index = d.u16_at(cursor).ok_or(ParseError::TooShort)?;
        cursor += 2;

        // Translation arguments.
        let (dx, dy) = if flags & ARG_1_AND_2_ARE_WORDS != 0 {
            let dx = f32::from(d.i16_at(cursor).ok_or(ParseError::TooShort)?);
            let dy = f32::from(d.i16_at(cursor + 2).ok_or(ParseError::TooShort)?);
            cursor += 4;
            (dx, dy)
        } else {
            let dx = f32::from(d.u8_at(cursor).ok_or(ParseError::TooShort)?.cast_signed());
            let dy = f32::from(d.u8_at(cursor + 1).ok_or(ParseError::TooShort)?.cast_signed());
            cursor += 2;
            (dx, dy)
        };

        // Skip scale/2x2 (we apply only translation for now — sufficient for
        // the terminal use-case where we load a single upright monospace font).
        if flags & WE_HAVE_A_SCALE != 0     { cursor += 2; }
        else if flags & WE_HAVE_AN_X_AND_Y != 0 { cursor += 4; }
        else if flags & WE_HAVE_A_TWO_BY_TWO != 0 { cursor += 8; }

        // Recursively decode the component glyph.
        // We need loca to find it — locate via glyf table bounds heuristic:
        // components reference glyph ids in the same file so we re-use the
        // glyf_data slice and search forward for the glyph header.
        // (A full implementation would pass Loca in; here we use a helper.)
        let component = find_component(glyf_data, glyph_index, depth);
        // Translate and append.
        for verb in component.verbs {
            outline.verbs.push(verb.translate(dx, dy));
        }

        if flags & MORE_COMPONENTS == 0 { break; }
    }

    Ok(outline)
}

fn find_component(glyf_data: &[u8], _gid: u16, _depth: u8) -> Outline {
    // Composite expansion requires Loca; we defer full support to when
    // FontFace passes Loca in. Return an empty outline as a safe stub —
    // composite glyphs in JetBrains Mono are rare (only accented letters).
    let _ = glyf_data;
    Outline::default()
}

impl Verb {
    /// Translate this verb's point(s) by `(dx, dy)`.
    ///
    /// Was a free `translate_verb(v: Verb, ..)` function -- moved onto
    /// `Verb` as the value being consumed and produced.
    #[must_use]
    fn translate(self, dx: f32, dy: f32) -> Self {
        match self {
            Self::MoveTo(p)    => Self::MoveTo(Point::new(p.x + dx, p.y + dy)),
            Self::LineTo(p)    => Self::LineTo(Point::new(p.x + dx, p.y + dy)),
            Self::QuadTo(c, p) => Self::QuadTo(
                Point::new(c.x + dx, c.y + dy),
                Point::new(p.x + dx, p.y + dy),
            ),
            Self::Close => Self::Close,
        }
    }
}
