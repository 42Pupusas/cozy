//! [`Typography`] -- the four text sizes the dashboard draws with, and the
//! ladder of sizes tile text picks from.

use config::FontConfig;
use statusbar::TextRenderer;

use crate::text_ladder::TextLadder;

pub struct Typography {
    pub hero: TextRenderer,
    pub title: TextRenderer,
    pub subtitle: TextRenderer,
    pub normal: TextRenderer,
    pub ladder: TextLadder,
}

impl Typography {
    /// Sizes in the order hero, title, subtitle, normal, in the embedded font.
    #[must_use]
    pub fn embedded(sizes: [f32; 4]) -> Self {
        Self {
            hero: TextRenderer::load(None, sizes[0]),
            title: TextRenderer::load(None, sizes[1]),
            subtitle: TextRenderer::load(None, sizes[2]),
            normal: TextRenderer::load(None, sizes[3]),
            ladder: TextLadder::embedded(sizes[3], sizes[0]),
        }
    }

    /// The configured font (and its emoji fallback) at each of `sizes`, the
    /// same face the terminal draws with. Every size loads in parallel and is
    /// ready before this returns, so no frame is ever drawn in the embedded
    /// bitmap font while a real face is configured.
    #[must_use]
    pub fn load(font: &FontConfig, sizes: [f32; 4]) -> Self {
        let data = font
            .path
            .as_deref()
            .and_then(|path| std::fs::read(path).ok());
        let mut all = sizes.to_vec();
        all.extend(TextLadder::sizes(sizes[3], sizes[0]));
        let mut renderers = std::thread::scope(|scope| {
            let data = data.as_deref();
            #[allow(
                clippy::needless_collect,
                reason = "every thread must start before the first join"
            )]
            let handles: Vec<_> = all
                .iter()
                .map(|&px| scope.spawn(move || Self::renderer(font, data, px)))
                .collect();
            handles
                .into_iter()
                .map(|handle| handle.join().unwrap_or(TextRenderer::Bitmap))
                .collect::<Vec<_>>()
        });
        let ladder = TextLadder::new(renderers.split_off(4));
        let mut tiers = renderers.into_iter();
        let mut next = || tiers.next().unwrap_or(TextRenderer::Bitmap);
        Self {
            hero: next(),
            title: next(),
            subtitle: next(),
            normal: next(),
            ladder,
        }
    }

    fn renderer(font: &FontConfig, data: Option<&[u8]>, px_size: f32) -> TextRenderer {
        let emoji = [font.emoji_path.as_deref()];
        let mut renderer = match (font.path.as_deref(), data) {
            (Some(path), Some(data)) => {
                TextRenderer::load_with_fallbacks_from_bytes(path, data, &emoji, px_size)
            }
            (path, _) => TextRenderer::load_with_fallbacks(path, &emoji, px_size),
        };
        renderer.prewarm_common_glyphs();
        renderer
    }
}
