//! A live-updatable [`AppearanceSettings`] snapshot, owned by a single
//! dedicated thread and reached only by message passing.
//!
//! [`SettingsService::dispatcher`]'s `ReadAll`/`Read` handlers must always
//! answer with the *current* settings, not a value frozen at process
//! startup -- but they're registered once as `Fn` closures (`dbus_server`'s
//! [`dbus_server::Dispatcher::register_method`] requires `Fn`, not
//! `FnMut`), so a later config reload has to reach them some way that
//! isn't "the closure owns the value outright".
//!
//! This deliberately does **not** reach for `Arc<Mutex<AppearanceSettings>>`.
//! Every worker topology already in this workspace (`bins/terminal`'s
//! pty/parser/ui threads, `platform::conn::WindowSender`'s
//! windows-push-onto-a-ring-and-the-event-loop-drains-it design) is a
//! single owner thread holding the real mutable state, reached by other
//! threads only through a `quetzalcoatl` ring -- never a lock. A `Mutex`
//! here would be the only shared-memory lock in the entire codebase for a
//! piece of state that's actually simpler as an owned value on its own
//! thread: [`Self::new`] spawns exactly one thread that owns an
//! `AppearanceSettings` as a plain field (no wrapper at all), and
//! [`Self::snapshot`]/[`Self::reload`] are just synchronous request/reply
//! round-trips over rings, not different in kind from `bins/terminal`'s
//! own `reply_tx` (a `PtyWorker` reply channel back to a caller) -- just
//! bidirectional per call, via a fresh single-slot [`quetzalcoatl::spsc`]
//! reply ring instead of a long-lived one, since each request needs
//! exactly one reply and no more.
//!
//! The owner thread needs no waker/eventfd of its own: `quetzalcoatl`'s
//! `pop_block`/`push` already park/unpark the two sides directly, the
//! same primitive `bins/terminal`'s worker threads block on.

use config::Config;
use dbus::value::Value;
use quetzalcoatl::capacity::Capacity;
use quetzalcoatl::{mpsc, spsc};

use portal_core::AppearanceSettings;

/// One request the owner thread understands, each carrying the
/// single-use reply ring's producer half -- the caller keeps the
/// consumer half and blocks on it, exactly the shape a synchronous RPC
/// over a ring takes without any of `dbus`/`dbus-server`'s own wire
/// format being involved (this is purely in-process).
enum StateRequest {
    /// Answer with the current snapshot.
    Snapshot(spsc::Producer<AppearanceSettings>),
    /// Replace the snapshot with `cfg`'s settings; answer with whichever
    /// keys actually changed (see [`AppearanceSettings::changed_from`]).
    /// Boxed so this variant doesn't balloon `StateRequest`'s size to
    /// `Config`'s (currently ~700 bytes) for every request, including the
    /// far more common `Snapshot` -- the ring stores `StateRequest` by
    /// value in every slot, so the enum's size is the ring's per-slot
    /// cost regardless of which variant is actually in use.
    Reload(Box<Config>, spsc::Producer<Vec<(&'static str, Value)>>),
}

/// A cheaply cloneable handle (an `mpsc::Producer` clone) to the owner
/// thread holding the live `AppearanceSettings`.
///
/// Cloned once per registered dispatch closure in
/// `SettingsService::dispatcher`, exactly as `mpsc::Producer::clone`'s own
/// doc describes: each clone is an independent producer that can push
/// requests concurrently, with no shared state beyond the lock-free ring
/// itself.
#[derive(Clone)]
pub struct SettingsState {
    request_tx: mpsc::Producer<StateRequest>,
}

// SAFETY: mirrors `platform::conn::WindowSender`'s identical impl and
// justification. `mpsc::Producer::push` is documented safe to call
// concurrently from multiple clones (that's the whole point of MPSC --
// each clone claims slots via atomic fetch-and-add, no aliasing between
// them); the only reason `Sync` isn't auto-derived is `Producer`'s
// internal `Cell<usize>` cache, which is never touched by two clones at
// once because each clone owns its own `Cell`, not a shared one.
unsafe impl Sync for SettingsState {}

/// Requests in flight at once before a producer would see a full ring --
/// generous headroom for what is, in practice, one call at a time (this
/// backend is not concurrently hammered), sized to match this
/// workspace's other small control-plane rings (e.g.
/// `bar_window.rs`'s `EVENT_RING_CAP` neighbors) rather than the large
/// data-plane rings `bins/terminal` sizes for throughput.
const REQUEST_RING_CAPACITY: usize = 16;

impl SettingsState {
    /// Snapshot `cfg`'s appearance settings as the owner thread's initial
    /// state, and spawn that thread. The thread runs until every
    /// [`SettingsState`] clone (and so every `mpsc::Producer` clone) has
    /// been dropped, at which point the request ring closes,
    /// `pop_block` returns `None`, and the thread exits on its own --
    /// no explicit shutdown signal needed.
    #[must_use]
    pub fn new(cfg: &Config) -> Self {
        let (request_tx, mut request_rx) =
            mpsc::RingBuffer::<StateRequest>::new(Capacity::exact(REQUEST_RING_CAPACITY)).split();
        let mut current = AppearanceSettings::from_config(cfg);
        std::thread::spawn(move || {
            while let Some(request) = request_rx.pop_block() {
                match request {
                    StateRequest::Snapshot(reply_tx) => {
                        // A dropped reply consumer (the caller gave up)
                        // is not this thread's problem -- there is no
                        // one left to see the error either way.
                        let _ = reply_tx.push(current);
                    }
                    StateRequest::Reload(cfg, reply_tx) => {
                        let new = AppearanceSettings::from_config(&cfg);
                        let changed = current.changed_from(new);
                        current = new;
                        let _ = reply_tx.push(changed);
                    }
                }
            }
        });
        Self { request_tx }
    }

    /// The current settings snapshot, fetched from the owner thread over
    /// a fresh one-shot reply ring. `AppearanceSettings` is `Copy`, so
    /// the round trip hands back an owned value with nothing left
    /// borrowed from the owner thread.
    #[must_use]
    pub fn snapshot(&self) -> AppearanceSettings {
        let (reply_tx, mut reply_rx) = spsc::RingBuffer::new(Capacity::exact(1)).split();
        // The request ring only fills if the owner thread has fallen
        // behind by `REQUEST_RING_CAPACITY` in-flight requests, which
        // cannot happen for a synchronous, one-request-at-a-time caller
        // (this handle blocks on the reply before making another
        // request) -- a push failure here would mean the owner thread
        // has already exited, in which case `pop_block` below correctly
        // reports that by returning `None`.
        let _ = self.request_tx.push(StateRequest::Snapshot(reply_tx));
        reply_rx
            .pop_block()
            .expect("the settings owner thread outlives every SettingsState handle")
    }

    /// Replace the owner thread's stored snapshot with `cfg`'s settings,
    /// returning whichever documented keys actually changed value (see
    /// [`AppearanceSettings::changed_from`]) -- exactly the set a caller
    /// (`PortalLoop`) needs to know which `SettingChanged` signals, if
    /// any, to emit after a config-file save.
    #[must_use]
    pub fn reload(&self, cfg: &Config) -> Vec<(&'static str, Value)> {
        let (reply_tx, mut reply_rx) = spsc::RingBuffer::new(Capacity::exact(1)).split();
        let _ = self
            .request_tx
            .push(StateRequest::Reload(Box::new(cfg.clone()), reply_tx));
        reply_rx
            .pop_block()
            .expect("the settings owner thread outlives every SettingsState handle")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use config::Color;

    fn config_with_background(rgb: (u8, u8, u8)) -> Config {
        let mut cfg = Config::default();
        cfg.files.background = Some(Color::from_rgb(rgb.0, rgb.1, rgb.2));
        cfg
    }

    #[test]
    fn snapshot_reflects_the_config_it_was_built_from() {
        let cfg = config_with_background((0x18, 0x18, 0x18));
        let state = SettingsState::new(&cfg);
        assert_eq!(state.snapshot(), AppearanceSettings::from_config(&cfg));
    }

    #[test]
    fn reload_with_an_identical_config_reports_no_changes() {
        let cfg = config_with_background((0x18, 0x18, 0x18));
        let state = SettingsState::new(&cfg);
        assert_eq!(state.reload(&cfg), vec![]);
    }

    #[test]
    fn reload_with_a_different_background_reports_color_scheme_changed() {
        let cfg = config_with_background((0x18, 0x18, 0x18));
        let state = SettingsState::new(&cfg);
        let new_cfg = config_with_background((0xf8, 0xf8, 0xf8));
        let changed = state.reload(&new_cfg);
        assert_eq!(changed.len(), 1);
        assert_eq!(changed[0].0, "color-scheme");
    }

    #[test]
    fn reload_actually_replaces_the_stored_snapshot() {
        let cfg = config_with_background((0x18, 0x18, 0x18));
        let state = SettingsState::new(&cfg);
        let new_cfg = config_with_background((0xf8, 0xf8, 0xf8));
        let _ = state.reload(&new_cfg);
        assert_eq!(state.snapshot(), AppearanceSettings::from_config(&new_cfg));
    }

    #[test]
    fn cloned_handles_share_the_same_owner_thread_state() {
        let cfg = config_with_background((0x18, 0x18, 0x18));
        let state = SettingsState::new(&cfg);
        let cloned = state.clone();
        let new_cfg = config_with_background((0xf8, 0xf8, 0xf8));
        let _ = state.reload(&new_cfg);
        assert_eq!(cloned.snapshot(), AppearanceSettings::from_config(&new_cfg));
    }
}
