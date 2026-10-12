//! The IMPORTER tab (Importer.dc.html) and the pages it opens: an archive's
//! games, the import in progress, and what went wrong.
//!
//! The home is two big actions side by side (search Flashpoint, add an
//! address), each saying what it does, then "Mes sources" with how many there
//! are and the order in force, then one row per saved source: a tile saying
//! what kind it is, its name over its site, how much of it is on the card as a
//! bar and a count, and a check once it is all there or a chevron while there
//! is more to get. The two actions used to be list rows with "Flashpoint" and
//! "archive.org" in the value column, which read as settings, and the sources
//! had no way to tell an archive (A opens its list) from a single file (A
//! downloads it).

use super::super::kit::icons::Icon;
use super::super::kit::list::Row;
use super::super::kit::theme::{col, radius, ring, size};
use super::super::*;

// ─── Numbers, sizes and dates, the way each language writes them ────────────

/// `t` with its one `{}` filled.
fn fill(t: &str, a: &str) -> std::string::String {
    t.replacen("{}", a, 1)
}

/// `t` with its two `{}` filled, in order.
fn fill2(t: &str, a: &str, b: &str) -> std::string::String {
    t.replacen("{}", a, 1).replacen("{}", b, 1)
}

/// A count with the language's thousands separator: "3 633", "3,633".
pub(crate) fn imp_count(n: u64) -> std::string::String {
    let digits = n.to_string();
    if digits.len() <= 3 {
        return digits;
    }
    let sep = crate::loc_ui::ui().imp_thousands;
    let mut out = std::string::String::with_capacity(digits.len() + 4);
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push_str(sep);
        }
        out.push(ch);
    }
    out
}

/// "2 sur 5", "1,2 Go sur 3,9 Go".
pub(crate) fn imp_n_of(a: &str, b: &str) -> std::string::String {
    fill2(crate::loc_ui::ui().imp_n_of, a, b)
}

/// A size as the language writes it, with one decimal under ten ("3,4 Mo",
/// "18 Mo", "1,2 Go"). Empty for 0, which is what an unknown size arrives as.
///
/// It used to be "12 MB" in every language and whole numbers only, so a small
/// download read "700 KB / 1 MB" and then jumped.
pub(crate) fn imp_size(bytes: u64) -> std::string::String {
    let u = crate::loc_ui::ui();
    if bytes == 0 {
        return std::string::String::new();
    }
    const K: u64 = 1024;
    let (tpl, unit) = if bytes >= K * K * K {
        (u.imp_size_gb, K * K * K)
    } else if bytes >= K * K {
        (u.imp_size_mb, K * K)
    } else if bytes >= K {
        (u.imp_size_kb, K)
    } else {
        return fill(u.imp_size_b, &bytes.to_string());
    };
    // Rounded to a tenth first, so 9.96 becomes "10", not "10,0".
    let tenths = (bytes as u128 * 10 + unit as u128 / 2) / unit as u128;
    let num = if tenths < 100 && tenths % 10 != 0 {
        std::format!("{}{}{}", tenths / 10, u.imp_decimal_mark, tenths % 10)
    } else {
        ((tenths + 5) / 10).to_string()
    };
    fill(tpl, &num)
}

/// A day as the language writes it: "12 oct. 2026", "Oct 12, 2026",
/// "2026年10月12日". `m` is 1..=12.
pub(crate) fn imp_date(y: i64, m: u32, d: u32) -> std::string::String {
    let u = crate::loc_ui::ui();
    let month = u.imp_months.split('|').nth(m.saturating_sub(1) as usize).unwrap_or("");
    u.imp_date
        .replace("{d}", &d.to_string())
        .replace("{m}", month)
        .replace("{y}", &y.to_string())
}

/// A release date as Flashpoint gives it ("2009-06-05", "2009-06", "2009"):
/// the whole day when it is all there, the year otherwise. Empty when it is
/// not a date at all.
pub(crate) fn imp_release(raw: &str) -> std::string::String {
    let raw = raw.trim();
    let mut parts = raw.split('-');
    let y = parts.next().and_then(|p| p.parse::<i64>().ok());
    let m = parts.next().and_then(|p| p.parse::<u32>().ok());
    let d = parts.next().and_then(|p| p.get(..2).unwrap_or(p).parse::<u32>().ok());
    match (y, m, d) {
        (Some(y), Some(m), Some(d)) if (1..=12).contains(&m) && (1..=31).contains(&d) => imp_date(y, m, d),
        (Some(y), _, _) if y > 1900 => y.to_string(),
        _ => std::string::String::new(),
    }
}

/// The year of a Flashpoint release date, or "".
pub(crate) fn imp_year(raw: &str) -> &str {
    let y = raw.trim().get(..4).unwrap_or("");
    if y.len() == 4 && y.chars().all(|c| c.is_ascii_digit()) {
        y
    } else {
        ""
    }
}

/// A search term between the language's quotes.
pub(crate) fn imp_quote(term: &str) -> std::string::String {
    fill(crate::loc_ui::ui().imp_quote, term.trim())
}

/// "42 %  ·  1,2 Go sur 3,9 Go  ·  3,1 Mo/s  ·  2 min restantes": a transfer
/// in one line. `rate` is bytes per second once it is known.
pub(crate) fn imp_transfer_line(done: u64, total: u64, rate: Option<f32>) -> std::string::String {
    let u = crate::loc_ui::ui();
    const SEP: &str = "  \u{00B7}  ";
    let mut parts: std::vec::Vec<std::string::String> = std::vec::Vec::new();
    if total > 0 {
        let pct = ((done as f64 / total as f64) * 100.0).clamp(0.0, 100.0) as u32;
        parts.push(std::format!("{} %", pct));
        parts.push(imp_n_of(&imp_size(done.max(1)), &imp_size(total)));
    } else {
        parts.push(imp_size(done.max(1)));
    }
    if let Some(r) = rate.filter(|r| *r > 1.0) {
        parts.push(fill(u.imp_per_sec, &imp_size(r as u64)));
        if total > done {
            let secs = ((total - done) as f32 / r).ceil() as u64;
            parts.push(if secs >= 60 {
                fill(u.imp_eta_min, &secs.div_ceil(60).to_string())
            } else {
                fill(u.imp_eta_sec, &secs.max(1).to_string())
            });
        }
    }
    parts.join(SEP)
}

// ─── The importer home ──────────────────────────────────────────────────────

/// One saved source as its row shows it.
pub(crate) struct SourceRow<'a> {
    pub name: &'a str,
    pub host: &'a str,
    /// A direct `.swf` link (A downloads it) rather than an archive (A opens
    /// its list of games).
    pub direct: bool,
    /// Games of it on the card, and how many it holds (`None` = never opened,
    /// so not known yet).
    pub have: u32,
    pub total: Option<u32>,
    pub favorite: bool,
}

/// The importer home as data. `selection` counts the two cards first (0 =
/// Flashpoint, 1 = add an address), then the sources; `scroll` counts source
/// rows only, since the cards never scroll away.
pub(crate) struct ImporterHome<'a> {
    pub rows: &'a [SourceRow<'a>],
    pub selection: usize,
    pub scroll: usize,
    pub filter: Option<&'a str>,
    /// Every saved source, before the search.
    pub total: usize,
    /// The order in force, as the outcome it gives ("Dernières ajoutées").
    pub sort: &'a str,
    /// False when the page is only the backdrop of something drawn over it (a
    /// window, the list that opens out of a row): it then neither drives the
    /// shared glide nor publishes rows for touch, which belong to what is on top.
    pub interactive: bool,
}

/// Glide key of the ring that moves between the two cards.
const CARD_RING_KEY: u32 = 0x7100_0000;
/// Where the vertical scrollbar of every importer page sits, in the right
/// margin past the rows (which end at the page margin, 48 px in).
pub(crate) const IMPORT_SCROLLBAR_INSET: f32 = 40.0;
/// The chevron's colour on a row that is not focused (the mockup's #5A6A8F).
const CHEVRON_IDLE: u32 = 0xFF_5A_6A_8F;
/// The tint of a focused icon tile: the gold at 14 %, as on the cards.
const TILE_FOCUS: u32 = 0x24_FF_D7_40;
const ICON_FOCUS: u32 = 0xFF_FF_E0_7A;

impl SwitchRenderBackend {
    /// Draw the importer home (see `ImporterHome`).
    pub(crate) fn draw_library_distant_list(&mut self, home: &ImporterHome) {
        use crate::library::{
            importer_item_rect, IMPORTER_CARD_TOP, IMPORTER_PINNED_ROWS as PINNED, IMPORTER_ROW_H as ROW_H,
            IMPORTER_ROW_TOP as ROW_TOP, IMPORTER_VISIBLE_ROWS as VISIBLE,
        };
        self.library_clear();
        let vw = self.dimensions.width as f32;
        let lc = crate::loc::s();
        let u = crate::loc_ui::ui();
        let n = home.rows.len();
        let sel = home.selection;
        let band_top = ROW_TOP;
        let band_bot = ROW_TOP + VISIBLE as f32 * ROW_H;
        let max_scroll = n.saturating_sub(VISIBLE);
        let target_scroll = home.scroll.min(max_scroll) as f32 * ROW_H;
        let target_pill = ROW_TOP + sel.saturating_sub(PINNED) as f32 * ROW_H;
        let now = unsafe { ruffle_tick_now() };

        // The scroll and the focus pill glide, on the gallery's shared state
        // like every other page with a moving list; a backdrop is drawn where
        // it rests.
        let (mut scroll_px, mut pill_y) = (target_scroll, target_pill);
        if home.interactive {
            let touch_scroll = gallery_touch_scroll_read();
            if let Ok(mut a) = gallery_anim().lock() {
                if !a.inited {
                    a.inited = true;
                    a.last_tick = now;
                    a.last_sel = sel;
                    a.scroll_px = target_scroll;
                    a.sel_y = target_pill;
                    a.sel_x = 0.0;
                    a.sel_w = 0.0;
                    a.pop = 0.0;
                } else {
                    let freq = unsafe { ruffle_tick_freq() } as f32;
                    let dt = if freq > 0.0 { (now.saturating_sub(a.last_tick) as f32 / freq).min(0.1) } else { 1.0 / 60.0 };
                    a.last_tick = now;
                    // Coming down from a card the pill appears on its row
                    // rather than flying in from wherever it last was.
                    if a.last_sel < PINNED && sel >= PINNED {
                        a.sel_y = target_pill;
                    }
                    a.last_sel = sel;
                    a.scroll_px = ease_to(a.scroll_px, target_scroll, dt, 16.0);
                    a.sel_y = ease_to(a.sel_y, target_pill, dt, 18.0);
                }
                // A finger on the list wins over the easing, 1:1.
                if let Some(px) = touch_scroll {
                    a.scroll_px = px;
                }
                scroll_px = a.scroll_px;
                pill_y = a.sel_y;
            }
        }

        // ── The two actions ─────────────────────────────────────────────
        let cards = [
            (Icon::Search, u.imp_fp_title, u.imp_fp_sub),
            (Icon::Link, u.imp_add_title, u.imp_add_sub),
        ];
        for (i, (icon, title, sub)) in cards.iter().enumerate() {
            let (x, y, w, h) = importer_item_rect(i, 0.0, vw);
            let focused = sel == i;
            self.kit_rrect(x, y, w, h, 16.0, if focused { col::TILE } else { col::PANEL });
            let cy = y + h * 0.5;
            const TILE: f32 = 52.0;
            self.kit_rrect(x + 20.0, cy - TILE * 0.5, TILE, TILE, 14.0, if focused { TILE_FOCUS } else { col::LINE });
            self.kit_icon(x + 20.0 + 13.0, cy, 26.0, *icon, if focused { ICON_FOCUS } else { col::TEXT2 });
            // Right edge: X on the Flashpoint card says it works from
            // anywhere on the tab; the other one goes somewhere.
            let right = x + w - 20.0;
            let tail = if i == 0 {
                let bw = self.kit_button_width(Btn::X, 26.0);
                self.kit_button(right - bw, cy, 26.0, Btn::X, col::GLYPH);
                bw
            } else {
                self.kit_icon(right - 18.0, cy, 18.0, Icon::ChevronRight, if focused { col::GOLD } else { CHEVRON_IDLE });
                18.0
            };
            let tx = x + 88.0;
            let avail = right - tail - 16.0 - tx;
            let t = self.kit_fit(title, size::TAB, avail);
            self.kit_text_mid(tx, cy - 11.0, size::TAB, &t, col::TEXT);
            let st = self.kit_fit(sub, size::CAPTION + 1.0, avail);
            self.kit_text_mid(tx, cy + 13.0, size::CAPTION + 1.0, &st, col::TEXT3);
        }
        if sel < PINNED {
            let (x, y, w, h) = importer_item_rect(sel, 0.0, vw);
            let rx = if home.interactive { eased_list_x(x, CARD_RING_KEY, now) } else { x };
            self.kit_ring(rx, y, w, h, 16.0, ring::GAP, ring::WIDTH, col::GOLD);
        }

        // ── "Mes sources", how many, and the order in force ─────────────
        let hy = band_top - 20.0;
        let searching = home.filter.map_or(false, |f| !f.trim().is_empty());
        let mut hx = IMPORTER_MARGIN_X + 12.0;
        hx += self.kit_text_mid(hx, hy, size::LABEL, lc.dist_sources, col::TEXT) + 10.0;
        let sort_w = if n > 0 {
            let pw = self.kit_measure(home.sort, size::CAPTION + 1.0);
            let right = vw - IMPORTER_MARGIN_X - 12.0;
            self.kit_text_mid(right - pw, hy, size::CAPTION + 1.0, home.sort, col::TEXT2);
            self.kit_icon(right - pw - 8.0 - 16.0, hy, 16.0, Icon::Sort, col::TEXT2);
            pw + 8.0 + 16.0 + 24.0
        } else {
            0.0
        };
        if searching {
            let count = std::format!("{} / {}", imp_count(n as u64), imp_count(home.total as u64));
            hx += self.kit_text_mid(hx, hy, size::LABEL, &count, col::TEXT2);
            let term = std::format!("  \u{00B7}  {}", imp_quote(home.filter.unwrap_or("")));
            let term = self.kit_fit(&term, size::LABEL, (vw - IMPORTER_MARGIN_X - 12.0 - sort_w - hx).max(0.0));
            self.kit_text_mid(hx, hy, size::LABEL, &term, col::TEXT3);
        } else {
            self.kit_text_mid(hx, hy, size::LABEL, &imp_count(home.total as u64), col::TEXT3);
        }

        // ── The sources ─────────────────────────────────────────────────
        self.set_clip(0.0, band_top, vw, band_bot - band_top);
        if sel >= PINNED && n > 0 {
            let (x, _, w, _) = importer_item_rect(PINNED, 0.0, vw);
            self.kit_focus_pill(x, pill_y - scroll_px + 3.0, w, ROW_H - 6.0);
        }
        let first = (scroll_px / ROW_H).floor().max(0.0) as usize;
        let last = (first + VISIBLE + 2).min(n);
        for k in first..last {
            let row = &home.rows[k];
            let (x, y, w, h) = importer_item_rect(k + PINNED, scroll_px, vw);
            if y + h < band_top || y > band_bot {
                continue;
            }
            self.imp_source_row(x, y + h * 0.5, w, row, sel == k + PINNED);
        }
        self.clear_clip();

        // ── Nothing to list ─────────────────────────────────────────────
        if n == 0 {
            let cy = (band_top + band_bot) * 0.5 - 20.0;
            if searching {
                let msg = fill(u.imp_no_source_match, &imp_quote(home.filter.unwrap_or("")));
                self.imp_empty_state(Icon::Search, &msg, cy);
            } else {
                self.imp_empty_state(Icon::Link, u.imp_empty_sources, cy);
            }
        }

        self.draw_scrollbar(vw - IMPORT_SCROLLBAR_INSET, band_top + 4.0, band_bot - band_top - 8.0, scroll_px, ROW_H, VISIBLE, n);

        // ── Touch ───────────────────────────────────────────────────────
        // The cards stay put while the rows scroll, but the hit test reads
        // every cell in the list's scrolled space, so theirs are written
        // already shifted by the scroll. A row caught half out of its band
        // takes no tap: it would steal one aimed at the header or the cards.
        if home.interactive {
            let mut cells: std::vec::Vec<GalleryCell> = std::vec::Vec::with_capacity(n + PINNED);
            for i in 0..PINNED {
                let (x, y, w, h) = importer_item_rect(i, 0.0, vw);
                cells.push(GalleryCell { row: 0, cx: x + w * 0.5, x, y: y + scroll_px, w, h });
            }
            for k in 0..n {
                let (x, y, w, h) = importer_item_rect(k + PINNED, scroll_px, vw);
                let shown = y >= band_top - 1.0 && y + h <= band_bot + 1.0;
                cells.push(if shown {
                    GalleryCell { row: k as u32 + 1, cx: x + w * 0.5, x, y: y + scroll_px, w, h }
                } else {
                    GalleryCell::default()
                });
            }
            if let Ok(mut g) = gallery_cache().lock() {
                *g = (cells, n as u32);
            }
            if let Ok(mut v) = gallery_view().lock() {
                *v = GalleryView {
                    scroll_px,
                    pitch: ROW_H,
                    band_top: IMPORTER_CARD_TOP,
                    band_bot,
                    rows_total: n as u32,
                    rows_visible: VISIBLE as u32,
                    horizontal: false,
                    off_min: 0.0,
                    off_max: 0.0,
                };
            }
            if let Some(rect) = (sel >= PINNED).then(|| importer_item_rect(sel, scroll_px, vw)) {
                import_sel_rect_set(rect);
            }
        }

        // ── Hints: what A does here, and only what can be done ──────────
        self.kit_footer_band();
        let a_verb = match sel {
            0 => u.open,
            1 => u.add,
            _ => match home.rows.get(sel - PINNED) {
                Some(r) if r.direct && r.have > 0 => u.hint_play,
                Some(r) if r.direct => u.download,
                _ => u.open,
            },
        };
        let mut hints: std::vec::Vec<HintItem> = std::vec![(&[Btn::A][..], a_verb)];
        if sel >= PINNED {
            hints.push((&[Btn::Plus][..], u.hint_options));
        }
        if n > 1 {
            hints.push((&[Btn::Y][..], u.hint_sort));
        }
        if home.total > 0 {
            hints.push((&[Btn::Minus][..], u.hint_search));
        }
        if searching {
            hints.push((&[Btn::B][..], u.imp_show_all));
        }
        self.imp_footer_hints(&hints);
        unsafe {
            glUseProgram(0);
            glBindVertexArray(0);
        }
        self.gl_state.invalidate();
    }

    /// One source row centred on `cy` between `x` and `x + w` (the focus pill
    /// is the caller's).
    fn imp_source_row(&mut self, x: f32, cy: f32, w: f32, row: &SourceRow, focused: bool) {
        const PAD: f32 = 12.0;
        const TILE: f32 = 38.0;
        const BAR_W: f32 = 120.0;
        const COUNT_W: f32 = 92.0;
        const MARK: f32 = 18.0;
        const GAP: f32 = 14.0;
        let u = crate::loc_ui::ui();
        let tx = x + PAD;
        self.kit_rrect(tx, cy - TILE * 0.5, TILE, TILE, 10.0, if focused { TILE_FOCUS } else { col::TILE });
        let icon = if row.direct { Icon::File } else { Icon::Archive };
        self.kit_icon(tx + (TILE - 19.0) * 0.5, cy, 19.0, icon, if focused { ICON_FOCUS } else { col::TEXT2 });
        if row.favorite {
            // The favourite's solid star, on the tile's corner as on a cover.
            const CHIP: f32 = 18.0;
            let (cx0, cy0) = (tx - 5.0, cy - TILE * 0.5 - 5.0);
            self.kit_rrect(cx0, cy0, CHIP, CHIP, 6.0, 0xE6_0A_10_1C);
            let sw = self.kit_measure("\u{2605}", 12.0);
            self.kit_text_mid((cx0 + (CHIP - sw) * 0.5).round(), cy0 + CHIP * 0.5, 12.0, "\u{2605}", col::GOLD);
        }

        // The right-hand block, from the edge inwards: the mark, the count,
        // the bar. What it says depends on what is known.
        let mut right = x + w - PAD;
        let complete = matches!(row.total, Some(t) if t > 0 && row.have >= t);
        if row.direct {
            if row.have > 0 {
                self.kit_icon(right - MARK, cy, MARK, Icon::Check, col::OK);
                right -= MARK + 10.0;
                let t = u.on_card;
                let tw = self.kit_measure(t, size::LABEL);
                self.kit_text_mid(right - tw, cy, size::LABEL, t, col::TEXT2);
                right -= tw + GAP;
            } else {
                self.kit_icon(right - MARK, cy, MARK, Icon::Download, if focused { col::GOLD } else { col::TEXT3 });
                right -= MARK + GAP;
            }
        } else {
            if complete {
                self.kit_icon(right - MARK, cy, MARK, Icon::Check, col::OK);
            } else {
                self.kit_icon(right - MARK, cy, MARK, Icon::ChevronRight, if focused { col::GOLD } else { CHEVRON_IDLE });
            }
            right -= MARK + GAP;
            match row.total {
                Some(total) if total > 0 => {
                    let count = std::format!("{} / {}", imp_count(row.have as u64), imp_count(total as u64));
                    let cw = self.kit_measure(&count, size::LABEL);
                    self.kit_text_mid(right - cw, cy, size::LABEL, &count, col::TEXT2);
                    right -= cw.max(COUNT_W) + GAP;
                    let bx = right - BAR_W;
                    self.kit_rrect(bx, cy - 2.0, BAR_W, 4.0, radius::BAR, col::LINE);
                    if row.have > 0 {
                        // At least a sliver once something is on the card, so
                        // 6 of 3 633 does not read as nothing.
                        let f = (row.have as f32 / total as f32).clamp(0.03, 1.0);
                        self.kit_rrect(bx, cy - 2.0, BAR_W * f, 4.0, radius::BAR, if complete { col::OK } else { col::GOLD });
                    }
                    right = bx - GAP;
                }
                // Never opened, but games from it ARE on the card: say how many.
                _ if row.have > 0 => {
                    let t = fill(u.imp_on_card_n, &imp_count(row.have as u64));
                    let tw = self.kit_measure(&t, size::LABEL);
                    self.kit_text_mid(right - tw, cy, size::LABEL, &t, col::TEXT2);
                    right -= tw + GAP;
                }
                _ => {}
            }
        }

        let lx = tx + TILE + GAP;
        let avail = (right - 6.0 - lx).max(40.0);
        let name = self.kit_fit_mid(row.name, size::BODY, avail);
        self.kit_text_mid(lx, cy - 9.0, size::BODY, &name, col::TEXT);
        if !row.host.is_empty() {
            let host = self.kit_fit(row.host, size::CAPTION, avail);
            self.kit_text_mid(lx, cy + 11.0, size::CAPTION, &host, col::TEXT3);
        }
    }

    /// An icon over a sentence, centred on the page around `cy`: an empty
    /// list says why it is empty rather than leaving a blank band.
    fn imp_empty_state(&mut self, icon: Icon, text: &str, cy: f32) {
        let vw = self.dimensions.width as f32;
        let lines = self.kit_wrap(text, size::BODY, 640.0, 3);
        let lh = 26.0;
        let top = cy - (lines.len() as f32 * lh) * 0.5;
        self.kit_icon(((vw - 40.0) * 0.5).round(), top - 30.0, 40.0, icon, col::TEXT3);
        let mut ly = top + 12.0;
        for line in &lines {
            let lw = self.kit_measure(line, size::BODY);
            self.kit_text_mid(((vw - lw) * 0.5).round(), ly, size::BODY, line, col::TEXT2);
            ly += lh;
        }
    }

    /// The footer's hints, right-aligned, dropping from the end the ones that
    /// would run into the credit on the left. The first hint is always what A
    /// does, so it is the last to go.
    pub(crate) fn imp_footer_hints(&mut self, hints: &[HintItem]) {
        let vw = self.dimensions.width as f32;
        let vh = self.dimensions.height as f32;
        let max_w = vw - 40.0 - 290.0;
        let mut n = hints.len();
        while n > 1 && self.kit_hints_width(&hints[..n]) > max_w {
            n -= 1;
        }
        self.kit_hints_right(vw - 40.0, vh - 24.0, &hints[..n]);
    }

    /// The order in force, drawn right-aligned at `right` on the line `cy`:
    /// the sort icon, then the outcome ("Les plus lourds"). Returns its width.
    pub(crate) fn imp_sort_badge(&mut self, right: f32, cy: f32, phrase: &str) -> f32 {
        let pw = self.kit_measure(phrase, size::CAPTION + 1.0);
        self.kit_text_mid(right - pw, cy, size::CAPTION + 1.0, phrase, col::TEXT2);
        self.kit_icon(right - pw - 8.0 - 16.0, cy, 16.0, Icon::Sort, col::TEXT2);
        pw + 8.0 + 16.0
    }
}

/// The importer pages' left margin (Importer.dc.html: 48 px), shared with the
/// geometry `library` computes rows and cards from.
const IMPORTER_MARGIN_X: f32 = crate::library::IMPORTER_MARGIN;

// ─── An archive's games ─────────────────────────────────────────────────────

/// An archive.org item's games, as data.
pub(crate) struct FilesPage<'a> {
    /// The source's name, for the title ("" = the generic title).
    pub source: &'a str,
    /// The files on screen, in the order shown (search applied).
    pub files: &'a [crate::net::RemoteFile],
    /// Which of `files` are already on the card.
    pub on_card: &'a [bool],
    pub selection: usize,
    pub scroll: usize,
    pub filter: Option<&'a str>,
    /// Every file of the item, before the search, and how many of them are on
    /// the card.
    pub total: usize,
    pub have: usize,
    /// The order in force, as its outcome.
    pub sort: &'a str,
    /// The reveal window while the list opens out of its row, intersected with
    /// the rows' own band.
    pub outer_clip: Option<(f32, f32, f32, f32)>,
    pub interactive: bool,
}

impl SwitchRenderBackend {
    /// An archive.org item's games: its name on top with how many games it
    /// holds and how many are on the card, then one row per game with its
    /// size, a tick on the ones already there.
    pub(crate) fn draw_library_distant_files(&mut self, p: &FilesPage) {
        use crate::library::{FILES_ROW_H as PITCH, FILES_ROW_TOP as TOP, DISTANT_VISIBLE_ROWS as VISIBLE};
        self.library_clear();
        let vw = self.dimensions.width as f32;
        let lc = crate::loc::s();
        let u = crate::loc_ui::ui();
        let total = p.files.len();
        let searching = p.filter.map_or(false, |f| !f.trim().is_empty());
        let sub = if searching {
            std::format!(
                "{} / {}  \u{00B7}  {}",
                imp_count(total as u64),
                imp_count(p.total as u64),
                imp_quote(p.filter.unwrap_or(""))
            )
        } else if p.have > 0 {
            std::format!(
                "{}  \u{00B7}  {}",
                crate::loc_ui::games(p.total),
                fill(u.imp_on_card_n, &imp_count(p.have as u64))
            )
        } else {
            crate::loc_ui::games(p.total)
        };
        let title = if p.source.is_empty() { lc.files_title } else { p.source };
        // The order in force sits at the right of the line under the title.
        let badge_w = if total > 1 { self.imp_sort_badge(vw - IMPORTER_MARGIN_X, 102.0, p.sort) + 24.0 } else { 0.0 };
        let sub = self.kit_fit(&sub, size::LABEL + 1.0, vw - 2.0 * IMPORTER_MARGIN_X - badge_w);
        self.kit_page_header(&[u.tab_import], title, Some(&sub));

        let band_top = TOP;
        let band_bot = TOP + VISIBLE as f32 * PITCH;
        let max_scroll = total.saturating_sub(VISIBLE);
        let target_scroll = p.scroll.min(max_scroll) as f32 * PITCH;
        let target_hover = TOP + p.selection as f32 * PITCH;
        let (mut scroll_px, mut hover_y) = (target_scroll, target_hover);
        if p.interactive {
            let mut cells: std::vec::Vec<GalleryCell> = std::vec::Vec::with_capacity(total);
            for i in 0..total {
                cells.push(GalleryCell { row: i as u32, cx: vw * 0.5, x: 0.0, y: TOP + i as f32 * PITCH, w: vw, h: PITCH });
            }
            if let Ok(mut g) = gallery_cache().lock() {
                *g = (cells, total as u32);
            }
            let touch_scroll = gallery_touch_scroll_read();
            if let Ok(mut a) = gallery_anim().lock() {
                let now = unsafe { ruffle_tick_now() };
                if !a.inited {
                    a.inited = true;
                    a.last_tick = now;
                    a.last_sel = p.selection;
                    a.scroll_px = target_scroll;
                    a.sel_x = 0.0;
                    a.sel_y = target_hover;
                    a.sel_w = 0.0;
                    a.pop = 0.0;
                } else {
                    let freq = unsafe { ruffle_tick_freq() } as f32;
                    let dt = if freq > 0.0 { (now.saturating_sub(a.last_tick) as f32 / freq).min(0.1) } else { 1.0 / 60.0 };
                    a.last_tick = now;
                    a.last_sel = p.selection;
                    a.scroll_px = ease_to(a.scroll_px, target_scroll, dt, 16.0);
                    a.sel_y = ease_to(a.sel_y, target_hover, dt, 18.0);
                }
                if let Some(px) = touch_scroll {
                    a.scroll_px = px;
                }
                scroll_px = a.scroll_px;
                hover_y = a.sel_y;
            }
            if let Ok(mut v) = gallery_view().lock() {
                *v = GalleryView {
                    scroll_px,
                    pitch: PITCH,
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
        // The rows' band, intersected with the reveal window when there is one.
        let band = (0.0f32, band_top - 2.0, vw, band_bot - band_top + 4.0);
        let clip = match p.outer_clip {
            Some((ox, oy, ow, oh)) => {
                let x0 = band.0.max(ox);
                let y0 = band.1.max(oy);
                let x1 = (band.0 + band.2).min(ox + ow);
                let y1 = (band.1 + band.3).min(oy + oh);
                (x0, y0, (x1 - x0).max(0.0), (y1 - y0).max(0.0))
            }
            None => band,
        };
        self.set_clip(clip.0, clip.1, clip.2, clip.3);
        let (x, w) = (IMPORTER_MARGIN_X, vw - 2.0 * IMPORTER_MARGIN_X);
        if total > 0 {
            self.kit_focus_pill(x, hover_y - scroll_px + 3.0, w, PITCH - 6.0);
        }
        for i in 0..total {
            let y = TOP + i as f32 * PITCH - scroll_px;
            if y + PITCH < band_top - 8.0 || y > band_bot + 8.0 {
                continue;
            }
            let f = &p.files[i];
            let have = p.on_card.get(i).copied().unwrap_or(false);
            let size_txt = imp_size(f.size_bytes);
            // The game's name as Jouer shows it: no ".swf" on every row.
            let shown = crate::library::pretty_name(&f.name);
            let row = Row::new(Icon::File, &shown).value(&size_txt).mark_if(have, Icon::CircleCheck, col::OK);
            self.kit_row_item(x, y + PITCH * 0.5, w, &row, i == p.selection);
        }
        match p.outer_clip {
            Some((ox, oy, ow, oh)) => self.set_clip(ox, oy, ow, oh),
            None => self.clear_clip(),
        }
        if total == 0 && searching {
            let msg = fill(u.imp_no_file_match, &imp_quote(p.filter.unwrap_or("")));
            self.imp_empty_state(Icon::Search, &msg, (band_top + band_bot) * 0.5 - 20.0);
        }
        self.draw_scrollbar(vw - IMPORT_SCROLLBAR_INSET, TOP + 4.0, VISIBLE as f32 * PITCH - 8.0, scroll_px, PITCH, VISIBLE, total);
        if p.interactive && total > 0 {
            let row_y = TOP + p.selection as f32 * PITCH - scroll_px;
            import_sel_rect_set((x, row_y + 3.0, w, PITCH - 6.0));
        }

        // A on a game already on the card plays it.
        self.kit_footer_band();
        let have_sel = p.on_card.get(p.selection).copied().unwrap_or(false);
        let page = [Btn::L, Btn::R];
        let mut hints: std::vec::Vec<HintItem> = std::vec::Vec::new();
        if total > 0 {
            hints.push((&[Btn::A][..], if have_sel { u.hint_play } else { u.download }));
        }
        if total > 1 {
            hints.push((&[Btn::Y][..], u.hint_sort));
        }
        hints.push((&[Btn::Minus][..], u.hint_search));
        if total > VISIBLE {
            hints.push((&page[..], u.page));
        }
        hints.push((&[Btn::B][..], u.back));
        self.imp_footer_hints(&hints);
        unsafe {
            glUseProgram(0);
            glBindVertexArray(0);
        }
        self.gl_state.invalidate();
    }
}

// ─── The import in progress ─────────────────────────────────────────────────

/// Where one step of an import stands.
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum StepState {
    Done,
    Active,
    Waiting,
}

/// One step of an import: download, extraction, the game's other files, its
/// data, its cover.
pub(crate) struct ImportStep<'a> {
    pub icon: Icon,
    pub label: &'a str,
    pub state: StepState,
    /// The active step's bar: `None` sweeps (an amount not known yet).
    pub frac: Option<f32>,
    /// Under the active step's bar ("42 %  ·  1,2 Go sur 3,9 Go ...").
    pub detail: &'a str,
}

/// An import as one page: what it is, then its steps, each ticked when done.
pub(crate) struct ImportProgress<'a> {
    pub crumbs: &'a [&'a str],
    pub title: &'a str,
    /// A Flashpoint logo to show on the left, streamed like the grid's.
    pub logo_url: Option<&'a str>,
    /// Shown instead when there is no logo.
    pub icon: Icon,
    pub steps: &'a [ImportStep<'a>],
    pub hints: &'a [HintItem<'a>],
}

impl SwitchRenderBackend {
    /// One import, one page (see `ImportProgress`). It used to be four
    /// unrelated screens: the download, an extraction drawn by the library
    /// mover with a folder path for a title, a bar restarting for each extra
    /// file, then a bar frozen at 100 % while the cover was fetched.
    pub(crate) fn draw_import_progress(&mut self, p: &ImportProgress) {
        const ART_W: f32 = 260.0;
        const ART_H: f32 = 162.0;
        const BAR_H: f32 = 10.0;
        self.library_clear();
        let vw = self.dimensions.width as f32;
        let x0 = IMPORTER_MARGIN_X;
        let top = self.kit_page_header(p.crumbs, p.title, None) + 30.0;

        // The game's logo, or what kind of import it is.
        let logo = p.logo_url.and_then(|url| {
            self.pump_thumbnail_load();
            match self.thumb_for(url) {
                Some(ThumbTex::Image { tex, w, h, amb }) if tex != 0 => Some((tex, w, h, amb)),
                _ => None,
            }
        });
        match logo {
            Some((tex, w, h, amb)) => self.kit_cover(x0, top, ART_W, ART_H, tex, w, h, amb, 1.0),
            None => {
                self.kit_rrect(x0, top, ART_W, ART_H, radius::TILE, col::TILE);
                self.kit_icon(x0 + (ART_W - 48.0) * 0.5, top + ART_H * 0.5, 48.0, p.icon, col::TEXT3);
            }
        }

        // The steps, top to bottom.
        let sx = x0 + ART_W + 40.0;
        let right = vw - IMPORTER_MARGIN_X;
        let now = unsafe { ruffle_tick_now() };
        let mut y = top + 4.0;
        for step in p.steps {
            let cy = y + 14.0;
            match step.state {
                StepState::Done => {
                    self.kit_icon(sx, cy, 22.0, Icon::CircleCheck, col::OK);
                }
                StepState::Active => {
                    self.draw_spinner(sx + 11.0, cy, 9.0, now);
                }
                StepState::Waiting => {
                    self.kit_icon(sx + 1.0, cy, 20.0, step.icon, col::TEXT3);
                }
            }
            let ink = match step.state {
                StepState::Active => col::TEXT,
                StepState::Done => col::TEXT2,
                StepState::Waiting => col::TEXT3,
            };
            let lx = sx + 38.0;
            let label = self.kit_fit(step.label, size::ROW, right - lx);
            self.kit_text_mid(lx, cy, size::ROW, &label, ink);
            if step.state != StepState::Active {
                y += 46.0;
                continue;
            }
            let by = y + 34.0;
            let bw = right - lx;
            self.kit_rrect(lx, by, bw, BAR_H, BAR_H * 0.5, col::LINE);
            match step.frac {
                Some(f) => {
                    let f = f.clamp(0.0, 1.0);
                    if f > 0.0 {
                        self.kit_rrect(lx, by, (bw * f).max(BAR_H), BAR_H, BAR_H * 0.5, col::GOLD);
                    }
                }
                None => {
                    // An amount not known yet: a band sweeps the track rather
                    // than a fill claiming a share it does not know.
                    let freq = unsafe { ruffle_tick_freq() } as f32;
                    let t = if freq > 0.0 { (now as f32 / freq) * 0.7 } else { 0.0 };
                    let seg = bw * 0.25;
                    let ph = t - t.floor();
                    let bx = lx - seg + (bw + seg) * ph;
                    let (a, b) = (bx.max(lx), (bx + seg).min(lx + bw));
                    if b > a {
                        self.kit_rrect(a, by, b - a, BAR_H, BAR_H * 0.5, col::GOLD);
                    }
                }
            }
            if !step.detail.is_empty() {
                let d = self.kit_fit(step.detail, size::LABEL + 1.0, bw);
                self.kit_text_mid(lx, by + BAR_H + 20.0, size::LABEL + 1.0, &d, col::TEXT2);
            }
            y += 92.0;
        }

        self.kit_footer_band();
        if !p.hints.is_empty() {
            self.imp_footer_hints(p.hints);
        }
        unsafe {
            glUseProgram(0);
            glBindVertexArray(0);
        }
        self.gl_state.invalidate();
    }

    /// The page an archive's games will be on, while its list is fetched: its
    /// header already there, a spinner where the list will be, B to cancel.
    /// Drawn inside the window that opens out of the source's row, so the page
    /// the window opens onto is the page that stays.
    pub(crate) fn draw_import_loading(&mut self, crumbs: &[&str], title: &str, sub: &str, now: u64) {
        self.library_clear();
        let vw = self.dimensions.width as f32;
        self.kit_page_header(crumbs, title, Some(sub));
        self.draw_spinner(vw * 0.5, 380.0, 26.0, now);
        self.kit_footer_band();
        self.imp_footer_hints(&[(&[Btn::B][..], crate::loc_ui::ui().cancel)]);
        unsafe {
            glUseProgram(0);
            glBindVertexArray(0);
        }
        self.gl_state.invalidate();
    }

    /// What went wrong on the importer, as a page: where it happened
    /// ("Importer › papalouieswfs"), what failed as the title, the message in
    /// a card under a red mark, and the hints that can do something about it.
    ///
    /// It was "Erreur" over the message with "A B OK" under it: two glyphs and
    /// no verb, nothing saying which source or game, and a keyboard as the only
    /// way out of a timeout.
    pub(crate) fn draw_import_error(&mut self, crumbs: &[&str], title: &str, msg: &str, hints: &[HintItem]) {
        const DISC: f32 = 56.0;
        self.library_clear();
        let vw = self.dimensions.width as f32;
        let x0 = IMPORTER_MARGIN_X;
        let top = self.kit_page_header(crumbs, title, None) + 30.0;
        let w = vw - 2.0 * x0;
        let text_x = x0 + 36.0 + DISC + 24.0;
        let lines = self.kit_wrap(msg, size::BODY, x0 + w - 36.0 - text_x, 8);
        let lh = (size::BODY * 1.45).round();
        let h = (lines.len() as f32 * lh + 72.0).max(DISC + 72.0);
        self.kit_panel(x0, top, w, h, false);
        self.kit_rrect(x0 + 36.0, top + 36.0, DISC, DISC, DISC * 0.5, 0xFF_4A_22_34);
        self.kit_icon(x0 + 36.0 + 14.0, top + 36.0 + DISC * 0.5, 28.0, Icon::Alert, col::DANGER);
        // A short message sits level with the mark; a long one starts at its top.
        let block = lines.len() as f32 * lh;
        let mut ly = top + 36.0 + (DISC - block).max(0.0) * 0.5 + lh * 0.5;
        for line in &lines {
            self.kit_text_mid(text_x, ly, size::BODY, line, col::TEXT);
            ly += lh;
        }
        self.kit_footer_band();
        self.imp_footer_hints(hints);
        unsafe {
            glUseProgram(0);
            glBindVertexArray(0);
        }
        self.gl_state.invalidate();
    }
}

// ─── The rect a launch grows from ───────────────────────────────────────────

/// The selected item's last drawn rect on an importer page (a source row, a
/// game row, a Flashpoint tile), so "A = Jouer" can grow the game's cover out
/// of what was pressed, as the Play grid does from its tile.
static IMPORT_SEL_RECT: std::sync::Mutex<(f32, f32, f32, f32)> = std::sync::Mutex::new((0.0, 0.0, 0.0, 0.0));

pub(crate) fn import_sel_rect_set(rect: (f32, f32, f32, f32)) {
    if let Ok(mut g) = IMPORT_SEL_RECT.lock() {
        *g = rect;
    }
}

pub(crate) fn import_sel_rect() -> (f32, f32, f32, f32) {
    IMPORT_SEL_RECT.lock().map(|g| *g).unwrap_or((0.0, 0.0, 0.0, 0.0))
}
