//! Background worker: owns every module computation and scroll-triggered
//! volume/brightness adjustment, on its own thread away from the Wayland
//! render/event loop -- see `main.rs`'s module doc for why.

use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::time::{Duration, Instant};

use statusbar::{ModuleKind, ScrollAccumulator, settings_page, steps_to_delta_pct};

/// How often each module recomputes. Cheap `/proc`/`/sys` reads run often;
/// modules that shell out to a CLI (`iw`, `pactl`, `bluetoothctl`) run less
/// often to keep from spawning processes constantly.
const CLOCK_PERIOD: Duration = Duration::from_millis(1000);
const FAST_PERIOD: Duration = Duration::from_secs(2); // battery, temp, mem
const SLOW_PERIOD: Duration = Duration::from_secs(5); // wifi, bluetooth, volume, brightness
/// `cpu_usage` samples twice ~120ms apart, so give it its own slightly
/// longer period rather than sharing `FAST_PERIOD`. Currently unused --
/// see the comment where it would be called in `State::refresh`.
#[allow(dead_code)]
const CPU_PERIOD: Duration = Duration::from_millis(2000);

/// Percentage points nudged per wheel detent when scroll-adjusting
/// vol-out/vol-in/brightness.
const SCROLL_STEP_PCT: i32 = 5;
/// Minimum spacing between `force_refresh_control` calls, in milliseconds.
const CONTROL_REFRESH_DEBOUNCE_MS: u64 = 150;
/// Minimum spacing between actually spawning a `pactl`/sysfs-brightness
/// *adjustment* while scrolling.
const SCROLL_APPLY_DEBOUNCE_MS: u64 = 60;
/// Idle poll granularity when nothing's due and no scroll is queued --
/// matches the render thread's own idle cadence.
const IDLE_POLL_MS: u64 = 100;
/// Minimum spacing between spawning `cosmic-settings` for a click, so a
/// double-click (or two clicks landing in the same channel-drain batch)
/// only opens one window instead of two racing to grab focus.
const CLICK_DEBOUNCE_MS: u64 = 400;

const DASH: &str = "—";

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

    /// Center section -- unused for now (kept as an empty group so the
    /// three-section layout is exercised even with nothing docked there;
    /// a sway-IPC workspace/window-title module would land here).
    #[allow(clippy::unused_self, clippy::missing_const_for_fn)]
    pub fn center_segments(&self) -> Vec<(ModuleKind, &str)> {
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
            (ModuleKind::VolOut, self.vol_out.as_str()),
            (ModuleKind::VolIn, self.vol_in.as_str()),
            (ModuleKind::Brightness, self.brightness.as_str()),
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

struct State {
    snap: Snapshot,
    last_clock: Instant,
    last_fast: Instant,
    last_slow: Instant,
    #[allow(dead_code)]
    last_cpu: Instant,
    last_control_refresh: Instant,
    pending_scroll: Option<(ModuleKind, ScrollAccumulator)>,
    last_scroll_apply: Instant,
    last_click_spawn: Instant,
    vol_out_known: Option<KnownPct>,
    vol_in_known: Option<KnownPct>,
    brightness_known: Option<KnownPct>,
}

impl State {
    fn new() -> Self {
        let now = Instant::now();
        // Back-date every timer so the very first `refresh` call (right
        // after the bar maps) populates every module immediately instead of
        // waiting out a full period on a blank bar.
        let stale = now - Duration::from_secs(3600);
        Self {
            snap: Snapshot::default(),
            last_clock: stale,
            last_fast: stale,
            last_slow: stale,
            last_cpu: stale,
            last_control_refresh: stale,
            pending_scroll: None,
            last_scroll_apply: stale,
            last_click_spawn: stale,
            vol_out_known: None,
            vol_in_known: None,
            brightness_known: None,
        }
    }

    /// Recompute whichever modules are due; returns whether anything changed.
    fn refresh(&mut self) -> bool {
        let now = Instant::now();
        let mut changed = false;

        if now.duration_since(self.last_clock) >= CLOCK_PERIOD {
            self.last_clock = now;
            if let Some(c) = statusbar::modules::clock() {
                changed |= self.snap.clock != c;
                self.snap.clock = c;
            }
        }

        if now.duration_since(self.last_fast) >= FAST_PERIOD {
            self.last_fast = now;
            let d = statusbar::modules::date().unwrap_or_else(|| DASH.into());
            changed |= self.snap.date != d;
            self.snap.date = d;
            let b = statusbar::modules::battery().unwrap_or_else(|| DASH.into());
            changed |= self.snap.battery != b;
            self.snap.battery = b;
            let t = statusbar::modules::temperature().unwrap_or_else(|| DASH.into());
            changed |= self.snap.temp != t;
            self.snap.temp = t;
            let m = statusbar::modules::memory().unwrap_or_else(|| DASH.into());
            changed |= self.snap.mem != m;
            self.snap.mem = m;
        }

        // `Cpu` is commented out of `Snapshot::right_segments` (disabled
        // per user request) -- `cpu_usage()` blocks its caller for ~120ms
        // (two `/proc/stat` samples straddling a `thread::sleep`); even on
        // the worker thread (which no longer stalls Wayland event
        // processing) there's no point paying that for a number nothing
        // draws. Re-enable with a `last_cpu`/`CPU_PERIOD`-gated call here
        // alongside uncommenting the segment.

        if now.duration_since(self.last_slow) >= SLOW_PERIOD {
            self.last_slow = now;
            let w = statusbar::modules::wifi().unwrap_or_else(|| DASH.into());
            changed |= self.snap.wifi != w;
            self.snap.wifi = w;
            let bt = statusbar::modules::bluetooth().unwrap_or_else(|| DASH.into());
            changed |= self.snap.bluetooth != bt;
            self.snap.bluetooth = bt;
            if let Some((s, pct, muted)) = statusbar::modules::volume_out_raw() {
                changed |= self.snap.vol_out != s;
                self.snap.vol_out = s;
                self.vol_out_known = Some(KnownPct { pct, muted });
            } else {
                changed |= self.snap.vol_out != DASH;
                self.snap.vol_out = DASH.into();
                self.vol_out_known = None;
            }
            if let Some((s, pct, muted)) = statusbar::modules::volume_in_raw() {
                changed |= self.snap.vol_in != s;
                self.snap.vol_in = s;
                self.vol_in_known = Some(KnownPct { pct, muted });
            } else {
                changed |= self.snap.vol_in != DASH;
                self.snap.vol_in = DASH.into();
                self.vol_in_known = None;
            }
            if let Some((s, pct)) = statusbar::modules::brightness_raw() {
                changed |= self.snap.brightness != s;
                self.snap.brightness = s;
                self.brightness_known = Some(KnownPct { pct, muted: false });
            } else {
                changed |= self.snap.brightness != DASH;
                self.snap.brightness = DASH.into();
                self.brightness_known = None;
            }
        }

        changed
    }

    /// Immediately re-read just the one control `kind` was scrolled on
    /// (skipping `SLOW_PERIOD`'s cooldown), debounced to at most once per
    /// `CONTROL_REFRESH_DEBOUNCE_MS`. NOT called right after firing a
    /// scroll-triggered mutation any more -- `adjust_volume_out`/etc. spawn
    /// `pactl`/write sysfs and return immediately without waiting for the
    /// change to land (deliberately, to keep the worker non-blocking; see
    /// `modules::adjust_volume`'s doc), so a readback moments later would
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
        if now.duration_since(self.last_control_refresh)
            < Duration::from_millis(CONTROL_REFRESH_DEBOUNCE_MS)
        {
            return false;
        }
        self.last_control_refresh = now;
        match kind {
            ModuleKind::VolOut => {
                if let Some((s, pct, muted)) = statusbar::modules::volume_out_raw() {
                    let changed = self.snap.vol_out != s;
                    self.snap.vol_out = s;
                    self.vol_out_known = Some(KnownPct { pct, muted });
                    changed
                } else {
                    false
                }
            }
            ModuleKind::VolIn => {
                if let Some((s, pct, muted)) = statusbar::modules::volume_in_raw() {
                    let changed = self.snap.vol_in != s;
                    self.snap.vol_in = s;
                    self.vol_in_known = Some(KnownPct { pct, muted });
                    changed
                } else {
                    false
                }
            }
            ModuleKind::Brightness => {
                if let Some((s, pct)) = statusbar::modules::brightness_raw() {
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
                known.pct = (known.pct as i32 + delta_pct).clamp(0, 100) as u32;
                let s = statusbar::modules::format_volume(false, known.pct, known.muted);
                let changed = self.snap.vol_out != s;
                self.snap.vol_out = s;
                changed
            }
            ModuleKind::VolIn => {
                let known = self.vol_in_known.get_or_insert_with(KnownPct::default);
                known.pct = (known.pct as i32 + delta_pct).clamp(0, 100) as u32;
                let s = statusbar::modules::format_volume(true, known.pct, known.muted);
                let changed = self.snap.vol_in != s;
                self.snap.vol_in = s;
                changed
            }
            ModuleKind::Brightness => {
                let known = self.brightness_known.get_or_insert_with(KnownPct::default);
                known.pct = (known.pct as i32 + delta_pct).clamp(0, 100) as u32;
                let s = statusbar::modules::format_brightness(known.pct);
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
        let delta = steps_to_delta_pct(steps, SCROLL_STEP_PCT);
        match kind {
            ModuleKind::VolOut => {
                let Some(known) = self.vol_out_known else {
                    return false;
                };
                let pct = (known.pct as i32 + delta).clamp(0, 100) as u32;
                let s = statusbar::modules::format_volume(false, pct, known.muted);
                let changed = self.snap.vol_out != s;
                self.snap.vol_out = s;
                changed
            }
            ModuleKind::VolIn => {
                let Some(known) = self.vol_in_known else {
                    return false;
                };
                let pct = (known.pct as i32 + delta).clamp(0, 100) as u32;
                let s = statusbar::modules::format_volume(true, pct, known.muted);
                let changed = self.snap.vol_in != s;
                self.snap.vol_in = s;
                changed
            }
            ModuleKind::Brightness => {
                let Some(known) = self.brightness_known else {
                    return false;
                };
                let pct = (known.pct as i32 + delta).clamp(0, 100) as u32;
                let s = statusbar::modules::format_brightness(pct);
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
        if now.duration_since(self.last_scroll_apply)
            < Duration::from_millis(SCROLL_APPLY_DEBOUNCE_MS)
        {
            return false;
        }
        self.last_scroll_apply = now;
        let steps = acc.take_steps();
        self.pending_scroll = Some((kind, acc));
        if steps == 0 {
            return false;
        }
        let delta = steps_to_delta_pct(steps, SCROLL_STEP_PCT);
        match kind {
            ModuleKind::VolOut => statusbar::modules::adjust_volume_out(delta),
            ModuleKind::VolIn => statusbar::modules::adjust_volume_in(delta),
            ModuleKind::Brightness => statusbar::modules::adjust_brightness(delta),
            _ => {}
        }
        // Commit the delta straight into `*_known` instead of racing a
        // readback against the fire-and-forget mutation above -- see
        // `advance_known`'s doc.
        self.advance_known(kind, delta)
    }

    /// Open the COSMIC settings page for `kind`, if it has one (see
    /// `statusbar::click::settings_page` -- mirrors the user's own
    /// `~/.config/waybar/config` `on-click` bindings), debounced so a
    /// double-click doesn't race two `cosmic-settings` instances for
    /// focus. Fire-and-forget, same posture as the volume/brightness
    /// mutations: spawn and immediately let go, never block the worker
    /// loop on it.
    fn handle_click(&mut self, kind: ModuleKind) {
        let Some(page) = settings_page(kind) else {
            return;
        };
        let now = Instant::now();
        if now.duration_since(self.last_click_spawn) < Duration::from_millis(CLICK_DEBOUNCE_MS) {
            return;
        }
        self.last_click_spawn = now;
        // Prefer the user's own `cosmic-popup` wrapper (auto-closes the
        // settings window on focus loss, exactly like waybar's binding
        // does) when it's on PATH; fall back to bare `cosmic-settings`
        // otherwise so this still works without that convenience script.
        let spawned = std::process::Command::new("cosmic-popup")
            .arg(page)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
        if spawned.is_err() {
            let _ = std::process::Command::new("cosmic-settings")
                .arg(page)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn();
        }
    }
}

/// Run forever: refresh modules on their timers, drain queued scroll deltas
/// and apply/read-back volume-brightness adjustments, sending a fresh
/// [`Snapshot`] to `snapshot_tx` (and signalling `wake`) whenever anything
/// changes. All the blocking work (process spawns, `/proc` reads, the
/// deliberate ~120ms `cpu_usage` sample window) lives entirely on this
/// thread -- see `main.rs`'s module doc.
pub fn run(
    snapshot_tx: &Sender<Snapshot>,
    scroll_rx: &Receiver<(ModuleKind, i32)>,
    click_rx: &Receiver<ModuleKind>,
    wake: &waker::Waker,
) {
    let mut state = State::new();
    loop {
        let mut changed = state.refresh();

        while let Ok(kind) = click_rx.try_recv() {
            state.handle_click(kind);
        }

        // Block on the scroll channel itself (not a plain sleep) so a
        // wheel event is picked up the instant it arrives instead of
        // waiting out an idle poll interval -- `recv_timeout` still
        // returns on its own after `wait_ms` so module timers keep firing
        // when nothing's scrolling. Drain every already-queued delta once
        // one arrives (each one repaints the optimistic percentage as it
        // goes -- see `queue_scroll`), same coalescing behavior as before
        // for the real mutation.
        let wait_ms = if state.pending_scroll.is_some() {
            SCROLL_APPLY_DEBOUNCE_MS
        } else {
            IDLE_POLL_MS
        };
        match scroll_rx.recv_timeout(Duration::from_millis(wait_ms)) {
            Ok((kind, raw_px)) => {
                changed |= state.queue_scroll(kind, raw_px);
                loop {
                    match scroll_rx.try_recv() {
                        Ok((kind, raw_px)) => changed |= state.queue_scroll(kind, raw_px),
                        Err(TryRecvError::Empty) => break,
                        Err(TryRecvError::Disconnected) => return,
                    }
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => return,
        }
        changed |= state.apply_pending_scroll();

        if changed {
            let _ = snapshot_tx.send(state.snap.clone());
            wake.signal();
        }
    }
}
