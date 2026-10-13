//! The importer's discovery shelves: a studio drawn at random, and games drawn
//! at random from the whole archive. Both are db-api searches (Flashpoint's
//! public API, the one the search and the downloads already use); this module
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
use crate::sources::flashpoint::CatalogEntry;

extern "C" {
    fn ruffle_unix_time() -> i64;
}

/// Seconds since 1970 on the console's clock: with the tick, what seeds the
/// draws, so two launches do not start from the same studio.
pub fn unix_time() -> u64 {
    unsafe { ruffle_unix_time() }.max(0) as u64
}

const SEARCH_BASE: &str = "https://db-api.unstable.life/search";
const SCREENSHOT_BASE: &str = "https://db-api.unstable.life/screenshot";

/// What a shelf asks for: what the download and the details window need, as
/// the search asks, and the tags `family_filter` reads.
const FIELDS: &str = "id,title,developer,publisher,releaseDate,zipped,launchCommand,tags";

/// The filters every shelf search carries: Flash games (not animations) that
/// run, without the entries the archive flags as extreme.
const SHELF_FILTERS: &str = "platform=Flash&library=arcade&status=Playable";

/// Games a shelf shows. Its last two tiles are its actions: see them all
/// (or more of the genre), and draw again.
pub const SHELF_GAMES: usize = 5;

/// A studio needs this many games, after `family_filter`, to be drawn. Fewer
/// than a full shelf is fine: a small studio is often where the good games
/// nobody heard of are (Jonathan, 2026-10-13); under three, the shelf would
/// look empty.
pub const STUDIO_MIN_GAMES: usize = 3;

/// How many hex digits a sample of the archive takes: three keep about one
/// game in 140, some 500 Flash games of every kind and size.
const SAMPLE_HEX: usize = 3;

/// How many hex digits a draw takes. A draw asks for the games whose
/// identifier contains a few random hex digits: identifiers are random, so
/// that is a random sample of the archive, and the API has no random order of
/// its own. Four keep about one Flash game in 2 300, some thirty of them
/// (measured on 2026-10-13); "see more" takes one digit fewer, sixteen times
/// as many.
const DRAW_HEX: usize = 4;

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
];

/// A search that samples the archive: the developers of the games whose
/// identifier contains a few hex digits drawn by `rng`.
pub fn sample_url(rng: &mut Rng) -> std::string::String {
    let hex = rng.hex(SAMPLE_HEX);
    std::format!("{}?id={}&{}&filter=true&fields=developer", SEARCH_BASE, hex, SHELF_FILTERS)
}

/// The studios of a sample (`sample_url`'s answer), each once, in an order
/// `rng` shuffles: the candidates, tried in turn until one has enough games,
/// and the next ones at each draw. Names too short to search for, and
/// placeholders, are left out.
pub fn studio_candidates(bytes: &[u8], rng: &mut Rng) -> std::vec::Vec<std::string::String> {
    let Ok(serde_json::Value::Array(arr)) = serde_json::from_slice::<serde_json::Value>(bytes) else {
        return std::vec::Vec::new();
    };
    // The first name of each field only: a game credits its studio first,
    // then its other hands, and one author's games can credit dozens of his
    // own aliases ("Duke Boobee BZ Lar", "Dr. Steward 75K"...), each of which
    // used to be a studio of its own and drew the same games five times.
    let mut names: std::vec::Vec<std::string::String> = std::vec::Vec::new();
    for g in &arr {
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

/// The names in a db-api `developer` field: several are joined by `;` or by
/// a slash ("John Cooney / jmtb02", "DX Interactive; Damien Clarke").
pub fn segments(dev: &str) -> impl Iterator<Item = &str> {
    dev.split([';', '/']).map(str::trim).filter(|s| !s.is_empty())
}

/// The search for `name`'s games (a substring match on the developer field;
/// `family_filter` keeps the exact name). `filter` leaves out what the archive
/// flags as extreme: always on a shelf, the player's choice on the results
/// page (as for a search). At most `STUDIO_LIMIT` rows come back: a studio can
/// have more than 1 500 games, half a megabyte to fetch and parse for a shelf
/// of six, and the results page shows sixty at most anyway.
pub fn studio_url(name: &str, filter: bool) -> std::string::String {
    std::format!(
        "{}?developer={}&{}&filter={}&limit={}&fields={}",
        SEARCH_BASE,
        net::url_encode_path(name),
        SHELF_FILTERS,
        filter,
        STUDIO_LIMIT,
        FIELDS
    )
}

/// Rows a studio's search asks for at most (see `studio_url`).
const STUDIO_LIMIT: usize = 300;

/// A draw: the games whose identifier contains `DRAW_HEX` digits drawn by
/// `rng`, same filters as a studio's.
pub fn draw_url(rng: &mut Rng) -> std::string::String {
    let hex = rng.hex(DRAW_HEX);
    std::format!("{}?id={}&{}&filter=true&fields={}", SEARCH_BASE, hex, SHELF_FILTERS, FIELDS)
}

/// "See more" of the draw: one hex digit fewer, sixteen times as many games,
/// at most `STUDIO_LIMIT` rows (the results show sixty).
pub fn draw_more_url(rng: &mut Rng, filter: bool) -> std::string::String {
    let hex = rng.hex(DRAW_HEX - 1);
    std::format!(
        "{}?id={}&{}&filter={}&limit={}&fields={}",
        SEARCH_BASE,
        hex,
        SHELF_FILTERS,
        filter,
        STUDIO_LIMIT,
        FIELDS
    )
}

/// A search's answer with the games a shelf must not show taken out (any of
/// `EXCLUDED_TAGS`), and, for a studio, the games of another developer whose
/// name only contains `developer`'s (the search matches a substring: "Max"
/// would find "Max Games" and "Maxwell"). Handed to `gamezip::parse_search`
/// as it is. An answer that is not a list comes back untouched, for the
/// parser to say what is wrong with it.
pub fn family_filter(bytes: &[u8], developer: Option<&str>) -> std::vec::Vec<u8> {
    let Ok(serde_json::Value::Array(arr)) = serde_json::from_slice::<serde_json::Value>(bytes) else {
        return bytes.to_vec();
    };
    let kept: std::vec::Vec<serde_json::Value> = arr
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
            clean && theirs
        })
        .collect();
    serde_json::to_vec(&serde_json::Value::Array(kept)).unwrap_or_else(|_| bytes.to_vec())
}

/// A game's screenshot as a tile shows it: reduced by the API (320 px wide,
/// JPEG), about 15 KB where the full PNG is near 500 KB. Flashpoint's
/// bandwidth is paid for by donations; a shelf costs it next to nothing.
pub fn screenshot_url(id: &str) -> std::string::String {
    std::format!("{}?id={}&width=320&format=jpeg&quality=80", SCREENSHOT_BASE, net::url_encode_path(id))
}

/// A small xorshift, seeded by the clock: the samples, the studios drawn from
/// them, and which games a shelf shows.
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

    /// `len` random hex digits.
    pub fn hex(&mut self, len: usize) -> std::string::String {
        const DIGITS: &[u8; 16] = b"0123456789abcdef";
        (0..len).map(|_| DIGITS[self.below(16)] as char).collect()
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
