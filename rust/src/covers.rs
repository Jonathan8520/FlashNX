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

use crate::sources::flashpoint;

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
fn find_in_roots(suffix: &str) -> Option<std::string::String> {
    for root in USER_SD_ROOTS {
        let p = std::format!("{}/{}", root, suffix);
        if std::path::Path::new(&p).exists() {
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
    if std::path::Path::new(&cached).exists() {
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
    let mut data: std::vec::Vec<u8> = std::vec::Vec::new();
    let mut buf = [0u8; 4096];
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
    let bytes = read_file_bounded(path, 16 * 1024 * 1024)?;
    decode_bytes(&bytes)
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

/// Fetch a cover for `basename` from Flashpoint by `search_name`, picking the
/// candidate at `pick` (the cover-picker selection), and cache it. Returns the
/// cached path on success. Synchronous (user-initiated via the "Jaquette"
/// action; never auto-triggered).
pub fn fetch_and_cache(
    basename: &str,
    candidate: &flashpoint::CatalogEntry,
) -> Result<std::string::String, std::string::String> {
    // Logos are small PNGs; 4 MB cap is plenty and matches the metadata cap.
    let bytes = crate::net::http_get(&candidate.cover_url, 4 * 1024 * 1024)?;
    let _ = std::fs::create_dir_all(COVER_CACHE_DIR);
    let path = cache_path(basename);
    std::fs::write(&path, &bytes).map_err(|e| std::format!("write cover: {}", e))?;
    crate::sd::commit();
    Ok(path)
}
