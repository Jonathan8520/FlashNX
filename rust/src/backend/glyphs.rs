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
/// How many atlas textures we will allocate before giving up on new glyphs.
///
/// One 1024² atlas holds 22 shelves of 22 cells at `RASTER_PX`, so about 484
/// distinct characters. That is plenty for a Chinese INTERFACE, and nowhere
/// near enough for a Chinese LIBRARY: the interface alone is a few hundred, and
/// every game title a player types in (issue #75) adds more. Past the 484th the
/// old single atlas went permanently blind -- not a fallback glyph, nothing at
/// all, for the rest of the session. Four textures is 16 MB of GPU memory for
/// ~1900 characters, and none of it is allocated until the first non-Latin
/// character is actually drawn.
const ATLAS_MAX: usize = 4;
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
    fn ruffle_tick_now() -> u64;
    fn ruffle_tick_freq() -> u64;
}

fn ms_since(t0: u64) -> u64 {
    let freq = unsafe { ruffle_tick_freq() };
    if freq == 0 {
        return 0;
    }
    unsafe { ruffle_tick_now() }.saturating_sub(t0) * 1000 / freq
}

/// Total ms spent rasterizing glyphs since boot, and how many. Reported next to
/// the parse cost so the two halves of "Chinese is slow" can be told apart:
/// one big one-off (fontdue unfolding every glyph in the font) versus a per-
/// character cost that the persistent `glyph_cache` already removes.
static RASTER_MS: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);
static RASTER_N: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

/// Log the running total every 50 glyphs — enough to see the shape of the cost
/// without a line per character. A whole UI is a few hundred glyphs, so this is
/// a handful of lines a session and none at all once the cache is warm.
fn note_raster(ms: u64) {
    RASTER_MS.fetch_add(ms, core::sync::atomic::Ordering::Relaxed);
    let n = RASTER_N.fetch_add(1, core::sync::atomic::Ordering::Relaxed) + 1;
    if n % 50 == 0 {
        log(&std::format!(
            "glyphs: rasterized {} glyphs, {} ms total\n",
            n,
            RASTER_MS.load(core::sync::atomic::Ordering::Relaxed),
        ));
    }
}

/// Headroom `fontdue` needs to expand the shared CJK font. Probed with a
/// FALLIBLE allocation because the parse itself has none: an allocator failure
/// inside `fontdue::Font::from_bytes` aborts the process, which on hardware is
/// an Atmosphere fatal (2168-0002, measured in applet mode).
///
/// 192 MB because the parse was MEASURED at a 136 MB peak (counted at the
/// global allocator; `query_ram` reads the heap crt0 reserved and reported +0).
/// The first value here was a guessed 96 MB, which would have let the probe
/// succeed and fontdue abort anyway. fontdue also retains nearly all of it: the
/// parsed `Font` holds those structures for as long as the atlas lives.
const FONT_PARSE_HEADROOM: usize = 192 * 1024 * 1024;

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
    /// Which atlas texture holds this glyph. Carried per glyph, not read from
    /// the atlas: once a second texture exists (see `ATLAS_MAX`) the characters
    /// of a single word can live on different ones.
    pub tex: GLuint,
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

/// One rasterized glyph's ink, kept ACROSS renderer lifetimes.
///
/// The atlas texture belongs to a renderer and dies with it, so every game quit
/// cost a full `fontdue::Font::from_bytes` over the 7.8 MB shared font — seconds
/// of spinner, every time, to redraw characters that had been rasterized minutes
/// earlier. Keeping the parsed font instead is not an option: it retains ~136 MB
/// (see `FONT_PARSE_HEADROOM`) and a game needs that heap far more than the menus
/// need instant Chinese. The COVERAGE is the cheap half — one byte per pixel,
/// ~2 KB a glyph, about a megabyte for a whole UI's worth — so the font stays
/// per-renderer and lazily parsed, and this survives everything.
struct CachedGlyph {
    /// 8-bit coverage, `w * h` bytes. Empty for an inkless glyph.
    cov: std::vec::Vec<u8>,
    w: usize,
    h: usize,
    xmin: f32,
    ymin: f32,
}

/// `None` value = the font has no such glyph. Cached like a hit so a miss never
/// re-parses the font either. A `Vec` rather than a map because `HashMap::new`
/// is not const, and this is only consulted when the per-renderer map misses.
fn glyph_cache() -> &'static std::sync::Mutex<std::vec::Vec<(char, Option<CachedGlyph>)>> {
    static C: std::sync::Mutex<std::vec::Vec<(char, Option<CachedGlyph>)>> =
        std::sync::Mutex::new(std::vec::Vec::new());
    &C
}

/// Allocate one empty `ATLAS_DIM²` RGBA8 texture. Cleared to transparent so an
/// unwritten region can never show through a glyph's bilinear edge.
///
/// NOTE for the caller: this binds and unbinds texture unit 0 with raw GL, so
/// the renderer's state cache must be invalidated afterwards.
fn new_atlas_texture() -> Option<GLuint> {
    let mut tex: GLuint = 0;
    unsafe {
        glGenTextures(1, &mut tex);
        if tex == 0 {
            log("glyphs: glGenTextures returned 0\n");
            return None;
        }
        glBindTexture(GL_TEXTURE_2D, tex);
        glPixelStorei(GL_UNPACK_ALIGNMENT, 1);
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
    Some(tex)
}

/// One GL texture holding lazily-rasterized glyphs, packed shelf-style.
pub struct FontAtlas {
    /// Parsed ON DEMAND, and only when `glyph_cache` cannot answer — see
    /// `CachedGlyph`. `None` = not parsed yet OR the parse failed.
    font: Option<fontdue::Font>,
    font_tried: bool,
    /// The texture being packed into now; `texs` holds it and every earlier one
    /// (all of them stay alive: their glyphs are still referenced).
    tex: GLuint,
    texs: std::vec::Vec<GLuint>,
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
        let tex = new_atlas_texture()?;
        Some(FontAtlas {
            font: None,
            font_tried: false,
            tex,
            texs: std::vec![tex],
            glyphs: HashMap::new(),
            pen_x: PAD,
            pen_y: PAD,
            shelf_h: 0,
            full: false,
        })
    }

    /// Move the pen to a fresh texture. `false` = no more are allowed or GL
    /// refused one, and the caller stops taking new glyphs.
    fn grow(&mut self) -> bool {
        if self.texs.len() >= ATLAS_MAX {
            log(&std::format!(
                "glyphs: {} atlases full ({} glyphs), no more room\n",
                self.texs.len(),
                self.glyphs.len(),
            ));
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
        log(&std::format!("glyphs: atlas {} opened\n", self.texs.len()));
        true
    }

    /// Ensure `ch` is in the atlas and return its info. `None` = the font has
    /// no such glyph or the atlas is full (caller advances but draws nothing).
    /// Sets `*uploaded` when it bound + wrote the texture, so the caller can
    /// invalidate its GL state cache once per miss.
    pub fn ensure(&mut self, ch: char, uploaded: &mut bool) -> Option<GlyphInfo> {
        if let Some(cached) = self.glyphs.get(&ch) {
            return *cached;
        }
        let (gw, gh, xmin, ymin, coverage) = match self.ink(ch) {
            Some(g) => g,
            // Missing glyph → cache the miss, draw nothing (no tofu).
            None => {
                self.glyphs.insert(ch, None);
                return None;
            }
        };
        // Inkless (ideographic space, etc.): advance only.
        if gw == 0 || gh == 0 || coverage.is_empty() {
            let info = GlyphInfo {
                tex: self.tex,
                uv: [0.0; 4],
                w: 0.0,
                h: 0.0,
                xmin,
                ymin,
                blank: true,
            };
            self.glyphs.insert(ch, Some(info));
            return Some(info);
        }
        if self.full {
            self.glyphs.insert(ch, None);
            return None;
        }
        // Shelf pack: wrap to a new row when this glyph won't fit the current.
        if self.pen_x + gw + PAD > ATLAS_DIM {
            self.pen_x = PAD;
            self.pen_y += self.shelf_h + PAD;
            self.shelf_h = 0;
        }
        if self.pen_y + gh + PAD > ATLAS_DIM && !self.grow() {
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
            tex: self.tex,
            uv: [
                x0 as f32 / dim,
                y0 as f32 / dim,
                gw as f32 / dim,
                gh as f32 / dim,
            ],
            w: gw as f32,
            h: gh as f32,
            xmin,
            ymin,
            blank: false,
        };
        self.glyphs.insert(ch, Some(info));
        Some(info)
    }

    /// Coverage + metrics for `ch`: from the process-wide cache when it has it,
    /// otherwise rasterized (parsing the font on the spot if this is the first
    /// character that needs it) and put there. `None` = no such glyph, or no
    /// font at all — indistinguishable to the caller, which draws blank either
    /// way.
    ///
    /// Copies the coverage out of the cache rather than borrowing it: `ensure`
    /// then owns a plain `Vec` for the length of the upload and the cache mutex
    /// is released immediately. A glyph is ~2 KB and this runs once per
    /// character per renderer, so the copy is not worth borrowing gymnastics.
    fn ink(&mut self, ch: char) -> Option<(usize, usize, f32, f32, std::vec::Vec<u8>)> {
        if let Ok(cache) = glyph_cache().lock() {
            if let Some((_, entry)) = cache.iter().find(|(c, _)| *c == ch) {
                let g = entry.as_ref()?;
                return Some((g.w, g.h, g.xmin, g.ymin, g.cov.clone()));
            }
        }
        let raster = self.font().and_then(|font| {
            if font.lookup_glyph_index(ch) == 0 {
                return None;
            }
            let t0 = unsafe { ruffle_tick_now() };
            let (metrics, coverage) = font.rasterize(ch, RASTER_PX);
            note_raster(ms_since(t0));
            Some((metrics, coverage))
        });
        // A font we could not parse is NOT cached as a miss: the next renderer
        // may well have the heap for it, and writing `None` here would make the
        // blank permanent for the rest of the session.
        let (metrics, coverage) = match raster {
            Some(r) => r,
            None if self.font.is_none() => return None,
            None => {
                if let Ok(mut cache) = glyph_cache().lock() {
                    cache.push((ch, None));
                }
                return None;
            }
        };
        let (w, h) = (metrics.width, metrics.height);
        let (xmin, ymin) = (metrics.xmin as f32, metrics.ymin as f32);
        if let Ok(mut cache) = glyph_cache().lock() {
            cache.push((
                ch,
                Some(CachedGlyph { cov: coverage.clone(), w, h, xmin, ymin }),
            ));
        }
        Some((w, h, xmin, ymin, coverage))
    }

    /// The parsed shared font, loaded on first need. Tried ONCE per atlas: a
    /// failure here is a missing font service or a heap that could not hold the
    /// parse, and retrying it per character would re-run the expensive part on
    /// every glyph of every frame.
    fn font(&mut self) -> Option<&fontdue::Font> {
        if self.font_tried {
            return self.font.as_ref();
        }
        self.font_tried = true;
        let bytes: &'static [u8] = unsafe {
            let mut len: u32 = 0;
            let ptr = ruffle_shared_font(1 /* PlSharedFontType_ChineseSimplified */, &mut len);
            if ptr.is_null() || len == 0 {
                log("glyphs: shared font unavailable (null/0) — CJK will draw blank\n");
                return None;
            }
            core::slice::from_raw_parts(ptr, len as usize)
        };
        let t0 = unsafe { ruffle_tick_now() };
        match fontdue::Font::from_bytes(bytes, fontdue::FontSettings::default()) {
            Ok(f) => {
                log(&std::format!(
                    "glyphs: shared font parsed ({} KB) in {} ms\n",
                    bytes.len() / 1024,
                    ms_since(t0),
                ));
                self.font = Some(f);
            }
            Err(_) => log("glyphs: fontdue rejected the shared font\n"),
        }
        self.font.as_ref()
    }
}

impl Drop for FontAtlas {
    fn drop(&mut self) {
        if self.tex != 0 {
            unsafe { glDeleteTextures(1, &self.tex) };
        }
    }
}
