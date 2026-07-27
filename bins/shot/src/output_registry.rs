//! Discovering outputs and their connector names.
//!
//! `--output DP-1` needs the mapping from a human-typed connector name to a
//! `wl_output` object. That name arrives in `wl_output.name`, a version-4
//! event, which is why `platform`'s `WL_OUTPUT` sits at v4 -- the information
//! is already free, we just have to collect it before capturing.

use platform::Result;
use platform::conn::Sender;
use platform::event::RawEvent;
use platform::id::{CallbackId, DisplayId, ObjectId, OutputId, RegistryId};
use platform::protocol::{
    self as proto, CallbackEvent, DisplayRequest, OutputEvent,
};
use platform::surface::SurfaceBootstrap;
use platform::wire::{MessageBuilder, WireEvent};
use quetzalcoatl::spsc::Consumer;

/// One discovered output and, once its `name` event arrives, its connector.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiscoveredOutput {
    pub id: OutputId,
    /// The connector name (`DP-1`), absent on compositors older than
    /// `wl_output` v4.
    pub name: Option<String>,
}

/// The set of outputs this capture may target.
pub struct OutputRegistry {
    outputs: Vec<DiscoveredOutput>,
}

impl OutputRegistry {
    /// Start empty; registry globals fill the set.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            outputs: Vec::new(),
        }
    }

    /// Bind a newly announced `wl_output`.
    pub fn bind(
        &mut self,
        sender: &mut Sender,
        registry: RegistryId,
        global_name: u32,
        advertised_version: u32,
    ) -> Result<()> {
        let version = proto::WL_OUTPUT.version.min(advertised_version);
        let id: OutputId = SurfaceBootstrap::bind(
            sender,
            registry,
            global_name,
            proto::WL_OUTPUT.name,
            version,
        )?;
        self.outputs.push(DiscoveredOutput { id, name: None });
        Ok(())
    }

    /// Collect `wl_output.name` events until a sync round-trip proves the
    /// compositor has sent everything it intends to.
    ///
    /// Without the round-trip we'd have to guess how long to wait for names,
    /// and a `--output` lookup could fail purely by racing the compositor.
    pub fn collect_names(
        &mut self,
        sender: &mut Sender,
        events: &mut Consumer<RawEvent>,
    ) -> Result<()> {
        let callback: CallbackId = sender.alloc_id();
        sender
            .request(MessageBuilder::new(DisplayId(1), DisplayRequest::Sync).new_id(callback))?;
        loop {
            let Some(event) = events.pop_block() else {
                // The reader stopped; whatever names we have are all we get.
                return Ok(());
            };
            // The sync `done` ends discovery. It carries one argument, so it
            // arrives inlined as `Wire` rather than as raw bytes.
            if event.is(callback) && event.event::<CallbackEvent>() == Some(CallbackEvent::Done) {
                return Ok(());
            }
            // `wl_output.name` is string-bearing, but the reader only falls
            // back to `Bytes` for `wl_display`/`wl_registry`; every other
            // object's events come through as `Wire` with the full message
            // retained, so the string must be re-parsed out of `wire_bytes`.
            // Matching on `RawEvent::Bytes` here instead silently finds no
            // names at all, and `--output` can then never match anything.
            if event.event::<OutputEvent>() != Some(OutputEvent::Name) {
                continue;
            }
            let Some(bytes) = event.wire_bytes() else {
                continue;
            };
            let Some((mut wire, _)) = WireEvent::parse(bytes) else {
                continue;
            };
            if let Some(output) = self
                .outputs
                .iter_mut()
                .find(|o| o.id.raw() == wire.object_id)
            {
                output.name = Some(wire.string());
            }
        }
    }

    /// Every discovered output, in announcement order.
    #[must_use]
    pub fn outputs(&self) -> &[DiscoveredOutput] {
        &self.outputs
    }

    /// Find an output by connector name.
    #[must_use]
    pub fn by_name(&self, name: &str) -> Option<&DiscoveredOutput> {
        self.outputs
            .iter()
            .find(|output| output.name.as_deref() == Some(name))
    }

    /// The names available, for an error message when a lookup fails.
    ///
    /// Being told "no output named DP-9" is far less useful than also being
    /// told which ones exist.
    #[must_use]
    pub fn known_names(&self) -> String {
        let names: Vec<&str> = self
            .outputs
            .iter()
            .filter_map(|output| output.name.as_deref())
            .collect();
        if names.is_empty() {
            "none (compositor does not report output names)".to_string()
        } else {
            names.join(", ")
        }
    }
}

impl Default for OutputRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::{DiscoveredOutput, OutputRegistry};
    use platform::id::OutputId;

    /// Builds registries without a compositor, keeping fixture construction
    /// on an owner rather than in a bare helper.
    struct Fixture;

    impl Fixture {
        fn with(outputs: &[(u32, Option<&str>)]) -> OutputRegistry {
            OutputRegistry {
                outputs: outputs
                    .iter()
                    .map(|(id, name)| DiscoveredOutput {
                        id: OutputId(*id),
                        name: name.map(ToString::to_string),
                    })
                    .collect(),
            }
        }
    }

    #[test]
    fn finds_an_output_by_its_connector_name() {
        let registry = Fixture::with(&[(10, Some("eDP-1")), (11, Some("DP-1"))]);
        assert_eq!(
            registry.by_name("DP-1").map(|o| o.id),
            Some(OutputId(11))
        );
    }

    #[test]
    fn an_unknown_name_finds_nothing_rather_than_the_first_output() {
        // Falling back to "some output" would silently screenshot the wrong
        // monitor, which looks like a capture bug rather than a typo.
        let registry = Fixture::with(&[(10, Some("eDP-1"))]);
        assert!(registry.by_name("DP-9").is_none());
    }

    #[test]
    fn known_names_lists_what_the_user_could_have_meant() {
        let registry = Fixture::with(&[(10, Some("eDP-1")), (11, Some("DP-1"))]);
        assert_eq!(registry.known_names(), "eDP-1, DP-1");
    }

    #[test]
    fn known_names_explains_an_older_compositor_rather_than_showing_nothing() {
        // Pre-v4 compositors send no names at all; an empty list would look
        // like "no outputs", which is a different and more alarming problem.
        let registry = Fixture::with(&[(10, None)]);
        assert!(registry.known_names().contains("does not report"));
    }
}
