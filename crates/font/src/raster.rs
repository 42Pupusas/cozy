//! Scanline rasterizer: converts an [`Outline`] into an 8-bit alpha [`Bitmap`].
//!
//! Algorithm:
//! 1. Transform font-unit coordinates → pixel coordinates (scale + translate).
//! 2. Decompose quadratic Béziers into short line segments.
//! 3. For every scanline, collect all edge crossings and sort them.
//! 4. Fill spans using the non-zero winding rule.
//!
//! Pure Rust, zero allocations beyond the output bitmap and temporary edge list.

use crate::outline::{Outline, Point, Verb};

// ── Public types ──────────────────────────────────────────────────────────────

/// An 8-bit alpha-channel glyph bitmap (top-left origin).
#[derive(Debug, Clone)]
pub struct Bitmap {
    pub width:  u32,
    pub height: u32,
    /// Row-major, 1 byte per pixel — 0 = transparent, 255 = fully covered.
    pub data:   Vec<u8>,
}

impl Bitmap {
    pub fn blank(width: u32, height: u32) -> Self {
        Self { width, height, data: vec![0u8; (width * height) as usize] }
    }
}

// ── Edge ──────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy)]
struct Edge {
    x0: f32, y0: f32,
    x1: f32, y1: f32,
    /// +1 going up, -1 going down (in screen coords where Y increases down).
    winding: i8,
}

// ── Public entry ──────────────────────────────────────────────────────────────

/// Rasterize `outline` into a bitmap of size `w × h`.
///
/// `scale`     — font-units → pixels (typically `px_size / units_per_em`)
/// `baseline`  — Y pixel at which the font baseline sits (usually close to `h`)
/// `x_offset`  — horizontal shift in pixels (left side bearing applied by caller)
pub fn rasterize(outline: &Outline, w: u32, h: u32, scale: f32, baseline: f32, x_offset: f32) -> Bitmap {
    let mut edges: Vec<Edge> = Vec::new();

    // Transform a font-unit point to pixel coords.
    let tx = |p: Point| -> (f32, f32) {
        let px = p.x * scale + x_offset;
        // Y in fonts goes up; in pixels it goes down — flip around baseline.
        let py = baseline - p.y * scale;
        (px, py)
    };

    let mut current = (0.0f32, 0.0f32);
    let mut contour_start = (0.0f32, 0.0f32);

    for &verb in &outline.verbs {
        match verb {
            Verb::MoveTo(p) => {
                let (px, py) = tx(p);
                current = (px, py);
                contour_start = (px, py);
            }
            Verb::LineTo(p) => {
                let (px, py) = tx(p);
                push_edge(&mut edges, current, (px, py));
                current = (px, py);
            }
            Verb::QuadTo(ctrl, end) => {
                let (cx, cy) = tx(ctrl);
                let (ex, ey) = tx(end);
                // Subdivide into ~8 segments — sufficient for pixel-scale glyphs.
                subdivide_quad(&mut edges, current, (cx, cy), (ex, ey), 3);
                current = (ex, ey);
            }
            Verb::Close => {
                push_edge(&mut edges, current, contour_start);
                current = contour_start;
            }
        }
    }

    // Scanline fill.
    let mut bmp = Bitmap::blank(w, h);
    for scanline in 0..h {
        let y = scanline as f32 + 0.5; // sample at center of pixel row

        // Collect X intersections on this scanline with non-zero winding.
        let mut crossings: Vec<(f32, i8)> = edges.iter()
            .filter_map(|e| edge_x_at(e, y))
            .collect();

        crossings.sort_unstable_by(|a, b| a.0.partial_cmp(&b.0).unwrap());

        let mut winding = 0i32;
        let mut prev_x  = 0.0f32;
        let mut inside   = false;

        for (x, w_delta) in crossings {
            if inside {
                // Fill from prev_x to x.
                let x0 = (prev_x.ceil() as i64).max(0) as u32;
                let x1 = (x.ceil() as i64).max(0).min(w as i64) as u32;
                for col in x0..x1 {
                    bmp.data[(scanline * w + col) as usize] = 255;
                }
            }
            winding += w_delta as i32;
            inside   = winding != 0;
            prev_x   = x;
        }
    }

    bmp
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn push_edge(edges: &mut Vec<Edge>, p0: (f32, f32), p1: (f32, f32)) {
    if (p0.1 - p1.1).abs() < 1e-6 { return; } // horizontal — skip
    let (winding, (x0, y0), (x1, y1)) = if p0.1 < p1.1 {
        (-1i8, p0, p1) // going down in screen coords
    } else {
        (1i8, p1, p0)  // going up
    };
    edges.push(Edge { x0, y0, x1, y1, winding });
}

fn edge_x_at(e: &Edge, y: f32) -> Option<(f32, i8)> {
    if y < e.y0 || y >= e.y1 { return None; }
    let t = (y - e.y0) / (e.y1 - e.y0);
    let x = e.x0 + t * (e.x1 - e.x0);
    Some((x, e.winding))
}

/// Recursively subdivide a quadratic Bézier into line segments.
fn subdivide_quad(
    edges:   &mut Vec<Edge>,
    p0:      (f32, f32),
    ctrl:    (f32, f32),
    p1:      (f32, f32),
    depth:   u8,
) {
    if depth == 0 {
        push_edge(edges, p0, p1);
        return;
    }
    // De Casteljau midpoint split.
    let m0 = lerp2(p0, ctrl, 0.5);
    let m1 = lerp2(ctrl, p1, 0.5);
    let mid = lerp2(m0, m1, 0.5);
    subdivide_quad(edges, p0, m0, mid, depth - 1);
    subdivide_quad(edges, mid, m1, p1, depth - 1);
}

fn lerp2(a: (f32, f32), b: (f32, f32), t: f32) -> (f32, f32) {
    (a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t)
}
