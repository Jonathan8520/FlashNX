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
    /// applet heap, so launching would OOM. Shows a "use title takeover"
    /// notice (P1c) instead of the embedded red SWF. Carries the list row to
    /// return to on dismiss.
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
    DeleteConfirm { game_idx: usize },
    /// Cover picker (OPTIONS > JAQUETTE, v1.2.0). Lists Flashpoint search
    /// candidates from `State::cover_candidates`; `selection` indexes that vec.
    /// `State::cover_msg` (non-empty) shows a notice instead of a list (covers
    /// off / no results / fetch error). A = fetch+cache the chosen logo.
    CoverPicker { game_idx: usize, selection: usize },
    /// Flashpoint game gallery (IMPORTER > X): a cover grid of search hits;
    /// A downloads the selected game's GameZIP. Reuses `cover_candidates` for
    /// storage and `draw_library_cover_picker` for rendering.
    FpGallery { selection: usize, scroll: usize },
    /// Details popup for the Flashpoint game at `selection` (`+` on the gallery):
    /// full title + developer/publisher/date + download size. `size` is the
    /// GameZIP's Content-Length (0 = unknown / probe failed). `scroll` restores
    /// the gallery on close.
    FpDetails { selection: usize, scroll: usize, size: u64 },
    /// JOUER sort picker (Y): centered modal — A-Z / recent / most-played / size.
    /// `prev_sel`/`prev_scroll` restore the gallery cursor when B (cancel) is hit.
    SortModal { selection: usize, prev_sel: usize, prev_scroll: usize },
    /// Sort picker for the IMPORTER lists (Y). `kind` picks the target list +
    /// option set (`SORT_TARGET_*`): archive.org files (name/size), the
    /// Flashpoint gallery (name only — developers are unknown), or the IMPORTER
    /// home's saved URLs (added/name/source). `reverse` mirrors the JOUER
    /// modal's X toggle. `prev_*` restore the cursor on cancel.
    RemoteSortModal { selection: usize, kind: u8, reverse: bool, prev_sel: usize, prev_scroll: usize },
    // ── Phase 3.7: DISTANT mode (archive.org import) ───────────────────
    /// IMPORTER home: the saved-URL list. Row 0 is the "+ add" row, rows 1..
    /// index `importer_view` (the filtered + sorted history). `scroll_offset` is
    /// the topmost visible row — a real field, not derived from `selection`, so
    /// a touch drag can scroll the list without moving the cursor. A launches
    /// the selected URL (or adds one); + opens per-URL options.
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
    /// `State::distant_error`. B or A dismisses back to DistantIdle.
    DistantError,
    /// Confirm removing a history URL (from DistantUrlOptions > delete).
    /// A = delete + persist, B/Minus = cancel.
    DistantHistoryConfirm,
    /// Per-URL options modal (Plus on a history URL): rename(edit) / delete /
    /// back. `url_idx` indexes `url_history`; `selection` indexes the options.
    DistantUrlOptions { url_idx: usize, selection: usize },
    // ── Settings modal (Plus from the library) ─────────────────────────
    /// Global settings. `selection` indexes the 3 entries: 0 = default
    /// controls, 1 = language, 2 = back.
    SettingsModal { selection: usize },
    /// Editing the GLOBAL DEFAULT keymap via the reused `menu::*` editor.
    SettingsKeymapEditor,
    /// Language picker. `selection` indexes `loc::PICKER_LANGS`.
    SettingsLanguagePicker { selection: usize },
    /// Game DEFAULTS sub-modal: 0 = display mode, 1 = screen filter, 2 = cursor
    /// speed. Each row cycles in place. These are the values a game that was
    /// never set from its own pause menu will use.
    SettingsPrefsModal { selection: usize },
    // ── Bug report (RÉGLAGES → SIGNALER UN BUG) ─────────────────────────
    /// Pick which game is broken. `selection` indexes `State::entries`,
    /// `scroll_offset` is the topmost visible row. A → describe + send,
    /// B → back to the RÉGLAGES tab.
    BugPicker { selection: usize, scroll_offset: usize },
    /// Result of a bug submission. `State::bug_ok` picks the success/failure
    /// styling, `State::bug_msg` is the (already-localized) message. A/B dismiss
    /// back to the RÉGLAGES tab.
    BugResult,
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
    ProfileDeleteConfirm { game_idx: usize, profile_idx: usize },
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
pub(crate) const OPTIONS_ENTRIES: &[&str] = &[
    "FAVORI", "TOUCHES", "RENOMMER", "JAQUETTE", "SUPPRIMER",
];

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
        Screen::SettingsModal { .. } => Some(Tab::Reglages),
        _ => None,
    }
}

/// The game most recently launched, as `(basename, display_name)`. Set on
/// pick (JOUER), and deliberately NOT cleared by `reset()` so that:
///   1. quitting a game back to the library restores the cursor to that
///      row (instead of jumping to the top), and
///   2. the in-game pause menu can show the game's name under "PAUSE".
static LAST_PLAYED: Mutex<Option<(std::string::String, std::string::String)>> = Mutex::new(None);

/// System tick captured when a game is launched; `open()` (return to library)
/// subtracts it to bank the session's playtime. NOT cleared by `reset()` so it
/// survives the game->library teardown.
static LAUNCH_TICK: Mutex<Option<u64>> = Mutex::new(None);

/// Companion-SWF count of the launching game's `<game>.files/` folder
/// (multi-file indicator, v1.3.0). Set at launch (A-press), shown on the launch
/// reveal, reset to 0 on return to the library so the quit reveal stays clean.
static LAUNCH_COMPANIONS: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(0);

/// Active library sort, persisted to `sdmc:/flashnx/sort.txt`
/// (0 = A-Z, 1 = recent, 2 = recently played, 3 = most played, 4 = size).
/// Default A-Z.
static SORT_MODE: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);
/// Whether the active sort is reversed (the modal's X toggle). Persisted in the
/// same `sort.txt` as a trailing `R` after the mode digit. Default off.
static SORT_REVERSE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
/// One-shot guard: load playtime + sort prefs once per boot (not every open()).
static PREFS_LOADED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

const SORT_PATH: &str = "sdmc:/flashnx/sort.txt";

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum SortMode {
    Alpha,
    Recent,
    RecentlyPlayed,
    MostPlayed,
    Size,
}

pub(crate) fn current_sort_mode() -> SortMode {
    match SORT_MODE.load(std::sync::atomic::Ordering::Relaxed) {
        1 => SortMode::Recent,
        2 => SortMode::RecentlyPlayed,
        3 => SortMode::MostPlayed,
        4 => SortMode::Size,
        _ => SortMode::Alpha,
    }
}

/// 0..=4 index of the active sort (for the sort modal cursor).
pub(crate) fn sort_mode_index() -> usize {
    SORT_MODE.load(std::sync::atomic::Ordering::Relaxed) as usize
}

/// Whether the active sort is currently reversed.
pub(crate) fn current_sort_reverse() -> bool {
    SORT_REVERSE.load(std::sync::atomic::Ordering::Relaxed)
}

/// Persist the mode + reverse flag together to `sort.txt` (digit, then `R` if
/// reversed). Best-effort.
fn persist_sort() {
    let mode = SORT_MODE.load(std::sync::atomic::Ordering::Relaxed);
    let rev = SORT_REVERSE.load(std::sync::atomic::Ordering::Relaxed);
    write_sort_pref(SORT_PATH, mode, rev);
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

/// Set + persist the active sort mode (0..=4).
pub(crate) fn set_sort_mode(idx: u8) {
    SORT_MODE.store(idx, std::sync::atomic::Ordering::Relaxed);
    persist_sort();
}

/// Set + persist the reverse flag.
pub(crate) fn set_sort_reverse(rev: bool) {
    SORT_REVERSE.store(rev, std::sync::atomic::Ordering::Relaxed);
    persist_sort();
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
        let (mode, rev) = read_sort_prefs(SORT_PATH);
        SORT_MODE.store(mode, std::sync::atomic::Ordering::Relaxed);
        SORT_REVERSE.store(rev, std::sync::atomic::Ordering::Relaxed);
        let (mode, rev) = read_sort_prefs(IMPORT_SORT_PATH);
        IMPORT_SORT.store(mode, std::sync::atomic::Ordering::Relaxed);
        IMPORT_REVERSE.store(rev, std::sync::atomic::Ordering::Relaxed);
    }
}

// ── IMPORTER home sort (Y on the saved-URL list) ─────────────────────────
//
// Separate from the JOUER sort: different list, different modes, and the user
// expects each tab to keep its own choice. 0 = ADDED (most recent first — the
// order URLs arrive in, which is what you want right after importing), 1 = NAME
// (by the row's display label), 2 = SOURCE (grouped by host), 3 = FILES (how
// many `.swf` the source holds).
const IMPORT_SORT_PATH: &str = "sdmc:/flashnx/import_sort.txt";
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
    write_sort_pref(IMPORT_SORT_PATH, idx, rev);
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
            entries.sort_by(|a, b| b.size_bytes.cmp(&a.size_bytes).then_with(|| alpha(a, b)))
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

fn note_played(basename: &str, display_name: &str) {
    if let Ok(mut g) = LAST_PLAYED.lock() {
        *g = Some((basename.into(), display_name.into()));
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
        .unwrap_or_else(|| basename.clone());
    note_played(&basename, &display_name);
}

fn last_played_basename() -> Option<std::string::String> {
    LAST_PLAYED.lock().ok().and_then(|g| g.as_ref().map(|(b, _)| b.clone()))
}

/// Display name of the currently/last launched game — used by the pause
/// menu (`render::draw_menu_overlay`) to show the title under "PAUSE".
pub fn active_display_name() -> Option<std::string::String> {
    LAST_PLAYED.lock().ok().and_then(|g| g.as_ref().map(|(_, n)| n.clone()))
}

/// Sidecar JSON written next to the SWF — gives a display name override
/// without touching the physical filename. Per the README 3.4.bis design:
/// **never** rename the .swf file itself (saves/keymap/etc. all key off
/// basename). Sidecar lives at `sdmc:/ruffle/<basename>.meta.json`.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub(crate) struct MetaSidecar {
    pub display_name: Option<std::string::String>,
}

/// User-facing SD roots. Order = priority for lookup (read). Writes
/// always go to entry 0 (the new `sdmc:/flashnx/`). The legacy
/// `sdmc:/ruffle/` is kept for backward compat — users coming from
/// pre-rename builds still see their saves/sidecars without manual
/// migration.
const USER_SD_ROOTS: &[&str] = &["sdmc:/flashnx", "sdmc:/ruffle"];

/// Find a user-facing sidecar / config file by suffix (e.g.
/// "Super_Mario_63_2010.swf.meta.json") under one of the known SD
/// roots. Returns the first existing path, or None.
fn find_user_file(suffix: &str) -> Option<std::string::String> {
    for root in USER_SD_ROOTS {
        let p = std::format!("{}/{}", root, suffix);
        if file_exists(&p) {
            return Some(p);
        }
    }
    None
}

/// Where to WRITE a user-facing sidecar / config. Always the primary
/// root (entry 0 of `USER_SD_ROOTS`) — new state goes to `flashnx/`.
fn primary_user_path(suffix: &str) -> std::string::String {
    std::format!("{}/{}", USER_SD_ROOTS[0], suffix)
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
    pub display_name: std::string::String,
    pub size_bytes: u64,
    pub swf_version: u8,
    /// 0 = uncompressed FWS, 1 = zlib CWS, 2 = lzma ZWS.
    pub compression_label: &'static str,
    /// True if the movie is ActionScript 3 (AVM2). Surfaced as a neutral "AS3"
    /// tag in the library — Ruffle's AVM2 is less complete than AVM1, so it's
    /// the riskier engine, but it's informational only: many AS3 games run fine
    /// (Mario Forever) while others don't (Pursuit of Hat), so we flag the
    /// engine rather than claim a game is broken.
    pub is_as3: bool,
    /// 0xRRGGBB derived from a hash of the basename — drives the per-game
    /// color chip in the list. Same hash always produces the same color
    /// across reboots (no persistence needed) because the input is stable.
    pub color_chip: u32,
    /// File mtime (seconds since epoch, 0 if unavailable) — drives the "recent
    /// first" sort. Relative order is reliable even with a wrong console clock
    /// (all files are stamped by the same clock).
    pub mtime: u64,
    /// archive.org item id this game was imported from, read from the `<swf>.url`
    /// sidecar at scan time. Empty for hand-copied files and for non-archive.org
    /// imports. Lets the IMPORTER list count how many of an item's files are
    /// already on SD without touching the filesystem again.
    pub source_item: std::string::String,
}

pub(crate) struct State {
    pub(crate) screen: Screen,
    pub(crate) entries: std::vec::Vec<Entry>,
    /// Set when the user presses A on a game. Read by C++ via
    /// `ruffle_library_selected_path` after the loop exits.
    selected_path: Option<std::string::String>,
    /// GL texture id for the FlashNX banner image (assets/banner.png decoded
    /// at init). 0 = not loaded (decode failed or init not called yet).
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
    pub(crate) url_meta: std::vec::Vec<(std::string::String, u32, u64, bool)>,
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
    /// Bug-report result message (`Screen::BugResult`) + whether it succeeded
    /// (drives the green/red notice styling).
    bug_msg: std::string::String,
    bug_ok: bool,
}

static LIBRARY: Mutex<State> = Mutex::new(State {
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
    cover_candidates: std::vec::Vec::new(),
    cover_use_shot: false,
    profile_matches: std::vec::Vec::new(),
    touches_can_revert: false,
    touches_has_backup: false,
    touches_cursor_idx: -1,
    touches_show_cursor: true,
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
    bug_msg: std::string::String::new(),
    bug_ok: false,
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

/// Rows of covers visible at once in the JOUER justified gallery (v1.2.0).
/// Must match the number of rows `draw_library_gallery` fits under the banner.
pub const GALLERY_ROWS_VISIBLE: usize = 3;

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

/// Rows visible at once in the bug-report game picker (RÉGLAGES → SIGNALER UN
/// BUG). A simple scrollable name list — shares the layout in
/// `draw_library_bug_picker`.
pub const BUG_PICKER_VISIBLE_ROWS: usize = 10;

// ── Setup / FFI helpers ───────────────────────────────────────────────────

extern "C" {
    fn ruffle_tick_now() -> u64;
    fn ruffle_tick_freq() -> u64;
    fn ruffle_log_cstr(msg: *const core::ffi::c_char);
    /// 1 = small applet memory pool (can't launch games — see P1c notice),
    /// 0 = full title-takeover heap. Defined in cpp/src/ruffle_bridge.cpp.
    fn ruffle_is_applet_mode() -> core::ffi::c_int;
}

fn log(s: &str) {
    let mut bytes = s.as_bytes().to_vec();
    bytes.push(0);
    unsafe { ruffle_log_cstr(bytes.as_ptr() as *const _) };
}

/// Called from C++ once per `.swf` found during the SD scan. Parses the
/// header inline so the library list has full metadata to display.
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
                    e.size_bytes = sz; // reflect it now, without waiting for a re-scan
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
const SCAN_INDEX_FILE: &str = "sdmc:/flashnx/.swf_header_index.json";

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
    let parsed = read_sd_text(SCAN_INDEX_FILE, 1024 * 1024)
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
fn scan_note_header(path: &str, mtime: u64, size: u64, ver: u8, comp: &str, as3: bool) {
    if let Ok(mut rows) = SCAN_HEADERS.lock() {
        rows.push(IndexEntry {
            path: path.to_string(),
            mtime,
            size,
            ver,
            comp: comp.to_string(),
            as3,
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
        if std::fs::write(SCAN_INDEX_FILE, json.as_bytes()).is_ok() {
            note_file_created(SCAN_INDEX_FILE);
            crate::sd::commit();
            log(&std::format!("library: swf header index written ({} games)\n", rows.len()));
        }
    }
}

/// Cached SWF header for `path`, if the index has it at exactly this mtime.
fn index_lookup(path: &str, mtime: u64) -> Option<(u64, u8, &'static str, bool)> {
    // A game whose mtime is unknown (0) can't be validated — don't trust it.
    if mtime == 0 {
        return None;
    }
    let idx = SCAN_INDEX.lock().ok()?;
    let e = idx.iter().find(|e| e.path == path && e.mtime == mtime)?;
    let comp = static_compression_label(&e.comp)?;
    Some((e.size, e.ver, comp, e.as3))
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
         | .url {:.0} ms | log {:.0} ms\n",
        entries, ms(0), ms(1), ms(2), ms(3), ms(4),
    ));
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
    let (header_size, swf_version, compression_label, is_as3) =
        match index_lookup(path, mtime) {
            Some(hit) => hit,
            None => {
                SCAN_INDEX_DIRTY.store(true, core::sync::atomic::Ordering::Relaxed);
                match scan_timed(0, || read_swf_header(path)) {
                    Some(h) => (h.size_bytes, h.version, h.compression_label, h.is_as3),
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
    scan_note_header(path, mtime, header_size, swf_version, compression_label, is_as3);
    // Prefer the cached real footprint (multi-file games); fall back to the SWF
    // header's declared size for single-file / pre-feature games.
    let size_bytes = scan_timed(1, || read_filesize_cache(path)).unwrap_or(header_size);
    let color_chip = color_from_basename(&basename);
    // Honour a per-game display-name override from the .meta.json sidecar
    // (Phase 3.4.bis RENOMMER). Sidecar absent / unparseable / empty
    // display_name → fall back to basename.
    let display_name = scan_timed(2, || read_meta_sidecar(&basename))
        .and_then(|m| m.display_name)
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| basename.clone());
    // Which import source this game came from (its `.url` sidecar). Read here,
    // alongside the other sidecars, so the IMPORTER list can count an item's
    // installed files in memory instead of hitting the SD every frame.
    let source_item = scan_timed(3, || read_source_item(path));
    let entry = Entry {
        path: path.to_string(),
        display_name,
        basename,
        size_bytes,
        swf_version,
        compression_label,
        is_as3,
        color_chip,
        mtime,
        source_item,
    };
    scan_timed(4, || {
        log(&std::format!(
            "library: added {} (SWF v{} {}, {})\n",
            path, swf_version, compression_label,
            if is_as3 { "AS3/AVM2" } else { "AS2/AVM1" },
        ))
    });
    if let Ok(mut s) = LIBRARY.lock() {
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
    log_scan_breakdown(LIBRARY.lock().map(|g| g.entries.len()).unwrap_or(0));
    // Reload URL history from SD on each open so changes from a previous
    // .nro boot are visible. Cheap (file is <2 KB typical).
    load_history_from_sd();
    // Boot-once: load persisted playtime + sort preference.
    ensure_prefs_loaded();
    // Returning from a game? Bank that session's playtime before we re-sort.
    bank_playtime();
    // If we're re-opening after quitting a game, land the cursor back on
    // that game's row instead of the top of the list.
    let want = last_played_basename();
    let applet = unsafe { ruffle_is_applet_mode() } != 0;
    log(&std::format!("library: open applet_mode={}\n", applet));
    // Identity of the just-played game, for the quit "close" reveal below.
    let mut collapse_info: Option<(std::string::String, std::string::String, u32)> = None;
    if let Ok(mut s) = LIBRARY.lock() {
        s.applet_mode = applet;
        // Fresh open = no filter, so `want`'s absolute index below is a valid
        // filtered-view position (view == full list).
        s.local_filter = None;
        s.anim_origin_ticks = unsafe { ruffle_tick_now() };
        // Apply the active sort BEFORE the cursor position is computed below.
        sort_entries(&mut s.entries, current_sort_mode(), current_sort_reverse());
        s.screen = if s.entries.is_empty() {
            Screen::Empty
        } else {
            let selection = want
                .as_deref()
                .and_then(|b| s.entries.iter().position(|e| e.basename == b))
                .unwrap_or(0);
            // The JOUER gallery scrolls by ROWS (not flat index), and a tile's
            // row depends on the justified layout — so derive the scroll from the
            // layout the last render published (still valid across a quit->library
            // round-trip: same entries). `gallery_scroll_for` falls back to 0 when
            // no layout exists yet (first boot). Using the old flat `clamp_scroll`
            // here put a last-row game off-screen (blank gallery on quit).
            let scroll_offset = gallery_scroll_for(selection, jouer_scroll());
            Screen::List { selection, scroll_offset }
        };
        // Returning from a game (last-played still present) → grab its identity
        // so we can play the cover shrinking back to its tile.
        if let Some(b) = want.as_deref() {
            if let Some(e) = s.entries.iter().find(|e| e.basename == b) {
                collapse_info = Some((e.basename.clone(), e.display_name.clone(), e.color_chip));
            }
        }
    }
    // Snap the gallery glide to wherever the cursor lands (last-played row),
    // so the first JOUER frame doesn't slide in from a stale position.
    // The multi-file badge is a launch-only cue; clear it so the quit reveal
    // (cover shrinking back to the tile) doesn't carry it.
    LAUNCH_COMPANIONS.store(0, std::sync::atomic::Ordering::Relaxed);
    crate::backend::render::gallery_anim_reset();
    // Play the quit "close" reveal: the cover shrinks from full screen back to
    // the launched tile (rect = the tile's pre-launch screen box, still cached).
    if let Some((bn, dn, color)) = collapse_info {
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
    let mut list: std::vec::Vec<(std::string::String, u32, u64, bool)> =
        std::vec::Vec::with_capacity(arr.len());
    for row in arr {
        let Some(cells) = row.as_array() else { continue };
        let Some(url) = cells.first().and_then(|v| v.as_str()).filter(|u| !u.is_empty()) else {
            continue;
        };
        let total = cells.get(1).and_then(|v| v.as_u64()).unwrap_or(0) as u32;
        let added = cells.get(2).and_then(|v| v.as_u64()).unwrap_or(0);
        let fav = cells.get(3).and_then(|v| v.as_bool()).unwrap_or(false);
        list.push((url.to_string(), total, added, fav));
    }
    if let Ok(mut s) = LIBRARY.lock() {
        log(&std::format!("library: history meta LOADED {} entrie(s)\n", list.len()));
        s.url_meta = list;
        s.meta_loaded = true;
    }
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
    s.url_meta.retain(|(u, _, _, _)| history.iter().any(|h| h == u));
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
    match s.url_meta.iter_mut().find(|(u, _, _, _)| u == url) {
        Some(slot) => {
            if slot.1 == total {
                return; // unchanged — no need to rewrite the card
            }
            slot.1 = total;
        }
        None => s.url_meta.push((url.to_string(), total, now_epoch(), false)),
    }
    save_history_meta(&mut s);
}

/// True if the saved URL is favorited (pinned to the top of the IMPORTER list
/// and flagged with a gold diamond, like a favorited game).
pub(crate) fn url_is_favorite(s: &State, url: &str) -> bool {
    s.url_meta
        .iter()
        .find(|(u, _, _, _)| u == url)
        .map(|(_, _, _, f)| *f)
        .unwrap_or(false)
}

/// Flip a saved URL's favorite state and persist. Returns the NEW state.
fn toggle_url_favorite(s: &mut State, url: &str) -> bool {
    let now_fav = match s.url_meta.iter_mut().find(|(u, _, _, _)| u == url) {
        Some(slot) => {
            slot.3 = !slot.3;
            slot.3
        }
        None => {
            s.url_meta.push((url.to_string(), 0, now_epoch(), true));
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

/// `YYYY-MM-DD` for a UNIX epoch, or empty for 0/unknown. Civil-from-days
/// (Howard Hinnant's algorithm) — no date crate in a no_std-ish build.
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
    std::format!("{:04}-{:02}-{:02}", y, m, d)
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
fn read_source_item(swf_path: &str) -> std::string::String {
    let path = std::format!("{}.url", swf_path);
    if !file_exists(&path) {
        return std::string::String::new();
    }
    let Ok(url) = read_sd_text(&path, 4096) else {
        return std::string::String::new();
    };
    if !url.contains("archive.org") {
        return std::string::String::new();
    }
    crate::net::extract_item_id(url.trim())
        .map(|id| id.to_lowercase())
        .unwrap_or_default()
}

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
        // New entry: stamp when it was saved (shown in its options modal).
        s.url_meta.push((url.clone(), 0, now_epoch(), false));
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
    /// Eased scroll (px) captured when the drag began.
    start_scroll_px: f32,
}

static TOUCH: Mutex<TouchState> = Mutex::new(TouchState {
    down: false,
    start_x: 0.0,
    start_y: 0.0,
    dragging: false,
    start_scroll_px: 0.0,
});

/// Forward the per-frame touchscreen state to the JOUER gallery: drag to scroll,
/// tap a tile to select it, tap the already-selected tile again to launch it.
/// `(x, y)` are screen px; `pressed` is whether a finger is down this frame.
/// No-op on every other screen (those stay button-driven).
pub fn touch(x: f32, y: f32, pressed: bool) {
    use crate::backend::render as r;
    // Movement (px) before a press is treated as a drag instead of a tap.
    const DRAG_THRESH: f32 = 16.0;

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

    let mut t = match TOUCH.lock() {
        Ok(t) => t,
        Err(_) => return,
    };

    // Off a touchable gallery: cancel any in-flight gesture, drop scroll override.
    if !on_gallery {
        if t.down {
            r::gallery_touch_scroll_set(None);
        }
        t.down = false;
        t.dragging = false;
        return;
    }

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
                        // Tap a row to select it, tap the selected row again to
                        // activate it (fetch/download the URL, or open the
                        // add-URL keyboard). Dragging is handled above.
                        if hit == selection {
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
                // Back to REGLAGES PAR DEFAUT, which is where the global
                // keymap row now lives.
                *p = Some(PendingClose::CloseMenuGoto(Screen::SettingsPrefsModal { selection: 0 }));
            }
            crate::backend::render::modal_close_begin();
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
                        s.screen = Screen::SettingsPrefsModal { selection: 0 };
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
                let target = if button == "L" { tab.prev() } else { tab.next() };
                if let Ok(mut s) = LIBRARY.lock() {
                    s.screen = match target {
                        Tab::Jouer => {
                            if s.entries.is_empty() {
                                Screen::Empty
                            } else {
                                Screen::List { selection: 0, scroll_offset: 0 }
                            }
                        }
                        Tab::Importer => distant_idle_at(0),
                        Tab::Reglages => Screen::SettingsModal { selection: 0 },
                    };
                }
                // Slide the incoming tab's content in from the side pressed
                // (L = from the left, R = from the right); the navbar stays fixed.
                // (Tabs slide; modals/editors scale.)
                let dir = if button == "L" { -1.0 } else { 1.0 };
                crate::backend::render::tab_transition_begin(dir);
                // Snap the JOUER glide so the gallery lands on its (reset) top
                // instead of gliding from a stale cursor.
                crate::backend::render::gallery_anim_reset();
                return true;
            }
        }
        if let Screen::DistantIdle { selection, scroll_offset } = screen_snap {
            if button == "X" {
                // X on the IMPORTER home = search Flashpoint for a game to
                // download. Hoisted: swkbd + HTTPS must run WITHOUT the lock.
                run_fp_search_flow();
                return true;
            }
            if button == "Minus" {
                // Search the saved URLs — same Minus-opens-swkbd convention as
                // the JOUER gallery and the archive.org file list.
                run_import_search_flow();
                return true;
            }
            if button == "A" {
                // A on a URL row launches it; A on row 0 (the "+ add" row, which
                // has no URL behind it) opens swkbd.
                let url = LIBRARY.lock().ok().and_then(|s| {
                    importer_abs_for_row(&s, selection)
                        .and_then(|abs| s.url_history.get(abs).cloned())
                });
                match url {
                    // archive.org → async fetch + reveal (begun inside, opens the
                    // row with a spinner); direct .swf → download screen. The
                    // expand is started by `run_archive_fetch_async`, not here.
                    Some(u) => run_fetch_for_url(&u, selection, scroll_offset),
                    None => run_add_url_flow(),
                }
                return true;
            }
        }
        // Y on the error notice = fix the URL that just failed, pre-filled, and
        // retry it on the spot (typos in a long archive.org URL are THE common
        // failure). Hoisted: swkbd + the retry fetch must run without the lock.
        if matches!(screen_snap, Screen::DistantError) && button == "Y" {
            let failed = LIBRARY
                .lock()
                .ok()
                .map(|s| s.failed_url.clone())
                .unwrap_or_default();
            if !failed.is_empty() {
                run_fix_url_flow(&failed);
                return true;
            }
        }
        // EDIT a URL from its options modal (entry 1) — swkbd, hoisted.
        if let Screen::DistantUrlOptions { url_idx, selection } = screen_snap {
            if button == "A" && selection == 1 {
                run_edit_url_flow(url_idx);
                return true;
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
            if button == "A" && selection == 5 {
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
                run_open_preview_flow(game_idx, selection);
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
        if let Screen::ProfileDeleteConfirm { game_idx, profile_idx } = screen_snap {
            if button == "A" {
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
        // A on the RÉGLAGES "FAIRE UNE PROPOSITION" row (index 4) = swkbd + POST.
        // Hoisted for the same reason.
        if let Screen::SettingsModal { selection } = screen_snap {
            if button == "A" && selection == 4 {
                run_suggestion_flow();
                return true;
            }
            // PSEUDO (#20): swkbd to set the community-profile nickname. Hoisted.
            if button == "A" && selection == 5 {
                run_pseudo_flow();
                return true;
            }
        }
        // `+` on a Flashpoint gallery tile = details popup. Hoisted: it does a
        // blocking HEAD to read the download size, which must not run under the
        // LIBRARY lock.
        if let Screen::FpGallery { selection, scroll } = screen_snap {
            if button == "Plus" {
                run_fp_info_flow(selection, scroll);
                return true;
            }
            // X = re-edit the ACTIVE search without going back to the IMPORTER
            // home. `run_fp_search_flow` pre-fills swkbd from `cover_query` (the
            // current term), so the user can fix/refine it in place. Hoisted:
            // swkbd + the async GET must run WITHOUT the LIBRARY lock.
            if button == "X" {
                run_fp_search_flow();
                return true;
            }
            // Hidden ZL+ZR chord (synthesised in main.cpp): toggle the content
            // filter and re-run the query. Hoisted because it starts an async GET.
            if button == "ZL+ZR" {
                run_fp_filter_toggle_flow();
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
            match button {
                "Minus" => { s.screen = Screen::Quit; }
                "Y" => { s.screen = distant_idle_at(0); }
                _ => {}
            }
            true
        }
        Screen::List { selection, scroll_offset } => {
            handle_list_input(&mut s, button, selection, scroll_offset);
            true
        }
        Screen::AppletNotice { selection, scroll_offset } => {
            // Any dismiss button returns to the same list row.
            if matches!(button, "A" | "B" | "Minus") {
                s.screen = Screen::List { selection, scroll_offset };
            }
            true
        }
        Screen::OptionsModal { game_idx, selection } => {
            handle_options_input(&mut s, button, game_idx, selection);
            true
        }
        Screen::TouchesEditor { .. } => false,
        Screen::DeleteConfirm { game_idx } => {
            handle_delete_confirm_input(&mut s, button, game_idx);
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
            // `+` is hoisted only for FpGallery; here `+`/`B` close back to the
            // gallery (the deferred modal-close machinery scales it out).
            if matches!(button, "B" | "Plus") {
                s.screen = Screen::FpGallery { selection, scroll };
            }
            true
        }
        Screen::SortModal { selection, prev_sel, prev_scroll } => {
            handle_sort_modal_input(&mut s, button, selection, prev_sel, prev_scroll);
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
            // A on entry 0 (edit) is hoisted; here we handle move / delete / back.
            handle_distant_url_options_input(&mut s, button, url_idx, selection);
            true
        }
        Screen::DistantFiles { selection, scroll_offset } => {
            handle_distant_files_input(&mut s, button, selection, scroll_offset);
            true
        }
        Screen::DistantLoading => {
            // Input is suspended during the opening reveal (handled at the top of
            // input via `distant_reveal_active`); once open, B/Minus/Y cancels the
            // async fetch and returns to the IMPORTER list.
            if matches!(button, "B" | "Minus" | "Y") {
                net::cancel_archive_fetch();
                s.screen = distant_idle_at(0);
            }
            true
        }
        Screen::DistantDownloading => {
            // B = cancel download -> show the "cancelled" notice (DistantError).
            // The download state (`download_resume_pos` / `download_zip_extract`)
            // is left intact so DistantError's dismiss can route back to the list
            // the download came from. Any other button is ignored during DL.
            if matches!(button, "B") {
                net::cancel_download();
                s.distant_error = std::string::String::from(crate::loc::s().err_dl_cancelled);
                s.screen = Screen::DistantError;
            }
            true
        }
        Screen::DistantError => {
            // Y (fix the URL) is hoisted — swkbd can't run under the lock.
            if matches!(button, "A" | "B" | "Minus") {
                s.distant_error.clear();
                s.failed_url.clear();
                // Dismiss the notice back to the LIST the download came from
                // (Flashpoint results / archive.org file list), restoring the
                // cursor, rather than the IMPORTER home. `download_zip_extract`
                // = a Flashpoint GameZIP; `download_resume_pos` carries the
                // cursor. Non-download errors (no resume pos) fall back to home.
                let resume = s.download_resume_pos.take();
                let was_fp = s.download_zip_extract.is_some() || s.download_fp_direct;
                clear_download_state(&mut s);
                match resume {
                    Some((sel, scroll)) if was_fp => {
                        s.screen = Screen::FpGallery { selection: sel, scroll };
                    }
                    Some((sel, scroll)) if !s.remote_files.is_empty() => {
                        let filtered_len =
                            filtered_indices(&s.remote_files, &s.distant_filter).len();
                        let sel = sel.min(filtered_len.saturating_sub(1));
                        let scroll = clamp_scroll(scroll, sel, DISTANT_VISIBLE_ROWS);
                        s.screen = Screen::DistantFiles { selection: sel, scroll_offset: scroll };
                    }
                    _ => {
                        s.screen = distant_idle_at(0);
                    }
                }
            }
            true
        }
        Screen::DistantHistoryConfirm => {
            // Both paths scale OUT first (panel stays populated); the delete (A)
            // is deferred to render's close-done, cancel (B) just returns.
            match button {
                "A" => {
                    if let Ok(mut p) = PENDING_AFTER_CLOSE.lock() {
                        *p = Some(PendingClose::DeleteHistory);
                    }
                    crate::backend::render::modal_close_begin();
                }
                "B" | "Minus" => {
                    let idx = s.history_idx.unwrap_or(0);
                    let row = importer_row_for_abs(&s, idx);
                    if let Ok(mut p) = PENDING_AFTER_CLOSE.lock() {
                        *p = Some(PendingClose::Goto(distant_idle_at(row)));
                    }
                    crate::backend::render::modal_close_begin();
                }
                _ => {}
            }
            true
        }
        Screen::SettingsModal { selection } => {
            handle_settings_input(&mut s, button, selection);
            true
        }
        // Owned by the reused menu::* editor (handled at the top of input()).
        Screen::SettingsKeymapEditor => false,
        Screen::SettingsLanguagePicker { selection } => {
            handle_settings_language_input(&mut s, button, selection);
            true
        }
        Screen::SettingsPrefsModal { selection } => {
            handle_settings_prefs_input(&mut s, button, selection);
            true
        }
        Screen::BugPicker { selection, scroll_offset } => {
            handle_bug_picker_input(&mut s, button, selection, scroll_offset);
            true
        }
        Screen::BugResult => {
            if matches!(button, "A" | "B" | "Minus") {
                s.bug_msg.clear();
                // Back to the RÉGLAGES tab, cursor on SIGNALER UN BUG (row 3).
                s.screen = Screen::SettingsModal { selection: 3 };
            }
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
        Screen::ProfileDeleteConfirm { game_idx, profile_idx } => {
            // A (delete) is hoisted in input(); B cancels back to the picker.
            if matches!(button, "B" | "Minus") {
                s.screen = Screen::ProfileList { game_idx, selection: profile_idx };
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
                goto_touches_menu(&mut s, game_idx, 4); // the REVERT row
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
    let new_screen = s.screen;
    if modal_close_deferred(modal_kind(screen_copy))
        && modal_kind(new_screen) == 0
        && new_screen != screen_copy
    {
        if let Ok(mut p) = PENDING_AFTER_CLOSE.lock() {
            *p = Some(PendingClose::Goto(new_screen));
        }
        s.screen = screen_copy;
        crate::backend::render::modal_close_begin();
    }
    consumed
}

/// Settings tab entries: 0 = home layout, 1 = game defaults, 2 = language,
/// 3 = report a bug, 4 = make a suggestion, 5 = nickname, 6 = quit. (No BACK —
/// leave via L/R.) Rows 4 and 5 are hoisted BY INDEX in `input()` because they
/// open the system keyboard, so any reordering has to move those two with it.
fn handle_settings_input(s: &mut State, button: &str, mut selection: usize) {
    const LAST: usize = 6;
    match button {
        "Up" | "StickLUp" => {
            selection = if selection == 0 { LAST } else { selection - 1 };
        }
        "Down" | "StickLDown" => {
            selection = if selection >= LAST { 0 } else { selection + 1 };
        }
        "A" => {
            match selection {
                0 => {
                    // JOUER layout: cycle in place. The global keymap row that used
                    // to sit here moved into REGLAGES PAR DEFAUT, where it belongs
                    // (it IS a per-game default), which freed index 0 — the only
                    // slot a new row can take without shifting the by-index hoists
                    // on rows 3 and 5.
                    crate::loc::set_home_view(crate::loc::home_view() + 1);
                    crate::loc::save_current();
                    // Forget the previous layout's eased state, or the new one
                    // slides in from coordinates that meant something else.
                    crate::backend::render::home_anim_reset();
                }
                1 => {
                    // Game defaults sub-modal (keymap / display / filter / cursor).
                    s.screen = Screen::SettingsPrefsModal { selection: 0 };
                    return;
                }
                2 => {
                    let cur = crate::loc::current().index();
                    s.screen = Screen::SettingsLanguagePicker { selection: cur };
                }
                3 => {
                    // SIGNALER UN BUG — pick the broken game, then describe + send.
                    // No game on SD = nothing to report; show a short notice.
                    if s.entries.is_empty() {
                        s.bug_ok = false;
                        s.bug_msg = crate::loc::s().bug_no_games.to_string();
                        s.screen = Screen::BugResult;
                    } else {
                        s.screen = Screen::BugPicker { selection: 0, scroll_offset: 0 };
                    }
                }
                // 4 = FAIRE UNE PROPOSITION and 5 = PSEUDO are hoisted in input()
                // (they open the swkbd).
                6 => {
                    // QUIT (Minus is SEARCH now). Exits the .nro.
                    s.screen = Screen::Quit;
                }
                _ => {}
            }
            return;
        }
        // No B "back to JOUER": the navbar (L/R) is the only inter-tab nav.
        _ => {}
    }
    s.screen = Screen::SettingsModal { selection };
}

/// Game DEFAULTS sub-modal (0 = global keymap, 1 = display, 2 = filter,
/// 3 = cursor speed). Rows 1-3 cycle in place on A, like the in-game rows they
/// mirror; row 0 opens the keymap editor. B returns to REGLAGES on the row that
/// opened this.
///
/// These are DEFAULTS, not overrides: they apply to a game that has never been
/// set from its own pause menu. A game with its own sidecar keeps its value, and
/// keeps it even if the default changes later.
fn handle_settings_prefs_input(s: &mut State, button: &str, mut selection: usize) {
    const LAST: usize = 3;
    match button {
        "Up" | "StickLUp" => {
            selection = if selection == 0 { LAST } else { selection - 1 };
        }
        "Down" | "StickLDown" => {
            selection = if selection >= LAST { 0 } else { selection + 1 };
        }
        "A" => {
            match selection {
                0 => {
                    // Global default keymap, moved here from the REGLAGES top
                    // level: it is a per-game default like the three below.
                    keymap::init_for_global_default();
                    menu::open();
                    s.screen = Screen::SettingsKeymapEditor;
                    return;
                }
                1 => {
                    let next = (crate::loc::default_display_mode() + 1)
                        % keymap::DISPLAY_MODE_COUNT;
                    crate::loc::set_default_display_mode(next);
                    crate::loc::save_current();
                }
                2 => {
                    let next = (crate::loc::default_screen_filter() + 1)
                        % keymap::SCREEN_FILTER_COUNT;
                    crate::loc::set_default_screen_filter(next);
                    crate::loc::save_current();
                }
                _ => {
                    // Cursor speed keeps its own home: C++ owns the live value and
                    // its `sdmc:/flashnx/cursor_speed` file, and knows on its own
                    // that out of a game this cycles the GLOBAL default.
                    unsafe { ruffle_cursor_speed_cycle() };
                }
            }
        }
        "B" | "Minus" => {
            // Back to REGLAGES PAR DEFAUT (row 1), the row that opened this.
            s.screen = Screen::SettingsModal { selection: 1 };
            return;
        }
        _ => {}
    }
    s.screen = Screen::SettingsPrefsModal { selection };
}

/// Language picker: `selection` indexes `loc::PICKER_LANGS`. A applies +
/// persists the choice; B/Minus cancels back to the settings modal.
fn handle_settings_language_input(s: &mut State, button: &str, mut selection: usize) {
    // The picker is a 3-column grid (see draw_library_language_picker): left/right
    // step linearly (wrapping), up/down move by a row and clamp to the last row.
    const COLS: usize = 3;
    let n = crate::loc::PICKER_LANGS.len();
    let last = n.saturating_sub(1);
    match button {
        "Left" | "StickLLeft" => {
            selection = if selection == 0 { last } else { selection - 1 };
        }
        "Right" | "StickLRight" => {
            selection = if selection >= last { 0 } else { selection + 1 };
        }
        "Up" | "StickLUp" => {
            selection = if selection >= COLS {
                selection - COLS
            } else {
                // Wrap to the bottom of the same column (last row that has it).
                let col = selection % COLS;
                let rows = n.div_ceil(COLS);
                let mut t = (rows - 1) * COLS + col;
                if t > last {
                    t = t.saturating_sub(COLS);
                }
                t
            };
        }
        "Down" | "StickLDown" => {
            let t = selection + COLS;
            selection = if t <= last { t } else { selection % COLS };
        }
        "A" => {
            if let Some(&lang) = crate::loc::PICKER_LANGS.get(selection) {
                crate::loc::set(lang);
                crate::loc::save(lang);
            }
            s.screen = Screen::SettingsModal { selection: 2 };
            return;
        }
        "B" | "Minus" => {
            s.screen = Screen::SettingsModal { selection: 2 };
            return;
        }
        _ => {}
    }
    s.screen = Screen::SettingsLanguagePicker { selection };
}

/// Bug-report game picker (RÉGLAGES → SIGNALER UN BUG). A scrollable list of
/// the local games; A (hoisted) describes + sends, B returns to RÉGLAGES.
/// `selection`/`scroll` index `entries` directly (no filter here).
fn handle_bug_picker_input(s: &mut State, button: &str, mut selection: usize, mut scroll: usize) {
    let total = s.entries.len();
    let last = total.saturating_sub(1);
    match button {
        "Up" | "StickLUp" => {
            if total == 0 {
                return;
            }
            selection = if selection == 0 { last } else { selection - 1 };
            scroll = clamp_scroll(scroll, selection, BUG_PICKER_VISIBLE_ROWS);
        }
        "Down" | "StickLDown" => {
            if total == 0 {
                return;
            }
            selection = if selection >= last { 0 } else { selection + 1 };
            scroll = clamp_scroll(scroll, selection, BUG_PICKER_VISIBLE_ROWS);
        }
        "B" => {
            // Back to SIGNALER UN BUG (row 3), the row that opened the picker.
            s.screen = Screen::SettingsModal { selection: 3 };
            return;
        }
        // A is hoisted in input() (swkbd + HTTPS POST run without the lock).
        _ => {}
    }
    s.screen = Screen::BugPicker { selection, scroll_offset: scroll };
}

/// BugPicker > A: snapshot the chosen game's metadata, open swkbd for a
/// description, then POST the report to the relay. Hoisted from `input()` — the
/// keyboard + synchronous HTTPS POST must NOT run under the LIBRARY lock.
fn run_bug_report_flow(game_idx: usize) {
    // Snapshot the game's technical info under the lock, then release it.
    let base = match LIBRARY.lock() {
        Ok(g) => g.entries.get(game_idx).map(|e| {
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
    // Description is optional — cancel (None) aborts the whole report.
    let Some(description) = net::prompt_bug() else {
        return;
    };
    let applet = unsafe { ruffle_is_applet_mode() } != 0;
    let report = crate::bugreport::Report {
        kind: "bug",
        game,
        file,
        source_url,
        size,
        swf_version,
        compression: compression.to_string(),
        as3,
        app_version: crate::bugreport::APP_VERSION,
        lang: crate::loc::current().code(),
        applet,
        description: description.trim().to_string(),
    };
    submit_and_show(&report);
}

/// RÉGLAGES > FAIRE UNE PROPOSITION: open swkbd for a free-text idea and POST it
/// as a "suggestion" issue (same relay/token as the bug report, different
/// label). No game involved. Hoisted from `input()` — swkbd + the synchronous
/// HTTPS POST must not run under the LIBRARY lock. Empty input aborts.
fn run_suggestion_flow() {
    let Some(text) = net::prompt_long(crate::loc::s().kbd_suggest_header, crate::loc::s().kbd_suggest_guide)
    else {
        return; // cancelled
    };
    let text = text.trim().to_string();
    if text.is_empty() {
        return; // nothing to send
    }
    let report = crate::bugreport::Report {
        kind: "suggestion",
        game: std::string::String::new(),
        file: std::string::String::new(),
        source_url: std::string::String::new(),
        size: 0,
        swf_version: 0,
        compression: std::string::String::new(),
        as3: false,
        app_version: crate::bugreport::APP_VERSION,
        lang: crate::loc::current().code(),
        applet: unsafe { ruffle_is_applet_mode() } != 0,
        description: text,
    };
    submit_and_show(&report);
}

/// RÉGLAGES > PSEUDO (#20): open swkbd (prefilled with the current nickname) to
/// set the community-profile nickname. Hoisted from `input()` — swkbd must run
/// without the LIBRARY lock. Empty input clears it.
fn run_pseudo_flow() {
    let current = crate::profiles::author_name();
    let Some(name) = net::prompt_pseudo(&current) else {
        return; // cancelled
    };
    crate::profiles::set_author_name(&name);
    if let Ok(mut s) = LIBRARY.lock() {
        s.screen = Screen::SettingsModal { selection: 5 };
    }
}

/// Submit a bug/suggestion report and land on the result screen.
fn submit_and_show(report: &crate::bugreport::Report) {
    let (ok, msg) = match crate::bugreport::submit(report) {
        Ok(()) => (true, crate::loc::s().bug_ok_msg.to_string()),
        Err(e) => (false, e),
    };
    if let Ok(mut s) = LIBRARY.lock() {
        s.bug_ok = ok;
        s.bug_msg = msg;
        s.screen = Screen::BugResult;
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
    if let Ok(mut s) = LIBRARY.lock() {
        // The picker modal sizes itself to its row count and is centred, with no
        // window and no scroll: 140 + 52*rows + 60 px tall. Past 10 rows that is
        // taller than the 720 px screen and its top edge goes negative, so the
        // first entries are drawn off-screen and cannot be reached. The preview
        // list is already capped the same way (`cap_preview_rows`).
        let mut matches = matches;
        matches.truncate(10);
        s.profile_matches = matches;
        s.active_profile_id = active_id;
        s.screen = Screen::ProfileList { game_idx, selection: 0 };
    }
}

/// Input for the community-profile picker (#20). Rows = matched profiles only
/// (revert moved to the TOUCHES sub-menu). A opens the before/after preview
/// (hoisted — it reads the current keymap off SD); B returns to the sub-menu.
fn handle_profile_list_input(s: &mut State, button: &str, game_idx: usize, mut selection: usize) {
    let n = s.profile_matches.len();
    // At least one row (the "no profile" notice) so nav has somewhere to sit.
    let last = n.max(1) - 1;
    match button {
        "Up" | "StickLUp" => {
            selection = if selection == 0 { last } else { selection - 1 };
        }
        "Down" | "StickLDown" => {
            selection = if selection >= last { 0 } else { selection + 1 };
        }
        // "A" (open preview) is hoisted to `run_open_preview_flow` — it reads the
        // current keymap file to build the diff, which we keep out of this lock.
        "X" => {
            // Delete MY OWN shared profile: only on a row this install shared.
            if s
                .profile_matches
                .get(selection)
                .is_some_and(|m| crate::profiles::is_mine(&m.profile.id))
            {
                s.screen = Screen::ProfileDeleteConfirm { game_idx, profile_idx: selection };
                return;
            }
        }
        "B" | "Minus" => {
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
    let row_count = TOUCHES_MENU_FIXED_ROWS + s.touches_can_revert as usize;
    s.screen = Screen::TouchesMenu { game_idx, selection: selection.min(row_count - 1) };
}

/// Fixed TOUCHES sub-menu rows: edit (0), apply (1), share (2), cursor speed (3),
/// show-cursor toggle (4). A revert row (5) is appended when `touches_can_revert`.
const TOUCHES_MENU_FIXED_ROWS: usize = 5;

/// Cursor-speed presets as x10 multipliers. MUST stay in sync with
/// `CURSOR_SPEED_MULTS` in cpp/src/main.cpp (the C++ side owns the live value;
/// we only display + persist the index here). Default index = 1 (x1.0).
const CURSOR_X10: &[u32] = &[5, 10, 15, 20, 25, 30, 40, 50];

/// Input for the TOUCHES sub-menu (#20). Edit (0) + cursor speed (3) + show-cursor
/// toggle (4) are handled here; apply (1) / share (2) / revert (5) are hoisted in
/// `input()` (file I/O / network). B returns to OPTIONS on the TOUCHES row.
fn handle_touches_menu_input(s: &mut State, button: &str, game_idx: usize, mut selection: usize) {
    let row_count = TOUCHES_MENU_FIXED_ROWS + s.touches_can_revert as usize;
    let last = row_count - 1;
    match button {
        "Up" | "StickLUp" => {
            selection = if selection == 0 { last } else { selection - 1 };
        }
        "Down" | "StickLDown" => {
            selection = if selection >= last { 0 } else { selection + 1 };
        }
        "A" => {
            // EDIT (0) + CURSOR SPEED (3) + SHOW-CURSOR (4) are local (file I/O
            // only). Apply (1) / share (2) / revert (5) need network → hoisted in
            // `input()`.
            if selection == 0 {
                // Init the keymap for THIS game so current_binding / set_binding
                // land in the right sidecar, then open the editor.
                if let Some(entry) = s.entries.get(game_idx) {
                    keymap::init_for_swf(&entry.basename);
                }
                menu::open();
                s.screen = Screen::TouchesEditor { game_idx };
                return;
            }
            if selection == 3 {
                // Cycle the per-game cursor speed in place (persist to the game's
                // `<basename>.cursor`; applied next launch). -1/unset → start at
                // the x1.0 default (index 1).
                let cur = if s.touches_cursor_idx < 0 { 1 } else { s.touches_cursor_idx };
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
        }
        "B" | "Minus" => {
            s.screen = Screen::OptionsModal { game_idx, selection: 1 }; // TOUCHES row
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
    let filtered = local_filtered_indices(&s.entries, &s.local_filter);
    let total = filtered.len();
    // v1.2.0: JOUER is a justified cover GALLERY (variable tiles per row). 2D
    // nav reads the layout the renderer publishes each frame: Left/Right =
    // prev/next tile in flow order, Up/Down = nearest tile in the adjacent row.
    // No wrap-around.
    match button {
        "Left" | "StickLLeft" => {
            if total > 0 && selection > 0 { selection -= 1; }
            scroll = gallery_scroll_for(selection, scroll);
        }
        "Right" | "StickLRight" => {
            if total > 0 && selection + 1 < total { selection += 1; }
            scroll = gallery_scroll_for(selection, scroll);
        }
        "Up" | "StickLUp" => {
            // Single-row layouts have no adjacent row for `gallery_neighbor` to
            // find, so Up/Down would be inert there. They jump instead — Up
            // FORWARD, matching the row's own left-to-right reading order rather
            // than a vertical list's.
            if matches!(crate::loc::home_view(), 2 | 3) {
                if total > 0 {
                    selection = (selection + home_page_step()).min(total - 1);
                }
            } else if let Some(ns) = gallery_neighbor(selection, -1) {
                selection = ns;
            }
            scroll = gallery_scroll_for(selection, scroll);
        }
        "Down" | "StickLDown" => {
            if matches!(crate::loc::home_view(), 2 | 3) {
                selection = selection.saturating_sub(home_page_step());
            } else if let Some(ns) = gallery_neighbor(selection, 1) {
                selection = ns;
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
                note_played(&entry.basename, &entry.display_name);
                if let Ok(mut g) = LAUNCH_TICK.lock() {
                    *g = Some(unsafe { ruffle_tick_now() });
                }
                // Multi-file indicator: count companion SWFs in <game>.files/ so
                // the launch reveal can flag a multi-file game.
                {
                    let mut p = entry.path.as_bytes().to_vec();
                    p.push(0);
                    let n = unsafe {
                        swf_picker_count_companions(p.as_ptr() as *const core::ffi::c_char)
                    };
                    LAUNCH_COMPANIONS.store(n.max(0), std::sync::atomic::Ordering::Relaxed);
                }
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
        // X = search (hoisted at the top of `input()`). `+` = per-game OPTIONS
        // (moved off ZL); the Settings modal `+` used to open is now the
        // REGLAGES navbar tab (reached with L/R).
        "Plus" => {
            if let Some(&abs) = filtered.get(selection) {
                s.screen = Screen::OptionsModal { game_idx: abs, selection: 0 };
            }
            return;
        }
        "Y" => {
            // Open the sort picker (cursor on the active mode); remember where we
            // were so B (cancel) restores it instead of jumping to the top.
            s.screen = Screen::SortModal {
                selection: sort_mode_index(),
                prev_sel: selection,
                prev_scroll: scroll,
            };
            return;
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
// id / file the URL points at) with the host dimmed beside it, the "+ add" row
// is pinned FIRST so it's reachable without scrolling, and the list filters
// (Minus) + sorts (Y) like every other list in the app. `url_history` itself
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
/// `selection` is a ROW index — row 0 is the "+ add" row, rows 1.. index this.
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
    match import_sort_index() {
        1 => idx.sort_by_key(|&i| importer_label(&s.url_history[i]).to_lowercase()),
        // SOURCE: grouped by host, by name inside a group.
        2 => idx.sort_by_key(|&i| {
            let u = &s.url_history[i];
            (importer_host(u).to_lowercase(), importer_label(u).to_lowercase())
        }),
        // FILES: biggest source first. A URL never opened has no count, and
        // sorts last rather than pretending to be empty.
        3 => idx.sort_by_key(|&i| {
            let u = &s.url_history[i];
            let total = importer_progress(s, u).1;
            (
                std::cmp::Reverse(total.unwrap_or(0)),
                total.is_none(),
                importer_label(u).to_lowercase(),
            )
        }),
        // ADDED: history is oldest-first, so newest-first = reversed.
        _ => idx.reverse(),
    }
    if import_sort_reverse() {
        idx.reverse();
    }
    // Favorites pinned on top whatever the sort, exactly like the JOUER gallery
    // (`sort_entries`). `sort_by_key` is stable, so the active order survives
    // inside each group — and the reverse above must not un-pin them, hence
    // pinning LAST.
    idx.sort_by_key(|&i| !url_is_favorite(s, &s.url_history[i]));
    idx
}

/// Rows of the IMPORTER list visible at once. Shared by the renderer (layout),
/// `distant_row_rect` (reveal geometry) and the scroll clamps, so they can't
/// drift apart.
pub const IMPORTER_VISIBLE_ROWS: usize = 10;

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

/// A `DistantIdle` screen parked on `row`, keeping the current scroll unless the
/// row would be off screen. Every caller handing the cursor back to a specific
/// row goes through this, so none of them can scroll the list out from under the
/// user or select a row the list isn't showing.
fn distant_idle_at(row: usize) -> Screen {
    let cur = IMPORT_SCROLL.load(std::sync::atomic::Ordering::Relaxed);
    Screen::DistantIdle {
        selection: row,
        scroll_offset: clamp_scroll(cur, row, IMPORTER_VISIBLE_ROWS),
    }
}

/// Row index (0 = the "+ add" row) of history entry `abs` in the current view,
/// so screens that act on an ABSOLUTE url index can hand the cursor back to the
/// right row. Falls back to the add row when `abs` is gone or filtered out.
fn importer_row_for_abs(s: &State, abs: usize) -> usize {
    importer_view(s)
        .iter()
        .position(|&i| i == abs)
        .map(|p| p + 1)
        .unwrap_or(0)
}

/// Absolute `url_history` index behind IMPORTER row `row` (rows 1..), or `None`
/// for row 0 (the "+ add" row) and out-of-range rows.
fn importer_abs_for_row(s: &State, row: usize) -> Option<usize> {
    if row == 0 {
        return None;
    }
    importer_view(s).get(row - 1).copied()
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
    let total = s
        .url_meta
        .iter()
        .find(|(u, _, _, _)| u == url)
        .map(|(_, n, _, _)| *n);
    let Some(id) = crate::net::extract_item_id(raw.as_str()).map(|i| i.to_lowercase()) else {
        return (0, total);
    };
    let have = s.entries.iter().filter(|e| e.source_item == id).count() as u32;
    // The cached total predates any file we've since downloaded, so never let
    // the badge read "5/3" if the item grew or the cache is stale.
    (have, total.map(|t| t.max(have)))
}

/// Per-row display data for the IMPORTER home, in view order: labels, host tags,
/// "is this a direct .swf" flags, and the `(on SD, total)` file counts. The
/// trailing `usize` is the UNFILTERED url count, for the "3 / 21" sub-line.
fn importer_rows(
    s: &State,
) -> (
    std::vec::Vec<std::string::String>,
    std::vec::Vec<std::string::String>,
    std::vec::Vec<bool>,
    std::vec::Vec<(u32, Option<u32>)>,
    std::vec::Vec<bool>,
    usize,
) {
    let view = importer_view(s);
    let mut labels = std::vec::Vec::with_capacity(view.len());
    let mut hosts = std::vec::Vec::with_capacity(view.len());
    let mut direct = std::vec::Vec::with_capacity(view.len());
    let mut progress = std::vec::Vec::with_capacity(view.len());
    let mut favorite = std::vec::Vec::with_capacity(view.len());
    for &i in &view {
        let u = &s.url_history[i];
        labels.push(importer_label(u));
        hosts.push(importer_host(u));
        direct.push(matches!(
            crate::sources::classify(crate::sources::wayback_raw(u).as_str()),
            crate::sources::SourceKind::DirectUrl
        ));
        progress.push(importer_progress(s, u));
        favorite.push(url_is_favorite(s, u));
    }
    (labels, hosts, direct, progress, favorite, s.url_history.len())
}

fn handle_distant_idle_input(
    s: &mut State,
    button: &str,
    mut selection: usize,
    mut scroll: usize,
) {
    // A (launch / add) and Minus (search) are hoisted (swkbd + HTTPS). Here we
    // navigate, open per-URL options and the sort picker. Row 0 = "+ add".
    let last = importer_view(s).len();
    match button {
        // Wrap both ways: with a long history, "up from the top" is the fastest
        // way to the oldest entries.
        "Up" | "StickLUp" => {
            selection = if selection == 0 { last } else { selection - 1 };
            scroll = clamp_scroll(scroll, selection, IMPORTER_VISIBLE_ROWS);
        }
        "Down" | "StickLDown" => {
            selection = if selection >= last { 0 } else { selection + 1 };
            scroll = clamp_scroll(scroll, selection, IMPORTER_VISIBLE_ROWS);
        }
        "Plus" => {
            // Options on a real URL row (not the "+ add" row).
            if let Some(abs) = importer_abs_for_row(s, selection) {
                s.screen = Screen::DistantUrlOptions { url_idx: abs, selection: 0 };
                return;
            }
        }
        "Y" => {
            // Sort picker (cursor on the active mode, direction pre-set).
            s.screen = Screen::RemoteSortModal {
                selection: import_sort_index(),
                kind: SORT_TARGET_IMPORT,
                reverse: import_sort_reverse(),
                prev_sel: selection,
                prev_scroll: scroll,
            };
            return;
        }
        // No B "back to JOUER": the navbar (L/R) is the only inter-tab nav.
        _ => {}
    }
    s.screen = Screen::DistantIdle { selection, scroll_offset: scroll };
}

/// DistantUrlOptions modal nav (Up/Down move, B back). A on entry 0 (edit) is
/// hoisted; entry 1 = delete (confirm), entry 2 = back.
fn handle_distant_url_options_input(
    s: &mut State,
    button: &str,
    url_idx: usize,
    mut selection: usize,
) {
    // 0 = favorite, 1 = edit, 2 = delete (no back row — B backs out).
    const LAST: usize = 2;
    let back = |s: &mut State| {
        // `url_idx` is an ABSOLUTE history index; the list is sorted/filtered, so
        // ask the view which row that entry is on.
        s.screen = distant_idle_at(importer_row_for_abs(s, url_idx));
    };
    match button {
        "Up" | "StickLUp" => {
            selection = if selection == 0 { LAST } else { selection - 1 };
        }
        "Down" | "StickLDown" => {
            selection = if selection >= LAST { 0 } else { selection + 1 };
        }
        "A" => {
            // 1 (edit) is hoisted in input(); 0 (favorite) and 2 (delete) here.
            if selection == 0 {
                if let Some(url) = s.url_history.get(url_idx).cloned() {
                    let now_fav = toggle_url_favorite(s, &url);
                    let lc = crate::loc::s();
                    let msg = if now_fav { lc.fav_added } else { lc.fav_removed };
                    set_toast(s, msg.to_string(), TOAST_OK);
                    // Pinning reorders the list, so hand the cursor back to the
                    // row this URL now occupies instead of a stale index.
                    back(s);
                }
                return;
            }
            if selection == 2 {
                // Delete -> reuse the existing confirm screen via history_idx.
                s.history_idx = Some(url_idx);
                s.screen = Screen::DistantHistoryConfirm;
            }
            return;
        }
        "B" => {
            back(s);
            return;
        }
        _ => {}
    }
    s.screen = Screen::DistantUrlOptions { url_idx, selection };
}

/// Remove the currently-displayed history URL and persist the trimmed list.
/// Keeps `history_idx` in range (or None when the list becomes empty).
fn delete_current_history(s: &mut State) {
    let Some(idx) = s.history_idx else { return };
    if idx >= s.url_history.len() {
        return;
    }
    s.url_history.remove(idx);
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
    }
}

/// IMPORTER "+ add" row: prompt for a NEW url (no prefill), then fetch. Called
/// from `input()` only (swkbd + HTTPS must run without the LIBRARY lock).
fn run_add_url_flow() {
    let Some(url) = net::prompt_url_with_initial(None) else {
        return; // user cancelled
    };
    // The reveal grows from the "+ add" row, pinned first (row 0), which is only
    // ever on screen at scroll 0.
    run_fetch_for_url(&url, 0, 0);
}

/// IMPORTER home search (Minus): swkbd pre-filled with the active filter, then
/// narrow the saved-URL list to entries matching it (URL or label). Empty input
/// clears the filter. Called from `input()` only (swkbd needs the lock free).
fn run_import_search_flow() {
    let current = LIBRARY
        .lock()
        .ok()
        .and_then(|s| s.import_filter.clone())
        .unwrap_or_default();
    let Some(typed) = net::prompt_search(&current) else {
        return; // cancelled
    };
    let trimmed = typed.trim().to_string();
    if let Ok(mut s) = LIBRARY.lock() {
        s.import_filter = if trimmed.is_empty() { None } else { Some(trimmed) };
        // Fresh (filtered) list: put the cursor on its first URL row, or on the
        // "+ add" row when nothing matched.
        let row = if importer_view(&s).is_empty() { 0 } else { 1 };
        s.screen = distant_idle_at(row);
    }
    crate::backend::render::gallery_anim_reset();
}

/// Error screen (Y): re-type the URL that just failed, pre-filled, then retry it
/// straight away. If the bad URL was already in the history it's UPDATED in
/// place (rather than leaving the broken one behind next to the fixed one).
/// Called from `input()` only — swkbd + the retry fetch need the lock free.
fn run_fix_url_flow(failed: &str) {
    let Some(fixed) = net::prompt_url_with_initial(Some(failed)) else {
        return; // cancelled — stay on the error notice
    };
    let fixed = fixed.trim().to_string();
    if fixed.is_empty() {
        return;
    }
    let row = {
        let Ok(mut s) = LIBRARY.lock() else { return };
        // Leave the error screen behind: clear the notice AND the half-finished
        // download bookkeeping, exactly like the A/B dismiss path does.
        s.distant_error.clear();
        s.failed_url.clear();
        clear_download_state(&mut s);
        if let Some(pos) = s.url_history.iter().position(|u| u == failed) {
            s.url_history[pos] = fixed.clone();
            if s.history_loaded {
                let snapshot = s.url_history.clone();
                save_history_to_sd(&snapshot);
            }
            importer_row_for_abs(&s, pos)
        } else {
            0
        }
    };
    run_fetch_for_url(
        &fixed,
        row,
        clamp_scroll(
            IMPORT_SCROLL.load(std::sync::atomic::Ordering::Relaxed),
            row,
            IMPORTER_VISIBLE_ROWS,
        ),
    );
}

/// DistantUrlOptions > edit: swkbd prefilled with the existing URL. Commit
/// replaces it (empty input deletes it), persists, returns to the list. Called
/// from `input()` only.
fn run_edit_url_flow(url_idx: usize) {
    let prefill = LIBRARY
        .lock()
        .ok()
        .and_then(|s| s.url_history.get(url_idx).cloned());
    let Some(prefill) = prefill else { return };
    let Some(new_url) = net::prompt_url_with_initial(Some(&prefill)) else {
        return; // cancelled — leave history untouched
    };
    let new_url = new_url.trim().to_string();
    if let Ok(mut s) = LIBRARY.lock() {
        if url_idx < s.url_history.len() {
            if new_url.is_empty() {
                s.url_history.remove(url_idx);
            } else {
                s.url_history[url_idx] = new_url;
            }
        }
        if s.history_loaded {
            let snapshot = s.url_history.clone();
            save_history_to_sd(&snapshot);
        }
        s.screen = distant_idle_at(importer_row_for_abs(&s, url_idx));
    }
}

/// Fetch the given URL and transition state. Used both by the post-swkbd
/// path (`run_url_fetch_flow`) and by the ZR re-fetch-without-swkbd path.
/// v1.2.0: routes by source shape so the IMPORTER is "globalisable" — an
/// archive.org URL/item-id goes through the metadata file list, a direct
/// `.swf` URL downloads straight to SD.
fn run_fetch_for_url(url: &str, source_sel: usize, source_scroll: usize) {
    // Remember what we're about to try: if it blows up, the error screen offers
    // Y = re-type THIS url instead of sending the user back to the list. Cleared
    // as soon as the URL proves good (metadata parsed / download finished).
    if let Ok(mut s) = LIBRARY.lock() {
        s.failed_url = url.to_string();
    }
    match crate::sources::classify(url) {
        crate::sources::SourceKind::DirectUrl => run_direct_download(url),
        crate::sources::SourceKind::ArchiveOrg => {
            run_archive_fetch_async(url, source_sel, source_scroll)
        }
    }
}

/// Direct `.swf` URL import: download straight to SD, no metadata list. The
/// filename is derived from the URL's last path segment (query/fragment
/// stripped); a re-download just overwrites + refreshes the entry.
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

fn run_direct_download(url: &str) {
    // Wayback URLs need the `id_` modifier to serve the raw file instead of the
    // HTML page wrapper (e.g. .../web/<ts>id_/...mting.swf). No-op otherwise.
    let url = crate::sources::wayback_raw(url);
    let url = url.as_str();
    let safe_name = safe_name_from_url(url);
    // Already on SD? Skip the re-download (the game is playable from JOUER),
    // mirroring the Flashpoint grid's A-press guard.
    if let Ok(s) = LIBRARY.lock() {
        if s.entries.iter().any(|e| e.basename == safe_name) {
            log(&std::format!("library: direct download skip - {} deja sur SD\n", safe_name));
            return;
        }
    }
    let out_path = std::format!("{}/{}", USER_SD_ROOTS[0], safe_name);
    match net::start_download(url, &out_path) {
        Ok(()) => {
            push_history(url);
            if let Ok(mut s) = LIBRARY.lock() {
                s.download_file_name = safe_name;
                s.download_out_path = out_path;
                s.download_source_url = url.to_string();
                s.download_resume_pos = None;
                s.screen = Screen::DistantDownloading;
            }
        }
        Err(e) => set_distant_error(&e),
    }
}

/// IMPORTER > X: search Flashpoint for Flash games by name and open the cover
/// gallery (`FpGallery`) of the hits. Hoisted from `input()` — runs swkbd + a
/// synchronous HTTPS search WITHOUT the LIBRARY lock. On A in the gallery the
/// user downloads the selected game's GameZIP (see `handle_fp_gallery_input`).
/// Reuses `cover_candidates`/`cover_msg`/`cover_query` (the cover-picker state);
/// pre-fills the keyboard with the last query so re-searching to refine is easy.
fn run_fp_search_flow() {
    let (initial, filter) = LIBRARY
        .lock()
        .ok()
        .map(|s| (s.cover_query.clone(), s.fp_content_filter))
        .unwrap_or((std::string::String::new(), true));
    let Some(query) = net::prompt_search(&initial) else {
        return;
    };
    let query = query.trim().to_string();
    if query.is_empty() {
        return;
    }
    // Start the search ASYNC (was a blocking `gamezip::search` that froze the UI
    // for a second or two). Flip straight to the FpGallery with `fp_loading` set:
    // its render arm shows a spinner and polls `net::tick_get_async`, then fills
    // the grid. Sorting + the "no hits" message move there too.
    log(&std::format!("library: flashpoint game search \"{}\" (async)\n", query));
    // Drop any thumbnail fetch still in flight from a previous gallery so the
    // isolated handle starts clean.
    crate::backend::render::thumb_cancel_all();
    // Fresh result set (selection jumps to 0): snap the gallery glide to the top
    // so it doesn't streak from the previous gallery's scroll (shared anim).
    crate::backend::render::gallery_anim_reset();
    match net::start_get_async(&crate::sources::gamezip::search_url(&query, filter)) {
        Ok(()) => {
            if let Ok(mut s) = LIBRARY.lock() {
                s.cover_candidates = std::vec::Vec::new();
                s.cover_msg = std::string::String::new();
                s.cover_query = query;
                s.fp_loading = true;
                s.screen = Screen::FpGallery { selection: 0, scroll: 0 };
            }
        }
        Err(e) => {
            if let Ok(mut s) = LIBRARY.lock() {
                s.cover_candidates = std::vec::Vec::new();
                s.cover_msg = e;
                s.cover_query = query;
                s.fp_loading = false;
                s.screen = Screen::FpGallery { selection: 0, scroll: 0 };
            }
        }
    }
}

/// Hidden ZL+ZR chord in the Flashpoint results grid: flip the session content
/// filter and re-run the LAST query, no keyboard. Lets the importer reach the
/// mature-rated catalogue (issue #33) without ever surfacing the option in
/// RÉGLAGES. Hoisted from `input()` — `start_get_async` must run without the
/// LIBRARY lock, mirroring `run_fp_search_flow`. No-op if a search is already in
/// flight or nothing has been searched yet.
fn run_fp_filter_toggle_flow() {
    let (query, filter) = {
        let Ok(mut s) = LIBRARY.lock() else { return };
        if s.fp_loading {
            return;
        }
        s.fp_content_filter = !s.fp_content_filter;
        (s.cover_query.clone(), s.fp_content_filter)
    };
    if query.trim().is_empty() {
        return;
    }
    log(&std::format!(
        "library: flashpoint filter toggle -> filter={} (re-search \"{}\")\n",
        filter, query
    ));
    crate::backend::render::thumb_cancel_all();
    // Fresh result set (selection jumps to 0): snap the gallery glide to the top
    // so it doesn't streak from the previous gallery's scroll (shared anim).
    crate::backend::render::gallery_anim_reset();
    match net::start_get_async(&crate::sources::gamezip::search_url(&query, filter)) {
        Ok(()) => {
            if let Ok(mut s) = LIBRARY.lock() {
                s.cover_candidates = std::vec::Vec::new();
                s.cover_msg = std::string::String::new();
                s.fp_loading = true;
                s.screen = Screen::FpGallery { selection: 0, scroll: 0 };
            }
        }
        Err(e) => {
            if let Ok(mut s) = LIBRARY.lock() {
                s.cover_candidates = std::vec::Vec::new();
                s.cover_msg = e;
                s.fp_loading = false;
                s.screen = Screen::FpGallery { selection: 0, scroll: 0 };
            }
        }
    }
}

/// archive.org item import (async): start the metadata fetch + open the reveal
/// window (with a spinner) from the launched row. `render`'s DistantLoading arm
/// polls `net::tick_archive_fetch` each frame and switches to DistantFiles on
/// success (no UI freeze). `source_sel`/`source_scroll` are the history row the
/// window grows from and the scroll the list was at.
fn run_archive_fetch_async(url: &str, source_sel: usize, source_scroll: usize) {
    let Some(item_id) = net::extract_item_id(url) else {
        set_distant_error(crate::loc::s().err_url_invalid);
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
        Err(e) => set_distant_error(&e),
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
        "Up" | "StickLUp" => {
            if total == 0 { return; }
            selection = if selection == 0 { last } else { selection - 1 };
            scroll = clamp_scroll(scroll, selection, DISTANT_VISIBLE_ROWS);
        }
        "Down" | "StickLDown" => {
            if total == 0 { return; }
            selection = if selection >= last { 0 } else { selection + 1 };
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
            let safe_name: std::string::String = file
                .name
                .chars()
                .map(|c| if matches!(c, '/' | '\\') { '_' } else { c })
                .collect();
            let out_path = std::format!("{}/{}", USER_SD_ROOTS[0], safe_name);
            // If the file is already on SD (entry exists from boot scan),
            // block the download entirely — the OK badge is the signal,
            // re-downloading would just waste bandwidth + overwrite the
            // file. To play it, the user backs out to LOCAL (Y) and hits
            // A on the same file from there. Silent no-op = no popup
            // noise during list navigation.
            if s.entries.iter().any(|e| e.basename == safe_name) {
                log(&std::format!(
                    "library: A ignore — {} deja sur SD (bascule en LOCAL pour jouer)\n",
                    safe_name,
                ));
                return;
            }
            match net::start_download(&file.download_url, &out_path) {
                Ok(()) => {
                    s.download_file_name = file.name.clone();
                    s.download_out_path = out_path;
                    s.download_source_url = file.download_url.clone();
                    // Remember where the cursor was so we can put it back
                    // after the download finishes (otherwise the user has
                    // to rescroll through 1000s of entries to find their
                    // place).
                    s.download_resume_pos = Some((selection, scroll));
                    s.screen = Screen::DistantDownloading;
                }
                Err(e) => {
                    s.distant_error = e;
                    s.screen = Screen::DistantError;
                }
            }
            return;
        }
        "Y" => {
            // Sort picker for the archive.org file list (name / size).
            s.screen = Screen::RemoteSortModal {
                selection: 0,
                kind: SORT_TARGET_FILES,
                reverse: false,
                prev_sel: selection,
                prev_scroll: scroll,
            };
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
        Some(q) => files
            .iter()
            .enumerate()
            .filter(|(_, f)| f.name.to_lowercase().contains(&q))
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
) -> std::vec::Vec<usize> {
    let needle = filter
        .as_deref()
        .map(|s| s.trim().to_lowercase())
        .filter(|s| !s.is_empty());
    match needle {
        None => (0..entries.len()).collect(),
        Some(q) => entries
            .iter()
            .enumerate()
            .filter(|(_, e)| {
                e.display_name.to_lowercase().contains(&q)
                    || e.basename.to_lowercase().contains(&q)
            })
            .map(|(i, _)| i)
            .collect(),
    }
}

/// Build a `Screen::List` positioned on the entry at ABSOLUTE index `abs`,
/// translated into the current filtered view so sub-screens (OPTIONS, etc.)
/// can return to the right row WITHOUT clearing the active filter. Falls back
/// to row 0 if `abs` isn't in the filtered set.
fn list_screen_for_abs(s: &State, abs: usize) -> Screen {
    let filtered = local_filtered_indices(&s.entries, &s.local_filter);
    let pos = filtered.iter().position(|&i| i == abs).unwrap_or(0);
    // Gallery scroll is ROW-based, not a linear item index — use the published
    // layout so returning from OPTIONS lands the game's ROW on screen. (Bug
    // fix: clamp_scroll's linear value was read as a row index → games on row
    // 2/3 came back scrolled past the end, showing a blank screen.)
    let scroll = gallery_scroll_for(pos, jouer_scroll());
    Screen::List { selection: pos, scroll_offset: scroll }
}

/// Set the DistantError state + message from anywhere (used after FFI
/// returns where we don't already hold the lock).
fn set_distant_error(msg: &str) {
    if let Ok(mut s) = LIBRARY.lock() {
        s.distant_error = msg.to_string();
        s.screen = Screen::DistantError;
    }
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

fn handle_options_input(s: &mut State, button: &str, game_idx: usize, mut selection: usize) {
    let last = OPTIONS_ENTRIES.len().saturating_sub(1);
    match button {
        "Up" | "StickLUp" => {
            selection = if selection == 0 { last } else { selection - 1 };
        }
        "Down" | "StickLDown" => {
            selection = if selection >= last { 0 } else { selection + 1 };
        }
        "A" => {
            match OPTIONS_ENTRIES[selection] {
                "FAVORI" => {
                    // Toggle, then re-pin: favorites jump to the top of the
                    // gallery, so this game's index changes. Re-point the modal
                    // at the SAME game (by basename) so it keeps showing it.
                    if let Some(basename) = s.entries.get(game_idx).map(|e| e.basename.clone()) {
                        crate::favorites::toggle(&basename);
                        sort_entries(&mut s.entries, current_sort_mode(), current_sort_reverse());
                        let new_idx = s
                            .entries
                            .iter()
                            .position(|e| e.basename == basename)
                            .unwrap_or(game_idx);
                        s.screen = Screen::OptionsModal { game_idx: new_idx, selection };
                    }
                    return;
                }
                "TOUCHES" => {
                    // Open the TOUCHES sub-menu (edit / apply / share / revert).
                    // #20 regroup: apply+share used to be sibling OPTIONS rows.
                    goto_touches_menu(s, game_idx, 0);
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
                "SUPPRIMER" => {
                    s.screen = Screen::DeleteConfirm { game_idx };
                    return;
                }
                _ => {}
            }
        }
        "B" | "Minus" => {
            s.screen = list_screen_for_abs(s, game_idx);
            return;
        }
        _ => {}
    }
    s.screen = Screen::OptionsModal { game_idx, selection };
}

/// Build a Flashpoint search query from a game's display name: drop a trailing
/// `.swf`, turn `_`/`-` into spaces so "Super_Mario_63" matches "Super Mario 63".
fn cover_query_from_name(name: &str) -> std::string::String {
    let base = if name.len() > 4 && name[name.len() - 4..].eq_ignore_ascii_case(".swf") {
        &name[..name.len() - 4]
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
                s.screen = Screen::OptionsModal { game_idx, selection: 3 };
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
            if selection >= cols {
                selection -= cols;
            }
        }
        "Down" | "StickLDown" => {
            if selection + cols < total {
                selection += cols;
            }
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
            s.screen = Screen::OptionsModal { game_idx, selection: 3 };
            return;
        }
        _ => {}
    }
    s.screen = Screen::CoverPicker { game_idx, selection };
}

/// Flashpoint gallery (IMPORTER > X) navigation + download. A cover grid of the
/// search hits; A downloads the selected game's GameZIP (async — then
/// `on_download_finished` unzips it), B returns to the IMPORTER list. Called
/// under the LIBRARY lock; `net::start_download` is non-blocking so that's fine.
fn handle_fp_gallery_input(s: &mut State, button: &str, mut selection: usize, mut scroll: usize) {
    // Async search still loading (spinner showing): ignore navigation; B/Minus
    // cancels the in-flight fetch and returns to the importer.
    if s.fp_loading {
        if matches!(button, "B" | "Minus") {
            net::cancel_archive_fetch();
            s.fp_loading = false;
            s.screen = distant_idle_at(0);
        }
        return;
    }
    let total = s.cover_candidates.len();
    let cols = FP_GALLERY_COLS;
    let rows_visible = FP_GALLERY_ROWS;
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
            if selection >= cols {
                selection -= cols;
            }
        }
        "Down" | "StickLDown" => {
            if selection + cols < total {
                selection += cols;
            }
        }
        "A" => {
            let Some(cand) = s.cover_candidates.get(selection).cloned() else {
                return;
            };
            let swf_name = crate::sources::gamezip::swf_filename(&cand.title);
            // Already on SD → silent no-op (play it from JOUER instead).
            if s.entries.iter().any(|e| e.basename == swf_name) {
                log(&std::format!("library: A ignore — {} deja sur SD\n", swf_name));
                return;
            }
            let swf_path = std::format!("{}/{}", USER_SD_ROOTS[0], swf_name);
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
                s.distant_error = crate::loc::s().err_fp_html_game.to_string();
                s.screen = Screen::DistantError;
                return;
            }
            let (url, out_path, fp_direct) = if cand.zipped {
                let zip_path = std::format!("{}/.fpdl.zip", USER_SD_ROOTS[0]);
                (crate::sources::gamezip::get_url(&cand.id), zip_path, false)
            } else {
                match crate::sources::gamezip::htdocs_url_from_command(&cand.launch_command) {
                    Some(u) => (u, swf_path.clone(), true),
                    None => {
                        // parse_search only keeps non-zipped games with a usable
                        // command, so this is unexpected — surface an error rather
                        // than a silent no-op.
                        s.distant_error = crate::loc::s().err_no_swf.to_string();
                        s.screen = Screen::DistantError;
                        return;
                    }
                }
            };
            match net::start_download(&url, &out_path) {
                Ok(()) => {
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
                    s.screen = Screen::DistantDownloading;
                }
                Err(e) => {
                    s.distant_error = e;
                    s.screen = Screen::DistantError;
                }
            }
            return;
        }
        "Y" => {
            // Sort picker for the Flashpoint results (name only — devs unknown).
            s.screen = Screen::RemoteSortModal {
                selection: 0,
                kind: SORT_TARGET_FP,
                reverse: false,
                prev_sel: selection,
                prev_scroll: scroll,
            };
            return;
        }
        "B" => {
            crate::backend::render::thumb_cancel_all();
            s.cover_candidates.clear();
            s.cover_msg.clear();
            s.cover_query.clear();
            s.screen = distant_idle_at(0);
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

/// FpGallery `+`: show the details popup for the selected game. Snapshots the
/// candidate under the lock, then (without the lock) does a blocking HEAD to
/// read the download size, and opens `FpDetails`. Called from `input()` only.
/// Size 0 = the probe failed (shown as "?" in the popup).
fn run_fp_info_flow(selection: usize, scroll: usize) {
    let cand = match LIBRARY.lock() {
        Ok(g) => g
            .cover_candidates
            .get(selection)
            .map(|c| (c.id.clone(), c.zipped, c.launch_command.clone())),
        Err(_) => None,
    };
    let Some((id, zipped, launch_command)) = cand else { return };
    // Probe the URL the download will ACTUALLY use, mirroring the choice the
    // `A` branch of `handle_fp_gallery_input` makes: the GameZIP server for a
    // zipped game, the htdocs mirror for a legacy "loose" one. Probing
    // `/get?id=` for a legacy entry always 404s, so those games used to show
    // "?" for a size we can in fact read. `parse_search` drops non-zipped
    // entries whose launchCommand gives no htdocs URL, so a legacy game on
    // screen always has one.
    let url = if zipped {
        Some(crate::sources::gamezip::get_url(&id))
    } else {
        crate::sources::gamezip::htdocs_url_from_command(&launch_command)
    };
    let size = url.as_deref().and_then(net::head_content_length).unwrap_or(0);
    // The game's blurb, fetched per-game (see `fetch_description`). Best-effort:
    // an empty result just means the popup shows the facts without the text.
    let desc = crate::sources::gamezip::fetch_description(&id);
    if let Ok(mut s) = LIBRARY.lock() {
        // Only open if we're still on the gallery (user might have navigated
        // away during the HEAD).
        if matches!(s.screen, Screen::FpGallery { .. }) {
            s.fp_desc = desc;
            s.screen = Screen::FpDetails { selection, scroll, size };
        }
    }
}

/// JOUER sort picker (Y) input: Up/Down move, A applies + persists + re-sorts,
/// B cancels. After a sort change the list reorders, so the cursor resets to top.
fn handle_sort_modal_input(
    s: &mut State,
    button: &str,
    mut selection: usize,
    prev_sel: usize,
    prev_scroll: usize,
) {
    const N: usize = 5;
    match button {
        "Up" | "StickLUp" => {
            if selection > 0 {
                selection -= 1;
            }
        }
        "Down" | "StickLDown" => {
            if selection + 1 < N {
                selection += 1;
            }
        }
        "X" => {
            // Toggle the sort direction in place: flip + persist + re-sort now.
            // The list reorders, so a later B can't restore a meaningful cursor —
            // park prev at the top.
            set_sort_reverse(!current_sort_reverse());
            sort_entries(&mut s.entries, current_sort_mode(), current_sort_reverse());
            s.screen = Screen::SortModal { selection, prev_sel: 0, prev_scroll: 0 };
            return;
        }
        "A" => {
            // Applied a new sort → the list reorders, so land on the top.
            set_sort_mode(selection as u8);
            sort_entries(&mut s.entries, current_sort_mode(), current_sort_reverse());
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
    s.screen = Screen::SortModal { selection, prev_sel, prev_scroll };
}

/// Which list a `RemoteSortModal` sorts.
pub(crate) const SORT_TARGET_FILES: u8 = 0; // archive.org file list
pub(crate) const SORT_TARGET_FP: u8 = 1; // Flashpoint result gallery
pub(crate) const SORT_TARGET_IMPORT: u8 = 2; // IMPORTER home (saved URLs)

/// Number of criteria in the IMPORTER sort picker, per target. Flashpoint
/// exposes only NAME (developers are unknown — removed); archive.org exposes
/// NAME + SIZE; the saved-URL list exposes ADDED / NAME / SOURCE. Direction
/// (asc/desc) is a separate X toggle, not a criterion.
fn remote_sort_count(kind: u8) -> usize {
    match kind {
        SORT_TARGET_FP => 1,
        SORT_TARGET_IMPORT => IMPORT_SORT_MODES,
        _ => 2,
    }
}

/// IMPORTER sort picker (Y). `kind` selects the target list + option set (see
/// `SORT_TARGET_*`). X toggles direction; A applies + returns to the list
/// (cursor top), B restores. The file/gallery sorts reorder their in-memory vec
/// and are forgotten with it; the saved-URL sort is a persisted PREFERENCE
/// (`url_history` keeps its insertion order — that's the ADDED criterion).
fn handle_remote_sort_modal_input(
    s: &mut State,
    button: &str,
    mut selection: usize,
    kind: u8,
    reverse: bool,
    prev_sel: usize,
    prev_scroll: usize,
) {
    let n = remote_sort_count(kind);
    match button {
        "Up" | "StickLUp" => {
            if selection > 0 {
                selection -= 1;
            }
        }
        "Down" | "StickLDown" => {
            if selection + 1 < n {
                selection += 1;
            }
        }
        "X" => {
            // Flip the direction indicator (applied on A). Stays in the modal.
            s.screen = Screen::RemoteSortModal {
                selection,
                kind,
                reverse: !reverse,
                prev_sel,
                prev_scroll,
            };
            return;
        }
        "A" => {
            match kind {
                SORT_TARGET_FP => {
                    // Only NAME (A-Z); X gives Z-A.
                    s.cover_candidates
                        .sort_by(|a, b| a.title.to_lowercase().cmp(&b.title.to_lowercase()));
                    if reverse {
                        s.cover_candidates.reverse();
                    }
                    // Re-ordered list: snap the glide to the top (no streak).
                    crate::backend::render::gallery_anim_reset();
                    s.screen = Screen::FpGallery { selection: 0, scroll: 0 };
                }
                SORT_TARGET_IMPORT => {
                    // Persist the choice: `importer_view` reads it every frame,
                    // so the list is already re-ordered when we land back on it.
                    set_import_sort(selection as u8, reverse);
                    crate::backend::render::gallery_anim_reset();
                    // Cursor on the first URL row (row 0 is "+ add").
                    let row = if importer_view(s).is_empty() { 0 } else { 1 };
                    s.screen = distant_idle_at(row);
                }
                _ => {
                    if selection == 1 {
                        // SIZE: biggest first by default.
                        s.remote_files.sort_by(|a, b| {
                            b.size_bytes
                                .cmp(&a.size_bytes)
                                .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
                        });
                    } else {
                        s.remote_files
                            .sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
                    }
                    if reverse {
                        s.remote_files.reverse();
                    }
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
                entry.basename.clone()
            } else {
                new_name_trimmed
            };
        }
    }
}

/// Handles the destructive SUPPRIMER confirmation screen. A = run the
/// delete + back to List (with selection clamped to the now-shorter list).
/// B / Minus = cancel + back to OptionsModal.
fn handle_delete_confirm_input(s: &mut State, button: &str, game_idx: usize) {
    match button {
        "A" => {
            // Defer the delete until the close-out finishes: stash the action,
            // start the scale-out, and leave the screen on DeleteConfirm so its
            // panel stays populated while it shrinks. render() runs the delete
            // + lands on List (or Empty) once the pop is done.
            if let Ok(mut p) = PENDING_AFTER_CLOSE.lock() {
                *p = Some(PendingClose::DeleteGame { game_idx });
            }
            crate::backend::render::modal_close_begin();
        }
        "B" | "Minus" => {
            // Cancel back to OPTIONS (modal -> modal: instant swap, OPTIONS pops).
            s.screen = Screen::OptionsModal { game_idx, selection: 0 };
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
    let map = std::format!("{}/{}.solmap", USER_SD_ROOTS[0], basename);
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
        let path = std::format!("{}/{}", USER_SD_ROOTS[0], name);
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
    // Saves this game was seen reading under the OLD movie-keyed name. The C++
    // sweep matches `<basename>*` and can never reach them — that is why deleting
    // Agent P Strikes Back left its save behind, ready to be picked up again on
    // reinstall. Removed here, BEFORE the sweep, so the sweep still takes the
    // `.solmap` itself (it matches the prefix).
    delete_recorded_legacy_saves(&entry.basename);
    let mut path_c = entry.path.as_bytes().to_vec();
    path_c.push(0);
    let rc = unsafe {
        swf_picker_delete_game(path_c.as_ptr() as *const core::ffi::c_char)
    };
    // The C++ scan only sees the .swf's own directory; clean up the cached
    // cover (covers/ subdir) and stem-named sidecars it can't reach, then
    // commit so the unlinks (C++ + Rust) actually persist to the SD card.
    let extra = crate::covers::remove_for(&entry.basename);
    // Drop the renderer's cached cover TEXTURE (keyed by basename) too: deleting
    // the file alone left a re-import of the same name showing the old cover
    // straight from GPU memory. Mirrors run_cover_fetch_flow's invalidate.
    crate::backend::render::invalidate_cover(&entry.basename);
    crate::sd::commit();
    log(&std::format!(
        "library: SUPPRIMER {} -> rc={} (+{} cover/sidecar)\n",
        entry.path, rc, extra,
    ));
    s.entries.remove(game_idx);
    // Drop it from favorites too, so a re-import of the same name doesn't come
    // back pre-starred.
    crate::favorites::remove(&entry.basename);
    // Clear the session "downloaded" mark too. The IMPORTER green OK badge is a
    // union of `entries` (updated just above) and this set; a same-session
    // download lingering here kept the badge lit after a delete, while the
    // A-press "already on SD" check (entries-only) disagreed and re-downloaded.
    s.downloaded_basenames.retain(|n| n != &entry.basename);
}

extern "C" {
    fn swf_picker_delete_game(swf_path: *const core::ffi::c_char) -> core::ffi::c_int;
    fn swf_picker_count_companions(swf_path: *const core::ffi::c_char) -> core::ffi::c_int;
    // Cursor-speed preset (main.cpp): cycle to the next preset (returns the new
    // index), and read the current multiplier x10 (5,10,15,20,25) for the label.
    pub(crate) fn ruffle_cursor_speed_cycle() -> core::ffi::c_int;
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
    let Some(typed) = net::prompt_search(&current) else {
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

/// LOCAL list search (X) — same behaviour as the DISTANT filter: opens swkbd
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
    let Some(typed) = net::prompt_search(&current) else {
        return; // cancelled
    };
    let trimmed = typed.trim().to_string();
    if let Ok(mut s) = LIBRARY.lock() {
        s.local_filter = if trimmed.is_empty() { None } else { Some(trimmed) };
        s.screen = Screen::List { selection: 0, scroll_offset: 0 };
    }
    // Filtered view = brand-new layout; snap the glide to the top.
    crate::backend::render::gallery_anim_reset();
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

/// Screen rect of history row `sel` in the IMPORTER list (mirrors the layout in
/// `draw_library_distant_list`: top=160, row_h=50, 10 visible — keep the two in
/// step). Used as the expand/collapse reveal's grow-from / shrink-to box.
fn distant_row_rect(sel: usize, scroll: usize, vw: f32) -> (f32, f32, f32, f32) {
    const TOP: f32 = 160.0;
    const ROW_H: f32 = 50.0;
    // The list scrolls freely now (touch drag), so the row's screen position
    // comes from the ACTUAL scroll offset, not one re-derived from `sel`.
    let row_y = TOP + (sel as f32 - scroll as f32) * ROW_H;
    let row_y = row_y.clamp(TOP, TOP + (IMPORTER_VISIBLE_ROWS - 1) as f32 * ROW_H);
    (20.0, row_y - 8.0, vw - 40.0, ROW_H + 4.0)
}

/// Stable id per panel/modal screen (0 = not a modal). Used by `render` to fire
/// the scale-in "pop" exactly once when a modal is first shown. Full-screen
/// notices (AppletNotice / DistantError / Downloading) are NOT modals here — no
/// panel to scale. The TOUCHES editors ARE modals (scale in like the rest).
fn modal_kind(screen: Screen) -> u8 {
    match screen {
        Screen::OptionsModal { .. } => 1,
        Screen::DeleteConfirm { .. } => 2,
        Screen::CoverPicker { .. } => 3,
        Screen::SettingsLanguagePicker { .. } => 4,
        Screen::SettingsPrefsModal { .. } => 10,
        Screen::DistantUrlOptions { .. } => 5,
        Screen::DistantHistoryConfirm => 6,
        // The keymap editor delegates to `menu::*`; fold its sub-screen id in so a
        // List -> Keyboard (etc.) transition CHANGES the kind and re-fires the
        // scale-in "pop", exactly like the in-game `ruffle_touches_draw` path.
        // Distinct 70-79 / 80-89 bands so they never collide with the ids above.
        Screen::TouchesEditor { .. } => 70 + menu::screen_kind(),
        Screen::SettingsKeymapEditor => 80 + menu::screen_kind(),
        Screen::SortModal { .. } => 9,
        Screen::RemoteSortModal { .. } => 10,
        Screen::FpDetails { .. } => 11,
        // #20 profile modals: distinct ids so each gets the scale-in "pop" when it
        // appears (they were kind 0 = no animation, unlike every other modal).
        Screen::TouchesMenu { .. } => 12,
        Screen::ProfileList { .. } => 13,
        Screen::ProfilePreview { .. } => 14,
        Screen::RevertPreview { .. } => 15,
        Screen::ProfileShareConfirm { .. } => 16,
        Screen::ProfileDeleteConfirm { .. } => 17,
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
    DeleteHistory,
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
    let label = LIBRARY
        .lock()
        .ok()
        .and_then(|s| s.entries.get(game_idx).map(|e| e.display_name.clone()))
        .unwrap_or_default();
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
fn modal_close_deferred(kind: u8) -> bool {
    matches!(kind, 1 | 3 | 4 | 5 | 9 | 10 | 11)
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
        let idx = local_filtered_indices(&s.entries, &s.local_filter);
        let entries: std::vec::Vec<Entry> = idx.iter().map(|&i| s.entries[i].clone()).collect();
        (
            LibraryListSnapshot {
                entries,
                banner_tex: s.banner_tex,
                banner_w: s.banner_w,
                banner_h: s.banner_h,
            },
            s.local_filter.clone(),
            s.entries.len(),
        )
    });
    if let Some((snap, filter, total_unfiltered)) = snapshot {
        let phase_ticks = unsafe { ruffle_tick_now() }.saturating_sub(anim_origin);
        // Same data, same selection, same actions — only the layout differs
        // (REGLAGES > AFFICHAGE HOME). Each one publishes the same cell table, so
        // navigation, touch and the launch reveal are shared.
        let a = (
            selection,
            scroll_offset,
            &snap.entries,
            snap.banner_tex,
            snap.banner_w,
            snap.banner_h,
            phase_ticks,
            filter.as_deref(),
            total_unfiltered,
        );
        match crate::loc::home_view() {
            1 => backend.draw_library_list_view(a.0, a.1, a.2, a.3, a.4, a.5, a.6, a.7, a.8),
            // BANDE is now the big-cover-plus-full-width-row layout: it was built as
            // ETAGERE, and reads better as the strip than the strip did. The old
            // centred-cover strip is gone rather than kept as a near-duplicate.
            2 => backend.draw_library_shelf_view(a.0, a.1, a.2, a.3, a.4, a.5, a.6, a.7, a.8),
            3 => backend.draw_library_etagere_view(a.0, a.1, a.2, a.3, a.4, a.5, a.6, a.7, a.8),
            _ => backend.draw_library_gallery(a.0, a.1, a.2, a.3, a.4, a.5, a.6, a.7, a.8),
        }
    }
}

/// Draw the launch/quit reveal window for openness `frac` (0 = tile, 1 = full):
/// the game's cover clipped to a window that opens/closes between the two, plus
/// the bright border + dim chrome so the rectangle reads over the gallery behind.
fn draw_game_reveal_window(backend: &mut SwitchRenderBackend, frac: f32, fade: f32) {
    let (vw, vh) = backend.screen_size();
    let ((rx, ry, rw, rh), basename, name, color) = crate::backend::render::game_reveal_info();
    let wx = rx * (1.0 - frac);
    let wy = ry * (1.0 - frac);
    let ww = rw + (vw - rw) * frac;
    let wh = rh + (vh - rh) * frac;
    backend.set_clip(wx, wy, ww, wh);
    backend.draw_game_reveal_tile(0.0, 0.0, vw, vh, &basename, &name, color);
    // Launch fade-to-black: a black veil over the (full-screen) cover so the game
    // pops calmly out of the dark instead of replacing the cover in one frame.
    if fade > 0.0 {
        let alpha = (fade.clamp(0.0, 1.0) * 255.0) as u32;
        backend.draw_overlay_rect(0.0, 0.0, vw, vh, alpha << 24);
    }
    backend.clear_clip();
    backend.draw_reveal_chrome(wx, wy, ww, wh);
    // Multi-file indicator (v1.3.0): once the cover fills the screen (the SWF is
    // loading behind it), flag that this game pulls in companion SWFs from its
    // `.files/` folder. Count is reset to 0 on the quit reveal, so launch-only.
    let companions = LAUNCH_COMPANIONS.load(std::sync::atomic::Ordering::Relaxed);
    if companions > 0 && frac >= 0.85 {
        backend.draw_multifile_badge(crate::loc::s().multifile, companions);
    }
}

/// Render the current screen using the backend. C++ calls this each frame
/// while the library is active, AFTER `glClear` (we own the entire
/// framebuffer — no Ruffle behind us at this stage).
pub fn render(backend: &mut SwitchRenderBackend) {
    // Run any deferred profile network flow now that its loading panel has shown
    // a frame (#20). May transition the screen, so do it before reading it.
    drive_pending_net();
    let s = match LIBRARY.lock() {
        Ok(g) => g,
        Err(_) => return,
    };
    let mut screen = s.screen;
    let anim_origin = s.anim_origin_ticks;
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
                        let new_sel = game_idx.min(s2.entries.len() - 1);
                        let scroll = gallery_scroll_for(new_sel, jouer_scroll());
                        s2.screen = Screen::List { selection: new_sel, scroll_offset: scroll };
                        // Layout shifted (a tile is gone) — snap the glide.
                        crate::backend::render::gallery_anim_reset();
                    }
                }
                Some(PendingClose::DeleteHistory) => {
                    let idx = s2.history_idx.unwrap_or(0);
                    delete_current_history(&mut s2);
                    // The deleted entry's absolute index now belongs to whatever
                    // shifted into its place; park the cursor on that row.
                    let row = importer_row_for_abs(&s2, idx);
                    s2.screen = distant_idle_at(row);
                }
                None => {}
            }
            screen = s2.screen;
        }
        if let Ok(mut last) = LAST_MODAL_KIND.lock() {
            *last = modal_kind(screen);
        }
    }
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

    // Phase = current tick - origin. Drives sin() animations. ms-resolution
    // is enough; we compute it lazily below to avoid an FFI call when the
    // current screen has no animation.
    match screen {
        Screen::Inactive | Screen::Picked | Screen::Quit => {}
        Screen::AppletNotice { .. } => {
            backend.draw_library_applet_notice(crate::loc::s().applet_notice);
        }
        Screen::Empty => {
            backend.draw_library_empty();
        }
        Screen::List { selection, scroll_offset } => {
            // Remember where the gallery is, so every path that comes BACK to it
            // restores this scroll instead of deriving one from the row.
            JOUER_SCROLL.store(scroll_offset, std::sync::atomic::Ordering::Relaxed);
            draw_gallery(backend, selection, scroll_offset, anim_origin);
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
            draw_gallery(backend, selection, scroll_offset, anim_origin);
            let done = match crate::backend::render::game_reveal_step(now) {
                Some((frac, fade, _collapsing, done)) => {
                    draw_game_reveal_window(backend, frac, fade);
                    done
                }
                None => true,
            };
            if done {
                if let Ok(mut s) = LIBRARY.lock() {
                    s.screen = Screen::Picked;
                }
            }
        }
        Screen::OptionsModal { game_idx, selection } => {
            let entry_snapshot = LIBRARY
                .lock()
                .ok()
                .and_then(|s| s.entries.get(game_idx).cloned());
            if let Some(entry) = entry_snapshot {
                // OPTIONS_ENTRIES stays the stable logical key list (matched
                // in input handling); display uses localized labels in the
                // SAME order. Must match OPTIONS_ENTRIES exactly (FAVORI /
                // TOUCHES / RENOMMER / JAQUETTE / SUPPRIMER) — apply+share moved
                // into the TOUCHES sub-menu (#20 regroup), so they're NOT here.
                let lc = crate::loc::s();
                let fav_label = if crate::favorites::is_favorite(&entry.basename) {
                    lc.opt_unfavorite
                } else {
                    lc.opt_favorite
                };
                let labels = [
                    fav_label,
                    lc.opt_keys,
                    lc.opt_rename,
                    lc.opt_cover,
                    lc.opt_delete,
                ];
                backend.draw_library_options(
                    &entry.display_name,
                    selection,
                    &labels,
                );
            }
        }
        Screen::TouchesEditor { .. } => {
            // Backdrop = Library list frozen behind. Cheapest path: redraw
            // a dim panel + delegate to menu::draw.
            backend.draw_library_dim_backdrop();
            menu::draw(backend, now);
        }
        Screen::SettingsModal { selection } => {
            // REGLAGES is a full navbar TAB now (not a popup): no dim backdrop,
            // no BACK entry — leave via L/R.
            let lc = crate::loc::s();
            // Nickname row shows the current value (or the "(none)" placeholder).
            let author = crate::profiles::author_name();
            let pseudo_label = std::format!(
                "{} : {}",
                lc.set_pseudo,
                if author.is_empty() { lc.none } else { author.as_str() },
            );
            let home_label = std::format!(
                "{}: {}",
                lc.set_home_view,
                crate::loc::home_view_label(crate::loc::home_view()),
            );
            let entries = [
                home_label.as_str(), lc.set_game_prefs, lc.set_language,
                lc.set_report_bug, lc.set_suggest, pseudo_label.as_str(), lc.set_quit,
            ];
            backend.draw_library_settings(selection, &entries);
        }
        Screen::SettingsPrefsModal { selection } => {
            // Game DEFAULTS. Each row carries its live value, same shape as the
            // in-game rows these mirror.
            let lc = crate::loc::s();
            let display_label = std::format!(
                "{}: {}",
                lc.set_display_mode,
                crate::loc::display_mode_label(crate::loc::default_display_mode()),
            );
            let filter_label = std::format!(
                "{}: {}",
                lc.set_screen_filter,
                crate::loc::screen_filter_label(crate::loc::default_screen_filter()),
            );
            let m = unsafe { ruffle_cursor_speed_mult_x10() };
            let cursor_label = std::format!("{}: x{}.{}", lc.set_cursor_speed, m / 10, m % 10);
            let labels = [
                lc.set_keys,
                display_label.as_str(),
                filter_label.as_str(),
                cursor_label.as_str(),
            ];
            backend.draw_library_dim_backdrop();
            backend.draw_library_options(lc.prefs_title, selection, &labels);
        }
        Screen::SettingsKeymapEditor => {
            // Same as TouchesEditor — the editor edits the global default
            // (keymap module was pointed there on entry).
            backend.draw_library_dim_backdrop();
            menu::draw(backend, now);
        }
        Screen::SettingsLanguagePicker { selection } => {
            // First open is slow: drawing the native-name list pulls in the shared
            // CJK font once. Show a full-size spinner (an overlay, NOT inside the
            // modal — so the modal's open animation doesn't shrink it) one frame;
            // the next frame draws the list (loading the font), then it's cached.
            use std::sync::atomic::Ordering;
            let loading = LANG_FONT_WARM.load(Ordering::Relaxed) == 0;
            LANG_FONT_WARM.store(if loading { 1 } else { 2 }, Ordering::Relaxed);
            backend.draw_library_dim_backdrop();
            if loading {
                backend.draw_loading_overlay(crate::loc::s().lang_title, now);
            } else {
                let names: std::vec::Vec<&str> =
                    crate::loc::PICKER_LANGS.iter().map(|l| l.picker_name()).collect();
                backend.draw_library_language_picker(selection, &names);
            }
        }
        Screen::BugPicker { selection, scroll_offset } => {
            let names = LIBRARY
                .lock()
                .ok()
                .map(|s| {
                    s.entries
                        .iter()
                        .map(|e| e.display_name.clone())
                        .collect::<std::vec::Vec<_>>()
                })
                .unwrap_or_default();
            let refs: std::vec::Vec<&str> = names.iter().map(|n| n.as_str()).collect();
            let lc = crate::loc::s();
            backend.draw_library_bug_picker(
                selection,
                scroll_offset,
                &refs,
                BUG_PICKER_VISIBLE_ROWS,
                lc.bug_pick_title,
                lc.bug_pick_footer,
            );
        }
        Screen::BugResult => {
            let (msg, ok) = LIBRARY
                .lock()
                .ok()
                .map(|s| (s.bug_msg.clone(), s.bug_ok))
                .unwrap_or_default();
            backend.draw_library_bug_result(&msg, ok);
        }
        Screen::TouchesMenu { game_idx, selection } => {
            let lc = crate::loc::s();
            let snap = LIBRARY.lock().ok().map(|s| {
                let game = s
                    .entries
                    .get(game_idx)
                    .map(|e| e.display_name.clone())
                    .unwrap_or_default();
                // Cursor row label, e.g. "Vitesse du curseur: x1.5" (unset → x1.0).
                let idx = if s.touches_cursor_idx < 0 {
                    1
                } else {
                    (s.touches_cursor_idx as usize).min(CURSOR_X10.len() - 1)
                };
                let x10 = CURSOR_X10[idx];
                let cursor = std::format!("{}: x{}.{}", lc.set_cursor_speed, x10 / 10, x10 % 10);
                // Show-cursor row label, e.g. "Afficher le curseur: AFFICHÉ".
                let show_cur = std::format!(
                    "{}: {}",
                    lc.show_cursor,
                    if s.touches_show_cursor { lc.cursor_shown } else { lc.cursor_hidden },
                );
                (game, cursor, show_cur, s.touches_can_revert, s.touches_has_backup)
            });
            if let Some((game, cursor, show_cur, can_revert, has_backup)) = snap {
                // Order MUST match the row indices in handle_touches_menu_input /
                // the input() dispatch: edit, apply, share, cursor, show-cursor,
                // (revert).
                let mut rows: std::vec::Vec<&str> =
                    std::vec![lc.touches_edit, lc.opt_apply, lc.opt_share, &cursor, &show_cur];
                if can_revert {
                    // Distinct label: restore my keys (a backup exists) vs reset
                    // to the default controls (none to restore).
                    rows.push(if has_backup {
                        lc.profile_revert
                    } else {
                        lc.touches_revert_default
                    });
                }
                backend.draw_library_list_modal(lc.opt_keys, &game, selection, &rows, lc.touches_footer, false);
            }
        }
        Screen::ProfileList { game_idx, selection } => {
            let lc = crate::loc::s();
            let snap = LIBRARY.lock().ok().map(|s| {
                let game = s
                    .entries
                    .get(game_idx)
                    .map(|e| e.display_name.clone())
                    .unwrap_or_default();
                let active = &s.active_profile_id;
                let rows: std::vec::Vec<std::string::String> = s
                    .profile_matches
                    .iter()
                    .map(|m| {
                        // Title, then the author's nickname (so several profiles
                        // for one game are distinguishable), then an "active" tag
                        // on the one currently applied.
                        let mut row = m.profile.title().to_string();
                        if !m.profile.author.is_empty() {
                            row.push_str(" - ");
                            row.push_str(&m.profile.author);
                        }
                        if !active.is_empty() && m.profile.id == *active {
                            row.push(' ');
                            row.push_str(lc.profile_active);
                        }
                        row
                    })
                    .collect();
                // Whether the highlighted row is one this install shared (drives
                // the "X: delete mine" footer hint).
                let sel_is_mine = s
                    .profile_matches
                    .get(selection)
                    .is_some_and(|m| crate::profiles::is_mine(&m.profile.id));
                (game, rows, sel_is_mine)
            });
            if let Some((game, mut rows, sel_is_mine)) = snap {
                if rows.is_empty() {
                    rows.push(lc.profile_none.to_string());
                }
                let refs: std::vec::Vec<&str> = rows.iter().map(|r| r.as_str()).collect();
                // Append the delete hint only when sitting on your own profile.
                let footer = if sel_is_mine {
                    std::format!("{}   {}", lc.profile_footer, lc.profile_del_hint)
                } else {
                    lc.profile_footer.to_string()
                };
                backend.draw_library_list_modal(
                    lc.profile_title,
                    &game,
                    selection,
                    &refs,
                    &footer,
                    true,
                );
            }
        }
        Screen::ProfileDeleteConfirm { profile_idx, .. } => {
            let lc = crate::loc::s();
            let name = LIBRARY
                .lock()
                .ok()
                .and_then(|s| {
                    s.profile_matches.get(profile_idx).map(|m| {
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
            // usize::MAX = no cursor (it's a confirm: A deletes / B cancels).
            backend.draw_library_list_modal(
                lc.profile_del_confirm,
                "",
                usize::MAX,
                &refs,
                lc.del_footer,
                true,
            );
        }
        Screen::ProfileLoading => {
            // Dim + loading panel (game name + spinner) shown for the frame before
            // the deferred GitHub call runs (#20). Same overlay helper as LANGUES.
            let label = NET_LOADING_LABEL.lock().ok().map(|l| l.clone()).unwrap_or_default();
            backend.draw_loading_overlay(&label, now);
        }
        Screen::ProfilePreview { .. } => {
            let lc = crate::loc::s();
            let rows = LIBRARY
                .lock()
                .ok()
                .map(|s| s.preview_rows.clone())
                .unwrap_or_default();
            let mut rows = rows;
            if rows.is_empty() {
                rows.push(lc.profile_preview_none.to_string());
            }
            let refs: std::vec::Vec<&str> = rows.iter().map(|r| r.as_str()).collect();
            // usize::MAX = no highlighted row (it's a read-only diff, A/B only).
            backend.draw_library_list_modal(
                lc.profile_preview_title,
                "",
                usize::MAX,
                &refs,
                lc.profile_preview_footer,
                true,
            );
        }
        Screen::RevertPreview { .. } => {
            let lc = crate::loc::s();
            let mut rows = LIBRARY
                .lock()
                .ok()
                .map(|s| s.preview_rows.clone())
                .unwrap_or_default();
            if rows.is_empty() {
                rows.push(lc.profile_preview_none.to_string());
            }
            let refs: std::vec::Vec<&str> = rows.iter().map(|r| r.as_str()).collect();
            backend.draw_library_list_modal(
                lc.revert_preview_title,
                "",
                usize::MAX,
                &refs,
                lc.revert_preview_footer,
                true,
            );
        }
        Screen::ProfileShareConfirm { .. } => {
            let lc = crate::loc::s();
            let (is_update, mut rows) = LIBRARY
                .lock()
                .ok()
                .map(|s| (s.share_is_update, s.preview_rows.clone()))
                .unwrap_or((false, std::vec::Vec::new()));
            // Make it explicit this UPDATES the player's one shared profile (vs
            // creates the first), and show the before/after diff under it.
            let subtitle = if is_update {
                lc.share_confirm_update
            } else {
                lc.profile_share_confirm
            };
            if rows.is_empty() {
                rows.push(lc.profile_preview_none.to_string());
            }
            let refs: std::vec::Vec<&str> = rows.iter().map(|r| r.as_str()).collect();
            // usize::MAX = no highlighted row (it's a confirm, A:share / B:cancel).
            backend.draw_library_list_modal(lc.opt_share, subtitle, usize::MAX, &refs, lc.lang_footer, true);
        }
        Screen::DeleteConfirm { game_idx } => {
            let snap = LIBRARY
                .lock()
                .ok()
                .and_then(|s| s.entries.get(game_idx).map(|e| (e.display_name.clone(), e.basename.clone())));
            if let Some((display_name, basename)) = snap {
                backend.draw_library_delete_confirm(&display_name, &basename);
            }
        }
        Screen::CoverPicker { game_idx, selection } => {
            // Snapshot candidate labels + notice + game name, drop the lock
            // before the GL draw.
            let snap = LIBRARY.lock().ok().map(|s| {
                let titles: std::vec::Vec<std::string::String> = s
                    .cover_candidates
                    .iter()
                    .map(|c| {
                        if c.developer.is_empty() {
                            c.title.clone()
                        } else {
                            std::format!("{} - {}", c.title, c.developer)
                        }
                    })
                    .collect();
                let urls: std::vec::Vec<std::string::String> =
                    s.cover_candidates
                        .iter()
                        .map(|c| cover_url_for_source(&c.cover_url, s.cover_use_shot))
                        .collect();
                let name = s
                    .entries
                    .get(game_idx)
                    .map(|e| e.display_name.clone())
                    .unwrap_or_default();
                (titles, urls, s.cover_msg.clone(), name, s.cover_use_shot)
            });
            if let Some((titles, urls, msg, name, use_shot)) = snap {
                let title_refs: std::vec::Vec<&str> = titles.iter().map(|x| x.as_str()).collect();
                let url_refs: std::vec::Vec<&str> = urls.iter().map(|x| x.as_str()).collect();
                let lc = crate::loc::s();
                // The hint names what Y GIVES you, so it reads as an action rather
                // than as a state you have to decode.
                let footer = std::format!(
                    "{}   {}",
                    lc.cover_footer,
                    if use_shot { lc.cover_show_logos } else { lc.cover_show_shots },
                );
                backend.draw_library_dim_backdrop();
                backend.draw_library_cover_picker(
                    &name, selection, &title_refs, &url_refs, &msg,
                    lc.cover_title, &footer,
                );
            }
        }
        Screen::FpGallery { selection, scroll } => {
            // Async search in flight: spinner + poll until the result list lands,
            // then fill the grid. Avoids the UI freeze the old blocking search had.
            let loading = LIBRARY.lock().ok().map(|s| s.fp_loading).unwrap_or(false);
            if loading {
                let (vw, vh) = backend.screen_size();
                let q = LIBRARY.lock().ok().map(|s| s.cover_query.clone()).unwrap_or_default();
                backend.draw_overlay_rect(0.0, 0.0, vw, vh, 0xFF_14_20_38);
                backend.draw_loading_panel(&q, now);
                match net::tick_get_async() {
                    net::GetPoll::Pending => {}
                    net::GetPoll::Done(bytes) => {
                        let (mut cands, msg) =
                            match crate::sources::gamezip::parse_search(&bytes) {
                                Ok(list) if list.is_empty() => {
                                    (std::vec::Vec::new(), crate::loc::s().cover_none.to_string())
                                }
                                Ok(list) => (list, std::string::String::new()),
                                Err(e) => (std::vec::Vec::new(), e),
                            };
                        // db-api order reads as random in a grid → default A-Z
                        // (the user re-sorts / reverses with Y).
                        cands.sort_by(|a, b| a.title.to_lowercase().cmp(&b.title.to_lowercase()));
                        if let Ok(mut s) = LIBRARY.lock() {
                            s.cover_candidates = cands;
                            s.cover_msg = msg;
                            s.fp_loading = false;
                        }
                    }
                    net::GetPoll::Error(e) => {
                        if let Ok(mut s) = LIBRARY.lock() {
                            s.cover_candidates = std::vec::Vec::new();
                            s.cover_msg = e;
                            s.fp_loading = false;
                        }
                    }
                }
            } else {
                // Full-page scrollable cover grid (not a modal). Subtitle = the
                // search query; thumbnails load progressively from the candidates'
                // cover_url (Flashpoint logos, same source as covers).
                let snap = LIBRARY.lock().ok().map(|s| {
                    let titles: std::vec::Vec<std::string::String> = s
                        .cover_candidates
                        .iter()
                        .map(|c| {
                            if c.developer.is_empty() {
                                c.title.clone()
                            } else {
                                std::format!("{} - {}", c.title, c.developer)
                            }
                        })
                        .collect();
                    let urls: std::vec::Vec<std::string::String> =
                        s.cover_candidates.iter().map(|c| c.cover_url.clone()).collect();
                    // Which hits are already in the local library (drives the OK badge).
                    let installed: std::vec::Vec<bool> = s
                        .cover_candidates
                        .iter()
                        .map(|c| {
                            let name = crate::sources::gamezip::swf_filename(&c.title);
                            s.entries.iter().any(|e| e.basename == name)
                        })
                        .collect();
                    (titles, urls, installed, s.cover_msg.clone(), s.cover_query.clone(), s.fp_content_filter)
                });
                if let Some((titles, urls, installed, msg, query, filter)) = snap {
                    let title_refs: std::vec::Vec<&str> =
                        titles.iter().map(|x| x.as_str()).collect();
                    let url_refs: std::vec::Vec<&str> = urls.iter().map(|x| x.as_str()).collect();
                    // Footer's ZL+ZR hint shows the live filter state: the `{}`
                    // placeholder in `fp_footer` becomes ON (filtered, default) or
                    // OFF (mature catalogue revealed), so the chord's effect is
                    // legible without an extra on-screen widget.
                    let lc = crate::loc::s();
                    let on_off = if filter { lc.lbl_on } else { lc.lbl_off };
                    let footer = lc.fp_footer.replace("{}", on_off);
                    backend.draw_library_fp_gallery(
                        &query, selection, scroll, &title_refs, &url_refs, &installed, &msg,
                        lc.fp_title, &footer,
                    );
                }
            }
        }
        Screen::FpDetails { selection, size, .. } => {
            let snap = LIBRARY.lock().ok().and_then(|s| {
                let desc = s.fp_desc.clone();
                s.cover_candidates.get(selection).map(|c| {
                    (
                        c.title.clone(),
                        c.developer.clone(),
                        c.publisher.clone(),
                        c.release_date.clone(),
                        c.cover_url.clone(),
                        desc,
                    )
                })
            });
            if let Some((title, dev, publisher, date, cover, desc)) = snap {
                backend.draw_library_dim_backdrop();
                backend.draw_library_fp_details(
                    &title, &dev, &publisher, &date, size, &cover, &desc,
                );
            }
        }
        Screen::SortModal { selection, .. } => {
            let lc = crate::loc::s();
            let labels = [
                lc.sort_alpha,
                lc.sort_recent,
                lc.sort_recent_played,
                lc.sort_played,
                lc.sort_size,
            ];
            let dir = if current_sort_reverse() { lc.sort_dir_desc } else { lc.sort_dir_asc };
            backend.draw_library_sort_modal(selection, &labels, lc.sort_title, lc.sort_footer, dir);
        }
        Screen::RemoteSortModal { selection, kind, reverse, .. } => {
            let lc = crate::loc::s();
            // Flashpoint: NAME only. archive.org: NAME + SIZE. Saved URLs:
            // ADDED / NAME / SOURCE (same order as `importer_view` reads).
            let labels: &[&str] = match kind {
                SORT_TARGET_FP => &[lc.sort_alpha],
                SORT_TARGET_IMPORT => {
                    &[lc.sort_recent, lc.sort_alpha, lc.sort_source, lc.sort_files]
                }
                _ => &[lc.sort_alpha, lc.sort_size],
            };
            let dir = if reverse { lc.sort_dir_desc } else { lc.sort_dir_asc };
            backend.draw_library_sort_modal(selection, labels, lc.sort_title, lc.sort_footer, dir);
        }
        // ── Phase 3.7 DISTANT mode ─────────────────────────────────────
        Screen::DistantIdle { selection, scroll_offset } => {
            // Remember where the list is so every "back to the list" path can
            // restore this scroll instead of recomputing one from the row.
            IMPORT_SCROLL.store(scroll_offset, std::sync::atomic::Ordering::Relaxed);
            // Rows in VIEW order (search filter + active sort): a readable label,
            // its host tag, and whether that URL's game is already on SD.
            let (labels, hosts, direct, progress, favorite, total, filter) = LIBRARY
                .lock()
                .ok()
                .map(|s| {
                    let (labels, hosts, direct, progress, favorite, total) = importer_rows(&s);
                    (labels, hosts, direct, progress, favorite, total, s.import_filter.clone())
                })
                .unwrap_or_default();
            let lrefs: std::vec::Vec<&str> = labels.iter().map(|u| u.as_str()).collect();
            let hrefs: std::vec::Vec<&str> = hosts.iter().map(|u| u.as_str()).collect();
            backend.draw_library_distant_list(
                selection,
                &lrefs,
                &hrefs,
                &direct,
                &progress,
                &favorite,
                crate::loc::s().dist_add,
                scroll_offset,
                filter.as_deref(),
                total,
                true,
            );
        }
        Screen::DistantUrlOptions { url_idx, selection } => {
            // Same modal style as the per-game OPTIONS, plus an info block: which
            // kind of source it is, how much of it is on SD, when it was saved,
            // and the full URL (the row only shows a short label). No back row —
            // B backs out.
            let lc = crate::loc::s();
            let snap = LIBRARY.lock().ok().and_then(|s| {
                let url = s.url_history.get(url_idx).cloned()?;
                let (have, total) = importer_progress(&s, &url);
                let direct = matches!(
                    crate::sources::classify(crate::sources::wayback_raw(&url).as_str()),
                    crate::sources::SourceKind::DirectUrl
                );
                let added = s
                    .url_meta
                    .iter()
                    .find(|(u, _, _, _)| u == &url)
                    .map(|(_, _, t, _)| *t)
                    .unwrap_or(0);
                let fav = url_is_favorite(&s, &url);
                Some((url, have, total, direct, added, fav))
            });
            if let Some((url, have, total, direct, added, fav)) = snap {
                let mut info: std::vec::Vec<std::string::String> = std::vec::Vec::new();
                info.push(std::format!(
                    "{} : {}",
                    lc.url_info_type,
                    if direct { lc.url_type_swf } else { lc.url_type_list },
                ));
                if let Some(total) = total {
                    info.push(std::format!("{} : {}/{}", lc.url_info_files, have, total));
                }
                let date = format_date(added);
                if !date.is_empty() {
                    info.push(std::format!("{} : {}", lc.url_info_added, date));
                }
                let fav_label = if fav { lc.opt_unfavorite } else { lc.opt_favorite };
                backend.draw_library_url_options(
                    importer_label(&url).as_str(),
                    selection,
                    &[fav_label, lc.opt_edit, lc.opt_delete],
                    &info,
                    &url,
                    lc.options_footer,
                );
            }
        }
        Screen::DistantLoading => {
            // Async metadata fetch in flight: the reveal window opens from the
            // launched row with a spinner, the IMPORTER list shows behind. Poll
            // the fetch each frame and switch to DistantFiles on success.
            let (labels, hosts, direct, progress, favorite, total, title) = LIBRARY
                .lock()
                .ok()
                .map(|s| {
                    let (labels, hosts, direct, progress, favorite, total) = importer_rows(&s);
                    (labels, hosts, direct, progress, favorite, total, s.pending_fetch_url.clone())
                })
                .unwrap_or_default();
            let lrefs: std::vec::Vec<&str> = labels.iter().map(|u| u.as_str()).collect();
            let hrefs: std::vec::Vec<&str> = hosts.iter().map(|u| u.as_str()).collect();
            let src = crate::backend::render::distant_reveal_source_sel();
            let src_scroll = crate::backend::render::distant_reveal_source_scroll();
            backend.draw_library_distant_list(
                src,
                &lrefs,
                &hrefs,
                &direct,
                &progress,
                &favorite,
                crate::loc::s().dist_add,
                src_scroll,
                None,
                total,
                false,
            );
            // Window: expanding (reveal active) or full screen (reveal done).
            let frac = crate::backend::render::distant_reveal_step(now)
                .map(|(f, _, _)| f)
                .unwrap_or(1.0);
            let (vw, vh) = backend.screen_size();
            let (rx, ry, rw, rh) = distant_row_rect(src, src_scroll, vw);
            let wx = rx * (1.0 - frac);
            let wy = ry * (1.0 - frac);
            let ww = rw + (vw - rw) * frac;
            let wh = rh + (vh - rh) * frac;
            backend.set_clip(wx, wy, ww, wh);
            // Opaque navy panel inside the window REPLACES the URL list there;
            // then the loading content (URL title + spinner). The URL list stays
            // visible (dimmed) only OUTSIDE the window via the chrome below.
            backend.draw_overlay_rect(wx, wy, ww, wh, 0xFF_14_20_38);
            backend.draw_loading_panel(&title, now);
            backend.clear_clip();
            backend.draw_reveal_chrome(wx, wy, ww, wh);
            // Poll the async fetch.
            match net::tick_archive_fetch() {
                net::ArchivePoll::Pending => {}
                net::ArchivePoll::Done(files) => {
                    if files.is_empty() {
                        set_distant_error(crate::loc::s().err_no_swf);
                    } else {
                        let total = files.len() as u32;
                        let url = {
                            let mut g = LIBRARY.lock().ok();
                            if let Some(s) = g.as_mut() {
                                s.remote_files = files;
                                // The URL parsed: no "fix the URL" to offer any more.
                                s.failed_url.clear();
                                // Fresh list: snap the glide to the top.
                                crate::backend::render::gallery_anim_reset();
                                s.screen = Screen::DistantFiles { selection: 0, scroll_offset: 0 };
                                Some(s.pending_fetch_url.clone())
                            } else {
                                None
                            }
                        };
                        if let Some(u) = url {
                            push_history(&u);
                            // Now that we know how many `.swf` this item holds,
                            // cache it so the list can show "4/13" from now on.
                            remember_url_total(&u, total);
                            // A newly-added URL lands wherever the active sort
                            // puts it, so retarget the reveal on its REAL row:
                            // that's where the window must collapse back to.
                            let new_row = LIBRARY
                                .lock()
                                .ok()
                                .map(|s| {
                                    let abs = s
                                        .url_history
                                        .iter()
                                        .position(|h| h == &u)
                                        .unwrap_or_else(|| s.url_history.len().saturating_sub(1));
                                    importer_row_for_abs(&s, abs)
                                })
                                .unwrap_or(0);
                            crate::backend::render::distant_reveal_set_source(
                                new_row,
                                clamp_scroll(
                                    IMPORT_SCROLL.load(std::sync::atomic::Ordering::Relaxed),
                                    new_row,
                                    IMPORTER_VISIBLE_ROWS,
                                ),
                            );
                        }
                    }
                }
                net::ArchivePoll::Error(e) => set_distant_error(&e),
            }
        }
        Screen::DistantFiles { selection, scroll_offset } => {
            // Union of session-downloaded basenames (filled by
            // `on_download_finished`) and basenames already scanned
            // from SD into `entries`. The latter catches files that
            // were on SD before this .nro boot — fixes the "OK badge
            // missed across sessions" report.
            let (files, marked, filter, total, under) = LIBRARY
                .lock()
                .ok()
                .map(|s| {
                    let mut marked = s.downloaded_basenames.clone();
                    for e in &s.entries {
                        if !marked.iter().any(|n| n == &e.basename) {
                            marked.push(e.basename.clone());
                        }
                    }
                    let idx = filtered_indices(&s.remote_files, &s.distant_filter);
                    let total = s.remote_files.len();
                    let filtered: std::vec::Vec<crate::net::RemoteFile> =
                        idx.iter().map(|&i| s.remote_files[i].clone()).collect();
                    // `under` = the IMPORTER rows drawn UNDER the reveal window.
                    (filtered, marked, s.distant_filter.clone(), total, importer_rows(&s))
                })
                .unwrap_or_default();
            // Expand/collapse reveal (v1.2.0): while a reveal runs, draw the
            // IMPORTER list underneath and the file list clipped to a window that
            // opens from / closes to the launched URL's row.
            if let Some((frac, collapsing, done)) =
                crate::backend::render::distant_reveal_step(now)
            {
                let (vw, vh) = backend.screen_size();
                let src = crate::backend::render::distant_reveal_source_sel();
                let src_scroll = crate::backend::render::distant_reveal_source_scroll();
                let (rx, ry, rw, rh) = distant_row_rect(src, src_scroll, vw);
                // Lerp the window from the row rect (frac 0) to full screen (1).
                let wx = rx * (1.0 - frac);
                let wy = ry * (1.0 - frac);
                let ww = rw + (vw - rw) * frac;
                let wh = rh + (vh - rh) * frac;
                let lrefs: std::vec::Vec<&str> = under.0.iter().map(|u| u.as_str()).collect();
                let hrefs: std::vec::Vec<&str> = under.1.iter().map(|u| u.as_str()).collect();
                backend.draw_library_distant_list(
                    src,
                    &lrefs,
                    &hrefs,
                    &under.2,
                    &under.3,
                    &under.4,
                    crate::loc::s().dist_add,
                    src_scroll,
                    None,
                    under.5,
                    false,
                );
                backend.set_clip(wx, wy, ww, wh);
                backend.draw_library_distant_files(
                    selection, scroll_offset, &files, DISTANT_VISIBLE_ROWS,
                    &marked, filter.as_deref(), total, Some((wx, wy, ww, wh)),
                );
                backend.clear_clip();
                // Make the opening/closing rectangle visible (same navy behind):
                // dim outside + bright border.
                backend.draw_reveal_chrome(wx, wy, ww, wh);
                if done && collapsing {
                    if let Ok(mut s) = LIBRARY.lock() {
                        s.remote_files.clear();
                        s.distant_filter = None;
                        // `src` is the ROW the window collapsed to; clamp it to
                        // the rows that exist (add row + the current view).
                        let rows = importer_view(&s).len();
                        s.screen = distant_idle_at(src.min(rows));
                    }
                }
            } else {
                backend.draw_library_distant_files(
                    selection, scroll_offset, &files, DISTANT_VISIBLE_ROWS,
                    &marked, filter.as_deref(), total, None,
                );
            }
        }
        Screen::DistantDownloading => {
            // Pump the curl multi handle once per frame and check completion. The
            // progress snapshot reflects whatever the last tick updated.
            let companion = LIBRARY
                .lock()
                .ok()
                .map(|s| {
                    (
                        s.dl_companion_active,
                        s.dl_companion_current.clone(),
                        s.dl_companion_queue.len(),
                    )
                })
                .unwrap_or((false, std::string::String::new(), 0));
            if companion.0 {
                // Companion phase (multi-file game): each sibling streams through
                // the same bar, labelled "MULTI-FICHIERS : <name> (remaining)".
                let (done, total) = net::download_progress();
                let label = std::format!(
                    "{} : {} ({})",
                    crate::loc::s().multifile,
                    companion.1,
                    companion.2 + 1,
                );
                backend.draw_library_distant_downloading(&label, done, total);
                match net::tick_download() {
                    Ok(false) => {}
                    // Done OR errored: a failed companion is skipped (the game may
                    // still partly work), the queue carries on / finalizes.
                    _ => companion_download_finished(),
                }
            } else {
                let (done, total) = net::download_progress();
                // Snapshot the name for the UI — prefer the REAL Flashpoint title
                // (with its `:` etc.) over the sanitized on-SD filename.
                let file_name = LIBRARY
                    .lock()
                    .ok()
                    .map(|s| {
                        s.download_title
                            .clone()
                            .filter(|t| !t.trim().is_empty())
                            .unwrap_or_else(|| s.download_file_name.clone())
                    })
                    .unwrap_or_default();
                backend.draw_library_distant_downloading(&file_name, done, total);
                match net::tick_download() {
                    Ok(false) => {}
                    Ok(true) => on_download_finished(),
                    Err(msg) => set_distant_error(&msg),
                }
            }
        }
        Screen::DistantError => {
            // A URL-level failure carries the URL that broke → the footer offers
            // Y (re-type it). File-level failures don't, and show A/B only.
            let (msg, can_fix) = LIBRARY
                .lock()
                .ok()
                .map(|s| (s.distant_error.clone(), !s.failed_url.is_empty()))
                .unwrap_or_default();
            backend.draw_library_distant_error(&msg, can_fix);
        }
        Screen::DistantHistoryConfirm => {
            let url = LIBRARY
                .lock()
                .ok()
                .and_then(|s| s.history_idx.and_then(|i| s.url_history.get(i).cloned()))
                .unwrap_or_default();
            backend.draw_library_history_delete_confirm(&url);
        }
    }

    // ── Navbar (v1.2.0) ──────────────────────────────────────────────────
    // Drawn last so the tab strip sits on top of every tab-home screen. L/R
    // switches tabs (see `input()`); sub-screens (`screen_tab` == None) show
    // no navbar. Reset the transform first so the navbar itself never moves.
    backend.clear_ui_transform();
    if let Some(tab) = screen_tab(screen) {
        backend.draw_navbar(tab.index());
        // Version label, bottom-right of the home screens.
        backend.draw_version_badge();
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
        backend.draw_toast(&msg, kind);
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
    let (swf, entry_name) =
        match crate::sources::gamezip::extract_gamezip_tree(zip_path, &files_dir, launch_command) {
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
fn companion_download_finished() {
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
        finalize_gamezip_download();
    }
}

/// Finish a Flashpoint game download once its main SWF (and any companions) are
/// on the SD card: add it to the local library, write the source-URL sidecar,
/// auto-fetch the cover, restore the real title, and return to the FpGallery.
/// Reads the `download_*` state (still populated until here). Shared by the
/// no-companion path and the companion-phase completion.
fn finalize_gamezip_download() {
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
    // manual "Jaquette" step. Synchronous HTTPS, best-effort.
    if let Some(url) = &cover_url {
        if !file_name.is_empty() {
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
fn on_download_finished() {
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
            clear_download_state(&mut s);
        }
        // `failed_url` still holds this URL (only a good download clears it), so
        // the error screen keeps offering Y = correct it and retry.
        set_distant_error(crate::loc::s().err_dl_not_a_game);
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
        finalize_gamezip_download();
        return;
    }

    // Flashpoint GameZIP: the downloaded file is a `.zip`; extract its `.swf`
    // to `swf_path` and add THAT (not the zip), then delete the temp zip.
    if let Some(swf_path) = zip_extract {
        let Some((entry_name, swf)) = extract_gamezip_main(&out_path, &swf_path, &launch_command) else {
            let _ = std::fs::remove_file(&out_path);
            if let Ok(mut s) = LIBRARY.lock() {
                s.download_file_name.clear();
                s.download_out_path.clear();
                s.download_zip_extract = None;
                s.download_fp_direct = false;
                s.download_cover_url = None;
                s.download_title = None;
                s.download_resume_pos = None;
            }
            set_distant_error(crate::loc::s().err_no_swf);
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
        finalize_gamezip_download();
        return;
    }

    // Add to the LOCAL entries list (so when the user backs out of
    // DISTANT mode, the file appears in the LOCAL library). The magic bytes were
    // checked above, so a failure here means a SWF that starts right and then
    // stops — truncated mid-transfer, most likely. Reported below rather than
    // dropped: the file is on the card either way, so "it downloaded" and "you
    // can play it" are not the same claim.
    let added = add_or_replace_path(&out_path);
    if !added {
        log(&std::format!(
            "library: {} downloaded but its SWF header will not parse - no entry created\n",
            out_path,
        ));
    }
    // Record the source URL (direct .swf or archive.org file) next to the .swf,
    // for later bug-report attribution.
    write_url_sidecar(&out_path, &source_url);
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
            for name in refs {
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
            // Direct `.swf` import (no metadata list) — back to IMPORTER home.
            s.screen = distant_idle_at(0);
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
    // add_path pushed unconditionally. Dedup: if there are two entries
    // with the same path, keep the most recent (last pushed) one.
    if let Ok(mut s) = LIBRARY.lock() {
        // Find duplicates by path. add_path pushed the latest at the end.
        let last_idx = s.entries.len().saturating_sub(1);
        if last_idx > 0 {
            let last_path = s.entries[last_idx].path.clone();
            // Look earlier in the list for the same path; if found, remove it.
            if let Some(prev_idx) = s.entries[..last_idx]
                .iter()
                .position(|e| e.path == last_path)
            {
                s.entries.remove(prev_idx);
            }
        }
        // Re-apply the active sort so a freshly downloaded / imported game lands
        // in its sorted slot (e.g. A-Z) instead of stuck at the bottom of the
        // list. The "now" mtime stamp from `add_path` still floats it to the top
        // under the RECENT sort.
        sort_entries(&mut s.entries, current_sort_mode(), current_sort_reverse());
    }
    true
}

pub(crate) struct LibraryListSnapshot {
    pub entries: std::vec::Vec<Entry>,
    pub banner_tex: u32,
    pub banner_w: u32,
    pub banner_h: u32,
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
