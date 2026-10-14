//! The importer's discovery shelves: a studio drawn at random, and games drawn
//! at random from the whole archive. Both are searches of Flashpoint's site
//! (`flashpoint::api_url`, the route the game search asks too); this module
//! builds them and picks what a shelf shows.
//!
//! The second shelf drew inside one of eleven genres first, to keep the
//! anonymous dress-up games of a pure draw away; it read as a handful of
//! kinds of games ("t'as mis que 4 types de jeux ou quoi ? met du random
//! pur"), so it draws from everything.
//!
//! Neither depends on what the player owns or plays: a shelf built from the
//! library ("because you play X") was tried in the mockups and dropped, a
//! signal that depends on each player's games being too thin to trust.
//!
//! Neither runs out either. The studio shelf was first a list of twenty
//! studios, one a day: after twenty days it came round again (Jonathan: "au
//! bout de quelques jours on a épuisé les propositions ? non il faut du truc
//! à vie"). Then one studio a day from the whole archive; then, as a day is a
//! long wait for the next one ("un studio au hasard, ça laisse du choix de
//! découverte sans venir une fois par jour"), a studio drawn at random from a
//! sample of the archive, another at each draw. Thousands of studios qualify.

use crate::net;
use crate::sources::flashpoint::{self, CatalogEntry};

/// db-api still makes the screenshots smaller, new games' too: it fetches
/// them from the archive's image server by the game's id.
const SCREENSHOT_BASE: &str = "https://db-api.unstable.life/screenshot";

/// What every shelf search asks for: Flash games (not animations) that run.
/// The entries the archive flags as extreme are left out by the route itself.
///
/// `library:arcade` is read and then dropped by the archive's search (FPA
/// 0.13.7: the conversion of a parsed filter copies every field but the
/// library), so a third to a half of what comes back is animations
/// (`library` "theatre"; checked 2026-10-14, the same total with or without
/// it). `is_game` keeps the games; the filter stays in the query for the day
/// it works.
const SHELF_QUERY: &str = "platform:Flash library:arcade status:Playable";

/// A game, not an animation: in the archive's `arcade` library. An entry
/// that does not say is kept.
fn is_game(g: &serde_json::Value) -> bool {
    g.get("library").and_then(|v| v.as_str()).map_or(true, |lib| lib == "arcade")
}

/// Games a shelf shows. Its last two tiles are its actions: see them all
/// (or more of the genre), and draw again.
pub const SHELF_GAMES: usize = 5;

/// A studio needs this many games, after `family_filter`, to be drawn. Fewer
/// than a full shelf is fine: a small studio is often where the good games
/// nobody heard of are (Jonathan, 2026-10-13); under three, the shelf would
/// look empty.
pub const STUDIO_MIN_GAMES: usize = 3;

/// Games a sample of the archive draws, for their studios: some forty first
/// credits once the one-letter names and the placeholders are out, of every
/// kind and size.
const SAMPLE_GAMES: usize = 50;

/// Games a draw asks for. The route picks them at random from all the games
/// that match; db-api had no random order, and a draw asked for the games
/// whose identifier held a few random hex digits instead. Thirty leave enough
/// for a shelf of five once `family_filter` and the games that cannot run are
/// out; "see more" asks for all the route gives.
const DRAW_GAMES: usize = 30;

/// The most games one search of the route gives.
const ROUTE_MAX: usize = 100;

/// Content warnings a shelf leaves out, on top of the archive's own filter
/// (`filter=true`, which hides what it flags as extreme). A shelf is what the
/// tab opens on, chosen by nobody: a crude title or a gory screenshot there
/// is shown to whoever holds the console. Mild and moderate violence stay,
/// or fighting and shooting games would go. The search and the results keep
/// the player's own filter.
const EXCLUDED_TAGS: &[&str] = &[
    "Adult",
    "LEGACY-Extreme",
    "Sexual Content",
    "Sexual Harassment",
    "Sexual Violence",
    "Nudity",
    "Strong Language",
    "Crude Humor",
    "Gore",
    "Strong Violence",
    "Real World Violence",
    "Drugs",
    "Racism",
    "Homophobia",
    "Transphobia",
    "Suicide",
    "Domestic Violence",
    "Animal Cruelty",
    "Cannibalism",
    // Neither a warning nor a rating: Newgrounds' junk submissions (often a
    // screen of insults, "[KK]dont zero thisASSHOLE") and the screamers, a
    // face that jumps out with a shriek (Jonathan, 2026-10-14). Some 240 and
    // 320 Flash games, a third of a percent of the archive.
    "Spam",
    "Jumpscare",
];

/// A search that samples the archive: `SAMPLE_GAMES` games drawn at random,
/// for their developers.
pub fn sample_url() -> std::string::String {
    flashpoint::api_url(SHELF_QUERY, SAMPLE_GAMES, false, true)
}

/// The studios of a sample (`sample_url`'s answer), each once, in an order
/// `rng` shuffles: the candidates, tried in turn until one has enough games,
/// and the next ones at each draw. Names too short to search for, and
/// placeholders, are left out.
pub fn studio_candidates(bytes: &[u8], rng: &mut Rng) -> std::vec::Vec<std::string::String> {
    let Ok(json) = serde_json::from_slice::<serde_json::Value>(bytes) else {
        return std::vec::Vec::new();
    };
    let Some(arr) = flashpoint::api_games(&json) else {
        return std::vec::Vec::new();
    };
    // The first name of each field only: a game credits its studio first,
    // then its other hands, and one author's games can credit dozens of his
    // own aliases ("Duke Boobee BZ Lar", "Dr. Steward 75K"...), each of which
    // used to be a studio of its own and drew the same games five times.
    let mut names: std::vec::Vec<std::string::String> = std::vec::Vec::new();
    // Games only: an animator's name would draw a shelf of animations, every
    // one of them left out by `family_filter`.
    for g in arr.iter().filter(|g| is_game(g)) {
        let dev = g.get("developer").and_then(|v| v.as_str()).unwrap_or("");
        let Some(seg) = segments(dev).next() else { continue };
        let lower = seg.to_lowercase();
        let junk = seg.chars().count() < 3
            || matches!(lower.as_str(), "unknown" | "anonymous" | "n/a" | "various" | "none" | "?");
        if !junk && !names.iter().any(|n| n.eq_ignore_ascii_case(seg)) {
            names.push(seg.to_string());
        }
    }
    for i in (1..names.len()).rev() {
        let j = rng.below(i + 1);
        names.swap(i, j);
    }
    names
}

/// The names in a Flashpoint `developer` field: several are joined by `;` or
/// by a slash ("John Cooney / jmtb02", "DX Interactive; Damien Clarke").
pub fn segments(dev: &str) -> impl Iterator<Item = &str> {
    dev.split([';', '/']).map(str::trim).filter(|s| !s.is_empty())
}

/// The search for `name`'s games (a substring match on the developer field,
/// the name between quotes so that its spaces and colons stay in it;
/// `family_filter` keeps the exact name). `filter` leaves out what the archive
/// flags as extreme: always on a shelf, the player's choice on the results
/// page (as for a search). The route gives a hundred games at most, by name: a
/// studio can have more than 1 500, and the results page shows sixty anyway.
pub fn studio_url(name: &str, filter: bool) -> std::string::String {
    let query = std::format!("{} dev:\"{}\"", SHELF_QUERY, name.replace('"', ""));
    flashpoint::api_url(&query, ROUTE_MAX, !filter, false)
}

/// The search for `name`'s games from a game's details ("Du même studio", Y):
/// a search, not a shelf, so Flash games of every library and status, and
/// the player's filter (`filter`). `same_developer` keeps the exact name.
pub fn developer_url(name: &str, filter: bool) -> std::string::String {
    let query = std::format!("dev:\"{}\" platform:Flash", name.replace('"', ""));
    flashpoint::api_url(&query, ROUTE_MAX, !filter, false)
}

/// A search's answer with only `name`'s own games: `family_filter`'s
/// developer test alone, the player's filter having already chosen the rest.
pub fn same_developer(bytes: &[u8], name: &str) -> std::vec::Vec<u8> {
    let Ok(mut json) = serde_json::from_slice::<serde_json::Value>(bytes) else {
        return bytes.to_vec();
    };
    let Some(serde_json::Value::Array(arr)) = json.get_mut("games") else {
        return bytes.to_vec();
    };
    arr.retain(|g| {
        let dev = g.get("developer").and_then(|v| v.as_str()).unwrap_or("");
        segments(dev).any(|s| s.eq_ignore_ascii_case(name))
    });
    serde_json::to_vec(&json).unwrap_or_else(|_| bytes.to_vec())
}

/// A draw: `DRAW_GAMES` games picked at random.
pub fn draw_url() -> std::string::String {
    flashpoint::api_url(SHELF_QUERY, DRAW_GAMES, false, true)
}

/// "See more" of the draw: as many games as the route gives, picked at random
/// (the results show sixty).
pub fn draw_more_url(filter: bool) -> std::string::String {
    flashpoint::api_url(SHELF_QUERY, ROUTE_MAX, !filter, true)
}

/// A search's answer with the games a shelf must not show taken out (any of
/// `EXCLUDED_TAGS`, and the animations, `is_game`), and, for a studio, the
/// games of another developer whose name only contains `developer`'s (the
/// search matches a substring: "Max" would find "Max Games" and "Maxwell").
/// Handed to `gamezip::parse_search` as it is. An answer that is not the
/// route's comes back untouched, for the parser to say what is wrong with it.
pub fn family_filter(bytes: &[u8], developer: Option<&str>) -> std::vec::Vec<u8> {
    let Ok(mut json) = serde_json::from_slice::<serde_json::Value>(bytes) else {
        return bytes.to_vec();
    };
    let Some(serde_json::Value::Array(arr)) = json.get_mut("games") else {
        return bytes.to_vec();
    };
    let kept: std::vec::Vec<serde_json::Value> = std::mem::take(arr)
        .into_iter()
        .filter(|g| {
            let clean = match g.get("tags") {
                Some(serde_json::Value::Array(tags)) => {
                    !tags.iter().filter_map(|t| t.as_str()).any(|t| EXCLUDED_TAGS.contains(&t))
                }
                Some(serde_json::Value::String(tags)) => !tags.split("; ").any(|t| EXCLUDED_TAGS.contains(&t)),
                _ => true,
            };
            let theirs = match developer {
                Some(name) => {
                    let dev = g.get("developer").and_then(|v| v.as_str()).unwrap_or("");
                    segments(dev).any(|s| s.eq_ignore_ascii_case(name))
                }
                None => true,
            };
            is_game(g) && clean && theirs
        })
        .collect();
    *arr = kept;
    serde_json::to_vec(&json).unwrap_or_else(|_| bytes.to_vec())
}

/// A game's screenshot as a tile shows it: reduced by the API (320 px wide,
/// JPEG), about 15 KB where the full PNG is near 500 KB. Flashpoint's
/// bandwidth is paid for by donations; a shelf costs it next to nothing.
pub fn screenshot_url(id: &str) -> std::string::String {
    std::format!("{}?id={}&width=320&format=jpeg&quality=80", SCREENSHOT_BASE, net::url_encode_path(id))
}

/// A small xorshift, seeded by the clock: the order the studios of a sample
/// are tried in, and which games a shelf shows. The samples and the draws
/// themselves are picked by the route.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        // Never all zero, the one state an xorshift cannot leave; and mixed, so
        // two consecutive days do not start from neighbouring states.
        Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }

    pub fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    /// A number in `0..n` (`n` > 0).
    pub fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
}

/// Up to `SHELF_GAMES` of `all`, chosen by `rng`: the first ones of a
/// Fisher-Yates pass. The caller puts them in the results' order.
pub fn pick(all: &[CatalogEntry], rng: &mut Rng) -> std::vec::Vec<CatalogEntry> {
    let mut idx: std::vec::Vec<usize> = (0..all.len()).collect();
    let take = SHELF_GAMES.min(idx.len());
    for i in 0..take {
        let j = i + rng.below(idx.len() - i);
        idx.swap(i, j);
    }
    idx[..take].iter().map(|&i| all[i].clone()).collect()
}
