//! Flashpoint GameZIP download — fetch a playable `.swf` for a Flash game the
//! user picks from a Flashpoint search, WITHOUT any auth or OAuth.
//!
//! Pipeline (all PUBLIC endpoints, verified 2026-10-14):
//!   1. search   : flashpointarchive.org/api/search?query=platform:Flash "<word>"...
//!   2. download : download.unstable.life/gib-roms/Games/<uuid>-<ms>.zip -> the GameZIP
//!   3. the ZIP holds `content.json` + `content/{host}{path}/<game>.swf` (deflate)
//!
//! The search gives each game its game data, and the download's name is built
//! from the newest one's `dateAdded` in milliseconds, as Flashpoint's own web
//! player (9o3o) builds it; Flashpoint's admin said building it is fine
//! (2026-10-14). It used to be db-api's `/get?id=`, which found the ZIP from
//! the UUID alone, but db-api stopped taking new games on 2025-08-17 and
//! serves the data of that day (a game fixed since downloads unfixed); `/get`
//! is kept for a game whose data date cannot be read. We unzip with `flate2`
//! (already in the dep tree — see `extract_first_swf`). CONTENT-NEUTRAL stays:
//! the user searches and picks a game; we only fetch what they explicitly ask
//! for, live from Flashpoint.

use crate::net;
use crate::sources::flashpoint;
use std::io::{Read, Seek, SeekFrom};

const GET_BASE: &str = "https://db-api.unstable.life/get";
const ZIP_BASE: &str = "https://download.unstable.life/gib-roms/Games";

/// Games a search keeps at most (`parse_search`).
pub const SEARCH_MAX: usize = 60;

/// Games a search asks the route for: all it gives, so the ones left out as
/// not runnable or as repeats leave room for others before `SEARCH_MAX`.
const SEARCH_ASK: usize = 100;

/// db-api's download of a game's GameZIP, by UUID alone: the fallback for a
/// game whose data date cannot be read (`zip_url`).
pub fn get_url(id: &str) -> std::string::String {
    std::format!("{}?id={}", GET_BASE, id)
}

/// Where the GameZIP of game `id` downloads from, its game data dated
/// `data_date` (ISO 8601, as the route gives it): `<id>-<ms>.zip`, the date in
/// milliseconds since 1970. `None` when the date cannot be read.
pub fn zip_url(id: &str, data_date: &str) -> Option<std::string::String> {
    iso_millis(data_date).map(|ms| std::format!("{}/{}-{}.zip", ZIP_BASE, id, ms))
}

/// Milliseconds since 1970 (UTC) of an ISO 8601 date and time:
/// `2025-08-17T19:24:19.379271Z` is 1755458659379. The fraction past the
/// millisecond is cut, not rounded, as the archive names its files (checked
/// on 71 games, every one's ZIP found at that name). An offset (`+02:00`) is
/// taken into account; none, or `Z`, is UTC.
fn iso_millis(s: &str) -> Option<i64> {
    let b = s.trim().as_bytes();
    if b.len() < 19 || b[4] != b'-' || b[7] != b'-' || !matches!(b[10], b'T' | b't' | b' ') || b[13] != b':' || b[16] != b':' {
        return None;
    }
    let num = |from: usize, to: usize| -> Option<i64> {
        let mut n = 0i64;
        for &c in &b[from..to] {
            if !c.is_ascii_digit() {
                return None;
            }
            n = n * 10 + (c - b'0') as i64;
        }
        Some(n)
    };
    let (y, mo, d) = (num(0, 4)?, num(5, 7)?, num(8, 10)?);
    let (h, mi, sec) = (num(11, 13)?, num(14, 16)?, num(17, 19)?);
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) {
        return None;
    }
    let mut i = 19;
    let mut ms = 0i64;
    if b.get(i) == Some(&b'.') {
        i += 1;
        let start = i;
        while i < b.len() && b[i].is_ascii_digit() {
            if i - start < 3 {
                ms = ms * 10 + (b[i] - b'0') as i64;
            }
            i += 1;
        }
        // ".5" is 500 ms, not 5.
        for _ in (i - start).min(3)..3 {
            ms *= 10;
        }
    }
    let offset_min = match b.get(i) {
        None | Some(b'Z') | Some(b'z') => 0,
        Some(&sign) if (sign == b'+' || sign == b'-') && b.len() >= i + 6 && b[i + 3] == b':' => {
            let m = num(i + 1, i + 3)? * 60 + num(i + 4, i + 6)?;
            if sign == b'-' { -m } else { m }
        }
        _ => return None,
    };
    // Days since 1970-01-01 of a civil date (H. Hinnant's days_from_civil).
    let yy = if mo <= 2 { y - 1 } else { y };
    let era = yy.div_euclid(400);
    let yoe = yy - era * 400;
    let mp = (mo + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(days * 86_400_000 + (h * 3600 + mi * 60 + sec) * 1000 + ms - offset_min * 60_000)
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

/// The route's search for a game named `name` (`flashpoint::query_words`),
/// among Flash games. Split from `parse_search` so the Flashpoint game search
/// can run through the async GET path (spinner) instead of blocking the UI on
/// a synchronous HTTP. `filter` maps to Flashpoint's content filter (its
/// "Filter entries" checkbox): `true` (the default) excludes entries the
/// archive flags as extreme — same default the cover search in flashpoint.rs
/// uses. The importer can flip it to `false` (ZL+ZR in the results) to
/// surface the mature-rated catalogue, matching the official launcher.
pub fn search_url(name: &str, filter: bool) -> std::string::String {
    // The words first: see `flashpoint::query_words`.
    let query = std::format!("{} platform:Flash", flashpoint::query_words(name));
    flashpoint::api_url(&query, SEARCH_ASK, !filter, false)
}

/// Parse the route's answer (`flashpoint::api_games`) into up to 60 fetchable
/// `CatalogEntry` hits. A game with game data downloads its GameZIP
/// (`zip_url`, from the newest of them); a legacy "loose" game has none, but
/// its files live on the htdocs mirror, fetched via its launchCommand, so it is
/// KEPT when that command maps to a usable htdocs URL (and dropped otherwise —
/// nothing to download). Deduped: the archive holds the same game several
/// times (exact UUID repeats AND same-title alternate entries), which clutters
/// the cover grid; we keep the first of each.
pub fn parse_search(
    bytes: &[u8],
) -> Result<std::vec::Vec<flashpoint::CatalogEntry>, std::string::String> {
    const MAX: usize = SEARCH_MAX;
    let json: serde_json::Value = serde_json::from_slice(bytes).map_err(|e| {
        net::log(&std::format!("gamezip: search JSON parse failed: {}\n", e));
        crate::loc::err_json(&e.to_string())
    })?;
    // Anything but the route's answer is not an answer to a search: say it was
    // unreadable, not the archive.org wording this used to borrow.
    let arr = flashpoint::api_games(&json)
        .ok_or_else(|| std::string::String::from(crate::loc_ui::ui().imp_err_fp_unreadable))?;
    let mut out: std::vec::Vec<flashpoint::CatalogEntry> = std::vec::Vec::new();
    let mut seen_ids: std::vec::Vec<std::string::String> = std::vec::Vec::new();
    let mut seen_titles: std::vec::Vec<std::string::String> = std::vec::Vec::new();
    for g in arr {
        let id = g.get("id").and_then(|v| v.as_str()).unwrap_or("");
        if id.is_empty() {
            continue;
        }
        // The newest game data is the one the archive plays: a game fixed
        // since it was added has a newer one, and both stay downloadable.
        let data = g
            .get("gameData")
            .and_then(|v| v.as_array())
            .and_then(|all| all.iter().max_by(|a, b| str_of(a, "dateAdded").cmp(str_of(b, "dateAdded"))));
        let zipped = data.is_some();
        // The game data's command names the entry IN its ZIP; the game's own
        // (legacy) one is where it was before it had any, and may point
        // elsewhere (Hot Dog Bush, issue #72).
        let launch_command = match data.map(|d| str_of(d, "launchCommand")) {
            Some(lc) if !lc.is_empty() => lc,
            _ => str_of(g, "legacyLaunchCommand"),
        }
        .to_string();
        // A game without game data has no GameZIP, but its files live on the
        // htdocs mirror, reachable from the launchCommand URL — keep it only
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
        // Title AND developer: two games can share a title. The archive has
        // two "Sonny" (Y8's arcade game, Armor Games' RPG), and the title alone
        // kept the first, so the RPG could not be found at all (#131).
        let title_key = std::format!(
            "{}\u{1}{}",
            title.trim().to_ascii_lowercase(),
            str_of(g, "developer").trim().to_ascii_lowercase()
        );
        if !title.trim().is_empty() && seen_titles.iter().any(|s| *s == title_key) {
            continue;
        }
        seen_ids.push(id.to_string());
        if !title.trim().is_empty() {
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
        // A date that cannot be read leaves the download to db-api's `/get`.
        let zip = data
            .map(|d| zip_url(id, str_of(d, "dateAdded")).unwrap_or_else(|| get_url(id)))
            .unwrap_or_default();
        let size = data.and_then(|d| d.get("size")).and_then(|v| v.as_u64()).unwrap_or(0);
        out.push(flashpoint::CatalogEntry {
            id: id.to_string(),
            title,
            developer,
            publisher,
            release_date,
            cover_url: flashpoint::cover_url(id),
            launch_command,
            zipped,
            zip_url: zip,
            size,
            description: str_of(g, "originalDescription").trim().to_string(),
        });
        if out.len() >= MAX {
            break;
        }
    }
    Ok(out)
}

/// The text field `key` of a JSON object, "" when it has none.
fn str_of<'a>(v: &'a serde_json::Value, key: &str) -> &'a str {
    v.get(key).and_then(|v| v.as_str()).unwrap_or("")
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

/// One local file header of a GameZIP. An entry of 4 GB or more says
/// 0xFFFFFFFF in its 32-bit sizes and keeps the real ones in a ZIP64 extra
/// field (#130: Happy Wheels' level database, 18 GB in one entry); its header
/// "MUST include BOTH" sizes there, original first (APPNOTE 4.5.3).
struct LocalEntry {
    method: u16,
    csize: u64,
    usize: u64,
    name_off: u64,
    nlen: u64,
    data_off: u64,
}

impl LocalEntry {
    fn next_off(&self) -> u64 {
        self.data_off + self.csize
    }
}

/// The local header at `off`, or `None` past the last one, on a read error, or
/// for an entry whose sizes cannot be known from it (a data descriptor, which
/// Flashpoint's archives do not use, or 0xFFFFFFFF with no ZIP64 field).
fn read_local_entry(f: &mut std::fs::File, off: u64) -> Option<LocalEntry> {
    let mut header = [0u8; 30];
    if !read_at(f, off, &mut header) || &header[0..4] != b"PK\x03\x04" {
        return None;
    }
    let flags = u16::from_le_bytes([header[6], header[7]]);
    let method = u16::from_le_bytes([header[8], header[9]]);
    let csize32 = u32::from_le_bytes([header[18], header[19], header[20], header[21]]);
    let usize32 = u32::from_le_bytes([header[22], header[23], header[24], header[25]]);
    let nlen = u16::from_le_bytes([header[26], header[27]]) as u64;
    let elen = u16::from_le_bytes([header[28], header[29]]) as u64;
    if flags & 0x0008 != 0 {
        return None;
    }
    let (mut csize, mut usize_) = (csize32 as u64, usize32 as u64);
    if csize32 == 0xFFFF_FFFF || usize32 == 0xFFFF_FFFF {
        let mut extra = std::vec![0u8; elen as usize];
        if !read_at(f, off + 30 + nlen, &mut extra) {
            return None;
        }
        let mut sizes = None;
        let mut i = 0usize;
        while i + 4 <= extra.len() {
            let id = u16::from_le_bytes([extra[i], extra[i + 1]]);
            let len = u16::from_le_bytes([extra[i + 2], extra[i + 3]]) as usize;
            let body = extra.get(i + 4..i + 4 + len)?;
            if id == 0x0001 && body.len() >= 16 {
                let u = u64::from_le_bytes(body[0..8].try_into().ok()?);
                let c = u64::from_le_bytes(body[8..16].try_into().ok()?);
                sizes = Some((c, u));
            }
            i += 4 + len;
        }
        (csize, usize_) = sizes?;
    }
    Some(LocalEntry {
        method,
        csize,
        usize: usize_,
        name_off: off + 30,
        nlen,
        data_off: off + 30 + nlen + elen,
    })
}

/// Strip one pair of surrounding quotes from a Flashpoint `launchCommand`. The
/// db wraps commands whose URL contains a space in double quotes (e.g.
/// `"http://i.4cdn.org/f/I'm Dead.swf"`); left in, the trailing `"` sticks to the
/// entry name and never matches the zip's literal paths (and the `.swf` sniff
/// below fails). ~112 games in a 151k-game sample carry quoted commands.
pub fn unquote(launch_command: &str) -> &str {
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
/// Read the JS `config` object off a DISNEY container page.
///
/// Disney/Yamago minigames don't carry FlashVars at all: their `index.html`
/// declares a plain object and hands it to `disneygames-iframe.js`, which is
/// what builds the movie's parameters in the browser. That script is not
/// archived, so we read the object ourselves. Shape (Tron Uprising: Escape from
/// Argon City, verified against its GameZIP 2026-07-31):
///
/// ```text
/// var config = {"apiVersion":"2.5","assetId":"4c766a2f...","elementId":"DisneyGame",
///               "filename":"trnu_spl_act_escapefromargoncity.swf","techType":"AS3", ...};
/// disneyGames.addGame(config);
/// ```
///
/// Returns the raw key/value pairs, or empty when the page isn't one of these.
/// `synthesize_disney_params` turns them into the parameters the movie reads.
pub fn disney_config_from_html(
    html: &[u8],
) -> std::vec::Vec<(std::string::String, std::string::String)> {
    let text = std::string::String::from_utf8_lossy(html);
    let lower = text.to_ascii_lowercase();
    let mut out: std::vec::Vec<(std::string::String, std::string::String)> = std::vec::Vec::new();
    // Only these pages: the marker is the container script's own entry point.
    if !lower.contains("disneygames") {
        return out;
    }
    // The object initialiser of `config`/`gameConfig`, or the argument of
    // `addGame({...})` when the page inlines it.
    for needle in ["addgame(", "config ="] {
        let mut from = 0usize;
        while let Some(rel) = lower[from..].find(needle) {
            let at = from + rel;
            from = at + needle.len();
            let tail = &text[at..];
            let Some(orel) = tail.find('{') else { continue };
            if orel > 40 {
                continue; // too far to be this identifier's initialiser
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
    }
    out
}

/// Build the parameters a Disney/Yamago minigame reads (`apiUrl`,
/// `apiConfigUrl`, `gameConfigUrl` and friends) from its container `config` and
/// the movie's own URL, the way `disneygames-iframe.js` would.
///
/// The API SWF these point at is NOT archived anywhere (checked: the Flashpoint
/// legacy htdocs mirror 404s on `/v1/game_container/**`), so loading it fails.
/// That is fine and is the whole point: a load that FAILS raises an IOError the
/// game can handle, whereas the null URL it gets today throws TypeError #2007
/// out of `Loader.load()` during construction, which kills the movie before a
/// single frame runs. Passing plausible URLs is what buys the game its chance.
pub fn synthesize_disney_params(
    config: &[(std::string::String, std::string::String)],
    movie_url: &str,
) -> std::vec::Vec<(std::string::String, std::string::String)> {
    let get = |k: &str| -> std::string::String {
        config
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(k))
            .map(|(_, v)| v.clone())
            .unwrap_or_default()
    };
    // `http://host/v1/files/<id>/<dir>/game.swf` -> host root and game dir.
    let host_root = movie_url
        .splitn(4, '/')
        .take(3)
        .collect::<std::vec::Vec<_>>()
        .join("/");
    let game_dir = movie_url
        .rsplit_once('/')
        .map(|(d, _)| d.to_string())
        .unwrap_or_else(|| movie_url.to_string());
    let gc = std::format!("{}/v1/game_container", host_root);
    let asset_id = get("assetId");
    let tech = {
        let t = get("techType");
        if t.is_empty() { std::string::String::from("AS3") } else { t }
    };
    // Only the AS3 API filename has ever been observed (Agent P Strikes Back);
    // an AS2 title would want the `MinigameAPI_*` family instead. Neither file
    // is archived, so the exact name changes nothing beyond the log.
    let api_swf = if tech.eq_ignore_ascii_case("AS3") {
        "swf/as3MinigameApi_2_5_6.swf"
    } else {
        "swf/MinigameAPI_2_2.swf"
    };
    let div = {
        let d = get("elementId");
        if d.is_empty() { std::string::String::from("DisneyGame") } else { d }
    };
    std::vec![
        (std::string::String::from("id"), asset_id.clone()),
        (std::string::String::from("game"), asset_id),
        (std::string::String::from("gameDivId"), div),
        (std::string::String::from("fileUrl"), movie_url.to_string()),
        (std::string::String::from("type"), tech),
        (
            std::string::String::from("gameConfigUrl"),
            std::format!("{}/game_config.xml", game_dir),
        ),
        (std::string::String::from("apiUrl"), std::format!("{}/{}", gc, api_swf)),
        (
            std::string::String::from("apiConfigUrl"),
            std::format!("{}/xml/minigameAPIConfig.xml", gc),
        ),
    ]
}

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
    let mut name_buf: std::vec::Vec<u8> = std::vec::Vec::new();
    let mut comp: std::vec::Vec<u8> = std::vec::Vec::new();
    let mut off: u64 = 0;
    while off + 30 <= file_len {
        // Data descriptor / unreadable sizes: stop (see `read_local_entry`).
        let Some(e) = read_local_entry(f, off) else {
            break;
        };
        let (method, csize, usize_, data_off, next_off) = (e.method, e.csize, e.usize, e.data_off, e.next_off());
        if next_off > file_len {
            break;
        }
        name_buf.resize(e.nlen as usize, 0);
        if !read_at(f, e.name_off, &mut name_buf) {
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
pub(crate) fn percent_decode(s: &str) -> std::string::String {
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

extern "C" {
    // C++/libnx file write (cpp/src/swf_picker.cpp). Creates parent dirs +
    // writes the file. Returns 1 on success, 0 on failure. We go through C++
    // because Rust's std::fs on Horizon silently fails to persist some files
    // during a big multi-file extraction (write returns Ok but the file is
    // later unreadable; std::fs::metadata even returns a timestamp as the size).
    fn swf_picker_write_file(path: *const core::ffi::c_char, data: *const u8, len: u32) -> core::ffi::c_int;
}

extern "C" {
    // One file written in pieces (cpp/src/swf_picker.cpp), a concatenation file
    // from 4 GB on. 1 on success; `keep` 0 removes what was written.
    fn swf_picker_stream_open(path: *const core::ffi::c_char, size: u64) -> core::ffi::c_int;
    fn swf_picker_stream_write(data: *const u8, len: u32) -> core::ffi::c_int;
    fn swf_picker_stream_close(keep: core::ffi::c_int) -> core::ffi::c_int;
}

/// Extract entry `e` to `<files_dir>/<rel>` in pieces, never holding it whole:
/// read a megabyte of the zip, inflate it, write what came out, again. For an
/// entry over the in-memory cap (#130: 18 GB of SQLite in Happy Wheels).
/// `on_progress` moves with the bytes read, since one entry can take minutes.
/// False when the card refused a write or the data did not inflate to the size
/// the header announced; nothing of the file is left then.
fn stream_entry(
    f: &mut std::fs::File,
    e: &LocalEntry,
    files_dir: &str,
    rel: &str,
    file_len: u64,
    on_progress: &mut dyn FnMut(u64, u64),
) -> bool {
    let full = std::format!("{}/{}", files_dir.trim_end_matches('/'), rel);
    let Ok(c_path) = std::ffi::CString::new(full.clone()) else {
        return false;
    };
    if e.method != 0 && e.method != 8 {
        net::log(&std::format!("extract: {} uses method {}, skipped\n", full, e.method));
        return true;
    }
    if unsafe { swf_picker_stream_open(c_path.as_ptr(), e.usize) } == 0 {
        return false;
    }
    net::log(&std::format!("extract: {} streamed ({} -> {} bytes)\n", full, e.csize, e.usize));
    const IN_CHUNK: usize = 1024 * 1024;
    const OUT_CHUNK: usize = 4 * 1024 * 1024;
    let mut input = std::vec![0u8; IN_CHUNK];
    let mut output = std::vec![0u8; OUT_CHUNK];
    let mut inflater = flate2::Decompress::new(false);
    let mut read_done: u64 = 0;
    let mut written: u64 = 0;
    let mut ok = f.seek(SeekFrom::Start(e.data_off)).is_ok();
    while ok && read_done < e.csize {
        let n = (e.csize - read_done).min(IN_CHUNK as u64) as usize;
        if !read_full(f, &mut input[..n]) {
            ok = false;
            break;
        }
        read_done += n as u64;
        on_progress(e.data_off + read_done, file_len);
        if e.method == 0 {
            ok = unsafe { swf_picker_stream_write(input.as_ptr(), n as u32) } != 0;
            written += n as u64;
            continue;
        }
        let mut slice = &input[..n];
        loop {
            let before_in = inflater.total_in();
            let before_out = inflater.total_out();
            let status = inflater.decompress(slice, &mut output, flate2::FlushDecompress::None);
            let used = (inflater.total_in() - before_in) as usize;
            let made = (inflater.total_out() - before_out) as usize;
            slice = &slice[used..];
            if made > 0 {
                if unsafe { swf_picker_stream_write(output.as_ptr(), made as u32) } == 0 {
                    ok = false;
                    break;
                }
                written += made as u64;
            }
            match status {
                Ok(flate2::Status::StreamEnd) => break,
                Ok(_) => {
                    // Room left in the output and nothing taken: this chunk is
                    // used up, the next one goes on.
                    if slice.is_empty() && made < OUT_CHUNK {
                        break;
                    }
                    if used == 0 && made == 0 {
                        ok = false;
                        break;
                    }
                }
                Err(_) => {
                    ok = false;
                    break;
                }
            }
        }
    }
    if ok && written != e.usize {
        net::log(&std::format!("extract: {} inflated to {} bytes, header said {}\n", full, written, e.usize));
        ok = false;
    }
    let closed = unsafe { swf_picker_stream_close(ok as core::ffi::c_int) } != 0;
    ok && closed
}

/// Write `bytes` to `<files_dir>/<rel>` (rel is a forward-slash `content/`-
/// relative path like `host/path/file.swf`), creating parent dirs. Best-effort.
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
/// back to the BEST-RANKED `.swf` (see `fallback_rank`) when the command is empty
/// or its entry is absent from the archive.
/// Returns None if no SWF is found. Bails (stops) on data-descriptor entries —
/// Flashpoint GameZIPs don't use them (verified) and their local-header sizes
/// would be unreliable.
/// Hosts whose bundled SWFs are ad / analytics shims, never the game itself.
/// Flashpoint GameZIPs archive the whole page a game was served from, ad frames
/// included, so these sit in the archive next to the real thing.
fn is_ad_shim_entry(entry_name: &str) -> bool {
    const AD_HOSTS: [&str; 6] = [
        "adserver.bigwigmedia.com",
        "ad4game.com",
        "mochiads.com",
        "mochibot.com",
        "doubleclick.net",
        "google-analytics.com",
    ];
    let lower = entry_name.to_ascii_lowercase();
    AD_HOSTS.iter().any(|h| lower.contains(h))
}

/// Rank a non-launch `.swf` as a candidate for "the game", when the launchCommand
/// entry is missing from the archive. Higher wins; ties break on the LARGER file.
///
/// This used to be "first `.swf` in the archive", which picks whatever the zip
/// happens to store first. Hot Dog Bush (issue #72) is the case that shows why:
/// db-api gives that uuid two launchCommands, and the one we keep
/// (`localflash/hotdogbushy/...`) is not in the GameZIP, while the other
/// (`cdn.2dplay.com/hot-dog-bush/hot-dog-bush.swf`, 3 MB) is. The archive's first
/// entry is `adserver.bigwigmedia.com/ingamead2.swf` — 4.8 KB of in-game ad
/// server, which we then booted INSTEAD of the game: it played an advert, asked
/// for a banner from a host dead since 2020, traced "Ad completed" and sat there
/// forever, because there is no game behind an ad frame.
///
/// Ranking rather than filtering: if an ad shim is the only `.swf` in the archive
/// it is still returned, exactly as before.
fn fallback_rank(entry_name: &str, launch_file: Option<&str>) -> u8 {
    let file = entry_name.rsplit('/').next().unwrap_or("");
    // Same file name under a different host/path — the game simply moved hosts
    // between the launchCommand and the archived copy. Strongest signal we have.
    if launch_file.is_some_and(|lf| file.eq_ignore_ascii_case(lf)) {
        return 2;
    }
    if is_ad_shim_entry(entry_name) {
        0
    } else {
        1
    }
}

/// Why an extraction produced no game.
pub enum ExtractFail {
    /// Nothing usable in the archive: no `.swf`, a corrupt zip, or every entry
    /// over the per-entry cap.
    NoSwf,
    /// The contents do not fit on the card beside the zip: (the zip's size,
    /// needed, free) bytes.
    NoSpace(u64, u64, u64),
    /// The card refused a write part-way (full, or write-protected).
    Write,
}

/// Kept free on top of what an extraction writes (net.cpp `SPACE_MARGIN`).
const SPACE_MARGIN: u64 = 8 * 1024 * 1024;

/// What extracting the archive will write: the unpacked size of every entry,
/// read from the local headers alone, stopping where the extraction stops.
fn unpacked_size(f: &mut std::fs::File, file_len: u64) -> u64 {
    let mut off: u64 = 0;
    let mut total: u64 = 0;
    while off + 30 <= file_len {
        let Some(e) = read_local_entry(f, off) else {
            break;
        };
        total += e.usize;
        off = e.next_off();
    }
    total
}

/// What a remote GameZIP unpacks to, from its central directory, read with a
/// few HTTP ranges before the download: the room it needs is then known before
/// a byte of it lands, instead of twice the zip (#130: 9.2 GB that unpack to
/// 18 GB). `None` when the server or the archive does not allow it; the
/// download then checks twice its size, as before.
pub fn remote_unpacked_size(url: &str, zip_size: u64) -> Option<u64> {
    const TAIL: u64 = 64 * 1024 + 22;
    if zip_size < 22 {
        return None;
    }
    let tail_start = zip_size.saturating_sub(TAIL);
    let tail = net::get_range(url, tail_start, zip_size - tail_start)?;
    let eocd = tail.windows(4).rposition(|w| w == b"PK\x05\x06")?;
    let rd16 = |b: &[u8], i: usize| u16::from_le_bytes([b[i], b[i + 1]]) as u64;
    let rd32 = |b: &[u8], i: usize| u32::from_le_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]]) as u64;
    let rd64 = |b: &[u8], i: usize| u64::from_le_bytes(b[i..i + 8].try_into().unwrap_or([0; 8]));
    let e = tail.get(eocd..eocd + 22)?;
    let (mut count, mut cd_size, mut cd_off) = (rd16(e, 10), rd32(e, 12), rd32(e, 16));
    // ZIP64: the locator sits right before the classic record and points at
    // the ZIP64 one, which holds the real counts and offsets.
    if eocd >= 20 && &tail[eocd - 20..eocd - 16] == b"PK\x06\x07" {
        let rec_off = rd64(&tail, eocd - 12);
        let rec = net::get_range(url, rec_off, 56)?;
        if rec.len() < 56 || &rec[0..4] != b"PK\x06\x06" {
            return None;
        }
        (count, cd_size, cd_off) = (rd64(&rec, 32), rd64(&rec, 40), rd64(&rec, 48));
    }
    if cd_size == 0 || cd_size > 16 * 1024 * 1024 || cd_off.checked_add(cd_size)? > zip_size {
        return None;
    }
    let cd = net::get_range(url, cd_off, cd_size)?;
    let mut total: u64 = 0;
    let mut p = 0usize;
    let mut seen = 0u64;
    while p + 46 <= cd.len() && &cd[p..p + 4] == b"PK\x01\x02" {
        let mut usize_ = rd32(&cd, p + 24);
        let nlen = rd16(&cd, p + 28) as usize;
        let elen = rd16(&cd, p + 30) as usize;
        let clen = rd16(&cd, p + 32) as usize;
        if usize_ == 0xFFFF_FFFF {
            // In the central directory the ZIP64 field holds only the sizes
            // that overflowed, original size first.
            let extra = cd.get(p + 46 + nlen..p + 46 + nlen + elen)?;
            let mut i = 0usize;
            while i + 4 <= extra.len() {
                let id = rd16(extra, i);
                let len = rd16(extra, i + 2) as usize;
                if id == 0x0001 && len >= 8 && i + 12 <= extra.len() {
                    usize_ = rd64(extra, i + 4);
                }
                i += 4 + len;
            }
        }
        total = total.saturating_add(usize_);
        seen += 1;
        p += 46 + nlen + elen + clen;
    }
    if seen != count {
        net::log(&std::format!("gamezip: central directory lists {} entries, {} read\n", count, seen));
        return None;
    }
    net::log(&std::format!("gamezip: {} entries, {} bytes unpacked (central directory)\n", seen, total));
    Some(total)
}

/// `on_progress(consumed, total)` is called once per entry, in bytes of the ZIP
/// walked so far. It exists because this function BLOCKS for as long as the
/// download did on a big game: Super Smash Flash 2 is 3.1 GB of archive, and the
/// screen stayed on a full download bar for minutes with nothing moving, which
/// reads as a freeze. A player reported it as "not downloading, gets stuck at
/// 3.9G" (#117); the download had in fact finished.
///
/// A closure rather than a renderer handle, so this module keeps knowing nothing
/// about drawing. The caller decides how often a call is worth a frame: one draw
/// per entry would be 1474 vsync waits on SSF2, which is 24 seconds spent
/// waiting for the display rather than extracting.
pub fn extract_gamezip_tree(
    zip_path: &str,
    files_dir: &str,
    launch_command: &str,
    on_progress: &mut dyn FnMut(u64, u64),
) -> Result<(std::vec::Vec<u8>, std::string::String), ExtractFail> {
    // STREAMED extraction: walk the GameZIP's local file headers straight off the
    // SD card, holding only ONE entry's bytes in RAM at a time. The whole archive
    // is never read into memory, so a multi-GB GameZIP (e.g. Super Smash Flash 2
    // ~3.4 GB) extracts within the Switch's ~3.2 GB heap. (The old path read the
    // entire zip into a Vec, capped at 512 MB — anything bigger silently failed.)
    let mut f = std::fs::File::open(zip_path).map_err(|_| ExtractFail::NoSwf)?;
    let file_len = f.seek(SeekFrom::End(0)).map_err(|_| ExtractFail::NoSwf)?;

    // The zip stays on the card until the extraction is done, so its contents
    // have to fit beside it. Checked before the first write, because a card
    // that filled part-way used to keep extracting for minutes, leave gigabytes
    // of half a game behind and then say the archive held no game.
    if let Some(free) = net::sd_free_bytes() {
        let need = unpacked_size(&mut f, file_len) + SPACE_MARGIN;
        if need > free {
            net::log(&std::format!("extract: {} bytes to write, {} free -> not extracting\n", need, free));
            return Err(ExtractFail::NoSpace(file_len, need, free));
        }
    }

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
    const PER_ENTRY_CAP: u64 = 384 * 1024 * 1024;

    // Fallback used only if the launch entry never shows up: keep the best-ranked
    // `.swf` seen so far (bytes, entry name, rank), not the first one.
    let launch_file = launch_entry
        .as_deref()
        .and_then(|e| e.rsplit('/').next())
        .map(|s| s.to_string());
    let mut fallback_swf: Option<(std::vec::Vec<u8>, std::string::String, u8)> = None;
    let mut launch_swf: Option<(std::vec::Vec<u8>, std::string::String)> = None;

    let mut entries = 0usize;
    let mut written = 0usize;
    let mut inflate_fail = 0usize;

    let mut name_buf: std::vec::Vec<u8> = std::vec::Vec::new();
    let mut comp: std::vec::Vec<u8> = std::vec::Vec::new(); // reused; peak = largest entry
    let mut off: u64 = 0;

    while off + 30 <= file_len {
        on_progress(off, file_len);
        // A data descriptor (sizes after the data) or unreadable sizes: stop.
        // ZIP64 sizes are read from the entry's extra field (#130).
        let Some(e) = read_local_entry(&mut f, off) else {
            let mut sig = [0u8; 4];
            if read_at(&mut f, off, &mut sig) && &sig == b"PK\x03\x04" {
                net::log(&std::format!("extract: entry at {} has no usable sizes, stopping\n", off));
            }
            break;
        };
        let (method, csize, usize_, data_off, next_off) = (e.method, e.csize, e.usize, e.data_off, e.next_off());
        if next_off > file_len {
            break;
        }
        name_buf.resize(e.nlen as usize, 0);
        if !read_at(&mut f, e.name_off, &mut name_buf) {
            break;
        }
        let name = std::string::String::from_utf8_lossy(&name_buf).into_owned();
        // Only files under `content/` (skip content.json and directory entries).
        if let Some(rel) = name.strip_prefix("content/").filter(|r| !r.is_empty() && !r.ends_with('/'))
        {
            entries += 1;
            // Too big to hold in memory, and not a movie FlashNX could play
            // anyway: written in pieces straight from the zip to the card, as
            // big as it is (Happy Wheels' 18 GB level database, #130).
            let is_movie = name.to_ascii_lowercase().ends_with(".swf");
            if (csize > PER_ENTRY_CAP || usize_ > PER_ENTRY_CAP) && !is_movie {
                if !stream_entry(&mut f, &e, files_dir, rel, file_len, on_progress) {
                    net::log(&std::format!(
                        "extract: {} entries, {} written, stopped at a refused write ({}, {} bytes)\n",
                        entries, written, name, usize_,
                    ));
                    return Err(ExtractFail::Write);
                }
                written += 1;
                crate::sd::commit();
                off = next_off;
                continue;
            }
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
                    fallback_swf = None; // free the fallback's (large) buffer
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
                        // Every entry after this one would fail the same way:
                        // stop here instead of inflating the rest for nothing.
                        net::log(&std::format!(
                            "extract: {} entries, {} written, stopped at a refused write\n",
                            entries, written,
                        ));
                        return Err(ExtractFail::Write);
                    }
                    if is_swf && launch_swf.is_none() {
                        let rank = fallback_rank(&name, launch_file.as_deref());
                        let better = match &fallback_swf {
                            None => true,
                            Some((b, _, r)) => rank > *r || (rank == *r && bytes.len() > b.len()),
                        };
                        if better {
                            fallback_swf = Some((bytes, name.clone(), rank));
                        }
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
        "extract: {} entries, {} written, {} inflate-failed\n",
        entries, written, inflate_fail,
    ));
    if launch_swf.is_none() {
        if let Some((bytes, name, rank)) = &fallback_swf {
            net::log(&std::format!(
                "extract: launch entry {} absent from the zip -> falling back to {} ({} bytes, rank {})\n",
                launch_entry.as_deref().unwrap_or("(none)"),
                name,
                bytes.len(),
                rank,
            ));
        }
    }
    launch_swf.or_else(|| fallback_swf.map(|(b, n, _)| (b, n))).ok_or(ExtractFail::NoSwf)
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
    let mut out: std::vec::Vec<std::string::String> = std::vec::Vec::new();
    scan_names_with_ext(&buf.data, b"swf", &mut out);
    out
}

/// Extensions a Flash game loads as DATA rather than code: level tables, config,
/// dialogue. Fetched verbatim and never re-scanned, unlike companion SWFs.
const ASSET_EXTS: [&[u8]; 6] = [b"txt", b"xml", b"json", b"dat", b"csv", b"ini"];

/// Scan a (possibly compressed) SWF for the DATA files it loads (`LoadVars.load`,
/// `XML.load`, `loadVariables`), deduped, bare leaf names, in discovery order.
///
/// Why this exists next to `scan_swf_siblings`: that one matches the literal
/// bytes `.swf`, so a game whose levels live in a `.txt` looked entirely
/// self-contained. BFDIA 5b (issue #73) is exactly that — a single
/// `LoadVars.load("levels.txt")`. Without the file the game does not fail, it
/// WAITS: a green background at a steady 60 fps with a 2 ms tick, forever.
pub fn scan_swf_assets(swf_bytes: &[u8]) -> std::vec::Vec<std::string::String> {
    let buf = match swf::decompress_swf(swf_bytes) {
        Ok(b) => b,
        Err(_) => return std::vec::Vec::new(),
    };
    let mut out: std::vec::Vec<std::string::String> = std::vec::Vec::new();
    for ext in ASSET_EXTS {
        scan_names_with_ext(&buf.data, ext, &mut out);
    }
    out
}

/// Append every `<name>.<ext>` string constant found in `data` to `out`, deduped
/// case-insensitively. Walks back over `[A-Za-z0-9_-]` from the dot, so a load
/// from a subdirectory yields just the leaf name — enough for the flat companion
/// layout these games use. Requires a non-alphanumeric byte after the extension,
/// so `.txtures` is not read as a `.txt` file.
fn scan_names_with_ext(data: &[u8], ext: &[u8], out: &mut std::vec::Vec<std::string::String>) {
    let n = data.len();
    let elen = ext.len();
    let mut i = 0usize;
    while i + 1 + elen <= n {
        let is_ext = data[i] == b'.'
            && data[i + 1..i + 1 + elen]
                .iter()
                .zip(ext)
                .all(|(a, b)| a.eq_ignore_ascii_case(b))
            && data
                .get(i + 1 + elen)
                .map_or(true, |c| !c.is_ascii_alphanumeric());
        if is_ext {
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
                if let Ok(s) = std::str::from_utf8(&data[start..i + 1 + elen]) {
                    if !out.iter().any(|x| x.eq_ignore_ascii_case(s)) {
                        out.push(s.to_string());
                    }
                }
            }
            i += 1 + elen;
        } else {
            i += 1;
        }
    }
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
