//! Standalone smoke runner for validating the shared stress contract.
use headless_stress::StressOptions;

fn main() {
    let options = StressOptions::parse(std::env::args().skip(1))
        .expect("invalid headless stress arguments")
        .expect("stress-headless requires --headless-stress");
    let completed = options.run(|round| {
        let _ = std::hint::black_box(round.wrapping_mul(31));
    });
    println!("completed {completed} headless stress rounds");
}
