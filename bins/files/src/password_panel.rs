//! [`PasswordPanel`] -- where the password prompt sits on screen, and how it
//! is drawn.
//!
//! Split from `window.rs` rather than added to it because the modal is the
//! one surface with geometry of its own: it is not a band of the frame, it
//! floats over every band, and its size follows the text polkit chose rather
//! than the window. That is a whole responsibility, so it gets a file.
//!
//! [`PasswordModal`] owns what the prompt *says*; this owns where it lands
//! and what colour it is. The split matters for testing: the layout below is
//! pure arithmetic over a window rectangle, so every degenerate case -- a
//! window narrower than the modal, one shorter than the modal, a zero-sized
//! surface during a resize -- is checkable without a compositor.
//!
//! # The scrim is not decoration
//!
//! The whole window behind the modal is dimmed. It states that the browser
//! is not accepting input, which is *true* -- [`crate::keymap::Focus`]
//! swallows every key that is not part of the reply -- and a modal without
//! one reads as an ordinary panel whose surroundings should still work.
//! A user who cannot tell input is blocked concludes the window has frozen.
//!
//! # Nothing here can reach the plaintext
//!
//! The panel is handed [`PasswordModal::display_text`], which is bullets for
//! a concealed field. There is deliberately no path from a painter to the
//! secret itself -- see [`crate::secret_input::SecretInput`].

use platform::draw::{Canvas, Color, Rect};
use statusbar::TextRenderer;

use crate::password_modal::PasswordModal;
use tiles::spacing::Spacing;

/// The colours and rhythm the modal draws itself with.
///
/// Grouped rather than passed loose for the same reason as
/// [`crate::drag_badge::BadgeStyle`]: they travel together and mean nothing
/// apart.
#[derive(Clone, Copy)]
pub struct PanelStyle {
    pub spacing: Spacing,
    /// Fill behind the modal card.
    pub surface: Color,
    /// The card's outline.
    pub border: Color,
    /// The title and the prompt.
    pub text: Color,
    /// The notice line, and any subordinate text.
    pub muted: Color,
    /// A warning from the helper, e.g. a failed attempt.
    pub notice: Color,
    /// Fill behind the reply field.
    pub field: Color,
    /// The reply field's outline while it has the keyboard.
    pub focus_ring: Color,
    /// The caret after the last bullet.
    pub caret: Color,
    /// What the scrim is made of -- the window's own background, applied at
    /// [`PasswordPanel::SCRIM_ALPHA`].
    pub scrim: Color,
}

/// The three type tiers the modal draws with.
///
/// Grouped for the same reason [`PanelStyle`] is: they are one vocabulary,
/// and passing them loose put eight parameters on [`PasswordPanel::paint`] --
/// at which point the call site is a row of identically-typed arguments that
/// can be swapped without the compiler noticing, and the title would silently
/// draw in the caption tier.
pub struct PanelText<'a> {
    /// The modal's title.
    pub title: &'a mut TextRenderer,
    /// The prompt polkit sent, and the reply field.
    pub body: &'a mut TextRenderer,
    /// The notice line under the field.
    pub detail: &'a mut TextRenderer,
}

/// The modal's resolved rectangle and the boxes inside it.
///
/// Built per paint from the window size, like [`crate::frame::Frame`]: a
/// handful of additions, cheap enough that caching it would only create a
/// chance for it to go stale against a resize.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PasswordPanel {
    /// The card itself.
    card: Rect,
    /// The reply field inside it.
    field: Rect,
    /// Where the title's baseline box starts.
    title_y: i32,
    /// Where the prompt text starts.
    prompt_y: i32,
    /// Where the notice line starts, below the field.
    notice_y: i32,
    /// The horizontal inset for text inside the card.
    inset: i32,
}

impl PasswordPanel {
    /// The modal's width, as a multiple of the guide's sidebar width.
    ///
    /// Wide enough for a sentence of prompt text, which is what polkit sends
    /// -- "Authentication is required to mount KINGSTON" rather than
    /// "Password:". A field-sized box would wrap that into an unreadable
    /// stack.
    const WIDTH_UNITS: i32 = 3;

    /// The narrowest card worth drawing, as a multiple of the sidebar width.
    ///
    /// The card shrinks to fit a narrow window, but only so far. Below this
    /// the prompt truncates to a few characters and the field holds one
    /// bullet, which is not a dialog -- it is a box that cannot state what it
    /// is asking. Refusing is better: the mount then fails with a message
    /// rather than asking a question the user cannot read.
    const MIN_WIDTH_UNITS: i32 = 1;

    /// How far the scrim darkens the window behind the modal, out of 255.
    ///
    /// Enough that the browser reads as unavailable, short of hiding what is
    /// behind it -- the user is mounting a device they can see in the
    /// sidebar, and blacking out the row they just clicked removes the
    /// context for the question being asked.
    const SCRIM_ALPHA: u8 = 168;

    /// Lay the modal out inside a window of `bounds`.
    ///
    /// Centred horizontally, and placed above the vertical centre: a box at
    /// the exact middle sits lower than it looks, and the field the user is
    /// typing into should not be the thing nearest the bottom edge.
    ///
    /// Returns `None` when the window cannot hold the card. Drawing a modal
    /// larger than its window would put the reply field partly off screen,
    /// where the user could type into something they cannot see.
    #[must_use]
    pub fn lay_out(bounds: Rect, style: PanelStyle, text: &PanelText<'_>) -> Option<Self> {
        let spacing = style.spacing;
        let inset = spacing.l();
        let width = (spacing.sidebar_width() * Self::WIDTH_UNITS).min(bounds.w - inset * 2);
        if width < spacing.sidebar_width() * Self::MIN_WIDTH_UNITS {
            return None;
        }
        let field_h = text.body.line_height() + spacing.m();
        let height = spacing.l()
            + text.title.line_height()
            + spacing.m()
            + text.body.line_height()
            + spacing.m()
            + field_h
            + spacing.m()
            + text.detail.line_height()
            + spacing.l();
        if width <= 0 || height > bounds.h || width > bounds.w {
            return None;
        }
        let x = bounds.x + (bounds.w - width) / 2;
        // Three-eighths rather than a half: optical centring.
        let y = bounds.y + ((bounds.h - height) * 3 / 8).max(0);
        let title_y = y + spacing.l();
        let prompt_y = title_y + text.title.line_height() + spacing.m();
        let field_y = prompt_y + text.body.line_height() + spacing.m();
        Some(Self {
            card: Rect::new(x, y, width, height),
            field: Rect::new(x + inset, field_y, (width - inset * 2).max(0), field_h),
            title_y,
            prompt_y,
            notice_y: field_y + field_h + spacing.m(),
            inset,
        })
    }

    /// The card's rectangle.
    ///
    /// Read by the layout tests, which are the only thing that needs it: the
    /// window swallows every pointer event while the prompt is up rather
    /// than hit-testing the card, so there is no click to resolve against it.
    #[must_use]
    #[cfg_attr(not(test), allow(dead_code))]
    pub const fn card(&self) -> Rect {
        self.card
    }

    /// Draw the scrim, the card, and everything the prompt says.
    ///
    /// Takes the modal by reference and reads only its display text, so the
    /// concealed reply cannot reach the canvas.
    pub fn paint(
        &self,
        canvas: &mut Canvas<'_>,
        modal: &PasswordModal,
        style: PanelStyle,
        text: &mut PanelText<'_>,
        bounds: Rect,
    ) {
        let spacing = style.spacing;
        let radius = spacing.radius();
        // Blended, not filled: the row the user just clicked stays visible
        // behind the question being asked about it.
        canvas.blend_rect(bounds, style.scrim, Self::SCRIM_ALPHA);
        canvas.fill_round_rect(self.card, style.surface, radius);
        canvas.draw_round_rect(self.card, style.border, radius);

        let text_x = self.card.x + self.inset;
        text.title.draw_text(
            canvas,
            PasswordModal::TITLE,
            text_x,
            self.title_y,
            style.text,
            style.surface,
        );
        // polkit's own wording, verbatim: it is localised, and it may name
        // the specific administrator whose password is wanted.
        if let Some(prompt) = modal.prompt() {
            let room = self.card.w - self.inset * 2;
            let shown =
                crate::row_format::RowFormat::truncate(prompt, Self::columns_for(text.body, room));
            text.body.draw_text(
                canvas,
                &shown,
                text_x,
                self.prompt_y,
                style.text,
                style.surface,
            );
        }

        canvas.fill_round_rect(self.field, style.field, radius);
        // The field always carries the focus ring: while this is on screen it
        // is the only thing that has the keyboard, so a ring that came and
        // went would suggest the reply might be going somewhere else.
        canvas.draw_round_rect(self.field, style.focus_ring, radius);
        let shown = modal.display_text();
        let inner_x = self.field.x + spacing.m();
        let text_y = self.field.y + (self.field.h - text.body.line_height()) / 2;
        text.body
            .draw_text(canvas, &shown, inner_x, text_y, style.text, style.field);
        let caret_x = inner_x + text.body.measure(&shown);
        canvas.fill_rect(
            Rect::new(caret_x, text_y, spacing.rule(), text.body.line_height()),
            style.caret,
        );

        // "2 attempts remaining" is the only warning before an account locks.
        // Drawn in the notice colour rather than the muted one: it is a
        // consequence, not a caption.
        if let Some(notice) = modal.notice() {
            let room = self.card.w - self.inset * 2;
            let shown = crate::row_format::RowFormat::truncate(
                notice,
                Self::columns_for(text.detail, room),
            );
            text.detail.draw_text(
                canvas,
                &shown,
                text_x,
                self.notice_y,
                style.notice,
                style.surface,
            );
        } else {
            text.detail.draw_text(
                canvas,
                Self::HINT,
                text_x,
                self.notice_y,
                style.muted,
                style.surface,
            );
        }
    }

    /// Shown under the field when the helper has said nothing.
    ///
    /// The prompt swallows every key, so the two that still work are the two
    /// the user must be told about.
    const HINT: &'static str = "Enter to confirm, Esc to cancel";

    /// How many characters of `tier` fit in `room` pixels.
    ///
    /// Measured against a representative glyph rather than the string itself,
    /// matching how the row and sidebar truncation already estimate width.
    fn columns_for(tier: &mut TextRenderer, room: i32) -> usize {
        let unit = tier.measure("n").max(1);
        usize::try_from((room / unit).max(0)).unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::{PanelStyle, PanelText, PasswordPanel};
    use crate::password_modal::PasswordModal;
    use tiles::spacing::Spacing;
    use tiles::theme::Theme;
    use tiles::typography::Typography;
    use platform::draw::Rect;

    fn style() -> PanelStyle {
        let theme = Theme::default().with_spacing(Spacing::new(13.0, 1.0));
        PanelStyle {
            spacing: theme.spacing,
            surface: theme.sidebar_bg,
            border: theme.control_border,
            text: theme.text,
            muted: theme.muted,
            notice: theme.error,
            field: theme.filter_bg,
            focus_ring: theme.focus_ring,
            caret: theme.caret,
            scrim: theme.background,
        }
    }

    fn scale() -> Typography {
        Typography::load(&config::Config::default(), 1.0)
    }

    fn panel(width: i32, height: i32) -> Option<PasswordPanel> {
        let mut type_scale = scale();
        PasswordPanel::lay_out(
            Rect::new(0, 0, width, height),
            style(),
            &PanelText {
                title: &mut type_scale.title,
                body: &mut type_scale.body,
                detail: &mut type_scale.detail,
            },
        )
    }

    #[test]
    fn the_card_is_centred_horizontally_in_the_window() {
        let panel = panel(1280, 790).expect("a modal fits");
        let card = panel.card();
        let left = card.x;
        let right = 1280 - (card.x + card.w);
        assert!((left - right).abs() <= 1, "left {left} vs right {right}");
    }

    /// Optically centred, which means above the true middle: a box at the
    /// exact centre reads as sitting low.
    #[test]
    fn the_card_sits_above_the_vertical_centre() {
        let panel = panel(1280, 790).expect("a modal fits");
        let card = panel.card();
        let centre = card.y + card.h / 2;
        assert!(centre < 790 / 2, "the card must not hang low");
        assert!(card.y > 0, "nor be pinned to the top edge");
    }

    /// The reply field must be inside the card, or the user types into
    /// something drawn outside the box they are looking at.
    #[test]
    fn the_field_sits_inside_the_card() {
        let panel = panel(1280, 790).expect("a modal fits");
        let card = panel.card();
        let field = panel.field;
        assert!(field.x >= card.x && field.x + field.w <= card.x + card.w);
        assert!(field.y >= card.y && field.y + field.h <= card.y + card.h);
        assert!(field.h > 0 && field.w > 0);
    }

    /// Every element must be ordered down the card and stay within it, or
    /// the notice would overprint the field.
    #[test]
    fn the_contents_are_ordered_top_to_bottom_within_the_card() {
        let panel = panel(1280, 790).expect("a modal fits");
        let card = panel.card();
        assert!(panel.title_y >= card.y);
        assert!(panel.prompt_y > panel.title_y);
        assert!(panel.field.y > panel.prompt_y);
        assert!(panel.notice_y >= panel.field.y + panel.field.h);
        assert!(panel.notice_y < card.y + card.h, "the notice must fit");
    }

    /// A window too short for the card must produce no modal rather than one
    /// hanging off the screen -- the field could otherwise be unreachable.
    #[test]
    fn a_window_too_short_lays_out_nothing() {
        assert!(panel(1280, 40).is_none());
    }

    /// A window too narrow is refused rather than squeezed: a card a few
    /// characters wide cannot state what it is asking, and a password prompt
    /// whose question is unreadable is worse than no prompt at all.
    #[test]
    fn a_window_too_narrow_lays_out_nothing_rather_than_a_sliver() {
        assert!(panel(60, 790).is_none(), "a sliver is not a dialog");
        assert!(panel(120, 790).is_none());
    }

    /// It must still fit a genuinely small window, or the modal would be
    /// unreachable on a tiled quarter-screen.
    #[test]
    fn a_modest_window_still_gets_a_prompt() {
        let panel = panel(640, 480).expect("a small window must still prompt");
        assert!(panel.card().w > 0);
        assert!(panel.card().x >= 0);
    }

    /// A zero-sized surface arrives during resizes and must not panic or
    /// divide by anything.
    #[test]
    fn a_degenerate_window_lays_out_nothing() {
        assert!(panel(0, 0).is_none());
        assert!(panel(-10, -10).is_none());
    }

    /// The card must never be wider than the window it floats over.
    #[test]
    fn the_card_never_exceeds_its_window() {
        for width in [200, 400, 800, 1280, 3840] {
            let Some(panel) = panel(width, 900) else {
                continue;
            };
            let card = panel.card();
            assert!(card.x >= 0, "at {width} the card starts off screen");
            assert!(
                card.x + card.w <= width,
                "at {width} the card runs off the right edge"
            );
        }
    }

    /// The panel is given the modal's *display* text, so a concealed reply
    /// reaches the canvas as bullets. This pins the seam rather than the
    /// masking itself, which `password_modal` already covers.
    #[test]
    fn the_panel_is_only_ever_given_the_masked_reply() {
        let mut modal = PasswordModal::new();
        modal.open("Password: ", false);
        for c in "hunter2".chars() {
            modal.insert(c);
        }
        let shown = modal.display_text();
        assert!(!shown.contains("hunter2"));
        assert_eq!(shown.chars().count(), 7);
    }

    /// The scrim must actually be translucent, or the modal would black out
    /// the sidebar row the user just clicked and remove the question's
    /// context.
    #[test]
    fn the_scrim_dims_without_hiding_what_is_behind_it() {
        const {
            assert!(PasswordPanel::SCRIM_ALPHA > 0, "it must dim something");
            assert!(
                PasswordPanel::SCRIM_ALPHA < u8::MAX,
                "it must not be opaque"
            );
        }
    }

    /// The two keys that still work behind the prompt are the two the user
    /// has to be told about.
    #[test]
    fn the_hint_names_the_keys_that_still_work() {
        assert!(PasswordPanel::HINT.contains("Enter"));
        assert!(PasswordPanel::HINT.contains("Esc"));
    }
}
