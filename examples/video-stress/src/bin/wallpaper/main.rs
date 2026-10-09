//! Wayland background player: plays a packed clip as the desktop wallpaper.

mod video_app;
mod video_globals;
mod video_surface;

use video_app::VideoApp;
use video_stress::{FramePack, PlayerOptions, VideoPalette};

/// The command-line entry point.
struct Application;

impl Application {
    /// Print help without touching Wayland.
    fn handle_meta_command(arguments: &[String]) -> bool {
        if arguments.iter().any(|arg| arg == "-h" || arg == "--help") {
            let program = std::env::args()
                .next()
                .unwrap_or_else(|| "video-wallpaper".to_owned());
            println!("{}", PlayerOptions::usage(&program));
            return true;
        }
        false
    }

    /// Load the clip and play it on every output.
    fn run(arguments: Vec<String>) -> Result<(), String> {
        let options = PlayerOptions::parse(arguments)?;
        let palette = VideoPalette::default();
        let pack = FramePack::load(&options.pack, palette.unlit(), palette.lit())
            .map_err(|error| format!("{}: {error}", options.pack.display()))?;
        if pack.is_empty() {
            return Err("the pack holds no frames".to_owned());
        }
        let header = pack.header();
        let wake = waker::Waker::new().map_err(|error| error.to_string())?;
        let mut app =
            VideoApp::connect(&wake, options, pack, palette).map_err(|error| error.to_string())?;
        println!(
            "clip {}x{} | {} frames | playing at {} fps",
            header.width,
            header.height,
            header.frame_count,
            app.rate()
        );
        let (stats, report) = app.run().map_err(|error| error.to_string())?;
        println!("{}", stats.summary());
        print!("{report}");
        Ok(())
    }
}

fn main() {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    if Application::handle_meta_command(&arguments) {
        return;
    }
    if let Err(error) = Application::run(arguments) {
        eprintln!("video-wallpaper: {error}");
        std::process::exit(1);
    }
}
