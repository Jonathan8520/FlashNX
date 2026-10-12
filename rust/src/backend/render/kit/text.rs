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
//!
//! Two halves. The METRICS (which font has a character, how wide it is) are
//! process-wide, because `render.rs` measures text from free functions that have
//! no renderer at hand, and every measuring helper there has to agree with what
//! gets drawn. The ATLAS is per renderer, because it is GL textures.
//!
//! No kerning, on purpose: the old helpers measure a string as the sum of its
//! characters' widths, and drawing exactly that sum is what keeps centring,
//! cutting and wrapping right everywhere they are used.

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
    /// The interface icons (`kit/icons.rs`), compiled into the binary. Never
    /// reached through a string: its code points share the Private Use Area
    /// with NintendoExt's buttons.
    Icons,
}

/// One hint in a bar: the button(s) and what pressing them does.
pub(crate) type HintItem<'a> = (&'a [Btn], &'a str);

/// A controller button, drawn with the console's own glyph for it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
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
    /// The d-pad, for "change a value" hints.
    Dpad,
    /// The d-pad with one arm lit: a control that IS one of its directions.
    DpadUp,
    DpadDown,
    DpadLeft,
    DpadRight,
    /// A stick, to move: the framing hint, and the four direction rows of the
    /// controls editor.
    LStick,
    RStick,
    /// A stick pushed in (L3 / R3), a button of its own.
    LStickPress,
    RStickPress,
    /// The rail buttons of a Joy-Con held on its own, sideways.
    SL,
    SR,
}

impl Btn {
    /// Code points in NintendoExt, as the homebrews that use the shared font
    /// map them (ReiNX Toolkit's `Font.hpp`, sphaira's `nvg_util.cpp`).
    ///
    /// The sticks were taken from Borealis, whose LSTICK is the stick CLICK:
    /// U+E104 / U+E105 are the sticks pressed in, U+E101 / U+E102 the sticks
    /// themselves. With the click glyph on the four direction rows, the editor
    /// showed a press icon beside an arrow, which is what Jonathan asked about.
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
            Btn::SL => '\u{E0E8}',
            Btn::SR => '\u{E0E9}',
            Btn::Plus => '\u{E0EF}',
            Btn::Minus => '\u{E0F0}',
            Btn::Dpad => '\u{E0EA}',
            Btn::DpadUp => '\u{E0EB}',
            Btn::DpadDown => '\u{E0EC}',
            Btn::DpadLeft => '\u{E0ED}',
            Btn::DpadRight => '\u{E0EE}',
            Btn::LStick => '\u{E101}',
            Btn::RStick => '\u{E102}',
            Btn::LStickPress => '\u{E104}',
            Btn::RStickPress => '\u{E105}',
        }
    }

    /// What to print inside a drawn disc when NintendoExt is unavailable.
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
            Btn::SL => "SL",
            Btn::SR => "SR",
            Btn::Plus => "+",
            Btn::Minus => "\u{2212}",
            Btn::Dpad => "\u{2195}",
            Btn::DpadUp => "\u{2191}",
            Btn::DpadDown => "\u{2193}",
            Btn::DpadLeft => "\u{2190}",
            Btn::DpadRight => "\u{2192}",
            Btn::LStick | Btn::LStickPress => "L",
            Btn::RStick | Btn::RStickPress => "R",
        }
    }
}

/// The button glyphs this file draws, to say in the log which ones the
/// console's NintendoExt lacks (`kit_button` then draws a disc with a letter).
const LOGGED_GLYPHS: [char; 8] =
    ['\u{E0E8}', '\u{E0E9}', '\u{E0EB}', '\u{E0EE}', '\u{E101}', '\u{E102}', '\u{E104}', '\u{E105}'];

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

// ─── Metrics, process-wide ──────────────────────────────────────────────────

struct Faces {
    std: ttf_parser::Face<'static>,
    ext: Option<ttf_parser::Face<'static>>,
    icons: Option<ttf_parser::Face<'static>>,
    cn: Option<ttf_parser::Face<'static>>,
    cn_tried: bool,
    /// Which face and glyph a character resolved to, misses included, so the
    /// fallback chain runs once per character.
    chars: HashMap<char, Option<(FaceId, u16)>>,
}

impl Faces {
    fn face(&self, id: FaceId) -> Option<&ttf_parser::Face<'static>> {
        match id {
            FaceId::Standard => Some(&self.std),
            FaceId::Ext => self.ext.as_ref(),
            FaceId::Chinese => self.cn.as_ref(),
            FaceId::Icons => self.icons.as_ref(),
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

    fn advance_gid(&self, face: FaceId, gid: ttf_parser::GlyphId, px: f32) -> f32 {
        let s = self.scale(face, px);
        self.face(face)
            .and_then(|f| f.glyph_hor_advance(gid))
            .map_or(0.0, |a| a as f32 * s)
    }

    /// Width of one character. A character no font has still takes a little
    /// room: a gap reads better than a box, and better than letters colliding.
    fn advance(&mut self, ch: char, px: f32) -> f32 {
        match self.resolve(ch) {
            Some((face, gid)) => self.advance_gid(face, gid, px),
            None => px * 0.3,
        }
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
}

static FACES: std::sync::Mutex<Option<Faces>> = std::sync::Mutex::new(None);
/// 0 = not tried, 1 = ready, 2 = no Standard font (old 5x7 font everywhere).
static FACES_STATE: core::sync::atomic::AtomicU8 = core::sync::atomic::AtomicU8::new(0);

fn with_faces<R>(f: impl FnOnce(&mut Faces) -> R) -> Option<R> {
    use core::sync::atomic::Ordering;
    if FACES_STATE.load(Ordering::Relaxed) == 2 {
        return None;
    }
    let mut guard = FACES.lock().ok()?;
    if guard.is_none() {
        if FACES_STATE.load(Ordering::Relaxed) != 0 {
            return None;
        }
        match shared_face(PL_STANDARD) {
            Some(std) => {
                let ext = shared_face(PL_NINTENDO_EXT);
                let lacks: std::vec::Vec<std::string::String> = ext
                    .as_ref()
                    .map(|e| {
                        LOGGED_GLYPHS
                            .iter()
                            .filter(|c| e.glyph_index(**c).map_or(true, |g| g.0 == 0))
                            .map(|c| std::format!("{:04X}", *c as u32))
                            .collect()
                    })
                    .unwrap_or_default();
                log_str(&std::format!(
                    "kit: fonts ready (standard {} upem, nintendo-ext {}{})\n",
                    std.units_per_em(),
                    if ext.is_some() { "yes" } else { "MISSING, glyphs drawn by hand" },
                    if lacks.is_empty() { std::string::String::new() } else { std::format!(", lacks {}", lacks.join(" ")) },
                ));
                let icons = ttf_parser::Face::parse(super::icons::ICON_FONT, 0).ok();
                *guard = Some(Faces { std, ext, icons, cn: None, cn_tried: false, chars: HashMap::new() });
                FACES_STATE.store(1, Ordering::Relaxed);
            }
            None => {
                log_str("kit: no shared Standard font, keeping the 5x7 font\n");
                FACES_STATE.store(2, Ordering::Relaxed);
                return None;
            }
        }
    }
    guard.as_mut().map(f)
}

/// Whether the console fonts are usable. `false` keeps every screen on the old
/// 5x7 font, which is still there for exactly that case.
pub(crate) fn fonts_available() -> bool {
    with_faces(|_| ()).is_some()
}

/// Width of `ch` at `px` in the console font, `None` without one.
pub(crate) fn advance_px(ch: char, px: f32) -> Option<f32> {
    with_faces(|f| f.advance(ch, px))
}

pub(crate) fn measure_px(text: &str, px: f32) -> Option<f32> {
    with_faces(|f| text.chars().map(|c| f.advance(c, px)).sum())
}

pub(crate) fn cap_height_px(px: f32) -> f32 {
    with_faces(|f| f.cap_height(px)).unwrap_or(px * 0.7)
}

pub(crate) fn ascent_px(px: f32) -> f32 {
    with_faces(|f| f.ascent(px)).unwrap_or(px * 0.8)
}

// ─── Atlas, per renderer ────────────────────────────────────────────────────

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

pub(crate) struct UiText {
    raster: ab_glyph_rasterizer::Rasterizer,
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
    fn new() -> Option<UiText> {
        if !fonts_available() {
            return None;
        }
        let tex = new_atlas_texture()?;
        Some(UiText {
            raster: ab_glyph_rasterizer::Rasterizer::new(0, 0),
            glyphs: HashMap::new(),
            tex,
            texs: std::vec![tex],
            pen_x: PAD,
            pen_y: PAD,
            shelf_h: 0,
            full: false,
        })
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
    fn ensure(&mut self, face: FaceId, gid: ttf_parser::GlyphId, px: u16, uploaded: &mut bool) -> Option<UiGlyph> {
        let key = (face, gid.0, px);
        if let Some(hit) = self.glyphs.get(&key) {
            return *hit;
        }
        let raster = &mut self.raster;
        let ink = with_faces(|f| f.face(face).map(|fc| raster_gid_px(fc, raster, gid, px as f32))).flatten();
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
    /// The kit's atlas, built on first use. `None` = no console font.
    fn kit_atlas(&mut self) -> Option<&mut UiText> {
        if !self.kit_text_tried {
            self.kit_text_tried = true;
            self.kit_text = UiText::new().map(std::boxed::Box::new);
            // Building the atlas bound a texture with raw GL.
            self.gl_state.invalidate();
        }
        self.kit_text.as_deref_mut()
    }

    /// Width of `text` at `px`, exactly as the kit draws it.
    pub fn kit_measure(&mut self, text: &str, px: f32) -> f32 {
        match measure_px(text, px) {
            Some(w) => w,
            None => self.measure_text(text, (px / 8.5).max(1.0)),
        }
    }

    /// `text`, shortened with an ellipsis until it fits `max_w`.
    pub fn kit_fit(&mut self, text: &str, px: f32, max_w: f32) -> std::string::String {
        if self.kit_measure(text, px) <= max_w {
            return text.to_string();
        }
        const ELL: char = '\u{2026}';
        let budget = max_w - advance_px(ELL, px).unwrap_or(px * 0.6);
        let mut out = std::string::String::new();
        let mut w = 0.0;
        for ch in text.chars() {
            let cw = advance_px(ch, px).unwrap_or(px * 0.5);
            if w + cw > budget {
                break;
            }
            out.push(ch);
            w += cw;
        }
        let mut out = out.trim_end().to_string();
        out.push(ELL);
        out
    }

    /// `text` on at most `max_lines` lines of `max_w`, broken on spaces (or
    /// anywhere, for a word wider than a line); the last line is ellipsised.
    pub fn kit_wrap(&mut self, text: &str, px: f32, max_w: f32, max_lines: usize) -> std::vec::Vec<std::string::String> {
        let mut lines: std::vec::Vec<std::string::String> = std::vec::Vec::new();
        let mut cur = std::string::String::new();
        let mut words = text.split_whitespace().peekable();
        while let Some(word) = words.next() {
            let candidate = if cur.is_empty() { word.to_string() } else { std::format!("{} {}", cur, word) };
            if self.kit_measure(&candidate, px) <= max_w {
                cur = candidate;
                continue;
            }
            if !cur.is_empty() {
                lines.push(core::mem::take(&mut cur));
            }
            if lines.len() + 1 >= max_lines {
                // Last line: everything left, ellipsised.
                let rest: std::vec::Vec<&str> = core::iter::once(word).chain(words.by_ref()).collect();
                lines.push(self.kit_fit(&rest.join(" "), px, max_w));
                return lines;
            }
            if self.kit_measure(word, px) > max_w {
                // One word wider than a line: cut it where it overflows.
                let mut chunk = std::string::String::new();
                for ch in word.chars() {
                    chunk.push(ch);
                    if self.kit_measure(&chunk, px) > max_w {
                        chunk.pop();
                        lines.push(core::mem::take(&mut chunk));
                        chunk.push(ch);
                        if lines.len() + 1 >= max_lines {
                            break;
                        }
                    }
                }
                cur = chunk;
            } else {
                cur = word.to_string();
            }
        }
        if !cur.is_empty() {
            lines.push(if lines.len() + 1 >= max_lines { self.kit_fit(&cur, px, max_w) } else { cur });
        }
        lines
    }

    /// `text`, with characters dropped from the MIDDLE until it fits `max_w`.
    /// For game names: what tells two episodes of a series apart is at the end.
    pub fn kit_fit_mid(&mut self, text: &str, px: f32, max_w: f32) -> std::string::String {
        if self.kit_measure(text, px) <= max_w {
            return text.to_string();
        }
        let chars: std::vec::Vec<char> = text.chars().collect();
        let mut prefix = std::vec::Vec::with_capacity(chars.len() + 1);
        prefix.push(0.0f32);
        for c in &chars {
            let w = prefix[prefix.len() - 1] + advance_px(*c, px).unwrap_or(px * 0.5);
            prefix.push(w);
        }
        let total = prefix[chars.len()];
        let ell = advance_px('\u{2026}', px).unwrap_or(px * 0.6);
        for keep in (0..chars.len()).rev() {
            let head = (keep + 1) / 2;
            let tail = keep - head;
            if prefix[head] + ell + (total - prefix[chars.len() - tail]) <= max_w {
                let mut out: std::string::String = chars[..head].iter().collect();
                let t = out.trim_end().len();
                out.truncate(t);
                out.push('\u{2026}');
                out.extend(chars[chars.len() - tail..].iter().skip_while(|c| c.is_whitespace()));
                return out;
            }
        }
        std::string::String::from("\u{2026}")
    }

    /// Draw `text` with its baseline at `baseline`; returns its width.
    pub fn kit_text_baseline(&mut self, x: f32, baseline: f32, px: f32, text: &str, rgba: u32) -> f32 {
        let pxi = px.round().clamp(1.0, 200.0) as u16;
        if self.kit_atlas().is_none() {
            let s = (px / 8.5).max(1.0);
            self.draw_text_5x7(x, baseline - 7.0 * s, s, text, swf::Color::from_rgba(rgba));
            return self.measure_text(text, s);
        }
        // Resolve and measure first (metrics lock), then rasterize (atlas).
        let placed: std::vec::Vec<(FaceId, ttf_parser::GlyphId, f32)> = with_faces(|f| {
            let mut out = std::vec::Vec::with_capacity(text.len());
            let mut pen = 0.0f32;
            for ch in text.chars() {
                match f.resolve(ch) {
                    Some((face, gid)) => {
                        out.push((face, gid, pen));
                        pen += f.advance_gid(face, gid, px);
                    }
                    None => pen += px * 0.3,
                }
            }
            out.push((FaceId::Standard, ttf_parser::GlyphId(0), pen));
            out
        })
        .unwrap_or_default();
        let width = placed.last().map_or(0.0, |p| p.2);
        let mut uploaded = false;
        let mut batches: std::vec::Vec<(GLuint, std::vec::Vec<f32>)> = std::vec::Vec::new();
        {
            let Some(t) = self.kit_atlas() else { return width };
            let by = baseline.round();
            for &(face, gid, pen) in placed.iter().take(placed.len().saturating_sub(1)) {
                let Some(g) = t.ensure(face, gid, pxi, &mut uploaded) else { continue };
                if g.blank {
                    continue;
                }
                let gx = (x + pen).round() + g.xmin;
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
        self.kit_text_baseline(x, top + ascent_px(px), px, text, rgba)
    }

    /// Draw `text` centred vertically on `cy` (on its capitals); returns its
    /// width.
    pub fn kit_text_mid(&mut self, x: f32, cy: f32, px: f32, text: &str, rgba: u32) -> f32 {
        self.kit_text_baseline(x, cy + cap_height_px(px) * 0.5, px, text, rgba)
    }

    /// Width a button glyph takes at `px`.
    pub fn kit_button_width(&mut self, b: Btn, px: f32) -> f32 {
        let ch = b.glyph();
        with_faces(|f| match f.resolve(ch) {
            Some((face, gid)) if face == FaceId::Ext => f.advance_gid(face, gid, px),
            _ => (px * 0.92).round(),
        })
        .unwrap_or((px * 0.92).round())
    }

    /// Draw one button glyph centred on `cy`, its left edge at `x`; returns the
    /// width it took. The glyph is centred on its own ink rather than on the
    /// text line, because a circle and a letter do not share a middle.
    pub fn kit_button(&mut self, x: f32, cy: f32, px: f32, b: Btn, rgba: u32) -> f32 {
        let pxi = px.round().clamp(1.0, 200.0) as u16;
        let found = with_faces(|f| match f.resolve(b.glyph()) {
            Some((face, gid)) if face == FaceId::Ext => Some((face, gid, f.advance_gid(face, gid, px))),
            _ => None,
        })
        .flatten();
        let mut uploaded = false;
        let drawn = match (found, self.kit_atlas()) {
            (Some((face, gid, adv)), Some(t)) => t.ensure(face, gid, pxi, &mut uploaded).map(|g| (g, adv)),
            _ => None,
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

    /// Width of one hint: its glyphs, then its label.
    fn kit_hint_width(&mut self, btns: &[Btn], label: &str) -> f32 {
        use super::theme::{size, space};
        let mut w = 0.0;
        for (i, b) in btns.iter().enumerate() {
            if i > 0 {
                w += space::GLYPH_PAIR_GAP;
            }
            w += self.kit_button_width(*b, size::GLYPH);
        }
        if !btns.is_empty() && !label.is_empty() {
            w += space::GLYPH_GAP;
        }
        w + self.kit_measure(label, size::HINT)
    }

    /// Draw an interface icon in the `px` x `px` box whose left edge is `x`,
    /// centred on `cy`; returns `px`, the width it takes. The icon font's em is
    /// the icon's box (see `kit/icons.rs`), so the bitmap goes where its
    /// bearings say.
    pub fn kit_icon(&mut self, x: f32, cy: f32, px: f32, icon: super::icons::Icon, rgba: u32) -> f32 {
        let pxi = px.round().clamp(1.0, 200.0) as u16;
        let gid = with_faces(|f| f.icons.as_ref().and_then(|face| face.glyph_index(icon.codepoint()))).flatten();
        let Some(gid) = gid else { return px };
        let mut uploaded = false;
        let drawn = match self.kit_atlas() {
            Some(t) => t.ensure(FaceId::Icons, gid, pxi, &mut uploaded),
            None => None,
        };
        if uploaded {
            self.gl_state.invalidate();
        }
        if let Some(g) = drawn.filter(|g| !g.blank) {
            let top = (cy - px * 0.5).round();
            let gx = x.round() + g.xmin;
            let gy = top + px - (g.ymin + g.h);
            let mut verts = quad(gx, gy, g.w, g.h, g.uv).to_vec();
            self.flush_atlas_quads(&mut verts, g.tex, swf::Color::from_rgba(rgba));
        }
        px
    }

    /// Width of a hint bar, so a caller can centre or right-align it.
    pub fn kit_hints_width(&mut self, items: &[HintItem]) -> f32 {
        use super::theme::space;
        let mut total = 0.0;
        for (i, (btns, label)) in items.iter().enumerate() {
            if i > 0 {
                total += space::HINT_GAP;
            }
            total += self.kit_hint_width(btns, label);
        }
        total
    }

    /// A row of "glyph + verb" hints starting at `x`, centred on `cy`.
    ///
    /// The old footers were one translated sentence per screen
    /// ("A:LANCER   +:OPTIONS..."), nine languages each. A hint is now a button
    /// and a word, so a screen lists what its buttons do and the layout is
    /// shared.
    pub fn kit_hints(&mut self, x: f32, cy: f32, items: &[HintItem]) {
        use super::theme::{col, size, space};
        let mut x = x;
        for (btns, label) in items.iter() {
            for (i, b) in btns.iter().enumerate() {
                if i > 0 {
                    x += space::GLYPH_PAIR_GAP;
                }
                x += self.kit_button(x, cy, size::GLYPH, *b, col::GLYPH);
            }
            if !btns.is_empty() && !label.is_empty() {
                x += space::GLYPH_GAP;
            }
            x += self.kit_text_mid(x, cy, size::HINT, label, col::TEXT) + space::HINT_GAP;
        }
    }

    /// `kit_hints`, centred on `cx`.
    pub fn kit_hints_centered(&mut self, cx: f32, cy: f32, items: &[HintItem]) {
        let w = self.kit_hints_width(items);
        self.kit_hints((cx - w * 0.5).round(), cy, items);
    }

    /// `kit_hints`, ending at `right`.
    pub fn kit_hints_right(&mut self, right: f32, cy: f32, items: &[HintItem]) {
        let w = self.kit_hints_width(items);
        self.kit_hints(right - w, cy, items);
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
}
