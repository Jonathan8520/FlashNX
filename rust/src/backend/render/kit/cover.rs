//! Cover art in a box of any shape, by one rule shared by every view.
//!
//! Covers come in every shape: square logos, 16:9 screenshots, 4:1 banners. The
//! grid used to crop them all to fill its tiles, which cut titles in half, and
//! then zoomed the selected one back out to show what had been cut. The rule
//! now: if filling the box costs at most `FILL_LIMIT` of the image, fill it;
//! otherwise show the WHOLE image, over a blurred, darkened copy of itself that
//! fills the rest of the box. Nothing is hidden, so nothing has to move when the
//! cursor arrives, and every tile still has the same size.
//!
//! The blurred copy is free on this GPU: a ~20 px wide version of the image,
//! softened once on the CPU and stretched with linear filtering, is the blur.
//! Steam, LaunchBox and ES-DE all ran into the mixed-shape problem; a backdrop
//! made from the image itself is the answer most of them settled on.

use super::super::*;

/// The most of an image the fill mode may crop away. 15% loses a sliver at the
/// edges of a near-fit image and nothing that matters; past it, titles go.
pub(crate) const FILL_LIMIT: f32 = 0.15;

/// Whether an `img_w` x `img_h` image fills a `box_w` x `box_h` box (cropped)
/// rather than sitting whole on its backdrop.
pub(crate) fn fills(img_w: u32, img_h: u32, box_w: f32, box_h: f32) -> bool {
    if img_w == 0 || img_h == 0 || box_w <= 0.0 || box_h <= 0.0 {
        return true;
    }
    let r = img_w as f32 / img_h as f32;
    let b = box_w / box_h;
    1.0 - r.min(b) / r.max(b) <= FILL_LIMIT
}

/// Width of the backdrop texture. Small on purpose: stretched with linear
/// filtering, its pixels ARE the blur.
const AMBIENT_W: usize = 20;
/// How dark the backdrop is next to the image itself (0..1 of its colour).
const AMBIENT_LIGHT: f32 = 0.5;

/// Average `rgba` (w x h) down to a small softened copy, as RGBA8.
pub(crate) fn ambient_pixels(rgba: &[u8], w: u32, h: u32) -> Option<(std::vec::Vec<u8>, u32, u32)> {
    let (w, h) = (w as usize, h as usize);
    if w == 0 || h == 0 || rgba.len() < w * h * 4 {
        return None;
    }
    let aw = AMBIENT_W.min(w).max(1);
    let ah = ((AMBIENT_W as f32 * h as f32 / w as f32).round() as usize).clamp(2, 40).min(h);
    // Box-average each cell of the source into one pixel.
    let mut acc = std::vec![0f32; aw * ah * 3];
    for ay in 0..ah {
        let y0 = ay * h / ah;
        let y1 = ((ay + 1) * h / ah).max(y0 + 1);
        for ax in 0..aw {
            let x0 = ax * w / aw;
            let x1 = ((ax + 1) * w / aw).max(x0 + 1);
            let (mut r, mut g, mut b, mut n) = (0f32, 0f32, 0f32, 0f32);
            // Sampling every 2nd pixel is plenty for an average this coarse.
            let mut y = y0;
            while y < y1 {
                let mut x = x0;
                while x < x1 {
                    let i = (y * w + x) * 4;
                    let a = rgba[i + 3] as f32 / 255.0;
                    r += rgba[i] as f32 * a;
                    g += rgba[i + 1] as f32 * a;
                    b += rgba[i + 2] as f32 * a;
                    n += 1.0;
                    x += 2;
                }
                y += 2;
            }
            let o = (ay * aw + ax) * 3;
            let n = n.max(1.0);
            acc[o] = r / n;
            acc[o + 1] = g / n;
            acc[o + 2] = b / n;
        }
    }
    // Two passes of a 3x3 box blur, edges clamped: soft, with no blocks.
    for _ in 0..2 {
        let src = acc.clone();
        for y in 0..ah {
            for x in 0..aw {
                let mut s = [0f32; 3];
                let mut n = 0f32;
                for dy in [-1i32, 0, 1] {
                    for dx in [-1i32, 0, 1] {
                        let sx = (x as i32 + dx).clamp(0, aw as i32 - 1) as usize;
                        let sy = (y as i32 + dy).clamp(0, ah as i32 - 1) as usize;
                        let i = (sy * aw + sx) * 3;
                        s[0] += src[i];
                        s[1] += src[i + 1];
                        s[2] += src[i + 2];
                        n += 1.0;
                    }
                }
                let o = (y * aw + x) * 3;
                acc[o] = s[0] / n;
                acc[o + 1] = s[1] / n;
                acc[o + 2] = s[2] / n;
            }
        }
    }
    let mut out = std::vec::Vec::with_capacity(aw * ah * 4);
    for px in acc.chunks(3) {
        for c in px {
            out.push((c * AMBIENT_LIGHT).clamp(0.0, 255.0) as u8);
        }
        out.push(255);
    }
    Some((out, aw as u32, ah as u32))
}

/// `a` and `b` (0xRRGGBB) mixed, `t` of the way from `a` to `b`.
fn mix_rgb(a: u32, b: u32, t: f32) -> u32 {
    let ch = |s: u32| -> (f32, f32) {
        (((a >> s) & 0xFF) as f32, ((b >> s) & 0xFF) as f32)
    };
    let mut out = 0u32;
    for s in [16u32, 8, 0] {
        let (x, y) = ch(s);
        out |= ((x + (y - x) * t).round().clamp(0.0, 255.0) as u32) << s;
    }
    out
}

impl SwitchRenderBackend {
    /// The tile of a game with no cover: its colour, deepened towards the page,
    /// with its NAME on it rather than three initials. "BAD" told you nothing
    /// about which of two Bad Ice Creams it was.
    pub(crate) fn kit_default_tile(&mut self, x: f32, y: f32, w: f32, h: f32, chip: u32, name: &str) {
        use super::theme::col;
        let chip = chip & 0x00FF_FFFF;
        let fill = mix_rgb(chip, col::BG & 0x00FF_FFFF, 0.55);
        let ink = mix_rgb(chip, 0xFF_FF_FF, 0.78);
        self.draw_overlay_rect(x, y, w, h, 0xFF00_0000 | fill);
        let px = if w >= 180.0 { 18.0 } else { 15.0 };
        let pad = (w * 0.07).clamp(10.0, 16.0);
        let shown = name.strip_suffix(".swf").or_else(|| name.strip_suffix(".SWF")).unwrap_or(name);
        let lines = self.kit_wrap(shown, px, w - 2.0 * pad, 3);
        let lh = (px * 1.25).round();
        let mut baseline = y + h - pad - lh * (lines.len().saturating_sub(1)) as f32;
        for line in &lines {
            self.kit_text_baseline(x + pad, baseline, px, line, 0xFF00_0000 | ink);
            baseline += lh;
        }
    }

    /// Upload the backdrop for a cover. 0 when there is nothing to make it from.
    pub(crate) fn kit_ambient_texture(&mut self, rgba: &[u8], w: u32, h: u32) -> GLuint {
        match ambient_pixels(rgba, w, h) {
            Some((px, aw, ah)) => self.upload_rgba_texture(&px, aw, ah),
            None => 0,
        }
    }

    /// Draw a cover in the box `(x, y, w, h)` by the rule above. `alpha` fades
    /// the whole thing in over whatever is under it.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn kit_cover(&mut self, x: f32, y: f32, w: f32, h: f32, tex: GLuint, tw: u32, th: u32, amb: GLuint, alpha: f32) {
        if tex == 0 || tw == 0 || th == 0 {
            return;
        }
        if fills(tw, th, w, h) {
            self.draw_textured_rect_cover(x, y, w, h, tex, tw, th, alpha);
            return;
        }
        if amb != 0 {
            // The backdrop's own aspect is the image's, so crop it to the box
            // the same way the fill mode would.
            self.draw_textured_rect_cover(x, y, w, h, amb, tw, th, alpha);
        } else {
            self.draw_overlay_rect(x, y, w, h, ((alpha.clamp(0.0, 1.0) * 255.0) as u32) << 24 | 0x0A_10_1C);
        }
        let s = (w / tw as f32).min(h / th as f32);
        let (dw, dh) = (tw as f32 * s, th as f32 * s);
        let (dx, dy) = ((x + (w - dw) * 0.5).round(), (y + (h - dh) * 0.5).round());
        // `draw_textured_rect_cover` into a box of the image's own aspect is a
        // plain fitted draw, with the alpha the fade needs.
        self.draw_textured_rect_cover(dx, dy, dw.round(), dh.round(), tex, tw, th, alpha);
    }
}
