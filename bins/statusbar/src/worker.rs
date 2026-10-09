//! Background worker: owns every module computation and scroll-triggered
//! volume/brightness adjustment, on its own thread away from the Wayland
//! render/event loop -- see `main.rs`'s module doc for why.

use std::time::{Duration, Instant};

use panel_ui::CommandInbox;
use quetzalcoatl::mpsc::Producer;
use statusbar::{ModuleKind, ScrollAccumulator, TrayItem, Volume};
use waker::Poller;

/// How often each module recomputes. Cheap `/proc`/`/sys` reads run often;
/// modules that shell out to a CLI (`iw`, `pactl`, `bluetoothctl`) run less
/// often to keep from spawning processes constantly.
const CLOCK_PERIOD: Duration = Duration::from_secs(1);
const FAST_PERIOD: Duration = Duration::from_secs(2); // battery, temp, mem
const SLOW_PERIOD: Duration = Duration::from_secs(5); // wifi, bluetooth, volume, brightness
/// How often `[[bar.notifications]]` re-runs `niri msg --json windows`.
/// Deliberately its own (short) period rather than piggybacking on
/// `SLOW_PERIOD`: an unread mention is exactly the kind of thing a user
/// wants to notice promptly, and a `niri msg` round trip is cheap (a local
/// IPC socket call, not a network request or a `pactl`-style mixer probe).
const NOTIFICATIONS_PERIOD: Duration = Duration::from_secs(2);
/// `cpu_usage` samples twice ~120ms apart, so give it its own slightly
/// longer period rather than sharing `FAST_PERIOD`. Currently unused --
/// see the comment where it would be called in `State::refresh`.
#[allow(dead_code)]
const CPU_PERIOD: Duration = Duration::from_secs(2);

/// Percentage points nudged per wheel detent when scroll-adjusting
/// vol-out/vol-in/brightness.
const SCROLL_STEP_PCT: i32 = 5;
/// Minimum spacing between `force_refresh_control` calls, in milliseconds.
const CONTROL_REFRESH_DEBOUNCE_MS: u64 = 150;
/// Minimum spacing between actually spawning a `pactl`/sysfs-brightness
/// *adjustment* while scrolling.
const SCROLL_APPLY_DEBOUNCE_MS: u64 = 60;
/// Minimum spacing between spawning `cosmic-settings` for a click, so a
/// double-click (or two clicks landing in the same channel-drain batch)
/// only opens one window instead of two racing to grab focus.
const CLICK_DEBOUNCE_MS: u64 = 400;

const DASH: &str = "—";

/// Tracks "has at least `period` elapsed since the last time this fired."
///
/// Every module-refresh timer (clock/fast/slow/notifications) and every
/// debounce guard (control-refresh/scroll-apply/click-spawn) in this file is
/// the exact same shape: an `Instant` plus a fixed `Duration`, checked with
/// hand-rolled `now.duration_since(last) >= PERIOD` arithmetic at each call
/// site. Bundling that pattern here means each site becomes one
/// `.is_due(now)` / `.mark(now)` pair instead of seven near-identical
/// copies of the same two-line check that could individually drift (e.g.
/// one site comparing with `>` instead of `>=`).
#[derive(Debug, Clone, Copy)]
struct Cadence {
    period: Duration,
    last: Instant,
}

impl Cadence {
    /// A cadence that fires the instant it's first polled -- back-dated by
    /// an hour (dwarfing every period used in this module) rather than
    /// requiring a real elapsed period, so the worker's very first refresh
    /// after startup populates every module immediately instead of leaving
    /// the bar blank until the first period elapses.
    fn due_now(period: Duration, now: Instant) -> Self {
        let last = now.checked_sub(Duration::from_hours(1)).unwrap_or(now);
        Self { period, last }
    }

    /// Whether at least `period` has elapsed since the last `mark`. Doesn't
    /// mutate -- callers only pay for the corresponding work (and call
    /// `mark`) when this is true, the same "check first, act second" shape
    /// every site already had.
    fn is_due(&self, now: Instant) -> bool {
        now.duration_since(self.last) >= self.period
    }

    /// Record that this cadence just fired, resetting its wait.
    const fn mark(&mut self, now: Instant) {
        self.last = now;
    }

    /// How long until this cadence is next due; zero once it already is.
    fn remaining(&self, now: Instant) -> Duration {
        self.period.saturating_sub(now.duration_since(self.last))
    }
}

/// What the render thread asks of the worker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BarCommand {
    Scroll { kind: ModuleKind, raw_px: i32 },
    Click { kind: ModuleKind, x0: i32, x1: i32 },
}

/// A fully-rendered set of module strings, sent to the render thread
/// whenever any of them changes. Deliberately just owned `String`s (not a
/// reference into worker state) since it crosses a thread boundary.
#[derive(Default, Clone)]
pub struct Snapshot {
    clock: String,
    date: String,
    battery: String,
    temp: String,
    #[allow(dead_code)] // populated but not drawn -- see `right_segments`
    cpu: String,
    #[allow(dead_code)]
    mem: String,
    wifi: String,
    bluetooth: String,
    vol_out: String,
    vol_in: String,
    brightness: String,
    /// Rendered `[[bar.notifications]]` segments, in config order --
    /// parallel to `State::notification_rules`, empty strings for
    /// not-currently-running apps skipped in `left_segments`.
    notifications: Vec<String>,
    /// Native StatusNotifier/AppIndicator items are supplied asynchronously
    /// by the dedicated D-Bus watcher, not recomputed by this polling worker.
    tray: Vec<TrayItem>,
}

impl Snapshot {
    /// Left section, left-to-right draw order: the always-visible
    /// connectivity cluster (clock/date live in `right_segments`).
    pub fn left_segments(&self) -> Vec<(ModuleKind, &str)> {
        vec![
            (ModuleKind::Wifi, self.wifi.as_str()),
            (ModuleKind::Bluetooth, self.bluetooth.as_str()),
        ]
    }

    /// Text-only app-urgency indicators retained for configurations that
    /// explicitly use them. Native system-tray items are painted between this
    /// connectivity cluster and these optional segments in `Painter`.
    pub fn notification_segments(&self) -> impl Iterator<Item = (ModuleKind, &str)> {
        self.notifications
            .iter()
            .map(|s| (ModuleKind::Notification, s.as_str()))
    }

    /// Native `StatusNotifier` items in registration order.
    pub fn tray_items(&self) -> &[TrayItem] {
        &self.tray
    }

    /// Replace the watcher-owned tray snapshot without involving the polling
    /// worker: this preserves prompt notification updates even if a sensor
    /// subprocess happens to be slow.
    pub fn replace_tray(&mut self, tray: Vec<TrayItem>) {
        self.tray = tray;
    }

    /// Center section -- unused for now (kept as an empty group so the
    /// three-section layout is exercised even with nothing docked there;
    /// a sway-IPC workspace/window-title module would land here). Takes no
    /// `&self`: it doesn't depend on any snapshot content yet, so it's an
    /// associated fn rather than a method that would trip `unused_self`.
    pub const fn center_segments() -> Vec<(ModuleKind, &'static str)> {
        Vec::new()
    }

    /// Right section, right-to-left draw order (rightmost module first):
    /// everything not claimed by `left_segments`.
    pub fn right_segments(&self) -> Vec<(ModuleKind, &str)> {
        vec![
            (ModuleKind::Date, self.date.as_str()),
            (ModuleKind::Clock, self.clock.as_str()),
            (ModuleKind::Battery, self.battery.as_str()),
            (ModuleKind::Temp, self.temp.as_str()),
            // (ModuleKind::Cpu, self.cpu.as_str()), // disabled per user request
            // (ModuleKind::Mem, self.mem.as_str()), // disabled per user request
            (ModuleKind::Brightness, self.brightness.as_str()),
            (ModuleKind::VolIn, self.vol_in.as_str()),
            (ModuleKind::VolOut, self.vol_out.as_str()),
        ]
    }
}

/// Last real (`pactl`/sysfs) reading for a scroll-adjustable control,
/// cached so a scroll detent can paint an *optimistic* delta and re-render
/// instantly instead of waiting on the next throttled real read-back --
/// see `State::apply_optimistic`.
#[derive(Clone, Copy, Default)]
struct KnownPct {
    pct: u32,
    muted: bool,
}

impl KnownPct {
    /// Nudge a cached percentage by a signed delta, clamped to `0..=100`.
    /// `pct` is always in `0..=100` (never near `i32`/`u32`'s extremes), so
    /// the sign-changing casts here can't actually wrap or lose
    /// information -- `cast_signed`/`cast_unsigned` say that plainly
    /// instead of leaving it to an `as` cast clippy has to warn about.
    fn nudge_pct(pct: u32, delta_pct: i32) -> u32 {
        (pct.cast_signed() + delta_pct)
            .clamp(0, 100)
            .cast_unsigned()
    }
}

struct State {
    snap: Snapshot,
    clock: Cadence,
    fast: Cadence,
    slow: Cadence,
    notifications: Cadence,
    #[allow(dead_code)]
    cpu: Cadence,
    /// `[[bar.notifications]]` rules read from config at startup -- static
    /// for the process lifetime (same posture as every other config-driven
    /// bar setting; no live config-reload support anywhere in this bar).
    notification_rules: Vec<config::NotificationRule>,
    control_refresh: Cadence,
    pending_scroll: Option<(ModuleKind, ScrollAccumulator)>,
    scroll_apply: Cadence,
    click_spawn: Cadence,
    vol_out_known: Option<KnownPct>,
    vol_in_known: Option<KnownPct>,
    brightness_known: Option<KnownPct>,
}

impl State {
    fn new(notification_rules: Vec<config::NotificationRule>) -> Self {
        let now = Instant::now();
        // Every cadence starts "already due" so the very first `refresh`
        // call (right after the bar maps) populates every module
        // immediately instead of waiting out a full period on a blank bar.
        Self {
            snap: Snapshot::default(),
            clock: Cadence::due_now(CLOCK_PERIOD, now),
            fast: Cadence::due_now(FAST_PERIOD, now),
            slow: Cadence::due_now(SLOW_PERIOD, now),
            notifications: Cadence::due_now(NOTIFICATIONS_PERIOD, now),
            cpu: Cadence::due_now(CPU_PERIOD, now),
            notification_rules,
            control_refresh: Cadence::due_now(
                Duration::from_millis(CONTROL_REFRESH_DEBOUNCE_MS),
                now,
            ),
            pending_scroll: None,
            scroll_apply: Cadence::due_now(Duration::from_millis(SCROLL_APPLY_DEBOUNCE_MS), now),
            click_spawn: Cadence::due_now(Duration::from_millis(CLICK_DEBOUNCE_MS), now),
            vol_out_known: None,
            vol_in_known: None,
            brightness_known: None,
        }
    }

    /// How long the worker may sleep before something is due: the soonest
    /// module refresh, or the scroll debounce while whole detents wait to be
    /// applied.
    fn next_wake(&self, now: Instant) -> Duration {
        let mut soonest = [&self.clock, &self.fast, &self.slow, &self.notifications]
            .into_iter()
            .map(|cadence| cadence.remaining(now))
            .min()
            .unwrap_or(CLOCK_PERIOD);
        if self.has_unapplied_steps() {
            soonest = soonest.min(self.scroll_apply.remaining(now));
        }
        soonest
    }

    /// Whether the pending scroll holds at least one whole detent that
    /// `apply_pending_scroll` has not yet turned into a mutation.
    fn has_unapplied_steps(&self) -> bool {
        self.pending_scroll.is_some_and(|(_, acc)| {
            let mut peek = acc;
            peek.take_steps() != 0
        })
    }

    /// Apply one command from the render thread; returns whether the
    /// snapshot changed.
    fn execute(&mut self, command: BarCommand, clicks: &config::BarClickConfig) -> bool {
        match command {
            BarCommand::Scroll { kind, raw_px } => self.queue_scroll(kind, raw_px),
            BarCommand::Click { kind, x0, x1 } => {
                self.handle_click(kind, x0, x1, clicks);
                false
            }
        }
    }

    /// Recompute whichever modules are due; returns whether anything changed.
    fn refresh(&mut self) -> bool {
        let now = Instant::now();
        let mut changed = false;

        if self.clock.is_due(now) {
            self.clock.mark(now);
            if let Some(c) = statusbar::Clock::time() {
                changed |= self.snap.clock != c;
                self.snap.clock = c;
            }
        }

        if self.fast.is_due(now) {
            self.fast.mark(now);
            let d = statusbar::Clock::date().unwrap_or_else(|| DASH.into());
            changed |= self.snap.date != d;
            self.snap.date = d;
            let b = statusbar::Battery::read().unwrap_or_else(|| DASH.into());
            changed |= self.snap.battery != b;
            self.snap.battery = b;
            let t = statusbar::Temperature::read().unwrap_or_else(|| DASH.into());
            changed |= self.snap.temp != t;
            self.snap.temp = t;
            let m = statusbar::Memory::read().unwrap_or_else(|| DASH.into());
            changed |= self.snap.mem != m;
            self.snap.mem = m;
        }

        // `Cpu` is commented out of `Snapshot::right_segments` (disabled
        // per user request) -- `cpu_usage()` blocks its caller for ~120ms
        // (two `/proc/stat` samples straddling a `thread::sleep`); even on
        // the worker thread (which no longer stalls Wayland event
        // processing) there's no point paying that for a number nothing
        // draws. Re-enable with a `self.cpu`/`CPU_PERIOD`-gated call here
        // alongside uncommenting the segment.

        if self.slow.is_due(now) {
            self.slow.mark(now);
            let w = statusbar::Wifi::read().unwrap_or_else(|| DASH.into());
            changed |= self.snap.wifi != w;
            self.snap.wifi = w;
            let bt = statusbar::Bluetooth::read().unwrap_or_else(|| DASH.into());
            changed |= self.snap.bluetooth != bt;
            self.snap.bluetooth = bt;
            if let Some((s, pct, muted)) = Volume::sink_raw() {
                changed |= self.snap.vol_out != s;
                self.snap.vol_out = s;
                self.vol_out_known = Some(KnownPct { pct, muted });
            } else {
                changed |= self.snap.vol_out != DASH;
                self.snap.vol_out = DASH.into();
                self.vol_out_known = None;
            }
            if let Some((s, pct, muted)) = Volume::source_raw() {
                changed |= self.snap.vol_in != s;
                self.snap.vol_in = s;
                self.vol_in_known = Some(KnownPct { pct, muted });
            } else {
                changed |= self.snap.vol_in != DASH;
                self.snap.vol_in = DASH.into();
                self.vol_in_known = None;
            }
            if let Some((s, pct)) = statusbar::Brightness::read_raw() {
                changed |= self.snap.brightness != s;
                self.snap.brightness = s;
                self.brightness_known = Some(KnownPct { pct, muted: false });
            } else {
                changed |= self.snap.brightness != DASH;
                self.snap.brightness = DASH.into();
                self.brightness_known = None;
            }
        }

        if self.notifications.is_due(now) {
            self.notifications.mark(now);
            let rendered: Vec<String> = statusbar::Notifications::query(&self.notification_rules)
                .unwrap_or_default()
                .into_iter()
                .map(|seg| seg.rendered)
                .collect();
            changed |= self.snap.notifications != rendered;
            self.snap.notifications = rendered;
        }

        changed
    }

    /// Immediately re-read just the one control `kind` was scrolled on
    /// (skipping `SLOW_PERIOD`'s cooldown), debounced to at most once per
    /// `CONTROL_REFRESH_DEBOUNCE_MS`. NOT called right after firing a
    /// scroll-triggered mutation any more -- `Volume::adjust_sink`/etc. spawn
    /// `pactl`/write sysfs and return immediately without waiting for the
    /// change to land (deliberately, to keep the worker non-blocking; see
    /// `Volume::adjust`'s doc), so a readback moments later would
    /// race it and often observe the *pre*-mutation value. `apply_optimistic`
    /// then keeps compounding its next delta on top of that stale reading,
    /// which is exactly the drift bug this once caused -- see
    /// `apply_pending_scroll`, which now advances `*_known` by the applied
    /// delta directly instead of racing a readback. This is kept only for
    /// external callers that don't have a just-applied delta to reason
    /// about (there are none currently -- reserved for future use, e.g. a
    /// keybind-triggered mute toggle). Returns whether the snapshot changed.
    #[allow(dead_code)]
    fn force_refresh_control(&mut self, kind: ModuleKind) -> bool {
        let now = Instant::now();
        if !self.control_refresh.is_due(now) {
            return false;
        }
        self.control_refresh.mark(now);
        match kind {
            ModuleKind::VolOut => {
                if let Some((s, pct, muted)) = Volume::sink_raw() {
                    let changed = self.snap.vol_out != s;
                    self.snap.vol_out = s;
                    self.vol_out_known = Some(KnownPct { pct, muted });
                    changed
                } else {
                    false
                }
            }
            ModuleKind::VolIn => {
                if let Some((s, pct, muted)) = Volume::source_raw() {
                    let changed = self.snap.vol_in != s;
                    self.snap.vol_in = s;
                    self.vol_in_known = Some(KnownPct { pct, muted });
                    changed
                } else {
                    false
                }
            }
            ModuleKind::Brightness => {
                if let Some((s, pct)) = statusbar::Brightness::read_raw() {
                    let changed = self.snap.brightness != s;
                    self.snap.brightness = s;
                    self.brightness_known = Some(KnownPct { pct, muted: false });
                    changed
                } else {
                    false
                }
            }
            _ => false,
        }
    }

    /// Advance the cached `*_known` reading for `kind` by `delta_pct`
    /// (clamped to `0..=100`) and repaint the snapshot from it -- called
    /// right after actually spawning the real `pactl`/sysfs mutation for
    /// that same delta, so the cache stays the ground truth for the next
    /// `apply_optimistic` call without needing a readback that would race
    /// the (fire-and-forget, non-blocking) mutation. Any drift this misses
    /// -- the mixer clamping oddly, another process changing it
    /// concurrently -- gets corrected by the next `SLOW_PERIOD` `refresh`
    /// (every 5s), same as before this optimistic path existed. Returns
    /// whether the snapshot changed.
    fn advance_known(&mut self, kind: ModuleKind, delta_pct: i32) -> bool {
        match kind {
            ModuleKind::VolOut => {
                let known = self.vol_out_known.get_or_insert_with(KnownPct::default);
                known.pct = KnownPct::nudge_pct(known.pct, delta_pct);
                let s = Volume::format(false, known.pct, known.muted);
                let changed = self.snap.vol_out != s;
                self.snap.vol_out = s;
                changed
            }
            ModuleKind::VolIn => {
                let known = self.vol_in_known.get_or_insert_with(KnownPct::default);
                known.pct = KnownPct::nudge_pct(known.pct, delta_pct);
                let s = Volume::format(true, known.pct, known.muted);
                let changed = self.snap.vol_in != s;
                self.snap.vol_in = s;
                changed
            }
            ModuleKind::Brightness => {
                let known = self.brightness_known.get_or_insert_with(KnownPct::default);
                known.pct = KnownPct::nudge_pct(known.pct, delta_pct);
                let s = statusbar::Brightness::format(known.pct);
                let changed = self.snap.brightness != s;
                self.snap.brightness = s;
                changed
            }
            _ => false,
        }
    }

    /// Queue a raw scroll delta AND immediately paint an optimistic
    /// percentage for it -- see `apply_optimistic`'s doc for why the two
    /// are split: this only touches in-memory state (accumulator math +
    /// string formatting, no I/O), so it can run on every scroll event with
    /// zero debounce, while the actual mixer/backlight mutation stays
    /// throttled in `apply_pending_scroll`. Returns whether the snapshot
    /// changed.
    fn queue_scroll(&mut self, kind: ModuleKind, raw_px: i32) -> bool {
        match &mut self.pending_scroll {
            Some((k, acc)) if *k == kind => acc.add(raw_px),
            _ => {
                let mut acc = ScrollAccumulator::new();
                acc.add(raw_px);
                self.pending_scroll = Some((kind, acc));
            }
        }
        self.apply_optimistic(kind)
    }

    /// Paint the *display* percentage for `kind` immediately from whatever
    /// whole detents are pending right now (without draining them --
    /// `apply_pending_scroll` still owns draining, since it's the one that
    /// actually fires the throttled `pactl`/sysfs mutation).
    ///
    /// This exists because before it, the number on screen only moved once
    /// every `SCROLL_APPLY_DEBOUNCE_MS` *and* only after a real
    /// `pactl`/sysfs round trip finished -- correct, but perceptibly behind
    /// the wheel next to waybar (whose module UI isn't coupled to a
    /// synchronous backend read on every scroll event either). We already
    /// know the *current* value from the last real read (`*_known`); a
    /// whole detent's percentage delta is always `+-SCROLL_STEP_PCT`
    /// regardless of what pactl/sysfs eventually settles on, so painting
    /// `known.pct + delta` the instant a detent completes -- without
    /// waiting for the real mutation to even be spawned -- closes that gap.
    /// `force_refresh_control`'s throttled real read-back still runs
    /// afterwards and corrects any drift (clamping at 0/100, another
    /// process changing the mixer concurrently, etc).
    fn apply_optimistic(&mut self, kind: ModuleKind) -> bool {
        let Some((_, acc)) = &self.pending_scroll else {
            return false;
        };
        // Peek the step count this delta would produce without consuming it
        // -- draining happens once, in `apply_pending_scroll`, so the real
        // mutation still only fires per whole detent exactly as before.
        let mut peek = *acc;
        let steps = peek.take_steps();
        if steps == 0 {
            return false;
        }
        let delta = ScrollAccumulator::steps_to_delta_pct(steps, SCROLL_STEP_PCT);
        match kind {
            ModuleKind::VolOut => {
                let Some(known) = self.vol_out_known else {
                    return false;
                };
                let pct = KnownPct::nudge_pct(known.pct, delta);
                let s = Volume::format(false, pct, known.muted);
                let changed = self.snap.vol_out != s;
                self.snap.vol_out = s;
                changed
            }
            ModuleKind::VolIn => {
                let Some(known) = self.vol_in_known else {
                    return false;
                };
                let pct = KnownPct::nudge_pct(known.pct, delta);
                let s = Volume::format(true, pct, known.muted);
                let changed = self.snap.vol_in != s;
                self.snap.vol_in = s;
                changed
            }
            ModuleKind::Brightness => {
                let Some(known) = self.brightness_known else {
                    return false;
                };
                let pct = KnownPct::nudge_pct(known.pct, delta);
                let s = statusbar::Brightness::format(pct);
                let changed = self.snap.brightness != s;
                self.snap.brightness = s;
                changed
            }
            _ => false,
        }
    }

    /// Convert whatever whole detents' worth of raw pixels have
    /// accumulated into one `pactl`/sysfs-brightness spawn (if any, and if
    /// the debounce window has elapsed), then re-reads just that control.
    /// Returns whether the snapshot changed.
    fn apply_pending_scroll(&mut self) -> bool {
        let Some((kind, mut acc)) = self.pending_scroll else {
            return false;
        };
        let now = Instant::now();
        if !self.scroll_apply.is_due(now) {
            return false;
        }
        self.scroll_apply.mark(now);
        let steps = acc.take_steps();
        self.pending_scroll = Some((kind, acc));
        if steps == 0 {
            return false;
        }
        let delta = ScrollAccumulator::steps_to_delta_pct(steps, SCROLL_STEP_PCT);
        match kind {
            ModuleKind::VolOut => Volume::adjust_sink(delta),
            ModuleKind::VolIn => Volume::adjust_source(delta),
            ModuleKind::Brightness => statusbar::Brightness::adjust(delta),
            _ => {}
        }
        // Commit the delta straight into `*_known` instead of racing a
        // readback against the fire-and-forget mutation above -- see
        // `advance_known`'s doc.
        self.advance_known(kind, delta)
    }

    /// Open the clicked module's panel (or COSMIC settings page, see
    /// `ModuleKind::settings_page`), debounced so a double-click doesn't
    /// race two instances for focus. Fire-and-forget, same posture as
    /// the volume/brightness mutations: spawn and immediately let go,
    /// never block the worker loop on it.
    fn handle_click(
        &mut self,
        kind: ModuleKind,
        x0: i32,
        x1: i32,
        clicks: &config::BarClickConfig,
    ) {
        let Some(action) = statusbar::click::ClickAction::for_module(kind, clicks, x0, x1) else {
            return;
        };
        let now = Instant::now();
        if !self.click_spawn.is_due(now) {
            return;
        }
        self.click_spawn.mark(now);
        let spawned = std::process::Command::new(&action.program)
            .args(&action.args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
        if spawned.is_err()
            && let Some((program, args)) = action.fallback
        {
            let _ = std::process::Command::new(program)
                .args(args)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn();
        }
    }
}

/// The background worker thread itself -- a marker type grouping
/// [`Self::spawn_thread`] (called from `main`) with [`Self::run`] (the
/// thread body), instead of leaving them as two free functions with no
/// shared owner.
pub struct Worker;

impl Worker {
    /// Spawn the worker thread (see the module doc for why module
    /// computation lives on its own thread) -- split out of `main` purely
    /// to keep that function under clippy's line-count lint; no behavior
    /// of its own beyond the `Builder::spawn` call itself.
    pub fn spawn_thread(
        snapshot_tx: Producer<Snapshot>,
        inbox: CommandInbox<BarCommand>,
        wake: waker::Waker,
        notification_rules: Vec<config::NotificationRule>,
        click_config: config::BarClickConfig,
    ) {
        std::thread::Builder::new()
            .name("statusbar-worker".into())
            .spawn(move || {
                Self::run(
                    &snapshot_tx,
                    inbox,
                    &wake,
                    notification_rules,
                    &click_config,
                );
            })
            .expect("spawn statusbar worker thread");
    }

    /// Run until the render thread goes away: refresh modules when their
    /// periods fall due, apply queued scroll and click commands, and send a
    /// fresh [`Snapshot`] to `snapshot_tx` (and signal `wake`) whenever
    /// anything changes. All the blocking work (process spawns, `/proc`
    /// reads) lives entirely on this thread -- see `main.rs`'s module doc.
    ///
    /// Between passes the thread sleeps in `poll(2)` on the command inbox,
    /// for exactly as long as the soonest module period or scroll debounce
    /// has left. A command wakes it at once; nothing else does.
    fn run(
        snapshot_tx: &Producer<Snapshot>,
        mut inbox: CommandInbox<BarCommand>,
        wake: &waker::Waker,
        notification_rules: Vec<config::NotificationRule>,
        click_config: &config::BarClickConfig,
    ) {
        let mut state = State::new(notification_rules);
        loop {
            let mut changed = state.refresh();
            let batch = inbox.take();
            for command in batch.commands {
                changed |= state.execute(command, click_config);
            }
            changed |= state.apply_pending_scroll();

            if changed {
                if snapshot_tx.push_block(state.snap.clone()).is_err() {
                    return;
                }
                wake.signal();
            }
            if batch.closed {
                return;
            }
            let due_in = state.next_wake(Instant::now());
            let _ = Poller::one(inbox.raw_fd()).poll(Self::poll_timeout_ms(due_in));
        }
    }

    /// `due_in` as a `poll(2)` timeout, rounded up so the thread never wakes
    /// a hair before the deadline and spins through a zero-length wait.
    fn poll_timeout_ms(due_in: Duration) -> i32 {
        let millis = due_in.as_nanos().div_ceil(1_000_000);
        i32::try_from(millis).unwrap_or(i32::MAX)
    }
}

#[cfg(test)]
mod tests {
    use super::{CLOCK_PERIOD, Cadence, SCROLL_APPLY_DEBOUNCE_MS, State, Worker};
    use statusbar::{ModuleKind, scroll::PX_PER_STEP};
    use std::time::{Duration, Instant};

    fn settled_state(now: Instant) -> State {
        let mut state = State::new(Vec::new());
        state.clock.mark(now);
        state.fast.mark(now);
        state.slow.mark(now);
        state.notifications.mark(now);
        state.scroll_apply.mark(now);
        state
    }

    #[test]
    fn a_fresh_cadence_is_due_with_nothing_left_to_wait() {
        let now = Instant::now();

        assert_eq!(
            Cadence::due_now(CLOCK_PERIOD, now).remaining(now),
            Duration::ZERO
        );
    }

    #[test]
    fn a_marked_cadence_waits_out_its_period() {
        let now = Instant::now();
        let mut cadence = Cadence::due_now(Duration::from_secs(5), now);
        cadence.mark(now);

        assert_eq!(cadence.remaining(now), Duration::from_secs(5));
        assert_eq!(
            cadence.remaining(now + Duration::from_secs(2)),
            Duration::from_secs(3)
        );
        assert_eq!(
            cadence.remaining(now + Duration::from_secs(9)),
            Duration::ZERO
        );
    }

    #[test]
    fn an_idle_worker_sleeps_until_the_clock_next_changes() {
        let now = Instant::now();
        let state = settled_state(now);

        assert_eq!(state.next_wake(now), CLOCK_PERIOD);
    }

    #[test]
    fn the_soonest_module_period_sets_the_sleep() {
        let now = Instant::now();
        let state = settled_state(now);

        assert_eq!(
            state.next_wake(now + Duration::from_millis(400)),
            Duration::from_millis(600)
        );
    }

    #[test]
    fn a_sub_detent_scroll_adds_no_deadline() {
        let now = Instant::now();
        let mut state = settled_state(now);
        state.queue_scroll(ModuleKind::VolOut, PX_PER_STEP - 1);

        assert_eq!(state.next_wake(now), CLOCK_PERIOD);
    }

    #[test]
    fn a_whole_detent_wakes_the_worker_at_the_scroll_debounce() {
        let now = Instant::now();
        let mut state = settled_state(now);
        state.queue_scroll(ModuleKind::VolOut, PX_PER_STEP);

        assert_eq!(
            state.next_wake(now),
            Duration::from_millis(SCROLL_APPLY_DEBOUNCE_MS)
        );
    }

    #[test]
    fn poll_timeouts_round_up_to_a_whole_millisecond() {
        assert_eq!(Worker::poll_timeout_ms(Duration::ZERO), 0);
        assert_eq!(Worker::poll_timeout_ms(Duration::from_micros(1)), 1);
        assert_eq!(Worker::poll_timeout_ms(Duration::from_millis(60)), 60);
        assert_eq!(Worker::poll_timeout_ms(Duration::from_micros(59_001)), 60);
    }
}
