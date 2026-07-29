//! Terminal parser/grid workflow stress without PTY or Wayland.
use grid::Grid;
use headless_stress::StressOptions;
use platform::selection::SelectionTracker;
use vt_parser::Parser;

/// Drives the production VT parser and terminal grid with realistic shell and
/// full-screen application traffic, while discarding rendered pixels.
pub struct TerminalStress;

impl TerminalStress {
    /// Run headless mode when requested.
    pub fn maybe_run(arguments: &[String]) -> Result<bool, String> {
        let Some(options) = StressOptions::parse(arguments.iter().cloned())? else {
            return Ok(false);
        };
        let mut parser = Parser::new();
        let mut grid = Grid::new(120, 40);
        // Production snapshots cross into the UI thread, where selection and
        // link handling retain a cell mirror independent of the renderer.
        // Keep this consumer in the stress path so snapshot experiments are
        // measured end-to-end instead of only on the parser side.
        let mut selection = SelectionTracker::new();
        let completed = options.run(|round| {
            let prompt = format!("\x1b[32muser@host\x1b[0m:~$ printf 'round-{round}\\n'\r\n");
            parser.feed(prompt.as_bytes(), |event| grid.apply(&event));
            let full_screen = b"\x1b[?1049h\x1b[2J\x1b[H\x1b[1;34mhtop\x1b[0m\r\nCPU  12%  MEM  34%\r\n\x1b[?1049l";
            parser.feed(full_screen, |event| grid.apply(&event));
            let snapshot = grid.take_dirty();
            selection.ingest_snapshot(&snapshot);
            let _ = std::hint::black_box(snapshot);
            let _ = std::hint::black_box(grid.take_title());
            let _ = std::hint::black_box(grid.take_bell());
        });
        println!("terminal headless stress: {completed} rounds");
        Ok(true)
    }
}
