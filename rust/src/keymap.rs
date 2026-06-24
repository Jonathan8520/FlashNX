//! User-editable Switch-button → Flash-key bindings.
//!
//! Lookup hierarchy (first hit wins):
//!   1. `sdmc:/ruffle/<basename>.keymap.json` — per-game override (basename
//!      derived from the loaded SWF's URL, e.g. `Super_Mario_63_2010.swf`)
//!   2. `sdmc:/ruffle/keymap_default.json`    — global default chosen by the
//!      user
//!   3. Hardcoded fallback (`FALLBACK_BINDINGS`) — Mario-63-biased Flash
//!      platformer baseline, ships in the .nro. Always available so we never
//!      hand back an empty table.
//!
//! On first boot, if `keymap_default.json` is missing, we write the fallback
//! to SD so the user discovers the file's existence + schema in their
//! `sdmc:/ruffle/` folder.
//!
//! Pattern stolen from RetroArch (`.rmp` per-game remaps in `config/remaps/`)
//! and ScummVM (`[gameid]` INI sections). See README "Customisation des
//! touches" for the user-facing doc.
//!
//! No UI wizard yet — power users edit JSON manually. The in-game remap
//! wizard ("REMAPPER" entry in the pause menu) is a planned follow-up that
//! will write to the same sidecar files.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{Read, Write};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

/// JSON-serialisable keymap. The field order in `bindings` is preserved
/// (BTreeMap → alphabetical) so the file is diff-stable when the user
/// regenerates / edits it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Keymap {
    pub version: u32,
    /// Player 1 (controller 1) bindings.
    pub bindings: BTreeMap<std::string::String, std::string::String>,
    /// Player 2 (controller 2) bindings for local 2-player (issue #40), in the
    /// SAME file. `#[serde(default)]` so pre-#40 keymaps still load (an absent /
    /// empty map gets the P2 defaults via `merge_fallback_defaults_p2`).
    #[serde(default)]
    pub bindings_p2: BTreeMap<std::string::String, std::string::String>,
    /// Provenance of these bindings (issue #20, community control profiles), so
    /// the UI never clobbers hand-made controls silently:
    ///   "" / "default" = untouched fallback,
    ///   "user"         = edited by hand in the TOUCHES editor,
    ///   "community:<id>" = an applied shared profile.
    /// `#[serde(default)]` keeps pre-v1.4.0 keymaps loading (they read as "").
    #[serde(default)]
    pub source: std::string::String,
}

/// Hardcoded fallback baked into the .nro. Mirrors what `cpp/src/main.cpp`
/// used to declare as the static `BINDINGS` array — Mario 63's Z/X/Shift
/// platformer convention plus universal Space/Enter/Escape/P/arrows. Buttons
/// reserved by the runtime ("Minus" = pause-menu) are absent on purpose;
/// see `RESERVED_BUTTONS` for the explanation.
pub const FALLBACK_BINDINGS: &[(&str, &str)] = &[
    ("A",            "Space"),  // jump in most Flash games
    ("B",            "Z"),      // alt jump (Mario 63 uses Z)
    ("X",            "X"),      // run / item / dive
    ("Y",            "Shift"),  // alt run
    ("R",            "Enter"),  // "Press Start" prompts
    ("Plus",         "P"),      // standard in-game pause key
    ("L",            "Escape"),
    ("ZR",           "Left click"),  // primary click (also the legacy hardcoded ZR)
    ("ZL",           "Right click"), // secondary click
    ("Left",         "Left"),
    ("Right",        "Right"),
    ("Up",           "Up"),
    ("Down",         "Down"),
    ("StickLLeft",   "Left"),
    ("StickLRight",  "Right"),
    ("StickLUp",     "Up"),
    ("StickLDown",   "Down"),
];

/// Buttons the runtime owns — user cannot remap these via JSON. Trying to
/// bind one is silently ignored on load (with a warn log so the user sees
/// why their edit didn't take). Keeps the pause menu always reachable.
pub const RESERVED_BUTTONS: &[&str] = &["Minus"];

/// Buttons exposed in the TOUCHES editor UI (Phase 3.3 suite). Order =
/// display order in the list. Subset of all joycon buttons known to the C++
/// input layer; reserved buttons are absent on purpose.
pub const EDITABLE_BUTTONS: &[&str] = &[
    "A", "B", "X", "Y",
    "L", "R", "ZL", "ZR", "Plus",
    "SL", "SR",
    "Up", "Down", "Left", "Right",
    "StickLUp", "StickLDown", "StickLLeft", "StickLRight",
    // Right stick: binding ANY of these switches the right stick from cursor to
    // d-pad mode in-game (C++ skips the cursor path when one is set).
    "StickRUp", "StickRDown", "StickRLeft", "StickRRight",
    // Stick CLICKS (press the analog sticks in, L3/R3). Distinct from the
    // directional StickL*/StickR* above — they don't affect cursor/d-pad mode.
    "StickLPress", "StickRPress",
];

/// Flash-key options shown in the TOUCHES dropdown. Index 0 ("(none)")
/// unbinds the button. Must be a superset of what `flash_key_name_to_sk`
/// recognises — additions here without a matching `flash_key_name_to_sk`
/// arm will log "unknown Flash key" at lookup time. These are the CANONICAL,
/// language-stable names written to the keymap file; the TOUCHES editor shows
/// `flash_key_display(name)` (translated) but always stores the name verbatim.
pub const ALL_FLASH_KEYS: &[&str] = &[
    "(none)",
    // Common modifier / nav keys first (most used in Flash games).
    "Space",
    "Enter",
    "Escape",
    "Shift",
    "Control",
    "Alt",
    "Tab",
    "Backspace",
    "Up", "Down", "Left", "Right",
    // Full alphabet (A-Z). Sorted so the user can navigate by letter.
    "A", "B", "C", "D", "E", "F", "G", "H", "I", "J", "K", "L", "M",
    "N", "O", "P", "Q", "R", "S", "T", "U", "V", "W", "X", "Y", "Z",
    // Numpad digits 0-9 FIRST — distinct Flash key codes from the top row, and the
    // common P2 convention in 2-player fighters, so they're the more-used digits
    // here. PR #46 (YuQiyang).
    "Num0", "Num1", "Num2", "Num3", "Num4",
    "Num5", "Num6", "Num7", "Num8", "Num9",
    // Top-row digits 0-9.
    "0", "1", "2", "3", "4", "5", "6", "7", "8", "9",
    // Mouse clicks (at the cursor) — for games that need clicking from buttons
    // rather than the touchscreen. Routed to the mouse, not a key event.
    "Left click",
    "Right click",
];

/// SWF basename the keymap was loaded for. Used by `save_sidecar` to know
/// where to write. Set by `init_for_swf` — replaced when the user goes
/// back to the library and picks a different game (Phase 3.4 quit-to-
/// library flow). REDEMARRER keeps the same basename and so re-uses the
/// already-loaded keymap.
static ACTIVE_BASENAME: Mutex<Option<std::string::String>> = Mutex::new(None);

/// Sentinel "basename" set by `init_for_global_default`. When this is the
/// active basename, `save_sidecar` writes `keymap_default.json` instead of
/// a per-game `<basename>.keymap.json`. The leading control char makes it
/// impossible to collide with a real `.swf` file name.
const GLOBAL_SENTINEL: &str = "\u{1}__global_default__";

/// In-memory keymap. Lock is held briefly across single-key edits + during
/// sidecar write.
static ACTIVE_KEYMAP: Mutex<Option<Keymap>> = Mutex::new(None);

fn fallback_keymap() -> Keymap {
    let mut bindings = BTreeMap::new();
    for (btn, key) in FALLBACK_BINDINGS {
        bindings.insert((*btn).into(), (*key).into());
    }
    Keymap {
        version: 1,
        bindings,
        bindings_p2: p2_default_bindings(),
        source: "default".into(),
    }
}

/// Fill in the FALLBACK default for any button the loaded keymap doesn't mention
/// at all. Lets games keep getting sensible defaults as we add buttons (the
/// ZR/ZL mouse clicks, etc.) WITHOUT rewriting every saved sidecar: an absent
/// button gains its default; a button the user deliberately unbound is stored as
/// "" (present) and is left as-is. Applied in-memory after every load — the file
/// on SD is only rewritten when the user next edits a binding.
fn merge_fallback_defaults(km: &mut Keymap) {
    for (btn, key) in FALLBACK_BINDINGS {
        km.bindings
            .entry((*btn).into())
            .or_insert_with(|| (*key).into());
    }
}

/// v1.4.0: the mouse-click pseudo-keys used to be stored under French
/// identifiers ("Clic gauche"/"Clic droit"). Rewrite them in-memory to the
/// English canonical names ("Left click"/"Right click") on load, so the editor
/// dropdown + display match and the file is rewritten to English the next time
/// the user edits a binding. Old files keep working untouched until then —
/// `flash_key_name_to_sk` still accepts the French names. Applied to both the
/// P1 and P2 maps.
fn migrate_legacy_key_names(km: &mut Keymap) {
    for map in [&mut km.bindings, &mut km.bindings_p2] {
        for v in map.values_mut() {
            if v.as_str() == "Clic gauche" {
                *v = "Left click".into();
            } else if v.as_str() == "Clic droit" {
                *v = "Right click".into();
            }
        }
    }
}

/// User-visible SD roots, priority order. Reads scan all, first hit
/// wins. Writes always go to entry 0. Mirrors `library::USER_SD_ROOTS`
/// — duplicated here to keep keymap a leaf module (no library dep).
const USER_SD_ROOTS: &[&str] = &["sdmc:/flashnx", "sdmc:/ruffle"];

fn find_user_path(suffix: &str) -> Option<std::string::String> {
    for root in USER_SD_ROOTS {
        let p = std::format!("{}/{}", root, suffix);
        if std::path::Path::new(&p).exists() {
            return Some(p);
        }
    }
    None
}

fn primary_path(suffix: &str) -> std::string::String {
    std::format!("{}/{}", USER_SD_ROOTS[0], suffix)
}

/// Read a small JSON file using chunked 4 KB reads — same workaround as
/// `SwitchStorageBackend::get`. `std::fs::read` on Horizon newlib returns
/// `ENOMEM` once the buffer hits ~32 KB; keymap files are far below that
/// threshold today but we share the safe path defensively.
fn read_json_file(path: &str) -> Option<std::string::String> {
    let mut file = File::open(path).ok()?;
    let mut data = std::vec::Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        match file.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => data.extend_from_slice(&buf[..n]),
            Err(_) => return None,
        }
    }
    std::string::String::from_utf8(data).ok()
}

fn parse_keymap(json: &str, source: &str) -> Option<Keymap> {
    match serde_json::from_str::<Keymap>(json) {
        Ok(mut km) => {
            // Strip reserved buttons silently — they'd break the runtime
            // contract if honoured.
            for btn in RESERVED_BUTTONS {
                if km.bindings.remove(*btn).is_some() {
                    log(&std::format!(
                        "keymap: ignoring reserved button '{}' in {}\n",
                        btn, source,
                    ));
                }
            }
            // Upgrade pre-v1.4.0 French mouse-click identifiers to English.
            migrate_legacy_key_names(&mut km);
            Some(km)
        }
        Err(e) => {
            log(&std::format!(
                "keymap: failed to parse {} ({}), skipping\n",
                source, e,
            ));
            None
        }
    }
}

fn write_default_to_sd(path: &str, keymap: &Keymap) {
    // Pretty-printed so a user opening it in Notepad sees one binding per
    // line — easy to edit without breaking comma placement.
    let json = match serde_json::to_string_pretty(keymap) {
        Ok(s) => s,
        Err(e) => {
            log(&std::format!("keymap: serialize default failed: {}\n", e));
            return;
        }
    };
    match File::create(path) {
        Ok(mut f) => {
            if let Err(e) = f.write_all(json.as_bytes()) {
                log(&std::format!(
                    "keymap: write default to {} failed: {}\n",
                    path, e,
                ));
            } else {
                crate::sd::commit();
                log(&std::format!(
                    "keymap: wrote default to {} ({} bytes) — user can now edit it\n",
                    path,
                    json.len(),
                ));
            }
        }
        Err(e) => {
            log(&std::format!(
                "keymap: create {} failed: {} (SD card readonly?)\n",
                path, e,
            ));
        }
    }
}

/// Initialise `ACTIVE_KEYMAP` for the given SWF basename (e.g.
/// "Super_Mario_63_2010.swf"). Called from `ruffle_init` and from the
/// library's pre-launch OPTIONS > TOUCHES path. Idempotent **for the same
/// basename** — REDEMARRER keeps the same keymap, but back-to-library +
/// pick-different-game reloads the per-game sidecar so the user doesn't
/// inherit the previous game's bindings.
pub fn init_for_swf(swf_basename: &str) {
    // Skip reloading only if it's the same game AND the keymap is still loaded.
    // `apply_keymap` / `revert_profile` clear ACTIVE_KEYMAP but keep the
    // basename; without the keymap check we'd short-circuit here and the TOUCHES
    // editor would read a None keymap = empty bindings (in-game vs library
    // mismatch after applying a community profile, #20).
    let same_game = ACTIVE_BASENAME
        .lock()
        .map(|g| g.as_deref() == Some(swf_basename))
        .unwrap_or(false);
    let keymap_loaded = ACTIVE_KEYMAP.lock().map(|k| k.is_some()).unwrap_or(false);
    if same_game && keymap_loaded {
        return;
    }
    // Lookup order: new `sdmc:/flashnx/` first, then legacy
    // `sdmc:/ruffle/`. Writes (save_sidecar, default bootstrap) go to
    // the primary `flashnx/` location.
    let sidecar_name = std::format!("{}.keymap.json", swf_basename);
    let sidecar = find_user_path(&sidecar_name);
    let default = find_user_path("keymap_default.json");
    let default_write = primary_path("keymap_default.json");

    let mut km = if let Some(txt) = sidecar.as_deref().and_then(read_json_file) {
        let path_str = sidecar.as_deref().unwrap_or("?");
        log(std::format!("keymap: using per-game sidecar {}\n", path_str));
        parse_keymap(&txt, path_str).unwrap_or_else(|| {
            log("keymap: sidecar invalid, falling back to default\n");
            try_default_or_fallback(default.as_deref())
        })
    } else if let Some(txt) = default.as_deref().and_then(read_json_file) {
        let path_str = default.as_deref().unwrap_or("?");
        log(std::format!("keymap: using global default {}\n", path_str));
        parse_keymap(&txt, path_str).unwrap_or_else(|| {
            log("keymap: default invalid, falling back to hardcoded\n");
            fallback_keymap()
        })
    } else {
        // No JSON on SD at all — write the hardcoded fallback to the new
        // default path so the user discovers the schema in their flashnx
        // dir on next reboot / SD inspection. Only on first ever boot.
        log("keymap: no JSON on SD, bootstrapping global default + using hardcoded fallback\n");
        let km = fallback_keymap();
        write_default_to_sd(&default_write, &km);
        km
    };

    // Backfill defaults for buttons this keymap predates (e.g. ZR/ZL clicks on a
    // sidecar saved before they were editable), so the editor and behaviour agree.
    merge_fallback_defaults(&mut km);
    merge_fallback_defaults_p2(&mut km); // issue #40: ensure P2 has working defaults

    if let Ok(mut g) = ACTIVE_BASENAME.lock() {
        *g = Some(swf_basename.into());
    }
    if let Ok(mut g) = ACTIVE_KEYMAP.lock() {
        *g = Some(km);
    }
}

/// Load the GLOBAL DEFAULT keymap (`keymap_default.json`, or the hardcoded
/// fallback bootstrapped to SD) into `ACTIVE_KEYMAP` for editing via the
/// reused TOUCHES editor. Sets the active basename to `GLOBAL_SENTINEL` so
/// subsequent `set_binding` / `save_sidecar` calls persist to
/// `keymap_default.json` instead of a per-game sidecar. Called from the
/// library Settings modal (Plus → DEFAULT CONTROLS).
pub fn init_for_global_default() {
    let default = find_user_path("keymap_default.json");
    let mut km = if let Some(txt) = default.as_deref().and_then(read_json_file) {
        let path_str = default.as_deref().unwrap_or("?");
        log(std::format!("keymap: editing global default {}\n", path_str));
        parse_keymap(&txt, path_str).unwrap_or_else(fallback_keymap)
    } else {
        log("keymap: no global default on SD, bootstrapping from fallback\n");
        let km = fallback_keymap();
        write_default_to_sd(&primary_path("keymap_default.json"), &km);
        km
    };
    merge_fallback_defaults(&mut km);
    merge_fallback_defaults_p2(&mut km); // issue #40: ensure P2 has working defaults
    if let Ok(mut g) = ACTIVE_BASENAME.lock() {
        *g = Some(GLOBAL_SENTINEL.into());
    }
    if let Ok(mut g) = ACTIVE_KEYMAP.lock() {
        *g = Some(km);
    }
}

/// Clear the active keymap so the next `init_for_swf` re-reads from SD.
/// Called by `ruffle_library_reset` when the user quits a game back to
/// the library — the next pick may be a different game with a different
/// per-game sidecar, so we drop the current one to force a fresh load.
pub fn reset() {
    if let Ok(mut g) = ACTIVE_KEYMAP.lock() {
        *g = None;
    }
    if let Ok(mut g) = ACTIVE_BASENAME.lock() {
        *g = None;
    }
    set_edit_player(1);
}

/// Current binding for `button` (e.g. "A"), or `None` if unbound. Caller
/// gets an owned String to avoid holding the Mutex across UI work.
pub fn current_binding(button: &str) -> Option<std::string::String> {
    let g = ACTIVE_KEYMAP.lock().ok()?;
    let km = g.as_ref()?;
    let map = if edit_player() == 2 { &km.bindings_p2 } else { &km.bindings };
    map.get(button)
        .filter(|v| !v.is_empty()) // "" = explicitly unbound (see set_binding)
        .cloned()
}

/// Set `button` → `flash_key` (e.g. "A" → "Space"). `None` clears the
/// binding. Triggers a write to the per-game sidecar so the change persists
/// across reboots. Returns false on write failure (in-memory change still
/// applied — caller can retry / surface error).
pub fn set_binding(button: &str, flash_key: Option<&str>) -> bool {
    {
        let mut g = match ACTIVE_KEYMAP.lock() {
            Ok(g) => g,
            Err(_) => return false,
        };
        let Some(km) = g.as_mut() else { return false };
        let map = if edit_player() == 2 { &mut km.bindings_p2 } else { &mut km.bindings };
        match flash_key {
            Some(k) => {
                map.insert(button.into(), k.into());
            }
            None => {
                // Store an explicit empty marker instead of removing the key, so
                // a deliberate unbind survives the default-merge on the next load
                // (an ABSENT button gets its fallback default; an empty one stays
                // off). See merge_fallback_defaults.
                map.insert(button.into(), std::string::String::new());
            }
        }
        // The user just edited by hand → mark the keymap as user-authored so a
        // later community profile (#20) asks before overwriting it.
        km.source = "user".into();
    }
    save_sidecar()
}

// ── Community profiles (issue #20): provenance-aware apply / revert ──────────
// These operate on a per-game sidecar FILE directly (by basename), not on the
// active in-memory keymap — they're driven from the library OPTIONS list before
// a game launches, so the file is what the next `init_for_swf` will read.

fn keymap_read_path(basename: &str) -> Option<std::string::String> {
    find_user_path(&std::format!("{}.keymap.json", basename))
}
fn keymap_write_path(basename: &str) -> std::string::String {
    primary_path(&std::format!("{}.keymap.json", basename))
}
fn keymap_backup_path(basename: &str) -> std::string::String {
    primary_path(&std::format!("{}.keymap.bak.json", basename))
}

fn read_sidecar_file(basename: &str) -> Option<Keymap> {
    let txt = read_json_file(&keymap_read_path(basename)?)?;
    serde_json::from_str::<Keymap>(&txt).ok()
}

fn write_keymap_file(path: &str, km: &Keymap) -> bool {
    let json = match serde_json::to_string_pretty(km) {
        Ok(s) => s,
        Err(e) => {
            log(std::format!("keymap: serialize for apply failed: {}\n", e));
            return false;
        }
    };
    match File::create(path) {
        Ok(mut f) => {
            let ok = f.write_all(json.as_bytes()).is_ok();
            if ok {
                crate::sd::commit();
            }
            ok
        }
        Err(e) => {
            log(std::format!("keymap: create {} failed: {}\n", path, e));
            false
        }
    }
}

/// Tag `basename`'s keymap as catalog profile `id` (source = "community:<id>")
/// after the user SHARES their own controls. Same tag as an APPLIED profile,
/// because the meaning is the same: "these controls ARE catalog profile <id>".
/// So the picker marks it active and re-sharing the unchanged keymap is blocked.
/// Editing a key flips the source back to "user" (shareable again). Returns false
/// on write failure.
pub fn mark_shared(basename: &str, id: &str) -> bool {
    let mut km = effective_for(basename);
    km.source = std::format!("community:{}", id);
    write_keymap_file(&keymap_write_path(basename), &km)
}

/// Inverse of `mark_shared`/applied: when the catalog profile `id` is DELETED by
/// its owner (#20), demote `basename`'s keymap back to "user" IF it was still
/// tagged `community:<id>` — the controls are unchanged, just no longer published,
/// so they become shareable again (and the "already in the catalog" share block
/// lifts). No-op when the keymap points at a different profile or isn't tagged.
/// Returns false on write failure.
pub fn unmark_shared(basename: &str, id: &str) -> bool {
    let mut km = effective_for(basename);
    if km.source != std::format!("community:{}", id) {
        return true; // tagged for another profile (or already "user") → leave it
    }
    km.source = "user".into();
    write_keymap_file(&keymap_write_path(basename), &km)
}

/// Provenance of the on-disk sidecar for `basename` (see `Keymap::source`).
/// "default" when there is no sidecar (the game runs on the fallback).
// Wired by the library OPTIONS profile UI (#20, next increment).
#[allow(dead_code)]
pub fn provenance(basename: &str) -> std::string::String {
    match read_sidecar_file(basename) {
        Some(k) if !k.source.is_empty() => k.source,
        // A sidecar with no source tag is a pre-v1.4.0 file; only hand edits
        // ever wrote a per-game sidecar, so assume "user" and protect it.
        Some(_) => "user".into(),
        None => "default".into(),
    }
}

/// True when a `revert_profile` would restore a hand-made keymap (a backup
/// exists from a previous `apply_keymap` over user-authored controls).
#[allow(dead_code)]
pub fn has_backup(basename: &str) -> bool {
    std::path::Path::new(&keymap_backup_path(basename)).exists()
}

/// Apply `km` as the sidecar for `basename`. NON-DESTRUCTIVE: if the existing
/// sidecar was hand-authored ("user"), it's copied to `<basename>.keymap.bak.json`
/// first so `revert_profile` can bring it back. `km.source` should already say
/// where it came from (e.g. "community:<id>"). Returns false on write failure.
pub fn apply_keymap(basename: &str, km: &Keymap) -> bool {
    let existing = read_sidecar_file(basename);
    if let Some(ref ex) = existing {
        // Back up anything that isn't already a community profile — i.e. real
        // user work, including legacy untagged sidecars (source == "").
        if !ex.source.starts_with("community:") {
            if let Some(src) = keymap_read_path(basename) {
                // Best-effort backup; an unwritable SD still lets the apply go
                // through (the user simply won't have a one-tap revert).
                let _ = std::fs::copy(&src, keymap_backup_path(basename));
            }
        }
    }
    // (Cursor speed lives in its own per-game `.cursor` file, not the keymap.)
    let ok = write_keymap_file(&keymap_write_path(basename), km);
    // If this game's keymap is the one currently loaded in memory, drop it so a
    // re-entry reloads the freshly written file instead of the stale map.
    if ok {
        if let Ok(g) = ACTIVE_BASENAME.lock() {
            if g.as_deref() == Some(basename) {
                if let Ok(mut k) = ACTIVE_KEYMAP.lock() {
                    *k = None;
                }
            }
        }
    }
    ok
}

/// Undo an applied profile for `basename`: restore the hand-made backup if one
/// exists, else remove the sidecar so the game falls back to the defaults.
#[allow(dead_code)]
pub fn revert_profile(basename: &str) -> bool {
    let dst = keymap_write_path(basename);
    let bak = keymap_backup_path(basename);
    let ok = if std::path::Path::new(&bak).exists() {
        // Restore the backup, then drop it.
        let restored = std::fs::copy(&bak, &dst).is_ok();
        if restored {
            let _ = std::fs::remove_file(&bak);
        }
        restored
    } else {
        // No hand-made keymap to restore → revert to the fallback default.
        let _ = std::fs::remove_file(&dst);
        true
    };
    if ok {
        crate::sd::commit();
        if let Ok(g) = ACTIVE_BASENAME.lock() {
            if g.as_deref() == Some(basename) {
                if let Ok(mut k) = ACTIVE_KEYMAP.lock() {
                    *k = None;
                }
            }
        }
    }
    ok
}

/// Persist the active keymap to `sdmc:/ruffle/<basename>.keymap.json`. Auto
/// called by `set_binding`; also callable directly. Returns true on success.
pub fn save_sidecar() -> bool {
    let basename = match ACTIVE_BASENAME.lock() {
        Ok(g) => match g.as_ref() {
            Some(b) => b.clone(),
            None => return false,
        },
        Err(_) => return false,
    };
    let km = match ACTIVE_KEYMAP.lock() {
        Ok(g) => match g.as_ref() {
            Some(k) => k.clone(),
            None => return false,
        },
        Err(_) => return false,
    };
    // The global-default editor writes keymap_default.json; per-game
    // editing writes the basename sidecar.
    let path = if basename == GLOBAL_SENTINEL {
        primary_path("keymap_default.json")
    } else {
        primary_path(&std::format!("{}.keymap.json", basename))
    };
    let json = match serde_json::to_string_pretty(&km) {
        Ok(s) => s,
        Err(e) => {
            log(std::format!("keymap: serialize for save failed: {}\n", e));
            return false;
        }
    };
    match File::create(&path) {
        Ok(mut f) => match f.write_all(json.as_bytes()) {
            Ok(_) => {
                crate::sd::commit();
                log(std::format!(
                    "keymap: saved sidecar {} ({} bytes)\n",
                    path,
                    json.len(),
                ));
                true
            }
            Err(e) => {
                log(std::format!("keymap: write to {} failed: {}\n", path, e));
                false
            }
        },
        Err(e) => {
            log(std::format!("keymap: create {} failed: {}\n", path, e));
            false
        }
    }
}

/// Resolve the EFFECTIVE keymap for `basename` exactly as the game would load
/// it (per-game sidecar → global default → hardcoded fallback, defaults merged),
/// WITHOUT touching the active in-memory keymap. Used to snapshot a game's
/// current controls for sharing as a community profile (#20).
pub fn effective_for(basename: &str) -> Keymap {
    let sidecar = find_user_path(&std::format!("{}.keymap.json", basename));
    let default = find_user_path("keymap_default.json");
    let mut km = if let Some(txt) = sidecar.as_deref().and_then(read_json_file) {
        parse_keymap(&txt, "share")
            .unwrap_or_else(|| try_default_or_fallback(default.as_deref()))
    } else if let Some(txt) = default.as_deref().and_then(read_json_file) {
        parse_keymap(&txt, "share").unwrap_or_else(fallback_keymap)
    } else {
        fallback_keymap()
    };
    merge_fallback_defaults(&mut km);
    merge_fallback_defaults_p2(&mut km);
    km
}

/// What `revert_profile` WOULD restore for `basename`, WITHOUT writing anything
/// — used to preview the before/after of a revert. Mirrors `revert_profile`:
/// the hand-made backup if one exists, else the global default (then fallback).
pub fn revert_target(basename: &str) -> Keymap {
    let bak = keymap_backup_path(basename);
    let default = find_user_path("keymap_default.json");
    let mut km = if std::path::Path::new(&bak).exists() {
        read_json_file(&bak)
            .and_then(|t| parse_keymap(&t, "share"))
            .unwrap_or_else(|| try_default_or_fallback(default.as_deref()))
    } else {
        try_default_or_fallback(default.as_deref())
    };
    merge_fallback_defaults(&mut km);
    merge_fallback_defaults_p2(&mut km);
    km
}

/// Whether a revert would actually CHANGE this game's controls — i.e. the
/// effective keymap differs (P1 or P2) from what `revert_target` would restore
/// (the hand-made backup, else the global default). Drives whether the "Revenir"
/// row appears: CONTENT-based, so it shows exactly when there's something to
/// undo and HIDES once the controls match the target again (edit a key, then set
/// it back). Replaces the old `provenance != "default"` test, which only checked
/// that a sidecar FILE existed — it stayed on after you reverted a change by hand.
pub fn has_revert(basename: &str) -> bool {
    let cur = effective_for(basename);
    let tgt = revert_target(basename);
    cur.bindings != tgt.bindings || cur.bindings_p2 != tgt.bindings_p2
}

/// Build "<button>: <current> -> <target>" diff lines for the keys that differ
/// between two keymaps, across BOTH players (#40). P1 rows are unprefixed (the
/// common single-player case stays clean); P2 rows carry a "P2 " tag so a
/// two-player change is attributed to the right pad. The single source of truth
/// for every profile/revert/share preview (library + in-game) — diffing only P1
/// here was the bug where editing a P2 key showed "no changes" yet still dropped
/// the active tag.
pub fn binding_diff_rows(
    cur_p1: &std::collections::BTreeMap<std::string::String, std::string::String>,
    tgt_p1: &std::collections::BTreeMap<std::string::String, std::string::String>,
    cur_p2: &std::collections::BTreeMap<std::string::String, std::string::String>,
    tgt_p2: &std::collections::BTreeMap<std::string::String, std::string::String>,
) -> std::vec::Vec<std::string::String> {
    let none = crate::loc::s().none;
    let disp = |k: &str| -> std::string::String {
        if k.is_empty() {
            none.to_string()
        } else {
            flash_key_display(k).into_owned()
        }
    };
    let mut rows = std::vec::Vec::new();
    for (label, cur, tgt) in [("", cur_p1, tgt_p1), ("P2 ", cur_p2, tgt_p2)] {
        for btn in EDITABLE_BUTTONS {
            let c = cur.get(*btn).map(std::string::String::as_str).unwrap_or("");
            let n = tgt.get(*btn).map(std::string::String::as_str).unwrap_or("");
            if c != n {
                rows.push(std::format!("{}{}: {} -> {}", label, btn, disp(c), disp(n)));
            }
        }
    }
    rows
}

/// The active game's basename, or None when no game is active / the global
/// default is being edited. Cursor speed is per-GAME only.
pub(crate) fn active_game_basename() -> Option<std::string::String> {
    let g = ACTIVE_BASENAME.lock().ok()?;
    match g.as_ref() {
        Some(b) if b.as_str() != GLOBAL_SENTINEL => Some(b.clone()),
        _ => None,
    }
}

/// Per-game cursor-speed preset for the active game, or -1 if unset. Stored in
/// its OWN tiny `<basename>.cursor` file (NOT the keymap) so changing pointer
/// speed never rewrites or snapshots the key bindings. Read by C++ at launch.
pub fn cursor_speed() -> i32 {
    match active_game_basename() {
        Some(b) => cursor_speed_for(&b),
        None => -1,
    }
}

/// Per-game cursor-speed preset for an ARBITRARY game (by basename), or -1 if
/// unset. Lets the library OPTIONS > TOUCHES sub-menu read a game's speed without
/// that game being the active one (#20). Same `<basename>.cursor` file.
pub fn cursor_speed_for(basename: &str) -> i32 {
    find_user_path(&std::format!("{}.cursor", basename))
        .and_then(|p| read_json_file(&p))
        .and_then(|s| s.trim().parse::<i32>().ok())
        .unwrap_or(-1)
}

/// Persist a cursor-speed preset for an ARBITRARY game (by basename). `idx < 0`
/// clears the per-game file. Used by the library sub-menu (the in-game VITESSE
/// uses `set_cursor_speed`, which targets the active game).
pub fn set_cursor_speed_for(basename: &str, idx: i32) {
    let path = primary_path(&std::format!("{}.cursor", basename));
    if idx < 0 {
        let _ = std::fs::remove_file(&path);
    } else if std::fs::write(&path, std::format!("{}", idx).as_bytes()).is_err() {
        return;
    }
    crate::sd::commit();
}

/// Persist the active GAME's per-game cursor-speed preset to `<basename>.cursor`.
/// Called (only) from the in-game VITESSE cycle — C++ handles the library /
/// RÉGLAGES global default separately. `idx < 0` clears the per-game file.
pub fn set_cursor_speed(idx: i32) {
    let Some(basename) = active_game_basename() else {
        return;
    };
    let path = primary_path(&std::format!("{}.cursor", basename));
    if idx < 0 {
        let _ = std::fs::remove_file(&path);
    } else if std::fs::write(&path, std::format!("{}", idx).as_bytes()).is_err() {
        return;
    }
    crate::sd::commit();
}

fn try_default_or_fallback(default_path: Option<&str>) -> Keymap {
    let Some(default_path) = default_path else { return fallback_keymap(); };
    if let Some(txt) = read_json_file(default_path) {
        parse_keymap(&txt, default_path).unwrap_or_else(fallback_keymap)
    } else {
        fallback_keymap()
    }
}

/// Look up the Flash key bound to `button_name` (e.g. "A", "StickLLeft").
/// Returns `None` if no binding exists (button is unmapped). Returns the
/// SK_* code from `crate::sk_*` if a binding exists and the target key is
/// recognised. Returns `Some(SK_NONE)` if the JSON binds to a key we don't
/// support yet — caller treats that as "ignored".
pub fn lookup(button_name: &str) -> Option<core::ffi::c_int> {
    let g = ACTIVE_KEYMAP.lock().ok()?;
    let km = g.as_ref()?;
    let key_name = km.bindings.get(button_name)?;
    if key_name.is_empty() {
        return Some(crate::SK_NONE); // "" = explicitly unbound
    }
    Some(flash_key_name_to_sk(key_name))
}

// ─── Player 2 keymap (issue #40, local 2-player) ────────────────────────────
// Flash "2-player" is just two key-sets on one keyboard, so a SECOND controller
// feeds the SAME `ruffle_handle_key` pipeline through a separate set of bindings.
// These live in the SAME keymap file as P1, under `bindings_p2` (one keymap per
// game), and are edited via the TOUCHES editor's P1/P2 toggle (X). Controller 2
// idle / absent = no input, so single-player is unaffected.
//
// Default movement = WASD, NOT arrows: P1's default movement is arrows, so
// P1 (arrows) + P2 (WASD) don't collide — the standard 2-player co-op layout
// (e.g. Fireboy & Watergirl: Fireboy = arrows, Watergirl = WASD). For games
// where P2 uses arrows instead (e.g. DBZ Devolution P2 = arrows + 1-6), switch
// the editor to JOUEUR 2 (X) and remap.
pub const FALLBACK_BINDINGS_P2: &[(&str, &str)] = &[
    ("Left", "A"),
    ("Right", "D"),
    ("Up", "W"),
    ("Down", "S"),
    ("StickLLeft", "A"),
    ("StickLRight", "D"),
    ("StickLUp", "W"),
    ("StickLDown", "S"),
    // Action buttons -> NUMPAD digits: the actual P2 convention in most 2-player
    // Flash fighters (Super Smash Flash 2 P2 = Numpad 1-4, KOF Wing, etc.) — the
    // top-row "1".."6" we used before produced the wrong key codes for them.
    // Games that want the top row instead are remapped per-game / via a profile.
    ("A", "Num1"),
    ("B", "Num2"),
    ("X", "Num3"),
    ("Y", "Num4"),
    ("R", "Num5"),
    ("L", "Num6"),
];

fn p2_default_bindings() -> BTreeMap<std::string::String, std::string::String> {
    let mut m = BTreeMap::new();
    for (btn, key) in FALLBACK_BINDINGS_P2 {
        m.insert((*btn).into(), (*key).into());
    }
    m
}

/// Backfill P2 defaults for any P2 button the loaded keymap doesn't mention, so
/// pre-#40 files (no `bindings_p2`) and partial maps still get a working P2.
fn merge_fallback_defaults_p2(km: &mut Keymap) {
    for (btn, key) in FALLBACK_BINDINGS_P2 {
        km.bindings_p2
            .entry((*btn).into())
            .or_insert_with(|| (*key).into());
    }
}

/// Which player the TOUCHES editor is currently editing (1 or 2). In-game input
/// is always resolved per-player (`lookup` = P1, `lookup_p2` = P2); this only
/// steers the editor's `current_binding` / `set_binding` to the right map.
static EDIT_PLAYER: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(1);

pub fn edit_player() -> u8 {
    EDIT_PLAYER.load(std::sync::atomic::Ordering::Relaxed)
}

pub fn set_edit_player(player: u8) {
    EDIT_PLAYER.store(
        if player == 2 { 2 } else { 1 },
        std::sync::atomic::Ordering::Relaxed,
    );
}

/// Player-2 equivalent of [`lookup`]: resolve a controller-2 button to its Flash
/// `SK_*` code via the `bindings_p2` map of the active keymap.
pub fn lookup_p2(button_name: &str) -> Option<core::ffi::c_int> {
    let g = ACTIVE_KEYMAP.lock().ok()?;
    let km = g.as_ref()?;
    let key_name = km.bindings_p2.get(button_name)?;
    if key_name.is_empty() {
        return Some(crate::SK_NONE);
    }
    Some(flash_key_name_to_sk(key_name))
}

/// Localised DISPLAY label for a Flash-key NAME. The stored/internal names stay
/// language-stable (so a keymap saved in one language still resolves in another);
/// only the label shown in the TOUCHES editor is translated. The unbind sentinel
/// and the mouse-click pseudo-keys are action labels (translated); plain key
/// names (Space, A-Z, digits) are universal and returned as-is.
pub fn flash_key_display(name: &str) -> std::borrow::Cow<'_, str> {
    use std::borrow::Cow;
    let lc = crate::loc::s();
    let translated: &'static str = match name {
        "(none)" => lc.none,
        "Left click" => lc.flash_mouse_left,
        "Right click" => lc.flash_mouse_right,
        // Back-compat: pre-v1.4.0 keymaps stored the clicks in French. Kept so a
        // hand-edited / not-yet-migrated value still gets a translated label.
        "Clic gauche" => lc.flash_mouse_left,
        "Clic droit" => lc.flash_mouse_right,
        "Space" => lc.flash_space,
        "Enter" => lc.flash_enter,
        "Escape" => lc.flash_escape,
        "Shift" => lc.flash_shift,
        "Control" => lc.flash_control,
        "Alt" => lc.flash_alt,
        "Tab" => lc.flash_tab,
        "Backspace" => lc.flash_backspace,
        "Up" => lc.flash_up,
        "Down" => lc.flash_down,
        "Left" => lc.flash_left,
        "Right" => lc.flash_right,
        // Letters (A-Z) and digits (0-9) are universal — shown as typed.
        other => return Cow::Borrowed(other),
    };
    Cow::Borrowed(translated)
}

/// Map a Flash key NAME (as written in JSON, e.g. "Space", "Z") to one of
/// our `SK_*` integer constants. Add new entries here as we expand keyboard
/// support. Unknown names log a warning and return `SK_NONE`.
fn flash_key_name_to_sk(name: &str) -> core::ffi::c_int {
    match name {
        "Space"     => crate::SK_SPACE,
        "Enter"     => crate::SK_ENTER,
        "Escape"    => crate::SK_ESCAPE,
        "Shift"     => crate::SK_SHIFT,
        "Control"   => crate::SK_CONTROL,
        "Alt"       => crate::SK_ALT,
        "Tab"       => crate::SK_TAB,
        "Backspace" => crate::SK_BACKSPACE,
        "Left"      => crate::SK_LEFT,
        "Right"     => crate::SK_RIGHT,
        "Up"        => crate::SK_UP,
        "Down"      => crate::SK_DOWN,
        // A-Z.
        "A" => crate::SK_A, "B" => crate::SK_B, "C" => crate::SK_C,
        "D" => crate::SK_D, "E" => crate::SK_E, "F" => crate::SK_F,
        "G" => crate::SK_G, "H" => crate::SK_H, "I" => crate::SK_I,
        "J" => crate::SK_J, "K" => crate::SK_K, "L" => crate::SK_L,
        "M" => crate::SK_M, "N" => crate::SK_N, "O" => crate::SK_O,
        "P" => crate::SK_P, "Q" => crate::SK_Q, "R" => crate::SK_R,
        "S" => crate::SK_S, "T" => crate::SK_T, "U" => crate::SK_U,
        "V" => crate::SK_V, "W" => crate::SK_W, "X" => crate::SK_X,
        "Y" => crate::SK_Y, "Z" => crate::SK_Z,
        // 0-9.
        "0" => crate::SK_0, "1" => crate::SK_1, "2" => crate::SK_2,
        "3" => crate::SK_3, "4" => crate::SK_4, "5" => crate::SK_5,
        "6" => crate::SK_6, "7" => crate::SK_7, "8" => crate::SK_8,
        "9" => crate::SK_9,
        // Numpad digits (distinct keycodes from the top row). PR #46 (YuQiyang).
        "Num0" => crate::SK_NUMPAD0, "Num1" => crate::SK_NUMPAD1,
        "Num2" => crate::SK_NUMPAD2, "Num3" => crate::SK_NUMPAD3,
        "Num4" => crate::SK_NUMPAD4, "Num5" => crate::SK_NUMPAD5,
        "Num6" => crate::SK_NUMPAD6, "Num7" => crate::SK_NUMPAD7,
        "Num8" => crate::SK_NUMPAD8, "Num9" => crate::SK_NUMPAD9,
        // Mouse-click pseudo-keys (routed to the mouse, not the keyboard).
        "Left click" => crate::SK_MOUSE_LEFT,
        "Right click" => crate::SK_MOUSE_RIGHT,
        // Back-compat: pre-v1.4.0 keymaps stored these in French. Accepted so
        // existing files keep resolving even before they're rewritten in English.
        "Clic gauche" => crate::SK_MOUSE_LEFT,
        "Clic droit" => crate::SK_MOUSE_RIGHT,
        other => {
            log(std::format!(
                "keymap: unknown Flash key '{}' in bindings — ignored\n",
                other,
            ));
            crate::SK_NONE
        }
    }
}

// ── Logging helper ────────────────────────────────────────────────────────
// Shadow of crate::log/log_str so this module is self-contained for the
// `b"..."` byte-string case while still routing through the same C
// `ruffle_log_cstr` sink.

extern "C" {
    fn ruffle_log_cstr(msg: *const core::ffi::c_char);
}

trait LogArg {
    fn emit(self);
}

impl LogArg for &str {
    fn emit(self) {
        let mut bytes = self.as_bytes().to_vec();
        bytes.push(0);
        unsafe { ruffle_log_cstr(bytes.as_ptr() as *const _) };
    }
}

impl LogArg for &std::string::String {
    fn emit(self) {
        self.as_str().emit();
    }
}

impl LogArg for std::string::String {
    fn emit(self) {
        self.as_str().emit();
    }
}

impl LogArg for &[u8] {
    fn emit(self) {
        // Always copy + append NUL — relying on the caller to include `\0`
        // in their byte literal is a footgun (forgot one → ruffle_log_cstr
        // reads past the buffer into adjacent .rodata until it finds a
        // stray NUL, dumping unrelated strings into stdout — caught
        // 2026-05-25 nuit during first hardware test of this module).
        let mut v = self.to_vec();
        v.push(0);
        unsafe { ruffle_log_cstr(v.as_ptr() as *const _) };
    }
}

// b"..." literals have type &[u8; N] — coerce to slice so we don't force
// the caller to write `&b"..."[..]` at each site.
impl<const N: usize> LogArg for &[u8; N] {
    fn emit(self) {
        let s: &[u8] = self;
        s.emit();
    }
}

fn log<T: LogArg>(msg: T) {
    msg.emit();
}
