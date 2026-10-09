//! [`Icon`] -- the glyph drawn on each tool button, as geometry.
//!
//! Each tool is advertised by its own shape rather than a letter: a cursor
//! arrow for select, a rectangle for rect, and so on. The text tool draws a
//! real "T", which is the one case where a letterform *is* the icon.
//!
//! Icons are described as unit-square strokes (`0.0..=1.0` on both axes) so
//! one description scales to any button size and any output density. The
//! window maps them onto pixels; nothing here knows the pitch.

use crate::session::Tool;

/// One stroke of an icon, in unit-square coordinates.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Stroke {
    /// A straight line.
    Line { x1: f64, y1: f64, x2: f64, y2: f64 },
    /// A rectangle outline.
    Rect { x: f64, y: f64, w: f64, h: f64 },
    /// An ellipse outline inscribed in a box.
    Ellipse { x: f64, y: f64, w: f64, h: f64 },
    /// A closed polygon through the listed points, used for arrowheads and
    /// the select cursor.
    Poly(&'static [(f64, f64)]),
}

impl Stroke {
    /// Map this stroke onto a pixel box, returning the same variant with
    /// coordinates in physical pixels.
    #[must_use]
    #[allow(clippy::cast_possible_truncation)]
    pub fn place(self, x: i32, y: i32, w: i32, h: i32) -> PlacedStroke {
        let fx = |u: f64| x + (u * f64::from(w)).round() as i32;
        let fy = |v: f64| y + (v * f64::from(h)).round() as i32;
        match self {
            Self::Line { x1, y1, x2, y2 } => PlacedStroke::Line {
                x1: fx(x1),
                y1: fy(y1),
                x2: fx(x2),
                y2: fy(y2),
            },
            Self::Rect {
                x: rx,
                y: ry,
                w: rw,
                h: rh,
            } => PlacedStroke::Rect {
                x: fx(rx),
                y: fy(ry),
                w: fx(rx + rw) - fx(rx),
                h: fy(ry + rh) - fy(ry),
            },
            Self::Ellipse {
                x: ex,
                y: ey,
                w: ew,
                h: eh,
            } => PlacedStroke::Ellipse {
                x: fx(ex),
                y: fy(ey),
                w: fx(ex + ew) - fx(ex),
                h: fy(ey + eh) - fy(ey),
            },
            Self::Poly(points) => {
                PlacedStroke::Poly(points.iter().map(|&(u, v)| (fx(u), fy(v))).collect())
            }
        }
    }
}

/// A [`Stroke`] resolved to physical pixels, ready to paint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlacedStroke {
    Line { x1: i32, y1: i32, x2: i32, y2: i32 },
    Rect { x: i32, y: i32, w: i32, h: i32 },
    Ellipse { x: i32, y: i32, w: i32, h: i32 },
    Poly(Vec<(i32, i32)>),
}

/// The glyph for one tool.
pub struct Icon;

impl Icon {
    /// The select tool's arrow: a classic pointer silhouette.
    const SELECT: &'static [(f64, f64)] = &[
        (0.28, 0.14),
        (0.28, 0.82),
        (0.45, 0.66),
        (0.56, 0.90),
        (0.68, 0.84),
        (0.57, 0.61),
        (0.76, 0.58),
    ];

    /// The arrow tool's head, a filled triangle at the line's end.
    const ARROWHEAD: &'static [(f64, f64)] = &[(0.82, 0.24), (0.82, 0.52), (0.56, 0.38)];

    /// The pan tool: a four-way chevron cluster reads as "move the view".
    const PAN_UP: &'static [(f64, f64)] = &[(0.50, 0.10), (0.38, 0.28), (0.62, 0.28)];
    const PAN_DOWN: &'static [(f64, f64)] = &[(0.50, 0.90), (0.38, 0.72), (0.62, 0.72)];
    const PAN_LEFT: &'static [(f64, f64)] = &[(0.10, 0.50), (0.28, 0.38), (0.28, 0.62)];
    const PAN_RIGHT: &'static [(f64, f64)] = &[(0.90, 0.50), (0.72, 0.38), (0.72, 0.62)];

    /// The strokes that draw `tool`'s icon.
    #[must_use]
    pub const fn strokes(tool: Tool) -> &'static [Stroke] {
        match tool {
            Tool::Select => &[Stroke::Poly(Self::SELECT)],
            Tool::Rect => &[Stroke::Rect {
                x: 0.16,
                y: 0.24,
                w: 0.68,
                h: 0.52,
            }],
            Tool::Ellipse => &[Stroke::Ellipse {
                x: 0.14,
                y: 0.22,
                w: 0.72,
                h: 0.56,
            }],
            Tool::Arrow => &[
                Stroke::Line {
                    x1: 0.16,
                    y1: 0.72,
                    x2: 0.74,
                    y2: 0.32,
                },
                Stroke::Poly(Self::ARROWHEAD),
            ],
            // A real letterform: the one icon where the glyph is the point.
            Tool::Note => &[
                Stroke::Line {
                    x1: 0.20,
                    y1: 0.22,
                    x2: 0.80,
                    y2: 0.22,
                },
                Stroke::Line {
                    x1: 0.50,
                    y1: 0.22,
                    x2: 0.50,
                    y2: 0.80,
                },
            ],
            Tool::Pan => &[
                Stroke::Poly(Self::PAN_UP),
                Stroke::Poly(Self::PAN_DOWN),
                Stroke::Poly(Self::PAN_LEFT),
                Stroke::Poly(Self::PAN_RIGHT),
            ],
        }
    }

    /// Whether `tool`'s polygons are filled rather than outlined.
    #[must_use]
    pub const fn filled(tool: Tool) -> bool {
        matches!(tool, Tool::Select | Tool::Arrow | Tool::Pan)
    }
}

#[cfg(test)]
#[allow(clippy::float_cmp)]
mod tests {
    use super::*;

    /// Every coordinate in a stroke, for range checking.
    fn coords(s: &Stroke) -> Vec<(f64, f64)> {
        match *s {
            Stroke::Line { x1, y1, x2, y2 } => vec![(x1, y1), (x2, y2)],
            Stroke::Rect { x, y, w, h } | Stroke::Ellipse { x, y, w, h } => {
                vec![(x, y), (x + w, y + h)]
            }
            Stroke::Poly(p) => p.to_vec(),
        }
    }

    #[test]
    fn every_tool_has_an_icon() {
        for tool in Tool::ALL {
            assert!(!Icon::strokes(tool).is_empty(), "{tool:?} has no icon");
        }
    }

    #[test]
    fn every_icon_stays_inside_the_unit_square() {
        for tool in Tool::ALL {
            for stroke in Icon::strokes(tool) {
                for (u, v) in coords(stroke) {
                    assert!(
                        (0.0..=1.0).contains(&u) && (0.0..=1.0).contains(&v),
                        "{tool:?} escapes the button: ({u}, {v})"
                    );
                }
            }
        }
    }

    #[test]
    fn every_icon_actually_spans_some_area() {
        // A degenerate icon would paint as a dot or nothing at all.
        for tool in Tool::ALL {
            let all: Vec<(f64, f64)> = Icon::strokes(tool).iter().flat_map(coords).collect();
            let min_x = all.iter().map(|p| p.0).fold(f64::MAX, f64::min);
            let max_x = all.iter().map(|p| p.0).fold(f64::MIN, f64::max);
            let min_y = all.iter().map(|p| p.1).fold(f64::MAX, f64::min);
            let max_y = all.iter().map(|p| p.1).fold(f64::MIN, f64::max);
            assert!(max_x - min_x > 0.3, "{tool:?} is too narrow");
            assert!(max_y - min_y > 0.3, "{tool:?} is too short");
        }
    }

    #[test]
    fn the_text_tool_draws_a_letter_t() {
        // A horizontal bar on top and a vertical stem centred under it.
        let strokes = Icon::strokes(Tool::Note);
        assert_eq!(strokes.len(), 2, "a T is exactly two strokes");
        let Stroke::Line {
            y1: bar_y,
            y2: bar_y2,
            x1: bx1,
            x2: bx2,
        } = strokes[0]
        else {
            panic!("the T's bar must be a line");
        };
        assert_eq!(bar_y, bar_y2, "the bar must be horizontal");
        let Stroke::Line {
            x1: sx1,
            x2: sx2,
            y1: sy1,
            y2: sy2,
        } = strokes[1]
        else {
            panic!("the T's stem must be a line");
        };
        assert_eq!(sx1, sx2, "the stem must be vertical");
        assert!(sy2 > sy1, "the stem must run downward");
        let mid = f64::midpoint(bx1, bx2);
        assert!(
            (sx1 - mid).abs() < 1e-9,
            "the stem must centre under the bar"
        );
        assert!((sy1 - bar_y).abs() < 1e-9, "the stem must start at the bar");
    }

    #[test]
    fn no_icon_is_a_letter_except_the_text_tool() {
        // Every other icon is a shape: rect draws a rect, ellipse an
        // ellipse, and the pointer tools draw polygons.
        assert!(matches!(Icon::strokes(Tool::Rect)[0], Stroke::Rect { .. }));
        assert!(matches!(
            Icon::strokes(Tool::Ellipse)[0],
            Stroke::Ellipse { .. }
        ));
        assert!(matches!(Icon::strokes(Tool::Select)[0], Stroke::Poly(_)));
        assert!(matches!(Icon::strokes(Tool::Pan)[0], Stroke::Poly(_)));
    }

    #[test]
    fn the_arrow_icon_has_a_shaft_and_a_head() {
        let strokes = Icon::strokes(Tool::Arrow);
        assert!(matches!(strokes[0], Stroke::Line { .. }), "needs a shaft");
        let Stroke::Poly(head) = strokes[1] else {
            panic!("needs an arrowhead polygon");
        };
        assert_eq!(head.len(), 3, "an arrowhead is a triangle");
    }

    #[test]
    fn the_pan_icon_points_in_four_directions() {
        let strokes = Icon::strokes(Tool::Pan);
        assert_eq!(strokes.len(), 4);
        for s in strokes {
            assert!(matches!(s, Stroke::Poly(p) if p.len() == 3));
        }
    }

    #[test]
    fn placing_a_stroke_maps_the_unit_square_onto_the_box() {
        let s = Stroke::Line {
            x1: 0.0,
            y1: 0.0,
            x2: 1.0,
            y2: 1.0,
        };
        let placed = s.place(10, 20, 100, 50);
        assert_eq!(
            placed,
            PlacedStroke::Line {
                x1: 10,
                y1: 20,
                x2: 110,
                y2: 70
            }
        );
    }

    #[test]
    fn a_placed_rect_keeps_its_proportions() {
        let s = Stroke::Rect {
            x: 0.25,
            y: 0.5,
            w: 0.5,
            h: 0.5,
        };
        let PlacedStroke::Rect { x, y, w, h } = s.place(0, 0, 100, 100) else {
            panic!("a rect must place as a rect");
        };
        assert_eq!((x, y, w, h), (25, 50, 50, 50));
    }

    #[test]
    fn a_placed_polygon_keeps_its_point_count() {
        let PlacedStroke::Poly(points) = Stroke::Poly(Icon::SELECT).place(0, 0, 32, 32) else {
            panic!("a poly must place as a poly");
        };
        assert_eq!(points.len(), Icon::SELECT.len());
    }

    #[test]
    fn every_placed_icon_lands_inside_its_button() {
        for tool in Tool::ALL {
            for stroke in Icon::strokes(tool) {
                let placed = stroke.place(5, 7, 40, 40);
                let points = match placed {
                    PlacedStroke::Line { x1, y1, x2, y2 } => vec![(x1, y1), (x2, y2)],
                    PlacedStroke::Rect { x, y, w, h } | PlacedStroke::Ellipse { x, y, w, h } => {
                        vec![(x, y), (x + w, y + h)]
                    }
                    PlacedStroke::Poly(p) => p,
                };
                for (px, py) in points {
                    assert!(
                        (5..=45).contains(&px) && (7..=47).contains(&py),
                        "{tool:?} painted outside its button at ({px}, {py})"
                    );
                }
            }
        }
    }

    #[test]
    fn only_the_pointer_style_icons_are_filled() {
        assert!(Icon::filled(Tool::Select));
        assert!(Icon::filled(Tool::Arrow));
        assert!(!Icon::filled(Tool::Rect));
        assert!(!Icon::filled(Tool::Ellipse));
        assert!(!Icon::filled(Tool::Note));
    }
}
