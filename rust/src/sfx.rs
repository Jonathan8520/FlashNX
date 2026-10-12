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

/// A sound the handler of a press chose itself, played instead of the one
/// read from the screens. Arming a row and keeping its value (Decide), or
/// putting it back (Back), leave the screen exactly as it was, and so does a
/// refusal (Error): the difference cannot say what happened, the handler can.
static CHOSEN: core::sync::atomic::AtomicU8 = core::sync::atomic::AtomicU8::new(CHOSEN_NONE);
const CHOSEN_NONE: u8 = 0xFF;
const CHOSEN_SILENCE: u8 = 0xFE;

/// This press makes `s`, whatever the screens say.
pub(crate) fn choose(s: Sfx) {
    CHOSEN.store(s as u8, core::sync::atomic::Ordering::Relaxed);
}

/// This press makes no sound: it did nothing (A on the language already in
/// use, on a page with only things to read).
pub(crate) fn hush() {
    CHOSEN.store(CHOSEN_SILENCE, core::sync::atomic::Ordering::Relaxed);
}

/// The sound the press just handled chose, if it chose one (and forget it):
/// `Some(None)` is a chosen silence.
pub(crate) fn take_chosen() -> Option<Option<Sfx>> {
    const ALL: [Sfx; 9] = [
        Sfx::Move,
        Sfx::Decide,
        Sfx::Back,
        Sfx::Tab,
        Sfx::Toggle,
        Sfx::Error,
        Sfx::Open,
        Sfx::Close,
        Sfx::Launch,
    ];
    match CHOSEN.swap(CHOSEN_NONE, core::sync::atomic::Ordering::Relaxed) {
        CHOSEN_NONE => None,
        CHOSEN_SILENCE => Some(None),
        v => Some(ALL.iter().copied().find(|s| *s as u8 == v)),
    }
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
    // A sound the handler chose wins (see `choose`). Asked before the editor
    // below: closing the defaults' controls editor is decided out here, the
    // editor still on screen while it scales out.
    if let Some(chosen) = take_chosen() {
        if let Some(s) = chosen {
            play(s);
        }
        return;
    }
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
    // from (`library::render`): many arrive later, from a download. The
    // report the relay refused is one too: with the window's sweep on top,
    // a failed send played two sounds at once.
    if matches!(after, Screen::DistantError | Screen::SendFailed { .. }) {
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
        // A press that moved nothing on screen: a value changed in place (the
        // handler said so) clicks. Anything else deserves silence: a step
        // against the end of a list or a choice, and an A, an X or a Y with
        // nothing to act on (they used to click like a toggle whatever they
        // did). What acts in place without changing a value says so itself
        // (`choose`: arming a row, a refusal, a report sent).
        if changed {
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
