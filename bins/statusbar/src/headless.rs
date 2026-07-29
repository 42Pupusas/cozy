//! Statusbar module stress without layer-shell, painting, or worker threads.
use crate::worker::Snapshot;
use headless_stress::StressOptions;
use statusbar::{ModuleKind, ScrollAccumulator};

/// Exercises system module probes, formatting, snapshot assembly, and scroll
/// policy while deliberately omitting the rendered bar and Wayland transport.
pub struct StatusbarStress;

impl StatusbarStress {
    /// Run headless mode when requested.
    pub fn maybe_run(arguments: &[String]) -> Result<bool, String> {
        let Some(options) = StressOptions::parse(arguments.iter().cloned())? else {
            return Ok(false);
        };
        let mut snapshot = Snapshot::default();
        let mut accumulator = ScrollAccumulator::default();
        let completed = options.run(|round| {
            let now = statusbar::Clock::time().unwrap_or_else(|| "—".to_owned());
            let date = statusbar::Clock::date().unwrap_or_else(|| "—".to_owned());
            let battery = statusbar::Battery::read().unwrap_or_else(|| "—".to_owned());
            let memory = statusbar::Memory::read().unwrap_or_else(|| "—".to_owned());
            let temperature = statusbar::Temperature::read().unwrap_or_else(|| "—".to_owned());
            let wifi = statusbar::Wifi::read().unwrap_or_else(|| "—".to_owned());
            let bluetooth = statusbar::Bluetooth::read().unwrap_or_else(|| "—".to_owned());
            snapshot = Snapshot::default();
            let _ =
                std::hint::black_box((now, date, battery, memory, temperature, wifi, bluetooth));
            let _ = std::hint::black_box(snapshot.left_segments());
            let _ = std::hint::black_box(snapshot.right_segments());
            accumulator.add(if round % 2 == 0 { 15 } else { -15 });
            let _ = std::hint::black_box((ModuleKind::Brightness, accumulator.take_steps()));
        });
        println!("statusbar headless stress: {completed} rounds");
        Ok(true)
    }
}
