//! The RÉGLAGES tab, and everything opened from it.
//!
//! Laid out like the console's own System Settings (Reglages.dc.html): the
//! sections down a column on the left, Quitter FlashNX pinned at its foot, and
//! beside it the open section's settings, changed where they are shown. Up and
//! Down in the column change the section and the pane follows at once; Right
//! or A go into the pane; B, or a fresh Left, come back out. L and R still
//! change tabs.
//!
//! A setting changes the way it does everywhere (input rule 1): a switch flips
//! on A; a choice or a number is armed by A, then Left and Right move it and
//! A keeps it (B puts it back), so a stick pushed sideways on the way down the
//! pane changes nothing. The language and the home layout are places the
//! cursor walks to, applied by A (sweeping the cursor across nine scripts
//! would leave a player in one they cannot read), and moving the library asks
//! first (it renames real files).
//!
//! A deeper level opens inside the pane with the path above it: the browser of
//! the games folder, the game list of a bug report. Only the questions (move
//! the library? the report could not be sent) and the controls editor sit on
//! top, over the page dimmed behind them.

use super::*;
use crate::backend::render::{ui_col, Btn, Choice, HintItem, Icon, Row};

/// The column on the left, top to bottom. Quitter is not a section: it sits at
/// the foot of the column and acts on A; its pane only explains it, and the
/// cursor never goes in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SetSection {
    Library,
    Defaults,
    Language,
    Profile,
    Help,
    About,
    Quit,
}

impl SetSection {
    const ALL: [SetSection; 7] = [
        SetSection::Library,
        SetSection::Defaults,
        SetSection::Language,
        SetSection::Profile,
        SetSection::Help,
        SetSection::About,
        SetSection::Quit,
    ];

    pub(crate) fn index(self) -> usize {
        Self::ALL.iter().position(|s| *s == self).unwrap_or(0)
    }

    fn at(i: usize) -> SetSection {
        Self::ALL.get(i).copied().unwrap_or(SetSection::Library)
    }

    /// The pane's rows, by name, for the sections made of rows.
    fn rows(self) -> &'static [&'static str] {
        match self {
            SetSection::Library => LIBRARY_ROWS,
            SetSection::Defaults => DEFAULTS_ROWS,
            SetSection::Profile => PROFILE_ROWS,
            SetSection::Help => HELP_ROWS,
            SetSection::Language | SetSection::About | SetSection::Quit => &[],
        }
    }

    /// How many places the cursor can take in the pane. None in About, which
    /// only has things to read, nor in Quitter, whose pane only explains what
    /// it does: the cursor does not go in there.
    fn pane_len(self) -> usize {
        match self {
            SetSection::Language => crate::loc::PICKER_LANGS.len(),
            other => other.rows().len(),
        }
    }
}

// The rows of each section, in the order they are drawn. Matched by name,
// never by number: every screen that comes back to the tab lands on the row
// that opened it through these names.
const LIBRARY_ROWS: &[&str] = &["VUE", "DOSSIER", "SONS"];
// The game defaults in the groups and the order of a game's options (the
// controls, the picture as the pause's ÉCRAN lists it, the performance).
const DEFAULTS_ROWS: &[&str] =
    &["TOUCHES", "VITESSE", "AFFICHAGE", "ROTATION", "ZOOM", "PIXELS", "FILTRE", "FPS", "OVERCLOCK"];
const PROFILE_ROWS: &[&str] = &["PSEUDO"];
const HELP_ROWS: &[&str] = &["BUG", "IDEE"];

/// The languages, three to a row.
const LANG_COLS: usize = 3;
/// The overclock's settings in the order its choice shows them, as everywhere
/// else (Inactif, Auto, Actif).
const POWER_ORDER: [u8; 3] = [0, keymap::POWER_AUTO, 1];

/// Where row `key` of `section` sits (0 if it is not there).
pub(crate) fn row_of(section: SetSection, key: &str) -> usize {
    section.rows().iter().position(|k| *k == key).unwrap_or(0)
}

fn key_of(section: SetSection, row: usize) -> &'static str {
    section.rows().get(row).copied().unwrap_or("")
}

/// The tab with the cursor on row `key` of `section`'s pane: where a page or
/// a flow opened from that row comes back to.
pub(crate) fn settings_at(section: SetSection, key: &str) -> Screen {
    Screen::Settings { section, in_pane: true, row: row_of(section, key) }
}

/// The section last open, for coming back to the tab with L / R.
static LAST_SECTION: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);

/// The tab as a tab switch finds it: the cursor in the column, on the section
/// that was open when the player left. Quitter is the one entry not to come
/// back to, with the next A closing the app.
pub(crate) fn settings_home() -> Screen {
    let section = SetSection::at(LAST_SECTION.load(core::sync::atomic::Ordering::Relaxed));
    let section = if section == SetSection::Quit { SetSection::Library } else { section };
    Screen::Settings { section, in_pane: false, row: 0 }
}

/// Where the cursor lands on going into a section's pane: the first row, or in
/// Langue the language in use.
fn entry_row(section: SetSection) -> usize {
    match section {
        SetSection::Language => crate::loc::current().index(),
        _ => 0,
    }
}

/// Set when Right took the cursor from the column into the pane. The same
/// held Right keeps sending repeat ticks after it, and without this they would
/// land on the first setting and move it. A fresh press clears it.
static HELD_INTO_PANE: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

/// Input on the tab: in the column or in the pane.
pub(crate) fn handle_settings_input(s: &mut State, button: &str, section: SetSection, in_pane: bool, row: usize) {
    if in_pane {
        pane_input(s, button, section, row);
    } else {
        column_input(s, button, section);
    }
}

fn column_input(s: &mut State, button: &str, section: SetSection) {
    match button {
        "Up" | "StickLUp" | "Down" | "StickLDown" => {
            let delta = if matches!(button, "Up" | "StickLUp") { -1 } else { 1 };
            let next = SetSection::at(list_step_wrap(section.index(), delta, SetSection::ALL.len()));
            s.screen = Screen::Settings { section: next, in_pane: false, row: 0 };
        }
        "A" if section == SetSection::Quit => s.screen = Screen::Quit,
        "A" | "Right" | "StickLRight" if section.pane_len() > 0 => {
            if button != "A" {
                // On a press: a Right held from somewhere else must not walk in.
                if !nav_may_wrap() {
                    return;
                }
                HELD_INTO_PANE.store(true, core::sync::atomic::Ordering::Relaxed);
            }
            s.view_focus = None;
            s.screen = Screen::Settings { section, in_pane: true, row: entry_row(section) };
        }
        // À propos has only things to read: A has nothing to do, and says
        // nothing.
        "A" => crate::sfx::hush(),
        _ => {}
    }
}

fn pane_input(s: &mut State, button: &str, section: SetSection, row: usize) {
    let fresh = nav_may_wrap();
    let sideways = matches!(button, "Left" | "StickLLeft" | "Right" | "StickLRight");
    if fresh {
        HELD_INTO_PANE.store(false, core::sync::atomic::Ordering::Relaxed);
    } else if sideways && HELD_INTO_PANE.load(core::sync::atomic::Ordering::Relaxed) {
        return;
    }
    let n = section.pane_len();
    let row = row.min(n.saturating_sub(1));
    // An armed row (input rules 1 and 2): Left / Right move its value, A
    // keeps it (written then), B or `-` put it back, Up / Down keep it and
    // move on. Nothing else acts, Left included: on an armed row it is the
    // value's, not the way back to the column.
    if let Some(a) = armed_here(s) {
        match button {
            "Left" | "StickLLeft" | "Right" | "StickLRight" => {
                let dir = if matches!(button, "Left" | "StickLLeft") { -1 } else { 1 };
                let next = armed_next(section, row, a.cur, dir);
                if next != a.cur {
                    s.armed = Some(Armed { cur: next, ..a });
                    crate::sfx::value_changed();
                }
                return;
            }
            "A" => {
                confirm_armed(s);
                crate::sfx::choose(crate::sfx::Sfx::Decide);
                return;
            }
            "B" | "Minus" => {
                s.armed = None;
                crate::sfx::choose(crate::sfx::Sfx::Back);
                return;
            }
            // A held direction against the end of the list moves nothing, and
            // keeps the row armed (`list_step_moves`).
            "Up" | "StickLUp" | "Down" | "StickLDown" => {
                if !list_step_moves(button, row, n) {
                    return;
                }
                confirm_armed(s);
            }
            _ => return,
        }
    }
    if section == SetSection::Library && key_of(section, row) == "VUE" && view_cards_input(s, button, row, fresh) {
        return;
    }
    match button {
        "B" | "Minus" => {
            s.view_focus = None;
            s.screen = Screen::Settings { section, in_pane: false, row };
            return;
        }
        "Up" | "StickLUp" | "Down" | "StickLDown" => {
            // A card walked to and left without A stays as it was.
            s.view_focus = None;
            let down = matches!(button, "Down" | "StickLDown");
            // A press on an end comes round, a held direction stops there,
            // as in every list of the app.
            let next = if section == SetSection::Language {
                grid_step(row, down, LANG_COLS, n)
            } else {
                list_step_wrap(row, if down { 1 } else { -1 }, n)
            };
            s.screen = Screen::Settings { section, in_pane: true, row: next };
            return;
        }
        _ => {}
    }
    if section == SetSection::Language {
        language_input(s, button, row, fresh);
        return;
    }
    match button {
        // Left points at the section column: a fresh press goes back to it,
        // from any row, as on the console (input rule 3). A held Left stays on
        // the row, and Right does nothing (A or Right came in).
        "Left" | "StickLLeft" if fresh => {
            s.screen = Screen::Settings { section, in_pane: false, row };
        }
        "A" => row_a(s, section, key_of(section, row)),
        _ => {}
    }
}

/// A on row `key` of `section`: arm a value, flip a switch, open what the row
/// opens. The nickname and the idea open the keyboard: hoisted in `input()`.
fn row_a(s: &mut State, section: SetSection, key: &str) {
    match (section, key) {
        (SetSection::Library, "DOSSIER") => {
            // Opens beside the folder in use, the cursor on it: the row opens
            // on what it shows, among folders it could move to. Its own
            // listing holds none (every folder in it is part of the library),
            // and a new folder made beside it can take it. A library at the
            // card's root has nothing beside it, and opens on itself.
            let root = primary_root();
            if browse_parent(&root).is_some() {
                browse_go_up(s, &root);
            } else {
                s.dir_browse = list_dirs(&root);
                s.dir_path = root;
                s.screen = Screen::GamesDirPicker { selection: 0, scroll: 0 };
            }
            // The folder it opened on: B leaves from there or above it, and
            // climbs anywhere else (`b_climbs`).
            if let Ok(mut g) = BROWSE_START.lock() {
                *g = s.dir_path.clone();
            }
        }
        (SetSection::Library, "SONS") => {
            // A switch. Turning them on is heard at once: the press's click
            // (`sfx::after_input`) plays after the switch.
            crate::loc::set_ui_sounds(!crate::loc::ui_sounds());
            crate::loc::save_current();
            crate::sfx::value_changed();
        }
        (SetSection::Defaults, "TOUCHES") => {
            // The global default keymap, in the editor every game uses.
            keymap::init_for_global_default();
            menu::open(None);
            s.screen = Screen::SettingsKeymapEditor;
        }
        (SetSection::Defaults, "FPS") => {
            let on = crate::loc::default_fps_counter() == 1;
            crate::loc::set_default_fps_counter((!on) as u8);
            crate::loc::save_current();
            default_written();
            crate::sfx::value_changed();
        }
        (SetSection::Defaults, k) => arm_here(s, default_value(k)),
        (SetSection::Help, "BUG") => {
            // A report is about one game: with none on the card the row is
            // greyed out, and A says why, with the refusal's note, instead of
            // opening an empty list.
            if s.entries.is_empty() {
                set_toast(s, crate::loc::s().bug_no_games.to_string(), TOAST_INFO);
                crate::sfx::choose(crate::sfx::Sfx::Error);
            } else {
                s.screen = Screen::BugPicker { selection: 0, scroll_offset: 0 };
            }
        }
        _ => {}
    }
}

/// The languages: Left on the first column goes back to the section column,
/// Right stops at the end of a row, A applies (the home layouts work the same
/// way, `view_cards_input`). Applied on the move, a sweep of the cursor would
/// pass through scripts a player may not be able to read their way back from.
fn language_input(s: &mut State, button: &str, row: usize, fresh: bool) {
    let n = crate::loc::PICKER_LANGS.len();
    let at = |row: usize| Screen::Settings { section: SetSection::Language, in_pane: true, row };
    match button {
        "A" => match crate::loc::PICKER_LANGS.get(row) {
            Some(&lang) if lang != crate::loc::current() => {
                crate::loc::set(lang);
                crate::loc::save(lang);
                crate::sfx::value_changed();
            }
            // The language already in use: nothing to apply, nothing to hear.
            _ => crate::sfx::hush(),
        },
        "Left" | "StickLLeft" => {
            if row % LANG_COLS > 0 {
                s.screen = at(row - 1);
            } else if fresh {
                s.screen = Screen::Settings { section: SetSection::Language, in_pane: false, row };
            }
        }
        "Right" | "StickLRight" => {
            if row % LANG_COLS + 1 < LANG_COLS && row + 1 < n {
                s.screen = at(row + 1);
            }
        }
        _ => {}
    }
}

/// The home layouts, chosen as the languages are (Jonathan: "il faut le faire
/// comme langues donc A pour valider"): Left and Right walk the four cards
/// with nothing applied, A applies the card under the cursor. A fresh Left on
/// the first card goes back to the section column; Right stops on the last.
/// Leaving the row without A leaves the layout in use as it was. Answers
/// whether the press was the cards'.
fn view_cards_input(s: &mut State, button: &str, row: usize, fresh: bool) -> bool {
    let in_use = crate::loc::home_view();
    let card = s.view_focus.unwrap_or(in_use).min(crate::loc::HOME_VIEW_COUNT - 1);
    let walk_to = |s: &mut State, to: u8| {
        s.view_focus = if to == in_use { None } else { Some(to) };
        // The screen stays the same, so `after_input` would hear nothing.
        crate::sfx::choose(crate::sfx::Sfx::Move);
    };
    match button {
        "Left" | "StickLLeft" => {
            if card > 0 {
                walk_to(s, card - 1);
            } else if fresh {
                s.view_focus = None;
                s.screen = Screen::Settings { section: SetSection::Library, in_pane: false, row };
            }
            true
        }
        "Right" | "StickLRight" => {
            if card + 1 < crate::loc::HOME_VIEW_COUNT {
                walk_to(s, card + 1);
            }
            true
        }
        "A" => {
            s.view_focus = None;
            if card != in_use {
                apply_home_view(card);
                crate::sfx::value_changed();
            } else {
                // The layout already in use: nothing to apply, nothing to hear.
                crate::sfx::hush();
            }
            true
        }
        _ => false,
    }
}

/// Apply home layout `v`: written for the next boot, and the home's eased
/// offsets dropped, since they belong to the layout that produced them and a
/// new layout reading them would slide in from coordinates meaning something
/// else.
fn apply_home_view(v: u8) {
    crate::loc::set_home_view(v);
    crate::loc::save_current();
    crate::backend::render::home_anim_reset();
}

/// The value of the game default `key` as its row steps it: an index into its
/// options, a preset index (the cursor speed), or the zoom's percentage.
fn default_value(key: &str) -> u16 {
    use crate::loc as l;
    match key {
        "AFFICHAGE" => l::default_display_mode() as u16,
        "ROTATION" => l::default_rotation() as u16,
        "PIXELS" => l::default_pixel_filter() as u16,
        "FILTRE" => l::default_screen_filter() as u16,
        "OVERCLOCK" => POWER_ORDER.iter().position(|m| *m == l::default_power_mode()).unwrap_or(0) as u16,
        "ZOOM" => l::default_zoom(),
        // C++ owns the live value, which out of a game IS the global default.
        "VITESSE" => unsafe { ruffle_cursor_speed_index() }.max(0) as u16,
        _ => 0,
    }
}

/// The value armed row `row` of `section` moves to on one step `dir` from
/// `cur`, stopping at the ends. The default zoom steps through its presets
/// (`loc::zoom_preset_step`), as a game's own does in its options, not the
/// free percentage of the pause: there is no picture here to judge one
/// against.
pub(crate) fn armed_next(section: SetSection, row: usize, cur: u16, dir: i32) -> u16 {
    let n = match (section, key_of(section, row)) {
        (SetSection::Defaults, "AFFICHAGE") => keymap::DISPLAY_MODE_COUNT,
        (SetSection::Defaults, "ROTATION") => keymap::ROTATION_COUNT,
        (SetSection::Defaults, "PIXELS") => keymap::PIXEL_FILTER_COUNT,
        (SetSection::Defaults, "FILTRE") => keymap::SCREEN_FILTER_COUNT,
        (SetSection::Defaults, "OVERCLOCK") => POWER_ORDER.len() as u8,
        (SetSection::Defaults, "VITESSE") => crate::loc::CURSOR_SPEED_X10.len() as u8,
        (SetSection::Defaults, "ZOOM") => return crate::loc::zoom_preset_step(cur, dir),
        _ => return cur,
    };
    armed_step(cur, n as u16, dir)
}

/// Write the value armed row `row` of `section` was kept at.
pub(crate) fn write_armed(section: SetSection, row: usize, v: u16) {
    use crate::loc as l;
    let i = v.min(u8::MAX as u16) as u8;
    match (section, key_of(section, row)) {
        (SetSection::Defaults, "AFFICHAGE") => l::set_default_display_mode(i),
        (SetSection::Defaults, "ROTATION") => l::set_default_rotation(i),
        (SetSection::Defaults, "PIXELS") => l::set_default_pixel_filter(i),
        (SetSection::Defaults, "FILTRE") => l::set_default_screen_filter(i),
        // No clock is touched from here: a raise outside gameplay would be
        // spent entirely on drawing a menu.
        (SetSection::Defaults, "OVERCLOCK") => l::set_default_power_mode(POWER_ORDER[(i as usize).min(POWER_ORDER.len() - 1)]),
        (SetSection::Defaults, "ZOOM") => l::set_default_zoom(v),
        // C++ owns the live value and its file, and knows on its own that out
        // of a game this sets the global default.
        (SetSection::Defaults, "VITESSE") => {
            unsafe { ruffle_cursor_speed_set(i as core::ffi::c_int) };
            return;
        }
        _ => return,
    }
    l::save_current();
    default_written();
}

/// A game default was written: the games that follow it show it in their
/// options, whose copy of the last game's settings is read again.
fn default_written() {
    options_panel::game_prefs_forget();
}

/// A tap on the page, on cell `idx` of its touch table: the column's entries
/// first, then the pane's places (in Bibliothèque the four cards, then its
/// rows). Puts the cursor there, and answers true when it was already there,
/// which the caller turns into A. A layout card is a place like a language:
/// the first tap takes the cursor to it, a tap on the card under the cursor
/// applies it.
pub(crate) fn touch_select(s: &mut State, section: SetSection, in_pane: bool, row: usize, idx: usize) -> bool {
    let column = SetSection::ALL.len();
    if idx < column {
        let target = SetSection::at(idx);
        if !in_pane && target == section {
            return true;
        }
        s.screen = Screen::Settings { section: target, in_pane: false, row: 0 };
        return false;
    }
    let mut cell = idx - column;
    if section == SetSection::Library {
        if cell < LIBRARY_CARDS {
            let v = cell as u8;
            let in_use = crate::loc::home_view();
            let on_cards = in_pane && row == row_of(SetSection::Library, "VUE");
            if on_cards && s.view_focus.unwrap_or(in_use) == v {
                return true;
            }
            s.view_focus = if v == in_use { None } else { Some(v) };
            if on_cards {
                // Same screen, another card: the move the cursor makes.
                crate::sfx::choose(crate::sfx::Sfx::Move);
            }
            s.screen = settings_at(SetSection::Library, "VUE");
            return false;
        }
        cell = cell - LIBRARY_CARDS + 1;
    }
    if cell >= section.pane_len() {
        return false;
    }
    if in_pane && row == cell {
        return true;
    }
    s.screen = Screen::Settings { section, in_pane: true, row: cell };
    false
}

/// A tap on part `part` of the control in cell `idx` of the page's table (see
/// `touch_select` for the cells): an option of a game default is set at once
/// and kept, a switch flips (input rule 6). The cursor goes to that row. The
/// caller has already kept a value armed elsewhere, and plays the sound.
pub(crate) fn touch_part(s: &mut State, section: SetSection, idx: usize, part: usize) {
    use crate::loc as l;
    let column = SetSection::ALL.len();
    let Some(mut cell) = idx.checked_sub(column) else { return };
    if section == SetSection::Library {
        // The cards are tapped as cells, not parts.
        let Some(c) = cell.checked_sub(LIBRARY_CARDS) else { return };
        cell = c + 1;
    }
    if cell >= section.pane_len() {
        return;
    }
    s.screen = Screen::Settings { section, in_pane: true, row: cell };
    let key = key_of(section, cell);
    let i = part.min(u8::MAX as usize) as u8;
    let changed = match (section, key) {
        (SetSection::Library, "SONS") => {
            l::set_ui_sounds(!l::ui_sounds());
            true
        }
        (SetSection::Defaults, "FPS") => {
            l::set_default_fps_counter((l::default_fps_counter() != 1) as u8);
            default_written();
            true
        }
        (SetSection::Defaults, "AFFICHAGE" | "ROTATION" | "PIXELS" | "FILTRE" | "OVERCLOCK") => {
            let cur = default_value(key);
            if part as u16 != cur {
                write_armed(section, cell, i as u16);
                crate::sfx::value_changed();
            }
            return;
        }
        _ => false,
    };
    if changed {
        l::save_current();
        crate::sfx::value_changed();
    }
}

/// The home layouts, which take one place of the cursor and four of the
/// touch table.
const LIBRARY_CARDS: usize = 4;

// ── Flows that leave the page: the keyboard, the network ──────────────────

/// The nickname, read once: the page shows it on every frame, and it lives
/// in a file on the card. Forgotten when it is set, so the next frame reads
/// the new one.
static NICKNAME: Mutex<Option<std::string::String>> = Mutex::new(None);

fn nickname() -> std::string::String {
    if let Ok(g) = NICKNAME.lock() {
        if let Some(n) = g.as_ref() {
            return n.clone();
        }
    }
    let n = crate::profiles::author_name();
    if let Ok(mut g) = NICKNAME.lock() {
        *g = Some(n.clone());
    }
    n
}

/// Profil > Pseudo (#20): the keyboard, prefilled with the current nickname.
/// Empty clears it. Hoisted from `input()`: the keyboard must not run under
/// the library lock.
pub(crate) fn run_pseudo_flow() {
    let current = nickname();
    if let Some(name) = net::prompt_pseudo(&current) {
        crate::profiles::set_author_name(&name);
        if let Ok(mut g) = NICKNAME.lock() {
            *g = None;
        }
        // A value changed in place (the row shows the new name): it clicks,
        // the same name typed again does not.
        if nickname() != current {
            crate::sfx::value_changed();
        }
    }
    if let Ok(mut s) = LIBRARY.lock() {
        s.screen = settings_at(SetSection::Profile, "PSEUDO");
    }
}

/// Aide et retours > Proposer une idée: the keyboard for a free text, posted
/// as a "suggestion" issue (same relay and token as a bug report, another
/// label). No game involved. Hoisted from `input()`: the keyboard must not run
/// under the library lock; the text is then sent behind a card
/// (`submit_report`). Empty input sends nothing.
pub(crate) fn run_suggestion_flow() {
    let lc = crate::loc::s();
    let Some(text) = net::prompt_long(lc.kbd_suggest_header, lc.kbd_suggest_guide) else {
        return;
    };
    let text = text.trim().to_string();
    if text.is_empty() {
        return;
    }
    let report = crate::bugreport::Report {
        kind: "suggestion",
        game: std::string::String::new(),
        file: std::string::String::new(),
        source_url: std::string::String::new(),
        size: 0,
        disk_size: 0,
        swf_version: 0,
        compression: std::string::String::new(),
        as3: false,
        app_version: crate::bugreport::APP_VERSION,
        lang: crate::loc::current().code(),
        applet: unsafe { ruffle_is_applet_mode() } != 0,
        // Empty on purpose: the relay's suggestion branch renders none of the
        // game fields, and there is no game to measure. -1 is the truthful
        // "no companion folder", and a dock state read here would describe the
        // menu the idea was typed in, which explains nothing.
        companions: -1,
        companion_dirs: -1,
        power_mode: 0,
        power_refused: false,
        power_auto_pct: -1,
        docked: false,
        description: text,
    };
    submit_report(report, "IDEE");
}

/// A report the relay refused, kept with the reason, so Réessayer sends it
/// again without the description being typed a second time. Fermer leaves it
/// here: only the question reads it, and only a failed send opens that.
static FAILED_SEND: Mutex<Option<(crate::bugreport::Report, std::string::String)>> = Mutex::new(None);

/// A report on its way, with the row it started on and whether the page has
/// shown the card saying so (`drive_pending_send`).
static PENDING_SEND: Mutex<Option<(crate::bugreport::Report, &'static str, bool)>> = Mutex::new(None);

/// Send `report`, which started on the Aide et retours row `key`, on the next
/// frames: the Aide et retours page is drawn once with a card saying it is
/// being sent, then the POST runs. Run at once from the keyboard's flow, it
/// froze the last frame from before the keyboard for as long as the relay took
/// (up to 30 s on a dead connection), with nothing saying anything was going on.
///
/// The row is carried rather than derived from `report.kind`: it is a fact
/// about the menu, and dismissing a sent suggestion once landed the cursor on
/// the bug row above the button just pressed (#113).
pub(crate) fn submit_report(report: crate::bugreport::Report, key: &'static str) {
    if let Ok(mut g) = PENDING_SEND.lock() {
        *g = Some((report, key, false));
    }
    if let Ok(mut s) = LIBRARY.lock() {
        let row = row_of(SetSection::Help, key);
        s.screen = Screen::Settings { section: SetSection::Help, in_pane: true, row };
    }
}

/// Whether a report is on its way: the page shows the card, and no press
/// reaches it until the relay has answered.
pub(crate) fn sending() -> bool {
    PENDING_SEND.lock().map(|g| g.is_some()).unwrap_or(false)
}

/// The report `submit_report` queued: the first call lets the frame with the
/// card be drawn, the next one sends it. Called at the top of `render`, like
/// the profile flows' `drive_pending_net`.
pub(crate) fn drive_pending_send() {
    let job = {
        let Ok(mut g) = PENDING_SEND.lock() else { return };
        match g.take() {
            None => return,
            Some((report, key, false)) => {
                *g = Some((report, key, true));
                return;
            }
            Some((report, key, true)) => (report, key),
        }
    };
    send_report(job.0, job.1);
}

/// Send `report` now. Sent: a toast says so and the cursor is back on the row,
/// the tab never left. Not sent: a question over the page gives the reason and
/// offers to try again, the report kept for it.
fn send_report(report: crate::bugreport::Report, key: &'static str) {
    // The success line follows the FLOW, not the relay (#83): both go through
    // the same endpoint, but "your report was sent" after an idea reads like
    // the button sent the wrong thing.
    let lc = crate::loc::s();
    let sent = if report.kind == "suggestion" { lc.suggest_ok_msg } else { lc.bug_ok_msg };
    let row = row_of(SetSection::Help, key);
    match crate::bugreport::submit(&report) {
        Ok(()) => {
            if let Ok(mut g) = FAILED_SEND.lock() {
                *g = None;
            }
            if let Ok(mut s) = LIBRARY.lock() {
                set_toast(&mut s, sent.to_string(), TOAST_OK);
                s.screen = Screen::Settings { section: SetSection::Help, in_pane: true, row };
            }
            // Played now, with the toast: this runs from `render`, a frame
            // after the card, outside any press. A sound only chosen here
            // waited for the next press, which then made it instead of its
            // own (a back for B, a move for Down).
            crate::sfx::play(crate::sfx::Sfx::Decide);
        }
        Err(e) => {
            if let Ok(mut g) = FAILED_SEND.lock() {
                *g = Some((report, e));
            }
            if let Ok(mut s) = LIBRARY.lock() {
                // On Réessayer: the question is there to be answered with one
                // press, and nothing is lost either way. A retry that fails
                // again comes back to it from the page with the card, so the
                // note `render` plays for an error arriving marks it too.
                s.screen = Screen::SendFailed { row, selection: 1 };
            }
        }
    }
}

/// A on Réessayer: the kept report, sent again behind the card
/// (`submit_report`). Hoisted from `input()` with the other report flows.
pub(crate) fn run_retry_flow(row: usize) {
    let kept = FAILED_SEND.lock().ok().and_then(|mut g| g.take());
    match kept {
        Some((report, _)) => submit_report(report, key_of(SetSection::Help, row)),
        None => {
            if let Ok(mut s) = LIBRARY.lock() {
                s.screen = Screen::Settings { section: SetSection::Help, in_pane: true, row };
            }
        }
    }
}

/// The question after a refused report: Fermer (0) or Réessayer (1), side by
/// side. A on Réessayer is hoisted, so an A that reaches here is Fermer.
pub(crate) fn handle_send_failed_input(s: &mut State, button: &str, row: usize) {
    match button {
        // The answers side by side (input rule 4).
        "Left" | "StickLLeft" | "Right" | "StickLRight" => {
            s.screen = Screen::SendFailed { row, selection: answer_at(button, 0) }
        }
        "A" | "B" | "Minus" => {
            // The kept report stays: the question is drawn again through its
            // close, its reason read from there, and a reason gone empty would
            // shrink the box as it leaves. The next send replaces or clears it.
            s.screen = Screen::Settings { section: SetSection::Help, in_pane: true, row };
        }
        _ => {}
    }
}

// ── Folders ───────────────────────────────────────────────────────────────

/// The folder the games-folder browser opened on (beside the library). B
/// leaves from there and from the folders above it, which lead back to it;
/// anywhere else it climbs.
static BROWSE_START: Mutex<std::string::String> = Mutex::new(std::string::String::new());

/// Whether B in the browser showing `path` climbs a level (true) or leaves it.
///
/// The browser opens beside the library, and B there used to climb on to the
/// card's root before a second B left (a third for a library in
/// `/roms/flashnx`), while the footer said "Remonter" and the crumb
/// "Bibliothèque". So B leaves on the folder it opened on and on the ones
/// above it, the way the player came. Everywhere else it climbs: below that
/// folder, and in another branch of the card reached through the root, where
/// leaving at once lost the whole walk down. Never at the card's root, which
/// has nothing above it.
pub(crate) fn b_climbs(path: &str) -> bool {
    let start = BROWSE_START.lock().map(|g| g.clone()).unwrap_or_default();
    browse_parent(path).is_some() && !same_dir(path, &start) && !inside_dir(&start, path)
}

/// A tap on a page opened in the pane, `k` places past its list
/// (`draw_settings` publishes them after the rows): the crumb over the page
/// (`k` 0) goes back to the row that opened it; an entry of the column opens
/// that section, as on the tab itself.
pub(crate) fn subpage_tap(s: &mut State, k: usize) {
    let back = match s.screen {
        Screen::GamesDirPicker { .. } => settings_at(SetSection::Library, "DOSSIER"),
        Screen::BugPicker { .. } => settings_at(SetSection::Help, "BUG"),
        _ => return,
    };
    let target = match k.checked_sub(1) {
        None => back,
        Some(i) => match SetSection::ALL.get(i) {
            Some(&section) => Screen::Settings { section, in_pane: false, row: 0 },
            None => return,
        },
    };
    // Out of the page, to the row that opened it or on to the column: the way
    // back, with B's sound. A tap has no press for the screens to read it
    // from, and the cursor moving out of the page sounded like a step down a
    // list.
    crate::sfx::choose(crate::sfx::Sfx::Back);
    s.dir_browse.clear();
    s.screen = target;
}

/// `name` inside the folder `path` (`sdmc:/` already ends in a separator).
pub(crate) fn child_dir(path: &str, name: &str) -> std::string::String {
    if path.ends_with('/') {
        std::format!("{}{}", path, name)
    } else {
        std::format!("{}/{}", path, name)
    }
}

/// Whether `path` is the card's root, which cannot take the library: the
/// paths of the move and the library's own root are built for a folder.
pub(crate) fn is_card_root(path: &str) -> bool {
    path.trim_end_matches('/') == "sdmc:"
}

/// Whether two folder paths name the same folder, a trailing slash or not.
pub(crate) fn same_dir(a: &str, b: &str) -> bool {
    a.trim_end_matches('/') == b.trim_end_matches('/')
}

/// Whether `path` is INSIDE the library at `root` (not the library itself).
/// Such a folder cannot take the library: moving it there renames folders
/// into themselves, which fails, and the games of a shelf chosen that way lose
/// their folder.
pub(crate) fn inside_library(path: &str, root: &str) -> bool {
    inside_dir(path, root)
}

/// Whether `path` is a folder somewhere under `dir` (not `dir` itself), a
/// trailing slash or not: the card's root is written `sdmc:/`, and read with
/// its slash it passed for a folder inside itself.
fn inside_dir(path: &str, dir: &str) -> bool {
    let (path, dir) = (path.trim_end_matches('/'), dir.trim_end_matches('/'));
    !dir.is_empty() && path.len() > dir.len() && path.starts_with(dir) && path.as_bytes()[dir.len()] == b'/'
}

/// Whether `path` is the library at `root` or a folder inside it: nowhere a
/// new folder could take the library either, so the browser makes none there.
pub(crate) fn within_library(path: &str, root: &str) -> bool {
    same_dir(path, root) || inside_library(path, root)
}

/// A path as the page shows it: from the card's root, `sdmc:` left out.
fn shown_path(path: &str) -> &str {
    if path == "sdmc:/" {
        "/"
    } else {
        path.trim_start_matches("sdmc:")
    }
}

// ── Drawing ───────────────────────────────────────────────────────────────

/// What the pane holds.
#[derive(Clone, Copy)]
enum Body {
    /// The section itself; `row` is the cursor when `in_pane`.
    Section { in_pane: bool, row: usize },
    /// The browser of the games folder (Bibliothèque).
    Folders { selection: usize, scroll: usize },
    /// The games a bug report can be about (Aide et retours).
    Games { selection: usize, scroll: usize },
}

/// Glide key of the cursor in `section`'s pane: the same key across the
/// section's blocks, so the pill glides from one block to the next.
fn pane_glide(section: SetSection) -> u32 {
    0x5E20_0000 | section.index() as u32
}

/// The tab's page for `screen`: the tab itself, a page opened in its pane, or
/// with `under` the page as it stands behind a window (the controls editor, a
/// question), still and out of reach of the finger. `folder` is the browser's
/// folder and its subfolders, snapshotted by the caller.
pub(crate) fn draw_settings(
    backend: &mut SwitchRenderBackend,
    screen: Screen,
    folder: (&str, &[std::string::String]),
    logo: (u32, u32, u32),
    game_count: usize,
    under: bool,
) {
    let u = crate::loc_ui::ui();
    let (section, column, body) = match screen {
        Screen::Settings { section, in_pane, row } => (section, !in_pane, Body::Section { in_pane, row }),
        Screen::GamesDirPicker { selection, scroll } => {
            (SetSection::Library, false, Body::Folders { selection, scroll })
        }
        // The question is about the folder on show, asked from its first row.
        Screen::GamesDirConfirm { .. } => (SetSection::Library, false, Body::Folders { selection: 0, scroll: 0 }),
        Screen::BugPicker { selection, scroll_offset } => {
            (SetSection::Help, false, Body::Games { selection, scroll: scroll_offset })
        }
        Screen::SettingsKeymapEditor => (
            SetSection::Defaults,
            false,
            Body::Section { in_pane: true, row: row_of(SetSection::Defaults, "TOUCHES") },
        ),
        Screen::SendFailed { row, .. } => (SetSection::Help, false, Body::Section { in_pane: true, row }),
        _ => return,
    };
    if !under && matches!(screen, Screen::Settings { .. }) {
        LAST_SECTION.store(section.index(), core::sync::atomic::Ordering::Relaxed);
    }
    // The value the armed row shows (`Armed`), drawn in place of the stored
    // one until A keeps it, and the layout card under the cursor.
    let (armed, view_focus) = LIBRARY
        .lock()
        .ok()
        .map(|s| (s.armed.filter(|a| a.screen == screen).map(|a| a.cur), s.view_focus))
        .unwrap_or((None, None));
    backend.settings_still(under);
    backend.library_clear();
    let entries = [
        (Icon::Grid, u.sec_library),
        (Icon::Gamepad, u.row_game_prefs),
        (Icon::Globe, u.row_language),
        (Icon::User, u.sec_profile),
        (Icon::Bug, u.sec_help),
        (Icon::Info, u.sec_about),
        (Icon::Power, u.row_quit),
    ];
    let mut cells = backend.settings_nav(&entries, section.index(), column && !under);
    let pane_key = match body {
        Body::Section { .. } => section.index() as u32,
        Body::Folders { .. } => 0x100,
        Body::Games { .. } => 0x101,
    };
    let saved = backend.settings_pane_begin(pane_key);
    let mut hints: std::vec::Vec<HintItem<'static>> = std::vec::Vec::new();
    // The parts of the pane's controls a finger can tap, by cell of `cells`.
    let mut segs: crate::backend::render::CtrlSegs = std::vec::Vec::new();
    let mut font_cold = false;
    // The rows a page opened in the pane published for its list.
    let mut list_rows = 0;
    match body {
        Body::Section { in_pane, row } => {
            let focus = if in_pane { Some(row) } else { None };
            match section {
                SetSection::Library => library_pane(backend, focus, view_focus, game_count, &mut cells, &mut segs, &mut hints),
                SetSection::Defaults => defaults_pane(backend, focus, armed, &mut cells, &mut segs, &mut hints),
                SetSection::Language => {
                    font_cold = language_font_cold();
                    if font_cold {
                        backend.settings_title(u.row_language, None);
                    } else {
                        language_pane(backend, focus, &mut cells, &mut hints);
                    }
                }
                SetSection::Profile => profile_pane(backend, focus, &mut cells, &mut hints),
                SetSection::Help => help_pane(backend, focus, game_count, &mut cells, &mut hints),
                SetSection::About => about_pane(backend, logo),
                SetSection::Quit => quit_pane(backend),
            }
        }
        Body::Folders { selection, scroll } => {
            list_rows = folders_page(backend, folder, selection, scroll, &mut hints);
        }
        Body::Games { selection, scroll } => list_rows = games_page(backend, selection, scroll, &mut hints),
    }
    backend.settings_pane_end(saved);
    if column {
        hints = column_hints(section);
    }
    // A page in the pane is not the tab's home, so the tab strip is not drawn
    // for it after this: it is drawn here, and nothing on it can be tapped.
    // Nor do L and R switch tabs from it (the bug list pages with them), so
    // the strip goes without its L and R chips.
    if !matches!(screen, Screen::Settings { .. }) {
        backend.draw_navbar_with(Tab::Reglages.index(), logo, false);
    }
    backend.settings_footer(if under { &[] } else { &hints });
    // The pages opened in the pane publish their lists themselves; under a
    // window nothing here may be tapped. After a page's rows come the crumb
    // over it, which goes back like B, and the column beside it, which opens
    // a section as on the tab (`subpage_tap`): neither could be tapped there,
    // and the crumb drawn as a way back said it could.
    let sending = sending();
    if !under && !sending {
        match body {
            Body::Section { .. } => backend.settings_publish(cells, segs),
            Body::Folders { .. } | Body::Games { .. } => {
                let crumb = if matches!(body, Body::Folders { .. }) { u.sec_library } else { u.sec_help };
                let mut tail = std::vec![backend.settings_crumb_cell(crumb)];
                tail.extend(cells);
                backend.settings_publish_tail(list_rows, tail);
            }
        }
    }
    backend.settings_still(false);
    if font_cold {
        backend.draw_loading_overlay(crate::loc::s().lang_title, unsafe { ruffle_tick_now() });
    }
    if sending {
        backend.settings_loading_card(u.loading_report, unsafe { ruffle_tick_now() });
    }
}

/// Without the console's fonts, the first frame that names the languages pulls
/// the Chinese glyphs into the old atlas, which is slow: that frame shows a
/// spinner first, the next one the names. The console's fonts parse the Chinese
/// face on first use at no cost worth a spinner.
fn language_font_cold() -> bool {
    use std::sync::atomic::Ordering;
    let cold = LANG_FONT_WARM.load(Ordering::Relaxed) == 0 && !crate::backend::render::kit_fonts_available();
    LANG_FONT_WARM.store(if cold { 1 } else { 2 }, Ordering::Relaxed);
    cold
}

/// What the buttons do with the cursor in the column.
fn column_hints(section: SetSection) -> std::vec::Vec<HintItem<'static>> {
    let u = crate::loc_ui::ui();
    match section {
        SetSection::Quit => std::vec![(&[Btn::A][..], u.quit)],
        // Only things to read: A has nothing to do.
        SetSection::About => std::vec::Vec::new(),
        _ => std::vec![(&[Btn::A][..], u.open)],
    }
}

/// A way through the menus, step by step, joined the way crumbs join them. The
/// steps are the labels the menus themselves show, so a help that names a
/// place names it in the words the player will find there, in every language.
///
/// The spaces are no-break ones: `kit_wrap` breaks a help only on the others,
/// so the path goes to the next line whole, its double spacing kept. Joined
/// with plain spaces, a help ended one line on "Pausa" and began the next on
/// "› Imagen › Zoom.", and every path lost its spacing to the wrap, which puts
/// one space between words.
pub(crate) fn menu_path(steps: &[&str]) -> std::string::String {
    steps.join("\u{a0}\u{a0}\u{203A}\u{a0}\u{a0}")
}

/// Bibliothèque: the home layouts as cards, the games folder, the interface
/// sounds, each with what it is for underneath. `view_focus` is the card the
/// cursor is on when it is not the layout in use.
fn library_pane(
    b: &mut SwitchRenderBackend,
    focus: Option<usize>,
    view_focus: Option<u8>,
    game_count: usize,
    cells: &mut std::vec::Vec<(f32, f32, f32, f32)>,
    segs: &mut crate::backend::render::CtrlSegs,
    hints: &mut std::vec::Vec<HintItem<'static>>,
) {
    let u = crate::loc_ui::ui();
    let lib = SetSection::Library;
    let glide = pane_glide(lib);
    let on_row = |key: &str| focus.filter(|r| *r == row_of(lib, key)).map(|_| 0);
    let top = b.settings_title(u.sec_library, None);
    let in_use = crate::loc::home_view();
    // The card under the cursor, when the cursor is on the cards: the one in
    // use until Left / Right walk away from it.
    let card = on_row("VUE").map(|_| view_focus.unwrap_or(in_use));
    let desc = match card.unwrap_or(in_use) {
        1 => u.view_desc_list,
        2 => u.view_desc_strip,
        3 => u.view_desc_shelf,
        _ => u.view_desc_grid,
    };
    let (c, y) = b.settings_layouts(top, u.row_home_view, in_use, card, desc);
    cells.extend(c);
    let root = primary_root();
    let folder = [Row::new(Icon::Folder, u.row_games_dir).value(shown_path(&root)).opens()];
    let (c, y, _) = b.settings_block(y + 8.0, &folder, on_row("DOSSIER"), glide);
    cells.extend(c);
    let line = std::format!("{}  \u{00B7}  {}", u.help_games_dir, crate::loc_ui::games(game_count));
    let y = b.settings_note(y + 8.0, &line, 3);
    let on = crate::loc::ui_sounds();
    let sounds = [Row::new(if on { Icon::Volume } else { Icon::VolumeOff }, u.row_sounds).toggle(on)];
    let (c, y, sg) = b.settings_block(y + 18.0, &sounds, on_row("SONS"), glide);
    // The switch's knob, counted as a cell of the page's table.
    let base = cells.len();
    segs.extend(sg.into_iter().map(|(r, p, rect)| (base + r, p, rect)));
    cells.extend(c);
    b.settings_note(y + 8.0, u.help_sounds, 2);
    *hints = match focus.map(|r| key_of(lib, r)) {
        // As on the languages: A only where it would change something.
        Some("VUE") if card == Some(in_use) => std::vec![(&[Btn::B][..], u.back)],
        Some("VUE") => std::vec![(&[Btn::A][..], u.apply), (&[Btn::B][..], u.back)],
        Some("DOSSIER") => std::vec![(&[Btn::A][..], u.open), (&[Btn::B][..], u.back)],
        Some(_) => std::vec![(&[Btn::A][..], u.change), (&[Btn::B][..], u.back)],
        None => std::vec::Vec::new(),
    };
}

/// What Left / Right, A and B do on an armed row (input rule 2).
fn armed_hints() -> std::vec::Vec<HintItem<'static>> {
    let u = crate::loc_ui::ui();
    std::vec![(&[Btn::DpadLR][..], u.change), (&[Btn::A][..], u.validate), (&[Btn::B][..], u.cancel)]
}

/// Jeu par défaut: what every game starts with until it is set on its own, in
/// the groups and with the controls and options of a game's options and of the
/// pause, so a setting looks the same wherever it is changed (the rotation's
/// first option is "0°" on all three). The box under the rows explains the row
/// with the cursor, Overclock setting by setting. `armed` is the value the row
/// under the cursor shows while armed.
fn defaults_pane(
    b: &mut SwitchRenderBackend,
    focus: Option<usize>,
    armed: Option<u16>,
    cells: &mut std::vec::Vec<(f32, f32, f32, f32)>,
    segs: &mut crate::backend::render::CtrlSegs,
    hints: &mut std::vec::Vec<HintItem<'static>>,
) {
    use crate::loc as l;
    let lc = l::s();
    let u = crate::loc_ui::ui();
    let def = SetSection::Defaults;
    let top = b.settings_title(u.row_game_prefs, Some(u.prefs_sub));
    let key = focus.map(|r| key_of(def, r));
    // A row's value: the armed one on the armed row, the stored one elsewhere.
    let value = |k: &str| match (key, armed) {
        (Some(fk), Some(v)) if fk == k => v,
        _ => default_value(k),
    };
    let is_armed = |k: &str| armed.is_some() && key == Some(k);
    let display = [l::display_mode_label(0), l::display_mode_label(1), l::display_mode_label(2)];
    let rotation = [l::rotation_label(0), l::rotation_label(1), l::rotation_label(2), l::rotation_label(3)];
    let pixels = [l::pixel_filter_label(0), l::pixel_filter_label(1), l::pixel_filter_label(2)];
    let filters = [l::screen_filter_label(0), l::screen_filter_label(1), l::screen_filter_label(2)];
    let power_at = value("OVERCLOCK") as usize;
    let power = POWER_ORDER[power_at.min(POWER_ORDER.len() - 1)];
    let power_opts = POWER_ORDER.map(l::overclock_label);
    let zoom_v = value("ZOOM");
    let zoom = crate::loc_ui::percent(zoom_v as u32);
    let zoom_ends = l::zoom_preset_ends(zoom_v);
    let n_speeds = l::CURSOR_SPEED_X10.len();
    let speed_idx = (value("VITESSE") as usize).min(n_speeds.saturating_sub(1));
    let speed = l::speed_label(speed_idx as i32);
    // In the order of `DEFAULTS_ROWS`, which the cursor and the help read.
    let rows = [
        Row::new(Icon::Gamepad, lc.keys_title).opens(),
        Row::new(Icon::Pointer, lc.set_cursor_speed)
            .cycle(&speed)
            .ends(speed_idx == 0, speed_idx + 1 >= n_speeds)
            .armed_if(is_armed("VITESSE")),
        Row::new(Icon::Monitor, lc.set_display_mode)
            .choice(&display, value("AFFICHAGE") as usize)
            .armed_if(is_armed("AFFICHAGE")),
        Row::new(Icon::Rotate, lc.set_rotation).choice(&rotation, value("ROTATION") as usize).armed_if(is_armed("ROTATION")),
        Row::new(Icon::ZoomIn, lc.set_zoom).cycle(&zoom).ends(zoom_ends.0, zoom_ends.1).armed_if(is_armed("ZOOM")),
        Row::new(Icon::Pixels, lc.set_pixel_filter).choice(&pixels, value("PIXELS") as usize).armed_if(is_armed("PIXELS")),
        Row::new(Icon::Tv, lc.set_screen_filter).choice(&filters, value("FILTRE") as usize).armed_if(is_armed("FILTRE")),
        Row::new(Icon::Gauge, lc.set_fps).toggle(l::default_fps_counter() == 1),
        Row::new(Icon::Cpu, lc.set_overclock).choice(&power_opts, power_at).armed_if(is_armed("OVERCLOCK")),
    ];
    let sections = [
        (row_of(def, "TOUCHES"), u.sec_controls),
        (row_of(def, "AFFICHAGE"), u.sec_image),
        (row_of(def, "OVERCLOCK"), u.sec_perf),
    ];
    let built: std::string::String;
    let help: &str = match key {
        // The cursor still in the column: what the section is about.
        None => u.help_game_prefs,
        Some("AFFICHAGE") => u.help_display,
        Some("ROTATION") => u.help_rotation,
        Some("ZOOM") => {
            // Where the zoom is fine-tuned in a game: the pause's own row and
            // page names, which are not the same words in every language.
            let path = menu_path(&[lc.pause_title, lc.menu_screen, lc.set_zoom]);
            built = u.help_zoom_default.replacen("{}", &path, 1);
            &built
        }
        Some("PIXELS") => u.help_pixels,
        Some("FILTRE") => u.help_filter,
        Some("OVERCLOCK") => {
            let base = match power {
                0 => u.help_oc_off,
                keymap::POWER_AUTO => u.help_oc_auto,
                _ => u.help_oc_on,
            };
            // The battery veto, when the last game asked for a raise and the
            // console said no: the setting alone would not explain the speed.
            if power != 0 && crate::backend::render::overclock_refused() {
                built = std::format!("{} {}", base, lc.overclock_refused);
                &built
            } else {
                base
            }
        }
        Some("FPS") => u.help_fps,
        Some("TOUCHES") => {
            // A game's own controls, in the words of the menus that set them,
            // joined so the path does not break around its arrows.
            built = u.help_keys_default.replacen("{}", &menu_path(&[u.hint_options, lc.opt_keys]), 1);
            &built
        }
        Some(_) => u.help_cursor_default,
    };
    let (c, sg) = b.settings_rows(top + 4.0, &rows, &sections, focus.unwrap_or(usize::MAX), pane_glide(def), help);
    let base = cells.len();
    segs.extend(sg.into_iter().map(|(r, p, rect)| (base + r, p, rect)));
    cells.extend(c);
    *hints = match key {
        _ if armed.is_some() => armed_hints(),
        Some("TOUCHES") => std::vec![(&[Btn::A][..], u.open), (&[Btn::B][..], u.back)],
        Some("FPS") => std::vec![(&[Btn::A][..], u.change), (&[Btn::B][..], u.back)],
        Some(_) => std::vec![(&[Btn::A][..], u.edit), (&[Btn::B][..], u.back)],
        None => std::vec::Vec::new(),
    };
}

/// Langue: the nine languages, applied with A.
fn language_pane(
    b: &mut SwitchRenderBackend,
    focus: Option<usize>,
    cells: &mut std::vec::Vec<(f32, f32, f32, f32)>,
    hints: &mut std::vec::Vec<HintItem<'static>>,
) {
    let u = crate::loc_ui::ui();
    let top = b.settings_title(u.row_language, None);
    let codes: std::vec::Vec<std::string::String> =
        crate::loc::PICKER_LANGS.iter().map(|l| l.code().to_uppercase()).collect();
    // Each language under its own name, and under that its name in the
    // interface's language (the way the console's own picker writes them):
    // flags were the other idea, but a language is not a country (English,
    // Portuguese and Chinese each belong to several).
    let langs: std::vec::Vec<(&str, &str, &str)> = crate::loc::PICKER_LANGS
        .iter()
        .zip(codes.iter())
        .map(|(l, c)| (c.as_str(), l.picker_name(), l.name_in_ui()))
        .collect();
    let active = crate::loc::current().index();
    let (c, y) = b.settings_languages(top + 8.0, &langs, active, focus, u.lang_current);
    cells.extend(c);
    b.settings_note(y + 16.0, u.help_language, 2);
    *hints = match focus {
        // On the language in use, A has nothing to apply.
        Some(f) if f == active => std::vec![(&[Btn::B][..], u.back)],
        Some(_) => std::vec![(&[Btn::A][..], u.apply), (&[Btn::B][..], u.back)],
        None => std::vec::Vec::new(),
    };
}

/// Profil: the nickname shown on the controls a player shares, and where
/// sharing them happens.
fn profile_pane(
    b: &mut SwitchRenderBackend,
    focus: Option<usize>,
    cells: &mut std::vec::Vec<(f32, f32, f32, f32)>,
    hints: &mut std::vec::Vec<HintItem<'static>>,
) {
    let lc = crate::loc::s();
    let u = crate::loc_ui::ui();
    let top = b.settings_title(u.sec_profile, None);
    let name = nickname();
    let row = [Row::new(Icon::User, u.row_pseudo)
        .value(if name.is_empty() { u.not_set } else { name.as_str() })
        .keyboard()];
    let (c, y, _) = b.settings_block(top, &row, focus.map(|_| 0), pane_glide(SetSection::Profile));
    cells.extend(c);
    let y = b.settings_note(y + 8.0, u.help_pseudo, 2);
    // The way to a game's sharing, in the words its menus use.
    let path = menu_path(&[u.hint_options, lc.opt_keys, lc.opt_share]);
    b.settings_note_icon(y + 16.0, Icon::Share, &u.help_share_where.replacen("{}", &path, 1), ui_col::TEXT3);
    if focus.is_some() {
        *hints = std::vec![(&[Btn::A][..], u.type_text), (&[Btn::B][..], u.back)];
    }
}

/// Aide et retours: report a bug (a game list opens in the pane), suggest an
/// idea (the keyboard), and where both end up.
fn help_pane(
    b: &mut SwitchRenderBackend,
    focus: Option<usize>,
    game_count: usize,
    cells: &mut std::vec::Vec<(f32, f32, f32, f32)>,
    hints: &mut std::vec::Vec<HintItem<'static>>,
) {
    let lc = crate::loc::s();
    let u = crate::loc_ui::ui();
    let help = SetSection::Help;
    let glide = pane_glide(help);
    let (bug_row, idea_row) = (row_of(help, "BUG"), row_of(help, "IDEE"));
    let top = b.settings_title(u.sec_help, None);
    let empty = game_count == 0;
    let count = crate::loc_ui::games(game_count);
    let bug = [Row::new(Icon::Bug, u.row_bug)
        .value(if empty { "" } else { count.as_str() })
        .opens_if(!empty)
        .dim(empty)];
    let (c, y, _) = b.settings_block(top, &bug, focus.filter(|r| *r == bug_row).map(|_| 0), glide);
    cells.extend(c);
    let y = b.settings_note(y + 8.0, if empty { lc.bug_no_games } else { u.help_bug }, 3);
    let idea = [Row::new(Icon::Lightbulb, u.row_suggest).keyboard()];
    let (c, y, _) = b.settings_block(y + 18.0, &idea, focus.filter(|r| *r == idea_row).map(|_| 0), glide);
    cells.extend(c);
    let y = b.settings_note(y + 8.0, u.help_suggest, 3);
    b.settings_note_icon(y + 16.0, Icon::Link, "github.com/Jonathan8520/FlashNX/issues", ui_col::TEXT2);
    *hints = match focus {
        Some(r) if r == bug_row && empty => std::vec![(&[Btn::B][..], u.back)],
        Some(r) if r == bug_row => std::vec![(&[Btn::A][..], u.open), (&[Btn::B][..], u.back)],
        Some(_) => std::vec![(&[Btn::A][..], u.type_text), (&[Btn::B][..], u.back)],
        None => std::vec::Vec::new(),
    };
}

/// À propos: the version, who made FlashNX and under which licence, what it
/// is built on, and the memory it runs with.
fn about_pane(b: &mut SwitchRenderBackend, logo: (u32, u32, u32)) {
    let lc = crate::loc::s();
    let u = crate::loc_ui::ui();
    let top = b.settings_title(u.sec_about, None);
    let version = u.about_version.replacen("{}", crate::bugreport::APP_VERSION, 1);
    // The author's name is the licence's attribution: written as it is, in
    // every language, like the credit in the footer band.
    let by = u.about_by.replacen("{}", "Jonathan8520", 1);
    let y = b.settings_about_header(top + 4.0, logo, &version, &by);
    let applet = unsafe { ruffle_is_applet_mode() } != 0;
    let rows = [
        Row::new(Icon::Code, u.about_source).value("github.com/Jonathan8520/FlashNX"),
        Row::new(Icon::Play, u.about_engine).value("Ruffle"),
        Row::new(Icon::Image, u.about_icons).value("Lucide (ISC)"),
        Row::new(Icon::HardDrive, u.about_memory).value(if applet { u.mem_applet } else { u.mem_full }),
    ];
    let (_, y, _) = b.settings_block(y + 14.0, &rows, None, pane_glide(SetSection::About));
    // Said here before a launch fails, not only after; and with the whole
    // memory, what that word means (the row alone read as jargon).
    if applet {
        b.settings_note(y + 8.0, lc.applet_notice, 3);
    } else {
        b.settings_note(y + 8.0, u.mem_full_note, 3);
    }
}

/// Quitter: what it does, beside the entry that does it.
fn quit_pane(b: &mut SwitchRenderBackend) {
    let u = crate::loc_ui::ui();
    let top = b.settings_title(u.row_quit, None);
    b.settings_note(top, u.help_quit_app, 2);
}

/// Bibliothèque > Dossier des jeux: the card one level at a time, inside the
/// pane. "Use this folder" and "Parent folder" act on the folder shown, so
/// they stay put while the listing under them scrolls.
fn folders_page(
    b: &mut SwitchRenderBackend,
    folder: (&str, &[std::string::String]),
    selection: usize,
    scroll: usize,
    hints: &mut std::vec::Vec<HintItem<'static>>,
) -> usize {
    let lc = crate::loc::s();
    let u = crate::loc_ui::ui();
    let (path, listing) = folder;
    let top = b.settings_subpage_title(u.sec_library, u.row_games_dir, shown_path(path));
    let up = browse_parent(path).is_some();
    let root = primary_root();
    let mut rows: std::vec::Vec<Row> = std::vec::Vec::with_capacity(listing.len() + 2);
    // The library's own folder cannot be chosen again: the row says it is the
    // one in use instead of closing the browser without a word.
    rows.push(if same_dir(path, &root) {
        Row::new(Icon::CircleCheck, u.dir_current).dim(true)
    } else if is_card_root(path) {
        // The root is no destination: said on the row A would act on, with
        // the mark of a note rather than of the action it does not offer.
        Row::new(Icon::Info, u.dir_root_refused).dim(true)
    } else {
        Row::new(Icon::Check, u.dir_choose)
    });
    if up {
        rows.push(Row::new(Icon::ParentFolder, u.dir_up));
    }
    for name in listing.iter() {
        let full = child_dir(path, name);
        let row = Row::new(Icon::Folder, name);
        rows.push(if same_dir(&full, &root) {
            // The folder the library uses, marked: without it the listing is
            // a wall of names and nothing says which one you are in.
            row.opens().value(lc.games_dir_current).mark(Icon::CircleCheck, ui_col::OK)
        } else if inside_library(&full, &root) {
            // The library's own shelves are no place to move it to.
            row.value(u.in_library).dim(true)
        } else {
            row.opens()
        });
    }
    let pinned = 1 + up as usize;
    // Keyed on the PATH: walking into a folder replaces every row at once,
    // and a glide across that would show the new listing streaking past under
    // a cursor travelling to a row it was never on. A new key snaps instead.
    let mut key: u32 = 2166136261;
    for c in path.as_bytes() {
        key ^= *c as u32;
        key = key.wrapping_mul(16777619);
    }
    let key = 0x8000_0000 | (key & 0x00FF_FFFF);
    b.settings_dir_list(top, &rows, selection, scroll, pinned, pinned + GAMES_DIR_VISIBLE, key);
    let dim = rows.get(selection).is_none_or(|r| r.dim);
    let a_verb = if dim {
        None
    } else if selection == 0 {
        Some(u.choose)
    } else if up && selection == 1 {
        Some(u.go_up)
    } else {
        Some(u.open)
    };
    hints.clear();
    if let Some(v) = a_verb {
        hints.push((&[Btn::A][..], v));
    }
    // In the library, a new folder would be one more greyed row: X makes none
    // there (`run_new_folder_flow` says why if it is pressed anyway).
    if !within_library(path, &root) {
        hints.push((&[Btn::X][..], lc.games_dir_new));
    }
    hints.push((&[Btn::B][..], if b_climbs(path) { u.go_up } else { u.back }));
    rows.len()
}

/// Aide et retours > Signaler un bug: which game? Last played first, each with
/// its cover and the time played, which is usually enough to find the one that
/// just broke.
fn games_page(
    b: &mut SwitchRenderBackend,
    selection: usize,
    scroll: usize,
    hints: &mut std::vec::Vec<HintItem<'static>>,
) -> usize {
    let u = crate::loc_ui::ui();
    let top = b.settings_subpage_title(u.sec_help, u.row_bug, u.bug_pick_sub);
    let games = LIBRARY
        .lock()
        .ok()
        .map(|s| {
            bug_picker_order(&s.entries)
                .into_iter()
                .filter_map(|i| s.entries.get(i))
                .map(|e| {
                    let secs = crate::playtime::get(&e.basename);
                    let played = if secs >= 60 { crate::loc_ui::playtime(secs) } else { std::string::String::new() };
                    (e.display_name.clone(), e.basename.clone(), played)
                })
                .collect::<std::vec::Vec<_>>()
        })
        .unwrap_or_default();
    let refs: std::vec::Vec<(&str, &str, &str)> =
        games.iter().map(|(n, b, p)| (n.as_str(), b.as_str(), p.as_str())).collect();
    b.settings_game_list(top, &refs, selection, scroll, BUG_PICKER_VISIBLE_ROWS);
    *hints = std::vec![
        (&[Btn::A][..], u.choose),
        (&[Btn::L, Btn::R][..], u.page),
        (&[Btn::B][..], u.back),
    ];
    refs.len()
}

/// "Move the whole library here?", over the browser: how many games, and
/// where to. With none, what still moves (the settings and the covers).
pub(crate) fn draw_move_question(backend: &mut SwitchRenderBackend, selection: usize, dir_path: &str, game_count: usize) {
    let lc = crate::loc::s();
    let u = crate::loc_ui::ui();
    let body = if game_count == 0 {
        u.move_no_games.to_string()
    } else {
        crate::loc::fill_count(lc.games_dir_confirm_1, lc.games_dir_confirm_n, game_count as i32)
    };
    backend.draw_confirm(
        Icon::FolderOpen,
        lc.games_dir_confirm,
        &body,
        Some(shown_path(dir_path)),
        &[
            Choice { label: lc.games_dir_confirm_no, destructive: false },
            Choice { label: lc.games_dir_confirm_yes, destructive: false },
        ],
        selection,
        false,
        crate::loc_ui::ui().cancel,
    );
}

/// "Could not send", over the page: the relay's reason, Fermer or Réessayer.
pub(crate) fn draw_send_failed(backend: &mut SwitchRenderBackend, selection: usize) {
    let u = crate::loc_ui::ui();
    let reason = FAILED_SEND
        .lock()
        .ok()
        .and_then(|g| g.as_ref().map(|(_, e)| e.clone()))
        .unwrap_or_default();
    backend.draw_confirm(
        Icon::Alert,
        u.send_failed,
        &reason,
        None,
        &[Choice { label: u.close, destructive: false }, Choice { label: u.retry, destructive: false }],
        selection,
        false,
        u.cancel,
    );
}
