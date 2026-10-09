//! Pure volume panel state and control construction.

use crate::pactl::MixerSnapshot;
use crate::pending_edits::PendingEdits;

pub const PANEL_WIDTH: i32 = 430;
pub const PANEL_HEIGHT: i32 = 440;
pub const VISIBLE_CONTROLS: usize = 10;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VolumeTarget {
    Sink(String),
    Source(String),
    SinkInput(u32),
    SourceOutput(u32),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VolumeCommand {
    SetVolume { target: VolumeTarget, percent: u32 },
    ToggleMute { target: VolumeTarget },
    SetDefaultSink(String),
    SetDefaultSource(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ControlKind {
    Device {
        target: VolumeTarget,
        is_default: bool,
    },
    Slider {
        target: VolumeTarget,
        percent: u32,
        muted: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VolumeControl {
    pub section: &'static str,
    pub label: String,
    pub detail: String,
    pub kind: ControlKind,
}

#[derive(Debug, Default)]
pub struct VolumeModel {
    pub snapshot: MixerSnapshot,
    pub selected: usize,
    pub scroll: usize,
    pending: PendingEdits,
}

impl VolumeModel {
    pub fn apply(&mut self, snapshot: MixerSnapshot) {
        self.snapshot = snapshot;
        self.pending.reapply(&mut self.snapshot);
        self.clamp_selection();
    }

    #[must_use]
    pub fn controls(&self) -> Vec<VolumeControl> {
        let mut controls = Vec::new();
        for sink in &self.snapshot.sinks {
            let is_default = sink.name == self.snapshot.default_sink;
            controls.push(VolumeControl {
                section: "Output devices",
                label: Self::device_label(&sink.description, &sink.name),
                detail: if is_default {
                    "default".into()
                } else {
                    String::new()
                },
                kind: ControlKind::Device {
                    target: VolumeTarget::Sink(sink.name.clone()),
                    is_default,
                },
            });
            if is_default {
                controls.push(VolumeControl {
                    section: "Output volume",
                    label: "Speakers".into(),
                    detail: String::new(),
                    kind: ControlKind::Slider {
                        target: VolumeTarget::Sink(sink.name.clone()),
                        percent: sink.volume_percent(),
                        muted: sink.mute,
                    },
                });
            }
        }
        for source in &self.snapshot.sources {
            if source.name.ends_with(".monitor") {
                continue;
            }
            let is_default = source.name == self.snapshot.default_source;
            controls.push(VolumeControl {
                section: "Input devices",
                label: Self::device_label(&source.description, &source.name),
                detail: if is_default {
                    "default".into()
                } else {
                    String::new()
                },
                kind: ControlKind::Device {
                    target: VolumeTarget::Source(source.name.clone()),
                    is_default,
                },
            });
            if is_default {
                controls.push(VolumeControl {
                    section: "Input volume",
                    label: "Microphone".into(),
                    detail: String::new(),
                    kind: ControlKind::Slider {
                        target: VolumeTarget::Source(source.name.clone()),
                        percent: source.volume_percent(),
                        muted: source.mute,
                    },
                });
            }
        }
        for stream in &self.snapshot.sink_inputs {
            controls.push(VolumeControl {
                section: "Playback programs",
                label: Self::stream_label(stream.display_name(), stream.index),
                detail: String::new(),
                kind: ControlKind::Slider {
                    target: VolumeTarget::SinkInput(stream.index),
                    percent: stream.volume_percent(),
                    muted: stream.mute,
                },
            });
        }
        for stream in &self.snapshot.source_outputs {
            controls.push(VolumeControl {
                section: "Capture programs",
                label: Self::stream_label(stream.display_name(), stream.index),
                detail: String::new(),
                kind: ControlKind::Slider {
                    target: VolumeTarget::SourceOutput(stream.index),
                    percent: stream.volume_percent(),
                    muted: stream.mute,
                },
            });
        }
        controls
    }

    fn device_label(description: &str, name: &str) -> String {
        if description.is_empty() {
            name.to_owned()
        } else {
            description.to_owned()
        }
    }

    fn stream_label(name: &str, index: u32) -> String {
        if name.is_empty() {
            format!("Stream {index}")
        } else {
            name.to_owned()
        }
    }

    pub fn move_selection(&mut self, delta: i32) {
        let len = self.controls().len();
        if len == 0 {
            self.selected = 0;
            self.scroll = 0;
            return;
        }
        self.selected = if delta < 0 {
            self.selected.checked_sub(1).unwrap_or(len - 1)
        } else {
            (self.selected + 1) % len
        };
        self.keep_visible();
    }

    #[must_use]
    pub fn selected_command(&mut self) -> Option<VolumeCommand> {
        let command = self.selected_command_for()?;
        if let VolumeCommand::ToggleMute { target } = &command {
            self.toggle_mute(&target.clone());
        }
        Some(command)
    }

    fn selected_command_for(&self) -> Option<VolumeCommand> {
        let control = self.controls().get(self.selected)?.clone();
        match control.kind {
            ControlKind::Device {
                target: VolumeTarget::Sink(name),
                is_default: false,
            } => Some(VolumeCommand::SetDefaultSink(name)),
            ControlKind::Device {
                target: VolumeTarget::Source(name),
                is_default: false,
            } => Some(VolumeCommand::SetDefaultSource(name)),
            ControlKind::Device { target, .. } | ControlKind::Slider { target, .. } => {
                Some(VolumeCommand::ToggleMute { target })
            }
        }
    }

    #[must_use]
    pub fn adjust_selected(&mut self, delta: i32) -> Option<VolumeCommand> {
        let control = self.controls().get(self.selected)?.clone();
        let ControlKind::Slider {
            target, percent, ..
        } = control.kind
        else {
            return None;
        };
        let percent = percent
            .cast_signed()
            .saturating_add(delta)
            .clamp(0, 100)
            .cast_unsigned();
        self.set_volume(&target, percent);
        Some(VolumeCommand::SetVolume { target, percent })
    }

    /// Show a requested volume straight away, and hold it against the
    /// snapshot that may already be in flight. See [`PendingEdits`].
    pub fn set_volume(&mut self, target: &VolumeTarget, percent: u32) {
        self.snapshot.set_volume(target, percent);
        self.pending.volume(target, percent);
    }

    /// Flip a mute straight away, for the same reason as [`Self::set_volume`].
    pub fn toggle_mute(&mut self, target: &VolumeTarget) {
        let muted = !self.snapshot.is_muted(target);
        self.snapshot.set_mute(target, muted);
        self.pending.mute(target, muted);
    }

    fn clamp_selection(&mut self) {
        let len = self.controls().len();
        self.selected = self.selected.min(len.saturating_sub(1));
        self.keep_visible();
    }

    const fn keep_visible(&mut self) {
        if self.selected < self.scroll {
            self.scroll = self.selected;
        } else if self.selected >= self.scroll + VISIBLE_CONTROLS {
            self.scroll = self.selected + 1 - VISIBLE_CONTROLS;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pactl::{PactlDevice, PactlStream};

    impl PactlDevice {
        pub(crate) fn fixture(name: &str, description: &str, percent: u32) -> Self {
            Self::fixture_value(name, description, percent)
        }
    }

    impl PactlStream {
        pub(crate) fn stream_fixture(index: u32, name: &str, percent: u32) -> Self {
            Self::fixture_stream(index, name, percent)
        }
    }

    fn snapshot() -> MixerSnapshot {
        MixerSnapshot {
            sinks: vec![PactlDevice::fixture("sink-a", "Speakers", 60)],
            sources: vec![PactlDevice::fixture("source-a", "Mic", 40)],
            sink_inputs: vec![PactlStream::stream_fixture(7, "Music", 75)],
            source_outputs: Vec::new(),
            default_sink: "sink-a".into(),
            default_source: "source-a".into(),
        }
    }

    #[test]
    fn controls_include_default_device_sliders_and_programs() {
        let mut model = VolumeModel::default();
        model.apply(snapshot());
        let controls = model.controls();
        assert_eq!(controls.len(), 5);
        assert_eq!(controls[1].section, "Output volume");
        assert_eq!(controls[3].section, "Input volume");
        assert_eq!(controls[4].section, "Playback programs");
    }

    #[test]
    fn right_key_adjusts_a_slider() {
        let mut model = VolumeModel::default();
        model.apply(snapshot());
        model.selected = 1;
        assert_eq!(
            model.adjust_selected(5),
            Some(VolumeCommand::SetVolume {
                target: VolumeTarget::Sink("sink-a".into()),
                percent: 65,
            })
        );
    }

    #[test]
    fn enter_on_default_device_toggles_mute() {
        let mut model = VolumeModel::default();
        model.apply(snapshot());
        assert_eq!(
            model.selected_command(),
            Some(VolumeCommand::ToggleMute {
                target: VolumeTarget::Sink("sink-a".into()),
            })
        );
    }
}
