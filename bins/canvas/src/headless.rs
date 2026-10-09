//! Deterministic canvas workflow without Wayland or a rendered window.

use crate::camera::Camera;
use crate::doc::Doc;
use crate::session::{Session, Tool};
use headless_stress::StressOptions;
use platform::protocol::PointerButton;
use platform::toplevel::Key;
use platform::toplevel::KeyPress;
use platform::toplevel::modifiers::ModifierState;
use platform::toplevel::pointer::{PointerEventKind, Position};

/// Runs the same draw/select/move/zoom/note-edit state transitions a busy
/// user exercises, omitting all platform work.
pub struct CanvasStress;

impl CanvasStress {
    /// Parse and run the stress mode when requested.
    pub fn maybe_run(arguments: &[String]) -> Result<bool, String> {
        let Some(options) = StressOptions::parse(arguments.iter().cloned())? else {
            return Ok(false);
        };
        let mut session = Session::new(Doc::default(), Camera::default(), 8);
        let completed = options.run(|round| {
            let tool = Tool::ALL[usize::try_from(round % Tool::ALL.len() as u64).unwrap_or(0)];
            session.set_tool(tool);
            let phase = round % 4;
            match phase {
                0 => Self::draw_rect(&mut session, round),
                1 => Self::drag_select(&mut session),
                2 => Self::zoom_and_pan(&mut session, round),
                _ => Self::type_into_a_note(&mut session, round),
            }
        });
        println!(
            "canvas headless stress: {completed} rounds, {} shapes",
            session.doc.len()
        );
        Ok(true)
    }

    fn press(session: &mut Session, x: i32, y: i32) {
        session.pointer(PointerEventKind::Press {
            button: PointerButton::Left,
            at: Position { x, y },
            modifiers: ModifierState::default(),
        });
    }

    fn release(session: &mut Session, x: i32, y: i32) {
        session.pointer(PointerEventKind::Release {
            button: PointerButton::Left,
            at: Position { x, y },
            modifiers: ModifierState::default(),
        });
    }

    fn draw_rect(session: &mut Session, round: u64) {
        let base = i32::try_from(round % 200).unwrap_or_default() * 2;
        session.set_tool(Tool::Rect);
        Self::press(session, base + 10, base + 10);
        session.pointer(PointerEventKind::Motion(Position {
            x: base + 110,
            y: base + 60,
        }));
        Self::release(session, base + 110, base + 60);
    }

    fn drag_select(session: &mut Session) {
        session.set_tool(Tool::Select);
        Self::press(session, 12, 30);
        session.pointer(PointerEventKind::Motion(Position { x: 60, y: 30 }));
        Self::release(session, 60, 30);
    }

    fn zoom_and_pan(session: &mut Session, round: u64) {
        let ctrl = ModifierState::from_wire(1 << 2, 0, 0, 0);
        session.pointer(PointerEventKind::Scroll {
            lines: if round.is_multiple_of(2) { -1 } else { 1 },
            at: Position { x: 200, y: 200 },
            modifiers: ctrl,
        });
        session.pointer(PointerEventKind::Scroll {
            lines: 2,
            at: Position { x: 0, y: 0 },
            modifiers: ModifierState::default(),
        });
    }

    fn type_into_a_note(session: &mut Session, round: u64) {
        session.set_tool(Tool::Note);
        Self::press(session, 200, 200);
        let c = char::from_u32(u32::try_from(round % 26 + 97).unwrap_or_else(|_| u32::from(b'a')))
            .unwrap_or('a');
        session.key(KeyPress {
            key: Key::Char(c),
            modifiers: ModifierState::default(),
            repeat: false,
        });
        session.key(KeyPress {
            key: Key::Escape,
            modifiers: ModifierState::default(),
            repeat: false,
        });
    }
}
