//! Flashpoint Archive — metadata / cover lookup ONLY.
//!
//! Used to enrich the LOCAL library with cover art for games the user already
//! owns. We hit the public, MIT-licensed db-api search endpoint by game NAME
//! and build the logo (cover) URL from the returned UUID. We deliberately
//! NEVER resolve or download game binaries from Flashpoint — see the project
//! notes / plan: covers are metadata enrichment, not redistribution.
//!
//! Endpoints (verified 2026-06-05):
//!   - search: https://db-api.unstable.life/search?smartSearch=<name>&filter=true&fields=...
//!   - logo:   https://infinity.unstable.life/images/Logos/<id[0:2]>/<id[2:4]>/<id>.png

use crate::net;

/// A Flashpoint catalog hit — only the fields we need to label a cover
/// candidate and fetch its logo.
#[derive(Debug, Clone)]
pub struct CatalogEntry {
    pub id: std::string::String,
    pub title: std::string::String,
    pub developer: std::string::String,
    /// Publisher + release date — shown in the Flashpoint details popup (`+`).
    /// Empty when unknown / not requested (the cover picker leaves them blank).
    pub publisher: std::string::String,
    pub release_date: std::string::String,
    /// Fully-built logo (cover) URL for `id`.
    pub cover_url: std::string::String,
    /// Flashpoint `launchCommand` (the URL of the game's ENTRY swf, e.g.
    /// `http://i.flipline.com/.../PapaLouie2_v2_1.swf`). A GameZIP can bundle
    /// several SWF versions; this says which one to launch. Empty for cover-only
    /// hits (the cover picker doesn't request it).
    pub launch_command: std::string::String,
}

const SEARCH_BASE: &str = "https://db-api.unstable.life/search";
const IMAGE_BASE: &str = "https://infinity.unstable.life/images";

/// Build the Flashpoint logo (cover) URL for a game UUID. The image server
/// shards by the first two id bytes: `Logos/<id[0:2]>/<id[2:4]>/<id>.png`.
pub fn logo_url(id: &str) -> std::string::String {
    if id.len() >= 4 {
        std::format!(
            "{}/Logos/{}/{}/{}.png",
            IMAGE_BASE,
            &id[0..2],
            &id[2..4],
            id
        )
    } else {
        std::format!("{}/Logos/{}.png", IMAGE_BASE, id)
    }
}

/// Search Flashpoint by game name. Returns up to a handful of candidates so the
/// user can pick a cover. Synchronous HTTPS GET + JSON parse; the response is
/// small (a few KB per hit).
pub fn search(
    name: &str,
) -> Result<std::vec::Vec<CatalogEntry>, std::string::String> {
    let q = net::url_encode_path(name.trim());
    let url = std::format!(
        "{}?smartSearch={}&filter=true&fields=id,title,developer,platform,library",
        SEARCH_BASE, q
    );
    // Log the exact URL hit: distinguishes a mangled/encoded query from a real
    // no-match, the single most useful clue when a cover search comes back empty.
    net::log(&std::format!("flashpoint: GET {}\n", url));
    // 1 MB cap is generous for a name search (the catalog browser is out of
    // scope; we only want the top matches).
    let bytes = net::http_get(&url, 1024 * 1024)?;
    // Response size tells a successful-but-empty result (e.g. "[]") apart from a
    // truncated/garbage body before we even try to parse it.
    net::log(&std::format!("flashpoint: {} bytes received\n", bytes.len()));
    let json: serde_json::Value = serde_json::from_slice(&bytes)
        .map_err(|e| crate::loc::err_json(&e.to_string()))?;
    // db-api returns a top-level JSON array of game objects.
    let arr = json
        .as_array()
        .ok_or_else(|| std::string::String::from(crate::loc::s().err_json_no_files))?;
    let mut out: std::vec::Vec<CatalogEntry> = std::vec::Vec::new();
    for g in arr {
        let id = g.get("id").and_then(|v| v.as_str()).unwrap_or("");
        if id.is_empty() {
            continue;
        }
        let title = g
            .get("title")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let developer = g
            .get("developer")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        out.push(CatalogEntry {
            id: id.to_string(),
            title,
            developer,
            // The cover picker doesn't show these; left blank to keep its
            // response small (the FpGallery details popup fills them — see
            // gamezip::search).
            publisher: std::string::String::new(),
            release_date: std::string::String::new(),
            cover_url: logo_url(id),
            launch_command: std::string::String::new(),
        });
        // A short candidate list is all the cover-picker needs.
        if out.len() >= 12 {
            break;
        }
    }
    Ok(out)
}
