//! Flashpoint search: the results as a grid of logos, and a game's details.
//!
//! Both were drawn in the old style: a centred title, "OK" in a green box on
//! the games already on the card, "?" and "..." for a logo that failed or was
//! still coming, and square tiles with page-coloured corners painted on. The
//! grid now has the Play tab's tiles and ring, an icon for each state, and the
//! details window reads as a game sheet.

use super::super::kit::icons::Icon;
use super::super::kit::theme::{col, radius, ring, size};
use super::super::*;

impl SwitchRenderBackend {
    /// The results grid. `scroll_row` is the first row the input layer wants
    /// on screen; the drawing eases toward it. `filter_on` is the mature
    /// content filter's state, shown in the hints.
    #[allow(clippy::too_many_arguments)]
    pub fn draw_library_fp_gallery(
        &mut self,
        query: &str,
        selection: usize,
        scroll_row: usize,
        titles: &[&str],
        urls: &[&str],
        installed: &[bool],
        msg: &str,
        filter_on: bool,
    ) {
        self.library_clear();
        let vw = self.dimensions.width as f32;
        let vh = self.dimensions.height as f32;
        let lc = crate::loc::s();
        let u = crate::loc_ui::ui();
        let cols = crate::library::FP_GALLERY_COLS;
        let rows_visible = crate::library::FP_GALLERY_ROWS;
        let n = urls.len();

        let q = std::format!("\u{201C}{}\u{201D}", query.trim());
        let grid_top =
            self.kit_page_header(&[u.tab_import], lc.fp_title, if query.trim().is_empty() { None } else { Some(&q) }) + 22.0;

        // The hints first: they are the same whatever the grid holds.
        self.kit_footer_band();
        let filter = u.fp_filter.replacen("{}", if filter_on { lc.lbl_on } else { lc.lbl_off }, 1);
        let have = installed.get(selection).copied().unwrap_or(true);
        let chord = [Btn::ZL, Btn::ZR];
        let hints = [
            (&[Btn::A][..], u.download),
            (&[Btn::Plus][..], u.info),
            (&[Btn::Minus][..], u.hint_search),
            (&[Btn::Y][..], u.hint_sort),
            (&chord[..], filter.as_str()),
            (&[Btn::B][..], u.back),
        ];
        // A on a game already on the card does nothing, so it is not offered.
        let shown_hints: &[kit::text::HintItem] = if n == 0 {
            &hints[2..]
        } else if have {
            &hints[1..]
        } else {
            &hints[..]
        };
        self.kit_hints_right(vw - 40.0, vh - 24.0, shown_hints);

        if n == 0 {
            // Also the error surface of a failed search, so the message is a
            // sentence: wrapped, in a card, under a search icon.
            let m = if msg.is_empty() { lc.cover_none } else { msg };
            let lines = self.kit_wrap(m, size::BODY, 640.0, 6);
            let mut y = vh * 0.5 - 20.0 - lines.len() as f32 * 12.0;
            self.kit_icon(((vw - 40.0) * 0.5).round(), y - 34.0, 40.0, Icon::Search, col::TEXT3);
            for line in &lines {
                let w = self.kit_measure(line, size::BODY);
                self.kit_text_mid(((vw - w) * 0.5).round(), y + 20.0, size::BODY, line, col::TEXT2);
                y += 26.0;
            }
            unsafe {
                glUseProgram(0);
                glBindVertexArray(0);
            }
            self.gl_state.invalidate();
            return;
        }

        const MARGIN: f32 = 40.0;
        const GAP: f32 = 18.0;
        const LABEL_H: f32 = 26.0;
        let grid_bottom = vh - 48.0 - 8.0;
        let inner_w = vw - MARGIN * 2.0 - 16.0;
        let cell_w = (inner_w - GAP * (cols as f32 - 1.0)) / cols as f32;
        let row_h = (grid_bottom - grid_top) / rows_visible as f32;
        let thumb_h = (row_h - LABEL_H - GAP).max(40.0);
        self.pump_thumbnail_load();

        let rows_total = n.div_ceil(cols) as u32;
        let pitch = row_h;
        let mut cells: std::vec::Vec<GalleryCell> = std::vec::Vec::with_capacity(n);
        for i in 0..n {
            let row = (i / cols) as u32;
            let cx = MARGIN + (i % cols) as f32 * (cell_w + GAP);
            cells.push(GalleryCell { row, cx: cx + cell_w * 0.5, x: cx, y: grid_top + row as f32 * pitch, w: cell_w, h: thumb_h });
        }
        if let Ok(mut g) = gallery_cache().lock() {
            *g = (cells, rows_total);
        }
        // Room above the first row for its ring.
        let band_top = grid_top - 12.0;
        let band_bot = grid_bottom;
        let target_scroll = scroll_row as f32 * pitch;
        let target_x = MARGIN + (selection % cols) as f32 * (cell_w + GAP);
        let target_y = grid_top + (selection / cols) as f32 * pitch;
        let (mut scroll_px, mut fx, mut fy, mut pop) = (target_scroll, target_x, target_y, 0.0f32);
        let touch_scroll = gallery_touch_scroll_read();
        if let Ok(mut a) = gallery_anim().lock() {
            let now = unsafe { ruffle_tick_now() };
            if !a.inited {
                a.inited = true;
                a.last_tick = now;
                a.last_sel = selection;
                a.sel_x = target_x;
                a.sel_y = target_y;
                a.sel_w = cell_w;
                a.scroll_px = target_scroll;
                a.pop = 0.0;
            } else {
                let freq = unsafe { ruffle_tick_freq() } as f32;
                let dt = if freq > 0.0 {
                    (now.saturating_sub(a.last_tick) as f32 / freq).min(0.1)
                } else {
                    1.0 / 60.0
                };
                a.last_tick = now;
                if selection != a.last_sel {
                    a.pop = 1.0;
                    a.last_sel = selection;
                }
                // Softer than the Play grid: this one is heavier to draw, and a
                // longer time constant keeps the glide smooth when it dips.
                a.sel_x = ease_to(a.sel_x, target_x, dt, 11.0);
                a.sel_y = ease_to(a.sel_y, target_y, dt, 11.0);
                a.sel_w = ease_to(a.sel_w, cell_w, dt, 11.0);
                a.scroll_px = ease_to(a.scroll_px, target_scroll, dt, 11.0);
                a.pop = ease_to(a.pop, 0.0, dt, 10.0);
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
                rows_total,
                rows_visible: rows_visible as u32,
                horizontal: false,
                off_min: 0.0,
                off_max: 0.0,
            };
        }
        self.set_clip(0.0, band_top, vw, band_bot - band_top);
        // Each tile opens to its whole logo while selected and closes on its
        // own clock when left, as on the Play grid.
        tile_open_frame();
        for i in 0..n {
            let cx = MARGIN + (i % cols) as f32 * (cell_w + GAP);
            let cy = grid_top + (i / cols) as f32 * pitch - scroll_px;
            if cy + thumb_h + LABEL_H < band_top - 8.0 || cy > band_bot + 8.0 {
                continue;
            }
            match self.thumb_for(urls[i]) {
                Some(ThumbTex::Image { tex, w, h }) => {
                    let b = tile_open(tile_key(urls[i]), i == selection);
                    self.kit_cover_reveal(cx, cy, cell_w, thumb_h, tex, w, h, 0, b, 1.0);
                }
                other => {
                    self.kit_rrect(cx, cy, cell_w, thumb_h, radius::TILE, 0xFF_14_1F_36);
                    let icon = if matches!(other, Some(ThumbTex::Failed)) { Icon::Image } else { Icon::Hourglass };
                    self.kit_icon(cx + (cell_w - 28.0) * 0.5, cy + thumb_h * 0.5, 28.0, icon, col::TEXT3);
                }
            }
            if installed.get(i).copied().unwrap_or(false) {
                // Already on the card.
                const CHIP: f32 = 26.0;
                let (bx, by) = (cx + cell_w - CHIP - 6.0, cy + 6.0);
                self.kit_rrect(bx, by, CHIP, CHIP, 8.0, 0xB8_0A_10_1C);
                self.kit_icon(bx + 5.0, by + CHIP * 0.5, 16.0, Icon::CircleCheck, col::OK);
            }
            if let Some(t) = titles.get(i) {
                let px = size::CAPTION + 1.0;
                let shown = self.kit_fit_mid(t, px, cell_w);
                let lw = self.kit_measure(&shown, px);
                let ink = if i == selection { col::TEXT } else { col::TEXT2 };
                self.kit_text_mid((cx + (cell_w - lw) * 0.5).round(), cy + thumb_h + 15.0, px, &shown, ink);
            }
        }
        let grow = pop * 4.0;
        self.kit_ring(fx - grow, fy - scroll_px - grow, cell_w + 2.0 * grow, thumb_h + 2.0 * grow, radius::TILE, ring::GAP, ring::WIDTH, col::GOLD);
        self.clear_clip();
        self.draw_scrollbar(vw - 30.0, grid_top, grid_bottom - grid_top, scroll_px, pitch, rows_visible, rows_total as usize);
        unsafe {
            glUseProgram(0);
            glBindVertexArray(0);
        }
        self.gl_state.invalidate();
    }

    /// A result's details: its logo, title, who made and published it, when,
    /// how big the download is, and its blurb.
    #[allow(clippy::too_many_arguments)]
    pub fn draw_library_fp_details(
        &mut self,
        title: &str,
        developer: &str,
        publisher: &str,
        release_date: &str,
        size_bytes: u64,
        cover_url: &str,
        description: &str,
        installed: bool,
    ) {
        const W: f32 = 860.0;
        const PAD: f32 = 36.0;
        const ART_W: f32 = 220.0;
        const ART_H: f32 = 165.0;
        let lc = crate::loc::s();
        let u = crate::loc_ui::ui();
        let cover = match self.thumb_for(cover_url) {
            Some(ThumbTex::Image { tex, w, h }) if tex != 0 => Some((tex, w, h)),
            _ => None,
        };
        let mut facts: std::vec::Vec<(Icon, std::string::String)> = std::vec::Vec::new();
        if !developer.is_empty() {
            facts.push((Icon::User, std::format!("{} : {}", lc.fp_details_dev, developer)));
        }
        if !publisher.is_empty() {
            facts.push((Icon::Users, std::format!("{} : {}", lc.fp_details_publisher, publisher)));
        }
        if !release_date.is_empty() {
            facts.push((Icon::Calendar, std::format!("{} : {}", lc.fp_details_date, release_date)));
        }
        let size = if size_bytes > 0 { format_size_pretty(size_bytes) } else { std::string::String::from("?") };
        facts.push((Icon::Download, std::format!("{} : {}", lc.fp_details_size, size)));

        let text_x0 = PAD + ART_W + 28.0;
        let text_w = W - text_x0 - PAD;
        let title_lines = self.kit_wrap(title, size::WINDOW_TITLE, text_w, 2);
        let head_h = (title_lines.len() as f32 * 30.0 + 14.0 + facts.len() as f32 * 28.0).max(ART_H);
        let desc_lines = self.kit_wrap(description, size::LABEL + 1.0, W - 2.0 * PAD, 6);
        let desc_h = if desc_lines.is_empty() { 0.0 } else { 24.0 + desc_lines.len() as f32 * 23.0 };
        let h = PAD + head_h + desc_h + 30.0 + 60.0;
        let frame = self.draw_modal_frame(W, 0, Some(h), false, "", None, None);
        let (x, y) = (frame.x, frame.y);
        let (ax, ay) = (x + PAD, y + PAD);
        match cover {
            Some((tex, w, h)) => self.kit_cover(ax, ay, ART_W, ART_H, tex, w, h, 0, 1.0),
            None => {
                self.kit_rrect(ax, ay, ART_W, ART_H, radius::TILE, 0xFF_14_1F_36);
                self.kit_icon(ax + (ART_W - 36.0) * 0.5, ay + ART_H * 0.5, 36.0, Icon::Image, col::TEXT3);
            }
        }
        let tx = x + text_x0;
        let mut ty = ay + 14.0;
        for line in &title_lines {
            self.kit_text_mid(tx, ty, size::WINDOW_TITLE, line, col::TEXT);
            ty += 30.0;
        }
        ty += 6.0;
        for (icon, text) in &facts {
            self.kit_icon(tx, ty, 16.0, *icon, col::TEXT3);
            let t = self.kit_fit(text, size::LABEL + 1.0, text_w - 26.0);
            self.kit_text_mid(tx + 26.0, ty, size::LABEL + 1.0, &t, col::TEXT2);
            ty += 28.0;
        }
        if installed {
            let label = self.kit_fit(u.on_card, size::LABEL, text_w - 26.0);
            self.kit_icon(tx, ty, 16.0, Icon::CircleCheck, col::OK);
            self.kit_text_mid(tx + 26.0, ty, size::LABEL, &label, col::OK);
        }
        let mut dy = ay + head_h + 24.0;
        for line in &desc_lines {
            self.kit_text_mid(ax, dy, size::LABEL + 1.0, line, col::TEXT3);
            dy += 23.0;
        }
        if installed {
            self.modal_hints(&frame, &[(&[Btn::B], u.back)], false);
        } else {
            self.modal_hints(&frame, &[(&[Btn::A], u.download), (&[Btn::B], u.back)], false);
        }
        unsafe {
            glUseProgram(0);
            glBindVertexArray(0);
        }
        self.gl_state.invalidate();
    }
}
