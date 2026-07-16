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
    pub width: u32,
    pub height: u32,
    /// Row-major, 1 byte per pixel — 0 = transparent, 255 = fully covered.
    pub data: Vec<u8>,
}

impl Bitmap {
    #[must_use]
    pub fn blank(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            data: vec![0u8; (width * height) as usize],
        }
    }
}

// ── Edge ──────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy)]
struct Edge {
    x0: f32,
    y0: f32,
    x1: f32,
    y1: f32,
    /// +1 going up, -1 going down (in screen coords where Y increases down).
    winding: i8,
}

impl Edge {
    /// Build an edge from `p0`→`p1`, normalizing so `y0 < y1` and recording
    /// winding direction. Returns `None` for horizontal segments (they never
    /// contribute a scanline crossing).
    ///
    /// Was a free `push_edge(edges: &mut Vec<Edge>, ..)` function that both
    /// built an `Edge` and pushed it; split so construction is a plain
    /// `Edge::new` and the caller decides whether/where to push.
    fn new(p0: (f32, f32), p1: (f32, f32)) -> Option<Self> {
        if (p0.1 - p1.1).abs() < 1e-6 {
            return None;
        } // horizontal — skip
        let (winding, (x0, y0), (x1, y1)) = if p0.1 < p1.1 {
            (-1i8, p0, p1) // going down in screen coords
        } else {
            (1i8, p1, p0) // going up
        };
        Some(Self {
            x0,
            y0,
            x1,
            y1,
            winding,
        })
    }

    /// The X coordinate (and winding delta) where this edge crosses
    /// scanline `y`, or `None` if it doesn't cross at that height.
    ///
    /// Was a free `edge_x_at(e: &Edge, y: f32)` function -- moved onto
    /// `Edge` as the type being queried.
    fn x_at(&self, y: f32) -> Option<(f32, i8)> {
        if y < self.y0 || y >= self.y1 {
            return None;
        }
        let t = (y - self.y0) / (self.y1 - self.y0);
        let x = t.mul_add(self.x1 - self.x0, self.x0);
        Some((x, self.winding))
    }

    /// Recursively subdivide a quadratic Bézier into line-segment edges,
    /// pushing them onto `edges`.
    ///
    /// Was a free `subdivide_quad(edges, ..)` function -- moved onto `Edge`
    /// since it's the type being produced and pushed.
    fn subdivide_quad(
        edges: &mut Vec<Self>,
        p0: (f32, f32),
        ctrl: (f32, f32),
        p1: (f32, f32),
        depth: u8,
    ) {
        if depth == 0 {
            edges.extend(Self::new(p0, p1));
            return;
        }
        // De Casteljau midpoint split.
        let m0 = p0.lerp2(ctrl, 0.5);
        let m1 = ctrl.lerp2(p1, 0.5);
        let mid = m0.lerp2(m1, 0.5);
        Self::subdivide_quad(edges, p0, m0, mid, depth - 1);
        Self::subdivide_quad(edges, mid, m1, p1, depth - 1);
    }
}

/// Extends the `(f32, f32)` pixel-coordinate pair with linear interpolation,
/// as a method instead of a free `lerp2(a, b, t)` function -- mirrors
/// [`crate::outline::Point::lerp`] for the raw-tuple form used during
/// rasterization.
trait Lerp2 {
    fn lerp2(self, other: Self, t: f32) -> Self;
}

impl Lerp2 for (f32, f32) {
    fn lerp2(self, other: Self, t: f32) -> Self {
        (
            (other.0 - self.0).mul_add(t, self.0),
            (other.1 - self.1).mul_add(t, self.1),
        )
    }
}

// ── Public entry ──────────────────────────────────────────────────────────────

impl Outline {
    /// Rasterize this outline into a bitmap of size `w × h`.
    ///
    /// `scale`     — font-units → pixels (typically `px_size / units_per_em`)
    /// `baseline`  — Y pixel at which the font baseline sits (usually close to `h`)
    /// `x_offset`  — horizontal shift in pixels (left side bearing applied by caller)
    ///
    /// Was a free `rasterize(outline: &Outline, ..)` function — moved onto
    /// `Outline` itself since it's the only thing being consumed here; this
    /// also disambiguates by name from `FontFace::rasterize` (`outline.rasterize(..)`
    /// vs `face.rasterize(codepoint)`) instead of both being called `rasterize`
    /// at different scopes.
    #[must_use]
    pub fn rasterize(&self, w: u32, h: u32, scale: f32, baseline: f32, x_offset: f32) -> Bitmap {
        let mut edges: Vec<Edge> = Vec::new();

        // Transform a font-unit point to pixel coords.
        let tx = |p: Point| -> (f32, f32) {
            let px = p.x.mul_add(scale, x_offset);
            // Y in fonts goes up; in pixels it goes down — flip around baseline.
            let py = p.y.mul_add(-scale, baseline);
            (px, py)
        };

        let mut current = (0.0f32, 0.0f32);
        let mut contour_start = (0.0f32, 0.0f32);

        for &verb in &self.verbs {
            match verb {
                Verb::MoveTo(p) => {
                    let (px, py) = tx(p);
                    current = (px, py);
                    contour_start = (px, py);
                }
                Verb::LineTo(p) => {
                    let (px, py) = tx(p);
                    edges.extend(Edge::new(current, (px, py)));
                    current = (px, py);
                }
                Verb::QuadTo(ctrl, end) => {
                    let (cx, cy) = tx(ctrl);
                    let (ex, ey) = tx(end);
                    // Subdivide into ~8 segments — sufficient for pixel-scale glyphs.
                    Edge::subdivide_quad(&mut edges, current, (cx, cy), (ex, ey), 3);
                    current = (ex, ey);
                }
                Verb::Close => {
                    edges.extend(Edge::new(current, contour_start));
                    current = contour_start;
                }
            }
        }

        // Scanline fill.
        let mut bmp = Bitmap::blank(w, h);
        for scanline in 0..h {
            let y = scanline as f32 + 0.5; // sample at center of pixel row

            // Collect X intersections on this scanline with non-zero winding.
            let mut crossings: Vec<(f32, i8)> = edges.iter().filter_map(|e| e.x_at(y)).collect();

            crossings.sort_unstable_by(|a, b| a.0.partial_cmp(&b.0).unwrap());

            let mut winding = 0i32;
            let mut prev_x = 0.0f32;
            let mut inside = false;

            for (x, w_delta) in crossings {
                if inside {
                    // Fill from prev_x to x.
                    let x0 = (prev_x.ceil() as i64).max(0) as u32;
                    let x1 = (x.ceil() as i64).max(0).min(i64::from(w)) as u32;
                    for col in x0..x1 {
                        bmp.data[(scanline * w + col) as usize] = 255;
                    }
                }
                winding += i32::from(w_delta);
                inside = winding != 0;
                prev_x = x;
            }
        }

        bmp
    }
}
