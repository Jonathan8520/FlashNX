//! The values the pause changes on the game being played, armed by A, moved
//! by Left / Right while armed, kept by A, put back by B (input rules 1 and
//! 2).
//!
//! Jonathan, 2026-10-12: Left and Right changed a value wherever the cursor
//! happened to be, so a stick pushed slightly sideways on the way down a list
//! changed what it passed without anyone noticing. A value now changes only
//! once A has armed its row, and the rows say so (the gold outline and
//! arrows of `kit_ctrl_parts`).
//!
//! The pause's own rows (OVERCLOCK, and ÉCRAN's AFFICHAGE, ROTATION, PIXELS,
//! FILTRE) are routed by C++, the Touches et curseur page by `menu`. Both come
//! here, and both pages are drawn from here, so the row outlined on screen is
//! always the one Left and Right act on.
//!
//! While a row is armed its value is applied LIVE: the frozen frame behind the
//! panel is redrawn with it, the pointer moves at the new speed. Nothing is
//! written until it is kept, and then once: a held direction steps every
//! 80 ms, and each step used to rewrite the game's `.prefs`. B applies again
//! what was in force before A, which is still what the card holds, so a game
//! that followed Réglages still does.

use core::ffi::c_int;

extern "C" {
    fn ruffle_cursor_speed_index() -> c_int;
    fn ruffle_cursor_speed_preview(idx: c_int);
    fn ruffle_cursor_speed_set(idx: c_int) -> c_int;
    fn ruffle_cursor_speed_follow_global();
}

/// A value row of the pause's pages.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum PauseValue {
    Overclock,
    Display,
    Rotation,
    Pixels,
    Filter,
    CursorSpeed,
    Gyro,
}

/// The overclock's settings in the order its row shows them.
pub(crate) const POWER_ORDER: [u8; 3] = [0, crate::keymap::POWER_AUTO, 1];

impl PauseValue {
    /// How many options its row has.
    fn count(self) -> u8 {
        match self {
            PauseValue::Overclock => POWER_ORDER.len() as u8,
            PauseValue::Display => crate::keymap::DISPLAY_MODE_COUNT,
            PauseValue::Rotation => crate::keymap::ROTATION_COUNT,
            PauseValue::Pixels => crate::keymap::PIXEL_FILTER_COUNT,
            PauseValue::Filter => crate::keymap::SCREEN_FILTER_COUNT,
            PauseValue::CursorSpeed => crate::loc::CURSOR_SPEED_X10.len() as u8,
            PauseValue::Gyro => crate::keymap::GYRO_MODE_COUNT,
        }
    }
}

struct Armed {
    value: PauseValue,
    /// The option in force when A armed the row, and the one shown now.
    prior: u8,
    cur: u8,
    /// The framing when the row was armed. Turning the picture recentres it,
    /// and B (or coming back to the first turn) puts it back with the turn.
    pan: (i32, i32),
}

static ARMED: std::sync::Mutex<Option<Armed>> = std::sync::Mutex::new(None);

/// The option in force for `v`, as an index into its row's options: the armed
/// one while `v` is armed, else what the game runs with (the card's value: the
/// two only part while a row is armed).
pub(crate) fn current(v: PauseValue) -> u8 {
    if let Ok(g) = ARMED.lock() {
        if let Some(a) = g.as_ref().filter(|a| a.value == v) {
            return a.cur;
        }
    }
    live(v)
}

/// What the game runs with, read from where each value lives while it plays.
fn live(v: PauseValue) -> u8 {
    let n = v.count().max(1);
    let i = match v {
        PauseValue::Overclock => {
            let shown = crate::clock_auto::shown_mode();
            POWER_ORDER.iter().position(|m| *m == shown).unwrap_or(0) as u8
        }
        PauseValue::Display => crate::display_mode_shown(),
        PauseValue::Rotation => crate::backend::render::game_rotation(),
        PauseValue::Pixels => crate::backend::render::pixel_filter(),
        PauseValue::Filter => crate::backend::render::screen_filter(),
        PauseValue::CursorSpeed => unsafe { ruffle_cursor_speed_index() }.max(0) as u8,
        PauseValue::Gyro => crate::keymap::GYRO_MODE.load(core::sync::atomic::Ordering::Relaxed),
    };
    i.min(n - 1)
}

/// The row armed, if any.
pub(crate) fn armed() -> Option<PauseValue> {
    ARMED.lock().ok().and_then(|g| g.as_ref().map(|a| a.value))
}

/// Put option `i` of `v` on the game, without keeping it. False when the
/// console refused it (a raise of the clock), in which case what was in force
/// stays in force.
fn apply(v: PauseValue, i: u8, pan: (i32, i32)) -> bool {
    use crate::backend::render as r;
    match v {
        PauseValue::Overclock => {
            let want = POWER_ORDER[(i as usize).min(POWER_ORDER.len() - 1)];
            let before = crate::clock_auto::shown_mode();
            let got = crate::clock_auto::apply_setting(want);
            if want == 1 && got != 1 {
                // Refused (the battery out of its Normal state, or clkrst
                // unavailable). Asking for ON turned AUTO's governor off on
                // the way, so AUTO is entered again, and the refusal marked
                // again after it (entering AUTO clears it): the help keeps
                // saying why the raise did not happen. From OFF nothing moved.
                if before == crate::keymap::POWER_AUTO {
                    crate::clock_auto::apply_setting(crate::keymap::POWER_AUTO);
                    r::note_overclock_refused();
                }
                crate::net::log(&std::format!("menu: OVERCLOCK asked={} REFUSED, kept {}\n", want, before));
                return false;
            }
            true
        }
        PauseValue::Display => {
            crate::display_mode_live(i);
            true
        }
        PauseValue::Rotation => {
            crate::rotation_live(i, pan);
            true
        }
        PauseValue::Pixels => {
            r::set_pixel_filter(i);
            true
        }
        PauseValue::Filter => {
            r::set_screen_filter(i);
            true
        }
        PauseValue::CursorSpeed => {
            unsafe { ruffle_cursor_speed_preview(i as c_int) };
            true
        }
        PauseValue::Gyro => {
            crate::keymap::GYRO_MODE.store(i, core::sync::atomic::Ordering::Relaxed);
            true
        }
    }
}

/// Write option `i` of `v` for the game, now in force. `pan` is the framing
/// before a turn of the picture recentred it.
fn persist(v: PauseValue, i: u8, pan: (i32, i32)) {
    use crate::keymap as k;
    match v {
        // What the hardware accepted, not what was asked for: a refused
        // raise must not leave the game marked ON while it runs at 1020 MHz.
        PauseValue::Overclock => {
            let got = crate::clock_auto::shown_mode();
            k::set_power_mode(got);
            crate::net::log(&std::format!("menu: OVERCLOCK kept {}\n", got));
        }
        PauseValue::Display => k::set_display_mode(i),
        PauseValue::Rotation => {
            k::set_rotation(i);
            // The turn recentred the framing: the card follows, so the next
            // launch does not frame the new turn with the old turn's offset.
            if pan != (0, 0) {
                k::set_zoom(crate::backend::render::game_zoom_percent(), 0, 0);
            }
        }
        PauseValue::Pixels => k::set_pixel_filter(i),
        PauseValue::Filter => k::set_screen_filter(i),
        PauseValue::CursorSpeed => {
            unsafe { ruffle_cursor_speed_set(i as c_int) };
        }
        PauseValue::Gyro => k::set_gyro(i),
    }
    forget_own();
}

/// Arm `v` (A on its row). A row armed before is kept first.
pub(crate) fn arm(v: PauseValue) {
    confirm();
    let cur = live(v);
    if let Ok(mut g) = ARMED.lock() {
        *g = Some(Armed { value: v, prior: cur, cur, pan: crate::backend::render::game_pan() });
    }
}

/// Left (`dir` < 0) or Right on the armed row: 1 when the value moved, 0 when
/// it was already at that end (or nothing is armed), -1 when the console
/// refused the new value.
pub(crate) fn step(dir: i32) -> i32 {
    let Ok(mut g) = ARMED.lock() else { return 0 };
    let Some(a) = g.as_mut() else { return 0 };
    let n = a.value.count();
    let next = if dir < 0 { a.cur.saturating_sub(1) } else { (a.cur + 1).min(n.saturating_sub(1)) };
    if next == a.cur {
        return 0;
    }
    // Back on the turn the row was armed on, the framing comes back with it.
    let pan = if next == a.prior { a.pan } else { (0, 0) };
    if !apply(a.value, next, pan) {
        return -1;
    }
    a.cur = next;
    1
}

/// Keep the armed value (A, Up / Down, closing the pause, a tap elsewhere):
/// written when it differs from the one before A. False when nothing was
/// armed.
pub(crate) fn confirm() -> bool {
    let taken = ARMED.lock().ok().and_then(|mut g| g.take());
    let Some(a) = taken else { return false };
    if a.cur != a.prior {
        persist(a.value, a.cur, a.pan);
    }
    true
}

/// Put back the value from before A (B). False when nothing was armed.
pub(crate) fn cancel() -> bool {
    let taken = ARMED.lock().ok().and_then(|mut g| g.take());
    let Some(a) = taken else { return false };
    if a.cur != a.prior {
        apply(a.value, a.prior, a.pan);
    }
    true
}

/// A tap on option `i` of `v`: set at once and kept (input rule 6), with no
/// arming. A row armed elsewhere is kept first. 1 when the value changed, 0
/// when `i` was already in force, -1 when the console refused it.
pub(crate) fn set(v: PauseValue, i: u8) -> i32 {
    let held = ARMED.lock().ok().and_then(|mut g| {
        if g.as_ref().is_some_and(|a| a.value == v) {
            g.take()
        } else {
            None
        }
    });
    let (prior, pan) = match held {
        // The armed row itself: the tapped option is the one kept.
        Some(a) => (a.prior, a.pan),
        None => {
            confirm();
            (live(v), crate::backend::render::game_pan())
        }
    };
    let i = i.min(v.count().saturating_sub(1));
    let before = live(v);
    if i != before && !apply(v, i, if i == prior { pan } else { (0, 0) }) {
        // What was in force stays, and a value the armed row had moved to
        // before the tap is still kept.
        if before != prior {
            persist(v, before, pan);
        }
        return -1;
    }
    if i != prior {
        persist(v, i, pan);
    }
    (i != before) as i32
}

/// A fresh pause, or a game ending: an armed row left behind (the pause shut
/// by a path that kept nothing) gives back what it changed.
pub(crate) fn pause_opened() {
    cancel();
    forget_own();
}

/// A new game: the old one's armed row and cached keys mean nothing for it,
/// and its player starts from what the card holds.
pub(crate) fn forget() {
    if let Ok(mut g) = ARMED.lock() {
        *g = None;
    }
    forget_own();
}

// ── "par défaut", and X ───────────────────────────────────────────────────

/// The `.prefs` keys behind a row, by its name (`render::SCREEN_ITEMS`,
/// `MENU_ITEMS`), as the game options in the launcher name them too.
fn own_keys(key: &str) -> &'static [&'static str] {
    match key {
        "OVERCLOCK" => &["power"],
        "AFFICHAGE" => &["display"],
        "ROTATION" => &["rot"],
        "ZOOM" => &["zoom", "panx", "pany"],
        "PIXELS" => &["pixels"],
        "FILTRE" => &["filter"],
        "FPS" => &["fps"],
        _ => &[],
    }
}

/// The running game's own keys, and whether it has a cursor speed of its own:
/// read once per pause (the rows ask every frame, and each answer is a read of
/// the card), forgotten after every write.
static OWN: std::sync::Mutex<Option<(std::vec::Vec<std::string::String>, bool)>> = std::sync::Mutex::new(None);

fn forget_own() {
    if let Ok(mut g) = OWN.lock() {
        *g = None;
    }
}

/// The running game's `.prefs` or cursor speed were written from outside this
/// module (the framing mode kept a zoom, a profile brought a speed): the rows'
/// "par défaut" notes are read again.
pub(crate) fn prefs_written() {
    forget_own();
}

/// Whether the running game has a value of its own for row `key` (else it
/// follows the default set in Réglages).
fn has_own(key: &str) -> bool {
    let Ok(mut g) = OWN.lock() else { return false };
    if g.is_none() {
        *g = crate::keymap::active_game_basename()
            .map(|b| (crate::keymap::pref_keys(&b), crate::keymap::cursor_speed_for(&b) >= 0));
    }
    let Some((keys, speed)) = g.as_ref() else { return false };
    if key == "VITESSE" {
        return *speed;
    }
    own_keys(key).first().is_some_and(|k0| keys.iter().any(|k| k == k0))
}

/// Whether row `key` follows the default from Réglages: its row says
/// "par défaut", as the game's options do in the launcher.
pub(crate) fn follows_default(key: &str) -> bool {
    !has_own(key)
}

/// X on row `key`: the game drops its own value and follows Réglages again,
/// at once. False when it had none (nothing happens, nothing sounds).
pub(crate) fn reset_default(key: &str) -> bool {
    use crate::backend::render as r;
    use crate::loc as l;
    if !has_own(key) {
        return false;
    }
    let Some(b) = crate::keymap::active_game_basename() else { return false };
    if key == "VITESSE" {
        crate::keymap::set_cursor_speed(-1);
        unsafe { ruffle_cursor_speed_follow_global() };
        forget_own();
        return true;
    }
    crate::keymap::clear_prefs(&b, own_keys(key));
    match key {
        "OVERCLOCK" => {
            crate::clock_auto::apply_setting(l::default_power_mode());
        }
        "AFFICHAGE" => crate::display_mode_live(l::default_display_mode()),
        "ROTATION" => {
            let q = l::default_rotation();
            if q != r::game_rotation() {
                let pan = r::game_pan();
                crate::rotation_live(q, (0, 0));
                if pan != (0, 0) {
                    crate::keymap::set_zoom(r::game_zoom_percent(), 0, 0);
                }
            }
        }
        "ZOOM" => crate::zoom_live(l::default_zoom(), 0, 0),
        "PIXELS" => r::set_pixel_filter(l::default_pixel_filter()),
        "FILTRE" => r::set_screen_filter(l::default_screen_filter()),
        "FPS" => r::set_fps_counter_shown(l::default_fps_counter() == 1),
        _ => {}
    }
    forget_own();
    true
}

/// A on the counter's switch: flipped and kept. Only the flag moves: the
/// reading taken while the game ran stays, so the corner preview and the
/// pause's speed chip keep their number (resetting it left "--" until the
/// game ran again).
pub(crate) fn toggle_fps() {
    let on = !crate::backend::render::fps_counter_on();
    crate::keymap::set_fps_counter(on as u8);
    crate::backend::render::set_fps_counter_shown(on);
    forget_own();
}
