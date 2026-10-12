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
    /// Pause menu: the row that opens the screen sub-panel (display mode,
    /// rotation, filter). Also that panel's own title.
    pub menu_screen: &'static str,
    pub menu_restart: &'static str,
    pub menu_quit: &'static str,
    pub menu_cursor: &'static str,
    pub pause_title: &'static str,
    pub pause_footer: &'static str,
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
    /// OPTIONS > move this game onto a shelf (issue #68).
    pub opt_folder: &'static str,
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
    /// Label naming the open folder in the home sub-line, in the slot the
    /// active filter uses (issue #68). Shown as "12 / 77 - <label>: MARIO".
    pub home_folder: &'static str,
    /// First row of the shelf picker: take this game off every shelf. Describes
    /// a STATE, not a destination -- it used to say ACCUEIL, which promised a
    /// place to go, and the only thing it moves is a game that is physically
    /// filed in a directory (back to the root of the games folder).
    pub folder_root: &'static str,
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
    /// REGLAGES row: where new games are downloaded to (#79).
    pub set_games_dir: &'static str,
    /// Folder picker: confirm the highlighted folder as the destination.
    pub games_dir_pick: &'static str,
    /// Folder picker: go back up one level.
    pub games_dir_up: &'static str,
    /// Folder picker: create a subfolder here, so a card can be laid out
    /// without leaving FlashNX for a file manager.
    pub games_dir_new: &'static str,
    pub kbd_newdir_guide: &'static str,
    /// Folder picker footer: it has three actions, and the shared
    /// two-action footer hid the one nobody would guess.
    pub games_dir_footer: &'static str,
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
    /// Ask whether the games already on the card should follow.
    pub games_dir_ask_move: &'static str,
    pub games_dir_move: &'static str,
    pub games_dir_keep: &'static str,
    /// Shown while the move runs, and when it is done.
    pub games_dir_moving: &'static str,
    /// Titre de la barre d'extraction d'un GameZIP (#117). Un gros jeu
    /// met aussi longtemps a s'extraire qu'a se telecharger, et l'ecran
    /// restait fige sur la barre de telechargement pleine.
    pub extracting: &'static str,
    /// Ligne RACCOURCI de la modale par jeu, et ses deux retours.
    pub opt_shortcut: &'static str,
    pub shortcut_ok: &'static str,
    pub shortcut_err: &'static str,
    /// Libelle et message de l'etat inverse : le raccourci existe deja.
    pub opt_shortcut_del: &'static str,
    pub shortcut_removed: &'static str,
    pub games_dir_moved: &'static str,
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
    /// Free-zoom row in the pause menu, shown as "<label>: <percent> %", and
    /// again as the heading of the framing mode it opens. Issue #101.
    pub set_zoom: &'static str,
    /// The one line of the zoom-adjust mode: what each button does while the
    /// picture is being framed. Button names (A, B, ZL, ZR, L3) stay as they
    /// are printed on the console; only the verbs are translated.
    pub zoom_legend: &'static str,
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
    /// One line under the row while it is selected, like `overclock_hint`.
    /// AUTO and SMOOTH draw the same picture on every game whose bitmaps all
    /// ask for smoothing, which is most of them, so without this two of the
    /// three positions look like a dead button.
    pub pixels_hint: &'static str,
    /// Pause row + defaults row: raise the CPU clock for heavy games.
    pub set_overclock: &'static str,
    /// The console's own profile, untouched.
    pub overclock_off: &'static str,
    /// CPU 1785 MHz. The GPU is never touched.
    pub overclock_on: &'static str,
    /// One line under the row. Says the two things the evidence actually
    /// supports: it drains the battery faster and it runs hotter. It does NOT
    /// claim hardware damage, because nothing in the record supports that, and
    /// dressing folklore as a warning is worse than saying nothing.
    pub overclock_hint: &'static str,
    /// Replaces the hint when the console refused the raise. Seen for real on
    /// 2026-08-26 at 7% charge: psm reports the pack out of its Normal voltage
    /// state and the raise is refused, which without this line is a button
    /// pressed eight times with nothing happening and no reason given.
    pub overclock_refused: &'static str,
    /// Replaces `overclock_hint` while the row reads AUTO: the raise comes and
    /// goes on its own, so the row has to say when. Its label is `pixels_auto`,
    /// the same word in every language we ship.
    pub overclock_auto_hint: &'static str,
    /// ECRAN row + defaults row: show the frame rate over the running game (#111).
    pub set_fps: &'static str,
    /// One line under that row. It has to say what the SECOND number is, because
    /// nothing here declares 60 (Mario 63 asks for 32, Papa Louie 3 for 30, Icy
    /// Tower for 24) and a lone "15" means half speed on one of those and full
    /// speed on none of them.
    pub fps_hint: &'static str,
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
    /// Last row of the APPLY picker, and the title of the list it opens: the
    /// player's own games with controls of their own, to copy from.
    pub profile_copy_row: &'static str,
    /// Toast when no other game has controls of its own to copy.
    pub profile_copy_none: &'static str,
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
    /// Footer of the shelf picker. Says all three actions, B included: it is the
    /// only way out and it had stopped being written down.
    pub folder_picker_footer: &'static str,
    /// Title of the confirmation that empties a shelf (issue #68). One {}: the
    /// folder's name.
    pub folder_del_title: &'static str,
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
    menu_restart: "Restart",
    menu_quit: "Quit",
    menu_cursor: "Cursor",
    pause_title: "Pause",
    pause_footer: "A:OK   B:Cancel   up/down:Nav",
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
    empty_l2: "sdmc:/flashnx/",
    empty_l3: "Then restart FlashNX.",
    empty_footer: "Y:Remote import   -:Quit",
    list_footer: "L/R:Tabs  A:Play  Y:Sort  -:Search  +:Options",
    applet_title: "Applet mode",
    applet_notice: "Launching a game needs the full app memory, which applet mode does not have. In the homebrew menu, hold R on a title (or use a forwarder) to start FlashNX with full memory.",
    options_title: "Options",
    opt_keys: "Controls and cursor",
    opt_rename: "Rename",
    opt_edit: "Edit",
    opt_folder: "Folder",
    opt_delete: "Delete",
    opt_back: "Back",
    options_footer: "A:OK   B:Back",
    del_title: "Delete?",
    del_l1: "The .swf file, the saves (.sol),",
    del_l2: "the controls and the alias will be erased.",
    del_l3: "This cannot be undone.",
    del_footer: "A: Delete     B: Cancel",
    dist_title: "Remote import",
    dist_search_games: "Search for a game",
    dist_sources: "My sources",
    dist_empty_hint: "Search a game by name, or paste an address",
    dist_add: "+ Add a URL",
    dist_list_footer: "A:Launch   +:Options   Y:Sort   -:Search   X:Flashpoint",
    dist_count: "{} URL(s)",
    dist_subtitle: "Download SWF from archive.org",
    dist_press_a: "Press A to enter a URL",
    dist_example1: "Example: https://archive.org/details/<item-id>",
    dist_example2: "Or simply <item-id>",
    dist_history: "History",
    dist_hint_zr: "ZR : Load this URL directly",
    dist_hint_a: "A  : Enter / edit URL (keyboard)",
    dist_hint_lr: "L / R : Previous / next URL",
    dist_footer_hist: "ZR:Open  A:Edit  ZL:Delete  L/R:Nav  Y:Local  -:Quit",
    dist_footer_nohist: "A:Enter URL   Y:Back local   -:Quit",
    files_title: "Remote files",
    files_filter: "Filter",
    home_folder: "Folder",
    folder_root: "No folder",
    files_footer: "A:Download   Y:Sort   -:Search   L/R:Page   B:Back",
    dl_title: "Downloading",
    dl_footer: "B:Cancel",
    toast_dl_ok: "{} downloaded",
    toast_assets_missing: "Missing game data: {}. This game may not start.",
    err_title: "Error",
    err_footer: "A/B:OK",
    err_footer_fix: "A/B:OK   Y:Fix the URL",
    settings_title: "Settings",
    set_keys: "Default controls",
    set_language: "Language",
    set_quit: "Quit",
    set_pseudo: "Nickname",
    kbd_pseudo_guide: "Your name, shown next to your shared profiles",
    set_cursor_speed: "Cursor speed",
    set_display_mode: "Display",
    display_fit: "Fit",
    display_fill: "Fill",
    display_stretch: "Stretch",
    set_zoom: "Zoom",
    zoom_legend: "ZL/L: -   ZR/R: +   X/Y and pad: fine   stick/touch: frame   2 fingers: zoom   L3: Reset   A: OK   B: Cancel",
    set_screen_filter: "Filter",
    set_home_view: "Home view",
    set_games_dir: "Games folder",
    games_dir_pick: "Choose",
    games_dir_up: "Up",
    games_dir_new: "New folder",
    games_dir_footer: "A:Choose  B:Up  X:New folder",
    games_dir_current: "in use",
    games_dir_confirm: "Move the whole library here?",
    games_dir_confirm_n: "{} games will be moved to:",
    games_dir_confirm_1: "{} game will be moved to:",
    games_dir_confirm_yes: "Move",
    games_dir_confirm_no: "Cancel",
    kbd_newdir_guide: "Name of the folder to create",
    games_dir_ask_move: "Move the games already on the card?",
    games_dir_move: "Move them",
    games_dir_keep: "Leave them",
    games_dir_moving: "Moving...",
    extracting: "Extracting...",
    opt_shortcut: "HOME menu",
    shortcut_ok: "Tile installed on the HOME menu",
    shortcut_err: "Could not install the tile",
    opt_shortcut_del: "Remove from HOME menu",
    shortcut_removed: "Tile removed",
    games_dir_moved: "Moved",
    home_grid: "Grid",
    home_list: "List",
    home_strip: "Strip",
    home_shelf: "Shelf",
    set_game_prefs: "Game defaults",
    prefs_title: "Default settings",
    filter_none: "None",
    filter_scanlines: "Scanlines",
    filter_crt: "CRT",
    set_pixel_filter: "Pixels",
    pixels_auto: "Auto",
    pixels_smooth: "Smooth",
    pixels_sharp: "Sharp",
    pixels_hint: "AUTO: as the game asks. SHARP: no blur.",
    set_overclock: "Overclock",
    overclock_off: "Off",
    overclock_on: "On",
    overclock_hint: "ON: more battery used, more heat.",
    overclock_refused: "Overclock unavailable: battery too low.",
    overclock_auto_hint: "AUTO: speeds up only slow games.",
    set_fps: "FPS counter",
    fps_hint: "Frames per second, then the rate the game asks for.",
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
    set_back: "Back",
    set_report_bug: "Report a bug",
    tab_play: "Play",
    tab_import: "Import",
    tab_settings: "Settings",
    set_covers: "Online covers",
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
    profile_footer: "A:Apply   B:Back   up/down:Nav",
    profile_none: "No profile for this game yet. Share yours to help!",
    profile_catalog_offline: "Catalog unavailable. Check your connection.",
    profile_revert: "Revert to my controls",
    profile_applied_ok: "Profile applied. Your previous controls were saved.",
    profile_reverted_ok: "Your controls were restored.",
    profile_share_confirm: "Share your controls for this game?",
    share_confirm_update: "Updates your shared profile:",
    touches_edit: "Edit controls",
    touches_footer: "A:Select   B:Back   up/down:Nav",
    touches_revert_default: "Reset to default controls",
    profile_preview_title: "Mine → profile",
    profile_preview_footer: "A:Apply   B:Back",
    profile_preview_none: "This profile changes none of your keys.",
    profile_active: "(Active)",
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
    folder_picker_footer: "A:On/off  X:Empty  B:Back",
    folder_del_title: "Delete folder {}",
    profile_del_confirm: "Delete your shared profile?",
    profile_del_ok: "Your shared profile was deleted.",
    profile_del_not_mine: "Nothing to delete: not shared from this console.",
    profile_del_hint: "X:Delete",
    revert_preview_title: "Now → after revert",
    revert_preview_footer: "A:Revert   B:Back",
    cover_title: "Choose a cover",
    cover_footer: "A: Choose   -: Search   up/down: Move   B: Back",
    cover_show_logos: "Y: Logos",
    cover_show_shots: "Y: Screenshots",
    cover_off_notice: "Enable online covers in settings",
    cover_none: "No results",
    fp_title: "Flashpoint",
    fp_footer: "A:Download   X:Search   Y:Sort   +:Info   ZL+ZR:Filter {}   B:Back",
    fp_details_title: "Details",
    fp_details_dev: "Developer",
    fp_details_publisher: "Publisher",
    fp_details_date: "Released",
    fp_details_size: "Download size",
    fp_details_footer: "B:Back",
    sort_title: "Sort by",
    sort_footer: "A:Choose   X:Reverse   B:Back",
    sort_alpha: "Name",
    sort_recent: "Added",
    sort_played: "Most played",
    sort_size: "Size",
    played_label: "Played",
    sort_recent_played: "Last played",
    sort_dev: "Developer",
    sort_source: "Source",
    sort_files: "File count",
    fav_added: "Added to favorites",
    fav_removed: "Removed from favorites",
    multifile: "Multi-file",
    sort_dir_asc: "Ascending",
    sort_dir_desc: "Descending",
    settings_footer: "L/R:Tabs   A:OK",
    lang_title: "Language",
    lang_footer: "A:OK   B:Cancel",
    histdel_title: "Delete URL?",
    url_info_type: "Type",
    url_type_swf: "Single .swf",
    url_type_list: "File list",
    url_info_files: "On SD",
    url_info_added: "Added",
    histdel_msg: "Remove this URL from the history?",
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
    bug_pick_title: "Report a bug",
    bug_pick_footer: "A:Choose   B:Back   up/down:Nav",
    bug_no_games: "No game to report yet. Import or drop a .swf first.",
    bug_ok_title: "Thanks!",
    bug_ok_msg: "Your report was sent. Thanks for helping improve FlashNX.",
    bug_fail_title: "Failed",
    kbd_bug_header: "FlashNX - Report a bug",
    kbd_bug_guide: "Opens a PUBLIC GitHub issue. Describe the problem. Optional: your @handle. A short technical log is attached.",
    set_suggest: "Make a suggestion",
    kbd_suggest_header: "FlashNX - Make a suggestion",
    kbd_suggest_guide: "Opens a PUBLIC GitHub issue. Your idea / feature request for FlashNX.",
    suggest_ok_msg: "Your suggestion was sent. Thanks for helping improve FlashNX.",
};

const FR: Strings = Strings {
    optimizing: "Optimisation",
    menu_resume: "Reprendre",
    menu_keys: "Touches et curseur",
    menu_screen: "Écran",
    menu_restart: "Redémarrer",
    menu_quit: "Quitter",
    menu_cursor: "Curseur",
    pause_title: "Pause",
    pause_footer: "A:OK   B:Annuler   haut/bas:Nav",
    keys_title: "Touches",
    keys_footer: "A:Éditer  L/R:Mode  X:J1/J2  B:Retour",
    keys_dropdown_footer: "A:OK   B:Annuler   haut/bas:Nav",
    keys_modifier: "Modificateur",
    none: "(aucune)",
    flash_mouse_left: "Clic gauche",
    flash_mouse_right: "Clic droit",
    flash_keyboard: "Clavier",
    flash_space: "Espace",
    flash_enter: "EntrÉe",
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
    empty_l2: "sdmc:/flashnx/",
    empty_l3: "Puis redémarre FlashNX.",
    empty_footer: "Y:Import distant   -:Quitter",
    list_footer: "L/R:Onglets  A:Jouer  Y:Trier  -:Recherche  +:Options",
    applet_title: "Mode applet",
    applet_notice: "Lancer un jeu demande toute la mémoire de l'app, indisponible en mode applet. Dans le menu Homebrew, tiens R sur un titre (ou un forwarder) pour démarrer FlashNX avec toute la mémoire.",
    options_title: "Options",
    opt_keys: "Touches et curseur",
    opt_rename: "Renommer",
    opt_edit: "Éditer",
    opt_folder: "Dossier",
    opt_delete: "Supprimer",
    opt_back: "Retour",
    options_footer: "A:OK   B:Retour",
    del_title: "Supprimer ?",
    del_l1: "LE FICHIER .swf, LES SAUVEGARDES (.sol),",
    del_l2: "Les touches et l'alias seront effacés.",
    del_l3: "Action irréversible.",
    del_footer: "A: Supprimer     B: Annuler",
    dist_title: "Import distant",
    dist_search_games: "Chercher un jeu",
    dist_sources: "Mes sources",
    dist_empty_hint: "Cherche un jeu par son nom ou colle une adresse",
    dist_add: "+ Ajouter une URL",
    dist_list_footer: "A:Lancer   +:Options   Y:Trier   -:Recherche   X:Flashpoint",
    dist_count: "{} URL",
    dist_subtitle: "Téléchargement de SWF depuis archive.org",
    dist_press_a: "Appuie sur A pour saisir une URL",
    dist_example1: "Exemple : https://archive.org/details/<item-id>",
    dist_example2: "Ou simplement <item-id>",
    dist_history: "Historique",
    dist_hint_zr: "ZR : Charger cette URL directement",
    dist_hint_a: "A  : Saisir / éditer URL (clavier)",
    dist_hint_lr: "L / R : URL précédente / suivante",
    dist_footer_hist: "ZR:Ouvrir  A:Éditer  ZL:Suppr  L/R:Nav  Y:Local  -:Quitter",
    dist_footer_nohist: "A:Saisir URL   Y:Retour local   -:Quitter",
    files_title: "Fichiers distants",
    files_filter: "Filtre",
    home_folder: "Dossier",
    folder_root: "Aucun dossier",
    files_footer: "A:Télécharger   Y:Trier   -:Recherche   L/R:Page   B:Retour",
    dl_title: "Téléchargement",
    dl_footer: "B:Annuler",
    toast_dl_ok: "{} téléchargé",
    toast_assets_missing: "Données de jeu manquantes : {}. Ce jeu risque de ne pas démarrer.",
    err_title: "Erreur",
    err_footer: "A/B:OK",
    err_footer_fix: "A/B:OK   Y:Corriger l'URL",
    settings_title: "Réglages",
    set_keys: "Touches par défaut",
    set_language: "Langue",
    set_quit: "Quitter",
    set_pseudo: "Pseudo",
    kbd_pseudo_guide: "Ton nom, affiché à côté de tes profils partagés",
    set_cursor_speed: "Vitesse du curseur",
    set_display_mode: "Affichage",
    display_fit: "Intégral",
    display_fill: "Remplir",
    display_stretch: "Étirer",
    set_zoom: "Zoom",
    zoom_legend: "ZL/L: -   ZR/R: +   X/Y et croix : précis   stick/doigt: cadrer   2 doigts: zoom   L3: 100 %   A: OK   B: Annuler",
    set_screen_filter: "Filtre",
    set_home_view: "Vue accueil",
    set_games_dir: "Dossier jeux",
    games_dir_pick: "Choisir",
    games_dir_up: "Remonter",
    games_dir_new: "Nouveau dossier",
    games_dir_footer: "A:Choisir  B:Remonter  X:Nouveau",
    games_dir_current: "actuel",
    games_dir_confirm: "DÉplacer toute la bibliothÈque ici ?",
    games_dir_confirm_n: "{} jeux seront déplacés vers :",
    games_dir_confirm_1: "{} jeu sera déplacé vers :",
    games_dir_confirm_yes: "DÉplacer",
    games_dir_confirm_no: "Annuler",
    kbd_newdir_guide: "Nom du dossier à créer",
    games_dir_ask_move: "DÉplacer les jeux dÉjÀ sur la carte ?",
    games_dir_move: "Les deplacer",
    games_dir_keep: "Les laisser",
    games_dir_moving: "DÉplacement...",
    extracting: "Extraction...",
    opt_shortcut: "Menu HOME",
    shortcut_ok: "Tuile installée sur le menu HOME",
    shortcut_err: "Impossible d'installer la tuile",
    opt_shortcut_del: "Retirer du menu HOME",
    shortcut_removed: "Tuile retirée",
    games_dir_moved: "DÉplacÉs",
    home_grid: "Grille",
    home_list: "Liste",
    home_strip: "Bande",
    home_shelf: "Étagère",
    set_game_prefs: "Préférences de jeu",
    prefs_title: "Réglages par défaut",
    filter_none: "Aucun",
    filter_scanlines: "Lignes",
    filter_crt: "CRT",
    set_pixel_filter: "Pixels",
    pixels_auto: "Auto",
    pixels_smooth: "Lissés",
    pixels_sharp: "Nets",
    pixels_hint: "AUTO : comme le jeu le demande. NETS : aucun flou.",
    set_overclock: "Overclock",
    overclock_off: "Inactif",
    overclock_on: "Actif",
    overclock_hint: "ACTIF : plus de batterie, plus de chaleur.",
    overclock_refused: "Overclock indisponible : batterie trop faible.",
    overclock_auto_hint: "AUTO : seulement si le jeu ralentit.",
    set_fps: "Compteur FPS",
    fps_hint: "Images par seconde, puis la cadence demandée par le jeu.",
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
    set_back: "Retour",
    set_report_bug: "Signaler un bug",
    tab_play: "Jouer",
    tab_import: "Importer",
    tab_settings: "Réglages",
    set_covers: "Jaquettes en ligne",
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
    profile_footer: "A:Appliquer   B:Retour   haut/bas:Nav",
    profile_none: "Aucun profil pour ce jeu. Partage le tien pour aider !",
    profile_catalog_offline: "Catalogue indisponible. Vérifie ta connexion.",
    profile_revert: "Revenir à mes touches",
    profile_applied_ok: "Profil appliqué. Tes touches précédentes sont sauvegardées.",
    profile_reverted_ok: "Tes touches ont été restaurées.",
    profile_share_confirm: "Partager tes touches pour ce jeu ?",
    share_confirm_update: "Met à jour ton profil partagé :",
    touches_edit: "Modifier les touches",
    touches_footer: "A:Choisir   B:Retour   haut/bas:Nav",
    touches_revert_default: "Revenir aux touches par défaut",
    profile_preview_title: "Moi → profil",
    profile_preview_footer: "A:Appliquer   B:Retour",
    profile_preview_none: "Ce profil ne change aucune touche.",
    profile_active: "(Actif)",
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
    folder_picker_footer: "A:Ajouter/retirer  X:Vider  B:Retour",
    folder_del_title: "Supprimer le dossier {}",
    profile_del_confirm: "Supprimer ton profil partagé ?",
    profile_del_ok: "Ton profil partagé a été supprimé.",
    profile_del_not_mine: "Rien à supprimer : pas partagé depuis cette console.",
    profile_del_hint: "X:Supprimer",
    revert_preview_title: "Actuel → après retour",
    revert_preview_footer: "A:Revenir   B:Retour",
    cover_title: "Choisir une jaquette",
    cover_footer: "A: Choisir   -: Rechercher   haut/bas: Naviguer   B: Retour",
    cover_show_logos: "Y: Logos",
    cover_show_shots: "Y: Captures",
    cover_off_notice: "Active les jaquettes en ligne dans les réglages",
    cover_none: "Aucun résultat",
    fp_title: "Flashpoint",
    fp_footer: "A:Télécharger   X:Recherche   Y:Trier   +:Infos   ZL+ZR:Filtre {}   B:Retour",
    fp_details_title: "Détails",
    fp_details_dev: "Développeur",
    fp_details_publisher: "Éditeur",
    fp_details_date: "Sortie",
    fp_details_size: "Taille du téléchargement",
    fp_details_footer: "B:Retour",
    sort_title: "Trier par",
    sort_footer: "A:Choisir   X:Inverser   B:Retour",
    sort_alpha: "Nom",
    sort_recent: "Ajouté",
    sort_played: "Plus joués",
    sort_size: "Taille",
    played_label: "Joué",
    sort_recent_played: "Dernier joué",
    sort_dev: "Développeur",
    sort_source: "Source",
    sort_files: "Nombre de fichiers",
    fav_added: "Ajouté aux favoris",
    fav_removed: "Retiré des favoris",
    multifile: "Multi-fichiers",
    sort_dir_asc: "Croissant",
    sort_dir_desc: "Décroissant",
    settings_footer: "L/R:Onglets   A:OK",
    lang_title: "Langue",
    lang_footer: "A:OK   B:Annuler",
    histdel_title: "Supprimer URL ?",
    url_info_type: "Type",
    url_type_swf: "Fichier .swf",
    url_type_list: "Liste de fichiers",
    url_info_files: "Sur la SD",
    url_info_added: "Ajoutée le",
    histdel_msg: "Retirer cette URL de l'historique ?",
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
    err_json_no_files: "JSON SANS CHAMP \"files\"",
    err_dl_start: "Impossible de lancer le téléchargement (code {}).",
    err_dl_failed: "Téléchargement échoué (code {})",
    err_dl_cancelled: "Téléchargement annulé par l'utilisateur.",
    err_url_invalid: "URL INVALIDE. ATTENDU UNE URL ARCHIVE.ORG TYPE https://archive.org/details/<id> OU SIMPLEMENT <id>.",
    err_no_swf: "Aucun fichier .swf trouvé pour ce jeu.",
    err_dl_not_a_game: "Le téléchargement n'a pas renvoyé un fichier de jeu. Vérifie l'adresse.",
    err_fp_html_game: "Ce jeu se lance via une page HTML (FlashVars), pas encore supporté.",
    kbd_url_header: "FlashNX - Import distant",
    kbd_url_guide: "URL archive.org de l'item (ex: https://archive.org/download/your-game-id)",
    kbd_rename_header: "FlashNX - Renommer le jeu",
    kbd_rename_guide: "Nom d'affichage (laisser vide pour revenir au nom de fichier)",
    kbd_search_header: "FlashNX - Rechercher",
    kbd_search_guide: "Filtre nom de fichier (vide = tout afficher)",
    bug_pick_title: "Signaler un bug",
    bug_pick_footer: "A:Choisir   B:Retour   haut/bas:Nav",
    bug_no_games: "Aucun jeu à signaler. Importe ou dépose un .swf d'abord.",
    bug_ok_title: "Merci !",
    bug_ok_msg: "Ton rapport a été envoyé. Merci de m'aider à améliorer FlashNX.",
    bug_fail_title: "Échec",
    kbd_bug_header: "FlashNX - Signaler un bug",
    kbd_bug_guide: "Ouvre une issue GitHub PUBLIQUE. Décris le problème. Option : ton @pseudo. Un court journal technique est joint.",
    set_suggest: "Faire une proposition",
    kbd_suggest_header: "FlashNX - Faire une proposition",
    kbd_suggest_guide: "Ouvre une issue GitHub PUBLIQUE. Ton idée / proposition pour FlashNX.",
    suggest_ok_msg: "Ta proposition a été envoyée. Merci de m'aider à améliorer FlashNX.",
};

const ES: Strings = Strings {
    optimizing: "Optimizando",
    menu_resume: "Reanudar",
    menu_keys: "Controles y cursor",
    menu_screen: "Pantalla",
    menu_restart: "Reiniciar",
    menu_quit: "Salir",
    menu_cursor: "Cursor",
    pause_title: "Pausa",
    pause_footer: "A:OK   B:Cancelar   arriba/abajo:Nav",
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
    flash_shift: "MayÚs",
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
    empty_l2: "sdmc:/flashnx/",
    empty_l3: "Y reinicia FlashNX.",
    empty_footer: "Y:Importar remoto   -:Salir",
    list_footer: "L/R:Pestañas  A:Jugar  Y:Ordenar  -:Buscar  +:Opciones",
    applet_title: "Modo applet",
    applet_notice: "Ejecutar un juego necesita toda la memoria de la app, que el modo applet no tiene. En el homebrew menu, mantén R sobre un título (o usa un forwarder) para iniciar FlashNX con toda la memoria.",
    options_title: "Opciones",
    opt_keys: "Controles y cursor",
    opt_rename: "Renombrar",
    opt_edit: "Editar",
    opt_folder: "Carpeta",
    opt_delete: "Borrar",
    opt_back: "Volver",
    options_footer: "A:OK   B:Volver",
    del_title: "¿Borrar?",
    del_l1: "EL ARCHIVO .swf, LAS PARTIDAS (.sol),",
    del_l2: "Los controles y el alias se borrarán.",
    del_l3: "Acción irreversible.",
    del_footer: "A: Borrar     B: Cancelar",
    dist_title: "Importar remoto",
    dist_search_games: "Buscar un juego",
    dist_sources: "Mis fuentes",
    dist_empty_hint: "Busca un juego por su nombre o pega una dirección",
    dist_add: "+ Añadir una URL",
    dist_list_footer: "A:Lanzar   +:Opciones   Y:Ordenar   -:Buscar   X:Flashpoint",
    dist_count: "{} URL",
    dist_subtitle: "Descargar SWF desde archive.org",
    dist_press_a: "Pulsa A para introducir una URL",
    dist_example1: "Ejemplo: https://archive.org/details/<item-id>",
    dist_example2: "O simplemente <item-id>",
    dist_history: "Historial",
    dist_hint_zr: "ZR : Cargar esta URL directamente",
    dist_hint_a: "A  : Introducir / editar URL (teclado)",
    dist_hint_lr: "L / R : URL anterior / siguiente",
    dist_footer_hist: "ZR:Abrir  A:Editar  ZL:Borrar  L/R:Nav  Y:Local  -:Salir",
    dist_footer_nohist: "A:Introducir URL   Y:Volver local   -:Salir",
    files_title: "Archivos remotos",
    files_filter: "Filtro",
    home_folder: "Carpeta",
    folder_root: "Sin carpeta",
    files_footer: "A:Descargar   Y:Ordenar   -:Buscar   L/R:Página   B:Volver",
    dl_title: "Descargando",
    dl_footer: "B:Cancelar",
    toast_dl_ok: "{} descargado",
    toast_assets_missing: "Faltan datos del juego: {}. Puede que no arranque.",
    err_title: "Error",
    err_footer: "A/B:OK",
    err_footer_fix: "A/B:OK   Y:Corregir la URL",
    settings_title: "Ajustes",
    set_keys: "Controles por defecto",
    set_language: "Idioma",
    set_quit: "Salir",
    set_pseudo: "Apodo",
    kbd_pseudo_guide: "Tu nombre, junto a tus perfiles compartidos",
    set_cursor_speed: "Velocidad del cursor",
    set_display_mode: "Pantalla",
    display_fit: "Ajustar",
    display_fill: "Rellenar",
    display_stretch: "Estirar",
    set_zoom: "Zoom",
    zoom_legend: "ZL/L: -   ZR/R: +   X/Y y cruz: preciso   stick/dedo: encuadrar   2 dedos: zoom   L3: 100 %   A: OK   B: Cancelar",
    set_screen_filter: "Filtro",
    set_home_view: "Vista inicio",
    set_games_dir: "Carpeta juegos",
    games_dir_pick: "Elegir",
    games_dir_up: "Subir",
    games_dir_new: "Nueva carpeta",
    games_dir_footer: "A:Elegir  B:Subir  X:Nueva",
    games_dir_current: "en uso",
    games_dir_confirm: "Mover toda la biblioteca aqui?",
    games_dir_confirm_n: "{} juegos se moveran a:",
    games_dir_confirm_1: "{} juego se movera a:",
    games_dir_confirm_yes: "Mover",
    games_dir_confirm_no: "Cancelar",
    kbd_newdir_guide: "Nombre de la carpeta a crear",
    games_dir_ask_move: "Mover los juegos ya en la tarjeta?",
    games_dir_move: "Moverlos",
    games_dir_keep: "Dejarlos",
    games_dir_moving: "Moviendo...",
    extracting: "Extrayendo...",
    opt_shortcut: "Menú HOME",
    shortcut_ok: "Icono instalado en el menú HOME",
    shortcut_err: "No se pudo instalar el icono",
    opt_shortcut_del: "Quitar del menú HOME",
    shortcut_removed: "Icono eliminado",
    games_dir_moved: "Movidos",
    home_grid: "Cuadrícula",
    home_list: "Lista",
    home_strip: "Tira",
    home_shelf: "Estante",
    set_game_prefs: "Preferencias",
    prefs_title: "Ajustes por defecto",
    filter_none: "Ninguno",
    filter_scanlines: "Líneas",
    filter_crt: "CRT",
    set_pixel_filter: "Píxeles",
    pixels_auto: "Auto",
    pixels_smooth: "Suaves",
    pixels_sharp: "Nítidos",
    pixels_hint: "AUTO: como pide el juego. NÍTIDOS: sin desenfoque.",
    set_overclock: "Overclock",
    overclock_off: "Inactivo",
    overclock_on: "Activo",
    overclock_hint: "ACTIVO: más batería, más calor.",
    overclock_refused: "Overclock no disponible: batería muy baja.",
    overclock_auto_hint: "AUTO: solo si el juego va lento.",
    set_fps: "Contador FPS",
    fps_hint: "Fotogramas por segundo, y la tasa que pide el juego.",
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
    set_back: "Volver",
    set_report_bug: "Reportar un fallo",
    tab_play: "Jugar",
    tab_import: "Importar",
    tab_settings: "Ajustes",
    set_covers: "Carátulas en línea",
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
    profile_footer: "A:Aplicar   B:Volver   arriba/abajo:Nav",
    profile_none: "Aún no hay perfil para este juego. ¡Comparte el tuyo!",
    profile_catalog_offline: "Catálogo no disponible. Comprueba tu conexión.",
    profile_revert: "Volver a mis controles",
    profile_applied_ok: "Perfil aplicado. Tus controles anteriores se guardaron.",
    profile_reverted_ok: "Tus controles se restauraron.",
    profile_share_confirm: "¿Compartir tus controles para este juego?",
    share_confirm_update: "Actualiza tu perfil compartido:",
    touches_edit: "Editar controles",
    touches_footer: "A:Elegir   B:Volver   arriba/abajo:Nav",
    touches_revert_default: "Restablecer controles por defecto",
    profile_preview_title: "Míos → perfil",
    profile_preview_footer: "A:Aplicar   B:Volver",
    profile_preview_none: "Este perfil no cambia ninguna tecla.",
    profile_active: "(Activo)",
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
    folder_picker_footer: "A:Poner/quitar  X:Vaciar  B:Volver",
    folder_del_title: "Borrar la carpeta {}",
    profile_del_confirm: "¿Borrar tu perfil compartido?",
    profile_del_ok: "Tu perfil compartido fue borrado.",
    profile_del_not_mine: "Nada que borrar desde esta consola.",
    profile_del_hint: "X:Borrar",
    revert_preview_title: "Actual → tras revertir",
    revert_preview_footer: "A:Revertir   B:Volver",
    cover_title: "Elegir carátula",
    cover_footer: "A: Elegir   -: Buscar   arriba/abajo: Mover   B: Volver",
    cover_show_logos: "Y: Logos",
    cover_show_shots: "Y: Capturas",
    cover_off_notice: "Activa carátulas en línea en Ajustes",
    cover_none: "Sin resultados",
    fp_title: "Flashpoint",
    fp_footer: "A:Descargar   X:Buscar   Y:Ordenar   +:Info   ZL+ZR:Filtro {}   B:Volver",
    fp_details_title: "Detalles",
    fp_details_dev: "Desarrollador",
    fp_details_publisher: "Editor",
    fp_details_date: "Lanzamiento",
    fp_details_size: "Tamaño de descarga",
    fp_details_footer: "B:Volver",
    sort_title: "Ordenar por",
    sort_footer: "A:Elegir   X:Invertir   B:Volver",
    sort_alpha: "Nombre",
    sort_recent: "Añadido",
    sort_played: "Más jugados",
    sort_size: "Tamaño",
    played_label: "Jugado",
    sort_recent_played: "Último jugado",
    sort_dev: "Desarrollador",
    sort_source: "Fuente",
    sort_files: "Número de archivos",
    fav_added: "Añadido a favoritos",
    fav_removed: "Quitado de favoritos",
    multifile: "Multiarchivo",
    sort_dir_asc: "Ascendente",
    sort_dir_desc: "Descendente",
    settings_footer: "L/R:Pestañas   A:OK",
    lang_title: "Idioma",
    lang_footer: "A:OK   B:Cancelar",
    histdel_title: "¿Borrar la URL?",
    url_info_type: "Tipo",
    url_type_swf: "Archivo .swf",
    url_type_list: "Lista de archivos",
    url_info_files: "En la SD",
    url_info_added: "Añadida el",
    histdel_msg: "¿Quitar esta URL del historial?",
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
    err_json_no_files: "JSON SIN CAMPO \"files\"",
    err_dl_start: "No se pudo iniciar la descarga (código {}).",
    err_dl_failed: "Descarga fallida (código {})",
    err_dl_cancelled: "Descarga cancelada por el usuario.",
    err_url_invalid: "URL INVÁLIDA. SE ESPERABA UNA URL DE ARCHIVE.ORG TIPO https://archive.org/details/<id> O SIMPLEMENTE <id>.",
    err_no_swf: "No se encontró ningún archivo .swf para este juego.",
    err_dl_not_a_game: "La descarga no devolvió un archivo de juego. Comprueba la dirección.",
    err_fp_html_game: "Este juego se inicia desde una página HTML (FlashVars); aún no compatible.",
    kbd_url_header: "FlashNX - Importar remoto",
    kbd_url_guide: "URL del item de archive.org (ej: https://archive.org/download/your-game-id)",
    kbd_rename_header: "FlashNX - Renombrar juego",
    kbd_rename_guide: "Nombre a mostrar (dejar vacio para volver al nombre de archivo)",
    kbd_search_header: "FlashNX - Buscar",
    kbd_search_guide: "Filtrar por nombre de archivo (vacio = mostrar todo)",
    bug_pick_title: "Reportar un fallo",
    bug_pick_footer: "A:Elegir   B:Volver   arriba/abajo:Nav",
    bug_no_games: "No hay juego que reportar. Importa o coloca un .swf primero.",
    bug_ok_title: "¡Gracias!",
    bug_ok_msg: "Tu informe se envió. Gracias por ayudar a mejorar FlashNX.",
    bug_fail_title: "Fallo",
    kbd_bug_header: "FlashNX - Reportar un fallo",
    kbd_bug_guide: "Abre un issue PUBLICO en GitHub. Describe el problema. Opcional: tu @usuario. Se adjunta un registro tecnico breve.",
    set_suggest: "Hacer una sugerencia",
    kbd_suggest_header: "FlashNX - Hacer una sugerencia",
    kbd_suggest_guide: "Abre un issue PUBLICO en GitHub. Tu idea / sugerencia para FlashNX.",
    suggest_ok_msg: "Tu sugerencia se envió. Gracias por ayudar a mejorar FlashNX.",
};

// Russian — UPPERCASE Cyrillic (draw_text does not case-fold non-ASCII, and
// only uppercase Cyrillic glyphs were added to the font).
const RU: Strings = Strings {
    optimizing: "Оптимизация",
    menu_resume: "Продолжить",
    menu_keys: "Клавиши и курсор",
    menu_screen: "Экран",
    menu_restart: "Заново",
    menu_quit: "Выход",
    menu_cursor: "Курсор",
    pause_title: "Пауза",
    pause_footer: "A:ОК   B:Отмена   вверх/вниз:Нав",
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
    empty_l2: "sdmc:/flashnx/",
    empty_l3: "Затем перезапустите FlashNX.",
    empty_footer: "Y:Загрузка по сети   -:Выход",
    list_footer: "L/R:Вкладки  A:Играть  Y:Сорт  -:Поиск  +:Опции",
    applet_title: "Режим апплета",
    applet_notice: "Для запуска игры нужна вся память приложения, которой нет в режиме апплета. В homebrew menu удерживайте R на игре, чтобы запустить FlashNX со всей памятью.",
    options_title: "Опции",
    opt_keys: "Клавиши и курсор",
    opt_rename: "Переименовать",
    opt_edit: "Изменить",
    opt_folder: "Папка",
    opt_delete: "Удалить",
    opt_back: "Назад",
    options_footer: "A:ОК   B:Назад",
    del_title: "Удалить?",
    del_l1: "ФАЙЛ .swf, СОХРАНЕНИЯ (.sol),",
    del_l2: "Клавиши и псевдоним будут удалены.",
    del_l3: "Действие необратимо.",
    del_footer: "A: Удалить     B: Отмена",
    dist_title: "Загрузка по сети",
    dist_search_games: "Найти игру",
    dist_sources: "Мои источники",
    dist_empty_hint: "Найдите игру по названию или вставьте ссылку",
    dist_add: "+ Добавить URL",
    dist_list_footer: "A:Запуск   +:Опции   Y:Сорт   -:Поиск   X:Flashpoint",
    dist_count: "{} URL",
    dist_subtitle: "Загрузка SWF с archive.org",
    dist_press_a: "Нажмите A, чтобы ввести URL",
    dist_example1: "Пример: https://archive.org/details/<item-id>",
    dist_example2: "Или просто <item-id>",
    dist_history: "История",
    dist_hint_zr: "ZR : Загрузить этот URL напрямую",
    dist_hint_a: "A  : Ввести / изменить URL (клавиатура)",
    dist_hint_lr: "L / R : Предыдущий / следующий URL",
    dist_footer_hist: "ZR:Откр  A:Изм  ZL:Удал  L/R:Нав  Y:Назад  -:Выход",
    dist_footer_nohist: "A:Ввести URL   Y:Назад   -:Выход",
    files_title: "Файлы по сети",
    files_filter: "Фильтр",
    home_folder: "Папка",
    folder_root: "Без папки",
    files_footer: "A:Загрузить   Y:Сорт   -:Поиск   L/R:Страница   B:Назад",
    dl_title: "Загрузка",
    dl_footer: "B:Отмена",
    toast_dl_ok: "{} загружено",
    toast_assets_missing: "Нет данных игры: {}. Игра может не запуститься.",
    err_title: "Ошибка",
    err_footer: "A/B:ОК",
    err_footer_fix: "A/B:ОК   Y:Исправить URL",
    settings_title: "Настройки",
    set_keys: "Клавиши по умолчанию",
    set_language: "Язык",
    set_quit: "Выход",
    set_pseudo: "Ник",
    kbd_pseudo_guide: "Имя рядом с твоими профилями в каталоге",
    set_cursor_speed: "Скорость курсора",
    set_display_mode: "Экран",
    display_fit: "Вписать",
    display_fill: "Заполнить",
    display_stretch: "Растянуть",
    set_zoom: "Масштаб",
    zoom_legend: "ZL/L: -   ZR/R: +   X/Y, крест: точно   стик/палец: кадр   2 пальца: масштаб   L3: 100 %   A: OK   B: Отмена",
    set_screen_filter: "Фильтр",
    set_home_view: "Вид главной",
    set_games_dir: "Папка игр",
    games_dir_pick: "Выбрать",
    games_dir_up: "Вверх",
    games_dir_new: "Новая папка",
    games_dir_footer: "A:Выбрать  B:Вверх  X:Новая",
    games_dir_current: "текущая",
    games_dir_confirm: "Переместить всю библиотеку сюда?",
    games_dir_confirm_n: "{} игр будет перемещено в:",
    games_dir_confirm_1: "{} игра будет перемещена в:",
    games_dir_confirm_yes: "Переместить",
    games_dir_confirm_no: "Отмена",
    kbd_newdir_guide: "Имя создаваемой папки",
    games_dir_ask_move: "Переместить игры с карты?",
    games_dir_move: "Переместить",
    games_dir_keep: "Оставить",
    games_dir_moving: "Перемещение...",
    extracting: "Извлечение...",
    opt_shortcut: "Меню HOME",
    shortcut_ok: "Значок установлен в меню HOME",
    shortcut_err: "Не удалось установить значок",
    opt_shortcut_del: "Убрать из меню HOME",
    shortcut_removed: "Значок удален",
    games_dir_moved: "Перемещено",
    home_grid: "Сетка",
    home_list: "Список",
    home_strip: "Лента",
    home_shelf: "Полка",
    set_game_prefs: "Предпочтения",
    prefs_title: "По умолчанию",
    filter_none: "Нет",
    filter_scanlines: "Линии",
    filter_crt: "CRT",
    set_pixel_filter: "Пиксели",
    pixels_auto: "Авто",
    pixels_smooth: "Сглаженные",
    pixels_sharp: "Чёткие",
    pixels_hint: "АВТО: как просит игра. ЧЁТКИЕ: без размытия.",
    set_overclock: "Разгон",
    overclock_off: "Выкл",
    overclock_on: "Вкл",
    overclock_hint: "ВКЛ: больше расход батареи и нагрев.",
    overclock_refused: "Разгон недоступен: батарея разряжена.",
    overclock_auto_hint: "АВТО: только если игра тормозит.",
    set_fps: "Счётчик FPS",
    fps_hint: "Кадров в секунду и частота, которую запрашивает игра.",
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
    set_back: "Назад",
    set_report_bug: "Сообщить об ошибке",
    tab_play: "Играть",
    tab_import: "Загрузка",
    tab_settings: "Настройки",
    set_covers: "Обложки онлайн",
    lbl_on: "On",
    lbl_off: "Off",
    opt_cover: "Обложка",
    opt_favorite: "В избранное",
    opt_unfavorite: "Из избранного",
    opt_share: "Поделиться клавишами",
    profile_shared_ok: "Ваши клавиши отправлены. Спасибо за помощь сообществу.",
    opt_apply: "Применить профиль",
    profile_title: "Профили клавиш",
    profile_copy_row: "Копировать из другой игры",
    profile_copy_none: "У других игр пока нет своих клавиш",
    profile_footer: "A:Применить   B:Назад   вверх/вниз:Нав",
    profile_none: "Для этой игры пока нет профиля. Поделитесь своим!",
    profile_catalog_offline: "Каталог недоступен. Проверьте подключение.",
    profile_revert: "Вернуть мои клавиши",
    profile_applied_ok: "Профиль применен. Ваши прежние клавиши сохранены.",
    profile_reverted_ok: "Ваши клавиши восстановлены.",
    profile_share_confirm: "Поделиться клавишами для этой игры?",
    share_confirm_update: "Обновит ваш общий профиль:",
    touches_edit: "Изменить клавиши",
    touches_footer: "A:Выбрать   B:Назад   вверх/вниз:Нав",
    touches_revert_default: "Сбросить на клавиши по умолчанию",
    profile_preview_title: "Мои → профиль",
    profile_preview_footer: "A:Применить   B:Назад",
    profile_preview_none: "Этот профиль не меняет клавиши.",
    profile_active: "(Активный)",
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
    folder_picker_footer: "A:Вкл/выкл  X:Очист  B:Назад",
    folder_del_title: "Удалить папку {}",
    profile_del_confirm: "Удалить ваш профиль?",
    profile_del_ok: "Ваш профиль удален из каталога.",
    profile_del_not_mine: "Нечего удалять: не опубликовано с этой консоли.",
    profile_del_hint: "X:Удалить",
    revert_preview_title: "Сейчас → после сброса",
    revert_preview_footer: "A:Сброс   B:Назад",
    cover_title: "Выберите обложку",
    cover_footer: "A: Выбрать   -: Поиск   вверх/вниз: Навигация   B: Назад",
    cover_show_logos: "Y: Логотипы",
    cover_show_shots: "Y: Скриншоты",
    cover_off_notice: "Включите обложки онлайн в настройках",
    cover_none: "Нет результатов",
    fp_title: "Flashpoint",
    fp_footer: "A:Скачать   X:Поиск   Y:Сорт   +:Инфо   ZL+ZR:Фильтр {}   B:Назад",
    fp_details_title: "Подробности",
    fp_details_dev: "Разработчик",
    fp_details_publisher: "Издатель",
    fp_details_date: "Выпуск",
    fp_details_size: "Размер загрузки",
    fp_details_footer: "B:Назад",
    sort_title: "Сортировка",
    sort_footer: "A:Выбрать   X:Обратно   B:Назад",
    sort_alpha: "Имя",
    sort_recent: "Добавлен",
    sort_played: "По времени",
    sort_size: "Размер",
    played_label: "Сыграно",
    sort_recent_played: "Недавно играли",
    sort_dev: "Разработчик",
    sort_source: "Источник",
    sort_files: "Число файлов",
    fav_added: "Добавлено в избранное",
    fav_removed: "Удалено из избранного",
    multifile: "Многофайловая",
    sort_dir_asc: "По возр.",
    sort_dir_desc: "По убыв.",
    settings_footer: "L/R:Вкладки   A:ОК",
    lang_title: "Язык",
    lang_footer: "A:ОК   B:Отмена",
    histdel_title: "Удалить URL?",
    url_info_type: "Тип",
    url_type_swf: "Один .swf",
    url_type_list: "Список файлов",
    url_info_files: "На SD",
    url_info_added: "Добавлено",
    histdel_msg: "Убрать этот URL из истории?",
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
    err_json_no_files: "В JSON НЕТ ПОЛЯ \"files\"",
    err_dl_start: "Не удалось начать загрузку (код {}).",
    err_dl_failed: "Загрузка не удалась (код {})",
    err_dl_cancelled: "Загрузка отменена пользователем.",
    err_url_invalid: "НЕВЕРНЫЙ URL. ОЖИДАЛСЯ URL ARCHIVE.ORG ВИДА https://archive.org/details/<id> ИЛИ ПРОСТО <id>.",
    err_no_swf: "Для этой игры не найдено файлов .swf.",
    err_dl_not_a_game: "Загрузка вернула не игровой файл. Проверьте адрес.",
    err_fp_html_game: "Эта игра запускается через HTML-страницу (FlashVars), пока не поддерживается.",
    kbd_url_header: "FlashNX - Zagruzka po seti",
    kbd_url_guide: "URL elementa archive.org (naprimer: https://archive.org/download/your-game-id)",
    kbd_rename_header: "FlashNX - Pereimenovat",
    kbd_rename_guide: "Otobrazhaemoe imya (ostavte pustym chtoby vernut imya fayla)",
    kbd_search_header: "FlashNX - Poisk",
    kbd_search_guide: "Filtr po imeni fayla (pusto = pokazat vse)",
    bug_pick_title: "Сообщить об ошибке",
    bug_pick_footer: "A:Выбрать   B:Назад   вверх/вниз:Нав",
    bug_no_games: "Нет игр для отчета. Сначала добавьте или загрузите .swf.",
    bug_ok_title: "Спасибо!",
    bug_ok_msg: "Отчет отправлен. Спасибо, что помогаете улучшить FlashNX.",
    bug_fail_title: "Ошибка",
    kbd_bug_header: "FlashNX - Soobshchit ob oshibke",
    kbd_bug_guide: "Otkroet PUBLICHNUYU issue na GitHub. Opishite problemu. @nik po zhelaniyu. Kratkiy tekhnicheskiy log prilagaetsya.",
    set_suggest: "Предложить идею",
    kbd_suggest_header: "FlashNX - Predlozhit ideyu",
    kbd_suggest_guide: "Otkroet PUBLICHNUYU issue na GitHub. Vasha ideya / predlozhenie.",
    suggest_ok_msg: "Предложение отправлено. Спасибо, что помогаете улучшить FlashNX.",
};

// German — uppercase Latin. draw_text folds ASCII to uppercase but NOT
// accented letters, so umlauts are written in their UPPERCASE form (Ä Ö Ü)
// to hit the glyphs added to the font; ß is rendered as SS (uppercase German
// convention). The kbd_* strings go to the Switch software keyboard, which
// renders full Unicode, so those keep natural mixed case + lowercase accents.
const DE: Strings = Strings {
    optimizing: "Optimierung",
    menu_resume: "Fortsetzen",
    menu_keys: "Steuerung und Cursor",
    menu_screen: "Bildschirm",
    menu_restart: "Neustart",
    menu_quit: "Beenden",
    menu_cursor: "Cursor",
    pause_title: "Pause",
    pause_footer: "A:OK   B:Abbrechen   hoch/runter:Nav",
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
    flash_backspace: "RÜcktaste",
    flash_up: "Hoch",
    flash_down: "Runter",
    flash_left: "Links",
    flash_right: "Rechts",
    empty_title: "Keine Spiele",
    empty_l1: "Lege .swf-Dateien in",
    empty_l2: "sdmc:/flashnx/",
    empty_l3: "Dann FlashNX neu starten.",
    empty_footer: "Y:Online-Import   -:Beenden",
    list_footer: "L/R:Tabs  A:Spielen  Y:Sort.  -:Suche  +:Optionen",
    applet_title: "Applet-Modus",
    applet_notice: "Ein Spiel zu starten braucht den vollen App-Speicher, den der Applet-Modus nicht hat. Halte im Homebrew-Menü R auf einem Titel (oder nutze einen Forwarder), um FlashNX mit vollem Speicher zu starten.",
    options_title: "Optionen",
    opt_keys: "Steuerung und Cursor",
    opt_rename: "Umbenennen",
    opt_edit: "Bearbeiten",
    opt_folder: "Ordner",
    opt_delete: "Löschen",
    opt_back: "Zurück",
    options_footer: "A:OK   B:Zurück",
    del_title: "Löschen?",
    del_l1: "DIE .swf-DATEI, DIE SPEICHER (.sol),",
    del_l2: "Die Steuerung und der Alias werden gelöscht.",
    del_l3: "Nicht rückgängig zu machen.",
    del_footer: "A: Löschen     B: Abbrechen",
    dist_title: "Online-Import",
    dist_search_games: "Spiel suchen",
    dist_sources: "Meine Quellen",
    dist_empty_hint: "Spiel per Name suchen oder Adresse einfügen",
    dist_add: "+ URL hinzufügen",
    dist_list_footer: "A:Starten   +:Optionen   Y:Sort.   -:Suche   X:Flashpoint",
    dist_count: "{} URL(s)",
    dist_subtitle: "SWF von archive.org herunterladen",
    dist_press_a: "Drücke A, um eine URL einzugeben",
    dist_example1: "Beispiel: https://archive.org/details/<item-id>",
    dist_example2: "Oder einfach <item-id>",
    dist_history: "Verlauf",
    dist_hint_zr: "ZR : Diese URL direkt laden",
    dist_hint_a: "A  : URL eingeben / bearbeiten (Tastatur)",
    dist_hint_lr: "L / R : Vorherige / nächste URL",
    dist_footer_hist: "ZR:Öffnen  A:Bearb.  ZL:Lösch.  L/R:Nav  Y:Lokal  -:Beenden",
    dist_footer_nohist: "A:URL eingeben   Y:Zurück lokal   -:Beenden",
    files_title: "Entfernte Dateien",
    files_filter: "Filter",
    home_folder: "Ordner",
    folder_root: "Kein Ordner",
    files_footer: "A:Laden   Y:Sort.   -:Suche   L/R:Seite   B:Zurück",
    dl_title: "Wird geladen",
    dl_footer: "B:Abbrechen",
    toast_dl_ok: "{} heruntergeladen",
    toast_assets_missing: "Spieldaten fehlen: {}. Das Spiel startet möglicherweise nicht.",
    err_title: "Fehler",
    err_footer: "A/B:OK",
    err_footer_fix: "A/B:OK   Y:URL korrigieren",
    settings_title: "Einstellungen",
    set_keys: "Standardsteuerung",
    set_language: "Sprache",
    set_quit: "Beenden",
    set_pseudo: "Spitzname",
    kbd_pseudo_guide: "Dein Name, neben deinen geteilten Profilen",
    set_cursor_speed: "Cursorgeschwindigkeit",
    set_display_mode: "Anzeige",
    display_fit: "Einpassen",
    display_fill: "Füllen",
    display_stretch: "Strecken",
    set_zoom: "Zoom",
    zoom_legend: "ZL/L: -   ZR/R: +   X/Y und Kreuz: fein   Stick/Finger: Ausschnitt   2 Finger: Zoom   L3: 100 %   A: OK   B: Abbrechen",
    set_screen_filter: "Filter",
    set_home_view: "Startansicht",
    set_games_dir: "Spiele-Ordner",
    games_dir_pick: "WÄhlen",
    games_dir_up: "ZurÜck",
    games_dir_new: "Neuer Ordner",
    games_dir_footer: "A:Wählen  B:Zurück  X:Neu",
    games_dir_current: "aktuell",
    games_dir_confirm: "Ganze Bibliothek hierher verschieben?",
    games_dir_confirm_n: "{} Spiele werden verschoben nach:",
    games_dir_confirm_1: "{} Spiel wird verschoben nach:",
    games_dir_confirm_yes: "Verschieben",
    games_dir_confirm_no: "Abbrechen",
    kbd_newdir_guide: "Name des neuen Ordners",
    games_dir_ask_move: "Spiele auf der Karte mitnehmen?",
    games_dir_move: "Verschieben",
    games_dir_keep: "Behalten",
    games_dir_moving: "Verschieben...",
    extracting: "Entpacken...",
    opt_shortcut: "HOME-Menü",
    shortcut_ok: "Kachel im HOME-Menü installiert",
    shortcut_err: "Kachel konnte nicht installiert werden",
    opt_shortcut_del: "Aus HOME-Menü entfernen",
    shortcut_removed: "Kachel entfernt",
    games_dir_moved: "Verschoben",
    home_grid: "Raster",
    home_list: "Liste",
    home_strip: "Leiste",
    home_shelf: "Regal",
    set_game_prefs: "Spielvorgaben",
    prefs_title: "Standardwerte",
    filter_none: "Keiner",
    filter_scanlines: "Linien",
    filter_crt: "CRT",
    set_pixel_filter: "Pixel",
    pixels_auto: "Auto",
    pixels_smooth: "Weich",
    pixels_sharp: "Scharf",
    pixels_hint: "AUTO: wie das Spiel es verlangt. SCHARF: keine Unschärfe.",
    set_overclock: "Overclock",
    overclock_off: "Aus",
    overclock_on: "Ein",
    overclock_hint: "EIN: mehr Akkuverbrauch, mehr Wärme.",
    overclock_refused: "Overclock nicht verfügbar: Akku zu schwach.",
    overclock_auto_hint: "AUTO: nur bei langsamen Spielen.",
    set_fps: "FPS-Anzeige",
    fps_hint: "Bilder pro Sekunde, dann die vom Spiel geforderte Rate.",
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
    set_back: "Zurück",
    set_report_bug: "Fehler melden",
    tab_play: "Spielen",
    tab_import: "Import",
    tab_settings: "Einstellungen",
    set_covers: "Online-Cover",
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
    profile_footer: "A:Anwenden   B:Zurück   hoch/runter:Nav",
    profile_none: "Noch kein Profil für dieses Spiel. Teile deins!",
    profile_catalog_offline: "Katalog nicht verfügbar. Prüfe deine Verbindung.",
    profile_revert: "Zu meiner Steuerung zurück",
    profile_applied_ok: "Profil angewendet. Deine bisherige Steuerung wurde gesichert.",
    profile_reverted_ok: "Deine Steuerung wurde wiederhergestellt.",
    profile_share_confirm: "Deine Steuerung für dieses Spiel teilen?",
    share_confirm_update: "Aktualisiert dein geteiltes Profil:",
    touches_edit: "Steuerung bearbeiten",
    touches_footer: "A:Wählen   B:Zurück   hoch/runter:Nav",
    touches_revert_default: "Auf Standard zurücksetzen",
    profile_preview_title: "Meine → Profil",
    profile_preview_footer: "A:Anwenden   B:Zurück",
    profile_preview_none: "Dieses Profil ändert keine Taste.",
    profile_active: "(Aktiv)",
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
    folder_picker_footer: "A:Ein/aus  X:Leeren  B:Zurück",
    folder_del_title: "Ordner {} löschen",
    profile_del_confirm: "Dein geteiltes Profil löschen?",
    profile_del_ok: "Dein geteiltes Profil wurde gelöscht.",
    profile_del_not_mine: "Nichts zu löschen von dieser Konsole.",
    profile_del_hint: "X:Löschen",
    revert_preview_title: "Jetzt → nach dem Zurücksetzen",
    revert_preview_footer: "A:Rücksetzen   B:Zurück",
    cover_title: "Cover wählen",
    cover_footer: "A: Wählen   -: Suche   hoch/runter: Bewegen   B: Zurück",
    cover_show_logos: "Y: Logos",
    cover_show_shots: "Y: Screenshots",
    cover_off_notice: "Online-Cover in den Einstellungen aktivieren",
    cover_none: "Keine Treffer",
    fp_title: "Flashpoint",
    fp_footer: "A:Laden   X:Suche   Y:Sort.   +:Info   ZL+ZR:Filter {}   B:Zurück",
    fp_details_title: "Details",
    fp_details_dev: "Entwickler",
    fp_details_publisher: "Herausgeber",
    fp_details_date: "Veröffentlicht",
    fp_details_size: "Download-Größe",
    fp_details_footer: "B:Zurück",
    sort_title: "Sortieren nach",
    sort_footer: "A:Wählen   X:Umkehren   B:Zurück",
    sort_alpha: "Name",
    sort_recent: "Hinzugefügt",
    sort_played: "Meist gespielt",
    sort_size: "Größe",
    played_label: "Gespielt",
    sort_recent_played: "Zuletzt gespielt",
    sort_dev: "Entwickler",
    sort_source: "Quelle",
    sort_files: "Dateianzahl",
    fav_added: "Zu Favoriten hinzugefügt",
    fav_removed: "Aus Favoriten entfernt",
    multifile: "Mehrdatei",
    sort_dir_asc: "Aufsteigend",
    sort_dir_desc: "Absteigend",
    settings_footer: "L/R:Tabs   A:OK",
    lang_title: "Sprache",
    lang_footer: "A:OK   B:Abbrechen",
    histdel_title: "URL löschen?",
    url_info_type: "Typ",
    url_type_swf: "Einzelne .swf",
    url_type_list: "Dateiliste",
    url_info_files: "Auf SD",
    url_info_added: "Hinzugefügt",
    histdel_msg: "Diese URL aus dem Verlauf entfernen?",
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
    err_json_no_files: "JSON OHNE \"files\"-FELD",
    err_dl_start: "Download konnte nicht gestartet werden (Code {}).",
    err_dl_failed: "Download fehlgeschlagen (Code {})",
    err_dl_cancelled: "Download vom Benutzer abgebrochen.",
    err_url_invalid: "UNGÜLTIGE URL. ERWARTET WIRD EINE ARCHIVE.ORG-URL WIE https://archive.org/details/<id> ODER EINFACH <id>.",
    err_no_swf: "Keine .swf-Datei für dieses Spiel gefunden.",
    err_dl_not_a_game: "Der Download hat keine Spieldatei geliefert. Prüfe die Adresse.",
    err_fp_html_game: "Dieses Spiel startet über eine HTML-Seite (FlashVars), noch nicht unterstützt.",
    kbd_url_header: "FlashNX - Online-Import",
    kbd_url_guide: "archive.org-Item-URL (z.B. https://archive.org/download/your-game-id)",
    kbd_rename_header: "FlashNX - Spiel umbenennen",
    kbd_rename_guide: "Anzeigename (leer lassen, um zum Dateinamen zurückzukehren)",
    kbd_search_header: "FlashNX - Suche",
    kbd_search_guide: "Nach Dateiname filtern (leer = alles anzeigen)",
    bug_pick_title: "Fehler melden",
    bug_pick_footer: "A:Wählen   B:Zurück   hoch/runter:Nav",
    bug_no_games: "Noch kein Spiel zu melden. Importiere oder lege zuerst eine .swf ab.",
    bug_ok_title: "Danke!",
    bug_ok_msg: "Dein Bericht wurde gesendet. Danke, dass du hilfst, FlashNX zu verbessern.",
    bug_fail_title: "Fehlgeschlagen",
    kbd_bug_header: "FlashNX - Fehler melden",
    kbd_bug_guide: "Öffnet ein öffentliches GitHub-Issue. Beschreibe das Problem. Optional: dein @Name. Ein kurzes technisches Protokoll wird angehängt.",
    set_suggest: "Vorschlag machen",
    kbd_suggest_header: "FlashNX - Vorschlag machen",
    kbd_suggest_guide: "Öffnet ein öffentliches GitHub-Issue. Deine Idee / dein Wunsch für FlashNX.",
    suggest_ok_msg: "Dein Vorschlag wurde gesendet. Danke, dass du hilfst, FlashNX zu verbessern.",
};

// Italian — uppercase Latin. Accented uppercase vowels (À È É Ì Ò Ù) are
// written directly to hit the font glyphs (draw_text does not fold accents).
const IT: Strings = Strings {
    optimizing: "Ottimizzazione",
    menu_resume: "Riprendi",
    menu_keys: "Comandi e cursore",
    menu_screen: "Schermo",
    menu_restart: "Riavvia",
    menu_quit: "Esci",
    menu_cursor: "Cursore",
    pause_title: "Pausa",
    pause_footer: "A:OK   B:Annulla   su/giù:Nav",
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
    flash_down: "GiÙ",
    flash_left: "Sinistra",
    flash_right: "Destra",
    empty_title: "Nessun gioco",
    empty_l1: "Inserisci file .swf in",
    empty_l2: "sdmc:/flashnx/",
    empty_l3: "Poi riavvia FlashNX.",
    empty_footer: "Y:Import remoto   -:Esci",
    list_footer: "L/R:Schede  A:Gioca  Y:Ordina  -:Cerca  +:Opzioni",
    applet_title: "Modalità applet",
    applet_notice: "Avviare un gioco richiede tutta la memoria dell'app, che la modalità applet non ha. Nell'homebrew menu, tieni premuto R su un titolo (o usa un forwarder) per avviare FlashNX con tutta la memoria.",
    options_title: "Opzioni",
    opt_keys: "Comandi e cursore",
    opt_rename: "Rinomina",
    opt_edit: "Modifica",
    opt_folder: "Cartella",
    opt_delete: "Elimina",
    opt_back: "Indietro",
    options_footer: "A:OK   B:Indietro",
    del_title: "Eliminare?",
    del_l1: "IL FILE .swf, I SALVATAGGI (.sol),",
    del_l2: "I comandi e l'alias saranno eliminati.",
    del_l3: "Azione irreversibile.",
    del_footer: "A: Elimina     B: Annulla",
    dist_title: "Import remoto",
    dist_search_games: "Cerca un gioco",
    dist_sources: "Le mie fonti",
    dist_empty_hint: "Cerca un gioco per nome, o incolla un indirizzo",
    dist_add: "+ Aggiungi un URL",
    dist_list_footer: "A:Avvia   +:Opzioni   Y:Ordina   -:Cerca   X:Flashpoint",
    dist_count: "{} URL",
    dist_subtitle: "Scarica SWF da archive.org",
    dist_press_a: "Premi A per inserire un URL",
    dist_example1: "Esempio: https://archive.org/details/<item-id>",
    dist_example2: "O semplicemente <item-id>",
    dist_history: "Cronologia",
    dist_hint_zr: "ZR : Carica questo URL direttamente",
    dist_hint_a: "A  : Inserisci / modifica URL (tastiera)",
    dist_hint_lr: "L / R : URL precedente / successivo",
    dist_footer_hist: "ZR:Apri  A:Mod.  ZL:Elim.  L/R:Nav  Y:Locale  -:Esci",
    dist_footer_nohist: "A:Inserisci URL   Y:Torna locale   -:Esci",
    files_title: "File remoti",
    files_filter: "Filtro",
    home_folder: "Cartella",
    folder_root: "Nessuna cartella",
    files_footer: "A:Scarica   Y:Ordina   -:Cerca   L/R:Pagina   B:Indietro",
    dl_title: "Scaricamento",
    dl_footer: "B:Annulla",
    toast_dl_ok: "{} scaricato",
    toast_assets_missing: "Dati di gioco mancanti: {}. Il gioco potrebbe non avviarsi.",
    err_title: "Errore",
    err_footer: "A/B:OK",
    err_footer_fix: "A/B:OK   Y:Correggi l'URL",
    settings_title: "Impostazioni",
    set_keys: "Comandi predefiniti",
    set_language: "Lingua",
    set_quit: "Esci",
    set_pseudo: "Soprannome",
    kbd_pseudo_guide: "Il tuo nome, accanto ai profili condivisi",
    set_cursor_speed: "Velocità cursore",
    set_display_mode: "Schermo",
    display_fit: "Adatta",
    display_fill: "Riempi",
    display_stretch: "Allarga",
    set_zoom: "Zoom",
    zoom_legend: "ZL/L: -   ZR/R: +   X/Y e croce: preciso   stick/dito: inquadra   2 dita: zoom   L3: 100 %   A: OK   B: Annulla",
    set_screen_filter: "Filtro",
    set_home_view: "Vista iniziale",
    set_games_dir: "Cartella giochi",
    games_dir_pick: "Scegli",
    games_dir_up: "Su",
    games_dir_new: "Nuova cartella",
    games_dir_footer: "A:Scegli  B:Su  X:Nuova",
    games_dir_current: "in uso",
    games_dir_confirm: "Spostare qui tutta la libreria?",
    games_dir_confirm_n: "{} giochi saranno spostati in:",
    games_dir_confirm_1: "{} gioco sarà spostato in:",
    games_dir_confirm_yes: "Sposta",
    games_dir_confirm_no: "Annulla",
    kbd_newdir_guide: "Nome della cartella da creare",
    games_dir_ask_move: "Spostare i giochi gia sulla scheda?",
    games_dir_move: "Spostali",
    games_dir_keep: "Lasciali",
    games_dir_moving: "Spostamento...",
    extracting: "Estrazione...",
    opt_shortcut: "Menu HOME",
    shortcut_ok: "Icona installata nel menu HOME",
    shortcut_err: "Impossibile installare l'icona",
    opt_shortcut_del: "Rimuovi dal menu HOME",
    shortcut_removed: "Icona rimossa",
    games_dir_moved: "Spostati",
    home_grid: "Griglia",
    home_list: "Elenco",
    home_strip: "Striscia",
    home_shelf: "Scaffale",
    set_game_prefs: "Preferenze",
    prefs_title: "Valori predefiniti",
    filter_none: "Nessuno",
    filter_scanlines: "Linee",
    filter_crt: "CRT",
    set_pixel_filter: "Pixel",
    pixels_auto: "Auto",
    pixels_smooth: "Morbidi",
    pixels_sharp: "Nitidi",
    pixels_hint: "AUTO: come chiede il gioco. NITIDI: nessuna sfocatura.",
    set_overclock: "Overclock",
    overclock_off: "Inattivo",
    overclock_on: "Attivo",
    overclock_hint: "ATTIVO: più batteria, più calore.",
    overclock_refused: "Overclock non disponibile: batteria troppo bassa.",
    overclock_auto_hint: "AUTO: solo se il gioco rallenta.",
    set_fps: "Contatore FPS",
    fps_hint: "Fotogrammi al secondo, poi la frequenza richiesta dal gioco.",
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
    set_back: "Indietro",
    set_report_bug: "Segnala un bug",
    tab_play: "Gioca",
    tab_import: "Importa",
    tab_settings: "Impostazioni",
    set_covers: "Copertine online",
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
    profile_footer: "A:Applica   B:Indietro   su/giù:Nav",
    profile_none: "Nessun profilo per questo gioco. Condividi il tuo!",
    profile_catalog_offline: "Catalogo non disponibile. Controlla la connessione.",
    profile_revert: "Torna ai miei comandi",
    profile_applied_ok: "Profilo applicato. I tuoi comandi precedenti sono salvati.",
    profile_reverted_ok: "I tuoi comandi sono stati ripristinati.",
    profile_share_confirm: "Condividere i tuoi comandi per questo gioco?",
    share_confirm_update: "Aggiorna il tuo profilo condiviso:",
    touches_edit: "Modifica comandi",
    touches_footer: "A:Scegli   B:Indietro   su/giù:Nav",
    touches_revert_default: "Ripristina comandi predefiniti",
    profile_preview_title: "Miei → profilo",
    profile_preview_footer: "A:Applica   B:Indietro",
    profile_preview_none: "Questo profilo non cambia nessun tasto.",
    profile_active: "(Attivo)",
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
    folder_picker_footer: "A:Metti/togli  X:Svuota  B:Indietro",
    folder_del_title: "Elimina la cartella {}",
    profile_del_confirm: "Eliminare il tuo profilo condiviso?",
    profile_del_ok: "Il tuo profilo condiviso è stato eliminato.",
    profile_del_not_mine: "Niente da eliminare da questa console.",
    profile_del_hint: "X:Elimina",
    revert_preview_title: "Ora → dopo il ripristino",
    revert_preview_footer: "A:Ripristina   B:Indietro",
    cover_title: "Scegli una copertina",
    cover_footer: "A: Scegli   -: Cerca   su/giù: Sposta   B: Indietro",
    cover_show_logos: "Y: Loghi",
    cover_show_shots: "Y: Schermate",
    cover_off_notice: "Attiva le copertine online nelle impostazioni",
    cover_none: "Nessun risultato",
    fp_title: "Flashpoint",
    fp_footer: "A:Scarica   X:Cerca   Y:Ordina   +:Info   ZL+ZR:Filtro {}   B:Indietro",
    fp_details_title: "Dettagli",
    fp_details_dev: "Sviluppatore",
    fp_details_publisher: "Editore",
    fp_details_date: "Uscita",
    fp_details_size: "Dimensione download",
    fp_details_footer: "B:Indietro",
    sort_title: "Ordina per",
    sort_footer: "A:Scegli   X:Inverti   B:Indietro",
    sort_alpha: "Nome",
    sort_recent: "Aggiunto",
    sort_played: "Più giocati",
    sort_size: "Dimensione",
    played_label: "Giocato",
    sort_recent_played: "Giocato di recente",
    sort_dev: "Sviluppatore",
    sort_source: "Fonte",
    sort_files: "Numero di file",
    fav_added: "Aggiunto ai preferiti",
    fav_removed: "Rimosso dai preferiti",
    multifile: "Multi-file",
    sort_dir_asc: "Crescente",
    sort_dir_desc: "Decrescente",
    settings_footer: "L/R:Schede   A:OK",
    lang_title: "Lingua",
    lang_footer: "A:OK   B:Annulla",
    histdel_title: "Eliminare l'URL?",
    url_info_type: "Tipo",
    url_type_swf: "File .swf",
    url_type_list: "Elenco di file",
    url_info_files: "Su SD",
    url_info_added: "Aggiunta il",
    histdel_msg: "Rimuovere questo URL dalla cronologia?",
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
    err_json_no_files: "JSON SENZA CAMPO \"files\"",
    err_dl_start: "Impossibile avviare il download (codice {}).",
    err_dl_failed: "Download fallito (codice {})",
    err_dl_cancelled: "Download annullato dall'utente.",
    err_url_invalid: "URL NON VALIDO. ATTESO UN URL DI ARCHIVE.ORG TIPO https://archive.org/details/<id> O SEMPLICEMENTE <id>.",
    err_no_swf: "Nessun file .swf trovato per questo gioco.",
    err_dl_not_a_game: "Il download non ha restituito un file di gioco. Controlla l'indirizzo.",
    err_fp_html_game: "Questo gioco si avvia da una pagina HTML (FlashVars), non ancora supportato.",
    kbd_url_header: "FlashNX - Import remoto",
    kbd_url_guide: "URL dell'elemento archive.org (es: https://archive.org/download/your-game-id)",
    kbd_rename_header: "FlashNX - Rinomina gioco",
    kbd_rename_guide: "Nome visualizzato (lascia vuoto per tornare al nome del file)",
    kbd_search_header: "FlashNX - Cerca",
    kbd_search_guide: "Filtra per nome file (vuoto = mostra tutto)",
    bug_pick_title: "Segnala un bug",
    bug_pick_footer: "A:Scegli   B:Indietro   su/giù:Nav",
    bug_no_games: "Nessun gioco da segnalare. Importa o inserisci prima un .swf.",
    bug_ok_title: "Grazie!",
    bug_ok_msg: "La tua segnalazione è stata inviata. Grazie per aiutare a migliorare FlashNX.",
    bug_fail_title: "Fallito",
    kbd_bug_header: "FlashNX - Segnala un bug",
    kbd_bug_guide: "Apre una issue PUBBLICA su GitHub. Descrivi il problema. Facoltativo: il tuo @nome. Viene allegato un breve log tecnico.",
    set_suggest: "Proponi un'idea",
    kbd_suggest_header: "FlashNX - Proponi un'idea",
    kbd_suggest_guide: "Apre una issue PUBBLICA su GitHub. La tua idea / proposta per FlashNX.",
    suggest_ok_msg: "La tua proposta è stata inviata. Grazie per aiutare a migliorare FlashNX.",
};

// Portuguese (Brazil) — uppercase Latin. Accented uppercase letters
// (Á Â Ã É Ê Í Ó Ô Õ Ú Ç) are written directly to hit the font glyphs.
const PT: Strings = Strings {
    optimizing: "Otimizando",
    menu_resume: "Continuar",
    menu_keys: "Controles e cursor",
    menu_screen: "Tela",
    menu_restart: "Reiniciar",
    menu_quit: "Sair",
    menu_cursor: "Cursor",
    pause_title: "Pausa",
    pause_footer: "A:OK   B:Cancelar   cima/baixo:Nav",
    keys_title: "Controles",
    keys_footer: "A:Editar  L/R:Modo  X:J1/J2  B:Voltar",
    keys_dropdown_footer: "A:OK   B:Cancelar   cima/baixo:Nav",
    keys_modifier: "Modificador",
    none: "(nenhuma)",
    flash_mouse_left: "Clique esquerdo",
    flash_mouse_right: "Clique direito",
    flash_keyboard: "Teclado",
    flash_space: "EspaÇo",
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
    empty_l2: "sdmc:/flashnx/",
    empty_l3: "Depois reinicie o FlashNX.",
    empty_footer: "Y:Importar remoto   -:Sair",
    list_footer: "L/R:Abas  A:Jogar  Y:Ordenar  -:Buscar  +:Opções",
    applet_title: "Modo applet",
    applet_notice: "Iniciar um jogo precisa de toda a memória do app, que o modo applet não tem. No homebrew menu, segure R sobre um título (ou use um forwarder) para iniciar o FlashNX com toda a memória.",
    options_title: "Opções",
    opt_keys: "Controles e cursor",
    opt_rename: "Renomear",
    opt_edit: "Editar",
    opt_folder: "Pasta",
    opt_delete: "Excluir",
    opt_back: "Voltar",
    options_footer: "A:OK   B:Voltar",
    del_title: "Excluir?",
    del_l1: "O ARQUIVO .swf, OS SAVES (.sol),",
    del_l2: "Os controles e o alias serão apagados.",
    del_l3: "Ação irreversível.",
    del_footer: "A: Excluir     B: Cancelar",
    dist_title: "Importar remoto",
    dist_search_games: "Procurar um jogo",
    dist_sources: "Minhas fontes",
    dist_empty_hint: "Procure um jogo pelo nome, ou cole um endereço",
    dist_add: "+ Adicionar uma URL",
    dist_list_footer: "A:Iniciar   +:Opções   Y:Ordenar   -:Buscar   X:Flashpoint",
    dist_count: "{} URL(s)",
    dist_subtitle: "Baixar SWF do archive.org",
    dist_press_a: "Pressione A para inserir uma URL",
    dist_example1: "Exemplo: https://archive.org/details/<item-id>",
    dist_example2: "Ou simplesmente <item-id>",
    dist_history: "Histórico",
    dist_hint_zr: "ZR : Carregar esta URL diretamente",
    dist_hint_a: "A  : Inserir / editar URL (teclado)",
    dist_hint_lr: "L / R : URL anterior / seguinte",
    dist_footer_hist: "ZR:Abrir  A:Editar  ZL:Excluir  L/R:Nav  Y:Local  -:Sair",
    dist_footer_nohist: "A:Inserir URL   Y:Voltar local   -:Sair",
    files_title: "Arquivos remotos",
    files_filter: "Filtro",
    home_folder: "Pasta",
    folder_root: "Sem pasta",
    files_footer: "A:Baixar   Y:Ordenar   -:Buscar   L/R:Página   B:Voltar",
    dl_title: "Baixando",
    dl_footer: "B:Cancelar",
    toast_dl_ok: "{} baixado",
    toast_assets_missing: "Dados do jogo ausentes: {}. O jogo pode não iniciar.",
    err_title: "Erro",
    err_footer: "A/B:OK",
    err_footer_fix: "A/B:OK   Y:Corrigir a URL",
    settings_title: "Ajustes",
    set_keys: "Controles padrão",
    set_language: "Idioma",
    set_quit: "Sair",
    set_pseudo: "Apelido",
    kbd_pseudo_guide: "Seu nome, ao lado dos perfis compartilhados",
    set_cursor_speed: "Velocidade do cursor",
    set_display_mode: "Tela",
    display_fit: "Ajustar",
    display_fill: "Preencher",
    display_stretch: "Esticar",
    set_zoom: "Zoom",
    zoom_legend: "ZL/L: -   ZR/R: +   X/Y e cruz: preciso   stick/dedo: enquadrar   2 dedos: zoom   L3: 100 %   A: OK   B: Cancelar",
    set_screen_filter: "Filtro",
    set_home_view: "Vista início",
    set_games_dir: "Pasta de jogos",
    games_dir_pick: "Escolher",
    games_dir_up: "Subir",
    games_dir_new: "Nova pasta",
    games_dir_footer: "A:Escolher  B:Subir  X:Nova",
    games_dir_current: "em uso",
    games_dir_confirm: "Mover toda a biblioteca para ca?",
    games_dir_confirm_n: "{} jogos serao movidos para:",
    games_dir_confirm_1: "{} jogo sera movido para:",
    games_dir_confirm_yes: "Mover",
    games_dir_confirm_no: "Cancelar",
    kbd_newdir_guide: "Nome da pasta a criar",
    games_dir_ask_move: "Mover os jogos ja no cartao?",
    games_dir_move: "Mover",
    games_dir_keep: "Deixar",
    games_dir_moving: "Movendo...",
    extracting: "Extraindo...",
    opt_shortcut: "Menu HOME",
    shortcut_ok: "Ícone instalado no menu HOME",
    shortcut_err: "Não foi possível instalar o ícone",
    opt_shortcut_del: "Remover do menu HOME",
    shortcut_removed: "Ícone removido",
    games_dir_moved: "Movidos",
    home_grid: "Grade",
    home_list: "Lista",
    home_strip: "Faixa",
    home_shelf: "Prateleira",
    set_game_prefs: "Preferências",
    prefs_title: "Padrões",
    filter_none: "Nenhum",
    filter_scanlines: "Linhas",
    filter_crt: "CRT",
    set_pixel_filter: "Pixels",
    pixels_auto: "Auto",
    pixels_smooth: "Suaves",
    pixels_sharp: "Nítidos",
    pixels_hint: "AUTO: como o jogo pede. NÍTIDOS: sem desfoque.",
    set_overclock: "Overclock",
    overclock_off: "Inativo",
    overclock_on: "Ativo",
    overclock_hint: "ATIVO: mais bateria, mais calor.",
    overclock_refused: "Overclock indisponível: bateria fraca.",
    overclock_auto_hint: "AUTO: só se o jogo ficar lento.",
    set_fps: "Contador FPS",
    fps_hint: "Quadros por segundo, e a taxa que o jogo pede.",
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
    set_back: "Voltar",
    set_report_bug: "Relatar um bug",
    tab_play: "Jogar",
    tab_import: "Importar",
    tab_settings: "Ajustes",
    set_covers: "Capas online",
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
    profile_footer: "A:Aplicar   B:Voltar   cima/baixo:Nav",
    profile_none: "Ainda sem perfil para este jogo. Compartilhe o seu!",
    profile_catalog_offline: "Catálogo indisponível. Verifique sua conexão.",
    profile_revert: "Voltar aos meus controles",
    profile_applied_ok: "Perfil aplicado. Seus controles anteriores foram salvos.",
    profile_reverted_ok: "Seus controles foram restaurados.",
    profile_share_confirm: "Compartilhar seus controles para este jogo?",
    share_confirm_update: "Atualiza seu perfil compartilhado:",
    touches_edit: "Editar controles",
    touches_footer: "A:Escolher   B:Voltar   cima/baixo:Nav",
    touches_revert_default: "Restaurar controles padrão",
    profile_preview_title: "Meus → perfil",
    profile_preview_footer: "A:Aplicar   B:Voltar",
    profile_preview_none: "Este perfil não muda nenhuma tecla.",
    profile_active: "(Ativo)",
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
    folder_picker_footer: "A:Por/tirar  X:Esvaziar  B:Voltar",
    folder_del_title: "Excluir a pasta {}",
    profile_del_confirm: "Remover seu perfil compartilhado?",
    profile_del_ok: "Seu perfil compartilhado foi removido.",
    profile_del_not_mine: "Nada a remover deste console.",
    profile_del_hint: "X:Remover",
    revert_preview_title: "Agora → após reverter",
    revert_preview_footer: "A:Reverter   B:Voltar",
    cover_title: "Escolher uma capa",
    cover_footer: "A: Escolher   -: Buscar   cima/baixo: Mover   B: Voltar",
    cover_show_logos: "Y: Logos",
    cover_show_shots: "Y: Capturas",
    cover_off_notice: "Ative as capas online nos ajustes",
    cover_none: "Sem resultados",
    fp_title: "Flashpoint",
    fp_footer: "A:Baixar   X:Buscar   Y:Ordenar   +:Info   ZL+ZR:Filtro {}   B:Voltar",
    fp_details_title: "Detalhes",
    fp_details_dev: "Desenvolvedor",
    fp_details_publisher: "Distribuidora",
    fp_details_date: "Lançamento",
    fp_details_size: "Tamanho do download",
    fp_details_footer: "B:Voltar",
    sort_title: "Ordenar por",
    sort_footer: "A:Escolher   X:Inverter   B:Voltar",
    sort_alpha: "Nome",
    sort_recent: "Adicionado",
    sort_played: "Mais jogados",
    sort_size: "Tamanho",
    played_label: "Jogado",
    sort_recent_played: "Jogado por último",
    sort_dev: "Desenvolvedor",
    sort_source: "Fonte",
    sort_files: "Número de arquivos",
    fav_added: "Adicionado aos favoritos",
    fav_removed: "Removido dos favoritos",
    multifile: "Multiarquivo",
    sort_dir_asc: "Crescente",
    sort_dir_desc: "Decrescente",
    settings_footer: "L/R:Abas   A:OK",
    lang_title: "Idioma",
    lang_footer: "A:OK   B:Cancelar",
    histdel_title: "Excluir a URL?",
    url_info_type: "Tipo",
    url_type_swf: "Arquivo .swf",
    url_type_list: "Lista de arquivos",
    url_info_files: "No SD",
    url_info_added: "Adicionada em",
    histdel_msg: "Remover esta URL do histórico?",
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
    err_json_no_files: "JSON SEM O CAMPO \"files\"",
    err_dl_start: "Não foi possível iniciar o download (código {}).",
    err_dl_failed: "Falha no download (código {})",
    err_dl_cancelled: "Download cancelado pelo usuário.",
    err_url_invalid: "URL INVÁLIDA. ESPERADA UMA URL DO ARCHIVE.ORG TIPO https://archive.org/details/<id> OU SIMPLESMENTE <id>.",
    err_no_swf: "Nenhum arquivo .swf encontrado para este jogo.",
    err_dl_not_a_game: "O download não retornou um arquivo de jogo. Verifique o endereço.",
    err_fp_html_game: "Este jogo inicia por uma página HTML (FlashVars), ainda não suportado.",
    kbd_url_header: "FlashNX - Importar remoto",
    kbd_url_guide: "URL do item do archive.org (ex: https://archive.org/download/your-game-id)",
    kbd_rename_header: "FlashNX - Renomear jogo",
    kbd_rename_guide: "Nome de exibição (deixe vazio para voltar ao nome do arquivo)",
    kbd_search_header: "FlashNX - Buscar",
    kbd_search_guide: "Filtrar por nome de arquivo (vazio = mostrar tudo)",
    bug_pick_title: "Relatar um bug",
    bug_pick_footer: "A:Escolher   B:Voltar   cima/baixo:Nav",
    bug_no_games: "Nenhum jogo para relatar. Importe ou coloque um .swf primeiro.",
    bug_ok_title: "Obrigado!",
    bug_ok_msg: "Seu relato foi enviado. Obrigado por ajudar a melhorar o FlashNX.",
    bug_fail_title: "Falhou",
    kbd_bug_header: "FlashNX - Relatar um bug",
    kbd_bug_guide: "Abre uma issue PÚBLICA no GitHub. Descreva o problema. Opcional: seu @usuário. Um breve log tecnico e anexado.",
    set_suggest: "Fazer uma sugestão",
    kbd_suggest_header: "FlashNX - Fazer uma sugestão",
    kbd_suggest_guide: "Abre uma issue PÚBLICA no GitHub. Sua ideia / sugestão para o FlashNX.",
    suggest_ok_msg: "Sua sugestão foi enviada. Obrigado por ajudar a melhorar o FlashNX.",
};

// Simplified Chinese (issue #41). CJK has no case, so unlike the Latin tables
// these are written verbatim; they render through the shared-font glyph atlas
// (backend/glyphs.rs), while the ASCII parts (button names, paths, URLs) still
// come from the 5x7 bitmap font (folded to uppercase). Button-hint footers keep
// ASCII ":" / spacing for parity with the other locales; prose uses Chinese
// punctuation. The kbd_* strings go to the Switch software keyboard (full
// Unicode), so they read as natural mixed Chinese + Latin.
const ZH: Strings = Strings {
    optimizing: "优化中",
    menu_resume: "继续",
    menu_keys: "按键与光标",
    menu_screen: "屏幕",
    menu_restart: "重新开始",
    menu_quit: "退出",
    menu_cursor: "光标",
    pause_title: "暂停",
    pause_footer: "A:确定   B:取消   上/下:导航",
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
    empty_l2: "SDMC:/FLASHNX/",
    empty_l3: "然后重启 FLASHNX。",
    empty_footer: "Y:在线导入   -:退出",
    list_footer: "L/R:标签  A:开始  Y:排序  -:搜索  +:选项",
    applet_title: "小程序模式",
    applet_notice: "启动游戏需要应用的全部内存，小程序模式没有。在自制软件菜单中，按住 R 选择标题（或使用转发器）以全内存启动 FLASHNX。",
    options_title: "选项",
    opt_keys: "按键与光标",
    opt_rename: "重命名",
    opt_edit: "编辑",
    opt_folder: "文件夹",
    opt_delete: "删除",
    opt_back: "返回",
    options_footer: "A:确定   B:返回",
    del_title: "删除？",
    del_l1: ".swf 文件、存档 (.sol)、",
    del_l2: "按键和别名都将被删除。",
    del_l3: "此操作无法撤销。",
    del_footer: "A: 删除     B: 取消",
    dist_title: "在线导入",
    dist_search_games: "搜索游戏",
    dist_sources: "我的来源",
    dist_empty_hint: "按名称搜索游戏，或粘贴网址",
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
    home_folder: "文件夹",
    folder_root: "无文件夹",
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
    set_zoom: "缩放",
    zoom_legend: "ZL/L: -   ZR/R: +   X/Y 和方向键: 微调   摇杆/触摸: 取景   双指: 缩放   L3: 100 %   A: 确定   B: 取消",
    set_screen_filter: "滤镜",
    set_home_view: "主页视图",
    set_games_dir: "游戏文件夹",
    games_dir_pick: "选择",
    games_dir_up: "上一级",
    games_dir_new: "新建文件夹",
    games_dir_footer: "A:选择  B:上一级  X:新建",
    games_dir_current: "使用中",
    games_dir_confirm: "将整个游戏库移动到这里？",
    games_dir_confirm_n: "{} 个游戏将移动到：",
    games_dir_confirm_1: "{} 个游戏将移动到：",
    games_dir_confirm_yes: "移动",
    games_dir_confirm_no: "取消",
    kbd_newdir_guide: "要创建的文件夹名称",
    games_dir_ask_move: "移动卡上已有的游戏？",
    games_dir_move: "移动",
    games_dir_keep: "保留",
    games_dir_moving: "移动中…",
    extracting: "解压中…",
    opt_shortcut: "主页菜单",
    shortcut_ok: "图标已安装到主页菜单",
    shortcut_err: "无法安装图标",
    opt_shortcut_del: "从主页菜单移除",
    shortcut_removed: "图标已移除",
    games_dir_moved: "已移动",
    home_grid: "网格",
    home_list: "列表",
    home_strip: "横条",
    home_shelf: "书架",
    set_game_prefs: "游戏默认",
    prefs_title: "默认设置",
    filter_none: "无",
    filter_scanlines: "扫描线",
    filter_crt: "CRT",
    set_pixel_filter: "像素",
    pixels_auto: "自动",
    pixels_smooth: "平滑",
    pixels_sharp: "锐利",
    pixels_hint: "自动：按游戏的要求。锐利：不模糊。",
    set_overclock: "超频",
    overclock_off: "关闭",
    overclock_on: "开启",
    overclock_hint: "开启：更耗电，发热更多。",
    overclock_refused: "超频不可用：电量过低。",
    overclock_auto_hint: "自动：仅在游戏变慢时提速。",
    set_fps: "帧率显示",
    fps_hint: "每秒帧数，以及游戏要求的帧率。",
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
    profile_copy_row: "从其他游戏复制",
    profile_copy_none: "还没有其他游戏设置过自己的按键",
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
    profile_preview_title: "我的 → 配置",
    profile_preview_footer: "A:应用   B:返回",
    profile_preview_none: "此配置不会改变任何按键。",
    profile_active: "(使用中)",
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
    folder_picker_footer: "A:加入/移出  X:清空  B:返回",
    folder_del_title: "删除文件夹 {}",
    profile_del_confirm: "删除你分享的配置？",
    profile_del_ok: "你分享的配置已从目录中删除。",
    profile_del_not_mine: "这台主机没有可删除的内容。",
    profile_del_hint: "X:删除",
    revert_preview_title: "当前 → 还原后",
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
    err_https: "传输失败 ({})。请稍后重试。",
    err_offline: "找不到地址 (CURL {})。请检查主机的 WIFI，然后检查 DNS。",
    err_unreachable: "服务器拒绝了连接 (CURL {})。主机在线，中间有东西在阻断。",
    err_timeout: "服务器未及时响应。请重试。",
    err_tls: "安全连接被拒绝。请检查主机的日期和时间：时钟错误会导致 HTTPS 失败。",
    err_response_big: "响应过大。请使用更精确的搜索。",
    err_sd_write: "无法写入 SD 卡。可能已满或被写保护。",
    err_http_status: "服务器拒绝了请求 (HTTP {})。请稍后重试。",
    err_server_busy: "服务器过载 (HTTP {})。链接没有问题，请过几分钟再试。",
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
    kbd_bug_guide: "将打开一个公开的 GitHub issue。请描述问题。可选：你的 @用户名。将附带一份简短的技术日志。",
    set_suggest: "提出建议",
    kbd_suggest_header: "FlashNX - 提出建议",
    kbd_suggest_guide: "将打开一个公开的 GitHub issue。你对 FlashNX 的想法 / 建议。",
    suggest_ok_msg: "你的建议已发送。感谢你帮助改进 FLASHNX。",
};

// Turkish. Drawn strings are UPPERCASE with only the caps our 5x7 font carries
// (Ç Ö Ü + the added Ğ/Ş); the dotted capital I is
// rendered as a plain ASCII "I" (standard all-caps relaxation, avoids a glyph).
// The `kbd_*` guides go to the Switch software keyboard, which renders full
// Turkish diacritics, so those keep proper lowercase spelling.
const TR: Strings = Strings {
    optimizing: "Optimize ediliyor",
    menu_resume: "Devam et",
    menu_keys: "Kontroller ve imleç",
    menu_screen: "Ekran",
    menu_restart: "Yeniden başlat",
    menu_quit: "Çıkış",
    menu_cursor: "İmleç",
    pause_title: "Duraklatıldı",
    pause_footer: "A:Tamam   B:İptal   yukarı/aşağı",
    keys_title: "Kontroller",
    keys_footer: "A:Düzenle  L/R:Mod  X:O1/O2  B:Geri",
    keys_dropdown_footer: "A:Tamam   B:İptal   yukarı/aşağı",
    keys_modifier: "Değiştirici",
    none: "(yok)",
    flash_mouse_left: "Sol tik",
    flash_mouse_right: "SaĞ tik",
    flash_keyboard: "Klavye",
    flash_space: "BoŞluk",
    flash_enter: "Enter",
    flash_escape: "Esc",
    flash_shift: "Shift",
    flash_control: "Ctrl",
    flash_alt: "Alt",
    flash_tab: "Tab",
    flash_backspace: "Geri sil",
    flash_up: "Yukari",
    flash_down: "AŞaĞi",
    flash_left: "Sol",
    flash_right: "SaĞ",
    empty_title: "Oyun yok",
    empty_l1: ".swf dosyalarını şuraya koyun",
    empty_l2: "sdmc:/flashnx/",
    empty_l3: "Sonra FlashNX'ı yeniden başlatın.",
    empty_footer: "Y:Uzaktan aktar   -:Çıkış",
    list_footer: "L/R:Sekmeler  A:Oyna  Y:Sırala  -:Ara  +:Seçenek",
    applet_title: "Applet modu",
    applet_notice: "Bir oyun başlatmak uygulamanın tüm belleğini gerektirir, applet modunda bu bellek yoktur. Homebrew menüsünde bir oyuna (veya forwarder'a) R tuşunu basılı tutarak FlashNX'ı tam bellekle başlatın.",
    options_title: "Seçenekler",
    opt_keys: "Kontroller ve imleç",
    opt_rename: "Yeniden adlandır",
    opt_edit: "Düzenle",
    opt_folder: "Klasör",
    opt_delete: "Sil",
    opt_back: "Geri",
    options_footer: "A:Tamam   B:Geri",
    del_title: "Silinsin mi?",
    del_l1: ".swf DOSYASI, KAYITLAR (.sol),",
    del_l2: "Kontroller ve takma ad silinecek.",
    del_l3: "Bu geri alınamaz.",
    del_footer: "A: Sil     B: İptal",
    dist_title: "Uzaktan aktar",
    dist_search_games: "Oyun ara",
    dist_sources: "Kaynaklarım",
    dist_empty_hint: "Oyunu adıyla arayın veya bir adres yapıştırın",
    dist_add: "+ URL ekle",
    dist_list_footer: "A:Başlat   +:Seçenek   Y:Sırala   -:Ara   X:Flashpoint",
    dist_count: "{} URL",
    dist_subtitle: "archive.org'dan SWF indir",
    dist_press_a: "URL girmek için A'ya basın",
    dist_example1: "Örnek: https://archive.org/details/<item-id>",
    dist_example2: "Veya sadece <item-id>",
    dist_history: "Geçmiş",
    dist_hint_zr: "ZR : Bu URL'yi doğrudan yükle",
    dist_hint_a: "A  : URL gir / düzenle (klavye)",
    dist_hint_lr: "L / R : Önceki / sonraki URL",
    dist_footer_hist: "ZR:Aç  A:Düzenle  ZL:Sil  L/R:Gez  Y:Yerel  -:Çık",
    dist_footer_nohist: "A:URL gir   Y:Yerel   -:Çıkış",
    files_title: "Uzak dosyalar",
    files_filter: "Filtre",
    home_folder: "Klasör",
    folder_root: "Klasörsüz",
    files_footer: "A:İndir   Y:Sırala   -:Ara   L/R:Sayfa   B:Geri",
    dl_title: "İndiriliyor",
    dl_footer: "B:İptal",
    toast_dl_ok: "{} indirildi",
    toast_assets_missing: "Oyun verileri eksik: {}. Oyun başlamayabilir.",
    err_title: "Hata",
    err_footer: "A/B:Tamam",
    err_footer_fix: "A/B:Tamam   Y:URL'yi düzelt",
    settings_title: "Ayarlar",
    set_keys: "Varsayılan kontroller",
    set_language: "Dil",
    set_quit: "Çıkış",
    set_pseudo: "Takma ad",
    kbd_pseudo_guide: "Paylaştığın profillerde görünen adın",
    set_cursor_speed: "İmleç hızı",
    set_display_mode: "Ekran",
    display_fit: "Sığdır",
    display_fill: "Doldur",
    display_stretch: "Ger",
    set_zoom: "Zoom",
    zoom_legend: "ZL/L: -   ZR/R: +   X/Y ve yön tuşu: hassas   çubuk/dokunma: kadraj   2 parmak: zoom   L3: 100 %   A: Tamam   B: İptal",
    set_screen_filter: "Filtre",
    set_home_view: "Ana ekran",
    set_games_dir: "Oyun klasörü",
    games_dir_pick: "Sec",
    games_dir_up: "Yukari",
    games_dir_new: "Yeni klasor",
    games_dir_footer: "A:Seç  B:Yukarı  X:Yeni",
    games_dir_current: "kullanimda",
    games_dir_confirm: "Tum kutuphane buraya tasinsin mi?",
    games_dir_confirm_n: "{} oyun şuraya taşınacak:",
    games_dir_confirm_1: "{} oyun şuraya taşınacak:",
    games_dir_confirm_yes: "Tasi",
    games_dir_confirm_no: "Iptal",
    kbd_newdir_guide: "Olusturulacak klasorun adi",
    games_dir_ask_move: "Karttaki oyunlar tasinsin mi?",
    games_dir_move: "Tasi",
    games_dir_keep: "Birak",
    games_dir_moving: "Tasiniyor...",
    extracting: "Cikartiliyor...",
    opt_shortcut: "Ana menü",
    shortcut_ok: "Kutucuk ana menüye eklendi",
    shortcut_err: "Kutucuk yüklenemedi",
    opt_shortcut_del: "Ana menüden kaldır",
    shortcut_removed: "Kutucuk kaldırıldı",
    games_dir_moved: "Tasindi",
    home_grid: "Izgara",
    home_list: "Liste",
    home_strip: "Şerit",
    home_shelf: "Raf",
    set_game_prefs: "Oyun varsayılanları",
    prefs_title: "Varsayılanlar",
    filter_none: "Yok",
    filter_scanlines: "Çizgiler",
    filter_crt: "CRT",
    set_pixel_filter: "Pikseller",
    pixels_auto: "Otomatik",
    pixels_smooth: "Yumuşak",
    pixels_sharp: "Keskin",
    pixels_hint: "OTOMATİK: oyunun istediği gibi. KESKİN: bulanıklık yok.",
    set_overclock: "Overclock",
    overclock_off: "Kapalı",
    overclock_on: "Açık",
    overclock_hint: "AÇIK: daha çok pil, daha çok ısı.",
    overclock_refused: "Overclock kullanılamaz: pil çok düşük.",
    overclock_auto_hint: "OTOMATİK: yalnızca oyun yavaşlarsa.",
    set_fps: "FPS sayacı",
    fps_hint: "Saniyedeki kare sayısı ve oyunun istediği hız.",
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
    set_back: "Geri",
    set_report_bug: "Hata bildir",
    tab_play: "Oyna",
    tab_import: "Aktar",
    tab_settings: "Ayarlar",
    set_covers: "Çevrimiçi kapaklar",
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
    profile_footer: "A:Uygula   B:Geri   yuk/aşa:gez",
    profile_none: "Bu oyun için henüz profil yok. Kendininkini paylaş!",
    profile_catalog_offline: "Katalog kullanılamıyor. Bağlantını kontrol et.",
    profile_revert: "Kendi kontrollerime dön",
    profile_applied_ok: "Profil uygulandı. Önceki kontrollerin kaydedildi.",
    profile_reverted_ok: "Kontrollerin geri yüklendi.",
    profile_share_confirm: "Bu oyun için kontrollerin paylaşılsın mı?",
    share_confirm_update: "Paylaşılan profilini günceller:",
    touches_edit: "Kontrolleri düzenle",
    touches_footer: "A:Seç   B:Geri   yuk/aşa:gez",
    touches_revert_default: "Varsayılan kontrollere dön",
    profile_preview_title: "Benimki → profil",
    profile_preview_footer: "A:Uygula   B:Geri",
    profile_preview_none: "Bu profil hiçbir tuşunu değiştirmez.",
    profile_active: "(Etkin)",
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
    folder_picker_footer: "A:Ekle/çıkar  X:Boşalt  B:Geri",
    folder_del_title: "{} klasörünü sil",
    profile_del_confirm: "Paylaşılan profilin silinsin mi?",
    profile_del_ok: "Paylaşılan profilin silindi.",
    profile_del_not_mine: "Silinecek bir şey yok: bu konsoldan paylaşılmadı.",
    profile_del_hint: "X:Sil",
    revert_preview_title: "Şimdi → geri alınca",
    revert_preview_footer: "A:Geri al   B:Geri",
    cover_title: "Kapak seç",
    cover_footer: "A:Seç   -:Ara   yuk/aşa:gez   B:Geri",
    cover_show_logos: "Y: Logolar",
    cover_show_shots: "Y: Ekran imgeleri",
    cover_off_notice: "Ayarlardan çevrimiçi kapakları açın",
    cover_none: "Sonuç yok",
    fp_title: "Flashpoint",
    fp_footer: "A:İndir  X:Ara  Y:Sırala  +:Bilgi  ZL+ZR:Filtre {}  B:Geri",
    fp_details_title: "Ayrıntılar",
    fp_details_dev: "Geliştirici",
    fp_details_publisher: "Yayıncı",
    fp_details_date: "Çıkış tarihi",
    fp_details_size: "İndirme boyutu",
    fp_details_footer: "B:Geri",
    sort_title: "Sıralama",
    sort_footer: "A:Seç   X:Ters   B:Geri",
    sort_alpha: "Ad",
    sort_recent: "Eklenme",
    sort_played: "En çok oynanan",
    sort_size: "Boyut",
    played_label: "Oynandı",
    sort_recent_played: "Son oynanan",
    sort_dev: "Geliştirici",
    sort_source: "Kaynak",
    sort_files: "Dosya sayısı",
    fav_added: "Favorilere eklendi",
    fav_removed: "Favorilerden çıkarıldı",
    multifile: "Çok dosyalı",
    sort_dir_asc: "Artan",
    sort_dir_desc: "Azalan",
    settings_footer: "L/R:Sekmeler   A:Tamam",
    lang_title: "Dil",
    lang_footer: "A:Tamam   B:İptal",
    histdel_title: "URL silinsin mi?",
    url_info_type: "Tür",
    url_type_swf: "Tek .swf",
    url_type_list: "Dosya listesi",
    url_info_files: "SD kartta",
    url_info_added: "Eklendi",
    histdel_msg: "Bu URL geçmişten silinsin mi?",
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
    err_json_no_files: "JSON'DA \"files\" ALANI YOK",
    err_dl_start: "İndirme başlatılamadı (kod {}).",
    err_dl_failed: "İndirme başarısız (kod {})",
    err_dl_cancelled: "İndirme kullanıcı tarafından iptal edildi.",
    err_url_invalid: "GEÇERSİZ URL. https://archive.org/details/<id> GİBİ BİR URL VEYA SADECE <id> BEKLENİYOR.",
    err_no_swf: "Bu oyun için .swf dosyası bulunamadı.",
    err_dl_not_a_game: "İndirme bir oyun dosyası döndürmedi. Adresi kontrol edin.",
    err_fp_html_game: "Bu oyun bir HTML sayfasından (FlashVars) başlar, henüz desteklenmiyor.",
    kbd_url_header: "FlashNX - Uzaktan içe aktarma",
    kbd_url_guide: "archive.org öğe URL'si (örn. https://archive.org/download/oyun-kimligi)",
    kbd_rename_header: "FlashNX - Oyunu yeniden adlandır",
    kbd_rename_guide: "Görünen ad (dosya adına dönmek için boş bırakın)",
    kbd_search_header: "FlashNX - Ara",
    kbd_search_guide: "Dosya adına göre filtrele (boş = hepsini göster)",
    bug_pick_title: "Hata bildir",
    bug_pick_footer: "A:Seç   B:Geri   yuk/aşa:gez",
    bug_no_games: "Bildirilecek oyun yok. Önce bir .swf aktarın.",
    bug_ok_title: "Teşekkürler!",
    bug_ok_msg: "Raporun gönderildi. FlashNX'ı geliştirmeye yardım için teşekkürler.",
    bug_fail_title: "Başarısız",
    kbd_bug_header: "FlashNX - Hata bildir",
    kbd_bug_guide: "Herkese açık bir GitHub konusu açar. Sorunu anlat. İsteğe bağlı: @kullanıcı adın. Kısa bir teknik günlük eklenir.",
    set_suggest: "Öneride bulun",
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
pub fn cycle_default_zoom() {
    let cur = default_zoom();
    let idx = DEFAULT_ZOOM_STEPS.iter().position(|v| *v == cur);
    let next = match idx {
        Some(i) => DEFAULT_ZOOM_STEPS[(i + 1) % DEFAULT_ZOOM_STEPS.len()],
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
        "{{\n    \"language\": \"{}\",\n    \"covers_online\": {},\n    \"display_mode\": {},\n    \"rotation\": {},\n    \"zoom\": {},\n    \"pixel_filter\": {},\n    \"screen_filter\": {},\n    \"overclock\": {},\n    \"fps_counter\": {},\n    \"home_view\": {}\n}}\n",
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
