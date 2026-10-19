//! Flashpoint Archive — its search, and the cover lookup.
//!
//! Every search asks the official site's JSON route, `/api/search`, which
//! takes the search box's own syntax and keeps up with the archive (the site
//! syncs its database from FPFSS every day). db-api, which FlashNX asked
//! before, stopped taking new games on 2025-08-17 (issues #122 and #124); the
//! route was added to the site for this (Flashpoint-Website-Deno#4).
//! `gamezip` and `discover` build their searches here.
//!
//! The cover lookup enriches the LOCAL library with cover art for games the
//! user already owns: a search by name, and the logo URL built from the UUID.
//!
//! Endpoints (verified 2026-10-14):
//!   - search: https://flashpointarchive.org/api/search?query=<words>&limit=<n>
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
    /// True when the game has game data, a GameZIP (`zip_url`). False = legacy
    /// "loose" entry with none; we download those directly from the htdocs
    /// mirror via the `launch_command` instead (single SWF + companions).
    /// Always false for cover-only hits.
    pub zipped: bool,
    /// Where its GameZIP downloads from (`gamezip::zip_url`, from its newest
    /// game data). Empty when it has none, or for cover-only hits.
    pub zip_url: std::string::String,
    /// The GameZIP's size in bytes, as the archive gives it; 0 when unknown.
    pub size: u64,
    /// The blurb from the game's original site (`originalDescription`), for
    /// the details window. Empty when there is none.
    pub description: std::string::String,
}

/// The site's search route (see the module notes).
const API_SEARCH: &str = "https://flashpointarchive.org/api/search";

/// The route's search for `query`, in the search box's syntax: `limit` games
/// (the route gives 100 at most), the ones the archive flags as extreme left
/// out unless `nsfw`, at random when `random` (a new pick each time, which
/// the route does not let be cached).
pub fn api_url(query: &str, limit: usize, nsfw: bool, random: bool) -> std::string::String {
    let mut url = std::format!("{}?query={}&limit={}", API_SEARCH, net::url_encode_path(query.trim()), limit);
    if nsfw {
        url.push_str("&nsfw=true");
    }
    if random {
        url.push_str("&random=true");
    }
    url
}

/// What the player typed, as words for the route: each between quotes. The
/// search box's syntax gives meaning to a leading `-` (not), `#` (a tag), `!`
/// (a platform) and `@` (a developer), and to `:`, `=`, `<` and `>` (a field),
/// so "Learn 2 Fly: Penguin" would ask for a field named "Fly"; a quoted word
/// is only a word, found in the title, the other titles, the developer, the
/// publisher or the series, as db-api's search found it. Quotes typed are
/// dropped, the one thing a quoted word cannot hold.
///
/// The words go BEFORE any field in a query (`"Jam" platform:Flash`, not
/// `platform:Flash "Jam"`): the archive's parser (FPA 0.13.7, `parse_user_input`)
/// keeps the `:` of the last field it read and, a quoted word skipping the
/// field reading, prefixes the word with it, so it looks for ":Jam" and finds
/// nothing (checked on the site, 2026-10-14). A field's own quoted value
/// (`dev:"John Cooney"`) is read with its field and is fine anywhere.
pub fn query_words(text: &str) -> std::string::String {
    text.split_whitespace()
        .map(|w| w.replace('"', ""))
        .filter(|w| !w.is_empty())
        .map(|w| std::format!("\"{}\"", w))
        .collect::<std::vec::Vec<_>>()
        .join(" ")
}

/// What the player typed without its numbers, for a second try when the search
/// found nothing (#131): every word must be in the entry, so "Sonny 1" misses
/// "Sonny" and "Super Fighters 2" misses "Superfighters", a game with no sequel
/// in the archive. Digits and roman numerals up to X go; `None` when nothing
/// would go, or nothing would be left.
pub fn words_without_numbers(text: &str) -> Option<std::string::String> {
    const ROMAN: [&str; 10] = ["i", "ii", "iii", "iv", "v", "vi", "vii", "viii", "ix", "x"];
    let words: std::vec::Vec<&str> = text.split_whitespace().collect();
    let kept: std::vec::Vec<&str> = words
        .iter()
        .copied()
        .filter(|w| {
            let w = w.trim_matches(|c: char| !c.is_alphanumeric()).to_ascii_lowercase();
            !(w.is_empty() || w.chars().all(|c| c.is_ascii_digit()) || ROMAN.contains(&w.as_str()))
        })
        .collect();
    (!kept.is_empty() && kept.len() < words.len()).then(|| kept.join(" "))
}

/// The games of the route's answer: `{"total", "pages", "page", "games"}`, or
/// `{"games"}` for a random pick. `None` when it is not that.
pub fn api_games(json: &serde_json::Value) -> Option<&std::vec::Vec<serde_json::Value>> {
    json.get("games").and_then(|g| g.as_array())
}
const IMAGE_BASE: &str = "https://infinity.unstable.life/images";

/// Default cover art: the game's LOGO, as it has always been. The picker offers
/// the screenshot as an alternative (issue #59) rather than swapping the default,
/// because a change of default would silently restyle a library already full of
/// chosen covers.
///
/// Both kinds live on the same server under the same sharded scheme, at the same
/// cost, and differ mainly in shape. Screenshots are always game-shaped, measured
/// between 1.4:1 and 1.7:1 across a sample of the library, so they fill a tile as
/// they are. Logos have no common format: QWOP's is a 640x76 banner, 8.4:1, which
/// loses nearly everything to the tile crop, while Hot Dog Bush's is a 156x175
/// badge. Which one reads better is per game, which is exactly why it is a choice
/// and not a rule.
pub fn cover_url(id: &str) -> std::string::String {
    sharded_image_url("Logos", id)
}

/// The picker's Y toggle reaches the screenshot by swapping the `Logos/` segment
/// of this URL for `Screenshots/` (see `library::cover_url_for_source`), rather
/// than rebuilding it from the id, so the sharding rule lives in one place.

/// `<base>/<kind>/<id[0:2]>/<id[2:4]>/<id>.png`. Despite the `.png`, the server
/// actually returns JPEG for both kinds; the decoder sniffs the content, so the
/// extension is only part of the address.
fn sharded_image_url(kind: &str, id: &str) -> std::string::String {
    if id.len() >= 4 {
        std::format!(
            "{}/{}/{}/{}/{}.png",
            IMAGE_BASE,
            kind,
            &id[0..2],
            &id[2..4],
            id
        )
    } else {
        std::format!("{}/{}/{}.png", IMAGE_BASE, kind, id)
    }
}

/// Search Flashpoint by game name. Returns up to a handful of candidates so the
/// user can pick a cover. Synchronous HTTPS GET + JSON parse; the response is
/// small (a few KB per hit).
pub fn search(
    name: &str,
) -> Result<std::vec::Vec<CatalogEntry>, std::string::String> {
    // Nothing to search for: the route would answer with the archive's first
    // games by name, which are nobody's cover.
    let words = query_words(name);
    if words.is_empty() {
        return Ok(std::vec::Vec::new());
    }
    let url = api_url(&words, 12, false, false);
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
    // Anything but the route's answer is unreadable, as the game search says
    // (gamezip::parse_search): the archive.org wording this borrowed named a
    // field Flashpoint never had.
    let arr = api_games(&json)
        .ok_or_else(|| std::string::String::from(crate::loc_ui::ui().imp_err_fp_unreadable))?;
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
            // The cover picker shows none of these (the FpGallery details
            // popup does, from gamezip::parse_search).
            publisher: std::string::String::new(),
            release_date: std::string::String::new(),
            cover_url: cover_url(id),
            launch_command: std::string::String::new(),
            zipped: false,
            zip_url: std::string::String::new(),
            size: 0,
            description: std::string::String::new(),
        });
        // A short candidate list is all the cover-picker needs.
        if out.len() >= 12 {
            break;
        }
    }
    Ok(out)
}
