//! The one panel the bar may have open, and the routing of popup input to
//! it.
//!
//! Any click on the bar closes the open panel; a click on a panel module
//! other than the open one then opens that module's panel, so a repeated
//! click toggles. The compositor dismisses the popup on clicks outside the
//! bar's own surfaces.

use panel_ui::{Panel, PanelFlow, Theme};
use platform::draw::{Canvas, Point};
use statusbar::{Anchored, BarWindow, ModuleAnchor, PopupStyle, TextRenderer};

use crate::panel_kind::PanelKind;

struct OpenPanel {
    kind: PanelKind,
    panel: Box<dyn Panel>,
}

/// Placement settings that are fixed for the bar's lifetime.
pub struct PanelPlacement {
    pub side: config::PanelSide,
    pub gap: i32,
    pub bar_height: i32,
    pub position: config::BarPosition,
}

impl PanelPlacement {
    #[must_use]
    pub fn from_config(cfg: &config::Config) -> Self {
        Self {
            side: cfg.panel_side(),
            gap: i32::try_from(cfg.panel_gap()).unwrap_or(8),
            bar_height: i32::try_from(cfg.bar_height()).unwrap_or(28),
            position: cfg.bar_position(),
        }
    }
}

pub struct PanelHost {
    open: Option<OpenPanel>,
    text: TextRenderer,
    theme: Theme,
    placement: PanelPlacement,
}

impl PanelHost {
    #[must_use]
    pub fn new(cfg: &config::Config) -> Self {
        Self {
            open: None,
            text: TextRenderer::load_with_fallbacks(
                cfg.bar_font_path(),
                &[cfg.bar_emoji_path(), cfg.bar_icon_path()],
                cfg.bar_font_px_size(),
            ),
            theme: Theme::from_config(cfg),
            placement: PanelPlacement::from_config(cfg),
        }
    }

    /// A click on the bar. `x0`/`x1` are the clicked module's span in
    /// logical pixels, `None` when the click hit no panel module.
    pub fn bar_clicked(&mut self, window: &mut BarWindow, target: Option<(PanelKind, i32, i32)>) {
        let was = self.open.as_ref().map(|o| o.kind);
        self.close(window);
        let Some((kind, x0, x1)) = target else {
            return;
        };
        if was == Some(kind) || !window.can_open_popup() {
            return;
        }
        let panel = kind.open(window.waker());
        let anchor = ModuleAnchor {
            x0,
            x1,
            bar_height: self.placement.bar_height,
            position: self.placement.position,
        };
        let (w, h) = panel.size();
        let positioner = Anchored::resolve(
            anchor,
            self.placement.side,
            w,
            window.logical_width(),
            self.placement.gap,
        )
        .positioner((w, h));
        let style = PopupStyle {
            background: self.theme.bg,
            bg_alpha: self.theme.bg_alpha,
        };
        match window.open_popup(&positioner, style) {
            Ok(true) => self.open = Some(OpenPanel { kind, panel }),
            Ok(false) => Self::retire(panel),
            Err(e) => {
                eprintln!("statusbar: cannot open panel: {e}");
                Self::retire(panel);
            }
        }
    }

    pub fn close(&mut self, window: &mut BarWindow) {
        if let Some(open) = self.open.take() {
            window.close_popup();
            Self::retire(open.panel);
        }
    }

    /// Apply worker updates and typed keys, and close the panel when the
    /// compositor dismissed it or a key asked to.
    pub fn pump(&mut self, window: &mut BarWindow) {
        let Some(open) = &mut self.open else {
            return;
        };
        if window.popup_dismissed() {
            self.close(window);
            return;
        }
        let mut redraw = open.panel.drain();
        for key in window.take_keys() {
            redraw = true;
            if open.panel.key(key) == PanelFlow::Close {
                self.close(window);
                return;
            }
        }
        if redraw {
            window.request_popup_redraw();
        }
    }

    pub fn click(&mut self, window: &mut BarWindow, at: Point) {
        if let Some(open) = &mut self.open {
            open.panel.click(at);
            window.request_popup_redraw();
        }
    }

    pub fn scroll(&mut self, window: &mut BarWindow, at: Point, delta: i32) {
        if let Some(open) = &mut self.open {
            open.panel.scroll(at, delta);
            window.request_popup_redraw();
        }
    }

    pub fn paint(&mut self, canvas: &mut Canvas) {
        if let Some(open) = &mut self.open {
            open.panel.paint(canvas, &mut self.text, &self.theme);
        }
    }

    /// Dropping a panel joins its worker, which may first finish a D-Bus
    /// call; that wait happens off the render thread.
    fn retire(panel: Box<dyn Panel>) {
        let spawned = std::thread::Builder::new()
            .name("statusbar-panel-retire".into())
            .spawn(move || drop(panel));
        if let Err(e) = spawned {
            eprintln!("statusbar: cannot retire panel off-thread: {e}");
        }
    }
}
