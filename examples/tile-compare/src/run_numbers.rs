//! [`RunNumbers`] -- the figures one run produced, as a single `result` line
//! a parent [`crate::sweep::Sweep`] can parse back.

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct RunNumbers {
    pub fps: f64,
    pub client_cpu: f64,
    pub work_us: f64,
    pub compositor_cpu: f64,
    pub compositor_gfx: f64,
    pub compositor_gpu_mem_kb: f64,
    pub gpu_busy: f64,
    pub power_w: f64,
}

impl RunNumbers {
    const PREFIX: &'static str = "result";
    pub const FIELDS: usize = 8;

    #[must_use]
    pub fn to_line(self) -> String {
        let values = self.values().map(|v| format!("{v:.3}"));
        format!("{} {}", Self::PREFIX, values.join(" "))
    }

    #[must_use]
    pub fn parse_line(line: &str) -> Option<Self> {
        let rest = line.strip_prefix(Self::PREFIX)?.trim();
        let values: Vec<f64> = rest
            .split_whitespace()
            .map(str::parse)
            .collect::<Result<_, _>>()
            .ok()?;
        let values: [f64; Self::FIELDS] = values.try_into().ok()?;
        Some(Self::from_values(values))
    }

    #[must_use]
    pub const fn values(&self) -> [f64; Self::FIELDS] {
        [
            self.fps,
            self.client_cpu,
            self.work_us,
            self.compositor_cpu,
            self.compositor_gfx,
            self.compositor_gpu_mem_kb,
            self.gpu_busy,
            self.power_w,
        ]
    }

    #[must_use]
    pub const fn from_values(v: [f64; Self::FIELDS]) -> Self {
        Self {
            fps: v[0],
            client_cpu: v[1],
            work_us: v[2],
            compositor_cpu: v[3],
            compositor_gfx: v[4],
            compositor_gpu_mem_kb: v[5],
            gpu_busy: v[6],
            power_w: v[7],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::RunNumbers;

    #[test]
    fn a_result_line_round_trips() {
        let numbers = RunNumbers::from_values([60.0, 2.5, 140.0, 12.0, 480.5, 0.0, 3.0, 6.25]);
        assert_eq!(RunNumbers::parse_line(&numbers.to_line()), Some(numbers));
    }

    #[test]
    fn other_lines_are_not_results() {
        assert_eq!(RunNumbers::parse_line("mode            buffer"), None);
        assert_eq!(RunNumbers::parse_line("result 1 2 3"), None);
    }
}
