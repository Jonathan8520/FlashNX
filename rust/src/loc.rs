//! UI string localization (EN / FR / ES / RU / DE / IT / PT / ZH / TR).
//!
//! Every string is written the way its language writes it, accents and lower
//! case included: the interface kit draws them in the console's own fonts
//! (`backend/render/kit/text.rs`). The strings of the screens rebuilt on the
//! kit live in `loc_ui.rs`.
//!
//! Lookup is a plain `&'static Strings` per language, chosen by a global
//! atomic set at boot from `settings.json` (or auto-detected from the Switch
//! system language) and changeable at runtime via the Settings modal.
//!
//! Flash KEY names ("Space", "Shift", "A".."Z") are technical identifiers
//! stored in keymap JSON and are intentionally NOT translated -- only the
//! "(none)" placeholder is localized (`none`).

use core::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::fs::File;
use std::io::{Read, Write};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    En,
    Fr,
    Es,
    Ru,
    De,
    It,
    Pt,
    Zh,
    Tr,
}

impl Lang {
    /// True when this language's strings are drawn from the shared-font glyph
    /// atlas rather than the embedded 5x7 bitmap font. Latin and Cyrillic are
    /// in the bitmap font, so only CJK depends on the atlas.
    pub fn needs_cjk(self) -> bool {
        matches!(self, Lang::Zh)
    }

    pub fn index(self) -> usize {
        match self {
            Lang::En => 0,
            Lang::Fr => 1,
            Lang::Es => 2,
            Lang::Ru => 3,
            Lang::De => 4,
            Lang::It => 5,
            Lang::Pt => 6,
            Lang::Zh => 7,
            Lang::Tr => 8,
        }
    }
    pub fn from_index(i: usize) -> Lang {
        match i {
            1 => Lang::Fr,
            2 => Lang::Es,
            3 => Lang::Ru,
            4 => Lang::De,
            5 => Lang::It,
            6 => Lang::Pt,
            7 => Lang::Zh,
            8 => Lang::Tr,
            _ => Lang::En,
        }
    }
    pub fn code(self) -> &'static str {
        match self {
            Lang::En => "en",
            Lang::Fr => "fr",
            Lang::Es => "es",
            Lang::Ru => "ru",
            Lang::De => "de",
            Lang::It => "it",
            Lang::Pt => "pt",
            Lang::Zh => "zh",
            Lang::Tr => "tr",
        }
    }
    pub fn from_code(s: &str) -> Option<Lang> {
        match s {
            "en" => Some(Lang::En),
            "fr" => Some(Lang::Fr),
            "es" => Some(Lang::Es),
            "ru" => Some(Lang::Ru),
            "de" => Some(Lang::De),
            "it" => Some(Lang::It),
            "pt" => Some(Lang::Pt),
            "zh" => Some(Lang::Zh),
            "tr" => Some(Lang::Tr),
            _ => None,
        }
    }
    /// Name for the picker, in its own language. Without the console fonts a
    /// CJK name falls back to the old glyph atlas, and when that cannot be
    /// afforded it would render as a blank row: show a Latin label instead.
    /// Picking it then resolves to English (see `set`), which is what it will
    /// look like too.
    pub fn picker_name(self) -> &'static str {
        if self.needs_cjk() && !cjk_drawable() {
            return match self {
                Lang::Zh => "Chinese (needs more memory)",
                _ => "Unavailable",
            };
        }
        self.native_name()
    }

    pub fn native_name(self) -> &'static str {
        match self {
            Lang::En => "English",
            Lang::Fr => "Français",
            Lang::Es => "Español",
            Lang::Ru => "Русский",
            Lang::De => "Deutsch",
            Lang::It => "Italiano",
            Lang::Pt => "Português",
            Lang::Zh => "中文",
            Lang::Tr => "Türkçe",
        }
    }
}

/// Whether Chinese can be drawn: always with the console fonts (the kit loads
/// the shared Chinese font on first use), else only if the old glyph atlas
/// fits in memory.
fn cjk_drawable() -> bool {
    crate::backend::render::kit_fonts_available() || crate::backend::glyphs::cjk_possible()
}

/// Order of languages in the picker (matches `Lang` index order).
pub const PICKER_LANGS: &[Lang] = &[
    Lang::En,
    Lang::Fr,
    Lang::Es,
    Lang::Ru,
    Lang::De,
    Lang::It,
    Lang::Pt,
    Lang::Zh,
    Lang::Tr,
];

/// Every translatable UI string. One instance per language below.
pub struct Strings {
    // Pause menu (in-game)
    pub menu_resume: &'static str,
    pub menu_keys: &'static str,
    /// Pause menu: the row that opens the screen sub-panel (display mode,
    /// rotation, filter). Also that panel's own title.
    pub menu_screen: &'static str,
    pub menu_restart: &'static str,
    pub menu_quit: &'static str,
    pub pause_title: &'static str,
    // TOUCHES (keymap) editor
    pub keys_title: &'static str,
    pub keys_footer: &'static str,
    pub keys_dropdown_footer: &'static str,
    /// Value a pad row shows when its button is a MODIFIER and so cannot send
    /// a key of its own -- see `keymap::slot_is_modifier`.
    pub keys_modifier: &'static str,
    pub none: &'static str,
    /// TOUCHES dropdown labels for the mouse-click pseudo-keys (the stored key
    /// name stays language-stable; only this display label is translated).
    pub flash_mouse_left: &'static str,
    pub flash_mouse_right: &'static str,
    /// Label of the pseudo-key that opens the console keyboard by hand, for the
    /// text boxes Ruffle does not raise it on.
    pub flash_keyboard: &'static str,
    /// TOUCHES dropdown labels for the NAMED Flash keys. Letters (A-Z) and
    /// digits (0-9) are universal and shown as-is, so they have no loc field.
    pub flash_space: &'static str,
    pub flash_enter: &'static str,
    pub flash_escape: &'static str,
    pub flash_shift: &'static str,
    pub flash_control: &'static str,
    pub flash_alt: &'static str,
    pub flash_tab: &'static str,
    pub flash_backspace: &'static str,
    pub flash_up: &'static str,
    pub flash_down: &'static str,
    pub flash_left: &'static str,
    pub flash_right: &'static str,
    // Library empty state
    pub empty_title: &'static str,
    pub empty_l1: &'static str,
    pub empty_l3: &'static str,
    // Library list
    // Applet-mode notice (P1c) — shown when a game can't launch (small heap)
    pub applet_title: &'static str,
    pub applet_notice: &'static str,
    // OPTIONS modal (per-game)
    pub opt_keys: &'static str,
    pub opt_rename: &'static str,
    pub opt_edit: &'static str,
    /// OPTIONS > move this game onto a shelf (issue #68).
    pub opt_folder: &'static str,
    pub opt_delete: &'static str,
    // Delete confirm
    // DISTANT idle / history
    /// Pinned IMPORTER row that opens the Flashpoint search. Above "+ add a
    /// URL" because it is the path that needs no URL at all.
    pub dist_search_games: &'static str,
    /// Heading on the rule that separates the two actions from the saved
    /// sources below them.
    pub dist_sources: &'static str,
    /// Shown in place of the count on a library with no saved source yet:
    /// the page has two rows and never said what either of them does.
    pub dist_empty_hint: &'static str,
    pub dist_add: &'static str,
    /// IMPORTER sub-line: saved-URL count ({} = number).
    pub dist_count: &'static str,
    // DISTANT files list
    pub files_title: &'static str,
    /// First row of the shelf picker: take this game off every shelf. Describes
    /// a STATE, not a destination -- it used to say ACCUEIL, which promised a
    /// place to go, and the only thing it moves is a game that is physically
    /// filed in a directory (back to the root of the games folder).
    pub folder_root: &'static str,
    // Download
    pub dl_title: &'static str,
    /// Toast flashed when a game finishes downloading ({} = its name).
    pub toast_dl_ok: &'static str,
    /// {} = comma-separated file names. Shown when a downloaded game references
    /// data files that are not available, so the player learns it now instead of
    /// meeting a game that loads forever.
    pub toast_assets_missing: &'static str,
    // Error toast
    pub err_title: &'static str,
    pub err_footer: &'static str,
    /// Error footer when the failing URL can be re-typed on the spot (Y).
    pub err_footer_fix: &'static str,
    // Settings modal (Plus from library)
    pub set_keys: &'static str,
    pub set_cursor_speed: &'static str,
    /// Stage scaling row in a game's OPTIONS, shown as "<label>: <mode>". Issues
    /// #65, #69 and #74: three players in three languages asking to lose the
    /// black bars, #74 putting it best ("the menu is fullscreen but the game is
    /// not").
    pub set_display_mode: &'static str,
    /// Keeps the aspect ratio, black bars where the game does not reach.
    pub display_fit: &'static str,
    /// Fills the screen keeping the aspect ratio, cropping what overflows.
    pub display_fill: &'static str,
    /// Fills the screen by distorting the picture.
    pub display_stretch: &'static str,
    /// Folder picker: create a subfolder here, so a card can be laid out
    /// without leaving FlashNX for a file manager.
    pub games_dir_new: &'static str,
    pub kbd_newdir_guide: &'static str,
    /// Marks the folder currently in use, inside the listing.
    pub games_dir_current: &'static str,
    /// Confirmation before moving a whole library.
    pub games_dir_confirm: &'static str,
    /// Confirmation detail: how many games are about to move, and
    /// where. `{}` takes the count.
    pub games_dir_confirm_n: &'static str,
    /// The same message for exactly one game. Two fields rather than a
    /// "game(s)" that fits neither: a count in brackets is a form no
    /// language actually uses when speaking.
    pub games_dir_confirm_1: &'static str,
    pub games_dir_confirm_yes: &'static str,
    pub games_dir_confirm_no: &'static str,
    /// Shown while the move runs, and when it is done.
    pub games_dir_moving: &'static str,
    /// Titre de la barre d'extraction d'un GameZIP (#117). Un gros jeu
    /// met aussi longtemps a s'extraire qu'a se telecharger, et l'ecran
    /// restait fige sur la barre de telechargement pleine.
    pub extracting: &'static str,
    pub shortcut_ok: &'static str,
    pub shortcut_err: &'static str,
    pub shortcut_removed: &'static str,
    /// Free-zoom row in the pause menu, shown as "<label>: <percent> %", and
    /// again as the heading of the framing mode it opens. Issue #101.
    pub set_zoom: &'static str,
    /// Screen-filter row in the pause menu, shown as "<label>: <filter>".
    /// The other half of issue #65, which asked for "stretch and filters".
    pub set_screen_filter: &'static str,
    /// No filter (default).
    pub filter_none: &'static str,
    /// Darkens every other scanline.
    pub filter_scanlines: &'static str,
    /// Scanlines + RGB stripe mask + vignette.
    pub filter_crt: &'static str,
    /// Texture-sampling row in the pause menu, shown as "<label>: <value>"
    /// (issue #108). Named after what it acts on, not after what it does: a
    /// "FILTERING" row directly under the screen filter's "FILTER" is two rows
    /// one word apart on the same panel, and "FILTRAGE" under "FILTRE" is
    /// worse. What it acts on is the pixels of the game's own bitmaps, which
    /// the free zoom magnifies -- which is how a player noticed at all.
    pub set_pixel_filter: &'static str,
    /// Follow the SWF's own smoothing flag. What Flash Player did, so the
    /// default.
    pub pixels_auto: &'static str,
    /// Force bilinear on every bitmap.
    pub pixels_smooth: &'static str,
    /// Force nearest-neighbour on every bitmap.
    pub pixels_sharp: &'static str,
    /// Pause row + defaults row: raise the CPU clock for heavy games.
    pub set_overclock: &'static str,
    /// The console's own profile, untouched.
    pub overclock_off: &'static str,
    /// CPU 1785 MHz. The GPU is never touched.
    pub overclock_on: &'static str,
    /// Replaces the hint when the console refused the raise. Seen for real on
    /// 2026-08-26 at 7% charge: psm reports the pack out of its Normal voltage
    /// state and the raise is refused, which without this line is a button
    /// pressed eight times with nothing happening and no reason given.
    pub overclock_refused: &'static str,
    /// ECRAN row + defaults row: show the frame rate over the running game (#111).
    pub set_fps: &'static str,
    /// Pause row + defaults row: turn the picture a quarter at a time (#78).
    pub set_rotation: &'static str,
    pub rot_none: &'static str,
    pub rot_90: &'static str,
    pub rot_180: &'static str,
    pub rot_270: &'static str,
    /// TOUCHES row + share/diff label for the per-game "show cursor" toggle.
    pub show_cursor: &'static str,
    /// TOUCHES row (#119): moving the controller moves the cursor. Values:
    /// `overclock_off` (the same OFF word), `gyro_marble`, `gyro_pointer`.
    pub set_gyro: &'static str,
    /// Gyroscope like a marble on the screen: the cursor goes toward the edge
    /// that goes down, as far as the console is tilted.
    pub gyro_marble: &'static str,
    /// Gyroscope like a Wii remote: the cursor goes where the controller points.
    pub gyro_pointer: &'static str,
    /// Values for the show-cursor toggle (shown / hidden).
    pub cursor_shown: &'static str,
    pub cursor_hidden: &'static str,
    /// RÉGLAGES row label for the community-profile nickname (#20).
    pub set_pseudo: &'static str,
    /// Keyboard guide line when typing the nickname.
    pub kbd_pseudo_guide: &'static str,
    // Navbar tabs (v1.2.0) — switched with L/R.
    // v1.2.0 covers + online toggle.
    pub lbl_on: &'static str,
    pub lbl_off: &'static str,
    pub opt_cover: &'static str,
    /// OPTIONS row label: add the game to favorites (when not yet favorited).
    pub opt_favorite: &'static str,
    /// OPTIONS row label: remove the game from favorites (when already favorited).
    pub opt_unfavorite: &'static str,
    /// OPTIONS row label: share this game's controls as a community profile (#20).
    pub opt_share: &'static str,
    /// Success toast shown after the controls were shared (#20).
    pub profile_shared_ok: &'static str,
    // Apply-a-profile UI (#20).
    /// OPTIONS row: apply a community control profile to this game.
    pub opt_apply: &'static str,
    /// Header of the profile picker modal.
    pub profile_title: &'static str,
    /// Last row of the APPLY picker, and the title of the list it opens: the
    /// player's own games with controls of their own, to copy from.
    pub profile_copy_row: &'static str,
    /// Toast when no other game has controls of its own to copy.
    pub profile_copy_none: &'static str,
    /// Shown in the picker when no profile matches this game.
    pub profile_none: &'static str,
    /// Shown instead of `profile_none` when the catalog could not be read at
    /// all: an empty list and an unreachable server are not the same statement.
    pub profile_catalog_offline: &'static str,
    /// Picker row that restores the user's own controls (after applying one).
    pub profile_revert: &'static str,
    /// Toast after applying a profile (note: previous controls are backed up).
    pub profile_applied_ok: &'static str,
    /// Toast after reverting to the user's own controls.
    pub profile_reverted_ok: &'static str,
    /// Confirmation question shown before sharing a game's controls (first time).
    pub profile_share_confirm: &'static str,
    /// Subtitle on the share confirm when it UPDATES the player's existing shared
    /// profile (one slot per person) — shown above the before/after diff.
    pub share_confirm_update: &'static str,
    // TOUCHES sub-menu (#20 regroup): everything control-related lives under one
    // entry so apply/share/revert don't read as game-save actions in OPTIONS.
    /// Sub-menu row: open the per-button keymap editor.
    pub touches_edit: &'static str,
    /// Revert row when there's NO hand-made backup to restore: reverting drops
    /// the applied profile and falls back to the global default controls. Worded
    /// distinctly from `profile_revert` so the user knows it won't restore custom
    /// keys they never made.
    pub touches_revert_default: &'static str,
    // Apply preview (#20): a before/after diff shown before a profile is applied.
    /// Shown in the preview when the profile changes none of the current keys.
    pub profile_preview_none: &'static str,
    /// Toast when sharing would duplicate an already-existing identical profile.
    pub profile_share_dup: &'static str,
    /// Toast when the URL the user just typed names a game already on the card.
    /// `{}` is the file name.
    pub toast_already_imported: &'static str,
    /// Toast when a game is moved onto the shelf it is already on (issue #68).
    pub toast_folder_same: &'static str,
    /// Toast after a game is put ON a shelf. One {}: the shelf name.
    pub toast_folder_on: &'static str,
    /// Toast after a game is taken OFF a shelf. One {}: the shelf name.
    pub toast_folder_off_of: &'static str,
    /// Toast after a game is taken off every shelf at once (the ACCUEIL row).
    pub toast_folder_off: &'static str,
    /// Toast when A is pressed on a shelf the game is on because its FILE sits
    /// in a directory of that name: no label can take it off, only moving it.
    pub toast_folder_physical: &'static str,
    /// Toast when a game cannot come back to the root because another game
    /// already holds that file name there. One {}: the file name. NOT an SD
    /// error -- the card is fine, the name is taken.
    pub toast_name_taken: &'static str,
    /// Toast after a shelf is emptied. Not `games_dir_moved`: a shelf can empty
    /// with nothing having moved at all, when every member was there by label.
    pub folder_emptied: &'static str,
    /// Confirmation detail for emptying a shelf. One {}: how many games leave it.
    pub folder_leave_n: &'static str,
    /// The same message for exactly one game. Two fields rather than a
    /// "game(s)" that fits neither: a count in brackets is a form no
    /// language actually uses when speaking.
    pub folder_leave_1: &'static str,
    /// Confirm prompt for deleting one of your OWN shared profiles (#20).
    pub profile_del_confirm: &'static str,
    /// Toast after your shared profile is removed from the catalog.
    pub profile_del_ok: &'static str,
    /// Toast when there's nothing to delete (no profile shared from this console).
    pub profile_del_not_mine: &'static str,
    pub cover_title: &'static str,
    /// Cover-picker hint for the Y toggle, worded as the ACTION it performs
    /// (what pressing it gives you), not the state you are in. Screenshots
    /// are the default source since issue #59; logos stay one press away for
    /// the games whose logo reads better.
    pub cover_show_logos: &'static str,
    pub cover_show_shots: &'static str,
    pub cover_none: &'static str,
    // Flashpoint game gallery (IMPORTER > X — browse + download a game).
    pub fp_title: &'static str,
    // Flashpoint details popup (`+` on a gallery tile).
    pub fp_details_dev: &'static str,
    pub fp_details_publisher: &'static str,
    pub fp_details_date: &'static str,
    pub fp_details_size: &'static str,
    // JOUER sort picker (Y).
    pub sort_title: &'static str,
    pub sort_alpha: &'static str,
    pub sort_recent: &'static str,
    pub sort_played: &'static str,
    pub sort_size: &'static str,
    pub sort_recent_played: &'static str,
    /// IMPORTER sort mode: group saved URLs by host (archive.org, ...).
    pub sort_source: &'static str,
    /// IMPORTER sort mode: by how many `.swf` the source holds.
    pub sort_files: &'static str,
    /// Toasts for the IMPORTER favorite toggle.
    pub fav_added: &'static str,
    pub fav_removed: &'static str,
    /// Label shown on the launch/loading screen when a game loads companion
    /// SWFs from its `<game>.files/` folder (multi-file game). Drawn with the
    /// companion count appended, e.g. "MULTI-FILE (6)".
    pub multifile: &'static str,
    /// Sort direction labels shown in the sort modals (toggled with X).
    pub sort_dir_asc: &'static str,
    pub sort_dir_desc: &'static str,
    pub lang_title: &'static str,
    // URL-history delete confirm (X on the DISTANT idle screen)
    pub histdel_title: &'static str,
    /// IMPORTER per-URL options modal: info labels + the two source kinds.
    pub url_info_type: &'static str,
    pub url_type_swf: &'static str,
    pub url_type_list: &'static str,
    pub url_info_files: &'static str,
    pub url_info_added: &'static str,
    pub histdel_msg: &'static str,
    // Error MESSAGES (word-wrapped in the error toast). `{}` placeholders
    // are substituted at runtime via str::replace.
    pub err_too_large: &'static str,
    pub err_https: &'static str,         // {} = failure detail (curl/http)
    /// DNS / connect failure: no usable network at all.
    /// curl 5/6: the address never resolved. Names WiFi and DNS both, because
    /// a console with its WiFi off fails exactly the same way as one whose DNS
    /// is broken — blaming only the WiFi sends a connected player to check the
    /// one thing that works (#103).
    pub err_offline: &'static str,
    /// curl 7: the name resolved and the connection was refused. A different
    /// instruction entirely — the network is up, something between the console
    /// and the server is saying no.
    pub err_unreachable: &'static str,
    /// The request timed out.
    pub err_timeout: &'static str,
    /// TLS handshake / certificate failure. The Switch-specific cause is a wrong
    /// console clock, which is why the clock advice lives HERE and nowhere else.
    pub err_tls: &'static str,
    /// The response blew past our in-memory cap (usually far too broad a search).
    pub err_response_big: &'static str,
    /// A download failed WRITING to the SD card (full / unwritable).
    pub err_sd_write: &'static str,
    /// The server answered an error status. {} = the HTTP code.
    pub err_http_status: &'static str,
    /// 429 / 5xx: the host is up but temporarily unable, which is a different
    /// instruction to the player than a refusal or a dead link — wait, do not
    /// go looking for a mistake in the URL. archive.org does this under load.
    pub err_server_busy: &'static str,
    /// HTTP 404 specifically: the URL or item id is wrong / gone.
    pub err_not_found: &'static str,
    pub err_json: &'static str,          // {} = parser detail
    pub err_json_no_files: &'static str,
    pub err_dl_start: &'static str,      // {} = code
    pub err_dl_failed: &'static str,     // {} = code
    pub err_dl_cancelled: &'static str,
    pub err_url_invalid: &'static str,
    pub err_no_swf: &'static str,
    /// Shown when a download completed (HTTP 200) but the bytes are not the file
    /// we asked for — an HTML listing/error page, a redirect target, a truncated
    /// body. Without this the file was saved under the game's name, announced as a
    /// success, and only dropped much later at the library scan, silently.
    pub err_dl_not_a_game: &'static str,
    /// Shown when a Flashpoint game launches via an HTML page + FlashVars
    /// (launchCommand isn't a `.swf`) so it can't run as a bare SWF — refused
    /// before the (often huge) GameZIP download instead of failing after it.
    pub err_fp_html_game: &'static str,
    // swkbd prompts (passed to the C++ software keyboard)
    pub kbd_url_header: &'static str,
    pub kbd_url_guide: &'static str,
    pub kbd_rename_header: &'static str,
    pub kbd_rename_guide: &'static str,
    pub kbd_search_header: &'static str,
    pub kbd_search_guide: &'static str,
    // Bug report (RÉGLAGES → SIGNALER UN BUG)
    pub bug_no_games: &'static str,
    pub bug_ok_title: &'static str,
    pub bug_ok_msg: &'static str,
    pub bug_fail_title: &'static str,
    pub kbd_bug_header: &'static str,
    pub kbd_bug_guide: &'static str,
    // Suggestion / feature request (RÉGLAGES → FAIRE UNE PROPOSITION). Reuses the
    // same relay + token as the bug report; only the issue label differs.
    pub kbd_suggest_header: &'static str,
    pub kbd_suggest_guide: &'static str,
    /// Confirmation after a suggestion is sent. Separate from `bug_ok_msg`: the
    /// two flows share the relay, but a player who sent an idea was being told
    /// their "report" was sent (#83).
    pub suggest_ok_msg: &'static str,
    // First-boot size backfill panel title (v1.5.x): shown while the one-time
    // footprint recompute runs, so that migration isn't a black screen.
    pub optimizing: &'static str,
}

const EN: Strings = Strings {
    optimizing: "Optimizing",
    menu_resume: "Resume",
    menu_keys: "Controls and cursor",
    menu_screen: "Screen",
    menu_restart: "Restart game",
    menu_quit: "Quit game",
    pause_title: "Pause",
    keys_title: "Controls",
    keys_footer: "A:Edit  L/R:Mode  X:P1/P2  B:Back",
    keys_dropdown_footer: "A:OK   B:Cancel   up/down:Nav",
    keys_modifier: "Modifier",
    none: "(none)",
    flash_mouse_left: "Left click",
    flash_mouse_right: "Right click",
    flash_keyboard: "Keyboard",
    flash_space: "Space",
    flash_enter: "Enter",
    flash_escape: "Escape",
    flash_shift: "Shift",
    flash_control: "Control",
    flash_alt: "Alt",
    flash_tab: "Tab",
    flash_backspace: "Backspace",
    flash_up: "Up",
    flash_down: "Down",
    flash_left: "Left",
    flash_right: "Right",
    empty_title: "No games",
    empty_l1: "Drop .swf files into",
    empty_l3: "Then restart FlashNX.",
    applet_title: "Applet mode",
    applet_notice: "Launching a game needs the full app memory, which applet mode does not have. In the homebrew menu, hold R on a title (or use a forwarder) to start FlashNX with full memory.",
    opt_keys: "Controls and cursor",
    opt_rename: "Rename",
    opt_edit: "Edit",
    opt_folder: "Folders",
    opt_delete: "Delete",
    dist_search_games: "Search for a game",
    dist_sources: "My sources",
    dist_empty_hint: "Search a game by name, or paste an address",
    dist_add: "+ Add a URL",
    dist_count: "{} URL(s)",
    files_title: "Remote files",
    folder_root: "No folder",
    dl_title: "Downloading",
    toast_dl_ok: "{} downloaded",
    toast_assets_missing: "Missing game data: {}. This game may not start.",
    err_title: "Error",
    err_footer: "A/B:OK",
    err_footer_fix: "A/B:OK   Y:Fix the URL",
    set_keys: "Default controls",
    set_pseudo: "Nickname",
    kbd_pseudo_guide: "Your name, shown next to your shared profiles",
    set_cursor_speed: "Cursor speed",
    set_display_mode: "Display",
    display_fit: "Fit",
    display_fill: "Fill",
    display_stretch: "Stretch",
    set_zoom: "Zoom",
    set_screen_filter: "Filter",
    games_dir_new: "New folder",
    games_dir_current: "in use",
    games_dir_confirm: "Move the whole library here?",
    games_dir_confirm_n: "{} games will be moved to:",
    games_dir_confirm_1: "{} game will be moved to:",
    games_dir_confirm_yes: "Move",
    games_dir_confirm_no: "Cancel",
    kbd_newdir_guide: "Name of the folder to create",
    games_dir_moving: "Moving…",
    extracting: "Extracting…",
    shortcut_ok: "Tile installed on the HOME menu",
    shortcut_err: "Could not install the tile",
    shortcut_removed: "Tile removed",
    filter_none: "None",
    filter_scanlines: "Scanlines",
    filter_crt: "CRT",
    set_pixel_filter: "Pixels",
    pixels_auto: "Auto",
    pixels_smooth: "Smooth",
    pixels_sharp: "Sharp",
    set_overclock: "Overclock",
    overclock_off: "Off",
    overclock_on: "On",
    overclock_refused: "Overclock unavailable: battery too low.",
    set_fps: "FPS counter",
    set_rotation: "Rotation",
    rot_none: "None",
    rot_90: "90°",
    rot_180: "180°",
    rot_270: "270°",
    show_cursor: "Show cursor",
    set_gyro: "Gyroscope",
    gyro_marble: "Marble",
    gyro_pointer: "Pointer",
    cursor_shown: "Shown",
    cursor_hidden: "Hidden",
    lbl_on: "On",
    lbl_off: "Off",
    opt_cover: "Cover",
    opt_favorite: "Favorite",
    opt_unfavorite: "Remove favorite",
    opt_share: "Share controls",
    profile_shared_ok: "Your controls were sent. Thanks for helping the community.",
    opt_apply: "Apply a profile",
    profile_title: "Control profiles",
    profile_copy_row: "Copy from another game",
    profile_copy_none: "No other game has controls of its own yet",
    profile_none: "No profile for this game yet. Share yours to help!",
    profile_catalog_offline: "Catalog unavailable. Check your connection.",
    profile_revert: "Revert to my controls",
    profile_applied_ok: "Profile applied. Your previous controls were saved.",
    profile_reverted_ok: "Your controls were restored.",
    profile_share_confirm: "Share your controls for this game?",
    share_confirm_update: "Updates your shared profile:",
    touches_edit: "Edit controls",
    touches_revert_default: "Reset to default controls",
    profile_preview_none: "This profile changes none of your keys.",
    profile_share_dup: "Already in the catalog. Edit a key to share your own version.",
    toast_already_imported: "{} is already in your library.",
    toast_folder_same: "Already in this folder.",
    toast_folder_on: "Added to {}.",
    toast_folder_off_of: "Removed from {}.",
    toast_folder_off: "Off every folder",
    toast_folder_physical: "In this folder on the card. Move it to take it off.",
    toast_name_taken: "{} already exists at the root.",
    folder_emptied: "Folder emptied",
    folder_leave_n: "{} games leave this folder",
    folder_leave_1: "{} game leaves this folder",
    profile_del_confirm: "Delete your shared profile?",
    profile_del_ok: "Your shared profile was deleted.",
    profile_del_not_mine: "Nothing to delete: not shared from this console.",
    cover_title: "Choose a cover",
    cover_show_logos: "Y: Logos",
    cover_show_shots: "Y: Screenshots",
    cover_none: "No results",
    fp_title: "Flashpoint",
    fp_details_dev: "Developer",
    fp_details_publisher: "Publisher",
    fp_details_date: "Released",
    fp_details_size: "Download size",
    sort_title: "Sort by",
    sort_alpha: "Name",
    sort_recent: "Added",
    sort_played: "Most played",
    sort_size: "Size",
    sort_recent_played: "Last played",
    sort_source: "Source",
    sort_files: "File count",
    fav_added: "Added to favorites",
    fav_removed: "Removed from favorites",
    multifile: "Multi-file",
    sort_dir_asc: "Ascending",
    sort_dir_desc: "Descending",
    lang_title: "Language",
    histdel_title: "Delete this source?",
    url_info_type: "Type",
    url_type_swf: "Single .swf",
    url_type_list: "File list",
    url_info_files: "On SD",
    url_info_added: "Added",
    histdel_msg: "It leaves your sources. Games already downloaded stay on the card.",
    err_too_large: "Archive.org response too large (>4 MB). Item too big for this build.",
    err_https: "Transfer failed ({}). Try again in a moment.",
    err_offline: "Address not found (curl {}). Check the console's Wi-Fi, then its DNS.",
    err_unreachable: "The server refused the connection (curl {}). The console is online, so something between it and the server is blocking this.",
    err_timeout: "The server did not answer in time. Try again.",
    err_tls: "Secure connection refused. Check the console's date and time: a wrong clock breaks HTTPS.",
    err_response_big: "Response too large for this build. Try a narrower search.",
    err_sd_write: "Could not write to the SD card. It may be full or write-protected.",
    err_http_status: "The server refused the request (HTTP {}). Try again later.",
    err_server_busy: "The server is overloaded (HTTP {}). Your link is fine — try again in a few minutes.",
    err_not_found: "Not found (404). The URL or the item id is wrong, or it was removed.",
    err_json: "Unreadable archive.org JSON: {}",
    err_json_no_files: "JSON has no \"files\" field",
    err_dl_start: "Could not start the download (code {}).",
    err_dl_failed: "Download failed (code {})",
    err_dl_cancelled: "Download cancelled by the user.",
    err_url_invalid: "Invalid URL. Expected an archive.org URL like https://archive.org/details/<id> or simply <id>.",
    err_no_swf: "No .SWF file found for this game.",
    err_dl_not_a_game: "The download did not return a game file. Check the address and try again.",
    err_fp_html_game: "This game launches from an HTML page (FlashVars) and isn't supported yet.",
    kbd_url_header: "FlashNX - Remote import",
    kbd_url_guide: "archive.org item URL (e.g. https://archive.org/download/your-game-id)",
    kbd_rename_header: "FlashNX - Rename game",
    kbd_rename_guide: "Display name (leave empty to revert to the file name)",
    kbd_search_header: "FlashNX - Search",
    kbd_search_guide: "Filter by file name (empty = show everything)",
    bug_no_games: "No game to report yet. Import or drop a .swf first.",
    bug_ok_title: "Thanks!",
    bug_ok_msg: "Your report was sent. Thanks for helping improve FlashNX.",
    bug_fail_title: "Failed",
    kbd_bug_header: "FlashNX - Report a bug",
    kbd_bug_guide: "Opens a public GitHub issue. Describe the problem. Optional: your @handle. A short technical log is attached.",
    kbd_suggest_header: "FlashNX - Make a suggestion",
    kbd_suggest_guide: "Opens a public GitHub issue. Your idea / feature request for FlashNX.",
    suggest_ok_msg: "Your suggestion was sent. Thanks for helping improve FlashNX.",
};

const FR: Strings = Strings {
    optimizing: "Optimisation",
    menu_resume: "Reprendre",
    menu_keys: "Touches et curseur",
    menu_screen: "Écran",
    menu_restart: "Redémarrer le jeu",
    menu_quit: "Quitter le jeu",
    pause_title: "Pause",
    keys_title: "Touches",
    keys_footer: "A:Éditer  L/R:Mode  X:J1/J2  B:Retour",
    keys_dropdown_footer: "A:OK   B:Annuler   haut/bas:Nav",
    keys_modifier: "Modificateur",
    none: "(aucune)",
    flash_mouse_left: "Clic gauche",
    flash_mouse_right: "Clic droit",
    flash_keyboard: "Clavier",
    flash_space: "Espace",
    flash_enter: "Entrée",
    flash_escape: "Échap",
    flash_shift: "Maj",
    flash_control: "Ctrl",
    flash_alt: "Alt",
    flash_tab: "Tab",
    flash_backspace: "Retour arr.",
    flash_up: "Haut",
    flash_down: "Bas",
    flash_left: "Gauche",
    flash_right: "Droite",
    empty_title: "Aucun jeu",
    empty_l1: "Dépose tes fichiers .swf dans",
    empty_l3: "Puis redémarre FlashNX.",
    applet_title: "Mode applet",
    applet_notice: "Lancer un jeu demande toute la mémoire de l'app, indisponible en mode applet. Dans le menu Homebrew, tiens R sur un titre (ou un forwarder) pour démarrer FlashNX avec toute la mémoire.",
    opt_keys: "Touches et curseur",
    opt_rename: "Renommer",
    opt_edit: "Éditer",
    opt_folder: "Dossiers",
    opt_delete: "Supprimer",
    dist_search_games: "Chercher un jeu",
    dist_sources: "Mes sources",
    dist_empty_hint: "Cherche un jeu par son nom ou colle une adresse",
    dist_add: "+ Ajouter une URL",
    dist_count: "{} URL",
    files_title: "Fichiers distants",
    folder_root: "Aucun dossier",
    dl_title: "Téléchargement",
    toast_dl_ok: "{} téléchargé",
    toast_assets_missing: "Données de jeu manquantes : {}. Ce jeu risque de ne pas démarrer.",
    err_title: "Erreur",
    err_footer: "A/B:OK",
    err_footer_fix: "A/B:OK   Y:Corriger l'URL",
    set_keys: "Touches par défaut",
    set_pseudo: "Pseudo",
    kbd_pseudo_guide: "Ton nom, affiché à côté de tes profils partagés",
    set_cursor_speed: "Vitesse du curseur",
    set_display_mode: "Affichage",
    display_fit: "Intégral",
    display_fill: "Remplir",
    display_stretch: "Étirer",
    set_zoom: "Zoom",
    set_screen_filter: "Filtre",
    games_dir_new: "Nouveau dossier",
    games_dir_current: "actuel",
    games_dir_confirm: "Déplacer toute la bibliothèque ici ?",
    games_dir_confirm_n: "{} jeux seront déplacés vers :",
    games_dir_confirm_1: "{} jeu sera déplacé vers :",
    games_dir_confirm_yes: "Déplacer",
    games_dir_confirm_no: "Annuler",
    kbd_newdir_guide: "Nom du dossier à créer",
    games_dir_moving: "Déplacement\u{2026}",
    extracting: "Extraction…",
    shortcut_ok: "Tuile installée sur le menu HOME",
    shortcut_err: "Impossible d'installer la tuile",
    shortcut_removed: "Tuile retirée",
    filter_none: "Aucun",
    filter_scanlines: "Lignes",
    filter_crt: "CRT",
    set_pixel_filter: "Pixels",
    pixels_auto: "Auto",
    pixels_smooth: "Lissés",
    pixels_sharp: "Nets",
    set_overclock: "Overclock",
    overclock_off: "Inactif",
    overclock_on: "Actif",
    overclock_refused: "Overclock indisponible : batterie trop faible.",
    set_fps: "Compteur FPS",
    set_rotation: "Rotation",
    rot_none: "Aucune",
    rot_90: "90°",
    rot_180: "180°",
    rot_270: "270°",
    show_cursor: "Afficher le curseur",
    set_gyro: "Gyroscope",
    gyro_marble: "Bille",
    gyro_pointer: "Visée",
    cursor_shown: "Affiché",
    cursor_hidden: "Masqué",
    lbl_on: "On",
    lbl_off: "Off",
    opt_cover: "Jaquette",
    opt_favorite: "Favori",
    opt_unfavorite: "Retirer des favoris",
    opt_share: "Partager les touches",
    profile_shared_ok: "Tes touches ont été envoyées. Merci pour la communauté.",
    opt_apply: "Appliquer un profil",
    profile_title: "Profils de touches",
    profile_copy_row: "Copier d'un autre jeu",
    profile_copy_none: "Aucun autre jeu n'a encore ses propres touches",
    profile_none: "Aucun profil pour ce jeu. Partage le tien pour aider !",
    profile_catalog_offline: "Catalogue indisponible. Vérifie ta connexion.",
    profile_revert: "Revenir à mes touches",
    profile_applied_ok: "Profil appliqué. Tes touches précédentes sont sauvegardées.",
    profile_reverted_ok: "Tes touches ont été restaurées.",
    profile_share_confirm: "Partager tes touches pour ce jeu ?",
    share_confirm_update: "Met à jour ton profil partagé :",
    touches_edit: "Modifier les touches",
    touches_revert_default: "Revenir aux touches par défaut",
    profile_preview_none: "Ce profil ne change aucune touche.",
    profile_share_dup: "Déjà dans le catalogue. Modifie une touche pour partager ta version.",
    toast_already_imported: "{} est déjà dans ta ludothèque.",
    toast_folder_same: "Déjà dans ce dossier.",
    toast_folder_on: "Ajouté à {}.",
    toast_folder_off_of: "Retiré de {}.",
    toast_folder_off: "Retiré de tous les dossiers",
    toast_folder_physical: "Dans ce dossier sur la carte. Déplace-le pour l'en sortir.",
    toast_name_taken: "{} existe déjà à la racine.",
    folder_emptied: "Dossier vidé",
    folder_leave_n: "{} jeux quittent ce dossier",
    folder_leave_1: "{} jeu quitte ce dossier",
    profile_del_confirm: "Supprimer ton profil partagé ?",
    profile_del_ok: "Ton profil partagé a été supprimé.",
    profile_del_not_mine: "Rien à supprimer : pas partagé depuis cette console.",
    cover_title: "Choisir une jaquette",
    cover_show_logos: "Y: Logos",
    cover_show_shots: "Y: Captures",
    cover_none: "Aucun résultat",
    fp_title: "Flashpoint",
    fp_details_dev: "Développeur",
    fp_details_publisher: "Éditeur",
    fp_details_date: "Sortie",
    fp_details_size: "Taille du téléchargement",
    sort_title: "Trier par",
    sort_alpha: "Nom",
    sort_recent: "Ajouté",
    sort_played: "Plus joués",
    sort_size: "Taille",
    sort_recent_played: "Dernier joué",
    sort_source: "Source",
    sort_files: "Nombre de fichiers",
    fav_added: "Ajouté aux favoris",
    fav_removed: "Retiré des favoris",
    multifile: "Multi-fichiers",
    sort_dir_asc: "Croissant",
    sort_dir_desc: "Décroissant",
    lang_title: "Langue",
    histdel_title: "Supprimer cette source ?",
    url_info_type: "Type",
    url_type_swf: "Fichier .swf",
    url_type_list: "Liste de fichiers",
    url_info_files: "Sur la SD",
    url_info_added: "Ajoutée le",
    histdel_msg: "Elle disparaît de tes sources. Les jeux déjà téléchargés restent sur la carte.",
    err_too_large: "Réponse archive.org trop volumineuse (> 4 Mo). Item trop massif pour cette version.",
    err_https: "Échec du transfert ({}). Réessaie dans un instant.",
    err_offline: "Adresse introuvable (curl {}). Vérifie le Wi-Fi de la console, puis son DNS.",
    err_unreachable: "Le serveur a refusé la connexion (curl {}). La console est en ligne, quelque chose entre elle et le serveur bloque.",
    err_timeout: "Le serveur n'a pas répondu à temps. Réessaie.",
    err_tls: "Connexion sécurisée refusée. Vérifie la date et l'heure de la console : une horloge fausse casse le HTTPS.",
    err_response_big: "Réponse trop volumineuse. Essaie une recherche plus précise.",
    err_sd_write: "Écriture impossible sur la carte SD. Elle est peut-être pleine.",
    err_http_status: "Le serveur a refusé la requête (HTTP {}). Réessaie plus tard.",
    err_server_busy: "Le serveur est saturé (HTTP {}). Ton lien n'est pas en cause, réessaie dans quelques minutes.",
    err_not_found: "Introuvable (404). L'URL ou l'identifiant de l'item est faux, ou il a été supprimé.",
    err_json: "JSON archive.org illisible : {}",
    err_json_no_files: "JSON sans champ \"files\"",
    err_dl_start: "Impossible de lancer le téléchargement (code {}).",
    err_dl_failed: "Téléchargement échoué (code {})",
    err_dl_cancelled: "Téléchargement annulé par l'utilisateur.",
    err_url_invalid: "URL invalide. Attendu : une URL archive.org du type https://archive.org/details/<id>, ou simplement <id>.",
    err_no_swf: "Aucun fichier .swf trouvé pour ce jeu.",
    err_dl_not_a_game: "Le téléchargement n'a pas renvoyé un fichier de jeu. Vérifie l'adresse.",
    err_fp_html_game: "Ce jeu se lance via une page HTML (FlashVars), pas encore supporté.",
    kbd_url_header: "FlashNX - Import distant",
    kbd_url_guide: "URL archive.org de l'item (ex: https://archive.org/download/your-game-id)",
    kbd_rename_header: "FlashNX - Renommer le jeu",
    kbd_rename_guide: "Nom d'affichage (laisser vide pour revenir au nom de fichier)",
    kbd_search_header: "FlashNX - Rechercher",
    kbd_search_guide: "Filtre nom de fichier (vide = tout afficher)",
    bug_no_games: "Aucun jeu à signaler. Importe ou dépose un .swf d'abord.",
    bug_ok_title: "Merci !",
    bug_ok_msg: "Ton rapport a été envoyé. Merci de m'aider à améliorer FlashNX.",
    bug_fail_title: "Échec",
    kbd_bug_header: "FlashNX - Signaler un bug",
    kbd_bug_guide: "Ouvre une issue GitHub publique. Décris le problème. Option : ton @pseudo. Un court journal technique est joint.",
    kbd_suggest_header: "FlashNX - Faire une proposition",
    kbd_suggest_guide: "Ouvre une issue GitHub publique. Ton idée / proposition pour FlashNX.",
    suggest_ok_msg: "Ta proposition a été envoyée. Merci de m'aider à améliorer FlashNX.",
};

const ES: Strings = Strings {
    optimizing: "Optimizando",
    menu_resume: "Reanudar",
    menu_keys: "Controles y cursor",
    menu_screen: "Pantalla",
    menu_restart: "Reiniciar juego",
    menu_quit: "Salir del juego",
    pause_title: "Pausa",
    keys_title: "Controles",
    keys_footer: "A:Editar  L/R:Modo  X:J1/J2  B:Volver",
    keys_dropdown_footer: "A:OK   B:Cancelar   arriba/abajo:Nav",
    keys_modifier: "Modificador",
    none: "(ninguna)",
    flash_mouse_left: "Clic izquierdo",
    flash_mouse_right: "Clic derecho",
    flash_keyboard: "Teclado",
    flash_space: "Espacio",
    flash_enter: "Intro",
    flash_escape: "Escape",
    flash_shift: "Mayús",
    flash_control: "Ctrl",
    flash_alt: "Alt",
    flash_tab: "Tab",
    flash_backspace: "Retroceso",
    flash_up: "Arriba",
    flash_down: "Abajo",
    flash_left: "Izquierda",
    flash_right: "Derecha",
    empty_title: "Sin juegos",
    empty_l1: "Coloca archivos .swf en",
    empty_l3: "Y reinicia FlashNX.",
    applet_title: "Modo applet",
    applet_notice: "Ejecutar un juego necesita toda la memoria de la app, que el modo applet no tiene. En el homebrew menu, mantén R sobre un título (o usa un forwarder) para iniciar FlashNX con toda la memoria.",
    opt_keys: "Controles y cursor",
    opt_rename: "Renombrar",
    opt_edit: "Editar",
    opt_folder: "Carpetas",
    opt_delete: "Borrar",
    dist_search_games: "Buscar un juego",
    dist_sources: "Mis fuentes",
    dist_empty_hint: "Busca un juego por su nombre o pega una dirección",
    dist_add: "+ Añadir una URL",
    dist_count: "{} URL",
    files_title: "Archivos remotos",
    folder_root: "Sin carpeta",
    dl_title: "Descargando",
    toast_dl_ok: "{} descargado",
    toast_assets_missing: "Faltan datos del juego: {}. Puede que no arranque.",
    err_title: "Error",
    err_footer: "A/B:OK",
    err_footer_fix: "A/B:OK   Y:Corregir la URL",
    set_keys: "Controles por defecto",
    set_pseudo: "Apodo",
    kbd_pseudo_guide: "Tu nombre, junto a tus perfiles compartidos",
    set_cursor_speed: "Velocidad del cursor",
    set_display_mode: "Pantalla",
    display_fit: "Ajustar",
    display_fill: "Rellenar",
    display_stretch: "Estirar",
    set_zoom: "Zoom",
    set_screen_filter: "Filtro",
    games_dir_new: "Nueva carpeta",
    games_dir_current: "en uso",
    games_dir_confirm: "Mover toda la biblioteca aqui?",
    games_dir_confirm_n: "{} juegos se moveran a:",
    games_dir_confirm_1: "{} juego se movera a:",
    games_dir_confirm_yes: "Mover",
    games_dir_confirm_no: "Cancelar",
    kbd_newdir_guide: "Nombre de la carpeta a crear",
    games_dir_moving: "Moviendo…",
    extracting: "Extrayendo…",
    shortcut_ok: "Icono instalado en el menú HOME",
    shortcut_err: "No se pudo instalar el icono",
    shortcut_removed: "Icono eliminado",
    filter_none: "Ninguno",
    filter_scanlines: "Líneas",
    filter_crt: "CRT",
    set_pixel_filter: "Píxeles",
    pixels_auto: "Auto",
    pixels_smooth: "Suaves",
    pixels_sharp: "Nítidos",
    set_overclock: "Overclock",
    overclock_off: "Inactivo",
    overclock_on: "Activo",
    overclock_refused: "Overclock no disponible: batería muy baja.",
    set_fps: "Contador FPS",
    set_rotation: "Rotación",
    rot_none: "Ninguna",
    rot_90: "90°",
    rot_180: "180°",
    rot_270: "270°",
    show_cursor: "Mostrar cursor",
    set_gyro: "Giroscopio",
    gyro_marble: "Canica",
    gyro_pointer: "Puntero",
    cursor_shown: "Visible",
    cursor_hidden: "Oculto",
    lbl_on: "On",
    lbl_off: "Off",
    opt_cover: "Carátula",
    opt_favorite: "Favorito",
    opt_unfavorite: "Quitar favorito",
    opt_share: "Compartir controles",
    profile_shared_ok: "Tus controles se enviaron. Gracias por ayudar a la comunidad.",
    opt_apply: "Aplicar un perfil",
    profile_title: "Perfiles de control",
    profile_copy_row: "Copiar de otro juego",
    profile_copy_none: "Ningún otro juego tiene aún controles propios",
    profile_none: "Aún no hay perfil para este juego. ¡Comparte el tuyo!",
    profile_catalog_offline: "Catálogo no disponible. Comprueba tu conexión.",
    profile_revert: "Volver a mis controles",
    profile_applied_ok: "Perfil aplicado. Tus controles anteriores se guardaron.",
    profile_reverted_ok: "Tus controles se restauraron.",
    profile_share_confirm: "¿Compartir tus controles para este juego?",
    share_confirm_update: "Actualiza tu perfil compartido:",
    touches_edit: "Editar controles",
    touches_revert_default: "Restablecer controles por defecto",
    profile_preview_none: "Este perfil no cambia ninguna tecla.",
    profile_share_dup: "Ya está en el catálogo. Cambia una tecla para compartir tu versión.",
    toast_already_imported: "{} ya está en tu biblioteca.",
    toast_folder_same: "Ya está en esta carpeta.",
    toast_folder_on: "Añadido a {}.",
    toast_folder_off_of: "Quitado de {}.",
    toast_folder_off: "Fuera de todas las carpetas",
    toast_folder_physical: "En esta carpeta en la tarjeta. Muévelo para sacarlo.",
    toast_name_taken: "{} ya existe en la raíz.",
    folder_emptied: "Carpeta vaciada",
    folder_leave_n: "{} juegos salen de esta carpeta",
    folder_leave_1: "{} juego sale de esta carpeta",
    profile_del_confirm: "¿Borrar tu perfil compartido?",
    profile_del_ok: "Tu perfil compartido fue borrado.",
    profile_del_not_mine: "Nada que borrar desde esta consola.",
    cover_title: "Elegir carátula",
    cover_show_logos: "Y: Logos",
    cover_show_shots: "Y: Capturas",
    cover_none: "Sin resultados",
    fp_title: "Flashpoint",
    fp_details_dev: "Desarrollador",
    fp_details_publisher: "Editor",
    fp_details_date: "Lanzamiento",
    fp_details_size: "Tamaño de descarga",
    sort_title: "Ordenar por",
    sort_alpha: "Nombre",
    sort_recent: "Añadido",
    sort_played: "Más jugados",
    sort_size: "Tamaño",
    sort_recent_played: "Último jugado",
    sort_source: "Fuente",
    sort_files: "Número de archivos",
    fav_added: "Añadido a favoritos",
    fav_removed: "Quitado de favoritos",
    multifile: "Multiarchivo",
    sort_dir_asc: "Ascendente",
    sort_dir_desc: "Descendente",
    lang_title: "Idioma",
    histdel_title: "¿Borrar esta fuente?",
    url_info_type: "Tipo",
    url_type_swf: "Archivo .swf",
    url_type_list: "Lista de archivos",
    url_info_files: "En la SD",
    url_info_added: "Añadida el",
    histdel_msg: "Desaparece de tus fuentes. Los juegos ya descargados se quedan en la tarjeta.",
    err_too_large: "Respuesta de archive.org demasiado grande (>4 MB). Item demasiado grande para esta versión.",
    err_https: "Fallo de transferencia ({}). Inténtalo de nuevo en un momento.",
    err_offline: "Dirección no encontrada (curl {}). Comprueba el Wi-Fi de la consola y luego su DNS.",
    err_unreachable: "El servidor rechazó la conexión (curl {}). La consola está en línea, algo entre ella y el servidor lo bloquea.",
    err_timeout: "El servidor no respondió a tiempo. Inténtalo de nuevo.",
    err_tls: "Conexión segura rechazada. Comprueba la fecha y la hora de la consola: un reloj mal ajustado rompe HTTPS.",
    err_response_big: "Respuesta demasiado grande. Prueba una búsqueda más precisa.",
    err_sd_write: "No se pudo escribir en la tarjeta SD. Quizá esté llena.",
    err_http_status: "El servidor rechazó la petición (HTTP {}). Inténtalo más tarde.",
    err_server_busy: "El servidor está saturado (HTTP {}). Tu enlace está bien, inténtalo en unos minutos.",
    err_not_found: "No encontrado (404). La URL o el ID del item es incorrecto, o fue eliminado.",
    err_json: "JSON de archive.org ilegible: {}",
    err_json_no_files: "JSON sin campo \"files\"",
    err_dl_start: "No se pudo iniciar la descarga (código {}).",
    err_dl_failed: "Descarga fallida (código {})",
    err_dl_cancelled: "Descarga cancelada por el usuario.",
    err_url_invalid: "URL inválida. Se esperaba una URL de archive.org tipo https://archive.org/details/<id> o simplemente <id>.",
    err_no_swf: "No se encontró ningún archivo .swf para este juego.",
    err_dl_not_a_game: "La descarga no devolvió un archivo de juego. Comprueba la dirección.",
    err_fp_html_game: "Este juego se inicia desde una página HTML (FlashVars); aún no compatible.",
    kbd_url_header: "FlashNX - Importar remoto",
    kbd_url_guide: "URL del item de archive.org (ej: https://archive.org/download/your-game-id)",
    kbd_rename_header: "FlashNX - Renombrar juego",
    kbd_rename_guide: "Nombre a mostrar (déjalo vacío para volver al nombre de archivo)",
    kbd_search_header: "FlashNX - Buscar",
    kbd_search_guide: "Filtrar por nombre de archivo (vacio = mostrar todo)",
    bug_no_games: "No hay juego que reportar. Importa o coloca un .swf primero.",
    bug_ok_title: "¡Gracias!",
    bug_ok_msg: "Tu informe se envió. Gracias por ayudar a mejorar FlashNX.",
    bug_fail_title: "Fallo",
    kbd_bug_header: "FlashNX - Reportar un fallo",
    kbd_bug_guide: "Abre un issue público en GitHub. Describe el problema. Opcional: tu @usuario. Se adjunta un registro técnico breve.",
    kbd_suggest_header: "FlashNX - Hacer una sugerencia",
    kbd_suggest_guide: "Abre un issue público en GitHub. Tu idea / sugerencia para FlashNX.",
    suggest_ok_msg: "Tu sugerencia se envió. Gracias por ayudar a mejorar FlashNX.",
};

// Russian.
const RU: Strings = Strings {
    optimizing: "Оптимизация",
    menu_resume: "Продолжить",
    menu_keys: "Клавиши и курсор",
    menu_screen: "Экран",
    menu_restart: "Начать заново",
    menu_quit: "Выйти из игры",
    pause_title: "Пауза",
    keys_title: "Клавиши",
    keys_footer: "A:Изменить  L/R:Режим  X:P1/P2  B:Назад",
    keys_dropdown_footer: "A:ОК   B:Отмена   вверх/вниз:Нав",
    keys_modifier: "Модификатор",
    none: "(Нет)",
    flash_mouse_left: "Левый клик",
    flash_mouse_right: "Правый клик",
    flash_keyboard: "Клавиатура",
    flash_space: "Пробел",
    flash_enter: "Ввод",
    flash_escape: "Esc",
    flash_shift: "Shift",
    flash_control: "Ctrl",
    flash_alt: "Alt",
    flash_tab: "Tab",
    flash_backspace: "Backspace",
    flash_up: "Вверх",
    flash_down: "Вниз",
    flash_left: "Влево",
    flash_right: "Вправо",
    empty_title: "Нет игр",
    empty_l1: "Поместите файлы .swf в",
    empty_l3: "Затем перезапустите FlashNX.",
    applet_title: "Режим апплета",
    applet_notice: "Для запуска игры нужна вся память приложения, которой нет в режиме апплета. В homebrew menu удерживайте R на игре, чтобы запустить FlashNX со всей памятью.",
    opt_keys: "Клавиши и курсор",
    opt_rename: "Переименовать",
    opt_edit: "Изменить",
    opt_folder: "Папки",
    opt_delete: "Удалить",
    dist_search_games: "Найти игру",
    dist_sources: "Мои источники",
    dist_empty_hint: "Найдите игру по названию или вставьте ссылку",
    dist_add: "+ Добавить URL",
    dist_count: "{} URL",
    files_title: "Файлы по сети",
    folder_root: "Без папки",
    dl_title: "Загрузка",
    toast_dl_ok: "{} загружено",
    toast_assets_missing: "Нет данных игры: {}. Игра может не запуститься.",
    err_title: "Ошибка",
    err_footer: "A/B:ОК",
    err_footer_fix: "A/B:ОК   Y:Исправить URL",
    set_keys: "Клавиши по умолчанию",
    set_pseudo: "Ник",
    kbd_pseudo_guide: "Имя рядом с твоими профилями в каталоге",
    set_cursor_speed: "Скорость курсора",
    set_display_mode: "Экран",
    display_fit: "Вписать",
    display_fill: "Заполнить",
    display_stretch: "Растянуть",
    set_zoom: "Масштаб",
    set_screen_filter: "Фильтр",
    games_dir_new: "Новая папка",
    games_dir_current: "текущая",
    games_dir_confirm: "Переместить всю библиотеку сюда?",
    games_dir_confirm_n: "{} игр будет перемещено в:",
    games_dir_confirm_1: "{} игра будет перемещена в:",
    games_dir_confirm_yes: "Переместить",
    games_dir_confirm_no: "Отмена",
    kbd_newdir_guide: "Имя создаваемой папки",
    games_dir_moving: "Перемещение…",
    extracting: "Извлечение…",
    shortcut_ok: "Значок установлен в меню HOME",
    shortcut_err: "Не удалось установить значок",
    shortcut_removed: "Значок удален",
    filter_none: "Нет",
    filter_scanlines: "Линии",
    filter_crt: "CRT",
    set_pixel_filter: "Пиксели",
    pixels_auto: "Авто",
    pixels_smooth: "Сглаженные",
    pixels_sharp: "Чёткие",
    set_overclock: "Разгон",
    overclock_off: "Выкл",
    overclock_on: "Вкл",
    overclock_refused: "Разгон недоступен: батарея разряжена.",
    set_fps: "Счётчик FPS",
    set_rotation: "Поворот",
    rot_none: "Нет",
    rot_90: "90°",
    rot_180: "180°",
    rot_270: "270°",
    show_cursor: "Показать курсор",
    set_gyro: "Гироскоп",
    gyro_marble: "Шарик",
    gyro_pointer: "Прицел",
    cursor_shown: "Показан",
    cursor_hidden: "Скрыт",
    lbl_on: "On",
    lbl_off: "Off",
    opt_cover: "Обложка",
    opt_favorite: "Избранное",
    opt_unfavorite: "Из избранного",
    opt_share: "Поделиться клавишами",
    profile_shared_ok: "Ваши клавиши отправлены. Спасибо за помощь сообществу.",
    opt_apply: "Применить профиль",
    profile_title: "Профили клавиш",
    profile_copy_row: "Копировать из другой игры",
    profile_copy_none: "У других игр пока нет своих клавиш",
    profile_none: "Для этой игры пока нет профиля. Поделитесь своим!",
    profile_catalog_offline: "Каталог недоступен. Проверьте подключение.",
    profile_revert: "Вернуть мои клавиши",
    profile_applied_ok: "Профиль применен. Ваши прежние клавиши сохранены.",
    profile_reverted_ok: "Ваши клавиши восстановлены.",
    profile_share_confirm: "Поделиться клавишами для этой игры?",
    share_confirm_update: "Обновит ваш общий профиль:",
    touches_edit: "Изменить клавиши",
    touches_revert_default: "Сбросить на клавиши по умолчанию",
    profile_preview_none: "Этот профиль не меняет клавиши.",
    profile_share_dup: "Уже в каталоге. Измените клавишу, чтобы поделиться своей версией.",
    toast_already_imported: "{} уже в вашей библиотеке.",
    toast_folder_same: "Уже в этой папке.",
    toast_folder_on: "Добавлено в {}.",
    toast_folder_off_of: "Убрано из {}.",
    toast_folder_off: "Убрано из всех папок",
    toast_folder_physical: "В этой папке на карте. Переместите его.",
    toast_name_taken: "{} уже есть в корне.",
    folder_emptied: "Папка очищена",
    folder_leave_n: "{} игр покидают папку",
    folder_leave_1: "{} игра покидает папку",
    profile_del_confirm: "Удалить ваш профиль?",
    profile_del_ok: "Ваш профиль удален из каталога.",
    profile_del_not_mine: "Нечего удалять: не опубликовано с этой консоли.",
    cover_title: "Выберите обложку",
    cover_show_logos: "Y: Логотипы",
    cover_show_shots: "Y: Скриншоты",
    cover_none: "Нет результатов",
    fp_title: "Flashpoint",
    fp_details_dev: "Разработчик",
    fp_details_publisher: "Издатель",
    fp_details_date: "Выпуск",
    fp_details_size: "Размер загрузки",
    sort_title: "Сортировка",
    sort_alpha: "Имя",
    sort_recent: "Добавлен",
    sort_played: "По времени",
    sort_size: "Размер",
    sort_recent_played: "Недавно играли",
    sort_source: "Источник",
    sort_files: "Число файлов",
    fav_added: "Добавлено в избранное",
    fav_removed: "Удалено из избранного",
    multifile: "Многофайловая",
    sort_dir_asc: "По возр.",
    sort_dir_desc: "По убыв.",
    lang_title: "Язык",
    histdel_title: "Удалить этот источник?",
    url_info_type: "Тип",
    url_type_swf: "Один .swf",
    url_type_list: "Список файлов",
    url_info_files: "На SD",
    url_info_added: "Добавлено",
    histdel_msg: "Он исчезнет из твоих источников. Уже скачанные игры останутся на карте.",
    err_too_large: "Ответ archive.org слишком большой (>4 МБ). Элемент слишком велик для этой версии.",
    err_https: "Сбой передачи ({}). Попробуйте через минуту.",
    err_offline: "Адрес не найден (curl {}). Проверьте Wi-Fi консоли, затем DNS.",
    err_unreachable: "Сервер отклонил соединение (curl {}). Консоль онлайн, что-то между ней и сервером блокирует.",
    err_timeout: "Сервер не ответил вовремя. Попробуйте снова.",
    err_tls: "Защищённое соединение отклонено. Проверьте дату и время консоли: неверные часы ломают HTTPS.",
    err_response_big: "Ответ слишком большой. Уточните запрос.",
    err_sd_write: "Не удалось записать на SD-карту. Возможно, она заполнена.",
    err_http_status: "Сервер отклонил запрос (HTTP {}). Попробуйте позже.",
    err_server_busy: "Сервер перегружен (HTTP {}). Ссылка верна, попробуйте через несколько минут.",
    err_not_found: "Не найдено (404). Неверный URL или ID элемента, либо он удалён.",
    err_json: "Нечитаемый JSON archive.org: {}",
    err_json_no_files: "В JSON нет поля \"files\"",
    err_dl_start: "Не удалось начать загрузку (код {}).",
    err_dl_failed: "Загрузка не удалась (код {})",
    err_dl_cancelled: "Загрузка отменена пользователем.",
    err_url_invalid: "Неверный URL. Ожидался URL archive.org вида https://archive.org/details/<id> или просто <id>.",
    err_no_swf: "Для этой игры не найдено файлов .swf.",
    err_dl_not_a_game: "Загрузка вернула не игровой файл. Проверьте адрес.",
    err_fp_html_game: "Эта игра запускается через HTML-страницу (FlashVars), пока не поддерживается.",
    kbd_url_header: "FlashNX - Загрузка по сети",
    kbd_url_guide: "URL элемента archive.org (например: https://archive.org/download/your-game-id)",
    kbd_rename_header: "FlashNX - Переименовать",
    kbd_rename_guide: "Отображаемое имя (оставь пустым, чтобы вернуть имя файла)",
    kbd_search_header: "FlashNX - Поиск",
    kbd_search_guide: "Фильтр по имени файла (пусто = показать все)",
    bug_no_games: "Нет игр для отчета. Сначала добавьте или загрузите .swf.",
    bug_ok_title: "Спасибо!",
    bug_ok_msg: "Отчет отправлен. Спасибо, что помогаете улучшить FlashNX.",
    bug_fail_title: "Ошибка",
    kbd_bug_header: "FlashNX - Сообщить об ошибке",
    kbd_bug_guide: "Откроет публичную issue на GitHub. Опиши проблему. По желанию: твой @ник. Прилагается краткий технический лог.",
    kbd_suggest_header: "FlashNX - Предложить идею",
    kbd_suggest_guide: "Откроет публичную issue на GitHub. Твоя идея / предложение для FlashNX.",
    suggest_ok_msg: "Предложение отправлено. Спасибо, что помогаете улучшить FlashNX.",
};

// German.
const DE: Strings = Strings {
    optimizing: "Optimierung",
    menu_resume: "Fortsetzen",
    menu_keys: "Steuerung und Cursor",
    menu_screen: "Bildschirm",
    menu_restart: "Spiel neu starten",
    menu_quit: "Spiel beenden",
    pause_title: "Pause",
    keys_title: "Steuerung",
    keys_footer: "A:Bearb.  L/R:Modus  X:S1/S2  B:Zurück",
    keys_dropdown_footer: "A:OK   B:Abbrechen   hoch/runter:Nav",
    keys_modifier: "Modifikator",
    none: "(keine)",
    flash_mouse_left: "Linksklick",
    flash_mouse_right: "Rechtsklick",
    flash_keyboard: "Tastatur",
    flash_space: "Leertaste",
    flash_enter: "Enter",
    flash_escape: "Escape",
    flash_shift: "Umschalt",
    flash_control: "Strg",
    flash_alt: "Alt",
    flash_tab: "Tab",
    flash_backspace: "Rücktaste",
    flash_up: "Hoch",
    flash_down: "Runter",
    flash_left: "Links",
    flash_right: "Rechts",
    empty_title: "Keine Spiele",
    empty_l1: "Lege .swf-Dateien in",
    empty_l3: "Dann FlashNX neu starten.",
    applet_title: "Applet-Modus",
    applet_notice: "Ein Spiel zu starten braucht den vollen App-Speicher, den der Applet-Modus nicht hat. Halte im Homebrew-Menü R auf einem Titel (oder nutze einen Forwarder), um FlashNX mit vollem Speicher zu starten.",
    opt_keys: "Steuerung und Cursor",
    opt_rename: "Umbenennen",
    opt_edit: "Bearbeiten",
    opt_folder: "Ordner",
    opt_delete: "Löschen",
    dist_search_games: "Spiel suchen",
    dist_sources: "Meine Quellen",
    dist_empty_hint: "Spiel per Name suchen oder Adresse einfügen",
    dist_add: "+ URL hinzufügen",
    dist_count: "{} URL(s)",
    files_title: "Entfernte Dateien",
    folder_root: "Kein Ordner",
    dl_title: "Wird geladen",
    toast_dl_ok: "{} heruntergeladen",
    toast_assets_missing: "Spieldaten fehlen: {}. Das Spiel startet möglicherweise nicht.",
    err_title: "Fehler",
    err_footer: "A/B:OK",
    err_footer_fix: "A/B:OK   Y:URL korrigieren",
    set_keys: "Standardsteuerung",
    set_pseudo: "Spitzname",
    kbd_pseudo_guide: "Dein Name, neben deinen geteilten Profilen",
    set_cursor_speed: "Cursorgeschwindigkeit",
    set_display_mode: "Anzeige",
    display_fit: "Einpassen",
    display_fill: "Füllen",
    display_stretch: "Strecken",
    set_zoom: "Zoom",
    set_screen_filter: "Filter",
    games_dir_new: "Neuer Ordner",
    games_dir_current: "aktuell",
    games_dir_confirm: "Ganze Bibliothek hierher verschieben?",
    games_dir_confirm_n: "{} Spiele werden verschoben nach:",
    games_dir_confirm_1: "{} Spiel wird verschoben nach:",
    games_dir_confirm_yes: "Verschieben",
    games_dir_confirm_no: "Abbrechen",
    kbd_newdir_guide: "Name des neuen Ordners",
    games_dir_moving: "Verschieben…",
    extracting: "Entpacken…",
    shortcut_ok: "Kachel im HOME-Menü installiert",
    shortcut_err: "Kachel konnte nicht installiert werden",
    shortcut_removed: "Kachel entfernt",
    filter_none: "Keiner",
    filter_scanlines: "Linien",
    filter_crt: "CRT",
    set_pixel_filter: "Pixel",
    pixels_auto: "Auto",
    pixels_smooth: "Weich",
    pixels_sharp: "Scharf",
    set_overclock: "Overclock",
    overclock_off: "Aus",
    overclock_on: "Ein",
    overclock_refused: "Overclock nicht verfügbar: Akku zu schwach.",
    set_fps: "FPS-Anzeige",
    set_rotation: "Drehung",
    rot_none: "Keine",
    rot_90: "90°",
    rot_180: "180°",
    rot_270: "270°",
    show_cursor: "Cursor zeigen",
    set_gyro: "Gyroskop",
    gyro_marble: "Murmel",
    gyro_pointer: "Zeiger",
    cursor_shown: "Sichtbar",
    cursor_hidden: "Verborgen",
    lbl_on: "An",
    lbl_off: "Aus",
    opt_cover: "Cover",
    opt_favorite: "Favorit",
    opt_unfavorite: "Favorit entfernen",
    opt_share: "Steuerung teilen",
    profile_shared_ok: "Deine Steuerung wurde gesendet. Danke für deine Hilfe.",
    opt_apply: "Profil anwenden",
    profile_title: "Steuerungsprofile",
    profile_copy_row: "Von einem anderen Spiel kopieren",
    profile_copy_none: "Noch kein anderes Spiel hat eigene Tasten",
    profile_none: "Noch kein Profil für dieses Spiel. Teile deins!",
    profile_catalog_offline: "Katalog nicht verfügbar. Prüfe deine Verbindung.",
    profile_revert: "Zu meiner Steuerung zurück",
    profile_applied_ok: "Profil angewendet. Deine bisherige Steuerung wurde gesichert.",
    profile_reverted_ok: "Deine Steuerung wurde wiederhergestellt.",
    profile_share_confirm: "Deine Steuerung für dieses Spiel teilen?",
    share_confirm_update: "Aktualisiert dein geteiltes Profil:",
    touches_edit: "Steuerung bearbeiten",
    touches_revert_default: "Auf Standard zurücksetzen",
    profile_preview_none: "Dieses Profil ändert keine Taste.",
    profile_share_dup: "Bereits im Katalog. Ändere eine Taste, um deine Version zu teilen.",
    toast_already_imported: "{} ist bereits in deiner Bibliothek.",
    toast_folder_same: "Bereits in diesem Ordner.",
    toast_folder_on: "Zu {} hinzugefügt.",
    toast_folder_off_of: "Aus {} entfernt.",
    toast_folder_off: "Aus allen Ordnern entfernt",
    toast_folder_physical: "Auf der Karte in diesem Ordner. Zum Entfernen verschieben.",
    toast_name_taken: "{} existiert bereits im Stammverzeichnis.",
    folder_emptied: "Ordner geleert",
    folder_leave_n: "{} Spiele verlassen diesen Ordner",
    folder_leave_1: "{} Spiel verlässt diesen Ordner",
    profile_del_confirm: "Dein geteiltes Profil löschen?",
    profile_del_ok: "Dein geteiltes Profil wurde gelöscht.",
    profile_del_not_mine: "Nichts zu löschen von dieser Konsole.",
    cover_title: "Cover wählen",
    cover_show_logos: "Y: Logos",
    cover_show_shots: "Y: Screenshots",
    cover_none: "Keine Treffer",
    fp_title: "Flashpoint",
    fp_details_dev: "Entwickler",
    fp_details_publisher: "Herausgeber",
    fp_details_date: "Veröffentlicht",
    fp_details_size: "Download-Größe",
    sort_title: "Sortieren nach",
    sort_alpha: "Name",
    sort_recent: "Hinzugefügt",
    sort_played: "Meist gespielt",
    sort_size: "Größe",
    sort_recent_played: "Zuletzt gespielt",
    sort_source: "Quelle",
    sort_files: "Dateianzahl",
    fav_added: "Zu Favoriten hinzugefügt",
    fav_removed: "Aus Favoriten entfernt",
    multifile: "Mehrdatei",
    sort_dir_asc: "Aufsteigend",
    sort_dir_desc: "Absteigend",
    lang_title: "Sprache",
    histdel_title: "Diese Quelle löschen?",
    url_info_type: "Typ",
    url_type_swf: "Einzelne .swf",
    url_type_list: "Dateiliste",
    url_info_files: "Auf SD",
    url_info_added: "Hinzugefügt",
    histdel_msg: "Sie verschwindet aus deinen Quellen. Bereits geladene Spiele bleiben auf der Karte.",
    err_too_large: "archive.org-Antwort zu groß (>4 MB). Element zu groß für diese Version.",
    err_https: "Übertragung fehlgeschlagen ({}). Gleich nochmal versuchen.",
    err_offline: "Adresse nicht gefunden (curl {}). Prüfe das WLAN der Konsole, dann ihren DNS.",
    err_unreachable: "Der Server hat die Verbindung abgelehnt (curl {}). Die Konsole ist online, etwas dazwischen blockiert.",
    err_timeout: "Der Server hat nicht rechtzeitig geantwortet. Erneut versuchen.",
    err_tls: "Sichere Verbindung abgelehnt. Prüfe Datum und Uhrzeit der Konsole: eine falsche Uhr bricht HTTPS.",
    err_response_big: "Antwort zu groß. Versuche eine genauere Suche.",
    err_sd_write: "Schreiben auf die SD-Karte fehlgeschlagen. Sie ist eventuell voll.",
    err_http_status: "Der Server hat die Anfrage abgelehnt (HTTP {}). Später erneut versuchen.",
    err_server_busy: "Der Server ist überlastet (HTTP {}). Dein Link ist in Ordnung, versuche es in ein paar Minuten erneut.",
    err_not_found: "Nicht gefunden (404). URL oder Item-ID ist falsch, oder es wurde entfernt.",
    err_json: "Unlesbares archive.org-JSON: {}",
    err_json_no_files: "JSON ohne \"files\"-Feld",
    err_dl_start: "Download konnte nicht gestartet werden (Code {}).",
    err_dl_failed: "Download fehlgeschlagen (Code {})",
    err_dl_cancelled: "Download vom Benutzer abgebrochen.",
    err_url_invalid: "Ungültige URL. Erwartet wird eine archive.org-URL wie https://archive.org/details/<id> oder einfach <id>.",
    err_no_swf: "Keine .swf-Datei für dieses Spiel gefunden.",
    err_dl_not_a_game: "Der Download hat keine Spieldatei geliefert. Prüfe die Adresse.",
    err_fp_html_game: "Dieses Spiel startet über eine HTML-Seite (FlashVars), noch nicht unterstützt.",
    kbd_url_header: "FlashNX - Online-Import",
    kbd_url_guide: "archive.org-Item-URL (z.B. https://archive.org/download/your-game-id)",
    kbd_rename_header: "FlashNX - Spiel umbenennen",
    kbd_rename_guide: "Anzeigename (leer lassen, um zum Dateinamen zurückzukehren)",
    kbd_search_header: "FlashNX - Suche",
    kbd_search_guide: "Nach Dateiname filtern (leer = alles anzeigen)",
    bug_no_games: "Noch kein Spiel zu melden. Importiere oder lege zuerst eine .swf ab.",
    bug_ok_title: "Danke!",
    bug_ok_msg: "Dein Bericht wurde gesendet. Danke, dass du hilfst, FlashNX zu verbessern.",
    bug_fail_title: "Fehlgeschlagen",
    kbd_bug_header: "FlashNX - Fehler melden",
    kbd_bug_guide: "Öffnet ein öffentliches GitHub-Issue. Beschreibe das Problem. Optional: dein @Name. Ein kurzes technisches Protokoll wird angehängt.",
    kbd_suggest_header: "FlashNX - Vorschlag machen",
    kbd_suggest_guide: "Öffnet ein öffentliches GitHub-Issue. Deine Idee / dein Wunsch für FlashNX.",
    suggest_ok_msg: "Dein Vorschlag wurde gesendet. Danke, dass du hilfst, FlashNX zu verbessern.",
};

// Italian.
const IT: Strings = Strings {
    optimizing: "Ottimizzazione",
    menu_resume: "Riprendi",
    menu_keys: "Comandi e cursore",
    menu_screen: "Schermo",
    menu_restart: "Riavvia il gioco",
    menu_quit: "Esci dal gioco",
    pause_title: "Pausa",
    keys_title: "Comandi",
    keys_footer: "A:Modifica  L/R:Modo  X:G1/G2  B:Indietro",
    keys_dropdown_footer: "A:OK   B:Annulla   su/giù:Nav",
    keys_modifier: "Modificatore",
    none: "(nessuno)",
    flash_mouse_left: "Clic sinistro",
    flash_mouse_right: "Clic destro",
    flash_keyboard: "Tastiera",
    flash_space: "Spazio",
    flash_enter: "Invio",
    flash_escape: "Esc",
    flash_shift: "Maiusc",
    flash_control: "Ctrl",
    flash_alt: "Alt",
    flash_tab: "Tab",
    flash_backspace: "Backspace",
    flash_up: "Su",
    flash_down: "Giù",
    flash_left: "Sinistra",
    flash_right: "Destra",
    empty_title: "Nessun gioco",
    empty_l1: "Inserisci file .swf in",
    empty_l3: "Poi riavvia FlashNX.",
    applet_title: "Modalità applet",
    applet_notice: "Avviare un gioco richiede tutta la memoria dell'app, che la modalità applet non ha. Nell'homebrew menu, tieni premuto R su un titolo (o usa un forwarder) per avviare FlashNX con tutta la memoria.",
    opt_keys: "Comandi e cursore",
    opt_rename: "Rinomina",
    opt_edit: "Modifica",
    opt_folder: "Cartelle",
    opt_delete: "Elimina",
    dist_search_games: "Cerca un gioco",
    dist_sources: "Le mie fonti",
    dist_empty_hint: "Cerca un gioco per nome, o incolla un indirizzo",
    dist_add: "+ Aggiungi un URL",
    dist_count: "{} URL",
    files_title: "File remoti",
    folder_root: "Nessuna cartella",
    dl_title: "Scaricamento",
    toast_dl_ok: "{} scaricato",
    toast_assets_missing: "Dati di gioco mancanti: {}. Il gioco potrebbe non avviarsi.",
    err_title: "Errore",
    err_footer: "A/B:OK",
    err_footer_fix: "A/B:OK   Y:Correggi l'URL",
    set_keys: "Comandi predefiniti",
    set_pseudo: "Soprannome",
    kbd_pseudo_guide: "Il tuo nome, accanto ai profili condivisi",
    set_cursor_speed: "Velocità cursore",
    set_display_mode: "Schermo",
    display_fit: "Adatta",
    display_fill: "Riempi",
    display_stretch: "Allarga",
    set_zoom: "Zoom",
    set_screen_filter: "Filtro",
    games_dir_new: "Nuova cartella",
    games_dir_current: "in uso",
    games_dir_confirm: "Spostare qui tutta la libreria?",
    games_dir_confirm_n: "{} giochi saranno spostati in:",
    games_dir_confirm_1: "{} gioco sarà spostato in:",
    games_dir_confirm_yes: "Sposta",
    games_dir_confirm_no: "Annulla",
    kbd_newdir_guide: "Nome della cartella da creare",
    games_dir_moving: "Spostamento…",
    extracting: "Estrazione…",
    shortcut_ok: "Icona installata nel menu HOME",
    shortcut_err: "Impossibile installare l'icona",
    shortcut_removed: "Icona rimossa",
    filter_none: "Nessuno",
    filter_scanlines: "Linee",
    filter_crt: "CRT",
    set_pixel_filter: "Pixel",
    pixels_auto: "Auto",
    pixels_smooth: "Morbidi",
    pixels_sharp: "Nitidi",
    set_overclock: "Overclock",
    overclock_off: "Inattivo",
    overclock_on: "Attivo",
    overclock_refused: "Overclock non disponibile: batteria troppo bassa.",
    set_fps: "Contatore FPS",
    set_rotation: "Rotazione",
    rot_none: "Nessuna",
    rot_90: "90°",
    rot_180: "180°",
    rot_270: "270°",
    show_cursor: "Mostra cursore",
    set_gyro: "Giroscopio",
    gyro_marble: "Biglia",
    gyro_pointer: "Puntatore",
    cursor_shown: "Visibile",
    cursor_hidden: "Nascosto",
    lbl_on: "On",
    lbl_off: "Off",
    opt_cover: "Copertina",
    opt_favorite: "Preferito",
    opt_unfavorite: "Rimuovi preferito",
    opt_share: "Condividi comandi",
    profile_shared_ok: "I tuoi comandi sono stati inviati. Grazie per il contributo.",
    opt_apply: "Applica un profilo",
    profile_title: "Profili comandi",
    profile_copy_row: "Copia da un altro gioco",
    profile_copy_none: "Nessun altro gioco ha ancora comandi propri",
    profile_none: "Nessun profilo per questo gioco. Condividi il tuo!",
    profile_catalog_offline: "Catalogo non disponibile. Controlla la connessione.",
    profile_revert: "Torna ai miei comandi",
    profile_applied_ok: "Profilo applicato. I tuoi comandi precedenti sono salvati.",
    profile_reverted_ok: "I tuoi comandi sono stati ripristinati.",
    profile_share_confirm: "Condividere i tuoi comandi per questo gioco?",
    share_confirm_update: "Aggiorna il tuo profilo condiviso:",
    touches_edit: "Modifica comandi",
    touches_revert_default: "Ripristina comandi predefiniti",
    profile_preview_none: "Questo profilo non cambia nessun tasto.",
    profile_share_dup: "Già nel catalogo. Cambia un tasto per condividere la tua versione.",
    toast_already_imported: "{} è già nella tua libreria.",
    toast_folder_same: "Già in questa cartella.",
    toast_folder_on: "Aggiunto a {}.",
    toast_folder_off_of: "Rimosso da {}.",
    toast_folder_off: "Rimosso da tutte le cartelle",
    toast_folder_physical: "In questa cartella sulla scheda. Spostalo per toglierlo.",
    toast_name_taken: "{} esiste già nella radice.",
    folder_emptied: "Cartella svuotata",
    folder_leave_n: "{} giochi lasciano questa cartella",
    folder_leave_1: "{} gioco lascia questa cartella",
    profile_del_confirm: "Eliminare il tuo profilo condiviso?",
    profile_del_ok: "Il tuo profilo condiviso è stato eliminato.",
    profile_del_not_mine: "Niente da eliminare da questa console.",
    cover_title: "Scegli una copertina",
    cover_show_logos: "Y: Loghi",
    cover_show_shots: "Y: Schermate",
    cover_none: "Nessun risultato",
    fp_title: "Flashpoint",
    fp_details_dev: "Sviluppatore",
    fp_details_publisher: "Editore",
    fp_details_date: "Uscita",
    fp_details_size: "Dimensione download",
    sort_title: "Ordina per",
    sort_alpha: "Nome",
    sort_recent: "Aggiunto",
    sort_played: "Più giocati",
    sort_size: "Dimensione",
    sort_recent_played: "Giocato di recente",
    sort_source: "Fonte",
    sort_files: "Numero di file",
    fav_added: "Aggiunto ai preferiti",
    fav_removed: "Rimosso dai preferiti",
    multifile: "Multi-file",
    sort_dir_asc: "Crescente",
    sort_dir_desc: "Decrescente",
    lang_title: "Lingua",
    histdel_title: "Eliminare questa fonte?",
    url_info_type: "Tipo",
    url_type_swf: "File .swf",
    url_type_list: "Elenco di file",
    url_info_files: "Su SD",
    url_info_added: "Aggiunta il",
    histdel_msg: "Sparisce dalle tue fonti. I giochi già scaricati restano sulla scheda.",
    err_too_large: "Risposta di archive.org troppo grande (>4 MB). Elemento troppo grande per questa versione.",
    err_https: "Trasferimento fallito ({}). Riprova tra poco.",
    err_offline: "Indirizzo non trovato (curl {}). Controlla il Wi-Fi della console, poi il suo DNS.",
    err_unreachable: "Il server ha rifiutato la connessione (curl {}). La console è online, qualcosa in mezzo blocca.",
    err_timeout: "Il server non ha risposto in tempo. Riprova.",
    err_tls: "Connessione sicura rifiutata. Controlla data e ora della console: un orologio errato rompe l'HTTPS.",
    err_response_big: "Risposta troppo grande. Prova una ricerca più precisa.",
    err_sd_write: "Impossibile scrivere sulla scheda SD. Forse è piena.",
    err_http_status: "Il server ha rifiutato la richiesta (HTTP {}). Riprova più tardi.",
    err_server_busy: "Il server è sovraccarico (HTTP {}). Il tuo link è corretto, riprova tra qualche minuto.",
    err_not_found: "Non trovato (404). URL o ID dell'elemento errato, oppure è stato rimosso.",
    err_json: "JSON di archive.org illeggibile: {}",
    err_json_no_files: "JSON senza campo \"files\"",
    err_dl_start: "Impossibile avviare il download (codice {}).",
    err_dl_failed: "Download fallito (codice {})",
    err_dl_cancelled: "Download annullato dall'utente.",
    err_url_invalid: "URL non valido. Atteso un URL di archive.org tipo https://archive.org/details/<id> o semplicemente <id>.",
    err_no_swf: "Nessun file .swf trovato per questo gioco.",
    err_dl_not_a_game: "Il download non ha restituito un file di gioco. Controlla l'indirizzo.",
    err_fp_html_game: "Questo gioco si avvia da una pagina HTML (FlashVars), non ancora supportato.",
    kbd_url_header: "FlashNX - Import remoto",
    kbd_url_guide: "URL dell'elemento archive.org (es: https://archive.org/download/your-game-id)",
    kbd_rename_header: "FlashNX - Rinomina gioco",
    kbd_rename_guide: "Nome visualizzato (lascia vuoto per tornare al nome del file)",
    kbd_search_header: "FlashNX - Cerca",
    kbd_search_guide: "Filtra per nome file (vuoto = mostra tutto)",
    bug_no_games: "Nessun gioco da segnalare. Importa o inserisci prima un .swf.",
    bug_ok_title: "Grazie!",
    bug_ok_msg: "La tua segnalazione è stata inviata. Grazie per aiutare a migliorare FlashNX.",
    bug_fail_title: "Fallito",
    kbd_bug_header: "FlashNX - Segnala un bug",
    kbd_bug_guide: "Apre una issue pubblica su GitHub. Descrivi il problema. Facoltativo: il tuo @nome. Viene allegato un breve log tecnico.",
    kbd_suggest_header: "FlashNX - Proponi un'idea",
    kbd_suggest_guide: "Apre una issue pubblica su GitHub. La tua idea / proposta per FlashNX.",
    suggest_ok_msg: "La tua proposta è stata inviata. Grazie per aiutare a migliorare FlashNX.",
};

// Portuguese (Brazil).
const PT: Strings = Strings {
    optimizing: "Otimizando",
    menu_resume: "Continuar",
    menu_keys: "Controles e cursor",
    menu_screen: "Tela",
    menu_restart: "Reiniciar jogo",
    menu_quit: "Sair do jogo",
    pause_title: "Pausa",
    keys_title: "Controles",
    keys_footer: "A:Editar  L/R:Modo  X:J1/J2  B:Voltar",
    keys_dropdown_footer: "A:OK   B:Cancelar   cima/baixo:Nav",
    keys_modifier: "Modificador",
    none: "(nenhuma)",
    flash_mouse_left: "Clique esquerdo",
    flash_mouse_right: "Clique direito",
    flash_keyboard: "Teclado",
    flash_space: "Espaço",
    flash_enter: "Enter",
    flash_escape: "Esc",
    flash_shift: "Shift",
    flash_control: "Ctrl",
    flash_alt: "Alt",
    flash_tab: "Tab",
    flash_backspace: "Backspace",
    flash_up: "Cima",
    flash_down: "Baixo",
    flash_left: "Esquerda",
    flash_right: "Direita",
    empty_title: "Nenhum jogo",
    empty_l1: "Coloque arquivos .swf em",
    empty_l3: "Depois reinicie o FlashNX.",
    applet_title: "Modo applet",
    applet_notice: "Iniciar um jogo precisa de toda a memória do app, que o modo applet não tem. No homebrew menu, segure R sobre um título (ou use um forwarder) para iniciar o FlashNX com toda a memória.",
    opt_keys: "Controles e cursor",
    opt_rename: "Renomear",
    opt_edit: "Editar",
    opt_folder: "Pastas",
    opt_delete: "Excluir",
    dist_search_games: "Procurar um jogo",
    dist_sources: "Minhas fontes",
    dist_empty_hint: "Procure um jogo pelo nome, ou cole um endereço",
    dist_add: "+ Adicionar uma URL",
    dist_count: "{} URL(s)",
    files_title: "Arquivos remotos",
    folder_root: "Sem pasta",
    dl_title: "Baixando",
    toast_dl_ok: "{} baixado",
    toast_assets_missing: "Dados do jogo ausentes: {}. O jogo pode não iniciar.",
    err_title: "Erro",
    err_footer: "A/B:OK",
    err_footer_fix: "A/B:OK   Y:Corrigir a URL",
    set_keys: "Controles padrão",
    set_pseudo: "Apelido",
    kbd_pseudo_guide: "Seu nome, ao lado dos perfis compartilhados",
    set_cursor_speed: "Velocidade do cursor",
    set_display_mode: "Tela",
    display_fit: "Ajustar",
    display_fill: "Preencher",
    display_stretch: "Esticar",
    set_zoom: "Zoom",
    set_screen_filter: "Filtro",
    games_dir_new: "Nova pasta",
    games_dir_current: "em uso",
    games_dir_confirm: "Mover toda a biblioteca para ca?",
    games_dir_confirm_n: "{} jogos serao movidos para:",
    games_dir_confirm_1: "{} jogo sera movido para:",
    games_dir_confirm_yes: "Mover",
    games_dir_confirm_no: "Cancelar",
    kbd_newdir_guide: "Nome da pasta a criar",
    games_dir_moving: "Movendo…",
    extracting: "Extraindo…",
    shortcut_ok: "Ícone instalado no menu HOME",
    shortcut_err: "Não foi possível instalar o ícone",
    shortcut_removed: "Ícone removido",
    filter_none: "Nenhum",
    filter_scanlines: "Linhas",
    filter_crt: "CRT",
    set_pixel_filter: "Pixels",
    pixels_auto: "Auto",
    pixels_smooth: "Suaves",
    pixels_sharp: "Nítidos",
    set_overclock: "Overclock",
    overclock_off: "Inativo",
    overclock_on: "Ativo",
    overclock_refused: "Overclock indisponível: bateria fraca.",
    set_fps: "Contador FPS",
    set_rotation: "Rotação",
    rot_none: "Nenhuma",
    rot_90: "90°",
    rot_180: "180°",
    rot_270: "270°",
    show_cursor: "Mostrar cursor",
    set_gyro: "Giroscópio",
    gyro_marble: "Bolinha",
    gyro_pointer: "Ponteiro",
    cursor_shown: "Visível",
    cursor_hidden: "Oculto",
    lbl_on: "On",
    lbl_off: "Off",
    opt_cover: "Capa",
    opt_favorite: "Favorito",
    opt_unfavorite: "Remover favorito",
    opt_share: "Compartilhar controles",
    profile_shared_ok: "Seus controles foram enviados. Obrigado por ajudar a comunidade.",
    opt_apply: "Aplicar um perfil",
    profile_title: "Perfis de controle",
    profile_copy_row: "Copiar de outro jogo",
    profile_copy_none: "Nenhum outro jogo tem controles próprios ainda",
    profile_none: "Ainda sem perfil para este jogo. Compartilhe o seu!",
    profile_catalog_offline: "Catálogo indisponível. Verifique sua conexão.",
    profile_revert: "Voltar aos meus controles",
    profile_applied_ok: "Perfil aplicado. Seus controles anteriores foram salvos.",
    profile_reverted_ok: "Seus controles foram restaurados.",
    profile_share_confirm: "Compartilhar seus controles para este jogo?",
    share_confirm_update: "Atualiza seu perfil compartilhado:",
    touches_edit: "Editar controles",
    touches_revert_default: "Restaurar controles padrão",
    profile_preview_none: "Este perfil não muda nenhuma tecla.",
    profile_share_dup: "Já está no catálogo. Mude uma tecla para compartilhar sua versão.",
    toast_already_imported: "{} já está na sua biblioteca.",
    toast_folder_same: "Já está nesta pasta.",
    toast_folder_on: "Adicionado a {}.",
    toast_folder_off_of: "Removido de {}.",
    toast_folder_off: "Fora de todas as pastas",
    toast_folder_physical: "Nesta pasta no cartão. Mova-o para tirar.",
    toast_name_taken: "{} já existe na raiz.",
    folder_emptied: "Pasta esvaziada",
    folder_leave_n: "{} jogos saem desta pasta",
    folder_leave_1: "{} jogo sai desta pasta",
    profile_del_confirm: "Remover seu perfil compartilhado?",
    profile_del_ok: "Seu perfil compartilhado foi removido.",
    profile_del_not_mine: "Nada a remover deste console.",
    cover_title: "Escolher uma capa",
    cover_show_logos: "Y: Logos",
    cover_show_shots: "Y: Capturas",
    cover_none: "Sem resultados",
    fp_title: "Flashpoint",
    fp_details_dev: "Desenvolvedor",
    fp_details_publisher: "Distribuidora",
    fp_details_date: "Lançamento",
    fp_details_size: "Tamanho do download",
    sort_title: "Ordenar por",
    sort_alpha: "Nome",
    sort_recent: "Adicionado",
    sort_played: "Mais jogados",
    sort_size: "Tamanho",
    sort_recent_played: "Jogado por último",
    sort_source: "Fonte",
    sort_files: "Número de arquivos",
    fav_added: "Adicionado aos favoritos",
    fav_removed: "Removido dos favoritos",
    multifile: "Multiarquivo",
    sort_dir_asc: "Crescente",
    sort_dir_desc: "Decrescente",
    lang_title: "Idioma",
    histdel_title: "Excluir esta fonte?",
    url_info_type: "Tipo",
    url_type_swf: "Arquivo .swf",
    url_type_list: "Lista de arquivos",
    url_info_files: "No SD",
    url_info_added: "Adicionada em",
    histdel_msg: "Ela sai das suas fontes. Os jogos já baixados continuam no cartão.",
    err_too_large: "Resposta do archive.org muito grande (>4 MB). Item grande demais para esta versão.",
    err_https: "Falha na transferência ({}). Tente de novo daqui a pouco.",
    err_offline: "Endereço não encontrado (curl {}). Verifique o Wi-Fi do console e depois o DNS.",
    err_unreachable: "O servidor recusou a conexão (curl {}). O console está online, algo no meio está bloqueando.",
    err_timeout: "O servidor não respondeu a tempo. Tente novamente.",
    err_tls: "Conexão segura recusada. Verifique a data e a hora do console: um relógio errado quebra o HTTPS.",
    err_response_big: "Resposta muito grande. Tente uma busca mais precisa.",
    err_sd_write: "Não foi possível escrever no cartão SD. Talvez esteja cheio.",
    err_http_status: "O servidor recusou o pedido (HTTP {}). Tente mais tarde.",
    err_server_busy: "O servidor está sobrecarregado (HTTP {}). Seu link está certo, tente em alguns minutos.",
    err_not_found: "Não encontrado (404). A URL ou o ID do item está errado, ou foi removido.",
    err_json: "JSON do archive.org ilegível: {}",
    err_json_no_files: "JSON sem o campo \"files\"",
    err_dl_start: "Não foi possível iniciar o download (código {}).",
    err_dl_failed: "Falha no download (código {})",
    err_dl_cancelled: "Download cancelado pelo usuário.",
    err_url_invalid: "URL inválida. Esperada uma URL do archive.org tipo https://archive.org/details/<id> ou simplesmente <id>.",
    err_no_swf: "Nenhum arquivo .swf encontrado para este jogo.",
    err_dl_not_a_game: "O download não retornou um arquivo de jogo. Verifique o endereço.",
    err_fp_html_game: "Este jogo inicia por uma página HTML (FlashVars), ainda não suportado.",
    kbd_url_header: "FlashNX - Importar remoto",
    kbd_url_guide: "URL do item do archive.org (ex: https://archive.org/download/your-game-id)",
    kbd_rename_header: "FlashNX - Renomear jogo",
    kbd_rename_guide: "Nome de exibição (deixe vazio para voltar ao nome do arquivo)",
    kbd_search_header: "FlashNX - Buscar",
    kbd_search_guide: "Filtrar por nome de arquivo (vazio = mostrar tudo)",
    bug_no_games: "Nenhum jogo para relatar. Importe ou coloque um .swf primeiro.",
    bug_ok_title: "Obrigado!",
    bug_ok_msg: "Seu relato foi enviado. Obrigado por ajudar a melhorar o FlashNX.",
    bug_fail_title: "Falhou",
    kbd_bug_header: "FlashNX - Relatar um bug",
    kbd_bug_guide: "Abre uma issue pública no GitHub. Descreva o problema. Opcional: seu @usuário. Um breve log técnico é anexado.",
    kbd_suggest_header: "FlashNX - Fazer uma sugestão",
    kbd_suggest_guide: "Abre uma issue pública no GitHub. Sua ideia / sugestão para o FlashNX.",
    suggest_ok_msg: "Sua sugestão foi enviada. Obrigado por ajudar a melhorar o FlashNX.",
};

// Simplified Chinese (issue #41). Prose uses Chinese punctuation; names,
// paths and URLs stay in Latin script.
const ZH: Strings = Strings {
    optimizing: "优化中",
    menu_resume: "继续",
    menu_keys: "按键与光标",
    menu_screen: "屏幕",
    menu_restart: "重新开始游戏",
    menu_quit: "退出游戏",
    pause_title: "暂停",
    keys_title: "按键",
    keys_footer: "A:编辑  L/R:模式  X:玩家1/2  B:返回",
    keys_dropdown_footer: "A:确定   B:取消   上/下:导航",
    keys_modifier: "修饰键",
    none: "(无)",
    flash_mouse_left: "左键单击",
    flash_mouse_right: "右键单击",
    flash_keyboard: "键盘",
    flash_space: "空格",
    flash_enter: "回车",
    flash_escape: "Esc",
    flash_shift: "Shift",
    flash_control: "Ctrl",
    flash_alt: "Alt",
    flash_tab: "Tab",
    flash_backspace: "退格",
    flash_up: "上",
    flash_down: "下",
    flash_left: "左",
    flash_right: "右",
    empty_title: "没有游戏",
    empty_l1: "将 .SWF 文件放入",
    empty_l3: "然后重启 FlashNX。",
    applet_title: "小程序模式",
    applet_notice: "启动游戏需要应用的全部内存，小程序模式没有。在自制软件菜单中，按住 R 选择标题（或使用转发器）以全内存启动 FlashNX。",
    opt_keys: "按键与光标",
    opt_rename: "重命名",
    opt_edit: "编辑",
    opt_folder: "文件夹",
    opt_delete: "删除",
    dist_search_games: "搜索游戏",
    dist_sources: "我的来源",
    dist_empty_hint: "按名称搜索游戏，或粘贴网址",
    dist_add: "+ 添加网址",
    dist_count: "{} 个网址",
    files_title: "远程文件",
    folder_root: "无文件夹",
    dl_title: "下载中",
    toast_dl_ok: "{} 已下载",
    toast_assets_missing: "缺少游戏数据：{}。该游戏可能无法启动。",
    err_title: "错误",
    err_footer: "A/B:确定",
    err_footer_fix: "A/B:确定   Y:修正网址",
    set_keys: "默认按键",
    set_pseudo: "昵称",
    kbd_pseudo_guide: "显示在你分享的配置旁边的名字",
    set_cursor_speed: "光标速度",
    set_display_mode: "显示",
    display_fit: "适应",
    display_fill: "填满",
    display_stretch: "拉伸",
    set_zoom: "缩放",
    set_screen_filter: "滤镜",
    games_dir_new: "新建文件夹",
    games_dir_current: "使用中",
    games_dir_confirm: "将整个游戏库移动到这里？",
    games_dir_confirm_n: "{} 个游戏将移动到：",
    games_dir_confirm_1: "{} 个游戏将移动到：",
    games_dir_confirm_yes: "移动",
    games_dir_confirm_no: "取消",
    kbd_newdir_guide: "要创建的文件夹名称",
    games_dir_moving: "移动中…",
    extracting: "解压中…",
    shortcut_ok: "图标已安装到主页菜单",
    shortcut_err: "无法安装图标",
    shortcut_removed: "图标已移除",
    filter_none: "无",
    filter_scanlines: "扫描线",
    filter_crt: "CRT",
    set_pixel_filter: "像素",
    pixels_auto: "自动",
    pixels_smooth: "平滑",
    pixels_sharp: "锐利",
    set_overclock: "超频",
    overclock_off: "关闭",
    overclock_on: "开启",
    overclock_refused: "超频不可用：电量过低。",
    set_fps: "帧率显示",
    set_rotation: "旋转",
    rot_none: "无",
    rot_90: "90°",
    rot_180: "180°",
    rot_270: "270°",
    show_cursor: "显示光标",
    set_gyro: "陀螺仪",
    gyro_marble: "弹珠",
    gyro_pointer: "指针",
    cursor_shown: "显示",
    cursor_hidden: "隐藏",
    lbl_on: "开",
    lbl_off: "关",
    opt_cover: "封面",
    opt_favorite: "收藏",
    opt_unfavorite: "取消收藏",
    opt_share: "分享按键",
    profile_shared_ok: "你的按键已发送。感谢你为社区做出的贡献。",
    opt_apply: "应用配置",
    profile_title: "按键配置",
    profile_copy_row: "从其他游戏复制",
    profile_copy_none: "还没有其他游戏设置过自己的按键",
    profile_none: "这个游戏还没有配置。分享你的吧！",
    profile_catalog_offline: "目录不可用。请检查网络连接。",
    profile_revert: "恢复我的按键",
    profile_applied_ok: "配置已应用。你之前的按键已保存。",
    profile_reverted_ok: "你的按键已恢复。",
    profile_share_confirm: "分享你为这个游戏设置的按键？",
    share_confirm_update: "更新你已分享的配置：",
    touches_edit: "编辑按键",
    touches_revert_default: "恢复默认按键",
    profile_preview_none: "此配置不会改变任何按键。",
    profile_share_dup: "已在目录中。修改一个按键即可分享你的版本。",
    toast_already_imported: "{} 已经在你的库中。",
    toast_folder_same: "已在此文件夹中。",
    toast_folder_on: "已加入 {}。",
    toast_folder_off_of: "已移出 {}。",
    toast_folder_off: "已移出所有文件夹",
    toast_folder_physical: "卡上位于此文件夹。需移动文件才能移出。",
    toast_name_taken: "{} 已在根目录中。",
    folder_emptied: "文件夹已清空",
    folder_leave_n: "{} 个游戏离开此文件夹",
    folder_leave_1: "{} 个游戏离开此文件夹",
    profile_del_confirm: "删除你分享的配置？",
    profile_del_ok: "你分享的配置已从目录中删除。",
    profile_del_not_mine: "这台主机没有可删除的内容。",
    cover_title: "选择封面",
    cover_show_logos: "Y: 标志",
    cover_show_shots: "Y: 截图",
    cover_none: "无结果",
    fp_title: "Flashpoint",
    fp_details_dev: "开发者",
    fp_details_publisher: "发行商",
    fp_details_date: "发行日期",
    fp_details_size: "下载大小",
    sort_title: "排序方式",
    sort_alpha: "名称",
    sort_recent: "添加时间",
    sort_played: "最常玩",
    sort_size: "大小",
    sort_recent_played: "最近玩过",
    sort_source: "来源",
    sort_files: "文件数量",
    fav_added: "已加入收藏",
    fav_removed: "已取消收藏",
    multifile: "多文件",
    sort_dir_asc: "升序",
    sort_dir_desc: "降序",
    lang_title: "语言",
    histdel_title: "删除这个来源？",
    url_info_type: "类型",
    url_type_swf: "单个 .SWF",
    url_type_list: "文件列表",
    url_info_files: "已在 SD 卡",
    url_info_added: "添加于",
    histdel_msg: "它会从你的来源中移除。已下载的游戏仍保留在存储卡上。",
    err_too_large: "archive.org 响应过大 (>4 MB)。此项目对当前版本来说太大了。",
    err_https: "传输失败 ({})。请稍后重试。",
    err_offline: "找不到地址（curl {}）。请检查主机的 Wi-Fi，然后检查 DNS。",
    err_unreachable: "服务器拒绝了连接（curl {}）。主机在线，中间有东西在阻断。",
    err_timeout: "服务器未及时响应。请重试。",
    err_tls: "安全连接被拒绝。请检查主机的日期和时间：时钟错误会导致 HTTPS 失败。",
    err_response_big: "响应过大。请使用更精确的搜索。",
    err_sd_write: "无法写入 SD 卡。可能已满或被写保护。",
    err_http_status: "服务器拒绝了请求 (HTTP {})。请稍后重试。",
    err_server_busy: "服务器过载 (HTTP {})。链接没有问题，请过几分钟再试。",
    err_not_found: "未找到 (404)。网址或条目 ID 有误，或已被删除。",
    err_json: "无法读取 archive.org 的 JSON: {}",
    err_json_no_files: "JSON 中没有 \"files\" 字段",
    err_dl_start: "无法开始下载（代码 {}）。",
    err_dl_failed: "下载失败（代码 {}）",
    err_dl_cancelled: "下载已被用户取消。",
    err_url_invalid: "网址无效。应为 archive.org 网址，例如 https://archive.org/details/<id>，或直接输入 <id>。",
    err_no_swf: "未找到此游戏的 .SWF 文件。",
    err_dl_not_a_game: "下载返回的不是游戏文件。请检查地址。",
    err_fp_html_game: "此游戏通过 HTML 页面（FlashVars）启动，暂不支持。",
    kbd_url_header: "FlashNX - 在线导入",
    kbd_url_guide: "archive.org 项目网址（例如 https://archive.org/download/your-game-id）",
    kbd_rename_header: "FlashNX - 重命名游戏",
    kbd_rename_guide: "显示名称（留空以恢复文件名）",
    kbd_search_header: "FlashNX - 搜索",
    kbd_search_guide: "按文件名筛选（留空 = 显示全部）",
    bug_no_games: "暂无可报告的游戏。请先导入或放入一个 .SWF。",
    bug_ok_title: "谢谢！",
    bug_ok_msg: "你的报告已发送。感谢你帮助改进 FlashNX。",
    bug_fail_title: "失败",
    kbd_bug_header: "FlashNX - 报告问题",
    kbd_bug_guide: "将打开一个公开的 GitHub issue。请描述问题。可选：你的 @用户名。将附带一份简短的技术日志。",
    kbd_suggest_header: "FlashNX - 提出建议",
    kbd_suggest_guide: "将打开一个公开的 GitHub issue。你对 FlashNX 的想法 / 建议。",
    suggest_ok_msg: "你的建议已发送。感谢你帮助改进 FlashNX。",
};

// Turkish.
const TR: Strings = Strings {
    optimizing: "Optimize ediliyor",
    menu_resume: "Devam et",
    menu_keys: "Kontroller ve imleç",
    menu_screen: "Ekran",
    menu_restart: "Oyunu yeniden başlat",
    menu_quit: "Oyundan çık",
    pause_title: "Duraklatıldı",
    keys_title: "Kontroller",
    keys_footer: "A:Düzenle  L/R:Mod  X:O1/O2  B:Geri",
    keys_dropdown_footer: "A:Tamam   B:İptal   yukarı/aşağı",
    keys_modifier: "Değiştirici",
    none: "(yok)",
    flash_mouse_left: "Sol tık",
    flash_mouse_right: "Sağ tık",
    flash_keyboard: "Klavye",
    flash_space: "Boşluk",
    flash_enter: "Enter",
    flash_escape: "Esc",
    flash_shift: "Shift",
    flash_control: "Ctrl",
    flash_alt: "Alt",
    flash_tab: "Tab",
    flash_backspace: "Geri sil",
    flash_up: "Yukarı",
    flash_down: "Aşağı",
    flash_left: "Sol",
    flash_right: "Sağ",
    empty_title: "Oyun yok",
    empty_l1: ".swf dosyalarını şuraya koyun",
    empty_l3: "Sonra FlashNX'ı yeniden başlatın.",
    applet_title: "Applet modu",
    applet_notice: "Bir oyun başlatmak uygulamanın tüm belleğini gerektirir, applet modunda bu bellek yoktur. Homebrew menüsünde bir oyuna (veya forwarder'a) R tuşunu basılı tutarak FlashNX'ı tam bellekle başlatın.",
    opt_keys: "Kontroller ve imleç",
    opt_rename: "Yeniden adlandır",
    opt_edit: "Düzenle",
    opt_folder: "Klasörler",
    opt_delete: "Sil",
    dist_search_games: "Oyun ara",
    dist_sources: "Kaynaklarım",
    dist_empty_hint: "Oyunu adıyla arayın veya bir adres yapıştırın",
    dist_add: "+ URL ekle",
    dist_count: "{} URL",
    files_title: "Uzak dosyalar",
    folder_root: "Klasörsüz",
    dl_title: "İndiriliyor",
    toast_dl_ok: "{} indirildi",
    toast_assets_missing: "Oyun verileri eksik: {}. Oyun başlamayabilir.",
    err_title: "Hata",
    err_footer: "A/B:Tamam",
    err_footer_fix: "A/B:Tamam   Y:URL'yi düzelt",
    set_keys: "Varsayılan kontroller",
    set_pseudo: "Takma ad",
    kbd_pseudo_guide: "Paylaştığın profillerde görünen adın",
    set_cursor_speed: "İmleç hızı",
    set_display_mode: "Ekran",
    display_fit: "Sığdır",
    display_fill: "Doldur",
    display_stretch: "Ger",
    set_zoom: "Zoom",
    set_screen_filter: "Filtre",
    games_dir_new: "Yeni klasör",
    games_dir_current: "kullanımda",
    games_dir_confirm: "Tüm kütüphane buraya taşınsın mı?",
    games_dir_confirm_n: "{} oyun şuraya taşınacak:",
    games_dir_confirm_1: "{} oyun şuraya taşınacak:",
    games_dir_confirm_yes: "Taşı",
    games_dir_confirm_no: "İptal",
    kbd_newdir_guide: "Oluşturulacak klasörün adı",
    games_dir_moving: "Taşınıyor…",
    extracting: "Çıkartılıyor…",
    shortcut_ok: "Kutucuk ana menüye eklendi",
    shortcut_err: "Kutucuk yüklenemedi",
    shortcut_removed: "Kutucuk kaldırıldı",
    filter_none: "Yok",
    filter_scanlines: "Çizgiler",
    filter_crt: "CRT",
    set_pixel_filter: "Pikseller",
    pixels_auto: "Otomatik",
    pixels_smooth: "Yumuşak",
    pixels_sharp: "Keskin",
    set_overclock: "Overclock",
    overclock_off: "Kapalı",
    overclock_on: "Açık",
    overclock_refused: "Overclock kullanılamaz: pil çok düşük.",
    set_fps: "FPS sayacı",
    set_rotation: "Döndürme",
    rot_none: "Yok",
    rot_90: "90°",
    rot_180: "180°",
    rot_270: "270°",
    show_cursor: "İmleci göster",
    set_gyro: "Jiroskop",
    gyro_marble: "Bilye",
    gyro_pointer: "İşaretçi",
    cursor_shown: "Görünür",
    cursor_hidden: "Gizli",
    lbl_on: "Açık",
    lbl_off: "Kapalı",
    opt_cover: "Kapak",
    opt_favorite: "Favori",
    opt_unfavorite: "Favoriden çıkar",
    opt_share: "Kontrolleri paylaş",
    profile_shared_ok: "Kontrollerin gönderildi. Topluluğa yardım için teşekkürler.",
    opt_apply: "Profil uygula",
    profile_title: "Kontrol profilleri",
    profile_copy_row: "Başka bir oyundan kopyala",
    profile_copy_none: "Henüz kendi tuşları olan başka oyun yok",
    profile_none: "Bu oyun için henüz profil yok. Kendininkini paylaş!",
    profile_catalog_offline: "Katalog kullanılamıyor. Bağlantını kontrol et.",
    profile_revert: "Kendi kontrollerime dön",
    profile_applied_ok: "Profil uygulandı. Önceki kontrollerin kaydedildi.",
    profile_reverted_ok: "Kontrollerin geri yüklendi.",
    profile_share_confirm: "Bu oyun için kontrollerin paylaşılsın mı?",
    share_confirm_update: "Paylaşılan profilini günceller:",
    touches_edit: "Kontrolleri düzenle",
    touches_revert_default: "Varsayılan kontrollere dön",
    profile_preview_none: "Bu profil hiçbir tuşunu değiştirmez.",
    profile_share_dup: "Zaten katalogda. Kendi sürümünü paylaşmak için bir tuş değiştir.",
    toast_already_imported: "{} zaten kütüphanende.",
    toast_folder_same: "Zaten bu klasörde.",
    toast_folder_on: "{} klasörüne eklendi",
    toast_folder_off_of: "{} klasöründen çıkarıldı",
    toast_folder_off: "Tüm klasörlerden çıkarıldı",
    toast_folder_physical: "Kartta bu klasörde. Çıkarmak için taşıyın.",
    toast_name_taken: "{} kök dizinde zaten var.",
    folder_emptied: "Klasör boşaltıldı",
    folder_leave_n: "{} oyun bu klasörden çıkarıldı",
    folder_leave_1: "{} oyun bu klasörden çıkarıldı",
    profile_del_confirm: "Paylaşılan profilin silinsin mi?",
    profile_del_ok: "Paylaşılan profilin silindi.",
    profile_del_not_mine: "Silinecek bir şey yok: bu konsoldan paylaşılmadı.",
    cover_title: "Kapak seç",
    cover_show_logos: "Y: Logolar",
    cover_show_shots: "Y: Ekran imgeleri",
    cover_none: "Sonuç yok",
    fp_title: "Flashpoint",
    fp_details_dev: "Geliştirici",
    fp_details_publisher: "Yayıncı",
    fp_details_date: "Çıkış tarihi",
    fp_details_size: "İndirme boyutu",
    sort_title: "Sıralama",
    sort_alpha: "Ad",
    sort_recent: "Eklenme",
    sort_played: "En çok oynanan",
    sort_size: "Boyut",
    sort_recent_played: "Son oynanan",
    sort_source: "Kaynak",
    sort_files: "Dosya sayısı",
    fav_added: "Favorilere eklendi",
    fav_removed: "Favorilerden çıkarıldı",
    multifile: "Çok dosyalı",
    sort_dir_asc: "Artan",
    sort_dir_desc: "Azalan",
    lang_title: "Dil",
    histdel_title: "Bu kaynak silinsin mi?",
    url_info_type: "Tür",
    url_type_swf: "Tek .swf",
    url_type_list: "Dosya listesi",
    url_info_files: "SD kartta",
    url_info_added: "Eklendi",
    histdel_msg: "Kaynaklarından kalkar. İndirilmiş oyunlar kartta kalır.",
    err_too_large: "archive.org yanıtı çok büyük (>4 MB). Bu sürüm için çok büyük.",
    err_https: "Aktarım başarısız ({}). Biraz sonra tekrar deneyin.",
    err_offline: "Adres bulunamadı (curl {}). Konsolun Wi-Fi bağlantısını, sonra DNS'ini kontrol edin.",
    err_unreachable: "Sunucu bağlantıyı reddetti (curl {}). Konsol çevrimiçi, arada bir şey engelliyor.",
    err_timeout: "Sunucu zamanında yanıt vermedi. Tekrar deneyin.",
    err_tls: "Güvenli bağlantı reddedildi. Konsolun tarih ve saatini kontrol edin: yanlış saat HTTPS'i bozar.",
    err_response_big: "Yanıt çok büyük. Daha dar bir arama deneyin.",
    err_sd_write: "SD karta yazılamadı. Dolu olabilir.",
    err_http_status: "Sunucu isteği reddetti (HTTP {}). Daha sonra deneyin.",
    err_server_busy: "Sunucu aşırı yüklü (HTTP {}). Bağlantı doğru, birkaç dakika sonra deneyin.",
    err_not_found: "Bulunamadı (404). URL veya öğe kimliği yanlış ya da kaldırılmış.",
    err_json: "Okunamayan archive.org JSON: {}",
    err_json_no_files: "JSON'da \"files\" alanı yok",
    err_dl_start: "İndirme başlatılamadı (kod {}).",
    err_dl_failed: "İndirme başarısız (kod {})",
    err_dl_cancelled: "İndirme kullanıcı tarafından iptal edildi.",
    err_url_invalid: "Geçersiz URL. https://archive.org/details/<id> gibi bir URL veya yalnızca <id> bekleniyor.",
    err_no_swf: "Bu oyun için .swf dosyası bulunamadı.",
    err_dl_not_a_game: "İndirme bir oyun dosyası döndürmedi. Adresi kontrol edin.",
    err_fp_html_game: "Bu oyun bir HTML sayfasından (FlashVars) başlar, henüz desteklenmiyor.",
    kbd_url_header: "FlashNX - Uzaktan içe aktarma",
    kbd_url_guide: "archive.org öğe URL'si (örn. https://archive.org/download/oyun-kimligi)",
    kbd_rename_header: "FlashNX - Oyunu yeniden adlandır",
    kbd_rename_guide: "Görünen ad (dosya adına dönmek için boş bırakın)",
    kbd_search_header: "FlashNX - Ara",
    kbd_search_guide: "Dosya adına göre filtrele (boş = hepsini göster)",
    bug_no_games: "Bildirilecek oyun yok. Önce bir .swf aktarın.",
    bug_ok_title: "Teşekkürler!",
    bug_ok_msg: "Raporun gönderildi. FlashNX'ı geliştirmeye yardım için teşekkürler.",
    bug_fail_title: "Başarısız",
    kbd_bug_header: "FlashNX - Hata bildir",
    kbd_bug_guide: "Herkese açık bir GitHub konusu açar. Sorunu anlat. İsteğe bağlı: @kullanıcı adın. Kısa bir teknik günlük eklenir.",
    kbd_suggest_header: "FlashNX - Öneride bulun",
    kbd_suggest_guide: "Herkese açık bir GitHub konusu açar. FlashNX için fikrin / özellik isteğin.",
    suggest_ok_msg: "Önerin gönderildi. FlashNX'ı geliştirmeye yardım için teşekkürler.",
};

/// Current language, as a `Lang` index. Default English; overridden by
/// `init()` at boot from settings.json / system language.
static CURRENT: AtomicU8 = AtomicU8::new(0);

pub fn current() -> Lang {
    Lang::from_index(CURRENT.load(Ordering::Relaxed) as usize)
}

pub fn set(lang: Lang) {
    // A CJK language is only usable if the glyph atlas can exist: its strings
    // are drawn ENTIRELY from the shared font, so without it the whole UI
    // renders blank, not just a label. Applet mode cannot afford that font
    // (see `glyphs::cjk_possible`), so fall back to English there rather than
    // hand someone an invisible interface.
    let lang = if lang.needs_cjk() && !cjk_drawable() {
        Lang::En
    } else {
        lang
    };
    CURRENT.store(lang.index() as u8, Ordering::Relaxed);
}

/// v1.2.0 — whether the per-game "JAQUETTE" action may reach Flashpoint to
/// fetch cover art. OFF by default: with it off, FlashNX makes ZERO unsolicited
/// network requests (covers come only from local sidecars/cache). Persisted in
/// settings.json alongside the language.
static COVERS_ONLINE: AtomicBool = AtomicBool::new(false);

pub fn covers_online() -> bool {
    // v1.2.0: online covers are always available (no user toggle). Kept as a
    // function so the persisted setting stays consistent and a toggle could
    // come back later without touching call sites.
    let _ = COVERS_ONLINE.load(Ordering::Relaxed);
    true
}

pub fn set_covers_online(v: bool) {
    COVERS_ONLINE.store(v, Ordering::Relaxed);
}

/// Default stage-scaling mode and screen filter, for games that have never been
/// set from their own pause menu. Persisted in settings.json.
///
/// The rule is deliberately flat: a game WITH a sidecar wins, a game WITHOUT it
/// follows these. No third "inherit" state is exposed anywhere, because the only
/// way to show one honestly would be a fourth position in the in-game cycle whose
/// meaning is abstract. The price is that a game set once stops following a later
/// change of default, which is predictable and explainable.
static DEFAULT_DISPLAY_MODE: AtomicU8 = AtomicU8::new(0);
static DEFAULT_SCREEN_FILTER: AtomicU8 = AtomicU8::new(0);
/// Default texture sampling (issue #108). 0 = AUTO, and it stays 0: AUTO is the
/// only value that is not a preference but a fidelity guarantee, since it is
/// what Flash Player itself did with the SWF's `smoothing` flag. A player who
/// wants every game sharp can move this; nobody gets moved off faithful by
/// never opening the menu.
static DEFAULT_PIXEL_FILTER: AtomicU8 = AtomicU8::new(0);
/// Default power setting. AUTO since 2026-10-08 (it was OFF): see
/// `keymap::power_mode_for`.
static DEFAULT_POWER_MODE: AtomicU8 = AtomicU8::new(crate::keymap::POWER_AUTO);
static DEFAULT_ROTATION: AtomicU8 = AtomicU8::new(0);
static DEFAULT_ZOOM: core::sync::atomic::AtomicU16 = core::sync::atomic::AtomicU16::new(100);

/// JOUER tab layout: 0 = cover grid (the default since v1.2.0), 1 = title list
/// with the selected game's cover beside it (issue #52), 2 = covers scrolling
/// sideways with the selected one shown large. Persisted in settings.json.
///
/// A view, not a filter: the same games, the same order, the same actions. Which
/// one suits depends on the library and on the covers in it, so it stays a
/// choice. The grid remains the default and nothing about it changes.
static HOME_VIEW: AtomicU8 = AtomicU8::new(0);

/// RÉGLAGES > interface sounds. On by default.
static UI_SOUNDS: AtomicBool = AtomicBool::new(true);

pub fn ui_sounds() -> bool {
    UI_SOUNDS.load(Ordering::Relaxed)
}

pub fn set_ui_sounds(on: bool) {
    UI_SOUNDS.store(on, Ordering::Relaxed);
    crate::sfx::set_enabled(on);
}

/// Number of layouts, for cycling the REGLAGES row.
pub const HOME_VIEW_COUNT: u8 = 4;

pub fn home_view() -> u8 {
    HOME_VIEW.load(Ordering::Relaxed) % HOME_VIEW_COUNT
}

pub fn set_home_view(v: u8) {
    HOME_VIEW.store(v % HOME_VIEW_COUNT, Ordering::Relaxed);
}

/// Value of the TOUCHES > GYROSCOPE row (#119), for `keymap::GYRO_*`.
pub fn gyro_mode_label(mode: u8) -> &'static str {
    match mode {
        crate::keymap::GYRO_MARBLE => s().gyro_marble,
        crate::keymap::GYRO_POINTER => s().gyro_pointer,
        _ => s().overclock_off,
    }
}

pub fn default_display_mode() -> u8 {
    DEFAULT_DISPLAY_MODE.load(Ordering::Relaxed)
}

pub fn set_default_display_mode(v: u8) {
    DEFAULT_DISPLAY_MODE.store(v, Ordering::Relaxed);
}

pub fn default_rotation() -> u8 {
    DEFAULT_ROTATION.load(Ordering::Relaxed)
}

pub fn set_default_rotation(v: u8) {
    DEFAULT_ROTATION.store(v, Ordering::Relaxed);
}

/// Display name of a rotation. Stored per game like the scaling mode, because
/// turning the picture is a property of the game: a portrait game wants it
/// always and a landscape game never.
pub fn rotation_label(v: u8) -> &'static str {
    match v {
        1 => s().rot_90,
        2 => s().rot_180,
        3 => s().rot_270,
        _ => s().rot_none,
    }
}

/// Zoom a game starts at when it has never been framed itself (issue #101).
///
/// PRESETS, not the free percentage the pause menu gives. There is no picture
/// on screen in REGLAGES to judge one against, and a value picked blind is not
/// a value anybody chose -- so this offers the handful worth defaulting to, and
/// leaves the pixel-by-pixel work to the game it applies to.
///
/// The framing offset has NO global default and never will: it is measured in
/// screen pixels against one particular picture, and means nothing away from it.
pub const DEFAULT_ZOOM_STEPS: &[u16] = &[100, 110, 125, 150, 175, 200, 250, 300];

pub fn default_zoom() -> u16 {
    DEFAULT_ZOOM.load(Ordering::Relaxed)
}

pub fn set_default_zoom(v: u16) {
    DEFAULT_ZOOM.store(v, Ordering::Relaxed);
}

/// Step to the next preset, wrapping. A value that is not one of them (hand
/// edited into settings.json) lands on the first.
pub fn step_default_zoom(dir: i32) {
    let cur = default_zoom();
    let n = DEFAULT_ZOOM_STEPS.len();
    let idx = DEFAULT_ZOOM_STEPS.iter().position(|v| *v == cur);
    let next = match idx {
        Some(i) if dir < 0 => DEFAULT_ZOOM_STEPS[(i + n - 1) % n],
        Some(i) => DEFAULT_ZOOM_STEPS[(i + 1) % n],
        None => DEFAULT_ZOOM_STEPS[0],
    };
    set_default_zoom(next);
}

pub fn default_screen_filter() -> u8 {
    DEFAULT_SCREEN_FILTER.load(Ordering::Relaxed)
}

pub fn set_default_screen_filter(v: u8) {
    DEFAULT_SCREEN_FILTER.store(v, Ordering::Relaxed);
}

pub fn default_pixel_filter() -> u8 {
    DEFAULT_PIXEL_FILTER.load(Ordering::Relaxed)
}

pub fn set_default_pixel_filter(v: u8) {
    DEFAULT_PIXEL_FILTER.store(v, Ordering::Relaxed);
}

pub fn default_power_mode() -> u8 {
    DEFAULT_POWER_MODE.load(Ordering::Relaxed)
}

pub fn set_default_power_mode(v: u8) {
    DEFAULT_POWER_MODE.store(v, Ordering::Relaxed);
}

/// Default for the FPS counter (issue #111). 0 = hidden, and it stays 0: a
/// counter is an instrument, and an instrument nobody asked for is furniture
/// over the corner of every game they own.
static DEFAULT_FPS_COUNTER: AtomicU8 = AtomicU8::new(0);

pub fn default_fps_counter() -> u8 {
    DEFAULT_FPS_COUNTER.load(Ordering::Relaxed)
}

pub fn set_default_fps_counter(v: u8) {
    DEFAULT_FPS_COUNTER.store(v, Ordering::Relaxed);
}

/// Display name for the FPS counter row. Borrows the show-cursor pair rather
/// than adding a second set of nine translations that would say the same thing:
/// the question is identical (something drawn over the game is shown or it is
/// not) and so is the answer in every language we ship.
pub fn fps_counter_label(v: u8) -> &'static str {
    if v == 1 { s().cursor_shown } else { s().cursor_hidden }
}

/// Display name of a power mode. Stored PER GAME (`keymap::power_mode_for`);
/// this side is only the default a game without its own value follows.
pub fn overclock_label(mode: u8) -> &'static str {
    match mode {
        1 => s().overclock_on,
        crate::keymap::POWER_AUTO => s().pixels_auto,
        _ => s().overclock_off,
    }
}

/// Display name of a stage-scaling mode. The value itself is stored PER GAME
/// (see `keymap::display_mode_for`), not in settings.json: filling the screen
/// costs cropping, and how much that costs depends entirely on the game. A 4:3
/// game loses a little top and bottom; a portrait game like Flappy Bird
/// (500x700) would lose about 60% of its playfield.
/// Cycle order: INTEGRAL -> ETIRER -> REMPLIR. The stretch sits before the fill
/// on purpose, because the first press should land on the mode that hides
/// nothing: stretching distorts but still shows 100% of the game, while filling
/// crops up to a quarter of a 4:3 game's height, which is where its score and
/// life bars live.
pub fn display_mode_label(v: u8) -> &'static str {
    match v {
        1 => s().display_stretch,
        2 => s().display_fill,
        _ => s().display_fit,
    }
}

/// Display name of a screen filter. Stored per game like the scaling mode.
pub fn screen_filter_label(v: u8) -> &'static str {
    match v {
        1 => s().filter_scanlines,
        2 => s().filter_crt,
        _ => s().filter_none,
    }
}

/// Display name of a texture-sampling mode (issue #108). Stored per game like
/// the scaling mode, because which games are hurt by bilinear is a property of
/// the ART: a global SHARP would trade the blur on pixel-art games for hard
/// edges on the vector-drawn ones, which are most of the library.
///
/// Cycle order AUTO -> SMOOTH -> SHARP. AUTO first because it is the faithful
/// value and the one a game comes back to; SMOOTH before SHARP because SMOOTH
/// is what the player was already getting, so the first press off AUTO changes
/// nothing visible and the second is the one that answers the complaint.
pub fn pixel_filter_label(v: u8) -> &'static str {
    match v {
        1 => s().pixels_smooth,
        2 => s().pixels_sharp,
        _ => s().pixels_auto,
    }
}

/// Cursor-speed presets as x10 multipliers, in index order. MUST stay in sync
/// with `CURSOR_SPEED_MULTS` in cpp/src/main.cpp (the C++ side owns the live
/// value; this side only shows it).
pub const CURSOR_SPEED_X10: &[u32] = &[5, 10, 15, 20, 25, 30, 40, 50];

/// A multiplier as the menus show it: "×1,5" in French, "×1.5" in English.
pub fn mult_label(x10: u32) -> std::string::String {
    let mark = match current() {
        Lang::En | Lang::Zh => '.',
        _ => ',',
    };
    std::format!("\u{00D7}{}{}{}", x10 / 10, mark, x10 % 10)
}

/// A cursor-speed preset (by index) as the menus show it.
pub fn speed_label(idx: i32) -> std::string::String {
    let i = idx.clamp(0, CURSOR_SPEED_X10.len() as i32 - 1) as usize;
    mult_label(CURSOR_SPEED_X10[i])
}

/// The active language's string table. Short name because it's called a lot.
pub fn s() -> &'static Strings {
    match current() {
        Lang::En => &EN,
        Lang::Fr => &FR,
        Lang::Es => &ES,
        Lang::Ru => &RU,
        Lang::De => &DE,
        Lang::It => &IT,
        Lang::Pt => &PT,
        Lang::Zh => &ZH,
        Lang::Tr => &TR,
    }
}

// ── Dynamic strings ─────────────────────────────────────────────────────

/// "{n} .swf file(s) found" with real singular/plural agreement per language
/// (no literal "(s)"). Russian uses a count-style phrasing that sidesteps its
/// 3-form agreement.
pub fn files_found(n: usize) -> std::string::String {
    match current() {
        Lang::En if n == 1 => "1 .swf file found".into(),
        Lang::En => std::format!("{} .swf files found", n),
        // French: 0 and 1 are singular.
        Lang::Fr if n <= 1 => std::format!("{} fichier .swf trouvé", n),
        Lang::Fr => std::format!("{} fichiers .swf trouvés", n),
        Lang::Es if n == 1 => "1 archivo .swf encontrado".into(),
        Lang::Es => std::format!("{} archivos .swf encontrados", n),
        Lang::Ru => std::format!("Найдено файлов .swf: {}", n),
        // German: 0 takes the plural ("0 Dateien").
        Lang::De if n == 1 => "1 .swf-Datei gefunden".into(),
        Lang::De => std::format!("{} .swf-Dateien gefunden", n),
        Lang::It if n == 1 => "1 file .swf trovato".into(),
        Lang::It => std::format!("{} file .swf trovati", n),
        Lang::Pt if n == 1 => "1 arquivo .swf encontrado".into(),
        Lang::Pt => std::format!("{} arquivos .swf encontrados", n),
        // Chinese has no plural agreement: a count phrasing reads naturally.
        Lang::Zh => std::format!("\u{627E}\u{5230} .swf \u{6587}\u{4EF6}: {}", n), // 找到 .swf 文件: {}
        // Turkish has no plural after a number (the numeral already marks it).
        Lang::Tr => std::format!("{} .swf dosyası bulundu", n),
    }
}

/// "3 / 21 · “mario”", the sub-line a list screen shows while a search is
/// active; `plain` supplies the wording used when there is none.
///
/// The three list screens each composed this by hand, and the copies had already
/// drifted: two of them treated a search of nothing but spaces as an ACTIVE
/// filter and printed a "FILTRE:" with an empty term after it, while the third
/// trimmed first. The separator was just as free to drift, in a line the user
/// reads on every screen of the app.
pub fn count_line(
    shown: usize,
    total: usize,
    filter: Option<&str>,
    plain: impl FnOnce() -> std::string::String,
) -> std::string::String {
    match filter {
        Some(f) if !f.trim().is_empty() => {
            std::format!("{} / {}  \u{00B7}  \u{201C}{}\u{201D}", shown, total, f.trim())
        }
        _ => plain(),
    }
}

/// Substitute a single `{}` placeholder in one of the error templates.
fn fill(template: &str, arg: &str) -> std::string::String {
    template.replacen("{}", arg, 1)
}

/// A counted message, in the form the count actually calls for.
///
/// English, French, Spanish, German, Italian and Portuguese all inflect on one
/// versus many, and writing "jeu(x)" to dodge that is a form nobody says out
/// loud. Chinese and Turkish do not inflect at all and simply pass the same
/// string twice.
///
/// Russian really wants three forms (1, 2-4, 5+); it gets the nominative for one
/// and the genitive plural for the rest, which is right everywhere except 2-4.
/// Correcting that means a third field for one language, and it is a smaller
/// error than the brackets it replaces.
pub fn fill_count(one: &'static str, many: &'static str, n: i32) -> std::string::String {
    fill(if n == 1 { one } else { many }, &n.to_string())
}

/// Fill a one-placeholder message with a number. The code goes ON SCREEN on
/// purpose: a player who reports "it says check my WiFi" tells us nothing, while
/// "curl 6" and "curl 7" are two different faults with two different fixes, and
/// most reports arrive without a log (#103).
pub fn fill_one(template: &'static str, value: i32) -> std::string::String {
    fill(template, &value.to_string())
}

/// Same, for a one-placeholder message whose argument is a name.
pub fn fill_name(template: &'static str, name: &str) -> std::string::String {
    fill(template, name)
}

pub fn err_https(detail: &str) -> std::string::String {
    fill(s().err_https, detail)
}
pub fn err_json(detail: &str) -> std::string::String {
    fill(s().err_json, detail)
}
pub fn err_dl_start(code: i32) -> std::string::String {
    fill(s().err_dl_start, &code.to_string())
}
pub fn err_dl_failed(code: i32) -> std::string::String {
    fill(s().err_dl_failed, &code.to_string())
}
/// HTTP status the server answered with (>= 400). 404 gets its own wording —
/// "not found" is a different action for the user than "the server said no".
pub fn err_http_status(code: i64) -> std::string::String {
    if code == 404 {
        return std::string::String::from(s().err_not_found);
    }
    // Up but temporarily unable, which asks the player to wait rather than to
    // check their link. 429 is the rate limit; 500/502/503/504 are what a loaded
    // archive.org answers, and the Wayback Machine hands out 503 by the fistful
    // when it is busy.
    if matches!(code, 429 | 500 | 502 | 503 | 504) {
        return fill(s().err_server_busy, &code.to_string());
    }
    fill(s().err_http_status, &code.to_string())
}

// ── Persistence + boot init ─────────────────────────────────────────────

/// Settings live beside the library and travel with it (#79); the built-in
/// roots stay as a read fallback for installs that predate the move.
fn settings_read_path() -> Option<std::string::String> {
    let p = crate::library::config_read_path("settings.json");
    if std::path::Path::new(&p).exists() {
        return Some(p);
    }
    None
}

fn settings_write_path() -> std::string::String {
    crate::library::config_write_path("settings.json")
}

/// Pull the `"language"` value out of settings.json with a tiny hand parser
/// (no serde struct needed for one field). Looks for `"language"` then the
/// next quoted token.
fn parse_language(json: &str) -> Option<Lang> {
    let idx = json.find("\"language\"")?;
    let rest = &json[idx + "\"language\"".len()..];
    let colon = rest.find(':')?;
    let after = &rest[colon + 1..];
    let q1 = after.find('"')?;
    let after2 = &after[q1 + 1..];
    let q2 = after2.find('"')?;
    Lang::from_code(&after2[..q2])
}

fn read_small_file(path: &str) -> Option<std::string::String> {
    let mut file = File::open(path).ok()?;
    let mut data = std::vec::Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        match file.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => data.extend_from_slice(&buf[..n]),
            Err(_) => return None,
        }
    }
    std::string::String::from_utf8(data).ok()
}

/// Parse the boolean `"covers_online"` value out of settings.json (tiny hand
/// parser, like `parse_language`). Absent → None (keep the default).
fn parse_covers_online(json: &str) -> Option<bool> {
    let idx = json.find("\"covers_online\"")?;
    let rest = &json[idx + "\"covers_online\"".len()..];
    let colon = rest.find(':')?;
    let after = rest[colon + 1..].trim_start();
    if after.starts_with("true") {
        Some(true)
    } else if after.starts_with("false") {
        Some(false)
    } else {
        None
    }
}

/// Parse a small unsigned integer setting out of settings.json. Absent or out of
/// range → None (keep the default). Same tiny hand parser as the others.
fn parse_u8_setting(json: &str, key: &str, max: u8) -> Option<u8> {
    let needle = std::format!("\"{}\"", key);
    let idx = json.find(&needle)?;
    let rest = &json[idx + needle.len()..];
    let colon = rest.find(':')?;
    let after = rest[colon + 1..].trim_start();
    let digits: std::string::String = after.chars().take_while(|c| c.is_ascii_digit()).collect();
    digits.parse::<u8>().ok().filter(|v| *v < max)
}

/// Same, for a value that does not fit in a byte. The zoom is a percentage that
/// runs to 500 (issue #101), and `lo`/`hi` are INCLUSIVE here because a
/// percentage has a meaningful upper value, unlike the counts above where `max`
/// is one past the last valid index.
fn parse_u16_setting(json: &str, key: &str, lo: u16, hi: u16) -> Option<u16> {
    let needle = std::format!("\"{}\"", key);
    let idx = json.find(&needle)?;
    let rest = &json[idx + needle.len()..];
    let colon = rest.find(':')?;
    let after = rest[colon + 1..].trim_start();
    let digits: std::string::String = after.chars().take_while(|c| c.is_ascii_digit()).collect();
    digits.parse::<u16>().ok().filter(|v| (lo..=hi).contains(v))
}

/// Write the full settings file. Everything persists together so saving one
/// setting never drops another.
fn write_settings(lang: Lang, covers: bool) -> bool {
    let path = settings_write_path();
    let json = std::format!(
        "{{\n    \"language\": \"{}\",\n    \"covers_online\": {},\n    \"display_mode\": {},\n    \"rotation\": {},\n    \"zoom\": {},\n    \"pixel_filter\": {},\n    \"screen_filter\": {},\n    \"overclock\": {},\n    \"fps_counter\": {},\n    \"home_view\": {},\n    \"ui_sounds\": {}\n}}\n",
        lang.code(),
        covers,
        default_display_mode(),
        default_rotation(),
        default_zoom(),
        default_pixel_filter(),
        default_screen_filter(),
        default_power_mode(),
        default_fps_counter(),
        home_view(),
        ui_sounds() as u8,
    );
    match File::create(&path) {
        Ok(mut f) => {
            let ok = f.write_all(json.as_bytes()).is_ok();
            if ok {
                // Flush so the choice survives a mode switch / abrupt exit.
                crate::sd::commit();
            }
            ok
        }
        Err(_) => false,
    }
}

/// Persist the chosen language. Best-effort. (The covers-online flag is always
/// on in v1.2.0 — no toggle — but stays in the file for forward-compat.)
pub fn save(lang: Lang) -> bool {
    write_settings(lang, covers_online())
}

/// Persist the current settings without changing the language. Used when a
/// default is cycled in REGLAGES. Best-effort.
pub fn save_current() -> bool {
    write_settings(current(), covers_online())
}

extern "C" {
    /// Switch system language → our index (0 En, 1 Fr, 2 Es, 3 Ru, 4 De,
    /// 5 It, 6 Pt, 7 Zh), or -1 if unsupported / detection failed. Defined in
    /// cpp/src/ruffle_bridge.cpp.
    fn ruffle_detect_system_lang() -> core::ffi::c_int;
}

/// Choose the boot language: settings.json if present, else the Switch
/// system language, else English. Called once from `ruffle_library_init`.
pub fn init() {
    // 1. Explicit user choice persisted on SD wins.
    if let Some(path) = settings_read_path() {
        if let Some(txt) = read_small_file(&path) {
            if let Some(c) = parse_covers_online(&txt) {
                set_covers_online(c);
            }
            // Read BEFORE the language early-return below, or these would only be
            // picked up on consoles whose settings.json has no language.
            if let Some(v) = parse_u8_setting(&txt, "display_mode", 3) {
                set_default_display_mode(v);
            }
            if let Some(v) = parse_u8_setting(&txt, "rotation", 4) {
                set_default_rotation(v);
            }
            if let Some(v) = parse_u16_setting(&txt, "zoom", 100, 500) {
                set_default_zoom(v);
            }
            // `pixel_filter` is the texture sampling (issue #108) and
            // `screen_filter` the CRT post-process. Two keys, two settings; the
            // names are one word apart on purpose so a hand-edited file cannot
            // set one thinking it set the other.
            if let Some(v) = parse_u8_setting(&txt, "pixel_filter", 3) {
                set_default_pixel_filter(v);
            }
            if let Some(v) = parse_u8_setting(&txt, "screen_filter", 3) {
                set_default_screen_filter(v);
            }
            // `overclock` (0 OFF, 1 ON, 2 AUTO) replaced `power_mode` (0/1) when
            // AUTO became the default. A `power_mode` of 0 was written by every
            // save of this file whether or not the player ever touched the row,
            // so it says nothing and leaves the new default in place; a 1 was
            // always a choice, and is kept.
            if let Some(v) = parse_u8_setting(&txt, "overclock", crate::keymap::POWER_MODE_COUNT) {
                set_default_power_mode(v);
            } else if parse_u8_setting(&txt, "power_mode", 2) == Some(1) {
                set_default_power_mode(1);
            }
            if let Some(v) = parse_u8_setting(&txt, "fps_counter", 2) {
                set_default_fps_counter(v);
            }
            if let Some(v) = parse_u8_setting(&txt, "home_view", HOME_VIEW_COUNT) {
                set_home_view(v);
            }
            if let Some(v) = parse_u8_setting(&txt, "ui_sounds", 2) {
                set_ui_sounds(v == 1);
            }
            if let Some(lang) = parse_language(&txt) {
                set(lang);
                return;
            }
        }
    }
    // 2. Otherwise follow the console's system language if we support it.
    let sys = unsafe { ruffle_detect_system_lang() };
    if sys >= 0 && sys <= 7 {
        set(Lang::from_index(sys as usize));
        return;
    }
    // 3. Fallback: English (already the default).
}
