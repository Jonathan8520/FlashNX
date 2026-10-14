//! Flashpoint search: the results as a grid of logos, and a game's details.
//!
//! The grid is the Play tab's: the same tiles (16:10, gap 20, page margin 48),
//! each opening on its own clock when selected and lifting off the grid, the
//! logo whole on its blurred backdrop, and under the grid the selected game's
//! title with who made it and when, instead of a caption cut in the middle
//! under every tile. While a search runs the page is already there, its tiles
//! waiting, so nothing jumps when the results land; a search that failed says
//! so in red with a way to try again, and one that found nothing says that.

use super::super::kit::icons::Icon;
use super::super::kit::theme::{col, radius, ring, size};
use super::super::*;

/// What the results page is showing.
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum FpState {
    /// The search is on its way.
    Loading,
    Results,
    /// The search worked and found nothing.
    Empty,
    /// The search could not be made (no network, a timeout).
    Error,
}

/// The results page, as data.
pub(crate) struct FpGrid<'a> {
    pub query: &'a str,
    pub state: FpState,
    /// The error's message, for `FpState::Error`.
    pub msg: &'a str,
    pub selection: usize,
    pub scroll: usize,
    pub titles: &'a [&'a str],
    pub devs: &'a [&'a str],
    pub years: &'a [&'a str],
    pub urls: &'a [&'a str],
    pub installed: &'a [bool],
    /// The family filter's state, shown in its hint.
    pub filter_on: bool,
    /// The order in force, as its outcome.
    pub sort: &'a str,
    /// False under a window or a reveal: no shared glide, no touch table.
    pub interactive: bool,
}

/// How far a tile grows when it opens, as on the Play grid.
const LIFT: f32 = 0.05;

impl SwitchRenderBackend {
    /// The results grid (see `FpGrid`).
    pub(crate) fn draw_library_fp_gallery(&mut self, g: &FpGrid) {
        use crate::library::{fp_tile_rect, FP_GALLERY_COLS as COLS, FP_GALLERY_ROWS as ROWS, FP_GRID_TOP as TOP};
        self.library_clear();
        let vw = self.dimensions.width as f32;
        let lc = crate::loc::s();
        let u = crate::loc_ui::ui();
        let n = if g.state == FpState::Results { g.urls.len() } else { 0 };
        let q = super::importer::imp_quote(g.query);
        let sub = match g.state {
            FpState::Loading => u.imp_fp_searching.replacen("{}", &q, 1),
            FpState::Results => std::format!("{}  \u{00B7}  {}", q, crate::loc_ui::games(n)),
            _ => q,
        };
        let badge_w = if n > 1 { self.imp_sort_badge(vw - crate::library::IMPORTER_MARGIN, 102.0, g.sort) + 24.0 } else { 0.0 };
        let sub = self.kit_fit(&sub, size::LABEL + 1.0, vw - 2.0 * crate::library::IMPORTER_MARGIN - badge_w - 40.0);
        let logo = self.fp_logo();
        self.kit_page_header_logo(&[u.tab_import], lc.fp_title, Some(&sub), logo);
        let now = unsafe { ruffle_tick_now() };
        if g.state == FpState::Loading {
            // Something moving beside "Recherche de « mario »…", so a slow
            // search does not look like a frozen page.
            let sw = self.kit_measure(&sub, size::LABEL + 1.0);
            self.draw_spinner(crate::library::IMPORTER_MARGIN + sw + 24.0, 102.0, 8.0, now);
        }
        // No tiles to tap but on a page of results: the last page's table must
        // not stay live under a finger (a stale hit would select a game that is
        // not there, or try the search again).
        let (_, _, tile_w, tile_h) = fp_tile_rect(0, 0.0, vw);
        let pitch = tile_h + crate::library::FP_GRID_GAP;
        let band_top = TOP - 12.0;
        let band_bot = TOP + ROWS as f32 * pitch - crate::library::FP_GRID_GAP + 8.0;
        if g.interactive && g.state != FpState::Results {
            if let Ok(mut c) = gallery_cache().lock() {
                *c = (std::vec::Vec::new(), 0);
            }
            if let Ok(mut v) = gallery_view().lock() {
                *v = GalleryView { pitch, band_top, band_bot, rows_visible: ROWS as u32, ..GalleryView::default() };
            }
        }

        match g.state {
            FpState::Loading => {
                // The tiles the results will fill, waiting.
                for i in 0..COLS * ROWS {
                    let (x, y, w, h) = fp_tile_rect(i, 0.0, vw);
                    self.kit_rrect(x, y, w, h, radius::TILE, col::TILE);
                    self.kit_icon(x + (w - 28.0) * 0.5, y + h * 0.5, 28.0, Icon::Hourglass, col::TEXT3);
                }
                self.kit_footer_band();
                self.imp_footer_hints(&[(&[Btn::B][..], u.cancel)]);
                return self.fp_done();
            }
            FpState::Empty => {
                self.imp_empty_state_fp(Icon::Search, None, u.imp_fp_none, (band_top + band_bot) * 0.5);
                self.kit_footer_band();
                let filter = u.fp_filter.replacen("{}", if g.filter_on { u.imp_filter_on } else { u.imp_filter_off }, 1);
                let chord = [Btn::ZL, Btn::ZR];
                self.imp_footer_hints(&[
                    (&[Btn::Minus][..], u.imp_new_search),
                    (&[Btn::B][..], u.back),
                    (&chord[..], filter.as_str()),
                ]);
                return self.fp_done();
            }
            FpState::Error => {
                self.imp_empty_state_fp(Icon::WifiOff, Some(u.imp_err_search), g.msg, (band_top + band_bot) * 0.5);
                self.kit_footer_band();
                self.imp_footer_hints(&[
                    (&[Btn::A][..], u.imp_retry),
                    (&[Btn::Minus][..], u.imp_new_search),
                    (&[Btn::B][..], u.back),
                ]);
                return self.fp_done();
            }
            FpState::Results => {}
        }

        self.pump_thumbnail_load();
        let rows_total = n.div_ceil(COLS);
        let sel = g.selection.min(n.saturating_sub(1));
        let target_scroll = g.scroll as f32 * pitch;
        let (tx0, ty0, _, _) = fp_tile_rect(sel, 0.0, vw);
        let (mut scroll_px, mut fx, mut fy, mut pop) = (target_scroll, tx0, ty0, 0.0f32);
        if g.interactive {
            let mut cells: std::vec::Vec<GalleryCell> = std::vec::Vec::with_capacity(n);
            for i in 0..n {
                let (x, y, w, h) = fp_tile_rect(i, 0.0, vw);
                cells.push(GalleryCell { row: (i / COLS) as u32, cx: x + w * 0.5, x, y, w, h });
            }
            if let Ok(mut c) = gallery_cache().lock() {
                *c = (cells, rows_total as u32);
            }
            let touch_scroll = gallery_touch_scroll_read();
            if let Ok(mut a) = gallery_anim().lock() {
                if !a.inited {
                    a.inited = true;
                    a.last_tick = now;
                    a.last_sel = sel;
                    a.sel_x = tx0;
                    a.sel_y = ty0;
                    a.sel_w = tile_w;
                    a.scroll_px = target_scroll;
                    a.pop = 0.0;
                } else {
                    let freq = unsafe { ruffle_tick_freq() } as f32;
                    let dt = if freq > 0.0 { (now.saturating_sub(a.last_tick) as f32 / freq).min(0.1) } else { 1.0 / 60.0 };
                    a.last_tick = now;
                    if sel != a.last_sel {
                        a.pop = 1.0;
                        a.last_sel = sel;
                    }
                    // The Play grid's rates, so the two grids move alike;
                    // down the grid, the scroll's own rate, so the ring cannot
                    // run past its tile while a long jump scrolls the grid.
                    a.sel_x = ease_to(a.sel_x, tx0, dt, 18.0);
                    a.sel_y = ease_to(a.sel_y, ty0, dt, 16.0);
                    a.sel_w = ease_to(a.sel_w, tile_w, dt, 18.0);
                    a.scroll_px = ease_to(a.scroll_px, target_scroll, dt, 16.0);
                    a.pop = ease_to(a.pop, 0.0, dt, 12.0);
                }
                if let Some(px) = touch_scroll {
                    a.scroll_px = px;
                }
                scroll_px = a.scroll_px;
                fx = a.sel_x;
                fy = a.sel_y;
                pop = a.pop;
            }
            if let Ok(mut v) = gallery_view().lock() {
                *v = GalleryView {
                    scroll_px,
                    pitch,
                    band_top,
                    band_bot,
                    rows_total: rows_total as u32,
                    rows_visible: ROWS as u32,
                    horizontal: false,
                    off_min: 0.0,
                    off_max: 0.0,
                };
            }
            let (sx, sy, sw, sh) = fp_tile_rect(sel, scroll_px, vw);
            super::importer::import_sel_rect_set((sx, sy, sw, sh));
        }

        self.set_clip(0.0, band_top, vw, band_bot - band_top);
        // Each tile opens to its whole logo while selected and closes on its
        // own clock when left, as on the Play grid.
        tile_open_frame();
        let mut selected_open = 0.0f32;
        for i in 0..n {
            let (x0, y0, w0, h0) = fp_tile_rect(i, scroll_px, vw);
            if y0 + h0 < band_top - 8.0 || y0 > band_bot + 8.0 {
                continue;
            }
            let b = tile_open(tile_key(g.urls[i]), i == sel);
            if i == sel {
                selected_open = b;
            }
            let grow = LIFT * b;
            let (x, y, w, h) = (x0 - w0 * grow * 0.5, y0 - h0 * grow * 0.5, w0 * (1.0 + grow), h0 * (1.0 + grow));
            if b > 0.0 {
                self.kit_shadow(x, y + 8.0, w, h, radius::TILE, 16.0, ((0x90 as f32 * b) as u32) << 24);
            }
            match self.thumb_for(g.urls[i]) {
                Some(ThumbTex::Image { tex, w: tw, h: th, amb }) => {
                    // At rest as filled as the 15 % rule allows, the rule the
                    // Play grid follows, so a cover looks the same on both: a
                    // banner logo rested cropped to a few of its letters here
                    // ("Walk the Stork" showed "the S"). The selected tile
                    // opens the rest of the way to the whole image.
                    let rest = super::super::kit::cover::rest_t(tw, th, w0, h0);
                    self.kit_cover_reveal(x, y, w, h, tex, tw, th, amb, rest + (1.0 - rest) * b, 1.0);
                }
                Some(ThumbTex::Failed) => {
                    // No logo: the tile carries the game's name, as a game
                    // without a cover does on the Play grid.
                    let title = g.titles.get(i).copied().unwrap_or("");
                    self.kit_default_tile(x, y, w, h, tile_tint(g.urls[i]), title);
                }
                None => {
                    self.kit_rrect(x, y, w, h, radius::TILE, col::TILE);
                    self.kit_icon(x + (w - 28.0) * 0.5, y + h * 0.5, 28.0, Icon::Hourglass, col::TEXT3);
                }
            }
            if g.installed.get(i).copied().unwrap_or(false) {
                // Already on the card.
                const CHIP: f32 = 26.0;
                let (bx, by) = (x + w - CHIP - 8.0, y + 8.0);
                self.kit_rrect(bx, by, CHIP, CHIP, 8.0, 0xB8_0A_10_1C);
                self.kit_icon(bx + 5.0, by + CHIP * 0.5, 16.0, Icon::CircleCheck, col::OK);
            }
        }
        // The ring hugs the lifted tile: the same growth, plus the brief snap
        // right after a move. Under a clip of its own, as tall as the band
        // plus what the lift, the snap and the ring itself add round a tile:
        // under the tiles' clip the bottom of the ring was cut on the third
        // row, and gone during the snap.
        self.clear_clip();
        let lift = LIFT * selected_open;
        let grow = pop * 3.0;
        let margin = (LIFT * tile_h * 0.5 + 3.0 + ring::GAP + ring::WIDTH + 1.0).ceil();
        self.set_clip(0.0, band_top - margin, vw, band_bot - band_top + 2.0 * margin);
        let rx = fx - tile_w * lift * 0.5 - grow;
        let ry = fy - scroll_px - tile_h * lift * 0.5 - grow;
        self.kit_ring(rx, ry, tile_w * (1.0 + lift) + 2.0 * grow, tile_h * (1.0 + lift) + 2.0 * grow, radius::TILE, ring::GAP, ring::WIDTH, col::GOLD);
        self.clear_clip();
        self.draw_scrollbar(
            vw - super::importer::IMPORT_SCROLLBAR_INSET,
            TOP,
            ROWS as f32 * pitch - crate::library::FP_GRID_GAP,
            scroll_px,
            pitch,
            ROWS,
            rows_total,
        );

        // The selected game under the grid, as on the Play tab: its title,
        // then who made it, when, and whether it is already on the card.
        if let Some(title) = g.titles.get(sel) {
            let x = crate::library::IMPORTER_MARGIN;
            let maxw = vw - 2.0 * x;
            let t = self.kit_fit(title, size::TITLE, maxw);
            self.kit_text_mid(x, 616.0, size::TITLE, &t, col::GOLD);
            let mut fx = x;
            let mut fact = |s: &mut Self, icon: Icon, text: &str, ink: u32| {
                if text.is_empty() || fx > x + maxw - 60.0 {
                    return;
                }
                s.kit_icon(fx, 648.0, 16.0, icon, ink);
                let tt = s.kit_fit(text, size::LABEL, x + maxw - fx - 23.0);
                fx += 16.0 + 7.0 + s.kit_text_mid(fx + 23.0, 648.0, size::LABEL, &tt, col::TEXT2) + 22.0;
            };
            fact(self, Icon::User, g.devs.get(sel).copied().unwrap_or(""), col::TEXT3);
            fact(self, Icon::Calendar, g.years.get(sel).copied().unwrap_or(""), col::TEXT3);
            if g.installed.get(sel).copied().unwrap_or(false) {
                fact(self, Icon::CircleCheck, u.on_card, col::OK);
            }
        }

        // A on a game already on the card plays it.
        self.kit_footer_band();
        let have = g.installed.get(sel).copied().unwrap_or(false);
        let filter = u.fp_filter.replacen("{}", if g.filter_on { u.imp_filter_on } else { u.imp_filter_off }, 1);
        let chord = [Btn::ZL, Btn::ZR];
        let mut hints: std::vec::Vec<kit::text::HintItem> = std::vec![
            (&[Btn::A][..], if have { u.hint_play } else { u.download }),
            (&[Btn::Plus][..], u.info),
            (&[Btn::Minus][..], u.imp_new_search),
        ];
        if n > 1 {
            hints.push((&[Btn::Y][..], u.hint_sort));
        }
        hints.push((&[Btn::B][..], u.back));
        // The family filter is a hidden chord: shown when there is room, and
        // the first to go when there is not.
        hints.push((&chord[..], filter.as_str()));
        self.imp_footer_hints(&hints);
        self.fp_done()
    }

    fn fp_done(&mut self) {
        unsafe {
            glUseProgram(0);
            glBindVertexArray(0);
        }
        self.gl_state.invalidate();
    }

    /// An icon, an optional title and a sentence, centred around `cy` in the
    /// grid's place: no results, or a search that failed (the icon in red).
    fn imp_empty_state_fp(&mut self, icon: Icon, title: Option<&str>, text: &str, cy: f32) {
        let vw = self.dimensions.width as f32;
        let lines = self.kit_wrap(text, size::BODY, 680.0, 4);
        let lh = 26.0;
        let block = 46.0 + if title.is_some() { 36.0 } else { 0.0 } + lines.len() as f32 * lh;
        let mut y = cy - block * 0.5;
        let ink = if title.is_some() { col::DANGER } else { col::TEXT3 };
        self.kit_icon(((vw - 40.0) * 0.5).round(), y + 20.0, 40.0, icon, ink);
        y += 46.0 + 14.0;
        if let Some(t) = title {
            let tw = self.kit_measure(t, size::WINDOW_TITLE);
            self.kit_text_mid(((vw - tw) * 0.5).round(), y, size::WINDOW_TITLE, t, col::TEXT);
            y += 36.0;
        }
        for line in &lines {
            let w = self.kit_measure(line, size::BODY);
            self.kit_text_mid(((vw - w) * 0.5).round(), y, size::BODY, line, col::TEXT2);
            y += lh;
        }
    }
}

/// A quiet colour of its own for a tile whose logo did not come, from what it
/// shows, so two such tiles side by side do not look like one.
pub(super) fn tile_tint(key: &str) -> u32 {
    const TINTS: [u32; 6] = [0x5B_7F_D9, 0x8C_7C_CF, 0x4F_A8_9C, 0xC2_8A_4A, 0xB8_5E_7E, 0x6F_9A_4F];
    TINTS[(tile_key(key) % TINTS.len() as u64) as usize]
}

/// A result's details, as data.
pub(crate) struct FpDetailsView<'a> {
    pub title: &'a str,
    pub developer: &'a str,
    pub publisher: &'a str,
    /// Already written for the reader ("5 juin 2009", "2009").
    pub release: &'a str,
    /// The download's size: `None` while it is being read, `Some(0)` when the
    /// server did not say.
    pub size: Option<u64>,
    pub cover_url: &'a str,
    /// Its screenshot, under the logo (`discover::screenshot_url`, the one the
    /// shelves show, so a shelf's game has it already). Empty for none.
    pub shot_url: &'a str,
    /// The blurb, empty for none. The search gives it with the game.
    pub description: &'a str,
    pub installed: bool,
}

impl SwitchRenderBackend {
    /// A result's details: its pictures down the left, the logo then the
    /// screenshot, each whole on its backdrop; beside them the title, who made
    /// and published it, when, how big the download is, and its blurb. Opens
    /// at once on what the search already knows; a size it did not know fills
    /// in when it arrives, a spinner holding its place.
    pub(crate) fn draw_library_fp_details(&mut self, d: &FpDetailsView) {
        const W: f32 = 960.0;
        const PAD: f32 = 36.0;
        const ART_W: f32 = 240.0;
        const ART_H: f32 = 150.0;
        const ART_GAP: f32 = 14.0;
        const MAX_H: f32 = 620.0;
        let u = crate::loc_ui::ui();
        self.pump_thumbnail_load();
        let image = |t: Option<ThumbTex>| match t {
            Some(ThumbTex::Image { tex, w, h, amb }) if tex != 0 => Some((tex, w, h, amb)),
            _ => None,
        };
        let cover = image(self.thumb_for(d.cover_url));
        let shot_tex = if d.shot_url.is_empty() { Some(ThumbTex::Failed) } else { self.thumb_for(d.shot_url) };
        let shot = image(shot_tex);
        // Facts as icon and value, as the game options write theirs: "Label :
        // value" put a French space before the colon in every language.
        let mut facts: std::vec::Vec<(Icon, std::string::String, u32)> = std::vec::Vec::new();
        if !d.developer.is_empty() {
            facts.push((Icon::User, d.developer.to_string(), col::TEXT2));
        }
        if !d.publisher.is_empty() && d.publisher != d.developer {
            facts.push((Icon::Users, d.publisher.to_string(), col::TEXT2));
        }
        if !d.release.is_empty() {
            facts.push((Icon::Calendar, d.release.to_string(), col::TEXT2));
        }
        let size_pending = d.size.is_none();
        match d.size {
            Some(0) => {}
            Some(b) => facts.push((Icon::Download, super::importer::imp_size(b), col::TEXT2)),
            None => facts.push((Icon::Download, std::string::String::new(), col::TEXT2)),
        }
        if d.installed {
            facts.push((Icon::CircleCheck, u.on_card.to_string(), col::OK));
        }

        let text_x0 = PAD + ART_W + 28.0;
        let text_w = W - text_x0 - PAD;
        let title_lines = self.kit_wrap(d.title, size::WINDOW_TITLE, text_w, 2);
        let head_h = title_lines.len() as f32 * 30.0 + 14.0 + facts.len() as f32 * 28.0;
        // The blurb runs under the facts, beside the pictures, and gets what
        // the window has room for, up to its full height, rather than six lines
        // and an ellipsis.
        let room = MAX_H - PAD - head_h - 24.0 - 30.0 - 60.0;
        let max_lines = ((room / 23.0).floor() as usize).max(2);
        let desc_lines = self.kit_wrap(d.description, size::LABEL + 1.0, text_w, max_lines);
        // The screenshot's place is settled when the window opens: a game known
        // to have none shows its logo alone, and one found to have none once
        // the window is up keeps an empty tile rather than the window shrinking
        // under the reader, recentred, the title and the facts moving.
        static SHOT_SLOT: std::sync::Mutex<(std::string::String, bool)> =
            std::sync::Mutex::new((std::string::String::new(), true));
        let id = std::format!("{}\u{1F}{}", d.title, d.cover_url);
        let has_shot = !matches!(shot_tex, Some(ThumbTex::Failed));
        let shot_slot = match SHOT_SLOT.lock() {
            Ok(mut g) => {
                if g.0 != id {
                    *g = (id, has_shot);
                }
                g.1
            }
            Err(_) => has_shot,
        };
        let art_h = if shot_slot { 2.0 * ART_H + ART_GAP } else { ART_H };
        let desc_h = if desc_lines.is_empty() { 0.0 } else { 24.0 + desc_lines.len() as f32 * 23.0 };
        let h = PAD + art_h.max(head_h + desc_h) + 30.0 + 60.0;
        let frame = self.draw_modal_frame(W, 0, Some(h), false, "", None, None);
        let (x, y) = (frame.x, frame.y);
        let (ax, ay) = (x + PAD, y + PAD);
        let mut arts = std::vec![(ay, cover)];
        if shot_slot {
            arts.push((ay + ART_H + ART_GAP, shot));
        }
        for (py, art) in arts {
            match art {
                Some((tex, w, h, amb)) => self.kit_cover(ax, py, ART_W, ART_H, tex, w, h, amb, 1.0),
                None => {
                    self.kit_rrect(ax, py, ART_W, ART_H, radius::TILE, col::TILE);
                    self.kit_icon(ax + (ART_W - 36.0) * 0.5, py + ART_H * 0.5, 36.0, Icon::Image, col::TEXT3);
                }
            }
        }
        let tx = x + text_x0;
        let mut ty = ay + 14.0;
        for line in &title_lines {
            self.kit_text_mid(tx, ty, size::WINDOW_TITLE, line, col::TEXT);
            ty += 30.0;
        }
        ty += 6.0;
        let now = unsafe { ruffle_tick_now() };
        for (icon, text, ink) in &facts {
            self.kit_icon(tx, ty, 16.0, *icon, if *ink == col::OK { col::OK } else { col::TEXT3 });
            if text.is_empty() && size_pending && *icon == Icon::Download {
                self.draw_spinner(tx + 26.0 + 9.0, ty, 7.0, now);
            } else {
                let t = self.kit_fit(text, size::LABEL + 1.0, text_w - 26.0);
                self.kit_text_mid(tx + 26.0, ty, size::LABEL + 1.0, &t, *ink);
            }
            ty += 28.0;
        }
        let mut ly = ay + head_h + 24.0;
        for line in &desc_lines {
            self.kit_text_mid(tx, ly, size::LABEL + 1.0, line, col::TEXT3);
            ly += 23.0;
        }
        self.modal_hints(
            &frame,
            &[(&[Btn::A], if d.installed { u.hint_play } else { u.download }), (&[Btn::B], u.back)],
            false,
        );
        self.fp_done();
    }
}
