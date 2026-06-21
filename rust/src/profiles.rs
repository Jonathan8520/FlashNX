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
            matches.push(Match { profile: p, kind });
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
// The full catalog lives in the repo under `community-profiles/`: an
// `index.json` (match keys + file names) plus one `*.profile.json` per entry.
// The app fetches the index once per session, then a profile file on demand
// when it matches the open game. Maintainer-curated (issues → files), so the
// app only ever READS these — see `share()` for the contribution side.

const ONLINE_BASE: &str =
    "https://raw.githubusercontent.com/Jonathan8520/FlashNX/main/community-profiles/";

#[derive(Debug, Clone, Deserialize)]
struct IndexEntry {
    id: std::string::String,
    #[serde(default)]
    title: std::string::String,
    #[serde(default)]
    fp_uuid: std::string::String,
    #[serde(default)]
    swf_hash: std::string::String,
    /// File under `community-profiles/` (e.g. "mario63-default.profile.json").
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
    INDEX_TRIED.store(true, Ordering::Relaxed);
    let url = std::format!("{}index.json", ONLINE_BASE);
    let entries = match crate::net::http_get(&url, 64 * 1024) {
        Ok(bytes) => std::string::String::from_utf8(bytes)
            .ok()
            .and_then(|t| serde_json::from_str::<std::vec::Vec<IndexEntry>>(&t).ok())
            .unwrap_or_default(),
        Err(e) => {
            crate::net::log(&std::format!("profiles: index fetch failed: {}\n", e));
            std::vec::Vec::new()
        }
    };
    if let Ok(mut g) = ONLINE_INDEX.lock() {
        *g = Some(entries.clone());
    }
    entries
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
        let url = std::format!("{}{}", ONLINE_BASE, e.file);
        match crate::net::http_get(&url, 16 * 1024) {
            Ok(bytes) => {
                if let Some(p) = std::string::String::from_utf8(bytes)
                    .ok()
                    .and_then(|t| serde_json::from_str::<Profile>(&t).ok())
                {
                    out.push(Match { profile: p, kind });
                }
            }
            Err(err) => {
                crate::net::log(&std::format!("profiles: fetch {} failed: {}\n", e.file, err))
            }
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
    matches.sort_by(|a, b| {
        let exact = |m: &Match| m.kind != MatchKind::Title;
        exact(b)
            .cmp(&exact(a))
            .then(b.profile.verified.cmp(&a.profile.verified))
            .then(b.profile.completeness().cmp(&a.profile.completeness()))
    });
    matches
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
    /// Tells the relay Worker to open a `profile` issue (not a bug/suggestion).
    kind: &'a str,
    app_version: &'a str,
    lang: &'a str,
    title: &'a str,
    fp_uuid: &'a str,
    swf_hash: &'a str,
    bindings: &'a BTreeMap<std::string::String, std::string::String>,
    bindings_p2: &'a BTreeMap<std::string::String, std::string::String>,
}

/// Submit the player's current controls for a game as a community profile
/// candidate. Goes through the SAME relay + endpoint as bug reports: the Worker
/// opens a `profile`-labelled GitHub issue (no login, token stays server-side),
/// which a maintainer curates into the catalog. Returns a localized error on
/// failure.
pub fn share(
    title: &str,
    fp_uuid: &str,
    swf_hash: &str,
    km: &Keymap,
) -> Result<(), std::string::String> {
    let payload = SharePayload {
        kind: "profile",
        app_version: crate::bugreport::APP_VERSION,
        lang: crate::loc::current().code(),
        title,
        fp_uuid,
        swf_hash,
        bindings: &km.bindings,
        bindings_p2: &km.bindings_p2,
    };
    let body = serde_json::to_string(&payload)
        .map_err(|e| std::format!("encode failed: {}", e))?;
    crate::net::log(&std::format!("profiles: sharing '{}' ({} bytes)\n", title, body.len()));
    crate::net::post_json(crate::bugreport::BUG_REPORT_ENDPOINT, &body, 16 * 1024).map(|_| ())
}
