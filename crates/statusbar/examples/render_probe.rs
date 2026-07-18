//! Throwaway: render the bar's CPU icon glyph through the real
//! `TextRenderer::load_with_fallbacks` pipeline and dump a PPM, to see
//! definitively whether it paints ink or leaves a blank cell.
use platform::draw::{Canvas, Color};
use statusbar::TextRenderer;

fn main() {
    let primary = "/home/cuarentaydos/.local/share/fonts/NotoSansMono/NotoSansMono-SemiBold.ttf";
    let icon = "/home/cuarentaydos/.local/share/fonts/JetBrainsMono/JetBrainsMonoNerdFontMono-Regular.ttf";

    let mut text = TextRenderer::load_with_fallbacks(Some(primary), &[None, Some(icon)], 24.0);

    let w = 200;
    let h = 40;
    let mut buf = vec![0xFF181616u32; (w * h) as usize];
    let bg = Color::rgb(0x18, 0x16, 0x16);
    let fg = Color::rgb(0xff, 0xff, 0xff);
    {
        let mut canvas = Canvas::new(&mut buf, w, h);
        let y = text.baseline_y(h);
        text.draw_text(&mut canvas, "\u{f4bc} 95%", 4, y, fg, bg);
    }

    let path = "render_probe.ppm";
    let mut f = std::io::BufWriter::new(std::fs::File::create(path).unwrap());
    use std::io::Write;
    write!(f, "P6\n{w} {h}\n255\n").unwrap();
    for px in &buf {
        f.write_all(&[((px >> 16) & 0xff) as u8, ((px >> 8) & 0xff) as u8, (px & 0xff) as u8])
            .unwrap();
    }
    println!("wrote {path}");
}
