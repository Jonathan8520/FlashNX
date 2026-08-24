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
//! ttf-parser reads them directly — there is no XOR deobfuscation to do here.
//!
//! Glyphs are rasterized once at a fixed `RASTER_PX` and scaled with GL_LINEAR
//! at draw time. CJK is treated as full-width / monospace (`draw_text` uses a
//! fixed cell advance, not the font's per-glyph advance) so `measure_text` and
//! `draw_text` agree on widths without the atlas needing to exist yet.
//!
//! **The outline half is `ttf-parser`, the ink half is `ab_glyph_rasterizer`,
//! and both replaced `fontdue` in v1.8 for one measured reason:** fontdue
//! unfolded all 28 944 glyphs of the 7.8 MB shared font on parse — 1942 ms and
//! 136 MB retained — to serve the ~100 a session draws, and paid it again at
//! every renderer teardown. Rasterizing itself was already free (50 glyphs,
//! 0 ms), so nothing about the DRAWING needed fixing; the parse did. The same
//! parse now measures 0 ms on hardware.
//!
//! `Sink` and `raster_gid` were checked on a PC against `simsun` before they
//! ever reached hardware, because a missing Y flip and an unbalanced contour
//! both produce garbage that is obvious as ASCII art and expensive to find over
//! a 5-minute build and a netload.

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
/// the parse cost so the two halves of "Chinese is slow" can be told apart: one
/// big one-off (which is what fontdue's up-front unfolding was) versus a
/// per-character cost that the persistent `glyph_cache` already removes. Kept
/// after the swap precisely because it is what proves the cost moved.
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

/// Room asked for before offering a CJK language: two atlas textures' worth.
///
/// Tied to a real cost, unlike the 96 MB then 192 MB that fontdue's expansion
/// needed and never reliably got. One atlas is 4 MB and the second only opens
/// past ~440 distinct characters, which a Chinese INTERFACE never reaches — so
/// this asks for the interface plus one library's worth of headroom.
const ATLAS_HEADROOM: usize = 2 * ATLAS_DIM * ATLAS_DIM * 4;

/// Can this process afford the CJK font? Decided once and cached: the answer
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
    // APPLET MODE IS NO LONGER REFUSED — history first, because the refusal was
    // written from a console fatal and deserves to be undone on the record.
    //
    // Measured then: with the UI already in Chinese the atlas is built on the
    // FIRST FRAME, heap still nearly empty, so the free-memory probe passed,
    // `fontdue::Font::from_bytes` then asked for more than it had reserved, and
    // the process aborted into an Atmosphere fatal (2168-0002). Opening the
    // picker later with a loaded heap made the same probe fail and looked
    // "fixed" — it had only ever been passing by accident of timing. The lesson
    // was that a free-memory probe cannot guard an allocation that cannot fail,
    // so we asked the MODE instead.
    //
    // That whole shape is gone. ttf-parser expands nothing, and the atlas
    // staging above is fallible now, so there is no unfallible allocation left
    // on this path to guard. What remains is a real and BOUNDED cost — one
    // 1024² RGBA atlas is 4 MB, up to `ATLAS_MAX` of them — so a probe is
    // legitimate again, for a number that means something this time rather than
    // the 96 MB that was invented for fontdue and passed anyway.
    //
    // It is a tripwire, not a guarantee, and that is now an acceptable thing to
    // be: if it passes and GL still refuses, `new_atlas_texture` returns None,
    // `draw_text` falls back to hollow cells, and the screen reads as "this
    // mode cannot draw these characters" instead of rebooting the console.
    let applet = unsafe { ruffle_is_applet_mode() } != 0;
    let (used, total) = crate::query_ram();
    let mut probe: std::vec::Vec<u8> = std::vec::Vec::new();
    let ok = probe.try_reserve_exact(ATLAS_HEADROOM).is_ok();
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
    /// Bearings (px at `RASTER_PX`): `xmin` = left bearing, `ymin` =
    /// baseline-to-bitmap-bottom, y-UP, so it is negative whenever ink descends
    /// below the baseline — which for CJK is most of the time.
    pub xmin: f32,
    pub ymin: f32,
    /// True when the glyph has no ink (e.g. an ideographic space): the caller
    /// still advances the pen but draws nothing.
    pub blank: bool,
}

/// One rasterized glyph's ink, kept ACROSS renderer lifetimes.
///
/// The atlas texture belongs to a renderer and dies with it, so every game quit
/// used to cost a full `fontdue::Font::from_bytes` over the 7.8 MB shared font:
/// seconds of spinner, every time, to redraw characters that had been
/// rasterized minutes earlier. This cache was the answer to that, and it is why
/// only two parses were paid across five renderers instead of five.
///
/// It stays now that the parse is cheap. The coverage is still the expensive
/// half per character (outlining and sweeping a dense ideogram is real work,
/// even if it no longer registers against a 2-second parse), it is one byte per
/// pixel — ~2 KB a glyph, about a megabyte for a whole UI's worth — and it is
/// the only thing here that survives a renderer at all.
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
    // Cleared in STRIPS, from a buffer that is ALLOWED to fail.
    //
    // This used to stage the whole 1024² image: a 4 MB `vec![0u8; ..]` with no
    // fallible path, and under `panic = "abort"` an allocation the pool cannot
    // serve is a console fatal, not an error. It was tolerable only while a
    // 136 MB font parse stood in front of it and applet mode was refused
    // outright — with both of those gone it became the largest thing left on
    // this path that could still take the console down. 64 rows is 256 KB,
    // uploaded sixteen times.
    const STRIP_ROWS: usize = 64;
    let strip = ATLAS_DIM * STRIP_ROWS * 4;
    let mut zeros: std::vec::Vec<u8> = std::vec::Vec::new();
    if zeros.try_reserve_exact(strip).is_err() {
        log("glyphs: no room to stage the atlas — CJK will draw hollow cells\n");
        return None;
    }
    zeros.resize(strip, 0);
    let mut tex: GLuint = 0;
    unsafe {
        glGenTextures(1, &mut tex);
        if tex == 0 {
            log("glyphs: glGenTextures returned 0\n");
            return None;
        }
        glBindTexture(GL_TEXTURE_2D, tex);
        glPixelStorei(GL_UNPACK_ALIGNMENT, 1);
        // Storage first, contents second: a null pointer allocates the texture
        // without reading anything, so the 4 MB never has to exist on our side.
        glTexImage2D(
            GL_TEXTURE_2D,
            0,
            GL_RGBA8 as GLint,
            ATLAS_DIM as GLsizei,
            ATLAS_DIM as GLsizei,
            0,
            GL_RGBA,
            GL_UNSIGNED_BYTE,
            core::ptr::null(),
        );
        // Then zero it. An unwritten region is undefined, and it would show
        // through the bilinear edge of the first glyph packed next to it.
        let mut row = 0usize;
        while row < ATLAS_DIM {
            let rows = STRIP_ROWS.min(ATLAS_DIM - row);
            glTexSubImage2D(
                GL_TEXTURE_2D,
                0,
                0,
                row as GLint,
                ATLAS_DIM as GLsizei,
                rows as GLsizei,
                GL_RGBA,
                GL_UNSIGNED_BYTE,
                zeros.as_ptr() as *const _,
            );
            row += rows;
        }
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
    ///
    /// BORROWS the `pl` mapping rather than owning a copy of it; `load_font`
    /// explains why that is sound and what would break it.
    font: Option<ttf_parser::Face<'static>>,
    font_tried: bool,
    /// Coverage scratch for one glyph, kept here so its buffer is allocated
    /// once and `reset` reuses it instead of every character paying for a
    /// fresh one.
    raster: ab_glyph_rasterizer::Rasterizer,
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
        // History, because it is why this guard exists at all: opening the
        // language picker in APPLET mode took the whole console down with an
        // Atmosphere fatal (2168-0002). The shared font mapped and read fine;
        // the process died inside `fontdue::Font::from_bytes` on a 2 KB
        // allocation, because fontdue expanded the 7.8 MB font into far more
        // than its file size with no fallible allocation anywhere — and under
        // `panic = "abort"` a failed allocation is not a degradation, it is a
        // console reboot. See `cjk_possible` for what the guard checks now that
        // the parse is no longer the thing to be afraid of.
        if !cjk_possible() {
            log("glyphs: not enough memory to parse the CJK font — drawing it blank\n");
            return None;
        }
        let tex = new_atlas_texture()?;
        Some(FontAtlas {
            font: None,
            font_tried: false,
            // Zero-sized until the first glyph sizes it; four floats until then.
            raster: ab_glyph_rasterizer::Rasterizer::new(0, 0),
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
        // `grow` reset the pen, but a glyph taller or wider than a whole atlas
        // still would not fit, and the upload below would write past 1024 px
        // without GL saying a word. Cheap, and the box we size from is now the
        // control hull, which is a little wider than the one fontdue reported.
        if self.pen_x + gw + PAD > ATLAS_DIM || self.pen_y + gh + PAD > ATLAS_DIM {
            self.glyphs.insert(ch, None);
            return None;
        }
        let x0 = self.pen_x;
        let y0 = self.pen_y;
        // The rasterizer gives 8-bit coverage; expand to white RGBA with alpha
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
        self.load_font();
        // Two fields borrowed separately on purpose: `font` read, `raster`
        // mutated. Routing this through a `&mut self` method instead does not
        // borrow-check, which is why `load_font` returns nothing.
        let raster = match self.font.as_ref() {
            Some(face) => raster_glyph(face, &mut self.raster, ch),
            None => None,
        };
        // A font we could not parse is NOT cached as a miss: the next renderer
        // may well have the heap for it, and writing `None` here would make the
        // blank permanent for the rest of the session.
        let (w, h, xmin, ymin, coverage) = match raster {
            Some(r) => r,
            None if self.font.is_none() => return None,
            None => {
                if let Ok(mut cache) = glyph_cache().lock() {
                    cache.push((ch, None));
                }
                return None;
            }
        };
        if let Ok(mut cache) = glyph_cache().lock() {
            cache.push((
                ch,
                Some(CachedGlyph { cov: coverage.clone(), w, h, xmin, ymin }),
            ));
        }
        Some((w, h, xmin, ymin, coverage))
    }

    /// Parse the shared font, ONCE per atlas. A failure here is a missing font
    /// service or bytes we cannot read, and retrying it per character would
    /// re-run it on every glyph of every frame.
    ///
    /// Returns nothing: callers need `self.font` and another field at the same
    /// time, and a `&mut self` accessor would hold the whole struct borrowed.
    fn load_font(&mut self) {
        if self.font_tried {
            return;
        }
        self.font_tried = true;
        let bytes: &'static [u8] = unsafe {
            let mut len: u32 = 0;
            let ptr = ruffle_shared_font(1 /* PlSharedFontType_ChineseSimplified */, &mut len);
            if ptr.is_null() || len == 0 {
                log("glyphs: shared font unavailable (null/0) — CJK will draw blank\n");
                return;
            }
            // `'static` is honest here, and load-bearing in a way it was not
            // under fontdue: ruffle_bridge.cpp calls `plInitialize` once behind
            // a `static bool` and never calls `plExit`, so the mapping outlives
            // the process. fontdue COPIED everything out at parse time; this
            // `Face` POINTS INTO that mapping for its whole life. Adding any
            // `pl` cleanup on the C++ side turns it into a dangling pointer.
            core::slice::from_raw_parts(ptr, len as usize)
        };
        let t0 = unsafe { ruffle_tick_now() };
        match ttf_parser::Face::parse(bytes, 0) {
            Ok(f) => {
                // Three things we cannot learn from a PC, answered in one line
                // on the first hardware run: the em size (the whole scale hangs
                // off it), whether the outlines are `glyf` or CFF (a CFF with a
                // non-standard FontMatrix would need handling `outline_glyph`
                // does not do), and whether the blob is a collection where face
                // 0 might be the wrong one.
                log(&std::format!(
                    "glyphs: shared font parsed ({} KB, {} upem, {} glyphs, glyf={}, faces={}) in {} ms\n",
                    bytes.len() / 1024,
                    f.units_per_em(),
                    f.number_of_glyphs(),
                    f.tables().glyf.is_some(),
                    ttf_parser::fonts_in_collection(bytes).unwrap_or(1),
                    ms_since(t0),
                ));
                self.font = Some(f);
            }
            Err(e) => log(&std::format!("glyphs: ttf-parser rejected the shared font: {}\n", e)),
        }
    }
}

/// An outline sink that keeps nothing.
///
/// `outline_glyph` returns the glyph's bounding box as its RESULT, so running it
/// once with this builder is how we size the pixel grid before rasterizing into
/// it. The alternative — buffering the segments from a single pass — would
/// allocate per glyph, and not allocating is the entire point of the swap.
struct BBoxOnly;

impl ttf_parser::OutlineBuilder for BBoxOnly {
    fn move_to(&mut self, _: f32, _: f32) {}
    fn line_to(&mut self, _: f32, _: f32) {}
    fn quad_to(&mut self, _: f32, _: f32, _: f32, _: f32) {}
    fn curve_to(&mut self, _: f32, _: f32, _: f32, _: f32, _: f32, _: f32) {}
    fn close(&mut self) {}
}

/// Feeds a glyph's outline to the rasterizer, converting font units to the
/// bitmap's own pixel frame on the way through.
struct Sink<'r> {
    r: &'r mut ab_glyph_rasterizer::Rasterizer,
    /// Pixels per font unit: `RASTER_PX / units_per_em`.
    scale: f32,
    /// Bitmap left edge, in scaled px from the pen. Also the glyph's `xmin`.
    x0: f32,
    /// Bitmap TOP edge, in scaled px above the baseline. Also `ymin + h`.
    y1: f32,
    start: ab_glyph_rasterizer::Point,
    last: ab_glyph_rasterizer::Point,
    open: bool,
}

impl<'r> Sink<'r> {
    /// Font units (y up from the baseline) to bitmap pixels (y down from the
    /// top-left). The Y flip lives here and nowhere else.
    #[inline]
    fn map(&self, x: f32, y: f32) -> ab_glyph_rasterizer::Point {
        ab_glyph_rasterizer::point(x * self.scale - self.x0, self.y1 - y * self.scale)
    }

    /// Draw the closing edge back to the contour's first point.
    ///
    /// The rasterizer does NOT close contours for us, and its accumulator is a
    /// running signed area swept left to right: an unclosed contour never
    /// balances, so the ink does not merely look wrong, it SMEARS across every
    /// pixel to its right for the rest of the row. Called from `close()` and
    /// again from `move_to`, so a contour the font left open cannot leak into
    /// the next one; `open` makes it idempotent.
    fn close_contour(&mut self) {
        if self.open {
            self.r.draw_line(self.last, self.start);
            self.open = false;
        }
    }
}

impl<'r> ttf_parser::OutlineBuilder for Sink<'r> {
    fn move_to(&mut self, x: f32, y: f32) {
        self.close_contour();
        let p = self.map(x, y);
        self.start = p;
        self.last = p;
        self.open = true;
    }

    fn line_to(&mut self, x: f32, y: f32) {
        let p = self.map(x, y);
        self.r.draw_line(self.last, p);
        self.last = p;
    }

    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        let (c, p) = (self.map(x1, y1), self.map(x, y));
        self.r.draw_quad(self.last, c, p);
        self.last = p;
    }

    /// CFF/CFF2 only — the `glyf` path emits move/line/quad/close and never a
    /// cubic. Implemented anyway because which of the two the Switch ships is
    /// not knowable from a PC; the log in `load_font` answers it.
    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        let (c1, c2, p) = (self.map(x1, y1), self.map(x2, y2), self.map(x, y));
        self.r.draw_cubic(self.last, c1, c2, p);
        self.last = p;
    }

    fn close(&mut self) {
        self.close_contour();
    }
}

/// `None` = the font has no such character, which is a cacheable miss. A
/// character that EXISTS but has no ink (an ideographic space) comes back as a
/// zero-sized tuple instead, because those two are different verdicts: one is
/// remembered as absent, the other advances the pen and draws nothing.
fn raster_glyph(
    face: &ttf_parser::Face<'static>,
    raster: &mut ab_glyph_rasterizer::Rasterizer,
    ch: char,
) -> Option<(usize, usize, f32, f32, std::vec::Vec<u8>)> {
    // fontdue answered 0 for an absent character; ttf-parser answers None. Take
    // GlyphId(0) as absent too: that is `.notdef`, and rasterizing it would
    // draw the tofu box the old `== 0` test existed to prevent.
    let gid = match face.glyph_index(ch) {
        Some(g) if g.0 != 0 => g,
        _ => return None,
    };
    let t0 = unsafe { ruffle_tick_now() };
    let out = raster_gid(face, raster, gid);
    // Still measured around ONE glyph and never around the parse. This is the
    // counter that separated "the parse costs 1942 ms" from "50 glyphs cost
    // 0 ms", and it is how we can tell whether the cost actually moved.
    note_raster(ms_since(t0));
    Some(out)
}

/// Outline to 8-bit coverage, in exactly the units `ink` hands back.
///
/// Validated on the host before it ever reached hardware (see the harness note
/// in the module header): `一` comes out a horizontal stroke, `国` a closed
/// frame with its compartments open, U+3000 inkless, and a Latin `A` the right
/// way up — the four things a Y flip or an unbalanced contour would each break
/// differently.
fn raster_gid(
    face: &ttf_parser::Face<'static>,
    raster: &mut ab_glyph_rasterizer::Rasterizer,
    gid: ttf_parser::GlyphId,
) -> (usize, usize, f32, f32, std::vec::Vec<u8>) {
    let blank = || (0usize, 0usize, 0.0f32, 0.0f32, std::vec::Vec::new());
    // No outline at all: an ideographic space. NOT a miss — `ensure` must take
    // its `blank: true` branch so the pen advances and nothing is packed.
    let Some(bbox) = face.outline_glyph(gid, &mut BBoxOnly) else {
        return blank();
    };
    // `RASTER_PX` is PIXELS PER EM, which is what fontdue's `scale_factor`
    // meant too — not a bitmap height and not a cap height. `render.rs` divides
    // by `RASTER_PX` to size the quad, so any other normalisation (line height,
    // ascender, ascent+descent) would resize every CJK glyph on screen without
    // a single constant changing. `units_per_em` is guaranteed 16..=16384 by
    // `head` parsing, which `Face::parse` already ran, so it cannot be zero.
    let scale = RASTER_PX / face.units_per_em() as f32;
    // Integer grid, rounded OUTWARDS on both sides. `x0`/`y0` are the bearings:
    // floor of the scaled box minimum, exactly what fontdue reported. The width
    // is algebraically fontdue's `ceil(width + fract(xmin))`, which is what
    // keeps a column of ink from being lost on an off-grid glyph.
    let x0 = (bbox.x_min as f32 * scale).floor();
    let y0 = (bbox.y_min as f32 * scale).floor();
    let x1 = (bbox.x_max as f32 * scale).ceil();
    let y1 = (bbox.y_max as f32 * scale).ceil();
    let (w, h) = ((x1 - x0) as usize, (y1 - y0) as usize);
    // Degenerate, or bigger than a shelf can ever hold. The old code had no
    // size check at all: it trusted fontdue never to exceed ~RASTER_PX, and a
    // `glTexSubImage2D` running past 1024 px would have been silent corruption.
    if w == 0 || h == 0 || w > ATLAS_DIM - 2 * PAD || h > ATLAS_DIM - 2 * PAD {
        return blank();
    }
    raster.reset(w, h);
    {
        let mut sink = Sink {
            r: raster,
            scale,
            x0,
            y1,
            start: ab_glyph_rasterizer::point(0.0, 0.0),
            last: ab_glyph_rasterizer::point(0.0, 0.0),
            open: false,
        };
        face.outline_glyph(gid, &mut sink);
        // A font is not obliged to emit a final `close`.
        sink.close_contour();
    }
    // Exactly `w * h` bytes, row-major, row 0 = the TOP row, no stride —
    // `ensure` indexes it as `coverage[i]` for `i in 0..gw * gh`, so a short
    // buffer would panic and `panic = "abort"` makes that a console fatal.
    let mut cov = std::vec![0u8; w * h];
    raster.for_each_pixel(|i, a| {
        cov[i] = (a * 255.0 + 0.5).clamp(0.0, 255.0) as u8;
    });
    (w, h, x0, y0, cov)
}

impl Drop for FontAtlas {
    fn drop(&mut self) {
        // EVERY texture, not just the current one. `grow` keeps up to
        // `ATLAS_MAX` alive and only `self.tex` was ever deleted, so a session
        // that opened a second atlas leaked 4 MB of GPU memory per game quit.
        for tex in &self.texs {
            if *tex != 0 {
                unsafe { glDeleteTextures(1, tex) };
            }
        }
    }
}
