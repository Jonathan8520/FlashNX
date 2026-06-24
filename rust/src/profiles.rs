//! Community control profiles (issue #20).
//!
//! A "profile" is a ready-made keymap for a specific game, so a player doesn't
//! have to configure controls by hand. Profiles are matched to the local game
//! by, in priority order: Flashpoint UUID, a stable hash of the `.swf` bytes,
//! then a normalized title (fuzzy — only ever *suggested*, never auto-applied).
//!
//! ## Provenance & non-destructive apply
//! Applying a profile writes the game's `<basename>.keymap.json` with
//! `source = "community:<id>"`, after backing up any hand-made keymap so the
//! user can revert (see `keymap::apply_keymap` / `keymap::revert_profile`).
//!
//! ## Sources (phased — see issue #20)
//! - Phase 1 (here): a small CURATED catalog bundled in the `.nro`
//!   (`assets/profiles/*.json`), plus sharing your own via the existing relay.
//! - Phase 2: fetch the community catalog over HTTPS (an `index.json` + files).
//! - Phase 3: popularity signal ("most applied") via the relay Worker.
//
// TODO(#20, next increment): wire these into the library OPTIONS list (an
// "Apply a profile" row + preview, and a "Share my controls" row). Until then
// the public surface is unused, hence the module-level allow.
#![allow(dead_code)]

use std::collections::BTreeMap;
use std::fs::File;
use std::io::Read;

use serde::Deserialize;

use crate::keymap::Keymap;

/// Profiles baked into the binary. Curated/known-good layouts that ship with
/// every build so popular games work out of the box with no network. Add a
/// JSON file under `assets/profiles/` and a line here to extend the set.
const BUNDLED_JSON: &[&str] = &[include_str!("../../assets/profiles/mario63.profile.json")];

#[derive(Debug, Clone, Deserialize)]
struct ProfileGame {
    #[serde(default)]
    title: std::string::String,
    /// Flashpoint UUID this profile targets (empty = not a Flashpoint match).
    #[serde(default)]
    fp_uuid: std::string::String,
    /// `swf_hash` of the exact `.swf` this profile was made for (empty = match
    /// by title only).
    #[serde(default)]
    swf_hash: std::string::String,
}

/// One control profile (the on-disk / on-wire format, distinct from the local
/// `Keymap` because it carries matching keys + curation metadata).
#[derive(Debug, Clone, Deserialize)]
pub struct Profile {
    #[serde(default)]
    pub schema: u32,
    pub id: std::string::String,
    game: ProfileGame,
    #[serde(default)]
    pub author: std::string::String,
    /// Curated/tested by a maintainer — sorted to the top of the list.
    #[serde(default)]
    pub verified: bool,
    #[serde(default)]
    pub notes: std::string::String,
    pub bindings: BTreeMap<std::string::String, std::string::String>,
    #[serde(default)]
    pub bindings_p2: BTreeMap<std::string::String, std::string::String>,
}

impl Profile {
    pub fn title(&self) -> &str {
        &self.game.title
    }
    /// Rough completeness signal for ranking: how many P1 buttons are mapped.
    fn completeness(&self) -> usize {
        self.bindings.values().filter(|v| !v.is_empty()).count()
    }
}

/// Parse the bundled catalog (skips any entry that fails to parse, loudly).
pub fn bundled() -> std::vec::Vec<Profile> {
    let mut out = std::vec::Vec::new();
    for (i, raw) in BUNDLED_JSON.iter().enumerate() {
        match serde_json::from_str::<Profile>(raw) {
            Ok(p) => out.push(p),
            Err(e) => crate::net::log(&std::format!(
                "profiles: bundled entry {} failed to parse: {}\n",
                i, e,
            )),
        }
    }
    out
}

/// Stable 64-bit content hash (FNV-1a, hex) of a `.swf` file, used as a match
/// key. NOT cryptographic — only the app ever computes it (on both the share
/// and the lookup side), so it just has to be deterministic across builds.
/// Reads in 8 KB chunks (Horizon newlib `read` dislikes huge buffers).
pub fn swf_hash_of(path: &str) -> Option<std::string::String> {
    let mut file = File::open(path).ok()?;
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325; // FNV offset basis
    let mut buf = [0u8; 8192];
    loop {
        match file.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                for &b in &buf[..n] {
                    hash ^= b as u64;
                    hash = hash.wrapping_mul(0x0000_0100_0000_01b3); // FNV prime
                }
            }
            Err(_) => return None,
        }
    }
    Some(std::format!("{:016x}", hash))
}

extern "C" {
    /// Monotonic system tick (`armGetSystemTick`). Only used here to SEED the
    /// one-time install id — the custom getrandom backend is a fixed-seed LCG
    /// (see `__getrandom_v03_custom` in lib.rs), so it can't give two installs
    /// different ids on its own.
    fn ruffle_tick_now() -> u64;
}

/// A short, per-INSTALL identifier (8 hex chars), generated once and persisted
/// to `sdmc:/flashnx/install_id`. It's appended to every shared profile's id so
/// two people sharing controls for the SAME game land on DIFFERENT files (they
/// coexist in the catalog) instead of clobbering each other — while this same
/// install re-sharing a game keeps the same id, so it UPDATES its own profile
/// rather than piling up duplicates. Not an identity/login: it's only a dedup
/// key, carries no personal data, and a fresh value on a reset SD is harmless.
pub fn install_id() -> std::string::String {
    // Reuse an existing id if one is on the SD (either root, for legacy installs).
    for root in ["sdmc:/flashnx", "sdmc:/ruffle"] {
        let p = std::format!("{}/install_id", root);
        if let Some(txt) = read_small_file(&p) {
            let t = txt.trim();
            if t.len() >= 4 {
                return t.chars().take(16).filter(|c| c.is_ascii_hexdigit()).collect();
            }
        }
    }
    // First run: seed an xorshift from the boot tick (differs per install/boot),
    // mix a few rounds, keep 32 bits → 8 hex chars.
    let mut state = unsafe { ruffle_tick_now() } ^ 0x9E37_79B9_7F4A_7C15;
    if state == 0 {
        state = 0x2545_F491_4F6C_DD1D; // never seed the xorshift with all-zero
    }
    for _ in 0..8 {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
    }
    let id = std::format!("{:08x}", state as u32);
    // Persist best-effort. If the write fails the id just regenerates next boot
    // (worst case: this install's future shares land as new files rather than
    // updates — never a clobber of someone else's profile).
    let path = "sdmc:/flashnx/install_id";
    if std::fs::write(path, id.as_bytes()).is_ok() {
        crate::sd::commit();
    }
    id
}

/// Read a tiny text file with the chunked-read workaround (Horizon newlib
/// dislikes large `std::fs::read` buffers). Returns None if absent/unreadable.
fn read_small_file(path: &str) -> Option<std::string::String> {
    let mut file = File::open(path).ok()?;
    let mut data = std::vec::Vec::new();
    let mut buf = [0u8; 256];
    loop {
        match file.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => data.extend_from_slice(&buf[..n]),
            Err(_) => return None,
        }
        if data.len() > 4096 {
            break; // an install_id is 8 bytes; bail on anything absurd
        }
    }
    std::string::String::from_utf8(data).ok()
}

/// The player's chosen nickname, shown next to their shared profiles so several
/// profiles for the same game are distinguishable. Empty if unset. It's an
/// anonymous LABEL only, not an identity — the install id stays the dedup key, so
/// two people picking the same nickname still never collide. Stored in
/// `sdmc:/flashnx/author`, capped to 24 chars.
pub fn author_name() -> std::string::String {
    for root in ["sdmc:/flashnx", "sdmc:/ruffle"] {
        if let Some(txt) = read_small_file(&std::format!("{}/author", root)) {
            let t = txt.trim();
            if !t.is_empty() {
                return t.chars().take(24).collect();
            }
        }
    }
    std::string::String::new()
}

/// Persist the player's nickname (RÉGLAGES > PSEUDO). Empty clears it.
pub fn set_author_name(name: &str) {
    let clean: std::string::String = name.trim().chars().take(24).collect();
    let path = "sdmc:/flashnx/author";
    if clean.is_empty() {
        let _ = std::fs::remove_file(path);
    } else if std::fs::write(path, clean.as_bytes()).is_err() {
        return;
    }
    crate::sd::commit();
}

/// Secret per-install token that proves ownership of this install's shared
/// profiles (#20). SERVER-generated on the first share (trust-on-first-use) and
/// returned in the share response; we persist it in `sdmc:/flashnx/owner_token`.
/// Required to UPDATE or DELETE our own profiles. Unlike `install_id` (a PUBLIC
/// dedup key, visible in every shared id), this never leaves the device except
/// over HTTPS to the relay. Empty until the first successful share. Hex/alnum.
fn owner_token() -> std::string::String {
    for root in ["sdmc:/flashnx", "sdmc:/ruffle"] {
        if let Some(txt) = read_small_file(&std::format!("{}/owner_token", root)) {
            let t: std::string::String =
                txt.trim().chars().take(64).filter(|c| c.is_ascii_alphanumeric()).collect();
            if !t.is_empty() {
                return t;
            }
        }
    }
    std::string::String::new()
}

/// Persist the owner token returned by the relay (idempotent; ignores empties).
fn set_owner_token(token: &str) {
    let clean: std::string::String =
        token.chars().take(64).filter(|c| c.is_ascii_alphanumeric()).collect();
    if clean.len() < 16 {
        return; // ignore junk / too-short to be a real token
    }
    if std::fs::write("sdmc:/flashnx/owner_token", clean.as_bytes()).is_ok() {
        crate::sd::commit();
    }
}

/// True when `profile_id` was shared by THIS install (its id carries our
/// install-id suffix). The picker only offers "delete" on these. The real
/// authorization is the server-side owner-token check; this is just the UI gate.
pub fn is_mine(profile_id: &str) -> bool {
    let suffix = std::format!("-{}", install_id());
    suffix.len() > 1 && profile_id.ends_with(&suffix)
}

/// Normalize a title for fuzzy matching: lowercase, keep only alphanumerics.
/// "Super Mario 63!" and "super-mario_63" both become "supermario63".
fn normalize_title(s: &str) -> std::string::String {
    s.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(|c| c.to_lowercase())
        .collect()
}

/// How a profile matched the local game — drives the UI (exact = offer to
/// apply; fuzzy = only suggest, the user confirms it's the right game).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MatchKind {
    Uuid,
    Hash,
    Title,
}

#[derive(Clone)]
pub struct Match {
    pub profile: Profile,
    pub kind: MatchKind,
    /// "Most applied" popularity count from the relay (#20, Phase 3); 0 when
    /// unknown / the counter is unavailable.
    pub applied: u32,
}

/// Find profiles for a game identified by its Flashpoint UUID (may be empty),
/// `.swf` hash (may be empty), and display title. Exact matches (UUID/hash)
/// come first, then title suggestions; within each, verified + more-complete
/// profiles rank higher. Phase 1 searches only the bundled catalog.
pub fn matches_for(fp_uuid: &str, swf_hash: &str, title: &str) -> std::vec::Vec<Match> {
    let title_norm = normalize_title(title);
    let mut matches: std::vec::Vec<Match> = std::vec::Vec::new();
    for p in bundled() {
        let kind = if !fp_uuid.is_empty() && p.game.fp_uuid == fp_uuid {
            Some(MatchKind::Uuid)
        } else if !swf_hash.is_empty() && p.game.swf_hash == swf_hash {
            Some(MatchKind::Hash)
        } else if !title_norm.is_empty() && normalize_title(&p.game.title) == title_norm {
            Some(MatchKind::Title)
        } else {
            None
        };
        if let Some(kind) = kind {
            matches.push(Match { profile: p, kind, applied: 0 });
        }
    }
    // Exact (UUID/hash) before fuzzy (title); then verified; then completeness.
    matches.sort_by(|a, b| {
        let exact = |m: &Match| m.kind != MatchKind::Title;
        exact(b)
            .cmp(&exact(a))
            .then(b.profile.verified.cmp(&a.profile.verified))
            .then(b.profile.completeness().cmp(&a.profile.completeness()))
    });
    matches
}

// ── Phase 2: online community catalog ───────────────────────────────────────
//
// The catalog lives on the dedicated `community-profiles` branch: an
// `index.json` (match keys + file names) plus one `<id>.profile.json` per entry,
// at the branch root. The app fetches the index once per session, then a profile
// file on demand when it matches the open game. Shares are auto-pushed onto this
// branch by the relay Worker (`handleProfileShare`), which also maintains
// index.json — the app only ever READS these (see `share()` for the upload side).

// We read the catalog through the GitHub **API** (contents endpoint) rather than
// raw.githubusercontent.com. raw has BOTH a CDN cache AND an origin propagation
// delay, so a just-shared profile wouldn't appear for seconds-to-minutes (a
// `?cb=` busts the CDN but not the origin lag). The API reads straight from git
// → always fresh. It returns the file as base64 JSON, which we decode.
// Dedicated orphan `community-profiles` branch so shares never touch `main`.
const GH_API_BASE: &str = "https://api.github.com/repos/Jonathan8520/FlashNX/contents/";
const GH_REF: &str = "community-profiles";

/// Fetch a file from the catalog branch via the GitHub API and return its text.
/// The API responds `{ "content": "<base64>", "encoding": "base64" }`; we decode
/// it. `_=<tick>` defeats any conditional caching. None on any failure (network,
/// rate limit, missing file).
fn gh_api_fetch_text(path: &str, cap: usize) -> Option<std::string::String> {
    let url = std::format!(
        "{}{}?ref={}&_={}",
        GH_API_BASE,
        path,
        GH_REF,
        unsafe { ruffle_tick_now() },
    );
    let bytes = crate::net::http_get(&url, cap).ok()?;
    let text = std::string::String::from_utf8(bytes).ok()?;
    #[derive(serde::Deserialize)]
    struct ApiFile {
        #[serde(default)]
        content: std::string::String,
    }
    let f: ApiFile = serde_json::from_str(&text).ok()?;
    let decoded = base64_decode(&f.content)?;
    std::string::String::from_utf8(decoded).ok()
}

/// Minimal standard-base64 decoder (GitHub wraps `content` at 60 cols with
/// newlines, which we skip). Avoids pulling a crate for this one use.
fn base64_decode(s: &str) -> Option<std::vec::Vec<u8>> {
    fn val(c: u8) -> Option<u8> {
        match c {
            b'A'..=b'Z' => Some(c - b'A'),
            b'a'..=b'z' => Some(c - b'a' + 26),
            b'0'..=b'9' => Some(c - b'0' + 52),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None, // whitespace / padding → skipped
        }
    }
    let mut out = std::vec::Vec::new();
    let mut buf = 0u32;
    let mut bits = 0u32;
    for &c in s.as_bytes() {
        if c == b'=' {
            break;
        }
        let Some(v) = val(c) else { continue };
        buf = (buf << 6) | u32::from(v);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buf >> bits) as u8);
        }
    }
    Some(out)
}

#[derive(Debug, Clone, Deserialize)]
struct IndexEntry {
    id: std::string::String,
    #[serde(default)]
    title: std::string::String,
    #[serde(default)]
    fp_uuid: std::string::String,
    #[serde(default)]
    swf_hash: std::string::String,
    /// Path of the profile file on the branch, relative to its root. Grouped
    /// under a per-game folder now (e.g. "super-mario-63/super-mario-63-ab12-cd34
    /// .profile.json"); the app just GETs it via the API, so the layout is opaque.
    file: std::string::String,
    #[serde(default)]
    verified: bool,
}

static ONLINE_INDEX: std::sync::Mutex<Option<std::vec::Vec<IndexEntry>>> =
    std::sync::Mutex::new(None);
static INDEX_TRIED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Fetch + cache the online catalog index (once per session). A network failure
/// caches an empty index so we don't re-hit it on every picker open.
fn fetch_index() -> std::vec::Vec<IndexEntry> {
    use std::sync::atomic::Ordering;
    if INDEX_TRIED.load(Ordering::Relaxed) {
        return ONLINE_INDEX
            .lock()
            .ok()
            .and_then(|g| g.clone())
            .unwrap_or_default();
    }
    // Fetch via the GitHub API (fresh; raw.githubusercontent lags new commits).
    match gh_api_fetch_text("index.json", 256 * 1024) {
        Some(text) => {
            let entries =
                serde_json::from_str::<std::vec::Vec<IndexEntry>>(&text).unwrap_or_default();
            crate::net::log(&std::format!(
                "profiles: index fetched ({} entries)\n",
                entries.len(),
            ));
            // Cache ONLY a successful fetch, so a transient failure retries on the
            // next picker open instead of sticking empty for the whole session.
            if let Ok(mut g) = ONLINE_INDEX.lock() {
                *g = Some(entries.clone());
            }
            INDEX_TRIED.store(true, Ordering::Relaxed);
            entries
        }
        None => {
            crate::net::log("profiles: index fetch failed (api)\n");
            std::vec::Vec::new()
        }
    }
}

/// Drop the cached online catalog + apply counts so the next picker open
/// re-fetches them. Called after a share so the player sees their own profile
/// appear without relaunching (we read via the GitHub API, which is fresh).
pub fn invalidate_online_cache() {
    use std::sync::atomic::Ordering;
    INDEX_TRIED.store(false, Ordering::Relaxed);
    if let Ok(mut g) = ONLINE_INDEX.lock() {
        *g = None;
    }
    COUNTS_TRIED.store(false, Ordering::Relaxed);
    if let Ok(mut g) = COUNTS.lock() {
        *g = None;
    }
}

/// Online profiles matching the game (one HTTPS GET per matching index entry,
/// usually 0–2). Network-bound — only call from a hoisted flow.
fn online_matches_for(fp_uuid: &str, swf_hash: &str, title: &str) -> std::vec::Vec<Match> {
    let title_norm = normalize_title(title);
    let mut out = std::vec::Vec::new();
    for e in fetch_index() {
        let kind = if !fp_uuid.is_empty() && e.fp_uuid == fp_uuid {
            MatchKind::Uuid
        } else if !swf_hash.is_empty() && e.swf_hash == swf_hash {
            MatchKind::Hash
        } else if !title_norm.is_empty() && normalize_title(&e.title) == title_norm {
            MatchKind::Title
        } else {
            continue;
        };
        // Fetch the profile file via the API too (fresh): a file gets overwritten
        // in place on re-share, and a just-shared file lags on raw — fetching it
        // from the API means it shows up immediately.
        match gh_api_fetch_text(&e.file, 64 * 1024) {
            Some(text) => {
                if let Ok(p) = serde_json::from_str::<Profile>(&text) {
                    out.push(Match { profile: p, kind, applied: 0 });
                } else {
                    crate::net::log(&std::format!("profiles: parse {} failed\n", e.file));
                }
            }
            None => crate::net::log(&std::format!("profiles: fetch {} failed (api)\n", e.file)),
        }
    }
    out
}

/// All matches (bundled + online), deduped by profile id (bundled wins), sorted
/// exact-before-fuzzy / verified / completeness. The entry point the picker
/// uses; does network, so call it hoisted.
pub fn all_matches_for(fp_uuid: &str, swf_hash: &str, title: &str) -> std::vec::Vec<Match> {
    let mut matches = matches_for(fp_uuid, swf_hash, title); // bundled
    let mut seen: std::collections::BTreeSet<std::string::String> =
        matches.iter().map(|m| m.profile.id.clone()).collect();
    for m in online_matches_for(fp_uuid, swf_hash, title) {
        if seen.insert(m.profile.id.clone()) {
            matches.push(m);
        }
    }
    // Popularity signal (#20, Phase 3): tag each with its "most applied" count.
    let counts = fetch_counts();
    for m in matches.iter_mut() {
        m.applied = counts.get(&m.profile.id).copied().unwrap_or(0);
    }
    matches.sort_by(|a, b| {
        let exact = |m: &Match| m.kind != MatchKind::Title;
        exact(b)
            .cmp(&exact(a))
            .then(b.profile.verified.cmp(&a.profile.verified))
            .then(b.applied.cmp(&a.applied))
            .then(b.profile.completeness().cmp(&a.profile.completeness()))
    });
    matches
}

// ── Phase 3: "most applied" popularity counter (via the relay Worker + KV) ───

/// Build a relay URL for `path` from the bug-report endpoint (same Worker).
fn worker_url(path: &str) -> std::string::String {
    let ep = crate::bugreport::BUG_REPORT_ENDPOINT;
    let base = ep.strip_suffix("/report").unwrap_or(ep);
    std::format!("{}{}", base, path)
}

static COUNTS: std::sync::Mutex<Option<std::collections::BTreeMap<std::string::String, u32>>> =
    std::sync::Mutex::new(None);
static COUNTS_TRIED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Fetch + cache the per-profile apply counts (once per session). Empty on any
/// failure (the picker then just orders by verified + completeness).
fn fetch_counts() -> std::collections::BTreeMap<std::string::String, u32> {
    use std::sync::atomic::Ordering;
    if COUNTS_TRIED.load(Ordering::Relaxed) {
        return COUNTS.lock().ok().and_then(|g| g.clone()).unwrap_or_default();
    }
    COUNTS_TRIED.store(true, Ordering::Relaxed);
    let map = match crate::net::http_get(&worker_url("/counts"), 64 * 1024) {
        Ok(bytes) => std::string::String::from_utf8(bytes)
            .ok()
            .and_then(|t| {
                serde_json::from_str::<std::collections::BTreeMap<std::string::String, u32>>(&t).ok()
            })
            .unwrap_or_default(),
        Err(_) => std::collections::BTreeMap::new(),
    };
    if let Ok(mut g) = COUNTS.lock() {
        *g = Some(map.clone());
    }
    map
}

#[derive(serde::Serialize)]
struct AppliedBody<'a> {
    id: &'a str,
}

/// Best-effort: bump a profile's "applied" count on the relay (#20, Phase 3).
/// Fire-and-forget — failures (offline, counter unconfigured) are ignored. Call
/// from a hoisted flow; the POST blocks for the round-trip.
pub fn record_applied(id: &str) {
    let body = match serde_json::to_string(&AppliedBody { id }) {
        Ok(b) => b,
        Err(_) => return,
    };
    let _ = crate::net::post_json(&worker_url("/applied"), &body, 4096);
}

/// Apply `profile` to `basename`'s keymap sidecar (non-destructive: backs up a
/// hand-made keymap first). Tags the result `source = "community:<id>"`.
pub fn apply(basename: &str, profile: &Profile) -> bool {
    let km = Keymap {
        version: 1,
        bindings: profile.bindings.clone(),
        bindings_p2: profile.bindings_p2.clone(),
        source: std::format!("community:{}", profile.id),
    };
    crate::keymap::apply_keymap(basename, &km)
}

// ── Sharing (reuses the bug-report relay, issue #20) ────────────────────────

#[derive(serde::Serialize)]
struct SharePayload<'a> {
    /// Tells the relay Worker this is a profile share (auto-pushed), not a
    /// bug/suggestion (which open issues).
    kind: &'a str,
    app_version: &'a str,
    lang: &'a str,
    title: &'a str,
    fp_uuid: &'a str,
    swf_hash: &'a str,
    /// Per-install dedup suffix (see `install_id`): lets several people share
    /// the same game without overwriting each other, while letting THIS install
    /// update its own profile on a re-share.
    install_id: &'a str,
    /// Optional display nickname (see `author_name`): shown next to the profile in
    /// the picker so several profiles for one game are distinguishable.
    author: &'a str,
    /// Secret owner token (see `owner_token`). Empty on the very first share; the
    /// relay then claims this install and returns a freshly-minted token. Required
    /// to update or delete this profile afterwards.
    owner_token: &'a str,
    bindings: &'a BTreeMap<std::string::String, std::string::String>,
    bindings_p2: &'a BTreeMap<std::string::String, std::string::String>,
}

/// Submit the player's current controls for a game as a community profile.
/// Goes through the SAME relay + endpoint as bug reports (no login, token stays
/// server-side), but the Worker AUTO-PUSHES it straight onto the
/// `community-profiles` branch (no manual curation). Returns a localized error
/// on failure.
/// Returns the catalog `id` the Worker assigned on success, so the caller can
/// tag the local keymap as "this is now catalog profile <id>" (blocks pointless
/// re-shares + marks it active in the picker).
pub fn share(
    title: &str,
    fp_uuid: &str,
    swf_hash: &str,
    km: &Keymap,
) -> Result<std::string::String, std::string::String> {
    let install = install_id();
    let author = author_name();
    let token = owner_token(); // empty on the first share; the relay mints one
    let payload = SharePayload {
        kind: "profile",
        app_version: crate::bugreport::APP_VERSION,
        lang: crate::loc::current().code(),
        title,
        fp_uuid,
        swf_hash,
        install_id: &install,
        author: &author,
        owner_token: &token,
        bindings: &km.bindings,
        bindings_p2: &km.bindings_p2,
    };
    let body = serde_json::to_string(&payload)
        .map_err(|e| std::format!("encode failed: {}", e))?;
    crate::net::log(&std::format!("profiles: sharing '{}' ({} bytes)\n", title, body.len()));
    let resp = crate::net::post_json(crate::bugreport::BUG_REPORT_ENDPOINT, &body, 16 * 1024)?;
    // Parse the Worker's { ok, id, token, error }. Previously we treated any HTTP
    // response as success — a worker-side failure (e.g. token can't write) then
    // showed a false "shared OK". Now an `ok:false` surfaces the real error.
    #[derive(serde::Deserialize)]
    struct ShareResp {
        #[serde(default)]
        ok: bool,
        #[serde(default)]
        id: std::string::String,
        /// Owner token (#20): present on the first share for this install (and on
        /// re-claims). Persist it so later updates/deletes can authenticate.
        #[serde(default)]
        token: std::string::String,
        #[serde(default)]
        error: std::string::String,
    }
    let parsed: ShareResp = std::string::String::from_utf8(resp)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or(ShareResp {
            ok: false,
            id: std::string::String::new(),
            token: std::string::String::new(),
            error: std::string::String::new(),
        });
    if parsed.ok {
        if !parsed.token.is_empty() {
            set_owner_token(&parsed.token);
        }
        Ok(parsed.id)
    } else {
        Err(if parsed.error.is_empty() {
            crate::loc::s().bug_fail_title.to_string()
        } else {
            parsed.error
        })
    }
}

/// Ask the relay to REMOVE one of our own shared profiles (#20). Ownership is
/// proved server-side by the install's secret owner token (plus the id carrying
/// our install suffix), so this can only ever delete profiles we shared. Returns
/// a localized error string on failure. Call from a hoisted flow — the HTTPS POST
/// blocks for the round-trip.
pub fn delete(id: &str) -> Result<(), std::string::String> {
    let install = install_id();
    let token = owner_token();
    if token.is_empty() {
        // No token on this device → we never shared from here (or the SD was
        // reset). Nothing we can prove ownership of.
        return Err(crate::loc::s().profile_del_not_mine.to_string());
    }
    #[derive(serde::Serialize)]
    struct DeletePayload<'a> {
        kind: &'a str,
        id: &'a str,
        install_id: &'a str,
        owner_token: &'a str,
    }
    let payload = DeletePayload {
        kind: "profile_delete",
        id,
        install_id: &install,
        owner_token: &token,
    };
    let body = serde_json::to_string(&payload).map_err(|e| std::format!("encode failed: {}", e))?;
    crate::net::log(&std::format!("profiles: deleting '{}'\n", id));
    let resp = crate::net::post_json(crate::bugreport::BUG_REPORT_ENDPOINT, &body, 4096)?;
    #[derive(serde::Deserialize)]
    struct DelResp {
        #[serde(default)]
        ok: bool,
        #[serde(default)]
        error: std::string::String,
    }
    let parsed: DelResp = std::string::String::from_utf8(resp)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or(DelResp { ok: false, error: std::string::String::new() });
    if parsed.ok {
        // Drop the cached catalog so the picker stops showing the deleted profile.
        invalidate_online_cache();
        Ok(())
    } else {
        Err(if parsed.error.is_empty() {
            crate::loc::s().bug_fail_title.to_string()
        } else {
            parsed.error
        })
    }
}
