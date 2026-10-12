//! A list that scrolls: the rows, the focus pill gliding between them, the
//! smooth scroll, the touch table and the scrollbar, once.
//!
//! Each list screen used to carry its own copy of this: the bug picker, the
//! folder picker, the importer, the remote files, each with its own constants
//! and its own idea of where a row's text sits. The copies drifted (one clipped
//! the pill, another did not; one published its rows for touch in absolute
//! indices, another in visible ones). Here the screen says how tall a row is
//! and how to draw one; everything that moves is shared.

use super::super::*;
use super::theme::col;

/// Where a scrolling list sits and how it behaves.
pub(crate) struct ScrollRows {
    /// Left edge and width of the rows (the focus pill fills them).
    pub x: f32,
    pub w: f32,
    /// Top of the first row.
    pub top: f32,
    pub row_h: f32,
    /// Height of the focus pill, centred in its row.
    pub pill_h: f32,
    /// Leading rows that stay put while the rest scroll (actions that act on
    /// the whole list, like the folder picker's "choose this folder").
    pub pinned: usize,
    /// Rows the list has room for, pinned ones included.
    pub visible: usize,
    /// Glide key. Change it when the rows change meaning (a new folder is a new
    /// list): a glide across that change would show the new rows streaking
    /// past under a cursor travelling to a row it was never on.
    pub key: u32,
    /// Draw a scrollbar at this x when the list does not fit.
    pub scrollbar_x: Option<f32>,
}

impl SwitchRenderBackend {
    /// Draw `total` rows, `selection` and `scroll` in absolute row indices
    /// (`scroll` counted from the first scrolling row). `draw_row(self, i, cy,
    /// focused)` draws row `i` centred on `cy`; the pill is already under it.
    ///
    /// Publishes the touch table (absolute indices, rows outside the band get
    /// an empty rect) and the drag-scroll view for the scrolling part.
    pub(crate) fn kit_scroll_rows(
        &mut self,
        spec: &ScrollRows,
        total: usize,
        selection: usize,
        scroll: usize,
        mut draw_row: impl FnMut(&mut Self, usize, f32, bool),
    ) {
        let pinned = spec.pinned.min(total);
        let n_scroll = total - pinned;
        let vis_scroll = spec.visible.saturating_sub(pinned).min(n_scroll);
        let shown = pinned + vis_scroll;
        let rh = spec.row_h;
        let now = unsafe { ruffle_tick_now() };
        let key_cursor = spec.key;
        let key_scroll = spec.key ^ 0x4000_0000;
        let max_off = n_scroll.saturating_sub(vis_scroll) as f32 * rh;
        let scroll_off = eased_scroll_px((scroll as f32 * rh).min(max_off), key_scroll, now);
        let row_top = |i: usize| -> f32 {
            if i < pinned {
                spec.top + i as f32 * rh
            } else {
                spec.top + i as f32 * rh - scroll_off
            }
        };
        let band_top = spec.top + pinned as f32 * rh;
        let band_bot = spec.top + shown as f32 * rh;
        row_view_publish(RowView {
            key: key_scroll,
            kind: ui_screen_kind(),
            band_top,
            band_bot,
            row_h: rh,
            scroll_px: scroll_off,
            max_off,
            total: n_scroll as u32,
            visible: vis_scroll as u32,
            base: pinned as u32,
        });

        let pill_dy = (rh - spec.pill_h) * 0.5;
        if selection < total {
            let hy = eased_list_y(row_top(selection), key_cursor, now);
            let clip = selection >= pinned;
            if clip {
                self.set_clip(spec.x - 4.0, band_top, spec.w + 8.0, band_bot - band_top);
            }
            self.kit_focus_pill(spec.x, hy + pill_dy, spec.w, spec.pill_h);
            if clip {
                self.clear_clip();
            }
        }

        let mut cells: std::vec::Vec<(f32, f32, f32, f32)> = std::vec![(0.0, 0.0, 0.0, 0.0); total];
        for i in 0..pinned {
            let y = row_top(i);
            cells[i] = (spec.x, y, spec.w, rh);
            draw_row(self, i, y + rh * 0.5, i == selection);
        }
        if vis_scroll > 0 {
            let first = pinned + (scroll_off / rh).floor().max(0.0) as usize;
            let last = (first + vis_scroll + 2).min(total);
            self.set_clip(spec.x - 4.0, band_top, spec.w + 8.0, band_bot - band_top);
            for i in first..last {
                let y = row_top(i);
                // What is seen is the clip's business; what is touchable is
                // this: a row caught half out of the band must not take a tap
                // aimed at the header or the row below.
                if y >= band_top - 1.0 && y + rh <= band_bot + 1.0 {
                    cells[i] = (spec.x, y, spec.w, rh);
                }
                draw_row(self, i, y + rh * 0.5, i == selection);
            }
            self.clear_clip();
        }
        ui_cells_publish(ui_screen_kind(), cells);

        if let Some(sx) = spec.scrollbar_x {
            self.draw_scrollbar(sx, band_top + 4.0, band_bot - band_top - 8.0, scroll_off, rh, vis_scroll, n_scroll);
        }
    }

    /// Draw `row` centred on `cy` between `x` and `x + w` (see `Row`). The
    /// focus pill is the caller's, so it can glide between rows.
    pub(crate) fn kit_row_item(&mut self, x: f32, cy: f32, w: f32, row: &Row, focused: bool) {
        use super::theme::size;
        const PAD: f32 = 18.0;
        const ICON: f32 = 20.0;
        const MARK: f32 = 16.0;
        const CHEVRON: f32 = 18.0;
        let mut lx = x + PAD;
        if let Some(ic) = row.icon {
            let ink = if row.danger {
                col::DANGER
            } else if focused {
                col::GOLD
            } else if row.dim {
                col::TEXT3
            } else {
                col::TEXT2
            };
            match row.filled {
                // The solid star from the console font, centred in the icon's box.
                Some(fill) if ic == super::icons::Icon::Star => {
                    let sw = self.kit_measure("\u{2605}", ICON);
                    self.kit_text_mid((lx + (ICON - sw) * 0.5).round(), cy, ICON, "\u{2605}", fill);
                }
                _ => {
                    self.kit_icon(lx, cy, ICON, ic, ink);
                }
            }
            lx += ICON + 14.0;
        }
        let mut right = x + w - PAD;
        if row.opens {
            self.kit_icon(right - CHEVRON + 2.0, cy, CHEVRON, super::icons::Icon::ChevronRight, col::TEXT3);
            right -= CHEVRON + 8.0;
        }
        let mut avail = right - lx;
        if !row.value.is_empty() {
            let px = size::LABEL + 1.0;
            let v = self.kit_fit_mid(row.value, px, (avail * 0.4).max(60.0));
            let vw = self.kit_measure(&v, px);
            let ink = if focused { col::GOLD } else { col::TEXT3 };
            self.kit_text_mid(right - vw, cy, px, &v, ink);
            avail -= vw + 20.0;
        }
        if row.mark.is_some() {
            avail -= MARK + 10.0;
        }
        let ink = if row.danger {
            col::DANGER
        } else if row.dim {
            col::TEXT3
        } else if focused {
            col::TEXT
        } else {
            col::TEXT2
        };
        let l = self.kit_fit_mid(row.label, size::ROW, avail);
        let lw = self.kit_text_mid(lx, cy, size::ROW, &l, ink);
        if let Some((ic, mink)) = row.mark {
            self.kit_icon(lx + lw + 10.0, cy, MARK, ic, mink);
        }
    }
}

/// What a list row shows: an icon, the label, a small mark after it ("in
/// use", "on this shelf"), and on the right a value, then a chevron when the
/// row opens another screen. `dim` greys a row out without hiding it;
/// `danger` reddens the one that loses something.
#[derive(Clone, Copy)]
pub(crate) struct Row<'a> {
    pub icon: Option<super::icons::Icon>,
    /// The icon drawn FILLED, in this colour: a favourite's star is a solid
    /// gold star, not the outline every other row has.
    pub filled: Option<u32>,
    pub label: &'a str,
    pub value: &'a str,
    pub mark: Option<(super::icons::Icon, u32)>,
    pub dim: bool,
    pub danger: bool,
    pub opens: bool,
}

impl<'a> Row<'a> {
    pub(crate) fn new(icon: super::icons::Icon, label: &'a str) -> Row<'a> {
        Row { icon: Some(icon), filled: None, label, value: "", mark: None, dim: false, danger: false, opens: false }
    }

    /// Draw the icon solid in `ink` when `on` (only the star has a solid form:
    /// the console font's black star).
    pub(crate) fn filled_if(mut self, on: bool, ink: u32) -> Row<'a> {
        if on {
            self.filled = Some(ink);
        }
        self
    }

    pub(crate) fn value(mut self, value: &'a str) -> Row<'a> {
        self.value = value;
        self
    }

    pub(crate) fn mark(mut self, icon: super::icons::Icon, ink: u32) -> Row<'a> {
        self.mark = Some((icon, ink));
        self
    }

    pub(crate) fn mark_if(self, on: bool, icon: super::icons::Icon, ink: u32) -> Row<'a> {
        if on { self.mark(icon, ink) } else { self }
    }

    pub(crate) fn opens(mut self) -> Row<'a> {
        self.opens = true;
        self
    }

    pub(crate) fn danger(mut self) -> Row<'a> {
        self.danger = true;
        self
    }

    pub(crate) fn dim(mut self, dim: bool) -> Row<'a> {
        self.dim = dim;
        self
    }
}
