//! The IMPORTER tab (Importer.dc.html) and the pages it opens: an archive's
//! games, the import in progress, and what went wrong.
//!
//! The home ("Importer · À découvrir", board C1) is a page that scrolls: two
//! shelves of Flashpoint games, the studio of the day and a draw in a genre
//! (six screenshots each, then the shelf's action), then "Mes sources" with
//! how many there are, the add button and the order in force, then one row
//! per saved source: a tile saying what kind it is, its name over its site,
//! how much of it is on the card as a bar and a count, and a check once it is
//! all there or a chevron while there is more to get. The search is a button
//! in the tab bar, run from anywhere by X. The page used to open on two big
//! cards (search, add an address): it said what could be done, but gave
//! nothing to want.

use super::super::kit::icons::Icon;
use super::super::kit::list::Row;
use super::super::kit::theme::{col, radius, size};
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

/// What a transfer has got so far: `imp_size`, except that nothing yet reads
/// "0 o". `imp_size` keeps "" for 0 because a size not known arrives as 0, and
/// rounding the start of a download up to one byte showed "1 o sur 340 Mo".
pub(crate) fn imp_amount(bytes: u64) -> std::string::String {
    if bytes == 0 {
        fill(crate::loc_ui::ui().imp_size_b, "0")
    } else {
        imp_size(bytes)
    }
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
        parts.push(crate::loc_ui::percent(pct));
        parts.push(imp_n_of(&imp_amount(done), &imp_size(total)));
    } else {
        parts.push(imp_amount(done));
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

/// One game on a discovery shelf.
pub(crate) struct ShelfTile<'a> {
    pub title: &'a str,
    /// Its screenshot's address (`discover::screenshot_url`).
    pub thumb: &'a str,
    /// On the card already: the tick, and A plays it.
    pub installed: bool,
}

/// A discovery shelf of the importer home: its line (what it is, which
/// studio or genre, a word on how it changes), six games, then its action.
pub(crate) struct ShelfView<'a> {
    /// On the page (see `library::Shelf::shown`).
    pub shown: bool,
    /// Its search is on its way: waiting tiles.
    pub loading: bool,
    pub title: &'a str,
    pub sub: &'a str,
    pub note: &'a str,
    pub tiles: &'a [ShelfTile<'a>],
    /// Its two actions, after its games: see all (or more), draw again.
    pub actions: [ShelfAction<'a>; 2],
}

/// One of a shelf's two action tiles: what it does (the name under it), the
/// word under its icon, its icon, and whether its icon is gold (draw again,
/// the tile that changes the shelf, as the dice was in the mockup).
pub(crate) struct ShelfAction<'a> {
    pub label: &'a str,
    pub sub: &'a str,
    pub icon: Icon,
    pub gold: bool,
}

/// The importer home as data ("Importer · À découvrir", board C1).
/// `selection` counts the studio's seven tiles, the draw's seven, the add
/// button, then the sources (`library::IMPORTER_PINNED_ROWS`); `scroll` is
/// the page's, in pixels.
pub(crate) struct ImporterHome<'a> {
    pub rows: &'a [SourceRow<'a>],
    pub selection: usize,
    pub scroll: usize,
    pub filter: Option<&'a str>,
    /// Every saved source, before the search.
    pub total: usize,
    /// The order in force, as the outcome it gives ("Dernières ajoutées").
    pub sort: &'a str,
    /// The studio of the day, then the draw.
    pub shelves: [ShelfView<'a>; 2],
    /// False when the page is only the backdrop of something drawn over it (a
    /// window, the list that opens out of a row): it then neither drives the
    /// shared glide nor publishes rows for touch, which belong to what is on top.
    pub interactive: bool,
}
/// How far inside its row the focus pill sits, above and below.
const PILL_INSET: f32 = 3.0;
/// The rate (`ease_to`) of both the list's scroll and the focus, which must
/// be one rate. Headed for a row, the focus is carried by each frame's scroll
/// change and then eased: at one rate the two make a single glide on the
/// screen, from where the focus is to where its row will rest, however far the
/// list scrolls. At two rates (16 and 18) they pulled apart on a long scroll:
/// from a card to the first of 60 sources the focus rose into the tabs before
/// coming down, and to the last one it shot past its row out of the band. 16
/// is every list's scroll rate.
const GLIDE_RATE: f32 = 16.0;
/// Where the vertical scrollbar of every importer page sits, in the right
/// margin past the rows (which end at the page margin, 48 px in).
pub(crate) const IMPORT_SCROLLBAR_INSET: f32 = 40.0;
/// The chevron's colour on a row that is not focused (the mockup's #5A6A8F).
const CHEVRON_IDLE: u32 = 0xFF_5A_6A_8F;
/// The tint of a focused icon tile: the gold at 14 %, as on the cards.
const TILE_FOCUS: u32 = 0x24_FF_D7_40;
const ICON_FOCUS: u32 = 0xFF_FF_E0_7A;

/// `idle` turned `hl` of the way (0 to 1) into `focused`, both opaque: a card
/// or a row lights up as far as the focus covers it.
fn lit(idle: u32, focused: u32, hl: f32) -> u32 {
    0xFF00_0000 | super::super::kit::cover::mix_rgb(idle, focused, hl.clamp(0.0, 1.0))
}

/// How much of `item` the focus rect `f` covers, 0 to 1. Each card and row
/// lights up by this much, so its highlight comes and goes with the focus
/// passing over it instead of switching on the frame the cursor moves.
fn coverage(f: (f32, f32, f32, f32), item: (f32, f32, f32, f32)) -> f32 {
    let w = (f.0 + f.2).min(item.0 + item.2) - f.0.max(item.0);
    let h = (f.1 + f.3).min(item.1 + item.3) - f.1.max(item.1);
    if w <= 0.0 || h <= 0.0 || item.2 <= 0.0 || item.3 <= 0.0 {
        return 0.0;
    }
    (w * h / (item.2 * item.3)).clamp(0.0, 1.0)
}

impl SwitchRenderBackend {
    /// Draw the importer home (see `ImporterHome`).
    pub(crate) fn draw_library_distant_list(&mut self, home: &ImporterHome) {
        use crate::library::{
            add_button_set, importer_item_rect, importer_max_scroll, importer_note_top, shelf_tile_size, shelf_top,
            sources_head_top, sources_rows_top, ADD_BUTTON_H, IMPORTER_ADD_ROW as ADD, IMPORTER_BAND_BOT as BAND_BOT,
            IMPORTER_BAND_TOP as BAND_TOP, IMPORTER_NOTE_H, IMPORTER_PINNED_ROWS as PINNED, IMPORTER_ROW_H as ROW_H,
            SHELF_COLS, SHELF_HEAD_H, SHELF_SEE, SOURCES_HEAD_H,
        };
        self.library_clear();
        let vw = self.dimensions.width as f32;
        let lc = crate::loc::s();
        let u = crate::loc_ui::ui();
        let n = home.rows.len();
        // A row the list no longer has (it shrank under the cursor) is drawn
        // as the last one, as the next press will take it.
        let sel = home.selection.min(n + PINNED - 1);
        let now = unsafe { ruffle_tick_now() };
        // The shelves' screenshots come in through the thumbnail pool.
        self.pump_thumbnail_load();

        // ── The search, a button in the tab bar ─────────────────────────
        self.imp_search_button(vw);

        // ── Where the add button goes ───────────────────────────────────
        // In the "Mes sources" band, before the order in force: both are
        // words, so the button is placed from what they measure, before
        // anything is laid out from it (the focus, the touch table).
        let searching = home.filter.map_or(false, |f| !f.trim().is_empty());
        let band_right = vw - IMPORTER_MARGIN_X - 12.0;
        let sort_w = if n > 0 { self.kit_measure(home.sort, size::CAPTION + 1.0) + 8.0 + 16.0 } else { 0.0 };
        let add_w = 14.0 + 16.0 + 8.0 + self.kit_measure(u.imp_add_title, size::CAPTION + 1.0) + 16.0;
        let add_x = band_right - sort_w - if n > 0 { 18.0 } else { 0.0 } - add_w;
        add_button_set(add_x, add_w);

        // ONE focus for the page, from tile to tile, down to the add button
        // and the rows and back: its rect, its look (a ring round a tile or
        // the button, a pill on a row) and its band glide together, so it
        // never jumps at either end. The whole page scrolls now, so the focus
        // is carried by each frame's scroll change, then eased at the
        // scroll's own rate: one glide on the screen, from where it is to
        // where its item will rest, however far the page scrolls.
        let max_scroll = importer_max_scroll(n, vw);
        let target_scroll = (home.scroll as f32).min(max_scroll);
        let on_row = sel > ADD;
        let corner = if sel == ADD { ADD_BUTTON_H * 0.5 } else { radius::TILE };
        let focus_at = |scroll_px: f32| {
            let (x, y, w, h) = importer_item_rect(sel, scroll_px, vw);
            if on_row {
                FocusGlide::at((x, y + PILL_INSET, w, h - 2.0 * PILL_INSET), 1.0, (BAND_TOP, BAND_BOT))
            } else {
                FocusGlide::at((x, y, w, h), 0.0, (BAND_TOP, BAND_BOT))
            }
        };
        let (mut scroll_px, mut focus) = (target_scroll, focus_at(target_scroll));
        if home.interactive {
            let touch_scroll = gallery_touch_scroll_read();
            if let Ok(mut a) = gallery_anim().lock() {
                if !a.inited {
                    // A fresh page (the tab, a new search or order, back from
                    // another page): everything where it belongs, no flight.
                    a.inited = true;
                    a.last_tick = now;
                    a.scroll_px = touch_scroll.unwrap_or(target_scroll);
                    a.imp_focus = focus_at(a.scroll_px);
                } else {
                    let freq = unsafe { ruffle_tick_freq() } as f32;
                    let dt = if freq > 0.0 { (now.saturating_sub(a.last_tick) as f32 / freq).min(0.1) } else { 1.0 / 60.0 };
                    a.last_tick = now;
                    let before = a.scroll_px;
                    // A finger on the page wins over the easing, 1:1.
                    let eased = ease_to(a.scroll_px, target_scroll, dt, GLIDE_RATE);
                    a.scroll_px = touch_scroll.unwrap_or(eased);
                    a.imp_focus.y -= a.scroll_px - before;
                    let to = focus_at(a.scroll_px);
                    a.imp_focus.ease(&to, dt, GLIDE_RATE);
                }
                scroll_px = a.scroll_px;
                focus = a.imp_focus;
            }
        }
        let frect = focus.rect();

        // ── The shelves ─────────────────────────────────────────────────
        self.set_clip(0.0, BAND_TOP, vw, BAND_BOT - BAND_TOP);
        let (_, tile_h) = shelf_tile_size(vw);
        let label_px = size::CAPTION + 1.0;
        for (k, shelf) in home.shelves.iter().enumerate() {
            let Some(top) = shelf_top(k, vw) else { continue };
            let y0 = top - scroll_px;
            if y0 > BAND_BOT || y0 + SHELF_HEAD_H + tile_h + 30.0 < BAND_TOP {
                continue;
            }
            // Its line: where its games come from (Flashpoint's logo), what it
            // is, which studio or genre, how it changes.
            let hy = y0 + 11.0;
            let mut hx = IMPORTER_MARGIN_X + 12.0;
            let logo_w = self.draw_fp_logo(hx, hy, 16.0);
            if logo_w > 0.0 {
                hx += logo_w + 8.0;
            }
            hx += self.kit_text_mid(hx, hy, size::LABEL, shelf.title, col::TEXT) + 10.0;
            let note_w = self.kit_measure(shelf.note, label_px);
            self.kit_text_mid(band_right - note_w, hy, label_px, shelf.note, col::TEXT2);
            if !shelf.sub.is_empty() {
                let st = self.kit_fit(shelf.sub, size::LABEL, (band_right - note_w - 24.0 - hx).max(0.0));
                self.kit_text_mid(hx, hy, size::LABEL, &st, col::TEXT3);
            }
            for c in 0..SHELF_COLS {
                let (x, y, w, h) = importer_item_rect(k * SHELF_COLS + c, scroll_px, vw);
                let hl = coverage(frect, (x, y, w, h));
                // The name under the tile, lit as far as the focus covers it.
                let label_y = y + h + 6.0 + 9.0;
                if c >= SHELF_SEE {
                    let action = &shelf.actions[c - SHELF_SEE];
                    self.imp_shelf_action(x, y, w, h, action, shelf.loading, hl);
                    let t = self.kit_fit(action.label, label_px, w);
                    self.kit_text_mid(x, label_y, label_px, &t, lit(col::TEXT2, col::TEXT, hl));
                    continue;
                }
                if shelf.loading {
                    self.kit_rrect(x, y, w, h, radius::TILE, col::TILE);
                    self.kit_icon(x + (w - 24.0) * 0.5, y + h * 0.5, 24.0, Icon::Hourglass, col::TEXT3);
                    continue;
                }
                // A shelf with fewer games: nothing there, its action
                // already sits right after the last one.
                let Some(tile) = shelf.tiles.get(c) else { continue };
                match self.thumb_for(tile.thumb) {
                    Some(ThumbTex::Image { tex, w: iw, h: ih, amb }) => self.kit_cover(x, y, w, h, tex, iw, ih, amb, 1.0),
                    // No screenshot: the tile carries the game's name, as a
                    // game without a cover does on the Play grid.
                    Some(ThumbTex::Failed) => {
                        self.kit_default_tile(x, y, w, h, super::flashpoint::tile_tint(tile.thumb), tile.title)
                    }
                    None => {
                        self.kit_rrect(x, y, w, h, radius::TILE, col::TILE);
                        self.kit_icon(x + (w - 24.0) * 0.5, y + h * 0.5, 24.0, Icon::Hourglass, col::TEXT3);
                    }
                }
                if tile.installed {
                    // Already on the card, as in the results.
                    const CHIP: f32 = 26.0;
                    let (bx, by) = (x + w - CHIP - 8.0, y + 8.0);
                    self.kit_rrect(bx, by, CHIP, CHIP, 8.0, 0xB8_0A_10_1C);
                    self.kit_icon(bx + 5.0, by + CHIP * 0.5, 16.0, Icon::CircleCheck, col::OK);
                }
                let t = self.kit_fit(tile.title, label_px, w);
                self.kit_text_mid(x, label_y, label_px, &t, lit(col::TEXT2, col::TEXT, hl));
            }
        }
        // Neither shelf could be fetched: one line where they were says so,
        // rather than a page that lost its top without a word.
        if let Some(top) = importer_note_top() {
            let cy = top + IMPORTER_NOTE_H * 0.5 - scroll_px;
            let x = IMPORTER_MARGIN_X + 12.0;
            self.kit_icon(x, cy, 18.0, Icon::WifiOff, col::TEXT3);
            let t = self.kit_fit(u.disc_unavailable, size::LABEL, band_right - x - 28.0);
            self.kit_text_mid(x + 28.0, cy, size::LABEL, &t, col::TEXT3);
        }

        // ── "Mes sources", how many, the add button, the order in force ──
        let hy = sources_head_top(vw) - scroll_px + (SOURCES_HEAD_H - 6.0) * 0.5;
        let mut hx = IMPORTER_MARGIN_X + 12.0;
        hx += self.kit_text_mid(hx, hy, size::LABEL, lc.dist_sources, col::TEXT) + 10.0;
        if n > 0 {
            let pw = self.kit_measure(home.sort, label_px);
            self.kit_text_mid(band_right - pw, hy, label_px, home.sort, col::TEXT2);
            self.kit_icon(band_right - pw - 8.0 - 16.0, hy, 16.0, Icon::Sort, col::TEXT2);
        }
        if searching {
            let count = std::format!("{} / {}", imp_count(n as u64), imp_count(home.total as u64));
            hx += self.kit_text_mid(hx, hy, size::LABEL, &count, col::TEXT2);
            let term = std::format!("  \u{00B7}  {}", imp_quote(home.filter.unwrap_or("")));
            let term = self.kit_fit(&term, size::LABEL, (add_x - 18.0 - hx).max(0.0));
            self.kit_text_mid(hx, hy, size::LABEL, &term, col::TEXT3);
        } else {
            self.kit_text_mid(hx, hy, size::LABEL, &imp_count(home.total as u64), col::TEXT3);
        }
        {
            let (bx, by, bw, bh) = importer_item_rect(ADD, scroll_px, vw);
            let hl = coverage(frect, (bx, by, bw, bh));
            let cy = by + bh * 0.5;
            self.kit_rrect(bx, by, bw, bh, bh * 0.5, lit(col::PANEL, col::TILE, hl));
            self.kit_icon(bx + 14.0, cy, 16.0, Icon::Link, lit(col::TEXT2, ICON_FOCUS, hl));
            self.kit_text_mid(bx + 14.0 + 16.0 + 8.0, cy, label_px, u.imp_add_title, col::TEXT);
        }
        self.clear_clip();

        // ── The focus ───────────────────────────────────────────────────
        // Over the tiles (a ring sits round a tile, not on it) and under the
        // rows' words, inside the band the page scrolls in.
        self.set_clip(0.0, focus.clip_top, vw, (focus.clip_bot - focus.clip_top).max(0.0));
        self.kit_focus_morph(focus.x, focus.y, focus.w, focus.h, corner, focus.look);
        self.clear_clip();

        // ── The sources ─────────────────────────────────────────────────
        self.set_clip(0.0, BAND_TOP, vw, BAND_BOT - BAND_TOP);
        let rows_top = sources_rows_top(vw);
        let first = ((scroll_px + BAND_TOP - rows_top) / ROW_H).floor().max(0.0) as usize;
        let last = (((scroll_px + BAND_BOT - rows_top) / ROW_H).ceil().max(0.0) as usize + 1).min(n);
        for k in first..last {
            let (x, y, w, h) = importer_item_rect(k + PINNED, scroll_px, vw);
            let hl = coverage(frect, (x, y + PILL_INSET, w, h - 2.0 * PILL_INSET));
            self.imp_source_row(x, y + h * 0.5, w, &home.rows[k], hl);
        }
        self.clear_clip();

        // ── Nothing to list ─────────────────────────────────────────────
        if n == 0 {
            let cy = (rows_top - scroll_px + 70.0).min(BAND_BOT - 90.0);
            if searching {
                let msg = fill(u.imp_no_source_match, &imp_quote(home.filter.unwrap_or("")));
                self.imp_empty_state(Icon::Search, &msg, cy);
            } else {
                self.imp_empty_state(Icon::Link, u.imp_empty_sources, cy);
            }
        }

        // The page's scrollbar, in pixels: its whole height against the band.
        let band_h = BAND_BOT - BAND_TOP;
        self.draw_scrollbar(
            vw - IMPORT_SCROLLBAR_INSET,
            BAND_TOP + 8.0,
            band_h - 16.0,
            scroll_px,
            1.0,
            band_h as usize,
            (max_scroll + band_h) as usize,
        );

        // ── Touch ───────────────────────────────────────────────────────
        // One cell per item, at its index in the selection, so a hit is the
        // selection it lands on; an item off the page, half out of the band,
        // or with nothing in it (a shelf not shown, an empty place) takes no
        // tap. Cells are in the page's scrolled space.
        if home.interactive {
            let mut cells: std::vec::Vec<GalleryCell> = std::vec::Vec::with_capacity(n + PINNED);
            for i in 0..n + PINNED {
                let (x, y, w, h) = importer_item_rect(i, scroll_px, vw);
                let there = if i < ADD {
                    let (k, c) = (i / SHELF_COLS, i % SHELF_COLS);
                    let shelf = &home.shelves[k];
                    shelf.shown && (shelf.loading || c >= SHELF_SEE || c < shelf.tiles.len())
                } else {
                    true
                };
                let inside = y >= BAND_TOP - 1.0 && y + h <= BAND_BOT + 1.0;
                cells.push(if there && inside {
                    GalleryCell { row: i as u32, cx: x + w * 0.5, x, y: y + scroll_px, w, h }
                } else {
                    GalleryCell::default()
                });
            }
            if let Ok(mut g) = gallery_cache().lock() {
                *g = (cells, (n + PINNED) as u32);
            }
            if let Ok(mut v) = gallery_view().lock() {
                // In pixels: a pitch of one, the page as its rows.
                *v = GalleryView {
                    scroll_px,
                    pitch: 1.0,
                    band_top: BAND_TOP,
                    band_bot: BAND_BOT,
                    rows_total: (max_scroll + band_h) as u32,
                    rows_visible: band_h as u32,
                    horizontal: false,
                    off_min: 0.0,
                    off_max: 0.0,
                };
            }
            // What a game started from here grows out of.
            import_sel_rect_set(importer_item_rect(sel, scroll_px, vw));
        }

        // ── Hints: what A does here, and only what can be done ──────────
        self.kit_footer_band();
        let mut hints: std::vec::Vec<HintItem> = std::vec::Vec::new();
        if sel < ADD {
            let (k, c) = (sel / SHELF_COLS, sel % SHELF_COLS);
            let shelf = &home.shelves[k];
            if c >= SHELF_SEE {
                if !shelf.loading {
                    hints.push((&[Btn::A][..], shelf.actions[c - SHELF_SEE].label));
                }
            } else if let (false, Some(t)) = (shelf.loading, shelf.tiles.get(c)) {
                hints.push((&[Btn::A][..], if t.installed { u.hint_play } else { u.download }));
                hints.push((&[Btn::Plus][..], u.info));
            }
            hints.push((&[Btn::X][..], lc.fp_title));
        } else if sel == ADD {
            hints.push((&[Btn::A][..], u.add));
            hints.push((&[Btn::X][..], lc.fp_title));
        } else {
            let a_verb = match home.rows.get(sel - PINNED) {
                Some(r) if r.direct && r.have > 0 => u.hint_play,
                Some(r) if r.direct => u.download,
                _ => u.open,
            };
            hints.push((&[Btn::A][..], a_verb));
            hints.push((&[Btn::Plus][..], u.hint_options));
        }
        if sel >= ADD {
            if n > 1 {
                hints.push((&[Btn::Y][..], u.hint_sort));
            }
            // Over a search that found nothing, Minus says what it is for
            // there, in the words Jouer uses; B shows every source again,
            // the same words on every list.
            if home.total > 0 {
                let verb = if searching && n == 0 { u.home_edit_search } else { u.hint_search };
                hints.push((&[Btn::Minus][..], verb));
            }
        }
        if searching {
            hints.push((&[Btn::B][..], u.home_show_all));
        }
        self.imp_footer_hints(&hints);
        unsafe {
            glUseProgram(0);
            glBindVertexArray(0);
        }
        self.gl_state.invalidate();
    }

    /// Flashpoint's logo (`assets/flashpoint.png`, its colourised arrow,
    /// cropped and made smaller; CC BY 4.0, credited in About), made the first
    /// time it is drawn. `None` when it could not be: the caller draws an icon.
    /// About shows it too, on the row that credits it.
    pub(crate) fn fp_logo(&mut self) -> Option<(GLuint, u32, u32)> {
        if !self.fp_logo_tried {
            self.fp_logo_tried = true;
            const PNG: &[u8] = include_bytes!("../../../../../assets/flashpoint.png");
            if let Some((mut rgba, w, h)) = crate::covers::decode_bytes(PNG) {
                // Drawn far smaller than it is, through its mipmaps, which
                // would average its transparent margin's black into its edges.
                crate::covers::bleed_transparent(&mut rgba, w, h);
                let tex = self.upload_cover_texture(&rgba, w, h);
                self.fp_logo = (tex != 0).then_some((tex, w, h));
            }
        }
        self.fp_logo
    }

    /// Flashpoint's logo `h` px tall, from `x`, centred on `cy`. Returns its
    /// width, 0 when there is no logo (nothing drawn).
    fn draw_fp_logo(&mut self, x: f32, cy: f32, h: f32) -> f32 {
        let Some((tex, tw, th)) = self.fp_logo() else { return 0.0 };
        let w = (h * tw as f32 / th.max(1) as f32).round();
        self.kit_image(x, (cy - h * 0.5).round(), w, h, tex, [0.0, 0.0, 1.0, 1.0], 1.0, 0.0);
        w
    }

    /// The importer's search, a button at the right of the tab bar:
    /// Flashpoint's logo, what it searches, and X, which runs it from anywhere
    /// on the page. It does not scroll with the page. Published for a tap and
    /// for the results page, which opens out of it.
    fn imp_search_button(&mut self, vw: f32) {
        const H: f32 = 40.0;
        const CY: f32 = 36.0;
        const LOGO_H: f32 = 20.0;
        let u = crate::loc_ui::ui();
        let px = size::LABEL;
        let glyph = 24.0;
        let gw = self.kit_button_width(Btn::X, glyph);
        let label = self.kit_fit(u.imp_fp_title, px, 260.0);
        let tw = self.kit_measure(&label, px);
        // The logo's width, or the search icon's when it could not be made.
        let mark_w = self.fp_logo().map_or(18.0, |(_, lw, lh)| (LOGO_H * lw as f32 / lh.max(1) as f32).round());
        let w = 16.0 + mark_w + 10.0 + tw + 12.0 + gw + 8.0;
        let x = vw - 40.0 - w;
        self.kit_rrect(x, CY - H * 0.5, w, H, H * 0.5, col::TILE);
        if self.draw_fp_logo(x + 16.0, CY, LOGO_H) == 0.0 {
            self.kit_icon(x + 16.0, CY, 18.0, Icon::Search, col::TEXT2);
        }
        self.kit_text_mid(x + 16.0 + mark_w + 10.0, CY, px, &label, col::TEXT2);
        self.kit_button(x + w - 8.0 - gw, CY, glyph, Btn::X, col::GLYPH);
        crate::library::search_button_set((x, CY - H * 0.5, w, H));
    }

    /// One of a shelf's action tiles: an icon in its box, a word under it
    /// (how many games the studio has; that another one comes), lit as far as
    /// the focus covers it. Nothing under the icon while the shelf loads.
    #[allow(clippy::too_many_arguments)]
    fn imp_shelf_action(&mut self, x: f32, y: f32, w: f32, h: f32, action: &ShelfAction, loading: bool, hl: f32) {
        const BOX: f32 = 44.0;
        let gold = action.gold;
        self.kit_rrect(x, y, w, h, radius::TILE, lit(col::PANEL, col::TILE, hl));
        let sub = if loading { "" } else { action.sub };
        let cy = y + h * 0.5 - if sub.is_empty() { 0.0 } else { 10.0 };
        let bx = x + (w - BOX) * 0.5;
        if gold {
            self.kit_rrect(bx, cy - BOX * 0.5, BOX, BOX, 12.0, TILE_FOCUS);
        } else {
            self.kit_rrect(bx, cy - BOX * 0.5, BOX, BOX, 12.0, col::LINE);
        }
        let ink = if gold { ICON_FOCUS } else { lit(col::TEXT2, ICON_FOCUS, hl) };
        self.kit_icon(bx + (BOX - 22.0) * 0.5, cy, 22.0, action.icon, ink);
        if !sub.is_empty() {
            let t = self.kit_fit(sub, size::CAPTION, w - 12.0);
            let tw = self.kit_measure(&t, size::CAPTION);
            self.kit_text_mid((x + (w - tw) * 0.5).round(), cy + BOX * 0.5 + 12.0, size::CAPTION, &t, col::TEXT3);
        }
    }

    /// An icon's tile on a card or a row, `hl` of the way from `idle` to
    /// `focused` with the gold tint over it, which is how it looks under the
    /// focus. The tint is translucent and used to sit straight on what was
    /// under the tile (the card's fill, the pill's); laid over an opaque
    /// colour of its own instead, it looks the same at rest and fades with
    /// the focus.
    #[allow(clippy::too_many_arguments)]
    fn imp_icon_tile(&mut self, x: f32, y: f32, side: f32, corner: f32, idle: u32, focused: u32, hl: f32) {
        self.kit_rrect(x, y, side, side, corner, lit(idle, focused, hl));
        let alpha = ((TILE_FOCUS >> 24) as f32 * hl.clamp(0.0, 1.0)).round() as u32;
        if alpha > 0 {
            self.kit_rrect(x, y, side, side, corner, (alpha << 24) | (TILE_FOCUS & 0x00FF_FFFF));
        }
    }

    /// One source row centred on `cy` between `x` and `x + w`, lit as far as
    /// the focus covers it (`hl`, 0 to 1; the focus itself is the caller's).
    fn imp_source_row(&mut self, x: f32, cy: f32, w: f32, row: &SourceRow, hl: f32) {
        const PAD: f32 = 12.0;
        const TILE: f32 = 38.0;
        const BAR_W: f32 = 120.0;
        const COUNT_W: f32 = 92.0;
        const MARK: f32 = 18.0;
        const GAP: f32 = 14.0;
        let u = crate::loc_ui::ui();
        let tx = x + PAD;
        // Under the focus the tile sat on the pill's fill: it turns into it.
        self.imp_icon_tile(tx, cy - TILE * 0.5, TILE, 10.0, col::TILE, col::CHIP, hl);
        let icon = if row.direct { Icon::File } else { Icon::Archive };
        self.kit_icon(tx + (TILE - 19.0) * 0.5, cy, 19.0, icon, lit(col::TEXT2, ICON_FOCUS, hl));
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
                self.kit_icon(right - MARK, cy, MARK, Icon::Download, lit(col::TEXT3, col::GOLD, hl));
                right -= MARK + GAP;
            }
        } else {
            if complete {
                self.kit_icon(right - MARK, cy, MARK, Icon::Check, col::OK);
            } else {
                self.kit_icon(right - MARK, cy, MARK, Icon::ChevronRight, lit(CHEVRON_IDLE, col::GOLD, hl));
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
        // A bare archive.org identifier has no site to show under it: the name
        // then sits in the middle of its row.
        let name_cy = if row.host.is_empty() { cy } else { cy - 9.0 };
        self.kit_text_mid(lx, name_cy, size::BODY, &name, col::TEXT);
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
                    // One rate for both: the pill is drawn at its content
                    // place minus the scroll, and at two rates it ran past its
                    // row on a long jump (last game to first), as the home's
                    // focus did (`GLIDE_RATE`).
                    a.scroll_px = ease_to(a.scroll_px, target_scroll, dt, GLIDE_RATE);
                    a.sel_y = ease_to(a.sel_y, target_hover, dt, GLIDE_RATE);
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
        // B shows every game again after a search before it goes back, as on
        // every list, and says so; over a search that found nothing, Minus
        // says what it is for there, in the words Jouer uses.
        hints.push((&[Btn::Minus][..], if searching && total == 0 { u.home_edit_search } else { u.hint_search }));
        if total > VISIBLE {
            hints.push((&page[..], u.page));
        }
        hints.push((&[Btn::B][..], if searching { u.home_show_all } else { u.back }));
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
