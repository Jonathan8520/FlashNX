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

    /// One list row centred on `cy`, between `x` and `x + w`: the label at the
    /// left, the value (if any) right-aligned. The focus pill is drawn by the
    /// caller, so it can glide between rows.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn kit_row(&mut self, x: f32, cy: f32, w: f32, label: &str, value: Option<&str>, focused: bool, px: f32) {
        const PAD: f32 = 20.0;
        let ink = if focused { col::TEXT } else { col::TEXT2 };
        let mut avail = w - 2.0 * PAD;
        if let Some(v) = value {
            let vw_max = (avail * 0.45).max(60.0);
            let v = self.kit_fit(v, px, vw_max);
            let vw = self.kit_measure(&v, px);
            let vink = if focused { col::GOLD } else { col::TEXT3 };
            self.kit_text_mid(x + w - PAD - vw, cy, px, &v, vink);
            avail -= vw + 16.0;
        }
        let l = self.kit_fit(label, px, avail);
        self.kit_text_mid(x + PAD, cy, px, &l, ink);
    }
}
