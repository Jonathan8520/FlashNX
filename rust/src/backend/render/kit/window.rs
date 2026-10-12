//! Windows and the lists inside them: one panel, one row, one focus mark, one
//! hint bar for every modal in the launcher and in game.
//!
//! The rows used to be drawn by hand in five places (`draw_modal_rows`, the
//! folder picker, the profile list, the language picker...), each with its own
//! amber wash and its own `>` cursor, and the copies had drifted apart. A row is
//! now `kit_row`: the label on the left, its current value on the right, and
//! the focused one on a raised pill with a gold edge.

use super::super::*;
use super::text::{Btn, HintItem};
use super::theme::{col, radius};

/// A row's label and, for a "LABEL: VALUE" row, its value.
///
/// Settings rows have always been built as one string with a colon in it
/// ("Affichage : Étirer"), at a dozen call sites. Splitting it here puts every
/// value in the same column without touching those sites. Only a short label
/// is split, so a game title with a colon in it ("Scooby-Doo: Mayan...") stays
/// whole.
pub(crate) fn split_label_value(row: &str) -> (&str, Option<&str>) {
    for sep in [" : ", ": "] {
        if let Some(pos) = row.find(sep) {
            let (label, value) = (row[..pos].trim(), row[pos + sep.len()..].trim());
            if !label.is_empty() && !value.is_empty() && label.chars().count() <= 28 && !label.contains('/') {
                return (label, Some(value));
            }
        }
    }
    (row, None)
}

fn key_token(t: &str) -> Option<Option<Btn>> {
    // Some(Some(b)) = a button, Some(None) = a direction word (dropped),
    // None = not a key at all.
    let up = t.trim().to_uppercase();
    let b = match up.as_str() {
        "A" => Btn::A,
        "B" => Btn::B,
        "X" => Btn::X,
        "Y" => Btn::Y,
        "L" => Btn::L,
        "R" => Btn::R,
        "ZL" => Btn::ZL,
        "ZR" => Btn::ZR,
        "+" => Btn::Plus,
        "-" | "\u{2212}" => Btn::Minus,
        "L3" | "LS" => Btn::LStick,
        "R3" | "RS" => Btn::RStick,
        _ => {
            const DIRECTIONS: &[&str] = &[
                "UP", "DOWN", "HAUT", "BAS", "ARRIBA", "ABAJO", "ВВЕРХ", "ВНИЗ", "HOCH",
                "RUNTER", "SU", "GIÙ", "CIMA", "BAIXO", "YUKARI", "AŞAĞI", "YUK", "AŞA", "上", "下",
            ];
            return if DIRECTIONS.contains(&up.as_str()) { Some(None) } else { None };
        }
    };
    Some(Some(b))
}

/// The old one-line footers ("A:OK   B:RETOUR   HAUT/BAS:NAV") as hints.
///
/// Every screen not yet rebuilt still passes one of those strings, in nine
/// languages. Read as data, they give the same "glyph + verb" bar the rebuilt
/// screens have, with no string table to rewrite first. Up/down hints are
/// dropped: moving a cursor with the stick needs no explaining.
pub(crate) fn parse_footer(text: &str) -> std::vec::Vec<(std::vec::Vec<Btn>, std::string::String)> {
    let mut out = std::vec::Vec::new();
    let mut segments: std::vec::Vec<&str> = std::vec::Vec::new();
    // Segments are separated by two spaces or more.
    let mut start = 0;
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b' ' && i + 1 < bytes.len() && bytes[i + 1] == b' ' {
            segments.push(&text[start..i]);
            while i < bytes.len() && bytes[i] == b' ' {
                i += 1;
            }
            start = i;
            continue;
        }
        i += 1;
    }
    segments.push(&text[start..]);
    for seg in segments {
        let seg = seg.trim();
        if seg.is_empty() {
            continue;
        }
        let Some(colon) = seg.find(':') else {
            out.push((std::vec::Vec::new(), seg.to_string()));
            continue;
        };
        let (key, label) = (seg[..colon].trim(), seg[colon + 1..].trim());
        let tokens: std::vec::Vec<&str> = if key == "+" || key == "-" || key == "\u{2212}" {
            std::vec![key]
        } else {
            key.split(['/', '+']).map(|t| t.trim()).filter(|t| !t.is_empty()).collect()
        };
        let parsed: std::vec::Vec<Option<Option<Btn>>> = tokens.iter().map(|t| key_token(t)).collect();
        if parsed.is_empty() || parsed.iter().any(|p| p.is_none()) {
            out.push((std::vec::Vec::new(), seg.to_string()));
        } else if parsed.iter().all(|p| matches!(p, Some(None))) {
            // Only directions: a navigation hint, dropped.
        } else {
            let btns = parsed.into_iter().flatten().flatten().collect();
            let mut label = label.to_string();
            if let Some(first) = label.chars().next() {
                let up: std::string::String = first.to_uppercase().collect();
                label.replace_range(..first.len_utf8(), &up);
            }
            out.push((btns, label));
        }
    }
    out
}

impl SwitchRenderBackend {
    /// Draw parsed footer hints: `centered` on `cx`, else ending at `right`.
    pub(crate) fn kit_footer_hints(&mut self, text: &str, cx_or_right: f32, cy: f32, centered: bool) {
        let parsed = parse_footer(text);
        let items: std::vec::Vec<HintItem> = parsed.iter().map(|(b, l)| (b.as_slice(), l.as_str())).collect();
        if centered {
            self.kit_hints_centered(cx_or_right, cy, &items);
        } else {
            self.kit_hints_right(cx_or_right, cy, &items);
        }
    }

    /// A raised panel: soft shadow, rounded body, a one-pixel edge.
    pub(crate) fn kit_panel(&mut self, x: f32, y: f32, w: f32, h: f32, danger: bool) {
        let (fill, line) = if danger { (col::DANGER_PANEL, col::DANGER_LINE) } else { (col::PANEL, col::PANEL_LINE) };
        self.kit_shadow(x, y + 10.0, w, h, radius::PANEL, 26.0, col::SHADOW);
        self.kit_shape(x, y, w, h, radius::PANEL, fill, 1.0, line, 0.0);
    }

    /// The focus mark of a list row: a raised pill with a gold edge.
    pub(crate) fn kit_focus_pill(&mut self, x: f32, y: f32, w: f32, h: f32) {
        self.kit_shape(x, y, w, h, radius::ROW, col::CHIP, 2.0, col::GOLD, 0.0);
    }

    /// Width of a segmented control with these labels at `px`.
    pub(crate) fn kit_segmented_width(&mut self, labels: &[&str], px: f32) -> f32 {
        const PAD_X: f32 = 16.0;
        const INSET: f32 = 3.0;
        labels.iter().map(|l| self.kit_measure(l, px) + 2.0 * PAD_X).sum::<f32>() + 2.0 * INSET
    }

    /// A segmented control (one track, the active segment filled gold) with
    /// its left edge at `x`, centred on `cy`. Returns its width.
    pub(crate) fn kit_segmented(&mut self, x: f32, cy: f32, labels: &[&str], active: usize, px: f32) -> f32 {
        const PAD_X: f32 = 16.0;
        const INSET: f32 = 3.0;
        const H: f32 = 36.0;
        let total = self.kit_segmented_width(labels, px);
        let y = (cy - H * 0.5).round();
        self.kit_rrect(x, y, total, H, H * 0.5, 0xFF_0E_17_2A);
        let mut sx = x + INSET;
        for (i, lbl) in labels.iter().enumerate() {
            let lw = self.kit_measure(lbl, px);
            let w = lw + 2.0 * PAD_X;
            let ink = if i == active {
                self.kit_rrect(sx, y + INSET, w, H - 2.0 * INSET, (H - 2.0 * INSET) * 0.5, col::GOLD);
                0xFF_1A_14_05
            } else {
                col::TEXT2
            };
            self.kit_text_mid(sx + PAD_X, cy, px, lbl, ink);
            sx += w;
        }
        total
    }

    /// The in-game sheet: a panel down the left side of the screen, over the
    /// frozen game, which stays visible on the right (the ECRAN rows preview on
    /// it). From the top: where you are ("Pause › Écran"), the game's name, the
    /// rows, a box explaining the row under the cursor, and the hints, whose B
    /// says where it goes. Every in-game page is this same panel, so moving
    /// between them reads as moving through one menu, not a stack of popups.
    ///
    /// `steppers` has bit `i` set when row `i`'s value also steps with Left and
    /// Right; the focused one then shows an arrow either side of its value.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn kit_side_sheet(
        &mut self,
        crumbs: &[&str],
        title: &str,
        rows: &[&str],
        selection: usize,
        help: Option<&str>,
        hints: &[HintItem],
        glide_key: u32,
        steppers: u32,
    ) {
        use super::theme::size;
        const ROW_H: f32 = 52.0;
        const PILL_H: f32 = 46.0;
        let vw = self.dimensions.width as f32;
        let vh = self.dimensions.height as f32;
        // Behind the sheet the game is only lightly dimmed: it is the preview.
        self.fill_screen_dim(0x70_05_0A_16);
        let (x, y) = (16.0, 16.0);
        let w = 460.0f32.min(vw - 32.0);
        let h = vh - 32.0;
        self.kit_panel(x, y, w, h, false);

        // Where you are.
        let mut cx = x + 28.0;
        for (i, c) in crumbs.iter().enumerate() {
            if i > 0 {
                cx += self.kit_text_mid(cx, y + 40.0, size::CAPTION + 1.0, "  \u{203A}  ", col::TEXT3);
            }
            let ink = if i + 1 == crumbs.len() { col::TEXT2 } else { col::TEXT3 };
            cx += self.kit_text_mid(cx, y + 40.0, size::CAPTION + 1.0, c, ink);
        }
        let t = self.kit_fit_mid(title, size::WINDOW_TITLE, w - 56.0);
        self.kit_text_mid(x + 28.0, y + 74.0, size::WINDOW_TITLE, &t, col::TEXT);
        self.draw_overlay_rect(x + 20.0, y + 100.0, w - 40.0, 1.0, col::LINE);

        // The rows, with the focus pill gliding between them.
        let top = y + 116.0;
        let (px, pw) = (x + 12.0, w - 24.0);
        if selection < rows.len() {
            let now = unsafe { ruffle_tick_now() };
            let hy = eased_list_y(top + selection as f32 * ROW_H, glide_key, now);
            self.kit_focus_pill(px, hy + (ROW_H - PILL_H) * 0.5, pw, PILL_H);
        }
        for (i, row) in rows.iter().enumerate() {
            let cy = top + i as f32 * ROW_H + ROW_H * 0.5;
            let (label, value) = split_label_value(row);
            let stepper = i < 32 && steppers & (1 << i) != 0;
            self.kit_row(px, cy, pw, label, value, i == selection, size::ROW, stepper);
        }

        // What the row under the cursor does, always in the same place.
        let hint_cy = y + h - 30.0;
        if let Some(help) = help.filter(|s| !s.trim().is_empty()) {
            let lines = self.kit_wrap(help, size::LABEL, w - 72.0, 3);
            let lh = (size::LABEL * 1.35).round();
            let box_h = 28.0 + lh * lines.len() as f32;
            let box_y = hint_cy - 30.0 - box_h;
            self.kit_rrect(x + 16.0, box_y, w - 32.0, box_h, radius::ROW, col::CHIP & 0x00FF_FFFF | 0xB0 << 24);
            let mut ly = box_y + 14.0 + lh * 0.5;
            for line in &lines {
                self.kit_text_mid(x + 36.0, ly, size::LABEL, line, col::TEXT2);
                ly += lh;
            }
        }
        self.draw_overlay_rect(x + 20.0, hint_cy - 26.0, w - 40.0, 1.0, col::LINE);
        self.kit_hints_right(x + w - 24.0, hint_cy, hints);
    }

    /// One list row centred on `cy`, between `x` and `x + w`: the label at the
    /// left, the value (if any) right-aligned. The focus pill is drawn by the
    /// caller, so it can glide between rows.
    ///
    /// A `stepper` row's value also changes with Left and Right. The arrows that
    /// say so are drawn on the focused row only, but their room is kept on
    /// every stepper row, so the values stay in one column as the focus moves.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn kit_row(
        &mut self,
        x: f32,
        cy: f32,
        w: f32,
        label: &str,
        value: Option<&str>,
        focused: bool,
        px: f32,
        stepper: bool,
    ) {
        use super::icons::Icon;
        const PAD: f32 = 20.0;
        const ARROW: f32 = 16.0;
        const ARROW_GAP: f32 = 6.0;
        let ink = if focused { col::TEXT } else { col::TEXT2 };
        let mut avail = w - 2.0 * PAD;
        if let Some(v) = value {
            let mut right = x + w - PAD;
            if stepper {
                if focused {
                    self.kit_icon(right - ARROW, cy, ARROW, Icon::ChevronRight, col::GOLD);
                }
                right -= ARROW + ARROW_GAP;
                avail -= ARROW + ARROW_GAP;
            }
            let vw_max = (avail * 0.45).max(60.0);
            let v = self.kit_fit(v, px, vw_max);
            let vw = self.kit_measure(&v, px);
            let vink = if focused { col::GOLD } else { col::TEXT3 };
            self.kit_text_mid(right - vw, cy, px, &v, vink);
            avail -= vw + 16.0;
            if stepper {
                if focused {
                    self.kit_icon(right - vw - ARROW_GAP - ARROW, cy, ARROW, Icon::ChevronLeft, col::GOLD);
                }
                avail -= ARROW + ARROW_GAP;
            }
        }
        let l = self.kit_fit(label, px, avail);
        self.kit_text_mid(x + PAD, cy, px, &l, ink);
    }
}

impl SwitchRenderBackend {
    /// The head of a full page that is not a tab: where it sits ("Réglages ›
    /// Signaler un bug"), its title, and a line saying what to do here. Returns
    /// the y where the page's content can start.
    pub(crate) fn kit_page_header(&mut self, crumbs: &[&str], title: &str, subtitle: Option<&str>) -> f32 {
        use super::theme::size;
        let vw = self.dimensions.width as f32;
        let x = 48.0;
        let mut cx = x;
        for (i, c) in crumbs.iter().enumerate() {
            if i > 0 {
                cx += self.kit_text_mid(cx, 34.0, size::CAPTION + 1.0, "  \u{203A}  ", col::TEXT3);
            }
            let ink = if i + 1 == crumbs.len() { col::TEXT2 } else { col::TEXT3 };
            cx += self.kit_text_mid(cx, 34.0, size::CAPTION + 1.0, c, ink);
        }
        let t = self.kit_fit(title, size::TITLE, vw - 2.0 * x);
        self.kit_text_mid(x, 70.0, size::TITLE, &t, col::TEXT);
        match subtitle {
            Some(sub) => {
                let s = self.kit_fit(sub, size::LABEL + 1.0, vw - 2.0 * x);
                self.kit_text_mid(x, 102.0, size::LABEL + 1.0, &s, col::TEXT3);
                126.0
            }
            None => 100.0,
        }
    }
}
