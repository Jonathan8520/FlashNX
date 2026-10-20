//! The windows and the page of FlashNX's own update (`crate::updater`): the
//! offer, over the Play grid once per version or from the À propos row; the
//! download page; and the window saying the new version is in place, which
//! can start FlashNX again.

use super::*;
use crate::backend::render::{Btn, Choice, HintItem, Icon};
use crate::updater::Progress;

/// The page an update's window goes back to.
pub(crate) fn back_to(from: UpdFrom) -> Screen {
    match from {
        UpdFrom::Gallery { selection, scroll } => Screen::List { selection, scroll_offset: scroll },
        UpdFrom::About => settings_at(SetSection::About, "MAJ"),
    }
}

/// Once per frame, before the screen is read: the check, the window offered
/// once per version over the Play grid at rest, and the download page's work.
pub(crate) fn drive() {
    crate::updater::drive();
    let Ok(mut s) = LIBRARY.lock() else { return };
    match s.screen {
        Screen::List { selection, scroll_offset } => {
            // Never over a window still closing onto the grid.
            if PENDING_AFTER_CLOSE.lock().map(|g| g.is_some()).unwrap_or(true) {
                return;
            }
            if crate::updater::take_offer().is_some() {
                OFFER_RAISED_AT.store(unsafe { ruffle_tick_now() }, std::sync::atomic::Ordering::Relaxed);
                s.screen = Screen::UpdateOffer {
                    selection: 1,
                    from: UpdFrom::Gallery { selection, scroll: scroll_offset },
                };
            }
        }
        Screen::UpdateWorking { from } => match crate::updater::tick_install() {
            Progress::Pending => {}
            Progress::Done => s.screen = Screen::UpdateDone { selection: 1, from },
            Progress::Failed(msg) => {
                s.screen = back_to(from);
                set_toast(&mut s, msg, TOAST_ERR);
            }
        },
        _ => {}
    }
}

/// À propos > Mise à jour, A: the offer when a newer version is known, the
/// installed window when it is in place, else a check now.
pub(crate) fn about_row_a(s: &mut State) {
    // Debug marker: A starts FlashNX again at once, with no update, so the
    // restart can be tried on its own.
    if crate::sources::gamezip::read_file_bounded("sdmc:/switch/FlashNX/restart_test.on", 4).is_some() {
        log("update: restart_test.on present -> starting FlashNX again\n");
        if crate::updater::request_restart() {
            s.screen = Screen::Quit;
        }
        return;
    }
    if crate::updater::installed().is_some() {
        s.screen = Screen::UpdateDone { selection: 1, from: UpdFrom::About };
        return;
    }
    if crate::updater::available().is_some() {
        s.screen = Screen::UpdateOffer { selection: 1, from: UpdFrom::About };
        return;
    }
    if !crate::updater::check_now() {
        set_toast(s, crate::loc::s().err_no_internet.to_string(), TOAST_ERR);
    }
}

/// When the offer last came up on its own over the grid (ticks).
static OFFER_RAISED_AT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
/// A is not taken for this long after the offer came up on its own: the player
/// may have been pressing A for a game just then, and that press must not be
/// the one that installs.
const OFFER_A_DEAD_MS: u64 = 500;

/// The offer: Plus tard (0) or Installer (1), side by side, starting on
/// Installer (Jonathan: the answer the window is there for).
pub(crate) fn offer_input(s: &mut State, button: &str, selection: usize, from: UpdFrom) {
    if button == "A" && matches!(from, UpdFrom::Gallery { .. }) {
        let raised = OFFER_RAISED_AT.load(std::sync::atomic::Ordering::Relaxed);
        let now = unsafe { ruffle_tick_now() };
        let freq = unsafe { ruffle_tick_freq() }.max(1);
        if now.saturating_sub(raised) < freq * OFFER_A_DEAD_MS / 1000 {
            crate::sfx::hush();
            return;
        }
    }
    match button {
        "Left" | "StickLLeft" | "Right" | "StickLRight" => {
            s.screen = Screen::UpdateOffer { selection: answer_at(button, selection), from };
        }
        "A" if selection == 1 => match crate::updater::start_install() {
            Ok(()) => s.screen = Screen::UpdateWorking { from },
            Err(msg) => {
                s.screen = back_to(from);
                set_toast(s, msg, TOAST_ERR);
            }
        },
        "A" | "B" | "Minus" => s.screen = back_to(from),
        _ => {}
    }
}

/// The download page: B stops it, and nothing of it stays.
pub(crate) fn working_input(s: &mut State, button: &str, from: UpdFrom) {
    if matches!(button, "B" | "Minus") {
        crate::updater::cancel_install();
        s.screen = back_to(from);
    }
}

/// In place: Plus tard (0) or Redémarrer (1), or a notice to close when this
/// loader cannot start FlashNX again.
pub(crate) fn done_input(s: &mut State, button: &str, selection: usize, from: UpdFrom) {
    let can = crate::updater::restart_supported();
    match button {
        "Left" | "StickLLeft" | "Right" | "StickLRight" if can => {
            s.screen = Screen::UpdateDone { selection: answer_at(button, selection), from };
        }
        "A" if can && selection == 1 => {
            if crate::updater::request_restart() {
                s.screen = Screen::Quit;
            } else {
                s.screen = back_to(from);
                set_toast(s, crate::loc_ui::ui().upd_done_manual.to_string(), TOAST_INFO);
            }
        }
        "A" | "B" | "Minus" => s.screen = back_to(from),
        _ => {}
    }
}

/// The page under a window: the Play grid it came up over, or the RÉGLAGES
/// tab on À propos, still and out of reach of the finger.
pub(crate) fn draw_behind(
    backend: &mut SwitchRenderBackend,
    from: UpdFrom,
    anim_origin: u64,
    folder: (&str, &[std::string::String]),
    game_count: usize,
) {
    let logo = LIBRARY.lock().map(|s| (s.banner_tex, s.banner_w, s.banner_h)).unwrap_or((0, 0, 0));
    match from {
        UpdFrom::Gallery { selection, scroll } => {
            draw_gallery(backend, selection, scroll, anim_origin);
            backend.draw_navbar_with(Tab::Jouer.index(), logo, false);
        }
        UpdFrom::About => {
            // The About pane: the library's size is not on it.
            settings_tab::draw_settings(backend, back_to(from), folder, logo, game_count, 0, true);
            backend.draw_navbar_with(Tab::Reglages.index(), logo, false);
        }
    }
    crate::backend::render::navbar_clear();
}

/// The offer or the installed window, over `draw_behind`.
pub(crate) fn draw_window(backend: &mut SwitchRenderBackend, screen: Screen) {
    let u = crate::loc_ui::ui();
    match screen {
        Screen::UpdateOffer { selection, .. } => {
            let Some(r) = crate::updater::available() else { return };
            let title = u.upd_offer_title.replacen("{}", &r.version, 1);
            let body = u
                .upd_offer_body
                .replacen("{}", crate::bugreport::APP_VERSION, 1)
                .replacen("{}", &crate::backend::render::imp_size(r.size), 1);
            backend.draw_confirm(
                Icon::CloudDownload,
                &title,
                &body,
                None,
                &[Choice { label: u.upd_later, destructive: false }, Choice { label: u.install, destructive: false }],
                selection,
                false,
                u.upd_later,
            );
        }
        Screen::UpdateDone { selection, .. } => {
            let version = crate::updater::installed().unwrap_or_default();
            let title = u.upd_done_title.replacen("{}", &version, 1);
            if crate::updater::restart_supported() {
                backend.draw_confirm(
                    Icon::CircleCheck,
                    &title,
                    u.upd_done_restart,
                    None,
                    &[
                        Choice { label: u.upd_later, destructive: false },
                        Choice { label: u.upd_restart, destructive: false },
                    ],
                    selection,
                    false,
                    u.upd_later,
                );
            } else {
                backend.draw_confirm(
                    Icon::CircleCheck,
                    &title,
                    u.upd_done_manual,
                    None,
                    &[Choice { label: u.close, destructive: false }],
                    0,
                    false,
                    u.close,
                );
            }
        }
        _ => {}
    }
}

/// The download page: the version, a bar, and how much has arrived.
pub(crate) fn draw_page(backend: &mut SwitchRenderBackend) {
    let u = crate::loc_ui::ui();
    let version = crate::updater::available().map(|r| r.version).unwrap_or_default();
    let name = std::format!("FlashNX {}", version);
    let (frac, detail) = if crate::updater::verifying() {
        (Some(1.0), u.upd_verifying.to_string())
    } else {
        let (done, total) = crate::net::download_progress();
        let frac = (total > 0).then(|| (done as f32 / total as f32).clamp(0.0, 1.0));
        let detail = if total > 0 {
            crate::backend::render::imp_n_of(
                &crate::backend::render::imp_amount(done),
                &crate::backend::render::imp_size(total),
            )
        } else {
            crate::backend::render::imp_amount(done)
        };
        (frac, detail)
    };
    let hints: [HintItem; 1] = [(&[Btn::B], u.cancel)];
    backend.draw_progress_page(Icon::CloudDownload, u.upd_page_title, &name, frac, &detail, &hints);
}
