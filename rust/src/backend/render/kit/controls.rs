//! What sits on the right of a settings row, and the small parts around rows.
//!
//! A setting used to be a line of text, "Overclock : Auto", changed by pressing
//! A until the wanted value came round. The mockup shows each kind of setting
//! as what it is: a choice among a few shows all of them with the current one
//! lit, a range or a long list shows its value, an on/off setting is a switch.
//! Same parts in the launcher and in the game, so a setting looks the same
//! wherever it is changed.
//!
//! A choice or a range changes only once A has armed its row (input rule 1):
//! a stick pushed sideways on the way down a list must not change what it
//! passes. The armed control is outlined in gold with an arrow on each side
//! (`kit_ctrl_parts`), and only then do Left and Right move it.

use super::super::*;
use super::icons::Icon;
use super::theme::{col, radius, size};

/// What a row shows on its right.
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum Ctrl<'a> {
    None,
    /// A value to read: a folder, a count, a version.
    Value(&'a str),
    /// A value stepped with Left / Right once A has armed its row: drawn
    /// alone at rest, between two gold arrows while armed.
    Cycle(&'a str),
    /// One of a few options, all shown, the current one lit.
    Choice(&'a [&'a str], usize),
    /// On or off.
    Toggle(bool),
}

const CHOICE_H: f32 = 32.0;
/// Padding either side of an option and the options' text size, tried in turn
/// before a choice gives up and shows its current option alone (its arrows
/// come only once the row is armed): the usual one, a tighter one, then
/// tighter again a size down. The last is the final step before that
/// fallback. Measured with the console's Latin widths, the
/// settings' choices fit by the second; the third serves the sort windows,
/// whose rows hold two whole orders and room for a tick, and leaves a few
/// pixels' margin for a choice that misses on the console's own font (the
/// Russian ÉCRAN is the closest call).
const CHOICE_TIERS: [(f32, f32); 3] = [(12.0, 14.0), (7.0, 14.0), (4.0, 13.0)];
const CHOICE_INSET: f32 = 3.0;
const TOGGLE_W: f32 = 44.0;
const TOGGLE_H: f32 = 26.0;
/// Room either side of a cycle's value: the box the armed outline draws.
const CYCLE_PAD: f32 = 12.0;
/// The arrows beside an armed control, and their gap from it.
const ARM_ARROW: f32 = 12.0;
const ARM_GAP: f32 = 5.0;
/// An armed arrow at the end of its range: the gold, faded.
const ARM_ARROW_DIM: u32 = 0x59_FF_D7_40;

/// The options of the last choice `kit_ctrl` drew whole, left to right, for
/// `kit_ctrl_parts` to hand on as tap targets and to outline. Filled while
/// the options are laid out, so the targets are exactly where they are drawn.
static CHOICE_SEGS: std::sync::Mutex<std::vec::Vec<(f32, f32, f32, f32)>> =
    std::sync::Mutex::new(std::vec::Vec::new());

fn choice_seg_push(r: (f32, f32, f32, f32)) {
    if let Ok(mut g) = CHOICE_SEGS.lock() {
        g.push(r);
    }
}

impl SwitchRenderBackend {
    fn kit_choice_width(&mut self, opts: &[&str], pad: f32, px: f32) -> f32 {
        let mut w = 2.0 * CHOICE_INSET;
        for o in opts {
            w += self.kit_measure(o, px) + 2.0 * pad;
        }
        w
    }

    /// The padding and text size a choice is drawn with in `max_w`, or None
    /// when even the tightest does not fit.
    fn kit_choice_pad(&mut self, opts: &[&str], max_w: f32) -> Option<(f32, f32)> {
        CHOICE_TIERS.into_iter().find(|&(pad, px)| self.kit_choice_width(opts, pad, px) <= max_w)
    }

    /// Width `ctrl` will take (at most `max_w`: a choice that would not fit is
    /// drawn as its current option alone, as a cycle is at rest, its arrows
    /// only while the row is armed).
    pub(crate) fn kit_ctrl_width(&mut self, ctrl: &Ctrl, max_w: f32) -> f32 {
        match *ctrl {
            Ctrl::None => 0.0,
            Ctrl::Value(v) => self.kit_measure(v, size::LABEL + 1.0).min(max_w),
            Ctrl::Cycle(v) => (self.kit_measure(v, size::LABEL + 1.0) + 2.0 * CYCLE_PAD).min(max_w),
            Ctrl::Choice(opts, active) => match self.kit_choice_pad(opts, max_w) {
                Some((pad, px)) => self.kit_choice_width(opts, pad, px),
                None => {
                    let v = opts.get(active).copied().unwrap_or("");
                    self.kit_ctrl_width(&Ctrl::Cycle(v), max_w)
                }
            },
            Ctrl::Toggle(_) => TOGGLE_W,
        }
    }

    /// Draw `ctrl` ending at `right`, centred on `cy`, in at most `max_w`;
    /// returns the width it took. `focused` brightens it on the row with the
    /// cursor; the outline and arrows of an armed row are `kit_ctrl_parts`'.
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
                // The value alone, right-aligned. The arrows saying Left and
                // Right change it come with the armed outline only
                // (`kit_ctrl_parts`): drawn at rest, they promised a change
                // the arrows no longer make until A arms the row.
                let px = size::LABEL + 1.0;
                let v = self.kit_fit_mid(v, px, (max_w - 2.0 * CYCLE_PAD).max(20.0));
                let vw = self.kit_measure(&v, px);
                self.kit_text_mid((right - CYCLE_PAD - vw).round(), cy, px, &v, if focused { col::TEXT } else { col::TEXT2 });
                vw + 2.0 * CYCLE_PAD
            }
            Ctrl::Choice(opts, active) => {
                let Some((pad, px)) = self.kit_choice_pad(opts, max_w) else {
                    let v = opts.get(active).copied().unwrap_or("");
                    return self.kit_ctrl(right, cy, &Ctrl::Cycle(v), focused, max_w);
                };
                let total = self.kit_choice_width(opts, pad, px);
                let x0 = (right - total).round();
                let y0 = (cy - CHOICE_H * 0.5).round();
                self.kit_rrect(x0, y0, total, CHOICE_H, 10.0, 0xFF_10_19_2E);
                let mut sx = x0 + CHOICE_INSET;
                for (i, o) in opts.iter().enumerate() {
                    let ow = self.kit_measure(o, px);
                    let segw = ow + 2.0 * pad;
                    let ink = if i == active {
                        // The option in force is lit gold on every row, focused
                        // or not, as on the mockups: the panel reads as a list of
                        // current settings at a glance. The focus is the row's
                        // pill, not this.
                        self.kit_rrect(sx, y0 + CHOICE_INSET, segw, CHOICE_H - 2.0 * CHOICE_INSET, 8.0, col::GOLD);
                        0xFF_1A_14_05
                    } else if focused {
                        col::TEXT2
                    } else {
                        col::TEXT3
                    };
                    self.kit_text_mid((sx + pad).round(), cy, px, o, ink);
                    sx += segw;
                    // Where this option sits, for a finger and for an outline.
                    choice_seg_push((sx - segw, y0, segw, CHOICE_H));
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

    /// What goes on and around `row`'s control once `kit_ctrl` has drawn it,
    /// `cw` wide and ending at `right`, centred on `cy`: the gold outline and
    /// arrows of an armed row, the focus outline of a choice's lit option.
    /// Returns the parts a finger can tap, as (part, rect) (see `CtrlSegs`).
    pub(crate) fn kit_ctrl_parts(
        &mut self,
        right: f32,
        cy: f32,
        cw: f32,
        row: &super::list::Row,
    ) -> std::vec::Vec<(usize, (f32, f32, f32, f32))> {
        use super::list::{SEG_NEXT, SEG_PREV};
        let segs = CHOICE_SEGS.lock().map(|mut g| core::mem::take(&mut *g)).unwrap_or_default();
        let mut parts = std::vec::Vec::new();
        // A finger is taller than an option: the targets reach a little above
        // and below what is drawn, still inside the row.
        let tall = |r: (f32, f32, f32, f32)| (r.0, r.1 - 6.0, r.2, r.3 + 12.0);
        match row.ctrl {
            // Only a choice drawn whole has options to tap: one that fell back
            // to its lit value is a cycle to a finger, armed by a tap like A.
            Ctrl::Choice(opts, active) if segs.len() == opts.len() => {
                if row.seg_focus {
                    if let Some(&(x, y, w, h)) = segs.get(active) {
                        // Around the lit fill, which is inset in its track.
                        self.kit_ring(x, y + CHOICE_INSET, w, h - 2.0 * CHOICE_INSET, 8.0, 2.0, 2.0, col::GOLD);
                    }
                }
                parts.extend(segs.iter().enumerate().map(|(i, r)| (i, tall(*r))));
            }
            Ctrl::Toggle(_) => {
                let r = ((right - TOGGLE_W).round(), (cy - TOGGLE_H * 0.5).round(), TOGGLE_W, TOGGLE_H);
                parts.push((0, tall(r)));
            }
            _ => {}
        }
        if row.armed && matches!(row.ctrl, Ctrl::Choice(..) | Ctrl::Cycle(_)) {
            let (low, high) = match row.ctrl {
                Ctrl::Choice(opts, active) => (active == 0, active + 1 >= opts.len()),
                _ => row.ends,
            };
            let x0 = (right - cw).round();
            let y0 = (cy - CHOICE_H * 0.5).round();
            // The control itself: a choice's track, or the box around a
            // cycle's value. A gap, then the gold, so it reads on either.
            self.kit_ring(x0, y0, cw, CHOICE_H, 10.0, 1.0, 2.0, col::GOLD);
            let (lx, rx) = (x0 - ARM_GAP - ARM_ARROW, right + ARM_GAP);
            self.kit_icon(lx, cy, ARM_ARROW, Icon::ChevronLeft, if low { ARM_ARROW_DIM } else { col::GOLD });
            self.kit_icon(rx, cy, ARM_ARROW, Icon::ChevronRight, if high { ARM_ARROW_DIM } else { col::GOLD });
            parts.push((SEG_PREV, (lx - 4.0, y0 - 6.0, ARM_ARROW + 8.0, CHOICE_H + 12.0)));
            parts.push((SEG_NEXT, (rx - 4.0, y0 - 6.0, ARM_ARROW + 8.0, CHOICE_H + 12.0)));
        }
        parts
    }

    /// A key of the keyboard drawn as one: a raised cap with a darker lip and
    /// its legend, from `x`, centred on `cy`, `h` tall, at most `max_w` wide (a
    /// one-letter key is square). `dim` for a key that is only inherited.
    /// Returns its width.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn kit_keycap(&mut self, x: f32, cy: f32, max_w: f32, h: f32, label: &str, px: f32, dim: bool) -> f32 {
        const PAD_X: f32 = 9.0;
        let t = self.kit_fit(label, px, (max_w - 2.0 * PAD_X).max(0.0));
        let tw = self.kit_measure(&t, px);
        let w = (tw + 2.0 * PAD_X).max(h).min(max_w.max(h));
        let y = (cy - h * 0.5).round();
        let x = x.round();
        let (face, lip, ink) = if dim {
            (col::TILE, 0xFF_15_1E_33, col::TEXT3)
        } else {
            (col::CHIP, 0xFF_1A_1F_27, col::TEXT)
        };
        self.kit_rrect(x, y, w, h, 6.0, lip);
        self.kit_rrect(x, y, w, h - 2.0, 6.0, face);
        self.kit_text_mid((x + (w - tw) * 0.5).round(), y + (h - 2.0) * 0.5, px, &t, ink);
        w
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
