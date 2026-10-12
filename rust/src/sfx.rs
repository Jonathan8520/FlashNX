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
        // A press that moved nothing: a value changed in place (A on a value
        // row, Left / Right on a stepper), or a step against the end of a list
        // that does not wrap, which deserves silence.
        let stepper = match before {
            // Every row but the controls one is a value.
            Screen::SettingsPrefsModal { selection } => selection > 0,
            // Cursor speed, cursor, gyroscope.
            Screen::TouchesMenu { selection, .. } => (3..=5).contains(&selection),
            Screen::SettingsModal { selection } => selection == crate::library::set_row::SOUNDS,
            _ => false,
        };
        if matches!(button, "A" | "X" | "Y") || (is_nav(button) && stepper) {
            play(Sfx::Toggle);
        }
        return;
    }
    if core::mem::discriminant(&before) == core::mem::discriminant(&after) {
        // Same screen, something in it changed: the cursor (or the list under
        // it), or a value.
        play(if is_nav(button) { Sfx::Move } else { Sfx::Toggle });
        return;
    }
    play(match button {
        "B" | "Minus" => Sfx::Back,
        // A window opening over the page has its own, softer sweep.
        _ if opened => Sfx::Open,
        _ => Sfx::Decide,
    });
}
