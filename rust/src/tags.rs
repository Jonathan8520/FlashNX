//! Shelf labels, persisted to `tags.json` BESIDE THE LIBRARY — that is, in the
//! games folder, wherever the player has pointed it (#79), not at a fixed
//! `sdmc:/flashnx/`. Keyed by `.swf` basename.
//!
//! A shelf used to be one thing: a real subdirectory of the games folder, so a
//! game was on exactly one shelf because a file is in exactly one place. These
//! labels are the second half. A game's shelves are the UNION of the directory
//! it physically sits in and the labels listed here, so it can be on several at
//! once without anything being copied.
//!
//! Real directories keep working untouched: whoever files their games from a PC
//! sees those folders as shelves exactly as before. Nothing here moves a file.
//!
//! Keyed by BASENAME, like [crate::favorites], the cover cache, the save files,
//! the keymap and the playtime. That is the identity this app already uses for
//! "the same game", and a label that disagreed with a star about which game it
//! belonged to would be a second, competing answer to the same question. The
//! known cost is the known cost of that choice everywhere else: two games with
//! one file name share it (see `twin_basename_exists` in library.rs).
//!
//! JSON: an object of basename -> array of labels, e.g.
//! `{"mario.swf": ["MARIO", "PLATFORM"]}`. Vec storage, NOT HashMap:
//! `HashMap::new` crashes on Horizon without the stdlib RandomState patch, which
//! is why favorites/playtime/covers all use Vec too.

use std::sync::Mutex;

/// File name only: it lives beside the library, wherever that is (#79).
const FILE_NAME: &str = "tags.json";

/// Biggest file we will read. One line per tagged game; a library of thousands
/// with a handful of labels each is still far under this.
const MAX_BYTES: usize = 256 * 1024;

/// Biggest file we will WRITE. Strictly under [MAX_BYTES] so anything this app
/// puts on the card can always be read back.
const WRITE_MAX: usize = 192 * 1024;

/// Read path: the games folder first, the built-in roots after, so an install
/// from before the folder could move still finds its data.
fn read_path() -> std::string::String {
    crate::library::config_read_path(FILE_NAME)
}

/// Write path: always beside the library.
fn write_path() -> std::string::String {
    crate::library::config_write_path(FILE_NAME)
}

/// `(basename, labels)`. A game with no labels is not stored at all.
static TAGS: Mutex<std::vec::Vec<(std::string::String, std::vec::Vec<std::string::String>)>> =
    Mutex::new(std::vec::Vec::new());

/// True once the table on the card is known — either read successfully, or shown
/// not to exist. False means the in-memory table is not the file's contents, and
/// `save` must not write: `fs::write` truncates, so labelling one game from a
/// table we merely FAILED to read would replace every label with that single
/// entry. Same guard, and the same reason, as `favorites::LOADED`.
static LOADED: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

/// Load the persisted labels from SD.
///
/// A missing file is a normal first boot and counts as loaded (empty really is
/// the truth). Anything else — unreadable, over the cap, not JSON, not an
/// object — leaves the table sealed rather than quietly empty.
pub fn load() {
    use core::sync::atomic::Ordering::Relaxed;
    match std::fs::metadata(read_path()) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            LOADED.store(true, Relaxed);
            return;
        }
        Err(e) => {
            crate::net::log(&std::format!(
                "tags: {} unreadable ({}) - shelf labels are frozen this session\n",
                read_path(),
                e,
            ));
            return;
        }
        Ok(_) => {}
    }
    let Some(bytes) = read_file_bounded(&read_path(), MAX_BYTES) else {
        crate::net::log("tags: read failed or over cap - shelf labels are frozen this session\n");
        return;
    };
    let Ok(json) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
        crate::net::log("tags: file is not valid JSON - shelf labels are frozen this session\n");
        return;
    };
    let Some(obj) = json.as_object() else {
        crate::net::log("tags: file is not a JSON object - shelf labels are frozen this session\n");
        return;
    };
    if let Ok(mut g) = TAGS.lock() {
        g.clear();
        for (basename, v) in obj {
            if basename.is_empty() {
                continue;
            }
            let Some(arr) = v.as_array() else { continue };
            let mut labels: std::vec::Vec<std::string::String> = std::vec::Vec::new();
            for l in arr {
                if let Some(s) = l.as_str() {
                    // Stored VERBATIM (no trim): `on_shelf` compares a label
                    // with a directory name straight off the card, which is
                    // not trimmed either. An all-blank label would be a shelf
                    // with no name, which the UI can neither show nor take off.
                    if !s.trim().is_empty() && !labels.iter().any(|x| x == s) {
                        labels.push(s.to_string());
                    }
                }
            }
            if !labels.is_empty() {
                g.push((basename.clone(), labels));
            }
        }
        LOADED.store(true, Relaxed);
        crate::net::log(&std::format!("tags: {} labelled game(s) loaded\n", g.len()));
    }
}

/// The labels on `basename`, in the order they were added.
pub fn tags_for(basename: &str) -> std::vec::Vec<std::string::String> {
    TAGS.lock()
        .ok()
        .and_then(|g| g.iter().find(|(b, _)| b == basename).map(|(_, t)| t.clone()))
        .unwrap_or_default()
}

/// Add or remove `tag` on `basename`, persist, and return the NEW state.
/// `None` means nothing was changed because the table is frozen (see [save]) --
/// the caller must report a failure rather than a state it cannot keep.
///
/// The tag is stored VERBATIM. It used to be trimmed, and `on_shelf` compares it
/// against a directory name straight off the card, which is not trimmed: a shelf
/// whose directory name had a leading space got its label filed under a
/// different name, so the game did not appear on the shelf and a ghost one
/// showed up beside it.
pub fn toggle(basename: &str, tag: &str) -> Option<bool> {
    if basename.is_empty() || tag.trim().is_empty() {
        return None;
    }
    // Checked BEFORE the table is touched: `save` refuses to write a table it
    // never read, and a mutation that cannot be persisted would show on screen
    // for one session and be gone at the next boot.
    if !LOADED.load(core::sync::atomic::Ordering::Relaxed) {
        crate::net::log("tags: refusing to change a table that was never read
");
        return None;
    }
    let now_on = {
        let Ok(mut g) = TAGS.lock() else { return None };
        match g.iter().position(|(b, _)| b == basename) {
            Some(i) => {
                let labels = &mut g[i].1;
                match labels.iter().position(|x| x == tag) {
                    Some(p) => {
                        labels.remove(p);
                        // A game with no labels left leaves the file rather than
                        // sitting in it as an empty array.
                        if labels.is_empty() {
                            g.remove(i);
                        }
                        false
                    }
                    None => {
                        labels.push(tag.to_string());
                        true
                    }
                }
            }
            None => {
                g.push((basename.to_string(), std::vec![tag.to_string()]));
                true
            }
        }
    };
    if !save() {
        // Put the table back: what is on screen has to be what is on the card.
        let _ = revert_toggle(basename, tag, now_on);
        return None;
    }
    Some(now_on)
}

/// Undo an in-memory toggle whose write failed.
fn revert_toggle(basename: &str, tag: &str, was_added: bool) -> bool {
    let Ok(mut g) = TAGS.lock() else { return false };
    match g.iter().position(|(b, _)| b == basename) {
        Some(i) => {
            let labels = &mut g[i].1;
            if was_added {
                if let Some(p) = labels.iter().position(|x| x == tag) {
                    labels.remove(p);
                }
                if labels.is_empty() {
                    g.remove(i);
                }
            } else if !labels.iter().any(|x| x == tag) {
                labels.push(tag.to_string());
            }
        }
        None => {
            if !was_added {
                g.push((basename.to_string(), std::vec![tag.to_string()]));
            }
        }
    }
    true
}

/// Drop every label from `basename`. Returns how many it had, or `None` if the
/// table is frozen and nothing could be written.
pub fn clear_for(basename: &str) -> Option<usize> {
    if !LOADED.load(core::sync::atomic::Ordering::Relaxed) {
        crate::net::log("tags: refusing to change a table that was never read
");
        return None;
    }
    let taken = {
        let Ok(mut g) = TAGS.lock() else { return None };
        match g.iter().position(|(b, _)| b == basename) {
            Some(i) => Some(g.remove(i).1),
            None => None,
        }
    };
    let Some(labels) = taken else { return Some(0) };
    if !save() {
        // Put them back: the card still has them.
        if let Ok(mut g) = TAGS.lock() {
            g.push((basename.to_string(), labels));
        }
        return None;
    }
    Some(labels.len())
}

/// Drop `basename` entirely (its game was deleted). Best effort: a frozen table
/// simply keeps the line, which costs a few bytes and no correctness.
pub fn remove(basename: &str) {
    let _ = clear_for(basename);
}

/// Take `tag` off every game that carries it. Returns how many lost it, or
/// `None` if the table is frozen and nothing could be written.
///
/// This is what emptying a shelf does to the label half; the games that are
/// physically inside a directory of that name are a separate matter, and the
/// caller moves those.
pub fn remove_tag_everywhere(tag: &str) -> Option<usize> {
    if !LOADED.load(core::sync::atomic::Ordering::Relaxed) {
        crate::net::log("tags: refusing to change a table that was never read
");
        return None;
    }
    let before = { let Ok(g) = TAGS.lock() else { return None }; g.clone() };
    let n = {
        let Ok(mut g) = TAGS.lock() else { return None };
        let mut n = 0;
        for (_, labels) in g.iter_mut() {
            if let Some(p) = labels.iter().position(|x| x == tag) {
                labels.remove(p);
                n += 1;
            }
        }
        g.retain(|(_, labels)| !labels.is_empty());
        n
    };
    if n == 0 {
        return Some(0);
    }
    if !save() {
        if let Ok(mut g) = TAGS.lock() {
            *g = before;
        }
        return None;
    }
    Some(n)
}

/// Write the table out. Returns whether the card now holds it.
fn save() -> bool {
    if !LOADED.load(core::sync::atomic::Ordering::Relaxed) {
        crate::net::log("tags: NOT saving - the table was never read, writing would replace it
");
        return false;
    }
    let mut obj = serde_json::Map::new();
    match TAGS.lock() {
        Ok(g) => {
            for (basename, labels) in g.iter() {
                let arr: std::vec::Vec<serde_json::Value> = labels
                    .iter()
                    .map(|l| serde_json::Value::from(l.clone()))
                    .collect();
                obj.insert(basename.clone(), serde_json::Value::Array(arr));
            }
        }
        Err(_) => return false,
    }
    let json = serde_json::Value::Object(obj);
    let Ok(text) = serde_json::to_string_pretty(&json) else {
        return false;
    };
    // A file we would not be able to READ back is a file we must not write.
    // `load` gives up past MAX_BYTES and then seals the table for ever, so a
    // write that crossed that line would make the labels unrecoverable without
    // a card reader -- not even removing labels would persist, since every
    // writer comes through here. WRITE_MAX sits well under MAX_BYTES so the
    // file can always be read back and shrunk again.
    if text.len() > WRITE_MAX {
        crate::net::log(&std::format!(
            "tags: NOT saving - {} bytes is over the {} byte cap
",
            text.len(),
            WRITE_MAX,
        ));
        return false;
    }
    match std::fs::write(write_path(), text.as_bytes()) {
        Ok(()) => {
            crate::sd::commit();
            true
        }
        Err(e) => {
            crate::net::log(&std::format!("tags: save failed: {}
", e));
            false
        }
    }
}

/// Bounded file read — `std::fs::read` can spuriously OOM on Horizon (it
/// pre-reserves from a bogus applet-mode fstat); read in fixed chunks instead.
fn read_file_bounded(path: &str, max: usize) -> Option<std::vec::Vec<u8>> {
    use std::io::Read;
    let mut f = std::fs::File::open(path).ok()?;
    let mut data: std::vec::Vec<u8> = std::vec::Vec::new();
    let mut buf = [0u8; 4096];
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
