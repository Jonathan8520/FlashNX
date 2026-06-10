//! UI string localization (FR / EN / ES / RU).
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
}

impl Lang {
    pub fn index(self) -> usize {
        match self {
            Lang::En => 0,
            Lang::Fr => 1,
            Lang::Es => 2,
            Lang::Ru => 3,
        }
    }
    pub fn from_index(i: usize) -> Lang {
        match i {
            1 => Lang::Fr,
            2 => Lang::Es,
            3 => Lang::Ru,
            _ => Lang::En,
        }
    }
    pub fn code(self) -> &'static str {
        match self {
            Lang::En => "en",
            Lang::Fr => "fr",
            Lang::Es => "es",
            Lang::Ru => "ru",
        }
    }
    pub fn from_code(s: &str) -> Option<Lang> {
        match s {
            "en" => Some(Lang::En),
            "fr" => Some(Lang::Fr),
            "es" => Some(Lang::Es),
            "ru" => Some(Lang::Ru),
            _ => None,
        }
    }
    /// Native display name shown in the language picker. Uppercase, no
    /// accents for Latin (font has none); Russian in uppercase Cyrillic.
    pub fn native_name(self) -> &'static str {
        match self {
            Lang::En => "ENGLISH",
            Lang::Fr => "FRAN\u{00C7}AIS",
            Lang::Es => "ESPA\u{00D1}OL",
            Lang::Ru => "\u{0420}\u{0423}\u{0421}\u{0421}\u{041A}\u{0418}\u{0419}", // РУССКИЙ
        }
    }
}

/// Order of languages in the picker (matches `Lang` index order).
pub const PICKER_LANGS: &[Lang] = &[Lang::En, Lang::Fr, Lang::Es, Lang::Ru];

/// Every translatable UI string. One instance per language below.
pub struct Strings {
    // Pause menu (in-game)
    pub menu_resume: &'static str,
    pub menu_keys: &'static str,
    pub menu_restart: &'static str,
    pub menu_quit: &'static str,
    pub pause_title: &'static str,
    pub pause_footer: &'static str,
    // TOUCHES (keymap) editor
    pub keys_title: &'static str,
    pub keys_footer: &'static str,
    pub keys_dropdown_footer: &'static str,
    pub none: &'static str,
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
    // Error toast
    pub err_title: &'static str,
    pub err_footer: &'static str,
    // Settings modal (Plus from library)
    pub settings_title: &'static str,
    pub set_keys: &'static str,
    pub set_language: &'static str,
    pub set_quit: &'static str,
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
    pub cover_title: &'static str,
    pub cover_footer: &'static str,
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
    /// Sort direction labels shown in the sort modals (toggled with X).
    pub sort_dir_asc: &'static str,
    pub sort_dir_desc: &'static str,
    pub settings_footer: &'static str,
    pub lang_title: &'static str,
    pub lang_footer: &'static str,
    // URL-history delete confirm (X on the DISTANT idle screen)
    pub histdel_title: &'static str,
    pub histdel_msg: &'static str,
    // Error MESSAGES (word-wrapped in the error toast). `{}` placeholders
    // are substituted at runtime via str::replace.
    pub err_too_large: &'static str,
    pub err_https: &'static str,         // {} = failure detail (curl/http)
    pub err_json: &'static str,          // {} = parser detail
    pub err_json_no_files: &'static str,
    pub err_dl_start: &'static str,      // {} = code
    pub err_dl_failed: &'static str,     // {} = code
    pub err_dl_cancelled: &'static str,
    pub err_url_invalid: &'static str,
    pub err_no_swf: &'static str,
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
}

const EN: Strings = Strings {
    menu_resume: "RESUME",
    menu_keys: "CONTROLS",
    menu_restart: "RESTART",
    menu_quit: "QUIT",
    pause_title: "PAUSE",
    pause_footer: "A:OK   B:CANCEL   UP/DOWN:NAV",
    keys_title: "CONTROLS",
    keys_footer: "A:EDIT   B:BACK   UP/DOWN:NAV",
    keys_dropdown_footer: "A:OK   B:CANCEL   UP/DOWN:NAV",
    none: "(none)",
    empty_title: "NO GAMES",
    empty_l1: "DROP .SWF FILES INTO",
    empty_l2: "SDMC:/FLASHNX/   OR   SDMC:/SWITCH/FLASHNX/",
    empty_l3: "THEN RESTART FLASHNX.",
    empty_footer: "Y:REMOTE IMPORT   -:QUIT",
    list_footer: "L/R:TABS  A:PLAY  Y:SORT  -:SEARCH  +:OPTIONS",
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
    dist_list_footer: "A:LAUNCH   X:FLASHPOINT   +:OPTIONS   L/R:TABS",
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
    files_footer: "A:DOWNLOAD   Y:SORT   X:SEARCH   L/R:PAGE   B:BACK",
    dl_title: "DOWNLOADING",
    dl_footer: "B:CANCEL",
    err_title: "ERROR",
    err_footer: "A/B:OK",
    settings_title: "SETTINGS",
    set_keys: "DEFAULT CONTROLS",
    set_language: "LANGUAGE",
    set_quit: "QUIT",
    set_back: "BACK",
    set_report_bug: "REPORT A BUG",
    tab_play: "PLAY",
    tab_import: "IMPORT",
    tab_settings: "SETTINGS",
    set_covers: "ONLINE COVERS",
    lbl_on: "ON",
    lbl_off: "OFF",
    opt_cover: "COVER",
    cover_title: "CHOOSE A COVER",
    cover_footer: "A: CHOOSE   -: SEARCH   UP/DOWN: MOVE   B: BACK",
    cover_off_notice: "ENABLE ONLINE COVERS IN SETTINGS",
    cover_none: "NO RESULTS",
    fp_title: "FLASHPOINT",
    fp_footer: "A:DOWNLOAD   Y:SORT   +:INFO   DPAD:MOVE   B:BACK",
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
    sort_dir_asc: "ASCENDING",
    sort_dir_desc: "DESCENDING",
    settings_footer: "L/R:TABS   A:OK",
    lang_title: "LANGUAGE",
    lang_footer: "A:OK   B:CANCEL",
    histdel_title: "DELETE URL ?",
    histdel_msg: "Remove this URL from the history?",
    err_too_large: "Archive.org response too large (>4 MB). Item too big for this build.",
    err_https: "HTTPS failed ({}). Check the clock, WiFi + the URL.",
    err_json: "Unreadable archive.org JSON: {}",
    err_json_no_files: "JSON has no \"files\" field",
    err_dl_start: "Could not start the download (code {}).",
    err_dl_failed: "Download failed (code {})",
    err_dl_cancelled: "Download cancelled by the user.",
    err_url_invalid: "Invalid URL. Expected an archive.org URL like https://archive.org/details/<id> or simply <id>.",
    err_no_swf: "No .SWF file found in this archive.org item.",
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
    kbd_bug_guide: "Describe the problem (display, sound, crash...). Empty = just send the game info.",
    set_suggest: "MAKE A SUGGESTION",
    kbd_suggest_header: "FlashNX - Make a suggestion",
    kbd_suggest_guide: "Your idea / feature request for FlashNX.",
};

const FR: Strings = Strings {
    menu_resume: "REPRENDRE",
    menu_keys: "TOUCHES",
    menu_restart: "RED\u{00C9}MARRER",
    menu_quit: "QUITTER",
    pause_title: "PAUSE",
    pause_footer: "A:OK   B:ANNULER   HAUT/BAS:NAV",
    keys_title: "TOUCHES",
    keys_footer: "A:\u{00C9}DITER   B:RETOUR   HAUT/BAS:NAV",
    keys_dropdown_footer: "A:OK   B:ANNULER   HAUT/BAS:NAV",
    none: "(aucune)",
    empty_title: "AUCUN JEU",
    empty_l1: "D\u{00C9}POSEZ DES FICHIERS .SWF DANS",
    empty_l2: "SDMC:/FLASHNX/   OU   SDMC:/SWITCH/FLASHNX/",
    empty_l3: "PUIS RED\u{00C9}MARREZ FLASHNX.",
    empty_footer: "Y:IMPORT DISTANT   -:QUITTER",
    list_footer: "L/R:ONGLETS  A:JOUER  Y:TRIER  -:RECHERCHE  +:OPTIONS",
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
    dist_list_footer: "A:LANCER   X:FLASHPOINT   +:OPTIONS   L/R:ONGLETS",
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
    files_footer: "A:T\u{00C9}L\u{00C9}CHARGER   Y:TRIER   X:RECHERCHE   L/R:PAGE   B:RETOUR",
    dl_title: "T\u{00C9}L\u{00C9}CHARGEMENT",
    dl_footer: "B:ANNULER",
    err_title: "ERREUR",
    err_footer: "A/B:OK",
    settings_title: "R\u{00C9}GLAGES",
    set_keys: "TOUCHES PAR D\u{00C9}FAUT",
    set_language: "LANGUE",
    set_quit: "QUITTER",
    set_back: "RETOUR",
    set_report_bug: "SIGNALER UN BUG",
    tab_play: "JOUER",
    tab_import: "IMPORTER",
    tab_settings: "R\u{00C9}GLAGES",
    set_covers: "JAQUETTES EN LIGNE",
    lbl_on: "ON",
    lbl_off: "OFF",
    opt_cover: "JAQUETTE",
    cover_title: "CHOISIR UNE JAQUETTE",
    cover_footer: "A: CHOISIR   -: RECHERCHER   HAUT/BAS: NAVIGUER   B: RETOUR",
    cover_off_notice: "ACTIVE LES JAQUETTES EN LIGNE DANS LES R\u{00C9}GLAGES",
    cover_none: "AUCUN R\u{00C9}SULTAT",
    fp_title: "FLASHPOINT",
    fp_footer: "A:T\u{00C9}L\u{00C9}CHARGER   Y:TRIER   +:INFOS   DPAD:NAV   B:RETOUR",
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
    sort_dir_asc: "CROISSANT",
    sort_dir_desc: "D\u{00C9}CROISSANT",
    settings_footer: "L/R:ONGLETS   A:OK",
    lang_title: "LANGUE",
    lang_footer: "A:OK   B:ANNULER",
    histdel_title: "SUPPRIMER URL ?",
    histdel_msg: "RETIRER CETTE URL DE L'HISTORIQUE ?",
    err_too_large: "R\u{00C9}PONSE ARCHIVE.ORG TROP VOLUMINEUSE (>4 MB). ITEM TROP MASSIF POUR CETTE VERSION.",
    err_https: "\u{00C9}CHEC HTTPS ({}). V\u{00C9}RIFIEZ L'HORLOGE, LE WIFI + L'URL.",
    err_json: "JSON ARCHIVE.ORG ILLISIBLE : {}",
    err_json_no_files: "JSON SANS CHAMP \"files\"",
    err_dl_start: "IMPOSSIBLE DE LANCER LE T\u{00C9}L\u{00C9}CHARGEMENT (CODE {}).",
    err_dl_failed: "T\u{00C9}L\u{00C9}CHARGEMENT \u{00C9}CHOU\u{00C9} (CODE {})",
    err_dl_cancelled: "T\u{00C9}L\u{00C9}CHARGEMENT ANNUL\u{00C9} PAR L'UTILISATEUR.",
    err_url_invalid: "URL INVALIDE. ATTENDU UNE URL ARCHIVE.ORG TYPE https://archive.org/details/<id> OU SIMPLEMENT <id>.",
    err_no_swf: "AUCUN FICHIER .SWF TROUV\u{00C9} DANS CET ITEM ARCHIVE.ORG.",
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
    kbd_bug_guide: "Decris le probleme (affichage, son, plantage...). Vide = envoyer juste les infos du jeu.",
    set_suggest: "FAIRE UNE PROPOSITION",
    kbd_suggest_header: "FlashNX - Faire une proposition",
    kbd_suggest_guide: "Ton idee / proposition de fonctionnalite pour FlashNX.",
};

const ES: Strings = Strings {
    menu_resume: "REANUDAR",
    menu_keys: "CONTROLES",
    menu_restart: "REINICIAR",
    menu_quit: "SALIR",
    pause_title: "PAUSA",
    pause_footer: "A:OK   B:CANCELAR   ARRIBA/ABAJO:NAV",
    keys_title: "CONTROLES",
    keys_footer: "A:EDITAR   B:VOLVER   ARRIBA/ABAJO:NAV",
    keys_dropdown_footer: "A:OK   B:CANCELAR   ARRIBA/ABAJO:NAV",
    none: "(ninguna)",
    empty_title: "SIN JUEGOS",
    empty_l1: "COLOCA ARCHIVOS .SWF EN",
    empty_l2: "SDMC:/FLASHNX/   O   SDMC:/SWITCH/FLASHNX/",
    empty_l3: "Y REINICIA FLASHNX.",
    empty_footer: "Y:IMPORTAR REMOTO   -:SALIR",
    list_footer: "L/R:PESTA\u{00D1}AS  A:JUGAR  Y:ORDENAR  -:BUSCAR  +:OPCIONES",
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
    dist_list_footer: "A:LANZAR   X:FLASHPOINT   +:OPCIONES   L/R:PESTA\u{00D1}AS",
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
    files_footer: "A:DESCARGAR   Y:ORDENAR   X:BUSCAR   L/R:P\u{00C1}GINA   B:VOLVER",
    dl_title: "DESCARGANDO",
    dl_footer: "B:CANCELAR",
    err_title: "ERROR",
    err_footer: "A/B:OK",
    settings_title: "AJUSTES",
    set_keys: "CONTROLES POR DEFECTO",
    set_language: "IDIOMA",
    set_quit: "SALIR",
    set_back: "VOLVER",
    set_report_bug: "REPORTAR UN FALLO",
    tab_play: "JUGAR",
    tab_import: "IMPORTAR",
    tab_settings: "AJUSTES",
    set_covers: "CARATULAS EN LINEA",
    lbl_on: "ON",
    lbl_off: "OFF",
    opt_cover: "CARATULA",
    cover_title: "ELEGIR CARATULA",
    cover_footer: "A: ELEGIR   -: BUSCAR   ARRIBA/ABAJO: MOVER   B: VOLVER",
    cover_off_notice: "ACTIVA CARATULAS EN LINEA EN AJUSTES",
    cover_none: "SIN RESULTADOS",
    fp_title: "FLASHPOINT",
    fp_footer: "A:DESCARGAR   Y:ORDENAR   +:INFO   DPAD:NAV   B:VOLVER",
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
    sort_dir_asc: "ASCENDENTE",
    sort_dir_desc: "DESCENDENTE",
    settings_footer: "L/R:PESTA\u{00D1}AS   A:OK",
    lang_title: "IDIOMA",
    lang_footer: "A:OK   B:CANCELAR",
    histdel_title: "\u{00BF}BORRAR URL ?",
    histdel_msg: "\u{00BF}QUITAR ESTA URL DEL HISTORIAL?",
    err_too_large: "RESPUESTA DE ARCHIVE.ORG DEMASIADO GRANDE (>4 MB). ITEM DEMASIADO GRANDE PARA ESTA VERSI\u{00D3}N.",
    err_https: "FALLO HTTPS ({}). COMPRUEBA EL RELOJ, EL WIFI + LA URL.",
    err_json: "JSON DE ARCHIVE.ORG ILEGIBLE: {}",
    err_json_no_files: "JSON SIN CAMPO \"files\"",
    err_dl_start: "NO SE PUDO INICIAR LA DESCARGA (C\u{00D3}DIGO {}).",
    err_dl_failed: "DESCARGA FALLIDA (C\u{00D3}DIGO {})",
    err_dl_cancelled: "DESCARGA CANCELADA POR EL USUARIO.",
    err_url_invalid: "URL INV\u{00C1}LIDA. SE ESPERABA UNA URL DE ARCHIVE.ORG TIPO https://archive.org/details/<id> O SIMPLEMENTE <id>.",
    err_no_swf: "NO SE ENCONTR\u{00D3} NING\u{00DA}N ARCHIVO .SWF EN ESTE ITEM DE ARCHIVE.ORG.",
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
    kbd_bug_guide: "Describe el problema (pantalla, sonido, cierre...). Vacio = enviar solo la info del juego.",
    set_suggest: "HACER UNA SUGERENCIA",
    kbd_suggest_header: "FlashNX - Hacer una sugerencia",
    kbd_suggest_guide: "Tu idea / sugerencia de funcion para FlashNX.",
};

// Russian — UPPERCASE Cyrillic (draw_text does not case-fold non-ASCII, and
// only uppercase Cyrillic glyphs were added to the font).
const RU: Strings = Strings {
    menu_resume: "ПРОДОЛЖИТЬ",
    menu_keys: "КЛАВИШИ",
    menu_restart: "ЗАНОВО",
    menu_quit: "ВЫХОД",
    pause_title: "ПАУЗА",
    pause_footer: "A:ОК   B:ОТМЕНА   ВВЕРХ/ВНИЗ:НАВ",
    keys_title: "КЛАВИШИ",
    keys_footer: "A:ИЗМЕНИТЬ   B:НАЗАД   ВВЕРХ/ВНИЗ:НАВ",
    keys_dropdown_footer: "A:ОК   B:ОТМЕНА   ВВЕРХ/ВНИЗ:НАВ",
    none: "(НЕТ)",
    empty_title: "НЕТ ИГР",
    empty_l1: "ПОМЕСТИТЕ ФАЙЛЫ .SWF В",
    empty_l2: "SDMC:/FLASHNX/   ИЛИ   SDMC:/SWITCH/FLASHNX/",
    empty_l3: "ЗАТЕМ ПЕРЕЗАПУСТИТЕ FLASHNX.",
    empty_footer: "Y:ЗАГРУЗКА ПО СЕТИ   -:ВЫХОД",
    list_footer: "L/R:ВКЛАДКИ  A:ИГРАТЬ  Y:СОРТ  -:ПОИСК  +:ОПЦИИ",
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
    dist_list_footer: "A:ЗАПУСК   X:FLASHPOINT   +:ОПЦИИ   L/R:ВКЛАДКИ",
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
    files_footer: "A:ЗАГРУЗИТЬ   Y:СОРТ   X:ПОИСК   L/R:СТРАНИЦА   B:НАЗАД",
    dl_title: "ЗАГРУЗКА",
    dl_footer: "B:ОТМЕНА",
    err_title: "ОШИБКА",
    err_footer: "A/B:ОК",
    settings_title: "НАСТРОЙКИ",
    set_keys: "КЛАВИШИ ПО УМОЛЧАНИЮ",
    set_language: "ЯЗЫК",
    set_quit: "ВЫХОД",
    set_back: "НАЗАД",
    set_report_bug: "СООБЩИТЬ ОБ ОШИБКЕ",
    tab_play: "ИГРАТЬ",
    tab_import: "ЗАГРУЗКА",
    tab_settings: "НАСТРОЙКИ",
    set_covers: "ОБЛОЖКИ ОНЛАЙН",
    lbl_on: "ON",
    lbl_off: "OFF",
    opt_cover: "ОБЛОЖКА",
    cover_title: "ВЫБЕРИТЕ ОБЛОЖКУ",
    cover_footer: "A: ВЫБРАТЬ   -: ПОИСК   ВВЕРХ/ВНИЗ: НАВИГАЦИЯ   B: НАЗАД",
    cover_off_notice: "ВКЛЮЧИТЕ ОБЛОЖКИ ОНЛАЙН В НАСТРОЙКАХ",
    cover_none: "НЕТ РЕЗУЛЬТАТОВ",
    fp_title: "FLASHPOINT",
    fp_footer: "A:СКАЧАТЬ   Y:СОРТ   +:ИНФО   DPAD:НАВ   B:НАЗАД",
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
    sort_dir_asc: "ПО ВОЗР.",
    sort_dir_desc: "ПО УБЫВ.",
    settings_footer: "L/R:ВКЛАДКИ   A:ОК",
    lang_title: "ЯЗЫК",
    lang_footer: "A:ОК   B:ОТМЕНА",
    histdel_title: "УДАЛИТЬ URL ?",
    histdel_msg: "УБРАТЬ ЭТОТ URL ИЗ ИСТОРИИ?",
    err_too_large: "ОТВЕТ ARCHIVE.ORG СЛИШКОМ БОЛЬШОЙ (>4 МБ). ЭЛЕМЕНТ СЛИШКОМ ВЕЛИК ДЛЯ ЭТОЙ ВЕРСИИ.",
    err_https: "ОШИБКА HTTPS ({}). ПРОВЕРЬТЕ ЧАСЫ, WIFI + URL.",
    err_json: "НЕЧИТАЕМЫЙ JSON ARCHIVE.ORG: {}",
    err_json_no_files: "В JSON НЕТ ПОЛЯ \"files\"",
    err_dl_start: "НЕ УДАЛОСЬ НАЧАТЬ ЗАГРУЗКУ (КОД {}).",
    err_dl_failed: "ЗАГРУЗКА НЕ УДАЛАСЬ (КОД {})",
    err_dl_cancelled: "ЗАГРУЗКА ОТМЕНЕНА ПОЛЬЗОВАТЕЛЕМ.",
    err_url_invalid: "НЕВЕРНЫЙ URL. ОЖИДАЛСЯ URL ARCHIVE.ORG ВИДА https://archive.org/details/<id> ИЛИ ПРОСТО <id>.",
    err_no_swf: "В ЭТОМ ЭЛЕМЕНТЕ ARCHIVE.ORG НЕ НАЙДЕНО ФАЙЛОВ .SWF.",
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
    kbd_bug_guide: "Opishite problemu (ekran, zvuk, sboy...). Pusto = otpravit tolko info ob igre.",
    set_suggest: "ПРЕДЛОЖИТЬ ИДЕЮ",
    kbd_suggest_header: "FlashNX - Predlozhit ideyu",
    kbd_suggest_guide: "Vasha ideya / predlozhenie dlya FlashNX.",
};

/// Current language, as a `Lang` index. Default English; overridden by
/// `init()` at boot from settings.json / system language.
static CURRENT: AtomicU8 = AtomicU8::new(0);

pub fn current() -> Lang {
    Lang::from_index(CURRENT.load(Ordering::Relaxed) as usize)
}

pub fn set(lang: Lang) {
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

/// The active language's string table. Short name because it's called a lot.
pub fn s() -> &'static Strings {
    match current() {
        Lang::En => &EN,
        Lang::Fr => &FR,
        Lang::Es => &ES,
        Lang::Ru => &RU,
    }
}

// ── Dynamic strings ─────────────────────────────────────────────────────

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

/// Write the full settings file (language + covers toggle). Both settings
/// persist together so saving one never drops the other.
fn write_settings(lang: Lang, covers: bool) -> bool {
    let path = settings_write_path();
    let json = std::format!(
        "{{\n    \"language\": \"{}\",\n    \"covers_online\": {}\n}}\n",
        lang.code(),
        covers,
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

extern "C" {
    /// Switch system language → our index (0 En, 1 Fr, 2 Es, 3 Ru), or -1
    /// if unsupported / detection failed. Defined in cpp/src/ruffle_bridge.cpp.
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
            if let Some(lang) = parse_language(&txt) {
                set(lang);
                return;
            }
        }
    }
    // 2. Otherwise follow the console's system language if we support it.
    let sys = unsafe { ruffle_detect_system_lang() };
    if sys >= 0 && sys <= 3 {
        set(Lang::from_index(sys as usize));
        return;
    }
    // 3. Fallback: English (already the default).
}
