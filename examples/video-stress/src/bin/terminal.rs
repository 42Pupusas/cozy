//! Plays a packed clip as block characters through a terminal.

use video_stress::{FramePack, PlayerOptions, TerminalPlayer, TerminalSize};

/// Binary entry point kept on a marker owner rather than as a free function.
struct Application;

impl Application {
    /// Print usage without opening the pack when help is requested.
    fn handle_meta_command(arguments: &[String], program: &str) -> bool {
        if arguments
            .iter()
            .any(|argument| argument == "-h" || argument == "--help")
        {
            println!("{}", PlayerOptions::usage(program));
            return true;
        }
        false
    }

    fn run(arguments: Vec<String>) -> Result<(), String> {
        let options = PlayerOptions::parse(arguments)?;
        let pack = FramePack::load(&options.pack, 0, 1).map_err(|error| error.to_string())?;
        let size = Self::grid_size(&options);
        let mut player = TerminalPlayer::new(pack, size, options);
        let mut out = std::io::stdout().lock();
        let stats = player
            .play(&mut out)
            .map_err(|error| format!("failed to write frames: {error}"))?;
        let (columns, rows) = player.grid();
        let (sample_x, sample_y) = player.samples();
        println!("{}", stats.summary());
        println!("grid {columns}x{rows} cells | samples {sample_x}x{sample_y}");
        Ok(())
    }

    /// Read the terminal size, honouring an explicit grid override.
    fn grid_size(options: &PlayerOptions) -> TerminalSize {
        let size = TerminalSize::query();
        match options.grid {
            Some((columns, rows)) => size.with_grid(columns, rows),
            None => size,
        }
    }
}

fn main() {
    let mut args = std::env::args();
    let program = args.next().unwrap_or_else(|| "video-terminal".to_owned());
    let arguments: Vec<String> = args.collect();
    if Application::handle_meta_command(&arguments, &program) {
        return;
    }
    if let Err(error) = Application::run(arguments) {
        eprintln!("video-terminal: {error}");
        std::process::exit(1);
    }
}
