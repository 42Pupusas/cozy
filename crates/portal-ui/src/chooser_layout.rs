use platform::draw::{Point, Rect};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FooterTarget {
    Up,
    Cancel,
    Accept,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FooterWidths {
    pub up: i32,
    pub cancel: i32,
    pub accept: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FooterMetrics {
    pub gutter: i32,
    pub inset: i32,
    pub gap: i32,
    pub height: i32,
    pub button: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChooserLayout {
    field: Rect,
    footer: Rect,
    caption: Rect,
    up: Rect,
    cancel: Rect,
    accept: Rect,
}

impl ChooserLayout {
    #[must_use]
    pub fn new(width: i32, height: i32, metrics: FooterMetrics, widths: FooterWidths) -> Self {
        let width = width.max(0);
        let height = height.max(0);
        let footer_height = metrics.height.min(height);
        let footer = Rect::new(0, height - footer_height, width, footer_height);
        let field = Rect::new(
            metrics.gutter,
            metrics.gutter,
            (width - metrics.gutter * 2).max(0),
            (footer.y - metrics.gutter * 2).max(0),
        );
        let row_y = footer.y + (footer.h - metrics.button) / 2;
        let button = |x: i32, w: i32| Rect::new(x, row_y, w, metrics.button);
        let accept = button(width - metrics.inset - widths.accept, widths.accept);
        let cancel = button(accept.x - metrics.gap - widths.cancel, widths.cancel);
        let up = button(metrics.inset, widths.up);
        let caption_x = up.right() + metrics.gap;
        let caption = Rect::new(
            caption_x,
            row_y,
            (cancel.x - metrics.gap - caption_x).max(0),
            metrics.button,
        );
        Self {
            field,
            footer,
            caption,
            up,
            cancel,
            accept,
        }
    }

    #[must_use]
    pub const fn field(&self) -> Rect {
        self.field
    }

    #[must_use]
    pub const fn footer(&self) -> Rect {
        self.footer
    }

    #[must_use]
    pub const fn caption(&self) -> Rect {
        self.caption
    }

    #[must_use]
    pub const fn button(&self, target: FooterTarget) -> Rect {
        match target {
            FooterTarget::Up => self.up,
            FooterTarget::Cancel => self.cancel,
            FooterTarget::Accept => self.accept,
        }
    }

    #[must_use]
    pub fn hit(&self, at: Point) -> Option<FooterTarget> {
        [FooterTarget::Accept, FooterTarget::Cancel, FooterTarget::Up]
            .into_iter()
            .find(|&target| self.button(target).contains(at))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const METRICS: FooterMetrics = FooterMetrics {
        gutter: 4,
        inset: 12,
        gap: 8,
        height: 48,
        button: 32,
    };
    const WIDTHS: FooterWidths = FooterWidths {
        up: 60,
        cancel: 80,
        accept: 90,
    };

    fn layout() -> ChooserLayout {
        ChooserLayout::new(900, 600, METRICS, WIDTHS)
    }

    fn centre(rect: Rect) -> Point {
        Point::new(rect.x + rect.w / 2, rect.y + rect.h / 2)
    }

    #[test]
    fn the_footer_sits_along_the_bottom_edge() {
        let footer = layout().footer();
        assert_eq!(footer.bottom(), 600);
        assert_eq!(footer.h, 48);
        assert_eq!(footer.w, 900);
    }

    #[test]
    fn the_field_fills_what_the_footer_leaves_inside_the_margin() {
        let field = layout().field();
        assert_eq!((field.x, field.y), (4, 4));
        assert_eq!(field.w, 892);
        assert_eq!(field.bottom(), 600 - 48 - 4);
    }

    #[test]
    fn accept_is_flush_right_and_cancel_sits_beside_it() {
        let layout = layout();
        let accept = layout.button(FooterTarget::Accept);
        let cancel = layout.button(FooterTarget::Cancel);
        assert_eq!(accept.right(), 900 - METRICS.inset);
        assert_eq!(cancel.right() + METRICS.gap, accept.x);
    }

    #[test]
    fn up_is_flush_left() {
        assert_eq!(layout().button(FooterTarget::Up).x, METRICS.inset);
    }

    #[test]
    fn the_caption_fills_the_gap_between_up_and_cancel() {
        let layout = layout();
        let caption = layout.caption();
        assert_eq!(
            caption.x,
            layout.button(FooterTarget::Up).right() + METRICS.gap
        );
        assert_eq!(
            caption.right() + METRICS.gap,
            layout.button(FooterTarget::Cancel).x
        );
    }

    #[test]
    fn buttons_are_centred_in_the_footer() {
        let layout = layout();
        let footer = layout.footer();
        let button = layout.button(FooterTarget::Accept);
        assert_eq!(button.y - footer.y, footer.bottom() - button.bottom());
    }

    #[test]
    fn a_point_on_each_button_hits_that_button() {
        let layout = layout();
        for target in [FooterTarget::Up, FooterTarget::Cancel, FooterTarget::Accept] {
            assert_eq!(layout.hit(centre(layout.button(target))), Some(target));
        }
    }

    #[test]
    fn the_field_and_the_caption_hit_no_button() {
        let layout = layout();
        assert_eq!(layout.hit(centre(layout.field())), None);
        assert_eq!(layout.hit(centre(layout.caption())), None);
    }

    #[test]
    fn the_buttons_never_overlap() {
        let layout = layout();
        let up = layout.button(FooterTarget::Up);
        let cancel = layout.button(FooterTarget::Cancel);
        let accept = layout.button(FooterTarget::Accept);
        assert!(up.right() <= cancel.x);
        assert!(cancel.right() <= accept.x);
    }

    #[test]
    fn a_degenerate_window_yields_no_negative_extents() {
        let layout = ChooserLayout::new(10, 10, METRICS, WIDTHS);
        for rect in [layout.field(), layout.footer(), layout.caption()] {
            assert!(rect.w >= 0 && rect.h >= 0, "{rect:?}");
        }
    }
}
