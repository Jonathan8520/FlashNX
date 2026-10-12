//! Interface text in the console's own fonts, rasterized at the size it is drawn.
//!
//! The old launcher font is a 5x7 pattern drawn by hand in `render.rs`: capitals
//! only, an accented letter folded onto its capital, and every character the
//! patterns lack sent to a second font in fixed-width cells. That is why "é"
//! looked lower-case beside its own word and a Chinese title came out a third
//! too wide. Here every character comes from the fonts the console itself draws
//! its menus with, at its real width:
//!
//! - **Standard** (`pl` kind 0): Latin, Cyrillic, Greek, kana and the JIS kanji.
//! - **NintendoExt** (kind 5): the button glyphs, A = U+E0E0 and so on.
//! - **Chinese Simplified** (kind 1): whatever Standard lacks, parsed on first
//!   use only.
//!
//! None of it ships in the .nro. Each glyph is rasterized once per pixel size
//! and drawn 1:1, which is what keeps 15 px text sharp: the CJK atlas next door
//! rasterizes everything at 44 px and lets the sampler shrink it.

use super::super::*;
use crate::backend::glyphs::{new_atlas_texture, raster_gid_px, ATLAS_DIM, PAD};
use std::collections::HashMap;

extern "C" {
    fn ruffle_shared_font(kind: core::ffi::c_int, out_size: *mut u32) -> *const u8;
}

/// libnx `PlSharedFontType`.
const PL_STANDARD: core::ffi::c_int = 0;
const PL_CHINESE_SIMPLIFIED: core::ffi::c_int = 1;
const PL_NINTENDO_EXT: core::ffi::c_int = 5;

/// Two atlases at most: one 1024² texture holds well over a thousand glyphs at
/// interface sizes, and the second only opens for a long Chinese library.
const ATLAS_MAX: usize = 2;

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum FaceId {
    Standard,
    Ext,
    Chinese,
}

/// A controller button, drawn with the console's own glyph for it.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Btn {
    A,
    B,
    X,
    Y,
    L,
    R,
    ZL,
    ZR,
    Plus,
    Minus,
}

impl Btn {
    /// Code points in NintendoExt, as Borealis and the other homebrews that use
    /// the shared font map them.
    fn glyph(self) -> char {
        match self {
            Btn::A => '\u{E0E0}',
            Btn::B => '\u{E0E1}',
            Btn::X => '\u{E0E2}',
            Btn::Y => '\u{E0E3}',
            Btn::L => '\u{E0E4}',
            Btn::R => '\u{E0E5}',
            Btn::ZL => '\u{E0E6}',
            Btn::ZR => '\u{E0E7}',
            Btn::Plus => '\u{E0EF}',
            Btn::Minus => '\u{E0F0}',
        }
    }

    /// What to print inside a drawn circle when NintendoExt is unavailable.
    fn letter(self) -> &'static str {
        match self {
            Btn::A => "A",
            Btn::B => "B",
            Btn::X => "X",
            Btn::Y => "Y",
            Btn::L => "L",
            Btn::R => "R",
            Btn::ZL => "ZL",
            Btn::ZR => "ZR",
            Btn::Plus => "+",
            Btn::Minus => "\u{2212}",
        }
    }
}

#[derive(Clone, Copy)]
struct UiGlyph {
    tex: GLuint,
    /// Atlas UV sub-rect `[u0, v0, du, dv]`.
    uv: [f32; 4],
    w: f32,
    h: f32,
    /// Left bearing, and baseline to bitmap bottom with y up.
    xmin: f32,
    ymin: f32,
    blank: bool,
}

/// One laid-out glyph: where it comes from and its pen offset from the start of
/// the line.
#[derive(Clone, Copy)]
struct Placed {
    face: FaceId,
    gid: ttf_parser::GlyphId,
    x: f32,
}

fn shared_face(kind: core::ffi::c_int) -> Option<ttf_parser::Face<'static>> {
    let bytes: &'static [u8] = unsafe {
        let mut len: u32 = 0;
        let ptr = ruffle_shared_font(kind, &mut len);
        if ptr.is_null() || len == 0 {
            return None;
        }
        // 'static for the reason `glyphs.rs` gives: `pl` is initialized once and
        // never torn down, so the mapping outlives every Face borrowing it.
        core::slice::from_raw_parts(ptr, len as usize)
    };
    ttf_parser::Face::parse(bytes, 0).ok()
}

pub(crate) struct UiText {
    std: ttf_parser::Face<'static>,
    ext: Option<ttf_parser::Face<'static>>,
    cn: Option<ttf_parser::Face<'static>>,
    cn_tried: bool,
    raster: ab_glyph_rasterizer::Rasterizer,
    /// Which face and glyph a character resolved to, misses included, so the
    /// fallback chain runs once per character per renderer.
    chars: HashMap<char, Option<(FaceId, u16)>>,
    /// Keyed by face, glyph and pixel size: the same letter at 15 and 26 px is
    /// two bitmaps, each drawn at its own size.
    glyphs: HashMap<(FaceId, u16, u16), Option<UiGlyph>>,
    tex: GLuint,
    texs: std::vec::Vec<GLuint>,
    pen_x: usize,
    pen_y: usize,
    shelf_h: usize,
    full: bool,
}

impl UiText {
    /// `None` when the Standard font cannot be had, and the caller falls back
    /// to the old font rather than drawing nothing.
    fn new() -> Option<UiText> {
        let std = shared_face(PL_STANDARD)?;
        let ext = shared_face(PL_NINTENDO_EXT);
        let tex = new_atlas_texture()?;
        log_str(&std::format!(
            "kit: fonts ready (standard {} upem, nintendo-ext {})\n",
            std.units_per_em(),
            if ext.is_some() { "yes" } else { "MISSING, glyphs drawn by hand" },
        ));
        Some(UiText {
            std,
            ext,
            cn: None,
            cn_tried: false,
            raster: ab_glyph_rasterizer::Rasterizer::new(0, 0),
            chars: HashMap::new(),
            glyphs: HashMap::new(),
            tex,
            texs: std::vec![tex],
            pen_x: PAD,
            pen_y: PAD,
            shelf_h: 0,
            full: false,
        })
    }

    fn face(&self, id: FaceId) -> Option<&ttf_parser::Face<'static>> {
        match id {
            FaceId::Standard => Some(&self.std),
            FaceId::Ext => self.ext.as_ref(),
            FaceId::Chinese => self.cn.as_ref(),
        }
    }

    fn resolve(&mut self, ch: char) -> Option<(FaceId, ttf_parser::GlyphId)> {
        if let Some(hit) = self.chars.get(&ch) {
            return hit.map(|(f, g)| (f, ttf_parser::GlyphId(g)));
        }
        // GlyphId 0 is `.notdef`, the tofu box: treat it as absent.
        let find = |face: Option<&ttf_parser::Face<'static>>| {
            face.and_then(|f| f.glyph_index(ch)).filter(|g| g.0 != 0)
        };
        let private_use = (0xE000..=0xF8FF).contains(&(ch as u32));
        let mut found = None;
        if private_use {
            found = find(self.ext.as_ref()).map(|g| (FaceId::Ext, g));
        }
        if found.is_none() {
            found = find(Some(&self.std)).map(|g| (FaceId::Standard, g));
        }
        if found.is_none() && !private_use {
            if !self.cn_tried {
                self.cn_tried = true;
                self.cn = shared_face(PL_CHINESE_SIMPLIFIED);
            }
            found = find(self.cn.as_ref()).map(|g| (FaceId::Chinese, g));
        }
        self.chars.insert(ch, found.map(|(f, g)| (f, g.0)));
        found
    }

    fn scale(&self, face: FaceId, px: f32) -> f32 {
        self.face(face).map_or(0.0, |f| px / f.units_per_em() as f32)
    }

    fn advance(&self, face: FaceId, gid: ttf_parser::GlyphId, px: f32) -> f32 {
        let s = self.scale(face, px);
        self.face(face)
            .and_then(|f| f.glyph_hor_advance(gid))
            .map_or(0.0, |a| a as f32 * s)
    }

    /// Pair kerning from the `kern` table, when the font has one. A font that
    /// kerns only through GPOS gets none: that needs shaping, and interface
    /// labels do not need it enough to bring a shaper in.
    fn kern(&self, face: FaceId, l: ttf_parser::GlyphId, r: ttf_parser::GlyphId, px: f32) -> f32 {
        let Some(f) = self.face(face) else { return 0.0 };
        let Some(table) = f.tables().kern else { return 0.0 };
        for st in table.subtables {
            if st.horizontal && !st.variable {
                if let Some(v) = st.glyphs_kerning(l, r) {
                    return v as f32 * self.scale(face, px);
                }
            }
        }
        0.0
    }

    fn ascent(&self, px: f32) -> f32 {
        self.std.ascender() as f32 * self.scale(FaceId::Standard, px)
    }

    /// Height of a capital, what centring a label on a line actually wants.
    fn cap_height(&self, px: f32) -> f32 {
        match self.std.capital_height() {
            Some(c) if c > 0 => c as f32 * self.scale(FaceId::Standard, px),
            _ => px * 0.7,
        }
    }

    fn layout(&mut self, text: &str, px: f32) -> (std::vec::Vec<Placed>, f32) {
        let mut out = std::vec::Vec::with_capacity(text.len());
        let mut pen = 0.0f32;
        let mut prev: Option<(FaceId, ttf_parser::GlyphId)> = None;
        for ch in text.chars() {
            let Some((face, gid)) = self.resolve(ch) else {
                // No font has it. A gap, not a box: one stray symbol in a title
                // should not shout.
                pen += px * 0.3;
                prev = None;
                continue;
            };
            if let Some((pf, pg)) = prev {
                if pf == face {
                    pen += self.kern(face, pg, gid, px);
                }
            }
            out.push(Placed { face, gid, x: pen });
            pen += self.advance(face, gid, px);
            prev = Some((face, gid));
        }
        (out, pen)
    }

    fn grow(&mut self) -> bool {
        if self.texs.len() >= ATLAS_MAX {
            return false;
        }
        let Some(tex) = new_atlas_texture() else {
            return false;
        };
        self.tex = tex;
        self.texs.push(tex);
        self.pen_x = PAD;
        self.pen_y = PAD;
        self.shelf_h = 0;
        true
    }

    /// The glyph's bitmap at `px`, rasterized and packed on first use. Sets
    /// `*uploaded` when it wrote a texture behind the GL state cache's back.
    fn ensure(
        &mut self,
        face: FaceId,
        gid: ttf_parser::GlyphId,
        px: u16,
        uploaded: &mut bool,
    ) -> Option<UiGlyph> {
        let key = (face, gid.0, px);
        if let Some(hit) = self.glyphs.get(&key) {
            return *hit;
        }
        let ink = match face {
            FaceId::Standard => Some(raster_gid_px(&self.std, &mut self.raster, gid, px as f32)),
            FaceId::Ext => self
                .ext
                .as_ref()
                .map(|f| raster_gid_px(f, &mut self.raster, gid, px as f32)),
            FaceId::Chinese => self
                .cn
                .as_ref()
                .map(|f| raster_gid_px(f, &mut self.raster, gid, px as f32)),
        };
        let Some((gw, gh, xmin, ymin, cov)) = ink else {
            self.glyphs.insert(key, None);
            return None;
        };
        if gw == 0 || gh == 0 || cov.is_empty() {
            let info = UiGlyph { tex: self.tex, uv: [0.0; 4], w: 0.0, h: 0.0, xmin, ymin, blank: true };
            self.glyphs.insert(key, Some(info));
            return Some(info);
        }
        if self.full {
            self.glyphs.insert(key, None);
            return None;
        }
        if self.pen_x + gw + PAD > ATLAS_DIM {
            self.pen_x = PAD;
            self.pen_y += self.shelf_h + PAD;
            self.shelf_h = 0;
        }
        if self.pen_y + gh + PAD > ATLAS_DIM && !self.grow() {
            self.full = true;
            log_str("kit: glyph atlases full\n");
            self.glyphs.insert(key, None);
            return None;
        }
        if self.pen_x + gw + PAD > ATLAS_DIM || self.pen_y + gh + PAD > ATLAS_DIM {
            self.glyphs.insert(key, None);
            return None;
        }
        let (x0, y0) = (self.pen_x, self.pen_y);
        // White with the coverage as alpha, so the draw colour tints it.
        let mut rgba = std::vec![255u8; gw * gh * 4];
        for (i, a) in cov.iter().enumerate() {
            rgba[i * 4 + 3] = *a;
        }
        unsafe {
            glBindTexture(GL_TEXTURE_2D, self.tex);
            glPixelStorei(GL_UNPACK_ALIGNMENT, 1);
            glTexSubImage2D(
                GL_TEXTURE_2D,
                0,
                x0 as GLint,
                y0 as GLint,
                gw as GLsizei,
                gh as GLsizei,
                GL_RGBA,
                GL_UNSIGNED_BYTE,
                rgba.as_ptr() as *const _,
            );
        }
        *uploaded = true;
        self.pen_x += gw + PAD;
        self.shelf_h = self.shelf_h.max(gh);
        let dim = ATLAS_DIM as f32;
        let info = UiGlyph {
            tex: self.tex,
            uv: [x0 as f32 / dim, y0 as f32 / dim, gw as f32 / dim, gh as f32 / dim],
            w: gw as f32,
            h: gh as f32,
            xmin,
            ymin,
            blank: false,
        };
        self.glyphs.insert(key, Some(info));
        Some(info)
    }
}

impl Drop for UiText {
    fn drop(&mut self) {
        for tex in &self.texs {
            if *tex != 0 {
                unsafe { glDeleteTextures(1, tex) };
            }
        }
    }
}

fn quad(x0: f32, y0: f32, w: f32, h: f32, uv: [f32; 4]) -> [f32; 24] {
    let (x1, y1) = (x0 + w, y0 + h);
    let (s0, t0, s1, t1) = (uv[0], uv[1], uv[0] + uv[2], uv[1] + uv[3]);
    #[rustfmt::skip]
    let q = [
        x0, y0, s0, t0,
        x1, y0, s1, t0,
        x1, y1, s1, t1,
        x0, y0, s0, t0,
        x1, y1, s1, t1,
        x0, y1, s0, t1,
    ];
    q
}

impl SwitchRenderBackend {
    /// The kit's text state, built on first use. `None` = no shared font, and
    /// every kit text call falls back to the old 5x7 font.
    fn kit_fonts(&mut self) -> Option<&mut UiText> {
        if !self.kit_text_tried {
            self.kit_text_tried = true;
            self.kit_text = UiText::new().map(std::boxed::Box::new);
            // Building the atlas bound a texture with raw GL.
            self.gl_state.invalidate();
        }
        self.kit_text.as_deref_mut()
    }

    /// Width of `text` at `px`, exactly as `kit_text` will lay it out.
    pub fn kit_measure(&mut self, text: &str, px: f32) -> f32 {
        match self.kit_fonts() {
            Some(t) => t.layout(text, px).1,
            None => self.measure_text(text, (px / 9.0).max(1.0)),
        }
    }

    /// `text`, shortened with an ellipsis until it fits `max_w`.
    pub fn kit_fit(&mut self, text: &str, px: f32, max_w: f32) -> std::string::String {
        if self.kit_measure(text, px) <= max_w {
            return text.to_string();
        }
        const ELL: &str = "\u{2026}";
        let budget = max_w - self.kit_measure(ELL, px);
        let mut out = std::string::String::new();
        for ch in text.chars() {
            out.push(ch);
            if self.kit_measure(&out, px) > budget {
                out.pop();
                break;
            }
        }
        let trimmed = out.trim_end().to_string();
        trimmed + ELL
    }

    /// `text`, with characters dropped from the MIDDLE until it fits `max_w`.
    /// For game names: what tells two episodes of a series apart is at the end.
    pub fn kit_fit_mid(&mut self, text: &str, px: f32, max_w: f32) -> std::string::String {
        if self.kit_measure(text, px) <= max_w {
            return text.to_string();
        }
        let chars: std::vec::Vec<char> = text.chars().collect();
        let mut keep = chars.len();
        while keep > 1 {
            keep -= 1;
            let head = (keep + 1) / 2;
            let tail = keep - head;
            let mut s: std::string::String = chars[..head].iter().collect();
            let s_trim = s.trim_end().len();
            s.truncate(s_trim);
            s.push('\u{2026}');
            s.extend(chars[chars.len() - tail..].iter().skip_while(|c| c.is_whitespace()));
            if self.kit_measure(&s, px) <= max_w {
                return s;
            }
        }
        std::string::String::from("\u{2026}")
    }

    /// The favourite marker: a gold star on a dark chip, for the top-left
    /// corner of a cover.
    pub fn kit_favorite_chip(&mut self, x: f32, y: f32) {
        use super::theme::col;
        const CHIP: f32 = 26.0;
        const STAR: f32 = 16.0;
        self.kit_rrect(x, y, CHIP, CHIP, 8.0, 0xB8_0A_10_1C);
        let w = self.kit_measure("\u{2605}", STAR);
        self.kit_text_mid((x + (CHIP - w) * 0.5).round(), y + CHIP * 0.5, STAR, "\u{2605}", col::GOLD);
    }

    /// Draw `text` with its baseline at `baseline`; returns its width.
    pub fn kit_text_baseline(&mut self, x: f32, baseline: f32, px: f32, text: &str, rgba: u32) -> f32 {
        let pxi = px.round().clamp(1.0, 200.0) as u16;
        let mut uploaded = false;
        let mut batches: std::vec::Vec<(GLuint, std::vec::Vec<f32>)> = std::vec::Vec::new();
        let width;
        {
            let Some(t) = self.kit_fonts() else {
                let s = (px / 9.0).max(1.0);
                self.draw_text(x, baseline - 7.0 * s, s, text, swf::Color::from_rgba(rgba));
                return self.measure_text(text, s);
            };
            let (placed, w) = t.layout(text, px);
            width = w;
            let by = baseline.round();
            for p in placed {
                let Some(g) = t.ensure(p.face, p.gid, pxi, &mut uploaded) else { continue };
                if g.blank {
                    continue;
                }
                let gx = (x + p.x).round() + g.xmin;
                let gy = by - (g.ymin + g.h);
                let q = quad(gx, gy, g.w, g.h, g.uv);
                match batches.iter_mut().find(|(tex, _)| *tex == g.tex) {
                    Some((_, v)) => v.extend_from_slice(&q),
                    None => batches.push((g.tex, q.to_vec())),
                }
            }
        }
        if uploaded {
            self.gl_state.invalidate();
        }
        let color = swf::Color::from_rgba(rgba);
        for (tex, mut verts) in batches {
            self.flush_atlas_quads(&mut verts, tex, color);
        }
        width
    }

    /// Draw `text` with the top of its line box at `top`; returns its width.
    pub fn kit_text(&mut self, x: f32, top: f32, px: f32, text: &str, rgba: u32) -> f32 {
        let ascent = match self.kit_fonts() {
            Some(t) => t.ascent(px),
            None => px * 0.8,
        };
        self.kit_text_baseline(x, top + ascent, px, text, rgba)
    }

    /// Draw `text` centred vertically on `cy` (on its capitals); returns its
    /// width.
    pub fn kit_text_mid(&mut self, x: f32, cy: f32, px: f32, text: &str, rgba: u32) -> f32 {
        let cap = match self.kit_fonts() {
            Some(t) => t.cap_height(px),
            None => px * 0.7,
        };
        self.kit_text_baseline(x, cy + cap * 0.5, px, text, rgba)
    }

    /// Width a button glyph takes at `px`.
    pub fn kit_button_width(&mut self, b: Btn, px: f32) -> f32 {
        let ch = b.glyph();
        match self.kit_fonts() {
            Some(t) => match t.resolve(ch) {
                Some((face, gid)) if face == FaceId::Ext => t.advance(face, gid, px),
                _ => px,
            },
            None => px,
        }
    }

    /// Draw one button glyph centred on `cy`, its left edge at `x`; returns the
    /// width it took. The glyph is centred on its own ink rather than on the
    /// text line, because a circle and a letter do not share a middle.
    pub fn kit_button(&mut self, x: f32, cy: f32, px: f32, b: Btn, rgba: u32) -> f32 {
        let pxi = px.round().clamp(1.0, 200.0) as u16;
        let mut uploaded = false;
        let drawn = {
            match self.kit_fonts() {
                Some(t) => match t.resolve(b.glyph()) {
                    Some((face, gid)) if face == FaceId::Ext => {
                        let adv = t.advance(face, gid, px);
                        t.ensure(face, gid, pxi, &mut uploaded).map(|g| (g, adv))
                    }
                    _ => None,
                },
                None => None,
            }
        };
        if uploaded {
            self.gl_state.invalidate();
        }
        match drawn {
            Some((g, adv)) if !g.blank => {
                let gx = (x + (adv - g.w) * 0.5).round();
                let gy = (cy - g.h * 0.5).round();
                let mut verts = quad(gx, gy, g.w, g.h, g.uv).to_vec();
                self.flush_atlas_quads(&mut verts, g.tex, swf::Color::from_rgba(rgba));
                adv
            }
            _ => {
                // No NintendoExt: a disc with the letter knocked out in the page
                // colour, which is what the glyph looks like anyway.
                let d = (px * 0.92).round();
                let r = d * 0.5;
                self.kit_rrect(x, cy - r, d, d, r, rgba);
                let lp = (px * 0.55).round();
                let lw = self.kit_measure(b.letter(), lp);
                self.kit_text_mid(x + (d - lw) * 0.5, cy, lp, b.letter(), super::theme::col::BG);
                d
            }
        }
    }

    /// A row of "glyph + verb" hints ending at `right`, centred on `cy`.
    ///
    /// The old footers were one translated sentence per screen
    /// ("A:LANCER   +:OPTIONS..."), nine languages each. A hint is now a button
    /// and a word, so a screen lists what its buttons do and the layout is
    /// shared.
    pub fn kit_hints_right(&mut self, right: f32, cy: f32, items: &[(Btn, &str)]) {
        use super::theme::{col, size};
        const GLYPH_GAP: f32 = 9.0;
        const ITEM_GAP: f32 = 26.0;
        let mut widths = std::vec::Vec::with_capacity(items.len());
        let mut total = 0.0;
        for (i, (b, label)) in items.iter().enumerate() {
            let w = self.kit_button_width(*b, size::GLYPH) + GLYPH_GAP + self.kit_measure(label, size::HINT);
            widths.push(w);
            total += w + if i > 0 { ITEM_GAP } else { 0.0 };
        }
        let mut x = right - total;
        for (i, (b, label)) in items.iter().enumerate() {
            let gw = self.kit_button(x, cy, size::GLYPH, *b, col::GLYPH);
            self.kit_text_mid(x + gw + GLYPH_GAP, cy, size::HINT, label, col::TEXT);
            x += widths[i] + ITEM_GAP;
        }
    }
}
