//! The launcher's windows: a list of settings, a list of places or things that
//! scrolls, and a question with its answers.
//!
//! The old `draw_library_options` titled every one of them "Options" and put
//! the real name in the subtitle, so the game defaults opened as "Options /
//! Réglages par défaut". A window now says what it is.

use super::super::kit::icons::Icon;
use super::super::*;

impl SwitchRenderBackend {
    /// A centred list window. Bit `i` of `steppers` marks row `i` as one whose
    /// value also steps with Left and Right.
    #[allow(clippy::too_many_arguments)]
    pub fn draw_list_window(
        &mut self,
        title: &str,
        subtitle: Option<&str>,
        selection: usize,
        rows: &[&str],
        steppers: u32,
        hints: &[HintItem],
    ) {
        let frame = self.draw_modal_frame(MODAL_W, rows.len(), None, false, title, subtitle, None);
        self.draw_modal_rows_stepping(&frame, selection, rows, steppers);
        self.modal_hints(&frame, hints, false);
        unsafe {
            glUseProgram(0);
            glBindVertexArray(0);
        }
        self.gl_state.invalidate();
    }
}

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

/// One row of `draw_rows_window`: the kit's list row.
pub(crate) use super::super::kit::list::Row as WinRow;

impl SwitchRenderBackend {
    /// A window holding a list that may scroll, its first `pinned` rows held
    /// above the rest with a hairline under them (actions on the whole list,
    /// like "use this folder"). `visible` rows fit, pinned ones included.
    /// `key` is the list's glide key: change it when the rows change meaning.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn draw_rows_window(
        &mut self,
        title: &str,
        subtitle: Option<&str>,
        rows: &[WinRow],
        selection: usize,
        scroll: usize,
        pinned: usize,
        visible: usize,
        key: u32,
        hints: &[HintItem],
    ) {
        use super::super::kit::list::ScrollRows;
        let shown = visible.min(rows.len()).max(1);
        let frame = self.draw_modal_frame(MODAL_W_WIDE, shown, None, false, title, subtitle, None);
        let scrolls = rows.len() > visible;
        let spec = ScrollRows {
            x: frame.x + 16.0,
            w: frame.w - 32.0 - if scrolls { 16.0 } else { 0.0 },
            top: frame.rows_top() + MODAL_ROW_CY - MODAL_ROW_H * 0.5,
            row_h: MODAL_ROW_H,
            pill_h: MODAL_PILL_H,
            pinned,
            visible,
            key,
            scrollbar_x: if scrolls { Some(frame.x + frame.w - 26.0) } else { None },
        };
        let (x, w) = (spec.x, spec.w);
        self.kit_scroll_rows(&spec, rows.len(), selection, scroll, |s, i, cy, focused| {
            let r = &rows[i];
            s.kit_row_item(x, cy, w, r, focused);
        });
        if pinned > 0 && rows.len() > pinned {
            let ly = (spec.top + pinned as f32 * MODAL_ROW_H).round();
            self.draw_overlay_rect(x + 12.0, ly, w - 24.0, 1.0, kit::theme::col::LINE);
        }
        self.modal_hints(&frame, hints, false);
        unsafe {
            glUseProgram(0);
            glBindVertexArray(0);
        }
        self.gl_state.invalidate();
    }
}

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

impl SwitchRenderBackend {
    /// The sort picker: the orders as rows with an icon, the one in force
    /// ticked, and under the title the direction as a two-part switch that X
    /// flips. Nothing applies before A: B leaves the list as it was.
    pub(crate) fn draw_sort_window(
        &mut self,
        title: &str,
        options: &[(Icon, &str)],
        selection: usize,
        active: Option<usize>,
        reverse: bool,
    ) {
        use super::super::kit::list::ScrollRows;
        use kit::theme::{col, size};
        let lc = crate::loc::s();
        let u = crate::loc_ui::ui();
        let frame = self.draw_modal_frame(MODAL_W, options.len(), None, false, title, Some(""), None);
        // The direction, where the subtitle would be.
        let sx = frame.x + MODAL_ROW_X;
        let cy = frame.y + 82.0;
        let gw = self.kit_button(sx, cy, size::GLYPH, Btn::X, col::GLYPH);
        self.kit_segmented(sx + gw + 10.0, cy, &[lc.sort_dir_asc, lc.sort_dir_desc], reverse as usize, size::LABEL);
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
        self.kit_scroll_rows(&spec, options.len(), selection, 0, |s, i, cy, focused| {
            let (icon, label) = options[i];
            let row = WinRow::new(icon, label).mark_if(active == Some(i), Icon::Check, col::OK);
            s.kit_row_item(x, cy, w, &row, focused);
        });
        self.modal_hints(
            &frame,
            &[(&[Btn::A], u.apply), (&[Btn::X], u.order), (&[Btn::B], u.cancel)],
            false,
        );
        unsafe {
            glUseProgram(0);
            glBindVertexArray(0);
        }
        self.gl_state.invalidate();
    }
}
