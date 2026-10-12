//! Interface sounds: the clicks of the menus.
//!
//! The sounds themselves are synthesised and played by the C++ audio backend
//! (`cpp/src/audio.cpp`, voices 1-3), so a click lands within one audren frame
//! of the press. This side decides WHICH sound a press deserves.
//!
//! The launcher does not ask each of its thirty screens to remember to make a
//! noise: `library::input` hands the screen before and after every press to
//! `after_input`, which reads what happened from the difference. The cursor
//! moved, a window opened, B went back, the tab changed, a value changed in
//! place. A screen added later is covered without a line of sound code.

use core::ffi::c_int;

extern "C" {
    fn ruffle_ui_sound(kind: c_int);
    fn ruffle_ui_sounds_set_enabled(on: c_int);
}

/// One interface sound. The order is the C++ side's `UiSound`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Sfx {
    /// The cursor moved.
    Move = 0,
    /// A opened something or did something.
    Decide = 1,
    /// B went back.
    Back = 2,
    /// L / R changed tab.
    Tab = 3,
    /// A value changed in place.
    Toggle = 4,
    /// Refused, or failed.
    Error = 5,
    /// A window opened.
    Open = 6,
    /// A window closed.
    Close = 7,
    /// A game is starting.
    Launch = 8,
}

pub(crate) fn play(s: Sfx) {
    unsafe { ruffle_ui_sound(s as c_int) };
}

/// Set by a handler when a press changed a value in place (a choice moved, a
/// switch flipped), read once by the next `after_input` / `menu::input`.
///
/// The screen alone cannot say it: a value changing leaves the screen exactly
/// as it was, and so does a step refused at the end of a choice. Guessing from
/// the row ("this row is a stepper") clicked on both, so a refused step clicked
/// as loudly as a real one.
static CHANGED: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

/// Say that the press being handled changed a value.
pub(crate) fn value_changed() {
    CHANGED.store(true, core::sync::atomic::Ordering::Relaxed);
}

/// Whether the press just handled changed a value (and forget it).
pub(crate) fn take_changed() -> bool {
    CHANGED.swap(false, core::sync::atomic::Ordering::Relaxed)
}

/// Set by a handler when the press being handled did nothing at all (A on a
/// row that is only there to say why it cannot be chosen), read once by the
/// next `after_input`. Such a press leaves the screen as it was, which reads
/// like a value changed in place, and it clicked for nothing.
static IDLE: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

/// Say that the press being handled did nothing: it stays silent.
pub(crate) fn press_did_nothing() {
    IDLE.store(true, core::sync::atomic::Ordering::Relaxed);
}

/// RÉGLAGES > interface sounds: tell the backend.
pub(crate) fn set_enabled(on: bool) {
    unsafe { ruffle_ui_sounds_set_enabled(on as c_int) };
}

fn is_nav(button: &str) -> bool {
    matches!(
        button,
        "Up" | "Down" | "Left" | "Right" | "StickLUp" | "StickLDown" | "StickLLeft" | "StickLRight" | "ZL" | "ZR"
    )
}

/// The sound one launcher press deserves, read from the screen before and
/// after it. `closing` says whether the press started a window's close-out
/// (the screen itself only changes when it finishes); `opened`, whether a
/// window came up over a page.
pub(crate) fn after_input(
    button: &str,
    before: crate::library::Screen,
    after: crate::library::Screen,
    closing: bool,
    tab_before: Option<crate::library::Tab>,
    tab_after: Option<crate::library::Tab>,
    opened: bool,
) {
    use crate::library::Screen;
    let changed = take_changed();
    // Read (and forgotten) here, whatever happens next, like `changed`.
    let idle = IDLE.swap(false, core::sync::atomic::Ordering::Relaxed);
    // The controls editor plays its own sounds (`menu::input`).
    let in_editor = |s: Screen| matches!(s, Screen::TouchesEditor { .. } | Screen::SettingsKeymapEditor);
    if in_editor(before) && in_editor(after) {
        return;
    }
    if matches!(after, Screen::Launching { .. }) && !matches!(before, Screen::Launching { .. }) {
        play(Sfx::Launch);
        return;
    }
    if matches!(after, Screen::Quit) {
        play(Sfx::Close);
        return;
    }
    // An error screen sounds its own note when it appears, wherever it came
    // from (`library::render`): many arrive later, from a download.
    if matches!(after, Screen::DistantError) {
        return;
    }
    // ZL / ZR walk the folders: sideways, like a tab.
    if matches!(button, "ZL" | "ZR") && before != after {
        play(Sfx::Tab);
        return;
    }
    if closing {
        play(if matches!(button, "B" | "Minus") { Sfx::Back } else { Sfx::Decide });
        return;
    }
    if tab_before != tab_after && tab_before.is_some() && tab_after.is_some() {
        play(Sfx::Tab);
        return;
    }
    if before == after {
        // A press that moved nothing: a value changed in place (the handler
        // said so), A on a value row, or a step against the end of a list or a
        // choice, which deserves silence. (RÉGLAGES clicks its own values as
        // they move: only it knows whether Left or Right changed anything.)
        // A press its handler says did nothing is silent too.
        if !idle && (changed || matches!(button, "A" | "X" | "Y")) {
            play(Sfx::Toggle);
        }
        return;
    }
    // Into or out of the RÉGLAGES pane: a level, not a step of the cursor.
    if let (Screen::Settings { in_pane: a, .. }, Screen::Settings { in_pane: b, .. }) = (before, after) {
        if a != b {
            play(match button {
                "B" | "Minus" => Sfx::Back,
                "A" => Sfx::Decide,
                _ => Sfx::Move,
            });
            return;
        }
    }
    if core::mem::discriminant(&before) == core::mem::discriminant(&after) {
        // Same screen, something in it changed: a value (the handler said so),
        // the cursor or the list under it, or the game a panel is about (L / R
        // in a game's options, a move).
        play(if changed {
            Sfx::Toggle
        } else if matches!(button, "B" | "Minus") {
            // B on the home clearing a search or going back to every game.
            Sfx::Back
        } else if is_nav(button) || matches!(button, "L" | "R") {
            Sfx::Move
        } else {
            Sfx::Toggle
        });
        return;
    }
    play(match button {
        "B" | "Minus" => Sfx::Back,
        // A window opening over the page has its own, softer sweep.
        _ if opened => Sfx::Open,
        _ => Sfx::Decide,
    });
}
