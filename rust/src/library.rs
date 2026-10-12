//! Library UI — the "FlashNX launcher" shown at boot before Ruffle starts.
//!
//! Phase 3.4 — replaces the silent `swf_picker_run` first-hit logic with a
//! full game list. C++ enumerates every `.swf` in `sdmc:/ruffle/` and
//! `sdmc:/switch/ruffle/` and forwards each path here via
//! `ruffle_library_add_path`. We parse each file's SWF header lazily on the
//! way in (version + size + dims) so the metadata panel doesn't need to
//! re-open files on every cursor move.
//!
//! Screens (state machine, like `menu::Screen`):
//!   - **Empty**:        no SWF found, instructions where to drop files.
//!   - **List**:         scrollable game list, A=JOUER, X=OPTIONS, -=QUITTER.
//!   - **OptionsModal**: per-game options (TOUCHES + RETOUR for v1).
//!   - **Picked**:       user picked a game; `selected_path` is set; C++
//!                       polls `is_active()` and exits the library loop.
//!   - **Quit**:         user pressed - on the empty/list; C++ exits the
//!                       worker thread (and the `.nro`).
//!
//! Rendering lives in `backend::render::SwitchRenderBackend::draw_library_*`
//! (mirroring the TOUCHES list pattern) so this module stays focused on
//! state + input.
//!
//! When the user selects TOUCHES from the options modal, we delegate to the
//! existing `menu` module's editor — same `menu::open` / `menu::input` /
//! `menu::draw` we use mid-game. To make that work pre-launch, we re-init
//! the keymap module for the chosen game's basename via
//! `keymap::init_for_swf` here too (the keymap module is `OnceLock` so the
//! first init wins — fine because the library always picks BEFORE Ruffle).

use std::fs::File;
use std::io::Read;
use std::sync::Mutex;

use crate::backend::render::SwitchRenderBackend;
use crate::net::{self, RemoteFile};
use crate::{keymap, menu};

/// Currently displayed library screen. `Inactive` is set before
/// `ruffle_library_init` runs and after the user has picked a game.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Screen {
    Inactive,
    /// No SWF on SD. Shows a help message ("drop .swf in sdmc:/ruffle/").
    Empty,
    /// Main local game list. `selection` indexes `State::entries`,
    /// `scroll_offset` is the topmost visible row.
    List { selection: usize, scroll_offset: usize },
    /// OPTIONS modal for the game at `game_idx`. `selection` indexes
    /// `OPTIONS_ENTRIES`.
    OptionsModal { game_idx: usize, selection: usize },
    /// User pressed A on a game; main loop reads `selected_path` and exits.
    Picked,
    /// Transient launch animation (v1.2.0): the cover reveal is playing (tile ->
    /// full screen) before we flip to `Picked`. `is_active` stays true so the C++
    /// library loop keeps rendering it; `render` flips to `Picked` when the
    /// reveal finishes. Carries the gallery row so the frozen gallery draws behind.
    Launching { selection: usize, scroll_offset: usize },
    /// Applet mode: the user pressed A on a game but we have only the small
    /// applet heap, so launching would OOM. A window over the page says how to
    /// get the full memory (P1c) instead of the embedded red SWF. Carries the
    /// list row to return to on dismiss (pressed on an importer page, the page
    /// to go back to is held in `LAUNCH_BACKDROP` instead).
    AppletNotice { selection: usize, scroll_offset: usize },
    /// User pressed - or chose to quit; main loop exits the `.nro`.
    Quit,
    /// User pressed A on OPTIONS > TOUCHES — control delegated to
    /// `menu::*`. When `menu::is_active()` returns false we return to the
    /// OptionsModal screen.
    TouchesEditor { game_idx: usize },
    /// Confirm screen for OPTIONS > SUPPRIMER. A = delete .swf + all
    /// sidecars / saves matching the basename, then back to List. B =
    /// back to OptionsModal. Destructive, hence the explicit step.
    /// `selection`: 0 = cancel (left, where it opens), 1 = delete.
    DeleteConfirm { game_idx: usize, selection: usize },
    /// Cover picker (OPTIONS > JAQUETTE, v1.2.0). Lists Flashpoint search
    /// candidates from `State::cover_candidates`; `selection` indexes that vec.
    /// `State::cover_msg` (non-empty) shows a notice instead of a list (covers
    /// off / no results / fetch error). A = fetch+cache the chosen logo.
    CoverPicker { game_idx: usize, selection: usize },
    /// Flashpoint game gallery (IMPORTER > X): a cover grid of search hits;
    /// A downloads the selected game's GameZIP, or plays it when a game of
    /// that name is already on the card. Reuses `cover_candidates` for
    /// storage; drawn by `draw_library_fp_gallery`.
    FpGallery { selection: usize, scroll: usize },
    /// Details popup for the Flashpoint game at `selection` (`+` on the gallery):
    /// full title + developer/publisher/date + download size. `size` is the
    /// GameZIP's Content-Length (0 = unknown / probe failed). `scroll` restores
    /// the gallery on close.
    FpDetails { selection: usize, scroll: usize, size: u64 },
    /// JOUER sort picker (Y): centered modal — A-Z / recent / most-played / size.
    /// `prev_sel`/`prev_scroll` restore the gallery cursor when B (cancel) is hit.
    /// Bit `i` of `dirs` set = row `i` shows (and A would apply) its reversed
    /// order: each row keeps the direction it was turned to.
    SortModal { selection: usize, prev_sel: usize, prev_scroll: usize, dirs: u8 },
    /// Sort picker for the IMPORTER lists (Y). `kind` picks the target list +
    /// option set (`SORT_TARGET_*`, see `remote_sort_options`): an archive's
    /// files, the Flashpoint results (name, developer, release date), or the
    /// IMPORTER home's saved sources. `reverse` mirrors the JOUER modal's X
    /// toggle. `prev_*` restore the cursor on cancel.
    RemoteSortModal { selection: usize, kind: u8, reverse: bool, prev_sel: usize, prev_scroll: usize },
    // ── Phase 3.7: DISTANT mode (archive.org import) ───────────────────
    /// IMPORTER home: the two action cards, then the saved sources. Rows 0 and 1
    /// are the cards (search Flashpoint, add an address), rows 2.. index
    /// `importer_view` (the filtered + sorted history). `scroll_offset` is the
    /// topmost visible SOURCE (the cards never scroll) — a real field, not
    /// derived from `selection`, so a touch drag can scroll the list without
    /// moving the cursor. A opens the selected source (or acts on a card); +
    /// opens a source's options.
    DistantIdle { selection: usize, scroll_offset: usize },
    /// Async archive.org metadata fetch in flight (v1.2.0). The reveal window is
    /// open from the launched URL's row with a spinner; `net::tick_archive_fetch`
    /// is polled each frame in `render`. On success → DistantFiles, on error →
    /// DistantError. `State::pending_fetch_url` holds the URL to remember on success.
    DistantLoading,
    /// After a successful archive.org metadata fetch. Lists `RemoteFile`s
    /// stored in `State::remote_files`; `selection` indexes that vec.
    DistantFiles { selection: usize, scroll_offset: usize },
    /// Download in flight. The filename and target path live in
    /// `State::download_file_name` / `download_out_path`. Progress polled
    /// every frame via `net::download_progress`.
    DistantDownloading,
    /// Error from URL parse / metadata fetch / download. Message in
    /// `State::distant_error`. A tries again when that can work
    /// (`import_retry`), B goes back to the list the failure came from.
    DistantError,
    /// Confirm removing a history URL (from DistantUrlOptions > delete).
    /// A = delete + persist, B/Minus = cancel.
    /// `selection`: 0 = cancel (left, where it opens), 1 = delete.
    DistantHistoryConfirm { selection: usize },
    /// A saved source's options (Plus on its row): a side panel at the right
    /// edge over the importer, rows favourite / edit the address / delete.
    /// `url_idx` indexes `url_history`; `selection` indexes the rows.
    DistantUrlOptions { url_idx: usize, selection: usize },
    /// Text typed into "add an address" that cannot be one (it has spaces): the
    /// question offering to search Flashpoint for it instead, the text in
    /// `State::import_typed`. `selection`: 0 = retype it, 1 = search.
    ImportNotAddress { selection: usize },
    /// B during a long download: stop it and lose what came, or carry on. The
    /// download keeps running under the question. `selection`: 0 = carry on
    /// (where it opens), 1 = stop.
    DistantCancelConfirm { selection: usize },
    // ── RÉGLAGES (`settings_tab`) ───────────────────────────────────────
    /// The tab: its sections down the left, the open one's settings in the
    /// pane. `in_pane` says which side has the cursor; `row` is the cursor's
    /// place in the pane (a row, a language, the layout cards).
    Settings { section: SetSection, in_pane: bool, row: usize },
    /// RÉGLAGES > Bibliothèque > Dossier des jeux (#79), inside the pane:
    /// browse the card one level at a time and choose where the library goes.
    /// The folder shown and its subfolders are `dir_path` / `dir_browse`;
    /// `selection` 0 = "use this folder".
    GamesDirPicker { selection: usize, scroll: usize },
    /// OPTIONS > DOSSIER (issue #68): put this one game on a shelf. Rows are
    /// the root, then every folder that already holds a game, then "new folder".
    /// The list is rebuilt from the entries each frame, so nothing is stored.
    GameFolderPicker { game_idx: usize, selection: usize },
    /// Confirm emptying a shelf (X on the picker, issue #68): every game in it
    /// goes back to ACCUEIL and the folder is removed. Nothing is destroyed, but
    /// one press can undo a dozen deliberate moves, so it is asked.
    /// `folder_idx` indexes `folder_counts`, re-derived when it is answered.
    GameFolderDelete { game_idx: usize, folder_idx: usize, selection: usize },
    /// Confirm moving the library into the folder held in `dir_path`. Asked
    /// because a move is not undone by pressing B: it renames real files.
    /// `selection`: 0 = cancel (left, where it opens), 1 = move.
    GamesDirConfirm { selection: usize },
    /// The move itself, with a real count behind it. A library of a hundred
    /// games is a hundred renames plus their sidecars, and a frozen screen with
    /// no explanation is how a user learns to fear a feature.
    GamesDirMoving,
    /// Editing the GLOBAL DEFAULT keymap (RÉGLAGES > Jeu par défaut > Touches)
    /// in the reused `menu::*` editor, over the tab.
    SettingsKeymapEditor,
    /// RÉGLAGES > Aide et retours > Signaler un bug, inside the pane: which
    /// game is broken. `selection` is a row of `bug_picker_order`,
    /// `scroll_offset` the topmost visible row. A describes + sends, B goes
    /// back to the row that opened it.
    BugPicker { selection: usize, scroll_offset: usize },
    /// A bug report or an idea the relay did not take, asked over the page:
    /// Fermer (0) or Réessayer (1), which sends the kept report again. `row` is
    /// the Aide et retours row the flow started on, carried rather than
    /// assumed: one screen serves both flows, and dismissing a suggestion once
    /// landed the cursor on the row above the button just pressed (#113).
    SendFailed { row: usize, selection: usize },
    /// TOUCHES sub-menu (#20 regroup): everything controls-related for a game in
    /// one place. `selection` indexes [edit, apply, share, (revert)] — the revert
    /// row is present only when `State::touches_can_revert`. Reached from OPTIONS
    /// > TOUCHES; B returns to OPTIONS.
    TouchesMenu { game_idx: usize, selection: usize },
    /// Picker of community control profiles matching a game (#20, TOUCHES >
    /// APPLIQUER). `selection` indexes `State::profile_matches`. A opens the
    /// before/after preview; B returns to the TOUCHES sub-menu.
    ProfileList { game_idx: usize, selection: usize },
    /// Before/after preview of a profile (#20): the keys it changes, mine ->
    /// profile. `profile_idx` indexes `State::profile_matches`; the diff lines are
    /// snapshotted into `State::preview_rows`. A applies (hoisted), B → ProfileList.
    ProfilePreview { game_idx: usize, profile_idx: usize },
    /// Before/after preview of a REVERT (#20): current -> what revert restores
    /// (backup or default). Diff lines in `State::preview_rows`. A reverts
    /// (hoisted), B → TouchesMenu.
    RevertPreview { game_idx: usize },
    /// Confirm before sharing a game's controls (#20, TOUCHES > PARTAGER). Shows
    /// the before/after of the player's shared profile. A confirms (POSTs,
    /// hoisted) → toast + sub-menu; B cancels back to the sub-menu.
    ProfileShareConfirm { game_idx: usize },
    /// Confirm deleting one of YOUR OWN shared profiles from the catalog (#20,
    /// X on a self-shared row in the picker). A confirms (DELETEs via the relay,
    /// hoisted) → toast + picker; B cancels back to the picker.
    /// `selection`: 0 = cancel (where it opens), 1 = delete.
    ProfileDeleteConfirm { game_idx: usize, profile_idx: usize, selection: usize },
    /// Transient "loading" panel shown for ONE frame before a deferred profile
    /// network flow runs (#20), so a blocking GitHub call doesn't freeze on stale
    /// content. `render` runs the stashed `PendingNet` the next frame, which sets
    /// the real result screen. Carries no state — the action is in `PENDING_NET`.
    ProfileLoading,
}

// No "RETOUR" entry: B already backs out of the modal, so a dedicated row is
// redundant clutter. APPLIQUER / PARTAGER moved OUT of here into the TOUCHES
// sub-menu (#20 regroup) so they don't read as game-save actions next to
// RENOMMER / JAQUETTE / SUPPRIMER.
mod options_panel;
pub(crate) use options_panel::{opt_row, OPTIONS_ENTRIES};
mod settings_tab;
pub(crate) use settings_tab::SetSection;
use settings_tab::settings_at;

/// Top-level navbar tabs (v1.2.0), switched with the L/R shoulder buttons.
/// Each maps to a "home" screen. The navbar is drawn on every tab-home screen;
/// sub-screens (OPTIONS modal, DISTANT file list, download, in-game) are NOT
/// tab-homes, so L/R keeps any local meaning there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Tab {
    Jouer,
    Importer,
    Reglages,
}

impl Tab {
    pub(crate) const ORDER: [Tab; 3] = [Tab::Jouer, Tab::Importer, Tab::Reglages];
    pub(crate) fn index(self) -> usize {
        match self {
            Tab::Jouer => 0,
            Tab::Importer => 1,
            Tab::Reglages => 2,
        }
    }
    /// The tab a navbar cell stands for. `None` for anything out of range --
    /// the strip has three cells and the hit test can only answer within it,
    /// but a bound that is checked is a bound that cannot become a panic.
    pub(crate) fn from_index(i: usize) -> Option<Tab> {
        Tab::ORDER.get(i).copied()
    }
    fn next(self) -> Tab {
        Tab::ORDER[(self.index() + 1) % Tab::ORDER.len()]
    }
    fn prev(self) -> Tab {
        Tab::ORDER[(self.index() + Tab::ORDER.len() - 1) % Tab::ORDER.len()]
    }
}

/// The tab a screen belongs to — `Some` only for tab-HOME screens (where the
/// navbar shows and L/R switches tabs). Sub-screens return `None` so L/R keeps
/// its local meaning (or is ignored) there.
pub(crate) fn screen_tab(screen: Screen) -> Option<Tab> {
    match screen {
        Screen::List { .. } | Screen::Empty => Some(Tab::Jouer),
        Screen::DistantIdle { .. } => Some(Tab::Importer),
        Screen::Settings { .. } => Some(Tab::Reglages),
        _ => None,
    }
}

/// Switch to `target`, sliding its content in from `dir` (-1 = from the left).
///
/// One body for the shoulders and for a tap on the strip: two copies would
/// drift, and the slide direction is the only thing that differs between them.
pub(crate) fn goto_tab(target: Tab, dir: f32) {
    // Belt and braces with the gate in `input()` and in the navbar branch of
    // `touch()`: a reveal left armed with nobody to step it pins
    // `game_reveal_active()` true and every button after it is swallowed. The
    // gates stop us getting here mid-reveal; this makes sure that a path added
    // later cannot resurrect the trap.
    crate::backend::render::game_reveal_cancel();
    if let Ok(mut s) = LIBRARY.lock() {
        // Leaving the Play tab: remember the game the cursor is on.
        if let Screen::List { selection, .. } = s.screen {
            shelf_cursor_save(&s, selection);
        }
        s.screen = match target {
            Tab::Jouer => {
                if s.entries.is_empty() {
                    Screen::Empty
                } else {
                    // Back on that game, not on the first one: a look at
                    // REGLAGES or IMPORTER is not a reason to lose your place.
                    let row = shelf_cursor_row(&s);
                    Screen::List { selection: row, scroll_offset: home_scroll_for(row, jouer_scroll()) }
                }
            }
            Tab::Importer => distant_idle_at(&s, IMPORT_ROW.load(std::sync::atomic::Ordering::Relaxed)),
            // Back on the section the player left, not on the first one.
            Tab::Reglages => settings_tab::settings_home(),
        };
    }
    // Slide the incoming tab's content in from the side asked for; the navbar
    // stays fixed, and so does nothing else. (Tabs slide; modals and editors
    // scale.)
    crate::backend::render::home_header_pin(false);
    crate::backend::render::tab_transition_begin(dir);
    // Snap the JOUER glide and its strip, so the gallery lands on its cursor
    // instead of gliding from wherever the last frame left them.
    crate::backend::render::gallery_anim_reset();
    crate::backend::render::home_strip_snap();
}

/// The game most recently launched, as `(path, basename, display_name)`. Set on
/// pick (JOUER), and deliberately NOT cleared by `reset()` so that:
///   1. quitting a game back to the library restores the cursor to that
///      row (instead of jumping to the top), and
///   2. the in-game pause menu can show the game's name under "PAUSE".
///
/// The PATH is the identity. Folders (#68) let two games share a file name, and
/// finding the row by name alone landed the cursor -- and the closing fade -- on
/// whichever namesake the sort happened to put first. The basename is kept
/// beside it because the playtime and the pause title are keyed by name.
static LAST_PLAYED: Mutex<Option<(std::string::String, std::string::String, std::string::String)>> =
    Mutex::new(None);

/// System tick captured when a game is launched; `open()` (return to library)
/// subtracts it to bank the session's playtime. NOT cleared by `reset()` so it
/// survives the game->library teardown.
static LAUNCH_TICK: Mutex<Option<u64>> = Mutex::new(None);

/// Whether the launching game has a companion folder (`Entry::multi`), for
/// the launch reveal's "several files" badge. Set at launch (A-press), cleared
/// on return to the library so the quit reveal stays clean.
///
/// It was a count of the flat `.swf` files in the folder, read off the card at
/// the press. A Flashpoint GameZIP keeps its files in host directories, so that
/// count was 0 and the badge never showed for most multi-file games; the tile
/// and the launch now answer from the same flag.
static LAUNCH_MULTI: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Active library sort, persisted to `sdmc:/flashnx/sort.txt`
/// (0 = A-Z, 1 = recent, 2 = recently played, 3 = most played, 4 = size).
/// Default A-Z.
static SORT_MODE: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);
/// Whether the active sort is reversed (the modal's X toggle). Persisted in the
/// same `sort.txt` as a trailing `R` after the mode digit. Default off.
static SORT_REVERSE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
/// One-shot guard: load playtime + sort prefs once per boot (not every open()).
static PREFS_LOADED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Names of the app's own files. Their FOLDER is wherever the library lives —
/// see `config_read_path` / `config_write_path`.
const SORT_FILE: &str = "sort.txt";

/// Per-folder sorts (issue #68): `name=digit[R]` lines, one file for all of
/// them rather than one file per shelf — the games folder is walked by the boot
/// scan, and that is exactly why `.display`/`.rot`/`.filter` were folded into a
/// single `.prefs` per game.
///
/// TOUS has no line here: its sort IS the home's, in `sort.txt`. Cards written
/// before the ACCUEIL stop was dropped may still carry a `/` line; it is read
/// without complaint and never written again.
const FOLDER_SORT_FILE: &str = "sort_folders.txt";
static FOLDER_SORTS: Mutex<std::vec::Vec<(std::string::String, u8, bool)>> =
    Mutex::new(std::vec::Vec::new());

/// The shelf the home is showing, mirrored out of `State`.
///
/// The sort is asked for in nine places that have no `State` in hand, and the
/// answer now depends on which shelf is open. Mirroring it costs one string;
/// threading it would have changed nine signatures to ask the same question.
static ACTIVE_FOLDER: Mutex<Option<std::string::String>> = Mutex::new(None);

/// Set the open shelf. The strip learns it from `draw_gallery`, under the same
/// lock, so the chips and the sort can never disagree about which shelf the
/// player is on.
pub(crate) fn set_active_folder(folder: Option<&str>) {
    if let Ok(mut g) = ACTIVE_FOLDER.lock() {
        // Only on a real change: the gallery calls this every frame.
        if g.as_deref() != folder {
            *g = folder.map(|s| s.to_string());
        }
    }
}

/// The shelf whose sort applies, or `None` for TOUS, which uses the home's.
///
/// Every named shelf keeps its own rule: it holds a different set of games from
/// TOUS, and a set you look at on its own deserves an order you chose on its
/// own. Only TOUS -- the whole library, the thing the home sort was always
/// about -- falls through to the global.
fn sorting_folder() -> Option<std::string::String> {
    let f = ACTIVE_FOLDER.lock().ok().and_then(|g| g.clone())?;
    Some(if f.is_empty() {
        ROOT_SORT_KEY.to_string()
    } else {
        f
    })
}

/// Legacy key for the sort of the ACCUEIL stop, which no longer exists.
///
/// Kept because cards in the wild already carry a `/=<digit>` line and reading
/// one must not become a parse error. Nothing writes it any more.
///
/// A lone slash, because that is the one thing a folder name can never be: FAT
/// forbids it in a directory name, so this can never collide with a real shelf.
/// The empty string would have worked too and produced a line reading `=2R`,
/// which is unreadable to anyone opening the file from a PC — and that file
/// exists to be readable, like every other preference on the card.
const ROOT_SORT_KEY: &str = "/";

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum SortMode {
    Alpha,
    Recent,
    RecentlyPlayed,
    MostPlayed,
    Size,
}

pub(crate) fn current_sort_mode() -> SortMode {
    match sort_mode_index() as u8 {
        1 => SortMode::Recent,
        2 => SortMode::RecentlyPlayed,
        3 => SortMode::MostPlayed,
        4 => SortMode::Size,
        _ => SortMode::Alpha,
    }
}

/// 0..=4 index of the active sort (for the sort modal cursor).
///
/// The OPEN SHELF's sort when it has one of its own, the home's otherwise
/// (issue #68). Alphabetical suits a shelf of a series; "most played" suits the
/// whole library; there is no reason one choice should govern both.
pub(crate) fn sort_mode_index() -> usize {
    if let Some(f) = sorting_folder() {
        if let Ok(g) = FOLDER_SORTS.lock() {
            if let Some((_, m, _)) = g.iter().find(|(n, _, _)| *n == f) {
                return *m as usize;
            }
        }
    }
    SORT_MODE.load(std::sync::atomic::Ordering::Relaxed) as usize
}

/// Whether the active sort is currently reversed.
pub(crate) fn current_sort_reverse() -> bool {
    if let Some(f) = sorting_folder() {
        if let Ok(g) = FOLDER_SORTS.lock() {
            if let Some((_, _, r)) = g.iter().find(|(n, _, _)| *n == f) {
                return *r;
            }
        }
    }
    SORT_REVERSE.load(std::sync::atomic::Ordering::Relaxed)
}

/// Store the open shelf's sort, or the home's when none is open, then persist.
/// The mode and its direction always together, so they can never land in
/// different places.
fn store_active_sort(mode: u8, rev: bool) {
    match sorting_folder() {
        Some(f) => {
            if let Ok(mut g) = FOLDER_SORTS.lock() {
                match g.iter_mut().find(|(n, _, _)| *n == f) {
                    Some(e) => {
                        e.1 = mode;
                        e.2 = rev;
                    }
                    None => g.push((f, mode, rev)),
                }
            }
            persist_folder_sorts();
        }
        None => {
            SORT_MODE.store(mode, std::sync::atomic::Ordering::Relaxed);
            SORT_REVERSE.store(rev, std::sync::atomic::Ordering::Relaxed);
            persist_sort();
        }
    }
}

/// Set only when `load_folder_sorts` actually READ the file (or established that
/// there is none). A failed read leaves the table empty, which is indistinguishable
/// from "no shelf has a sort of its own" — and the first sort change after that
/// would rewrite the file from that empty table, wiping every other shelf's
/// choice for good. This says which of the two it was.
static FOLDER_SORTS_LOADED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Drop the stored sort of every shelf that no longer exists.
///
/// A shelf is not a thing you create and destroy: it exists exactly as long as
/// a game is on it. So its sort line has to be swept by the same rule, or
/// `sort_folders.txt` grows one orphan per reorganisation and -- worse --
/// recreating a shelf of the same name later makes it inherit an order chosen
/// in a previous life. Emptying a shelf already did this; taking the last label
/// off one, and deleting its last game, did not.
///
/// Never runs on an EMPTY library. A scan that failed, or a games folder just
/// pointed somewhere else, both present as "no shelf exists any more" and would
/// take every sort with them -- the same trap that made `tags::prune` a bad
/// idea at boot.
fn prune_folder_sorts(entries: &[Entry]) {
    if entries.is_empty() {
        return;
    }
    let live = folder_counts(entries);
    let changed = match FOLDER_SORTS.lock() {
        Ok(mut g) => {
            let before = g.len();
            g.retain(|(n, _, _)| n == FAVORITES_SHELF || live.iter().any(|(l, _)| l == n));
            before != g.len()
        }
        Err(_) => false,
    };
    if changed {
        persist_folder_sorts();
    }
}

/// `name=digit[R]` lines, one per shelf that has been given a sort of its own.
fn persist_folder_sorts() {
    // Never write a whole file out of a table we failed to fill. One shelf's new
    // sort is not worth every other shelf's.
    if !FOLDER_SORTS_LOADED.load(std::sync::atomic::Ordering::Relaxed) {
        log("library: sort_folders.txt was never read; refusing to overwrite it\n");
        return;
    }
    let Ok(g) = FOLDER_SORTS.lock() else {
        return;
    };
    let body: std::string::String = g
        .iter()
        .map(|(n, m, r)| std::format!("{}={}{}\n", n, m, if *r { "R" } else { "" }))
        .collect();
    let path = config_write_path(FOLDER_SORT_FILE);
    if std::fs::write(&path, body.as_bytes()).is_ok() {
        crate::sd::commit();
    }
}

fn load_folder_sorts() {
    let path = config_read_path(FOLDER_SORT_FILE);
    // Bounded read, NOT `read_to_string`: that one pre-reserves a buffer sized
    // from fstat, and in APPLET mode the size comes back bogus, so the reserve
    // fails with OutOfMemory and a 300-byte file reads as an error. Same trap the
    // URL history fell into, and the same fix as `read_meta_sidecar`.
    let txt = match read_sd_text(&path, 64 * 1024) {
        Ok(t) => t,
        Err(e) => {
            // A file that is not there yet is not a failure: there is simply
            // nothing to keep, and writing one later is safe.
            if e.kind() == std::io::ErrorKind::NotFound {
                FOLDER_SORTS_LOADED.store(true, std::sync::atomic::Ordering::Relaxed);
            } else {
                log(&std::format!("library: sort_folders.txt unreadable: {}\n", e));
            }
            return;
        }
    };
    let Ok(mut g) = FOLDER_SORTS.lock() else {
        return;
    };
    FOLDER_SORTS_LOADED.store(true, std::sync::atomic::Ordering::Relaxed);
    g.clear();
    for line in txt.lines() {
        // Split on the LAST '=': a folder is free to have one in its name.
        let Some(eq) = line.rfind('=') else {
            continue;
        };
        let (name, val) = (&line[..eq], line[eq + 1..].trim());
        if name.is_empty() || val.is_empty() {
            continue;
        }
        let mode = match val.as_bytes()[0] {
            b'1' => 1,
            b'2' => 2,
            b'3' => 3,
            b'4' => 4,
            _ => 0,
        };
        g.push((name.to_string(), mode, val.contains(['R', 'r'])));
    }
}

/// Persist the mode + reverse flag together to `sort.txt` (digit, then `R` if
/// reversed). Best-effort.
fn persist_sort() {
    let mode = SORT_MODE.load(std::sync::atomic::Ordering::Relaxed);
    let rev = SORT_REVERSE.load(std::sync::atomic::Ordering::Relaxed);
    write_sort_pref(&config_write_path(SORT_FILE), mode, rev);
}

/// Shared `digit[R]` writer for both sort prefs (JOUER's `sort.txt` and the
/// IMPORTER's `import_sort.txt`). Best-effort.
fn write_sort_pref(path: &str, mode: u8, rev: bool) {
    let digit = match mode {
        1 => '1',
        2 => '2',
        3 => '3',
        4 => '4',
        _ => '0',
    };
    let txt = if rev {
        std::format!("{}R", digit)
    } else {
        digit.to_string()
    };
    if std::fs::write(path, txt.as_bytes()).is_ok() {
        crate::sd::commit();
    }
}

/// Read the persisted `(mode, reverse)` pair from a `digit[R]` pref file. First
/// byte = mode digit; a trailing `R` (legacy files lacked it) = reversed.
fn read_sort_prefs(path: &str) -> (u8, bool) {
    use std::io::Read;
    let mut f = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(_) => return (0, false),
    };
    let mut b = [0u8; 4];
    let n = f.read(&mut b).unwrap_or(0);
    if n == 0 {
        return (0, false);
    }
    let mode = match b[0] {
        b'1' => 1,
        b'2' => 2,
        b'3' => 3,
        b'4' => 4,
        _ => 0,
    };
    let rev = b[..n].iter().any(|&c| c == b'R' || c == b'r');
    (mode, rev)
}

/// Boot-once load of persisted playtime + sort preferences into memory.
fn ensure_prefs_loaded() {
    if !PREFS_LOADED.swap(true, std::sync::atomic::Ordering::Relaxed) {
        crate::playtime::load();
        crate::favorites::load();
        crate::tags::load();
        let (mode, rev) = read_sort_prefs(&config_read_path(SORT_FILE));
        SORT_MODE.store(mode, std::sync::atomic::Ordering::Relaxed);
        SORT_REVERSE.store(rev, std::sync::atomic::Ordering::Relaxed);
        let (mode, rev) = read_sort_prefs(&config_read_path(IMPORT_SORT_FILE));
        IMPORT_SORT.store(mode, std::sync::atomic::Ordering::Relaxed);
        IMPORT_REVERSE.store(rev, std::sync::atomic::Ordering::Relaxed);
        load_folder_sorts();
    }
}

// ── IMPORTER home sort (Y on the saved-URL list) ─────────────────────────
//
// Separate from the JOUER sort: different list, different modes, and the user
// expects each tab to keep its own choice. 0 = ADDED (most recent first — the
// order URLs arrive in, which is what you want right after importing), 1 = NAME
// (by the row's display label), 2 = SOURCE (grouped by host), 3 = FILES (how
// many `.swf` the source holds).
const IMPORT_SORT_FILE: &str = "import_sort.txt";
static IMPORT_SORT: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);
static IMPORT_REVERSE: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);
/// Number of IMPORTER sort modes (matches the labels in the sort modal).
pub(crate) const IMPORT_SORT_MODES: usize = 4;

/// 0..=3 index of the active IMPORTER sort (also the sort modal's cursor).
pub(crate) fn import_sort_index() -> usize {
    (IMPORT_SORT.load(std::sync::atomic::Ordering::Relaxed) as usize).min(IMPORT_SORT_MODES - 1)
}

/// Whether the active IMPORTER sort is reversed (the modal's X toggle).
pub(crate) fn import_sort_reverse() -> bool {
    IMPORT_REVERSE.load(std::sync::atomic::Ordering::Relaxed)
}

/// Set + persist the IMPORTER sort mode and direction together (they're only
/// ever changed together, when A is pressed on the sort modal).
pub(crate) fn set_import_sort(idx: u8, rev: bool) {
    IMPORT_SORT.store(idx, std::sync::atomic::Ordering::Relaxed);
    IMPORT_REVERSE.store(rev, std::sync::atomic::Ordering::Relaxed);
    write_sort_pref(&config_write_path(IMPORT_SORT_FILE), idx, rev);
}

/// On return from a game, add its session duration to the running total.
/// Consumes `LAUNCH_TICK` so each session is banked exactly once.
fn bank_playtime() {
    let start = LAUNCH_TICK.lock().ok().and_then(|mut g| g.take());
    if let (Some(start), Some(base)) = (start, last_played_basename()) {
        let now = unsafe { ruffle_tick_now() };
        let freq = unsafe { ruffle_tick_freq() };
        if freq > 0 && now > start {
            crate::playtime::add(&base, (now - start) / freq);
        }
    }
}

/// Sort `entries` in place by the given mode (alpha tiebreak everywhere). When
/// `reverse` is set, the whole sorted order is flipped (so A-Z → Z-A, newest →
/// oldest, biggest → smallest, etc.).
pub(crate) fn sort_entries(entries: &mut std::vec::Vec<Entry>, mode: SortMode, reverse: bool) {
    let alpha = |a: &Entry, b: &Entry| {
        a.display_name
            .to_lowercase()
            .cmp(&b.display_name.to_lowercase())
    };
    match mode {
        SortMode::Alpha => entries.sort_by(alpha),
        SortMode::Recent => entries.sort_by(|a, b| b.mtime.cmp(&a.mtime).then_with(|| alpha(a, b))),
        SortMode::RecentlyPlayed => entries.sort_by(|a, b| {
            crate::playtime::get_last(&b.basename)
                .cmp(&crate::playtime::get_last(&a.basename))
                .then_with(|| alpha(a, b))
        }),
        SortMode::MostPlayed => entries.sort_by(|a, b| {
            crate::playtime::get(&b.basename)
                .cmp(&crate::playtime::get(&a.basename))
                .then_with(|| alpha(a, b))
        }),
        SortMode::Size => {
            entries.sort_by(|a, b| b.card_bytes.cmp(&a.card_bytes).then_with(|| alpha(a, b)))
        }
    }
    if reverse {
        entries.reverse();
    }
    // Favorites pinned to the top, regardless of sort mode / reverse. A stable
    // partition (sort_by_key on a bool: false=0 sorts first) keeps the active
    // order within the favorites and non-favorites groups.
    entries.sort_by_key(|e| !crate::favorites::is_favorite(&e.basename));
}

/// The icon of sort `mode` (`sort_mode_index`): the same on the strip under the
/// tabs as on that sort's row in the sort window, so the two read as one thing.
pub(crate) fn sort_icon(mode: usize) -> crate::backend::render::Icon {
    use crate::backend::render::Icon;
    match mode {
        1 => Icon::Calendar,
        2 => Icon::History,
        3 => Icon::Flame,
        4 => Icon::HardDrive,
        _ => Icon::SortAlpha,
    }
}

/// The order sort `mode` gives, written as what the player sees and with its
/// direction in it ("Derniers ajoutés", "De Z à A"). The sort window and the
/// strip use these same words. The old labels were menu nouns ("Ajouté",
/// "Dernier joué") under a "Croissant / Décroissant" switch that said the
/// opposite of the list for four sorts out of five, because their natural
/// order is the descending one.
pub(crate) fn sort_state_label(mode: usize, reverse: bool) -> &'static str {
    let u = crate::loc_ui::ui();
    let (natural, flipped) = match mode {
        1 => (u.sort_newest, u.sort_oldest),
        2 => (u.sort_played_recently, u.sort_played_long_ago),
        3 => (u.sort_most_played, u.sort_least_played),
        4 => (u.sort_largest, u.sort_smallest),
        _ => (u.sort_az, u.sort_za),
    };
    if reverse { flipped } else { natural }
}

/// What sort `mode` is BY, for its row in the sort window ("Date d'ajout").
pub(crate) fn sort_criterion(mode: usize) -> &'static str {
    let (lc, u) = (crate::loc::s(), crate::loc_ui::ui());
    match mode {
        1 => u.sort_by_added,
        2 => u.sort_by_last_played,
        3 => u.sort_by_playtime,
        4 => lc.sort_size,
        _ => lc.sort_alpha,
    }
}

/// The key `folder_filter` holds while the Favourites chip is open: a slash,
/// which no folder name can contain (FAT forbids it, the same reason
/// `ROOT_SORT_KEY` is one), and a star, so its line in `sort_folders.txt` says
/// what it is. Being a shelf like the others, it keeps a sort of its own.
pub(crate) const FAVORITES_SHELF: &str = "/\u{2605}";

/// One stop of the shelf strip and of the ZL / ZR ring: the `folder_filter` it
/// sets (`None` for TOUS) and what its chip shows.
pub(crate) struct ShelfStop {
    pub key: Option<std::string::String>,
    pub kind: crate::backend::render::HomeChipKind,
    pub label: std::string::String,
    pub count: usize,
}

/// The stops, in the strip's order (Main.dc.html): TOUS, then the favourites
/// when there is one, then every shelf by name.
fn shelf_stops(entries: &[Entry]) -> std::vec::Vec<ShelfStop> {
    use crate::backend::render::HomeChipKind;
    let u = crate::loc_ui::ui();
    let mut out = std::vec![ShelfStop {
        key: None,
        kind: HomeChipKind::All,
        label: u.home_all.to_string(),
        count: entries.len(),
    }];
    let favs = entries.iter().filter(|e| crate::favorites::is_favorite(&e.basename)).count();
    if favs > 0 {
        out.push(ShelfStop {
            key: Some(FAVORITES_SHELF.to_string()),
            kind: HomeChipKind::Favorites,
            label: u.home_favorites.to_string(),
            count: favs,
        });
    }
    for (name, count) in folder_counts(entries) {
        // A directory on the card, or only a label: two icons, as the options'
        // folder page tells the two apart.
        let kind = if shelf_has_directory(entries, &name) { HomeChipKind::Folder } else { HomeChipKind::Label };
        out.push(ShelfStop { key: Some(name.clone()), kind, label: name, count });
    }
    out
}

/// What the strip shows, hashed without allocating, since it is asked every
/// frame: each game's folder, its labels, whether it is a favourite, and the
/// language the fixed chips are named in.
fn shelves_fingerprint(entries: &[Entry]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let mut eat = |b: u8| {
        h ^= b as u64;
        h = h.wrapping_mul(0x0100_0000_01b3);
    };
    eat(crate::loc::current() as u8);
    for e in entries {
        for b in e.folder.as_bytes() {
            eat(*b);
        }
        eat(0xFF);
        for t in &e.tags {
            for b in t.as_bytes() {
                eat(*b);
            }
            eat(0xFE);
        }
        eat(crate::favorites::is_favorite(&e.basename) as u8);
    }
    h ^ entries.len() as u64
}

/// The stops, and their fingerprint, built again only when it changes: the
/// shelf counts walk every game's shelves and the gallery asks every frame.
static SHELF_STOPS: Mutex<(Option<u64>, std::vec::Vec<ShelfStop>)> = Mutex::new((None, std::vec::Vec::new()));

fn with_shelf_stops<R>(entries: &[Entry], f: impl FnOnce(&[ShelfStop], u64) -> R) -> R {
    let fp = shelves_fingerprint(entries);
    let mut g = match SHELF_STOPS.lock() {
        Ok(g) => g,
        Err(poisoned) => poisoned.into_inner(),
    };
    if g.0 != Some(fp) {
        g.1 = shelf_stops(entries);
        g.0 = Some(fp);
    }
    f(&g.1, fp)
}

/// The keys of the stops, in order, and which one is open.
fn shelf_keys(s: &State) -> (std::vec::Vec<Option<std::string::String>>, usize) {
    let keys: std::vec::Vec<Option<std::string::String>> =
        with_shelf_stops(&s.entries, |stops, _| stops.iter().map(|st| st.key.clone()).collect());
    let cur = keys.iter().position(|k| k.as_deref() == s.folder_filter.as_deref()).unwrap_or(0);
    (keys, cur)
}

/// The game each shelf was left on, by path (TOUS under `None`), so walking to
/// another shelf, or another tab, and back lands where you were rather than on
/// the first game. A Vec, not a HashMap: `HashMap::new` crashes on Horizon
/// without the RandomState patch, the reason favorites.rs keeps a Vec too.
static SHELF_CURSOR: Mutex<std::vec::Vec<(Option<std::string::String>, std::string::String)>> =
    Mutex::new(std::vec::Vec::new());

/// Remember the game at `selection` (a row of what is on screen) for the open
/// shelf.
fn shelf_cursor_save(s: &State, selection: usize) {
    let shown = local_filtered_indices(&s.entries, &s.local_filter, &s.folder_filter);
    let Some(path) = shown.get(selection).and_then(|&i| s.entries.get(i)).map(|e| e.path.clone()) else {
        return;
    };
    if let Ok(mut g) = SHELF_CURSOR.lock() {
        match g.iter_mut().find(|(k, _)| k.as_deref() == s.folder_filter.as_deref()) {
            Some(e) => e.1 = path,
            None => g.push((s.folder_filter.clone(), path)),
        }
    }
}

/// The row of the game at `path` in what is on screen, if it is there.
fn row_of_path(s: &State, path: &str) -> Option<usize> {
    let shown = local_filtered_indices(&s.entries, &s.local_filter, &s.folder_filter);
    shown.iter().position(|&i| s.entries.get(i).is_some_and(|e| e.path == path))
}

/// The row the open shelf was last left on, or the first.
fn shelf_cursor_row(s: &State) -> usize {
    let path = SHELF_CURSOR.lock().ok().and_then(|g| {
        g.iter().find(|(k, _)| k.as_deref() == s.folder_filter.as_deref()).map(|(_, p)| p.clone())
    });
    path.and_then(|p| row_of_path(s, &p)).unwrap_or(0)
}

/// Open the shelf `key` (`None` = TOUS): ZL / ZR, a tap on its chip, or B back
/// to TOUS. The content slides in from `dir`'s side while the strip stays still
/// and its fill travels to the new chip. `keep` names a game to stay on (B
/// keeps the selected one); otherwise the cursor goes back to where it was
/// last left on that shelf.
fn goto_shelf(s: &mut State, key: Option<std::string::String>, dir: f32, selection: usize, keep: Option<std::string::String>) {
    shelf_cursor_save(s, selection);
    // Walking to another shelf ends the search: the term was typed against
    // the shelf you were on, and carrying it over would show a handful of rows
    // with no visible reason.
    s.local_filter = None;
    s.folder_filter = key;
    set_active_folder(s.folder_filter.as_deref());
    // Each shelf keeps its own sort (falling back to the home's), so the list
    // has to be re-ordered for the one just walked into.
    sort_entries(&mut s.entries, current_sort_mode(), current_sort_reverse());
    let row = keep.and_then(|p| row_of_path(s, &p)).unwrap_or_else(|| shelf_cursor_row(s));
    // A different list: snap every view's glide and start its covers closed,
    // or the gallery slides in from rows that no longer exist.
    crate::backend::render::gallery_anim_reset();
    crate::backend::render::tile_open_reset();
    crate::backend::render::home_header_pin(true);
    crate::backend::render::tab_transition_begin(dir);
    s.screen = Screen::List { selection: row, scroll_offset: home_scroll_for(row, 0) };
}

/// The scroll that keeps `sel` on screen, starting from `scroll`, for the view
/// in use. Worked out from the view's own shape rather than from the layout the
/// last frame published, which still describes the list or the view from
/// before when the shelf or the view has just changed.
fn home_scroll_for(sel: usize, scroll: usize) -> usize {
    let per_row = match crate::loc::home_view() {
        1 => 1,
        2 | 3 => return 0,
        _ => GALLERY_COLS,
    };
    let visible = home_rows_visible();
    let row = sel / per_row;
    if row < scroll {
        row
    } else if row >= scroll + visible {
        row + 1 - visible
    } else {
        scroll
    }
}

/// A shelf can end while the cursor is on it: its last favourite taken off from
/// the options, its last label removed. Back to TOUS then, on the same game,
/// before the frame is drawn, rather than leaving the list on the fallback
/// with a strip that no longer has the shelf to light. Returns the cursor to
/// draw with.
fn settle_vanished_shelf(selection: usize, scroll: usize) -> (usize, usize) {
    let Ok(mut s) = LIBRARY.lock() else { return (selection, scroll) };
    let vanished = match s.folder_filter.as_deref() {
        Some(f) => !s.entries.iter().any(|e| on_shelf(e, f)),
        None => false,
    };
    if !vanished {
        return (selection, scroll);
    }
    // The list on screen was the fallback (every game), so the row is a row
    // of the whole library.
    let shown = local_filtered_indices(&s.entries, &s.local_filter, &s.folder_filter);
    let keep = shown.get(selection).and_then(|&i| s.entries.get(i)).map(|e| e.path.clone());
    s.folder_filter = None;
    set_active_folder(None);
    sort_entries(&mut s.entries, current_sort_mode(), current_sort_reverse());
    let row = keep.and_then(|p| row_of_path(&s, &p)).unwrap_or(0);
    let scroll = home_scroll_for(row, 0);
    s.screen = Screen::List { selection: row, scroll_offset: scroll };
    crate::backend::render::gallery_anim_reset();
    (row, scroll)
}

fn note_played(path: &str, basename: &str, display_name: &str) {
    if let Ok(mut g) = LAST_PLAYED.lock() {
        *g = Some((path.into(), basename.into(), display_name.into()));
    }
}

/// Set the "active game" (LAST_PLAYED → the pause modal's name subtitle) from a
/// SWF path, for the forwarder launch that bypasses the library UI. Mirrors a
/// normal library entry: basename = the filename, display name = the
/// `.meta.json` real-title override (Flashpoint downloads carry one), else the
/// basename. Called from `ruffle_set_swf_path` when no game is active yet.
pub fn note_played_from_path(path: &str) {
    let basename = path
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(path)
        .to_string();
    if basename.is_empty() {
        return;
    }
    let display_name = read_meta_sidecar(&basename)
        .and_then(|m| m.display_name)
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| pretty_name(&basename));
    note_played(path, &basename, &display_name);
}

fn last_played_basename() -> Option<std::string::String> {
    LAST_PLAYED.lock().ok().and_then(|g| g.as_ref().map(|(_, b, _)| b.clone()))
}

/// Path of the game last launched. The identity to re-find it by.
fn last_played_path() -> Option<std::string::String> {
    LAST_PLAYED.lock().ok().and_then(|g| g.as_ref().map(|(p, _, _)| p.clone()))
}

/// (basename, display name) of every game in the library, once each. For the
/// COPY FROM ANOTHER GAME list; never call it with LIBRARY held.
///
/// A game launched from its HOME tile runs without the library: then the
/// games that have controls of their own are read off the card instead, from
/// their `<basename>.keymap.json` sidecars, named as the scan would name them.
pub fn owned_games() -> std::vec::Vec<(std::string::String, std::string::String)> {
    let mut seen = std::collections::BTreeSet::new();
    if let Ok(g) = LIBRARY.try_lock() {
        if !g.entries.is_empty() {
            return g
                .entries
                .iter()
                .filter(|e| seen.insert(e.basename.clone()))
                .map(|e| (e.basename.clone(), e.display_name.clone()))
                .collect();
        }
    }
    let mut out = std::vec::Vec::new();
    for root in search_roots() {
        for file in list_files_top(&root) {
            let Some(basename) = file.strip_suffix(".keymap.json") else {
                continue;
            };
            if basename.is_empty() || !seen.insert(basename.to_string()) {
                continue;
            }
            let name = read_meta_sidecar(basename)
                .and_then(|m| m.display_name)
                .filter(|s| !s.trim().is_empty())
                .unwrap_or_else(|| pretty_name(basename));
            out.push((basename.to_string(), name));
        }
    }
    out
}

/// Display name of the currently/last launched game — used by the pause
/// menu (`render::draw_menu_overlay`) to show the title under "PAUSE".
pub fn active_display_name() -> Option<std::string::String> {
    LAST_PLAYED.lock().ok().and_then(|g| g.as_ref().map(|(_, _, n)| n.clone()))
}

/// Sidecar JSON written next to the SWF — gives a display name override
/// without touching the physical filename. Per the README 3.4.bis design:
/// **never** rename the .swf file itself (saves/keymap/etc. all key off
/// basename). Sidecar lives at `sdmc:/ruffle/<basename>.meta.json`.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub(crate) struct MetaSidecar {
    pub display_name: Option<std::string::String>,
}

/// Built-in SD roots, in lookup priority order. The legacy `sdmc:/ruffle/` is
/// kept for backward compat — users coming from pre-rename builds still see
/// their saves and sidecars without manual migration.
const USER_SD_ROOTS: &[&str] = &["sdmc:/flashnx", "sdmc:/ruffle"];

/// Where the player has asked new games to live, one line, no trailing slash.
/// Absent = the built-in default. Same shape as `cursor_speed`: a tiny file
/// beside the library rather than an entry in a settings blob, so it can be
/// inspected and fixed from a card reader when something goes wrong.
/// Where the games folder is recorded — beside the `.nro`, not beside the
/// games.
///
/// A lone file in an otherwise empty `sdmc:/flashnx/` reads as leftovers, and
/// deleting leftovers is exactly what a tidy person does; the app would then
/// forget where its library went. `sdmc:/switch/FlashNX/` is the homebrew's own
/// folder, next to the `.nro` and the diagnostic markers, and nobody empties
/// that. It is also the one path that can never itself be moved.
const GAMES_DIR_FILE: &str = "sdmc:/switch/FlashNX/games_dir";

/// Where the pointer used to live. Read-only, and migrated on the first
/// write: an install that already chose a folder must not lose it just
/// because the bookkeeping moved.
const GAMES_DIR_FILE_LEGACY: &str = "sdmc:/flashnx/games_dir";

/// The root that RECEIVES new games and new sidecars.
///
/// Defaults to `sdmc:/flashnx`, and follows the player's choice once they pick
/// a folder in REGLAGES (#79: SD cards laid out for emulation frontends want
/// `roms/flashnx`, and others want something else entirely, so this is a path
/// and not a switch between two conventions).
///
/// Read from disk on every call rather than cached: it changes at most once in
/// a session, from a menu, and a stale cache here would write a download into
/// the old folder while the library scans the new one.
/// Cached answer, because this is read from drawing code.
///
/// The REGLAGES row and the folder picker both show the current folder, so an
/// uncached read meant two file opens and a `stat` on every frame — a few
/// milliseconds of SD I/O per frame on Horizon, where each of those is an IPC
/// round trip. It was enough to make the console's screenshot capture fail on
/// that screen. Invalidated by `set_primary_root`, the only writer.
static PRIMARY_ROOT_CACHE: Mutex<Option<std::string::String>> = Mutex::new(None);

pub(crate) fn primary_root() -> std::string::String {
    if let Ok(g) = PRIMARY_ROOT_CACHE.lock() {
        if let Some(v) = g.as_ref() {
            return v.clone();
        }
    }
    let v = primary_root_uncached();
    if let Ok(mut g) = PRIMARY_ROOT_CACHE.lock() {
        *g = Some(v.clone());
    }
    v
}

fn primary_root_uncached() -> std::string::String {
    let recorded = read_small_file(GAMES_DIR_FILE)
        .or_else(|| read_small_file(GAMES_DIR_FILE_LEGACY));
    if let Some(raw) = recorded {
        let chosen = raw.trim().trim_end_matches('/');
        // A folder that no longer exists (card swapped, folder deleted) must not
        // strand downloads: fall back rather than fail, and say so in the log.
        if !chosen.is_empty() {
            if dir_exists(chosen) {
                return chosen.into();
            }
            crate::net::log(&std::format!(
                "games dir: {} is gone, falling back to {}\n",
                chosen, USER_SD_ROOTS[0],
            ));
        }
    }
    USER_SD_ROOTS[0].into()
}

/// Record the player's chosen games folder. Empty string restores the default.
pub(crate) fn set_primary_root(path: &str) {
    let trimmed = path.trim().trim_end_matches('/');
    // Drop the cache first: everything below re-reads through `primary_root`.
    if let Ok(mut g) = PRIMARY_ROOT_CACHE.lock() {
        *g = None;
    }
    // The old copy goes either way, so the two can never disagree.
    let _ = std::fs::remove_file(GAMES_DIR_FILE_LEGACY);
    if trimmed.is_empty() || trimmed == USER_SD_ROOTS[0] {
        let _ = std::fs::remove_file(GAMES_DIR_FILE);
        return;
    }
    let _ = std::fs::create_dir_all("sdmc:/switch/FlashNX");
    let _ = std::fs::write(GAMES_DIR_FILE, trimmed.as_bytes());
}

/// Every root worth SEARCHING, chosen folder first, then the built-ins. The
/// chosen one leads because that is where the current session writes; the
/// built-ins stay so a half-finished move never hides a game.
fn search_roots() -> std::vec::Vec<std::string::String> {
    let mut roots: std::vec::Vec<std::string::String> = std::vec::Vec::with_capacity(3);
    let primary = primary_root();
    if !USER_SD_ROOTS.iter().any(|r| *r == primary) {
        roots.push(primary);
    }
    for r in USER_SD_ROOTS {
        roots.push((*r).into());
    }
    roots
}

/// Where to READ one of the app's own files: the games folder first, then the
/// built-in roots so an install that predates the move still finds its settings
/// and migrates them on the next write.
pub(crate) fn config_read_path(name: &str) -> std::string::String {
    for root in search_roots() {
        let p = std::format!("{}/{}", root, name);
        if file_exists(&p) {
            return p;
        }
    }
    std::format!("{}/{}", primary_root(), name)
}

/// Delete one of the app's own files from EVERY root it could be sitting in.
///
/// Clearing a setting has to clear it everywhere, not just where we would write
/// it: a file written before the library moved is still on the old root, and
/// `config_read_path` would find it again the moment the new one is gone. The
/// setting would come back by itself, which reads as the app ignoring you.
pub(crate) fn config_remove_everywhere(name: &str) {
    for root in search_roots() {
        let _ = std::fs::remove_file(std::format!("{}/{}", root, name));
    }
}

/// Where to WRITE one of the app's own files: always beside the library.
///
/// Everything the app owns travels with the games, because "moved" should mean
/// moved. Exactly one file cannot: `games_dir`, which records where the library
/// went — a pointer that lived with the thing it points at would be unfindable.
pub(crate) fn config_write_path(name: &str) -> std::string::String {
    std::format!("{}/{}", primary_root(), name)
}

/// Find a user-facing sidecar / config file by suffix (e.g.
/// "Super_Mario_63_2010.swf.meta.json") under one of the known SD
/// roots. Returns the first existing path, or None.
fn find_user_file(suffix: &str) -> Option<std::string::String> {
    for root in search_roots() {
        let p = std::format!("{}/{}", root, suffix);
        if file_exists(&p) {
            return Some(p);
        }
    }
    None
}

/// Where to WRITE a user-facing sidecar / config: the player's games folder.
fn primary_user_path(suffix: &str) -> std::string::String {
    std::format!("{}/{}", primary_root(), suffix)
}

/// The name a game is shown under when it has never been renamed: its file
/// name, made readable.
///
/// The file name was shown as is, extension included ("Super Mario
/// Cloud.swf"), and the files that come from Flashpoint carry its file-system
/// escapes: ":" became "_ " ("Jinx_ Dark and Stormy Night") and "'" became "_"
/// ("Garfield_s Scary Scavenger Hunt"). A download from a site often arrives as
/// "haunt-the-house-719579f9": dashes for spaces and a hash on the end. None of
/// it is part of the game's name, so none of it is shown. RENOMMER still wins
/// over all of this, and the file on the card is not touched.
pub(crate) fn pretty_name(basename: &str) -> std::string::String {
    let stem = basename
        .strip_suffix(".swf")
        .or_else(|| basename.strip_suffix(".SWF"))
        .unwrap_or(basename);
    let mut name = stem.replace("_ ", ": ").replace("_s ", "'s ");
    if name.ends_with("_s") {
        name.truncate(name.len() - 2);
        name.push_str("'s");
    }
    name = name.replace('_', " ");
    if !name.contains(' ') && name.contains('-') {
        let mut parts: std::vec::Vec<&str> = name.split('-').filter(|p| !p.is_empty()).collect();
        if parts.len() > 1 && parts.last().map_or(false, |t| looks_like_download_hash(t)) {
            parts.pop();
        }
        name = parts.join(" ");
    }
    let name = name.split_whitespace().collect::<std::vec::Vec<_>>().join(" ");
    if name.is_empty() {
        return basename.to_string();
    }
    // An all-lower-case name ("super gravity rush") gets its words capitalised;
    // a name that already has capitals is left exactly as its author wrote it.
    if !name.chars().any(|c| c.is_uppercase()) {
        return name
            .split(' ')
            .map(|w| {
                let mut cs = w.chars();
                match cs.next() {
                    Some(f) => f.to_uppercase().chain(cs).collect::<std::string::String>(),
                    None => std::string::String::new(),
                }
            })
            .collect::<std::vec::Vec<_>>()
            .join(" ");
    }
    name
}

fn read_meta_sidecar(basename: &str) -> Option<MetaSidecar> {
    let suffix = std::format!("{}.meta.json", basename);
    let path = find_user_file(&suffix)?;
    // Bounded read (not read_to_string) so display-name overrides survive
    // applet mode too — see read_sd_text.
    let txt = read_sd_text(&path, 64 * 1024).ok()?;
    serde_json::from_str(&txt).ok()
}

/// Persist a display-name override. Empty string removes the sidecar so
/// the library reverts to showing the basename. Always writes to the
/// primary root (`sdmc:/flashnx/`); legacy `sdmc:/ruffle/<...>.meta.json`
/// is left untouched (would orphan, but harmless).
fn write_meta_sidecar(basename: &str, display_name: &str) -> bool {
    let path = primary_user_path(&std::format!("{}.meta.json", basename));
    if display_name.trim().is_empty() {
        // "Leave empty to revert" — so success here means BOTH copies are gone.
        // It used to return true having verified neither: a sidecar that refused
        // to unlink reported a successful revert and then resurrected the old
        // name at the next scan. A file that was already absent is success.
        let gone = |r: std::io::Result<()>| {
            matches!(&r, Ok(())) || matches!(&r, Err(e) if e.kind() == std::io::ErrorKind::NotFound)
        };
        let mut ok = gone(std::fs::remove_file(&path).inspect(|_| note_file_removed(&path)));
        // Also try to clean up any legacy copy so the next library
        // boot doesn't resurrect a stale display name.
        if let Some(legacy) = find_user_file(&std::format!("{}.meta.json", basename)) {
            ok &= gone(std::fs::remove_file(&legacy).inspect(|_| note_file_removed(&legacy)));
        }
        crate::sd::commit();
        if !ok {
            log(&std::format!("library: could not remove the meta sidecar for {}\n", basename));
        }
        return ok;
    }
    let meta = MetaSidecar {
        display_name: Some(display_name.to_string()),
    };
    match serde_json::to_string_pretty(&meta) {
        Ok(json) => {
            let ok = std::fs::write(&path, json.as_bytes()).is_ok();
            if ok {
                note_file_created(&path); // renamed this session → index must see it
                crate::sd::commit();
            }
            ok
        }
        Err(_) => false,
    }
}

/// Cached SWF header data parsed once at scan time.
#[derive(Debug, Clone)]
pub(crate) struct Entry {
    pub path: std::string::String,
    pub basename: std::string::String,
    /// Folder this game sits in ON THE CARD, relative to the games root, empty
    /// at the root (issue #68). Only the FIRST segment: a game three levels down
    /// belongs to its top-level folder as far as the library is concerned,
    /// because that is the shelf a player thinks in. Nothing here is a key --
    /// see `basename`.
    ///
    /// This is the PHYSICAL half of shelf membership and is what the move and
    /// empty code acts on. It is NOT the answer to "is this game on shelf X" --
    /// ask `on_shelf`, which also weighs the labels below.
    pub folder: std::string::String,
    /// Shelf LABELS from `tags.json`, keyed by basename. The other half: a game
    /// is on the union of its directory and these, which is what lets one game
    /// sit on several shelves without anything being copied.
    ///
    /// Snapshotted into the entry at scan time rather than looked up per frame:
    /// the gallery asks about shelf membership for every game on every draw, and
    /// that is not a question worth taking a mutex for sixty times a second.
    /// `refresh_tags` puts it back in step after any change.
    pub tags: std::vec::Vec<std::string::String>,
    pub display_name: std::string::String,
    /// The SWF header's declared length (the UNCOMPRESSED size of a CWS or ZWS
    /// file), or the `.filesize` footprint of a multi-file game. The bug report
    /// sends this beside the header's own number; what the player is shown is
    /// `card_bytes`.
    pub size_bytes: u64,
    /// What the game takes on the card: the `.swf` as stored, or for a
    /// multi-file game the footprint of the file and its companion folder. The
    /// facts line and the size sort read this. They used to read `size_bytes`,
    /// so a 6 MB compressed file showed about 18 MB beside an icon promising
    /// its size on the card.
    pub card_bytes: u64,
    /// The game has a `<stem>.files/` companion folder: its tile carries the
    /// layers badge and its facts say it is made of several files. Those are
    /// the big games that take longest to start, and the launch was the only
    /// place that said so, too late to help.
    pub multi: bool,
    pub swf_version: u8,
    /// 0 = uncompressed FWS, 1 = zlib CWS, 2 = lzma ZWS.
    pub compression_label: &'static str,
    /// True if the movie is ActionScript 3 (AVM2). Named in the facts line under
    /// the selection, in the same colour as every other fact, and NOWHERE else.
    ///
    /// It used to carry an amber badge on the tile as well, from a time when
    /// AVM2 really was the riskier engine. It is not one now: enough has been
    /// fixed that whether a game runs has stopped tracking which ActionScript it
    /// was written in, and a warning colour on a third of the library was
    /// telling players their game was fragile when it was not.
    pub is_as3: bool,
    /// 0xRRGGBB derived from a hash of the basename — drives the per-game
    /// color chip in the list. Same hash always produces the same color
    /// across reboots (no persistence needed) because the input is stable.
    pub color_chip: u32,
    /// File mtime (seconds since epoch, 0 if unavailable) — drives the "recent
    /// first" sort. Relative order is reliable even with a wrong console clock
    /// (all files are stamped by the same clock).
    pub mtime: u64,
}

pub(crate) struct State {
    pub(crate) screen: Screen,
    pub(crate) entries: std::vec::Vec<Entry>,
    /// Set when the user presses A on a game. Read by C++ via
    /// `ruffle_library_selected_path` after the loop exits.
    selected_path: Option<std::string::String>,
    /// GL texture id for the FlashNX banner image (assets/banner.png decoded
    /// at init). 0 = not loaded (decode failed or init not called yet).
    /// Folder currently shown by `GamesDirPicker`, and its subfolder names.
    /// `Screen` is `Copy`, so the path cannot ride in the variant; both live
    /// here and are filled when a level opens, so drawing never touches the SD.
    pub(crate) dir_path: std::string::String,
    pub(crate) dir_browse: std::vec::Vec<std::string::String>,
    /// Games folder the user just accepted. Picked up by `input()` once the
    /// library lock is released, which queues the move; the move itself then
    /// advances a slice per frame from `render`. Everything about this has to
    /// happen outside the lock — the rescan at the end re-enters the library,
    /// and doing it in the handler deadlocks the app instead of changing a
    /// setting.
    pub(crate) pending_games_dir: Option<std::string::String>,
    pub(crate) banner_tex: u32,
    pub(crate) banner_w: u32,
    pub(crate) banner_h: u32,
    /// Monotonic wall-clock ticks captured at init; render() subtracts this
    /// to feed a stable phase into sin() animations (cursor pulse, selection
    /// pulse).
    pub(crate) anim_origin_ticks: u64,
    // ── Phase 3.7 DISTANT mode auxiliary state ─────────────────────────
    /// Files listed by the last archive.org metadata fetch. Populated in
    /// `enter_distant_via_url`, cleared on every successful navigation
    /// back to LOCAL. Indexed by `Screen::DistantFiles::selection`.
    pub(crate) remote_files: std::vec::Vec<RemoteFile>,
    /// Filename + target SD path of the file currently downloading. Saved
    /// so `Screen::DistantDownloading` can show them and post-download
    /// can `add_path` to the local list without re-deriving.
    pub(crate) download_file_name: std::string::String,
    pub(crate) download_out_path: std::string::String,
    /// Source URL of the in-flight download (direct `.swf`, archive.org file, or
    /// Flashpoint GameZIP). Written to a `<game>.swf.url` sidecar on completion so
    /// a later bug report can name where the game came from.
    pub(crate) download_source_url: std::string::String,
    /// Set when the in-flight download is a Flashpoint GameZIP: holds the FINAL
    /// `.swf` path to extract the zip into (the download itself goes to a temp
    /// `.zip`). `None` for a normal direct/archive.org `.swf` download.
    pub(crate) download_zip_extract: Option<std::string::String>,
    /// Set when the in-flight download is a NON-zipped Flashpoint game: the file
    /// downloaded straight from the htdocs mirror IS the entry `.swf` (no zip to
    /// extract), so `on_download_finished` skips extraction, fetches companions
    /// from htdocs, then runs the Flashpoint finalize (cover/title/base sidecar).
    /// `download_out_path` == the final `.swf` path in this case. False for
    /// zipped GameZIP downloads and for archive.org/direct downloads.
    pub(crate) download_fp_direct: bool,
    /// Set for a Flashpoint GameZIP download: the `launchCommand` (entry SWF URL,
    /// e.g. `http://i.flipline.com/.../PapaLouie2_v2_1.swf`). A GameZIP can bundle
    /// several SWF versions; this picks the right one to launch. Empty otherwise.
    pub(crate) download_launch_command: std::string::String,
    /// Set when a Flashpoint game download is in flight: the cover/logo URL to
    /// fetch + cache automatically once the `.swf` lands (so the game shows its
    /// art in JOUER without a manual "Jaquette" step). `None` for archive.org /
    /// direct downloads (those carry no known cover).
    pub(crate) download_cover_url: Option<std::string::String>,
    /// Set for a Flashpoint download: the REAL game title. The on-SD `.swf`
    /// filename is sanitized (e.g. `:` → `_`), so we persist the true title as a
    /// display-name sidecar once the file lands — the game then shows its real
    /// name AND a later "Jaquette" cover search uses the real title (not the
    /// mangled filename, which finds nothing). `None` for archive.org / direct.
    pub(crate) download_title: Option<std::string::String>,
    /// Last error message — shown in `Screen::DistantError` until user
    /// dismisses with A/B.
    pub(crate) distant_error: std::string::String,
    /// URL of the import attempt that produced `distant_error`, so the error
    /// screen can offer "Y: fix the URL" with the bad one PRE-FILLED instead of
    /// bouncing the user back to the list to retype it. Empty = the failure
    /// wasn't URL-level (e.g. a file download inside a valid item), so no Y.
    pub(crate) failed_url: std::string::String,
    /// Set of basenames already downloaded this session. Used to draw a
    /// `✓` next to entries in `Screen::DistantFiles` so the user knows
    /// what's been pulled. Cleared by `reset` (back-to-library).
    pub(crate) downloaded_basenames: std::vec::Vec<std::string::String>,
    /// URL history persisted across boots (see `distant_history.json`).
    /// `history_idx` indexes this; 0 = oldest, len-1 = most recent.
    /// Loaded lazily by `load_history_from_sd` on first DistantIdle entry.
    pub(crate) url_history: std::vec::Vec<std::string::String>,
    /// Currently-displayed history index in `Screen::DistantIdle`. Cycled
    /// with L/R. None when history is empty.
    pub(crate) history_idx: Option<usize>,
    /// Active substring filter on the DistantFiles list (Minus (-) = open
    /// swkbd to set/edit; empty input = clear). Lowercase, substring match
    /// against the lowercased filename. `None` = no filter (show all).
    pub(crate) distant_filter: Option<std::string::String>,
    /// Same idea for the IMPORTER HOME's saved-URL list (Minus). Matched against
    /// the URL *and* its display label, so "mario" finds
    /// `https://archive.org/details/super-mario-63` either way.
    pub(crate) import_filter: Option<std::string::String>,
    /// Blurb for the game shown in the Flashpoint details popup (`+`), fetched
    /// per-game when the popup opens. Empty = none published / fetch failed.
    pub(crate) fp_desc: std::string::String,
    /// `(url, .swf count, added epoch, favorite)` side data, persisted in
    /// `distant_history_meta.json`. Feeds the "4/13" badge, the options modal's
    /// "added on", and the favorite pin; a URL never opened has no count yet.
    /// Favorites live here rather than in `favorites.json` (which is keyed by
    /// game basename) so all per-URL data stays in one file.
    /// Per-URL side data: `(url, total_swf, added_epoch, favorite, have)`.
    ///
    /// `have` is how many of the item's `.swf` files are already on the card,
    /// counted BY NAME -- the same question the OK badge and the A-press guard
    /// ask. It is cached because the IMPORTER home lists every saved URL without
    /// fetching any of their file lists, so the count cannot be recomputed there;
    /// it is refreshed each time the item's list is opened, and bumped on every
    /// download from it.
    pub(crate) url_meta: std::vec::Vec<(std::string::String, u32, u64, bool, u32)>,
    /// True once `load_history_meta_from_sd` has a TRUSTWORTHY view of the meta
    /// file (parsed, or confirmed absent). False after a read/parse error, which
    /// blocks saving so we can't clobber a table we failed to read. Mirrors
    /// `history_loaded`.
    meta_loaded: bool,
    /// (selection, scroll_offset) snapshot taken when the user pressed A
    /// on a file in DistantFiles, so `on_download_finished` can put them
    /// back on the same row instead of jumping to the top of the list.
    /// Stored as filtered-list indices (matches the screen state).
    pub(crate) download_resume_pos: Option<(usize, usize)>,
    /// True once `load_history_from_sd` has established a TRUSTWORTHY view of
    /// the on-disk history: either a successful parse, or a confirmed-absent
    /// file (legit empty first boot). False after a read/parse ERROR — in
    /// that case we must NOT persist, or we'd overwrite URLs we merely failed
    /// to read. Guards every `save_history_to_sd` call site.
    history_loaded: bool,
    /// True when FlashNX runs with the small applet memory pool (album
    /// takeover) — games can't be launched (OOM). Set in `open()`. Drives the
    /// `AppletNotice` screen shown instead of the embedded red SWF (P1c).
    applet_mode: bool,
    /// Active substring filter on the LOCAL list (Minus (-) = open swkbd to
    /// set/edit; empty input = clear). Mirrors `distant_filter`: lowercase, substring
    /// match against the lowercased display name OR basename. `None` = show
    /// all. While set, `Screen::List` selection/scroll index the FILTERED
    /// view (see `local_filtered_indices`).
    local_filter: Option<std::string::String>,
    /// Folder chip currently picked on the home strip (issue #68). `None` = the
    /// TOUS chip, every game whatever folder it sits in. Not persisted: a shelf
    /// is where you are, not a setting, and coming back to a library that only
    /// shows a third of itself for a reason two sessions old is a bug report.
    pub(crate) folder_filter: Option<std::string::String>,
    /// Flashpoint cover candidates for the current `CoverPicker` (OPTIONS >
    /// JAQUETTE). Filled by `run_cover_search_flow`, indexed by the picker
    /// selection, consumed by `run_cover_fetch_flow`.
    cover_candidates: std::vec::Vec<crate::sources::flashpoint::CatalogEntry>,
    /// Cover picker showing screenshots instead of logos (Y toggles). Logos stay
    /// the default; the screenshot is an ALTERNATIVE offered per game (issue #59),
    /// not a new rule, because changing the default would restyle every cover
    /// already chosen. Not persisted: a per-visit view, not a setting.
    cover_use_shot: bool,
    /// Community control profiles matching the game in the open `ProfileList`
    /// (#20). Filled by `run_open_profiles_flow`, indexed by the picker selection.
    profile_matches: std::vec::Vec<crate::profiles::Match>,
    /// True while the picker lists the player's other games to copy from
    /// (COPY FROM ANOTHER GAME); `profile_saved_matches` keeps the community
    /// list for B.
    profile_copy_mode: bool,
    profile_saved_matches: std::vec::Vec<crate::profiles::Match>,
    /// Whether the open `TouchesMenu` shows a "revert" row (#20): true when the
    /// game has a hand-made backup to restore OR a community profile is currently
    /// applied (so reverting drops it). Recomputed on every entry to the sub-menu.
    touches_can_revert: bool,
    /// When `touches_can_revert`, whether a hand-made backup exists (revert
    /// RESTORES it) vs none (revert resets to the default controls). Drives which
    /// revert label is shown.
    touches_has_backup: bool,
    /// Snapshot of the game's per-game cursor-speed preset index for the open
    /// TOUCHES sub-menu (#20), or -1 = unset (shows the x1.0 default). The cursor
    /// row cycles it; persisted to `<basename>.cursor`.
    touches_cursor_idx: i32,
    /// Snapshot of the game's per-game "show cursor" flag for the open TOUCHES
    /// sub-menu (#20). The show-cursor row toggles it; persisted in the keymap.
    touches_show_cursor: bool,
    /// Same, for the gyroscope cursor row (#119), a `keymap::GYRO_*` mode;
    /// persisted in `<basename>.prefs`.
    touches_gyro: u8,
    /// Snapshotted before/after diff lines for the open `ProfilePreview` (#20),
    /// each like "Up: Space -> W". Built by `run_open_preview_flow`.
    preview_rows: std::vec::Vec<std::string::String>,
    /// Id of the community profile currently applied to the game in the open
    /// `ProfileList` (#20), so the picker can tag the active row. Empty = none.
    /// Snapshotted by `run_open_profiles_flow` from the keymap's provenance.
    active_profile_id: std::string::String,
    /// For the open `ProfileShareConfirm`: whether sharing will UPDATE the
    /// player's existing shared profile (vs create the first one). Drives the
    /// confirm subtitle so it's clear sharing edits one slot, not piles up (#20).
    share_is_update: bool,
    // Transient toast (#20): a small non-blocking banner drawn over the current
    // screen for `toast_frames` frames, instead of a full-screen "thanks" modal.
    // `toast_kind`: 0 = success (green), 1 = error (red), 2 = info (blue).
    toast_msg: std::string::String,
    toast_kind: u8,
    toast_frames: u32,
    /// Notice shown on the `CoverPicker` when there's no list to show (covers
    /// off / no results / fetch error). Empty = render the candidate list.
    cover_msg: std::string::String,
    /// Last search term used for the `CoverPicker`, so Minus (refine) can
    /// pre-fill the keyboard with it instead of the raw filename-derived query.
    cover_query: std::string::String,
    /// True while an async Flashpoint game search (X in the importer) is in
    /// flight: the `FpGallery` arm then shows a spinner and polls the async GET
    /// instead of drawing the (still-empty) cover grid. Avoids the UI freeze the
    /// old synchronous `gamezip::search` caused.
    fp_loading: bool,
    /// Flashpoint content filter for the importer's game search (X). `true` =
    /// Flashpoint's default "Filter entries" (hides mature-rated entries); the
    /// hidden ZL+ZR chord in the results grid flips it and re-runs the query.
    /// Session-only (not persisted) so every launch starts back on the safe
    /// default — there's deliberately no settings row exposing it.
    fp_content_filter: bool,
    /// Incremental companion-SWF download (multi-file game, v1.3.0). After the
    /// main GameZIP lands, each sibling SWF is pulled through the normal
    /// `https_download_*` path so it shows on the SAME progress bar ("LINKED
    /// FILES"). The `DistantDownloading` render arm drives this queue: download a
    /// companion, read it back, scan it for further companions (BFS), repeat,
    /// then finalize. `seen` holds every name ever queued so we don't loop.
    dl_companion_active: bool,
    dl_companion_base: std::string::String,
    dl_companion_dir: std::string::String,
    dl_companion_current: std::string::String,
    dl_companion_queue: std::vec::Vec<std::string::String>,
    dl_companion_seen: std::vec::Vec<std::string::String>,
    dl_companion_done: u32,
    /// URL of the async archive.org fetch in flight (`Screen::DistantLoading`),
    /// pushed to history once the fetch succeeds.
    pending_fetch_url: std::string::String,
    /// The order an archive's games are shown in: (criterion, against its
    /// natural order), as the sort window last applied it. Back to name, A to
    /// Z, whenever a list is fetched, so the window opens on what the page shows
    /// and the page can say how it is sorted.
    files_sort: (u8, bool),
    /// The same for the Flashpoint results.
    fp_sort: (u8, bool),
    /// The last Flashpoint search, for the keyboard's prefill and for A "try
    /// again" on a search that failed. Not `cover_query`: that one is the cover
    /// picker's, and B out of the results used to wipe it.
    fp_query: std::string::String,
    /// The Flashpoint results page shows a search that failed (`cover_msg` says
    /// why) rather than one that found nothing.
    fp_error: bool,
    /// The details window's download size is still to come: `fp_size_slot` is
    /// the thumbnail slot asking for it (-1 when none is running yet), for the
    /// result `fp_size_for`.
    fp_size_pending: bool,
    fp_size_slot: i32,
    fp_size_for: usize,
    /// The details window's blurb has arrived (in `fp_desc`); until then the
    /// thumbnail slot fetching it, -1 when none is running yet.
    fp_desc_ready: bool,
    fp_desc_slot: i32,
    /// The importer row an import started from (A or X on the importer): where
    /// B out of Flashpoint and the error page bring the cursor back, instead of
    /// to the top of the list.
    import_return_row: usize,
    /// The error page: what failed, which titles it, and on what, which is its
    /// breadcrumb. `distant_error` is the message.
    import_error_kind: ErrKind,
    import_error_context: std::string::String,
    /// Where B on the error page goes: the list the failure came from
    /// (`Screen::Inactive` = the importer, on `import_return_row`).
    import_error_back: Screen,
    /// The Flashpoint result the details blurb in flight is for.
    fp_desc_for: usize,
    /// What A on the error page sends again, when trying again can help.
    import_retry: Option<RetryAction>,
    /// A word typed as an address that archive.org did not know: the error page
    /// offers to search Flashpoint for it (X).
    import_fp_suggest: std::string::String,
    /// The text `ImportNotAddress` is about.
    import_typed: std::string::String,
    /// Steps the import page lists beyond the download, once known: the game's
    /// other files, its data files.
    import_had_files: bool,
    import_had_data: bool,
}

static LIBRARY: Mutex<State> = Mutex::new(State {
    dir_path: std::string::String::new(),
    dir_browse: std::vec::Vec::new(),
    pending_games_dir: None,
    screen: Screen::Inactive,
    entries: std::vec::Vec::new(),
    selected_path: None,
    banner_tex: 0,
    banner_w: 0,
    banner_h: 0,
    anim_origin_ticks: 0,
    remote_files: std::vec::Vec::new(),
    download_file_name: std::string::String::new(),
    download_out_path: std::string::String::new(),
    download_source_url: std::string::String::new(),
    download_zip_extract: None,
    download_fp_direct: false,
    download_launch_command: std::string::String::new(),
    download_cover_url: None,
    download_title: None,
    distant_error: std::string::String::new(),
    failed_url: std::string::String::new(),
    downloaded_basenames: std::vec::Vec::new(),
    url_history: std::vec::Vec::new(),
    history_idx: None,
    distant_filter: None,
    import_filter: None,
    fp_desc: std::string::String::new(),
    url_meta: std::vec::Vec::new(),
    meta_loaded: false,
    download_resume_pos: None,
    history_loaded: false,
    applet_mode: false,
    local_filter: None,
    folder_filter: None,
    cover_candidates: std::vec::Vec::new(),
    cover_use_shot: false,
    profile_matches: std::vec::Vec::new(),
    profile_copy_mode: false,
    profile_saved_matches: std::vec::Vec::new(),
    touches_can_revert: false,
    touches_has_backup: false,
    touches_cursor_idx: -1,
    touches_show_cursor: true,
    touches_gyro: keymap::GYRO_OFF,
    preview_rows: std::vec::Vec::new(),
    active_profile_id: std::string::String::new(),
    share_is_update: false,
    toast_msg: std::string::String::new(),
    toast_kind: 0,
    toast_frames: 0,
    cover_msg: std::string::String::new(),
    cover_query: std::string::String::new(),
    fp_loading: false,
    fp_content_filter: true,
    dl_companion_active: false,
    dl_companion_base: std::string::String::new(),
    dl_companion_dir: std::string::String::new(),
    dl_companion_current: std::string::String::new(),
    dl_companion_queue: std::vec::Vec::new(),
    dl_companion_seen: std::vec::Vec::new(),
    dl_companion_done: 0,
    pending_fetch_url: std::string::String::new(),
    files_sort: (0, false),
    fp_sort: (0, false),
    fp_query: std::string::String::new(),
    fp_error: false,
    fp_size_pending: false,
    fp_size_slot: -1,
    fp_size_for: usize::MAX,
    fp_desc_ready: false,
    fp_desc_slot: -1,
    import_return_row: 0,
    import_error_kind: ErrKind::Open,
    import_error_context: std::string::String::new(),
    import_error_back: Screen::Inactive,
    fp_desc_for: usize::MAX,
    import_retry: None,
    import_fp_suggest: std::string::String::new(),
    import_typed: std::string::String::new(),
    import_had_files: false,
    import_had_data: false,
});

/// Where the URL history persists across boots. Format: JSON array of
/// strings, max `HISTORY_MAX` entries (LRU-style — newest at end, oldest
/// dropped when we exceed). Lives in the same dir as `cacert.pem` so the user's
/// `sdmc:/ruffle/` stays SWF-only.
const HISTORY_PATH: &str = "sdmc:/switch/FlashNX/distant_history.json";
/// Runaway guard, NOT a budget. It used to be 20 and silently deleted the
/// oldest URL every time you imported past it. The IMPORTER home now has search
/// (Minus), sort (Y) and a scrolling window, so a long history costs nothing to
/// live with — and losing an import you saved is never an acceptable trade.
const HISTORY_MAX: usize = 500;

/// Per-URL side data: `[url, .swf count, added epoch]`. The count comes from the
/// last successful metadata fetch (so the list can show "4/13 on SD" without
/// re-fetching); the epoch is when the URL was first saved. Kept OUT of
/// `distant_history.json` so that file stays the plain URL list it has always
/// been. A missing/corrupt file just means "unknown", never an error.
const HISTORY_META_PATH: &str = "sdmc:/switch/FlashNX/distant_history_meta.json";

/// Rows of covers visible at once in the JOUER grid. `draw_library_gallery`
/// lays its rows out from this.
pub const GALLERY_ROWS_VISIBLE: usize = 3;

/// Covers per row in the JOUER grid (Main.dc.html: five).
pub const GALLERY_COLS: usize = 5;

/// Rows of the JOUER content band visible at once, for the layout in use. The
/// scroll clamp and the renderer both read this, so they cannot drift apart when
/// the view changes: the grid fits 3 rows of covers, the list 13 rows of text,
/// and the single-row layouts are one row by definition.
pub fn home_rows_visible() -> usize {
    match crate::loc::home_view() {
        1 => 13,
        2 | 3 => 1,
        _ => GALLERY_ROWS_VISIBLE,
    }
}

/// Nav directions held this frame, as a bitmask: 1 = Up, 2 = Down, 4 = Left,
/// 8 = Right. Set by the C++ library loop once per frame, BEFORE it forwards
/// that frame's events.
///
/// It exists for one reason: a stick held diagonally is two directions, and the
/// input layer forwards each one on its own. Where one of the two is a
/// multi-game jump, a single diagonal flick moved five games. An event does not
/// know what else is pressed, so the held state has to come from somewhere.
static NAV_HELD: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

#[no_mangle]
pub extern "C" fn ruffle_library_nav_held(mask: u8) {
    NAV_HELD.store(mask, std::sync::atomic::Ordering::Relaxed);
}

/// True when a vertical AND a horizontal direction are held at once.
fn nav_is_diagonal() -> bool {
    let m = NAV_HELD.load(std::sync::atomic::Ordering::Relaxed);
    (m & 0b0011) != 0 && (m & 0b1100) != 0
}

/// Set by C++ before each forwarded nav event: 0 for a fresh press, 1 for an
/// auto-repeat tick of a held direction.
static NAV_REPEAT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

#[no_mangle]
pub extern "C" fn ruffle_nav_repeat(is_repeat: core::ffi::c_int) {
    NAV_REPEAT.store(is_repeat != 0, std::sync::atomic::Ordering::Relaxed);
}

/// Whether the event being handled is allowed to cross the end of a list.
///
/// A held direction must STOP at the end, and only a new press may come back
/// round to the start. Without this, wrapping and auto-repeat combine into a
/// carousel: hold Down and the selection runs off the bottom, reappears at the
/// top and keeps going, so you cannot use the hold to reach the end of a long
/// library -- the one thing it is for. Stopping is also what says "this is the
/// end", which a list that never stops can never say. The controls editor and
/// its key picker (`menu.rs`) follow it too, in game as in the launcher.
pub(crate) fn nav_may_wrap() -> bool {
    !NAV_REPEAT.load(std::sync::atomic::Ordering::Relaxed)
}

/// Whether the last move came from a held direction's auto-repeat rather than
/// a press. The grid's ring marks a press with a small pop and leaves a held
/// direction alone: armed on every repeat, it pulsed for as long as the
/// direction was held.
pub(crate) fn nav_is_repeat() -> bool {
    NAV_REPEAT.load(std::sync::atomic::Ordering::Relaxed)
}

/// Move `sel` one row up (`down = false`) or down inside a `cols`-wide grid of
/// `total` items, clamping to the last item of a partial row and wrapping at the
/// ends.
///
/// Both grid screens tested `selection + cols < total`, which does the right
/// thing only while every row is full. On a search returning 11 hits over three
/// rows of five, pressing Down from the fourth column of the middle row asked for
/// index 13, the test failed, and the key did NOTHING -- even though the last row
/// was right there with one result in it. The bottom row of an odd-sized result
/// set was reachable from column 0 only. Now the move lands on the nearest item
/// of the target row, and past the last row it comes back to the first, the way
/// every list in the app now does.
pub(crate) fn grid_step(sel: usize, down: bool, cols: usize, total: usize) -> usize {
    if total == 0 || cols == 0 {
        return 0;
    }
    let last = total - 1;
    let col = sel % cols;
    let row = sel / cols;
    let rows = last / cols + 1;
    // A held direction stops on the last row instead of coming round -- see
    // `nav_may_wrap`. Staying on `row` means the step produces no move at all,
    // which is exactly what "stopped at the end" should feel like.
    let target_row = if down {
        if row + 1 < rows {
            row + 1
        } else if nav_may_wrap() {
            0
        } else {
            row
        }
    } else if row > 0 {
        row - 1
    } else if nav_may_wrap() {
        rows - 1
    } else {
        0
    };
    (target_row * cols + col).min(last)
}

/// Move `sel` by `delta` inside `total` rows, stopping at the edge and only
/// crossing over on the NEXT press.
///
/// Shared by every text list -- the home LISTE, the saved URLs and an item's
/// file list -- because they are the same object to a player and were three
/// different behaviours in the code. Two rules rather than one, and the
/// difference only shows with a step of several rows: wrapping on the same press
/// would turn "skip five" near the end of a long list into a jump back near its
/// end, which is not what the button says.
pub(crate) fn list_step_wrap(sel: usize, delta: isize, total: usize) -> usize {
    if total == 0 {
        return 0;
    }
    let last = total - 1;
    // The crossing itself is gated by `nav_may_wrap`: a held direction stops on
    // the edge row, a new press comes round.
    if delta < 0 {
        if sel == 0 {
            if nav_may_wrap() { last } else { 0 }
        } else {
            sel.saturating_sub((-delta) as usize)
        }
    } else if sel >= last {
        if nav_may_wrap() { 0 } else { last }
    } else {
        (sel + delta as usize).min(last)
    }
}

/// Rows skipped by Left/Right in LISTE. Five, not a screenful: a screenful is
/// what a page jump is for, and it would land on a row with none of the titles
/// you were reading still visible.
pub const LIST_SKIP: usize = 5;

/// Games skipped by one page jump.
///
/// BANDE and ETAGERE put every game on row 0, so `gallery_neighbor` — which looks
/// for the nearest cell in the ADJACENT row — never finds anything and Up/Down do
/// nothing at all in them. Crossing a 77-game library then costs 77 presses of
/// Left/Right. Up/Down become a page jump there instead of staying inert.
pub fn home_page_step() -> usize {
    match crate::loc::home_view() {
        1 => 13, // LISTE: one screenful
        // BANDE / ETAGERE: a short hop, not a page. The row shows about five
        // covers, so a big jump loses your place entirely — four keeps one of the
        // covers you were looking at on screen.
        2 | 3 => 4,
        _ => GALLERY_ROWS_VISIBLE * 5, // GRILLE: one screenful of tiles
    }
}

/// Columns in the OPTIONS > JAQUETTE thumbnail picker grid. Shared so the
/// renderer's layout and the input handler's 2D navigation agree.
pub const COVER_PICKER_COLS: usize = 4;

/// Flashpoint result gallery (IMPORTER > X): full-page scrollable cover grid.
/// Shared between `draw_library_fp_gallery` (layout) and `handle_fp_gallery_input`
/// (2D nav + scroll clamp) so they agree on columns / visible rows.
pub const FP_GALLERY_COLS: usize = 5;
pub const FP_GALLERY_ROWS: usize = 3;

/// `visible_rows` on the DISTANT (archive.org) files screen. Larger than
/// LOCAL because typical archive.org dumps run 80-3600+ entries — 10 is
/// the most that fits between the header at y≈150 and footer at y≈680
/// with `ROW_SPACING=50`.
pub const DISTANT_VISIBLE_ROWS: usize = 10;

/// Rows visible at once in the bug report's game list (RÉGLAGES > Aide et
/// retours > Signaler un bug), which opens inside the pane: eight rows with a
/// cover each fit under its head (`settings_game_list`).
pub const BUG_PICKER_VISIBLE_ROWS: usize = 8;

/// Subfolders visible at once in the games folder browser, under its two
/// pinned rows (`settings_dir_list`). The input side scrolls by it and lands
/// on a folder just left with it, so all three read this one number.
pub const GAMES_DIR_VISIBLE: usize = 7;

// ── Setup / FFI helpers ───────────────────────────────────────────────────

extern "C" {
    fn ruffle_tick_now() -> u64;
    fn ruffle_tick_freq() -> u64;
    fn ruffle_log_cstr(msg: *const core::ffi::c_char);
    /// 1 = small applet memory pool (can't launch games — see P1c notice),
    /// 0 = full title-takeover heap. Defined in cpp/src/ruffle_bridge.cpp.
    fn ruffle_is_applet_mode() -> core::ffi::c_int;
    /// 1 = docked, 0 = handheld. Same symbol render.rs reads for the telemetry
    /// line, redeclared here because that extern block is private — the same
    /// per-module duplication `ruffle_tick_now` and `ruffle_log_cstr` already use.
    fn ruffle_is_docked() -> core::ffi::c_int;
    /// Build a HOME-menu tile for one game and install it. 1 on success, 0 on
    /// failure with `err` filled in (see cpp/src/nsp.cpp).
    ///
    /// Not cheap, unlike the `.nro` shortcut this replaced: it builds three NCAs
    /// and writes them through `ncm`, so it takes a moment and it talks to
    /// services. It still fits in one press because nothing here waits on the
    /// network and the whole payload is under a megabyte.
    fn nsp_install_tile(
        program_id: u64,
        title: *const core::ffi::c_char,
        swf_path: *const core::ffi::c_char,
        icon_jpeg: *const u8,
        icon_len: u32,
        err: *mut core::ffi::c_char,
        err_cap: u32,
    ) -> core::ffi::c_int;
    /// Write a file to the card, creating parent directories. Used here for a
    /// tile's config file, and the same writer every other file goes through:
    /// Rust's `std::fs::write` reports success on Horizon and the file then
    /// reads back ENOENT on some writes.
    fn swf_picker_write_file(
        path: *const core::ffi::c_char,
        data: *const u8,
        len: u32,
    ) -> core::ffi::c_int;
    /// Present what has just been drawn. Normally the C++ frame loop owns this,
    /// and nothing in Rust needs it; the GameZIP extraction does, because it
    /// blocks that loop for minutes and has to put its own frames on screen
    /// while it runs (#117). Defined in cpp/src/gl_context.cpp.
    fn gl_context_swap();
}

fn log(s: &str) {
    let mut bytes = s.as_bytes().to_vec();
    bytes.push(0);
    unsafe { ruffle_log_cstr(bytes.as_ptr() as *const _) };
}

extern "C" {
    // C++/libnx: flat file size, and recursive `<stem>.files/` tree size (0 if
    // absent). Horizon-safe (opendir/readdir/stat). The tree walk is called ONLY
    // at download time — never on the scan (per-scan it added ~10s for SSF2).
    fn swf_picker_file_size(path: *const core::ffi::c_char) -> i64;
    fn swf_picker_files_dir_size(swf_path: *const core::ffi::c_char) -> i64;
}

/// Flat on-SD byte length of `path`, via C++/libnx `stat` (Rust's
/// `fs::metadata().len()` returns garbage on Horizon — see `read_swf_header`).
/// 0 when the file is missing or unreadable.
pub(crate) fn file_size(path: &str) -> u64 {
    let Ok(c) = std::ffi::CString::new(path) else {
        return 0;
    };
    unsafe { swf_picker_file_size(c.as_ptr()) }.max(0) as u64
}

/// Read the cached real on-SD footprint (flat SWF + `<stem>.files/` tree) from
/// `<swf_path>.filesize`, written once at download. Direct bounded read (one
/// open, fails fast if absent) so the scan stays cheap. None -> caller keeps the
/// header size (single-file games + multi-file games from before this feature).
fn read_filesize_cache(swf_path: &str) -> Option<u64> {
    let p = std::format!("{}.filesize", swf_path);
    if !file_exists(&p) {
        return None; // no open() at all for the common single-file game
    }
    read_sd_text(&p, 64).ok()?.trim().parse::<u64>().ok().filter(|&n| n > 0)
}

/// Compute a game's real on-SD footprint (flat SWF disk size + companion tree)
/// via C++/libnx and cache it in `<swf_path>.filesize`. Called once at the end
/// of a download so the footer shows GBs, not a 2 MB loader. NOT on the scan.
fn cache_game_footprint(swf_path: &str) {
    let Ok(c) = std::ffi::CString::new(swf_path) else {
        return;
    };
    let tree = unsafe { swf_picker_files_dir_size(c.as_ptr()) }.max(0) as u64;
    // Only multi-file games get a cached footprint: a single-file game has no
    // companion tree, so its SWF-header size (shown by default) is already right.
    if tree == 0 {
        return;
    }
    let flat = unsafe { swf_picker_file_size(c.as_ptr()) }.max(0) as u64;
    let p = std::format!("{}.filesize", swf_path);
    if std::fs::write(&p, (flat + tree).to_string().as_bytes()).is_ok() {
        // Written AFTER the scan listed this directory: without this the index
        // would report the fresh sidecar as absent and the game we just
        // downloaded would show its header size instead of its real footprint.
        note_file_created(&p);
        crate::sd::commit();
    }
}

/// One-time migration (marker-guarded): compute + cache the real footprint of
/// EVERY already-installed multi-file game, so their footer size is correct
/// without a re-download. This walks the `.files/` trees ONCE (blocking, a few
/// seconds — dominated by Super Smash Flash 2's 1474 files); afterwards the scan
/// only reads the cached `<swf>.filesize`. New downloads are cached at download
/// time, so they're skipped here (already have the sidecar).
fn backfill_game_sizes() {
    let marker = primary_user_path(".filesize_backfill1.done");
    if crate::sources::gamezip::read_file_bounded(&marker, 4).is_some() {
        return; // already backfilled on a previous boot
    }
    let paths: std::vec::Vec<std::string::String> = match LIBRARY.lock() {
        Ok(g) => g.entries.iter().map(|e| e.path.clone()).collect(),
        Err(_) => return,
    };
    let mut done = 0usize;
    for swf_path in &paths {
        if read_filesize_cache(swf_path).is_some() {
            continue; // already cached (new download, or a prior partial run)
        }
        cache_game_footprint(swf_path); // walks + writes .filesize for multi-file games
        if let Some(sz) = read_filesize_cache(swf_path) {
            if let Ok(mut g) = LIBRARY.lock() {
                if let Some(e) = g.entries.iter_mut().find(|e| e.path == *swf_path) {
                    // Reflect it now, without waiting for a re-scan.
                    e.size_bytes = sz;
                    e.card_bytes = sz;
                    e.multi = true;
                }
            }
            done += 1;
        }
    }
    log(&std::format!("filesize: backfilled {} multi-file game footprints\n", done));
    let _ = std::fs::write(&marker, b"1");
    crate::sd::commit();
}

/// Every path seen by the SD scan's `readdir` (games AND sidecars), so a
/// sidecar's existence is a memory lookup instead of an SD probe. Filled by
/// `ruffle_library_note_file` before any `add_path` for that directory.
///
/// Only the SCAN populates this, so it answers for the scanned roots and
/// nothing else: `scan_knows_dir` gates every use, and any path under a
/// directory the scan never listed falls back to touching the SD. Sidecars
/// written later (a download, a rename) go through `note_file_created` so the
/// index doesn't go stale within a session.
static SCAN_FILES: Mutex<std::collections::BTreeSet<std::string::String>> =
    Mutex::new(std::collections::BTreeSet::new());

/// Directories `readdir` actually listed. A path outside them is unknown to the
/// index, not absent.
static SCAN_DIRS: Mutex<std::vec::Vec<std::string::String>> = Mutex::new(std::vec::Vec::new());

fn parent_dir(path: &str) -> &str {
    match path.rfind('/') {
        Some(i) => &path[..i],
        None => "",
    }
}

/// Record one path from the scan's directory listing.
pub fn note_file(path: &str) {
    if let Ok(mut set) = SCAN_FILES.lock() {
        set.insert(path.to_string());
    }
}

/// Mark a directory as fully enumerated (listed, or confirmed absent), which is
/// what lets `file_exists` answer "no" without touching the SD.
pub fn note_dir(dir: &str) {
    let dir = dir.trim_end_matches('/').to_string();
    if let Ok(mut dirs) = SCAN_DIRS.lock() {
        if !dirs.iter().any(|d| *d == dir) {
            dirs.push(dir);
        }
    }
}

/// Keep the index truthful when we CREATE a file in a scanned directory after
/// the scan (cover download, `.filesize` cache, rename sidecar).
pub fn note_file_created(path: &str) {
    if let Ok(mut set) = SCAN_FILES.lock() {
        if !set.is_empty() {
            set.insert(path.to_string());
        }
    }
}

/// Same, for a file we DELETE after the scan (cover removal, game delete).
pub fn note_file_removed(path: &str) {
    if let Ok(mut set) = SCAN_FILES.lock() {
        set.remove(path);
    }
}

/// Did the scan list the directory holding `path`? False → the index can't
/// answer for it and the caller must hit the SD.
fn scan_knows_dir(path: &str) -> bool {
    let dir = parent_dir(path);
    SCAN_DIRS
        .lock()
        .map(|d| d.iter().any(|x| x == dir))
        .unwrap_or(false)
}

/// Existence check that avoids the SD when the scan already listed that
/// directory. Same answer as `Path::exists()`, ~0 cost for the common case of a
/// sidecar that isn't there.
pub(crate) fn file_exists(path: &str) -> bool {
    if scan_knows_dir(path) {
        return SCAN_FILES
            .lock()
            .map(|s| s.contains(path))
            .unwrap_or(false);
    }
    std::path::Path::new(path).exists()
}


extern "C" {
    fn swf_picker_list_dirs(dir: *const core::ffi::c_char, out: *mut u8, cap: i32) -> i32;
    /// Directory existence via libnx: `std::fs::metadata` gets this wrong on
    /// Horizon and reports live folders as absent.
    fn swf_picker_dir_exists(path: *const core::ffi::c_char) -> i32;
}

/// Does this file exist ON THE CARD, right now?
///
/// `file_exists` answers from the scan index when it knows the directory, which
/// is right for a sidecar probe and wrong the moment files are being moved: the
/// index still describes where things were. A whole folder move once reported
/// "0 moved, 419 already there" and did nothing at all, because every
/// destination looked occupied to a cache that had not caught up.
pub(crate) fn file_exists_now(path: &str) -> bool {
    let c = std::format!("{}\0", path);
    unsafe { swf_picker_file_size(c.as_ptr() as *const core::ffi::c_char) >= 0 }
}

/// Subfolder names of `dir`, alphabetical. Empty when the folder cannot be read.
///
/// Goes through C++ because Rust's `read_dir` corrupts entry names on Horizon —
/// the same reason the library scan lives there. One level only: a folder picker
/// never shows more, and a full card holds tens of thousands of entries.
pub(crate) fn list_dirs(dir: &str) -> std::vec::Vec<std::string::String> {
    let mut buf = std::vec![0u8; 64 * 1024];
    let c = std::format!("{}\0", dir);
    let n = unsafe {
        swf_picker_list_dirs(c.as_ptr() as *const core::ffi::c_char, buf.as_mut_ptr(), buf.len() as i32)
    };
    if n <= 0 {
        return std::vec::Vec::new();
    }
    let end = buf.iter().position(|b| *b == 0).unwrap_or(buf.len());
    std::string::String::from_utf8_lossy(&buf[..end])
        .lines()
        .filter(|l| !l.is_empty() && !is_our_bookkeeping(l))
        .map(|l| l.to_string())
        .collect()
}

/// Every subfolder of `dir`, including the `<game>.files/` trees the picker
/// hides. The move needs the real contents, not the tidied view.
pub(crate) fn list_dirs_all(dir: &str) -> std::vec::Vec<std::string::String> {
    let mut buf = std::vec![0u8; 64 * 1024];
    let c = std::format!("{}\0", dir);
    let n = unsafe {
        swf_picker_list_dirs(c.as_ptr() as *const core::ffi::c_char, buf.as_mut_ptr(), buf.len() as i32)
    };
    if n <= 0 {
        return std::vec::Vec::new();
    }
    let end = buf.iter().position(|b| *b == 0).unwrap_or(buf.len());
    std::string::String::from_utf8_lossy(&buf[..end])
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| l.to_string())
        .collect()
}

/// Folders FlashNX manages itself, which are never a place to put games: the
/// `<game>.files/` companion trees of multi-file titles, and the cover cache.
/// Listing them buries the card's real folders under one row per installed
/// game, which is exactly what the picker is trying to help with.
fn is_our_bookkeeping(name: &str) -> bool {
    name.eq_ignore_ascii_case("covers") || name.to_ascii_lowercase().ends_with(".files")
}

/// The folder above `path`, or None at the top of the card.
///
/// Distinct from `parent_dir` above, which is a plain string trim used by the
/// scan index. `sdmc:/foo` sits one level under the root, and its parent is
/// `sdmc:/`, not `sdmc:` — an SD path keeps the slash. Getting this wrong made
/// the root unreachable: the picker decided there was no parent and closed.
fn browse_parent(path: &str) -> Option<std::string::String> {
    let trimmed = path.trim_end_matches('/');
    if trimmed == "sdmc:" {
        return None;
    }
    match trimmed.rfind('/') {
        Some(i) if i > "sdmc:".len() => Some(trimmed[..i].to_string()),
        Some(_) => Some("sdmc:/".to_string()),
        None => None,
    }
}

/// True when `path` exists AND is a directory.
///
/// Goes through C++ for the same reason the scan does: Rust's `std::fs` is not
/// reliable on Horizon. `metadata()` reported the freshly created
/// `roms/flashnx` as absent, so the games folder fell back to the default on
/// every read and the user's choice never took effect (#79).
pub(crate) fn dir_exists(path: &str) -> bool {
    let c = std::format!("{}\0", path);
    unsafe { swf_picker_dir_exists(c.as_ptr() as *const core::ffi::c_char) != 0 }
}

/// Read a short single-line config file (a path, a number). None when absent or
/// unreadable — every caller has a default, and a missing file is the normal
/// case rather than an error worth reporting.
pub(crate) fn read_small_file(path: &str) -> Option<std::string::String> {
    std::fs::read_to_string(path).ok()
}
// ── SWF header index ──────────────────────────────────────────────────────
//
// The scan opened every game to read its SWF header: an open, a read and a zlib
// inflate each, 293 ms for 71 games. That work is identical on every scan, and
// the scan runs on each return to the library, not just at boot.
//
// So the parsed header is cached in one file, keyed by the game's mtime.
//
// DELIBERATELY LIMITED TO THE HEADER. Everything cached here is read out of the
// `.swf` itself, so a game cannot change without its mtime changing, and the
// mtime check alone makes a stale row impossible. Caching the sidecars too
// (`.url`, `.meta.json`, `.filesize`) would save another 263 ms but those CAN
// change while the `.swf` sits untouched, which would mean invalidating from
// every writer and trusting that we found them all. Not worth a wrong name in
// the list; the sidecars are still read on every scan.
//
// The file is a pure CACHE. Missing, corrupt or unparseable, it is ignored and
// everything is read off the SD, so it needs no migration and no versioning
// beyond a rename.

/// Named for what it holds. A previous shape of this cache also mirrored the
/// sidecars under another name; since the file is disposable, a rename is the
/// whole migration story.
const SCAN_INDEX_NAME: &str = ".swf_header_index.json";

#[derive(serde::Serialize, serde::Deserialize)]
struct IndexEntry {
    path: std::string::String,
    mtime: u64,
    /// `file_length` from the SWF header (NOT the `.filesize` sidecar, which is
    /// a different number and is still read every scan).
    size: u64,
    ver: u8,
    /// "FWS" / "CWS" / "ZWS" — mapped back to the `&'static str` the UI holds.
    comp: std::string::String,
    as3: bool,
    /// The `.swf`'s own size on the card, as stored. Read out of the same file,
    /// so the mtime check covers it as well. 0 in an index written before it
    /// existed: the game is asked once, and the index rewritten with it.
    #[serde(default)]
    disk: u64,
}

static SCAN_INDEX: Mutex<std::vec::Vec<IndexEntry>> = Mutex::new(std::vec::Vec::new());
/// Rows collected by the scan in progress (hits and misses alike), which is
/// what gets written back. Kept apart from `Entry` because the field the index
/// stores is the SWF header's own length, while `Entry::size_bytes` may have
/// been replaced by the `.filesize` sidecar's footprint.
static SCAN_HEADERS: Mutex<std::vec::Vec<IndexEntry>> = Mutex::new(std::vec::Vec::new());
/// Set when any game missed the index, so `scan_end` knows to rewrite it.
static SCAN_INDEX_DIRTY: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);
/// How many rows the index held at `scan_begin`, so a game removed since then
/// (its row is now dead weight) also triggers a rewrite.
static SCAN_INDEX_ROWS: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);

fn static_compression_label(s: &str) -> Option<&'static str> {
    match s {
        "FWS" => Some("FWS"),
        "CWS" => Some("CWS"),
        "ZWS" => Some("ZWS"),
        _ => None,
    }
}

/// Load the cached headers. Called once per scan, before any `add_path`.
pub fn scan_begin() {
    SCAN_INDEX_DIRTY.store(false, core::sync::atomic::Ordering::Relaxed);
    let parsed = read_sd_text(&config_read_path(SCAN_INDEX_NAME), 1024 * 1024)
        .ok()
        .and_then(|txt| serde_json::from_str::<std::vec::Vec<IndexEntry>>(&txt).ok())
        .unwrap_or_default();
    SCAN_INDEX_ROWS.store(parsed.len(), core::sync::atomic::Ordering::Relaxed);
    if let Ok(mut idx) = SCAN_INDEX.lock() {
        *idx = parsed;
    }
    if let Ok(mut rows) = SCAN_HEADERS.lock() {
        rows.clear();
    }
}

/// Record one game's header for the write-back at `scan_end`.
#[allow(clippy::too_many_arguments)]
fn scan_note_header(path: &str, mtime: u64, size: u64, ver: u8, comp: &str, as3: bool, disk: u64) {
    if let Ok(mut rows) = SCAN_HEADERS.lock() {
        rows.push(IndexEntry {
            path: path.to_string(),
            mtime,
            size,
            ver,
            comp: comp.to_string(),
            as3,
            disk,
        });
    }
}

/// Rewrite the cache from what the scan just produced, but only if a game
/// missed it or one has gone — a steady library writes nothing here.
pub fn scan_end() {
    let missed = SCAN_INDEX_DIRTY.swap(false, core::sync::atomic::Ordering::Relaxed);
    let rows: std::vec::Vec<IndexEntry> = match SCAN_HEADERS.lock() {
        Ok(mut r) => core::mem::take(&mut *r),
        Err(_) => return,
    };
    // A game removed since the last scan leaves a dead row behind, so the row
    // count disagreeing is also a reason to rewrite.
    let stale_rows = SCAN_INDEX_ROWS.load(core::sync::atomic::Ordering::Relaxed) != rows.len();
    if let Ok(mut idx) = SCAN_INDEX.lock() {
        idx.clear(); // done with it until the next scan
    }
    if !missed && !stale_rows {
        return;
    }
    if let Ok(json) = serde_json::to_string(&rows) {
        let index_path = config_write_path(SCAN_INDEX_NAME);
        if std::fs::write(&index_path, json.as_bytes()).is_ok() {
            note_file_created(&index_path);
            crate::sd::commit();
            log(&std::format!("library: swf header index written ({} games)\n", rows.len()));
        }
    }
}

/// Cached SWF header for `path`, and the file's size on the card (0 when the
/// row predates it), if the index has it at exactly this mtime.
fn index_lookup(path: &str, mtime: u64) -> Option<(u64, u8, &'static str, bool, u64)> {
    // A game whose mtime is unknown (0) can't be validated — don't trust it.
    if mtime == 0 {
        return None;
    }
    let idx = SCAN_INDEX.lock().ok()?;
    let e = idx.iter().find(|e| e.path == path && e.mtime == mtime)?;
    let comp = static_compression_label(&e.comp)?;
    Some((e.size, e.ver, comp, e.as3, e.disk))
}

/// Per-part cost of the SD scan, in system ticks. The scan runs before the
/// first frame, so every millisecond here is black screen — this attributes it
/// to the actual file access instead of us guessing. Dumped once by `open()`.
static SCAN_TICKS: [core::sync::atomic::AtomicU64; 5] = [
    core::sync::atomic::AtomicU64::new(0),
    core::sync::atomic::AtomicU64::new(0),
    core::sync::atomic::AtomicU64::new(0),
    core::sync::atomic::AtomicU64::new(0),
    core::sync::atomic::AtomicU64::new(0),
];

/// Time `f`, banking the elapsed ticks into `SCAN_TICKS[slot]`.
fn scan_timed<T>(slot: usize, f: impl FnOnce() -> T) -> T {
    let t0 = unsafe { ruffle_tick_now() };
    let out = f();
    let dt = unsafe { ruffle_tick_now() }.saturating_sub(t0);
    SCAN_TICKS[slot].fetch_add(dt, core::sync::atomic::Ordering::Relaxed);
    out
}

/// Log the scan's per-part breakdown (SWF header / sidecars / log calls).
fn log_scan_breakdown(entries: usize) {
    let freq = unsafe { ruffle_tick_freq() } as f64;
    let ms = |i: usize| {
        (SCAN_TICKS[i].load(core::sync::atomic::Ordering::Relaxed) as f64) * 1000.0 / freq
    };
    log(&std::format!(
        "boot: scan {} games — swf header {:.0} ms | .filesize {:.0} ms | .meta.json {:.0} ms \
         | log {:.0} ms\n",
        entries, ms(0), ms(1), ms(2), ms(4),
    ));
}

/// The shelf a game belongs to: its FIRST path segment below whichever search
/// root contains it, or empty when it sits at the root (issue #68).
///
/// First segment only, deliberately. The scan walks three levels so nothing
/// disappears, but `flashnx/Mario/NES/smb.swf` still belongs to MARIO: a player
/// who made one folder thinks of one shelf, and a chip strip that grew a level
/// every time someone nested a folder would be unusable.
///
/// Matched against the search roots rather than parsed, because the games folder
/// is configurable (issue #79) and the legacy roots are still live: a path is
/// only "in a folder" relative to the root it was found under.
fn folder_of(path: &str) -> std::string::String {
    let norm = path.replace('\\', "/");
    for root in search_roots() {
        let root = root.trim_end_matches('/');
        let Some(rest) = norm.strip_prefix(root) else {
            continue;
        };
        // The remainder MUST start at a separator. Without this test a root of
        // `sdmc:/flashnx` would swallow `sdmc:/flashnxOLD/game.swf` and report
        // its folder as "OLD" -- a shelf named after half a directory name.
        if !rest.starts_with('/') {
            continue;
        }
        let rest = rest.trim_start_matches('/');
        return match rest.split_once('/') {
            // A slash left means at least one folder before the file name.
            Some((first, _)) => first.to_string(),
            None => std::string::String::new(),
        };
    }
    std::string::String::new()
}

/// Where a game ended up after a move attempt.
enum MoveOutcome {
    /// It travelled. This is its new path.
    Moved(std::string::String),
    /// Nothing moved, or everything that moved was put back. The game is exactly
    /// where it was and the caller must not touch its recorded path.
    ///
    /// `name_taken` says WHY, because the two reasons want different words. A
    /// file of that name already at the destination is a diagnosis -- another
    /// game owns that name there -- and telling the player the card refused to
    /// write is both false and useless. Anything else really is a write that
    /// failed.
    Refused { name_taken: bool },
    /// The move broke part way AND the undo broke too. The `.swf` is at this
    /// path now; the caller must record it even while reporting the failure,
    /// because an entry that names a file which is not there is worse than one
    /// that names a game in the wrong folder.
    Stranded(std::string::String),
}

/// Move ONE game onto a shelf: its `.swf` and, if it has one, its `.files/`
/// companion tree (issue #68). `folder` empty = back to the root.
///
/// Companions come in TWO shapes and only the second travels.
///
/// Keyed by FILE NAME, at the root of the games folder, left where they are:
/// saves and `.solmap` (backend/storage.rs), `.keymap.json` / `.prefs` /
/// `.cursor` (keymap.rs), `.meta.json`, the cover cache, playtime, favourites.
/// Moving the `.swf` leaves every one of them answering exactly as before.
///
/// Resolved from the `.swf`'s OWN PATH, so they have to travel with it:
///  - `<stem>.files/`, the companion asset tree (lib.rs `sidecar_dir`)
///  - `<path>.base`, the original launchCommand URL. Losing it is not cosmetic:
///    lib.rs falls back to a flat synthetic base, which switches off the htdocs
///    repair in the navigator AND hides the container HTML, so a Flashpoint game
///    sits on its loader for ever (Super Brawl 2, 2026-06-14).
///  - `<path>.url` (import source) and `<path>.filesize` (real footprint, whose
///    backfill is marker-guarded and would never recompute).
///
/// Refuses rather than overwrite: a name already taken in the destination means
/// two different games, and silently merging them would cost the player one of
/// the two.
///
/// Every outcome says where the `.swf` IS, because the caller stores that path
/// and `delete_game` later derives from it the directory whose `<basename>.*`
/// it sweeps. A rollback that fails silently is how an entry comes to name a
/// path with no file on it, and how SUPPRIMER then deletes a game's save while
/// leaving the game.
/// `move_game_inner`, and the game's HOME icon follows it: the icon's config
/// names the game by its full path, which the move just changed.
fn move_game_to_folder(path: &str, basename: &str, folder: &str) -> MoveOutcome {
    let outcome = move_game_inner(path, basename, folder);
    if let MoveOutcome::Moved(ref np) | MoveOutcome::Stranded(ref np) = outcome {
        if np != path {
            tiles_follow(path, np, false);
        }
    }
    outcome
}

fn move_game_inner(path: &str, basename: &str, folder: &str) -> MoveOutcome {
    let root = primary_root();
    let dest_dir = if folder.is_empty() {
        root.clone()
    } else {
        std::format!("{}/{}", root, folder)
    };
    let dest = std::format!("{}/{}", dest_dir, basename);
    if dest == path {
        return MoveOutcome::Moved(dest); // already there
    }
    // A shelf is a NAME, and `folder_of` recognises that name under any search
    // root, so one shelf on screen can span two roots. Checking only the primary
    // one let a game land on a shelf that already holds a namesake under
    // `sdmc:/ruffle`: two rows with one file name, sharing one save file, one
    // keymap and one cover. Ask every root the shelf could be living under.
    for r in search_roots() {
        let r = r.trim_end_matches('/');
        let twin = if folder.is_empty() {
            std::format!("{}/{}", r, basename)
        } else {
            std::format!("{}/{}/{}", r, folder, basename)
        };
        if twin != path && file_exists_now(&twin) {
            log(&std::format!("library: {} already exists, move refused\n", twin));
            return MoveOutcome::Refused { name_taken: true };
        }
    }
    if !folder.is_empty() && !dir_exists(&dest_dir) {
        if std::fs::create_dir_all(&dest_dir).is_err() {
            log(&std::format!("library: could not create {}\n", dest_dir));
            return MoveOutcome::Refused { name_taken: false };
        }
    }
    if file_exists_now(&dest) {
        log(&std::format!("library: {} already exists, move refused\n", dest));
        return MoveOutcome::Refused { name_taken: true };
    }
    if std::fs::rename(path, &dest).is_err() {
        log(&std::format!("library: move {} -> {} failed\n", path, dest));
        return MoveOutcome::Refused { name_taken: false };
    }
    // The companion tree, when the game has one. Its absence is the normal case
    // (single-file games), so a missing source is not a failure.
    //
    // The extension is stripped CASE-INSENSITIVELY: the scan accepts `.SWF`
    // (`ends_with_swf` uses strcasecmp) and lib.rs builds the tree name from
    // `file_stem`, so a game named `.SWF` really does have a `<stem>.files` and a
    // case-sensitive strip would have left it behind.
    // `len() > 4`, not `>= 4`: a file named exactly `.swf` is a leading-dot name
    // with no extension, so `Path::file_stem` — which is what lib.rs builds the
    // tree name from — returns `.swf` whole. Stripping it here would look for
    // `.files` and never find `.swf.files`.
    // `is_char_boundary` for the same reason the rename path checks it (#75,
    // Chinese titles): slicing a `&str` off a byte boundary is a panic, and
    // `panic = "abort"` makes that a console fatal. Today the invariant holds --
    // every entry comes from a name the C++ scan accepted as ending in `.swf` --
    // but that invariant lives in another language, in another file.
    let cut = |s: &str| -> std::string::String {
        let at = s.len().saturating_sub(4);
        if s.len() > 4 && s.is_char_boundary(at) && s[at..].eq_ignore_ascii_case(".swf") {
            s[..at].to_string()
        } else {
            s.to_string()
        }
    };
    let stem = cut(basename);
    let src_files = std::format!("{}.files", cut(path));
    // Remembered for the `.base` rollback below, which has to undo this too --
    // but ONLY if this move is what put it there. A tree already sitting in the
    // destination would otherwise be dragged out of it by an unrelated failure.
    let moved_files_dest = std::format!("{}/{}.files", dest_dir, stem);
    let mut files_travelled = false;
    if dir_exists(&src_files) {
        if std::fs::rename(&src_files, &moved_files_dest).is_err() {
            // The `.swf` is already across. Put it back rather than leave a game
            // separated from the assets it needs to run.
            //
            // And if putting it back ALSO fails, say where it really is. Throwing
            // that result away left the entry pointing at a path with no file on
            // it, and `delete_game` derives the directory to sweep from that
            // path: SUPPRIMER then took the saves, the controls and the cover
            // while the `.swf` itself, being elsewhere, survived and came back at
            // the next scan with nothing in it.
            if std::fs::rename(&dest, path).is_err() {
                log(&std::format!(
                    "library: {} could not be put back; it is at {}\n",
                    path, dest
                ));
                return MoveOutcome::Stranded(dest);
            }
            log(&std::format!("library: move of {} failed, game put back\n", src_files));
            return MoveOutcome::Refused { name_taken: false };
        }
        files_travelled = true;
    }
    // The three path-keyed sidecars, in two tiers.
    //
    // `.base` is NOT best-effort. Losing it is the difference between a
    // Flashpoint game running and sitting on its loader for ever, so a failure
    // there puts everything back and the caller reports it. `.url` and
    // `.filesize` are metadata: an import source and a footprint. Losing one is
    // worth a log line, not a rollback that could itself half-fail.
    let base_src = std::format!("{}.base", path);
    if file_exists_now(&base_src) {
        let base_dst = std::format!("{}.base", dest);
        if std::fs::rename(&base_src, &base_dst).is_ok() {
            note_file_removed(&base_src);
            note_file_created(&base_dst);
        } else {
            // The asset tree comes back FIRST, and the `.swf` only follows it if
            // it made the trip. A game and its `<stem>.files/` have to end up in
            // the same place: `lib.rs` derives the tree's path from the `.swf`'s,
            // so a `.swf` back at its old path with its assets stuck at the
            // destination is a game that starts and never loads anything.
            // Leaving both across is the lesser of the two broken states.
            if files_travelled && std::fs::rename(&moved_files_dest, &src_files).is_err() {
                log(&std::format!(
                    "library: {} could not be put back; the game stays at {}\n",
                    moved_files_dest, dest
                ));
                return MoveOutcome::Stranded(dest);
            }
            if std::fs::rename(&dest, path).is_err() {
                log(&std::format!(
                    "library: {} could not be put back; it is at {}\n",
                    path, dest
                ));
                return MoveOutcome::Stranded(dest);
            }
            log(&std::format!("library: {} not moved, game put back\n", base_src));
            return MoveOutcome::Refused { name_taken: false };
        }
    }
    for suffix in [".url", ".filesize", ".arena"] {
        let src = std::format!("{}{}", path, suffix);
        if !file_exists_now(&src) {
            continue;
        }
        let dst = std::format!("{}{}", dest, suffix);
        if std::fs::rename(&src, &dst).is_ok() {
            note_file_removed(&src);
            note_file_created(&dst);
        } else {
            log(&std::format!("library: sidecar {} not moved\n", src));
        }
    }
    crate::sd::commit();
    log(&std::format!("library: moved {} -> {}\n", path, dest));
    MoveOutcome::Moved(dest)
}

pub fn add_path(path: &str, mtime: u64) -> bool {
    let basename = path
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(path)
        .to_string();
    // The SWF header is the one part of a scan that can be cached safely: it is
    // read out of the `.swf`, so an unchanged mtime guarantees it is current.
    // The sidecars below are read every scan (they change on their own).
    let (header_size, swf_version, compression_label, is_as3, mut disk) =
        match index_lookup(path, mtime) {
            Some(hit) => hit,
            None => {
                SCAN_INDEX_DIRTY.store(true, core::sync::atomic::Ordering::Relaxed);
                match scan_timed(0, || read_swf_header(path)) {
                    Some(h) => (h.size_bytes, h.version, h.compression_label, h.is_as3, 0),
                    None => {
                        log(&std::format!(
                            "library: failed to parse SWF header for {}, skipping\n",
                            path,
                        ));
                        return false;
                    }
                }
            }
        };
    // The file's own size on the card (a stat), read once and then kept in the
    // index with its header; a row written before the index had it is asked
    // once, and the index rewritten.
    // Only a size actually read is worth a rewrite: a file the stat cannot
    // answer for would otherwise rewrite the index on every scan.
    if disk == 0 {
        disk = scan_timed(0, || file_size(path));
        if disk > 0 {
            SCAN_INDEX_DIRTY.store(true, core::sync::atomic::Ordering::Relaxed);
        }
    }
    scan_note_header(path, mtime, header_size, swf_version, compression_label, is_as3, disk);
    // Prefer the cached real footprint (multi-file games); fall back to the SWF
    // header's declared size for single-file / pre-feature games. What the
    // player is shown falls back to the file's size on the card instead.
    let footprint = scan_timed(1, || read_filesize_cache(path));
    let size_bytes = footprint.unwrap_or(header_size);
    let card_bytes = footprint.unwrap_or(if disk > 0 { disk } else { header_size });
    // A companion folder is a directory the scan listed, so this is a memory
    // lookup; a game downloaded since has its `.filesize`, written with it.
    let multi = footprint.is_some() || file_exists(&crate::sidecar_dir_for(Some(path)).to_string_lossy());
    let color_chip = color_from_basename(&basename);
    // Honour a per-game display-name override from the .meta.json sidecar
    // (Phase 3.4.bis RENOMMER). Sidecar absent / unparseable / empty
    // display_name → fall back to basename.
    let display_name = scan_timed(2, || read_meta_sidecar(&basename))
        .and_then(|m| m.display_name)
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| pretty_name(&basename));
    // Which import source this game came from (its `.url` sidecar). Read here,
    // alongside the other sidecars, so the IMPORTER list can count an item's
    // installed files in memory instead of hitting the SD every frame.
    let folder = folder_of(path);
    // Snapshotted here, once per scan, for the same reason every other per-game
    // fact is: the gallery asks about shelf membership on every draw.
    let tags = crate::tags::tags_for(&basename);
    let entry = Entry {
        path: path.to_string(),
        display_name,
        basename,
        folder,
        tags,
        size_bytes,
        card_bytes,
        multi,
        swf_version,
        compression_label,
        is_as3,
        color_chip,
        mtime,
    };
    scan_timed(4, || {
        log(&std::format!(
            "library: added {} (SWF v{} {}, {})\n",
            path, swf_version, compression_label,
            if is_as3 { "AS3/AVM2" } else { "AS2/AVM1" },
        ))
    });
    if let Ok(mut s) = LIBRARY.lock() {
        // Two games can now share a file name, one per folder (issue #68), and
        // every companion file is keyed by that name alone: they would share
        // their saves, their controls and their cover without a word.
        //
        // Both are still listed -- a game vanishing is what this whole change
        // exists to stop -- but the collision is named in the log, because it is
        // the one thing folders make possible that nobody would expect.
        // Same PATH already listed = the same file reached by two scan roots
        // that overlap (a chosen games folder sitting inside a built-in one, now
        // that the scan recurses). Listing it twice would show the player two of
        // every game and make the second one's index a lie.
        //
        // REPLACED, not skipped: `add_or_replace_path` re-adds a game whose file
        // just changed under it (a re-download) and relies on the newest push
        // winning. Dropping the new entry here would leave the gallery showing
        // the size, date and engine of the file that is no longer there.
        if let Some(old) = s.entries.iter().position(|e| e.path == entry.path) {
            s.entries[old] = entry;
            return true;
        }
        if let Some(other) = s.entries.iter().find(|e| e.basename == entry.basename) {
            log(&std::format!(
                "library: WARNING two games named {} ({} and {}); they share saves, keys and shelf labels\n",
                entry.basename, other.path, entry.path,
            ));
        }
        s.entries.push(entry);
    }
    true
}

/// Transition from Inactive → List (or Empty if `entries` is empty). Called
/// after C++ has finished scanning and the GL renderer is up.
/// One-time cleanup (guarded by a marker file): remove the redundant copy of
/// each game's ENTRY SWF from its `.files/` tree. The entry SWF is stored flat
/// as `<game>.swf` and the SidecarNavigator serves the entry URL from that flat
/// file, so the identical tree copy is dead weight — up to ~30 MB for a
/// single-SWF GameZIP (e.g. Infiltrating the Airship). New downloads never write
/// it (see `gamezip::extract_gamezip_tree`); this reclaims it on games already
/// on the SD. `<game>.swf.base` gives the entry URL; non-zipped / single-file
/// games have no host-pathed tree copy, so the computed path is absent and the
/// delete is a harmless no-op.
fn cleanup_duplicate_entries() {
    // v2 marker: re-runs once on installs cleaned by v1 to also prune the empty
    // dir chains the first pass left behind.
    let marker = primary_user_path(".entry_dedup2.done");
    if crate::sources::gamezip::read_file_bounded(&marker, 4).is_some() {
        return; // already cleaned on a previous boot
    }
    // Snapshot game paths under the lock; do the fs work without holding it.
    let paths: std::vec::Vec<std::string::String> = match LIBRARY.lock() {
        Ok(g) => g.entries.iter().map(|e| e.path.clone()).collect(),
        Err(_) => return,
    };
    let mut removed = 0usize;
    for swf_path in &paths {
        let base_path = std::format!("{}.base", swf_path);
        let Some(url) = crate::sources::gamezip::read_file_bounded(&base_path, 4096)
            .and_then(|b| std::string::String::from_utf8(b).ok())
        else {
            continue; // no `.base` → single-file / non-Flashpoint game, nothing to dedup
        };
        let url = url.trim();
        let Some(rest) = url
            .strip_prefix("http://")
            .or_else(|| url.strip_prefix("https://"))
        else {
            continue;
        };
        let rest = rest.split(['?', '#']).next().unwrap_or(rest);
        if rest.is_empty() || rest.ends_with('/') {
            continue;
        }
        // Containment: a `..` segment must never walk this unlink out of the
        // game's own `.files/` dir. No catalog entry is known to carry one, but
        // the READ path already guards it (`SidecarNavigator::push_segments`) and
        // a DELETE path must not be laxer than the read path.
        if rest.split('/').any(|s| s == "..") {
            continue;
        }
        // `<game>.files/<host>/<path>/<entry>.swf` — the tree copy of the entry
        // SWF (only zipped GameZIPs have a host-pathed tree here; others no-op).
        let files_root = crate::sidecar_dir_for(Some(swf_path))
            .to_string_lossy()
            .trim_end_matches('/')
            .to_string();
        let tree_entry = std::format!("{}/{}", files_root, rest);
        if std::fs::remove_file(&tree_entry).is_ok() {
            removed += 1;
            log(&std::format!("dedup: removed redundant tree entry {}\n", tree_entry));
        }
        // Prune the now-empty dir chain up to (and including) `<game>.files/`.
        // `remove_dir` (rmdir) only succeeds on an EMPTY dir, so a multi-file
        // game's populated companion dirs are left untouched. Runs whether we
        // removed the entry just now or a previous (v1) pass did.
        let mut cur = std::path::Path::new(&tree_entry).parent().map(|p| p.to_path_buf());
        while let Some(d) = cur {
            if !d.to_string_lossy().starts_with(&files_root) {
                break; // climbed above <game>.files/
            }
            if std::fs::remove_dir(&d).is_err() {
                break; // non-empty (companions remain) or already gone
            }
            cur = d.parent().map(|p| p.to_path_buf());
        }
    }
    log(&std::format!(
        "dedup: cleaned {} redundant entry-SWF tree copies\n",
        removed
    ));
    let _ = std::fs::write(&marker, b"1");
    crate::sd::commit();
}

pub fn open() {
    // The one-time first-boot housekeeping (dedup + size backfill) is NOT run
    // here: it blocks a few seconds and open() runs before the first frame, so
    // doing it inline would be a black screen. It's deferred to `render` behind
    // the "Optimisation" loading panel instead (see `maybe_arm_startup_migrations`
    // / `PendingNet::RunStartupMigrations`), which shows a frame first, then runs.
    // Any finger business from before a game launched is over.
    touch_press_cancel();
    crate::backend::render::row_touch_cancel();
    log_scan_breakdown(LIBRARY.lock().map(|g| g.entries.len()).unwrap_or(0));
    // Reload URL history from SD on each open so changes from a previous
    // .nro boot are visible. Cheap (file is <2 KB typical).
    load_history_from_sd();
    // Boot-once: load persisted playtime + sort preference.
    ensure_prefs_loaded();
    // The scan runs BEFORE this (main.cpp calls `swf_picker_run` then
    // `ruffle_library_open`), so every entry was built while `tags.json` was
    // still unread and got an empty label list. Put them in step now, or every
    // label is invisible until the player happens to toggle one.
    //
    // Here rather than by loading tags earlier: `ensure_prefs_loaded` is where
    // every persisted table is read, and moving one of them out of it to suit
    // the scan would be the kind of ordering nobody can see from the code.
    //
    // Nothing is PRUNED here, deliberately. Dropping the labels of games the
    // scan did not find looks tidy and is a trap: a card that failed to mount,
    // a games folder just pointed somewhere else, or a scan cut short would all
    // present as "these games no longer exist" and take every label with them.
    // A stale line costs a few bytes; `favorites.json` has never pruned either,
    // for the same reason. In-app deletes clear their own labels (`delete_game`).
    if let Ok(mut s) = LIBRARY.lock() {
        refresh_tags(&mut s.entries);
    }
    // Returning from a game? Bank that session's playtime before we re-sort.
    bank_playtime();
    // If we're re-opening after quitting a game, land the cursor back on
    // that game's row instead of the top of the list.
    let want = last_played_basename();
    // By PATH first. Two games can share a file name now, and the one the
    // player just quit is a particular file, not whichever namesake the sort
    // put first. The name stays as a fallback for the case the path no longer
    // exists -- the game was moved or renamed while it ran.
    let want_path = last_played_path();
    let applet = unsafe { ruffle_is_applet_mode() } != 0;
    log(&std::format!("library: open applet_mode={}\n", applet));
    // The game just played was started from an importer page. The cell table on
    // record is then that page's (its cards and sources, an archive's games or
    // the Flashpoint tiles), not the Play grid's: the cursor's row cannot be
    // read from it, nor the tile to shrink into, and input must not act on it
    // before the grid has drawn its own.
    let from_importer = LAUNCHED_FROM_IMPORTER.swap(false, std::sync::atomic::Ordering::Relaxed);
    if from_importer {
        crate::backend::render::gallery_layout_clear();
    }
    // Identity of the just-played game, for the quit "close" reveal below.
    let mut collapse_info: Option<(std::string::String, std::string::String, u32)> = None;
    if let Ok(mut s) = LIBRARY.lock() {
        s.applet_mode = applet;
        // Fresh open = no search filter.
        s.local_filter = None;
        // The OPEN SHELF SURVIVES (issue #68). Quitting a game is a RETURN, not
        // a fresh start: you left from a shelf, you come back to it, the same
        // way the cursor comes back to the game you played. Dropping it sent
        // the player to TOUS and made them walk back every time.
        //
        // Validated against the entries that were just rescanned, because a
        // shelf can have ceased to exist while the game was running -- emptied
        // from its own OPTIONS, or the card reorganised from a PC.
        if let Some(f) = s.folder_filter.clone() {
            let still_there = f.is_empty() || s.entries.iter().any(|e| on_shelf(e, &f));
            if !still_there {
                s.folder_filter = None;
            }
        }
        set_active_folder(s.folder_filter.as_deref());
        s.anim_origin_ticks = unsafe { ruffle_tick_now() };
        // Apply the active sort BEFORE the cursor position is computed below.
        sort_entries(&mut s.entries, current_sort_mode(), current_sort_reverse());
        // ABSOLUTE index of the game just played. By PATH first: folders (#68)
        // let two games share a file name, and the one just quit is a particular
        // FILE, not whichever namesake the sort put first. The name is the
        // fallback, for when the path is gone -- moved or renamed while it ran.
        //
        // Computed here rather than inside the branch below, because the closing
        // fade has to grow from the same tile the cursor lands on.
        // Kept UNDEFAULTED as well as defaulted, because the two readers below
        // want different things from a miss. The cursor has to land somewhere,
        // so it falls back to the top. The closing fade must NOT: "no game was
        // played" and "the game played is row 0" are different states, and
        // conflating them made a cold boot -- where nothing has been played at
        // all -- shrink game number one from full screen into the top-left
        // corner, over a tile rect that had never been published.
        let found = want_path
            .as_deref()
            .and_then(|p| s.entries.iter().position(|e| e.path == p))
            .or_else(|| {
                want.as_deref()
                    .and_then(|b| s.entries.iter().position(|e| e.basename == b))
            });
        let abs = found.unwrap_or(0);
        s.screen = if s.entries.is_empty() {
            Screen::Empty
        } else {
            // Translated into the shelf's view: `Screen::List` counts rows of
            // what is on screen, and with a shelf open those are not the same
            // number.
            let shown = local_filtered_indices(&s.entries, &s.local_filter, &s.folder_filter);
            let selection = shown.iter().position(|&i| i == abs).unwrap_or(0);
            // The JOUER gallery scrolls by ROWS (not flat index), and a tile's
            // row depends on the justified layout -- so derive the scroll from
            // the layout the last render published (still valid across a
            // quit->library round-trip: same entries). `gallery_scroll_for`
            // falls back to 0 when no layout exists yet (first boot). Using the
            // old flat `clamp_scroll` here put a last-row game off-screen.
            //
            // After a game started from the importer there is no grid layout to
            // read (see `from_importer`): the gallery keeps the scroll it was
            // left at, and its first frame scrolls the game into view once it
            // has published its own (`JOUER_RESCROLL`).
            let scroll_offset = if from_importer {
                JOUER_RESCROLL.store(true, std::sync::atomic::Ordering::Relaxed);
                jouer_scroll()
            } else {
                gallery_scroll_for(selection, jouer_scroll())
            };
            Screen::List { selection, scroll_offset }
        };
        // Returning from a game (last-played still present) -> grab its identity
        // so we can play the cover shrinking back to its tile. `found`, never
        // `abs`: a boot with nothing played, or a last-played game deleted from a
        // PC between sessions, must play NO collapse rather than someone else's.
        if let Some(e) = found.and_then(|i| s.entries.get(i)) {
            collapse_info = Some((e.basename.clone(), e.display_name.clone(), e.color_chip));
        }
    }
    // Snap the gallery glide to wherever the cursor lands (last-played row),
    // so the first JOUER frame doesn't slide in from a stale position.
    // The multi-file badge is a launch-only cue; clear it so the quit reveal
    // (cover shrinking back to the tile) doesn't carry it.
    LAUNCH_MULTI.store(false, std::sync::atomic::Ordering::Relaxed);
    crate::backend::render::gallery_anim_reset();
    crate::backend::render::home_strip_snap();
    crate::backend::render::home_header_pin(false);
    // Play the quit "close" reveal: the cover shrinks from full screen back to
    // the launched tile (rect = the tile's pre-launch screen box, still cached).
    // Not after a game started from the importer: no tile was pressed, and the
    // cached box belongs to whatever the grid last showed (nothing at all on a
    // fresh boot, which shrank the cover into the corner).
    if let (Some((bn, dn, color)), false) = (collapse_info, from_importer) {
        let rect = crate::backend::render::gallery_sel_rect_read();
        crate::backend::render::game_reveal_begin(true, rect, &bn, &dn, color);
    }
}

/// Read a small SD text file WITHOUT trusting `metadata().len()` for the
/// initial buffer allocation. `std::fs::read_to_string` pre-reserves a buffer
/// sized from fstat; in APPLET mode that size comes back bogus, so the reserve
/// fails with `OutOfMemory` and the read errors out even though the file is
/// <2 KB (this is exactly why the URL history showed empty in applet — P0).
/// Reading in a bounded loop (like `loc.rs`'s settings reader, and like the
/// SWF-header scan that works fine in applet) sidesteps the bad pre-reserve.
/// `File::open` still yields `NotFound` for an absent file, so callers can
/// keep distinguishing "absent" from a real read error.
fn read_sd_text(path: &str, max: usize) -> std::io::Result<std::string::String> {
    use std::io::Read;
    let mut file = std::fs::File::open(path)?;
    let mut data: std::vec::Vec<u8> = std::vec::Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        data.extend_from_slice(&buf[..n]);
        if data.len() > max {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "file too large",
            ));
        }
    }
    std::string::String::from_utf8(data)
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidData, "not utf-8"))
}

/// Read the per-URL side data.
///
/// Parsed FIELD BY FIELD out of a generic JSON array rather than deserialised
/// into a fixed tuple: a row is `[url, count?, added?, favorite?]` and anything
/// missing takes its default. Deserialising into a rigid tuple meant that adding
/// one field to this file made every existing row fail to parse — the counts
/// silently reset to "?" on the next build, and the first save then overwrote
/// the file with the empty table. This shape can gain fields without ever
/// invalidating what's already on the card.
fn load_history_meta_from_sd() {
    let txt = match read_sd_text(HISTORY_META_PATH, 256 * 1024) {
        Ok(s) => s,
        // Absent = legitimately nothing learnt yet; safe to write later.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            if let Ok(mut s) = LIBRARY.lock() {
                s.meta_loaded = true;
            }
            return;
        }
        Err(e) => {
            log(&std::format!("library: history meta read failed: {}\n", e));
            return;
        }
    };
    let Ok(json) = serde_json::from_str::<serde_json::Value>(&txt) else {
        log("library: history meta JSON unreadable (will NOT overwrite)\n");
        return;
    };
    let Some(arr) = json.as_array() else {
        log("library: history meta is not an array (will NOT overwrite)\n");
        return;
    };
    let mut list: std::vec::Vec<(std::string::String, u32, u64, bool, u32)> =
        std::vec::Vec::with_capacity(arr.len());
    for row in arr {
        let Some(cells) = row.as_array() else { continue };
        let Some(url) = cells.first().and_then(|v| v.as_str()).filter(|u| !u.is_empty()) else {
            continue;
        };
        let total = cells.get(1).and_then(|v| v.as_u64()).unwrap_or(0) as u32;
        let added = cells.get(2).and_then(|v| v.as_u64()).unwrap_or(0);
        let fav = cells.get(3).and_then(|v| v.as_bool()).unwrap_or(false);
        // Absent in files written before the field existed: 0 until the item's
        // list is opened once, which recomputes it.
        let have = cells.get(4).and_then(|v| v.as_u64()).unwrap_or(0) as u32;
        list.push((url.to_string(), total, added, fav, have));
    }
    if let Ok(mut s) = LIBRARY.lock() {
        log(&std::format!("library: history meta LOADED {} entrie(s)\n", list.len()));
        s.url_meta = list;
        s.meta_loaded = true;
    }
}

/// One more file from `source_url`'s archive.org item is on the card.
///
/// Matched on the ITEM id rather than the URL string, because `url_meta` is keyed
/// on what the user saved (a `/details/` link, or a bare item id) while a
/// download carries a `/download/<item>/<file>` link.
fn bump_url_have(source_url: &str) {
    let Some(item) = crate::net::extract_item_id(source_url).map(|i| i.to_lowercase()) else {
        return;
    };
    let Ok(mut s) = LIBRARY.lock() else { return };
    let Some(slot) = s.url_meta.iter_mut().find(|(u, _, _, _, _)| {
        crate::net::extract_item_id(u).map(|i| i.to_lowercase()).as_deref() == Some(item.as_str())
    }) else {
        return;
    };
    // Never past the item's own file count: the cached total can be stale, and a
    // row reading "82 / 81" would be worse than one lagging by a file.
    if slot.1 > 0 && slot.4 >= slot.1 {
        return;
    }
    slot.4 += 1;
    save_history_meta(&mut s);
}

/// Persist the side-data table, dropping rows for URLs no longer in the history
/// so the file can't grow forever behind the user's back. Best-effort.
///
/// Refuses to write when the last load ERRORED (same rule as the history file):
/// overwriting a table we merely failed to read would turn a transient glitch
/// into permanent data loss.
fn save_history_meta(s: &mut State) {
    // The prune below is only as trustworthy as the URL list it prunes against,
    // so an unread history blocks the write too — otherwise a failed history
    // read would make us drop every row as "no longer in the history".
    if !s.meta_loaded || !s.history_loaded {
        log("library: history meta not loaded cleanly — keeping the SD copy\n");
        return;
    }
    let history = s.url_history.clone();
    s.url_meta.retain(|(u, _, _, _, _)| history.iter().any(|h| h == u));
    let snapshot = s.url_meta.clone();
    match serde_json::to_string(&snapshot) {
        Ok(json) => match std::fs::write(HISTORY_META_PATH, json.as_bytes()) {
            Ok(()) => crate::sd::commit(),
            // The only unlogged branch in an otherwise fully instrumented pair.
            // This table holds the IMPORTER's favourite diamonds, and the caller
            // has already flipped the star and toasted it green — so without this
            // line the star reappears un-starred at the next boot, every time,
            // with nothing written down anywhere.
            Err(e) => log(&std::format!("library: history meta save failed: {}\n", e)),
        },
        Err(e) => log(&std::format!("library: history meta serialise failed: {}\n", e)),
    }
}

/// Remember how many `.swf` files `url` holds (learnt from a metadata fetch),
/// keeping whatever added-date the row already carries.
fn remember_url_total(url: &str, total: u32) {
    let Ok(mut s) = LIBRARY.lock() else { return };
    // Recount while the item's file list IS in memory: this is the only moment
    // the names are known, and the IMPORTER home has to show the figure later
    // without them. By NAME, exactly like the OK badge that will be drawn from
    // the same list one frame from now.
    let have = s
        .remote_files
        .iter()
        .filter(|f| {
            let name: std::string::String = f
                .name
                .chars()
                .map(|c| if matches!(c, '/' | '\\') { '_' } else { c })
                .collect();
            s.entries.iter().any(|e| e.basename == name)
        })
        .count() as u32;
    match s.url_meta.iter_mut().find(|(u, _, _, _, _)| u == url) {
        Some(slot) => {
            if slot.1 == total && slot.4 == have {
                return; // unchanged — no need to rewrite the card
            }
            slot.1 = total;
            slot.4 = have;
        }
        None => s.url_meta.push((url.to_string(), total, now_epoch(), false, have)),
    }
    save_history_meta(&mut s);
}

/// True if the saved URL is favorited (pinned to the top of the IMPORTER list
/// and marked with a solid star chip, like a favorited game).
pub(crate) fn url_is_favorite(s: &State, url: &str) -> bool {
    s.url_meta
        .iter()
        .find(|(u, _, _, _, _)| u == url)
        .map(|(_, _, _, f, _)| *f)
        .unwrap_or(false)
}

/// Flip a saved URL's favorite state and persist. Returns the NEW state.
fn toggle_url_favorite(s: &mut State, url: &str) -> bool {
    let now_fav = match s.url_meta.iter_mut().find(|(u, _, _, _, _)| u == url) {
        Some(slot) => {
            slot.3 = !slot.3;
            slot.3
        }
        None => {
            s.url_meta.push((url.to_string(), 0, now_epoch(), true, 0));
            true
        }
    };
    save_history_meta(s);
    now_fav
}

/// Wall-clock seconds since the epoch, 0 if unavailable. The console clock can
/// be wrong (that's a known Switch/emuNAND issue) — this only ever feeds an
/// informational "added on" line, never a decision.
fn now_epoch() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// The day a UNIX epoch falls on, as the language writes it ("12 oct. 2026",
/// "Oct 12, 2026"), or empty for 0/unknown. It read "2026-10-12" in every
/// language. Civil-from-days (Howard Hinnant's algorithm) — no date crate in a
/// no_std-ish build.
pub(crate) fn format_date(epoch: u64) -> std::string::String {
    if epoch == 0 {
        return std::string::String::new();
    }
    let z = (epoch / 86_400) as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    crate::backend::render::imp_date(y, m as u32, d as u32)
}

/// Strings the importer no longer shows, kept in the tables until they leave
/// all nine at once after the merge (one branch rewriting `loc.rs`, not four).
/// Read here only so the build stays free of warnings meanwhile; this function
/// goes with them.
#[allow(dead_code)]
fn importer_retired_strings() {
    let lc = crate::loc::s();
    let _ = (
        lc.opt_edit,
        lc.dist_search_games,
        lc.dist_empty_hint,
        lc.dist_add,
        lc.dist_count,
        lc.dl_title,
        lc.err_title,
        lc.err_footer_fix,
        lc.extracting,
        lc.lbl_on,
        lc.lbl_off,
    );
    let _ = (
        lc.fp_details_publisher,
        lc.fp_details_date,
        lc.fp_details_size,
        lc.fav_added,
        lc.fav_removed,
        lc.url_info_type,
        crate::loc_ui::ui().filter_verb,
    );
    let _ = crate::loc::files_found(0);
    let _ = crate::loc::count_line(0, 0, None, std::string::String::new);
}

fn load_history_from_sd() {
    load_history_meta_from_sd();
    // 256 KB cap: the history is a few KB at most; the cap is a safety net.
    let txt = match read_sd_text(HISTORY_PATH, 256 * 1024) {
        Ok(s) => s,
        Err(e) => {
            // Distinguish "file absent" (legit empty first boot — safe to
            // persist later) from a real read error (must NOT overwrite, or
            // we'd clobber an existing history we just couldn't read).
            if e.kind() == std::io::ErrorKind::NotFound {
                log(&std::format!("library: history file ABSENT at {}\n", HISTORY_PATH));
                if let Ok(mut s) = LIBRARY.lock() {
                    s.url_history.clear();
                    s.history_idx = None;
                    s.history_loaded = true;
                }
            } else {
                log(&std::format!("library: history read failed: {} (will NOT overwrite)\n", e));
                if let Ok(mut s) = LIBRARY.lock() {
                    s.history_loaded = false;
                }
            }
            return;
        }
    };
    let list: std::vec::Vec<std::string::String> = match serde_json::from_str(&txt) {
        Ok(v) => v,
        Err(e) => {
            // Corrupt/partial JSON: keep the file, refuse to persist over it.
            log(&std::format!("library: history JSON parse failed: {} (will NOT overwrite)\n", e));
            if let Ok(mut s) = LIBRARY.lock() {
                s.history_loaded = false;
            }
            return;
        }
    };
    if let Ok(mut s) = LIBRARY.lock() {
        s.url_history = list;
        if !s.url_history.is_empty() {
            s.history_idx = Some(s.url_history.len() - 1);
        } else {
            s.history_idx = None;
        }
        s.history_loaded = true;
        log(&std::format!(
            "library: history LOADED {} url(s) from {}\n",
            s.url_history.len(), HISTORY_PATH,
        ));
    }
}

fn save_history_to_sd(history: &[std::string::String]) {
    let json = match serde_json::to_string_pretty(&history) {
        Ok(s) => s,
        Err(e) => {
            log(&std::format!("library: history serialise failed: {}\n", e));
            return;
        }
    };
    if let Err(e) = std::fs::write(HISTORY_PATH, json.as_bytes()) {
        log(&std::format!("library: history write failed: {}\n", e));
        return;
    }
    // Flush to the physical card so the applet/app modes agree (see sd.rs).
    crate::sd::commit();
    log(&std::format!(
        "library: history SAVED {} url(s) + committed to {}\n",
        history.len(), HISTORY_PATH,
    ));
}

/// Write the source URL a game was imported from to a `<swf_path>.url` sidecar,
/// so a later in-app bug report can say where it came from. No-op for an empty
/// URL (hand-copied files). Best-effort: a failed write just leaves no sidecar.
fn write_url_sidecar(swf_path: &str, url: &str) {
    let url = url.trim();
    if url.is_empty() {
        return;
    }
    let path = std::format!("{}.url", swf_path);
    if let Err(e) = std::fs::write(&path, url.as_bytes()) {
        log(&std::format!("library: url sidecar write failed: {}\n", e));
        return;
    }
    note_file_created(&path); // keep the directory index truthful for this session
    crate::sd::commit();
}

/// Absolute `.swf` paths of every scanned game.
///
/// Exists so the sidecar navigator can look for an asset in ANOTHER game's tree
/// without listing directories: `std::fs::read_dir` corrupts entry names on
/// Horizon (see the `SWF_CANDIDATES` note in lib.rs), so the C++ scan we already
/// did is the only trustworthy enumeration of what sits on the card.
pub(crate) fn scanned_game_paths() -> std::vec::Vec<std::string::String> {
    LIBRARY
        .lock()
        .map(|s| s.entries.iter().map(|e| e.path.clone()).collect())
        .unwrap_or_default()
}

/// archive.org item id behind a game's `<swf>.url` sidecar, lowercased. Empty
/// when there's no sidecar (hand-copied file) or the source wasn't archive.org
/// (a direct `.swf` URL or a Flashpoint GameZIP — those are matched by filename
/// instead, so they need no id).

/// Record `url` in the history if it's new. Saves to SD.
///
/// A known URL keeps its POSITION: the file's order is the "ADDED" sort, and
/// bumping an entry to the end on every successful open silently turned that
/// sort into "last opened" — an order the label never promised and that made
/// the list reshuffle under the cursor just for looking at something.
fn push_history(url: &str) {
    let url = url.trim().to_string();
    if url.is_empty() {
        return;
    }
    if let Ok(mut s) = LIBRARY.lock() {
        if let Some(pos) = s.url_history.iter().position(|u| u == &url) {
            // Already known — nothing to write, just remember which row it is.
            s.history_idx = Some(pos);
            return;
        }
        // New entry: stamp when it was saved (shown in its options modal). A
        // row this address already has is from an earlier life of it (deleted
        // while the table could not be saved), not this source's: it would
        // bring back the old star and date, and `find` would keep reading it
        // before the new one.
        s.url_meta.retain(|(u, _, _, _, _)| u != &url);
        s.url_meta.push((url.clone(), 0, now_epoch(), false, 0));
        s.url_history.push(url);
        // Hard ceiling only as a runaway guard (a corrupt writer, not a user):
        // dropping someone's oldest import to make room is never what they
        // meant, and the list is searchable + sortable now, so it can be long.
        if s.url_history.len() > HISTORY_MAX {
            log(&std::format!(
                "library: history at the {} cap — dropping the oldest URL\n",
                HISTORY_MAX,
            ));
            s.url_history.remove(0);
        }
        s.history_idx = Some(s.url_history.len() - 1);
        let snapshot = s.url_history.clone();
        let loaded = s.history_loaded;
        if loaded {
            save_history_meta(&mut s);
        }
        drop(s);
        // Only persist when we trust our view of the file. If the last load
        // errored, keep the addition in memory but leave the SD file intact
        // rather than overwrite a history we failed to read.
        if loaded {
            save_history_to_sd(&snapshot);
        } else {
            log("library: history not loaded cleanly — keeping new URL in memory only\n");
        }
    }
}

/// True while the library should keep getting input + render frames. C++
/// loops on this. Returns false once the user picks a game (Picked) or
/// asks to quit (Quit).
pub fn is_active() -> bool {
    match LIBRARY.lock().map(|s| s.screen) {
        Ok(Screen::Inactive) | Ok(Screen::Picked) | Ok(Screen::Quit) => false,
        Ok(_) => true,
        Err(_) => false,
    }
}

/// True if the user picked a game (vs quit). Lets C++ distinguish "load
/// SWF" from "exit `.nro`" after the library loop ends.
pub fn picked() -> bool {
    matches!(LIBRARY.lock().map(|s| s.screen), Ok(Screen::Picked))
}

/// Owned copy of the chosen path. None until the user presses A on a game.
pub fn selected_path() -> Option<std::string::String> {
    LIBRARY.lock().ok().and_then(|s| s.selected_path.clone())
}

/// Reset the library back to Inactive — clears entries, the picked-path
/// slot, and the menu/touches editor sub-screens. Banner texture, banner
/// dims, and anim origin are deliberately kept so we don't re-decode +
/// re-upload the banner PNG every back-to-library cycle. Caller (FFI
/// `ruffle_library_reset`) MUST shutdown and re-init the renderer between
/// game sessions because dropping the SwitchRenderBackend invalidates the
/// banner texture handle that lives on it — we re-decode when the
/// renderer reappears via `ruffle_library_init`.
pub fn reset() {
    // Cancel any download / metadata fetch in flight before we clear state —
    // avoids the C++ multi handle leaking + the partial file staying on SD.
    net::cancel_download();
    net::cancel_archive_fetch();
    crate::backend::render::thumb_cancel_all();
    if let Ok(mut s) = LIBRARY.lock() {
        s.pending_fetch_url.clear();
        s.entries.clear();
        s.selected_path = None;
        s.screen = Screen::Inactive;
        s.banner_tex = 0;
        s.banner_w = 0;
        s.banner_h = 0;
        s.remote_files.clear();
        // Includes the companion phase, so a download abandoned by launching a
        // game cannot arm the next one.
        clear_download_state(&mut s);
        s.distant_error.clear();
        s.failed_url.clear();
        s.downloaded_basenames.clear();
        s.distant_filter = None;
        s.import_filter = None;
        s.import_retry = None;
        s.import_fp_suggest.clear();
        s.import_error_back = Screen::Inactive;
        // The thumbnail pool was just emptied: no blurb or size is coming any
        // more.
        s.fp_desc_slot = -1;
        s.fp_desc_ready = false;
        s.fp_size_slot = -1;
        s.fp_size_pending = false;
        // url_history + history_idx are deliberately NOT cleared — they
        // persist across back-to-library cycles AND across .nro reboots
        // (loaded fresh from SD by load_history_from_sd at every library
        // re-open).
    }
    // Make sure any open keymap editor sub-screen closes too — defensive,
    // in practice menu::close() is called by the library state machine on
    // every exit-from-touches transition, but a back-to-library from
    // mid-game means the user never left the in-game menu cleanly.
    menu::close();
}

/// Forward a Switch-button down-edge from C++. Returns true if consumed.
/// Touch gesture state for the JOUER gallery. Process-wide; only the List
/// screen is touch-driven. Lock order is always TOUCH then LIBRARY.
struct TouchState {
    /// A finger was down on the previous `touch` call.
    down: bool,
    /// Down position (screen px). For a tap this is also the lift position.
    start_x: f32,
    start_y: f32,
    /// True once the finger moved past the drag threshold this gesture.
    dragging: bool,
    /// Set when the press landed on the navbar. The strip is drawn OVER the
    /// screen, so a press there belongs to it whatever is underneath, and no
    /// gallery or row gesture may start.
    nav_press: Option<usize>,
    /// The same for the shelf strip under the tabs, on the Play home.
    strip_press: Option<crate::backend::render::StripHit>,
    /// Set when a press on the navbar or the shelf strip was called off while
    /// the finger stayed down: it slid off its target, or the home went away
    /// under it. The rest of that touch belongs to nobody. Without this the
    /// same finger fell through to whatever lay under it next, started a
    /// gallery gesture at the edge of the grid, and tapped (or launched) the
    /// tile there when it lifted. The lift clears it.
    dead: bool,
    /// Eased scroll (px) captured when the drag began.
    start_scroll_px: f32,
}

static TOUCH: Mutex<TouchState> = Mutex::new(TouchState {
    down: false,
    start_x: 0.0,
    start_y: 0.0,
    dragging: false,
    start_scroll_px: 0.0,
    nav_press: None,
    strip_press: None,
    dead: false,
});

/// Forward the per-frame touchscreen state to the JOUER gallery: drag to scroll,
/// tap a tile to select it, tap the already-selected tile again to launch it.
/// `(x, y)` are screen px; `pressed` is whether a finger is down this frame.
/// No-op on every other screen (those stay button-driven).
/// A tap landed off the gallery: find the row under it and act.
///
/// Tap a row to put the cursor on it, tap the row the cursor is already on to
/// activate it — the same two-step the gallery has always used, and for the same
/// reason: a single tap that activated would fire on whatever your thumb landed
/// on, with no chance to see what you were about to do.
///
/// The activation is `input("A")`, never a copy of what A does. Every screen's
/// A already knows how to open, confirm, rebind or delete; duplicating any of
/// that here is how a touch path drifts away from the button path.
fn tap_ui_row(x: f32, y: f32) {
    // The kind of the screen that is up RIGHT NOW, read from the lock rather
    // than from the render stamp: C++ serves buttons before touch in the same
    // frame, so on the frame a button changes screens the stamp still names the
    // old one -- and would match its stale table exactly.
    let kind = match LIBRARY.lock() {
        Ok(s) => touch_kind(s.screen),
        Err(_) => return,
    };
    let Some(hit) = crate::backend::render::ui_cells_hit(kind, x, y) else {
        return;
    };
    // The keymap editor keeps its selection in the `menu` module, not here.
    if matches!(kind, 70..=89) {
        if menu::touch_select(hit) {
            input("A");
        }
        return;
    }
    let mut activate = false;
    if let Ok(mut s) = LIBRARY.lock() {
        activate = set_screen_selection(&mut s, hit);
    }
    if activate {
        input("A");
    }
}

/// A row-list drag finished: store where it left the list, and bring the cursor
/// along. `offset` is in the screen's own scroll space; `sel_lo..=sel_hi` are the
/// rows now on screen, in the space `selection` is counted in.
///
/// A cursor left behind off-screen is not a cursor: the next press of Down would
/// scroll the list back to it and undo the whole gesture in one frame.
fn commit_row_scroll(offset: usize, sel_lo: usize, sel_hi: usize) {
    let hi = sel_hi.max(sel_lo);
    // The rows above the scrolling part, if any (the directory tree pins two).
    let base = sel_lo - offset.min(sel_lo);
    let screen = match LIBRARY.lock() {
        Ok(s) => s.screen,
        Err(_) => return,
    };
    // The keymap editor is a pad: every control is on screen at once, so it
    // publishes no scrolling row view and a drag over it produces no offset to
    // commit. Guarded rather than left to fall through, because the arms below
    // would read a LIBRARY selection that screen does not own.
    if matches!(screen, Screen::TouchesEditor { .. } | Screen::SettingsKeymapEditor) {
        return;
    }
    let Ok(mut s) = LIBRARY.lock() else { return };
    match screen {
        Screen::BugPicker { selection, .. } => {
            s.screen = Screen::BugPicker {
                selection: selection.clamp(sel_lo, hi),
                scroll_offset: offset,
            };
        }
        Screen::GamesDirPicker { selection, .. } => {
            // CHOISIR and REMONTER are pinned above the listing and never leave
            // the screen, so a cursor parked on one of them is still visible and
            // must not be dragged down into the listing.
            let sel = if selection < base { selection } else { selection.clamp(sel_lo, hi) };
            s.screen = Screen::GamesDirPicker { selection: sel, scroll: offset };
        }
        // The shelf picker has no scroll of its own: its window is DERIVED from
        // the cursor, which the formula pins to the window's LAST row. Putting
        // the cursor there is the only way to say where the list should rest.
        Screen::GameFolderPicker { game_idx, .. } => {
            s.screen = Screen::GameFolderPicker { game_idx, selection: hi };
        }
        _ => {}
    }
}

/// Move the current screen's cursor to `idx`. Returns true when it was ALREADY
/// there, which is the caller's signal to activate instead.
///
/// `idx` is a row of the table the LIVE screen published (`ui_cells_hit` refuses
/// anything else), so it is in range for that screen's own rows. The one thing
/// that is not guaranteed is a screen whose renderer publishes NO table at all:
/// nothing republishes, the previous panel's table stays up, and a hit resolves
/// against geometry that belongs to something else. Those screens are listed
/// below and answer `false` rather than moving a cursor they cannot place.
fn set_screen_selection(s: &mut State, idx: usize) -> bool {
    macro_rules! put {
        ($cur:expr, $mk:expr) => {{
            if $cur == idx {
                return true;
            }
            s.screen = $mk;
            return false;
        }};
    }
    match s.screen {
        Screen::OptionsModal { game_idx, selection } => {
            put!(selection, Screen::OptionsModal { game_idx, selection: idx })
        }
        Screen::GameFolderPicker { game_idx, selection } => {
            put!(selection, Screen::GameFolderPicker { game_idx, selection: idx })
        }
        Screen::GameFolderDelete { game_idx, folder_idx, selection } => put!(
            selection,
            Screen::GameFolderDelete { game_idx, folder_idx, selection: idx }
        ),
        // The column and the pane share one table (see `settings_tab`).
        Screen::Settings { section, in_pane, row } => settings_tab::touch_select(s, section, in_pane, row, idx),
        Screen::SendFailed { row, selection } => {
            put!(selection, Screen::SendFailed { row, selection: idx })
        }
        Screen::GamesDirPicker { selection, scroll } => {
            put!(selection, Screen::GamesDirPicker { selection: idx, scroll })
        }
        Screen::GamesDirConfirm { selection } => {
            put!(selection, Screen::GamesDirConfirm { selection: idx })
        }
        Screen::DeleteConfirm { game_idx, selection } => {
            put!(selection, Screen::DeleteConfirm { game_idx, selection: idx })
        }
        Screen::DistantHistoryConfirm { selection } => {
            put!(selection, Screen::DistantHistoryConfirm { selection: idx })
        }
        Screen::DistantCancelConfirm { selection } => {
            put!(selection, Screen::DistantCancelConfirm { selection: idx })
        }
        Screen::ImportNotAddress { selection } => {
            put!(selection, Screen::ImportNotAddress { selection: idx })
        }
        // One answer, always the focused one: the first tap gives it.
        Screen::AppletNotice { .. } => idx == 0,
        Screen::ProfileDeleteConfirm { game_idx, profile_idx, selection } => {
            put!(selection, Screen::ProfileDeleteConfirm { game_idx, profile_idx, selection: idx })
        }
        Screen::RemoteSortModal { selection, kind, reverse, prev_sel, prev_scroll } => {
            if idx >= remote_sort_count(kind) {
                return false;
            }
            put!(selection, Screen::RemoteSortModal { selection: idx, kind, reverse, prev_sel, prev_scroll })
        }
        Screen::DistantUrlOptions { url_idx, selection } => {
            // Favourite, edit, delete.
            if idx >= 3 {
                return false;
            }
            put!(selection, Screen::DistantUrlOptions { url_idx, selection: idx })
        }
        Screen::SortModal { selection, prev_sel, prev_scroll, dirs } => {
            if idx >= SORT_MODES {
                return false;
            }
            put!(selection, Screen::SortModal { selection: idx, prev_sel, prev_scroll, dirs })
        }
        Screen::BugPicker { selection, scroll_offset } => {
            put!(selection, Screen::BugPicker { selection: idx, scroll_offset })
        }
        Screen::TouchesMenu { game_idx, selection } => {
            put!(selection, Screen::TouchesMenu { game_idx, selection: idx })
        }
        Screen::ProfileList { game_idx, selection } => {
            // The page publishes every row by its index in the list.
            if idx >= s.profile_matches.len() {
                return false;
            }
            put!(selection, Screen::ProfileList { game_idx, selection: idx })
        }
        Screen::CoverPicker { game_idx, selection } => {
            put!(selection, Screen::CoverPicker { game_idx, selection: idx })
        }
        _ => false,
    }
}

/// Forget a press that landed on the navbar or the shelf strip.
///
/// A press arms `nav_press` or `strip_press` and a RELEASE spends it, and
/// anything can happen in between: a button opens a modal, a game launches and
/// quits, the keyboard takes the screen. The release then arrives on a screen
/// nobody pressed anything on. `screen_tab` already refuses while a modal is
/// up, but a game round-trip lands back on JOUER with both strips drawn again
/// -- and C++ sends a no-finger sample every frame, so the first one back would
/// spend the stale press and switch tabs, or shelves, on its own.
fn touch_press_cancel() {
    if let Ok(mut t) = TOUCH.lock() {
        t.nav_press = None;
        t.strip_press = None;
        t.dead = false;
    }
}

/// A tap released on the shelf strip. Never called with a lock held: the
/// search chip opens the keyboard, and the indicators go through `input` like
/// the buttons they stand for.
fn strip_tap(hit: crate::backend::render::StripHit) {
    use crate::backend::render::StripHit;
    match hit {
        StripHit::Search => run_local_search_flow(),
        StripHit::Sort => {
            input("Y");
        }
        StripHit::View => {
            input("X");
        }
        StripHit::Shelf(i) => {
            if let Ok(mut s) = LIBRARY.lock() {
                if let Screen::List { selection, .. } = s.screen {
                    let (keys, cur) = shelf_keys(&s);
                    if i != cur && i < keys.len() {
                        goto_shelf(&mut s, keys[i].clone(), if i > cur { 1.0 } else { -1.0 }, selection, None);
                        // The sound ZL / ZR make for the same move.
                        crate::sfx::play(crate::sfx::Sfx::Tab);
                    }
                }
            }
        }
    }
}

pub fn touch(x: f32, y: f32, pressed: bool) {
    use crate::backend::render as r;
    // Movement (px) before a press is treated as a drag instead of a tap.
    const DRAG_THRESH: f32 = 16.0;

    // A touch whose press was called off goes nowhere until it lifts (see
    // `TouchState::dead`). Asked before the suspend window below, so the lift
    // that ends it counts even during an animation: missed, it would void the
    // next touch whole.
    if let Ok(mut t) = TOUCH.lock() {
        if t.dead {
            if !pressed {
                t.dead = false;
            }
            return;
        }
    }

    // Touch works on the JOUER gallery, the Flashpoint results grid AND the
    // archive.org file list (IMPORTER): all three feed the same render
    // anim/view/cache singletons (only one renders per frame), so the gesture
    // math is shared and only the write-back Screen variant differs.
    let on_gallery = matches!(
        LIBRARY.lock().map(|s| s.screen),
        Ok(Screen::List { .. })
            | Ok(Screen::FpGallery { .. })
            | Ok(Screen::DistantFiles { .. })
            | Ok(Screen::DistantIdle { .. })
    );

    // ── The navbar is asked FIRST ────────────────────────────────────────
    // It sits on top of every tab-home screen, so a finger in the strip is
    // aimed at it and not at the gallery behind. Answered on RELEASE like
    // every other tap; the press only remembers which tab was under it.
    {
        // The SAME suspend window `input()` opens with (see the gate below the
        // button dispatch). A screen must not be re-navigated mid-animation, and
        // here that is not a cosmetic rule:
        //
        // `game_reveal.active` is cleared in exactly one place, inside
        // `game_reveal_step`, and for a quit-collapse that runs only from the
        // `Screen::List` arm of `render`. Switching tabs mid-collapse leaves the
        // animation with nobody to step it, so `game_reveal_active()` stays true
        // for ever -- and `input()` then answers every button by swallowing it.
        // The launcher draws perfectly and nothing responds.
        if r::modal_close_active() || r::distant_reveal_active() || r::game_reveal_active() {
            return;
        }
        let Ok(mut t) = TOUCH.lock() else { return };
        // Not while a chip is held: a finger sliding up from the shelf strip
        // armed a tab on top of the chip, and lifting it on the tab already
        // open left the chip armed for the next finger-up sample to tap. It
        // falls through to the strip instead, which calls it off.
        if pressed && !t.down && t.nav_press.is_none() && t.strip_press.is_none() {
            if let Some(tab) = r::navbar_hit(x, y) {
                t.nav_press = Some(tab);
                return;
            }
        } else if t.nav_press.is_some() {
            if pressed {
                // Still held: the finger may have wandered off the strip, and a
                // press you can slide away from to cancel is how every button
                // on every touchscreen behaves. Disarmed here rather than
                // second-guessed on release, which carries no position at all
                // (C++ reports a lifted finger as (0, 0)). The rest of the
                // touch dies with it, or the same finger would carry on into
                // the shelf strip or the gallery below.
                if r::navbar_hit(x, y).is_none() {
                    t.nav_press = None;
                    t.dead = true;
                }
                return;
            }
            let tab = t.nav_press.take();
            // `goto_tab` re-locks LIBRARY, so the gesture lock goes first: the
            // house rule is TOUCH then LIBRARY, never the other way.
            drop(t);
            if let Some(idx) = tab {
                let cur = LIBRARY
                    .lock()
                    .ok()
                    .and_then(|s| screen_tab(s.screen))
                    .map(|t| t.index());
                if let (Some(cur), Some(target)) = (cur, Tab::from_index(idx)) {
                    // Already there: a tap on the tab you are on does nothing,
                    // rather than resetting its cursor to the top.
                    if cur != idx {
                        goto_tab(target, if idx > cur { 1.0 } else { -1.0 });
                    }
                }
            }
            return;
        }
    }

    // ── Then the shelf strip, on the Play home ───────────────────────────
    // The same press-then-release contract as the navbar: a chip opens its
    // shelf, the search chip edits the search, the two indicators open the
    // sort window and change the view.
    if matches!(LIBRARY.lock().map(|s| s.screen), Ok(Screen::List { .. })) {
        let Ok(mut t) = TOUCH.lock() else { return };
        if pressed && !t.down && t.strip_press.is_none() {
            if let Some(hit) = r::strip_hit(x, y) {
                t.strip_press = Some(hit);
                return;
            }
        } else if let Some(hit) = t.strip_press {
            if pressed {
                // Slid off the target: the press is cancelled, and the rest of
                // the touch with it. Let through, the finger reached the grid
                // and lifting it there picked the tile under it, or launched
                // the selected one.
                if r::strip_hit(x, y) != Some(hit) {
                    t.strip_press = None;
                    t.dead = true;
                }
                return;
            }
            t.strip_press = None;
            drop(t);
            strip_tap(hit);
            return;
        }
    } else if let Ok(mut t) = TOUCH.lock() {
        // The home went away under a held chip: Y opened the sort window, +
        // the options (a game launched by A is caught by `open` on the way
        // back, the keyboard by the search itself). The press is void. Left
        // armed, it was spent by the first finger-up sample back on the home
        // (C++ sends one every frame), which tapped the chip on its own; and
        // the finger still down must not start a gesture on the window either.
        if t.strip_press.take().is_some() && pressed {
            t.dead = true;
            return;
        }
    }

    let mut t = match TOUCH.lock() {
        Ok(t) => t,
        Err(_) => return,
    };

    // Off a touchable gallery: cancel any in-flight gesture, drop scroll override.
    if !on_gallery {
        if t.down {
            t.down = false;
            t.dragging = false;
            r::gallery_touch_scroll_set(None);
        }
        // Everything else IS touchable now: modals, the language grid, the
        // report list, the folder tree, the keyboard. They publish their rows
        // the way the gallery publishes its tiles, so one handler serves all of
        // them and no layout is known here either.
        //
        // The gesture itself lives in `render`, because the in-game keymap
        // editor feeds the same one from a different entry point and two copies
        // would drift apart the first time either was touched. It decides
        // between a tap and a drag; a list too short to scroll only ever
        // produces taps.
        //
        // `input` re-locks LIBRARY, so the gesture lock goes first: the house
        // rule is TOUCH then LIBRARY, never the other way.
        drop(t);
        match r::row_touch_feed(x, y, pressed) {
            r::RowTouch::Tap(sx, sy) => tap_ui_row(sx, sy),
            r::RowTouch::Scrolled { offset, sel_lo, sel_hi } => {
                commit_row_scroll(offset, sel_lo, sel_hi)
            }
            _ => {}
        }
        return;
    }

    // On a gallery the row gesture is not fed at all, so anything it had in
    // flight has to be dropped here. Left alone, a finger that went down on a
    // modal and lifted after a B took us back to the gallery stayed `down`
    // forever; the next non-gallery screen's first sample took the `!pressed &&
    // down` branch and produced a phantom tap at coordinates from minutes ago --
    // and a phantom tap on the row the cursor already sits on is an `input("A")`.
    r::row_touch_cancel();

    let (scroll_px, pitch, band_top, band_bot, rows_total, rows_visible) = r::gallery_view_read();
    // The strip layout scrolls SIDEWAYS. Same gesture, same thresholds, same
    // snap-on-release — only the axis and the clamp differ, so both share this
    // handler rather than growing a second one that would drift out of step.
    let (horizontal, off_min, off_max) = r::gallery_axis_read();
    let max_scroll_px = (rows_total.saturating_sub(rows_visible)) as f32 * pitch;

    // Finger down: begin a gesture only inside the gallery band.
    if pressed && !t.down {
        if y >= band_top && y <= band_bot {
            t.down = true;
            t.dragging = false;
            t.start_x = x;
            t.start_y = y;
            t.start_scroll_px = scroll_px;
        }
        return;
    }

    // Finger held: promote to a drag past the threshold, then track 1:1.
    if pressed && t.down {
        let dx = x - t.start_x;
        let dy = y - t.start_y;
        if !t.dragging && (dx * dx + dy * dy) > DRAG_THRESH * DRAG_THRESH {
            t.dragging = true;
        }
        if t.dragging {
            let px = if horizontal {
                // Drag right (dx > 0) pulls earlier tiles into view.
                (t.start_scroll_px + dx).clamp(off_min, off_max)
            } else {
                // Drag down (dy > 0) pulls earlier rows into view.
                (t.start_scroll_px - dy).clamp(0.0, max_scroll_px)
            };
            r::gallery_touch_scroll_set(Some(px));
        }
        return;
    }

    // Finger up.
    if t.down {
        t.down = false;
        if t.dragging {
            t.dragging = false;
            // Snap to wherever the drag left the view, then release the override
            // so the glide settles. On the strip there are no rows to land on: the
            // offset IS the selection, so it snaps to the nearest tile instead.
            if horizontal {
                let sel = if pitch > 0.0 {
                    ((off_max - scroll_px) / pitch).round().max(0.0) as usize
                } else {
                    0
                };
                let sel = sel.min(rows_total.saturating_sub(1) as usize);
                if let Ok(mut s) = LIBRARY.lock() {
                    if let Screen::List { scroll_offset, .. } = s.screen {
                        s.screen = Screen::List { selection: sel, scroll_offset };
                    }
                }
                r::gallery_touch_scroll_set(None);
                return;
            }
            let new_off = if pitch > 0.0 {
                (scroll_px / pitch).round() as usize
            } else {
                0
            };
            let max_off = rows_total.saturating_sub(rows_visible) as usize;
            let new_off = new_off.min(max_off);
            if let Ok(mut s) = LIBRARY.lock() {
                match s.screen {
                    Screen::List { selection, .. } => {
                        s.screen = Screen::List { selection, scroll_offset: new_off };
                    }
                    Screen::FpGallery { selection, .. } => {
                        s.screen = Screen::FpGallery { selection, scroll: new_off };
                    }
                    Screen::DistantFiles { selection, .. } => {
                        s.screen = Screen::DistantFiles { selection, scroll_offset: new_off };
                    }
                    Screen::DistantIdle { selection, .. } => {
                        s.screen = Screen::DistantIdle { selection, scroll_offset: new_off };
                    }
                    _ => {}
                }
            }
            r::gallery_touch_scroll_set(None);
        } else if horizontal
            // Only when no TILE was hit. In BANDE the big cover is not a cell, so
            // this is the only way to tap it. In ETAGERE the enlarged cover IS a
            // cell, and testing this rect first made every tap on it launch at once
            // instead of selecting — worse, mid-slide it launched `selection` while
            // a different tile sat under the finger. The cell table wins; this is
            // the fallback for layouts whose hero is not in it.
            && r::gallery_hit_test(t.start_x, t.start_y).is_none()
            && {
                let (rx, ry, rw, rh) = r::gallery_sel_rect_read();
                t.start_x >= rx && t.start_x <= rx + rw && t.start_y >= ry && t.start_y <= ry + rh
            }
        {
            let mut launch = false;
            if let Ok(s) = LIBRARY.lock() {
                launch = !s.applet_mode && !s.entries.is_empty();
            }
            if launch {
                input("A");
            }
            return;
        } else if let Some(hit) = r::gallery_hit_test(t.start_x, t.start_y) {
            // Tap: first tap on a tile selects it; tapping the already-selected
            // tile activates it (reuse the A-press path — launch on JOUER,
            // download on the Flashpoint grid).
            let mut launch = false;
            if let Ok(mut s) = LIBRARY.lock() {
                match s.screen {
                    Screen::List { selection, scroll_offset } => {
                        if hit == selection {
                            launch = true;
                        } else {
                            s.screen = Screen::List { selection: hit, scroll_offset };
                        }
                    }
                    Screen::FpGallery { selection, scroll } => {
                        if hit == selection {
                            launch = true;
                        } else {
                            s.screen = Screen::FpGallery { selection: hit, scroll };
                        }
                    }
                    Screen::DistantFiles { selection, scroll_offset } => {
                        if hit == selection {
                            launch = true;
                        } else {
                            s.screen = Screen::DistantFiles { selection: hit, scroll_offset };
                        }
                    }
                    Screen::DistantIdle { selection, scroll_offset } => {
                        // The two cards are buttons: one tap acts. A source row
                        // is selected by a first tap and opened by a second, so
                        // a thumb landing on the wrong one only moves the cursor.
                        // Dragging is handled above.
                        if hit < IMPORTER_PINNED_ROWS || hit == selection {
                            s.screen = Screen::DistantIdle { selection: hit, scroll_offset };
                            launch = true;
                        } else {
                            s.screen = Screen::DistantIdle { selection: hit, scroll_offset };
                        }
                    }
                    _ => {}
                }
            }
            if launch {
                // input("A") re-locks LIBRARY: drop TOUCH first to keep the
                // TOUCH-then-LIBRARY order and avoid any re-entrancy.
                drop(t);
                input("A");
            }
        }
    }
}

pub fn input(button: &str) -> bool {
    // Every press, then the sound it deserves, read from what it changed
    // (`sfx::after_input`): no screen has to remember to make a noise.
    let before = LIBRARY.lock().map(|s| s.screen).ok();
    let closing_before = crate::backend::render::modal_close_active();
    let reveal_before = crate::backend::render::distant_reveal_active();
    let consumed = input_inner(button);
    if let (Some(b), Ok(a)) = (before, LIBRARY.lock().map(|s| s.screen)) {
        let closing = (!closing_before && crate::backend::render::modal_close_active())
            || (!reveal_before && crate::backend::render::distant_reveal_active());
        let opened = modal_kind(a) != 0 && modal_kind(b) == 0;
        crate::sfx::after_input(button, b, a, closing, screen_tab(b), screen_tab(a), opened);
    }
    consumed
}

fn input_inner(button: &str) -> bool {
    // Suspend input during a close-out / reveal transition (brief) so screens
    // can't be re-navigated mid-animation; the deferred swap lands in render()
    // when the animation finishes.
    if crate::backend::render::modal_close_active()
        || crate::backend::render::distant_reveal_active()
        || crate::backend::render::game_reveal_active()
    {
        return true;
    }
    // Settings keymap editor: closing it (B at the editor's top level — the List
    // screen, kind 2 — not inside a dropdown) scales the editor OUT before
    // landing on the REGLAGES tab. Intercept B HERE, before forwarding to menu,
    // so the menu stays active (drawable) during the scale-out; render closes it
    // + swaps to the tab when the pop finishes. The tab itself never pops (it's a
    // tab, not a modal). Only the Settings editor — TouchesEditor keeps its
    // instant swap to the (modal) sub-menu, which pops in.
    if matches!(button, "B" | "Minus") && menu::screen_kind() == 2 {
        let is_settings_editor = LIBRARY
            .lock()
            .map(|s| matches!(s.screen, Screen::SettingsKeymapEditor))
            .unwrap_or(false);
        if is_settings_editor {
            if let Ok(mut p) = PENDING_AFTER_CLOSE.lock() {
                // Back to Jeu par défaut, on the Touches row that opened it.
                *p = Some(PendingClose::CloseMenuGoto(settings_at(SetSection::Defaults, "TOUCHES")));
            }
            crate::backend::render::modal_close_begin();
            return true;
        }
    }
    // A game's controls editor: Y opens the shared profiles, the mockup's
    // shortcut. They are a page of the game's Touches et curseur (its "Apply a
    // profile" row) and come off the network, so the editor closes onto that
    // page and the page's own flow runs from there: B on the profiles comes
    // back to the page, where the path says it goes.
    if button == "Y" && menu::screen_kind() == 2 {
        let game = LIBRARY.lock().ok().and_then(|s| match s.screen {
            Screen::TouchesEditor { game_idx } => Some(game_idx),
            _ => None,
        });
        if let Some(game_idx) = game {
            menu::close();
            if let Ok(mut s) = LIBRARY.lock() {
                goto_touches_menu(&mut s, game_idx, 1);
            }
            defer_net(PendingNet::OpenProfiles { game_idx }, game_idx);
            return true;
        }
    }
    // Sub-screen: TOUCHES editor owns input while active.
    if menu::is_active() {
        let consumed = menu::input(button);
        // If menu just closed itself, fall back to the OPTIONS modal.
        if !menu::is_active() {
            let mut dest: Option<Screen> = None;
            if let Ok(mut s) = LIBRARY.lock() {
                match s.screen {
                    Screen::TouchesEditor { game_idx } => {
                        // Back to the TOUCHES sub-menu's EDIT row (#20 regroup).
                        goto_touches_menu(&mut s, game_idx, 0);
                        dest = Some(s.screen);
                    }
                    Screen::SettingsKeymapEditor => {
                        s.screen = settings_at(SetSection::Defaults, "TOUCHES");
                        dest = Some(s.screen);
                    }
                    _ => {}
                }
            }
            if let Some(d) = dest {
                // Scale the destination back in ONLY if it's a real modal (the
                // OPTIONS / TOUCHES sub-menu). The REGLAGES destination is a TAB,
                // not a popup, so it must NOT pop — its sub-modals animate, it
                // doesn't. modal_kind == 0 means "not a modal".
                if modal_kind(d) != 0 {
                    crate::backend::render::modal_open_begin();
                }
            }
        }
        return consumed;
    }
    // Special case: A / ZR on DistantIdle triggers swkbd or HTTPS metadata
    // fetch, both synchronous + several seconds long. We MUST release the
    // LIBRARY lock for the duration so render() can keep redrawing the
    // last-known screen state and any other callers don't deadlock.
    //
    // Same for A on OPTIONS > RENOMMER — opens swkbd to type the new
    // display name. Hoisted here for the same reason.
    {
        let screen_snap = match LIBRARY.lock() {
            Ok(g) => g.screen,
            Err(_) => return false,
        };
        // ── Navbar (v1.2.0): L/R switch tabs from any tab-home screen. ──
        // Intercepted before per-screen handling so the home screens don't
        // need to know about it. Sub-screens (`screen_tab` == None) keep L/R
        // for their own use (e.g. nothing today; DISTANT paging moved to Up/Down).
        if matches!(button, "L" | "R") {
            if let Some(tab) = screen_tab(screen_snap) {
                // One tab per PRESS. L / R auto-repeat (the file list pages
                // with them), and a held shoulder spun the tabs past the one
                // wanted every 80 ms.
                if !nav_may_wrap() {
                    return true;
                }
                let target = if button == "L" { tab.prev() } else { tab.next() };
                goto_tab(target, if button == "L" { -1.0 } else { 1.0 });
                return true;
            }
        }
        if let Screen::DistantIdle { selection, scroll_offset } = screen_snap {
            if button == "X" {
                // X anywhere on the IMPORTER home = search Flashpoint for a game
                // to download (the glyph on the Flashpoint card says so).
                // Hoisted: swkbd + HTTPS must run WITHOUT the lock.
                run_fp_search_flow(Some(selection));
                return true;
            }
            if button == "Minus" {
                // Search the saved URLs — same Minus-opens-swkbd convention as
                // the JOUER gallery and the archive.org file list. Nothing to
                // search before the first one is saved (the hint is not shown).
                let any = LIBRARY.lock().map(|s| !s.url_history.is_empty()).unwrap_or(false);
                if any {
                    run_import_search_flow();
                }
                return true;
            }
            if button == "A" {
                // The Flashpoint card runs the same flow X runs, so the shortcut
                // and the card cannot drift apart; the add card asks for an
                // address.
                if selection == 0 {
                    run_fp_search_flow(Some(0));
                    return true;
                }
                if selection == 1 {
                    run_add_url_flow(None);
                    return true;
                }
                let url = LIBRARY.lock().ok().and_then(|s| {
                    importer_abs_for_row(&s, selection)
                        .and_then(|abs| s.url_history.get(abs).cloned())
                });
                let Some(u) = url else { return true };
                // A direct link whose game is already on the card plays it.
                if url_is_direct(&u) {
                    let name = safe_name_from_url(crate::sources::wayback_raw(&u).as_str());
                    if let Ok(mut s) = LIBRARY.lock() {
                        if let Some(abs) = s.entries.iter().position(|e| e.basename == name) {
                            let rect = crate::backend::render::import_sel_rect();
                            launch_from_importer(&mut s, abs, rect, Screen::DistantIdle { selection, scroll_offset });
                            return true;
                        }
                    }
                }
                // archive.org → async fetch + reveal (begun inside, opens the row
                // with a spinner); direct .swf → download screen.
                run_fetch_for_url(&u, selection, scroll_offset);
                return true;
            }
        }
        // The error page: A sends the same request again where that can help;
        // X searches Flashpoint for a word archive.org did not know; Y retypes
        // the address that failed, pre-filled (typos in a long archive.org
        // address are THE common failure). Hoisted: the keyboard and the
        // network run without the lock.
        if matches!(screen_snap, Screen::DistantError) {
            let (retry, suggest, failed) = LIBRARY
                .lock()
                .ok()
                .map(|s| (s.import_retry.clone(), s.import_fp_suggest.clone(), s.failed_url.clone()))
                .unwrap_or_default();
            if button == "A" {
                if let Some(action) = retry {
                    retry_import(action);
                    return true;
                }
            }
            if button == "X" && !suggest.is_empty() {
                let origin = LIBRARY.lock().map(|mut s| {
                    clear_import_error(&mut s);
                    s.import_return_row
                });
                start_fp_search(&suggest, origin.ok());
                return true;
            }
            if button == "Y" && !failed.is_empty() {
                run_fix_url_flow(&failed);
                return true;
            }
        }
        // EDIT a URL from its options panel (row 1) — swkbd, hoisted.
        if let Screen::DistantUrlOptions { url_idx, selection } = screen_snap {
            if button == "A" && selection == 1 {
                run_edit_url_flow(url_idx);
                return true;
            }
        }
        // Text that was not an address: retype it, or search Flashpoint for it.
        if let Screen::ImportNotAddress { selection } = screen_snap {
            if button == "A" {
                let typed = LIBRARY.lock().map(|s| s.import_typed.clone()).unwrap_or_default();
                if selection == 0 {
                    run_add_url_flow(Some(&typed));
                } else {
                    start_fp_search(&typed, Some(1));
                }
                return true;
            }
        }
        // NOUVEAU DOSSIER on the per-game folder picker (issue #68): the last
        // row opens swkbd, so it is hoisted out of the handler like every other
        // keyboard flow -- the library lock must not be held across the applet.
        if let Screen::GameFolderPicker { game_idx, selection } = screen_snap {
            if button == "A" {
                let last = LIBRARY
                    .lock()
                    .map(|s| game_folder_rows(&s).len() + 1)
                    .unwrap_or(1);
                if selection == last {
                    run_new_game_folder_flow(game_idx);
                    return true;
                }
            }
        }
        if let Screen::OptionsModal { game_idx, selection } = screen_snap {
            if button == "A" && OPTIONS_ENTRIES.get(selection).copied() == Some("RENOMMER") {
                run_rename_flow(game_idx);
                return true;
            }
            // JAQUETTE = Flashpoint cover search by name. Hoisted: it's a
            // synchronous HTTPS call that must not run under the LIBRARY lock.
            if button == "A" && OPTIONS_ENTRIES.get(selection).copied() == Some("JAQUETTE") {
                run_cover_search_flow(game_idx);
                return true;
            }
        }
        // TOUCHES sub-menu (#20 regroup): APPLY (1) opens the picker; SHARE (2)
        // opens the share confirm (fetches my existing shared profile for the
        // diff); REVERT (5) opens the revert preview. All hoisted (network/I/O).
        // EDIT (0) + CURSOR SPEED (3) + SHOW-CURSOR (4) are handled under the lock.
        if let Screen::TouchesMenu { game_idx, selection } = screen_snap {
            if button == "A" && selection == 1 {
                defer_net(PendingNet::OpenProfiles { game_idx }, game_idx);
                return true;
            }
            if button == "A" && selection == 2 {
                // PARTAGER. If the controls are already a catalog profile (unedited),
                // this is an instant "already shared" toast — run it inline so no
                // loading panel flashes. Otherwise it fetches my shared profile for
                // the before/after diff (network) → defer + show the panel.
                let dup = LIBRARY
                    .lock()
                    .ok()
                    .and_then(|g| g.entries.get(game_idx).map(|e| e.basename.clone()))
                    .map(|b| keymap::provenance(&b).starts_with("community:"))
                    .unwrap_or(false);
                if dup {
                    run_open_share_confirm_flow(game_idx);
                } else {
                    defer_net(PendingNet::OpenShareConfirm { game_idx }, game_idx);
                }
                return true;
            }
            if button == "A" && selection == 6 {
                run_open_revert_preview_flow(game_idx);
                return true;
            }
        }
        // A on the revert preview = actually revert (#20). Hoisted (file I/O).
        if let Screen::RevertPreview { game_idx } = screen_snap {
            if button == "A" {
                run_touches_revert(game_idx);
                return true;
            }
        }
        if let Screen::ProfileShareConfirm { game_idx } = screen_snap {
            if button == "A" {
                defer_net(PendingNet::ShareProfile { game_idx }, game_idx);
                return true;
            }
        }
        // A on the CoverPicker = download + cache the chosen logo (HTTPS).
        // Minus = refine: re-type the title and re-search (swkbd + HTTPS).
        if let Screen::CoverPicker { game_idx, selection } = screen_snap {
            if button == "A" {
                run_cover_fetch_flow(game_idx, selection);
                return true;
            }
            if button == "Minus" {
                run_cover_research_flow(game_idx);
                return true;
            }
        }
        // A on a ProfileList row = open the before/after preview (#20). Hoisted:
        // building the diff reads the current keymap off SD.
        if let Screen::ProfileList { game_idx, selection } = screen_snap {
            if button == "A" {
                // The COPY FROM ANOTHER GAME row opens the list of the player's
                // other games instead (reads their keymaps, so hoisted too).
                let copy_row = LIBRARY
                    .lock()
                    .ok()
                    .and_then(|g| {
                        g.profile_matches
                            .get(selection)
                            .map(|m| crate::profiles::is_copy_row(&m.profile.id))
                    })
                    .unwrap_or(false);
                if copy_row {
                    run_open_copies_flow(game_idx, selection);
                } else {
                    run_open_preview_flow(game_idx, selection);
                }
                return true;
            }
        }
        // A on the preview = apply the profile (#20). Hoisted: applying bumps the
        // relay popularity counter (a network POST) outside the LIBRARY lock.
        if let Screen::ProfilePreview { game_idx, profile_idx } = screen_snap {
            if button == "A" {
                defer_net(PendingNet::ApplyProfile { game_idx, profile_idx }, game_idx);
                return true;
            }
        }
        // A on the delete confirm = remove my shared profile (#20). Hoisted: the
        // DELETE is an HTTPS POST that must not run under the LIBRARY lock.
        if let Screen::ProfileDeleteConfirm { game_idx, profile_idx, selection } = screen_snap {
            // A on "Delete" deletes (a network call, hoisted); A on "Cancel"
            // falls through to the handler, which goes back.
            if button == "A" && selection == 1 {
                defer_net(PendingNet::DeleteProfile { game_idx, profile_idx }, game_idx);
                return true;
            }
        }
        // SEARCH is on Minus (-) now (was X). Hoisted because swkbd is a
        // synchronous fullscreen applet that must not run under the LIBRARY lock.
        if matches!(screen_snap, Screen::DistantFiles { .. }) && button == "Minus" {
            run_distant_search_flow();
            return true;
        }
        if matches!(screen_snap, Screen::List { .. }) && button == "Minus" {
            run_local_search_flow();
            return true;
        }
        // A on the bug-report picker = describe + send. Hoisted: swkbd + the
        // synchronous HTTPS POST must not run under the LIBRARY lock.
        if let Screen::BugPicker { selection, .. } = screen_snap {
            if button == "A" {
                run_bug_report_flow(selection);
                return true;
            }
        }
        // X in the folder picker = create a subfolder here. Hoisted because the
        // swkbd blocks, and blocking under the library lock freezes the app.
        if let Screen::GamesDirPicker { .. } = screen_snap {
            if button == "X" {
                run_new_folder_flow();
                return true;
            }
        }
        // RÉGLAGES rows that open the keyboard: the nickname (#20) and the idea
        // (keyboard, then a POST). Hoisted for the same reason.
        if let Screen::Settings { section, in_pane: true, row } = screen_snap {
            if button == "A" && section == SetSection::Profile && row == settings_tab::row_of(section, "PSEUDO") {
                settings_tab::run_pseudo_flow();
                return true;
            }
            if button == "A" && section == SetSection::Help && row == settings_tab::row_of(section, "IDEE") {
                settings_tab::run_suggestion_flow();
                return true;
            }
        }
        // Réessayer after a report the relay did not take: the POST again.
        if let Screen::SendFailed { row, selection: 1 } = screen_snap {
            if button == "A" {
                settings_tab::run_retry_flow(row);
                return true;
            }
        }
        if let Screen::FpGallery { .. } = screen_snap {
            // X or Minus = a new search without going back to the IMPORTER home,
            // the keyboard pre-filled with this one so it can be refined.
            // Hoisted: swkbd + the async GET must run WITHOUT the LIBRARY lock.
            // Not while the search is still loading, where Minus cancels it.
            let (loading, failed) = LIBRARY.lock().map(|s| (s.fp_loading, s.fp_error)).unwrap_or((false, false));
            if !loading && (button == "X" || button == "Minus") {
                run_fp_search_flow(None);
                return true;
            }
            // A on a search that failed: the same search again, no keyboard.
            if failed && button == "A" {
                run_fp_requery(false);
                return true;
            }
            // Hidden ZL+ZR chord (synthesised in main.cpp): flip the family
            // filter and re-run the query. Hoisted because it starts an async GET.
            if button == "ZL+ZR" {
                run_fp_requery(true);
                return true;
            }
        }
    }
    let mut s = match LIBRARY.lock() {
        Ok(g) => g,
        Err(_) => return false,
    };
    let screen_copy = s.screen;
    let consumed = match screen_copy {
        Screen::Inactive | Screen::Picked | Screen::Quit => false,
        // Input is suspended during the launch reveal (handled above); this arm
        // only satisfies exhaustiveness.
        Screen::Launching { .. } => true,
        Screen::Empty => {
            // A (and Y, its old shortcut) go and find games. Minus used to quit
            // the app from here, the one screen where the search button closed
            // everything; quitting is in RÉGLAGES, like from any other tab.
            if matches!(button, "A" | "Y") {
                s.screen = distant_idle_at(&s, 0);
            }
            true
        }
        Screen::List { selection, scroll_offset } => {
            handle_list_input(&mut s, button, selection, scroll_offset);
            true
        }
        Screen::AppletNotice { selection, scroll_offset } => {
            // Any dismiss button returns where the game was pressed: the same
            // list row, or the importer page it was on (`launch_from_importer`).
            if matches!(button, "A" | "B" | "Minus") {
                let page = LAUNCH_BACKDROP.lock().ok().and_then(|mut g| g.take());
                s.screen = page.unwrap_or(Screen::List { selection, scroll_offset });
            }
            true
        }
        Screen::OptionsModal { game_idx, selection } => {
            handle_options_input(&mut s, button, game_idx, selection);
            true
        }
        Screen::GameFolderPicker { game_idx, selection } => {
            // A on the last row (new folder) is hoisted — swkbd.
            handle_game_folder_input(&mut s, button, game_idx, selection);
            true
        }
        Screen::GameFolderDelete { game_idx, folder_idx, selection } => {
            handle_game_folder_delete_input(&mut s, button, game_idx, folder_idx, selection);
            true
        }
        Screen::TouchesEditor { .. } => false,
        Screen::DeleteConfirm { game_idx, selection } => {
            handle_delete_confirm_input(&mut s, button, game_idx, selection);
            true
        }
        Screen::CoverPicker { game_idx, selection } => {
            // A is hoisted (HTTPS fetch); here we only move/cancel.
            handle_cover_picker_input(&mut s, button, game_idx, selection);
            true
        }
        Screen::FpGallery { selection, scroll } => {
            handle_fp_gallery_input(&mut s, button, selection, scroll);
            true
        }
        Screen::FpDetails { selection, scroll, .. } => {
            // `+` / `B` close back to the results (the deferred modal-close
            // machinery scales it out). A does exactly what A on the tile behind
            // does, downloading the game or playing it when it is on the card:
            // same code, so the two cannot disagree about what a press does.
            if matches!(button, "B" | "Plus") {
                s.screen = Screen::FpGallery { selection, scroll };
            } else if button == "A" {
                s.screen = Screen::FpGallery { selection, scroll };
                handle_fp_gallery_input(&mut s, "A", selection, scroll);
            }
            true
        }
        Screen::SortModal { selection, prev_sel, prev_scroll, dirs } => {
            handle_sort_modal_input(&mut s, button, selection, prev_sel, prev_scroll, dirs);
            true
        }
        Screen::RemoteSortModal { selection, kind, reverse, prev_sel, prev_scroll } => {
            handle_remote_sort_modal_input(&mut s, button, selection, kind, reverse, prev_sel, prev_scroll);
            true
        }
        Screen::DistantIdle { selection, scroll_offset } => {
            handle_distant_idle_input(&mut s, button, selection, scroll_offset);
            true
        }
        Screen::DistantUrlOptions { url_idx, selection } => {
            // A on row 1 (edit the address) is hoisted; here: move, the
            // favourite switch, delete, back.
            handle_distant_url_options_input(&mut s, button, url_idx, selection);
            true
        }
        Screen::DistantFiles { selection, scroll_offset } => {
            handle_distant_files_input(&mut s, button, selection, scroll_offset);
            true
        }
        Screen::DistantLoading => {
            // Input is suspended during the opening reveal (handled at the top of
            // input via `distant_reveal_active`); once open, B cancels the async
            // fetch and folds the window back into the row it came out of, where
            // render lands the cursor. (Y cancelled too, for no reason any hint
            // ever gave.)
            if matches!(button, "B" | "Minus") {
                net::cancel_archive_fetch();
                // Nothing failed: no address to fix and nothing to send again
                // from a later error page that is about something else.
                s.failed_url.clear();
                s.import_retry = None;
                crate::backend::render::distant_reveal_begin_collapse();
            }
            true
        }
        Screen::DistantDownloading => {
            // B stops the download. Past ten seconds or 20 MB it asks first,
            // since stopping loses what came; before that it just stops. The
            // game's other files are small and fetched as part of the import,
            // so B waits for them. Any other button is ignored.
            if button == "B" && !s.dl_companion_active {
                let (done, _) = net::download_progress();
                if download_elapsed() > 10.0 || done > 20 * 1024 * 1024 {
                    s.screen = Screen::DistantCancelConfirm { selection: 0 };
                } else {
                    stop_download(&mut s);
                }
            }
            true
        }
        Screen::DistantCancelConfirm { selection } => {
            match button {
                // The answers side by side: Left / Up on the first, Right / Down
                // on the second, so a held direction rests on one.
                "Left" | "StickLLeft" | "Up" | "StickLUp" => s.screen = Screen::DistantCancelConfirm { selection: 0 },
                "Right" | "StickLRight" | "Down" | "StickLDown" => {
                    s.screen = Screen::DistantCancelConfirm { selection: 1 }
                }
                // Stop: the question scales out with the page still behind it,
                // the download keeps going until it has, then render stops it.
                "A" if selection == 1 => {
                    if let Ok(mut p) = PENDING_AFTER_CLOSE.lock() {
                        *p = Some(PendingClose::StopDownload);
                    }
                    crate::backend::render::modal_close_begin();
                }
                // Carry on.
                "A" | "B" | "Minus" => s.screen = Screen::DistantDownloading,
                _ => {}
            }
            true
        }
        Screen::DistantError => {
            // A with something to send again, X and Y are hoisted (the network,
            // the keyboard). Otherwise A, B and Minus go back to the list the
            // failure came from.
            if matches!(button, "A" | "B" | "Minus") {
                leave_import_error(&mut s);
            }
            true
        }
        Screen::ImportNotAddress { .. } => {
            // A is hoisted (the keyboard, or the search).
            match button {
                "Left" | "StickLLeft" | "Up" | "StickLUp" => s.screen = Screen::ImportNotAddress { selection: 0 },
                "Right" | "StickLRight" | "Down" | "StickLDown" => s.screen = Screen::ImportNotAddress { selection: 1 },
                "B" | "Minus" => s.screen = distant_idle_at(&s, 1),
                _ => {}
            }
            true
        }
        Screen::DistantHistoryConfirm { selection } => {
            // The delete (A on "Delete") scales the panel out first and runs at
            // render's close-done; Cancel drops back onto the source's options,
            // on the row that asked, as a game's delete does.
            match button {
                "Left" | "StickLLeft" | "Up" | "StickLUp" => s.screen = Screen::DistantHistoryConfirm { selection: 0 },
                "Right" | "StickLRight" | "Down" | "StickLDown" => {
                    s.screen = Screen::DistantHistoryConfirm { selection: 1 }
                }
                "A" if selection == 1 => {
                    let idx = s.history_idx.unwrap_or(0);
                    let row = importer_row_for_abs(&s, idx);
                    if let Ok(mut p) = PENDING_AFTER_CLOSE.lock() {
                        *p = Some(PendingClose::DeleteHistory { row });
                    }
                    crate::backend::render::modal_close_begin();
                }
                "A" | "B" | "Minus" => {
                    let url_idx = s.history_idx.unwrap_or(0);
                    s.screen = Screen::DistantUrlOptions { url_idx, selection: 2 };
                }
                _ => {}
            }
            true
        }
        Screen::Settings { section, in_pane, row } => {
            settings_tab::handle_settings_input(&mut s, button, section, in_pane, row);
            true
        }
        Screen::GamesDirPicker { selection, scroll } => {
            handle_games_dir_input(&mut s, button, selection, scroll);
            true
        }
        Screen::GamesDirConfirm { selection } => {
            handle_games_dir_confirm_input(&mut s, button, selection);
            true
        }
        // Renames are running; nothing here should react to a button.
        Screen::GamesDirMoving => true,
        // Owned by the reused menu::* editor (handled at the top of input()).
        Screen::SettingsKeymapEditor => false,
        Screen::BugPicker { selection, scroll_offset } => {
            handle_bug_picker_input(&mut s, button, selection, scroll_offset);
            true
        }
        Screen::SendFailed { row, .. } => {
            // A on Réessayer is hoisted (a network call); the rest is here.
            settings_tab::handle_send_failed_input(&mut s, button, row);
            true
        }
        Screen::TouchesMenu { game_idx, selection } => {
            handle_touches_menu_input(&mut s, button, game_idx, selection);
            true
        }
        Screen::ProfileList { game_idx, selection } => {
            handle_profile_list_input(&mut s, button, game_idx, selection);
            true
        }
        Screen::ProfileDeleteConfirm { game_idx, profile_idx, selection } => {
            // A on "Delete" is hoisted in input(); everything else here.
            match button {
                "Left" | "StickLLeft" | "Right" | "StickLRight" | "Up" | "StickLUp" | "Down" | "StickLDown" => {
                    s.screen = Screen::ProfileDeleteConfirm { game_idx, profile_idx, selection: 1 - selection.min(1) };
                }
                "A" | "B" | "Minus" => {
                    s.screen = Screen::ProfileList { game_idx, selection: profile_idx };
                }
                _ => {}
            }
            true
        }
        // Transient loading frame (#20): swallow input while the deferred flow runs.
        Screen::ProfileLoading => true,
        Screen::ProfilePreview { game_idx, profile_idx } => {
            // A (apply) is hoisted in input(); B returns to the picker.
            if matches!(button, "B" | "Minus") {
                s.preview_rows.clear();
                s.screen = Screen::ProfileList { game_idx, selection: profile_idx };
            }
            true
        }
        Screen::RevertPreview { game_idx } => {
            // A (revert) is hoisted in input(); B returns to the sub-menu.
            if matches!(button, "B" | "Minus") {
                s.preview_rows.clear();
                goto_touches_menu(&mut s, game_idx, 6); // the REVERT row
            }
            true
        }
        Screen::ProfileShareConfirm { game_idx } => {
            if matches!(button, "B" | "Minus") {
                // Cancel → back to the TOUCHES sub-menu's SHARE row (index 2).
                goto_touches_menu(&mut s, game_idx, 2);
            }
            // "A" (confirm) is hoisted to run_share_profile_flow.
            true
        }
    };
    // Defer a modal close so it can scale OUT: render() swaps to the stashed
    // target once the close pop finishes. Only plain modal -> non-modal closes
    // for the deferred kinds; modal -> modal (including -> TOUCHES editor) keeps
    // its instant swap, and render's open detection scales the new panel IN.
    // DERIVED, not enumerated. This used to consult a hand-kept list of ids, so
    // adding a modal meant remembering to add its number and nothing complained
    // when you forgot — which is exactly how the AFFICHAGE HOME picker ended up
    // opening with a pop and then vanishing on close.
    //
    // The rule is simply what the list was trying to spell: a panel that hands
    // back to a NON-modal screen scales out first. Modal-to-modal keeps its
    // instant swap (the new panel scales itself in), and the destructive
    // confirmations are excluded by `new_screen != screen_copy` on their own,
    // because they arm their own close and never touch `s.screen` here — their
    // mutation has to run AFTER the pop, not before it.
    let new_screen = s.screen;
    if modal_kind(screen_copy) != 0
        && modal_kind(new_screen) == 0
        && new_screen != screen_copy
        && !crate::backend::render::modal_close_active()
    {
        if let Ok(mut p) = PENDING_AFTER_CLOSE.lock() {
            *p = Some(PendingClose::Goto(new_screen));
        }
        s.screen = screen_copy;
        crate::backend::render::modal_close_begin();
    }
    let games_dir = s.pending_games_dir.take();
    drop(s);
    // Queued here, outside the lock; `drive_pending_move` does the work a few
    // entries at a time so the progress bar is a progress bar and not a
    // screenshot taken after the fact.
    if let Some(dest) = games_dir {
        start_move(&dest);
    }
    consumed
}

/// A games-folder move in flight, advanced a few entries per frame.
///
/// Doing the whole sweep inside `input()` was simpler and wrong: the frame that
/// pressed the button is the frame that ran every rename, so the app froze for
/// as long as the move took and the progress bar was only ever drawn once, at
/// 100%, after the freeze. A user cannot tell that apart from a crash. Slicing
/// it lets the bar mean what it says.
struct MoveJob {
    src: std::string::String,
    dest: std::string::String,
    /// Top-level file names, then subfolder names; `idx` walks both in order.
    files: std::vec::Vec<std::string::String>,
    dirs: std::vec::Vec<std::string::String>,
    idx: usize,
    moved: usize,
    /// Entries that stayed in `src`: a file or folder of the same name was
    /// already at the destination, or the rename failed. Told to the player
    /// at the end, not only to the log: part of the library is still behind.
    skipped: usize,
}

static MOVE_JOB: Mutex<Option<MoveJob>> = Mutex::new(None);

/// Where the running move goes from and to, for its progress page. `None`
/// when no move is running: `start_move` refused it, or it has finished.
pub(crate) fn move_paths() -> Option<(std::string::String, std::string::String)> {
    MOVE_JOB.lock().ok().and_then(|g| g.as_ref().map(|j| (j.src.clone(), j.dest.clone())))
}

/// How far the games-folder move has got: (done, total) entries.
///
/// Atomics rather than `State`, because the move runs with the library lock
/// released — that is the whole reason it can be shown at all.
static MOVE_DONE: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
static MOVE_TOTAL: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);

/// Progress of the running move, for the drawing side.
pub(crate) fn move_progress() -> (usize, usize) {
    (
        MOVE_DONE.load(core::sync::atomic::Ordering::Relaxed),
        MOVE_TOTAL.load(core::sync::atomic::Ordering::Relaxed),
    )
}

extern "C" {
    /// The C++ SD scan. Re-run after the games folder changes so the list
    /// reflects the new roots without a trip back to the home menu.
    fn swf_picker_run();
    /// Top-level file names of one directory (see navigator.rs: Rust cannot
    /// list a directory on Horizon without corrupting the names).
    fn swf_picker_list_files(
        dir: *const core::ffi::c_char,
        out: *mut core::ffi::c_char,
        cap: core::ffi::c_int,
    ) -> core::ffi::c_int;
    /// The program ID a game's tile owns, derived from the game's path.
    ///
    /// Derived and not stored: the same game always resolves to the same tile,
    /// so installing twice replaces rather than duplicates, and removing one
    /// needs no file to remember what it was called.
    fn nsp_program_id_for(swf_path: *const core::ffi::c_char) -> u64;
    /// Uninstall a tile: its record, its content and its save slot.
    /// 1 when one was removed.
    fn nsp_remove_tile(program_id: u64) -> core::ffi::c_int;
    /// 1 when a tile for this program ID is installed on the console.
    fn nsp_tile_exists(program_id: u64) -> core::ffi::c_int;
}

/// Top-level file names of `dir`, straight from one `readdir`.
///
/// Was built on the recursive tree walk and filtered afterwards, which meant
/// enumerating every file inside every `<game>.files/` companion tree to obtain
/// the four hundred names at the top — several seconds of frozen screen before
/// a move could even start, growing with the library rather than with the job.
fn list_files_top(dir: &str) -> std::vec::Vec<std::string::String> {
    let mut buf = std::vec![0u8; 256 * 1024];
    let c = std::format!("{}\0", dir);
    let n = unsafe {
        swf_picker_list_files(
            c.as_ptr() as *const core::ffi::c_char,
            buf.as_mut_ptr() as *mut core::ffi::c_char,
            buf.len() as core::ffi::c_int,
        )
    };
    if n <= 0 {
        return std::vec::Vec::new();
    }
    let end = buf.iter().position(|b| *b == 0).unwrap_or(buf.len());
    std::string::String::from_utf8_lossy(&buf[..end])
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| l.to_string())
        .collect()
}

/// Files that must NOT follow the library.
///
/// Every one of these is read from a HARDCODED `sdmc:/flashnx/...` path
/// elsewhere in the app, so moving it does not relocate a setting — it deletes
/// it. Grep for `"sdmc:/flashnx/` before adding a file to the app, and add it
/// here if it turns up.
const MOVE_STAYS: &[&str] = &[
    // The ONLY file that cannot travel: it records where the library went, and a
    // pointer stored inside the thing it points at is unfindable. Everything
    // else the app owns follows the games, because a folder you were told was
    // emptied should be empty — leaving settings behind invites deleting them.
    "games_dir",
];

/// Queue a move of the whole games folder into `dest`.
///
/// Moving the library means moving the WHOLE folder, not the parts this code
/// happens to recognise. Matching games and their sidecars by name looked
/// thorough and was not: the oldest saves carry the movie's URL-encoded name
/// (`Super%20Mario%2063.swf...` beside a game called `Super Mario 63.swf`),
/// nothing claimed them, and 37 of them stayed behind on the first real move —
/// unreachable afterwards, because saves are read from the games folder. Taking
/// everything removes the whole class of "which files did we forget", and means
/// the app never has to read two folders to find one save.
fn start_move(dest: &str) -> bool {
    let src = primary_root();
    if src == dest {
        return false;
    }
    let _ = std::fs::create_dir_all(dest);
    let files = list_files_top(&src);
    let dirs = list_dirs_all(&src);
    MOVE_DONE.store(0, core::sync::atomic::Ordering::Relaxed);
    MOVE_TOTAL.store(files.len() + dirs.len(), core::sync::atomic::Ordering::Relaxed);
    if let Ok(mut g) = MOVE_JOB.lock() {
        *g = Some(MoveJob {
            src,
            dest: dest.to_string(),
            files,
            dirs,
            idx: 0,
            moved: 0,
            skipped: 0,
        });
        true
    } else {
        false
    }
}

/// Move one entry. Returns false once the job is finished.
///
/// A rename inside one card is a directory update — `fsdev_rename` maps to
/// Horizon's `RenameFile`/`RenameDirectory` — so a 300 MB game moves as fast as
/// a 300 KB one, and a slice of a few entries always fits inside a frame.
fn step_move(job: &mut MoveJob) -> bool {
    let n_files = job.files.len();
    if job.idx >= n_files + job.dirs.len() {
        return false;
    }
    let i = job.idx;
    job.idx += 1;
    MOVE_DONE.store(job.idx, core::sync::atomic::Ordering::Relaxed);

    if i < n_files {
        let name = job.files[i].clone();
        if MOVE_STAYS.contains(&name.as_str()) {
            return true;
        }
        let to = std::format!("{}/{}", job.dest, name);
        // NEVER overwrite. A file of the same name in the destination is someone
        // else's — its own game, its own progress — and Horizon's rename gives no
        // portable promise about what it does to it, so this does not find out
        // the hard way. It stays put and gets named in the log.
        if file_exists_now(&to) {
            log(&std::format!("games dir: {} already there, left in place
", name));
            job.skipped += 1;
            return true;
        }
        let from = std::format!("{}/{}", job.src, name);
        if std::fs::rename(&from, &to).is_err() {
            log(&std::format!("games dir: could not move {}
", name));
            job.skipped += 1;
            return true;
        }
        if name.to_ascii_lowercase().ends_with(".swf") {
            job.moved += 1;
        }
        return true;
    }

    // Companion trees, the cover cache, and any other folder the library owns.
    // Every folder goes, `covers/` included: the cover cache is read from
    // beside the library now, so leaving it behind would strand 150 images and
    // make the source folder look stubbornly non-empty.
    let name = job.dirs[i - n_files].clone();
    let to = std::format!("{}/{}", job.dest, name);
    if dir_exists(&to) {
        log(&std::format!("games dir: {}/ already there, left in place
", name));
        job.skipped += 1;
        return true;
    }
    let from = std::format!("{}/{}", job.src, name);
    if std::fs::rename(&from, &to).is_err() {
        log(&std::format!("games dir: could not move {}/
", name));
        job.skipped += 1;
    }
    true
}

/// Advance a queued move, then finish it. Called once per frame from `render`,
/// before the library lock is taken.
fn drive_pending_move() {
    // A slice, not the lot: enough that a big library still moves in a second or
    // two, small enough that the frame this runs in still gets drawn.
    const PER_FRAME: usize = 8;
    // Not while the question is still closing: a small library would be moved
    // before its progress page ever showed, and the page would then open on a
    // move that no longer exists.
    if crate::backend::render::modal_close_active() {
        return;
    }
    let finished = {
        let mut g = match MOVE_JOB.lock() {
            Ok(g) => g,
            Err(_) => return,
        };
        let Some(job) = g.as_mut() else { return };
        let mut alive = true;
        for _ in 0..PER_FRAME {
            alive = step_move(job);
            if !alive {
                break;
            }
        }
        if alive {
            return; // more to do; the bar moves on the next frame
        }
        let done = g.take().expect("checked above");
        (done.src, done.dest, done.moved, done.skipped)
    };
    let (src, dest, moved, skipped) = finished;

    set_primary_root(&dest);
    tiles_follow(&src, &dest, true);
    log(&std::format!(
        "games dir: now {} ({} moved, {} left in {})
",
        dest, moved, skipped, src,
    ));
    // Rebuild the list: `add_path` PUSHES, and the boot scan relies on starting
    // from an empty one, so rescanning in place left every game listed twice —
    // once at an address that no longer exists.
    if let Ok(mut s) = LIBRARY.lock() {
        s.entries.clear();
    }
    unsafe { swf_picker_run() };
    if let Ok(mut s) = LIBRARY.lock() {
        // Every entry was just rebuilt by the scan, which cannot know about
        // the labels: `add_path` reads them, but this is the one rebuild that
        // does not go back through `open()`. Without this the shelves that
        // exist only as labels vanish until the next boot -- and the shelf
        // check below would then judge them gone.
        refresh_tags(&mut s.entries);
        // The open shelf, revalidated against the library as it now stands: the
        // games folder just changed, so a shelf that lived under the old one is
        // no longer scanned and no longer exists.
        if let Some(f) = s.folder_filter.clone() {
            if !f.is_empty() && !s.entries.iter().any(|e| on_shelf(e, &f)) {
                s.folder_filter = None;
                set_active_folder(None);
            }
        }
        // The sort is applied in `open()` and nowhere else, so a list rebuilt
        // mid-session came back in raw scan order and stayed that way until the
        // user opened the sort menu and picked the mode it was already set to.
        let (mode, rev) = (current_sort_mode(), current_sort_reverse());
        sort_entries(&mut s.entries, mode, rev);
        // Back where the user pressed the button, whatever the library holds:
        // nothing else leaves this screen, and it swallows input on purpose, so
        // forgetting this left the app frozen on a finished progress bar.
        s.screen = settings_at(SetSection::Library, "DOSSIER");
        // And how it went, not a silent jump back. What stayed behind matters
        // more than the count, so it is the one said when there is both.
        let u = crate::loc_ui::ui();
        if skipped > 0 {
            let left = crate::loc::fill_count(u.move_left_1, u.move_left_n, skipped as i32)
                .replacen("{}", src.trim_start_matches("sdmc:"), 1);
            set_toast(&mut s, left, TOAST_INFO);
        } else {
            let n = crate::loc_ui::games(s.entries.len());
            set_toast(&mut s, u.lib_moved.replacen("{}", &n, 1), TOAST_OK);
        }
    }
}
/// Climb one level of the browse tree and land the cursor on the folder just
/// left, rather than at the top of the parent.
///
/// Walking out of a folder and being dropped on row zero means finding your
/// place again by eye every single time — and the folder you just came out of
/// is the one thing you are certain to be looking for. Shared by the REMONTER
/// row and by B, which is how most people actually go back up, and by the
/// RÉGLAGES row that opens the browser beside the library, on its folder.
fn browse_go_up(s: &mut State, path: &str) {
    let Some(parent) = browse_parent(path) else {
        return;
    };
    let leaving = path
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or("")
        .to_string();
    let listing = list_dirs(&parent);
    // Rows are CHOISIR, then REMONTER when the parent has a parent of its own,
    // then the listing — hence the offset applied to a listing index.
    let up_next = if browse_parent(&parent).is_some() { 1 } else { 0 };
    let pos = listing.iter().position(|d| *d == leaving);
    let sel = pos.map_or(0, |i| i + 1 + up_next);
    // Scroll the parent's listing so the folder we just left is ON SCREEN.
    // Landing on it is the whole point, and a cursor parked on row 30 of a
    // window showing rows 0-5 is a cursor nobody can see.
    let scroll = pos.map_or(0, |i| {
        i.saturating_sub(GAMES_DIR_VISIBLE - 1)
            .min(listing.len().saturating_sub(GAMES_DIR_VISIBLE))
    });
    s.dir_browse = listing;
    s.dir_path = parent;
    s.screen = Screen::GamesDirPicker { selection: sel, scroll };
}

/// Folder browser of RÉGLAGES > Bibliothèque > Dossier des jeux (#79).
///
/// Row 0 chooses the folder currently shown; the rows below descend into its
/// subfolders. B goes back UP a level rather than out, which is what a file
/// browser is expected to do — leaving happens from the top level, or with `-`.
fn handle_games_dir_input(s: &mut State, button: &str, mut selection: usize, mut scroll: usize) {
    let path = s.dir_path.clone();
    // Rows: 0 = choose this folder, then "up" unless we are at the card root,
    // then one row per subfolder. `up` is a row and not just a B shortcut so
    // that going up is visible rather than folklore.
    let up = if browse_parent(&path).is_some() { 1 } else { 0 };
    let total = 1 + up + s.dir_browse.len();
    match button {
        // A press on an end comes round, a held direction stops there.
        "Up" | "StickLUp" => selection = list_step_wrap(selection, -1, total),
        "Down" | "StickLDown" => selection = list_step_wrap(selection, 1, total),
        "A" => {
            let root = primary_root();
            if selection == 0 {
                // Choosing a folder moves the library into it. There is no
                // "leave them" option: the old folder stays scanned either way,
                // so leaving them changed nothing a user could see except
                // splitting the library across two folders, one of which the app
                // would never write to again — an extra screen, an extra state,
                // and the way the misplaced-saves bug stayed hidden.
                //
                // The library's own folder is no destination: its row says it
                // is the one in use, and A on it does nothing.
                if !settings_tab::same_dir(&path, &root) {
                    // On "Cancel", like every question of the app: this one
                    // renames every file of the library, and a double press of
                    // A must not be how that happens.
                    s.screen = Screen::GamesDirConfirm { selection: 0 };
                }
                return;
            }
            if up == 1 && selection == 1 {
                browse_go_up(s, &path);
                return;
            }
            let Some(child_name) = s.dir_browse.get(selection - 1 - up) else { return };
            let child = settings_tab::child_dir(&path, child_name);
            // A folder inside the library cannot take it: moving it there would
            // rename the folders into themselves. Refused here with the reason,
            // rather than discovered by a move that half fails.
            if settings_tab::inside_library(&child, &root) {
                set_toast(s, crate::loc_ui::ui().dir_inside_lib.to_string(), TOAST_INFO);
                return;
            }
            s.dir_browse = list_dirs(&child);
            s.dir_path = child;
            s.screen = Screen::GamesDirPicker { selection: 0, scroll: 0 };
            return;
        }
        "B" => {
            // Same as the "up" row — including landing on the folder just left,
            // which is the way most people go back up and the reason the fix on
            // the row alone was invisible.
            if browse_parent(&path).is_some() {
                browse_go_up(s, &path);
            } else {
                s.dir_browse.clear();
                s.screen = settings_at(SetSection::Library, "DOSSIER");
            }
            return;
        }
        "Minus" => {
            s.dir_browse.clear();
            s.screen = settings_at(SetSection::Library, "DOSSIER");
            return;
        }
        _ => {}
    }
    // Only the subfolder rows scroll; rows 0 and "up" stay pinned above them.
    let first_scrolling = 1 + up;
    if selection < first_scrolling {
        scroll = 0;
    } else if selection < scroll + first_scrolling {
        scroll = selection - first_scrolling;
    } else if selection >= scroll + first_scrolling + GAMES_DIR_VISIBLE {
        scroll = selection + 1 - first_scrolling - GAMES_DIR_VISIBLE;
    }
    s.screen = Screen::GamesDirPicker { selection, scroll };
}

/// Display order of the bug-report picker: last played first, whatever sort the
/// library itself is on (#99). You report the game you have just been playing,
/// so it belongs at the top of that list even when the library is showing A-Z.
/// Returns positions into `entries`, so nothing is reordered behind the screen —
/// the picker's row N is `bug_picker_order(..)[N]`, and both the drawing and the
/// A press have to read it the same way or they point at two different games.
///
/// Favourites are NOT pinned here, unlike `sort_entries`: a starred game is not
/// more likely to be the one you are reporting.
fn bug_picker_order(entries: &[Entry]) -> std::vec::Vec<usize> {
    let mut order: std::vec::Vec<usize> = (0..entries.len()).collect();
    order.sort_by(|&a, &b| {
        let (ea, eb) = (&entries[a], &entries[b]);
        crate::playtime::get_last(&eb.basename)
            .cmp(&crate::playtime::get_last(&ea.basename))
            .then_with(|| {
                ea.display_name
                    .to_lowercase()
                    .cmp(&eb.display_name.to_lowercase())
            })
    });
    order
}

/// BugPicker > A: snapshot the chosen game's metadata, open swkbd for a
/// description, then POST the report to the relay. Hoisted from `input()` — the
/// keyboard + synchronous HTTPS POST must NOT run under the LIBRARY lock.
///
/// `row` is a row of the picker, not a position in `entries`: the two differ
/// since the picker has its own order.
fn run_bug_report_flow(row: usize) {
    // Snapshot the game's technical info under the lock, then release it.
    let base = match LIBRARY.lock() {
        Ok(g) => bug_picker_order(&g.entries)
            .get(row)
            .and_then(|&i| g.entries.get(i))
            .map(|e| {
                (
                    e.display_name.clone(),
                    e.basename.clone(),
                    e.path.clone(),
                    e.size_bytes,
                    e.swf_version,
                    e.compression_label,
                    e.is_as3,
                )
            }),
        Err(_) => None,
    };
    let Some((game, file, path, size, swf_version, compression, as3)) = base else {
        return;
    };
    // Source URL recorded at import time in a `<game>.swf.url` sidecar (direct,
    // archive.org or Flashpoint). Empty for hand-copied files and games imported
    // before this existed — lets a report name where an arbitrarily-named game
    // (e.g. `7k7k7k.swf`) came from.
    let source_url = read_sd_text(&std::format!("{}.url", path), 4096)
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    // Both size numbers, kept apart. `Entry::size_bytes` is the `.filesize`
    // footprint when the game has one and the SWF header's `file_length`
    // otherwise, and the issue never said which: #112 showed 3.9 MB (a header)
    // next to #110's 3.38 GB (a card footprint).
    //
    // Free for a single-file game, whose `size` already IS the header length.
    // Only a game whose number was replaced by the footprint pays the re-open,
    // and that is an 8-byte read plus at most a 64-byte inflate, not a rescan.
    let disk_size = read_filesize_cache(&path).unwrap_or(0);
    let header_size = if disk_size == 0 {
        size
    } else {
        read_swf_header(&path).map(|h| h.size_bytes).unwrap_or(0)
    };
    // Shape of the companion folder, read at its ROOT only.
    //
    // Two numbers rather than one because `swf_picker_count_companions` counts
    // flat `.swf` files and skips directories, while a Flashpoint GameZIP writes
    // `<stem>.files/<host>/<path>/...` — so on its own it reports 0 for a
    // healthy multi-file game and would flag every one of them as broken.
    //
    // -1 is "no companion folder at all", which is what separates a plain
    // single-file game from #112: there the folder was present and EMPTY and the
    // game sat on its loading screen forever. `extract_gamezip_tree` never
    // leaves an empty one behind (it creates parent dirs on demand), so 0/0 is
    // always a half-written or half-lost install.
    //
    // Root level, deliberately: `swf_picker_files_dir_size` walks the whole tree,
    // which is seconds on Super Smash Flash 2's 1474 files. Sending a bug report
    // must not feel like the app has hung.
    let files_dir = crate::sidecar_dir_for(Some(&path))
        .to_string_lossy()
        .into_owned();
    let (companions, companion_dirs) = if dir_exists(&files_dir) {
        let mut p = path.as_bytes().to_vec();
        p.push(0);
        let flat =
            unsafe { swf_picker_count_companions(p.as_ptr() as *const core::ffi::c_char) };
        (flat.max(0), list_dirs_all(&files_dir).len() as i32)
    } else {
        (-1, -1)
    };
    // The OVERCLOCK that was in force for THIS game, per-game value already
    // folded with the REGLAGES default. #109 arrived as a performance report with
    // no way to tell whether the boost was on.
    //
    // NOT `overclock_refused()`: that flag is session-global and belongs to the
    // last raise that was ASKED FOR, whichever game that was, so pinning it on
    // this one would be wrong with the appearance of right.
    let power_mode = keymap::power_mode_for(&file);
    // And whether the console actually granted it: the setting and what ran are
    // not the same fact when the battery veto fires.
    let power_refused = crate::backend::render::overclock_ever_refused();
    // On AUTO the setting says nothing about the clock the game ran at: how much
    // of its AUTO time it spent raised. Same scope as `overclock_ever_refused`,
    // the last game launched.
    let power_auto_pct = crate::clock_auto::high_share_pct();
    // Description is optional — cancel (None) aborts the whole report.
    let Some(description) = net::prompt_bug() else {
        return;
    };
    let applet = unsafe { ruffle_is_applet_mode() } != 0;
    let docked = unsafe { ruffle_is_docked() } != 0;
    let report = crate::bugreport::Report {
        kind: "bug",
        game,
        file,
        source_url,
        size: header_size,
        disk_size,
        swf_version,
        compression: compression.to_string(),
        as3,
        app_version: crate::bugreport::APP_VERSION,
        lang: crate::loc::current().code(),
        applet,
        companions,
        companion_dirs,
        power_mode,
        power_refused,
        power_auto_pct,
        docked,
        description: description.trim().to_string(),
    };
    // The bug row: the game list this came through has no other way in.
    settings_tab::submit_report(report, "BUG");
}

/// The bug report's game list (RÉGLAGES > Aide et retours > Signaler un bug),
/// inside the pane. A (hoisted) describes + sends, B goes back to the row that
/// opened it, L and R turn a page. `selection` / `scroll` are rows of
/// `bug_picker_order`.
fn handle_bug_picker_input(s: &mut State, button: &str, mut selection: usize, mut scroll: usize) {
    let total = s.entries.len();
    match button {
        // A press on an end comes round, a held direction stops there: in a
        // library of three hundred games, holding Down has to be able to park on
        // the last one.
        "Up" | "StickLUp" => selection = list_step_wrap(selection, -1, total),
        "Down" | "StickLDown" => selection = list_step_wrap(selection, 1, total),
        // A screenful at a time, stopping at the ends.
        "L" => selection = selection.saturating_sub(BUG_PICKER_VISIBLE_ROWS),
        "R" => selection = (selection + BUG_PICKER_VISIBLE_ROWS).min(total.saturating_sub(1)),
        "B" | "Minus" => {
            s.screen = settings_at(SetSection::Help, "BUG");
            return;
        }
        // A is hoisted in input() (swkbd + HTTPS POST run without the lock).
        _ => return,
    }
    scroll = clamp_scroll(scroll, selection, BUG_PICKER_VISIBLE_ROWS);
    s.screen = Screen::BugPicker { selection, scroll_offset: scroll };
}

/// "Move the whole library here?": 0 = cancel, where it opens, 1 = move. The
/// work itself is recorded and run outside the lock, like every other action
/// here that touches the SD.
fn handle_games_dir_confirm_input(s: &mut State, button: &str, selection: usize) {
    match button {
        // Left is the first answer, Right the second: a held direction stays
        // where it points instead of flipping between the two.
        "Left" | "StickLLeft" => s.screen = Screen::GamesDirConfirm { selection: 0 },
        "Right" | "StickLRight" => s.screen = Screen::GamesDirConfirm { selection: 1 },
        "A" => {
            if selection == 1 {
                s.pending_games_dir = Some(s.dir_path.clone());
                s.dir_browse.clear();
                s.screen = Screen::GamesDirMoving;
            } else {
                s.screen = Screen::GamesDirPicker { selection: 0, scroll: 0 };
            }
        }
        "B" | "Minus" => s.screen = Screen::GamesDirPicker { selection: 0, scroll: 0 },
        _ => {}
    }
}

/// OPTIONS > DOSSIER > NOUVEAU DOSSIER (issue #68): name a shelf and put this
/// game on it in one gesture. Hoisted out of the handler — swkbd blocks, and the
/// library lock must not be held across it.
fn run_new_game_folder_flow(game_idx: usize) {
    let lc = crate::loc::s();
    let Some(name) = net::prompt_with(lc.games_dir_new, lc.kbd_newdir_guide, "") else {
        return;
    };
    // Separators would silently build a tree, or walk out of the games folder
    // entirely. Same guard as the games-folder browser's own new-folder flow.
    let safe: std::string::String = name
        .chars()
        .filter(|c| !matches!(*c, '/' | ':' | '\\'))
        .collect();
    let safe = safe.trim().to_string();
    if safe.is_empty() {
        return;
    }
    // A name the SCAN refuses to walk would swallow the game whole: `covers` and
    // anything ending `.files` are skipped as our own bookkeeping, and a name of
    // dots is the parent directory however many separators were filtered out of
    // it. The game would move somewhere nothing lists, with no way back.
    if is_our_bookkeeping(&safe) || safe.chars().all(|c| c == '.') {
        log(&std::format!("library: folder name {} refused\n", safe));
        // Said as what it is: a reserved name, not a card that failed.
        if let Ok(mut s) = LIBRARY.lock() {
            set_toast(&mut s, crate::loc_ui::ui().folder_name_reserved.to_string(), TOAST_INFO);
        }
        return;
    }
    if let Ok(mut s) = LIBRARY.lock() {
        // A name that is ALREADY one of this game's shelves would be toggled
        // OFF by `apply_game_folder`, because A is a bascule now. A row called
        // "new shelf" that quietly takes the game off one is the opposite of
        // what it says, so it is refused here instead.
        let already = s
            .entries
            .get(game_idx)
            .is_some_and(|e| on_shelf(e, &safe));
        if already {
            set_toast(&mut s, crate::loc::s().toast_folder_same.to_string(), TOAST_ERR);
        } else {
            apply_game_folder(&mut s, game_idx, &safe);
        }
        // `apply_game_folder` already lands on the picker and has re-found the
        // game's index; only the cursor row is ours to set, onto the shelf that
        // was just created.
        let row = folder_counts(&s.entries)
            .iter()
            .position(|(n, _)| *n == safe)
            .map_or(0, |i| i + 1);
        if let Screen::GameFolderPicker { game_idx: idx, .. } = s.screen {
            s.screen = Screen::GameFolderPicker { game_idx: idx, selection: row };
        }
    }
}

/// Create a subfolder in the folder the browser is showing (#79).
///
/// Saves a trip to a file manager for the common case: someone laying their
/// card out as `roms/flashnx` can make that folder from inside the browser
/// that is asking them to choose it.
fn run_new_folder_flow() {
    let here = match LIBRARY.lock() {
        Ok(s) => s.dir_path.clone(),
        Err(_) => return,
    };
    // In the library or one of its folders, the new folder could not take the
    // library either, and would only be written to the card to sit greyed out
    // under the cursor. The footer offers no X there; a press says why, before
    // the keyboard asks for a name that leads nowhere.
    if settings_tab::within_library(&here, &primary_root()) {
        if let Ok(mut s) = LIBRARY.lock() {
            set_toast(&mut s, crate::loc_ui::ui().dir_new_in_lib.to_string(), TOAST_INFO);
        }
        return;
    }
    crate::net::log(&std::format!("games dir: new folder in {}
", here));
    let lc = crate::loc::s();
    let Some(name) = net::prompt_with(lc.games_dir_new, lc.kbd_newdir_guide, "") else {
        crate::net::log("games dir: new folder cancelled or keyboard unavailable
");
        return;
    };
    // Separators would silently create a tree, or escape the folder entirely.
    let safe: std::string::String = name
        .chars()
        .filter(|c| !matches!(*c, '/' | ':' | '\\'))
        .collect();
    let safe = safe.trim();
    if safe.is_empty() {
        return;
    }
    let path = if here.ends_with('/') {
        std::format!("{}{}", here, safe)
    } else {
        std::format!("{}/{}", here, safe)
    };
    if let Err(e) = std::fs::create_dir_all(&path) {
        crate::net::log(&std::format!("games dir: could not create {}: {}
", path, e));
        return;
    }
    // Show it straight away, with the cursor on it: the point of creating a
    // folder here is to then choose it.
    if let Ok(mut s) = LIBRARY.lock() {
        s.dir_browse = list_dirs(&here);
        let idx = s.dir_browse.iter().position(|d| d == safe);
        let up = if browse_parent(&here).is_some() { 1 } else { 0 };
        let selection = idx.map(|i| i + 1 + up).unwrap_or(0);
        // Scrolled so the new folder is on screen: a cursor on a row of the
        // listing that the window does not show is a cursor nobody can see.
        let len = s.dir_browse.len();
        let scroll = idx.map_or(0, |i| {
            i.saturating_sub(GAMES_DIR_VISIBLE - 1).min(len.saturating_sub(GAMES_DIR_VISIBLE))
        });
        s.screen = Screen::GamesDirPicker { selection, scroll };
    }
}

/// TOUCHES sub-menu SHARE (#20): open the share confirm showing a before/after of
/// the player's EXISTING shared profile (if any) -> the controls about to be sent,
/// so it's clear sharing UPDATES the player's one slot, not piles up. Hoisted
/// (network + file I/O). If the controls already come from the catalog unchanged,
/// flashes an info toast instead (nothing to share until edited).
fn run_open_share_confirm_flow(game_idx: usize) {
    let snap = match LIBRARY.lock() {
        Ok(g) => g
            .entries
            .get(game_idx)
            .map(|e| (e.basename.clone(), e.display_name.clone(), e.path.clone())),
        Err(_) => None,
    };
    let Some((basename, title, path)) = snap else {
        return;
    };
    // Already a catalog profile, unchanged (applied or already shared) → nothing
    // new to send until a key is edited.
    if keymap::provenance(&basename).starts_with("community:") {
        if let Ok(mut s) = LIBRARY.lock() {
            set_toast(&mut s, crate::loc::s().profile_share_dup.to_string(), TOAST_INFO);
            goto_touches_menu(&mut s, game_idx, 2);
        }
        return;
    }
    let current = keymap::effective_for(&basename);
    let swf_hash = crate::profiles::swf_hash_of(&path).unwrap_or_default();
    let suffix = std::format!("-{}", crate::profiles::install_id());
    // My existing shared profile for this game (catalog id ends with my install
    // suffix), if any. The confirm shows its bindings -> my current ones.
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
    let rows = cap_preview_rows(rows);
    if let Ok(mut s) = LIBRARY.lock() {
        s.preview_rows = rows;
        s.share_is_update = is_update;
        s.screen = Screen::ProfileShareConfirm { game_idx };
    }
}

/// Confirmed SHARE (#20): send the game's current controls as a community
/// profile. Hoisted out of `input()` — the HTTPS POST must not run under the
/// LIBRARY lock. Flashes a toast (green/red) and returns to the sub-menu.
fn run_share_profile_flow(game_idx: usize) {
    let snap = match LIBRARY.lock() {
        Ok(g) => g
            .entries
            .get(game_idx)
            .map(|e| (e.display_name.clone(), e.basename.clone(), e.path.clone())),
        Err(_) => None,
    };
    let Some((title, basename, path)) = snap else {
        return;
    };
    // What we share = the game's EFFECTIVE controls (its sidecar, else the
    // defaults) + a content hash of the .swf as the match key. The install id
    // (server-side) already keeps this to ONE slot per device per game, so a
    // re-share UPDATES your own profile rather than piling up duplicates — no
    // client-side content de-dup needed (it was rejecting legit variants).
    let km = keymap::effective_for(&basename);
    let swf_hash = crate::profiles::swf_hash_of(&path).unwrap_or_default();
    let cursor_speed = keymap::cursor_speed_for(&basename);
    let (ok, msg) = match crate::profiles::share(&title, "", &swf_hash, &km, cursor_speed) {
        Ok(id) => {
            // Tag the local keymap as catalog profile <id>: blocks a pointless
            // re-share of the unchanged controls and marks it active in the
            // picker. Editing a key flips it back to "user" (shareable again).
            keymap::mark_shared(&basename, &id);
            // Drop the cached catalog so the picker re-fetches and shows the
            // just-shared profile without relaunching (GitHub's CDN may still
            // lag a minute or two server-side).
            crate::profiles::invalidate_online_cache();
            (true, crate::loc::s().profile_shared_ok.to_string())
        }
        Err(e) => (false, e),
    };
    if let Ok(mut s) = LIBRARY.lock() {
        set_toast(&mut s, msg, if ok { TOAST_OK } else { TOAST_ERR });
        goto_touches_menu(&mut s, game_idx, 2); // back to the SHARE row
    }
}

/// OPTIONS > APPLIQUER: compute the community profiles matching a game and open
/// the picker (#20). Hoisted — hashing the .swf is file I/O we keep out of the
/// LIBRARY lock. Always opens the picker (which shows a notice when empty).
fn run_open_profiles_flow(game_idx: usize) {
    let snap = match LIBRARY.lock() {
        Ok(g) => g
            .entries
            .get(game_idx)
            .map(|e| (e.display_name.clone(), e.basename.clone(), e.path.clone())),
        Err(_) => None,
    };
    let Some((title, basename, path)) = snap else {
        return;
    };
    let swf_hash = crate::profiles::swf_hash_of(&path).unwrap_or_default();
    // fp_uuid not persisted yet (Phase 1b) → match by hash + title for now.
    // all_matches_for merges the bundled catalog with the online one (network).
    let matches = crate::profiles::all_matches_for("", &swf_hash, &title);
    // Tag the ACTIVE row dynamically: the profile whose bindings EXACTLY match the
    // game's current controls (P1 + P2). Content-based so it's right even if the
    // provenance tag was lost; the provenance id is a fallback (covers bundled
    // partial profiles whose merged keymap differs from the stored bindings).
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
        "profiles: APPLIQUER '{}' hash={} -> {} match(es), active='{}'\n",
        title,
        swf_hash,
        matches.len(),
        active_id,
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
    if let Ok(mut s) = LIBRARY.lock() {
        // Every profile, then the COPY FROM ANOTHER GAME row. The picker used
        // to keep ten (its modal did not scroll and grew 52 px a row); it now
        // shows a scrolling window (`draw_library_list_modal_window`).
        s.profile_matches = crate::profiles::with_copy_row(matches);
        s.profile_copy_mode = false;
        s.profile_saved_matches.clear();
        s.active_profile_id = active_id;
        if let Some(msg) = notice {
            set_toast(&mut s, msg.to_string(), TOAST_INFO);
        }
        s.screen = Screen::ProfileList { game_idx, selection: 0 };
    }
}

/// COPY FROM ANOTHER GAME: list the player's other games with controls of
/// their own, in place of the community profiles (same picker, preview and
/// apply). Hoisted: it reads every candidate's keymap off SD. `row` is the copy
/// row, where the cursor stays when there is nothing to copy.
fn run_open_copies_flow(game_idx: usize, row: usize) {
    let snap = match LIBRARY.lock() {
        Ok(g) => g.entries.get(game_idx).map(|e| (e.basename.clone(), e.display_name.clone())),
        Err(_) => None,
    };
    let Some((basename, title)) = snap else {
        return;
    };
    // `local_copies` takes the LIBRARY lock itself (owned_games): not held here.
    let copies = crate::profiles::local_copies(&basename, &title);
    crate::net::log(&std::format!(
        "profiles: COPY FROM ANOTHER GAME '{}' -> {} game(s)\n",
        title,
        copies.len(),
    ));
    if let Ok(mut s) = LIBRARY.lock() {
        if copies.is_empty() {
            set_toast(&mut s, crate::loc::s().profile_copy_none.to_string(), TOAST_INFO);
            s.screen = Screen::ProfileList { game_idx, selection: row };
            return;
        }
        s.profile_saved_matches = std::mem::replace(&mut s.profile_matches, copies);
        s.profile_copy_mode = true;
        s.screen = Screen::ProfileList { game_idx, selection: 0 };
    }
}

/// Input for the community-profile picker (#20). Rows = matched profiles only
/// (revert moved to the TOUCHES sub-menu). A opens the before/after preview
/// (hoisted — it reads the current keymap off SD); B returns to the sub-menu.
fn handle_profile_list_input(s: &mut State, button: &str, game_idx: usize, mut selection: usize) {
    let n = s.profile_matches.len();
    // At least one row (the "no profile" notice) so nav has somewhere to sit.
    let total = n.max(1);
    match button {
        "Up" | "StickLUp" => selection = list_step_wrap(selection, -1, total),
        "Down" | "StickLDown" => selection = list_step_wrap(selection, 1, total),
        // "A" (open preview) is hoisted to `run_open_preview_flow` — it reads the
        // current keymap file to build the diff, which we keep out of this lock.
        "X" => {
            // Delete MY OWN shared profile: only on a row this install shared.
            if s
                .profile_matches
                .get(selection)
                .is_some_and(|m| crate::profiles::is_mine(&m.profile.id))
            {
                s.screen = Screen::ProfileDeleteConfirm { game_idx, profile_idx: selection, selection: 0 };
                return;
            }
        }
        "B" | "Minus" => {
            // From the copy list, back to the community list, on the copy row.
            if s.profile_copy_mode {
                s.profile_copy_mode = false;
                s.profile_matches = std::mem::take(&mut s.profile_saved_matches);
                let last = s.profile_matches.len().max(1) - 1;
                s.screen = Screen::ProfileList { game_idx, selection: last };
                return;
            }
            goto_touches_menu(s, game_idx, 1); // back to the sub-menu's APPLY row
            return;
        }
        _ => {}
    }
    s.screen = Screen::ProfileList { game_idx, selection };
}

/// A-on-confirm (#20): DELETE one of my own shared profiles via the relay.
/// Hoisted — the HTTPS POST must not run under the LIBRARY lock. Toasts the
/// result and drops the row from the picker on success.
fn run_delete_profile_flow(game_idx: usize, profile_idx: usize) {
    let id = match LIBRARY.lock() {
        Ok(g) => g.profile_matches.get(profile_idx).map(|m| m.profile.id.clone()),
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
        // The deleted profile is no longer in the catalog: if this game's keymap
        // still carries its tag, demote it to "user" so SHARE works again (it was
        // wrongly saying "already in the catalog" after a delete).
        if let Some(basename) = LIBRARY
            .lock()
            .ok()
            .and_then(|g| g.entries.get(game_idx).map(|e| e.basename.clone()))
        {
            keymap::unmark_shared(&basename, &id);
        }
    }
    if let Ok(mut s) = LIBRARY.lock() {
        set_toast(&mut s, msg, kind);
        if ok {
            s.profile_matches.retain(|m| m.profile.id != id);
        }
        let n = s.profile_matches.len();
        let selection = if n == 0 { 0 } else { profile_idx.min(n - 1) };
        s.screen = Screen::ProfileList { game_idx, selection };
    }
}

/// A on a profile row (#20): build the before/after diff and open the preview.
/// Hoisted — `keymap::effective_for` reads the SD sidecar, kept out of the lock.
/// Each diff line is "<button>: <mine> -> <profile>" for the keys that change,
/// across both players (P2 rows tagged "P2 "). Shared profiles carry every button
/// (they're made from the fully-merged effective keymap), so iterating the
/// profile's bindings is a faithful "what this will set".
fn run_open_preview_flow(game_idx: usize, profile_idx: usize) {
    let snap = match LIBRARY.lock() {
        Ok(g) => g.entries.get(game_idx).map(|e| e.basename.clone()).and_then(|b| {
            g.profile_matches.get(profile_idx).map(|m| (b, m.profile.clone()))
        }),
        Err(_) => None,
    };
    let Some((basename, profile)) = snap else {
        return;
    };
    let current = keymap::effective_for(&basename);
    let mut rows = keymap::binding_diff_rows(&current, &profile.to_keymap());
    if let Some(r) = keymap::cursor_diff_row(keymap::cursor_speed_for(&basename), profile.cursor_speed) {
        rows.push(r);
    }
    // Nothing differs → don't open a preview that offers "APPLY" for a no-op.
    // Flash a neutral toast and stay on the picker (e.g. already-active profile).
    if rows.is_empty() {
        if let Ok(mut s) = LIBRARY.lock() {
            set_toast(&mut s, crate::loc::s().profile_preview_none.to_string(), TOAST_INFO);
            s.screen = Screen::ProfileList { game_idx, selection: profile_idx };
        }
        return;
    }
    if let Ok(mut s) = LIBRARY.lock() {
        s.preview_rows = cap_preview_rows(rows);
        s.screen = Screen::ProfilePreview { game_idx, profile_idx };
    }
}

/// TOUCHES sub-menu REVERT (#20): build the before/after of a revert and open its
/// preview (don't revert yet). Hoisted — reads the keymap + backup off SD.
fn run_open_revert_preview_flow(game_idx: usize) {
    let basename = match LIBRARY.lock() {
        Ok(g) => g.entries.get(game_idx).map(|e| e.basename.clone()),
        Err(_) => None,
    };
    let Some(basename) = basename else {
        return;
    };
    let current = keymap::effective_for(&basename);
    let target = keymap::revert_target(&basename);
    let rows = cap_preview_rows(keymap::binding_diff_rows(&current, &target));
    if let Ok(mut s) = LIBRARY.lock() {
        s.preview_rows = rows; // may be empty — revert still clears the active tag
        s.screen = Screen::RevertPreview { game_idx };
    }
}

/// Cap a diff list so the auto-sized modal still fits the screen, noting overflow.
fn cap_preview_rows(mut rows: std::vec::Vec<std::string::String>) -> std::vec::Vec<std::string::String> {
    const MAX_PREVIEW_ROWS: usize = 8;
    if rows.len() > MAX_PREVIEW_ROWS {
        let extra = rows.len() - MAX_PREVIEW_ROWS;
        rows.truncate(MAX_PREVIEW_ROWS);
        rows.push(std::format!("(+{} ...)", extra));
    }
    rows
}

/// A on the preview (#20): actually apply the profile. Hoisted so the popularity
/// counter POST runs outside the LIBRARY lock. Apply is non-destructive (it backs
/// up a hand-made keymap first), then we land on a result toast.
fn run_profile_apply_flow(game_idx: usize, profile_idx: usize) {
    let snap = match LIBRARY.lock() {
        Ok(g) => g.entries.get(game_idx).map(|e| e.basename.clone()).and_then(|b| {
            g.profile_matches.get(profile_idx).map(|m| (b, m.profile.clone()))
        }),
        Err(_) => None,
    };
    let Some((basename, profile)) = snap else {
        return;
    };
    let ok = crate::profiles::apply(&basename, &profile);
    if ok {
        // Best-effort popularity bump (network; failures ignored).
        crate::profiles::record_applied(&profile.id);
    }
    let lc = crate::loc::s();
    let msg = if ok { lc.profile_applied_ok } else { lc.bug_fail_title };
    if let Ok(mut s) = LIBRARY.lock() {
        set_toast(&mut s, msg.to_string(), if ok { TOAST_OK } else { TOAST_ERR });
        goto_touches_menu(&mut s, game_idx, 1); // back to the sub-menu (APPLY row)
    }
}

// Toast kinds (#20): colour of the transient banner. See `set_toast`.
const TOAST_OK: u8 = 0; // green
const TOAST_ERR: u8 = 1; // red
const TOAST_INFO: u8 = 2; // blue
/// How long a toast stays up (~2.5 s at 60 fps).
const TOAST_FRAMES: u32 = 150;

/// Flash a small non-blocking toast over the current screen instead of switching
/// to a full-screen "thanks" modal (#20). Drawn + counted down in `render`.
fn set_toast(s: &mut State, msg: std::string::String, kind: u8) {
    if kind == TOAST_ERR {
        crate::sfx::play(crate::sfx::Sfx::Error);
    }
    s.toast_msg = msg;
    s.toast_kind = kind;
    s.toast_frames = TOAST_FRAMES;
}

/// Enter the TOUCHES sub-menu for a game (#20 regroup), recomputing whether the
/// revert row applies + which label it needs. Called from OPTIONS > TOUCHES and
/// from every return into the sub-menu, so revert availability is always fresh.
/// `selection` is clamped to the rows actually present.
fn goto_touches_menu(s: &mut State, game_idx: usize, selection: usize) {
    let (has_backup, can_revert) = s
        .entries
        .get(game_idx)
        .map(|e| (keymap::has_backup(&e.basename), keymap::has_revert(&e.basename)))
        .unwrap_or((false, false));
    s.touches_has_backup = has_backup;
    // Show "revert" CONTENT-based: exactly when the game's effective controls
    // differ from what a revert would restore (backup, else global default). So it
    // appears as soon as a key really changes and HIDES once you set it back — no
    // longer a stale flag that lingered just because a sidecar file existed. Label
    // adapts: restore my hand-made keys if a backup exists, else reset to default.
    s.touches_can_revert = can_revert;
    // Snapshot the per-game cursor speed for the cursor row (read once here, not
    // every render frame).
    s.touches_cursor_idx = s
        .entries
        .get(game_idx)
        .map(|e| keymap::cursor_speed_for(&e.basename))
        .unwrap_or(-1);
    s.touches_show_cursor = s
        .entries
        .get(game_idx)
        .map(|e| keymap::show_cursor_for(&e.basename))
        .unwrap_or(true);
    s.touches_gyro = s
        .entries
        .get(game_idx)
        .map_or(keymap::GYRO_OFF, |e| keymap::gyro_mode_for(&e.basename));
    let row_count = TOUCHES_MENU_FIXED_ROWS + s.touches_can_revert as usize;
    s.screen = Screen::TouchesMenu { game_idx, selection: selection.min(row_count - 1) };
}

/// Fixed TOUCHES sub-menu rows: edit (0), apply (1), share (2), cursor speed (3),
/// show-cursor toggle (4), gyroscope cursor (5). A revert row (6) is appended
/// when `touches_can_revert`.
const TOUCHES_MENU_FIXED_ROWS: usize = 6;

/// Cursor-speed presets as x10 multipliers (`loc::CURSOR_SPEED_X10`).
const CURSOR_X10: &[u32] = crate::loc::CURSOR_SPEED_X10;

/// The preset a game's Touches page steps from: its own, or, when it has none,
/// the global one it follows (the C++ side's live value, which in the launcher
/// IS the global one). Stepping from x1.0 instead made Right go SLOWER for a
/// game following a global x2.0.
fn touches_speed_idx(own: i32) -> i32 {
    if own >= 0 {
        return own.min(CURSOR_X10.len() as i32 - 1);
    }
    let m = unsafe { ruffle_cursor_speed_mult_x10() }.max(0) as u32;
    CURSOR_X10.iter().position(|v| *v == m).unwrap_or(1) as i32
}

/// Input for the TOUCHES sub-menu (#20). Edit (0) + cursor speed (3) + show-cursor
/// toggle (4) are handled here; apply (1) / share (2) / revert (5) are hoisted in
/// `input()` (file I/O / network). B returns to OPTIONS on the TOUCHES row.
fn handle_touches_menu_input(s: &mut State, button: &str, game_idx: usize, mut selection: usize) {
    let row_count = TOUCHES_MENU_FIXED_ROWS + s.touches_can_revert as usize;
    let last = row_count - 1;
    // The three value rows step with Left and Right too, either way, as the
    // same rows do in the pause menu.
    let dir: i32 = match button {
        "Left" | "StickLLeft" => -1,
        "Right" | "StickLRight" => 1,
        _ => 0,
    };
    if dir != 0 && (3..=5).contains(&selection) {
        // Along what the row shows, stopping at the ends: the speed one preset
        // down or up, the switch off or on, the gyroscope one setting along.
        if let Some(basename) = s.entries.get(game_idx).map(|e| e.basename.clone()) {
            match selection {
                3 => {
                    let n = CURSOR_X10.len() as i32;
                    let cur = touches_speed_idx(s.touches_cursor_idx);
                    let next = (cur + dir).clamp(0, n - 1);
                    if next != cur {
                        keymap::set_cursor_speed_for(&basename, next);
                        s.touches_cursor_idx = next;
                        crate::sfx::value_changed();
                    }
                }
                4 => {
                    if s.touches_show_cursor != (dir > 0) {
                        keymap::toggle_show_cursor_for(&basename);
                        s.touches_show_cursor = !s.touches_show_cursor;
                        crate::sfx::value_changed();
                    }
                }
                _ => {
                    let cur = s.touches_gyro;
                    if (dir < 0 && cur > 0) || (dir > 0 && cur + 1 < keymap::GYRO_MODE_COUNT) {
                        s.touches_gyro = keymap::step_gyro_for(&basename, dir);
                        crate::sfx::value_changed();
                    }
                }
            }
        }
        s.screen = Screen::TouchesMenu { game_idx, selection };
        return;
    }
    match button {
        "Up" | "StickLUp" => selection = list_step_wrap(selection, -1, last + 1),
        "Down" | "StickLDown" => selection = list_step_wrap(selection, 1, last + 1),
        "A" => {
            // EDIT (0) + CURSOR SPEED (3) + SHOW-CURSOR (4) are local (file I/O
            // only). Apply (1) / share (2) / revert (5) need network → hoisted in
            // `input()`.
            if selection == 0 {
                // Init the keymap for THIS game so current_binding / set_binding
                // land in the right sidecar, then open the editor, which names
                // the game in its header.
                let name = s.entries.get(game_idx).map(|e| e.display_name.clone()).unwrap_or_default();
                if let Some(entry) = s.entries.get(game_idx) {
                    keymap::init_for_swf(&entry.basename);
                }
                menu::open(Some(&name));
                s.screen = Screen::TouchesEditor { game_idx };
                return;
            }
            if selection == 3 {
                // Cycle the per-game cursor speed in place (persist to the game's
                // `<basename>.cursor`; applied next launch). Unset starts from the
                // global speed the game was following.
                let cur = touches_speed_idx(s.touches_cursor_idx);
                let next = ((cur as usize + 1) % CURSOR_X10.len()) as i32;
                if let Some(basename) = s.entries.get(game_idx).map(|e| e.basename.clone()) {
                    keymap::set_cursor_speed_for(&basename, next);
                }
                s.touches_cursor_idx = next;
                s.screen = Screen::TouchesMenu { game_idx, selection };
                return;
            }
            if selection == 4 {
                // Toggle the per-game "show cursor" flag (persists in the keymap;
                // applied next launch). Snapshot flips so the row label updates now.
                if let Some(basename) = s.entries.get(game_idx).map(|e| e.basename.clone()) {
                    keymap::toggle_show_cursor_for(&basename);
                    s.touches_show_cursor = !s.touches_show_cursor;
                }
                s.screen = Screen::TouchesMenu { game_idx, selection };
                return;
            }
            if selection == 5 {
                // Gyroscope cursor (#119), per game, applied next launch.
                if let Some(basename) = s.entries.get(game_idx).map(|e| e.basename.clone()) {
                    s.touches_gyro = keymap::cycle_gyro_for(&basename);
                }
                s.screen = Screen::TouchesMenu { game_idx, selection };
                return;
            }
        }
        "B" | "Minus" => {
            s.screen = Screen::OptionsModal { game_idx, selection: opt_row("TOUCHES") };
            return;
        }
        _ => {}
    }
    s.screen = Screen::TouchesMenu { game_idx, selection };
}

/// TOUCHES sub-menu revert (#20). Hoisted to mirror the other profile actions
/// (keeps file I/O off the snappy path). Restores a hand-made backup if present,
/// else drops the applied profile back to the default controls.
fn run_touches_revert(game_idx: usize) {
    let basename = match LIBRARY.lock() {
        Ok(g) => g.entries.get(game_idx).map(|e| e.basename.clone()),
        Err(_) => None,
    };
    let Some(basename) = basename else {
        return;
    };
    let ok = keymap::revert_profile(&basename);
    let lc = crate::loc::s();
    let msg = if ok { lc.profile_reverted_ok } else { lc.bug_fail_title };
    if let Ok(mut s) = LIBRARY.lock() {
        set_toast(&mut s, msg.to_string(), if ok { TOAST_OK } else { TOAST_ERR });
        goto_touches_menu(&mut s, game_idx, 0); // back to the sub-menu
    }
}

fn handle_list_input(s: &mut State, button: &str, mut selection: usize, mut scroll: usize) {
    // `selection` / `scroll` index the FILTERED view (== the full list when no
    // filter is set). Map back to the absolute `entries` index via `filtered`
    // for actions that touch a specific game (play / options).
    let filtered = local_filtered_indices(&s.entries, &s.local_filter, &s.folder_filter);
    let total = filtered.len();
    // v1.2.0: JOUER is a justified cover GALLERY (variable tiles per row). 2D
    // nav reads the layout the renderer publishes each frame: Left/Right =
    // prev/next tile in flow order, Up/Down = nearest tile in the adjacent row.
    //
    // It WRAPS, the way the IMPORTER list already did: past the last game you
    // land on the first, and the other way round. Two rules rather than one,
    // though, and the difference matters with a step of four or five: a move
    // STOPS at the edge first, and only the NEXT press crosses over. Wrapping on
    // the same press would turn "skip five" near the end of a 79-game library
    // into a jump back to the seventy-fourth, which is not what the button says.
    match button {
        "Left" | "StickLLeft" => {
            // In LISTE the rows run vertically, so Left/Right did exactly what
            // Up/Down did — one row — and two of the four directions were dead
            // weight. They skip instead, which is what they are for in a list.
            if crate::loc::home_view() == 1 {
                // Not on a diagonal: the jump is what makes the stick feel
                // twitchy there, since the other axis is already moving one row
                // and the two land five apart. Held diagonally, the row step
                // alone applies and the movement stays the normal one.
                if nav_is_diagonal() {
                    return;
                }
                selection = list_step_wrap(selection, -(LIST_SKIP as isize), total);
            } else if total > 0 {
                selection = list_step_wrap(selection, -1, total);
            }
            scroll = gallery_scroll_for(selection, scroll);
        }
        "Right" | "StickLRight" => {
            if crate::loc::home_view() == 1 {
                if nav_is_diagonal() {
                    return;
                }
                selection = list_step_wrap(selection, LIST_SKIP as isize, total);
            } else if total > 0 {
                selection = list_step_wrap(selection, 1, total);
            }
            scroll = gallery_scroll_for(selection, scroll);
        }
        "Up" | "StickLUp" => {
            // Single-row layouts have no adjacent row for `gallery_neighbor` to
            // find, so Up/Down would be inert there. They jump instead, Down
            // forward and Up back: the same direction they move in GRILLE and
            // LISTE, so changing view never turns the vertical axis round. (Up
            // used to jump forward here, with nothing on screen to say so.)
            if matches!(crate::loc::home_view(), 2 | 3) {
                // Same rule as LISTE, other axis: here the row IS horizontal, so
                // Left/Right is the normal path and the jump belongs to Up/Down.
                if nav_is_diagonal() {
                    return;
                }
                selection = list_step_wrap(selection, -(home_page_step() as isize), total);
            } else if let Some(ns) = gallery_neighbor(selection, -1) {
                selection = ns;
            } else if total > 0 && nav_may_wrap() {
                // Top row: round to the LAST game rather than sit there, same as
                // the list does at its own ends. Gated like every other crossing
                // -- this is the one that is written out by hand instead of going
                // through `list_step_wrap`, which is how it kept looping under a
                // held direction after the others had been fixed.
                selection = total - 1;
            }
            scroll = gallery_scroll_for(selection, scroll);
        }
        "Down" | "StickLDown" => {
            if matches!(crate::loc::home_view(), 2 | 3) {
                if nav_is_diagonal() {
                    return;
                }
                selection = list_step_wrap(selection, home_page_step() as isize, total);
            } else if let Some(ns) = gallery_neighbor(selection, 1) {
                selection = ns;
            } else if total > 0 && nav_may_wrap() {
                // Bottom row: round to the first game, on a press only.
                selection = 0;
            }
            scroll = gallery_scroll_for(selection, scroll);
        }
        "A" => {
            let Some(&abs) = filtered.get(selection) else { return; };
            if let Some(entry) = s.entries.get(abs) {
                // Applet mode has too little RAM to launch a SWF (it would
                // OOM into the embedded red screen). Show a clear notice
                // instead of attempting the launch (P1c).
                if s.applet_mode {
                    s.screen = Screen::AppletNotice { selection, scroll_offset: scroll };
                    return;
                }
                s.selected_path = Some(entry.path.clone());
                // Remember it so quit-to-library lands back on this row and
                // the pause menu can show the title.
                note_played(&entry.path, &entry.basename, &entry.display_name);
                if let Ok(mut g) = LAUNCH_TICK.lock() {
                    *g = Some(unsafe { ruffle_tick_now() });
                }
                // The launch reveal flags a multi-file game, as its tile does.
                LAUNCH_MULTI.store(entry.multi, std::sync::atomic::Ordering::Relaxed);
                log(&std::format!(
                    "library: JOUER -> {} ({})\n",
                    entry.display_name, entry.path,
                ));
                // Play the cover "open" reveal (tile -> full screen) first; render
                // flips to Picked when it finishes, then the SWF loads behind the
                // frozen full-screen cover (a free loading screen).
                let rect = crate::backend::render::gallery_sel_rect_read();
                crate::backend::render::game_reveal_begin(
                    false,
                    rect,
                    &entry.basename,
                    &entry.display_name,
                    entry.color_chip,
                );
                s.screen = Screen::Launching { selection, scroll_offset: scroll };
            }
            return;
        }
        // Minus = search (hoisted at the top of `input()`). `+` = per-game
        // OPTIONS (moved off ZL); the Settings modal `+` used to open is now
        // the REGLAGES navbar tab (reached with L/R).
        "Plus" => {
            if let Some(&abs) = filtered.get(selection) {
                s.screen = Screen::OptionsModal { game_idx: abs, selection: 0 };
            }
            return;
        }
        "Y" => {
            // Open the sort picker (cursor on the active mode, turned the way it
            // is); remember where we were so B (cancel) restores it instead of
            // jumping to the top.
            let mode = sort_mode_index();
            s.screen = Screen::SortModal {
                selection: mode,
                prev_sel: selection,
                prev_scroll: scroll,
                dirs: if current_sort_reverse() { 1 << mode } else { 0 },
            };
            return;
        }
        // ZL / ZR walk the shelves (issue #68), in echo of L / R walking the
        // navbar tabs: shoulders move you sideways, the pair further in moves
        // you between shelves. The ring is the strip's chips in order (TOUS,
        // the favourites, every shelf), so where they land is on screen.
        //
        // There used to be an ACCUEIL stop, "everything on no shelf". It earned
        // its place when filing meant MOVING a file; now that filing is one
        // press, it was TOUS minus a few games, a near-copy of the stop next to
        // it costing a press on every turn of the ring.
        "ZL" | "ZR" => {
            let (keys, cur) = shelf_keys(s);
            if keys.len() < 2 {
                return;
            }
            let n = keys.len();
            let next = if button == "ZR" { (cur + 1) % n } else { (cur + n - 1) % n };
            // ZR walks right, so the new shelf arrives from the right.
            goto_shelf(s, keys[next].clone(), if button == "ZR" { 1.0 } else { -1.0 }, selection, None);
            return;
        }
        // B, the way back everywhere else, is the way back here too: out of a
        // search first, then off a shelf to TOUS, keeping the game the cursor
        // is on in both cases. It did nothing at all, so the only way back to
        // TOUS was ZL / ZR through every shelf, and a search could only be
        // cleared by erasing it in the keyboard.
        "B" => {
            let keep = filtered.get(selection).and_then(|&i| s.entries.get(i)).map(|e| e.path.clone());
            let searching = s.local_filter.as_deref().is_some_and(|f| !f.trim().is_empty());
            if searching {
                s.local_filter = None;
                let row = keep.and_then(|p| row_of_path(s, &p)).unwrap_or(0);
                crate::backend::render::gallery_anim_reset();
                crate::backend::render::tile_open_reset();
                s.screen = Screen::List { selection: row, scroll_offset: home_scroll_for(row, 0) };
            } else if s.folder_filter.is_some() {
                goto_shelf(s, None, -1.0, selection, keep);
            }
            return;
        }
        // X: the next view, from where the view is shown (the strip's right
        // end), instead of two screens away in REGLAGES. Same game, same order,
        // same shelf; only the layout changes, and it snaps rather than slides.
        "X" => {
            let next = (crate::loc::home_view() + 1) % crate::loc::HOME_VIEW_COUNT;
            crate::loc::set_home_view(next);
            crate::loc::save_current();
            crate::backend::render::home_anim_reset();
            crate::backend::render::gallery_anim_reset();
            scroll = home_scroll_for(selection, 0);
        }
        _ => {}
    }
    s.screen = Screen::List { selection, scroll_offset: scroll };
}

// ── Phase 3.7: DISTANT mode input handlers ────────────────────────────

// ── IMPORTER home: display rows, search + sort ─────────────────────────
//
// The saved-URL list used to be raw URLs in insertion order, add-row last —
// fine for three entries, unreadable at thirty. Rows now show a NAME (the item
// id / file the URL points at) with the host dimmed beside it, the two action
// cards (search Flashpoint, add an address) are pinned above the list so they
// are reachable without scrolling, and the list filters (Minus) + sorts (Y)
// like every other list in the app. `url_history` itself
// stays in insertion order: it IS the "ADDED" sort, and reordering it on disk
// would throw that away.

/// Human-readable name for a saved import URL: the archive.org item id, or the
/// file the URL points at, minus scheme / query / extension. Falls back to the
/// host (then to the raw URL) so a row is never blank.
pub(crate) fn importer_label(url: &str) -> std::string::String {
    let raw = url.trim();
    let no_scheme = raw.split_once("://").map(|(_, rest)| rest).unwrap_or(raw);
    let (host, path) = match no_scheme.split_once('/') {
        Some((h, p)) => (h, p),
        None => (no_scheme, ""),
    };
    // Query + fragment never identify the game.
    let path = path.split(['?', '#']).next().unwrap_or("");
    let pick = if host.contains("archive.org") {
        // /details/<id>, /download/<id>/<file>, /stream/<id> — the item id is
        // the useful part. A bare `<id>` (no slash) lands in `host` instead.
        let mut segs = path.split('/').filter(|s| !s.is_empty());
        match segs.next() {
            Some("details") | Some("download") | Some("stream") => segs.next(),
            other => other,
        }
    } else {
        // Everything else: the last segment is the file (game.swf, ...).
        path.rsplit('/').find(|s| !s.is_empty())
    };
    let name = match pick {
        Some(n) if !n.is_empty() => n,
        _ => host,
    };
    // Drop a trailing file extension so ".swf" doesn't eat row width. Guarded
    // on a short ALPHABETIC suffix so version-ish ids ("sonic_v1.2") survive.
    let name = match name.rsplit_once('.') {
        Some((stem, ext))
            if !stem.is_empty()
                && (2..=4).contains(&ext.len())
                && ext.chars().all(|c| c.is_ascii_alphabetic()) =>
        {
            stem
        }
        _ => name,
    };
    if name.is_empty() {
        raw.to_string()
    } else {
        name.to_string()
    }
}

/// Host tag drawn dimmed at the right of an IMPORTER row ("archive.org", ...).
/// Empty for a bare archive.org item id — there's no host in it to show.
pub(crate) fn importer_host(url: &str) -> std::string::String {
    let Some((_, rest)) = url.trim().split_once("://") else {
        return std::string::String::new();
    };
    let host = rest.split('/').next().unwrap_or("");
    host.strip_prefix("www.").unwrap_or(host).to_string()
}

/// Absolute indices into `url_history` in the order the IMPORTER home DISPLAYS
/// them: search filter first, then the active sort. `Screen::DistantIdle`'s
/// `selection` is a ROW index — rows 0 and 1 are the cards, rows 2.. index this.
pub(crate) fn importer_view(s: &State) -> std::vec::Vec<usize> {
    let needle = s
        .import_filter
        .as_deref()
        .map(|f| f.trim().to_lowercase())
        .filter(|f| !f.is_empty());
    let mut idx: std::vec::Vec<usize> = (0..s.url_history.len())
        .filter(|&i| match &needle {
            None => true,
            // Match the URL *and* the label: "mario" finds
            // `archive.org/details/super-mario-63` either way.
            Some(q) => {
                let u = &s.url_history[i];
                u.to_lowercase().contains(q) || importer_label(u).to_lowercase().contains(q)
            }
        })
        .collect();
    let sort = import_sort_index();
    let rev = import_sort_reverse();
    match sort {
        1 => idx.sort_by_key(|&i| importer_label(&s.url_history[i]).to_lowercase()),
        // SOURCE: grouped by host, by name inside a group.
        2 => idx.sort_by_key(|&i| {
            let u = &s.url_history[i];
            (importer_host(u).to_lowercase(), importer_label(u).to_lowercase())
        }),
        // FILES: biggest source first, or fewest first when turned round. A
        // URL never opened has no count, and sorts last in BOTH orders rather
        // than pretending to be empty: turning the finished list round put
        // every one of them on top, a column of blanks hiding the rest (the
        // rule `sort_fp_candidates` keeps for the same reason). So the order is
        // applied here, and the list is not turned round below.
        3 => {
            let mut keyed: std::vec::Vec<(usize, Option<u32>, std::string::String)> = idx
                .iter()
                .map(|&i| {
                    let u = &s.url_history[i];
                    (i, importer_progress(s, u).1, importer_label(u).to_lowercase())
                })
                .collect();
            keyed.sort_by(|a, b| {
                a.1.is_none()
                    .cmp(&b.1.is_none())
                    .then_with(|| {
                        let (x, y) = (a.1.unwrap_or(0), b.1.unwrap_or(0));
                        if rev { x.cmp(&y) } else { y.cmp(&x) }
                    })
                    .then_with(|| a.2.cmp(&b.2))
            });
            idx = keyed.into_iter().map(|k| k.0).collect();
        }
        // ADDED: history is oldest-first, so newest-first = reversed.
        _ => idx.reverse(),
    }
    if rev && sort != 3 {
        idx.reverse();
    }
    // Favorites pinned on top whatever the sort, exactly like the JOUER gallery
    // (`sort_entries`). `sort_by_key` is stable, so the active order survives
    // inside each group — and the reverse above must not un-pin them, hence
    // pinning LAST.
    idx.sort_by_key(|&i| !url_is_favorite(s, &s.url_history[i]));
    idx
}

/// The importer home's layout (Importer.dc.html), in screen pixels: two action
/// cards side by side under the tabs, the "Mes sources" band, then the sources
/// at a 58 px pitch. Shared by the renderer, the reveal that opens a row
/// (`distant_row_rect`), the touch table and the scroll clamps, through
/// `importer_item_rect`: the renderer laying rows out from its own literals while
/// the reveal re-typed them is how a window once grew out of the row above.
pub const IMPORTER_MARGIN: f32 = 48.0;
pub const IMPORTER_CARD_TOP: f32 = 84.0;
pub const IMPORTER_CARD_H: f32 = 92.0;
pub const IMPORTER_CARD_GAP: f32 = 16.0;
pub const IMPORTER_ROW_TOP: f32 = 228.0;
pub const IMPORTER_ROW_H: f32 = 58.0;
/// Source rows visible at once, down to y 634, clear of the footer.
pub const IMPORTER_VISIBLE_ROWS: usize = 7;

/// The two actions above the saved sources: search Flashpoint, then add an
/// address. `Screen::DistantIdle::selection` counts them first.
///
/// Flashpoint comes FIRST because it is the only import path that needs no URL:
/// you type a name and pick a cover. The tab was built around archive.org URLs
/// and kept that shape long after, so on a fresh install the whole screen said
/// "paste an address" and the easy path was one letter in the footer legend.
///
/// Every row <-> URL conversion goes through `importer_row_for_abs` and
/// `importer_abs_for_row`, so this number is the only place the offset lives.
pub const IMPORTER_PINNED_ROWS: usize = 2;

/// Screen rect of importer item `row`: a card for rows 0 and 1, else a source
/// row, at the list's pixel scroll `scroll_px` (source rows only scroll).
pub(crate) fn importer_item_rect(row: usize, scroll_px: f32, vw: f32) -> (f32, f32, f32, f32) {
    let w_all = vw - 2.0 * IMPORTER_MARGIN;
    if row < IMPORTER_PINNED_ROWS {
        let cw = ((w_all - IMPORTER_CARD_GAP) * 0.5).floor();
        let x = IMPORTER_MARGIN + row as f32 * (cw + IMPORTER_CARD_GAP);
        return (x, IMPORTER_CARD_TOP, cw, IMPORTER_CARD_H);
    }
    let k = (row - IMPORTER_PINNED_ROWS) as f32;
    (IMPORTER_MARGIN, IMPORTER_ROW_TOP + k * IMPORTER_ROW_H - scroll_px, w_all, IMPORTER_ROW_H)
}

/// An archive's game list: the rows' top (under the page header) and pitch.
pub const FILES_ROW_TOP: f32 = 140.0;
pub const FILES_ROW_H: f32 = 50.0;

/// The Flashpoint results grid: the Play grid's tiles (16:10, gap 20, the page
/// margin), its top under the page header, and the selected game's title and
/// facts under it.
pub const FP_GRID_TOP: f32 = 140.0;
pub const FP_GRID_GAP: f32 = 20.0;

/// Screen rect of Flashpoint tile `i` at the grid's pixel scroll `scroll_px`.
pub(crate) fn fp_tile_rect(i: usize, scroll_px: f32, vw: f32) -> (f32, f32, f32, f32) {
    let w_all = vw - 2.0 * IMPORTER_MARGIN;
    let w = (w_all - FP_GRID_GAP * (FP_GALLERY_COLS as f32 - 1.0)) / FP_GALLERY_COLS as f32;
    let h = (w * 10.0 / 16.0).round();
    let (col, row) = (i % FP_GALLERY_COLS, i / FP_GALLERY_COLS);
    (
        IMPORTER_MARGIN + col as f32 * (w + FP_GRID_GAP),
        FP_GRID_TOP + row as f32 * (h + FP_GRID_GAP) - scroll_px,
        w,
        h,
    )
}

/// Last importer row (the add card when there is no source).
fn importer_last_row(s: &State) -> usize {
    importer_view(s).len() + IMPORTER_PINNED_ROWS - 1
}

/// The importer's scroll for the cursor on `row`, from the scroll it is at:
/// moved only as far as the row needs, never past the end of `n` sources. A
/// card needs none, it never scrolls away.
fn importer_scroll_for(row: usize, cur: usize, n: usize) -> usize {
    let max = n.saturating_sub(IMPORTER_VISIBLE_ROWS);
    if row < IMPORTER_PINNED_ROWS {
        return cur.min(max);
    }
    clamp_scroll(cur, row - IMPORTER_PINNED_ROWS, IMPORTER_VISIBLE_ROWS).min(max)
}

/// The importer row the cursor was last on, so leaving the tab and coming back
/// lands where it was (`IMPORT_SCROLL` keeps the scroll the same way).
static IMPORT_ROW: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// The card last focused, 0 or 1: Up from the first source goes back to it.
static IMPORT_CARD: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// When the importer home was last drawn as the page, to tell a frame that
/// follows another from one that comes back to it.
static HOME_TICK: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Scroll the IMPORTER list was last drawn at. Every "come back to the list"
/// path clamps from THIS rather than from 0: recomputing the scroll from the row
/// alone dragged the view whenever the cursor sat past the first page (open `+`
/// on row 13, come back, and the list had jumped so row 13 was the bottom one).
static IMPORT_SCROLL: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Same thing for the JOUER gallery, and for the same reason: quitting a game,
/// closing OPTIONS or deleting a game all rebuilt the scroll from the row alone
/// (`gallery_scroll_for(row, 0)`), which always parks that row on the LAST
/// visible line — so the gallery jumped under the cursor on every return.
static JOUER_SCROLL: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Scroll the JOUER gallery was last drawn at, for those restore paths.
fn jouer_scroll() -> usize {
    JOUER_SCROLL.load(std::sync::atomic::Ordering::Relaxed)
}

/// The gallery's next frame must scroll its selection into view from the table
/// it is about to publish. Set by `open` after a game started from an importer
/// page: the table on record then was that page's, and reading the row from it
/// put a game of row 2 on "row 12" and scrolled the grid past its end (blank,
/// the cursor off screen, until a direction was pressed).
static JOUER_RESCROLL: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// A `DistantIdle` screen parked on `row` (clamped to the rows there are),
/// keeping the current scroll unless the row would be off screen. Every caller
/// handing the cursor back to a specific row goes through this, so none of them
/// can scroll the list out from under the user or select a row the list isn't
/// showing.
fn distant_idle_at(s: &State, row: usize) -> Screen {
    let cur = IMPORT_SCROLL.load(std::sync::atomic::Ordering::Relaxed);
    let row = row.min(importer_last_row(s));
    Screen::DistantIdle { selection: row, scroll_offset: importer_scroll_for(row, cur, importer_view(s).len()) }
}

/// Importer row of history entry `abs` in the current view, so screens that act
/// on an ABSOLUTE url index can hand the cursor back to the right row. Falls back
/// to the add card when `abs` is gone or filtered out.
fn importer_row_for_abs(s: &State, abs: usize) -> usize {
    importer_view(s)
        .iter()
        .position(|&i| i == abs)
        .map(|p| p + IMPORTER_PINNED_ROWS)
        .unwrap_or(1)
}

/// Absolute `url_history` index behind an IMPORTER row, or `None` for the two
/// pinned action rows at the top and for out-of-range rows.
fn importer_abs_for_row(s: &State, row: usize) -> Option<usize> {
    if row < IMPORTER_PINNED_ROWS {
        return None;
    }
    importer_view(s).get(row - IMPORTER_PINNED_ROWS).copied()
}

/// How many of a saved URL's `.swf` files are already on SD, and how many there
/// are in total. `None` for the total = never opened, so we don't know yet (the
/// row then shows no count rather than a made-up one).
///
/// A direct `.swf` URL is one file by definition, matched by the name
/// `run_direct_download` would give it. An archive.org item is matched by item
/// id against the `.url` sidecar each downloaded game carries, so the count
/// survives reboots and doesn't care which files were picked.
fn importer_progress(s: &State, url: &str) -> (u32, Option<u32>) {
    let raw = crate::sources::wayback_raw(url);
    if matches!(
        crate::sources::classify(raw.as_str()),
        crate::sources::SourceKind::DirectUrl
    ) {
        let name = safe_name_from_url(raw.as_str());
        let have = s.entries.iter().any(|e| e.basename == name);
        return (have as u32, Some(1));
    }
    // Both numbers come from the cache, because this row is drawn for every saved
    // URL and none of their file lists are in memory.
    //
    // `have` used to be counted from the `.url` sidecars instead
    // (`entries.filter(source_item == id)`), which answered a different question
    // than the OK badge in the file list -- the badge asks whether a game of that
    // NAME is on the card. On an 81-file item that read "2 / 81" under a dozen
    // ticked rows. The sidecars exist for bug-report attribution, not as an
    // inventory, and the A-press guard already treats the name as the identity,
    // so the name is what both now count.
    let row = s.url_meta.iter().find(|(u, _, _, _, _)| u == url);
    let total = row.map(|(_, n, _, _, _)| *n);
    let have = row.map(|(_, _, _, _, h)| *h).unwrap_or(0);
    // The cached total predates any file we've since downloaded, so never let
    // the badge read "5/3" if the item grew or the cache is stale.
    (have, total.map(|t| t.max(have)))
}

/// One saved source as the importer home draws it, owned so it outlives the
/// lock.
struct HomeRow {
    name: std::string::String,
    host: std::string::String,
    direct: bool,
    have: u32,
    total: Option<u32>,
    favorite: bool,
}

/// The importer home's rows, in view order (search, then sort).
fn importer_home_rows(s: &State) -> std::vec::Vec<HomeRow> {
    importer_view(s)
        .iter()
        .map(|&i| {
            let u = &s.url_history[i];
            let (have, total) = importer_progress(s, u);
            HomeRow {
                name: importer_label(u),
                host: importer_host(u),
                direct: url_is_direct(u),
                have,
                total,
                favorite: url_is_favorite(s, u),
            }
        })
        .collect()
}

/// A direct `.swf` link (A downloads it), not an archive (A opens its list).
fn url_is_direct(url: &str) -> bool {
    matches!(
        crate::sources::classify(crate::sources::wayback_raw(url).as_str()),
        crate::sources::SourceKind::DirectUrl
    )
}

/// Draw the importer home with the cursor on `selection` at `scroll` (see
/// `ImporterHome`); `interactive` false when it is only the backdrop of what is
/// drawn over it.
fn draw_importer_home(backend: &mut SwitchRenderBackend, selection: usize, scroll: usize, interactive: bool) {
    use crate::backend::render::{ImporterHome, SourceRow};
    let snap = LIBRARY
        .lock()
        .ok()
        .map(|s| (importer_home_rows(&s), s.url_history.len(), s.import_filter.clone()));
    let Some((rows, total, filter)) = snap else { return };
    let refs: std::vec::Vec<SourceRow> = rows
        .iter()
        .map(|r| SourceRow {
            name: &r.name,
            host: &r.host,
            direct: r.direct,
            have: r.have,
            total: r.total,
            favorite: r.favorite,
        })
        .collect();
    let sort = remote_sort_phrase(SORT_TARGET_IMPORT, import_sort_index() as u8, import_sort_reverse());
    backend.draw_library_distant_list(&ImporterHome {
        rows: &refs,
        selection,
        scroll,
        filter: filter.as_deref(),
        total,
        sort,
        interactive,
    });
}

fn handle_distant_idle_input(
    s: &mut State,
    button: &str,
    mut selection: usize,
    scroll: usize,
) {
    // A (open / add), X (Flashpoint) and Minus (search) are hoisted: they open
    // the keyboard or the network. Here: moving, the options, the sort, and B.
    //
    // The two cards sit side by side over the list. Down from a card goes to
    // the first source and Up from the first source back to the card it came
    // from; past either end of the list a NEW press comes round (Up on a card
    // to the last source, Down on the last source to the cards) and a held one
    // stops, like every list.
    let n = importer_view(s).len();
    let last = n + IMPORTER_PINNED_ROWS - 1;
    // A row the list no longer has (it shrank under the cursor) counts as the last.
    selection = selection.min(last);
    let card = IMPORT_CARD.load(std::sync::atomic::Ordering::Relaxed).min(IMPORTER_PINNED_ROWS - 1);
    match button {
        "Up" | "StickLUp" => {
            if selection < IMPORTER_PINNED_ROWS {
                if n > 0 && nav_may_wrap() {
                    selection = last;
                }
            } else if selection == IMPORTER_PINNED_ROWS {
                selection = card;
            } else {
                selection -= 1;
            }
        }
        "Down" | "StickLDown" => {
            if selection < IMPORTER_PINNED_ROWS {
                if n > 0 {
                    selection = IMPORTER_PINNED_ROWS;
                }
            } else if selection >= last {
                if nav_may_wrap() {
                    selection = card;
                }
            } else {
                selection += 1;
            }
        }
        // On the cards Left / Right go from one to the other; in the list they
        // skip five sources, as in every other list. Suppressed on a diagonal.
        "Left" | "StickLLeft" | "Right" | "StickLRight" => {
            if nav_is_diagonal() {
                return;
            }
            let back = matches!(button, "Left" | "StickLLeft");
            if selection < IMPORTER_PINNED_ROWS {
                selection = if back { 0 } else { 1 };
            } else if n > 0 {
                let k = selection - IMPORTER_PINNED_ROWS;
                let step = if back { -(LIST_SKIP as isize) } else { LIST_SKIP as isize };
                selection = list_step_wrap(k, step, n) + IMPORTER_PINNED_ROWS;
            }
        }
        "Plus" => {
            // Options on a saved source (the cards have none).
            if let Some(abs) = importer_abs_for_row(s, selection) {
                s.screen = Screen::DistantUrlOptions { url_idx: abs, selection: 0 };
                return;
            }
        }
        "Y" => {
            // The sort window, on the order in force. Nothing to sort with one
            // source or none.
            if n > 1 {
                s.screen = Screen::RemoteSortModal {
                    selection: import_sort_index(),
                    kind: SORT_TARGET_IMPORT,
                    reverse: import_sort_reverse(),
                    prev_sel: selection,
                    prev_scroll: scroll,
                };
                return;
            }
        }
        "B" => {
            // A search narrows the list: B shows them all again, the cursor
            // staying on the source it was on. (No "back to Jouer": the tabs
            // are L / R.)
            if s.import_filter.is_some() {
                let abs = importer_abs_for_row(s, selection);
                s.import_filter = None;
                if let Some(abs) = abs {
                    selection = importer_row_for_abs(s, abs);
                }
                crate::backend::render::gallery_anim_reset();
            }
        }
        _ => {}
    }
    if selection < IMPORTER_PINNED_ROWS {
        IMPORT_CARD.store(selection, std::sync::atomic::Ordering::Relaxed);
    }
    let n = importer_view(s).len();
    s.screen = Screen::DistantIdle { selection, scroll_offset: importer_scroll_for(selection, scroll, n) };
}

/// A saved source's options (`DistantUrlOptions`), a side panel over the list.
/// Rows: favourite (A or Left / Right flip it, and the panel stays open while
/// the list re-sorts underneath, as the game options do), edit the address (A,
/// hoisted: the keyboard), delete (A asks first). B goes back to the source's
/// row.
fn handle_distant_url_options_input(
    s: &mut State,
    button: &str,
    url_idx: usize,
    mut selection: usize,
) {
    const ROWS: usize = 3;
    let back = |s: &mut State| {
        // `url_idx` is an ABSOLUTE history index; the list is sorted/filtered, so
        // ask the view which row that entry is on.
        let row = importer_row_for_abs(s, url_idx);
        s.screen = distant_idle_at(s, row);
    };
    match button {
        "Up" | "StickLUp" => selection = list_step_wrap(selection, -1, ROWS),
        "Down" | "StickLDown" => selection = list_step_wrap(selection, 1, ROWS),
        // The favourite row is a switch: Right turns it on, Left off.
        "Left" | "StickLLeft" | "Right" | "StickLRight" if selection == 0 => {
            if let Some(url) = s.url_history.get(url_idx).cloned() {
                let want = matches!(button, "Right" | "StickLRight");
                if url_is_favorite(s, &url) != want {
                    toggle_url_favorite(s, &url);
                }
            }
        }
        "A" => match selection {
            0 => {
                if let Some(url) = s.url_history.get(url_idx).cloned() {
                    toggle_url_favorite(s, &url);
                }
            }
            2 => {
                s.history_idx = Some(url_idx);
                s.screen = Screen::DistantHistoryConfirm { selection: 0 };
                return;
            }
            // 1 (edit) is hoisted in input().
            _ => {}
        },
        "B" | "Plus" => {
            back(s);
            return;
        }
        _ => {}
    }
    s.screen = Screen::DistantUrlOptions { url_idx, selection };
}

/// A saved source's options as a side panel (`Sheet`), handed to `f`: the
/// source's name with its kind, how much of it is on the card and when it was
/// saved as chips, then favourite / edit the address / delete, a line of help
/// for each.
fn with_source_sheet<R>(
    backend: &mut SwitchRenderBackend,
    url_idx: usize,
    selection: usize,
    f: impl FnOnce(&mut SwitchRenderBackend, &crate::backend::render::Sheet) -> R,
) -> Option<R> {
    use crate::backend::render::{ui_col, Btn, Icon, Row, Sheet, OPTIONS_W};
    let snap = LIBRARY.lock().ok().and_then(|s| {
        let url = s.url_history.get(url_idx).cloned()?;
        let (have, total) = importer_progress(&s, &url);
        let added = s
            .url_meta
            .iter()
            .find(|(u, _, _, _, _)| u == &url)
            .map(|(_, _, t, _, _)| *t)
            .unwrap_or(0);
        let fav = url_is_favorite(&s, &url);
        Some((url, have, total, added, fav))
    })?;
    let (url, have, total, added, fav) = snap;
    let lc = crate::loc::s();
    let u = crate::loc_ui::ui();
    let direct = url_is_direct(&url);
    let name = importer_label(&url);
    // The facts as chips, dropped from the end when they would overflow the
    // panel: a long translation must not run off its edge.
    let kind = if direct { lc.url_type_swf } else { lc.url_type_list };
    let on_card = match total {
        Some(t) if t > 0 => Some(
            lc.url_info_files
                .replacen("{}", &crate::backend::render::imp_count(have as u64), 1)
                .replacen("{}", &crate::backend::render::imp_count(t as u64), 1),
        ),
        _ if have > 0 => Some(u.imp_on_card_n.replacen("{}", &have.to_string(), 1)),
        _ => None,
    };
    let date = format_date(added);
    let added_txt = (!date.is_empty()).then(|| std::format!("{} {}", lc.url_info_added, date));
    let mut chip_list: std::vec::Vec<(Icon, std::string::String, u32)> =
        std::vec![(if direct { Icon::File } else { Icon::Archive }, kind.to_string(), ui_col::TEXT2)];
    if let Some(t) = on_card {
        let complete = matches!(total, Some(t) if t > 0 && have >= t);
        chip_list.push((Icon::HardDrive, t, if complete { ui_col::OK } else { ui_col::TEXT2 }));
    }
    if let Some(t) = added_txt {
        chip_list.push((Icon::Calendar, t, ui_col::TEXT2));
    }
    let room = OPTIONS_W - 48.0;
    loop {
        let w: f32 = chip_list
            .iter()
            .map(|(_, t, _)| 40.0 + backend.kit_measure(t, 13.0))
            .sum::<f32>()
            + 8.0 * chip_list.len().saturating_sub(1) as f32;
        if w <= room || chip_list.len() <= 1 {
            break;
        }
        chip_list.pop();
    }
    let chips: std::vec::Vec<(Icon, &str, u32)> = chip_list.iter().map(|(i, t, c)| (*i, t.as_str(), *c)).collect();
    let rows = [
        Row::new(Icon::Star, lc.opt_favorite).filled_if(fav, ui_col::GOLD).toggle(fav),
        Row::new(Icon::Pencil, u.imp_edit_address),
        Row::new(Icon::Trash, u.imp_delete_source).danger(),
    ];
    let help_edit = u.imp_help_src_edit.replacen("{}", &url, 1);
    let help = match selection {
        0 => u.imp_help_src_fav,
        1 => help_edit.as_str(),
        _ => lc.histdel_msg,
    };
    let a_verb = match selection {
        0 if fav => lc.opt_unfavorite,
        0 => u.imp_fav_add,
        1 => u.edit,
        _ => lc.opt_delete,
    };
    let hints = [(&[Btn::A][..], a_verb), (&[Btn::B][..], u.back)];
    let crumbs = [u.tab_import];
    let mut sheet = Sheet::new(&name, &rows, selection);
    sheet.kicker = Some(if direct { Icon::File } else { Icon::Archive });
    sheet.crumbs = &crumbs;
    sheet.subtitle = Some(u.imp_src_sub);
    sheet.chips = &chips;
    sheet.row_h = 48.0;
    sheet.glide_key = 0x6400_0000;
    sheet.help = help;
    sheet.hints = &hints;
    Some(f(backend, &sheet))
}

/// The importer with a saved source's options over it: the list as it was,
/// dimmed, and the panel at the right edge, sliding in and out on the window
/// timeline (`slide`, None when settled). The delete question, when it is up,
/// sits on top of both. `kind` is the touch tag the panel's rows publish under.
fn draw_source_options(
    backend: &mut SwitchRenderBackend,
    url_idx: usize,
    selection: usize,
    slide: Option<f32>,
    question: Option<usize>,
    kind: u32,
) {
    use crate::backend::render::OPTIONS_W;
    let row = LIBRARY.lock().map(|s| importer_row_for_abs(&s, url_idx)).unwrap_or(0);
    let scroll = IMPORT_SCROLL.load(std::sync::atomic::Ordering::Relaxed);
    backend.clear_ui_transform();
    crate::backend::render::set_ui_screen_kind(0);
    draw_importer_home(backend, row, scroll, false);
    let logo = LIBRARY.lock().ok().map(|s| (s.banner_tex, s.banner_w, s.banner_h)).unwrap_or_default();
    // L and R do not switch tabs under the panel: no L / R chips.
    backend.draw_navbar_with(Tab::Importer.index(), logo, false);
    crate::backend::render::navbar_clear();
    backend.kit_scrim(0x9E_06_0A_14);
    crate::backend::render::set_ui_screen_kind(kind);
    if let Some(p) = slide {
        backend.set_ui_sheet_slide_right(p);
    }
    let (vw, _) = backend.screen_size();
    let x = vw - crate::backend::render::SHEET_MARGIN - OPTIONS_W;
    with_source_sheet(backend, url_idx, selection, |b, sheet| b.kit_sheet_at(sheet, x, OPTIONS_W));
    backend.clear_ui_transform();
    if let Some(q) = question {
        use crate::backend::render::{Choice, Icon};
        let lc = crate::loc::s();
        let u = crate::loc_ui::ui();
        let detail = LIBRARY
            .lock()
            .ok()
            .and_then(|s| s.url_history.get(url_idx).cloned())
            .map(|url| {
                let host = importer_host(&url);
                if host.is_empty() {
                    importer_label(&url)
                } else {
                    std::format!("{}  \u{00B7}  {}", importer_label(&url), host)
                }
            })
            .unwrap_or_default();
        if let Some(p) = slide {
            backend.set_ui_modal_scale(p);
        }
        backend.draw_confirm(
            Icon::Trash,
            lc.histdel_title,
            lc.histdel_msg,
            Some(&detail),
            &[
                Choice { label: u.cancel, destructive: false },
                Choice { label: lc.opt_delete, destructive: true },
            ],
            q,
            true,
        );
        backend.clear_ui_transform();
    }
}

/// Remove the currently-displayed history URL and persist the trimmed list.
/// Keeps `history_idx` in range (or None when the list becomes empty).
fn delete_current_history(s: &mut State) {
    let Some(idx) = s.history_idx else { return };
    if idx >= s.url_history.len() {
        return;
    }
    let removed = s.url_history.remove(idx);
    // Its side data goes with it, in memory too: left there, it came back with
    // the address (the old star, the old date) when the same one was added
    // again.
    s.url_meta.retain(|(u, _, _, _, _)| u != &removed);
    s.history_idx = if s.url_history.is_empty() {
        None
    } else {
        Some(idx.min(s.url_history.len() - 1))
    };
    // save_history_to_sd does not lock LIBRARY, so calling it while we hold
    // the guard `s` is safe. Skip the write if the last load errored (same
    // guard as push_history) — a delete should never clobber an unread file.
    if s.history_loaded {
        let snapshot = s.url_history.clone();
        save_history_to_sd(&snapshot);
        save_history_meta(s);
    }
}

/// Saved source `idx` now goes by `new_url` (its address edited, or fixed
/// from an error page), and the index it has afterwards.
///
/// Its favourite and its added date go with it: the side data is keyed by the
/// address, and renaming only the history lost the star and the date, and
/// moved the source out of the pinned group. Its counts described the old
/// address's item and start again. An address that is already saved is not
/// listed twice: the source is merged into that entry, which keeps the earlier
/// date and a star from either.
fn rename_source(s: &mut State, idx: usize, new_url: &str) -> usize {
    let Some(old) = s.url_history.get(idx).cloned() else { return idx };
    if old == new_url {
        return idx;
    }
    let old_meta = s.url_meta.iter().position(|(u, _, _, _, _)| *u == old).map(|i| s.url_meta.remove(i));
    let (at, merged) = match s.url_history.iter().position(|u| u == new_url) {
        Some(existing) => {
            s.url_history.remove(idx);
            if let Some((_, _, added, fav, _)) = old_meta {
                match s.url_meta.iter_mut().find(|(u, _, _, _, _)| u == new_url) {
                    Some(slot) => {
                        slot.3 |= fav;
                        if added != 0 && (slot.2 == 0 || added < slot.2) {
                            slot.2 = added;
                        }
                    }
                    None => s.url_meta.push((new_url.to_string(), 0, added, fav, 0)),
                }
            }
            (if existing > idx { existing - 1 } else { existing }, true)
        }
        None => {
            s.url_history[idx] = new_url.to_string();
            s.url_meta.retain(|(u, _, _, _, _)| u != new_url);
            if let Some((_, _, added, fav, _)) = old_meta {
                s.url_meta.push((new_url.to_string(), 0, added, fav, 0));
            }
            (idx, false)
        }
    };
    if let Some(h) = s.history_idx {
        s.history_idx = Some(if h == idx {
            at
        } else if merged && h > idx {
            h - 1
        } else {
            h
        });
    }
    if s.history_loaded {
        let snapshot = s.url_history.clone();
        save_history_to_sd(&snapshot);
        save_history_meta(s);
    }
    at
}

/// The add card: the keyboard for a new address (`initial` pre-fills it when
/// retyping text that was not one), then open it. Text with a space in it
/// cannot be an address (an archive.org identifier never has one): it is a
/// game's name, so the importer offers to search Flashpoint for it instead of
/// failing on it. Called from `input()` only (the keyboard needs the lock free).
fn run_add_url_flow(initial: Option<&str>) {
    let lc = crate::loc::s();
    let Some(url) = net::prompt_url(lc.kbd_url_header, lc.kbd_url_guide, initial) else {
        return; // user cancelled
    };
    let url = url.trim().to_string();
    if url.is_empty() {
        return;
    }
    if url.contains(char::is_whitespace) {
        if let Ok(mut s) = LIBRARY.lock() {
            // A name typed after an archive.org address is still a name: the
            // question quotes it, and Flashpoint is searched for it, without
            // the address in front.
            s.import_typed = strip_archive_prefix(&url).trim().to_string();
            s.screen = Screen::ImportNotAddress { selection: 1 };
        }
        return;
    }
    // The window opens out of the add card. The importer is put back under it
    // first: retyped from the "not an address" question, that question was
    // still the screen, and an address that leads nowhere new (a direct link to
    // a game already on the card only says so in a toast) left it up behind
    // the toast. A cancelled keyboard returned above and keeps it.
    if let Ok(mut s) = LIBRARY.lock() {
        s.import_return_row = 1;
        s.screen = distant_idle_at(&s, 1);
    }
    run_fetch_for_url(&url, 1, IMPORT_SCROLL.load(std::sync::atomic::Ordering::Relaxed));
}

/// `text` without a leading archive.org download or details address
/// (`https://archive.org/download/`, `archive.org/details/`...), compared
/// without regard to case.
fn strip_archive_prefix(text: &str) -> &str {
    let lower = text.to_ascii_lowercase();
    for scheme in ["https://", "http://", ""] {
        for host in ["www.archive.org/", "archive.org/"] {
            for kind in ["download/", "details/"] {
                let prefix = std::format!("{}{}{}", scheme, host, kind);
                // ASCII on both sides of the cut, since the prefix matched it.
                if lower.starts_with(&prefix) {
                    return text.get(prefix.len()..).unwrap_or(text);
                }
            }
        }
    }
    text
}

/// IMPORTER home search (Minus): the keyboard pre-filled with the active search,
/// then narrow the saved sources to the ones matching it (address or name).
/// Empty input clears it. Called from `input()` only (the keyboard needs the
/// lock free).
fn run_import_search_flow() {
    let current = LIBRARY
        .lock()
        .ok()
        .and_then(|s| s.import_filter.clone())
        .unwrap_or_default();
    let u = crate::loc_ui::ui();
    let Some(typed) = net::prompt_search_as(u.imp_kbd_src_header, u.imp_kbd_src_guide, &current) else {
        return; // cancelled
    };
    let trimmed = typed.trim().to_string();
    if let Ok(mut s) = LIBRARY.lock() {
        s.import_filter = if trimmed.is_empty() { None } else { Some(trimmed) };
        // Fresh (filtered) list: the cursor on its first source, under the two
        // cards, or on the first card when nothing matched.
        let row = if importer_view(&s).is_empty() { 0 } else { IMPORTER_PINNED_ROWS };
        IMPORT_SCROLL.store(0, std::sync::atomic::Ordering::Relaxed);
        s.screen = distant_idle_at(&s, row);
    }
    crate::backend::render::gallery_anim_reset();
}

/// Error screen (Y): re-type the address that just failed, pre-filled, then try
/// it straight away. If the bad one was already saved it is UPDATED in place
/// (rather than leaving the broken one behind next to the fixed one). Called
/// from `input()` only: the keyboard and the retry need the lock free.
fn run_fix_url_flow(failed: &str) {
    let lc = crate::loc::s();
    let u = crate::loc_ui::ui();
    let Some(fixed) = net::prompt_url(u.imp_edit_address, lc.kbd_url_guide, Some(failed)) else {
        return; // cancelled: stay on the error page
    };
    let fixed = fixed.trim().to_string();
    if fixed.is_empty() {
        return;
    }
    let row = {
        let Ok(mut s) = LIBRARY.lock() else { return };
        // Leave the error page behind: the notice AND the half-finished
        // download bookkeeping, exactly like the dismiss does.
        clear_import_error(&mut s);
        clear_download_state(&mut s);
        if let Some(pos) = s.url_history.iter().position(|u| u == failed) {
            let at = rename_source(&mut s, pos, &fixed);
            importer_row_for_abs(&s, at)
        } else {
            s.import_return_row
        }
    };
    let scroll = IMPORT_SCROLL.load(std::sync::atomic::Ordering::Relaxed);
    run_fetch_for_url(&fixed, row, scroll);
}

/// Leave a modal for `target`, scaling the panel OUT first when there is a panel
/// to scale.
///
/// The generic interception lives in `input()`, on the value of `s.screen`
/// before and after a button is handled. A flow HOISTED out of that path -- one
/// that opens the system keyboard and then assigns `s.screen` itself -- skips it
/// entirely, which is why editing a saved URL closed its panel with a hard cut
/// while every other panel shrank. This is the door those flows go through
/// instead of assigning directly.
///
/// Falls back to the instant swap when there is nothing to animate (the current
/// screen is not a modal, or the target is another modal, which scales itself
/// in). Never use it for a close that carries a MUTATION: those must run after
/// the pop, which is what `PendingClose`'s own variants are for.
fn close_modal_to(s: &mut State, target: Screen) {
    let from = s.screen;
    if modal_kind(from) != 0
        && modal_kind(target) == 0
        && target != from
        && !crate::backend::render::modal_close_active()
    {
        if let Ok(mut p) = PENDING_AFTER_CLOSE.lock() {
            *p = Some(PendingClose::Goto(target));
        }
        crate::backend::render::modal_close_begin();
        return; // `s.screen` stays put so the panel is still drawn while it shrinks
    }
    s.screen = target;
}

/// DistantUrlOptions > edit: swkbd prefilled with the existing URL. Commit
/// replaces it (`rename_source`: its star and its date go with it), persists,
/// returns to the list; an emptied field is a cancel. Called from `input()`
/// only.
fn run_edit_url_flow(url_idx: usize) {
    let prefill = LIBRARY
        .lock()
        .ok()
        .and_then(|s| s.url_history.get(url_idx).cloned());
    let Some(prefill) = prefill else { return };
    let header = crate::loc_ui::ui().imp_edit_address;
    let Some(new_url) = net::prompt_url(header, crate::loc::s().kbd_url_guide, Some(&prefill)) else {
        return; // cancelled: the history stays as it was
    };
    let new_url = new_url.trim().to_string();
    // Emptied: a cancel, not a delete. Deleting has its own row and asks
    // first; clearing a text field is how people start retyping.
    if new_url.is_empty() {
        return;
    }
    if let Ok(mut s) = LIBRARY.lock() {
        let at = rename_source(&mut s, url_idx, &new_url);
        // The panel shrinks away on the source as it now is: merged into an
        // entry saved earlier, its old index is somebody else's.
        if let Screen::DistantUrlOptions { selection, .. } = s.screen {
            s.screen = Screen::DistantUrlOptions { url_idx: at, selection };
        }
        let row = importer_row_for_abs(&s, at);
        let target = distant_idle_at(&s, row);
        close_modal_to(&mut s, target);
    }
}

/// Fetch the given URL and transition state. Used by the keyboard flows (add,
/// fix), by A on a saved source and by the error page's retry.
/// v1.2.0: routes by source shape so the IMPORTER is "globalisable" — an
/// archive.org URL/item-id goes through the metadata file list, a direct
/// `.swf` URL downloads straight to SD.
fn run_fetch_for_url(url: &str, source_sel: usize, source_scroll: usize) {
    // Remember what we're about to try: if it blows up, the error page offers
    // Y = re-type THIS address instead of sending the user back to the list, and
    // A = the same request again. `failed_url` is cleared as soon as the address
    // proves good (metadata parsed / download finished).
    if let Ok(mut s) = LIBRARY.lock() {
        s.failed_url = url.to_string();
        s.import_return_row = source_sel;
        s.import_retry = Some(RetryAction::Fetch { url: url.to_string(), row: source_sel });
    }
    match crate::sources::classify(url) {
        crate::sources::SourceKind::DirectUrl => run_direct_download(url),
        crate::sources::SourceKind::ArchiveOrg => {
            run_archive_fetch_async(url, source_sel, source_scroll)
        }
    }
}

/// On-SD `.swf` filename FlashNX gives a directly-imported URL: last path
/// segment, query/fragment stripped, path separators sanitized, `.swf`
/// enforced. Kept standalone so the bug-report flow can recover a game's
/// source URL by matching this against the import history (`url_history`).
fn safe_name_from_url(url: &str) -> std::string::String {
    let tail = url.rsplit('/').next().unwrap_or("");
    let stem = tail.split(['?', '#']).next().unwrap_or(tail);
    let base = if stem.is_empty() { "download.swf" } else { stem };
    let cleaned: std::string::String = base
        .chars()
        .map(|c| if matches!(c, '/' | '\\') { '_' } else { c })
        .collect();
    if cleaned.to_ascii_lowercase().ends_with(".swf") {
        cleaned
    } else {
        std::format!("{}.swf", cleaned)
    }
}

/// Direct `.swf` URL import: download straight to SD, no metadata list. The
/// filename is derived from the URL's last path segment (query/fragment
/// stripped); a game of that name already on the card is not downloaded again.
fn run_direct_download(url: &str) {
    // Wayback URLs need the `id_` modifier to serve the raw file instead of the
    // HTML page wrapper (e.g. .../web/<ts>id_/...mting.swf). No-op otherwise.
    let url = crate::sources::wayback_raw(url);
    let url = url.as_str();
    let safe_name = safe_name_from_url(url);
    // Already on SD? Skip the re-download (the game is playable from JOUER),
    // mirroring the Flashpoint grid's A-press guard.
    if let Ok(mut s) = LIBRARY.lock() {
        if s.entries.iter().any(|e| e.basename == safe_name) {
            log(&std::format!("library: direct download skip - {} deja sur SD\n", safe_name));
            // Said out loud. Typing an address and landing back on the same list
            // with no row, no toast and no error did not distinguish "you already
            // have it" from "the button did not register" — and since identity is
            // the last path segment alone, it also covers "a DIFFERENT game with
            // the same file name". The URL still goes into the history: it is a
            // valid address the user asked for, and dropping it meant the saved
            // list quietly disagreed with what they typed.
            let shown = safe_name.clone();
            set_toast(
                &mut s,
                crate::loc::s().toast_already_imported.replace("{}", &shown),
                TOAST_INFO,
            );
            // The address is good, and nothing was sent to fail.
            s.failed_url.clear();
            s.import_retry = None;
            drop(s);
            push_history(url);
            return;
        }
    }
    let out_path = std::format!("{}/{}", primary_root(), safe_name);
    match net::start_download(url, &out_path) {
        Ok(()) => {
            push_history(url);
            note_download_started();
            if let Ok(mut s) = LIBRARY.lock() {
                s.download_file_name = safe_name;
                s.download_out_path = out_path;
                s.download_source_url = url.to_string();
                s.download_resume_pos = None;
                s.import_had_files = false;
                s.import_had_data = false;
                s.screen = Screen::DistantDownloading;
            }
        }
        Err(e) => set_import_error(ErrKind::Download, &e, true, &pretty_name(&safe_name)),
    }
}

/// IMPORTER > X (or A on the Flashpoint card): the keyboard for a game's name,
/// pre-filled with the last search, then the search. `origin` is the importer
/// row it was asked from, `None` from the results page itself (a new search).
/// Called from `input()` only (the keyboard needs the lock free).
fn run_fp_search_flow(origin: Option<usize>) {
    let initial = LIBRARY.lock().map(|s| s.fp_query.clone()).unwrap_or_default();
    let u = crate::loc_ui::ui();
    let Some(query) = net::prompt_search_as(u.imp_fp_title, u.imp_kbd_fp_guide, &initial) else {
        return;
    };
    let query = query.trim().to_string();
    if query.is_empty() {
        return;
    }
    start_fp_search(&query, origin);
}

/// Search Flashpoint for `query`, behind the results page: its header and
/// waiting tiles are up at once and the results fill them when they land (the
/// old blocking search froze the screen for a second or two). From the importer
/// (`origin` = the row it was asked from) the page opens out of the Flashpoint
/// card, as an archive opens out of its row, and B folds it back.
fn start_fp_search(query: &str, origin: Option<usize>) {
    let filter = LIBRARY.lock().map(|s| s.fp_content_filter).unwrap_or(true);
    log(&std::format!("library: flashpoint game search \"{}\" (async)\n", query));
    // Drop any thumbnail still coming for the previous results, and the
    // details blurb with them: a slot freed here can be handed to a logo next.
    // Their logos go too, textures and all: those results are gone.
    crate::backend::render::thumb_cancel_all();
    crate::backend::render::thumb_cache_clear();
    // A fresh result set: snap the glide and the tiles' clocks to rest.
    crate::backend::render::gallery_anim_reset();
    crate::backend::render::tile_open_reset();
    let started = net::start_get_async(&crate::sources::gamezip::search_url(query, filter));
    if let Ok(mut s) = LIBRARY.lock() {
        s.cover_candidates = std::vec::Vec::new();
        s.fp_query = query.to_string();
        s.fp_sort = (0, false);
        s.fp_desc_slot = -1;
        s.fp_size_slot = -1;
        match started {
            Ok(()) => {
                s.cover_msg = std::string::String::new();
                s.fp_loading = true;
                s.fp_error = false;
            }
            Err(e) => {
                s.cover_msg = e;
                s.fp_loading = false;
                s.fp_error = true;
            }
        }
        if let Some(row) = origin {
            s.import_return_row = row;
        }
        s.screen = Screen::FpGallery { selection: 0, scroll: 0 };
    }
    if origin.is_some() {
        crate::backend::render::distant_reveal_begin_expand(0, IMPORT_SCROLL.load(std::sync::atomic::Ordering::Relaxed));
    }
}

/// The last search again, without the keyboard: A "try again" on a search that
/// failed, and the hidden ZL+ZR chord, which flips the family filter first
/// (issue #33: the mature-rated catalogue, reachable without a settings row).
/// No-op while a search is already running or before any. Called from
/// `input()` only.
fn run_fp_requery(flip_filter: bool) {
    let query = {
        let Ok(mut s) = LIBRARY.lock() else { return };
        if s.fp_loading {
            return;
        }
        if flip_filter {
            s.fp_content_filter = !s.fp_content_filter;
            log(&std::format!("library: flashpoint filter -> {}\n", s.fp_content_filter));
        }
        s.fp_query.clone()
    };
    if query.trim().is_empty() {
        return;
    }
    start_fp_search(&query, None);
}

/// archive.org item import (async): start the metadata fetch + open the reveal
/// window (with a spinner) from the launched row. `render`'s DistantLoading arm
/// polls `net::tick_archive_fetch` each frame and switches to DistantFiles on
/// success (no UI freeze). `source_sel`/`source_scroll` are the history row the
/// window grows from and the scroll the list was at.
fn run_archive_fetch_async(url: &str, source_sel: usize, source_scroll: usize) {
    let Some(item_id) = net::extract_item_id(url) else {
        set_import_error(ErrKind::Open, crate::loc::s().err_url_invalid, false, &importer_label(url));
        return;
    };
    log(&std::format!("library: async fetch archive.org metadata for {}\n", item_id));
    match net::start_archive_fetch(&item_id) {
        Ok(()) => {
            if let Ok(mut s) = LIBRARY.lock() {
                s.pending_fetch_url = url.to_string();
                s.screen = Screen::DistantLoading;
            }
            crate::backend::render::distant_reveal_begin_expand(source_sel, source_scroll);
        }
        Err(e) => set_import_error(ErrKind::Open, &e, true, &importer_label(url)),
    }
}

fn handle_distant_files_input(
    s: &mut State,
    button: &str,
    mut selection: usize,
    mut scroll: usize,
) {
    // `selection` indexes the FILTERED view, not the raw `remote_files`.
    // When the user presses A we map back to the absolute file via the
    // current filter snapshot. Same applies for L/R page nav + Up/Down.
    let filtered: std::vec::Vec<usize> = filtered_indices(&s.remote_files, &s.distant_filter);
    let total = filtered.len();
    let last = total.saturating_sub(1);
    match button {
        // A new press comes round at the ends, a held one stops there: on a
        // 3 600-game archive, holding Down has to reach the last one.
        "Up" | "StickLUp" => {
            if total == 0 { return; }
            selection = list_step_wrap(selection, -1, total);
            scroll = clamp_scroll(scroll, selection, DISTANT_VISIBLE_ROWS);
        }
        "Down" | "StickLDown" => {
            if total == 0 { return; }
            selection = list_step_wrap(selection, 1, total);
            scroll = clamp_scroll(scroll, selection, DISTANT_VISIBLE_ROWS);
        }
        // Left/Right skip five rows, between the single step of Up/Down and the
        // full screenful of L/R. Same five as every other list in the app.
        "Left" | "StickLLeft" => {
            if total == 0 || nav_is_diagonal() { return; }
            selection = list_step_wrap(selection, -(LIST_SKIP as isize), total);
            scroll = clamp_scroll(scroll, selection, DISTANT_VISIBLE_ROWS);
        }
        "Right" | "StickLRight" => {
            if total == 0 || nav_is_diagonal() { return; }
            selection = list_step_wrap(selection, LIST_SKIP as isize, total);
            scroll = clamp_scroll(scroll, selection, DISTANT_VISIBLE_ROWS);
        }
        "L" => {
            // Page up — jump by visible_rows, saturate at 0 (no wrap;
            // page nav is for fast traversal, wrapping would be confusing).
            if total == 0 { return; }
            selection = selection.saturating_sub(DISTANT_VISIBLE_ROWS);
            scroll = clamp_scroll(scroll, selection, DISTANT_VISIBLE_ROWS);
        }
        "R" => {
            // Page down — jump by visible_rows, saturate at last.
            if total == 0 { return; }
            selection = (selection + DISTANT_VISIBLE_ROWS).min(last);
            scroll = clamp_scroll(scroll, selection, DISTANT_VISIBLE_ROWS);
        }
        "A" => {
            let Some(abs_idx) = filtered.get(selection).copied() else {
                return;
            };
            let Some(file) = s.remote_files.get(abs_idx).cloned() else {
                return;
            };
            let safe_name = safe_file_name(&file.name);
            let out_path = std::format!("{}/{}", primary_root(), safe_name);
            // Already on the card: A plays it. Re-downloading would only
            // overwrite the same file, and the row's tick already says it is
            // there; it used to be a silent no-op, so to play what you had just
            // downloaded you had to go and find it on Jouer.
            if let Some(abs) = s.entries.iter().position(|e| e.basename == safe_name) {
                let rect = crate::backend::render::import_sel_rect();
                launch_from_importer(s, abs, rect, Screen::DistantFiles { selection, scroll_offset: scroll });
                return;
            }
            // A game of an archive that opened: if it fails, the archive's
            // address is not what needs fixing (no Y on the error page).
            s.failed_url.clear();
            s.import_retry = Some(RetryAction::ArchiveFile { selection, scroll });
            match net::start_download(&file.download_url, &out_path) {
                Ok(()) => {
                    note_download_started();
                    s.download_file_name = file.name.clone();
                    s.download_out_path = out_path;
                    s.download_source_url = file.download_url.clone();
                    // Remember where the cursor was so we can put it back
                    // after the download finishes (otherwise the user has
                    // to rescroll through 1000s of entries to find their
                    // place).
                    s.download_resume_pos = Some((selection, scroll));
                    s.import_had_files = false;
                    s.import_had_data = false;
                    s.screen = Screen::DistantDownloading;
                }
                Err(e) => {
                    let name = pretty_name(&file.name);
                    import_error(s, ErrKind::Download, &e, true, &name);
                }
            }
            return;
        }
        "Y" => {
            // The sort window, on the order the list is in.
            if total > 1 {
                s.screen = Screen::RemoteSortModal {
                    selection: s.files_sort.0 as usize,
                    kind: SORT_TARGET_FILES,
                    reverse: s.files_sort.1,
                    prev_sel: selection,
                    prev_scroll: scroll,
                };
            }
            return;
        }
        "B" => {
            // Collapse the file list back down to its row; render() clears +
            // returns to the IMPORTER list once the reveal finishes. Keep the
            // screen + remote_files so the list stays drawable while it shrinks.
            crate::backend::render::distant_reveal_begin_collapse();
            return;
        }
        // Minus (-) = search, handled at the top of `input()` (hoisted because
        // swkbd is a synchronous fullscreen applet that mustn't run under the
        // LIBRARY lock).
        _ => {}
    }
    s.screen = Screen::DistantFiles { selection, scroll_offset: scroll };
}

/// The name a game of an archive is saved under on the card: its file name,
/// path separators made harmless. What the tick, the count and the A-press
/// guard all compare with the library's file names.
fn safe_file_name(name: &str) -> std::string::String {
    name.chars().map(|c| if matches!(c, '/' | '\\') { '_' } else { c }).collect()
}

/// Order an archive's games by `sort` (criterion, reversed): 0 = name, A first;
/// 1 = size, the largest first.
fn sort_remote_files(files: &mut [RemoteFile], sort: (u8, bool)) {
    let name = |f: &RemoteFile| f.name.to_lowercase();
    match sort.0 {
        1 => files.sort_by(|a, b| b.size_bytes.cmp(&a.size_bytes).then_with(|| name(a).cmp(&name(b)))),
        _ => files.sort_by(|a, b| name(a).cmp(&name(b))),
    }
    if sort.1 {
        files.reverse();
    }
}

/// Order the Flashpoint results by `sort` (criterion, reversed): 0 = name, A
/// first; 1 = developer, A first; 2 = release date, the newest first. A game
/// with no developer or no date goes last whichever way round: many fan entries
/// have none, and a column of blanks at the top would hide the rest.
fn sort_fp_candidates(c: &mut [crate::sources::flashpoint::CatalogEntry], sort: (u8, bool)) {
    use crate::sources::flashpoint::CatalogEntry;
    let title = |x: &CatalogEntry| x.title.to_lowercase();
    let rev = sort.1;
    match sort.0 {
        1 => c.sort_by(|a, b| {
            let (da, db) = (a.developer.trim().to_lowercase(), b.developer.trim().to_lowercase());
            da.is_empty()
                .cmp(&db.is_empty())
                .then_with(|| if rev { db.cmp(&da) } else { da.cmp(&db) })
                .then_with(|| title(a).cmp(&title(b)))
        }),
        2 => c.sort_by(|a, b| {
            let (ya, yb) = (a.release_date.trim(), b.release_date.trim());
            ya.is_empty()
                .cmp(&yb.is_empty())
                .then_with(|| if rev { ya.cmp(yb) } else { yb.cmp(ya) })
                .then_with(|| title(a).cmp(&title(b)))
        }),
        _ => {
            c.sort_by(|a, b| title(a).cmp(&title(b)));
            if rev {
                c.reverse();
            }
        }
    }
}

/// Compute the list of absolute indices into `files` that match the
/// active filter. `None` filter or empty string = all indices. Match is
/// substring, case-insensitive, on the filename.
pub(crate) fn filtered_indices(
    files: &[crate::net::RemoteFile],
    filter: &Option<std::string::String>,
) -> std::vec::Vec<usize> {
    let needle = filter
        .as_deref()
        .map(|s| s.trim().to_lowercase())
        .filter(|s| !s.is_empty());
    match needle {
        None => (0..files.len()).collect(),
        // The file name, or the name the row shows ("papa louie" finds
        // `papa_louie_2.swf`).
        Some(q) => files
            .iter()
            .enumerate()
            .filter(|(_, f)| f.name.to_lowercase().contains(&q) || pretty_name(&f.name).to_lowercase().contains(&q))
            .map(|(i, _)| i)
            .collect(),
    }
}

/// Absolute indices into `entries` that match the active LOCAL filter.
/// `None`/empty = all indices. Substring, case-insensitive, on the display
/// name OR the basename. Mirror of `filtered_indices` for the local list.
pub(crate) fn local_filtered_indices(
    entries: &[Entry],
    filter: &Option<std::string::String>,
    folder: &Option<std::string::String>,
) -> std::vec::Vec<usize> {
    let needle = filter
        .as_deref()
        .map(|s| s.trim().to_lowercase())
        .filter(|s| !s.is_empty());
    // A SEARCH RUNS INSIDE THE OPEN SHELF (issue #68). The two narrowings
    // compose rather than override: what is on screen is what you are searching,
    // which is the only rule that does not need explaining. TOUS is the shelf
    // that holds everything, so a search from there still reaches everything.
    if let Some(q) = needle {
        return entries
            .iter()
            .enumerate()
            .filter(|(_, e)| match folder.as_deref() {
                None => true,
                Some(f) => on_shelf(e, f),
            })
            .filter(|(_, e)| {
                e.display_name.to_lowercase().contains(&q)
                    || e.basename.to_lowercase().contains(&q)
            })
            .map(|(i, _)| i)
            .collect();
    }
    match folder.as_deref() {
        None => (0..entries.len()).collect(),
        Some(f) => {
            let picked: std::vec::Vec<usize> = entries
                .iter()
                .enumerate()
                .filter(|(_, e)| on_shelf(e, f))
                .map(|(i, _)| i)
                .collect();
            // A named folder that holds nothing does not exist -- the strip is
            // built from the games themselves. An empty answer means it emptied
            // under the player (its last game deleted or moved), and the honest
            // fallback is everything rather than a blank screen with no way out.
            //
            // The CALLERS are expected to clear `folder_filter` when they notice;
            // this is the safety net, not the mechanism, which is why the delete
            // path checks membership itself rather than testing for an empty
            // result here.
            if picked.is_empty() {
                (0..entries.len()).collect()
            } else {
                picked
            }
        }
    }
}

/// Is this game on the shelf named `name`?
///
/// The ONE place that answers it, because there are two ways to be on a shelf
/// and only one of them is visible in a path. A game is on `name` if it sits in
/// a directory called that, OR if it carries that label. Asking `e.folder ==
/// name` anywhere else is asking only half the question.
///
/// An empty `name` is not a shelf: it is ACCUEIL, "on no shelf at all", and has
/// its own test below.
pub(crate) fn on_shelf(e: &Entry, name: &str) -> bool {
    if name.is_empty() {
        return false;
    }
    // The favourites are a shelf too, the strip's second chip.
    if name == FAVORITES_SHELF {
        return crate::favorites::is_favorite(&e.basename);
    }
    e.folder == name || e.tags.iter().any(|t| t == name)
}

/// Is this game on NO shelf? Neither filed in a directory nor labelled.
pub(crate) fn on_no_shelf(e: &Entry) -> bool {
    e.folder.is_empty() && e.tags.is_empty()
}

/// Every shelf this game is on, directory first. Deduplicated: a game inside
/// `MARIO/` that is also labelled MARIO is on one shelf, not two.
pub(crate) fn shelves_of(e: &Entry) -> std::vec::Vec<std::string::String> {
    let mut out: std::vec::Vec<std::string::String> = std::vec::Vec::new();
    if !e.folder.is_empty() {
        out.push(e.folder.clone());
    }
    for t in &e.tags {
        if !out.iter().any(|x| x == t) {
            out.push(t.clone());
        }
    }
    out
}

/// Re-read every entry's labels from `tags.json` into the in-memory snapshot.
/// Called after anything that changes them, since the entries carry a copy so
/// the gallery does not take a mutex per game per frame.
pub(crate) fn refresh_tags(entries: &mut [Entry]) {
    for e in entries.iter_mut() {
        e.tags = crate::tags::tags_for(&e.basename);
    }
}

/// Is there ANOTHER listed game with this file name, somewhere else? Folders
/// (issue #68) made that possible, and it changes what deleting one may touch:
/// the companions keyed by name are then shared, not owned.
///
/// Takes the entries as a SLICE, never the lock: the only caller is
/// `delete_game`, which already holds it, and re-entering a std Mutex freezes
/// the console.
fn twin_basename_exists(entries: &[Entry], basename: &str, path: &str) -> bool {
    entries
        .iter()
        .any(|e| e.basename == basename && e.path != path)
}

/// Every folder that holds at least one game, sorted, plus how many each holds.
/// Games at the root are NOT a folder here: they are what "all" already shows,
/// and a shelf named after nothing would be the widest chip on the strip.
pub(crate) fn folder_counts(entries: &[Entry]) -> std::vec::Vec<(std::string::String, usize)> {
    let mut out: std::vec::Vec<(std::string::String, usize)> = std::vec::Vec::new();
    for e in entries {
        // Every shelf this game is on, not just the directory it sits in: a
        // labelled game counts toward each shelf it was put on, which is the
        // whole point of the labels. `shelves_of` deduplicates, so a game inside
        // `MARIO/` that also carries the MARIO label is counted once.
        for name in shelves_of(e) {
            match out.iter_mut().find(|(n, _)| *n == name) {
                Some((_, c)) => *c += 1,
                None => out.push((name, 1)),
            }
        }
    }
    out.sort_by(|a, b| a.0.to_lowercase().cmp(&b.0.to_lowercase()));
    out
}

/// Does a shelf of this name exist as a real DIRECTORY holding at least one
/// game? Emptying it then has files to move; a shelf that is only a label has
/// nothing but the label.
pub(crate) fn shelf_has_directory(entries: &[Entry], name: &str) -> bool {
    !name.is_empty() && entries.iter().any(|e| e.folder == name)
}

/// Build a `Screen::List` positioned on the entry at ABSOLUTE index `abs`,
/// translated into the current filtered view so sub-screens (OPTIONS, etc.)
/// can return to the right row WITHOUT clearing the active filter. Falls back
/// to row 0 if `abs` isn't in the filtered set.
fn list_screen_for_abs(s: &State, abs: usize) -> Screen {
    let filtered = local_filtered_indices(&s.entries, &s.local_filter, &s.folder_filter);
    let pos = filtered.iter().position(|&i| i == abs).unwrap_or(0);
    // Gallery scroll is ROW-based, not a linear item index — use the published
    // layout so returning from OPTIONS lands the game's ROW on screen. (Bug
    // fix: clamp_scroll's linear value was read as a row index → games on row
    // 2/3 came back scrolled past the end, showing a blank screen.)
    let scroll = gallery_scroll_for(pos, jouer_scroll());
    Screen::List { selection: pos, scroll_offset: scroll }
}

/// What failed, for the error page's title.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum ErrKind {
    /// A source that could not be opened (or an address not understood).
    Open,
    /// A download that failed.
    Download,
    /// An archive with no Flash game in it.
    Empty,
    /// A Flashpoint game FlashNX cannot start yet.
    Unsupported,
}

/// What A "try again" on the error page sends again: set when an operation
/// starts, so a failure can repeat it without the keyboard.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum RetryAction {
    /// Open (or, for a direct link, download) this address again, from this
    /// importer row.
    Fetch { url: std::string::String, row: usize },
    /// Download this game of the open archive again (its row in the list).
    ArchiveFile { selection: usize, scroll: usize },
    /// Download this Flashpoint result again.
    FpGame { selection: usize, scroll: usize },
}

/// Put up the importer's error page: `kind` titles it, `msg` says what happened,
/// `context` (the source or game) is its breadcrumb, and `retry` keeps the
/// action A sends again (set when the operation started), for the failures that
/// trying again can fix: a timeout, a server busy, no network.
fn import_error(s: &mut State, kind: ErrKind, msg: &str, retry: bool, context: &str) {
    // The page is full-screen and needs input to be dismissable, so end any
    // reveal still in flight. Without this a failure during the expand
    // (`DistantLoading` -> here) left the animation permanently "active", and
    // `input()` suspends every button while one is — a screen that draws fine
    // and never answers again.
    crate::backend::render::distant_reveal_cancel();
    // Where B goes back to: the list the failure came from.
    s.import_error_back = match s.screen {
        Screen::FpGallery { selection, scroll } => Screen::FpGallery { selection, scroll },
        Screen::DistantFiles { selection, scroll_offset } => Screen::DistantFiles { selection, scroll_offset },
        Screen::DistantDownloading | Screen::DistantCancelConfirm { .. } => download_back_screen(s),
        _ => Screen::Inactive,
    };
    s.distant_error = msg.to_string();
    s.import_error_kind = kind;
    s.import_error_context = context.to_string();
    if !retry {
        s.import_retry = None;
    }
    s.screen = Screen::DistantError;
}

/// `import_error`, from code that does not hold the lock.
fn set_import_error(kind: ErrKind, msg: &str, retry: bool, context: &str) {
    if let Ok(mut s) = LIBRARY.lock() {
        import_error(&mut s, kind, msg, retry, context);
    }
}

/// Forget the error page's state (it is being left).
fn clear_import_error(s: &mut State) {
    s.distant_error.clear();
    s.failed_url.clear();
    s.import_retry = None;
    s.import_fp_suggest.clear();
    s.import_error_context.clear();
    s.import_error_back = Screen::Inactive;
}

/// Leave the error page for the list it came from: the Flashpoint results, the
/// archive's games, or the importer on the row the import started from.
fn leave_import_error(s: &mut State) {
    let back = s.import_error_back;
    clear_import_error(s);
    clear_download_state(s);
    s.screen = match back {
        Screen::FpGallery { selection, scroll } if !s.cover_candidates.is_empty() => {
            Screen::FpGallery { selection: selection.min(s.cover_candidates.len() - 1), scroll }
        }
        Screen::DistantFiles { selection, scroll_offset } if !s.remote_files.is_empty() => {
            let len = filtered_indices(&s.remote_files, &s.distant_filter).len();
            let sel = selection.min(len.saturating_sub(1));
            Screen::DistantFiles { selection: sel, scroll_offset: clamp_scroll(scroll_offset, sel, DISTANT_VISIBLE_ROWS) }
        }
        _ => {
            let row = s.import_return_row;
            distant_idle_at(s, row)
        }
    };
}

/// The list a download came from, for when it ends without a game (stopped,
/// failed): the Flashpoint results, the archive's games (on the row it was
/// started from), or the importer on the source's row.
fn download_back_screen(s: &State) -> Screen {
    let (sel, scroll) = s.download_resume_pos.unwrap_or((0, 0));
    if s.download_zip_extract.is_some() || s.download_fp_direct {
        Screen::FpGallery { selection: sel.min(s.cover_candidates.len().saturating_sub(1)), scroll }
    } else if !s.remote_files.is_empty() {
        let len = filtered_indices(&s.remote_files, &s.distant_filter).len();
        let sel = sel.min(len.saturating_sub(1));
        Screen::DistantFiles { selection: sel, scroll_offset: clamp_scroll(scroll, sel, DISTANT_VISIBLE_ROWS) }
    } else {
        let row = s
            .url_history
            .iter()
            .position(|u| *u == s.download_source_url || crate::sources::wayback_raw(u) == s.download_source_url)
            .map(|abs| importer_row_for_abs(s, abs))
            .unwrap_or(s.import_return_row);
        distant_idle_at(s, row)
    }
}

/// Stop the download in flight and go back to the list it came from, with a
/// word saying so. B used to abort at once onto a red error page that needed a
/// second press to leave, even for a 3.9 GB game at 95 %.
fn stop_download(s: &mut State) {
    net::cancel_download();
    let back = download_back_screen(s);
    clear_download_state(s);
    s.import_retry = None;
    s.screen = back;
    set_toast(s, crate::loc::s().err_dl_cancelled.to_string(), TOAST_INFO);
}

/// Drop the half-finished download bookkeeping so none of it leaks into the
/// next one. Callers that need `download_resume_pos` must `take()` it FIRST —
/// this clears it too.
fn clear_download_state(s: &mut State) {
    s.download_file_name.clear();
    s.download_out_path.clear();
    s.download_zip_extract = None;
    s.download_fp_direct = false;
    s.download_cover_url = None;
    s.download_title = None;
    s.download_resume_pos = None;
    // The companion phase too. It was left armed: cancelling a multi-file game
    // mid-companion kept `dl_companion_active` set with a queue still in it, so
    // the NEXT download's completion walked into the companion branch, wrote the
    // leftover names into the new game's `.files/` tree and finalized the wrong
    // one. `dl_companion_done` also drives the progress line, which then counted
    // from wherever the abandoned download had got to.
    s.dl_companion_active = false;
    s.dl_companion_base.clear();
    s.dl_companion_dir.clear();
    s.dl_companion_current.clear();
    s.dl_companion_queue.clear();
    s.dl_companion_seen.clear();
    s.dl_companion_done = 0;
}

/// First `n` bytes of a file, for magic-number sniffing. Empty on any error or
/// short file — both mean "not the file we expected", which is what callers do
/// with it. Deliberately NOT `read_file_bounded`: this must stay cheap on a
/// multi-GB GameZIP.
fn file_head(path: &str, n: usize) -> std::vec::Vec<u8> {
    use std::io::Read;
    let mut buf = std::vec![0u8; n];
    // Not a `match` guard: a variable bound in a pattern stays immutable until the
    // guard ends, so `read_exact` cannot borrow the handle there.
    match std::fs::File::open(path) {
        Ok(mut f) => {
            if f.read_exact(&mut buf).is_ok() {
                buf
            } else {
                std::vec::Vec::new()
            }
        }
        Err(_) => std::vec::Vec::new(),
    }
}

/// Rows of the OPTIONS > DOSSIER picker for `game_idx`: the root, then every
/// folder that already holds a game, then "new folder". Rebuilt each time from
/// the entries, so it can never describe shelves that no longer exist.
///
/// Returns the folder NAMES; the caller maps row 0 to the root and the last row
/// to the keyboard.
fn game_folder_rows(s: &State) -> std::vec::Vec<std::string::String> {
    folder_counts(&s.entries).into_iter().map(|(n, _)| n).collect()
}

fn handle_game_folder_input(s: &mut State, button: &str, game_idx: usize, mut selection: usize) {
    let folders = game_folder_rows(s);
    // root + folders + "new folder"
    let last = folders.len() + 1;
    // CLAMPED before anything reads it. `selection` does not only come from the
    // D-pad: the touch layer writes it from the table the PREVIOUS frame
    // published, and A is a toggle now, so one press can make a row disappear
    // while that table still lists it. A row index one past the end reaching
    // `folders[selection - 1]` is not a wrong menu entry, it is an out-of-range
    // index -- and `panic = "abort"` makes that a console fatal.
    selection = selection.min(last);
    match button {
        "Up" | "StickLUp" => selection = list_step_wrap(selection, -1, last + 1),
        "Down" | "StickLDown" => selection = list_step_wrap(selection, 1, last + 1),
        "A" => {
            // The last row opens swkbd and is hoisted out of here, like RENOMMER:
            // the keyboard must never run under the library lock.
            if selection == last {
                return;
            }
            let target = if selection == 0 {
                std::string::String::new()
            } else {
                // Belt and braces with the clamp above: this is the one place
                // that indexes, so it is the one place that must never trust.
                match folders.get(selection - 1) {
                    Some(n) => n.clone(),
                    None => return,
                }
            };
            apply_game_folder(s, game_idx, &target);
            return;
        }
        "X" => {
            // Empty a shelf: rows 1..=folders.len() are the real folders; the
            // root and "new folder" rows have nothing to empty.
            if selection >= 1 && selection <= folders.len() {
                s.screen = Screen::GameFolderDelete {
                    game_idx,
                    folder_idx: selection - 1,
                    selection: 0, // cursor on Cancel: this undoes deliberate work
                };
            }
            return;
        }
        "B" | "Minus" => {
            s.screen = Screen::OptionsModal { game_idx, selection: opt_row("DOSSIER") };
            return;
        }
        _ => {}
    }
    s.screen = Screen::GameFolderPicker { game_idx, selection };
}

fn handle_game_folder_delete_input(
    s: &mut State,
    button: &str,
    game_idx: usize,
    folder_idx: usize,
    mut selection: usize,
) {
    match button {
        "Up" | "Down" | "StickLUp" | "StickLDown" | "Left" | "Right" | "StickLLeft" | "StickLRight" => {
            selection = 1 - selection.min(1);
        }
        "A" => {
            // Cancel on the left (0), empty on the right (1), like the
            // console's own questions.
            if selection == 1 {
                // `empty_folder` re-sorts AND rewrites the paths, so it is the
                // only thing that can tell us where this game ended up. Pinning
                // from out here by path could not work: the path is what it
                // changes.
                let idx = empty_folder(s, folder_idx, game_idx);
                // The shelf is gone, so its row is gone: land on the root, which
                // is where its games now are.
                s.screen = Screen::GameFolderPicker { game_idx: idx, selection: 0 };
            } else {
                // NO. Back to the row the player was on, not to the top —
                // declining must not also move the cursor.
                s.screen = Screen::GameFolderPicker { game_idx, selection: folder_idx + 1 };
            }
            return;
        }
        "B" | "Minus" => {
            s.screen = Screen::GameFolderPicker { game_idx, selection: folder_idx + 1 };
            return;
        }
        _ => {}
    }
    s.screen = Screen::GameFolderDelete { game_idx, folder_idx, selection };
}

/// Take every game off a shelf: the ones physically inside its directory move
/// back to the root, the ones there by label lose the label. Then remove the
/// directory, if there was one.
///
/// Not destructive: this is the move that was already validated, run backwards
/// once per game. A game that refuses to move keeps the folder alive, and the
/// `remove_dir` at the end fails harmlessly — better a folder that outlives its
/// last game than a game left pointing at a directory that no longer exists.
/// `pinned` is an entry index the caller needs to keep track of across the
/// re-sort; the RETURNED index is where that same game ended up. Pinning by path
/// from outside cannot work: emptying a shelf is precisely what rewrites the
/// paths of the games on it, so an outside pin always misses for the one game
/// that matters -- the one whose OPTIONS are open, which is on that shelf in the
/// normal flow -- and falls back to a stale index into a freshly re-sorted list.
/// SUPPRIMER on that index deletes a game the player never chose.
fn empty_folder(s: &mut State, folder_idx: usize, pinned: usize) -> usize {
    let folders = folder_counts(&s.entries);
    let Some((name, _)) = folders.get(folder_idx).cloned() else {
        return pinned;
    };
    // Is this shelf backed by a real directory at all? Asked BEFORE the moves,
    // which are what empty it.
    let had_directory = shelf_has_directory(&s.entries, &name);
    // Followed ACROSS the moves below, which rewrite it.
    let mut pinned_path = s.entries.get(pinned).map(|e| e.path.clone());
    let victims: std::vec::Vec<usize> = s
        .entries
        .iter()
        .enumerate()
        // Only the games PHYSICALLY in the directory are moved. A game that is
        // on this shelf because of a label has no file to move: its membership
        // is taken off wholesale below, in one write.
        .filter(|(_, e)| e.folder == name)
        .map(|(i, _)| i)
        .collect();
    let mut moved = 0usize;
    for i in victims {
        let Some((path, basename)) = s.entries.get(i).map(|e| (e.path.clone(), e.basename.clone()))
        else {
            continue;
        };
        // A stranded game counts as MOVED for the bookkeeping even though the
        // operation failed: its `.swf` really is at that path now, and the entry
        // has to say so. It just does not count toward the tally the toast
        // reports, which is about work that went cleanly.
        let (new_path, clean) = match move_game_to_folder(&path, &basename, "") {
            MoveOutcome::Moved(p) => (Some(p), true),
            MoveOutcome::Stranded(p) => (Some(p), false),
            MoveOutcome::Refused { .. } => (None, false),
        };
        if let Some(new_path) = new_path {
            note_file_removed(&path);
            note_file_created(&new_path);
            if pinned_path.as_deref() == Some(path.as_str()) {
                pinned_path = Some(new_path.clone());
            }
            if let Some(e) = s.entries.get_mut(i) {
                e.folder = folder_of(&new_path);
                e.path = new_path;
            }
            if clean {
                moved += 1;
            }
        }
    }
    // The label half of the shelf, in one write. Emptying a shelf has to empty
    // it whichever way a game got onto it, or a shelf would refuse to disappear
    // and nobody could see why.
    // `None` = the table could not be written, so the labels are still on the
    // card and this shelf has NOT been emptied. The verdict below asks the
    // library, which still carries them, so it reports the failure on its own.
    if crate::tags::remove_tag_everywhere(&name).unwrap_or(0) > 0 {
        refresh_tags(&mut s.entries);
    }
    // Removed where the games ACTUALLY were, not where the primary root is: a
    // shelf found under a legacy root lives there, and `remove_dir` on the
    // primary would fail while the real directory stayed behind.
    //
    // Only if there IS one. A shelf that exists purely as a label has no
    // directory to remove, and asking for one would be a guess at a path that
    // was never involved.
    let dir = if had_directory { folder_dir_of(&name) } else { std::string::String::new() };
    let gone = !dir.is_empty() && std::fs::remove_dir(&dir).is_ok();
    crate::sd::commit();
    log(&std::format!(
        "library: folder {} emptied ({} game(s) home, dir {} removed={})\n",
        name, moved, dir, gone,
    ));
    // ONLY if the shelf emptied is the one on screen. The folder comes from the
    // picker's cursor and has nothing to do with where the player is standing:
    // emptying MARIO used to throw them off SONIC, and — worse — silently widen
    // a search running inside SONIC to the whole library, because
    // `local_filtered_indices` composes the two.
    if s.folder_filter.as_deref() == Some(name.as_str()) {
        s.folder_filter = None;
        set_active_folder(None);
    }
    if let Ok(mut g) = FOLDER_SORTS.lock() {
        g.retain(|(n, _, _)| *n != name);
    }
    persist_folder_sorts();
    sort_entries(&mut s.entries, current_sort_mode(), current_sort_reverse());
    // Say what actually happened. A game whose destination name is already taken
    // is refused (`move_game_to_folder`), which keeps the shelf alive -- and a
    // green "moved" over a shelf still sitting there is worse than no message.
    // Did the shelf actually go? Asked of the LIBRARY, not of `remove_dir`.
    // A shelf can hold a subdirectory of its own (the scan goes three deep) or
    // a stray readme, and `remove_dir` refuses a directory that is not empty --
    // so a move where every single game came home reported a red SD error
    // because a `MARIO/NES/` folder was left behind. What the player asked for
    // is that no game be on this shelf any more; that is what we check.
    let empty_now = !s.entries.iter().any(|e| on_shelf(e, &name));
    if empty_now {
        // `folder_emptied`, not `games_dir_moved`: a shelf can empty with nothing
        // having moved at all, when every game on it was there by label.
        set_toast(s, crate::loc::s().folder_emptied.to_string(), TOAST_OK);
    } else {
        set_toast(s, crate::loc::s().err_sd_write.to_string(), TOAST_ERR);
    }
    pinned_path
        .and_then(|p| s.entries.iter().position(|e| e.path == p))
        .unwrap_or_else(|| pinned.min(s.entries.len().saturating_sub(1)))
}

/// Where a shelf's directory actually is. Folders live under whichever search
/// root their games were found in, and only the primary is the one new games
/// land in — so a folder under a legacy root is not `primary_root()/name`.
fn folder_dir_of(name: &str) -> std::string::String {
    for root in search_roots() {
        let d = std::format!("{}/{}", root.trim_end_matches('/'), name);
        if dir_exists(&d) {
            return d;
        }
    }
    std::format!("{}/{}", primary_root(), name)
}

/// Put a game on a shelf, or take it off one.
///
/// Nothing is MOVED here any more. A shelf membership is a label in
/// [crate::tags], so this toggles: press A on a shelf the game is already on and
/// it comes off. That is what lets one game sit on several shelves at once,
/// which a directory could never express because a file is in one place.
///
/// The one exception is ACCUEIL (an empty `target`), which means "on no shelf at
/// all". Labels alone cannot deliver that for a game that physically lives in a
/// directory, so that row -- and only that row -- still moves the file back to
/// the root. It is the only thing in the app that writes to the card's layout.
pub(crate) fn apply_game_folder(s: &mut State, game_idx: usize, target: &str) {
    let Some((path, basename, folder)) = s
        .entries
        .get(game_idx)
        .map(|e| (e.path.clone(), e.basename.clone(), e.folder.clone()))
    else {
        return;
    };
    // The row of the shelf being aimed at, in the list as it stands. Computed
    // fresh at each exit, because toggling a label can CREATE a shelf or make
    // the last one vanish, and every row below it then shifts. Every exit --
    // refusing, failing and succeeding -- has to land on the row the player
    // pressed A on.
    let row_of = |s: &State, target: &str| -> usize {
        if target.is_empty() {
            0
        } else {
            folder_counts(&s.entries)
                .iter()
                .position(|(n, _)| n == target)
                .map_or(0, |i| i + 1)
        }
    };
    // Keep the open shelf honest after a change: a shelf that just lost its last
    // game no longer exists, and leaving the filter on it would show the whole
    // library under a header naming something that is gone.
    let settle = |s: &mut State| {
        // A toggle can take the last game off a shelf, which ends it.
        prune_folder_sorts(&s.entries);
        let vanished = match s.folder_filter.as_deref() {
            Some(f) if !f.is_empty() => !s.entries.iter().any(|e| on_shelf(e, f)),
            _ => false,
        };
        if vanished {
            s.folder_filter = None;
            set_active_folder(None);
            sort_entries(&mut s.entries, current_sort_mode(), current_sort_reverse());
        }
    };

    // ── ACCUEIL: off every shelf ────────────────────────────────
    if target.is_empty() {
        // Nothing physical to do: the labels ARE the whole membership.
        if folder.is_empty() {
            let Some(had_labels) = crate::tags::clear_for(&basename) else {
                set_toast(s, crate::loc::s().err_sd_write.to_string(), TOAST_ERR);
                let row = row_of(s, target);
                s.screen = Screen::GameFolderPicker { game_idx, selection: row };
                return;
            };
            if had_labels == 0 {
                // Already on no shelf: say so rather than report work that did
                // not happen.
                let row = row_of(s, target);
                set_toast(s, crate::loc::s().toast_folder_same.to_string(), TOAST_ERR);
                s.screen = Screen::GameFolderPicker { game_idx, selection: row };
                return;
            }
            refresh_tags(&mut s.entries);
            settle(s);
            set_toast(s, crate::loc::s().toast_folder_off.to_string(), TOAST_OK);
            // By PATH: `settle` may have re-sorted the library under us, and the
            // sort mode itself depends on which shelf is open.
            let idx = s.entries.iter().position(|e| e.path == path).unwrap_or(game_idx);
            let row = row_of(s, target);
            s.screen = Screen::GameFolderPicker { game_idx: idx, selection: row };
            return;
        }
        // Physically filed. MOVE FIRST, strip the labels only once the file has
        // actually come home.
        //
        // The other way round loses them for nothing: `move_game_to_folder`
        // refuses whenever a namesake already sits at the root -- the ordinary
        // two-games-one-file-name case this whole design accepts -- and the
        // labels were already erased from the card by then, under a red toast
        // saying the operation had failed.
        let outcome = move_game_to_folder(&path, &basename, "");
        // Where the `.swf` ended up, whatever the verdict. A stranded game is a
        // FAILURE the player must be told about, but its file really has moved,
        // and an entry naming a path with no file on it is worse: `delete_game`
        // sweeps the directory it reads off that path.
        let landed = match outcome {
            MoveOutcome::Moved(ref np) | MoveOutcome::Stranded(ref np) => Some(np.clone()),
            MoveOutcome::Refused { .. } => None,
        };
        let Some(new_path) = landed else {
            // Nothing moved and nothing was erased. The game is exactly as it
            // was, which is the only state worth leaving it in.
            // Say WHICH failure. A name already taken at the root is a
            // diagnosis the player can act on -- another game owns that file
            // name there -- and reporting it as a card that refused to write is
            // both false and useless.
            let taken = matches!(outcome, MoveOutcome::Refused { name_taken: true });
            set_toast(
                s,
                if taken {
                    crate::loc::fill_name(crate::loc::s().toast_name_taken, &basename)
                } else {
                    crate::loc::s().err_sd_write.to_string()
                },
                TOAST_ERR,
            );
            let row = row_of(s, target);
            s.screen = Screen::GameFolderPicker { game_idx, selection: row };
            return;
        };
        // The scan index still holds the old path; a stale "this file is absent"
        // answer there would make the game unlaunchable until the next boot.
        note_file_removed(&path);
        note_file_created(&new_path);
        let now_in = folder_of(&new_path);
        if let Some(e) = s.entries.get_mut(game_idx) {
            e.path = new_path.clone();
            e.folder = now_in;
        }
        // The file is home; the labels can go now.
        let cleared = crate::tags::clear_for(&basename).is_some();
        if cleared {
            refresh_tags(&mut s.entries);
        }
        settle(s);
        let ok = matches!(outcome, MoveOutcome::Moved(_)) && cleared;
        set_toast(
            s,
            if ok {
                crate::loc::s().toast_folder_off.to_string()
            } else {
                crate::loc::s().err_sd_write.to_string()
            },
            if ok { TOAST_OK } else { TOAST_ERR },
        );
        let idx = s
            .entries
            .iter()
            .position(|e| e.path == new_path)
            .unwrap_or(game_idx);
        let row = row_of(s, target);
        s.screen = Screen::GameFolderPicker { game_idx: idx, selection: row };
        return;
    }

    // ── A named shelf: toggle the label ─────────────────────────────────────
    //
    // A game that sits in a REAL directory of this name is on this shelf because
    // of where it is, and no label can take it off. Say that instead of quietly
    // doing nothing, or of adding a label that changes nothing.
    if folder == target {
        let row = row_of(s, target);
        set_toast(s, crate::loc::s().toast_folder_physical.to_string(), TOAST_ERR);
        s.screen = Screen::GameFolderPicker { game_idx, selection: row };
        return;
    }
    // `None` = nothing was written, so nothing may be shown as done. The table
    // is sealed when `tags.json` exists but could not be read, and a green toast
    // over a label that will be gone at the next boot is the worst of both.
    let Some(now_on) = crate::tags::toggle(&basename, target) else {
        set_toast(s, crate::loc::s().err_sd_write.to_string(), TOAST_ERR);
        let row = row_of(s, target);
        s.screen = Screen::GameFolderPicker { game_idx, selection: row };
        return;
    };
    refresh_tags(&mut s.entries);
    settle(s);
    set_toast(
        s,
        if now_on {
            crate::loc::fill_name(crate::loc::s().toast_folder_on, target)
        } else {
            crate::loc::fill_name(crate::loc::s().toast_folder_off_of, target)
        },
        TOAST_OK,
    );
    // Re-find the game by PATH: `settle` may have re-sorted the library under us.
    let idx = s
        .entries
        .iter()
        .position(|e| e.path == path)
        .unwrap_or(game_idx);
    let row = row_of(s, target);
    s.screen = Screen::GameFolderPicker { game_idx: idx, selection: row };
}

/// The tile a game owns, or None when its path cannot cross FFI.
fn tile_id_for(swf_path: &str) -> Option<u64> {
    let c = std::ffi::CString::new(swf_path).ok()?;
    Some(unsafe { nsp_program_id_for(c.as_ptr()) })
}

/// Where a failed install leaves its reason, for reading back over FTP.
const TILE_ERROR_PATH: &str = "sdmc:/switch/FlashNX/tile_error.txt";

/// Where a tile records which game it launches.
///
/// Named after the program ID and not the game, for one reason: the tile hands
/// this path to FlashNX as a command-line argument, and the homebrew ABI splits
/// arguments on spaces. Sixteen hex digits never contain one; `Papa Louie 2`
/// does. See forwarder/source/main.c.
fn tile_cfg_path(program_id: u64) -> std::string::String {
    std::format!("{}/{:016X}.cfg", TILES_DIR, program_id)
}

/// The folder of the tiles' configs, one `<program ID>.cfg` per tile.
const TILES_DIR: &str = "sdmc:/switch/FlashNX/tiles";

/// The tiles' configs: (program ID, the game's path), read from
/// `sdmc:/switch/FlashNX/tiles/` once and kept until a config is written.
static TILE_INDEX: Mutex<Option<std::vec::Vec<(u64, std::string::String)>>> = Mutex::new(None);

/// Listed by C++ and read with `read_sd_text`, never with `std::fs`: Rust's
/// `read_dir` corrupts entry names on Horizon (see `list_dirs`), and a
/// `<ID>.cfg` that loses two bytes no longer parses, so the index came back
/// empty and every game fell back to the hash of its CURRENT path, which a
/// moved game's tile does not carry. `read_to_string` also sizes its buffer
/// from fstat, which is bogus in applet mode.
fn tile_index() -> std::vec::Vec<(u64, std::string::String)> {
    if let Ok(g) = TILE_INDEX.lock() {
        if let Some(v) = g.as_ref() {
            return v.clone();
        }
    }
    let mut v = std::vec::Vec::new();
    for name in list_files_top(TILES_DIR) {
        let Some(hex) = name.strip_suffix(".cfg") else { continue };
        let Ok(id) = u64::from_str_radix(hex, 16) else { continue };
        if let Ok(text) = read_sd_text(&std::format!("{}/{}", TILES_DIR, name), 4096) {
            v.push((id, text.trim().to_string()));
        }
    }
    if let Ok(mut g) = TILE_INDEX.lock() {
        *g = Some(v.clone());
    }
    v
}

fn tile_index_forget() {
    if let Ok(mut g) = TILE_INDEX.lock() {
        *g = None;
    }
}

/// The program ID of the tile that launches the game at `swf_path`.
///
/// Found by what the configs SAY, not by hashing the path: the hash is the
/// path's at install time, and a game moved since (into a folder, or with the
/// whole library) would no longer find its own tile. The row then read "no
/// icon", A installed a second one, and deleting the game left the first on
/// the HOME menu for good. The hash is only the fallback, for a tile whose
/// config is missing.
fn tile_for(swf_path: &str) -> Option<u64> {
    tile_index()
        .into_iter()
        .find(|(_, p)| p == swf_path)
        .map(|(id, _)| id)
        .or_else(|| tile_id_for(swf_path))
}

/// A game moved from `from` to `to` (or, with `prefix`, every game under the
/// folder `from` moved under `to`): the HOME icons launching it are pointed at
/// its new place. The installed tile keeps its program ID; only the path in
/// its config changes.
fn tiles_follow(from: &str, to: &str, prefix: bool) {
    let mut wrote = false;
    for (id, p) in tile_index() {
        let new_path = if prefix {
            match p.strip_prefix(from) {
                Some(rest) if rest.starts_with('/') => std::format!("{}{}", to, rest),
                _ => continue,
            }
        } else if p == from {
            to.to_string()
        } else {
            continue;
        };
        let cfg = tile_cfg_path(id);
        if let Ok(c) = std::ffi::CString::new(cfg.as_str()) {
            let b = new_path.as_bytes();
            let ok = unsafe { swf_picker_write_file(c.as_ptr(), b.as_ptr(), b.len() as u32) } == 1;
            log(&std::format!("tile: {:016X} now launches {}{}\n", id, new_path, if ok { "" } else { " (NOT written)" }));
            wrote |= ok;
        }
    }
    if wrote {
        crate::sd::commit();
        tile_index_forget();
    }
}

/// True when this game already has a tile on the HOME menu.
///
/// Asked of the console rather than of the card: somebody can uninstall a tile
/// from the HOME menu itself, and the row has to say what is actually there.
fn shortcut_here(swf_path: &str) -> bool {
    match tile_for(swf_path) {
        Some(id) => (unsafe { nsp_tile_exists(id) }) == 1,
        None => false,
    }
}

/// Uninstall a game's tile and drop its config file. True when one went away.
fn tile_remove(swf_path: &str) -> bool {
    let Some(id) = tile_for(swf_path) else { return false };
    if unsafe { nsp_tile_exists(id) } != 1 {
        return false;
    }
    let removed = unsafe { nsp_remove_tile(id) } == 1;
    if removed {
        // The config outlives the tile otherwise, and the next game whose path
        // hashes here would inherit it.
        let _ = std::fs::remove_file(tile_cfg_path(id));
        tile_index_forget();
    }
    removed
}

/// Favourite on or off from the options (`want`, or the other way when None),
/// keeping the panel on the SAME game: favourites jump to the top of the
/// gallery, so the game's index changes.
fn options_set_favorite(s: &mut State, game_idx: usize, selection: usize, want: Option<bool>) {
    // By PATH, not by basename. Folders (issue #68) let two games share a file
    // name, and re-pointing by name could land the panel on the other one --
    // after which every row of it, RENOMMER and SUPPRIMER included, would act on
    // a game the player never opened.
    let ident = s.entries.get(game_idx).map(|e| (e.basename.clone(), e.path.clone()));
    if let Some((basename, path)) = ident {
        let fav = crate::favorites::is_favorite(&basename);
        if want.is_some_and(|w| w == fav) {
            return;
        }
        crate::favorites::toggle(&basename);
        crate::sfx::value_changed();
        sort_entries(&mut s.entries, current_sort_mode(), current_sort_reverse());
        let new_idx = s.entries.iter().position(|e| e.path == path).unwrap_or(game_idx);
        s.screen = Screen::OptionsModal { game_idx: new_idx, selection };
    }
}

fn handle_options_input(s: &mut State, button: &str, game_idx: usize, mut selection: usize) {
    let total = OPTIONS_ENTRIES.len();
    let key = OPTIONS_ENTRIES.get(selection).copied().unwrap_or("");
    // Left / Right change a setting where it is shown; on an action row they
    // do nothing (an action is never one sideways press away).
    let dir: i32 = match button {
        "Left" | "StickLLeft" => -1,
        "Right" | "StickLRight" => 1,
        _ => 0,
    };
    if dir != 0 {
        if key == "FAVORI" {
            options_set_favorite(s, game_idx, selection, Some(dir > 0));
            return;
        }
        if options_panel::options_step(s, game_idx, key, dir) {
            crate::sfx::value_changed();
        }
        s.screen = Screen::OptionsModal { game_idx, selection };
        return;
    }
    match button {
        // A fresh press wraps round (Up on the first row reaches Supprimer), a
        // held direction stops at the end.
        "Up" | "StickLUp" => selection = list_step_wrap(selection, -1, total),
        "Down" | "StickLDown" => selection = list_step_wrap(selection, 1, total),
        "A" => {
            if options_panel::is_setting(key) {
                if options_panel::options_step(s, game_idx, key, crate::STEP_CYCLE as i32) {
                    crate::sfx::value_changed();
                }
                s.screen = Screen::OptionsModal { game_idx, selection };
                return;
            }
            // `.get`, not `[..]`: `selection` can arrive from the touch layer,
            // and with `panic = "abort"` a row that is out of range is a console
            // fatal rather than a wrong menu entry.
            let Some(&entry) = OPTIONS_ENTRIES.get(selection) else {
                return;
            };
            match entry {
                "FAVORI" => {
                    options_set_favorite(s, game_idx, selection, None);
                    return;
                }
                "TOUCHES" => {
                    // Open the TOUCHES sub-menu (edit / apply / share / revert).
                    // #20 regroup: apply+share used to be sibling OPTIONS rows.
                    goto_touches_menu(s, game_idx, 0);
                    return;
                }
                "RACCOURCI" => {
                    // Write a `.nro` into `sdmc:/switch/` that launches this one
                    // game, so hbmenu and Sphaira list it under its own name
                    // instead of "FlashNX | angry-birds.swf".
                    //
                    // Done inline rather than hoisted like RENOMMER: no keyboard
                    // and no network, so it still fits in one press.
                    let Some(e) = s.entries.get(game_idx) else { return };
                    // Without the extension: `display_name` is the on-SD file
                    // name when the game has no rename override, and hbmenu shows
                    // this string verbatim under the tile. The first shortcut
                    // ever made read "Agent P Strikes Back.swf" on the home
                    // screen, which is a file name, not a game.
                    let basename = e.basename.clone();
                    let (path, title) = (
                        e.path.clone(),
                        e.display_name
                            .strip_suffix(".swf")
                            .or_else(|| e.display_name.strip_suffix(".SWF"))
                            .unwrap_or(&e.display_name)
                            .to_string(),
                    );
                    let Some(program_id) = tile_id_for(&path) else { return };
                    let lc = crate::loc::s();

                    // A toggle, not an add: pressed twice it used to say "added"
                    // again, which reads as piling up duplicates when there has
                    // only ever been one.
                    options_panel::game_prefs_forget();
                    if shortcut_here(&path) {
                        let removed = tile_remove(&path);
                        crate::sd::commit();
                        set_toast(
                            s,
                            if removed { lc.shortcut_removed } else { lc.shortcut_err }.to_string(),
                            if removed { TOAST_OK } else { TOAST_ERR },
                        );
                        return;
                    }

                    // The config first: the tile is what the player sees, so it
                    // must never appear pointing at a file that is not there.
                    let cfg = tile_cfg_path(program_id);
                    let bytes = path.as_bytes();
                    let wrote_cfg = match std::ffi::CString::new(cfg.as_str()) {
                        Ok(c) => {
                            let rc = unsafe {
                                swf_picker_write_file(
                                    c.as_ptr(),
                                    bytes.as_ptr(),
                                    bytes.len() as u32,
                                )
                            };
                            rc == 1
                        }
                        Err(_) => false,
                    };
                    if !wrote_cfg {
                        log(&std::format!("tile: could not write {}\n", cfg));
                        set_toast(s, lc.shortcut_err.to_string(), TOAST_ERR);
                        return;
                    }

                    // The game's box art, as a 256x256 JPEG. None when the game
                    // has no cover, which is fine: the tile installs anyway and
                    // the HOME menu draws its own placeholder.
                    let icon = crate::covers::shortcut_icon_jpeg(&basename);
                    let mut err = [0u8; 96];
                    let rc = match (
                        std::ffi::CString::new(title.as_str()),
                        std::ffi::CString::new(path.as_str()),
                    ) {
                        (Ok(t), Ok(p)) => unsafe {
                            nsp_install_tile(
                                program_id,
                                t.as_ptr(),
                                p.as_ptr(),
                                icon.as_ref().map_or(core::ptr::null(), |v| v.as_ptr()),
                                icon.as_ref().map_or(0, |v| v.len() as u32),
                                err.as_mut_ptr() as *mut core::ffi::c_char,
                                err.len() as u32,
                            )
                        },
                        _ => 0,
                    };
                    let reason = std::ffi::CStr::from_bytes_until_nul(&err)
                        .ok()
                        .and_then(|c| c.to_str().ok())
                        .unwrap_or("?")
                        .to_string();
                    if rc == 1 {
                        crate::sd::commit();
                        tile_index_forget();
                        set_toast(s, lc.shortcut_ok.to_string(), TOAST_OK);
                    } else {
                        // The config would otherwise sit there naming a tile
                        // that does not exist.
                        let _ = std::fs::remove_file(&cfg);
                        log(&std::format!("tile: install failed for {} ({})\n", path, reason));
                        // And on the card, because the toast is gone in three
                        // seconds and the log only exists when somebody is
                        // watching over nxlink. A failure that leaves a file
                        // behind can be read afterwards, over FTP, by whoever is
                        // trying to work out what happened.
                        let note = std::format!("{}\n{}\n{}\n", path, title, reason);
                        if let Ok(c) = std::ffi::CString::new(TILE_ERROR_PATH) {
                            let b = note.as_bytes();
                            unsafe {
                                swf_picker_write_file(c.as_ptr(), b.as_ptr(), b.len() as u32)
                            };
                        }
                        // The reason is in the toast and not only in the log: a
                        // console without signature patches and a full card fail
                        // the same way from the menu, and the player is the one
                        // who has to tell them apart.
                        set_toast(
                            s,
                            std::format!("{} ({})", lc.shortcut_err, reason),
                            TOAST_ERR,
                        );
                    }
                    return;
                }
                "RENOMMER" => {
                    // Handled at the top of `input()` (hoisted out
                    // because it calls into swkbd). No-op here.
                    return;
                }
                "JAQUETTE" => {
                    // Handled at the top of `input()` (hoisted — Flashpoint
                    // search is a synchronous HTTPS call). No-op here.
                    return;
                }
                "DOSSIER" => {
                    s.screen = Screen::GameFolderPicker { game_idx, selection: 0 };
                    return;
                }
                "SUPPRIMER" => {
                    // On "Cancel": deleting is one press away, never zero.
                    s.screen = Screen::DeleteConfirm { game_idx, selection: 0 };
                    return;
                }
                _ => {}
            }
        }
        // + opened the panel, so + closes it too, like B.
        "B" | "Minus" | "Plus" => {
            s.screen = list_screen_for_abs(s, game_idx);
            return;
        }
        // Back to the global default for this setting.
        "X" => {
            if options_panel::options_reset(s, game_idx, key) {
                crate::sfx::value_changed();
            }
        }
        // The previous / next game of the list behind, on the same row: setting
        // up several games one after the other without closing the panel each
        // time. Stops at the ends (the list behind follows the cursor).
        "L" | "R" => {
            // One game per press: a held shoulder would race through the list,
            // each step reading the next game's settings from the card.
            if !nav_may_wrap() {
                return;
            }
            let shown = local_filtered_indices(&s.entries, &s.local_filter, &s.folder_filter);
            if let Some(pos) = shown.iter().position(|&i| i == game_idx) {
                let next = if button == "L" {
                    pos.checked_sub(1)
                } else if pos + 1 < shown.len() {
                    Some(pos + 1)
                } else {
                    None
                };
                if let Some(n) = next {
                    s.screen = Screen::OptionsModal { game_idx: shown[n], selection };
                    return;
                }
            }
        }
        _ => {}
    }
    s.screen = Screen::OptionsModal { game_idx, selection };
}

/// Build a Flashpoint search query from a game's display name: drop a trailing
/// `.swf`, turn `_`/`-` into spaces so "Super_Mario_63" matches "Super Mario 63".
fn cover_query_from_name(name: &str) -> std::string::String {
    // `is_char_boundary` BEFORE the slice: `name` is the display name, i.e.
    // whatever the RENOMMER keyboard wrote, and since issue #75 that can be
    // Chinese. `name[name.len() - 4..]` on a five-character Han title indexes
    // into the middle of the fourth character, and a &str slice off a boundary
    // panics -- with `panic = "abort"`, straight to an Atmosphere fatal. The
    // second slice needs no guard: it only runs once the tail compared equal to
    // the ASCII ".swf", which puts the cut on a boundary by construction.
    let cut = name.len().wrapping_sub(4);
    let base = if name.len() > 4
        && name.is_char_boundary(cut)
        && name[cut..].eq_ignore_ascii_case(".swf")
    {
        &name[..cut]
    } else {
        name
    };
    let spaced = base.replace(['_', '-'], " ");
    let mut toks: std::vec::Vec<&str> = spaced.split_whitespace().collect();
    // Drop a single trailing download-id suffix (e.g. `pursuit-of-hat-2-15938d603`,
    // `haunt-the-house-719579f9`): itch/Flashpoint mirrors append these and they
    // make smartSearch return nothing. Keep at least one real word.
    if toks.len() > 1 && looks_like_download_hash(toks[toks.len() - 1]) {
        toks.pop();
    }
    toks.join(" ")
}

/// True for a trailing download-id token like `15938d603` / `719579f9`: hex,
/// at least 6 chars, AND containing a digit (so real hex-letter words such as
/// "facade"/"decade" are left alone). Strips only what is clearly an id suffix.
fn looks_like_download_hash(tok: &str) -> bool {
    tok.len() >= 6
        && tok.bytes().all(|b| b.is_ascii_hexdigit())
        && tok.bytes().any(|b| b.is_ascii_digit())
}

/// OPTIONS > JAQUETTE: search Flashpoint by the game's name and open the cover
/// picker. Gated on the online-covers toggle (OFF → notice). Called from
/// `input()` only — runs a synchronous HTTPS search WITHOUT the LIBRARY lock.
fn run_cover_search_flow(game_idx: usize) {
    let name = match LIBRARY.lock() {
        Ok(g) => match g.entries.get(game_idx) {
            Some(e) => e.display_name.clone(),
            None => return,
        },
        Err(_) => return,
    };
    let query = cover_query_from_name(&name);
    run_cover_search_with(game_idx, query);
}

/// CoverPicker > Minus: re-open the keyboard pre-filled with the last query so
/// the user can fix a name the filename couldn't match (e.g. `catmario` ->
/// `cat mario`), then re-run the search. Called from `input()` only — swkbd +
/// HTTPS must run WITHOUT the LIBRARY lock.
fn run_cover_research_flow(game_idx: usize) {
    let current = match LIBRARY.lock() {
        Ok(g) if !g.cover_query.is_empty() => g.cover_query.clone(),
        Ok(g) => g
            .entries
            .get(game_idx)
            .map(|e| cover_query_from_name(&e.display_name))
            .unwrap_or_default(),
        Err(_) => return,
    };
    let Some(typed) = net::prompt_search(&current) else {
        return; // cancelled — keep the picker as it was
    };
    let query = typed.trim().to_string();
    if query.is_empty() {
        return; // empty submit = no-op, don't wipe the current list
    }
    run_cover_search_with(game_idx, query);
}

/// Shared Flashpoint cover search: runs the synchronous HTTPS search for
/// `query`, remembers it (for Minus/refine pre-fill), and opens or refreshes
/// the CoverPicker. Called from `input()` only — no LIBRARY lock during HTTPS.
fn run_cover_search_with(game_idx: usize, query: std::string::String) {
    let (cands, msg) = match crate::sources::flashpoint::search(&query) {
        Ok(list) if list.is_empty() => {
            (std::vec::Vec::new(), crate::loc::s().cover_none.to_string())
        }
        Ok(list) => (list, std::string::String::new()),
        Err(e) => (std::vec::Vec::new(), e),
    };
    log(&std::format!(
        "covers: search \"{}\" -> {} candidate(s){}\n",
        query,
        cands.len(),
        if msg.is_empty() { std::string::String::new() } else { std::format!(" [{}]", msg) },
    ));
    // New result set → drop any thumbnail fetch still in flight.
    crate::backend::render::thumb_cancel_all();
    if let Ok(mut s) = LIBRARY.lock() {
        s.cover_candidates = cands;
        s.cover_msg = msg;
        s.cover_query = query;
        s.screen = Screen::CoverPicker { game_idx, selection: 0 };
    }
}

/// A Flashpoint cover URL, in the source the picker is currently showing. The
/// two variants differ only by one path segment, so the swap is a substitution
/// rather than a second URL to carry around.
fn cover_url_for_source(cover_url: &str, use_shot: bool) -> std::string::String {
    if use_shot {
        cover_url.replace("/Logos/", "/Screenshots/")
    } else {
        cover_url.to_string()
    }
}

/// CoverPicker > A: download the chosen candidate's logo and cache it as the
/// game's cover. Called from `input()` only — synchronous HTTPS download
/// WITHOUT the LIBRARY lock. On success, invalidates the backend cover-texture
/// cache so the grid shows the new art, and returns to the OPTIONS modal.
fn run_cover_fetch_flow(game_idx: usize, selection: usize) {
    let picked = match LIBRARY.lock() {
        Ok(g) => {
            let cand = g
                .cover_candidates
                .get(selection)
                .map(|c| cover_url_for_source(&c.cover_url, g.cover_use_shot));
            let base = g.entries.get(game_idx).map(|e| e.basename.clone());
            match (cand, base) {
                (Some(c), Some(b)) => Some((c, b)),
                _ => None,
            }
        }
        Err(_) => return,
    };
    // Fetch what the picker was SHOWING, not the candidate's default source.
    let Some((url, basename)) = picked else { return };
    log(&std::format!("covers: fetch \"{}\" <- {}\n", basename, url));
    match crate::covers::fetch_url_and_cache(&basename, &url) {
        Ok(path) => {
            log(&std::format!("covers: cached -> {}\n", path));
            crate::backend::render::invalidate_cover(&basename);
            if let Ok(mut s) = LIBRARY.lock() {
                s.cover_candidates.clear();
                s.cover_msg.clear();
                s.cover_query.clear();
                // Back to OPTIONS on the JAQUETTE row (index 3 after #20 regroup
                // removed APPLIQUER/PARTAGER from the list).
                s.screen = Screen::OptionsModal { game_idx, selection: opt_row("JAQUETTE") };
            }
        }
        Err(e) => {
            log(&std::format!("covers: fetch FAILED: {}\n", e));
            if let Ok(mut s) = LIBRARY.lock() {
                s.cover_msg = e;
            }
        }
    }
}

/// CoverPicker navigation (Up/Down move, B cancel). A (fetch) and Minus
/// (refine search) are hoisted in `input()` (HTTPS / swkbd).
fn handle_cover_picker_input(s: &mut State, button: &str, game_idx: usize, mut selection: usize) {
    let total = s.cover_candidates.len();
    let cols = COVER_PICKER_COLS;
    match button {
        "Left" | "StickLLeft" => {
            if total > 0 && selection > 0 {
                selection -= 1;
            }
        }
        "Right" | "StickLRight" => {
            if total > 0 && selection + 1 < total {
                selection += 1;
            }
        }
        "Up" | "StickLUp" => {
            selection = grid_step(selection, false, cols, total);
        }
        "Down" | "StickLDown" => {
            selection = grid_step(selection, true, cols, total);
        }
        "Y" => {
            // Swap the whole grid between screenshots and logos. The thumbnails in
            // flight are for the other source, so drop them rather than let them
            // land under the new URLs.
            s.cover_use_shot = !s.cover_use_shot;
            crate::backend::render::thumb_cancel_all();
        }
        "B" => {
            crate::backend::render::thumb_cancel_all();
            s.cover_candidates.clear();
            s.cover_msg.clear();
            s.cover_query.clear();
            s.cover_use_shot = false;
            // JAQUETTE row (index 3 after #20 regroup).
            s.screen = Screen::OptionsModal { game_idx, selection: opt_row("JAQUETTE") };
            return;
        }
        _ => {}
    }
    s.screen = Screen::CoverPicker { game_idx, selection };
}

/// Flashpoint results (IMPORTER > X) navigation + download. A cover grid of the
/// search hits; A downloads the selected game's GameZIP (async — then
/// `on_download_finished` unzips it), or plays it when it is already on the
/// card; B folds the page back into the Flashpoint card. Called under the
/// LIBRARY lock; `net::start_download` is non-blocking so that's fine.
fn handle_fp_gallery_input(s: &mut State, button: &str, mut selection: usize, mut scroll: usize) {
    // Search still on its way: only B (or Minus) answers, cancelling it. The
    // page stays "loading" while it folds back (the fold clears it once done):
    // cleared here, the folding page read as a search that found nothing.
    if s.fp_loading {
        if matches!(button, "B" | "Minus") {
            net::cancel_archive_fetch();
            crate::backend::render::distant_reveal_begin_collapse();
        }
        return;
    }
    let total = s.cover_candidates.len();
    // A failed search or none found: A (try again) and Minus (a new search) are
    // hoisted; B folds the page back.
    if s.fp_error || total == 0 {
        if button == "B" {
            crate::backend::render::distant_reveal_begin_collapse();
        }
        return;
    }
    let cols = FP_GALLERY_COLS;
    let rows_visible = FP_GALLERY_ROWS;
    match button {
        "Left" | "StickLLeft" => {
            if selection > 0 {
                selection -= 1;
            }
        }
        "Right" | "StickLRight" => {
            if selection + 1 < total {
                selection += 1;
            }
        }
        "Up" | "StickLUp" => {
            selection = grid_step(selection, false, cols, total);
        }
        "Down" | "StickLDown" => {
            selection = grid_step(selection, true, cols, total);
        }
        "A" => {
            let Some(cand) = s.cover_candidates.get(selection).cloned() else {
                return;
            };
            let swf_name = crate::sources::gamezip::swf_filename(&cand.title);
            // Already on the card: A plays it, from here.
            if let Some(abs) = s.entries.iter().position(|e| e.basename == swf_name) {
                let rect = crate::backend::render::import_sel_rect();
                launch_from_importer(s, abs, rect, Screen::FpGallery { selection, scroll });
                return;
            }
            // No address of the player's in a Flashpoint download: one left
            // from an archive tried before must not be offered for fixing (Y)
            // on this game's error page, whichever way it fails.
            s.failed_url.clear();
            let swf_path = std::format!("{}/{}", primary_root(), swf_name);
            // Zipped games come from the GameZIP server: download a temp `.zip`,
            // then extract. Non-zipped (legacy "loose") games aren't on that
            // server, so download their entry `.swf` straight from the htdocs
            // mirror (derived from the launchCommand); companions are fetched
            // afterwards, same as a GameZIP. `fp_direct` routes the finish path.
            // Backstop for `parse_search`'s runnable filter. A bare `.swf` is
            // runnable; a zipped HTML wrapper is too (we resolve the embedded
            // entry SWF out of the GameZIP after download — e.g. Agent P Strikes
            // Back). A non-.swf, non-wrapper (or non-zipped HTML) entry has
            // nothing we can launch, so bail with a clear message BEFORE
            // transferring the GameZIP.
            let runnable = crate::sources::gamezip::launch_command_is_swf(&cand.launch_command)
                || (cand.zipped
                    && crate::sources::gamezip::launch_command_is_html(&cand.launch_command));
            if !runnable {
                import_error(s, ErrKind::Unsupported, crate::loc::s().err_fp_html_game, false, &cand.title);
                return;
            }
            let (url, out_path, fp_direct) = if cand.zipped {
                let zip_path = std::format!("{}/.fpdl.zip", primary_root());
                (crate::sources::gamezip::get_url(&cand.id), zip_path, false)
            } else {
                match crate::sources::gamezip::htdocs_url_from_command(&cand.launch_command) {
                    Some(u) => (u, swf_path.clone(), true),
                    None => {
                        // parse_search only keeps non-zipped games with a usable
                        // command, so this is unexpected — surface an error rather
                        // than a silent no-op.
                        let msg = crate::loc_ui::ui().imp_err_fp_no_swf;
                        import_error(s, ErrKind::Unsupported, msg, false, &cand.title);
                        return;
                    }
                }
            };
            s.import_retry = Some(RetryAction::FpGame { selection, scroll });
            match net::start_download(&url, &out_path) {
                Ok(()) => {
                    note_download_started();
                    s.download_file_name = swf_name;
                    s.download_out_path = out_path;
                    s.download_source_url = url.clone();
                    // Holds the FINAL .swf path for the Flashpoint finalize. For a
                    // zipped game the download (a temp .zip) extracts INTO it; for
                    // a non-zipped game it == download_out_path (downloaded direct).
                    s.download_zip_extract = Some(swf_path);
                    s.download_fp_direct = fp_direct;
                    s.download_launch_command = cand.launch_command.clone();
                    // Grab this game's cover automatically once the .swf lands.
                    s.download_cover_url = Some(cand.cover_url.clone());
                    // Keep the REAL title so we can restore it as the display
                    // name (the filename loses `:` and other chars).
                    s.download_title = Some(cand.title.clone());
                    s.download_resume_pos = Some((selection, scroll));
                    s.import_had_files = false;
                    s.import_had_data = false;
                    s.screen = Screen::DistantDownloading;
                }
                Err(e) => import_error(s, ErrKind::Download, &e, true, &cand.title),
            }
            return;
        }
        "Plus" => {
            // The details window opens at once on what the search already
            // knows; its size and blurb arrive while it is up.
            open_fp_details(s, selection, scroll);
            return;
        }
        "Y" => {
            // The sort window, on the order the results are in.
            if total > 1 {
                s.screen = Screen::RemoteSortModal {
                    selection: s.fp_sort.0 as usize,
                    kind: SORT_TARGET_FP,
                    reverse: s.fp_sort.1,
                    prev_sel: selection,
                    prev_scroll: scroll,
                };
            }
            return;
        }
        "B" => {
            // Fold the page back into the Flashpoint card; render lands on the
            // importer row the search was asked from once it has.
            crate::backend::render::distant_reveal_begin_collapse();
            return;
        }
        _ => {}
    }
    // Keep the selected row visible (edge-scroll).
    let sel_row = selection / cols;
    if sel_row < scroll {
        scroll = sel_row;
    } else if sel_row >= scroll + rows_visible {
        scroll = sel_row + 1 - rows_visible;
    }
    s.screen = Screen::FpGallery { selection, scroll };
}

/// Open a result's details window on what the search already returned (logo,
/// title, developer, publisher, date). The download size and the blurb used to
/// be fetched BEFORE it opened, two blocking HTTPS round trips that froze the
/// last frame for seconds after `+` with nothing to show the press was taken;
/// now the window is up at once and they fill in (`drive_fp_details`).
fn open_fp_details(s: &mut State, selection: usize, scroll: usize) {
    s.fp_desc.clear();
    s.fp_desc_ready = false;
    s.fp_size_pending = true;
    s.screen = Screen::FpDetails { selection, scroll, size: 0 };
}

/// Fill in the details window: its blurb and its download size, both through
/// the thumbnail pool (async, like the logos), so the window keeps answering
/// while they come. The size was a blocking HEAD, which froze the window, its
/// spinner and its buttons for as long as the server took. Called once a frame
/// while the window is up, and from the results page, which collects a request
/// still on its way after the window closed.
fn drive_fp_details() {
    // The blurb: collect the transfer in flight, or start the one needed.
    let (slot, for_sel, want) = match LIBRARY.lock() {
        Ok(s) => {
            let want = match s.screen {
                Screen::FpDetails { selection, .. } if !s.fp_desc_ready => Some(selection),
                _ => None,
            };
            (s.fp_desc_slot, s.fp_desc_for, want)
        }
        Err(_) => return,
    };
    if slot >= 0 {
        net::thumb_pump();
        if net::thumb_slot_status(slot) != 0 {
            let bytes = net::thumb_slot_take(slot);
            if let Ok(mut s) = LIBRARY.lock() {
                s.fp_desc_slot = -1;
                // A blurb for another result (the window was closed and another
                // opened while it came) is dropped; the right one starts next.
                if want == Some(for_sel) {
                    s.fp_desc = bytes.map(|b| crate::sources::gamezip::parse_description(&b)).unwrap_or_default();
                    s.fp_desc_ready = true;
                }
            }
        }
    } else if let Some(sel) = want {
        let id = LIBRARY.lock().ok().and_then(|s| s.cover_candidates.get(sel).map(|c| c.id.clone()));
        if let Some(id) = id {
            let started = net::thumb_start(&crate::sources::gamezip::description_url(&id));
            if let Ok(mut s) = LIBRARY.lock() {
                if started >= 0 {
                    s.fp_desc_slot = started;
                    s.fp_desc_for = sel;
                } else if id.is_empty() {
                    s.fp_desc_ready = true;
                }
            }
        }
    }

    // The size, the same way, by a HEAD.
    let (slot, for_sel, want) = match LIBRARY.lock() {
        Ok(s) => {
            let want = match s.screen {
                Screen::FpDetails { selection, .. } if s.fp_size_pending => Some(selection),
                _ => None,
            };
            (s.fp_size_slot, s.fp_size_for, want)
        }
        Err(_) => return,
    };
    if slot >= 0 {
        net::thumb_pump();
        if net::thumb_slot_status(slot) != 0 {
            let size = net::thumb_slot_take_length(slot);
            if let Ok(mut s) = LIBRARY.lock() {
                s.fp_size_slot = -1;
                // An answer for another result is dropped, as a blurb is.
                if want == Some(for_sel) {
                    s.fp_size_pending = false;
                    if let Screen::FpDetails { selection, scroll, .. } = s.screen {
                        s.screen = Screen::FpDetails { selection, scroll, size: size.unwrap_or(0) };
                    }
                }
            }
        }
    } else if let Some(sel) = want {
        // Probe the URL the download will ACTUALLY use, mirroring the choice the
        // `A` branch of `handle_fp_gallery_input` makes: the GameZIP server for a
        // zipped game, the htdocs mirror for a legacy "loose" one. Probing
        // `/get?id=` for a legacy entry always 404s.
        let url = LIBRARY.lock().ok().and_then(|s| {
            s.cover_candidates.get(sel).and_then(|c| {
                if c.zipped {
                    Some(crate::sources::gamezip::get_url(&c.id))
                } else {
                    crate::sources::gamezip::htdocs_url_from_command(&c.launch_command)
                }
            })
        });
        let started = url.as_deref().map(net::thumb_start_head);
        if let Ok(mut s) = LIBRARY.lock() {
            match started {
                Some(slot) if slot >= 0 => {
                    s.fp_size_slot = slot;
                    s.fp_size_for = sel;
                }
                // The pool is busy with logos: asked again next frame.
                Some(-5) => {}
                // Nowhere to ask, or no way to: the window does without the size.
                _ => s.fp_size_pending = false,
            }
        }
    }
}

/// Start game `abs` from an importer page ("A = Jouer" on a game already on the
/// card): the Play grid's launch, its cover growing out of `rect` (what was
/// pressed), with `backdrop`, the importer page, drawn behind it instead of
/// the grid. After the game the library opens on Jouer with it selected, like
/// any game played.
fn launch_from_importer(s: &mut State, abs: usize, rect: (f32, f32, f32, f32), backdrop: Screen) {
    let Some(entry) = s.entries.get(abs).cloned() else { return };
    // The game's Jouer row, with the scroll Jouer was left at. Not through
    // `list_screen_for_abs`: the cell table it reads rows from is this importer
    // page's, not the grid's (`open` places the cursor once the grid is back).
    let selection = local_filtered_indices(&s.entries, &s.local_filter, &s.folder_filter)
        .iter()
        .position(|&i| i == abs)
        .unwrap_or(0);
    let scroll_offset = jouer_scroll();
    // Applet mode has too little RAM to launch a SWF: the same notice as Jouer,
    // which then goes back to this page rather than over to Jouer.
    if s.applet_mode {
        if let Ok(mut g) = LAUNCH_BACKDROP.lock() {
            *g = Some(backdrop);
        }
        s.screen = Screen::AppletNotice { selection, scroll_offset };
        return;
    }
    s.selected_path = Some(entry.path.clone());
    note_played(&entry.path, &entry.basename, &entry.display_name);
    if let Ok(mut g) = LAUNCH_TICK.lock() {
        *g = Some(unsafe { ruffle_tick_now() });
    }
    LAUNCH_MULTI.store(entry.multi, std::sync::atomic::Ordering::Relaxed);
    log(&std::format!("library: IMPORTER -> {} ({})\n", entry.display_name, entry.path));
    crate::backend::render::game_reveal_begin(false, rect, &entry.basename, &entry.display_name, entry.color_chip);
    if let Ok(mut g) = LAUNCH_BACKDROP.lock() {
        *g = Some(backdrop);
    }
    LAUNCHED_FROM_IMPORTER.store(true, std::sync::atomic::Ordering::Relaxed);
    s.screen = Screen::Launching { selection, scroll_offset };
}

/// The game running was started from the importer: back in the library, its
/// cover has no tile to shrink into (see `open`).
static LAUNCHED_FROM_IMPORTER: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// The importer page a game was launched from, drawn behind its launch instead
/// of the Play grid (`launch_from_importer`). Cleared when the launch is done.
/// In applet mode, where no game launches, it is the page the notice saying so
/// goes back to, taken when the notice is dismissed.
static LAUNCH_BACKDROP: Mutex<Option<Screen>> = Mutex::new(None);

/// The download in flight: when it started, and its speed (bytes per second,
/// eased), sampled twice a second. Drives the "3,1 Mo/s · 2 min" of the import
/// page and whether B asks before stopping it.
struct DlRate {
    start: u64,
    tick: u64,
    bytes: u64,
    rate: f32,
}

static DL_RATE: Mutex<DlRate> = Mutex::new(DlRate { start: 0, tick: 0, bytes: 0, rate: 0.0 });

/// A download just started: its clock and speed start from zero.
fn note_download_started() {
    let now = unsafe { ruffle_tick_now() };
    if let Ok(mut g) = DL_RATE.lock() {
        *g = DlRate { start: now, tick: now, bytes: 0, rate: 0.0 };
    }
}

/// The download's speed, once there is enough of it to say (a second and a
/// half in), given the bytes received so far.
fn download_rate(done: u64) -> Option<f32> {
    let now = unsafe { ruffle_tick_now() };
    let freq = unsafe { ruffle_tick_freq() }.max(1) as f32;
    let mut g = DL_RATE.lock().ok()?;
    let dt = now.saturating_sub(g.tick) as f32 / freq;
    if dt >= 0.5 {
        let inst = done.saturating_sub(g.bytes) as f32 / dt;
        g.rate = if g.rate <= 0.0 { inst } else { g.rate * 0.7 + inst * 0.3 };
        g.tick = now;
        g.bytes = done;
    }
    let elapsed = now.saturating_sub(g.start) as f32 / freq;
    (elapsed >= 1.5 && g.rate > 0.0).then_some(g.rate)
}

/// Seconds since the download in flight started.
fn download_elapsed() -> f32 {
    let now = unsafe { ruffle_tick_now() };
    let freq = unsafe { ruffle_tick_freq() }.max(1) as f32;
    DL_RATE.lock().map(|g| now.saturating_sub(g.start) as f32 / freq).unwrap_or(0.0)
}

/// How many ways the Play tab sorts (`SortMode`, `sort_mode_index`).
const SORT_MODES: usize = 5;

/// JOUER sort picker (Y) input: Up/Down move between the ways to sort, Left /
/// Right turn the focused one to its natural or reversed order (X flips it), A
/// applies it, B cancels. After a sort change the list reorders, so the cursor
/// resets to the top.
fn handle_sort_modal_input(
    s: &mut State,
    button: &str,
    mut selection: usize,
    prev_sel: usize,
    prev_scroll: usize,
    mut dirs: u8,
) {
    let bit = 1u8 << selection.min(7);
    match button {
        "Up" | "StickLUp" => selection = list_step_wrap(selection, -1, SORT_MODES),
        "Down" | "StickLDown" => selection = list_step_wrap(selection, 1, SORT_MODES),
        // The row's two orders are a choice shown whole: Left lights the
        // first, Right the second, and they stop there like every choice.
        "Left" | "StickLLeft" => dirs &= !bit,
        "Right" | "StickLRight" => dirs |= bit,
        // Nothing applies before A, the same as the importer's pickers: X used
        // to re-sort the library on the spot, so B ("cancel") could no longer
        // cancel anything.
        "X" => dirs ^= bit,
        "A" => {
            store_active_sort(selection as u8, dirs & bit != 0);
            sort_entries(&mut s.entries, current_sort_mode(), current_sort_reverse());
            // A new order is a new list: every view snaps to the top and its
            // covers start closed, rather than gliding from the old row.
            crate::backend::render::gallery_anim_reset();
            crate::backend::render::tile_open_reset();
            s.screen = Screen::List { selection: 0, scroll_offset: 0 };
            return;
        }
        "B" => {
            // Cancel → restore the cursor where it was.
            s.screen = Screen::List { selection: prev_sel, scroll_offset: prev_scroll };
            return;
        }
        _ => {}
    }
    s.screen = Screen::SortModal { selection, prev_sel, prev_scroll, dirs };
}

/// Which list a `RemoteSortModal` sorts.
pub(crate) const SORT_TARGET_FILES: u8 = 0; // archive.org file list
pub(crate) const SORT_TARGET_FP: u8 = 1; // Flashpoint result gallery
pub(crate) const SORT_TARGET_IMPORT: u8 = 2; // IMPORTER home (saved URLs)

/// The criteria a `RemoteSortModal` offers for `kind`, in order: icon, name,
/// and the two orders it gives, its natural one first (the newest, A to Z, the
/// largest...) then reversed, as the page headers name the order in force.
/// `reverse` everywhere means "against the natural order", so a header never
/// says "Croissant" over a list running biggest first.
fn remote_sort_options(kind: u8) -> std::vec::Vec<(crate::backend::render::Icon, &'static str, [&'static str; 2])> {
    use crate::backend::render::Icon;
    let lc = crate::loc::s();
    let u = crate::loc_ui::ui();
    let name = (Icon::SortAlpha, lc.sort_alpha, [u.imp_name_az, u.imp_name_za]);
    match kind {
        SORT_TARGET_FP => std::vec![
            name,
            (Icon::User, lc.fp_details_dev, [u.imp_dev_az, u.imp_dev_za]),
            (Icon::Calendar, u.imp_sort_release, [u.imp_rel_new, u.imp_rel_old]),
        ],
        SORT_TARGET_IMPORT => std::vec![
            (Icon::Calendar, u.imp_sort_added, [u.imp_src_newest, u.imp_src_oldest]),
            name,
            (Icon::Link, lc.sort_source, [u.imp_site_az, u.imp_site_za]),
            (Icon::Archive, lc.sort_files, [u.imp_src_most, u.imp_src_fewest]),
        ],
        _ => std::vec![name, (Icon::HardDrive, lc.sort_size, [u.imp_size_big, u.imp_size_small])],
    }
}

/// The order `crit` / `rev` of `kind` gives, as its outcome ("Les plus lourds").
fn remote_sort_phrase(kind: u8, crit: u8, rev: bool) -> &'static str {
    remote_sort_options(kind).get(crit as usize).map(|o| o.2[rev as usize]).unwrap_or("")
}

/// Number of criteria in the IMPORTER sort picker, per target.
fn remote_sort_count(kind: u8) -> usize {
    remote_sort_options(kind).len()
}

/// IMPORTER sort picker (Y). `kind` selects the target list + option set (see
/// `SORT_TARGET_*`). Left / Right (or X) pick the order, A applies + returns to
/// the list (cursor top), B restores. The file and Flashpoint lists keep their
/// order in `files_sort` / `fp_sort` until they are fetched again, so the window
/// reopens on it; the saved-URL sort is a persisted PREFERENCE (`url_history`
/// keeps its insertion order — that's the ADDED criterion).
fn handle_remote_sort_modal_input(
    s: &mut State,
    button: &str,
    mut selection: usize,
    kind: u8,
    mut reverse: bool,
    prev_sel: usize,
    prev_scroll: usize,
) {
    let n = remote_sort_count(kind);
    match button {
        // A new press comes round, a held one stops at the end.
        "Up" | "StickLUp" => selection = list_step_wrap(selection, -1, n),
        "Down" | "StickLDown" => selection = list_step_wrap(selection, 1, n),
        // The natural order on the left, its reverse on the right; X flips.
        "Left" | "StickLLeft" => reverse = false,
        "Right" | "StickLRight" => reverse = true,
        "X" => reverse = !reverse,
        "A" => {
            match kind {
                SORT_TARGET_FP => {
                    s.fp_sort = (selection as u8, reverse);
                    let order = s.fp_sort;
                    sort_fp_candidates(&mut s.cover_candidates, order);
                    // Re-ordered list: snap the glide to the top (no streak).
                    crate::backend::render::gallery_anim_reset();
                    s.screen = Screen::FpGallery { selection: 0, scroll: 0 };
                }
                SORT_TARGET_IMPORT => {
                    // Persist the choice: `importer_view` reads it every frame,
                    // so the list is already re-ordered when we land back on it.
                    set_import_sort(selection as u8, reverse);
                    crate::backend::render::gallery_anim_reset();
                    // Cursor on the first source, under the two cards.
                    let row = if importer_view(s).is_empty() { 0 } else { IMPORTER_PINNED_ROWS };
                    IMPORT_SCROLL.store(0, std::sync::atomic::Ordering::Relaxed);
                    s.screen = distant_idle_at(s, row);
                }
                _ => {
                    s.files_sort = (selection as u8, reverse);
                    let order = s.files_sort;
                    sort_remote_files(&mut s.remote_files, order);
                    // Re-ordered list: snap the glide to the top.
                    crate::backend::render::gallery_anim_reset();
                    s.screen = Screen::DistantFiles { selection: 0, scroll_offset: 0 };
                }
            }
            return;
        }
        "B" => {
            // Cancel → restore the list cursor where it was.
            s.screen = match kind {
                SORT_TARGET_FP => Screen::FpGallery { selection: prev_sel, scroll: prev_scroll },
                SORT_TARGET_IMPORT => Screen::DistantIdle {
                    selection: prev_sel,
                    scroll_offset: prev_scroll,
                },
                _ => Screen::DistantFiles { selection: prev_sel, scroll_offset: prev_scroll },
            };
            return;
        }
        _ => {}
    }
    s.screen = Screen::RemoteSortModal { selection, kind, reverse, prev_sel, prev_scroll };
}

/// RENOMMER flow: open swkbd with the current display_name pre-filled,
/// write the .meta.json sidecar with the result, update the in-memory
/// entry. Empty input removes the sidecar (revert to basename). Called
/// from `input()` only — must NOT hold the LIBRARY lock during swkbd.
fn run_rename_flow(game_idx: usize) {
    let (basename, current_display) = match LIBRARY.lock() {
        Ok(g) => match g.entries.get(game_idx) {
            Some(e) => (e.basename.clone(), e.display_name.clone()),
            None => return,
        },
        Err(_) => return,
    };
    let Some(new_name) = net::prompt_rename(&current_display) else {
        return; // cancelled
    };
    let new_name_trimmed = new_name.trim().to_string();
    let persisted = write_meta_sidecar(&basename, &new_name_trimmed);
    if !persisted {
        log("library: write_meta_sidecar failed - rename not applied\n");
    }
    if let Ok(mut s) = LIBRARY.lock() {
        if !persisted {
            // The tile changing IS the only success signal this flow has, so
            // showing the new name after a failed write says the rename worked;
            // it comes back under the old name at the next scan, with no clue why.
            set_toast(&mut s, crate::loc::s().err_sd_write.to_string(), TOAST_ERR);
            return;
        }
        if let Some(entry) = s.entries.get_mut(game_idx) {
            entry.display_name = if new_name_trimmed.is_empty() {
                pretty_name(&entry.basename)
            } else {
                new_name_trimmed
            };
        }
    }
}

/// Handles the destructive SUPPRIMER confirmation screen. A = run the
/// delete + back to List (with selection clamped to the now-shorter list).
/// B / Minus = cancel + back to OptionsModal.
fn handle_delete_confirm_input(s: &mut State, button: &str, game_idx: usize, selection: usize) {
    match button {
        "Left" | "StickLLeft" | "Right" | "StickLRight" | "Up" | "StickLUp" | "Down" | "StickLDown" => {
            s.screen = Screen::DeleteConfirm { game_idx, selection: 1 - selection.min(1) };
        }
        "A" if selection == 1 => {
            // Defer the delete until the close-out finishes: stash the action,
            // start the scale-out, and leave the screen on DeleteConfirm so its
            // panel stays populated while it shrinks. render() runs the delete
            // + lands on List (or Empty) once the pop is done.
            if let Ok(mut p) = PENDING_AFTER_CLOSE.lock() {
                *p = Some(PendingClose::DeleteGame { game_idx });
            }
            crate::backend::render::modal_close_begin();
        }
        "A" | "B" | "Minus" => {
            // Cancel back to OPTIONS, on the row that opened this (the last).
            s.screen = Screen::OptionsModal { game_idx, selection: opt_row("SUPPRIMER") };
        }
        _ => {}
    }
}

/// Wipe the `.swf` + all matching sidecars / saves for a game, then
/// remove the in-memory entry. Idempotent re-runs are harmless (delete
/// of missing files is silently ignored by C++). The actual unlink work
/// is done in C++ via `swf_picker_delete_game` (uses opendir/readdir
/// which sidesteps the Horizon `read_dir` truncation bug). We only need
/// to pass the .swf path — C++ derives the basename and scans the parent
/// dir for `<basename>.*` matches.
/// Delete the old-shape save files this game was RECORDED reading, listed one per
/// line in `<basename>.solmap` by the storage backend.
///
/// Only files the game demonstrably opened are touched; nothing is inferred from
/// a name. In the collision case two games can both have recorded the same file —
/// deleting one then removes it, and the other keeps playing from the copy it
/// wrote under its own key the first time it saved.
fn delete_recorded_legacy_saves(basename: &str) {
    let map = std::format!("{}/{}.solmap", primary_root(), basename);
    let Some(bytes) = crate::sources::gamezip::read_file_bounded(&map, 64 * 1024) else {
        return;
    };
    let Ok(text) = std::string::String::from_utf8(bytes) else {
        return;
    };
    for line in text.lines() {
        let name = line.trim();
        // A path separator here would mean the record was tampered with; the
        // storage backend only ever writes a bare file name.
        if name.is_empty() || name.contains('/') || name.contains('\\') {
            continue;
        }
        let path = std::format!("{}/{}", primary_root(), name);
        match std::fs::remove_file(&path) {
            Ok(()) => log(&std::format!("library: removed legacy save {}\n", path)),
            Err(e) => log(&std::format!("library: legacy save {} not removed: {}\n", path, e)),
        }
    }
}

fn delete_game(s: &mut State, game_idx: usize) {
    let Some(entry) = s.entries.get(game_idx).cloned() else {
        return;
    };
    // Does another game share this file name? Asked FIRST, because everything
    // below that touches a name-keyed file has to know.
    let twins = twin_basename_exists(&s.entries, &entry.basename, &entry.path);
    // Saves this game was seen reading under the OLD movie-keyed name. The C++
    // sweep matches `<basename>*` and can never reach them — that is why deleting
    // Agent P Strikes Back left its save behind, ready to be picked up again on
    // reinstall. Removed here, BEFORE the sweep, so the sweep still takes the
    // `.solmap` itself (it matches the prefix).
    //
    // Not when a namesake exists. The `.solmap` this reads is `<basename>.solmap`
    // at the root: two games with one file name write into the SAME file and
    // accumulate both their lists in it, so deleting either would unlink the
    // other's legacy saves.
    if !twins {
        delete_recorded_legacy_saves(&entry.basename);
    }
    let mut path_c = entry.path.as_bytes().to_vec();
    path_c.push(0);
    // Second directory to sweep: the root of the games folder. A game inside a
    // folder (issue #68) keeps every companion but its `.files/` tree there, and
    // the sweep beside the .swf would never see them.
    //
    // UNLESS ANOTHER GAME SHARES ITS FILE NAME. Every one of those companions is
    // keyed by that name alone, so the two games genuinely share them: deleting
    // one would take the other's saves, controls, title and cover with it. When
    // that happens the companions are left behind — orphaned files cost disk,
    // and the alternative costs someone else's save.
    let mut root_c = if twins {
        log(&std::format!(
            "library: {} has a namesake elsewhere; its shared sidecars are kept\n",
            entry.basename,
        ));
        std::vec::Vec::new()
    } else {
        primary_root().into_bytes()
    };
    root_c.push(0);
    let rc = unsafe {
        swf_picker_delete_game(
            path_c.as_ptr() as *const core::ffi::c_char,
            root_c.as_ptr() as *const core::ffi::c_char,
        )
    };
    // And its HOME-menu tile, if it has one: a tile left behind points at a
    // `.swf` that no longer exists, and the player only finds out by pressing it
    // and landing back in the library.
    //
    // The ID comes from the path, so this can only ever name the tile this game
    // installed. It is checked before it is removed, which keeps a game that
    // never had a tile from asking the console to uninstall anything.
    if tile_remove(&entry.path) {
        log(&std::format!("library: removed tile for {}\n", entry.display_name));
    }
    // The C++ scan only sees the .swf's own directory; clean up the cached
    // cover (covers/ subdir) and stem-named sidecars it can't reach, then
    // commit so the unlinks (C++ + Rust) actually persist to the SD card.
    //
    // Skipped for a shared file name, exactly like the sweep above. The cover is
    // `covers/<basename>.cover.png` and the manual overrides are `<stem>.png`
    // across every root: not one component of either is the path, so both belong
    // to the namesake as much as to this game. The sweep had been taught this
    // and these three lines had not, so a delete still took the survivor's cover
    // and its star while the log said the shared sidecars were kept.
    let extra = if twins {
        0
    } else {
        let n = crate::covers::remove_for(&entry.basename);
        // Drop the renderer's cached cover TEXTURE (keyed by basename) too:
        // deleting the file alone left a re-import of the same name showing the
        // old cover straight from GPU memory. Mirrors run_cover_fetch_flow's.
        crate::backend::render::invalidate_cover(&entry.basename);
        n
    };
    crate::sd::commit();
    log(&std::format!(
        "library: SUPPRIMER {} -> rc={} (+{} cover/sidecar)\n",
        entry.path, rc, extra,
    ));
    s.entries.remove(game_idx);
    // Drop it from favorites too, so a re-import of the same name doesn't come
    // back pre-starred. Not when a namesake survives: `favorites.json` is
    // indexed by file name, so its star is the same star.
    if !twins {
        crate::favorites::remove(&entry.basename);
        // Its shelf labels go the same way, and for the same reason:
        // `tags.json` is keyed by file name too, so a namesake is standing on
        // those shelves as much as this game was. Without this, reinstalling a
        // game under a name used before puts it straight back on shelves
        // nobody chose for it.
        crate::tags::remove(&entry.basename);
    }
    // Clear the session "downloaded" mark too. The IMPORTER green OK badge is a
    // union of `entries` (updated just above) and this set; a same-session
    // download lingering here kept the badge lit after a delete, while the
    // A-press "already on SD" check (entries-only) disagreed and re-downloaded.
    s.downloaded_basenames.retain(|n| n != &entry.basename);
}

extern "C" {
    fn swf_picker_delete_game(
        swf_path: *const core::ffi::c_char,
        also_dir: *const core::ffi::c_char,
    ) -> core::ffi::c_int;
    fn swf_picker_count_companions(swf_path: *const core::ffi::c_char) -> core::ffi::c_int;
    // Cursor-speed preset (main.cpp): cycle to the next preset (returns the new
    // index), and read the current multiplier x10 (5,10,15,20,25) for the label.
    pub(crate) fn ruffle_cursor_speed_step(dir: core::ffi::c_int) -> core::ffi::c_int;
    pub(crate) fn ruffle_cursor_speed_mult_x10() -> core::ffi::c_int;
}

/// Search flow: open swkbd pre-filled with the current filter, submit
/// becomes the new filter. Empty input clears the filter. Selection +
/// scroll reset to 0 so the user starts at the top of the filtered view.
fn run_distant_search_flow() {
    let current = LIBRARY
        .lock()
        .ok()
        .and_then(|s| s.distant_filter.clone())
        .unwrap_or_default();
    let u = crate::loc_ui::ui();
    let Some(typed) = net::prompt_search_as(u.imp_kbd_files_header, u.imp_kbd_files_guide, &current) else {
        return; // cancelled
    };
    let trimmed = typed.trim().to_string();
    if let Ok(mut s) = LIBRARY.lock() {
        s.distant_filter = if trimmed.is_empty() { None } else { Some(trimmed) };
        // Fresh (filtered) list: snap the glide to the top.
        crate::backend::render::gallery_anim_reset();
        s.screen = Screen::DistantFiles { selection: 0, scroll_offset: 0 };
    }
}

/// LOCAL list search (Minus) — same behaviour as the DISTANT filter: opens swkbd
/// (pre-filled with the current filter so it can be refined), then narrows the
/// list to entries whose display name or basename contains the text. Empty
/// input clears the filter. Selection resets to the top of the filtered view.
/// Called from `input()` only (swkbd must not run while we hold the lock).
fn run_local_search_flow() {
    let current = LIBRARY
        .lock()
        .ok()
        .and_then(|s| s.local_filter.clone())
        .unwrap_or_default();
    let typed = net::prompt_search(&current);
    // The keyboard had the screen and the finger, and the home under it never
    // changed: a chip held while - was pressed would otherwise be tapped by the
    // first finger-up sample once the keyboard is gone.
    touch_press_cancel();
    let Some(typed) = typed else {
        return; // cancelled
    };
    let trimmed = typed.trim().to_string();
    if let Ok(mut s) = LIBRARY.lock() {
        s.local_filter = if trimmed.is_empty() { None } else { Some(trimmed) };
        s.screen = Screen::List { selection: 0, scroll_offset: 0 };
    }
    // Filtered view = brand-new list; snap every view's glide to the top and
    // start its covers closed.
    crate::backend::render::gallery_anim_reset();
    crate::backend::render::tile_open_reset();
}

fn clamp_scroll(mut scroll: usize, selection: usize, visible_rows: usize) -> usize {
    if selection < scroll {
        scroll = selection;
    } else if selection >= scroll + visible_rows {
        scroll = selection + 1 - visible_rows;
    }
    scroll
}

/// Move selection to the spatially-nearest tile in the row `dir` (-1 up /
/// +1 down) of the JOUER gallery, using the layout the renderer published via
/// `gallery_layout_read`. `None` if there's no such row (or no layout yet).
fn gallery_neighbor(selection: usize, dir: i32) -> Option<usize> {
    let (cells, rows) = crate::backend::render::gallery_layout_read();
    if selection >= cells.len() || rows == 0 {
        return None;
    }
    let cur = cells[selection];
    let target_row = cur.row as i32 + dir;
    if target_row < 0 || target_row as u32 >= rows {
        return None;
    }
    let target_row = target_row as u32;
    let mut best: Option<(usize, f32)> = None;
    for (i, c) in cells.iter().enumerate() {
        if c.row == target_row {
            let d = (c.cx - cur.cx).abs();
            if best.map_or(true, |(_, bd)| d < bd) {
                best = Some((i, d));
            }
        }
    }
    best.map(|(i, _)| i)
}

/// First-visible-row scroll so `selection`'s row stays on screen in the JOUER
/// gallery. Reads the published layout for the selection's row.
fn gallery_scroll_for(selection: usize, scroll: usize) -> usize {
    let (cells, _rows) = crate::backend::render::gallery_layout_read();
    if selection >= cells.len() {
        return scroll;
    }
    let visible = home_rows_visible();
    let sel_row = cells[selection].row as usize;
    if sel_row < scroll {
        sel_row
    } else if sel_row >= scroll + visible {
        sel_row + 1 - visible
    } else {
        scroll
    }
}

/// Screen rect of importer item `sel` (a card, or a source row as its focus
/// pill sits), from the SAME geometry the renderer lays them out with
/// (`importer_item_rect`). The expand/collapse reveal's grow-from / shrink-to
/// box; a row scrolled out of view is held at the edge of the band, so the
/// window still grows from somewhere on screen.
fn distant_row_rect(sel: usize, scroll: usize, vw: f32) -> (f32, f32, f32, f32) {
    let (x, y, w, h) = importer_item_rect(sel, scroll as f32 * IMPORTER_ROW_H, vw);
    if sel < IMPORTER_PINNED_ROWS {
        return (x, y, w, h);
    }
    let y = y.clamp(IMPORTER_ROW_TOP, IMPORTER_ROW_TOP + (IMPORTER_VISIBLE_ROWS - 1) as f32 * IMPORTER_ROW_H);
    (x, y + 3.0, w, h - 6.0)
}

/// Id of the list on screen, for the touch tables. Never zero for a screen that
/// has tappable rows.
///
/// Separate from `modal_kind` on purpose: that one decides which screens get the
/// modal scale-in, and full pages like REGLAGES and the bug-report list are
/// deliberately not modals. Reusing it made both of them untouchable, because a
/// table tagged 0 is refused by the hit test — the guard that stops a stale
/// table from being hit.
fn touch_kind(screen: Screen) -> u32 {
    match screen {
        // Full pages, ids of their own well clear of the modal ones: the
        // RÉGLAGES tab and the two pages that open in its pane.
        Screen::Settings { .. } => 100,
        Screen::BugPicker { .. } => 101,
        Screen::GamesDirPicker { .. } => 102,
        // The keymap editor carries its sub-screen in the 70/80 bands: the pad
        // and its key picker share one window (one modal kind) but publish
        // different tables.
        Screen::TouchesEditor { .. } => 70 + menu::screen_kind() as u32,
        Screen::SettingsKeymapEditor => 80 + menu::screen_kind() as u32,
        other => modal_kind(other) as u32,
    }
}

/// Stable id per panel/modal screen (0 = not a modal). Used by `render` to fire
/// the scale-in "pop" exactly once when a modal is first shown. Full pages
/// (DistantError, the download) are NOT modals here: no panel to scale. The
/// TOUCHES editors ARE modals (scale in like the rest).
fn modal_kind(screen: Screen) -> u8 {
    // A game's options and everything opened from them are ONE panel stack
    // (`options_panel`): the panel slides in from the right once, its pages
    // come out beside it, and none of them pops on its own.
    if options_panel::is_options_family(screen) {
        return 1;
    }
    match screen {
        Screen::OptionsModal { .. } => 1,
        Screen::DeleteConfirm { .. } => 2,
        Screen::CoverPicker { .. } => 3,
        // A source's options and the question deleting it asks are one panel
        // stack, like a game's options: the question comes up over the panel
        // without the panel popping again, and Cancel drops back onto it.
        Screen::DistantUrlOptions { .. } | Screen::DistantHistoryConfirm { .. } => 5,
        Screen::ImportNotAddress { .. } => 24,
        // Its own id: it shared 23 with SendFailed, and a table one of them
        // left behind answered the other's first frame.
        Screen::DistantCancelConfirm { .. } => 25,
        // The applet-mode notice, a window over the page the game was pressed
        // on: it pops in and scales out like the others.
        Screen::AppletNotice { .. } => 26,
        // The keymap editor delegates to `menu::*`; its window pops in once, and
        // its key picker opens inside that window (`menu::window_kind` counts
        // the two as one), exactly like the in-game `ruffle_touches_draw` path.
        // Distinct 70-79 / 80-89 bands so they never collide with the ids above.
        Screen::TouchesEditor { .. } => 70 + menu::window_kind(),
        Screen::SettingsKeymapEditor => 80 + menu::window_kind(),
        Screen::SortModal { .. } => 9,
        Screen::RemoteSortModal { .. } => 7,
        Screen::FpDetails { .. } => 11,
        // #20 profile modals: distinct ids so each gets the scale-in "pop" when it
        // appears (they were kind 0 = no animation, unlike every other modal).
        Screen::TouchesMenu { .. } => 12,
        Screen::ProfileList { .. } => 13,
        Screen::ProfilePreview { .. } => 14,
        Screen::RevertPreview { .. } => 15,
        Screen::ProfileShareConfirm { .. } => 16,
        Screen::ProfileDeleteConfirm { .. } => 17,
        // The question about moving the library (#79). Its browser is no
        // window: it opens inside the RÉGLAGES pane, a page and not a pop.
        Screen::GamesDirConfirm { .. } => 20,
        // A report the relay did not take, asked over the page.
        Screen::SendFailed { .. } => 23,
        Screen::GameFolderPicker { .. } => 21,
        Screen::GameFolderDelete { .. } => 22,
        _ => 0,
    }
}

/// Last modal kind seen by `render` (0 = none), so we trigger the open pop only
/// on the frame a modal first appears, not every frame it stays up.
static LAST_MODAL_KIND: Mutex<u8> = Mutex::new(0);

/// What `render` does once a deferred close-out finishes. Most closes just swap
/// to another screen; the destructive confirms defer their MUTATION here too, so
/// the panel stays populated while it scales out and the delete + swap land
/// together at the very end. Set by `input`, consumed by `render`.
enum PendingClose {
    Goto(Screen),
    /// Like `Goto`, but also closes the `menu` module first — used by the
    /// Settings keymap editor, whose panel is `menu`-drawn: it must stay active
    /// (drawable) while it scales OUT, then close as the pop lands on the tab.
    CloseMenuGoto(Screen),
    DeleteGame { game_idx: usize },
    /// Delete the saved source `history_idx`; the cursor stays on `row`, the
    /// view row it was on, so the source below slides under it.
    DeleteHistory { row: usize },
    /// Stop the download in flight (its question said yes).
    StopDownload,
}

static PENDING_AFTER_CLOSE: Mutex<Option<PendingClose>> = Mutex::new(None);

/// A profile network/IO flow deferred by one frame (#20) so a "loading" panel
/// shows BEFORE the blocking GitHub call freezes the UI thread. `input` stashes
/// the action (via `defer_net`) + flips to `Screen::ProfileLoading`; `render`
/// draws the panel that frame, then runs the flow the next, which sets the real
/// result screen. Lower-tech than the archive.org async fetch, but enough: the
/// catalog index/counts cache after the first open, so only the first click is
/// slow. `bool` = armed (false the frame it's set, true the frame it runs).
enum PendingNet {
    OpenProfiles { game_idx: usize },
    OpenShareConfirm { game_idx: usize },
    ShareProfile { game_idx: usize },
    ApplyProfile { game_idx: usize, profile_idx: usize },
    DeleteProfile { game_idx: usize, profile_idx: usize },
    /// One-time first-boot housekeeping (dedup + size backfill), run behind the
    /// "Optimisation" loading panel instead of freezing on a black screen.
    RunStartupMigrations,
}
static PENDING_NET: Mutex<Option<(PendingNet, bool)>> = Mutex::new(None);
static NET_LOADING_LABEL: Mutex<String> = Mutex::new(String::new());
/// True while the first-boot "Optimisation" modal should overlay the gallery:
/// set when the housekeeping is queued, cleared once it finishes. The screen
/// itself stays on JOUER, so the modal composites over the (dimmed) gallery.
static OPTIMIZING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// First-open warm-up for the language picker: 0 = cold, 1 = panel shown (font not
/// loaded yet), 2 = loaded. The picker lists every language's NATIVE name incl.
/// CJK ("中文"), which lazily pulls in the shared Switch font the first time it's
/// drawn — slow. We draw a loading panel one frame, then the list (loading the
/// font), then it's cached. RESET on each library renderer (re)creation: launching
/// a game drops the renderer + its GL glyph atlas, so after a game the font needs
/// re-uploading and the panel must show again (`note_renderer_reset`).
static LANG_FONT_WARM: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

/// Called when the standalone library renderer is (re)created (boot + after every
/// game quit). The new renderer has an empty glyph atlas, so the CJK font is no
/// longer warm — reset so the next language-picker open shows the loading panel
/// again instead of freezing on the re-upload.
pub fn note_renderer_reset() {
    LANG_FONT_WARM.store(0, std::sync::atomic::Ordering::Relaxed);
}

/// Stash `action` to run next frame + show the loading panel now (titled with the
/// game name). Call from `input` instead of running the flow inline.
fn defer_net(action: PendingNet, game_idx: usize) {
    // What the wait is for, under its spinner (the panels behind already say
    // which game).
    let u = crate::loc_ui::ui();
    let label = match &action {
        PendingNet::OpenProfiles { .. } => u.loading_profiles,
        PendingNet::OpenShareConfirm { .. } => u.loading_share_check,
        PendingNet::ShareProfile { .. } => u.loading_share,
        PendingNet::ApplyProfile { .. } => u.loading_apply,
        PendingNet::DeleteProfile { .. } => u.loading_delete,
        PendingNet::RunStartupMigrations => crate::loc::s().optimizing,
    }
    .to_string();
    let _ = game_idx;
    if let Ok(mut l) = NET_LOADING_LABEL.lock() {
        *l = label;
    }
    if let Ok(mut p) = PENDING_NET.lock() {
        *p = Some((action, false));
    }
    if let Ok(mut s) = LIBRARY.lock() {
        s.screen = Screen::ProfileLoading;
    }
}

/// Run a deferred profile flow once its loading panel has been shown a frame.
/// First call after `defer_net` just arms it (the panel draws this frame); the
/// next call runs the blocking flow, which transitions to the result screen.
/// Called at the top of `render`, before the screen is drawn.
fn drive_pending_net() {
    // First frame after boot may queue the one-time housekeeping behind the panel.
    maybe_arm_startup_migrations();
    let action = {
        let mut g = match PENDING_NET.lock() {
            Ok(g) => g,
            Err(_) => return,
        };
        match g.take() {
            None => return,
            Some((a, false)) => {
                *g = Some((a, true)); // armed; panel shows this frame, run next
                return;
            }
            Some((a, true)) => a, // run now (last frame's panel is on the display)
        }
    };
    // Another deferred flow can only be in the slot if it replaced our queued
    // migration (both write PENDING_NET). Drop the overlay in that case, so the
    // modal can't linger over the UI for a migration that will never run; the
    // markers stay unwritten, so the next boot simply retries.
    if !matches!(action, PendingNet::RunStartupMigrations) {
        OPTIMIZING.store(false, std::sync::atomic::Ordering::Relaxed);
    }
    match action {
        PendingNet::RunStartupMigrations => {
            // Order matters: dedup first (so the freed entry copy isn't counted),
            // then the footprint backfill. Both self-guard on their own markers,
            // so this is a fast no-op once either has already run. The modal armed
            // last frame is on the display during this blocking work; drop it once
            // done so this frame redraws the plain gallery.
            cleanup_duplicate_entries();
            backfill_game_sizes();
            OPTIMIZING.store(false, std::sync::atomic::Ordering::Relaxed);
        }
        PendingNet::OpenProfiles { game_idx } => run_open_profiles_flow(game_idx),
        PendingNet::OpenShareConfirm { game_idx } => run_open_share_confirm_flow(game_idx),
        PendingNet::ShareProfile { game_idx } => run_share_profile_flow(game_idx),
        PendingNet::ApplyProfile { game_idx, profile_idx } => {
            run_profile_apply_flow(game_idx, profile_idx)
        }
        PendingNet::DeleteProfile { game_idx, profile_idx } => {
            run_delete_profile_flow(game_idx, profile_idx)
        }
    }
}

/// On the first render frame of a boot, if the one-time housekeeping (entry
/// dedup + size backfill) hasn't run yet, raise the `OPTIMIZING` overlay flag and
/// queue `RunStartupMigrations`. The screen stays on JOUER, so a bordered
/// "Optimisation" modal composites over the dimmed gallery. The arm-then-run
/// cadence of `drive_pending_net` shows that modal for a frame before the
/// blocking work runs, so it isn't a black screen. Considered once per boot; a
/// no-op when both markers are already present.
fn maybe_arm_startup_migrations() {
    use std::sync::atomic::{AtomicBool, Ordering};
    static CHECKED: AtomicBool = AtomicBool::new(false);
    if CHECKED.swap(true, Ordering::Relaxed) {
        return;
    }
    let has = |name: &str| {
        crate::sources::gamezip::read_file_bounded(&primary_user_path(name), 4).is_some()
    };
    if has(".entry_dedup2.done") && has(".filesize_backfill1.done") {
        return; // nothing to migrate — skip the panel entirely
    }
    // An empty library has nothing to migrate: skip the modal WITHOUT stamping the
    // markers. Stamping them here would retire the migration for good, so a library
    // restored AFTERWARDS (SD backup, FTP copy, card swap — how users actually move
    // their games) would keep its duplicate entry SWFs and wrong footer sizes with
    // no way to re-trigger it. Re-checked on a later boot instead; the cost when
    // there is genuinely nothing to do is two 4-byte reads.
    if LIBRARY.lock().map(|s| s.entries.is_empty()).unwrap_or(true) {
        return;
    }
    // Overlay the "Optimisation" modal on the gallery and queue the (blocking)
    // housekeeping. The screen stays on JOUER, so the modal composites over the
    // dimmed gallery. `drive_pending_net`'s arm-then-run cadence gives it a frame
    // on screen before the work runs.
    OPTIMIZING.store(true, std::sync::atomic::Ordering::Relaxed);
    if let Ok(mut p) = PENDING_NET.lock() {
        *p = Some((PendingNet::RunStartupMigrations, false));
    }
}

/// Modal kinds whose close the generic interception defers for a scale-out
/// (plain modal -> non-modal). The destructive confirms (DeleteConfirm=2,
/// DistantHistoryConfirm=6) are handled EXPLICITLY in their own handlers instead
/// (they defer the mutation too — see `PendingClose`), so they're not listed here.


/// The window a reveal opens through, `frac` of the way from `rect` (0) to
/// the whole screen (1).
fn reveal_window(rect: (f32, f32, f32, f32), vw: f32, vh: f32, frac: f32) -> (f32, f32, f32, f32) {
    let (rx, ry, rw, rh) = rect;
    (rx * (1.0 - frac), ry * (1.0 - frac), rw + (vw - rw) * frac, rh + (vh - rh) * frac)
}

/// The importer home as a backdrop (under a window, a reveal, a launch), with
/// its tabs, as it looks when it is the page: none of it takes input.
fn draw_home_backdrop(backend: &mut SwitchRenderBackend, selection: usize, scroll: usize) {
    draw_importer_home(backend, selection, scroll, false);
    let logo = LIBRARY.lock().ok().map(|s| (s.banner_tex, s.banner_w, s.banner_h)).unwrap_or_default();
    // Always under something (a window, a page opening out of a row, a
    // launch): L and R do not switch tabs from there, so no L / R chips.
    backend.draw_navbar_with(Tab::Importer.index(), logo, false);
    crate::backend::render::navbar_clear();
}

/// The importer page `screen` as a backdrop, for a launch started from it.
fn draw_importer_backdrop(backend: &mut SwitchRenderBackend, screen: Screen) {
    match screen {
        Screen::DistantIdle { selection, scroll_offset } => draw_home_backdrop(backend, selection, scroll_offset),
        Screen::DistantFiles { selection, scroll_offset } => {
            draw_files_page(backend, selection, scroll_offset, None, false)
        }
        Screen::FpGallery { selection, scroll } | Screen::FpDetails { selection, scroll, .. } => {
            draw_fp_page(backend, selection, scroll, false)
        }
        _ => {}
    }
}

/// An archive's games (see `FilesPage`), from the state. `outer_clip` is the
/// reveal window while the list opens out of its row.
fn draw_files_page(
    backend: &mut SwitchRenderBackend,
    selection: usize,
    scroll: usize,
    outer_clip: Option<(f32, f32, f32, f32)>,
    interactive: bool,
) {
    use crate::backend::render::FilesPage;
    // A game is on the card when one of that NAME is, which is the question the
    // A press asks before playing it instead of downloading it again.
    let snap = LIBRARY.lock().ok().map(|s| {
        let source = importer_label(&s.pending_fetch_url);
        // The names on the card, sorted once a frame. Asking every file of a
        // 3 600-game archive to walk the whole library, twice (its tick, then
        // the count), was millions of string compares a frame, also paid with
        // the page behind a window or a launch.
        let mut names: std::vec::Vec<&str> = s
            .entries
            .iter()
            .map(|e| e.basename.as_str())
            .chain(s.downloaded_basenames.iter().map(|n| n.as_str()))
            .collect();
        names.sort_unstable();
        names.dedup();
        let has = |name: &str| {
            // `safe_file_name` only changes a name with a folder in it, so the
            // rest are looked up as they are, without a copy.
            if name.contains(['/', '\\']) {
                names.binary_search(&safe_file_name(name).as_str()).is_ok()
            } else {
                names.binary_search(&name).is_ok()
            }
        };
        let idx = filtered_indices(&s.remote_files, &s.distant_filter);
        let files: std::vec::Vec<RemoteFile> = idx.iter().map(|&i| s.remote_files[i].clone()).collect();
        let on_card: std::vec::Vec<bool> = files.iter().map(|f| has(&f.name)).collect();
        let have = s.remote_files.iter().filter(|f| has(&f.name)).count();
        let sort = remote_sort_phrase(SORT_TARGET_FILES, s.files_sort.0, s.files_sort.1);
        (source, files, on_card, s.distant_filter.clone(), s.remote_files.len(), have, sort)
    });
    let Some((source, files, on_card, filter, total, have, sort)) = snap else { return };
    backend.draw_library_distant_files(&FilesPage {
        source: &source,
        files: &files,
        on_card: &on_card,
        selection,
        scroll,
        filter: filter.as_deref(),
        total,
        have,
        sort,
        outer_clip,
        interactive,
    });
}

/// The Flashpoint results (see `FpGrid`), from the state.
fn draw_fp_page(backend: &mut SwitchRenderBackend, selection: usize, scroll: usize, interactive: bool) {
    use crate::backend::render::{FpGrid, FpState};
    let snap = LIBRARY.lock().ok().map(|s| {
        let c = &s.cover_candidates;
        let titles: std::vec::Vec<std::string::String> = c.iter().map(|x| x.title.clone()).collect();
        let devs: std::vec::Vec<std::string::String> = c.iter().map(|x| x.developer.trim().to_string()).collect();
        let years: std::vec::Vec<std::string::String> =
            c.iter().map(|x| crate::backend::render::imp_year(&x.release_date).to_string()).collect();
        let urls: std::vec::Vec<std::string::String> = c.iter().map(|x| x.cover_url.clone()).collect();
        // Which hits are already in the library (the tick, and A = Jouer).
        let installed: std::vec::Vec<bool> = c
            .iter()
            .map(|x| {
                let name = crate::sources::gamezip::swf_filename(&x.title);
                s.entries.iter().any(|e| e.basename == name)
            })
            .collect();
        let state = if s.fp_loading {
            FpState::Loading
        } else if s.fp_error {
            FpState::Error
        } else if c.is_empty() {
            FpState::Empty
        } else {
            FpState::Results
        };
        let sort = remote_sort_phrase(SORT_TARGET_FP, s.fp_sort.0, s.fp_sort.1);
        (titles, devs, years, urls, installed, state, s.cover_msg.clone(), s.fp_query.clone(), s.fp_content_filter, sort)
    });
    let Some((titles, devs, years, urls, installed, state, msg, query, filter_on, sort)) = snap else { return };
    let t: std::vec::Vec<&str> = titles.iter().map(|x| x.as_str()).collect();
    let d: std::vec::Vec<&str> = devs.iter().map(|x| x.as_str()).collect();
    let y: std::vec::Vec<&str> = years.iter().map(|x| x.as_str()).collect();
    let u: std::vec::Vec<&str> = urls.iter().map(|x| x.as_str()).collect();
    backend.draw_library_fp_gallery(&FpGrid {
        query: &query,
        state,
        msg: &msg,
        selection,
        scroll,
        titles: &t,
        devs: &d,
        years: &y,
        urls: &u,
        installed: &installed,
        filter_on,
        sort,
        interactive,
    });
}

/// Poll the Flashpoint search in flight; the results fill the page when it
/// lands, in name order (db-api's own order reads as random in a grid).
fn poll_fp_search() {
    match net::tick_get_async() {
        net::GetPoll::Pending => {}
        net::GetPoll::Done(bytes) => {
            let (mut cands, msg, failed) = match crate::sources::gamezip::parse_search(&bytes) {
                Ok(list) => (list, std::string::String::new(), false),
                Err(e) => (std::vec::Vec::new(), e, true),
            };
            sort_fp_candidates(&mut cands, (0, false));
            if let Ok(mut s) = LIBRARY.lock() {
                s.cover_candidates = cands;
                s.cover_msg = msg;
                s.fp_loading = false;
                s.fp_error = failed;
                s.fp_sort = (0, false);
            }
        }
        net::GetPoll::Error(e) => {
            if let Ok(mut s) = LIBRARY.lock() {
                s.cover_candidates = std::vec::Vec::new();
                s.cover_msg = e;
                s.fp_loading = false;
                s.fp_error = true;
            }
        }
    }
}

/// Poll the archive.org fetch in flight (`DistantLoading`): its games, in name
/// order, or what went wrong.
fn poll_archive_fetch() {
    match net::tick_archive_fetch() {
        net::ArchivePoll::Pending => {}
        net::ArchivePoll::Done(mut files) => {
            if files.is_empty() {
                let label = LIBRARY.lock().map(|s| importer_label(&s.pending_fetch_url)).unwrap_or_default();
                set_import_error(ErrKind::Empty, crate::loc::s().err_no_swf, false, &label);
                return;
            }
            sort_remote_files(&mut files, (0, false));
            let total = files.len() as u32;
            let url = LIBRARY.lock().ok().map(|mut s| {
                s.remote_files = files;
                s.files_sort = (0, false);
                // The address worked: nothing to fix or retry on it any more.
                s.failed_url.clear();
                s.import_retry = None;
                // Fresh list: snap the glide to the top.
                crate::backend::render::gallery_anim_reset();
                s.screen = Screen::DistantFiles { selection: 0, scroll_offset: 0 };
                s.pending_fetch_url.clone()
            });
            if let Some(u) = url {
                push_history(&u);
                // Now that we know how many `.swf` this item holds, cache it so
                // the list can show "4 / 13" from now on.
                remember_url_total(&u, total);
                // A newly added address lands wherever the active sort puts it,
                // so retarget the reveal on its REAL row: that's where the window
                // must collapse back to.
                let target = LIBRARY.lock().ok().map(|s| {
                    let abs = s
                        .url_history
                        .iter()
                        .position(|h| h == &u)
                        .unwrap_or_else(|| s.url_history.len().saturating_sub(1));
                    let row = importer_row_for_abs(&s, abs);
                    let scroll = importer_scroll_for(
                        row,
                        IMPORT_SCROLL.load(std::sync::atomic::Ordering::Relaxed),
                        importer_view(&s).len(),
                    );
                    (row, scroll)
                });
                if let Some((row, scroll)) = target {
                    crate::backend::render::distant_reveal_set_source(row, scroll);
                }
            }
        }
        net::ArchivePoll::Error(f) => {
            let Ok(mut s) = LIBRARY.lock() else { return };
            let label = importer_label(&s.pending_fetch_url);
            // archive.org answering "nothing here" is a wrong address, which
            // asking again will not fix (`f.retry` is false for it, as for a 404
            // or an answer too big). When what was typed is one bare word, it
            // may have been a game's name: offer Flashpoint for it.
            if f.msg == crate::loc_ui::ui().imp_err_item_missing {
                // The identifier asked for, which is the word itself when it
                // came after an archive.org address: a game's name typed there
                // is still one bare word.
                let typed = net::extract_item_id(&s.failed_url).unwrap_or_default();
                if !typed.is_empty() && !typed.contains('/') && !typed.contains('.') {
                    s.import_fp_suggest = typed;
                }
            }
            import_error(&mut s, ErrKind::Open, &f.msg, f.retry, &label);
        }
    }
}

/// Send again what failed (A on the error page), without the keyboard. Called
/// from `input()` only: opening an address takes the lock itself.
fn retry_import(action: RetryAction) {
    match action {
        RetryAction::Fetch { url, row } => {
            if let Ok(mut s) = LIBRARY.lock() {
                clear_import_error(&mut s);
                clear_download_state(&mut s);
            }
            run_fetch_for_url(&url, row, IMPORT_SCROLL.load(std::sync::atomic::Ordering::Relaxed));
        }
        RetryAction::ArchiveFile { selection, scroll } => {
            let Ok(mut s) = LIBRARY.lock() else { return };
            clear_import_error(&mut s);
            clear_download_state(&mut s);
            if s.remote_files.is_empty() {
                let row = s.import_return_row;
                s.screen = distant_idle_at(&s, row);
                return;
            }
            s.screen = Screen::DistantFiles { selection, scroll_offset: scroll };
            handle_distant_files_input(&mut s, "A", selection, scroll);
        }
        RetryAction::FpGame { selection, scroll } => {
            let Ok(mut s) = LIBRARY.lock() else { return };
            clear_import_error(&mut s);
            clear_download_state(&mut s);
            if s.cover_candidates.is_empty() {
                let row = s.import_return_row;
                s.screen = distant_idle_at(&s, row);
                return;
            }
            s.screen = Screen::FpGallery { selection, scroll };
            handle_fp_gallery_input(&mut s, "A", selection, scroll);
        }
    }
}

/// The steps of an import, in the order they run.
#[derive(Clone, Copy, PartialEq, PartialOrd)]
enum ImportPhase {
    Download,
    Extract,
    Files,
    Data,
    Cover,
}

/// The import page at `phase` (see `ImportProgress`): `frac` and `detail` are
/// the active step's bar and line, `cancellable` whether B stops it.
fn draw_import_page(
    backend: &mut SwitchRenderBackend,
    phase: ImportPhase,
    frac: Option<f32>,
    detail: &str,
    cancellable: bool,
) {
    use crate::backend::render::{Btn, Icon, ImportProgress, ImportStep, StepState};
    let snap = LIBRARY.lock().ok().map(|s| {
        let fp = s.download_zip_extract.is_some() || s.download_fp_direct;
        let zipped = s.download_zip_extract.is_some() && !s.download_fp_direct;
        let title = s
            .download_title
            .clone()
            .filter(|t| !t.trim().is_empty())
            .unwrap_or_else(|| pretty_name(&s.download_file_name));
        let cover = if fp { s.download_cover_url.clone() } else { None };
        let source = (!fp && !s.remote_files.is_empty()).then(|| importer_label(&s.pending_fetch_url));
        (fp, zipped, title, cover, source, s.import_had_files, s.import_had_data)
    });
    let Some((fp, zipped, title, cover, source, had_files, had_data)) = snap else { return };
    let lc = crate::loc::s();
    let u = crate::loc_ui::ui();
    let mut list: std::vec::Vec<(ImportPhase, Icon, &str)> =
        std::vec![(ImportPhase::Download, Icon::Download, u.imp_step_download)];
    if zipped {
        list.push((ImportPhase::Extract, Icon::Package, u.imp_step_extract));
    }
    if had_files || phase == ImportPhase::Files {
        list.push((ImportPhase::Files, Icon::Layers, u.imp_step_files));
    }
    if had_data || phase == ImportPhase::Data {
        list.push((ImportPhase::Data, Icon::File, u.imp_step_data));
    }
    if fp || crate::loc::covers_online() {
        list.push((ImportPhase::Cover, Icon::Image, lc.opt_cover));
    }
    let steps: std::vec::Vec<ImportStep> = list
        .iter()
        .map(|(ph, icon, label)| {
            let state = if *ph < phase {
                StepState::Done
            } else if *ph == phase {
                StepState::Active
            } else {
                StepState::Waiting
            };
            ImportStep {
                icon: *icon,
                label,
                state,
                frac: if *ph == phase { frac } else { None },
                detail: if *ph == phase { detail } else { "" },
            }
        })
        .collect();
    let mut crumbs: std::vec::Vec<&str> = std::vec![u.tab_import];
    if fp {
        crumbs.push(lc.fp_title);
    } else if let Some(src) = source.as_deref() {
        crumbs.push(src);
    }
    let hints = [(&[Btn::B][..], u.cancel)];
    backend.draw_import_progress(&ImportProgress {
        crumbs: &crumbs,
        title: &title,
        logo_url: cover.as_deref(),
        icon: if fp { Icon::Package } else { Icon::Download },
        steps: &steps,
        hints: if cancellable { &hints } else { &[] },
    });
}

/// The import page for a step that blocks the frame loop (the extraction, the
/// game's data, its cover): drawn and shown at once, since nothing else will be
/// until the step returns. A bar frozen at 100 % while the cover was fetched
/// read as a hang.
fn show_import_step(backend: &mut SwitchRenderBackend, phase: ImportPhase, frac: Option<f32>, detail: &str) {
    draw_import_page(backend, phase, frac, detail, false);
    unsafe { gl_context_swap() };
}

/// Pump the download in flight one frame and draw where it stands: the main
/// transfer, then the game's other files. `tick` false only draws (its stop
/// question is closing).
fn drive_download(backend: &mut SwitchRenderBackend, tick: bool) {
    let companion = LIBRARY
        .lock()
        .ok()
        .map(|s| {
            (
                s.dl_companion_active,
                s.dl_companion_current.clone(),
                s.dl_companion_queue.len() as u32,
                s.dl_companion_done,
            )
        })
        .unwrap_or((false, std::string::String::new(), 0, 0));
    let (done, total) = net::download_progress();
    let frac = (total > 0).then(|| (done as f32 / total as f32).clamp(0.0, 1.0));
    if companion.0 {
        // The game's other files, one after the other on the same bar: which
        // one of how many, and its name.
        let (current, queued, finished) = (&companion.1, companion.2, companion.3);
        let n = crate::backend::render::imp_n_of(&(finished + 1).to_string(), &(finished + 1 + queued).to_string());
        let detail = std::format!("{}  \u{00B7}  {}", n, current);
        draw_import_page(backend, ImportPhase::Files, frac, &detail, false);
        if tick {
            match net::tick_download() {
                Ok(false) => {}
                // Done OR errored: a failed file is skipped (the game may still
                // partly work, and the mirror can serve it at play time), the
                // queue carries on / finalizes.
                _ => companion_download_finished(backend),
            }
        }
    } else {
        let line = crate::backend::render::imp_transfer_line(done, total, download_rate(done));
        draw_import_page(backend, ImportPhase::Download, frac, &line, true);
        if tick {
            match net::tick_download() {
                Ok(false) => {}
                Ok(true) => on_download_finished(backend),
                Err(f) => {
                    if let Ok(mut s) = LIBRARY.lock() {
                        let name = s
                            .download_title
                            .clone()
                            .filter(|t| !t.trim().is_empty())
                            .unwrap_or_else(|| pretty_name(&s.download_file_name));
                        import_error(&mut s, ErrKind::Download, &f.msg, f.retry, &name);
                    }
                }
            }
        }
    }
}

/// The importer's error page, from the state: what failed (the title), on what
/// (the breadcrumb), the message, and the hints that can do something about it.
fn draw_error_page(backend: &mut SwitchRenderBackend) {
    use crate::backend::render::{Btn, HintItem};
    let snap = LIBRARY.lock().ok().map(|s| {
        (
            s.distant_error.clone(),
            s.import_error_kind,
            s.import_error_context.clone(),
            s.import_retry.is_some(),
            !s.import_fp_suggest.is_empty(),
            !s.failed_url.is_empty(),
        )
    });
    let Some((msg, kind, context, retry, suggest, can_fix)) = snap else { return };
    let u = crate::loc_ui::ui();
    let title = match kind {
        ErrKind::Open => u.imp_err_open,
        ErrKind::Download => u.imp_err_download,
        ErrKind::Empty => u.imp_err_empty,
        ErrKind::Unsupported => u.imp_err_unsupported,
    };
    let mut crumbs: std::vec::Vec<&str> = std::vec![u.tab_import];
    if !context.is_empty() {
        crumbs.push(&context);
    }
    let mut hints: std::vec::Vec<HintItem> = std::vec::Vec::new();
    if retry {
        hints.push((&[Btn::A][..], u.imp_retry));
    }
    if suggest {
        hints.push((&[Btn::X][..], u.imp_fp_title));
    }
    if can_fix {
        hints.push((&[Btn::Y][..], u.imp_edit_address));
    }
    hints.push((&[Btn::B][..], u.back));
    backend.draw_import_error(&crumbs, title, &msg, &hints);
}

/// Draw the JOUER gallery (snapshot + draw) — shared by the List screen and the
/// frozen background of the `Launching` reveal.
fn draw_gallery(
    backend: &mut SwitchRenderBackend,
    selection: usize,
    scroll_offset: usize,
    anim_origin: u64,
) {
    let snapshot = LIBRARY.lock().ok().map(|s| {
        set_active_folder(s.folder_filter.as_deref());
        let idx = local_filtered_indices(&s.entries, &s.local_filter, &s.folder_filter);
        // The strip under the tabs, published here, under the same lock that
        // reads the entries, so it can never describe a different list than
        // the one being drawn.
        with_shelf_stops(&s.entries, |stops, fp| {
            use crate::backend::render::{HomeChip, HomeChipKind, HomeStripState};
            let active = stops.iter().position(|st| st.key.as_deref() == s.folder_filter.as_deref()).unwrap_or(0);
            // Favourites lead every order; the sort indicator says so with a
            // star whenever one leads the list on screen (they are pinned
            // first, so the first game tells), except on their own shelf.
            let favorites_first = s.folder_filter.as_deref() != Some(FAVORITES_SHELF)
                && idx.first().is_some_and(|&i| crate::favorites::is_favorite(&s.entries[i].basename));
            crate::backend::render::home_strip_publish(
                fp,
                || {
                    stops
                        .iter()
                        .map(|st| HomeChip {
                            kind: st.kind,
                            label: st.label.clone(),
                            count: if st.kind == HomeChipKind::All { Some(st.count) } else { None },
                        })
                        .collect()
                },
                &HomeStripState {
                    active,
                    search: s.local_filter.as_deref(),
                    matches: idx.len(),
                    sort_mode: sort_mode_index(),
                    sort_reverse: current_sort_reverse(),
                    favorites_first,
                },
            );
        });
        let entries: std::vec::Vec<Entry> = idx.iter().map(|&i| s.entries[i].clone()).collect();
        (
            entries,
            // How many games the player is LOOKING AT before their search, i.e.
            // the open shelf, not the library: the tab row's count.
            local_filtered_indices(&s.entries, &None, &s.folder_filter).len(),
        )
    });
    if let Some((entries, total_unfiltered)) = snapshot {
        let phase_ticks = unsafe { ruffle_tick_now() }.saturating_sub(anim_origin);
        // Same data, same selection, same actions — only the layout differs
        // (REGLAGES > AFFICHAGE HOME). Each one publishes the same cell table, so
        // navigation, touch and the launch reveal are shared.
        let a = (selection, scroll_offset, &entries, phase_ticks, total_unfiltered);
        match crate::loc::home_view() {
            1 => backend.draw_library_list_view(a.0, a.1, a.2, a.3, a.4),
            // BANDE is now the big-cover-plus-full-width-row layout: it was built as
            // ETAGERE, and reads better as the strip than the strip did. The old
            // centred-cover strip is gone rather than kept as a near-duplicate.
            2 => backend.draw_library_shelf_view(a.0, a.1, a.2, a.3, a.4),
            3 => backend.draw_library_etagere_view(a.0, a.1, a.2, a.3, a.4),
            _ => backend.draw_library_gallery(a.0, a.1, a.2, a.3, a.4),
        }
    }
}

/// Draw the launch/quit reveal for openness `frac` (0 = tile, 1 = full): the
/// game's cover carried in a window that grows from the tile to the screen, or
/// shrinks back to it, over the gallery behind.
fn draw_game_reveal_window(backend: &mut SwitchRenderBackend, frac: f32, fade: f32) {
    let (vw, vh) = backend.screen_size();
    let ((rx, ry, rw, rh), basename, name, color) = crate::backend::render::game_reveal_info();
    let wx = rx * (1.0 - frac);
    let wy = ry * (1.0 - frac);
    let ww = rw + (vw - rw) * frac;
    let wh = rh + (vh - rh) * frac;
    backend.draw_game_reveal(wx, wy, ww, wh, frac, (rw, rh), &basename, &name, color);
    // Launch fade-to-black: a black veil over the (full-screen) cover so the game
    // pops calmly out of the dark instead of replacing the cover in one frame.
    if fade > 0.0 {
        let alpha = (fade.clamp(0.0, 1.0) * 255.0) as u32;
        backend.draw_overlay_rect(0.0, 0.0, vw, vh, alpha << 24);
    }
    // Multi-file indicator (v1.3.0): once the cover fills the screen (the SWF is
    // loading behind it), flag that this game pulls in companion SWFs from its
    // `.files/` folder. Count is reset to 0 on the quit reveal, so launch-only.
    let multi = LAUNCH_MULTI.load(std::sync::atomic::Ordering::Relaxed);
    if multi && frac >= 0.85 {
        // Fading in over the last stretch of the opening, not popping.
        backend.draw_multifile_badge((frac - 0.85) / 0.15);
    }
}

/// Render the current screen using the backend. C++ calls this each frame
/// while the library is active, AFTER `glClear` (we own the entire
/// framebuffer — no Ruffle behind us at this stage).
pub fn render(backend: &mut SwitchRenderBackend) {
    // Run any deferred profile network flow now that its loading panel has shown
    // a frame (#20). May transition the screen, so do it before reading it.
    drive_pending_net();
    drive_pending_move();
    let s = match LIBRARY.lock() {
        Ok(g) => g,
        Err(_) => return,
    };
    let mut screen = s.screen;
    let anim_origin = s.anim_origin_ticks;
    // An error arriving, from a press or from a download finishing: one note.
    {
        static LAST_ERROR: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);
        let is_error = matches!(screen, Screen::DistantError | Screen::SendFailed { .. });
        if is_error && !LAST_ERROR.swap(true, core::sync::atomic::Ordering::Relaxed) {
            crate::sfx::play(crate::sfx::Sfx::Error);
        } else if !is_error {
            LAST_ERROR.store(false, core::sync::atomic::Ordering::Relaxed);
        }
    }
    // The folder picker draws from state the lock owns, so snapshot it here
    // rather than reach back for it after the guard is gone.
    let (dir_path, dir_browse) = match s.screen {
        Screen::GamesDirPicker { .. } | Screen::GamesDirConfirm { .. } => {
            (s.dir_path.clone(), s.dir_browse.clone())
        }
        _ => (std::string::String::new(), std::vec::Vec::new()),
    };
    let game_count = s.entries.len();
    drop(s);

    let now = unsafe { ruffle_tick_now() };

    // Scale pop (v1.2.0): everything that appears scales UP — tab switches,
    // modals, and the TOUCHES editor (open begun in `input`). Modals also scale
    // DOWN on close; that screen-swap is DEFERRED (input rewinds the screen and
    // starts the close pop), and we apply the stashed target only once the pop
    // finishes here. The transform is reset to identity before the navbar below.
    let closing = crate::backend::render::modal_close_active();
    // Open detection: fire the grow pop the first frame a modal appears. Skipped
    // while closing, since the screen is the rewound modal then.
    if !closing {
        let k = modal_kind(screen);
        if let Ok(mut last) = LAST_MODAL_KIND.lock() {
            if k != 0 && k != *last {
                crate::backend::render::modal_open_begin();
            }
            *last = k;
        }
    }
    let (modal_scale, modal_active, close_done) = crate::backend::render::modal_scale_step(now);
    if close_done {
        // Close pop finished — apply the stashed action/target now.
        let pending = PENDING_AFTER_CLOSE.lock().ok().and_then(|mut p| p.take());
        if let Ok(mut s2) = LIBRARY.lock() {
            match pending {
                Some(PendingClose::Goto(t)) => {
                    s2.screen = t;
                }
                Some(PendingClose::CloseMenuGoto(t)) => {
                    // The editor finished scaling out — now close the menu module
                    // and land on the tab (which does not pop, being a tab).
                    menu::close();
                    s2.screen = t;
                }
                Some(PendingClose::DeleteGame { game_idx }) => {
                    delete_game(&mut s2, game_idx);
                    if s2.entries.is_empty() {
                        s2.screen = Screen::Empty;
                    } else {
                        s2.local_filter = None;
                        // `game_idx` is an ABSOLUTE index and the list is a
                        // FILTERED view of it. With no folder open the two
                        // coincide, which is why this was fine until folders
                        // (issue #68): inside a shelf of six, deleting the
                        // fortieth game of the library left the cursor at row 40
                        // of a six-row list. Clamp against the view that is
                        // actually on screen, and drop an emptied shelf.
                        // Did the shelf on screen just lose its last game? Asked
                        // by MEMBERSHIP, not by an empty filter result: a named
                        // folder that matches nothing falls back to the whole
                        // library, so testing the result would never fire.
                        let gone = match s2.folder_filter.as_deref() {
                            Some(f) if !f.is_empty() => {
                                !s2.entries.iter().any(|e| on_shelf(e, f))
                            }
                            _ => false,
                        };
                        if gone {
                            s2.folder_filter = None;
                            set_active_folder(None);
                            sort_entries(
                                &mut s2.entries,
                                current_sort_mode(),
                                current_sort_reverse(),
                            );
                        }
                        // The deleted game may have been the last on its shelf,
                        // wherever the cursor happened to be.
                        prune_folder_sorts(&s2.entries);
                        let shown = local_filtered_indices(
                            &s2.entries,
                            &s2.local_filter,
                            &s2.folder_filter,
                        );
                        // Same ROW as the game that was deleted, clamped to the
                        // new length: the neighbour slides under the cursor,
                        // which is where the eye already is.
                        let row = shown
                            .iter()
                            .position(|&i| i >= game_idx)
                            .unwrap_or(shown.len().saturating_sub(1));
                        let new_sel = row.min(shown.len().saturating_sub(1));
                        let scroll = gallery_scroll_for(new_sel, jouer_scroll());
                        s2.screen = Screen::List { selection: new_sel, scroll_offset: scroll };
                        // Layout shifted (a tile is gone) — snap the glide and
                        // start the covers closed.
                        crate::backend::render::gallery_anim_reset();
                        crate::backend::render::tile_open_reset();
                    }
                }
                Some(PendingClose::DeleteHistory { row }) => {
                    delete_current_history(&mut s2);
                    // The same ROW, clamped to what is left: the source below
                    // slides under the cursor, where the eye already is.
                    s2.screen = distant_idle_at(&s2, row);
                }
                Some(PendingClose::StopDownload) => {
                    // Not once the game's other files have started: they
                    // finish the import (see the B rule on DistantDownloading).
                    if s2.dl_companion_active {
                        s2.screen = Screen::DistantDownloading;
                    } else {
                        stop_download(&mut s2);
                    }
                }
                None => {}
            }
            screen = s2.screen;
        }
        // If the close landed ON another modal, arm ITS open pop — and step the
        // animation once in this same frame. Recording the kind without arming
        // anything is what made the global keymap editor's parent panel appear at
        // full size for one frame and then drop to 0.55: a visible hiccup with no
        // cause on screen. Stepping here also stops that first frame drawing at
        // 1.0 before the pop has started.
        let kind = modal_kind(screen);
        if kind != 0 {
            crate::backend::render::modal_open_begin();
            let _ = crate::backend::render::modal_scale_step(now);
        }
        if let Ok(mut last) = LAST_MODAL_KIND.lock() {
            *last = kind;
        }
    }
    // Stamped for the touch tables, EVERY frame: whatever list draws next tags
    // its rows with it, so a tap can only ever act on the panel actually up.
    // (It used to sit inside the close-pop branch above, which meant the stamp
    // only ever landed on the frame a modal finished closing -- every other
    // frame the tables carried a stale id, or 0.)
    //
    // NOT `modal_kind`. That one drives the scale-in pop, so a screen has to
    // earn a place in it by being a modal -- REGLAGES and the report list are
    // full pages and answer 0 there, and a table tagged 0 is ignored by the hit
    // test. That is exactly why those two stayed dead to the touch.
    //
    // A panel scaling OUT is stamped 0: its rows are still drawn, but shrinking
    // away from where they were, so a tap landing on them would act on a panel
    // the user has already dismissed.
    // The same goes for a tab slide: the rows are mid-translation, the table is
    // not.
    let ui_kind = if closing || modal_active || crate::backend::render::tab_slide_active() {
        0
    } else {
        touch_kind(screen)
    };
    crate::backend::render::set_ui_screen_kind(ui_kind);
    // Modals + the TOUCHES editor SCALE (open/close pop); tab-home content SLIDES
    // on an L/R switch. `modal_active` = an in-flight scale pop; a settled modal
    // stays full; a tab-home slides while a tab slide runs; else identity.
    if modal_active {
        backend.set_ui_modal_scale(modal_scale);
    } else if modal_kind(screen) != 0 {
        backend.set_ui_modal_scale(1.0);
    } else if crate::backend::render::tab_slide_active() {
        let dx = crate::backend::render::tab_slide_translate(now, 140.0);
        backend.set_ui_slide(dx);
    } else {
        backend.clear_ui_transform();
    }

    // The options family slides its panel in from the right on the window
    // timeline, and publishes its rows under the screen's own touch tag. Only
    // when it comes from somewhere else: back from the controls editor (drawn
    // over the same panels) the panel is already in place, and sliding it in
    // again would replay its opening for nothing.
    let class: u8 = if options_panel::is_options_family(screen) {
        1
    } else if matches!(screen, Screen::TouchesEditor { .. }) {
        2
    } else {
        0
    };
    static CLASS_PREV: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);
    static SLIDE_IN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);
    match (CLASS_PREV.swap(class, std::sync::atomic::Ordering::Relaxed), class) {
        (0, 1) => SLIDE_IN.store(true, std::sync::atomic::Ordering::Relaxed),
        (2, 1) => SLIDE_IN.store(false, std::sync::atomic::Ordering::Relaxed),
        _ => {}
    }
    let slide_in = SLIDE_IN.load(std::sync::atomic::Ordering::Relaxed);
    let family_slide = if closing || (modal_active && slide_in) { Some(modal_scale) } else { None };
    let family_kind = if closing || modal_active { 0 } else { touch_kind(screen) };

    // Phase = current tick - origin. Drives sin() animations. ms-resolution
    // is enough; we compute it lazily below to avoid an FFI call when the
    // current screen has no animation.
    match screen {
        Screen::Inactive | Screen::Picked | Screen::Quit => {}
        Screen::AppletNotice { selection, scroll_offset } => {
            use crate::backend::render::{Choice, Icon};
            // A window over the page the game was pressed on, like every other
            // window: the grid, or the importer page of a game started from
            // there, still and taking no taps. While the window scales out,
            // the press has already taken that page off `LAUNCH_BACKDROP` and
            // the close's target names it instead.
            let behind = match PENDING_AFTER_CLOSE.lock().ok().as_deref() {
                Some(Some(PendingClose::Goto(page))) => Some(*page),
                _ => LAUNCH_BACKDROP.lock().ok().and_then(|g| *g),
            };
            backend.clear_ui_transform();
            crate::backend::render::set_ui_screen_kind(0);
            match behind {
                Some(page @ (Screen::DistantIdle { .. } | Screen::DistantFiles { .. } | Screen::FpGallery { .. })) => {
                    draw_importer_backdrop(backend, page)
                }
                other => {
                    let (sel, scroll) = match other {
                        Some(Screen::List { selection, scroll_offset }) => (selection, scroll_offset),
                        _ => (selection, scroll_offset),
                    };
                    let logo = LIBRARY.lock().map(|s| (s.banner_tex, s.banner_w, s.banner_h)).unwrap_or((0, 0, 0));
                    draw_gallery(backend, sel, scroll, anim_origin);
                    backend.draw_navbar_with(Tab::Jouer.index(), logo, false);
                    crate::backend::render::navbar_clear();
                }
            }
            crate::backend::render::set_ui_screen_kind(ui_kind);
            backend.set_ui_modal_scale(if modal_active { modal_scale } else { 1.0 });
            let (lc, u) = (crate::loc::s(), crate::loc_ui::ui());
            backend.draw_confirm(
                Icon::Info,
                lc.applet_title,
                lc.applet_notice,
                None,
                &[Choice { label: u.close, destructive: false }],
                0,
                false,
            );
        }
        Screen::Empty => {
            backend.draw_library_empty();
        }
        Screen::List { selection, scroll_offset } => {
            let (selection, scroll_offset) = settle_vanished_shelf(selection, scroll_offset);
            let mut scroll_offset = scroll_offset;
            draw_gallery(backend, selection, scroll_offset, anim_origin);
            // Back from a game started on an importer page: the grid has just
            // published its own table, so the game's row can be read now. Drawn
            // again at once if that scrolls, so no frame shows the old scroll.
            if JOUER_RESCROLL.swap(false, std::sync::atomic::Ordering::Relaxed) {
                let placed = gallery_scroll_for(selection, scroll_offset);
                if placed != scroll_offset {
                    if let Ok(mut s) = LIBRARY.lock() {
                        if s.screen == (Screen::List { selection, scroll_offset }) {
                            s.screen = Screen::List { selection, scroll_offset: placed };
                        }
                    }
                    scroll_offset = placed;
                    crate::backend::render::gallery_anim_reset();
                    draw_gallery(backend, selection, scroll_offset, anim_origin);
                }
            }
            // Remember where the gallery is, so every path that comes BACK to it
            // restores this scroll instead of deriving one from the row.
            JOUER_SCROLL.store(scroll_offset, std::sync::atomic::Ordering::Relaxed);
            // Quit collapse reveal: the cover shrinks from full screen back to its
            // tile over the gallery (active only right after returning from a
            // game; a no-op otherwise — the anim deactivates itself when idle).
            if let Some((frac, fade, _collapsing, _done)) =
                crate::backend::render::game_reveal_step(now)
            {
                draw_game_reveal_window(backend, frac, fade);
            }
        }
        Screen::Launching { selection, scroll_offset } => {
            // Launch reveal: the cover opens from the tile to full screen over the
            // frozen gallery, then we flip to Picked (the C++ loop exits + loads
            // the SWF behind that frozen full-screen cover = a free loading screen).
            // A game started from the importer opens over the importer page it
            // was pressed on instead (`launch_from_importer`).
            match LAUNCH_BACKDROP.lock().ok().and_then(|g| *g) {
                Some(page) => draw_importer_backdrop(backend, page),
                None => draw_gallery(backend, selection, scroll_offset, anim_origin),
            }
            let done = match crate::backend::render::game_reveal_step(now) {
                Some((frac, fade, _collapsing, done)) => {
                    draw_game_reveal_window(backend, frac, fade);
                    done
                }
                None => true,
            };
            if done {
                if let Ok(mut g) = LAUNCH_BACKDROP.lock() {
                    *g = None;
                }
                if let Ok(mut s) = LIBRARY.lock() {
                    s.screen = Screen::Picked;
                }
            }
        }
        Screen::OptionsModal { .. } | Screen::GameFolderPicker { .. } | Screen::GameFolderDelete { .. } => {
            options_panel::draw_options_family(backend, screen, now, anim_origin, family_slide, family_kind);
        }
        Screen::TouchesEditor { game_idx } => {
            // The game's options and their Touches page stay behind the editor,
            // still, taking no taps, dimmed by the editor's scrim: the path
            // stays on screen and B is seen to land where it was. It used to
            // clear the screen to flat navy at the deepest level of the path.
            backend.clear_ui_transform();
            crate::backend::render::set_ui_screen_kind(0);
            options_panel::draw_options_family(
                backend,
                Screen::TouchesMenu { game_idx, selection: 0 },
                now,
                anim_origin,
                None,
                0,
            );
            crate::backend::render::set_ui_screen_kind(ui_kind);
            backend.set_ui_modal_scale(if modal_active { modal_scale } else { 1.0 });
            menu::draw(backend, now);
        }
        Screen::Settings { .. } | Screen::GamesDirPicker { .. } | Screen::BugPicker { .. } => {
            // A navbar TAB, not a popup: the page, its column and its pane, or
            // a page opened inside the pane.
            let logo = LIBRARY.lock().ok().map(|s| (s.banner_tex, s.banner_w, s.banner_h)).unwrap_or_default();
            settings_tab::draw_settings(backend, screen, (&dir_path, &dir_browse), logo, game_count, false);
        }
        Screen::GamesDirConfirm { .. } | Screen::SettingsKeymapEditor | Screen::SendFailed { .. } => {
            // A window over the tab: the page stays behind it, still, dimmed by
            // the window's scrim, so B is seen to land where it was. Drawn
            // outside the window's pop and with no touch table of its own.
            let logo = LIBRARY.lock().ok().map(|s| (s.banner_tex, s.banner_w, s.banner_h)).unwrap_or_default();
            backend.clear_ui_transform();
            crate::backend::render::set_ui_screen_kind(0);
            settings_tab::draw_settings(backend, screen, (&dir_path, &dir_browse), logo, game_count, true);
            crate::backend::render::navbar_clear();
            crate::backend::render::set_ui_screen_kind(ui_kind);
            backend.set_ui_modal_scale(if modal_active { modal_scale } else { 1.0 });
            match screen {
                Screen::GamesDirConfirm { selection } => {
                    // Moving a whole library is not undone by pressing B
                    // afterwards, so it is asked once -- and it says HOW MANY
                    // games and WHERE, because "move the library?" tells you
                    // nothing about what you agree to.
                    settings_tab::draw_move_question(backend, selection, &dir_path, game_count);
                }
                Screen::SendFailed { selection, .. } => settings_tab::draw_send_failed(backend, selection),
                // The editor edits the global default: the keymap module was
                // pointed there on entry.
                _ => menu::draw(backend, now),
            }
        }
        Screen::GamesDirMoving => {
            // Drawn for every frame of the move: where from, where to, and a
            // real count, not a spinner that says only "something is
            // happening".
            let (done, total) = move_progress();
            match move_paths() {
                Some((src, dest)) => backend.draw_library_move_page(
                    src.trim_start_matches("sdmc:"),
                    dest.trim_start_matches("sdmc:"),
                    done,
                    total,
                ),
                None => {
                    // No move behind this page (`start_move` refused it, or it
                    // ended without sending the screen back): the screen
                    // swallows every button, so it must not stay up on its own.
                    let root = primary_root();
                    backend.draw_library_move_page(root.trim_start_matches("sdmc:"), "", total, total);
                    if let Ok(mut s) = LIBRARY.lock() {
                        s.screen = settings_at(SetSection::Library, "DOSSIER");
                    }
                }
            }
        }
        Screen::TouchesMenu { .. }
        | Screen::ProfileList { .. }
        | Screen::ProfileDeleteConfirm { .. }
        | Screen::ProfilePreview { .. }
        | Screen::RevertPreview { .. }
        | Screen::ProfileShareConfirm { .. }
        | Screen::DeleteConfirm { .. }
        | Screen::CoverPicker { .. } => {
            options_panel::draw_options_family(backend, screen, now, anim_origin, family_slide, family_kind);
        }
        Screen::ProfileLoading => {
            // The panels as they were, the spinner over them, for the frame
            // before the deferred GitHub call runs (#20).
            let label = NET_LOADING_LABEL.lock().ok().map(|l| l.clone()).unwrap_or_default();
            options_panel::draw_options_loading(backend, now, anim_origin, &label);
        }
        Screen::FpGallery { selection, scroll } => {
            let reveal = crate::backend::render::distant_reveal_step(now);
            // The search in flight lands here, and a details blurb still on its
            // way after its window closed is collected. Not while the page folds
            // back: B cancelled the search, whose handle now answers with an
            // error, and the fold shows the waiting page it started from.
            let folding = reveal.map_or(false, |(_, collapsing, _)| collapsing);
            if !folding && LIBRARY.lock().map(|s| s.fp_loading).unwrap_or(false) {
                poll_fp_search();
            }
            drive_fp_details();
            // The page opens out of the Flashpoint card and folds back into it,
            // as an archive opens out of its row.
            if let Some((frac, collapsing, done)) = reveal {
                let (vw, vh) = backend.screen_size();
                let row = LIBRARY.lock().map(|s| s.import_return_row).unwrap_or(0);
                draw_home_backdrop(backend, row, IMPORT_SCROLL.load(std::sync::atomic::Ordering::Relaxed));
                let (rx, ry, rw, rh) = importer_item_rect(0, 0.0, vw);
                let (wx, wy, ww, wh) = reveal_window((rx, ry, rw, rh), vw, vh, frac);
                backend.set_clip(wx, wy, ww, wh);
                draw_fp_page(backend, selection, scroll, false);
                backend.clear_clip();
                backend.draw_reveal_chrome(wx, wy, ww, wh);
                if done && collapsing {
                    // Back on the importer, on the row the search was asked
                    // from. The query stays, for the keyboard's next prefill;
                    // the results go, and their logos' textures with them.
                    crate::backend::render::thumb_cancel_all();
                    crate::backend::render::thumb_cache_clear();
                    if let Ok(mut s) = LIBRARY.lock() {
                        s.cover_candidates.clear();
                        s.cover_msg.clear();
                        s.fp_error = false;
                        s.fp_loading = false;
                        s.fp_desc_slot = -1;
                        s.fp_size_slot = -1;
                        let row = s.import_return_row;
                        s.screen = distant_idle_at(&s, row);
                    }
                }
            } else {
                draw_fp_page(backend, selection, scroll, true);
            }
        }
        Screen::FpDetails { selection, scroll, size } => {
            drive_fp_details();
            // The results stay behind the window.
            backend.clear_ui_transform();
            draw_fp_page(backend, selection, scroll, false);
            backend.set_ui_modal_scale(if modal_active { modal_scale } else { 1.0 });
            let snap = LIBRARY.lock().ok().and_then(|s| {
                let desc = s.fp_desc_ready.then(|| s.fp_desc.clone());
                let pending = s.fp_size_pending;
                s.cover_candidates.get(selection).map(|c| {
                    let name = crate::sources::gamezip::swf_filename(&c.title);
                    (
                        c.title.clone(),
                        c.developer.clone(),
                        c.publisher.clone(),
                        crate::backend::render::imp_release(&c.release_date),
                        c.cover_url.clone(),
                        desc,
                        s.entries.iter().any(|e| e.basename == name),
                        pending,
                    )
                })
            });
            if let Some((title, dev, publisher, release, cover, desc, installed, pending)) = snap {
                backend.draw_library_fp_details(&crate::backend::render::FpDetailsView {
                    title: &title,
                    developer: &dev,
                    publisher: &publisher,
                    release: &release,
                    size: if pending { None } else { Some(size) },
                    cover_url: &cover,
                    description: desc.as_deref(),
                    installed,
                });
            }
        }
        Screen::SortModal { selection, prev_sel, prev_scroll, dirs } => {
            use crate::backend::render::SortOption;
            // The home under the window, as it will be once the window closes
            // (a new sort lands on the first game), still and taking no taps,
            // its covers' clocks running. Nothing drew it: the window's scrim
            // landed on whatever the last frame left, darker every frame.
            let (behind_sel, behind_scroll) = match PENDING_AFTER_CLOSE.lock().ok().as_deref() {
                Some(Some(PendingClose::Goto(Screen::List { selection, scroll_offset }))) => (*selection, *scroll_offset),
                _ => (prev_sel, prev_scroll),
            };
            let logo = LIBRARY.lock().map(|s| (s.banner_tex, s.banner_w, s.banner_h)).unwrap_or((0, 0, 0));
            backend.clear_ui_transform();
            crate::backend::render::set_ui_screen_kind(0);
            draw_gallery(backend, behind_sel, behind_scroll, anim_origin);
            // L and R do not switch tabs under the window: no L / R chips.
            backend.draw_navbar_with(Tab::Jouer.index(), logo, false);
            crate::backend::render::navbar_clear();
            crate::backend::render::set_ui_screen_kind(family_kind);
            backend.set_ui_modal_scale(if modal_active { modal_scale } else { 1.0 });

            let (lc, u) = (crate::loc::s(), crate::loc_ui::ui());
            let options: [SortOption; SORT_MODES] = core::array::from_fn(|m| SortOption {
                icon: sort_icon(m),
                label: sort_criterion(m),
                orders: [sort_state_label(m, false), sort_state_label(m, true)],
            });
            // Each shelf keeps its own order, so the window says which one.
            let shelf = LIBRARY
                .lock()
                .ok()
                .and_then(|s| s.folder_filter.clone())
                .map(|f| if f == FAVORITES_SHELF { u.home_favorites.to_string() } else { f })
                .unwrap_or_else(|| u.home_all.to_string());
            let scope = u.sort_scope.replacen("{}", &shelf, 1);
            backend.draw_sort_window(
                lc.sort_title,
                Some(&scope),
                &options,
                selection,
                Some(sort_mode_index()),
                dirs,
                Some(u.help_sort),
            );
        }
        Screen::RemoteSortModal { selection, kind, reverse, prev_sel, prev_scroll } => {
            // The list being sorted stays behind the window, as it was.
            backend.clear_ui_transform();
            match kind {
                SORT_TARGET_IMPORT => draw_home_backdrop(backend, prev_sel, prev_scroll),
                SORT_TARGET_FP => draw_fp_page(backend, prev_sel, prev_scroll, false),
                _ => draw_files_page(backend, prev_sel, prev_scroll, None, false),
            }
            backend.set_ui_modal_scale(if modal_active { modal_scale } else { 1.0 });
            // Each criterion with its two orders as words, natural first, like
            // the Play tab's picker; the one flag turns them all.
            let options: std::vec::Vec<crate::backend::render::SortOption> = remote_sort_options(kind)
                .into_iter()
                .map(|(icon, label, orders)| crate::backend::render::SortOption { icon, label, orders })
                .collect();
            // The order in force is ticked on every list now: the file list and
            // the Flashpoint results keep theirs until they are fetched again.
            let active = LIBRARY
                .lock()
                .map(|s| match kind {
                    SORT_TARGET_FP => s.fp_sort.0 as usize,
                    SORT_TARGET_IMPORT => import_sort_index(),
                    _ => s.files_sort.0 as usize,
                })
                .unwrap_or(0);
            let dirs = if reverse { u8::MAX } else { 0 };
            backend.draw_sort_window(crate::loc::s().sort_title, None, &options, selection, Some(active), dirs, None);
        }
        // ── Phase 3.7 DISTANT mode ─────────────────────────────────────
        Screen::DistantIdle { selection, scroll_offset } => {
            // Remember where the list is (and the row) so every "back to the
            // list" path, and the tab itself, can restore them.
            IMPORT_SCROLL.store(scroll_offset, std::sync::atomic::Ordering::Relaxed);
            IMPORT_ROW.store(selection, std::sync::atomic::Ordering::Relaxed);
            // Back from another page (an archive folding into its row, the
            // results, an error): the glide state is that page's, so the pill
            // and the scroll snap to the home's instead of flying in from it.
            let gap = now.saturating_sub(HOME_TICK.swap(now, std::sync::atomic::Ordering::Relaxed));
            if gap > unsafe { ruffle_tick_freq() } / 20 {
                crate::backend::render::gallery_anim_reset();
            }
            draw_importer_home(backend, selection, scroll_offset, true);
        }
        // A source's panel slides on every pop of its own. It borrowed the game
        // options' `family_slide`, whose "slide in" flag only a game's panels
        // reset, so after a game's controls editor it came in at full
        // position in one frame. Its question pops nothing (same kind), so
        // this never replays the slide under it.
        Screen::DistantUrlOptions { url_idx, selection } => {
            let slide = if closing || modal_active { Some(modal_scale) } else { None };
            draw_source_options(backend, url_idx, selection, slide, None, family_kind);
        }
        Screen::DistantHistoryConfirm { selection } => {
            let url_idx = LIBRARY.lock().map(|s| s.history_idx.unwrap_or(0)).unwrap_or(0);
            let slide = if closing || modal_active { Some(modal_scale) } else { None };
            draw_source_options(backend, url_idx, 2, slide, Some(selection), family_kind);
        }
        Screen::ImportNotAddress { selection } => {
            use crate::backend::render::{Choice, Icon};
            backend.clear_ui_transform();
            draw_home_backdrop(backend, 1, IMPORT_SCROLL.load(std::sync::atomic::Ordering::Relaxed));
            backend.set_ui_modal_scale(if modal_active { modal_scale } else { 1.0 });
            let u = crate::loc_ui::ui();
            let typed = LIBRARY.lock().map(|s| s.import_typed.clone()).unwrap_or_default();
            let body = u.imp_not_address_body.replacen("{}", &crate::backend::render::imp_quote(&typed), 1);
            backend.draw_confirm(
                Icon::Search,
                u.imp_not_address,
                &body,
                None,
                &[
                    Choice { label: u.edit, destructive: false },
                    Choice { label: u.hint_search, destructive: false },
                ],
                selection,
                false,
            );
        }
        Screen::DistantLoading => {
            // Async metadata fetch in flight: the window opens from the launched
            // row onto the page that will show the games, its header already
            // there and a spinner where the list will be, the importer behind.
            // Poll the fetch each frame and switch to DistantFiles on success.
            let title = LIBRARY.lock().map(|s| importer_label(&s.pending_fetch_url)).unwrap_or_default();
            let step = crate::backend::render::distant_reveal_step(now);
            let frac = step.map(|(f, _, _)| f).unwrap_or(1.0);
            let collapsing = step.map_or(false, |(_, c, _)| c);
            let folded = step.map_or(false, |(_, c, d)| c && d);
            let (vw, vh) = backend.screen_size();
            let src = crate::backend::render::distant_reveal_source_sel();
            let src_scroll = crate::backend::render::distant_reveal_source_scroll();
            draw_home_backdrop(backend, src, src_scroll);
            let (wx, wy, ww, wh) = reveal_window(distant_row_rect(src, src_scroll, vw), vw, vh, frac);
            backend.set_clip(wx, wy, ww, wh);
            let u = crate::loc_ui::ui();
            backend.draw_import_loading(&[u.tab_import], &title, u.imp_reading_list, now);
            backend.clear_clip();
            backend.draw_reveal_chrome(wx, wy, ww, wh);
            if folded {
                // B cancelled: back on the row the window folded into.
                if let Ok(mut s) = LIBRARY.lock() {
                    s.screen = distant_idle_at(&s, src);
                }
            } else if !collapsing {
                poll_archive_fetch();
            }
        }
        Screen::DistantFiles { selection, scroll_offset } => {
            // Expand/collapse reveal (v1.2.0): while a reveal runs, draw the
            // IMPORTER list underneath and the file list clipped to a window that
            // opens from / closes to the launched URL's row.
            if let Some((frac, collapsing, done)) = crate::backend::render::distant_reveal_step(now) {
                let (vw, vh) = backend.screen_size();
                let src = crate::backend::render::distant_reveal_source_sel();
                let src_scroll = crate::backend::render::distant_reveal_source_scroll();
                draw_home_backdrop(backend, src, src_scroll);
                let window = reveal_window(distant_row_rect(src, src_scroll, vw), vw, vh, frac);
                let (wx, wy, ww, wh) = window;
                backend.set_clip(wx, wy, ww, wh);
                draw_files_page(backend, selection, scroll_offset, Some(window), false);
                backend.clear_clip();
                // Make the opening/closing rectangle visible (same navy behind):
                // dim outside + bright border.
                backend.draw_reveal_chrome(wx, wy, ww, wh);
                if done && collapsing {
                    if let Ok(mut s) = LIBRARY.lock() {
                        s.remote_files.clear();
                        s.distant_filter = None;
                        // `src` is the ROW the window collapsed to; `distant_idle_at`
                        // clamps it to the rows that exist.
                        s.screen = distant_idle_at(&s, src);
                    }
                }
            } else {
                draw_files_page(backend, selection, scroll_offset, None, true);
            }
        }
        Screen::DistantDownloading => {
            drive_download(backend, true);
        }
        Screen::DistantCancelConfirm { selection } => {
            use crate::backend::render::{Choice, Icon};
            backend.clear_ui_transform();
            // The download carries on under the question, except while the
            // question scales out: a stop or a carry-on is on its way then.
            drive_download(backend, !closing);
            // The download may have ended this very frame, taking the question
            // with it: a finished game or an error page replaces it. Ending
            // into the game's other files takes it too. Those finish the import
            // and B waits for them, and a stop now would leave the game's
            // .swf on the card without its library entry, sidecars or cover.
            if let Ok(mut s) = LIBRARY.lock() {
                if s.dl_companion_active && matches!(s.screen, Screen::DistantCancelConfirm { .. }) {
                    s.screen = Screen::DistantDownloading;
                }
            }
            if matches!(LIBRARY.lock().map(|s| s.screen), Ok(Screen::DistantCancelConfirm { .. })) {
                backend.set_ui_modal_scale(if modal_active { modal_scale } else { 1.0 });
                let u = crate::loc_ui::ui();
                let (done, total) = net::download_progress();
                let amount = if total > 0 {
                    crate::backend::render::imp_n_of(
                        &crate::backend::render::imp_amount(done),
                        &crate::backend::render::imp_size(total),
                    )
                } else {
                    crate::backend::render::imp_amount(done)
                };
                let body = u.imp_stop_body.replacen("{}", &amount, 1);
                backend.draw_confirm(
                    Icon::Download,
                    u.imp_stop_title,
                    &body,
                    None,
                    &[
                        Choice { label: u.imp_keep_going, destructive: false },
                        Choice { label: u.imp_stop, destructive: true },
                    ],
                    selection,
                    true,
                );
            }
        }
        Screen::DistantError => {
            // Safety net for the reveal trap fixed in `import_error`: a notice
            // nobody can dismiss is the worst failure mode there is, so make
            // sure nothing is left suspending input while it's up. Idempotent.
            crate::backend::render::distant_reveal_cancel();
            draw_error_page(backend);
        }
    }

    // ── Navbar (v1.2.0) ──────────────────────────────────────────────────
    // Drawn last so the tab strip sits on top of every tab-home screen. L/R
    // switches tabs (see `input()`); sub-screens (`screen_tab` == None) show
    // no navbar. Reset the transform first so the navbar itself never moves.
    backend.clear_ui_transform();
    if let Some(tab) = screen_tab(screen) {
        // The author credit and the version are in every page's footer band
        // (`kit_footer_band`), so the corner badge is gone.
        let logo = LIBRARY
            .lock()
            .ok()
            .map(|s| (s.banner_tex, s.banner_w, s.banner_h))
            .unwrap_or_default();
        backend.draw_navbar(tab.index(), logo);
    } else {
        // No strip on a sub-screen, so nothing on it may be tapped. Without
        // this the cells published by the last home screen stayed live, and a
        // finger in the top 38 px of a modal switched tabs out from under it.
        crate::backend::render::navbar_clear();
    }

    // ── Transient toast (#20) ────────────────────────────────────────────
    // Drawn on top of everything (incl. the navbar) and counted down here, so a
    // share/apply/revert gives quick feedback without a blocking "thanks" screen.
    let toast = LIBRARY.lock().ok().and_then(|mut s| {
        if s.toast_frames > 0 {
            s.toast_frames -= 1;
            Some((s.toast_msg.clone(), s.toast_kind))
        } else {
            None
        }
    });
    if let Some((msg, kind)) = toast {
        // Over the library, left of a game's options and its page, rather than
        // on top of them.
        match options_panel::panels_left_edge(screen, backend.screen_size().0) {
            Some(right) => backend.draw_toast_left_of(&msg, kind, right),
            None => backend.draw_toast(&msg, kind),
        }
    }

    // First-boot housekeeping: a bordered "Optimisation" modal composited over
    // the dimmed gallery while the deferred migration runs. Drawn last so it sits
    // on top; the flag is set by `maybe_arm_startup_migrations` and cleared once
    // `RunStartupMigrations` finishes.
    if OPTIMIZING.load(std::sync::atomic::Ordering::Relaxed) {
        backend.draw_optimizing_modal(crate::loc::s().optimizing, now);
    }
}

/// Read a downloaded Flashpoint GameZIP at `zip_path`, extract its `.swf` to
/// `swf_path`, and add it to the local library. Returns false on any
/// read/parse/write failure (caller then shows `err_no_swf`).
/// Extract the GameZIP's main `.swf` to `swf_path` and return its ZIP entry
/// name (used to build the companion htdocs base) + the SWF bytes (scanned for
/// which companions to pull). Companion fetch + library add happen afterwards,
/// incrementally, so they show on the download progress bar. None on failure.
fn extract_gamezip_main(
    zip_path: &str,
    swf_path: &str,
    launch_command: &str,
    backend: &mut SwitchRenderBackend,
) -> Option<(std::string::String, std::vec::Vec<u8>)> {
    // No whole-archive read: `extract_gamezip_tree` now STREAMS the ZIP straight
    // off the SD card, one entry at a time (see its docs), so a multi-GB GameZIP
    // (Super Smash Flash 2 ~3.4 GB) extracts within the ~3.2 GB heap instead of
    // being refused by the old 512 MB in-RAM cap.
    // Mirror the GameZIP's full `content/<host>/<path>` tree into the game's
    // sidecar dir, so the SidecarNavigator can serve every bundled asset (alt SWF
    // versions, ad-network stubs, xml/png) by its original URL at play time. The
    // returned SWF is the launchCommand's entry (or the first .swf) — we write it
    // flat as the library entry.
    let files_dir = crate::sidecar_dir_for(Some(swf_path))
        .to_string_lossy()
        .into_owned();
    // Don't pre-create `<game>.files/`: `write_tree_file` creates parent dirs on
    // demand, so a single-SWF game (whose entry is now skipped from the tree)
    // leaves NO empty `.files/` folder behind.
    // Draw the extraction's own frames while it runs. It blocks the frame loop
    // for as long as the download took on a big game, and the screen used to sit
    // on a full download bar the whole time, which is indistinguishable from a
    // freeze (#117: "not downloading, gets stuck at 3.9G" — it had downloaded).
    //
    // Throttled on WALL CLOCK, not on entries: `gl_context_swap` waits for
    // vsync, so drawing on every one of Super Smash Flash 2's 1474 entries would
    // spend 24 seconds waiting for the display. Ten frames a second is smooth to
    // read and costs well under 1 % of an extraction measured in minutes.
    let freq = unsafe { ruffle_tick_freq() };
    let period = if freq == 0 { u64::MAX } else { freq / 10 };
    let mut last_draw: u64 = 0;
    let mut draw_progress = |done: u64, total: u64| {
        let now = unsafe { ruffle_tick_now() };
        if now.saturating_sub(last_draw) < period {
            return;
        }
        last_draw = now;
        // The import page, its extraction step active: one page for the whole
        // import, where this used to borrow the library mover's screen, titled
        // with the games folder's path.
        let frac = (total > 0).then(|| (done as f64 / total as f64).clamp(0.0, 1.0) as f32);
        let detail = if total > 0 {
            let pct = (done as f64 / total as f64 * 100.0).clamp(0.0, 100.0) as u32;
            std::format!(
                "{} %  \u{00B7}  {}",
                pct,
                crate::backend::render::imp_n_of(
                    &crate::backend::render::imp_amount(done),
                    &crate::backend::render::imp_size(total)
                )
            )
        } else {
            std::string::String::new()
        };
        show_import_step(backend, ImportPhase::Extract, frac, &detail);
    };
    let (swf, entry_name) =
        match crate::sources::gamezip::extract_gamezip_tree(
            zip_path,
            &files_dir,
            launch_command,
            &mut draw_progress,
        ) {
            Some(v) => v,
            None => {
                // No SWF produced (empty/corrupt zip, or every entry over the
                // per-entry cap). Wipe the partial tree so a failed multi-GB
                // extraction doesn't strand gigabytes on the SD card.
                let _ = std::fs::remove_dir_all(&files_dir);
                return None;
            }
        };
    if std::fs::write(swf_path, &swf).is_err() {
        log("library: gamezip .swf write failed\n");
        return None;
    }
    crate::sd::commit();
    log(&std::format!(
        "library: gamezip extracted -> {} ({} bytes); full tree -> {}\n",
        swf_path,
        swf.len(),
        files_dir,
    ));
    Some((entry_name, swf))
}

/// Pop the next companion name and start its download into `<files-dir>/<name>`
/// via the normal `https_download_*` path (so it shows on the progress bar).
/// Returns true if a download started, false when the queue is empty (the
/// companion phase is then done). Names that fail to start are skipped.
fn start_next_companion() -> bool {
    loop {
        let (name, base, dir) = {
            let mut g = match LIBRARY.lock() {
                Ok(g) => g,
                Err(_) => return false,
            };
            match g.dl_companion_queue.pop() {
                Some(n) => (n, g.dl_companion_base.clone(), g.dl_companion_dir.clone()),
                None => return false,
            }
        };
        let url = std::format!("{}{}", base, name);
        let out = std::format!("{}/{}", dir, name);
        match net::start_download(&url, &out) {
            Ok(()) => {
                if let Ok(mut g) = LIBRARY.lock() {
                    g.dl_companion_current = name;
                }
                return true;
            }
            Err(e) => {
                log(&std::format!("library: companion start failed {} ({})\n", url, e));
                // skip this one, try the next
            }
        }
    }
}

/// A companion finished downloading: read it back, scan it for further
/// companions (BFS — e.g. maingame.swf pulls console.swf/endgame.swf), queue
/// the new ones, then start the next download or finalize when the queue dries.
fn companion_download_finished(backend: &mut SwitchRenderBackend) {
    let (dir, current) = match LIBRARY.lock() {
        Ok(g) => (g.dl_companion_dir.clone(), g.dl_companion_current.clone()),
        Err(_) => return,
    };
    let path = std::format!("{}/{}", dir, current);
    if let Some(bytes) = crate::sources::gamezip::read_file_bounded(&path, 16 * 1024 * 1024) {
        let is_swf = bytes.len() > 8 && {
            let sig = &bytes[0..3];
            sig == b"FWS" || sig == b"CWS" || sig == b"ZWS"
        };
        if is_swf {
            let more = crate::sources::gamezip::scan_swf_siblings(&bytes);
            if let Ok(mut s) = LIBRARY.lock() {
                for n in more {
                    let lower = n.to_ascii_lowercase();
                    if !s.dl_companion_seen.iter().any(|x| *x == lower) {
                        s.dl_companion_seen.push(lower);
                        s.dl_companion_queue.push(n);
                    }
                }
            }
        } else {
            // The queue only ever holds `.swf` names, so a body that is not a SWF
            // is an error page or a redirect landing page. Left in place it would
            // sit in the sidecar tree under the exact name the game asks for, and
            // the navigator would serve it as the real thing — satisfying the
            // request with junk instead of missing it and falling through to the
            // mirror, which is the one path that could still repair it.
            log(&std::format!(
                "library: companion {} is not a SWF (first bytes {:02X?}) - removed\n",
                current,
                &bytes[..bytes.len().min(4)],
            ));
            let _ = std::fs::remove_file(&path);
        }
    } else {
        // Over the 16 MB scan cap, or unreadable. The file itself may be perfectly
        // good, so it stays; only the search for ITS companions is skipped, and a
        // game needing a second-level companion would then wait on it in silence.
        log(&std::format!(
            "library: companion {} not read back - its own companions were not searched\n",
            current,
        ));
    }
    if let Ok(mut s) = LIBRARY.lock() {
        s.dl_companion_done += 1;
    }
    crate::sd::commit();
    if !start_next_companion() {
        if let Ok(mut s) = LIBRARY.lock() {
            s.dl_companion_active = false;
        }
        finalize_gamezip_download(backend);
    }
}

/// Finish a Flashpoint game download once its main SWF (and any companions) are
/// on the SD card: add it to the local library, write the source-URL sidecar,
/// auto-fetch the cover, restore the real title, and return to the FpGallery.
/// Reads the `download_*` state (still populated until here). Shared by the
/// no-companion path and the companion-phase completion.
fn finalize_gamezip_download(backend: &mut SwitchRenderBackend) {
    let (swf_path, file_name, cover_url, real_title, source_url, launch_command) = match LIBRARY.lock() {
        Ok(g) => (
            g.download_zip_extract.clone().unwrap_or_default(),
            g.download_file_name.clone(),
            g.download_cover_url.clone(),
            g.download_title.clone(),
            g.download_source_url.clone(),
            g.download_launch_command.clone(),
        ),
        Err(_) => return,
    };
    if swf_path.is_empty() {
        return;
    }
    // Cache the game's REAL on-SD footprint (flat SWF + companion `.files/` tree)
    // in a `<swf>.filesize` sidecar so the footer shows GBs, not the 2 MB loader.
    // Done once here (the tree + companions are complete) and BEFORE add_path so
    // the new entry picks it up right away. The scan only reads the cached number.
    cache_game_footprint(&swf_path);
    // Kept, because this is what decides whether the game EXISTS for the user.
    // `add_or_replace_path` returns false when the SWF header will not parse —
    // a truncated extraction, or an entry that only looks like a SWF — and the
    // entry is then never created. Discarding it meant the download screen
    // closed, a green "downloaded" toast appeared, and the game was simply
    // absent from JOUER with no explanation available anywhere.
    let added = add_or_replace_path(&swf_path);
    if !added {
        log(&std::format!(
            "library: {} extracted but its SWF header will not parse - no entry created\n",
            swf_path,
        ));
    }
    // Record where this game came from (Flashpoint GameZIP URL) next to the
    // extracted .swf, for later bug-report attribution.
    write_url_sidecar(&swf_path, &source_url);
    // Record the original launchCommand URL in a `<game>.swf.base` sidecar. At
    // launch, lib.rs uses it as the movie's base URL so the game's relative
    // loads (configuration.xml, data/*.xml, assets/**/*.swf) resolve to the
    // host-pathed `.files/<host>/<path>` tree the SidecarNavigator serves.
    // Only meaningful for Flashpoint GameZIPs (host-pathed); direct/single-file
    // imports leave it absent and keep the flat synthetic base URL.
    // Unquoted FIRST. Flashpoint quotes any launchCommand containing a space, so
    // the raw string starts with a quote and this test failed on every one of them
    // — silently. The game then booted with the synthetic base URL, which breaks
    // its relative asset loads AND disables the on-demand mirror repair that would
    // have covered the miss. A deterministic trigger, not a flaky one.
    let launch_command = crate::sources::gamezip::unquote(&launch_command).to_string();
    if launch_command.starts_with("http://") || launch_command.starts_with("https://") {
        let base_path = std::format!("{}.base", swf_path);
        if let Err(e) = std::fs::write(&base_path, launch_command.as_bytes()) {
            log(&std::format!("library: base sidecar write failed: {}\n", e));
        } else {
            crate::sd::commit();
            log(&std::format!("library: base sidecar -> {}\n", base_path));
        }
    } else if !launch_command.is_empty() {
        // Says so rather than leaving the absence to be inferred from silence.
        log(&std::format!(
            "library: no base sidecar, launchCommand is not an http URL: {}\n",
            launch_command,
        ));
    }
    // Auto-fetch the game's cover (logo) so JOUER shows its art right away — no
    // manual "Jaquette" step. Synchronous HTTPS, best-effort, so the import page
    // shows that step first.
    if let Some(url) = &cover_url {
        if !file_name.is_empty() {
            show_import_step(backend, ImportPhase::Cover, None, "");
            match crate::covers::fetch_url_and_cache(&file_name, url) {
                Ok(p) => {
                    log(&std::format!("library: auto-cover cached -> {}\n", p));
                    crate::backend::render::invalidate_cover(&file_name);
                }
                Err(e) => log(&std::format!("library: auto-cover failed: {}\n", e)),
            }
        }
    }
    // Persist the REAL Flashpoint title as the display name.
    let stem = file_name.strip_suffix(".swf").unwrap_or(&file_name);
    let restored_title = real_title
        .as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty() && *t != stem);
    // Toast label, owned up-front: `file_name` (which `stem` borrows) is moved
    // into `downloaded_basenames` below.
    let shown = restored_title.unwrap_or(stem).to_string();
    if let (Some(title), false) = (restored_title, file_name.is_empty()) {
        if write_meta_sidecar(&file_name, title) {
            log(&std::format!("library: saved real title \"{}\" for {}\n", title, file_name));
        }
    }
    if let Ok(mut s) = LIBRARY.lock() {
        s.download_file_name.clear();
        s.download_out_path.clear();
        s.download_zip_extract = None;
        s.download_fp_direct = false;
        s.download_cover_url = None;
        s.download_title = None;
        if let Some(title) = restored_title {
            if let Some(e) = s.entries.iter_mut().find(|e| e.basename == file_name) {
                e.display_name = title.to_string();
            }
        }
        // Only mark it downloaded (the gallery's OK badge) if it actually became
        // a playable entry.
        if added && !file_name.is_empty() && !s.downloaded_basenames.iter().any(|n| n == &file_name)
        {
            s.downloaded_basenames.push(file_name);
        }
        // Back to the Flashpoint gallery so the user can grab another game.
        let (sel, scroll) = s.download_resume_pos.take().unwrap_or((0, 0));
        let sel = sel.min(s.cover_candidates.len().saturating_sub(1));
        s.screen = Screen::FpGallery { selection: sel, scroll };
        // The gallery looks identical after a download (bar the OK badge), so
        // confirm it landed — the download screen vanishing isn't a confirmation.
        if added {
            set_toast(
                &mut s,
                crate::loc::s().toast_dl_ok.replace("{}", &shown),
                TOAST_OK,
            );
        } else {
            set_toast(&mut s, crate::loc::s().err_dl_not_a_game.to_string(), TOAST_ERR);
        }
    }
}

/// Called from `render()` after `tick_download` returns Ok(true). Adds
/// the downloaded file to the local entries list (so it's playable when
/// the user goes back to LOCAL) and returns to the DistantFiles screen
/// so the user can keep picking other files from the same archive.org
/// item without re-typing the URL. The just-downloaded basename is
/// tracked in `downloaded_basenames` so the list shows a `✓` next to it.
fn on_download_finished(backend: &mut SwitchRenderBackend) {
    // `cover_url` / `real_title` are read from state inside
    // `finalize_gamezip_download` (also called from the companion phase), so we
    // only pull what this function uses directly here.
    let (out_path, file_name, zip_extract, source_url, launch_command, fp_direct) = match LIBRARY.lock() {
        Ok(g) => (
            g.download_out_path.clone(),
            g.download_file_name.clone(),
            g.download_zip_extract.clone(),
            g.download_source_url.clone(),
            g.download_launch_command.clone(),
            g.download_fp_direct,
        ),
        Err(_) => return,
    };
    if out_path.is_empty() {
        return;
    }
    log(&std::format!("library: download finished -> {}\n", out_path));

    // HTTP 200 does not mean we got the file we asked for. An URL fragment cuts
    // the request short and archive.org answers with its HTML listing page; a dead
    // mirror answers with an error page; a redirect can land on something else
    // entirely. All of those used to be written under the game's name and
    // announced as a success, then dropped in silence at the next library scan
    // ("failed to parse SWF header ... skipping"), leaving a ghost file on the SD
    // card and no way for the user to know what went wrong (issue #73, where the
    // same URL was retried four times with the same mute result). Check the magic
    // bytes here, while we still know which URL to blame.
    // `download_zip_extract` is Some on BOTH Flashpoint branches: for a zipped
    // game it is where the temp .zip will extract to, but for a non-zipped one it
    // IS the .swf being downloaded straight from the htdocs mirror (see the
    // comment where it is set). Deciding the expected magic from it alone tested
    // every legacy Flashpoint game's valid FWS/CWS against PK\x03\x04, deleted the
    // good file and reported a bad address — the whole loose-import path, killed
    // by the check meant to protect it. `fp_direct` is the flag that separates them.
    let expect_zip = zip_extract.is_some() && !fp_direct;
    let head = file_head(&out_path, 4);
    let looks_right = if expect_zip {
        head.starts_with(b"PK\x03\x04")
    } else {
        head.starts_with(b"FWS") || head.starts_with(b"CWS") || head.starts_with(b"ZWS")
    };
    if !looks_right {
        log(&std::format!(
            "library: downloaded file is not a {} (first bytes {:02X?}) -> discarding {}\n",
            if expect_zip { "zip" } else { "swf" },
            head,
            out_path,
        ));
        let _ = std::fs::remove_file(&out_path);
        if let Ok(mut s) = LIBRARY.lock() {
            // `failed_url` still holds this URL (only a good download clears
            // it), so the error page keeps offering Y = correct it. Not "try
            // again": the same address would answer the same page.
            let name = s
                .download_title
                .clone()
                .filter(|t| !t.trim().is_empty())
                .unwrap_or_else(|| pretty_name(&file_name));
            import_error(&mut s, ErrKind::Download, crate::loc::s().err_dl_not_a_game, false, &name);
            clear_download_state(&mut s);
        }
        return;
    }

    // Non-zipped Flashpoint game: the downloaded file IS the entry `.swf`, already
    // at its final path (no zip to extract). Fetch any companion `.swf` files from
    // the same htdocs directory (flat into `<game>.files/`, where the navigator's
    // leaf-name fallback finds them), then finalize like a GameZIP (cover/title/
    // base sidecar/library add). Single-file games (no companions) finalize at once.
    if fp_direct {
        let swf_path = out_path.clone();
        let companions = crate::sources::gamezip::read_file_bounded(&swf_path, 64 * 1024 * 1024)
            .map(|b| crate::sources::gamezip::scan_swf_siblings(&b))
            .unwrap_or_default();
        // Companion base = the entry's htdocs directory (its URL minus the file).
        let base = crate::sources::gamezip::htdocs_url_from_command(&launch_command)
            .and_then(|u| u.rfind('/').map(|i| u[..=i].to_string()));
        if let (Some(base), false) = (base, companions.is_empty()) {
            let files_dir = crate::sidecar_dir_for(Some(&swf_path))
                .to_string_lossy()
                .into_owned();
            let _ = std::fs::create_dir_all(&files_dir);
            let main_name = launch_command
                .rsplit('/')
                .next()
                .unwrap_or("")
                .split(['?', '#'])
                .next()
                .unwrap_or("")
                .to_ascii_lowercase();
            if let Ok(mut s) = LIBRARY.lock() {
                s.dl_companion_base = base;
                s.dl_companion_dir = files_dir;
                s.dl_companion_current = std::string::String::new();
                s.dl_companion_done = 0;
                s.dl_companion_seen = std::vec![main_name];
                s.dl_companion_queue = std::vec::Vec::new();
                for n in companions {
                    let lower = n.to_ascii_lowercase();
                    if !s.dl_companion_seen.iter().any(|x| *x == lower) {
                        s.dl_companion_seen.push(lower);
                        s.dl_companion_queue.push(n);
                    }
                }
                s.dl_companion_active = true;
                s.import_had_files = true;
            }
            // Start the first companion; the DistantDownloading arm drives the
            // rest and finalizes when the queue dries. None started → finalize now.
            if start_next_companion() {
                return;
            }
            if let Ok(mut s) = LIBRARY.lock() {
                s.dl_companion_active = false;
            }
        }
        finalize_gamezip_download(backend);
        return;
    }

    // Flashpoint GameZIP: the downloaded file is a `.zip`; extract its `.swf`
    // to `swf_path` and add THAT (not the zip), then delete the temp zip.
    if let Some(swf_path) = zip_extract {
        let Some((entry_name, swf)) = extract_gamezip_main(&out_path, &swf_path, &launch_command, backend) else {
            let _ = std::fs::remove_file(&out_path);
            if let Ok(mut s) = LIBRARY.lock() {
                let name = s.download_title.clone().unwrap_or_else(|| pretty_name(&file_name));
                import_error(&mut s, ErrKind::Download, crate::loc_ui::ui().imp_err_fp_no_swf, false, &name);
                clear_download_state(&mut s);
            }
            return;
        };
        let _ = std::fs::remove_file(&out_path);
        // HTML-wrapped game: the launchCommand pointed at the `index.html`
        // wrapper, but extraction resolved and wrote the real entry SWF
        // (entry_name). Rewrite the stored launchCommand to that SWF's URL so
        // finalize's `.base` sidecar makes the game's relative loads
        // (game_config.xml, companion SWFs) resolve against the extracted tree.
        if !crate::sources::gamezip::launch_command_is_swf(&launch_command) {
            if let Some(u) = crate::sources::gamezip::entry_url_from_name(&entry_name) {
                if let Ok(mut s) = LIBRARY.lock() {
                    s.download_launch_command = u;
                }
            }
        }
        // Multi-file game: queue its companion SWFs so they download (on the same
        // progress bar) before we finalize, so they're present when the user
        // launches it. No companions → finalize straight away.
        let base = crate::sources::gamezip::htdocs_base_from_entry(&entry_name);
        let companions = crate::sources::gamezip::scan_swf_siblings(&swf);
        if let (Some(base), false) = (base, companions.is_empty()) {
            let files_dir = crate::sidecar_dir_for(Some(&swf_path))
                .to_string_lossy()
                .into_owned();
            let _ = std::fs::create_dir_all(&files_dir);
            let main_name = entry_name.rsplit('/').next().unwrap_or("").to_ascii_lowercase();
            if let Ok(mut s) = LIBRARY.lock() {
                s.dl_companion_base = base;
                s.dl_companion_dir = files_dir;
                s.dl_companion_current = std::string::String::new();
                s.dl_companion_done = 0;
                s.dl_companion_seen = std::vec![main_name];
                s.dl_companion_queue = std::vec::Vec::new();
                for n in companions {
                    let lower = n.to_ascii_lowercase();
                    if !s.dl_companion_seen.iter().any(|x| *x == lower) {
                        s.dl_companion_seen.push(lower);
                        s.dl_companion_queue.push(n);
                    }
                }
                s.dl_companion_active = true;
                s.import_had_files = true;
            }
            // Start the first companion; the DistantDownloading arm drives the
            // rest and finalizes when the queue dries. If none actually started,
            // fall through to finalize now.
            if start_next_companion() {
                return;
            }
            if let Ok(mut s) = LIBRARY.lock() {
                s.dl_companion_active = false;
            }
        }
        finalize_gamezip_download(backend);
        return;
    }

    // Add to the LOCAL entries list (so when the user backs out of
    // DISTANT mode, the file appears in the LOCAL library). The magic bytes were
    // checked above, so a failure here means a SWF that starts right and then
    // stops — truncated mid-transfer, most likely. Reported below rather than
    // dropped: the file is on the card either way, so "it downloaded" and "you
    // can play it" are not the same claim.
    // The `.url` sidecar FIRST, then the entry. `add_path` reads that sidecar to
    // fill `source_item`, so adding before writing left the new entry with an
    // empty source: nothing could say which archive.org item the file came from
    // until the next full scan, and the file list had to fall back on matching
    // names to decide what you already had.
    write_url_sidecar(&out_path, &source_url);
    let added = add_or_replace_path(&out_path);
    if !added {
        log(&std::format!(
            "library: {} downloaded but its SWF header will not parse - no entry created\n",
            out_path,
        ));
    }
    // Keep the IMPORTER row's "4 / 81" honest between two openings of the item.
    // It is recomputed from the file list whenever that list is fetched, but the
    // user can download several files in a row without leaving the screen, and
    // the row would otherwise show the figure from before any of them.
    if added {
        bump_url_have(&source_url);
    }
    // A `.swf` from archive.org is not always the whole game. When its levels or
    // config live in a companion data file, the game loads that file at startup
    // and, if it is missing, does not fail — it WAITS: BFDIA 5b (issue #73) sits
    // on a green background at a steady 60 fps, its one `levels.txt` request
    // having failed back at frame 13, with nothing on screen to say so. Those
    // files are almost always in the same item as the `.swf`, so fetch the ones
    // this SWF actually names. Only referenced names: never the whole item, which
    // is mostly archive.org's own bookkeeping (`*_meta.xml`, `*_files.xml`,
    // torrents, thumbnails). Synchronous like the auto-cover below — these are
    // small data files. Anything referenced but absent is collected so the user
    // is TOLD, instead of discovering it as a frozen game later.
    let mut missing_assets: std::vec::Vec<std::string::String> = std::vec::Vec::new();
    if let Some(item) = crate::net::extract_item_id(&source_url) {
        let item_files = crate::net::archive_item_files(&item);
        let refs = if item_files.is_empty() {
            std::vec::Vec::new()
        } else {
            crate::sources::gamezip::read_file_bounded(&out_path, 64 * 1024 * 1024)
                .map(|b| crate::sources::gamezip::scan_swf_assets(&b))
                .unwrap_or_default()
        };
        if !refs.is_empty() {
            let files_dir = crate::sidecar_dir_for(Some(&out_path))
                .to_string_lossy()
                .into_owned();
            let mut got = 0usize;
            if let Ok(mut s) = LIBRARY.lock() {
                s.import_had_data = true;
            }
            let count = refs.len();
            for (i, name) in refs.into_iter().enumerate() {
                // Each file is a blocking request: the page says which.
                let n = crate::backend::render::imp_n_of(&(i + 1).to_string(), &count.to_string());
                show_import_step(
                    backend,
                    ImportPhase::Data,
                    Some(i as f32 / count as f32),
                    &std::format!("{}  \u{00B7}  {}", n, name),
                );
                // Match the item's real spelling (case can differ) — the URL must
                // use the name archive.org stores, the sidecar file too.
                let Some(actual) = item_files.iter().find(|f| f.eq_ignore_ascii_case(&name)) else {
                    log(&std::format!("library: asset {} referenced but not in the item\n", name));
                    missing_assets.push(name);
                    continue;
                };
                let url = std::format!("https://archive.org/download/{}/{}", item, actual);
                match crate::net::http_get(&url, 8 * 1024 * 1024) {
                    Ok(bytes) => {
                        let _ = std::fs::create_dir_all(&files_dir);
                        let dest = std::format!("{}/{}", files_dir, actual);
                        if std::fs::write(&dest, &bytes).is_ok() {
                            got += 1;
                            // The SD scan already ran; without this the launcher
                            // would not know the file exists.
                            note_file_created(&dest);
                            log(&std::format!(
                                "library: asset {} ({} bytes) -> {}\n", actual, bytes.len(), dest
                            ));
                        } else {
                            log(&std::format!("library: asset write failed {}\n", dest));
                            missing_assets.push(name);
                        }
                    }
                    Err(e) => {
                        log(&std::format!("library: asset fetch {} failed: {}\n", url, e));
                        missing_assets.push(name);
                    }
                }
            }
            if got > 0 {
                crate::sd::commit();
            }
        }
    }

    // Best-effort auto-cover for archive.org / direct imports (issue #33): unlike
    // Flashpoint downloads (which carry an exact cover_url), these have no cover,
    // so search Flashpoint by name and cache the logo ONLY when the search returns
    // EXACTLY ONE match -> a confident hit. 0 or several -> skip silently (the
    // manual OPTIONS > JAQUETTE picker stays available). Concatenated filenames
    // like "cactusmccoy_v2_1" usually return 0 and are left alone. Runs here with
    // NO lock held (synchronous HTTPS, same as the manual cover flow); gated on
    // the online-covers toggle. The cache key is the saved file's basename so it
    // matches the library entry (download_file_name can differ after sanitizing).
    if crate::loc::covers_online() {
        let basename = std::path::Path::new(&out_path)
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let query = cover_query_from_name(&basename);
        if !basename.is_empty() && !query.is_empty() {
            show_import_step(backend, ImportPhase::Cover, None, "");
            match crate::sources::flashpoint::search(&query) {
                Ok(cands) if cands.len() == 1 => {
                    let url = cands[0].cover_url.clone();
                    match crate::covers::fetch_url_and_cache(&basename, &url) {
                        Ok(p) => {
                            log(&std::format!("library: auto-cover (single match) cached -> {}\n", p));
                            crate::backend::render::invalidate_cover(&basename);
                        }
                        Err(e) => log(&std::format!("library: auto-cover fetch failed: {}\n", e)),
                    }
                }
                Ok(cands) => log(&std::format!(
                    "library: auto-cover skipped ({} match(es) for \"{}\")\n", cands.len(), query
                )),
                Err(e) => log(&std::format!("library: auto-cover search failed: {}\n", e)),
            }
        }
    }
    // Toast label, owned before `file_name` is moved into the session list.
    let shown = file_name
        .strip_suffix(".swf")
        .unwrap_or(&file_name)
        .to_string();
    if let Ok(mut s) = LIBRARY.lock() {
        s.download_file_name.clear();
        s.download_out_path.clear();
        // The URL downloaded fine, so there's nothing left to "fix" on it.
        s.failed_url.clear();
        if added && !file_name.is_empty() && !s.downloaded_basenames.iter().any(|n| n == &file_name)
        {
            s.downloaded_basenames.push(file_name);
        }
        if !added {
            set_toast(&mut s, crate::loc::s().err_dl_not_a_game.to_string(), TOAST_ERR);
        } else if !missing_assets.is_empty() {
            // It downloaded, but it will ask at startup for files nobody has, and
            // then wait on them without a word. Name them now: a frozen game
            // twenty seconds into a launch is not a diagnosis a player can make.
            set_toast(
                &mut s,
                crate::loc::s()
                    .toast_assets_missing
                    .replace("{}", &missing_assets.join(", ")),
                TOAST_ERR,
            );
        } else if !shown.is_empty() {
            set_toast(
                &mut s,
                crate::loc::s().toast_dl_ok.replace("{}", &shown),
                TOAST_OK,
            );
        }
        // Return to DistantFiles, NOT LOCAL — the user almost certainly
        // wants to pick more files from the same item. To go back to
        // LOCAL they hit Y or B from the DistantFiles screen.
        // Restore the (selection, scroll) we snapshotted at A-press so
        // the cursor lands on the same row the user just downloaded
        // (handy when stepping through a long list).
        let (sel, scroll) = s.download_resume_pos.take().unwrap_or((0, 0));
        if s.remote_files.is_empty() {
            // Direct `.swf` import (no metadata list): back to the importer, on
            // the address's own row, where A now plays it.
            let row = s
                .url_history
                .iter()
                .position(|u| *u == source_url || crate::sources::wayback_raw(u) == source_url)
                .map(|abs| importer_row_for_abs(&s, abs))
                .unwrap_or(s.import_return_row);
            s.screen = distant_idle_at(&s, row);
        } else {
            // Defensive clamp: if filter changed mid-download (it can't via
            // input lock, but if upstream code ever frees the lock) keep the
            // selection in range.
            let filtered_len = filtered_indices(&s.remote_files, &s.distant_filter).len();
            let sel = sel.min(filtered_len.saturating_sub(1));
            let scroll = clamp_scroll(scroll, sel, DISTANT_VISIBLE_ROWS);
            s.screen = Screen::DistantFiles { selection: sel, scroll_offset: scroll };
        }
    }
}

/// `add_path` with dedupe-by-path: if the basename is already known,
/// REPLACE the entry rather than push a duplicate. Used by the download
/// completion path so re-downloading an existing file refreshes metadata
/// instead of growing the list.
fn add_or_replace_path(path: &str) -> bool {
    // Just-downloaded file → stamp "now" so the "recent" sort floats it to the
    // top until the next full scan reads the real on-disk mtime.
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    if !add_path(path, now) {
        return false;
    }
    // The dedup that used to live here is gone: `add_path` now REPLACES an entry
    // with the same path in place (issue #68 made overlapping scan roots
    // possible), so the list can no longer hold two of one game and there is
    // nothing left to remove.
    if let Ok(mut s) = LIBRARY.lock() {
        // Re-apply the active sort so a freshly downloaded / imported game lands
        // in its sorted slot (e.g. A-Z) instead of stuck at the bottom of the
        // list. The "now" mtime stamp from `add_path` still floats it to the top
        // under the RECENT sort.
        sort_entries(&mut s.entries, current_sort_mode(), current_sort_reverse());
    }
    true
}

// ── Banner PNG decoding ───────────────────────────────────────────────────

const BANNER_PNG: &[u8] = include_bytes!("../../assets/banner.png");

/// Decode the embedded banner PNG into RGBA bytes + dims. Called once from
/// `ruffle_library_init` (after the renderer is up). On success the caller
/// uploads the bytes as a GL texture; on failure (corrupt PNG, OOM) we just
/// fall back to drawing the title via the pixel font (existing path).
pub(crate) fn decode_banner() -> Option<(std::vec::Vec<u8>, u32, u32)> {
    // png 0.18 wants BufRead + Seek — wrap the static byte slice in a
    // Cursor so std::io::Read + Seek are satisfied without extra alloc.
    let cursor = std::io::Cursor::new(BANNER_PNG);
    let decoder = png::Decoder::new(cursor);
    let mut reader = match decoder.read_info() {
        Ok(r) => r,
        Err(e) => {
            log(&std::format!("library: banner PNG decode_info failed: {:?}\n", e));
            return None;
        }
    };
    let info = reader.info().clone();
    let w = info.width;
    let h = info.height;
    let out_size = reader.output_buffer_size()?;
    let mut buf = std::vec![0u8; out_size];
    if let Err(e) = reader.next_frame(&mut buf) {
        log(&std::format!("library: banner PNG decode failed: {:?}\n", e));
        return None;
    }
    // Promote to RGBA8 if needed. assets/banner.png is RGBA per the README
    // spec, but the user might re-export as RGB or palette down the line.
    let rgba = match info.color_type {
        png::ColorType::Rgba => buf,
        png::ColorType::Rgb => {
            let mut out = std::vec::Vec::with_capacity(buf.len() / 3 * 4);
            for px in buf.chunks_exact(3) {
                out.extend_from_slice(&[px[0], px[1], px[2], 0xFF]);
            }
            out
        }
        png::ColorType::GrayscaleAlpha => {
            let mut out = std::vec::Vec::with_capacity(buf.len() * 2);
            for px in buf.chunks_exact(2) {
                out.extend_from_slice(&[px[0], px[0], px[0], px[1]]);
            }
            out
        }
        png::ColorType::Grayscale => {
            let mut out = std::vec::Vec::with_capacity(buf.len() * 4);
            for &px in &buf {
                out.extend_from_slice(&[px, px, px, 0xFF]);
            }
            out
        }
        png::ColorType::Indexed => {
            log("library: indexed-color PNG banner not supported, ignoring\n");
            return None;
        }
    };
    log(&std::format!(
        "library: banner PNG decoded {}x{} ({} bytes RGBA)\n",
        w, h, rgba.len(),
    ));
    Some((rgba, w, h))
}

/// Called by `ruffle_library_init` after `decode_banner` returned ok. Stores
/// the GL texture id + dims into the library state so `render` can pass
/// them to `draw_library_list`.
pub(crate) fn set_banner_texture(tex: u32, w: u32, h: u32) {
    if let Ok(mut s) = LIBRARY.lock() {
        s.banner_tex = tex;
        s.banner_w = w;
        s.banner_h = h;
    }
}

// ── SWF header parsing ────────────────────────────────────────────────────

struct ParsedSwfHeader {
    size_bytes: u64,
    version: u8,
    compression_label: &'static str,
    is_as3: bool,
}

/// Best-effort ActionScript-3 (AVM2) detection. The authoritative signal is the
/// `FileAttributes` tag's `ActionScript3` flag; that tag is mandatory as the
/// FIRST tag of any SWF >= 8, so we only need the first few dozen bytes of the
/// (decompressed) body — not the whole movie. `file` must be positioned right
/// after the 8-byte SWF header.
fn detect_as3(file: &mut File, version: u8, compression_label: &str) -> bool {
    // SWF < 8 predates FileAttributes → always AVM1.
    if version < 8 {
        return false;
    }
    let mut prefix = [0u8; 64];
    let got = match compression_label {
        "FWS" => fill_read(file, &mut prefix),
        "CWS" => {
            let mut z = flate2::read::ZlibDecoder::new(file);
            fill_read(&mut z, &mut prefix)
        }
        // ZWS = LZMA, only ever emitted for SWF >= 13 (the AS3 era). We don't
        // wire the LZMA prefix reader, so treat it as AS3 by version.
        _ => return true,
    };
    parse_as3_flag(&prefix[..got]).unwrap_or(version >= 9)
}

/// Read repeatedly until `buf` is full or the stream ends. Returns bytes read.
fn fill_read<R: Read>(r: &mut R, buf: &mut [u8]) -> usize {
    let mut n = 0;
    while n < buf.len() {
        match r.read(&mut buf[n..]) {
            Ok(0) => break,
            Ok(k) => n += k,
            Err(_) => break,
        }
    }
    n
}

/// Parse the decompressed SWF body prefix: skip the stage RECT + frame
/// rate/count, then read the first tag. For SWF >= 8 that's `FileAttributes`
/// (tag code 69), whose first flag byte carries `ActionScript3` at bit 3.
fn parse_as3_flag(buf: &[u8]) -> Option<bool> {
    let first = *buf.first()?;
    // RECT: top 5 bits of byte 0 = nbits, then 4 fields of nbits each.
    let nbits = (first >> 3) as usize;
    let rect_bytes = (5 + nbits * 4 + 7) / 8;
    // RECT, then frame rate (u16) + frame count (u16), then the first tag.
    let p = rect_bytes + 4;
    let (lo, hi) = (*buf.get(p)?, *buf.get(p + 1)?);
    let tag_code = u16::from_le_bytes([lo, hi]) >> 6;
    if tag_code != 69 {
        // No FileAttributes as the first tag → not AS3-flagged.
        return Some(false);
    }
    let flags = *buf.get(p + 2)?;
    Some(flags & 0x08 != 0) // ActionScript3 = bit 3.
}

fn read_swf_header(path: &str) -> Option<ParsedSwfHeader> {
    // Take size from the SWF header's `file_length` (u32 LE at bytes 4..8)
    // rather than `fs::metadata().len()` — on Horizon/newlib the latter
    // returned a bogus value (~1.6 GB for every file) that hosed the
    // library metadata panel. The SWF field is canonical anyway: for
    // compressed (CWS/ZWS) movies it's the uncompressed size.
    let mut file = File::open(path).ok()?;
    let mut buf = [0u8; 8];
    let n = file.read(&mut buf).ok()?;
    if n < 8 {
        return None;
    }
    let compression_label = match &buf[0..3] {
        b"FWS" => "FWS",
        b"CWS" => "CWS",
        b"ZWS" => "ZWS",
        _ => return None,
    };
    let version = buf[3];
    let size_bytes = u32::from_le_bytes([buf[4], buf[5], buf[6], buf[7]]) as u64;
    // `file` is now positioned just after the 8-byte header — exactly where
    // detect_as3 expects to start the (possibly compressed) body.
    let is_as3 = detect_as3(&mut file, version, compression_label);
    Some(ParsedSwfHeader {
        size_bytes,
        version,
        compression_label,
        is_as3,
    })
}

// ── Color chip from basename ──────────────────────────────────────────────

/// FNV-1a-style 32-bit hash, folded into HSV (H from hash, S/V fixed) so
/// every basename maps to a distinct vivid color. Tied to the basename
/// (not display_name) so renaming a game in 3.4.bis sidecar doesn't change
/// the chip — same physical file, same chip, less visual jank.
fn color_from_basename(basename: &str) -> u32 {
    let mut h: u32 = 2166136261;
    for &b in basename.as_bytes() {
        h ^= b as u32;
        h = h.wrapping_mul(16777619);
    }
    // Map hash → hue [0, 360), then HSV(H, 0.65, 0.95) → RGB.
    let hue = (h % 360) as f32;
    hsv_to_rgb_u32(hue, 0.65, 0.95)
}

fn hsv_to_rgb_u32(h: f32, s: f32, v: f32) -> u32 {
    let c = v * s;
    let h6 = h / 60.0;
    let x = c * (1.0 - ((h6 % 2.0) - 1.0).abs());
    let (r1, g1, b1) = match h6 as i32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = v - c;
    let r = ((r1 + m) * 255.0) as u32 & 0xFF;
    let g = ((g1 + m) * 255.0) as u32 & 0xFF;
    let b = ((b1 + m) * 255.0) as u32 & 0xFF;
    (r << 16) | (g << 8) | b
}

/// Strings this branch stopped reading. The tables in `loc.rs` and `loc_ui.rs`
/// are only edited by the merge, which deletes them; until then this keeps
/// them read, so the build stays free of warnings. Delete it with them.
#[allow(dead_code)]
fn retired_strings() -> [&'static str; 6] {
    let (lc, u) = (crate::loc::s(), crate::loc_ui::ui());
    [lc.sort_recent, lc.sort_played, lc.sort_recent_played, lc.sort_dir_asc, lc.sort_dir_desc, u.order]
}
