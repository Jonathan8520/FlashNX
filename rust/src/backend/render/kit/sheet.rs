//! The side panel, and the cascade that opens a sub-panel beside it.
//!
//! One panel for every list of settings or actions that sits over something
//! still worth seeing: the pause menu over the frozen game, a game's options
//! over the library. From the top: a caption saying where you are ("Pause",
//! with its icon), the title, optional status chips, a hairline, the rows
//! (icon, label, control, chevron), a box explaining the focused row, and the
//! hints. Built from data (`Sheet`), so the in-game pause and the launcher's
//! options are the same object and cannot drift apart again: they had, the
//! in-game one had no icons at all.
//!
//! The cascade (Jonathan, 2026-10-12: "appuyer sur Écran ajoute le modal sur
//! la droite du principal, un fil d'Ariane moderne"). A row with a chevron no
//! longer swaps the panel for another: the parent folds to a narrow rail that
//! keeps its rows and marks the one you came from, and the sub-panel slides
//! out from under it to its right. B slides it back. The path stays on screen,
//! and the game keeps the right part of the screen, which is where the ECRAN
//! settings show their effect.

use super::super::*;
use super::icons::Icon;
use super::list::Row;
use super::text::HintItem;
use super::theme::{col, radius, size};

/// A side panel, as data.
pub(crate) struct Sheet<'a> {
    /// The small caption over the title, with its icon: where this panel is.
    pub kicker: Option<Icon>,
    pub crumbs: &'a [&'a str],
    pub title: &'a str,
    /// A line under the title ("Options de ce jeu").
    pub subtitle: Option<&'a str>,
    /// The game's cover beside the title: (texture, width, height, its
    /// blurred backdrop or 0), drawn by the cover rule (whole over its own
    /// blur when filling the box would crop too much).
    pub thumb: Option<(GLuint, u32, u32, GLuint)>,
    /// What stands in for a missing cover: the game's generated tile (its
    /// colour and name), as on the grid.
    pub thumb_tile: Option<(u32, &'a str)>,
    /// Status chips under the title: icon, text, icon colour.
    pub chips: &'a [(Icon, &'a str, u32)],
    pub rows: &'a [Row<'a>],
    /// Section titles: (index of the row they sit over, text).
    pub sections: &'a [(usize, &'a str)],
    /// The focused row, or `usize::MAX` for a list that is only read.
    pub selection: usize,
    pub row_h: f32,
    /// Glide key of the focus pill; also seeds the scroll's.
    pub glide_key: u32,
    pub help: &'a str,
    /// Keep room for the help box even on a row without help, so the rows do
    /// not move when the cursor goes from one to the other. False for a list
    /// that never has any (a read-only preview).
    pub reserve_help: bool,
    pub hints: &'a [HintItem<'a>],
    /// A before / after table in place of the rows: lines of
    /// `keymap::binding_diff_rows` (`"what\tbefore\tafter"`), under the three
    /// column titles of `diff_heads`.
    pub diff: &'a [std::string::String],
    pub diff_heads: [&'a str; 3],
}

impl<'a> Sheet<'a> {
    pub(crate) fn new(title: &'a str, rows: &'a [Row<'a>], selection: usize) -> Sheet<'a> {
        Sheet {
            kicker: None,
            crumbs: &[],
            title,
            subtitle: None,
            thumb: None,
            thumb_tile: None,
            chips: &[],
            rows,
            sections: &[],
            selection,
            row_h: 52.0,
            glide_key: 0,
            help: "",
            reserve_help: true,
            hints: &[],
            diff: &[],
            diff_heads: ["", "", ""],
        }
    }
}

/// Panel geometry. The in-game panel hugs the left edge (the game shows on the
/// right); the cascade's widths are chosen so the rail and the sub-panel
/// together still leave about 40% of the picture in view.
pub(crate) const SHEET_MARGIN: f32 = 16.0;
pub(crate) const SHEET_W: f32 = 440.0;
/// The parent folds to its icons alone: wide enough for a 20 px icon on its
/// pill, so the path stays readable (the page's caption spells it out) while
/// the game keeps nearly 60% of the screen for the ÉCRAN preview. With the
/// labels kept (288 px) the preview was down to 41%, a 4:3 game to 372 px.
pub(crate) const RAIL_W: f32 = 76.0;
pub(crate) const CHILD_W: f32 = 440.0;
pub(crate) const CASCADE_GAP: f32 = 10.0;
/// The launcher's options panel hugs the RIGHT edge, the library showing on the
/// left (Options.dc.html: 520 wide); its pages open to its left.
pub(crate) const OPTIONS_W: f32 = 520.0;
pub(crate) const OPTIONS_CHILD_W: f32 = 480.0;

/// Which edge a cascade hangs from.
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum Side {
    /// The pause: panels from the left, over the game.
    Left,
    /// A game's options in the launcher: panels from the right, over the library.
    Right,
}
/// The gap a section title takes over its rows.
const SECTION_H: f32 = 36.0;
/// Rows are this far apart beyond their own height.
const ROW_GAP: f32 = 2.0;
/// The help box always gets room for four lines, so the rows' area does not
/// change size (and the list does not jump) when the help under it grows.
/// Four, not three: a French help naming three options runs to a fourth line
/// in a 440 px panel, and its end was the part that got cut.
const HELP_LINES: usize = 4;
const HINTS_BAND: f32 = 60.0;

/// The room kept for the help box over the hints, with its gap.
fn help_room() -> f32 {
    28.0 + (size::LABEL * 1.45).round() * HELP_LINES as f32 + 10.0
}

/// The bottom of the pause's panels as last drawn (`kit_cascade`), for what
/// goes under them in portrait (the loading card).
static CASCADE_BOTTOM: std::sync::Mutex<f32> = std::sync::Mutex::new(0.0);

pub(crate) fn cascade_bottom() -> f32 {
    CASCADE_BOTTOM.lock().map(|g| *g).unwrap_or(0.0)
}

/// The cascade's progress, 0 (one panel) to 1 (rail plus sub-panel), eased on
/// its own clock.
struct Cascade {
    t: f32,
    tick: u64,
}

static CASCADE: std::sync::Mutex<Cascade> = std::sync::Mutex::new(Cascade { t: 0.0, tick: 0 });

/// How long the parent takes to fold and the child to come out, in
/// milliseconds: the panels' timing on the mockup's sheet ("fenêtres 180 ms").
const CASCADE_MS: f32 = 180.0;

/// Step the cascade toward open (`open`) or closed and return its eased
/// progress. Called once per frame by whatever draws the pause.
pub(crate) fn cascade_step(open: bool) -> f32 {
    let now = unsafe { ruffle_tick_now() };
    let freq = unsafe { ruffle_tick_freq() }.max(1) as f32;
    let Ok(mut c) = CASCADE.lock() else {
        return if open { 1.0 } else { 0.0 };
    };
    let dt = if c.tick == 0 { 0.0 } else { (now.saturating_sub(c.tick) as f32 * 1000.0 / freq).min(100.0) };
    c.tick = now;
    let step = dt / CASCADE_MS;
    c.t = if open { (c.t + step).min(1.0) } else { (c.t - step).max(0.0) };
    let t = c.t;
    // Ease out: quick to start, settling at the end.
    1.0 - (1.0 - t) * (1.0 - t) * (1.0 - t)
}

/// Snap the cascade shut (a fresh pause opens on the main panel alone).
pub(crate) fn cascade_reset() {
    if let Ok(mut c) = CASCADE.lock() {
        c.t = 0.0;
        c.tick = 0;
    }
}

/// The panel `kit_sheet_rail` drew last, for `kit_cascade_on` to make it the
/// way back for a finger: the target is the rail as drawn, whatever height it
/// was given. Taken from a height of its own, a rail shorter than the screen
/// (a panel that hugs its rows) left a strip under it that went back.
static RAIL_DRAWN: std::sync::Mutex<(f32, f32, f32, f32)> = std::sync::Mutex::new((0.0, 0.0, 0.0, 0.0));

impl SwitchRenderBackend {
    /// Height of the header for `s`, in the full panel. The rail uses the same,
    /// so its rows stay exactly where they were while it folds.
    fn sheet_header_h(&mut self, s: &Sheet) -> f32 {
        if s.thumb.is_some() {
            return 22.0 + 60.0 + 18.0;
        }
        let mut h = 22.0;
        if s.kicker.is_some() || !s.crumbs.is_empty() {
            h += 17.0 + 6.0;
        }
        h += 29.0;
        if s.subtitle.is_some() {
            h += 4.0 + 18.0;
        }
        if !s.chips.is_empty() {
            h += 12.0 + 28.0;
        }
        h + 16.0
    }

    /// Top and height of the panel for `s`. In landscape every panel is the
    /// screen's height. In portrait (a turned picture, 1280 tall) a panel that
    /// tall put the help box 600 px under the rows it explains, and its dim
    /// covered the lower half of the picture, where ÉCRAN shows what it does:
    /// there a panel hugs its content, from the top, never taller than the
    /// screen. The help box keeps its reserved lines, so a panel does not
    /// change height as the cursor moves.
    fn sheet_frame(&mut self, s: &Sheet) -> (f32, f32) {
        let (vw, vh) = (self.dimensions.width as f32, self.dimensions.height as f32);
        let full = vh - 2.0 * SHEET_MARGIN;
        if vh <= vw {
            return (SHEET_MARGIN, full);
        }
        let body = if s.diff.is_empty() { Self::sheet_layout(s).1 } else { SECTION_H + s.diff.len() as f32 * s.row_h };
        let help = if s.reserve_help { help_room() } else { 0.0 };
        let content = self.sheet_header_h(s) + 10.0 + body + help + HINTS_BAND;
        (SHEET_MARGIN, content.min(full).round())
    }

    /// The caption line: an icon and the path, the last step brighter.
    fn sheet_kicker(&mut self, x: f32, cy: f32, max_w: f32, s: &Sheet) {
        let px = size::CAPTION + 1.0;
        let mut cx = x;
        if let Some(ic) = s.kicker {
            self.kit_icon(cx, cy, 14.0, ic, col::TEXT3);
            cx += 14.0 + 8.0;
        }
        let end = x + max_w;
        let sep = "  \u{203A}  ";
        // A path too long for its line loses its first steps, never its last:
        // the last is where B goes back to, and on a page that deep the first
        // are the panel folded into the rail beside it (the pause, a game's
        // options). Cut at its end, the path of a profile's page read "Pausa ›
        // Controles y cursor › Perfiles comp…" in Spanish. Only a last step
        // too long on its own is still cut.
        let sep_w = self.kit_measure(sep, px);
        let mut first = 0;
        while first + 1 < s.crumbs.len() {
            let mut need = sep_w * (s.crumbs.len() - first - 1) as f32;
            for c in s.crumbs.iter().skip(first) {
                need += self.kit_measure(c, px);
            }
            if need <= end - cx {
                break;
            }
            first += 1;
        }
        for (i, c) in s.crumbs.iter().enumerate().skip(first) {
            if i > first {
                cx += self.kit_text_mid(cx, cy, px, sep, col::TEXT3);
            }
            let ink = if i + 1 == s.crumbs.len() { col::TEXT2 } else { col::TEXT3 };
            let t = self.kit_fit(c, px, (end - cx).max(0.0));
            cx += self.kit_text_mid(cx, cy, px, &t, ink);
        }
    }

    /// Draw the header of `s` in a panel at `x`, `w` wide. `chips` false keeps
    /// their room empty (the rail). Returns the y of the hairline under it.
    fn sheet_header(&mut self, s: &Sheet, x: f32, w: f32, chips: bool) -> f32 {
        let top = SHEET_MARGIN;
        let mut y = top + 22.0;
        let inner = w - 48.0;
        if let Some(thumb) = s.thumb {
            const TW: f32 = 96.0;
            const TH: f32 = 60.0;
            self.sheet_thumb(x + 24.0, y, TW, TH, thumb, s.thumb_tile);
            let tx = x + 24.0 + TW + 16.0;
            let tmax = x + w - 24.0 - tx;
            // A game's name: cut in the middle, where two episodes of a series
            // differ at the end.
            let t = self.kit_fit_mid(s.title, size::WINDOW_TITLE - 1.0, tmax);
            let has_sub = s.subtitle.is_some();
            let tcy = if has_sub { y + 20.0 } else { y + TH * 0.5 };
            self.kit_text_mid(tx, tcy, size::WINDOW_TITLE - 1.0, &t, col::TEXT);
            if let Some(sub) = s.subtitle {
                let st = self.kit_fit(sub, size::CAPTION + 1.0, tmax);
                self.kit_text_mid(tx, y + 44.0, size::CAPTION + 1.0, &st, col::TEXT3);
            }
            y += TH + 18.0;
        } else {
            if s.kicker.is_some() || !s.crumbs.is_empty() {
                self.sheet_kicker(x + 24.0, y + 8.5, inner, s);
                y += 17.0 + 6.0;
            }
            let t = self.kit_fit(s.title, size::WINDOW_TITLE + 2.0, inner);
            self.kit_text_mid(x + 24.0, y + 14.5, size::WINDOW_TITLE + 2.0, &t, col::TEXT);
            y += 29.0;
            if let Some(sub) = s.subtitle {
                let st = self.kit_fit(sub, size::CAPTION + 1.0, inner);
                self.kit_text_mid(x + 24.0, y + 4.0 + 9.0, size::CAPTION + 1.0, &st, col::TEXT3);
                y += 4.0 + 18.0;
            }
            if !s.chips.is_empty() {
                y += 12.0;
                if chips {
                    let mut cx = x + 24.0;
                    for (ic, text, ink) in s.chips {
                        let cw = self.kit_status_chip(cx, y + 14.0, *ic, text, *ink);
                        cx += cw + 8.0;
                    }
                }
                y += 28.0;
            }
            y += 16.0;
        }
        let line_y = y.round();
        self.draw_overlay_rect(x + 24.0, line_y, w - 48.0, 1.0, col::LINE);
        line_y
    }

    /// A panel's cover in a `w` x `h` box: the cover rule (filled when little
    /// is cropped, else whole over its blur), the generated tile when there is
    /// no cover yet.
    fn sheet_thumb(&mut self, x: f32, y: f32, w: f32, h: f32, thumb: (GLuint, u32, u32, GLuint), tile: Option<(u32, &str)>) {
        let (tex, tw, th, amb) = thumb;
        if tex != 0 && tw > 0 && th > 0 {
            self.kit_cover(x, y, w, h, tex, tw, th, amb, 1.0);
        } else if let Some((chip, name)) = tile {
            self.kit_default_tile(x, y, w, h, chip, name);
        } else {
            self.kit_rrect(x, y, w, h, 8.0, col::TILE);
        }
    }

    /// Where each row sits from the top of the rows' area, and the content's
    /// total height.
    fn sheet_layout(s: &Sheet) -> (std::vec::Vec<f32>, f32) {
        let mut ys = std::vec::Vec::with_capacity(s.rows.len());
        let mut y = 0.0;
        for i in 0..s.rows.len() {
            if s.sections.iter().any(|(at, _)| *at == i) {
                y += SECTION_H;
            }
            ys.push(y);
            y += s.row_h + ROW_GAP;
        }
        (ys, y)
    }

    /// The whole panel, at `x`, `w` wide, full height. Publishes its rows for
    /// touch when it has a cursor.
    pub(crate) fn kit_sheet_at(&mut self, s: &Sheet, x: f32, w: f32) {
        self.sheet_draw(s, x, w, true);
    }

    /// `kit_sheet_at`; `publish` false for a panel that is only being shown on
    /// its way somewhere (it must not take the taps of the page in front).
    fn sheet_draw(&mut self, s: &Sheet, x: f32, w: f32, publish: bool) {
        let (top, h) = self.sheet_frame(s);
        self.kit_panel(x, top, w, h, false);
        let line_y = self.sheet_header(s, x, w, true);

        // The help box and the hints, from the bottom up.
        let hints_cy = top + h - HINTS_BAND * 0.5;
        let help_bottom = top + h - HINTS_BAND;
        let help_w = w - 32.0;
        let help_room = if s.reserve_help { help_room() } else { 0.0 };
        if !s.help.trim().is_empty() {
            self.kit_help_box(x + 16.0, help_bottom, help_w, s.help, HELP_LINES);
        }
        // The bar fits the panel: a hint that would run past its edge (a
        // long translation, a row with four of them) is left out, from the
        // end of the bar but never the last one, the way back. Drawn whole,
        // the options' bar started 50 to 100 px left of the panel, over the
        // dimmed library.
        let mut hints: std::vec::Vec<HintItem> = s.hints.to_vec();
        while hints.len() > 1 && self.kit_hints_width(&hints) > w - 48.0 {
            hints.remove(hints.len() - 2);
        }
        self.kit_hints_right(x + w - 24.0, hints_cy, &hints);

        // A before / after table instead of rows.
        if !s.diff.is_empty() {
            let area_top = line_y + 10.0;
            let (rx, rw) = (x + 16.0, w - 32.0);
            self.kit_diff_heads(rx, area_top + SECTION_H - 14.0, rw, s.diff_heads);
            let mut y = area_top + SECTION_H;
            let bottom = help_bottom - help_room;
            for line in s.diff {
                if y + s.row_h > bottom + 1.0 {
                    break;
                }
                self.kit_diff_line(rx, y + s.row_h * 0.5, rw, line);
                y += s.row_h;
            }
            return;
        }

        // The rows.
        let area_top = line_y + 10.0;
        let area_bot = help_bottom - help_room.max(0.0);
        let area_h = (area_bot - area_top).max(s.row_h);
        let (ys, content_h) = Self::sheet_layout(s);
        let now = unsafe { ruffle_tick_now() };
        let max_off = (content_h - area_h).max(0.0);
        let target = if s.selection < s.rows.len() && max_off > 0.0 {
            (ys[s.selection] + s.row_h * 0.5 - area_h * 0.5).clamp(0.0, max_off)
        } else {
            0.0
        };
        let scroll = if max_off > 0.0 { eased_scroll_px(target, s.glide_key ^ 0x5000_0000, now) } else { 0.0 };
        let (rx, rw) = (x + 16.0, w - 32.0 - if max_off > 0.0 { 10.0 } else { 0.0 });
        let clip = max_off > 0.0;
        if clip {
            self.set_clip(x + 4.0, area_top, w - 8.0, area_bot - area_top);
        }
        for (at, text) in s.sections {
            if let Some(ry) = ys.get(*at) {
                let cy = area_top + ry - scroll - 6.0 - 8.0;
                self.kit_section(rx + 12.0, cy, text);
            }
        }
        if s.selection < s.rows.len() {
            let hy = eased_list_y(area_top + ys[s.selection] - scroll, s.glide_key, now);
            self.kit_focus_pill(rx, hy, rw, s.row_h);
        }
        let mut cells: std::vec::Vec<(f32, f32, f32, f32)> = std::vec![(0.0, 0.0, 0.0, 0.0); s.rows.len()];
        let mut segs: super::list::CtrlSegs = std::vec::Vec::new();
        for (i, row) in s.rows.iter().enumerate() {
            let y = area_top + ys[i] - scroll;
            if y + s.row_h < area_top - 1.0 || y > area_bot + 1.0 {
                continue;
            }
            let whole = y >= area_top - 1.0 && y + s.row_h <= area_bot + 1.0;
            if whole {
                cells[i] = (rx, y, rw, s.row_h);
            }
            let parts = self.kit_row_item(rx, y + s.row_h * 0.5, rw, row, i == s.selection);
            // A row half out of the band takes no tap, its options neither.
            if whole {
                segs.extend(parts.into_iter().map(|(p, r)| (i, p, r)));
            }
        }
        if clip {
            self.clear_clip();
            // Where the visible part sits in the whole list.
            let track_h = area_bot - area_top - 8.0;
            let thumb_h = (track_h * area_h / content_h).max(28.0).min(track_h);
            let ty = area_top + 4.0 + (track_h - thumb_h) * (scroll / max_off).clamp(0.0, 1.0);
            self.kit_rrect(x + w - 14.0, area_top + 4.0, 4.0, track_h, 2.0, col::LINE);
            self.kit_rrect(x + w - 14.0, ty, 4.0, thumb_h, 2.0, col::THUMB);
        }
        if publish && s.selection < s.rows.len() {
            ui_cells_publish(ui_screen_kind(), cells);
            ui_segs_publish(ui_screen_kind(), segs);
        }
    }

    /// The scroll the rows of `s` rest at with row `row` centred, and the top
    /// and bottom of their area, for a panel at full height: the rail draws its
    /// icons where the full panel had its rows.
    fn sheet_rows_frame(&mut self, s: &Sheet, row: usize) -> (f32, f32, f32) {
        let (top, h) = self.sheet_frame(s);
        let area_top = top + self.sheet_header_h(s) + 10.0;
        let help_room = if s.reserve_help { help_room() } else { 0.0 };
        let area_bot = top + h - HINTS_BAND - help_room;
        let area_h = (area_bot - area_top).max(s.row_h);
        let (ys, content_h) = Self::sheet_layout(s);
        let max_off = (content_h - area_h).max(0.0);
        let scroll = match ys.get(row) {
            Some(y) if max_off > 0.0 => (y + s.row_h * 0.5 - area_h * 0.5).clamp(0.0, max_off),
            _ => 0.0,
        };
        (scroll, area_top, area_bot)
    }

    /// Column titles over a diff table ("Touche", "Avant", "Après").
    pub(crate) fn kit_diff_heads(&mut self, x: f32, cy: f32, w: f32, heads: [&str; 3]) {
        let (c0, c1) = (w * 0.34, w * 0.33);
        self.kit_section(x + 12.0, cy, heads[0]);
        self.kit_section(x + c0, cy, heads[1]);
        self.kit_section(x + c0 + c1, cy, heads[2]);
    }

    /// One line of a before / after table (`keymap::binding_diff_rows`): the
    /// button drawn as the editor draws it, what it sends now, dim, and what it
    /// will send, bright. A line without tabs (a count of lines left out) is
    /// written across.
    ///
    /// A name that is no button (a cursor setting) may run into the first half
    /// of "Avant", pushing that value right: "Afficher le curseur" was cut to
    /// "Afficher le cu…" in a column sized for a glyph. A value that does not
    /// fit its column steps down a size, then goes on two lines, before
    /// anything is cut: "Comme en Normal", what a combo layer's empty button
    /// sends, ran out of both columns.
    pub(crate) fn kit_diff_line(&mut self, x: f32, cy: f32, w: f32, line: &str) {
        let px = size::LABEL + 1.0;
        let mut it = line.split('\t');
        let what = it.next().unwrap_or("");
        match (it.next(), it.next()) {
            (Some(before), Some(after)) => {
                let (c0, c1) = (w * 0.34, w * 0.33);
                let what_max = if Self::diff_what_is_button(what) { c0 - 20.0 } else { c0 + c1 * 0.5 - 20.0 };
                let what_end = self.kit_diff_what(x + 12.0, cy, what_max, what);
                let bx = (x + c0).max(what_end + 12.0);
                self.kit_diff_value(bx, cy, x + c0 + c1 - 14.0 - bx, before, col::TEXT3);
                self.kit_diff_value(x + c0 + c1, cy, w - c0 - c1 - 12.0, after, col::TEXT);
            }
            _ => {
                let t = self.kit_fit(what, px, w - 24.0);
                self.kit_text_mid(x + 12.0, cy, px, &t, col::TEXT3);
            }
        }
    }

    /// A value of a diff line from `x`, in `room`: at 16 px, else 13, else on
    /// two lines at 13 (the lines are 44 px tall), the second cut if it must.
    fn kit_diff_value(&mut self, x: f32, cy: f32, room: f32, text: &str, ink: u32) {
        let room = room.max(0.0);
        for p in [size::LABEL + 1.0, size::CAPTION] {
            if self.kit_measure(text, p) <= room {
                self.kit_text_mid(x, cy, p, text, ink);
                return;
            }
        }
        let p = size::CAPTION;
        let lines = self.kit_wrap(text, p, room, 2);
        let lh = (p * 1.25).round();
        let mut ly = cy - lh * (lines.len().max(1) - 1) as f32 * 0.5;
        for l in &lines {
            self.kit_text_mid(x, ly, p, l, ink);
            ly += lh;
        }
    }

    /// Whether a diff line's first column names a button (drawn as glyphs),
    /// rather than a setting written as words.
    fn diff_what_is_button(what: &str) -> bool {
        let rest = what.strip_prefix("P2 ").unwrap_or(what);
        let rest = rest.split_once('+').map_or(rest, |(_, b)| b);
        crate::keymap::PAD_SLOTS.iter().any(|s| s.name == rest)
    }

    /// The control a diff line is about: "P2 ZL+StickLUp" is a J2 chip, the ZL
    /// glyph, a plus and the left stick's badge. A name that is no button (a
    /// cursor setting) is written as it is, the cursor's speed after the
    /// pointer its row has. Returns where it ends.
    fn kit_diff_what(&mut self, x: f32, cy: f32, max_w: f32, what: &str) -> f32 {
        let px = size::LABEL;
        let mut cx = x;
        let mut rest = what;
        if let Some(r) = rest.strip_prefix("P2 ") {
            let p2 = crate::loc_ui::ui().p2;
            let tw = self.kit_measure(p2, size::CAPTION);
            self.kit_rrect(cx, cy - 11.0, tw + 14.0, 22.0, 6.0, col::PILL);
            self.kit_text_mid(cx + 7.0, cy, size::CAPTION, p2, col::PILL_INK);
            cx += tw + 14.0 + 8.0;
            rest = r;
        }
        let slot_glyph = |name: &str| crate::keymap::PAD_SLOTS.iter().find(|s| s.name == name).map(|s| s.glyph);
        if let Some((m, b)) = rest.split_once('+') {
            if let (Some(mg), Some(_)) = (slot_glyph(m), slot_glyph(b)) {
                let mw = self.pad_badge_width(mg, px);
                self.kit_pad_badge(cx, cy, mw, mg, px, col::TEXT2);
                cx += mw + 4.0;
                cx += self.kit_text_mid(cx, cy, px, "+", col::TEXT3) + 4.0;
                rest = b;
            }
        }
        match slot_glyph(rest) {
            Some(g) => {
                let bw = self.pad_badge_width(g, px);
                self.kit_pad_badge(cx, cy, bw, g, px, col::TEXT);
                cx + bw
            }
            None => {
                // The speed's name is one word in some languages ("Velocidad"):
                // alone under an empty column head, among button glyphs, it
                // did not say whose speed it was. The pointer says it, as on
                // the speed's row and in the editor's footer. "Afficher le
                // curseur" names the cursor already, and an icon before it
                // would leave "Masqué" 54 px of the in-game "Avant" column,
                // too few even at 13 px.
                if rest == crate::loc::s().set_cursor_speed {
                    self.kit_icon(cx, cy, 16.0, Icon::Pointer, col::TEXT2);
                    cx += 16.0 + 8.0;
                }
                let t = self.kit_fit(rest, px, (x + max_w - cx).max(0.0));
                cx + self.kit_text_mid(cx, cy, px, &t, col::TEXT2)
            }
        }
    }

    /// The parent folded to its icons, `w` wide: each row's icon where the row
    /// was, the one the page came from on a pill with a chevron pointing at the
    /// page. It is the path, and the page's caption spells it out; a tap on
    /// either goes back a level (`kit_cascade_on`), as does the direction
    /// pointing at it (input rule 3).
    pub(crate) fn kit_sheet_rail(&mut self, s: &Sheet, x: f32, w: f32, branch: usize, side: Side) {
        // The parent's own height: in portrait it hugs its rows, and the rail
        // keeps the panel it folds from.
        let (top, h) = self.sheet_frame(s);
        self.kit_panel(x, top, w, h, false);
        let cx = x + w * 0.5;
        // The head: the panel's own icon (the pause's), or its cover.
        if let Some(thumb) = s.thumb {
            let (tw_, th_) = ((w - 20.0).max(8.0), ((w - 20.0) * 0.625).max(5.0));
            self.sheet_thumb(cx - tw_ * 0.5, top + 22.0, tw_, th_, thumb, s.thumb_tile);
        } else if let Some(ic) = s.kicker {
            self.kit_icon(cx - 10.0, top + 22.0 + 8.5, 20.0, ic, col::TEXT3);
        }
        let line_y = (top + self.sheet_header_h(s)).round();
        self.draw_overlay_rect(x + 12.0, line_y, w - 24.0, 1.0, col::LINE);
        let (scroll, area_top, area_bot) = self.sheet_rows_frame(s, branch);
        let (ys, _) = Self::sheet_layout(s);
        self.set_clip(x, area_top, w, area_bot - area_top);
        for (i, row) in s.rows.iter().enumerate() {
            let y = area_top + ys[i] - scroll;
            if y + s.row_h < area_top || y > area_bot {
                continue;
            }
            let cy = y + s.row_h * 0.5;
            let on = i == branch;
            if on {
                self.kit_rrect(x + 10.0, y, w - 20.0, s.row_h, radius::ROW, col::CHIP);
                // The chevron points at the page, on the side it opened on.
                let (chev, chx) = if side == Side::Left {
                    (Icon::ChevronRight, x + w - 10.0 - 13.0)
                } else {
                    (Icon::ChevronLeft, x + 10.0 + 1.0)
                };
                self.kit_icon(chx, cy, 12.0, chev, col::GOLD);
            }
            let ink = if on {
                col::GOLD
            } else if row.danger {
                col::DANGER
            } else {
                col::TEXT3
            };
            if let Some(ic) = row.icon {
                self.kit_icon(cx - 10.0, cy, 20.0, ic, ink);
            }
        }
        self.clear_clip();
        // Where a tap on the rail lands (`kit_cascade_on`): this panel.
        if let Ok(mut g) = RAIL_DRAWN.lock() {
            *g = (x, top, w, h);
        }
    }

    /// The pause and its sub-panels, `t` of the way from the parent alone (0) to
    /// the rail plus `child` (1).
    pub(crate) fn kit_cascade(&mut self, parent: &Sheet, branch: usize, child: Option<&Sheet>, t: f32) {
        // Behind the panels the game is only lightly dimmed: it is the preview.
        // With a page open, only the panels' side is: the rest of the picture,
        // where ÉCRAN shows what it does, stays at full brightness, with a soft
        // edge between the two. In portrait the panels hug their rows at the
        // top, so the band stops under them too and the picture below stays
        // bright as well.
        //
        // The dim comes and goes with the panel's own slide: at full strength
        // on the first frame after Minus, it was there before the panel, and on
        // the way out it stayed until the slide had ended, then went in one
        // frame (`ui_sheet_reveal`).
        let e = if child.is_some() { t.clamp(0.0, 1.0) } else { 0.0 };
        const DIM: u32 = col::SCRIM_GAME;
        let a = (DIM >> 24) as f32 * self.ui_sheet_reveal.clamp(0.0, 1.0);
        let (vw, vh) = (self.dimensions.width as f32, self.dimensions.height as f32);
        let (_, parent_h) = self.sheet_frame(parent);
        let child_h = match child {
            Some(c) => self.sheet_frame(c).1,
            None => 0.0,
        };
        let bottom = SHEET_MARGIN + parent_h.max(child_h);
        if let Ok(mut g) = CASCADE_BOTTOM.lock() {
            *g = bottom;
        }
        if e < 1.0 {
            self.fill_screen_dim(((a * (1.0 - e)) as u32) << 24 | (DIM & 0x00FF_FFFF));
        }
        if e > 0.0 {
            let right = SHEET_MARGIN + RAIL_W + CASCADE_GAP + CHILD_W + 24.0;
            let band_bottom = if vh > vw { bottom + 24.0 } else { vh + 200.0 };
            self.dim_band(right, band_bottom, ((a * e) as u32) << 24 | (DIM & 0x00FF_FFFF));
        }
        self.kit_cascade_on(Side::Left, SHEET_W, CHILD_W, parent, branch, child, t);
    }

    /// Darken the screen from its top-left corner to `right` and `bottom`,
    /// fading out over the last 80 px, still whatever the panels are doing.
    fn dim_band(&mut self, right: f32, bottom: f32, rgba: u32) {
        let saved = (self.ui_scale, self.ui_pivot_x, self.ui_pivot_y, self.ui_translate_x, self.ui_translate_y);
        self.clear_ui_transform();
        self.kit_shadow(-200.0, -200.0, right + 160.0, bottom + 160.0, 0.0, 80.0, rgba);
        self.ui_scale = saved.0;
        self.ui_pivot_x = saved.1;
        self.ui_pivot_y = saved.2;
        self.ui_translate_x = saved.3;
        self.ui_translate_y = saved.4;
    }

    /// The same cascade from either edge, with no dim of its own: `full_w` is
    /// the parent's width alone, `child_w` the page's.
    ///
    /// On the way in, the parent's edge moves in towards the rail, cutting the
    /// panel down (its controls, hints and help go first, its icons last), and
    /// the page is uncovered from the rail outwards, at its final place: text
    /// that slides is text nobody can read until it stops. Halfway, the cut
    /// panel gives way to the rail. B plays it backwards.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn kit_cascade_on(
        &mut self,
        side: Side,
        full_w: f32,
        child_w: f32,
        parent: &Sheet,
        branch: usize,
        child: Option<&Sheet>,
        t: f32,
    ) {
        let vw = self.dimensions.width as f32;
        let vh = self.dimensions.height as f32;
        // Left edge of a panel `w` wide hung from `side`.
        let at = |w: f32| if side == Side::Left { SHEET_MARGIN } else { vw - SHEET_MARGIN - w };
        if t <= 0.0 || child.is_none() {
            self.kit_sheet_at(parent, at(full_w), full_w);
            return;
        }
        let e = t.clamp(0.0, 1.0);
        let pw = (full_w + (RAIL_W - full_w) * e).round();
        if let Some(c) = child {
            let cx = if side == Side::Left {
                SHEET_MARGIN + RAIL_W + CASCADE_GAP
            } else {
                vw - SHEET_MARGIN - RAIL_W - CASCADE_GAP - child_w
            };
            // Uncovered from the rail's side, its shadow included.
            let shown = ((child_w + 40.0) * e).round();
            let clip = if side == Side::Left {
                (cx - 12.0, 0.0, shown, vh)
            } else {
                (cx + child_w + 28.0 - shown, 0.0, shown, vh)
            };
            if e < 1.0 {
                self.set_clip_outer(Some(clip));
            }
            self.kit_sheet_at(c, cx, child_w);
            if e < 1.0 {
                self.set_clip_outer(None);
            }
        }
        let px = at(pw);
        if e < 0.5 {
            // The full panel, cut down by its moving edge (and its shadow with
            // it), drawn where it stood.
            self.set_clip_outer(Some((px - 30.0, 0.0, pw + 60.0, vh)));
            self.sheet_draw(parent, at(full_w), full_w, false);
            self.set_clip_outer(None);
            // The moving edge itself, so the cut reads as the panel's border.
            let ex = if side == Side::Left { px + pw - 1.0 } else { px };
            let (ptop, ph) = self.sheet_frame(parent);
            self.draw_overlay_rect(ex, ptop + 20.0, 1.0, ph - 40.0, col::PANEL_LINE);
        } else {
            self.kit_sheet_rail(parent, px, pw, branch, side);
        }
        // Settled, the rail and the page's "‹ Parent" caption take a finger
        // back a level, as B does: both look like the way back (input rule
        // 6). Published after the page's own rows, which clear the table.
        if let Some(c) = child.filter(|_| e >= 1.0) {
            let cx = if side == Side::Left {
                SHEET_MARGIN + RAIL_W + CASCADE_GAP
            } else {
                vw - SHEET_MARGIN - RAIL_W - CASCADE_GAP - child_w
            };
            // The rail just drawn above (settled, the cascade draws the rail,
            // never the cut panel), as `kit_sheet_rail` placed it.
            let rail = RAIL_DRAWN.lock().map(|g| *g).unwrap_or((px, SHEET_MARGIN, pw, vh - 2.0 * SHEET_MARGIN));
            let mut back = std::vec![rail];
            if c.thumb.is_none() && (c.kicker.is_some() || !c.crumbs.is_empty()) {
                back.push((cx + 16.0, SHEET_MARGIN + 12.0, child_w - 32.0, 37.0));
            }
            ui_back_publish(ui_screen_kind(), back);
        }
    }

    /// The page behind a panel, darkened (the scrim of a window).
    pub(crate) fn kit_scrim(&mut self, rgba: u32) {
        self.fill_screen_dim(rgba);
    }
}
