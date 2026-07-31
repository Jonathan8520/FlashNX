//! Phase 3.7 — archive.org import layer (HTTPS via C++ libcurl).
//!
//! Splits the work between Rust and C++ along the natural boundary:
//!   - C++ does the curl + TLS + file I/O work (see `cpp/src/net.cpp`).
//!   - Rust does URL parsing, JSON parsing, and state transitions.
//!
//! The CA bundle (Mozilla's ca-bundle.crt from msys2, ~228 KB) is embedded
//! here via `include_bytes!` and written to SD at boot. We pay 228 KB in
//! the .nro but skip a runtime download (the user couldn't bootstrap
//! HTTPS without already having HTTPS).

use core::ffi::{c_char, c_int};

extern "C" {
    fn write_cacert_to_sd(data: *const c_char, len: c_int) -> c_int;
    fn https_get_into_buf(url: *const c_char, buf: *mut c_char, cap: c_int) -> c_int;
    // Synchronous HTTPS POST with a JSON body (bug-report relay). Same return
    // contract as https_get_into_buf (-1 init, -2 transfer, -3 overflow).
    fn https_post_json(url: *const c_char, body: *const c_char, buf: *mut c_char, cap: c_int) -> c_int;
    // Fills `out` with a short description of the last transfer failure
    // ("curl 60 (...) http 0"). Read after a negative `https_get_into_buf`.
    fn https_last_error_desc(out: *mut c_char, cap: c_int);
    // Same failure as raw numbers, so we can map it to a SPECIFIC message.
    fn https_last_curl_code() -> c_int;
    fn https_last_http_code() -> c_int;
    fn https_download_start(url: *const c_char, out_path: *const c_char) -> c_int;
    fn https_download_tick() -> c_int;
    fn https_download_progress(done_out: *mut u64, total_out: *mut u64);
    fn https_download_cancel();
    // Async in-memory GET (archive.org metadata) — same multi machinery as the
    // download, but non-blocking so the UI can spin while it runs.
    fn https_get_start(url: *const c_char) -> c_int;
    fn https_get_tick() -> c_int;
    fn https_get_buffer(out: *mut c_char, cap: c_int) -> c_int;
    fn https_get_cancel();
    // Isolated async GET for cover/logo thumbnails (separate curl handle from
    // the metadata GET above — see net.cpp). Lets the gallery stream logos
    // without ever blocking the render thread.
    fn https_thumb_start(url: *const c_char) -> c_int;
    fn https_thumb_tick() -> c_int;
    fn https_thumb_slot_status(slot: c_int) -> c_int;
    fn https_thumb_slot_take(slot: c_int, out: *mut c_char, cap: c_int) -> c_int;
    fn https_thumb_cancel();
    // Synchronous HEAD → Content-Length (or -1). Flashpoint details popup.
    fn https_head_content_length(url: *const c_char) -> i64;
    // header/guide are localized prompt strings supplied by Rust (loc.rs).
    fn swkbd_prompt_url(header: *const c_char, guide: *const c_char, initial: *const c_char, out: *mut c_char, cap: c_int) -> c_int;
    fn swkbd_prompt_rename(header: *const c_char, guide: *const c_char, initial: *const c_char, out: *mut c_char, cap: c_int) -> c_int;
    fn swkbd_prompt_search(header: *const c_char, guide: *const c_char, initial: *const c_char, out: *mut c_char, cap: c_int) -> c_int;
    fn ruffle_log_cstr(msg: *const c_char);
}

/// NUL-terminate a string for passing to C.
fn cstr(s: &str) -> std::vec::Vec<u8> {
    let mut v = s.as_bytes().to_vec();
    v.push(0);
    v
}

pub(crate) fn log(s: &str) {
    let mut bytes = s.as_bytes().to_vec();
    bytes.push(0);
    unsafe { ruffle_log_cstr(bytes.as_ptr() as *const _) };
}

/// Mozilla CA bundle (ca-bundle.crt copy from /c/devkitPro/msys2/usr/ssl/
/// certs/). 152 root certs as of May 2026. Mosts modern HTTPS servers
/// (archive.org / cloudflare / let's encrypt) verify against this.
const CACERT_PEM: &[u8] = include_bytes!("../../assets/cacert.pem");

/// Called once from `ruffle_library_init`. Writes the embedded CA bundle
/// to `sdmc:/switch/FlashNX/cacert.pem` (idempotent — skips if
/// already present at the right size). libcurl reads from that path via
/// CURLOPT_CAINFO. We can't use CURLOPT_CAINFO_BLOB (added in curl 7.77)
/// because switch-curl is 7.69.
pub fn boot_init() {
    let len = CACERT_PEM.len();
    let rc = unsafe {
        write_cacert_to_sd(CACERT_PEM.as_ptr() as *const c_char, len as c_int)
    };
    // Log presence + size at every boot. A missing/short cacert.pem makes
    // EVERY HTTPS call fail (curl 77/60); when a user reports the import
    // error this line tells us immediately whether the bundle is in place.
    log(&std::format!(
        "net: cacert.pem {} bytes, write_cacert_to_sd rc={}{}\n",
        len, rc,
        if rc != 0 { " (CA verify may fail)" } else { "" },
    ));
}

/// Short description of the last libcurl failure recorded by the C++ layer,
/// e.g. `"curl 60 (SSL peer certificate or SSH remote key was not OK) http 0"`.
/// Used to turn the opaque `-2` import error into something a user (with no
/// nxlink) can actually act on.
fn last_https_error() -> std::string::String {
    let mut buf = std::vec![0u8; 192];
    unsafe { https_last_error_desc(buf.as_mut_ptr() as *mut c_char, buf.len() as c_int) };
    let nul = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    buf.truncate(nul);
    std::string::String::from_utf8_lossy(&buf).into_owned()
}

/// Where the failed transfer was writing. Changes what curl 23 (write error)
/// means: in memory it's OUR cap rejecting an oversized response; to a file it's
/// the SD card refusing the write (full / unwritable).
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Sink {
    Memory,
    Sd,
}

/// The most SPECIFIC message we can give for the last transfer failure. One
/// catch-all sentence ("check the clock, the WiFi and the URL") is worse than
/// nothing: it blames the console clock for a search that merely returned too
/// much data, and it trains the user to ignore the error. The curl code says
/// which failure it actually was, so say that.
pub(crate) fn https_error_message(sink: Sink) -> std::string::String {
    let curl = unsafe { https_last_curl_code() };
    let http = unsafe { https_last_http_code() } as i64;
    let lc = crate::loc::s();
    let owned = |s: &'static str| std::string::String::from(s);
    match curl {
        // CURLE_WRITE_ERROR — our write callback returned short.
        23 => match sink {
            Sink::Memory => owned(lc.err_response_big),
            Sink::Sd => owned(lc.err_sd_write),
        },
        // COULDN'T_RESOLVE_PROXY / _HOST / COULDN'T_CONNECT: no usable network.
        5 | 6 | 7 => owned(lc.err_offline),
        // OPERATION_TIMEDOUT.
        28 => owned(lc.err_timeout),
        // TLS handshake / certificate problems. On Switch the usual cause is a
        // wrong console clock (the cert reads as not-yet-valid), so THIS is the
        // one place the clock advice belongs.
        35 | 51 | 53 | 54 | 58 | 59 | 60 | 66 | 77 | 83 => owned(lc.err_tls),
        // Transfer itself was fine — the server said no (404 / 403 / 429 / 5xx).
        _ if http >= 400 => crate::loc::err_http_status(http),
        _ => crate::loc::err_https(&last_https_error()),
    }
}

/// Generic synchronous HTTPS GET into a freshly-allocated buffer; returns the
/// raw response bytes (truncated to the real length). `cap` bounds the
/// response (the C++ side returns -3 on overflow). Reuses the exact same
/// curl+TLS path as the archive.org metadata fetch — User-Agent `FlashNX/...`,
/// follows redirects, CA bundle from `cacert.pem`. Used by the `sources` layer
/// (Flashpoint search JSON, cover logos) and by `fetch_archive_metadata`.
pub(crate) fn http_get(
    url: &str,
    cap: usize,
) -> Result<std::vec::Vec<u8>, std::string::String> {
    let mut buf = std::vec![0u8; cap];
    let mut url_c = url.as_bytes().to_vec();
    url_c.push(0);
    let n = unsafe {
        https_get_into_buf(
            url_c.as_ptr() as *const c_char,
            buf.as_mut_ptr() as *mut c_char,
            buf.len() as c_int,
        )
    };
    if n == -3 {
        return Err(std::string::String::from(crate::loc::s().err_response_big));
    }
    if n < 0 {
        // -2 = transfer failed (curl/HTTP); name the real cause. Other negatives
        // (-1 init) carry no curl result, so show the raw code.
        if n == -2 {
            return Err(https_error_message(Sink::Memory));
        }
        return Err(crate::loc::err_https(&std::format!("rc {}", n)));
    }
    buf.truncate(n as usize);
    Ok(buf)
}

/// Synchronous HTTPS POST of a JSON `body` to `url`; returns the response
/// bytes (truncated to the real length). Same TLS path as `http_get`. Used by
/// the bug-report relay (`crate::bugreport`). Blocks the caller for the
/// duration (a couple seconds) — run it hoisted out of the LIBRARY lock, like
/// the other HTTPS flows.
pub(crate) fn post_json(
    url: &str,
    body: &str,
    cap: usize,
) -> Result<std::vec::Vec<u8>, std::string::String> {
    let mut buf = std::vec![0u8; cap];
    let url_c = cstr(url);
    let body_c = cstr(body);
    let n = unsafe {
        https_post_json(
            url_c.as_ptr() as *const c_char,
            body_c.as_ptr() as *const c_char,
            buf.as_mut_ptr() as *mut c_char,
            buf.len() as c_int,
        )
    };
    if n == -3 {
        return Err(std::string::String::from(crate::loc::s().err_too_large));
    }
    if n < 0 {
        if n == -2 {
            return Err(https_error_message(Sink::Memory));
        }
        return Err(crate::loc::err_https(&std::format!("rc {}", n)));
    }
    buf.truncate(n as usize);
    Ok(buf)
}

// ── archive.org metadata ───────────────────────────────────────────────

/// One file inside an archive.org item. Only fields we actually use.
#[derive(Debug, Clone)]
pub struct RemoteFile {
    pub name: std::string::String,
    pub size_bytes: u64,
    pub download_url: std::string::String,
}

/// Extract the item-id from an archive.org URL (or treat the input as a
/// bare item-id). Accepts:
///   - https://archive.org/details/<id>
///   - https://archive.org/download/<id>[/<filename>]
///   - <id>                                           (bare)
pub fn extract_item_id(url_or_id: &str) -> Option<std::string::String> {
    let trimmed = url_or_id.trim();
    if !trimmed.contains("://") && !trimmed.contains('/') {
        // Bare item-id.
        if trimmed.is_empty() {
            return None;
        }
        return Some(trimmed.to_string());
    }
    let parts: std::vec::Vec<&str> = trimmed.split('/').collect();
    for (i, p) in parts.iter().enumerate() {
        if (*p == "details" || *p == "download") && i + 1 < parts.len() {
            let id = parts[i + 1];
            if !id.is_empty() {
                return Some(id.to_string());
            }
        }
    }
    None
}

/// Parse archive.org metadata JSON → the item's `.swf` files. Used by the async
/// `tick_archive_fetch`. archive.org
/// returns `{server, dir, files:[...]}`; we build each download URL as
/// `https://archive.org/download/<item_id>/<filename URL-encoded>` (archive.org
/// redirects to the CDN, keeping us URL-stable across mirror moves).
fn parse_archive_metadata(
    buf: &[u8],
    item_id: &str,
) -> Result<std::vec::Vec<RemoteFile>, std::string::String> {
    let json: serde_json::Value = serde_json::from_slice(buf).map_err(|e| {
        log(&std::format!("net: JSON parse failed: {}\n", e));
        crate::loc::err_json(&e.to_string())
    })?;
    let files_json = json
        .get("files")
        .and_then(|v| v.as_array())
        .ok_or_else(|| std::string::String::from(crate::loc::s().err_json_no_files))?;
    let mut out: std::vec::Vec<RemoteFile> = std::vec::Vec::new();
    for f in files_json {
        let format = f.get("format").and_then(|v| v.as_str()).unwrap_or("");
        if format != "Shockwave Flash" {
            // Filter early — items have many non-SWF files (thumbnails, XMLs…).
            continue;
        }
        let name = f.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string();
        if name.is_empty() {
            continue;
        }
        let size_bytes = f
            .get("size")
            .and_then(|v| v.as_str())
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(0);
        let download_url = std::format!(
            "https://archive.org/download/{}/{}",
            item_id,
            url_encode_path(&name),
        );
        out.push(RemoteFile { name, size_bytes, download_url });
    }
    log(&std::format!("net: archive.org/{} -> {} .swf file(s)\n", item_id, out.len()));
    Ok(out)
}

/// Result of polling the async metadata fetch.
pub enum ArchivePoll {
    Pending,
    Done(std::vec::Vec<RemoteFile>),
    Error(std::string::String),
}

/// item-id of the in-flight async fetch, kept so completion can build the
/// per-file download URLs (mirrors the sync path's `item_id` argument).
static FETCH_ITEM_ID: std::sync::Mutex<std::string::String> =
    std::sync::Mutex::new(std::string::String::new());

/// Start the async archive.org metadata fetch (non-blocking). Returns
/// immediately; the C++ multi handle runs on each `tick_archive_fetch`.
pub fn start_archive_fetch(item_id: &str) -> Result<(), std::string::String> {
    let url = std::format!("https://archive.org/metadata/{}", item_id);
    let url_c = cstr(&url);
    let rc = unsafe { https_get_start(url_c.as_ptr() as *const c_char) };
    if rc != 0 {
        log(&std::format!("net: https_get_start rc={}\n", rc));
        return Err(crate::loc::err_https(&std::format!("rc {}", rc)));
    }
    if let Ok(mut g) = FETCH_ITEM_ID.lock() {
        *g = item_id.to_string();
    }
    Ok(())
}

/// Poll the async metadata fetch once (call per frame while loading).
pub fn tick_archive_fetch() -> ArchivePoll {
    let rc = unsafe { https_get_tick() };
    if rc == 0 {
        return ArchivePoll::Pending;
    }
    if rc < 0 {
        log(&std::format!("net: archive fetch failed: {}\n", last_https_error()));
        return ArchivePoll::Error(https_error_message(Sink::Memory));
    }
    // rc == 1: response ready — copy out of C++ and parse.
    const CAP: usize = 8 * 1024 * 1024;
    let mut buf = std::vec![0u8; CAP];
    let n = unsafe { https_get_buffer(buf.as_mut_ptr() as *mut c_char, CAP as c_int) };
    if n < 0 {
        return ArchivePoll::Error(std::string::String::from(crate::loc::s().err_too_large));
    }
    buf.truncate(n as usize);
    let item_id = FETCH_ITEM_ID.lock().map(|g| g.clone()).unwrap_or_default();
    match parse_archive_metadata(&buf, &item_id) {
        Ok(files) => ArchivePoll::Done(files),
        Err(e) => ArchivePoll::Error(e),
    }
}

/// Abort an in-flight async metadata fetch (user backed out).
pub fn cancel_archive_fetch() {
    unsafe { https_get_cancel() };
}

/// Result of polling a generic async GET (`start_get_async` / `tick_get_async`).
pub enum GetPoll {
    Pending,
    Done(std::vec::Vec<u8>),
    Error(std::string::String),
}

/// Start a generic async GET of `url` (non-blocking). Shares the single C++
/// `https_get_*` multi handle with the archive.org metadata fetch, so only one
/// of the two runs at a time (the UI serialises them). Used for the async
/// Flashpoint game search so its result list arrives behind a spinner instead
/// of freezing the UI on the blocking HTTP.
pub fn start_get_async(url: &str) -> Result<(), std::string::String> {
    let url_c = cstr(url);
    let rc = unsafe { https_get_start(url_c.as_ptr() as *const c_char) };
    if rc != 0 {
        log(&std::format!("net: https_get_start rc={}\n", rc));
        return Err(crate::loc::err_https(&std::format!("rc {}", rc)));
    }
    Ok(())
}

/// Poll a generic async GET once (call per frame while loading). On `Done`,
/// hands back the raw response bytes for the caller to parse.
pub fn tick_get_async() -> GetPoll {
    let rc = unsafe { https_get_tick() };
    if rc == 0 {
        return GetPoll::Pending;
    }
    if rc < 0 {
        log(&std::format!("net: async GET failed: {}\n", last_https_error()));
        return GetPoll::Error(https_error_message(Sink::Memory));
    }
    const CAP: usize = 4 * 1024 * 1024;
    let mut buf = std::vec![0u8; CAP];
    let n = unsafe { https_get_buffer(buf.as_mut_ptr() as *mut c_char, CAP as c_int) };
    if n < 0 {
        return GetPoll::Error(std::string::String::from(crate::loc::s().err_response_big));
    }
    buf.truncate(n as usize);
    GetPoll::Done(buf)
}

// ── Async thumbnail GET (cover/logo grids) ─────────────────────────────────

/// Start an async thumbnail GET in a free pool slot (covers download in
/// PARALLEL, up to the C++ pool size). Returns the slot index (>=0) to poll, or
/// a negative value when the pool is full / init failed. The render thumbnail
/// driver tracks slot -> url and polls each slot.
pub(crate) fn thumb_start(url: &str) -> i32 {
    let url_c = cstr(url);
    unsafe { https_thumb_start(url_c.as_ptr() as *const c_char) }
}

/// Pump EVERY in-flight thumbnail transfer once (call once per frame before
/// polling the slots).
pub(crate) fn thumb_pump() {
    unsafe {
        https_thumb_tick();
    }
}

/// Poll one slot: 1 = done OK, 0 = in flight, negative = done error / free.
pub(crate) fn thumb_slot_status(slot: i32) -> i32 {
    unsafe { https_thumb_slot_status(slot) }
}

/// Take a finished slot's bytes and free it. `Some` on success, `None` on error
/// / oversize. Call once after `thumb_slot_status(slot) != 0`.
pub(crate) fn thumb_slot_take(slot: i32) -> Option<std::vec::Vec<u8>> {
    const CAP: usize = 4 * 1024 * 1024;
    let mut buf = std::vec![0u8; CAP];
    let n = unsafe { https_thumb_slot_take(slot, buf.as_mut_ptr() as *mut c_char, CAP as c_int) };
    if n < 0 {
        return None;
    }
    buf.truncate(n as usize);
    Some(buf)
}

/// Abort ALL in-flight thumbnail GETs (gallery left / new search started).
pub(crate) fn thumb_cancel() {
    unsafe { https_thumb_cancel() };
}

/// Content-Length of `url` via a HEAD request (follows redirects). `None` if the
/// server doesn't report it / the request fails. Blocking (~a few hundred ms) —
/// call hoisted out of the LIBRARY lock. Used by the Flashpoint details popup.
pub(crate) fn head_content_length(url: &str) -> Option<u64> {
    let url_c = cstr(url);
    let n = unsafe { https_head_content_length(url_c.as_ptr() as *const c_char) };
    if n > 0 {
        Some(n as u64)
    } else {
        None
    }
}

/// Percent-encode characters that aren't URL-safe in a path segment.
/// Keeps ASCII alphanumerics, `.`, `-`, `_`, `~`; everything else
/// becomes %XX (UTF-8 byte-per-byte). Spaces → %20.
pub(crate) fn url_encode_path(s: &str) -> std::string::String {
    let mut out = std::string::String::with_capacity(s.len());
    for &b in s.as_bytes() {
        let safe = matches!(b,
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'.' | b'-' | b'_' | b'~');
        if safe {
            out.push(b as char);
        } else {
            out.push_str(&std::format!("%{:02X}", b));
        }
    }
    out
}

// ── Download lifecycle ─────────────────────────────────────────────────

/// Start an async download. The C++ side sets up a curl multi handle;
/// subsequent calls to `download_tick` pump it without blocking.
pub fn start_download(url: &str, out_path: &str) -> Result<(), std::string::String> {
    let mut url_c = url.as_bytes().to_vec();
    url_c.push(0);
    let mut path_c = out_path.as_bytes().to_vec();
    path_c.push(0);
    let rc = unsafe {
        https_download_start(url_c.as_ptr() as *const c_char, path_c.as_ptr() as *const c_char)
    };
    if rc != 0 {
        return Err(crate::loc::err_dl_start(rc));
    }
    Ok(())
}

/// Returns:
///   - `Ok(false)` → still in progress
///   - `Ok(true)`  → finished successfully
///   - `Err(msg)`  → failed; the partial output file has been removed
pub fn tick_download() -> Result<bool, std::string::String> {
    let rc = unsafe { https_download_tick() };
    if rc == 0 {
        return Ok(false);
    }
    if rc == 1 {
        return Ok(true);
    }
    // -2 = the transfer itself failed and the curl result was recorded; name the
    // cause (SD full, server 404, clock/TLS...) rather than printing "code -2".
    if rc == -2 {
        log(&std::format!("net: download failed: {}\n", last_https_error()));
        return Err(https_error_message(Sink::Sd));
    }
    Err(crate::loc::err_dl_failed(rc))
}

/// Current bytes downloaded / total bytes (0 until the Content-Length
/// header arrives).
pub fn download_progress() -> (u64, u64) {
    let mut done = 0u64;
    let mut total = 0u64;
    unsafe { https_download_progress(&mut done as *mut _, &mut total as *mut _) };
    (done, total)
}

pub fn cancel_download() {
    unsafe { https_download_cancel() };
}

// ── swkbd URL input ────────────────────────────────────────────────────

/// Prompt the user for an archive.org URL via libnx's software keyboard.
/// `initial` pre-fills the input field — pass the most recent history
/// URL so the user can edit a neighbouring item-id with a few keystrokes
/// instead of retyping the whole URL. Pass `None` for a default prefix.
/// Synchronous — the keyboard applet takes over the whole screen until
/// the user submits or cancels. Returns None if cancelled.
pub fn prompt_url_with_initial(initial: Option<&str>) -> Option<std::string::String> {
    let mut buf = std::vec![0u8; 1024];
    // Build NUL-terminated initial string (or NULL ptr if None).
    let initial_owned: Option<std::vec::Vec<u8>> = initial.map(|s| {
        let mut v = s.as_bytes().to_vec();
        v.push(0);
        v
    });
    let initial_ptr = initial_owned
        .as_ref()
        .map(|v| v.as_ptr() as *const c_char)
        .unwrap_or(core::ptr::null());
    let header_c = cstr(crate::loc::s().kbd_url_header);
    let guide_c = cstr(crate::loc::s().kbd_url_guide);
    let rc = unsafe {
        swkbd_prompt_url(
            header_c.as_ptr() as *const c_char,
            guide_c.as_ptr() as *const c_char,
            initial_ptr,
            buf.as_mut_ptr() as *mut c_char,
            buf.len() as c_int,
        )
    };
    if rc != 0 {
        return None;
    }
    let nul = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    buf.truncate(nul);
    std::string::String::from_utf8(buf).ok().filter(|s| !s.is_empty())
}

/// Prompt for a new display name (Phase 3.4.bis RENOMMER). `initial` is
/// the current display name pre-filled so a small edit is one keystroke
/// away. Returns the typed text (which may be empty — meaning "revert
/// to basename") on commit, None on cancel.
pub fn prompt_rename(initial: &str) -> Option<std::string::String> {
    let mut buf = std::vec![0u8; 512];
    let mut initial_owned = initial.as_bytes().to_vec();
    initial_owned.push(0);
    let header_c = cstr(crate::loc::s().kbd_rename_header);
    let guide_c = cstr(crate::loc::s().kbd_rename_guide);
    let rc = unsafe {
        swkbd_prompt_rename(
            header_c.as_ptr() as *const c_char,
            guide_c.as_ptr() as *const c_char,
            initial_owned.as_ptr() as *const c_char,
            buf.as_mut_ptr() as *mut c_char,
            buf.len() as c_int,
        )
    };
    if rc != 0 {
        return None;
    }
    let nul = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    buf.truncate(nul);
    // We allow returning an empty string (caller interprets as "revert
    // to basename"), so don't filter empties here.
    std::string::String::from_utf8(buf).ok()
}

/// Short single-line prompt for the player's nickname (RÉGLAGES > PSEUDO),
/// pre-filled with the current value. Empty return = clear the nickname.
pub fn prompt_pseudo(initial: &str) -> Option<std::string::String> {
    let mut buf = std::vec![0u8; 128];
    let mut initial_owned = initial.as_bytes().to_vec();
    initial_owned.push(0);
    let header_c = cstr(crate::loc::s().set_pseudo);
    let guide_c = cstr(crate::loc::s().kbd_pseudo_guide);
    let rc = unsafe {
        swkbd_prompt_search(
            header_c.as_ptr() as *const c_char,
            guide_c.as_ptr() as *const c_char,
            initial_owned.as_ptr() as *const c_char,
            buf.as_mut_ptr() as *mut c_char,
            buf.len() as c_int,
        )
    };
    if rc != 0 {
        return None;
    }
    let nul = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    buf.truncate(nul);
    std::string::String::from_utf8(buf).ok()
}

/// Generic long free-text prompt (2 KB buffer, no prefill). Reuses the generic
/// C++ `swkbd_prompt_search` with caller-supplied header/guide. Used by the bug
/// report and the suggestion flow. Returns None on cancel; may return an empty
/// string (caller decides whether that's allowed).
pub fn prompt_long(header: &str, guide: &str) -> Option<std::string::String> {
    let mut buf = std::vec![0u8; 2048];
    let initial = [0u8]; // empty prefill
    let header_c = cstr(header);
    let guide_c = cstr(guide);
    let rc = unsafe {
        swkbd_prompt_search(
            header_c.as_ptr() as *const c_char,
            guide_c.as_ptr() as *const c_char,
            initial.as_ptr() as *const c_char,
            buf.as_mut_ptr() as *mut c_char,
            buf.len() as c_int,
        )
    };
    if rc != 0 {
        return None;
    }
    let nul = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    buf.truncate(nul);
    std::string::String::from_utf8(buf).ok()
}

/// Bug-report description prompt (empty input allowed — the report still carries
/// the game's technical info).
pub fn prompt_bug() -> Option<std::string::String> {
    prompt_long(crate::loc::s().kbd_bug_header, crate::loc::s().kbd_bug_guide)
}

/// Search prompt for filtering the DistantFiles list. `initial` pre-fills
/// the input with the currently-active filter so the user can refine it.
/// Returns the typed text on commit (empty string = clear filter), None
/// on cancel.
pub fn prompt_search(initial: &str) -> Option<std::string::String> {
    let mut buf = std::vec![0u8; 256];
    let mut initial_owned = initial.as_bytes().to_vec();
    initial_owned.push(0);
    let header_c = cstr(crate::loc::s().kbd_search_header);
    let guide_c = cstr(crate::loc::s().kbd_search_guide);
    let rc = unsafe {
        swkbd_prompt_search(
            header_c.as_ptr() as *const c_char,
            guide_c.as_ptr() as *const c_char,
            initial_owned.as_ptr() as *const c_char,
            buf.as_mut_ptr() as *mut c_char,
            buf.len() as c_int,
        )
    };
    if rc != 0 {
        return None;
    }
    let nul = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    buf.truncate(nul);
    std::string::String::from_utf8(buf).ok()
}
