//! A Flashpoint game's description in the interface's language (X in its
//! details window).
//!
//! The text goes to the relay's `/translate` (the same Cloudflare Worker as the
//! bug reports, `tools/bug-report-worker/`), which asks Workers AI and keeps
//! every translation in KV: a description someone already translated comes
//! back at once, for everybody. Which model translates is the relay's choice,
//! so a retired model is replaced there without a new FlashNX.
//!
//! X turns the translation on for the session, not for one window: the next
//! game's details open translated too, until X again. The player asked for
//! descriptions in their language, not for this one. A failure (no network,
//! the day's quota spent) turns it off with a toast, rather than failing
//! again in every window after.
//!
//! The request runs on net.cpp's request thread (`net::start_post_async`) and
//! is polled from the frames (`drive`), so the window keeps answering, its
//! spinner turning, while the translation comes.
//!
//! X is not offered for a description already written in the interface's
//! language (`written_in`), which is most of them in English: Flashpoint's
//! descriptions are in English even for most foreign games. Measured on 1 700
//! of them (6 random draws, and 11 by game language): an English interface
//! hides X on 87 % of the descriptions, and no description in another
//! language lost it.

use crate::net;

const ENDPOINT: &str = "https://flashnx.j-levy228.workers.dev/translate";

/// Translations kept for the session, the oldest dropped past this. One
/// details window after another, the same few descriptions come back.
const CACHE_MAX: usize = 64;

/// The relay refuses more (`TRANSLATE_MAX` in worker.js): such a text is not
/// sent at all.
const TEXT_MAX: usize = 4000;

struct Tr {
    on: bool,
    /// The request on its way, 0 for none, and the text it is for.
    job: i32,
    job_key: u64,
    cache: std::vec::Vec<(u64, std::string::String)>,
    /// Whether X is offered for the last text asked about, by key: the
    /// window asks every frame, about the same description.
    offered: (u64, bool),
}

static TR: std::sync::Mutex<Tr> = std::sync::Mutex::new(Tr {
    on: false,
    job: 0,
    job_key: 0,
    cache: std::vec::Vec::new(),
    offered: (0, false),
});

/// What a details window shows in place of its description.
pub(crate) enum View {
    /// The description as Flashpoint gives it.
    Original,
    /// Translation on, still on its way: the original meanwhile.
    Pending,
    Translated(std::string::String),
}

/// The text and the language it is wanted in, as one key (FNV-1a).
fn key(text: &str, lang: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in lang.bytes().chain([0u8]).chain(text.bytes()) {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// Whether X can do anything for this description: there is one, the relay
/// takes it, and it is not already in the interface's language.
pub(crate) fn available(text: &str) -> bool {
    match TR.lock() {
        Ok(mut t) => offered(&mut t, text),
        Err(_) => false,
    }
}

fn offered(t: &mut Tr, text: &str) -> bool {
    let text = text.trim();
    if text.is_empty() || text.len() > TEXT_MAX {
        return false;
    }
    let lang = crate::loc::current();
    let k = key(text, lang.code());
    if t.offered.0 != k {
        t.offered = (k, !written_in(text, detector_lang(lang)));
    }
    t.offered.1
}

fn detector_lang(lang: crate::loc::Lang) -> whichlang::Lang {
    use crate::loc::Lang as L;
    use whichlang::Lang as W;
    match lang {
        L::En => W::Eng,
        L::Fr => W::Fra,
        L::Es => W::Spa,
        L::Ru => W::Rus,
        L::De => W::Deu,
        L::It => W::Ita,
        L::Pt => W::Por,
        L::Zh => W::Cmn,
        L::Tr => W::Tur,
    }
}

/// Below this many letters a text is not judged: X stays.
const MIN_LETTERS: usize = 30;
/// The pieces that vote, a sentence or a few, at least this many letters.
const PIECE_LETTERS: usize = 40;

fn letters(s: &str) -> usize {
    s.chars().filter(|c| c.is_alphabetic()).count()
}

/// `text` is written in `lang`, beyond doubt. Every test can only keep X:
/// wrongly hidden, a description could not be translated at all, where X
/// wrongly kept only costs a request that brings the same text back.
///
/// whichlang (Quickwit's detector) names the language of the whole text,
/// then of each of its pieces, and three in four must agree: a description
/// in two languages ("EN: ... ZH: ...") keeps X. It knows 16 languages and
/// files the others under the nearest it knows, which the tests on letters
/// catch: Polish read as German, Indonesian as Turkish, Tagalog as German,
/// Bulgarian and Ukrainian as Russian were all seen on real texts or tried.
fn written_in(text: &str, lang: whichlang::Lang) -> bool {
    use whichlang::{detect_language, Lang as W};
    let n = letters(text);
    if n < MIN_LETTERS || detect_language(text) != lang {
        return false;
    }
    // Letters the language does not have (ą ł ż in "German", і ї є in
    // "Russian"): a few are names, more is another language.
    let alien = text.chars().filter(|c| c.is_alphabetic() && foreign(*c, lang)).count();
    if alien > (n * 3 / 100).max(2) {
        return false;
    }
    // Bulgarian has exactly the Russian letters, but no "ы", which Russian
    // can hardly write a sentence without, and "ъ" all over.
    if lang == W::Rus {
        let count = |set: &[char]| text.chars().filter(|c| set.contains(c)).count();
        if count(&['ы', 'Ы']) == 0 && count(&['ъ', 'Ъ']) >= 2 {
            return false;
        }
    }
    if !evidence(text, lang) {
        return false;
    }
    let pieces = pieces(text);
    let agree = pieces.iter().filter(|p| detect_language(p) == lang).count();
    agree * 4 >= pieces.len() * 3
}

/// A letter `lang` does not write: another script, or for the Latin ones an
/// accent of another language. Latin names are at home in a Russian or a
/// Chinese text.
fn foreign(c: char, lang: whichlang::Lang) -> bool {
    use whichlang::Lang as W;
    let cyrillic = ('\u{0400}'..='\u{04FF}').contains(&c);
    let kana_hangul = ('\u{3040}'..='\u{30FF}').contains(&c) || ('\u{AC00}'..='\u{D7AF}').contains(&c);
    let han = ('\u{4E00}'..='\u{9FFF}').contains(&c) || ('\u{3400}'..='\u{4DBF}').contains(&c);
    let lower = c.to_lowercase().next().unwrap_or(c);
    match lang {
        W::Rus => cyrillic && !matches!(lower, 'а'..='я' | 'ё'),
        W::Cmn => kana_hangul,
        W::Tur if c == 'İ' => false,
        _ => !c.is_ascii() && (cyrillic || kana_hangul || han || !accents(lang).contains(lower)),
    }
}

/// The accented letters a Latin language writes.
fn accents(lang: whichlang::Lang) -> &'static str {
    use whichlang::Lang as W;
    match lang {
        W::Fra => "àâæçéèêëîïôœùûüÿ",
        W::Spa => "áéíñóúü",
        W::Deu => "äöüß",
        W::Ita => "àèéìíîòóùú",
        W::Por => "áâãàçéêíóôõú",
        // And the capital "İ", whose lower case is a plain "i".
        W::Tur => "çğıöşü",
        _ => "",
    }
}

/// Something only `lang` would write: one of its accented letters, or for
/// English, which has none, one of its commonest words. A language the
/// detector does not know, written without accents (Indonesian, Tagalog), is
/// otherwise filed under one it does.
fn evidence(text: &str, lang: whichlang::Lang) -> bool {
    use whichlang::Lang as W;
    match lang {
        W::Eng => {
            const WORDS: [&str; 22] = [
                "the", "and", "you", "your", "to", "of", "is", "in", "with", "for", "a", "i", "it", "this", "that",
                "on", "my", "are", "was", "be", "can", "all",
            ];
            text.split(|c: char| !c.is_alphabetic())
                .any(|w| WORDS.iter().any(|x| w.eq_ignore_ascii_case(x)))
        }
        W::Rus | W::Cmn => true,
        _ => {
            let marks = accents(lang);
            text.chars().any(|c| {
                (lang == W::Spa && matches!(c, '¡' | '¿'))
                    || (lang == W::Tur && c == 'İ')
                    || marks.contains(c.to_lowercase().next().unwrap_or(c))
            })
        }
    }
}

/// A sentence or a few, each with at least `PIECE_LETTERS` letters; a short
/// end joins the piece before it.
fn pieces(text: &str) -> std::vec::Vec<&str> {
    let mut spans: std::vec::Vec<(usize, usize)> = std::vec::Vec::new();
    let (mut start, mut count) = (0, 0);
    for (i, c) in text.char_indices() {
        if c.is_alphabetic() {
            count += 1;
        }
        if matches!(c, '.' | '!' | '?' | '\n' | '。' | '！' | '？') && count >= PIECE_LETTERS {
            let end = i + c.len_utf8();
            spans.push((start, end));
            start = end;
            count = 0;
        }
    }
    if count > 0 || spans.is_empty() {
        match spans.last_mut() {
            Some(last) if count < PIECE_LETTERS => last.1 = text.len(),
            _ => spans.push((start, text.len())),
        }
    }
    spans.into_iter().map(|(a, b)| &text[a..b]).collect()
}

pub(crate) fn is_on() -> bool {
    TR.lock().map(|t| t.on).unwrap_or(false)
}

/// X: translation on, or back to the originals.
pub(crate) fn toggle() {
    if let Ok(mut t) = TR.lock() {
        t.on = !t.on;
    }
}

pub(crate) fn view(text: &str) -> View {
    let Ok(mut t) = TR.lock() else { return View::Original };
    if !t.on || !offered(&mut t, text) {
        return View::Original;
    }
    let k = key(text.trim(), crate::loc::current().code());
    match t.cache.iter().find(|(ck, _)| *ck == k) {
        Some((_, tr)) => View::Translated(tr.clone()),
        None => View::Pending,
    }
}

#[derive(serde::Deserialize)]
struct Answer {
    ok: bool,
    #[serde(default)]
    text: std::string::String,
}

/// Once a frame while a details window is up, with its game's Flashpoint id
/// and description, and from the page behind it (`None`), which collects an
/// answer still on its way after the window closed. Starts the request the
/// window needs when translation is on and `can_start` (no search of the page
/// in flight on the same thread, which a new request would cancel). Returns
/// the message of a failure, for a toast: translation is off again by then.
///
/// The relay translates a text only once it has checked with Flashpoint that
/// it is that game's description, hence the id.
pub(crate) fn drive(game: Option<(&str, &str)>, can_start: bool) -> Option<std::string::String> {
    let mut t = TR.lock().ok()?;
    if t.job != 0 {
        match net::tick_post_async(t.job, 64 * 1024) {
            Some(net::GetPoll::Pending) => return None,
            Some(net::GetPoll::Done(bytes)) => {
                t.job = 0;
                match serde_json::from_slice::<Answer>(&bytes) {
                    Ok(a) if a.ok && !a.text.trim().is_empty() => {
                        if t.cache.len() >= CACHE_MAX {
                            t.cache.remove(0);
                        }
                        let k = t.job_key;
                        t.cache.push((k, a.text.trim().to_string()));
                    }
                    _ => {
                        net::log(&std::format!(
                            "translate: unusable answer ({} bytes): {}\n",
                            bytes.len(),
                            std::string::String::from_utf8_lossy(&bytes[..bytes.len().min(200)])
                        ));
                        return fail(&mut t, std::string::String::from(crate::loc_ui::ui().fp_tr_failed));
                    }
                }
            }
            Some(net::GetPoll::Error(e)) => {
                net::log(&std::format!("translate: request failed: {}\n", e));
                return fail(&mut t, std::string::String::from(crate::loc_ui::ui().fp_tr_failed));
            }
            // Superseded by another request (a new search): nobody waits for
            // it any more.
            None => t.job = 0,
        }
    }
    let (id, text) = game?;
    let text = text.trim();
    if !t.on || !can_start || !offered(&mut t, text) {
        return None;
    }
    let lang = crate::loc::current().code();
    let k = key(text, lang);
    if t.cache.iter().any(|(ck, _)| *ck == k) || (t.job != 0 && t.job_key == k) {
        return None;
    }
    let body = serde_json::json!({ "id": id, "text": text, "lang": lang }).to_string();
    match net::start_post_async(ENDPOINT, &body) {
        Ok(id) => {
            net::log(&std::format!("translate: {} chars into {}\n", text.chars().count(), lang));
            t.job = id;
            t.job_key = k;
            None
        }
        // Offline, mostly: the message says so.
        Err(e) => fail(&mut t, e),
    }
}

fn fail(t: &mut Tr, msg: std::string::String) -> Option<std::string::String> {
    t.on = false;
    t.job = 0;
    Some(msg)
}
