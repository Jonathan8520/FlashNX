//! What sits on the right of a settings row, and the small parts around rows.
//!
//! A setting used to be a line of text, "Overclock : Auto", changed by pressing
//! A until the wanted value came round. The mockup shows each kind of setting
//! as what it is: a choice among a few shows all of them with the current one
//! lit (Left/Right move it), a range or a long list shows its value between two
//! arrows, an on/off setting is a switch. Same parts in the launcher and in the
//! game, so a setting looks the same wherever it is changed.

use super::super::*;
use super::icons::Icon;
use super::theme::{col, radius, size};

/// What a row shows on its right.
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum Ctrl<'a> {
    None,
    /// A value to read: a folder, a count, a version.
    Value(&'a str),
    /// A value stepped with Left / Right (and A): drawn between two arrows.
    Cycle(&'a str),
    /// One of a few options, all shown, the current one lit.
    Choice(&'a [&'a str], usize),
    /// On or off.
    Toggle(bool),
}

const CHOICE_H: f32 = 32.0;
const CHOICE_PAD: f32 = 12.0;
const CHOICE_INSET: f32 = 3.0;
const CHOICE_PX: f32 = 14.0;
const TOGGLE_W: f32 = 44.0;
const TOGGLE_H: f32 = 26.0;
const ARROW: f32 = 14.0;
const CYCLE_MIN: f32 = 64.0;

impl SwitchRenderBackend {
    fn kit_choice_width(&mut self, opts: &[&str]) -> f32 {
        let mut w = 2.0 * CHOICE_INSET;
        for o in opts {
            w += self.kit_measure(o, CHOICE_PX) + 2.0 * CHOICE_PAD;
        }
        w
    }

    /// Width `ctrl` will take (at most `max_w`: a choice that would not fit is
    /// drawn as its current option between arrows).
    pub(crate) fn kit_ctrl_width(&mut self, ctrl: &Ctrl, max_w: f32) -> f32 {
        match *ctrl {
            Ctrl::None => 0.0,
            Ctrl::Value(v) => self.kit_measure(v, size::LABEL + 1.0).min(max_w),
            Ctrl::Cycle(v) => (self.kit_measure(v, size::LABEL + 1.0).max(CYCLE_MIN) + 2.0 * (ARROW + 10.0)).min(max_w),
            Ctrl::Choice(opts, active) => {
                let w = self.kit_choice_width(opts);
                if w <= max_w {
                    w
                } else {
                    let v = opts.get(active).copied().unwrap_or("");
                    self.kit_ctrl_width(&Ctrl::Cycle(v), max_w)
                }
            }
            Ctrl::Toggle(_) => TOGGLE_W,
        }
    }

    /// Draw `ctrl` ending at `right`, centred on `cy`, in at most `max_w`;
    /// returns the width it took. `focused` lights what Left / Right act on.
    pub(crate) fn kit_ctrl(&mut self, right: f32, cy: f32, ctrl: &Ctrl, focused: bool, max_w: f32) -> f32 {
        match *ctrl {
            Ctrl::None => 0.0,
            Ctrl::Value(v) => {
                let px = size::LABEL + 1.0;
                let v = self.kit_fit_mid(v, px, max_w);
                let w = self.kit_measure(&v, px);
                self.kit_text_mid((right - w).round(), cy, px, &v, if focused { col::GOLD } else { col::TEXT3 });
                w
            }
            Ctrl::Cycle(v) => {
                let px = size::LABEL + 1.0;
                let inner_max = (max_w - 2.0 * (ARROW + 10.0)).max(20.0);
                let v = self.kit_fit_mid(v, px, inner_max);
                let vw = self.kit_measure(&v, px);
                let inner = vw.max(CYCLE_MIN).min(inner_max);
                let total = inner + 2.0 * (ARROW + 10.0);
                let x0 = right - total;
                let arrow_ink = if focused { col::GOLD } else { 0xFF_5A_6A_8F };
                self.kit_icon(x0, cy, ARROW, Icon::ChevronLeft, arrow_ink);
                let vx = x0 + ARROW + 10.0 + (inner - vw) * 0.5;
                self.kit_text_mid(vx.round(), cy, px, &v, if focused { col::TEXT } else { col::TEXT2 });
                self.kit_icon(right - ARROW, cy, ARROW, Icon::ChevronRight, arrow_ink);
                total
            }
            Ctrl::Choice(opts, active) => {
                let total = self.kit_choice_width(opts);
                if total > max_w {
                    let v = opts.get(active).copied().unwrap_or("");
                    return self.kit_ctrl(right, cy, &Ctrl::Cycle(v), focused, max_w);
                }
                let x0 = (right - total).round();
                let y0 = (cy - CHOICE_H * 0.5).round();
                self.kit_rrect(x0, y0, total, CHOICE_H, 10.0, 0xFF_10_19_2E);
                let mut sx = x0 + CHOICE_INSET;
                for (i, o) in opts.iter().enumerate() {
                    let ow = self.kit_measure(o, CHOICE_PX);
                    let segw = ow + 2.0 * CHOICE_PAD;
                    let ink = if i == active {
                        // The current option: gold when the row is focused,
                        // a quieter lit chip when it is not.
                        let fill = if focused { col::GOLD } else { col::PILL };
                        self.kit_rrect(sx, y0 + CHOICE_INSET, segw, CHOICE_H - 2.0 * CHOICE_INSET, 8.0, fill);
                        if focused { 0xFF_1A_14_05 } else { col::TEXT }
                    } else if focused {
                        col::TEXT2
                    } else {
                        col::TEXT3
                    };
                    self.kit_text_mid((sx + CHOICE_PAD).round(), cy, CHOICE_PX, o, ink);
                    sx += segw;
                }
                total
            }
            Ctrl::Toggle(on) => {
                let x0 = (right - TOGGLE_W).round();
                let y0 = (cy - TOGGLE_H * 0.5).round();
                let track = if on { col::GOLD } else { col::PILL };
                self.kit_rrect(x0, y0, TOGGLE_W, TOGGLE_H, TOGGLE_H * 0.5, track);
                let knob = TOGGLE_H - 6.0;
                let kx = if on { x0 + TOGGLE_W - 3.0 - knob } else { x0 + 3.0 };
                self.kit_rrect(kx, y0 + 3.0, knob, knob, knob * 0.5, if on { 0xFF_FF_FF_FF } else { col::TEXT2 });
                TOGGLE_W
            }
        }
    }

    /// Width of a chip (see `kit_chip`).
    pub(crate) fn kit_chip_width(&mut self, icon: Option<Icon>, label: &str, count: Option<&str>) -> f32 {
        let px = size::LABEL;
        let mut w = 14.0 + self.kit_measure(label, px) + 14.0;
        if icon.is_some() {
            w += 16.0 + 8.0;
        }
        if let Some(c) = count {
            w += 6.0 + self.kit_measure(c, px);
        }
        w
    }

    /// A pill with an icon, a label and a count, for a row of folders;
    /// `active` fills it. Returns its width.
    pub(crate) fn kit_chip(&mut self, x: f32, cy: f32, icon: Option<Icon>, label: &str, count: Option<&str>, active: bool) -> f32 {
        const H: f32 = 34.0;
        let px = size::LABEL;
        let w = self.kit_chip_width(icon, label, count);
        if active {
            self.kit_rrect(x, (cy - H * 0.5).round(), w, H, H * 0.5, col::CHIP);
        }
        let ink = if active { col::TEXT } else { col::TEXT2 };
        let mut pen = x + 14.0;
        if let Some(ic) = icon {
            self.kit_icon(pen, cy, 16.0, ic, if active { col::GOLD } else { col::TEXT3 });
            pen += 16.0 + 8.0;
        }
        pen += self.kit_text_mid(pen, cy, px, label, ink);
        if let Some(c) = count {
            self.kit_text_mid(pen + 6.0, cy, px, c, col::TEXT3);
        }
        w
    }

    /// A small flat chip with an icon and a fact ("33 images/s"). Returns its
    /// width.
    pub(crate) fn kit_status_chip(&mut self, x: f32, cy: f32, icon: Icon, text: &str, ink: u32) -> f32 {
        const H: f32 = 28.0;
        let px = size::CAPTION;
        let w = 10.0 + 14.0 + 6.0 + self.kit_measure(text, px) + 10.0;
        self.kit_rrect(x, (cy - H * 0.5).round(), w, H, 8.0, col::TILE);
        self.kit_icon(x + 10.0, cy, 14.0, icon, ink);
        self.kit_text_mid(x + 10.0 + 14.0 + 6.0, cy, px, text, col::TEXT2);
        w
    }

    /// A section title over a group of rows.
    pub(crate) fn kit_section(&mut self, x: f32, cy: f32, text: &str) {
        self.kit_text_mid(x, cy, size::CAPTION + 1.0, text, col::TEXT3);
    }

    /// Height `kit_help_box` takes for `text` in `w`.
    pub(crate) fn kit_help_box_height(&mut self, text: &str, w: f32, max_lines: usize) -> f32 {
        let lines = self.kit_wrap(text, size::LABEL, w - 16.0 - 18.0 - 12.0 - 16.0, max_lines);
        let lh = (size::LABEL * 1.45).round();
        28.0 + lh * lines.len().max(1) as f32
    }

    /// The box explaining the focused row: an info icon and the text, in a
    /// tile-coloured box whose BOTTOM edge is at `bottom`. Returns its height.
    pub(crate) fn kit_help_box(&mut self, x: f32, bottom: f32, w: f32, text: &str, max_lines: usize) -> f32 {
        if text.trim().is_empty() {
            return 0.0;
        }
        let lines = self.kit_wrap(text, size::LABEL, w - 16.0 - 18.0 - 12.0 - 16.0, max_lines);
        let lh = (size::LABEL * 1.45).round();
        let h = 28.0 + lh * lines.len().max(1) as f32;
        let y = bottom - h;
        self.kit_rrect(x, y, w, h, radius::ROW, col::TILE);
        self.kit_icon(x + 16.0, y + 14.0 + lh * 0.5, 18.0, Icon::Info, col::TEXT2);
        let mut ly = y + 14.0 + lh * 0.5;
        for line in &lines {
            self.kit_text_mid(x + 16.0 + 18.0 + 12.0, ly, size::LABEL, line, col::TEXT2);
            ly += lh;
        }
        h
    }
}
