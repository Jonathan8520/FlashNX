//! Local cover art for library games.
//!
//! v1.2.0 makes covers MANDATORY: every game shows one. Resolution priority:
//!   1. manual sidecar `<game>.png` / `<game>.jpg` the user drops on SD,
//!   2. an online cover cached from Flashpoint (opt-in, per-game — see the
//!      "Jaquette" action; cached as `covers/<basename>.cover.png`),
//!   3. a generated DEFAULT tile (color + title), drawn by render.rs.
//!
//! This module owns paths, decoding and the opt-in online fetch. The texture
//! upload and the default-tile drawing live in render.rs (it has the GL
//! backend + the game's color/title). Covers are metadata enrichment of games
//! the user ALREADY owns — we never download game binaries from Flashpoint.


/// SD roots scanned for manual cover sidecars (mirrors `library::USER_SD_ROOTS`
/// read priority: new `flashnx/` first, legacy `ruffle/` for back-compat).
const USER_SD_ROOTS: &[&str] = &["sdmc:/flashnx", "sdmc:/ruffle"];

/// Where online covers are cached. Kept separate from the game `.swf`s so a
/// user browsing `flashnx/` sees only their games.
const COVER_CACHE_DIR: &str = "sdmc:/flashnx/covers";

/// Resolved cover for a game. `Default` → render the generated tile.
pub enum Cover {
    /// A decodable image file on SD (manual sidecar or cached online cover).
    Image(std::string::String),
    /// No image found — caller draws the generated default.
    Default,
}

/// Strip a trailing `.swf` (case-insensitive) to get the cover stem, so a
/// game `Mario.swf` matches a sidecar `Mario.png` (the natural name) as well
/// as `Mario.swf.png`.
fn stem(basename: &str) -> &str {
    if basename.len() > 4 && basename[basename.len() - 4..].eq_ignore_ascii_case(".swf") {
        &basename[..basename.len() - 4]
    } else {
        basename
    }
}

/// First existing path among the known SD roots for `suffix`, or None.
///
/// Answered from the scan's directory index when it covers that directory:
/// `resolve` probes four sidecar names across two roots plus the cache dir, and
/// on hardware each `Path::exists()` runs ~1.2 ms — 9 to 13 ms per game, which
/// was the single biggest slice of a cover load regardless of image size.
fn find_in_roots(suffix: &str) -> Option<std::string::String> {
    for root in USER_SD_ROOTS {
        let p = std::format!("{}/{}", root, suffix);
        if crate::library::file_exists(&p) {
            return Some(p);
        }
    }
    None
}

/// Cache path for a game's online-fetched cover.
fn cache_path(basename: &str) -> std::string::String {
    std::format!("{}/{}.cover.png", COVER_CACHE_DIR, basename)
}

/// Resolve the cover IMAGE for a game (by `.swf` basename), or `Default`.
/// Priority: manual sidecar (png/jpg, stem or full-basename) > cached online
/// cover > default.
pub fn resolve(basename: &str) -> Cover {
    let st = stem(basename);
    let candidates = [
        std::format!("{}.png", st),
        std::format!("{}.jpg", st),
        std::format!("{}.png", basename),
        std::format!("{}.jpg", basename),
    ];
    for c in &candidates {
        if let Some(p) = find_in_roots(c) {
            return Cover::Image(p);
        }
    }
    let cached = cache_path(basename);
    if crate::library::file_exists(&cached) {
        return Cover::Image(cached);
    }
    Cover::Default
}

// ── decoding ───────────────────────────────────────────────────────────────

/// Decode cover image BYTES (PNG or JPEG) to RGBA8 + dims. Dispatches on magic
/// bytes, not extension, so a mislabelled file / `?type=` URL still works.
pub fn decode_bytes(bytes: &[u8]) -> Option<(std::vec::Vec<u8>, u32, u32)> {
    if bytes.len() >= 4 && &bytes[0..4] == b"\x89PNG" {
        decode_png(bytes)
    } else if bytes.len() >= 2 && bytes[0] == 0xFF && bytes[1] == 0xD8 {
        decode_jpeg(bytes)
    } else {
        // Last resort: try PNG then JPEG (some servers omit a clean header).
        decode_png(bytes).or_else(|| decode_jpeg(bytes))
    }
}

/// Read a file into bytes with a bounded 4 KB-chunk loop. NEVER use
/// `std::fs::read`/`read_to_end` on Horizon: the newlib glue can return a
/// spurious `OutOfMemory` (size/heap-timing dependent), which is exactly why
/// some cached covers decoded and others silently fell back to the default
/// tile. See the project's fs::* gotchas. `max` caps a runaway read.
fn read_file_bounded(path: &str, max: usize) -> Option<std::vec::Vec<u8>> {
    use std::io::Read;
    let mut f = std::fs::File::open(path).ok()?;
    let mut data: std::vec::Vec<u8> = std::vec::Vec::with_capacity(256 * 1024);
    // 64 KB chunks, not 4 KB: each read is an fsdev IPC round trip, and covers
    // run a few hundred KB — the small chunk turned one cover into ~100 trips.
    let mut buf = std::vec![0u8; 64 * 1024];
    loop {
        match f.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                data.extend_from_slice(&buf[..n]);
                if data.len() > max {
                    return None;
                }
            }
            Err(_) => return None,
        }
    }
    Some(data)
}

/// Decode a cover image FILE (PNG or JPEG) to RGBA8 + dims.
pub fn decode_file(path: &str) -> Option<(std::vec::Vec<u8>, u32, u32)> {
    let bytes = read_cover_bytes(path)?;
    decode_bytes(&bytes)
}

/// Read a cover file's raw bytes. Split out of `decode_file` so the caller can
/// time the SD read and the decode separately.
pub fn read_cover_bytes(path: &str) -> Option<std::vec::Vec<u8>> {
    read_file_bounded(path, 16 * 1024 * 1024)
}

// ── Gallery thumbnail cache ────────────────────────────────────────────────
//
// A gallery tile is 227x132, but covers ship at up to 1126x619 — so drawing the
// grid re-decoded ~700 000 pixels per game, every session, to fill 30 000.
// Measured on hardware: 13 to 24 ms per cover of PNG decode alone, which is one
// dropped frame per tile the first time a row scrolls into view.
//
// So the first decode also writes a tile-sized thumbnail next to the cover, and
// later sessions read that instead. The file is our own container rather than a
// PNG because it carries the SOURCE FILE'S LENGTH in its header: replace a cover
// (in-app or by dropping a new file on the SD from a PC) and the length no
// longer matches, so the thumbnail misses and gets rewritten. No invalidation
// logic, no stale art, and the rewrite overwrites in place so nothing orphans.
//
// The full-resolution image is still what the launch/quit reveal draws — that
// one fills the screen, where a tile-sized thumbnail would be visibly soft.

/// Thumbnail box. Slightly above the 227x132 tile so the selected tile's "pop"
/// (which inflates it a few px) still samples at or below 1:1.
const THUMB_MAX_W: u32 = 256;
const THUMB_MAX_H: u32 = 160;

/// `magic(6) | src_len u32 LE | w u16 LE | h u16 LE`, then zlib RGBA8.
const THUMB_MAGIC: &[u8; 6] = b"FNXTH1";
const THUMB_HEADER: usize = 14;

fn thumb_path(basename: &str) -> std::string::String {
    std::format!("{}/{}.thumb", COVER_CACHE_DIR, basename)
}

/// Scale `rgba` down so it just covers the tile box, preserving aspect. Box
/// average (not nearest) — covers are photos and logos, and point-sampling a
/// 4x reduction shimmers. Returns the input untouched when it's already small
/// enough; we never upscale into the cache.
pub fn downscale_for_tile(
    rgba: std::vec::Vec<u8>,
    w: u32,
    h: u32,
) -> (std::vec::Vec<u8>, u32, u32) {
    if w == 0 || h == 0 {
        return (rgba, w, h);
    }
    // Crop-to-fill needs the LARGER of the two ratios: the tile is filled, then
    // the overflow is cropped at draw time.
    let scale = (THUMB_MAX_W as f32 / w as f32).max(THUMB_MAX_H as f32 / h as f32);
    if scale >= 1.0 {
        return (rgba, w, h);
    }
    let dw = ((w as f32 * scale).round() as u32).max(1);
    let dh = ((h as f32 * scale).round() as u32).max(1);
    let mut out = std::vec![0u8; (dw as usize) * (dh as usize) * 4];
    for dy in 0..dh {
        let y0 = (dy as u64 * h as u64 / dh as u64) as u32;
        let y1 = (((dy + 1) as u64 * h as u64 / dh as u64) as u32).max(y0 + 1).min(h);
        for dx in 0..dw {
            let x0 = (dx as u64 * w as u64 / dw as u64) as u32;
            let x1 = (((dx + 1) as u64 * w as u64 / dw as u64) as u32).max(x0 + 1).min(w);
            let (mut r, mut g, mut b, mut a, mut n) = (0u32, 0u32, 0u32, 0u32, 0u32);
            for sy in y0..y1 {
                let row = (sy as usize) * (w as usize) * 4;
                for sx in x0..x1 {
                    let i = row + (sx as usize) * 4;
                    r += rgba[i] as u32;
                    g += rgba[i + 1] as u32;
                    b += rgba[i + 2] as u32;
                    a += rgba[i + 3] as u32;
                    n += 1;
                }
            }
            let n = n.max(1);
            let o = ((dy as usize) * (dw as usize) + dx as usize) * 4;
            out[o] = (r / n) as u8;
            out[o + 1] = (g / n) as u8;
            out[o + 2] = (b / n) as u8;
            out[o + 3] = (a / n) as u8;
        }
    }
    (out, dw, dh)
}

/// Read a game's cached thumbnail, but only if it was built from a source cover
/// of exactly `src_len` bytes. `None` → caller decodes the full cover.
pub fn read_thumb(basename: &str, src_len: u64) -> Option<(std::vec::Vec<u8>, u32, u32)> {
    use std::io::Read;
    let path = thumb_path(basename);
    if src_len == 0 || !crate::library::file_exists(&path) {
        return None;
    }
    let bytes = read_file_bounded(&path, 4 * 1024 * 1024)?;
    if bytes.len() < THUMB_HEADER || &bytes[0..6] != THUMB_MAGIC {
        return None;
    }
    let stamped = u32::from_le_bytes([bytes[6], bytes[7], bytes[8], bytes[9]]) as u64;
    if stamped != src_len {
        return None; // cover changed since this thumbnail was written
    }
    let w = u16::from_le_bytes([bytes[10], bytes[11]]) as u32;
    let h = u16::from_le_bytes([bytes[12], bytes[13]]) as u32;
    let want = (w as usize) * (h as usize) * 4;
    if want == 0 {
        return None;
    }
    let mut rgba: std::vec::Vec<u8> = std::vec::Vec::with_capacity(want);
    flate2::read::ZlibDecoder::new(&bytes[THUMB_HEADER..])
        .read_to_end(&mut rgba)
        .ok()?;
    if rgba.len() != want {
        return None;
    }
    Some((rgba, w, h))
}

/// Write a game's tile thumbnail, stamped with the source cover's length.
/// Best-effort: a failure just means the next session decodes the full cover.
pub fn write_thumb(basename: &str, src_len: u64, rgba: &[u8], w: u32, h: u32) {
    use std::io::Write;
    if src_len == 0 || src_len > u32::MAX as u64 || w == 0 || h == 0 {
        return;
    }
    if w > u16::MAX as u32 || h > u16::MAX as u32 {
        return;
    }
    let mut enc =
        flate2::write::ZlibEncoder::new(std::vec::Vec::new(), flate2::Compression::fast());
    if enc.write_all(rgba).is_err() {
        return;
    }
    let Ok(body) = enc.finish() else { return };
    let mut out = std::vec::Vec::with_capacity(THUMB_HEADER + body.len());
    out.extend_from_slice(THUMB_MAGIC);
    out.extend_from_slice(&(src_len as u32).to_le_bytes());
    out.extend_from_slice(&(w as u16).to_le_bytes());
    out.extend_from_slice(&(h as u16).to_le_bytes());
    out.extend_from_slice(&body);
    let _ = std::fs::create_dir_all(COVER_CACHE_DIR);
    let path = thumb_path(basename);
    if std::fs::write(&path, &out).is_ok() {
        crate::library::note_file_created(&path);
        crate::sd::commit();
    }
}

/// Decode PNG bytes to RGBA8 (mirrors `library::decode_banner`, promoting any
/// color type to RGBA; indexed PNGs are rejected).
fn decode_png(bytes: &[u8]) -> Option<(std::vec::Vec<u8>, u32, u32)> {
    let cursor = std::io::Cursor::new(bytes);
    let mut decoder = png::Decoder::new(cursor);
    // EXPAND: palette -> RGB, grayscale<8 -> 8-bit, tRNS -> alpha. This is what
    // makes INDEXED PNGs work (many Flashpoint logos are palette PNGs — without
    // this they decoded to None and showed the default tile / a "?" thumbnail).
    // STRIP_16: 16-bit -> 8-bit. So the frame normalizes to an 8-bit
    // RGB/RGBA/gray buffer the match below handles; Indexed never reaches output.
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = decoder.read_info().ok()?;
    let (color_type, _bd) = reader.output_color_type();
    let (w, h) = {
        let info = reader.info();
        (info.width, info.height)
    };
    let out_size = reader.output_buffer_size()?;
    let mut buf = std::vec![0u8; out_size];
    reader.next_frame(&mut buf).ok()?;
    let rgba = match color_type {
        png::ColorType::Rgba => buf,
        png::ColorType::Rgb => {
            let mut out = std::vec::Vec::with_capacity(buf.len() / 3 * 4);
            for px in buf.chunks_exact(3) {
                out.extend_from_slice(&[px[0], px[1], px[2], 0xFF]);
            }
            out
        }
        png::ColorType::GrayscaleAlpha => {
            let mut out = std::vec::Vec::with_capacity(buf.len() * 2);
            for px in buf.chunks_exact(2) {
                out.extend_from_slice(&[px[0], px[0], px[0], px[1]]);
            }
            out
        }
        png::ColorType::Grayscale => {
            let mut out = std::vec::Vec::with_capacity(buf.len() * 4);
            for &px in &buf {
                out.extend_from_slice(&[px, px, px, 0xFF]);
            }
            out
        }
        png::ColorType::Indexed => return None,
    };
    Some((rgba, w, h))
}

/// Decode JPEG bytes to RGBA8 via the Switch fork of jpeg-decoder (forced
/// single-threaded). Handles RGB24 and grayscale (L8); anything exotic is
/// dropped (caller falls back to the default tile).
fn decode_jpeg(bytes: &[u8]) -> Option<(std::vec::Vec<u8>, u32, u32)> {
    let mut decoder = jpeg_decoder::Decoder::new(std::io::Cursor::new(bytes));
    let pixels = decoder.decode().ok()?;
    let info = decoder.info()?;
    let (w, h) = (info.width as u32, info.height as u32);
    let rgba = match info.pixel_format {
        jpeg_decoder::PixelFormat::RGB24 => {
            let mut out = std::vec::Vec::with_capacity(pixels.len() / 3 * 4);
            for px in pixels.chunks_exact(3) {
                out.extend_from_slice(&[px[0], px[1], px[2], 0xFF]);
            }
            out
        }
        jpeg_decoder::PixelFormat::L8 => {
            let mut out = std::vec::Vec::with_capacity(pixels.len() * 4);
            for &px in &pixels {
                out.extend_from_slice(&[px, px, px, 0xFF]);
            }
            out
        }
        _ => return None,
    };
    Some((rgba, w, h))
}

// ── opt-in online fetch ──────────────────────────────────────────────────────

/// Download a cover image from `cover_url` and cache it as `basename`'s cover.
/// Synchronous (HTTPS GET). Used by the per-game "Jaquette" picker and by the
/// Flashpoint game download, which grabs the cover automatically so the game
/// shows its art in JOUER without a manual step.
pub fn fetch_url_and_cache(
    basename: &str,
    cover_url: &str,
) -> Result<std::string::String, std::string::String> {
    // A few KB up to a few hundred KB; 4 MB cap is plenty.
    // A game can have a logo and no screenshot, or the reverse (issue #59 added
    // the second source). When the one asked for is missing, try the other rather
    // than leave the game blank: a badly shaped cover still beats no cover. One
    // substitution only, and only on the sharded image URLs we built ourselves.
    let bytes = match crate::net::http_get(cover_url, 4 * 1024 * 1024) {
        Ok(b) => b,
        Err(e) if cover_url.contains("/Logos/") || cover_url.contains("/Screenshots/") => {
            let other = if cover_url.contains("/Logos/") {
                cover_url.replace("/Logos/", "/Screenshots/")
            } else {
                cover_url.replace("/Screenshots/", "/Logos/")
            };
            crate::net::log(&std::format!(
                "covers: {} unavailable ({}), trying {}\n", cover_url, e, other
            ));
            crate::net::http_get(&other, 4 * 1024 * 1024)?
        }
        Err(e) => return Err(e),
    };
    let _ = std::fs::create_dir_all(COVER_CACHE_DIR);
    let path = cache_path(basename);
    std::fs::write(&path, &bytes).map_err(|e| std::format!("write cover: {}", e))?;
    // Cached after the scan listed `covers/` — tell the index, or `resolve` would
    // keep reporting this game as cover-less for the rest of the session.
    crate::library::note_file_created(&path);
    crate::sd::commit();
    Ok(path)
}

/// Remove the cover files tied to a game on delete that the C++ dir scan can't
/// reach: the cached online cover lives in the `covers/` SUBDIR (a different
/// directory than the `.swf`), and stem-named manual sidecars (`<name>.png`)
/// don't match the C++ `<basename>.swf.*` prefix. The `<basename>.swf.png/.jpg`
/// sidecars ARE deleted C++-side, so they're not repeated here. Best-effort
/// (missing files are fine); returns the count removed for logging. The caller
/// commits the SD. `basename` is the full `.swf` filename (e.g. `Mario.swf`).
pub fn remove_for(basename: &str) -> u32 {
    let mut removed = 0u32;
    // 1) Cached online cover (covers/<basename>.cover.png) + its tile thumbnail.
    let cached = cache_path(basename);
    if std::fs::remove_file(&cached).is_ok() {
        crate::library::note_file_removed(&cached);
        removed += 1;
    }
    let thumb = thumb_path(basename);
    if std::fs::remove_file(&thumb).is_ok() {
        crate::library::note_file_removed(&thumb);
        removed += 1;
    }
    // 2) Stem-named manual sidecars (the natural `<name>.png`/`.jpg` form that
    //    `resolve` accepts) across the SD roots.
    let st = stem(basename);
    for root in USER_SD_ROOTS {
        for ext in ["png", "jpg"] {
            let p = std::format!("{}/{}.{}", root, st, ext);
            if std::fs::remove_file(&p).is_ok() {
                crate::library::note_file_removed(&p);
                removed += 1;
            }
        }
    }
    removed
}
