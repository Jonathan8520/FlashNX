//! The IMPORTER tab: two actions pinned on top (search Flashpoint, add an
//! address), then the sources you saved.
//!
//! The rows used to be told apart by colour and by tags in capitals ("SWF",
//! "LIST", "ARCHIVE.ORG"), with a "+" glued to the add row and a diamond for a
//! favourite because the font had no star. Each row now starts with an icon that
//! says what pressing A on it does: a file downloads, an archive opens its list.

use super::super::kit::icons::Icon;
use super::super::kit::theme::{col, size};
use super::super::*;

impl SwitchRenderBackend {
    /// `selection` and `scroll_offset` are row indices, the pinned rows
    /// included. `interactive` is false when this page is only drawn under the
    /// file list that opens out of one of its rows: then it must neither drive
    /// the shared scroll animation nor publish rows for touch, which belong to
    /// the list drawn over it that frame.
    #[allow(clippy::too_many_arguments)]
    pub fn draw_library_distant_list(
        &mut self,
        selection: usize,
        labels: &[&str],
        hosts: &[&str],
        direct: &[bool],
        progress: &[(u32, Option<u32>)],
        favorite: &[bool],
        search_label: &str,
        add_label: &str,
        scroll_offset: usize,
        filter: Option<&str>,
        total_unfiltered: usize,
        interactive: bool,
    ) {
        self.library_clear();
        let vw = self.dimensions.width as f32;
        let vh = self.dimensions.height as f32;
        let lc = crate::loc::s();
        let u = crate::loc_ui::ui();
        const PINNED: usize = crate::library::IMPORTER_PINNED_ROWS;

        // The line under the tabs, as on the Play tab: how many sources, or
        // with a search "3 / 21 “mario”". With nothing saved yet the count is a
        // fact nobody needs, so the line says what the two rows are for.
        let cy = 94.0;
        let searching = filter.map_or(false, |f| !f.trim().is_empty());
        if total_unfiltered == 0 && !searching {
            let hint = self.kit_fit(lc.dist_empty_hint, size::LABEL, vw - 80.0);
            self.kit_text_mid(40.0, cy, size::LABEL, &hint, col::TEXT2);
        } else if searching {
            let count = std::format!("{} / {}", labels.len(), total_unfiltered);
            let x = 40.0 + self.kit_text_mid(40.0, cy, size::LABEL, &count, col::TEXT2);
            let term = std::format!("   \u{201C}{}\u{201D}", filter.unwrap_or("").trim());
            let term = self.kit_fit(&term, size::LABEL, vw * 0.6 - x);
            self.kit_text_mid(x, cy, size::LABEL, &term, col::TEXT3);
        } else {
            let count = lc.dist_count.replace("{}", &total_unfiltered.to_string());
            self.kit_text_mid(40.0, cy, size::LABEL, &count, col::TEXT2);
        }

        let total = labels.len() + PINNED;
        // Shared with `library`, which computes the scroll clamp and the reveal
        // box of an opening row from the same numbers.
        const VISIBLE: usize = crate::library::IMPORTER_VISIBLE_ROWS;
        let row_h = crate::library::IMPORTER_ROW_H;
        let top = crate::library::IMPORTER_ROW_TOP;
        let gap = if labels.is_empty() { 0.0 } else { crate::library::IMPORTER_SECTION_GAP };
        // Content-space top of row `i`: one pitch throughout, plus the section
        // header's band under the pinned rows. The drag-scroll and the touch
        // cells are built on the pitch, so it is the same on both sides.
        let row_y = |i: usize| top + i as f32 * row_h + if i >= PINNED { gap } else { 0.0 };
        let band_top = top - 8.0;
        let band_bot = top + VISIBLE as f32 * row_h + gap;
        let target_scroll = scroll_offset as f32 * row_h;
        let target_hover = row_y(selection);
        let mut scroll_px = target_scroll;
        let mut hover_y = target_hover;
        if interactive {
            let touch_scroll = gallery_touch_scroll_read();
            if let Ok(mut a) = gallery_anim().lock() {
                let now = unsafe { ruffle_tick_now() };
                if !a.inited {
                    a.inited = true;
                    a.last_tick = now;
                    a.last_sel = selection;
                    a.scroll_px = target_scroll;
                    a.sel_x = 0.0;
                    a.sel_y = target_hover;
                    a.sel_w = 0.0;
                    a.pop = 0.0;
                } else {
                    let freq = unsafe { ruffle_tick_freq() } as f32;
                    let dt = if freq > 0.0 {
                        (now.saturating_sub(a.last_tick) as f32 / freq).min(0.1)
                    } else {
                        1.0 / 60.0
                    };
                    a.last_tick = now;
                    a.last_sel = selection;
                    a.scroll_px = ease_to(a.scroll_px, target_scroll, dt, 16.0);
                    a.sel_y = ease_to(a.sel_y, target_hover, dt, 18.0);
                }
                // A finger on the list wins over the easing, 1:1.
                if let Some(px) = touch_scroll {
                    a.scroll_px = px;
                }
                scroll_px = a.scroll_px;
                hover_y = a.sel_y;
            }
        }

        let (x, w) = (40.0, vw - 80.0 - 20.0);
        self.set_clip(0.0, band_top, vw, band_bot - band_top);
        // The focus pill, gliding between rows.
        self.kit_focus_pill(x, hover_y - scroll_px + 3.0, w, row_h - 6.0);

        for i in 0..total {
            let y = row_y(i) - scroll_px;
            if y + row_h < band_top - 8.0 || y > band_bot + 8.0 {
                continue;
            }
            let rcy = y + row_h * 0.5;
            let focused = i == selection;
            if i < PINNED {
                // The two actions, and where each one looks.
                let (icon, label, source) = if i == 0 {
                    (Icon::Search, search_label, "Flashpoint")
                } else {
                    (Icon::Link, add_label.trim_start_matches(['+', ' ']), "archive.org")
                };
                self.kit_list_row(x, rcy, w, Some(icon), label, source, None, focused, false);
                continue;
            }
            let k = i - PINNED;
            // Right of the row: how much of the source is on the card ("4/13",
            // green once complete, gold while partial), then where it lives.
            let host = hosts.get(k).copied().unwrap_or("");
            let mut right = x + w - 18.0;
            if !host.is_empty() {
                let host = self.kit_fit(host, size::CAPTION + 1.0, 200.0);
                let hw = self.kit_measure(&host, size::CAPTION + 1.0);
                self.kit_text_mid(right - hw, rcy, size::CAPTION + 1.0, &host, col::TEXT3);
                right -= hw + 22.0;
            }
            let count = match progress.get(k).copied() {
                Some((have, Some(all))) => Some((
                    std::format!("{}/{}", have, all),
                    if all > 0 && have >= all {
                        col::OK
                    } else if have > 0 {
                        col::GOLD
                    } else {
                        col::TEXT3
                    },
                )),
                // Never opened, but files from it ARE on the card: still say so.
                Some((have, None)) if have > 0 => Some((std::format!("{}/?", have), col::GOLD)),
                _ => None,
            };
            if let Some((txt, ink)) = count {
                let cw = self.kit_measure(&txt, size::LABEL);
                self.kit_text_mid(right - cw, rcy, size::LABEL, &txt, ink);
                right -= cw + 22.0;
            }
            // A direct .swf downloads on A; an archive opens its file list.
            let is_direct = direct.get(k).copied().unwrap_or(false);
            let icon = if is_direct { Icon::File } else { Icon::Archive };
            let ink_icon = if focused { col::GOLD } else { col::TEXT2 };
            self.kit_icon(x + 18.0, rcy, 20.0, icon, ink_icon);
            let lx = x + 18.0 + 20.0 + 14.0;
            let star = favorite.get(k).copied().unwrap_or(false);
            let avail = right - lx - if star { 28.0 } else { 0.0 };
            let label = self.kit_fit_mid(labels[k], size::ROW, avail);
            let lw = self.kit_text_mid(lx, rcy, size::ROW, &label, if focused { col::TEXT } else { col::TEXT2 });
            if star {
                self.kit_icon(lx + lw + 10.0, rcy, 16.0, Icon::Star, col::GOLD);
            }
        }

        // The header of the saved half, in content space so it scrolls with
        // the rows.
        if !labels.is_empty() {
            let hy = top + PINNED as f32 * row_h + gap * 0.5 + 2.0 - scroll_px;
            if hy > band_top && hy < band_bot {
                let hw = self.kit_text_mid(x + 18.0, hy, size::CAPTION + 1.0, lc.dist_sources, col::TEXT3);
                self.draw_overlay_rect(x + 18.0 + hw + 14.0, hy.round(), w - hw - 50.0, 1.0, col::LINE);
            }
        }
        self.clear_clip();

        // A search that matched nothing would otherwise be a blank page under
        // two rows: say so.
        if labels.is_empty() && searching {
            let none = lc.cover_none;
            let nw = self.kit_measure(none, size::BODY);
            self.kit_text_mid(((vw - nw) * 0.5).round(), top + row_h * 3.0, size::BODY, none, col::TEXT3);
        }

        if interactive {
            let mut cells: std::vec::Vec<GalleryCell> = std::vec::Vec::with_capacity(total);
            for i in 0..total {
                cells.push(GalleryCell { row: i as u32, cx: vw * 0.5, x: 0.0, y: row_y(i), w: vw, h: row_h });
            }
            if let Ok(mut g) = gallery_cache().lock() {
                *g = (cells, total as u32);
            }
            if let Ok(mut v) = gallery_view().lock() {
                *v = GalleryView {
                    scroll_px,
                    pitch: row_h,
                    band_top,
                    band_bot,
                    rows_total: total as u32,
                    rows_visible: VISIBLE as u32,
                    horizontal: false,
                    off_min: 0.0,
                    off_max: 0.0,
                };
            }
        }
        self.draw_scrollbar(vw - 44.0, top, VISIBLE as f32 * row_h, scroll_px, row_h, VISIBLE, total);

        // The hints follow the row: the two actions are not opened, a file is
        // downloaded, an archive is opened, and only a saved source has options.
        self.kit_footer_band();
        let a_verb = match selection {
            0 => u.hint_search,
            1 => u.add,
            _ => match direct.get(selection - PINNED) {
                Some(true) => u.download,
                _ => u.open,
            },
        };
        let hy = vh - 24.0;
        if selection >= PINNED {
            self.kit_hints_right(
                vw - 40.0,
                hy,
                &[
                    (&[Btn::A], a_verb),
                    (&[Btn::Plus], u.hint_options),
                    (&[Btn::Y], u.hint_sort),
                    (&[Btn::Minus], u.filter_verb),
                ],
            );
        } else {
            self.kit_hints_right(
                vw - 40.0,
                hy,
                &[(&[Btn::A], a_verb), (&[Btn::Y], u.hint_sort), (&[Btn::Minus], u.filter_verb)],
            );
        }
        unsafe {
            glUseProgram(0);
            glBindVertexArray(0);
        }
        self.gl_state.invalidate();
    }
}
