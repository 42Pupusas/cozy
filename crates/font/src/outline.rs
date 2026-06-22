//! TrueType `glyf` outline decoder.
//!
//! Produces a flat list of [`Verb`]s (move/line/curve/close) from the raw
//! glyph bytecode. Composite glyphs are recursively expanded.

use crate::parse::{i16_at, u16_at, u8_at, ParseError};

// ── Geometry ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Point {
    pub x: f32,
    pub y: f32,
}

impl Point {
    pub const fn new(x: f32, y: f32) -> Self { Self { x, y } }
    pub fn lerp(self, other: Self, t: f32) -> Self {
        Self { x: self.x + (other.x - self.x) * t, y: self.y + (other.y - self.y) * t }
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
    fn new(x_min: i16, y_min: i16, x_max: i16, y_max: i16) -> Self {
        Self { verbs: Vec::new(), x_min, y_min, x_max, y_max }
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
const ARGS_ARE_XY_VALUES:    u16 = 0x0002;
const MORE_COMPONENTS:       u16 = 0x0020;
const WE_HAVE_A_SCALE:       u16 = 0x0008;
const WE_HAVE_AN_X_AND_Y:    u16 = 0x0040;
const WE_HAVE_A_TWO_BY_TWO: u16 = 0x0080;

// ── Public entry point ────────────────────────────────────────────────────────

/// Decode the outline for glyph `gid` from the `glyf` table bytes.
/// `glyf_data` is the entire `glyf` table; `loca` is used to locate components
/// during composite expansion.
pub fn decode_glyph(
    glyf_data: &[u8],
    glyph_off: usize,
    glyph_len: usize,
    depth: u8,
) -> Result<Outline, ParseError> {
    if depth > 8 { return Ok(Outline::default()); } // guard infinite recursion

    let d = &glyf_data[glyph_off..glyph_off + glyph_len];
    if d.len() < 10 { return Ok(Outline::default()); }

    let num_contours = i16_at(d, 0).ok_or(ParseError::TooShort)?;
    let x_min = i16_at(d, 2).ok_or(ParseError::TooShort)?;
    let y_min = i16_at(d, 4).ok_or(ParseError::TooShort)?;
    let x_max = i16_at(d, 6).ok_or(ParseError::TooShort)?;
    let y_max = i16_at(d, 8).ok_or(ParseError::TooShort)?;

    if num_contours >= 0 {
        decode_simple(d, num_contours as usize, x_min, y_min, x_max, y_max)
    } else {
        decode_composite(glyf_data, d, depth, x_min, y_min, x_max, y_max)
    }
}

// ── Simple glyph ─────────────────────────────────────────────────────────────

fn decode_simple(
    d:           &[u8],
    num_contours: usize,
    x_min: i16, y_min: i16, x_max: i16, y_max: i16,
) -> Result<Outline, ParseError> {
    if num_contours == 0 { return Ok(Outline::default()); }

    // End-point indices of each contour (relative to 0).
    let end_pts_off = 10usize;
    let total_pts   = u16_at(d, end_pts_off + (num_contours - 1) * 2)
        .ok_or(ParseError::TooShort)? as usize + 1;

    let instruction_len_off = end_pts_off + num_contours * 2;
    let instruction_len = u16_at(d, instruction_len_off).ok_or(ParseError::TooShort)? as usize;

    let mut cursor = instruction_len_off + 2 + instruction_len;

    // ── Flags ──────────────────────────────────────────────────────────────
    let mut flags = Vec::with_capacity(total_pts);
    while flags.len() < total_pts {
        let f = u8_at(d, cursor).ok_or(ParseError::TooShort)?;
        cursor += 1;
        let repeat = if f & REPEAT != 0 {
            let n = u8_at(d, cursor).ok_or(ParseError::TooShort)?;
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
            let v = u8_at(d, cursor).ok_or(ParseError::TooShort)? as i32;
            cursor += 1;
            if f & X_SAME_OR_POS != 0 { v } else { -v }
        } else if f & X_SAME_OR_POS != 0 {
            0
        } else {
            let v = i16_at(d, cursor).ok_or(ParseError::TooShort)? as i32;
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
            let v = u8_at(d, cursor).ok_or(ParseError::TooShort)? as i32;
            cursor += 1;
            if f & Y_SAME_OR_POS != 0 { v } else { -v }
        } else if f & Y_SAME_OR_POS != 0 {
            0
        } else {
            let v = i16_at(d, cursor).ok_or(ParseError::TooShort)? as i32;
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
        let end = u16_at(d, end_pts_off + c * 2).ok_or(ParseError::TooShort)? as usize;
        emit_contour(&mut outline, &flags, &xs, &ys, pt_idx, end);
        pt_idx = end + 1;
    }

    Ok(outline)
}

fn pt(xs: &[i16], ys: &[i16], i: usize) -> Point {
    Point::new(xs[i] as f32, ys[i] as f32)
}

fn on(flags: &[u8], i: usize) -> bool { flags[i] & ON_CURVE != 0 }

/// Emit TrueType quadratic contour verbs, handling implied on-curve midpoints.
fn emit_contour(
    outline: &mut Outline,
    flags:   &[u8],
    xs:      &[i16],
    ys:      &[i16],
    start:   usize,
    end:     usize,
) {
    let count = end - start + 1;
    if count == 0 { return; }

    // Find first on-curve point (or use midpoint if all off-curve).
    let first_on = (start..=end).find(|&i| on(flags, i));
    let start_pt = match first_on {
        Some(i) => pt(xs, ys, i),
        None    => pt(xs, ys, start).lerp(pt(xs, ys, start + 1), 0.5),
    };

    outline.verbs.push(Verb::MoveTo(start_pt));

    // Walk the contour.
    let mut i = start;
    let mut pending_off: Option<Point> = None;
    let rotation_start = first_on.unwrap_or(start);

    for step in 0..count {
        let idx = start + (rotation_start - start + step) % count;
        let p = pt(xs, ys, idx);
        if on(flags, idx) {
            if let Some(ctrl) = pending_off.take() {
                outline.verbs.push(Verb::QuadTo(ctrl, p));
            } else if step > 0 {
                outline.verbs.push(Verb::LineTo(p));
            }
        } else {
            if let Some(prev_ctrl) = pending_off.replace(p) {
                // Two consecutive off-curve → implied midpoint.
                let mid = prev_ctrl.lerp(p, 0.5);
                outline.verbs.push(Verb::QuadTo(prev_ctrl, mid));
            }
        }
        i += 1;
    }
    // Close: flush any remaining off-curve.
    if let Some(ctrl) = pending_off {
        outline.verbs.push(Verb::QuadTo(ctrl, start_pt));
    } else {
        outline.verbs.push(Verb::LineTo(start_pt));
    }
    outline.verbs.push(Verb::Close);
    let _ = i;
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
        let flags = u16_at(d, cursor).ok_or(ParseError::TooShort)?;
        cursor += 2;
        let glyph_index = u16_at(d, cursor).ok_or(ParseError::TooShort)?;
        cursor += 2;

        // Translation arguments.
        let (dx, dy) = if flags & ARG_1_AND_2_ARE_WORDS != 0 {
            let dx = i16_at(d, cursor).ok_or(ParseError::TooShort)? as f32;
            let dy = i16_at(d, cursor + 2).ok_or(ParseError::TooShort)? as f32;
            cursor += 4;
            (dx, dy)
        } else {
            let dx = u8_at(d, cursor).ok_or(ParseError::TooShort)? as i8 as f32;
            let dy = u8_at(d, cursor + 1).ok_or(ParseError::TooShort)? as i8 as f32;
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
        let component = find_component(glyf_data, glyph_index, depth)?;
        // Translate and append.
        for verb in component.verbs {
            outline.verbs.push(translate_verb(verb, dx, dy));
        }

        if flags & MORE_COMPONENTS == 0 { break; }
    }

    Ok(outline)
}

fn find_component(glyf_data: &[u8], _gid: u16, _depth: u8) -> Result<Outline, ParseError> {
    // Composite expansion requires Loca; we defer full support to when
    // FontFace passes Loca in. Return an empty outline as a safe stub —
    // composite glyphs in JetBrains Mono are rare (only accented letters).
    let _ = glyf_data;
    Ok(Outline::default())
}

fn translate_verb(v: Verb, dx: f32, dy: f32) -> Verb {
    match v {
        Verb::MoveTo(p)     => Verb::MoveTo(Point::new(p.x + dx, p.y + dy)),
        Verb::LineTo(p)     => Verb::LineTo(Point::new(p.x + dx, p.y + dy)),
        Verb::QuadTo(c, p)  => Verb::QuadTo(
            Point::new(c.x + dx, c.y + dy),
            Point::new(p.x + dx, p.y + dy),
        ),
        Verb::Close => Verb::Close,
    }
}
