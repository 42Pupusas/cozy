//! [`Sweep`] -- runs each configuration several times as a child process,
//! round-robin so slow drift on the machine spreads over every mode, and
//! prints the median of each figure. A window run below [`Sweep::MIN_FPS`]
//! was throttled by the compositor (off-screen or covered) and is dropped.

use std::process::Command;

use crate::run_numbers::RunNumbers;

pub struct Sweep {
    seconds: u64,
    repeats: usize,
    configs: Vec<SweepConfig>,
}

#[derive(Clone, Debug)]
pub struct SweepConfig {
    pub mode: &'static str,
    pub tiles: usize,
    pub period: u64,
}

impl SweepConfig {
    fn accepts(&self, numbers: &RunNumbers) -> bool {
        self.mode == "idle" || numbers.fps >= Sweep::MIN_FPS
    }

    fn label(&self) -> String {
        if self.mode == "idle" {
            return "idle".to_owned();
        }
        format!("{} t{} p{}", self.mode, self.tiles, self.period)
    }
}

impl Sweep {
    pub const MIN_FPS: f64 = 55.0;

    #[must_use]
    pub const fn new(seconds: u64, repeats: usize, configs: Vec<SweepConfig>) -> Self {
        Self {
            seconds,
            repeats,
            configs,
        }
    }

    pub fn run(&self) {
        let exe = std::env::current_exe().expect("own executable path");
        let mut results: Vec<Vec<RunNumbers>> = vec![Vec::new(); self.configs.len()];
        let mut dropped = vec![0_usize; self.configs.len()];
        for round in 0..self.repeats {
            for (index, config) in self.configs.iter().enumerate() {
                eprintln!("round {}/{}: {}", round + 1, self.repeats, config.label());
                match self.run_one(&exe, config) {
                    Some(numbers) if config.accepts(&numbers) => results[index].push(numbers),
                    Some(numbers) => {
                        eprintln!("  dropped: {:.1} fps, window was throttled", numbers.fps);
                        dropped[index] += 1;
                    }
                    None => dropped[index] += 1,
                }
            }
        }
        Self::print_table(&self.configs, &results, &dropped);
    }

    fn run_one(&self, exe: &std::path::Path, config: &SweepConfig) -> Option<RunNumbers> {
        let output = Command::new(exe)
            .args([
                config.mode,
                &self.seconds.to_string(),
                &config.tiles.to_string(),
                &config.period.to_string(),
            ])
            .output()
            .ok()?;
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .find_map(RunNumbers::parse_line)
    }

    fn print_table(configs: &[SweepConfig], results: &[Vec<RunNumbers>], dropped: &[usize]) {
        println!(
            "{:<22} {:>3} {:>4} {:>6} {:>7} {:>8} {:>8} {:>8} {:>8} {:>6} {:>7}",
            "config",
            "n",
            "drop",
            "fps",
            "client%",
            "work us",
            "niri%",
            "gfx%",
            "gpumemMB",
            "busy%",
            "watts"
        );
        for ((config, runs), dropped) in configs.iter().zip(results).zip(dropped) {
            let median = Self::median(runs);
            println!(
                "{:<22} {:>3} {:>4} {:>6.1} {:>7.2} {:>8.1} {:>8.2} {:>8.2} {:>8.1} {:>6.1} {:>7.2}",
                config.label(),
                runs.len(),
                dropped,
                median.fps,
                median.client_cpu,
                median.work_us,
                median.compositor_cpu,
                median.compositor_gfx,
                median.compositor_gpu_mem_kb / 1024.0,
                median.gpu_busy,
                median.power_w
            );
        }
    }

    /// The per-field median, each figure taken independently.
    #[must_use]
    pub fn median(runs: &[RunNumbers]) -> RunNumbers {
        let mut out = [0.0; RunNumbers::FIELDS];
        for (field, slot) in out.iter_mut().enumerate() {
            let mut column: Vec<f64> = runs.iter().map(|r| r.values()[field]).collect();
            column.sort_by(f64::total_cmp);
            *slot = match column.len() {
                0 => 0.0,
                n if n % 2 == 1 => column[n / 2],
                n => f64::midpoint(column[n / 2 - 1], column[n / 2]),
            };
        }
        RunNumbers::from_values(out)
    }
}

#[cfg(test)]
mod tests {
    use super::Sweep;
    use crate::run_numbers::RunNumbers;

    fn with_fps(fps: f64) -> RunNumbers {
        RunNumbers {
            fps,
            ..RunNumbers::default()
        }
    }

    #[test]
    fn the_median_of_an_odd_count_is_the_middle_value() {
        let runs = [with_fps(3.0), with_fps(100.0), with_fps(1.0)];
        assert!((Sweep::median(&runs).fps - 3.0).abs() < f64::EPSILON);
    }

    #[test]
    fn the_median_of_an_even_count_averages_the_middle_pair() {
        let runs = [with_fps(1.0), with_fps(2.0), with_fps(4.0), with_fps(50.0)];
        assert!((Sweep::median(&runs).fps - 3.0).abs() < f64::EPSILON);
    }

    #[test]
    fn no_runs_give_zeroes() {
        assert_eq!(Sweep::median(&[]), RunNumbers::default());
    }
}
