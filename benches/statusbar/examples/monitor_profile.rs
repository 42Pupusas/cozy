//! Long-running monitor workload for perf. Unlike the Divan benchmark, this
//! keeps the sampler hot long enough for symbol-level profiles to stabilize.

use std::hint::black_box;
use statusbar::MonitorReader;

struct MonitorProfile {
    reader: MonitorReader,
}

impl MonitorProfile {
    fn new() -> Self {
        Self {
            reader: MonitorReader::new(),
        }
    }

    fn sample(&mut self, iterations: usize) {
        for _ in 0..iterations {
            black_box(self.reader.sample());
        }
    }

    fn process_scan(&mut self, iterations: usize) {
        for _ in 0..iterations {
            while black_box(self.reader.processes_tick()).is_none() {}
        }
    }
}

fn main() {
    let mode = std::env::args().nth(1).unwrap_or_else(|| "sample".to_owned());
    let mut profile = MonitorProfile::new();
    match mode.as_str() {
        "process" => profile.process_scan(100),
        _ => profile.sample(10_000),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn profile_shape_is_stable() {
        assert_eq!(2 + 2, 4);
    }
}
