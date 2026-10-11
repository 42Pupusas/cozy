use platform::draw::{Canvas, Color, Rect};
use tiles::theme::Theme;
use tiles::typography::Typography;

use crate::chooser_layout::{ChooserLayout, FooterMetrics, FooterTarget, FooterWidths};
use crate::tail_fit::TailFit;

pub struct FooterState<'a> {
    pub caption: String,
    pub accept_label: &'a str,
    pub accept_enabled: bool,
    pub up_enabled: bool,
    pub hovered: Option<FooterTarget>,
}

#[derive(Clone, Copy)]
struct ButtonState {
    enabled: bool,
    hovered: bool,
    primary: bool,
}

pub struct ChooserFooter<'a> {
    theme: &'a Theme,
    type_scale: &'a mut Typography,
}

impl<'a> ChooserFooter<'a> {
    const UP_LABEL: &'static str = "Up";
    const CANCEL_LABEL: &'static str = "Cancel";
    const HOVER_STRENGTH: u8 = 48;

    pub const fn new(theme: &'a Theme, type_scale: &'a mut Typography) -> Self {
        Self { theme, type_scale }
    }

    #[must_use]
    pub fn metrics(theme: &Theme) -> FooterMetrics {
        FooterMetrics {
            gutter: theme.spacing.gutter(),
            inset: theme.spacing.m(),
            gap: theme.spacing.s(),
            height: theme.spacing.toolbar_height(),
            button: theme.spacing.control_height(),
        }
    }

    pub fn widths(&mut self, accept_label: &str) -> FooterWidths {
        let pad = self.theme.spacing.m() * 2;
        let body = &mut self.type_scale.body;
        FooterWidths {
            up: body.measure(Self::UP_LABEL) + pad,
            cancel: body.measure(Self::CANCEL_LABEL) + pad,
            accept: body.measure(accept_label) + pad,
        }
    }

    pub fn paint(
        &mut self,
        canvas: &mut Canvas<'_>,
        layout: &ChooserLayout,
        state: &FooterState<'_>,
    ) {
        let theme = self.theme;
        let bar = layout.footer();
        canvas.fill_rect_glass(bar, theme.sidebar_bg);
        canvas.fill_rect(Rect::new(bar.x, bar.y, bar.w, 1), theme.sidebar_border);
        self.paint_caption(canvas, layout.caption(), &state.caption);
        let look = |target: FooterTarget, enabled: bool, primary: bool| ButtonState {
            enabled,
            hovered: state.hovered == Some(target),
            primary,
        };
        self.paint_button(
            canvas,
            layout.button(FooterTarget::Up),
            Self::UP_LABEL,
            look(FooterTarget::Up, state.up_enabled, false),
        );
        self.paint_button(
            canvas,
            layout.button(FooterTarget::Cancel),
            Self::CANCEL_LABEL,
            look(FooterTarget::Cancel, true, false),
        );
        self.paint_button(
            canvas,
            layout.button(FooterTarget::Accept),
            state.accept_label,
            look(FooterTarget::Accept, state.accept_enabled, true),
        );
    }

    fn paint_caption(&mut self, canvas: &mut Canvas<'_>, area: Rect, caption: &str) {
        let theme = self.theme;
        let body = &mut self.type_scale.body;
        let text = TailFit::fit(caption, area.w, |candidate| body.measure(candidate));
        let y = area.y + body.baseline_y(area.h);
        body.draw_text(canvas, &text, area.x, y, theme.muted, theme.sidebar_bg);
    }

    fn colors(&self, state: ButtonState) -> (Color, Color) {
        let theme = self.theme;
        match (state.enabled, state.primary) {
            (false, _) => (theme.sidebar_bg, theme.disabled),
            (true, true) if state.hovered => (
                theme
                    .accent_surface
                    .blend_opaque_color(theme.accent, Self::HOVER_STRENGTH),
                theme.accent,
            ),
            (true, true) => (theme.accent_surface, theme.accent),
            (true, false) if state.hovered => (theme.hover, theme.text),
            (true, false) => (theme.sidebar_bg, theme.text),
        }
    }

    fn paint_button(
        &mut self,
        canvas: &mut Canvas<'_>,
        rect: Rect,
        label: &str,
        state: ButtonState,
    ) {
        let theme = self.theme;
        let (fill, ink) = self.colors(state);
        let radius = theme.spacing.radius_small();
        canvas.fill_round_rect(rect, fill, radius);
        canvas.draw_round_rect(rect, theme.control_border, radius);
        let body = &mut self.type_scale.body;
        let x = rect.x + (rect.w - body.measure(label)) / 2;
        let y = rect.y + (rect.h - body.line_height()) / 2;
        body.draw_text(canvas, label, x, y, ink, fill);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixture {
        theme: Theme,
        type_scale: Typography,
    }

    impl Fixture {
        fn new() -> Self {
            let cfg = config::Config::default();
            Self {
                theme: Theme::from_config(&cfg),
                type_scale: Typography::load(&cfg, 1.0),
            }
        }

        fn layout(&mut self, label: &str) -> ChooserLayout {
            let widths = ChooserFooter::new(&self.theme, &mut self.type_scale).widths(label);
            ChooserLayout::new(900, 600, ChooserFooter::metrics(&self.theme), widths)
        }

        fn frame(&mut self, state: &FooterState<'_>) -> Vec<u32> {
            let layout = self.layout(state.accept_label);
            let mut pixels = vec![0u32; 900 * 600];
            let mut canvas = Canvas::new(&mut pixels, 900, 600);
            canvas.clear(self.theme.background);
            ChooserFooter::new(&self.theme, &mut self.type_scale).paint(
                &mut canvas,
                &layout,
                state,
            );
            pixels
        }

        fn state(accept_enabled: bool, hovered: Option<FooterTarget>) -> FooterState<'static> {
            FooterState {
                caption: String::from("/home/me"),
                accept_label: "Open",
                accept_enabled,
                up_enabled: true,
                hovered,
            }
        }
    }

    #[test]
    fn a_longer_label_makes_a_wider_button() {
        let mut fixture = Fixture::new();
        let short = ChooserFooter::new(&fixture.theme, &mut fixture.type_scale).widths("Ok");
        let long = ChooserFooter::new(&fixture.theme, &mut fixture.type_scale).widths("Select");
        assert!(long.accept > short.accept);
        assert_eq!(long.cancel, short.cancel);
    }

    #[test]
    fn the_footer_fits_its_buttons() {
        let theme = Theme::from_config(&config::Config::default());
        let metrics = ChooserFooter::metrics(&theme);
        assert!(metrics.height >= metrics.button);
    }

    #[test]
    fn the_accept_button_looks_different_when_it_cannot_be_used() {
        let mut fixture = Fixture::new();
        let live = fixture.frame(&Fixture::state(true, None));
        let dead = fixture.frame(&Fixture::state(false, None));
        assert_ne!(live, dead);
    }

    #[test]
    fn hovering_a_button_changes_the_frame() {
        let mut fixture = Fixture::new();
        let rest = fixture.frame(&Fixture::state(true, None));
        let over = fixture.frame(&Fixture::state(true, Some(FooterTarget::Cancel)));
        assert_ne!(rest, over);
    }

    #[test]
    fn nothing_is_painted_above_the_footer() {
        let mut fixture = Fixture::new();
        let layout = fixture.layout("Open");
        let frame = fixture.frame(&Fixture::state(true, None));
        let background = platform::SurfaceFormat::opaque().pack_glass(fixture.theme.background);
        let above = usize::try_from(layout.footer().y).expect("on screen") * 900;
        assert!(frame[..above].iter().all(|&px| px == background));
    }
}
