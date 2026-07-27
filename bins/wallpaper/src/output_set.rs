//! Registry-name ownership for the wallpaper client's output lifecycle.
//!
//! `wl_registry.global_remove` identifies an output by its *registry name*,
//! while every subsequent output event uses the client-created `wl_output`
//! object id. Keeping that translation in one owner prevents a background
//! surface surviving after its output disappeared (and prevents a replacement
//! global from being mistaken for the old monitor).

use platform::Result;
use platform::conn::Sender;
use platform::id::{OutputId, RegistryId};
use platform::protocol::{self as proto};
use platform::surface::SurfaceBootstrap;

/// A live `wl_output` together with the compositor-global name that created it.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Output {
    /// The registry-global name used by `wl_registry.global_remove`.
    pub global_name: u32,
    /// The client-side object through which this output sends events.
    pub id: OutputId,
}

/// The wallpaper client's currently advertised outputs.
///
/// This is deliberately a small vector rather than a map: ordinary desktops
/// expose one to four outputs, and preserving announcement order gives a
/// stable, unsurprising order for future per-output configuration matching.
pub struct OutputSet {
    outputs: Vec<Output>,
}

impl OutputSet {
    /// Start with no compositor outputs. Registry globals fill the set.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            outputs: Vec::new(),
        }
    }

    /// Bind and record one newly announced `wl_output`.
    ///
    /// Duplicate announcements are ignored. Wayland globals are unique by
    /// name, and ignoring a duplicate is safer than allocating a second object
    /// that would both claim to represent the same physical output.
    pub fn bind_global(
        &mut self,
        sender: &mut Sender,
        registry: RegistryId,
        global_name: u32,
        advertised_version: u32,
    ) -> Result<Option<Output>> {
        if self.output_for_global(global_name).is_some() {
            return Ok(None);
        }
        let version = proto::WL_OUTPUT.version.min(advertised_version);
        let id = SurfaceBootstrap::bind(
            sender,
            registry,
            global_name,
            proto::WL_OUTPUT.name,
            version,
        )?;
        let output = Output { global_name, id };
        self.outputs.push(output);
        Ok(Some(output))
    }

    /// Forget an output after the compositor removes its registry global.
    ///
    /// Returns the removed output so the caller can destroy its associated
    /// layer surface and shm objects before dropping its per-output state.
    pub fn remove_global(&mut self, global_name: u32) -> Option<Output> {
        let index = self
            .outputs
            .iter()
            .position(|output| output.global_name == global_name)?;
        Some(self.outputs.remove(index))
    }

    /// Look up the tracked output receiving an event for `id`.
    #[must_use]
    pub fn by_id(&self, id: OutputId) -> Option<Output> {
        self.outputs.iter().copied().find(|output| output.id == id)
    }

    /// Outputs in registry-announcement order.
    #[must_use]
    pub fn outputs(&self) -> &[Output] {
        &self.outputs
    }

    fn output_for_global(&self, global_name: u32) -> Option<Output> {
        self.outputs
            .iter()
            .copied()
            .find(|output| output.global_name == global_name)
    }
}

impl Default for OutputSet {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::{Output, OutputSet};
    use platform::id::OutputId;

    /// Owns fixture construction so tests don't introduce bare helper
    /// functions into a codebase that keeps logic on concrete owners.
    struct Fixture;

    impl Fixture {
        fn output(global_name: u32, object_id: u32) -> Output {
            Output {
                global_name,
                id: OutputId(object_id),
            }
        }

        fn with_outputs(outputs: &[Output]) -> OutputSet {
            OutputSet {
                outputs: outputs.to_vec(),
            }
        }
    }

    #[test]
    fn remove_global_returns_exact_output_and_preserves_others() {
        let first = Fixture::output(7, 20);
        let second = Fixture::output(9, 21);
        let mut outputs = Fixture::with_outputs(&[first, second]);

        assert_eq!(outputs.remove_global(7), Some(first));
        assert_eq!(outputs.outputs(), &[second]);
        assert_eq!(outputs.by_id(OutputId(20)), None);
        assert_eq!(outputs.by_id(OutputId(21)), Some(second));
    }

    #[test]
    fn removing_unknown_global_does_not_disturb_live_outputs() {
        let output = Fixture::output(3, 12);
        let mut outputs = Fixture::with_outputs(&[output]);

        assert_eq!(outputs.remove_global(99), None);
        assert_eq!(outputs.outputs(), &[output]);
    }
}
