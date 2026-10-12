//! The RÉGLAGES tab: the settings in groups on the left, and beside them a card
//! that says what the row under the cursor is for.
//!
//! The old page was a column of centred capitals, each value glued to its label
//! ("DOSSIER JEUX : /ROMS/FLASHNX"), and nothing said what a row did before it
//! was pressed. A row now has an icon, its value on the right and a chevron when
//! it opens another screen; the card explains it, and for the home layout shows
//! the four layouts instead of naming them.

use super::super::kit::icons::Icon;
use super::super::kit::list::{Row, ScrollRows};
use super::super::kit::theme::{col, radius, ring, size};
use super::super::*;

/// One row of the page.
pub(crate) struct SettingsRow<'a> {
    pub icon: Icon,
    pub label: &'a str,
    /// The current value, on the right of the row and under the card's title.
    /// Empty for an action.
    pub value: &'a str,
    /// What the row is for, in the card.
    pub help: &'a str,
    /// The group the row belongs to. A header opens before the first row of a
    /// group; a group with no name (the quit row) still opens a gap.
    pub section: &'a str,
    /// What A does here, for the hint bar.
    pub verb: &'a str,
    /// A opens another screen: the row gets a chevron.
    pub opens: bool,
    pub extra: SettingsExtra<'a>,
}

/// What the card shows under its explanation.
pub(crate) enum SettingsExtra<'a> {
    None,
    /// The four home layouts, the current one ringed.
    HomeViews(u8),
    /// The games folder, and how many games the library holds.
    Folder(&'a str, usize),
}

/// Where the list starts, under the navbar band.
const TOP: f32 = 92.0;
const ROW_H: f32 = 50.0;
const PILL_H: f32 = 44.0;
const SECTION_H: f32 = 34.0;
const GAP_H: f32 = 14.0;
const SIDE: f32 = 40.0;
const COL_GAP: f32 = 36.0;
const ICON_PX: f32 = 22.0;

impl SwitchRenderBackend {
    pub fn draw_library_settings(&mut self, selection: usize, rows: &[SettingsRow]) {
        self.library_clear();
        let vw = self.dimensions.width as f32;
        let vh = self.dimensions.height as f32;
        let kind = ui_screen_kind();
        let now = unsafe { ruffle_tick_now() };
        let list_x = SIDE;
        let list_w = ((vw - 2.0 * SIDE - COL_GAP) * 0.47).round();
        let card_x = list_x + list_w + COL_GAP;
        let card_w = vw - SIDE - card_x;
        let bottom = vh - 48.0 - 18.0;

        // Lay the rows out first: a header (or a bare gap) opens before a row
        // whose group differs from the one above it.
        let mut centres: std::vec::Vec<f32> = std::vec::Vec::with_capacity(rows.len());
        let mut y = TOP;
        for (i, r) in rows.iter().enumerate() {
            if i == 0 || rows[i - 1].section != r.section {
                if r.section.is_empty() {
                    if i > 0 {
                        y += GAP_H;
                    }
                } else {
                    self.kit_text_mid(list_x + 16.0, y + SECTION_H - 13.0, size::CAPTION + 1.0, r.section, col::TEXT3);
                    y += SECTION_H;
                }
            }
            centres.push(y + ROW_H * 0.5);
            y += ROW_H;
        }

        let sel = selection.min(rows.len().saturating_sub(1));
        if let Some(&c) = centres.get(sel) {
            let hy = eased_list_y(c - PILL_H * 0.5, kind, now);
            self.kit_focus_pill(list_x, hy, list_w, PILL_H);
        }
        let mut cells: std::vec::Vec<(f32, f32, f32, f32)> = std::vec::Vec::with_capacity(rows.len());
        for (i, r) in rows.iter().enumerate() {
            let cy = centres[i];
            let focused = i == sel;
            self.kit_icon(list_x + 16.0, cy, ICON_PX, r.icon, if focused { col::GOLD } else { col::TEXT2 });
            let lx = list_x + 16.0 + ICON_PX + 14.0;
            let right = list_x + list_w - if r.opens { 42.0 } else { 18.0 };
            let label = self.kit_fit(r.label, size::ROW, right - lx);
            let label_w = self.kit_text_mid(lx, cy, size::ROW, &label, col::TEXT);
            if !r.value.is_empty() {
                let avail = right - (lx + label_w + 24.0);
                if avail > 30.0 {
                    let px = size::LABEL + 1.0;
                    let v = self.kit_fit_mid(r.value, px, avail);
                    let vw_ = self.kit_measure(&v, px);
                    self.kit_text_mid(right - vw_, cy, px, &v, if focused { col::GOLD } else { col::TEXT2 });
                }
            }
            if r.opens {
                self.kit_icon(list_x + list_w - 34.0, cy, 18.0, Icon::ChevronRight, col::TEXT3);
            }
            cells.push((list_x, cy - ROW_H * 0.5, list_w, ROW_H));
        }
        ui_cells_publish(kind, cells);

        if let Some(r) = rows.get(sel) {
            self.settings_card(card_x, TOP + 6.0, card_w, bottom - TOP - 6.0, r);
        }

        self.kit_footer_band();
        if let Some(r) = rows.get(sel) {
            self.kit_hints_right(vw - 40.0, vh - 24.0, &[(&[Btn::A], r.verb)]);
        }
        unsafe {
            glUseProgram(0);
            glBindVertexArray(0);
        }
        self.gl_state.invalidate();
    }

    /// The card beside the list: the row's icon on a disc, its name and value,
    /// what it is for, and at the foot what FlashNX is and is built on.
    fn settings_card(&mut self, x: f32, y: f32, w: f32, h: f32, r: &SettingsRow) {
        const PAD: f32 = 32.0;
        const DISC: f32 = 64.0;
        self.kit_shape(x, y, w, h, radius::PANEL, col::PANEL, 1.0, col::PANEL_LINE, 0.0);
        let (ix, iy) = (x + PAD, y + PAD);
        let inner_w = w - 2.0 * PAD;
        self.kit_rrect(ix, iy, DISC, DISC, DISC * 0.5, col::CHIP);
        self.kit_icon(ix + 16.0, iy + DISC * 0.5, 32.0, r.icon, col::GOLD);
        let tx = ix + DISC + 20.0;
        let tw = x + w - PAD - tx;
        let title = self.kit_fit(r.label, size::TITLE, tw);
        if r.value.is_empty() {
            self.kit_text_mid(tx, iy + DISC * 0.5, size::TITLE, &title, col::TEXT);
        } else {
            self.kit_text_mid(tx, iy + 19.0, size::TITLE, &title, col::TEXT);
            let v = self.kit_fit_mid(r.value, size::BODY, tw);
            self.kit_text_mid(tx, iy + 49.0, size::BODY, &v, col::GOLD);
        }

        let mut ty = iy + DISC + 36.0;
        for line in self.kit_wrap(r.help, size::BODY, inner_w, 5) {
            self.kit_text_mid(ix, ty, size::BODY, &line, col::TEXT2);
            ty += 26.0;
        }
        ty += 14.0;
        match r.extra {
            SettingsExtra::None => {}
            SettingsExtra::HomeViews(cur) => self.settings_view_sketches(ix, ty, inner_w, cur),
            SettingsExtra::Folder(path, n) => {
                const H: f32 = 48.0;
                self.kit_rrect(ix, ty, inner_w, H, radius::ROW, 0xFF_14_1F_36);
                let cy = ty + H * 0.5;
                self.kit_icon(ix + 14.0, cy, 20.0, Icon::FolderOpen, col::GOLD);
                let count = crate::loc_ui::games(n);
                let cw = self.kit_measure(&count, size::LABEL);
                let pw = inner_w - 14.0 - 20.0 - 12.0 - cw - 28.0;
                let p = self.kit_fit_mid(path, size::LABEL + 1.0, pw);
                self.kit_text_mid(ix + 14.0 + 20.0 + 12.0, cy, size::LABEL + 1.0, &p, col::TEXT);
                self.kit_text_mid(ix + inner_w - 14.0 - cw, cy, size::LABEL, &count, col::TEXT3);
            }
        }

        // At the foot: where the project lives, and what it stands on.
        let fy = y + h - 70.0;
        self.draw_overlay_rect(ix, fy, inner_w, 1.0, col::LINE);
        self.kit_icon(ix, fy + 26.0, 16.0, Icon::Code, col::TEXT3);
        self.kit_text_mid(ix + 26.0, fy + 26.0, size::LABEL, "github.com/Jonathan8520/FlashNX", col::TEXT2);
        let credits = self.kit_fit(crate::loc_ui::ui().about_credits, size::CAPTION + 1.0, inner_w);
        self.kit_text_mid(ix, fy + 50.0, size::CAPTION + 1.0, &credits, col::TEXT3);
    }

    /// The four home layouts as small drawings with their names, the current
    /// one ringed in gold.
    fn settings_view_sketches(&mut self, x: f32, y: f32, w: f32, cur: u8) {
        const GAP: f32 = 14.0;
        let bw = ((w - 3.0 * GAP) / 4.0).floor();
        let bh = (bw * 0.64).round();
        for v in 0..4u8 {
            let bx = x + v as f32 * (bw + GAP);
            let on = v == cur;
            self.kit_rrect(bx, y, bw, bh, 10.0, 0xFF_14_1F_36);
            if on {
                self.kit_ring(bx, y, bw, bh, 10.0, 2.0, 2.0, col::GOLD);
            }
            self.sketch_home_view(bx, y, bw, bh, v, if on { col::GOLD } else { col::PILL });
            let label = crate::loc_ui::view_label(v);
            let label = self.kit_fit(label, size::LABEL, bw);
            let lw = self.kit_measure(&label, size::LABEL);
            self.kit_text_mid((bx + (bw - lw) * 0.5).round(), y + bh + 20.0, size::LABEL, &label, if on { col::TEXT } else { col::TEXT3 });
        }
    }

    /// One layout, drawn in blocks: covers are rectangles, text is a bar.
    fn sketch_home_view(&mut self, x: f32, y: f32, w: f32, h: f32, view: u8, ink: u32) {
        let faint = (ink & 0x00FF_FFFF) | 0x6000_0000;
        let p = 10.0;
        let (ix, iy, iw, ih) = (x + p, y + p, w - 2.0 * p, h - 2.0 * p);
        match view {
            // Grille: covers in rows.
            0 => {
                let (cols, rows, g) = (4usize, 2usize, 4.0f32);
                let tw = (iw - g * (cols - 1) as f32) / cols as f32;
                let th = (ih - g * (rows - 1) as f32) / rows as f32;
                for r in 0..rows {
                    for c in 0..cols {
                        let tint = if r == 0 && c == 1 { ink } else { faint };
                        self.kit_rrect(ix + c as f32 * (tw + g), iy + r as f32 * (th + g), tw, th, 2.5, tint);
                    }
                }
            }
            // Liste: titles down the left, the chosen cover large on the right.
            1 => {
                let n = 5usize;
                let g = 3.0;
                let lh = (ih - g * (n - 1) as f32) / n as f32;
                let lw = iw * 0.5;
                for i in 0..n {
                    let tint = if i == 1 { ink } else { faint };
                    self.kit_rrect(ix, iy + i as f32 * (lh + g), lw, lh, lh * 0.5, tint);
                }
                self.kit_rrect(ix + lw + 6.0, iy, iw - lw - 6.0, ih, 3.0, ink);
            }
            // Bande: one large cover, a ribbon of small ones under it.
            2 => {
                let hero_h = ih * 0.62;
                self.kit_rrect(ix + iw * 0.18, iy, iw * 0.64, hero_h, 3.0, ink);
                let n = 5usize;
                let g = 3.0;
                let cw = (iw - g * (n - 1) as f32) / n as f32;
                let ch = ih - hero_h - 5.0;
                for i in 0..n {
                    let tint = if i == 2 { ink } else { faint };
                    self.kit_rrect(ix + i as f32 * (cw + g), iy + hero_h + 5.0, cw, ch, 2.0, tint);
                }
            }
            // Étagère: one row of covers, the chosen one grown, text under it.
            _ => {
                let row_h = ih * 0.66;
                let small_h = row_h * 0.74;
                let g = 4.0;
                let cw_small = (iw - 2.0 * g) / 4.2;
                let cw_big = iw - 2.0 * g - 2.0 * cw_small;
                let mut cx = ix;
                for i in 0..3 {
                    let (cw, ch, tint) = if i == 1 { (cw_big, row_h, ink) } else { (cw_small, small_h, faint) };
                    self.kit_rrect(cx, iy + (row_h - ch), cw, ch, 2.5, tint);
                    cx += cw + g;
                }
                let ty = iy + row_h + 5.0;
                let bar = (ih - row_h - 5.0 - 3.0) * 0.5;
                self.kit_rrect(ix + cw_small + g, ty, cw_big, bar, bar * 0.5, faint);
                self.kit_rrect(ix + cw_small + g, ty + bar + 3.0, cw_big * 0.6, bar, bar * 0.5, faint);
            }
        }
    }
}

/// Glide keys of the two pickers below, apart from every list's.
const GLIDE_KEY_HOME_VIEW: u32 = 57;

impl SwitchRenderBackend {
    /// RÉGLAGES > home layout: the four layouts drawn side by side, ringed
    /// where the cursor is, the one in use ticked, and the cursor's described
    /// under them. Choosing a layout from its picture beats choosing it from
    /// its name: "Bande" and "Étagère" say nothing until you have seen them.
    pub fn draw_home_view_picker(&mut self, selection: usize) {
        const CARD_W: f32 = 150.0;
        const CARD_H: f32 = 100.0;
        const GAP: f32 = 18.0;
        const LABEL_H: f32 = 40.0;
        const DESC_H: f32 = 62.0;
        let u = crate::loc_ui::ui();
        let current = crate::loc::home_view() as usize;
        let grid_w = 4.0 * CARD_W + 3.0 * GAP;
        let h = MODAL_PAD_TOP_TIGHT + CARD_H + LABEL_H + DESC_H + MODAL_PAD_BOTTOM;
        let frame = self.draw_modal_frame(MODAL_W_WIDE, 0, Some(h), false, u.row_home_view, None, None);
        let left = (frame.x + (frame.w - grid_w) * 0.5).round();
        let top = frame.rows_top();
        let sel = selection.min(3);
        let now = unsafe { ruffle_tick_now() };
        let hx = eased_list_x(left + sel as f32 * (CARD_W + GAP), GLIDE_KEY_HOME_VIEW, now);
        let mut cells = std::vec::Vec::with_capacity(4);
        for v in 0..4usize {
            let bx = left + v as f32 * (CARD_W + GAP);
            let on = v == sel;
            self.kit_rrect(bx, top, CARD_W, CARD_H, radius::TILE, 0xFF_14_1F_36);
            self.sketch_home_view(bx + 4.0, top + 4.0, CARD_W - 8.0, CARD_H - 8.0, v as u8, if on { col::GOLD } else { col::PILL });
            let label = crate::loc_ui::view_label(v as u8);
            let lw = self.kit_measure(label, size::BODY);
            let tick = if v == current { 22.0 } else { 0.0 };
            let lx = (bx + (CARD_W - lw - tick) * 0.5).round();
            let ly = top + CARD_H + 22.0;
            self.kit_text_mid(lx, ly, size::BODY, label, if on { col::TEXT } else { col::TEXT2 });
            if v == current {
                self.kit_icon(lx + lw + 6.0, ly, 16.0, Icon::CircleCheck, col::OK);
            }
            cells.push((bx - GAP * 0.5, top - 8.0, CARD_W + GAP, CARD_H + LABEL_H));
        }
        // The ring glides; drawn after the cards so it sits on them.
        self.kit_ring(hx, top, CARD_W, CARD_H, radius::TILE, ring::GAP, ring::WIDTH, col::GOLD);
        ui_cells_publish(ui_screen_kind(), cells);

        let desc = match sel {
            1 => u.view_desc_list,
            2 => u.view_desc_strip,
            3 => u.view_desc_shelf,
            _ => u.view_desc_grid,
        };
        let mut dy = top + CARD_H + LABEL_H + 22.0;
        for line in self.kit_wrap(desc, size::BODY, grid_w, 2) {
            let w = self.kit_measure(&line, size::BODY);
            self.kit_text_mid((frame.x + (frame.w - w) * 0.5).round(), dy, size::BODY, &line, col::TEXT2);
            dy += 24.0;
        }
        self.modal_hints(&frame, &[(&[Btn::A], u.apply), (&[Btn::B], u.cancel)], false);
        unsafe {
            glUseProgram(0);
            glBindVertexArray(0);
        }
        self.gl_state.invalidate();
    }

    /// RÉGLAGES > language: the nine languages as flags in a three-column grid,
    /// each named in its own language, the one in use ticked.
    pub fn draw_library_language_picker(&mut self, selection: usize, languages: &[&str]) {
        const COLS: usize = 3;
        const CELL_W: f32 = 200.0;
        const CELL_H: f32 = 100.0;
        const FLAG_W: f32 = 78.0;
        const FLAG_H: f32 = 52.0;
        let lc = crate::loc::s();
        let u = crate::loc_ui::ui();
        let n = languages.len();
        let rows_n = n.div_ceil(COLS);
        let grid_w = COLS as f32 * CELL_W;
        let grid_h = rows_n as f32 * CELL_H;
        let h = MODAL_PAD_TOP_TIGHT + grid_h + MODAL_PAD_BOTTOM + 8.0;
        let frame = self.draw_modal_frame(MODAL_W_WIDE, 0, Some(h), false, lc.lang_title, None, None);
        let active = crate::loc::current().index();
        let left = (frame.x + (frame.w - grid_w) * 0.5).round();
        let top = frame.rows_top();
        if selection < n {
            // One pill gliding in both axes: this is a grid, so the cursor goes
            // sideways as often as down.
            let now = unsafe { ruffle_tick_now() };
            let hx = eased_list_x(left + (selection % COLS) as f32 * CELL_W, GLIDE_KEY_LANG, now);
            let hy = eased_list_y(top + (selection / COLS) as f32 * CELL_H, GLIDE_KEY_LANG, now);
            self.kit_focus_pill(hx + 6.0, hy, CELL_W - 12.0, CELL_H - 8.0);
        }
        let mut cells = std::vec::Vec::with_capacity(n);
        for (i, name) in languages.iter().enumerate() {
            let cx = left + (i % COLS) as f32 * CELL_W;
            let cy = top + (i / COLS) as f32 * CELL_H;
            let is_sel = i == selection;
            let fx = (cx + (CELL_W - FLAG_W) * 0.5).round();
            let fy = cy + 10.0;
            if let Some(&l) = crate::loc::PICKER_LANGS.get(i) {
                self.draw_flag(l, fx, fy, FLAG_W, FLAG_H);
            }
            self.flag_stroke(fx, fy, FLAG_W, FLAG_H, if is_sel { col::GOLD & 0xFF_FFFF } else { 0x0E_17_2A });
            let px = size::LABEL + 1.0;
            let name = self.kit_fit(name, px, CELL_W - 40.0);
            let nw = self.kit_measure(&name, px);
            let tick = if i == active { 20.0 } else { 0.0 };
            let nx = (cx + (CELL_W - nw - tick) * 0.5).round();
            let ny = fy + FLAG_H + 18.0;
            self.kit_text_mid(nx, ny, px, &name, if is_sel { col::TEXT } else { col::TEXT2 });
            if i == active {
                self.kit_icon(nx + nw + 5.0, ny, 15.0, Icon::CircleCheck, col::OK);
            }
            cells.push((cx, cy, CELL_W, CELL_H));
        }
        ui_cells_publish(ui_screen_kind(), cells);
        self.modal_hints(&frame, &[(&[Btn::A], u.apply), (&[Btn::B], u.cancel)], false);
        unsafe {
            glUseProgram(0);
            glBindVertexArray(0);
        }
        self.gl_state.invalidate();
    }
}

/// Glide key of the bug report's game list.
const GLIDE_KEY_BUG_PAGE: u32 = 58;

impl SwitchRenderBackend {
    /// RÉGLAGES > report a bug: which game? Last played first, each with the
    /// time played, which is usually enough to find the one that just broke.
    pub fn draw_library_bug_picker(
        &mut self,
        selection: usize,
        scroll_offset: usize,
        games: &[(&str, std::string::String)],
        visible_rows: usize,
    ) {
        let u = crate::loc_ui::ui();
        self.library_clear();
        let vw = self.dimensions.width as f32;
        let vh = self.dimensions.height as f32;
        let top = self.kit_page_header(&[u.tab_settings], u.row_bug, Some(u.bug_pick_sub)) + 14.0;
        let spec = ScrollRows {
            x: 40.0,
            w: vw - 80.0 - 24.0,
            top,
            row_h: 50.0,
            pill_h: 44.0,
            pinned: 0,
            visible: visible_rows,
            key: GLIDE_KEY_BUG_PAGE,
            scrollbar_x: Some(vw - 52.0),
        };
        let w = spec.w;
        self.kit_scroll_rows(&spec, games.len(), selection, scroll_offset, |s, i, cy, focused| {
            let (name, played) = &games[i];
            s.kit_row_item(40.0, cy, w, &Row::new(Icon::Gamepad, name).value(played), focused);
        });
        self.kit_footer_band();
        self.kit_hints_right(vw - 40.0, vh - 24.0, &[(&[Btn::A], u.choose), (&[Btn::B], u.back)]);
        unsafe {
            glUseProgram(0);
            glBindVertexArray(0);
        }
        self.gl_state.invalidate();
    }
}
