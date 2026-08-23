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
//!   - **Profiles / Preview / ShareConfirm / RevertPreview** — the profile flows,
//!     drawn with the same generic `draw_library_list_modal` the library uses.
//!
//! All state lives behind a single Mutex so C++ can drive the screen via a thin
//! FFI surface: `is_active`, `open`/`open_submenu`, `input`, `draw`,
//! `consume_dirty`. Network actions drop the lock before the HTTPS call.

use std::sync::Mutex;

use crate::backend::render::SwitchRenderBackend;
use crate::keymap;

// C++-owned cursor speed (live + per-game while in a game), reused from the
// library extern block: cycle to the next preset / read the x10 label value.
use crate::library::{ruffle_cursor_speed_cycle, ruffle_cursor_speed_mult_x10};

// In-game TOUCHES sub-menu rows (#20 Option 1): edit / apply / share / cursor
// speed / show-cursor toggle, then a revert row (index 5) when there's a custom
// keymap to undo. Mirrors the library sub-menu order.
const MENU_EDIT: usize = 0;
const MENU_APPLY: usize = 1;
const MENU_SHARE: usize = 2;
const MENU_CURSOR: usize = 3;
const MENU_SHOWCURSOR: usize = 4;
const MENU_REVERT: usize = 5;
const MENU_FIXED_ROWS: usize = 5;

// Toast kinds (match `SwitchRenderBackend::draw_toast`): 0 green, 1 red, 2 blue.
const TOAST_OK: u8 = 0;
const TOAST_ERR: u8 = 1;
const TOAST_INFO: u8 = 2;
const TOAST_FRAMES: u32 = 150; // ~2.5 s at 60 fps

/// Cap a diff list so the auto-sized modal still fits the screen.
const MAX_PREVIEW_ROWS: usize = 8;

#[derive(Debug, Clone, Copy)]
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
    DeleteConfirm { profile_idx: usize },
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
    /// Whether the sub-menu shows a revert row (game has a custom keymap).
    can_revert: bool,
    /// Whether a hand-made backup exists (revert restores it) vs none (resets to
    /// default) — drives the revert label.
    has_backup: bool,
    /// Profiles matching the running game (in-game apply picker).
    matches: std::vec::Vec<crate::profiles::Match>,
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
    can_revert: false,
    has_backup: false,
    matches: std::vec::Vec::new(),
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
            if idx >= crate::keymap::PAD_SLOTS.len() {
                return false;
            }
            // A modifier row takes no cursor from a finger either, and returning
            // false means the tap is simply ignored rather than counted as the
            // first half of a double-tap that would then activate it.
            if crate::keymap::PAD_SLOTS
                .get(idx)
                .map(|s| crate::keymap::slot_is_modifier(s.name))
                .unwrap_or(false)
            {
                return false;
            }
            if selection == idx {
                return true;
            }
            s.screen = Screen::List { selection: idx };
            false
        }
        // The rows of the sub-menu and the profile picker are drawn by the
        // shared `draw_modal_rows`, which publishes its table like every other
        // modal in the app -- so they answer a finger for the same reason the
        // library's modals do.
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
        Screen::Profiles { selection } => {
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

/// Open the keymap EDITOR directly (library OPTIONS > TOUCHES > Éditer). The
/// in-game pause entry uses `open_submenu` instead.
pub fn open() {
    keymap::set_edit_player(1); // always start on Player 1 (issue #40)...
    keymap::reset_edit_subtabs(); // ...both players on the NORMAL sub-tab, without
    // clearing the persisted modifiers (combos stay enabled in-game) (issue #57).
    // Row 0 is ZL, and ZL is locked the moment it has a combo layer -- so the
    // editor could OPEN with its cursor on a row that takes no A. Resolved
    // before the lock, not inside it, to keep this off the TOUCHES -> keymap
    // nesting entirely.
    let start = unstick_cursor(0);
    if let Ok(mut s) = TOUCHES.lock() {
        s.submenu = false;
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
        rows.push(std::format!("(+{} ...)", extra));
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
            handle_list_input(&mut s, button, selection);
            true
        }
        Screen::Keyboard { button_idx, key_idx } => {
            handle_keyboard_input(&mut s, button, button_idx, key_idx);
            true
        }
        Screen::Profiles { selection } => {
            // A opens the before/after preview (reads the keymap) — hoist it.
            if button == "A" {
                let n = s.matches.len();
                drop(s);
                if selection < n {
                    run_open_preview(selection);
                }
                return true;
            }
            handle_profiles_input(&mut s, button, selection);
            true
        }
        Screen::DeleteConfirm { profile_idx } => {
            // A deletes my shared profile (HTTPS) — defer + loading panel; B cancels.
            if button == "A" {
                drop(s);
                defer_net(PendingNet::Delete { profile_idx });
                return true;
            }
            if matches!(button, "B" | "Minus") {
                s.screen = Screen::Profiles { selection: profile_idx };
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
    match button {
        "Up" | "StickLUp" => {
            selection = if selection == 0 { last } else { selection - 1 };
        }
        "Down" | "StickLDown" => {
            selection = if selection >= last { 0 } else { selection + 1 };
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
                unsafe { ruffle_cursor_speed_cycle() };
            }
            MENU_SHOWCURSOR => {
                // Toggle the per-game pointer visibility (persists in the keymap).
                keymap::toggle_show_cursor();
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
    let last = s.matches.len().max(1) - 1;
    match button {
        "Up" | "StickLUp" => selection = if selection == 0 { last } else { selection - 1 },
        "Down" | "StickLDown" => selection = if selection >= last { 0 } else { selection + 1 },
        "X" => {
            // Delete MY OWN shared profile: only on a row this install shared.
            if s
                .matches
                .get(selection)
                .is_some_and(|m| crate::profiles::is_mine(&m.profile.id))
            {
                s.screen = Screen::DeleteConfirm { profile_idx: selection };
                return;
            }
        }
        "B" | "Minus" => {
            s.screen = Screen::Menu { selection: MENU_APPLY };
            return;
        }
        _ => {}
    }
    s.screen = Screen::Profiles { selection };
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
    if let Ok(mut s) = TOUCHES.lock() {
        s.matches = matches;
        s.active_id = active_id;
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
fn pad_step(cur: usize, dir: PadDir) -> usize {
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
    // Nothing that way. Up and Down wrap inside their own column, the way the
    // list they replace wrapped; a cursor that stops dead at the top of a column
    // reads as a stuck stick. Left and Right at the outer edge stay put — there
    // is no third column to wrap to.
    let want_max = match dir {
        PadDir::Up => true,
        PadDir::Down => false,
        _ => return cur,
    };
    let mut wrap = cur;
    let mut edge = cy;
    for i in 0..n {
        if slot_locked(i) {
            continue;
        }
        let (x, y) = pad_center(i);
        if same_col(x) && ((want_max && y > edge) || (!want_max && y < edge)) {
            edge = y;
            wrap = i;
        }
    }
    wrap
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
        let next = pad_step(selection, PadDir::Down);
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
    match button {
        "Up" | "StickLUp" => selection = pad_step(selection, PadDir::Up),
        "Down" | "StickLDown" => selection = pad_step(selection, PadDir::Down),
        // Left and Right finally mean something on this screen: the pad has two
        // columns, and they cross to the other hand at the same height. The
        // SHOULDER L/R still change the combo layer, as they always did.
        "Left" | "StickLLeft" => selection = pad_step(selection, PadDir::Left),
        "Right" | "StickLRight" => selection = pad_step(selection, PadDir::Right),
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
            // layer switched while the cursor sat here, can put it on one.
            if keymap::slot_is_modifier(slot.name) {
                set_toast(
                    s,
                    std::format!("{} : {}", slot.name, crate::loc::s().keys_modifier),
                    TOAST_INFO,
                );
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
            // In-game: back to the sub-menu (Éditer row). Library: close.
            s.screen = if s.submenu {
                Screen::Menu { selection: MENU_EDIT }
            } else {
                Screen::Inactive
            };
            return;
        }
        _ => {}
    }
    s.screen = Screen::List { selection };
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
    let (_, _, x, w) = keymap::KEYBOARD[i];
    x + w * 0.5
}

/// Nearest key to `cx` (by centre) among those on `row`. Rows always have ≥1 key,
/// so this never returns None for a valid row.
fn kbd_nearest_on_row(row: u8, cx: f32) -> Option<usize> {
    keymap::KEYBOARD
        .iter()
        .enumerate()
        .filter(|(_, k)| k.1 == row)
        .min_by(|(ia, _), (ib, _)| {
            (kbd_center(*ia) - cx)
                .abs()
                .total_cmp(&(kbd_center(*ib) - cx).abs())
        })
        .map(|(i, _)| i)
}

fn handle_keyboard_input(
    s: &mut State,
    button: &str,
    button_idx: usize,
    mut key_idx: usize,
) {
    let (name, cur_row, _, _) = keymap::KEYBOARD[key_idx];
    let cur_cx = kbd_center(key_idx);
    let n_rows = keymap::KEYBOARD_ROWS_N as u8;
    // Row-scoped left/right by centre; up/down jump to the vertically adjacent row
    // and land on the key whose centre is nearest — so the numpad column stays put.
    match button {
        "Left" | "StickLLeft" => {
            key_idx = keymap::KEYBOARD
                .iter()
                .enumerate()
                .filter(|(_, k)| k.1 == cur_row && kbd_center_of(k) < cur_cx - 0.01)
                .max_by(|a, b| kbd_center_of(a.1).total_cmp(&kbd_center_of(b.1)))
                .map(|(i, _)| i)
                // Wrap to the rightmost key on the row.
                .or_else(|| {
                    keymap::KEYBOARD
                        .iter()
                        .enumerate()
                        .filter(|(_, k)| k.1 == cur_row)
                        .max_by(|a, b| kbd_center_of(a.1).total_cmp(&kbd_center_of(b.1)))
                        .map(|(i, _)| i)
                })
                .unwrap_or(key_idx);
        }
        "Right" | "StickLRight" => {
            key_idx = keymap::KEYBOARD
                .iter()
                .enumerate()
                .filter(|(_, k)| k.1 == cur_row && kbd_center_of(k) > cur_cx + 0.01)
                .min_by(|a, b| kbd_center_of(a.1).total_cmp(&kbd_center_of(b.1)))
                .map(|(i, _)| i)
                .or_else(|| {
                    keymap::KEYBOARD
                        .iter()
                        .enumerate()
                        .filter(|(_, k)| k.1 == cur_row)
                        .min_by(|a, b| kbd_center_of(a.1).total_cmp(&kbd_center_of(b.1)))
                        .map(|(i, _)| i)
                })
                .unwrap_or(key_idx);
        }
        "Up" | "StickLUp" => {
            let target = if cur_row == 0 { n_rows - 1 } else { cur_row - 1 };
            key_idx = kbd_nearest_on_row(target, cur_cx).unwrap_or(key_idx);
        }
        "Down" | "StickLDown" => {
            let target = if cur_row + 1 >= n_rows { 0 } else { cur_row + 1 };
            key_idx = kbd_nearest_on_row(target, cur_cx).unwrap_or(key_idx);
        }
        "A" => {
            // "(none)" unbinds; every other key is a real flash-key name.
            let target = if name == "(none)" { None } else { Some(name) };
            let Some(edit_btn) = keymap::PAD_SLOTS.get(button_idx).map(|s| s.name) else {
                return;
            };
            if keymap::set_binding(edit_btn, target) {
                s.dirty = true;
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
    // Active game name, shown as the subtitle under the header — same as the
    // on-cover OPTIONS / profile picker (it was missing in-game).
    let game = crate::library::active_display_name().unwrap_or_default();
    match screen {
        Screen::Inactive => {}
        Screen::Loading => {
            // Loading panel over the frozen game while a deferred GitHub call runs.
            // Same overlay helper as on-cover, so it looks identical.
            backend.draw_loading_overlay(&game, now);
        }
        Screen::Menu { selection } => {
            // Snapshot the dynamic bits under the lock.
            let (can_revert, has_backup) = TOUCHES
                .lock()
                .map(|s| (s.can_revert, s.has_backup))
                .unwrap_or((false, false));
            let m = unsafe { ruffle_cursor_speed_mult_x10() };
            let cursor = std::format!("{}: x{}.{}", lc.set_cursor_speed, m / 10, m % 10);
            let show_cur = std::format!(
                "{}: {}",
                lc.show_cursor,
                if keymap::show_cursor() { lc.cursor_shown } else { lc.cursor_hidden },
            );
            let mut rows: std::vec::Vec<&str> = std::vec![
                lc.touches_edit,
                lc.opt_apply,
                lc.opt_share,
                cursor.as_str(),
                show_cur.as_str(),
            ];
            if can_revert {
                rows.push(if has_backup { lc.profile_revert } else { lc.touches_revert_default });
            }
            backend.draw_library_list_modal(lc.opt_keys, &game, selection, &rows, lc.touches_footer, false);
        }
        Screen::List { selection } => {
            // Parallel to PAD_SLOTS, which is both the order the pad lays out
            // and the order `selection` counts in. `true` = this button is a
            // modifier in the open layer and sends no key of its own.
            let bindings: std::vec::Vec<(Option<std::string::String>, bool)> = keymap::PAD_SLOTS
                .iter()
                .map(|slot| (keymap::current_binding(slot.name), keymap::slot_is_modifier(slot.name)))
                .collect();
            backend.draw_touches_pad(
                selection,
                &bindings,
                keymap::edit_player(),
                keymap::edit_subtab_index(),
            );
        }
        Screen::Keyboard { button_idx, key_idx } => {
            // Title shows the chord in a combo sub-tab ("ZL+A"), else the button.
            let btn = keymap::PAD_SLOTS
                .get(button_idx)
                .map(|s| s.name)
                .unwrap_or("");
            let modif = keymap::edit_subtab_modifier();
            let label = if modif.is_empty() {
                btn.to_string()
            } else {
                std::format!("{}+{}", modif, btn)
            };
            let used = keymap::current_map_used_keys();
            backend.draw_touches_keyboard(&label, key_idx, &used);
        }
        Screen::Profiles { selection } => {
            let (active, mut rows): (std::string::String, std::vec::Vec<std::string::String>) =
                TOUCHES
                    .lock()
                    .map(|s| {
                        (
                            s.active_id.clone(),
                            s.matches
                                .iter()
                                .map(|m| {
                                    // Title + author nickname (distinguishes profiles).
                                    let mut r = m.profile.title().to_string();
                                    if !m.profile.author.is_empty() {
                                        r.push_str(" - ");
                                        r.push_str(&m.profile.author);
                                    }
                                    r
                                })
                                .collect(),
                        )
                    })
                    .unwrap_or_default();
            // Tag the active row.
            let active_ids: std::vec::Vec<std::string::String> = TOUCHES
                .lock()
                .map(|s| s.matches.iter().map(|m| m.profile.id.clone()).collect())
                .unwrap_or_default();
            for (i, id) in active_ids.iter().enumerate() {
                if !active.is_empty() && *id == active {
                    if let Some(r) = rows.get_mut(i) {
                        r.push(' ');
                        r.push_str(lc.profile_active);
                    }
                }
            }
            if rows.is_empty() {
                // Twin of the on-cover picker in library.rs: an unreachable
                // catalog must not be reported as an empty one.
                rows.push(if crate::profiles::catalog_unavailable() {
                    lc.profile_catalog_offline.to_string()
                } else {
                    lc.profile_none.to_string()
                });
            }
            let refs: std::vec::Vec<&str> = rows.iter().map(|r| r.as_str()).collect();
            // Append the delete hint only when sitting on your own profile.
            let footer = if active_ids
                .get(selection)
                .is_some_and(|id| crate::profiles::is_mine(id))
            {
                std::format!("{}   {}", lc.profile_footer, lc.profile_del_hint)
            } else {
                lc.profile_footer.to_string()
            };
            backend.draw_library_list_modal(lc.profile_title, &game, selection, &refs, &footer, true);
        }
        Screen::DeleteConfirm { profile_idx } => {
            let name = TOUCHES
                .lock()
                .ok()
                .and_then(|s| {
                    s.matches.get(profile_idx).map(|m| {
                        let mut r = m.profile.title().to_string();
                        if !m.profile.author.is_empty() {
                            r.push_str(" - ");
                            r.push_str(&m.profile.author);
                        }
                        r
                    })
                })
                .unwrap_or_default();
            let refs = [name.as_str()];
            // usize::MAX = no cursor (confirm: A deletes / B cancels).
            backend.draw_library_list_modal(lc.profile_del_confirm, "", usize::MAX, &refs, lc.del_footer, true);
        }
        Screen::Preview { .. } => {
            let mut rows = TOUCHES.lock().map(|s| s.preview_rows.clone()).unwrap_or_default();
            if rows.is_empty() {
                rows.push(lc.profile_preview_none.to_string());
            }
            let refs: std::vec::Vec<&str> = rows.iter().map(|r| r.as_str()).collect();
            backend.draw_library_list_modal(lc.profile_preview_title, "", usize::MAX, &refs, lc.profile_preview_footer, true);
        }
        Screen::ShareConfirm => {
            let (is_update, mut rows) = TOUCHES
                .lock()
                .map(|s| (s.share_is_update, s.preview_rows.clone()))
                .unwrap_or((false, std::vec::Vec::new()));
            let subtitle = if is_update { lc.share_confirm_update } else { lc.profile_share_confirm };
            if rows.is_empty() {
                rows.push(lc.profile_preview_none.to_string());
            }
            let refs: std::vec::Vec<&str> = rows.iter().map(|r| r.as_str()).collect();
            backend.draw_library_list_modal(lc.opt_share, subtitle, usize::MAX, &refs, lc.lang_footer, true);
        }
        Screen::RevertPreview => {
            let mut rows = TOUCHES.lock().map(|s| s.preview_rows.clone()).unwrap_or_default();
            if rows.is_empty() {
                rows.push(lc.profile_preview_none.to_string());
            }
            let refs: std::vec::Vec<&str> = rows.iter().map(|r| r.as_str()).collect();
            backend.draw_library_list_modal(lc.revert_preview_title, "", usize::MAX, &refs, lc.revert_preview_footer, true);
        }
    }
    if let Some((msg, kind)) = toast {
        backend.draw_toast(&msg, kind);
    }
}
