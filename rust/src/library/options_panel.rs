//! A game's options in the launcher, and everything opened from them.
//!
//! The panel the mockup draws (Options.dc.html): at the right edge, over the
//! library dimmed behind it, the game's cover and name on top, then the rows
//! in titled groups. First the game in the library (favourite, cover, folders,
//! name, HOME icon), its controls, then its picture and its performance (the
//! settings the pause's ÉCRAN and OVERCLOCK change, here before the game is
//! even launched: a portrait game can be turned before its first run), and the
//! delete row last, on its own.
//!
//! A row with a chevron opens its page BESIDE the panel, which folds to a rail
//! at the right edge (`kit_cascade_on`, the in-game pause does the same from
//! the left): Touches et curseur, the shared profiles and their previews, the
//! folders. The path stays on screen and B goes back the way A came. Only the
//! questions (delete this game? this profile? empty this folder?) and the
//! cover picker sit on top, centred.

use super::*;
use crate::backend::render::{ui_col, Btn, Icon, Row, Sheet, Side, OPTIONS_CHILD_W, OPTIONS_W};

/// The rows of the game options, in order. Matched by name, never by index:
/// `opt_row` finds where a row sits, so adding one moves nothing else.
pub(crate) const OPTIONS_ENTRIES: &[&str] = &[
    "FAVORI", "JAQUETTE", "DOSSIER", "RENOMMER", "RACCOURCI",
    "TOUCHES",
    "AFFICHAGE", "ROTATION", "ZOOM", "PIXELS", "FILTRE",
    "OVERCLOCK", "FPS",
    "SUPPRIMER",
];

/// Where row `key` of `OPTIONS_ENTRIES` sits (0 if it is not there).
pub(crate) fn opt_row(key: &str) -> usize {
    OPTIONS_ENTRIES.iter().position(|k| *k == key).unwrap_or(0)
}

/// A game's picture and performance settings, and whether it has a HOME
/// icon, as its options show them.
#[derive(Clone, Copy)]
pub(crate) struct GamePrefs {
    pub display: u8,
    pub rotation: u8,
    pub zoom: u16,
    pub pixels: u8,
    pub filter: u8,
    pub power: u8,
    pub fps: u8,
    pub home_icon: bool,
    /// The settings this game has a value of its own for (the `.prefs` keys);
    /// the others follow the global default from Réglages.
    pub own: u8,
}

/// Bits of `GamePrefs::own`, and the `.prefs` keys behind each setting row.
const OWN_KEYS: [(&str, &[&str]); 7] = [
    ("AFFICHAGE", &["display"]),
    ("ROTATION", &["rot"]),
    ("ZOOM", &["zoom", "panx", "pany"]),
    ("PIXELS", &["pixels"]),
    ("FILTRE", &["filter"]),
    ("OVERCLOCK", &["power"]),
    ("FPS", &["fps"]),
];

fn own_bit(key: &str) -> u8 {
    OWN_KEYS.iter().position(|(k, _)| *k == key).map_or(0, |i| 1 << i)
}

/// The last game's settings, keyed by its path. Each of them is a read of the
/// game's `.prefs` on the card and the HOME icon an IPC round trip through ns:
/// asked every frame, that was eight SD reads and a title listing sixty times
/// a second while the panel was open.
static GAME_PREFS: Mutex<Option<(std::string::String, GamePrefs)>> = Mutex::new(None);

pub(crate) fn game_prefs(basename: &str, path: &str) -> GamePrefs {
    if let Ok(g) = GAME_PREFS.lock() {
        if let Some((k, p)) = g.as_ref() {
            if k == path {
                return *p;
            }
        }
    }
    let p = GamePrefs {
        display: keymap::display_mode_for(basename),
        rotation: keymap::rotation_for(basename),
        zoom: keymap::zoom_for(basename),
        pixels: keymap::pixel_filter_for(basename),
        filter: keymap::screen_filter_for(basename),
        power: keymap::power_mode_for(basename),
        fps: keymap::fps_counter_for(basename),
        home_icon: shortcut_here(path),
        own: {
            let keys = keymap::pref_keys(basename);
            OWN_KEYS
                .iter()
                .enumerate()
                .filter(|(_, (_, ks))| keys.iter().any(|k| k == ks[0]))
                .fold(0u8, |m, (i, _)| m | 1 << i)
        },
    };
    if let Ok(mut g) = GAME_PREFS.lock() {
        *g = Some((path.to_string(), p));
    }
    p
}

/// Read the settings again on the next frame (one of them just changed).
pub(crate) fn game_prefs_forget() {
    if let Ok(mut g) = GAME_PREFS.lock() {
        *g = None;
    }
}

/// The overclock's three settings in the order its row shows them.
const POWER_ORDER: [u8; 3] = [0, keymap::POWER_AUTO, 1];

/// Whether row `key` is one of the game's settings (as opposed to an action).
pub(crate) fn is_setting(key: &str) -> bool {
    matches!(key, "AFFICHAGE" | "ROTATION" | "ZOOM" | "PIXELS" | "FILTRE" | "OVERCLOCK" | "FPS")
}

/// Left / Right (`dir` -1 / 1) or A (`STEP_CYCLE`) on a settings row: the
/// lit option moves along and stops at the ends, A goes round. Written for the
/// next launch. Returns whether the value moved (a step refused at an end
/// writes nothing and makes no sound).
pub(crate) fn options_step(s: &mut State, game_idx: usize, key: &str, dir: i32) -> bool {
    let Some(e) = s.entries.get(game_idx) else { return false };
    let (b, path) = (e.basename.clone(), e.path.clone());
    let p = game_prefs(&b, &path);
    let d = dir as core::ffi::c_int;
    let moved = match key {
        "AFFICHAGE" => {
            let v = crate::step_choice(p.display, keymap::DISPLAY_MODE_COUNT, d);
            v != p.display && {
                keymap::set_display_mode_for(&b, v);
                true
            }
        }
        "ROTATION" => {
            let v = crate::step_choice(p.rotation, keymap::ROTATION_COUNT, d);
            v != p.rotation && {
                keymap::set_rotation_for(&b, v);
                true
            }
        }
        "ZOOM" => {
            use crate::backend::render::{ZOOM_MAX, ZOOM_MIN};
            // A steps up like Right: a percentage has no "round" to go.
            let z = if dir < 0 { p.zoom.saturating_sub(10) } else { p.zoom.saturating_add(10) };
            let z = z.clamp(ZOOM_MIN, ZOOM_MAX);
            z != p.zoom && {
                keymap::set_zoom_for(&b, z);
                true
            }
        }
        "PIXELS" => {
            let v = crate::step_choice(p.pixels, keymap::PIXEL_FILTER_COUNT, d);
            v != p.pixels && {
                keymap::set_pixel_filter_for(&b, v);
                true
            }
        }
        "FILTRE" => {
            let v = crate::step_choice(p.filter, keymap::SCREEN_FILTER_COUNT, d);
            v != p.filter && {
                keymap::set_screen_filter_for(&b, v);
                true
            }
        }
        "OVERCLOCK" => {
            let at = POWER_ORDER.iter().position(|m| *m == p.power).unwrap_or(0) as u8;
            let v = POWER_ORDER[crate::step_choice(at, 3, d) as usize];
            v != p.power && {
                keymap::set_power_mode_for(&b, v);
                true
            }
        }
        "FPS" => {
            let v = crate::step_choice(p.fps, keymap::FPS_COUNTER_COUNT, d);
            v != p.fps && {
                keymap::set_fps_counter_for(&b, v);
                true
            }
        }
        _ => false,
    };
    if moved {
        game_prefs_forget();
    }
    moved
}

/// X on a setting row: the game drops its own value and follows the global
/// default again. Returns whether there was one to drop.
pub(crate) fn options_reset(s: &mut State, game_idx: usize, key: &str) -> bool {
    let Some(e) = s.entries.get(game_idx) else { return false };
    let (b, path) = (e.basename.clone(), e.path.clone());
    let p = game_prefs(&b, &path);
    let Some((_, keys)) = OWN_KEYS.iter().find(|(k, _)| *k == key) else { return false };
    if p.own & own_bit(key) == 0 {
        return false;
    }
    keymap::clear_prefs(&b, keys);
    game_prefs_forget();
    true
}

/// "MARIO", "MARIO +2", or "Aucun": the folders a game is on, short enough
/// for the right of a row.
fn shelves_value(e: &Entry) -> std::string::String {
    let on = shelves_of(e);
    match on.len() {
        0 => crate::loc_ui::ui().folders_none.to_string(),
        1 => on[0].clone(),
        n => std::format!("{} +{}", on[0], n - 1),
    }
}

/// The options panel as data, handed to `f`.
pub(crate) fn with_options_sheet<R>(
    backend: &mut SwitchRenderBackend,
    entry: &Entry,
    selection: usize,
    f: impl FnOnce(&mut SwitchRenderBackend, &Sheet) -> R,
) -> R {
    let lc = crate::loc::s();
    let u = crate::loc_ui::ui();
    let p = game_prefs(&entry.basename, &entry.path);
    let fav = crate::favorites::is_favorite(&entry.basename);
    let shelves = shelves_value(entry);
    let display_opts = [lc.display_fit, lc.display_stretch, lc.display_fill];
    let rot_opts = [lc.rot_none, lc.rot_90, lc.rot_180, lc.rot_270];
    let pix_opts = [lc.pixels_auto, lc.pixels_smooth, lc.pixels_sharp];
    let filter_opts = [lc.filter_none, lc.filter_scanlines, lc.filter_crt];
    let oc_opts = [lc.overclock_off, lc.pixels_auto, lc.overclock_on];
    let oc_at = POWER_ORDER.iter().position(|m| *m == p.power).unwrap_or(0);
    let zoom = std::format!("{} %", p.zoom);
    // A setting with no value of its own says so ("par défaut"): one sideways
    // press gives the game its own value, and X takes it back.
    let note = |k: &str| if is_setting(k) && p.own & own_bit(k) == 0 { Some(u.tag_default) } else { None };
    let rows: std::vec::Vec<Row> = OPTIONS_ENTRIES
        .iter()
        .map(|k| match *k {
            // A switch, like the other on / off rows, and the star filled when
            // it is on: the row says what the game IS, not what A would do.
            "FAVORI" => Row::new(Icon::Star, lc.opt_favorite).filled_if(fav, ui_col::GOLD).toggle(fav),
            "TOUCHES" => Row::new(Icon::Gamepad, lc.opt_keys).opens(),
            "JAQUETTE" => Row::new(Icon::Image, lc.opt_cover).opens(),
            "DOSSIER" => Row::new(Icon::Folder, lc.opt_folder).value(&shelves).opens(),
            "RENOMMER" => Row::new(Icon::Pencil, lc.opt_rename),
            "RACCOURCI" => Row::new(Icon::Home, u.opt_home_icon).toggle(p.home_icon),
            "AFFICHAGE" => Row::new(Icon::Monitor, lc.set_display_mode).choice(&display_opts, p.display as usize),
            "ROTATION" => Row::new(Icon::Rotate, lc.set_rotation).choice(&rot_opts, p.rotation as usize),
            "ZOOM" => Row::new(Icon::ZoomIn, lc.set_zoom).cycle(&zoom),
            "PIXELS" => Row::new(Icon::Pixels, lc.set_pixel_filter).choice(&pix_opts, p.pixels as usize),
            "FILTRE" => Row::new(Icon::Tv, lc.set_screen_filter).choice(&filter_opts, p.filter as usize),
            "OVERCLOCK" => Row::new(Icon::Cpu, lc.set_overclock).choice(&oc_opts, oc_at),
            "FPS" => Row::new(Icon::Gauge, lc.set_fps).toggle(p.fps == 1),
            _ => Row::new(Icon::Trash, u.opt_delete_game).danger(),
        })
        .zip(OPTIONS_ENTRIES.iter())
        .map(|(r, k)| r.note_opt(note(k)))
        .collect();
    let key = OPTIONS_ENTRIES.get(selection).copied().unwrap_or("");
    let help = match key {
        "FAVORI" => u.help_opt_fav,
        "TOUCHES" => u.help_keys,
        "JAQUETTE" => u.help_opt_cover,
        "DOSSIER" => u.help_opt_folders,
        "RENOMMER" => u.help_opt_rename,
        "RACCOURCI" => u.help_opt_home,
        "AFFICHAGE" => u.help_display,
        "ROTATION" => u.help_rotation,
        "ZOOM" => u.help_opt_zoom,
        "PIXELS" => u.help_pixels,
        "FILTRE" => u.help_filter,
        "OVERCLOCK" => match p.power {
            0 => u.help_oc_off,
            keymap::POWER_AUTO => u.help_oc_auto,
            _ => u.help_oc_on,
        },
        "FPS" => u.help_fps,
        _ => u.help_opt_delete,
    };
    let a_verb = match key {
        "TOUCHES" | "JAQUETTE" | "DOSSIER" => u.open,
        "RENOMMER" => lc.opt_rename,
        "RACCOURCI" => {
            if p.home_icon {
                u.remove
            } else {
                u.install
            }
        }
        _ => lc.opt_delete,
    };
    let stepper = matches!(
        key,
        "FAVORI" | "AFFICHAGE" | "ROTATION" | "ZOOM" | "PIXELS" | "FILTRE" | "OVERCLOCK" | "FPS"
    );
    let hints_step = [(&[Btn::Dpad][..], u.change), (&[Btn::L, Btn::R][..], u.other_game), (&[Btn::B][..], u.back)];
    // On a setting the game has its own value for, X goes back to the default.
    let hints_own = [
        (&[Btn::Dpad][..], u.change),
        (&[Btn::X][..], u.reset_default),
        (&[Btn::L, Btn::R][..], u.other_game),
        (&[Btn::B][..], u.back),
    ];
    let own_here = is_setting(key) && p.own & own_bit(key) != 0;
    let hints_act = [(&[Btn::A][..], a_verb), (&[Btn::L, Btn::R][..], u.other_game), (&[Btn::B][..], u.back)];
    // Every group under its title: the game in the library, its controls, its
    // picture and its performance, then the delete row on its own.
    let sections = [
        (opt_row("FAVORI"), u.sec_library),
        (opt_row("TOUCHES"), u.sec_controls),
        (opt_row("AFFICHAGE"), u.sec_image),
        (opt_row("OVERCLOCK"), u.sec_perf),
        (opt_row("SUPPRIMER"), ""),
    ];
    let thumb = backend.cover_thumb(&entry.basename);
    let mut sheet = Sheet::new(&entry.display_name, &rows, selection);
    sheet.subtitle = Some(u.opt_subtitle);
    sheet.thumb = Some(thumb.unwrap_or((0, 0, 0, 0)));
    sheet.thumb_tile = Some((entry.color_chip, &entry.basename));
    sheet.sections = &sections;
    sheet.row_h = 48.0;
    sheet.glide_key = 0x6200_0000;
    sheet.help = help;
    sheet.hints = if own_here {
        &hints_own
    } else if stepper {
        &hints_step
    } else {
        &hints_act
    };
    f(backend, &sheet)
}

/// What sits beside the options while one of their pages is open, kept for the
/// frames where the page itself is not on screen (a loading panel).
static LAST_CHILD: Mutex<Option<Screen>> = Mutex::new(None);

/// Whether `screen` is drawn by `draw_options_family`.
pub(crate) fn is_options_family(screen: Screen) -> bool {
    matches!(
        screen,
        Screen::OptionsModal { .. }
            | Screen::DeleteConfirm { .. }
            | Screen::CoverPicker { .. }
            | Screen::TouchesMenu { .. }
            | Screen::ProfileList { .. }
            | Screen::ProfilePreview { .. }
            | Screen::RevertPreview { .. }
            | Screen::ProfileShareConfirm { .. }
            | Screen::ProfileDeleteConfirm { .. }
            | Screen::GameFolderPicker { .. }
            | Screen::GameFolderDelete { .. }
    )
}

/// The left edge of the options panels on screen for `screen` (the panel, or
/// the rail and the page beside it), None when `screen` is not one of theirs.
pub(crate) fn panels_left_edge(screen: Screen, vw: f32) -> Option<f32> {
    if !is_options_family(screen) {
        return None;
    }
    let page_open = matches!(
        screen,
        Screen::TouchesMenu { .. }
            | Screen::ProfileList { .. }
            | Screen::ProfilePreview { .. }
            | Screen::RevertPreview { .. }
            | Screen::ProfileShareConfirm { .. }
            | Screen::ProfileDeleteConfirm { .. }
            | Screen::GameFolderPicker { .. }
            | Screen::GameFolderDelete { .. }
    );
    Some(if page_open { vw - 16.0 - 76.0 - 10.0 - OPTIONS_CHILD_W } else { vw - 16.0 - OPTIONS_W })
}

/// The game a family screen is about.
fn family_game(screen: Screen) -> Option<usize> {
    Some(match screen {
        Screen::OptionsModal { game_idx, .. }
        | Screen::DeleteConfirm { game_idx, .. }
        | Screen::CoverPicker { game_idx, .. }
        | Screen::TouchesMenu { game_idx, .. }
        | Screen::ProfileList { game_idx, .. }
        | Screen::ProfilePreview { game_idx, .. }
        | Screen::RevertPreview { game_idx }
        | Screen::ProfileShareConfirm { game_idx }
        | Screen::ProfileDeleteConfirm { game_idx, .. }
        | Screen::GameFolderPicker { game_idx, .. }
        | Screen::GameFolderDelete { game_idx, .. } => game_idx,
        _ => return None,
    })
}

/// The options row a page hangs from (the rail marks it).
fn branch_of(screen: Screen) -> usize {
    match screen {
        Screen::GameFolderPicker { .. } | Screen::GameFolderDelete { .. } => opt_row("DOSSIER"),
        Screen::CoverPicker { .. } => opt_row("JAQUETTE"),
        Screen::DeleteConfirm { .. } => opt_row("SUPPRIMER"),
        _ => opt_row("TOUCHES"),
    }
}

/// Draw `screen`, one of the options family, over the library: the home as it
/// was, dimmed; the options panel at the right, sliding in on `slide` (the
/// modal timeline, None when settled); the page beside it; a question on top.
/// `kind` is the touch tag the panels publish under.
pub(crate) fn draw_options_family(
    backend: &mut SwitchRenderBackend,
    screen: Screen,
    now: u64,
    anim_origin: u64,
    slide: Option<f32>,
    kind: u32,
) {
    let Some(game_idx) = family_game(screen) else { return };
    let snap = LIBRARY.lock().ok().map(|s| {
        (
            list_screen_for_abs(&s, game_idx),
            s.entries.get(game_idx).cloned(),
            (s.banner_tex, s.banner_w, s.banner_h),
        )
    });
    let Some((home, Some(entry), logo)) = snap else { return };

    // The library as it was, still, under the panels: the mockup shows it at
    // 62 %, and the tiles' clocks keep running instead of jumping when the
    // panel closes. It takes no taps.
    backend.clear_ui_transform();
    crate::backend::render::set_ui_screen_kind(0);
    if let Screen::List { selection, scroll_offset } = home {
        draw_gallery(backend, selection, scroll_offset, anim_origin);
    }
    backend.draw_navbar(Tab::Jouer.index(), logo);
    crate::backend::render::navbar_clear();
    backend.kit_scrim(ui_col::SCRIM);
    crate::backend::render::set_ui_screen_kind(kind);

    // The page beside the options, or none. A loading panel keeps the last
    // page on screen under it rather than folding the options back open.
    let page = match screen {
        Screen::OptionsModal { .. } | Screen::DeleteConfirm { .. } | Screen::CoverPicker { .. } => None,
        Screen::ProfileDeleteConfirm { game_idx, profile_idx, .. } => {
            Some(Screen::ProfileList { game_idx, selection: profile_idx })
        }
        Screen::GameFolderDelete { game_idx, folder_idx, .. } => {
            Some(Screen::GameFolderPicker { game_idx, selection: folder_idx + 1 })
        }
        other => Some(other),
    };
    let t = crate::backend::render::cascade_step(page.is_some());
    // After B the page is gone from the screen state but folds back for a few
    // frames: drawn from memory, without a cursor (it takes no taps on its way
    // out). Forgotten once folded.
    let shown = match page {
        Some(p) => {
            if let Ok(mut g) = LAST_CHILD.lock() {
                *g = Some(p);
            }
            Some((p, true))
        }
        None => {
            let last = LAST_CHILD.lock().ok().and_then(|mut g| {
                if t <= 0.0 {
                    *g = None;
                }
                *g
            });
            last.map(|p| (p, false))
        }
    };
    let opt_sel = match screen {
        Screen::OptionsModal { selection, .. } => selection,
        other => branch_of(other),
    };
    if let Some(p) = slide {
        backend.set_ui_sheet_slide_right(p);
    }
    draw_family_panels(backend, &entry, opt_sel, shown, t);
    backend.clear_ui_transform();

    // A question or the cover picker on top, popping in by itself.
    let dialog_key = match screen {
        Screen::DeleteConfirm { .. } => 1,
        Screen::ProfileDeleteConfirm { .. } => 2,
        Screen::GameFolderDelete { .. } => 3,
        Screen::CoverPicker { .. } => 4,
        _ => 0,
    };
    if dialog_key != 0 && slide.is_none() {
        backend.set_ui_modal_scale(crate::backend::render::dialog_pop_scale(dialog_key));
    }
    draw_family_dialog(backend, screen, &entry);
    backend.clear_ui_transform();
    if dialog_key == 0 {
        crate::backend::render::dialog_pop_scale(0);
    }
    let _ = now;
}

/// The loading frame of a profile flow: the panels as they were, the spinner
/// over them.
pub(crate) fn draw_options_loading(backend: &mut SwitchRenderBackend, now: u64, anim_origin: u64, label: &str) {
    let last = LAST_CHILD.lock().ok().and_then(|g| *g);
    match last {
        Some(page) => {
            draw_options_family(backend, page, now, anim_origin, None, 0);
            // Over the library side, left of the panels.
            let vw = backend.screen_size().0;
            let left_of_panels = vw - 16.0 - 76.0 - 10.0 - OPTIONS_CHILD_W;
            backend.draw_loading_card(label, now, 0.0, left_of_panels);
        }
        None => backend.draw_loading_overlay(label, now),
    }
}

/// `page` is the page beside the options and whether it is live (false while
/// it folds back after B: no cursor, no taps).
fn draw_family_panels(backend: &mut SwitchRenderBackend, entry: &Entry, opt_sel: usize, page: Option<(Screen, bool)>, t: f32) {
    let lc = crate::loc::s();
    let u = crate::loc_ui::ui();
    let live = page.map_or(true, |(_, l)| l);
    let page = page.map(|(p, _)| p);
    let branch = page.map(branch_of).unwrap_or(0);
    // A page folding back is drawn without its cursor.
    let sel = |s: usize| if live { s } else { usize::MAX };
    let cascade = |b: &mut SwitchRenderBackend, parent: &Sheet, child: Option<&Sheet>| {
        b.kit_cascade_on(Side::Right, OPTIONS_W, OPTIONS_CHILD_W, parent, branch, child, t);
    };
    with_options_sheet(backend, entry, opt_sel, |b, parent| {
        // The way back, spelled out over each page.
        let back_options = [u.hint_options];
        let back_keys = [u.hint_options, lc.menu_keys];
        match page {
            Some(Screen::TouchesMenu { selection, .. }) => {
                let v = LIBRARY
                    .lock()
                    .ok()
                    .map(|s| menu::TouchesView {
                        speed_x10: launcher_speed_x10(s.touches_cursor_idx),
                        shown: s.touches_show_cursor,
                        gyro: s.touches_gyro,
                        can_revert: s.touches_can_revert,
                        has_backup: s.touches_has_backup,
                    });
                if let Some(v) = v {
                    menu::with_touches_view(b, &v, sel(selection), &back_options, |b, c| cascade(b, parent, Some(c)));
                }
            }
            Some(Screen::ProfileList { selection, .. }) => {
                let snap = LIBRARY.lock().ok().map(|s| {
                    (
                        s.profile_matches.iter().map(menu::ProfileItem::of).collect::<std::vec::Vec<_>>(),
                        s.active_profile_id.clone(),
                        s.profile_copy_mode,
                    )
                });
                if let Some((items, active, copy_mode)) = snap {
                    menu::with_profiles_view(b, &items, &active, copy_mode, sel(selection), &back_keys, |b, c| {
                        cascade(b, parent, Some(c))
                    });
                }
            }
            Some(Screen::ProfilePreview { profile_idx, .. }) => {
                let (rows, name) = LIBRARY
                    .lock()
                    .ok()
                    .map(|s| {
                        (
                            s.preview_rows.clone(),
                            s.profile_matches.get(profile_idx).map(|m| menu::profile_name(&m.profile)).unwrap_or_default(),
                        )
                    })
                    .unwrap_or_default();
                let crumbs = [u.hint_options, lc.menu_keys, lc.profile_title];
                let hints = [(&[Btn::A][..], u.apply), (&[Btn::B][..], u.back)];
                menu::with_diff_view(b, &crumbs, &name, Some(u.preview_sub), &rows, &hints, |b, c| {
                    cascade(b, parent, Some(c))
                });
            }
            Some(Screen::RevertPreview { .. }) => {
                let (rows, has_backup) = LIBRARY
                    .lock()
                    .ok()
                    .map(|s| (s.preview_rows.clone(), s.touches_has_backup))
                    .unwrap_or_default();
                let title = if has_backup { lc.profile_revert } else { lc.touches_revert_default };
                let hints = [(&[Btn::A][..], u.restore), (&[Btn::B][..], u.back)];
                menu::with_diff_view(b, &back_keys, title, None, &rows, &hints, |b, c| cascade(b, parent, Some(c)));
            }
            Some(Screen::ProfileShareConfirm { .. }) => {
                let (rows, is_update) = LIBRARY
                    .lock()
                    .ok()
                    .map(|s| (s.preview_rows.clone(), s.share_is_update))
                    .unwrap_or_default();
                let question = if is_update { lc.share_confirm_update } else { lc.profile_share_confirm };
                let hints = [(&[Btn::A][..], u.share), (&[Btn::B][..], u.cancel)];
                menu::with_diff_view(b, &back_keys, lc.opt_share, Some(question), &rows, &hints, |b, c| {
                    cascade(b, parent, Some(c))
                });
            }
            Some(Screen::GameFolderPicker { selection, .. }) => {
                with_folders_view(b, entry, sel(selection), &back_options, |b, c| cascade(b, parent, Some(c)));
            }
            _ => cascade(b, parent, None),
        }
    });
}

/// The cursor speed a game's Touches page shows: its own, or the global one it
/// follows when it has none (the C++ side's live value, which in the launcher
/// IS the global one).
fn launcher_speed_x10(idx: i32) -> u32 {
    if idx >= 0 {
        let i = (idx as usize).min(crate::loc::CURSOR_SPEED_X10.len() - 1);
        crate::loc::CURSOR_SPEED_X10[i]
    } else {
        (unsafe { ruffle_cursor_speed_mult_x10() }).max(5) as u32
    }
}

/// The folders page as data, handed to `f`: "No folder" first, then every
/// folder with its count, the ones this game is on ticked (the one its file
/// sits in marked with the card instead: only "No folder" takes it out), then
/// a new folder.
fn with_folders_view<R>(
    backend: &mut SwitchRenderBackend,
    entry: &Entry,
    selection: usize,
    crumbs: &[&str],
    f: impl FnOnce(&mut SwitchRenderBackend, &Sheet) -> R,
) -> R {
    let lc = crate::loc::s();
    let u = crate::loc_ui::ui();
    let folders = LIBRARY.lock().map(|s| folder_counts(&s.entries)).unwrap_or_default();
    let physical = if entry.folder.is_empty() { None } else { Some(entry.folder.clone()) };
    let on = shelves_of(entry);
    let bare = on_no_shelf(entry);
    let counts: std::vec::Vec<std::string::String> =
        folders.iter().map(|(_, c)| crate::loc_ui::games(*c)).collect();
    let mut rows: std::vec::Vec<Row> = std::vec::Vec::with_capacity(folders.len() + 2);
    rows.push(Row::new(Icon::Grid, lc.folder_root).mark_if(bare, Icon::Check, ui_col::OK));
    for (i, (n, _)) in folders.iter().enumerate() {
        let row = Row::new(Icon::Folder, n).value(&counts[i]);
        rows.push(if physical.as_deref() == Some(n.as_str()) {
            row.mark(Icon::HardDrive, ui_col::TEXT2)
        } else if on.iter().any(|x| x == n) {
            row.mark(Icon::Check, ui_col::OK)
        } else {
            row
        });
    }
    rows.push(Row::new(Icon::FolderPlus, lc.games_dir_new));
    let last = rows.len() - 1;
    let on_physical = selection >= 1
        && selection < last
        && folders.get(selection - 1).is_some_and(|(n, _)| physical.as_deref() == Some(n.as_str()));
    let a_verb = if selection == 0 {
        u.choose
    } else if selection == last {
        u.create
    } else {
        match folders.get(selection - 1) {
            Some((n, _)) if on.iter().any(|x| x == n) => u.remove,
            _ => u.add,
        }
    };
    let help = if on_physical { u.help_folder_physical } else { u.help_opt_folders };
    let on_shelf_row = selection >= 1 && selection < last;
    let hints_physical = [(&[Btn::X][..], u.empty_verb), (&[Btn::B][..], u.back)];
    let hints_shelf = [(&[Btn::A][..], a_verb), (&[Btn::X][..], u.empty_verb), (&[Btn::B][..], u.back)];
    let hints_other = [(&[Btn::A][..], a_verb), (&[Btn::B][..], u.back)];
    let mut sheet = Sheet::new(u.folders, &rows, selection);
    sheet.kicker = Some(Icon::ChevronLeft);
    sheet.crumbs = crumbs;
    sheet.row_h = 48.0;
    sheet.glide_key = 0x6100_0000;
    sheet.help = help;
    sheet.hints = if on_physical {
        &hints_physical
    } else if on_shelf_row {
        &hints_shelf
    } else {
        &hints_other
    };
    f(backend, &sheet)
}

fn draw_family_dialog(backend: &mut SwitchRenderBackend, screen: Screen, entry: &Entry) {
    use crate::backend::render::Choice;
    let lc = crate::loc::s();
    let u = crate::loc_ui::ui();
    match screen {
        Screen::DeleteConfirm { selection, .. } => {
            let detail = if entry.display_name == entry.basename || pretty_name(&entry.basename) == entry.display_name {
                entry.display_name.clone()
            } else {
                std::format!("{}  \u{00B7}  {}", entry.display_name, entry.basename)
            };
            backend.draw_confirm(
                Icon::Trash,
                u.delete_game_title,
                u.delete_game_body,
                Some(&detail),
                &[
                    Choice { label: u.cancel, destructive: false },
                    Choice { label: lc.opt_delete, destructive: true },
                ],
                selection,
                true,
            );
        }
        Screen::ProfileDeleteConfirm { profile_idx, selection, .. } => {
            let name = LIBRARY
                .lock()
                .ok()
                .and_then(|s| s.profile_matches.get(profile_idx).map(|m| menu::profile_name(&m.profile)))
                .unwrap_or_default();
            backend.draw_confirm(
                Icon::Trash,
                lc.profile_del_confirm,
                u.profile_del_body,
                Some(&name),
                &[
                    Choice { label: u.cancel, destructive: false },
                    Choice { label: lc.opt_delete, destructive: true },
                ],
                selection,
                true,
            );
        }
        Screen::GameFolderDelete { folder_idx, selection, .. } => {
            let folders = LIBRARY.lock().map(|s| folder_counts(&s.entries)).unwrap_or_default();
            if let Some((name, count)) = folders.get(folder_idx) {
                // Says WHICH folder and HOW MANY games leave it. Nothing is
                // deleted, so the panel is not red; only the button that takes
                // the folder away is.
                let title = u.folder_empty_title.replacen("{}", name, 1);
                let body = std::format!(
                    "{}. {}",
                    crate::loc::fill_count(lc.folder_leave_1, lc.folder_leave_n, *count as i32),
                    u.folder_empty_body,
                );
                backend.draw_confirm(
                    Icon::Folder,
                    &title,
                    &body,
                    None,
                    &[
                        Choice { label: lc.games_dir_confirm_no, destructive: false },
                        Choice { label: u.empty_verb, destructive: true },
                    ],
                    selection,
                    false,
                );
            }
        }
        Screen::CoverPicker { selection, .. } => {
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
                let urls: std::vec::Vec<std::string::String> = s
                    .cover_candidates
                    .iter()
                    .map(|c| cover_url_for_source(&c.cover_url, s.cover_use_shot))
                    .collect();
                (titles, urls, s.cover_msg.clone(), s.cover_use_shot)
            });
            if let Some((titles, urls, msg, use_shot)) = snap {
                let title_refs: std::vec::Vec<&str> = titles.iter().map(|x| x.as_str()).collect();
                let url_refs: std::vec::Vec<&str> = urls.iter().map(|x| x.as_str()).collect();
                backend.draw_library_cover_picker(&entry.display_name, selection, &title_refs, &url_refs, &msg, use_shot);
            }
        }
        _ => {}
    }
}
