//! Cover art in a box of any shape.
//!
//! Covers come in every shape: square logos, 16:9 screenshots, banners ten
//! times wider than tall. Two ways of showing them, one per kind of box:
//!
//! - A TILE in a grid or a strip fills its box at rest (cropped, so the grid is
//!   calm and every tile reads as a picture) and OPENS when selected: it grows a
//!   little, lifts on a shadow, and relaxes to the whole image over a blurred,
//!   darkened copy of itself (`kit_cover_reveal`).
//! - A box that shows one cover on its own (the launch, a detail panel) shows it
//!   whole over that backdrop unless filling costs at most `FILL_LIMIT` of the
//!   image (`kit_cover`).
//!
//! Corners are cut by the image shader (`kit_image`), not painted over in the
//! page colour, so a cover looks right over a shadow or a panel too.
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

/// The texture sub-rectangle `[u0, v0, du, dv]` that crops an `img_w` x
/// `img_h` image to fill a `box_w` x `box_h` box, centred.
pub(crate) fn crop_uv(img_w: u32, img_h: u32, box_w: f32, box_h: f32) -> [f32; 4] {
    if img_w == 0 || img_h == 0 || box_w <= 0.0 || box_h <= 0.0 {
        return [0.0, 0.0, 1.0, 1.0];
    }
    let img = img_w as f32 / img_h as f32;
    let bx = box_w / box_h;
    if img > bx {
        let f = bx / img;
        [(1.0 - f) * 0.5, 0.0, f, 1.0]
    } else {
        let f = img / bx;
        [0.0, (1.0 - f) * 0.5, 1.0, f]
    }
}

/// `a` and `b` (0xRRGGBB) mixed, `t` of the way from `a` to `b`.
pub(crate) fn mix_rgb(a: u32, b: u32, t: f32) -> u32 {
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
        self.kit_rrect(x, y, w, h, super::theme::radius::TILE, 0xFF00_0000 | fill);
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

    /// A cover that fills its tile at rest and OPENS to show itself whole as
    /// the tile is selected: `t` runs 0 (cropped to fill) to 1 (the whole
    /// image, centred over its blurred backdrop).
    ///
    /// A grid of filled tiles is the calmer picture, and a banner ten times
    /// wider than tall would be a thin strip if shown whole at rest. Opening
    /// the one under the cursor gives both, and the movement is what makes the
    /// grid feel alive.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn kit_cover_reveal(&mut self, x: f32, y: f32, w: f32, h: f32, tex: GLuint, tw: u32, th: u32, amb: GLuint, t: f32, alpha: f32) {
        if tex == 0 || tw == 0 || th == 0 {
            return;
        }
        let r = super::theme::radius::TILE;
        let t = t.clamp(0.0, 1.0);
        let a = tw as f32 / th as f32;
        let (fit_w, fit_h) = if a > w / h { (w, w / a) } else { (h * a, h) };
        if t > 0.0 && (fit_w < w - 0.5 || fit_h < h - 0.5) {
            if amb != 0 {
                self.kit_image(x, y, w, h, amb, crop_uv(tw, th, w, h), alpha, r);
            } else {
                self.kit_rrect(x, y, w, h, r, ((alpha.clamp(0.0, 1.0) * 255.0) as u32) << 24 | 0x0A_10_1C);
            }
        }
        // The drawn box shrinks from the tile to the fitted image while the crop
        // in it relaxes, so at t = 1 nothing is cut.
        let dw = w + (fit_w - w) * t;
        let dh = h + (fit_h - h) * t;
        let (dx, dy) = (x + (w - dw) * 0.5, y + (h - dh) * 0.5);
        self.kit_image(dx, dy, dw, dh, tex, crop_uv(tw, th, dw, dh), alpha, r);
    }

    /// Draw a cover in the box `(x, y, w, h)` by the rule above. `alpha` fades
    /// the whole thing in over whatever is under it.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn kit_cover(&mut self, x: f32, y: f32, w: f32, h: f32, tex: GLuint, tw: u32, th: u32, amb: GLuint, alpha: f32) {
        if tex == 0 || tw == 0 || th == 0 {
            return;
        }
        let r = super::theme::radius::TILE;
        if fills(tw, th, w, h) {
            self.kit_image(x, y, w, h, tex, crop_uv(tw, th, w, h), alpha, r);
            return;
        }
        self.kit_cover_reveal(x, y, w, h, tex, tw, th, amb, 1.0, alpha);
    }
}
