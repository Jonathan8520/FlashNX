//! Flashpoint GameZIP download — fetch a playable `.swf` for a Flash game the
//! user picks from a Flashpoint search, WITHOUT any auth or OAuth.
//!
//! Pipeline (all PUBLIC endpoints, verified on hardware data 2026-06-07):
//!   1. search   : db-api.unstable.life/search?smartSearch=<name>&platform=Flash&fields=id,title
//!   2. download : db-api.unstable.life/get?id=<uuid>   -> a ZIP (the GameZIP)
//!   3. the ZIP holds `content.json` + `content/{host}{path}/<game>.swf` (deflate)
//!
//! `/get?id=` resolves the UUID to the packed ZIP server-side, so we never need
//! the game_data `dateAdded` timestamp NOR an OAuth token (the admin-confirmed
//! "fpfss client_credentials" route is only a fallback if `/get` is ever
//! retired). We unzip with `flate2` (already in the dep tree — see
//! `extract_first_swf`). CONTENT-NEUTRAL stays: the user searches and picks a
//! game; we only fetch what they explicitly ask for, live from Flashpoint.

use crate::net;
use crate::sources::flashpoint;
use std::io::Read;

const SEARCH_BASE: &str = "https://db-api.unstable.life/search";
const GET_BASE: &str = "https://db-api.unstable.life/get";

/// Public download URL for a game's GameZIP — by UUID alone, no auth.
pub fn get_url(id: &str) -> std::string::String {
    std::format!("{}?id={}", GET_BASE, id)
}

/// Build a safe `<title>.swf` filename from a game title. ASCII-only (the
/// pixel font `draw_text` folds to ASCII) and no path separators, so it is
/// both the on-SD filename and the list label.
pub fn swf_filename(title: &str) -> std::string::String {
    let mut s: std::string::String = title
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, ' ' | '-' | '_' | '.' | '(' | ')') {
                c
            } else {
                '_'
            }
        })
        .collect();
    s = s.trim().to_string();
    if s.is_empty() {
        s = std::string::String::from("game");
    }
    if !s.to_ascii_lowercase().ends_with(".swf") {
        s.push_str(".swf");
    }
    s
}

/// Search Flashpoint for Flash games by name. Returns up to `MAX` hits as
/// `CatalogEntry`s (id = UUID for the `/get?id=` download, title, developer,
/// cover_url) so the cover-grid renderer can show them as a gallery and the
/// download flow can build the GameZIP URL from `id`.
pub fn search(name: &str) -> Result<std::vec::Vec<flashpoint::CatalogEntry>, std::string::String> {
    const MAX: usize = 60;
    let q = net::url_encode_path(name.trim());
    let url = std::format!(
        "{}?smartSearch={}&platform=Flash&fields=id,title,developer,publisher,releaseDate,zipped",
        SEARCH_BASE, q
    );
    let bytes = net::http_get(&url, 1024 * 1024)?;
    let json: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|e| crate::loc::err_json(&e.to_string()))?;
    let arr = json
        .as_array()
        .ok_or_else(|| std::string::String::from(crate::loc::s().err_json_no_files))?;
    let mut out: std::vec::Vec<flashpoint::CatalogEntry> = std::vec::Vec::new();
    for g in arr {
        let id = g.get("id").and_then(|v| v.as_str()).unwrap_or("");
        if id.is_empty() {
            continue;
        }
        // Skip legacy games not in the GameZIP server: `/get?id=` 404s on them
        // (verified 2026-06-07 — ~9% of hits). Only `zipped` games are fetchable.
        if !g.get("zipped").and_then(|v| v.as_bool()).unwrap_or(false) {
            continue;
        }
        let title = g.get("title").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let developer = g
            .get("developer")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let publisher = g
            .get("publisher")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let release_date = g
            .get("releaseDate")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        out.push(flashpoint::CatalogEntry {
            id: id.to_string(),
            title,
            developer,
            publisher,
            release_date,
            cover_url: flashpoint::logo_url(id),
        });
        if out.len() >= MAX {
            break;
        }
    }
    Ok(out)
}

// ── ZIP extraction (dependency-free, flate2 only) ─────────────────────────────

/// Inflate a raw DEFLATE stream (ZIP compression method 8).
fn inflate(comp: &[u8], expected: usize) -> Option<std::vec::Vec<u8>> {
    let mut d = flate2::read::DeflateDecoder::new(comp);
    let mut out = std::vec::Vec::with_capacity(expected);
    d.read_to_end(&mut out).ok()?;
    Some(out)
}

/// Walk a ZIP's local file headers sequentially and return the first `.swf`
/// entry, decompressed. Handles store (0) and deflate (8); bails on
/// data-descriptor entries (their sizes aren't in the local header). Flashpoint
/// GameZIPs are simple (no data descriptor, deflate) — verified byte-for-byte
/// against a real GameZIP 2026-06-07 (CWS magic, exact uncompressed size).
pub fn extract_first_swf(zip: &[u8]) -> Option<std::vec::Vec<u8>> {
    let mut i = 0usize;
    while i + 30 <= zip.len() && &zip[i..i + 4] == b"PK\x03\x04" {
        let flags = u16::from_le_bytes([zip[i + 6], zip[i + 7]]);
        let method = u16::from_le_bytes([zip[i + 8], zip[i + 9]]);
        let csize =
            u32::from_le_bytes([zip[i + 18], zip[i + 19], zip[i + 20], zip[i + 21]]) as usize;
        let usize_ =
            u32::from_le_bytes([zip[i + 22], zip[i + 23], zip[i + 24], zip[i + 25]]) as usize;
        let nlen = u16::from_le_bytes([zip[i + 26], zip[i + 27]]) as usize;
        let elen = u16::from_le_bytes([zip[i + 28], zip[i + 29]]) as usize;
        if flags & 0x0008 != 0 {
            return None; // data descriptor: csize/usize not reliable here
        }
        let name_start = i + 30;
        let name_end = name_start + nlen;
        if name_end > zip.len() {
            return None;
        }
        let name = std::string::String::from_utf8_lossy(&zip[name_start..name_end]);
        let data_start = name_end + elen;
        let data_end = data_start + csize;
        if data_end > zip.len() {
            return None;
        }
        if name.to_ascii_lowercase().ends_with(".swf") {
            let comp = &zip[data_start..data_end];
            return match method {
                0 => Some(comp.to_vec()),
                8 => inflate(comp, usize_),
                _ => None,
            };
        }
        i = data_end;
    }
    None
}

/// Bounded file read for the downloaded GameZIP. Mirrors
/// `covers::read_file_bounded` — NEVER use `std::fs::read` on Horizon (newlib
/// glue can spuriously return OutOfMemory). `max` caps a runaway read.
pub fn read_file_bounded(path: &str, max: usize) -> Option<std::vec::Vec<u8>> {
    let mut f = std::fs::File::open(path).ok()?;
    let mut data: std::vec::Vec<u8> = std::vec::Vec::new();
    let mut buf = [0u8; 8192];
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
