//! In-game pause "TOUCHES" sub-menu + keymap editor.
//!
//! Reached from the pause menu's TOUCHES entry (`ruffle_touches_open` ->
//! `open_submenu`). Mirrors the library OPTIONS > TOUCHES sub-menu (#20 Option 1):
//!
//!   - **Menu** — edit keys / apply a profile / share my controls / cursor speed
//!     / revert. Apply / share / revert do HTTPS synchronously (the game is
//!     paused, so a brief freeze is fine — no async infra here).
//!   - **List / Dropdown** — the per-button keymap editor (also used directly by
//!     the library via `open`).
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

// In-game TOUCHES sub-menu rows (#20 Option 1): edit / apply / share / cursor,
// then a revert row (index 4) when there's a custom keymap to undo. Mirrors the
// library sub-menu order.
const MENU_EDIT: usize = 0;
const MENU_APPLY: usize = 1;
const MENU_SHARE: usize = 2;
const MENU_CURSOR: usize = 3;
const MENU_REVERT: usize = 4;
const MENU_FIXED_ROWS: usize = 4;

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
    /// `selection` indexes `EDITABLE_BUTTONS`, `scroll_offset` is the topmost
    /// visible row (8-rows-at-a-time window).
    List { selection: usize, scroll_offset: usize },
    /// `button_idx` indexes `EDITABLE_BUTTONS`, `selection` indexes
    /// `ALL_FLASH_KEYS`, `scroll_offset` is the dropdown's top visible row.
    Dropdown { button_idx: usize, selection: usize, scroll_offset: usize },
    /// Community-profile picker (in-game apply). `selection` indexes `matches`.
    Profiles { selection: usize },
    /// Before/after preview of an apply. `profile_idx` indexes `matches`.
    Preview { profile_idx: usize },
    /// Confirm sharing (before/after of my shared profile).
    ShareConfirm,
    /// Before/after preview of a revert.
    RevertPreview,
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

/// Maximum rows shown at once in the list screen. Keep in sync with the
/// vertical space `draw_touches_list` allocates in render.rs.
pub const LIST_VISIBLE_ROWS: usize = 8;
/// Maximum rows shown at once in the Flash-key dropdown (48 entries: A-Z, 0-9,
/// mods). 10 rows × 40 px fits the 720-px screen with header + footer.
pub const DROPDOWN_VISIBLE_ROWS: usize = 10;

pub fn is_active() -> bool {
    TOUCHES
        .lock()
        .map(|s| !matches!(s.screen, Screen::Inactive))
        .unwrap_or(false)
}

/// Open the keymap EDITOR directly (library OPTIONS > TOUCHES > Éditer). The
/// in-game pause entry uses `open_submenu` instead.
pub fn open() {
    keymap::set_edit_player(1); // always start on Player 1 (issue #40)
    if let Ok(mut s) = TOUCHES.lock() {
        s.submenu = false;
        s.screen = Screen::List { selection: 0, scroll_offset: 0 };
    }
}

/// Open the IN-GAME TOUCHES sub-menu (#20 Option 1). Editing from here returns to
/// this menu; apply/share/revert run from here too.
pub fn open_submenu() {
    let (can_revert, has_backup) = keymap::active_game_basename()
        .map(|b| (keymap::provenance(&b) != "default", keymap::has_backup(&b)))
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
                    MENU_APPLY => run_open_profiles(),
                    MENU_SHARE => run_open_share_confirm(),
                    _ => run_open_revert_preview(),
                }
                return true;
            }
            handle_menu_input(&mut s, button, selection);
            true
        }
        Screen::List { selection, scroll_offset } => {
            handle_list_input(&mut s, button, selection, scroll_offset);
            true
        }
        Screen::Dropdown { button_idx, selection, scroll_offset } => {
            handle_dropdown_input(&mut s, button, button_idx, selection, scroll_offset);
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
        Screen::Preview { profile_idx } => {
            if button == "A" {
                drop(s);
                run_apply(profile_idx);
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
                run_share();
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
                s.screen = Screen::List { selection: 0, scroll_offset: 0 };
                return;
            }
            MENU_CURSOR => {
                // Cycle the live per-game cursor speed (C++ applies + persists).
                unsafe { ruffle_cursor_speed_cycle() };
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
            (m.profile.bindings == current.bindings && m.profile.bindings_p2 == current.bindings_p2)
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
    let rows = keymap::binding_diff_rows(&current.bindings, &profile.bindings);
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
    let can_revert = keymap::provenance(&basename) != "default";
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
    let before = match mine {
        Some(m) => m.profile.bindings,
        None => keymap::revert_target(&basename).bindings, // ~default for a first share
    };
    let rows = cap_rows(keymap::binding_diff_rows(&before, &current.bindings));
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
    let lc = crate::loc::s();
    let (msg, kind) = match crate::profiles::share(&title, "", &swf_hash, &km) {
        Ok(id) => {
            keymap::mark_shared(&basename, &id);
            crate::profiles::invalidate_online_cache();
            (lc.profile_shared_ok.to_string(), TOAST_OK)
        }
        Err(e) => (e, TOAST_ERR),
    };
    let can_revert = keymap::provenance(&basename) != "default";
    let has_backup = keymap::has_backup(&basename);
    if let Ok(mut s) = TOUCHES.lock() {
        s.can_revert = can_revert;
        s.has_backup = has_backup;
        set_toast(&mut s, msg, kind);
        s.screen = Screen::Menu { selection: MENU_SHARE };
    }
}

/// REVENIR: open the before/after preview of a revert.
fn run_open_revert_preview() {
    let Some((basename, _, _)) = game_ctx() else { return };
    let current = keymap::effective_for(&basename);
    let target = keymap::revert_target(&basename);
    let rows = cap_rows(keymap::binding_diff_rows(&current.bindings, &target.bindings));
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
    let can_revert = keymap::provenance(&basename) != "default";
    let has_backup = keymap::has_backup(&basename);
    if let Ok(mut s) = TOUCHES.lock() {
        s.dirty = ok; // refresh live bindings
        s.can_revert = can_revert;
        s.has_backup = has_backup;
        set_toast(&mut s, msg.to_string(), if ok { TOAST_OK } else { TOAST_ERR });
        s.screen = Screen::Menu { selection: MENU_EDIT };
    }
}

fn handle_list_input(s: &mut State, button: &str, mut selection: usize, mut scroll: usize) {
    let last = keymap::EDITABLE_BUTTONS.len().saturating_sub(1);
    match button {
        "Up" | "StickLUp" => {
            selection = if selection == 0 { last } else { selection - 1 };
            scroll = clamp_scroll(scroll, selection);
        }
        "Down" | "StickLDown" => {
            selection = if selection >= last { 0 } else { selection + 1 };
            scroll = clamp_scroll(scroll, selection);
        }
        "X" => {
            // Toggle which player's bindings we're editing (issue #40).
            keymap::set_edit_player(if keymap::edit_player() == 2 { 1 } else { 2 });
        }
        "A" => {
            let btn = keymap::EDITABLE_BUTTONS[selection];
            let current = keymap::current_binding(btn);
            let dropdown_sel = current
                .as_deref()
                .and_then(|k| keymap::ALL_FLASH_KEYS.iter().position(|x| *x == k))
                .unwrap_or(0); // index 0 = "(none)"
            let dropdown_scroll = clamp_dropdown_scroll(0, dropdown_sel);
            s.screen = Screen::Dropdown {
                button_idx: selection,
                selection: dropdown_sel,
                scroll_offset: dropdown_scroll,
            };
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
    s.screen = Screen::List { selection, scroll_offset: scroll };
}

fn handle_dropdown_input(
    s: &mut State,
    button: &str,
    button_idx: usize,
    mut selection: usize,
    mut scroll: usize,
) {
    let last = keymap::ALL_FLASH_KEYS.len().saturating_sub(1);
    match button {
        "Up" | "StickLUp" => {
            selection = if selection == 0 { last } else { selection - 1 };
            scroll = clamp_dropdown_scroll(scroll, selection);
        }
        "Down" | "StickLDown" => {
            selection = if selection >= last { 0 } else { selection + 1 };
            scroll = clamp_dropdown_scroll(scroll, selection);
        }
        "A" => {
            let btn = keymap::EDITABLE_BUTTONS[button_idx];
            let target = if selection == 0 {
                None
            } else {
                Some(keymap::ALL_FLASH_KEYS[selection])
            };
            let ok = keymap::set_binding(btn, target);
            if ok {
                s.dirty = true;
            }
            let scroll = clamp_scroll(0, button_idx);
            s.screen = Screen::List { selection: button_idx, scroll_offset: scroll };
            return;
        }
        "B" | "Minus" => {
            let scroll = clamp_scroll(0, button_idx);
            s.screen = Screen::List { selection: button_idx, scroll_offset: scroll };
            return;
        }
        _ => {}
    }
    s.screen = Screen::Dropdown { button_idx, selection, scroll_offset: scroll };
}

/// Adjust `scroll` so the row at `selection` is visible. Window = `LIST_VISIBLE_ROWS`.
fn clamp_scroll(mut scroll: usize, selection: usize) -> usize {
    if selection < scroll {
        scroll = selection;
    } else if selection >= scroll + LIST_VISIBLE_ROWS {
        scroll = selection + 1 - LIST_VISIBLE_ROWS;
    }
    scroll
}

/// Same as `clamp_scroll` but for the Flash-key dropdown window (10 rows).
fn clamp_dropdown_scroll(mut scroll: usize, selection: usize) -> usize {
    if selection < scroll {
        scroll = selection;
    } else if selection >= scroll + DROPDOWN_VISIBLE_ROWS {
        scroll = selection + 1 - DROPDOWN_VISIBLE_ROWS;
    }
    scroll
}

/// Draw the current TOUCHES screen + any active toast. No-op when inactive.
pub fn draw(backend: &mut SwitchRenderBackend) {
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
    match screen {
        Screen::Inactive => {}
        Screen::Menu { selection } => {
            // Snapshot the dynamic bits under the lock.
            let (can_revert, has_backup) = TOUCHES
                .lock()
                .map(|s| (s.can_revert, s.has_backup))
                .unwrap_or((false, false));
            let m = unsafe { ruffle_cursor_speed_mult_x10() };
            let cursor = std::format!("{}: x{}.{}", lc.set_cursor_speed, m / 10, m % 10);
            let mut rows: std::vec::Vec<&str> =
                std::vec![lc.touches_edit, lc.opt_apply, lc.opt_share, cursor.as_str()];
            if can_revert {
                rows.push(if has_backup { lc.profile_revert } else { lc.touches_revert_default });
            }
            backend.draw_library_list_modal(lc.opt_keys, "", selection, &rows, lc.touches_footer);
        }
        Screen::List { selection, scroll_offset } => {
            let bindings: std::vec::Vec<(&'static str, Option<std::string::String>)> =
                keymap::EDITABLE_BUTTONS
                    .iter()
                    .map(|btn| (*btn, keymap::current_binding(btn)))
                    .collect();
            backend.draw_touches_list(selection, scroll_offset, &bindings, LIST_VISIBLE_ROWS, keymap::edit_player());
        }
        Screen::Dropdown { button_idx, selection, scroll_offset } => {
            let btn = keymap::EDITABLE_BUTTONS[button_idx];
            backend.draw_touches_dropdown(btn, selection, scroll_offset, keymap::ALL_FLASH_KEYS, DROPDOWN_VISIBLE_ROWS);
        }
        Screen::Profiles { selection } => {
            let (active, mut rows): (std::string::String, std::vec::Vec<std::string::String>) =
                TOUCHES
                    .lock()
                    .map(|s| {
                        (
                            s.active_id.clone(),
                            s.matches.iter().map(|m| m.profile.title().to_string()).collect(),
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
                rows.push(lc.profile_none.to_string());
            }
            let refs: std::vec::Vec<&str> = rows.iter().map(|r| r.as_str()).collect();
            backend.draw_library_list_modal(lc.profile_title, "", selection, &refs, lc.profile_footer);
        }
        Screen::Preview { .. } => {
            let mut rows = TOUCHES.lock().map(|s| s.preview_rows.clone()).unwrap_or_default();
            if rows.is_empty() {
                rows.push(lc.profile_preview_none.to_string());
            }
            let refs: std::vec::Vec<&str> = rows.iter().map(|r| r.as_str()).collect();
            backend.draw_library_list_modal(lc.profile_preview_title, "", usize::MAX, &refs, lc.profile_preview_footer);
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
            backend.draw_library_list_modal(lc.opt_share, subtitle, usize::MAX, &refs, lc.lang_footer);
        }
        Screen::RevertPreview => {
            let mut rows = TOUCHES.lock().map(|s| s.preview_rows.clone()).unwrap_or_default();
            if rows.is_empty() {
                rows.push(lc.profile_preview_none.to_string());
            }
            let refs: std::vec::Vec<&str> = rows.iter().map(|r| r.as_str()).collect();
            backend.draw_library_list_modal(lc.revert_preview_title, "", usize::MAX, &refs, lc.revert_preview_footer);
        }
    }
    if let Some((msg, kind)) = toast {
        backend.draw_toast(&msg, kind);
    }
}
