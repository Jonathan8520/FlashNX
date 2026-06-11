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

/// Build the db-api search URL for `name`. Split from `parse_search` so the
/// Flashpoint game search can run through the async GET path (spinner) instead
/// of blocking the UI on a synchronous HTTP. `filter=true` applies Flashpoint's
/// default content filter (its "Filter entries" checkbox), excluding entries the
/// archive flags as extreme — same default the cover search in flashpoint.rs uses.
pub fn search_url(name: &str) -> std::string::String {
    let q = net::url_encode_path(name.trim());
    std::format!(
        "{}?smartSearch={}&platform=Flash&filter=true&fields=id,title,developer,publisher,releaseDate,zipped",
        SEARCH_BASE, q
    )
}

/// Parse a db-api search response into up to 60 fetchable `CatalogEntry` hits
/// (id = UUID for the `/get?id=` download, title, developer, cover_url). Only
/// `zipped` games are kept — the others 404 on `/get?id=`.
pub fn parse_search(
    bytes: &[u8],
) -> Result<std::vec::Vec<flashpoint::CatalogEntry>, std::string::String> {
    const MAX: usize = 60;
    let json: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|e| crate::loc::err_json(&e.to_string()))?;
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
/// entry, decompressed, paired with its entry name (e.g.
/// `content/<host>/<path>/main.swf` — used by `htdocs_base_from_entry` to find
/// the game's companion files). Handles store (0) and deflate (8); bails on
/// data-descriptor entries (their sizes aren't in the local header). Flashpoint
/// GameZIPs are simple (no data descriptor, deflate) — verified byte-for-byte
/// against a real GameZIP 2026-06-07 (CWS magic, exact uncompressed size).
pub fn extract_first_swf(zip: &[u8]) -> Option<(std::vec::Vec<u8>, std::string::String)> {
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
            let bytes = match method {
                0 => Some(comp.to_vec()),
                8 => inflate(comp, usize_),
                _ => None,
            }?;
            return Some((bytes, name.into_owned()));
        }
        i = data_end;
    }
    None
}

// ── Multi-file games: companion SWF fetch ─────────────────────────────────────

/// Build the Flashpoint "Legacy htdocs" base URL for a game's companion files
/// from the GameZIP entry name. The entry is `content/<host>/<path>/<file>.swf`;
/// companions live next to it on the public mirror at
/// `https://infinity.unstable.life/Flashpoint/Legacy/htdocs/<host>/<path>/`
/// (the db-api GameZIP often ships ONLY the main SWF — the full set is here).
/// Returns None if the entry isn't under `content/` or has no directory.
pub fn htdocs_base_from_entry(entry: &str) -> Option<std::string::String> {
    let rest = entry.strip_prefix("content/")?;
    let slash = rest.rfind('/')?;
    let dir = &rest[..slash];
    if dir.is_empty() {
        return None;
    }
    Some(std::format!(
        "https://infinity.unstable.life/Flashpoint/Legacy/htdocs/{}/",
        dir
    ))
}

/// Scan a (possibly compressed) SWF for the `<name>.swf` files it loads
/// (`loadMovie` / `GetURL` string constants), deduped, in discovery order.
/// Best-effort and bare-filenames only: the scan walks back over
/// `[A-Za-z0-9_-]` so a load in a subdir yields just the leaf name. Enough for
/// the flat companion layout Flashpoint games use (e.g. `top.swf`, `books.swf`).
pub fn scan_swf_siblings(swf_bytes: &[u8]) -> std::vec::Vec<std::string::String> {
    let buf = match swf::decompress_swf(swf_bytes) {
        Ok(b) => b,
        Err(_) => return std::vec::Vec::new(),
    };
    let data = &buf.data;
    let n = data.len();
    let mut out: std::vec::Vec<std::string::String> = std::vec::Vec::new();
    let mut i = 0usize;
    while i + 4 <= n {
        if data[i] == b'.'
            && data[i + 1].eq_ignore_ascii_case(&b's')
            && data[i + 2].eq_ignore_ascii_case(&b'w')
            && data[i + 3].eq_ignore_ascii_case(&b'f')
        {
            let mut start = i;
            while start > 0 {
                let c = data[start - 1];
                if c.is_ascii_alphanumeric() || c == b'_' || c == b'-' {
                    start -= 1;
                } else {
                    break;
                }
            }
            if start < i {
                if let Ok(s) = std::str::from_utf8(&data[start..i + 4]) {
                    if !out.iter().any(|x| x.eq_ignore_ascii_case(s)) {
                        out.push(s.to_string());
                    }
                }
            }
            i += 4;
        } else {
            i += 1;
        }
    }
    out
}

/// Download a multi-file game's companion SWFs into `files_dir` (the sidecar
/// `<game>.files/` folder the SidecarNavigator reads at play time). Starts from
/// the names `main_swf` references, fetches each from `htdocs_base`, then scans
/// each downloaded companion for further references (breadth-first) until none
/// remain. `main_name` (the game's own entry file, e.g. "main.swf") is skipped.
/// Best-effort and synchronous (small files, runs in the post-download finish
/// step like the auto-cover fetch): a failed/non-SWF fetch is logged and
/// skipped. Returns the number of companions written. CONTENT-NEUTRAL: same
/// Flashpoint mirror the GameZIP itself came from, only for the game the user
/// explicitly downloaded.
pub fn fetch_siblings(
    main_swf: &[u8],
    main_name: &str,
    htdocs_base: &str,
    files_dir: &str,
) -> usize {
    const MAX_FILES: usize = 32;
    const PER_FILE_CAP: usize = 4 * 1024 * 1024;
    let mut queue = scan_swf_siblings(main_swf);
    if queue.is_empty() {
        return 0;
    }
    if std::fs::create_dir_all(files_dir).is_err() {
        net::log(&std::format!("siblings: can't create dir {}\n", files_dir));
        return 0;
    }
    let mut seen: std::vec::Vec<std::string::String> =
        std::vec![main_name.to_ascii_lowercase()];
    let mut written = 0usize;
    while let Some(name) = queue.pop() {
        let lower = name.to_ascii_lowercase();
        if seen.iter().any(|s| *s == lower) {
            continue;
        }
        seen.push(lower);
        if written >= MAX_FILES {
            net::log("siblings: hit MAX_FILES cap, stopping\n");
            break;
        }
        let url = std::format!("{}{}", htdocs_base, name);
        match net::http_get(&url, PER_FILE_CAP) {
            Ok(bytes) => {
                let is_swf = bytes.len() > 8 && {
                    let sig = &bytes[0..3];
                    sig == b"FWS" || sig == b"CWS" || sig == b"ZWS"
                };
                if !is_swf {
                    net::log(&std::format!("siblings: {} not a SWF, skip\n", name));
                    continue;
                }
                let dest = std::format!("{}/{}", files_dir, name);
                if std::fs::write(&dest, &bytes).is_ok() {
                    written += 1;
                    net::log(&std::format!(
                        "siblings: fetched {} ({} bytes) -> {}\n",
                        name,
                        bytes.len(),
                        dest
                    ));
                    for more in scan_swf_siblings(&bytes) {
                        queue.push(more);
                    }
                } else {
                    net::log(&std::format!("siblings: write failed {}\n", dest));
                }
            }
            Err(e) => {
                net::log(&std::format!("siblings: fetch {} failed: {}\n", url, e));
            }
        }
    }
    written
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
