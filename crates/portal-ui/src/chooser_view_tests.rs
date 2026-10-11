#![cfg(test)]

use std::path::PathBuf;

use platform::protocol::PointerButton;
use platform::toplevel::{Key, KeyPress, ModifierState, Painter, PhysicalSize, PointerEventKind, Position};
use portal_core::{ChooserKind, ChooserOutcome, ChooserRequest};
use tiles::theme::Theme;
use tiles::typography::Typography;

use crate::chooser_layout::FooterTarget;
use crate::chooser_model::ChooserModel;
use crate::chooser_view::ChooserView;
use crate::test_support::ScratchDir;

struct Rig {
    dir: ScratchDir,
}

impl Rig {
    const SIZE: PhysicalSize = PhysicalSize {
        width: 900,
        height: 600,
    };

    fn new(label: &str) -> Self {
        let dir = ScratchDir::new(label);
        dir.file("alpha.txt");
        dir.file("beta.txt");
        dir.folder("sub");
        dir.file("sub/inner.txt");
        Self { dir }
    }

    fn view(&self, directory: bool, multiple: bool) -> ChooserView {
        let request = ChooserRequest {
            app_id: String::new(),
            parent_window: String::new(),
            title: String::new(),
            kind: ChooserKind::Open {
                multiple,
                directory,
            },
            accept_label: None,
            modal: true,
            filters: vec![],
            current_filter: None,
            choices: vec![],
            current_folder: None,
        };
        let model = ChooserModel::open_at(&request, self.dir.path()).expect("model");
        let cfg = config::Config::default();
        let theme = Theme::from_config(&cfg);
        let type_scale = Typography::load(&cfg, 1.0);
        let mut view = ChooserView::new(
            model,
            theme,
            type_scale,
            Self::SIZE,
            waker::Waker::new().expect("eventfd"),
        );
        view.advance_motion();
        view
    }

    fn key(key: Key) -> KeyPress {
        KeyPress {
            key,
            modifiers: ModifierState::default(),
            repeat: false,
        }
    }

    fn press(view: &mut ChooserView, at: Position) {
        view.on_pointer(PointerEventKind::Press {
            button: PointerButton::Left,
            at,
            modifiers: ModifierState::default(),
        });
    }

    fn click(view: &mut ChooserView, at: Position) {
        Self::press(view, at);
        view.on_pointer(PointerEventKind::Release {
            button: PointerButton::Left,
            at,
            modifiers: ModifierState::default(),
        });
    }

    fn picked(view: &mut ChooserView) -> Vec<PathBuf> {
        match view.take_outcome() {
            Some(ChooserOutcome::Picked(selection)) => selection.paths,
            other => panic!("expected a pick, got {other:?}"),
        }
    }
}

#[test]
fn a_fresh_view_has_no_outcome() {
    let rig = Rig::new("view_fresh");
    assert!(rig.view(false, false).take_outcome().is_none());
}

#[test]
fn clicking_a_file_selects_it_without_finishing() {
    let rig = Rig::new("view_click_file");
    let mut view = rig.view(false, false);
    let at = view.tile_centre("alpha.txt");
    Rig::click(&mut view, at);
    assert!(view.take_outcome().is_none());
    assert!(view.model().can_accept());
}

#[test]
fn double_clicking_a_file_picks_it() {
    let rig = Rig::new("view_double_file");
    let mut view = rig.view(false, false);
    let at = view.tile_centre("alpha.txt");
    Rig::click(&mut view, at);
    Rig::click(&mut view, at);
    assert_eq!(Rig::picked(&mut view), vec![rig.dir.path().join("alpha.txt")]);
}

#[test]
fn double_clicking_a_folder_enters_it() {
    let rig = Rig::new("view_double_folder");
    let mut view = rig.view(false, false);
    let at = view.tile_centre("sub");
    Rig::click(&mut view, at);
    Rig::click(&mut view, at);
    assert!(view.take_outcome().is_none());
    assert_eq!(view.model().navigator().path(), rig.dir.path().join("sub"));
}

#[test]
fn the_accept_button_returns_the_selection() {
    let rig = Rig::new("view_accept");
    let mut view = rig.view(false, false);
    let tile = view.tile_centre("beta.txt");
    Rig::click(&mut view, tile);
    let accept = view.button_centre(FooterTarget::Accept);
    Rig::click(&mut view, accept);
    assert_eq!(Rig::picked(&mut view), vec![rig.dir.path().join("beta.txt")]);
}

#[test]
fn the_accept_button_does_nothing_with_no_selection() {
    let rig = Rig::new("view_accept_empty");
    let mut view = rig.view(false, false);
    let accept = view.button_centre(FooterTarget::Accept);
    Rig::click(&mut view, accept);
    assert!(view.take_outcome().is_none());
}

#[test]
fn the_cancel_button_cancels() {
    let rig = Rig::new("view_cancel");
    let mut view = rig.view(false, false);
    let cancel = view.button_centre(FooterTarget::Cancel);
    Rig::click(&mut view, cancel);
    assert_eq!(view.take_outcome(), Some(ChooserOutcome::Cancelled));
}

#[test]
fn the_up_button_leaves_the_folder() {
    let rig = Rig::new("view_up");
    let mut view = rig.view(false, false);
    let at = view.tile_centre("sub");
    Rig::click(&mut view, at);
    Rig::click(&mut view, at);
    let up = view.button_centre(FooterTarget::Up);
    Rig::click(&mut view, up);
    assert_eq!(view.model().navigator().path(), rig.dir.path());
}

#[test]
fn escape_cancels() {
    let rig = Rig::new("view_escape");
    let mut view = rig.view(false, false);
    view.on_key(Rig::key(Key::Escape));
    assert_eq!(view.take_outcome(), Some(ChooserOutcome::Cancelled));
}

#[test]
fn arrow_then_enter_picks_the_tile_the_arrow_reached() {
    let rig = Rig::new("view_arrow_enter");
    let mut view = rig.view(false, false);
    view.on_key(Rig::key(Key::Down));
    let first = view.model().navigator().cursor_entry().map(|e| e.path.clone());
    view.on_key(Rig::key(Key::Enter));
    match (first, view.take_outcome()) {
        (Some(path), Some(ChooserOutcome::Picked(selection))) => {
            assert_eq!(selection.paths, vec![path]);
        }
        (Some(path), None) => {
            assert_eq!(view.model().navigator().path(), path);
        }
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn a_click_outside_the_tiles_and_buttons_keeps_the_selection() {
    let rig = Rig::new("view_margin");
    let mut view = rig.view(false, false);
    let at = view.tile_centre("alpha.txt");
    Rig::click(&mut view, at);
    Rig::click(&mut view, Position { x: 1, y: 1 });
    assert!(view.model().can_accept());
    let caption = view.caption_centre();
    Rig::click(&mut view, caption);
    assert!(view.model().can_accept());
    assert!(view.take_outcome().is_none());
}

#[test]
fn a_second_click_on_another_tile_moves_the_selection() {
    let rig = Rig::new("view_reselect");
    let mut view = rig.view(false, false);
    let first = view.tile_centre("alpha.txt");
    let second = view.tile_centre("beta.txt");
    Rig::click(&mut view, first);
    Rig::click(&mut view, second);
    assert_eq!(
        view.model().navigator().selected_paths(),
        vec![rig.dir.path().join("beta.txt")]
    );
}

#[test]
fn directory_mode_ignores_clicks_on_files() {
    let rig = Rig::new("view_dir_files");
    let mut view = rig.view(true, false);
    let at = view.tile_centre("alpha.txt");
    Rig::click(&mut view, at);
    Rig::click(&mut view, at);
    assert!(view.take_outcome().is_none());
    assert_eq!(view.model().navigator().path(), rig.dir.path());
}

#[test]
fn directory_mode_accepts_the_open_folder_from_the_button() {
    let rig = Rig::new("view_dir_accept");
    let mut view = rig.view(true, false);
    let accept = view.button_centre(FooterTarget::Accept);
    Rig::click(&mut view, accept);
    assert_eq!(Rig::picked(&mut view), vec![rig.dir.path().to_path_buf()]);
}

#[test]
fn a_frame_paints_every_pixel_of_an_opaque_window() {
    let rig = Rig::new("view_paint_opaque");
    let mut view = rig.view(false, false);
    let pixels = view.painted();
    assert!(pixels.iter().all(|&px| px >> 24 == 0));
    assert!(pixels.iter().any(|&px| px != pixels[0]));
}

#[test]
fn painting_is_not_changed_by_a_selection_until_it_glides() {
    let rig = Rig::new("view_paint_select");
    let mut view = rig.view(false, false);
    let before = view.painted();
    let at = view.tile_centre("alpha.txt");
    Rig::click(&mut view, at);
    let mut now = 0;
    while view.step_motion(now) {
        now += 16;
        assert!(now < 10_000, "the glides never settled");
    }
    assert_ne!(view.painted(), before);
}

#[test]
fn a_view_with_no_motion_asks_for_no_frames() {
    let rig = Rig::new("view_idle");
    let mut view = rig.view(false, false);
    let mut now = 0;
    while view.step_motion(now) {
        now += 16;
        assert!(now < 10_000, "the glides never settled");
    }
    assert_eq!(view.motion_wait_ms(), None);
}

#[test]
fn the_wheel_over_a_fitting_folder_changes_nothing() {
    let rig = Rig::new("view_wheel");
    let mut view = rig.view(false, false);
    let before = view.painted();
    view.on_pointer(PointerEventKind::Scroll {
        lines: 3,
        at: Position { x: 100, y: 100 },
        modifiers: ModifierState::default(),
    });
    assert_eq!(view.painted(), before);
}
