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
    /// Player 1 "combo layer" bindings (issue #57): the key each button sends
    /// while the `combo_modifier` button is HELD. A mode-shift second layer (one
    /// layer, like a keyboard Fn key) that ~doubles the reachable keys for games
    /// with more inputs than the pad has buttons. `#[serde(default)]` keeps
    /// pre-#57 keymaps loading (absent = empty = no combos). A button with NO
    /// entry here falls through to its base binding while the modifier is held,
    /// so holding the modifier never breaks movement — only remapped buttons change.
    /// Combo layers (issue #57), ONE PER MODIFIER button. Outer key is the held
    /// modifier ("ZL"/"ZR"/"L"/"R"); inner map is that modifier's button→key
    /// bindings. A modifier with a non-empty layer is ACTIVE in-game: its button
    /// becomes a modifier (its own base key muted), and holding it makes every
    /// other button send that layer's key (falling through to the base key for
    /// buttons the layer doesn't map). All four modifiers work independently, so
    /// `L+A` and `R+A` can differ. `#[serde(default)]` keeps older keymaps loading.
    #[serde(default)]
    pub combo_layers: BTreeMap<std::string::String, BTreeMap<std::string::String, std::string::String>>,
    /// Player 2 combo layers — the P2 counterpart of `combo_layers`.
    #[serde(default)]
    pub combo_layers_p2: BTreeMap<std::string::String, BTreeMap<std::string::String, std::string::String>>,
    // Legacy single-layer combo fields (pre per-modifier). READ for migration into
    // `combo_layers` on load, then never written (`skip_serializing`).
    #[serde(default, skip_serializing)]
    pub bindings_layer2: BTreeMap<std::string::String, std::string::String>,
    #[serde(default, skip_serializing)]
    pub bindings_layer2_p2: BTreeMap<std::string::String, std::string::String>,
    #[serde(default, skip_serializing)]
    pub combo_modifier: std::string::String,
    #[serde(default, skip_serializing)]
    pub combo_modifier_p2: std::string::String,
    /// Provenance of these bindings (issue #20, community control profiles), so
    /// the UI never clobbers hand-made controls silently:
    ///   "" / "default" = untouched fallback,
    ///   "user"         = edited by hand in the TOUCHES editor,
    ///   "community:<id>" = an applied shared profile.
    /// `#[serde(default)]` keeps pre-v1.4.0 keymaps loading (they read as "").
    #[serde(default)]
    pub source: std::string::String,
    /// Whether the on-screen mouse pointer sprite is drawn while THIS game runs
    /// (per-game "show cursor" toggle). When false, the C++ layer skips drawing
    /// the pointer, but clicks still fire (right-stick / ZR / touch keep working) —
    /// it only hides the visual, for pad/keyboard games where the pointer is just
    /// clutter. Rides IN the keymap (not a separate file like cursor SPEED) so it
    /// travels through share/apply/diff automatically. `default_show_cursor` keeps
    /// pre-toggle keymaps loading as SHOWN.
    #[serde(default = "default_show_cursor")]
    pub show_cursor: bool,
}

/// Default for `Keymap::show_cursor`: the pointer is SHOWN unless a game's keymap
/// explicitly turns it off. Also the value every fresh keymap starts with.
pub(crate) fn default_show_cursor() -> bool {
    true
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

/// Visual keyboard layout for the TOUCHES picker (issue #55), as POSITIONED keys:
/// `(name, row, x, w)` where `row` is the 0-based keyboard row and `x`/`w` are in
/// "key units" (1 unit ≈ one letter key). This lets us lay out a real PC keyboard
/// — staggered rows, a wide space bar, and the NUMPAD as a block on the RIGHT
/// (rows 1-4, x≥16) instead of dumped at the bottom. The renderer draws each key
/// at its slot; `menu.rs` navigates geometrically (nearest key in the pressed
/// direction). Every `name` MUST be resolvable by `flash_key_name_to_sk` (or be
/// the "(none)" unbind sentinel). QWERTY is fixed for every language: Flash reads
/// US keyCodes and CJK boards are physically QWERTY too (see the #55 discussion).
/// Labels are localised at draw time via `keyboard_label`; stored names are stable.
pub const KEYBOARD: &[(&str, u8, f32, f32)] = &[
    // Row 0 — Esc + function keys, grouped in fours like a real board.
    ("Escape", 0, 0.0, 1.0),
    ("F1", 0, 1.5, 1.0), ("F2", 0, 2.5, 1.0), ("F3", 0, 3.5, 1.0), ("F4", 0, 4.5, 1.0),
    ("F5", 0, 5.75, 1.0), ("F6", 0, 6.75, 1.0), ("F7", 0, 7.75, 1.0), ("F8", 0, 8.75, 1.0),
    ("F9", 0, 10.0, 1.0), ("F10", 0, 11.0, 1.0), ("F11", 0, 12.0, 1.0), ("F12", 0, 13.0, 1.0),
    // Row 1 — number row (+ Backspace), then numpad 7 8 9 /.
    ("`", 1, 0.0, 1.0), ("1", 1, 1.0, 1.0), ("2", 1, 2.0, 1.0), ("3", 1, 3.0, 1.0),
    ("4", 1, 4.0, 1.0), ("5", 1, 5.0, 1.0), ("6", 1, 6.0, 1.0), ("7", 1, 7.0, 1.0),
    ("8", 1, 8.0, 1.0), ("9", 1, 9.0, 1.0), ("0", 1, 10.0, 1.0), ("-", 1, 11.0, 1.0),
    ("=", 1, 12.0, 1.0), ("Backspace", 1, 13.0, 2.0),
    ("Num7", 1, 16.0, 1.0), ("Num8", 1, 17.0, 1.0), ("Num9", 1, 18.0, 1.0), ("Num/", 1, 19.0, 1.0),
    // Row 2 — QWERTY top row, then numpad 4 5 6 *.
    ("Tab", 2, 0.0, 1.5),
    ("Q", 2, 1.5, 1.0), ("W", 2, 2.5, 1.0), ("E", 2, 3.5, 1.0), ("R", 2, 4.5, 1.0),
    ("T", 2, 5.5, 1.0), ("Y", 2, 6.5, 1.0), ("U", 2, 7.5, 1.0), ("I", 2, 8.5, 1.0),
    ("O", 2, 9.5, 1.0), ("P", 2, 10.5, 1.0), ("[", 2, 11.5, 1.0), ("]", 2, 12.5, 1.0),
    ("\\", 2, 13.5, 1.5),
    ("Num4", 2, 16.0, 1.0), ("Num5", 2, 17.0, 1.0), ("Num6", 2, 18.0, 1.0), ("Num*", 2, 19.0, 1.0),
    // Row 3 — home row (CapsLock at its real home-row-left spot), then numpad 1 2 3 -.
    ("CapsLock", 3, 0.0, 1.75),
    ("A", 3, 1.75, 1.0), ("S", 3, 2.75, 1.0), ("D", 3, 3.75, 1.0), ("F", 3, 4.75, 1.0),
    ("G", 3, 5.75, 1.0), ("H", 3, 6.75, 1.0), ("J", 3, 7.75, 1.0), ("K", 3, 8.75, 1.0),
    ("L", 3, 9.75, 1.0), (";", 3, 10.75, 1.0), ("'", 3, 11.75, 1.0), ("Enter", 3, 12.75, 2.25),
    ("Num1", 3, 16.0, 1.0), ("Num2", 3, 17.0, 1.0), ("Num3", 3, 18.0, 1.0), ("Num-", 3, 19.0, 1.0),
    // Row 4 — shift row, then numpad 0 . Enter +.
    ("Shift", 4, 0.0, 2.25),
    ("Z", 4, 2.25, 1.0), ("X", 4, 3.25, 1.0), ("C", 4, 4.25, 1.0), ("V", 4, 5.25, 1.0),
    ("B", 4, 6.25, 1.0), ("N", 4, 7.25, 1.0), ("M", 4, 8.25, 1.0), (",", 4, 9.25, 1.0),
    (".", 4, 10.25, 1.0), ("/", 4, 11.25, 1.0),
    ("Num0", 4, 16.0, 1.0), ("Num.", 4, 17.0, 1.0), ("NumEnter", 4, 18.0, 1.0), ("Num+", 4, 19.0, 1.0),
    // Row 5 — Ctrl + Alt + space bar + arrow cluster (Ctrl moved here from the
    // home row now that CapsLock owns that slot; bottom-left is its real place).
    ("Control", 5, 0.0, 1.5), ("Alt", 5, 1.5, 1.5), ("Space", 5, 3.0, 5.0),
    ("Left", 5, 8.0, 1.0), ("Up", 5, 9.0, 1.0), ("Down", 5, 10.0, 1.0), ("Right", 5, 11.0, 1.0),
    // Unbind + mouse clicks + the keyboard — a horizontal row along the bottom,
    // under the numpad, so they sit "under the numbers" without the lop-sided
    // stacked look. KEYBOARD takes the free space to the left of (none), which
    // keeps the three that were already there exactly where players know them.
    // Ends at 7.8, not 8.0: the other three are spaced 0.2 apart, and a key that
    // touches its neighbour reads as one wide key with a line through it.
    ("Keyboard", 6, 3.7, 4.1),
    ("(none)", 6, 8.0, 3.4), ("Left click", 6, 11.6, 4.1), ("Right click", 6, 15.9, 4.1),
];

/// Total width of the keyboard in units (max `x + w`) — the renderer scales to it.
pub const KEYBOARD_UNITS_W: f32 = 20.0;
/// Number of keyboard rows (0-based rows 0..=6).
pub const KEYBOARD_ROWS_N: usize = 7;

/// Short DISPLAY label for a keyboard key on the visual picker. Keeps the board
/// compact (full names like "Backspace" would blow the key width) and localises
/// the action keys via `flash_key_display`. Plain glyph keys (letters, digits,
/// symbols) render as their own name.
pub fn keyboard_label(name: &str) -> std::borrow::Cow<'static, str> {
    use std::borrow::Cow;
    let lc = crate::loc::s(); // 'static — the translated action labels
    match name {
        "Escape" => Cow::Borrowed("Esc"),
        "Backspace" => Cow::Borrowed("Bksp"),
        "Control" => Cow::Borrowed("Ctrl"),
        "CapsLock" => Cow::Borrowed("Caps"),
        "Enter" => Cow::Borrowed("Ent"),
        // Numpad: compact "N" labels so the keys stay legible at cell size.
        "Num0" => Cow::Borrowed("N0"),
        "Num1" => Cow::Borrowed("N1"),
        "Num2" => Cow::Borrowed("N2"),
        "Num3" => Cow::Borrowed("N3"),
        "Num4" => Cow::Borrowed("N4"),
        "Num5" => Cow::Borrowed("N5"),
        "Num6" => Cow::Borrowed("N6"),
        "Num7" => Cow::Borrowed("N7"),
        "Num8" => Cow::Borrowed("N8"),
        "Num9" => Cow::Borrowed("N9"),
        "Num+" => Cow::Borrowed("N+"),
        "Num-" => Cow::Borrowed("N-"),
        "Num*" => Cow::Borrowed("N*"),
        "Num/" => Cow::Borrowed("N/"),
        "Num." => Cow::Borrowed("N."),
        "NumEnter" => Cow::Borrowed("NEnt"),
        // Action keys (none / mouse clicks) are localised; everything else — the
        // letters, digits, symbols, F-keys — shows verbatim.
        "(none)" => Cow::Borrowed(lc.none),
        "Left click" => Cow::Borrowed(lc.flash_mouse_left),
        "Right click" => Cow::Borrowed(lc.flash_mouse_right),
        "Keyboard" => Cow::Borrowed(lc.flash_keyboard),
        other => Cow::Owned(other.to_string()),
    }
}

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
        combo_layers: BTreeMap::new(), // combos start empty (opt-in per game)
        combo_layers_p2: BTreeMap::new(),
        bindings_layer2: BTreeMap::new(), // legacy (migration only)
        bindings_layer2_p2: BTreeMap::new(),
        combo_modifier: std::string::String::new(),
        combo_modifier_p2: std::string::String::new(),
        source: "default".into(),
        show_cursor: true,
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

/// Migrate the pre-per-modifier single combo layer (issue #57 v1) into the new
/// per-modifier `combo_layers` map: the old `bindings_layer2` becomes the layer of
/// the old `combo_modifier`. Clears the legacy fields so they don't get re-read.
/// No-op once migrated (legacy fields empty).
fn migrate_legacy_combo(km: &mut Keymap) {
    if !km.combo_modifier.is_empty() && !km.bindings_layer2.is_empty() {
        km.combo_layers
            .entry(std::mem::take(&mut km.combo_modifier))
            .or_insert_with(|| std::mem::take(&mut km.bindings_layer2));
    }
    if !km.combo_modifier_p2.is_empty() && !km.bindings_layer2_p2.is_empty() {
        km.combo_layers_p2
            .entry(std::mem::take(&mut km.combo_modifier_p2))
            .or_insert_with(|| std::mem::take(&mut km.bindings_layer2_p2));
    }
    // Drop any leftover legacy state so it never leaks back.
    km.bindings_layer2.clear();
    km.bindings_layer2_p2.clear();
    km.combo_modifier.clear();
    km.combo_modifier_p2.clear();
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
            // Upgrade the pre-per-modifier single combo layer (#57 v1).
            migrate_legacy_combo(&mut km);
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
    reset_edit_subtabs();
}

/// Current binding for `button` (e.g. "A"), or `None` if unbound. Caller
/// gets an owned String to avoid holding the Mutex across UI work.
pub fn current_binding(button: &str) -> Option<std::string::String> {
    let g = ACTIVE_KEYMAP.lock().ok()?;
    let km = g.as_ref()?;
    // The editor's active SUB-TAB decides the map: "" = base (P1/P2), else the
    // combo layer of that modifier (issue #57, per-modifier). `edit_subtab_*` are
    // atomics — no re-lock of ACTIVE_KEYMAP → no deadlock.
    let combo_mod = edit_subtab_modifier();
    let p2 = edit_player() == 2;
    let val = if combo_mod.is_empty() {
        let map = if p2 { &km.bindings_p2 } else { &km.bindings };
        map.get(button)
    } else {
        let layers = if p2 { &km.combo_layers_p2 } else { &km.combo_layers };
        layers.get(combo_mod).and_then(|m| m.get(button))
    };
    val.filter(|v| !v.is_empty()) // "" = explicitly unbound (see set_binding)
        .cloned()
}

/// The set of Flash-key NAMES already bound to SOME button in the map the editor
/// is currently on (player + sub-tab). The keyboard picker tints these so the user
/// sees a key is already in use elsewhere (they can still pick it).
pub fn current_map_used_keys() -> std::collections::BTreeSet<std::string::String> {
    let mut set = std::collections::BTreeSet::new();
    if let Ok(g) = ACTIVE_KEYMAP.lock() {
        if let Some(km) = g.as_ref() {
            let combo_mod = edit_subtab_modifier();
            let p2 = edit_player() == 2;
            let map = if combo_mod.is_empty() {
                if p2 { Some(&km.bindings_p2) } else { Some(&km.bindings) }
            } else {
                let layers = if p2 { &km.combo_layers_p2 } else { &km.combo_layers };
                layers.get(combo_mod)
            };
            if let Some(map) = map {
                for v in map.values() {
                    if !v.is_empty() {
                        set.insert(v.clone());
                    }
                }
            }
        }
    }
    set
}

/// Set `button` → `flash_key` (e.g. "A" → "Space"). `None` clears the
/// binding. Triggers a write to the per-game sidecar so the change persists
/// across reboots. Returns false on write failure (in-memory change still
/// applied — caller can retry / surface error).
pub fn set_binding(button: &str, flash_key: Option<&str>) -> bool {
    // What to put back if the write fails: (combo layer, previous value,
    // previous source). The in-memory keymap is what the TOUCHES list draws, so
    // leaving the new binding there after the file refused to change would show a
    // key the game does not have — indistinguishable from a successful rebind,
    // and gone at the next restart.
    let undo: Option<(std::string::String, Option<std::string::String>, std::string::String)>;
    {
        let mut g = match ACTIVE_KEYMAP.lock() {
            Ok(g) => g,
            Err(_) => return false,
        };
        let Some(km) = g.as_mut() else { return false };
        let combo_mod = edit_subtab_modifier();
        let p2 = edit_player() == 2;
        // Empty marker (not remove) so a deliberate unbind survives the next
        // default-merge (ABSENT button gets its fallback; empty stays off).
        let val = flash_key.map(std::string::String::from).unwrap_or_default();
        let prev_source = km.source.clone();
        if combo_mod.is_empty() {
            let map = if p2 { &mut km.bindings_p2 } else { &mut km.bindings };
            let prev = map.insert(button.into(), val);
            undo = Some((std::string::String::new(), prev, prev_source));
        } else {
            // Per-modifier combo layer (issue #57): create it on first bind.
            let layers = if p2 { &mut km.combo_layers_p2 } else { &mut km.combo_layers };
            let prev = layers
                .entry(combo_mod.to_string())
                .or_default()
                .insert(button.into(), val);
            undo = Some((combo_mod.to_string(), prev, prev_source));
        }
        // The user just edited by hand → mark the keymap as user-authored so a
        // later community profile (#20) asks before overwriting it.
        km.source = "user".into();
    }
    if save_sidecar() {
        return true;
    }
    // The card refused the write. Put the keymap back the way it was so what the
    // list shows is what the game will use.
    if let Some((layer, prev, prev_source)) = undo {
        if let Ok(mut g) = ACTIVE_KEYMAP.lock() {
            if let Some(km) = g.as_mut() {
                let p2 = edit_player() == 2;
                let map = if layer.is_empty() {
                    if p2 { &mut km.bindings_p2 } else { &mut km.bindings }
                } else if p2 {
                    km.combo_layers_p2.entry(layer.clone()).or_default()
                } else {
                    km.combo_layers.entry(layer.clone()).or_default()
                };
                match prev {
                    Some(v) => {
                        map.insert(button.into(), v);
                    }
                    None => {
                        map.remove(button);
                    }
                }
                km.source = prev_source;
            }
        }
    }
    log(&std::format!(
        "keymap: rebind of {} not saved - in-memory binding rolled back\n",
        button,
    ));
    false
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
    // Content, not existence. This one predicate decides whether the REVENIR row
    // appears AND what `revert_profile` copies back over the live sidecar, so a
    // backup that exists but holds nothing is worse than no backup at all: the
    // row invites the user to restore, and restoring writes the empty file over
    // the keymap they hand-made. `fs::copy` allocates fresh clusters while
    // `write_keymap_file` truncates and reuses them, so a nearly-full card fails
    // exactly here — the backup — while the apply it protects still succeeds.
    let path = keymap_backup_path(basename);
    read_json_file(&path).is_some_and(|txt| serde_json::from_str::<Keymap>(&txt).is_ok())
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
                // through (the user simply won't have a one-tap revert). Said out
                // loud, though: the screen promises "your previous controls were
                // saved", and this is the only place that knows whether they were.
                // A part-written backup is cleaned up rather than left to look
                // like a restore point — `has_backup` re-parses it for the same
                // reason.
                let dst = keymap_backup_path(basename);
                if let Err(e) = std::fs::copy(&src, &dst) {
                    log(&std::format!(
                        "keymap: backup of {} failed ({}) - no revert point for this apply\n",
                        basename, e,
                    ));
                    let _ = std::fs::remove_file(&dst);
                }
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
    // `has_backup`, not `exists`: an empty or corrupt backup must not be copied
    // over a working sidecar.
    let ok = if has_backup(basename) {
        // Restore the backup, then drop it.
        let restored = std::fs::copy(&bak, &dst).is_ok();
        if restored {
            let _ = std::fs::remove_file(&bak);
        }
        restored
    } else {
        // No usable backup → revert to the fallback default by removing the
        // sidecar. Reported honestly: the screen says "your controls were
        // restored", so a failed unlink (which leaves the applied profile in
        // place) must not come back as success. A file that was already gone is
        // still the desired end state.
        match std::fs::remove_file(&dst) {
            Ok(()) => true,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => true,
            Err(e) => {
                log(&std::format!(
                    "keymap: revert could not remove {} ({}) - controls unchanged\n",
                    dst, e,
                ));
                false
            }
        }
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
    // Full compare (base + combos + modifier, #57) so a combo-only change still
    // surfaces the "Revenir" row.
    !binding_diff_rows(&cur, &tgt).is_empty()
}

/// Build "<button>: <current> -> <target>" diff lines for the keys that differ
/// between two keymaps, across BOTH players (#40). P1 rows are unprefixed (the
/// common single-player case stays clean); P2 rows carry a "P2 " tag so a
/// two-player change is attributed to the right pad. The single source of truth
/// for every profile/revert/share preview (library + in-game) — diffing only P1
/// here was the bug where editing a P2 key showed "no changes" yet still dropped
/// the active tag.
pub fn binding_diff_rows(cur: &Keymap, tgt: &Keymap) -> std::vec::Vec<std::string::String> {
    let none = crate::loc::s().none;
    let disp = |k: &str| -> std::string::String {
        if k.is_empty() {
            none.to_string()
        } else {
            flash_key_display(k).into_owned()
        }
    };
    let mut rows = std::vec::Vec::new();
    let mut diff_map = |label: &str,
                        c_map: &BTreeMap<std::string::String, std::string::String>,
                        t_map: &BTreeMap<std::string::String, std::string::String>| {
        for btn in EDITABLE_BUTTONS {
            let c = c_map.get(*btn).map(std::string::String::as_str).unwrap_or("");
            let n = t_map.get(*btn).map(std::string::String::as_str).unwrap_or("");
            if c != n {
                rows.push(std::format!("{}{}: {} -> {}", label, btn, disp(c), disp(n)));
            }
        }
    };
    // Base bindings, both players (#40).
    diff_map("", &cur.bindings, &tgt.bindings);
    diff_map("P2 ", &cur.bindings_p2, &tgt.bindings_p2);
    // Per-modifier combo layers, both players (#57): a combo-only change must show
    // up too, and under the RIGHT modifier ("ZL+A" vs "R+A"). An absent layer diffs
    // against an empty one, so turning a combo on/off registers.
    let empty = BTreeMap::new();
    for m in &SUBTAB_MODS[1..] {
        let c1 = cur.combo_layers.get(*m).unwrap_or(&empty);
        let t1 = tgt.combo_layers.get(*m).unwrap_or(&empty);
        diff_map(&std::format!("{}+", m), c1, t1);
        let c2 = cur.combo_layers_p2.get(*m).unwrap_or(&empty);
        let t2 = tgt.combo_layers_p2.get(*m).unwrap_or(&empty);
        diff_map(&std::format!("P2 {}+", m), c2, t2);
    }
    // Cursor visibility (show-cursor toggle) rides IN the keymap, so a change to it
    // must register too — otherwise hiding the pointer would read as "no changes"
    // and drop the shareable/active tag (same class of bug as the P2/combo miss).
    if cur.show_cursor != tgt.show_cursor {
        let lc = crate::loc::s();
        let onoff = |b: bool| if b { lc.cursor_shown } else { lc.cursor_hidden };
        rows.push(std::format!(
            "{}: {} -> {}",
            lc.show_cursor,
            onoff(cur.show_cursor),
            onoff(tgt.show_cursor),
        ));
    }
    rows
}

/// A preview row for a cursor-speed change (issue #57 sharing), or None when
/// `cur == tgt`. Cursor speed lives outside the keymap, so it isn't in
/// `binding_diff_rows` — the share/apply preview appends this so the user SEES the
/// pointer speed is part of the profile. `-1` shows as the "(none)" label.
pub fn cursor_diff_row(cur: i32, tgt: i32) -> Option<std::string::String> {
    if cur == tgt {
        return None;
    }
    let disp = |v: i32| -> std::string::String {
        if v < 0 {
            crate::loc::s().none.to_string()
        } else {
            std::format!("#{}", v)
        }
    };
    Some(std::format!(
        "{}: {} -> {}",
        crate::loc::s().set_cursor_speed,
        disp(cur),
        disp(tgt),
    ))
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

/// Number of stage-scaling modes: 0 = fit (aspect kept, black bars, the Flash
/// default), 1 = fill (aspect kept, overflow cropped), 2 = stretch (fills by
/// distorting).
pub const DISPLAY_MODE_COUNT: u8 = 3;

/// Per-game stage-scaling mode (by basename), 0 when unset. Stored in its OWN
/// tiny `<basename>.display` file, like `.cursor`, so changing how a game fills
/// the screen never rewrites its key bindings.
///
/// PER GAME rather than a global setting, because filling the screen is paid for
/// in cropped picture and the price depends on the game: a 4:3 game loses a
/// little top and bottom and looks better for it, while a portrait game such as
/// Flappy Bird (500x700) would lose roughly 60% of its playfield. One global
/// switch would have made the second case the cost of fixing the first.
/// Issues #65, #69, #74.
pub fn display_mode_for(basename: &str) -> u8 {
    read_pref(basename, "display")
        .filter(|v| *v < DISPLAY_MODE_COUNT)
        .unwrap_or_else(crate::loc::default_display_mode)
}

/// One file per game for the three settings the pause menu cycles, instead of
/// three: `<basename>.prefs`, holding `key=value` lines.
///
/// Not a migration: `.display`, `.filter` and `.rot` all landed after v1.6.0 and
/// have never been published, so nobody's card carries them. `.cursor` is NOT
/// folded in -- that one shipped, and the C++ side owns its live value.
///
/// The reason is the SD card's directory listing, not read speed: none of these
/// are opened during the boot scan, but the scan walks every entry in
/// `sdmc:/flashnx/`, and 79 games times three files is up to 237 entries to step
/// over before the first tile is drawn.
fn prefs_path(basename: &str) -> std::string::String {
    primary_path(&std::format!("{}.prefs", basename))
}

fn read_pref(basename: &str, key: &str) -> Option<u8> {
    let path = find_user_path(&std::format!("{}.prefs", basename))?;
    let text = read_json_file(&path)?;
    for line in text.lines() {
        let (k, v) = line.split_once('=')?;
        if k.trim() == key {
            return v.trim().parse::<u8>().ok();
        }
    }
    None
}

/// Rewrite the file with `key` set, keeping every other key it held.
///
/// Read-modify-write rather than append: three settings sharing one file means a
/// blind append would grow it without bound and leave the older value in front,
/// where the reader above would find it first.
fn write_pref(basename: &str, key: &str, value: u8) -> bool {
    let mut out: std::vec::Vec<(std::string::String, u8)> = std::vec::Vec::new();
    if let Some(path) = find_user_path(&std::format!("{}.prefs", basename)) {
        if let Some(text) = read_json_file(&path) {
            for line in text.lines() {
                if let Some((k, v)) = line.split_once('=') {
                    let k = k.trim();
                    if k != key && !k.is_empty() {
                        if let Ok(n) = v.trim().parse::<u8>() {
                            out.push((k.to_string(), n));
                        }
                    }
                }
            }
        }
    }
    out.push((key.to_string(), value));
    out.sort_by(|a, b| a.0.cmp(&b.0));
    let body: std::string::String =
        out.iter().map(|(k, v)| std::format!("{}={}
", k, v)).collect();
    let path = prefs_path(basename);
    if let Err(e) = std::fs::write(&path, body.as_bytes()) {
        log(&std::format!("keymap: prefs for {} not saved: {}
", basename, e));
        return false;
    }
    crate::sd::commit();
    true
}

/// Quarter-turns of the picture: 0 = none, 1 = 90 CW, 2 = 180, 3 = 270 (#78).
pub const ROTATION_COUNT: u8 = 4;

/// Per-game rotation (by basename). Its own `<basename>.rot` file, exactly like
/// `.display` and `.filter`: turning the picture is a property of the GAME, not
/// of the console. A portrait game wants it always, a landscape game never.
pub fn rotation_for(basename: &str) -> u8 {
    read_pref(basename, "rot")
        .filter(|v| *v < ROTATION_COUNT)
        .unwrap_or_else(crate::loc::default_rotation)
}

/// Rotation of the ACTIVE game, 0 when nothing is playing.
pub fn rotation() -> u8 {
    match active_game_basename() {
        Some(b) => rotation_for(&b),
        None => 0,
    }
}

pub fn set_rotation(q: u8) {
    if let Some(b) = active_game_basename() {
        set_rotation_for(&b, q);
    }
}

/// Always written, 0 included, for the same reason as the display mode: a
/// missing file means "follow the global default", so a deliberate "no rotation"
/// has to be distinguishable from "never set".
pub fn set_rotation_for(basename: &str, q: u8) {
    write_pref(basename, "rot", q);
}

/// Number of screen filters: 0 = none, 1 = scanlines, 2 = CRT.
pub const SCREEN_FILTER_COUNT: u8 = 3;

/// Per-game screen filter (by basename), 0 when unset. Its own tiny
/// `<basename>.filter` file, like `.cursor` and `.display`.
pub fn screen_filter_for(basename: &str) -> u8 {
    read_pref(basename, "filter")
        .filter(|v| *v < SCREEN_FILTER_COUNT)
        .unwrap_or_else(crate::loc::default_screen_filter)
}

/// Screen filter of the ACTIVE game, 0 when unset or when no game is active.
pub fn screen_filter() -> u8 {
    match active_game_basename() {
        Some(b) => screen_filter_for(&b),
        None => 0,
    }
}

/// Persist the ACTIVE game's screen filter. Filter 0 is the default, so it
/// clears the file rather than writing it.
pub fn set_screen_filter(mode: u8) {
    let Some(basename) = active_game_basename() else {
        return;
    };
    // Always written, 0 included: since a missing file now means "follow the
    // global default", deleting it on 0 would make "no filter, deliberately"
    // indistinguishable from "never touched" and the game would drift back to the
    // default the next time it changes.
    // A failure is reported by `write_pref`, which matters: a swallowed one
    // looks like a DEAD BUTTON here, because the row's label is rebuilt by
    // re-reading the file and the next value is derived from the same unchanged
    // file, so pressing again re-applies exactly what was there.
    write_pref(&basename, "filter", mode);
}

/// Stage-scaling mode of the ACTIVE game (the one being played), 0 when unset
/// or when no game is active.
pub fn display_mode() -> u8 {
    match active_game_basename() {
        Some(b) => display_mode_for(&b),
        None => 0,
    }
}

/// Persist the ACTIVE game's stage-scaling mode. Called from the in-game pause
/// menu, which also applies it to the running player so the change is visible
/// behind the panel straight away.
pub fn set_display_mode(mode: u8) {
    if let Some(b) = active_game_basename() {
        set_display_mode_for(&b, mode);
    }
}

/// Persist a game's stage-scaling mode. Always written, 0 included: a missing
/// file means "follow the global default", so clearing it on 0 would lose the
/// difference between a deliberate INTEGRAL and a game nobody ever set.
pub fn set_display_mode_for(basename: &str, mode: u8) {
    write_pref(basename, "display", mode);
}

/// Persist a cursor-speed preset for an ARBITRARY game (by basename). `idx < 0`
/// clears the per-game file. Used by the library sub-menu (the in-game VITESSE
/// uses `set_cursor_speed`, which targets the active game).
pub fn set_cursor_speed_for(basename: &str, idx: i32) {
    if write_cursor_speed(basename, idx) {
        mark_controls_touched(basename);
    }
}

/// The same write WITHOUT `mark_controls_touched`, for use while applying a
/// community profile.
///
/// `apply` tags the sidecar `community:<id>` and then set the cursor speed, whose
/// mark re-read that very sidecar and rewrote the source as `"user"` — erasing
/// the tag one line after writing it. Nothing looked wrong (the controls are
/// correct), but the keymap then claimed to be the player's own work, so the
/// guard that stops PARTAGER re-uploading someone else's profile under this
/// install's id no longer fired.
pub fn set_cursor_speed_from_profile(basename: &str, idx: i32) -> bool {
    write_cursor_speed(basename, idx)
}

fn write_cursor_speed(basename: &str, idx: i32) -> bool {
    let path = primary_path(&std::format!("{}.cursor", basename));
    if idx < 0 {
        if let Err(e) = std::fs::remove_file(&path) {
            if e.kind() != std::io::ErrorKind::NotFound {
                log(&std::format!("keymap: could not clear {} ({})\n", path, e));
                return false;
            }
        }
    } else if let Err(e) = std::fs::write(&path, std::format!("{}", idx).as_bytes()) {
        log(&std::format!("keymap: cursor speed for {} not saved: {}\n", basename, e));
        return false;
    }
    crate::sd::commit();
    true
}

/// After a NON-keymap per-game change (cursor speed), a keymap still tagged as an
/// already-shared community profile should count as user-modified again, so the
/// share flow lifts its "already shared" block and lets you re-share the updated
/// setup. Flips a `community:*` sidecar (and the in-memory active keymap, if it's
/// this game) to `source = "user"`. No-op when there's no community sidecar.
pub fn mark_controls_touched(basename: &str) {
    if let Some(mut km) = read_sidecar_file(basename) {
        if km.source.starts_with("community:") {
            km.source = "user".into();
            write_keymap_file(&keymap_write_path(basename), &km);
        }
    }
    if let Ok(mut g) = ACTIVE_KEYMAP.lock() {
        let is_this = ACTIVE_BASENAME
            .lock()
            .ok()
            .and_then(|b| b.as_deref().map(|s| s == basename))
            .unwrap_or(false);
        if is_this {
            if let Some(km) = g.as_mut() {
                if km.source.starts_with("community:") {
                    km.source = "user".into();
                }
            }
        }
    }
}

/// Persist the active GAME's per-game cursor-speed preset to `<basename>.cursor`.
/// Called (only) from the in-game VITESSE cycle — C++ handles the library /
/// RÉGLAGES global default separately. `idx < 0` clears the per-game file.
pub fn set_cursor_speed(idx: i32) {
    let Some(basename) = active_game_basename() else {
        return;
    };
    // Shares `write_cursor_speed` with the by-basename variant so the two cannot
    // drift, and so this one reports its failures like the rest.
    if write_cursor_speed(&basename, idx) {
        mark_controls_touched(&basename);
    }
}

// ── Show-cursor toggle (per-game pointer visibility) ────────────────────────

/// Whether the ACTIVE game's on-screen pointer is drawn. Reads the live keymap;
/// defaults to SHOWN when nothing's loaded. Read by C++ each frame (via FFI) to
/// decide whether to draw the cursor sprite.
pub fn show_cursor() -> bool {
    ACTIVE_KEYMAP
        .lock()
        .ok()
        .and_then(|g| g.as_ref().map(|k| k.show_cursor))
        .unwrap_or(true)
}

/// Same, for an ARBITRARY game by basename — the library OPTIONS > TOUCHES menu
/// reads/labels a game's flag without that game being active. Falls back to SHOWN.
pub fn show_cursor_for(basename: &str) -> bool {
    effective_for(basename).show_cursor
}

/// Toggle the ACTIVE game's "show cursor" flag, persisting to its sidecar and
/// flipping provenance to "user" (a hand edit → shareable again). No-op when no
/// game keymap is loaded.
pub fn toggle_show_cursor() {
    {
        let mut g = match ACTIVE_KEYMAP.lock() {
            Ok(g) => g,
            Err(_) => return,
        };
        let Some(km) = g.as_mut() else { return };
        km.show_cursor = !km.show_cursor;
        km.source = "user".into();
    }
    save_sidecar();
}

/// Toggle it for an ARBITRARY game (library side, game not running): read the
/// effective keymap, flip the flag, mark it "user", and write the sidecar so the
/// next launch picks it up.
pub fn toggle_show_cursor_for(basename: &str) {
    let mut km = effective_for(basename);
    km.show_cursor = !km.show_cursor;
    km.source = "user".into();
    write_keymap_file(&keymap_write_path(basename), &km);
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

/// The editor's active SUB-TAB, PER PLAYER (index 0 = P1, 1 = P2), as an index
/// into `SUBTAB_MODS`: 0 = NORMAL (base bindings), 1..=4 = the ZL/ZR/L/R combo
/// layer being edited (issue #57, per-modifier). Session state, reset to NORMAL
/// for both when the editor opens (the persisted combo layers are untouched — a
/// modifier stays active in-game as long as its layer has bindings). Steers only
/// the editor's `current_binding` / `set_binding`; in-game uses `lookup_combo*`.
static EDIT_SUBTAB: [std::sync::atomic::AtomicU8; 2] = [
    std::sync::atomic::AtomicU8::new(0),
    std::sync::atomic::AtomicU8::new(0),
];

/// Sub-tab index -> modifier name. Index 0 ("") = the NORMAL (base) tab.
pub const SUBTAB_MODS: [&str; 5] = ["", "ZL", "ZR", "L", "R"];

fn subtab_slot() -> usize {
    if edit_player() == 2 { 1 } else { 0 }
}

/// Current sub-tab index (0..=4) for the edit player. 0 = NORMAL.
pub fn edit_subtab_index() -> usize {
    EDIT_SUBTAB[subtab_slot()].load(std::sync::atomic::Ordering::Relaxed) as usize
}

/// Set the edit player's sub-tab index (clamped to a valid slot).
pub fn set_edit_subtab_index(idx: usize) {
    EDIT_SUBTAB[subtab_slot()].store(
        idx.min(SUBTAB_MODS.len() - 1) as u8,
        std::sync::atomic::Ordering::Relaxed,
    );
}

/// The modifier name of the edit player's current sub-tab ("" = NORMAL/base).
pub fn edit_subtab_modifier() -> &'static str {
    SUBTAB_MODS[edit_subtab_index()]
}

/// Reset BOTH players' sub-tabs to NORMAL — called when the editor opens so it
/// always starts clean, without touching the persisted combo layers.
pub fn reset_edit_subtabs() {
    for v in &EDIT_SUBTAB {
        v.store(0, std::sync::atomic::Ordering::Relaxed);
    }
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

/// The Flash `SK_*` a button sends while `modifier` (ZL/ZR/L/R) is held on
/// controller 1 (issue #57, per-modifier layer). `None` = no combo override for
/// this button → the C++ input layer falls through to the base key, so a held
/// modifier never breaks unremapped buttons.
pub fn lookup_combo(modifier: &str, button_name: &str) -> Option<core::ffi::c_int> {
    let g = ACTIVE_KEYMAP.lock().ok()?;
    let km = g.as_ref()?;
    let key_name = km.combo_layers.get(modifier)?.get(button_name)?;
    if key_name.is_empty() {
        return None;
    }
    Some(flash_key_name_to_sk(key_name))
}

/// Player-2 counterpart of [`lookup_combo`].
pub fn lookup_combo_p2(modifier: &str, button_name: &str) -> Option<core::ffi::c_int> {
    let g = ACTIVE_KEYMAP.lock().ok()?;
    let km = g.as_ref()?;
    let key_name = km.combo_layers_p2.get(modifier)?.get(button_name)?;
    if key_name.is_empty() {
        return None;
    }
    Some(flash_key_name_to_sk(key_name))
}

/// A combo layer is "active" if it has at least one NON-empty binding (a layer
/// where every button is explicitly unbound counts as off).
fn layer_has_binding(
    layer: Option<&BTreeMap<std::string::String, std::string::String>>,
) -> bool {
    layer.map(|m| m.values().any(|v| !v.is_empty())).unwrap_or(false)
}

/// True when P1's `modifier` (ZL/ZR/L/R) has a live combo layer → in-game that
/// button acts as a modifier (its own base key muted; hold it to reach the layer).
pub fn combo_active(modifier: &str) -> bool {
    ACTIVE_KEYMAP
        .lock()
        .ok()
        .and_then(|g| g.as_ref().map(|k| layer_has_binding(k.combo_layers.get(modifier))))
        .unwrap_or(false)
}

/// Player-2 counterpart of [`combo_active`].
pub fn combo_active_p2(modifier: &str) -> bool {
    ACTIVE_KEYMAP
        .lock()
        .ok()
        .and_then(|g| g.as_ref().map(|k| layer_has_binding(k.combo_layers_p2.get(modifier))))
        .unwrap_or(false)
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
        "Keyboard" => lc.flash_keyboard,
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
        "CapsLock"  => crate::SK_CAPSLOCK,
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
        // Function keys F1-F12.
        "F1" => crate::SK_F1, "F2" => crate::SK_F2, "F3" => crate::SK_F3,
        "F4" => crate::SK_F4, "F5" => crate::SK_F5, "F6" => crate::SK_F6,
        "F7" => crate::SK_F7, "F8" => crate::SK_F8, "F9" => crate::SK_F9,
        "F10" => crate::SK_F10, "F11" => crate::SK_F11, "F12" => crate::SK_F12,
        // Punctuation / symbols.
        "-" => crate::SK_MINUS, "=" => crate::SK_EQUALS,
        "[" => crate::SK_LBRACKET, "]" => crate::SK_RBRACKET,
        ";" => crate::SK_SEMICOLON, "'" => crate::SK_QUOTE,
        "," => crate::SK_COMMA, "." => crate::SK_PERIOD,
        "/" => crate::SK_SLASH, "\\" => crate::SK_BACKSLASH,
        "`" => crate::SK_BACKQUOTE,
        // Numpad operators.
        "Num+" => crate::SK_NUMPAD_ADD, "Num-" => crate::SK_NUMPAD_SUB,
        "Num*" => crate::SK_NUMPAD_MUL, "Num/" => crate::SK_NUMPAD_DIV,
        "Num." => crate::SK_NUMPAD_DECIMAL, "NumEnter" => crate::SK_NUMPAD_ENTER,
        // Mouse-click pseudo-keys (routed to the mouse, not the keyboard).
        "Left click" => crate::SK_MOUSE_LEFT,
        "Right click" => crate::SK_MOUSE_RIGHT,
        // Opens the console keyboard by hand.
        "Keyboard" => crate::SK_KEYBOARD,
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
