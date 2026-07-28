//! End-to-end proof that [`platform::toplevel::Toplevel`] actually maps,
//! paints, resizes, and closes against a real compositor.
//!
//! Unit tests can verify the geometry arithmetic and the lifecycle rules,
//! but they cannot prove the handshake is right -- getting the configure
//! ordering wrong produces a protocol error that only a live compositor will
//! ever tell you about. So this example exists as the integration check.
//!
//! Run headless in CI-ish fashion with a frame budget:
//!   `cargo run -p platform --example toplevel_spike -- --frames 3`
//! or interactively (runs until closed):
//!   `cargo run -p platform --example toplevel_spike`

use platform::draw::{Canvas, Color, Rect};
use platform::toplevel::{LogicalSize, Painter, PhysicalSize, Toplevel, WindowEvent};

/// Paints a simple test pattern: a background, a border, and a moving band
/// so successive frames are visibly different.
struct Checkerboard {
    frames: u32,
}

impl Painter for Checkerboard {
    fn background(&self) -> Color {
        Color::rgb(0x1e, 0x1e, 0x2e)
    }

    fn paint(&mut self, canvas: &mut Canvas<'_>, size: PhysicalSize) {
        self.frames += 1;
        canvas.fill_rect(Rect::new(0, 0, size.width, size.height), self.background());
        // A border proves we know the true window bounds -- if the geometry
        // maths were wrong this would be clipped or float away from the edge.
        let accent = Color::rgb(0xa6, 0xe3, 0xa1);
        let t = 4;
        canvas.fill_rect(Rect::new(0, 0, size.width, t), accent);
        canvas.fill_rect(Rect::new(0, size.height - t, size.width, t), accent);
        canvas.fill_rect(Rect::new(0, 0, t, size.height), accent);
        canvas.fill_rect(Rect::new(size.width - t, 0, t, size.height), accent);
        // A band whose position depends on the frame count, so a frozen
        // window is visually distinguishable from a live one.
        let band_h = 40;
        let y = i32::try_from(self.frames * 7).unwrap_or(0) % (size.height - band_h).max(1);
        canvas.fill_rect(
            Rect::new(t, y, size.width - 2 * t, band_h),
            Color::rgb(0x89, 0xb4, 0xfa),
        );
    }
}

/// Parses `--frames N` from the command line.
struct Args {
    max_frames: Option<u32>,
}

impl Args {
    fn parse() -> Self {
        let mut args = std::env::args().skip(1);
        let mut max_frames = None;
        while let Some(a) = args.next() {
            if a == "--frames" {
                max_frames = args.next().and_then(|v| v.parse().ok());
            }
        }
        Self { max_frames }
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    let wake = waker::Waker::new()?;
    let mut painter = Checkerboard { frames: 0 };

    let mut window = Toplevel::new(
        "toplevel spike",
        "dev.terminal.spike",
        LogicalSize {
            width: 800,
            height: 600,
        },
        wake,
        painter.background(),
    )?;

    println!("window opened at {:?}", window.size());

    let mut ticks = 0u32;
    loop {
        window.tick(&mut painter)?;

        while let Some(ev) = window.next_event() {
            match ev {
                WindowEvent::CloseRequested => println!("close requested"),
                WindowEvent::Resized(s) => println!("resized to {}x{}", s.width, s.height),
            }
        }
        if window.is_closing() {
            break;
        }

        ticks += 1;
        if let Some(max) = args.max_frames
            && painter.frames >= max
        {
            println!("painted {} frames; exiting", painter.frames);
            break;
        }
        // A guard so a compositor that never configures us cannot hang a
        // scripted run forever.
        if args.max_frames.is_some() && ticks > 10_000 {
            return Err("timed out waiting for frames".into());
        }

        // Keep asking for frames so the band animates.
        window.request_redraw();
        window.wait(16);
    }

    println!("painted {} frames total", painter.frames);
    window.close()?;
    println!("closed cleanly");
    Ok(())
}
