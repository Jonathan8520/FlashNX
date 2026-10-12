//! The launcher's windows: a question with its answers, the page of work under
//! way, and the sort picker.
//!
//! The old `draw_library_options` titled every one of them "Options" and put
//! the real name in the subtitle, so the game defaults opened as "Options /
//! Réglages par défaut". A window now says what it is. (The settings that used
//! to sit in list windows here are edited in the RÉGLAGES pane now.)

use super::super::kit::icons::Icon;
use super::super::*;

/// One answer of a `draw_confirm` question.
pub(crate) struct Choice<'a> {
    pub label: &'a str,
    /// Drawn in red: the answer that loses something.
    pub destructive: bool,
}

impl SwitchRenderBackend {
    /// A question with its answers side by side along the bottom, the way the
    /// console asks: an icon, the question, what it will do, optionally the
    /// thing it acts on in a chip (a path, a game), then the buttons. `danger`
    /// reddens the panel for what cannot be undone.
    ///
    /// The old confirms were a paragraph and a footer line, "A: SUPPRIMER
    /// B: ANNULER": nothing was selected, so the button you pressed to get
    /// there was the one that deleted. A selection that starts on the safe
    /// answer is what the callers use now.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn draw_confirm(
        &mut self,
        icon: Icon,
        title: &str,
        body: &str,
        detail: Option<&str>,
        choices: &[Choice],
        selection: usize,
        danger: bool,
    ) {
        use kit::theme::{col, radius, size};
        const PAD: f32 = 36.0;
        const DISC: f32 = 52.0;
        const BTN_H: f32 = 50.0;
        const BTN_GAP: f32 = 14.0;
        unsafe {
            glEnable(GL_BLEND);
            glBlendFunc(GL_SRC_ALPHA, GL_ONE_MINUS_SRC_ALPHA);
            glDisable(GL_STENCIL_TEST);
        }
        let vw = self.dimensions.width as f32;
        let vh = self.dimensions.height as f32;
        self.fill_screen_dim(if danger { MODAL_DIM_DANGER } else { col::SCRIM });
        let w = 640.0f32.min(vw - 40.0);
        let text_x = PAD + DISC + 18.0;
        let text_w = w - text_x - PAD;
        let lines = self.kit_wrap(body, size::BODY, text_w, 5);
        let lh = 25.0;
        let head_h = (34.0 + lines.len() as f32 * lh).max(DISC);
        let detail_h = if detail.is_some() { 60.0 } else { 0.0 };
        let h = PAD + head_h + detail_h + 30.0 + BTN_H + 30.0 + 44.0;
        let x = ((vw - w) * 0.5).round();
        let y = ((vh - h) * 0.5).round();
        self.kit_panel(x, y, w, h, danger);

        let ink = if danger { col::DANGER } else { col::GOLD };
        self.kit_rrect(x + PAD, y + PAD, DISC, DISC, DISC * 0.5, if danger { 0xFF_4A_22_34 } else { col::CHIP });
        self.kit_icon(x + PAD + 13.0, y + PAD + DISC * 0.5, 26.0, icon, ink);
        let t = self.kit_fit(title, size::WINDOW_TITLE, text_w);
        self.kit_text_mid(x + text_x, y + PAD + 14.0, size::WINDOW_TITLE, &t, col::TEXT);
        let mut ly = y + PAD + 14.0 + 32.0;
        for line in &lines {
            self.kit_text_mid(x + text_x, ly, size::BODY, line, col::TEXT2);
            ly += lh;
        }
        let mut by = y + PAD + head_h + 26.0;
        if let Some(d) = detail {
            let chip_y = by - 6.0;
            self.kit_rrect(x + PAD, chip_y, w - 2.0 * PAD, 44.0, radius::ROW, if danger { 0x80_1A_0C_14 } else { 0xFF_14_1F_36 });
            let dt = self.kit_fit_mid(d, size::LABEL + 1.0, w - 2.0 * PAD - 32.0);
            self.kit_text_mid(x + PAD + 16.0, chip_y + 22.0, size::LABEL + 1.0, &dt, col::TEXT);
            by += detail_h;
        }

        // The answers.
        let n = choices.len().max(1) as f32;
        let bw = ((w - 2.0 * PAD - BTN_GAP * (n - 1.0)) / n).floor();
        let mut cells = std::vec::Vec::with_capacity(choices.len());
        for (i, c) in choices.iter().enumerate() {
            let bx = x + PAD + i as f32 * (bw + BTN_GAP);
            let on = i == selection;
            if on {
                self.kit_shape(bx, by, bw, BTN_H, radius::ROW, col::CHIP, 2.0, col::GOLD, 0.0);
            } else {
                self.kit_rrect(bx, by, bw, BTN_H, radius::ROW, if danger { 0x60_00_00_00 } else { 0xFF_22_32_4F });
            }
            let label = self.kit_fit(c.label, size::ROW, bw - 24.0);
            let lw = self.kit_measure(&label, size::ROW);
            let ink = if c.destructive {
                col::DANGER
            } else if on {
                col::TEXT
            } else {
                col::TEXT2
            };
            self.kit_text_mid((bx + (bw - lw) * 0.5).round(), by + BTN_H * 0.5, size::ROW, &label, ink);
            cells.push((bx, by, bw, BTN_H));
        }
        ui_cells_publish(ui_screen_kind(), cells);

        let u = crate::loc_ui::ui();
        let hy = y + h - 28.0;
        self.draw_overlay_rect(x + 20.0, hy - 28.0, w - 40.0, 1.0, if danger { col::DANGER_LINE } else { col::LINE });
        self.kit_hints_centered(x + w * 0.5, hy, &[(&[Btn::A], u.validate), (&[Btn::B], u.cancel)]);
        unsafe {
            glUseProgram(0);
            glBindVertexArray(0);
        }
        self.gl_state.invalidate();
    }
}

/// One row of a window's list: the kit's list row.
pub(crate) use super::super::kit::list::Row as WinRow;

impl SwitchRenderBackend {
    /// A page with one card in its middle, for work in progress: an icon, what
    /// is happening, on what, a bar and the numbers under it. `frac` None is an
    /// amount not known yet: a band sweeps the track instead of a fill claiming
    /// a share it does not know.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn draw_progress_page(
        &mut self,
        icon: Icon,
        title: &str,
        name: &str,
        frac: Option<f32>,
        detail: &str,
        hints: &[HintItem],
    ) {
        use kit::theme::{col, size};
        const PAD: f32 = 40.0;
        const DISC: f32 = 56.0;
        const BAR_H: f32 = 12.0;
        self.library_clear();
        let vw = self.dimensions.width as f32;
        let vh = self.dimensions.height as f32;
        let w = 760.0f32.min(vw - 80.0);
        let h = 250.0;
        let x = ((vw - w) * 0.5).round();
        let y = ((vh - 48.0 - h) * 0.5).round();
        self.kit_panel(x, y, w, h, false);
        self.kit_rrect(x + PAD, y + PAD, DISC, DISC, DISC * 0.5, col::CHIP);
        self.kit_icon(x + PAD + 14.0, y + PAD + DISC * 0.5, 28.0, icon, col::GOLD);
        let tx = x + PAD + DISC + 20.0;
        let tw = x + w - PAD - tx;
        let t = self.kit_fit(title, size::TITLE, tw);
        self.kit_text_mid(tx, y + PAD + 16.0, size::TITLE, &t, col::TEXT);
        let n = self.kit_fit_mid(name, size::BODY, tw);
        self.kit_text_mid(tx, y + PAD + 46.0, size::BODY, &n, col::TEXT2);

        let bx = x + PAD;
        let bw = w - 2.0 * PAD;
        let by = y + PAD + DISC + 44.0;
        self.kit_rrect(bx, by, bw, BAR_H, BAR_H * 0.5, col::LINE);
        match frac {
            Some(f) => {
                let f = f.clamp(0.0, 1.0);
                if f > 0.0 {
                    self.kit_rrect(bx, by, (bw * f).max(BAR_H), BAR_H, BAR_H * 0.5, col::GOLD);
                }
            }
            None => {
                let now = unsafe { ruffle_tick_now() };
                let freq = unsafe { ruffle_tick_freq() } as f32;
                let t = if freq > 0.0 { (now as f32 / freq) * 0.7 } else { 0.0 };
                let seg = bw * 0.25;
                let p = t - t.floor();
                let sx = bx - seg + (bw + seg) * p;
                let x0 = sx.max(bx);
                let x1 = (sx + seg).min(bx + bw);
                if x1 > x0 {
                    self.kit_rrect(x0, by, x1 - x0, BAR_H, BAR_H * 0.5, col::GOLD);
                }
            }
        }
        let d = self.kit_fit(detail, size::LABEL + 1.0, bw);
        self.kit_text_mid(bx, by + BAR_H + 26.0, size::LABEL + 1.0, &d, col::TEXT2);

        self.kit_footer_band();
        if !hints.is_empty() {
            self.kit_hints_right(vw - 40.0, vh - 24.0, hints);
        }
        unsafe {
            glUseProgram(0);
            glBindVertexArray(0);
        }
        self.gl_state.invalidate();
    }
}

/// One way to sort, as the sort window shows it: its icon, what it sorts BY,
/// and the two orders it gives, written as what the player will see, the
/// natural one first ("Derniers ajoutés", "Premiers ajoutés").
pub(crate) struct SortOption<'a> {
    pub icon: Icon,
    pub label: &'a str,
    pub orders: [&'a str; 2],
}

impl SwitchRenderBackend {
    /// The sort picker (input rule 5). Each row is a way to sort, with both
    /// its orders as words on its right and the one it would apply lit; on the
    /// focused row that one carries the gold focus outline, which Left / Right
    /// move between the two orders (X flips it). A applies the focused way and
    /// order, B leaves the list as it was. The sort in force is ticked. Bit
    /// `i` of `reversed` set = row `i` shows its second order. `subtitle` says
    /// what the order applies to, `help` what the rows cannot. A tap on an
    /// order puts the focus there, a second one applies it.
    ///
    /// It used to be noun rows ("Ajouté", "Plus joués") under one "Croissant /
    /// Décroissant" switch for all of them, which said the opposite of the list
    /// for most rows: their natural order is the descending one.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn draw_sort_window(
        &mut self,
        title: &str,
        subtitle: Option<&str>,
        options: &[SortOption],
        selection: usize,
        active: Option<usize>,
        reversed: u8,
        help: Option<&str>,
    ) {
        use super::super::kit::list::ScrollRows;
        use kit::theme::col;
        let u = crate::loc_ui::ui();
        // Wide enough for both orders of a row side by side in most languages;
        // a pair too long for its row shows the lit one alone instead.
        let width = MODAL_W_WIDE.min(self.dimensions.width as f32 - 40.0);
        let help = help.filter(|h| !h.trim().is_empty());
        let help_h = help.map_or(0.0, |t| self.kit_help_box_height(t, width - 32.0, 3) + 20.0);
        let pad_top = if subtitle.is_some() { MODAL_PAD_TOP } else { MODAL_PAD_TOP_TIGHT };
        let h = pad_top + options.len().max(1) as f32 * MODAL_ROW_H + help_h + MODAL_PAD_BOTTOM;
        let frame = self.draw_modal_frame(width, options.len(), Some(h), false, title, subtitle, None);
        let spec = ScrollRows {
            x: frame.x + 16.0,
            w: frame.w - 32.0,
            top: frame.rows_top() + MODAL_ROW_CY - MODAL_ROW_H * 0.5,
            row_h: MODAL_ROW_H,
            pill_h: MODAL_PILL_H,
            pinned: 0,
            visible: options.len(),
            key: 0x6200_0000,
            scrollbar_x: None,
        };
        let (x, w) = (spec.x, spec.w);
        let mut segs: kit::list::CtrlSegs = std::vec::Vec::new();
        self.kit_scroll_rows(&spec, options.len(), selection, 0, |s, i, cy, focused| {
            let o = &options[i];
            let lit = if i < 8 { ((reversed >> i) & 1) as usize } else { 0 };
            let row = WinRow::new(o.icon, o.label)
                .choice(&o.orders, lit)
                .mark_if(active == Some(i), Icon::Check, col::OK)
                .seg_focus_if(focused);
            let parts = s.kit_row_item(x, cy, w, &row, focused);
            segs.extend(parts.into_iter().map(|(p, r)| (i, p, r)));
        });
        // After the rows' own table, which `kit_scroll_rows` published.
        ui_segs_publish(ui_screen_kind(), segs);
        if let Some(text) = help {
            self.kit_help_box(frame.x + 16.0, frame.y + frame.h - MODAL_PAD_BOTTOM - 10.0, frame.w - 32.0, text, 3);
        }
        // Left / Right move the focus: navigation, which needs no hint.
        self.modal_hints(&frame, &[(&[Btn::A], u.apply), (&[Btn::X], u.sort_flip), (&[Btn::B], u.cancel)], false);
        unsafe {
            glUseProgram(0);
            glBindVertexArray(0);
        }
        self.gl_state.invalidate();
    }
}
