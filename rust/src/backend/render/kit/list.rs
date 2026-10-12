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
    /// focus pill is the caller's, so it can glide between rows. Returns the
    /// parts of its control a finger can tap, as (part, rect) (`CtrlSegs`
    /// without the row, which only the caller knows).
    pub(crate) fn kit_row_item(
        &mut self,
        x: f32,
        cy: f32,
        w: f32,
        row: &Row,
        focused: bool,
    ) -> std::vec::Vec<(usize, (f32, f32, f32, f32))> {
        use super::theme::size;
        const PAD: f32 = 18.0;
        const ICON: f32 = 20.0;
        const MARK: f32 = 16.0;
        const CHEVRON: f32 = 18.0;
        const KEYBOARD: f32 = 18.0;
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
                // A little larger than the icons: a font glyph fills less of its
                // em than a Lucide icon does, and at the same size it looked a
                // size smaller than the outlines on the rows around it.
                Some(fill) if ic == super::icons::Icon::Star => {
                    const STAR: f32 = 25.0;
                    let sw = self.kit_measure("\u{2605}", STAR);
                    self.kit_text_mid((lx + (ICON - sw) * 0.5).round(), cy, STAR, "\u{2605}", fill);
                }
                _ => {
                    self.kit_icon(lx, cy, ICON, ic, ink);
                }
            }
            lx += ICON + 14.0;
        }
        let mut right = x + w - PAD;
        if row.opens {
            let ink = if focused { col::GOLD } else { col::CHEVRON };
            self.kit_icon(right - CHEVRON + 2.0, cy, CHEVRON, super::icons::Icon::ChevronRight, ink);
            right -= CHEVRON + 8.0;
        } else if row.keyboard {
            let ink = if focused { col::GOLD } else { 0xFF_5A_6A_8F };
            self.kit_icon(right - KEYBOARD, cy, KEYBOARD, super::icons::Icon::Keyboard, ink);
            right -= KEYBOARD + 10.0;
        }
        let mut parts = std::vec::Vec::new();
        let mut avail = right - lx;
        // The mark after the label is part of the label's room, counted before
        // the control is sized: taken from the label afterwards, a ticked sort
        // row cut "Última partida" when its choice could have tightened. A row
        // that can carry one (`mark_if`) counts it ticked or not, so the rows
        // of a list size their controls alike: counted on the ticked row
        // alone, a sort row stepped down a size, or folded to one value,
        // the moment it became the sort in force.
        let mark_w = if row.mark.is_some() { MARK + 10.0 } else { 0.0 };
        let mark_room = if row.mark_room { MARK + 10.0 } else { mark_w };
        if row.ctrl != super::controls::Ctrl::None {
            // The control takes what the label leaves it: the label is short
            // ("Affichage", "Rotation") and a choice is only useful with every
            // option on show. A share of the row (60% once) cut "Intégral |
            // Étirer | Remplir" back to one value in a 440 px
            // panel, the very cycling the choice replaced. Only a choice that
            // still does not fit falls back (`kit_ctrl`).
            let label_w = self.kit_measure(row.label, size::ROW).min(avail * 0.5);
            let max_w = (avail - label_w - mark_room - 20.0).max(60.0);
            let cw = self.kit_ctrl(right, cy, &row.ctrl, focused && !row.dim, max_w);
            parts = self.kit_ctrl_parts(right, cy, cw, row);
            avail -= cw + 20.0;
        }
        avail -= mark_w;
        let ink = if row.danger {
            col::DANGER
        } else if row.dim {
            col::TEXT3
        } else if focused {
            col::TEXT
        } else {
            col::TEXT2
        };
        // A note ("par défaut") is a caption under the label, so it never
        // takes the control's width: set aside beside the control, it pushed
        // "Intégral | Étirer | Remplir" back to one value on
        // every game that follows the default, until a press gave the game
        // its own value and the row redrew as three segments.
        let label_cy = if row.note.is_some() { cy - 8.0 } else { cy };
        let l = self.kit_fit_mid(row.label, size::ROW, avail);
        let lw = self.kit_text_mid(lx, label_cy, size::ROW, &l, ink);
        if let Some((ic, mink)) = row.mark {
            self.kit_icon(lx + lw + 10.0, label_cy, MARK, ic, mink);
        }
        if let Some(n) = row.note {
            let n = self.kit_fit(n, size::CAPTION, (avail + mark_w).max(0.0));
            self.kit_text_mid(lx, cy + 11.0, size::CAPTION, &n, col::TEXT3);
        }
        parts
    }
}

/// What a list row shows: an icon, the label, a small mark after it ("in
/// use", "on this shelf"), and on the right its control (a value, a choice, a
/// switch: `Ctrl`), then a chevron when the row opens another screen. `dim`
/// greys a row out without hiding it; `danger` reddens the one that loses
/// something.
///
/// The chevron is the promise that A goes somewhere: a row without one acts
/// where it is (it toggles, it restarts, it quits). Jonathan read "Redémarrer"
/// and "Écran" as the same kind of row until the second one had it. A keyboard
/// in the same place says the other way of leaving the page: A brings up the
/// Switch keyboard (a nickname, a name, an idea to send).
#[derive(Clone, Copy)]
pub(crate) struct Row<'a> {
    pub icon: Option<super::icons::Icon>,
    /// The icon drawn FILLED, in this colour: a favourite's star is a solid
    /// gold star, not the outline every other row has.
    pub filled: Option<u32>,
    pub label: &'a str,
    pub ctrl: super::controls::Ctrl<'a>,
    pub mark: Option<(super::icons::Icon, u32)>,
    /// The mark's room is kept even while the row has none: a row of a list
    /// that marks one of its rows (`mark_if`), whose control must not change
    /// size when the mark moves to it.
    pub mark_room: bool,
    /// A small caption under the label ("par défaut").
    pub note: Option<&'a str>,
    pub dim: bool,
    pub danger: bool,
    pub opens: bool,
    pub keyboard: bool,
    /// Armed by A (input rule 1): Left / Right change the value until A keeps
    /// it or B puts it back. The control gets a gold outline and a gold arrow
    /// on each side, so the row says the arrows act on it before they do.
    pub armed: bool,
    /// The value sits at the low / high end of its range: the armed arrow on
    /// that side is dimmed. A choice knows it from its lit option; a cycle (a
    /// number) is told.
    pub ends: (bool, bool),
    /// The lit option of a choice carries the gold focus outline: the sort
    /// windows, where Left / Right move the focus between a row's orders.
    pub seg_focus: bool,
}

impl<'a> Row<'a> {
    pub(crate) fn new(icon: super::icons::Icon, label: &'a str) -> Row<'a> {
        Row {
            icon: Some(icon),
            filled: None,
            label,
            ctrl: super::controls::Ctrl::None,
            mark: None,
            mark_room: false,
            note: None,
            dim: false,
            danger: false,
            opens: false,
            keyboard: false,
            armed: false,
            ends: (false, false),
            seg_focus: false,
        }
    }

    /// A row with no icon (a list of names).
    pub(crate) fn plain(label: &'a str) -> Row<'a> {
        Row { icon: None, ..Row::new(super::icons::Icon::File, label) }
    }

    /// Draw the icon solid in `ink` when `on` (only the star has a solid form:
    /// the console font's black star).
    pub(crate) fn filled_if(mut self, on: bool, ink: u32) -> Row<'a> {
        if on {
            self.filled = Some(ink);
        }
        self
    }

    /// A value to read on the right.
    pub(crate) fn value(mut self, value: &'a str) -> Row<'a> {
        if !value.is_empty() {
            self.ctrl = super::controls::Ctrl::Value(value);
        }
        self
    }

    /// A value Left / Right step once A has armed the row (shown alone at
    /// rest, between gold arrows while armed).
    pub(crate) fn cycle(mut self, value: &'a str) -> Row<'a> {
        self.ctrl = super::controls::Ctrl::Cycle(value);
        self
    }

    /// One of a few options, all shown, `active` lit.
    pub(crate) fn choice(mut self, opts: &'a [&'a str], active: usize) -> Row<'a> {
        self.ctrl = super::controls::Ctrl::Choice(opts, active);
        self
    }

    /// An on / off switch.
    pub(crate) fn toggle(mut self, on: bool) -> Row<'a> {
        self.ctrl = super::controls::Ctrl::Toggle(on);
        self
    }

    /// A small caption under the label, when there is one.
    pub(crate) fn note_opt(mut self, note: Option<&'a str>) -> Row<'a> {
        self.note = note;
        self
    }

    pub(crate) fn mark(mut self, icon: super::icons::Icon, ink: u32) -> Row<'a> {
        self.mark = Some((icon, ink));
        self
    }

    /// The mark when `on`, and its room either way (see `mark_room`).
    pub(crate) fn mark_if(self, on: bool, icon: super::icons::Icon, ink: u32) -> Row<'a> {
        let mut row = if on { self.mark(icon, ink) } else { self };
        row.mark_room = true;
        row
    }

    pub(crate) fn opens(mut self) -> Row<'a> {
        self.opens = true;
        self
    }

    pub(crate) fn opens_if(mut self, on: bool) -> Row<'a> {
        self.opens = on;
        self
    }

    /// A opens the Switch keyboard: the row gets a keyboard where a chevron
    /// would go.
    pub(crate) fn keyboard(mut self) -> Row<'a> {
        self.keyboard = true;
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

    /// Armed (see `armed`) when `on`.
    pub(crate) fn armed_if(mut self, on: bool) -> Row<'a> {
        self.armed = on;
        self
    }

    /// A cycle's value at the low and / or high end of its range.
    pub(crate) fn ends(mut self, low: bool, high: bool) -> Row<'a> {
        self.ends = (low, high);
        self
    }

    /// The choice's lit option carries the focus outline when `on`.
    pub(crate) fn seg_focus_if(mut self, on: bool) -> Row<'a> {
        self.seg_focus = on;
        self
    }
}

/// What a tap on a row's control means, beside the plain options of a choice
/// (their index): the armed arrows step the value.
pub(crate) const SEG_PREV: usize = usize::MAX - 1;
pub(crate) const SEG_NEXT: usize = usize::MAX;

/// The tappable parts of the controls a list drew: (row, part, rect), a part
/// being an option of a choice (its index), the knob of a switch (0), or an
/// armed arrow (`SEG_PREV` / `SEG_NEXT`). Published beside the rows' own
/// table (`ui_segs_publish`), so a tap on an option sets it directly instead
/// of being taken as a tap on the row.
pub(crate) type CtrlSegs = std::vec::Vec<(usize, usize, (f32, f32, f32, f32))>;
