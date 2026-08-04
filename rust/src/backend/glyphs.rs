//! CJK / Unicode glyph atlas.
//!
//! The hand-drawn 5x7 `GLYPHS` bitmap font in `render.rs` only carries Latin
//! + Cyrillic. For scripts with thousands of glyphs (Chinese now; Korean /
//! Japanese fall out of the same path later), we rasterize on demand from the
//! Switch **shared system font** — nothing is shipped in the .nro — into a GL
//! texture atlas, and `draw_text` falls back to a textured quad for any
//! codepoint the bitmap font lacks.
//!
//! Font source: libnx `pl` service, exposed via the C++ FFI
//! `ruffle_shared_font`. `plGetSharedFontByType` returns DECRYPTED TTF/OTF
//! bytes (the `pl` sysmodule unwraps Nintendo's BFTTF container for us), so
//! fontdue parses them directly — there is no XOR deobfuscation to do here.
//!
//! Glyphs are rasterized once at a fixed `RASTER_PX` and scaled with GL_LINEAR
//! at draw time. CJK is treated as full-width / monospace (`draw_text` uses a
//! fixed cell advance, not fontdue's per-glyph advance) so `measure_text` and
//! `draw_text` agree on widths without the atlas needing to exist yet.

use crate::ffi::gl::*;
use std::collections::HashMap;

/// Fixed pixel size glyphs are rasterized at; draw-time scaling is linear.
/// Larger = sharper when the UI draws big, heavier atlas. 44 fits ~500 CJK
/// cells in a 1024² atlas, plenty for the whole UI.
pub const RASTER_PX: f32 = 44.0;
/// Atlas texture side (RGBA8).
const ATLAS_DIM: usize = 1024;
/// 1px gap between packed glyphs so bilinear sampling never bleeds neighbours.
const PAD: usize = 1;

extern "C" {
    fn ruffle_log_cstr(msg: *const core::ffi::c_char);
    /// 1 = small applet memory pool, 0 = full title-takeover heap. The shared
    /// CJK font does not fit in the former (see `cjk_possible`).
    fn ruffle_is_applet_mode() -> core::ffi::c_int;
    /// Pointer to the DECRYPTED shared-font bytes for `kind`
    /// (1 = Chinese Simplified, matching `PlSharedFontType`), with the length
    /// written to `out_size`; null if the font service is unavailable.
    /// Defined in cpp/src/ruffle_bridge.cpp (libnx `pl`).
    fn ruffle_shared_font(kind: core::ffi::c_int, out_size: *mut u32) -> *const u8;
}

/// Headroom `fontdue` needs to expand the shared CJK font. Probed with a
/// FALLIBLE allocation because the parse itself has none: an allocator failure
/// inside `fontdue::Font::from_bytes` aborts the process, which on hardware is
/// an Atmosphere fatal (2168-0002, measured in applet mode). `query_ram` cannot
/// answer this, it reports the heap crt0 reserved rather than what is live.
const FONT_PARSE_HEADROOM: usize = 96 * 1024 * 1024;

/// Can this process afford the CJK font? Probed once and cached: the answer
/// decides both whether the atlas is built and whether a CJK UI language is
/// offered at all, and those two must never disagree.
///
/// 0 = not probed, 1 = yes, 2 = no.
static CJK_POSSIBLE: core::sync::atomic::AtomicU8 = core::sync::atomic::AtomicU8::new(0);

pub fn cjk_possible() -> bool {
    match CJK_POSSIBLE.load(core::sync::atomic::Ordering::Relaxed) {
        1 => return true,
        2 => return false,
        _ => {}
    }
    // APPLET MODE IS REFUSED OUTRIGHT, not probed.
    //
    // Measured on hardware: with the UI already set to Chinese, the atlas is
    // built on the FIRST FRAME, while the heap is still nearly empty. The
    // free-memory probe below therefore succeeded, fontdue then asked for more
    // than it had reserved, and the process aborted into an Atmosphere fatal.
    // Opening the language picker later, with a loaded heap, made the same
    // probe fail and looked "fixed" - the guard was only ever passing by
    // accident of timing.
    //
    // A free-memory probe cannot answer this: it measures the moment it runs,
    // not the peak of a parse that has no fallible allocation. The applet pool
    // is small enough that the answer is no regardless of when we ask, so ask
    // the mode instead. Applet mode cannot launch games anyway.
    let applet = unsafe { ruffle_is_applet_mode() } != 0;
    let (used, total) = crate::query_ram();
    let ok = if applet {
        false
    } else {
        let mut probe: std::vec::Vec<u8> = std::vec::Vec::new();
        probe.try_reserve_exact(FONT_PARSE_HEADROOM).is_ok()
    };
    log(&std::format!(
        "glyphs: cjk_possible={} (applet={}, pool {}/{} KB)\n",
        ok, applet, used / 1024, total / 1024,
    ));
    CJK_POSSIBLE.store(if ok { 1 } else { 2 }, core::sync::atomic::Ordering::Relaxed);
    ok
}

/// Flush-per-line logging, so the last line printed before a fatal is the step
/// that died (the C++ side fflushes every call).
fn log(s: &str) {
    let mut bytes = s.as_bytes().to_vec();
    bytes.push(0);
    unsafe { ruffle_log_cstr(bytes.as_ptr() as *const _) };
}

/// Atlas placement + metrics for one rasterized glyph, all at `RASTER_PX`.
#[derive(Clone, Copy)]
pub struct GlyphInfo {
    /// Atlas UV sub-rect `[u0, v0, du, dv]`.
    pub uv: [f32; 4],
    /// Rasterized bitmap size in px (at `RASTER_PX`).
    pub w: f32,
    pub h: f32,
    /// fontdue bearings (px at `RASTER_PX`): `xmin` = left bearing,
    /// `ymin` = baseline-to-bitmap-bottom (y-up).
    pub xmin: f32,
    pub ymin: f32,
    /// True when the glyph has no ink (e.g. an ideographic space): the caller
    /// still advances the pen but draws nothing.
    pub blank: bool,
}

/// One GL texture holding lazily-rasterized glyphs, packed shelf-style.
pub struct FontAtlas {
    font: fontdue::Font,
    tex: GLuint,
    /// `None` value = looked up and the font has no such glyph (or atlas full):
    /// caches the miss so we never re-rasterize it.
    glyphs: HashMap<char, Option<GlyphInfo>>,
    pen_x: usize,
    pen_y: usize,
    shelf_h: usize,
    full: bool,
}

impl FontAtlas {
    /// Build from the Switch Chinese-Simplified shared font. Returns `None` if
    /// the font service is unavailable or the bytes don't parse — the caller
    /// then renders CJK as blanks (graceful, no crash).
    pub fn new() -> Option<FontAtlas> {
        // Opening the language picker in APPLET mode took the whole console
        // down with an Atmosphere fatal (2168-0002). Measured on hardware: the
        // shared font maps fine and reads fine, and the process dies inside
        // `fontdue::Font::from_bytes`, on a 2 KB allocation.
        //
        // fontdue expands the 7.8 MB CJK font into far more than its file size,
        // and Rust has no fallible allocation there: an allocator failure
        // inside it ABORTS the process. Applet mode's pool cannot hold it.
        //
        // So probe for the headroom with an allocation that is ALLOWED to fail,
        // and draw CJK blank when it is not there. `query_ram` is not usable
        // for this: it reports the heap crt0 reserved, not what is live.
        // A language name rendered blank is a bad screen; a fatal is a console
        // reboot and a lost session.
        if !cjk_possible() {
            log("glyphs: not enough memory to parse the CJK font — drawing it blank\n");
            return None;
        }
        let bytes: &'static [u8] = unsafe {
            let mut len: u32 = 0;
            let ptr = ruffle_shared_font(1 /* PlSharedFontType_ChineseSimplified */, &mut len);
            if ptr.is_null() || len == 0 {
                log("glyphs: shared font unavailable (null/0) — CJK will draw blank\n");
                return None;
            }
            core::slice::from_raw_parts(ptr, len as usize)
        };
        let font = match fontdue::Font::from_bytes(bytes, fontdue::FontSettings::default()) {
            Ok(f) => f,
            Err(_) => {
                log("glyphs: fontdue rejected the shared font\n");
                return None;
            }
        };

        let mut tex: GLuint = 0;
        unsafe {
            glGenTextures(1, &mut tex);
            if tex == 0 {
                log("glyphs: glGenTextures returned 0\n");
                return None;
            }
            glBindTexture(GL_TEXTURE_2D, tex);
            glPixelStorei(GL_UNPACK_ALIGNMENT, 1);
            // Start fully transparent so unwritten regions never show.
            let zeros = std::vec![0u8; ATLAS_DIM * ATLAS_DIM * 4];
            glTexImage2D(
                GL_TEXTURE_2D,
                0,
                GL_RGBA8 as GLint,
                ATLAS_DIM as GLsizei,
                ATLAS_DIM as GLsizei,
                0,
                GL_RGBA,
                GL_UNSIGNED_BYTE,
                zeros.as_ptr() as *const _,
            );
            glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, GL_LINEAR as GLint);
            glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER, GL_LINEAR as GLint);
            glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_WRAP_S, GL_CLAMP_TO_EDGE as GLint);
            glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_WRAP_T, GL_CLAMP_TO_EDGE as GLint);
            glBindTexture(GL_TEXTURE_2D, 0);
        }
        Some(FontAtlas {
            font,
            tex,
            glyphs: HashMap::new(),
            pen_x: PAD,
            pen_y: PAD,
            shelf_h: 0,
            full: false,
        })
    }

    pub fn texture(&self) -> GLuint {
        self.tex
    }

    /// Ensure `ch` is in the atlas and return its info. `None` = the font has
    /// no such glyph or the atlas is full (caller advances but draws nothing).
    /// Sets `*uploaded` when it bound + wrote the texture, so the caller can
    /// invalidate its GL state cache once per miss.
    pub fn ensure(&mut self, ch: char, uploaded: &mut bool) -> Option<GlyphInfo> {
        if let Some(cached) = self.glyphs.get(&ch) {
            return *cached;
        }
        // Missing glyph in this font → cache the miss, draw nothing (no tofu).
        if self.font.lookup_glyph_index(ch) == 0 {
            self.glyphs.insert(ch, None);
            return None;
        }
        let (metrics, coverage) = self.font.rasterize(ch, RASTER_PX);
        // Inkless (ideographic space, etc.): advance only.
        if metrics.width == 0 || metrics.height == 0 || coverage.is_empty() {
            let info = GlyphInfo {
                uv: [0.0; 4],
                w: 0.0,
                h: 0.0,
                xmin: metrics.xmin as f32,
                ymin: metrics.ymin as f32,
                blank: true,
            };
            self.glyphs.insert(ch, Some(info));
            return Some(info);
        }
        if self.full {
            self.glyphs.insert(ch, None);
            return None;
        }
        let gw = metrics.width;
        let gh = metrics.height;
        // Shelf pack: wrap to a new row when this glyph won't fit the current.
        if self.pen_x + gw + PAD > ATLAS_DIM {
            self.pen_x = PAD;
            self.pen_y += self.shelf_h + PAD;
            self.shelf_h = 0;
        }
        if self.pen_y + gh + PAD > ATLAS_DIM {
            self.full = true;
            self.glyphs.insert(ch, None);
            return None;
        }
        let x0 = self.pen_x;
        let y0 = self.pen_y;
        // fontdue gives 8-bit coverage; expand to white RGBA with alpha=coverage
        // so a draw with `mult = text colour` tints it (see draw_atlas_glyph).
        let mut rgba = std::vec![0u8; gw * gh * 4];
        for i in 0..gw * gh {
            rgba[i * 4] = 255;
            rgba[i * 4 + 1] = 255;
            rgba[i * 4 + 2] = 255;
            rgba[i * 4 + 3] = coverage[i];
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
        if gh > self.shelf_h {
            self.shelf_h = gh;
        }
        let dim = ATLAS_DIM as f32;
        let info = GlyphInfo {
            uv: [
                x0 as f32 / dim,
                y0 as f32 / dim,
                gw as f32 / dim,
                gh as f32 / dim,
            ],
            w: gw as f32,
            h: gh as f32,
            xmin: metrics.xmin as f32,
            ymin: metrics.ymin as f32,
            blank: false,
        };
        self.glyphs.insert(ch, Some(info));
        Some(info)
    }
}

impl Drop for FontAtlas {
    fn drop(&mut self) {
        if self.tex != 0 {
            unsafe { glDeleteTextures(1, &self.tex) };
        }
    }
}
