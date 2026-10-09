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
use platform::toplevel::{
    Key, KeyPress, LogicalSize, Painter, PhysicalSize, PointerEventKind, Toplevel, WindowEvent,
};

/// Paints a simple test pattern: a background, a border, and a moving band
/// so successive frames are visibly different.
struct Checkerboard {
    frames: u32,
    /// Set when Escape is pressed, so the example proves keyboard input
    /// round-trips all the way to a decision the app acts on.
    quit: bool,
    /// Last pointer position, drawn as a cursor so motion is visible.
    cursor: Option<(i32, i32)>,
    /// Scroll accumulator, moving the band -- proof that axis events arrive
    /// with the right sign.
    scroll: i32,
    focused: bool,
}

impl Painter for Checkerboard {
    fn background(&self) -> Color {
        Color::rgb(0x1e, 0x1e, 0x2e)
    }

    fn on_key(&mut self, press: KeyPress) {
        // Repeats are labelled so a held key is visibly different from a
        // storm of physical presses.
        let tag = if press.repeat { " (repeat)" } else { "" };
        println!("key: {:?} mods={:?}{tag}", press.key, press.modifiers);
        if press.key == Key::Escape {
            self.quit = true;
        }
    }

    fn on_pointer(&mut self, event: PointerEventKind) {
        match event {
            PointerEventKind::Motion(p) | PointerEventKind::Enter(p) => {
                self.cursor = Some((p.x, p.y));
            }
            PointerEventKind::Leave => self.cursor = None,
            PointerEventKind::Press { button, at, .. } => {
                println!("button {button:?} pressed at {},{}", at.x, at.y);
            }
            PointerEventKind::Release { .. } => {}
            PointerEventKind::Scroll { lines, .. } => {
                self.scroll += lines;
                println!("scroll {lines:+} (total {})", self.scroll);
            }
        }
    }

    fn on_focus(&mut self, focused: bool) {
        self.focused = focused;
        println!("focus: {focused}");
    }

    fn paint(&mut self, canvas: &mut Canvas<'_>, size: PhysicalSize) {
        self.frames += 1;
        canvas.fill_rect(Rect::new(0, 0, size.width, size.height), self.background());
        // A border proves we know the true window bounds -- if the geometry
        // maths were wrong this would be clipped or float away from the edge.
        // Border colour tracks focus, so keyboard focus is visible.
        let accent = if self.focused {
            Color::rgb(0xa6, 0xe3, 0xa1)
        } else {
            Color::rgb(0x58, 0x5b, 0x70)
        };
        let t = 4;
        canvas.fill_rect(Rect::new(0, 0, size.width, t), accent);
        canvas.fill_rect(Rect::new(0, size.height - t, size.width, t), accent);
        canvas.fill_rect(Rect::new(0, 0, t, size.height), accent);
        canvas.fill_rect(Rect::new(size.width - t, 0, t, size.height), accent);
        // A band whose position depends on the frame count, so a frozen
        // window is visually distinguishable from a live one.
        let band_h = 40;
        let y = (i32::try_from(self.frames * 7).unwrap_or(0) + self.scroll * 20)
            .rem_euclid((size.height - band_h).max(1));
        canvas.fill_rect(
            Rect::new(t, y, size.width - 2 * t, band_h),
            Color::rgb(0x89, 0xb4, 0xfa),
        );
        // A crosshair at the pointer: proves motion events arrive with
        // coordinates in the same space we paint in.
        if let Some((cx, cy)) = self.cursor {
            let hot = Color::rgb(0xf3, 0x8b, 0xa8);
            canvas.fill_rect(Rect::new(cx - 10, cy - 1, 21, 3), hot);
            canvas.fill_rect(Rect::new(cx - 1, cy - 10, 3, 21), hot);
        }
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
    let mut painter = Checkerboard {
        frames: 0,
        quit: false,
        cursor: None,
        scroll: 0,
        focused: false,
    };

    let mut window = Toplevel::new(
        "toplevel spike",
        "cozy.spike",
        LogicalSize {
            width: 800,
            height: 600,
        },
        wake,
        painter.background(),
    )?;

    println!("window opened at {:?}", window.size());

    loop {
        window.tick(&mut painter)?;

        while let Some(ev) = window.next_event() {
            match ev {
                WindowEvent::CloseRequested => println!("close requested"),
                WindowEvent::Resized(s) => println!("resized to {}x{}", s.width, s.height),
            }
        }
        if window.is_closing() || painter.quit {
            break;
        }

        if let Some(max) = args.max_frames
            && painter.frames >= max
        {
            println!("painted {} frames; exiting", painter.frames);
            break;
        }
        window.request_redraw();
        window.wait(-1);
    }

    println!("painted {} frames total", painter.frames);
    window.close()?;
    println!("closed cleanly");
    Ok(())
}
