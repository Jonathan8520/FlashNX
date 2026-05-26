//! Library UI — the "FlashNX launcher" shown at boot before Ruffle starts.
//!
//! Phase 3.4 — replaces the silent `swf_picker_run` first-hit logic with a
//! full game list. C++ enumerates every `.swf` in `sdmc:/ruffle/` and
//! `sdmc:/switch/ruffle/` and forwards each path here via
//! `ruffle_library_add_path`. We parse each file's SWF header lazily on the
//! way in (version + size + dims) so the metadata panel doesn't need to
//! re-open files on every cursor move.
//!
//! Screens (state machine, like `menu::Screen`):
//!   - **Empty**:        no SWF found, instructions where to drop files.
//!   - **List**:         scrollable game list, A=JOUER, X=OPTIONS, -=QUITTER.
//!   - **OptionsModal**: per-game options (TOUCHES + RETOUR for v1).
//!   - **Picked**:       user picked a game; `selected_path` is set; C++
//!                       polls `is_active()` and exits the library loop.
//!   - **Quit**:         user pressed - on the empty/list; C++ exits the
//!                       worker thread (and the `.nro`).
//!
//! Rendering lives in `backend::render::SwitchRenderBackend::draw_library_*`
//! (mirroring the TOUCHES list pattern) so this module stays focused on
//! state + input.
//!
//! When the user selects TOUCHES from the options modal, we delegate to the
//! existing `menu` module's editor — same `menu::open` / `menu::input` /
//! `menu::draw` we use mid-game. To make that work pre-launch, we re-init
//! the keymap module for the chosen game's basename via
//! `keymap::init_for_swf` here too (the keymap module is `OnceLock` so the
//! first init wins — fine because the library always picks BEFORE Ruffle).

use std::fs::File;
use std::io::Read;
use std::sync::Mutex;

use crate::backend::render::SwitchRenderBackend;
use crate::{keymap, menu};

/// Currently displayed library screen. `Inactive` is set before
/// `ruffle_library_init` runs and after the user has picked a game.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Screen {
    Inactive,
    /// No SWF on SD. Shows a help message ("drop .swf in sdmc:/ruffle/").
    Empty,
    /// Main game list. `selection` indexes `Library::entries`,
    /// `scroll_offset` is the topmost visible row.
    List { selection: usize, scroll_offset: usize },
    /// OPTIONS modal for the game at `game_idx`. `selection` indexes
    /// `OPTIONS_ENTRIES`.
    OptionsModal { game_idx: usize, selection: usize },
    /// User pressed A on a game; main loop reads `selected_path` and exits.
    Picked,
    /// User pressed - or chose to quit; main loop exits the `.nro`.
    Quit,
    /// User pressed A on OPTIONS > TOUCHES — control delegated to
    /// `menu::*`. When `menu::is_active()` returns false we return to the
    /// OptionsModal screen.
    TouchesEditor { game_idx: usize },
}

pub(crate) const OPTIONS_ENTRIES: &[&str] = &["TOUCHES", "RETOUR"];

/// Cached SWF header data parsed once at scan time. Compressed (CWS) movies
/// also store dims here — we inflate the first ~256 bytes via flate2 to
/// reach the RECT.
#[derive(Debug, Clone)]
pub(crate) struct Entry {
    pub path: std::string::String,
    pub basename: std::string::String,
    pub display_name: std::string::String,
    pub size_bytes: u64,
    pub swf_version: u8,
    /// 0 = uncompressed FWS, 1 = zlib CWS, 2 = lzma ZWS.
    pub compression_label: &'static str,
    /// `None` if we couldn't parse the RECT (rare CWS/ZWS edge cases).
    pub width_px: Option<u32>,
    pub height_px: Option<u32>,
    /// 0xRRGGBB derived from a hash of the basename — drives the per-game
    /// color chip in the list. Same hash always produces the same color
    /// across reboots (no persistence needed) because the input is stable.
    pub color_chip: u32,
}

pub(crate) struct State {
    pub(crate) screen: Screen,
    pub(crate) entries: std::vec::Vec<Entry>,
    /// Set when the user presses A on a game. Read by C++ via
    /// `ruffle_library_selected_path` after the loop exits.
    selected_path: Option<std::string::String>,
    /// GL texture id for the FlashNX banner image (assets/banner.png decoded
    /// at init). 0 = not loaded (decode failed or init not called yet).
    pub(crate) banner_tex: u32,
    pub(crate) banner_w: u32,
    pub(crate) banner_h: u32,
    /// Monotonic wall-clock ticks captured at init; render() subtracts this
    /// to feed a stable phase into sin() animations (cursor pulse, selection
    /// pulse).
    pub(crate) anim_origin_ticks: u64,
}

static LIBRARY: Mutex<State> = Mutex::new(State {
    screen: Screen::Inactive,
    entries: std::vec::Vec::new(),
    selected_path: None,
    banner_tex: 0,
    banner_w: 0,
    banner_h: 0,
    anim_origin_ticks: 0,
});

/// `visible_rows` on the list screen — keep in sync with the slot count
/// drawn in `draw_library_list`. Picked so 1280×720 fits header + banner +
/// rows + footer with margin.
pub const LIST_VISIBLE_ROWS: usize = 6;

// ── Setup / FFI helpers ───────────────────────────────────────────────────

extern "C" {
    fn ruffle_tick_now() -> u64;
    fn ruffle_log_cstr(msg: *const core::ffi::c_char);
}

fn log(s: &str) {
    let mut bytes = s.as_bytes().to_vec();
    bytes.push(0);
    unsafe { ruffle_log_cstr(bytes.as_ptr() as *const _) };
}

/// Called from C++ once per `.swf` found during the SD scan. Parses the
/// header inline so the library list has full metadata to display.
pub fn add_path(path: &str) -> bool {
    let basename = path
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(path)
        .to_string();
    let (size_bytes, swf_version, compression_label, dims) = match read_swf_header(path) {
        Some(h) => (h.size_bytes, h.version, h.compression_label, h.dims),
        None => {
            log(&std::format!(
                "library: failed to parse SWF header for {}, skipping\n",
                path,
            ));
            return false;
        }
    };
    let color_chip = color_from_basename(&basename);
    let entry = Entry {
        path: path.to_string(),
        display_name: basename.clone(),
        basename,
        size_bytes,
        swf_version,
        compression_label,
        width_px: dims.map(|(w, _)| w),
        height_px: dims.map(|(_, h)| h),
        color_chip,
    };
    if let Ok(mut s) = LIBRARY.lock() {
        s.entries.push(entry);
    }
    true
}

/// Transition from Inactive → List (or Empty if `entries` is empty). Called
/// after C++ has finished scanning and the GL renderer is up.
pub fn open() {
    if let Ok(mut s) = LIBRARY.lock() {
        s.anim_origin_ticks = unsafe { ruffle_tick_now() };
        s.screen = if s.entries.is_empty() {
            Screen::Empty
        } else {
            Screen::List { selection: 0, scroll_offset: 0 }
        };
    }
}

/// True while the library should keep getting input + render frames. C++
/// loops on this. Returns false once the user picks a game (Picked) or
/// asks to quit (Quit).
pub fn is_active() -> bool {
    match LIBRARY.lock().map(|s| s.screen) {
        Ok(Screen::Inactive) | Ok(Screen::Picked) | Ok(Screen::Quit) => false,
        Ok(_) => true,
        Err(_) => false,
    }
}

/// True if the user picked a game (vs quit). Lets C++ distinguish "load
/// SWF" from "exit `.nro`" after the library loop ends.
pub fn picked() -> bool {
    matches!(LIBRARY.lock().map(|s| s.screen), Ok(Screen::Picked))
}

/// Owned copy of the chosen path. None until the user presses A on a game.
pub fn selected_path() -> Option<std::string::String> {
    LIBRARY.lock().ok().and_then(|s| s.selected_path.clone())
}

/// Forward a Switch-button down-edge from C++. Returns true if consumed.
pub fn input(button: &str) -> bool {
    // Sub-screen: TOUCHES editor owns input while active.
    if menu::is_active() {
        let consumed = menu::input(button);
        // If menu just closed itself, fall back to the OPTIONS modal.
        if !menu::is_active() {
            if let Ok(mut s) = LIBRARY.lock() {
                if let Screen::TouchesEditor { game_idx } = s.screen {
                    s.screen = Screen::OptionsModal { game_idx, selection: 0 };
                }
            }
        }
        return consumed;
    }
    let mut s = match LIBRARY.lock() {
        Ok(g) => g,
        Err(_) => return false,
    };
    let screen_copy = s.screen;
    match screen_copy {
        Screen::Inactive | Screen::Picked | Screen::Quit => false,
        Screen::Empty => {
            if matches!(button, "Minus" | "B") {
                s.screen = Screen::Quit;
            }
            true
        }
        Screen::List { selection, scroll_offset } => {
            handle_list_input(&mut s, button, selection, scroll_offset);
            true
        }
        Screen::OptionsModal { game_idx, selection } => {
            handle_options_input(&mut s, button, game_idx, selection);
            true
        }
        Screen::TouchesEditor { .. } => false,
    }
}

fn handle_list_input(s: &mut State, button: &str, mut selection: usize, mut scroll: usize) {
    let last = s.entries.len().saturating_sub(1);
    match button {
        "Up" | "StickLUp" => {
            selection = if selection == 0 { last } else { selection - 1 };
            scroll = clamp_scroll(scroll, selection);
        }
        "Down" | "StickLDown" => {
            selection = if selection >= last { 0 } else { selection + 1 };
            scroll = clamp_scroll(scroll, selection);
        }
        "A" => {
            if let Some(entry) = s.entries.get(selection) {
                s.selected_path = Some(entry.path.clone());
                log(&std::format!(
                    "library: JOUER -> {} ({})\n",
                    entry.display_name, entry.path,
                ));
                s.screen = Screen::Picked;
            }
            return;
        }
        "X" => {
            if !s.entries.is_empty() {
                s.screen = Screen::OptionsModal { game_idx: selection, selection: 0 };
            }
            return;
        }
        "Minus" => {
            s.screen = Screen::Quit;
            return;
        }
        _ => {}
    }
    s.screen = Screen::List { selection, scroll_offset: scroll };
}

fn handle_options_input(s: &mut State, button: &str, game_idx: usize, mut selection: usize) {
    let last = OPTIONS_ENTRIES.len().saturating_sub(1);
    match button {
        "Up" | "StickLUp" => {
            selection = if selection == 0 { last } else { selection - 1 };
        }
        "Down" | "StickLDown" => {
            selection = if selection >= last { 0 } else { selection + 1 };
        }
        "A" => {
            match OPTIONS_ENTRIES[selection] {
                "TOUCHES" => {
                    // Initialise the keymap for THIS game so the editor's
                    // current_binding/set_binding land in the right
                    // sidecar file. `init_for_swf` is OnceLock and may
                    // already have been called by a prior open; that's OK
                    // because we ALWAYS pick a game before ruffle_init
                    // runs, so this is the first init.
                    if let Some(entry) = s.entries.get(game_idx) {
                        keymap::init_for_swf(&entry.basename);
                    }
                    menu::open();
                    s.screen = Screen::TouchesEditor { game_idx };
                    return;
                }
                "RETOUR" => {
                    let scroll = clamp_scroll(0, game_idx);
                    s.screen = Screen::List { selection: game_idx, scroll_offset: scroll };
                    return;
                }
                _ => {}
            }
        }
        "B" | "Minus" => {
            let scroll = clamp_scroll(0, game_idx);
            s.screen = Screen::List { selection: game_idx, scroll_offset: scroll };
            return;
        }
        _ => {}
    }
    s.screen = Screen::OptionsModal { game_idx, selection };
}

fn clamp_scroll(mut scroll: usize, selection: usize) -> usize {
    if selection < scroll {
        scroll = selection;
    } else if selection >= scroll + LIST_VISIBLE_ROWS {
        scroll = selection + 1 - LIST_VISIBLE_ROWS;
    }
    scroll
}

/// Render the current screen using the backend. C++ calls this each frame
/// while the library is active, AFTER `glClear` (we own the entire
/// framebuffer — no Ruffle behind us at this stage).
pub fn render(backend: &mut SwitchRenderBackend) {
    let s = match LIBRARY.lock() {
        Ok(g) => g,
        Err(_) => return,
    };
    let screen = s.screen;
    let anim_origin = s.anim_origin_ticks;
    drop(s);

    // Phase = current tick - origin. Drives sin() animations. ms-resolution
    // is enough; we compute it lazily below to avoid an FFI call when the
    // current screen has no animation.
    match screen {
        Screen::Inactive | Screen::Picked | Screen::Quit => {}
        Screen::Empty => {
            backend.draw_library_empty();
        }
        Screen::List { selection, scroll_offset } => {
            // Snapshot entries + banner state so we don't hold the lock
            // across the GL FFI calls in draw_library_list.
            let snapshot = LIBRARY.lock().ok().map(|s| {
                LibraryListSnapshot {
                    entries: s.entries.clone(),
                    banner_tex: s.banner_tex,
                    banner_w: s.banner_w,
                    banner_h: s.banner_h,
                }
            });
            if let Some(snap) = snapshot {
                let now = unsafe { ruffle_tick_now() };
                let phase_ticks = now.saturating_sub(anim_origin);
                backend.draw_library_list(
                    selection,
                    scroll_offset,
                    &snap.entries,
                    LIST_VISIBLE_ROWS,
                    snap.banner_tex,
                    snap.banner_w,
                    snap.banner_h,
                    phase_ticks,
                );
            }
        }
        Screen::OptionsModal { game_idx, selection } => {
            let entry_snapshot = LIBRARY
                .lock()
                .ok()
                .and_then(|s| s.entries.get(game_idx).cloned());
            if let Some(entry) = entry_snapshot {
                backend.draw_library_options(
                    &entry.display_name,
                    selection,
                    OPTIONS_ENTRIES,
                );
            }
        }
        Screen::TouchesEditor { .. } => {
            // Backdrop = Library list frozen behind. Cheapest path: redraw
            // a dim panel + delegate to menu::draw.
            backend.draw_library_dim_backdrop();
            menu::draw(backend);
        }
    }
}

pub(crate) struct LibraryListSnapshot {
    pub entries: std::vec::Vec<Entry>,
    pub banner_tex: u32,
    pub banner_w: u32,
    pub banner_h: u32,
}

// ── Banner PNG decoding ───────────────────────────────────────────────────

const BANNER_PNG: &[u8] = include_bytes!("../../assets/banner.png");

/// Decode the embedded banner PNG into RGBA bytes + dims. Called once from
/// `ruffle_library_init` (after the renderer is up). On success the caller
/// uploads the bytes as a GL texture; on failure (corrupt PNG, OOM) we just
/// fall back to drawing the title via the pixel font (existing path).
pub(crate) fn decode_banner() -> Option<(std::vec::Vec<u8>, u32, u32)> {
    // png 0.18 wants BufRead + Seek — wrap the static byte slice in a
    // Cursor so std::io::Read + Seek are satisfied without extra alloc.
    let cursor = std::io::Cursor::new(BANNER_PNG);
    let decoder = png::Decoder::new(cursor);
    let mut reader = match decoder.read_info() {
        Ok(r) => r,
        Err(e) => {
            log(&std::format!("library: banner PNG decode_info failed: {:?}\n", e));
            return None;
        }
    };
    let info = reader.info().clone();
    let w = info.width;
    let h = info.height;
    let out_size = reader.output_buffer_size()?;
    let mut buf = std::vec![0u8; out_size];
    if let Err(e) = reader.next_frame(&mut buf) {
        log(&std::format!("library: banner PNG decode failed: {:?}\n", e));
        return None;
    }
    // Promote to RGBA8 if needed. assets/banner.png is RGBA per the README
    // spec, but the user might re-export as RGB or palette down the line.
    let rgba = match info.color_type {
        png::ColorType::Rgba => buf,
        png::ColorType::Rgb => {
            let mut out = std::vec::Vec::with_capacity(buf.len() / 3 * 4);
            for px in buf.chunks_exact(3) {
                out.extend_from_slice(&[px[0], px[1], px[2], 0xFF]);
            }
            out
        }
        png::ColorType::GrayscaleAlpha => {
            let mut out = std::vec::Vec::with_capacity(buf.len() * 2);
            for px in buf.chunks_exact(2) {
                out.extend_from_slice(&[px[0], px[0], px[0], px[1]]);
            }
            out
        }
        png::ColorType::Grayscale => {
            let mut out = std::vec::Vec::with_capacity(buf.len() * 4);
            for &px in &buf {
                out.extend_from_slice(&[px, px, px, 0xFF]);
            }
            out
        }
        png::ColorType::Indexed => {
            log("library: indexed-color PNG banner not supported, ignoring\n");
            return None;
        }
    };
    log(&std::format!(
        "library: banner PNG decoded {}x{} ({} bytes RGBA)\n",
        w, h, rgba.len(),
    ));
    Some((rgba, w, h))
}

/// Called by `ruffle_library_init` after `decode_banner` returned ok. Stores
/// the GL texture id + dims into the library state so `render` can pass
/// them to `draw_library_list`.
pub(crate) fn set_banner_texture(tex: u32, w: u32, h: u32) {
    if let Ok(mut s) = LIBRARY.lock() {
        s.banner_tex = tex;
        s.banner_w = w;
        s.banner_h = h;
    }
}

// ── SWF header parsing ────────────────────────────────────────────────────

struct ParsedSwfHeader {
    size_bytes: u64,
    version: u8,
    compression_label: &'static str,
    dims: Option<(u32, u32)>,
}

fn read_swf_header(path: &str) -> Option<ParsedSwfHeader> {
    // Chunked 4 KB reads — matches the safe path keymap.rs uses for the
    // ENOMEM-at-32KB newlib quirk. For the SWF header we only ever need
    // the first chunk.
    let mut file = File::open(path).ok()?;
    let size_bytes = file.metadata().ok().map(|m| m.len()).unwrap_or(0);
    let mut buf = [0u8; 4096];
    let n = file.read(&mut buf).ok()?;
    if n < 8 {
        return None;
    }
    let (compression_label, compressed) = match &buf[0..3] {
        b"FWS" => ("FWS", None),
        b"CWS" => ("CWS", Some(true)),
        b"ZWS" => ("ZWS", Some(false)),
        _ => return None,
    };
    let version = buf[3];
    // Body starts at byte 8 (after sig + version + file_length). For CWS
    // it's a zlib stream we need to inflate to reach the RECT.
    let dims = match compressed {
        None => parse_rect(&buf[8..n]),
        Some(true) => inflate_and_parse_rect(&buf[8..n]),
        Some(false) => None, // ZWS (lzma) — rare, skip dims for v1
    };
    Some(ParsedSwfHeader {
        size_bytes,
        version,
        compression_label,
        dims,
    })
}

fn inflate_and_parse_rect(compressed_body: &[u8]) -> Option<(u32, u32)> {
    use flate2::read::ZlibDecoder;
    let mut decoder = ZlibDecoder::new(compressed_body);
    let mut out = [0u8; 256];
    // The decoder may return less than 256 bytes if the compressed input
    // we have doesn't expand that far — that's fine, RECT fits in <=17.
    let n = decoder.read(&mut out).ok()?;
    if n == 0 {
        return None;
    }
    parse_rect(&out[..n])
}

/// Parse the SWF RECT (stage bounds) from the uncompressed body. Returns
/// (width_px, height_px) — the difference between Xmax/Ymax and Xmin/Ymin,
/// converted from twips (1/20 px) to pixels. Returns None on truncated input.
fn parse_rect(body: &[u8]) -> Option<(u32, u32)> {
    if body.is_empty() {
        return None;
    }
    let mut bits = BitReader::new(body);
    let nbits = bits.read_ubits(5)? as u32;
    if nbits == 0 || nbits > 31 {
        return None;
    }
    let xmin = bits.read_sbits(nbits as u8)?;
    let xmax = bits.read_sbits(nbits as u8)?;
    let ymin = bits.read_sbits(nbits as u8)?;
    let ymax = bits.read_sbits(nbits as u8)?;
    let w = ((xmax - xmin).max(0) as u64 / 20) as u32;
    let h = ((ymax - ymin).max(0) as u64 / 20) as u32;
    Some((w, h))
}

/// Minimal MSB-first bit reader. Mirrors what `swf::read::Reader` does
/// internally — we inline it here to avoid pulling more of the swf crate's
/// internals (Reader isn't a re-export, and we only need ~10 lines of it).
struct BitReader<'a> {
    data: &'a [u8],
    /// Byte cursor + remaining-bits-in-current-byte counter.
    byte: usize,
    bit: u8,
}

impl<'a> BitReader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, byte: 0, bit: 8 }
    }

    fn read_ubits(&mut self, n: u8) -> Option<u32> {
        let mut acc: u32 = 0;
        for _ in 0..n {
            if self.byte >= self.data.len() {
                return None;
            }
            if self.bit == 0 {
                self.byte += 1;
                self.bit = 8;
                if self.byte >= self.data.len() {
                    return None;
                }
            }
            let bit = (self.data[self.byte] >> (self.bit - 1)) & 1;
            acc = (acc << 1) | (bit as u32);
            self.bit -= 1;
        }
        Some(acc)
    }

    /// Signed n-bit read, sign-extended from the top bit.
    fn read_sbits(&mut self, n: u8) -> Option<i64> {
        if n == 0 {
            return Some(0);
        }
        let raw = self.read_ubits(n)? as i64;
        let sign_mask = 1i64 << (n - 1);
        if raw & sign_mask != 0 {
            // Negative — sign extend.
            let high_mask = !((sign_mask << 1) - 1);
            Some(raw | high_mask)
        } else {
            Some(raw)
        }
    }
}

// ── Color chip from basename ──────────────────────────────────────────────

/// FNV-1a-style 32-bit hash, folded into HSV (H from hash, S/V fixed) so
/// every basename maps to a distinct vivid color. Tied to the basename
/// (not display_name) so renaming a game in 3.4.bis sidecar doesn't change
/// the chip — same physical file, same chip, less visual jank.
fn color_from_basename(basename: &str) -> u32 {
    let mut h: u32 = 2166136261;
    for &b in basename.as_bytes() {
        h ^= b as u32;
        h = h.wrapping_mul(16777619);
    }
    // Map hash → hue [0, 360), then HSV(H, 0.65, 0.95) → RGB.
    let hue = (h % 360) as f32;
    hsv_to_rgb_u32(hue, 0.65, 0.95)
}

fn hsv_to_rgb_u32(h: f32, s: f32, v: f32) -> u32 {
    let c = v * s;
    let h6 = h / 60.0;
    let x = c * (1.0 - ((h6 % 2.0) - 1.0).abs());
    let (r1, g1, b1) = match h6 as i32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = v - c;
    let r = ((r1 + m) * 255.0) as u32 & 0xFF;
    let g = ((g1 + m) * 255.0) as u32 & 0xFF;
    let b = ((b1 + m) * 255.0) as u32 & 0xFF;
    (r << 16) | (g << 8) | b
}
