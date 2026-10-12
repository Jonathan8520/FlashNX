//! A game's cover picker.
//!
//! A game's own options are the side panel of `library::options_panel`; a saved
//! source's are the importer's side panel (`library`, `with_source_sheet`).

use super::super::kit::icons::Icon;
use super::super::kit::theme::{col, size};
use super::super::*;

impl SwitchRenderBackend {
    /// OPTIONS > cover: the candidates found online as a grid, the gold ring
    /// gliding between them, the chosen one's title under the grid.
    /// `use_shot` says whether the grid shows screenshots rather than logos,
    /// for Y's hint, which names what pressing it gives.
    #[allow(clippy::too_many_arguments)]
    pub fn draw_library_cover_picker(
        &mut self,
        game_name: &str,
        selection: usize,
        titles: &[&str],
        urls: &[&str],
        msg: &str,
        use_shot: bool,
    ) {
        use super::super::kit::theme::{radius, ring};
        const W: f32 = 980.0;
        const MARGIN: f32 = 40.0;
        const GAP: f32 = 16.0;
        const THUMB_H: f32 = 124.0;
        let lc = crate::loc::s();
        let u = crate::loc_ui::ui();
        let y_hint = if use_shot { lc.cover_show_logos } else { lc.cover_show_shots };
        let y_hint = y_hint.trim_start_matches("Y:").trim();
        let cols = crate::library::COVER_PICKER_COLS;
        let n = urls.len();
        if n == 0 {
            // Covers off, nothing found, or an error: say which, in the window.
            let frame = self.draw_modal_frame(720.0, 0, Some(280.0), false, lc.cover_title, Some(game_name), None);
            let m = if msg.is_empty() { lc.cover_none } else { msg };
            let lines = self.kit_wrap(m, size::BODY, frame.w - 120.0, 3);
            let mut ly = frame.y + 150.0;
            for line in &lines {
                let lw = self.kit_measure(line, size::BODY);
                self.kit_text_mid((frame.x + (frame.w - lw) * 0.5).round(), ly, size::BODY, line, col::TEXT2);
                ly += 26.0;
            }
            self.modal_hints(&frame, &[(&[Btn::Minus], u.hint_search), (&[Btn::B], u.back)], false);
            // Nothing to tap here. Published all the same: the last table under
            // this tag is the options panel's, and two taps on the dimmed
            // panel around the window moved the picker's hidden cursor.
            ui_cells_publish(ui_screen_kind(), std::vec::Vec::new());
            unsafe {
                glUseProgram(0);
                glBindVertexArray(0);
            }
            self.gl_state.invalidate();
            return;
        }
        let w = W.min(self.dimensions.width as f32 - 40.0);
        let cell_w = ((w - MARGIN * 2.0 - GAP * (cols as f32 - 1.0)) / cols as f32).floor();
        let rows = n.div_ceil(cols);
        let h = MODAL_PAD_TOP - 14.0 + rows as f32 * (THUMB_H + GAP) + 44.0 + 60.0;
        let frame = self.draw_modal_frame(w, 0, Some(h), false, lc.cover_title, Some(game_name), None);
        let grid_top = frame.y + MODAL_PAD_TOP - 14.0;
        let grid_left = frame.x + MARGIN;
        self.pump_thumbnail_load();
        let now = unsafe { ruffle_tick_now() };
        let (fx, fy) = if selection < n {
            (
                eased_list_x(grid_left + (selection % cols) as f32 * (cell_w + GAP), GLIDE_KEY_COVER, now),
                eased_list_y(grid_top + (selection / cols) as f32 * (THUMB_H + GAP), GLIDE_KEY_COVER, now),
            )
        } else {
            (0.0, 0.0)
        };
        let mut cells = std::vec::Vec::with_capacity(n);
        for i in 0..n {
            let cx = grid_left + (i % cols) as f32 * (cell_w + GAP);
            let cy = grid_top + (i / cols) as f32 * (THUMB_H + GAP);
            cells.push((cx, cy, cell_w, THUMB_H));
            match self.thumb_for(urls[i]) {
                Some(ThumbTex::Image { tex, w, h, amb }) => self.kit_cover(cx, cy, cell_w, THUMB_H, tex, w, h, amb, 1.0),
                other => {
                    self.kit_rrect(cx, cy, cell_w, THUMB_H, radius::TILE, col::TILE);
                    let icon = if matches!(other, Some(ThumbTex::Failed)) { Icon::Image } else { Icon::Hourglass };
                    self.kit_icon(cx + (cell_w - 28.0) * 0.5, cy + THUMB_H * 0.5, 28.0, icon, col::TEXT3);
                }
            }
        }
        ui_cells_publish(ui_screen_kind(), cells);
        if selection < n {
            self.kit_ring(fx, fy, cell_w, THUMB_H, radius::TILE, ring::GAP, ring::WIDTH, col::GOLD);
        }
        if let Some(t) = titles.get(selection) {
            let shown = self.kit_fit_mid(t, size::BODY, frame.w - 80.0);
            let tw = self.kit_measure(&shown, size::BODY);
            let ty = grid_top + rows as f32 * (THUMB_H + GAP) + 14.0;
            self.kit_text_mid((frame.x + (frame.w - tw) * 0.5).round(), ty, size::BODY, &shown, col::TEXT2);
        }
        self.modal_hints(
            &frame,
            &[
                (&[Btn::A], u.apply),
                (&[Btn::Y], y_hint),
                (&[Btn::Minus], u.hint_search),
                (&[Btn::B], u.back),
            ],
            false,
        );
        unsafe {
            glUseProgram(0);
            glBindVertexArray(0);
        }
        self.gl_state.invalidate();
    }
}
