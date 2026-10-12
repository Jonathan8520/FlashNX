//! The RÉGLAGES tab, and everything opened from it.
//!
//! Laid out like the console's own System Settings (Reglages.dc.html): the
//! sections down a column on the left, Quitter FlashNX pinned at its foot, and
//! beside it the open section's settings, changed where they are shown. Up and
//! Down in the column change the section and the pane follows at once; Right
//! or A go into the pane; B, or Left on a row with nothing to move sideways,
//! come back out. L and R still change tabs.
//!
//! Everything applies the moment it changes, with two exceptions that one more
//! press could not take back: the language waits for A (sweeping the cursor
//! across nine scripts would leave a player in one they cannot read), and
//! moving the library asks first (it renames real files).
//!
//! A deeper level opens inside the pane with the path above it: the browser of
//! the games folder, the game list of a bug report. Only the questions (move
//! the library? the report could not be sent) and the controls editor sit on
//! top, over the page dimmed behind them.

use super::*;
use crate::backend::render::{ui_col, Btn, Choice, HintItem, Icon, Row};

/// The column on the left, top to bottom. Quitter is not a section: it sits at
/// the foot of the column, has no pane, and acts on A.
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
    /// only has things to read, nor in Quitter, which has no pane: the cursor
    /// does not go in there.
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
            s.screen = Screen::Settings { section, in_pane: true, row: entry_row(section) };
        }
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
    match button {
        "B" | "Minus" => {
            s.screen = Screen::Settings { section, in_pane: false, row };
            return;
        }
        "Up" | "StickLUp" | "Down" | "StickLDown" => {
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
    let dir: i32 = match button {
        "Left" | "StickLLeft" => -1,
        "Right" | "StickLRight" => 1,
        "A" => crate::STEP_CYCLE as i32,
        _ => return,
    };
    if section == SetSection::Language {
        language_input(s, dir, row, fresh);
        return;
    }
    let took = row_input(s, section, key_of(section, row), dir);
    // Left on a row with nothing to move sideways goes back to the column, as
    // on the console. Only on a press: a held Left stops on the row.
    if !took && dir < 0 && fresh {
        s.screen = Screen::Settings { section, in_pane: false, row };
    }
}

/// Left (-1), Right (1) or A (`STEP_CYCLE`) on row `key` of `section`.
/// Returns whether the row took it: a row with nothing to move sideways does
/// not take Left or Right.
fn row_input(s: &mut State, section: SetSection, key: &str, dir: i32) -> bool {
    let press = dir >= crate::STEP_CYCLE as i32;
    match (section, key) {
        (SetSection::Library, "VUE") => {
            // The layout is applied as the cursor reaches it, so the card in
            // use and the card under the cursor are always the same one.
            let cur = crate::loc::home_view();
            let next = crate::step_choice(cur, crate::loc::HOME_VIEW_COUNT, dir as core::ffi::c_int);
            if next != cur {
                apply_home_view(next);
                clicked(dir);
            }
            true
        }
        (SetSection::Library, "DOSSIER") => {
            if press {
                // Opens beside the folder in use, the cursor on it: the row
                // opens on what it shows, among folders it could move to. Its
                // own listing holds none (every folder in it is part of the
                // library), and a new folder made beside it can take it. A
                // library at the card's root has nothing beside it, and opens
                // on itself.
                let root = primary_root();
                if browse_parent(&root).is_some() {
                    browse_go_up(s, &root);
                } else {
                    s.dir_browse = list_dirs(&root);
                    s.dir_path = root;
                    s.screen = Screen::GamesDirPicker { selection: 0, scroll: 0 };
                }
            }
            press
        }
        (SetSection::Library, "SONS") => {
            if let Some(on) = toggle_step(crate::loc::ui_sounds(), dir) {
                // Turning them on is heard at once: the click below, or the
                // press's own (`sfx::after_input`), plays after the switch.
                crate::loc::set_ui_sounds(on);
                crate::loc::save_current();
                clicked(dir);
            }
            true
        }
        (SetSection::Defaults, "TOUCHES") => {
            if press {
                // The global default keymap, in the editor every game uses.
                keymap::init_for_global_default();
                menu::open(None);
                s.screen = Screen::SettingsKeymapEditor;
            }
            press
        }
        (SetSection::Defaults, k) => {
            if default_step(k, dir) {
                clicked(dir);
            }
            true
        }
        (SetSection::Help, "BUG") => {
            if press {
                // A report is about one game: with none on the card the row is
                // greyed out, and A says why instead of opening an empty list.
                if s.entries.is_empty() {
                    set_toast(s, crate::loc::s().bug_no_games.to_string(), TOAST_INFO);
                } else {
                    s.screen = Screen::BugPicker { selection: 0, scroll_offset: 0 };
                }
            }
            press
        }
        // The nickname and the idea open the keyboard: hoisted in `input()`.
        _ => false,
    }
}

/// The languages: Left on the first column goes back to the section column,
/// Right stops at the end of a row, A applies. The one setting that waits for
/// A: applied on the move, a sweep of the cursor would pass through scripts a
/// player may not be able to read their way back from.
fn language_input(s: &mut State, dir: i32, row: usize, fresh: bool) {
    let n = crate::loc::PICKER_LANGS.len();
    let at = |row: usize| Screen::Settings { section: SetSection::Language, in_pane: true, row };
    if dir >= crate::STEP_CYCLE as i32 {
        if let Some(&lang) = crate::loc::PICKER_LANGS.get(row) {
            if lang != crate::loc::current() {
                crate::loc::set(lang);
                crate::loc::save(lang);
            }
        }
    } else if dir < 0 {
        if row % LANG_COLS > 0 {
            s.screen = at(row - 1);
        } else if fresh {
            s.screen = Screen::Settings { section: SetSection::Language, in_pane: false, row };
        }
    } else if row % LANG_COLS + 1 < LANG_COLS && row + 1 < n {
        s.screen = at(row + 1);
    }
}

/// The click of a value moved by Left or Right. A press of A is heard by
/// `sfx::after_input`, which hears every A; a sideways step it cannot tell
/// from one against the end of a choice, which deserves silence.
fn clicked(dir: i32) {
    if dir < crate::STEP_CYCLE as i32 {
        crate::sfx::play(crate::sfx::Sfx::Toggle);
    }
}

/// A switch moved by `dir`: Right turns it on, Left off, A flips it. `None`
/// when that changes nothing, which is also what keeps a held direction from
/// flipping it on and off with every repeat.
fn toggle_step(on: bool, dir: i32) -> Option<bool> {
    let want = if dir >= crate::STEP_CYCLE as i32 { !on } else { dir > 0 };
    (want != on).then_some(want)
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

/// One step on a game default. These are DEFAULTS, not overrides: a game with
/// a value of its own (set from its options or its pause) keeps it, and keeps
/// it when the default changes later. Returns whether a value changed.
fn default_step(key: &str, dir: i32) -> bool {
    use crate::loc as l;
    let d = dir as core::ffi::c_int;
    let set_if = |cur: u8, next: u8, set: fn(u8)| -> bool {
        if next == cur {
            false
        } else {
            set(next);
            true
        }
    };
    let changed = match key {
        "AFFICHAGE" => {
            let cur = l::default_display_mode();
            set_if(cur, crate::step_choice(cur, keymap::DISPLAY_MODE_COUNT, d), l::set_default_display_mode)
        }
        "ROTATION" => {
            let cur = l::default_rotation();
            set_if(cur, crate::step_choice(cur, keymap::ROTATION_COUNT, d), l::set_default_rotation)
        }
        "PIXELS" => {
            let cur = l::default_pixel_filter();
            set_if(cur, crate::step_choice(cur, keymap::PIXEL_FILTER_COUNT, d), l::set_default_pixel_filter)
        }
        "FILTRE" => {
            let cur = l::default_screen_filter();
            set_if(cur, crate::step_choice(cur, keymap::SCREEN_FILTER_COUNT, d), l::set_default_screen_filter)
        }
        // No clock is touched from here: a raise outside gameplay would be
        // spent entirely on drawing a menu.
        "OVERCLOCK" => {
            let cur = l::default_power_mode();
            let at = POWER_ORDER.iter().position(|m| *m == cur).unwrap_or(0) as u8;
            set_if(cur, POWER_ORDER[crate::step_choice(at, 3, d) as usize], l::set_default_power_mode)
        }
        "FPS" => match toggle_step(l::default_fps_counter() == 1, dir) {
            Some(on) => {
                l::set_default_fps_counter(on as u8);
                true
            }
            None => false,
        },
        "ZOOM" => step_default_zoom(dir),
        // C++ owns the live value and its file, and knows on its own that out
        // of a game this steps the global default.
        "VITESSE" => return step_default_cursor(dir),
        _ => false,
    };
    if changed {
        l::save_current();
    }
    changed
}

/// The default zoom's presets (`loc::DEFAULT_ZOOM_STEPS`): presets and not
/// the free percentage of the pause, because there is no picture here to
/// judge one against. Left and Right stop at the ends, so holding one reaches
/// the end and stays there; A goes round, as on every choice.
fn step_default_zoom(dir: i32) -> bool {
    if dir >= crate::STEP_CYCLE as i32 {
        crate::loc::step_default_zoom(1);
        return true;
    }
    let steps = crate::loc::DEFAULT_ZOOM_STEPS;
    let cur = crate::loc::default_zoom();
    let next = match steps.iter().position(|v| *v == cur) {
        Some(i) if dir < 0 => steps[i.saturating_sub(1)],
        Some(i) => steps[(i + 1).min(steps.len() - 1)],
        // A value that is not a preset (hand-edited into settings.json) comes
        // back to the first one, as it always has.
        None => steps[0],
    };
    if next == cur {
        return false;
    }
    crate::loc::set_default_zoom(next);
    true
}

/// The cursor speed of the games that have none of their own. C++ steps it
/// round past either end; Left and Right stop there instead, and only A goes
/// round.
fn step_default_cursor(dir: i32) -> bool {
    let speeds = crate::loc::CURSOR_SPEED_X10;
    let m = unsafe { ruffle_cursor_speed_mult_x10() }.max(0) as u32;
    let press = dir >= crate::STEP_CYCLE as i32;
    let at_end = (dir < 0 && m <= speeds[0]) || (dir > 0 && m >= speeds[speeds.len() - 1]);
    if !press && at_end {
        return false;
    }
    unsafe { ruffle_cursor_speed_step(if dir < 0 { -1 } else { 1 }) };
    true
}

/// A tap on the page, on cell `idx` of its touch table: the column's entries
/// first, then the pane's places (in Bibliothèque the four cards, then its
/// rows). Puts the cursor there, and answers true when it was already there,
/// which the caller turns into A.
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
            // A card is applied as it is tapped, the way Left and Right apply
            // it: there is nothing to confirm, and tapping the card in use
            // again must not move to the next one.
            let v = cell as u8;
            if crate::loc::home_view() != v {
                apply_home_view(v);
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
    }
    if let Ok(mut s) = LIBRARY.lock() {
        s.screen = settings_at(SetSection::Profile, "PSEUDO");
    }
}

/// Aide et retours > Proposer une idée: the keyboard for a free text, posted
/// as a "suggestion" issue (same relay and token as a bug report, another
/// label). No game involved. Hoisted from `input()`: the keyboard and the
/// synchronous POST must not run under the library lock. Empty input sends
/// nothing.
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

/// Send `report`, which started on the Aide et retours row `key`. Sent: a
/// toast says so and the cursor is back on the row, the tab never left. Not
/// sent: a question over the page gives the reason and offers to try again,
/// the report kept for it.
///
/// The row is carried rather than derived from `report.kind`: it is a fact
/// about the menu, and dismissing a sent suggestion once landed the cursor on
/// the bug row above the button just pressed (#113).
pub(crate) fn submit_report(report: crate::bugreport::Report, key: &'static str) {
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
        }
        Err(e) => {
            if let Ok(mut g) = FAILED_SEND.lock() {
                *g = Some((report, e));
            }
            if let Ok(mut s) = LIBRARY.lock() {
                // A retry that failed again stays on the same question, so the
                // note that marks an error arriving has to be played by hand.
                if matches!(s.screen, Screen::SendFailed { .. }) {
                    crate::sfx::play(crate::sfx::Sfx::Error);
                }
                // On Réessayer: the question is there to be answered with one
                // press, and nothing is lost either way.
                s.screen = Screen::SendFailed { row, selection: 1 };
            }
        }
    }
}

/// A on Réessayer: the kept report, sent again. Hoisted from `input()`, being
/// a network call.
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
        // Left is the first answer, Right the second: a held direction stays
        // where it points instead of flipping between the two.
        "Left" | "StickLLeft" => s.screen = Screen::SendFailed { row, selection: 0 },
        "Right" | "StickLRight" => s.screen = Screen::SendFailed { row, selection: 1 },
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

/// `name` inside the folder `path` (`sdmc:/` already ends in a separator).
pub(crate) fn child_dir(path: &str, name: &str) -> std::string::String {
    if path.ends_with('/') {
        std::format!("{}{}", path, name)
    } else {
        std::format!("{}/{}", path, name)
    }
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
    let root = root.trim_end_matches('/');
    path.len() > root.len() && path.starts_with(root) && path.as_bytes()[root.len()] == b'/'
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
    let mut font_cold = false;
    match body {
        Body::Section { in_pane, row } => {
            let focus = if in_pane { Some(row) } else { None };
            match section {
                SetSection::Library => library_pane(backend, focus, game_count, &mut cells, &mut hints),
                SetSection::Defaults => defaults_pane(backend, focus, &mut cells, &mut hints),
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
        Body::Folders { selection, scroll } => folders_page(backend, folder, selection, scroll, &mut hints),
        Body::Games { selection, scroll } => games_page(backend, selection, scroll, &mut hints),
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
    // window nothing here may be tapped.
    if !under && matches!(body, Body::Section { .. }) {
        backend.settings_publish(cells);
    }
    backend.settings_still(false);
    if font_cold {
        backend.draw_loading_overlay(crate::loc::s().lang_title, unsafe { ruffle_tick_now() });
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
fn menu_path(steps: &[&str]) -> std::string::String {
    steps.join("  \u{203A}  ")
}

/// Bibliothèque: the home layouts as cards, the games folder, the interface
/// sounds, each with what it is for underneath.
fn library_pane(
    b: &mut SwitchRenderBackend,
    focus: Option<usize>,
    game_count: usize,
    cells: &mut std::vec::Vec<(f32, f32, f32, f32)>,
    hints: &mut std::vec::Vec<HintItem<'static>>,
) {
    let u = crate::loc_ui::ui();
    let lib = SetSection::Library;
    let glide = pane_glide(lib);
    let on_row = |key: &str| focus.filter(|r| *r == row_of(lib, key)).map(|_| 0);
    let top = b.settings_title(u.sec_library, None);
    let view = crate::loc::home_view();
    let desc = match view {
        1 => u.view_desc_list,
        2 => u.view_desc_strip,
        3 => u.view_desc_shelf,
        _ => u.view_desc_grid,
    };
    let (c, y) = b.settings_layouts(top, u.row_home_view, view, on_row("VUE").is_some(), desc);
    cells.extend(c);
    let root = primary_root();
    let folder = [Row::new(Icon::Folder, u.row_games_dir).value(shown_path(&root)).opens()];
    let (c, y) = b.settings_block(y + 8.0, &folder, on_row("DOSSIER"), glide);
    cells.extend(c);
    let line = std::format!("{}  \u{00B7}  {}", u.help_games_dir, crate::loc_ui::games(game_count));
    let y = b.settings_note(y + 8.0, &line, 3);
    let on = crate::loc::ui_sounds();
    let sounds = [Row::new(if on { Icon::Volume } else { Icon::VolumeOff }, u.row_sounds).toggle(on)];
    let (c, y) = b.settings_block(y + 18.0, &sounds, on_row("SONS"), glide);
    cells.extend(c);
    b.settings_note(y + 8.0, u.help_sounds, 2);
    *hints = match focus.map(|r| key_of(lib, r)) {
        Some("VUE") => std::vec![(&[Btn::Dpad][..], u.change_view), (&[Btn::B][..], u.back)],
        Some("DOSSIER") => std::vec![(&[Btn::A][..], u.open), (&[Btn::B][..], u.back)],
        Some(_) => std::vec![(&[Btn::Dpad][..], u.change), (&[Btn::B][..], u.back)],
        None => std::vec::Vec::new(),
    };
}

/// Jeu par défaut: what every game starts with until it is set on its own, in
/// the groups and with the controls of a game's options, so a setting looks the
/// same wherever it is changed. The box under the rows explains the row with
/// the cursor, Overclock setting by setting.
fn defaults_pane(
    b: &mut SwitchRenderBackend,
    focus: Option<usize>,
    cells: &mut std::vec::Vec<(f32, f32, f32, f32)>,
    hints: &mut std::vec::Vec<HintItem<'static>>,
) {
    use crate::loc as l;
    let lc = l::s();
    let u = crate::loc_ui::ui();
    let def = SetSection::Defaults;
    let top = b.settings_title(u.row_game_prefs, Some(u.prefs_sub));
    let display = [l::display_mode_label(0), l::display_mode_label(1), l::display_mode_label(2)];
    let rotation = [l::rotation_label(0), l::rotation_label(1), l::rotation_label(2), l::rotation_label(3)];
    let pixels = [l::pixel_filter_label(0), l::pixel_filter_label(1), l::pixel_filter_label(2)];
    let filters = [l::screen_filter_label(0), l::screen_filter_label(1), l::screen_filter_label(2)];
    let power = l::default_power_mode();
    let power_opts = POWER_ORDER.map(l::overclock_label);
    let power_at = POWER_ORDER.iter().position(|m| *m == power).unwrap_or(0);
    let zoom = std::format!("{} %", l::default_zoom());
    let speed = l::mult_label(unsafe { ruffle_cursor_speed_mult_x10() }.max(5) as u32);
    // In the order of `DEFAULTS_ROWS`, which the cursor and the help read.
    let rows = [
        Row::new(Icon::Gamepad, lc.keys_title).opens(),
        Row::new(Icon::Pointer, lc.set_cursor_speed).cycle(&speed),
        Row::new(Icon::Monitor, lc.set_display_mode).choice(&display, l::default_display_mode() as usize),
        Row::new(Icon::Rotate, lc.set_rotation).choice(&rotation, l::default_rotation() as usize),
        Row::new(Icon::ZoomIn, lc.set_zoom).cycle(&zoom),
        Row::new(Icon::Pixels, lc.set_pixel_filter).choice(&pixels, l::default_pixel_filter() as usize),
        Row::new(Icon::Tv, lc.set_screen_filter).choice(&filters, l::default_screen_filter() as usize),
        Row::new(Icon::Gauge, lc.set_fps).toggle(l::default_fps_counter() == 1),
        Row::new(Icon::Cpu, lc.set_overclock).choice(&power_opts, power_at),
    ];
    let sections = [
        (row_of(def, "TOUCHES"), u.sec_controls),
        (row_of(def, "AFFICHAGE"), u.sec_image),
        (row_of(def, "OVERCLOCK"), u.sec_perf),
    ];
    let key = focus.map(|r| key_of(def, r));
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
        Some("TOUCHES") => u.help_keys_default,
        Some(_) => u.help_cursor_default,
    };
    cells.extend(b.settings_rows(top + 4.0, &rows, &sections, focus.unwrap_or(usize::MAX), pane_glide(def), help));
    *hints = match key {
        Some("TOUCHES") => std::vec![(&[Btn::A][..], u.open), (&[Btn::B][..], u.back)],
        Some(_) => std::vec![(&[Btn::Dpad][..], u.change), (&[Btn::B][..], u.back)],
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
    let langs: std::vec::Vec<(&str, &str)> = crate::loc::PICKER_LANGS
        .iter()
        .zip(codes.iter())
        .map(|(l, c)| (c.as_str(), l.picker_name()))
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
    let (c, y) = b.settings_block(top, &row, focus.map(|_| 0), pane_glide(SetSection::Profile));
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
    let (c, y) = b.settings_block(top, &bug, focus.filter(|r| *r == bug_row).map(|_| 0), glide);
    cells.extend(c);
    let y = b.settings_note(y + 8.0, if empty { lc.bug_no_games } else { u.help_bug }, 3);
    let idea = [Row::new(Icon::Lightbulb, u.row_suggest).keyboard()];
    let (c, y) = b.settings_block(y + 18.0, &idea, focus.filter(|r| *r == idea_row).map(|_| 0), glide);
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
    let (_, y) = b.settings_block(y + 14.0, &rows, None, pane_glide(SetSection::About));
    // Said here before a launch fails, not only after.
    if applet {
        b.settings_note(y + 8.0, lc.applet_notice, 3);
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
) {
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
    hints.push((&[Btn::B][..], if up { u.go_up } else { u.back }));
}

/// Aide et retours > Signaler un bug: which game? Last played first, each with
/// its cover and the time played, which is usually enough to find the one that
/// just broke.
fn games_page(
    b: &mut SwitchRenderBackend,
    selection: usize,
    scroll: usize,
    hints: &mut std::vec::Vec<HintItem<'static>>,
) {
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
    );
}
