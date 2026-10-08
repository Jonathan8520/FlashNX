//! OVERCLOCK: AUTO. The CPU goes to 1785 MHz only while a game falls behind its
//! own frame rate AND the raise measurably helps it, and goes back to the stock
//! clock once the game would keep up without it.
//!
//! Why a third position. ON holds 1785 for the whole session, including games
//! that already run at full speed at 1020, where the cores spend the spare time
//! idling at the higher voltage that frequency needs, and games whose wall is
//! not the CPU (Dragon City gained +15 % from it, at 0.2x of its speed,
//! 2026-08-26). OFF leaves x1.41 on average on the table for the games that do
//! need it (paired measurement, 10 games, from x1.07 to x1.75).
//!
//! Nothing here touches a clock. Every change goes through `apply_power_mode`,
//! so the battery veto, the focus hook, the periodic re-assert and the restore
//! on every exit path hold exactly as they do for ON.
//!
//! The control law is the one published for frame-deadline DVFS: raise on a
//! short run of missed deadlines, lower on a long run of slack. No homebrew on
//! this console does it (sys-clk and RetroArch hold fixed rates), so every
//! threshold below is a measurement made in the running game, never a guess
//! about it:
//! - UP: the movie ran under 95 % of its nominal speed for two half-second
//!   windows in a row.
//! - KEEP or REJECT: one window after the raise is skipped (the change itself),
//!   the next two must show the speed at 97 % or more, or 15 % above what it
//!   was. Otherwise the raise bought nothing: back down, and the next try waits
//!   30 s, doubling to 4 min.
//! - DOWN: a TRY, not a prediction. After 3 s at full speed, and unless the
//!   work of one SWF frame scaled as if all of it were CPU (x1.75) is far over
//!   the frame's budget, the clock goes back down and the game shows whether it
//!   keeps up. If it falls behind within seconds, it goes back up and the next
//!   try waits twice as long (3 s, 6, 12... up to 60). A first version
//!   PREDICTED instead, against the display slot of a frame, and never came
//!   down: on Five Minutes to Kill it predicted 47 ms at stock for a scene that
//!   held 1.00x at 1020 (2026-10-08). The host loop renders less when frames
//!   get longer and not all of the work scales with the clock, so the model
//!   only ever erred one way.
//!
//! Plus one guard ON does not have: no raise at a skin temperature of 56 C or
//! more, until it is back under 52. Horizon's answer to heat in handheld is
//! sleep (timer at 58 C), not throttling, and the plateau measured with ON held
//! is 44 C, so this is not expected to fire; it is there so AUTO can never be
//! the reason a console goes to sleep mid-game.

use std::sync::Mutex;

extern "C" {
    fn ruffle_tick_now() -> u64;
    fn ruffle_tick_freq() -> u64;
    /// Skin temperature in milli-C, 0 when unavailable (ruffle_bridge.cpp).
    fn flashnx_skin_temp_mc() -> i32;
    /// 1 while we are the foreground app.
    fn flashnx_has_focus() -> core::ffi::c_int;
    /// The mode the clock code WANTS (not the one applied): drops to 0 under
    /// us only when the battery veto in `flashnx_clocks_reassert` fires.
    fn flashnx_clock_mode_wanted() -> core::ffi::c_int;
}

/// Under this fraction of its nominal speed, a window counts as behind.
const UP_SPEED: f32 = 0.95;
/// Behind windows in a row before a raise (one second).
const UP_WINDOWS: u32 = 2;
/// Windows thrown away right after a raise: the one the change happened in.
const PROBE_SKIP: u32 = 1;
/// Windows measured after that to judge the raise.
const PROBE_WINDOWS: u32 = 2;
/// A raise is kept if the game reaches this speed...
const KEEP_SPEED: f32 = 0.97;
/// ...or gains at least this much over the speed it had before.
const KEEP_GAIN: f32 = 1.15;
/// 1785 / 1020: the stock clock's cost for work that is all CPU.
const STOCK_SCALE: f32 = 1.75;
/// No try at stock while the work so scaled is over this many budgets: an
/// overestimate (see the module comment), so only a sanity gate.
const TRY_GATE: f32 = 1.25;
/// Quiet windows in a row before going down (3 s), doubled after each
/// DOWN that a new UP follows within `OSC_S`, up to `QUIET_MAX`.
const DOWN_WINDOWS: u32 = 6;
const QUIET_MAX: u32 = 120;
const OSC_S: u64 = 10;
/// Time at stock after which the doubled requirement is forgotten.
const RELAX_S: u64 = 60;
/// Least time spent raised before a DOWN.
const MIN_HIGH_S: u64 = 3;
/// A raise kept for a gain under this is checked again every `RECHECK_S`
/// while the game is still behind: a kept raise has no other way to find out
/// that the gain it was kept for came from a scene change.
const RECHECK_GAIN: f32 = 1.3;
const RECHECK_S: u64 = 120;
/// Wait after a raise that bought nothing, doubled each time, reset by a KEEP.
const RETRY_S: u64 = 30;
const RETRY_MAX_S: u64 = 240;
/// Wait after a raise the console refused (battery out of its Normal state).
const REFUSED_RETRY_S: u64 = 60;
/// Skin temperature guard, milli-C, read every `TEMP_EVERY_S`.
const HOT_MC: i32 = 56_000;
const COOL_MC: i32 = 52_000;
const TEMP_EVERY_S: u64 = 5;
/// A `clocks: auto state` line every this many windows (5 s): what the
/// decisions were made on, so the thresholds can be checked against a log.
const STATE_EVERY: u32 = 10;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// The setting is not AUTO.
    Off,
    /// Stock clock, watching for the game to fall behind.
    Low,
    /// Just raised, measuring whether it helps.
    Probe,
    /// Raised and kept, watching for the game to need it no more.
    High,
}

/// One measuring window. Opened and closed on ticks that advanced the movie,
/// like the FPS counter's (`fps_sample` in render.rs), so it spans whole frame
/// intervals.
#[derive(Clone, Copy)]
struct Window {
    start: u64,
    acc_start: u64,
    frames: u32,
    tick: u64,
    render: u64,
    renders: u32,
}

impl Window {
    const EMPTY: Window = Window { start: 0, acc_start: 0, frames: 0, tick: 0, render: 0, renders: 0 };
}

struct Gov {
    phase: Phase,
    win: Window,
    /// Tick the current phase began on.
    since: u64,
    behind: u32,
    /// Sums over the windows of the current run (behind, or probe).
    speed_sum: f32,
    work_sum: f32,
    /// Speed and work before the raise, kept for the probe's verdict.
    base_speed: f32,
    base_work: f32,
    probe_n: u32,
    quiet: u32,
    quiet_needed: u32,
    last_down: u64,
    /// Gain measured by the last KEEP.
    gain: f32,
    retry_at: u64,
    backoff_s: u64,
    hot: bool,
    temp_at: u64,
    // Per game, for the summary line and the bug report.
    auto_since: u64,
    auto_ticks: u64,
    high_since: u64,
    high_ticks: u64,
    ups: u32,
    keeps: u32,
    rejects: u32,
    downs: u32,
    refusals: u32,
    /// The summary line has been written for this game.
    summarized: bool,
    /// Windows judged, for the periodic state line.
    windows: u32,
}

static GOV: Mutex<Gov> = Mutex::new(Gov {
    phase: Phase::Off,
    win: Window::EMPTY,
    since: 0,
    behind: 0,
    speed_sum: 0.0,
    work_sum: 0.0,
    base_speed: 0.0,
    base_work: 0.0,
    probe_n: 0,
    quiet: 0,
    quiet_needed: DOWN_WINDOWS,
    last_down: 0,
    gain: 0.0,
    retry_at: 0,
    backoff_s: RETRY_S,
    hot: false,
    temp_at: 0,
    auto_since: 0,
    auto_ticks: 0,
    high_since: 0,
    high_ticks: 0,
    ups: 0,
    keeps: 0,
    rejects: 0,
    downs: 0,
    refusals: 0,
    summarized: false,
    windows: 0,
});

fn now() -> u64 {
    unsafe { ruffle_tick_now() }
}

fn freq() -> u64 {
    unsafe { ruffle_tick_freq() }.max(1)
}

fn log(s: &str) {
    crate::log_str(s);
}

impl Gov {
    fn raised(&self) -> bool {
        matches!(self.phase, Phase::Probe | Phase::High)
    }

    fn enter(&mut self, phase: Phase, now: u64) {
        self.phase = phase;
        self.since = now;
        self.behind = 0;
        self.speed_sum = 0.0;
        self.work_sum = 0.0;
        self.probe_n = 0;
        self.quiet = 0;
    }

    /// Ask for 1785. False when not granted: not the foreground app (silent,
    /// nothing to wait for), or refused by the clock code (battery, clkrst).
    fn raise(&mut self, now: u64, why: &str) -> bool {
        if unsafe { flashnx_has_focus() } == 0 {
            return false;
        }
        let t = self::now();
        let got = crate::backend::render::apply_power_mode(1);
        let took_us = self::now().saturating_sub(t) * 1_000_000 / freq();
        if got != 1 {
            self.refusals += 1;
            self.retry_at = now + REFUSED_RETRY_S * freq();
            log(&std::format!(
                "clocks: auto UP refused ({}), next try in {} s\n",
                why, REFUSED_RETRY_S,
            ));
            return false;
        }
        self.ups += 1;
        self.high_since = now.max(1);
        log(&std::format!("clocks: auto UP {} took={}us\n", why, took_us));
        true
    }

    /// Back to the stock clock, and to `Low`.
    fn lower(&mut self, now: u64) {
        crate::backend::render::apply_power_mode(0);
        self.close_high(now);
        self.enter(Phase::Low, now);
    }

    fn close_high(&mut self, now: u64) {
        if self.high_since != 0 {
            self.high_ticks += now.saturating_sub(self.high_since);
            self.high_since = 0;
        }
    }

    fn close_auto(&mut self, now: u64) {
        self.close_high(now);
        if self.auto_since != 0 {
            self.auto_ticks += now.saturating_sub(self.auto_since);
            self.auto_since = 0;
        }
    }

    /// The clock code dropped the raise without us: the battery veto.
    fn lost(&mut self, now: u64) {
        self.close_high(now);
        self.enter(Phase::Low, now);
        self.retry_at = now + REFUSED_RETRY_S * freq();
        log(&std::format!(
            "clocks: auto raise dropped by the battery veto, next try in {} s\n",
            REFUSED_RETRY_S,
        ));
    }

    fn decide(&mut self, now: u64, speed: f32, work_us: f32, budget_us: f32) {
        let f = freq();
        self.windows += 1;
        if self.windows % STATE_EVERY == 0 {
            let wait_s = self.retry_at.saturating_sub(now) / f;
            log(&std::format!(
                "clocks: auto state={} speed={:.2} work={:.1}ms budget={:.1}ms quiet={}/{} wait={}s\n",
                match self.phase {
                    Phase::Low => "LOW",
                    Phase::Probe => "PROBE",
                    Phase::High => "HIGH",
                    Phase::Off => "OFF",
                },
                speed, work_us / 1000.0, budget_us / 1000.0,
                self.quiet, self.quiet_needed, wait_s,
            ));
        }
        if now >= self.temp_at {
            self.temp_at = now + TEMP_EVERY_S * f;
            let mc = unsafe { flashnx_skin_temp_mc() };
            if mc > 0 && !self.hot && mc >= HOT_MC {
                self.hot = true;
                log(&std::format!(
                    "clocks: auto skin {}.{} C, no raise until under {} C\n",
                    mc / 1000, (mc % 1000) / 100, COOL_MC / 1000,
                ));
                if self.raised() {
                    self.lower(now);
                }
                return;
            }
            if mc > 0 && self.hot && mc < COOL_MC {
                self.hot = false;
                log("clocks: auto skin back under the guard\n");
            }
        }
        match self.phase {
            Phase::Off => {}
            Phase::Low => {
                if now.saturating_sub(self.since) > RELAX_S * f {
                    self.quiet_needed = DOWN_WINDOWS;
                }
                if speed >= UP_SPEED || self.hot || now < self.retry_at {
                    self.behind = 0;
                    self.speed_sum = 0.0;
                    self.work_sum = 0.0;
                    return;
                }
                self.behind += 1;
                self.speed_sum += speed;
                self.work_sum += work_us;
                if self.behind < UP_WINDOWS {
                    return;
                }
                let base_speed = self.speed_sum / self.behind as f32;
                let base_work = self.work_sum / self.behind as f32;
                self.behind = 0;
                self.speed_sum = 0.0;
                self.work_sum = 0.0;
                // Back up within seconds of coming down: the DOWN was early.
                // Ask for twice the quiet before the next one.
                if self.last_down != 0 && now.saturating_sub(self.last_down) < OSC_S * f {
                    self.quiet_needed = (self.quiet_needed * 2).min(QUIET_MAX);
                }
                let why = std::format!(
                    "speed={:.2} work={:.1}ms budget={:.1}ms",
                    base_speed, base_work / 1000.0, budget_us / 1000.0,
                );
                if self.raise(now, &why) {
                    self.enter(Phase::Probe, now);
                    self.base_speed = base_speed;
                    self.base_work = base_work;
                }
            }
            Phase::Probe => {
                if unsafe { flashnx_clock_mode_wanted() } == 0 {
                    self.lost(now);
                    return;
                }
                self.probe_n += 1;
                if self.probe_n <= PROBE_SKIP {
                    return;
                }
                self.speed_sum += speed;
                self.work_sum += work_us;
                if self.probe_n < PROBE_SKIP + PROBE_WINDOWS {
                    return;
                }
                let hi_speed = self.speed_sum / PROBE_WINDOWS as f32;
                let hi_work = self.work_sum / PROBE_WINDOWS as f32;
                let gain = hi_speed / self.base_speed.max(0.01);
                let verdict = std::format!(
                    "x{:.2} (speed {:.2} -> {:.2}, work {:.1} -> {:.1} ms)",
                    gain, self.base_speed, hi_speed,
                    self.base_work / 1000.0, hi_work / 1000.0,
                );
                if hi_speed >= KEEP_SPEED || gain >= KEEP_GAIN {
                    self.keeps += 1;
                    self.gain = gain;
                    self.backoff_s = RETRY_S;
                    self.enter(Phase::High, now);
                    log(&std::format!("clocks: auto KEEP {}\n", verdict));
                } else {
                    self.rejects += 1;
                    self.lower(now);
                    self.retry_at = now + self.backoff_s * f;
                    log(&std::format!(
                        "clocks: auto REJECT {}, next try in {} s\n",
                        verdict, self.backoff_s,
                    ));
                    self.backoff_s = (self.backoff_s * 2).min(RETRY_MAX_S);
                }
            }
            Phase::High => {
                if unsafe { flashnx_clock_mode_wanted() } == 0 {
                    self.lost(now);
                    return;
                }
                let stock_us = work_us * STOCK_SCALE;
                if speed >= KEEP_SPEED && stock_us <= TRY_GATE * budget_us {
                    self.quiet += 1;
                } else {
                    self.quiet = 0;
                }
                let held = now.saturating_sub(self.since);
                if self.quiet >= self.quiet_needed && held >= MIN_HIGH_S * f {
                    // A try: `Low` raises again within a second if the game
                    // falls behind, and that UP doubles `quiet_needed`.
                    let held_s = held / f;
                    self.downs += 1;
                    self.last_down = now;
                    self.lower(now);
                    log(&std::format!(
                        "clocks: auto DOWN speed={:.2} work={:.1}ms budget={:.1}ms, after {} s\n",
                        speed, work_us / 1000.0, budget_us / 1000.0, held_s,
                    ));
                    return;
                }
                if self.gain < RECHECK_GAIN && held >= RECHECK_S * f && speed < KEEP_SPEED {
                    // No backoff: still behind at stock, `Low` raises again
                    // within a second and the probe measures the gain afresh.
                    let gain = self.gain;
                    self.lower(now);
                    log(&std::format!(
                        "clocks: auto RECHECK kept for x{:.2}, measuring again\n",
                        gain,
                    ));
                }
            }
        }
    }
}

/// Apply a power setting (keymap::POWER_*) for the running game. Returns what
/// to persist: AUTO itself, or for ON/OFF the mode the hardware accepted.
pub fn apply_setting(setting: u8) -> u8 {
    let now = now();
    let Ok(mut g) = GOV.lock() else {
        // AUTO with no governor is OFF, never ON (`apply_power_mode` raises
        // for 1 only).
        return crate::backend::render::apply_power_mode(setting);
    };
    if setting == crate::keymap::POWER_AUTO {
        if g.phase == Phase::Off {
            // Start at the stock clock, whatever ON left: the first second
            // of play says whether the game needs more.
            crate::backend::render::apply_power_mode(0);
            g.close_high(now);
            g.enter(Phase::Low, now);
            g.win = Window::EMPTY;
            g.auto_since = now.max(1);
        }
        return crate::keymap::POWER_AUTO;
    }
    g.close_auto(now);
    g.phase = Phase::Off;
    drop(g);
    crate::backend::render::apply_power_mode(setting)
}

/// True while the running game is on AUTO.
pub fn is_auto() -> bool {
    GOV.lock().map(|g| g.phase != Phase::Off).unwrap_or(false)
}

/// What the OVERCLOCK row shows: AUTO, or the mode in force.
pub fn shown_mode() -> u8 {
    if is_auto() {
        crate::keymap::POWER_AUTO
    } else {
        crate::backend::render::current_power_mode()
    }
}

/// A new game (or a restart): its own statistics, and a clean slate.
pub fn begin_game() {
    // A restart builds a new Player without going through `ruffle_shutdown`.
    end_game();
    let Ok(mut g) = GOV.lock() else { return };
    g.summarized = false;
    g.win = Window::EMPTY;
    g.quiet_needed = DOWN_WINDOWS;
    g.last_down = 0;
    g.gain = 0.0;
    g.retry_at = 0;
    g.backoff_s = RETRY_S;
    g.hot = false;
    g.temp_at = 0;
    g.auto_ticks = 0;
    g.high_ticks = 0;
    g.ups = 0;
    g.keeps = 0;
    g.rejects = 0;
    g.downs = 0;
    g.refusals = 0;
}

/// The game is gone (the clock itself is restored by main.cpp right after).
/// Closes the accounts the bug report reads, and says what AUTO did.
pub fn end_game() {
    let now = now();
    let Ok(mut g) = GOV.lock() else { return };
    let was_auto = g.phase != Phase::Off;
    g.close_auto(now);
    g.phase = Phase::Off;
    if !g.summarized && (was_auto || g.auto_ticks > 0) {
        g.summarized = true;
        let f = freq();
        log(&std::format!(
            "clocks: auto summary: raised {} s of {} s, ups={} keeps={} rejects={} downs={} refused={}\n",
            g.high_ticks / f, g.auto_ticks / f, g.ups, g.keeps, g.rejects, g.downs, g.refusals,
        ));
    }
}

/// Share of the AUTO time the last game spent raised, in percent; -1 when it
/// never ran on AUTO.
pub fn high_share_pct() -> i32 {
    let Ok(g) = GOV.lock() else { return -1 };
    let now = now();
    let mut auto = g.auto_ticks;
    let mut high = g.high_ticks;
    if g.auto_since != 0 {
        auto += now.saturating_sub(g.auto_since);
    }
    if g.high_since != 0 {
        high += now.saturating_sub(g.high_since);
    }
    if auto == 0 {
        return -1;
    }
    (high * 100 / auto).min(100) as i32
}

/// Drop the open window: a stretch of wall clock went by with no frames in it
/// (pause, HOME, sleep). Same hook and same reason as `fps_window_reset`.
pub fn window_reset() {
    if let Ok(mut g) = GOV.lock() {
        g.win = Window::EMPTY;
    }
}

/// Fold one host frame into the open window, and decide when it closes.
///
/// `t1` is the tick right after `Player::tick`, `tick_dt` and `render_dt` the
/// two halves of the frame's work, `nominal_fps` the rate the movie declares.
pub fn sample(t1: u64, tick_dt: u64, render_dt: u64, nominal_fps: f64) {
    let Ok(mut g) = GOV.lock() else { return };
    if g.phase == Phase::Off {
        return;
    }
    if !(nominal_fps.is_finite() && nominal_fps >= 1.0) {
        g.win = Window::EMPTY;
        return;
    }
    let (frames_run, _, _, _) = ruffle_core::flashnx_gc_probe();
    let acc = ruffle_core::flashnx_frame_accumulator_us();
    let f = freq();
    let w = &mut g.win;
    if w.start == 0 {
        // Opened on a tick that advanced the movie; its frames happened before
        // `t1`, so they are not this window's.
        if frames_run > 0 {
            *w = Window { start: t1.max(1), acc_start: acc, ..Window::EMPTY };
        }
        return;
    }
    w.frames += frames_run;
    w.tick += tick_dt;
    w.render += render_dt;
    w.renders += 1;
    let dt = t1.saturating_sub(w.start);
    if dt < f / 2 {
        return;
    }
    // At 3 fps half a second holds one frame: wait for a few, with a ceiling
    // so a game that has stopped still gets judged. Close on a tick that
    // advanced the movie, as the window opened on one.
    if (w.frames < 3 || frames_run == 0) && dt < f * 2 {
        return;
    }
    let win = *w;
    *w = Window { start: t1.max(1), acc_start: acc, ..Window::EMPTY };
    if win.frames == 0 {
        return;
    }
    // Speed: the frames' worth of movie time over the wall time, with the
    // accumulator's change folded in (time received, not yet a frame, or
    // thrown away), exactly as the FPS counter reads it.
    let budget_us = 1_000_000.0 / nominal_fps;
    let frames_eq = win.frames as f64 + (acc as f64 - win.acc_start as f64) / budget_us;
    let wall_us = dt as f64 * 1_000_000.0 / f as f64;
    let speed = (frames_eq.max(0.0) * budget_us / wall_us) as f32;
    // Work of one SWF frame: its share of the ticks, plus one render.
    let to_us = 1_000_000.0 / f as f64;
    let work_us = (win.tick as f64 / win.frames as f64
        + win.render as f64 / win.renders.max(1) as f64)
        * to_us;
    g.decide(t1, speed, work_us as f32, budget_us as f32);
}
