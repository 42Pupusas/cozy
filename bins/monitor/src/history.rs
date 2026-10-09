//! [`History`] -- the rolling series the chart tiles draw.

use std::collections::VecDeque;

use sysmon::MonitorSample;

/// One minute of chart history at four points per second.
pub const HISTORY: usize = 60 * 4;

pub struct History {
    pub cpu: VecDeque<f32>,
    pub cores: Vec<VecDeque<f32>>,
    pub memory: VecDeque<f32>,
    pub temp: VecDeque<f32>,
    pub rx: VecDeque<f32>,
    pub tx: VecDeque<f32>,
    pub sequence: u64,
}

impl History {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            cpu: VecDeque::new(),
            cores: Vec::new(),
            memory: VecDeque::new(),
            temp: VecDeque::new(),
            rx: VecDeque::new(),
            tx: VecDeque::new(),
            sequence: 0,
        }
    }

    fn push(queue: &mut VecDeque<f32>, value: f32) {
        queue.push_back(value);
        if queue.len() > HISTORY {
            queue.pop_front();
        }
    }

    pub fn add(&mut self, sample: &MonitorSample) {
        self.sequence = self.sequence.saturating_add(1);
        Self::push(&mut self.cpu, sample.cpu);
        self.cores.resize_with(sample.cores.len(), VecDeque::new);
        for (history, value) in self.cores.iter_mut().zip(&sample.cores) {
            Self::push(history, *value);
        }
        Self::push(&mut self.memory, sample.memory);
        Self::push(&mut self.temp, sample.temperature);
        Self::push(&mut self.rx, sample.network_rx);
        Self::push(&mut self.tx, sample.network_tx);
    }
}

impl Default for History {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::{HISTORY, History};
    use sysmon::MonitorSample;

    #[test]
    fn series_are_capped_and_cores_follow_the_sample() {
        let mut history = History::new();
        let sample = MonitorSample {
            cores: vec![1.0, 2.0, 3.0],
            ..Default::default()
        };
        for _ in 0..HISTORY + 10 {
            history.add(&sample);
        }
        assert_eq!(history.cpu.len(), HISTORY);
        assert_eq!(history.cores.len(), 3);
        assert!(history.cores.iter().all(|core| core.len() == HISTORY));
        assert_eq!(history.sequence, u64::try_from(HISTORY + 10).unwrap_or(0));
    }
}
