//! Deterministic, non-rendering workflow stress harness shared by desktop bins.

use std::time::{Duration, Instant};

/// Command-line controls shared by all headless stress entry points.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StressOptions {
    /// Number of complete workflow rounds.
    pub iterations: u64,
    /// Optional wall-clock limit in addition to the iteration budget.
    pub duration: Option<Duration>,
}

impl Default for StressOptions {
    fn default() -> Self {
        Self {
            iterations: 10_000,
            duration: None,
        }
    }
}

impl StressOptions {
    /// Parse headless controls, ignoring ordinary application arguments.
    pub fn parse<I>(arguments: I) -> Result<Option<Self>, String>
    where
        I: IntoIterator<Item = String>,
    {
        let args: Vec<String> = arguments.into_iter().collect();
        if !args.iter().any(|arg| arg == "--headless-stress") {
            return Ok(None);
        }
        let mut options = Self::default();
        let mut index = 0;
        while index < args.len() {
            match args[index].as_str() {
                "--stress-iterations" => {
                    index += 1;
                    options.iterations = args
                        .get(index)
                        .ok_or("--stress-iterations needs a value")?
                        .parse()
                        .map_err(|_| "--stress-iterations must be an integer")?;
                }
                "--stress-duration" => {
                    index += 1;
                    let milliseconds: u64 = args
                        .get(index)
                        .ok_or("--stress-duration needs a value")?
                        .parse()
                        .map_err(|_| "--stress-duration must be milliseconds")?;
                    options.duration = Some(Duration::from_millis(milliseconds));
                }
                _ => {}
            }
            index += 1;
        }
        Ok(Some(options))
    }

    /// Run a workflow until the iteration or duration budget is exhausted.
    pub fn run<F>(self, mut workflow: F) -> u64
    where
        F: FnMut(u64),
    {
        let started = Instant::now();
        let mut completed = 0;
        while completed < self.iterations
            && self.duration.is_none_or(|limit| started.elapsed() < limit)
        {
            workflow(completed);
            completed += 1;
        }
        completed
    }
}

#[cfg(test)]
mod tests {
    use super::{Duration, StressOptions};

    #[test]
    fn parses_stress_controls_without_wayland() {
        let options = StressOptions::parse([
            "--headless-stress".to_owned(),
            "--stress-iterations".to_owned(),
            "42".to_owned(),
            "--stress-duration".to_owned(),
            "10".to_owned(),
        ])
        .unwrap()
        .unwrap();
        assert_eq!(options.iterations, 42);
        assert_eq!(options.duration, Some(Duration::from_millis(10)));
    }

    #[test]
    fn absent_flag_keeps_normal_path() {
        assert_eq!(StressOptions::parse(["--help".to_owned()]).unwrap(), None);
    }
}
