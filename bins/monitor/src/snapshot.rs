//! [`Snapshot`] -- one worker publication: a sample, and the process list
//! when a sweep finished on this tick.

use sysmon::{MonitorSample, ProcessInfo};

#[derive(Clone, Default)]
pub struct Snapshot {
    pub sample: MonitorSample,
    pub sweep: Option<Vec<ProcessInfo>>,
}
