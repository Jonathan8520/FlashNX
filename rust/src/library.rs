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
    /// JOUER sort picker (Y): centered modal — A-Z / recent / most-played / size.
    /// `prev_sel`/`prev_scroll` restore the gallery cursor when B (cancel) is hit.
    SortModal { selection: usize, prev_sel: usize, prev_scroll: usize },
    /// Sort picker for the DISTANT lists (Y). `fp` = Flashpoint gallery (sorts
    /// `cover_candidates` by name/developer) vs archive.org files (sorts
    /// `remote_files` by name/size). `prev_*` restore the cursor on cancel.
    RemoteSortModal { selection: usize, fp: bool, prev_sel: usize, prev_scroll: usize },
    // ── Phase 3.7: DISTANT mode (archive.org import) ───────────────────
    /// IMPORTER home: a navigable LIST of saved URLs + a trailing "+ add" row.
    /// `selection` indexes [history urls.., add-row]. A launches the selected
    /// URL (or adds one on the add-row); + opens per-URL options.
    DistantIdle { selection: usize },
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
}

pub(crate) const OPTIONS_ENTRIES: &[&str] =
    &["TOUCHES", "RENOMMER", "JAQUETTE", "SUPPRIMER", "RETOUR"];

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

/// Active library sort, persisted to `sdmc:/flashnx/sort.txt`
/// (0 = A-Z, 1 = recent, 2 = most played). Default A-Z.
static SORT_MODE: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);
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

/// 0/1/2 index of the active sort (for the sort modal cursor).
pub(crate) fn sort_mode_index() -> usize {
    SORT_MODE.load(std::sync::atomic::Ordering::Relaxed) as usize
}

/// Set + persist the active sort mode (0/1/2).
pub(crate) fn set_sort_mode(idx: u8) {
    SORT_MODE.store(idx, std::sync::atomic::Ordering::Relaxed);
    let c: &[u8] = match idx {
        1 => b"1",
        2 => b"2",
        3 => b"3",
        4 => b"4",
        _ => b"0",
    };
    if std::fs::write(SORT_PATH, c).is_ok() {
        crate::sd::commit();
    }
}

fn read_sort_mode() -> u8 {
    use std::io::Read;
    let mut f = match std::fs::File::open(SORT_PATH) {
        Ok(f) => f,
        Err(_) => return 0,
    };
    let mut b = [0u8; 1];
    match f.read(&mut b) {
        Ok(n) if n >= 1 => match b[0] {
            b'1' => 1,
            b'2' => 2,
            b'3' => 3,
            b'4' => 4,
            _ => 0,
        },
        _ => 0,
    }
}

/// Boot-once load of persisted playtime + sort preference into memory.
fn ensure_prefs_loaded() {
    if !PREFS_LOADED.swap(true, std::sync::atomic::Ordering::Relaxed) {
        crate::playtime::load();
        SORT_MODE.store(read_sort_mode(), std::sync::atomic::Ordering::Relaxed);
    }
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

/// Sort `entries` in place by the given mode (alpha tiebreak everywhere).
pub(crate) fn sort_entries(entries: &mut std::vec::Vec<Entry>, mode: SortMode) {
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
}

fn note_played(basename: &str, display_name: &str) {
    if let Ok(mut g) = LAST_PLAYED.lock() {
        *g = Some((basename.into(), display_name.into()));
    }
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
        if std::path::Path::new(&p).exists() {
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
        let _ = std::fs::remove_file(&path);
        // Also try to clean up any legacy copy so the next library
        // boot doesn't resurrect a stale display name.
        if let Some(legacy) = find_user_file(&std::format!("{}.meta.json", basename)) {
            let _ = std::fs::remove_file(&legacy);
        }
        crate::sd::commit();
        return true;
    }
    let meta = MetaSidecar {
        display_name: Some(display_name.to_string()),
    };
    match serde_json::to_string_pretty(&meta) {
        Ok(json) => {
            let ok = std::fs::write(&path, json.as_bytes()).is_ok();
            if ok {
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
    /// Set when the in-flight download is a Flashpoint GameZIP: holds the FINAL
    /// `.swf` path to extract the zip into (the download itself goes to a temp
    /// `.zip`). `None` for a normal direct/archive.org `.swf` download.
    pub(crate) download_zip_extract: Option<std::string::String>,
    /// Last error message — shown in `Screen::DistantError` until user
    /// dismisses with A/B.
    pub(crate) distant_error: std::string::String,
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
    /// Active substring filter on the DistantFiles list (X = open swkbd
    /// to set/edit; empty input = clear). Lowercase, substring match
    /// against the lowercased filename. `None` = no filter (show all).
    pub(crate) distant_filter: Option<std::string::String>,
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
    /// Active substring filter on the LOCAL list (X = open swkbd to set/edit;
    /// empty input = clear). Mirrors `distant_filter`: lowercase, substring
    /// match against the lowercased display name OR basename. `None` = show
    /// all. While set, `Screen::List` selection/scroll index the FILTERED
    /// view (see `local_filtered_indices`).
    local_filter: Option<std::string::String>,
    /// Flashpoint cover candidates for the current `CoverPicker` (OPTIONS >
    /// JAQUETTE). Filled by `run_cover_search_flow`, indexed by the picker
    /// selection, consumed by `run_cover_fetch_flow`.
    cover_candidates: std::vec::Vec<crate::sources::flashpoint::CatalogEntry>,
    /// Notice shown on the `CoverPicker` when there's no list to show (covers
    /// off / no results / fetch error). Empty = render the candidate list.
    cover_msg: std::string::String,
    /// Last search term used for the `CoverPicker`, so Minus (refine) can
    /// pre-fill the keyboard with it instead of the raw filename-derived query.
    cover_query: std::string::String,
    /// URL of the async archive.org fetch in flight (`Screen::DistantLoading`),
    /// pushed to history once the fetch succeeds.
    pending_fetch_url: std::string::String,
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
    download_zip_extract: None,
    distant_error: std::string::String::new(),
    downloaded_basenames: std::vec::Vec::new(),
    url_history: std::vec::Vec::new(),
    history_idx: None,
    distant_filter: None,
    download_resume_pos: None,
    history_loaded: false,
    applet_mode: false,
    local_filter: None,
    cover_candidates: std::vec::Vec::new(),
    cover_msg: std::string::String::new(),
    cover_query: std::string::String::new(),
    pending_fetch_url: std::string::String::new(),
});

/// Where the URL history persists across boots. Format: JSON array of
/// strings, max 20 entries (LRU-style — newest at end, oldest dropped
/// when we exceed). Lives in the same dir as `cacert.pem` so the user's
/// `sdmc:/ruffle/` stays SWF-only.
const HISTORY_PATH: &str = "sdmc:/switch/FlashNX/distant_history.json";
const HISTORY_MAX: usize = 20;

/// Rows of covers visible at once in the JOUER justified gallery (v1.2.0).
/// Must match the number of rows `draw_library_gallery` fits under the banner.
pub const GALLERY_ROWS_VISIBLE: usize = 3;

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
pub fn add_path(path: &str, mtime: u64) -> bool {
    let basename = path
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(path)
        .to_string();
    let (size_bytes, swf_version, compression_label, is_as3) = match read_swf_header(path) {
        Some(h) => (h.size_bytes, h.version, h.compression_label, h.is_as3),
        None => {
            log(&std::format!(
                "library: failed to parse SWF header for {}, skipping\n",
                path,
            ));
            return false;
        }
    };
    let color_chip = color_from_basename(&basename);
    // Honour a per-game display-name override from the .meta.json sidecar
    // (Phase 3.4.bis RENOMMER). Sidecar absent / unparseable / empty
    // display_name → fall back to basename.
    let display_name = read_meta_sidecar(&basename)
        .and_then(|m| m.display_name)
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| basename.clone());
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
    };
    log(&std::format!(
        "library: added {} (SWF v{} {}, {})\n",
        path, swf_version, compression_label,
        if is_as3 { "AS3/AVM2" } else { "AS2/AVM1" },
    ));
    if let Ok(mut s) = LIBRARY.lock() {
        s.entries.push(entry);
    }
    true
}

/// Transition from Inactive → List (or Empty if `entries` is empty). Called
/// after C++ has finished scanning and the GL renderer is up.
pub fn open() {
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
        sort_entries(&mut s.entries, current_sort_mode());
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
            let scroll_offset = gallery_scroll_for(selection, 0);
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

fn load_history_from_sd() {
    // 256 KB cap: history is max 20 URLs (<2 KB); the cap is just a safety net.
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

/// Push `url` onto the history. De-dups (if already present, moves to
/// most-recent end). Truncates to `HISTORY_MAX`. Saves to SD.
fn push_history(url: &str) {
    let url = url.trim().to_string();
    if url.is_empty() {
        return;
    }
    if let Ok(mut s) = LIBRARY.lock() {
        if let Some(pos) = s.url_history.iter().position(|u| u == &url) {
            s.url_history.remove(pos);
        }
        s.url_history.push(url);
        while s.url_history.len() > HISTORY_MAX {
            s.url_history.remove(0);
        }
        s.history_idx = Some(s.url_history.len() - 1);
        let snapshot = s.url_history.clone();
        let loaded = s.history_loaded;
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
    if let Ok(mut s) = LIBRARY.lock() {
        s.pending_fetch_url.clear();
        s.entries.clear();
        s.selected_path = None;
        s.screen = Screen::Inactive;
        s.banner_tex = 0;
        s.banner_w = 0;
        s.banner_h = 0;
        s.remote_files.clear();
        s.download_file_name.clear();
        s.download_out_path.clear();
        s.download_zip_extract = None;
        s.distant_error.clear();
        s.downloaded_basenames.clear();
        s.distant_filter = None;
        s.download_resume_pos = None;
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
    // Sub-screen: TOUCHES editor owns input while active.
    if menu::is_active() {
        let consumed = menu::input(button);
        // If menu just closed itself, fall back to the OPTIONS modal.
        if !menu::is_active() {
            let mut dest: Option<Screen> = None;
            if let Ok(mut s) = LIBRARY.lock() {
                match s.screen {
                    Screen::TouchesEditor { game_idx } => {
                        s.screen = Screen::OptionsModal { game_idx, selection: 0 };
                        dest = Some(s.screen);
                    }
                    Screen::SettingsKeymapEditor => {
                        s.screen = Screen::SettingsModal { selection: 0 };
                        dest = Some(s.screen);
                    }
                    _ => {}
                }
            }
            if dest.is_some() {
                // Closing TOUCHES: scale the destination panel back in (the
                // OPTIONS modal, or the REGLAGES tab content).
                crate::backend::render::modal_open_begin();
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
                        Tab::Importer => Screen::DistantIdle { selection: 0 },
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
        if let Screen::DistantIdle { selection } = screen_snap {
            if button == "X" {
                // X on the IMPORTER home = search Flashpoint for a game to
                // download. Hoisted: swkbd + HTTPS must run WITHOUT the lock.
                run_fp_search_flow();
                return true;
            }
            if button == "A" {
                // A on a URL row launches it; A on the trailing "+ add" row
                // (selection == history len, so get() is None) opens swkbd.
                let url = LIBRARY
                    .lock()
                    .ok()
                    .and_then(|s| s.url_history.get(selection).cloned());
                match url {
                    // archive.org → async fetch + reveal (begun inside, opens the
                    // row with a spinner); direct .swf → download screen. The
                    // expand is started by `run_archive_fetch_async`, not here.
                    Some(u) => run_fetch_for_url(&u, selection),
                    None => run_add_url_flow(),
                }
                return true;
            }
        }
        // EDIT a URL from its options modal (entry 0) — swkbd, hoisted.
        if let Screen::DistantUrlOptions { url_idx, selection } = screen_snap {
            if button == "A" && selection == 0 {
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
                "Y" => { s.screen = Screen::DistantIdle { selection: 0 }; }
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
        Screen::SortModal { selection, prev_sel, prev_scroll } => {
            handle_sort_modal_input(&mut s, button, selection, prev_sel, prev_scroll);
            true
        }
        Screen::RemoteSortModal { selection, fp, prev_sel, prev_scroll } => {
            handle_remote_sort_modal_input(&mut s, button, selection, fp, prev_sel, prev_scroll);
            true
        }
        Screen::DistantIdle { selection } => {
            handle_distant_idle_input(&mut s, button, selection);
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
                s.screen = Screen::DistantIdle { selection: 0 };
            }
            true
        }
        Screen::DistantDownloading => {
            // B = cancel download. Any other button is ignored during DL.
            if matches!(button, "B") {
                net::cancel_download();
                s.distant_error = std::string::String::from(crate::loc::s().err_dl_cancelled);
                s.screen = Screen::DistantError;
            }
            true
        }
        Screen::DistantError => {
            if matches!(button, "A" | "B" | "Minus") {
                s.distant_error.clear();
                s.screen = Screen::DistantIdle { selection: 0 };
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
                    let n = s.url_history.len();
                    if let Ok(mut p) = PENDING_AFTER_CLOSE.lock() {
                        *p = Some(PendingClose::Goto(Screen::DistantIdle {
                            selection: idx.min(n),
                        }));
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

/// Settings modal: 0 = default controls, 1 = language, 2 = back.
fn handle_settings_input(s: &mut State, button: &str, mut selection: usize) {
    // 0 = default controls, 1 = language, 2 = quit. (No BACK — leave via L/R.)
    const LAST: usize = 2;
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
                    // Reuse the TOUCHES editor, pointed at the global default.
                    keymap::init_for_global_default();
                    menu::open();
                    s.screen = Screen::SettingsKeymapEditor;
                }
                1 => {
                    let cur = crate::loc::current().index();
                    s.screen = Screen::SettingsLanguagePicker { selection: cur };
                }
                2 => {
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

/// Language picker: `selection` indexes `loc::PICKER_LANGS`. A applies +
/// persists the choice; B/Minus cancels back to the settings modal.
fn handle_settings_language_input(s: &mut State, button: &str, mut selection: usize) {
    let last = crate::loc::PICKER_LANGS.len().saturating_sub(1);
    match button {
        "Up" | "StickLUp" => {
            selection = if selection == 0 { last } else { selection - 1 };
        }
        "Down" | "StickLDown" => {
            selection = if selection >= last { 0 } else { selection + 1 };
        }
        "A" => {
            if let Some(&lang) = crate::loc::PICKER_LANGS.get(selection) {
                crate::loc::set(lang);
                crate::loc::save(lang);
            }
            s.screen = Screen::SettingsModal { selection: 1 };
            return;
        }
        "B" | "Minus" => {
            s.screen = Screen::SettingsModal { selection: 1 };
            return;
        }
        _ => {}
    }
    s.screen = Screen::SettingsLanguagePicker { selection };
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
            if let Some(ns) = gallery_neighbor(selection, -1) { selection = ns; }
            scroll = gallery_scroll_for(selection, scroll);
        }
        "Down" | "StickLDown" => {
            if let Some(ns) = gallery_neighbor(selection, 1) { selection = ns; }
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

fn handle_distant_idle_input(s: &mut State, button: &str, mut selection: usize) {
    // A (launch / add) is hoisted (swkbd + HTTPS). Here we just navigate the
    // list and open per-URL options. The add-row is index `n` (== history len).
    let n = s.url_history.len();
    match button {
        "Up" | "StickLUp" => {
            if selection > 0 {
                selection -= 1;
            }
        }
        "Down" | "StickLDown" => {
            if selection < n {
                selection += 1;
            }
        }
        "Plus" => {
            // Options on a real URL row (not the trailing add-row).
            if selection < n {
                s.screen = Screen::DistantUrlOptions { url_idx: selection, selection: 0 };
                return;
            }
        }
        // No B "back to JOUER": the navbar (L/R) is the only inter-tab nav.
        _ => {}
    }
    s.screen = Screen::DistantIdle { selection };
}

/// DistantUrlOptions modal nav (Up/Down move, B back). A on entry 0 (edit) is
/// hoisted; entry 1 = delete (confirm), entry 2 = back.
fn handle_distant_url_options_input(
    s: &mut State,
    button: &str,
    url_idx: usize,
    mut selection: usize,
) {
    const LAST: usize = 2; // 0 = edit, 1 = delete, 2 = back
    let back = |s: &mut State| {
        let n = s.url_history.len();
        s.screen = Screen::DistantIdle { selection: url_idx.min(n) };
    };
    match button {
        "Up" | "StickLUp" => {
            selection = if selection == 0 { LAST } else { selection - 1 };
        }
        "Down" | "StickLDown" => {
            selection = if selection >= LAST { 0 } else { selection + 1 };
        }
        "A" => {
            if selection == 1 {
                // Delete -> reuse the existing confirm screen via history_idx.
                s.history_idx = Some(url_idx);
                s.screen = Screen::DistantHistoryConfirm;
            } else {
                // 0 (edit) is hoisted; 2 = back.
                back(s);
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
    // The reveal grows from the trailing "+ add" row (== current history len).
    let add_row = LIBRARY.lock().map(|s| s.url_history.len()).unwrap_or(0);
    run_fetch_for_url(&url, add_row);
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
        let n = s.url_history.len();
        s.screen = Screen::DistantIdle { selection: url_idx.min(n) };
    }
}

/// Fetch the given URL and transition state. Used both by the post-swkbd
/// path (`run_url_fetch_flow`) and by the ZR re-fetch-without-swkbd path.
/// v1.2.0: routes by source shape so the IMPORTER is "globalisable" — an
/// archive.org URL/item-id goes through the metadata file list, a direct
/// `.swf` URL downloads straight to SD.
fn run_fetch_for_url(url: &str, source_sel: usize) {
    match crate::sources::classify(url) {
        crate::sources::SourceKind::DirectUrl => run_direct_download(url),
        crate::sources::SourceKind::ArchiveOrg => run_archive_fetch_async(url, source_sel),
    }
}

/// Direct `.swf` URL import: download straight to SD, no metadata list. The
/// filename is derived from the URL's last path segment (query/fragment
/// stripped); a re-download just overwrites + refreshes the entry.
fn run_direct_download(url: &str) {
    let tail = url.rsplit('/').next().unwrap_or("");
    let stem = tail.split(['?', '#']).next().unwrap_or(tail);
    let base = if stem.is_empty() { "download.swf" } else { stem };
    let cleaned: std::string::String = base
        .chars()
        .map(|c| if matches!(c, '/' | '\\') { '_' } else { c })
        .collect();
    let safe_name = if cleaned.to_ascii_lowercase().ends_with(".swf") {
        cleaned
    } else {
        std::format!("{}.swf", cleaned)
    };
    let out_path = std::format!("{}/{}", USER_SD_ROOTS[0], safe_name);
    match net::start_download(url, &out_path) {
        Ok(()) => {
            push_history(url);
            if let Ok(mut s) = LIBRARY.lock() {
                s.download_file_name = safe_name;
                s.download_out_path = out_path;
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
    let initial = LIBRARY
        .lock()
        .ok()
        .map(|s| s.cover_query.clone())
        .unwrap_or_default();
    let Some(query) = net::prompt_search(&initial) else {
        return;
    };
    let query = query.trim().to_string();
    if query.is_empty() {
        return;
    }
    let (cands, msg) = match crate::sources::gamezip::search(&query) {
        Ok(list) if list.is_empty() => {
            (std::vec::Vec::new(), crate::loc::s().cover_none.to_string())
        }
        Ok(list) => (list, std::string::String::new()),
        Err(e) => (std::vec::Vec::new(), e),
    };
    log(&std::format!(
        "library: flashpoint game search \"{}\" -> {} hit(s)\n",
        query,
        cands.len(),
    ));
    if let Ok(mut s) = LIBRARY.lock() {
        s.cover_candidates = cands;
        s.cover_msg = msg;
        s.cover_query = query;
        s.screen = Screen::FpGallery { selection: 0, scroll: 0 };
    }
}

/// archive.org item import (async): start the metadata fetch + open the reveal
/// window (with a spinner) from the launched row. `render`'s DistantLoading arm
/// polls `net::tick_archive_fetch` each frame and switches to DistantFiles on
/// success (no UI freeze). `source_sel` is the history row the window grows from.
fn run_archive_fetch_async(url: &str, source_sel: usize) {
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
            crate::backend::render::distant_reveal_begin_expand(source_sel);
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
                fp: false,
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
        // X = handled at the top of `input()` (hoisted because swkbd
        // is a synchronous fullscreen applet that mustn't run under
        // the LIBRARY lock).
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
    let scroll = gallery_scroll_for(pos, 0);
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
                "TOUCHES" => {
                    // Initialise the keymap for THIS game so the editor's
                    // current_binding/set_binding land in the right
                    // sidecar file. Re-init is a no-op if the basename
                    // matches the last init.
                    if let Some(entry) = s.entries.get(game_idx) {
                        keymap::init_for_swf(&entry.basename);
                    }
                    menu::open();
                    s.screen = Screen::TouchesEditor { game_idx };
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
                "RETOUR" => {
                    s.screen = list_screen_for_abs(s, game_idx);
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
    if let Ok(mut s) = LIBRARY.lock() {
        s.cover_candidates = cands;
        s.cover_msg = msg;
        s.cover_query = query;
        s.screen = Screen::CoverPicker { game_idx, selection: 0 };
    }
}

/// CoverPicker > A: download the chosen candidate's logo and cache it as the
/// game's cover. Called from `input()` only — synchronous HTTPS download
/// WITHOUT the LIBRARY lock. On success, invalidates the backend cover-texture
/// cache so the grid shows the new art, and returns to the OPTIONS modal.
fn run_cover_fetch_flow(game_idx: usize, selection: usize) {
    let picked = match LIBRARY.lock() {
        Ok(g) => {
            let cand = g.cover_candidates.get(selection).cloned();
            let base = g.entries.get(game_idx).map(|e| e.basename.clone());
            match (cand, base) {
                (Some(c), Some(b)) => Some((c, b)),
                _ => None,
            }
        }
        Err(_) => return,
    };
    let Some((cand, basename)) = picked else { return };
    log(&std::format!("covers: fetch \"{}\" <- {}\n", basename, cand.cover_url));
    match crate::covers::fetch_and_cache(&basename, &cand) {
        Ok(path) => {
            log(&std::format!("covers: cached -> {}\n", path));
            crate::backend::render::invalidate_cover(&basename);
            if let Ok(mut s) = LIBRARY.lock() {
                s.cover_candidates.clear();
                s.cover_msg.clear();
                s.cover_query.clear();
                // Back to OPTIONS on the JAQUETTE row (index 2).
                s.screen = Screen::OptionsModal { game_idx, selection: 2 };
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
        "B" => {
            s.cover_candidates.clear();
            s.cover_msg.clear();
            s.cover_query.clear();
            s.screen = Screen::OptionsModal { game_idx, selection: 2 };
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
            let zip_path = std::format!("{}/.fpdl.zip", USER_SD_ROOTS[0]);
            let url = crate::sources::gamezip::get_url(&cand.id);
            match net::start_download(&url, &zip_path) {
                Ok(()) => {
                    s.download_file_name = swf_name;
                    s.download_out_path = zip_path;
                    s.download_zip_extract = Some(swf_path);
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
            // Sort picker for the Flashpoint results (name / developer).
            s.screen = Screen::RemoteSortModal {
                selection: 0,
                fp: true,
                prev_sel: selection,
                prev_scroll: scroll,
            };
            return;
        }
        "B" => {
            s.cover_candidates.clear();
            s.cover_msg.clear();
            s.cover_query.clear();
            s.screen = Screen::DistantIdle { selection: 0 };
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
        "A" => {
            // Applied a new sort → the list reorders, so land on the top.
            set_sort_mode(selection as u8);
            sort_entries(&mut s.entries, current_sort_mode());
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

/// DISTANT sort picker (Y). `fp` selects the target list + option set:
/// Flashpoint = name / developer (`cover_candidates`); archive.org = name / size
/// (`remote_files`). A applies + returns to the list (cursor top), B restores.
fn handle_remote_sort_modal_input(
    s: &mut State,
    button: &str,
    mut selection: usize,
    fp: bool,
    prev_sel: usize,
    prev_scroll: usize,
) {
    const N: usize = 2;
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
        "A" => {
            if fp {
                if selection == 1 {
                    s.cover_candidates.sort_by(|a, b| {
                        a.developer
                            .to_lowercase()
                            .cmp(&b.developer.to_lowercase())
                            .then_with(|| a.title.to_lowercase().cmp(&b.title.to_lowercase()))
                    });
                } else {
                    s.cover_candidates
                        .sort_by(|a, b| a.title.to_lowercase().cmp(&b.title.to_lowercase()));
                }
                s.screen = Screen::FpGallery { selection: 0, scroll: 0 };
            } else {
                if selection == 1 {
                    s.remote_files.sort_by(|a, b| {
                        b.size_bytes
                            .cmp(&a.size_bytes)
                            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
                    });
                } else {
                    s.remote_files
                        .sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
                }
                s.screen = Screen::DistantFiles { selection: 0, scroll_offset: 0 };
            }
            return;
        }
        "B" => {
            // Cancel → restore the list cursor where it was.
            if fp {
                s.screen = Screen::FpGallery { selection: prev_sel, scroll: prev_scroll };
            } else {
                s.screen = Screen::DistantFiles { selection: prev_sel, scroll_offset: prev_scroll };
            }
            return;
        }
        _ => {}
    }
    s.screen = Screen::RemoteSortModal { selection, fp, prev_sel, prev_scroll };
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
        log("library: write_meta_sidecar failed (in-memory rename only)\n");
    }
    // Update the in-memory entry.
    if let Ok(mut s) = LIBRARY.lock() {
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
fn delete_game(s: &mut State, game_idx: usize) {
    let Some(entry) = s.entries.get(game_idx).cloned() else {
        return;
    };
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
    // Clear the session "downloaded" mark too. The IMPORTER green OK badge is a
    // union of `entries` (updated just above) and this set; a same-session
    // download lingering here kept the badge lit after a delete, while the
    // A-press "already on SD" check (entries-only) disagreed and re-downloaded.
    s.downloaded_basenames.retain(|n| n != &entry.basename);
}

extern "C" {
    fn swf_picker_delete_game(swf_path: *const core::ffi::c_char) -> core::ffi::c_int;
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
    let sel_row = cells[selection].row as usize;
    if sel_row < scroll {
        sel_row
    } else if sel_row >= scroll + GALLERY_ROWS_VISIBLE {
        sel_row + 1 - GALLERY_ROWS_VISIBLE
    } else {
        scroll
    }
}

/// Screen rect of history row `sel` in the IMPORTER list (matches the layout in
/// `draw_library_distant_list`: top=160, row_h=50, 9 visible). Used as the
/// expand/collapse reveal's grow-from / shrink-to box.
fn distant_row_rect(sel: usize, vw: f32) -> (f32, f32, f32, f32) {
    const TOP: f32 = 160.0;
    const ROW_H: f32 = 50.0;
    const VISIBLE: usize = 9;
    let first = if sel < VISIBLE { 0 } else { sel + 1 - VISIBLE };
    let row_y = TOP + (sel - first) as f32 * ROW_H;
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
        Screen::DistantUrlOptions { .. } => 5,
        Screen::DistantHistoryConfirm => 6,
        Screen::TouchesEditor { .. } => 7,
        Screen::SettingsKeymapEditor => 8,
        Screen::SortModal { .. } => 9,
        Screen::RemoteSortModal { .. } => 10,
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
    DeleteGame { game_idx: usize },
    DeleteHistory,
}

static PENDING_AFTER_CLOSE: Mutex<Option<PendingClose>> = Mutex::new(None);

/// Modal kinds whose close the generic interception defers for a scale-out
/// (plain modal -> non-modal). The destructive confirms (DeleteConfirm=2,
/// DistantHistoryConfirm=6) are handled EXPLICITLY in their own handlers instead
/// (they defer the mutation too — see `PendingClose`), so they're not listed here.
fn modal_close_deferred(kind: u8) -> bool {
    matches!(kind, 1 | 3 | 4 | 5 | 9 | 10)
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
        backend.draw_library_gallery(
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
}

/// Render the current screen using the backend. C++ calls this each frame
/// while the library is active, AFTER `glClear` (we own the entire
/// framebuffer — no Ruffle behind us at this stage).
pub fn render(backend: &mut SwitchRenderBackend) {
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
                Some(PendingClose::DeleteGame { game_idx }) => {
                    delete_game(&mut s2, game_idx);
                    if s2.entries.is_empty() {
                        s2.screen = Screen::Empty;
                    } else {
                        s2.local_filter = None;
                        let new_sel = game_idx.min(s2.entries.len() - 1);
                        let scroll = gallery_scroll_for(new_sel, 0);
                        s2.screen = Screen::List { selection: new_sel, scroll_offset: scroll };
                        // Layout shifted (a tile is gone) — snap the glide.
                        crate::backend::render::gallery_anim_reset();
                    }
                }
                Some(PendingClose::DeleteHistory) => {
                    let idx = s2.history_idx.unwrap_or(0);
                    delete_current_history(&mut s2);
                    let n = s2.url_history.len();
                    s2.screen = Screen::DistantIdle { selection: idx.min(n) };
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
                // same order (TOUCHES / RENOMMER / SUPPRIMER / RETOUR).
                let lc = crate::loc::s();
                // Order must match OPTIONS_ENTRIES (TOUCHES/RENOMMER/JAQUETTE/
                // SUPPRIMER/RETOUR).
                let labels = [lc.opt_keys, lc.opt_rename, lc.opt_cover, lc.opt_delete, lc.opt_back];
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
            menu::draw(backend);
        }
        Screen::SettingsModal { selection } => {
            // REGLAGES is a full navbar TAB now (not a popup): no dim backdrop,
            // no BACK entry — leave via L/R.
            let lc = crate::loc::s();
            let entries = [lc.set_keys, lc.set_language, lc.set_quit];
            backend.draw_library_settings(selection, &entries);
        }
        Screen::SettingsKeymapEditor => {
            // Same as TouchesEditor — the editor edits the global default
            // (keymap module was pointed there on entry).
            backend.draw_library_dim_backdrop();
            menu::draw(backend);
        }
        Screen::SettingsLanguagePicker { selection } => {
            backend.draw_library_dim_backdrop();
            let names: std::vec::Vec<&str> =
                crate::loc::PICKER_LANGS.iter().map(|l| l.native_name()).collect();
            backend.draw_library_language_picker(selection, &names);
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
                    s.cover_candidates.iter().map(|c| c.cover_url.clone()).collect();
                let name = s
                    .entries
                    .get(game_idx)
                    .map(|e| e.display_name.clone())
                    .unwrap_or_default();
                (titles, urls, s.cover_msg.clone(), name)
            });
            if let Some((titles, urls, msg, name)) = snap {
                let title_refs: std::vec::Vec<&str> = titles.iter().map(|x| x.as_str()).collect();
                let url_refs: std::vec::Vec<&str> = urls.iter().map(|x| x.as_str()).collect();
                backend.draw_library_dim_backdrop();
                backend.draw_library_cover_picker(
                    &name, selection, &title_refs, &url_refs, &msg,
                    crate::loc::s().cover_title, crate::loc::s().cover_footer,
                );
            }
        }
        Screen::FpGallery { selection, scroll } => {
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
                (titles, urls, installed, s.cover_msg.clone(), s.cover_query.clone())
            });
            if let Some((titles, urls, installed, msg, query)) = snap {
                let title_refs: std::vec::Vec<&str> = titles.iter().map(|x| x.as_str()).collect();
                let url_refs: std::vec::Vec<&str> = urls.iter().map(|x| x.as_str()).collect();
                backend.draw_library_fp_gallery(
                    &query, selection, scroll, &title_refs, &url_refs, &installed, &msg,
                    crate::loc::s().fp_title, crate::loc::s().fp_footer,
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
            backend.draw_library_sort_modal(selection, &labels, lc.sort_title, lc.sort_footer);
        }
        Screen::RemoteSortModal { selection, fp, .. } => {
            let lc = crate::loc::s();
            let labels: [&str; 2] = if fp {
                [lc.sort_alpha, lc.sort_dev]
            } else {
                [lc.sort_alpha, lc.sort_size]
            };
            backend.draw_library_sort_modal(selection, &labels, lc.sort_title, lc.sort_footer);
        }
        // ── Phase 3.7 DISTANT mode ─────────────────────────────────────
        Screen::DistantIdle { selection } => {
            // Snapshot the URL history; render the list (+ trailing add-row).
            let urls = LIBRARY
                .lock()
                .ok()
                .map(|s| s.url_history.clone())
                .unwrap_or_default();
            let refs: std::vec::Vec<&str> = urls.iter().map(|u| u.as_str()).collect();
            backend.draw_library_distant_list(selection, &refs, crate::loc::s().dist_add);
        }
        Screen::DistantUrlOptions { url_idx, selection } => {
            let url = LIBRARY
                .lock()
                .ok()
                .and_then(|s| s.url_history.get(url_idx).cloned())
                .unwrap_or_default();
            // Reuse the per-game OPTIONS modal style for the URL options.
            let lc = crate::loc::s();
            let labels = [lc.opt_edit, lc.opt_delete, lc.opt_back];
            backend.draw_library_dim_backdrop();
            backend.draw_library_options(&url, selection, &labels);
        }
        Screen::DistantLoading => {
            // Async metadata fetch in flight: the reveal window opens from the
            // launched row with a spinner, the IMPORTER list shows behind. Poll
            // the fetch each frame and switch to DistantFiles on success.
            let (urls, title) = LIBRARY
                .lock()
                .ok()
                .map(|s| (s.url_history.clone(), s.pending_fetch_url.clone()))
                .unwrap_or_default();
            let refs: std::vec::Vec<&str> = urls.iter().map(|u| u.as_str()).collect();
            let src = crate::backend::render::distant_reveal_source_sel();
            backend.draw_library_distant_list(src, &refs, crate::loc::s().dist_add);
            // Window: expanding (reveal active) or full screen (reveal done).
            let frac = crate::backend::render::distant_reveal_step(now)
                .map(|(f, _, _)| f)
                .unwrap_or(1.0);
            let (vw, vh) = backend.screen_size();
            let (rx, ry, rw, rh) = distant_row_rect(src, vw);
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
                        let url = {
                            let mut g = LIBRARY.lock().ok();
                            if let Some(s) = g.as_mut() {
                                s.remote_files = files;
                                s.screen = Screen::DistantFiles { selection: 0, scroll_offset: 0 };
                                Some(s.pending_fetch_url.clone())
                            } else {
                                None
                            }
                        };
                        if let Some(u) = url {
                            push_history(&u);
                            // push_history moved this URL to the most-recent END
                            // of history — retarget the reveal so closing collapses
                            // to its NEW row (and the cursor lands there).
                            let new_idx = LIBRARY
                                .lock()
                                .ok()
                                .map(|s| s.url_history.len().saturating_sub(1))
                                .unwrap_or(0);
                            crate::backend::render::distant_reveal_set_source(new_idx);
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
            let (files, marked, filter, total, urls) = LIBRARY
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
                    (filtered, marked, s.distant_filter.clone(), total, s.url_history.clone())
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
                let (rx, ry, rw, rh) = distant_row_rect(src, vw);
                // Lerp the window from the row rect (frac 0) to full screen (1).
                let wx = rx * (1.0 - frac);
                let wy = ry * (1.0 - frac);
                let ww = rw + (vw - rw) * frac;
                let wh = rh + (vh - rh) * frac;
                let refs: std::vec::Vec<&str> = urls.iter().map(|u| u.as_str()).collect();
                backend.draw_library_distant_list(src, &refs, crate::loc::s().dist_add);
                backend.set_clip(wx, wy, ww, wh);
                backend.draw_library_distant_files(
                    selection, scroll_offset, &files, DISTANT_VISIBLE_ROWS,
                    &marked, filter.as_deref(), total,
                );
                backend.clear_clip();
                // Make the opening/closing rectangle visible (same navy behind):
                // dim outside + bright border.
                backend.draw_reveal_chrome(wx, wy, ww, wh);
                if done && collapsing {
                    if let Ok(mut s) = LIBRARY.lock() {
                        s.remote_files.clear();
                        s.distant_filter = None;
                        let n = s.url_history.len();
                        s.screen = Screen::DistantIdle { selection: src.min(n) };
                    }
                }
            } else {
                backend.draw_library_distant_files(
                    selection, scroll_offset, &files, DISTANT_VISIBLE_ROWS,
                    &marked, filter.as_deref(), total,
                );
            }
        }
        Screen::DistantDownloading => {
            // Pump the curl multi handle once per frame and check
            // completion. The progress snapshot reflects whatever the
            // last tick updated.
            let (done, total) = net::download_progress();
            // Snapshot the filename for the UI.
            let file_name = LIBRARY
                .lock()
                .ok()
                .map(|s| s.download_file_name.clone())
                .unwrap_or_default();
            backend.draw_library_distant_downloading(&file_name, done, total);
            match net::tick_download() {
                Ok(false) => {}
                Ok(true) => on_download_finished(),
                Err(msg) => set_distant_error(&msg),
            }
        }
        Screen::DistantError => {
            let msg = LIBRARY
                .lock()
                .ok()
                .map(|s| s.distant_error.clone())
                .unwrap_or_default();
            backend.draw_library_distant_error(&msg);
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
    }
}

/// Read a downloaded Flashpoint GameZIP at `zip_path`, extract its `.swf` to
/// `swf_path`, and add it to the local library. Returns false on any
/// read/parse/write failure (caller then shows `err_no_swf`).
fn finish_gamezip(zip_path: &str, swf_path: &str) -> bool {
    let Some(zip) = crate::sources::gamezip::read_file_bounded(zip_path, 64 * 1024 * 1024) else {
        log("library: gamezip read failed\n");
        return false;
    };
    let Some(swf) = crate::sources::gamezip::extract_first_swf(&zip) else {
        log("library: gamezip has no .swf entry\n");
        return false;
    };
    if std::fs::write(swf_path, &swf).is_err() {
        log("library: gamezip .swf write failed\n");
        return false;
    }
    crate::sd::commit();
    log(&std::format!(
        "library: gamezip extracted -> {} ({} bytes)\n",
        swf_path,
        swf.len(),
    ));
    add_or_replace_path(swf_path)
}

/// Called from `render()` after `tick_download` returns Ok(true). Adds
/// the downloaded file to the local entries list (so it's playable when
/// the user goes back to LOCAL) and returns to the DistantFiles screen
/// so the user can keep picking other files from the same archive.org
/// item without re-typing the URL. The just-downloaded basename is
/// tracked in `downloaded_basenames` so the list shows a `✓` next to it.
fn on_download_finished() {
    let (out_path, file_name, zip_extract) = match LIBRARY.lock() {
        Ok(g) => (
            g.download_out_path.clone(),
            g.download_file_name.clone(),
            g.download_zip_extract.clone(),
        ),
        Err(_) => return,
    };
    if out_path.is_empty() {
        return;
    }
    log(&std::format!("library: download finished -> {}\n", out_path));

    // Flashpoint GameZIP: the downloaded file is a `.zip`; extract its `.swf`
    // to `swf_path` and add THAT (not the zip), then delete the temp zip.
    if let Some(swf_path) = zip_extract {
        let added = finish_gamezip(&out_path, &swf_path);
        let _ = std::fs::remove_file(&out_path);
        if !added {
            if let Ok(mut s) = LIBRARY.lock() {
                s.download_file_name.clear();
                s.download_out_path.clear();
                s.download_zip_extract = None;
                s.download_resume_pos = None;
            }
            set_distant_error(crate::loc::s().err_no_swf);
            return;
        }
        if let Ok(mut s) = LIBRARY.lock() {
            s.download_file_name.clear();
            s.download_out_path.clear();
            s.download_zip_extract = None;
            if !file_name.is_empty() && !s.downloaded_basenames.iter().any(|n| n == &file_name) {
                s.downloaded_basenames.push(file_name);
            }
            // Back to the Flashpoint gallery so the user can grab another game.
            let (sel, scroll) = s.download_resume_pos.take().unwrap_or((0, 0));
            let sel = sel.min(s.cover_candidates.len().saturating_sub(1));
            s.screen = Screen::FpGallery { selection: sel, scroll };
        }
        return;
    }

    // Add to the LOCAL entries list (so when the user backs out of
    // DISTANT mode, the file appears in the LOCAL library).
    let _ = add_or_replace_path(&out_path);
    if let Ok(mut s) = LIBRARY.lock() {
        s.download_file_name.clear();
        s.download_out_path.clear();
        if !file_name.is_empty() && !s.downloaded_basenames.iter().any(|n| n == &file_name) {
            s.downloaded_basenames.push(file_name);
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
            s.screen = Screen::DistantIdle { selection: 0 };
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
        if last_idx == 0 {
            return true;
        }
        let last_path = s.entries[last_idx].path.clone();
        // Look earlier in the list for the same path; if found, remove it.
        if let Some(prev_idx) = s.entries[..last_idx]
            .iter()
            .position(|e| e.path == last_path)
        {
            s.entries.remove(prev_idx);
        }
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
