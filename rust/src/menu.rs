//! In-game pause "TOUCHES" sub-menu + keymap editor.
//!
//! Reached from the pause menu's TOUCHES entry (`ruffle_touches_open` ->
//! `open_submenu`). Mirrors the library OPTIONS > TOUCHES sub-menu (#20 Option 1):
//!
//!   - **Menu** — edit keys / apply a profile / share my controls / cursor speed
//!     / revert. Apply / share / revert do HTTPS synchronously (the game is
//!     paused, so a brief freeze is fine — no async infra here).
//!   - **List / Keyboard** — the per-button keymap editor (also used directly by
//!     the library via `open`). The picker is a visual QWERTY board (issue #55).
//!   - **Profiles / Preview / ShareConfirm / RevertPreview** — the profile flows.
//!
//! The sub-menu and the profile flows open beside the pause, which folds to a
//! rail (`kit_cascade`): going into them reads as going deeper into one menu
//! rather than as a window opening over it. Only the editor and its keyboard
//! are windows.
//!
//! All state lives behind a single Mutex so C++ can drive the screen via a thin
//! FFI surface: `is_active`, `open`/`open_submenu`, `input`, `draw`,
//! `consume_dirty`. Network actions drop the lock before the HTTPS call.

use std::sync::Mutex;

use crate::backend::render::{PadValue, SwitchRenderBackend, PAUSE_ROW_TOUCHES};
use crate::keymap;

// C++-owned cursor speed (live + per-game while in a game), reused from the
// library extern block: cycle to the next preset / read the x10 label value.
use crate::library::{ruffle_cursor_speed_mult_x10, ruffle_cursor_speed_step};

// In-game TOUCHES sub-menu rows (#20 Option 1): edit / apply / share / cursor
// speed / show-cursor toggle, then a revert row (index 5) when there's a custom
// keymap to undo. Mirrors the library sub-menu order.
const MENU_EDIT: usize = 0;
const MENU_APPLY: usize = 1;
const MENU_SHARE: usize = 2;
const MENU_CURSOR: usize = 3;
const MENU_SHOWCURSOR: usize = 4;
const MENU_GYRO: usize = 5;
const MENU_REVERT: usize = 6;
const MENU_FIXED_ROWS: usize = 6;

// Toast kinds (match `SwitchRenderBackend::draw_toast`): 0 green, 1 red, 2 blue.
const TOAST_OK: u8 = 0;
const TOAST_ERR: u8 = 1;
const TOAST_INFO: u8 = 2;
const TOAST_FRAMES: u32 = 150; // ~2.5 s at 60 fps

const GLIDE_KEY_TOUCHES_MENU: u32 = 59;
const GLIDE_KEY_TOUCHES_PROFILES: u32 = 60;

/// Cap a diff list so the auto-sized modal still fits the screen.
const MAX_PREVIEW_ROWS: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq)]
enum Screen {
    Inactive,
    /// In-game TOUCHES sub-menu (edit / apply / share / cursor / revert).
    Menu { selection: usize },
    /// The pad view — the keymap editor. `selection` indexes `keymap::PAD_SLOTS`.
    /// No scroll: the pad shows all twenty-five controls at once, so there is no
    /// window for a cursor to fall out of and nothing to put back on the way in.
    List { selection: usize },
    /// Visual keyboard picker (issue #55). `button_idx` indexes
    /// `keymap::PAD_SLOTS`; `key_idx` indexes `keymap::KEYBOARD` (positioned
    /// keys, geometric 2D nav).
    Keyboard { button_idx: usize, key_idx: usize },
    /// Community-profile picker (in-game apply). `selection` indexes `matches`.
    Profiles { selection: usize },
    /// Before/after preview of an apply. `profile_idx` indexes `matches`.
    Preview { profile_idx: usize },
    /// Confirm sharing (before/after of my shared profile).
    ShareConfirm,
    /// Before/after preview of a revert.
    RevertPreview,
    /// Confirm deleting one of MY OWN shared profiles (X on a self-shared row in
    /// the picker). `profile_idx` indexes `matches`. A deletes (hoisted), B back.
    /// `selection`: 0 = cancel (where it opens), 1 = delete.
    DeleteConfirm { profile_idx: usize, selection: usize },
    /// Transient loading panel shown for one frame before a deferred profile
    /// network flow runs (so the blocking GitHub call doesn't freeze on stale
    /// content). `draw` runs the stashed `PendingNet` the next frame.
    Loading,
}

struct State {
    screen: Screen,
    /// Set true after the keymap changes (set_binding / apply / revert) so C++
    /// refreshes its runtime BINDINGS table next frame. Cleared by `consume_dirty`.
    dirty: bool,
    /// True while the IN-GAME sub-menu flow owns the screen (entered via
    /// `open_submenu`): the editor's B returns to the sub-menu instead of closing.
    /// The library opens the editor directly (`open`) with this false.
    submenu: bool,
    /// Opened from the launcher: the game whose options it came from, or None
    /// for the defaults of every game (RÉGLAGES). The editor's header names it.
    /// In game the running game's name is read as it is drawn.
    subject: Option<std::string::String>,
    /// That game's cursor speed, x10, for the editor's footer. Read once on the
    /// way in: in the launcher the live speed is the global one, and the game's
    /// own is a file on the card, which is no read for every frame.
    subject_speed_x10: u32,
    /// Whether the sub-menu shows a revert row (game has a custom keymap).
    can_revert: bool,
    /// Whether a hand-made backup exists (revert restores it) vs none (resets to
    /// default) — drives the revert label.
    has_backup: bool,
    /// Profiles matching the running game (in-game apply picker).
    matches: std::vec::Vec<crate::profiles::Match>,
    /// True while the picker lists the player's other games to copy from
    /// (COPY FROM ANOTHER GAME); `saved_matches` keeps the community list for B.
    copy_mode: bool,
    saved_matches: std::vec::Vec<crate::profiles::Match>,
    /// Id of the profile whose bindings match the current keymap (the active tag).
    active_id: std::string::String,
    /// Snapshotted before/after diff lines for the open Preview / ShareConfirm /
    /// RevertPreview.
    preview_rows: std::vec::Vec<std::string::String>,
    /// Whether sharing will UPDATE an existing shared profile vs create the first.
    share_is_update: bool,
    /// Transient toast over the current screen (msg + kind + frames remaining).
    toast_msg: std::string::String,
    toast_kind: u8,
    toast_frames: u32,
}

static TOUCHES: Mutex<State> = Mutex::new(State {
    screen: Screen::Inactive,
    dirty: false,
    submenu: false,
    subject: None,
    subject_speed_x10: 10,
    can_revert: false,
    has_backup: false,
    matches: std::vec::Vec::new(),
    copy_mode: false,
    saved_matches: std::vec::Vec::new(),
    active_id: std::string::String::new(),
    preview_rows: std::vec::Vec::new(),
    share_is_update: false,
    toast_msg: std::string::String::new(),
    toast_kind: 0,
    toast_frames: 0,
});

pub fn is_active() -> bool {
    TOUCHES
        .lock()
        .map(|s| !matches!(s.screen, Screen::Inactive))
        .unwrap_or(false)
}

/// Put the editor's cursor on the row or key a finger just landed on.
///
/// Returns true when it was ALREADY there, which the caller reads as "activate"
/// — the same double-tap the gallery has always used. A key that fired on the
/// first tap would rebind a button before the player could see which key their
/// thumb had covered, and on a keyboard of a hundred caps a thumb covers four.
///
/// Only the two screens that publish a touch table answer: the per-button row
/// list and the keyboard. The rest keep their buttons.
pub fn touch_select(idx: usize) -> bool {
    let Ok(mut s) = TOUCHES.lock() else {
        return false;
    };
    match s.screen {
        Screen::Keyboard { button_idx, key_idx } => {
            if idx >= crate::keymap::KEYBOARD.len() {
                return false;
            }
            if key_idx == idx {
                return true;
            }
            s.screen = Screen::Keyboard { button_idx, key_idx: idx };
            false
        }
        // Every arm BOUNDS `idx` before storing it, the way the one above does.
        // `idx` is a row of the published table, and the table outlives the panel
        // that published it by one frame: C++ serves buttons before touch, so a
        // button that changes screens leaves the finger to be hit-tested against
        // the previous screen's rows. A row of the 92-key keyboard stored as a
        // row of the 25-slot pad is not a wrong cursor, it is `PAD_SLOTS[60]`
        // on the next A -- and `panic = "abort"` makes that a console fatal,
        // not an exception.
        Screen::List { selection } => {
            use crate::backend::render::{PAD_CELL_ICON, PAD_CELL_LAYER, PAD_CELL_PLAYER};
            let n = crate::keymap::PAD_SLOTS.len();
            // The header's switches: a tap switches, as X and the shoulders
            // do, and there is nothing to take after it.
            if (PAD_CELL_PLAYER..PAD_CELL_ICON).contains(&idx) {
                let before = (keymap::edit_player(), keymap::edit_subtab_index());
                if idx < PAD_CELL_LAYER {
                    keymap::set_edit_player(if idx == PAD_CELL_PLAYER { 1 } else { 2 });
                } else {
                    keymap::set_edit_subtab_index(idx - PAD_CELL_LAYER);
                }
                if (keymap::edit_player(), keymap::edit_subtab_index()) != before {
                    crate::sfx::play(crate::sfx::Sfx::Tab);
                }
                s.screen = Screen::List { selection: unstick_cursor(selection) };
                return false;
            }
            // A control on the picture stands for its row. A stick stands for
            // the one of its five rows already under the cursor, else its
            // first, so a second tap on it opens the row being read.
            let row = if (PAD_CELL_ICON..PAD_CELL_ICON + n).contains(&idx) {
                let r = idx - PAD_CELL_ICON;
                let same_stick = crate::keymap::PAD_SLOTS
                    .get(r)
                    .zip(crate::keymap::PAD_SLOTS.get(selection))
                    .is_some_and(|(a, b)| matches!(a.shape, keymap::PadIcon::Stick(_)) && a.icon == b.icon);
                if same_stick { selection } else { r }
            } else if idx < n {
                idx
            } else {
                return false;
            };
            // A modifier row takes no cursor from a finger either, and returning
            // false means the tap is not counted as the first half of a
            // double-tap that would then activate it. It says why, and sounds
            // the refusal, as A does (`input`).
            if let Some(slot) = crate::keymap::PAD_SLOTS.get(row).filter(|s| crate::keymap::slot_is_modifier(s.name)) {
                let msg = crate::loc_ui::ui().toast_modifier_locked.replace("{}", slot.name);
                set_toast(&mut s, msg, TOAST_INFO);
                crate::sfx::play(crate::sfx::Sfx::Error);
                return false;
            }
            if selection == row {
                return true;
            }
            s.screen = Screen::List { selection: row };
            false
        }
        // The rows of the sub-menu and the profile picker are pages of the
        // pause sheet, whose rows publish their table like every list in the
        // app -- so they answer a finger for the same reason the library's do.
        Screen::Menu { selection } => {
            // Same count `handle_menu_input` bounds its cursor with.
            if idx > MENU_FIXED_ROWS - 1 + s.can_revert as usize {
                return false;
            }
            if selection == idx {
                return true;
            }
            s.screen = Screen::Menu { selection: idx };
            false
        }
        Screen::DeleteConfirm { profile_idx, selection } => {
            // The two buttons: 0 = cancel, 1 = delete.
            if idx > 1 {
                return false;
            }
            if selection == idx {
                return true;
            }
            s.screen = Screen::DeleteConfirm { profile_idx, selection: idx };
            false
        }
        Screen::Profiles { selection } => {
            // The sheet publishes every row by its index in the list.
            if idx >= s.matches.len() {
                return false;
            }
            if selection == idx {
                return true;
            }
            s.screen = Screen::Profiles { selection: idx };
            false
        }
        _ => false,
    }
}

/// Distinct id per active screen (0 = inactive). Lets the in-game caller
/// (`ruffle_touches_draw`) re-trigger the modal scale-in "pop" on each
/// transition, exactly like `library::modal_kind` does for the on-cover modals,
/// so the in-game TOUCHES sub-screens animate instead of snapping in.
pub fn screen_kind() -> u8 {
    TOUCHES
        .lock()
        .map(|s| match s.screen {
            Screen::Inactive => 0,
            Screen::Menu { .. } => 1,
            Screen::List { .. } => 2,
            Screen::Keyboard { .. } => 3,
            Screen::Profiles { .. } => 4,
            Screen::Preview { .. } => 5,
            Screen::ShareConfirm => 6,
            Screen::RevertPreview => 7,
            Screen::DeleteConfirm { .. } => 8,
            Screen::Loading => 9,
        })
        .unwrap_or(0)
}

/// Whether two screens (`screen_kind`) are one window: the editor and its key
/// picker share `render::touches_window`, so going between them changes what
/// is in the window and must not pop it in again after every key.
pub fn same_window(a: u8, b: u8) -> bool {
    matches!((a, b), (2, 3) | (3, 2))
}

/// `screen_kind`, the editor and its key picker counted as one window: what
/// decides whether a window pops in.
pub fn window_kind() -> u8 {
    match screen_kind() {
        3 => 2,
        k => k,
    }
}

/// Whether the screen up is a page of the pause sheet (the sub-menu, the
/// profile flows) rather than a window (the editor, its keyboard). A page
/// replaces the one before it in place; only a window pops.
pub fn screen_is_sheet() -> bool {
    TOUCHES
        .lock()
        .map(|s| {
            matches!(
                s.screen,
                Screen::Menu { .. }
                    | Screen::Profiles { .. }
                    | Screen::Preview { .. }
                    | Screen::ShareConfirm
                    | Screen::RevertPreview
            )
        })
        .unwrap_or(false)
}

/// Open the keymap EDITOR directly from the launcher: from a game's options
/// (`game` = its name; the keymap was pointed at it) or for the defaults of
/// every game (`None`). The in-game pause entry uses `open_submenu` instead.
pub fn open(game: Option<&str>) {
    keymap::set_edit_player(1); // always start on Player 1 (issue #40)...
    keymap::reset_edit_subtabs(); // ...both players on the NORMAL sub-tab, without
    // clearing the persisted modifiers (combos stay enabled in-game) (issue #57).
    // Row 0 is ZL, and ZL is locked the moment it has a combo layer -- so the
    // editor could OPEN with its cursor on a row that takes no A. Resolved
    // before the lock, not inside it, to keep this off the TOUCHES -> keymap
    // nesting entirely.
    let start = unstick_cursor(0);
    // The game's own cursor speed, else the global one it follows.
    let speed = keymap::active_game_basename()
        .map(|b| keymap::cursor_speed_for(&b))
        .filter(|i| *i >= 0)
        .and_then(|i| crate::loc::CURSOR_SPEED_X10.get(i as usize).copied())
        .unwrap_or_else(|| unsafe { ruffle_cursor_speed_mult_x10() }.max(5) as u32);
    if let Ok(mut s) = TOUCHES.lock() {
        s.submenu = false;
        s.subject = game.map(str::to_string);
        s.subject_speed_x10 = speed;
        s.screen = Screen::List { selection: start };
    }
}

/// Open the IN-GAME TOUCHES sub-menu (#20 Option 1). Editing from here returns to
/// this menu; apply/share/revert run from here too.
pub fn open_submenu() {
    let (can_revert, has_backup) = keymap::active_game_basename()
        .map(|b| (keymap::has_revert(&b), keymap::has_backup(&b)))
        .unwrap_or((false, false));
    if let Ok(mut s) = TOUCHES.lock() {
        s.submenu = true;
        s.can_revert = can_revert;
        s.has_backup = has_backup;
        s.screen = Screen::Menu { selection: 0 };
    }
}

pub fn close() {
    if let Ok(mut s) = TOUCHES.lock() {
        s.screen = Screen::Inactive;
    }
}

/// Returns true and clears the flag if the C++ caller should refresh its runtime
/// BINDINGS table (a binding changed since last call).
pub fn consume_dirty() -> bool {
    if let Ok(mut s) = TOUCHES.lock() {
        let d = s.dirty;
        s.dirty = false;
        d
    } else {
        false
    }
}

/// Flash a transient toast over the current screen (`draw` counts it down).
fn set_toast(s: &mut State, msg: std::string::String, kind: u8) {
    s.toast_msg = msg;
    s.toast_kind = kind;
    s.toast_frames = TOAST_FRAMES;
}

/// (basename, title, on-SD .swf path) of the running game, for the in-game
/// profile actions. None if no game is active.
fn game_ctx() -> Option<(std::string::String, std::string::String, std::string::String)> {
    let basename = keymap::active_game_basename()?;
    let title = crate::library::active_display_name().unwrap_or_else(|| basename.clone());
    let path = crate::last_swf_real_path().unwrap_or_default();
    Some((basename, title, path))
}

fn cap_rows(mut rows: std::vec::Vec<std::string::String>) -> std::vec::Vec<std::string::String> {
    if rows.len() > MAX_PREVIEW_ROWS {
        let extra = rows.len() - MAX_PREVIEW_ROWS;
        rows.truncate(MAX_PREVIEW_ROWS);
        rows.push(crate::loc_ui::ui().more_changes.replace("{}", &extra.to_string()));
    }
    rows
}

/// A profile network flow deferred one frame so the loading panel shows BEFORE
/// the blocking GitHub call freezes the UI (in-game mirror of `library`'s
/// `PendingNet`). `input` stashes it + flips to `Screen::Loading`; `draw` runs it
/// the next frame, which sets the result screen. `bool` = armed.
enum PendingNet {
    OpenProfiles,
    OpenShareConfirm,
    Apply { profile_idx: usize },
    Share,
    Delete { profile_idx: usize },
}
static PENDING_NET: std::sync::Mutex<Option<(PendingNet, bool)>> = std::sync::Mutex::new(None);

/// Stash `action` (run next frame) and flip to the loading screen now. Caller
/// holds no lock (it dropped `s` first).
fn defer_net(action: PendingNet) {
    if let Ok(mut p) = PENDING_NET.lock() {
        *p = Some((action, false));
    }
    if let Ok(mut s) = TOUCHES.lock() {
        s.screen = Screen::Loading;
    }
}

/// Run a deferred flow once its loading panel has shown a frame. Called at the
/// top of `draw`. First call arms it (panel draws this frame); the next runs the
/// blocking flow, which transitions to the result screen.
fn drive_pending_net() {
    let action = {
        let mut g = match PENDING_NET.lock() {
            Ok(g) => g,
            Err(_) => return,
        };
        match g.take() {
            None => return,
            Some((a, false)) => {
                *g = Some((a, true));
                return;
            }
            Some((a, true)) => a,
        }
    };
    match action {
        PendingNet::OpenProfiles => run_open_profiles(),
        PendingNet::OpenShareConfirm => run_open_share_confirm(),
        PendingNet::Apply { profile_idx } => run_apply(profile_idx),
        PendingNet::Share => run_share(),
        PendingNet::Delete { profile_idx } => run_delete(profile_idx),
    }
}

/// Forward a Switch-button **down-edge** event from C++. Returns true if the
/// event was consumed (input shouldn't fall through to the game / pause menu).
pub fn input(button: &str) -> bool {
    // The sound each press deserves, read from what it changed, as in the
    // launcher (`sfx::after_input`). Two changes the screen does not show by
    // itself are read here too: the editor's layer, and a refusal (A on a
    // modifier row, which used to click as if something had toggled).
    let before = TOUCHES.lock().map(|s| s.screen).ok();
    let layer_before = keymap::edit_subtab_index();
    let refused = button == "A"
        && matches!(before, Some(Screen::List { selection })
            if keymap::PAD_SLOTS.get(selection).is_some_and(|s| keymap::slot_is_modifier(s.name)));
    let consumed = input_inner(button);
    if let (Some(b), Ok(a)) = (before, TOUCHES.lock().map(|s| s.screen)) {
        use crate::sfx::{play, Sfx};
        let nav = matches!(
            button,
            "Up" | "Down" | "Left" | "Right" | "StickLUp" | "StickLDown" | "StickLLeft" | "StickLRight"
        );
        if refused {
            play(Sfx::Error);
        } else if matches!(a, Screen::Inactive) && !matches!(b, Screen::Inactive) {
            play(Sfx::Back);
        } else if matches!(button, "L" | "R") && matches!(b, Screen::List { .. }) {
            // A layer is a tab; one that did not change (the end, or a held
            // shoulder) is silent.
            if keymap::edit_subtab_index() != layer_before {
                play(Sfx::Tab);
            }
        } else if b == a {
            // Nothing moved: a value stepped in place, or a binding was set.
            let stepper = matches!(b, Screen::Menu { selection } if matches!(selection, MENU_CURSOR | MENU_SHOWCURSOR | MENU_GYRO));
            if matches!(button, "A" | "X") || (nav && stepper) {
                play(Sfx::Toggle);
            }
        } else if core::mem::discriminant(&b) == core::mem::discriminant(&a) {
            play(if nav { Sfx::Move } else { Sfx::Toggle });
        } else {
            play(if matches!(button, "B" | "Minus") { Sfx::Back } else { Sfx::Decide });
        }
    }
    consumed
}

fn input_inner(button: &str) -> bool {
    let mut s = match TOUCHES.lock() {
        Ok(g) => g,
        Err(_) => return false,
    };
    match s.screen {
        Screen::Inactive => false,
        Screen::Menu { selection } => {
            // Apply / share / revert do HTTPS + file I/O — run them WITHOUT the
            // lock held. Edit / cursor / nav are handled under the lock.
            let revert_row = if s.can_revert { MENU_REVERT } else { usize::MAX };
            if button == "A" && (selection == MENU_APPLY || selection == MENU_SHARE || selection == revert_row) {
                drop(s);
                match selection {
                    // APPLY fetches the catalog (network) → defer + loading panel.
                    MENU_APPLY => defer_net(PendingNet::OpenProfiles),
                    MENU_SHARE => {
                        // Instant "already shared" (no network) → run inline so no
                        // loading flash; else defer (fetches my shared profile).
                        let dup = game_ctx()
                            .map(|(b, _, _)| keymap::provenance(&b).starts_with("community:"))
                            .unwrap_or(false);
                        if dup {
                            run_open_share_confirm();
                        } else {
                            defer_net(PendingNet::OpenShareConfirm);
                        }
                    }
                    // REVERT preview is file I/O only (fast) — no panel needed.
                    _ => run_open_revert_preview(),
                }
                return true;
            }
            handle_menu_input(&mut s, button, selection);
            true
        }
        Screen::List { selection } => {
            // In game, B goes back to the Touches et curseur page and Y on to
            // its shared profiles. Both read the card (and Y the catalogue),
            // so they run without the lock, like that page's own rows.
            if s.submenu && matches!(button, "B" | "Minus" | "Y") {
                drop(s);
                leave_editor_in_game(button == "Y");
                return true;
            }
            handle_list_input(&mut s, button, selection);
            true
        }
        Screen::Keyboard { button_idx, key_idx } => {
            handle_keyboard_input(&mut s, button, button_idx, key_idx);
            true
        }
        Screen::Profiles { selection } => {
            // A opens the before/after preview (reads the keymap) — hoist it.
            // On the COPY FROM ANOTHER GAME row, it opens the list of the
            // player's other games instead (reads their keymaps).
            if button == "A" {
                let n = s.matches.len();
                let copy_row = s
                    .matches
                    .get(selection)
                    .is_some_and(|m| crate::profiles::is_copy_row(&m.profile.id));
                drop(s);
                if copy_row {
                    run_open_copies(selection);
                } else if selection < n {
                    run_open_preview(selection);
                }
                return true;
            }
            handle_profiles_input(&mut s, button, selection);
            true
        }
        Screen::DeleteConfirm { profile_idx, selection } => {
            // A on "Delete" deletes my shared profile (HTTPS): defer + loading
            // panel. A on "Cancel", B or Minus go back to the list.
            match button {
                "Left" | "StickLLeft" | "Right" | "StickLRight" | "Up" | "StickLUp" | "Down" | "StickLDown" => {
                    s.screen = Screen::DeleteConfirm { profile_idx, selection: 1 - selection.min(1) };
                }
                "A" if selection == 1 => {
                    drop(s);
                    defer_net(PendingNet::Delete { profile_idx });
                    return true;
                }
                "A" | "B" | "Minus" => {
                    s.screen = Screen::Profiles { selection: profile_idx };
                }
                _ => {}
            }
            true
        }
        Screen::Preview { profile_idx } => {
            if button == "A" {
                drop(s);
                defer_net(PendingNet::Apply { profile_idx });
                return true;
            }
            if matches!(button, "B" | "Minus") {
                s.preview_rows.clear();
                s.screen = Screen::Profiles { selection: profile_idx };
            }
            true
        }
        Screen::ShareConfirm => {
            if button == "A" {
                drop(s);
                defer_net(PendingNet::Share);
                return true;
            }
            if matches!(button, "B" | "Minus") {
                s.preview_rows.clear();
                s.screen = Screen::Menu { selection: MENU_SHARE };
            }
            true
        }
        Screen::RevertPreview => {
            if button == "A" {
                drop(s);
                run_revert();
                return true;
            }
            if matches!(button, "B" | "Minus") {
                s.preview_rows.clear();
                s.screen = Screen::Menu { selection: MENU_REVERT };
            }
            true
        }
        // Transient loading frame: swallow input while the deferred flow runs.
        Screen::Loading => true,
    }
}

/// Sub-menu nav + the under-lock actions (edit, cursor). Apply/share/revert are
/// hoisted in `input`.
fn handle_menu_input(s: &mut State, button: &str, mut selection: usize) {
    let last = MENU_FIXED_ROWS - 1 + s.can_revert as usize;
    // The three value rows step with Left and Right too, either way.
    let dir = match button {
        "Left" | "StickLLeft" => -1,
        "Right" | "StickLRight" => 1,
        _ => 0,
    };
    if dir != 0 {
        // Left / Right move along what the row shows: the speed one preset
        // down or up, the switch to off or on, the gyroscope one setting along,
        // each stopping at its end. A is what goes round.
        match selection {
            MENU_CURSOR => {
                let m = unsafe { ruffle_cursor_speed_mult_x10() }.max(0) as u32;
                let at = crate::loc::CURSOR_SPEED_X10.iter().position(|v| *v == m);
                let n = crate::loc::CURSOR_SPEED_X10.len();
                let blocked = match at {
                    Some(0) => dir < 0,
                    Some(i) => dir > 0 && i + 1 >= n,
                    None => false,
                };
                if !blocked {
                    unsafe { ruffle_cursor_speed_step(dir) };
                }
            }
            MENU_SHOWCURSOR => {
                if keymap::show_cursor() != (dir > 0) {
                    keymap::toggle_show_cursor();
                }
            }
            MENU_GYRO => {
                let cur = keymap::gyro_mode();
                if (dir < 0 && cur > 0) || (dir > 0 && cur + 1 < keymap::GYRO_MODE_COUNT) {
                    keymap::step_gyro(dir);
                }
            }
            _ => {}
        }
        s.screen = Screen::Menu { selection };
        return;
    }
    match button {
        // A fresh press wraps round, a held direction stops at the end, as on
        // every list of the launcher.
        "Up" | "StickLUp" => {
            selection = crate::library::list_step_wrap(selection, -1, last + 1);
        }
        "Down" | "StickLDown" => {
            selection = crate::library::list_step_wrap(selection, 1, last + 1);
        }
        "A" => match selection {
            MENU_EDIT => {
                // Enter the editor, KEEPING sub-menu context (B returns here).
                keymap::set_edit_player(1);
                keymap::reset_edit_subtabs(); // both players start on NORMAL (#57)
                s.screen = Screen::List { selection: unstick_cursor(0) };
                return;
            }
            MENU_CURSOR => {
                // Cycle the live per-game cursor speed (C++ applies + persists).
                unsafe { ruffle_cursor_speed_step(1) };
            }
            MENU_SHOWCURSOR => {
                // Toggle the per-game pointer visibility (persists in the keymap).
                keymap::toggle_show_cursor();
            }
            MENU_GYRO => {
                // Per-game gyroscope cursor (#119), OFF -> MARBLE -> POINTER;
                // the C++ loop picks it up on the next frame.
                keymap::cycle_gyro();
            }
            _ => {}
        },
        "B" | "Minus" => {
            s.screen = Screen::Inactive; // C++ returns to the pause main menu
            return;
        }
        _ => {}
    }
    s.screen = Screen::Menu { selection };
}

/// Picker nav (A is hoisted to open the preview).
fn handle_profiles_input(s: &mut State, button: &str, mut selection: usize) {
    let total = s.matches.len().max(1);
    match button {
        "Up" | "StickLUp" => selection = crate::library::list_step_wrap(selection, -1, total),
        "Down" | "StickLDown" => selection = crate::library::list_step_wrap(selection, 1, total),
        "X" => {
            // Delete MY OWN shared profile: only on a row this install shared.
            if s
                .matches
                .get(selection)
                .is_some_and(|m| crate::profiles::is_mine(&m.profile.id))
            {
                s.screen = Screen::DeleteConfirm { profile_idx: selection, selection: 0 };
                return;
            }
        }
        "B" | "Minus" => {
            // From the copy list, back to the community list, on the copy row.
            if s.copy_mode {
                s.copy_mode = false;
                s.matches = std::mem::take(&mut s.saved_matches);
                s.screen = Screen::Profiles { selection: s.matches.len().max(1) - 1 };
                return;
            }
            s.screen = Screen::Menu { selection: MENU_APPLY };
            return;
        }
        _ => {}
    }
    s.screen = Screen::Profiles { selection };
}

/// COPY FROM ANOTHER GAME: list the player's other games with controls of
/// their own, in place of the community profiles. `row` is the copy row, where
/// the cursor stays when there is nothing to copy.
fn run_open_copies(row: usize) {
    let Some((basename, title, _)) = game_ctx() else { return };
    let copies = crate::profiles::local_copies(&basename, &title);
    crate::net::log(&std::format!(
        "menu: in-game COPY FROM ANOTHER GAME '{}' -> {} game(s)\n",
        title,
        copies.len(),
    ));
    if let Ok(mut s) = TOUCHES.lock() {
        if copies.is_empty() {
            set_toast(&mut s, crate::loc::s().profile_copy_none.to_string(), TOAST_INFO);
            s.screen = Screen::Profiles { selection: row };
            return;
        }
        s.saved_matches = std::mem::replace(&mut s.matches, copies);
        s.copy_mode = true;
        s.screen = Screen::Profiles { selection: 0 };
    }
}

// ── In-game profile actions (run WITHOUT the TOUCHES lock held) ─────────────

/// APPLIQUER: fetch matching profiles for the running game + open the picker.
fn run_open_profiles() {
    let Some((basename, title, path)) = game_ctx() else { return };
    let swf_hash = crate::profiles::swf_hash_of(&path).unwrap_or_default();
    let matches = crate::profiles::all_matches_for("", &swf_hash, &title);
    // Active = the profile whose bindings match the current keymap (content-based,
    // so it's right regardless of the provenance tag), with the tag as fallback.
    let current = keymap::effective_for(&basename);
    let prov_id = keymap::provenance(&basename)
        .strip_prefix("community:")
        .unwrap_or("")
        .to_string();
    let active_id = matches
        .iter()
        .find(|m| {
            keymap::binding_diff_rows(&m.profile.to_keymap(), &current).is_empty()
                || (!prov_id.is_empty() && m.profile.id == prov_id)
        })
        .map(|m| m.profile.id.clone())
        .unwrap_or_default();
    crate::net::log(&std::format!(
        "menu: in-game APPLIQUER '{}' hash={} -> {} match(es)\n",
        title, swf_hash, matches.len(),
    ));
    // The list never comes up empty any more (the copy row ends it), so the
    // "no profile" / "offline" notice it used to show becomes a toast.
    let notice = if matches.is_empty() {
        Some(if crate::profiles::catalog_unavailable() {
            crate::loc::s().profile_catalog_offline
        } else {
            crate::loc::s().profile_none
        })
    } else {
        None
    };
    if let Ok(mut s) = TOUCHES.lock() {
        s.matches = crate::profiles::with_copy_row(matches);
        s.copy_mode = false;
        s.saved_matches.clear();
        s.active_id = active_id;
        if let Some(msg) = notice {
            set_toast(&mut s, msg.to_string(), TOAST_INFO);
        }
        s.screen = Screen::Profiles { selection: 0 };
    }
}

/// A on a profile row: build the before/after diff and open the preview.
fn run_open_preview(profile_idx: usize) {
    let Some((basename, _, _)) = game_ctx() else { return };
    let profile = match TOUCHES.lock() {
        Ok(g) => g.matches.get(profile_idx).map(|m| m.profile.clone()),
        Err(_) => None,
    };
    let Some(profile) = profile else { return };
    let current = keymap::effective_for(&basename);
    let mut rows = keymap::binding_diff_rows(&current, &profile.to_keymap());
    if let Some(r) = keymap::cursor_diff_row(keymap::cursor_speed_for(&basename), profile.cursor_speed) {
        rows.push(r);
    }
    if rows.is_empty() {
        if let Ok(mut s) = TOUCHES.lock() {
            set_toast(&mut s, crate::loc::s().profile_preview_none.to_string(), TOAST_INFO);
            s.screen = Screen::Profiles { selection: profile_idx };
        }
        return;
    }
    if let Ok(mut s) = TOUCHES.lock() {
        s.preview_rows = cap_rows(rows);
        s.screen = Screen::Preview { profile_idx };
    }
}

/// A on the preview: apply the profile (non-destructive), refresh live bindings.
fn run_apply(profile_idx: usize) {
    let Some((basename, _, _)) = game_ctx() else { return };
    let profile = match TOUCHES.lock() {
        Ok(g) => g.matches.get(profile_idx).map(|m| m.profile.clone()),
        Err(_) => None,
    };
    let Some(profile) = profile else { return };
    let ok = crate::profiles::apply(&basename, &profile);
    if ok {
        crate::profiles::record_applied(&profile.id);
        // apply_keymap clears ACTIVE_KEYMAP (it expects a relaunch to reload it).
        // In-game we DON'T relaunch, so reload now — otherwise the editor reads a
        // None keymap (AUCUNE everywhere) and the live controls go dead. #20.
        keymap::init_for_swf(&basename);
    }
    let lc = crate::loc::s();
    let msg = if ok { lc.profile_applied_ok } else { lc.bug_fail_title };
    let can_revert = keymap::has_revert(&basename);
    let has_backup = keymap::has_backup(&basename);
    if let Ok(mut s) = TOUCHES.lock() {
        // dirty → C++ repopulates BINDINGS so the new controls take effect now.
        s.dirty = ok;
        s.can_revert = can_revert;
        s.has_backup = has_backup;
        set_toast(&mut s, msg.to_string(), if ok { TOAST_OK } else { TOAST_ERR });
        s.screen = Screen::Menu { selection: MENU_APPLY };
    }
}

/// PARTAGER: open the share confirm with a before/after of my shared profile.
fn run_open_share_confirm() {
    let Some((basename, title, path)) = game_ctx() else { return };
    // Already a catalog profile, unchanged → nothing to share until edited.
    if keymap::provenance(&basename).starts_with("community:") {
        if let Ok(mut s) = TOUCHES.lock() {
            set_toast(&mut s, crate::loc::s().profile_share_dup.to_string(), TOAST_INFO);
            s.screen = Screen::Menu { selection: MENU_SHARE };
        }
        return;
    }
    let current = keymap::effective_for(&basename);
    let swf_hash = crate::profiles::swf_hash_of(&path).unwrap_or_default();
    let suffix = std::format!("-{}", crate::profiles::install_id());
    let mine = crate::profiles::all_matches_for("", &swf_hash, &title)
        .into_iter()
        .find(|m| m.profile.id.ends_with(&suffix));
    let is_update = mine.is_some();
    let before_cursor = mine.as_ref().map(|m| m.profile.cursor_speed).unwrap_or(-1);
    // "before" = the profile we'd update, else ~the default (first share). As a
    // full Keymap so base + combos + modifier all diff (#40/#57).
    let before = match mine {
        Some(m) => m.profile.to_keymap(),
        None => keymap::revert_target(&basename), // ~default for a first share
    };
    let mut rows = keymap::binding_diff_rows(&before, &current);
    // Cursor speed lives outside the keymap — append its change so it's visible.
    if let Some(r) = keymap::cursor_diff_row(before_cursor, keymap::cursor_speed_for(&basename)) {
        rows.push(r);
    }
    let rows = cap_rows(rows);
    if let Ok(mut s) = TOUCHES.lock() {
        s.preview_rows = rows;
        s.share_is_update = is_update;
        s.screen = Screen::ShareConfirm;
    }
}

/// A on the share confirm: POST the controls as a community profile.
fn run_share() {
    let Some((basename, title, path)) = game_ctx() else { return };
    let km = keymap::effective_for(&basename);
    let swf_hash = crate::profiles::swf_hash_of(&path).unwrap_or_default();
    let cursor_speed = keymap::cursor_speed_for(&basename);
    let lc = crate::loc::s();
    let (msg, kind) = match crate::profiles::share(&title, "", &swf_hash, &km, cursor_speed) {
        Ok(id) => {
            keymap::mark_shared(&basename, &id);
            crate::profiles::invalidate_online_cache();
            (lc.profile_shared_ok.to_string(), TOAST_OK)
        }
        Err(e) => (e, TOAST_ERR),
    };
    let can_revert = keymap::has_revert(&basename);
    let has_backup = keymap::has_backup(&basename);
    if let Ok(mut s) = TOUCHES.lock() {
        s.can_revert = can_revert;
        s.has_backup = has_backup;
        set_toast(&mut s, msg, kind);
        s.screen = Screen::Menu { selection: MENU_SHARE };
    }
}

/// X-on-confirm: DELETE one of my own shared profiles via the relay (#20).
/// Hoisted (HTTPS POST). Toasts the result and drops the row from the picker on
/// success. Ownership is enforced server-side by the install's owner token.
fn run_delete(profile_idx: usize) {
    let id = match TOUCHES.lock() {
        Ok(g) => g.matches.get(profile_idx).map(|m| m.profile.id.clone()),
        Err(_) => None,
    };
    let Some(id) = id else {
        return;
    };
    let lc = crate::loc::s();
    let (msg, kind, ok) = match crate::profiles::delete(&id) {
        Ok(()) => (lc.profile_del_ok.to_string(), TOAST_OK, true),
        Err(e) => (e, TOAST_ERR, false),
    };
    if ok {
        // No longer in the catalog → demote this game's keymap to "user" if it was
        // tagged with the deleted profile, so SHARE stops saying "already exists".
        if let Some((basename, _, _)) = game_ctx() {
            keymap::unmark_shared(&basename, &id);
        }
    }
    if let Ok(mut s) = TOUCHES.lock() {
        if ok {
            s.matches.retain(|m| m.profile.id != id);
        }
        let n = s.matches.len();
        let selection = if n == 0 { 0 } else { profile_idx.min(n - 1) };
        set_toast(&mut s, msg, kind);
        s.screen = Screen::Profiles { selection };
    }
}

/// REVENIR: open the before/after preview of a revert.
fn run_open_revert_preview() {
    let Some((basename, _, _)) = game_ctx() else { return };
    let current = keymap::effective_for(&basename);
    let target = keymap::revert_target(&basename);
    let rows = cap_rows(keymap::binding_diff_rows(&current, &target));
    if let Ok(mut s) = TOUCHES.lock() {
        s.preview_rows = rows;
        s.screen = Screen::RevertPreview;
    }
}

/// A on the revert preview: revert (restore backup or reset to default).
fn run_revert() {
    let Some((basename, _, _)) = game_ctx() else { return };
    let ok = keymap::revert_profile(&basename);
    if ok {
        // revert_profile also clears ACTIVE_KEYMAP — reload it in-game (see run_apply).
        keymap::init_for_swf(&basename);
    }
    let lc = crate::loc::s();
    let msg = if ok { lc.profile_reverted_ok } else { lc.bug_fail_title };
    let can_revert = keymap::has_revert(&basename);
    let has_backup = keymap::has_backup(&basename);
    if let Ok(mut s) = TOUCHES.lock() {
        s.dirty = ok; // refresh live bindings
        s.can_revert = can_revert;
        s.has_backup = has_backup;
        set_toast(&mut s, msg.to_string(), if ok { TOAST_OK } else { TOAST_ERR });
        s.screen = Screen::Menu { selection: MENU_EDIT };
    }
}

/// Direction of one cursor step on the pad.
#[derive(Clone, Copy, PartialEq, Eq)]
enum PadDir {
    Up,
    Down,
    Left,
    Right,
}

/// Centre of slot `i`'s value chip, in pad units.
///
/// `.get`, not `[..]`: with `panic = "abort"` an out-of-range index is a console
/// fatal, and a cursor reaches here from a touch table that outlives its panel.
fn pad_center(i: usize) -> (f32, f32) {
    keymap::PAD_SLOTS
        .get(i)
        .map(|s| (s.chip.0 + s.chip.2 * 0.5, s.chip.1 + s.chip.3 * 0.5))
        .unwrap_or((0.0, 0.0))
}

/// Move the pad cursor one step, on the chips' own centres.
///
/// Geometric rather than an index step, because the chips are two columns and
/// what the player means by Right is "the other hand, same height" — which an
/// index step would turn into "thirteen rows down". It also means the table in
/// keymap.rs can be re-laid-out without a matching edit here.
///
/// `wrap`: whether Up and Down may come round at the end of a column.
fn pad_step(cur: usize, dir: PadDir, wrap: bool) -> usize {
    let n = keymap::PAD_SLOTS.len();
    if n == 0 {
        return cur;
    }
    let (cx, cy) = pad_center(cur);
    // Same column = same chip x. The two columns are thirty-two units apart, so
    // half a unit of tolerance is generous and still cannot confuse them.
    let same_col = |x: f32| (x - cx).abs() < 0.5;
    let mut best: Option<(usize, f32)> = None;
    for i in 0..n {
        if i == cur || slot_locked(i) {
            continue;
        }
        let (x, y) = pad_center(i);
        let cost = match dir {
            PadDir::Up if same_col(x) && y < cy - 0.01 => cy - y,
            PadDir::Down if same_col(x) && y > cy + 0.01 => y - cy,
            // Across columns, the chip at the nearest HEIGHT; the x distance is
            // only a tie-break, and with two columns there is never a tie.
            PadDir::Left if x < cx - 0.5 => (y - cy).abs() * 100.0 + (cx - x),
            PadDir::Right if x > cx + 0.5 => (y - cy).abs() * 100.0 + (x - cx),
            _ => continue,
        };
        if best.map_or(true, |(_, b)| cost < b) {
            best = Some((i, cost));
        }
    }
    if let Some((i, _)) = best {
        return i;
    }
    // Nothing that way. Up and Down come round inside their own column on a
    // fresh press, and stop at the end under a held direction, as every list
    // of the app does (`library::nav_may_wrap`): holding Down used to circle a
    // column every 80 ms. Left and Right at the outer edge stay put, there is
    // no third column to wrap to.
    let want_max = match dir {
        PadDir::Up => true,
        PadDir::Down => false,
        _ => return cur,
    };
    if !wrap {
        return cur;
    }
    let mut far = cur;
    let mut edge = cy;
    for i in 0..n {
        if slot_locked(i) {
            continue;
        }
        let (x, y) = pad_center(i);
        if same_col(x) && ((want_max && y > edge) || (!want_max && y < edge)) {
            edge = y;
            far = i;
        }
    }
    far
}

/// Whether slot `i` is a modifier in the open layer, and so takes no cursor.
fn slot_locked(i: usize) -> bool {
    keymap::PAD_SLOTS
        .get(i)
        .map(|s| keymap::slot_is_modifier(s.name))
        .unwrap_or(false)
}

/// Put the cursor on a row that can actually be edited.
///
/// Called after anything that changes which rows are locked: switching layer
/// with L/R, switching player with X, and binding a key (the binding that makes
/// ZL a modifier is what locks ZL everywhere else). Without it the cursor sits
/// on a row it can no longer open, and A does nothing with no explanation.
fn unstick_cursor(mut selection: usize) -> usize {
    let n = keymap::PAD_SLOTS.len();
    if n == 0 || !slot_locked(selection) {
        return selection;
    }
    // Bounded: at most four buttons can ever be modifiers, so a lap of the
    // column always finds a free row -- but the loop is capped anyway rather
    // than trusting that to stay true.
    for _ in 0..n {
        let next = pad_step(selection, PadDir::Down, true);
        if next == selection {
            break;
        }
        selection = next;
        if !slot_locked(selection) {
            return selection;
        }
    }
    (0..n).find(|&i| !slot_locked(i)).unwrap_or(selection)
}

fn handle_list_input(s: &mut State, button: &str, mut selection: usize) {
    let wrap = crate::library::nav_may_wrap();
    match button {
        "Up" | "StickLUp" => selection = pad_step(selection, PadDir::Up, wrap),
        "Down" | "StickLDown" => selection = pad_step(selection, PadDir::Down, wrap),
        // Left and Right finally mean something on this screen: the pad has two
        // columns, and they cross to the other hand at the same height. The
        // SHOULDER L/R still change the combo layer, as they always did.
        "Left" | "StickLLeft" => selection = pad_step(selection, PadDir::Left, wrap),
        "Right" | "StickLRight" => selection = pad_step(selection, PadDir::Right, wrap),
        // Player toggle (issue #40): X flips P1 <-> P2 (2 items, a toggle is fine).
        // P1 and P2 have their OWN combo layers, so the swap can lock or free the
        // row under the cursor.
        "X" => {
            keymap::set_edit_player(if keymap::edit_player() == 2 { 1 } else { 2 });
            selection = unstick_cursor(selection);
        }
        // Combo sub-tab (issue #57, per-modifier): L/R move the VIEW along
        // [NORMAL, ZL, ZR, L, R]. NORMAL edits base bindings; a modifier position
        // edits THAT modifier's own combo layer. Pure view — a modifier becomes
        // active in-game as soon as its layer gets a binding (no separate toggle).
        "L" | "R" => {
            // One layer per press: the shoulders repeat when held (the
            // launcher pages long lists with them), and held here they ran
            // from Normal to R in half a second.
            if !wrap {
                return;
            }
            let cur = keymap::edit_subtab_index();
            let new = if button == "R" {
                (cur + 1).min(keymap::SUBTAB_MODS.len() - 1)
            } else {
                cur.saturating_sub(1)
            };
            if new != cur {
                keymap::set_edit_subtab_index(new);
                // The new layer's own modifier is not bindable in it, and the
                // cursor may be sitting exactly there.
                selection = unstick_cursor(selection);
            }
        }
        "A" => {
            // Open the visual keyboard at the button's current key (or "(none)").
            // `.get`, not `[..]`: with `panic = "abort"` an out-of-range row here
            // is a console fatal, and this index arrives from a cursor several
            // screens and one touch table away from where it is bounded.
            let Some(slot) = keymap::PAD_SLOTS.get(selection) else {
                return;
            };
            // Reachable even though the cursor skips locked rows: a tap, or a
            // layer switched while the cursor sat here, can put it on one. Say
            // what the button does now and how to get it back, not just a word.
            if keymap::slot_is_modifier(slot.name) {
                set_toast(s, crate::loc_ui::ui().toast_modifier_locked.replace("{}", slot.name), TOAST_INFO);
                s.screen = Screen::List { selection };
                return;
            }
            let key_idx = keymap::current_binding(slot.name)
                .as_deref()
                .and_then(kbd_index_of)
                .unwrap_or_else(kbd_none_index);
            s.screen = Screen::Keyboard { button_idx: selection, key_idx };
            return;
        }
        "B" | "Minus" => {
            // The launcher's editor closes; the launcher puts its page back
            // (`library::input`). In game B never reaches here: it is
            // `leave_editor_in_game`.
            s.screen = Screen::Inactive;
            return;
        }
        _ => {}
    }
    s.screen = Screen::List { selection };
}

/// Leave the in-game editor for the Touches et curseur page (B), or for its
/// shared profiles (Y, the mockup's shortcut; B there comes back to the page).
/// The page's "Revenir…" row depends on the keys just edited, so it is worked
/// out again here, as on the launcher side: it used to stay as it was when the
/// page opened, missing after a first edit and stale once a key was set back.
fn leave_editor_in_game(profiles: bool) {
    let (can_revert, has_backup) = keymap::active_game_basename()
        .map(|b| (keymap::has_revert(&b), keymap::has_backup(&b)))
        .unwrap_or((false, false));
    if let Ok(mut s) = TOUCHES.lock() {
        s.can_revert = can_revert;
        s.has_backup = has_backup;
        s.screen = Screen::Menu { selection: if profiles { MENU_APPLY } else { MENU_EDIT } };
    }
    if profiles {
        defer_net(PendingNet::OpenProfiles);
    }
}

/// Where the editor sits and what it edits, as its header and footer say.
struct Place {
    /// The page it was opened from, under the page that holds that page.
    crumbs: [&'static str; 2],
    subject: std::string::String,
    /// The cursor speed in force, x10. None for the defaults of every game,
    /// whose cursor speed is a row of their own window.
    speed_x10: Option<u32>,
    /// Whether Y opens the shared profiles: they are a game's.
    profiles: bool,
}

/// The editor's three doors: the pause (Pause › Touches et curseur), a game's
/// options in the launcher (Options › Touches et curseur), RÉGLAGES for the
/// defaults of every game. B goes back through the same door.
fn editor_place() -> Option<Place> {
    let (submenu, subject, speed) = TOUCHES
        .lock()
        .ok()
        .map(|s| (s.submenu, s.subject.clone(), s.subject_speed_x10))?;
    let lc = crate::loc::s();
    let u = crate::loc_ui::ui();
    Some(if submenu {
        Place {
            crumbs: [lc.pause_title, lc.menu_keys],
            subject: crate::library::active_display_name().unwrap_or_default(),
            speed_x10: Some(unsafe { ruffle_cursor_speed_mult_x10() }.max(5) as u32),
            profiles: true,
        }
    } else if let Some(subject) = subject {
        Place { crumbs: [u.hint_options, lc.menu_keys], subject, speed_x10: Some(speed), profiles: true }
    } else {
        Place {
            crumbs: [u.tab_settings, u.row_game_prefs],
            subject: u.keys_all_games.to_string(),
            speed_x10: None,
            profiles: false,
        }
    })
}

/// What each pad row shows, in `PAD_SLOTS` order: what the button really
/// sends in the open layer.
fn pad_values() -> std::vec::Vec<PadValue> {
    let normal = keymap::edit_subtab_index() == 0;
    keymap::PAD_SLOTS
        .iter()
        .map(|slot| {
            if keymap::slot_is_modifier(slot.name) {
                return PadValue::Modifier;
            }
            match keymap::current_binding(slot.name) {
                Some(k) => PadValue::Key(k),
                None if normal => PadValue::None,
                // The layer does not map it, so while its modifier is held it
                // sends its Normal key (`lookup_combo` falls through). It used
                // to say "(aucune)" there, and ZL+A looked dead while it sent
                // Espace.
                None => PadValue::Inherited(keymap::base_binding(slot.name)),
            }
        })
        .collect()
}

/// The layer tabs that carry a dot: the combo layers holding a key, so a
/// layer in use shows from any tab.
fn live_layers(player: u8) -> [bool; 5] {
    let mut live = [false; 5];
    for (i, m) in keymap::SUBTAB_MODS.iter().enumerate().skip(1) {
        live[i] = if player == 2 { keymap::combo_active_p2(m) } else { keymap::combo_active(m) };
    }
    live
}

/// Whether `stick`'s four direction rows read as one "mouse cursor": player
/// 1's stick sends nothing in any of them, which is when it moves the cursor
/// (`keymap::stick_drives_cursor`), in a combo layer too as long as the layer
/// maps none of them. Player 2 has no cursor.
fn stick_reads_as_cursor(values: &[PadValue], player: u8, stick: char) -> bool {
    player == 1
        && keymap::PAD_SLOTS
            .iter()
            .zip(values)
            .filter(|(slot, _)| keymap::stick_of_direction(slot.name) == Some(stick))
            .all(|(_, v)| matches!(v, PadValue::None | PadValue::Inherited(None)))
}

/// The line under the pad about the focused row: what is particular to it,
/// else what the open layer means, else what the screen is for.
fn pad_help(selection: usize, values: &[PadValue], player: u8, layer: usize) -> std::string::String {
    let u = crate::loc_ui::ui();
    let Some(slot) = keymap::PAD_SLOTS.get(selection) else {
        return std::string::String::new();
    };
    if matches!(slot.name, "SL" | "SR") {
        return u.help_pad_slsr.to_string();
    }
    if player == 1 && layer == 0 && keymap::stick_of_direction(slot.name).is_some() {
        return u.help_pad_stick.to_string();
    }
    if matches!(values.get(selection), Some(PadValue::Key(k)) | Some(PadValue::Inherited(Some(k))) if k == "Keyboard") {
        return u.help_key_keyboard.to_string();
    }
    if layer > 0 {
        return u.help_pad_layer.replace("{}", keymap::SUBTAB_MODS[layer.min(keymap::SUBTAB_MODS.len() - 1)]);
    }
    u.help_touches_edit.to_string()
}

/// Index of a Flash-key NAME in `keymap::KEYBOARD`, or None if it isn't on the
/// board. Used to open the picker with the cursor on the button's current binding.
fn kbd_index_of(name: &str) -> Option<usize> {
    keymap::KEYBOARD.iter().position(|k| k.0 == name)
}

/// Index of the "(none)" unbind key — the picker's default cursor for an unbound
/// button.
fn kbd_none_index() -> usize {
    kbd_index_of("(none)").unwrap_or(0)
}

/// Horizontal centre (in layout units) of key `i`, for geometric navigation.
fn kbd_center(i: usize) -> f32 {
    keymap::KEYBOARD.get(i).map(kbd_center_of).unwrap_or(0.0)
}

/// The key beside key `cur` on its row, `dir` -1 for Left and 1 for Right.
/// Past the end of the row, a fresh press comes round to its other end and a
/// held one stops (`library::nav_may_wrap`): held, it used to circle the row.
fn kbd_horizontal(cur: usize, dir: i32, wrap: bool) -> usize {
    let Some(&(_, row, _, _)) = keymap::KEYBOARD.get(cur) else { return cur };
    let cx = kbd_center(cur);
    let on_row = || keymap::KEYBOARD.iter().enumerate().filter(move |(_, k)| k.1 == row);
    let by_centre = |a: &(usize, &(&str, u8, f32, f32)), b: &(usize, &(&str, u8, f32, f32))| {
        kbd_center_of(a.1).total_cmp(&kbd_center_of(b.1))
    };
    let next = if dir < 0 {
        on_row().filter(|(_, k)| kbd_center_of(k) < cx - 0.01).max_by(by_centre)
    } else {
        on_row().filter(|(_, k)| kbd_center_of(k) > cx + 0.01).min_by(by_centre)
    };
    match next {
        Some((i, _)) => i,
        None if wrap => {
            let far = if dir < 0 { on_row().max_by(by_centre) } else { on_row().min_by(by_centre) };
            far.map_or(cur, |(i, _)| i)
        }
        None => cur,
    }
}

/// The key above (`dir` -1) or below (1) key `cur`: on the nearest row with a
/// key under the cursor, the key whose span is closest to the cursor's centre.
/// A row with nothing there (the space under the numpad, above it) is passed
/// over rather than landed on sideways. A fresh press comes round from the
/// last row to the first, and when no row has anything under the cursor it
/// takes the next row's nearest key; a held one stops.
fn kbd_vertical(cur: usize, dir: i32, wrap: bool) -> usize {
    let Some(&(_, row, _, _)) = keymap::KEYBOARD.get(cur) else { return cur };
    let cx = kbd_center(cur);
    let n = keymap::KEYBOARD_ROWS_N as i32;
    // 0 inside the key's span, else how far outside it the centre falls.
    let gap = |k: &(&str, u8, f32, f32)| -> f32 {
        if cx < k.2 {
            k.2 - cx
        } else if cx > k.2 + k.3 {
            cx - (k.2 + k.3)
        } else {
            0.0
        }
    };
    let best_on = |r: i32| -> Option<(usize, f32)> {
        keymap::KEYBOARD
            .iter()
            .enumerate()
            .filter(|(_, k)| k.1 as i32 == r)
            .map(|(i, k)| (i, gap(k), (kbd_center_of(k) - cx).abs()))
            .min_by(|a, b| a.1.total_cmp(&b.1).then(a.2.total_cmp(&b.2)))
            .map(|(i, g, _)| (i, g))
    };
    let mut nearest = None;
    let mut r = row as i32;
    for _ in 1..n {
        r += dir;
        if r < 0 || r >= n {
            if !wrap {
                break;
            }
            r = r.rem_euclid(n);
        }
        if let Some((i, g)) = best_on(r) {
            if nearest.is_none() {
                nearest = Some(i);
            }
            // Under the cursor, or within half a key of it.
            if g <= 0.5 {
                return i;
            }
        }
    }
    if wrap {
        nearest.unwrap_or(cur)
    } else {
        cur
    }
}

fn handle_keyboard_input(
    s: &mut State,
    button: &str,
    button_idx: usize,
    mut key_idx: usize,
) {
    let Some(&(name, _, _, _)) = keymap::KEYBOARD.get(key_idx) else { return };
    let wrap = crate::library::nav_may_wrap();
    // Left / Right along the row, Up / Down to the key under the cursor on the
    // next row that has one, so the numpad column stays put.
    match button {
        "Left" | "StickLLeft" => key_idx = kbd_horizontal(key_idx, -1, wrap),
        "Right" | "StickLRight" => key_idx = kbd_horizontal(key_idx, 1, wrap),
        "Up" | "StickLUp" => key_idx = kbd_vertical(key_idx, -1, wrap),
        "Down" | "StickLDown" => key_idx = kbd_vertical(key_idx, 1, wrap),
        "A" | "X" => {
            // "(none)" empties the button, and so does X from anywhere on the
            // board, without walking down to it; every other key is a real
            // flash-key name. Emptied in a combo layer, a button sends its
            // Normal key again (`keymap::set_binding`).
            let target = if button == "X" || name == "(none)" { None } else { Some(name) };
            let Some(edit_btn) = keymap::PAD_SLOTS.get(button_idx).map(|s| s.name) else {
                return;
            };
            // Giving a key to the first direction of a stick that moved the
            // cursor turns the whole stick into a d-pad (`main.cpp`): the
            // cursor stops following it and its other three directions send
            // nothing. That is said once, when it happens.
            let stick = keymap::stick_of_direction(edit_btn)
                .filter(|_| keymap::edit_player() == 1 && keymap::edit_subtab_index() == 0);
            let was_cursor = stick.is_some_and(keymap::stick_drives_cursor);
            if keymap::set_binding(edit_btn, target) {
                s.dirty = true;
                if let Some(st) = stick.filter(|st| was_cursor && !keymap::stick_drives_cursor(*st)) {
                    let u = crate::loc_ui::ui();
                    let msg = if st == 'R' { u.toast_stick_dpad_r } else { u.toast_stick_dpad_l };
                    set_toast(s, msg.to_string(), TOAST_INFO);
                }
            } else {
                // `set_binding` has already put the in-memory keymap back, so the
                // list below is truthful again — but nothing on screen would have
                // said the rebind did not take: no toast, no `dirty`, and the game
                // silently keeps the old key.
                set_toast(s, crate::loc::s().err_sd_write.to_string(), TOAST_ERR);
            }
            // The binding just made may have turned some button into a modifier
            // (the FIRST binding in a layer does), which locks that button's row
            // in every layer -- possibly the one we are returning to.
            s.screen = Screen::List { selection: unstick_cursor(button_idx) };
            return;
        }
        "B" | "Minus" => {
            s.screen = Screen::List { selection: unstick_cursor(button_idx) };
            return;
        }
        _ => {}
    }
    s.screen = Screen::Keyboard { button_idx, key_idx };
}

/// Centre (units) of a `keymap::KEYBOARD` tuple `(name, row, x, w)`.
fn kbd_center_of(k: &(&str, u8, f32, f32)) -> f32 {
    k.2 + k.3 * 0.5
}

/// A profile's name as the lists show it: its title, then who shared it.
pub(crate) fn profile_name(p: &crate::profiles::Profile) -> std::string::String {
    let mut r = p.title().to_string();
    if !p.author.is_empty() {
        r.push_str("  \u{00B7}  ");
        r.push_str(&p.author);
    }
    r
}

/// What the Touches et curseur page shows, wherever it is opened from: the
/// pause (this module's state) or a game's options in the launcher (the
/// library's). One page, two doors.
pub(crate) struct TouchesView {
    /// The cursor speed in force, x10 (a game with none of its own follows the
    /// global one, and shows it).
    pub speed_x10: u32,
    pub shown: bool,
    pub gyro: u8,
    pub can_revert: bool,
    pub has_backup: bool,
}

/// The Touches et curseur page as data, handed to `f`.
///
/// Two groups under their titles: the keys (edit them, take someone else's,
/// share yours) and the cursor (speed, shown or hidden, the gyroscope). Each
/// row shows what it does before it is pressed: a chevron where A opens a
/// page, the speed between arrows, a switch, the gyroscope's three settings
/// all on show.
pub(crate) fn with_touches_view<R>(
    backend: &mut SwitchRenderBackend,
    v: &TouchesView,
    selection: usize,
    crumbs: &[&str],
    f: impl FnOnce(&mut SwitchRenderBackend, &crate::backend::render::Sheet) -> R,
) -> R {
    use crate::backend::render::{Btn, Icon, Row, Sheet};
    let lc = crate::loc::s();
    let u = crate::loc_ui::ui();
    let speed = crate::loc::mult_label(v.speed_x10);
    let gyro_opts = [
        crate::loc::gyro_mode_label(keymap::GYRO_OFF),
        lc.gyro_marble,
        lc.gyro_pointer,
    ];
    let mut rows = std::vec![
        Row::new(Icon::Keyboard, lc.touches_edit).opens(),
        Row::new(Icon::Users, lc.opt_apply).opens(),
        Row::new(Icon::Share, lc.opt_share).opens(),
        Row::new(Icon::Gauge, lc.set_cursor_speed).cycle(&speed),
        Row::new(Icon::Pointer, lc.show_cursor).toggle(v.shown),
        Row::new(Icon::Compass, lc.set_gyro).choice(&gyro_opts, v.gyro as usize),
    ];
    if v.can_revert {
        rows.push(Row::new(Icon::Undo, if v.has_backup { lc.profile_revert } else { lc.touches_revert_default }).opens());
    }
    let help = match selection {
        MENU_EDIT => u.help_touches_edit,
        MENU_APPLY => u.help_touches_apply,
        MENU_SHARE => u.help_touches_share,
        MENU_CURSOR => u.help_cursor_speed,
        MENU_SHOWCURSOR => u.help_show_cursor,
        MENU_GYRO => u.help_gyro,
        _ => u.help_revert,
    };
    let stepper = matches!(selection, MENU_CURSOR | MENU_SHOWCURSOR | MENU_GYRO);
    let hints_step = [(&[Btn::Dpad][..], u.change), (&[Btn::B][..], u.back)];
    let hints_act = [(&[Btn::A][..], u.open), (&[Btn::B][..], u.back)];
    let sections = [(MENU_EDIT, lc.keys_title), (MENU_CURSOR, u.sec_cursor)];
    let mut sheet = Sheet::new(lc.menu_keys, &rows, selection);
    sheet.kicker = Some(Icon::ChevronLeft);
    sheet.crumbs = crumbs;
    sheet.sections = &sections;
    sheet.row_h = 48.0;
    sheet.glide_key = GLIDE_KEY_TOUCHES_MENU;
    sheet.help = help;
    sheet.hints = if stepper { &hints_step } else { &hints_act };
    f(backend, &sheet)
}

/// The in-game Touches et curseur page (beside the pause).
pub(crate) fn with_touches_sheet<R>(
    backend: &mut SwitchRenderBackend,
    selection: usize,
    f: impl FnOnce(&mut SwitchRenderBackend, &crate::backend::render::Sheet) -> R,
) -> R {
    let (can_revert, has_backup) = TOUCHES.lock().map(|s| (s.can_revert, s.has_backup)).unwrap_or((false, false));
    let v = TouchesView {
        speed_x10: unsafe { ruffle_cursor_speed_mult_x10() }.max(0) as u32,
        shown: keymap::show_cursor(),
        gyro: keymap::gyro_mode(),
        can_revert,
        has_backup,
    };
    let crumbs = [crate::loc::s().pause_title];
    with_touches_view(backend, &v, selection, &crumbs, f)
}

/// One shared profile as the list shows it.
pub(crate) struct ProfileItem {
    pub title: std::string::String,
    pub author: std::string::String,
    pub id: std::string::String,
}

impl ProfileItem {
    pub(crate) fn of(m: &crate::profiles::Match) -> ProfileItem {
        ProfileItem { title: m.profile.title().to_string(), author: m.profile.author.clone(), id: m.profile.id.clone() }
    }
}

/// The shared-profiles page as data, handed to `f`: one row per profile, the
/// player who shared it on the right ("Toi" for your own), a check on the one
/// in use; the list scrolls.
#[allow(clippy::too_many_arguments)]
pub(crate) fn with_profiles_view<R>(
    backend: &mut SwitchRenderBackend,
    items: &[ProfileItem],
    active: &str,
    copy_mode: bool,
    selection: usize,
    crumbs: &[&str],
    f: impl FnOnce(&mut SwitchRenderBackend, &crate::backend::render::Sheet) -> R,
) -> R {
    use crate::backend::render::{ui_col, Btn, Icon, Row, Sheet};
    let lc = crate::loc::s();
    let u = crate::loc_ui::ui();
    let mut rows: std::vec::Vec<Row> = items
        .iter()
        .map(|it| {
            let copy = crate::profiles::is_copy_row(&it.id);
            let mine = crate::profiles::is_mine(&it.id);
            let icon = if copy {
                Icon::Copy
            } else if mine {
                Icon::User
            } else {
                Icon::Users
            };
            let who: &str = if copy {
                ""
            } else if mine {
                u.profile_you
            } else {
                &it.author
            };
            Row::new(icon, &it.title)
                .value(who)
                .mark_if(!active.is_empty() && it.id == active, Icon::Check, ui_col::OK)
                .opens_if(copy)
        })
        .collect();
    let empty_msg = if crate::profiles::catalog_unavailable() { lc.profile_catalog_offline } else { lc.profile_none };
    if rows.is_empty() {
        // An unreachable catalog must not be reported as an empty one.
        rows.push(Row::new(Icon::Info, empty_msg).dim(true));
    }
    let mine = items.get(selection).is_some_and(|it| crate::profiles::is_mine(&it.id));
    let copy_row = items.get(selection).is_some_and(|it| crate::profiles::is_copy_row(&it.id));
    let title = if copy_mode { lc.profile_copy_row } else { lc.profile_title };
    let a_verb = if copy_row { u.open } else { u.preview };
    let hints_mine = [(&[Btn::A][..], a_verb), (&[Btn::X][..], lc.opt_delete), (&[Btn::B][..], u.back)];
    let hints = [(&[Btn::A][..], a_verb), (&[Btn::B][..], u.back)];
    let mut sheet = Sheet::new(title, &rows, if items.is_empty() { usize::MAX } else { selection });
    sheet.kicker = Some(Icon::ChevronLeft);
    sheet.crumbs = crumbs;
    sheet.row_h = 48.0;
    sheet.glide_key = GLIDE_KEY_TOUCHES_PROFILES;
    sheet.help = if copy_row { u.help_copy_profile } else { u.help_touches_apply };
    sheet.hints = if mine { &hints_mine } else { &hints };
    f(backend, &sheet)
}

/// The in-game shared-profiles page.
fn with_profiles_sheet<R>(
    backend: &mut SwitchRenderBackend,
    selection: usize,
    f: impl FnOnce(&mut SwitchRenderBackend, &crate::backend::render::Sheet) -> R,
) -> R {
    let (active, copy_mode, items) = TOUCHES
        .lock()
        .map(|s| (s.active_id.clone(), s.copy_mode, s.matches.iter().map(ProfileItem::of).collect::<std::vec::Vec<_>>()))
        .unwrap_or_default();
    let crumbs = [crate::loc::s().menu_keys];
    with_profiles_view(backend, &items, &active, copy_mode, selection, &crumbs, f)
}

/// A before / after page as data, handed to `f`: the changes as a table, under
/// what they are about.
pub(crate) fn with_diff_view<R>(
    backend: &mut SwitchRenderBackend,
    crumbs: &[&str],
    title: &str,
    subtitle: Option<&str>,
    rows: &[std::string::String],
    hints: &[crate::backend::render::HintItem],
    f: impl FnOnce(&mut SwitchRenderBackend, &crate::backend::render::Sheet) -> R,
) -> R {
    use crate::backend::render::{Icon, Sheet};
    let lc = crate::loc::s();
    let u = crate::loc_ui::ui();
    let none = [lc.profile_preview_none.to_string()];
    let mut sheet = Sheet::new(title, &[], usize::MAX);
    sheet.kicker = Some(Icon::ChevronLeft);
    sheet.crumbs = crumbs;
    sheet.subtitle = subtitle;
    sheet.row_h = 44.0;
    sheet.reserve_help = false;
    sheet.hints = hints;
    sheet.diff = if rows.is_empty() { &none } else { rows };
    sheet.diff_heads = ["", u.col_before, u.col_after];
    f(backend, &sheet)
}

/// A before / after page beside the pause rail.
fn draw_diff_page(
    backend: &mut SwitchRenderBackend,
    crumbs: &[&str],
    title: &str,
    subtitle: Option<&str>,
    rows: &[std::string::String],
    hints: &[crate::backend::render::HintItem],
) {
    let t = crate::backend::render::cascade_step(true);
    with_diff_view(backend, crumbs, title, subtitle, rows, hints, |b, c| {
        b.with_pause_sheet(PAUSE_ROW_TOUCHES, |b, parent| b.kit_cascade(parent, PAUSE_ROW_TOUCHES, Some(c), t));
    });
}

/// Draw the current TOUCHES screen + any active toast. No-op when inactive.
pub fn draw(backend: &mut SwitchRenderBackend, now: u64) {
    // Run any deferred profile network flow now that its loading panel showed a
    // frame. May transition the screen, so do it before snapshotting it.
    drive_pending_net();
    // Snapshot the screen + decrement the toast under one lock.
    let (screen, toast) = match TOUCHES.lock() {
        Ok(mut g) => {
            let t = if g.toast_frames > 0 {
                g.toast_frames -= 1;
                Some((g.toast_msg.clone(), g.toast_kind))
            } else {
                None
            };
            (g.screen, t)
        }
        Err(_) => return,
    };
    let lc = crate::loc::s();
    let u = crate::loc_ui::ui();
    use crate::backend::render::Btn;
    // Active game name, the sheet's title, as on every page of the pause.
    let game = crate::library::active_display_name().unwrap_or_default();
    match screen {
        Screen::Inactive => {}
        Screen::Loading => {
            // Loading panel over the frozen game while a deferred GitHub call runs.
            // Same overlay helper as on-cover, so it looks identical.
            backend.draw_loading_overlay(&game, now);
        }
        Screen::Menu { selection } => {
            crate::backend::render::pause_child_set(crate::backend::render::PauseChild::Touches(selection));
            let t = crate::backend::render::cascade_step(true);
            backend.with_pause_sheet(PAUSE_ROW_TOUCHES, |b, parent| {
                with_touches_sheet(b, selection, |b, c| b.kit_cascade(parent, PAUSE_ROW_TOUCHES, Some(c), t));
            });
        }
        Screen::List { selection } => {
            let Some(place) = editor_place() else { return };
            let values = pad_values();
            let player = keymap::edit_player();
            let layer = keymap::edit_subtab_index();
            let speed = place.speed_x10.map(crate::loc::mult_label);
            let help = pad_help(selection, &values, player, layer);
            backend.draw_touches_pad(&crate::backend::render::PadView {
                crumbs: &place.crumbs,
                subject: &place.subject,
                selection,
                values: &values,
                player,
                layer,
                live_layers: live_layers(player),
                stick_cursor: [
                    stick_reads_as_cursor(&values, player, 'L'),
                    stick_reads_as_cursor(&values, player, 'R'),
                ],
                cursor_speed: speed.as_deref(),
                profiles: place.profiles,
                help: &help,
            });
        }
        Screen::Keyboard { button_idx, key_idx } => {
            let Some(place) = editor_place() else { return };
            let Some(slot) = keymap::PAD_SLOTS.get(button_idx) else { return };
            // The chord in a combo layer: the layer's glyph, then the button.
            let modif = keymap::edit_subtab_modifier();
            let current = match keymap::current_binding(slot.name) {
                Some(k) => PadValue::Key(k),
                None if modif.is_empty() => PadValue::None,
                None => PadValue::Inherited(keymap::base_binding(slot.name)),
            };
            let used = keymap::current_map_used_keys(slot.name);
            let crumbs = [place.crumbs[0], place.crumbs[1], lc.keys_title];
            backend.draw_touches_keyboard(&crate::backend::render::KeysView {
                crumbs: &crumbs,
                glyph: slot.glyph,
                layer: (!modif.is_empty()).then_some(modif),
                player: keymap::edit_player(),
                current: &current,
                cursor: key_idx,
                used: &used,
            });
        }
        Screen::Profiles { selection } => {
            let t = crate::backend::render::cascade_step(true);
            backend.with_pause_sheet(PAUSE_ROW_TOUCHES, |b, parent| {
                with_profiles_sheet(b, selection, |b, c| b.kit_cascade(parent, PAUSE_ROW_TOUCHES, Some(c), t));
            });
        }
        Screen::DeleteConfirm { profile_idx, selection } => {
            // The list stays behind the question, so it is clear which profile
            // it is about.
            let t = crate::backend::render::cascade_step(true);
            backend.with_pause_sheet(PAUSE_ROW_TOUCHES, |b, parent| {
                with_profiles_sheet(b, profile_idx, |b, c| b.kit_cascade(parent, PAUSE_ROW_TOUCHES, Some(c), t));
            });
            let name = TOUCHES
                .lock()
                .ok()
                .and_then(|s| s.matches.get(profile_idx).map(|m| profile_name(&m.profile)))
                .unwrap_or_default();
            use crate::backend::render::{Choice, Icon};
            backend.draw_confirm(
                Icon::Trash,
                lc.profile_del_confirm,
                "",
                Some(&name),
                &[
                    Choice { label: u.cancel, destructive: false },
                    Choice { label: lc.opt_delete, destructive: true },
                ],
                selection,
                true,
            );
        }
        Screen::Preview { profile_idx } => {
            let (rows, name) = TOUCHES
                .lock()
                .map(|s| {
                    (
                        s.preview_rows.clone(),
                        s.matches.get(profile_idx).map(|m| profile_name(&m.profile)).unwrap_or_default(),
                    )
                })
                .unwrap_or_default();
            let crumbs = [lc.profile_title];
            let hints = [(&[Btn::A][..], u.apply), (&[Btn::B][..], u.back)];
            draw_diff_page(backend, &crumbs, &name, Some(u.preview_sub), &rows, &hints);
        }
        Screen::ShareConfirm => {
            let (is_update, rows) = TOUCHES
                .lock()
                .map(|s| (s.share_is_update, s.preview_rows.clone()))
                .unwrap_or((false, std::vec::Vec::new()));
            let question = if is_update { lc.share_confirm_update } else { lc.profile_share_confirm };
            let crumbs = [lc.menu_keys];
            let hints = [(&[Btn::A][..], u.share), (&[Btn::B][..], u.cancel)];
            draw_diff_page(backend, &crumbs, lc.opt_share, Some(question), &rows, &hints);
        }
        Screen::RevertPreview => {
            let (rows, has_backup) = TOUCHES
                .lock()
                .map(|s| (s.preview_rows.clone(), s.has_backup))
                .unwrap_or_default();
            let title = if has_backup { lc.profile_revert } else { lc.touches_revert_default };
            let crumbs = [lc.menu_keys];
            let hints = [(&[Btn::A][..], u.restore), (&[Btn::B][..], u.back)];
            draw_diff_page(backend, &crumbs, title, None, &rows, &hints);
        }
    }
    if let Some((msg, kind)) = toast {
        backend.draw_toast(&msg, kind);
    }
}
