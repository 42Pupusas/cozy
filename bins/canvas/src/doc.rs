//! [`Doc`] -- the ordered list of shapes plus the undo/redo history.
//!
//! The document is the single source of truth for *what is on the canvas*.
//! Every mutation goes through [`Doc::commit`], which snapshots the shape
//! list into the undo stack first. The camera is deliberately not part of
//! the document: panning is a view concern, and nobody expects undo to
//! move their viewport back.

use crate::shape::Shape;

/// How many undo steps to retain. Deep enough for a long editing session;
/// shallow enough that a huge diagram does not retain unbounded memory.
const HISTORY_LIMIT: usize = 100;

/// The canvas document: an ordered shape list plus undo/redo stacks.
#[derive(Debug, Clone, Default)]
pub struct Doc {
    shapes: Vec<Shape>,
    undo: Vec<Vec<Shape>>,
    redo: Vec<Vec<Shape>>,
}

impl Doc {
    /// A document pre-loaded with `shapes` (no history: loading a file is
    /// not undoable back to an empty canvas -- the file *is* the base
    /// state).
    #[must_use]
    pub const fn with_shapes(shapes: Vec<Shape>) -> Self {
        Self {
            shapes,
            undo: Vec::new(),
            redo: Vec::new(),
        }
    }

    /// The shapes in z-order (later = on top).
    #[must_use]
    pub fn shapes(&self) -> &[Shape] {
        &self.shapes
    }

    /// The shapes mutably, for direct edits that have already been
    /// snapshotted via [`Self::begin`].
    pub const fn shapes_mut(&mut self) -> &mut Vec<Shape> {
        &mut self.shapes
    }

    /// The topmost shape a world point hits, if any. Search from the top
    /// down so overlapping shapes select the visually-front one.
    #[must_use]
    pub fn hit_top(&self, p: (f64, f64), slop: f64) -> Option<usize> {
        self.shapes.iter().rposition(|s| s.hits(p, slop))
    }

    /// Begin a mutation: snapshot the current state onto the undo stack.
    /// The caller then mutates through the returned borrow.
    ///
    /// Coalescing is the caller's job: a drag commits once at drag end,
    /// not per motion event, so no history compression is needed here.
    pub fn begin(&mut self) -> &mut Vec<Shape> {
        self.push_undo_snapshot();
        self.redo.clear();
        &mut self.shapes
    }

    /// The number of shapes.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.shapes.len()
    }

    /// Whether the document is empty.
    #[cfg(test)]
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.shapes.is_empty()
    }

    /// Whether an undo is available.
    #[cfg(test)]
    #[must_use]
    pub const fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    /// Whether a redo is available.
    #[cfg(test)]
    #[must_use]
    pub const fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    /// Revert the last committed change. Returns whether anything happened.
    pub fn undo(&mut self) -> bool {
        let Some(previous) = self.undo.pop() else {
            return false;
        };
        self.redo
            .push(std::mem::replace(&mut self.shapes, previous));
        true
    }

    /// Re-apply the last undone change. Returns whether anything happened.
    pub fn redo(&mut self) -> bool {
        let Some(next) = self.redo.pop() else {
            return false;
        };
        self.undo.push(std::mem::replace(&mut self.shapes, next));
        true
    }

    fn push_undo_snapshot(&mut self) {
        self.undo.push(self.shapes.clone());
        if self.undo.len() > HISTORY_LIMIT {
            self.undo.remove(0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: f64) -> Shape {
        Shape::Rect {
            x,
            y: 0.0,
            w: 10.0,
            h: 10.0,
        }
    }

    #[test]
    fn a_fresh_document_is_empty() {
        let doc = Doc::default();
        assert!(doc.is_empty());
        assert!(!doc.can_undo());
        assert!(!doc.can_redo());
    }

    #[test]
    fn begin_snapshots_for_undo() {
        let mut doc = Doc::default();
        doc.begin().push(rect(0.0));
        assert_eq!(doc.len(), 1);
        assert!(doc.can_undo());
        assert!(doc.undo());
        assert!(doc.is_empty());
        assert!(doc.can_redo());
        assert!(doc.redo());
        assert_eq!(doc.len(), 1);
    }

    #[test]
    fn a_new_commit_clears_the_redo_stack() {
        // The classic undo/redo invariant: after undoing, a fresh edit
        // must orphan the redo branch, or redo would resurrect a state
        // that never followed from the new edit.
        let mut doc = Doc::default();
        doc.begin().push(rect(1.0));
        doc.undo();
        assert!(doc.can_redo());
        doc.begin().push(rect(2.0));
        assert!(!doc.can_redo(), "a new edit must clear redo");
    }

    #[test]
    fn undo_and_redo_report_when_they_do_nothing() {
        let mut doc = Doc::default();
        assert!(!doc.undo());
        assert!(!doc.redo());
    }

    #[test]
    fn hit_top_prefers_the_frontmost_shape() {
        let mut doc = Doc::default();
        doc.begin().push(Shape::Note {
            x: 0.0,
            y: 0.0,
            w: 50.0,
            h: 50.0,
            text: String::new(),
        });
        doc.begin().push(Shape::Note {
            x: 0.0,
            y: 0.0,
            w: 50.0,
            h: 50.0,
            text: String::new(),
        });
        // Both notes cover (25, 25); the later one wins.
        let hit = doc.hit_top((25.0, 25.0), 6.0);
        assert_eq!(hit, Some(1));
    }

    #[test]
    fn a_miss_reports_none() {
        let mut doc = Doc::default();
        doc.begin().push(rect(0.0));
        assert_eq!(doc.hit_top((500.0, 500.0), 6.0), None);
    }

    #[test]
    fn history_is_bounded() {
        let mut doc = Doc::default();
        for i in 0..(HISTORY_LIMIT + 20) {
            doc.begin()
                .push(rect(f64::from(u32::try_from(i).unwrap_or(0))));
        }
        assert_eq!(doc.len(), HISTORY_LIMIT + 20);
        let mut undone = 0;
        while doc.undo() {
            undone += 1;
        }
        assert_eq!(undone, HISTORY_LIMIT, "the oldest steps are dropped");
    }

    #[test]
    fn loading_a_file_is_the_base_state() {
        let doc = Doc::with_shapes(vec![rect(0.0), rect(1.0)]);
        assert_eq!(doc.len(), 2);
        assert!(!doc.can_undo(), "loading must not be undoable");
    }
}
