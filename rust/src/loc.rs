//! UI string localization (EN / FR / ES / RU / DE / IT / PT / ZH).
//!
//! All on-screen text routes through `draw_text` (render.rs), which only
//! carries an uppercase pixel font. ASCII lowercase is folded to uppercase
//! at draw time, but non-ASCII (Cyrillic) is NOT folded — so Russian strings
//! here are written in UPPERCASE Cyrillic, matching the glyphs added to the
//! font. Latin strings are written in uppercase too (display is uppercase
//! regardless) so the source reads like the screen.
//!
//! Lookup is a plain `&'static Strings` per language, chosen by a global
//! atomic set at boot from `settings.json` (or auto-detected from the Switch
//! system language) and changeable at runtime via the Settings modal.
//!
//! Flash KEY names ("Space", "Shift", "A".."Z") are technical identifiers
//! stored in keymap JSON and are intentionally NOT translated — only the
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
    /// Native display name shown in the language picker. Uppercase, no
    /// accents for Latin (font has none); Russian in uppercase Cyrillic.
    /// Name for the picker. A CJK name is drawn from the shared-font atlas, so
    /// when that font cannot be afforded it would render as a blank row: fall
    /// back to a Latin label the bitmap font can actually draw. Picking it then
    /// resolves to English (see `set`), which is what it will look like too.
    pub fn picker_name(self) -> &'static str {
        if self.needs_cjk() && !crate::backend::glyphs::cjk_possible() {
            return match self {
                Lang::Zh => "CHINESE (NEEDS MORE MEMORY)",
                _ => "UNAVAILABLE",
            };
        }
        self.native_name()
    }

    pub fn native_name(self) -> &'static str {
        match self {
            Lang::En => "ENGLISH",
            Lang::Fr => "FRAN\u{00C7}AIS",
            Lang::Es => "ESPA\u{00D1}OL",
            Lang::Ru => "\u{0420}\u{0423}\u{0421}\u{0421}\u{041A}\u{0418}\u{0419}", // РУССКИЙ
            Lang::De => "DEUTSCH",
            Lang::It => "ITALIANO",
            Lang::Pt => "PORTUGU\u{00CA}S", // PORTUGUÊS
            // Drawn via the shared-font atlas (CJK is not in the bitmap font).
            Lang::Zh => "\u{4E2D}\u{6587}", // 中文
            Lang::Tr => "T\u{00DC}RK\u{00C7}E", // TÜRKÇE
        }
    }
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
    pub menu_restart: &'static str,
    pub menu_quit: &'static str,
    pub menu_cursor: &'static str,
    pub pause_title: &'static str,
    pub pause_footer: &'static str,
    // TOUCHES (keymap) editor
    pub keys_title: &'static str,
    pub keys_footer: &'static str,
    pub keys_dropdown_footer: &'static str,
    pub none: &'static str,
    /// TOUCHES dropdown labels for the mouse-click pseudo-keys (the stored key
    /// name stays language-stable; only this display label is translated).
    pub flash_mouse_left: &'static str,
    pub flash_mouse_right: &'static str,
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
    pub empty_l2: &'static str,
    pub empty_l3: &'static str,
    pub empty_footer: &'static str,
    // Library list
    pub list_footer: &'static str,
    // Applet-mode notice (P1c) — shown when a game can't launch (small heap)
    pub applet_title: &'static str,
    pub applet_notice: &'static str,
    // OPTIONS modal (per-game)
    pub options_title: &'static str,
    pub opt_keys: &'static str,
    pub opt_rename: &'static str,
    pub opt_edit: &'static str,
    pub opt_delete: &'static str,
    pub opt_back: &'static str,
    pub options_footer: &'static str,
    // Delete confirm
    pub del_title: &'static str,
    pub del_l1: &'static str,
    pub del_l2: &'static str,
    pub del_l3: &'static str,
    pub del_footer: &'static str,
    // DISTANT idle / history
    pub dist_title: &'static str,
    pub dist_add: &'static str,
    pub dist_list_footer: &'static str,
    /// IMPORTER sub-line: saved-URL count ({} = number).
    pub dist_count: &'static str,
    pub dist_subtitle: &'static str,
    pub dist_press_a: &'static str,
    pub dist_example1: &'static str,
    pub dist_example2: &'static str,
    pub dist_history: &'static str,
    pub dist_hint_zr: &'static str,
    pub dist_hint_a: &'static str,
    pub dist_hint_lr: &'static str,
    pub dist_footer_hist: &'static str,
    pub dist_footer_nohist: &'static str,
    // DISTANT files list
    pub files_title: &'static str,
    pub files_filter: &'static str,
    pub files_footer: &'static str,
    // Download
    pub dl_title: &'static str,
    pub dl_footer: &'static str,
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
    pub settings_title: &'static str,
    pub set_keys: &'static str,
    pub set_language: &'static str,
    pub set_quit: &'static str,
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
    /// REGLAGES row opening the game-defaults sub-modal (display / filter /
    /// cursor speed). These are DEFAULTS: a game that has been set explicitly
    /// from its pause menu keeps its own value.
    /// REGLAGES row picking the JOUER tab's layout. Sits at index 0, where the
    /// global keymap row used to be: rows 3 and 5 are hoisted BY INDEX in
    /// `input()`, so a row can only be added at the very start or the very end
    /// without silently breaking them.
    pub set_home_view: &'static str,
    /// Cover grid, the layout since v1.2.0 and still the default.
    pub home_grid: &'static str,
    /// Text list of titles, cover of the selected game beside it (issue #52).
    pub home_list: &'static str,
    /// Covers scrolling sideways, the selected one shown large above.
    pub home_strip: &'static str,
    /// Console shelf: large covers on one line, the active one anchored left and
    /// grown, its details above.
    pub home_shelf: &'static str,
    pub set_game_prefs: &'static str,
    /// Title of that sub-modal.
    pub prefs_title: &'static str,
    /// Screen-filter row in the pause menu, shown as "<label>: <filter>".
    /// The other half of issue #65, which asked for "stretch and filters".
    pub set_screen_filter: &'static str,
    /// No filter (default).
    pub filter_none: &'static str,
    /// Darkens every other scanline.
    pub filter_scanlines: &'static str,
    /// Scanlines + RGB stripe mask + vignette.
    pub filter_crt: &'static str,
    /// TOUCHES row + share/diff label for the per-game "show cursor" toggle.
    pub show_cursor: &'static str,
    /// Values for the show-cursor toggle (shown / hidden).
    pub cursor_shown: &'static str,
    pub cursor_hidden: &'static str,
    /// RÉGLAGES row label for the community-profile nickname (#20).
    pub set_pseudo: &'static str,
    /// Keyboard guide line when typing the nickname.
    pub kbd_pseudo_guide: &'static str,
    pub set_back: &'static str,
    /// RÉGLAGES entry that opens the bug-report flow (pick a game → describe →
    /// send a GitHub issue, no account needed). See `crate::bugreport`.
    pub set_report_bug: &'static str,
    // Navbar tabs (v1.2.0) — switched with L/R.
    pub tab_play: &'static str,
    pub tab_import: &'static str,
    pub tab_settings: &'static str,
    // v1.2.0 covers + online toggle.
    pub set_covers: &'static str,
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
    /// Footer of the profile picker modal.
    pub profile_footer: &'static str,
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
    /// Footer of the TOUCHES sub-menu.
    pub touches_footer: &'static str,
    /// Revert row when there's NO hand-made backup to restore: reverting drops
    /// the applied profile and falls back to the global default controls. Worded
    /// distinctly from `profile_revert` so the user knows it won't restore custom
    /// keys they never made.
    pub touches_revert_default: &'static str,
    // Apply preview (#20): a before/after diff shown before a profile is applied.
    /// Header of the apply-preview screen (mine vs the profile's controls).
    pub profile_preview_title: &'static str,
    /// Footer of the apply-preview screen.
    pub profile_preview_footer: &'static str,
    /// Shown in the preview when the profile changes none of the current keys.
    pub profile_preview_none: &'static str,
    /// Tag appended to the profile row that's currently applied to the game.
    pub profile_active: &'static str,
    /// Toast when sharing would duplicate an already-existing identical profile.
    pub profile_share_dup: &'static str,
    /// Toast when the URL the user just typed names a game already on the card.
    /// `{}` is the file name.
    pub toast_already_imported: &'static str,
    /// Confirm prompt for deleting one of your OWN shared profiles (#20).
    pub profile_del_confirm: &'static str,
    /// Toast after your shared profile is removed from the catalog.
    pub profile_del_ok: &'static str,
    /// Toast when there's nothing to delete (no profile shared from this console).
    pub profile_del_not_mine: &'static str,
    /// Picker footer hint shown only when the highlighted row is your own profile.
    pub profile_del_hint: &'static str,
    /// Header of the revert preview (current keys -> what revert restores).
    pub revert_preview_title: &'static str,
    /// Footer of the revert preview.
    pub revert_preview_footer: &'static str,
    pub cover_title: &'static str,
    pub cover_footer: &'static str,
    /// Cover-picker hint for the Y toggle, worded as the ACTION it performs
    /// (what pressing it gives you), not the state you are in. Screenshots
    /// are the default source since issue #59; logos stay one press away for
    /// the games whose logo reads better.
    pub cover_show_logos: &'static str,
    pub cover_show_shots: &'static str,
    pub cover_off_notice: &'static str,
    pub cover_none: &'static str,
    // Flashpoint game gallery (IMPORTER > X — browse + download a game).
    pub fp_title: &'static str,
    pub fp_footer: &'static str,
    // Flashpoint details popup (`+` on a gallery tile).
    pub fp_details_title: &'static str,
    pub fp_details_dev: &'static str,
    pub fp_details_publisher: &'static str,
    pub fp_details_date: &'static str,
    pub fp_details_size: &'static str,
    pub fp_details_footer: &'static str,
    // JOUER sort picker (Y).
    pub sort_title: &'static str,
    pub sort_footer: &'static str,
    pub sort_alpha: &'static str,
    pub sort_recent: &'static str,
    pub sort_played: &'static str,
    pub sort_size: &'static str,
    pub played_label: &'static str,
    pub sort_recent_played: &'static str,
    pub sort_dev: &'static str,
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
    pub settings_footer: &'static str,
    pub lang_title: &'static str,
    pub lang_footer: &'static str,
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
    pub err_offline: &'static str,
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
    pub bug_pick_title: &'static str,
    pub bug_pick_footer: &'static str,
    pub bug_no_games: &'static str,
    pub bug_ok_title: &'static str,
    pub bug_ok_msg: &'static str,
    pub bug_fail_title: &'static str,
    pub kbd_bug_header: &'static str,
    pub kbd_bug_guide: &'static str,
    // Suggestion / feature request (RÉGLAGES → FAIRE UNE PROPOSITION). Reuses the
    // same relay + token as the bug report; only the issue label differs.
    pub set_suggest: &'static str,
    pub kbd_suggest_header: &'static str,
    pub kbd_suggest_guide: &'static str,
    // First-boot size backfill panel title (v1.5.x): shown while the one-time
    // footprint recompute runs, so that migration isn't a black screen.
    pub optimizing: &'static str,
}

const EN: Strings = Strings {
    optimizing: "OPTIMIZING",
    menu_resume: "RESUME",
    menu_keys: "CONTROLS",
    menu_restart: "RESTART",
    menu_quit: "QUIT",
    menu_cursor: "CURSOR",
    pause_title: "PAUSE",
    pause_footer: "A:OK   B:CANCEL   UP/DOWN:NAV",
    keys_title: "CONTROLS",
    keys_footer: "A:EDIT  L/R:MODE  X:P1/P2  B:BACK",
    keys_dropdown_footer: "A:OK   B:CANCEL   UP/DOWN:NAV",
    none: "(none)",
    flash_mouse_left: "Left click",
    flash_mouse_right: "Right click",
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
    empty_title: "NO GAMES",
    empty_l1: "DROP .SWF FILES INTO",
    empty_l2: "SDMC:/FLASHNX/   OR   SDMC:/SWITCH/FLASHNX/",
    empty_l3: "THEN RESTART FLASHNX.",
    empty_footer: "Y:REMOTE IMPORT   -:QUIT",
    list_footer: "L/R:TABS  A:PLAY  Y:SORT  -:SEARCH  +:OPTIONS  ZL/ZR:PAGE",
    applet_title: "APPLET MODE",
    applet_notice: "LAUNCHING A GAME NEEDS THE FULL APP MEMORY, WHICH APPLET MODE DOES NOT HAVE. IN THE HOMEBREW MENU, HOLD R ON A TITLE (OR USE A FORWARDER) TO START FLASHNX WITH FULL MEMORY.",
    options_title: "OPTIONS",
    opt_keys: "CONTROLS",
    opt_rename: "RENAME",
    opt_edit: "EDIT",
    opt_delete: "DELETE",
    opt_back: "BACK",
    options_footer: "A:OK   B:BACK",
    del_title: "DELETE ?",
    del_l1: "The .swf file, the saves (.sol),",
    del_l2: "the controls and the alias will be erased.",
    del_l3: "This cannot be undone.",
    del_footer: "A: DELETE     B: CANCEL",
    dist_title: "REMOTE IMPORT",
    dist_add: "+ ADD A URL",
    dist_list_footer: "A:LAUNCH   +:OPTIONS   Y:SORT   -:SEARCH   X:FLASHPOINT",
    dist_count: "{} URL(S)",
    dist_subtitle: "DOWNLOAD SWF FROM ARCHIVE.ORG",
    dist_press_a: "PRESS A TO ENTER A URL",
    dist_example1: "EXAMPLE: HTTPS://ARCHIVE.ORG/DETAILS/<ITEM-ID>",
    dist_example2: "OR SIMPLY <ITEM-ID>",
    dist_history: "HISTORY",
    dist_hint_zr: "ZR : LOAD THIS URL DIRECTLY",
    dist_hint_a: "A  : ENTER / EDIT URL (KEYBOARD)",
    dist_hint_lr: "L / R : PREVIOUS / NEXT URL",
    dist_footer_hist: "ZR:OPEN  A:EDIT  ZL:DELETE  L/R:NAV  Y:LOCAL  -:QUIT",
    dist_footer_nohist: "A:ENTER URL   Y:BACK LOCAL   -:QUIT",
    files_title: "REMOTE FILES",
    files_filter: "FILTER",
    files_footer: "A:DOWNLOAD   Y:SORT   -:SEARCH   L/R:PAGE   B:BACK",
    dl_title: "DOWNLOADING",
    dl_footer: "B:CANCEL",
    toast_dl_ok: "{} DOWNLOADED",
    toast_assets_missing: "MISSING GAME DATA: {}. THIS GAME MAY NOT START.",
    err_title: "ERROR",
    err_footer: "A/B:OK",
    err_footer_fix: "A/B:OK   Y:FIX THE URL",
    settings_title: "SETTINGS",
    set_keys: "DEFAULT CONTROLS",
    set_language: "LANGUAGE",
    set_quit: "QUIT",
    set_pseudo: "NICKNAME",
    kbd_pseudo_guide: "Your name, shown next to your shared profiles",
    set_cursor_speed: "CURSOR SPEED",
    set_display_mode: "DISPLAY",
    display_fit: "FIT",
    display_fill: "FILL",
    display_stretch: "STRETCH",
    set_screen_filter: "FILTER",
    set_home_view: "HOME VIEW",
    home_grid: "GRID",
    home_list: "LIST",
    home_strip: "STRIP",
    home_shelf: "SHELF",
    set_game_prefs: "GAME DEFAULTS",
    prefs_title: "DEFAULT SETTINGS",
    filter_none: "NONE",
    filter_scanlines: "SCANLINES",
    filter_crt: "CRT",
    show_cursor: "SHOW CURSOR",
    cursor_shown: "SHOWN",
    cursor_hidden: "HIDDEN",
    set_back: "BACK",
    set_report_bug: "REPORT A BUG",
    tab_play: "PLAY",
    tab_import: "IMPORT",
    tab_settings: "SETTINGS",
    set_covers: "ONLINE COVERS",
    lbl_on: "ON",
    lbl_off: "OFF",
    opt_cover: "COVER",
    opt_favorite: "FAVORITE",
    opt_unfavorite: "REMOVE FAVORITE",
    opt_share: "SHARE CONTROLS",
    profile_shared_ok: "YOUR CONTROLS WERE SENT. THANKS FOR HELPING THE COMMUNITY.",
    opt_apply: "APPLY A PROFILE",
    profile_title: "CONTROL PROFILES",
    profile_footer: "A:APPLY   B:BACK   UP/DOWN:NAV",
    profile_none: "NO PROFILE FOR THIS GAME YET. SHARE YOURS TO HELP!",
    profile_catalog_offline: "CATALOG UNAVAILABLE. CHECK YOUR CONNECTION.",
    profile_revert: "REVERT TO MY CONTROLS",
    profile_applied_ok: "PROFILE APPLIED. YOUR PREVIOUS CONTROLS WERE SAVED.",
    profile_reverted_ok: "YOUR CONTROLS WERE RESTORED.",
    profile_share_confirm: "SHARE YOUR CONTROLS FOR THIS GAME?",
    share_confirm_update: "UPDATES YOUR SHARED PROFILE:",
    touches_edit: "EDIT CONTROLS",
    touches_footer: "A:SELECT   B:BACK   UP/DOWN:NAV",
    touches_revert_default: "RESET TO DEFAULT CONTROLS",
    profile_preview_title: "MINE -> PROFILE",
    profile_preview_footer: "A:APPLY   B:BACK",
    profile_preview_none: "THIS PROFILE CHANGES NONE OF YOUR KEYS.",
    profile_active: "(ACTIVE)",
    profile_share_dup: "ALREADY IN THE CATALOG. EDIT A KEY TO SHARE YOUR OWN VERSION.",
    toast_already_imported: "{} IS ALREADY IN YOUR LIBRARY.",
    profile_del_confirm: "DELETE YOUR SHARED PROFILE?",
    profile_del_ok: "YOUR SHARED PROFILE WAS DELETED.",
    profile_del_not_mine: "NOTHING TO DELETE: NOT SHARED FROM THIS CONSOLE.",
    profile_del_hint: "X:DELETE",
    revert_preview_title: "NOW -> AFTER REVERT",
    revert_preview_footer: "A:REVERT   B:BACK",
    cover_title: "CHOOSE A COVER",
    cover_footer: "A: CHOOSE   -: SEARCH   UP/DOWN: MOVE   B: BACK",
    cover_show_logos: "Y: LOGOS",
    cover_show_shots: "Y: SCREENSHOTS",
    cover_off_notice: "ENABLE ONLINE COVERS IN SETTINGS",
    cover_none: "NO RESULTS",
    fp_title: "FLASHPOINT",
    fp_footer: "A:DOWNLOAD   X:SEARCH   Y:SORT   +:INFO   ZL+ZR:FILTER {}   B:BACK",
    fp_details_title: "DETAILS",
    fp_details_dev: "DEVELOPER",
    fp_details_publisher: "PUBLISHER",
    fp_details_date: "RELEASED",
    fp_details_size: "DOWNLOAD SIZE",
    fp_details_footer: "B:BACK",
    sort_title: "SORT BY",
    sort_footer: "A:CHOOSE   X:REVERSE   B:BACK",
    sort_alpha: "NAME",
    sort_recent: "ADDED",
    sort_played: "MOST PLAYED",
    sort_size: "SIZE",
    played_label: "PLAYED",
    sort_recent_played: "LAST PLAYED",
    sort_dev: "DEVELOPER",
    sort_source: "SOURCE",
    sort_files: "FILE COUNT",
    fav_added: "ADDED TO FAVORITES",
    fav_removed: "REMOVED FROM FAVORITES",
    multifile: "MULTI-FILE",
    sort_dir_asc: "ASCENDING",
    sort_dir_desc: "DESCENDING",
    settings_footer: "L/R:TABS   A:OK",
    lang_title: "LANGUAGE",
    lang_footer: "A:OK   B:CANCEL",
    histdel_title: "DELETE URL ?",
    url_info_type: "TYPE",
    url_type_swf: "SINGLE .SWF",
    url_type_list: "FILE LIST",
    url_info_files: "ON SD",
    url_info_added: "ADDED",
    histdel_msg: "Remove this URL from the history?",
    err_too_large: "Archive.org response too large (>4 MB). Item too big for this build.",
    err_https: "Transfer failed ({}). Check the WiFi and the URL.",
    err_offline: "No connection. Check the console's WiFi.",
    err_timeout: "The server did not answer in time. Try again.",
    err_tls: "Secure connection refused. Check the console's date and time: a wrong clock breaks HTTPS.",
    err_response_big: "Response too large for this build. Try a narrower search.",
    err_sd_write: "Could not write to the SD card. It may be full or write-protected.",
    err_http_status: "The server refused the request (HTTP {}). Try again later.",
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
    bug_pick_title: "REPORT A BUG",
    bug_pick_footer: "A:CHOOSE   B:BACK   UP/DOWN:NAV",
    bug_no_games: "NO GAME TO REPORT YET. IMPORT OR DROP A .SWF FIRST.",
    bug_ok_title: "THANKS!",
    bug_ok_msg: "YOUR REPORT WAS SENT. THANKS FOR HELPING IMPROVE FLASHNX.",
    bug_fail_title: "FAILED",
    kbd_bug_header: "FlashNX - Report a bug",
    kbd_bug_guide: "Opens a PUBLIC GitHub issue. Describe the problem. Optional: your @handle.",
    set_suggest: "MAKE A SUGGESTION",
    kbd_suggest_header: "FlashNX - Make a suggestion",
    kbd_suggest_guide: "Opens a PUBLIC GitHub issue. Your idea / feature request for FlashNX.",
};

const FR: Strings = Strings {
    optimizing: "OPTIMISATION",
    menu_resume: "REPRENDRE",
    menu_keys: "TOUCHES",
    menu_restart: "RED\u{00C9}MARRER",
    menu_quit: "QUITTER",
    menu_cursor: "CURSEUR",
    pause_title: "PAUSE",
    pause_footer: "A:OK   B:ANNULER   HAUT/BAS:NAV",
    keys_title: "TOUCHES",
    keys_footer: "A:\u{00C9}DITER  L/R:MODE  X:J1/J2  B:RETOUR",
    keys_dropdown_footer: "A:OK   B:ANNULER   HAUT/BAS:NAV",
    none: "(aucune)",
    flash_mouse_left: "Clic gauche",
    flash_mouse_right: "Clic droit",
    flash_space: "Espace",
    flash_enter: "Entr\u{00C9}e",
    flash_escape: "\u{00C9}chap",
    flash_shift: "Maj",
    flash_control: "Ctrl",
    flash_alt: "Alt",
    flash_tab: "Tab",
    flash_backspace: "Retour arr.",
    flash_up: "Haut",
    flash_down: "Bas",
    flash_left: "Gauche",
    flash_right: "Droite",
    empty_title: "AUCUN JEU",
    empty_l1: "D\u{00C9}POSEZ DES FICHIERS .SWF DANS",
    empty_l2: "SDMC:/FLASHNX/   OU   SDMC:/SWITCH/FLASHNX/",
    empty_l3: "PUIS RED\u{00C9}MARREZ FLASHNX.",
    empty_footer: "Y:IMPORT DISTANT   -:QUITTER",
    list_footer: "L/R:ONGLETS  A:JOUER  Y:TRIER  -:RECHERCHE  +:OPTIONS  ZL/ZR:PAGE",
    applet_title: "MODE APPLET",
    applet_notice: "LANCER UN JEU DEMANDE TOUTE LA M\u{00C9}MOIRE DE L'APP, INDISPONIBLE EN MODE APPLET. DANS LE HOMEBREW MENU, TIENS R SUR UN TITRE (OU UN FORWARDER) POUR D\u{00C9}MARRER FLASHNX AVEC TOUTE LA M\u{00C9}MOIRE.",
    options_title: "OPTIONS",
    opt_keys: "TOUCHES",
    opt_rename: "RENOMMER",
    opt_edit: "\u{00C9}DITER",
    opt_delete: "SUPPRIMER",
    opt_back: "RETOUR",
    options_footer: "A:OK   B:RETOUR",
    del_title: "SUPPRIMER ?",
    del_l1: "LE FICHIER .swf, LES SAUVEGARDES (.sol),",
    del_l2: "LES TOUCHES ET L'ALIAS SERONT EFFAC\u{00C9}S.",
    del_l3: "ACTION IRR\u{00C9}VERSIBLE.",
    del_footer: "A: SUPPRIMER     B: ANNULER",
    dist_title: "IMPORT DISTANT",
    dist_add: "+ AJOUTER UNE URL",
    dist_list_footer: "A:LANCER   +:OPTIONS   Y:TRIER   -:RECHERCHE   X:FLASHPOINT",
    dist_count: "{} URL",
    dist_subtitle: "T\u{00C9}L\u{00C9}CHARGEMENT DE SWF DEPUIS ARCHIVE.ORG",
    dist_press_a: "APPUYEZ SUR A POUR SAISIR UNE URL",
    dist_example1: "EXEMPLE: HTTPS://ARCHIVE.ORG/DETAILS/<ITEM-ID>",
    dist_example2: "OU SIMPLEMENT <ITEM-ID>",
    dist_history: "HISTORIQUE",
    dist_hint_zr: "ZR : CHARGER CETTE URL DIRECTEMENT",
    dist_hint_a: "A  : SAISIR / \u{00C9}DITER URL (CLAVIER)",
    dist_hint_lr: "L / R : URL PR\u{00C9}C\u{00C9}DENTE / SUIVANTE",
    dist_footer_hist: "ZR:OUVRIR  A:\u{00C9}DITER  ZL:SUPPR  L/R:NAV  Y:LOCAL  -:QUITTER",
    dist_footer_nohist: "A:SAISIR URL   Y:RETOUR LOCAL   -:QUITTER",
    files_title: "FICHIERS DISTANTS",
    files_filter: "FILTRE",
    files_footer: "A:T\u{00C9}L\u{00C9}CHARGER   Y:TRIER   -:RECHERCHE   L/R:PAGE   B:RETOUR",
    dl_title: "T\u{00C9}L\u{00C9}CHARGEMENT",
    dl_footer: "B:ANNULER",
    toast_dl_ok: "{} T\u{00C9}L\u{00C9}CHARG\u{00C9}",
    toast_assets_missing: "DONN\u{00C9}ES DE JEU MANQUANTES : {}. CE JEU RISQUE DE NE PAS D\u{00C9}MARRER.",
    err_title: "ERREUR",
    err_footer: "A/B:OK",
    err_footer_fix: "A/B:OK   Y:CORRIGER L'URL",
    settings_title: "R\u{00C9}GLAGES",
    set_keys: "TOUCHES PAR D\u{00C9}FAUT",
    set_language: "LANGUE",
    set_quit: "QUITTER",
    set_pseudo: "PSEUDO",
    kbd_pseudo_guide: "Ton nom, affich\u{00E9} \u{00E0} c\u{00F4}t\u{00E9} de tes profils partag\u{00E9}s",
    set_cursor_speed: "VITESSE DU CURSEUR",
    set_display_mode: "AFFICHAGE",
    display_fit: "INT\u{00C9}GRAL",
    display_fill: "REMPLIR",
    display_stretch: "\u{00C9}TIRER",
    set_screen_filter: "FILTRE",
    set_home_view: "VUE ACCUEIL",
    home_grid: "GRILLE",
    home_list: "LISTE",
    home_strip: "BANDE",
    home_shelf: "ETAGERE",
    set_game_prefs: "PR\u{00C9}F\u{00C9}RENCES DE JEU",
    prefs_title: "R\u{00C9}GLAGES PAR D\u{00C9}FAUT",
    filter_none: "AUCUN",
    filter_scanlines: "LIGNES",
    filter_crt: "CRT",
    show_cursor: "AFFICHER LE CURSEUR",
    cursor_shown: "AFFICHÉ",
    cursor_hidden: "MASQUÉ",
    set_back: "RETOUR",
    set_report_bug: "SIGNALER UN BUG",
    tab_play: "JOUER",
    tab_import: "IMPORTER",
    tab_settings: "R\u{00C9}GLAGES",
    set_covers: "JAQUETTES EN LIGNE",
    lbl_on: "ON",
    lbl_off: "OFF",
    opt_cover: "JAQUETTE",
    opt_favorite: "FAVORI",
    opt_unfavorite: "RETIRER DES FAVORIS",
    opt_share: "PARTAGER LES TOUCHES",
    profile_shared_ok: "TES TOUCHES ONT \u{00C9}T\u{00C9} ENVOY\u{00C9}ES. MERCI POUR LA COMMUNAUT\u{00C9}.",
    opt_apply: "APPLIQUER UN PROFIL",
    profile_title: "PROFILS DE TOUCHES",
    profile_footer: "A:APPLIQUER   B:RETOUR   HAUT/BAS:NAV",
    profile_none: "AUCUN PROFIL POUR CE JEU. PARTAGE LE TIEN POUR AIDER !",
    profile_catalog_offline: "CATALOGUE INDISPONIBLE. VERIFIE TA CONNEXION.",
    profile_revert: "REVENIR \u{00C0} MES TOUCHES",
    profile_applied_ok: "PROFIL APPLIQU\u{00C9}. TES TOUCHES PR\u{00C9}C\u{00C9}DENTES SONT SAUVEGARD\u{00C9}ES.",
    profile_reverted_ok: "TES TOUCHES ONT \u{00C9}T\u{00C9} RESTAUR\u{00C9}ES.",
    profile_share_confirm: "PARTAGER TES TOUCHES POUR CE JEU ?",
    share_confirm_update: "MET \u{00C0} JOUR TON PROFIL PARTAG\u{00C9} :",
    touches_edit: "MODIFIER LES TOUCHES",
    touches_footer: "A:CHOISIR   B:RETOUR   HAUT/BAS:NAV",
    touches_revert_default: "REVENIR AUX TOUCHES PAR D\u{00C9}FAUT",
    profile_preview_title: "MOI -> PROFIL",
    profile_preview_footer: "A:APPLIQUER   B:RETOUR",
    profile_preview_none: "CE PROFIL NE CHANGE AUCUNE TOUCHE.",
    profile_active: "(ACTIF)",
    profile_share_dup: "D\u{00C9}J\u{00C0} DANS LE CATALOGUE. MODIFIE UNE TOUCHE POUR PARTAGER TA VERSION.",
    toast_already_imported: "{} EST D\u{00C9}J\u{00C0} DANS TA LUDOTH\u{00C8}QUE.",
    profile_del_confirm: "SUPPRIMER TON PROFIL PARTAG\u{00C9} ?",
    profile_del_ok: "TON PROFIL PARTAG\u{00C9} A \u{00C9}T\u{00C9} SUPPRIM\u{00C9}.",
    profile_del_not_mine: "RIEN \u{00C0} SUPPRIMER : PAS PARTAG\u{00C9} DEPUIS CETTE CONSOLE.",
    profile_del_hint: "X:SUPPRIMER",
    revert_preview_title: "ACTUEL -> APR\u{00C8}S RETOUR",
    revert_preview_footer: "A:REVENIR   B:RETOUR",
    cover_title: "CHOISIR UNE JAQUETTE",
    cover_footer: "A: CHOISIR   -: RECHERCHER   HAUT/BAS: NAVIGUER   B: RETOUR",
    cover_show_logos: "Y: LOGOS",
    cover_show_shots: "Y: CAPTURES",
    cover_off_notice: "ACTIVE LES JAQUETTES EN LIGNE DANS LES R\u{00C9}GLAGES",
    cover_none: "AUCUN R\u{00C9}SULTAT",
    fp_title: "FLASHPOINT",
    fp_footer: "A:T\u{00C9}L\u{00C9}CHARGER   X:RECHERCHE   Y:TRIER   +:INFOS   ZL+ZR:FILTRE {}   B:RETOUR",
    fp_details_title: "D\u{00C9}TAILS",
    fp_details_dev: "D\u{00C9}VELOPPEUR",
    fp_details_publisher: "\u{00C9}DITEUR",
    fp_details_date: "SORTIE",
    fp_details_size: "TAILLE DU T\u{00C9}L\u{00C9}CHARGEMENT",
    fp_details_footer: "B:RETOUR",
    sort_title: "TRIER PAR",
    sort_footer: "A:CHOISIR   X:INVERSER   B:RETOUR",
    sort_alpha: "NOM",
    sort_recent: "AJOUT\u{00C9}",
    sort_played: "PLUS JOU\u{00C9}S",
    sort_size: "TAILLE",
    played_label: "JOU\u{00C9}",
    sort_recent_played: "DERNIER JOU\u{00C9}",
    sort_dev: "D\u{00C9}VELOPPEUR",
    sort_source: "SOURCE",
    sort_files: "NOMBRE DE FICHIERS",
    fav_added: "AJOUT\u{00C9} AUX FAVORIS",
    fav_removed: "RETIR\u{00C9} DES FAVORIS",
    multifile: "MULTI-FICHIERS",
    sort_dir_asc: "CROISSANT",
    sort_dir_desc: "D\u{00C9}CROISSANT",
    settings_footer: "L/R:ONGLETS   A:OK",
    lang_title: "LANGUE",
    lang_footer: "A:OK   B:ANNULER",
    histdel_title: "SUPPRIMER URL ?",
    url_info_type: "TYPE",
    url_type_swf: "FICHIER .SWF",
    url_type_list: "LISTE DE FICHIERS",
    url_info_files: "SUR LA SD",
    url_info_added: "AJOUT\u{00C9}E LE",
    histdel_msg: "RETIRER CETTE URL DE L'HISTORIQUE ?",
    err_too_large: "R\u{00C9}PONSE ARCHIVE.ORG TROP VOLUMINEUSE (>4 MB). ITEM TROP MASSIF POUR CETTE VERSION.",
    err_https: "\u{00C9}CHEC DU TRANSFERT ({}). V\u{00C9}RIFIEZ LE WIFI ET L'URL.",
    err_offline: "PAS DE CONNEXION. V\u{00C9}RIFIEZ LE WIFI DE LA CONSOLE.",
    err_timeout: "LE SERVEUR N'A PAS R\u{00C9}PONDU \u{00C0} TEMPS. R\u{00C9}ESSAYEZ.",
    err_tls: "CONNEXION S\u{00C9}CURIS\u{00C9}E REFUS\u{00C9}E. V\u{00C9}RIFIEZ LA DATE ET L'HEURE DE LA CONSOLE : UNE HORLOGE FAUSSE CASSE LE HTTPS.",
    err_response_big: "R\u{00C9}PONSE TROP VOLUMINEUSE. ESSAYEZ UNE RECHERCHE PLUS PR\u{00C9}CISE.",
    err_sd_write: "\u{00C9}CRITURE IMPOSSIBLE SUR LA CARTE SD. ELLE EST PEUT-\u{00CA}TRE PLEINE.",
    err_http_status: "LE SERVEUR A REFUS\u{00C9} LA REQU\u{00CA}TE (HTTP {}). R\u{00C9}ESSAYEZ PLUS TARD.",
    err_not_found: "INTROUVABLE (404). L'URL OU L'IDENTIFIANT DE L'ITEM EST FAUX, OU IL A \u{00C9}T\u{00C9} SUPPRIM\u{00C9}.",
    err_json: "JSON ARCHIVE.ORG ILLISIBLE : {}",
    err_json_no_files: "JSON SANS CHAMP \"files\"",
    err_dl_start: "IMPOSSIBLE DE LANCER LE T\u{00C9}L\u{00C9}CHARGEMENT (CODE {}).",
    err_dl_failed: "T\u{00C9}L\u{00C9}CHARGEMENT \u{00C9}CHOU\u{00C9} (CODE {})",
    err_dl_cancelled: "T\u{00C9}L\u{00C9}CHARGEMENT ANNUL\u{00C9} PAR L'UTILISATEUR.",
    err_url_invalid: "URL INVALIDE. ATTENDU UNE URL ARCHIVE.ORG TYPE https://archive.org/details/<id> OU SIMPLEMENT <id>.",
    err_no_swf: "AUCUN FICHIER .SWF TROUV\u{00C9} POUR CE JEU.",
    err_dl_not_a_game: "LE T\u{00C9}L\u{00C9}CHARGEMENT N'A PAS RENVOY\u{00C9} UN FICHIER DE JEU. V\u{00C9}RIFIEZ L'ADRESSE.",
    err_fp_html_game: "CE JEU SE LANCE VIA UNE PAGE HTML (FLASHVARS), PAS ENCORE SUPPORT\u{00C9}.",
    kbd_url_header: "FlashNX - Import distant",
    kbd_url_guide: "URL archive.org de l'item (ex: https://archive.org/download/your-game-id)",
    kbd_rename_header: "FlashNX - Renommer le jeu",
    kbd_rename_guide: "Nom d'affichage (laisser vide pour revenir au nom de fichier)",
    kbd_search_header: "FlashNX - Rechercher",
    kbd_search_guide: "Filtre nom de fichier (vide = tout afficher)",
    bug_pick_title: "SIGNALER UN BUG",
    bug_pick_footer: "A:CHOISIR   B:RETOUR   HAUT/BAS:NAV",
    bug_no_games: "AUCUN JEU \u{00C0} SIGNALER. IMPORTE OU D\u{00C9}POSE UN .SWF D'ABORD.",
    bug_ok_title: "MERCI !",
    bug_ok_msg: "TON RAPPORT A \u{00C9}T\u{00C9} ENVOY\u{00C9}. MERCI DE M'AIDER \u{00C0} AM\u{00C9}LIORER FLASHNX.",
    bug_fail_title: "\u{00C9}CHEC",
    kbd_bug_header: "FlashNX - Signaler un bug",
    kbd_bug_guide: "Ouvre une issue GitHub PUBLIQUE. Décris le problème. Option : ton @pseudo.",
    set_suggest: "FAIRE UNE PROPOSITION",
    kbd_suggest_header: "FlashNX - Faire une proposition",
    kbd_suggest_guide: "Ouvre une issue GitHub PUBLIQUE. Ton idée / proposition pour FlashNX.",
};

const ES: Strings = Strings {
    optimizing: "OPTIMIZANDO",
    menu_resume: "REANUDAR",
    menu_keys: "CONTROLES",
    menu_restart: "REINICIAR",
    menu_quit: "SALIR",
    menu_cursor: "CURSOR",
    pause_title: "PAUSA",
    pause_footer: "A:OK   B:CANCELAR   ARRIBA/ABAJO:NAV",
    keys_title: "CONTROLES",
    keys_footer: "A:EDITAR  L/R:MODO  X:J1/J2  B:VOLVER",
    keys_dropdown_footer: "A:OK   B:CANCELAR   ARRIBA/ABAJO:NAV",
    none: "(ninguna)",
    flash_mouse_left: "Clic izquierdo",
    flash_mouse_right: "Clic derecho",
    flash_space: "Espacio",
    flash_enter: "Intro",
    flash_escape: "Escape",
    flash_shift: "May\u{00DA}s",
    flash_control: "Ctrl",
    flash_alt: "Alt",
    flash_tab: "Tab",
    flash_backspace: "Retroceso",
    flash_up: "Arriba",
    flash_down: "Abajo",
    flash_left: "Izquierda",
    flash_right: "Derecha",
    empty_title: "SIN JUEGOS",
    empty_l1: "COLOCA ARCHIVOS .SWF EN",
    empty_l2: "SDMC:/FLASHNX/   O   SDMC:/SWITCH/FLASHNX/",
    empty_l3: "Y REINICIA FLASHNX.",
    empty_footer: "Y:IMPORTAR REMOTO   -:SALIR",
    list_footer: "L/R:PESTA\u{00D1}AS  A:JUGAR  Y:ORDENAR  -:BUSCAR  +:OPCIONES  ZL/ZR:P\u{00C1}GINA",
    applet_title: "MODO APPLET",
    applet_notice: "EJECUTAR UN JUEGO NECESITA TODA LA MEMORIA DE LA APP, QUE EL MODO APPLET NO TIENE. EN EL HOMEBREW MENU, MANT\u{00C9}N R SOBRE UN T\u{00CD}TULO (O USA UN FORWARDER) PARA INICIAR FLASHNX CON TODA LA MEMORIA.",
    options_title: "OPCIONES",
    opt_keys: "CONTROLES",
    opt_rename: "RENOMBRAR",
    opt_edit: "EDITAR",
    opt_delete: "BORRAR",
    opt_back: "VOLVER",
    options_footer: "A:OK   B:VOLVER",
    del_title: "\u{00BF}BORRAR ?",
    del_l1: "EL ARCHIVO .swf, LAS PARTIDAS (.sol),",
    del_l2: "LOS CONTROLES Y EL ALIAS SE BORRAR\u{00C1}N.",
    del_l3: "ACCI\u{00D3}N IRREVERSIBLE.",
    del_footer: "A: BORRAR     B: CANCELAR",
    dist_title: "IMPORTAR REMOTO",
    dist_add: "+ A\u{00D1}ADIR UNA URL",
    dist_list_footer: "A:LANZAR   +:OPCIONES   Y:ORDENAR   -:BUSCAR   X:FLASHPOINT",
    dist_count: "{} URL",
    dist_subtitle: "DESCARGAR SWF DESDE ARCHIVE.ORG",
    dist_press_a: "PULSA A PARA INTRODUCIR UNA URL",
    dist_example1: "EJEMPLO: HTTPS://ARCHIVE.ORG/DETAILS/<ITEM-ID>",
    dist_example2: "O SIMPLEMENTE <ITEM-ID>",
    dist_history: "HISTORIAL",
    dist_hint_zr: "ZR : CARGAR ESTA URL DIRECTAMENTE",
    dist_hint_a: "A  : INTRODUCIR / EDITAR URL (TECLADO)",
    dist_hint_lr: "L / R : URL ANTERIOR / SIGUIENTE",
    dist_footer_hist: "ZR:ABRIR  A:EDITAR  ZL:BORRAR  L/R:NAV  Y:LOCAL  -:SALIR",
    dist_footer_nohist: "A:INTRODUCIR URL   Y:VOLVER LOCAL   -:SALIR",
    files_title: "ARCHIVOS REMOTOS",
    files_filter: "FILTRO",
    files_footer: "A:DESCARGAR   Y:ORDENAR   -:BUSCAR   L/R:P\u{00C1}GINA   B:VOLVER",
    dl_title: "DESCARGANDO",
    dl_footer: "B:CANCELAR",
    toast_dl_ok: "{} DESCARGADO",
    toast_assets_missing: "FALTAN DATOS DEL JUEGO: {}. PUEDE QUE NO ARRANQUE.",
    err_title: "ERROR",
    err_footer: "A/B:OK",
    err_footer_fix: "A/B:OK   Y:CORREGIR LA URL",
    settings_title: "AJUSTES",
    set_keys: "CONTROLES POR DEFECTO",
    set_language: "IDIOMA",
    set_quit: "SALIR",
    set_pseudo: "APODO",
    kbd_pseudo_guide: "Tu nombre, junto a tus perfiles compartidos",
    set_cursor_speed: "VELOCIDAD DEL CURSOR",
    set_display_mode: "PANTALLA",
    display_fit: "AJUSTAR",
    display_fill: "RELLENAR",
    display_stretch: "ESTIRAR",
    set_screen_filter: "FILTRO",
    set_home_view: "VISTA INICIO",
    home_grid: "CUADRÍCULA",
    home_list: "LISTA",
    home_strip: "TIRA",
    home_shelf: "ESTANTE",
    set_game_prefs: "PREFERENCIAS",
    prefs_title: "AJUSTES POR DEFECTO",
    filter_none: "NINGUNO",
    filter_scanlines: "L\u{00CD}NEAS",
    filter_crt: "CRT",
    show_cursor: "MOSTRAR CURSOR",
    cursor_shown: "VISIBLE",
    cursor_hidden: "OCULTO",
    set_back: "VOLVER",
    set_report_bug: "REPORTAR UN FALLO",
    tab_play: "JUGAR",
    tab_import: "IMPORTAR",
    tab_settings: "AJUSTES",
    set_covers: "CARATULAS EN LINEA",
    lbl_on: "ON",
    lbl_off: "OFF",
    opt_cover: "CARATULA",
    opt_favorite: "FAVORITO",
    opt_unfavorite: "QUITAR FAVORITO",
    opt_share: "COMPARTIR CONTROLES",
    profile_shared_ok: "TUS CONTROLES SE ENVIARON. GRACIAS POR AYUDAR A LA COMUNIDAD.",
    opt_apply: "APLICAR UN PERFIL",
    profile_title: "PERFILES DE CONTROL",
    profile_footer: "A:APLICAR   B:VOLVER   ARRIBA/ABAJO:NAV",
    profile_none: "A\u{00DA}N NO HAY PERFIL PARA ESTE JUEGO. \u{00A1}COMPARTE EL TUYO!",
    profile_catalog_offline: "CAT\u{00C1}LOGO NO DISPONIBLE. COMPRUEBA TU CONEXI\u{00D3}N.",
    profile_revert: "VOLVER A MIS CONTROLES",
    profile_applied_ok: "PERFIL APLICADO. TUS CONTROLES ANTERIORES SE GUARDARON.",
    profile_reverted_ok: "TUS CONTROLES SE RESTAURARON.",
    profile_share_confirm: "\u{00BF}COMPARTIR TUS CONTROLES PARA ESTE JUEGO?",
    share_confirm_update: "ACTUALIZA TU PERFIL COMPARTIDO:",
    touches_edit: "EDITAR CONTROLES",
    touches_footer: "A:ELEGIR   B:VOLVER   ARRIBA/ABAJO:NAV",
    touches_revert_default: "RESTABLECER CONTROLES POR DEFECTO",
    profile_preview_title: "MIOS -> PERFIL",
    profile_preview_footer: "A:APLICAR   B:VOLVER",
    profile_preview_none: "ESTE PERFIL NO CAMBIA NINGUNA TECLA.",
    profile_active: "(ACTIVO)",
    profile_share_dup: "YA EST\u{00C1} EN EL CAT\u{00C1}LOGO. CAMBIA UNA TECLA PARA COMPARTIR TU VERSI\u{00D3}N.",
    toast_already_imported: "{} YA EST\u{00C1} EN TU BIBLIOTECA.",
    profile_del_confirm: "\u{00BF}BORRAR TU PERFIL COMPARTIDO?",
    profile_del_ok: "TU PERFIL COMPARTIDO FUE BORRADO.",
    profile_del_not_mine: "NADA QUE BORRAR DESDE ESTA CONSOLA.",
    profile_del_hint: "X:BORRAR",
    revert_preview_title: "ACTUAL -> TRAS REVERTIR",
    revert_preview_footer: "A:REVERTIR   B:VOLVER",
    cover_title: "ELEGIR CARATULA",
    cover_footer: "A: ELEGIR   -: BUSCAR   ARRIBA/ABAJO: MOVER   B: VOLVER",
    cover_show_logos: "Y: LOGOS",
    cover_show_shots: "Y: CAPTURAS",
    cover_off_notice: "ACTIVA CARATULAS EN LINEA EN AJUSTES",
    cover_none: "SIN RESULTADOS",
    fp_title: "FLASHPOINT",
    fp_footer: "A:DESCARGAR   X:BUSCAR   Y:ORDENAR   +:INFO   ZL+ZR:FILTRO {}   B:VOLVER",
    fp_details_title: "DETALLES",
    fp_details_dev: "DESARROLLADOR",
    fp_details_publisher: "EDITOR",
    fp_details_date: "LANZAMIENTO",
    fp_details_size: "TAMA\u{00D1}O DE DESCARGA",
    fp_details_footer: "B:VOLVER",
    sort_title: "ORDENAR POR",
    sort_footer: "A:ELEGIR   X:INVERTIR   B:VOLVER",
    sort_alpha: "NOMBRE",
    sort_recent: "A\u{00D1}ADIDO",
    sort_played: "M\u{00C1}S JUGADOS",
    sort_size: "TAMA\u{00D1}O",
    played_label: "JUGADO",
    sort_recent_played: "\u{00DA}LTIMO JUGADO",
    sort_dev: "DESARROLLADOR",
    sort_source: "FUENTE",
    sort_files: "N\u{00DA}MERO DE ARCHIVOS",
    fav_added: "A\u{00D1}ADIDO A FAVORITOS",
    fav_removed: "QUITADO DE FAVORITOS",
    multifile: "MULTIARCHIVO",
    sort_dir_asc: "ASCENDENTE",
    sort_dir_desc: "DESCENDENTE",
    settings_footer: "L/R:PESTA\u{00D1}AS   A:OK",
    lang_title: "IDIOMA",
    lang_footer: "A:OK   B:CANCELAR",
    histdel_title: "\u{00BF}BORRAR URL ?",
    url_info_type: "TIPO",
    url_type_swf: "ARCHIVO .SWF",
    url_type_list: "LISTA DE ARCHIVOS",
    url_info_files: "EN LA SD",
    url_info_added: "A\u{00D1}ADIDA EL",
    histdel_msg: "\u{00BF}QUITAR ESTA URL DEL HISTORIAL?",
    err_too_large: "RESPUESTA DE ARCHIVE.ORG DEMASIADO GRANDE (>4 MB). ITEM DEMASIADO GRANDE PARA ESTA VERSI\u{00D3}N.",
    err_https: "FALLO DE TRANSFERENCIA ({}). COMPRUEBA EL WIFI Y LA URL.",
    err_offline: "SIN CONEXI\u{00D3}N. COMPRUEBA EL WIFI DE LA CONSOLA.",
    err_timeout: "EL SERVIDOR NO RESPONDI\u{00D3} A TIEMPO. INT\u{00C9}NTALO DE NUEVO.",
    err_tls: "CONEXI\u{00D3}N SEGURA RECHAZADA. COMPRUEBA LA FECHA Y LA HORA DE LA CONSOLA: UN RELOJ MAL AJUSTADO ROMPE HTTPS.",
    err_response_big: "RESPUESTA DEMASIADO GRANDE. PRUEBA UNA B\u{00DA}SQUEDA M\u{00C1}S PRECISA.",
    err_sd_write: "NO SE PUDO ESCRIBIR EN LA TARJETA SD. QUIZ\u{00C1} EST\u{00C9} LLENA.",
    err_http_status: "EL SERVIDOR RECHAZ\u{00D3} LA PETICI\u{00D3}N (HTTP {}). INT\u{00C9}NTALO M\u{00C1}S TARDE.",
    err_not_found: "NO ENCONTRADO (404). LA URL O EL ID DEL ITEM ES INCORRECTO, O FUE ELIMINADO.",
    err_json: "JSON DE ARCHIVE.ORG ILEGIBLE: {}",
    err_json_no_files: "JSON SIN CAMPO \"files\"",
    err_dl_start: "NO SE PUDO INICIAR LA DESCARGA (C\u{00D3}DIGO {}).",
    err_dl_failed: "DESCARGA FALLIDA (C\u{00D3}DIGO {})",
    err_dl_cancelled: "DESCARGA CANCELADA POR EL USUARIO.",
    err_url_invalid: "URL INV\u{00C1}LIDA. SE ESPERABA UNA URL DE ARCHIVE.ORG TIPO https://archive.org/details/<id> O SIMPLEMENTE <id>.",
    err_no_swf: "NO SE ENCONTR\u{00D3} NING\u{00DA}N ARCHIVO .SWF PARA ESTE JUEGO.",
    err_dl_not_a_game: "LA DESCARGA NO DEVOLVI\u{00D3} UN ARCHIVO DE JUEGO. COMPRUEBA LA DIRECCI\u{00D3}N.",
    err_fp_html_game: "ESTE JUEGO SE INICIA DESDE UNA P\u{00C1}GINA HTML (FLASHVARS); A\u{00DA}N NO COMPATIBLE.",
    kbd_url_header: "FlashNX - Importar remoto",
    kbd_url_guide: "URL del item de archive.org (ej: https://archive.org/download/your-game-id)",
    kbd_rename_header: "FlashNX - Renombrar juego",
    kbd_rename_guide: "Nombre a mostrar (dejar vacio para volver al nombre de archivo)",
    kbd_search_header: "FlashNX - Buscar",
    kbd_search_guide: "Filtrar por nombre de archivo (vacio = mostrar todo)",
    bug_pick_title: "REPORTAR UN FALLO",
    bug_pick_footer: "A:ELEGIR   B:VOLVER   ARRIBA/ABAJO:NAV",
    bug_no_games: "NO HAY JUEGO QUE REPORTAR. IMPORTA O COLOCA UN .SWF PRIMERO.",
    bug_ok_title: "\u{00A1}GRACIAS!",
    bug_ok_msg: "TU INFORME SE ENVI\u{00D3}. GRACIAS POR AYUDAR A MEJORAR FLASHNX.",
    bug_fail_title: "FALLO",
    kbd_bug_header: "FlashNX - Reportar un fallo",
    kbd_bug_guide: "Abre un issue PUBLICO en GitHub. Describe el problema. Opcional: tu @usuario.",
    set_suggest: "HACER UNA SUGERENCIA",
    kbd_suggest_header: "FlashNX - Hacer una sugerencia",
    kbd_suggest_guide: "Abre un issue PUBLICO en GitHub. Tu idea / sugerencia para FlashNX.",
};

// Russian — UPPERCASE Cyrillic (draw_text does not case-fold non-ASCII, and
// only uppercase Cyrillic glyphs were added to the font).
const RU: Strings = Strings {
    optimizing: "OPTIMIZATSIYA",
    menu_resume: "ПРОДОЛЖИТЬ",
    menu_keys: "КЛАВИШИ",
    menu_restart: "ЗАНОВО",
    menu_quit: "ВЫХОД",
    menu_cursor: "КУРСОР",
    pause_title: "ПАУЗА",
    pause_footer: "A:ОК   B:ОТМЕНА   ВВЕРХ/ВНИЗ:НАВ",
    keys_title: "КЛАВИШИ",
    keys_footer: "A:ИЗМЕНИТЬ  L/R:РЕЖИМ  X:P1/P2  B:НАЗАД",
    keys_dropdown_footer: "A:ОК   B:ОТМЕНА   ВВЕРХ/ВНИЗ:НАВ",
    none: "(НЕТ)",
    flash_mouse_left: "ЛЕВЫЙ КЛИК",
    flash_mouse_right: "ПРАВЫЙ КЛИК",
    flash_space: "ПРОБЕЛ",
    flash_enter: "ВВОД",
    flash_escape: "ESC",
    flash_shift: "SHIFT",
    flash_control: "CTRL",
    flash_alt: "ALT",
    flash_tab: "TAB",
    flash_backspace: "BACKSPACE",
    flash_up: "ВВЕРХ",
    flash_down: "ВНИЗ",
    flash_left: "ВЛЕВО",
    flash_right: "ВПРАВО",
    empty_title: "НЕТ ИГР",
    empty_l1: "ПОМЕСТИТЕ ФАЙЛЫ .SWF В",
    empty_l2: "SDMC:/FLASHNX/   ИЛИ   SDMC:/SWITCH/FLASHNX/",
    empty_l3: "ЗАТЕМ ПЕРЕЗАПУСТИТЕ FLASHNX.",
    empty_footer: "Y:ЗАГРУЗКА ПО СЕТИ   -:ВЫХОД",
    list_footer: "L/R:ВКЛАДКИ  A:ИГРАТЬ  Y:СОРТ  -:ПОИСК  +:ОПЦИИ  ZL/ZR:СТР",
    applet_title: "РЕЖИМ АППЛЕТА",
    applet_notice: "ДЛЯ ЗАПУСКА ИГРЫ НУЖНА ВСЯ ПАМЯТЬ ПРИЛОЖЕНИЯ, КОТОРОЙ НЕТ В РЕЖИМЕ АППЛЕТА. В HOMEBREW MENU УДЕРЖИВАЙТЕ R НА ИГРЕ, ЧТОБЫ ЗАПУСТИТЬ FLASHNX СО ВСЕЙ ПАМЯТЬЮ.",
    options_title: "ОПЦИИ",
    opt_keys: "КЛАВИШИ",
    opt_rename: "ПЕРЕИМЕНОВАТЬ",
    opt_edit: "ИЗМЕНИТЬ",
    opt_delete: "УДАЛИТЬ",
    opt_back: "НАЗАД",
    options_footer: "A:ОК   B:НАЗАД",
    del_title: "УДАЛИТЬ ?",
    del_l1: "ФАЙЛ .swf, СОХРАНЕНИЯ (.sol),",
    del_l2: "КЛАВИШИ И ПСЕВДОНИМ БУДУТ УДАЛЕНЫ.",
    del_l3: "ДЕЙСТВИЕ НЕОБРАТИМО.",
    del_footer: "A: УДАЛИТЬ     B: ОТМЕНА",
    dist_title: "ЗАГРУЗКА ПО СЕТИ",
    dist_add: "+ ДОБАВИТЬ URL",
    dist_list_footer: "A:ЗАПУСК   +:ОПЦИИ   Y:СОРТ   -:ПОИСК   X:FLASHPOINT",
    dist_count: "{} URL",
    dist_subtitle: "ЗАГРУЗКА SWF С ARCHIVE.ORG",
    dist_press_a: "НАЖМИТЕ A ЧТОБЫ ВВЕСТИ URL",
    dist_example1: "ПРИМЕР: HTTPS://ARCHIVE.ORG/DETAILS/<ITEM-ID>",
    dist_example2: "ИЛИ ПРОСТО <ITEM-ID>",
    dist_history: "ИСТОРИЯ",
    dist_hint_zr: "ZR : ЗАГРУЗИТЬ ЭТОТ URL НАПРЯМУЮ",
    dist_hint_a: "A  : ВВЕСТИ / ИЗМЕНИТЬ URL (КЛАВИАТУРА)",
    dist_hint_lr: "L / R : ПРЕДЫДУЩИЙ / СЛЕДУЮЩИЙ URL",
    dist_footer_hist: "ZR:ОТКР  A:ИЗМ  ZL:УДАЛ  L/R:НАВ  Y:НАЗАД  -:ВЫХОД",
    dist_footer_nohist: "A:ВВЕСТИ URL   Y:НАЗАД   -:ВЫХОД",
    files_title: "ФАЙЛЫ ПО СЕТИ",
    files_filter: "ФИЛЬТР",
    files_footer: "A:ЗАГРУЗИТЬ   Y:СОРТ   -:ПОИСК   L/R:СТРАНИЦА   B:НАЗАД",
    dl_title: "ЗАГРУЗКА",
    dl_footer: "B:ОТМЕНА",
    toast_dl_ok: "{} ЗАГРУЖЕНО",
    toast_assets_missing: "НЕТ ДАННЫХ ИГРЫ: {}. ИГРА МОЖЕТ НЕ ЗАПУСТИТЬСЯ.",
    err_title: "ОШИБКА",
    err_footer: "A/B:ОК",
    err_footer_fix: "A/B:ОК   Y:ИСПРАВИТЬ URL",
    settings_title: "НАСТРОЙКИ",
    set_keys: "КЛАВИШИ ПО УМОЛЧАНИЮ",
    set_language: "ЯЗЫК",
    set_quit: "ВЫХОД",
    set_pseudo: "НИК",
    kbd_pseudo_guide: "Имя рядом с твоими профилями в каталоге",
    set_cursor_speed: "СКОРОСТЬ КУРСОРА",
    set_display_mode: "ЭКРАН",
    display_fit: "ВПИСАТЬ",
    display_fill: "ЗАПОЛНИТЬ",
    display_stretch: "РАСТЯНУТЬ",
    set_screen_filter: "ФИЛЬТР",
    set_home_view: "ВИД ГЛАВНОЙ",
    home_grid: "СЕТКА",
    home_list: "СПИСОК",
    home_strip: "ЛЕНТА",
    home_shelf: "ПОЛКА",
    set_game_prefs: "ПРЕДПОЧТЕНИЯ",
    prefs_title: "ПО УМОЛЧАНИЮ",
    filter_none: "НЕТ",
    filter_scanlines: "ЛИНИИ",
    filter_crt: "CRT",
    show_cursor: "ПОКАЗАТЬ КУРСОР",
    cursor_shown: "ПОКАЗАН",
    cursor_hidden: "СКРЫТ",
    set_back: "НАЗАД",
    set_report_bug: "СООБЩИТЬ ОБ ОШИБКЕ",
    tab_play: "ИГРАТЬ",
    tab_import: "ЗАГРУЗКА",
    tab_settings: "НАСТРОЙКИ",
    set_covers: "ОБЛОЖКИ ОНЛАЙН",
    lbl_on: "ON",
    lbl_off: "OFF",
    opt_cover: "ОБЛОЖКА",
    opt_favorite: "В ИЗБРАННОЕ",
    opt_unfavorite: "ИЗ ИЗБРАННОГО",
    opt_share: "ПОДЕЛИТЬСЯ КЛАВИШАМИ",
    profile_shared_ok: "ВАШИ КЛАВИШИ ОТПРАВЛЕНЫ. СПАСИБО ЗА ПОМОЩЬ СООБЩЕСТВУ.",
    opt_apply: "ПРИМЕНИТЬ ПРОФИЛЬ",
    profile_title: "ПРОФИЛИ КЛАВИШ",
    profile_footer: "A:ПРИМЕНИТЬ   B:НАЗАД   ВВЕРХ/ВНИЗ:НАВ",
    profile_none: "ДЛЯ ЭТОЙ ИГРЫ ПОКА НЕТ ПРОФИЛЯ. ПОДЕЛИТЕСЬ СВОИМ!",
    profile_catalog_offline: "КАТАЛОГ НЕДОСТУПЕН. ПРОВЕРЬТЕ ПОДКЛЮЧЕНИЕ.",
    profile_revert: "ВЕРНУТЬ МОИ КЛАВИШИ",
    profile_applied_ok: "ПРОФИЛЬ ПРИМЕНЕН. ВАШИ ПРЕЖНИЕ КЛАВИШИ СОХРАНЕНЫ.",
    profile_reverted_ok: "ВАШИ КЛАВИШИ ВОССТАНОВЛЕНЫ.",
    profile_share_confirm: "ПОДЕЛИТЬСЯ КЛАВИШАМИ ДЛЯ ЭТОЙ ИГРЫ?",
    share_confirm_update: "ОБНОВИТ ТВОЙ ОБЩИЙ ПРОФИЛЬ:",
    touches_edit: "ИЗМЕНИТЬ КЛАВИШИ",
    touches_footer: "A:ВЫБРАТЬ   B:НАЗАД   ВВЕРХ/ВНИЗ:НАВ",
    touches_revert_default: "СБРОСИТЬ НА КЛАВИШИ ПО УМОЛЧАНИЮ",
    profile_preview_title: "МОИ -> ПРОФИЛЬ",
    profile_preview_footer: "A:ПРИМЕНИТЬ   B:НАЗАД",
    profile_preview_none: "ЭТОТ ПРОФИЛЬ НЕ МЕНЯЕТ КЛАВИШИ.",
    profile_active: "(АКТИВНЫЙ)",
    profile_share_dup: "УЖЕ В КАТАЛОГЕ. ИЗМЕНИ КЛАВИШУ, ЧТОБЫ ПОДЕЛИТЬСЯ СВОЕЙ ВЕРСИЕЙ.",
    toast_already_imported: "{} УЖЕ В ВАШЕЙ БИБЛИОТЕКЕ.",
    profile_del_confirm: "УДАЛИТЬ ВАШ ПРОФИЛЬ?",
    profile_del_ok: "ВАШ ПРОФИЛЬ УДАЛЕН ИЗ КАТАЛОГА.",
    profile_del_not_mine: "НЕЧЕГО УДАЛЯТЬ: НЕ ОПУБЛИКОВАНО С ЭТОЙ КОНСОЛИ.",
    profile_del_hint: "X:УДАЛИТЬ",
    revert_preview_title: "СЕЙЧАС -> ПОСЛЕ СБРОСА",
    revert_preview_footer: "A:СБРОС   B:НАЗАД",
    cover_title: "ВЫБЕРИТЕ ОБЛОЖКУ",
    cover_footer: "A: ВЫБРАТЬ   -: ПОИСК   ВВЕРХ/ВНИЗ: НАВИГАЦИЯ   B: НАЗАД",
    cover_show_logos: "Y: ЛОГОТИПЫ",
    cover_show_shots: "Y: СКРИНШОТЫ",
    cover_off_notice: "ВКЛЮЧИТЕ ОБЛОЖКИ ОНЛАЙН В НАСТРОЙКАХ",
    cover_none: "НЕТ РЕЗУЛЬТАТОВ",
    fp_title: "FLASHPOINT",
    fp_footer: "A:СКАЧАТЬ   X:ПОИСК   Y:СОРТ   +:ИНФО   ZL+ZR:ФИЛЬТР {}   B:НАЗАД",
    fp_details_title: "ПОДРОБНОСТИ",
    fp_details_dev: "РАЗРАБОТЧИК",
    fp_details_publisher: "ИЗДАТЕЛЬ",
    fp_details_date: "ВЫПУСК",
    fp_details_size: "РАЗМЕР ЗАГРУЗКИ",
    fp_details_footer: "B:НАЗАД",
    sort_title: "СОРТИРОВКА",
    sort_footer: "A:ВЫБРАТЬ   X:ОБРАТНО   B:НАЗАД",
    sort_alpha: "ИМЯ",
    sort_recent: "ДОБАВЛЕН",
    sort_played: "ПО ВРЕМЕНИ",
    sort_size: "РАЗМЕР",
    played_label: "СЫГРАНО",
    sort_recent_played: "НЕДАВНО ИГРАЛИ",
    sort_dev: "РАЗРАБОТЧИК",
    sort_source: "ИСТОЧНИК",
    sort_files: "ЧИСЛО ФАЙЛОВ",
    fav_added: "ДОБАВЛЕНО В ИЗБРАННОЕ",
    fav_removed: "УДАЛЕНО ИЗ ИЗБРАННОГО",
    multifile: "МНОГОФАЙЛОВАЯ",
    sort_dir_asc: "ПО ВОЗР.",
    sort_dir_desc: "ПО УБЫВ.",
    settings_footer: "L/R:ВКЛАДКИ   A:ОК",
    lang_title: "ЯЗЫК",
    lang_footer: "A:ОК   B:ОТМЕНА",
    histdel_title: "УДАЛИТЬ URL ?",
    url_info_type: "ТИП",
    url_type_swf: "ОДИН .SWF",
    url_type_list: "СПИСОК ФАЙЛОВ",
    url_info_files: "НА SD",
    url_info_added: "ДОБАВЛЕНО",
    histdel_msg: "УБРАТЬ ЭТОТ URL ИЗ ИСТОРИИ?",
    err_too_large: "ОТВЕТ ARCHIVE.ORG СЛИШКОМ БОЛЬШОЙ (>4 МБ). ЭЛЕМЕНТ СЛИШКОМ ВЕЛИК ДЛЯ ЭТОЙ ВЕРСИИ.",
    err_https: "СБОЙ ПЕРЕДАЧИ ({}). ПРОВЕРЬТЕ WIFI И URL.",
    err_offline: "НЕТ СОЕДИНЕНИЯ. ПРОВЕРЬТЕ WIFI КОНСОЛИ.",
    err_timeout: "СЕРВЕР НЕ ОТВЕТИЛ ВОВРЕМЯ. ПОПРОБУЙТЕ СНОВА.",
    err_tls: "ЗАЩИЩЁННОЕ СОЕДИНЕНИЕ ОТКЛОНЕНО. ПРОВЕРЬТЕ ДАТУ И ВРЕМЯ КОНСОЛИ: НЕВЕРНЫЕ ЧАСЫ ЛОМАЮТ HTTPS.",
    err_response_big: "ОТВЕТ СЛИШКОМ БОЛЬШОЙ. УТОЧНИТЕ ЗАПРОС.",
    err_sd_write: "НЕ УДАЛОСЬ ЗАПИСАТЬ НА SD-КАРТУ. ВОЗМОЖНО, ОНА ЗАПОЛНЕНА.",
    err_http_status: "СЕРВЕР ОТКЛОНИЛ ЗАПРОС (HTTP {}). ПОПРОБУЙТЕ ПОЗЖЕ.",
    err_not_found: "НЕ НАЙДЕНО (404). НЕВЕРНЫЙ URL ИЛИ ID ЭЛЕМЕНТА, ЛИБО ОН УДАЛЁН.",
    err_json: "НЕЧИТАЕМЫЙ JSON ARCHIVE.ORG: {}",
    err_json_no_files: "В JSON НЕТ ПОЛЯ \"files\"",
    err_dl_start: "НЕ УДАЛОСЬ НАЧАТЬ ЗАГРУЗКУ (КОД {}).",
    err_dl_failed: "ЗАГРУЗКА НЕ УДАЛАСЬ (КОД {})",
    err_dl_cancelled: "ЗАГРУЗКА ОТМЕНЕНА ПОЛЬЗОВАТЕЛЕМ.",
    err_url_invalid: "НЕВЕРНЫЙ URL. ОЖИДАЛСЯ URL ARCHIVE.ORG ВИДА https://archive.org/details/<id> ИЛИ ПРОСТО <id>.",
    err_no_swf: "ДЛЯ ЭТОЙ ИГРЫ НЕ НАЙДЕНО ФАЙЛОВ .SWF.",
    err_dl_not_a_game: "ЗАГРУЗКА ВЕРНУЛА НЕ ИГРОВОЙ ФАЙЛ. ПРОВЕРЬТЕ АДРЕС.",
    err_fp_html_game: "ЭТА ИГРА ЗАПУСКАЕТСЯ ЧЕРЕЗ HTML-СТРАНИЦУ (FLASHVARS), ПОКА НЕ ПОДДЕРЖИВАЕТСЯ.",
    kbd_url_header: "FlashNX - Zagruzka po seti",
    kbd_url_guide: "URL elementa archive.org (naprimer: https://archive.org/download/your-game-id)",
    kbd_rename_header: "FlashNX - Pereimenovat",
    kbd_rename_guide: "Otobrazhaemoe imya (ostavte pustym chtoby vernut imya fayla)",
    kbd_search_header: "FlashNX - Poisk",
    kbd_search_guide: "Filtr po imeni fayla (pusto = pokazat vse)",
    bug_pick_title: "СООБЩИТЬ ОБ ОШИБКЕ",
    bug_pick_footer: "A:ВЫБРАТЬ   B:НАЗАД   ВВЕРХ/ВНИЗ:НАВ",
    bug_no_games: "НЕТ ИГР ДЛЯ ОТЧЕТА. СНАЧАЛА ДОБАВЬТЕ ИЛИ ЗАГРУЗИТЕ .SWF.",
    bug_ok_title: "СПАСИБО!",
    bug_ok_msg: "ОТЧЕТ ОТПРАВЛЕН. СПАСИБО ЧТО ПОМОГАЕТЕ УЛУЧШИТЬ FLASHNX.",
    bug_fail_title: "ОШИБКА",
    kbd_bug_header: "FlashNX - Soobshchit ob oshibke",
    kbd_bug_guide: "Otkroet PUBLICHNUYU issue na GitHub. Opishite problemu. @nik po zhelaniyu.",
    set_suggest: "ПРЕДЛОЖИТЬ ИДЕЮ",
    kbd_suggest_header: "FlashNX - Predlozhit ideyu",
    kbd_suggest_guide: "Otkroet PUBLICHNUYU issue na GitHub. Vasha ideya / predlozhenie.",
};

// German — uppercase Latin. draw_text folds ASCII to uppercase but NOT
// accented letters, so umlauts are written in their UPPERCASE form (Ä Ö Ü)
// to hit the glyphs added to the font; ß is rendered as SS (uppercase German
// convention). The kbd_* strings go to the Switch software keyboard, which
// renders full Unicode, so those keep natural mixed case + lowercase accents.
const DE: Strings = Strings {
    optimizing: "OPTIMIERUNG",
    menu_resume: "FORTSETZEN",
    menu_keys: "STEUERUNG",
    menu_restart: "NEUSTART",
    menu_quit: "BEENDEN",
    menu_cursor: "CURSOR",
    pause_title: "PAUSE",
    pause_footer: "A:OK   B:ABBRECHEN   HOCH/RUNTER:NAV",
    keys_title: "STEUERUNG",
    keys_footer: "A:BEARB.  L/R:MODUS  X:S1/S2  B:ZUR\u{00DC}CK",
    keys_dropdown_footer: "A:OK   B:ABBRECHEN   HOCH/RUNTER:NAV",
    none: "(keine)",
    flash_mouse_left: "Linksklick",
    flash_mouse_right: "Rechtsklick",
    flash_space: "Leertaste",
    flash_enter: "Enter",
    flash_escape: "Escape",
    flash_shift: "Umschalt",
    flash_control: "Strg",
    flash_alt: "Alt",
    flash_tab: "Tab",
    flash_backspace: "R\u{00DC}cktaste",
    flash_up: "Hoch",
    flash_down: "Runter",
    flash_left: "Links",
    flash_right: "Rechts",
    empty_title: "KEINE SPIELE",
    empty_l1: "LEGE .SWF-DATEIEN IN",
    empty_l2: "SDMC:/FLASHNX/   ODER   SDMC:/SWITCH/FLASHNX/",
    empty_l3: "DANN FLASHNX NEU STARTEN.",
    empty_footer: "Y:ONLINE-IMPORT   -:BEENDEN",
    list_footer: "L/R:TABS  A:SPIELEN  Y:SORT.  -:SUCHE  +:OPTIONEN  ZL/ZR:SEITE",
    applet_title: "APPLET-MODUS",
    applet_notice: "EIN SPIEL ZU STARTEN BRAUCHT DEN VOLLEN APP-SPEICHER, DEN DER APPLET-MODUS NICHT HAT. HALTE IM HOMEBREW-MEN\u{00DC} R AUF EINEM TITEL (ODER NUTZE EINEN FORWARDER), UM FLASHNX MIT VOLLEM SPEICHER ZU STARTEN.",
    options_title: "OPTIONEN",
    opt_keys: "STEUERUNG",
    opt_rename: "UMBENENNEN",
    opt_edit: "BEARBEITEN",
    opt_delete: "L\u{00D6}SCHEN",
    opt_back: "ZUR\u{00DC}CK",
    options_footer: "A:OK   B:ZUR\u{00DC}CK",
    del_title: "L\u{00D6}SCHEN ?",
    del_l1: "DIE .swf-DATEI, DIE SPEICHER (.sol),",
    del_l2: "DIE STEUERUNG UND DER ALIAS WERDEN GEL\u{00D6}SCHT.",
    del_l3: "NICHT R\u{00DC}CKG\u{00C4}NGIG ZU MACHEN.",
    del_footer: "A: L\u{00D6}SCHEN     B: ABBRECHEN",
    dist_title: "ONLINE-IMPORT",
    dist_add: "+ URL HINZUF\u{00DC}GEN",
    dist_list_footer: "A:STARTEN   +:OPTIONEN   Y:SORT.   -:SUCHE   X:FLASHPOINT",
    dist_count: "{} URL(S)",
    dist_subtitle: "SWF VON ARCHIVE.ORG HERUNTERLADEN",
    dist_press_a: "DR\u{00DC}CKE A, UM EINE URL EINZUGEBEN",
    dist_example1: "BEISPIEL: HTTPS://ARCHIVE.ORG/DETAILS/<ITEM-ID>",
    dist_example2: "ODER EINFACH <ITEM-ID>",
    dist_history: "VERLAUF",
    dist_hint_zr: "ZR : DIESE URL DIREKT LADEN",
    dist_hint_a: "A  : URL EINGEBEN / BEARBEITEN (TASTATUR)",
    dist_hint_lr: "L / R : VORHERIGE / N\u{00C4}CHSTE URL",
    dist_footer_hist: "ZR:\u{00D6}FFNEN  A:BEARB.  ZL:L\u{00D6}SCH.  L/R:NAV  Y:LOKAL  -:BEENDEN",
    dist_footer_nohist: "A:URL EINGEBEN   Y:ZUR\u{00DC}CK LOKAL   -:BEENDEN",
    files_title: "ENTFERNTE DATEIEN",
    files_filter: "FILTER",
    files_footer: "A:LADEN   Y:SORT.   -:SUCHE   L/R:SEITE   B:ZUR\u{00DC}CK",
    dl_title: "WIRD GELADEN",
    dl_footer: "B:ABBRECHEN",
    toast_dl_ok: "{} HERUNTERGELADEN",
    toast_assets_missing: "SPIELDATEN FEHLEN: {}. DAS SPIEL STARTET M\u{00D6}GLICHERWEISE NICHT.",
    err_title: "FEHLER",
    err_footer: "A/B:OK",
    err_footer_fix: "A/B:OK   Y:URL KORRIGIEREN",
    settings_title: "EINSTELLUNGEN",
    set_keys: "STANDARDSTEUERUNG",
    set_language: "SPRACHE",
    set_quit: "BEENDEN",
    set_pseudo: "SPITZNAME",
    kbd_pseudo_guide: "Dein Name, neben deinen geteilten Profilen",
    set_cursor_speed: "CURSORGESCHWINDIGKEIT",
    set_display_mode: "ANZEIGE",
    display_fit: "EINPASSEN",
    display_fill: "F\u{00DC}LLEN",
    display_stretch: "STRECKEN",
    set_screen_filter: "FILTER",
    set_home_view: "STARTANSICHT",
    home_grid: "RASTER",
    home_list: "LISTE",
    home_strip: "LEISTE",
    home_shelf: "REGAL",
    set_game_prefs: "SPIELVORGABEN",
    prefs_title: "STANDARDWERTE",
    filter_none: "KEINER",
    filter_scanlines: "LINIEN",
    filter_crt: "CRT",
    show_cursor: "CURSOR ZEIGEN",
    cursor_shown: "SICHTBAR",
    cursor_hidden: "VERBORGEN",
    set_back: "ZUR\u{00DC}CK",
    set_report_bug: "FEHLER MELDEN",
    tab_play: "SPIELEN",
    tab_import: "IMPORT",
    tab_settings: "EINSTELLUNGEN",
    set_covers: "ONLINE-COVER",
    lbl_on: "AN",
    lbl_off: "AUS",
    opt_cover: "COVER",
    opt_favorite: "FAVORIT",
    opt_unfavorite: "FAVORIT ENTFERNEN",
    opt_share: "STEUERUNG TEILEN",
    profile_shared_ok: "DEINE STEUERUNG WURDE GESENDET. DANKE F\u{00DC}R DEINE HILFE.",
    opt_apply: "PROFIL ANWENDEN",
    profile_title: "STEUERUNGSPROFILE",
    profile_footer: "A:ANWENDEN   B:ZUR\u{00DC}CK   HOCH/RUNTER:NAV",
    profile_none: "NOCH KEIN PROFIL F\u{00DC}R DIESES SPIEL. TEILE DEINS!",
    profile_catalog_offline: "KATALOG NICHT VERF\u{00DC}GBAR. PR\u{00DC}FE DEINE VERBINDUNG.",
    profile_revert: "ZU MEINER STEUERUNG ZUR\u{00DC}CK",
    profile_applied_ok: "PROFIL ANGEWENDET. DEINE BISHERIGE STEUERUNG WURDE GESICHERT.",
    profile_reverted_ok: "DEINE STEUERUNG WURDE WIEDERHERGESTELLT.",
    profile_share_confirm: "DEINE STEUERUNG F\u{00DC}R DIESES SPIEL TEILEN?",
    share_confirm_update: "AKTUALISIERT DEIN GETEILTES PROFIL:",
    touches_edit: "STEUERUNG BEARBEITEN",
    touches_footer: "A:W\u{00C4}HLEN   B:ZUR\u{00DC}CK   HOCH/RUNTER:NAV",
    touches_revert_default: "AUF STANDARD ZUR\u{00DC}CKSETZEN",
    profile_preview_title: "MEINE -> PROFIL",
    profile_preview_footer: "A:ANWENDEN   B:ZUR\u{00DC}CK",
    profile_preview_none: "DIESES PROFIL \u{00C4}NDERT KEINE TASTE.",
    profile_active: "(AKTIV)",
    profile_share_dup: "BEREITS IM KATALOG. \u{00C4}NDERE EINE TASTE, UM DEINE VERSION ZU TEILEN.",
    toast_already_imported: "{} IST BEREITS IN DEINER BIBLIOTHEK.",
    profile_del_confirm: "DEIN GETEILTES PROFIL L\u{00D6}SCHEN?",
    profile_del_ok: "DEIN GETEILTES PROFIL WURDE GEL\u{00D6}SCHT.",
    profile_del_not_mine: "NICHTS ZU L\u{00D6}SCHEN VON DIESER KONSOLE.",
    profile_del_hint: "X:L\u{00D6}SCHEN",
    revert_preview_title: "JETZT -> NACH R\u{00DC}CKSETZEN",
    revert_preview_footer: "A:R\u{00DC}CKSETZEN   B:ZUR\u{00DC}CK",
    cover_title: "COVER W\u{00C4}HLEN",
    cover_footer: "A: W\u{00C4}HLEN   -: SUCHE   HOCH/RUNTER: BEWEGEN   B: ZUR\u{00DC}CK",
    cover_show_logos: "Y: LOGOS",
    cover_show_shots: "Y: SCREENSHOTS",
    cover_off_notice: "ONLINE-COVER IN DEN EINSTELLUNGEN AKTIVIEREN",
    cover_none: "KEINE TREFFER",
    fp_title: "FLASHPOINT",
    fp_footer: "A:LADEN   X:SUCHE   Y:SORT.   +:INFO   ZL+ZR:FILTER {}   B:ZUR\u{00DC}CK",
    fp_details_title: "DETAILS",
    fp_details_dev: "ENTWICKLER",
    fp_details_publisher: "HERAUSGEBER",
    fp_details_date: "VER\u{00D6}FFENTLICHT",
    fp_details_size: "DOWNLOAD-GR\u{00D6}SSE",
    fp_details_footer: "B:ZUR\u{00DC}CK",
    sort_title: "SORTIEREN NACH",
    sort_footer: "A:W\u{00C4}HLEN   X:UMKEHREN   B:ZUR\u{00DC}CK",
    sort_alpha: "NAME",
    sort_recent: "HINZUGEF\u{00DC}GT",
    sort_played: "MEIST GESPIELT",
    sort_size: "GR\u{00D6}SSE",
    played_label: "GESPIELT",
    sort_recent_played: "ZULETZT GESPIELT",
    sort_dev: "ENTWICKLER",
    sort_source: "QUELLE",
    sort_files: "DATEIANZAHL",
    fav_added: "ZU FAVORITEN HINZUGEF\u{00DC}GT",
    fav_removed: "AUS FAVORITEN ENTFERNT",
    multifile: "MEHRDATEI",
    sort_dir_asc: "AUFSTEIGEND",
    sort_dir_desc: "ABSTEIGEND",
    settings_footer: "L/R:TABS   A:OK",
    lang_title: "SPRACHE",
    lang_footer: "A:OK   B:ABBRECHEN",
    histdel_title: "URL L\u{00D6}SCHEN ?",
    url_info_type: "TYP",
    url_type_swf: "EINZELNE .SWF",
    url_type_list: "DATEILISTE",
    url_info_files: "AUF SD",
    url_info_added: "HINZUGEF\u{00DC}GT",
    histdel_msg: "DIESE URL AUS DEM VERLAUF ENTFERNEN?",
    err_too_large: "ARCHIVE.ORG-ANTWORT ZU GROSS (>4 MB). ELEMENT ZU GROSS F\u{00DC}R DIESE VERSION.",
    err_https: "\u{00DC}BERTRAGUNG FEHLGESCHLAGEN ({}). PR\u{00DC}FE WLAN UND URL.",
    err_offline: "KEINE VERBINDUNG. PR\u{00DC}FE DAS WLAN DER KONSOLE.",
    err_timeout: "DER SERVER HAT NICHT RECHTZEITIG GEANTWORTET. ERNEUT VERSUCHEN.",
    err_tls: "SICHERE VERBINDUNG ABGELEHNT. PR\u{00DC}FE DATUM UND UHRZEIT DER KONSOLE: EINE FALSCHE UHR BRICHT HTTPS.",
    err_response_big: "ANTWORT ZU GROSS. VERSUCHE EINE GENAUERE SUCHE.",
    err_sd_write: "SCHREIBEN AUF DIE SD-KARTE FEHLGESCHLAGEN. SIE IST EVENTUELL VOLL.",
    err_http_status: "DER SERVER HAT DIE ANFRAGE ABGELEHNT (HTTP {}). SP\u{00C4}TER ERNEUT VERSUCHEN.",
    err_not_found: "NICHT GEFUNDEN (404). URL ODER ITEM-ID IST FALSCH, ODER ES WURDE ENTFERNT.",
    err_json: "UNLESBARES ARCHIVE.ORG-JSON: {}",
    err_json_no_files: "JSON OHNE \"files\"-FELD",
    err_dl_start: "DOWNLOAD KONNTE NICHT GESTARTET WERDEN (CODE {}).",
    err_dl_failed: "DOWNLOAD FEHLGESCHLAGEN (CODE {})",
    err_dl_cancelled: "DOWNLOAD VOM BENUTZER ABGEBROCHEN.",
    err_url_invalid: "UNG\u{00DC}LTIGE URL. ERWARTET WIRD EINE ARCHIVE.ORG-URL WIE https://archive.org/details/<id> ODER EINFACH <id>.",
    err_no_swf: "KEINE .SWF-DATEI F\u{00DC}R DIESES SPIEL GEFUNDEN.",
    err_dl_not_a_game: "DER DOWNLOAD HAT KEINE SPIELDATEI GELIEFERT. PR\u{00DC}FE DIE ADRESSE.",
    err_fp_html_game: "DIESES SPIEL STARTET \u{00DC}BER EINE HTML-SEITE (FLASHVARS), NOCH NICHT UNTERST\u{00DC}TZT.",
    kbd_url_header: "FlashNX - Online-Import",
    kbd_url_guide: "archive.org-Item-URL (z.B. https://archive.org/download/your-game-id)",
    kbd_rename_header: "FlashNX - Spiel umbenennen",
    kbd_rename_guide: "Anzeigename (leer lassen, um zum Dateinamen zur\u{00FC}ckzukehren)",
    kbd_search_header: "FlashNX - Suche",
    kbd_search_guide: "Nach Dateiname filtern (leer = alles anzeigen)",
    bug_pick_title: "FEHLER MELDEN",
    bug_pick_footer: "A:W\u{00C4}HLEN   B:ZUR\u{00DC}CK   HOCH/RUNTER:NAV",
    bug_no_games: "NOCH KEIN SPIEL ZU MELDEN. IMPORTIERE ODER LEGE ZUERST EINE .SWF AB.",
    bug_ok_title: "DANKE!",
    bug_ok_msg: "DEIN BERICHT WURDE GESENDET. DANKE, DASS DU HILFST, FLASHNX ZU VERBESSERN.",
    bug_fail_title: "FEHLGESCHLAGEN",
    kbd_bug_header: "FlashNX - Fehler melden",
    kbd_bug_guide: "\u{00D6}ffnet ein \u{00F6}ffentliches GitHub-Issue. Beschreibe das Problem. Optional: dein @Name.",
    set_suggest: "VORSCHLAG MACHEN",
    kbd_suggest_header: "FlashNX - Vorschlag machen",
    kbd_suggest_guide: "\u{00D6}ffnet ein \u{00F6}ffentliches GitHub-Issue. Deine Idee / dein Wunsch f\u{00FC}r FlashNX.",
};

// Italian — uppercase Latin. Accented uppercase vowels (À È É Ì Ò Ù) are
// written directly to hit the font glyphs (draw_text does not fold accents).
const IT: Strings = Strings {
    optimizing: "OTTIMIZZAZIONE",
    menu_resume: "RIPRENDI",
    menu_keys: "COMANDI",
    menu_restart: "RIAVVIA",
    menu_quit: "ESCI",
    menu_cursor: "CURSORE",
    pause_title: "PAUSA",
    pause_footer: "A:OK   B:ANNULLA   SU/GI\u{00D9}:NAV",
    keys_title: "COMANDI",
    keys_footer: "A:MODIFICA  L/R:MODO  X:G1/G2  B:INDIETRO",
    keys_dropdown_footer: "A:OK   B:ANNULLA   SU/GI\u{00D9}:NAV",
    none: "(nessuno)",
    flash_mouse_left: "Clic sinistro",
    flash_mouse_right: "Clic destro",
    flash_space: "Spazio",
    flash_enter: "Invio",
    flash_escape: "Esc",
    flash_shift: "Maiusc",
    flash_control: "Ctrl",
    flash_alt: "Alt",
    flash_tab: "Tab",
    flash_backspace: "Backspace",
    flash_up: "Su",
    flash_down: "Gi\u{00D9}",
    flash_left: "Sinistra",
    flash_right: "Destra",
    empty_title: "NESSUN GIOCO",
    empty_l1: "INSERISCI FILE .SWF IN",
    empty_l2: "SDMC:/FLASHNX/   O   SDMC:/SWITCH/FLASHNX/",
    empty_l3: "POI RIAVVIA FLASHNX.",
    empty_footer: "Y:IMPORT REMOTO   -:ESCI",
    list_footer: "L/R:SCHEDE  A:GIOCA  Y:ORDINA  -:CERCA  +:OPZIONI  ZL/ZR:PAGINA",
    applet_title: "MODALIT\u{00C0} APPLET",
    applet_notice: "AVVIARE UN GIOCO RICHIEDE TUTTA LA MEMORIA DELL'APP, CHE LA MODALIT\u{00C0} APPLET NON HA. NELL'HOMEBREW MENU, TIENI PREMUTO R SU UN TITOLO (O USA UN FORWARDER) PER AVVIARE FLASHNX CON TUTTA LA MEMORIA.",
    options_title: "OPZIONI",
    opt_keys: "COMANDI",
    opt_rename: "RINOMINA",
    opt_edit: "MODIFICA",
    opt_delete: "ELIMINA",
    opt_back: "INDIETRO",
    options_footer: "A:OK   B:INDIETRO",
    del_title: "ELIMINARE ?",
    del_l1: "IL FILE .swf, I SALVATAGGI (.sol),",
    del_l2: "I COMANDI E L'ALIAS SARANNO ELIMINATI.",
    del_l3: "AZIONE IRREVERSIBILE.",
    del_footer: "A: ELIMINA     B: ANNULLA",
    dist_title: "IMPORT REMOTO",
    dist_add: "+ AGGIUNGI UN URL",
    dist_list_footer: "A:AVVIA   +:OPZIONI   Y:ORDINA   -:CERCA   X:FLASHPOINT",
    dist_count: "{} URL",
    dist_subtitle: "SCARICA SWF DA ARCHIVE.ORG",
    dist_press_a: "PREMI A PER INSERIRE UN URL",
    dist_example1: "ESEMPIO: HTTPS://ARCHIVE.ORG/DETAILS/<ITEM-ID>",
    dist_example2: "O SEMPLICEMENTE <ITEM-ID>",
    dist_history: "CRONOLOGIA",
    dist_hint_zr: "ZR : CARICA QUESTO URL DIRETTAMENTE",
    dist_hint_a: "A  : INSERISCI / MODIFICA URL (TASTIERA)",
    dist_hint_lr: "L / R : URL PRECEDENTE / SUCCESSIVO",
    dist_footer_hist: "ZR:APRI  A:MOD.  ZL:ELIM.  L/R:NAV  Y:LOCALE  -:ESCI",
    dist_footer_nohist: "A:INSERISCI URL   Y:TORNA LOCALE   -:ESCI",
    files_title: "FILE REMOTI",
    files_filter: "FILTRO",
    files_footer: "A:SCARICA   Y:ORDINA   -:CERCA   L/R:PAGINA   B:INDIETRO",
    dl_title: "SCARICAMENTO",
    dl_footer: "B:ANNULLA",
    toast_dl_ok: "{} SCARICATO",
    toast_assets_missing: "DATI DI GIOCO MANCANTI: {}. IL GIOCO POTREBBE NON AVVIARSI.",
    err_title: "ERRORE",
    err_footer: "A/B:OK",
    err_footer_fix: "A/B:OK   Y:CORREGGI L'URL",
    settings_title: "IMPOSTAZIONI",
    set_keys: "COMANDI PREDEFINITI",
    set_language: "LINGUA",
    set_quit: "ESCI",
    set_pseudo: "SOPRANNOME",
    kbd_pseudo_guide: "Il tuo nome, accanto ai profili condivisi",
    set_cursor_speed: "VELOCIT\u{00C0} CURSORE",
    set_display_mode: "SCHERMO",
    display_fit: "ADATTA",
    display_fill: "RIEMPI",
    display_stretch: "ALLARGA",
    set_screen_filter: "FILTRO",
    set_home_view: "VISTA HOME",
    home_grid: "GRIGLIA",
    home_list: "ELENCO",
    home_strip: "STRISCIA",
    home_shelf: "SCAFFALE",
    set_game_prefs: "PREFERENZE",
    prefs_title: "VALORI PREDEFINITI",
    filter_none: "NESSUNO",
    filter_scanlines: "LINEE",
    filter_crt: "CRT",
    show_cursor: "MOSTRA CURSORE",
    cursor_shown: "VISIBILE",
    cursor_hidden: "NASCOSTO",
    set_back: "INDIETRO",
    set_report_bug: "SEGNALA UN BUG",
    tab_play: "GIOCA",
    tab_import: "IMPORTA",
    tab_settings: "IMPOSTAZIONI",
    set_covers: "COPERTINE ONLINE",
    lbl_on: "ON",
    lbl_off: "OFF",
    opt_cover: "COPERTINA",
    opt_favorite: "PREFERITO",
    opt_unfavorite: "RIMUOVI PREFERITO",
    opt_share: "CONDIVIDI COMANDI",
    profile_shared_ok: "I TUOI COMANDI SONO STATI INVIATI. GRAZIE PER IL CONTRIBUTO.",
    opt_apply: "APPLICA UN PROFILO",
    profile_title: "PROFILI COMANDI",
    profile_footer: "A:APPLICA   B:INDIETRO   SU/GI\u{00D9}:NAV",
    profile_none: "NESSUN PROFILO PER QUESTO GIOCO. CONDIVIDI IL TUO!",
    profile_catalog_offline: "CATALOGO NON DISPONIBILE. CONTROLLA LA CONNESSIONE.",
    profile_revert: "TORNA AI MIEI COMANDI",
    profile_applied_ok: "PROFILO APPLICATO. I TUOI COMANDI PRECEDENTI SONO SALVATI.",
    profile_reverted_ok: "I TUOI COMANDI SONO STATI RIPRISTINATI.",
    profile_share_confirm: "CONDIVIDERE I TUOI COMANDI PER QUESTO GIOCO?",
    share_confirm_update: "AGGIORNA IL TUO PROFILO CONDIVISO:",
    touches_edit: "MODIFICA COMANDI",
    touches_footer: "A:SCEGLI   B:INDIETRO   SU/GI\u{00D9}:NAV",
    touches_revert_default: "RIPRISTINA COMANDI PREDEFINITI",
    profile_preview_title: "MIEI -> PROFILO",
    profile_preview_footer: "A:APPLICA   B:INDIETRO",
    profile_preview_none: "QUESTO PROFILO NON CAMBIA NESSUN TASTO.",
    profile_active: "(ATTIVO)",
    profile_share_dup: "GI\u{00C0} NEL CATALOGO. CAMBIA UN TASTO PER CONDIVIDERE LA TUA VERSIONE.",
    toast_already_imported: "{} \u{00C8} GI\u{00C0} NELLA TUA LIBRERIA.",
    profile_del_confirm: "ELIMINARE IL TUO PROFILO CONDIVISO?",
    profile_del_ok: "IL TUO PROFILO CONDIVISO \u{00C8} STATO ELIMINATO.",
    profile_del_not_mine: "NIENTE DA ELIMINARE DA QUESTA CONSOLE.",
    profile_del_hint: "X:ELIMINA",
    revert_preview_title: "ORA -> DOPO IL RIPRISTINO",
    revert_preview_footer: "A:RIPRISTINA   B:INDIETRO",
    cover_title: "SCEGLI UNA COPERTINA",
    cover_footer: "A: SCEGLI   -: CERCA   SU/GI\u{00D9}: SPOSTA   B: INDIETRO",
    cover_show_logos: "Y: LOGHI",
    cover_show_shots: "Y: SCHERMATE",
    cover_off_notice: "ATTIVA LE COPERTINE ONLINE NELLE IMPOSTAZIONI",
    cover_none: "NESSUN RISULTATO",
    fp_title: "FLASHPOINT",
    fp_footer: "A:SCARICA   X:CERCA   Y:ORDINA   +:INFO   ZL+ZR:FILTRO {}   B:INDIETRO",
    fp_details_title: "DETTAGLI",
    fp_details_dev: "SVILUPPATORE",
    fp_details_publisher: "EDITORE",
    fp_details_date: "USCITA",
    fp_details_size: "DIMENSIONE DOWNLOAD",
    fp_details_footer: "B:INDIETRO",
    sort_title: "ORDINA PER",
    sort_footer: "A:SCEGLI   X:INVERTI   B:INDIETRO",
    sort_alpha: "NOME",
    sort_recent: "AGGIUNTO",
    sort_played: "PI\u{00D9} GIOCATI",
    sort_size: "DIMENSIONE",
    played_label: "GIOCATO",
    sort_recent_played: "GIOCATO DI RECENTE",
    sort_dev: "SVILUPPATORE",
    sort_source: "FONTE",
    sort_files: "NUMERO DI FILE",
    fav_added: "AGGIUNTO AI PREFERITI",
    fav_removed: "RIMOSSO DAI PREFERITI",
    multifile: "MULTI-FILE",
    sort_dir_asc: "CRESCENTE",
    sort_dir_desc: "DECRESCENTE",
    settings_footer: "L/R:SCHEDE   A:OK",
    lang_title: "LINGUA",
    lang_footer: "A:OK   B:ANNULLA",
    histdel_title: "ELIMINARE URL ?",
    url_info_type: "TIPO",
    url_type_swf: "FILE .SWF",
    url_type_list: "ELENCO DI FILE",
    url_info_files: "SU SD",
    url_info_added: "AGGIUNTA IL",
    histdel_msg: "RIMUOVERE QUESTO URL DALLA CRONOLOGIA?",
    err_too_large: "RISPOSTA DI ARCHIVE.ORG TROPPO GRANDE (>4 MB). ELEMENTO TROPPO GRANDE PER QUESTA VERSIONE.",
    err_https: "TRASFERIMENTO FALLITO ({}). CONTROLLA IL WIFI E L'URL.",
    err_offline: "NESSUNA CONNESSIONE. CONTROLLA IL WIFI DELLA CONSOLE.",
    err_timeout: "IL SERVER NON HA RISPOSTO IN TEMPO. RIPROVA.",
    err_tls: "CONNESSIONE SICURA RIFIUTATA. CONTROLLA DATA E ORA DELLA CONSOLE: UN OROLOGIO ERRATO ROMPE L'HTTPS.",
    err_response_big: "RISPOSTA TROPPO GRANDE. PROVA UNA RICERCA PI\u{00D9} PRECISA.",
    err_sd_write: "IMPOSSIBILE SCRIVERE SULLA SCHEDA SD. FORSE \u{00C8} PIENA.",
    err_http_status: "IL SERVER HA RIFIUTATO LA RICHIESTA (HTTP {}). RIPROVA PI\u{00D9} TARDI.",
    err_not_found: "NON TROVATO (404). URL O ID DELL'ELEMENTO ERRATO, OPPURE \u{00C8} STATO RIMOSSO.",
    err_json: "JSON DI ARCHIVE.ORG ILLEGGIBILE: {}",
    err_json_no_files: "JSON SENZA CAMPO \"files\"",
    err_dl_start: "IMPOSSIBILE AVVIARE IL DOWNLOAD (CODICE {}).",
    err_dl_failed: "DOWNLOAD FALLITO (CODICE {})",
    err_dl_cancelled: "DOWNLOAD ANNULLATO DALL'UTENTE.",
    err_url_invalid: "URL NON VALIDO. ATTESO UN URL DI ARCHIVE.ORG TIPO https://archive.org/details/<id> O SEMPLICEMENTE <id>.",
    err_no_swf: "NESSUN FILE .SWF TROVATO PER QUESTO GIOCO.",
    err_dl_not_a_game: "IL DOWNLOAD NON HA RESTITUITO UN FILE DI GIOCO. CONTROLLA L'INDIRIZZO.",
    err_fp_html_game: "QUESTO GIOCO SI AVVIA DA UNA PAGINA HTML (FLASHVARS), NON ANCORA SUPPORTATO.",
    kbd_url_header: "FlashNX - Import remoto",
    kbd_url_guide: "URL dell'elemento archive.org (es: https://archive.org/download/your-game-id)",
    kbd_rename_header: "FlashNX - Rinomina gioco",
    kbd_rename_guide: "Nome visualizzato (lascia vuoto per tornare al nome del file)",
    kbd_search_header: "FlashNX - Cerca",
    kbd_search_guide: "Filtra per nome file (vuoto = mostra tutto)",
    bug_pick_title: "SEGNALA UN BUG",
    bug_pick_footer: "A:SCEGLI   B:INDIETRO   SU/GI\u{00D9}:NAV",
    bug_no_games: "NESSUN GIOCO DA SEGNALARE. IMPORTA O INSERISCI PRIMA UN .SWF.",
    bug_ok_title: "GRAZIE!",
    bug_ok_msg: "LA TUA SEGNALAZIONE \u{00C8} STATA INVIATA. GRAZIE PER AIUTARE A MIGLIORARE FLASHNX.",
    bug_fail_title: "FALLITO",
    kbd_bug_header: "FlashNX - Segnala un bug",
    kbd_bug_guide: "Apre una issue PUBBLICA su GitHub. Descrivi il problema. Facoltativo: il tuo @nome.",
    set_suggest: "PROPONI UN'IDEA",
    kbd_suggest_header: "FlashNX - Proponi un'idea",
    kbd_suggest_guide: "Apre una issue PUBBLICA su GitHub. La tua idea / proposta per FlashNX.",
};

// Portuguese (Brazil) — uppercase Latin. Accented uppercase letters
// (Á Â Ã É Ê Í Ó Ô Õ Ú Ç) are written directly to hit the font glyphs.
const PT: Strings = Strings {
    optimizing: "OTIMIZANDO",
    menu_resume: "CONTINUAR",
    menu_keys: "CONTROLES",
    menu_restart: "REINICIAR",
    menu_quit: "SAIR",
    menu_cursor: "CURSOR",
    pause_title: "PAUSA",
    pause_footer: "A:OK   B:CANCELAR   CIMA/BAIXO:NAV",
    keys_title: "CONTROLES",
    keys_footer: "A:EDITAR  L/R:MODO  X:J1/J2  B:VOLTAR",
    keys_dropdown_footer: "A:OK   B:CANCELAR   CIMA/BAIXO:NAV",
    none: "(nenhuma)",
    flash_mouse_left: "Clique esquerdo",
    flash_mouse_right: "Clique direito",
    flash_space: "Espa\u{00C7}o",
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
    empty_title: "NENHUM JOGO",
    empty_l1: "COLOQUE ARQUIVOS .SWF EM",
    empty_l2: "SDMC:/FLASHNX/   OU   SDMC:/SWITCH/FLASHNX/",
    empty_l3: "DEPOIS REINICIE O FLASHNX.",
    empty_footer: "Y:IMPORTAR REMOTO   -:SAIR",
    list_footer: "L/R:ABAS  A:JOGAR  Y:ORDENAR  -:BUSCAR  +:OP\u{00C7}\u{00D5}ES  ZL/ZR:P\u{00C1}GINA",
    applet_title: "MODO APPLET",
    applet_notice: "INICIAR UM JOGO PRECISA DE TODA A MEM\u{00D3}RIA DO APP, QUE O MODO APPLET N\u{00C3}O TEM. NO HOMEBREW MENU, SEGURE R SOBRE UM T\u{00CD}TULO (OU USE UM FORWARDER) PARA INICIAR O FLASHNX COM TODA A MEM\u{00D3}RIA.",
    options_title: "OP\u{00C7}\u{00D5}ES",
    opt_keys: "CONTROLES",
    opt_rename: "RENOMEAR",
    opt_edit: "EDITAR",
    opt_delete: "EXCLUIR",
    opt_back: "VOLTAR",
    options_footer: "A:OK   B:VOLTAR",
    del_title: "EXCLUIR ?",
    del_l1: "O ARQUIVO .swf, OS SAVES (.sol),",
    del_l2: "OS CONTROLES E O ALIAS SER\u{00C3}O APAGADOS.",
    del_l3: "A\u{00C7}\u{00C3}O IRREVERS\u{00CD}VEL.",
    del_footer: "A: EXCLUIR     B: CANCELAR",
    dist_title: "IMPORTAR REMOTO",
    dist_add: "+ ADICIONAR UMA URL",
    dist_list_footer: "A:INICIAR   +:OP\u{00C7}\u{00D5}ES   Y:ORDENAR   -:BUSCAR   X:FLASHPOINT",
    dist_count: "{} URL(S)",
    dist_subtitle: "BAIXAR SWF DO ARCHIVE.ORG",
    dist_press_a: "PRESSIONE A PARA INSERIR UMA URL",
    dist_example1: "EXEMPLO: HTTPS://ARCHIVE.ORG/DETAILS/<ITEM-ID>",
    dist_example2: "OU SIMPLESMENTE <ITEM-ID>",
    dist_history: "HIST\u{00D3}RICO",
    dist_hint_zr: "ZR : CARREGAR ESTA URL DIRETAMENTE",
    dist_hint_a: "A  : INSERIR / EDITAR URL (TECLADO)",
    dist_hint_lr: "L / R : URL ANTERIOR / SEGUINTE",
    dist_footer_hist: "ZR:ABRIR  A:EDITAR  ZL:EXCLUIR  L/R:NAV  Y:LOCAL  -:SAIR",
    dist_footer_nohist: "A:INSERIR URL   Y:VOLTAR LOCAL   -:SAIR",
    files_title: "ARQUIVOS REMOTOS",
    files_filter: "FILTRO",
    files_footer: "A:BAIXAR   Y:ORDENAR   -:BUSCAR   L/R:P\u{00C1}GINA   B:VOLTAR",
    dl_title: "BAIXANDO",
    dl_footer: "B:CANCELAR",
    toast_dl_ok: "{} BAIXADO",
    toast_assets_missing: "DADOS DO JOGO AUSENTES: {}. O JOGO PODE N\u{00C3}O INICIAR.",
    err_title: "ERRO",
    err_footer: "A/B:OK",
    err_footer_fix: "A/B:OK   Y:CORRIGIR A URL",
    settings_title: "AJUSTES",
    set_keys: "CONTROLES PADR\u{00C3}O",
    set_language: "IDIOMA",
    set_quit: "SAIR",
    set_pseudo: "APELIDO",
    kbd_pseudo_guide: "Seu nome, ao lado dos perfis compartilhados",
    set_cursor_speed: "VELOCIDADE DO CURSOR",
    set_display_mode: "TELA",
    display_fit: "AJUSTAR",
    display_fill: "PREENCHER",
    display_stretch: "ESTICAR",
    set_screen_filter: "FILTRO",
    set_home_view: "VISTA INÍCIO",
    home_grid: "GRADE",
    home_list: "LISTA",
    home_strip: "FAIXA",
    home_shelf: "PRATELEIRA",
    set_game_prefs: "PREFER\u{00CA}NCIAS",
    prefs_title: "PADR\u{00D5}ES",
    filter_none: "NENHUM",
    filter_scanlines: "LINHAS",
    filter_crt: "CRT",
    show_cursor: "MOSTRAR CURSOR",
    cursor_shown: "VIS\u{00CD}VEL",
    cursor_hidden: "OCULTO",
    set_back: "VOLTAR",
    set_report_bug: "RELATAR UM BUG",
    tab_play: "JOGAR",
    tab_import: "IMPORTAR",
    tab_settings: "AJUSTES",
    set_covers: "CAPAS ONLINE",
    lbl_on: "ON",
    lbl_off: "OFF",
    opt_cover: "CAPA",
    opt_favorite: "FAVORITO",
    opt_unfavorite: "REMOVER FAVORITO",
    opt_share: "COMPARTILHAR CONTROLES",
    profile_shared_ok: "SEUS CONTROLES FORAM ENVIADOS. OBRIGADO POR AJUDAR A COMUNIDADE.",
    opt_apply: "APLICAR UM PERFIL",
    profile_title: "PERFIS DE CONTROLE",
    profile_footer: "A:APLICAR   B:VOLTAR   CIMA/BAIXO:NAV",
    profile_none: "AINDA SEM PERFIL PARA ESTE JOGO. COMPARTILHE O SEU!",
    profile_catalog_offline: "CAT\u{00C1}LOGO INDISPON\u{00CD}VEL. VERIFIQUE SUA CONEX\u{00C3}O.",
    profile_revert: "VOLTAR AOS MEUS CONTROLES",
    profile_applied_ok: "PERFIL APLICADO. SEUS CONTROLES ANTERIORES FORAM SALVOS.",
    profile_reverted_ok: "SEUS CONTROLES FORAM RESTAURADOS.",
    profile_share_confirm: "COMPARTILHAR SEUS CONTROLES PARA ESTE JOGO?",
    share_confirm_update: "ATUALIZA SEU PERFIL COMPARTILHADO:",
    touches_edit: "EDITAR CONTROLES",
    touches_footer: "A:ESCOLHER   B:VOLTAR   CIMA/BAIXO:NAV",
    touches_revert_default: "RESTAURAR CONTROLES PADR\u{00C3}O",
    profile_preview_title: "MEUS -> PERFIL",
    profile_preview_footer: "A:APLICAR   B:VOLTAR",
    profile_preview_none: "ESTE PERFIL N\u{00C3}O MUDA NENHUMA TECLA.",
    profile_active: "(ATIVO)",
    profile_share_dup: "J\u{00C1} EST\u{00C1} NO CAT\u{00C1}LOGO. MUDE UMA TECLA PARA COMPARTILHAR SUA VERS\u{00C3}O.",
    toast_already_imported: "{} J\u{00C1} EST\u{00C1} NA SUA BIBLIOTECA.",
    profile_del_confirm: "REMOVER SEU PERFIL COMPARTILHADO?",
    profile_del_ok: "SEU PERFIL COMPARTILHADO FOI REMOVIDO.",
    profile_del_not_mine: "NADA A REMOVER DESTE CONSOLE.",
    profile_del_hint: "X:REMOVER",
    revert_preview_title: "AGORA -> AP\u{00D3}S REVERTER",
    revert_preview_footer: "A:REVERTER   B:VOLTAR",
    cover_title: "ESCOLHER UMA CAPA",
    cover_footer: "A: ESCOLHER   -: BUSCAR   CIMA/BAIXO: MOVER   B: VOLTAR",
    cover_show_logos: "Y: LOGOS",
    cover_show_shots: "Y: CAPTURAS",
    cover_off_notice: "ATIVE AS CAPAS ONLINE NOS AJUSTES",
    cover_none: "SEM RESULTADOS",
    fp_title: "FLASHPOINT",
    fp_footer: "A:BAIXAR   X:BUSCAR   Y:ORDENAR   +:INFO   ZL+ZR:FILTRO {}   B:VOLTAR",
    fp_details_title: "DETALHES",
    fp_details_dev: "DESENVOLVEDOR",
    fp_details_publisher: "DISTRIBUIDORA",
    fp_details_date: "LAN\u{00C7}AMENTO",
    fp_details_size: "TAMANHO DO DOWNLOAD",
    fp_details_footer: "B:VOLTAR",
    sort_title: "ORDENAR POR",
    sort_footer: "A:ESCOLHER   X:INVERTER   B:VOLTAR",
    sort_alpha: "NOME",
    sort_recent: "ADICIONADO",
    sort_played: "MAIS JOGADOS",
    sort_size: "TAMANHO",
    played_label: "JOGADO",
    sort_recent_played: "JOGADO POR \u{00DA}LTIMO",
    sort_dev: "DESENVOLVEDOR",
    sort_source: "FONTE",
    sort_files: "N\u{00DA}MERO DE ARQUIVOS",
    fav_added: "ADICIONADO AOS FAVORITOS",
    fav_removed: "REMOVIDO DOS FAVORITOS",
    multifile: "MULTIARQUIVO",
    sort_dir_asc: "CRESCENTE",
    sort_dir_desc: "DECRESCENTE",
    settings_footer: "L/R:ABAS   A:OK",
    lang_title: "IDIOMA",
    lang_footer: "A:OK   B:CANCELAR",
    histdel_title: "EXCLUIR URL ?",
    url_info_type: "TIPO",
    url_type_swf: "ARQUIVO .SWF",
    url_type_list: "LISTA DE ARQUIVOS",
    url_info_files: "NO SD",
    url_info_added: "ADICIONADA EM",
    histdel_msg: "REMOVER ESTA URL DO HIST\u{00D3}RICO?",
    err_too_large: "RESPOSTA DO ARCHIVE.ORG MUITO GRANDE (>4 MB). ITEM GRANDE DEMAIS PARA ESTA VERS\u{00C3}O.",
    err_https: "FALHA NA TRANSFER\u{00CA}NCIA ({}). VERIFIQUE O WIFI E A URL.",
    err_offline: "SEM CONEX\u{00C3}O. VERIFIQUE O WIFI DO CONSOLE.",
    err_timeout: "O SERVIDOR N\u{00C3}O RESPONDEU A TEMPO. TENTE NOVAMENTE.",
    err_tls: "CONEX\u{00C3}O SEGURA RECUSADA. VERIFIQUE A DATA E A HORA DO CONSOLE: UM REL\u{00D3}GIO ERRADO QUEBRA O HTTPS.",
    err_response_big: "RESPOSTA MUITO GRANDE. TENTE UMA BUSCA MAIS PRECISA.",
    err_sd_write: "N\u{00C3}O FOI POSS\u{00CD}VEL ESCREVER NO CART\u{00C3}O SD. TALVEZ ESTEJA CHEIO.",
    err_http_status: "O SERVIDOR RECUSOU O PEDIDO (HTTP {}). TENTE MAIS TARDE.",
    err_not_found: "N\u{00C3}O ENCONTRADO (404). A URL OU O ID DO ITEM EST\u{00C1} ERRADO, OU FOI REMOVIDO.",
    err_json: "JSON DO ARCHIVE.ORG ILEG\u{00CD}VEL: {}",
    err_json_no_files: "JSON SEM O CAMPO \"files\"",
    err_dl_start: "N\u{00C3}O FOI POSS\u{00CD}VEL INICIAR O DOWNLOAD (C\u{00D3}DIGO {}).",
    err_dl_failed: "FALHA NO DOWNLOAD (C\u{00D3}DIGO {})",
    err_dl_cancelled: "DOWNLOAD CANCELADO PELO USU\u{00C1}RIO.",
    err_url_invalid: "URL INV\u{00C1}LIDA. ESPERADA UMA URL DO ARCHIVE.ORG TIPO https://archive.org/details/<id> OU SIMPLESMENTE <id>.",
    err_no_swf: "NENHUM ARQUIVO .SWF ENCONTRADO PARA ESTE JOGO.",
    err_dl_not_a_game: "O DOWNLOAD N\u{00C3}O RETORNOU UM ARQUIVO DE JOGO. VERIFIQUE O ENDERE\u{00C7}O.",
    err_fp_html_game: "ESTE JOGO INICIA POR UMA P\u{00C1}GINA HTML (FLASHVARS), AINDA N\u{00C3}O SUPORTADO.",
    kbd_url_header: "FlashNX - Importar remoto",
    kbd_url_guide: "URL do item do archive.org (ex: https://archive.org/download/your-game-id)",
    kbd_rename_header: "FlashNX - Renomear jogo",
    kbd_rename_guide: "Nome de exibi\u{00E7}\u{00E3}o (deixe vazio para voltar ao nome do arquivo)",
    kbd_search_header: "FlashNX - Buscar",
    kbd_search_guide: "Filtrar por nome de arquivo (vazio = mostrar tudo)",
    bug_pick_title: "RELATAR UM BUG",
    bug_pick_footer: "A:ESCOLHER   B:VOLTAR   CIMA/BAIXO:NAV",
    bug_no_games: "NENHUM JOGO PARA RELATAR. IMPORTE OU COLOQUE UM .SWF PRIMEIRO.",
    bug_ok_title: "OBRIGADO!",
    bug_ok_msg: "SEU RELATO FOI ENVIADO. OBRIGADO POR AJUDAR A MELHORAR O FLASHNX.",
    bug_fail_title: "FALHOU",
    kbd_bug_header: "FlashNX - Relatar um bug",
    kbd_bug_guide: "Abre uma issue P\u{00DA}BLICA no GitHub. Descreva o problema. Opcional: seu @usu\u{00E1}rio.",
    set_suggest: "FAZER UMA SUGEST\u{00C3}O",
    kbd_suggest_header: "FlashNX - Fazer uma sugest\u{00E3}o",
    kbd_suggest_guide: "Abre uma issue P\u{00DA}BLICA no GitHub. Sua ideia / sugest\u{00E3}o para o FlashNX.",
};

// Simplified Chinese (issue #41). CJK has no case, so unlike the Latin tables
// these are written verbatim; they render through the shared-font glyph atlas
// (backend/glyphs.rs), while the ASCII parts (button names, paths, URLs) still
// come from the 5x7 bitmap font (folded to uppercase). Button-hint footers keep
// ASCII ":" / spacing for parity with the other locales; prose uses Chinese
// punctuation. The kbd_* strings go to the Switch software keyboard (full
// Unicode), so they read as natural mixed Chinese + Latin.
const ZH: Strings = Strings {
    optimizing: "\u{4F18}\u{5316}\u{4E2D}",
    menu_resume: "继续",
    menu_keys: "按键",
    menu_restart: "重新开始",
    menu_quit: "退出",
    menu_cursor: "光标",
    pause_title: "暂停",
    pause_footer: "A:确定   B:取消   上/下:导航",
    keys_title: "按键",
    keys_footer: "A:编辑  L/R:模式  X:玩家1/2  B:返回",
    keys_dropdown_footer: "A:确定   B:取消   上/下:导航",
    none: "(无)",
    flash_mouse_left: "左键单击",
    flash_mouse_right: "右键单击",
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
    empty_l2: "SDMC:/FLASHNX/   或   SDMC:/SWITCH/FLASHNX/",
    empty_l3: "然后重启 FLASHNX。",
    empty_footer: "Y:在线导入   -:退出",
    list_footer: "L/R:标签  A:开始  Y:排序  -:搜索  +:选项  ZL/ZR:翻页",
    applet_title: "小程序模式",
    applet_notice: "启动游戏需要应用的全部内存，小程序模式没有。在自制软件菜单中，按住 R 选择标题（或使用转发器）以全内存启动 FLASHNX。",
    options_title: "选项",
    opt_keys: "按键",
    opt_rename: "重命名",
    opt_edit: "编辑",
    opt_delete: "删除",
    opt_back: "返回",
    options_footer: "A:确定   B:返回",
    del_title: "删除？",
    del_l1: ".swf 文件、存档 (.sol)、",
    del_l2: "按键和别名都将被删除。",
    del_l3: "此操作无法撤销。",
    del_footer: "A: 删除     B: 取消",
    dist_title: "在线导入",
    dist_add: "+ 添加网址",
    dist_list_footer: "A:启动   +:选项   Y:排序   -:搜索   X:FLASHPOINT",
    dist_count: "{} 个网址",
    dist_subtitle: "从 ARCHIVE.ORG 下载 SWF",
    dist_press_a: "按 A 输入网址",
    dist_example1: "示例: HTTPS://ARCHIVE.ORG/DETAILS/<ITEM-ID>",
    dist_example2: "或直接输入 <ITEM-ID>",
    dist_history: "历史记录",
    dist_hint_zr: "ZR : 直接加载此网址",
    dist_hint_a: "A  : 输入 / 编辑网址（键盘）",
    dist_hint_lr: "L / R : 上一个 / 下一个网址",
    dist_footer_hist: "ZR:打开  A:编辑  ZL:删除  L/R:导航  Y:本地  -:退出",
    dist_footer_nohist: "A:输入网址   Y:返回本地   -:退出",
    files_title: "远程文件",
    files_filter: "筛选",
    files_footer: "A:下载   Y:排序   -:搜索   L/R:翻页   B:返回",
    dl_title: "下载中",
    dl_footer: "B:取消",
    toast_dl_ok: "{} 已下载",
    toast_assets_missing: "缺少游戏数据：{}。该游戏可能无法启动。",
    err_title: "错误",
    err_footer: "A/B:确定",
    err_footer_fix: "A/B:确定   Y:修正网址",
    settings_title: "设置",
    set_keys: "默认按键",
    set_language: "语言",
    set_quit: "退出",
    set_pseudo: "昵称",
    kbd_pseudo_guide: "显示在你分享的配置旁边的名字",
    set_cursor_speed: "光标速度",
    set_display_mode: "显示",
    display_fit: "适应",
    display_fill: "填满",
    display_stretch: "拉伸",
    set_screen_filter: "滤镜",
    set_home_view: "主页视图",
    home_grid: "网格",
    home_list: "列表",
    home_strip: "横条",
    home_shelf: "书架",
    set_game_prefs: "游戏默认",
    prefs_title: "默认设置",
    filter_none: "无",
    filter_scanlines: "扫描线",
    filter_crt: "CRT",
    show_cursor: "显示光标",
    cursor_shown: "显示",
    cursor_hidden: "隐藏",
    set_back: "返回",
    set_report_bug: "报告问题",
    tab_play: "游戏",
    tab_import: "导入",
    tab_settings: "设置",
    set_covers: "在线封面",
    lbl_on: "开",
    lbl_off: "关",
    opt_cover: "封面",
    opt_favorite: "收藏",
    opt_unfavorite: "取消收藏",
    opt_share: "分享按键",
    profile_shared_ok: "你的按键已发送。感谢你为社区做出的贡献。",
    opt_apply: "应用配置",
    profile_title: "按键配置",
    profile_footer: "A:应用   B:返回   上/下:导航",
    profile_none: "这个游戏还没有配置。分享你的吧！",
    profile_catalog_offline: "目录不可用。请检查网络连接。",
    profile_revert: "恢复我的按键",
    profile_applied_ok: "配置已应用。你之前的按键已保存。",
    profile_reverted_ok: "你的按键已恢复。",
    profile_share_confirm: "分享你为这个游戏设置的按键？",
    share_confirm_update: "更新你已分享的配置：",
    touches_edit: "编辑按键",
    touches_footer: "A:选择   B:返回   上/下:导航",
    touches_revert_default: "恢复默认按键",
    profile_preview_title: "我的 -> 配置",
    profile_preview_footer: "A:应用   B:返回",
    profile_preview_none: "此配置不会改变任何按键。",
    profile_active: "(使用中)",
    profile_share_dup: "已在目录中。修改一个按键即可分享你的版本。",
    toast_already_imported: "{} 已经在你的库中。",
    profile_del_confirm: "删除你分享的配置？",
    profile_del_ok: "你分享的配置已从目录中删除。",
    profile_del_not_mine: "这台主机没有可删除的内容。",
    profile_del_hint: "X:删除",
    revert_preview_title: "当前 -> 还原后",
    revert_preview_footer: "A:还原   B:返回",
    cover_title: "选择封面",
    cover_footer: "A: 选择   -: 搜索   上/下: 移动   B: 返回",
    cover_show_logos: "Y: 标志",
    cover_show_shots: "Y: 截图",
    cover_off_notice: "在设置中启用在线封面",
    cover_none: "无结果",
    fp_title: "FLASHPOINT",
    fp_footer: "A:下载   X:搜索   Y:排序   +:信息   ZL+ZR:筛选 {}   B:返回",
    fp_details_title: "详情",
    fp_details_dev: "开发者",
    fp_details_publisher: "发行商",
    fp_details_date: "发行日期",
    fp_details_size: "下载大小",
    fp_details_footer: "B:返回",
    sort_title: "排序方式",
    sort_footer: "A:选择   X:反向   B:返回",
    sort_alpha: "名称",
    sort_recent: "添加时间",
    sort_played: "最常玩",
    sort_size: "大小",
    played_label: "已玩",
    sort_recent_played: "最近玩过",
    sort_dev: "开发者",
    sort_source: "来源",
    sort_files: "文件数量",
    fav_added: "已加入收藏",
    fav_removed: "已取消收藏",
    multifile: "多文件",
    sort_dir_asc: "升序",
    sort_dir_desc: "降序",
    settings_footer: "L/R:标签   A:确定",
    lang_title: "语言",
    lang_footer: "A:确定   B:取消",
    histdel_title: "删除网址？",
    url_info_type: "类型",
    url_type_swf: "单个 .SWF",
    url_type_list: "文件列表",
    url_info_files: "已在 SD 卡",
    url_info_added: "添加于",
    histdel_msg: "从历史记录中移除此网址？",
    err_too_large: "ARCHIVE.ORG 响应过大 (>4 MB)。此项目对当前版本来说太大了。",
    err_https: "传输失败 ({})。请检查 WIFI 和网址。",
    err_offline: "无网络连接。请检查主机的 WIFI。",
    err_timeout: "服务器未及时响应。请重试。",
    err_tls: "安全连接被拒绝。请检查主机的日期和时间：时钟错误会导致 HTTPS 失败。",
    err_response_big: "响应过大。请使用更精确的搜索。",
    err_sd_write: "无法写入 SD 卡。可能已满或被写保护。",
    err_http_status: "服务器拒绝了请求 (HTTP {})。请稍后重试。",
    err_not_found: "未找到 (404)。网址或条目 ID 有误，或已被删除。",
    err_json: "无法读取 ARCHIVE.ORG 的 JSON: {}",
    err_json_no_files: "JSON 中没有 \"files\" 字段",
    err_dl_start: "无法开始下载（代码 {}）。",
    err_dl_failed: "下载失败（代码 {}）",
    err_dl_cancelled: "下载已被用户取消。",
    err_url_invalid: "网址无效。应为 ARCHIVE.ORG 网址，例如 https://archive.org/details/<id>，或直接输入 <id>。",
    err_no_swf: "未找到此游戏的 .SWF 文件。",
    err_dl_not_a_game: "下载返回的不是游戏文件。请检查地址。",
    err_fp_html_game: "此游戏通过 HTML 页面（FLASHVARS）启动，暂不支持。",
    kbd_url_header: "FlashNX - 在线导入",
    kbd_url_guide: "archive.org 项目网址（例如 https://archive.org/download/your-game-id）",
    kbd_rename_header: "FlashNX - 重命名游戏",
    kbd_rename_guide: "显示名称（留空以恢复文件名）",
    kbd_search_header: "FlashNX - 搜索",
    kbd_search_guide: "按文件名筛选（留空 = 显示全部）",
    bug_pick_title: "报告问题",
    bug_pick_footer: "A:选择   B:返回   上/下:导航",
    bug_no_games: "暂无可报告的游戏。请先导入或放入一个 .SWF。",
    bug_ok_title: "谢谢！",
    bug_ok_msg: "你的报告已发送。感谢你帮助改进 FLASHNX。",
    bug_fail_title: "失败",
    kbd_bug_header: "FlashNX - 报告问题",
    kbd_bug_guide: "将打开一个公开的 GitHub issue。请描述问题。可选：你的 @用户名。",
    set_suggest: "提出建议",
    kbd_suggest_header: "FlashNX - 提出建议",
    kbd_suggest_guide: "将打开一个公开的 GitHub issue。你对 FlashNX 的想法 / 建议。",
};

// Turkish. Drawn strings are UPPERCASE with only the caps our 5x7 font carries
// (Ç Ö Ü + the added Ğ/Ş); the dotted capital I is
// rendered as a plain ASCII "I" (standard all-caps relaxation, avoids a glyph).
// The `kbd_*` guides go to the Switch software keyboard, which renders full
// Turkish diacritics, so those keep proper lowercase spelling.
const TR: Strings = Strings {
    optimizing: "OPTIMIZE EDILIYOR",
    menu_resume: "DEVAM ET",
    menu_keys: "KONTROLLER",
    menu_restart: "YEN\u{0130}DEN BA\u{015E}LAT",
    menu_quit: "\u{00C7}IKI\u{015E}",
    menu_cursor: "IMLE\u{00C7}",
    pause_title: "DURAKLATILDI",
    pause_footer: "A:TAMAM   B:IPTAL   YUKARI/A\u{015E}A\u{011E}I",
    keys_title: "KONTROLLER",
    keys_footer: "A:D\u{00DC}ZENLE  L/R:MOD  X:O1/O2  B:GER\u{0130}",
    keys_dropdown_footer: "A:TAMAM   B:IPTAL   YUKARI/A\u{015E}A\u{011E}I",
    none: "(yok)",
    flash_mouse_left: "Sol tik",
    flash_mouse_right: "Sa\u{011E} tik",
    flash_space: "Bo\u{015E}luk",
    flash_enter: "Enter",
    flash_escape: "Esc",
    flash_shift: "Shift",
    flash_control: "Ctrl",
    flash_alt: "Alt",
    flash_tab: "Tab",
    flash_backspace: "Geri sil",
    flash_up: "Yukari",
    flash_down: "A\u{015E}a\u{011E}i",
    flash_left: "Sol",
    flash_right: "Sa\u{011E}",
    empty_title: "OYUN YOK",
    empty_l1: ".SWF DOSYALARINI \u{015E}URAYA KOYUN",
    empty_l2: "SDMC:/FLASHNX/   VEYA   SDMC:/SWITCH/FLASHNX/",
    empty_l3: "SONRA FLASHNX'I YEN\u{0130}DEN BA\u{015E}LATIN.",
    empty_footer: "Y:UZAKTAN AKTAR   -:\u{00C7}IKI\u{015E}",
    list_footer: "L/R:SEKMELER  A:OYNA  Y:SIRALA  -:ARA  +:SE\u{00C7}ENEK  ZL/ZR:SAYFA",
    applet_title: "APPLET MODU",
    applet_notice: "B\u{0130}R OYUN BA\u{015E}LATMAK UYGULAMANIN T\u{00DC}M BELLE\u{011E}\u{0130}N\u{0130} GEREKT\u{0130}R\u{0130}R, APPLET MODUNDA BU BELLEK YOKTUR. HOMEBREW MEN\u{00DC}S\u{00DC}NDE B\u{0130}R OYUNA (VEYA FORWARDER'A) R TU\u{015E}UNU BASILI TUTARAK FLASHNX'I TAM BELLEKLE BA\u{015E}LATIN.",
    options_title: "SE\u{00C7}ENEKLER",
    opt_keys: "KONTROLLER",
    opt_rename: "YEN\u{0130}DEN ADLANDIR",
    opt_edit: "D\u{00DC}ZENLE",
    opt_delete: "S\u{0130}L",
    opt_back: "GER\u{0130}",
    options_footer: "A:TAMAM   B:GER\u{0130}",
    del_title: "S\u{0130}L\u{0130}NS\u{0130}N M\u{0130}?",
    del_l1: ".swf DOSYASI, KAYITLAR (.sol),",
    del_l2: "KONTROLLER VE TAKMA AD S\u{0130}L\u{0130}NECEK.",
    del_l3: "BU GER\u{0130} ALINAMAZ.",
    del_footer: "A: S\u{0130}L     B: IPTAL",
    dist_title: "UZAKTAN AKTAR",
    dist_add: "+ URL EKLE",
    dist_list_footer: "A:BA\u{015E}LAT   +:SE\u{00C7}ENEK   Y:SIRALA   -:ARA   X:FLASHPOINT",
    dist_count: "{} URL",
    dist_subtitle: "ARCHIVE.ORG'DAN SWF IND\u{0130}R",
    dist_press_a: "URL G\u{0130}RMEK I\u{00C7}IN A'YA BASIN",
    dist_example1: "\u{00D6}RNEK: HTTPS://ARCHIVE.ORG/DETAILS/<ITEM-ID>",
    dist_example2: "VEYA SADECE <ITEM-ID>",
    dist_history: "GE\u{00C7}M\u{0130}\u{015E}",
    dist_hint_zr: "ZR : BU URL'Y\u{0130} DO\u{011E}RUDAN Y\u{00DC}KLE",
    dist_hint_a: "A  : URL G\u{0130}R / D\u{00DC}ZENLE (KLAVYE)",
    dist_hint_lr: "L / R : \u{00D6}NCEK\u{0130} / SONRAK\u{0130} URL",
    dist_footer_hist: "ZR:A\u{00C7}  A:D\u{00DC}ZENLE  ZL:S\u{0130}L  L/R:GEZ  Y:YEREL  -:\u{00C7}IK",
    dist_footer_nohist: "A:URL G\u{0130}R   Y:YEREL   -:\u{00C7}IKI\u{015E}",
    files_title: "UZAK DOSYALAR",
    files_filter: "F\u{0130}LTRE",
    files_footer: "A:IND\u{0130}R   Y:SIRALA   -:ARA   L/R:SAYFA   B:GER\u{0130}",
    dl_title: "IND\u{0130}R\u{0130}L\u{0130}YOR",
    dl_footer: "B:IPTAL",
    toast_dl_ok: "{} \u{0130}ND\u{0130}R\u{0130}LD\u{0130}",
    toast_assets_missing: "OYUN VER\u{0130}LER\u{0130} EKS\u{0130}K: {}. OYUN BA\u{015E}LAMAYAB\u{0130}L\u{0130}R.",
    err_title: "HATA",
    err_footer: "A/B:TAMAM",
    err_footer_fix: "A/B:TAMAM   Y:URL'Y\u{0130} D\u{00DC}ZELT",
    settings_title: "AYARLAR",
    set_keys: "VARSAYILAN KONTROLLER",
    set_language: "D\u{0130}L",
    set_quit: "\u{00C7}IKI\u{015E}",
    set_pseudo: "TAKMA AD",
    kbd_pseudo_guide: "Payla\u{015F}t\u{0131}\u{011F}\u{0131}n profillerde g\u{00F6}r\u{00Fc}nen ad\u{0131}n",
    set_cursor_speed: "IMLE\u{00C7} HIZI",
    set_display_mode: "EKRAN",
    display_fit: "SI\u{011E}DIR",
    display_fill: "DOLDUR",
    display_stretch: "GER",
    set_screen_filter: "F\u{0130}LTRE",
    set_home_view: "ANA EKRAN",
    home_grid: "IZGARA",
    home_list: "LİSTE",
    home_strip: "SERİT",
    home_shelf: "RAF",
    set_game_prefs: "OYUN VARSAYILANLARI",
    prefs_title: "VARSAYILANLAR",
    filter_none: "YOK",
    filter_scanlines: "\u{00C7}\u{0130}ZG\u{0130}LER",
    filter_crt: "CRT",
    show_cursor: "IMLEC\u{0130} G\u{00D6}STER",
    cursor_shown: "G\u{00D6}R\u{00DC}N\u{00DC}R",
    cursor_hidden: "G\u{0130}ZL\u{0130}",
    set_back: "GER\u{0130}",
    set_report_bug: "HATA B\u{0130}LD\u{0130}R",
    tab_play: "OYNA",
    tab_import: "AKTAR",
    tab_settings: "AYARLAR",
    set_covers: "\u{00C7}EVR\u{0130}M\u{0130}\u{00C7}\u{0130} KAPAKLAR",
    lbl_on: "A\u{00C7}IK",
    lbl_off: "KAPALI",
    opt_cover: "KAPAK",
    opt_favorite: "FAVOR\u{0130}",
    opt_unfavorite: "FAVOR\u{0130}DEN \u{00C7}IKAR",
    opt_share: "KONTROLLER\u{0130} PAYLA\u{015E}",
    profile_shared_ok: "KONTROLLER\u{0130}N G\u{00D6}NDER\u{0130}LD\u{0130}. TOPLULU\u{011E}A YARDIM I\u{00C7}IN TE\u{015E}EKK\u{00DC}RLER.",
    opt_apply: "PROF\u{0130}L UYGULA",
    profile_title: "KONTROL PROF\u{0130}LLER\u{0130}",
    profile_footer: "A:UYGULA   B:GER\u{0130}   YUK/A\u{015E}A:GEZ",
    profile_none: "BU OYUN I\u{00C7}IN HEN\u{00DC}Z PROF\u{0130}L YOK. KEND\u{0130}N\u{0130}NK\u{0130}N\u{0130} PAYLA\u{015E}!",
    profile_catalog_offline: "KATALOG KULLANILAMIYOR. BA\u{011E}LANTINI KONTROL ET.",
    profile_revert: "KEND\u{0130} KONTROLLER\u{0130}ME D\u{00D6}N",
    profile_applied_ok: "PROF\u{0130}L UYGULANDI. \u{00D6}NCEK\u{0130} KONTROLLER\u{0130}N KAYDED\u{0130}LD\u{0130}.",
    profile_reverted_ok: "KONTROLLER\u{0130}N GER\u{0130} Y\u{00DC}KLEND\u{0130}.",
    profile_share_confirm: "BU OYUN I\u{00C7}IN KONTROLLER\u{0130}N PAYLA\u{015E}ILSIN MI?",
    share_confirm_update: "PAYLA\u{015E}ILAN PROF\u{0130}L\u{0130}N\u{0130} G\u{00DC}NCELLER:",
    touches_edit: "KONTROLLER\u{0130} D\u{00DC}ZENLE",
    touches_footer: "A:SE\u{00C7}   B:GER\u{0130}   YUK/A\u{015E}A:GEZ",
    touches_revert_default: "VARSAYILAN KONTROLLERE D\u{00D6}N",
    profile_preview_title: "BEN\u{0130}MK\u{0130} -> PROF\u{0130}L",
    profile_preview_footer: "A:UYGULA   B:GER\u{0130}",
    profile_preview_none: "BU PROF\u{0130}L H\u{0130}\u{00C7}B\u{0130}R TU\u{015E}UNU DE\u{011E}\u{0130}\u{015E}T\u{0130}RMEZ.",
    profile_active: "(ETK\u{0130}N)",
    profile_share_dup: "ZATEN KATALOGDA. KEND\u{0130} S\u{00DC}R\u{00DC}M\u{00DC}N\u{00DC} PAYLA\u{015E}MAK I\u{00C7}IN B\u{0130}R TU\u{015E} DE\u{011E}\u{0130}\u{015E}T\u{0130}R.",
    toast_already_imported: "{} ZATEN K\u{00DC}T\u{00DC}PHANENDE.",
    profile_del_confirm: "PAYLA\u{015E}ILAN PROF\u{0130}L\u{0130}N S\u{0130}L\u{0130}NS\u{0130}N M\u{0130}?",
    profile_del_ok: "PAYLA\u{015E}ILAN PROF\u{0130}L\u{0130}N S\u{0130}L\u{0130}ND\u{0130}.",
    profile_del_not_mine: "S\u{0130}L\u{0130}NECEK B\u{0130}R \u{015E}EY YOK: BU KONSOLDAN PAYLA\u{015E}ILMADI.",
    profile_del_hint: "X:S\u{0130}L",
    revert_preview_title: "\u{015E}\u{0130}MD\u{0130} -> GER\u{0130} ALINCA",
    revert_preview_footer: "A:GER\u{0130} AL   B:GER\u{0130}",
    cover_title: "KAPAK SE\u{00C7}",
    cover_footer: "A:SE\u{00C7}   -:ARA   YUK/A\u{015E}A:GEZ   B:GER\u{0130}",
    cover_show_logos: "Y: LOGOLAR",
    cover_show_shots: "Y: EKRAN İMGELERİ",
    cover_off_notice: "AYARLARDAN \u{00C7}EVR\u{0130}M\u{0130}\u{00C7}\u{0130} KAPAKLARI A\u{00C7}IN",
    cover_none: "SONU\u{00C7} YOK",
    fp_title: "FLASHPOINT",
    fp_footer: "A:IND\u{0130}R  X:ARA  Y:SIRALA  +:B\u{0130}LG\u{0130}  ZL+ZR:F\u{0130}LTRE {}  B:GER\u{0130}",
    fp_details_title: "AYRINTILAR",
    fp_details_dev: "GEL\u{0130}\u{015E}T\u{0130}R\u{0130}C\u{0130}",
    fp_details_publisher: "YAYINCI",
    fp_details_date: "\u{00C7}IKI\u{015E} TAR\u{0130}H\u{0130}",
    fp_details_size: "IND\u{0130}RME BOYUTU",
    fp_details_footer: "B:GER\u{0130}",
    sort_title: "SIRALAMA",
    sort_footer: "A:SE\u{00C7}   X:TERS   B:GER\u{0130}",
    sort_alpha: "AD",
    sort_recent: "EKLENME",
    sort_played: "EN \u{00C7}OK OYNANAN",
    sort_size: "BOYUT",
    played_label: "OYNANDI",
    sort_recent_played: "SON OYNANAN",
    sort_dev: "GEL\u{0130}\u{015E}T\u{0130}R\u{0130}C\u{0130}",
    sort_source: "KAYNAK",
    sort_files: "DOSYA SAYISI",
    fav_added: "FAVOR\u{0130}LERE EKLEND\u{0130}",
    fav_removed: "FAVOR\u{0130}LERDEN \u{00C7}IKARILDI",
    multifile: "\u{00C7}OK DOSYALI",
    sort_dir_asc: "ARTAN",
    sort_dir_desc: "AZALAN",
    settings_footer: "L/R:SEKMELER   A:TAMAM",
    lang_title: "D\u{0130}L",
    lang_footer: "A:TAMAM   B:IPTAL",
    histdel_title: "URL S\u{0130}L\u{0130}NS\u{0130}N M\u{0130}?",
    url_info_type: "T\u{00DC}R",
    url_type_swf: "TEK .SWF",
    url_type_list: "DOSYA L\u{0130}STES\u{0130}",
    url_info_files: "SD KARTTA",
    url_info_added: "EKLEND\u{0130}",
    histdel_msg: "BU URL GE\u{00C7}M\u{0130}\u{015E}TEN S\u{0130}L\u{0130}NS\u{0130}N M\u{0130}?",
    err_too_large: "ARCHIVE.ORG YANITI \u{00C7}OK B\u{00DC}Y\u{00DC}K (>4 MB). BU S\u{00DC}R\u{00DC}M I\u{00C7}IN \u{00C7}OK B\u{00DC}Y\u{00DC}K.",
    err_https: "AKTARIM BA\u{015E}ARISIZ ({}). WIFI VE URL'Y\u{0130} KONTROL ED\u{0130}N.",
    err_offline: "BA\u{011E}LANTI YOK. KONSOLUN WIFI BA\u{011E}LANTISINI KONTROL ED\u{0130}N.",
    err_timeout: "SUNUCU ZAMANINDA YANIT VERMED\u{0130}. TEKRAR DENEY\u{0130}N.",
    err_tls: "G\u{00DC}VENL\u{0130} BA\u{011E}LANTI REDDED\u{0130}LD\u{0130}. KONSOLUN TAR\u{0130}H VE SAAT\u{0130}N\u{0130} KONTROL ED\u{0130}N: YANLI\u{015E} SAAT HTTPS'\u{0130} BOZAR.",
    err_response_big: "YANIT \u{00C7}OK B\u{00DC}Y\u{00DC}K. DAHA DAR B\u{0130}R ARAMA DENEY\u{0130}N.",
    err_sd_write: "SD KARTA YAZILAMADI. DOLU OLAB\u{0130}L\u{0130}R.",
    err_http_status: "SUNUCU \u{0130}STE\u{011E}\u{0130} REDDETT\u{0130} (HTTP {}). DAHA SONRA DENEY\u{0130}N.",
    err_not_found: "BULUNAMADI (404). URL VEYA \u{00D6}\u{011E}E K\u{0130}ML\u{0130}\u{011E}\u{0130} YANLI\u{015E} YA DA KALDIRILMI\u{015E}.",
    err_json: "OKUNAMAYAN ARCHIVE.ORG JSON: {}",
    err_json_no_files: "JSON'DA \"files\" ALANI YOK",
    err_dl_start: "IND\u{0130}RME BA\u{015E}LATILAMADI (KOD {}).",
    err_dl_failed: "IND\u{0130}RME BA\u{015E}ARISIZ (KOD {})",
    err_dl_cancelled: "IND\u{0130}RME KULLANICI TARAFINDAN IPTAL ED\u{0130}LD\u{0130}.",
    err_url_invalid: "GE\u{00C7}ERS\u{0130}Z URL. https://archive.org/details/<id> G\u{0130}B\u{0130} B\u{0130}R URL VEYA SADECE <id> BEKLEN\u{0130}YOR.",
    err_no_swf: "BU OYUN \u{0130}\u{00C7}\u{0130}N .SWF DOSYASI BULUNAMADI.",
    err_dl_not_a_game: "\u{0130}ND\u{0130}RME B\u{0130}R OYUN DOSYASI D\u{00D6}ND\u{00DC}RMED\u{0130}. ADRES\u{0130} KONTROL ED\u{0130}N.",
    err_fp_html_game: "BU OYUN B\u{0130}R HTML SAYFASINDAN (FLASHVARS) BA\u{015E}LAR, HEN\u{00DC}Z DESTEKLENM\u{0130}YOR.",
    kbd_url_header: "FlashNX - Uzaktan i\u{00E7}e aktarma",
    kbd_url_guide: "archive.org \u{00F6}\u{011F}e URL'si (\u{00F6}rn. https://archive.org/download/oyun-kimligi)",
    kbd_rename_header: "FlashNX - Oyunu yeniden adland\u{0131}r",
    kbd_rename_guide: "G\u{00F6}r\u{00FC}nen ad (dosya ad\u{0131}na d\u{00F6}nmek i\u{00E7}in bo\u{015F} b\u{0131}rak\u{0131}n)",
    kbd_search_header: "FlashNX - Ara",
    kbd_search_guide: "Dosya ad\u{0131}na g\u{00F6}re filtrele (bo\u{015F} = hepsini g\u{00F6}ster)",
    bug_pick_title: "HATA B\u{0130}LD\u{0130}R",
    bug_pick_footer: "A:SE\u{00C7}   B:GER\u{0130}   YUK/A\u{015E}A:GEZ",
    bug_no_games: "B\u{0130}LD\u{0130}R\u{0130}LECEK OYUN YOK. \u{00D6}NCE B\u{0130}R .SWF AKTARIN.",
    bug_ok_title: "TE\u{015E}EKK\u{00DC}RLER!",
    bug_ok_msg: "RAPORUN G\u{00D6}NDER\u{0130}LD\u{0130}. FLASHNX'I GEL\u{0130}\u{015E}T\u{0130}RMEYE YARDIM I\u{00C7}IN TE\u{015E}EKK\u{00DC}RLER.",
    bug_fail_title: "BA\u{015E}ARISIZ",
    kbd_bug_header: "FlashNX - Hata bildir",
    kbd_bug_guide: "Herkese a\u{00E7}\u{0131}k bir GitHub konusu a\u{00E7}ar. Sorunu anlat. \u{0130}ste\u{011F}e ba\u{011F}l\u{0131}: @kullan\u{0131}c\u{0131} ad\u{0131}n.",
    set_suggest: "\u{00D6}NER\u{0130}DE BULUN",
    kbd_suggest_header: "FlashNX - \u{00D6}neride bulun",
    kbd_suggest_guide: "Herkese a\u{00E7}\u{0131}k bir GitHub konusu a\u{00E7}ar. FlashNX i\u{00E7}in fikrin / \u{00F6}zellik iste\u{011F}in.",
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
    let lang = if lang.needs_cjk() && !crate::backend::glyphs::cjk_possible() {
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

/// JOUER tab layout: 0 = cover grid (the default since v1.2.0), 1 = title list
/// with the selected game's cover beside it (issue #52), 2 = covers scrolling
/// sideways with the selected one shown large. Persisted in settings.json.
///
/// A view, not a filter: the same games, the same order, the same actions. Which
/// one suits depends on the library and on the covers in it, so it stays a
/// choice. The grid remains the default and nothing about it changes.
static HOME_VIEW: AtomicU8 = AtomicU8::new(0);

/// Number of layouts, for cycling the REGLAGES row.
pub const HOME_VIEW_COUNT: u8 = 4;

pub fn home_view() -> u8 {
    HOME_VIEW.load(Ordering::Relaxed) % HOME_VIEW_COUNT
}

pub fn set_home_view(v: u8) {
    HOME_VIEW.store(v % HOME_VIEW_COUNT, Ordering::Relaxed);
}

/// Display name of a layout, for the REGLAGES row.
pub fn home_view_label(v: u8) -> &'static str {
    match v % HOME_VIEW_COUNT {
        1 => s().home_list,
        2 => s().home_strip,
        3 => s().home_shelf,
        _ => s().home_grid,
    }
}

pub fn default_display_mode() -> u8 {
    DEFAULT_DISPLAY_MODE.load(Ordering::Relaxed)
}

pub fn set_default_display_mode(v: u8) {
    DEFAULT_DISPLAY_MODE.store(v, Ordering::Relaxed);
}

pub fn default_screen_filter() -> u8 {
    DEFAULT_SCREEN_FILTER.load(Ordering::Relaxed)
}

pub fn set_default_screen_filter(v: u8) {
    DEFAULT_SCREEN_FILTER.store(v, Ordering::Relaxed);
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

/// "{n} GAME(S)" under the JOUER banner. Same per-language agreement rules as
/// `files_found` (see there for why Russian/Chinese use a count phrasing).
pub fn games_count(n: usize) -> std::string::String {
    match current() {
        Lang::En if n == 1 => "1 GAME".into(),
        Lang::En => std::format!("{} GAMES", n),
        // French: 0 and 1 are singular.
        Lang::Fr if n <= 1 => std::format!("{} JEU", n),
        Lang::Fr => std::format!("{} JEUX", n),
        Lang::Es if n == 1 => "1 JUEGO".into(),
        Lang::Es => std::format!("{} JUEGOS", n),
        Lang::Ru => std::format!("ИГР: {}", n),
        // German: 0 takes the plural ("0 Spiele").
        Lang::De if n == 1 => "1 SPIEL".into(),
        Lang::De => std::format!("{} SPIELE", n),
        Lang::It if n == 1 => "1 GIOCO".into(),
        Lang::It => std::format!("{} GIOCHI", n),
        Lang::Pt if n == 1 => "1 JOGO".into(),
        Lang::Pt => std::format!("{} JOGOS", n),
        Lang::Zh => std::format!("\u{6E38}\u{620F}: {}", n), // 游戏: {}
        // Turkish has no plural after a number (the numeral already marks it).
        Lang::Tr => std::format!("{} OYUN", n),
    }
}

/// "{n} .SWF FILE(S) FOUND" with real singular/plural agreement per language
/// (no literal "(s)"). Russian uses a count-style phrasing that sidesteps its
/// 3-form agreement.
pub fn files_found(n: usize) -> std::string::String {
    match current() {
        Lang::En if n == 1 => "1 .SWF FILE FOUND".into(),
        Lang::En => std::format!("{} .SWF FILES FOUND", n),
        // French: 0 and 1 are singular.
        Lang::Fr if n <= 1 => std::format!("{} FICHIER .SWF TROUV\u{00C9}", n),
        Lang::Fr => std::format!("{} FICHIERS .SWF TROUV\u{00C9}S", n),
        Lang::Es if n == 1 => "1 ARCHIVO .SWF ENCONTRADO".into(),
        Lang::Es => std::format!("{} ARCHIVOS .SWF ENCONTRADOS", n),
        Lang::Ru => std::format!("НАЙДЕНО ФАЙЛОВ .SWF: {}", n),
        // German: 0 takes the plural ("0 Dateien").
        Lang::De if n == 1 => "1 .SWF-DATEI GEFUNDEN".into(),
        Lang::De => std::format!("{} .SWF-DATEIEN GEFUNDEN", n),
        Lang::It if n == 1 => "1 FILE .SWF TROVATO".into(),
        Lang::It => std::format!("{} FILE .SWF TROVATI", n),
        Lang::Pt if n == 1 => "1 ARQUIVO .SWF ENCONTRADO".into(),
        Lang::Pt => std::format!("{} ARQUIVOS .SWF ENCONTRADOS", n),
        // Chinese has no plural agreement: a count phrasing reads naturally.
        Lang::Zh => std::format!("\u{627E}\u{5230} .SWF \u{6587}\u{4EF6}: {}", n), // 找到 .SWF 文件: {}
        // Turkish has no plural after a number (the numeral already marks it).
        Lang::Tr => std::format!("{} .SWF DOSYASI BULUNDU", n),
    }
}

/// Substitute a single `{}` placeholder in one of the error templates.
fn fill(template: &str, arg: &str) -> std::string::String {
    template.replacen("{}", arg, 1)
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
    fill(s().err_http_status, &code.to_string())
}

// ── Persistence + boot init ─────────────────────────────────────────────

const SETTINGS_ROOTS: &[&str] = &["sdmc:/flashnx", "sdmc:/ruffle"];

fn settings_read_path() -> Option<std::string::String> {
    for root in SETTINGS_ROOTS {
        let p = std::format!("{}/settings.json", root);
        if std::path::Path::new(&p).exists() {
            return Some(p);
        }
    }
    None
}

fn settings_write_path() -> std::string::String {
    std::format!("{}/settings.json", SETTINGS_ROOTS[0])
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

/// Write the full settings file. Everything persists together so saving one
/// setting never drops another.
fn write_settings(lang: Lang, covers: bool) -> bool {
    let path = settings_write_path();
    let json = std::format!(
        "{{\n    \"language\": \"{}\",\n    \"covers_online\": {},\n    \"display_mode\": {},\n    \"screen_filter\": {},\n    \"home_view\": {}\n}}\n",
        lang.code(),
        covers,
        default_display_mode(),
        default_screen_filter(),
        home_view(),
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
            if let Some(v) = parse_u8_setting(&txt, "screen_filter", 3) {
                set_default_screen_filter(v);
            }
            if let Some(v) = parse_u8_setting(&txt, "home_view", HOME_VIEW_COUNT) {
                set_home_view(v);
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
