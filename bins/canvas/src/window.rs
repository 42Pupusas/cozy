//! [`CanvasWindow`] -- the [`Painter`] that renders a [`Session`] and routes
//! compositor input into it.
//!
//! The only module in the binary that knows about pixels or Wayland. Every
//! decision was already made by `Session`, [`Panel`], [`Handle`] and
//! [`Icon`]; what is left here is a straight transcription onto a canvas,
//! exactly the layering `bins/files` uses.

use platform::draw::{Canvas as PixCanvas, Color, Rect};
use platform::protocol::PointerButton;
use platform::toplevel::{Key, KeyPress, Painter, PhysicalSize, PointerEventKind, Position};
use statusbar::TextRenderer;

use crate::handle::{CursorShape, Handle};
use crate::icon::{Icon, PlacedStroke};
use crate::panel::{Panel, PanelHit, Slot};
use crate::session::{Session, Tool};
use crate::shape::{ArrowHead, Shape};
use crate::theme::Theme;

/// The painted (not grabbable) size of a resize handle, in physical pixels.
/// The grab zone around it is [`Handle::GRAB`], deliberately larger.
const HANDLE_PAINT: i32 = 8;

/// The painter over the session.
pub struct CanvasWindow {
    pub session: Session,
    theme: Theme,
    text: TextRenderer,
    panel: Panel,
    /// Output scale, for logical pointer -> physical canvas conversion.
    scale: i32,
    /// The cursor shape currently requested, so the window only re-asks the
    /// compositor when it actually changes.
    cursor: CursorShape,
    /// Set when the cursor shape changed and the event loop should push it.
    cursor_dirty: bool,
    /// Last known canvas size, for clamping the panel on resize.
    size: PhysicalSize,
    quit: bool,
}

impl CanvasWindow {
    /// Assemble the painter. `text` is the note/body font renderer; the
    /// caller loads it so the expensive font parse stays off this path.
    #[must_use]
    pub fn new(session: Session, theme: Theme, text: TextRenderer, scale: i32) -> Self {
        Self {
            session,
            theme,
            text,
            panel: Panel::new(scale),
            scale,
            cursor: CursorShape::Default,
            cursor_dirty: true,
            size: PhysicalSize {
                width: 0,
                height: 0,
            },
            quit: false,
        }
    }

    /// Whether the user asked to quit.
    #[must_use]
    pub const fn should_quit(&self) -> bool {
        self.quit
    }

    /// Take the pending cursor shape, if it changed since the last call.
    ///
    /// The event loop drains this after dispatching input and forwards it to
    /// `Toplevel::set_cursor`, which is the only place allowed to talk to
    /// the compositor.
    pub const fn take_cursor(&mut self) -> Option<CursorShape> {
        if self.cursor_dirty {
            self.cursor_dirty = false;
            return Some(self.cursor);
        }
        None
    }

    /// Resize the note being edited to the extent of its text.
    ///
    /// Only the window can measure text, so the session is told the result
    /// rather than asked to compute it. Without this the invisible
    /// hit-testing box would not match the visible glyphs.
    fn refit_note(&mut self) {
        let Some((_, editor)) = self.session.editing_note() else {
            return;
        };
        let lines = editor.line_count();
        let widest = (0..lines)
            .map(|i| self.text.measure(editor.line(i)))
            .max()
            .unwrap_or(0);
        let height = self.text.line_height() * i32::try_from(lines).unwrap_or(1);
        // Screen pixels back to world units: the box is stored in world
        // space, so a note keeps its size when the camera zooms.
        let zoom = self.session.camera.zoom();
        self.session
            .fit_note(f64::from(widest) / zoom, f64::from(height) / zoom);
    }

    /// Record the cursor the pointer should show at `p` (physical pixels).
    fn update_cursor(&mut self, p: (i32, i32)) {
        // The panel is chrome and sits above the canvas, so it wins.
        let want = self
            .panel
            .cursor_at(p)
            .unwrap_or_else(|| self.session.cursor_at(p));
        if want != self.cursor {
            self.cursor = want;
            self.cursor_dirty = true;
        }
    }

    /// Convert a logical pointer position into physical canvas pixels.
    const fn physical(&self, at: Position) -> (i32, i32) {
        (at.x * self.scale, at.y * self.scale)
    }

    /// The same conversion keeping the `Position` type.
    const fn to_physical_position(&self, at: Position) -> Position {
        Position {
            x: at.x * self.scale,
            y: at.y * self.scale,
        }
    }

    /// Paint the floating tool palette.
    fn paint_panel(&self, canvas: &mut PixCanvas<'_>) {
        let b = self.panel.bounds();
        let card = Rect::new(b.x, b.y, b.w, b.h);
        canvas.fill_round_rect(card, self.theme.surface, self.theme.note_radius);
        canvas.draw_round_rect(card, self.theme.border, self.theme.note_radius);
        self.paint_grip(canvas);
        for (tool, slot) in self.panel.slots() {
            self.paint_tool_button(canvas, tool, slot);
        }
    }

    /// Paint the drag grip: a few short rules that read as "grab here".
    fn paint_grip(&self, canvas: &mut PixCanvas<'_>) {
        let g = self.panel.grip();
        let inset = g.w / 4;
        let rows = 2;
        let gap = 3.max(g.h / 5);
        let top = g.y + (g.h - (rows - 1) * gap) / 2;
        for i in 0..rows {
            canvas.fill_rect(
                Rect::new(g.x + inset, top + i * gap, g.w - inset * 2, 1),
                self.theme.grid,
            );
        }
    }

    /// Paint one tool button and its icon.
    fn paint_tool_button(&self, canvas: &mut PixCanvas<'_>, tool: Tool, slot: Slot) {
        let active = self.session.tool() == tool;
        let rect = Rect::new(slot.x, slot.y, slot.w, slot.h);
        if active {
            canvas.fill_round_rect(rect, self.theme.selection, 4);
        }
        let ink = if active {
            self.theme.background
        } else {
            self.theme.text
        };
        let glyph = slot.inset(slot.w / 4);
        for stroke in Icon::strokes(tool) {
            self.paint_stroke(
                canvas,
                &stroke.place(glyph.x, glyph.y, glyph.w, glyph.h),
                ink,
                Icon::filled(tool),
            );
        }
    }

    /// Paint one icon stroke.
    fn paint_stroke(
        &self,
        canvas: &mut PixCanvas<'_>,
        stroke: &PlacedStroke,
        ink: Color,
        filled: bool,
    ) {
        let w = self.theme.stroke_width.max(1);
        match stroke {
            PlacedStroke::Line { x1, y1, x2, y2 } => canvas.draw_line(*x1, *y1, *x2, *y2, w, ink),
            PlacedStroke::Rect { x, y, w: rw, h } => {
                canvas.draw_rect(Rect::new(*x, *y, *rw, *h), ink);
            }
            PlacedStroke::Ellipse { x, y, w: ew, h } => {
                canvas.draw_ellipse(Rect::new(*x, *y, *ew, *h), ink);
            }
            PlacedStroke::Poly(points) => Self::paint_poly(canvas, points, ink, filled, w),
        }
    }

    /// Paint a closed polygon, filled by scanline or stroked edge by edge.
    fn paint_poly(
        canvas: &mut PixCanvas<'_>,
        points: &[(i32, i32)],
        ink: Color,
        filled: bool,
        width: i32,
    ) {
        if points.len() < 2 {
            return;
        }
        if filled {
            Self::fill_poly(canvas, points, ink);
        }
        for i in 0..points.len() {
            let a = points[i];
            let b = points[(i + 1) % points.len()];
            canvas.draw_line(a.0, a.1, b.0, b.1, width, ink);
        }
    }

    /// Scanline-fill a closed polygon.
    ///
    /// Even-odd rule: for each row, collect every edge crossing, sort them,
    /// and fill between alternating pairs. Enough for the small convex-ish
    /// icons here.
    #[allow(clippy::cast_possible_truncation)]
    fn fill_poly(canvas: &mut PixCanvas<'_>, points: &[(i32, i32)], ink: Color) {
        let top = points.iter().map(|p| p.1).min().unwrap_or(0);
        let bottom = points.iter().map(|p| p.1).max().unwrap_or(0);
        let mut crossings: Vec<i32> = Vec::with_capacity(points.len());
        for y in top..=bottom {
            crossings.clear();
            for i in 0..points.len() {
                let (x1, y1) = points[i];
                let (x2, y2) = points[(i + 1) % points.len()];
                if (y1 <= y && y2 > y) || (y2 <= y && y1 > y) {
                    let t = f64::from(y - y1) / f64::from(y2 - y1);
                    crossings.push(x1 + (t * f64::from(x2 - x1)).round() as i32);
                }
            }
            crossings.sort_unstable();
            for pair in crossings.as_chunks::<2>().0 {
                canvas.fill_rect(Rect::new(pair[0], y, pair[1] - pair[0] + 1, 1), ink);
            }
        }
    }

    /// Paint the dotted grid lattice across the visible world.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    fn paint_grid(&self, canvas: &mut PixCanvas<'_>) {
        let pitch = self.theme.grid_pitch;
        if pitch < 2 {
            return;
        }
        let zoom = self.session.camera.zoom();
        let step = ((f64::from(pitch) * zoom).round() as i32).max(4);
        let origin = self.session.camera.to_screen((0.0, 0.0));
        let first = |o: i32| o - (o.div_euclid(step) * step);
        let mut y = first(origin.1);
        while y < canvas.height() {
            let mut x = first(origin.0);
            while x < canvas.width() {
                if x >= 0 && y >= 0 {
                    canvas.set_pixel(x, y, self.theme.grid);
                }
                x += step;
            }
            y += step;
        }
    }

    /// The screen rect for a shape's bounds.
    fn screen_rect(&self, bounds: (f64, f64, f64, f64)) -> Rect {
        let (x, y, w, h) = bounds;
        let (sx, sy) = self.session.camera.to_screen((x, y));
        let (ex, ey) = self.session.camera.to_screen((x + w, y + h));
        Rect::new(sx.min(ex), sy.min(ey), (ex - sx).abs(), (ey - sy).abs())
    }

    /// Paint one shape (or the live preview).
    fn paint_shape(
        &mut self,
        canvas: &mut PixCanvas<'_>,
        shape: &Shape,
        selected: bool,
        index: Option<usize>,
    ) {
        let stroke = self.theme.stroke_width.max(1);
        let color = if selected {
            self.theme.selection
        } else {
            self.theme.text
        };
        match *shape {
            Shape::Rect { .. } => {
                let rect = self.screen_rect(shape.bounds());
                canvas.draw_rect(rect, color);
                if stroke > 1 {
                    canvas.draw_rect(rect.inset(1), color);
                }
            }
            Shape::Ellipse { .. } => {
                canvas.draw_ellipse(self.screen_rect(shape.bounds()), color);
            }
            Shape::Arrow { ax, ay, bx, by } => {
                let a = self.session.camera.to_screen((ax, ay));
                let b = self.session.camera.to_screen((bx, by));
                canvas.draw_line(a.0, a.1, b.0, b.1, stroke, color);
                self.paint_arrowhead(canvas, a, b, color);
            }
            Shape::Note { x, y, ref text, .. } => {
                self.paint_note(canvas, (x, y), text, color, index);
            }
        }
    }

    /// Paint a note as freeform text: no card, no border, just the glyphs.
    ///
    /// Excalidraw's text tool draws bare text on the canvas, so a box would
    /// be wrong here. The shape's bounds still exist for hit-testing; they
    /// are simply not painted.
    fn paint_note(
        &mut self,
        canvas: &mut PixCanvas<'_>,
        at: (f64, f64),
        text: &str,
        ink: Color,
        index: Option<usize>,
    ) {
        let (sx, sy) = self.session.camera.to_screen(at);
        let line_h = self.text.line_height();
        // While this note is being edited the editor holds the live text,
        // which is newer than the copy stored on the shape.
        //
        // Identified by index, never by comparing coordinates: a note that
        // has been dragged or refitted no longer sits at the position the
        // caller passed, and float equality would silently stop matching --
        // taking the caret with it.
        let editing = self.editing_lines(index);
        let lines: Vec<String> =
            editing.unwrap_or_else(|| text.split('\n').map(str::to_owned).collect());
        let mut y = sy;
        for line in &lines {
            self.text
                .draw_text(canvas, line, sx, y, ink, self.theme.background);
            y += line_h;
        }
        if self.is_editing(index) {
            self.paint_caret(canvas, (sx, sy), line_h);
        }
    }

    /// Whether the shape at `index` is the one currently being edited.
    fn is_editing(&self, index: Option<usize>) -> bool {
        matches!((self.session.editing_note(), index), (Some((i, _)), Some(n)) if i == n)
    }

    /// The live editor lines when the shape at `index` is being edited.
    fn editing_lines(&self, index: Option<usize>) -> Option<Vec<String>> {
        if !self.is_editing(index) {
            return None;
        }
        let (_, editor) = self.session.editing_note()?;
        Some(
            (0..editor.line_count())
                .map(|i| editor.line(i).to_owned())
                .collect(),
        )
    }

    /// Paint the text caret for the note being edited.
    ///
    /// Measured, not counted: the face is proportional, so the caret's x is
    /// the rendered width of the text before it, exactly as the filter box
    /// in `bins/files` places its own.
    fn paint_caret(&mut self, canvas: &mut PixCanvas<'_>, origin: (i32, i32), line_h: i32) {
        let Some((_, editor)) = self.session.editing_note() else {
            return;
        };
        let before = editor.line(editor.line)[..editor.col].to_owned();
        let w = self.text.measure(&before);
        let x = origin.0 + w;
        let y = origin.1 + line_h * i32::try_from(editor.line).unwrap_or(0);
        canvas.fill_rect(
            Rect::new(x, y, self.theme.caret_width.max(1), line_h),
            self.theme.caret,
        );
    }

    /// Paint the arrowhead at `b`, oriented along `a -> b`.
    fn paint_arrowhead(
        &self,
        canvas: &mut PixCanvas<'_>,
        a: (i32, i32),
        b: (i32, i32),
        color: Color,
    ) {
        let (dx, dy) = (f64::from(b.0 - a.0), f64::from(b.1 - a.1));
        let length = dx.hypot(dy);
        if length < 4.0 {
            return;
        }
        let (ux, uy) = (dx / length, dy / length);
        // The barb geometry lives in `shape::ArrowHead`, where it is tested;
        // this only turns the two directions into pixels.
        let head = f64::from(self.theme.arrow_head.max(1));
        for (wx, wy) in ArrowHead::barbs(ux, uy) {
            let barb = (
                b.0 + Self::f_to_i32(wx * head),
                b.1 + Self::f_to_i32(wy * head),
            );
            canvas.draw_line(
                b.0,
                b.1,
                barb.0,
                barb.1,
                self.theme.stroke_width.max(1),
                color,
            );
        }
    }

    #[allow(clippy::cast_possible_truncation)]
    const fn f_to_i32(v: f64) -> i32 {
        if v.is_nan() { 0 } else { v.round() as i32 }
    }

    /// Paint the eight resize handles around the selected shape.
    fn paint_selection_decorations(&mut self, canvas: &mut PixCanvas<'_>) {
        let Some(index) = self.session.selected() else {
            return;
        };
        let Some(shape) = self.session.doc.shapes().get(index) else {
            return;
        };
        let shape = shape.clone();
        let bounds = shape.bounds();
        self.paint_shape(canvas, &shape, true, Some(index));
        // A dashed-looking outline: the bounding box, so an arrow shows what
        // its handles refer to.
        canvas.draw_rect(self.screen_rect(bounds), self.theme.border);
        let half = HANDLE_PAINT / 2;
        for handle in Handle::RESIZE {
            let (wx, wy) = handle.anchor(bounds);
            let (sx, sy) = self.session.camera.to_screen((wx, wy));
            let square = Rect::new(sx - half, sy - half, HANDLE_PAINT, HANDLE_PAINT);
            canvas.fill_rect(square, self.theme.background);
            canvas.draw_rect(square, self.theme.selection);
        }
    }
}

impl Painter for CanvasWindow {
    fn background(&self) -> Color {
        self.theme.background
    }

    fn paint(&mut self, canvas: &mut PixCanvas<'_>, size: PhysicalSize) {
        if size != self.size {
            self.size = size;
            self.panel.clamp(size.width, size.height);
        }
        canvas.clear(self.theme.background);
        self.paint_grid(canvas);
        let count = self.session.doc.len();
        for index in 0..count {
            let shape = self.session.doc.shapes()[index].clone();
            let selected = self.session.selected() == Some(index);
            self.paint_shape(canvas, &shape, selected, Some(index));
        }
        if let Some(preview) = self.session.preview() {
            // The preview is not in the document, so it has no index.
            self.paint_shape(canvas, &preview, false, None);
        }
        self.paint_selection_decorations(canvas);
        // The panel floats above everything.
        self.paint_panel(canvas);
    }

    fn on_key(&mut self, press: KeyPress) {
        if matches!(press.key, Key::Char('q')) && press.modifiers.ctrl() {
            self.quit = true;
            return;
        }
        self.session.key(press);
        self.refit_note();
    }

    fn on_pointer(&mut self, event: PointerEventKind) {
        match event {
            PointerEventKind::Press {
                button: PointerButton::Left,
                at,
                ..
            } => {
                let p = self.physical(at);
                if let Some(hit) = self.panel.hit(p) {
                    match hit {
                        PanelHit::Grip => {
                            self.panel.press(p);
                        }
                        PanelHit::Tool(tool) => self.session.set_tool(tool),
                    }
                    self.update_cursor(p);
                    return;
                }
            }
            PointerEventKind::Motion(at) => {
                let p = self.physical(at);
                if self.panel.is_dragging() {
                    self.panel.drag(p, self.size.width, self.size.height);
                    self.update_cursor(p);
                    return;
                }
                self.update_cursor(p);
            }
            PointerEventKind::Release { at, .. } if self.panel.is_dragging() => {
                self.panel.release();
                self.update_cursor(self.physical(at));
                return;
            }
            _ => {}
        }
        let physical = match event {
            PointerEventKind::Press {
                button,
                at,
                modifiers,
            } => PointerEventKind::Press {
                button,
                at: self.to_physical_position(at),
                modifiers,
            },
            PointerEventKind::Release {
                button,
                at,
                modifiers,
            } => PointerEventKind::Release {
                button,
                at: self.to_physical_position(at),
                modifiers,
            },
            PointerEventKind::Motion(at) => PointerEventKind::Motion(self.to_physical_position(at)),
            PointerEventKind::Scroll {
                lines,
                at,
                modifiers,
            } => PointerEventKind::Scroll {
                lines,
                at: self.to_physical_position(at),
                modifiers,
            },
            other => other,
        };
        self.session.pointer(physical);
        // A gesture may have changed what is under the pointer.
        if let PointerEventKind::Press { at, .. }
        | PointerEventKind::Release { at, .. }
        | PointerEventKind::Motion(at) = event
        {
            self.update_cursor(self.physical(at));
        }
    }

    fn on_focus(&mut self, _focused: bool) {}
}
