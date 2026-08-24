//! Converts any video ffmpeg can read into a packed 1-bit clip.

use video_stress::{PackWriter, PrepareOptions};

/// Binary entry point kept on a marker owner rather than as a free function.
struct Application;

impl Application {
    /// Print usage without touching ffmpeg when help is requested.
    fn handle_meta_command(arguments: &[String], program: &str) -> bool {
        if arguments.is_empty()
            || arguments
                .iter()
                .any(|argument| argument == "-h" || argument == "--help")
        {
            println!("{}", PrepareOptions::usage(program));
            return true;
        }
        false
    }

    fn run(arguments: Vec<String>) -> Result<(), String> {
        let options = PrepareOptions::parse(arguments)?;
        let output = options.output.clone();
        let (width, height, fps) = (options.width, options.height, options.fps);
        let frames = PackWriter::new(options).write()?;
        println!(
            "wrote {} | {frames} frames | {width}x{height} @ {fps} fps | {:.1} MiB",
            output.display(),
            Self::pack_size(&output),
        );
        Ok(())
    }

    /// Size of the written pack in mebibytes, or zero when it cannot be read.
    #[allow(clippy::cast_precision_loss)]
    fn pack_size(path: &std::path::Path) -> f64 {
        std::fs::metadata(path).map_or(0.0, |data| data.len() as f64 / (1024.0 * 1024.0))
    }
}

fn main() {
    let mut args = std::env::args();
    let program = args.next().unwrap_or_else(|| "video-prepare".to_owned());
    let arguments: Vec<String> = args.collect();
    if Application::handle_meta_command(&arguments, &program) {
        return;
    }
    if let Err(error) = Application::run(arguments) {
        eprintln!("video-prepare: {error}");
        std::process::exit(1);
    }
}
