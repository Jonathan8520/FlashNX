//! Cover art in a box of any shape.
//!
//! Covers come in every shape: square logos, 16:9 screenshots, banners ten
//! times wider than tall. One rule for every box (Jaquettes.dc.html): a cover
//! fills its box when that crops at most `FILL_LIMIT` of it, and otherwise
//! shows whole over a blurred, darkened copy of itself.
//!
//! - A box that only shows (a strip tile, a picker cell) applies the rule as it
//!   stands (`kit_cover`).
//! - A TILE that OPENS when selected (the grid, the shelf) rests as close to
//!   filled as the rule allows (`rest_t`) and relaxes to the whole image as it
//!   lifts (`kit_cover_reveal`), so the same cover never loses more than the
//!   limit at rest, whichever view it is in.
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

/// How far open (`t` of `kit_cover_reveal`) a tile has to rest so its crop
/// costs at most `FILL_LIMIT` of an `img_w` x `img_h` image: 0 for a cover that
/// fills within the limit, up to 1 for one that has to show whole.
///
/// The opened box shrinks from the tile toward the fitted image while the crop
/// inside it relaxes, so the loss at `t` has a closed form, solved here for the
/// `t` where it equals the limit. In a 16:10 tile a 655x155 banner rests at
/// 0.89, a square logo at 0.71, a 4:3 picture at 0.12, and a 16:9 one fills.
pub(crate) fn rest_t(img_w: u32, img_h: u32, box_w: f32, box_h: f32) -> f32 {
    if img_w == 0 || img_h == 0 || box_w <= 0.0 || box_h <= 0.0 {
        return 0.0;
    }
    let a = img_w as f32 / img_h as f32;
    let keep = 1.0 - FILL_LIMIT;
    let t = if a > box_w / box_h {
        // Wider than the box: the box gets shorter as it opens.
        let span = box_h - box_w / a;
        if span <= 0.0 {
            return 0.0;
        }
        (box_h - box_w / (a * keep)) / span
    } else {
        // Taller: the box gets narrower.
        let span = box_w - box_h * a;
        if span <= 0.0 {
            return 0.0;
        }
        (box_w - a * box_h / keep) / span
    };
    t.clamp(0.0, 1.0)
}

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
/// How much the backdrop's colours are pushed apart from their grey, so a
/// darkened copy still reads as the cover's own colours and not as mud.
const AMBIENT_SATURATE: f32 = 1.3;
/// The backdrop is the image enlarged by this much behind the box (the mockup's
/// `left: -15%; width: 130%`), so its edges, the blurriest part, stay outside.
const AMBIENT_ZOOM: f32 = 1.3;

/// Average `rgba` (w x h) down to a small softened backdrop, as opaque RGBA8.
///
/// What the mockup's CSS does to the image (blur, `brightness(0.5)`,
/// `saturate(1.3)`) and then what the browser does with the result: lay it over
/// the tile colour. A blur keeps the image's alpha, so the transparent parts of
/// a logo come out as the tile itself. The first version composited the image
/// over BLACK instead (each sample weighted by its alpha, divided by the sample
/// count, written opaque), and every transparent PNG sat on a muddy dark box.
/// Here the colours are blurred premultiplied together with their coverage,
/// and the tile shows through exactly where the image does not cover it.
pub(crate) fn ambient_pixels(rgba: &[u8], w: u32, h: u32) -> Option<(std::vec::Vec<u8>, u32, u32)> {
    use super::theme::col;
    let (w, h) = (w as usize, h as usize);
    if w == 0 || h == 0 || rgba.len() < w * h * 4 {
        return None;
    }
    let aw = AMBIENT_W.min(w).max(1);
    let ah = ((AMBIENT_W as f32 * h as f32 / w as f32).round() as usize).clamp(2, 40).min(h);
    // Box-average each cell of the source into one pixel: the colour
    // premultiplied by its alpha, and the coverage (mean alpha).
    let mut acc = std::vec![0f32; aw * ah * 4];
    for ay in 0..ah {
        let y0 = ay * h / ah;
        let y1 = ((ay + 1) * h / ah).max(y0 + 1);
        for ax in 0..aw {
            let x0 = ax * w / aw;
            let x1 = ((ax + 1) * w / aw).max(x0 + 1);
            let (mut r, mut g, mut b, mut cov, mut n) = (0f32, 0f32, 0f32, 0f32, 0f32);
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
                    cov += a;
                    n += 1.0;
                    x += 2;
                }
                y += 2;
            }
            let o = (ay * aw + ax) * 4;
            let n = n.max(1.0);
            acc[o] = r / n;
            acc[o + 1] = g / n;
            acc[o + 2] = b / n;
            acc[o + 3] = cov / n;
        }
    }
    // Two passes of a 3x3 box blur over all four channels, edges clamped:
    // soft, with no blocks, and the coverage softened with the colour.
    for _ in 0..2 {
        let src = acc.clone();
        for y in 0..ah {
            for x in 0..aw {
                let mut s = [0f32; 4];
                let mut n = 0f32;
                for dy in [-1i32, 0, 1] {
                    for dx in [-1i32, 0, 1] {
                        let sx = (x as i32 + dx).clamp(0, aw as i32 - 1) as usize;
                        let sy = (y as i32 + dy).clamp(0, ah as i32 - 1) as usize;
                        let i = (sy * aw + sx) * 4;
                        for c in 0..4 {
                            s[c] += src[i + c];
                        }
                        n += 1.0;
                    }
                }
                let o = (y * aw + x) * 4;
                for c in 0..4 {
                    acc[o + c] = s[c] / n;
                }
            }
        }
    }
    let tile = [((col::TILE >> 16) & 0xFF) as f32, ((col::TILE >> 8) & 0xFF) as f32, (col::TILE & 0xFF) as f32];
    let mut out = std::vec::Vec::with_capacity(aw * ah * 4);
    for px in acc.chunks(4) {
        // Darken, then saturate. Both are linear in the colour, so applying
        // them to the premultiplied value is the same as applying them first.
        let p = [px[0] * AMBIENT_LIGHT, px[1] * AMBIENT_LIGHT, px[2] * AMBIENT_LIGHT];
        let lum = 0.213 * p[0] + 0.715 * p[1] + 0.072 * p[2];
        let uncovered = 1.0 - px[3].clamp(0.0, 1.0);
        for c in 0..3 {
            let v = (lum + (p[c] - lum) * AMBIENT_SATURATE).max(0.0) + tile[c] * uncovered;
            out.push(v.clamp(0.0, 255.0) as u8);
        }
        out.push(255);
    }
    Some((out, aw as u32, ah as u32))
}

/// The part of the backdrop shown in a `box_w` x `box_h` box: cropped to fill
/// it like the image would be, then enlarged by `AMBIENT_ZOOM` about its centre.
fn backdrop_uv(img_w: u32, img_h: u32, box_w: f32, box_h: f32) -> [f32; 4] {
    let [u0, v0, du, dv] = crop_uv(img_w, img_h, box_w, box_h);
    let (zu, zv) = (du / AMBIENT_ZOOM, dv / AMBIENT_ZOOM);
    [u0 + (du - zu) * 0.5, v0 + (dv - zv) * 0.5, zu, zv]
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

/// Under this height a game's generated tile is a small box (a panel's
/// header, the rail's head): one line of name, no gamepad.
const DEFAULT_TILE_SMALL_H: f32 = 80.0;

impl SwitchRenderBackend {
    /// The tile of a game with no cover: its colour, deepened towards the page,
    /// a gamepad in the corner and its NAME at the bottom, as the mockup draws
    /// it, rather than three initials. "BAD" told you nothing about which of two
    /// Bad Ice Creams it was.
    pub(crate) fn kit_default_tile(&mut self, x: f32, y: f32, w: f32, h: f32, chip: u32, name: &str) {
        self.kit_default_ground(x, y, w, h, chip);
        let ink = mix_rgb(chip & 0x00FF_FFFF, 0xFF_FF_FF, 0.78);
        let px = if w >= 180.0 { 20.0 } else { 17.0 };
        let pad = (w * 0.07).clamp(10.0, 16.0);
        let shown = name.strip_suffix(".swf").or_else(|| name.strip_suffix(".SWF")).unwrap_or(name);
        // A small box (a panel's 96 x 60 header, the rail's 56 x 35 head):
        // one line at 13 px, cut in the middle, centred, and no gamepad. The
        // grid's two lines at 17 px ran into the gamepad over them in the
        // header, and in the rail's head the first line sat above the tile.
        if h < DEFAULT_TILE_SMALL_H {
            const SMALL_PX: f32 = 13.0;
            let t = self.kit_fit_mid(shown, SMALL_PX, (w - 12.0).max(0.0));
            let tw = self.kit_measure(&t, SMALL_PX);
            self.kit_text_mid((x + (w - tw) * 0.5).round(), y + h * 0.5, SMALL_PX, &t, 0xFF00_0000 | ink);
            return;
        }
        // Room for the gamepad above the name, so the two never touch.
        let max_lines = if h >= 120.0 { 3 } else { 2 };
        let lines = self.kit_wrap(shown, px, w - 2.0 * pad, max_lines);
        let lh = (px * 1.15).round();
        let mut baseline = y + h - pad - lh * (lines.len().saturating_sub(1)) as f32;
        for line in &lines {
            self.kit_text_baseline(x + pad, baseline, px, line, 0xFF00_0000 | ink);
            baseline += lh;
        }
    }

    /// `kit_default_tile` grown from a tile of size `base` into the box
    /// `(x, y, w, h)`, with corners of radius `r`: the launch carries a game
    /// with no cover to the whole screen the way it carries a cover, and the
    /// quit brings it back. In a box of the base size with TILE corners it IS
    /// the tile, so neither end of the movement jumps.
    ///
    /// The tile is zoomed, not laid out again: the name keeps the lines it had
    /// on the tile, and it grows with its margins and the gamepad, up to a
    /// 64 px name, so nothing reflows on the way. Its letters are not drawn at
    /// every size they pass through. The glyph atlas keeps every size it is
    /// asked for and has no room to spare, and the old launch walked one name
    /// through fifty sizes. They are rasterized at the tile's size and at the
    /// final one, each scaled to the size of the moment, the first handing
    /// over to the second as the box grows.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn kit_default_tile_grown(
        &mut self,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        chip: u32,
        name: &str,
        base: (f32, f32),
        r: f32,
    ) {
        use super::icons::Icon;
        use super::theme::{col, radius};
        /// The largest the name grows to, as on the full-screen launch before.
        const NAME_MAX: f32 = 64.0;
        /// The gamepad's size on the tile.
        const PAD_ICON: f32 = 22.0;
        let (bw, bh) = base;
        if bw <= 0.0 || bh <= 0.0 || (w <= bw && h <= bh && r >= radius::TILE) {
            self.kit_default_tile(x, y, w, h, chip, name);
            return;
        }
        let (vw, vh) = (self.dimensions.width as f32, self.dimensions.height as f32);
        let px0 = if bw >= 180.0 { 20.0 } else { 17.0 };
        // The zoom at the end, on the whole screen: rounded down to a whole
        // size of the name, so that state draws its letters at their own
        // raster, unscaled. Then the zoom now, which reaches it there.
        let fit = (vw / bw).min(vh / bh).clamp(1.0, NAME_MAX / px0);
        let z_end = ((px0 * fit + 0.001).floor() / px0).max(1.0);
        let z = (w / bw).min(h / bh).clamp(1.0, z_end);
        // 0 while the box is the tile, 1 once it has grown to its final size.
        let f = if z_end > 1.0 { (z - 1.0) / (z_end - 1.0) } else { 1.0 };
        let chip = chip & 0x00FF_FFFF;
        self.kit_rrect(x, y, w, h, r, 0xFF00_0000 | mix_rgb(chip, col::BG & 0x00FF_FFFF, 0.55));
        let alpha = |a: f32, rgb: u32| ((a.clamp(0.0, 1.0) * 255.0).round() as u32) << 24 | rgb;
        let pad0 = (bw * 0.07).clamp(10.0, 16.0);
        let pad = pad0 * z;
        // Each element is drawn from two rasters, the tile's and the final
        // one, scaled about its own corner to its size of the moment. Their
        // fades overlap, one at full strength while the other comes or goes:
        // two half-faded copies let the ground through the letters half way.
        let (fade_tile, fade_end) = ((2.0 * (1.0 - f)).min(1.0), (2.0 * f).min(1.0));
        if bw >= 64.0 && bh >= DEFAULT_TILE_SMALL_H {
            let ink = mix_rgb(chip, col::BG & 0x00FF_FFFF, 0.25);
            let end = (PAD_ICON * z_end).round();
            let size = PAD_ICON + (end - PAD_ICON) * f;
            let (ix, iy) = ((x + pad).round(), (y + pad).round());
            for (raster, a) in [(PAD_ICON, fade_tile), (end, fade_end)] {
                if a > 0.0 {
                    self.kit_scaled(ix, iy, size / raster, |s| {
                        s.kit_icon(ix, iy + raster * 0.5, raster, Icon::Gamepad, alpha(a, ink));
                    });
                }
            }
        }
        let ink = mix_rgb(chip, 0xFF_FF_FF, 0.78);
        let shown = name.strip_suffix(".swf").or_else(|| name.strip_suffix(".SWF")).unwrap_or(name);
        let max_lines = if bh >= 120.0 { 3 } else { 2 };
        let lines = self.kit_wrap(shown, px0, bw - 2.0 * pad0, max_lines);
        let end = (px0 * z_end).round();
        let lh = (px0 * 1.15).round() * z;
        let mut baseline = y + h - pad - lh * (lines.len().saturating_sub(1)) as f32;
        let lx = (x + pad).round();
        for line in &lines {
            let by = baseline.round();
            for (raster, a) in [(px0, fade_tile), (end, fade_end)] {
                if a > 0.0 {
                    self.kit_scaled(lx, by, px0 * z / raster, |s| {
                        s.kit_text_baseline(lx, by, raster, line, alpha(a, ink));
                    });
                }
            }
            baseline += lh;
        }
    }

    /// Run `draw` with what it draws scaled by `s` about `(px, py)`, on top of
    /// the UI transform already in force.
    fn kit_scaled(&mut self, px: f32, py: f32, s: f32, draw: impl FnOnce(&mut Self)) {
        let saved = (self.ui_scale, self.ui_pivot_x, self.ui_pivot_y, self.ui_translate_x, self.ui_translate_y);
        let (s0, qx, qy, tx, ty) = saved;
        // p -> p*s + P*(1 - s), then the transform in force, q -> q*s0 + Q*(1 - s0) + T,
        // folded into one scale and one translation.
        self.ui_scale = s0 * s;
        self.ui_pivot_x = 0.0;
        self.ui_pivot_y = 0.0;
        self.ui_translate_x = s0 * px * (1.0 - s) + qx * (1.0 - s0) + tx;
        self.ui_translate_y = s0 * py * (1.0 - s) + qy * (1.0 - s0) + ty;
        draw(self);
        (self.ui_scale, self.ui_pivot_x, self.ui_pivot_y, self.ui_translate_x, self.ui_translate_y) = saved;
    }

    /// The generated tile without its name: what sits under a cover that is
    /// still fading in. With the name, the title showed through the art for the
    /// length of the fade.
    pub(crate) fn kit_default_ground(&mut self, x: f32, y: f32, w: f32, h: f32, chip: u32) {
        use super::theme::col;
        let chip = chip & 0x00FF_FFFF;
        let fill = mix_rgb(chip, col::BG & 0x00FF_FFFF, 0.55);
        self.kit_rrect(x, y, w, h, super::theme::radius::TILE, 0xFF00_0000 | fill);
        if w >= 64.0 && h >= DEFAULT_TILE_SMALL_H {
            let icon_ink = mix_rgb(chip, col::BG & 0x00FF_FFFF, 0.25);
            let pad = (w * 0.07).clamp(10.0, 16.0);
            self.kit_icon(x + pad, y + pad + 11.0, 22.0, super::icons::Icon::Gamepad, 0xFF00_0000 | icon_ink);
        }
    }

    /// The favourite star, SOLID, in the `px` x `px` box whose left edge is `x`,
    /// centred on `cy`; returns `px`. The console font's black star: Lucide's
    /// outline star is what a row shows for a game that is NOT a favourite
    /// (the options' switch), so a marked favourite never uses it.
    pub(crate) fn kit_star(&mut self, x: f32, cy: f32, px: f32, ink: u32) -> f32 {
        let w = self.kit_measure("\u{2605}", px);
        self.kit_text_mid((x + (px - w) * 0.5).round(), cy, px, "\u{2605}", ink);
        px
    }

    /// Upload the backdrop for a cover. 0 when there is nothing to make it from.
    pub(crate) fn kit_ambient_texture(&mut self, rgba: &[u8], w: u32, h: u32) -> GLuint {
        match ambient_pixels(rgba, w, h) {
            Some((px, aw, ah)) => self.upload_rgba_texture(&px, aw, ah),
            None => 0,
        }
    }

    /// A cover that opens to show itself whole as its tile is selected: `t`
    /// runs 0 (cropped to fill the tile) to 1 (the whole image, centred over
    /// its blurred backdrop). A tile rests at `rest_t`, not at 0, so a banner
    /// ten times wider than tall is never cut past the limit; opening the one
    /// under the cursor shows the rest, and the movement is what makes the grid
    /// feel alive.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn kit_cover_reveal(&mut self, x: f32, y: f32, w: f32, h: f32, tex: GLuint, tw: u32, th: u32, amb: GLuint, t: f32, alpha: f32) {
        self.kit_cover_reveal_r(x, y, w, h, tex, tw, th, amb, t, alpha, super::theme::radius::TILE);
    }

    /// `kit_cover_reveal` with its corner radius: the launch grows a tile into
    /// the whole screen, and its corners straighten as it goes.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn kit_cover_reveal_r(
        &mut self,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        tex: GLuint,
        tw: u32,
        th: u32,
        amb: GLuint,
        t: f32,
        alpha: f32,
        r: f32,
    ) {
        if tex == 0 || tw == 0 || th == 0 {
            return;
        }
        let t = t.clamp(0.0, 1.0);
        let a = tw as f32 / th as f32;
        let (fit_w, fit_h) = if a > w / h { (w, w / a) } else { (h * a, h) };
        // The ground is ALWAYS under the cover, at rest too. It used to be
        // drawn only while the tile was opening, so the frame the tile came to
        // rest it vanished: invisible under an opaque cover, but under a PNG
        // with transparent parts the blur turned into the page colour in one
        // frame, which is the jolt at the end of the animation. Drawn every
        // frame, the transparent parts always sit on the same ground.
        self.kit_cover_ground(x, y, w, h, tw, th, amb, alpha, r);
        // The drawn box shrinks from the tile to the fitted image while the crop
        // in it relaxes, so at t = 1 nothing is cut.
        let dw = w + (fit_w - w) * t;
        let dh = h + (fit_h - h) * t;
        let (dx, dy) = (x + (w - dw) * 0.5, y + (h - dh) * 0.5);
        self.kit_image(dx, dy, dw, dh, tex, crop_uv(tw, th, dw, dh), alpha, r);
    }

    /// What a cover's transparent parts sit on: its blurred backdrop, which is
    /// the tile colour wherever the image itself is transparent, or the tile
    /// colour alone for an image that has no backdrop (a Flashpoint thumbnail).
    /// The same ground in every view, so a logo never changes background
    /// between the grid, the strip and the detail panels.
    #[allow(clippy::too_many_arguments)]
    fn kit_cover_ground(&mut self, x: f32, y: f32, w: f32, h: f32, tw: u32, th: u32, amb: GLuint, alpha: f32, r: f32) {
        use super::theme::col;
        if amb != 0 {
            self.kit_image(x, y, w, h, amb, backdrop_uv(tw, th, w, h), alpha, r);
        } else {
            self.kit_rrect(x, y, w, h, r, ((alpha.clamp(0.0, 1.0) * 255.0) as u32) << 24 | (col::TILE & 0x00FF_FFFF));
        }
    }

    /// Draw a cover in the box `(x, y, w, h)` by the rule above, for a box that
    /// does not open. `alpha` fades the whole thing in over whatever is under it.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn kit_cover(&mut self, x: f32, y: f32, w: f32, h: f32, tex: GLuint, tw: u32, th: u32, amb: GLuint, alpha: f32) {
        if tex == 0 || tw == 0 || th == 0 {
            return;
        }
        let r = super::theme::radius::TILE;
        if fills(tw, th, w, h) {
            self.kit_cover_ground(x, y, w, h, tw, th, amb, alpha, r);
            self.kit_image(x, y, w, h, tex, crop_uv(tw, th, w, h), alpha, r);
            return;
        }
        self.kit_cover_reveal(x, y, w, h, tex, tw, th, amb, 1.0, alpha);
    }
}
