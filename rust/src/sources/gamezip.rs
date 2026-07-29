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
use std::io::{Read, Seek, SeekFrom};

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
        // Keep only entries we can actually run. A bare `.swf` launchCommand is
        // always runnable. An HTML wrapper (an `index.html` that embeds the game
        // SWF — e.g. Disney minigames like Agent P Strikes Back) is runnable too,
        // but ONLY for a zipped game: we read the wrapper out of the GameZIP after
        // download to find the real entry SWF (see `resolve_html_launch_entry`).
        // A non-zipped HTML entry has no GameZIP to read the wrapper from, so it
        // is dropped. This runs BEFORE the 60-game MAX cap, so freed slots fill
        // with runnable games. (The importer also guards the download, as a
        // backstop.)
        let runnable = launch_command_is_swf(&launch_command)
            || (zipped && launch_command_is_html(&launch_command));
        if !runnable {
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

/// Fill `buf` completely from the file's current cursor; false on short read /
/// error. Mirrors the manual read loop `read_file_bounded` uses (trusted on
/// Horizon, where a single `read`/`read_exact` can return a short count).
fn read_full(f: &mut std::fs::File, buf: &mut [u8]) -> bool {
    let mut done = 0usize;
    while done < buf.len() {
        match f.read(&mut buf[done..]) {
            Ok(0) => return false,
            Ok(n) => done += n,
            Err(_) => return false,
        }
    }
    true
}

/// Seek to absolute `off`, then fill `buf`. Used to walk a GameZIP's local file
/// headers straight off the SD card so the whole (multi-GB) archive never has to
/// be held in RAM (only the current entry's compressed bytes are).
fn read_at(f: &mut std::fs::File, off: u64, buf: &mut [u8]) -> bool {
    f.seek(SeekFrom::Start(off)).is_ok() && read_full(f, buf)
}

/// Strip one pair of surrounding quotes from a Flashpoint `launchCommand`. The
/// db wraps commands whose URL contains a space in double quotes (e.g.
/// `"http://i.4cdn.org/f/I'm Dead.swf"`); left in, the trailing `"` sticks to the
/// entry name and never matches the zip's literal paths (and the `.swf` sniff
/// below fails). ~112 games in a 151k-game sample carry quoted commands.
fn unquote(launch_command: &str) -> &str {
    let t = launch_command.trim();
    let b = t.as_bytes();
    if b.len() >= 2
        && ((b[0] == b'"' && b[b.len() - 1] == b'"')
            || (b[0] == b'\'' && b[b.len() - 1] == b'\''))
    {
        t[1..t.len() - 1].trim()
    } else {
        t
    }
}

/// Map a Flashpoint `launchCommand` URL to its ZIP content entry name:
/// `http://i.flipline.com/gamefiles/papalouie2/PapaLouie2_v2_1.swf?x=1`
///   -> `content/i.flipline.com/gamefiles/papalouie2/PapaLouie2_v2_1.swf`.
/// Returns None if empty or not a URL-shaped command.
fn launch_entry_from_command(launch_command: &str) -> Option<std::string::String> {
    let lc = unquote(launch_command);
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

/// True if a Flashpoint `launchCommand` points at a bare `.swf` entry — the only
/// kind we can run. Games launched through an HTML page + FlashVars (e.g. Dragon
/// City's `.../index.html`, whose page `embedSWF()`s a loader SWF that needs
/// FlashVars and a live backend) return false. The importer refuses those up
/// front instead of downloading a huge GameZIP that would just dead-end on a
/// black screen / "no .swf" error.
pub fn launch_command_is_swf(launch_command: &str) -> bool {
    let lc = unquote(launch_command);
    let path = lc.split(['?', '#']).next().unwrap_or(lc);
    path.trim_end_matches('/').to_ascii_lowercase().ends_with(".swf")
}

/// True if a Flashpoint `launchCommand` points at an HTML wrapper (`index.html`
/// / `*.htm[l]`) that embeds the real game SWF via `embedSWF()` or a config
/// object (e.g. Disney minigames). We can run these for a ZIPPED game by reading
/// the wrapper out of the GameZIP and resolving the SWF it references — see
/// `resolve_html_launch_entry`. Flashpoint sometimes appends `@<params>` to the
/// entry filename (e.g. `index.html@refOverride=na`), so the extension is checked
/// on the leaf segment with any trailing `@...` stripped.
pub fn launch_command_is_html(launch_command: &str) -> bool {
    let lc = unquote(launch_command);
    let path = lc.split(['?', '#']).next().unwrap_or(lc);
    let seg = path.rsplit('/').next().unwrap_or(path);
    let seg = seg.split('@').next().unwrap_or(seg);
    let seg = seg.trim_end_matches('/').to_ascii_lowercase();
    seg.ends_with(".html") || seg.ends_with(".htm")
}

/// Build the original game URL from a GameZIP content-entry name:
/// `content/<host>/<path>/<file>` -> `http://<host>/<path>/<file>`. Used to set
/// the movie's base URL for an HTML-wrapped game to its REAL entry SWF — the
/// launchCommand pointed at the `index.html` wrapper, but we extract and launch
/// the SWF it embeds, so the `.base` sidecar must name that SWF for its relative
/// loads (game_config.xml, companion SWFs) to resolve against the extracted tree.
pub fn entry_url_from_name(entry_name: &str) -> Option<std::string::String> {
    let rest = entry_name.strip_prefix("content/")?;
    if rest.is_empty() || rest.ends_with('/') {
        return None;
    }
    Some(std::format!("http://{}", rest))
}

/// Read the entry SWF filename an HTML wrapper embeds. Handles the common cases:
/// a config object (`"filename":"<name>.swf"`, Disney minigame container),
/// swfobject/`embedSWF("<name>.swf", ...)`, or — as a fallback — the first
/// quoted `*.swf` string on the page. Prefers a value anchored to a
/// `filename`/`embedSWF` key so an ad/loader SWF quoted earlier doesn't win.
/// Query/hash are stripped; the reference is returned verbatim (may be a
/// relative sub-path like `swf/game.swf`, resolved by the caller).
fn html_entry_swf(html: &[u8]) -> Option<std::string::String> {
    let text = std::string::String::from_utf8_lossy(html);
    let lower = text.to_ascii_lowercase();
    let anchor = lower.find("filename").or_else(|| lower.find("embedswf"));
    let bytes = text.as_bytes();
    let mut first: Option<std::string::String> = None;
    let mut anchored: Option<std::string::String> = None;
    let mut i = 0usize;
    while i < bytes.len() {
        let c = bytes[i];
        if c == b'"' || c == b'\'' {
            let quote = c;
            let start = i + 1;
            let mut j = start;
            while j < bytes.len() && bytes[j] != quote {
                j += 1;
            }
            if let Ok(s) = std::str::from_utf8(&bytes[start..j.min(bytes.len())]) {
                let path = s.split(['?', '#']).next().unwrap_or(s).trim();
                if !path.is_empty() && path.to_ascii_lowercase().ends_with(".swf") {
                    if first.is_none() {
                        first = Some(path.to_string());
                    }
                    if let Some(a) = anchor {
                        if start > a && anchored.is_none() {
                            anchored = Some(path.to_string());
                        }
                    }
                }
            }
            i = j + 1;
        } else {
            i += 1;
        }
    }
    anchored.or(first)
}

/// Push a trimmed key/value pair, dropping empties and surrounding whitespace.
fn push_flashvar(
    key: &mut std::string::String,
    val: &mut std::string::String,
    out: &mut std::vec::Vec<(std::string::String, std::string::String)>,
) {
    let k = key.trim().to_string();
    let v = val.trim().to_string();
    key.clear();
    val.clear();
    if !k.is_empty() {
        out.push((k, v));
    }
}

/// Parse the body of a JS object literal (`"k": v, "k2": v2`) into pairs.
/// Quote-aware and nesting-aware so a value containing `,` or `:` survives.
fn parse_js_object(body: &str, out: &mut std::vec::Vec<(std::string::String, std::string::String)>) {
    let (mut key, mut val) = (std::string::String::new(), std::string::String::new());
    let mut in_key = true;
    let mut quote: Option<char> = None;
    let mut depth = 0i32;
    for c in body.chars() {
        if let Some(q) = quote {
            if c == q {
                quote = None;
            } else if in_key {
                key.push(c);
            } else {
                val.push(c);
            }
            continue;
        }
        match c {
            '"' | '\'' => quote = Some(c),
            ':' if in_key && depth == 0 => in_key = false,
            ',' if depth == 0 => {
                push_flashvar(&mut key, &mut val, out);
                in_key = true;
            }
            '{' | '[' => {
                depth += 1;
                if !in_key {
                    val.push(c);
                }
            }
            '}' | ']' => {
                depth -= 1;
                if !in_key {
                    val.push(c);
                }
            }
            _ => {
                if in_key {
                    key.push(c)
                } else {
                    val.push(c)
                }
            }
        }
    }
    push_flashvar(&mut key, &mut val, out);
}

/// Extract the FlashVars an HTML container page passes to its embedded SWF.
///
/// Flashpoint's HTML-wrapped games ship an `index.html` whose JavaScript hands
/// the movie its configuration — WHICH SWF to actually load, where its assets
/// live, feature flags. A browser runs that JS; we don't, so without this the
/// embedded loader starts with no configuration and hangs forever on its own
/// splash (Dragon City: `DCLoader.swf` waits on `swftoload` and never leaves
/// "Loading").
///
/// Two shapes cover the common wrappers:
///   (a) swfobject 2.x — `flashVars = { "swftoload": "flash/Base.swf", ... }`
///   (b) classic embed — `<param name="FlashVars" value="a=1&amp;b=2">`
///
/// Returns the pairs in page order; empty when the page carries none (a game
/// whose container computes them in JS at runtime, e.g. the Disney minigames,
/// still needs its own handling).
pub fn flashvars_from_html(
    html: &[u8],
) -> std::vec::Vec<(std::string::String, std::string::String)> {
    let text = std::string::String::from_utf8_lossy(html);
    let lower = text.to_ascii_lowercase();
    let mut out: std::vec::Vec<(std::string::String, std::string::String)> = std::vec::Vec::new();

    // (a) Object literal. Scan every "flashvars" mention: the first may be an
    // unrelated word, and only a `{` CLOSE BEHIND it is the literal we want.
    let mut from = 0usize;
    while let Some(rel) = lower[from..].find("flashvars") {
        let at = from + rel;
        from = at + "flashvars".len();
        let tail = &text[at..];
        let Some(orel) = tail.find('{') else { continue };
        if orel > 40 {
            continue; // too far away to be this identifier's initialiser
        }
        let open = at + orel;
        let mut depth = 0i32;
        let mut close = None;
        for (i, c) in text[open..].char_indices() {
            if c == '{' {
                depth += 1;
            } else if c == '}' {
                depth -= 1;
                if depth == 0 {
                    close = Some(open + i);
                    break;
                }
            }
        }
        let Some(close) = close else { continue };
        parse_js_object(&text[open + 1..close], &mut out);
        if !out.is_empty() {
            return out;
        }
    }

    // (b) `name="FlashVars" value="a=1&b=2"`, or the same as an <embed> attr.
    // Scan quoted runs from the START of the enclosing tag: the mention itself
    // sits INSIDE a quoted run (`name="FlashVars"`), so pairing quotes from the
    // mention would misalign by one and read ` value=` as the payload.
    if let Some(at) = lower.find("flashvars") {
        let bytes = text.as_bytes();
        let mut i = text[..at].rfind('<').unwrap_or(0);
        while i < bytes.len() {
            let c = bytes[i];
            if c == b'"' || c == b'\'' {
                let start = i + 1;
                let mut j = start;
                while j < bytes.len() && bytes[j] != c {
                    j += 1;
                }
                let run = &text[start..j.min(text.len())];
                // Only a run that STARTS past the mention can be its value.
                if start > at && run.contains('=') {
                    for pair in run.replace("&amp;", "&").split('&') {
                        if let Some((k, v)) = pair.split_once('=') {
                            let k = k.trim();
                            if !k.is_empty() {
                                out.push((k.to_string(), v.trim().to_string()));
                            }
                        }
                    }
                    return out;
                }
                i = j + 1;
            } else if c == b'>' && i > at {
                break; // left the tag without finding a payload
            } else {
                i += 1;
            }
        }
    }
    out
}

/// Resolve an HTML-wrapped game's real entry SWF from inside its GameZIP.
/// `html_entry` is the (percent-decoded) `content/<host>/<path>/index.html…`
/// entry name derived from the launchCommand. Finds that wrapper in the zip,
/// inflates it, reads the SWF filename it embeds (`html_entry_swf`), and returns
/// the resolved `content/<host>/<path>/<file>.swf` entry name to launch. Returns
/// None when there's no matching wrapper or it embeds no local `.swf` (e.g. a
/// pure-HTML5 game) — the caller then falls back to the first `.swf` in the zip.
fn resolve_html_launch_entry(
    f: &mut std::fs::File,
    file_len: u64,
    html_entry: &str,
) -> Option<std::string::String> {
    // Directory of the wrapper (keeps its trailing slash), for resolving the
    // embedded (relative) SWF reference against.
    let dir = html_entry.rfind('/').map(|k| &html_entry[..=k]).unwrap_or("");
    let dir_lower = dir.to_ascii_lowercase();
    let mut header = [0u8; 30];
    let mut name_buf: std::vec::Vec<u8> = std::vec::Vec::new();
    let mut comp: std::vec::Vec<u8> = std::vec::Vec::new();
    let mut off: u64 = 0;
    while off + 30 <= file_len {
        if !read_at(f, off, &mut header) || &header[0..4] != b"PK\x03\x04" {
            break;
        }
        let flags = u16::from_le_bytes([header[6], header[7]]);
        let method = u16::from_le_bytes([header[8], header[9]]);
        let csize = u32::from_le_bytes([header[18], header[19], header[20], header[21]]);
        let usize_ = u32::from_le_bytes([header[22], header[23], header[24], header[25]]);
        let nlen = u16::from_le_bytes([header[26], header[27]]) as u64;
        let elen = u16::from_le_bytes([header[28], header[29]]) as u64;
        if flags & 0x0008 != 0 || csize == 0xFFFF_FFFF || usize_ == 0xFFFF_FFFF {
            break; // data descriptor / ZIP64: sizes not usable from the local header
        }
        let name_off = off + 30;
        let data_off = name_off + nlen + elen;
        let next_off = data_off + csize as u64;
        if next_off > file_len {
            break;
        }
        name_buf.resize(nlen as usize, 0);
        if !read_at(f, name_off, &mut name_buf) {
            break;
        }
        let name = std::string::String::from_utf8_lossy(&name_buf).into_owned();
        // Match the wrapper by its exact (decoded) name, or any `index.htm[l]` in
        // the launch directory (Flashpoint stores both `index.html` and the
        // `index.html@<params>` launch entry — same bytes).
        let dname = percent_decode(&name);
        let dname_lower = dname.to_ascii_lowercase();
        let is_wrapper = dname.eq_ignore_ascii_case(html_entry)
            || dname_lower.starts_with(&std::format!("{}index.htm", dir_lower));
        // Wrappers are tiny HTML; cap the read so a mislabelled giant can't spike RAM.
        if is_wrapper && csize <= 32 * 1024 * 1024 {
            comp.resize(csize as usize, 0);
            let bytes = if read_at(f, data_off, &mut comp) {
                match method {
                    0 => Some(comp.clone()),
                    8 => inflate(&comp, usize_ as usize),
                    _ => None,
                }
            } else {
                None
            };
            if let Some(bytes) = bytes {
                if let Some(swf_ref) = html_entry_swf(&bytes) {
                    let rel = swf_ref.trim_start_matches("./");
                    return Some(std::format!("{}{}", dir, rel));
                }
            }
        }
        off = next_off;
    }
    None
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

/// Percent-encode a mirror `host/path` string, KEEPING the `/` separators.
/// Normalizes first (decode then re-encode) so a launchCommand that already
/// carries `%XX` escapes isn't double-encoded, while a raw UTF-8 path (e.g. a
/// Japanese filename) gets encoded. Without this the htdocs mirror returns
/// HTTP 400/404 for un-encoded non-ASCII paths — the -2 launch error on
/// 包丁少女幻窓曲 (issue #51). ASCII paths (Garfield, Icy Tower…) are unchanged.
fn percent_encode_path(rest: &str) -> std::string::String {
    let decoded = percent_decode(rest);
    let mut out = std::string::String::with_capacity(decoded.len());
    for &b in decoded.as_bytes() {
        let safe = matches!(b,
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9'
            | b'.' | b'-' | b'_' | b'~' | b'/');
        if safe {
            out.push(b as char);
        } else {
            out.push_str(&std::format!("%{:02X}", b));
        }
    }
    out
}

/// Write `bytes` to an absolute sidecar path (creating parent dirs) via the
/// C++/libnx writer, then flush to SD. Used by the navigator's on-demand mirror
/// fetch to cache a dynamically-loaded asset so a replay is offline and a delete
/// cleans it up. Returns true on success.
pub fn write_sidecar_abs(abs_path: &str, bytes: &[u8]) -> bool {
    let c_path = match std::ffi::CString::new(abs_path) {
        Ok(c) => c,
        Err(_) => return false,
    };
    let ok = unsafe {
        swf_picker_write_file(c_path.as_ptr(), bytes.as_ptr(), bytes.len() as u32) != 0
    };
    if ok {
        crate::sd::commit();
    } else {
        net::log(&std::format!(
            "sidecar: C++ write failed {} ({} bytes)\n",
            abs_path,
            bytes.len()
        ));
    }
    ok
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
    zip_path: &str,
    files_dir: &str,
    launch_command: &str,
) -> Option<(std::vec::Vec<u8>, std::string::String)> {
    // STREAMED extraction: walk the GameZIP's local file headers straight off the
    // SD card, holding only ONE entry's bytes in RAM at a time. The whole archive
    // is never read into memory, so a multi-GB GameZIP (e.g. Super Smash Flash 2
    // ~3.4 GB) extracts within the Switch's ~3.2 GB heap. (The old path read the
    // entire zip into a Vec, capped at 512 MB — anything bigger silently failed.)
    let mut f = std::fs::File::open(zip_path).ok()?;
    let file_len = f.seek(SeekFrom::End(0)).ok()?;

    // Decode the launch entry up front so it matches the zip's literal (decoded)
    // entry names; the comparison below decodes each entry name too (robust to a
    // zip that stores encoded names).
    let launch_entry = launch_entry_from_command(launch_command).map(|e| percent_decode(&e));
    // HTML-wrapped game (the launchCommand is an `index.html`, not a bare `.swf`):
    // read the wrapper out of the zip to find the SWF it actually embeds, so we
    // launch the game (e.g. Agent P Strikes Back) instead of `loader.swf` / the
    // first `.swf`. Falls back to the raw entry (then first `.swf`) if unresolved.
    let launch_entry = if !launch_command_is_swf(launch_command) {
        launch_entry
            .as_deref()
            .and_then(|le| resolve_html_launch_entry(&mut f, file_len, le))
            .or(launch_entry)
    } else {
        launch_entry
    };

    // Cap a single entry's buffers so one pathological record can't OOM the heap.
    // Peak RAM is ~the largest single entry (compressed + inflated), not the whole
    // archive. Matches the large-SWF ceiling.
    const PER_ENTRY_CAP: u32 = 384 * 1024 * 1024;

    let mut first_swf: Option<(std::vec::Vec<u8>, std::string::String)> = None;
    let mut launch_swf: Option<(std::vec::Vec<u8>, std::string::String)> = None;

    let mut entries = 0usize;
    let mut written = 0usize;
    let mut failed = 0usize;
    let mut inflate_fail = 0usize;

    let mut header = [0u8; 30];
    let mut name_buf: std::vec::Vec<u8> = std::vec::Vec::new();
    let mut comp: std::vec::Vec<u8> = std::vec::Vec::new(); // reused; peak = largest entry
    let mut off: u64 = 0;

    while off + 30 <= file_len {
        if !read_at(&mut f, off, &mut header) || &header[0..4] != b"PK\x03\x04" {
            break;
        }
        let flags = u16::from_le_bytes([header[6], header[7]]);
        let method = u16::from_le_bytes([header[8], header[9]]);
        let csize = u32::from_le_bytes([header[18], header[19], header[20], header[21]]);
        let usize_ = u32::from_le_bytes([header[22], header[23], header[24], header[25]]);
        let nlen = u16::from_le_bytes([header[26], header[27]]) as u64;
        let elen = u16::from_le_bytes([header[28], header[29]]) as u64;
        if flags & 0x0008 != 0 {
            break; // data descriptor: sizes not in the local header
        }
        if csize == 0xFFFF_FFFF || usize_ == 0xFFFF_FFFF {
            net::log("extract: ZIP64 entry (0xFFFFFFFF size) — unsupported, stopping\n");
            break;
        }
        let name_off = off + 30;
        let data_off = name_off + nlen + elen;
        let next_off = data_off + csize as u64;
        if next_off > file_len {
            break;
        }
        name_buf.resize(nlen as usize, 0);
        if !read_at(&mut f, name_off, &mut name_buf) {
            break;
        }
        let name = std::string::String::from_utf8_lossy(&name_buf).into_owned();
        // Only files under `content/` (skip content.json and directory entries).
        if let Some(rel) = name.strip_prefix("content/").filter(|r| !r.is_empty() && !r.ends_with('/'))
        {
            entries += 1;
            if csize > PER_ENTRY_CAP || usize_ > PER_ENTRY_CAP {
                net::log(&std::format!(
                    "extract: entry {} over per-entry cap ({}->{} bytes), skipped\n",
                    name, csize, usize_,
                ));
                off = next_off;
                continue;
            }
            comp.resize(csize as usize, 0);
            let bytes = if read_at(&mut f, data_off, &mut comp) {
                match method {
                    0 => Some(comp.clone()),
                    8 => inflate(&comp, usize_ as usize),
                    _ => None,
                }
            } else {
                None
            };
            if let Some(bytes) = bytes {
                let is_swf = name.to_ascii_lowercase().ends_with(".swf");
                let is_launch = is_swf
                    && launch_entry
                        .as_deref()
                        .is_some_and(|le| percent_decode(&name).eq_ignore_ascii_case(le));
                // Skip the launch entry's tree copy: it is written flat as the
                // library `<game>.swf`, and the SidecarNavigator serves the entry
                // URL from that flat file (see its layer-0 fallback). Writing it
                // here too would store the whole game twice — up to ~30 MB for a
                // single-SWF GameZIP (e.g. Infiltrating the Airship).
                if is_launch {
                    launch_swf = Some((bytes, name.clone()));
                    first_swf = None; // free the fallback's (large) buffer
                } else {
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
                    if is_swf && first_swf.is_none() && launch_swf.is_none() {
                        first_swf = Some((bytes, name.clone()));
                    }
                }
            } else {
                inflate_fail += 1;
                net::log(&std::format!("extract: inflate/method/read failed for {} (method {})\n", name, method));
            }
        }
        off = next_off;
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
        percent_encode_path(rest)
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
        percent_encode_path(dir)
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
    use std::io::{Seek, SeekFrom};
    let mut f = std::fs::File::open(path).ok()?;
    let mut data: std::vec::Vec<u8> = std::vec::Vec::new();
    // Pre-size from the true on-disk length (seek is reliable on Horizon, unlike
    // metadata) so the chunked read never grows the Vec by doubling — which for a
    // big GameZIP (hundreds of MB) would briefly hold ~2x the final size and OOM.
    // Reject oversize up front; try_reserve_exact fails gracefully (-> None)
    // instead of aborting. Falls back to a growing read if seek is unavailable.
    if let Ok(end) = f.seek(SeekFrom::End(0)) {
        let size = end as usize;
        if size > max {
            return None;
        }
        f.seek(SeekFrom::Start(0)).ok()?;
        data.try_reserve_exact(size).ok()?;
    }
    let mut buf = [0u8; 65536];
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
