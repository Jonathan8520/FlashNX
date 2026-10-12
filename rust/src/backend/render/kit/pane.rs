//! A column of settings rows inside any rectangle: section titles over the
//! groups, the focus pill gliding between rows, a scroll that keeps the cursor
//! in view, the thin scrollbar when the rows do not fit, and the box under them
//! that explains the row with the cursor.
//!
//! These are the rows of a side panel (`sheet.rs`), freed from the panel. The
//! Réglages pane is a rectangle in a page, beside the column of sections, and
//! its rows had to look and move exactly like the pause's and the game
//! options': same row, same pill, same help box, same scroll. Everything here
//! takes the rectangle it draws in and returns the rows' touch cells instead of
//! publishing them, so a page that has other things to tap (the sections) can
//! publish one table for all of it, and a panel can hand its rows here too.

use super::super::*;
use super::list::Row;
use super::theme::{col, size};

/// The rows of a pane, as data.
pub(crate) struct Pane<'a> {
    pub rows: &'a [Row<'a>],
    /// Section titles: (index of the row they sit over, text).
    pub sections: &'a [(usize, &'a str)],
    /// The row with the cursor, or `usize::MAX` while the cursor is elsewhere
    /// on the page (the rows are then shown, but none is lit).
    pub selection: usize,
    pub row_h: f32,
    /// Glide key of the focus pill; also seeds the scroll's.
    pub glide_key: u32,
    pub help: &'a str,
    /// Keep room for the help box even when there is no help, so the rows do
    /// not move when the cursor goes from a row with help to one without.
    pub reserve_help: bool,
    /// Draw the pill and the scroll where they belong, without easing: for
    /// rows shown under a window that has a cursor of its own (the glide slots
    /// are shared, and two lists easing at once make both snap every frame).
    pub still: bool,
}

/// The room a section title takes over its rows (Options.dc.html: 36 px).
const SECTION_H: f32 = 36.0;
/// Rows are this far apart beyond their own height.
const ROW_GAP: f32 = 2.0;
/// The help box always gets room for three lines, so the rows' area keeps its
/// size (and the list does not jump) when the help under it grows.
const HELP_LINES: usize = 3;
/// Width of the scrollbar's track, and the room the rows leave it.
const BAR_W: f32 = 4.0;
const BAR_ROOM: f32 = 14.0;

/// Height the help box of a pane takes at the bottom of its rectangle, the gap
/// above it included.
pub(crate) fn help_room() -> f32 {
    let lh = (size::LABEL * 1.45).round();
    28.0 + lh * HELP_LINES as f32 + 12.0
}

impl SwitchRenderBackend {
    /// Draw `p` in the rectangle from `top` to `bottom`, `w` wide from `x`.
    /// Returns one touch cell per row: the row's rectangle, or an empty one
    /// for a row scrolled out of the visible band (half a row showing at an
    /// edge must not take a tap aimed at what is next to it).
    pub(crate) fn kit_pane_rows(
        &mut self,
        p: &Pane,
        x: f32,
        top: f32,
        w: f32,
        bottom: f32,
    ) -> std::vec::Vec<(f32, f32, f32, f32)> {
        let help_room = if p.reserve_help { help_room() } else { 0.0 };
        if !p.help.trim().is_empty() {
            self.kit_help_box(x, bottom, w, p.help, HELP_LINES);
        }
        let area_top = top;
        let area_bot = bottom - help_room;
        let area_h = (area_bot - area_top).max(p.row_h);

        // Where each row sits from the top of the area, a section title
        // opening a gap before the first row of its group.
        let mut ys = std::vec::Vec::with_capacity(p.rows.len());
        let mut y = 0.0;
        for i in 0..p.rows.len() {
            if p.sections.iter().any(|(at, _)| *at == i) {
                y += SECTION_H;
            }
            ys.push(y);
            y += p.row_h + ROW_GAP;
        }
        let content_h = (y - ROW_GAP).max(0.0);
        let focused = p.selection < p.rows.len();
        let max_off = (content_h - area_h).max(0.0);
        // The cursor's row kept in the middle of the band where it can be, the
        // way the panels scroll; back at the top while the cursor is elsewhere.
        let target = if focused && max_off > 0.0 {
            (ys[p.selection] + p.row_h * 0.5 - area_h * 0.5).clamp(0.0, max_off)
        } else {
            0.0
        };
        let now = unsafe { ruffle_tick_now() };
        let scroll = if max_off <= 0.0 {
            0.0
        } else if p.still {
            target
        } else {
            eased_scroll_px(target, p.glide_key ^ 0x5000_0000, now)
        };
        let scrolls = max_off > 0.0;
        let rw = w - if scrolls { BAR_ROOM } else { 0.0 };
        if scrolls {
            // The scissor is in screen pixels and ignores the slide a tab
            // switch puts on the page, so it follows that slide by hand: a
            // fixed clip would shave the rows on one side while they move.
            let tx = self.ui_translate_x;
            self.set_clip(x - 4.0 + tx, area_top, w + 8.0, area_bot - area_top);
        }
        for (at, text) in p.sections {
            if let Some(ry) = ys.get(*at) {
                self.kit_section(x + 12.0, area_top + ry - scroll - 14.0, text);
            }
        }
        if focused {
            let at = area_top + ys[p.selection] - scroll;
            let hy = if p.still { at } else { eased_list_y(at, p.glide_key, now) };
            self.kit_focus_pill(x, hy, rw, p.row_h);
        }
        let mut cells: std::vec::Vec<(f32, f32, f32, f32)> = std::vec![(0.0, 0.0, 0.0, 0.0); p.rows.len()];
        for (i, row) in p.rows.iter().enumerate() {
            let ry = area_top + ys[i] - scroll;
            if ry + p.row_h < area_top - 1.0 || ry > area_bot + 1.0 {
                continue;
            }
            if ry >= area_top - 1.0 && ry + p.row_h <= area_bot + 1.0 {
                cells[i] = (x, ry, rw, p.row_h);
            }
            self.kit_row_item(x, ry + p.row_h * 0.5, rw, row, i == p.selection);
        }
        if scrolls {
            self.clear_clip();
            // Where the visible part sits in the whole list.
            let track_h = area_bot - area_top - 8.0;
            let thumb_h = (track_h * area_h / content_h.max(1.0)).max(28.0).min(track_h);
            let ty = area_top + 4.0 + (track_h - thumb_h) * (scroll / max_off).clamp(0.0, 1.0);
            let bx = x + w - BAR_W;
            self.kit_rrect(bx, area_top + 4.0, BAR_W, track_h, BAR_W * 0.5, col::LINE);
            self.kit_rrect(bx, ty, BAR_W, thumb_h, BAR_W * 0.5, 0xFF_4A_5C_85);
        }
        cells
    }
}
