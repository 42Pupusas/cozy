//! `pactl --format=json` client: the typed view of `PipeWire`'s `PulseAudio`
//! compatibility API. One struct per `pactl` subcommand output shape, one
//! marker type owning the queries and the mutations.

use std::process::{Command, Stdio};

use json_bourne::{FromJson, parse_str};

/// `volume.front-left` out of pactl's JSON — one channel's raw value and
/// its percentage.
#[derive(Debug, Default, Clone, FromJson)]
#[bourne(deny_unknown_fields = false)]
struct ChannelVolume {
    #[bourne(default)]
    value_percent: u32,
}

/// The `volume` object: pactl reports per-channel entries with dashes in
/// their keys (`front-left`); the panel only needs one channel's
/// percentage (a stereo pair always agrees).
#[derive(Debug, Default, Clone, FromJson)]
#[bourne(deny_unknown_fields = false)]
struct VolumeMap {
    #[bourne(rename = "front-left", default)]
    front_left: Option<ChannelVolume>,
    #[bourne(default)]
    mono: Option<ChannelVolume>,
}

#[derive(Debug, Default, Clone, FromJson)]
#[bourne(deny_unknown_fields = false)]
struct StreamProperties {
    #[bourne(rename = "application.name", default)]
    application_name: String,
    #[bourne(rename = "media.name", default)]
    media_name: String,
}

/// One entry out of `pactl --format=json list sinks` / `list sources`.
///
/// Lenient (`deny_unknown_fields = false`): a sink object carries dozens
/// of fields (`ports`, `formats`, `proplist`, ...) this panel never reads.
#[derive(Debug, Clone, FromJson)]
#[bourne(deny_unknown_fields = false)]
pub struct PactlDevice {
    #[bourne(default)]
    pub name: String,
    #[bourne(default)]
    pub description: String,
    /// Driver string, e.g. `"PipeWire"` or `"ALSA"`.
    #[bourne(default)]
    pub driver: String,
    #[bourne(default)]
    volume: VolumeMap,
    #[bourne(default)]
    pub mute: bool,
    /// `state`: "running" / "suspended" / "idle" / "unknown".
    #[bourne(default)]
    pub state: String,
}

/// One entry out of `pactl --format=json list sink-inputs` — a running
/// program's playback stream.
#[derive(Debug, Clone, FromJson)]
#[bourne(deny_unknown_fields = false)]
pub struct PactlStream {
    #[bourne(default)]
    pub index: u32,
    #[bourne(default)]
    pub name: String,
    #[bourne(default)]
    properties: StreamProperties,
    #[bourne(default)]
    volume: VolumeMap,
    #[bourne(default)]
    pub mute: bool,
}

impl PactlDevice {
    #[must_use]
    pub fn volume_percent(&self) -> u32 {
        self.volume
            .front_left
            .as_ref()
            .or(self.volume.mono.as_ref())
            .map_or(0, |channel| channel.value_percent)
    }

    /// Record a volume the user just asked for, before `pactl` has
    /// confirmed it.
    pub const fn set_volume_percent(&mut self, percent: u32) {
        self.volume.set_percent(percent);
    }

    #[cfg(test)]
    pub(crate) fn fixture_value(name: &str, description: &str, percent: u32) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            driver: "PipeWire".into(),
            volume: VolumeMap {
                front_left: Some(ChannelVolume {
                    value_percent: percent,
                }),
                mono: None,
            },
            mute: false,
            state: "running".into(),
        }
    }
}

impl VolumeMap {
    const fn set_percent(&mut self, percent: u32) {
        if let Some(channel) = self.front_left.as_mut() {
            channel.value_percent = percent;
        }
        if let Some(channel) = self.mono.as_mut() {
            channel.value_percent = percent;
        }
        if self.front_left.is_none() && self.mono.is_none() {
            self.front_left = Some(ChannelVolume {
                value_percent: percent,
            });
        }
    }
}

impl PactlStream {
    #[must_use]
    pub fn volume_percent(&self) -> u32 {
        self.volume
            .front_left
            .as_ref()
            .or(self.volume.mono.as_ref())
            .map_or(0, |channel| channel.value_percent)
    }

    /// Record a volume the user just asked for, before `pactl` has
    /// confirmed it.
    pub const fn set_volume_percent(&mut self, percent: u32) {
        self.volume.set_percent(percent);
    }

    #[must_use]
    pub fn display_name(&self) -> &str {
        if !self.properties.application_name.is_empty() {
            &self.properties.application_name
        } else if !self.properties.media_name.is_empty() {
            &self.properties.media_name
        } else {
            &self.name
        }
    }

    #[cfg(test)]
    pub(crate) fn fixture_stream(index: u32, name: &str, percent: u32) -> Self {
        Self {
            index,
            name: name.into(),
            properties: StreamProperties {
                application_name: name.into(),
                media_name: String::new(),
            },
            volume: VolumeMap {
                front_left: Some(ChannelVolume {
                    value_percent: percent,
                }),
                mono: None,
            },
            mute: false,
        }
    }
}

/// One entry out of `pactl --format=json list source-outputs` — a running
/// program's capture stream.
pub type PactlCapture = PactlStream;

/// The whole panel model in one poll: default devices both ways plus
/// every program stream.
#[derive(Debug, Default, Clone)]
pub struct MixerSnapshot {
    pub sinks: Vec<PactlDevice>,
    pub sources: Vec<PactlDevice>,
    pub sink_inputs: Vec<PactlStream>,
    pub source_outputs: Vec<PactlCapture>,
    /// pactl spells the default sink `@DEFAULT_SINK@`; get-default-sink
    /// prints its name directly.
    pub default_sink: String,
    pub default_source: String,
}

impl MixerSnapshot {
    /// Overwrite a target's volume, if it is still present.
    pub fn set_volume(&mut self, target: &crate::model::VolumeTarget, percent: u32) {
        use crate::model::VolumeTarget;
        match target {
            VolumeTarget::Sink(name) => {
                if let Some(d) = self.sinks.iter_mut().find(|d| &d.name == name) {
                    d.set_volume_percent(percent);
                }
            }
            VolumeTarget::Source(name) => {
                if let Some(d) = self.sources.iter_mut().find(|d| &d.name == name) {
                    d.set_volume_percent(percent);
                }
            }
            VolumeTarget::SinkInput(index) => {
                if let Some(s) = self.sink_inputs.iter_mut().find(|s| &s.index == index) {
                    s.set_volume_percent(percent);
                }
            }
            VolumeTarget::SourceOutput(index) => {
                if let Some(s) = self.source_outputs.iter_mut().find(|s| &s.index == index) {
                    s.set_volume_percent(percent);
                }
            }
        }
    }

    /// Overwrite a target's mute state, if it is still present.
    pub fn set_mute(&mut self, target: &crate::model::VolumeTarget, muted: bool) {
        use crate::model::VolumeTarget;
        match target {
            VolumeTarget::Sink(name) => {
                if let Some(d) = self.sinks.iter_mut().find(|d| &d.name == name) {
                    d.mute = muted;
                }
            }
            VolumeTarget::Source(name) => {
                if let Some(d) = self.sources.iter_mut().find(|d| &d.name == name) {
                    d.mute = muted;
                }
            }
            VolumeTarget::SinkInput(index) => {
                if let Some(s) = self.sink_inputs.iter_mut().find(|s| &s.index == index) {
                    s.mute = muted;
                }
            }
            VolumeTarget::SourceOutput(index) => {
                if let Some(s) = self.source_outputs.iter_mut().find(|s| &s.index == index) {
                    s.mute = muted;
                }
            }
        }
    }

    /// Read a target's current mute state.
    #[must_use]
    pub fn is_muted(&self, target: &crate::model::VolumeTarget) -> bool {
        use crate::model::VolumeTarget;
        match target {
            VolumeTarget::Sink(name) => self
                .sinks
                .iter()
                .find(|d| &d.name == name)
                .is_some_and(|d| d.mute),
            VolumeTarget::Source(name) => self
                .sources
                .iter()
                .find(|d| &d.name == name)
                .is_some_and(|d| d.mute),
            VolumeTarget::SinkInput(index) => self
                .sink_inputs
                .iter()
                .find(|s| &s.index == index)
                .is_some_and(|s| s.mute),
            VolumeTarget::SourceOutput(index) => self
                .source_outputs
                .iter()
                .find(|s| &s.index == index)
                .is_some_and(|s| s.mute),
        }
    }
}

/// Marker type owning the pactl queries and mutations.
pub struct Pactl;

impl Pactl {
    /// Run `pactl --format=json <args>` and parse the JSON stdout.
    fn json<T: for<'a> FromJson<'a>>(args: &[&str]) -> Option<T> {
        let out = Command::new("pactl")
            .arg("--format=json")
            .args(args)
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        let text = String::from_utf8(out.stdout).ok()?;
        parse_str(&text).ok()
    }

    /// Run `pactl <args>` fire-and-forget (a mutation), matching the bar's
    /// own volume module's posture: spawn, never block. The server's
    /// change notification triggers the read-back.
    fn spawn(args: &[&str]) {
        let _ = Command::new("pactl")
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
    }

    /// Everything the panel shows, in four subprocess round trips.
    #[must_use]
    pub fn snapshot() -> MixerSnapshot {
        MixerSnapshot {
            sinks: Self::json::<Vec<PactlDevice>>(&["list", "sinks"]).unwrap_or_default(),
            sources: Self::json::<Vec<PactlDevice>>(&["list", "sources"]).unwrap_or_default(),
            sink_inputs: Self::json::<Vec<PactlStream>>(&["list", "sink-inputs"])
                .unwrap_or_default(),
            source_outputs: Self::json::<Vec<PactlCapture>>(&["list", "source-outputs"])
                .unwrap_or_default(),
            default_sink: Self::default_name("sink"),
            default_source: Self::default_name("source"),
        }
    }

    fn default_name(kind: &str) -> String {
        Command::new("pactl")
            .args([format!("get-default-{kind}").as_str()])
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .ok()
            .and_then(|o| {
                (o.status.success()).then(|| {
                    String::from_utf8(o.stdout)
                        .unwrap_or_default()
                        .trim()
                        .to_owned()
                })
            })
            .unwrap_or_default()
    }

    /// Set a sink's volume to an absolute percentage.
    pub fn set_sink_volume(name: &str, pct: u32) {
        Self::spawn(&["set-sink-volume", name, &format!("{pct}%")]);
    }

    /// Set a source's volume to an absolute percentage.
    pub fn set_source_volume(name: &str, pct: u32) {
        Self::spawn(&["set-source-volume", name, &format!("{pct}%")]);
    }

    /// Set one program stream's volume to an absolute percentage.
    pub fn set_sink_input_volume(index: u32, pct: u32) {
        let idx = index.to_string();
        Self::spawn(&["set-sink-input-volume", &idx, &format!("{pct}%")]);
    }

    pub fn set_source_output_volume(index: u32, pct: u32) {
        let idx = index.to_string();
        Self::spawn(&["set-source-output-volume", &idx, &format!("{pct}%")]);
    }

    /// Toggle a device's mute.
    pub fn toggle_sink_mute(name: &str) {
        Self::spawn(&["set-sink-mute", name, "toggle"]);
    }

    pub fn toggle_source_mute(name: &str) {
        Self::spawn(&["set-source-mute", name, "toggle"]);
    }

    pub fn toggle_sink_input_mute(index: u32) {
        let idx = index.to_string();
        Self::spawn(&["set-sink-input-mute", &idx, "toggle"]);
    }

    pub fn toggle_source_output_mute(index: u32) {
        let idx = index.to_string();
        Self::spawn(&["set-source-output-mute", &idx, "toggle"]);
    }

    /// Make `name` the default sink (output).
    pub fn set_default_sink(name: &str) {
        Self::spawn(&["set-default-sink", name]);
    }

    /// Make `name` the default source (input).
    pub fn set_default_source(name: &str) {
        Self::spawn(&["set-default-source", name]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A trimmed but shape-accurate slice of real `pactl --format=json
    /// list sinks` output.
    const SINKS_JSON: &str = r#"[
        {
            "index": 0,
            "name": "alsa_output.pci-0000_00_1f.3.analog-stereo",
            "driver": "PipeWire",
            "description": "Built-in Audio Analog Stereo",
            "sample_spec": {"format": "s16le", "rate": 48000, "channels": 2},
            "state": "running",
            "mute": false,
            "properties": {"application.name": "Music Player", "media.name": "Song"},
            "volume": {
                "front-left": {"value": 39269, "value_percent": 60},
                "front-right": {"value": 39269, "value_percent": 60}
            },
            "ports": [{"name": "analog-output-speaker"}]
        },
        {
            "index": 1,
            "name": "bluez_output.AA_BB_CC_DD_EE_FF.1",
            "driver": "PipeWire",
            "description": "WH-1000XM5",
            "state": "suspended",
            "mute": true,
            "volume": {
                "front-left": {"value": 32768, "value_percent": 50},
                "front-right": {"value": 32768, "value_percent": 50}
            }
        }
    ]"#;

    #[test]
    fn mono_stream_reads_nested_properties_and_volume() {
        let json = r#"[{
            "index": 9,
            "name": "audio-stream",
            "properties": {"application.name": "Recorder"},
            "volume": {"mono": {"value_percent": 44}},
            "mute": false
        }]"#;
        let streams: Vec<PactlStream> = parse_str(json).unwrap();
        assert_eq!(streams[0].display_name(), "Recorder");
        assert_eq!(streams[0].volume_percent(), 44);
    }

    #[test]
    fn sinks_parse_with_defaults_for_absent_fields() {
        let sinks: Vec<PactlDevice> = parse_str(SINKS_JSON).unwrap();
        assert_eq!(sinks.len(), 2);
        assert_eq!(sinks[0].description, "Built-in Audio Analog Stereo");
        assert_eq!(sinks[0].driver, "PipeWire");
        assert_eq!(sinks[0].state, "running");
        assert!(!sinks[0].mute);
        assert_eq!(sinks[0].volume_percent(), 60);
        // The second sink has no description-level surprises but IS muted.
        assert_eq!(sinks[1].description, "WH-1000XM5");
        assert!(sinks[1].mute);
    }
}
