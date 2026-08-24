//! Report which subcell modes a font face can draw without tofu.

use font::FontFace;
use video_stress::FontReport;

/// The command-line entry point.
struct FontCheck {
    paths: Vec<String>,
}

impl FontCheck {
    /// Collect the faces to test from the arguments.
    fn parse<I>(arguments: I) -> Self
    where
        I: IntoIterator<Item = String>,
    {
        Self {
            paths: arguments.into_iter().collect(),
        }
    }

    /// Report every face, and exit non-zero if any face fails to load.
    fn run(&self) -> i32 {
        if self.paths.is_empty() {
            eprintln!("usage: video-fontcheck <font.ttf> [more.ttf ...]");
            return 2;
        }
        let mut status = 0;
        for path in &self.paths {
            match FontFace::load(path, 16.0) {
                Ok(face) => print!("{}", FontReport::measure(path.clone(), &face).render()),
                Err(error) => {
                    eprintln!("font {path}\n  -> cannot load: {error:?}");
                    status = 1;
                }
            }
        }
        status
    }
}

fn main() {
    std::process::exit(FontCheck::parse(std::env::args().skip(1)).run());
}
