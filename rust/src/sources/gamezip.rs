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
/// of blocking the UI on a synchronous HTTP. `filter` maps to Flashpoint's
/// content filter (its "Filter entries" checkbox): `true` (the default) excludes
/// entries the archive flags as extreme — same default the cover search in
/// flashpoint.rs uses. The importer can flip it to `false` (ZL+ZR in the results)
/// to surface the mature-rated catalogue, matching the official launcher.
pub fn search_url(name: &str, filter: bool) -> std::string::String {
    let q = net::url_encode_path(name.trim());
    std::format!(
        "{}?smartSearch={}&platform=Flash&filter={}&fields=id,title,developer,publisher,releaseDate,zipped,launchCommand",
        SEARCH_BASE, q, filter
    )
}

/// Parse a db-api search response into up to 60 fetchable `CatalogEntry` hits
/// (id = UUID, title, developer, cover_url, launch_command, zipped). `zipped`
/// games download from the GameZIP server (`/get?id=`); non-zipped (legacy
/// "loose") games 404 there but are fetched directly from the htdocs mirror via
/// their launchCommand, so they're KEPT when that command maps to a usable
/// htdocs URL (and dropped otherwise — nothing to download). Deduped: db-api
/// returns the same game several times (exact UUID repeats AND same-title
/// alternate entries), which clutters the cover grid; we keep the first of each.
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
    let mut seen_ids: std::vec::Vec<std::string::String> = std::vec::Vec::new();
    let mut seen_titles: std::vec::Vec<std::string::String> = std::vec::Vec::new();
    for g in arr {
        let id = g.get("id").and_then(|v| v.as_str()).unwrap_or("");
        if id.is_empty() {
            continue;
        }
        let zipped = g.get("zipped").and_then(|v| v.as_bool()).unwrap_or(false);
        let launch_command = g
            .get("launchCommand")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        // A `zipped` game is fetched from the GameZIP server (`/get?id=`). A
        // non-zipped (legacy "loose") entry 404s there, but its files live on
        // the htdocs mirror, reachable from the launchCommand URL — keep it only
        // when that command maps to a usable htdocs URL (else nothing to fetch).
        if !zipped && htdocs_url_from_command(&launch_command).is_none() {
            continue;
        }
        let title = g.get("title").and_then(|v| v.as_str()).unwrap_or("").to_string();
        // Dedup by id (exact repeats) then by title (same game, alternate rows),
        // before the MAX cap so the grid fills with 60 DISTINCT games.
        if seen_ids.iter().any(|s| s == id) {
            continue;
        }
        let title_key = title.trim().to_ascii_lowercase();
        if !title_key.is_empty() && seen_titles.iter().any(|s| *s == title_key) {
            continue;
        }
        seen_ids.push(id.to_string());
        if !title_key.is_empty() {
            seen_titles.push(title_key);
        }
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
            launch_command,
            zipped,
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

/// Map a Flashpoint `launchCommand` URL to its ZIP content entry name:
/// `http://i.flipline.com/gamefiles/papalouie2/PapaLouie2_v2_1.swf?x=1`
///   -> `content/i.flipline.com/gamefiles/papalouie2/PapaLouie2_v2_1.swf`.
/// Returns None if empty or not a URL-shaped command.
fn launch_entry_from_command(launch_command: &str) -> Option<std::string::String> {
    let lc = launch_command.trim();
    if lc.is_empty() {
        return None;
    }
    let rest = lc.split_once("://").map(|(_, r)| r).unwrap_or(lc);
    let rest = rest.split(['?', '#']).next().unwrap_or(rest);
    if rest.is_empty() || rest.ends_with('/') {
        return None;
    }
    Some(std::format!("content/{}", rest))
}

/// Percent-decode a path so a launchCommand with an encoded path (e.g. `%20`)
/// matches the GameZIP's literal entry names. Flashpoint launchCommands are
/// URL-encoded but the zip stores literal paths (real spaces), so without this a
/// game whose entry SWF has spaces ("Five Minutes to Kill Yourself.swf") never
/// matches `launch_entry` -> we fall back to the WRONG swf (the first in the zip,
/// often a character/asset stub, e.g. character_bear.swf) and the game shows a
/// blank screen with a stray sprite. Only `%XX` is decoded; other bytes are kept
/// verbatim ('+' is a literal '+' in a path, not a space).
fn percent_decode(s: &str) -> std::string::String {
    let b = s.as_bytes();
    let mut out: std::vec::Vec<u8> = std::vec::Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let (Some(hi), Some(lo)) =
                ((b[i + 1] as char).to_digit(16), (b[i + 2] as char).to_digit(16))
            {
                out.push((hi * 16 + lo) as u8);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    std::string::String::from_utf8_lossy(&out).into_owned()
}

/// Write `bytes` to `<files_dir>/<rel>` (rel is a forward-slash `content/`-
/// relative path like `host/path/file.swf`), creating parent dirs. Best-effort.
extern "C" {
    // C++/libnx file write (cpp/src/swf_picker.cpp). Creates parent dirs +
    // writes the file. Returns 1 on success, 0 on failure. We go through C++
    // because Rust's std::fs on Horizon silently fails to persist some files
    // during a big multi-file extraction (write returns Ok but the file is
    // later unreadable; std::fs::metadata even returns a timestamp as the size).
    fn swf_picker_write_file(path: *const core::ffi::c_char, data: *const u8, len: u32) -> core::ffi::c_int;
}

fn write_tree_file(files_dir: &str, rel: &str, bytes: &[u8]) -> bool {
    let base = files_dir.trim_end_matches('/');
    let full = std::format!("{}/{}", base, rel);
    // Path must be NUL-terminated for C++; reject any (pathological) interior NUL.
    let c_path = match std::ffi::CString::new(full.clone()) {
        Ok(c) => c,
        Err(_) => return false,
    };
    let ok = unsafe {
        swf_picker_write_file(c_path.as_ptr(), bytes.as_ptr(), bytes.len() as u32)
    };
    if ok == 0 {
        net::log(&std::format!("extract: C++ write failed {} ({} bytes)\n", full, bytes.len()));
        false
    } else {
        true
    }
}

/// Extract the FULL `content/<host>/<path>/<file>` tree of a Flashpoint GameZIP
/// into `files_dir`, mirroring the host/path layout so the SidecarNavigator can
/// serve each asset by its original URL at play time. A GameZIP bundles a game's
/// whole asset set — alternate SWF versions, ad-network stubs (fliplineads etc.),
/// xml/png — so extracting it ALL makes the download self-contained, instead of
/// guessing companions from string scans (which only finds statically-referenced
/// `.swf` siblings and misses everything loaded by absolute URL at runtime).
///
/// `launch_command` (the Flashpoint launchCommand, the entry SWF's URL) selects
/// which SWF is the game's entry; the matching content entry is returned as
/// `(bytes, entry_name)` for the caller to write as the flat library SWF. Falls
/// back to the FIRST `.swf` when the command is empty or its entry is absent.
/// Returns None if no SWF is found. Bails (stops) on data-descriptor entries —
/// Flashpoint GameZIPs don't use them (verified) and their local-header sizes
/// would be unreliable.
pub fn extract_gamezip_tree(
    zip: &[u8],
    files_dir: &str,
    launch_command: &str,
) -> Option<(std::vec::Vec<u8>, std::string::String)> {
    // Decode the launch entry up front so it matches the zip's literal (decoded)
    // entry names; the comparison below decodes each entry name too (robust to a
    // zip that stores encoded names).
    let launch_entry = launch_entry_from_command(launch_command).map(|e| percent_decode(&e));
    let mut first_swf: Option<(std::vec::Vec<u8>, std::string::String)> = None;
    let mut launch_swf: Option<(std::vec::Vec<u8>, std::string::String)> = None;

    let mut entries = 0usize;
    let mut written = 0usize;
    let mut failed = 0usize;
    let mut inflate_fail = 0usize;
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
            break; // data descriptor: sizes not in the local header
        }
        let name_start = i + 30;
        let name_end = name_start + nlen;
        if name_end > zip.len() {
            break;
        }
        let name = std::string::String::from_utf8_lossy(&zip[name_start..name_end]).into_owned();
        let data_start = name_end + elen;
        let data_end = data_start + csize;
        if data_end > zip.len() {
            break;
        }
        // Only files under `content/` (skip content.json and directory entries).
        if let Some(rel) = name.strip_prefix("content/").filter(|r| !r.is_empty() && !r.ends_with('/'))
        {
            entries += 1;
            let comp = &zip[data_start..data_end];
            let bytes = match method {
                0 => Some(comp.to_vec()),
                8 => inflate(comp, usize_),
                _ => None,
            };
            if let Some(bytes) = bytes {
                if write_tree_file(files_dir, rel, &bytes) {
                    written += 1;
                    // Flush to SD periodically: a single commit after a big
                    // multi-file extraction (Super Brawl 2: 135 files / 109 MB)
                    // overflows the fsdev journal and silently loses some writes
                    // (the file reports written but later fs::read gives ENOENT).
                    if written % 16 == 0 {
                        crate::sd::commit();
                    }
                } else {
                    failed += 1;
                }
                if name.to_ascii_lowercase().ends_with(".swf") {
                    let is_launch = launch_entry
                        .as_deref()
                        .is_some_and(|le| percent_decode(&name).eq_ignore_ascii_case(le));
                    // `name` is still borrowed by `rel` here, so clone it (the
                    // entry name is short) rather than moving.
                    if is_launch {
                        launch_swf = Some((bytes, name.clone()));
                        first_swf = None; // free the fallback's (large) buffer
                    } else if first_swf.is_none() && launch_swf.is_none() {
                        first_swf = Some((bytes, name.clone()));
                    }
                }
            } else {
                inflate_fail += 1;
                net::log(&std::format!("extract: inflate/method failed for {} (method {})\n", name, method));
            }
        }
        i = data_end;
    }
    net::log(&std::format!(
        "extract: {} entries, {} written, {} write-failed, {} inflate-failed\n",
        entries, written, failed, inflate_fail,
    ));
    launch_swf.or(first_swf)
}

// ── Multi-file games: companion SWF fetch ─────────────────────────────────────

/// Build the Flashpoint "Legacy htdocs" base URL for a game's companion files
/// from the GameZIP entry name. The entry is `content/<host>/<path>/<file>.swf`;
/// companions live next to it on the public mirror at
/// `https://infinity.unstable.life/Flashpoint/Legacy/htdocs/<host>/<path>/`
/// (the db-api GameZIP often ships ONLY the main SWF — the full set is here).
/// Returns None if the entry isn't under `content/` or has no directory.
/// Map a Flashpoint `launchCommand` URL to the FULL htdocs URL of its entry SWF,
/// for downloading a non-zipped (legacy "loose") game directly from the mirror.
/// `http://localflash/icytower/icy_tower.swf`
///   -> `https://infinity.unstable.life/Flashpoint/Legacy/htdocs/localflash/icytower/icy_tower.swf`.
/// Returns None when the command isn't a usable host/path URL (so callers can
/// treat "no htdocs URL" as "not fetchable this way").
pub fn htdocs_url_from_command(launch_command: &str) -> Option<std::string::String> {
    let entry = launch_entry_from_command(launch_command)?; // content/<host>/<path>/<file>
    let rest = entry.strip_prefix("content/")?;
    if rest.is_empty() || rest.ends_with('/') {
        return None;
    }
    Some(std::format!(
        "https://infinity.unstable.life/Flashpoint/Legacy/htdocs/{}",
        rest
    ))
}

/// Base directory (companion) form of `htdocs_url_from_command`, from a GameZIP
/// `content/<host>/<path>/<file>` entry name.
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
