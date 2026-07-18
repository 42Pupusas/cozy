//! Standalone visual + programmatic check that the real font pipeline
//! (`crates/font` + `platform::Renderer`) actually rasterizes ink for ASCII,
//! box-drawing, and emoji codepoints — the thing a unit test can't fully
//! prove, since "does it look right" ultimately wants a human glance.
//!
//! Renders one row per category through the exact `Renderer::blit_cell` path
//! the real terminal uses (no shortcuts), then:
//!   1. uses `font::FontFace::has_glyph` on both the primary and emoji faces
//!      to know *before* rasterizing whether a codepoint is genuinely covered
//!      (catching a silent cmap/fallback-wiring regression) rather than just
//!      checking "some pixel changed", which a `.notdef` tofu box would also
//!      satisfy;
//!   2. cross-checks that against the backbuffer to confirm the rasterizer
//!      actually painted ink where coverage says it should; and
//!   3. writes a `.ppm`/`.png` image next to the binary so you can eyeball it.
//!
//! Exit code is nonzero if any REQUIRED category (ascii, box-drawing, emoji)
//! has an uncovered or blank glyph. CJK is rendered and reported but not
//! required to pass, since this terminal only wires up an emoji fallback by
//! default — a CJK codepoint tofu-ing is expected unless you pass a CJK-
//! covering font as the primary/emoji path yourself.
//!
//! ```sh
//! cargo run -p platform --example glyph_render_check -- \
//!     /path/to/MonoFont.ttf /path/to/EmojiFont.ttf
//! ```
//!
//! Both font paths are required args (no bundled fallback — see the config
//! docs on `[font] path` / `emoji_path`). Writes `glyph_render_check.ppm` in
//! the current directory.

// Scratch/manual-run diagnostic tool (same rationale as `flood_bench`): buffer
// dimensions are always small and positive in practice, so geometry casts get
// a blanket allow rather than defensive `try_from` everywhere.
#![allow(
    clippy::cast_sign_loss,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::too_many_lines,
    clippy::if_not_else
)]

use platform::draw::Canvas;
use platform::{Renderer, Theme};

/// One row of the check: a label (for stderr output), the literal text to
/// render, and whether every glyph in it MUST be covered for the check to
/// pass (vs. just reported).
struct Row {
    label: &'static str,
    text: &'static str,
    required: bool,
}

const ROWS: &[Row] = &[
    Row {
        label: "ascii",
        text: "Hello, World! 0123456789",
        required: true,
    },
    Row {
        label: "box-drawing",
        text: "┌─┬─┐│ │├─┼─┤└─┴─┘",
        required: true,
    },
    Row {
        label: "cjk",
        text: "国际化测试字符宽度渲染",
        required: false, // no CJK fallback wired up by default -- informational only
    },
    Row {
        label: "emoji",
        text: "\u{1F600}\u{1F642}\u{1F44D}\u{2764}\u{1F389}",
        required: true,
    },
];

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!(
            "usage: {prog} <mono-font.ttf> <emoji-font.ttf>\n\n\
             Both are required -- there is no bundled fallback font.\n\
             e.g.: {prog} /usr/share/fonts/truetype/jetbrains-mono/JetBrainsMono-Regular.ttf \\\n      \
             assets/fonts/NotoEmoji-Regular.ttf",
            prog = args.first().map_or("glyph_render_check", String::as_str),
        );
        std::process::exit(2);
    }
    let font_path = &args[1];
    let emoji_path = &args[2];

    // Separate, throwaway coverage probes (not used for rendering) so we can
    // ask "does ANY configured font actually cover this codepoint" before
    // touching the renderer's atlas/cache -- ground truth against which the
    // painted pixels are then cross-checked.
    let primary_probe = font::FontFace::load(font_path, 16.0)
        .unwrap_or_else(|e| panic!("failed to load primary font {font_path:?}: {e}"));
    let emoji_probe = font::FontFace::load(emoji_path, 16.0)
        .unwrap_or_else(|e| panic!("failed to load emoji font {emoji_path:?}: {e}"));

    let cfg = config::FontConfig {
        path: Some(font_path.clone()),
        size: 16,
        emoji_path: Some(emoji_path.clone()),
    };
    let mut renderer = Renderer::new(Theme::builtin(), &cfg);

    let cell_w = renderer.cell_w();
    let cell_h = renderer.cell_h();
    let cols = ROWS.iter().map(|r| r.text.chars().count()).max().unwrap_or(1) as i32;
    let rows = ROWS.len() as i32;
    let bw = (cols * cell_w).max(1);
    let bh = (rows * cell_h).max(1);
    let mut backbuffer = vec![0u32; (bw * bh) as usize];
    let bg_px = renderer.bg_xrgb();

    let mut any_required_failed = false;

    for (row_idx, row) in ROWS.iter().enumerate() {
        let mut row_ok = true;
        for (col_idx, ch) in row.text.chars().enumerate() {
            let cp = ch as u32;
            let cell = grid::Cell {
                ch,
                attrs: grid::Attrs::default(),
                wide: false,
                spacer: false,
                link_id: 0,
            };
            {
                let mut canvas = Canvas::new(&mut backbuffer, bw, bh);
                renderer.blit_cell(&mut canvas, col_idx as u16, row_idx as u16, &cell);
            }

            if ch == ' ' {
                continue;
            }

            let covered = primary_probe.has_glyph(cp) || emoji_probe.has_glyph(cp);

            let px0 = col_idx as i32 * cell_w;
            let py0 = row_idx as i32 * cell_h;
            let has_ink = (0..cell_h).any(|dy| {
                (0..cell_w).any(|dx| {
                    let x = px0 + dx;
                    let y = py0 + dy;
                    if x >= bw || y >= bh {
                        return false;
                    }
                    let idx = (y * bw + x) as usize;
                    backbuffer.get(idx).is_some_and(|&p| p != bg_px)
                })
            });

            let ok = covered && has_ink;
            if !ok {
                let why = if !covered {
                    "no configured font covers this codepoint (.notdef/tofu)"
                } else {
                    "font claims coverage but rasterizer painted no ink -- pipeline bug"
                };
                let sev = if row.required { "FAIL" } else { "info" };
                eprintln!(
                    "{sev} [{}]: U+{:04X} ({:?}) at col {col_idx}: {why}",
                    row.label, cp, ch
                );
                row_ok = false;
                if row.required {
                    any_required_failed = true;
                }
            }
        }
        let verdict = if row_ok {
            "OK — every glyph covered and rendered ink"
        } else if row.required {
            "FAILED — see above"
        } else {
            "not fully covered (expected -- no CJK font wired up; informational only)"
        };
        eprintln!("[{}] {verdict}", row.label);
    }

    let out_path = "glyph_render_check.ppm";
    if let Err(e) = write_ppm(out_path, &backbuffer, bw, bh) {
        eprintln!("warning: couldn't write {out_path}: {e}");
    } else {
        eprintln!("wrote {out_path} ({bw}x{bh}) -- open it to eyeball the result");
    }

    if any_required_failed {
        eprintln!("\nRESULT: FAIL -- a required category (ascii/box-drawing/emoji) has a gap");
        std::process::exit(1);
    }
    eprintln!("\nRESULT: PASS -- ascii, box-drawing, and emoji all covered and rendered ink");
}

/// Write a packed-XRGB8888 buffer as a binary PPM (P6, 8-bit RGB).
fn write_ppm(path: &str, buf: &[u32], w: i32, h: i32) -> std::io::Result<()> {
    use std::io::Write;
    let mut f = std::io::BufWriter::new(std::fs::File::create(path)?);
    write!(f, "P6\n{w} {h}\n255\n")?;
    let mut row = Vec::with_capacity(w as usize * 3);
    for y in 0..h {
        row.clear();
        for x in 0..w {
            let px = buf[(y * w + x) as usize];
            row.push(((px >> 16) & 0xff) as u8);
            row.push(((px >> 8) & 0xff) as u8);
            row.push((px & 0xff) as u8);
        }
        f.write_all(&row)?;
    }
    Ok(())
}
