//! [`Session`] -- the interaction state machine: which tool is active, what
//! gesture is in flight, and what becomes of each raw input event.
//!
//! This is the canvas equivalent of `browser::Browser`: every decision is
//! made here (headlessly testable), and the window painter is a straight
//! transcription onto pixels. It owns the [`Doc`], the [`Camera`], and the
//! note editor.

use crate::camera::Camera;
use crate::doc::Doc;
use crate::double_click::DoubleClick;
use crate::edit_note::NoteEditor;
use crate::handle::{CursorShape, Handle};
use crate::shape::Shape;
use platform::protocol::PointerButton;
use platform::toplevel::modifiers::ModifierState;
use platform::toplevel::pointer::{PointerEventKind, Position};
use platform::toplevel::{Key, KeyPress};

/// Which tool a click-drag wields.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tool {
    Select,
    Rect,
    Ellipse,
    Arrow,
    Note,
    Pan,
}

impl Tool {
    /// Every tool in toolbar order.
    pub const ALL: [Self; 6] = [
        Self::Select,
        Self::Rect,
        Self::Ellipse,
        Self::Arrow,
        Self::Note,
        Self::Pan,
    ];

    /// The tool's own keyboard shortcut.
    #[must_use]
    pub const fn shortcut(self) -> char {
        match self {
            Self::Select => 'v',
            Self::Rect => 'r',
            Self::Ellipse => 'o',
            Self::Arrow => 'a',
            Self::Note => 'n',
            Self::Pan => 'h',
        }
    }
}

/// A gesture in progress.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Gesture {
    /// Nothing in flight.
    Idle,
    /// Dragging out a new shape from `origin` to the live pointer.
    Draw {
        tool: Tool,
        origin: (f64, f64),
        current: (f64, f64),
    },
    /// Moving shape `index` (a snapshot of where it started).
    Move {
        index: usize,
        grab_offset: (f64, f64),
        current: (f64, f64),
    },
    /// Panning with the Pan tool (or the middle button).
    Pan { last: (i32, i32) },
    /// Resizing shape `index` by one of its eight handles.
    Resize { index: usize, handle: Handle },
}

impl Gesture {
    /// Whether this gesture, on release, commits an undo step.
    const fn commits(self) -> bool {
        matches!(
            self,
            Self::Draw { .. } | Self::Move { .. } | Self::Resize { .. }
        )
    }
}

/// The canvas session: document + camera + interaction state.
pub struct Session {
    pub doc: Doc,
    pub camera: Camera,
    tool: Tool,
    gesture: Gesture,
    /// The selected shape, if any (an index into `doc`).
    selected: Option<usize>,
    /// The note being edited, as an index + editor state. `None` when no
    /// note is in edit mode.
    editing: Option<(usize, NoteEditor)>,
    /// Snap pitch in world units; 0 disables.
    snap: i32,
    /// Press timing, so a double-click can reopen a note for editing.
    clicks: DoubleClick,
    /// Set when an edit finished and the document should be persisted.
    dirty: bool,
}

impl Default for Session {
    fn default() -> Self {
        Self::new(Doc::default(), Camera::default(), 8)
    }
}

impl Session {
    /// The smallest a note's box may be dragged, in world units. Text is
    /// freeform and the box only bounds hit-testing, so this is small.
    const NOTE_MIN: f64 = 16.0;

    /// Assemble a session from loaded parts.
    #[must_use]
    pub const fn new(doc: Doc, camera: Camera, snap: i32) -> Self {
        Self {
            doc,
            camera,
            tool: Tool::Select,
            gesture: Gesture::Idle,
            selected: None,
            editing: None,
            snap,
            clicks: DoubleClick::new(),
            dirty: false,
        }
    }

    /// The active tool.
    #[must_use]
    pub const fn tool(&self) -> Tool {
        self.tool
    }

    /// Switch the active tool, closing any open note edit first (a note
    /// edit is modal over the tool: Escape or a tool switch commits it).
    pub fn set_tool(&mut self, tool: Tool) {
        self.close_note();
        self.tool = tool;
        // The click that finished the last gesture must not pair with the
        // first click of the next one: they mean different things, and
        // fusing them reads as a spurious double-click.
        self.clicks.reset();
    }

    /// The selected shape index.
    #[must_use]
    pub const fn selected(&self) -> Option<usize> {
        self.selected
    }

    /// The note-edit state, if a note is being edited.
    #[must_use]
    pub fn editing_note(&self) -> Option<(usize, &NoteEditor)> {
        self.editing.as_ref().map(|(i, e)| (*i, e))
    }

    /// The live preview shape a `Draw` gesture would commit, if one is in
    /// flight. Rendered exactly like a committed shape.
    #[must_use]
    pub fn preview(&self) -> Option<Shape> {
        match self.gesture {
            Gesture::Draw {
                tool,
                origin,
                current,
            } => match tool {
                Tool::Rect => Some(Shape::Rect {
                    x: origin.0.min(current.0),
                    y: origin.1.min(current.1),
                    w: (current.0 - origin.0).abs(),
                    h: (current.1 - origin.1).abs(),
                }),
                Tool::Ellipse => Some(Shape::Ellipse {
                    x: origin.0.min(current.0),
                    y: origin.1.min(current.1),
                    w: (current.0 - origin.0).abs(),
                    h: (current.1 - origin.1).abs(),
                }),
                Tool::Arrow => Some(Shape::Arrow {
                    ax: origin.0,
                    ay: origin.1,
                    bx: current.0,
                    by: current.1,
                }),
                _ => None,
            },
            _ => None,
        }
    }

    /// Whether the document changed since the last persist.
    #[must_use]
    pub const fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// Mark the document persisted.
    pub const fn mark_saved(&mut self) {
        self.dirty = false;
    }

    /// The world-space slop for hit-testing at the current zoom, so the
    /// grab distance stays a constant number of screen pixels.
    fn hit_slop(&self) -> f64 {
        crate::shape::HIT_SLOP / self.camera.zoom()
    }

    /// Commit an open note edit back into the document.
    ///
    /// An empty note is removed rather than left behind: clicking with the
    /// text tool and typing nothing must not litter the canvas with an
    /// invisible, unclickable shape.
    pub fn close_note(&mut self) {
        let Some((index, editor)) = self.editing.take() else {
            return;
        };
        let text = editor.text();
        if text.is_empty() {
            let shapes = self.doc.shapes_mut();
            if index < shapes.len() && matches!(shapes[index], Shape::Note { .. }) {
                shapes.remove(index);
                self.selected = None;
            }
            return;
        }
        if let Some(Shape::Note { text: stored, .. }) = self.doc.shapes_mut().get_mut(index) {
            *stored = text;
        }
        self.dirty = true;
    }

    /// Reopen the note at `index` for editing, seeding the editor with its
    /// stored text. Returns whether `index` was a note.
    ///
    /// The text is read *before* closing any previous edit: `close_note`
    /// may delete an empty note, which shifts every later index -- so
    /// reading afterwards can seed the editor from the wrong shape, or from
    /// none at all.
    pub fn open_note(&mut self, index: usize) -> bool {
        let Some(Shape::Note { text, .. }) = self.doc.shapes().get(index) else {
            return false;
        };
        let mut editor = NoteEditor::new();
        editor.set_text(text);
        // Already editing this very note: leave the caret where it is
        // rather than jumping it to the end.
        if matches!(self.editing, Some((open, _)) if open == index) {
            return true;
        }
        self.close_note();
        // `close_note` may have removed an empty note that sat before this
        // one, so the index has to be re-resolved against the new list.
        let Some(index) = self.note_index_after_close(index) else {
            return false;
        };
        self.selected = Some(index);
        self.editing = Some((index, editor));
        true
    }

    /// Re-resolve `index` after a [`Self::close_note`] that may have removed
    /// an earlier shape.
    fn note_index_after_close(&self, index: usize) -> Option<usize> {
        if matches!(self.doc.shapes().get(index), Some(Shape::Note { .. })) {
            return Some(index);
        }
        // The only removal `close_note` performs is a single empty note, so
        // at most one index shifted down.
        let shifted = index.checked_sub(1)?;
        matches!(self.doc.shapes().get(shifted), Some(Shape::Note { .. })).then_some(shifted)
    }

    /// Resize the note being edited to fit `(w, h)` world units of text.
    ///
    /// The window measures the text (only it knows the font metrics) and
    /// reports the extent back; the box follows so hit-testing matches what
    /// is actually drawn. Freeform text has no visible border, so this box
    /// exists purely to be clickable.
    pub fn fit_note(&mut self, w: f64, h: f64) {
        let Some((index, _)) = &self.editing else {
            return;
        };
        let index = *index;
        if let Some(Shape::Note { w: nw, h: nh, .. }) = self.doc.shapes_mut().get_mut(index) {
            *nw = w.max(Self::NOTE_MIN);
            *nh = h.max(Self::NOTE_MIN);
        }
    }

    /// Feed one pointer event (physical pixels). Returns whether the
    /// session wants a repaint.
    pub fn pointer(&mut self, event: PointerEventKind) -> bool {
        match event {
            PointerEventKind::Press {
                button,
                at,
                modifiers,
            } => self.press(button, at, modifiers),
            PointerEventKind::Motion(at) => self.motion(at),
            PointerEventKind::Release { button, at, .. } => self.release(button, at),
            PointerEventKind::Scroll {
                lines,
                at,
                modifiers,
            } => self.scroll(lines, at, modifiers),
            PointerEventKind::Enter(_) | PointerEventKind::Leave => false,
        }
    }

    fn press(&mut self, button: PointerButton, at: Position, modifiers: ModifierState) -> bool {
        let screen = (at.x, at.y);
        let double = button == PointerButton::Left && self.clicks.press(screen);
        // Middle button always pans, whatever the tool.
        if button == PointerButton::Middle {
            self.gesture = Gesture::Pan { last: screen };
            return true;
        }
        if button != PointerButton::Left {
            return false;
        }
        // Space held pans, whatever the tool (the universal convention).
        if modifiers.ctrl() && self.tool == Tool::Select {
            // Ctrl+click on a selection: reserved for future context menus.
            return false;
        }
        let world = self.camera.to_world(screen);
        let world = self.snap_point(world);
        match self.tool {
            Tool::Pan => {
                self.gesture = Gesture::Pan { last: screen };
                true
            }
            Tool::Select => self.select_press(world, double),
            Tool::Rect | Tool::Ellipse | Tool::Arrow => {
                self.gesture = Gesture::Draw {
                    tool: self.tool,
                    origin: world,
                    current: world,
                };
                true
            }
            Tool::Note => self.note_press(world),
        }
    }

    fn select_press(&mut self, world: (f64, f64), double: bool) -> bool {
        // A double-click on a note reopens it for editing, which is how
        // every canvas tool behaves and the only way back into a note once
        // the select tool is active.
        if double
            && let Some(index) = self.doc.hit_top(world, self.hit_slop())
            && self.open_note(index)
        {
            self.gesture = Gesture::Idle;
            return true;
        }
        // A click inside the note being edited just moves its caret.
        if let Some((index, editor)) = &mut self.editing
            && let Some(Shape::Note { x, y, w, h, .. }) = self.doc.shapes().get(*index)
            && world.0 >= *x
            && world.0 <= *x + *w
            && world.1 >= *y
            && world.1 <= *y + *h
        {
            editor.move_end();
            return true;
        }
        // A handle on the selected shape outranks everything: it is drawn
        // on top, so it must be grabbed first.
        if let Some(index) = self.selected
            && let Some(handle) = self.grab_handle(index, world)
            && handle != Handle::Body
        {
            self.gesture = Gesture::Resize { index, handle };
            return true;
        }
        if let Some(index) = self.doc.hit_top(world, self.hit_slop()) {
            self.select(Some(index));
            let (bx, by, _, _) = self.doc.shapes()[index].bounds();
            self.gesture = Gesture::Move {
                index,
                grab_offset: (world.0 - bx, world.1 - by),
                current: world,
            };
        } else {
            self.select(None);
        }
        true
    }

    /// Move the selection, committing any open note edit that it leaves.
    ///
    /// Selection and editing are not independent: a note is edited *because*
    /// it is selected. Setting `selected` directly leaves `editing` pointing
    /// at a note that is no longer selected, and the window keeps painting
    /// its caret over a note nobody is editing.
    fn select(&mut self, index: Option<usize>) {
        if matches!(self.editing, Some((open, _)) if Some(open) != index) {
            self.close_note();
        }
        self.selected = index;
    }

    /// Which handle of shape `index` `world` grabs, if any.
    ///
    /// The grab zones scale with zoom so they stay a constant size on
    /// screen; see [`Handle::hit`].
    fn grab_handle(&self, index: usize, world: (f64, f64)) -> Option<Handle> {
        let shape = self.doc.shapes().get(index)?;
        Handle::hit(shape.bounds(), world, 1.0 / self.camera.zoom())
    }

    /// The cursor the pointer should show at screen position `screen`.
    ///
    /// Consulted every motion event, so it must stay allocation-free and
    /// cheap: it is a hit test against at most one selected shape plus the
    /// document's top hit.
    #[must_use]
    pub fn cursor_at(&self, screen: (i32, i32)) -> CursorShape {
        if matches!(self.gesture, Gesture::Pan { .. }) {
            return CursorShape::Grabbing;
        }
        if let Gesture::Resize { handle, .. } = self.gesture {
            return handle.cursor();
        }
        if matches!(self.gesture, Gesture::Move { .. }) {
            return CursorShape::Move;
        }
        let world = self.camera.to_world(screen);
        match self.tool {
            Tool::Pan => CursorShape::Grab,
            Tool::Rect | Tool::Ellipse | Tool::Arrow => CursorShape::Crosshair,
            Tool::Note => CursorShape::Text,
            Tool::Select => self.select_cursor(world),
        }
    }

    /// The cursor for the select tool, which depends on what is under the
    /// pointer rather than on the tool alone.
    fn select_cursor(&self, world: (f64, f64)) -> CursorShape {
        if let Some(index) = self.selected
            && let Some(handle) = self.grab_handle(index, world)
        {
            return handle.cursor();
        }
        if self.doc.hit_top(world, self.hit_slop()).is_some() {
            return CursorShape::Move;
        }
        CursorShape::Default
    }

    fn note_press(&mut self, world: (f64, f64)) -> bool {
        // A click on an existing note reopens it for editing.
        if let Some(index) = self.doc.hit_top(world, self.hit_slop())
            && self.open_note(index)
        {
            return true;
        }
        // Otherwise spawn a fresh note at the click and edit it. The box
        // starts at the minimum: text is freeform, so the note grows to fit
        // what is typed rather than reserving a card up front.
        self.close_note();
        let shapes = self.doc.begin();
        shapes.push(Shape::Note {
            x: world.0,
            y: world.1,
            w: Self::NOTE_MIN,
            h: Self::NOTE_MIN,
            text: String::new(),
        });
        let index = shapes.len() - 1;
        self.selected = Some(index);
        self.editing = Some((index, NoteEditor::new()));
        self.dirty = true;
        true
    }

    fn motion(&mut self, at: Position) -> bool {
        let screen = (at.x, at.y);
        match self.gesture {
            Gesture::Idle => false,
            Gesture::Draw { current, .. } => {
                if let Gesture::Draw { tool, origin, .. } = self.gesture {
                    let world = self.snap_point(self.camera.to_world(screen));
                    self.gesture = Gesture::Draw {
                        tool,
                        origin,
                        current: world,
                    };
                }
                let _ = current;
                true
            }
            Gesture::Move {
                index, grab_offset, ..
            } => {
                let world = self.snap_point(self.camera.to_world(screen));
                let target = (world.0 - grab_offset.0, world.1 - grab_offset.1);
                if let Some(shape) = self.doc.shapes_mut().get_mut(index) {
                    let (bx, by, _, _) = shape.bounds();
                    shape.translate(target.0 - bx, target.1 - by);
                }
                self.gesture = Gesture::Move {
                    index,
                    grab_offset,
                    current: world,
                };
                true
            }
            Gesture::Resize { index, handle } => {
                let world = self.snap_point(self.camera.to_world(screen));
                let Some(shape) = self.doc.shapes_mut().get_mut(index) else {
                    return true;
                };
                match shape {
                    // An arrow has no box to resize: a handle drags whichever
                    // endpoint it is nearest, which is what the two visible
                    // handles on a line mean.
                    Shape::Arrow { ax, ay, bx, by } => {
                        if handle.moves_left() || handle.moves_top() {
                            *ax = world.0;
                            *ay = world.1;
                        } else {
                            *bx = world.0;
                            *by = world.1;
                        }
                    }
                    Shape::Rect { x, y, w, h } | Shape::Ellipse { x, y, w, h } => {
                        let (nx, ny, nw, nh) = handle.resize((*x, *y, *w, *h), world, 1.0);
                        (*x, *y, *w, *h) = (nx, ny, nw, nh);
                    }
                    Shape::Note { x, y, w, h, .. } => {
                        let (nx, ny, nw, nh) =
                            handle.resize((*x, *y, *w, *h), world, Self::NOTE_MIN);
                        (*x, *y, *w, *h) = (nx, ny, nw, nh);
                    }
                }
                true
            }
            Gesture::Pan { last } => {
                let (dx, dy) = (screen.0 - last.0, screen.1 - last.1);
                self.camera.pan_by(dx, dy);
                self.gesture = Gesture::Pan { last: screen };
                true
            }
        }
    }

    fn release(&mut self, button: PointerButton, _at: Position) -> bool {
        let gesture = self.gesture;
        // Only the button that owns the gesture may end it.
        let owns = match gesture {
            Gesture::Pan { .. } => button == PointerButton::Middle || self.tool == Tool::Pan,
            _ => button == PointerButton::Left,
        };
        if !owns {
            return false;
        }
        match gesture {
            Gesture::Idle => false,
            Gesture::Draw {
                tool,
                origin,
                current,
            } => {
                let too_small =
                    (current.0 - origin.0).abs() < 3.0 && (current.1 - origin.1).abs() < 3.0;
                if too_small {
                    // A click, not a drag: don't litter the canvas with
                    // zero-size shapes.
                    self.gesture = Gesture::Idle;
                    return true;
                }
                let shape = match tool {
                    Tool::Rect => Shape::Rect {
                        x: origin.0.min(current.0),
                        y: origin.1.min(current.1),
                        w: (current.0 - origin.0).abs().max(1.0),
                        h: (current.1 - origin.1).abs().max(1.0),
                    },
                    Tool::Ellipse => Shape::Ellipse {
                        x: origin.0.min(current.0),
                        y: origin.1.min(current.1),
                        w: (current.0 - origin.0).abs().max(1.0),
                        h: (current.1 - origin.1).abs().max(1.0),
                    },
                    Tool::Arrow => Shape::Arrow {
                        ax: origin.0,
                        ay: origin.1,
                        bx: current.0,
                        by: current.1,
                    },
                    _ => unreachable!("draw gesture only exists for shape tools"),
                };
                let shapes = self.doc.begin();
                shapes.push(shape);
                let index = shapes.len() - 1;
                self.selected = Some(index);
                self.gesture = Gesture::Idle;
                self.dirty = true;
                true
            }
            Gesture::Move { .. } | Gesture::Resize { .. } => {
                if gesture.commits() {
                    self.dirty = true;
                }
                self.gesture = Gesture::Idle;
                true
            }
            Gesture::Pan { .. } => {
                self.gesture = Gesture::Idle;
                true
            }
        }
    }

    fn scroll(&mut self, lines: i32, at: Position, modifiers: ModifierState) -> bool {
        if modifiers.ctrl() {
            // Ctrl+wheel zooms, anchored at the cursor.
            let factor = if lines < 0 { 1.1 } else { 1.0 / 1.1 };
            self.camera.zoom_at((at.x, at.y), factor)
        } else {
            // Plain wheel pans vertically.
            self.camera.pan_by(0, lines * 40);
            true
        }
    }

    /// Snap a world point down to the grid when snapping is on.
    ///
    /// Snapping floors rather than rounds, so a point always belongs to the
    /// grid cell that contains it. Rounding would move a point across a
    /// cell boundary, which makes a shape's corner disagree with the cell
    /// the user dragged in.
    fn snap_point(&self, world: (f64, f64)) -> (f64, f64) {
        if self.snap <= 0 {
            return world;
        }
        let pitch = f64::from(self.snap);
        (
            (world.0 / pitch).floor() * pitch,
            (world.1 / pitch).floor() * pitch,
        )
    }

    /// Feed one key event. Returns whether the session wants a repaint.
    pub fn key(&mut self, press: KeyPress) -> bool {
        // A note edit consumes nearly everything.
        if let Some((_, editor)) = &mut self.editing {
            let consumed = match press.key {
                Key::Char(c) => {
                    editor.insert(c);
                    true
                }
                Key::Enter => {
                    editor.insert('\n');
                    true
                }
                Key::Backspace => {
                    editor.backspace();
                    true
                }
                Key::Delete => {
                    editor.delete();
                    true
                }
                Key::Left => {
                    editor.move_left();
                    true
                }
                Key::Right => {
                    editor.move_right();
                    true
                }
                Key::Up => {
                    editor.move_up();
                    true
                }
                Key::Down => {
                    editor.move_down();
                    true
                }
                Key::Home => {
                    editor.move_home();
                    true
                }
                Key::End => {
                    editor.move_end();
                    true
                }
                Key::Escape => {
                    self.close_note();
                    true
                }
                Key::Tab => {
                    editor.insert(' ');
                    true
                }
                _ => false,
            };
            if consumed {
                self.dirty = true;
                return true;
            }
            return false;
        }
        match press.key {
            Key::Escape => {
                self.select(None);
                true
            }
            Key::Delete | Key::Backspace => self.delete_selected(),
            Key::Char('z') if press.modifiers.ctrl() => self.doc.undo(),
            Key::Char('y') if press.modifiers.ctrl() => self.doc.redo(),
            Key::Char('c') if press.modifiers.ctrl() && press.modifiers.shift() => false,
            Key::Char('0') => {
                self.camera.reset();
                true
            }
            Key::Up if press.modifiers.ctrl() => self.camera.zoom_step((0, 0), true),
            Key::Down if press.modifiers.ctrl() => self.camera.zoom_step((0, 0), false),
            Key::Char(c) => Tool::ALL
                .iter()
                .find(|t| t.shortcut() == c)
                .is_some_and(|tool| {
                    self.set_tool(*tool);
                    true
                }),
            _ => false,
        }
    }

    fn delete_selected(&mut self) -> bool {
        let Some(index) = self.selected.take() else {
            return false;
        };
        if index >= self.doc.len() {
            return false;
        }
        let shapes = self.doc.begin();
        shapes.remove(index);
        self.dirty = true;
        true
    }
}

#[cfg(test)]
#[allow(clippy::float_cmp)]
mod tests {
    use super::*;

    fn session() -> Session {
        Session::default()
    }

    fn press_left_at(session: &mut Session, x: i32, y: i32) -> bool {
        session.pointer(PointerEventKind::Press {
            button: PointerButton::Left,
            at: Position { x, y },
            modifiers: ModifierState::default(),
        })
    }

    fn drag(session: &mut Session, from: (i32, i32), to: (i32, i32)) {
        press_left_at(session, from.0, from.1);
        session.pointer(PointerEventKind::Motion(Position { x: to.0, y: to.1 }));
        session.pointer(PointerEventKind::Release {
            button: PointerButton::Left,
            at: Position { x: to.0, y: to.1 },
            modifiers: ModifierState::default(),
        });
    }

    #[test]
    fn dragging_a_rect_tool_commits_one_rect() {
        let mut s = session();
        s.set_tool(Tool::Rect);
        drag(&mut s, (10, 10), (110, 60));
        assert_eq!(s.doc.len(), 1);
        assert!(matches!(s.doc.shapes()[0], Shape::Rect { .. }));
        if let Shape::Rect { x, y, w, h } = s.doc.shapes()[0] {
            assert_eq!(
                (x, y, w, h),
                (8.0, 8.0, 96.0, 48.0),
                "snapped to the 8px grid"
            );
        }
        assert!(s.is_dirty());
    }

    #[test]
    fn dragging_up_left_still_makes_a_positive_rect() {
        let mut s = session();
        s.set_tool(Tool::Rect);
        drag(&mut s, (110, 60), (10, 10));
        if let Shape::Rect { x, y, w, h } = s.doc.shapes()[0] {
            assert_eq!((x, y, w, h), (8.0, 8.0, 96.0, 48.0));
        }
    }

    #[test]
    fn a_click_with_a_shape_tool_commits_nothing() {
        let mut s = session();
        s.set_tool(Tool::Ellipse);
        drag(&mut s, (50, 50), (52, 51));
        assert!(s.doc.is_empty(), "a stray click must not litter the canvas");
    }

    #[test]
    fn drawing_an_arrow_keeps_its_direction() {
        let mut s = session();
        s.set_tool(Tool::Arrow);
        drag(&mut s, (0, 0), (80, 40));
        if let Shape::Arrow { ax, ay, bx, by } = s.doc.shapes()[0] {
            assert_eq!((ax, ay), (0.0, 0.0));
            assert_eq!((bx, by), (80.0, 40.0));
        } else {
            panic!("expected an arrow");
        }
    }

    #[test]
    fn the_note_tool_creates_and_edits_a_note() {
        let mut s = session();
        s.set_tool(Tool::Note);
        assert!(press_left_at(&mut s, 40, 40));
        assert!(s.editing_note().is_some());
        // Type into it.
        for c in "hi".chars() {
            s.key(KeyPress {
                key: Key::Char(c),
                modifiers: ModifierState::default(),
                repeat: false,
            });
        }
        // Escape commits the edit.
        s.key(KeyPress {
            key: Key::Escape,
            modifiers: ModifierState::default(),
            repeat: false,
        });
        assert!(s.editing_note().is_none());
        if let Shape::Note { text, .. } = &s.doc.shapes()[0] {
            assert_eq!(text, "hi");
        }
    }

    #[test]
    fn clicking_a_note_with_the_note_tool_reopens_it() {
        let mut s = session();
        s.set_tool(Tool::Note);
        press_left_at(&mut s, 40, 40);
        s.key(KeyPress {
            key: Key::Char('a'),
            modifiers: ModifierState::default(),
            repeat: false,
        });
        s.key(KeyPress {
            key: Key::Escape,
            modifiers: ModifierState::default(),
            repeat: false,
        });
        // Click again inside the note.
        press_left_at(&mut s, 50, 50);
        assert!(s.editing_note().is_some(), "reopened");
    }

    #[test]
    fn select_moves_a_shape() {
        let mut s = session();
        s.set_tool(Tool::Rect);
        drag(&mut s, (10, 10), (110, 60));
        s.set_tool(Tool::Select);
        // Grab near the left edge and drag right by 40.
        drag(&mut s, (10, 30), (50, 30));
        if let Shape::Rect { x, .. } = s.doc.shapes()[0] {
            assert!((x - 48.0).abs() < 1e-9, "moved right, got {x}");
        }
    }

    #[test]
    fn selecting_empty_space_clears_the_selection() {
        let mut s = session();
        s.set_tool(Tool::Rect);
        drag(&mut s, (10, 10), (110, 60));
        assert!(s.selected().is_some());
        // Only the select tool clears a selection: a shape tool would
        // start drawing instead.
        s.set_tool(Tool::Select);
        press_left_at(&mut s, 500, 500);
        assert!(s.selected().is_none());
    }

    #[test]
    fn delete_removes_the_selected_shape_and_undo_restores_it() {
        let mut s = session();
        s.set_tool(Tool::Rect);
        drag(&mut s, (10, 10), (110, 60));
        s.key(KeyPress {
            key: Key::Delete,
            modifiers: ModifierState::default(),
            repeat: false,
        });
        assert!(s.doc.is_empty());
        s.key(KeyPress {
            key: Key::Char('z'),
            modifiers: ModifierState::from_wire(1 << 2, 0, 0, 0),
            repeat: false,
        });
        assert_eq!(s.doc.len(), 1, "undo restored the deleted rect");
    }

    #[test]
    fn the_middle_button_pans_from_any_tool() {
        let mut s = session();
        s.set_tool(Tool::Rect);
        s.pointer(PointerEventKind::Press {
            button: PointerButton::Middle,
            at: Position { x: 0, y: 0 },
            modifiers: ModifierState::default(),
        });
        s.pointer(PointerEventKind::Motion(Position { x: -30, y: 20 }));
        s.pointer(PointerEventKind::Release {
            button: PointerButton::Middle,
            at: Position { x: -30, y: 20 },
            modifiers: ModifierState::default(),
        });
        // The same world point now sits 30px right / 20px up on screen.
        assert_eq!(s.camera.to_screen((0.0, 0.0)), (-30, 20));
        assert!(s.doc.is_empty(), "panning must not draw");
    }

    #[test]
    fn plain_scroll_pans_and_ctrl_scroll_zooms_at_the_cursor() {
        let mut s = session();
        s.pointer(PointerEventKind::Scroll {
            lines: 3,
            at: Position { x: 0, y: 0 },
            modifiers: ModifierState::default(),
        });
        assert_eq!(s.camera.to_screen((0.0, 0.0)).1, 120, "panned down");
        let ctrl = ModifierState::from_wire(1 << 2, 0, 0, 0);
        s.pointer(PointerEventKind::Scroll {
            lines: -1,
            at: Position { x: 100, y: 100 },
            modifiers: ctrl,
        });
        assert!(s.camera.zoom() > 1.0, "ctrl+wheel zoomed in");
        // The anchor stayed fixed.
        let world = s.camera.to_world((100, 100));
        assert!((world.0 - 100.0).abs() < 1e-6, "anchor drifted: {world:?}");
    }

    #[test]
    fn an_open_note_takes_priority_over_tool_shortcuts() {
        let mut s = session();
        s.set_tool(Tool::Note);
        press_left_at(&mut s, 40, 40);
        assert!(s.editing_note().is_some());
        // While a note is open, a bare letter is text, not a shortcut.
        s.key(KeyPress {
            key: Key::Char('v'),
            modifiers: ModifierState::default(),
            repeat: false,
        });
        assert_eq!(s.tool(), Tool::Note, "still typing, not switching tools");
        assert_eq!(
            s.editing_note().map(|(_, e)| e.text()),
            Some("v".to_owned())
        );
        // Escape commits the note; now the shortcut applies.
        s.key(KeyPress {
            key: Key::Escape,
            modifiers: ModifierState::default(),
            repeat: false,
        });
        assert!(s.editing_note().is_none());
        s.key(KeyPress {
            key: Key::Char('v'),
            modifiers: ModifierState::default(),
            repeat: false,
        });
        assert_eq!(s.tool(), Tool::Select);
    }

    #[test]
    fn a_draw_gesture_snaps_to_the_grid() {
        let mut s = Session::new(Doc::default(), Camera::default(), 16);
        s.set_tool(Tool::Rect);
        drag(&mut s, (5, 5), (50, 41));
        if let Shape::Rect { x, y, w, h } = s.doc.shapes()[0] {
            assert_eq!((x, y), (0.0, 0.0), "origin snapped to 0,0");
            assert_eq!(w, 48.0);
            assert_eq!(h, 32.0);
        }
    }

    #[test]
    fn zero_snap_disables_snapping() {
        let mut s = Session::new(Doc::default(), Camera::default(), 0);
        s.set_tool(Tool::Rect);
        drag(&mut s, (5, 5), (50, 41));
        if let Shape::Rect { x, y, .. } = s.doc.shapes()[0] {
            assert_eq!((x, y), (5.0, 5.0));
        }
    }

    #[test]
    fn undo_steps_back_through_draws() {
        let mut s = session();
        s.set_tool(Tool::Rect);
        drag(&mut s, (10, 10), (110, 60));
        drag(&mut s, (20, 20), (120, 70));
        assert_eq!(s.doc.len(), 2);
        let ctrl = ModifierState::from_wire(1 << 2, 0, 0, 0);
        s.key(KeyPress {
            key: Key::Char('z'),
            modifiers: ctrl,
            repeat: false,
        });
        assert_eq!(s.doc.len(), 1);
        s.key(KeyPress {
            key: Key::Char('y'),
            modifiers: ctrl,
            repeat: false,
        });
        assert_eq!(s.doc.len(), 2);
    }

    #[test]
    fn resizing_the_selected_shape_via_its_corner_handle() {
        let mut s = session();
        s.set_tool(Tool::Rect);
        drag(&mut s, (0, 0), (100, 50));
        // The handle is only grabbable with the select tool.
        s.set_tool(Tool::Select);
        // The bottom-right handle sits at world (96, 48) after snap.
        press_left_at(&mut s, 96, 48);
        assert!(
            matches!(s.gesture, Gesture::Resize { .. }),
            "the corner press must start a resize, got {:?}",
            s.gesture
        );
        s.pointer(PointerEventKind::Motion(Position { x: 200, y: 120 }));
        s.pointer(PointerEventKind::Release {
            button: PointerButton::Left,
            at: Position { x: 200, y: 120 },
            modifiers: ModifierState::default(),
        });
        if let Shape::Rect { w, h, .. } = s.doc.shapes()[0] {
            assert_eq!(w, 200.0, "resized wider");
            assert_eq!(h, 120.0, "resized taller");
        }
    }

    #[test]
    #[allow(clippy::cast_possible_truncation)]
    fn every_corner_and_edge_of_a_selection_is_grabbable() {
        let mut s = session();
        s.set_tool(Tool::Rect);
        drag(&mut s, (0, 0), (100, 50));
        s.set_tool(Tool::Select);
        let bounds = s.doc.shapes()[0].bounds();
        for handle in Handle::RESIZE {
            let (ax, ay) = handle.anchor(bounds);
            let at = (ax.round() as i32, ay.round() as i32);
            press_left_at(&mut s, at.0, at.1);
            assert!(
                matches!(s.gesture, Gesture::Resize { handle: h, .. } if h == handle),
                "{handle:?} at {at:?} did not start a resize, got {:?}",
                s.gesture
            );
            s.pointer(PointerEventKind::Release {
                button: PointerButton::Left,
                at: Position { x: at.0, y: at.1 },
                modifiers: ModifierState::default(),
            });
        }
    }

    #[test]
    fn dragging_the_left_edge_moves_the_origin_not_the_far_edge() {
        let mut s = session();
        s.set_tool(Tool::Rect);
        drag(&mut s, (0, 0), (100, 50));
        s.set_tool(Tool::Select);
        // Grab the left edge midpoint and pull it right by 32.
        drag(&mut s, (0, 24), (32, 24));
        if let Shape::Rect { x, w, .. } = s.doc.shapes()[0] {
            assert_eq!(x, 32.0, "the left edge must move");
            assert_eq!(x + w, 96.0, "the right edge must stay put");
        } else {
            panic!("expected a rect");
        }
    }

    #[test]
    fn the_pointer_reports_a_resize_cursor_over_a_handle() {
        let mut s = session();
        s.set_tool(Tool::Rect);
        drag(&mut s, (0, 0), (100, 50));
        s.set_tool(Tool::Select);
        assert_eq!(s.cursor_at((96, 48)), CursorShape::ResizeNwse);
        assert_eq!(s.cursor_at((0, 24)), CursorShape::ResizeEw);
        assert_eq!(s.cursor_at((48, 0)), CursorShape::ResizeNs);
    }

    #[test]
    fn the_pointer_reports_a_move_cursor_over_a_shapes_body() {
        let mut s = session();
        s.set_tool(Tool::Rect);
        drag(&mut s, (0, 0), (100, 50));
        s.set_tool(Tool::Select);
        assert_eq!(s.cursor_at((48, 24)), CursorShape::Move);
    }

    #[test]
    fn the_pointer_reports_a_plain_cursor_over_empty_space() {
        let mut s = session();
        s.set_tool(Tool::Select);
        assert_eq!(s.cursor_at((500, 500)), CursorShape::Default);
    }

    #[test]
    fn each_tool_advertises_its_own_cursor() {
        let mut s = session();
        s.set_tool(Tool::Rect);
        assert_eq!(s.cursor_at((500, 500)), CursorShape::Crosshair);
        s.set_tool(Tool::Note);
        assert_eq!(s.cursor_at((500, 500)), CursorShape::Text);
        s.set_tool(Tool::Pan);
        assert_eq!(s.cursor_at((500, 500)), CursorShape::Grab);
    }

    #[test]
    fn panning_shows_a_closed_hand_until_the_button_is_released() {
        let mut s = session();
        s.set_tool(Tool::Pan);
        press_left_at(&mut s, 100, 100);
        assert_eq!(s.cursor_at((100, 100)), CursorShape::Grabbing);
        s.pointer(PointerEventKind::Release {
            button: PointerButton::Left,
            at: Position { x: 100, y: 100 },
            modifiers: ModifierState::default(),
        });
        assert_eq!(s.cursor_at((100, 100)), CursorShape::Grab);
    }

    /// Write `text` into a fresh note at `(x, y)` and commit it.
    fn note_with(s: &mut Session, x: i32, y: i32, text: &str) {
        s.set_tool(Tool::Note);
        press_left_at(s, x, y);
        for c in text.chars() {
            s.key(KeyPress {
                key: Key::Char(c),
                modifiers: ModifierState::default(),
                repeat: false,
            });
        }
        s.key(KeyPress {
            key: Key::Escape,
            modifiers: ModifierState::default(),
            repeat: false,
        });
    }

    /// Two presses close together in time and space.
    fn double_click_at(s: &mut Session, x: i32, y: i32) {
        press_left_at(s, x, y);
        s.pointer(PointerEventKind::Release {
            button: PointerButton::Left,
            at: Position { x, y },
            modifiers: ModifierState::default(),
        });
        press_left_at(s, x, y);
    }

    #[test]
    fn a_committed_note_can_be_reopened_with_its_text_intact() {
        let mut s = session();
        note_with(&mut s, 40, 40, "hi");
        assert!(s.editing_note().is_none(), "the note was committed");
        // Clicking it again with the note tool reopens it.
        press_left_at(&mut s, 42, 42);
        let (_, editor) = s.editing_note().expect("the note reopened");
        assert_eq!(editor.text(), "hi", "the stored text must be restored");
    }

    #[test]
    fn a_double_click_with_the_select_tool_reopens_a_note() {
        let mut s = session();
        note_with(&mut s, 40, 40, "hi");
        s.set_tool(Tool::Select);
        double_click_at(&mut s, 42, 42);
        let (_, editor) = s.editing_note().expect("a double-click must reopen");
        assert_eq!(editor.text(), "hi");
    }

    #[test]
    fn a_single_click_with_the_select_tool_only_selects() {
        let mut s = session();
        note_with(&mut s, 40, 40, "hi");
        s.set_tool(Tool::Select);
        press_left_at(&mut s, 42, 42);
        assert!(
            s.editing_note().is_none(),
            "one click selects; it must not start editing"
        );
        assert!(s.selected().is_some());
    }

    #[test]
    fn reopening_appends_rather_than_replacing() {
        let mut s = session();
        note_with(&mut s, 40, 40, "ab");
        press_left_at(&mut s, 42, 42);
        s.key(KeyPress {
            key: Key::Char('c'),
            modifiers: ModifierState::default(),
            repeat: false,
        });
        let (_, editor) = s.editing_note().expect("still editing");
        assert_eq!(editor.text(), "abc", "the caret must start at the end");
    }

    #[test]
    fn backspace_in_a_reopened_note_deletes_the_last_character() {
        let mut s = session();
        note_with(&mut s, 40, 40, "abc");
        press_left_at(&mut s, 42, 42);
        s.key(KeyPress {
            key: Key::Backspace,
            modifiers: ModifierState::default(),
            repeat: false,
        });
        let (_, editor) = s.editing_note().expect("still editing");
        assert_eq!(editor.text(), "ab");
    }

    #[test]
    fn every_key_of_an_edit_reports_a_repaint() {
        // A key that edits but reports no repaint leaves the glyph on
        // screen until some other event happens, which reads as lag.
        let mut s = session();
        s.set_tool(Tool::Note);
        press_left_at(&mut s, 40, 40);
        for key in [
            Key::Char('a'),
            Key::Char('b'),
            Key::Backspace,
            Key::Left,
            Key::Right,
            Key::Home,
            Key::End,
            Key::Delete,
            Key::Enter,
        ] {
            assert!(
                s.key(KeyPress {
                    key,
                    modifiers: ModifierState::default(),
                    repeat: false,
                }),
                "{key:?} edited without asking for a repaint"
            );
        }
    }

    #[test]
    fn reopening_the_note_already_open_keeps_the_caret_where_it_is() {
        let mut s = session();
        s.set_tool(Tool::Note);
        press_left_at(&mut s, 40, 40);
        for c in "abc".chars() {
            s.key(KeyPress {
                key: Key::Char(c),
                modifiers: ModifierState::default(),
                repeat: false,
            });
        }
        s.key(KeyPress {
            key: Key::Home,
            modifiers: ModifierState::default(),
            repeat: false,
        });
        let index = s.selected().expect("the note is selected");
        assert!(s.open_note(index));
        let (_, editor) = s.editing_note().expect("still editing");
        assert_eq!(editor.col, 0, "the caret must not jump to the end");
    }

    #[test]
    fn a_note_left_empty_is_removed_rather_than_littering_the_canvas() {
        let mut s = session();
        s.set_tool(Tool::Note);
        press_left_at(&mut s, 40, 40);
        assert_eq!(s.doc.len(), 1, "the note exists while being edited");
        s.key(KeyPress {
            key: Key::Escape,
            modifiers: ModifierState::default(),
            repeat: false,
        });
        assert!(s.doc.is_empty(), "an empty note must not survive");
        assert!(s.selected().is_none());
    }

    #[test]
    fn a_note_with_text_survives_being_closed() {
        let mut s = session();
        s.set_tool(Tool::Note);
        press_left_at(&mut s, 40, 40);
        s.key(KeyPress {
            key: Key::Char('x'),
            modifiers: ModifierState::default(),
            repeat: false,
        });
        s.key(KeyPress {
            key: Key::Escape,
            modifiers: ModifierState::default(),
            repeat: false,
        });
        assert_eq!(s.doc.len(), 1);
    }

    #[test]
    fn fitting_a_note_grows_its_box_to_the_measured_text() {
        let mut s = session();
        s.set_tool(Tool::Note);
        press_left_at(&mut s, 40, 40);
        s.fit_note(240.0, 72.0);
        if let Shape::Note { w, h, .. } = &s.doc.shapes()[0] {
            assert_eq!((*w, *h), (240.0, 72.0));
        } else {
            panic!("expected a note");
        }
    }

    #[test]
    fn a_note_never_shrinks_below_its_clickable_minimum() {
        let mut s = session();
        s.set_tool(Tool::Note);
        press_left_at(&mut s, 40, 40);
        s.fit_note(0.0, 0.0);
        if let Shape::Note { w, h, .. } = &s.doc.shapes()[0] {
            assert!(*w >= Session::NOTE_MIN && *h >= Session::NOTE_MIN);
        }
    }

    #[test]
    fn escape_clears_the_selection() {
        let mut s = session();
        s.set_tool(Tool::Rect);
        drag(&mut s, (10, 10), (110, 60));
        assert!(s.selected().is_some());
        s.key(KeyPress {
            key: Key::Escape,
            modifiers: ModifierState::default(),
            repeat: false,
        });
        assert!(s.selected().is_none());
    }
}
