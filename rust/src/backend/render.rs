//! `SwitchRenderBackend` — Ruffle `RenderBackend` impl backed by switch-mesa GL.
//!
//! Phase 1.3 complete (2026-05-23). Three shader programs cover the bulk of
//! Flash's 2D rendering needs:
//!
//!   - **solid**:    per-vertex (pos.xy, rgba) + uniform `u_world` + color
//!                   transform (`u_mult`, `u_add`). Drives `RenderShape`
//!                   solid fills, `DrawRect`, and `DrawLine`/`DrawLineRect`.
//!   - **bitmap**:   per-vertex (pos.xy, uv.xy), samples `u_tex` and applies
//!                   color transform. Drives `RenderBitmap` (and bitmap
//!                   fills inside `RenderShape` once 1.3.6.b lands).
//!   - **gradient**: per-vertex (pos.xy), samples a 256x1 gradient ramp
//!                   texture indexed by `t` computed from a per-draw
//!                   `u_grad_local` matrix; supports linear/radial/focal
//!                   (focal currently approximated as radial) and the three
//!                   spread modes (pad, reflect, repeat).
//!
//! Masking uses the framebuffer stencil buffer (`EGL_STENCIL_SIZE=8` in
//! `gl_context.cpp`). The four mask commands (`push_mask`, `activate_mask`,
//! `deactivate_mask`, `pop_mask`) track a depth counter and toggle
//! `glColorMask`/`glStencilFunc`/`glStencilOp` accordingly.
//!
//! Coordinate convention:
//!   - Tessellator outputs vertex positions in *pixels* (lyon point2 of
//!     twips_to_pixels). Flash `Transform.matrix.tx/ty` are also converted
//!     to pixels before being placed in the world matrix. Then a final
//!     pixels → NDC step maps screen pixels (origin top-left, Y down) to
//!     OpenGL clip space (-1..1, Y up).

use std::any::Any;
use std::borrow::Cow;
use std::cell::Cell;
use std::num::NonZeroU32;
use std::sync::Arc;

use ruffle_render::backend::{
    BitmapCacheEntry, Context3D, Context3DProfile, PixelBenderOutput, PixelBenderTarget,
    RenderBackend, ShapeHandle, ShapeHandleImpl, ViewportDimensions,
};
use ruffle_render::bitmap::{
    Bitmap, BitmapFormat, BitmapHandle, BitmapHandleImpl, BitmapSource, PixelRegion, PixelSnapping,
    RgbaBufRead, SyncHandle,
};
use ruffle_render::commands::{CommandHandler, CommandList, RenderBlendMode};
use ruffle_render::error::Error;
use ruffle_render::filters::{DisplacementMapFilter, DisplacementMapFilterMode, Filter};
use ruffle_render::matrix::Matrix;
use ruffle_render::pixel_bender::{
    PixelBenderShader, PixelBenderShaderHandle, PixelBenderShaderImpl,
};
use ruffle_render::pixel_bender_support::PixelBenderShaderArgument;

/// FlashNX: minimal `PixelBenderShaderImpl` that just carries the parsed shader.
/// The Switch GL backend can't compile or run PixelBender (same limitation as
/// Ruffle's own webgl backend), but returning a handle from
/// `compile_pixelbender_shader` instead of `Err` lets AVM2 `Shader` / `ShaderData`
/// / `ShaderFilter` construction SUCCEED. That matters because games build these
/// inside `enterFrame` / `click` handlers (e.g. The Terminal) — erroring there
/// aborts the handler and silently breaks input + game logic. With a real handle:
/// `run_pixelbender_shader` still errs (so `ShaderJob` no-ops cleanly) and the
/// renderer already skips `Filter::ShaderFilter` (`is_filter_supported` = false),
/// so the shader EFFECT is simply absent — the game keeps running normally.
#[derive(Debug)]
struct NoopPixelBenderShader {
    shader: PixelBenderShader,
}

impl PixelBenderShaderImpl for NoopPixelBenderShader {
    fn parsed_shader(&self) -> &PixelBenderShader {
        &self.shader
    }
}
use ruffle_render::quality::StageQuality;
use ruffle_render::shape_utils::{DistilledShape, GradientType};
use ruffle_render::tessellator::{DrawType, Gradient, ShapeTessellator};
use ruffle_render::transform::Transform;
use std::sync::atomic::{AtomicU16, AtomicUsize, Ordering};
use std::sync::Mutex;

/// Pause-menu labels rendered by `draw_menu_overlay`. C++ maps the selected
/// index from this slice to an action (Resume / Touches / Restart / Quit).
/// Keep the order in sync with the `MENU_*` constants in `cpp/src/main.cpp`.
// VITESSE moved into the in-game TOUCHES sub-menu (#20 Option 1), so it's no
// longer a top-level pause entry.
pub const MENU_ITEMS: &[&str] = &[
    "REPRENDRE",
    "TOUCHES",
    "ECRAN",
    "OVERCLOCK",
    "REDEMARRER",
    "QUITTER",
];

/// The ECRAN sub-panel's rows, in order. AFFICHAGE / ROTATION / FILTRE used to
/// sit at the top level, where they made the pause menu seven rows deep and put
/// three settings of the same nature on the same footing as QUITTER. They all
/// answer one question -- "this game does not sit right on my screen" -- and
/// they all preview on the frozen frame behind the panel, so they belong
/// together behind one row.
/// ZOOM sits between ROTATION and FILTRE: the first three are geometry, the
/// filter is colour.
/// PIXELS (issue #108) sits between the two groups because it belongs to
/// neither: it decides how the picture is RESAMPLED once the geometry has fixed
/// its size, so it reads as the last step of the geometry and the first thing
/// the eye judges. It goes directly under ZOOM because the free zoom is what
/// magnified the blur enough for a player to report it.
/// FPS sits last: the first three are geometry, the filter is colour, and this
/// one is neither -- it adds something to the screen rather than changing what
/// is already on it. It belongs on this panel all the same, because it obeys
/// ECRAN's membership rule: it previews on the frozen frame behind the panel
/// (`ruffle_redraw_paused` draws it), so pressing A shows you exactly what you
/// are turning on, on this game's own picture.
pub const SCREEN_ITEMS: &[&str] =
    &["AFFICHAGE", "ROTATION", "ZOOM", "PIXELS", "FILTRE", "FPS"];

/// Rows of `MENU_ITEMS` that open a panel beside the pause.
pub const PAUSE_ROW_TOUCHES: usize = 1;
pub const PAUSE_ROW_SCREEN: usize = 2;

/// What sits to the right of the pause rail, kept so B can slide it back under
/// the pause instead of making it vanish: the frame after B, nothing draws the
/// sub-panel any more but `draw_menu_overlay`, which redraws it from this.
#[derive(Clone, Copy, PartialEq)]
pub enum PauseChild {
    None,
    /// ECRAN, with its cursor.
    Screen(usize),
    /// Touches et curseur, with its cursor.
    Touches(usize),
}

static PAUSE_CHILD: std::sync::Mutex<PauseChild> = std::sync::Mutex::new(PauseChild::None);

pub fn pause_child_set(c: PauseChild) {
    if let Ok(mut g) = PAUSE_CHILD.lock() {
        *g = c;
    }
}

fn pause_child() -> PauseChild {
    PAUSE_CHILD.lock().map(|g| *g).unwrap_or(PauseChild::None)
}

/// A fresh pause: the main panel alone, no sub-panel to slide back.
pub fn pause_reset() {
    pause_child_set(PauseChild::None);
    kit::sheet::cascade_reset();
}

// ── Unified modal style ────────────────────────────────────────────────────
// One look for every centered window: `draw_modal_frame` draws the panel, title
// and hints from the kit (`render/kit/window.rs`), and only each window's body
// differs. Panels size their HEIGHT to the row count; the WIDTH picks one of two
// tiers below, because a single width made short pickers look bloated.
//
/// Standard picker width — pause, OPTIONS, sort, language, key dropdown, lists.
const MODAL_W: f32 = 520.0;
/// Wide panel — only where the body genuinely needs the room: the two-column
/// TOUCHES editor and the danger confirms (long warning / URL lines).
const MODAL_W_WIDE: f32 = 720.0;
/// The page behind a destructive confirm is darker than behind other windows.
const MODAL_DIM_DANGER: u32 = 0xD0_05_0A_16;
/// Vertical metrics. A window is PAD_TOP (title[+subtitle]) + rows*ROW_H +
/// PAD_BOTTOM (hints). Fixed-height confirms override the total.
const MODAL_ROW_H: f32 = 52.0;
/// Space above the first row WITH a subtitle vs WITHOUT: the tight one drops
/// the empty subtitle band so a window without one doesn't waste ~50 px.
const MODAL_PAD_TOP: f32 = 140.0;
const MODAL_PAD_TOP_TIGHT: f32 = 90.0;
const MODAL_PAD_BOTTOM: f32 = 60.0;
/// Left edge of a window's title and of the text in its rows.
const MODAL_ROW_X: f32 = 36.0;
/// Centre of a row below the y its callers hand around (the top of the old
/// 5x7 capitals), and the height of the focus pill drawn on it.
const MODAL_ROW_CY: f32 = 10.0;
const MODAL_PILL_H: f32 = 46.0;

/// Glide slots for the lists that draw their own rows instead of going through
/// `draw_modal_rows`.
const GLIDE_KEY_PAUSE: u32 = 52;
const GLIDE_KEY_SCREEN: u32 = 53;
const GLIDE_KEY_LANG: u32 = 50;
const GLIDE_KEY_KEYS: u32 = 56;
/// The key picker's cursor: its own slot, so it snaps to the key it opens on
/// instead of flying in from the pad row.
const GLIDE_KEY_KEYBOARD: u32 = 61;
const GLIDE_KEY_COVER: u32 = 55;
// 52 and 54 used to be the folder picker's; it derives its own keys from the
// path it is showing, so a step into another folder snaps instead of sliding.

// ── Controller badges ──────────────────────────────────────────────────────

/// Which glyph a pad slot's label (`keymap::PadSlot::glyph`) is drawn with,
/// and the caption beside it: "LS↑" is the left stick and an arrow, "LS" alone
/// the left stick pushed in and "press", "↑" the d-pad with its top arm lit and
/// an arrow. One table for every place a button is named (the editor, its key
/// picker, the before / after tables), so they cannot disagree on a glyph again:
/// the editor and the diff table each had their own copy, and fixing one would
/// have left the other's widths wrong.
fn pad_badge_parts(label: &str) -> (Option<Btn>, &str) {
    let single = match label {
        "A" => Some(Btn::A),
        "B" => Some(Btn::B),
        "X" => Some(Btn::X),
        "Y" => Some(Btn::Y),
        "L" => Some(Btn::L),
        "R" => Some(Btn::R),
        "ZL" => Some(Btn::ZL),
        "ZR" => Some(Btn::ZR),
        "SL" => Some(Btn::SL),
        "SR" => Some(Btn::SR),
        "+" => Some(Btn::Plus),
        "-" => Some(Btn::Minus),
        _ => None,
    };
    if single.is_some() {
        return (single, "");
    }
    let press = crate::loc_ui::ui().stick_press;
    match label {
        "LS" => return (Some(Btn::LStickPress), press),
        "RS" => return (Some(Btn::RStickPress), press),
        "\u{2191}" => return (Some(Btn::DpadUp), label),
        "\u{2193}" => return (Some(Btn::DpadDown), label),
        "\u{2190}" => return (Some(Btn::DpadLeft), label),
        "\u{2192}" => return (Some(Btn::DpadRight), label),
        _ => {}
    }
    if let Some(arrow) = label.strip_prefix("LS") {
        return (Some(Btn::LStick), arrow);
    }
    if let Some(arrow) = label.strip_prefix("RS") {
        return (Some(Btn::RStick), arrow);
    }
    (None, label)
}

/// A badge's glyph beside text at `px`: the glyph's circle fills most of its
/// em where a letter fills about 70%, so it is drawn larger to look the same
/// size.
fn pad_glyph_px(px: f32) -> f32 {
    (px * 1.35).round()
}

/// The caption beside a badge's glyph: the mockup's 13 px beside 15 px text.
fn pad_caption_px(px: f32) -> f32 {
    (px * 0.87).round().max(10.0)
}

const PAD_CAPTION_GAP: f32 = 3.0;

// ── The controls editor's window ───────────────────────────────────────────

/// One row of the controls editor: what the button sends in the open layer.
#[derive(Clone, PartialEq)]
pub(crate) enum PadValue {
    /// A key, a click or the console keyboard (a `keymap` key name).
    Key(std::string::String),
    /// Nothing.
    None,
    /// In a combo layer, a button the layer does not map: it sends its Normal
    /// key while the modifier is held (`None` inside when that is nothing too).
    Inherited(Option<std::string::String>),
    /// A modifier: it holds its layer and sends no key of its own.
    Modifier,
}

/// The controls editor as data, gathered by `menu.rs`.
pub(crate) struct PadView<'a> {
    /// Where the editor was opened from ("Pause", "Touches et curseur").
    pub crumbs: &'a [&'a str],
    /// Beside the title: the game's name, or "for every game".
    pub subject: &'a str,
    /// Indexes `keymap::PAD_SLOTS`.
    pub selection: usize,
    /// One per `keymap::PAD_SLOTS`.
    pub values: &'a [PadValue],
    pub player: u8,
    /// Indexes `keymap::SUBTAB_MODS`: 0 is Normal.
    pub layer: usize,
    /// The combo layers that hold a key: their tab carries a dot.
    pub live_layers: [bool; 5],
    /// The sticks (left, right) whose four direction rows read as one value,
    /// the mouse cursor they move.
    pub stick_cursor: [bool; 2],
    /// The cursor speed in force ("×1,5"), or None where it does not apply.
    pub cursor_speed: Option<&'a str>,
    /// Whether Y opens the shared profiles from here.
    pub profiles: bool,
    /// The line about the focused row.
    pub help: &'a str,
}

/// The key picker as data, gathered by `menu.rs`.
pub(crate) struct KeysView<'a> {
    /// The editor's path, then the editor.
    pub crumbs: &'a [&'a str],
    /// The edited button's pad label (`keymap::PadSlot::glyph`).
    pub glyph: &'a str,
    /// The open combo layer's button ("ZL"), None in the Normal layer.
    pub layer: Option<&'a str>,
    pub player: u8,
    /// What the button sends now in that layer.
    pub current: &'a PadValue,
    /// Indexes `keymap::KEYBOARD`.
    pub cursor: usize,
    /// The keys the other buttons send, with their pad labels.
    pub used: &'a std::collections::BTreeMap<std::string::String, std::vec::Vec<&'static str>>,
}

/// What the editor's touch table holds after its twenty-five rows: the two
/// player segments, the five layer segments, then the controls on the picture
/// (one per row, in `PAD_SLOTS` order).
pub(crate) const PAD_CELL_PLAYER: usize = crate::keymap::PAD_SLOTS.len();
pub(crate) const PAD_CELL_LAYER: usize = PAD_CELL_PLAYER + 2;
pub(crate) const PAD_CELL_ICON: usize = PAD_CELL_LAYER + 5;

/// The window: as wide as the screen less this margin, its content this far
/// in, its header and footer bands, its title (Touches.dc.html).
const TOUCHES_MARGIN: f32 = 16.0;
const TOUCHES_PAD_X: f32 = 28.0;
const TOUCHES_HEAD_H: f32 = 104.0;
const TOUCHES_FOOT_H: f32 = 60.0;
const TOUCHES_TITLE_PX: f32 = 24.0;
/// "None", the Minus caption: quieter than any value.
const TOUCHES_QUIET: u32 = 0xFF_5A_6A_8F;

/// The header's segmented switches.
const SEG_H: f32 = 36.0;
const SEG_INSET: f32 = 3.0;
const SEG_PAD: f32 = 12.0;
const SEG_PX: f32 = 14.0;
/// Room for the dot after a live layer's name.
const SEG_DOT_ROOM: f32 = 10.0;

/// Geometry of a drawn modal frame, returned by `draw_modal_frame` so the caller
/// can lay its body (rows or free text) inside the shared chrome.
#[derive(Clone, Copy)]
struct ModalFrame {
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    /// Distance from `y` to the first body row — `MODAL_PAD_TOP` with a subtitle,
    /// `MODAL_PAD_TOP_TIGHT` without (no empty subtitle gap).
    pad_top: f32,
}

impl ModalFrame {
    /// Y of the first body row (just below the title/subtitle band).
    fn rows_top(&self) -> f32 {
        self.y + self.pad_top
    }
}

/// 5×7 pixel glyphs for the pause menu. ASCII art keeps the data
/// hand-editable: each row is exactly 5 chars wide, ' ' = off, anything
/// else = on. `draw_text` upper-cases input before lookup, so we only
/// carry one case. Unknown chars render as blank (the cursor still
/// advances). Add more entries here if a future label needs new
/// characters.
type Glyph = [&'static str; 7];

// CJK / atlas-glyph layout knobs, in the same "units" the 5x7 bitmap font uses
// (1 unit = `scale` px; a bitmap glyph is 7 units tall, 6 units advance). CJK
// is rendered full-width: one square cell `CJK_ADVANCE_UNITS` wide, used by
// BOTH `draw_text` and `measure_text` so centring matches rendering. These two
// are the only things to tweak on hardware to align CJK with the bitmap font.
/// How long a grid cover takes to open or close, in milliseconds. Matched to
/// the selection frame's own travel so the art and the cursor settle together.
const GRID_COVER_MS: f32 = 190.0;

/// How far each tile's cover is open, on its own clock.
///
/// It used to be one pair, `(opening, closing, start)`: the tile being selected
/// and the one being left. Run the cursor across six covers and only the last
/// two moved; the four in between snapped shut the moment the next one was
/// chosen, which reads as a glitch. Now every tile eases toward open while it
/// is selected and toward closed when it is not, from wherever it was, so a
/// cover left half open finishes closing by itself. A tile is named by a hash
/// of what it shows (its file name, its URL), not by its index, so a re-sort
/// cannot hand one tile's motion to another.
struct TileOpen {
    /// Tick of the last frame that stepped the clocks, and the step it took.
    tick: u64,
    dt_ms: f32,
    /// `(key, t)` for every tile not at rest; `t` runs 0 (closed) to 1.
    open: std::vec::Vec<(u64, f32)>,
}

static TILE_OPEN: Mutex<TileOpen> = Mutex::new(TileOpen { tick: 0, dt_ms: 0.0, open: std::vec::Vec::new() });

/// Start a frame of tile animation: how long since the last one, capped so a
/// stall (a game run, a sleep) does not jump every tile to its end.
fn tile_open_frame() {
    let now = unsafe { ruffle_tick_now() };
    let freq = unsafe { ruffle_tick_freq() }.max(1) as f32;
    if let Ok(mut g) = TILE_OPEN.lock() {
        let dt = if g.tick == 0 { 0.0 } else { (now.saturating_sub(g.tick) as f32 * 1000.0 / freq).min(100.0) };
        g.tick = now;
        g.dt_ms = dt;
    }
}

/// A stable key for a tile: FNV-1a of what it shows.
fn tile_key(name: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in name.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}

/// Step tile `key` toward open (`selected`) or closed and return how open it
/// is, smoothstepped for display. Call once per tile per frame, after
/// `tile_open_frame`.
fn tile_open(key: u64, selected: bool) -> f32 {
    let Ok(mut g) = TILE_OPEN.lock() else {
        return if selected { 1.0 } else { 0.0 };
    };
    let step = g.dt_ms / GRID_COVER_MS;
    let pos = g.open.iter().position(|(k, _)| *k == key);
    let cur = pos.map_or(0.0, |i| g.open[i].1);
    let next = if selected { (cur + step).min(1.0) } else { (cur - step).max(0.0) };
    match pos {
        Some(i) if next <= 0.0 && !selected => {
            g.open.swap_remove(i);
        }
        Some(i) => g.open[i].1 = next,
        None if next > 0.0 || selected => g.open.push((key, next)),
        None => {}
    }
    next * next * (3.0 - 2.0 * next)
}

/// Forget every tile's motion: the next frame starts with all covers at rest
/// (a new layout, a new list).
pub fn tile_open_reset() {
    if let Ok(mut g) = TILE_OPEN.lock() {
        g.open.clear();
        g.tick = 0;
    }
}

/// Full-width cell width (and render size) for an atlas glyph.
const CJK_ADVANCE_UNITS: f32 = 8.0;
/// Baseline offset from the line top; raise/lower to sit CJK on the Latin line.
const CJK_BASELINE_UNITS: f32 = 6.6;

/// The 5x7 bitmap pattern for `ch`, if the font carries one.
///
/// Latin and Cyrillic are only carried in UPPER case, the case the whole
/// interface is drawn in, so a letter is folded before lookup. ASCII first,
/// then the Unicode fold that turns 'e-acute' into its capital and a lowercase
/// Cyrillic letter into its own: without that second step a French or Russian
/// game title fell through to the shared-font atlas and came out in full-width
/// CJK cells, a third too wide and in a different face than the rest of its own
/// name. A fold that yields more than one character (the German sharp s) is
/// refused -- dropping a letter is worse than a wide one.
fn bitmap_glyph(ch: char) -> Option<&'static Glyph> {
    let lookup = ch.to_ascii_uppercase();
    if let Some((_, g)) = GLYPHS.iter().find(|(c, _)| *c == lookup) {
        return Some(g);
    }
    if (ch as u32) >= 0x80 {
        let mut up = ch.to_uppercase();
        let first = up.next()?;
        if up.next().is_none() && first != ch {
            if let Some((_, g)) = GLYPHS.iter().find(|(c, _)| *c == first) {
                return Some(g);
            }
        }
    }
    None
}

/// Width one character takes on a line. The single source of truth for
/// `measure_text` and `wrap_point`, so a title can never wrap to a width it is
/// not drawn at.
fn char_advance(ch: char, scale: f32) -> f32 {
    if let Some(w) = kit::text::advance_px(ch, legacy_px(scale)) {
        return w;
    }
    if bitmap_glyph(ch).is_some() {
        6.0 * scale
    } else if (ch as u32) >= 0x80 {
        CJK_ADVANCE_UNITS * scale
    } else {
        // Unknown ASCII: drawn blank, but it still advances the pen.
        6.0 * scale
    }
}

/// Console-font pixel size for an old 5x7 `scale`.
///
/// 8.5 px per unit puts the old sizes on the kit's own scale: 1.8 (captions)
/// lands on 15, 2.0 (body) on 17, 3.0 (titles) on 26. Capitals then come out a
/// little shorter than the 7-unit 5x7 ones, which is what lower case needs to
/// not look shouted.
fn legacy_px(scale: f32) -> f32 {
    (scale * 8.5).round().max(9.0)
}

/// Characters from a script written without spaces, where a line may break
/// between any two of them. Ranges, not a list: this covers Han, kana, Hangul,
/// the CJK symbol and punctuation blocks and the full-width forms.
fn is_cjk_wrappable(c: char) -> bool {
    matches!(c as u32,
        0x1100..=0x11FF     // Hangul Jamo
        | 0x2E80..=0x2FFF   // CJK radicals, Kangxi
        | 0x3001..=0x303F   // CJK symbols and punctuation (past the ideographic space)
        | 0x3041..=0x33FF   // kana, Hangul compatibility jamo, CJK compatibility
        | 0x3400..=0x4DBF   // unified ideographs extension A
        | 0x4E00..=0x9FFF   // unified ideographs
        | 0xA000..=0xA4CF   // Yi
        | 0xAC00..=0xD7A3   // Hangul syllables
        | 0xF900..=0xFAFF   // compatibility ideographs
        | 0xFF01..=0xFF60   // full-width forms
        | 0xFFE0..=0xFFE6
        | 0x20000..=0x3FFFD // extensions B and beyond
    )
}

static GLYPHS: &[(char, Glyph)] = &[
    (' ', ["     ", "     ", "     ", "     ", "     ", "     ", "     "]),
    ('A', [" ### ", "#   #", "#   #", "#####", "#   #", "#   #", "#   #"]),
    ('B', ["#### ", "#   #", "#   #", "#### ", "#   #", "#   #", "#### "]),
    ('C', [" ####", "#    ", "#    ", "#    ", "#    ", "#    ", " ####"]),
    ('D', ["#### ", "#   #", "#   #", "#   #", "#   #", "#   #", "#### "]),
    ('E', ["#####", "#    ", "#    ", "#### ", "#    ", "#    ", "#####"]),
    ('F', ["#####", "#    ", "#    ", "#### ", "#    ", "#    ", "#    "]),
    ('G', [" ####", "#    ", "#    ", "#  ##", "#   #", "#   #", " ####"]),
    ('H', ["#   #", "#   #", "#   #", "#####", "#   #", "#   #", "#   #"]),
    ('I', [" ### ", "  #  ", "  #  ", "  #  ", "  #  ", "  #  ", " ### "]),
    ('J', ["#####", "    #", "    #", "    #", "    #", "#   #", " ### "]),
    ('K', ["#   #", "#  # ", "# #  ", "##   ", "# #  ", "#  # ", "#   #"]),
    ('L', ["#    ", "#    ", "#    ", "#    ", "#    ", "#    ", "#####"]),
    ('M', ["#   #", "## ##", "# # #", "#   #", "#   #", "#   #", "#   #"]),
    ('N', ["#   #", "##  #", "# # #", "#  ##", "#   #", "#   #", "#   #"]),
    ('O', [" ### ", "#   #", "#   #", "#   #", "#   #", "#   #", " ### "]),
    ('P', ["#### ", "#   #", "#   #", "#### ", "#    ", "#    ", "#    "]),
    ('Q', [" ### ", "#   #", "#   #", "#   #", "# # #", "#  # ", " ## #"]),
    ('R', ["#### ", "#   #", "#   #", "#### ", "# #  ", "#  # ", "#   #"]),
    ('S', [" ####", "#    ", "#    ", " ### ", "    #", "    #", "#### "]),
    ('T', ["#####", "  #  ", "  #  ", "  #  ", "  #  ", "  #  ", "  #  "]),
    ('U', ["#   #", "#   #", "#   #", "#   #", "#   #", "#   #", " ### "]),
    ('V', ["#   #", "#   #", "#   #", "#   #", "#   #", " # # ", "  #  "]),
    ('W', ["#   #", "#   #", "#   #", "#   #", "# # #", "## ##", "#   #"]),
    ('X', ["#   #", "#   #", " # # ", "  #  ", " # # ", "#   #", "#   #"]),
    ('Y', ["#   #", "#   #", " # # ", "  #  ", "  #  ", "  #  ", "  #  "]),
    ('Z', ["#####", "    #", "   # ", "  #  ", " #   ", "#    ", "#####"]),
    ('0', [" ### ", "#   #", "#  ##", "# # #", "##  #", "#   #", " ### "]),
    ('1', ["  #  ", " ##  ", "  #  ", "  #  ", "  #  ", "  #  ", " ### "]),
    ('2', [" ### ", "#   #", "    #", "   # ", "  #  ", " #   ", "#####"]),
    ('3', [" ### ", "#   #", "    #", "  ## ", "    #", "#   #", " ### "]),
    ('4', ["#  # ", "#  # ", "#  # ", "#####", "   # ", "   # ", "   # "]),
    ('5', ["#####", "#    ", "#### ", "    #", "    #", "#   #", " ### "]),
    ('6', [" ### ", "#    ", "#    ", "#### ", "#   #", "#   #", " ### "]),
    ('7', ["#####", "    #", "   # ", "  #  ", " #   ", " #   ", " #   "]),
    ('8', [" ### ", "#   #", "#   #", " ### ", "#   #", "#   #", " ### "]),
    ('9', [" ### ", "#   #", "#   #", " ####", "    #", "    #", " ### "]),
    ('-', ["     ", "     ", "     ", "#####", "     ", "     ", "     "]),
    ('_', ["     ", "     ", "     ", "     ", "     ", "     ", "#####"]),
    ('=', ["     ", "     ", "#####", "     ", "#####", "     ", "     "]),
    ('>', ["#    ", " #   ", "  #  ", "   # ", "  #  ", " #   ", "#    "]),
    (':', ["     ", "  #  ", "  #  ", "     ", "  #  ", "  #  ", "     "]),
    ('.', ["     ", "     ", "     ", "     ", "     ", " ##  ", " ##  "]),
    ('/', ["    #", "    #", "   # ", "  #  ", " #   ", "#    ", "#    "]),
    // Punctuation (previously missing — rendered blank, e.g. "SUPPRIMER ?").
    (',', ["     ", "     ", "     ", "     ", "  ## ", "  #  ", " #   "]),
    ('\'', ["  #  ", "  #  ", " #   ", "     ", "     ", "     ", "     "]),
    ('?', [" ### ", "#   #", "    #", "   # ", "  #  ", "     ", "  #  "]),
    ('!', ["  #  ", "  #  ", "  #  ", "  #  ", "  #  ", "     ", "  #  "]),
    ('(', ["   # ", "  #  ", " #   ", " #   ", " #   ", "  #  ", "   # "]),
    (')', [" #   ", "  #  ", "   # ", "   # ", "   # ", "  #  ", " #   "]),
    ('[', [" ### ", " #   ", " #   ", " #   ", " #   ", " #   ", " ### "]),
    (']', [" ### ", "   # ", "   # ", "   # ", "   # ", "   # ", " ### "]),
    ('<', ["   # ", "  #  ", " #   ", "#    ", " #   ", "  #  ", "   # "]),
    ('+', ["     ", "  #  ", "  #  ", "#####", "  #  ", "  #  ", "     "]),
    ('%', ["##  #", "##  #", "   # ", "  #  ", " #   ", "#  ##", "#  ##"]),
    ('&', [" ##  ", "#  # ", "#  # ", " ##  ", "# # #", "#  # ", " ## #"]),
    // Keyboard-picker symbols (issue #55) that were missing from the font.
    (';', ["     ", "  #  ", "  #  ", "     ", "  ## ", "  #  ", " #   "]),
    ('\\', ["#    ", "#    ", " #   ", "  #  ", "   # ", "    #", "    #"]),
    ('`', [" #   ", "  #  ", "     ", "     ", "     ", "     ", "     "]),
    ('*', ["     ", "# # #", " ### ", "#####", " ### ", "# # #", "     "]),
    // Group separator for the facts line. Two columns, not one: a single column
    // is 1.8 px at scale 1.8 in a muted colour — detectable on a TV across a
    // room, not reliable. Two cost nothing, the advance stays 6 units either way.
    // Rows 0 and 6 are blank ON PURPOSE: capitals fill all seven rows, so a
    // full-height bar would have exactly a capital's extent and read as one more
    // letter in the word. Cut to five it reads as furniture — the same reasoning
    // as the LISTE row's colour chip, a mark smaller than what it separates.
    ('|', ["     ", "  ## ", "  ## ", "  ## ", "  ## ", "  ## ", "     "]),
    // Degree sign, for the rotation labels. Without it this ONE character fell
    // through to the shared system font -- the same path Chinese takes -- which
    // costs over 130 MB to load, delays the panel that first shows it, and is
    // fatal in applet mode. One glyph here instead of a font dependency for the
    // word "90 degrees".
    ('\u{00B0}', [" ##  ", "#  # ", " ##  ", "     ", "     ", "     ", "     "]),
    // Arrows, for the d-pad and stick directions on the TOUCHES pad view.
    // Same reasoning as the degree sign above: without them these four fall
    // through to the shared system font, which is fatal in applet mode. The
    // pad names a direction two dozen times on one panel, so it is exactly
    // the screen that must not depend on a 130 MB font load.
    ('\u{2191}', ["  #  ", " ### ", "# # #", "  #  ", "  #  ", "  #  ", "  #  "]),
    ('\u{2193}', ["  #  ", "  #  ", "  #  ", "  #  ", "# # #", " ### ", "  #  "]),
    ('\u{2190}', ["     ", "  #  ", " #   ", "#####", " #   ", "  #  ", "     "]),
    ('\u{2192}', ["     ", "  #  ", "   # ", "#####", "   # ", "  #  ", "     "]),
    ('\u{2026}', ["     ", "     ", "     ", "     ", "     ", "     ", "# # #"]), // …
    // Accented uppercase Latin (French + Spanish). The letter body is
    // compressed to 6 rows so the diacritic fits on row 0.
    ('\u{00C9}', ["  ## ", "     ", "#####", "#    ", "#### ", "#    ", "#####"]), // É
    ('\u{00C8}', [" ##  ", "     ", "#####", "#    ", "#### ", "#    ", "#####"]), // È
    ('\u{00CA}', [" # # ", "     ", "#####", "#    ", "#### ", "#    ", "#####"]), // Ê
    ('\u{00C0}', [" ##  ", "     ", " ### ", "#   #", "#####", "#   #", "#   #"]), // À
    ('\u{00C1}', ["  ## ", "     ", " ### ", "#   #", "#####", "#   #", "#   #"]), // Á
    ('\u{00CD}', ["  ## ", "     ", " ### ", "  #  ", "  #  ", "  #  ", " ### "]), // Í
    ('\u{00D3}', ["  ## ", "     ", " ### ", "#   #", "#   #", "#   #", " ### "]), // Ó
    ('\u{00DA}', ["  ## ", "     ", "#   #", "#   #", "#   #", "#   #", " ### "]), // Ú
    ('\u{00D1}', [" ### ", "     ", "#   #", "##  #", "# # #", "#  ##", "#   #"]), // Ñ
    ('\u{00C7}', [" ####", "#    ", "#    ", "#    ", "#    ", " ####", "  #  "]), // Ç (cedilla below)
    ('\u{00BF}', ["  #  ", "     ", "  #  ", " #   ", "#    ", "#   #", " ### "]), // ¿
    ('\u{00A1}', ["  #  ", "     ", "  #  ", "  #  ", "  #  ", "  #  ", "  #  "]), // ¡
    // Umlaut / circumflex / tilde / grave uppercase Latin for German, Italian
    // and Portuguese. Same compressed-body convention as the French/Spanish
    // accents above (diacritic on row 0, blank row 1, letter body on rows 2-6).
    // The umlaut and circumflex share the row-0 " # # " mark: no single locale
    // uses both an umlaut-X and a circumflex-X, so the glyphs never collide
    // on screen (German Ä vs Portuguese Â, etc.).
    ('\u{00C4}', [" # # ", "     ", " ### ", "#   #", "#####", "#   #", "#   #"]), // Ä
    ('\u{00D6}', [" # # ", "     ", " ### ", "#   #", "#   #", "#   #", " ### "]), // Ö
    ('\u{00DC}', [" # # ", "     ", "#   #", "#   #", "#   #", "#   #", " ### "]), // Ü
    ('\u{00C2}', [" # # ", "     ", " ### ", "#   #", "#####", "#   #", "#   #"]), // Â
    ('\u{00D4}', [" # # ", "     ", " ### ", "#   #", "#   #", "#   #", " ### "]), // Ô
    ('\u{00C3}', [" ### ", "     ", " ### ", "#   #", "#####", "#   #", "#   #"]), // Ã
    ('\u{00D5}', [" ### ", "     ", " ### ", "#   #", "#   #", "#   #", " ### "]), // Õ
    ('\u{00CC}', [" ##  ", "     ", " ### ", "  #  ", "  #  ", "  #  ", " ### "]), // Ì
    ('\u{00D2}', [" ##  ", "     ", " ### ", "#   #", "#   #", "#   #", " ### "]), // Ò
    ('\u{00D9}', [" ##  ", "     ", "#   #", "#   #", "#   #", "#   #", " ### "]), // Ù
    // Turkish uppercase: breve-G (Ğ), cedilla-S (Ş, mirrors Ç), dotted-I (İ).
    ('\u{011E}', [" ### ", "     ", " ####", "#    ", "#  ##", "#   #", " ####"]), // Ğ
    ('\u{015E}', [" ####", "#    ", " ### ", "    #", "    #", "#### ", "  #  "]), // Ş
    ('\u{0130}', ["  #  ", "     ", " ### ", "  #  ", "  #  ", "  #  ", " ### "]), // İ
    // Cyrillic uppercase (Russian locale). draw_text does not case-fold
    // non-ASCII, so RU strings are written uppercase to hit these directly.
    ('\u{0410}', [" ### ", "#   #", "#   #", "#####", "#   #", "#   #", "#   #"]), // А
    ('\u{0411}', ["#####", "#    ", "#    ", "#### ", "#   #", "#   #", "#### "]), // Б
    ('\u{0412}', ["#### ", "#   #", "#   #", "#### ", "#   #", "#   #", "#### "]), // В
    ('\u{0413}', ["#####", "#    ", "#    ", "#    ", "#    ", "#    ", "#    "]), // Г
    ('\u{0414}', [" ####", " #  #", " #  #", " #  #", " #  #", "#####", "#   #"]), // Д
    ('\u{0415}', ["#####", "#    ", "#    ", "#### ", "#    ", "#    ", "#####"]), // Е
    ('\u{0416}', ["# # #", "# # #", " ### ", "  #  ", " ### ", "# # #", "# # #"]), // Ж
    ('\u{0417}', [" ### ", "#   #", "    #", "  ## ", "    #", "#   #", " ### "]), // З
    ('\u{0418}', ["#   #", "#  ##", "# # #", "##  #", "#   #", "#   #", "#   #"]), // И
    ('\u{0419}', [" ### ", "#   #", "#  ##", "# # #", "##  #", "#   #", "#   #"]), // Й
    ('\u{041A}', ["#   #", "#  # ", "# #  ", "##   ", "# #  ", "#  # ", "#   #"]), // К
    ('\u{041B}', ["  ###", "  # #", "  # #", "  # #", " ## #", " #  #", "#   #"]), // Л
    ('\u{041C}', ["#   #", "## ##", "# # #", "#   #", "#   #", "#   #", "#   #"]), // М
    ('\u{041D}', ["#   #", "#   #", "#   #", "#####", "#   #", "#   #", "#   #"]), // Н
    ('\u{041E}', [" ### ", "#   #", "#   #", "#   #", "#   #", "#   #", " ### "]), // О
    ('\u{041F}', ["#####", "#   #", "#   #", "#   #", "#   #", "#   #", "#   #"]), // П
    ('\u{0420}', ["#### ", "#   #", "#   #", "#### ", "#    ", "#    ", "#    "]), // Р
    ('\u{0421}', [" ####", "#    ", "#    ", "#    ", "#    ", "#    ", " ####"]), // С
    ('\u{0422}', ["#####", "  #  ", "  #  ", "  #  ", "  #  ", "  #  ", "  #  "]), // Т
    ('\u{0423}', ["#   #", "#   #", " ####", "    #", "    #", "   # ", " ##  "]), // У
    ('\u{0424}', ["  #  ", " ### ", "# # #", "# # #", "# # #", " ### ", "  #  "]), // Ф
    ('\u{0425}', ["#   #", "#   #", " # # ", "  #  ", " # # ", "#   #", "#   #"]), // Х
    ('\u{0426}', ["#   #", "#   #", "#   #", "#   #", "#   #", "#####", "    #"]), // Ц
    ('\u{0427}', ["#   #", "#   #", "#   #", " ####", "    #", "    #", "    #"]), // Ч
    ('\u{0428}', ["# # #", "# # #", "# # #", "# # #", "# # #", "# # #", "#####"]), // Ш
    ('\u{0429}', ["# # #", "# # #", "# # #", "# # #", "# # #", "#####", "    #"]), // Щ
    ('\u{042A}', ["##   ", " #   ", " #   ", " ### ", " #  #", " #  #", " ### "]), // Ъ
    ('\u{042B}', ["#   #", "#   #", "#   #", "##  #", "# # #", "# # #", "## ##"]), // Ы
    ('\u{042C}', ["#    ", "#    ", "#    ", "#### ", "#   #", "#   #", "#### "]), // Ь
    ('\u{042D}', [" ### ", "#   #", "    #", "  ###", "    #", "#   #", " ### "]), // Э
    ('\u{042E}', ["#  # ", "# # #", "# # #", "# # #", "# # #", "# # #", "#  # "]), // Ю
    ('\u{042F}', [" ####", "#   #", "#   #", " ####", "  # #", " #  #", "#   #"]), // Я
];

// The facts line speaks in two voices, and each carries meaning rather than
// decoration. VALUE is the blue the line already used, so nothing that IS a
// fact changed colour. MUTED is for everything that is NOT a fact — the
// separators, and the word saying what the playtime number counts — so the
// distance between two groups stops being quantitative (one space versus
// three, which the eye cannot rank) and becomes qualitative: ink or no ink.
// The engine used to be amber here, quoting an amber badge on the tile above.
// Both are gone: an engine name is a fact like the others and gets the others'
// colour. Whether a game runs stopped depending on which one it was written in.
/// Quarter-turns CLOCKWISE applied to the game's picture, 0 to 3 (issue #78).
///
/// The console cannot turn its screen, so this turns the picture instead: the
/// player holds the Switch on its side. It is the game's answer to the same
/// problem the display modes answer for aspect ratio -- a portrait game like
/// Flappy Bird (500 by 700) uses a third of a 16:9 screen, and no amount of
/// scaling fixes that, only turning does.
///
/// Kept here, next to the one place every draw's matrix is built, because that
/// is the only place it can be applied ONCE and be right for everything: the
/// game, the pause panel over it, the pointer, the screen filter. Anything that
/// rotated separately would drift from the rest.
static GAME_ROTATION: core::sync::atomic::AtomicU8 = core::sync::atomic::AtomicU8::new(0);

pub fn set_game_rotation(quarters: u8) {
    GAME_ROTATION.store(quarters % 4, core::sync::atomic::Ordering::Relaxed);
}

pub fn game_rotation() -> u8 {
    GAME_ROTATION.load(core::sync::atomic::Ordering::Relaxed)
}

/// True when the picture is turned onto its side, so the logical viewport is
/// portrait while the framebuffer stays landscape.
pub fn rotation_swaps_axes() -> bool {
    matches!(game_rotation(), 1 | 3)
}

/// Folder the home is currently showing (issue #68); `None` = all of them.
///
/// A static rather than a tenth argument through all four view renderers. It is
/// read in exactly ONE place -- `draw_home_header`, to name the open shelf in
/// the slot the active filter already uses -- and threading it would have meant
/// four signatures and a dispatch tuple changed to move one line of text. The
/// four views themselves never learn folders exist: they are handed the games
/// that survived the filter, as they always were.
static HOME_FOLDER: std::sync::Mutex<Option<std::string::String>> =
    std::sync::Mutex::new(None);

/// Whether the library has any folder at all, so the header can ADVERTISE the
/// shoulder buttons. Nothing on the home says folders exist otherwise, and an
/// unadvertised button is a feature nobody finds.
static HOME_HAS_FOLDERS: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

pub fn set_home_folder(folder: Option<&str>) {
    if let Ok(mut g) = HOME_FOLDER.lock() {
        // Compared before writing: this is called once per gallery frame, and
        // the shelf changes about once a minute. Two allocations a frame for a
        // string that is almost always the same one is not free at 60 Hz.
        if g.as_deref() != folder {
            *g = folder.map(|s| s.to_string());
        }
    }
}

pub fn set_home_has_folders(any: bool) {
    HOME_HAS_FOLDERS.store(any, core::sync::atomic::Ordering::Relaxed);
}

/// Size of the WHOLE library, for the one line that needs to compare a shelf to
/// it. Everything else on screen counts what is in front of the player.
static HOME_LIBRARY_TOTAL: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);

pub fn set_home_library_total(n: usize) {
    HOME_LIBRARY_TOTAL.store(n, core::sync::atomic::Ordering::Relaxed);
}

/// Free zoom on the game's picture, in percent of the fitted size (issue #101),
/// with the framing offset that goes with it, in PHYSICAL screen pixels.
///
/// The three display modes decide how the stage fills the screen and can do
/// nothing about a margin baked into the stage itself: a 800x600 game whose
/// action happens in the middle 400x300 stays small in all three. This
/// magnifies the picture on top of whichever mode is set, and the pan says
/// which part of it to keep.
///
/// Applied in `world_matrix` right AFTER the quarter-turn, which is what makes
/// the pan physical: pushing the stick right moves the picture right on the
/// screen, whatever the rotation. Applying it before would have tied the axes
/// to the turned frame, so a turned picture would have panned sideways.
///
/// The GAME LAYER ONLY, unlike the rotation. A turned console means the player
/// is holding the Switch sideways, so the panel and the pointer turn with it; a
/// magnified picture means nothing of the sort, and a pause panel that grew with
/// the zoom would be unreadable at 400%.
static GAME_ZOOM: core::sync::atomic::AtomicU16 = core::sync::atomic::AtomicU16::new(100);
static GAME_PAN_X: core::sync::atomic::AtomicI32 = core::sync::atomic::AtomicI32::new(0);
static GAME_PAN_Y: core::sync::atomic::AtomicI32 = core::sync::atomic::AtomicI32::new(0);

/// 100 = fitted, the floor. Below it the picture would only gain black bars,
/// which is of use to exactly one setup (a TV that eats its own edges) and of
/// none to anyone else.
pub const ZOOM_MIN: u16 = 100;
pub const ZOOM_MAX: u16 = 500;

pub fn game_zoom_percent() -> u16 {
    GAME_ZOOM.load(core::sync::atomic::Ordering::Relaxed).clamp(ZOOM_MIN, ZOOM_MAX)
}

pub fn game_pan() -> (i32, i32) {
    (
        GAME_PAN_X.load(core::sync::atomic::Ordering::Relaxed),
        GAME_PAN_Y.load(core::sync::atomic::Ordering::Relaxed),
    )
}

/// Set the zoom and its framing. The pan is clamped so the magnified picture
/// always covers the screen: past that edge there is nothing to show but the
/// clear colour, and a player who panned into it would think the game had
/// crashed.
pub fn set_game_zoom(percent: u16, pan_x: i32, pan_y: i32, screen_w: f32, screen_h: f32) {
    let z = percent.clamp(ZOOM_MIN, ZOOM_MAX);
    let (cx, cy) = pan_limits(z, screen_w, screen_h);
    GAME_ZOOM.store(z, core::sync::atomic::Ordering::Relaxed);
    GAME_PAN_X.store(pan_x.clamp(-cx, cx), core::sync::atomic::Ordering::Relaxed);
    GAME_PAN_Y.store(pan_y.clamp(-cy, cy), core::sync::atomic::Ordering::Relaxed);
}

/// How far the framing may travel on each axis at `percent`, in screen pixels:
/// half the slack the magnification created. Zero at 100%, where there is
/// nothing outside the screen to go looking for.
pub fn pan_limits(percent: u16, screen_w: f32, screen_h: f32) -> (i32, i32) {
    let z = percent.clamp(ZOOM_MIN, ZOOM_MAX) as f32 / 100.0;
    (
        ((screen_w * (z - 1.0)) * 0.5).max(0.0) as i32,
        ((screen_h * (z - 1.0)) * 0.5).max(0.0) as i32,
    )
}

// Scrollbar palette, one copy for the seven places that spelled it out inline.
// They drifted exactly where an inline copy always drifts: the Flashpoint
// gallery's track came out white where every other one is slate, and three of
// the seven clamp their thumb 4 px shorter than the rest. Differences nobody
// decided, only inherited from whichever site happened to be open that day. The
// thumb is the selection amber on purpose: on every screen it is the cursor,
// seen from the edge of the page.
const SCROLLBAR_W: f32 = 4.0;
const SCROLLBAR_MIN_THUMB: f32 = 24.0;


/// Count of `GpuDraw`s currently alive (created minus dropped). Used to
/// detect leaks: if this monotonically grows (and matches `shapes_registered`
/// minus shape Drops), Ruffle is retaining shape handles forever and our
/// VBO/VAO/IBO pool fills up — exactly the suspected cause of the jetpack
/// crash (rocket nozzle particle system emits a new shape per frame, never
/// freed, until Mesa's bind table walks off the end and faults).
static LIVE_GPU_DRAWS: AtomicUsize = AtomicUsize::new(0);
/// Count of `GpuShape`s currently alive (created minus dropped). Should
/// roughly track `register_shape` calls if Ruffle never drops handles.
static LIVE_GPU_SHAPES: AtomicUsize = AtomicUsize::new(0);

// ─── Mega-buffer arena ─────────────────────────────────────────────────────
//
// Mario 63 + rocket nozzle = ~3 new shapes per frame, never freed by Ruffle
// for several seconds. Each shape used to create its own VBO + IBO + VAO,
// and Mesa-NVK on Tegra X1 segfaults inside `glBindBuffer` once we exceed
// ~27 000 simultaneously-live GL buffer handles (we caught it twice:
// x24=GL_ARRAY_BUFFER, FAR a poisoned slot pointer at offset 0x50 of a
// table, then a small index 0x1011 — Mesa's internal buffer slot table
// has a finite size which we walked off the end of).
//
// The fix is to stop creating GL objects per shape entirely: allocate one
// huge VBO and one huge IBO at boot, then suballocate ranges out of those
// for each shape via a freelist. From Mesa's point of view there are only
// ~5 GL handles total, no matter how many Ruffle shapes pile up.
//
// `glDrawElementsBaseVertex` lets us pack many shapes into a single VBO
// while letting each shape keep its own local 0..N index numbering: the
// driver shifts every fetched index by `base_vertex` before reading.
//
// Sizing: at the crash we had ~14 MB of vertex data + ~3 MB of indices
// in flight. We size for ~4x headroom so a long Mario 63 session has
// plenty of slack.
// Bumped 64 → 192 MB after The Binding of Isaac (2026-06-14): ~5170 live vector
// shapes peaked at ~114 MB of vertices, OOMing the 64 MB arena — alloc() then
// returned None, build_gpu_draw dropped the draw, and the art went invisible.
// 192 MB gives ~1.7× headroom over the observed peak.
// Bumped 192 → 384 MB after Infiltrating the Airship (#56, 2026-07-02): this
// Henry Stickmin game accumulates ~19 100 unique vector shapes (thousands of
// hand-drawn frames, never released by Ruffle), which filled the 192 MB arena at
// ~frame 360 (~10 000 shapes) and then DROPPED every further draw (arenaDropV
// climbed to ~9 100) — the classic white/blank screen. The arena free-lists on
// handle drop, but Ruffle keeps them all live, so the only lever is capacity. The
// `ram=used/total` heartbeat is the crt0-RESERVED heap (not live use), so there's
// headroom in the 3.2 GB title heap to hold the full working set.
const ARENA_VBO_SIZE: GLsizeiptr = 384 * 1024 * 1024;  // 384 MB
// Bumped 96 → 192 MB at the same time: index data scaled the same ~2× (peaked at
// ~93 MB for the first ~10 000 shapes, so ~178 MB for all ~19 100).
const ARENA_IBO_SIZE: GLsizeiptr = 192 * 1024 * 1024;  // 192 MB

// ─── Arena sized per game ──
//
// Those 576 MB are reserved for the worst game known, and they come straight out
// of malloc, which is what runs out: Super Smash Flash 2 uses 25 + 11 MB of them
// and dies of memory exhaustion loading a fight, while the largest peak in every
// September log is 45 MB. So each game records the peak it reached in
// `<game>.swf.arena` (beside the .swf, like `.url`), and its next launch reserves
// four times that, never less than the floors below, never more than the default.
// A game with no record (first launch, or one that never passed 4 MB) gets the
// default. An arena that overflows marks the record `full`, and that game gets
// the default from then on: one session of dropped draws at worst, never twice.
// Floors lowered from 128 / 64 MB (2026-10-06): four times the recorded peak
// is the margin; the floors only matter to games whose peak is small, and they
// held 192 MB for Super Smash Flash 2's 25 + 11 MB, on a heap it fills.
const ARENA_RECORD_FLOOR_V: GLsizeiptr = 48 * 1024 * 1024;
const ARENA_RECORD_FLOOR_I: GLsizeiptr = 24 * 1024 * 1024;

/// `(vertex MB, index MB, full)` from `<game>.swf.arena`, e.g. `v=25 i=11 full=0`.
fn read_arena_record(path: &str) -> Option<(u64, u64, bool)> {
    use std::io::Read;
    // A bounded loop, not `read_to_string`: on Horizon that preallocates from
    // fstat and has failed with OutOfMemory on files of a few bytes.
    let mut file = std::fs::File::open(path).ok()?;
    let mut buf = [0u8; 128];
    let n = file.read(&mut buf).ok()?;
    let text = core::str::from_utf8(&buf[..n]).ok()?;
    let (mut v, mut i, mut full) = (None, None, false);
    for field in text.split_whitespace() {
        match field.split_once('=') {
            Some(("v", x)) => v = x.parse().ok(),
            Some(("i", x)) => i = x.parse().ok(),
            Some(("full", x)) => full = x == "1",
            _ => {}
        }
    }
    Some((v?, i?, full))
}

fn write_arena_record(path: &str, v_mb: u64, i_mb: u64, full: bool) {
    let body = std::format!("v={} i={} full={}\n", v_mb, i_mb, full as u8);
    let mut path_c = path.as_bytes().to_vec();
    path_c.push(0);
    unsafe {
        swf_picker_write_file(
            path_c.as_ptr() as *const core::ffi::c_char,
            body.as_ptr(),
            body.len() as u32,
        );
    }
}

/// Arena sizes for a game with this record: 4x its peak, floored, capped.
fn arena_sizes_for(record: Option<(u64, u64, bool)>) -> (GLsizeiptr, GLsizeiptr) {
    match record {
        Some((v_mb, i_mb, false)) => {
            let mb = 1024 * 1024;
            (
                (4 * v_mb as GLsizeiptr * mb).clamp(ARENA_RECORD_FLOOR_V, ARENA_VBO_SIZE),
                (4 * i_mb as GLsizeiptr * mb).clamp(ARENA_RECORD_FLOOR_I, ARENA_IBO_SIZE),
            )
        }
        _ => (ARENA_VBO_SIZE, ARENA_IBO_SIZE),
    }
}
/// VBO alignment = one full vertex (pos.xy + rgba = 6 × f32 = 24 bytes).
/// MUST match the vertex stride so `glDrawElementsBaseVertex(base_vertex)`
/// can use `vbo_offset / 24` and land exactly on a vertex boundary.
/// First mega-arena attempt used 16-byte alignment (a power of two for
/// `&!(align-1)` rounding) — that produced offsets like 48, 64, 80 which
/// are NOT multiples of 24, so base_vertex was off by fractional vertices
/// and Mario 63 rendered as a corrupted mess. Round-up logic switched to
/// the generic `((x + a - 1) / a) * a` to allow non-power-of-2 alignments.
const ARENA_VBO_ALIGN: GLsizeiptr = 24;
/// IBO alignment = sizeof(u32). `glDrawElementsBaseVertex`'s `indices` byte
/// offset must be aligned to the index type (4 bytes for GL_UNSIGNED_INT).
const ARENA_IBO_ALIGN: GLsizeiptr = 4;

/// A texture write kept back until something actually reads that texture.
///
/// Ruffle decodes a BURST of video frames per tick whenever it is behind, and it
/// is behind on any game with video (measured: `tick=3929ms` for one second of
/// wall clock). Every decoded frame calls `update_texture`, so five or six full
/// 1280x720 uploads land per displayed frame and all but the last are overwritten
/// before anything can sample them. The driver keeps a transfer buffer for each:
/// measured on hardware, 936 MB of process memory disappeared OUTSIDE the heap in
/// one session (`malloc` ceiling 2088 -> 1152 MB while the heap held steady at
/// ~1000 MB), until a 3.7 MB allocation could not grow the heap and the app
/// aborted mid-scene.
///
/// Only the last write of a frame is observable, so only the last one is sent.
/// `data` is reused between frames, which also removes the per-frame multi-MB
/// allocation the conversion used to make.
struct PendingUpload {
    /// Standalone GL texture; 0 when the target is a region of an atlas.
    texture: GLuint,
    /// Keeps that standalone texture ALIVE until the write is sent.
    ///
    /// The write is now deferred to the start of the next frame, so between
    /// holding it and flushing it Ruffle can free the BitmapData -- and
    /// `StandaloneTexture::drop` calls `glDeleteTextures` straight away. The
    /// flush would then write to a dead name: silently ignored at best, and at
    /// worst `glGenTextures` has already recycled that name for something
    /// created in between, so the rectangle lands in an unrelated texture.
    /// Holding the `Arc` closes the window; `None` on the atlas path, whose
    /// atlases outlive the frame anyway.
    keep: Option<std::sync::Arc<StandaloneTexture>>,
    /// Atlas holding the target region, when `texture` is 0.
    atlas_index: usize,
    /// Destination rectangle, in the target texture's own pixels.
    dst_x: u32,
    dst_y: u32,
    w: u32,
    h: u32,
    /// Tightly packed rows of `w` pixels — the source stride is normalised on
    /// the way in, so the flush never needs GL_UNPACK_ROW_LENGTH.
    data: Vec<u8>,
}

struct BufferArena {
    gl_id: GLuint,
    target: GLenum,
    capacity: GLsizeiptr,
    /// Alignment for allocations in this arena (24 for vertex, 4 for index).
    align: GLsizeiptr,
    /// Free segments, sorted by offset, adjacent ones coalesced.
    free: Vec<(GLintptr, GLsizeiptr)>,
    /// High-water diagnostic: max bytes ever in use simultaneously.
    peak_in_use: GLsizeiptr,
    /// Failed-allocation diagnostic: log the first OOM in detail (once)…
    oom_warned: bool,
    /// …and keep a running count of dropped allocations, surfaced every
    /// heartbeat as `arenaDrop*`. A silent first-only warn_once is what hid the
    /// Binding of Isaac invisible-art bug for nine debug cycles (2026-06-14):
    /// once the 64 MB vertex arena filled, every subsequent shape's draw was
    /// dropped with no further trace. Keep this LOUD.
    alloc_failures: u32,
    /// FlashNX (2026-10-06): regions freed since the last fence, then batches
    /// of them each behind the fence (a `GLsync`, as an address) put after
    /// the last command that could read them. A region goes back to `free`
    /// only once its fence has signalled, which is what lets `upload` write
    /// without waiting for the GPU (see there).
    retired: Vec<(GLintptr, GLsizeiptr)>,
    quarantine: std::collections::VecDeque<(usize, Vec<(GLintptr, GLsizeiptr)>)>,
    quarantined_bytes: GLsizeiptr,
    /// Allocations that found the arena full and waited for a quarantined
    /// batch (`gpu:` line). Expected 0 outside of a full arena.
    quarantine_waits: u32,
}

/// FlashNX: writes into the shape arenas without waiting for the GPU (see
/// `BufferArena::upload`). Off from the `unsyncvbo.off` marker, to compare.
static UNSYNC_ARENA: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

pub fn set_unsync_arena(enabled: bool) {
    UNSYNC_ARENA.store(enabled, std::sync::atomic::Ordering::Relaxed);
}

impl BufferArena {
    fn new(target: GLenum, capacity: GLsizeiptr, align: GLsizeiptr) -> Self {
        let mut gl_id: GLuint = 0;
        unsafe {
            glGenBuffers(1, &mut gl_id);
            glBindBuffer(target, gl_id);
            // STATIC, not DYNAMIC. Measured: the two arenas cost 576 MB of
            // malloc, exactly their size, on a heap that runs out around 1.07 GB
            // — this GL keeps a CPU shadow of a DYNAMIC buffer, and we were
            // spending half of everything on two buffers that many games never
            // write a byte into. STATIC asks the driver to keep the
            // storage on its side; we still write through glBufferSubData, which
            // is legal with any hint (it is a hint, not a contract).
            glBufferData(target, capacity, core::ptr::null(), GL_STATIC_DRAW);
            glBindBuffer(target, 0);
        }
        Self {
            gl_id,
            target,
            capacity,
            align,
            free: std::vec![(0 as GLintptr, capacity)],
            peak_in_use: 0,
            oom_warned: false,
            alloc_failures: 0,
            retired: Vec::new(),
            quarantine: std::collections::VecDeque::new(),
            quarantined_bytes: 0,
            quarantine_waits: 0,
        }
    }

    /// FlashNX: a region of a dropped shape. Whatever the GPU has been sent
    /// may still read it, so it waits behind the next fence (`fence_retired`)
    /// instead of going straight back to `free`.
    fn retire(&mut self, offset: GLintptr, size: GLsizeiptr) {
        if !UNSYNC_ARENA.load(std::sync::atomic::Ordering::Relaxed) {
            self.free_region(offset, size);
            return;
        }
        self.quarantined_bytes += ((size + self.align - 1) / self.align) * self.align;
        self.retired.push((offset, size));
    }

    /// FlashNX: put a fence after everything sent so far and file the regions
    /// retired since the last one behind it, then give back every batch whose
    /// fence has signalled. Once per frame (`submit_frame`).
    fn fence_retired(&mut self) {
        if !self.retired.is_empty() {
            let fence = unsafe { glFenceSync(GL_SYNC_GPU_COMMANDS_COMPLETE, 0) };
            let batch = core::mem::take(&mut self.retired);
            if fence.is_null() {
                // No fence: wait for the GPU outright, the regions are safe.
                unsafe { glFinish() };
                self.release(batch);
            } else {
                self.quarantine.push_back((fence as usize, batch));
            }
        }
        while self.reclaim_front(false) {}
    }

    /// Give back the oldest quarantined batch if its fence has signalled, or,
    /// with `block`, once it has. False when there is none to give back.
    fn reclaim_front(&mut self, block: bool) -> bool {
        let Some(&(fence, _)) = self.quarantine.front() else {
            return false;
        };
        let fence = fence as GLsync;
        let signalled = |r: GLenum| r == GL_ALREADY_SIGNALED || r == GL_CONDITION_SATISFIED;
        let mut r = unsafe { glClientWaitSync(fence, 0, 0) };
        if !signalled(r) {
            if !block {
                return false;
            }
            // A second at a time, flushing so the fence can be reached.
            loop {
                r = unsafe { glClientWaitSync(fence, GL_SYNC_FLUSH_COMMANDS_BIT, 1_000_000_000) };
                if r != GL_TIMEOUT_EXPIRED {
                    break;
                }
            }
            if !signalled(r) {
                // GL_WAIT_FAILED: wait for everything instead.
                unsafe { glFinish() };
            }
        }
        let (_, batch) = self.quarantine.pop_front().unwrap();
        unsafe { glDeleteSync(fence) };
        self.release(batch);
        true
    }

    fn release(&mut self, batch: Vec<(GLintptr, GLsizeiptr)>) {
        for (offset, size) in batch {
            self.quarantined_bytes -= ((size + self.align - 1) / self.align) * self.align;
            self.free_region(offset, size);
        }
    }

    /// Allocate `size` bytes (rounded up to `self.align`). First-fit. Returns
    /// the byte offset, or `None` if the arena is full.
    fn alloc(&mut self, size: GLsizeiptr) -> Option<GLintptr> {
        loop {
            if let Some(offset) = self.alloc_free(size) {
                return Some(offset);
            }
            // FlashNX: full, but regions may only be waiting for the GPU to
            // be done with them (`retire`): wait for the oldest batch rather
            // than drop the draw. The ones retired this frame have no fence
            // yet: give them one first.
            if !self.retired.is_empty() {
                self.fence_retired();
                continue;
            }
            if !self.reclaim_front(true) {
                break;
            }
            self.quarantine_waits = self.quarantine_waits.saturating_add(1);
        }
        self.alloc_fail(size)
    }

    fn alloc_free(&mut self, size: GLsizeiptr) -> Option<GLintptr> {
        let size = ((size + self.align - 1) / self.align) * self.align;
        for i in 0..self.free.len() {
            let (off, sz) = self.free[i];
            if sz >= size {
                let alloc_off = off;
                if sz == size {
                    self.free.remove(i);
                } else {
                    self.free[i] = (off + size, sz - size);
                }
                let in_use = self.capacity - self.free_bytes();
                if in_use > self.peak_in_use {
                    self.peak_in_use = in_use;
                }
                return Some(alloc_off);
            }
        }
        None
    }

    fn alloc_fail(&mut self, size: GLsizeiptr) -> Option<GLintptr> {
        let size = ((size + self.align - 1) / self.align) * self.align;
        // Count EVERY drop (surfaced each heartbeat as arenaDrop*) and detail
        // the first one. A dropped alloc = a dropped draw = invisible geometry.
        self.alloc_failures = self.alloc_failures.saturating_add(1);
        if !self.oom_warned {
            self.oom_warned = true;
            let msg = std::format!(
                "ARENA OOM: target=0x{:04X} capacity={} requested={} peak_in_use={} — draws will be DROPPED (invisible geometry); bump ARENA_*_SIZE\n",
                self.target, self.capacity, size, self.peak_in_use,
            );
            let mut bytes = msg.into_bytes();
            bytes.push(0);
            unsafe { ruffle_log_cstr(bytes.as_ptr() as *const _) };
        }
        None
    }

    /// Free a previously-allocated region. Size MUST match the alloc size
    /// (after alignment rounding) — caller is responsible.
    fn free_region(&mut self, offset: GLintptr, size: GLsizeiptr) {
        let size = ((size + self.align - 1) / self.align) * self.align;
        let insert_idx = self.free.partition_point(|(off, _)| *off < offset);
        self.free.insert(insert_idx, (offset, size));
        // Coalesce with next.
        if insert_idx + 1 < self.free.len() {
            let (off, sz) = self.free[insert_idx];
            let (next_off, next_sz) = self.free[insert_idx + 1];
            if off + sz == next_off {
                self.free[insert_idx] = (off, sz + next_sz);
                self.free.remove(insert_idx + 1);
            }
        }
        // Coalesce with previous.
        if insert_idx > 0 {
            let (prev_off, prev_sz) = self.free[insert_idx - 1];
            let (off, sz) = self.free[insert_idx];
            if prev_off + prev_sz == off {
                self.free[insert_idx - 1] = (prev_off, prev_sz + sz);
                self.free.remove(insert_idx);
            }
        }
    }

    /// FlashNX (2026-10-06): mapped unsynchronized, not `glBufferSubData`.
    /// Our Mesa (devkitPro's 20.1, `nouveau_buffer_transfer_map`) keeps every
    /// buffer in GART on the Switch (no VRAM), and maps it with
    /// `nouveau_bo_map(.., NOUVEAU_BO_WR)` unless the map is unsynchronized:
    /// that waits until the GPU is done with the WHOLE buffer, every draw sent
    /// that uses any shape. Mesa skips the wait only for a range never written
    /// before (`valid_buffer_range`), so it started as soon as freed regions
    /// were reused. catmario redraws its vector graphics inside ~90
    /// `BitmapData.draw` a frame at the end of its first level: 62 % of those
    /// frames were spent in that wait (sampling profile), 40 ms a frame for a
    /// 24 fps game. The wait protected nothing we need: a region is written
    /// only when newly allocated, and a freed region comes back to `free` only
    /// once the GPU has passed the fence put after the last command that could
    /// read it (`retire`, `fence_retired`).
    fn upload(&self, offset: GLintptr, data: &[u8]) {
        unsafe {
            glBindBuffer(self.target, self.gl_id);
            if UNSYNC_ARENA.load(std::sync::atomic::Ordering::Relaxed) {
                let p = glMapBufferRange(
                    self.target,
                    offset,
                    data.len() as GLsizeiptr,
                    GL_MAP_WRITE_BIT | GL_MAP_UNSYNCHRONIZED_BIT,
                );
                if !p.is_null() {
                    core::ptr::copy_nonoverlapping(data.as_ptr(), p as *mut u8, data.len());
                    glUnmapBuffer(self.target);
                    return;
                }
            }
            glBufferSubData(
                self.target,
                offset,
                data.len() as GLsizeiptr,
                data.as_ptr() as *const _,
            );
        }
    }

    fn free_bytes(&self) -> GLsizeiptr {
        self.free.iter().map(|(_, sz)| *sz).sum()
    }

    fn in_use_bytes(&self) -> GLsizeiptr {
        self.capacity - self.free_bytes()
    }
}

impl Drop for BufferArena {
    fn drop(&mut self) {
        for (fence, _) in self.quarantine.drain(..) {
            unsafe { glDeleteSync(fence as GLsync) };
        }
        unsafe { glDeleteBuffers(1, &self.gl_id) };
    }
}

// ─── Pending frees queue ────────────────────────────────────────────────────
//
// `GpuDraw::drop` runs without access to the SwitchRenderBackend (it's just
// triggered by Arc reference count going to zero, anywhere Ruffle decides
// to release a ShapeHandle). We can't free arena regions directly from the
// Drop — they'd need &mut to the arena. Instead, Drop enqueues
// (offset, size) tuples here, and submit_frame drains them at the top of
// each frame, calling `BufferArena::free_region`.
struct PendingFree {
    vbo_offset: GLintptr,
    vbo_size: GLsizeiptr,
    ibo_offset: GLintptr,
    ibo_size: GLsizeiptr,
}
static PENDING_FREES: Mutex<Vec<PendingFree>> = Mutex::new(Vec::new());

// Atlas release queue (issue #56b / Super Bowser World): atlas indices whose
// owning bitmap was just dropped by Ruffle. `AtlasTicket::drop` enqueues here
// (no backend access from Drop, same pattern as PENDING_FREES); `submit_frame`
// drains it, decrements the atlas' live count, and frees the 16 MB texture once
// it hits 0. Without this, atlases were append-only and a game re-caching a large
// offscreen surface every frame leaked ~16 MB/frame until an OOM crash.
static PENDING_ATLAS_RELEASE: Mutex<Vec<(usize, [u32; 4])>> = Mutex::new(Vec::new());

// Reusable scratch for `upload_region_padded`'s (w+2)×(h+2) edge-replicated
// buffer. Super Bowser World registers/frees ~4500 ground-strip bitmaps in a
// session; a fresh `vec![0u8; ~1.24 MB]` per strip churned the newlib heap into
// fragments until a 1.2 MB alloc failed (OOM crash on the power-up spike). One
// grow-only per-thread buffer removes that churn entirely. GL-thread-only, so a
// thread_local is safe and lock-free.
thread_local! {
    static PAD_SCRATCH: std::cell::RefCell<Vec<u8>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// One per registered atlas-backed bitmap; wrapped in `Arc` and shared by every
/// clone of its `SwitchBitmapHandle` (incl. the per-frame draw-metadata copies),
/// so the release fires exactly once, when the LAST reference drops — i.e. when
/// Ruffle has released the bitmap and no draw still points at it.
#[derive(Debug)]
struct AtlasTicket {
    atlas_index: usize,
    /// The slot it occupies, `[x, y, w, h]`: content origin, then the size
    /// with padding. Given back to the atlas (`Atlas::free_slots`) on release.
    slot: [u32; 4],
}
impl Drop for AtlasTicket {
    fn drop(&mut self) {
        if let Ok(mut q) = PENDING_ATLAS_RELEASE.lock() {
            q.push((self.atlas_index, self.slot));
        }
    }
}

use swf::{BlendMode, Color, GradientSpread};

use crate::ffi::gl::*;
use crate::query_ram;

mod kit;
mod pages;
pub(crate) use kit::icons::Icon;
pub(crate) use kit::text::HintItem;
pub(crate) use kit::theme::col as ui_col;
pub(crate) use kit::text::Btn;
pub(crate) use pages::settings::{SettingsExtra, SettingsRow};
pub(crate) use pages::windows::{Choice, WinRow};
pub(crate) use kit::list::Row;
pub(crate) use kit::sheet::{cascade_step, Sheet, Side, OPTIONS_CHILD_W, OPTIONS_W};

/// Whether the console's fonts loaded, i.e. the interface draws with the kit
/// rather than the old 5x7 font.
pub(crate) fn kit_fonts_available() -> bool {
    kit::text::fonts_available()
}
use kit::theme;

extern "C" {
    fn ruffle_log_cstr(msg: *const core::ffi::c_char);
    /// The card writer every other file goes through (cpp/src/swf_picker.cpp):
    /// Rust's `std::fs::write` reports success on Horizon and the file then
    /// reads back ENOENT on some writes.
    fn swf_picker_write_file(
        path: *const core::ffi::c_char,
        data: *const u8,
        len: u32,
    ) -> core::ffi::c_int;
    /// Monotonic tick counter (armGetSystemTick). Used for FPS heartbeat.
    fn ruffle_tick_now() -> u64;
    /// The PREVIOUS frame's `eglSwapBuffers` and its whole loop period, in ticks
    /// (main.cpp). The SLOW line's total is tick + render only, and on this
    /// driver a texture deleted while the pending pushbuffer still uses it is
    /// freed at the next flush, after a blocking wait for that submission: a cost
    /// that can move out of `render` into the swap without leaving the frame.
    fn flashnx_prev_swap_ticks() -> u64;
    fn flashnx_prev_period_ticks() -> u64;
    /// Running counters of the buffer-object cache in our libdrm_nouveau copy
    /// (cpp/third_party/libdrm_nouveau), see `BOC_STATS`.
    fn flashnx_boc_stats(out: *mut u64, n: core::ffi::c_int);
    /// nvmap handles alive (cpp/third_party/libdrm_nouveau/nouveau.c).
    fn flashnx_nvmap_live() -> i64;
    /// Bytes of transfer memory nvdrv keeps its handles in (libnx symbol,
    /// set in nouveau.c).
    static __nx_nv_transfermem_size: u32;
    /// Turn that cache on or off (on by default; off empties it).
    fn flashnx_boc_set(on: core::ffi::c_int);
    /// Return every parked BO to the heap; the cache keeps working afterwards.
    fn flashnx_boc_trim() -> u64;
    /// The top-of-heap region for big blocks (cpp/src/flashnx_heap.c): region,
    /// bytes in blocks, blocks, holes, biggest block it could give now, room
    /// between newlib's break and it, big requests refused, of which newlib served.
    fn flashnx_heap_stats(out: *mut u64);
    /// Tick frequency in Hz (~19.2 MHz on Switch). Constant after boot.
    fn ruffle_tick_freq() -> u64;
    /// Actual current CPU clock in Hz (clkrst). 0 if unavailable. Lets the
    /// heartbeat confirm whether CpuBoostMode is holding the A57 at 1785 MHz.
    fn ruffle_cpu_clock_hz() -> u32;
    /// Actual current GPU clock in Hz (clkrst). The power mode never touches
    /// the GPU, so anything but the OS default here means something else did.
    fn ruffle_gpu_clock_hz() -> u32;
    /// Times the OS took a pinned clock back from under us. Without this the
    /// question is undecidable: the re-assert is deliberately silent, so a
    /// revoke followed by our own repair leaves no trace either way.
    fn flashnx_clock_drift() -> u32;
    /// Ask for a power mode (0 NORMAL / 1 HIGH). Returns the mode actually in
    /// force, which is not always the one asked for: clkrst can be unavailable
    /// and a raise is refused outright when the battery has left its Normal
    /// voltage state.
    fn flashnx_set_clock_mode(mode: core::ffi::c_int) -> core::ffi::c_int;
    /// The mode currently in force. Reads NORMAL whenever the hardware does not
    /// agree with what was asked for.
    fn flashnx_clock_mode() -> core::ffi::c_int;
    /// Skin temperature in milli-C, 0 when unavailable. Compare against the
    /// thresholds Horizon itself uses: 53000 (fan pinned at 60%), 58000 (60 s
    /// sleep timer), 61000 (10 s timer), 63000 (immediate sleep). In handheld
    /// the OS response is SLEEP, never throttling, so this is the number that
    /// says whether a long session is at risk of dropping the player out.
    fn flashnx_skin_temp_mc() -> i32;
    /// Battery cell temperature in milli-C, 0 when unavailable.
    fn flashnx_batt_temp_mc() -> i32;
    /// Charge in per-mille (1000 = full), 0 when unavailable. Two readings far
    /// enough apart are the only route to a power figure: psm exposes no
    /// instantaneous current at all.
    fn flashnx_batt_permille() -> i32;
    /// 1 when docked, 0 handheld.
    fn ruffle_is_docked() -> core::ffi::c_int;
    /// Bytes malloc currently has handed out. The `ram=` pair next to it is the
    /// heap the crt0 reserved, which never moves; this is the number an
    /// allocation failure actually runs out of.
    fn ruffle_heap_used() -> u64;
    /// Ask malloc for chunks until it refuses, free them all, return the total.
    /// Boot measured 3136 MB this way, yet a 3.7 MB request failed mid-game at
    /// heap=1068 MB: the ceiling must move once the GL driver has taken its
    /// share, and this is how we watch it move.
    fn ruffle_probe_heap_ceiling(chunk_bytes: u64, biggest_single_out: *mut u64) -> u64;
}

fn log(nul_terminated: &[u8]) {
    unsafe { ruffle_log_cstr(nul_terminated.as_ptr() as *const _) };
}

fn log_str(s: &str) {
    let mut b = s.as_bytes().to_vec();
    b.push(0);
    log(&b);
}

// ─── Per-frame backend-primitive timing (FPS-spike attribution) ──────────────
//
// Question we want answered: when `tick` spikes to ~1.3 s on one frame, is it
// OUR backend (a readback/upload/blit stalling the GPU) or pure AVM2 bytecode
// execution (upstream Ruffle)? We time the primitives Ruffle calls DURING
// player.tick() — render_offscreen (incl. the draw() repatriation), bitmap
// register/upload, copyPixels resolve — and surface them in the slow-frame line.
// A slow frame with huge `tick` but ~0 primN_us is pure AVM2; one where a primN
// dominates is a backend culprit we can fix.
//
// CUR_* accumulate within a frame via `PrimTimer` guards. submit_frame (which
// runs right after player.tick) snapshots CUR into LAST and zeroes CUR; the
// slow-frame logger then reads LAST. Raw ticks (~19.2 MHz), µs at display.
static PRIM_OFFSCREEN_CUR: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static PRIM_OFFSCREEN_LAST: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static PRIM_BMPUP_CUR: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static PRIM_BMPUP_LAST: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static PRIM_RESOLVE_CUR: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static PRIM_RESOLVE_LAST: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
// DIAG (2026-06-03, catmario perf): sub-phase breakdown of render_offscreen,
// which dominates frame time at ~330ms when cacheAsBitmap-heavy AS3 games run.
// ALLOC=make_standalone_texture, RENDER=render_commands_to_texture,
// READBACK=glReadPixels (atlas-slot repatriate), UPLOAD=atlas.upload_region.
// N=call count this frame, PIX=sum of readback-region pixels (glReadPixels cost).
static PRIM_OFF_ALLOC_CUR: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static PRIM_OFF_ALLOC_LAST: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static PRIM_OFF_RENDER_CUR: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static PRIM_OFF_RENDER_LAST: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static PRIM_OFF_READBACK_CUR: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static PRIM_OFF_READBACK_LAST: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static PRIM_OFF_UPLOAD_CUR: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static PRIM_OFF_UPLOAD_LAST: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static PRIM_OFF_N_CUR: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static PRIM_OFF_N_LAST: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static PRIM_OFF_PIX_CUR: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
/// FlashNX: `render_offscreen` copies only the region a draw can change (see
/// there). Off from the `offbounds.off` marker, to compare.
static OFFSCREEN_BOUNDS: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

pub fn set_offscreen_bounds(enabled: bool) {
    OFFSCREEN_BOUNDS.store(enabled, std::sync::atomic::Ordering::Relaxed);
}
static PRIM_OFF_PIX_LAST: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

// DIAG (2026-09-18, Mario 63 level 8-13): the BitmapCacheEntry loop is the
// single biggest block of `submit_frame` and had no timer at all, so a slow
// frame printed `render 68000us` with every sub-timer reading zero and there
// was nothing to attribute it to.
//
// That gap is why this file has twice accumulated a per-rebind cost that was
// never actually measured. Both the 0.77 ms below and the ~0.30 ms it was
// replaced by are `total render / rebind count`, which assigns the whole of
// `render` to rebinds by construction. The one controlled measurement we have
// (5b0a9bb, moving filter passes to a colour-only FBO) says a rebind went from
// 0.46 to 0.39 ms and concludes the attachments were not the story: "it is the
// 6 to 10 switches per chain that cost". Regressing the 8-13 log agrees that
// `render` tracks `rtBind` (r2 = 0.64, ~0.32 ms each) but `rtBind` is 0.93
// collinear with the number of filter CHAINS, and at a fixed chain count the
// slope collapses to zero or goes negative. So "0.3 ms per rebind" and "1.45 ms
// per chain" fit the same data equally well, and the same rtBind buys wildly
// different render times in different parts of the log (rtBind = 178 costs
// 20.2 ms at f1197 and 61.9 ms at f4428).
//
// These two timers are what separates the two readings, and nothing here should
// be treated as settled until a log carries them:
//   CACHE  = the whole cache_entries loop, so its share of `render` is read
//            rather than inferred.
//   RTBIND = the bind/attach/validate prologue ONLY, stopped before the pass
//            draws. `rtbindUs / rtBind` is then a real per-rebind cost, and
//            `cacheUs - rtbindUs` is what the passes themselves cost. If the
//            first is small and the second large, the cost is per chain and
//            shaving rebinds is the wrong lever.
static PRIM_CACHE_CUR: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static PRIM_CACHE_LAST: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static PRIM_RTBIND_CUR: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static PRIM_RTBIND_LAST: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

// DIAG round 2 (2026-09-22). Round 1 answered the question it was built for:
// on Mario 63's 8-13, over 1102 slow frames, a render-target rebind costs
// 8.7 us (min 7.8, max 11.3) and all 154 of them together are 3 % of `render`.
// The 0.77 ms this file used to quote was 88x too high, and the two rebind
// optimisations shipped alongside those counters are worth approximately
// nothing. What it did NOT answer is where the other 97 % sits:
//
//   cacheUs 24027 us (56 % of render), of which the passes are 22687 us (53 %)
//   and 18532 us (44 %) is outside the loop entirely.
//
// Fill is already ruled out by arithmetic: the meteor shape is 22.25 px, the
// stage matrix scales 2.844, the blur grows it 5.7 px a side, so a cache
// texture is ~75x75 and the blur runs at half resolution, i.e. 37x37 = 1369
// pixels a pass. ~190 us per pass cannot be that. It is CPU overhead per pass.
//
// So this round splits the two big blocks:
//   CONTENT = step 1 only, replaying the object's own draw commands.
//   FILTER  = steps 2 and 3, the chain.
//   MKTEX   = `create_empty_texture`, which Ruffle calls from `render_base`
//             BEFORE submit_frame, so its 20-34 calls a frame land in the 44 %
//             outside the loop, not in cacheUs. Round 1 filed it in the wrong
//             place; this measures it where it actually happens.
static PRIM_CONTENT_CUR: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static PRIM_CONTENT_LAST: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static PRIM_FILTCHAIN_CUR: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static PRIM_FILTCHAIN_LAST: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static PRIM_MKTEX_CUR: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static PRIM_MKTEX_LAST: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static PRIM_MKTEX_N_CUR: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static PRIM_MKTEX_N_LAST: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

// ─── Texture churn (2026-09-23) ───────────────────────────────────────────────
//
// On this driver a GL texture is not cheap to make or to free: devkitPro's
// libdrm_nouveau gave each one its own buffer object, made with a memalign,
// three nvmap ioctls, an address-space map (every ioctl an IPC to nvservices)
// and a memset, and freed after an untimed fence wait plus two more IPCs.
// Super Mario 63's world 8 made and freed 58 to 85 of them a frame (filtered
// meteors that rotate change their cache size every frame), ~50 ms of a 100 ms
// frame. The fix lives below GL, in our copy of that library
// (cpp/third_party/libdrm_nouveau, see its README): freed buffer objects are
// recycled by size. Measured over eight levels, `render` fell from 61 to 14 ms.
// Since 2026-09-24 a slightly larger one will do (up to 25 %), and BOs up to
// 8 MiB are kept: the sampling profiler still found the misses at 7-15 % of the
// heavy frames.
//
// A GL-level pool keyed by exact texture size was tried first and measured
// useless on top of it (+2.9 ms): the sizes that change every frame only match
// once rounded to the driver's tiles, which is what the libdrm cache keys on.
// The counters below stay, because they are what showed where the time went.

/// `flashnx_boc_stats` layout: running totals, `_T` in ticks, then the entries
/// and bytes held. Must match the enum in cpp/third_party/libdrm_nouveau/nouveau.c.
const BOC_STATS: usize = 19;
const BOC_NEW_N: usize = 0;
const BOC_NEW_T: usize = 1;
const BOC_HIT: usize = 2;
const BOC_ZERO_T: usize = 3;
const BOC_DEL_N: usize = 4;
const BOC_DEL_T: usize = 5;
const BOC_DEL_CACHED: usize = 6;
const BOC_EVICT: usize = 7;
const BOC_BUSY: usize = 8;
const BOC_IPC: usize = 9;
const BOC_NEW_FAIL: usize = 10;
const BOC_RETRY_OK: usize = 11;
const BOC_BIG_N: usize = 12;
const BOC_BIG_T: usize = 13;
const BOC_BIG_BYTES: usize = 14;
const BOC_OTHER_N: usize = 15;
const BOC_SLACK_HIT: usize = 16;
const BOC_HELD_N: usize = 17;
const BOC_HELD_BYTES: usize = 18;

fn boc_stats_now() -> [u64; BOC_STATS] {
    let mut now = [0u64; BOC_STATS];
    unsafe { flashnx_boc_stats(now.as_mut_ptr(), BOC_STATS as core::ffi::c_int) };
    now
}

/// Standalone textures alive right now, every origin (`stTex=`).
static LIVE_STANDALONE: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);

// Per-frame texture traffic, CUR -> LAST at the END of submit_frame like BLEND_*:
// a frame's creates happen in render_base and its deletes anywhere from the GC
// in the tick to the main command list, so only the end of submit sees them all.
static TEX_MAKE_N_CUR: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static TEX_MAKE_N_LAST: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static TEX_MAKE_T_CUR: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static TEX_MAKE_T_LAST: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static TEX_DEL_N_CUR: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static TEX_DEL_N_LAST: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static TEX_DEL_T_CUR: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static TEX_DEL_T_LAST: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
/// Filter-pool textures let go by the two-frame TTL in `begin_frame`.
static TEX_EVICT_N_CUR: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static TEX_EVICT_N_LAST: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
/// `begin_frame` alone. It runs inside `cacheUs`, and round 2's "loop overhead"
/// (6.9 ms at 48 chains) turned out to be exactly its TTL deletions.
static TEX_BF_T_CUR: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static TEX_BF_T_LAST: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// How many more `glCheckFramebufferStatus` calls to actually issue.
///
/// Every render-target switch used to validate the framebuffer, and a filtered
/// scene does 139 to 286 of those per frame (Mario 63's level 8-13: 16 blurred
/// meteors whose `onEnterFrame` rotates them, so `BitmapCache::is_dirty` — which
/// compares matrix a/b/c/d — fires on all of them every single frame).
///
/// This is NOT claimed to be where the time goes; see the DIAG note on
/// `PRIM_RTBIND_CUR` for why no per-rebind cost in this file has ever been
/// measured cleanly. It is dropped because the call cannot tell us anything new
/// after the first few, whatever it costs: there are exactly two FBOs here, both
/// with fixed formats (RGBA8 colour, plus D24S8 for the offscreen one), and
/// completeness does not depend on the attachment's SIZE under GL 3.0+, which
/// dropped `FRAMEBUFFER_INCOMPLETE_DIMENSIONS`. So validate while both FBOs are
/// being shaken out, then stop. A genuine failure past that still shows up: the
/// draw raises a GL error and the once-a-second drain logs it.
static FBO_VALIDATE_BUDGET: std::sync::atomic::AtomicU32 =
    std::sync::atomic::AtomicU32::new(64);

fn fbo_should_validate() -> bool {
    FBO_VALIDATE_BUDGET
        .try_update(
            std::sync::atomic::Ordering::Relaxed,
            std::sync::atomic::Ordering::Relaxed,
            |n| if n == 0 { None } else { Some(n - 1) },
        )
        .is_ok()
}

// DIAG (2026-07-29, Dragon City perf): time spent in `blend()`'s EXPENSIVE paths
// and how many groups take each. The suspicion is that a full-stage (1280x720)
// offscreen round-trip per blend group dominates the frame: Dragon City logs ~85
// groups/frame while render takes the whole frame. Nothing else in the frame is
// instrumented at that level, so the SLOW line currently shows every sub-timer at
// zero while render is huge.
//
// These are swapped CUR->FRAME at the END of `submit_frame`, NOT at the top like
// the PRIM_* pair above. That matters: blends happen INSIDE submit_frame, so a
// top-of-frame swap (which is what PRIM_* does) would always publish the previous
// frame's value and read as ~0 for work done during the same submit.
//
// The inline paths (Normal/Layer/Alpha/Erase/Shader) are deliberately NOT timed —
// they just execute the group with no extra texture, so timing them would
// attribute ordinary drawing to "blend cost".
static BLEND_TICKS_CUR: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static BLEND_TICKS_FRAME: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static BLEND_N_TRIVIAL_CUR: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static BLEND_N_TRIVIAL_FRAME: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static BLEND_N_COMPLEX_CUR: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static BLEND_N_COMPLEX_FRAME: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Render-target rebinds this frame (2026-08-24). A glow chain costs 6 of these
/// to draw 5 half-res quads, and the measured 105 ms at 23 chains works out to
/// ~0.77 ms per rebind — i.e. the filter cost is the target switching, not the
/// fill. Counted so the effect of giving filter passes their own colour-only
/// FBO can be read directly instead of inferred from total render time.
/// Same CUR/FRAME swap-at-end-of-submit discipline as BLEND_* above.
static RT_BIND_CUR: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static RT_BIND_FRAME: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Allocator traffic per frame (2026-08-25). Running totals from the counting
/// global allocator are differenced once per `submit_frame`; the deltas land
/// on the `SLOW` line as `alloc=` / `free=`.
static ALLOC_N_LAST: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static FREE_N_LAST: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static ALLOC_D_FRAME: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static FREE_D_FRAME: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static ALLOC_T_LAST: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static FREE_T_LAST: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static ALLOC_T_FRAME: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static FREE_T_FRAME: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static SMALL_N_LAST: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static SMALL_D_FRAME: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// RAII guard: adds elapsed ticks to a static on drop, covering every
/// early-return path of the timed function automatically.
struct PrimTimer {
    start: u64,
    acc: &'static std::sync::atomic::AtomicU64,
}
impl PrimTimer {
    fn new(acc: &'static std::sync::atomic::AtomicU64) -> Self {
        PrimTimer { start: unsafe { ruffle_tick_now() }, acc }
    }
}
impl Drop for PrimTimer {
    fn drop(&mut self) {
        let elapsed = unsafe { ruffle_tick_now() }.saturating_sub(self.start);
        self.acc.fetch_add(elapsed, std::sync::atomic::Ordering::Relaxed);
    }
}

// ─── GPU resources ────────────────────────────────────────────────────────────

// ─── Texture atlas ─────────────────────────────────────────────────────────
//
// Mario 63 (and likely many other Flash games) register hundreds of small
// bitmaps. One GL texture per bitmap exhausts driver resources on Tegra X1
// — a deterministic crash at ~600 textures was bisected on 2026-05-24.
//
// Atlas: a single 2048x2048 RGBA texture (16 MB) packed with a shelf-based
// allocator. New atlases are added when the current one fills up. Each
// bitmap becomes a sub-rectangle (u0,v0)–(u1,v1) of one atlas.

const ATLAS_SIZE: u32 = 2048;
const ATLAS_PAD: u32 = 1; // 1 px padding around each bitmap to avoid bleed

/// GPU-side memory budget for LIVE big right-sized atlases. This is now the SOLE
/// memory guard for the Super Bowser World class of game: the Ruffle-side CPU cap
/// is DISABLED (refusing a BitmapData returns `undefined`, which breaks the game's
/// blit engine into a per-frame re-render freeze — see `BITMAPDATA_BUDGET_BYTES`
/// in ruffle_core's bitmap_data.rs). Over budget, a big bitmap is rendered
/// invisible via a VALID `DroppedBitmap` handle — no GL texture, but the game's
/// `getPixel`/`copyPixels` still read Ruffle's CPU pixel Vec, so collision and
/// compositing keep working and the engine never sees a failure (no freeze). The
/// dropped surface is simply not drawn. Bounding here also curbs the transient
/// upload-buffer churn (`upload_region_padded`) whose 1.2 MB alloc failed on the
/// fragmented heap at ~481 MB of live GPU strips (the death-animation crash) — so
/// keep this comfortably under that. Tunable from the heartbeat `bigMB`.
const BIG_ATLAS_BUDGET_BYTES: u64 = 400 * 1024 * 1024;

/// A bitmap this big (in either axis, but still ≤ ATLAS_SIZE) gets its own
/// right-sized dedicated atlas rather than sharing a 2048² one — matches the
/// `big` test in `pack_into_atlas`. These are the surfaces the budget governs.
fn is_big_surface(w: u32, h: u32) -> bool {
    (w > ATLAS_SIZE / 2 || h > ATLAS_SIZE / 2) && w <= ATLAS_SIZE && h <= ATLAS_SIZE
}

struct Shelf {
    y: u32,
    height: u32,
    used_w: u32,
}

struct Atlas {
    texture: GLuint,
    width: u32,
    height: u32,
    shelves: Vec<Shelf>,
    /// Live bitmaps packed into this atlas (issue #56b / Super Bowser World).
    /// Incremented per `pack_into_atlas`, decremented when the owning
    /// `SwitchBitmapHandle` drops (via `PENDING_ATLAS_RELEASE`). At 0 the atlas is
    /// freed (`texture` deleted, set to 0 = dead slot, reusable) so a game that
    /// re-caches large offscreen surfaces every frame can't leak 16 MB/frame → OOM.
    live: u32,
    /// Last frame a draw sampled this atlas (0 = never), for `atlasIdle=`: how
    /// much GPU memory holds images nothing has shown for a while. Measured
    /// before building any eviction, to know what it could give back.
    last_draw_frame: Cell<u32>,
    /// Holds only SWF images that can be decoded again (see `ReloadSlot`), so
    /// that emptying it frees it. Never mixed with BitmapData surfaces.
    reloadable: bool,
    /// The images packed here, when `reloadable`.
    members: Vec<SlotLink>,
    /// Slots whose bitmap was released while the atlas lives on, `[x, y, w,
    /// h]` as in `AtlasTicket::slot`. The shelves never give space back: Super
    /// Smash Flash 2 creates and drops BitmapData every frame (one shadow per
    /// character), and a few long-lived ones pinned ~11 atlases (~200 MB)
    /// holding ~23 MB of live BitmapData (2026-10-02). New bitmaps take these
    /// first.
    free_slots: Vec<[u32; 4]>,
}

impl Atlas {
    fn new(size: u32) -> Option<Self> {
        Self::new_wh(size, size)
    }

    /// Allocate a `width × height` atlas texture. Shared atlases are square
    /// (`ATLAS_SIZE²`); a bitmap too large to share gets a right-sized dedicated
    /// atlas (issue #56b) so a 1824×1174 surface costs ~8.5 MB, not a full 16 MB
    /// 2048² — halving the memory of games that spam big offscreen surfaces.
    ///
    /// None when the GPU heap has no room. The texture NAME exists either way,
    /// but a failed glTexImage2D leaves it without a level-0 image, and the first
    /// `BitmapData.draw` that attaches such an atlas to an FBO dies in Mesa's
    /// `st_update_renderbuffer_surface` (DataAbort, FAR=0xe). That exact crash,
    /// same registers (x2 = x6 = 0x800, the atlas width), ended Super Smash
    /// Flash 2 three times: twice in August and once on 2026-09-23, each right
    /// after an `atlas: allocating` under memory exhaustion.
    /// `make_standalone_texture` has checked this since `9aa951a`; atlases never
    /// did.
    fn new_wh(width: u32, height: u32) -> Option<Self> {
        let mut tex: GLuint = 0;
        unsafe {
            glGenTextures(1, &mut tex);
            if tex == 0 {
                return None;
            }
            glBindTexture(GL_TEXTURE_2D, tex);
            glPixelStorei(GL_UNPACK_ALIGNMENT, 1);
            // Drain stale errors so the check below reads this call only.
            let mut drain = 0;
            while glGetError() != GL_NO_ERROR && drain < 16 {
                drain += 1;
            }
            glTexImage2D(
                GL_TEXTURE_2D,
                0,
                GL_RGBA8 as GLint,
                width as GLsizei,
                height as GLsizei,
                0,
                GL_RGBA,
                GL_UNSIGNED_BYTE,
                core::ptr::null(),
            );
            if glGetError() != GL_NO_ERROR {
                glBindTexture(GL_TEXTURE_2D, 0);
                glDeleteTextures(1, &tex);
                return None;
            }
            glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, GL_LINEAR as GLint);
            glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER, GL_LINEAR as GLint);
            glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_WRAP_S, GL_CLAMP_TO_EDGE as GLint);
            glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_WRAP_T, GL_CLAMP_TO_EDGE as GLint);
            glBindTexture(GL_TEXTURE_2D, 0);
        }
        Some(Self {
            texture: tex,
            width,
            height,
            shelves: Vec::new(),
            live: 0,
            last_draw_frame: Cell::new(0),
            reloadable: false,
            members: Vec::new(),
            free_slots: Vec::new(),
        })
    }

    /// Delete the GL texture and mark this slot DEAD (`texture == 0`) so
    /// `pack_into_atlas` can reuse the slot for a fresh atlas. Called when the
    /// atlas' last bitmap is released.
    fn free_gl(&mut self) {
        if self.texture != 0 {
            unsafe { glDeleteTextures(1, &self.texture) };
        }
        self.texture = 0;
        self.shelves.clear();
        self.live = 0;
        self.members.clear();
        self.free_slots.clear();
    }

    /// Try to allocate a `w×h` region (plus padding). Returns the content
    /// origin (without padding) and the slot it takes (see `AtlasTicket::slot`).
    fn pack(&mut self, w: u32, h: u32) -> Option<(u32, u32, [u32; 4])> {
        let w_full = w + 2 * ATLAS_PAD;
        let h_full = h + 2 * ATLAS_PAD;
        if w_full > self.width || h_full > self.height {
            return None;
        }
        // A released slot first: the smallest that fits, and not one more
        // than four times the size (a big hole is better left to a big bitmap;
        // what a smaller tenant leaves unused comes back with it).
        let need = w_full as u64 * h_full as u64;
        let mut best: Option<(usize, u64)> = None;
        for (i, f) in self.free_slots.iter().enumerate() {
            if f[2] >= w_full && f[3] >= h_full {
                let area = f[2] as u64 * f[3] as u64;
                if area <= need * 4 && best.is_none_or(|(_, a)| area < a) {
                    best = Some((i, area));
                }
            }
        }
        if let Some((i, _)) = best {
            SLOTS_REUSED.fetch_add(1, Ordering::Relaxed);
            let slot = self.free_slots.swap_remove(i);
            return Some((slot[0], slot[1], slot));
        }
        for shelf in &mut self.shelves {
            if shelf.height >= h_full && shelf.used_w + w_full <= self.width {
                let x = shelf.used_w + ATLAS_PAD;
                let y = shelf.y + ATLAS_PAD;
                shelf.used_w += w_full;
                return Some((x, y, [x, y, w_full, h_full]));
            }
        }
        let next_y = self.shelves.last().map(|s| s.y + s.height).unwrap_or(0);
        if next_y + h_full > self.height {
            return None;
        }
        self.shelves.push(Shelf {
            y: next_y,
            height: h_full,
            used_w: w_full,
        });
        let (x, y) = (ATLAS_PAD, next_y + ATLAS_PAD);
        Some((x, y, [x, y, w_full, h_full]))
    }

    /// `src_row_len_px` = the row length (in pixels) of the SOURCE `pixels`
    /// buffer, which may be wider than `w` when uploading a sub-region of a
    /// larger bitmap. Passed to GL_UNPACK_ROW_LENGTH so GL skips full source
    /// rows instead of packing them contiguously at width `w` (the latter
    /// shears partial-width uploads). `pixels` must start at the region's
    /// top-left pixel.
    fn upload_region(&self, x: u32, y: u32, w: u32, h: u32, src_row_len_px: u32, pixels: &[u8]) {
        unsafe {
            glBindTexture(GL_TEXTURE_2D, self.texture);
            glPixelStorei(GL_UNPACK_ALIGNMENT, 1);
            glPixelStorei(GL_UNPACK_ROW_LENGTH, src_row_len_px as GLint);
            glTexSubImage2D(
                GL_TEXTURE_2D,
                0,
                x as GLint,
                y as GLint,
                w as GLsizei,
                h as GLsizei,
                GL_RGBA,
                GL_UNSIGNED_BYTE,
                pixels.as_ptr() as *const _,
            );
            glPixelStorei(GL_UNPACK_ROW_LENGTH, 0);
            glBindTexture(GL_TEXTURE_2D, 0);
        }
    }

    /// Like `upload_region`, but also replicates the 1-pixel border into
    /// the surrounding pad area. Required for atlased rendering with
    /// LINEAR filtering: without edge bleed, sampling at the bitmap edge
    /// blends 50% transparent-black-pad → visible black grid between
    /// sprites in Mario 63. Caller must guarantee that (x, y) is at least
    /// ATLAS_PAD pixels away from the atlas borders (always true for our
    /// packer).
    fn upload_region_padded(&self, x: u32, y: u32, w: u32, h: u32, pixels: &[u8]) {
        if w == 0 || h == 0 {
            return;
        }
        // Build a (w+2) × (h+2) buffer with edge replication, into a REUSED
        // per-thread scratch (grow-only) to avoid a ~1.24 MB alloc/free per call
        // — that churn fragmented the heap to OOM under strip spam (see PAD_SCRATCH).
        let pw = (w + 2) as usize;
        let ph = (h + 2) as usize;
        let needed = pw * ph * 4;
        PAD_SCRATCH.with(|cell| {
            let mut scratch = cell.borrow_mut();
            if scratch.len() < needed {
                scratch.resize(needed, 0);
            }
            let buf = &mut scratch[..needed];
            let row_bytes = w as usize * 4;
            // Center rows: copy each source row into the buffer with 1 px
            // of horizontal replication on each side.
            for src_row in 0..h as usize {
                let src_off = src_row * row_bytes;
                let dst_row = src_row + 1;
                let dst_off = dst_row * pw * 4 + 4; // skip the left pad pixel
                buf[dst_off..dst_off + row_bytes]
                    .copy_from_slice(&pixels[src_off..src_off + row_bytes]);
                // Left pad pixel = first source pixel of this row.
                let lpad_off = dst_row * pw * 4;
                buf[lpad_off..lpad_off + 4].copy_from_slice(&pixels[src_off..src_off + 4]);
                // Right pad pixel = last source pixel of this row.
                let rpad_off = dst_row * pw * 4 + (pw - 1) * 4;
                let last_pix_off = src_off + (w as usize - 1) * 4;
                buf[rpad_off..rpad_off + 4]
                    .copy_from_slice(&pixels[last_pix_off..last_pix_off + 4]);
            }
            // Top pad row (row 0) = duplicate of first content row (row 1,
            // already has horizontal replication baked in).
            let row_stride = pw * 4;
            buf.copy_within(row_stride..2 * row_stride, 0);
            // Bottom pad row (row h+1) = duplicate of last content row (row h).
            let last_content = h as usize * row_stride;
            let last_pad = (h as usize + 1) * row_stride;
            buf.copy_within(last_content..last_content + row_stride, last_pad);

            unsafe {
                glBindTexture(GL_TEXTURE_2D, self.texture);
                glPixelStorei(GL_UNPACK_ALIGNMENT, 1);
                glTexSubImage2D(
                    GL_TEXTURE_2D,
                    0,
                    (x as i32) - 1,
                    (y as i32) - 1,
                    (w + 2) as GLsizei,
                    (h + 2) as GLsizei,
                    GL_RGBA,
                    GL_UNSIGNED_BYTE,
                    buf.as_ptr() as *const _,
                );
                glBindTexture(GL_TEXTURE_2D, 0);
            }
        });
    }
}

impl Drop for Atlas {
    fn drop(&mut self) {
        unsafe { glDeleteTextures(1, &self.texture) };
    }
}

#[derive(Clone, Debug)]
struct SwitchBitmapHandle {
    atlas_index: usize,
    /// Atlas-space UV bounds in [0, 1].
    u0: f32,
    v0: f32,
    u1: f32,
    v1: f32,
    width: u32,
    height: u32,
    /// Shared release token (issue #56b). Cloning the handle (per-frame draw meta)
    /// shares this Arc; the atlas' live count drops only when the LAST clone AND
    /// the Ruffle-owned handle are gone. `None` only for handles built before this
    /// existed (none in practice — every `pack_into_atlas` sets it).
    ticket: Option<Arc<AtlasTicket>>,
}
impl BitmapHandleImpl for SwitchBitmapHandle {}

// ─── Evictable SWF images ─────────────────────────────────────────────────────
//
// Every image a SWF shows gets decoded into an atlas the first time it is drawn
// and stayed there for the rest of the session (upstream's FIXME in
// `BitmapCharacter::bitmap_handle`). Super Smash Flash 2 decodes 1.5 GB of
// images across the ~50 files it loads (DAT102 alone, the menus: 406 MB), and
// its atlases grew by 150-240 MB per Classic level, 637 MB by level 4 where the
// heap ran out (2026-10-01 logs). Nothing ever drew most of them again.
//
// A SWF bitmap tag never changes and its bytes stay in the movie, so its
// texture can be dropped and decoded again. Such images arrive through
// `register_bitmap_reloadable` (Ruffle's `BitmapCharacter::bitmap_handle`) and
// only ever reach us as shape fills (`register_shape`). They are packed into
// atlases of their own (`Atlas::reloadable`): an atlas mixed with BitmapData
// surfaces could never be freed. `evict_idle_images` empties atlases whose
// images have not been drawn for a while; `render_shape` decodes an evicted
// image again when a shape draws it.

/// Where an image sits in an atlas.
#[derive(Clone, Copy, Debug)]
struct AtlasPlace {
    atlas_index: usize,
    u0: f32,
    v0: f32,
    u1: f32,
    v1: f32,
}

struct ReloadSlot {
    /// `None` once evicted.
    place: Cell<Option<AtlasPlace>>,
    /// Holds the atlas (its `live` count) while the image is resident.
    ticket: core::cell::RefCell<Option<Arc<AtlasTicket>>>,
    width: u32,
    height: u32,
    /// Last host frame a shape drew it.
    last_draw: Cell<u32>,
    /// Frame of the last failed reload, so a failure is not retried every draw.
    failed_at: Cell<u32>,
    reload: std::rc::Rc<dyn Fn() -> Option<Bitmap<'static>>>,
}

impl std::fmt::Debug for ReloadSlot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ReloadSlot({}x{}, {:?})", self.width, self.height, self.place.get())
    }
}

#[derive(Clone, Debug)]
struct ReloadableBitmap(std::rc::Rc<ReloadSlot>);
impl BitmapHandleImpl for ReloadableBitmap {}

/// An atlas' weak link to one of its images. The backend sits in a `static
/// Mutex`, so it has to be `Send`; the slots it points to are created, drawn,
/// evicted and dropped by the player thread only (Ruffle's handles are not
/// `Send` either), which is the thread that owns a game's backend.
struct SlotLink(std::rc::Weak<ReloadSlot>);
unsafe impl Send for SlotLink {}

/// Bitmaps packed into a released atlas slot (`Atlas::free_slots`), for the
/// `evict:` line.
static SLOTS_REUSED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Eviction starts once the reloadable atlases hold this much...
const EVICT_START_BYTES: u64 = 128 * 1024 * 1024;
/// ...and only touches images nothing has drawn for this many host frames
/// (20 s at 60 Hz; a slow game frame is still one host frame).
const EVICT_IDLE_FRAMES: u32 = 1200;
/// An atlas is emptied even if some of its images are still in use: they are
/// decoded again on their next draw, a few ms each. At most this many per
/// pass, so a pass in the middle of a fight costs a few frames at worst...
const EVICT_MAX_HOT: usize = 24;
/// ...and no more than this in all.
const EVICT_MAX_HOT_BYTES: u64 = 4 * 1024 * 1024;
/// Atlases emptied per pass, so a pass never turns into a long frame.
const EVICT_MAX_PER_PASS: usize = 4;

// ─── Standalone (FBO-attachable) textures ─────────────────────────────────────
//
// The atlas system above packs many bitmaps into shared GL textures, which is
// great for the common case but cannot be used as an FBO color attachment
// (you'd render over neighbours). cacheAsBitmap / filtered display objects need
// their own texture to render into and sample from, so Ruffle hands us a
// dedicated handle via `create_empty_texture`. This is the second BitmapHandle
// variant — code paths taking a BitmapHandle try `as_standalone_bitmap` before
// falling back to `as_switch_bitmap`. Mirrors the wgpu backend where EVERY
// bitmap is a standalone `Texture`.

/// A GL texture that owns its storage (not atlas-packed), suitable as an FBO
/// color attachment and as a sampling source. Owns the GL texture; the Drop
/// frees it.
pub(crate) struct StandaloneTexture {
    pub(crate) texture: GLuint,
    pub(crate) width: u32,
    pub(crate) height: u32,
    /// What it is for (`ST_*`), for the `stTexMB` breakdown.
    kind: usize,
}

/// What a standalone texture is for. `stTex=` only counted them, and Super
/// Smash Flash 2 ran out of memory at 1758 of them (2026-10-01) with no way to
/// tell how many MB they held or whose they were.
pub(crate) const ST_FILTER: usize = 0; // filter pool, and anything untagged
pub(crate) const ST_OFFSCREEN: usize = 1; // render_offscreen temps
pub(crate) const ST_CACHE: usize = 2; // cacheAsBitmap / filter caches (create_empty_texture)
pub(crate) const ST_BITMAPDATA: usize = 3; // a bitmap too big for an atlas
pub(crate) const ST_STAGE3D: usize = 4;
const ST_KINDS: usize = 5;
static ST_N: [std::sync::atomic::AtomicI64; ST_KINDS] =
    [const { std::sync::atomic::AtomicI64::new(0) }; ST_KINDS];
static ST_BYTES: [std::sync::atomic::AtomicI64; ST_KINDS] =
    [const { std::sync::atomic::AtomicI64::new(0) }; ST_KINDS];

fn st_count(kind: usize, width: u32, height: u32, sign: i64) {
    use std::sync::atomic::Ordering::Relaxed;
    ST_N[kind].fetch_add(sign, Relaxed);
    ST_BYTES[kind].fetch_add(sign * width as i64 * height as i64 * 4, Relaxed);
}

impl StandaloneTexture {
    /// Re-files this texture under `kind` (it starts as `ST_FILTER`).
    pub(crate) fn tagged(mut self, kind: usize) -> Self {
        st_count(self.kind, self.width, self.height, -1);
        st_count(kind, self.width, self.height, 1);
        self.kind = kind;
        self
    }
}

/// `stTexMB=` total, then per kind `n/MB`.
fn standalone_breakdown() -> std::string::String {
    use std::sync::atomic::Ordering::Relaxed;
    let mb = |k: usize| ST_BYTES[k].load(Relaxed) / (1024 * 1024);
    let total: i64 = (0..ST_KINDS).map(mb).sum();
    std::format!(
        "{} (filter {}/{} offscreen {}/{} cache {}/{} bitmapdata {}/{} stage3d {}/{})",
        total,
        ST_N[ST_FILTER].load(Relaxed), mb(ST_FILTER),
        ST_N[ST_OFFSCREEN].load(Relaxed), mb(ST_OFFSCREEN),
        ST_N[ST_CACHE].load(Relaxed), mb(ST_CACHE),
        ST_N[ST_BITMAPDATA].load(Relaxed), mb(ST_BITMAPDATA),
        ST_N[ST_STAGE3D].load(Relaxed), mb(ST_STAGE3D),
    )
}

impl Drop for StandaloneTexture {
    fn drop(&mut self) {
        let _pt = PrimTimer::new(&TEX_DEL_T_CUR);
        TEX_DEL_N_CUR.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        LIVE_STANDALONE.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
        st_count(self.kind, self.width, self.height, -1);
        unsafe { glDeleteTextures(1, &self.texture) };
    }
}

impl std::fmt::Debug for StandaloneTexture {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "StandaloneTexture(id={}, {}x{})", self.texture, self.width, self.height)
    }
}

/// BitmapHandle payload for a standalone texture. Cheap to clone (Arc); the
/// GL texture dies when the last Arc drops.
#[derive(Clone, Debug)]
pub(crate) struct StandaloneBitmap(pub(crate) Arc<StandaloneTexture>);
impl BitmapHandleImpl for StandaloneBitmap {}

/// Wrap a GL texture we already own as a bitmap handle the 2D compositor can
/// draw. Used by the Stage3D back buffer (#88): rather than teaching every draw
/// path about a new payload type, the 3D picture arrives as the same standalone
/// bitmap a big `BitmapData` uses. The handle owns the texture from here on.
pub(crate) fn standalone_bitmap_from_texture(
    texture: GLuint,
    width: u32,
    height: u32,
) -> BitmapHandle {
    // Its Drop counts it out like any other, so it has to be counted in.
    LIVE_STANDALONE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    st_count(ST_STAGE3D, width, height, 1);
    BitmapHandle(Arc::new(StandaloneBitmap(Arc::new(StandaloneTexture {
        texture,
        width,
        height,
        kind: ST_STAGE3D,
    }))))
}

/// BitmapHandle payload for a big surface we REFUSED to back with a texture
/// because the big-atlas memory budget was exhausted (Super Bowser World
/// cinematic OOM). Owns NO GL resource: `render_bitmap` draws nothing,
/// `update_texture`/`render_offscreen` no-op. The surface is invisible instead
/// of taking the whole app down with an allocation failure. Carries the
/// intended dims only for diagnostics.
#[derive(Clone, Debug)]
struct DroppedBitmap {
    #[allow(dead_code)]
    width: u32,
    #[allow(dead_code)]
    height: u32,
}
impl BitmapHandleImpl for DroppedBitmap {}

/// SyncHandle for `BitmapData.draw()` and `apply_filter`. Holds a NON-owning GL texture id (the
/// temp the draw commands were rendered into) plus the dirty region to read
/// back. Ruffle stores this in the BitmapData's `GpuModified` state and calls
/// `resolve_sync_handle` on the next CPU access (e.g. `copyPixels`), which
/// reads the pixels back into the BitmapData's CPU buffer. The texture itself
/// lives in the backend's `offscreen_temp_retired`/`offscreen_temp_pool`
/// (recycled one frame later, after Ruffle has resolved/dropped this handle in
/// the same tick), so this struct does NOT free it on drop — avoiding a
/// per-call texture alloc that cost ~90ms/frame on cacheAsBitmap-heavy games.
/// A recycled `render_offscreen` temp plus the frame it was last handed out on.
/// The stamp is what keeps a temp alive while a SyncHandle may still point at it
/// (see `offscreen_temp_pool`), and what lets a size nobody asks for any more age
/// out — the two things a flat byte cap could not tell apart.
struct PooledTemp {
    tex: StandaloneTexture,
    last_used_frame: u32,
}

/// A pending GPU -> CPU read for a `BitmapData.draw()` / `applyFilter` result.
///
/// ⚠️ `texture` MUST be storage that outlives the handle. Ruffle parks this in
/// `DirtyState::GpuModified` and only reads it on the next CPU access, which may
/// be many frames away (`bitmap_data.rs::sync`) — upstream's wgpu backend has no
/// such hazard because it renders straight into the BitmapData's own texture and
/// never uses a scratch. Pointing this at a pooled temp means the temp can be
/// freed (#14, hearts and enemies vanishing) or handed to another same-size draw
/// (silently reading back the WRONG sprite). So: standalone targets point at the
/// BitmapData's own texture, atlas targets at the atlas itself, with `ticket`
/// holding that atlas alive for as long as this handle exists.
struct BitmapDataSyncHandle {
    texture: GLuint,
    /// Full size of `texture` — the atlas dimensions when atlas-backed, needed to
    /// normalize the premultiplying blit. Not the region size.
    tex_w: u32,
    tex_h: u32,
    /// Read origin IN `texture`. For an atlas that is the packed region's base
    /// plus the dirty region; for a standalone texture just the dirty region.
    x: u32,
    y: u32,
    /// Dirty region size — must match the `bounds` Ruffle passed, since the
    /// readback closure indexes the buffer relative to this region's origin.
    w: u32,
    h: u32,
    /// Atlases store STRAIGHT alpha while Ruffle's BitmapData CPU pixels are
    /// PREMULTIPLIED, so an atlas read has to be converted. The conversion is
    /// done on the GPU at RESOLVE time via the existing `blit_premult` into a
    /// scratch that is consumed immediately — deliberately not by hand at
    /// readback, which is where the offroaders speckle came from.
    premult: bool,
    /// Keeps the atlas slot alive while this handle is pending. `BitmapRawData`
    /// already holds the BitmapHandle next to `dirty_state`, so this is belt and
    /// braces, but it makes the invariant local instead of inferred.
    _ticket: Option<Arc<AtlasTicket>>,
}
impl std::fmt::Debug for BitmapDataSyncHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "BitmapDataSyncHandle(tex={}, region={}x{}@{},{}, premult={})",
            self.texture, self.w, self.h, self.x, self.y, self.premult
        )
    }
}
impl SyncHandle for BitmapDataSyncHandle {}

/// Allocate a fresh transparent RGBA8 texture (linear + clamp-to-edge),
/// suitable as an FBO color attachment. Returns None for a zero dimension.
fn make_standalone_texture(width: u32, height: u32) -> Option<StandaloneTexture> {
    if width == 0 || height == 0 {
        return None;
    }
    let _pt = PrimTimer::new(&TEX_MAKE_T_CUR);
    TEX_MAKE_N_CUR.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let mut tex: GLuint = 0;
    unsafe {
        glGenTextures(1, &mut tex);
        // glGenTextures returns 0 on failure (e.g. GL out of memory / too many
        // live textures). Using a 0 texture as an FBO color attachment or
        // sampler source crashes Mesa with a NULL deref (Data Abort, FAR≈0x0e).
        // Bail so callers (the filter pool) skip the pass instead of crashing.
        if tex == 0 {
            ruffle_log_cstr(b"make_standalone_texture: glGenTextures returned 0 (OOM?)\n\0".as_ptr() as *const _);
            return None;
        }
        glBindTexture(GL_TEXTURE_2D, tex);
        glPixelStorei(GL_UNPACK_ALIGNMENT, 1);
        // Drain any stale GL error so the post-alloc check below is accurate.
        let mut drain = 0;
        while glGetError() != GL_NO_ERROR && drain < 16 {
            drain += 1;
        }
        // Allocate storage with NULL data: every consumer fully overwrites the
        // texture before sampling it (render_commands_to_texture glClears it;
        // filter passes draw the whole region). The old `vec![0u8; w*h*4]`
        // CPU-side zero-fill was pure overhead — and dominated frame time when
        // the (now bounded) filter pool had to re-allocate on a cache miss.
        glTexImage2D(
            GL_TEXTURE_2D, 0, GL_RGBA8 as GLint,
            width as GLsizei, height as GLsizei, 0,
            GL_RGBA, GL_UNSIGNED_BYTE, core::ptr::null(),
        );
        // glTexImage2D can fail with GL_OUT_OF_MEMORY on a large temp under GPU
        // pressure (Icy Tower's ~2 MP nested render_offscreen surfaces): the id is
        // valid but the texture has NO level-0 image, and FBO-attaching an
        // image-less texture crashes Mesa with a NULL deref (Data Abort, FAR≈0x0e)
        // exactly like a 0 id (glGenTextures is checked above; glTexImage2D was
        // not). Bail so callers skip the pass (draw no-ops) instead of aborting.
        if glGetError() != GL_NO_ERROR {
            glBindTexture(GL_TEXTURE_2D, 0);
            glDeleteTextures(1, &tex);
            // With the size, and not every time: SSF2 logged 16 769 of these in
            // one session, ~9 ms each (Mesa drains the GPU and retries before
            // giving up), and the log line itself goes over the network.
            static FAILS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
            let n = FAILS.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
            if n <= 20 || n % 500 == 0 {
                let msg = std::format!(
                    "make_standalone_texture: glTexImage2D failed for {}x{} ({} KB), skipped (GPU OOM?, failures={})\n",
                    width, height, width as u64 * height as u64 * 4 / 1024, n,
                );
                let mut b = msg.into_bytes();
                b.push(0);
                ruffle_log_cstr(b.as_ptr() as *const _);
            }
            return None;
        }
        glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, GL_LINEAR as GLint);
        glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER, GL_LINEAR as GLint);
        glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_WRAP_S, GL_CLAMP_TO_EDGE as GLint);
        glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_WRAP_T, GL_CLAMP_TO_EDGE as GLint);
        glBindTexture(GL_TEXTURE_2D, 0);
    }
    LIVE_STANDALONE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    st_count(ST_FILTER, width, height, 1);
    Some(StandaloneTexture { texture: tex, width, height, kind: ST_FILTER })
}

/// What kind of draw call this is (chooses the shader program).
enum DrawKind {
    Solid,
    Gradient {
        /// Index into `GpuShape::gradient_textures`.
        texture_index: usize,
        /// 3x3 column-major matrix that maps `a_pos` (shape pixels) to
        /// gradient-local coords. Pre-inverted on CPU.
        local_matrix: [GLfloat; 9],
        gradient_kind: i32, // 0=linear, 1=radial, 2=focal
        spread: i32,        // 0=pad, 1=reflect, 2=repeat
        focal: f32,
    },
    Bitmap {
        /// Index into `SwitchRenderBackend::atlases` — the GL texture is
        /// owned by the atlas, not per-draw. Ignored when `standalone` is set.
        atlas_index: usize,
        /// Atlas-space UV remap (origin.x, origin.y, scale.x, scale.y).
        /// Identity `[0,0,1,1]` for a standalone fill (the texture IS the
        /// whole bitmap).
        uv_remap: [f32; 4],
        /// Half a texel of the SOURCE bitmap, expressed in the [0,1] local UV
        /// space (`0.5 / width`, `0.5 / height`). Computed from the BITMAP's
        /// pixel size, never from an atlas dimension: after `uv_remap.zw` has
        /// scaled it, half a local texel and half an atlas texel are the same
        /// distance, and this way nothing here can repeat the #42 regression of
        /// assuming 2048 for an atlas that is right-sized.
        uv_inset: [f32; 2],
        /// 3x3 column-major matrix mapping `a_pos` (shape pixels) to UV
        /// in [0, 1] of the source bitmap. Pre-inverted by
        /// `swf_bitmap_to_gl_matrix`.
        local_matrix: [GLfloat; 9],
        /// The SWF fill style's smoothing flag. A file that says "do not smooth
        /// this" is asking for point sampling, and Flash gave it point sampling.
        is_smoothed: bool,
        is_repeating: bool,
        /// Set for fills whose source bitmap is too big for the 2048² atlas
        /// (e.g. Mario Combat's >2048 sky/floor): the standalone GL texture to
        /// sample instead of `atlas_index`. Holds the `Arc` so the texture
        /// outlives this draw (its `Drop` deletes the GL texture). Without this
        /// the fill fell back to `Solid` and rendered as a white block.
        standalone: Option<Arc<StandaloneTexture>>,
        /// An evictable SWF image: `atlas_index`/`uv_remap` are then unused,
        /// the draw reads where the image is NOW (it may have been evicted and
        /// decoded again elsewhere since this shape was registered).
        slot: Option<std::rc::Rc<ReloadSlot>>,
    },
}

struct GpuDraw {
    /// Byte offset of this draw's vertices inside the global vertex arena.
    vbo_offset: GLintptr,
    /// Allocated vertex bytes (multiple of `ARENA_ALIGN`).
    vbo_size: GLsizeiptr,
    /// Byte offset of this draw's indices inside the global index arena.
    ibo_offset: GLintptr,
    /// Allocated index bytes (multiple of `ARENA_ALIGN`).
    ibo_size: GLsizeiptr,
    num_indices: GLsizei,
    kind: DrawKind,
}

impl Drop for GpuDraw {
    fn drop(&mut self) {
        LIVE_GPU_DRAWS.fetch_sub(1, Ordering::Relaxed);
        // Can't free arena regions from here — no &mut to the backend.
        // Enqueue; submit_frame drains at the top of each frame.
        PENDING_FREES.lock().unwrap().push(PendingFree {
            vbo_offset: self.vbo_offset,
            vbo_size: self.vbo_size,
            ibo_offset: self.ibo_offset,
            ibo_size: self.ibo_size,
        });
    }
}

struct GpuShape {
    draws: Vec<GpuDraw>,
    gradient_textures: Vec<GLuint>,
}

impl Drop for GpuShape {
    fn drop(&mut self) {
        LIVE_GPU_SHAPES.fetch_sub(1, Ordering::Relaxed);
        if !self.gradient_textures.is_empty() {
            unsafe {
                glDeleteTextures(
                    self.gradient_textures.len() as GLsizei,
                    self.gradient_textures.as_ptr(),
                );
            }
        }
    }
}

impl std::fmt::Debug for GpuShape {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "GpuShape({} draws, {} gradients)", self.draws.len(), self.gradient_textures.len())
    }
}

#[derive(Debug)]
struct SwitchShapeHandle(Arc<GpuShape>);
impl ShapeHandleImpl for SwitchShapeHandle {}

// ─── Shader programs ──────────────────────────────────────────────────────────

struct SolidProgram {
    program: GLuint,
    u_world: GLint,
    u_mult: GLint,
    u_add: GLint,
}

struct BitmapProgram {
    program: GLuint,
    u_world: GLint,
    u_mult: GLint,
    u_add: GLint,
    u_tex: GLint,
    u_uv_remap: GLint,
    /// 0 = straight alpha (atlas, launcher art), 1 = premultiplied (standalone
    /// cache / filter / `BitmapData.draw` textures). See `BITMAP_FRAG`.
    u_premult: GLint,
}

struct GradientProgram {
    program: GLuint,
    u_world: GLint,
    u_mult: GLint,
    u_add: GLint,
    u_tex: GLint,
    u_grad_local: GLint,
    u_grad_kind: GLint,
    u_grad_spread: GLint,
    u_grad_focal: GLint,
}

/// Shader for "bitmap fill inside a shape": vertex computes UV from
/// `a_pos` via a per-draw 3×3 matrix (no per-vertex UV attribute), then
/// remaps from [0,1] to the atlas sub-rectangle. Fragment samples the
/// bound texture and applies color transform.
struct ShapeBitmapProgram {
    program: GLuint,
    u_world: GLint,
    u_mult: GLint,
    u_add: GLint,
    u_tex: GLint,
    u_uv: GLint,
    u_uv_remap: GLint,
    /// Half a texel of the SOURCE bitmap, in the [0,1] local UV the clamp
    /// branch works in. See the doc comment on `SHAPE_BITMAP_VERT`.
    u_uv_inset: GLint,
    u_wrap_mode: GLint,
}

impl Drop for SolidProgram {
    fn drop(&mut self) {
        unsafe { glDeleteProgram(self.program) };
    }
}
impl Drop for BitmapProgram {
    fn drop(&mut self) {
        unsafe { glDeleteProgram(self.program) };
    }
}
impl Drop for GradientProgram {
    fn drop(&mut self) {
        unsafe { glDeleteProgram(self.program) };
    }
}
impl Drop for ShapeBitmapProgram {
    fn drop(&mut self) {
        unsafe { glDeleteProgram(self.program) };
    }
}

// ─── Filter programs ──────────────────────────────────────────────────────────

struct ColorMatrixFilterProgram {
    program: GLuint,
    u_src_uv: GLint,
    u_color_mat: GLint,
    u_color_extra: GLint,
}
struct BlurFilterProgram {
    program: GLuint,
    u_src_uv: GLint,
    u_blur_dir: GLint,
    u_blur_m: GLint,
    u_blur_m2: GLint,
    u_blur_full_size: GLint,
    u_blur_first_weight: GLint,
    u_blur_last_offset: GLint,
    u_blur_last_weight: GLint,
}
struct GlowFilterProgram {
    program: GLuint,
    u_src_uv: GLint,
    u_blur_uv: GLint,
    u_color: GLint,
    u_strength: GLint,
    u_inner: GLint,
    u_knockout: GLint,
    u_composite_source: GLint,
}
impl Drop for ColorMatrixFilterProgram {
    fn drop(&mut self) { unsafe { glDeleteProgram(self.program) }; }
}
impl Drop for BlurFilterProgram {
    fn drop(&mut self) { unsafe { glDeleteProgram(self.program) }; }
}
impl Drop for GlowFilterProgram {
    fn drop(&mut self) { unsafe { glDeleteProgram(self.program) }; }
}

struct BevelFilterProgram {
    program: GLuint,
    u_src_uv: GLint,
    u_blur_uv_l: GLint,
    u_blur_uv_r: GLint,
    u_highlight: GLint,
    u_shadow: GLint,
    u_strength: GLint,
    u_bevel_type: GLint,
    u_knockout: GLint,
}
impl Drop for BevelFilterProgram {
    fn drop(&mut self) { unsafe { glDeleteProgram(self.program) }; }
}

/// DisplacementMapFilter program (#42): source at unit 0, map at unit 1, plus the
/// per-filter args (components/mode/scale/dims/offset/viewscale).
struct DisplacementMapFilterProgram {
    program: GLuint,
    u_src_uv: GLint,
    u_color: GLint,
    u_map_remap: GLint,
    u_comp_x: GLint,
    u_comp_y: GLint,
    u_mode: GLint,
    u_scale: GLint,
    u_source_size: GLint,
    u_map_size: GLint,
    u_offset: GLint,
    u_viewscale: GLint,
}
impl Drop for DisplacementMapFilterProgram {
    fn drop(&mut self) { unsafe { glDeleteProgram(self.program) }; }
}

/// Two-texture programs for `render_alpha_mask` and complex `blend` modes.
/// Both reuse FILTER_VERT (a full-quad pass with `u_src_uv`), and sample a
/// second texture at unit 1 in addition to `u_tex` at unit 0.
struct AlphaMaskProgram {
    program: GLuint,
    u_src_uv: GLint,
}
/// Single-texture full-quad blit program (FILTER_VERT + a chosen fragment
/// shader). Used for the premultiplied<->straight conversions that move
/// render_offscreen results between premultiplied temps and straight atlas
/// slots without a CPU readback.
struct BlitProgram {
    program: GLuint,
    u_src_uv: GLint,
}
/// Full-frame screen filter (issue #65): one pass over the finished game frame,
/// on its way to the real framebuffer.
struct ScreenFilterProgram {
    program: GLuint,
    u_src_uv: GLint,
    u_res: GLint,
    u_scan: GLint,
    u_mode: GLint,
}
impl Drop for ScreenFilterProgram {
    fn drop(&mut self) { unsafe { glDeleteProgram(self.program) }; }
}
impl Drop for BlitProgram {
    fn drop(&mut self) { unsafe { glDeleteProgram(self.program) }; }
}
struct ComplexBlendProgram {
    program: GLuint,
    u_src_uv: GLint,
    u_blend_mode: GLint,
    u_current_flip: GLint,
    /// `(zoom, pan_x / width, pan_y / height)` — the free zoom (issue #101) as
    /// this composite has to undo it. Identity `(1, 0, 0)` everywhere else.
    u_cur_zoom: GLint,
}
impl Drop for AlphaMaskProgram {
    fn drop(&mut self) { unsafe { glDeleteProgram(self.program) }; }
}
impl Drop for ComplexBlendProgram {
    fn drop(&mut self) { unsafe { glDeleteProgram(self.program) }; }
}

// ─── Stencil mask state ───────────────────────────────────────────────────────

/// DIAGNOSTIC (2026-06-03, catmario invisible world): when true, maskees draw
/// unconditionally (GL_ALWAYS) instead of being gated by the stencil coverage
/// count. If the world (platforms/ground/enemies) appears with this on, the
/// invisible-world bug is in our stencil masking; if it stays invisible, the
/// cached content itself is empty/not composited. Set back to false after.
/// Result (2026-06-03): world stayed invisible with gating off → NOT masking;
/// the cached content path is the culprit. Left at false.
const DISABLE_MASK_GATING: bool = false;

/// DIAGNOSTIC (2026-07-31, Agent P level-select washed out): when true, complex
/// blends composite with the shader's NORMAL fallback (mode 7 hits `return s;`)
/// instead of Multiply/Overlay/HardLight. Everything else — the group temp, the
/// backdrop snapshot, the mask clipping, the composite draw — is unchanged.
///
/// So: if the tiles come out CORRECT with this on, the pipeline is sound and the
/// fault is in the blend function or the values fed to it. If the washed-out
/// rectangle survives, the group temp itself is wrong and the blend mode is a
/// red herring.
///
/// RESULT (2026-07-31): the cyan rectangle survived UNCHANGED with this on, so
/// the blend function and its inputs are NOT the fault. Left at false, and it
/// still works as written — mode 7 hits the shader's existing fallback, so no
/// diagnostic branch has to live in the fragment shader for it.
const FORCE_NORMAL_COMPLEX_BLEND: bool = false;

// Two further probes ran the same day and are NOT kept, because each needed its
// own branch in COMPLEX_BLEND_FRAG — per-pixel cost in a path that is already
// 13-17% of this screen's render time, for code that would never run again.
// Their results, so nobody repeats them:
//
//   - Painting the GROUP's alpha (shader mode 8) drew EXACTLY the level-3 number
//     and its padlock, not a rectangle: the group temps are CORRECT, and the
//     stencil-disabled group render is not the problem. It also showed the
//     composite covers the WHOLE target — outside the group the screen survives
//     only because the shader re-emits the backdrop.
//   - Painting the BACKDROP SNAPSHOT alone (mode 9) showed a faithful copy of
//     the screen, with the cyan rectangle ALREADY IN IT. The complex blend does
//     not draw it.
//
// Conclusion: the whole blend path is exonerated. Agent P's level-select is
// missing CONTENT (the tile-1 panel, the level 1/2 numbers), it is not
// mis-composited — so the next investigation belongs in the display list, not
// here. The mask/stencil restore fixed below was a real and separate bug.



/// DIAGNOSTIC TOGGLE: when true, mask shapes stay invisible but the stencil
/// gating is skipped so maskees draw unconditionally. Used to confirm whether
/// the SMWF overworld blank screen is caused by our stencil masking. Set back
/// to false once the masking bug is understood/fixed.
#[derive(Default, Clone, Copy)]
struct MaskState {
    /// Nesting depth: 0 = no mask, N = drawing the Nth maskee. Doubles as the
    /// stencil coverage count a maskee at this depth must equal.
    depth: u32,
    /// True while we are drawing a MASK shape into the stencil (between
    /// push_mask/deactivate_mask and the following activate_mask/pop). Draws
    /// issued in this phase write the stencil region; if none happen, the
    /// maskee is gated against an empty stencil → invisible.
    writing: bool,
}

// ─── The backend ──────────────────────────────────────────────────────────────

/// Cached GL state to avoid redundant calls. On Mesa-Switch each call goes
/// through the driver's command-buffer encoder; even if value-unchanged
/// dispatches are cheap on PC, they're measurable on Tegra X1. Mario 63 in
/// the worst frame (FLUDD rocket) issues ~3 shapes/frame × 5 draws each =
/// ~15 draws/frame all on `shape_bitmap_prog` with the same atlas texture
/// and the same wrap_mode. With this cache, only one glUseProgram +
/// one glBindTexture per such run reaches the driver.
///
/// Interior mutability via `Cell` so the `use_*` helpers can keep `&self`
/// without bubbling `&mut self` through every render path.
#[derive(Default)]
struct GlStateCache {
    last_program: Cell<GLuint>,
    last_texture: Cell<GLuint>,
    last_wrap_mode: Cell<i32>,
    /// Mirror of `BITMAP_FRAG`'s `u_premult`. Nearly every draw in a frame is
    /// straight-alpha, so the handful of standalone draws are the only ones
    /// that ever reach the driver with this.
    last_premult: Cell<i32>,
    last_vao: Cell<GLuint>,
    /// Sampler object on texture unit 0. 0 means "none", so GL falls back to
    /// the texture's own parameters — which are LINEAR + CLAMP_TO_EDGE for
    /// every texture this project creates, making 0 exactly the behaviour that
    /// shipped before per-draw filtering existed.
    ///
    /// Unlike the fields above, this one is not only a mirror: a sampler really
    /// stays bound until something unbinds it, so `invalidate` has to put it
    /// back rather than merely forget it.
    last_sampler: Cell<GLuint>,
}

impl GlStateCache {
    /// Forget what we cached. Call at submit_frame start (after we know
    /// any external glXxx calls have potentially mutated state) and at end
    /// (where we reset GL to zero anyway).
    fn invalidate(&self) {
        self.last_program.set(0);
        self.last_texture.set(0);
        self.last_wrap_mode.set(-1);
        self.last_premult.set(-1);
        self.last_vao.set(0);
        // Not just forgotten: released. Every invalidate marks a hand-off to raw
        // GL, and a point sampler left on unit 0 would follow whoever runs next.
        self.clear_sampler();
    }

    /// Choose the sampler object for texture unit 0. 0 = none, i.e. keep the
    /// texture's own filtering.
    fn bind_sampler(&self, sampler: GLuint) {
        if self.last_sampler.get() != sampler {
            unsafe { glBindSampler(0, sampler) };
            self.last_sampler.set(sampler);
        }
    }

    /// Drop back to the texture's own filtering on unit 0.
    ///
    /// Any raw-GL pass that samples a texture without going through the `use_*`
    /// helpers must call this first. Filter passes are the ones that cannot
    /// survive a leaked point sampler: a blur interpolates between texels by
    /// construction, so sampling it NEAREST does not give a slightly different
    /// blur, it gives a mosaic.
    fn clear_sampler(&self) {
        self.bind_sampler(0);
    }

    fn use_program(&self, prog: GLuint) {
        if self.last_program.get() != prog {
            unsafe { glUseProgram(prog) };
            self.last_program.set(prog);
        }
    }

    fn bind_texture_unit0(&self, tex: GLuint) {
        if self.last_texture.get() != tex {
            unsafe {
                glActiveTexture(GL_TEXTURE0);
                glBindTexture(GL_TEXTURE_2D, tex);
            }
            self.last_texture.set(tex);
        }
    }

    fn set_wrap_mode(&self, location: GLint, mode: i32) {
        if self.last_wrap_mode.get() != mode {
            unsafe { glUniform1i(location, mode) };
            self.last_wrap_mode.set(mode);
        }
    }

    /// Alpha convention of the texture bound for `BITMAP_FRAG`. Same deal as
    /// `set_wrap_mode`: one `glUniform1i` only when the run of draws changes
    /// convention, which in practice is a few times a frame.
    fn set_premult(&self, location: GLint, premult: bool) {
        let v = i32::from(premult);
        if self.last_premult.get() != v {
            unsafe { glUniform1i(location, v) };
            self.last_premult.set(v);
        }
    }

    fn bind_vao(&self, vao: GLuint) {
        if self.last_vao.get() != vao {
            unsafe { glBindVertexArray(vao) };
            self.last_vao.set(vao);
        }
    }
}

pub struct SwitchRenderBackend {
    dimensions: ViewportDimensions,
    tessellator: ShapeTessellator,

    solid: SolidProgram,
    bitmap_prog: BitmapProgram,
    shape_bitmap_prog: ShapeBitmapProgram,
    gradient_prog: GradientProgram,

    /// Mesa-Switch GL state cache. See `GlStateCache` docs above.
    gl_state: GlStateCache,

    /// Point-sampling sampler object, bound on unit 0 for the draws that ask for
    /// it (see `sampler_for`). 0 if `glGenSamplers` failed, which simply leaves
    /// every draw on the texture's LINEAR, i.e. the old picture.
    sampler_sharp: GLuint,

    /// Solid unit quad (pos+rgba, 6 vertices). Used by `draw_rect`.
    rect_vao: GLuint,
    rect_vbo: GLuint,

    /// Bitmap unit quad (pos+uv, 6 vertices). Used by `render_bitmap`.
    bitmap_vao: GLuint,
    bitmap_vbo: GLuint,

    /// Shared-font glyph batch (pos+uv, dynamic). Its own pair on purpose —
    /// see `build_atlas_batch`.
    atlas_vao: GLuint,
    atlas_vbo: GLuint,

    /// Unit line (pos+rgba, 2 vertices). Used by `draw_line`.
    line_vao: GLuint,
    line_vbo: GLuint,

    /// Unit line-rect (pos+rgba, 5 vertices using GL_LINE_LOOP-equivalent
    /// via two segments × 4 — simpler: 4 separate GL_LINES, 8 verts).
    line_rect_vao: GLuint,
    line_rect_vbo: GLuint,

    mask: MaskState,
    warned_unsupported: u32,
    /// Frame counter for periodic `glGetError` polling.
    frame_count: u32,
    /// Bounding box of this window's draws, in viewport pixels (see note_draw_extent).
    draw_extent: Option<(f32, f32, f32, f32)>,
    /// Largest alpha multiplier seen this window: 0 means every draw was fully
    /// transparent, which looks exactly like drawing nothing.
    draw_max_alpha: f32,
    /// The one texture write held back until something reads a texture.
    pending_upload: Option<PendingUpload>,
    /// Buffer handed back by the last flush, so the next write refills it
    /// instead of allocating several megabytes again.
    upload_scratch: Vec<u8>,
    /// Diagnostic counters: how many shapes/bitmaps registered so far.
    shapes_registered: u32,
    bitmaps_registered: u32,
    bitmap_draws_emitted: u32,
    bitmap_render_count: u32,
    /// Big-surface memory tracking (Super Bowser World cinematic OOM, #56b
    /// follow-up). A "big" bitmap is one that gets a right-sized dedicated atlas
    /// (> ATLAS_SIZE/2 in a dimension, e.g. 1824×1174 = ~8.5 MB). These dominate
    /// memory: the game spawns dozens at the cutscene→gameplay transition. We
    /// track live bytes to (a) decide leak-vs-genuine-demand from the logs and
    /// (b) refuse new ones past `BIG_ATLAS_BUDGET_BYTES` so we degrade (invisible
    /// surface) instead of an OOM hard-crash.
    big_atlas_live_bytes: u64,
    big_atlas_peak_bytes: u64,
    big_atlas_alloc_total: u32,
    big_atlas_free_total: u32,
    big_atlas_dropped_total: u32,
    /// New atlases the GPU heap had no room for (`pack_into_atlas` fell back).
    atlas_alloc_failures: u32,
    /// Atlases made since the last `evict:` line.
    atlas_created: u32,
    /// Evictable SWF images (see `ReloadSlot`); `evict.off` turns them off.
    evict_on: bool,
    /// Since the last `evict:` line: atlases emptied, images and bytes evicted,
    /// images decoded again (bytes, time), decodes that failed.
    evict_atlases: u32,
    evict_images: u32,
    evict_bytes: u64,
    evict_reloads: u32,
    evict_reload_bytes: u64,
    evict_reload_ticks: u64,
    evict_reload_fail: u32,
    /// System tick at the start of the current heartbeat window (60 frames).
    /// Set to 0 on first heartbeat; FPS measurement skipped until we have
    /// two samples to subtract. Uses `armGetSystemTick` for high resolution
    /// — at ~19.2 MHz a 60-frame window resolves ~50 ns granularity, way
    /// better than what FPS measurement needs.
    heartbeat_tick: u64,
    /// Number of GL draw calls (glDrawElements*/glDrawArrays) emitted since
    /// the last heartbeat. Helps correlate FPS drops with draw-call count
    /// — if it spikes from ~30 to ~300, the next perf step is batching.
    draw_calls_this_window: u32,
    /// Mask diagnostics, reset per heartbeat window. `push_mask_window` counts
    /// `push_mask` calls; `alpha_mask_window` counts `render_alpha_mask` (which
    /// we currently SKIP — non-zero on a blank screen would explain it);
    /// `masked_draw_window` counts shape/bitmap draws issued while a stencil
    /// mask is active (gated on stencil EQUAL). If a screen draws thousands of
    /// masked things but shows nothing, the mask shape isn't writing stencil.
    push_mask_window: u32,
    alpha_mask_window: u32,
    masked_draw_window: u32,
    /// Draws issued while writing a mask shape into the stencil (writing=true).
    /// If this is ~0 while `masked_draw_window` is large, mask shapes aren't
    /// producing stencil → maskee gated empty → everything masked is invisible.
    mask_shape_draw_window: u32,
    /// Max `cache_entries` count in any frame of the current window. A periodic
    /// spike (e.g. once/sec) means an HUD/text element is re-caching + (with
    /// filters on) re-filtering on a timer — the idle-stutter suspect.
    cache_entries_max_window: u32,
    /// DIAG round 2: which filter-chain budget this frame actually ran under,
    /// printed on the SLOW line so each frame can be bucketed by regime.
    filter_budget_this_frame: u32,
    /// How many times Ruffle has called `render_offscreen` since boot —
    /// non-zero means something on stage uses `cacheAsBitmap` or a filter.
    /// Logged every heartbeat so we can correlate spikes with crashes.
    render_offscreen_calls: u32,
    /// How many times Ruffle has called `apply_filter` since boot.
    apply_filter_calls: u32,
    /// How many times we've read a BitmapData.draw() result back to the CPU
    /// (`resolve_sync_handle`). Non-zero confirms the tile-engine readback path
    /// (SMWF terrain) is firing.
    resolve_sync_calls: u32,
    /// One bit per `Filter` variant we've seen via `is_filter_supported`,
    /// so each variant is logged the first time only. Variant ordinals
    /// match `filter_variant_ordinal()`. `Cell` would be simpler but
    /// `is_filter_supported` takes `&self`, so we use an atomic.
    filters_seen_mask: AtomicU16,
    /// Pool of texture atlases. New atlases get appended when current is
    /// full. Bitmaps are packed into these instead of getting individual
    /// GL textures.
    atlases: Vec<Atlas>,

    /// Single global VBO for all shape draws (suballocated via freelist).
    /// All `GpuDraw::vbo_offset` are byte offsets into this buffer.
    vertex_arena: BufferArena,
    /// Single global IBO for all shape draws.
    index_arena: BufferArena,
    /// `<game>.swf.arena` for the game this backend runs (see `ARENA_RECORD_*`),
    /// None for the launcher.
    arena_record_path: Option<std::string::String>,
    /// What that file holds now, so a heartbeat writes only when it changes.
    arena_record_written: (u64, u64, bool),
    /// Single VAO used for every shape draw. Pre-configured at boot to
    /// read (pos.xy, rgba) from `vertex_arena` with stride 24, and to use
    /// `index_arena` as the element buffer. Each draw shifts the read
    /// origin via `glDrawElementsBaseVertex(base_vertex)`.
    shape_vao: GLuint,

    /// When `Some((w, h))`, `world_matrix` targets an offscreen FBO of that
    /// size (no Y-flip) instead of the main framebuffer. Set while replaying
    /// commands into a cache texture. Commands are pre-shifted by Ruffle to
    /// target-local coords, so no origin offset is needed.
    offscreen_dims: Option<(u32, u32)>,
    /// The GL texture currently attached to `offscreen_fbo` while replaying
    /// commands into it (set alongside `offscreen_dims`). A nested trivial blend
    /// (Add/Subtract/Screen inside a BitmapData.draw / cache render) needs this
    /// to RE-ATTACH the outer target after rendering its group into a pooled
    /// temp — the temp render detaches the colour attachment, so without this we
    /// couldn't composite the blended group back onto the enclosing offscreen.
    offscreen_target_tex: Option<GLuint>,
    /// Global pixel translation folded into `world_matrix` for the LIBRARY UI
    /// only (v1.2.0 polish). Lets `library::render` slide a whole screen's
    /// content for tab transitions / modal pops without every draw call knowing
    /// about it. Always 0 during in-game / offscreen rendering (set + reset
    /// around the library content draw, so the navbar and Ruffle are untouched).
    ui_translate_x: f32,
    ui_translate_y: f32,
    /// Uniform scale about (`ui_pivot_x`, `ui_pivot_y`) for the modal open/close
    /// pop. 1.0 = identity (always so in-game / offscreen).
    ui_scale: f32,
    ui_pivot_x: f32,
    ui_pivot_y: f32,
    /// Reusable FBO object (lazy; 0 = not created). Color attachment is
    /// rebound per offscreen render.
    offscreen_fbo: GLuint,
    /// Shared depth+stencil renderbuffer attached to `offscreen_fbo`, so
    /// stencil masks pushed by `commands.execute()` work inside the FBO.
    /// Grows monotonically; attached once.
    offscreen_depth_stencil: GLuint,
    offscreen_depth_stencil_dims: (u32, u32),
    /// Colour-only FBO for filter passes (lazy; 0 = not created).
    ///
    /// Filter passes used to share `offscreen_fbo`, which carries the
    /// depth+stencil renderbuffer above. That renderbuffer only ever grows and
    /// is attached for the process lifetime, so once any offscreen render had
    /// asked for a large target, every later filter pass ran against a
    /// full-size D24S8 — while `draw_filter_pass` had just disabled the stencil
    /// test and needed neither buffer. On a tile-based GPU that is what turns a
    /// 200x100 quad into a full tile-configuration cycle.
    ///
    /// Measured 2026-08-24: 23 chains cost 105 ms of render, ~0.77 ms per
    /// render-target rebind, with the fill itself already half-res and capped
    /// at one blur pass. The rebind is the unit of cost, so the attachment it
    /// drags along is what to remove. Nothing is ever attached here but colour.
    filter_fbo: GLuint,

    /// Screen-filter pass (issue #65). Built and allocated ON FIRST USE, so a
    /// player who never turns a filter on pays neither the shader compile at
    /// launch nor the 3.5 MB render target.
    screen_filter: Option<ScreenFilterProgram>,
    screen_filter_fbo: GLuint,
    screen_filter_tex: GLuint,
    /// Own depth+stencil, NOT the shared `offscreen_depth_stencil`: stencil masks
    /// are pushed by `commands.execute()` into whatever target is bound, so the
    /// frame target needs its own attachment, and keeping it separate leaves the
    /// offscreen temp machinery (and its long bug history) untouched.
    screen_filter_rbo: GLuint,
    screen_filter_dims: (u32, u32),
    /// Framebuffer that was bound when the frame started, restored by the resolve
    /// pass. Captured rather than assumed to be 0: the frame's target is whatever
    /// the C++ side bound, exactly like the offscreen paths' `prev_fbo`.
    screen_filter_prev_fbo: GLint,

    color_matrix_filter: ColorMatrixFilterProgram,
    unpremult_blit: BlitProgram,
    premult_blit: BlitProgram,
    blur_filter: BlurFilterProgram,
    glow_filter: GlowFilterProgram,
    bevel_filter: BevelFilterProgram,
    displacement_filter: DisplacementMapFilterProgram,
    /// Two-texture composite programs for soft alpha masks + complex blends.
    alpha_mask_prog: AlphaMaskProgram,
    complex_blend_prog: ComplexBlendProgram,
    /// How many times `blend` ran a real (non-Normal) composite this window,
    /// and `render_alpha_mask` ran a soft-mask composite. Surfaced in the
    /// heartbeat so a blank/wrong screen can be correlated with these paths.
    blend_window: u32,
    /// Pool of standalone textures reused across filter passes within a
    /// single submit_frame, keyed by `(width, height)`. Avoids paying
    /// glGenTextures + glTexImage2D + glDeleteTextures per filter per
    /// frame, which was the main fps killer in Phase 2.3's first try.
    filter_tex_pool: FilterTexturePool,
    /// libdrm buffer-object cache counters: the last running totals read, and
    /// this frame's share of them (the two held counts are levels, not deltas).
    boc_prev: [u64; BOC_STATS],
    boc_frame: [u64; BOC_STATS],

    /// Reusable temp textures for `render_offscreen` (BitmapData.draw /
    /// cacheAsBitmap). `_pool` holds textures free for reuse; `_retired` holds
    /// the ones handed to this frame's SyncHandles; `submit_frame` moves
    /// `_retired` back into `_pool`. This avoids a per-call glGenTextures +
    /// glTexImage2D + glDeleteTextures — which became the dominant cost
    /// (~90ms/frame, 48 allocs) once the readback was moved onto the GPU.
    ///
    /// ⚠️ A pooled temp can still be the target of a LIVE SyncHandle: Ruffle
    /// parks `DirtyState::GpuModified(handle, region)` in the BitmapData and only
    /// resolves it on the next CPU access, which may never come this frame. A
    /// standalone-backed draw is immune (its handle points at the BitmapData's
    /// own texture, see render_offscreen), but an atlas-backed one still points
    /// HERE. Freeing a temp out from under such a handle reads back empty, which
    /// is #14 (Papa Louie's missing sprites). Eviction is therefore driven by
    /// RECENCY, not by a byte budget: a temp still in play is never freed.
    offscreen_temp_pool: Vec<PooledTemp>,
    offscreen_temp_retired: Vec<StandaloneTexture>,
    /// Bytes the pool currently holds, for the heartbeat. NB the neighbouring
    /// `fpool=` is the FILTER texture pool, a different pool — this is `otpool=`.
    offscreen_temp_pool_bytes: usize,

    /// Per-frame perf attribution for the slow-frame detector. `frame_snapshot`
    /// is the raw counter state captured at the top of `submit_frame`;
    /// `last_frame` is the delta of the frame that just finished. lib.rs reads
    /// `last_frame` whenever a frame blows the FPS budget, so an FPS spike can
    /// be pinned on what the frame actually did (offscreen filter passes,
    /// bitmap uploads, shape tessellation, draw-call count, …). Cumulative
    /// counters (offscreen/filter/resolve/bmp/shape) are exact; the window
    /// counters (dc/blend/pmask/mdraw) under-report on the 1-in-60 heartbeat
    /// frame because the heartbeat zeroes them mid-`submit_frame`.
    frame_snapshot: FrameBreakdown,
    last_frame: FrameBreakdown,
    /// Lazily-built CJK glyph atlas (Chinese etc.) rasterized from the Switch
    /// shared system font. `None` until the first non-bitmap glyph is drawn;
    /// `atlas_init_done` guards against re-trying a failed init every frame.
    /// Declared last so it drops (freeing its GL texture) after the struct's
    /// own `Drop` body has run, while the GL context is still alive.
    font_atlas: Option<crate::backend::glyphs::FontAtlas>,
    atlas_init_done: bool,
    /// The interface kit's fonts and atlas (`render/kit/text.rs`), built on
    /// first use like `font_atlas`. Declared after the struct's own fields for
    /// the same reason: it frees GL textures in its Drop.
    kit_text: Option<std::boxed::Box<kit::text::UiText>>,
    kit_text_tried: bool,
    /// The kit's rounded-rectangle shader (`render/kit/shapes.rs`).
    kit_shapes: Option<kit::shapes::ShapeProgram>,
    kit_shapes_tried: bool,
    /// True only while the GAME's display list is being replayed to the screen,
    /// which is the one thing the free zoom (issue #101) applies to. Everything
    /// drawn around it -- the pause panel, the pointer, the zoom legend -- runs
    /// with this false and keeps its size.
    game_layer: bool,
}

/// One frame's worth of per-counter activity (or the raw snapshot used to
/// derive it). All fields are deltas in `last_frame`. Logged by the slow-frame
/// detector — see `SwitchRenderBackend::log_slow_frame`.
#[derive(Clone, Copy, Default)]
struct FrameBreakdown {
    /// GL draw calls (glDrawElements*/glDrawArrays) emitted this frame.
    draw_calls: u32,
    /// `render_offscreen` calls — cacheAsBitmap / filter source renders.
    offscreen: u32,
    /// `apply_filter` calls — individual blur/glow/bevel/color-matrix passes.
    filter: u32,
    /// `resolve_sync_handle` readbacks (BitmapData.draw() → CPU).
    resolve: u32,
    /// Bitmaps registered (texture uploads) this frame.
    bmp_uploads: u32,
    /// Shapes registered (tessellation) this frame.
    shape_regs: u32,
    /// Non-Normal blend composites run this frame.
    blend: u32,
    /// `push_mask` calls this frame.
    pushmask: u32,
    /// Draws issued under an active stencil mask this frame.
    masked_draw: u32,
    /// cacheAsBitmap entries processed by `submit_frame` this frame.
    cache_entries: u32,
    /// Filter chains actually run this frame (bounded by the per-frame budget).
    filter_chains: u32,
    /// DIAG round 2: the budget this frame ran under, so the alternating probe
    /// can be bucketed by regime when reading the log back.
    filter_budget: u32,
}

/// Pool of `StandaloneTexture` keyed by `(width, height)`. Acquire pulls an
/// existing entry of the right size or makes a fresh one; release pushes it
/// back for the next caller. Each entry is RGBA8 with linear sampling and
/// clamp-to-edge wrap — same setup as `make_standalone_texture`.
///
/// Reusing entries across filter passes prevents the per-frame texture
/// alloc/free thrash that brought Mario 63 down to 5 fps in the prior patch.
/// How many frames a pooled texture survives without being reused before the
/// pool frees it. 2 = "used this frame or last frame stays". This bounds the
/// pool to the recent working set: a stable filtered scene reuses every
/// texture each frame (0 reallocations after the first frame), while sizes
/// that stop appearing are reclaimed within 2 frames — preventing the
/// unbounded session-long growth that exhausted GL textures (→ glGenTextures
/// 0 → Mesa NULL-deref crash). A fixed COUNT cap was worse: once full of stale
/// sizes it blocked new ones, thrashing alloc/free every frame.
const FILTER_POOL_TTL_FRAMES: u64 = 2;

struct FilterTexturePool {
    /// Each entry carries the frame it was last released, for TTL eviction.
    buckets: std::collections::HashMap<(u32, u32), Vec<(StandaloneTexture, u64)>>,
    /// Total retained (for the heartbeat).
    pooled: usize,
    /// Set by `begin_frame`; `release` stamps freed textures with it.
    current_frame: u64,
}

impl FilterTexturePool {
    fn new() -> Self {
        Self { buckets: std::collections::HashMap::new(), pooled: 0, current_frame: 0 }
    }
    /// Reclaim textures not reused within `FILTER_POOL_TTL_FRAMES`. Called once
    /// per `submit_frame` before the cache_entries filter chain runs.
    fn begin_frame(&mut self, frame: u64) {
        let _pt = PrimTimer::new(&TEX_BF_T_CUR);
        self.current_frame = frame;
        let keep_from = frame.saturating_sub(FILTER_POOL_TTL_FRAMES - 1);
        let mut evicted = 0u64;
        for bucket in self.buckets.values_mut() {
            let before = bucket.len();
            bucket.retain(|(_, f)| *f >= keep_from); // dropped entries free their GL texture
            self.pooled -= before - bucket.len();
            evicted += (before - bucket.len()) as u64;
        }
        self.buckets.retain(|_, v| !v.is_empty());
        TEX_EVICT_N_CUR.fetch_add(evicted, std::sync::atomic::Ordering::Relaxed);
    }
    fn acquire(&mut self, w: u32, h: u32) -> Option<StandaloneTexture> {
        if let Some(bucket) = self.buckets.get_mut(&(w, h)) {
            if let Some((tex, _)) = bucket.pop() {
                self.pooled = self.pooled.saturating_sub(1);
                return Some(tex);
            }
        }
        make_standalone_texture(w, h)
    }
    fn release(&mut self, tex: StandaloneTexture) {
        let key = (tex.width, tex.height);
        let f = self.current_frame;
        self.buckets.entry(key).or_default().push((tex, f));
        self.pooled += 1;
    }
    fn len(&self) -> usize { self.pooled }
}

/// Returns a stable 0..=9 ordinal + short name for a `Filter` variant so we
/// can dedupe `is_filter_supported` logs without allocating a HashSet.
fn filter_variant_ordinal(f: &Filter) -> (u8, &'static str) {
    match f {
        Filter::BevelFilter(_) => (0, "Bevel"),
        Filter::BlurFilter(_) => (1, "Blur"),
        Filter::ColorMatrixFilter(_) => (2, "ColorMatrix"),
        Filter::ConvolutionFilter(_) => (3, "Convolution"),
        Filter::DisplacementMapFilter(_) => (4, "DisplacementMap"),
        Filter::DropShadowFilter(_) => (5, "DropShadow"),
        Filter::GlowFilter(_) => (6, "Glow"),
        Filter::GradientBevelFilter(_) => (7, "GradientBevel"),
        Filter::GradientGlowFilter(_) => (8, "GradientGlow"),
        Filter::ShaderFilter(_) => (9, "Shader"),
    }
}

// ─── Shaders source ───────────────────────────────────────────────────────────

const SOLID_VERT: &[u8] = b"#version 330 core\n\
layout(location = 0) in vec2 a_pos;\n\
layout(location = 1) in vec4 a_col;\n\
uniform mat3 u_world;\n\
out vec4 v_col;\n\
void main() {\n\
    vec3 p = u_world * vec3(a_pos, 1.0);\n\
    gl_Position = vec4(p.xy, 0.0, 1.0);\n\
    v_col = a_col;\n\
}\n\0";

const SOLID_FRAG: &[u8] = b"#version 330 core\n\
in vec4 v_col;\n\
out vec4 frag_color;\n\
uniform vec4 u_mult;\n\
uniform vec4 u_add;\n\
void main() {\n\
    frag_color = clamp(v_col * u_mult + u_add, 0.0, 1.0);\n\
}\n\0";

const BITMAP_VERT: &[u8] = b"#version 330 core\n\
layout(location = 0) in vec2 a_pos;\n\
layout(location = 1) in vec2 a_uv;\n\
uniform mat3 u_world;\n\
uniform vec4 u_uv_remap;\n\
out vec2 v_uv;\n\
void main() {\n\
    vec3 p = u_world * vec3(a_pos, 1.0);\n\
    gl_Position = vec4(p.xy, 0.0, 1.0);\n\
    v_uv = u_uv_remap.xy + a_uv * u_uv_remap.zw;\n\
}\n\0";

/// `u_premult` says which alpha convention the bound texture uses, and that
/// changes the colour-transform MATHS, not just the blend.
///
/// Atlas bitmaps hold STRAIGHT alpha, so `colour * mult + add` is already the
/// Flash formula and the straight-alpha blend applies coverage afterwards.
/// Standalone textures (cacheAsBitmap, filter results, `BitmapData.draw`) hold
/// PREMULTIPLIED alpha and are drawn with `glBlendFunc(GL_ONE, ...)`, which
/// adds the fragment's RGB to the target with no coverage weighting at all.
/// Running the straight-alpha formula on one of those wrote `u_add` over the
/// whole quad, transparent margin included: Mario 63's level 8-13 tints its
/// meteor layer by +31 (`8-13BG`, character 5893, places its child with
/// `add = [31, 31, 31, 0]`) and every blurred meteor under it — a filter forces
/// a bitmap cache, so each one comes back through here — sat inside a flat
/// #1F1F1F rectangle the size of its cache texture. 31 / 255 = 0.1216, which is
/// exactly the grey that showed up on screen.
///
/// The premultiplied branch therefore does what BOTH upstream backends do
/// (wgpu `bitmap.wgsl`, webgl `bitmap.frag`): unmultiply, transform, remultiply,
/// the whole thing skipped when alpha is zero. Skipping is what removes the
/// rectangle; unmultiplying is what stops the same `add` washing out the blurred
/// edge, which is the identical bug one alpha step further in.
///
/// The branch is uniform across a draw, so it costs a predicated jump and
/// nothing else — which matters, since this is the hottest fragment shader here.
const BITMAP_FRAG: &[u8] = b"#version 330 core\n\
in vec2 v_uv;\n\
out vec4 frag_color;\n\
uniform sampler2D u_tex;\n\
uniform vec4 u_mult;\n\
uniform vec4 u_add;\n\
uniform int u_premult;\n\
void main() {\n\
    vec4 c = texture(u_tex, v_uv);\n\
    if (u_premult == 0) {\n\
        frag_color = clamp(c * u_mult + u_add, 0.0, 1.0);\n\
    } else if (c.a > 0.0) {\n\
        vec4 s = clamp(vec4(c.rgb / c.a, c.a) * u_mult + u_add, 0.0, 1.0);\n\
        frag_color = vec4(s.rgb * s.a, s.a);\n\
    } else {\n\
        frag_color = c;\n\
    }\n\
}\n\0";

/// Vertex shader for gradient draws: just like solid except we forward the
/// pre-projection position (`a_pos`) so the frag can compute gradient-local
/// coords from it.
const GRADIENT_VERT: &[u8] = b"#version 330 core\n\
layout(location = 0) in vec2 a_pos;\n\
uniform mat3 u_world;\n\
out vec2 v_pos;\n\
void main() {\n\
    vec3 p = u_world * vec3(a_pos, 1.0);\n\
    gl_Position = vec4(p.xy, 0.0, 1.0);\n\
    v_pos = a_pos;\n\
}\n\0";

/// Vertex shader for bitmap fills inside shapes: computes the per-bitmap UV
/// from `u_uv * a_pos` (matrix already pre-inverted by `swf_bitmap_to_gl_matrix`)
/// and passes it through unmodified. The fragment shader handles wrap mode
/// (fract for repeating fills, clamp otherwise) BEFORE remapping into the
/// atlas sub-rect — doing fract/clamp before remap is critical, since the
/// atlas places multiple bitmaps in one texture and remapping an out-of-
/// range UV would index into a neighbour bitmap (visible bug: Mario 63's
/// ground tile showed Mario's hat sprite).
///
/// The clamp branch stops half a texel short of each edge (`u_uv_inset`), which
/// is where the slot's texel CENTRES are. `clamp(.., 0.0, 1.0)` returning
/// exactly 1.0 — which it does for every tessellated triangle that overruns the
/// bitmap rect — lands a NEAREST sample on the first ATLAS_PAD texel, one past
/// the content. `upload_region_padded` seeds that pad with a copy of the edge
/// pixel, so the colour was right; but `update_texture` writes only the content
/// rect (`flush_pending_upload` -> `upload_region`), so on any bitmap Ruffle
/// rewrites the pad goes stale and keeps whatever it held at registration. Under
/// LINEAR that was half of one row and nobody ever saw it. Under NEAREST it is
/// the whole row. The inset is what CLAMP_TO_EDGE would do if the bitmap owned
/// its own texture, and it is a no-op under LINEAR whenever the pad is fresh
/// (the pad IS the edge pixel, so blending the two changes nothing).
///
/// The fract branch gets no inset, deliberately: `fract` is in [0, 1), so it can
/// never reach the pad, and a repeating fill must keep sampling the true first
/// and last texel at the seam.
const SHAPE_BITMAP_VERT: &[u8] = b"#version 330 core\n\
layout(location = 0) in vec2 a_pos;\n\
uniform mat3 u_world;\n\
uniform mat3 u_uv;\n\
out vec2 v_uv_local;\n\
void main() {\n\
    vec3 p = u_world * vec3(a_pos, 1.0);\n\
    gl_Position = vec4(p.xy, 0.0, 1.0);\n\
    vec3 uv = u_uv * vec3(a_pos, 1.0);\n\
    v_uv_local = uv.xy;\n\
}\n\0";

const SHAPE_BITMAP_FRAG: &[u8] = b"#version 330 core\n\
in vec2 v_uv_local;\n\
out vec4 frag_color;\n\
uniform sampler2D u_tex;\n\
uniform vec4 u_mult;\n\
uniform vec4 u_add;\n\
uniform vec4 u_uv_remap;\n\
uniform vec2 u_uv_inset;\n\
uniform int u_wrap_mode;\n\
void main() {\n\
    vec2 local = (u_wrap_mode == 1) ? fract(v_uv_local) : clamp(v_uv_local, u_uv_inset, 1.0 - u_uv_inset);\n\
    vec2 atlas_uv = u_uv_remap.xy + local * u_uv_remap.zw;\n\
    vec4 c = texture(u_tex, atlas_uv);\n\
    frag_color = clamp(c * u_mult + u_add, 0.0, 1.0);\n\
}\n\0";

// `u_grad_local` here is the matrix produced by ruffle's `swf_to_gl_matrix`
// — already inverted *and* normalised so that `lp.x` is the linear gradient
// parameter in [0, 1], and `(lp.xy - 0.5)` is the radial offset (the
// gradient circle has radius 0.5, centred at (0.5, 0.5)).
const GRADIENT_FRAG: &[u8] = b"#version 330 core\n\
in vec2 v_pos;\n\
out vec4 frag_color;\n\
uniform sampler2D u_tex;\n\
uniform mat3 u_grad_local;\n\
uniform int u_grad_kind;\n\
uniform int u_grad_spread;\n\
uniform float u_grad_focal;\n\
uniform vec4 u_mult;\n\
uniform vec4 u_add;\n\
\n\
float apply_spread(float t, int mode) {\n\
    if (mode == 0) return clamp(t, 0.0, 1.0);\n\
    if (mode == 2) return fract(t);\n\
    float f = fract(t * 0.5) * 2.0;\n\
    return f > 1.0 ? 2.0 - f : f;\n\
}\n\
\n\
void main() {\n\
    vec3 lp = u_grad_local * vec3(v_pos, 1.0);\n\
    float t;\n\
    if (u_grad_kind == 0) {\n\
        // Linear: lp.x is already the gradient parameter.\n\
        t = lp.x;\n\
    } else {\n\
        // Radial / focal: centre at (0.5, 0.5), radius 0.5 -> multiply by 2.\n\
        vec2 d = lp.xy - vec2(0.5);\n\
        t = length(d) * 2.0;\n\
        if (u_grad_kind == 2) {\n\
            // Focal: very rough offset, good enough as a placeholder.\n\
            t = clamp(t + u_grad_focal * d.x * 2.0, 0.0, 1.0);\n\
        }\n\
    }\n\
    t = apply_spread(t, u_grad_spread);\n\
    vec4 c = texture(u_tex, vec2(t, 0.5));\n\
    frag_color = clamp(c * u_mult + u_add, 0.0, 1.0);\n\
}\n\0";

// ─── Filter shaders ───────────────────────────────────────────────────────────
//
// Ported from `third_party/ruffle/render/wgpu/shaders/filter/{blur,glow,color_matrix}.wgsl`
// with one convention difference: no Y-flip in the vertex stage. wgpu's filter
// vertex shader does `vec4(pos.x*2-1, 1-pos.y*2, ...)` to compensate for its
// top-left texture origin; GL stores texel(0,0) at bottom-left so the no-flip
// version is correct here.
//
// All filter passes share: unit quad input (pos.xy in [0,1]², matching
// `build_bitmap_quad`), `u_src_uv` re-mapping the [0,1] UV into a sub-rect of
// the source texture, and `u_tex` sampler bound at unit 0 (set at link time).

const FILTER_VERT: &[u8] = b"#version 330 core\n\
layout(location = 0) in vec2 a_pos;\n\
layout(location = 1) in vec2 a_uv;\n\
uniform vec4 u_src_uv;\n\
out vec2 v_uv;\n\
void main() {\n\
    gl_Position = vec4(a_pos.x * 2.0 - 1.0, a_pos.y * 2.0 - 1.0, 0.0, 1.0);\n\
    v_uv = u_src_uv.xy + a_uv * u_src_uv.zw;\n\
}\n\0";

/// Screen filter picked for the game being played: 0 none, 1 scanlines, 2 CRT.
/// An atomic rather than backend state because the pause menu that sets it runs
/// on the C++ side, with no borrow of the renderer available. Read once per
/// frame; 0 means the frame path is untouched.
static SCREEN_FILTER: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

/// Select the screen filter. Takes effect on the very next frame, which is what
/// makes the pause menu able to preview it on the frozen picture.
pub fn set_screen_filter(v: u8) {
    SCREEN_FILTER.store(v, std::sync::atomic::Ordering::Relaxed);
}

/// Texture sampling picked for the game being played: 0 AUTO, 1 SMOOTH (force
/// bilinear), 2 SHARP (force nearest). Issue #108. An atomic for the same
/// reason as the screen filter above -- the pause menu that sets it runs on the
/// C++ side, with no borrow of the renderer to hand.
///
/// AUTO is 0 so a game nobody has set gets the FAITHFUL behaviour, not a
/// preference: Ruffle hands `render_bitmap` the SWF's own `smoothing` flag, and
/// a pixel-art SWF sets it false precisely so the player point-samples. Forcing
/// bilinear on those is a difference from Flash Player, not only a missing
/// option.
static PIXEL_FILTER: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

/// Select the texture sampling. Read per draw, so it takes effect on the very
/// next frame -- which is what lets the pause menu preview it on the frozen
/// picture, like every other ECRAN row.
pub fn set_pixel_filter(v: u8) {
    PIXEL_FILTER.store(v, std::sync::atomic::Ordering::Relaxed);
}

/// The mode the sampling code resolves against the per-bitmap `smoothing` flag:
/// 1 forces linear, 2 forces nearest, 0 follows the flag.
pub fn pixel_filter() -> u8 {
    PIXEL_FILTER.load(std::sync::atomic::Ordering::Relaxed)
}

/// How SWF bitmaps are filtered. AUTO is the faithful setting, not the neutral
/// one: a SWF states per bitmap whether it wants smoothing, Flash point-sampled
/// the ones that said no, and we smoothed all of them regardless, which is why
/// pixel art has always come out of FlashNX soft. The two overrides exist
/// because plenty of files leave the flag at a default that contradicts what
/// they obviously wanted.
pub const TEXTURE_FILTER_AUTO: u8 = 0;
pub const TEXTURE_FILTER_SMOOTH: u8 = 1;
pub const TEXTURE_FILTER_SHARP: u8 = 2;

// The three constants above name the VALUES of `PIXEL_FILTER`, declared just
// under it. There was briefly a second atomic here holding the same setting, and
// it was inert: every writer in the tree stores into `PIXEL_FILTER` while the
// resolver read the other one, so the mode never left AUTO and both overrides
// silently did nothing. One atomic, one setter, one getter.

/// Ask for a power mode and report back the one actually in force.
///
/// Always read the answer rather than assuming the request took. clkrst can be
/// unavailable, and a raise is refused outright when psm says the battery has
/// left its Normal voltage state, which is the one case where the OS is telling
/// us in so many words that a boost must not be entered.
/// Set when a raise was ASKED FOR and did not take. Drives the footer line, so
/// the row can say why instead of looking like a dead button: this happened for
/// real on 2026-08-26, eight presses at 7% charge with psm refusing every one
/// and nothing on screen to explain it.
static POWER_REFUSED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn apply_power_mode(mode: u8) -> u8 {
    let want = if mode == 1 { 1 } else { 0 };
    let got = unsafe { flashnx_set_clock_mode(want) };
    let got = if got == 1 { 1 } else { 0 };
    let refused = want == 1 && got == 0;
    POWER_REFUSED.store(refused, std::sync::atomic::Ordering::Relaxed);
    if refused {
        POWER_REFUSED_EVER.store(true, std::sync::atomic::Ordering::Relaxed);
    }
    got
}

/// True when the last raise request was turned down.
pub fn overclock_refused() -> bool {
    POWER_REFUSED.load(std::sync::atomic::Ordering::Relaxed)
}

/// True when ANY raise was turned down since the current game was launched.
///
/// Separate from `overclock_refused`, which describes the LAST request and is
/// what the pause-menu footer wants. A bug report needs the other question:
/// psm refuses a raise while the battery is out of its Normal voltage state
/// (observed for real at 7% charge, eight presses in a row), so a player can
/// have OVERCLOCK set to ON, play the whole session at 1020 MHz, and file a
/// report about how slow it is. Publishing the SETTING alone would put "boost:
/// ON" at the top of a report about stock-clock performance, and that is the
/// first fact any frame rate is read against.
pub fn overclock_ever_refused() -> bool {
    POWER_REFUSED_EVER.load(std::sync::atomic::Ordering::Relaxed)
}

/// Cleared per launch from `ruffle_init`: the question is about this game.
pub fn clear_overclock_refused_ever() {
    POWER_REFUSED_EVER.store(false, std::sync::atomic::Ordering::Relaxed);
}

static POWER_REFUSED_EVER: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// The power mode in force right now, straight from the hardware side.
pub fn current_power_mode() -> u8 {
    let m = unsafe { flashnx_clock_mode() };
    if m == 1 { 1 } else { 0 }
}

pub fn screen_filter() -> u8 {
    SCREEN_FILTER.load(std::sync::atomic::Ordering::Relaxed)
}

// ── FPS counter (issue #111) ───────────────────────────────────────────────

/// Whether the counter is drawn for the game being played, mirrored out of that
/// game's `.prefs` at launch. An atomic for exactly the reason the screen filter
/// above is one: the stored value is a file on the SD card, and the paths that
/// need this one run every frame.
static FPS_COUNTER: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// The open measuring window: the system tick it started on (0 = none open) and
/// the SWF frames counted into it so far.
///
/// A TIME window, which is the whole difference from the `fps=` figure in the
/// heartbeat. That one divides by a fixed 60 FRAMES, so on a game at 9 fps it
/// refreshes once every 6.7 seconds -- right for a log line meant to be
/// compared between sessions, useless as something a player watches. The
/// heartbeat is deliberately untouched; this is a second, independent
/// measurement that happens to answer the same question at a usable cadence.
static FPS_WINDOW_START: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);
static FPS_WINDOW_FRAMES: std::sync::atomic::AtomicU32 =
    std::sync::atomic::AtomicU32::new(0);
/// Ruffle's frame accumulator (us) when the window opened, see `fps_sample`.
static FPS_WINDOW_ACC_US: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

/// Last closed window's measured rate and the rate the movie declares, both
/// times ten so one decimal survives without float formatting (the same trick
/// as the heartbeat's `fps_x10`). Real 0 = no window has closed yet; nominal
/// 0 = no movie has said what it wants.
static FPS_REAL_X10: std::sync::atomic::AtomicU32 =
    std::sync::atomic::AtomicU32::new(0);
static FPS_NOMINAL_X10: std::sync::atomic::AtomicU32 =
    std::sync::atomic::AtomicU32::new(0);

/// Window length as a divisor of the tick frequency. Half a second: short
/// enough that the number follows a scene change, long enough that a game down
/// at 3 fps still has frames in it to divide by.
const FPS_WINDOW_DIV: u64 = 2;

pub fn fps_counter_on() -> bool {
    FPS_COUNTER.load(std::sync::atomic::Ordering::Relaxed)
}

/// Turn the counter on or off, throwing away the MEASURED rate but keeping the
/// movie's nominal one.
///
/// Called on EVERY game launch (like the screen filter and the power mode) and
/// from the ECRAN row. Clearing the measured rate is the point in both cases:
/// without it the first half second of a new game shows the PREVIOUS game's
/// speed, printed under the new game's nominal rate.
///
/// The nominal rate is deliberately NOT cleared here, and clearing it was a real
/// bug: only `fps_sample` writes it, `fps_sample` runs only from the live tick,
/// and the pause loop does not tick. So turning the row on from ECRAN zeroed the
/// nominal and `fps_readout` then refused to draw anything, which made the row
/// look dead exactly where it is supposed to preview itself on the frozen frame.
/// A new game invalidates the nominal, so `ruffle_init` is where it is cleared.
pub fn set_fps_counter(on: bool) {
    FPS_COUNTER.store(on, std::sync::atomic::Ordering::Relaxed);
    FPS_REAL_X10.store(0, std::sync::atomic::Ordering::Relaxed);
    FPS_HAVE_READING.store(false, std::sync::atomic::Ordering::Relaxed);
    fps_window_reset();
}

/// Forget the movie's nominal rate. Called from `ruffle_init`, the one moment
/// the previous value stops describing anything.
pub fn clear_fps_nominal() {
    FPS_NOMINAL_X10.store(0, std::sync::atomic::Ordering::Relaxed);
}

/// Drop the open TELEMETRY window too, so the next heartbeat prints `fps=—`
/// rather than 60 frames divided by a stretch of wall clock the console spent
/// asleep.
///
/// The heartbeat has always mismeasured that window; it did not matter while the
/// line was one sample among a scrolling log. It matters now: a bug report keeps
/// the SLOWEST heartbeat of the session, so a single HOME-menu press was enough
/// to latch a fabricated "1.4 fps" as the headline number of every report filed
/// afterwards. `fps=—` is not a candidate (`hb_fps` in ruffle_bridge.cpp refuses
/// a non-numeric rate), so one window is skipped and nothing is invented.
pub fn heartbeat_window_reset() {
    HEARTBEAT_RESET.store(true, std::sync::atomic::Ordering::Relaxed);
}

/// Set by `heartbeat_window_reset`, consumed by the next heartbeat.
static HEARTBEAT_RESET: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Fold one `Player::tick` into the open window.
///
/// `frames_run` is how many SWF frames that tick ACTUALLY advanced, not how
/// many frames we drew. The two agree only once a frame costs more than its
/// nominal duration; below that the host loop sits at the display's 60 Hz while
/// a 30 fps movie advances 30 times, so a counter built on our own frames would
/// print "60 / 30" for a game running at exactly the right speed.
///
/// `now` is the tick already read for the tick/render profile, so this costs no
/// second clock read. `acc_us` is Ruffle's frame accumulator after that tick
/// (`flashnx_frame_accumulator_us`).
pub fn fps_sample(frames_run: u32, nominal_x10: u32, now: u64, acc_us: u64) {
    FPS_NOMINAL_X10.store(nominal_x10, std::sync::atomic::Ordering::Relaxed);
    let start = FPS_WINDOW_START.load(std::sync::atomic::Ordering::Relaxed);
    // No window open: the counter was just switched on, or a pause was left
    // (`fps_window_reset`). Open one here instead of measuring across the gap,
    // on a tick that advanced the movie (see the close below).
    if start == 0 {
        if frames_run == 0 {
            return;
        }
        FPS_WINDOW_START.store(now.max(1), std::sync::atomic::Ordering::Relaxed);
        // ZERO, not `frames_run`: those frames were advanced by the tick that
        // has just finished, so they happened BEFORE `now`. Seeding them into a
        // window whose clock starts here divides them by an interval they are
        // not inside, which reads as a spike on the first half second after
        // every resume -- exactly where Ruffle catches up several frames at once.
        FPS_WINDOW_FRAMES.store(0, std::sync::atomic::Ordering::Relaxed);
        FPS_WINDOW_ACC_US.store(acc_us, std::sync::atomic::Ordering::Relaxed);
        return;
    }
    let frames = FPS_WINDOW_FRAMES
        .fetch_add(frames_run, std::sync::atomic::Ordering::Relaxed)
        .saturating_add(frames_run);
    let freq = unsafe { ruffle_tick_freq() };
    let dt = now.saturating_sub(start);
    if freq == 0 || dt < freq / FPS_WINDOW_DIV {
        return;
    }
    // The window closes on wall clock but counts WHOLE movie frames, so its
    // resolution is one frame per window: at half a second that is +/- 2 fps,
    // which is nothing at 30 and a third of the reading at 3. The counter exists
    // for the games that run at 3, and a number that swings by a third with
    // nothing changing on screen is worse than a slower one. So also wait for a
    // few frames, with a ceiling so a game that has genuinely stopped still
    // reports its stop instead of freezing on the last good reading.
    const MIN_FRAMES: u32 = 4;
    const MAX_WINDOW_SECS: u64 = 3;
    if frames < MIN_FRAMES && dt < freq * MAX_WINDOW_SECS {
        return;
    }
    // Close on a tick that advanced the movie, as the window opened on one: it
    // then spans exactly `frames` frame intervals. Closing on any tick past the
    // half second counted whole frames over a span that could hold one more
    // frame than intervals: a game at exactly 30 read "31 / 30" every few
    // windows (16 frames in 31 vsyncs, Super Smash Flash 2, 2026-10-05). The
    // ceiling still closes the window of a game that has stopped.
    if frames_run == 0 && dt < freq * MAX_WINDOW_SECS {
        return;
    }
    // Whole frames still round: the wall time of the window is the frames run
    // plus what Ruffle's accumulator gained over it (time received, not yet a
    // frame), minus any time it threw away. A game keeping up exactly read
    // x1.03 when a window happened to close on a tick that spent time banked
    // before it opened (2026-10-06). Adding the accumulator's change makes the
    // reading the frames' worth of time over the wall time: x1.00 when nothing
    // is thrown away, below it by exactly what is.
    let acc_start = FPS_WINDOW_ACC_US.load(std::sync::atomic::Ordering::Relaxed);
    let frames_x10 = (frames as i64 * 10
        + (acc_us as i64 - acc_start as i64) * nominal_x10 as i64 / 1_000_000)
        .max(0) as u64;
    FPS_REAL_X10.store(
        ((frames_x10 * freq) / dt).min(u32::MAX as u64) as u32,
        std::sync::atomic::Ordering::Relaxed,
    );
    FPS_HAVE_READING.store(true, std::sync::atomic::Ordering::Relaxed);
    FPS_WINDOW_START.store(now.max(1), std::sync::atomic::Ordering::Relaxed);
    FPS_WINDOW_FRAMES.store(0, std::sync::atomic::Ordering::Relaxed);
    FPS_WINDOW_ACC_US.store(acc_us, std::sync::atomic::Ordering::Relaxed);
}

/// Drop the open window, keeping the last reading on screen.
///
/// Hung off `ruffle_skip_paused_time`, which every resume path already goes
/// through -- pause menu, ECRAN, TOUCHES, the in-game keyboard, and the
/// three-second gap that means the HOME menu or sleep. Without it the first
/// window after a pause spans the pause, so thirty seconds in a menu comes back
/// as a game running at one frame per second.
pub fn fps_window_reset() {
    FPS_WINDOW_START.store(0, std::sync::atomic::Ordering::Relaxed);
    FPS_WINDOW_FRAMES.store(0, std::sync::atomic::Ordering::Relaxed);
}

/// Last completed reading as `(measured x10, nominal x10)`.
///
/// `measured` is `None` until the first window closes, which is a different
/// thing from a measured zero: a game that has genuinely stopped reads 0, and
/// showing that as the "not measured yet" placeholder would hide the one state
/// the counter is most useful in. A shared sentinel conflated the two.
///
/// `nominal` is `None` when the movie declares no usable rate. Ruffle takes the
/// SWF header value verbatim, so a malformed file can leave it at 0; that used
/// to make the whole counter draw nothing, which reads as a setting that did not
/// take. Without a denominator there is no ratio, but the measured rate on its
/// own is still worth showing.
pub fn fps_readout() -> (Option<u32>, Option<u32>) {
    let nominal = FPS_NOMINAL_X10.load(std::sync::atomic::Ordering::Relaxed);
    let measured = if FPS_HAVE_READING.load(std::sync::atomic::Ordering::Relaxed) {
        Some(FPS_REAL_X10.load(std::sync::atomic::Ordering::Relaxed))
    } else {
        None
    };
    (measured, if nominal == 0 { None } else { Some(nominal) })
}

/// Set when a window has closed, so a measured 0 is not read as "no reading".
static FPS_HAVE_READING: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// One rate as text: whole where the movie declared a whole number (nearly all
/// of them do), one decimal where it did not -- a SWF header can hold 12.5.
fn fps_rate_str(x10: u32) -> std::string::String {
    if x10 % 10 == 0 {
        std::format!("{}", x10 / 10)
    } else {
        std::format!("{}.{}", x10 / 10, x10 % 10)
    }
}

/// Vertical resolution of the game's stage, set at launch. The scanline pitch is
/// derived from it so the pattern sits on the GAME's rows rather than on screen
/// pixels: one dark line per screen pixel is finer than the eye resolves and just
/// reads as a dimmer picture, which is exactly what the first version did.
static STAGE_HEIGHT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

pub fn set_stage_height(h: u32) {
    STAGE_HEIGHT.store(h, std::sync::atomic::Ordering::Relaxed);
}

/// Scanlines to draw over the screen height. Clamped so the pitch stays between
/// 2 and 4 screen pixels on a 720p panel: below 2 the lines disappear again, and
/// above 4 they stop looking like scanlines and start looking like blinds.
fn scanline_count() -> f32 {
    let h = STAGE_HEIGHT.load(std::sync::atomic::Ordering::Relaxed);
    (h.max(1) as f32).clamp(180.0, 360.0)
}

// Screen filter (issue #65), applied to the whole frame after the game is drawn.
// COLOUR ONLY, no geometry, on purpose: the mouse cursor and the touchscreen map
// straight onto the viewport, so bending the picture (the barrel distortion that
// makes a "CRT" look convincing) would leave what you see and what you click in
// different places. Every effect below only reweights the pixel it is given.
//   mode 1 SCANLINES — darken every other physical scanline.
//   mode 2 CRT       — scanlines, plus an RGB stripe mask and a soft vignette.
// `u_res` is the destination size in pixels, so lines and stripes land on real
// screen pixels rather than on stage coordinates.
const SCREEN_FILTER_FRAG: &[u8] = b"#version 330 core\n\
in vec2 v_uv;\n\
out vec4 frag_color;\n\
uniform sampler2D u_tex;\n\
uniform vec2 u_res;\n\
uniform float u_scan;\n\
uniform int u_mode;\n\
void main() {\n\
    vec3 c = texture(u_tex, v_uv).rgb;\n\
    // One smooth period per scanline. A hard every-other-row test aliases into\n\
    // moire as soon as the pitch is not an exact pixel multiple; a cosine keeps\n\
    // its shape at any pitch, which matters because the pitch follows the game.\n\
    float s = 0.5 + 0.5 * cos(6.2831853 * v_uv.y * u_scan);\n\
    if (u_mode == 1) {\n\
        c *= mix(0.55, 1.0, s);\n\
    } else {\n\
        c *= mix(0.42, 1.0, s);\n\
        // Aperture grille on 2px stripes (6px per RGB triplet). At 1px per\n\
        // channel the mask was below what the panel resolves and only\n\
        // desaturated the picture.\n\
        int col = int(mod(floor(v_uv.x * u_res.x * 0.5), 3.0));\n\
        vec3 mask = vec3(0.88);\n\
        if (col == 0) { mask.r = 1.16; }\n\
        else if (col == 1) { mask.g = 1.16; }\n\
        else { mask.b = 1.16; }\n\
        c *= mask;\n\
        vec2 d = v_uv - vec2(0.5);\n\
        c *= 1.0 - 0.5 * dot(d, d);\n\
    }\n\
    frag_color = vec4(c, 1.0);\n\
}\n\0";

// Faithful port of `color_matrix.wgsl`. 20-float ColorMatrix as a 4×4 mat plus
// a vec4 of "+" terms; un-premultiply rgb before the multiply, re-premultiply
// after, to match the Flash convention.
const COLOR_MATRIX_FRAG: &[u8] = b"#version 330 core\n\
in vec2 v_uv;\n\
out vec4 frag_color;\n\
uniform sampler2D u_tex;\n\
uniform mat4 u_color_mat;\n\
uniform vec4 u_color_extra;\n\
void main() {\n\
    vec4 src = texture(u_tex, v_uv);\n\
    vec3 rgb_un = src.a > 0.0 ? src.rgb / src.a : vec3(0.0);\n\
    vec4 in_vec = vec4(rgb_un, src.a);\n\
    vec4 out_vec = u_color_mat * in_vec + u_color_extra;\n\
    vec4 c = clamp(out_vec, 0.0, 1.0);\n\
    frag_color = vec4(c.rgb * c.a, c.a);\n\
}\n\0";

// Premultiplied -> straight-alpha copy. `render_offscreen` renders draw()
// commands into a PREMULTIPLIED temp texture; atlas slots store STRAIGHT
// alpha, so repatriating the result into an atlas slot needs an
// un-premultiply. Doing it on the GPU (this shader, into the atlas FBO)
// replaces a per-call `glReadPixels` + CPU divide + re-upload — that readback
// was ~78% of frame time on cacheAsBitmap-heavy AS3 games (catmario:
// ~260ms/frame across 48 draw() repatriations). Reuses FILTER_VERT.
const UNPREMULT_FRAG: &[u8] = b"#version 330 core\n\
in vec2 v_uv;\n\
out vec4 frag_color;\n\
uniform sampler2D u_tex;\n\
void main() {\n\
    vec4 src = texture(u_tex, v_uv);\n\
    frag_color = src.a > 0.0 ? vec4(src.rgb / src.a, src.a) : vec4(0.0);\n\
}\n\0";

// Straight-alpha -> premultiplied copy (inverse of UNPREMULT_FRAG). Used to
// SEED a render_offscreen temp with the BitmapData's existing (straight, atlas)
// content before compositing new draw() commands on top — Ruffle's
// `render_offscreen` must blend onto the previous contents (its wgpu backend
// uses `RenderTargetMode::FreshWithTexture`). Without this seed, a game that
// builds its frame by accumulating many draw()s into one BitmapData (a software
// blitter, e.g. catmario's `stageBitmapdata`) loses all but the last draw.
const PREMULT_FRAG: &[u8] = b"#version 330 core\n\
in vec2 v_uv;\n\
out vec4 frag_color;\n\
uniform sampler2D u_tex;\n\
void main() {\n\
    vec4 src = texture(u_tex, v_uv);\n\
    frag_color = vec4(src.rgb * src.a, src.a);\n\
}\n\0";

// Separable Gaussian-approximating blur, faithful port of `blur.wgsl`. The
// vertex stage pre-shifts UV so the fragment loop starts at the right offset
// (`u_blur_m` half-distance, `u_blur_m2 = m*2` outer bound). The last sample
// is fused with a fractional weight to handle non-integer kernel radii.
// See <https://fgiesen.wordpress.com/2012/08/01/fast-blurs-2/>.
const BLUR_VERT: &[u8] = b"#version 330 core\n\
layout(location = 0) in vec2 a_pos;\n\
layout(location = 1) in vec2 a_uv;\n\
uniform vec4 u_src_uv;\n\
uniform vec2 u_blur_dir;\n\
uniform float u_blur_m;\n\
out vec2 v_uv;\n\
void main() {\n\
    gl_Position = vec4(a_pos.x * 2.0 - 1.0, a_pos.y * 2.0 - 1.0, 0.0, 1.0);\n\
    vec2 raw = u_src_uv.xy + a_uv * u_src_uv.zw;\n\
    v_uv = raw - u_blur_dir * u_blur_m;\n\
}\n\0";

const BLUR_FRAG: &[u8] = b"#version 330 core\n\
in vec2 v_uv;\n\
out vec4 frag_color;\n\
uniform sampler2D u_tex;\n\
uniform vec2 u_blur_dir;\n\
uniform float u_blur_m2;\n\
uniform float u_blur_full_size;\n\
uniform float u_blur_first_weight;\n\
uniform float u_blur_last_offset;\n\
uniform float u_blur_last_weight;\n\
void main() {\n\
    vec2 direction = u_blur_dir;\n\
    vec4 total = vec4(0.0);\n\
    total += texture(u_tex, v_uv - direction) * u_blur_first_weight;\n\
    vec4 center = vec4(0.0);\n\
    for (float i = 0.5; i < u_blur_m2; i += 2.0) {\n\
        center += texture(u_tex, v_uv + direction * i);\n\
    }\n\
    total += center * 2.0;\n\
    vec2 last_loc = v_uv + direction * (u_blur_m2 + u_blur_last_offset);\n\
    total += texture(u_tex, last_loc) * u_blur_last_weight;\n\
    vec4 result = total / u_blur_full_size;\n\
    frag_color = floor(result * 255.0) / 255.0;\n\
}\n\0";

// Glow composite + DropShadow: faithful port of `glow.wgsl`. Reads the source
// texture (unit 0) and a pre-blurred version of it (unit 1), composites with
// a uniform colour + strength + inner/knockout/composite_source flags. The
// blur_uv is offset per-vertex by `u_blur_uv.xy` (DropShadow distance), so
// the blur effectively shifts on the destination.
const GLOW_VERT: &[u8] = b"#version 330 core\n\
layout(location = 0) in vec2 a_pos;\n\
layout(location = 1) in vec2 a_uv;\n\
uniform vec4 u_src_uv;\n\
uniform vec4 u_blur_uv;\n\
out vec2 v_src_uv;\n\
out vec2 v_blur_uv;\n\
void main() {\n\
    gl_Position = vec4(a_pos.x * 2.0 - 1.0, a_pos.y * 2.0 - 1.0, 0.0, 1.0);\n\
    v_src_uv = u_src_uv.xy + a_uv * u_src_uv.zw;\n\
    v_blur_uv = u_blur_uv.xy + a_uv * u_blur_uv.zw;\n\
}\n\0";

const GLOW_FRAG: &[u8] = b"#version 330 core\n\
in vec2 v_src_uv;\n\
in vec2 v_blur_uv;\n\
out vec4 frag_color;\n\
uniform sampler2D u_tex;\n\
uniform sampler2D u_blur_tex;\n\
uniform vec4 u_color;\n\
uniform float u_strength;\n\
uniform int u_inner;\n\
uniform int u_knockout;\n\
uniform int u_composite_source;\n\
void main() {\n\
    bool inner = u_inner != 0;\n\
    bool knockout = u_knockout != 0;\n\
    bool composite_source = u_composite_source != 0;\n\
    float blur_a = texture(u_blur_tex, v_blur_uv).a;\n\
    vec4 dst = texture(u_tex, v_src_uv);\n\
    if (v_blur_uv.x < 0.0 || v_blur_uv.x > 1.0 || v_blur_uv.y < 0.0 || v_blur_uv.y > 1.0) {\n\
        blur_a = 0.0;\n\
    }\n\
    vec4 color = vec4(u_color.r, u_color.g, u_color.b, 1.0);\n\
    if (inner) {\n\
        float alpha = u_color.a * clamp((1.0 - blur_a) * u_strength, 0.0, 1.0);\n\
        if (knockout) {\n\
            color = color * alpha * dst.a;\n\
        } else if (composite_source) {\n\
            color = color * alpha * dst.a + dst * (1.0 - alpha);\n\
        } else {\n\
            color = color * alpha * dst.a;\n\
        }\n\
    } else {\n\
        float alpha = u_color.a * clamp(blur_a * u_strength, 0.0, 1.0);\n\
        if (knockout) {\n\
            color = color * alpha * (1.0 - dst.a);\n\
        } else if (composite_source) {\n\
            color = color * alpha * (1.0 - dst.a) + dst;\n\
        } else {\n\
            color = color * alpha;\n\
        }\n\
    }\n\
    frag_color = color;\n\
}\n\0";

// Bevel: like Glow, but samples the blurred alpha at TWO opposite offsets
// (±blur_offset along the filter angle) to derive a highlight side and a
// shadow side. Faithful port of wgpu's `bevel.wgsl`.
const BEVEL_VERT: &[u8] = b"#version 330 core\n\
layout(location = 0) in vec2 a_pos;\n\
layout(location = 1) in vec2 a_uv;\n\
uniform vec4 u_src_uv;\n\
uniform vec4 u_blur_uv_l;\n\
uniform vec4 u_blur_uv_r;\n\
out vec2 v_src_uv;\n\
out vec2 v_blur_l;\n\
out vec2 v_blur_r;\n\
void main() {\n\
    gl_Position = vec4(a_pos.x * 2.0 - 1.0, a_pos.y * 2.0 - 1.0, 0.0, 1.0);\n\
    v_src_uv = u_src_uv.xy + a_uv * u_src_uv.zw;\n\
    v_blur_l = u_blur_uv_l.xy + a_uv * u_blur_uv_l.zw;\n\
    v_blur_r = u_blur_uv_r.xy + a_uv * u_blur_uv_r.zw;\n\
}\n\0";

const BEVEL_FRAG: &[u8] = b"#version 330 core\n\
in vec2 v_src_uv;\n\
in vec2 v_blur_l;\n\
in vec2 v_blur_r;\n\
out vec4 frag_color;\n\
uniform sampler2D u_tex;\n\
uniform sampler2D u_blur_tex;\n\
uniform vec4 u_highlight;\n\
uniform vec4 u_shadow;\n\
uniform float u_strength;\n\
uniform int u_bevel_type;\n\
uniform int u_knockout;\n\
void main() {\n\
    bool knockout = u_knockout != 0;\n\
    bool outer = (u_bevel_type == 0 || u_bevel_type == 2);\n\
    bool inner = (u_bevel_type == 1 || u_bevel_type == 2);\n\
    float bl = texture(u_blur_tex, v_blur_l).a;\n\
    float br = texture(u_blur_tex, v_blur_r).a;\n\
    vec4 dst = texture(u_tex, v_src_uv);\n\
    if (v_blur_l.x < 0.0 || v_blur_l.x > 1.0 || v_blur_l.y < 0.0 || v_blur_l.y > 1.0) bl = 0.0;\n\
    if (v_blur_r.x < 0.0 || v_blur_r.x > 1.0 || v_blur_r.y < 0.0 || v_blur_r.y > 1.0) br = 0.0;\n\
    float ha = clamp((bl - br) * u_strength, 0.0, 1.0);\n\
    float sa = clamp((br - bl) * u_strength, 0.0, 1.0);\n\
    vec4 glow = u_highlight * ha + u_shadow * sa;\n\
    vec4 outc;\n\
    if (inner && outer) {\n\
        outc = knockout ? glow : (dst - dst * glow.a + glow);\n\
    } else if (inner) {\n\
        outc = knockout ? (glow * dst.a) : (glow * dst.a + dst * (1.0 - glow.a));\n\
    } else {\n\
        outc = knockout ? (glow - glow * dst.a) : (dst + glow - glow * dst.a);\n\
    }\n\
    frag_color = outc;\n\
}\n\0";

// Alpha-mask composite, faithful port of `alpha_mask.wgsl`. Samples the
// pre-rendered maskee (unit 0) and mask (unit 1) textures at the same UV and
// outputs the maskee scaled by the mask's alpha — a soft/luminance mask that
// the stencil masking path can't express. Reuses FILTER_VERT (u_src_uv set to
// the full [0,1]² so v_uv == a_uv). Output is premultiplied; the caller draws
// the result back over the stage with premultiplied "over" blending.
const ALPHA_MASK_FRAG: &[u8] = b"#version 330 core\n\
in vec2 v_uv;\n\
out vec4 frag_color;\n\
uniform sampler2D u_tex;\n\
uniform sampler2D u_mask_tex;\n\
void main() {\n\
    vec4 dst = texture(u_tex, v_uv);\n\
    vec4 src = texture(u_mask_tex, v_uv);\n\
    frag_color = vec4(dst.rgb * src.a, dst.a * src.a);\n\
}\n\0";

// Complex (non-trivial) blend modes, faithful port of the wgpu `blend/*.wgsl`
// family (multiply/lighten/darken/difference/invert/overlay/hardlight). Samples
// the backdrop "parent" (unit 0, a glCopyTexSubImage2D snapshot of the current
// render target) and the freshly-rendered blend group "current" (unit 1), and
// writes the full composited pixel (premultiplied) so the caller can overwrite
// the target region with blending DISABLED. `u_current_flip` flips the current
// sampler's V when the target is the main framebuffer (which renders Y-flipped,
// unlike offscreen textures whose row 0 is the Flash top); the parent snapshot
// is always sampled straight since it's a 1:1 copy of the target. All the inner
// blend funcs operate on un-premultiplied colour, guarding the divide so a
// transparent backdrop (dst.a == 0) collapses the formula back to `src`.
const COMPLEX_BLEND_FRAG: &[u8] = b"#version 330 core\n\
in vec2 v_uv;\n\
out vec4 frag_color;\n\
uniform sampler2D u_tex;\n\
uniform sampler2D u_current_tex;\n\
uniform int u_blend_mode;\n\
uniform float u_current_flip;\n\
uniform vec3 u_cur_zoom;\n\
vec3 blend_func(vec3 s, vec3 d) {\n\
    if (u_blend_mode == 0) { return s * d; }\n\
    if (u_blend_mode == 1) { return max(s, d); }\n\
    if (u_blend_mode == 2) { return min(s, d); }\n\
    if (u_blend_mode == 3) { return abs(d - s); }\n\
    if (u_blend_mode == 4) { return 1.0 - d; }\n\
    if (u_blend_mode == 5) {\n\
        vec3 o = s;\n\
        o.r = (d.r <= 0.5) ? (2.0 * s.r * d.r) : (1.0 - 2.0 * (1.0 - d.r) * (1.0 - s.r));\n\
        o.g = (d.g <= 0.5) ? (2.0 * s.g * d.g) : (1.0 - 2.0 * (1.0 - d.g) * (1.0 - s.g));\n\
        o.b = (d.b <= 0.5) ? (2.0 * s.b * d.b) : (1.0 - 2.0 * (1.0 - d.b) * (1.0 - s.b));\n\
        return o;\n\
    }\n\
    if (u_blend_mode == 6) {\n\
        vec3 o = s;\n\
        o.r = (s.r <= 0.5) ? (2.0 * s.r * d.r) : (1.0 - 2.0 * (1.0 - d.r) * (1.0 - s.r));\n\
        o.g = (s.g <= 0.5) ? (2.0 * s.g * d.g) : (1.0 - 2.0 * (1.0 - d.g) * (1.0 - s.g));\n\
        o.b = (s.b <= 0.5) ? (2.0 * s.b * d.b) : (1.0 - 2.0 * (1.0 - d.b) * (1.0 - s.b));\n\
        return o;\n\
    }\n\
    return s;\n\
}\n\
void main() {\n\
    vec2 cuv = vec2(v_uv.x, mix(v_uv.y, 1.0 - v_uv.y, u_current_flip));\n\
    vec4 dst = texture(u_tex, v_uv);\n\
    cuv = (cuv - 0.5) / u_cur_zoom.x + 0.5 - u_cur_zoom.yz / u_cur_zoom.x;\n\
    if (any(lessThan(cuv, vec2(0.0))) || any(greaterThan(cuv, vec2(1.0)))) { frag_color = dst; return; }\n\
    vec4 src = texture(u_current_tex, cuv);\n\
    if (src.a <= 0.0) { frag_color = dst; return; }\n\
    vec3 s_un = src.rgb / src.a;\n\
    vec3 d_un = (dst.a > 0.0) ? (dst.rgb / dst.a) : vec3(0.0);\n\
    vec3 bf = blend_func(s_un, d_un);\n\
    vec3 rgb = src.rgb * (1.0 - dst.a) + dst.rgb * (1.0 - src.a) + src.a * dst.a * bf;\n\
    float a = src.a + dst.a * (1.0 - src.a);\n\
    frag_color = vec4(rgb, a);\n\
}\n\0";

// DisplacementMapFilter (#42, e.g. Papa Louie 3's rippling water). Faithful port
// of `displacement_map.wgsl`: for each dest pixel, sample the displacement map,
// pull the two selected channels, and offset the source lookup by
// `(component - 128) * viewscale * scale / 256`. Reuses FILTER_VERT, so `v_uv`
// is the source content coordinate in [0,1] (the source region fills its
// texture: source_point 0,0 + full size, which is how cacheAsBitmap sources come
// in). Source at unit 0, map at unit 1. Wrap mode is emulated with `fract` since
// our standalone textures are CLAMP_TO_EDGE.
const DISPLACEMENT_MAP_FRAG: &[u8] = b"#version 330 core\n\
in vec2 v_uv;\n\
out vec4 frag_color;\n\
uniform sampler2D u_tex;\n\
uniform sampler2D u_map_tex;\n\
uniform vec4 u_color;\n\
uniform vec4 u_map_remap;\n\
uniform int u_comp_x;\n\
uniform int u_comp_y;\n\
uniform int u_mode;\n\
uniform vec2 u_scale;\n\
uniform vec2 u_source_size;\n\
uniform vec2 u_map_size;\n\
uniform vec2 u_offset;\n\
uniform vec2 u_viewscale;\n\
float dm_comp(vec4 m, int c) {\n\
    if (c == 1) return m.r * 255.0;\n\
    if (c == 2) return m.g * 255.0;\n\
    if (c == 4) return m.b * 255.0;\n\
    if (c == 8) return m.a * 255.0;\n\
    return 128.0;\n\
}\n\
void main() {\n\
    vec2 source_pos = v_uv * u_source_size;\n\
    vec2 map_uv = (source_pos - u_offset) / u_viewscale / u_map_size;\n\
    // Map may live in a shared atlas: sample its sub-rect via u_map_remap.\n\
    vec2 map_uv_c = clamp(map_uv, 0.0, 1.0);\n\
    vec4 m = texture(u_map_tex, u_map_remap.xy + map_uv_c * u_map_remap.zw);\n\
    if (map_uv.x < 0.0 || map_uv.x > 1.0 || map_uv.y < 0.0 || map_uv.y > 1.0) {\n\
        m = vec4(0.5);\n\
    }\n\
    vec2 sc = u_viewscale * u_scale;\n\
    vec2 disp = source_pos + vec2(\n\
        (dm_comp(m, u_comp_x) - 128.0) * sc.x / 256.0,\n\
        (dm_comp(m, u_comp_y) - 128.0) * sc.y / 256.0);\n\
    vec2 duv = disp / u_source_size;\n\
    bool oob = duv.x < 0.0 || duv.x > 1.0 || duv.y < 0.0 || duv.y > 1.0;\n\
    if (u_mode == 0) { duv = fract(duv); }\n\
    else if (u_mode == 1) { duv = clamp(duv, 0.0, 1.0); }\n\
    else if (u_mode == 2 && oob) { duv = v_uv; }\n\
    vec4 result = texture(u_tex, duv);\n\
    if (u_mode == 3 && oob) { result = vec4(u_color.rgb, 1.0) * u_color.a; }\n\
    frag_color = result;\n\
}\n\0";

// ─── Shader build helpers ─────────────────────────────────────────────────────

fn compile_shader(kind: GLenum, src_nul: &[u8]) -> Option<GLuint> {
    unsafe {
        let shader = glCreateShader(kind);
        let src_ptr = src_nul.as_ptr() as *const GLchar;
        glShaderSource(shader, 1, &src_ptr, core::ptr::null());
        glCompileShader(shader);
        let mut status: GLint = 0;
        glGetShaderiv(shader, GL_COMPILE_STATUS, &mut status);
        if status == 0 {
            log(b"backend shader compile failed:\n\0");
            log_info_log(shader, false);
            glDeleteShader(shader);
            return None;
        }
        Some(shader)
    }
}

fn link_program(vert_src: &[u8], frag_src: &[u8]) -> Option<GLuint> {
    let vs = compile_shader(GL_VERTEX_SHADER, vert_src)?;
    let fs = compile_shader(GL_FRAGMENT_SHADER, frag_src)?;
    unsafe {
        let program = glCreateProgram();
        glAttachShader(program, vs);
        glAttachShader(program, fs);
        glLinkProgram(program);
        glDeleteShader(vs);
        glDeleteShader(fs);
        let mut status: GLint = 0;
        glGetProgramiv(program, GL_LINK_STATUS, &mut status);
        if status == 0 {
            log_info_log(program, true);
            glDeleteProgram(program);
            return None;
        }
        Some(program)
    }
}

fn log_info_log(handle: GLuint, is_program: bool) {
    unsafe {
        let mut buf: [u8; 1024] = [0; 1024];
        let mut written: GLsizei = 0;
        if is_program {
            glGetProgramInfoLog(handle, buf.len() as GLsizei, &mut written, buf.as_mut_ptr() as *mut GLchar);
        } else {
            glGetShaderInfoLog(handle, buf.len() as GLsizei, &mut written, buf.as_mut_ptr() as *mut GLchar);
        }
        buf[buf.len() - 1] = 0;
        ruffle_log_cstr(buf.as_ptr() as *const _);
    }
}

fn loc(program: GLuint, name: &[u8]) -> GLint {
    unsafe { glGetUniformLocation(program, name.as_ptr() as *const _) }
}

fn build_solid_program() -> Option<SolidProgram> {
    let program = link_program(SOLID_VERT, SOLID_FRAG)?;
    Some(SolidProgram {
        u_world: loc(program, b"u_world\0"),
        u_mult: loc(program, b"u_mult\0"),
        u_add: loc(program, b"u_add\0"),
        program,
    })
}

fn build_bitmap_program() -> Option<BitmapProgram> {
    let program = link_program(BITMAP_VERT, BITMAP_FRAG)?;
    Some(BitmapProgram {
        u_world: loc(program, b"u_world\0"),
        u_mult: loc(program, b"u_mult\0"),
        u_add: loc(program, b"u_add\0"),
        u_tex: loc(program, b"u_tex\0"),
        u_uv_remap: loc(program, b"u_uv_remap\0"),
        u_premult: loc(program, b"u_premult\0"),
        program,
    })
}

fn build_shape_bitmap_program() -> Option<ShapeBitmapProgram> {
    let program = link_program(SHAPE_BITMAP_VERT, SHAPE_BITMAP_FRAG)?;
    Some(ShapeBitmapProgram {
        u_world: loc(program, b"u_world\0"),
        u_mult: loc(program, b"u_mult\0"),
        u_add: loc(program, b"u_add\0"),
        u_tex: loc(program, b"u_tex\0"),
        u_uv: loc(program, b"u_uv\0"),
        u_uv_remap: loc(program, b"u_uv_remap\0"),
        u_uv_inset: loc(program, b"u_uv_inset\0"),
        u_wrap_mode: loc(program, b"u_wrap_mode\0"),
        program,
    })
}

fn build_gradient_program() -> Option<GradientProgram> {
    let program = link_program(GRADIENT_VERT, GRADIENT_FRAG)?;
    Some(GradientProgram {
        u_world: loc(program, b"u_world\0"),
        u_mult: loc(program, b"u_mult\0"),
        u_add: loc(program, b"u_add\0"),
        u_tex: loc(program, b"u_tex\0"),
        u_grad_local: loc(program, b"u_grad_local\0"),
        u_grad_kind: loc(program, b"u_grad_kind\0"),
        u_grad_spread: loc(program, b"u_grad_spread\0"),
        u_grad_focal: loc(program, b"u_grad_focal\0"),
        program,
    })
}

fn build_color_matrix_filter_program() -> Option<ColorMatrixFilterProgram> {
    let program = link_program(FILTER_VERT, COLOR_MATRIX_FRAG)?;
    Some(ColorMatrixFilterProgram {
        u_src_uv: loc(program, b"u_src_uv\0"),
        u_color_mat: loc(program, b"u_color_mat\0"),
        u_color_extra: loc(program, b"u_color_extra\0"),
        program,
    })
}

fn build_screen_filter_program() -> Option<ScreenFilterProgram> {
    let program = link_program(FILTER_VERT, SCREEN_FILTER_FRAG)?;
    Some(ScreenFilterProgram {
        u_src_uv: loc(program, b"u_src_uv\0"),
        u_res: loc(program, b"u_res\0"),
        u_scan: loc(program, b"u_scan\0"),
        u_mode: loc(program, b"u_mode\0"),
        program,
    })
}

fn build_unpremult_blit_program() -> Option<BlitProgram> {
    let program = link_program(FILTER_VERT, UNPREMULT_FRAG)?;
    Some(BlitProgram {
        u_src_uv: loc(program, b"u_src_uv\0"),
        program,
    })
}

fn build_premult_blit_program() -> Option<BlitProgram> {
    let program = link_program(FILTER_VERT, PREMULT_FRAG)?;
    Some(BlitProgram {
        u_src_uv: loc(program, b"u_src_uv\0"),
        program,
    })
}

fn build_blur_filter_program() -> Option<BlurFilterProgram> {
    let program = link_program(BLUR_VERT, BLUR_FRAG)?;
    Some(BlurFilterProgram {
        u_src_uv: loc(program, b"u_src_uv\0"),
        u_blur_dir: loc(program, b"u_blur_dir\0"),
        u_blur_m: loc(program, b"u_blur_m\0"),
        u_blur_m2: loc(program, b"u_blur_m2\0"),
        u_blur_full_size: loc(program, b"u_blur_full_size\0"),
        u_blur_first_weight: loc(program, b"u_blur_first_weight\0"),
        u_blur_last_offset: loc(program, b"u_blur_last_offset\0"),
        u_blur_last_weight: loc(program, b"u_blur_last_weight\0"),
        program,
    })
}

fn build_glow_filter_program() -> Option<GlowFilterProgram> {
    let program = link_program(GLOW_VERT, GLOW_FRAG)?;
    Some(GlowFilterProgram {
        u_src_uv: loc(program, b"u_src_uv\0"),
        u_blur_uv: loc(program, b"u_blur_uv\0"),
        u_color: loc(program, b"u_color\0"),
        u_strength: loc(program, b"u_strength\0"),
        u_inner: loc(program, b"u_inner\0"),
        u_knockout: loc(program, b"u_knockout\0"),
        u_composite_source: loc(program, b"u_composite_source\0"),
        program,
    })
}

fn build_bevel_filter_program() -> Option<BevelFilterProgram> {
    let program = link_program(BEVEL_VERT, BEVEL_FRAG)?;
    Some(BevelFilterProgram {
        u_src_uv: loc(program, b"u_src_uv\0"),
        u_blur_uv_l: loc(program, b"u_blur_uv_l\0"),
        u_blur_uv_r: loc(program, b"u_blur_uv_r\0"),
        u_highlight: loc(program, b"u_highlight\0"),
        u_shadow: loc(program, b"u_shadow\0"),
        u_strength: loc(program, b"u_strength\0"),
        u_bevel_type: loc(program, b"u_bevel_type\0"),
        u_knockout: loc(program, b"u_knockout\0"),
        program,
    })
}

fn build_displacement_map_filter_program() -> Option<DisplacementMapFilterProgram> {
    let program = link_program(FILTER_VERT, DISPLACEMENT_MAP_FRAG)?;
    Some(DisplacementMapFilterProgram {
        u_src_uv: loc(program, b"u_src_uv\0"),
        u_color: loc(program, b"u_color\0"),
        u_map_remap: loc(program, b"u_map_remap\0"),
        u_comp_x: loc(program, b"u_comp_x\0"),
        u_comp_y: loc(program, b"u_comp_y\0"),
        u_mode: loc(program, b"u_mode\0"),
        u_scale: loc(program, b"u_scale\0"),
        u_source_size: loc(program, b"u_source_size\0"),
        u_map_size: loc(program, b"u_map_size\0"),
        u_offset: loc(program, b"u_offset\0"),
        u_viewscale: loc(program, b"u_viewscale\0"),
        program,
    })
}

fn build_alpha_mask_program() -> Option<AlphaMaskProgram> {
    let program = link_program(FILTER_VERT, ALPHA_MASK_FRAG)?;
    Some(AlphaMaskProgram {
        u_src_uv: loc(program, b"u_src_uv\0"),
        program,
    })
}

fn build_complex_blend_program() -> Option<ComplexBlendProgram> {
    let program = link_program(FILTER_VERT, COMPLEX_BLEND_FRAG)?;
    Some(ComplexBlendProgram {
        u_src_uv: loc(program, b"u_src_uv\0"),
        u_blend_mode: loc(program, b"u_blend_mode\0"),
        u_current_flip: loc(program, b"u_current_flip\0"),
        u_cur_zoom: loc(program, b"u_cur_zoom\0"),
        program,
    })
}

// ─── Geometry helpers ─────────────────────────────────────────────────────────

fn build_solid_quad() -> (GLuint, GLuint) {
    #[rustfmt::skip]
    const QUAD: [f32; 36] = [
        0.0, 0.0, 1.0, 1.0, 1.0, 1.0,
        1.0, 0.0, 1.0, 1.0, 1.0, 1.0,
        1.0, 1.0, 1.0, 1.0, 1.0, 1.0,
        0.0, 0.0, 1.0, 1.0, 1.0, 1.0,
        1.0, 1.0, 1.0, 1.0, 1.0, 1.0,
        0.0, 1.0, 1.0, 1.0, 1.0, 1.0,
    ];
    upload_static_vbo_pos_rgba(&QUAD)
}

fn build_bitmap_quad() -> (GLuint, GLuint) {
    // (pos.xy, uv.xy) — 4 floats per vertex × 6 vertices = 24 floats.
    #[rustfmt::skip]
    const QUAD: [f32; 24] = [
        0.0, 0.0, 0.0, 0.0,
        1.0, 0.0, 1.0, 0.0,
        1.0, 1.0, 1.0, 1.0,
        0.0, 0.0, 0.0, 0.0,
        1.0, 1.0, 1.0, 1.0,
        0.0, 1.0, 0.0, 1.0,
    ];
    let mut vao: GLuint = 0;
    let mut vbo: GLuint = 0;
    unsafe {
        glGenVertexArrays(1, &mut vao);
        glBindVertexArray(vao);
        glGenBuffers(1, &mut vbo);
        glBindBuffer(GL_ARRAY_BUFFER, vbo);
        glBufferData(
            GL_ARRAY_BUFFER,
            core::mem::size_of_val(&QUAD) as GLsizeiptr,
            QUAD.as_ptr() as *const _,
            GL_STATIC_DRAW,
        );
        let stride = (4 * core::mem::size_of::<f32>()) as GLsizei;
        glEnableVertexAttribArray(0);
        glVertexAttribPointer(0, 2, GL_FLOAT, GL_FALSE, stride, core::ptr::null());
        glEnableVertexAttribArray(1);
        glVertexAttribPointer(
            1,
            2,
            GL_FLOAT,
            GL_FALSE,
            stride,
            (2 * core::mem::size_of::<f32>()) as *const _,
        );
        glBindVertexArray(0);
        glBindBuffer(GL_ARRAY_BUFFER, 0);
    }
    (vao, vbo)
}

/// Same attribute layout as `build_bitmap_quad`, but a DYNAMIC buffer that
/// `flush_atlas_quads` refills per text run.
///
/// It needs its own pair, not the bitmap quad's. A VAO records the buffer that
/// was bound when its attribute pointers were set, so a batch cannot borrow
/// `bitmap_vao` and point it elsewhere -- it can only overwrite `bitmap_vbo`,
/// and that buffer holds the STATIC unit quad every other bitmap draw relies
/// on (`render_bitmap`, the covers, the banner: all of them bind the VAO and
/// draw six vertices without uploading any). Overwriting it replaced that quad
/// with a glyph run for the rest of the renderer's life, so every bitmap drawn
/// afterwards got glyph geometry through a matrix built for a unit quad and
/// landed nowhere visible. That is what emptied the gallery the moment any CJK
/// text was drawn -- including the language picker, which lists every language
/// under its own name. Solid draws survive the same treatment only because
/// `draw_rect` re-uploads its vertices on every call.
fn build_atlas_batch() -> (GLuint, GLuint) {
    let mut vao: GLuint = 0;
    let mut vbo: GLuint = 0;
    unsafe {
        glGenVertexArrays(1, &mut vao);
        glBindVertexArray(vao);
        glGenBuffers(1, &mut vbo);
        glBindBuffer(GL_ARRAY_BUFFER, vbo);
        let stride = (4 * core::mem::size_of::<f32>()) as GLsizei;
        glEnableVertexAttribArray(0);
        glVertexAttribPointer(0, 2, GL_FLOAT, GL_FALSE, stride, core::ptr::null());
        glEnableVertexAttribArray(1);
        glVertexAttribPointer(
            1,
            2,
            GL_FLOAT,
            GL_FALSE,
            stride,
            (2 * core::mem::size_of::<f32>()) as *const _,
        );
        glBindVertexArray(0);
        glBindBuffer(GL_ARRAY_BUFFER, 0);
    }
    (vao, vbo)
}

fn build_line_segment() -> (GLuint, GLuint) {
    // Unit horizontal line: (0,0) to (1,0) with per-vertex white. Tinted by
    // a per-call DYNAMIC upload before drawing.
    #[rustfmt::skip]
    const LINE: [f32; 12] = [
        0.0, 0.0, 1.0, 1.0, 1.0, 1.0,
        1.0, 0.0, 1.0, 1.0, 1.0, 1.0,
    ];
    upload_static_vbo_pos_rgba(&LINE)
}

fn build_line_rect() -> (GLuint, GLuint) {
    // Four edges of a unit rect as 4 GL_LINES segments (8 vertices).
    #[rustfmt::skip]
    const LINES: [f32; 48] = [
        0.0, 0.0, 1.0, 1.0, 1.0, 1.0,  1.0, 0.0, 1.0, 1.0, 1.0, 1.0,
        1.0, 0.0, 1.0, 1.0, 1.0, 1.0,  1.0, 1.0, 1.0, 1.0, 1.0, 1.0,
        1.0, 1.0, 1.0, 1.0, 1.0, 1.0,  0.0, 1.0, 1.0, 1.0, 1.0, 1.0,
        0.0, 1.0, 1.0, 1.0, 1.0, 1.0,  0.0, 0.0, 1.0, 1.0, 1.0, 1.0,
    ];
    upload_static_vbo_pos_rgba(&LINES)
}

/// Build the single VAO used by every shape draw. Bound once per frame
/// during `submit_frame`, then each draw call uses
/// `glDrawElementsBaseVertex` to point at its own slice of the arenas.
///
/// The arena VBO is recorded as the source for attribs 0 (pos.xy) and 1
/// (rgba). The arena IBO is recorded as the VAO's element buffer. These
/// bindings persist for the lifetime of the VAO — `glBufferSubData` calls
/// to upload new shape data later don't disturb them.
fn build_shape_arena_vao(arena_vbo: GLuint, arena_ibo: GLuint) -> GLuint {
    let mut vao: GLuint = 0;
    unsafe {
        glGenVertexArrays(1, &mut vao);
        glBindVertexArray(vao);
        glBindBuffer(GL_ARRAY_BUFFER, arena_vbo);
        let stride = (6 * core::mem::size_of::<f32>()) as GLsizei;
        glEnableVertexAttribArray(0);
        glVertexAttribPointer(0, 2, GL_FLOAT, GL_FALSE, stride, core::ptr::null());
        glEnableVertexAttribArray(1);
        glVertexAttribPointer(
            1,
            4,
            GL_FLOAT,
            GL_FALSE,
            stride,
            (2 * core::mem::size_of::<f32>()) as *const _,
        );
        glBindBuffer(GL_ELEMENT_ARRAY_BUFFER, arena_ibo);
        glBindVertexArray(0);
        // Unbind GL_ARRAY_BUFFER without disturbing the VAO's recorded
        // attrib bindings (VAO already captured them above). The IBO bind
        // is part of VAO state in core profile, so we don't unbind that.
        glBindBuffer(GL_ARRAY_BUFFER, 0);
    }
    vao
}

/// Upload a (pos+rgba) interleaved f32 buffer to a fresh VAO/VBO with the
/// standard 6-float stride and attribute layout (loc 0 = vec2 pos, loc 1 =
/// vec4 col). Returns (vao, vbo).
fn upload_static_vbo_pos_rgba(verts: &[f32]) -> (GLuint, GLuint) {
    let mut vao: GLuint = 0;
    let mut vbo: GLuint = 0;
    unsafe {
        glGenVertexArrays(1, &mut vao);
        glBindVertexArray(vao);
        glGenBuffers(1, &mut vbo);
        glBindBuffer(GL_ARRAY_BUFFER, vbo);
        glBufferData(
            GL_ARRAY_BUFFER,
            (verts.len() * core::mem::size_of::<f32>()) as GLsizeiptr,
            verts.as_ptr() as *const _,
            GL_STATIC_DRAW,
        );
        let stride = (6 * core::mem::size_of::<f32>()) as GLsizei;
        glEnableVertexAttribArray(0);
        glVertexAttribPointer(0, 2, GL_FLOAT, GL_FALSE, stride, core::ptr::null());
        glEnableVertexAttribArray(1);
        glVertexAttribPointer(
            1,
            4,
            GL_FLOAT,
            GL_FALSE,
            stride,
            (2 * core::mem::size_of::<f32>()) as *const _,
        );
        glBindVertexArray(0);
        glBindBuffer(GL_ARRAY_BUFFER, 0);
    }
    (vao, vbo)
}

/// Upload one tessellated `Draw` and store its draw kind. Returns None for
/// degenerate draws (empty). `bitmap_meta` is `Some` only when the draw is
/// `DrawType::Bitmap` and the source bitmap was successfully resolved.
fn upload_draw(
    draw: &ruffle_render::tessellator::Draw,
    gradient_textures: &[GLuint],
    bitmap_meta: Option<&SwitchBitmapHandle>,
    standalone: Option<Arc<StandaloneTexture>>,
    slot: Option<std::rc::Rc<ReloadSlot>>,
    vertex_arena: &mut BufferArena,
    index_arena: &mut BufferArena,
) -> Option<GpuDraw> {
    if draw.vertices.is_empty() || draw.indices.is_empty() {
        return None;
    }

    // (pos.xy, rgba) interleaved.
    let mut verts: Vec<f32> = Vec::with_capacity(draw.vertices.len() * 6);
    for v in &draw.vertices {
        verts.push(v.x);
        verts.push(v.y);
        verts.push(v.color.r as f32 / 255.0);
        verts.push(v.color.g as f32 / 255.0);
        verts.push(v.color.b as f32 / 255.0);
        verts.push(v.color.a as f32 / 255.0);
    }

    // Allocate space in the global arenas. We pay no glGen* per draw —
    // the data lands inside the single mega-VBO and mega-IBO. The arenas'
    // freelists coalesce frees so long sessions don't fragment too badly.
    let vbo_bytes = (verts.len() * core::mem::size_of::<f32>()) as GLsizeiptr;
    let ibo_bytes = (draw.indices.len() * core::mem::size_of::<u32>()) as GLsizeiptr;
    let vbo_offset = vertex_arena.alloc(vbo_bytes)?;
    let ibo_offset = match index_arena.alloc(ibo_bytes) {
        Some(o) => o,
        None => {
            // Roll back the vertex alloc so we don't leak.
            vertex_arena.free_region(vbo_offset, vbo_bytes);
            return None;
        }
    };
    let verts_bytes: &[u8] = unsafe {
        core::slice::from_raw_parts(
            verts.as_ptr() as *const u8,
            verts.len() * core::mem::size_of::<f32>(),
        )
    };
    let indices_bytes: &[u8] = unsafe {
        core::slice::from_raw_parts(
            draw.indices.as_ptr() as *const u8,
            draw.indices.len() * core::mem::size_of::<u32>(),
        )
    };
    vertex_arena.upload(vbo_offset, verts_bytes);
    index_arena.upload(ibo_offset, indices_bytes);
    // Aligned sizes (what the arenas actually consumed) — needed at free
    // time. Mirror the rounding `alloc` does.
    let vbo_size = ((vbo_bytes + ARENA_VBO_ALIGN - 1) / ARENA_VBO_ALIGN) * ARENA_VBO_ALIGN;
    let ibo_size = ((ibo_bytes + ARENA_IBO_ALIGN - 1) / ARENA_IBO_ALIGN) * ARENA_IBO_ALIGN;

    let kind = match &draw.draw_type {
        DrawType::Color => DrawKind::Solid,
        DrawType::Gradient { matrix, gradient } => {
            // The tessellator's `matrix` is already inverted and normalised
            // by `swf_to_gl_matrix` so that `mat * vec3(vert_pixels, 1)` ∈
            // [0, 1] for linear gradients. Just flatten the [[f32; 3]; 3]
            // (column-major) into the 9-float layout `glUniformMatrix3fv`
            // expects.
            let local_matrix = [
                matrix[0][0], matrix[0][1], matrix[0][2],
                matrix[1][0], matrix[1][1], matrix[1][2],
                matrix[2][0], matrix[2][1], matrix[2][2],
            ];
            let texture_index = *gradient;
            if texture_index >= gradient_textures.len() {
                DrawKind::Solid
            } else {
                DrawKind::Gradient {
                    texture_index,
                    local_matrix,
                    gradient_kind: 0, // refined below by caller
                    spread: 0,
                    focal: 0.0,
                }
            }
        }
        DrawType::Bitmap(b) => {
            // `b.matrix` maps `a_pos` (shape pixels) to UV in [0,1] of the
            // source bitmap. The shader composes with `u_uv_remap` to land in
            // the atlas sub-rect (identity remap for a standalone full texture).
            let local_matrix = [
                b.matrix[0][0], b.matrix[0][1], b.matrix[0][2],
                b.matrix[1][0], b.matrix[1][1], b.matrix[1][2],
                b.matrix[2][0], b.matrix[2][1], b.matrix[2][2],
            ];
            // Half a texel of the source bitmap, in local UV. `max(1)` because a
            // degenerate 0-wide bitmap would otherwise send an infinity into the
            // shader's clamp.
            let inset_of = |w: u32, h: u32| -> [f32; 2] {
                [0.5 / w.max(1) as f32, 0.5 / h.max(1) as f32]
            };
            match (bitmap_meta, standalone) {
                // An evictable SWF image: located at draw time.
                _ if slot.is_some() => {
                    let (w, h) = slot.as_ref().map_or((1, 1), |s| (s.width, s.height));
                    DrawKind::Bitmap {
                        atlas_index: 0,
                        uv_remap: [0.0, 0.0, 1.0, 1.0],
                        uv_inset: inset_of(w, h),
                        local_matrix,
                        is_smoothed: b.is_smoothed,
                        is_repeating: b.is_repeating,
                        standalone: None,
                        slot,
                    }
                }
                // Common case: the fill bitmap is atlas-packed.
                (Some(meta), _) => DrawKind::Bitmap {
                    atlas_index: meta.atlas_index,
                    uv_remap: [meta.u0, meta.v0, meta.u1 - meta.u0, meta.v1 - meta.v0],
                    uv_inset: inset_of(meta.width, meta.height),
                    local_matrix,
                    is_smoothed: b.is_smoothed,
                    is_repeating: b.is_repeating,
                    standalone: None,
                    slot: None,
                },
                // >2048 fill: sample its standalone texture directly (full UV).
                // It owns its texture, so CLAMP_TO_EDGE would already keep a
                // NEAREST sample inside it; the inset is passed anyway because it
                // resolves to the same texel centres and keeps one code path.
                (None, Some(tex)) => DrawKind::Bitmap {
                    atlas_index: 0,
                    uv_remap: [0.0, 0.0, 1.0, 1.0],
                    uv_inset: inset_of(tex.width, tex.height),
                    local_matrix,
                    is_smoothed: b.is_smoothed,
                    is_repeating: b.is_repeating,
                    standalone: Some(tex),
                    slot: None,
                },
                // Bitmap never resolved → solid (degenerate; e.g. budget cut).
                (None, None) => DrawKind::Solid,
            }
        }
    };

    Some(GpuDraw {
        vbo_offset,
        vbo_size,
        ibo_offset,
        ibo_size,
        num_indices: draw.indices.len() as GLsizei,
        kind,
    })
}

/// Bake the gradient stops into a 256x1 RGBA texture. Linear interpolation
/// in sRGB regardless of `interpolation` mode — close enough for 1.3.6 iter 1.
fn build_gradient_texture(g: &Gradient) -> GLuint {
    let mut pixels = [0u8; 256 * 4];
    if g.records.is_empty() {
        // Empty: opaque white. Avoids div-by-zero in the loop below.
        for i in 0..256 {
            pixels[i * 4] = 255;
            pixels[i * 4 + 1] = 255;
            pixels[i * 4 + 2] = 255;
            pixels[i * 4 + 3] = 255;
        }
    } else {
        for i in 0..256 {
            // Find the two records bracketing this position.
            let pos = i as f32 / 255.0;
            let target = (pos * 255.0).round() as u8;
            let (lo, hi) = bracket(g, target);
            let color = if lo.ratio == hi.ratio {
                lo.color.clone()
            } else {
                let t = (target as f32 - lo.ratio as f32) / (hi.ratio as f32 - lo.ratio as f32);
                lerp_color(&lo.color, &hi.color, t)
            };
            pixels[i * 4] = color.r;
            pixels[i * 4 + 1] = color.g;
            pixels[i * 4 + 2] = color.b;
            pixels[i * 4 + 3] = color.a;
        }
    }

    let mut tex: GLuint = 0;
    unsafe {
        glGenTextures(1, &mut tex);
        glBindTexture(GL_TEXTURE_2D, tex);
        glPixelStorei(GL_UNPACK_ALIGNMENT, 1);
        glTexImage2D(
            GL_TEXTURE_2D,
            0,
            GL_RGBA8 as GLint,
            256,
            1,
            0,
            GL_RGBA,
            GL_UNSIGNED_BYTE,
            pixels.as_ptr() as *const _,
        );
        glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, GL_LINEAR as GLint);
        glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER, GL_LINEAR as GLint);
        glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_WRAP_S, GL_CLAMP_TO_EDGE as GLint);
        glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_WRAP_T, GL_CLAMP_TO_EDGE as GLint);
        glBindTexture(GL_TEXTURE_2D, 0);
    }
    tex
}

fn bracket(g: &Gradient, target: u8) -> (swf::GradientRecord, swf::GradientRecord) {
    let mut lo = g.records.first().cloned().unwrap();
    let mut hi = g.records.last().cloned().unwrap();
    for r in &g.records {
        if r.ratio <= target {
            lo = r.clone();
        }
        if r.ratio >= target {
            hi = r.clone();
            break;
        }
    }
    (lo, hi)
}

fn lerp_color(a: &Color, b: &Color, t: f32) -> Color {
    let t = t.clamp(0.0, 1.0);
    let mix = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color {
        r: mix(a.r, b.r),
        g: mix(a.g, b.g),
        b: mix(a.b, b.b),
        a: mix(a.a, b.a),
    }
}

/// Convert a Ruffle `Bitmap` into an RGBA byte buffer + dims. Returns None
/// for empty or unrecognised formats.
fn bitmap_to_rgba_bytes(bitmap: &Bitmap<'_>) -> Option<(Vec<u8>, u32, u32)> {
    let rgba = bitmap.clone().to_rgba();
    let w = rgba.width();
    let h = rgba.height();
    if w == 0 || h == 0 || rgba.format() != BitmapFormat::Rgba {
        return None;
    }
    Some((rgba.data().to_vec(), w, h))
}

// ─── Downcast helpers ─────────────────────────────────────────────────────────

fn as_switch_shape(handle: &ShapeHandle) -> Option<&SwitchShapeHandle> {
    <dyn Any>::downcast_ref(&*handle.0)
}

fn as_switch_bitmap(handle: &BitmapHandle) -> Option<&SwitchBitmapHandle> {
    <dyn Any>::downcast_ref(&*handle.0)
}

fn as_standalone_bitmap(handle: &BitmapHandle) -> Option<&StandaloneBitmap> {
    <dyn Any>::downcast_ref(&*handle.0)
}

fn as_dropped_bitmap(handle: &BitmapHandle) -> Option<&DroppedBitmap> {
    <dyn Any>::downcast_ref(&*handle.0)
}

fn as_reloadable_bitmap(handle: &BitmapHandle) -> Option<&ReloadableBitmap> {
    <dyn Any>::downcast_ref(&*handle.0)
}

/// Cached cover texture for a library game (v1.2.0 JOUER grid). Looked up by
/// `.swf` basename; a cover is decoded + uploaded once on first display and
/// kept for the backend's lifetime (the GL context outlives the library UI).
/// `Default` = no cover image found → the grid draws a generated tile.
#[derive(Clone, Copy)]
enum CoverTex {
    /// `amb` is the blurred backdrop the kit draws behind a cover that does
    /// not fill its box (`render/kit/cover.rs`); 0 when there is none.
    Image { tex: GLuint, w: u32, h: u32, amb: GLuint },
    Default,
}

/// Process-wide cover-texture cache. A function-local `static` keeps the GL
/// handles out of the (cloned) library snapshot; a plain Vec is fine for the
/// handful of games shown per session.
fn cover_cache() -> &'static std::sync::Mutex<std::vec::Vec<(std::string::String, CoverTex, u64)>> {
    static C: std::sync::Mutex<std::vec::Vec<(std::string::String, CoverTex, u64)>> =
        std::sync::Mutex::new(std::vec::Vec::new());
    &C
}

/// Full-resolution covers for the launch/quit reveal (see `cover_full_for`),
/// kept apart from the gallery's tile-sized textures.
fn reveal_cover_cache() -> &'static std::sync::Mutex<std::vec::Vec<(std::string::String, CoverTex)>>
{
    static C: std::sync::Mutex<std::vec::Vec<(std::string::String, CoverTex)>> =
        std::sync::Mutex::new(std::vec::Vec::new());
    &C
}

/// How many full-res covers stay resident.
///
/// Was two — the game being launched and the one just quit — which held while the
/// only full-res consumer was the launch reveal. The LISTE and BANDE layouts broke
/// that: their detail panel shows the SELECTED game at size, so moving the cursor
/// asks for a different full-res cover every step. At two entries the cache evicted
/// on every move and re-decoded, 13 to 24 ms of PNG/JPEG per row, outside the
/// one-decode-per-frame budget that protects the thumbnail path — a dropped frame
/// per step for as long as a direction is held. Eight covers a screenful, ~22 MB
/// worst case at 1126x619.
const REVEAL_CACHE_MAX: usize = 8;

/// Append one axis-aligned quad (two triangles) in PIXEL space with a flat
/// colour to a text batch. Layout matches the solid quad VAO: vec2 pos + vec4
/// rgba, stride 24.
fn push_text_quad(verts: &mut std::vec::Vec<f32>, x: f32, y: f32, w: f32, h: f32, c: [f32; 4]) {
    let (x0, y0, x1, y1) = (x, y, x + w, y + h);
    for (px, py) in [
        (x0, y0), (x1, y0), (x1, y1),
        (x0, y0), (x1, y1), (x0, y1),
    ] {
        verts.extend_from_slice(&[px, py, c[0], c[1], c[2], c[3]]);
    }
}

/// Cache-only cover lookup: never touches the SD, never decodes. `None` means
/// "not resolved yet" — the gallery draws the generated tile for this frame and
/// lets the per-frame decode budget pick it up. Decoding a cover costs ~25 ms
/// (read + PNG/JPEG decode + texture upload), so a 71-game library resolved
/// eagerly cost ~1.9 s of black screen on the first frame.
fn cover_lookup(basename: &str) -> Option<CoverTex> {
    cover_ready(basename).map(|(t, _)| t)
}

/// The cover AND the tick it became available, for the fade-in.
fn cover_ready(basename: &str) -> Option<(CoverTex, u64)> {
    cover_cache()
        .lock()
        .ok()?
        .iter()
        .find(|(b, _, _)| b == basename)
        .map(|(_, t, at)| (*t, *at))
}

/// How far into its fade a cover is: 0 the frame it lands, 1 once settled.
///
/// A tile decodes on some later frame than the one that first drew it, so the
/// generated placeholder was being replaced by the artwork between two frames —
/// which reads as a blink, one per cover, all the way down a scroll.
fn cover_fade(ready_at: u64) -> f32 {
    const FADE_TICKS: u64 = 19_200_000 / 100; // ~180 ms at the 19.2 MHz tick
    let now = unsafe { ruffle_tick_now() };
    let dt = now.saturating_sub(ready_at);
    if dt >= FADE_TICKS {
        1.0
    } else {
        dt as f32 / FADE_TICKS as f32
    }
}

/// How many covers may be decoded per gallery frame. Above this the remaining
/// tiles show their generated tile and resolve on the following frames, so
/// entering / scrolling the gallery never stalls on a burst of decodes.
///
/// ONE: a cover costs ~5 ms (small) to ~40 ms (a 1126x619 PNG) even with the
/// resolve probes served from the scan index, so decoding three in a frame
/// stacked into the 50-120 ms hitches measured while scrolling. At one per
/// frame the grid still fills in over a handful of frames, and idle frames keep
/// working through the backlog.
const COVER_DECODES_PER_FRAME: usize = 1;

/// Cumulative cover decode cost (ticks) + count, for the boot breakdown.
static COVER_DECODE_TICKS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static COVER_DECODE_COUNT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);


/// Drop a game's cached cover texture so the next frame re-resolves it (after
/// the user sets a new cover via OPTIONS > JAQUETTE). The old GL texture handle
/// is leaked — covers are tiny and this is rare, not worth a cross-thread
/// delete (the GL context only frees at app exit anyway).
pub fn invalidate_cover(basename: &str) {
    if let Ok(mut cache) = cover_cache().lock() {
        cache.retain(|(b, _, _)| b != basename);
    }
    // The reveal's full-res copy is keyed the same way and would otherwise keep
    // showing the previous art after a JAQUETTE change.
    if let Ok(mut cache) = reveal_cover_cache().lock() {
        cache.retain(|(b, _)| b != basename);
    }
}

/// One library tile's layout (row index + horizontal center), shared from the
/// gallery renderer to the input handler. The JOUER gallery has a variable
/// number of tiles per row (each cover keeps its natural width), so Up/Down
/// can't use fixed columns — input reads this to jump to the spatially nearest
/// tile in the row above/below.
#[derive(Clone, Copy, Default)]
pub struct GalleryCell {
    pub row: u32,
    pub cx: f32,
    /// Content-space tile rect for touch hit-testing. `x`/`w` are screen px;
    /// `y` is PRE vertical scroll, so screen y = `y - scroll_px` (read the live
    /// `scroll_px` from `gallery_view_read`).
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

fn gallery_cache() -> &'static std::sync::Mutex<(std::vec::Vec<GalleryCell>, u32)> {
    static C: std::sync::Mutex<(std::vec::Vec<GalleryCell>, u32)> =
        std::sync::Mutex::new((std::vec::Vec::new(), 0));
    &C
}

/// `(per-tile cells in filtered order, total row count)` from the last gallery
/// render. Read by `library::handle_list_input` for 2D navigation.
pub fn gallery_layout_read() -> (std::vec::Vec<GalleryCell>, u32) {
    gallery_cache().lock().map(|g| (g.0.clone(), g.1)).unwrap_or_default()
}

/// Selected tile's current screen rect (x,y,w,h) from the last gallery render.
/// The game launch/quit reveal grows the cover from / shrinks it to this box.
fn gallery_sel_rect() -> &'static std::sync::Mutex<(f32, f32, f32, f32)> {
    static R: std::sync::Mutex<(f32, f32, f32, f32)> =
        std::sync::Mutex::new((0.0, 0.0, 0.0, 0.0));
    &R
}

/// The selected tile's last-rendered screen rect (for the launch/quit reveal).
pub fn gallery_sel_rect_read() -> (f32, f32, f32, f32) {
    gallery_sel_rect().lock().map(|r| *r).unwrap_or((0.0, 0.0, 0.0, 0.0))
}

/// Live JOUER gallery viewport metrics, published each frame for the touch
/// layer: `(scroll_px, pitch, band_top, band_bot, rows_total, rows_visible)`.
#[derive(Clone, Copy, Default)]
struct GalleryView {
    scroll_px: f32,
    pitch: f32,
    band_top: f32,
    band_bot: f32,
    rows_total: u32,
    rows_visible: u32,
    /// True when the band scrolls SIDEWAYS (the strip layout). The touch layer
    /// reads it to know which finger axis drives the scroll; everything else in
    /// the gesture is identical.
    horizontal: bool,
    /// Valid range of `scroll_px`. Vertical bands run 0..=max; the strip's offset
    /// is negative-going, so the range is published rather than derived — the
    /// touch layer has no business knowing a layout's geometry.
    off_min: f32,
    off_max: f32,
}

fn gallery_view() -> &'static std::sync::Mutex<GalleryView> {
    static V: std::sync::Mutex<GalleryView> = std::sync::Mutex::new(GalleryView {
        scroll_px: 0.0,
        pitch: 0.0,
        band_top: 0.0,
        band_bot: 0.0,
        horizontal: false,
        off_min: 0.0,
        off_max: 0.0,
        rows_total: 0,
        rows_visible: 0,
    });
    &V
}

/// Read the last-rendered gallery viewport metrics (see `GalleryView`). All
/// zero before the first gallery frame, so the touch layer no-ops until then.
pub fn gallery_view_read() -> (f32, f32, f32, f32, u32, u32) {
    gallery_view()
        .lock()
        .map(|v| (v.scroll_px, v.pitch, v.band_top, v.band_bot, v.rows_total, v.rows_visible))
        .unwrap_or_default()
}

/// Axis + scroll range of the current band: `(horizontal, off_min, off_max)`.
pub fn gallery_axis_read() -> (bool, f32, f32) {
    gallery_view()
        .lock()
        .map(|v| (v.horizontal, v.off_min, v.off_max))
        .unwrap_or((false, 0.0, 0.0))
}

/// Touch-drag scroll override. `Some(px)` makes the gallery use this exact pixel
/// scroll (1:1 finger tracking) instead of easing toward the row offset; cleared
/// to `None` on finger release so the glide resumes and settles onto a row.
fn gallery_touch_scroll() -> &'static std::sync::Mutex<Option<f32>> {
    static T: std::sync::Mutex<Option<f32>> = std::sync::Mutex::new(None);
    &T
}

pub fn gallery_touch_scroll_set(v: Option<f32>) {
    if let Ok(mut t) = gallery_touch_scroll().lock() {
        *t = v;
    }
}

fn gallery_touch_scroll_read() -> Option<f32> {
    gallery_touch_scroll().lock().map(|t| *t).unwrap_or(None)
}

/// Hit-test a screen-space point against the last-rendered JOUER gallery.
/// Returns the tile index (filtered order, same space as `selection`) under the
/// point, or `None`. Used by the touch layer for tap-to-select / tap-to-launch.
pub fn gallery_hit_test(px: f32, py: f32) -> Option<usize> {
    let (cells, _rows) = gallery_layout_read();
    let (scroll_px, _pitch, band_top, band_bot, _rt, _rv) = gallery_view_read();
    if py < band_top || py > band_bot {
        return None;
    }
    // A horizontal band carries its scroll in X, and publishes cell `y` already in
    // screen space — subtracting the scroll there (which is what a vertical band
    // needs) threw the test rows off screen, so no tile was ever hit.
    let (horizontal, _, _) = gallery_axis_read();
    let scroll_px = if horizontal { 0.0 } else { scroll_px };
    for (i, c) in cells.iter().enumerate() {
        let sy = c.y - scroll_px;
        if px >= c.x && px <= c.x + c.w && py >= sy && py <= sy + c.h {
            return Some(i);
        }
    }
    None
}

/// Eased visual state for the JOUER gallery (v1.2.0 polish). The input layer
/// still works in discrete tile/row indices; this is purely cosmetic — the
/// selection frame glides toward the active tile and the row window scrolls in
/// pixels instead of snapping. Process-wide like `gallery_cache`; snapped to
/// its target whenever `inited` is false (set by `gallery_anim_reset` on every
/// fresh entry into the gallery, so the cursor never streaks from a stale spot).
#[derive(Clone, Copy)]
struct GalleryAnim {
    inited: bool,
    last_tick: u64,
    last_sel: usize,
    /// Selection frame in CONTENT space: `sel_x`/`sel_w` are screen px, `sel_y`
    /// is pre-scroll (screen y = `sel_y - scroll_px`) so the cursor glide and
    /// the scroll glide stay independent.
    sel_x: f32,
    sel_y: f32,
    sel_w: f32,
    /// Vertical scroll in pixels, eased toward `scroll_offset * pitch`.
    scroll_px: f32,
    /// Decays 1->0 after a selection change; drives a small frame "pop".
    pop: f32,
}

fn gallery_anim() -> &'static std::sync::Mutex<GalleryAnim> {
    static A: std::sync::Mutex<GalleryAnim> = std::sync::Mutex::new(GalleryAnim {
        inited: false,
        last_tick: 0,
        last_sel: 0,
        sel_x: 0.0,
        sel_y: 0.0,
        sel_w: 0.0,
        scroll_px: 0.0,
        pop: 0.0,
    });
    &A
}

/// Snap the gallery animation to its target on the next frame (no glide).
/// Called from `library` whenever the gallery is (re)entered with a possibly
/// far-away selection — fresh open, navbar switch into JOUER, new search — so
/// the cursor doesn't slide across the whole screen from a stale position.
pub fn gallery_anim_reset() {
    if let Ok(mut a) = gallery_anim().lock() {
        a.inited = false;
    }
}

/// Frame-rate aware approach of `cur` toward `target`. `rate` ~ 1/time-constant
/// (s^-1); `dt` is the frame delta in seconds. Linear in dt (no `exp()`: we
/// stay off libm like `approx_sin`), which is plenty smooth at ~60 fps.
/// Eased state for the LIST / STRIP / SHELF layouts.
///
/// A struct rather than repeated `eased_list_y` calls: that helper holds ONE
/// slot keyed by a caller id, so asking it to ease a scroll and a highlight in
/// the same frame made each call reset the other's state and nothing was damped
/// at all. The grid has always had its own struct for exactly this reason; these
/// layouts need the same, or they step where the grid glides.
struct HomeAnim {
    inited: bool,
    last_tick: u64,
    /// Vertical scroll of the list, in pixels.
    scroll_px: f32,
    /// Highlight bar, in CONTENT space (screen y = `hl_y - scroll_px`), so the
    /// cursor glide and the scroll glide stay independent — moving inside one
    /// screenful slides the bar, changing screenful slides the rows.
    hl_y: f32,
    /// Horizontal offset of the strip / shelf.
    off: f32,
    /// Fractional selected index: drives the magnification falloff, so arriving
    /// on a tile swells it while the one you left settles back, continuously.
    sel_pos: f32,
}

fn home_anim() -> &'static std::sync::Mutex<HomeAnim> {
    static A: std::sync::Mutex<HomeAnim> = std::sync::Mutex::new(HomeAnim {
        inited: false,
        last_tick: 0,
        scroll_px: 0.0,
        hl_y: 0.0,
        off: 0.0,
        sel_pos: 0.0,
    });
    &A
}

/// Advance the layout animation toward its targets and return the eased values.
/// Snaps on the first frame after a reset so opening a view never slides in from
/// wherever the previous one left off.
fn home_anim_step(
    now: u64,
    target_scroll: f32,
    target_hl: f32,
    target_off: f32,
    target_sel: f32,
) -> (f32, f32, f32, f32) {
    let mut a = match home_anim().lock() {
        Ok(g) => g,
        Err(_) => return (target_scroll, target_hl, target_off, target_sel),
    };
    if !a.inited {
        a.inited = true;
        a.last_tick = now;
        a.scroll_px = target_scroll;
        a.hl_y = target_hl;
        a.off = target_off;
        a.sel_pos = target_sel;
    } else {
        let freq = unsafe { ruffle_tick_freq() } as f32;
        let dt = if freq > 0.0 {
            (now.saturating_sub(a.last_tick) as f32 / freq).min(0.1)
        } else {
            1.0 / 60.0
        };
        a.last_tick = now;
        // Same rates as the grid, so the three layouts feel like one app.
        a.scroll_px = ease_to(a.scroll_px, target_scroll, dt, 16.0);
        a.hl_y = ease_to(a.hl_y, target_hl, dt, 18.0);
        a.off = ease_to(a.off, target_off, dt, 16.0);
        a.sel_pos = ease_to(a.sel_pos, target_sel, dt, 14.0);
    }
    (a.scroll_px, a.hl_y, a.off, a.sel_pos)
}

/// Overwrite the eased selection with the value a finger is dictating.
///
/// Without it `sel_pos` keeps converging on the selection from BEFORE the drag —
/// which is only written on finger-up — so the first frame after release renders
/// at the stale value and everything derived from it recoils at once: size, veil,
/// the light under the shelf, the rail. With it, release eases from where the
/// finger actually left the row.
pub fn home_anim_set_sel(sel_pos: f32) {
    if let Ok(mut a) = home_anim().lock() {
        if a.inited {
            a.sel_pos = sel_pos;
        }
    }
}

/// Forget the layout animation, so the next frame snaps instead of sliding in
/// from the previous view's state.
pub fn home_anim_reset() {
    // A new layout: its tiles start closed.
    tile_open_reset();
    if let Ok(mut a) = home_anim().lock() {
        a.inited = false;
    }
}

fn ease_to(cur: f32, target: f32, dt: f32, rate: f32) -> f32 {
    let t = (rate * dt).clamp(0.0, 1.0);
    cur + (target - cur) * t
}

/// Horizontal content slide for tab transitions (v1.2.0). The navbar stays put;
/// `library::render` slides the active tab's content in from the side the user
/// pressed (L = from the left, R = from the right) over a short ease-out. Begun
/// by `tab_transition_begin` (which knows the L/R direction), stepped each frame
/// by `tab_slide_translate`. Tabs slide; modals/editors scale — that split is
/// deliberate (lateral siblings slide, things that "pop up" scale).
#[derive(Clone, Copy)]
struct TabSlide {
    active: bool,
    inited: bool,
    last_tick: u64,
    t: f32,   // 0..1 progress
    dir: f32, // +1 = enter from right (R), -1 = enter from left (L)
}

fn tab_slide() -> &'static std::sync::Mutex<TabSlide> {
    static A: std::sync::Mutex<TabSlide> = std::sync::Mutex::new(TabSlide {
        active: false,
        inited: false,
        last_tick: 0,
        t: 0.0,
        dir: 1.0,
    });
    &A
}

/// Kick off a tab-change content slide. `dir` is +1 for the NEXT tab (R, content
/// enters from the right) and -1 for the PREVIOUS tab (L).
pub fn tab_transition_begin(dir: f32) {
    if let Ok(mut a) = tab_slide().lock() {
        a.active = true;
        a.inited = false;
        a.t = 0.0;
        a.dir = dir;
    }
}

/// Advance the tab slide and return the content x-translate in px (0 when idle).
/// The content eases from `slide_px * dir` to 0; `now` is absolute ticks.
pub fn tab_slide_translate(now: u64, slide_px: f32) -> f32 {
    let mut a = match tab_slide().lock() {
        Ok(g) => g,
        Err(_) => return 0.0,
    };
    if !a.active {
        return 0.0;
    }
    if !a.inited {
        a.inited = true;
        a.last_tick = now;
    }
    let freq = unsafe { ruffle_tick_freq() } as f32;
    let dt = if freq > 0.0 {
        (now.saturating_sub(a.last_tick) as f32 / freq).min(0.1)
    } else {
        1.0 / 60.0
    };
    a.last_tick = now;
    // ~6.5 /s => ~150 ms total. Ease-OUT (remaining squared) so the content
    // decelerates as it settles into place.
    a.t = (a.t + dt * 6.5).min(1.0);
    if a.t >= 1.0 {
        a.active = false;
        return 0.0;
    }
    let remaining = 1.0 - a.t;
    remaining * remaining * slide_px * a.dir
}

/// True while a tab slide is mid-flight (so `library::render` applies it).
pub fn tab_slide_active() -> bool {
    tab_slide().lock().map(|a| a.active).unwrap_or(false)
}


/// Modal "pop" (v1.2.0): a panel/modal screen scales UP from small to full when
/// it opens, and scales DOWN to a point when it closes. The close screen-swap is
/// deferred by `library` until this reports done, so the modal stays drawn while
/// it shrinks. The dim backdrop stays put (drawn scale/translate-immune via
/// `fill_screen_dim` / `glClear`). Stepped each frame by `modal_scale_step`.
#[derive(Clone, Copy, PartialEq)]
enum ModalMode {
    Idle,
    Opening,
    Closing,
}

#[derive(Clone, Copy)]
struct ModalAnim {
    mode: ModalMode,
    inited: bool,
    last_tick: u64,
    t: f32, // 0..1 progress within the current mode
}

fn modal_anim() -> &'static std::sync::Mutex<ModalAnim> {
    static A: std::sync::Mutex<ModalAnim> = std::sync::Mutex::new(ModalAnim {
        mode: ModalMode::Idle,
        inited: false,
        last_tick: 0,
        t: 0.0,
    });
    &A
}

const MODAL_OPEN_FROM: f32 = 0.55; // start scale when opening
const MODAL_CLOSE_TO: f32 = 0.0; // end scale when closing (vanishes to the pivot)

/// Begin the open pop (scale grows to full). Called when a modal first appears.
pub fn modal_open_begin() {
    if let Ok(mut a) = modal_anim().lock() {
        a.mode = ModalMode::Opening;
        a.inited = false;
        a.t = 0.0;
    }
}

/// Begin the close pop (scale shrinks away). Called by `library` when a modal's
/// close is requested; the real screen swap waits for `modal_scale_step` to
/// report the close finished.
pub fn modal_close_begin() {
    if let Ok(mut a) = modal_anim().lock() {
        a.mode = ModalMode::Closing;
        a.inited = false;
        a.t = 0.0;
    }
}

/// True while a close pop is mid-flight (input is suspended during this so the
/// modal can't be re-navigated as it scales away).
pub fn modal_close_active() -> bool {
    modal_anim().lock().map(|a| a.mode == ModalMode::Closing).unwrap_or(false)
}

/// Advance the modal pop. Returns `(scale, active, close_done)`:
///   - `scale`: uniform scale to apply to the modal content this frame.
///   - `active`: true while opening or closing (caller applies `scale`).
///   - `close_done`: true on the single frame a close finishes (caller then
///     swaps to the deferred target screen).
/// Whether the window timeline is playing its close.
fn modal_anim_closing() -> bool {
    modal_anim().lock().map_or(false, |a| a.mode == ModalMode::Closing)
}

pub fn modal_scale_step(now: u64) -> (f32, bool, bool) {
    let mut a = match modal_anim().lock() {
        Ok(g) => g,
        Err(_) => return (1.0, false, false),
    };
    if a.mode == ModalMode::Idle {
        return (1.0, false, false);
    }
    if !a.inited {
        a.inited = true;
        a.last_tick = now;
    }
    let freq = unsafe { ruffle_tick_freq() } as f32;
    let dt = if freq > 0.0 {
        (now.saturating_sub(a.last_tick) as f32 / freq).min(0.1)
    } else {
        1.0 / 60.0
    };
    a.last_tick = now;
    match a.mode {
        ModalMode::Opening => {
            a.t = (a.t + dt * 7.0).min(1.0); // ~140 ms
            if a.t >= 1.0 {
                a.mode = ModalMode::Idle;
                return (1.0, false, false);
            }
            let e = 1.0 - (1.0 - a.t) * (1.0 - a.t); // ease-out (settle in)
            (MODAL_OPEN_FROM + (1.0 - MODAL_OPEN_FROM) * e, true, false)
        }
        ModalMode::Closing => {
            a.t = (a.t + dt * 9.0).min(1.0); // ~110 ms, snappier
            if a.t >= 1.0 {
                a.mode = ModalMode::Idle;
                return (1.0, false, true);
            }
            let e = a.t * a.t; // ease-in (accelerate away)
            (1.0 + (MODAL_CLOSE_TO - 1.0) * e, true, false)
        }
        ModalMode::Idle => (1.0, false, false),
    }
}

/// Eased selection highlight for the plain vertical-list tabs (IMPORTER /
/// REGLAGES), so the cursor glides between rows like the JOUER frame. Tracks a
/// single screen-space y; `key` distinguishes lists so switching tabs snaps the
/// highlight to the new layout instead of sliding across it.
#[derive(Clone, Copy)]
struct ListHl {
    inited: bool,
    key: u32,
    last_tick: u64,
    y: f32,
}

fn list_hl() -> &'static std::sync::Mutex<ListHl> {
    static A: std::sync::Mutex<ListHl> = std::sync::Mutex::new(ListHl {
        inited: false,
        key: 0,
        last_tick: 0,
        y: 0.0,
    });
    &A
}

/// Tappable rows of whatever list drew last, in SCREEN pixels.
///
/// The gallery has had a cell table since v1.2.0 (`gallery_cache`) and it is why
/// the home is touchable while every modal in the app is not. This is the same
/// contract for everything else: the renderer publishes the rectangles it just
/// drew, the touch layer reads them and never learns a layout.
///
/// `kind` is the drawing screen's `modal_kind`, so a tap can only ever act on
/// the list actually on screen — a stale table from the panel before cannot be
/// hit, which is the bug the gallery's own `draw_home_empty` had to fix.
static UI_CELLS: std::sync::Mutex<(u32, std::vec::Vec<(f32, f32, f32, f32)>)> =
    std::sync::Mutex::new((0, std::vec::Vec::new()));

pub fn ui_cells_publish(kind: u32, rects: std::vec::Vec<(f32, f32, f32, f32)>) {
    if let Ok(mut g) = UI_CELLS.lock() {
        *g = (kind, rects);
    }
}

/// Where the three navbar tabs are, and whether the strip is even on screen.
///
/// Its OWN table, not `UI_CELLS`: the navbar is drawn OVER a screen that has
/// published rows of its own, and one table cannot hold both. It is also the
/// only thing on screen that is not part of the screen, so it is asked first.
static NAVBAR_CELLS: std::sync::Mutex<(bool, [(f32, f32, f32, f32); 3])> =
    std::sync::Mutex::new((false, [(0.0, 0.0, 0.0, 0.0); 3]));

pub fn navbar_publish(rects: [(f32, f32, f32, f32); 3]) {
    if let Ok(mut g) = NAVBAR_CELLS.lock() {
        *g = (true, rects);
    }
}

/// The strip is not drawn on sub-screens; say so, or a tap would switch tabs
/// from inside a modal that has no tabs.
pub fn navbar_clear() {
    if let Ok(mut g) = NAVBAR_CELLS.lock() {
        g.0 = false;
    }
}

/// Which tab is under `(x, y)`, or `None`.
pub fn navbar_hit(x: f32, y: f32) -> Option<usize> {
    let g = NAVBAR_CELLS.lock().ok()?;
    if !g.0 {
        return None;
    }
    g.1.iter().position(|(rx, ry, rw, rh)| {
        *rw > 0.0 && *rh > 0.0 && x >= *rx && x <= rx + rw && y >= *ry && y <= ry + rh
    })
}

/// Index of the row under `(x, y)`, but only if the published table belongs to
/// `live` — the screen that is up RIGHT NOW, read by the caller from the state
/// it owns.
///
/// Two things this must not be, both of which it has been:
///
/// - the table's own tag (`UI_CELLS.0`). That is what both callers used to pass,
///   so the test compared the tag with itself and only `kind == 0` ever rejected
///   anything. The check the comment promised did not exist.
/// - the stamp `ui_screen_kind()`. That is written during RENDER, and C++ serves
///   buttons before touch in the same frame, so on the frame a button changes
///   screens the stamp still names the old one and matches its stale table
///   exactly. One frame of lag is all it takes: the finger is hit-tested against
///   the previous screen's geometry and the index lands in the new screen's
///   selection.
///
/// Read from the live state, it cannot lag and cannot be laundered into a
/// tautology.
pub fn ui_cells_hit(live: u32, x: f32, y: f32) -> Option<usize> {
    if live == 0 {
        return None;
    }
    let g = UI_CELLS.lock().ok()?;
    if g.0 != live {
        return None;
    }
    // A ZERO-SIZE rect is how the paging lists mark a row that is scrolled out
    // of view, and the test below is inclusive at both edges -- so every one of
    // them sat at the origin waiting for a tap on the very first pixel. That is
    // a real press coordinate, not a theoretical one: touch arrives in panel
    // units, and `row_touch_feed` reports the PRESS position.
    g.1.iter().position(|(rx, ry, rw, rh)| {
        *rw > 0.0 && *rh > 0.0 && x >= *rx && x <= rx + rw && y >= *ry && y <= ry + rh
    })
}

/// Eased SCROLL offset, in pixels, for lists that page by whole rows.
///
/// Their `scroll_offset` is an integer count, so the whole list jumped a row at
/// a time while the cursor inside it glided — the two halves of one movement
/// disagreeing. This eases the pixel offset the rows are drawn at; the integer
/// stays the source of truth, nothing about the input side changes.
/// Four slots, not one: a modal that pages by rows is drawn OVER a screen that
/// also does (the keymap editor sits on the games list), and a single slot would
/// have the two of them stealing the key from each other every frame — both
/// snapping, neither easing. Four covers every stack we draw; a fifth list would
/// evict the oldest, which merely costs it one snap.
fn list_scroll() -> &'static std::sync::Mutex<[ListHl; 4]> {
    const EMPTY: ListHl = ListHl {
        inited: false,
        key: 0,
        last_tick: 0,
        y: 0.0,
    };
    static A: std::sync::Mutex<[ListHl; 4]> = std::sync::Mutex::new([EMPTY; 4]);
    &A
}

fn eased_scroll_px(target: f32, key: u32, now: u64) -> f32 {
    // A finger on the list wins over the easing: it tracks 1:1, and the slot is
    // dragged along with it so releasing does not snap back to where the eased
    // value had got to before the drag started.
    let held = match row_touch_scroll_read() {
        Some((k, px)) if k == key => Some(px),
        _ => None,
    };
    let mut slots = match list_scroll().lock() {
        Ok(g) => g,
        Err(_) => return held.unwrap_or(target),
    };
    // Ours if it exists; otherwise the first free slot, otherwise the one that
    // has gone longest without being drawn.
    let idx = match slots.iter().position(|s| s.inited && s.key == key) {
        Some(i) => i,
        None => match slots.iter().position(|s| !s.inited) {
            Some(i) => i,
            None => {
                let mut oldest = 0;
                for i in 1..slots.len() {
                    if slots[i].last_tick < slots[oldest].last_tick {
                        oldest = i;
                    }
                }
                oldest
            }
        },
    };
    let a = &mut slots[idx];
    // A finger beats the easing outright, and the slot is dragged along with it:
    // without that write, releasing would snap back to wherever the easing had
    // got to before the drag began.
    if let Some(px) = held {
        a.inited = true;
        a.key = key;
        a.last_tick = now;
        a.y = px;
        return px;
    }
    if !a.inited || a.key != key {
        a.inited = true;
        a.key = key;
        a.last_tick = now;
        a.y = target;
        return target;
    }
    let freq = unsafe { ruffle_tick_freq() } as f32;
    let dt = if freq > 0.0 {
        (now.saturating_sub(a.last_tick) as f32 / freq).min(0.1)
    } else {
        1.0 / 60.0
    };
    a.last_tick = now;
    a.y = ease_to(a.y, target, dt, 18.0);
    a.y
}

/// Live geometry of the row list drawn this frame, so a finger can drag it the
/// way it drags the gallery. Published by the paging lists themselves: the touch
/// layer has no business knowing where a panel puts its rows, and the geometry
/// here is responsive (the keymap panel shrinks with the picture).
#[derive(Clone, Copy, Default)]
pub struct RowView {
    /// Glide key of the list, so an override can name the list it belongs to.
    pub key: u32,
    /// Touch-table id of the screen that published this, checked against the
    /// live one before any gesture acts: a stale view from the last paging list
    /// is still sitting here when a panel with no scrolling is up.
    pub kind: u32,
    pub band_top: f32,
    pub band_bot: f32,
    pub row_h: f32,
    /// Where the list is drawn right now, in px.
    pub scroll_px: f32,
    /// Largest legal `scroll_px`. Zero means the list fits and cannot scroll.
    pub max_off: f32,
    pub total: u32,
    pub visible: u32,
    /// Absolute row index of scrolling row 0. Nonzero only where a panel keeps
    /// rows pinned above the scrolling part (the directory tree's CHOISIR and
    /// REMONTER), so `offset` stays in the space the caller stores it in while
    /// the cursor bounds come back in the space the caller's `selection` uses.
    pub base: u32,
}

fn row_view() -> &'static std::sync::Mutex<RowView> {
    static V: std::sync::Mutex<RowView> = std::sync::Mutex::new(RowView {
        key: 0,
        kind: 0,
        band_top: 0.0,
        band_bot: 0.0,
        row_h: 0.0,
        scroll_px: 0.0,
        max_off: 0.0,
        total: 0,
        visible: 0,
        base: 0,
    });
    &V
}

fn row_view_publish(v: RowView) {
    if let Ok(mut g) = row_view().lock() {
        *g = v;
    }
}

/// Touch-drag scroll override for a row list: `(glide key, px)`. While a finger
/// is down the list draws at exactly this offset (1:1 tracking) instead of
/// easing toward the integer row; cleared on release so the glide settles.
fn row_touch_scroll() -> &'static std::sync::Mutex<Option<(u32, f32)>> {
    static T: std::sync::Mutex<Option<(u32, f32)>> = std::sync::Mutex::new(None);
    &T
}

fn row_touch_scroll_read() -> Option<(u32, f32)> {
    row_touch_scroll().lock().map(|t| *t).unwrap_or(None)
}

/// What one touch sample did to a row list.
pub enum RowTouch {
    /// Nothing to act on this sample.
    None,
    /// A drag is in flight; the list is following the finger.
    Dragging,
    /// The finger lifted without dragging: a tap, at the press position.
    Tap(f32, f32),
    /// A drag ended: commit this integer row offset, and pull the cursor into
    /// the window it left the list on (`sel_lo..=sel_hi`, inclusive).
    Scrolled { offset: usize, sel_lo: usize, sel_hi: usize },
}

/// Gesture state for the row-list drag. Its own, not the library's: the in-game
/// keymap editor feeds the same gesture from a different entry point, and two
/// copies of this would drift apart the first time either was touched.
struct RowDrag {
    down: bool,
    dragging: bool,
    start_x: f32,
    start_y: f32,
    start_px: f32,
    key: u32,
}

fn row_drag() -> &'static std::sync::Mutex<RowDrag> {
    static D: std::sync::Mutex<RowDrag> = std::sync::Mutex::new(RowDrag {
        down: false,
        dragging: false,
        start_x: 0.0,
        start_y: 0.0,
        start_px: 0.0,
        key: 0,
    });
    &D
}

/// Feed one touchscreen sample to the row-list gesture. Drag inside a scrolling
/// band to scroll it; lift without dragging and it is a tap on a row.
///
/// Movement past the threshold in EITHER axis kills the tap even when the list
/// cannot scroll: a finger that travelled 60 px across a panel was not pointing
/// at the row it happens to be over when it lifts.
pub fn row_touch_feed(x: f32, y: f32, pressed: bool) -> RowTouch {
    const DRAG_THRESH: f32 = 16.0;
    let view = row_view().lock().map(|v| *v).unwrap_or_default();
    // A view is only worth acting on while the screen that published it is still
    // the screen on display.
    let live = view.kind != 0 && view.kind == ui_screen_kind() && view.max_off > 0.0
        && view.row_h > 0.0;
    let Ok(mut d) = row_drag().lock() else {
        return RowTouch::None;
    };
    if pressed && !d.down {
        d.down = true;
        d.dragging = false;
        d.start_x = x;
        d.start_y = y;
        d.start_px = view.scroll_px;
        // Only a press that lands INSIDE the band can scroll it. Remembering the
        // key here rather than reading it on release is what keeps a drag bound
        // to the list it started on.
        d.key = if live && y >= view.band_top && y <= view.band_bot { view.key } else { 0 };
        return RowTouch::None;
    }
    if pressed && d.down {
        let dx = x - d.start_x;
        let dy = y - d.start_y;
        if !d.dragging && (dx * dx + dy * dy) > DRAG_THRESH * DRAG_THRESH {
            d.dragging = true;
        }
        if d.dragging && d.key != 0 && live && d.key == view.key {
            // Drag down pulls earlier rows into view, like every list on the
            // console and unlike a scrollbar.
            let px = (d.start_px - dy).clamp(0.0, view.max_off);
            if let Ok(mut t) = row_touch_scroll().lock() {
                *t = Some((d.key, px));
            }
            return RowTouch::Dragging;
        }
        return RowTouch::None;
    }
    if !pressed && d.down {
        d.down = false;
        let was_drag = d.dragging;
        let key = d.key;
        // The PRESS position, not this sample's: the release frame carries no
        // finger, so C++ hands us (0,0) there.
        let (sx, sy) = (d.start_x, d.start_y);
        d.dragging = false;
        d.key = 0;
        drop(d);
        if !was_drag {
            return RowTouch::Tap(sx, sy);
        }
        let px = row_touch_scroll_read();
        if let Ok(mut t) = row_touch_scroll().lock() {
            *t = None;
        }
        let Some((k, px)) = px else { return RowTouch::None };
        if k != key || !live || k != view.key {
            return RowTouch::None;
        }
        // Land on a whole row: half a row showing at each edge is the state a
        // list should pass through, not the one it rests in.
        let max_row = (view.max_off / view.row_h).round().max(0.0) as usize;
        let offset = ((px / view.row_h).round().max(0.0) as usize).min(max_row);
        let vis = view.visible.max(1) as usize;
        let base = view.base as usize;
        let sel_lo = base + offset;
        let sel_hi = (sel_lo + vis - 1).min(base + view.total.saturating_sub(1) as usize);
        return RowTouch::Scrolled { offset, sel_lo, sel_hi };
    }
    RowTouch::None
}

/// Abandon any in-flight row drag (screen changed under the finger).
pub fn row_touch_cancel() {
    if let Ok(mut d) = row_drag().lock() {
        d.down = false;
        d.dragging = false;
        d.key = 0;
    }
    if let Ok(mut t) = row_touch_scroll().lock() {
        *t = None;
    }
}

/// The screen being drawn this frame, stamped by `library::render` from
/// `modal_kind`. Lets a shared row renderer tag its touch table without every
/// caller having to pass an id down.
static UI_SCREEN_KIND: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);

pub fn set_ui_screen_kind(kind: u32) {
    UI_SCREEN_KIND.store(kind, core::sync::atomic::Ordering::Relaxed);
}

fn ui_screen_kind() -> u32 {
    UI_SCREEN_KIND.load(core::sync::atomic::Ordering::Relaxed)
}

/// The same, for the X axis, so a GRID of choices can glide too — the language
/// picker moves sideways as often as it moves down. A second static rather than
/// a field on the first: the two are independent, and every existing caller is a
/// plain vertical list that must keep costing one lock.
fn list_hl_x() -> &'static std::sync::Mutex<ListHl> {
    static A: std::sync::Mutex<ListHl> = std::sync::Mutex::new(ListHl {
        inited: false,
        key: 0,
        last_tick: 0,
        y: 0.0,
    });
    &A
}

fn eased_list_x(target_x: f32, key: u32, now: u64) -> f32 {
    let mut a = match list_hl_x().lock() {
        Ok(g) => g,
        Err(_) => return target_x,
    };
    if !a.inited || a.key != key {
        a.inited = true;
        a.key = key;
        a.last_tick = now;
        a.y = target_x;
        return target_x;
    }
    let freq = unsafe { ruffle_tick_freq() } as f32;
    let dt = if freq > 0.0 {
        (now.saturating_sub(a.last_tick) as f32 / freq).min(0.1)
    } else {
        1.0 / 60.0
    };
    a.last_tick = now;
    a.y = ease_to(a.y, target_x, dt, 20.0);
    a.y
}

/// Advance + return the eased top-y of a list's selection highlight. Snaps to
/// `target_y` on the first frame or when `key` changes (different list).
fn eased_list_y(target_y: f32, key: u32, now: u64) -> f32 {
    let mut a = match list_hl().lock() {
        Ok(g) => g,
        Err(_) => return target_y,
    };
    if !a.inited || a.key != key {
        a.inited = true;
        a.key = key;
        a.last_tick = now;
        a.y = target_y;
        return target_y;
    }
    let freq = unsafe { ruffle_tick_freq() } as f32;
    let dt = if freq > 0.0 {
        (now.saturating_sub(a.last_tick) as f32 / freq).min(0.1)
    } else {
        1.0 / 60.0
    };
    a.last_tick = now;
    a.y = ease_to(a.y, target_y, dt, 20.0);
    a.y
}

/// Expand/collapse reveal for the IMPORTER drill-in (v1.2.0). Launching a saved
/// URL "opens" its row into the full-screen file list; closing collapses it back
/// to the row. Driven by a scissor window that grows from the row rect to the
/// full screen (expand) and shrinks back (collapse) — no scaling, just a clip
/// that opens. `source_sel` is the history row to grow from / shrink to.
#[derive(Clone, Copy)]
struct DistantReveal {
    active: bool,
    collapsing: bool,
    inited: bool,
    last_tick: u64,
    t: f32,
    source_sel: usize,
    /// Scroll offset the list had when the reveal started. The row's SCREEN
    /// position is `source_sel - source_scroll`, and since the list now scrolls
    /// freely that can't be re-derived from `source_sel` alone.
    source_scroll: usize,
}

fn distant_reveal() -> &'static std::sync::Mutex<DistantReveal> {
    static A: std::sync::Mutex<DistantReveal> = std::sync::Mutex::new(DistantReveal {
        active: false,
        collapsing: false,
        inited: false,
        last_tick: 0,
        t: 0.0,
        source_sel: 0,
        source_scroll: 0,
    });
    &A
}

/// Begin the expand reveal (row -> full screen) from history row `source_sel`,
/// with the list scrolled to `source_scroll`.
pub fn distant_reveal_begin_expand(source_sel: usize, source_scroll: usize) {
    if let Ok(mut a) = distant_reveal().lock() {
        a.active = true;
        a.collapsing = false;
        a.inited = false;
        a.t = 0.0;
        a.source_sel = source_sel;
        a.source_scroll = source_scroll;
    }
}

/// Begin the collapse reveal (full screen -> the row it grew from).
pub fn distant_reveal_begin_collapse() {
    if let Ok(mut a) = distant_reveal().lock() {
        a.active = true;
        a.collapsing = true;
        a.inited = false;
        a.t = 0.0;
    }
}

/// True while a reveal is running (input is suspended during it).
pub fn distant_reveal_active() -> bool {
    distant_reveal().lock().map(|a| a.active).unwrap_or(false)
}

/// Abandon a running reveal outright, without animating it to either end.
///
/// The reveal is only ever advanced by `distant_reveal_step`, which lives in
/// the `DistantLoading` / `DistantFiles` render arms. Any screen change that
/// leaves those two while the reveal is mid-flight therefore strands
/// `active = true` for the rest of the session — and since `library::input`
/// suspends ALL input while a reveal is active, the app renders perfectly and
/// answers no button ever again. That is what a failed archive.org fetch did:
/// `DistantLoading` -> `DistantError` with the expand still running, leaving a
/// permanently dead error screen (easiest repro: airplane mode, then open a
/// saved URL). Callers that jump straight to a full-screen notice call this so
/// the notice is dismissable.
pub fn distant_reveal_cancel() {
    if let Ok(mut a) = distant_reveal().lock() {
        a.active = false;
        a.collapsing = false;
        a.inited = false;
        a.t = 0.0;
    }
}

/// The history row the reveal grows from / shrinks to, and the scroll the list
/// was at — the underlay must redraw at that scroll for the box to line up.
pub fn distant_reveal_source_sel() -> usize {
    distant_reveal().lock().map(|a| a.source_sel).unwrap_or(0)
}

pub fn distant_reveal_source_scroll() -> usize {
    distant_reveal().lock().map(|a| a.source_scroll).unwrap_or(0)
}

/// Retarget the reveal's grow-from / shrink-to row. Used once a newly-imported
/// URL lands in the history, so the collapse (and the DistantIdle cursor) end on
/// its real row instead of the "+ add" row it grew from.
pub fn distant_reveal_set_source(idx: usize, scroll: usize) {
    if let Ok(mut a) = distant_reveal().lock() {
        a.source_sel = idx;
        a.source_scroll = scroll;
    }
}

/// Advance the reveal. Returns `(frac, collapsing, done)`:
///   - `frac`: 0..1 openness (already eased) — 0 = the row rect, 1 = full screen;
///     the caller lerps row->full by `frac`.
///   - `collapsing`: direction (for the caller's done handling).
///   - `done`: true on the frame the reveal finishes.
/// Returns None when idle.
pub fn distant_reveal_step(now: u64) -> Option<(f32, bool, bool)> {
    let mut a = match distant_reveal().lock() {
        Ok(g) => g,
        Err(_) => return None,
    };
    if !a.active {
        return None;
    }
    if !a.inited {
        a.inited = true;
        a.last_tick = now;
    }
    let freq = unsafe { ruffle_tick_freq() } as f32;
    let dt = if freq > 0.0 {
        (now.saturating_sub(a.last_tick) as f32 / freq).min(0.1)
    } else {
        1.0 / 60.0
    };
    a.last_tick = now;
    a.t = (a.t + dt * 6.0).min(1.0); // ~165 ms
    let collapsing = a.collapsing;
    if a.t >= 1.0 {
        a.active = false;
        return Some((if collapsing { 0.0 } else { 1.0 }, collapsing, true));
    }
    let e = 1.0 - (1.0 - a.t) * (1.0 - a.t); // ease-out openness
    let frac = if collapsing { 1.0 - e } else { e };
    Some((frac, collapsing, false))
}

/// Game launch/quit reveal (v1.2.0): the chosen game's cover "opens" from its
/// gallery tile to full screen on launch (then the SWF loads behind that frozen
/// full-screen frame = a free loading screen), and "closes" back to the tile on
/// quit. Same window-reveal as the IMPORTER drill-in, but the content is the
/// full-screen cover and the box is the selected tile. Holds the game identity
/// so the cover can be resolved from either render phase.
struct GameReveal {
    active: bool,
    collapsing: bool,
    inited: bool,
    last_tick: u64,
    t: f32,
    rx: f32,
    ry: f32,
    rw: f32,
    rh: f32,
    basename: std::string::String,
    display_name: std::string::String,
    color_chip: u32,
}

fn game_reveal() -> &'static std::sync::Mutex<GameReveal> {
    static A: std::sync::Mutex<GameReveal> = std::sync::Mutex::new(GameReveal {
        active: false,
        collapsing: false,
        inited: false,
        last_tick: 0,
        t: 0.0,
        rx: 0.0,
        ry: 0.0,
        rw: 0.0,
        rh: 0.0,
        basename: std::string::String::new(),
        display_name: std::string::String::new(),
        color_chip: 0,
    });
    &A
}

/// Begin a game reveal. `collapsing` = quit (full screen -> tile); otherwise
/// launch (tile -> full screen). `rect` is the gallery tile box; the rest is the
/// game identity used to draw the full-screen cover.
pub fn game_reveal_begin(
    collapsing: bool,
    rect: (f32, f32, f32, f32),
    basename: &str,
    display_name: &str,
    color_chip: u32,
) {
    if let Ok(mut a) = game_reveal().lock() {
        a.active = true;
        a.collapsing = collapsing;
        a.inited = false;
        a.t = 0.0;
        a.rx = rect.0;
        a.ry = rect.1;
        a.rw = rect.2;
        a.rh = rect.3;
        a.basename = basename.to_string();
        a.display_name = display_name.to_string();
        a.color_chip = color_chip;
    }
}

/// Abandon a launch/quit reveal in flight.
///
/// The animation is stepped by `game_reveal_step`, which only runs from the
/// `List` and `Launching` arms of `library::render`. Anything that navigates
/// away from those mid-reveal leaves it armed with nobody to finish it, and
/// `game_reveal_active()` then answers true for ever -- which `input()` reads as
/// "suspend every button". The launcher keeps drawing and stops responding.
///
/// The same net `distant_reveal_cancel` provides for the IMPORTER reveal.
pub fn game_reveal_cancel() {
    if let Ok(mut a) = game_reveal().lock() {
        a.active = false;
    }
}

/// True while a game reveal is running (input suspended; library loop kept alive).
pub fn game_reveal_active() -> bool {
    game_reveal().lock().map(|a| a.active).unwrap_or(false)
}

/// The reveal's tile rect + game identity `(rect, basename, display_name, color)`.
pub fn game_reveal_info() -> ((f32, f32, f32, f32), std::string::String, std::string::String, u32) {
    game_reveal()
        .lock()
        .map(|a| {
            (
                (a.rx, a.ry, a.rw, a.rh),
                a.basename.clone(),
                a.display_name.clone(),
                a.color_chip,
            )
        })
        .unwrap_or_default()
}

/// Advance the game reveal. Returns `(frac, fade, collapsing, done)`:
///   - `frac`: 0 = tile rect, 1 = full screen.
///   - `fade`: 0..1 black overlay alpha — the LAUNCH adds a fade-to-black phase
///     after the cover reaches full screen, so the game can pop calmly out of the
///     dark instead of replacing the (often wrong-aspect) cover in one frame.
///   - `collapsing` / `done` as before. None when idle.
/// Launch runs `t` over [0,2] (expand [0,1] then fade [1,2]); collapse over [0,1].
pub fn game_reveal_step(now: u64) -> Option<(f32, f32, bool, bool)> {
    let mut a = match game_reveal().lock() {
        Ok(g) => g,
        Err(_) => return None,
    };
    if !a.active {
        return None;
    }
    if !a.inited {
        a.inited = true;
        a.last_tick = now;
    }
    let freq = unsafe { ruffle_tick_freq() } as f32;
    let dt = if freq > 0.0 {
        (now.saturating_sub(a.last_tick) as f32 / freq).min(0.1)
    } else {
        1.0 / 60.0
    };
    a.last_tick = now;
    let collapsing = a.collapsing;
    let max_t = if collapsing { 1.0 } else { 2.0 };
    a.t = (a.t + dt * 5.5).min(max_t); // ~180 ms per phase
    if a.t >= max_t {
        a.active = false;
        // collapse done -> fully closed; launch done -> full screen + full black.
        let frac = if collapsing { 0.0 } else { 1.0 };
        let fade = if collapsing { 0.0 } else { 1.0 };
        return Some((frac, fade, collapsing, true));
    }
    if collapsing {
        let e = 1.0 - (1.0 - a.t) * (1.0 - a.t); // ease-out
        Some((1.0 - e, 0.0, true, false))
    } else if a.t <= 1.0 {
        let e = 1.0 - (1.0 - a.t) * (1.0 - a.t); // ease-out openness
        Some((e, 0.0, false, false))
    } else {
        // Fade phase: full screen, cover darkening to black.
        Some((1.0, a.t - 1.0, false, false))
    }
}

/// Cover-picker thumbnail state, keyed by the candidate's logo URL. Loaded
/// progressively (one per frame) so opening the picker never freezes.
#[derive(Clone, Copy)]
enum ThumbTex {
    Image { tex: GLuint, w: u32, h: u32 },
    Failed,
}

fn thumb_cache() -> &'static std::sync::Mutex<std::vec::Vec<(std::string::String, ThumbTex)>> {
    static C: std::sync::Mutex<std::vec::Vec<(std::string::String, ThumbTex)>> =
        std::sync::Mutex::new(std::vec::Vec::new());
    &C
}

fn thumb_lookup(url: &str) -> Option<ThumbTex> {
    thumb_cache()
        .lock()
        .ok()
        .and_then(|c| c.iter().find(|(u, _)| u == url).map(|(_, t)| *t))
}

/// URL of the thumbnail currently being fetched ASYNC (at most one at a time),
/// or None when idle. The gallery render starts the next uncached logo when
/// idle and `pump_thumbnail_load` finishes it — so the render thread NEVER
/// blocks on a logo download (some Flashpoint logos are hundreds of KB).
fn thumb_inflight() -> &'static std::sync::Mutex<std::vec::Vec<(i32, std::string::String)>> {
    static C: std::sync::Mutex<std::vec::Vec<(i32, std::string::String)>> =
        std::sync::Mutex::new(std::vec::Vec::new());
    &C
}

/// Cancel any in-flight thumbnail fetch and clear the in-flight marker. Called
/// when leaving a thumbnail screen (FpGallery / cover picker) or starting a new
/// search, so the isolated curl handle is never left wedged.
pub fn thumb_cancel_all() {
    crate::net::thumb_cancel();
    if let Ok(mut g) = thumb_inflight().lock() {
        g.clear();
    }
}

// ─── Backend implementation ───────────────────────────────────────────────────

/// True when `sdmc:/switch/FlashNX/<name>` exists. Same convention as the
/// C++ side's `trace.on` / `dumpvars.on` / `noalloc.on`: an experiment is a
/// file you drop on the card, never a default and never a menu entry.
pub(crate) fn marker_present(name: &str) -> bool {
    std::path::Path::new(&std::format!("sdmc:/switch/FlashNX/{}", name)).exists()
}

impl SwitchRenderBackend {
    /// Full backend for a game: the mega-arenas are sized for the worst SWF we
    /// know of (see the BufferArena block at the top of this file), or from
    /// what this game recorded in `<game_path>.arena` (see `ARENA_RECORD_*`).
    pub fn new(width: u32, height: u32, game_path: Option<&str>) -> Option<Self> {
        let record_path = game_path.map(|p| std::format!("{}.arena", p));
        let record = record_path.as_deref().and_then(read_arena_record);
        // `sdmc:/switch/FlashNX/arena.small` cuts the reservation by a third,
        // as an experiment only — never as a default.
        //
        // The 576 MB reserved here is the single largest block of memory
        // FlashNX holds, and Super Smash Flash 2 dies of exhaustion with only
        // ~98 MB of margin while using 36 MB of it. Across 70 captured logs the
        // worst arena use ever observed is 144 + 64 MB, and `arenaDropV` has
        // never been non-zero since the mega-arena landed.
        //
        // It is NOT a free win, which is why it is behind a marker: 192 MB of
        // VBO already failed once, on Infiltrating the Airship (#56) at ~10 000
        // shapes, and the failure mode is a silent white screen rather than a
        // crash. 256 MB clears that by only a third. Sweep Infiltrating the
        // Airship, Binding of Isaac and Henry Stickmin, watching `arena_v=`
        // peak and `arenaDropV`, before this is ever made the default.
        let (vbo, ibo) = if marker_present("arena.small") {
            log(b"arena: arena.small present -> 256/128 MB instead of 384/192\n\0");
            (256 * 1024 * 1024, 128 * 1024 * 1024)
        } else {
            let (vbo, ibo) = arena_sizes_for(record);
            if let Some((v, i, full)) = record {
                log_str(&std::format!(
                    "arena: record v={}MB i={}MB full={} -> {}/{} MB\n",
                    v,
                    i,
                    full as u8,
                    vbo / (1024 * 1024),
                    ibo / (1024 * 1024)
                ));
            }
            (vbo, ibo)
        };
        let mut backend = Self::new_sized(width, height, vbo, ibo)?;
        backend.arena_record_path = record_path;
        backend.arena_record_written = record.unwrap_or((0, 0, false));
        Some(backend)
    }

    /// `atlasIdle:` how many live atlases (and MB) no draw has sampled for 30 s
    /// and for 2 min, and how many were never drawn at all. Atlases fill up in
    /// the order images are first decoded, so a game that shows its menus, then
    /// a fight, leaves the menu images in atlases the fight never touches: this
    /// is the most an eviction of unused images could give back.
    fn log_atlas_idle(&self) {
        let now = self.frame_count;
        let (mut live, mut live_mb) = (0u32, 0u64);
        let (mut idle30, mut idle30_mb, mut idle120, mut idle120_mb) = (0u32, 0u64, 0u32, 0u64);
        let (mut never, mut never_mb) = (0u32, 0u64);
        for a in &self.atlases {
            if a.texture == 0 {
                continue;
            }
            let mb = a.width as u64 * a.height as u64 * 4 / (1024 * 1024);
            live += 1;
            live_mb += mb;
            let last = a.last_draw_frame.get();
            if last == 0 {
                never += 1;
                never_mb += mb;
                continue;
            }
            let age = now.saturating_sub(last);
            if age >= 1800 {
                idle30 += 1;
                idle30_mb += mb;
            }
            if age >= 7200 {
                idle120 += 1;
                idle120_mb += mb;
            }
        }
        log_str(&std::format!(
            "atlasIdle: f{} live={} ({}MB) idle30s={} ({}MB) idle2min={} ({}MB) neverDrawn={} ({}MB)\n",
            now, live, live_mb, idle30, idle30_mb, idle120, idle120_mb, never, never_mb
        ));
    }

    /// Pack an evictable image into a reloadable atlas and record where it
    /// went. False when there is no room (the caller falls back).
    fn pack_reloadable(
        &mut self,
        pixels: &[u8],
        width: u32,
        height: u32,
        slot: &std::rc::Rc<ReloadSlot>,
    ) -> bool {
        let Some(meta) = self.pack_into_atlas(pixels, width, height, true) else {
            return false;
        };
        slot.place.set(Some(AtlasPlace {
            atlas_index: meta.atlas_index,
            u0: meta.u0,
            v0: meta.v0,
            u1: meta.u1,
            v1: meta.v1,
        }));
        *slot.ticket.borrow_mut() = meta.ticket.clone();
        if let Some(a) = self.atlases.get_mut(meta.atlas_index) {
            a.members.push(SlotLink(std::rc::Rc::downgrade(slot)));
        }
        true
    }

    /// Where an evictable image is, decoding it again first if it was evicted.
    /// None when that fails (the draw is skipped; retried a second later).
    fn ensure_resident(&mut self, slot: &std::rc::Rc<ReloadSlot>) -> Option<AtlasPlace> {
        let now = self.frame_count.max(1);
        slot.last_draw.set(now);
        if let Some(p) = slot.place.get() {
            return Some(p);
        }
        let failed = slot.failed_at.get();
        if failed != 0 && now.wrapping_sub(failed) < 60 {
            return None;
        }
        let t0 = unsafe { ruffle_tick_now() };
        let decoded = (slot.reload)().and_then(|b| bitmap_to_rgba_bytes(&b));
        let ok = match decoded {
            Some((bytes, w, h)) if w == slot.width && h == slot.height => {
                self.pack_reloadable(&bytes, w, h, slot)
            }
            _ => false,
        };
        self.evict_reload_ticks = self
            .evict_reload_ticks
            .wrapping_add(unsafe { ruffle_tick_now() }.wrapping_sub(t0));
        // The upload bound (and unbound) a texture behind the cache's back,
        // possibly mid-draw: the next draw must bind for real.
        self.gl_state.invalidate();
        if !ok {
            slot.failed_at.set(now);
            self.evict_reload_fail = self.evict_reload_fail.wrapping_add(1);
            return None;
        }
        let bytes = slot.width as u64 * slot.height as u64 * 4;
        ruffle_core::flashnx_bitmap_residency(bytes, true);
        self.evict_reloads = self.evict_reloads.wrapping_add(1);
        self.evict_reload_bytes = self.evict_reload_bytes.wrapping_add(bytes);
        slot.place.get()
    }

    /// Empty the reloadable atlases nothing has drawn from for a while, once
    /// they hold more than `EVICT_START_BYTES`. An atlas is only emptied whole
    /// (that is what frees it): every image in it must be idle, give or take
    /// `EVICT_MAX_HOT` that are decoded again on their next draw. Runs right
    /// before the release drain, which then frees the emptied atlases.
    fn evict_idle_images(&mut self) {
        if !self.evict_on {
            return;
        }
        let now = self.frame_count;
        let atlas_bytes = |a: &Atlas| a.width as u64 * a.height as u64 * 4;
        let mut total: u64 = self
            .atlases
            .iter()
            .filter(|a| a.texture != 0 && a.reloadable)
            .map(atlas_bytes)
            .sum();
        if total <= EVICT_START_BYTES {
            return;
        }
        let mut order: std::vec::Vec<(u32, usize)> = self
            .atlases
            .iter()
            .enumerate()
            .filter(|(_, a)| a.texture != 0 && a.reloadable)
            .map(|(i, a)| (a.last_draw_frame.get(), i))
            .collect();
        order.sort_unstable();
        let mut emptied = 0;
        // What this pass may still send back to the decoder.
        let (mut hot_left, mut hot_bytes_left) = (EVICT_MAX_HOT, EVICT_MAX_HOT_BYTES);
        for (_, idx) in order {
            if emptied >= EVICT_MAX_PER_PASS || total <= EVICT_START_BYTES {
                break;
            }
            let atlas = &self.atlases[idx];
            let members: std::vec::Vec<std::rc::Rc<ReloadSlot>> = atlas
                .members
                .iter()
                .filter_map(|w| w.0.upgrade())
                .filter(|m| m.place.get().is_some_and(|p| p.atlas_index == idx))
                .collect();
            // Something else still holds a place here (a release not drained
            // yet): emptying would not free it. Next pass.
            if members.is_empty() || atlas.live as usize != members.len() {
                continue;
            }
            let (mut hot, mut hot_bytes) = (0usize, 0u64);
            for m in &members {
                if now.saturating_sub(m.last_draw.get()) < EVICT_IDLE_FRAMES {
                    hot += 1;
                    hot_bytes += m.width as u64 * m.height as u64 * 4;
                }
            }
            if hot > hot_left || hot_bytes > hot_bytes_left {
                continue;
            }
            hot_left -= hot;
            hot_bytes_left -= hot_bytes;
            let freed = atlas_bytes(atlas);
            for m in &members {
                m.place.set(None);
                // Dropping the ticket queues the release the drain applies.
                m.ticket.borrow_mut().take();
                ruffle_core::flashnx_bitmap_residency(
                    m.width as u64 * m.height as u64 * 4,
                    false,
                );
            }
            self.evict_images = self.evict_images.wrapping_add(members.len() as u32);
            self.atlases[idx].members.clear();
            self.evict_atlases = self.evict_atlases.wrapping_add(1);
            self.evict_bytes = self.evict_bytes.wrapping_add(freed);
            total = total.saturating_sub(freed);
            emptied += 1;
        }
    }

    /// `evict:` line, every 300 frames, then reset.
    fn log_evict(&mut self) {
        if !self.evict_on {
            return;
        }
        let (mut n, mut mb) = (0u32, 0u64);
        for a in &self.atlases {
            if a.texture != 0 && a.reloadable {
                n += 1;
                mb += a.width as u64 * a.height as u64 * 4 / (1024 * 1024);
            }
        }
        let ms = self.evict_reload_ticks as f64 * 1000.0 / unsafe { ruffle_tick_freq() }.max(1) as f64;
        log_str(&std::format!(
            "evict: f{} reloadable atlases {} ({}MB), other atlases {}; last 300 frames: emptied {} atlases ({}MB, {} images), decoded again {} ({}MB, {:.0} ms), failed {}, released slots reused {}, atlases made {}\n",
            self.frame_count, n, mb,
            self.atlases.iter().filter(|a| a.texture != 0 && !a.reloadable).count(),
            self.evict_atlases, self.evict_bytes / (1024 * 1024), self.evict_images,
            self.evict_reloads, self.evict_reload_bytes / (1024 * 1024), ms,
            self.evict_reload_fail,
            SLOTS_REUSED.swap(0, Ordering::Relaxed),
            self.atlas_created,
        ));
        self.atlas_created = 0;
        self.evict_atlases = 0;
        self.evict_images = 0;
        self.evict_bytes = 0;
        self.evict_reloads = 0;
        self.evict_reload_bytes = 0;
        self.evict_reload_ticks = 0;
        self.evict_reload_fail = 0;
    }

    /// Keep `<game>.swf.arena` up to date: called every heartbeat, writes only
    /// on news (an overflow, or a peak at least 4 MB past the file), so a
    /// session that crashes later still leaves its peak behind.
    fn arena_record_update(&mut self, v_peak_mb: u64, i_peak_mb: u64) {
        let Some(path) = self.arena_record_path.as_deref() else {
            return;
        };
        let (wv, wi, wfull) = self.arena_record_written;
        let full = wfull
            || self.vertex_arena.alloc_failures > 0
            || self.index_arena.alloc_failures > 0;
        let (v, i) = (v_peak_mb.max(wv), i_peak_mb.max(wi));
        if full == wfull && v < wv + 4 && i < wi + 4 {
            return;
        }
        write_arena_record(path, v, i, full);
        log_str(&std::format!(
            "arena: recorded v={}MB i={}MB full={} in {}\n",
            v, i, full as u8, path
        ));
        self.arena_record_written = (v, i, full);
    }

    /// Backend for the LAUNCHER UI. Identical except for the arenas: the library
    /// draws rects, text and textured quads, all through the static quad VAOs —
    /// it never registers a Ruffle shape, so the full 576 MB of arena it used to
    /// allocate was ~230 ms of boot latency for buffers nothing ever wrote to.
    /// The small arenas keep the shape path functional (and its OOM logging
    /// honest) rather than removing it.
    pub fn new_ui(width: u32, height: u32) -> Option<Self> {
        Self::new_sized(width, height, 4 * 1024 * 1024, 2 * 1024 * 1024)
    }

    fn new_sized(
        width: u32,
        height: u32,
        arena_vbo_size: GLsizeiptr,
        arena_ibo_size: GLsizeiptr,
    ) -> Option<Self> {
        // Reset cross-instance statics so the diagnostic counters and the
        // pending-frees queue match THIS backend, not whatever the previous
        // one left behind. Without this clear, restarting the Player (e.g.
        // pause-menu REDEMARRER) makes:
        //   - LIVE_GPU_DRAWS/SHAPES briefly noisy if old Drops race with
        //     new register_shape calls (they don't in practice — drops are
        //     synchronous on Arc=0 — but defensive cost is one atomic store).
        //   - PENDING_FREES the actual bug: stale (offset, size) tuples from
        //     the old arena get applied to the NEW fresh arena's freelist
        //     on first submit_frame drain, marking already-free regions as
        //     "double-free" and producing the `arena_v=-2MB(frag 18)`
        //     nonsense in heartbeat logs. Worse, the bogus free regions
        //     would alias with future allocs and silently corrupt draws.
        PENDING_FREES.lock().unwrap().clear();
        // Same bug, other queue. Ruffle's Player drops its renderer BEFORE its GC
        // arena (player.rs: `renderer` is declared ahead of `gc_arena`), so every
        // atlas bitmap of the game being left releases its ticket after that
        // backend is gone. Nothing drains the queue until the NEXT game's first
        // submit_frame, which then decrements ITS atlases at the stale indices
        // and can free one that live handles still point at: sprites vanishing
        // or showing another bitmap in the second game of a session, or after
        // REDEMARRER. The old atlases died with the old backend; the indices
        // mean nothing now.
        if let Ok(mut q) = PENDING_ATLAS_RELEASE.lock() {
            q.clear();
        }
        // A new backend means a new working set (a game starting, REDEMARRER,
        // back to the library): the blocks the last one parked in libdrm's BO
        // cache would only hold heap the next one may need.
        unsafe { flashnx_boc_trim() };
        // `sdmc:/switch/FlashNX/bocache.off` turns that cache off, to compare a
        // game with and without it on the same binary. An experiment, never a
        // default (see `marker_present`).
        if marker_present("bocache.off") {
            unsafe { flashnx_boc_set(0) };
            log(b"bocache: bocache.off present -> libdrm buffer-object cache DISABLED\n\0");
        }
        LIVE_GPU_DRAWS.store(0, Ordering::Relaxed);
        LIVE_GPU_SHAPES.store(0, Ordering::Relaxed);

        // Boot-cost instrumentation: shader compiles and the mega-arena
        // allocation both run before the first frame can be drawn, so each one
        // reports its own share of the launch wait.
        let t_prog = unsafe { ruffle_tick_now() };
        let solid = build_solid_program()?;
        let bitmap_prog = build_bitmap_program()?;
        let shape_bitmap_prog = build_shape_bitmap_program()?;
        let gradient_prog = build_gradient_program()?;
        let color_matrix_filter = build_color_matrix_filter_program()?;
        let unpremult_blit = build_unpremult_blit_program()?;
        let premult_blit = build_premult_blit_program()?;
        let blur_filter = build_blur_filter_program()?;
        let glow_filter = build_glow_filter_program()?;
        let bevel_filter = build_bevel_filter_program()?;
        let displacement_filter = build_displacement_map_filter_program()?;
        let alpha_mask_prog = build_alpha_mask_program()?;
        let complex_blend_prog = build_complex_blend_program()?;

        let (rect_vao, rect_vbo) = build_solid_quad();
        let (bitmap_vao, bitmap_vbo) = build_bitmap_quad();
        let (atlas_vao, atlas_vbo) = build_atlas_batch();
        let (line_vao, line_vbo) = build_line_segment();
        let (line_rect_vao, line_rect_vbo) = build_line_rect();
        // One sampler object for point sampling, bound per draw (see
        // `sampler_for`). It has to be a sampler and not texture parameters,
        // because the bitmaps that want it share a 2048 atlas with bitmaps that
        // do not: one texture object, hundreds of bitmaps, nowhere to put a
        // per-bitmap filter. glTexParameteri around each draw was the other
        // option and loses twice — it mutates the SHARED atlas, and it is the
        // per-frame texparam churn that bisection on 2026-05-24 implicated in a
        // Mario 63 driver-side issue (see `use_shape_bitmap`).
        //
        // A sampler supersedes the texture's WRAP as well as its filter, and its
        // own defaults are GL_REPEAT + GL_NEAREST_MIPMAP_LINEAR while no texture
        // here has a mipmap chain. Both wrap modes and a non-mipmap MIN filter
        // are restated for that reason, not for tidiness.
        let mut sampler_sharp: GLuint = 0;
        unsafe {
            glGenSamplers(1, &mut sampler_sharp);
            if sampler_sharp != 0 {
                glSamplerParameteri(sampler_sharp, GL_TEXTURE_MIN_FILTER, GL_NEAREST as GLint);
                glSamplerParameteri(sampler_sharp, GL_TEXTURE_MAG_FILTER, GL_NEAREST as GLint);
                glSamplerParameteri(sampler_sharp, GL_TEXTURE_WRAP_S, GL_CLAMP_TO_EDGE as GLint);
                glSamplerParameteri(sampler_sharp, GL_TEXTURE_WRAP_T, GL_CLAMP_TO_EDGE as GLint);
            }
        }
        let t_arena = unsafe { ruffle_tick_now() };
        // What the arenas cost in MALLOC, which is the memory that runs out (a
        // 3.7 MB allocation failed at heap=1068 MB while the reserved-heap
        // counter still read 3185 MB). The buffers are GL objects, but if Mesa
        // shadows them on the CPU they are the biggest single line in our
        // baseline — worth knowing before anyone tunes their size again.
        let heap_before_arenas = unsafe { ruffle_heap_used() };

        // Mega-buffer arena for all shape draws — see the BufferArena
        // comment block at the top of this file for the rationale.
        let vertex_arena = BufferArena::new(
            GL_ARRAY_BUFFER,
            arena_vbo_size,
            ARENA_VBO_ALIGN,
        );
        let index_arena = BufferArena::new(
            GL_ELEMENT_ARRAY_BUFFER,
            arena_ibo_size,
            ARENA_IBO_ALIGN,
        );
        let shape_vao = build_shape_arena_vao(vertex_arena.gl_id, index_arena.gl_id);
        {
            let freq = unsafe { ruffle_tick_freq() } as f64;
            let t_end = unsafe { ruffle_tick_now() };
            let ms_prog = (t_arena.saturating_sub(t_prog) as f64) * 1000.0 / freq;
            let ms_arena = (t_end.saturating_sub(t_arena) as f64) * 1000.0 / freq;
            let heap_after = unsafe { ruffle_heap_used() };
            // Same probe as boot, now that EGL, the shaders, the atlases and the
            // arenas have all taken their share. Boot said 3136 MB; whatever this
            // says is what a game actually has left to play with.
            let mut biggest_now: u64 = 0;
            let ceiling_now = unsafe {
                ruffle_probe_heap_ceiling(32 * 1024 * 1024, &mut biggest_now as *mut u64)
            };
            {
                let mut c = std::format!(
                    "boot: malloc ceiling AFTER renderer {} MB total, biggest single {} MB\n\0",
                    ceiling_now / (1024 * 1024),
                    biggest_now / (1024 * 1024),
                );
                unsafe { ruffle_log_cstr(c.as_mut_ptr() as *const _) };
            }
            let mut m = std::format!(
                "boot: renderer shaders {:.0} ms | arenas ({} MB) {:.0} ms | \
                 heap {} -> {} MB (arenas cost {} MB of malloc)\n\0",
                ms_prog,
                (arena_vbo_size + arena_ibo_size) / (1024 * 1024),
                ms_arena,
                heap_before_arenas / (1024 * 1024),
                heap_after / (1024 * 1024),
                heap_after.saturating_sub(heap_before_arenas) / (1024 * 1024),
            );
            unsafe { ruffle_log_cstr(m.as_mut_ptr() as *const _) };
        }

        // The texture samplers `u_tex` in every program are always bound to
        // texture unit 0. Set them once at link time so we don't have to
        // `glUniform1i(u_tex, 0)` on every draw. Mesa caches sampler bindings
        // per-program across glUseProgram switches, so this is permanent.
        unsafe {
            glUseProgram(bitmap_prog.program);
            glUniform1i(bitmap_prog.u_tex, 0);
            glUseProgram(shape_bitmap_prog.program);
            glUniform1i(shape_bitmap_prog.u_tex, 0);
            glUseProgram(gradient_prog.program);
            glUniform1i(gradient_prog.u_tex, 0);
            // Filter programs sample at unit 0; glow additionally samples a
            // pre-blurred source at unit 1.
            glUseProgram(color_matrix_filter.program);
            glUniform1i(loc(color_matrix_filter.program, b"u_tex\0"), 0);
            glUseProgram(unpremult_blit.program);
            glUniform1i(loc(unpremult_blit.program, b"u_tex\0"), 0);
            glUseProgram(premult_blit.program);
            glUniform1i(loc(premult_blit.program, b"u_tex\0"), 0);
            glUseProgram(blur_filter.program);
            glUniform1i(loc(blur_filter.program, b"u_tex\0"), 0);
            glUseProgram(glow_filter.program);
            glUniform1i(loc(glow_filter.program, b"u_tex\0"), 0);
            glUniform1i(loc(glow_filter.program, b"u_blur_tex\0"), 1);
            glUseProgram(bevel_filter.program);
            glUniform1i(loc(bevel_filter.program, b"u_tex\0"), 0);
            glUniform1i(loc(bevel_filter.program, b"u_blur_tex\0"), 1);
            // DisplacementMap: source at unit 0, displacement map at unit 1.
            glUseProgram(displacement_filter.program);
            glUniform1i(loc(displacement_filter.program, b"u_tex\0"), 0);
            glUniform1i(loc(displacement_filter.program, b"u_map_tex\0"), 1);
            // Two-texture composites: backdrop/maskee at unit 0, mask/current
            // at unit 1.
            glUseProgram(alpha_mask_prog.program);
            glUniform1i(loc(alpha_mask_prog.program, b"u_tex\0"), 0);
            glUniform1i(loc(alpha_mask_prog.program, b"u_mask_tex\0"), 1);
            glUseProgram(complex_blend_prog.program);
            glUniform1i(loc(complex_blend_prog.program, b"u_tex\0"), 0);
            glUniform1i(loc(complex_blend_prog.program, b"u_current_tex\0"), 1);
            glUseProgram(0);
        }

        Some(Self {
            dimensions: ViewportDimensions {
                width,
                height,
                scale_factor: 1.0,
            },
            tessellator: ShapeTessellator::new(),
            solid,
            bitmap_prog,
            shape_bitmap_prog,
            gradient_prog,
            color_matrix_filter,
            screen_filter: None,
            screen_filter_fbo: 0,
            screen_filter_tex: 0,
            screen_filter_rbo: 0,
            screen_filter_dims: (0, 0),
            screen_filter_prev_fbo: 0,
            unpremult_blit,
            premult_blit,
            blur_filter,
            glow_filter,
            bevel_filter,
            displacement_filter,
            alpha_mask_prog,
            complex_blend_prog,
            blend_window: 0,
            filter_tex_pool: FilterTexturePool::new(),
            // Counted from here, not from the process start.
            boc_prev: boc_stats_now(),
            boc_frame: [0; BOC_STATS],
            offscreen_temp_pool: Vec::new(),
            offscreen_temp_retired: Vec::new(),
            offscreen_temp_pool_bytes: 0,
            gl_state: GlStateCache::default(),
            sampler_sharp,
            rect_vao,
            rect_vbo,
            bitmap_vao,
            bitmap_vbo,
            atlas_vao,
            atlas_vbo,
            line_vao,
            line_vbo,
            line_rect_vao,
            line_rect_vbo,
            mask: MaskState::default(),
            warned_unsupported: 0,
            frame_count: 0,
            draw_extent: None,
            draw_max_alpha: 0.0,
            pending_upload: None,
            upload_scratch: std::vec::Vec::new(),
            shapes_registered: 0,
            bitmaps_registered: 0,
            bitmap_draws_emitted: 0,
            big_atlas_live_bytes: 0,
            big_atlas_peak_bytes: 0,
            big_atlas_alloc_total: 0,
            big_atlas_free_total: 0,
            big_atlas_dropped_total: 0,
            atlas_alloc_failures: 0,
            atlas_created: 0,
            evict_on: !marker_present("evict.off"),
            evict_atlases: 0,
            evict_images: 0,
            evict_bytes: 0,
            evict_reloads: 0,
            evict_reload_bytes: 0,
            evict_reload_ticks: 0,
            evict_reload_fail: 0,
            heartbeat_tick: 0,
            draw_calls_this_window: 0,
            push_mask_window: 0,
            alpha_mask_window: 0,
            masked_draw_window: 0,
            mask_shape_draw_window: 0,
            cache_entries_max_window: 0,
            filter_budget_this_frame: 0,
            render_offscreen_calls: 0,
            apply_filter_calls: 0,
            resolve_sync_calls: 0,
            filters_seen_mask: AtomicU16::new(0),
            bitmap_render_count: 0,
            atlases: Vec::new(),
            vertex_arena,
            index_arena,
            arena_record_path: None,
            arena_record_written: (0, 0, false),
            shape_vao,
            offscreen_dims: None,
            offscreen_target_tex: None,
            ui_translate_x: 0.0,
            ui_translate_y: 0.0,
            ui_scale: 1.0,
            ui_pivot_x: 0.0,
            ui_pivot_y: 0.0,
            offscreen_fbo: 0,
            offscreen_depth_stencil: 0,
            offscreen_depth_stencil_dims: (0, 0),
            filter_fbo: 0,
            frame_snapshot: FrameBreakdown::default(),
            last_frame: FrameBreakdown::default(),
            font_atlas: None,
            atlas_init_done: false,
            kit_text: None,
            kit_text_tried: false,
            kit_shapes: None,
            kit_shapes_tried: false,
            game_layer: false,
        })
    }

    /// Build the 3x3 column-major matrix that combines (Flash 2x3 affine)
    /// with (pixels → NDC). Sent as the `u_world` uniform.
    ///
    /// Main framebuffer: target = viewport, Y flipped (Flash top → NDC y=+1).
    /// Offscreen FBO (`offscreen_dims`): target = FBO size, NO Y flip so that
    /// Flash top maps to texel y=0 of the result — matching the convention of
    /// CPU-uploaded bitmaps (glTexImage2D row 0 = top = texel y=0), so a later
    /// `render_bitmap` of this texture samples it the same way as any bitmap.
    /// Commands are pre-shifted by Ruffle to target-local coords, so no origin
    /// offset is applied here.
    /// Where the frame's draws actually land, in viewport pixels, accumulated
    /// across a heartbeat window.
    ///
    /// A game can draw hundreds of objects and show nothing, and the two reasons
    /// need opposite fixes: the content sits outside the viewport (a transform
    /// or a stage-size problem), or it sits in the right place and is invisible
    /// (alpha, colour transform, a texture that never bound). The counters said
    /// "886 draws" for a blank screen on Peggle (#100) without separating those.
    fn note_draw_extent(&mut self, m: &Matrix) {
        let x = m.tx.to_pixels() as f32;
        let y = m.ty.to_pixels() as f32;
        // Where the draw is ANCHORED, plus its transformed unit vector. For a
        // bitmap that is the whole quad; for a shape it is the origin, since the
        // geometry's own extent lives in the vertex buffer. Enough either way to
        // answer the only question here: is this on the screen or not.
        let x2 = x + m.a + m.c;
        let y2 = y + m.b + m.d;
        let (lo_x, hi_x) = if x <= x2 { (x, x2) } else { (x2, x) };
        let (lo_y, hi_y) = if y <= y2 { (y, y2) } else { (y2, y) };
        self.draw_extent = match self.draw_extent {
            None => Some((lo_x, lo_y, hi_x, hi_y)),
            Some((a, b, c, d)) => Some((a.min(lo_x), b.min(lo_y), c.max(hi_x), d.max(hi_y))),
        };
    }

    fn world_matrix(&self, m: &Matrix) -> [GLfloat; 9] {
        let (w, h, flip_y) = match self.offscreen_dims {
            Some((ow, oh)) => (ow.max(1) as f32, oh.max(1) as f32, false),
            None => (
                self.dimensions.width.max(1) as f32,
                self.dimensions.height.max(1) as f32,
                true,
            ),
        };
        // LIBRARY-UI transform: a uniform scale about a pivot (the modal/tab/
        // editor open-close pop), with `ui_translate_*` kept for the dim-backdrop
        // exemption. Folded in so every draw honours it. Identity (scale 1, pivot
        // 0, translate 0) in-game / offscreen, so this is a no-op there.
        let s = self.ui_scale;
        let a = m.a * s;
        let b = m.b * s;
        let c = m.c * s;
        let d = m.d * s;
        let tx = m.tx.to_pixels() as f32 * s + self.ui_pivot_x * (1.0 - s) + self.ui_translate_x;
        let ty = m.ty.to_pixels() as f32 * s + self.ui_pivot_y * (1.0 - s) + self.ui_translate_y;
        // Quarter-turn, composed into the affine rather than applied afterwards.
        //
        // `w`/`h` above are the LOGICAL viewport, which is portrait while the
        // picture is turned; the framebuffer stays landscape whatever happens, so
        // the NDC divisor has to be the physical one. Composing here means every
        // draw is turned by construction -- there is no second path to forget.
        let rot = if flip_y { game_rotation() } else { 0 };
        let (pw, ph) = if matches!(rot, 1 | 3) { (h, w) } else { (w, h) };
        let (a, b, c, d, tx, ty) = match rot {
            // Clockwise: the logical top-left corner lands top-right.
            1 => (-b, a, -d, c, pw - ty, tx),
            2 => (-a, -b, -c, -d, pw - tx, ph - ty),
            3 => (b, -a, d, -c, ty, ph - tx),
            _ => (a, b, c, d, tx, ty),
        };
        // Free zoom (issue #101), about the middle of the SCREEN and after the
        // turn, so the pan is in the frame the player's stick is in. Gated on
        // `game_layer` because only the game grows: the pause panel, the pointer
        // and the zoom legend are all drawn outside it.
        let zp = game_zoom_percent();
        let (a, b, c, d, tx, ty) = if flip_y && self.game_layer && zp != 100 {
            let z = zp as f32 / 100.0;
            let (ox, oy) = game_pan();
            (
                a * z,
                b * z,
                c * z,
                d * z,
                tx * z + pw * 0.5 * (1.0 - z) + ox as f32,
                ty * z + ph * 0.5 * (1.0 - z) + oy as f32,
            )
        } else {
            (a, b, c, d, tx, ty)
        };
        let sx = 2.0 / pw;
        let sy = if flip_y { -2.0 / ph } else { 2.0 / ph };
        let ty_off = if flip_y { 1.0 } else { -1.0 };
        [
            a * sx,
            b * sy,
            0.0,
            c * sx,
            d * sy,
            0.0,
            tx * sx - 1.0,
            ty * sy + ty_off,
            1.0,
        ]
    }

    /// Lazy-create the reusable FBO + a shared depth-stencil renderbuffer
    /// sized to cover at least `(w, h)`. The renderbuffer is required so that
    /// stencil masks pushed by `commands.execute()` inside the FBO actually
    /// work (without it the stencil ops no-op and masked sub-trees vanish).
    /// Grows monotonically; attachment persists. Must be called with the FBO
    /// already bound.
    fn ensure_offscreen_depth_stencil(&mut self, w: u32, h: u32) {
        let need_create = self.offscreen_depth_stencil == 0;
        if need_create {
            unsafe {
                let mut rbo: GLuint = 0;
                glGenRenderbuffers(1, &mut rbo);
                self.offscreen_depth_stencil = rbo;
            }
        }
        let (cw, ch) = self.offscreen_depth_stencil_dims;
        let nw = cw.max(w).max(1);
        let nh = ch.max(h).max(1);
        if need_create || nw > cw || nh > ch {
            unsafe {
                glBindRenderbuffer(GL_RENDERBUFFER, self.offscreen_depth_stencil);
                glRenderbufferStorage(GL_RENDERBUFFER, GL_DEPTH24_STENCIL8, nw as GLsizei, nh as GLsizei);
                glBindRenderbuffer(GL_RENDERBUFFER, 0);
            }
            self.offscreen_depth_stencil_dims = (nw, nh);
        }
        if need_create {
            unsafe {
                glFramebufferRenderbuffer(
                    GL_FRAMEBUFFER, GL_DEPTH_STENCIL_ATTACHMENT, GL_RENDERBUFFER,
                    self.offscreen_depth_stencil,
                );
            }
        }
    }

    /// Bind `tex` as the FBO color attachment and replay `commands` into it.
    /// Restores the previous render target + viewport. Returns false if the
    /// FBO is incomplete.
    fn render_commands_to_texture(
        &mut self,
        tex: GLuint,
        tex_w: u32,
        tex_h: u32,
        commands: CommandList,
        clear: Option<Color>,
    ) -> bool {
        if self.offscreen_fbo == 0 {
            unsafe {
                let mut fbo: GLuint = 0;
                glGenFramebuffers(1, &mut fbo);
                self.offscreen_fbo = fbo;
            }
        }
        let mut prev_fbo: GLint = 0;
        let mut prev_vp: [GLint; 4] = [0; 4];
        let bind_timer = PrimTimer::new(&PRIM_RTBIND_CUR);
        unsafe {
            glGetIntegerv(GL_FRAMEBUFFER_BINDING, &mut prev_fbo);
            glGetIntegerv(GL_VIEWPORT, prev_vp.as_mut_ptr());
            RT_BIND_CUR.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            glBindFramebuffer(GL_FRAMEBUFFER, self.offscreen_fbo);
            glFramebufferTexture2D(GL_FRAMEBUFFER, GL_COLOR_ATTACHMENT0, GL_TEXTURE_2D, tex, 0);
        }
        self.ensure_offscreen_depth_stencil(tex_w, tex_h);
        unsafe {
            if fbo_should_validate() {
                let status = glCheckFramebufferStatus(GL_FRAMEBUFFER);
                if status != GL_FRAMEBUFFER_COMPLETE {
                    let msg = std::format!("offscreen: FBO incomplete 0x{:04X} ({}x{})\n", status, tex_w, tex_h);
                    let mut b = msg.into_bytes();
                    b.push(0);
                    ruffle_log_cstr(b.as_ptr() as *const _);
                    glBindFramebuffer(GL_FRAMEBUFFER, prev_fbo as GLuint);
                    return false;
                }
            }
            glViewport(0, 0, tex_w as GLsizei, tex_h as GLsizei);
            glClearStencil(0);
            // The clear below must reach every texel: a recycled cache texture
            // still holds its previous owner's picture, where a fresh one used to
            // come out of the driver zeroed. A mask can leave the colour mask off
            // (mask_push / mask_deactivate), and Stage3D's SetScissorRectangle can
            // leave the scissor on. The colour mask is restored on the way out by
            // `mask_restore_gl`, the scissor right after the clear. The stencil
            // mask is deliberately NOT forced: a nested blend inside a masked
            // region relies on this clear leaving the enclosing mask's stencil
            // alone, and the stencil is not part of the recycled texture anyway.
            glColorMask(GL_TRUE, GL_TRUE, GL_TRUE, GL_TRUE);
            let scissor_was_on = glIsEnabled(GL_SCISSOR_TEST) == GL_TRUE;
            if scissor_was_on {
                glDisable(GL_SCISSOR_TEST);
            }
            // `Some(color)` = fresh target (clear to color). `None` = composite
            // mode: the temp was pre-seeded with the BitmapData's existing
            // content (render_offscreen's FreshWithTexture semantics), so keep
            // the colour and only reset the stencil for this pass's masks.
            if let Some(c) = clear {
                glClearColor(
                    c.r as GLfloat / 255.0,
                    c.g as GLfloat / 255.0,
                    c.b as GLfloat / 255.0,
                    c.a as GLfloat / 255.0,
                );
                glClear(GL_COLOR_BUFFER_BIT | GL_STENCIL_BUFFER_BIT);
            } else {
                glClear(GL_STENCIL_BUFFER_BIT);
            }
            if scissor_was_on {
                glEnable(GL_SCISSOR_TEST);
            }
            // Premultiplied-alpha-correct accumulation: standard blend for RGB
            // but accumulate the alpha channel additively, otherwise a cache
            // texture's alpha ends up as `a²` and is too faint when sampled.
            glEnable(GL_BLEND);
            glBlendFuncSeparate(
                GL_SRC_ALPHA, GL_ONE_MINUS_SRC_ALPHA,
                GL_ONE, GL_ONE_MINUS_SRC_ALPHA,
            );
            glDisable(GL_STENCIL_TEST);
            glColorMask(GL_TRUE, GL_TRUE, GL_TRUE, GL_TRUE);
            glStencilMask(0xFF);
        }

        // DIAG: the target switch is done; everything past here is the pass
        // itself, which `cacheUs` already covers.
        drop(bind_timer);

        let prev_mask = self.mask;
        self.mask = MaskState::default();
        let prev_offscreen = self.offscreen_dims;
        let prev_target_tex = self.offscreen_target_tex;
        self.offscreen_dims = Some((tex_w, tex_h));
        self.offscreen_target_tex = Some(tex);
        self.gl_state.invalidate();

        commands.execute(self);

        self.offscreen_dims = prev_offscreen;
        self.offscreen_target_tex = prev_target_tex;
        self.mask = prev_mask;
        unsafe {
            // No explicit colour detach: rebinding this FBO always overwrites
            // attachment 0, and GL detaches automatically if the texture is
            // deleted while still attached. It was one more attachment change
            // per render-target switch, and there are 180-240 of those a frame
            // in Mario 63's 8-13.
            glBindFramebuffer(GL_FRAMEBUFFER, prev_fbo as GLuint);
            glViewport(prev_vp[0], prev_vp[1], prev_vp[2], prev_vp[3]);
            // Restore the main-framebuffer blend (non-separate is fine there).
            glBlendFunc(GL_SRC_ALPHA, GL_ONE_MINUS_SRC_ALPHA);
        }
        // ...and the stencil, which `glDisable(GL_STENCIL_TEST)` above turned off
        // for this pass. Without this the enclosing maskee drew unclipped.
        self.mask_restore_gl();
        self.gl_state.invalidate();
        true
    }

    /// Generic single-pass filter blit. Binds the reusable FBO to `dst_tex`,
    /// runs `program` over the unit quad sampling `src_tex` (with `src_pt` /
    /// `src_size` defining the sub-rect in source coords), and writes into
    /// `(dst_x, dst_y, dst_w, dst_h)` in destination viewport coords.
    /// `setup_uniforms` is called once the program is bound, before the draw,
    /// to push filter-specific uniforms. Blend is DISABLED (filter passes
    /// overwrite rather than composite) and stencil is OFF. Restores the
    /// previous FBO/viewport. Returns false on FBO incompleteness.
    #[allow(clippy::too_many_arguments)]
    /// Redirect this frame into the screen-filter target, creating it on demand.
    /// Returns false on any failure, and the caller then draws straight to the
    /// screen as before: a filter is a cosmetic option and must never cost a
    /// frame, let alone a game.
    fn begin_screen_filter(&mut self) -> bool {
        // Physical: this target IS the screen, so it must be the screen's shape.
        // The logical portrait size would capture a 720x1280 slab of a 1280x720
        // framebuffer and present it back squeezed.
        let (w, h) = self.physical_dims();
        let (w, h) = (w.max(1), h.max(1));
        if self.screen_filter.is_none() {
            self.screen_filter = build_screen_filter_program();
            if self.screen_filter.is_none() {
                return false;
            }
        }
        if self.screen_filter_dims != (w, h) {
            unsafe {
                if self.screen_filter_tex == 0 {
                    let mut t: GLuint = 0;
                    glGenTextures(1, &mut t);
                    self.screen_filter_tex = t;
                }
                glBindTexture(GL_TEXTURE_2D, self.screen_filter_tex);
                glTexImage2D(
                    GL_TEXTURE_2D, 0, GL_RGBA as GLint, w as GLsizei, h as GLsizei, 0,
                    GL_RGBA, GL_UNSIGNED_BYTE, core::ptr::null(),
                );
                glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, GL_LINEAR as GLint);
                glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER, GL_LINEAR as GLint);
                glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_WRAP_S, GL_CLAMP_TO_EDGE as GLint);
                glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_WRAP_T, GL_CLAMP_TO_EDGE as GLint);
                glBindTexture(GL_TEXTURE_2D, 0);
                if self.screen_filter_rbo == 0 {
                    let mut r: GLuint = 0;
                    glGenRenderbuffers(1, &mut r);
                    self.screen_filter_rbo = r;
                }
                glBindRenderbuffer(GL_RENDERBUFFER, self.screen_filter_rbo);
                glRenderbufferStorage(
                    GL_RENDERBUFFER, GL_DEPTH24_STENCIL8, w as GLsizei, h as GLsizei,
                );
                glBindRenderbuffer(GL_RENDERBUFFER, 0);
                if self.screen_filter_fbo == 0 {
                    let mut f: GLuint = 0;
                    glGenFramebuffers(1, &mut f);
                    self.screen_filter_fbo = f;
                }
            }
            self.screen_filter_dims = (w, h);
        }
        unsafe {
            let mut prev: GLint = 0;
            glGetIntegerv(GL_FRAMEBUFFER_BINDING, &mut prev);
            self.screen_filter_prev_fbo = prev;
            glBindFramebuffer(GL_FRAMEBUFFER, self.screen_filter_fbo);
            glFramebufferTexture2D(
                GL_FRAMEBUFFER, GL_COLOR_ATTACHMENT0, GL_TEXTURE_2D, self.screen_filter_tex, 0,
            );
            // The game pushes stencil masks into whatever target is bound, so this
            // one needs its own depth+stencil or every masked game would break the
            // moment a filter is switched on.
            glFramebufferRenderbuffer(
                GL_FRAMEBUFFER, GL_DEPTH_STENCIL_ATTACHMENT, GL_RENDERBUFFER,
                self.screen_filter_rbo,
            );
            let status = glCheckFramebufferStatus(GL_FRAMEBUFFER);
            if status != GL_FRAMEBUFFER_COMPLETE {
                let msg = std::format!("screen filter: FBO incomplete 0x{:04X}, bypassing\n", status);
                let mut b = msg.into_bytes();
                b.push(0);
                ruffle_log_cstr(b.as_ptr() as *const _);
                glBindFramebuffer(GL_FRAMEBUFFER, prev as GLuint);
                return false;
            }
        }
        self.gl_state.invalidate();
        true
    }

    /// Resolve the captured frame onto the real framebuffer through the filter.
    fn end_screen_filter(&mut self, mode: u8) {
        let Some(prog) = self.screen_filter.as_ref() else {
            return;
        };
        let (program, u_src_uv, u_res, u_scan, u_mode) =
            (prog.program, prog.u_src_uv, prog.u_res, prog.u_scan, prog.u_mode);
        let (w, h) = self.screen_filter_dims;
        let tex = self.screen_filter_tex;
        // Raw bind below, so the sampler from the frame's last bitmap draw is
        // still live. Scanlines and CRT both sample between texels.
        self.gl_state.clear_sampler();
        unsafe {
            glBindFramebuffer(GL_FRAMEBUFFER, self.screen_filter_prev_fbo as GLuint);
            glViewport(0, 0, w as GLsizei, h as GLsizei);
            glDisable(GL_BLEND);
            glDisable(GL_STENCIL_TEST);
            glColorMask(GL_TRUE, GL_TRUE, GL_TRUE, GL_TRUE);
            glUseProgram(program);
            glActiveTexture(GL_TEXTURE0);
            glBindTexture(GL_TEXTURE_2D, tex);
            glUniform4f(u_src_uv, 0.0, 0.0, 1.0, 1.0);
            glUniform2f(u_res, w as GLfloat, h as GLfloat);
            glUniform1f(u_scan, scanline_count() as GLfloat);
            glUniform1i(u_mode, mode as GLint);
            glBindVertexArray(self.bitmap_vao);
            glDrawArrays(GL_TRIANGLES, 0, 6);
            glBindVertexArray(0);
            glBindTexture(GL_TEXTURE_2D, 0);
            glUseProgram(0);
            glEnable(GL_BLEND);
        }
        self.gl_state.invalidate();
    }

    fn draw_filter_pass(
        &mut self,
        program: GLuint,
        u_src_uv_loc: GLint,
        src_tex: GLuint,
        src_w: u32,
        src_h: u32,
        src_pt: (u32, u32),
        src_size: (u32, u32),
        dst_tex: GLuint,
        dst_x: i32,
        dst_y: i32,
        dst_w: u32,
        dst_h: u32,
        setup_uniforms: impl FnOnce(),
    ) -> bool {
        // Central safety net: FBO-attaching a 0 (freed/absent) destination texture
        // makes Mesa update a null renderbuffer surface and crash (native DataAbort)
        // BEFORE the completeness check below can reject it. Callers should never
        // pass a dead texture, but bail cleanly if one slips through.
        if dst_tex == 0 {
            return false;
        }
        // Every filter, blit, alpha-mask composite and blur funnels through here,
        // and all of them bind their source texture raw instead of going through
        // `use_bitmap`. The state cache only invalidates on the way OUT, so a
        // sharp sampler left by the preceding bitmap draw would still be on unit
        // 0 for this pass's taps. A NEAREST blur is not a softer blur, it is a
        // mosaic.
        self.gl_state.clear_sampler();
        // Colour-only FBO, deliberately NOT `offscreen_fbo`: see `filter_fbo`.
        // A filter pass disables the stencil test two statements below and
        // never reads depth, so it must not drag the shared (and monotonically
        // growing) D24S8 renderbuffer along for the ride.
        if self.filter_fbo == 0 {
            unsafe {
                let mut fbo: GLuint = 0;
                glGenFramebuffers(1, &mut fbo);
                self.filter_fbo = fbo;
            }
        }
        let mut prev_fbo: GLint = 0;
        let mut prev_vp: [GLint; 4] = [0; 4];
        let bind_timer = PrimTimer::new(&PRIM_RTBIND_CUR);
        unsafe {
            glGetIntegerv(GL_FRAMEBUFFER_BINDING, &mut prev_fbo);
            glGetIntegerv(GL_VIEWPORT, prev_vp.as_mut_ptr());
            RT_BIND_CUR.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            glBindFramebuffer(GL_FRAMEBUFFER, self.filter_fbo);
            glFramebufferTexture2D(GL_FRAMEBUFFER, GL_COLOR_ATTACHMENT0, GL_TEXTURE_2D, dst_tex, 0);
        }
        unsafe {
            if fbo_should_validate() {
                let status = glCheckFramebufferStatus(GL_FRAMEBUFFER);
                if status != GL_FRAMEBUFFER_COMPLETE {
                    let msg = std::format!("filter pass: FBO incomplete 0x{:04X}\n", status);
                    let mut b = msg.into_bytes();
                    b.push(0);
                    ruffle_log_cstr(b.as_ptr() as *const _);
                    glBindFramebuffer(GL_FRAMEBUFFER, prev_fbo as GLuint);
                    return false;
                }
            }
            glViewport(dst_x, dst_y, dst_w as GLsizei, dst_h as GLsizei);
            glDisable(GL_BLEND);
            glDisable(GL_STENCIL_TEST);
            glColorMask(GL_TRUE, GL_TRUE, GL_TRUE, GL_TRUE);
            glUseProgram(program);
            glActiveTexture(GL_TEXTURE0);
            glBindTexture(GL_TEXTURE_2D, src_tex);
            // Source UV sub-rect: which region of src_tex to sample.
            let su = src_pt.0 as f32 / src_w.max(1) as f32;
            let sv = src_pt.1 as f32 / src_h.max(1) as f32;
            let sw = src_size.0 as f32 / src_w.max(1) as f32;
            let sh = src_size.1 as f32 / src_h.max(1) as f32;
            glUniform4f(u_src_uv_loc, su, sv, sw, sh);
        }
        // DIAG: stop the clock before the quad, so `rtbindUs` means the target
        // switch alone. Whatever is left in `cacheUs` after subtracting it is
        // the actual drawing, and the two answer different questions.
        drop(bind_timer);
        setup_uniforms();
        unsafe {
            glBindVertexArray(self.bitmap_vao);
            glDrawArrays(GL_TRIANGLES, 0, 6);
            glBindVertexArray(0);
            glBindTexture(GL_TEXTURE_2D, 0);
            glEnable(GL_BLEND);
            // No explicit colour detach — see `render_commands_to_texture`. This
            // is the hotter of the two: five of the six rebinds an entry costs
            // come through here.
            glBindFramebuffer(GL_FRAMEBUFFER, prev_fbo as GLuint);
            glViewport(prev_vp[0], prev_vp[1], prev_vp[2], prev_vp[3]);
        }
        self.gl_state.invalidate();
        true
    }

    /// Reorder a 20-float SWF ColorMatrixFilter into the `(mat4, vec4)` pair
    /// the GLSL `color_matrix.wgsl` expects (column-major mat4).
    fn color_matrix_uniforms(matrix: &[f32; 20]) -> ([f32; 16], [f32; 4]) {
        let mat4 = [
            matrix[0], matrix[5], matrix[10], matrix[15],  // col 0 = input r
            matrix[1], matrix[6], matrix[11], matrix[16],  // col 1 = input g
            matrix[2], matrix[7], matrix[12], matrix[17],  // col 2 = input b
            matrix[3], matrix[8], matrix[13], matrix[18],  // col 3 = input a
        ];
        let extras = [matrix[4] / 255.0, matrix[9] / 255.0, matrix[14] / 255.0, matrix[19] / 255.0];
        (mat4, extras)
    }

    /// Identity-blit `(src_tex, src_pt, src_size)` to `(dst_tex, dst_pt, dst_w, dst_h)`
    /// via the ColorMatrix shader with an identity matrix. Used to copy the
    /// final filter target back to a cache entry's destination texture.
    #[allow(clippy::too_many_arguments)]
    fn blit_identity(
        &mut self,
        src_tex: GLuint,
        src_w: u32,
        src_h: u32,
        src_pt: (u32, u32),
        src_size: (u32, u32),
        dst_tex: GLuint,
        dst_pt: (i32, i32),
        dst_w: u32,
        dst_h: u32,
    ) -> bool {
        let prog = self.color_matrix_filter.program;
        let u_src_uv = self.color_matrix_filter.u_src_uv;
        let u_mat = self.color_matrix_filter.u_color_mat;
        let u_extra = self.color_matrix_filter.u_color_extra;
        #[rustfmt::skip]
        let id: [f32; 16] = [
            1.0, 0.0, 0.0, 0.0,
            0.0, 1.0, 0.0, 0.0,
            0.0, 0.0, 1.0, 0.0,
            0.0, 0.0, 0.0, 1.0,
        ];
        let zero = [0.0_f32; 4];
        self.draw_filter_pass(
            prog, u_src_uv,
            src_tex, src_w, src_h, src_pt, src_size,
            dst_tex, dst_pt.0, dst_pt.1, dst_w, dst_h,
            move || unsafe {
                glUniformMatrix4fv(u_mat, 1, GL_FALSE, id.as_ptr());
                glUniform4f(u_extra, zero[0], zero[1], zero[2], zero[3]);
            },
        )
    }

    /// GPU premultiplied->straight blit of the `(src_pt, src_size)` sub-rect of
    /// `src_tex` into the `(dst_pt, src_size)` sub-rect of `dst_tex`, via
    /// UNPREMULT_FRAG. Used to repatriate a draw() render into an atlas slot
    /// without the per-call `glReadPixels` + CPU un-premultiply + re-upload that
    /// dominated frame time on cacheAsBitmap-heavy AS3 games.
    #[allow(clippy::too_many_arguments)]
    fn blit_unpremult(
        &mut self,
        src_tex: GLuint,
        src_w: u32,
        src_h: u32,
        src_pt: (u32, u32),
        src_size: (u32, u32),
        dst_tex: GLuint,
        dst_pt: (i32, i32),
        dst_w: u32,
        dst_h: u32,
    ) -> bool {
        let prog = self.unpremult_blit.program;
        let u_src_uv = self.unpremult_blit.u_src_uv;
        self.draw_filter_pass(
            prog, u_src_uv,
            src_tex, src_w, src_h, src_pt, src_size,
            dst_tex, dst_pt.0, dst_pt.1, dst_w, dst_h,
            || {},
        )
    }

    /// GPU straight->premultiplied blit (inverse of `blit_unpremult`), via
    /// PREMULT_FRAG. Seeds a render_offscreen temp with a BitmapData's existing
    /// (straight, atlas-stored) content so the new draw() commands composite
    /// onto it instead of replacing it.
    #[allow(clippy::too_many_arguments)]
    fn blit_premult(
        &mut self,
        src_tex: GLuint,
        src_w: u32,
        src_h: u32,
        src_pt: (u32, u32),
        src_size: (u32, u32),
        dst_tex: GLuint,
        dst_pt: (i32, i32),
        dst_w: u32,
        dst_h: u32,
    ) -> bool {
        let prog = self.premult_blit.program;
        let u_src_uv = self.premult_blit.u_src_uv;
        self.draw_filter_pass(
            prog, u_src_uv,
            src_tex, src_w, src_h, src_pt, src_size,
            dst_tex, dst_pt.0, dst_pt.1, dst_w, dst_h,
            || {},
        )
    }

    /// Acquire a temp texture for a `render_offscreen` pass: reuse a pooled one
    /// of the exact size if available (the steady-state case — BitmapData sizes
    /// are stable across frames), else allocate a fresh one.
    /// `render_commands_to_texture` clears it, so stale pooled content is fine.
    fn acquire_offscreen_temp(&mut self, w: u32, h: u32) -> Option<StandaloneTexture> {
        // FlashNX (2026-10-06): any temp at least this big serves, the smallest
        // one, within 4x the area asked so a small draw does not take a big
        // target away; new temps are made in 64-pixel steps so the next draw of
        // a nearby size finds one. Only an exact size used to match, and Super
        // Smash Flash 2 draws into BitmapData of every size: each miss is a GPU
        // object made (nvMapCreate and two mappings, 1-2 ms) and later destroyed,
        // and 389 frames of a session lost 20 ms or more to that. The callers
        // draw into the top-left (w, h) corner and pass the temp's own size
        // wherever they read it as a source.
        let fits = |t: &StandaloneTexture| {
            let area = t.width as u64 * t.height as u64;
            t.width >= w && t.height >= h && area <= 4 * (w as u64 * h as u64).max(64 * 64)
        };
        let area_of = |t: &StandaloneTexture| t.width as u64 * t.height as u64;
        if let Some(i) = self
            .offscreen_temp_pool
            .iter()
            .enumerate()
            .filter(|(_, t)| fits(&t.tex))
            .min_by_key(|(_, t)| area_of(&t.tex))
            .map(|(i, _)| i)
        {
            return Some(self.offscreen_temp_pool.swap_remove(i).tex);
        }
        // Big (standalone-backed) targets: reuse a temp already RETIRED this frame,
        // not just the recycled pool. The multisprite builder composites many big
        // draws into ONE strip per frame — Papa Louie 3's player strip is 8007x858
        // and takes ~8 draws/frame; without this each draw allocates a fresh ~27 MB
        // texture (they aren't back in the pool until submit_frame), ~220 MB/frame,
        // which exhausts the GPU (glTexImage2D OOM -> "does not support
        // BitmapData.draw" -> stuck on the loading screen). Reusing collapses that
        // to one live temp. Safe only because a standalone target's SyncHandle now
        // points at the BitmapData's own persistent texture, never at this temp
        // (see render_offscreen) — so a retired big temp is unreferenced. Small
        // atlas-backed temps (whose handle still IS the temp, resolved same-frame)
        // keep the old behaviour, hence the >ATLAS_SIZE gate.
        if w > ATLAS_SIZE || h > ATLAS_SIZE {
            if let Some(i) = self
                .offscreen_temp_retired
                .iter()
                .enumerate()
                .filter(|(_, t)| fits(t))
                .min_by_key(|(_, t)| area_of(t))
                .map(|(i, _)| i)
            {
                return Some(self.offscreen_temp_retired.swap_remove(i));
            }
        }
        let step = |n: u32| {
            let up = n.div_ceil(64) * 64;
            if up <= 16384 { up } else { n }
        };
        make_standalone_texture(step(w), step(h)).map(|t| t.tagged(ST_OFFSCREEN))
    }

    /// Read an (x, y, w, h) sub-rect of `tex` back into a CPU RGBA buffer with
    /// PREMULTIPLIED alpha. `tex` is one of our offscreen renders (premultiplied,
    /// texel row 0 = Flash top): attach it to the shared offscreen FBO,
    /// `glReadPixels` (row 0 = y=0 = texel row 0 = top — no Y-flip), and hand the
    /// bytes back unchanged. Ruffle's BitmapData CPU pixels are PREMULTIPLIED
    /// (`copy_pixels_to_bitmapdata` stores whatever resolve_sync_handle returns
    /// verbatim, and wgpu's `capture` returns the raw premultiplied GPU bytes —
    /// only image *export* un-multiplies), so we must NOT un-premultiply here. An
    /// earlier `rgb/a` divide returned straight: a no-op for opaque pixels
    /// (a=255, premult==straight — why tile engines worked) but for translucent
    /// content the divide snapped low-alpha colours to channel extremes →
    /// offroaders' cyan/magenta particle-smoke speckle. Saves/restores the bound
    /// FBO since this runs during AS execution. Buffer stride = w*4, row 0 = y_min.
    fn readback_region_straight(&mut self, tex: GLuint, x: u32, y: u32, w: u32, h: u32) -> Vec<u8> {
        let mut buf = vec![0u8; (w as usize) * (h as usize) * 4];
        if w == 0 || h == 0 {
            return buf;
        }
        // Colour-only FBO: a readback rasterises nothing, so there is no reason
        // to bind the one carrying the shared depth+stencil renderbuffer. Papa
        // Louie 3 pays this path once per frame (`primRes` ~3.1 ms of a 65 ms
        // frame, measured 2026-08-24), which is the one backend cost it has.
        if self.filter_fbo == 0 {
            unsafe {
                let mut fbo: GLuint = 0;
                glGenFramebuffers(1, &mut fbo);
                self.filter_fbo = fbo;
            }
        }
        let mut prev_fbo: GLint = 0;
        unsafe {
            glGetIntegerv(GL_FRAMEBUFFER_BINDING, &mut prev_fbo);
            RT_BIND_CUR.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            glBindFramebuffer(GL_FRAMEBUFFER, self.filter_fbo);
            glFramebufferTexture2D(
                GL_FRAMEBUFFER, GL_COLOR_ATTACHMENT0, GL_TEXTURE_2D, tex, 0,
            );
            glPixelStorei(GL_PACK_ALIGNMENT, 1);
            glReadPixels(
                x as GLint, y as GLint, w as GLsizei, h as GLsizei,
                GL_RGBA, GL_UNSIGNED_BYTE, buf.as_mut_ptr() as *mut _,
            );
            glFramebufferTexture2D(GL_FRAMEBUFFER, GL_COLOR_ATTACHMENT0, GL_TEXTURE_2D, 0, 0);
            glBindFramebuffer(GL_FRAMEBUFFER, prev_fbo as GLuint);
        }
        // Premultiplied already — return verbatim (no divide).
        buf
    }

    /// Apply a ColorMatrixFilter from `source` (full standalone) to
    /// `destination`. Handles source==dest via a pool temp.
    #[allow(clippy::too_many_arguments)]
    fn apply_color_matrix_raw(
        &mut self,
        src_tex: GLuint,
        src_w: u32,
        src_h: u32,
        source_point: (u32, u32),
        source_size: (u32, u32),
        dst_tex: GLuint,
        dest_point: (i32, i32),
        filter: &swf::ColorMatrixFilter,
    ) -> bool {
        let prog = self.color_matrix_filter.program;
        let u_src_uv = self.color_matrix_filter.u_src_uv;
        let u_mat = self.color_matrix_filter.u_color_mat;
        let u_extra = self.color_matrix_filter.u_color_extra;
        let (mat, extras) = Self::color_matrix_uniforms(&filter.matrix);

        if src_tex != dst_tex {
            return self.draw_filter_pass(
                prog, u_src_uv,
                src_tex, src_w, src_h, source_point, source_size,
                dst_tex, dest_point.0, dest_point.1, source_size.0, source_size.1,
                move || unsafe {
                    glUniformMatrix4fv(u_mat, 1, GL_FALSE, mat.as_ptr());
                    glUniform4f(u_extra, extras[0], extras[1], extras[2], extras[3]);
                },
            );
        }
        // In-place: filter into a temp, then identity-blit back.
        let Some(temp) = self.filter_tex_pool.acquire(source_size.0, source_size.1) else { return false };
        let temp_tex = temp.texture;
        let temp_w = temp.width;
        let temp_h = temp.height;
        let ok1 = self.draw_filter_pass(
            prog, u_src_uv,
            src_tex, src_w, src_h, source_point, source_size,
            temp_tex, 0, 0, source_size.0, source_size.1,
            move || unsafe {
                glUniformMatrix4fv(u_mat, 1, GL_FALSE, mat.as_ptr());
                glUniform4f(u_extra, extras[0], extras[1], extras[2], extras[3]);
            },
        );
        if !ok1 {
            self.filter_tex_pool.release(temp);
            return false;
        }
        let ok2 = self.blit_identity(
            temp_tex, temp_w, temp_h, (0, 0), (temp_w, temp_h),
            dst_tex, dest_point, source_size.0, source_size.1,
        );
        self.filter_tex_pool.release(temp);
        ok2
    }

    /// Run the H+V ping-pong loop of a separable blur. Returns the temp
    /// texture holding the blurred result, or None if the blur was impotent
    /// (no axis above 1.0). Caller releases the returned texture to the pool.
    fn run_blur_to_temp(
        &mut self,
        src_tex: GLuint,
        src_w: u32,
        src_h: u32,
        source_point: (u32, u32),
        source_size: (u32, u32),
        filter: &swf::BlurFilter,
    ) -> Option<StandaloneTexture> {
        // Cap blur quality passes at 1. Flash defaults to 3 (a box blur
        // iterated 3× ≈ Gaussian), but each pass is 2 extra FBO draws (H+V) per
        // filtered element — and Mario 63's menu filters dozens of cached text
        // elements per frame, so 3 passes tripled the offscreen draw load and
        // spiked render time. One pass is visually fine for thin glow/shadow
        // outlines and roughly thirds the blur cost.
        let num_passes = (filter.num_passes() as usize).min(1);
        let blur_x = filter.blur_x.to_f32().min(255.0);
        let blur_y = filter.blur_y.to_f32().min(255.0);

        // Neither axis blurs → keep the None contract (glow/bevel synthesise a
        // transparent halo; plain blur passes the source through). Checked up
        // front so the half-res seed below never runs for an impotent blur.
        if blur_x <= 1.0 && blur_y <= 1.0 {
            return None;
        }

        // HALF-RESOLUTION blur. Blur is low-frequency, so we downsample the
        // source, blur at ¼ the fill, and let callers upsample the result via
        // normalised-uv sampling / size-aware blit — visually identical for
        // glow/shadow/bevel halos. This was the dominant per-frame cost on
        // Mario 63's lit scenes: each filter chain ran ~8-11 ms and 9-26 chains
        // fire per frame, so `render` hit 90-260 ms (fps 9-26). The result temp
        // is half-size, which is transparent to callers ONLY because their
        // blur-offset uv divides by the SOURCE size, not the temp pixel size
        // (see apply_glow_or_drop_shadow_raw / apply_bevel_raw). Engage only
        // above a min size so thin outlines stay crisp and tiny surfaces don't
        // pay for the extra downsample.
        let downscale = source_size.0 >= 64 && source_size.1 >= 64;
        let (work_w, work_h, scale) = if downscale {
            ((source_size.0 / 2).max(1), (source_size.1 / 2).max(1), 0.5_f32)
        } else {
            (source_size.0, source_size.1, 1.0_f32)
        };

        let mut flip = self.filter_tex_pool.acquire(work_w, work_h)?;
        let Some(mut flop) = self.filter_tex_pool.acquire(work_w, work_h) else {
            self.filter_tex_pool.release(flip);
            return None;
        };

        // Seed `flip` with the source at work resolution — blit_identity scales
        // full-res src → work-res via linear filtering, which IS the downsample.
        if !self.blit_identity(
            src_tex, src_w, src_h, source_point, source_size,
            flip.texture, (0, 0), work_w, work_h,
        ) {
            self.filter_tex_pool.release(flip);
            self.filter_tex_pool.release(flop);
            return None;
        }

        let prog = self.blur_filter.program;
        let u_src_uv = self.blur_filter.u_src_uv;
        let u_dir = self.blur_filter.u_blur_dir;
        let u_m = self.blur_filter.u_blur_m;
        let u_m2 = self.blur_filter.u_blur_m2;
        let u_full = self.blur_filter.u_blur_full_size;
        let u_first = self.blur_filter.u_blur_first_weight;
        let u_last_off = self.blur_filter.u_blur_last_offset;
        let u_last_wt = self.blur_filter.u_blur_last_weight;

        let mut any_pass = false;
        for _ in 0..num_passes {
            for i in 0..2 {
                let horizontal = i % 2 == 0;
                // Strength is in source pixels; at work resolution each texel
                // spans 1/scale source px, so the kernel radius scales with
                // `scale` to keep the spatial blur the same.
                let strength = if horizontal { blur_x } else { blur_y } * scale;
                let full_size = strength.min(255.0);
                if full_size <= 1.0 { continue; }

                // `flip` is already seeded with the (downsampled) source, so we
                // always ping-pong on the work-res temps.
                let (sample_tex, sample_w, sample_h, sample_pt, sample_sz) =
                    (flip.texture, flip.width, flip.height, (0, 0), (flip.width, flip.height));
                // Fractional-radius fast blur (cf. fgiesen blog post).
                let radius = (full_size - 1.0) / 2.0;
                let m = radius.ceil() - 1.0;
                let alpha = ((radius - m) * 255.0).floor() / 255.0;
                let last_offset = 1.0 / ((1.0 / alpha) + 1.0);
                let last_weight = alpha + 1.0;
                let dir = if horizontal {
                    (1.0_f32 / sample_w.max(1) as f32, 0.0_f32)
                } else {
                    (0.0_f32, 1.0_f32 / sample_h.max(1) as f32)
                };
                let m_val = m;
                let m2_val = m * 2.0;
                let flop_tex = flop.texture;
                let flop_w = flop.width;
                let flop_h = flop.height;
                let ok = self.draw_filter_pass(
                    prog, u_src_uv,
                    sample_tex, sample_w, sample_h, sample_pt, sample_sz,
                    flop_tex, 0, 0, flop_w, flop_h,
                    move || unsafe {
                        glUniform2f(u_dir, dir.0, dir.1);
                        glUniform1f(u_m, m_val);
                        glUniform1f(u_m2, m2_val);
                        glUniform1f(u_full, full_size);
                        glUniform1f(u_first, alpha);
                        glUniform1f(u_last_off, last_offset);
                        glUniform1f(u_last_wt, last_weight);
                    },
                );
                if !ok {
                    self.filter_tex_pool.release(flip);
                    self.filter_tex_pool.release(flop);
                    return None;
                }
                any_pass = true;
                std::mem::swap(&mut flip, &mut flop);
            }
        }
        self.filter_tex_pool.release(flop);
        // `flip` holds the blurred source — or, if both scaled strengths fell
        // below 1 px (a sub-pixel blur on a large surface), merely the
        // downsampled seed, which is itself a valid mild low-freq halo. Either
        // way it's a usable result, so we never fall back to the None path here
        // (we already returned None up front for a truly impotent blur).
        let _ = any_pass;
        Some(flip)
    }

    /// Apply a Blur filter `source` → `destination`.
    #[allow(clippy::too_many_arguments)]
    fn apply_blur_raw(
        &mut self,
        src_tex: GLuint,
        src_w: u32,
        src_h: u32,
        source_point: (u32, u32),
        source_size: (u32, u32),
        dst_tex: GLuint,
        dest_point: (i32, i32),
        filter: &swf::BlurFilter,
    ) -> bool {
        match self.run_blur_to_temp(src_tex, src_w, src_h, source_point, source_size, filter) {
            Some(result) => {
                let rt = result.texture;
                let rw = result.width;
                let rh = result.height;
                let ok = self.blit_identity(
                    rt, rw, rh, (0, 0), (rw, rh),
                    dst_tex, dest_point, source_size.0, source_size.1,
                );
                self.filter_tex_pool.release(result);
                ok
            }
            // The in-place blur of the cache loop passes the same texture on both
            // sides, and `run_blur_to_temp` also returns None when it could not
            // get its temps. Blitting a texture onto itself samples the render
            // target, which is undefined; leaving the content unblurred is the
            // same as the pass-through this branch means.
            None if src_tex == dst_tex
                && source_point.0 as i32 == dest_point.0
                && source_point.1 as i32 == dest_point.1 =>
            {
                true
            }
            None => self.blit_identity(
                src_tex, src_w, src_h, source_point, source_size,
                dst_tex, dest_point, source_size.0, source_size.1,
            ),
        }
    }

    /// Apply a Glow (`blur_offset = (0, 0)`) or DropShadow (non-zero offset).
    /// `blur_offset` is in source pixels. Faithful to wgpu's
    /// `vertices_with_blur_offset`: `blur_uv = (source_left + blur_offset) /
    /// source_width`. DropShadow callers pass `(-x, -y)` so the blur sample
    /// at quad top-left lies above-left of source, visible shadow ends up
    /// down-right (the angle=0 convention).
    #[allow(clippy::too_many_arguments)]
    fn apply_glow_or_drop_shadow_raw(
        &mut self,
        src_tex: GLuint,
        src_w: u32,
        src_h: u32,
        source_point: (u32, u32),
        source_size: (u32, u32),
        dst_tex: GLuint,
        dest_point: (i32, i32),
        filter: &swf::GlowFilter,
        blur_offset: (f32, f32),
    ) -> bool {
        let blur_args = filter.inner_blur_filter();
        let blur_temp_opt = self.run_blur_to_temp(
            src_tex, src_w, src_h, source_point, source_size, &blur_args,
        );

        // If blur was impotent, synthesise a fully-transparent temp so the
        // glow shader reads blur_a=0 and outputs the "no glow" tint cleanly.
        // We don't bind the temp's pixel size: the blur temp may be half-res
        // (see run_blur_to_temp), and the blur-offset uv below is computed from
        // the SOURCE size so it's resolution-independent.
        let (blur_tex, blur_temp_to_release) = match blur_temp_opt {
            Some(t) => (t.texture, Some(t)),
            None => {
                let Some(empty) = self.filter_tex_pool.acquire(source_size.0, source_size.1) else {
                    return false;
                };
                // Pool entries may hold stale data — clear to transparent.
                if self.filter_fbo == 0 {
                    unsafe {
                        let mut fbo: GLuint = 0;
                        glGenFramebuffers(1, &mut fbo);
                        self.filter_fbo = fbo;
                    }
                }
                unsafe {
                    let mut prev_fbo: GLint = 0;
                    let mut prev_vp: [GLint; 4] = [0; 4];
                    glGetIntegerv(GL_FRAMEBUFFER_BINDING, &mut prev_fbo);
                    glGetIntegerv(GL_VIEWPORT, prev_vp.as_mut_ptr());
                    RT_BIND_CUR.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    glBindFramebuffer(GL_FRAMEBUFFER, self.filter_fbo);
                    glFramebufferTexture2D(GL_FRAMEBUFFER, GL_COLOR_ATTACHMENT0, GL_TEXTURE_2D, empty.texture, 0);
                    glClearColor(0.0, 0.0, 0.0, 0.0);
                    glClear(GL_COLOR_BUFFER_BIT);
                    glFramebufferTexture2D(GL_FRAMEBUFFER, GL_COLOR_ATTACHMENT0, GL_TEXTURE_2D, 0, 0);
                    glBindFramebuffer(GL_FRAMEBUFFER, prev_fbo as GLuint);
                    glViewport(prev_vp[0], prev_vp[1], prev_vp[2], prev_vp[3]);
                }
                (empty.texture, Some(empty))
            }
        };

        let prog = self.glow_filter.program;
        let u_src_uv = self.glow_filter.u_src_uv;
        let u_blur_uv = self.glow_filter.u_blur_uv;
        let u_color = self.glow_filter.u_color;
        let u_strength = self.glow_filter.u_strength;
        let u_inner = self.glow_filter.u_inner;
        let u_knockout = self.glow_filter.u_knockout;
        let u_composite_source = self.glow_filter.u_composite_source;

        // Blur UV remap matches wgpu: at quad (0,0), uv = blur_offset / W; at
        // quad (1,1), uv = 1 + blur_offset / W. Sign is direct (no negation).
        // Divide by the SOURCE size (not the blur temp's pixel size) so the
        // offset stays correct when the blur temp is half-res — the temp spans
        // the same [0,1] spatial region regardless of its resolution.
        let bu0 = blur_offset.0 / source_size.0.max(1) as f32;
        let bv0 = blur_offset.1 / source_size.1.max(1) as f32;
        let color_f = [
            filter.color.r as f32 / 255.0,
            filter.color.g as f32 / 255.0,
            filter.color.b as f32 / 255.0,
            filter.color.a as f32 / 255.0,
        ];
        let strength = filter.strength.to_f32();
        let inner_i: GLint = if filter.is_inner() { 1 } else { 0 };
        let knockout_i: GLint = if filter.is_knockout() { 1 } else { 0 };
        let composite_i: GLint = if filter.composite_source() { 1 } else { 0 };

        unsafe {
            glActiveTexture(GL_TEXTURE1);
            glBindTexture(GL_TEXTURE_2D, blur_tex);
            glActiveTexture(GL_TEXTURE0);
        }
        let ok = self.draw_filter_pass(
            prog, u_src_uv,
            src_tex, src_w, src_h, source_point, source_size,
            dst_tex, dest_point.0, dest_point.1, source_size.0, source_size.1,
            move || unsafe {
                glUniform4f(u_blur_uv, bu0, bv0, 1.0, 1.0);
                glUniform4f(u_color, color_f[0], color_f[1], color_f[2], color_f[3]);
                glUniform1f(u_strength, strength);
                glUniform1i(u_inner, inner_i);
                glUniform1i(u_knockout, knockout_i);
                glUniform1i(u_composite_source, composite_i);
            },
        );
        unsafe {
            glActiveTexture(GL_TEXTURE1);
            glBindTexture(GL_TEXTURE_2D, 0);
            glActiveTexture(GL_TEXTURE0);
        }
        if let Some(t) = blur_temp_to_release { self.filter_tex_pool.release(t); }
        ok
    }

    /// Bevel: blur the source alpha, then a composite pass samples that blur at
    /// two opposite offsets (±angle·distance) to make a highlight side and a
    /// shadow side. Faithful port of wgpu's bevel. Mirrors the glow path.
    fn apply_bevel_raw(
        &mut self,
        src_tex: GLuint,
        src_w: u32,
        src_h: u32,
        source_point: (u32, u32),
        source_size: (u32, u32),
        dst_tex: GLuint,
        dest_point: (i32, i32),
        filter: &swf::BevelFilter,
    ) -> bool {
        let blur_args = filter.inner_blur_filter();
        let blur_temp_opt = self.run_blur_to_temp(
            src_tex, src_w, src_h, source_point, source_size, &blur_args,
        );
        let (blur_tex, blur_temp_to_release) = match blur_temp_opt {
            Some(t) => (t.texture, Some(t)),
            None => {
                // Impotent blur → synthesise a transparent temp so both
                // samples read 0 (no highlight/shadow, source passes through).
                let Some(empty) = self.filter_tex_pool.acquire(source_size.0, source_size.1) else {
                    return false;
                };
                if self.filter_fbo == 0 {
                    unsafe {
                        let mut fbo: GLuint = 0;
                        glGenFramebuffers(1, &mut fbo);
                        self.filter_fbo = fbo;
                    }
                }
                unsafe {
                    let mut prev_fbo: GLint = 0;
                    let mut prev_vp: [GLint; 4] = [0; 4];
                    glGetIntegerv(GL_FRAMEBUFFER_BINDING, &mut prev_fbo);
                    glGetIntegerv(GL_VIEWPORT, prev_vp.as_mut_ptr());
                    RT_BIND_CUR.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    glBindFramebuffer(GL_FRAMEBUFFER, self.filter_fbo);
                    glFramebufferTexture2D(GL_FRAMEBUFFER, GL_COLOR_ATTACHMENT0, GL_TEXTURE_2D, empty.texture, 0);
                    glClearColor(0.0, 0.0, 0.0, 0.0);
                    glClear(GL_COLOR_BUFFER_BIT);
                    glFramebufferTexture2D(GL_FRAMEBUFFER, GL_COLOR_ATTACHMENT0, GL_TEXTURE_2D, 0, 0);
                    glBindFramebuffer(GL_FRAMEBUFFER, prev_fbo as GLuint);
                    glViewport(prev_vp[0], prev_vp[1], prev_vp[2], prev_vp[3]);
                }
                (empty.texture, Some(empty))
            }
        };

        // ±blur_offset along the filter angle, normalised to the SOURCE size
        // (not the blur temp's pixel size) so the highlight/shadow offset stays
        // correct when the blur temp is half-res — see run_blur_to_temp.
        let distance = filter.distance.to_f32();
        let angle = filter.angle.to_f32();
        let off = (angle.cos() * distance, angle.sin() * distance);
        let bw = source_size.0.max(1) as f32;
        let bh = source_size.1.max(1) as f32;
        let (lu, lv) = (off.0 / bw, off.1 / bh);
        let (ru, rv) = (-off.0 / bw, -off.1 / bh);

        // Premultiplied colors (matches wgpu) — the cache texture is later
        // drawn back with premultiplied "over".
        let prem = |c: swf::Color| {
            let a = c.a as f32 / 255.0;
            [c.r as f32 / 255.0 * a, c.g as f32 / 255.0 * a, c.b as f32 / 255.0 * a, a]
        };
        let hi = prem(filter.highlight_color);
        let sh = prem(filter.shadow_color);
        let strength = filter.strength.to_f32();
        let bevel_type: GLint = if filter.is_on_top() { 2 } else if filter.is_inner() { 1 } else { 0 };
        let knockout_i: GLint = if filter.is_knockout() { 1 } else { 0 };

        let prog = self.bevel_filter.program;
        let u_src_uv = self.bevel_filter.u_src_uv;
        let u_blur_uv_l = self.bevel_filter.u_blur_uv_l;
        let u_blur_uv_r = self.bevel_filter.u_blur_uv_r;
        let u_highlight = self.bevel_filter.u_highlight;
        let u_shadow = self.bevel_filter.u_shadow;
        let u_strength = self.bevel_filter.u_strength;
        let u_bevel_type = self.bevel_filter.u_bevel_type;
        let u_knockout = self.bevel_filter.u_knockout;

        unsafe {
            glActiveTexture(GL_TEXTURE1);
            glBindTexture(GL_TEXTURE_2D, blur_tex);
            glActiveTexture(GL_TEXTURE0);
        }
        let ok = self.draw_filter_pass(
            prog, u_src_uv,
            src_tex, src_w, src_h, source_point, source_size,
            dst_tex, dest_point.0, dest_point.1, source_size.0, source_size.1,
            move || unsafe {
                glUniform4f(u_blur_uv_l, lu, lv, 1.0, 1.0);
                glUniform4f(u_blur_uv_r, ru, rv, 1.0, 1.0);
                glUniform4f(u_highlight, hi[0], hi[1], hi[2], hi[3]);
                glUniform4f(u_shadow, sh[0], sh[1], sh[2], sh[3]);
                glUniform1f(u_strength, strength);
                glUniform1i(u_bevel_type, bevel_type);
                glUniform1i(u_knockout, knockout_i);
            },
        );
        unsafe {
            glActiveTexture(GL_TEXTURE1);
            glBindTexture(GL_TEXTURE_2D, 0);
            glActiveTexture(GL_TEXTURE0);
        }
        if let Some(t) = blur_temp_to_release { self.filter_tex_pool.release(t); }
        ok
    }

    /// Filter dispatcher used by both the trait `apply_filter` (for
    /// BitmapData operations Ruffle drives directly) and the
    /// `cache_entries` chain in `submit_frame`. Takes raw texture IDs so the
    /// cache_entries loop can use `FilterTexturePool` temps without wrapping
    /// each one in a `BitmapHandle` Arc (which would tie its lifetime to
    /// the Arc rather than the pool — the perf blocker for filtered scenes).
    #[allow(clippy::too_many_arguments)]
    /// Resolve a `BitmapHandle` to `(texture, tex_w, tex_h, base_x, base_y, is_atlas)`,
    /// whether it's a standalone texture (base 0,0; tex dims = content; PREMULT) or
    /// an atlas-packed bitmap (base = its atlas pixel offset; tex dims = ATLAS_SIZE;
    /// STRAIGHT alpha). Lets `apply_filter` work on BitmapData that live in the
    /// shared atlas, not just dedicated textures (#42: Papa Louie 3's water). The
    /// `is_atlas` flag picks the right premult/straight blit at each boundary.
    fn resolve_bitmap_tex(&self, handle: &BitmapHandle) -> Option<(GLuint, u32, u32, u32, u32, bool)> {
        if let Some(s) = as_standalone_bitmap(handle) {
            return Some((s.0.texture, s.0.width, s.0.height, 0, 0, false));
        }
        if let Some(b) = as_switch_bitmap(handle) {
            let atlas = self.atlases.get(b.atlas_index)?;
            if atlas.texture == 0 {
                return None;
            }
            // Real atlas dims — right-sized dedicated atlases aren't 2048² (#42).
            let base_x = (b.u0 * atlas.width as f32).round() as u32;
            let base_y = (b.v0 * atlas.height as f32).round() as u32;
            return Some((atlas.texture, atlas.width, atlas.height, base_x, base_y, true));
        }
        None
    }

    /// Resolve a bitmap handle for use as a SAMPLED map (displacement map): its
    /// texture, content dims, and the UV sub-rect `[u0, v0, uw, vh]` to remap [0,1]
    /// into (identity for a standalone, the atlas sub-rect for a packed bitmap).
    fn resolve_bitmap_map(&self, handle: &BitmapHandle) -> Option<(GLuint, u32, u32, [f32; 4])> {
        if let Some(s) = as_standalone_bitmap(handle) {
            return Some((s.0.texture, s.0.width, s.0.height, [0.0, 0.0, 1.0, 1.0]));
        }
        if let Some(b) = as_switch_bitmap(handle) {
            let atlas = self.atlases.get(b.atlas_index)?;
            if atlas.texture == 0 {
                return None;
            }
            return Some((atlas.texture, b.width, b.height, [b.u0, b.v0, b.u1 - b.u0, b.v1 - b.v0]));
        }
        None
    }

    fn apply_filter_raw(
        &mut self,
        src_tex: GLuint,
        src_w: u32,
        src_h: u32,
        source_point: (u32, u32),
        source_size: (u32, u32),
        dst_tex: GLuint,
        dest_point: (i32, i32),
        filter: &Filter,
    ) -> bool {
        match filter {
            Filter::ColorMatrixFilter(args) => self.apply_color_matrix_raw(
                src_tex, src_w, src_h, source_point, source_size,
                dst_tex, dest_point, args,
            ),
            Filter::BlurFilter(args) => self.apply_blur_raw(
                src_tex, src_w, src_h, source_point, source_size,
                dst_tex, dest_point, args,
            ),
            Filter::GlowFilter(args) => self.apply_glow_or_drop_shadow_raw(
                src_tex, src_w, src_h, source_point, source_size,
                dst_tex, dest_point, args, (0.0, 0.0),
            ),
            Filter::DropShadowFilter(args) => {
                let inner = args.inner_glow_filter();
                let dist = args.distance.to_f32();
                let angle = args.angle.to_f32();
                let x = angle.cos() * dist;
                let y = angle.sin() * dist;
                self.apply_glow_or_drop_shadow_raw(
                    src_tex, src_w, src_h, source_point, source_size,
                    dst_tex, dest_point, &inner, (-x, -y),
                )
            }
            Filter::BevelFilter(args) => self.apply_bevel_raw(
                src_tex, src_w, src_h, source_point, source_size,
                dst_tex, dest_point, args,
            ),
            Filter::DisplacementMapFilter(args) => self.apply_displacement_map_raw(
                src_tex, src_w, src_h, source_point, source_size,
                dst_tex, dest_point, args,
            ),
            _ => false,
        }
    }

    /// Apply a DisplacementMapFilter (#42): a single full-quad pass that samples
    /// the displacement map (unit 1) to offset the source lookup (unit 0). No-op
    /// (returns false, so Ruffle keeps the unfiltered source) when the map bitmap
    /// isn't a resolvable standalone texture. Assumes the source content fills its
    /// texture (source_point 0,0 + full size), as cacheAsBitmap sources do.
    #[allow(clippy::too_many_arguments)]
    fn apply_displacement_map_raw(
        &mut self,
        src_tex: GLuint,
        src_w: u32,
        src_h: u32,
        source_point: (u32, u32),
        source_size: (u32, u32),
        dst_tex: GLuint,
        dest_point: (i32, i32),
        args: &DisplacementMapFilter,
    ) -> bool {
        let Some(map_handle) = args.map_bitmap.as_ref() else { return false };
        let Some((map_tex, map_w, map_h, map_remap)) = self.resolve_bitmap_map(map_handle) else {
            self.warn_once(b"displacement: map bitmap unresolved (skipped)\n\0");
            return false;
        };
        let prog = self.displacement_filter.program;
        let p = &self.displacement_filter;
        let (u_src_uv, u_color, u_map_remap, u_comp_x, u_comp_y, u_mode, u_scale, u_source_size, u_map_size, u_offset, u_viewscale) = (
            p.u_src_uv, p.u_color, p.u_map_remap, p.u_comp_x, p.u_comp_y, p.u_mode, p.u_scale,
            p.u_source_size, p.u_map_size, p.u_offset, p.u_viewscale,
        );
        let color = [
            f32::from(args.color.r) / 255.0,
            f32::from(args.color.g) / 255.0,
            f32::from(args.color.b) / 255.0,
            f32::from(args.color.a) / 255.0,
        ];
        let comp_x = args.component_x as i32;
        let comp_y = args.component_y as i32;
        let mode = match args.mode {
            DisplacementMapFilterMode::Wrap => 0,
            DisplacementMapFilterMode::Clamp => 1,
            DisplacementMapFilterMode::Ignore => 2,
            DisplacementMapFilterMode::Color => 3,
        };
        // viewscale defaults to 0 on a freshly-`Default`ed filter; a zero would
        // divide the map lookup to infinity, so fall back to 1.0.
        let vsx = if args.viewscale_x.abs() > 1e-6 { args.viewscale_x } else { 1.0 };
        let vsy = if args.viewscale_y.abs() > 1e-6 { args.viewscale_y } else { 1.0 };
        let (sw, sh) = (source_size.0 as f32, source_size.1 as f32);
        let (scale_x, scale_y) = (args.scale_x, args.scale_y);
        let (off_x, off_y) = (args.map_point.0 as f32, args.map_point.1 as f32);
        let (mw, mh) = (map_w as f32, map_h as f32);
        unsafe {
            glActiveTexture(GL_TEXTURE1);
            glBindTexture(GL_TEXTURE_2D, map_tex);
            glActiveTexture(GL_TEXTURE0);
        }
        let ok = self.draw_filter_pass(
            prog, u_src_uv,
            src_tex, src_w, src_h, source_point, source_size,
            dst_tex, dest_point.0, dest_point.1, source_size.0, source_size.1,
            move || unsafe {
                glUniform4f(u_color, color[0], color[1], color[2], color[3]);
                glUniform4f(u_map_remap, map_remap[0], map_remap[1], map_remap[2], map_remap[3]);
                glUniform1i(u_comp_x, comp_x);
                glUniform1i(u_comp_y, comp_y);
                glUniform1i(u_mode, mode);
                glUniform2f(u_scale, scale_x, scale_y);
                glUniform2f(u_source_size, sw, sh);
                glUniform2f(u_map_size, mw, mh);
                glUniform2f(u_offset, off_x, off_y);
                glUniform2f(u_viewscale, vsx, vsy);
            },
        );
        unsafe {
            glActiveTexture(GL_TEXTURE1);
            glBindTexture(GL_TEXTURE_2D, 0);
            glActiveTexture(GL_TEXTURE0);
        }
        ok
    }

    /// Pixel dimensions of whatever we're currently rendering into: the main
    /// framebuffer normally, or the active offscreen FBO when replaying
    /// commands into a cache/blend/mask texture.
    fn current_target_dims(&self) -> (u32, u32) {
        match self.offscreen_dims {
            Some((w, h)) => (w, h),
            None => (self.dimensions.width, self.dimensions.height),
        }
    }

    /// Draw a standalone texture covering the whole current target (full-screen
    /// quad), reusing the proven standalone-`render_bitmap` path (bitmap shader
    /// + bitmap_vao + Y-flip-aware `world_matrix`), but with a caller-chosen GL
    /// blend state set just before the draw. Always restores the default
    /// premultiplied-over blend afterwards. `tex` is assumed premultiplied with
    /// texel row 0 = Flash top (every offscreen render we produce is).
    fn draw_fullscreen_texture(&mut self, tex: GLuint, tw: u32, th: u32, set_blend: impl FnOnce()) {
        let scaled = Matrix::scale(tw as f32, th as f32);
        self.note_draw_extent(&scaled);
        let world = self.world_matrix(&scaled);
        const IDENT_MULT: [f32; 4] = [1.0, 1.0, 1.0, 1.0];
        const IDENT_ADD: [f32; 4] = [0.0, 0.0, 0.0, 0.0];
        let uv_remap = [0.0, 0.0, 1.0, 1.0];
        self.use_bitmap(&world, &IDENT_MULT, &IDENT_ADD, tex, &uv_remap, 0, true);
        self.gl_state.bind_vao(self.bitmap_vao);
        self.draw_calls_this_window = self.draw_calls_this_window.saturating_add(1);
        set_blend();
        unsafe {
            glDrawArrays(GL_TRIANGLES, 0, 6);
            // Restore the main-pass blend so following draws composite normally.
            glBlendEquation(GL_FUNC_ADD);
            glBlendFunc(GL_SRC_ALPHA, GL_ONE_MINUS_SRC_ALPHA);
        }
    }

    /// Composite a soft alpha mask: `result_tex` ← maskee × mask.alpha. Both
    /// inputs and the output share the offscreen "row 0 = Flash top" layout, so
    /// the combine FBO pass samples them straight. Returns false on FBO failure.
    fn composite_alpha_mask(
        &mut self,
        maskee_tex: GLuint,
        mask_tex: GLuint,
        result_tex: GLuint,
        w: u32,
        h: u32,
    ) -> bool {
        unsafe {
            glActiveTexture(GL_TEXTURE1);
            glBindTexture(GL_TEXTURE_2D, mask_tex);
            glActiveTexture(GL_TEXTURE0);
        }
        let ok = self.draw_filter_pass(
            self.alpha_mask_prog.program,
            self.alpha_mask_prog.u_src_uv,
            maskee_tex, w, h, (0, 0), (w, h),
            result_tex, 0, 0, w, h,
            || {},
        );
        unsafe {
            glActiveTexture(GL_TEXTURE1);
            glBindTexture(GL_TEXTURE_2D, 0);
            glActiveTexture(GL_TEXTURE0);
        }
        ok
    }

    /// Run a complex blend (multiply/overlay/…) straight onto the current
    /// target: a full-screen quad samples the backdrop snapshot (`parent_tex`,
    /// unit 0) and the freshly-rendered blend group (`current_tex`, unit 1),
    /// outputs the full composite, and overwrites the target with blending
    /// DISABLED. `flip` (0/1) flips the current sampler's V on the main
    /// framebuffer (Y-flipped) vs an offscreen target (not flipped).
    fn composite_complex_to_current(
        &mut self,
        parent_tex: GLuint,
        current_tex: GLuint,
        w: u32,
        h: u32,
        mode: i32,
        flip: f32,
    ) {
        // Raw binds below (backdrop on unit 0, group on unit 1), so unit 0 still
        // carries whatever the last bitmap draw chose. Unit 1 never gets a
        // sampler, so it keeps the texture's own LINEAR either way.
        self.gl_state.clear_sampler();
        let prog = self.complex_blend_prog.program;
        let u_src_uv = self.complex_blend_prog.u_src_uv;
        let u_blend_mode = self.complex_blend_prog.u_blend_mode;
        let u_current_flip = self.complex_blend_prog.u_current_flip;
        let u_cur_zoom = self.complex_blend_prog.u_cur_zoom;
        // THE ZOOM HAS TO BE UNDONE HERE BY HAND (issue #101).
        //
        // Every other draw is magnified by `world_matrix`. This one is not: the
        // group was rasterized into a temp with `offscreen_dims` set, so it came
        // out at plain screen scale, and the composite is a raw NDC quad through
        // `FILTER_VERT`, which has no world matrix at all. Left alone, a
        // Multiply or Overlay group stays at 100% and unpanned over a picture
        // magnified around the screen centre -- a ghost in the wrong place and a
        // hole where it belonged, growing to a full screen width at 500%.
        //
        // So the shader samples the GROUP through the inverse mapping while the
        // backdrop stays 1:1 (it is a snapshot of the already-zoomed
        // framebuffer). `flip` is 1 only on the main framebuffer, which is the
        // only target the zoom exists on.
        let (cz, cpx, cpy) = if flip != 0.0 && self.game_layer && game_zoom_percent() != 100 {
            let z = game_zoom_percent() as f32 / 100.0;
            let (ox, oy) = game_pan();
            (
                z,
                ox as f32 / w.max(1) as f32,
                oy as f32 / h.max(1) as f32,
            )
        } else {
            (1.0, 0.0, 0.0)
        };
        unsafe {
            glViewport(0, 0, w as GLsizei, h as GLsizei);
            glDisable(GL_BLEND);
            // Keep the ACTIVE MASK in force. This composite is a FULLSCREEN
            // quad, so disabling the stencil painted the blended result over the
            // whole target instead of the masked region — and it left the test
            // DISABLED on the way out, so every later maskee in the frame drew
            // unclipped too (`mask_push` is the only place that re-enables it).
            // Agent P's level-select runs 4 complex blends per frame inside 6
            // masks, which is why its tiles washed out. Stencil WRITES stay
            // masked off so the composite cannot perturb the coverage counts.
            glColorMask(GL_TRUE, GL_TRUE, GL_TRUE, GL_TRUE);
            glStencilMask(0);
            glUseProgram(prog);
            glActiveTexture(GL_TEXTURE0);
            glBindTexture(GL_TEXTURE_2D, parent_tex);
            glActiveTexture(GL_TEXTURE1);
            glBindTexture(GL_TEXTURE_2D, current_tex);
            glUniform4f(u_src_uv, 0.0, 0.0, 1.0, 1.0);
            glUniform1i(u_blend_mode, mode);
            glUniform1f(u_current_flip, flip);
            glUniform3f(u_cur_zoom, cz, cpx, cpy);
            glBindVertexArray(self.bitmap_vao);
            glDrawArrays(GL_TRIANGLES, 0, 6);
            glBindVertexArray(0);
            glActiveTexture(GL_TEXTURE1);
            glBindTexture(GL_TEXTURE_2D, 0);
            glActiveTexture(GL_TEXTURE0);
            glBindTexture(GL_TEXTURE_2D, 0);
            glEnable(GL_BLEND);
            glBlendEquation(GL_FUNC_ADD);
            glBlendFunc(GL_SRC_ALPHA, GL_ONE_MINUS_SRC_ALPHA);
        }
        // Put the stencil back in sync with `self.mask` (colour mask, func, and
        // enable/disable), exactly like `render_commands_to_texture` does on its
        // way out. Without this the composite's raw GL state leaked into the
        // rest of the frame.
        self.mask_restore_gl();
        self.draw_calls_this_window = self.draw_calls_this_window.saturating_add(1);
        // The direct glUseProgram/bind above bypassed the state cache.
        self.gl_state.invalidate();
    }

    fn use_solid(&self, world: &[GLfloat; 9], mult: &[f32; 4], add: &[f32; 4]) {
        self.gl_state.use_program(self.solid.program);
        unsafe {
            glUniformMatrix3fv(self.solid.u_world, 1, GL_FALSE, world.as_ptr());
            glUniform4f(self.solid.u_mult, mult[0], mult[1], mult[2], mult[3]);
            glUniform4f(self.solid.u_add, add[0], add[1], add[2], add[3]);
        }
    }

    /// The sampler a SWF bitmap draw should use: `sampler_sharp` to point-sample,
    /// or 0 to keep the texture's LINEAR.
    ///
    /// `smoothing` is Ruffle's per-bitmap flag, which a pixel-art SWF sets false
    /// on purpose and which this backend used to discard. Returning 0 rather
    /// than a second "smooth" sampler object is deliberate: the SMOOTH path then
    /// issues no sampler state at all and is byte-for-byte what shipped before,
    /// and a failed `glGenSamplers` degrades to that same old picture instead of
    /// to a black one.
    fn sampler_for(&self, smoothing: bool) -> GLuint {
        let sharp = match pixel_filter() {
            TEXTURE_FILTER_SMOOTH => false,
            TEXTURE_FILTER_SHARP => true,
            // TEXTURE_FILTER_AUTO, and anything unknown: what the SWF asked for.
            TEXTURE_FILTER_AUTO | _ => !smoothing,
        };
        if sharp {
            self.sampler_sharp
        } else {
            0
        }
    }

    /// `premult` must be true exactly when `tex` holds premultiplied alpha,
    /// i.e. for the standalone (FBO-backed) textures Ruffle hands back for
    /// cacheAsBitmap / filtered objects / `BitmapData.draw`. Everything else
    /// here — the SWF atlas, banner, covers, glyph runs — is straight alpha.
    /// Getting this wrong is not cosmetic: see the `BITMAP_FRAG` doc comment.
    fn use_bitmap(
        &self,
        world: &[GLfloat; 9],
        mult: &[f32; 4],
        add: &[f32; 4],
        tex: GLuint,
        uv_remap: &[f32; 4],
        sampler_obj: GLuint,
        premult: bool,
    ) {
        // Sampler UNIFORM (u_tex = 0) set once at program link; no per-draw
        // glUniform1i(u_tex) needed here. `sampler_obj` is the unrelated sampler
        // OBJECT, and it is a per-draw choice on purpose: this one helper draws
        // SWF bitmaps out of a shared atlas AND launcher art (banner, covers,
        // glyph runs) out of its own textures, and only the first kind may ever
        // be point-sampled. Non-SWF callers pass 0 and keep LINEAR.
        self.gl_state.use_program(self.bitmap_prog.program);
        self.gl_state.bind_texture_unit0(tex);
        self.gl_state.bind_sampler(sampler_obj);
        self.gl_state.set_premult(self.bitmap_prog.u_premult, premult);
        unsafe {
            glUniformMatrix3fv(self.bitmap_prog.u_world, 1, GL_FALSE, world.as_ptr());
            glUniform4f(self.bitmap_prog.u_mult, mult[0], mult[1], mult[2], mult[3]);
            glUniform4f(self.bitmap_prog.u_add, add[0], add[1], add[2], add[3]);
            glUniform4f(
                self.bitmap_prog.u_uv_remap,
                uv_remap[0], uv_remap[1], uv_remap[2], uv_remap[3],
            );
        }
    }

    fn use_shape_bitmap(
        &self,
        world: &[GLfloat; 9],
        mult: &[f32; 4],
        add: &[f32; 4],
        tex: GLuint,
        uv_matrix: &[GLfloat; 9],
        uv_remap: &[f32; 4],
        uv_inset: &[f32; 2],
        is_repeating: bool,
        sampler_obj: GLuint,
    ) {
        // Atlas texture parameters are STILL set once at atlas creation; no per-
        // draw glTexParameteri (avoids per-frame state churn that bisection
        // on 2026-05-24 implicated in a Mario 63 driver-side issue).
        // u_wrap_mode and u_tex sampler are routed through the GL state
        // cache so identical-state runs of draws (very common for atlas
        // bitmap fills) only hit the driver once.
        //
        // The per-draw filter rides a sampler OBJECT for the same reason: it is
        // one bind through that cache instead of texparam churn, and it leaves
        // the shared atlas untouched, which glTexParameteri could not — the next
        // draw out of the same atlas usually wants the other filter.
        self.gl_state.use_program(self.shape_bitmap_prog.program);
        self.gl_state.bind_texture_unit0(tex);
        self.gl_state.bind_sampler(sampler_obj);
        // u_wrap_mode: 0 = clamp (default for non-repeating fills),
        // 1 = fract (for tile/repeat fills like Mario 63 ground).
        self.gl_state.set_wrap_mode(
            self.shape_bitmap_prog.u_wrap_mode,
            if is_repeating { 1 } else { 0 },
        );
        unsafe {
            glUniformMatrix3fv(self.shape_bitmap_prog.u_world, 1, GL_FALSE, world.as_ptr());
            glUniform4f(self.shape_bitmap_prog.u_mult, mult[0], mult[1], mult[2], mult[3]);
            glUniform4f(self.shape_bitmap_prog.u_add, add[0], add[1], add[2], add[3]);
            glUniformMatrix3fv(self.shape_bitmap_prog.u_uv, 1, GL_FALSE, uv_matrix.as_ptr());
            glUniform4f(
                self.shape_bitmap_prog.u_uv_remap,
                uv_remap[0], uv_remap[1], uv_remap[2], uv_remap[3],
            );
            glUniform2f(self.shape_bitmap_prog.u_uv_inset, uv_inset[0], uv_inset[1]);
        }
    }

    fn use_gradient(
        &self,
        world: &[GLfloat; 9],
        mult: &[f32; 4],
        add: &[f32; 4],
        tex: GLuint,
        local_matrix: &[GLfloat; 9],
        kind: i32,
        spread: i32,
        focal: f32,
    ) {
        self.gl_state.use_program(self.gradient_prog.program);
        self.gl_state.bind_texture_unit0(tex);
        // Said explicitly because the sampler on unit 0 is whatever the last
        // bitmap draw left there. The ramp is a 256x1 texture whose entire job
        // is to be interpolated; point-sampling it turns every gradient in the
        // frame into 256 hard bands.
        self.gl_state.bind_sampler(0);
        unsafe {
            glUniformMatrix3fv(self.gradient_prog.u_world, 1, GL_FALSE, world.as_ptr());
            glUniform4f(self.gradient_prog.u_mult, mult[0], mult[1], mult[2], mult[3]);
            glUniform4f(self.gradient_prog.u_add, add[0], add[1], add[2], add[3]);
            glUniformMatrix3fv(self.gradient_prog.u_grad_local, 1, GL_FALSE, local_matrix.as_ptr());
            glUniform1i(self.gradient_prog.u_grad_kind, kind);
            glUniform1i(self.gradient_prog.u_grad_spread, spread);
            glUniform1f(self.gradient_prog.u_grad_focal, focal);
        }
    }

    /// Pack a bitmap's RGBA pixels into one of our atlases. Returns the
    /// SwitchBitmapHandle metadata, or None if the bitmap is too big for
    /// the atlas size.
    fn pack_into_atlas(
        &mut self,
        pixels: &[u8],
        width: u32,
        height: u32,
        reloadable: bool,
    ) -> Option<SwitchBitmapHandle> {
        // A bitmap bigger than the atlas in either axis can NEVER be packed.
        // Bail out here — before the new-atlas path below would allocate a
        // doomed 16 MB GPU texture (`Atlas::new` → glGenTextures + glTexImage2D)
        // only for `pack` to then fail. That wasted allocation, under GPU memory
        // pressure, is what preceded a Mesa NULL-deref native crash (DataAbort,
        // FAR=0x98) on haunt-the-house's 3400x1600 background (atlas is 2048²).
        // Returning None makes register_bitmap report TooLarge cleanly, so
        // Ruffle no-ops that one oversized bitmap instead of taking down the app.
        if width > ATLAS_SIZE || height > ATLAS_SIZE {
            self.warn_once(b"pack_into_atlas: bitmap exceeds 2048 atlas, skipped (no crash)\n\0");
            return None;
        }
        for (idx, atlas) in self.atlases.iter_mut().enumerate() {
            if atlas.texture == 0 {
                continue; // freed/dead slot — reused in the new-atlas path below
            }
            // Evictable SWF images and everything else never share an atlas.
            if atlas.reloadable != reloadable {
                continue;
            }
            if let Some((x, y, slot)) = atlas.pack(width, height) {
                atlas.upload_region_padded(x, y, width, height, pixels);
                atlas.live += 1; // released when the handle's AtlasTicket drops
                // UVs are fractions of THIS atlas' size (atlases now vary: shared
                // 2048² vs right-sized dedicated ones, issue #56b).
                let (aw, ah) = (atlas.width as f32, atlas.height as f32);
                return Some(SwitchBitmapHandle {
                    atlas_index: idx,
                    u0: x as f32 / aw,
                    v0: y as f32 / ah,
                    u1: (x + width) as f32 / aw,
                    v1: (y + height) as f32 / ah,
                    width,
                    height,
                    ticket: Some(Arc::new(AtlasTicket { atlas_index: idx, slot })),
                });
            }
        }
        // No room in any shared atlas. A bitmap large in either axis gets its own
        // RIGHT-SIZED atlas (~the bitmap's bytes, not a full 16 MB 2048²) so games
        // that spam big offscreen surfaces use ~half the memory (issue #56b). It
        // stays atlas-backed (shape fills keep working — unlike the standalone
        // path). Small bitmaps still get a shared 2048² so more can pack into it.
        let big = width > ATLAS_SIZE / 2 || height > ATLAS_SIZE / 2;
        let new_atlas = if big {
            Atlas::new_wh(width + 2 * ATLAS_PAD, height + 2 * ATLAS_PAD)
        } else {
            Atlas::new(ATLAS_SIZE)
        };
        // No room for a new atlas: say so, and let register_bitmap fall back to
        // a standalone texture of the bitmap's own size, which is checked and
        // far smaller than the 16 MB an atlas asks for. Never an atlas without
        // storage (see `Atlas::new_wh`).
        self.atlas_created = self.atlas_created.wrapping_add(1);
        let Some(mut atlas) = new_atlas else {
            self.atlas_alloc_failures = self.atlas_alloc_failures.wrapping_add(1);
            if self.atlas_alloc_failures <= 8 || self.atlas_alloc_failures % 256 == 0 {
                let msg = std::format!(
                    "atlas: no GPU memory for a new {} atlas ({}x{} bitmap), falling back (failures={})\n",
                    if big { "right-sized" } else { "2048" },
                    width, height, self.atlas_alloc_failures,
                );
                let mut b = msg.into_bytes();
                b.push(0);
                unsafe { ruffle_log_cstr(b.as_ptr() as *const _) };
            }
            return None;
        };
        let Some((x, y, slot)) = atlas.pack(width, height) else {
            return None;
        };
        atlas.upload_region_padded(x, y, width, height, pixels);
        atlas.live = 1;
        atlas.reloadable = reloadable;
        let (aw, ah) = (atlas.width as f32, atlas.height as f32);
        let atlas_bytes = atlas.width as u64 * atlas.height as u64 * 4;
        let bytes_mb = atlas_bytes / (1024 * 1024);
        // Track live big-surface bytes so the release drain can subtract them and
        // the budget check upstream can refuse before the heap runs out (#56b OOM).
        if big {
            self.big_atlas_live_bytes = self.big_atlas_live_bytes.saturating_add(atlas_bytes);
            self.big_atlas_peak_bytes = self.big_atlas_peak_bytes.max(self.big_atlas_live_bytes);
            self.big_atlas_alloc_total = self.big_atlas_alloc_total.wrapping_add(1);
        }
        let new_atlas_index = match self.atlases.iter().position(|a| a.texture == 0) {
            Some(dead) => {
                self.atlases[dead] = atlas; // old dead Atlas (texture 0) dropped, no-op
                dead
            }
            None => {
                self.atlases.push(atlas);
                self.atlases.len() - 1
            }
        };
        // Not for every dedicated one: Super Bowser World makes ~5 a frame
        // (its ground strips) and this line alone was 1.4 % of its frames
        // (2026-10-08). The `evict:` line counts them all.
        if !big || self.big_atlas_alloc_total <= 64 || self.big_atlas_alloc_total % 500 == 0 {
            let msg = std::format!(
                "atlas: allocating #{} ({} MB) for {}x{} [big live={}MB peak={}MB alloc={} free={}]\n",
                new_atlas_index, bytes_mb, width, height,
                self.big_atlas_live_bytes / (1024 * 1024),
                self.big_atlas_peak_bytes / (1024 * 1024),
                self.big_atlas_alloc_total, self.big_atlas_free_total,
            );
            let mut bytes = msg.into_bytes();
            bytes.push(0);
            unsafe { ruffle_log_cstr(bytes.as_ptr() as *const _) };
        }
        Some(SwitchBitmapHandle {
            atlas_index: new_atlas_index,
            u0: x as f32 / aw,
            v0: y as f32 / ah,
            u1: (x + width) as f32 / aw,
            v1: (y + height) as f32 / ah,
            width,
            height,
            ticket: Some(Arc::new(AtlasTicket { atlas_index: new_atlas_index, slot })),
        })
    }

    fn warn_once(&mut self, msg: &[u8]) {
        if self.warned_unsupported < 8 {
            self.warned_unsupported += 1;
            log(msg);
        }
    }

    /// Keep a texture write instead of sending it (see [`PendingUpload`]).
    ///
    /// A write to a DIFFERENT target flushes the held one first: two targets
    /// cannot both wait, and the burst we are collapsing is always the same
    /// texture written over and over. Rows are copied tightly packed, so the
    /// flush never needs the source stride.
    #[allow(clippy::too_many_arguments)]
    fn hold_upload(
        &mut self,
        texture: GLuint,
        keep: Option<std::sync::Arc<StandaloneTexture>>,
        atlas_index: usize,
        dst_x: u32,
        dst_y: u32,
        w: u32,
        h: u32,
        src_row_px: u32,
        src_x: u32,
        src_y: u32,
        src: &[u8],
    ) {
        // The destination RECTANGLE is part of the identity, not just the
        // texture. On the atlas path `texture` is always 0, so without the rect
        // two different BitmapData packed into the same atlas looked like the
        // same target: no flush, then `data.clear()` below threw the first
        // one's pixels away. It never reached the GPU, and Ruffle had already
        // marked the bitmap clean, so the tile simply stopped updating -- with
        // no GL error and nothing in a log. The #89 case (same texture, same
        // full rectangle, once per decoded video frame) still coalesces; every
        // other pair falls back to a flush, which is what it did before.
        let same_target = match &self.pending_upload {
            Some(p) => {
                p.texture == texture
                    && p.atlas_index == atlas_index
                    && p.dst_x == dst_x
                    && p.dst_y == dst_y
                    && p.w == w
                    && p.h == h
            }
            None => false,
        };
        if !same_target {
            self.flush_pending_upload();
        }
        // Reuse a buffer rather than allocating: the video path lands here five
        // or six times per frame at 3.7 MB a piece, and allocating each of those
        // is half the churn we are here to stop. Either the write being replaced
        // lends its buffer, or the last flush left one in the scratch slot.
        let mut data = match self.pending_upload.take() {
            Some(p) => p.data,
            None => core::mem::take(&mut self.upload_scratch),
        };
        let row_bytes = (w as usize) * 4;
        data.clear();
        data.reserve(row_bytes * h as usize);
        let src_stride = src_row_px as usize * 4;
        for row in 0..h as usize {
            let start = (src_y as usize + row) * src_stride + (src_x as usize) * 4;
            let end = start + row_bytes;
            if end <= src.len() {
                data.extend_from_slice(&src[start..end]);
            } else {
                // Short source row: pad rather than skip, so the rectangle we
                // promise GL is always fully backed.
                data.resize(row_bytes * (row + 1), 0);
            }
        }
        self.pending_upload = Some(PendingUpload {
            texture,
            keep,
            atlas_index,
            dst_x,
            dst_y,
            w,
            h,
            data,
        });
    }

    /// Send the held write, if any. Called before anything can read a texture:
    /// every draw goes through `submit_frame` or `render_offscreen`, and every
    /// pixel read-back through `resolve_sync_handle`.
    fn flush_pending_upload(&mut self) {
        let Some(p) = self.pending_upload.take() else {
            return;
        };
        // The id comes from the texture we KEPT ALIVE, not from a copy taken
        // when the write was held: that is the whole point of the `keep` field,
        // and reading it here is what makes the guarantee load-bearing rather
        // than incidental.
        if let Some(keep) = p.keep.as_ref() {
            unsafe {
                glBindTexture(GL_TEXTURE_2D, keep.texture);
                glPixelStorei(GL_UNPACK_ALIGNMENT, 1);
                glTexSubImage2D(
                    GL_TEXTURE_2D, 0,
                    p.dst_x as GLint, p.dst_y as GLint,
                    p.w as GLsizei, p.h as GLsizei,
                    GL_RGBA, GL_UNSIGNED_BYTE,
                    p.data.as_ptr() as *const _,
                );
                glBindTexture(GL_TEXTURE_2D, 0);
            }
        } else if let Some(atlas) = self.atlases.get(p.atlas_index) {
            // Rows are already tight, so the source row length IS the width.
            atlas.upload_region(p.dst_x, p.dst_y, p.w, p.h, p.w, &p.data);
        }
        // Keep the buffer for the next write instead of freeing it.
        self.upload_scratch = p.data;
    }

    /// Snapshot the raw counters that feed `FrameBreakdown`, so a per-frame
    /// delta can be taken across `submit_frame`. The window counters
    /// (draw_calls/blend/pushmask/masked_draw) only grow within a single frame
    /// except on the heartbeat frame, where the heartbeat zeroes them — see the
    /// caveat on `frame_snapshot`.
    fn frame_counters(&self) -> FrameBreakdown {
        FrameBreakdown {
            draw_calls: self.draw_calls_this_window,
            offscreen: self.render_offscreen_calls,
            filter: self.apply_filter_calls,
            resolve: self.resolve_sync_calls,
            bmp_uploads: self.bitmaps_registered,
            shape_regs: self.shapes_registered,
            blend: self.blend_window,
            pushmask: self.push_mask_window,
            masked_draw: self.masked_draw_window,
            cache_entries: 0,
            filter_chains: 0,
            filter_budget: 0,
        }
    }

    /// Emit a one-line breakdown for a frame that blew the FPS budget. Called
    /// from lib.rs's `render_frame_with_dt` once it knows the frame's wall time
    /// (tick + render). `last_frame` was filled at the end of `submit_frame`.
    /// Timings are microseconds. This fires only on slow frames, so it never
    /// floods nxlink during smooth play but captures every spike with the
    /// activity that caused it.
    pub fn log_slow_frame(&self, total_us: u64, tick_us: u64, render_us: u64) {
        let fb = self.last_frame;
        // Backend-primitive time during this frame's tick (LAST_* snapshotted at
        // submit_frame). primOffs = render_offscreen incl. draw() repatriation;
        // primBmp = bitmap register/upload; primRes = copyPixels resolve. tick
        // huge + these ~0 ⇒ pure AVM2 (upstream); one dominating ⇒ our backend.
        let tick_freq = unsafe { ruffle_tick_freq() };
        let to_us = |t: u64| if tick_freq > 0 { t.saturating_mul(1_000_000) / tick_freq } else { 0 };
        let prim_offs = to_us(PRIM_OFFSCREEN_LAST.load(std::sync::atomic::Ordering::Relaxed));
        let prim_bmp = to_us(PRIM_BMPUP_LAST.load(std::sync::atomic::Ordering::Relaxed));
        let prim_res = to_us(PRIM_RESOLVE_LAST.load(std::sync::atomic::Ordering::Relaxed));
        let off_alloc = to_us(PRIM_OFF_ALLOC_LAST.load(std::sync::atomic::Ordering::Relaxed));
        let off_render = to_us(PRIM_OFF_RENDER_LAST.load(std::sync::atomic::Ordering::Relaxed));
        let off_readback = to_us(PRIM_OFF_READBACK_LAST.load(std::sync::atomic::Ordering::Relaxed));
        let off_upload = to_us(PRIM_OFF_UPLOAD_LAST.load(std::sync::atomic::Ordering::Relaxed));
        let off_n = PRIM_OFF_N_LAST.load(std::sync::atomic::Ordering::Relaxed);
        let off_pix = PRIM_OFF_PIX_LAST.load(std::sync::atomic::Ordering::Relaxed);
        // The cache_entries loop and, inside it, the render-target switches
        // alone. `cacheUs` is the share of `render` the filtered-object path
        // really owns; `rtbindUs / rtBind` is what one switch costs; the
        // difference is drawing. Before these two, a Mario 63 8-13 frame printed
        // `render 68000us` with every other sub-timer at zero.
        let cache_us = to_us(PRIM_CACHE_LAST.load(std::sync::atomic::Ordering::Relaxed));
        let rtbind_us = to_us(PRIM_RTBIND_LAST.load(std::sync::atomic::Ordering::Relaxed));
        // Round 2: `cacheUs` split into step 1 vs the chain, plus the texture
        // creation that happens outside the loop. `budget=` labels the frame so
        // the alternating probe can be bucketed.
        let content_us = to_us(PRIM_CONTENT_LAST.load(std::sync::atomic::Ordering::Relaxed));
        let filtchain_us = to_us(PRIM_FILTCHAIN_LAST.load(std::sync::atomic::Ordering::Relaxed));
        let mktex_us = to_us(PRIM_MKTEX_LAST.load(std::sync::atomic::Ordering::Relaxed));
        let mktex_n = PRIM_MKTEX_N_LAST.load(std::sync::atomic::Ordering::Relaxed);
        // Blend attribution (see the BLEND_* statics): blendMs is the ONLY timer
        // here that covers work done inside submit_frame, so it is the one that
        // can actually account for a large `render` with everything else at zero.
        let blend_us = to_us(BLEND_TICKS_FRAME.load(std::sync::atomic::Ordering::Relaxed));
        let blend_n_triv = BLEND_N_TRIVIAL_FRAME.load(std::sync::atomic::Ordering::Relaxed);
        let blend_n_cx = BLEND_N_COMPLEX_FRAME.load(std::sync::atomic::Ordering::Relaxed);
        let blend_pct = if render_us > 0 { blend_us.saturating_mul(100) / render_us } else { 0 };
        // Periodic-spike probes (2026-08-24). `swf=` is how many SWF frames the
        // tick actually ran: 1 means the game is in slow motion, 5 means a
        // catch-up burst. `gc=` is where the collector stopped (SLEEP/MARK/
        // MARKD/SWEEP) and `gcMB=` what the arena holds — a spike landing on
        // MARK/SWEEP while the quiet frames sit on SLEEP is the collector.
        // `rtBind=` is render-target rebinds, the unit the filter cost is
        // actually paid in.
        let (swf_frames, gc_phase, gc_alloc, gc_us) = ruffle_core::flashnx_gc_probe();
        let gc_name = match gc_phase {
            0 => "SLEEP",
            1 => "MARK",
            2 => "MARKD",
            _ => "SWEEP",
        };
        let rt_binds = RT_BIND_FRAME.load(std::sync::atomic::Ordering::Relaxed);
        // Per-frame allocator traffic, differenced in submit_frame (which runs
        // every frame) rather than here (which runs only on slow ones, so the
        // delta would silently span the gap). `free=` next to `gcUs=` is the
        // whole point: it says whether the sweep's cost is the frees it issues
        // or the traversal that issues them.
        let d_alloc = ALLOC_D_FRAME.load(std::sync::atomic::Ordering::Relaxed);
        let d_free = FREE_D_FRAME.load(std::sync::atomic::Ordering::Relaxed);
        let alloc_us = to_us(ALLOC_T_FRAME.load(std::sync::atomic::Ordering::Relaxed));
        let free_us = to_us(FREE_T_FRAME.load(std::sync::atomic::Ordering::Relaxed));
        let d_small = SMALL_D_FRAME.load(std::sync::atomic::Ordering::Relaxed);
        let small_pct = if d_alloc > 0 { d_small.saturating_mul(100) / d_alloc } else { 0 };
        // Share of the frame that newlib's malloc/free own outright.
        let heap_pct = if total_us > 0 {
            (alloc_us + free_us).saturating_mul(100) / total_us
        } else {
            0
        };
        // Texture churn (2026-09-23). `make=`/`del=` count and time every
        // standalone texture made or freed this frame, all origins; `bf=` is
        // `begin_frame`, inside cacheUs, where the filter pool's TTL deletions
        // land; `evict=` is how many it let go. The `bo*=` fields are the libdrm
        // side: buffer-object creates and deletes with their time inside libdrm,
        // cache hits, the zeroing a hit costs, hits refused because the GPU still
        // used the BO, fence IPCs, and what the cache holds. Since 2026-09-24:
        // `boBig=` colour BOs too big to cache, made fresh (count/time/KB),
        // `boOther=` BOs of another kind, `boSlack=` hits served by a larger BO,
        // and `ab=` the profiling build's A/B window (see `ruffle_ab_regime`).
        use std::sync::atomic::Ordering::Relaxed as R;
        let b = self.boc_frame;
        let make_n = TEX_MAKE_N_LAST.load(R);
        let make_us = to_us(TEX_MAKE_T_LAST.load(R));
        let del_n = TEX_DEL_N_LAST.load(R);
        let del_us = to_us(TEX_DEL_T_LAST.load(R));
        let bf_us = to_us(TEX_BF_T_LAST.load(R));
        let evict_n = TEX_EVICT_N_LAST.load(R);
        let st_live = LIVE_STANDALONE.load(R);
        // AVM1 (2026-09-23): actions run, time decoding them, time in bytecode
        // at all; then the clip-reference cache: on or off (`mcref.off`),
        // references handed out, how many it served, time spent.
        let (avm1_actions, avm1_decode, avm1_total) = ruffle_core::flashnx_avm1_probe();
        let (mcref_on, mcref_calls, mcref_hits, mcref_ticks) = ruffle_core::flashnx_mcref_probe();
        let prev_swap_us = to_us(unsafe { flashnx_prev_swap_ticks() });
        let prev_period_us = to_us(unsafe { flashnx_prev_period_ticks() });
        let msg = std::format!(
            "SLOW f{} {}us (tick {}us render {}us) swf={} gc={} gcUs={} gcMB={} alloc={}/{}us({}%sm) free={}/{}us heap={}%  rtBind={} cacheUs={}us rtbindUs={}us contentUs={}us filtUs={}us mktex={}/{}us budget={} primOffs={}us primBmp={}us primRes={}us dc={} offs={} filt={}({}chains) resolve={} bmpUp={} shpReg={} blend={} pmask={} mdraw={} cacheEnt={} | offN={} offPix={} alloc={}us render={}us readback={}us upload={}us | blendUs={} ({}% of render) blendTriv={} blendCx={} | make={}/{}us del={}/{}us bf={}us evict={} stTex={} pool={} prevSwapUs={} prevPeriodUs={} | boNew={}/{}us boHit={} boZeroUs={} boDel={}/{}us boKept={} boEvict={} boBusy={} boIpc={} boFail={} boRetryOk={} boc={}/{}KB boBig={}/{}us/{}KB boOther={} boSlack={} ab={} | avm1={}/{}us/{}us mcref={} {}/{}/{}us\n",
            self.frame_count,
            total_us, tick_us, render_us,
            swf_frames, gc_name, gc_us, gc_alloc / (1024 * 1024), d_alloc, alloc_us, small_pct, d_free, free_us, heap_pct, rt_binds,
            cache_us, rtbind_us, content_us, filtchain_us, mktex_n, mktex_us, fb.filter_budget,
            prim_offs, prim_bmp, prim_res,
            fb.draw_calls, fb.offscreen, fb.filter, fb.filter_chains,
            fb.resolve, fb.bmp_uploads, fb.shape_regs,
            fb.blend, fb.pushmask, fb.masked_draw, fb.cache_entries,
            off_n, off_pix, off_alloc, off_render, off_readback, off_upload,
            blend_us, blend_pct, blend_n_triv, blend_n_cx,
            make_n, make_us, del_n, del_us, bf_us, evict_n,
            st_live, self.filter_tex_pool.len(), prev_swap_us, prev_period_us,
            b[BOC_NEW_N], to_us(b[BOC_NEW_T]), b[BOC_HIT], to_us(b[BOC_ZERO_T]),
            b[BOC_DEL_N], to_us(b[BOC_DEL_T]), b[BOC_DEL_CACHED], b[BOC_EVICT],
            b[BOC_BUSY], b[BOC_IPC], b[BOC_NEW_FAIL], b[BOC_RETRY_OK],
            b[BOC_HELD_N], b[BOC_HELD_BYTES] / 1024,
            b[BOC_BIG_N], to_us(b[BOC_BIG_T]), b[BOC_BIG_BYTES] / 1024, b[BOC_OTHER_N],
            b[BOC_SLACK_HIT],
            crate::AB_REGIME.load(R),
            avm1_actions, to_us(avm1_decode), to_us(avm1_total),
            if mcref_on { "on" } else { "off" }, mcref_calls, mcref_hits, to_us(mcref_ticks),
        );
        let mut bytes = msg.into_bytes();
        bytes.push(0);
        unsafe { ruffle_log_cstr(bytes.as_ptr() as *const _) };
    }

    /// Draw a small white crosshair at the given screen pixel position.
    /// Intended to be called *after* `submit_frame` has returned so it
    /// overlays the player's rendering rather than getting cleared away.
    /// Re-binds the GL state we'd left in a fresh state at end of submit.
    pub fn draw_cursor_overlay(&mut self, x: f32, y: f32, clicked: bool) {
        const BAR_W: f32 = 24.0;
        const BAR_H: f32 = 4.0;
        // Black outline thickness on each side, so the "+" stays visible over
        // both light and dark game content.
        const OUTLINE: f32 = 2.0;
        // Red when clicked, white otherwise. Helps confirm clicks register.
        let color = if clicked {
            swf::Color::from_rgb(0xFF1744, 255)
        } else {
            swf::Color::from_rgb(0xFFFFFF, 255)
        };
        let outline = swf::Color::from_rgb(0x000000, 255);
        // A `w`×`h` bar centred on (x, y).
        let bar = |w: f32, h: f32| Matrix {
            a: w,
            b: 0.0,
            c: 0.0,
            d: h,
            tx: swf::Twips::from_pixels((x - w * 0.5) as f64),
            ty: swf::Twips::from_pixels((y - h * 0.5) as f64),
        };
        // Horizontal + vertical arms of the crosshair, each with a black backing
        // bar grown by OUTLINE on every side (drawn first, so the coloured bar on
        // top leaves a thin black rim around the whole "+").
        let h_out = bar(BAR_W + OUTLINE * 2.0, BAR_H + OUTLINE * 2.0);
        let v_out = bar(BAR_H + OUTLINE * 2.0, BAR_W + OUTLINE * 2.0);
        let h_mat = bar(BAR_W, BAR_H);
        let v_mat = bar(BAR_H, BAR_W);
        unsafe {
            glEnable(GL_BLEND);
            glBlendFunc(GL_SRC_ALPHA, GL_ONE_MINUS_SRC_ALPHA);
            glDisable(GL_STENCIL_TEST);
        }
        // Reuse CommandHandler's draw_rect path. It binds program + VAO and
        // uploads a fresh dynamic quad each call.
        <Self as CommandHandler>::draw_rect(self, outline, h_out);
        <Self as CommandHandler>::draw_rect(self, outline, v_out);
        <Self as CommandHandler>::draw_rect(self, color, h_mat);
        <Self as CommandHandler>::draw_rect(self, color, v_mat);
        unsafe {
            glUseProgram(0);
            glBindVertexArray(0);
        }
        // We just zeroed program + VAO, but the cache thinks they are still
        // bound. Invalidate so the next frame's first draw re-binds.
        self.gl_state.invalidate();
    }

    /// The hand-drawn 5x7 font, kept for a console without the shared fonts:
    /// every screen draws through the kit (`render/kit/text.rs`), which falls
    /// back to this when the console fonts cannot be loaded.
    pub fn draw_text_5x7(&mut self, x: f32, y: f32, scale: f32, text: &str, color: swf::Color) {
        // Every lit RUN of the 5x7 font used to be its own `draw_rect` — a
        // uniform upload + glBufferData + glDrawArrays each, ~8 per character.
        // A gallery frame carries ~250 characters, so text alone was thousands of
        // GL calls and ~16 ms, i.e. the entire 60 fps budget before anything else
        // was drawn. The runs are identical geometry, differing only in position,
        // so they are baked into ONE vertex buffer in pixel space and drawn in a
        // single call. Positions are pre-multiplied here instead of coming from
        // the per-draw world matrix; the matrix is identity so `world_matrix`
        // still applies the UI scale/pivot exactly as before.
        let r = color.r as f32 / 255.0;
        let g = color.g as f32 / 255.0;
        let b = color.b as f32 / 255.0;
        let a = color.a as f32 / 255.0;
        let mut verts: std::vec::Vec<f32> = std::vec::Vec::new();
        // Second batch, for shared-font glyphs: same idea, different program.
        let mut atlas_verts: std::vec::Vec<f32> = std::vec::Vec::new();
        let mut atlas_tex: GLuint = 0;
        let mut cur_x = x;
        for ch in text.chars() {
            // Uppercase fold — our font only carries capitals (`bitmap_glyph`).
            if let Some(pattern) = bitmap_glyph(ch) {
                // One rect per horizontal RUN of lit pixels, not per pixel: a 5x7
                // glyph is ~20 lit pixels but only ~8 runs.
                for (row_idx, row_str) in pattern.iter().enumerate() {
                    let py = y + row_idx as f32 * scale;
                    let mut run_start: Option<usize> = None;
                    // `len() + 1` so a run touching the right edge gets flushed.
                    for col_idx in 0..row_str.len() + 1 {
                        let lit = row_str.as_bytes().get(col_idx).map_or(false, |&b| b != b' ');
                        match (lit, run_start) {
                            (true, None) => run_start = Some(col_idx),
                            (false, Some(start)) => {
                                let cols = (col_idx - start) as f32;
                                push_text_quad(
                                    &mut verts,
                                    cur_x + start as f32 * scale,
                                    py,
                                    scale * cols,
                                    scale,
                                    [r, g, b, a],
                                );
                                run_start = None;
                            }
                            _ => {}
                        }
                    }
                }
                // Advance by 6 px (5-wide glyph + 1-px gap), scaled.
                cur_x += 6.0 * scale;
            } else if (ch as u32) >= 0x80 {
                // Non-Latin/Cyrillic (CJK …): shared-font atlas, full-width.
                // QUEUED, not drawn: they all sample the same atlas and share
                // this call's colour, so the whole run goes out in one batch
                // below. Queuing also means no flush of the bitmap-font batch
                // here -- the two are drawn separately at the end, which is
                // invisible because glyphs in a line of text never overlap.
                if let Some((tex, quad)) = self.atlas_glyph_quad(cur_x, y, scale, ch) {
                    if atlas_tex != 0 && atlas_tex != tex {
                        // The atlas grew into a new texture mid-string: close the
                        // batch before switching, rather than draw the tail with
                        // the wrong one.
                        self.flush_atlas_quads(&mut atlas_verts, atlas_tex, color);
                    }
                    atlas_tex = tex;
                    atlas_verts.extend_from_slice(&quad);
                } else if self.font_atlas.is_none() && is_cjk_wrappable(ch) {
                    // No atlas AT ALL -- no shared font service, or not enough
                    // room for one (glyphs::cjk_possible). Draw a hollow cell so a
                    // Chinese title reads as characters this mode cannot draw,
                    // instead of a run of blanks that looks like a game with no
                    // name. Only for the scripts the bitmap font could never
                    // stand in for: a stray symbol in a Latin title still draws
                    // as a gap, which reads better than a box in the middle of
                    // a word. And when the atlas EXISTS, a glyph the font does
                    // not have draws nothing: that is a font gap, not a mode.
                    let bw = (CJK_ADVANCE_UNITS - 2.0) * scale;
                    let bh = 5.0 * scale;
                    let t = scale.max(1.0);
                    let bx = cur_x + scale;
                    let by = y + scale;
                    let c = [r, g, b, a];
                    push_text_quad(&mut verts, bx, by, bw, t, c);
                    push_text_quad(&mut verts, bx, by + bh - t, bw, t, c);
                    push_text_quad(&mut verts, bx, by, t, bh, c);
                    push_text_quad(&mut verts, bx + bw - t, by, t, bh, c);
                }
                cur_x += CJK_ADVANCE_UNITS * scale;
            } else {
                // Unknown ASCII: blank, but keep the pen advancing as before.
                cur_x += 6.0 * scale;
            }
        }
        self.flush_text_quads(&mut verts);
        self.flush_atlas_quads(&mut atlas_verts, atlas_tex, color);
    }

    /// Draw queued text quads (pixel-space positions + per-vertex colour) in one
    /// call and clear the queue. Reuses the solid program and the static quad
    /// VAO — same attribute layout (vec2 pos, vec4 rgba), just more vertices.
    fn flush_text_quads(&mut self, verts: &mut std::vec::Vec<f32>) {
        if verts.is_empty() {
            return;
        }
        if self.mask.writing {
            self.mask_shape_draw_window = self
                .mask_shape_draw_window
                .saturating_add((verts.len() / 36) as u32);
        }
        // Identity: the vertices already carry their pixel coordinates, and this
        // still routes through `world_matrix` so the UI scale/pivot applies.
        let ident = Matrix {
            a: 1.0, b: 0.0, c: 0.0, d: 1.0,
            tx: swf::Twips::ZERO,
            ty: swf::Twips::ZERO,
        };
        let world = self.world_matrix(&ident);
        const IDENT_MULT: [f32; 4] = [1.0, 1.0, 1.0, 1.0];
        const IDENT_ADD: [f32; 4] = [0.0, 0.0, 0.0, 0.0];
        self.use_solid(&world, &IDENT_MULT, &IDENT_ADD);
        self.gl_state.bind_vao(self.rect_vao);
        self.draw_calls_this_window = self.draw_calls_this_window.saturating_add(1);
        unsafe {
            glBindBuffer(GL_ARRAY_BUFFER, self.rect_vbo);
            glBufferData(
                GL_ARRAY_BUFFER,
                (verts.len() * core::mem::size_of::<f32>()) as GLsizeiptr,
                verts.as_ptr() as *const _,
                GL_DYNAMIC_DRAW,
            );
            glDrawArrays(GL_TRIANGLES, 0, (verts.len() / 6) as GLsizei);
        }
        verts.clear();
    }

    pub fn measure_text(&self, text: &str, scale: f32) -> f32 {
        let mut w = 0.0;
        for ch in text.chars() {
            w += char_advance(ch, scale);
        }
        w
    }

    /// Draw one CJK (or other non-bitmap) glyph from the shared-font atlas at
    /// pen position `x` / line-top `y`, tinted `color`. The glyph occupies a
    /// full-width `CJK_ADVANCE_UNITS`-wide cell; the caller advances the pen by
    /// that width regardless of whether a glyph was actually drawn.
    /// Geometry of one shared-font glyph: its atlas texture and six vertices of
    /// (pos.xy, uv.xy), ready to go into a batch.
    ///
    /// Split out of `draw_atlas_glyph` so the batched path and the single-glyph
    /// path cannot disagree about where a character sits.
    fn atlas_glyph_quad(
        &mut self,
        x: f32,
        y: f32,
        scale: f32,
        ch: char,
    ) -> Option<(GLuint, [f32; 24])> {
        if !self.atlas_init_done {
            self.atlas_init_done = true;
            self.font_atlas = crate::backend::glyphs::FontAtlas::new();
            // Building the atlas binds and unbinds texture unit 0 with raw GL,
            // behind this cache's back. Without the resync the next bind of
            // whatever the cache believes is already bound is skipped, and the
            // first thing drawn after the very first non-Latin character
            // samples the wrong texture.
            self.gl_state.invalidate();
        }
        let mut uploaded = false;
        let (tex, info) = match self.font_atlas.as_mut() {
            // The texture comes from the GLYPH, not from the atlas: a long
            // Chinese library spills onto a second texture and the characters
            // packed before it still live on the first.
            Some(fa) => match fa.ensure(ch, &mut uploaded) {
                Some(info) => (info.tex, info),
                None => return None,
            },
            None => return None,
        };
        // A miss bound + wrote the atlas texture behind the GL state cache's
        // back; resync so the next bind is honoured.
        if uploaded {
            self.gl_state.invalidate();
        }
        if info.blank {
            return None;
        }
        let sf = (CJK_ADVANCE_UNITS * scale) / crate::backend::glyphs::RASTER_PX;
        let gw = info.w * sf;
        let gh = info.h * sf;
        let gx = x + info.xmin * sf;
        let baseline = y + CJK_BASELINE_UNITS * scale;
        let gy = baseline - (info.ymin + info.h) * sf;
        if gw <= 0.0 || gh <= 0.0 {
            return None;
        }
        let (u0, v0, du, dv) = (info.uv[0], info.uv[1], info.uv[2], info.uv[3]);
        let (x0, y0, x1, y1) = (gx, gy, gx + gw, gy + gh);
        let (s0, t0, s1, t1) = (u0, v0, u0 + du, v0 + dv);
        #[rustfmt::skip]
        let quad = [
            x0, y0, s0, t0,
            x1, y0, s1, t0,
            x1, y1, s1, t1,
            x0, y0, s0, t0,
            x1, y1, s1, t1,
            x0, y1, s0, t1,
        ];
        Some((tex, quad))
    }

    /// Draw every queued shared-font glyph in ONE call.
    ///
    /// They all sample the same atlas and a `draw_text` call has a single
    /// colour, so the whole run fits one batch. Before this, each character cost
    /// a program setup, a uniform upload and its own `glDrawArrays`, AND forced
    /// the bitmap-font batch to flush first -- a twenty-character Chinese label
    /// was forty draw calls, every frame. The 5x7 font was moved off that model
    /// in v1.6.0, which is what made the gallery smooth; this path stayed behind.
    ///
    /// `u_uv_remap` is the identity here so the per-vertex UVs pass through, and
    /// the world matrix is the identity for the same reason as
    /// `flush_text_quads`: the vertices already carry pixel coordinates, and
    /// going through `world_matrix` keeps the UI scale and the rotation applied.
    fn flush_atlas_quads(&mut self, verts: &mut std::vec::Vec<f32>, tex: GLuint, color: swf::Color) {
        if verts.is_empty() || tex == 0 {
            verts.clear();
            return;
        }
        let ident = Matrix {
            a: 1.0, b: 0.0, c: 0.0, d: 1.0,
            tx: swf::Twips::ZERO,
            ty: swf::Twips::ZERO,
        };
        let world = self.world_matrix(&ident);
        let mult = [
            color.r as f32 / 255.0,
            color.g as f32 / 255.0,
            color.b as f32 / 255.0,
            color.a as f32 / 255.0,
        ];
        const NO_ADD: [f32; 4] = [0.0, 0.0, 0.0, 0.0];
        const PASSTHROUGH_UV: [f32; 4] = [0.0, 0.0, 1.0, 1.0];
        self.use_bitmap(&world, &mult, &NO_ADD, tex, &PASSTHROUGH_UV, 0, false);
        self.gl_state.bind_vao(self.atlas_vao);
        self.draw_calls_this_window = self.draw_calls_this_window.saturating_add(1);
        unsafe {
            glBindBuffer(GL_ARRAY_BUFFER, self.atlas_vbo);
            glBufferData(
                GL_ARRAY_BUFFER,
                (verts.len() * core::mem::size_of::<f32>()) as GLsizeiptr,
                verts.as_ptr() as *const _,
                GL_DYNAMIC_DRAW,
            );
            glDrawArrays(GL_TRIANGLES, 0, (verts.len() / 4) as GLsizei);
        }
        verts.clear();
    }


    /// The pause panel as data, handed to `f` (its strings live in this
    /// frame). Rows follow `MENU_ITEMS`.
    ///
    /// The rows say what they are before they are pressed: a chevron on the two
    /// that open a panel (Touches et curseur, Écran), the overclock's three
    /// settings all on show with the one in force lit, and plain rows for the
    /// three that act at once. Under the title, two chips answer what a player
    /// pausing a slow game wants to know: how fast it was running, and whether
    /// the processor is raised.
    pub(crate) fn with_pause_sheet<R>(&mut self, selected: usize, f: impl FnOnce(&mut Self, &kit::sheet::Sheet) -> R) -> R {
        use kit::list::Row;
        use theme::col;
        let lc = crate::loc::s();
        let u = crate::loc_ui::ui();
        let game = crate::library::active_display_name().unwrap_or_default();
        // OVERCLOCK reads back `flashnx_clock_mode()`, not the stored
        // preference: a raise can be refused (clkrst unavailable, battery out
        // of its Normal voltage state) and a row claiming HIGH while the
        // console sits at 1020 is a support ticket. AUTO reads AUTO: whether it
        // holds the raise at this instant is the processor chip.
        //
        // It stays at the top level rather than joining ECRAN: every ECRAN row
        // previews on the frozen frame, and this one cannot.
        let power_shown = crate::clock_auto::shown_mode();
        let oc_opts = [lc.overclock_off, lc.pixels_auto, lc.overclock_on];
        let oc_active = match power_shown {
            0 => 0,
            crate::keymap::POWER_AUTO => 1,
            _ => 2,
        };
        let rows = [
            Row::new(Icon::Play, lc.menu_resume),
            Row::new(Icon::Gamepad, lc.menu_keys).opens(),
            Row::new(Icon::Monitor, lc.menu_screen).opens(),
            Row::new(Icon::Cpu, lc.set_overclock).choice(&oc_opts, oc_active),
            Row::new(Icon::Restart, lc.menu_restart),
            Row::new(Icon::LogOut, lc.menu_quit),
        ];
        debug_assert_eq!(rows.len(), MENU_ITEMS.len());
        let help = match MENU_ITEMS.get(selected) {
            Some(&"REPRENDRE") => u.help_resume,
            Some(&"TOUCHES") => u.help_keys,
            Some(&"ECRAN") => u.help_screen,
            Some(&"OVERCLOCK") => {
                if overclock_refused() {
                    lc.overclock_refused
                } else {
                    match power_shown {
                        0 => u.help_oc_off,
                        crate::keymap::POWER_AUTO => u.help_oc_auto,
                        _ => u.help_oc_on,
                    }
                }
            }
            Some(&"REDEMARRER") => u.help_restart,
            Some(&"QUITTER") => u.help_quit,
            _ => "",
        };
        // The A hint names what A does on THIS row; B always resumes.
        let a_verb = match MENU_ITEMS.get(selected) {
            Some(&"REPRENDRE") => u.resume,
            Some(&"TOUCHES") | Some(&"ECRAN") => u.open,
            Some(&"OVERCLOCK") => u.change,
            Some(&"REDEMARRER") => u.restart,
            Some(&"QUITTER") => u.quit,
            _ => u.choose,
        };
        let on_choice = MENU_ITEMS.get(selected) == Some(&"OVERCLOCK");
        let hints_choice = [(&[Btn::Dpad][..], u.change), (&[Btn::B][..], u.resume_game)];
        let hints_act = [(&[Btn::A][..], a_verb), (&[Btn::B][..], u.resume_game)];

        // The chips. The reading is the one frozen with the picture: no frame
        // runs while paused, so there is no newer speed to report.
        let (measured, nominal) = fps_readout();
        let fps_text;
        let mut chips: std::vec::Vec<(Icon, &str, u32)> = std::vec::Vec::new();
        if let Some(real_x10) = measured {
            fps_text = u.fps_chip.replace("{}", &fps_rate_str(real_x10));
            let ink = match nominal {
                Some(n) if n > 0 => match (real_x10 as u64 * 100 / n as u64) as u32 {
                    0..=65 => col::DANGER,
                    66..=94 => col::GOLD,
                    _ => col::OK,
                },
                _ => col::TEXT3,
            };
            chips.push((Icon::Gauge, fps_text.as_str(), ink));
        }
        let raised = current_power_mode() == 1;
        chips.push((
            Icon::Cpu,
            if raised { u.cpu_boosted } else { u.cpu_normal },
            if raised { col::GOLD } else { col::TEXT3 },
        ));

        let mut sheet = kit::sheet::Sheet::new(&game, &rows, selected);
        sheet.kicker = Some(Icon::Pause);
        sheet.crumbs = core::slice::from_ref(&lc.pause_title);
        sheet.chips = &chips;
        sheet.glide_key = GLIDE_KEY_PAUSE;
        sheet.help = help;
        sheet.hints = if on_choice { &hints_choice } else { &hints_act };
        f(self, &sheet)
    }

    /// Draw the pause over the frozen frame the caller re-rendered first.
    /// `selected` indexes `MENU_ITEMS`.
    ///
    /// When a sub-panel was open the frame before, it slides back under this
    /// one while the rail unfolds (`kit_cascade`): B goes back the way A came.
    pub fn draw_menu_overlay(&mut self, selected: usize) {
        let t = kit::sheet::cascade_step(false);
        let child = if t > 0.0 { pause_child() } else { PauseChild::None };
        self.with_pause_sheet(selected, |s, parent| match child {
            PauseChild::Screen(sel) => {
                s.with_screen_sheet(sel, |s, c| s.kit_cascade(parent, PAUSE_ROW_SCREEN, Some(c), t));
            }
            PauseChild::Touches(sel) => {
                crate::menu::with_touches_sheet(s, sel, |s, c| s.kit_cascade(parent, PAUSE_ROW_TOUCHES, Some(c), t));
            }
            PauseChild::None => s.kit_cascade(parent, 0, None, 0.0),
        });
        if t <= 0.0 {
            pause_child_set(PauseChild::None);
        }
        unsafe {
            glUseProgram(0);
            glBindVertexArray(0);
        }
        self.gl_state.invalidate();
    }

    /// The ECRAN panel as data, handed to `f`. Rows follow `SCREEN_ITEMS`.
    ///
    /// Every setting shows all its options, the one in force lit (Left and
    /// Right move it, A goes round); the zoom, a free percentage, shows its
    /// value between arrows; the counter is a switch. All of it previews on the
    /// frozen frame to the right, which C++ re-renders after each press.
    pub(crate) fn with_screen_sheet<R>(&mut self, selected: usize, f: impl FnOnce(&mut Self, &kit::sheet::Sheet) -> R) -> R {
        use kit::list::Row;
        let lc = crate::loc::s();
        let u = crate::loc_ui::ui();
        // Every row is read back from what is in force: the game's `.prefs` for
        // the ones stored per game, the renderer for the zoom and the counter.
        let display_opts = [lc.display_fit, lc.display_stretch, lc.display_fill];
        let rot_opts = [lc.rot_none, lc.rot_90, lc.rot_180, lc.rot_270];
        let pix_opts = [lc.pixels_auto, lc.pixels_smooth, lc.pixels_sharp];
        let filter_opts = [lc.filter_none, lc.filter_scanlines, lc.filter_crt];
        let zoom = std::format!("{} %", game_zoom_percent());
        let rows = [
            Row::new(Icon::Monitor, lc.set_display_mode).choice(&display_opts, crate::keymap::display_mode() as usize),
            Row::new(Icon::Rotate, lc.set_rotation).choice(&rot_opts, crate::keymap::rotation() as usize),
            Row::new(Icon::ZoomIn, lc.set_zoom).cycle(&zoom),
            Row::new(Icon::Pixels, lc.set_pixel_filter).choice(&pix_opts, crate::keymap::pixel_filter() as usize),
            Row::new(Icon::Tv, lc.set_screen_filter).choice(&filter_opts, crate::keymap::screen_filter() as usize),
            Row::new(Icon::Gauge, lc.set_fps).toggle(fps_counter_on()),
        ];
        debug_assert_eq!(rows.len(), SCREEN_ITEMS.len());
        let help = match SCREEN_ITEMS.get(selected) {
            Some(&"AFFICHAGE") => u.help_display,
            Some(&"ROTATION") => u.help_rotation,
            Some(&"ZOOM") => u.help_zoom,
            Some(&"PIXELS") => u.help_pixels,
            Some(&"FILTRE") => u.help_filter,
            Some(&"FPS") => u.help_fps,
            _ => "",
        };
        // The zoom row also opens the framing mode on A.
        let hints_zoom = [(&[Btn::Dpad][..], u.change), (&[Btn::A][..], u.zoom_frame), (&[Btn::B][..], u.back)];
        let hints = [(&[Btn::Dpad][..], u.change), (&[Btn::B][..], u.back)];
        let crumbs = [lc.pause_title];
        let mut sheet = kit::sheet::Sheet::new(lc.menu_screen, &rows, selected);
        sheet.kicker = Some(Icon::ChevronLeft);
        sheet.crumbs = &crumbs;
        sheet.glide_key = GLIDE_KEY_SCREEN;
        sheet.help = help;
        sheet.hints = if SCREEN_ITEMS.get(selected) == Some(&"ZOOM") { &hints_zoom } else { &hints };
        f(self, &sheet)
    }

    /// Draw the ECRAN panel beside the pause rail. `selected` indexes
    /// `SCREEN_ITEMS`.
    pub fn draw_screen_menu(&mut self, selected: usize) {
        pause_child_set(PauseChild::Screen(selected));
        let t = kit::sheet::cascade_step(true);
        self.with_pause_sheet(PAUSE_ROW_SCREEN, |s, parent| {
            s.with_screen_sheet(selected, |s, c| s.kit_cascade(parent, PAUSE_ROW_SCREEN, Some(c), t));
        });
        unsafe {
            glUseProgram(0);
            glBindVertexArray(0);
        }
        self.gl_state.invalidate();
    }

    /// Zoom-adjust mode (issue #101): the framing legend, over an untouched
    /// picture.
    ///
    /// NOTHING is laid over the picture but the two lines of text themselves.
    /// What the player is judging here IS the framing, to the pixel, so every
    /// band, plate or tint is a piece of the answer taken away -- and a tint
    /// would fight whatever they set in FILTRE one row down. The signal that the
    /// buttons have stopped driving the game is that the panel is gone and the
    /// legend has taken its place.
    ///
    /// Drawn outside `game_layer`, so the text keeps its size while the picture
    /// behind it grows.
    pub fn draw_zoom_overlay(&mut self, percent: u16) {
        use theme::{col, size};
        let u = crate::loc_ui::ui();
        let lc = crate::loc::s();
        let (vw, vh) = (self.dimensions.width as f32, self.dimensions.height as f32);
        unsafe {
            glEnable(GL_BLEND);
            glBlendFunc(GL_SRC_ALPHA, GL_ONE_MINUS_SRC_ALPHA);
        }
        // Two small plates on the picture, nothing else: what is being judged
        // here IS the framing. The top one carries the WHOLE framing state, not
        // just the percentage: the offset is half of what is being set, and it
        // is what lands in the game's `.prefs` as `panx` / `pany`.
        const PLATE: u32 = 0xC8_0A_10_1C;
        let (ox, oy) = game_pan();
        let head = std::format!("{}  {} %     X {}   Y {}", lc.set_zoom, percent, ox, oy);
        let hw = self.kit_measure(&head, size::BODY);
        let pw = hw + 40.0;
        self.kit_rrect(((vw - pw) * 0.5).round(), 14.0, pw, 40.0, 20.0, PLATE);
        self.kit_text_mid(((vw - hw) * 0.5).round(), 34.0, size::BODY, &head, col::GOLD);

        // What every button does in this mode (cpp/src/main.cpp, `zoom_mode`):
        // shoulders zoom coarsely, Y / X finely, either stick frames, the d-pad
        // frames by single pixels, pressing the left stick resets, A keeps, B
        // drops. Framing MOVES a stick and resetting PRESSES one: two glyphs.
        let hints: [kit::text::HintItem; 7] = [
            (&[Btn::ZL, Btn::ZR], u.zoom_coarse),
            (&[Btn::Y, Btn::X], u.zoom_fine),
            (&[Btn::LStick, Btn::RStick], u.zoom_frame),
            (&[Btn::Dpad], u.zoom_pixel),
            (&[Btn::LStickPress], u.zoom_reset),
            (&[Btn::A], u.validate),
            (&[Btn::B], u.cancel),
        ];
        let w = self.kit_hints_width(&hints);
        let pw = (w + 48.0).min(vw - 24.0);
        let py = vh - 66.0;
        self.kit_rrect(((vw - pw) * 0.5).round(), py, pw, 48.0, 24.0, PLATE);
        self.kit_hints_centered(vw * 0.5, py + 24.0, &hints);

        unsafe {
            glUseProgram(0);
            glBindVertexArray(0);
        }
        self.gl_state.invalidate();
    }

    /// FPS counter (issue #111): what the movie is actually running at, against
    /// what it asks for.
    ///
    /// A bare number answers nothing on this console. Nothing here declares 60:
    /// Mario 63 asks for 32, Papa Louie 3 and Agent P for 30, Icy Tower for 24.
    /// So the reading is shown against the movie's own rate and the ratio is
    /// spelled out, because the ratio is the thing the player actually feels --
    /// Papa Louie 3 at 15.1 of 30 is an in-game stopwatch reading 5 s while 10 s
    /// of real time goes past.
    ///
    /// Drawn OUTSIDE `game_layer`, like the pointer and the framing legend, so
    /// the free zoom (issue #101) leaves it alone. Nothing has to undo the zoom
    /// by hand here, unlike the pointer: `world_matrix` only folds it in while
    /// that flag is set, and `submit_frame` clears it before this is reached. At
    /// 400% a counter that scaled with the picture would be off the screen.
    ///
    /// No plate behind it, for the reason the framing legend has none: it sits
    /// on a running game, and a band wide enough to guarantee contrast hides a
    /// corner of the thing being measured. `draw_text_outlined` buys the
    /// legibility instead, over a white sky and over a black cave alike.
    ///
    /// Every character is in the 5x7 bitmap font -- digits, '/', '.', 'X' and
    /// the letters of FPS and OC -- so this pulls no shared-font glyph and stays
    /// safe in applet mode, where loading that font is fatal.
    pub fn draw_fps_overlay(&mut self) {
        use theme::col;
        let (measured, nominal) = fps_readout();
        unsafe {
            glEnable(GL_BLEND);
            glBlendFunc(GL_SRC_ALPHA, GL_ONE_MINUS_SRC_ALPHA);
            glDisable(GL_STENCIL_TEST);
        }
        // The rating, as a dot's colour: green at full speed, amber where the
        // slowdown shows, red below two thirds. A reading aid over numbers that
        // stay exact, so a glance is enough while both hands are on the game.
        let (text, dot) = if let (Some(real_x10), None) = (measured, nominal) {
            // The movie declares no usable rate: no denominator, no ratio, but
            // the measured rate still answers "is this running".
            (std::format!("{} FPS", fps_rate_str(real_x10)), col::TEXT3)
        } else if measured.is_none() {
            // No window has closed yet: the nominal, so the counter is visibly
            // alive. Not a measured zero, which is real and gets red below.
            (
                match nominal {
                    Some(n) => std::format!("-- / {} FPS", fps_rate_str(n)),
                    None => std::string::String::from("-- FPS"),
                },
                col::TEXT3,
            )
        } else {
            let real_x10 = measured.unwrap_or(0);
            let nominal_x10 = nominal.unwrap_or(0);
            // `fps_readout` guarantees a non-zero denominator.
            let pct = (real_x10 as u64 * 100 / nominal_x10 as u64) as u32;
            let dot = match pct {
                0..=65 => col::DANGER,
                66..=94 => col::GOLD,
                _ => col::OK,
            };
            (
                std::format!(
                    "{} / {} FPS  \u{00B7}  {}.{:02}\u{00D7}",
                    fps_rate_str(real_x10),
                    fps_rate_str(nominal_x10),
                    pct / 100,
                    pct % 100,
                ),
                dot,
            )
        };
        const PX: f32 = 15.0;
        const H: f32 = 28.0;
        const MARGIN: f32 = 14.0;
        // The LOGICAL viewport, portrait while the picture is turned (#78):
        // `world_matrix` composes the quarter-turn, so this corner is that corner
        // of the turned picture. Pinned to the RIGHT edge, so the pill grows to
        // the left as the number does instead of sliding.
        let vw = self.dimensions.width as f32;
        let tw = self.kit_measure(&text, PX);
        let w = 12.0 + 8.0 + 8.0 + tw + 12.0;
        let x = (vw - MARGIN - w).round();
        let y = MARGIN;
        self.kit_rrect(x, y, w, H, H * 0.5, 0xB8_0A_10_1C);
        self.kit_rrect(x + 12.0, y + H * 0.5 - 4.0, 8.0, 8.0, 4.0, dot);
        self.kit_text_mid(x + 12.0 + 8.0 + 8.0, y + H * 0.5, PX, &text, col::TEXT);
        // "OC" while the overclock is IN FORCE (the mode the hardware holds, not
        // the stored preference): a raise dropped for the battery then shows as
        // its absence next to the number it explains.
        if current_power_mode() == 1 {
            let ow = self.kit_measure("OC", PX);
            let cw = ow + 20.0;
            let cx = x - 8.0 - cw;
            self.kit_rrect(cx, y, cw, H, H * 0.5, 0xB8_0A_10_1C);
            self.kit_text_mid(cx + 10.0, y + H * 0.5, PX, "OC", 0xFF_40_C4_FF);
        }
        unsafe {
            glUseProgram(0);
            glBindVertexArray(0);
        }
        self.gl_state.invalidate();
    }

    /// TOUCHES pad view: the keymap editor (Touches.dc.html).
    ///
    /// Draws the controller from `keymap::PAD_SLOTS` (positioned controls in
    /// abstract units) with one row per control, in two columns flanking the
    /// picture, and lights the selected control in BOTH places at once: the row
    /// you are reading and the button you would press.
    ///
    /// That pairing is the point of the screen. The list this replaced could
    /// tell you a binding but never where the button was, and it showed eight of
    /// twenty-five at a time, so reading one keymap took four screens, times
    /// five combo layers, times two players. Nothing here scrolls.
    ///
    /// Around it, the window of the mockup: the path it was opened from and the
    /// game it edits, the player and layer switches beside the buttons that work
    /// them, one line about the row under the cursor, the cursor speed and the
    /// hints. Every value says what the button really does: a key on a keycap, a
    /// click beside the mouse, "None" quietly, in a combo layer the Normal key it
    /// falls back to, a modifier as the layer it holds (its button teal on the
    /// picture), and a stick that moves the cursor as one value across its four
    /// direction rows.
    pub(crate) fn draw_touches_pad(&mut self, v: &PadView) {
        use crate::keymap::{Nub, PadIcon, PAD_SLOTS};
        use theme::{col, size};
        // Amber is the cursor everywhere in this app. Teal marks a button that
        // is a MODIFIER rather than a key, the teal the key picker uses for a
        // key already in use, and with the same meaning: this one is spoken for.
        const SHELL_COL: u32 = 0xFF_1E_2C_4A;
        const ICON_COL: u32 = 0xFF_34_46_6B;
        const ICON_NUB_COL: u32 = 0xFF_5A_6A_8F;
        const ICON_SEL_COL: u32 = col::GOLD;
        const ICON_SEL_NUB: u32 = 0xFF_1A_14_05;
        const ICON_MOD_COL: u32 = 0xFF_2E_6E_63;
        const ICON_MOD_NUB: u32 = 0xFF_58_C0_AE;
        const ICON_LOCK_COL: u32 = 0xFF_22_32_4F;
        /// The band of the line about the focused row, over the footer.
        const HELP_H: f32 = 36.0;
        /// Between a column of rows and the picture.
        const COL_GAP: f32 = 28.0;
        /// The picture's width in pad units: the shell and its rail buttons
        /// (13.9 to 32.1), with a little air, centred on the shell's middle.
        const PIC_UNITS: f32 = 18.4;
        const PIC_MID: f32 = 23.0;

        let lc = crate::loc::s();
        let u = crate::loc_ui::ui();
        let vw = self.dimensions.width as f32;
        let vh = self.dimensions.height as f32;
        let inner_w = vw - 2.0 * TOUCHES_MARGIN - 2.0 * TOUCHES_PAD_X;

        // The header's two switches, measured first: on a narrow (turned)
        // screen they take a row of their own under the title.
        let players = [u.player_1, u.player_2];
        let layers = [u.layer_normal, "ZL", "ZR", "L", "R"];
        let gpx = 22.0;
        let bx = self.kit_button_width(Btn::X, gpx);
        let bl = self.kit_button_width(Btn::L, gpx);
        let br = self.kit_button_width(Btn::R, gpx);
        let pw = self.pad_segments_width(&players, &[false; 2]);
        let lw = self.pad_segments_width(&layers, &v.live_layers);
        // [X] players | [L] layers [R], 8 px apart and a hairline between.
        let switches_w = bx + pw + bl + lw + br + 49.0;
        let title_w = self.kit_measure(lc.keys_title, TOUCHES_TITLE_PX);
        let wide = title_w + 14.0 + 120.0 + 24.0 + switches_w <= inner_w;
        let head_h = if wide { TOUCHES_HEAD_H } else { TOUCHES_HEAD_H + 44.0 };

        // ONE scale from BOTH axes: this is a picture of an object, and letting
        // the axes disagree would stretch the controller. In landscape the height
        // decides and the window fills the screen; a turned picture gives a
        // narrow viewport, the width decides, and the window shrinks to the pad.
        let avail_h = (vh - 2.0 * TOUCHES_MARGIN - head_h - HELP_H - TOUCHES_FOOT_H).max(80.0);
        let unit = (inner_w / crate::keymap::PAD_UNITS_W).min(avail_h / crate::keymap::PAD_UNITS_H);
        let pad_h = crate::keymap::PAD_UNITS_H * unit;
        let (x, y, w, h) = self.touches_window(head_h + pad_h + HELP_H + TOUCHES_FOOT_H);
        let left = x + TOUCHES_PAD_X;
        let right = x + w - TOUCHES_PAD_X;

        // ── The header ───────────────────────────────────────────────────
        self.touches_crumbs(left, y + 30.0, inner_w, v.crumbs);
        let ty = y + 66.0;
        let tw = self.kit_text_mid(left, ty, TOUCHES_TITLE_PX, lc.keys_title, col::TEXT);
        let sw_cy = if wide { ty } else { y + 110.0 };
        let sw_x = right - switches_w;
        let subject_x = left + tw + 14.0;
        let subject_max = if wide { sw_x - 24.0 - subject_x } else { right - subject_x };
        if subject_max > 40.0 && !v.subject.is_empty() {
            // Cut in the middle: what tells two episodes of a series apart is
            // at the end of the name.
            let s = self.kit_fit_mid(v.subject, size::LABEL, subject_max);
            self.kit_text_mid(subject_x, ty, size::LABEL, &s, col::TEXT3);
        }
        // Both switches on show, each beside the button that works it: nothing
        // about the map being edited should need a guess-press to discover
        // (issue #55/#57).
        let mut cx = sw_x;
        cx += self.kit_button(cx, sw_cy, gpx, Btn::X, col::PILL_INK) + 8.0;
        let player_cells = self.pad_segments(cx, sw_cy, &players, if v.player == 2 { 1 } else { 0 }, &[false; 2]);
        cx += pw + 8.0;
        self.draw_overlay_rect(cx, sw_cy - 12.0, 1.0, 24.0, col::PANEL_LINE);
        cx += 9.0;
        cx += self.kit_button(cx, sw_cy, gpx, Btn::L, col::PILL_INK) + 8.0;
        let layer_cells = self.pad_segments(cx, sw_cy, &layers, v.layer.min(4), &v.live_layers);
        cx += lw + 8.0;
        self.kit_button(cx, sw_cy, gpx, Btn::R, col::PILL_INK);
        self.draw_overlay_rect(left, y + head_h - 6.0, inner_w, 1.0, col::LINE);

        // Units -> pixels. The picture is centred in the window at the one
        // scale; the two columns of rows run from the window's padding to the
        // picture, so they line up with the header and the footer whatever room
        // the height leaves. Their heights stay in units: a row and the button
        // it names are what the eye pairs.
        let ox = x + w * 0.5 - PIC_MID * unit;
        let oy = y + head_h;
        let px = |ux: f32| ox + ux * unit;
        let py = |uy: f32| oy + uy * unit;
        let col_w = ((inner_w - PIC_UNITS * unit) * 0.5 - COL_GAP).max(40.0);
        let row_rect = |i: usize| -> (f32, f32, f32, f32) {
            let Some(s) = PAD_SLOTS.get(i) else { return (0.0, 0.0, 0.0, 0.0) };
            let rx = if s.chip.0 < PIC_MID { left } else { right - col_w };
            (rx, py(s.chip.1), col_w, s.chip.3 * unit)
        };

        // ── The shell ────────────────────────────────────────────────────
        // Grips first, body over them, one colour: it is a silhouette, and a
        // seam between the three would read as three objects.
        for &(sx, sy, sw, sh, r) in crate::keymap::PAD_SHELL {
            self.draw_round_rect(px(sx), py(sy), sw * unit, sh * unit, r * unit, SHELL_COL);
        }

        // ── Minus, locked ────────────────────────────────────────────────
        {
            let (mx, my, mw, mh) = crate::keymap::PAD_MINUS;
            let (mxp, myp, d) = (px(mx), py(my), mw * unit);
            self.draw_round_rect(mxp, myp, d, mh * unit, d * 0.5, ICON_LOCK_COL);
            let lp = (d * 0.8).clamp(8.0, 16.0).round();
            let mlw = self.kit_measure("\u{2212}", lp);
            if mlw <= d - 2.0 {
                self.kit_text_mid((mxp + (d - mlw) * 0.5).round(), myp + mh * unit * 0.5, lp, "\u{2212}", ICON_NUB_COL);
            }
        }

        // ── The controls on the picture ──────────────────────────────────
        // The five slots of one stick share a rect on purpose, so a stick must
        // be drawn ONCE, with the nub of whichever of the five is selected.
        // Drawn per-slot they would overdraw each other, and the last one in the
        // table would always win. Same reason the selected control is drawn in a
        // second pass: the d-pad arms meet, and the one under the cursor has to
        // be the one on top.
        let is_mod = |i: usize| matches!(v.values.get(i), Some(PadValue::Modifier));
        let sel_slot = PAD_SLOTS.get(v.selection);
        // Each control's rect, for touch: a tap on the picture picks its row.
        // A stick answers once, on its first slot.
        let mut icon_cells: std::vec::Vec<(f32, f32, f32, f32)> = std::vec![(0.0, 0.0, 0.0, 0.0); PAD_SLOTS.len()];
        for pass in 0..2 {
            for (i, slot) in PAD_SLOTS.iter().enumerate() {
                let is_sel = i == v.selection;
                if is_sel != (pass == 1) {
                    continue;
                }
                let (ix, iy, iw, ih) = slot.icon;
                let (sx, sy, sw, sh) = (px(ix), py(iy), iw * unit, ih * unit);
                let (body, nub) = if is_sel {
                    (ICON_SEL_COL, ICON_SEL_NUB)
                } else if is_mod(i) {
                    (ICON_MOD_COL, ICON_MOD_NUB)
                } else {
                    (ICON_COL, ICON_NUB_COL)
                };
                match slot.shape {
                    PadIcon::Stick(n) => {
                        // Only the first slot of the group draws the ring, and
                        // only when the cursor is elsewhere: a selected slot owns
                        // its stick and draws it in pass 1 with its own nub.
                        let group_head = PAD_SLOTS
                            .iter()
                            .position(|o| matches!(o.shape, PadIcon::Stick(_)) && o.icon == slot.icon)
                            .unwrap_or(i);
                        if i == group_head {
                            icon_cells[i] = (sx, sy, sw, sh);
                        }
                        let owner = sel_slot
                            .filter(|s| matches!(s.shape, PadIcon::Stick(_)) && s.icon == slot.icon)
                            .is_some();
                        if (owner && !is_sel) || (!owner && i != group_head) {
                            continue;
                        }
                        let n = if is_sel { n } else { Nub::Press };
                        self.draw_round_rect(sx, sy, sw, sh, sw * 0.5, body);
                        let nd = sw * 0.52;
                        let off = sw * 0.20;
                        let (dx, dy) = match n {
                            Nub::Up => (0.0, -off),
                            Nub::Down => (0.0, off),
                            Nub::Left => (-off, 0.0),
                            Nub::Right => (off, 0.0),
                            Nub::Press => (0.0, 0.0),
                        };
                        self.draw_round_rect(
                            sx + sw * 0.5 + dx - nd * 0.5,
                            sy + sh * 0.5 + dy - nd * 0.5,
                            nd,
                            nd,
                            nd * 0.5,
                            nub,
                        );
                    }
                    PadIcon::Disc => {
                        icon_cells[i] = (sx, sy, sw, sh);
                        self.draw_round_rect(sx, sy, sw, sh, sw.min(sh) * 0.5, body);
                    }
                    PadIcon::Slab => {
                        icon_cells[i] = (sx, sy, sw, sh);
                        self.draw_round_rect(sx, sy, sw, sh, sw.min(sh) * 0.34, body);
                    }
                }
                // The glyph, when the shape is big enough to carry one. The rail
                // buttons are 0.7 units wide and never are; they are named by
                // their row and found by lighting up.
                if !matches!(slot.shape, PadIcon::Stick(_)) {
                    // Selected wins. The cursor should never be on a modifier
                    // row -- menu.rs steps over them and refuses them to a
                    // finger -- but a layer can change under a resting cursor,
                    // and a label that turned invisible for one frame would be
                    // a worse answer than one that stays readable.
                    let txt = if is_sel {
                        0xFF_1A_1A_1A
                    } else if is_mod(i) {
                        0xFF_EA_F7_F3
                    } else {
                        0xFF_E8_EE_F6
                    };
                    // The label shrinks to fit its button, and a button too small
                    // for one even at 9 px (the SL/SR rails) goes without.
                    let mut lp = (sh * 0.55).clamp(9.0, 17.0).round();
                    let mut glw = self.kit_measure(slot.glyph, lp);
                    if glw > sw - 4.0 {
                        lp = (lp * (sw - 4.0) / glw).floor();
                        glw = self.kit_measure(slot.glyph, lp);
                    }
                    if lp >= 9.0 {
                        self.kit_text_mid((sx + (sw - glw) * 0.5).round(), sy + sh * 0.5, lp, slot.glyph, txt);
                    }
                }
            }
        }

        // Why Minus has no row: it opens the pause, from wherever this editor
        // was opened. Under the picture, where the eye goes looking for it.
        {
            let npx = (unit * 0.6).clamp(10.0, size::CAPTION);
            let gw = self.kit_button_width(Btn::Minus, npx + 3.0);
            let nw = gw + 6.0 + self.kit_measure(u.keys_minus_note, npx);
            let nx = (px(PIC_MID) - nw * 0.5).round();
            let ncy = py(20.6);
            self.kit_button(nx, ncy, npx + 3.0, Btn::Minus, TOUCHES_QUIET);
            self.kit_text_mid(nx + gw + 6.0, ncy, npx, u.keys_minus_note, TOUCHES_QUIET);
        }

        // ── The rows ─────────────────────────────────────────────────────
        let row_h = PAD_SLOTS.first().map(|s| s.chip.3 * unit).unwrap_or(0.0);
        let txt = (row_h * 0.5).clamp(10.0, size::LABEL);
        // One badge column for every row, as wide as the widest badge, so the
        // values start in one place down both columns.
        let mut badge_w: f32 = 2.4 * unit;
        for slot in PAD_SLOTS {
            badge_w = badge_w.max(self.pad_badge_width(slot.glyph, txt));
        }
        let value_x = |rx: f32| rx + 10.0 + badge_w + 12.0;

        // A stick that moves the cursor is ONE thing, not four rows saying
        // "None": its direction rows share one value that says what it does.
        // Bind any of them and the stick becomes a d-pad (the help line says
        // so); the rows then read one by one again.
        let mut groups: std::vec::Vec<(f32, f32, f32, f32)> = std::vec::Vec::new();
        let mut in_group = [false; 32];
        for (si, stick) in ['L', 'R'].iter().enumerate() {
            if !v.stick_cursor[si] {
                continue;
            }
            let rows: std::vec::Vec<usize> = (0..PAD_SLOTS.len())
                .filter(|&i| crate::keymap::stick_of_direction(PAD_SLOTS[i].name) == Some(*stick))
                .collect();
            let (Some(&first), Some(&last)) = (rows.first(), rows.last()) else { continue };
            let (rx, ry0, rw, _) = row_rect(first);
            let (_, ry1, _, rh1) = row_rect(last);
            let gx = value_x(rx) - 6.0;
            groups.push((gx, ry0 + 3.0, rx + rw - 6.0 - gx, ry1 + rh1 - 3.0 - (ry0 + 3.0)));
            for i in rows {
                if let Some(f) = in_group.get_mut(i) {
                    *f = true;
                }
            }
        }
        for &(gx, gy, gw, gh) in &groups {
            self.kit_rrect(gx, gy, gw, gh, 8.0, col::TILE);
        }

        // The focus, eased on BOTH axes with one key: the cursor crosses
        // columns here, and a jump from the left hand to the right is the move
        // that most needs following by eye. The mockup's focused row: a darker
        // fill than a keycap, so the key on it stays a key, inside a gold edge.
        let now = unsafe { ruffle_tick_now() };
        if v.selection < PAD_SLOTS.len() {
            let (rx, ry, rw, rh) = row_rect(v.selection);
            let fx = eased_list_x(rx, GLIDE_KEY_KEYS, now);
            let fy = eased_list_y(ry, GLIDE_KEY_KEYS, now);
            self.kit_shape(fx, fy, rw, rh, 9.0, col::LINE, 2.0, col::GOLD, 0.0);
        }

        let mut cells: std::vec::Vec<(f32, f32, f32, f32)> =
            std::vec::Vec::with_capacity(PAD_CELL_ICON + PAD_SLOTS.len());
        for (i, slot) in PAD_SLOTS.iter().enumerate() {
            let (rx, ry, rw, rh) = row_rect(i);
            cells.push((rx, ry, rw, rh));
            let rcy = ry + rh * 0.5;
            let focused = i == v.selection;
            let value = v.values.get(i).cloned().unwrap_or(PadValue::None);
            let badge_ink = if value == PadValue::Modifier {
                col::TEXT3
            } else if focused {
                col::GOLD
            } else {
                col::TEXT
            };
            self.kit_pad_badge(rx + 10.0, rcy, badge_w, slot.glyph, txt, badge_ink);
            if in_group.get(i).copied().unwrap_or(false) {
                continue;
            }
            let vx = value_x(rx);
            let vmax = (rx + rw - 10.0 - vx).max(0.0);
            self.pad_value(vx, rcy, vmax, rh, &value, slot.name, txt);
        }
        for &(gx, gy, gw, gh) in &groups {
            let gcy = gy + gh * 0.5;
            let isz = (txt + 1.0).round();
            self.kit_icon(gx + 6.0, gcy, isz, Icon::Pointer, col::TEXT2);
            let t = self.kit_fit(u.keys_mouse_cursor, txt, (gw - 12.0 - isz - 8.0).max(0.0));
            self.kit_text_mid(gx + 6.0 + isz + 8.0, gcy, txt, &t, col::TEXT2);
        }

        // ── The line about the focused row, and the footer ───────────────
        let help_cy = y + h - TOUCHES_FOOT_H - HELP_H * 0.5;
        if !v.help.is_empty() {
            self.kit_icon(left, help_cy, 16.0, Icon::Info, col::TEXT3);
            let t = self.kit_fit(v.help, size::LABEL, inner_w - 26.0);
            self.kit_text_mid(left + 26.0, help_cy, size::LABEL, &t, col::TEXT2);
        }
        let hints_profiles: [kit::text::HintItem; 3] =
            [(&[Btn::A], u.edit), (&[Btn::Y], u.shared_profiles), (&[Btn::B], u.back)];
        let hints_plain: [kit::text::HintItem; 2] = [(&[Btn::A], u.edit), (&[Btn::B], u.back)];
        let hints_x = self.touches_footer(x, y, w, h, if v.profiles { &hints_profiles } else { &hints_plain });
        if let Some(speed) = v.cursor_speed {
            // The speed the cursor rows of the page behind set, read here
            // without going back (the mockup's footer). Left out rather than
            // run into the hints on a narrow screen.
            let s = std::format!("{} {}", lc.set_cursor_speed, speed);
            let fpx = size::CAPTION + 1.0;
            if left + 24.0 + self.kit_measure(&s, fpx) + 24.0 <= hints_x {
                let foot_cy = y + h - TOUCHES_FOOT_H * 0.5;
                self.kit_icon(left, foot_cy, 16.0, Icon::Pointer, col::TEXT3);
                self.kit_text_mid(left + 24.0, foot_cy, fpx, &s, col::TEXT3);
            }
        }

        // Touch: the rows, then the switches, then the picture (see the
        // `PAD_CELL_*` indices).
        cells.extend(player_cells);
        cells.extend(layer_cells);
        cells.extend(icon_cells);
        ui_cells_publish(ui_screen_kind(), cells);

        unsafe {
            glUseProgram(0);
            glBindVertexArray(0);
        }
        self.gl_state.invalidate();
    }

    /// One row's value in the controls editor, from `x`, at most `max_w` wide.
    #[allow(clippy::too_many_arguments)]
    fn pad_value(&mut self, x: f32, cy: f32, max_w: f32, row_h: f32, value: &PadValue, button: &str, px: f32) {
        use theme::col;
        let u = crate::loc_ui::ui();
        match value {
            PadValue::Key(k) => {
                self.pad_key(x, cy, max_w, row_h, k, px, false);
            }
            PadValue::None | PadValue::Inherited(None) => {
                let t = self.kit_fit(u.key_none, px, max_w);
                self.kit_text_mid(x, cy, px, &t, TOUCHES_QUIET);
            }
            PadValue::Inherited(Some(k)) => {
                // The key the layer falls back to, quieter, and why it is there.
                let cpx = pad_caption_px(px);
                let caption = u.key_same_as_normal;
                let cw = self.kit_measure(caption, cpx) + 10.0;
                let kw = self.pad_key(x, cy, (max_w - cw).max(max_w * 0.5), row_h, k, px, true);
                let room = (x + max_w - (x + kw + 10.0)).max(0.0);
                let c = self.kit_fit(caption, cpx, room);
                self.kit_text_mid(x + kw + 10.0, cy, cpx, &c, col::TEXT3);
            }
            PadValue::Modifier => {
                // Not the stored binding: it is still in the keymap, and it will
                // work again the day the layer is emptied, but showing it here
                // would promise a key press that `main.cpp` mutes.
                let isz = (px + 1.0).round();
                self.kit_icon(x, cy, isz, Icon::Layers, col::TEXT3);
                let label = crate::loc::s().keys_modifier.replace("{}", button);
                let t = self.kit_fit(&label, px, (max_w - isz - 7.0).max(0.0));
                self.kit_text_mid(x + isz + 7.0, cy, px, &t, col::TEXT3);
            }
        }
    }

    /// A key as the controls editor shows it, from `x`: a keycap for a key of
    /// the keyboard, the mouse beside a click, the keyboard icon beside the
    /// console keyboard (neither of those is a key to press). `dim` for the key
    /// a combo layer only inherits. Returns the width taken.
    #[allow(clippy::too_many_arguments)]
    fn pad_key(&mut self, x: f32, cy: f32, max_w: f32, row_h: f32, name: &str, px: f32, dim: bool) -> f32 {
        use theme::col;
        let label = crate::keymap::flash_key_display(name);
        let icon = match name {
            "Left click" | "Right click" | "Clic gauche" | "Clic droit" => Some(Icon::Mouse),
            "Keyboard" => Some(Icon::Keyboard),
            _ => None,
        };
        match icon {
            Some(ic) => {
                let isz = (px + 1.0).round();
                self.kit_icon(x, cy, isz, ic, if dim { col::TEXT3 } else { col::TEXT2 });
                let t = self.kit_fit(&label, px, (max_w - isz - 7.0).max(0.0));
                isz + 7.0 + self.kit_text_mid(x + isz + 7.0, cy, px, &t, if dim { col::TEXT3 } else { col::TEXT })
            }
            None => self.kit_keycap(x, cy, max_w, (row_h - 6.0).clamp(14.0, 24.0), &label, px, dim),
        }
    }

    /// The window the controls editor and its key picker share: the page dimmed
    /// behind, one raised panel `h` tall (at most the screen) across the whole
    /// width. Both screens use it, so going from a row to its keys and back
    /// changes what is in the window, not the window: they used to be a 1180 px
    /// and a 1120 px panel, each popping in over the other. Returns the panel.
    fn touches_window(&mut self, h: f32) -> (f32, f32, f32, f32) {
        unsafe {
            glEnable(GL_BLEND);
            glBlendFunc(GL_SRC_ALPHA, GL_ONE_MINUS_SRC_ALPHA);
            glDisable(GL_STENCIL_TEST);
        }
        let vw = self.dimensions.width as f32;
        let vh = self.dimensions.height as f32;
        self.fill_screen_dim(theme::col::SCRIM);
        let w = vw - 2.0 * TOUCHES_MARGIN;
        let h = h.min(vh - 2.0 * TOUCHES_MARGIN);
        let (x, y) = (TOUCHES_MARGIN, ((vh - h) * 0.5).round());
        self.kit_panel(x, y, w, h, false);
        (x, y, w, h)
    }

    /// The path over the controls editor's title ("Pause › Touches et
    /// curseur"), the last step brighter, as on the sheets it was opened from.
    fn touches_crumbs(&mut self, x: f32, cy: f32, max_w: f32, crumbs: &[&str]) {
        use theme::col;
        let px = theme::size::CAPTION + 1.0;
        let end = x + max_w;
        let mut cx = x;
        for (i, c) in crumbs.iter().enumerate() {
            if i > 0 {
                cx += self.kit_text_mid(cx, cy, px, "  \u{203A}  ", col::TEXT3);
            }
            let ink = if i + 1 == crumbs.len() { col::TEXT2 } else { col::TEXT3 };
            let t = self.kit_fit(c, px, (end - cx).max(0.0));
            cx += self.kit_text_mid(cx, cy, px, &t, ink);
        }
    }

    /// The bottom band of that window: its hairline and the hints at the right.
    /// What sits at the left (the cursor speed, the picker's legend) is the
    /// caller's; returns where the hints start, so it can stay clear of them.
    fn touches_footer(&mut self, x: f32, y: f32, w: f32, h: f32, hints: &[kit::text::HintItem]) -> f32 {
        let line_w = w - 2.0 * TOUCHES_PAD_X;
        self.draw_overlay_rect(x + TOUCHES_PAD_X, y + h - TOUCHES_FOOT_H, line_w, 1.0, theme::col::LINE);
        let right = x + w - TOUCHES_PAD_X;
        self.kit_hints_right(right, y + h - TOUCHES_FOOT_H * 0.5, hints);
        right - self.kit_hints_width(hints)
    }

    /// Width of `pad_segments` with these labels.
    fn pad_segments_width(&mut self, labels: &[&str], live: &[bool]) -> f32 {
        let mut w = 2.0 * SEG_INSET;
        for (i, l) in labels.iter().enumerate() {
            w += self.kit_measure(l, SEG_PX) + 2.0 * SEG_PAD;
            if live.get(i).copied().unwrap_or(false) {
                w += SEG_DOT_ROOM;
            }
        }
        w
    }

    /// A segmented switch as the controls editor's header draws it
    /// (Touches.dc.html): a dark track, the current option gold, and a dot after
    /// an option marked `live` (a combo layer that holds a key, so a layer in
    /// use shows from the Normal tab). Returns each option's rect, for touch.
    fn pad_segments(
        &mut self,
        x: f32,
        cy: f32,
        labels: &[&str],
        active: usize,
        live: &[bool],
    ) -> std::vec::Vec<(f32, f32, f32, f32)> {
        use theme::col;
        const DOT: f32 = 6.0;
        const ON_INK: u32 = 0xFF_1A_14_05;
        let total = self.pad_segments_width(labels, live);
        let y = (cy - SEG_H * 0.5).round();
        self.kit_rrect(x, y, total, SEG_H, 10.0, col::BG);
        let mut sx = x + SEG_INSET;
        let mut rects = std::vec::Vec::with_capacity(labels.len());
        for (i, l) in labels.iter().enumerate() {
            let lw = self.kit_measure(l, SEG_PX);
            let dot = live.get(i).copied().unwrap_or(false);
            let sw = lw + 2.0 * SEG_PAD + if dot { SEG_DOT_ROOM } else { 0.0 };
            let on = i == active;
            if on {
                self.kit_rrect(sx, y + SEG_INSET, sw, SEG_H - 2.0 * SEG_INSET, 8.0, col::GOLD);
            }
            self.kit_text_mid((sx + SEG_PAD).round(), cy, SEG_PX, l, if on { ON_INK } else { col::TEXT2 });
            if dot {
                let dx = (sx + SEG_PAD + lw + 4.0).round();
                self.kit_rrect(dx, (cy - 8.0).round(), DOT, DOT, DOT * 0.5, if on { ON_INK } else { col::GOLD });
            }
            rects.push((sx, y, sw, SEG_H));
            sx += sw;
        }
        rects
    }

    /// The width `kit_pad_badge` draws `label` in at `px`.
    fn pad_badge_width(&mut self, label: &str, px: f32) -> f32 {
        let (glyph, caption) = pad_badge_parts(label);
        match glyph {
            Some(b) => {
                let gw = self.kit_button_width(b, pad_glyph_px(px));
                if caption.is_empty() {
                    gw
                } else {
                    gw + PAD_CAPTION_GAP + self.kit_measure(caption, pad_caption_px(px))
                }
            }
            None => self.kit_measure(label, px),
        }
    }

    /// A controller slot's label as the console draws it, from `x` (left
    /// aligned, so a column of them starts in one place), at most `w` wide:
    /// the button's own glyph, then a small caption for what the glyph alone
    /// does not say, the direction of a stick or of the d-pad, or "press" for a
    /// stick pushed in. The caption is smaller and quieter than the glyph: drawn
    /// at the value's size, the arrow of a "→" row read as "maps to".
    fn kit_pad_badge(&mut self, x: f32, cy: f32, w: f32, label: &str, px: f32, ink: u32) {
        let (glyph, caption) = pad_badge_parts(label);
        let Some(b) = glyph else {
            let t = self.kit_fit(label, px, w);
            self.kit_text_mid(x.round(), cy, px, &t, ink);
            return;
        };
        let gx = x.round();
        let gw = self.kit_button(gx, cy, pad_glyph_px(px), b, ink);
        if !caption.is_empty() {
            // The glyph keeps the row's ink; the caption steps back a shade
            // unless the row is lit or dimmed as a whole.
            let cink = if ink == theme::col::TEXT { theme::col::TEXT2 } else { ink };
            let cpx = pad_caption_px(px);
            let cx = gx + gw + PAD_CAPTION_GAP;
            let c = self.kit_fit(caption, cpx, (x + w - cx).max(0.0));
            self.kit_text_mid(cx, cy, cpx, &c, cink);
        }
    }

    /// TOUCHES key picker (issue #55), shown when A is pressed on a row of the
    /// pad, in the editor's own window. Draws a real PC keyboard from
    /// `keymap::KEYBOARD` (positioned keys, numpad on the RIGHT under its
    /// caption, the arrows in an inverted T), and under it, in a band of their
    /// own, what is not a key: nothing, the two clicks, the console keyboard.
    /// The user navigates geometrically (menu.rs). QWERTY is fixed for every
    /// language.
    ///
    /// It says what is being edited and what it is now: "Key for" and the
    /// button's own glyph (behind its layer's glyph in a combo layer), the
    /// player and layer when they are not the obvious ones, the current key on
    /// a keycap and outlined on the board. A key another button already sends
    /// is teal, the legend at the bottom says so, and the line over it names
    /// that button when the cursor is on it. The cursor glides like the pad's.
    pub(crate) fn draw_touches_keyboard(&mut self, v: &KeysView) {
        use theme::{col, size};
        const KEY_GAP: f32 = 6.0;
        /// The band of the line about the focused key, over the footer.
        const HELP_H: f32 = 36.0;
        /// Under the board: the gap, the band's caption, the gap to its pills.
        const BAND_GAP: f32 = 14.0;
        const BAND_CAPTION: f32 = 22.0;
        const FACE: u32 = 0xFF_22_32_4F;
        const FACE_USED: u32 = 0xFF_2E_6E_63;
        const INK_USED: u32 = 0xFF_EA_F7_F3;
        const LIP: u32 = 0xFF_0E_17_2A;
        const ON_INK: u32 = 0xFF_1A_14_05;

        let u = crate::loc_ui::ui();
        let keys = crate::keymap::KEYBOARD;
        let vw = self.dimensions.width as f32;
        let vh = self.dimensions.height as f32;
        let inner_w = vw - 2.0 * TOUCHES_MARGIN - 2.0 * TOUCHES_PAD_X;
        let unit_w = inner_w / crate::keymap::KEYBOARD_UNITS_W;
        // The keys take the room the window has, but never come out taller
        // than wide: a turned picture gives a narrow window, and its keys
        // shrink with it.
        let board_rows = (crate::keymap::KEYBOARD_ROWS_N - 1) as f32;
        let full = vw >= vh;
        let fixed = TOUCHES_HEAD_H + BAND_GAP + BAND_CAPTION + HELP_H + TOUCHES_FOOT_H;
        let room = vh - 2.0 * TOUCHES_MARGIN - fixed;
        let gap = (KEY_GAP * unit_w / 60.0).clamp(3.0, KEY_GAP);
        let key_h = ((room / (board_rows + 1.0)) - gap).min(unit_w * 0.88).max(18.0);
        let pill_h = (key_h - 6.0).max(16.0);
        let block_h = board_rows * (key_h + gap) + BAND_GAP + BAND_CAPTION + pill_h;
        // In landscape the window is the editor's, to the pixel; turned, it is
        // as tall as its keys need.
        let content_h = TOUCHES_HEAD_H + block_h + 16.0 + HELP_H + TOUCHES_FOOT_H;
        let (x, y, w, h) = self.touches_window(if full { vh } else { content_h });
        let left = x + TOUCHES_PAD_X;
        let right = x + w - TOUCHES_PAD_X;

        // ── The header ───────────────────────────────────────────────────
        self.touches_crumbs(left, y + 30.0, inner_w, v.crumbs);
        let ty = y + 66.0;
        // "Key for" and the button's glyph where the sentence puts it (the
        // button comes first in Turkish and Chinese), its layer's glyph and a
        // plus before it: the title used to be "Touche pour StickLUp".
        let (before, after) = u.key_for.split_once("{}").unwrap_or((u.key_for, ""));
        let bpx = 20.0;
        let mut tx = left;
        tx += self.kit_text_mid(tx, ty, TOUCHES_TITLE_PX, before.trim_end(), col::TEXT);
        if !before.trim_end().is_empty() {
            tx += 10.0;
        }
        if let Some(m) = v.layer {
            let mw = self.pad_badge_width(m, bpx);
            self.kit_pad_badge(tx, ty, mw, m, bpx, col::TEXT2);
            tx += mw + 6.0;
            tx += self.kit_text_mid(tx, ty, TOUCHES_TITLE_PX, "+", col::TEXT3) + 6.0;
        }
        let gw = self.pad_badge_width(v.glyph, bpx);
        self.kit_pad_badge(tx, ty, gw, v.glyph, bpx, col::GOLD);
        tx += gw;
        if !after.trim_start().is_empty() {
            tx += 10.0;
            tx += self.kit_text_mid(tx, ty, TOUCHES_TITLE_PX, after.trim_start(), col::TEXT);
        }
        // On the right, where the editor has its switches: the player and the
        // layer when they are not the obvious ones, and the key it sends now.
        let mut ctx: std::vec::Vec<std::string::String> = std::vec::Vec::new();
        if v.player == 2 {
            ctx.push(u.player_2.to_string());
        }
        if let Some(m) = v.layer {
            ctx.push(u.key_layer.replacen("{}", m, 1));
        }
        ctx.push(u.key_current.to_string());
        let cpx = size::LABEL;
        let cur_w = self.keys_current_width(v.current, cpx);
        for drop_parts in [false, true] {
            let words = if drop_parts { u.key_current.to_string() } else { ctx.join("  \u{00B7}  ") };
            let ww = self.kit_measure(&words, cpx);
            let cw = ww + 10.0 + cur_w;
            if right - cw >= tx + 24.0 {
                let cx0 = right - cw;
                self.kit_text_mid(cx0, ty, cpx, &words, col::TEXT3);
                self.keys_current(cx0 + ww + 10.0, ty, v.current, cpx);
                break;
            }
        }
        self.draw_overlay_rect(left, y + TOUCHES_HEAD_H - 6.0, inner_w, 1.0, col::LINE);

        // ── The board ────────────────────────────────────────────────────
        let area_top = y + TOUCHES_HEAD_H;
        let area_bot = y + h - TOUCHES_FOOT_H - HELP_H;
        let top_y = (area_top + ((area_bot - area_top) - block_h) * 0.5).max(area_top).round();
        let band_y = top_y + board_rows * (key_h + gap) + BAND_GAP;
        let pills_y = band_y + BAND_CAPTION;
        let rect_of = |i: usize| -> (f32, f32, f32, f32) {
            let Some(&(_, row, kx, kw)) = keys.get(i) else { return (0.0, 0.0, 0.0, 0.0) };
            let kx0 = left + kx * unit_w;
            let kw0 = kw * unit_w - gap;
            if row as usize >= crate::keymap::KEYBOARD_ROWS_N - 1 {
                (kx0, pills_y, kw0, pill_h)
            } else {
                (kx0, top_y + row as f32 * (key_h + gap), kw0, key_h)
            }
        };
        let radius_of = |i: usize, kh: f32| -> f32 {
            match keys.get(i) {
                Some(k) if k.1 as usize >= crate::keymap::KEYBOARD_ROWS_N - 1 => kh * 0.5,
                _ => (key_h * 0.18).min(8.0),
            }
        };
        // The numpad's name over it, on the row the function keys leave empty
        // there: "N1".."NEnt" said it in two letters, the caption says it once.
        {
            let npx = size::CAPTION;
            let t = self.kit_fit(u.keys_numpad, npx, 4.0 * unit_w - gap);
            let tw = self.kit_measure(&t, npx);
            let ncx = left + 18.0 * unit_w - gap * 0.5;
            self.kit_text_mid((ncx - tw * 0.5).round(), top_y + key_h * 0.5, npx, &t, col::TEXT3);
        }
        self.kit_section(left, band_y + 8.0, u.keys_other);

        let current = match v.current {
            PadValue::Key(k) => k.as_str(),
            _ => "(none)",
        };
        let in_layer = v.layer.is_some();
        let mut cells: std::vec::Vec<(f32, f32, f32, f32)> = std::vec::Vec::with_capacity(keys.len());
        for (i, &(name, _, _, _)) in keys.iter().enumerate() {
            let (kx, ky, kw, kh) = rect_of(i);
            cells.push((kx, ky, kw, kh));
            let used = v.used.contains_key(name);
            let (face, ink) = if used { (FACE_USED, INK_USED) } else { (FACE, col::TEXT2) };
            let r = radius_of(i, kh);
            // A cap with a lip: the face, and a darker band under it, so the
            // keys read as keys.
            self.kit_rrect(kx, ky + 2.0, kw, kh, r, LIP);
            self.kit_rrect(kx, ky, kw, kh - 2.0, r, face);
            self.keys_legend(kx, ky, kw, kh, name, in_layer, ink);
            // The key it sends now, ringed just outside its cap, lip included:
            // once the cursor has left it, it is still on the board.
            if name == current {
                self.kit_shape(kx - 3.0, ky - 3.0, kw + 6.0, kh + 7.0, r + 3.0, 0, 2.0, col::GOLD, 0.0);
            }
        }

        // The cursor: a gold cap over the board, gliding from key to key on
        // its centre like the pad's focus, with the key's legend dark on it.
        if v.cursor < keys.len() {
            let (kx, ky, kw, kh) = rect_of(v.cursor);
            let now = unsafe { ruffle_tick_now() };
            let ecx = eased_list_x(kx + kw * 0.5, GLIDE_KEY_KEYBOARD, now);
            let ecy = eased_list_y(ky + kh * 0.5, GLIDE_KEY_KEYBOARD, now);
            let r = radius_of(v.cursor, kh);
            self.kit_rrect(ecx - kw * 0.5, ecy - kh * 0.5, kw, kh - 2.0, r, col::GOLD);
            if let Some(&(name, _, _, _)) = keys.get(v.cursor) {
                self.keys_legend(ecx - kw * 0.5, ecy - kh * 0.5, kw, kh, name, in_layer, ON_INK);
            }
        }

        // ── The line about the focused key, and the footer ───────────────
        let help_cy = y + h - TOUCHES_FOOT_H - HELP_H * 0.5;
        let focused = keys.get(v.cursor).map(|k| k.0).unwrap_or("");
        match v.used.get(focused) {
            // Who already sends it, as their glyphs: two buttons on one key is
            // allowed, but it should be a choice.
            Some(owners) if !owners.is_empty() => {
                let (pre, post) = u.key_used_by.split_once("{}").unwrap_or((u.key_used_by, ""));
                let mut hx = left;
                self.kit_rrect(hx, help_cy - 6.0, 12.0, 12.0, 3.0, FACE_USED);
                hx += 12.0 + 12.0;
                hx += self.kit_text_mid(hx, help_cy, size::LABEL, pre.trim_end(), col::TEXT2) + 8.0;
                for g in owners.iter().take(6) {
                    let bw = self.pad_badge_width(g, size::LABEL);
                    self.kit_pad_badge(hx, help_cy, bw, g, size::LABEL, col::TEXT);
                    hx += bw + 8.0;
                }
                if !post.trim().is_empty() {
                    self.kit_text_mid(hx, help_cy, size::LABEL, post.trim(), col::TEXT2);
                }
            }
            _ => {
                let help = match focused {
                    "(none)" if in_layer => u.help_key_inherit,
                    "(none)" => u.help_key_none,
                    "Keyboard" => u.help_key_keyboard,
                    "Left click" | "Right click" => u.help_key_click,
                    _ => "",
                };
                if !help.is_empty() {
                    self.kit_icon(left, help_cy, 16.0, Icon::Info, col::TEXT3);
                    let t = self.kit_fit(help, size::LABEL, inner_w - 26.0);
                    self.kit_text_mid(left + 26.0, help_cy, size::LABEL, &t, col::TEXT2);
                }
            }
        }
        // X empties the button without walking to "None": in a combo layer
        // that gives it back its Normal key, and the hint says which.
        let x_verb = if in_layer { u.key_same_as_normal } else { u.key_none };
        let hints: [kit::text::HintItem; 3] = [(&[Btn::A], u.choose), (&[Btn::X], x_verb), (&[Btn::B], u.cancel)];
        let hints_x = self.touches_footer(x, y, w, h, &hints);
        // What teal means, once, where the eye ends up.
        let fpx = size::CAPTION + 1.0;
        if left + 20.0 + self.kit_measure(u.key_used, fpx) + 24.0 <= hints_x {
            let foot_cy = y + h - TOUCHES_FOOT_H * 0.5;
            self.kit_rrect(left, foot_cy - 6.0, 12.0, 12.0, 3.0, FACE_USED);
            self.kit_text_mid(left + 20.0, foot_cy, fpx, u.key_used, col::TEXT3);
        }
        ui_cells_publish(ui_screen_kind(), cells);

        unsafe {
            glUseProgram(0);
            glBindVertexArray(0);
        }
        self.gl_state.invalidate();
    }

    /// The legend of key `name` in its cap at (`x`, `y`, `w`, `h`): its
    /// keyboard legend (`keymap::keyboard_label`), smaller before shorter, the
    /// way real keyboards print their word keys smaller than their letters; a
    /// key of the band gets its icon too.
    #[allow(clippy::too_many_arguments)]
    fn keys_legend(&mut self, x: f32, y: f32, w: f32, h: f32, name: &'static str, in_layer: bool, ink: u32) {
        let u = crate::loc_ui::ui();
        let (label, icon): (std::borrow::Cow<'static, str>, Option<Icon>) = match name {
            // In a combo layer an empty button sends its Normal key, so the
            // key that empties it says that instead of "None".
            "(none)" if in_layer => (std::borrow::Cow::Borrowed(u.key_same_as_normal), Some(Icon::Undo)),
            "(none)" => (std::borrow::Cow::Borrowed(u.key_none), Some(Icon::Close)),
            "Left click" | "Right click" => (crate::keymap::keyboard_label(name), Some(Icon::Mouse)),
            "Keyboard" => (crate::keymap::keyboard_label(name), Some(Icon::Keyboard)),
            _ => (crate::keymap::keyboard_label(name), None),
        };
        let big = (h * 0.36).clamp(10.0, 17.0);
        let small = (big * 0.76).round().max(10.0);
        let isz = if icon.is_some() { (big + 1.0).round() } else { 0.0 };
        let room = w - 12.0 - if icon.is_some() { isz + 8.0 } else { 0.0 };
        let px = if self.kit_measure(&label, big) <= room { big } else { small };
        let l = self.kit_fit(&label, px, room.max(0.0));
        let lw = self.kit_measure(&l, px);
        let total = lw + if icon.is_some() { isz + 8.0 } else { 0.0 };
        let mut cx = (x + (w - total) * 0.5).round();
        let cy = y + (h - 2.0) * 0.5;
        if let Some(ic) = icon {
            self.kit_icon(cx, cy, isz, ic, ink);
            cx += isz + 8.0;
        }
        self.kit_text_mid(cx, cy, px, &l, ink);
    }

    /// Width `keys_current` takes for `value` at `px`.
    fn keys_current_width(&mut self, value: &PadValue, px: f32) -> f32 {
        match value {
            PadValue::Key(k) | PadValue::Inherited(Some(k)) => {
                let label = crate::keymap::flash_key_display(k);
                let icon = matches!(k.as_str(), "Left click" | "Right click" | "Clic gauche" | "Clic droit" | "Keyboard");
                let lw = self.kit_measure(&label, px);
                if icon {
                    (px + 1.0).round() + 7.0 + lw
                } else {
                    (lw + 18.0).max(24.0)
                }
            }
            _ => self.kit_measure(crate::loc_ui::ui().key_none, px),
        }
    }

    /// The key a button sends now, as its row shows it (a keycap, a click...),
    /// for the picker's header.
    fn keys_current(&mut self, x: f32, cy: f32, value: &PadValue, px: f32) {
        match value {
            PadValue::Key(k) => {
                self.pad_key(x, cy, 400.0, 30.0, k, px, false);
            }
            PadValue::Inherited(Some(k)) => {
                self.pad_key(x, cy, 400.0, 30.0, k, px, true);
            }
            _ => {
                self.kit_text_mid(x, cy, px, crate::loc_ui::ui().key_none, TOUCHES_QUIET);
            }
        }
    }

    // ── Library UI (Phase 3.4) ──────────────────────────────────────────

    /// Upload an RGBA8 byte buffer as a standalone GL texture (not packed
    /// into any atlas). Used by the library boot path to upload
    /// `assets/banner.png` as a single texture that survives until the
    /// library renderer is dropped. Returns the GL id, or 0 on failure.
    pub fn upload_rgba_texture(&mut self, rgba: &[u8], width: u32, height: u32) -> GLuint {
        if width == 0 || height == 0 || rgba.len() < (width as usize) * (height as usize) * 4 {
            return 0;
        }
        let mut tex: GLuint = 0;
        unsafe {
            glGenTextures(1, &mut tex);
            glBindTexture(GL_TEXTURE_2D, tex);
            glPixelStorei(GL_UNPACK_ALIGNMENT, 1);
            glTexImage2D(
                GL_TEXTURE_2D,
                0,
                GL_RGBA8 as GLint,
                width as GLsizei,
                height as GLsizei,
                0,
                GL_RGBA,
                GL_UNSIGNED_BYTE,
                rgba.as_ptr() as *const _,
            );
            glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, GL_LINEAR as GLint);
            glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER, GL_LINEAR as GLint);
            glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_WRAP_S, GL_CLAMP_TO_EDGE as GLint);
            glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_WRAP_T, GL_CLAMP_TO_EDGE as GLint);
            glBindTexture(GL_TEXTURE_2D, 0);
        }
        // The cache thinks unit 0 is bound to whatever was there before. We
        // just clobbered it via the upload binds + unbind — invalidate so
        // the next draw re-binds correctly.
        self.gl_state.invalidate();
        tex
    }

    /// Draw a screen-aligned axis-aligned textured rectangle. Uses the
    /// existing `bitmap_prog` + unit-quad VAO; no per-call buffer upload.
    /// Identity color transform (mult=1, add=0).
    pub fn draw_textured_rect(
        &mut self,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        tex: GLuint,
    ) {
        if tex == 0 || w <= 0.0 || h <= 0.0 {
            return;
        }
        let mat = Matrix {
            a: w,
            b: 0.0,
            c: 0.0,
            d: h,
            tx: swf::Twips::from_pixels(x as f64),
            ty: swf::Twips::from_pixels(y as f64),
        };
        let world = self.world_matrix(&mat);
        let mult = [1.0, 1.0, 1.0, 1.0];
        let add = [0.0, 0.0, 0.0, 0.0];
        let uv_remap = [0.0, 0.0, 1.0, 1.0];
        self.use_bitmap(&world, &mult, &add, tex, &uv_remap, 0, false);
        self.gl_state.bind_vao(self.bitmap_vao);
        unsafe {
            glDrawArrays(GL_TRIANGLES, 0, 6);
        }
    }

    /// Draw `tex` filling the rect (x,y,w,h) with CROP-TO-FILL — no black bars.
    /// Scales the image to cover the whole rect and center-crops the overflow
    /// via a UV remap (the shader does `v_uv = remap.xy + uv * remap.zw`).
    /// `img_w`/`img_h` are the texture's pixel dims, used for the aspect ratio.
    /// This is what makes the cover grid look clean despite mixed cover sizes.
    pub fn draw_textured_rect_cover(
        &mut self,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        tex: GLuint,
        img_w: u32,
        img_h: u32,
        // 0 = invisible, 1 = fully drawn. Used to fade a cover in over the
        // generated tile: swapping between two frames reads as a flicker.
        alpha: f32,
    ) {
        if tex == 0 || w <= 0.0 || h <= 0.0 || img_w == 0 || img_h == 0 {
            return;
        }
        let tile_aspect = w / h;
        let img_aspect = img_w as f32 / img_h as f32;
        // remap = [offset_x, offset_y, scale_x, scale_y] over UV [0,1]. Crop the
        // long axis so the short axis fills the tile (center-cropped).
        let uv_remap = if img_aspect > tile_aspect {
            let fx = tile_aspect / img_aspect; // visible width fraction
            [(1.0 - fx) * 0.5, 0.0, fx, 1.0]
        } else {
            let fy = img_aspect / tile_aspect; // visible height fraction
            [0.0, (1.0 - fy) * 0.5, 1.0, fy]
        };
        let mat = Matrix {
            a: w,
            b: 0.0,
            c: 0.0,
            d: h,
            tx: swf::Twips::from_pixels(x as f64),
            ty: swf::Twips::from_pixels(y as f64),
        };
        let world = self.world_matrix(&mat);
        let a = alpha.clamp(0.0, 1.0);
        // STRAIGHT alpha, and the fade applied to the alpha channel alone.
        //
        // This used to premultiply (`mult = [a,a,a,a]` with `GL_ONE`), which is
        // right only for a texture that is already premultiplied. A cover comes
        // from a decoded PNG and is not: with `GL_ONE` its transparent pixels
        // were added at full strength, so wherever a cover had an alpha channel
        // its see-through corners came out as solid white blocks. Nearly every
        // cover is opaque, which is why it took a logo with cut corners to show
        // it (Super Smash Flash 2, reported 2026-08-21).
        let mult = [1.0, 1.0, 1.0, a];
        let add = [0.0, 0.0, 0.0, 0.0];
        if a <= 0.0 {
            return;
        }
        unsafe {
            glEnable(GL_BLEND);
            glBlendFunc(GL_SRC_ALPHA, GL_ONE_MINUS_SRC_ALPHA);
        }
        self.use_bitmap(&world, &mult, &add, tex, &uv_remap, 0, false);
        self.gl_state.bind_vao(self.bitmap_vao);
        unsafe {
            glDrawArrays(GL_TRIANGLES, 0, 6);
        }
    }

    /// Full-screen black clear used at the top of each library render. We
    /// own the framebuffer here (no Ruffle behind us pre-init).
    pub fn library_clear(&mut self) {
        unsafe {
            glDisable(GL_STENCIL_TEST);
            glDisable(GL_BLEND);
            glClearColor(0.078, 0.125, 0.219, 1.0); // dark navy, matches panels
            glClear(GL_COLOR_BUFFER_BIT | GL_STENCIL_BUFFER_BIT);
            glEnable(GL_BLEND);
            glBlendFunc(GL_SRC_ALPHA, GL_ONE_MINUS_SRC_ALPHA);
        }
        self.gl_state.invalidate();
    }

    /// Empty-state screen — no `.swf` found on SD. Shows where to drop files.
    pub fn draw_library_empty(&mut self) {
        use theme::{col, radius, size};
        self.library_clear();
        let vw = self.dimensions.width as f32;
        let vh = self.dimensions.height as f32;
        let lc = crate::loc::s();
        let u = crate::loc_ui::ui();
        let center = |s: &mut Self, text: &str, px: f32, cy: f32, ink: u32| {
            let w = s.kit_measure(text, px);
            s.kit_text_mid(((vw - w) * 0.5).round(), cy, px, text, ink);
        };
        // An icon, what is missing, where games go (the folder FlashNX really
        // uses, not the default it once had), and the other way to get some.
        let mut cy = 200.0;
        self.kit_icon(((vw - 64.0) * 0.5).round(), cy, 64.0, Icon::Gamepad, col::TEXT3);
        cy += 72.0;
        center(self, lc.empty_title, size::TITLE + 6.0, cy, col::TEXT);
        cy += 48.0;
        center(self, lc.empty_l1, size::BODY, cy, col::TEXT2);
        cy += 40.0;
        let root = crate::library::primary_root();
        let path = std::format!("{}/", root.trim_start_matches("sdmc:").trim_end_matches('/'));
        let pw = self.kit_measure(&path, size::BODY) + 20.0 + 12.0 + 32.0;
        let px0 = ((vw - pw) * 0.5).round();
        self.kit_rrect(px0, cy - 22.0, pw, 44.0, radius::ROW, col::CHIP);
        self.kit_icon(px0 + 16.0, cy, 20.0, Icon::FolderOpen, col::GOLD);
        self.kit_text_mid(px0 + 16.0 + 20.0 + 12.0, cy, size::BODY, &path, col::TEXT);
        cy += 44.0;
        center(self, lc.empty_l3, size::BODY, cy, col::TEXT3);
        cy += 56.0;
        center(self, u.empty_import, size::LABEL + 1.0, cy, col::TEXT3);

        self.kit_footer_band();
        self.kit_hints_right(vw - 40.0, vh - 24.0, &[(&[Btn::A], u.find_games)]);
        unsafe {
            glUseProgram(0);
            glBindVertexArray(0);
        }
        self.gl_state.invalidate();
    }

    /// Slide the whole library-UI content horizontally (tab transitions); no
    /// scale. `library::render` resets this via `clear_ui_transform` afterwards.
    pub fn set_ui_slide(&mut self, x: f32) {
        self.ui_scale = 1.0;
        self.ui_pivot_x = 0.0;
        self.ui_pivot_y = 0.0;
        self.ui_translate_x = x;
        self.ui_translate_y = 0.0;
    }

    /// Scale the whole library-UI content about the screen centre (modal pop).
    /// Frames submitted so far (wraps). Advances exactly once per `submit_frame`,
    /// so it is a frame-rate independent clock for "was the previous draw the
    /// frame right before this one?" — which is what the pause menu needs to tell
    /// a fresh open from a continuation (see `ruffle_draw_menu`).
    pub fn frame_count(&self) -> u32 {
        self.frame_count
    }

    /// The in-game sheet's open/close movement, driven by the same timeline as
    /// the modal pop (`modal_scale_step`): it slides in from the left edge and
    /// back out, instead of scaling about the centre of the screen.
    pub fn set_ui_sheet_slide(&mut self, scale: f32) {
        let p = if modal_anim_closing() {
            scale
        } else {
            ((scale - MODAL_OPEN_FROM) / (1.0 - MODAL_OPEN_FROM)).clamp(0.0, 1.0)
        };
        self.ui_scale = 1.0;
        self.ui_pivot_x = 0.0;
        self.ui_pivot_y = 0.0;
        self.ui_translate_x = -(1.0 - p) * 500.0;
        self.ui_translate_y = 0.0;
    }

    /// The launcher's side panel coming in from the RIGHT edge (a game's
    /// options), on the same timeline as `set_ui_sheet_slide`.
    pub fn set_ui_sheet_slide_right(&mut self, scale: f32) {
        let p = if modal_anim_closing() {
            scale
        } else {
            ((scale - MODAL_OPEN_FROM) / (1.0 - MODAL_OPEN_FROM)).clamp(0.0, 1.0)
        };
        self.ui_scale = 1.0;
        self.ui_pivot_x = 0.0;
        self.ui_pivot_y = 0.0;
        self.ui_translate_x = (1.0 - p) * 560.0;
        self.ui_translate_y = 0.0;
    }

    pub fn set_ui_modal_scale(&mut self, scale: f32) {
        self.ui_scale = scale;
        self.ui_pivot_x = self.dimensions.width as f32 * 0.5;
        self.ui_pivot_y = self.dimensions.height as f32 * 0.5;
        self.ui_translate_x = 0.0;
        self.ui_translate_y = 0.0;
    }

    /// Reset the library-UI transform to identity (before the fixed navbar, and
    /// for screens with no transition).
    /// Size of the real framebuffer, which never turns.
    ///
    /// `self.dimensions` is the LOGICAL viewport Ruffle composes for, and it is
    /// portrait while the picture is turned a quarter. Every glViewport that
    /// targets the screen wants THIS instead: setting the logical one squeezed
    /// the whole frame into the left 720 columns and left the previous, unturned
    /// frame showing in the rest.
    fn physical_dims(&self) -> (u32, u32) {
        if rotation_swaps_axes() {
            (self.dimensions.height, self.dimensions.width)
        } else {
            (self.dimensions.width, self.dimensions.height)
        }
    }

    pub fn clear_ui_transform(&mut self) {
        self.ui_scale = 1.0;
        self.ui_pivot_x = 0.0;
        self.ui_pivot_y = 0.0;
        self.ui_translate_x = 0.0;
        self.ui_translate_y = 0.0;
    }

    /// Library viewport size in pixels (for transition math in `library::render`).
    pub fn screen_size(&self) -> (f32, f32) {
        (self.dimensions.width as f32, self.dimensions.height as f32)
    }

    /// Clip subsequent draws to the LOGICAL rect (x,y,w,h), top-left origin.
    /// Used by the IMPORTER reveal to open/close the file list through a window
    /// (the window's `library_clear` glClear is confined to it, too), and by the
    /// scrolling row lists to hold their rows inside their band.
    ///
    /// The turn has to be undone here. `world_matrix` composes `game_rotation()`
    /// into every on-screen draw, so a panel drawn over a turned game lands
    /// somewhere the caller's logical rect does not describe; the scissor is
    /// axis-aligned in the FRAMEBUFFER, which never turns. This was harmless
    /// while the only caller was the launcher (where the rotation is forced back
    /// to 0), and stopped being harmless the moment the in-game keymap list
    /// started clipping its own band.
    ///
    /// A quarter-turn of an axis-aligned rect is still axis-aligned, so mapping
    /// the two corners is exact rather than a conservative bounding box.
    pub fn set_clip(&mut self, x: f32, y: f32, w: f32, h: f32) {
        let (pw, ph) = self.physical_dims();
        let (pw, ph) = (pw as f32, ph as f32);
        // Logical rect corners -> physical, top-left origin. Same mapping as the
        // translation half of `world_matrix`.
        let (x0, y0, x1, y1) = (x, y, x + w, y + h);
        let (px0, py0, px1, py1) = match game_rotation() {
            1 => (pw - y1, x0, pw - y0, x1),
            2 => (pw - x1, ph - y1, pw - x0, ph - y0),
            3 => (y0, ph - x1, y1, ph - x0),
            _ => (x0, y0, x1, y1),
        };
        // Clamp to the framebuffer, then convert to GL's bottom-left origin.
        let cx0 = px0.max(0.0).min(pw);
        let cx1 = px1.max(0.0).min(pw);
        let cy0 = py0.max(0.0).min(ph);
        let cy1 = py1.max(0.0).min(ph);
        unsafe {
            glEnable(GL_SCISSOR_TEST);
            glScissor(
                cx0 as GLint,
                (ph - cy1) as GLint,
                (cx1 - cx0).max(0.0) as GLsizei,
                (cy1 - cy0).max(0.0) as GLsizei,
            );
        }
    }

    /// Disable the scissor clip set by `set_clip`.
    pub fn clear_clip(&mut self) {
        unsafe {
            glDisable(GL_SCISSOR_TEST);
        }
    }

    /// Chrome for the IMPORTER reveal window (x,y,w,h): dim everything OUTSIDE it
    /// and draw a bright border around it, so the opening/closing rectangle reads
    /// clearly over the same-coloured list behind. Call after the clipped content.
    pub fn draw_reveal_chrome(&mut self, x: f32, y: f32, w: f32, h: f32) {
        let vw = self.dimensions.width as f32;
        let vh = self.dimensions.height as f32;
        // Dim the four panes outside the window (darkens the list behind so the
        // opening window pops). Shrinks to nothing as the window fills the screen.
        let dim = 0x88_05_0A_16;
        let panes = [
            (0.0, 0.0, vw, y),
            (0.0, y + h, vw, vh - (y + h)),
            (0.0, y, x, h),
            (x + w, y, vw - (x + w), h),
        ];
        for (rx, ry, rw, rh) in panes {
            if rw > 0.0 && rh > 0.0 {
                self.draw_overlay_rect(rx, ry, rw, rh, dim);
            }
        }
        // A thin gold edge on the window, outside it, so it reads against the
        // navy page and the navy content alike.
        self.kit_ring(x, y, w, h, 2.0, 0.0, 3.0, theme::col::GOLD);
    }

    /// Draw the game-reveal content for the rect (x,y,w,h): the game's cover
    /// LETTERBOXED (fit, keeps aspect — no crop, no stretch; black bars fill the
    /// rest) if it has one, else its colour chip + initials. Used full-screen as
    /// the launch/quit reveal window's content.
    pub fn draw_game_reveal_tile(
        &mut self,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        basename: &str,
        display_name: &str,
        color_chip: u32,
    ) {
        // FULL resolution here, not the gallery's tile thumbnail: this fills the
        // screen, where a 256-wide thumbnail upscales to visible mush.
        match self.cover_full_for(basename) {
            CoverTex::Image { tex, w: iw, h: ih, amb } if iw > 0 && ih > 0 => {
                // The cover whole, over its own blurred backdrop rather than
                // black bars: the box grows from the tile to the full screen,
                // and the art stays the art the tile showed.
                self.draw_overlay_rect(x, y, w, h, 0xFF_00_00_00);
                if amb != 0 {
                    self.draw_textured_rect_cover(x, y, w, h, amb, iw, ih, 1.0);
                }
                let cover_aspect = iw as f32 / ih as f32;
                let win_aspect = w / h;
                let (dw, dh) = if cover_aspect > win_aspect {
                    (w, w / cover_aspect)
                } else {
                    (h * cover_aspect, h)
                };
                self.draw_textured_rect(x + (w - dw) * 0.5, y + (h - dh) * 0.5, dw, dh, tex);
            }
            _ => {
                // No cover: the tile the library showed, its colour and the
                // game's name, grown with the window.
                let chip = color_chip & 0x00FF_FFFF;
                self.draw_overlay_rect(x, y, w, h, 0xFF00_0000 | kit::cover::mix_rgb(chip, theme::col::BG & 0x00FF_FFFF, 0.55));
                let px = (h / 9.0).clamp(18.0, 64.0).round();
                let lines = self.kit_wrap(display_name, px, w * 0.8, 3);
                let lh = (px * 1.25).round();
                let mut cy = y + (h - lh * (lines.len() as f32 - 1.0)) * 0.5;
                let ink = 0xFF00_0000 | kit::cover::mix_rgb(chip, 0xFF_FF_FF, 0.85);
                for line in &lines {
                    let tw = self.kit_measure(line, px);
                    self.kit_text_mid((x + (w - tw) * 0.5).round(), cy, px, line, ink);
                    cy += lh;
                }
            }
        }
    }

    /// Draw a solid AARRGGBB rectangle (x,y,w,h). Used for the reveal's letterbox
    /// bars and the launch fade-to-black overlay.
    pub fn draw_overlay_rect(&mut self, x: f32, y: f32, w: f32, h: f32, rgba: u32) {
        if w <= 0.0 || h <= 0.0 {
            return;
        }
        let m = Matrix {
            a: w, b: 0.0, c: 0.0, d: h,
            tx: swf::Twips::from_pixels(x as f64),
            ty: swf::Twips::from_pixels(y as f64),
        };
        <Self as CommandHandler>::draw_rect(self, swf::Color::from_rgba(rgba), m);
    }

    /// Loading spinner: N dots in a circle whose brightness waves around, driven
    /// by `now` ticks. Shown on the IMPORTER async-fetch screen.
    pub fn draw_spinner(&mut self, cx: f32, cy: f32, radius: f32, now: u64) {
        // Eight round dots on a ring, a brightness wave running round them.
        const N: usize = 8;
        let two_pi = 2.0 * core::f32::consts::PI;
        let freq = unsafe { ruffle_tick_freq() } as f32;
        let phase = if freq > 0.0 { (now as f32 / freq) * 6.0 } else { 0.0 };
        let dot = (radius * 0.30).max(5.0);
        for i in 0..N {
            let a = two_pi * (i as f32) / (N as f32);
            let dx = cx + radius * approx_sin(a + core::f32::consts::FRAC_PI_2); // cos
            let dy = cy + radius * approx_sin(a);
            let b = (approx_sin(phase - a) * 0.5 + 0.5).clamp(0.0, 1.0);
            let alpha = (50.0 + b * 205.0) as u32;
            let rgba = (alpha << 24) | (theme::col::GOLD & 0x00FF_FFFF);
            self.kit_rrect(dx - dot * 0.5, dy - dot * 0.5, dot, dot, dot * 0.5, rgba);
        }
    }

    /// Loading panel content for the IMPORTER async fetch: the URL/item title
    /// above a centred spinner. The caller fills the window with an opaque panel
    /// first (so the URL list behind is replaced, not seen through).
    pub fn draw_loading_panel(&mut self, title: &str, now: u64) {
        use theme::{col, size};
        let vw = self.dimensions.width as f32;
        let vh = self.dimensions.height as f32;
        let t = self.kit_fit_mid(title, size::BODY + 1.0, vw - 160.0);
        let tw = self.kit_measure(&t, size::BODY + 1.0);
        self.kit_text_mid(((vw - tw) * 0.5).round(), vh * 0.5 - 70.0, size::BODY + 1.0, &t, col::TEXT);
        self.draw_spinner(vw * 0.5, vh * 0.5 + 8.0, 26.0, now);
    }

    /// The footer band of a wait that can be cancelled: B, and what it does.
    pub fn draw_cancel_hint(&mut self) {
        let vw = self.dimensions.width as f32;
        let vh = self.dimensions.height as f32;
        self.kit_footer_band();
        self.kit_hints_right(vw - 40.0, vh - 24.0, &[(&[Btn::B], crate::loc_ui::ui().cancel)]);
    }

    /// Draw a full-screen dim rect that ignores the active UI transform, so a
    /// modal's backdrop stays full-screen + still while its panel scales in/out.
    /// Saves + restores the transform around the single draw.
    fn fill_screen_dim(&mut self, rgba: u32) {
        let saved = (
            self.ui_scale, self.ui_pivot_x, self.ui_pivot_y,
            self.ui_translate_x, self.ui_translate_y,
        );
        self.ui_scale = 1.0;
        self.ui_pivot_x = 0.0;
        self.ui_pivot_y = 0.0;
        self.ui_translate_x = 0.0;
        self.ui_translate_y = 0.0;
        let vw = self.dimensions.width as f32;
        let vh = self.dimensions.height as f32;
        let m = Matrix {
            a: vw, b: 0.0, c: 0.0, d: vh,
            tx: swf::Twips::from_pixels(0.0),
            ty: swf::Twips::from_pixels(0.0),
        };
        <Self as CommandHandler>::draw_rect(self, swf::Color::from_rgba(rgba), m);
        self.ui_scale = saved.0;
        self.ui_pivot_x = saved.1;
        self.ui_pivot_y = saved.2;
        self.ui_translate_x = saved.3;
        self.ui_translate_y = saved.4;
    }

    /// Draw the shared chrome of a centered window: the page darkened behind
    /// it, the raised panel, the title on the left with an optional subtitle
    /// under it, and the button hints along the bottom. Returns the panel
    /// geometry so the caller lays out its body via [`ModalFrame`]. The height
    /// is auto-sized to `rows` unless `fixed_h` is given; `danger=true` is the
    /// red panel of a destructive confirm.
    fn draw_modal_frame(
        &mut self,
        width: f32,
        rows: usize,
        fixed_h: Option<f32>,
        danger: bool,
        title: &str,
        subtitle: Option<&str>,
        footer: Option<&str>,
    ) -> ModalFrame {
        use theme::{col, size};
        unsafe {
            glEnable(GL_BLEND);
            glBlendFunc(GL_SRC_ALPHA, GL_ONE_MINUS_SRC_ALPHA);
            glDisable(GL_STENCIL_TEST);
        }
        let vw = self.dimensions.width as f32;
        let vh = self.dimensions.height as f32;

        // Translate-immune backdrop so the panel can scale in without the dim
        // sliding off an edge.
        self.fill_screen_dim(if danger { MODAL_DIM_DANGER } else { col::SCRIM });

        // Never wider than the screen it sits on: the logical viewport is 720
        // wide while the picture is turned a quarter.
        let w = width.min(vw - 2.0 * 20.0);
        let pad_top = if subtitle.is_some() { MODAL_PAD_TOP } else { MODAL_PAD_TOP_TIGHT };
        let h = fixed_h.unwrap_or(pad_top + rows.max(1) as f32 * MODAL_ROW_H + MODAL_PAD_BOTTOM);
        let x = ((vw - w) * 0.5).round();
        let y = ((vh - h) * 0.5).round();
        self.kit_panel(x, y, w, h, danger);

        // Title and subtitle on the left edge of the rows, not centred: the
        // eye then reads down one column, title to first row.
        let tx = x + MODAL_ROW_X;
        let avail = w - 2.0 * MODAL_ROW_X;
        let t = self.kit_fit(title, size::WINDOW_TITLE, avail);
        self.kit_text_mid(tx, y + 44.0, size::WINDOW_TITLE, &t, if danger { col::DANGER } else { col::TEXT });
        if let Some(sub) = subtitle {
            // Cut in the middle: it is usually a game's name.
            let s = self.kit_fit_mid(sub, size::LABEL, avail);
            self.kit_text_mid(tx, y + 78.0, size::LABEL, &s, col::TEXT3);
        }

        if let Some(f) = footer {
            self.draw_overlay_rect(x + 20.0, y + h - 56.0, w - 40.0, 1.0, if danger { col::DANGER_LINE } else { col::LINE });
            self.kit_footer_hints(f, x + w * 0.5, y + h - 28.0, true);
        }

        ModalFrame { x, y, w, h, pad_top }
    }

    /// The hint bar along the bottom of a window drawn with no footer text:
    /// the hairline, then the hints centred under the rows.
    fn modal_hints(&mut self, frame: &ModalFrame, hints: &[kit::text::HintItem], danger: bool) {
        use theme::col;
        let (x, y, w, h) = (frame.x, frame.y, frame.w, frame.h);
        self.draw_overlay_rect(x + 20.0, y + h - 56.0, w - 40.0, 1.0, if danger { col::DANGER_LINE } else { col::LINE });
        self.kit_hints_centered(x + w * 0.5, y + h - 28.0, hints);
    }

    /// `draw_modal_rows`, where bit `i` of `steppers` marks row `i` as one whose
    /// value also steps with Left and Right (see `kit_row`).
    fn draw_modal_rows_stepping(&mut self, frame: &ModalFrame, selection: usize, rows: &[&str], steppers: u32) {
        // Which screen these rows belong to, for the touch table and for the
        // glide key (one key per screen, so the focus never flies in from a
        // panel that is no longer open).
        let touch_kind = ui_screen_kind();
        let top = frame.rows_top();
        let (px, pw) = (frame.x + 16.0, frame.w - 32.0);
        if selection < rows.len() {
            let now = unsafe { ruffle_tick_now() };
            let hy = eased_list_y(top + selection as f32 * MODAL_ROW_H, touch_kind, now);
            self.kit_focus_pill(px, hy + MODAL_ROW_CY - MODAL_PILL_H * 0.5, pw, MODAL_PILL_H);
        }
        let mut cells: std::vec::Vec<(f32, f32, f32, f32)> = std::vec::Vec::with_capacity(rows.len());
        for (i, row) in rows.iter().enumerate() {
            let y = top + i as f32 * MODAL_ROW_H;
            let (label, value) = kit::window::split_label_value(row);
            let stepper = i < 32 && steppers & (1 << i) != 0;
            self.kit_row(px, y + MODAL_ROW_CY, pw, label, value, i == selection, theme::size::ROW, stepper);
            // The whole width of the panel: a row is a target for a thumb.
            cells.push((frame.x + 8.0, y + MODAL_ROW_CY - MODAL_ROW_H * 0.5, (frame.w - 16.0).max(0.0), MODAL_ROW_H));
        }
        ui_cells_publish(touch_kind, cells);
    }

    /// Top navbar: the tab strip switched with the L/R shoulder buttons.
    /// `active` indexes JOUER(0) / IMPORTER(1) / REGLAGES(2). Drawn last, over
    /// the top of every tab-home screen, by `library::render`.
    ///
    /// On the interface kit: the tabs in the console font, the active one white
    /// over a gold bar, and the console's own L and R glyphs either side, which
    /// say "the shoulders switch these" without a footer line spelling it out.
    pub fn draw_navbar(&mut self, active: usize, logo: (GLuint, u32, u32)) {
        use theme::{col, radius, size};
        let vw = self.dimensions.width as f32;
        // The logo, top-left in the strip's band as on the console's own
        // screens, on every tab: it used to be the Play tab's alone, so the
        // other two had no name at all.
        let (logo_tex, logo_w, logo_h) = logo;
        if logo_tex != 0 && logo_w > 0 && logo_h > 0 {
            let draw_h = 30.0;
            let draw_w = logo_w as f32 * draw_h / logo_h as f32;
            self.draw_textured_rect(32.0, 21.0, draw_w, draw_h, logo_tex);
        } else {
            self.kit_text_mid(40.0, 36.0, size::TAB, "FlashNX", col::GOLD);
        }
        let u = crate::loc_ui::ui();
        let tabs = [u.tab_play, u.tab_import, u.tab_settings];

        // The strip's band, for touch: the whole 0..72 height under the tabs.
        let nav_y = 0.0_f32;
        let nav_h = 72.0_f32;
        let cy = 36.0_f32;
        let gap = 30.0_f32;
        let shoulder_px = 22.0_f32;
        let widths = [
            self.kit_measure(tabs[0], size::TAB),
            self.kit_measure(tabs[1], size::TAB),
            self.kit_measure(tabs[2], size::TAB),
        ];
        let lw = self.kit_button_width(Btn::L, shoulder_px);
        let rw = self.kit_button_width(Btn::R, shoulder_px);
        let tabs_w: f32 = widths.iter().sum::<f32>() + gap * (tabs.len() as f32 - 1.0);
        let total = lw + gap + tabs_w + gap + rw;
        let mut x = ((vw - total) * 0.5).round();
        self.kit_button(x, cy, shoulder_px, Btn::L, col::PILL_INK);
        x += lw + gap;
        // Hit boxes: the full height of the strip and half the gap on each side,
        // because a label is not a target for a thumb.
        let mut cells = [(0.0f32, 0.0f32, 0.0f32, 0.0f32); 3];
        for (i, t) in tabs.iter().enumerate() {
            cells[i] = (x - gap * 0.5, nav_y, widths[i] + gap, nav_h);
            let is_active = i == active;
            let ink = if is_active { col::TEXT } else { col::TEXT3 };
            self.kit_text_mid(x, cy, size::TAB, t, ink);
            if is_active {
                self.kit_rrect(x, cy + 16.0, widths[i], 3.0, radius::BAR, col::GOLD);
            }
            x += widths[i] + gap;
        }
        navbar_publish(cells);
        self.kit_button(x, cy, shoulder_px, Btn::R, col::PILL_INK);

        unsafe {
            glUseProgram(0);
            glBindVertexArray(0);
        }
        self.gl_state.invalidate();
    }

    /// The bottom band of the home screens: a hairline, the author credit on
    /// the left and what the buttons do on the right.
    ///
    /// The credit and the version are in the band on the left
    /// (`kit_footer_band`).
    fn kit_home_footer(&mut self) {
        let vw = self.dimensions.width as f32;
        let vh = self.dimensions.height as f32;
        self.kit_footer_band();
        let cy = vh - 24.0;
        let u = crate::loc_ui::ui();
        self.kit_hints_right(
            vw - 40.0,
            cy,
            &[
                (&[Btn::A], u.hint_play),
                (&[Btn::Plus], u.hint_options),
                (&[Btn::Y], u.hint_sort),
                (&[Btn::Minus], u.hint_search),
            ],
        );
    }

    /// The hairline and the left-hand credit every full-page screen ends with.
    /// The author credit is baked into the interface so a rebuilt or rebranded
    /// copy carries it too: removing it is an MIT-attribution violation. The
    /// version rides along with it.
    fn kit_footer_band(&mut self) {
        use theme::{col, size};
        let vw = self.dimensions.width as f32;
        let vh = self.dimensions.height as f32;
        self.draw_overlay_rect(0.0, vh - 48.0, vw, 1.0, col::LINE);
        let credit = std::format!("Jonathan8520  \u{00B7}  v{}", crate::bugreport::APP_VERSION);
        self.kit_text_mid(40.0, vh - 24.0, size::CAPTION + 1.0, &credit, col::TEXT3);
    }

    /// The selected game's facts on one line, the kit way: values in the second
    /// text colour, separated by a dim middle dot. Returns the width drawn.
    fn kit_game_facts(&mut self, x: f32, cy: f32, entry: &crate::library::Entry) -> f32 {
        self.kit_game_facts_px(x, cy, entry, theme::size::LABEL)
    }

    /// Width `kit_game_facts_px` will take, for centring it.
    fn kit_game_facts_width(&mut self, entry: &crate::library::Entry, px: f32) -> f32 {
        let parts = Self::kit_facts_parts(entry);
        let sep = self.kit_measure("  \u{00B7}  ", px);
        let mut w = 0.0;
        for (i, p) in parts.iter().enumerate() {
            if i > 0 {
                w += sep;
            }
            w += self.kit_measure(p, px);
        }
        w
    }

    /// `kit_game_facts` at a chosen size.
    fn kit_game_facts_px(&mut self, x: f32, cy: f32, entry: &crate::library::Entry, px: f32) -> f32 {
        use theme::col;
        let parts = Self::kit_facts_parts(entry);
        let mut pen = x;
        for (i, p) in parts.iter().enumerate() {
            if i > 0 {
                pen += self.kit_text_mid(pen, cy, px, "  \u{00B7}  ", col::TEXT3);
            }
            pen += self.kit_text_mid(pen, cy, px, p, col::TEXT2);
        }
        pen - x
    }

    /// The facts themselves: size, what the file is, the engine, time played.
    fn kit_facts_parts(entry: &crate::library::Entry) -> std::vec::Vec<std::string::String> {
        let mut parts: std::vec::Vec<std::string::String> = std::vec::Vec::with_capacity(4);
        let sz = format_size_pretty(entry.size_bytes);
        if !sz.is_empty() {
            parts.push(sz);
        }
        parts.push(std::format!("SWF {} {}", entry.swf_version, entry.compression_label));
        parts.push(std::string::String::from(if entry.is_as3 { "AS3" } else { "AVM1" }));
        let secs = crate::playtime::get(&entry.basename);
        if secs >= 60 {
            parts.push(crate::loc_ui::playtime(secs));
        }
        parts
    }

    /// The selected game's cover in a detail panel: fitted into the box minus
    /// the caption under it, never blown up past twice its size (small and
    /// sharp reads better than large and soft), the art and its caption
    /// centred as one block. Rounded by the kit's shader, so no page-coloured
    /// corners are painted on. Returns the art's rectangle.
    #[allow(clippy::too_many_arguments)]
    fn kit_cover_panel(
        &mut self,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        caption_h: f32,
        basename: &str,
        name: &str,
        chip: u32,
    ) -> (f32, f32, f32, f32) {
        // Full resolution: these panels are large enough that a tile thumbnail
        // would show as mush.
        let cover = self.cover_full_for(basename);
        let (iw, ih) = match cover {
            CoverTex::Image { w: iw, h: ih, .. } if iw > 0 && ih > 0 => (iw as f32, ih as f32),
            _ => (4.0, 3.0),
        };
        let art_h = (h - caption_h).max(1.0);
        let img_aspect = iw / ih;
        let (mut dw, mut dh) = if img_aspect > w / art_h { (w, w / img_aspect) } else { (art_h * img_aspect, art_h) };
        if matches!(cover, CoverTex::Image { .. }) && dw > iw * 2.0 {
            dw = iw * 2.0;
            dh = ih * 2.0;
        }
        let dx = (x + (w - dw) * 0.5).round();
        let dy = (y + ((h - dh - caption_h) * 0.5).max(0.0)).round();
        match cover {
            CoverTex::Image { tex, w: tw, h: th, .. } => {
                self.kit_shadow(dx, dy + 8.0, dw, dh, theme::radius::TILE, 18.0, 0x70_00_00_00);
                self.kit_image(dx, dy, dw, dh, tex, [0.0, 0.0, 1.0, 1.0], 1.0, theme::radius::TILE);
                let _ = (tw, th);
            }
            CoverTex::Default => self.kit_default_tile(dx, dy, dw, dh, chip, name),
        }
        (dx, dy, dw, dh)
    }

    /// Multi-file launch indicator (v1.3.0): a centered label near the bottom of
    /// the launch/loading reveal, e.g. "MULTI-FILE (6)", telling the user this
    /// game pulls in companion SWFs from its `.files/` folder. The caller gates
    /// when it shows (launch reveal only, once the cover fills the screen).
    pub fn draw_multifile_badge(&mut self, label: &str, count: i32) {
        use theme::{col, size};
        let vw = self.dimensions.width as f32;
        let vh = self.dimensions.height as f32;
        // A pill over the bottom of the cover: an icon of stacked layers and the
        // count, on a dark ground so it reads over any art.
        let text = std::format!("{}  \u{00B7}  {}", label, count);
        let tw = self.kit_measure(&text, size::LABEL + 1.0);
        let w = 18.0 + 20.0 + 10.0 + tw + 20.0;
        let x = ((vw - w) * 0.5).round();
        let cy = vh - 56.0;
        self.kit_rrect(x, cy - 20.0, w, 40.0, 20.0, 0xC8_0A_10_1C);
        self.kit_icon(x + 18.0, cy, 20.0, Icon::Layers, col::GOLD);
        self.kit_text_mid(x + 18.0 + 20.0 + 10.0, cy, size::LABEL + 1.0, &text, col::TEXT);
        unsafe {
            glUseProgram(0);
            glBindVertexArray(0);
        }
        self.gl_state.invalidate();
    }

    /// Frame the content band and name the open shelf on the frame itself.
    ///
    /// Being inside a folder was legible in exactly one place: a line of small
    /// text in the top-left corner, above a grid that looked identical either
    /// way. A player could scroll a shelf for a minute wondering where their
    /// games had gone. A subset of the library has to LOOK like a subset.
    ///
    /// So the page gets a rule above and below its content, and the top one
    /// carries a plaque with the shelf's name, the way a labelled group has been
    /// drawn since forms had legends. It reads instantly and it says WHICH.
    ///
    /// Drawn AFTER the view, at fixed heights that fall in the gaps all four
    /// layouts already leave, so not one of them had to move a pixel to make
    /// room.
    pub fn draw_folder_frame(&mut self, name: &str) {
        // TWO HORIZONTAL RULES, no sides: BANDE and ETAGERE scroll SIDEWAYS and
        // let their tiles run off both edges on purpose, so vertical borders
        // would cut through the artwork. The rules sit in the gaps every layout
        // leaves (y110 above the content, `vh - 50` between the facts and the
        // footer). The folder's NAME is in the header line now, by its icon,
        // where it no longer needs a plate to stay readable over a cover.
        let _ = name;
        let (vw, vh) = (self.dimensions.width as f32, self.dimensions.height as f32);
        const TOP_Y: f32 = 110.0;
        const M: f32 = 20.0;
        let ink = (theme::col::GOLD & 0x00FF_FFFF) | 0x90 << 24;
        self.kit_rrect(M, TOP_Y, vw - 2.0 * M, 2.0, 1.0, ink);
        self.kit_rrect(M, vh - 50.0, vw - 2.0 * M, 2.0, 1.0, ink);
        unsafe {
            glUseProgram(0);
            glBindVertexArray(0);
        }
        self.gl_state.invalidate();
    }

    /// Full-resolution cover for the launch/quit reveal, cached separately from
    /// the gallery's tile thumbnails. Only the game being launched (and the one
    /// just quit) ever lands here, so the cache holds `REVEAL_CACHE_MAX` entries
    /// and deletes the texture it evicts. The full decode costs ~25 ms once,
    /// under an animation that is already covering a game load.
    fn cover_full_for(&mut self, basename: &str) -> CoverTex {
        if let Ok(cache) = reveal_cover_cache().lock() {
            if let Some((_, t)) = cache.iter().find(|(b, _)| b == basename) {
                return *t;
            }
        }
        let resolved = match crate::covers::resolve(basename) {
            crate::covers::Cover::Image(path) => {
                match crate::covers::decode_file(&path) {
                    Some((rgba, w, h)) => {
                        let tex = self.upload_rgba_texture(&rgba, w, h);
                        if tex != 0 {
                            let amb = self.kit_ambient_texture(&rgba, w, h);
                            CoverTex::Image { tex, w, h, amb }
                        } else {
                            CoverTex::Default
                        }
                    }
                    None => CoverTex::Default,
                }
            }
            crate::covers::Cover::Default => CoverTex::Default,
        };
        let evicted = if let Ok(mut cache) = reveal_cover_cache().lock() {
            cache.push((basename.to_string(), resolved));
            if cache.len() > REVEAL_CACHE_MAX {
                Some(cache.remove(0).1)
            } else {
                None
            }
        } else {
            None
        };
        // Full-res covers are megabytes of VRAM each — unlike the tile
        // thumbnails, these can't just be leaked. We're on the GL thread.
        if let Some(CoverTex::Image { tex, amb, .. }) = evicted {
            for t in [tex, amb] {
                if t != 0 {
                    unsafe { glDeleteTextures(1, &t) };
                }
            }
        }
        resolved
    }

    /// Resolve + cache a game's cover texture by basename. Decodes/uploads on
    /// first use; returns `Default` when there's no cover image (caller draws
    /// the generated tile).
    /// A game's cover as `(texture, width, height)` for a panel's header, or
    /// None when it has none (yet).
    pub(crate) fn cover_thumb(&mut self, basename: &str) -> Option<(GLuint, u32, u32)> {
        match self.cover_for(basename) {
            CoverTex::Image { tex, w, h, .. } => Some((tex, w, h)),
            CoverTex::Default => None,
        }
    }

    fn cover_for(&mut self, basename: &str) -> CoverTex {
        if let Some(t) = cover_lookup(basename) {
            return t;
        }
        let t0 = unsafe { ruffle_tick_now() };
        let resolved = match crate::covers::resolve(basename) {
            crate::covers::Cover::Image(path) => {
                // Source length stamps the thumbnail, so a cover replaced on the
                // SD invalidates it without any bookkeeping.
                let src_len = crate::library::file_size(&path);
                if let Some((rgba, w, h)) = crate::covers::read_thumb(basename, src_len) {
                    let tex = self.upload_rgba_texture(&rgba, w, h);
                    if tex != 0 {
                        let amb = self.kit_ambient_texture(&rgba, w, h);
                        CoverTex::Image { tex, w, h, amb }
                    } else {
                        CoverTex::Default
                    }
                } else {
                    match crate::covers::read_cover_bytes(&path)
                        .and_then(|b| crate::covers::decode_bytes(&b))
                    {
                        Some((rgba, w, h)) => {
                            // Shrink to tile size, draw THAT, and leave it on the
                            // SD so the next session skips the full decode.
                            let (small, sw, sh) = crate::covers::downscale_for_tile(rgba, w, h);
                            crate::covers::write_thumb(basename, src_len, &small, sw, sh);
                            let tex = self.upload_rgba_texture(&small, sw, sh);
                            if tex != 0 {
                                let amb = self.kit_ambient_texture(&small, sw, sh);
                                CoverTex::Image { tex, w: sw, h: sh, amb }
                            } else {
                                CoverTex::Default
                            }
                        }
                        None => CoverTex::Default,
                    }
                }
            }
            crate::covers::Cover::Default => CoverTex::Default,
        };
        if let Ok(mut cache) = cover_cache().lock() {
            cache.push((basename.to_string(), resolved, unsafe { ruffle_tick_now() }));
        }
        let dt = unsafe { ruffle_tick_now() }.saturating_sub(t0);
        COVER_DECODE_TICKS.fetch_add(dt, Ordering::Relaxed);
        COVER_DECODE_COUNT.fetch_add(1, Ordering::Relaxed);
        resolved
    }

    /// Cover/logo thumbnail for `url`, cached. NON-BLOCKING: returns the cached
    /// texture if ready, else `None` (the cell shows a "..." placeholder). When
    /// nothing is currently downloading, starts an ASYNC fetch for this url so
    /// the next uncached cell in the iteration kicks off one download; the fetch
    /// is finished by `pump_thumbnail_load` on a later frame. This way the render
    /// thread never blocks on a logo download (some are hundreds of KB).
    fn thumb_for(&mut self, url: &str) -> Option<ThumbTex> {
        if let Some(t) = thumb_lookup(url) {
            return Some(t);
        }
        // Not cached. Start a fetch in a free pool slot so several covers load in
        // PARALLEL: each uncached visible cell tries to grab a slot until the
        // pool is full (`thumb_start` < 0). Skip a url already in flight.
        if let Ok(mut inflight) = thumb_inflight().lock() {
            if !inflight.iter().any(|(_, u)| u == url) {
                let slot = crate::net::thumb_start(url);
                if slot >= 0 {
                    inflight.push((slot, url.to_string()));
                }
            }
        }
        None
    }

    /// Pump the single in-flight thumbnail fetch once per frame. On completion,
    /// decode + upload the logo, cache it (success OR failure so it's not
    /// retried), and clear the in-flight marker so the next cell can start. Call
    /// once at the top of each thumbnail screen's render.
    fn pump_thumbnail_load(&mut self) {
        // Advance every in-flight transfer, then reap the slots that finished.
        crate::net::thumb_pump();
        let done: std::vec::Vec<(i32, std::string::String)> = {
            let inflight = match thumb_inflight().lock() {
                Ok(g) => g,
                Err(_) => return,
            };
            inflight
                .iter()
                .filter(|(slot, _)| crate::net::thumb_slot_status(*slot) != 0)
                .cloned()
                .collect()
        };
        if done.is_empty() {
            return;
        }
        for (slot, url) in &done {
            // Take the bytes (frees the slot), then decode + upload + cache the
            // logo (success OR failure, so it's never retried).
            let state = match crate::net::thumb_slot_take(*slot) {
                Some(bytes) => match crate::covers::decode_bytes(&bytes) {
                    Some((rgba, w, h)) => {
                        let tex = self.upload_rgba_texture(&rgba, w, h);
                        if tex != 0 {
                            ThumbTex::Image { tex, w, h }
                        } else {
                            ThumbTex::Failed
                        }
                    }
                    None => ThumbTex::Failed,
                },
                None => ThumbTex::Failed,
            };
            if let Ok(mut c) = thumb_cache().lock() {
                c.push((url.clone(), state));
            }
        }
        if let Ok(mut inflight) = thumb_inflight().lock() {
            inflight.retain(|(slot, _)| !done.iter().any(|(ds, _)| ds == slot));
        }
    }

    /// JOUER library as a COVER GRID (v1.2.0; replaces the text list). Covers
    /// are mandatory: a game with no sidecar/cached cover gets a generated tile
    /// (per-game color + initials). `selection` is a linear index into
    /// `entries`; `scroll_offset` is the first visible item (multiple of
    /// `LIST_COLS`).
    #[allow(clippy::too_many_arguments)]
    /// Banner + the sub-line under it (active filter, else the library size).
    /// Shared by the three JOUER layouts so the header height is identical in all
    /// of them and switching views never shifts what is above the content band.
    fn draw_home_header(
        &mut self,
        banner_tex: GLuint,
        banner_w: u32,
        banner_h: u32,
        shown: usize,
        filter: Option<&str>,
        total_unfiltered: usize,
    ) {
        use theme::{col, size};
        let vw = self.dimensions.width as f32;
        // The logo is the navbar's (`draw_navbar`), drawn on every tab.
        let _ = (banner_tex, banner_w, banner_h);

        // What the grid is showing, in this order of precedence (unchanged): a
        // search inside an open folder, a search, an open folder, the whole
        // library. A search also ignores the folder, so it wins the line.
        let cy = 94.0;
        let left = 40.0;
        let folder_open = HOME_FOLDER.lock().ok().and_then(|g| g.clone());
        let library_total = HOME_LIBRARY_TOTAL
            .load(core::sync::atomic::Ordering::Relaxed)
            .max(shown);
        let searching = filter.map_or(false, |f| !f.trim().is_empty());
        let mut x = left;
        if searching {
            // "3 / 94" then the term, dimmer: the numbers are the answer, the
            // term is the reminder of the question.
            let count = std::format!("{} / {}", shown, total_unfiltered);
            x += self.kit_text_mid(x, cy, size::LABEL, &count, col::TEXT2);
            let term = std::format!("   \u{201C}{}\u{201D}", filter.unwrap_or("").trim());
            let term = self.kit_fit(&term, size::LABEL, vw * 0.5 - x);
            self.kit_text_mid(x, cy, size::LABEL, &term, col::TEXT3);
        } else if let Some(folder) = folder_open.as_deref() {
            // An open folder: its name, then how much of the library it holds.
            // ZL / ZR walk to the next folder, and say so.
            x += self.kit_icon(x, cy, 18.0, Icon::FolderOpen, col::GOLD) + 10.0;
            let name = self.kit_fit(folder, size::LABEL + 1.0, vw * 0.4);
            x += self.kit_text_mid(x, cy, size::LABEL + 1.0, &name, col::TEXT);
            let count = std::format!("  \u{00B7}  {} / {}", shown, library_total);
            x += self.kit_text_mid(x, cy, size::LABEL, &count, col::TEXT2) + 24.0;
            x += self.kit_button(x, cy, 20.0, Btn::ZL, col::PILL_INK) + 4.0;
            self.kit_button(x, cy, 20.0, Btn::ZR, col::PILL_INK);
        } else {
            x += self.kit_text_mid(x, cy, size::LABEL, &crate::loc_ui::games(shown), col::TEXT2);
            // Folders are walked with buttons on the back of the console, and
            // nothing else on the home says they exist: show the two glyphs, but
            // only while the library has a folder to walk.
            if HOME_HAS_FOLDERS.load(core::sync::atomic::Ordering::Relaxed) {
                x += 24.0;
                x += self.kit_button(x, cy, 20.0, Btn::ZL, col::PILL_INK) + 4.0;
                x += self.kit_button(x, cy, 20.0, Btn::ZR, col::PILL_INK) + 8.0;
                self.kit_text_mid(x, cy, size::LABEL, crate::loc_ui::ui().folders, col::TEXT3);
            }
        }

        // Right: the layout's name. It is changed from REGLAGES, two screens
        // away, and named nowhere else (#98).
        let view = crate::loc_ui::view_label(crate::loc::home_view());
        let vwid = self.kit_measure(view, size::LABEL);
        self.kit_text_mid(vw - 40.0 - vwid, cy, size::LABEL, view, col::TEXT3);
    }

    /// Filled rounded rectangle, built from horizontal slivers.
    ///
    /// Unlike `round_corners`, this draws the shape instead of masking a square
    /// one with the page colour, so it works over anything — a modal sits on a
    /// dimmed screenshot of the library, where painting a "background" colour into
    /// the corners would show the wrong thing.
    fn draw_round_rect(&mut self, x: f32, y: f32, w: f32, h: f32, r: f32, rgba: u32) {
        // The kit's distance-field shape: anti-aliased corners in one draw call,
        // where this used to stack one sliver per pixel row of each corner.
        self.kit_rrect(x, y, w, h, r, rgba);
    }

    /// JOUER layout 1 (issue #52): titles as a text list, the selected game's
    /// cover shown large beside them.
    ///
    /// The grid asks every cover to survive the same crop into the same box. Here
    /// one cover at a time gets a panel big enough to letterbox it, so a 640x76
    /// banner and a 156x175 badge are both fine — the shape problem the grid
    /// cannot solve. The panel also leaves room for what a tile never had: size,
    /// engine, and how long the game has been played.
    ///
    /// Publishes the same `GalleryCell` table as the grid, so 2D navigation, tap
    /// hit-testing and the launch reveal all work unchanged. One game per row
    /// means Up/Down step by one and Left/Right do the same, which is what a list
    /// should do anyway.
    #[allow(clippy::too_many_arguments)]
    pub fn draw_library_list_view(
        &mut self,
        selection: usize,
        scroll_offset: usize,
        entries: &[crate::library::Entry],
        banner_tex: GLuint,
        banner_w: u32,
        banner_h: u32,
        phase_ticks: u64,
        filter: Option<&str>,
        total_unfiltered: usize,
    ) {
        self.library_clear();
        let vw = self.dimensions.width as f32;
        self.draw_home_header(banner_tex, banner_w, banner_h, entries.len(), filter, total_unfiltered);

        const TOP: f32 = 126.0;
        // 40 px rows fit 13 titles between the header and the footer. The first
        // pass used 44 and showed 10, which left a visibly empty band below.
        const ROW_H: f32 = 40.0;
        let rows_visible = crate::library::home_rows_visible();
        let list_x = 40.0;
        let list_w = (vw * 0.50 - 40.0).max(120.0);
        let band_top = TOP - 8.0;
        let band_bot = TOP + rows_visible as f32 * ROW_H;
        if entries.is_empty() {
            self.draw_home_empty(band_top, band_bot);
            self.kit_home_footer();
            return;
        }

        let total = entries.len();
        let mut cells: std::vec::Vec<GalleryCell> = std::vec::Vec::with_capacity(total);
        for idx in 0..total {
            cells.push(GalleryCell {
                row: idx as u32,
                cx: list_x + list_w * 0.5,
                x: list_x,
                y: TOP + idx as f32 * ROW_H,
                w: list_w,
                h: ROW_H,
            });
        }
        if let Ok(mut g) = gallery_cache().lock() {
            *g = (cells, total as u32);
        }

        // One eased scroll drives both the rows and the highlight, so they can
        // never disagree mid-glide the way two separate easings would.
        let (eased_scroll, hl_content_y, _, _) = home_anim_step(
            phase_ticks,
            scroll_offset as f32 * ROW_H,
            TOP + selection as f32 * ROW_H,
            0.0,
            selection as f32,
        );
        let scroll_px = gallery_touch_scroll_read().unwrap_or(eased_scroll);
        if let Ok(mut v) = gallery_view().lock() {
            *v = GalleryView {
                scroll_px,
                pitch: ROW_H,
                band_top,
                band_bot,
                rows_total: total as u32,
                rows_visible: rows_visible as u32,
                horizontal: false,
                off_min: 0.0,
                off_max: 0.0,
            };
        }

        // ── Cover panel, drawn BEFORE the scissor so it is never clipped ──
        // The BOX is generous; the art is fitted inside it and the caption follows
        // the art, so a wide cover and a tall one both sit tight under their own
        // picture instead of leaving a hole under a fixed-size frame.
        let box_x = vw * 0.53;
        let box_w = vw - box_x - 40.0;
        let box_y = TOP + 6.0;
        // 340 gave this layout the SMALLEST cover in the app — 15.4% of the
        // screen — while ~100,000 px² sat empty below the caption, in the one
        // layout whose whole point is the cover panel. The panel now runs to
        // y616; the footer is at 678 and the list band is a disjoint x-range, so
        // nothing else moves. Per-cover gain, since `draw_cover_fitted` caps
        // upscaling at 2× native.
        let box_h = 460.0;
        let mut panel = (box_x, box_y, box_w, box_h);
        if let Some(e) = entries.get(selection) {
            use theme::{col, size};
            // Wrapped once, measured once: the caption's height is what the art
            // is fitted against, so the two cannot disagree.
            let lines = self.kit_wrap(&e.display_name, size::TITLE, box_w, 2);
            let caption_h = 22.0 + lines.len() as f32 * 32.0 + 30.0;
            panel = self.kit_cover_panel(box_x, box_y, box_w, box_h, caption_h, &e.basename, &e.display_name, e.color_chip);
            if crate::favorites::is_favorite(&e.basename) {
                self.kit_favorite_chip(panel.0 + 8.0, panel.1 + 8.0);
            }
            let mut ty = panel.1 + panel.3 + 30.0;
            for line in &lines {
                let lw = self.kit_measure(line, size::TITLE);
                self.kit_text_mid((box_x + ((box_w - lw) * 0.5).max(0.0)).round(), ty, size::TITLE, line, col::TEXT);
                ty += 32.0;
            }
            let fw = self.kit_game_facts_width(e, size::LABEL);
            self.kit_game_facts_px((box_x + ((box_w - fw) * 0.5).max(0.0)).round(), ty + 2.0, e, size::LABEL);
        }

        // ── Title list, clipped to its band ──
        // The top-left to bottom-left flip lives in `set_clip` alone. Spelled out
        // per view, one wrong sign moved the clipped band by its own height and
        // surfaced as a row bleeding over the footer in ONE layout out of four,
        // which costs a build and a netload to see.
        self.set_clip(0.0, band_top, vw, band_bot - band_top);
        // Highlight bar eased on its OWN track: the rows already glide with the
        // scroll, but between two rows of the same screenful the scroll does not
        // move, and a bar pinned to the discrete selection would jump while
        // everything around it slid. Its own easing is what makes moving through
        // the list feel continuous rather than stepped.
        let hl_y = hl_content_y - scroll_px;
        if !entries.is_empty() {
            self.kit_focus_pill(list_x, hl_y, list_w, ROW_H - 4.0);
        }
        for (idx, e) in entries.iter().enumerate() {
            let y = TOP + idx as f32 * ROW_H - scroll_px;
            if y + ROW_H < band_top || y > band_bot {
                continue;
            }

            // Colour dot: the same per-game hue the grid falls back to, kept here
            // so a game stays recognisable between the two layouts.
            let cy = y + (ROW_H - 4.0) * 0.5;
            self.kit_rrect(list_x + 14.0, cy - 5.0, 10.0, 10.0, 5.0, 0xFF00_0000 | e.color_chip);
            let focused = idx == selection;
            let ink = if focused { theme::col::TEXT } else { theme::col::TEXT2 };
            // Favourites also sort to the top, so the star confirms WHY a row is
            // up there rather than announcing something new.
            let fav = crate::favorites::is_favorite(&e.basename);
            let mut text_x = list_x + 36.0;
            if fav {
                self.kit_icon(text_x, cy, 16.0, Icon::Star, theme::col::GOLD);
                text_x += 24.0;
            }
            // Truncated to the column, never to the panel beside it.
            let label = self.kit_fit_mid(&e.display_name, theme::size::ROW - 1.0, list_x + list_w - text_x - 14.0);
            self.kit_text_mid(text_x, cy, theme::size::ROW - 1.0, &label, ink);
        }
        self.clear_clip();

        // Position bar. This was the only scrolling view in the app with no
        // feedback at all — GRILLE has a scrollbar, the horizontal layouts have a
        // rail, LISTE showed 13 of 77 with nothing to say which 13. Placed in the
        // 38 px gutter between the column and the panel, so it sits against the
        // thing it measures instead of at the screen edge.
        self.draw_scrollbar(
            list_x + list_w + 8.0,
            band_top,
            band_bot - band_top,
            scroll_px,
            ROW_H,
            rows_visible,
            entries.len(),
        );

        self.kit_home_footer();

        // The reveal grows from the COVER PANEL, not from a text row: the panel is
        // already showing that game's art at full size, so the launch continues it.
        if let Ok(mut r) = gallery_sel_rect().lock() {
            *r = panel;
        }
    }

    /// The centred key-hint line every full-page screen ends with, at the one
    /// baseline they all use.
    ///
    /// It was copied out at each site — measure, halve, draw at `vh - 42` in
    /// `0x99AABB` — which is how two of them ended up at a different height than
    /// the rest.
    /// The bottom band of a full-page screen, from one of the old footer
    /// strings: hairline, credit on the left, the parsed hints on the right.
    fn draw_page_footer(&mut self, text: &str) {
        let vw = self.dimensions.width as f32;
        let vh = self.dimensions.height as f32;
        self.kit_footer_band();
        self.kit_footer_hints(text, vw - 40.0, vh - 24.0, false);
    }

    /// Centred "NO RESULTS" for a home layout whose list came back empty.
    ///
    /// A search that matches nothing leaves the same `Screen::List` with zero
    /// entries, and each of the four layouts then drew its chrome around nothing
    /// at all: no tiles, no rows, no cover, no word. It was only ever mitigated by
    /// the header's "0 / 77" count, which is a side effect and not a message.
    fn draw_home_empty(&mut self, band_top: f32, band_bot: f32) {
        // Cleared FIRST. The four layouts return early from here without
        // publishing a layout, so the cache would still hold the cells of the
        // last non-empty frame — and `gallery_hit_test` walks exactly that list,
        // so a tap on the empty page would select a game that is not in the list
        // any more, then launch it on the second tap.
        if let Ok(mut g) = gallery_cache().lock() {
            *g = (std::vec::Vec::new(), 0);
        }
        // A search icon over the words: this is a search that found nothing.
        let msg = crate::loc::s().cover_none;
        let vw = self.dimensions.width as f32;
        let cy = (band_top + band_bot) * 0.5;
        self.kit_icon(((vw - 40.0) * 0.5).round(), cy - 30.0, 40.0, Icon::Search, theme::col::TEXT3);
        let w = self.kit_measure(msg, theme::size::BODY);
        self.kit_text_mid(((vw - w) * 0.5).round(), cy + 18.0, theme::size::BODY, msg, theme::col::TEXT2);
    }

    /// JOUER layout 2, BANDE: a large cover of the selected game with its details
    /// beside it, over one full-width row of the whole library.
    ///
    /// Three rules, each fixing something an earlier pass got wrong:
    ///
    /// The columns are FIXED. Sizing the page from the art's own aspect made the
    /// whole layout move every time the selection changed — a banner cover swelled
    /// across the screen, a square one left a hole. The hero is fitted inside a box
    /// it cannot outgrow, so the details and the row never move.
    ///
    /// EVERY game is drawn in the row, none skipped. Hiding the ones already
    /// passed made tiles appear and vanish a step at a time while the row glided,
    /// which is the popping: one discrete rule fighting one continuous one.
    ///
    /// The selection is anchored PART WAY IN, not at the edge, so the previous
    /// cover stays half visible. A shelf you can only see forwards on does not
    /// read as a shelf.
    /// The vertical scrollbar a scrolling page draws in its right-hand gutter: a
    /// track from `top` to `top + h` at `x`, and a thumb whose length is the
    /// visible share of `total` and whose position comes from the live pixel
    /// scroll.
    ///
    /// Seven screens each carried their own copy of this - two `Matrix` literals,
    /// a `max()`, a division guarded three different ways - and the copies stopped
    /// agreeing on the track colour and on the thumb's minimum length. Worse, two
    /// of them still derive their position from the integer row offset even though
    /// the rows they measure have been gliding on an eased pixel scroll since
    /// v1.2.0, so on those two the thumb jumps a full row while the list slides.
    /// Taking `scroll_px` and `pitch` rather than a ready-made fraction is what
    /// makes that last one impossible to reintroduce: there is no longer anywhere
    /// to feed it a stale number.
    fn draw_scrollbar(
        &mut self,
        x: f32,
        top: f32,
        h: f32,
        scroll_px: f32,
        pitch: f32,
        visible: usize,
        total: usize,
    ) {
        // Nothing to say when it all fits. This guard lived at all seven call
        // sites and is the one line of the seven that never diverged.
        if total <= visible || h <= 0.0 {
            return;
        }
        // `.min(h)` is unreachable today (total > visible makes the share < 1, and
        // h is always well over 24 px) and is there so a future short band cannot
        // produce a thumb longer than the track it slides in, which would send the
        // remaining travel negative.
        let thumb_h = (h * visible as f32 / total as f32).max(SCROLLBAR_MIN_THUMB).min(h);
        let max_scroll = (total - visible) as f32 * pitch;
        let t = if max_scroll > 0.0 {
            (scroll_px / max_scroll).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let w = 4.0;
        let bx = x + (SCROLLBAR_W - w) * 0.5;
        self.kit_rrect(bx, top, w, h, w * 0.5, theme::col::LINE);
        self.kit_rrect(bx, top + (h - thumb_h) * t, w, thumb_h, w * 0.5, theme::col::GOLD);
    }

    #[allow(clippy::too_many_arguments)]
    pub fn draw_library_shelf_view(
        &mut self,
        selection: usize,
        _scroll_offset: usize,
        entries: &[crate::library::Entry],
        banner_tex: GLuint,
        banner_w: u32,
        banner_h: u32,
        phase_ticks: u64,
        filter: Option<&str>,
        total_unfiltered: usize,
    ) {
        self.library_clear();
        let vw = self.dimensions.width as f32;
        self.draw_home_header(banner_tex, banner_w, banner_h, entries.len(), filter, total_unfiltered);
        if entries.is_empty() {
            self.draw_home_empty(124.0, 592.0);
            self.kit_home_footer();
            return;
        }

        const HERO_X: f32 = 56.0;
        const HERO_Y: f32 = 124.0;
        // Wider box than the first pass: a banner-shaped cover was being held to
        // 440 px and looked timid next to the empty column beside it. The row moves
        // down to keep its clearance.
        const HERO_W: f32 = 560.0;
        const HERO_H: f32 = 296.0;
        const COL_X: f32 = 652.0;   // details column, to the right of the hero
        const ROW_Y: f32 = 468.0;
        const ROW_H: f32 = 124.0;
        const ROW_W: f32 = 220.0;
        const GAP: f32 = 16.0;
        // Anchor of the selected tile. Left of it there is room for most of the
        // previous cover, which is what makes the row feel like a shelf you are
        // standing in the middle of rather than a queue you are at the head of.
        const ANCHOR: f32 = 196.0;
        let pitch = ROW_W + GAP;
        let total = entries.len();

        // One eased value drives the slide AND the fade, so they cannot disagree.
        let (_, _, eased_off, sel_pos) = home_anim_step(
            phase_ticks,
            0.0,
            0.0,
            ANCHOR - selection as f32 * pitch,
            selection as f32,
        );
        let off = gallery_touch_scroll_read().unwrap_or(eased_off);

        // ── Hero cover, fitted inside its fixed box ──
        let mut hero = (HERO_X, HERO_Y, HERO_W, HERO_H);
        if let Some(e) = entries.get(selection) {
            use theme::{col, size};
            hero = self.kit_cover_panel(HERO_X, HERO_Y, HERO_W, HERO_H, 0.0, &e.basename, &e.display_name, e.color_chip);
            if crate::favorites::is_favorite(&e.basename) {
                self.kit_favorite_chip(hero.0 + 10.0, hero.1 + 10.0);
            }
            // Three lines: the column is wide and the row does not start until
            // y468, so a long title has room for a third.
            let mut ty = HERO_Y + 22.0;
            for line in self.kit_wrap(&e.display_name, size::TITLE + 4.0, vw - COL_X - 48.0, 3) {
                self.kit_text_mid(COL_X, ty, size::TITLE + 4.0, &line, col::TEXT);
                ty += 38.0;
            }
            self.kit_game_facts(COL_X, ty + 6.0, e);
        }
        if let Ok(mut r) = gallery_sel_rect().lock() {
            *r = hero;
        }

        // ── Full-width row ──
        let mut cells: std::vec::Vec<GalleryCell> = std::vec::Vec::with_capacity(total);
        for i in 0..total {
            let x = off + i as f32 * pitch;
            cells.push(GalleryCell {
                row: 0,
                cx: x + ROW_W * 0.5,
                x,
                y: ROW_Y,
                w: ROW_W,
                h: ROW_H,
            });
        }
        if let Ok(mut g) = gallery_cache().lock() {
            *g = (cells, if total == 0 { 0 } else { 1 });
        }
        if let Ok(mut v) = gallery_view().lock() {
            *v = GalleryView {
                scroll_px: off,
                pitch,
                band_top: HERO_Y,
                band_bot: ROW_Y + ROW_H + 16.0,
                rows_total: total as u32,
                rows_visible: 1,
                horizontal: true,
                // Three trailing slots reserved, so the last game rests at
                // x=904 with the row still full behind it instead of sitting
                // alone at x=196 with 864 px of nothing to its right — two
                // thirds of the band, at the end of every library. Still an
                // exact multiple of `pitch` below `off_max`, so dragging to the
                // stop lands on a whole selection.
                //
                // Safe here in a way it would not be in ETAGERE: BANDE keys its
                // hero and its caption to `selection`, not to a pinned screen
                // slot, so nothing ends up described under the wrong cover.
                off_min: ANCHOR - (total.saturating_sub(4)).max(1) as f32 * pitch,
                off_max: ANCHOR,
            };
        }

        let mut decode_budget = COVER_DECODES_PER_FRAME;
        for (i, e) in entries.iter().enumerate() {
            let x = off + i as f32 * pitch;
            if x > vw || x + ROW_W < 0.0 {
                continue;
            }
            // From the SAME eased distance the veil below already uses, so the
            // cover opens up and brightens as one movement across a slide
            // instead of snapping at the frame the index changes.
            let u = (1.0 - (i as f32 - sel_pos).abs()).max(0.0);
            let b = u * u * (3.0 - 2.0 * u); // smoothstep, as ETAGERE's bump uses
            let (ry, rw, rh) = (ROW_Y, ROW_W, ROW_H);
            let cover = match cover_lookup(&e.basename) {
                Some(t) => t,
                None if decode_budget > 0 => {
                    decode_budget -= 1;
                    self.cover_for(&e.basename)
                }
                None => CoverTex::Default,
            };
            let _ = b;
            match cover {
                CoverTex::Image { tex, w, h, amb } => {
                    self.kit_cover(x, ry, rw, rh, tex, w, h, amb, 1.0);
                }
                CoverTex::Default => {
                    self.kit_default_tile(x, ry, rw, rh, e.color_chip, &e.display_name);
                }
            }
            if crate::favorites::is_favorite(&e.basename) {
                self.kit_favorite_chip(x + 8.0, ry + 8.0);
            }
            // Veil measured against the EASED selection, so it lifts and settles
            // with the slide instead of switching at the moment the index changes.
            // Rounded like the tile it darkens.
            let d = (i as f32 - sel_pos).abs().min(1.0);
            let alpha = (d * 0x70 as f32) as u32;
            if alpha > 0 {
                self.kit_rrect(x, ry, rw, rh, theme::radius::TILE, (alpha << 24) | 0x0C_10_18);
            }
        }
        // Selection frame, drawn after the row so it is never veiled, and placed
        // from the eased offset so it travels with the covers.
        if total > 0 {
            // `selection`, NOT `sel_pos`. The tiles are placed from `off` alone;
            // `home_anim_step` eases `off` at 16 and `sel_pos` at 14, so mixing
            // the two put the frame on a geometry the covers were not following
            // and it straddled two of them for the length of every slide. Using
            // the tile's own expression makes disagreement impossible — this is
            // the rule ETAGERE's header comment already states.
            let fx = off + selection as f32 * pitch;
            self.kit_ring(fx, ROW_Y, ROW_W, ROW_H, theme::radius::TILE, theme::ring::GAP, theme::ring::WIDTH, theme::col::GOLD);
        }

        // Position rail: 77 covers scrolled with nothing anywhere saying where in
        // them you are. Same idiom as the other horizontal layout, in the empty
        // band under the row.
        if total > 5 {
            let track_x = 56.0;
            let track_w = vw - 112.0;
            self.kit_rrect(track_x, 621.0, track_w, 4.0, 2.0, theme::col::LINE);
            let tw = (track_w * (vw / pitch) / total as f32).max(56.0);
            let t = (sel_pos / (total - 1) as f32).clamp(0.0, 1.0);
            self.kit_rrect(track_x + (track_w - tw) * t, 620.0, tw, 6.0, 3.0, theme::col::GOLD);
        }

        self.kit_home_footer();
    }

    /// JOUER layout 3, the shelf: ONE row of large covers, the selected one grown
    /// in place, everything under it text.
    ///
    /// The distinction from BANDE is structural, not decorative, and it is the
    /// whole point of this layout: BANDE is TWO objects, a fitted hero plus a
    /// separate ribbon of small chips. Here the row IS the page. The selected
    /// cover is not a second image drawn somewhere else, it is the member of the
    /// row that swelled. An earlier attempt moved BANDE's hero to the left and its
    /// row down, kept the two objects, and was rightly called the same layout.
    ///
    /// Load-bearing rule for whoever maintains this: nothing below the shelf may
    /// ever become a picture. Add a thumbnail or a cover panel down there and this
    /// collapses back into BANDE.
    ///
    /// Selection is marked by SIZE, by the veil lifting, and by the light under
    /// the shelf — never by a frame, which is BANDE's mark.
    #[allow(clippy::too_many_arguments)]
    pub fn draw_library_etagere_view(
        &mut self,
        selection: usize,
        _scroll_offset: usize,
        entries: &[crate::library::Entry],
        banner_tex: GLuint,
        banner_w: u32,
        banner_h: u32,
        phase_ticks: u64,
        filter: Option<&str>,
        total_unfiltered: usize,
    ) {
        self.library_clear();
        let vw = self.dimensions.width as f32;
        self.draw_home_header(banner_tex, banner_w, banner_h, entries.len(), filter, total_unfiltered);

        if entries.is_empty() {
            self.draw_home_empty(124.0, 560.0);
            self.kit_home_footer();
            return;
        }

        const W0: f32 = 248.0;              // resting tile
        const H0: f32 = 160.0;              // box aspect 1.55 at EVERY scale
        const GROW: f32 = 0.30;
        const PITCH: f32 = 304.0;
        const ANCHOR_CX: f32 = 328.0;       // fixed screen x of the active centre
        const SHELF_Y: f32 = 344.0;         // common BOTTOM edge; growth is upward
        const RIGHT_SLOTS: usize = 2;
        const UPSCALE_MAX: f32 = 1.25;
        const VEIL: u32 = 0x0C_10_18;
        let w1 = W0 * (1.0 + GROW);
        let h1 = H0 * (1.0 + GROW);
        let cap_x = ANCHOR_CX - w1 * 0.5;
        let cap_w = 1232.0 - cap_x;

        let n = entries.len();

        // ONE eased number. The offset is DERIVED from it rather than eased on its
        // own track: `home_anim_step` runs them at different rates (16 and 14), so
        // using both let the row and everything keyed to the selection drift apart
        // mid-slide.
        let (_, _, _, mut sel_pos) = home_anim_step(phase_ticks, 0.0, 0.0, 0.0, selection as f32);
        let tail = n.saturating_sub(1 + RIGHT_SLOTS) as f32;
        let off_max = ANCHOR_CX;
        // An exact multiple of PITCH below off_max, so a drag to the far stop lands
        // on a whole selection and never settles back a fraction of a tile.
        let off_min = off_max - tail * PITCH;
        let mut off = (ANCHOR_CX - sel_pos * PITCH).clamp(off_min, off_max);
        if let Some(px) = gallery_touch_scroll_read() {
            off = px.clamp(off_min, off_max);
            sel_pos = ((off_max - off) / PITCH).clamp(0.0, (n - 1) as f32);
            home_anim_set_sel(sel_pos);
        }

        // Per-tile geometry, purely a function of `sel_pos`.
        let bump = |i: usize| -> f32 {
            let u = (1.0 - (i as f32 - sel_pos).abs()).max(0.0);
            u * u * (3.0 - 2.0 * u) // smoothstep
        };
        // smoothstep(f) + smoothstep(1-f) == 1, so the two tiles straddling the
        // selection always sum to the same width: the gap between neighbours is a
        // constant 18.8 px at every phase. Nothing ever overlaps, so draw order is
        // free and plain index order is fine.
        let rect_of = |i: usize, b: f32| -> (f32, f32, f32, f32) {
            let sc = 1.0 + GROW * b;
            let w = W0 * sc;
            let h = H0 * sc;
            (off + i as f32 * PITCH - w * 0.5, SHELF_Y - h, w, h)
        };

        let mut cells: std::vec::Vec<GalleryCell> = std::vec::Vec::with_capacity(n);
        for i in 0..n {
            let (x, y, w, h) = rect_of(i, bump(i));
            cells.push(GalleryCell { row: 0, cx: x + w * 0.5, x, y, w, h });
        }
        let visible: std::vec::Vec<usize> = (0..n)
            .filter(|&i| cells[i].x + cells[i].w >= -8.0 && cells[i].x <= vw + 8.0)
            .collect();
        if let Ok(mut g) = gallery_cache().lock() {
            *g = (cells.clone(), 1);
        }
        if let Ok(mut r) = gallery_sel_rect().lock() {
            *r = cells
                .get(selection)
                .map(|c| (c.x, c.y, c.w, c.h))
                .unwrap_or((cap_x, SHELF_Y - h1, w1, h1));
        }
        if let Ok(mut v) = gallery_view().lock() {
            *v = GalleryView {
                scroll_px: off,
                pitch: PITCH,
                band_top: 150.0,
                band_bot: 392.0,
                rows_total: n as u32,
                rows_visible: 1,
                horizontal: true,
                off_min,
                off_max,
            };
        }

        // Decode the MISSING covers nearest the selection first. Index order would
        // spend the frame's single decode on the half-off-screen leftmost tile
        // instead of the one being looked at.
        let mut misses: std::vec::Vec<usize> = visible
            .iter()
            .copied()
            .filter(|&i| cover_lookup(&entries[i].basename).is_none())
            .collect();
        misses.sort_by(|&a, &b| {
            (a as f32 - sel_pos)
                .abs()
                .partial_cmp(&(b as f32 - sel_pos).abs())
                .unwrap_or(core::cmp::Ordering::Equal)
        });
        for &i in misses.iter().take(COVER_DECODES_PER_FRAME) {
            let _ = self.cover_for(&entries[i].basename);
        }

        // ── The shelf itself ──
        for &i in &visible {
            let e = &entries[i];
            let b = bump(i);
            let (x, y, w, h) = rect_of(i, b);
            let sc = 1.0 + GROW * b;
            match cover_lookup(&e.basename) {
                Some(CoverTex::Image { tex, w: iw, h: ih, amb }) if iw > 0 && ih > 0 => {
                    let a = iw as f32 / ih as f32;
                    let bx = W0 / H0;
                    // Resting zoom floor, a per-cover CONSTANT computed at BASE
                    // scale so it can never reflow: a cover that would have to be
                    // blown up past UPSCALE_MAX to fill the box rests part-way
                    // zoomed out instead of showing a magnified sliver.
                    let (u0, u1) = if a > bx {
                        (H0 / ih as f32, (W0 / a) / ih as f32)
                    } else {
                        (W0 / iw as f32, (H0 * a) / iw as f32)
                    };
                    let t_rest = if u0 <= UPSCALE_MAX || (u0 - u1) < 1e-3 {
                        0.0
                    } else {
                        ((u0 - UPSCALE_MAX) / (u0 - u1)).clamp(0.0, 1.0)
                    };
                    // One continuous zoom-out from the box to the whole cover,
                    // the cover's own blurred colours behind what it leaves.
                    let t = t_rest.max(b);
                    self.kit_cover_reveal(x, y, w, h, tex, iw, ih, amb, t, 1.0);
                }
                _ => self.kit_default_tile(x, y, w, h, e.color_chip, &e.display_name),
            }
            let veil = (0x66 as f32 * (i as f32 - sel_pos).abs().min(1.0)) as u32;
            if veil > 0 {
                self.kit_rrect(x, y, w, h, theme::radius::TILE, (veil << 24) | VEIL);
            }
            if crate::favorites::is_favorite(&e.basename) {
                self.kit_favorite_chip(x + 8.0 * sc, y + 8.0 * sc);
            }
        }

        // ── Light under the shelf ──
        // A dim rule, then a lit segment per tile whose brightness and thickness
        // follow the same bump. At rest exactly one segment is lit under the active
        // cover; mid-slide two are half-lit and the light hands over. A fixed slot
        // bar would be the one element not derived from the eased value.
        self.kit_rrect(24.0, 380.0, 1232.0, 2.0, 1.0, theme::col::LINE);
        for &i in &visible {
            let b = bump(i);
            if b <= 0.01 {
                continue;
            }
            let (x, _, w, _) = rect_of(i, b);
            let a = (b * 255.0) as u32;
            let hgt = 3.0 + 5.0 * b;
            self.kit_rrect(x + 8.0, 380.0, w - 16.0, hgt, hgt * 0.5, (a << 24) | (theme::col::GOLD & 0x00FF_FFFF));
        }

        // ── Everything below the shelf is text ──
        // It was four evenly-spaced rounded chips and a colour bar, and it looked
        // like a web dashboard dropped into a pixel-art launcher: six characters
        // centred in a 251 px box, four times across the screen. Nothing else in
        // FlashNX looks like that. The app's own idiom for secondary information
        // is a plain line in muted blue — the count under the banner, the key
        // hints in the footer, the facts line LISTE and BANDE already use — so
        // this uses the same one, and the shelf stays the only object on the page.
        if let Some(e) = entries.get(selection) {
            // Named from the INTEGER selection, not from `sel_pos.round()`: A
            // launches `selection`, so naming anything else would let the facts
            // describe a different game than the one that starts.
            let lines = self.kit_wrap(&e.display_name, theme::size::TITLE + 6.0, cap_w, 2);
            // 414, not 388: the lit segment above is `3.0 + 7.0 * b` tall from
            // y=380, so at full selection it reaches y=390 and the title's first
            // pixel row landed INSIDE it -- the name looked stuck to the light
            // under the active cover. The clearance has to be measured against
            // the bar's GROWN height, not its resting 3 px, which is what made
            // this only show on the selected tile. The extra room past mere
            // clearance is free: the page had ~170 px of nothing between the rail
            // and the footer.
            let mut ty = 430.0;
            for line in &lines {
                self.kit_text_mid(cap_x, ty, theme::size::TITLE + 6.0, line, theme::col::TEXT);
                ty += 40.0;
            }
            self.kit_game_facts_px(cap_x, ty + 2.0, e, theme::size::LABEL + 1.0);
        }

        // Position rail — the row shows about five games out of many, so where you
        // are in the library needs saying somewhere.
        if n > 5 {
            // Thin, and close under the text rather than stranded at the bottom of
            // the screen: it says where you are in the row, so it belongs with the
            // row's caption.
            // Follows the caption down. A two-line title ends its facts line at
            // y=506, so the old 503 would have been drawn THROUGH it.
            self.kit_rrect(cap_x, 549.0, cap_w, 4.0, 2.0, theme::col::LINE);
            let tw = (cap_w * (1280.0 / PITCH) / n as f32).max(56.0);
            let tx = cap_x + (cap_w - tw) * (sel_pos / (n - 1) as f32).clamp(0.0, 1.0);
            self.kit_rrect(tx, 548.0, tw, 6.0, 3.0, theme::col::GOLD);
        }

        self.kit_home_footer();
    }

    pub fn draw_library_gallery(
        &mut self,
        selection: usize,
        scroll_offset: usize,
        entries: &[crate::library::Entry],
        banner_tex: GLuint,
        banner_w: u32,
        banner_h: u32,
        phase_ticks: u64,
        filter: Option<&str>,
        total_unfiltered: usize,
    ) {
        self.library_clear();
        let vw = self.dimensions.width as f32;

        // Banner — compact, fully below the navbar strip (y 4..38). Scaled to a
        // small target height so it doesn't dominate the screen (was full 720x144).
        self.draw_home_header(banner_tex, banner_w, banner_h, entries.len(), filter, total_unfiltered);

        if entries.is_empty() {
            // GRILLE is the validated screen of the four; the guard is placed
            // after the header and before any tile work, so a non-empty library
            // reaches exactly the code it did before.
            self.draw_home_empty(126.0, 630.0);
            self.kit_home_footer();
            return;
        }

        // ── Cover gallery (v1.2.0) ───────────────────────────────────────
        // Fixed 5-per-row GRID. Every tile is the same size and covers are
        // CROP-TO-FILL (object-fit: cover, via draw_textured_rect_cover), so
        // the grid stays perfectly aligned whatever each cover's native aspect —
        // we accept cropping the overflow (the deliberate "5 per row, tant pis"
        // choice). `scroll_offset` is the first visible ROW.
        const COLS: usize = 5;
        const ROW_IMG_H: f32 = 132.0; // uniform tile height
        const GAP_X: f32 = 16.0;
        const GAP_Y: f32 = 22.0;
        const LEFT: f32 = 40.0;
        // 126, not 150: the shrunk banner ends at y102 and left a 46 px gap under
        // itself, which read as the header having lost its second line rather than
        // as clearance. 24 px is the same air the rest of the page uses. No row
        // count changes at any of the four layouts, so no scroll maths moves.
        const TOP: f32 = 126.0;
        let rows_visible = crate::library::GALLERY_ROWS_VISIBLE;
        let avail_w = vw - LEFT * 2.0;
        let cell_w = ((avail_w - (COLS as f32 - 1.0) * GAP_X) / COLS as f32).max(10.0);
        let pitch = ROW_IMG_H + GAP_Y;

        // Regular grid: tile i sits at (col = i % COLS, row = i / COLS).
        // `tiles` = (x, w, row); `cells` feeds input-side 2D navigation
        // (which reads row + center-x, so a fixed grid works unchanged).
        //
        // Geometry ONLY — no cover resolution here. This pass runs over the whole
        // library, and resolving a cover means an SD read + PNG/JPEG decode +
        // texture upload (~25 ms each): doing it for every entry made the first
        // frame cost ~1.9 s on a 71-game library, which is the black screen at
        // launch. Covers are resolved in the draw pass below, visible tiles only.
        let total = entries.len();
        let mut tiles: std::vec::Vec<(f32, f32, u32)> = std::vec::Vec::with_capacity(total);
        let mut cells: std::vec::Vec<GalleryCell> = std::vec::Vec::with_capacity(total);
        for (idx, _entry) in entries.iter().enumerate() {
            let col = idx % COLS;
            let row = (idx / COLS) as u32;
            let x = LEFT + col as f32 * (cell_w + GAP_X);
            tiles.push((x, cell_w, row));
            cells.push(GalleryCell {
                row,
                cx: x + cell_w * 0.5,
                x,
                y: TOP + row as f32 * pitch,
                w: cell_w,
                h: ROW_IMG_H,
            });
        }
        let rows_total = if total == 0 { 0 } else { ((total + COLS - 1) / COLS) as u32 };
        // Publish layout for input-side 2D navigation.
        if let Ok(mut g) = gallery_cache().lock() {
            *g = (cells, rows_total);
        }

        // Pass 2 — smooth-scrolled visible window (v1.2.0 polish). The input
        // layer still tracks a discrete first row (`scroll_offset`) + tile
        // index (`selection`); here we ease an actual pixel scroll toward that
        // row and glide a single selection frame toward the active tile, so
        // cursor moves and row changes slide instead of snapping. A scissor
        // clips the band so partially-scrolled rows don't bleed onto the banner
        // or the info line.
        // Clip band sits a touch ABOVE the first row (TOP) so the resting row's
        // top edge + its selection frame (which overhangs ~4px, more on a pop)
        // aren't rogned; the 16px headroom still leaves a gap to the banner so a
        // row scrolling UP fades out cleanly instead of overlapping it.
        // -12 (not -16): a tile scrolling in is still revealed gradually, but the
        // band no longer reaches up into the count / filter line above it.
        let band_top = TOP - 12.0;
        let band_bot = TOP + rows_visible as f32 * pitch;
        let target_scroll = scroll_offset as f32 * pitch;
        // Selected tile geometry in content space — the eased frame chases it.
        let (target_sel_x, target_sel_row, target_sel_w) = tiles
            .get(selection)
            .map(|&(tx, tw, trow)| (tx, trow, tw))
            .unwrap_or((LEFT, 0, 0.0));
        let target_sel_y = TOP + target_sel_row as f32 * pitch;

        // Advance the animation toward the targets (snap on the first frame
        // after a reset; ease otherwise). Falls back to the targets if the lock
        // is somehow unavailable — worst case is one un-eased frame.
        let mut scroll_px = target_scroll;
        let mut frame_x = target_sel_x;
        let mut frame_y = target_sel_y;
        let mut frame_w = target_sel_w;
        let mut pop = 0.0f32;
        let touch_scroll = gallery_touch_scroll_read();
        if let Ok(mut a) = gallery_anim().lock() {
            let now = phase_ticks;
            if !a.inited {
                a.inited = true;
                a.last_tick = now;
                a.last_sel = selection;
                a.sel_x = target_sel_x;
                a.sel_y = target_sel_y;
                a.sel_w = target_sel_w;
                a.scroll_px = target_scroll;
                a.pop = 0.0;
            } else {
                let freq = unsafe { ruffle_tick_freq() } as f32;
                let dt = if freq > 0.0 {
                    (now.saturating_sub(a.last_tick) as f32 / freq).min(0.1)
                } else {
                    1.0 / 60.0
                };
                a.last_tick = now;
                if selection != a.last_sel {
                    a.pop = 1.0; // kick the "snap" pop on every cursor move
                    a.last_sel = selection;
                }
                a.sel_x = ease_to(a.sel_x, target_sel_x, dt, 18.0);
                a.sel_y = ease_to(a.sel_y, target_sel_y, dt, 18.0);
                a.sel_w = ease_to(a.sel_w, target_sel_w, dt, 18.0);
                a.scroll_px = ease_to(a.scroll_px, target_scroll, dt, 16.0);
                a.pop = ease_to(a.pop, 0.0, dt, 12.0);
            }
            // Touch drag overrides the eased scroll with 1:1 finger tracking
            // (cleared to None on release, so the glide then settles onto a row).
            if let Some(px) = touch_scroll {
                a.scroll_px = px;
            }
            scroll_px = a.scroll_px;
            frame_x = a.sel_x;
            frame_y = a.sel_y;
            frame_w = a.sel_w;
            pop = a.pop;
        }

        // Publish the selected tile's current screen rect for the game launch /
        // quit reveal (the cover grows from / shrinks to it).
        if !tiles.is_empty() {
            let sel_y = TOP + target_sel_row as f32 * pitch - scroll_px;
            if let Ok(mut r) = gallery_sel_rect().lock() {
                *r = (target_sel_x, sel_y, target_sel_w, ROW_IMG_H);
            }
        }

        // Publish viewport metrics for the touch layer (drag-scroll + hit-test).
        if let Ok(mut v) = gallery_view().lock() {
            *v = GalleryView {
                scroll_px,
                pitch,
                band_top,
                band_bot,
                rows_total,
                rows_visible: rows_visible as u32,
                horizontal: false,
                off_min: 0.0,
                off_max: 0.0,
            };
        }

        // Clip to the gallery band. `set_clip` owns the top-left/bottom-left
        // conversion: every view that re-derived it had to be hunted down and
        // re-checked whenever the band geometry moved, and it moved often.
        self.set_clip(0.0, band_top, vw, band_bot - band_top);

        // Draw the rows that can intersect the band. `scroll_offset` moves at
        // most one row per input, so a ±1 window around it always covers the
        // partially-scrolled rows; the scissor does the exact clipping.
        let lo_row = scroll_offset.saturating_sub(1) as u32;
        let hi_row = (scroll_offset + rows_visible + 1) as u32;
        // Budget of NEW cover decodes for this frame — see COVER_DECODES_PER_FRAME.
        let mut decode_budget = COVER_DECODES_PER_FRAME;
        // Work AHEAD of the eye. Only the rows on screen used to be decoded, so
        // scrolling down met a fresh row of generated tiles every time and each
        // cover popped in behind the movement. Two rows of margin on each side
        // are decoded first, in the same one-per-frame budget, so an idle moment
        // buys the row you are about to reach.
        //
        // Not a fixed window: the NEAREST undecoded tile in the WHOLE list, one
        // per frame. Stopping at a couple of rows meant a fast scroll outran the
        // decoder and covers kept landing one by one behind the movement. Working
        // outwards instead means a second spent looking at the gallery quietly
        // finishes the rest of the library, and by the time you scroll there is
        // nothing left to decode. Costs nothing at startup — this only runs while
        // the gallery is on screen, and never more than one decode per frame.
        if decode_budget > 0 {
            // From the TOP of what is on screen, downwards, then below the fold,
            // then the rest. Starting from the middle of the band filled the grid
            // outwards from its centre, which reads as tiles popping upwards and
            // matches nothing the player can see.
            let mut best: Option<(u8, u32, usize)> = None;
            for (idx, &(_, _, trow)) in tiles.iter().enumerate() {
                if cover_lookup(&entries[idx].basename).is_some() {
                    continue;
                }
                let band = if trow >= lo_row && trow <= hi_row {
                    0
                } else if trow > hi_row {
                    1
                } else {
                    2
                };
                let key = (band, trow, idx);
                if best.map_or(true, |b| key < b) {
                    best = Some(key);
                }
            }
            if let Some((_, _, idx)) = best {
                decode_budget -= 1;
                let _ = self.cover_for(&entries[idx].basename);
            }
        }
        tile_open_frame();
        // How far a tile grows when it opens: enough to read as lifted off the
        // grid, little enough to stay inside the gap to its neighbours.
        const LIFT: f32 = 0.05;
        let mut selected_open = 0.0f32;
        for (idx, &(tx0, tw0, trow)) in tiles.iter().enumerate() {
            if trow < lo_row || trow > hi_row {
                continue;
            }
            let ty0 = TOP + trow as f32 * pitch - scroll_px;
            // Skip tiles fully outside the band (cheap reject before draw).
            if ty0 + ROW_IMG_H < band_top || ty0 > band_bot {
                continue;
            }
            // Every tile on its own clock: the selected one opens, any other
            // closes from wherever it was, so none snaps shut mid-way.
            let b = tile_open(tile_key(&entries[idx].basename), idx == selection);
            if idx == selection {
                selected_open = b;
            }
            let grow = LIFT * b;
            let (tx, ty, tw, th) = (
                tx0 - tw0 * grow * 0.5,
                ty0 - ROW_IMG_H * grow * 0.5,
                tw0 * (1.0 + grow),
                ROW_IMG_H * (1.0 + grow),
            );
            if b > 0.0 {
                self.kit_shadow(tx, ty + 8.0, tw, th, theme::radius::TILE, 16.0, ((0x90 as f32 * b) as u32) << 24);
            }
            // Cached cover, else decode one if this frame still has budget, else
            // the generated tile for now (it resolves on a following frame).
            let (cover, ready_at) = match cover_ready(&entries[idx].basename) {
                Some(v) => v,
                None if decode_budget > 0 => {
                    decode_budget -= 1;
                    let t = self.cover_for(&entries[idx].basename);
                    (t, unsafe { ruffle_tick_now() })
                }
                None => (CoverTex::Default, 0),
            };
            let fade = cover_fade(ready_at);
            // The generated tile under a cover that is still fading in, alone
            // when there is no cover at all.
            let cover_img = match cover {
                CoverTex::Image { tex, w, h, amb } => Some((tex, w, h, amb)),
                CoverTex::Default => None,
            };
            if cover_img.is_none() || fade < 1.0 {
                self.kit_default_tile(tx, ty, tw, th, entries[idx].color_chip, &entries[idx].display_name);
            }
            if let Some((tex, w, h, amb)) = cover_img {
                // Filled at rest; the selected tile opens to the whole cover
                // over its blurred backdrop.
                self.kit_cover_reveal(tx, ty, tw, th, tex, w, h, amb, b, fade);
            }


            // NO AS3 BADGE. It used to sit here, in amber, on every AS3 tile.
            //
            // It was put there when AVM2 was the riskier engine and the badge
            // was an honest warning. It is not one any more: game after game has
            // been made to run, and whether a given SWF works now has nothing to
            // do with which ActionScript it was written in. Marking a third of
            // the library with a warning colour for a property that no longer
            // predicts anything told players their game was fragile when it was
            // not. The engine is still named in the facts line under the
            // selection, where it belongs: a fact, not a verdict.

            // Favourite marker, top-left: a gold star on a dark chip (the chip
            // gives contrast on bright covers). The old 5x7 font had no star, so
            // this used to be a diamond built from a rotated square. Favourites
            // are also pinned to the top of the gallery (library::sort_entries).
            if crate::favorites::is_favorite(&entries[idx].basename) {
                self.kit_favorite_chip(tx + 8.0, ty + 8.0);
            }
        }

        // Single eased selection frame, drawn last and still inside the scissor
        // so it clips with its tile when partially scrolled. `pop` briefly
        // inflates it right after a move for a little tactile "snap"; `pulse`
        // keeps the existing breathing brightness.
        if !tiles.is_empty() {
            // The ring hugs the lifted tile: the same growth, plus the brief
            // "snap" right after a move.
            let lift = 0.05 * selected_open;
            let grow = pop * 3.0;
            let fx = frame_x - frame_w * lift * 0.5 - grow;
            let fy = frame_y - scroll_px - ROW_IMG_H * lift * 0.5 - grow;
            let fw = frame_w * (1.0 + lift) + 2.0 * grow;
            let fh = ROW_IMG_H * (1.0 + lift) + 2.0 * grow;
            // A gold ring that stands off the tile, concentric with its corners,
            // instead of the breathing frame drawn over the cover's edge.
            self.kit_ring(
                fx,
                fy,
                fw,
                fh,
                theme::radius::TILE,
                theme::ring::GAP,
                theme::ring::WIDTH,
                theme::col::GOLD,
            );
        }

        self.clear_clip();

        // Tracks the eased pixel scroll so the thumb glides with the tiles.
        self.draw_scrollbar(
            vw - 18.0,
            TOP,
            rows_visible as f32 * pitch,
            scroll_px,
            pitch,
            rows_visible,
            rows_total as usize,
        );

        // The selected game: its name in gold, its facts under it, both on the
        // grid's left edge rather than centred, so the eye goes from the tiles
        // straight down to the name. Cut in the MIDDLE when too long, because
        // what tells two "Scooby-Doo: Mayan Monster Mayhem Episode N" apart is
        // at the end.
        if let Some(entry) = entries.get(selection) {
            use theme::{col, size};
            let name = self.kit_fit_mid(&entry.display_name, size::TITLE, vw - 2.0 * LEFT);
            self.kit_text_mid(LEFT, 606.0, size::TITLE, &name, col::GOLD);
            self.kit_game_facts(LEFT, 642.0, entry);
        }

        self.kit_home_footer();

        unsafe {
            glUseProgram(0);
            glBindVertexArray(0);
        }
        self.gl_state.invalidate();
        // Boot cost attribution: report what the FIRST gallery frame spent on
        // covers (this is what used to own the launch black screen).
        {
            static LOGGED: std::sync::atomic::AtomicBool =
                std::sync::atomic::AtomicBool::new(false);
            if !LOGGED.swap(true, Ordering::Relaxed) {
                let freq = unsafe { ruffle_tick_freq() } as f64;
                let ms = (COVER_DECODE_TICKS.load(Ordering::Relaxed) as f64) * 1000.0 / freq;
                let mut m = std::format!(
                    "boot: first gallery frame — {} covers decoded, {:.0} ms\n\0",
                    COVER_DECODE_COUNT.load(Ordering::Relaxed), ms,
                );
                unsafe { ruffle_log_cstr(m.as_mut_ptr() as *const _) };
            }
        }
    }

    /// Progress of the games-folder move (#79): the progress page with a plain
    /// count of entries, and no cancel, because a half-cancelled rename sweep is
    /// worse than a finished one.
    pub fn draw_library_move_progress(&mut self, title: &str, done: usize, total: usize) {
        // Empty at 0 of 0 rather than full: a move that has not started should
        // not look finished.
        let frac = if total == 0 { 0.0 } else { (done as f32 / total as f32).clamp(0.0, 1.0) };
        let dest = crate::library::primary_root();
        self.draw_progress_page(
            Icon::FolderOpen,
            title,
            dest.trim_start_matches("sdmc:"),
            Some(frac),
            &std::format!("{} / {}", done, total),
            &[],
        );
    }

    // ── Phase 3.7: DISTANT mode screens ────────────────────────────────

    /// A download in flight: the progress page, with B to cancel.
    /// `bytes_total = 0` means no Content-Length yet: the bar sweeps instead.
    pub fn draw_library_distant_downloading(&mut self, file_name: &str, bytes_done: u64, bytes_total: u64) {
        let lc = crate::loc::s();
        let u = crate::loc_ui::ui();
        let (frac, detail) = if bytes_total > 0 {
            let f = (bytes_done as f32 / bytes_total as f32).clamp(0.0, 1.0);
            (
                Some(f),
                std::format!(
                    "{} %   \u{00B7}   {} / {}",
                    (f * 100.0) as u32,
                    format_size_pretty(bytes_done),
                    format_size_pretty(bytes_total),
                ),
            )
        } else {
            (None, std::format!("{}\u{2026}", format_size_pretty(bytes_done)))
        };
        self.draw_progress_page(Icon::Download, lc.dl_title, file_name, frac, &detail, &[(&[Btn::B], u.cancel)]);
    }

    /// Error toast for DISTANT mode (URL parse / metadata fetch / DL fail).
    /// `can_fix` = the failure carries the URL that caused it, so the footer
    /// advertises Y (re-type it here) on top of the usual A/B dismiss.
    pub fn draw_library_distant_error(&mut self, msg: &str, can_fix: bool) {
        let lc = crate::loc::s();
        let footer = if can_fix { lc.err_footer_fix } else { lc.err_footer };
        self.draw_centered_notice_footer(lc.err_title, 0xFF5040, msg, footer);
    }

    /// Applet-mode notice (P1c): same centered layout as the error toast, but
    /// an amber "info" title instead of red — games can't launch in applet
    /// mode, this is guidance rather than a failure.
    pub fn draw_library_applet_notice(&mut self, msg: &str) {
        self.draw_centered_notice(crate::loc::s().applet_title, 0xFFB740, msg);
    }

    /// Shared full-screen centered notice: big title (in `title_rgb`), a
    /// word-wrapped body, and the generic dismiss footer.
    fn draw_centered_notice(&mut self, title: &str, title_rgb: u32, msg: &str) {
        self.draw_centered_notice_footer(title, title_rgb, msg, crate::loc::s().err_footer);
    }

    /// `draw_centered_notice` with an explicit footer, for notices whose
    /// dismiss options depend on what failed.
    fn draw_centered_notice_footer(
        &mut self,
        title: &str,
        title_rgb: u32,
        msg: &str,
        footer: &str,
    ) {
        use theme::{col, size};
        self.library_clear();
        let vw = self.dimensions.width as f32;
        let vh = self.dimensions.height as f32;
        // A card in the middle of the page: an icon that says what kind of
        // news this is before a word is read, the title in the colour the
        // caller gave it (red for an error, amber for a notice, green for a
        // success), the message under it.
        let rgb = title_rgb & 0x00FF_FFFF;
        let (icon, ink) = match rgb {
            0x66DD66 => (Icon::CircleCheck, col::OK),
            0xFF5040 => (Icon::Alert, col::DANGER),
            _ => (Icon::Info, 0xFF00_0000 | rgb),
        };
        const DISC: f32 = 56.0;
        let w = 720.0f32.min(vw - 80.0);
        let text_x = 40.0 + DISC + 20.0;
        let lines = self.kit_wrap(msg, size::BODY, w - text_x - 40.0, 8);
        let lh = (size::BODY * 1.45).round();
        let h = (110.0 + lh * lines.len() as f32).max(40.0 + DISC + 40.0);
        let x = ((vw - w) * 0.5).round();
        let y = ((vh - 48.0 - h) * 0.5).round().max(90.0);
        self.kit_panel(x, y, w, h, false);
        self.kit_rrect(x + 40.0, y + 36.0, DISC, DISC, DISC * 0.5, col::CHIP);
        self.kit_icon(x + 40.0 + 14.0, y + 36.0 + DISC * 0.5, 28.0, icon, ink);
        let t = self.kit_fit(title, size::TITLE, w - text_x - 40.0);
        self.kit_text_mid(x + text_x, y + 52.0, size::TITLE, &t, ink);
        let mut ly = y + 96.0;
        for line in &lines {
            self.kit_text_mid(x + text_x, ly, size::BODY, line, col::TEXT2);
            ly += lh;
        }
        self.draw_page_footer(footer);
        unsafe {
            glUseProgram(0);
            glBindVertexArray(0);
        }
        self.gl_state.invalidate();
    }

    /// Bug-report result notice. Green title on success, red on failure; the
    /// body is the (already-localized) message. Reuses the shared centered
    /// notice layout.
    pub fn draw_library_bug_result(&mut self, msg: &str, ok: bool) {
        let lc = crate::loc::s();
        if ok {
            self.draw_centered_notice(lc.bug_ok_title, 0x66DD66, msg);
        } else {
            self.draw_centered_notice(lc.bug_fail_title, 0xFF5040, msg);
        }
    }

    /// Transient toast banner near the bottom of the screen (#20). `kind`: 0 =
    /// success (green), 1 = error (red), 2 = info (blue). Non-blocking — drawn on
    /// top of the current screen for a couple of seconds (the library loop counts
    /// it down), so a share/apply/revert confirms without a full "thanks" modal.
    pub fn draw_toast(&mut self, msg: &str, kind: u8) {
        use theme::{col, size};
        unsafe {
            glEnable(GL_BLEND);
            glBlendFunc(GL_SRC_ALPHA, GL_ONE_MINUS_SRC_ALPHA);
            glDisable(GL_STENCIL_TEST);
        }
        let vw = self.dimensions.width as f32;
        let vh = self.dimensions.height as f32;
        // A pill above the footer band, never over the hints. The dot says what
        // kind of news it is; the words say what happened.
        let dot = match kind {
            1 => col::DANGER,
            2 => col::GOLD,
            _ => col::OK,
        };
        const H: f32 = 52.0;
        const PAD: f32 = 24.0;
        const DOT: f32 = 10.0;
        let max_text = vw - 80.0 - 2.0 * PAD - DOT - 14.0;
        let text = self.kit_fit(msg, size::BODY, max_text);
        let tw = self.kit_measure(&text, size::BODY);
        let w = tw + 2.0 * PAD + DOT + 14.0;
        let x = ((vw - w) * 0.5).round();
        let y = vh - 48.0 - 20.0 - H;
        self.kit_shadow(x, y + 6.0, w, H, H * 0.5, 18.0, col::SHADOW);
        self.kit_shape(x, y, w, H, H * 0.5, col::PANEL, 1.0, col::PANEL_LINE, 0.0);
        self.kit_rrect(x + PAD, y + (H - DOT) * 0.5, DOT, DOT, DOT * 0.5, dot);
        self.kit_text_mid(x + PAD + DOT + 14.0, y + H * 0.5, size::BODY, &text, col::TEXT);
        unsafe {
            glUseProgram(0);
            glBindVertexArray(0);
        }
        self.gl_state.invalidate();
    }

    /// Language picker (Settings → LANGUAGE). `languages` are native display
    /// names in `loc::PICKER_LANGS` order. The currently-active language is
    /// tinted teal even when the cursor is elsewhere.
    /// Solid opaque rect at screen px, for the pixel flags. `rgb` is 0xRRGGBB.
    fn flag_fill(&mut self, x: f32, y: f32, w: f32, h: f32, rgb: u32) {
        self.draw_overlay_rect(x, y, w, h, 0xFF00_0000 | (rgb & 0x00FF_FFFF));
    }

    /// Filled circle centred on (cx, cy), radius r, approximated by stacked 1px
    /// horizontal spans. Used for the round parts of flags (crescent, disc, star).
    fn flag_disc(&mut self, cx: f32, cy: f32, r: f32, rgb: u32) {
        let ri = r.max(1.0) as i32;
        for dy in -ri..=ri {
            let hw = (r * r - (dy * dy) as f32).max(0.0).sqrt();
            if hw > 0.0 {
                self.flag_fill(cx - hw, cy + dy as f32, hw * 2.0, 1.0, rgb);
            }
        }
    }

    /// 2px frame around (x,y,w,h), used to outline a flag tile.
    fn flag_stroke(&mut self, x: f32, y: f32, w: f32, h: f32, rgb: u32) {
        const T: f32 = 2.0;
        self.flag_fill(x, y, w, T, rgb);
        self.flag_fill(x, y + h - T, w, T, rgb);
        self.flag_fill(x, y, T, h, rgb);
        self.flag_fill(x + w - T, y, T, h, rgb);
    }

    /// A small pixel flag for `lang` filling the w×h box at (x,y). Solid rects
    /// (+ `flag_disc` for the round flags). Deliberately simplified: the native
    /// name is drawn under it, so the flag only has to be recognisable, not exact.
    fn draw_flag(&mut self, lang: crate::loc::Lang, x: f32, y: f32, w: f32, h: f32) {
        use crate::loc::Lang;
        let third_v = w / 3.0;
        let third_h = h / 3.0;
        match lang {
            Lang::Fr => {
                self.flag_fill(x, y, third_v, h, 0x0055A4);
                self.flag_fill(x + third_v, y, third_v, h, 0xFFFFFF);
                self.flag_fill(x + 2.0 * third_v, y, w - 2.0 * third_v, h, 0xEF4135);
            }
            Lang::It => {
                self.flag_fill(x, y, third_v, h, 0x009246);
                self.flag_fill(x + third_v, y, third_v, h, 0xFFFFFF);
                self.flag_fill(x + 2.0 * third_v, y, w - 2.0 * third_v, h, 0xCE2B37);
            }
            Lang::De => {
                self.flag_fill(x, y, w, third_h, 0x000000);
                self.flag_fill(x, y + third_h, w, third_h, 0xDD0000);
                self.flag_fill(x, y + 2.0 * third_h, w, h - 2.0 * third_h, 0xFFCE00);
            }
            Lang::Ru => {
                self.flag_fill(x, y, w, third_h, 0xFFFFFF);
                self.flag_fill(x, y + third_h, w, third_h, 0x0039A6);
                self.flag_fill(x, y + 2.0 * third_h, w, h - 2.0 * third_h, 0xD52B1E);
            }
            Lang::Es => {
                self.flag_fill(x, y, w, h, 0xAA151B);
                self.flag_fill(x, y + h * 0.25, w, h * 0.5, 0xF1BF00);
            }
            Lang::En => {
                // Simplified Union Jack: blue field with a white-bordered red cross
                // (the diagonals are dropped — unreadable at this size).
                self.flag_fill(x, y, w, h, 0x012169);
                self.flag_fill(x, y + h * 0.35, w, h * 0.30, 0xFFFFFF);
                self.flag_fill(x + w * 0.35, y, w * 0.30, h, 0xFFFFFF);
                self.flag_fill(x, y + h * 0.42, w, h * 0.16, 0xC8102E);
                self.flag_fill(x + w * 0.42, y, w * 0.16, h, 0xC8102E);
            }
            Lang::Zh => {
                self.flag_fill(x, y, w, h, 0xDE2910);
                // One big yellow star (disc) + four small dots, upper-left quadrant.
                self.flag_disc(x + w * 0.24, y + h * 0.34, h * 0.15, 0xFFDE00);
                let d = (h * 0.05).max(2.0);
                for (fx, fy) in [(0.42, 0.16), (0.50, 0.30), (0.50, 0.50), (0.42, 0.64)] {
                    self.flag_fill(x + w * fx, y + h * fy, d, d, 0xFFDE00);
                }
            }
            Lang::Tr => {
                self.flag_fill(x, y, w, h, 0xE30A17);
                // Crescent = white disc minus a red disc offset to the right.
                self.flag_disc(x + w * 0.40, y + h * 0.50, h * 0.27, 0xFFFFFF);
                self.flag_disc(x + w * 0.47, y + h * 0.50, h * 0.21, 0xE30A17);
                // Star (approximated by a small disc).
                self.flag_disc(x + w * 0.60, y + h * 0.50, h * 0.09, 0xFFFFFF);
            }
            Lang::Pt => {
                // Brazil: green field, yellow diamond, blue disc.
                self.flag_fill(x, y, w, h, 0x009C3B);
                let cx = x + w * 0.5;
                let cy = y + h * 0.5;
                let hh = h * 0.42;
                let hw = w * 0.42;
                let steps = hh as i32;
                for dy in -steps..=steps {
                    let frac = 1.0 - (dy.abs() as f32) / hh;
                    let rw = hw * frac;
                    if rw > 0.0 {
                        self.flag_fill(cx - rw, cy + dy as f32, rw * 2.0, 1.0, 0xFFDF00);
                    }
                }
                self.flag_disc(cx, cy, h * 0.16, 0x002776);
            }
        }
    }

    /// Shared loading overlay: a full-screen dim + the standard loading panel
    /// (title + spinner). Drawn OUTSIDE any modal pop-in scale
    /// (`clear_ui_transform`) so it stays full size instead of shrinking with a
    /// modal's open animation. Used by the profile network flows AND the language
    /// picker's first open, so they all look identical. `now` drives the rotation.
    pub fn draw_loading_overlay(&mut self, title: &str, now: u64) {
        self.clear_ui_transform();
        let vw = self.dimensions.width as f32;
        let vh = self.dimensions.height as f32;
        self.draw_overlay_rect(0.0, 0.0, vw, vh, 0xC8_14_20_38);
        self.draw_loading_panel(title, now);
    }

    /// A bordered, centered "Optimisation" modal (shared modal chrome) composited
    /// OVER the live gallery during the one-time first-boot housekeeping — same
    /// look as the other modals (light dim + visible border), not a full-screen
    /// overlay. The caller draws the gallery first; this sits on top with a
    /// spinner under the title.
    pub fn draw_optimizing_modal(&mut self, title: &str, now: u64) {
        self.clear_ui_transform();
        let frame = self.draw_modal_frame(MODAL_W, 0, Some(200.0), false, title, None, None);
        self.draw_spinner(frame.x + frame.w * 0.5, frame.y + 130.0, 26.0, now);
    }

    pub fn draw_library_dim_backdrop(&mut self) {
        unsafe {
            glDisable(GL_STENCIL_TEST);
            glClearColor(0.047, 0.075, 0.137, 1.0);
            glClear(GL_COLOR_BUFFER_BIT | GL_STENCIL_BUFFER_BIT);
            glEnable(GL_BLEND);
            glBlendFunc(GL_SRC_ALPHA, GL_ONE_MINUS_SRC_ALPHA);
        }
        self.gl_state.invalidate();
    }

    // ── Mask state machine ──
    //
    // Flash mask sequence for one mask:
    //   1. push_mask     → begin drawing the mask shape
    //   2. (draw mask shape commands)
    //   3. activate_mask → mask done, begin drawing the maskee
    //   4. (draw maskee shape commands)
    //   5. deactivate_mask → maskee done
    //   6. pop_mask      → undo the stencil ref
    //
    // Scheme: INCR/DECR coverage counting. The frame starts with stencil
    // cleared to 0 (submit_frame). A maskee at nesting depth N is drawn where
    // the stencil count equals N — i.e. it was covered by all N enclosing mask
    // shapes (their intersection). Sequential masks each INCR from 0 then DECR
    // back, so no per-push full-buffer clear is needed. This replaced an
    // earlier bit-OR + REPLACE scheme whose written value didn't match the
    // EQUAL gate, leaving every maskee rejected (SMWF overworld was blank).
    //
    // Each pixel is counted ONCE per mask: the write only increments where
    // the stencil still equals depth-1 (and the clear only decrements where
    // it equals depth), like upstream Ruffle's wgpu backend. With a plain
    // ALWAYS+INCR, a mask shape whose fills or strokes overlap (one draw per
    // fill style) pushed the overlap to depth+1 and the EQUAL gate rejected
    // it: New Super Smash Flash's character-select portraits are masked by
    // their frame shape (black square + inset gradient square + outline), so
    // only the one-pixel ring outside the gradient survived and the boxes
    // looked empty (#116).
    fn mask_push(&mut self) {
        self.push_mask_window = self.push_mask_window.saturating_add(1);
        self.mask.writing = true;
        self.mask.depth = self.mask.depth.saturating_add(1);
        unsafe {
            glEnable(GL_STENCIL_TEST);
            // Mask shape writes stencil only (no color): increment coverage.
            glColorMask(GL_FALSE, GL_FALSE, GL_FALSE, GL_FALSE);
            glStencilMask(0xFF);
            glStencilFunc(GL_EQUAL, self.mask.depth as GLint - 1, 0xFF);
            glStencilOp(GL_KEEP, GL_KEEP, GL_INCR);
        }
    }

    fn mask_activate(&mut self) {
        // Mask shape done. Draw the maskee where coverage == nesting depth.
        self.mask.writing = false;
        unsafe {
            glColorMask(GL_TRUE, GL_TRUE, GL_TRUE, GL_TRUE);
            glStencilMask(0);
            let func = if DISABLE_MASK_GATING { GL_ALWAYS } else { GL_EQUAL };
            glStencilFunc(func, self.mask.depth as GLint, 0xFF);
            glStencilOp(GL_KEEP, GL_KEEP, GL_KEEP);
        }
    }

    fn mask_deactivate(&mut self) {
        // Maskee done. Redraw the mask shape decrementing coverage back, so
        // sibling/outer masks see a clean stencil without a full clear.
        self.mask.writing = true;
        unsafe {
            glColorMask(GL_FALSE, GL_FALSE, GL_FALSE, GL_FALSE);
            glStencilMask(0xFF);
            glStencilFunc(GL_EQUAL, self.mask.depth as GLint, 0xFF);
            glStencilOp(GL_KEEP, GL_KEEP, GL_DECR);
        }
    }

    /// Re-apply the GL stencil state that matches `self.mask`.
    ///
    /// An offscreen pass (`render_commands_to_texture`) disables the stencil
    /// test for its own target and restores `self.mask` afterwards — but the GL
    /// side was NOT restored, and `glEnable(GL_STENCIL_TEST)` only ever happens
    /// in `mask_push`. So after any blend/filter pass taken while a mask was
    /// active, the stencil stayed OFF until the next push and the maskee drew
    /// unclipped. Calling this on the way out closes that hole.
    fn mask_restore_gl(&self) {
        unsafe {
            if self.mask.depth == 0 {
                glDisable(GL_STENCIL_TEST);
                glColorMask(GL_TRUE, GL_TRUE, GL_TRUE, GL_TRUE);
                return;
            }
            glEnable(GL_STENCIL_TEST);
            if self.mask.writing {
                // Mid mask-shape: writing coverage, no colour. (INCR matches
                // `mask_push`; a pass taken mid-DECR is not distinguishable and
                // is not a case the command stream produces.)
                glColorMask(GL_FALSE, GL_FALSE, GL_FALSE, GL_FALSE);
                glStencilMask(0xFF);
                glStencilFunc(GL_EQUAL, self.mask.depth as GLint - 1, 0xFF);
                glStencilOp(GL_KEEP, GL_KEEP, GL_INCR);
            } else {
                // Drawing a maskee: gate at the current nesting depth.
                glColorMask(GL_TRUE, GL_TRUE, GL_TRUE, GL_TRUE);
                glStencilMask(0);
                let func = if DISABLE_MASK_GATING { GL_ALWAYS } else { GL_EQUAL };
                glStencilFunc(func, self.mask.depth as GLint, 0xFF);
                glStencilOp(GL_KEEP, GL_KEEP, GL_KEEP);
            }
        }
    }

    fn mask_pop(&mut self) {
        self.mask.writing = false;
        if self.mask.depth > 0 {
            self.mask.depth -= 1;
        }
        unsafe {
            glColorMask(GL_TRUE, GL_TRUE, GL_TRUE, GL_TRUE);
            if self.mask.depth == 0 {
                glDisable(GL_STENCIL_TEST);
            } else {
                // Resume gating the enclosing maskee at the outer depth.
                glStencilMask(0);
                let func = if DISABLE_MASK_GATING { GL_ALWAYS } else { GL_EQUAL };
                glStencilFunc(func, self.mask.depth as GLint, 0xFF);
                glStencilOp(GL_KEEP, GL_KEEP, GL_KEEP);
            }
        }
    }
}


/// Format a byte count as a short pretty string ("3 KB", "15 MB"). Picks
/// the largest unit that keeps the integer part ≤ 999. KiB-style (1024)
/// instead of decimal because that's what hbmenu / fsadm show for files.
fn format_size_pretty(bytes: u64) -> std::string::String {
    // Unknown size (e.g. Flashpoint search hits — db-api doesn't expose the
    // GameZIP size) → show nothing rather than a misleading "0 B".
    if bytes == 0 {
        return std::string::String::new();
    }
    const KB: u64 = 1024;
    const MB: u64 = 1024 * 1024;
    const GB: u64 = 1024 * 1024 * 1024;
    if bytes >= GB {
        std::format!("{}.{} GB", bytes / GB, (bytes % GB) / (GB / 10))
    } else if bytes >= MB {
        std::format!("{} MB", bytes / MB)
    } else if bytes >= KB {
        std::format!("{} KB", bytes / KB)
    } else {
        std::format!("{} B", bytes)
    }
}

/// Cheap sin approximation for UI animations. Bhaskara-I-style polynomial
/// — accurate to ~3 decimal places, no libm dependency, branch-free except
/// for the period fold. Plenty for visual pulses (we only use it to
/// modulate amber → bright-amber and a 4-pixel cursor offset).
fn approx_sin(x: f32) -> f32 {
    // Fold to [-π, π].
    let two_pi = 2.0 * core::f32::consts::PI;
    let mut t = x % two_pi;
    if t > core::f32::consts::PI { t -= two_pi; }
    if t < -core::f32::consts::PI { t += two_pi; }
    // Bhaskara I: sin(x) ≈ 16x(π − x) / (5π² − 4x(π − x)) for x ∈ [0, π].
    // Use sign symmetry for negative x.
    let sign = if t < 0.0 { -1.0 } else { 1.0 };
    let t = t.abs();
    let pi = core::f32::consts::PI;
    let num = 16.0 * t * (pi - t);
    let den = 5.0 * pi * pi - 4.0 * t * (pi - t);
    sign * (num / den)
}

impl RenderBackend for SwitchRenderBackend {
    fn viewport_dimensions(&self) -> ViewportDimensions {
        self.dimensions
    }

    fn set_viewport_dimensions(&mut self, dimensions: ViewportDimensions) {
        // `dimensions` is what RUFFLE composes for, and it is portrait while the
        // picture is turned. The framebuffer never turns, so glViewport gets the
        // physical rectangle and the matrix hook maps one onto the other.
        self.dimensions = dimensions;
        let (pw, ph) = if rotation_swaps_axes() {
            (dimensions.height, dimensions.width)
        } else {
            (dimensions.width, dimensions.height)
        };
        unsafe {
            glViewport(0, 0, pw as GLsizei, ph as GLsizei);
        }
    }

    fn register_shape(
        &mut self,
        shape: DistilledShape,
        bitmap_source: &dyn BitmapSource,
    ) -> ShapeHandle {
        let mesh = self.tessellator.tessellate_shape(shape, bitmap_source);

        // Bake gradient textures first so per-draw can reference them.
        let mut gradient_textures: Vec<GLuint> = Vec::with_capacity(mesh.gradients.len());
        for g in &mesh.gradients {
            gradient_textures.push(build_gradient_texture(g));
        }

        // Baseline: budget=0 → bitmap fills render as solid white (the
        // "blocs blancs" state of commit 6a2b858, README "phase 1.5"). With
        // ANY budget > 0 Mario 63 deterministically crashes at host frame
        // ~40 during render_shape's DrawKind::Bitmap path inside
        // submit_frame — bitmap registration is fine (650+ regs at flat
        // RAM), the bug is in the GL draw side. Restore =0 while we
        // instrument that exact path.
        // Crash fixed (2026-05-24): jpeg_decoder's std::thread::spawn for
        // JPEGs > 128*128 used to crash Switch newlib pthread. Forked the
        // crate to always use Immediate worker → no spawn → no crash.
        // We can now resolve every bitmap fill (full sprites for Mario 63).
        const PER_SHAPE_BITMAP_BUDGET: usize = usize::MAX;
        let mut bitmap_metas: Vec<Option<SwitchBitmapHandle>> =
            Vec::with_capacity(mesh.draws.len());
        // Parallel to `bitmap_metas`: the standalone texture for >2048 fills
        // that don't fit the atlas. Exactly one of the two is Some per bitmap
        // fill; both None means the fill renders solid (degenerate).
        let mut bitmap_standalones: Vec<Option<Arc<StandaloneTexture>>> =
            Vec::with_capacity(mesh.draws.len());
        // Parallel too: an evictable SWF image (see `ReloadSlot`).
        let mut bitmap_slots: Vec<Option<std::rc::Rc<ReloadSlot>>> =
            Vec::with_capacity(mesh.draws.len());
        let bitmap_fill_count = mesh
            .draws
            .iter()
            .filter(|d| matches!(d.draw_type, DrawType::Bitmap(_)))
            .count();
        let resolve_bitmaps = bitmap_fill_count <= PER_SHAPE_BITMAP_BUDGET;
        for draw in &mesh.draws {
            let mut slot = None;
            let (meta, standalone) = if resolve_bitmaps {
                if let DrawType::Bitmap(b) = &draw.draw_type {
                    match bitmap_source.bitmap_handle(b.bitmap_id, self) {
                        // Atlas-packed (common) vs standalone (>2048): pick
                        // whichever variant this handle is.
                        Some(h) => {
                            if let Some(sw) = as_switch_bitmap(&h) {
                                (Some(sw.clone()), None)
                            } else if let Some(st) = as_standalone_bitmap(&h) {
                                (None, Some(st.0.clone()))
                            } else if let Some(r) = as_reloadable_bitmap(&h) {
                                slot = Some(r.0.clone());
                                (None, None)
                            } else {
                                (None, None)
                            }
                        }
                        None => (None, None),
                    }
                } else {
                    (None, None)
                }
            } else {
                (None, None)
            };
            bitmap_metas.push(meta);
            bitmap_standalones.push(standalone);
            bitmap_slots.push(slot);
        }

        let mut draws: Vec<GpuDraw> = Vec::with_capacity(mesh.draws.len());
        for (idx, draw) in mesh.draws.iter().enumerate() {
            let meta_ref = bitmap_metas[idx].as_ref();
            let standalone = bitmap_standalones[idx].clone();
            let slot = bitmap_slots[idx].clone();
            if let Some(mut gpu) = upload_draw(
                draw,
                &gradient_textures,
                meta_ref,
                standalone,
                slot,
                &mut self.vertex_arena,
                &mut self.index_arena,
            ) {
                // Refine gradient parameters now that we have the Gradient.
                if let DrawKind::Gradient {
                    texture_index,
                    gradient_kind,
                    spread,
                    focal,
                    ..
                } = &mut gpu.kind
                {
                    let g = &mesh.gradients[*texture_index];
                    *gradient_kind = match g.gradient_type {
                        GradientType::Linear => 0,
                        GradientType::Radial => 1,
                        GradientType::Focal => 2,
                    };
                    *spread = match g.repeat_mode {
                        GradientSpread::Pad => 0,
                        GradientSpread::Reflect => 1,
                        GradientSpread::Repeat => 2,
                    };
                    *focal = f32::from(g.focal_point);
                }
                LIVE_GPU_DRAWS.fetch_add(1, Ordering::Relaxed);
                draws.push(gpu);
            }
        }

        self.shapes_registered = self.shapes_registered.wrapping_add(1);
        LIVE_GPU_SHAPES.fetch_add(1, Ordering::Relaxed);

        // Periodic visibility into shape pressure. With Mario 63's rocket
        // nozzle particle system pumping ~3 shapes/frame, this lets us see
        // whether Ruffle is dropping old handles (live stays bounded) or
        // not (live grows linearly with `shapes_registered`).
        if self.shapes_registered % 500 == 0 {
            let live_s = LIVE_GPU_SHAPES.load(Ordering::Relaxed);
            let live_d = LIVE_GPU_DRAWS.load(Ordering::Relaxed);
            let msg = std::format!(
                "register_shape: total={} live_shapes={} live_draws={}\n",
                self.shapes_registered, live_s, live_d,
            );
            let mut bytes = msg.into_bytes();
            bytes.push(0);
            unsafe { ruffle_log_cstr(bytes.as_ptr() as *const _) };
        }

        ShapeHandle(Arc::new(SwitchShapeHandle(Arc::new(GpuShape {
            draws,
            gradient_textures,
        }))))
    }

    fn render_offscreen(
        &mut self,
        handle: BitmapHandle,
        commands: CommandList,
        _quality: StageQuality,
        bounds: PixelRegion,
    ) -> Option<Box<dyn SyncHandle>> {
        let _pt = PrimTimer::new(&PRIM_OFFSCREEN_CUR);
        // A held write must land before anything samples its texture, and this
        // draws into (and from) textures.
        self.flush_pending_upload();
        self.render_offscreen_calls = self.render_offscreen_calls.wrapping_add(1);
        // Where the BitmapData's pixels live + its dimensions. BitmapData backs
        // its handle via `register_bitmap` (atlas) in the common
        // `new BitmapData()` case; large ones fall back to a standalone texture.
        #[derive(Clone, Copy)]
        enum Backing {
            Standalone(GLuint),
            Atlas { tex: GLuint, base_x: u32, base_y: u32, atlas_w: u32, atlas_h: u32 },
            /// A budget-dropped surface: no GPU texture to seed from or write back
            /// to. We still composite the draw() into a temp and return it, so the
            /// draw SUCCEEDS.
            Dropped,
        }
        let (tex_w, tex_h, backing) = if let Some(s) = as_standalone_bitmap(&handle) {
            // A freed standalone texture id is 0 — never FBO-attach it (see below).
            if s.0.texture == 0 {
                return None;
            }
            (s.0.width, s.0.height, Backing::Standalone(s.0.texture))
        } else if let Some(d) = as_dropped_bitmap(&handle) {
            // Budget-dropped big surface. BitmapData.draw() MUST still succeed:
            // returning None makes Ruffle log "does not support BitmapData.draw"
            // and the game re-issues the draw every frame, allocating a fresh
            // ~1.2 MB Vec each time → OOM on the death/power-up effect spike (the
            // recurring Super Bowser World crash). Compositing into a temp with no
            // backing (seed skipped, cleared transparent) lets the draw land in
            // Ruffle's CPU pixels via the returned sync handle — getPixel/copyPixels
            // keep working; only the on-screen display of this surface stays blank.
            (d.width, d.height, Backing::Dropped)
        } else if let Some(b) = as_switch_bitmap(&handle) {
            let Some(a) = self.atlases.get(b.atlas_index) else { return None };
            // The atlas may have been FREED (texture == 0) while the strip churn
            // recycles slots (Super Bowser World cycles ~7600 ground-strip atlases).
            // FBO-attaching texture 0 leaves the framebuffer incomplete; Mesa then
            // dereferences the null renderbuffer surface → native DataAbort (FAR=0xe,
            // in st_update_renderbuffer_surface). `resolve_bitmap_tex`/`render_bitmap`
            // already skip a dead atlas — render_offscreen must too. Bail: the draw()
            // just no-ops (the surface is gone) instead of crashing the app.
            if a.texture == 0 {
                return None;
            }
            // Base pixel offset + normalization must use the atlas' ACTUAL dims:
            // right-sized dedicated atlases aren't 2048² (#42 regression — hardcoding
            // ATLAS_SIZE here sampled/wrote the wrong region → striped water).
            let base_x = (b.u0 * a.width as f32).round() as u32;
            let base_y = (b.v0 * a.height as f32).round() as u32;
            (
                b.width, b.height,
                Backing::Atlas { tex: a.texture, base_x, base_y, atlas_w: a.width, atlas_h: a.height },
            )
        } else {
            self.warn_once(b"render_offscreen: unknown handle\n\0");
            return None;
        };
        if tex_w == 0 || tex_h == 0 {
            return None;
        }
        // render_offscreen must COMPOSITE the draw() commands onto the
        // BitmapData's existing content (Ruffle's wgpu backend uses
        // `RenderTargetMode::FreshWithTexture`), not replace it. We render into a
        // pooled temp (atlas slots can't be FBO targets) in three steps:
        //   1. SEED temp with the BitmapData's current pixels (premultiplied).
        //   2. COMPOSITE the new draw() commands on top (no colour clear).
        //   3. WRITE the result back into the BitmapData's storage.
        // Without the seed, a software-blitter game that accumulates many
        // draw()s into one BitmapData per frame (catmario's `stageBitmapdata`:
        // ~48 tile draws/frame) keeps only the last draw → invisible world.
        // `temp` is also returned as the SyncHandle, so copyPixels/getPixel
        // (SMWF's tile-engine readback) still resolve from the full result.
        PRIM_OFF_N_CUR.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        // FlashNX (2026-10-06): steps 1 and 3 copy only the region the draw can
        // change, not the whole BitmapData. That region is `bounds`: Ruffle's
        // `operations::draw` passes the source's bounds without filters
        // (`BoundsMode::Engine`, and a draw renders no filter: no cacheAsBitmap
        // in it) times the matrix, rounded outwards, clamped to the bitmap,
        // plus any region still waiting to be read back; it is also all that
        // Ruffle reads back. Two pixels more for antialiasing. catmario blits
        // ~90 sprites a frame into a full-screen BitmapData: each draw copied
        // the 480x420 bitmap twice (6 full-screen passes per sprite with the
        // render, ~22 Mpx a frame), and once the CPU stopped waiting on vertex
        // uploads the GPU was the wall (17 ms of a 33 ms frame waiting on it at
        // the swap). `offbounds.off` marker: whole bitmap again, to compare.
        let (rx, ry, rw, rh) = if OFFSCREEN_BOUNDS.load(std::sync::atomic::Ordering::Relaxed) {
            const MARGIN: u32 = 2;
            let x0 = bounds.x_min.saturating_sub(MARGIN).min(tex_w);
            let y0 = bounds.y_min.saturating_sub(MARGIN).min(tex_h);
            let x1 = bounds.x_max.saturating_add(MARGIN).min(tex_w).max(x0);
            let y1 = bounds.y_max.saturating_add(MARGIN).min(tex_h).max(y0);
            (x0, y0, x1 - x0, y1 - y0)
        } else {
            (0, 0, tex_w, tex_h)
        };
        PRIM_OFF_PIX_CUR.fetch_add(
            (rw as u64) * (rh as u64),
            std::sync::atomic::Ordering::Relaxed,
        );
        let temp = {
            let _t = PrimTimer::new(&PRIM_OFF_ALLOC_CUR);
            self.acquire_offscreen_temp(tex_w, tex_h)?
        };
        let temp_id = temp.texture;
        // FlashNX: may be bigger than (tex_w, tex_h) (`acquire_offscreen_temp`);
        // everything below draws into its top-left corner, and reads it with
        // its own size.
        let (temp_w, temp_h) = (temp.width, temp.height);

        // 1. Seed temp with the BitmapData's current content (premultiplied).
        {
            let _t = PrimTimer::new(&PRIM_OFF_READBACK_CUR);
            match backing {
                _ if rw == 0 || rh == 0 => {}
                Backing::Standalone(s_tex) => {
                    // Standalone already stores premultiplied — straight copy.
                    self.blit_identity(
                        s_tex, tex_w, tex_h, (rx, ry), (rw, rh),
                        temp_id, (rx as i32, ry as i32), rw, rh,
                    );
                }
                Backing::Atlas { tex, base_x, base_y, atlas_w, atlas_h } => {
                    // Atlas stores STRAIGHT alpha — premultiply it into temp.
                    self.blit_premult(
                        tex, atlas_w, atlas_h, (base_x + rx, base_y + ry), (rw, rh),
                        temp_id, (rx as i32, ry as i32), rw, rh,
                    );
                }
                // No backing to seed from — the temp is cleared in step 2 instead.
                Backing::Dropped => {}
            }
        }

        // 2. Composite the new draw() commands on top. Seeded backings composite
        // with no clear; a Dropped surface had no seed, so clear the (pooled, maybe
        // stale) temp to transparent first.
        let clear = match backing {
            Backing::Dropped => Some(Color { r: 0, g: 0, b: 0, a: 0 }),
            _ => None,
        };
        let rendered = {
            let _t = PrimTimer::new(&PRIM_OFF_RENDER_CUR);
            self.render_commands_to_texture(temp_id, tex_w, tex_h, commands, clear)
        };
        if !rendered {
            self.offscreen_temp_retired.push(temp);
            return None;
        }

        // 3. Write the composited result back into the BitmapData's storage.
        {
            let _t = PrimTimer::new(&PRIM_OFF_UPLOAD_CUR);
            match backing {
                _ if rw == 0 || rh == 0 => {}
                Backing::Standalone(s_tex) => {
                    self.blit_identity(
                        temp_id, temp_w, temp_h, (rx, ry), (rw, rh),
                        s_tex, (rx as i32, ry as i32), rw, rh,
                    );
                }
                Backing::Atlas { tex, base_x, base_y, .. } => {
                    self.blit_unpremult(
                        temp_id, temp_w, temp_h, (rx, ry), (rw, rh),
                        tex, ((base_x + rx) as i32, (base_y + ry) as i32), rw, rh,
                    );
                }
                // No backing texture — the result lives only in `temp`, returned
                // as the sync handle so Ruffle reads it back into its CPU pixels.
                Backing::Dropped => {}
            }
        }
        self.warn_once(b"render_offscreen: composite draw() -> handle\n\0");
        // The SyncHandle must stay resolvable until Ruffle actually reads it back,
        // which can be many frames away (`DirtyState::GpuModified` is only drained
        // by `bitmap_data.rs::sync` on the next CPU access). `temp` is pooled, so
        // it can be freed OR reissued to another same-size draw before then —
        // either way the read returns the wrong pixels (#14: PL2's strips read
        // back all-zero; hearts and enemies showing a neighbour's content). Steps
        // 1-3 above already put the finished result in the BitmapData's OWN
        // storage, so point the handle THERE for both real backings and never at
        // the temp:
        //   - Standalone: its texture is premultiplied, exactly the temp's
        //     convention, so it reads back directly.
        //   - Atlas: the packed region holds the result too, but in STRAIGHT
        //     alpha, so the handle carries `premult` and the conversion happens on
        //     the GPU at resolve time. `ticket` pins the atlas until then.
        // Only Dropped still points at the temp, because a budget-dropped surface
        // has no storage at all; its result is ephemeral by definition and the
        // surface is already blank on screen, so a stale read costs nothing.
        let atlas_ticket = as_switch_bitmap(&handle).and_then(|b| b.ticket.clone());
        let sync = match backing {
            Backing::Standalone(s_tex) => BitmapDataSyncHandle {
                texture: s_tex,
                tex_w, tex_h,
                x: bounds.x_min, y: bounds.y_min,
                w: bounds.width(), h: bounds.height(),
                premult: false,
                _ticket: None,
            },
            Backing::Atlas { tex, base_x, base_y, atlas_w, atlas_h } => BitmapDataSyncHandle {
                texture: tex,
                tex_w: atlas_w, tex_h: atlas_h,
                x: base_x + bounds.x_min, y: base_y + bounds.y_min,
                w: bounds.width(), h: bounds.height(),
                premult: true,
                _ticket: atlas_ticket,
            },
            Backing::Dropped => BitmapDataSyncHandle {
                texture: temp_id,
                tex_w: temp_w,
                tex_h: temp_h,
                x: bounds.x_min, y: bounds.y_min,
                w: bounds.width(), h: bounds.height(),
                premult: false,
                _ticket: None,
            },
        };
        // Retire temp for reuse next frame (submit_frame recycles it into the
        // pool) instead of freeing it.
        self.offscreen_temp_retired.push(temp);
        // Read back exactly `bounds`: the resolve closure indexes its buffer
        // relative to this region's origin with stride = bounds.width().
        Some(Box::new(sync))
    }

    fn apply_filter(
        &mut self,
        source: BitmapHandle,
        source_point: (u32, u32),
        source_size: (u32, u32),
        destination: BitmapHandle,
        dest_point: (i32, i32),
        filter: Filter,
    ) -> Option<Box<dyn SyncHandle>> {
        self.apply_filter_calls = self.apply_filter_calls.wrapping_add(1);
        let (fw, fh) = source_size;
        if fw == 0 || fh == 0 {
            return None;
        }
        // Resolve source + destination (atlas or standalone). Fail cleanly (Ruffle
        // keeps the unfiltered pixels) if either can't be resolved.
        let (src_tex, src_tw, src_th, src_bx, src_by, src_atlas) = self.resolve_bitmap_tex(&source)?;
        let (dst_tex, dst_tw, dst_th, dst_bx, dst_by, dst_atlas) = self.resolve_bitmap_tex(&destination)?;
        // Round-trip through two pool temps so the filter always sees a full,
        // 0,0-based source texture (its own coordinate assumption) and never reads
        // and writes the same atlas region at once: copy the source sub-rect into
        // temp_src (PREMULTIPLIED, the temps' convention), filter into temp_dst,
        // then blit temp_dst back to the dest sub-rect. The blits convert alpha at
        // each boundary: atlas is STRAIGHT (premult on the way in, un-premult on the
        // way out), standalone is already PREMULT (identity). The displacement is a
        // pure spatial remap, so it's convention-agnostic in between.
        let temp_src = self.filter_tex_pool.acquire(fw, fh)?;
        let src_pt = (src_bx + source_point.0, src_by + source_point.1);
        let ok_src = if src_atlas {
            self.blit_premult(src_tex, src_tw, src_th, src_pt, (fw, fh), temp_src.texture, (0, 0), fw, fh)
        } else {
            self.blit_identity(src_tex, src_tw, src_th, src_pt, (fw, fh), temp_src.texture, (0, 0), fw, fh)
        };
        // temp_dst is where the filter writes. Use an OFFSCREEN temp (not the
        // filter pool) so it can be RETIRED and survive until Ruffle resolves this
        // BitmapData back to CPU pixels this tick (mirrors render_offscreen).
        let temp_dst = match self.acquire_offscreen_temp(fw, fh) {
            Some(t) => t,
            None => {
                self.filter_tex_pool.release(temp_src);
                return None;
            }
        };
        let ok_filter = ok_src
            && self.apply_filter_raw(
                temp_src.texture, fw, fh, (0, 0), (fw, fh),
                temp_dst.texture, (0, 0), &filter,
            );
        // Write the filtered result into the destination backing for the display
        // path (render_bitmap). Atlas stores STRAIGHT, standalone is PREMULT.
        let ok = ok_filter && {
            let dx = (dst_bx as i32 + dest_point.0).max(0);
            let dy = (dst_by as i32 + dest_point.1).max(0);
            // FlashNX: temp_dst may be bigger than (fw, fh): read with its own size.
            let (tw, th) = (temp_dst.width, temp_dst.height);
            if dst_atlas {
                self.blit_unpremult(temp_dst.texture, tw, th, (0, 0), (fw, fh), dst_tex, (dx, dy), fw, fh)
            } else {
                self.blit_identity(temp_dst.texture, tw, th, (0, 0), (fw, fh), dst_tex, (dx, dy), fw, fh)
            }
        };
        self.filter_tex_pool.release(temp_src);
        // temp_dst held only the filtered sub-rect; its content is already blitted
        // into the dest backing above (for the display path). Recycle it.
        self.offscreen_temp_retired.push(temp_dst);
        if !ok {
            return None;
        }
        // Ruffle marks the WHOLE destination BitmapData dirty after applyFilter
        // (operations.rs: `region = for_whole_size(write.width, write.height)`), then
        // `copy_pixels_to_bitmapdata` indexes our readback buffer across that whole-dest
        // region. A handle sized to only the filtered sub-rect (fw×fh) overruns the
        // buffer as soon as the dest is larger than the filter output → `index out of
        // bounds` panic (GrindCraft: a DropShadow on a small element inside a bigger
        // canvas). So resolve the WHOLE dest — the dest backing already holds the
        // composited result (old pixels + the filtered sub-rect blitted in above).
        let (dest_w, dest_h) = if let Some(s) = as_standalone_bitmap(&destination) {
            (s.0.width, s.0.height)
        } else if let Some(b) = as_switch_bitmap(&destination) {
            (b.width, b.height)
        } else {
            // Unresolvable dest (e.g. a budget-dropped surface): skip the sync rather
            // than hand back a mismatched buffer. Ruffle keeps its CPU pixels.
            return None;
        };
        if dst_atlas {
            // The atlas stores STRAIGHT alpha at an offset; Ruffle's CPU pixels are
            // PREMULTIPLIED. This used to convert into a pooled temp right here and
            // hand the temp to Ruffle, which carried render_offscreen's bug: the
            // read can land many frames later, by which point the temp may have
            // been freed or reissued to another same-size draw. Point at the atlas
            // instead and let `resolve_sync_handle` do the conversion when the read
            // actually happens; `ticket` pins the atlas until then.
            Some(Box::new(BitmapDataSyncHandle {
                texture: dst_tex,
                tex_w: dst_tw, tex_h: dst_th,
                x: dst_bx, y: dst_by,
                w: dest_w, h: dest_h,
                premult: true,
                _ticket: as_switch_bitmap(&destination).and_then(|b| b.ticket.clone()),
            }))
        } else {
            // Standalone dest: its own texture IS the whole dest, premultiplied and
            // persistent — resolve it directly (same rationale as render_offscreen's
            // standalone handle: it survives a deferred read, unlike a pooled temp).
            Some(Box::new(BitmapDataSyncHandle {
                texture: dst_tex,
                tex_w: dest_w, tex_h: dest_h,
                x: 0, y: 0,
                w: dest_w, h: dest_h,
                premult: false,
                _ticket: None,
            }))
        }
    }

    fn is_filter_supported(&self, filter: &Filter) -> bool {
        let (ord, name) = filter_variant_ordinal(filter);
        let bit = 1u16 << ord;
        let prev = self.filters_seen_mask.fetch_or(bit, Ordering::Relaxed);
        if prev & bit == 0 {
            let msg = std::format!("is_filter_supported: {} (first sighting)\n", name);
            let mut bytes = msg.into_bytes();
            bytes.push(0);
            unsafe { ruffle_log_cstr(bytes.as_ptr() as *const _) };
        }
        // Re-enabled 2026-05-29 after fixing the crash root cause: the filter
        // shader chain itself was fine, but the FilterTexturePool grew
        // unbounded (one texture per distinct size, never freed) → texture
        // exhaustion → glGenTextures returned 0 → Mesa NULL-deref. Fixed by
        // bounding the pool (MAX_POOLED_FILTER_TEXTURES) and returning None
        // from make_standalone_texture on failure (filters then skip cleanly
        // instead of using a 0 texture). Restores glow/drop-shadow (e.g. the
        // outlined-letter borders Mario 63 draws on its menu text).
        matches!(
            filter,
            Filter::ColorMatrixFilter(_)
                | Filter::BlurFilter(_)
                | Filter::GlowFilter(_)
                | Filter::DropShadowFilter(_)
                | Filter::BevelFilter(_)
                | Filter::DisplacementMapFilter(_)
        )
    }

    fn is_offscreen_supported(&self) -> bool {
        // Enabled with the minimal cache path (no filter shaders yet). Ruffle
        // will cacheAsBitmap filtered/cached display objects, render their
        // commands into our standalone textures, and draw them back. Filters
        // in cache_entries are ignored for now (is_filter_supported = false),
        // so we render the unfiltered source content — visible content shows,
        // alpha~0+filter-only content (platforms) stays invisible until the
        // filter pipeline lands.
        true
    }

    fn submit_frame(
        &mut self,
        clear: Color,
        commands: CommandList,
        cache_entries: Vec<BitmapCacheEntry>,
    ) {
        // Everything this frame will sample must have been written first: this
        // is where the writes held back during the tick are sent, once each
        // rather than once per overwritten intermediate state.
        self.flush_pending_upload();
        // Drain any pending arena frees enqueued by `GpuDraw::drop`. Doing
        // this at frame boundaries (not from Drop itself) keeps us off the
        // hook for &mut access during arbitrary Ruffle drops, and keeps
        // arena bookkeeping localised to the GL thread.
        {
            let mut pending = PENDING_FREES.lock().unwrap();
            for f in pending.drain(..) {
                self.vertex_arena.retire(f.vbo_offset, f.vbo_size);
                self.index_arena.retire(f.ibo_offset, f.ibo_size);
            }
        }
        // Behind a fence until the GPU is done with them (`upload`).
        self.vertex_arena.fence_retired();
        self.index_arena.fence_retired();
        // Evictable SWF images: empty idle atlases once a second, just before
        // the drain below frees them.
        if self.frame_count % 60 == 0 {
            self.evict_idle_images();
        }
        // Drain atlas releases (issue #56b): a dropped bitmap's AtlasTicket enqueued
        // its atlas index; decrement the live count and free the 16 MB texture when
        // it reaches 0, so re-cached large offscreen surfaces don't leak to OOM.
        {
            let mut pending = PENDING_ATLAS_RELEASE.lock().unwrap();
            for (idx, slot) in pending.drain(..) {
                if let Some(atlas) = self.atlases.get_mut(idx) {
                    if atlas.texture != 0 {
                        atlas.live = atlas.live.saturating_sub(1);
                        if atlas.live > 0 {
                            // The slot is free for the next bitmap. Held texture
                            // writes were flushed at the top of submit_frame,
                            // before this drain, so none can still land in it.
                            atlas.free_slots.push(slot);
                        }
                        if atlas.live == 0 {
                            // Reclaim big-surface budget before the texture is
                            // dropped (dims read here — free_gl zeroes them). A
                            // dedicated big atlas is right-sized, so its dims are
                            // never exactly 2048² like a shared one — that's how we
                            // tell them apart (a shared atlas was never counted).
                            if atlas.width != ATLAS_SIZE || atlas.height != ATLAS_SIZE {
                                let bytes = atlas.width as u64 * atlas.height as u64 * 4;
                                self.big_atlas_live_bytes =
                                    self.big_atlas_live_bytes.saturating_sub(bytes);
                                self.big_atlas_free_total =
                                    self.big_atlas_free_total.wrapping_add(1);
                            }
                            atlas.free_gl(); // texture deleted, slot reusable
                        }
                    }
                }
            }
        }

        // Snapshot+reset the per-frame backend-primitive timers. We're at the
        // start of submit_frame — right after player.tick() ran the AVM frame and
        // any render_offscreen/upload/resolve it triggered — so CUR holds exactly
        // this frame's tick-side primitive time. Move it to LAST (read by
        // log_slow_frame, which runs just after) and zero CUR for the next frame.
        PRIM_OFFSCREEN_LAST.store(
            PRIM_OFFSCREEN_CUR.swap(0, std::sync::atomic::Ordering::Relaxed),
            std::sync::atomic::Ordering::Relaxed,
        );
        PRIM_BMPUP_LAST.store(
            PRIM_BMPUP_CUR.swap(0, std::sync::atomic::Ordering::Relaxed),
            std::sync::atomic::Ordering::Relaxed,
        );
        PRIM_RESOLVE_LAST.store(
            PRIM_RESOLVE_CUR.swap(0, std::sync::atomic::Ordering::Relaxed),
            std::sync::atomic::Ordering::Relaxed,
        );
        // DIAG: render_offscreen sub-phase timers (see statics near top).
        for (cur, last) in [
            (&PRIM_OFF_ALLOC_CUR, &PRIM_OFF_ALLOC_LAST),
            (&PRIM_OFF_RENDER_CUR, &PRIM_OFF_RENDER_LAST),
            (&PRIM_OFF_READBACK_CUR, &PRIM_OFF_READBACK_LAST),
            (&PRIM_OFF_UPLOAD_CUR, &PRIM_OFF_UPLOAD_LAST),
            (&PRIM_OFF_N_CUR, &PRIM_OFF_N_LAST),
            (&PRIM_OFF_PIX_CUR, &PRIM_OFF_PIX_LAST),
        ] {
            last.store(
                cur.swap(0, std::sync::atomic::Ordering::Relaxed),
                std::sync::atomic::Ordering::Relaxed,
            );
        }

        // Recycle this frame's render_offscreen temps into the reuse pool, then
        // evict by RECENCY. Every byte-quota scheme tried here was wrong, because
        // the question is not "how much may we hold" but "is anyone still using
        // this one":
        //   - A count cap of 128 hoarded ~330 MB of stale varied-size temps on Icy
        //     Tower (GL_OUT_OF_MEMORY), and `truncate` kept the stale FRONT while
        //     discarding the freshly retired tail that was about to be reused.
        //   - A flat 64 MB byte cap held exactly 68 of catmario's 87 same-size
        //     tiles, so the surplus was freed and re-created every frame until the
        //     texture heap fragmented and glTexImage2D failed.
        //   - Sizing the cap from measured demand fixed catmario but collapsed to
        //     its floor whenever a game stopped drawing offscreen for a moment
        //     (Papa Louie 3 sits at offN=0 during play), which freed temps a live
        //     SyncHandle still pointed at. That is #14 all over again: an
        //     atlas-backed handle points at the temp, Ruffle parks it in
        //     DirtyState::GpuModified until some later CPU access, and the
        //     readback then returns an empty texture. Hearts and enemies vanished
        //     while the player, whose 8007px strip is standalone and therefore
        //     immune, stayed visible.
        // Recency answers all three at once. A temp reacquired every frame keeps
        // its stamp refreshed and is never evicted (catmario). A temp whose size
        // nobody asks for again ages out on its own (Icy Tower), whatever the
        // budget would have allowed. And nothing is freed while it may still be
        // read, as long as the grace period outlasts a deferred resolve.
        // MAX_BYTES stays only as a backstop against a pathological game, and
        // evicts oldest-first so it can never again throw away the hot set.
        // Eviction only ever runs UNDER PRESSURE. Below the soft threshold the
        // pool keeps everything indefinitely, which is what the old flat cap did
        // in practice for a game that stops drawing offscreen (Papa Louie 3 sits
        // at offN=0 during play, so nothing ever pushed its temps out) and is the
        // behaviour its live SyncHandles were validated against. Freeing on a
        // timer alone would still be more aggressive than that and could strand a
        // late resolve, so idleness on its own must not cost a temp its life.
        const OFFSCREEN_TEMP_SOFT_BYTES: usize = 64 * 1024 * 1024;
        const OFFSCREEN_TEMP_HARD_BYTES: usize = 192 * 1024 * 1024;
        const OFFSCREEN_TEMP_GRACE_FRAMES: u32 = 120;
        let now = self.frame_count;
        for tex in self.offscreen_temp_retired.drain(..) {
            self.offscreen_temp_pool.push(PooledTemp { tex, last_used_frame: now });
        }
        let bytes_of = |t: &PooledTemp| (t.tex.width as usize) * (t.tex.height as usize) * 4;
        let mut held: usize = self.offscreen_temp_pool.iter().map(bytes_of).sum();
        if held > OFFSCREEN_TEMP_SOFT_BYTES {
            // Over the threshold: drop what nobody has asked for in a while.
            // catmario's 87 tiles all carry this frame's stamp so none qualify and
            // the hot set survives whole; Icy Tower's stale varied sizes are
            // exactly what this removes. `saturating_sub` keeps the first frames
            // sane before `frame_count` passes the grace window.
            let cutoff = now.saturating_sub(OFFSCREEN_TEMP_GRACE_FRAMES);
            self.offscreen_temp_pool.retain(|t| t.last_used_frame >= cutoff);
            held = self.offscreen_temp_pool.iter().map(bytes_of).sum();
        }
        if held > OFFSCREEN_TEMP_HARD_BYTES {
            // Backstop for a game that keeps a genuinely huge set hot. Drop the
            // least recently used first — never the hot set, which is how the old
            // `truncate` got it backwards. Sorting is needed because
            // `acquire_offscreen_temp` uses `swap_remove`, so pool order is not
            // stamp order; it only runs in this rare branch.
            self.offscreen_temp_pool.sort_by_key(|t| t.last_used_frame);
            let mut drop_before = 0usize;
            while held > OFFSCREEN_TEMP_HARD_BYTES
                && drop_before < self.offscreen_temp_pool.len()
            {
                held -= bytes_of(&self.offscreen_temp_pool[drop_before]);
                drop_before += 1;
            }
            self.offscreen_temp_pool.drain(0..drop_before);
        }
        self.offscreen_temp_pool_bytes = held;
        // Snapshot counters for the per-frame slow-frame breakdown (consumed
        // right after `commands.execute` below). `cache_entries` is moved by the
        // filter loop, so grab its length up front.
        self.frame_snapshot = self.frame_counters();
        let frame_cache_entries = cache_entries.len() as u32;

        // Render cacheAsBitmap entries: each has a standalone destination
        // texture, a command list, a clear color, and (ignored for now) a
        // filter list. Minimal path — render the source commands directly
        // into the cache texture. Ruffle later draws it back via
        // `render_bitmap`. Filters are NOT applied yet (see is_offscreen).
        // Faithful port of wgpu's submit_frame cache_entries flow
        // (`render/wgpu/src/backend.rs:512`):
        //   1. Render commands directly into entry.handle.texture — this is
        //      the first filter source.
        //   2. Chain filters: each apply() reads `current` and writes into a
        //      fresh pool texture. On unsupported filter (returns None) we
        //      passthrough — keep current_handle. wgpu uses an identity-blit
        //      fallback that allocates a fresh texture; our passthrough is
        //      functionally equivalent and saves one copy.
        //   3. If filters moved current off entry.handle, identity-blit the
        //      final filter texture back into entry.handle (so the cache
        //      texture sees the filtered result).
        self.cache_entries_max_window = self.cache_entries_max_window.max(cache_entries.len() as u32);
        // DIAG: time the whole loop (`cacheUs=` on the SLOW line). Dropped at the
        // end of this block, right before the counters are snapshotted.
        let cache_timer = PrimTimer::new(&PRIM_CACHE_CUR);
        // Age out filter-pool textures not reused recently (TTL eviction).
        self.filter_tex_pool.begin_frame(self.frame_count as u64);
        // Per-frame filter budget. Each filtered cache entry costs ~3-5
        // offscreen passes; a menu *transition* can re-filter dozens of
        // animated elements in one frame, spiking render time. Cap how many
        // filter CHAINS we run per frame — entries past the budget keep the
        // content from step 1 (text/shape) but skip their bevel/glow border for
        // that frame.
        //
        // IMPORTANT: step 1 (render the content into entry.handle) must run for
        // EVERY entry, every frame. `entry.handle` is NOT a persistent cache we
        // can leave stale — Ruffle re-uses/clears it, so skipping step 1 blanks
        // the whole element (the "tous les boutons clignotent / plus de texte"
        // regression). Only the *filter pass* is budgeted, never the content.
        //
        // Budget set high (was 6) so the bevel/glow borders stay present on
        // Mario 63's menus, where many text fields re-cache each frame
        // (cacheMax peaks ~40). Raising it trades a little render time on busy
        // transitions for the reflections no longer dropping in and out. Tune
        // down if a heavy menu hitches.
        const FILTER_CHAINS_PER_FRAME_BUDGET: usize = 48;
        // Round 2 alternated this with a cap of 8 under `instr` and measured
        // 659 us a chain on 8-13. Most of that was texture churn in libdrm, now
        // gone (`filtUs` 23 -> 6 ms with the buffer-object cache), so a lower
        // cap would cost glows for little. `budget=` stays on the SLOW line.
        let budget = FILTER_CHAINS_PER_FRAME_BUDGET;
        self.filter_budget_this_frame = budget as u32;
        let mut filter_chains_run: usize = 0;
        // No scissor anywhere in the loop, not only around step 1's clear: the
        // filter passes write their whole target through `draw_filter_pass`,
        // which never touches it. A Stage3D game can leave one on
        // (SetScissorRectangle), and a pooled temp holds another object's
        // pixels outside it, which the blur would then sample and carry on screen.
        let loop_scissor_was_on = unsafe { glIsEnabled(GL_SCISSOR_TEST) == GL_TRUE };
        if loop_scissor_was_on {
            unsafe { glDisable(GL_SCISSOR_TEST) };
        }
        for entry in cache_entries {
            let Some(standalone) = as_standalone_bitmap(&entry.handle) else {
                self.warn_once(b"cache_entry: non-standalone handle (skipped)\n\0");
                continue;
            };
            let dst_tex = standalone.0.texture;
            let w = standalone.0.width;
            let h = standalone.0.height;

            // Step 1: render the content into entry.handle (ALWAYS — see above).
            {
                let _pt = PrimTimer::new(&PRIM_CONTENT_CUR);
                self.render_commands_to_texture(dst_tex, w, h, entry.commands, Some(entry.clear));
            }
            if entry.filters.is_empty() {
                continue;
            }
            // Over the per-frame filter budget → leave this entry unfiltered for
            // this frame: text/shape is still present (step 1), just without the
            // bevel/glow border this frame.
            if filter_chains_run >= budget {
                continue;
            }
            filter_chains_run += 1;
            let _chain_timer = PrimTimer::new(&PRIM_FILTCHAIN_CUR);

            // Step 2: filter chain using the (now bounded) FilterTexturePool.
            // The first source is entry.handle.texture itself; each successful
            // filter writes into a fresh pool temp and the previous owned temp
            // is released. acquire() can return None (pool/​GL exhaustion guard)
            // — we break and keep whatever we have, so we never feed a 0 texture
            // to the shaders (the old crash).
            let mut current_tex = dst_tex;
            let mut current_owned: Option<StandaloneTexture> = None;
            let n_filters = entry.filters.len();
            for (i, filter) in entry.filters.into_iter().enumerate() {
                // When the LAST link of the chain is a real Blur, aim it straight
                // at `entry.handle` instead of one more pool temp. `run_blur_to_temp`
                // seeds its own temps from the source before anything writes to the
                // destination, so source and destination are allowed to be the same
                // texture here — and that deletes both the pool round trip and step
                // 3's blit below.
                //
                // That blit was a full-surface pass: bind, attach, viewport, one
                // screen-filling quad. Removing it is worth having whichever way
                // the cost of this loop turns out to break down, since it is one
                // fewer render-target switch AND one fewer pass in the chain, and
                // those are the two candidate units (see the `PRIM_RTBIND_CUR`
                // note). A single-Blur entry goes from 6 passes to 5.
                //
                // Blur only. ColorMatrix, and the glow/bevel family which re-read
                // the source in their final combine pass, would sample and write one
                // texture within a single pass, which is undefined.
                //
                // The strength test mirrors `run_blur_to_temp`'s own bail-out: below
                // it that function returns None and `apply_blur_raw` falls back to a
                // source → destination blit, which with both sides equal would be a
                // self-blit. Upstream's `filters.retain(|f| !f.impotent())` already
                // drops those, but it runs BEFORE `filter.scale(stage)`, so a stage
                // that scaled DOWN could still produce one. Cheap to just check.
                let blur_lands_in_place = i + 1 == n_filters
                    && match &filter {
                        Filter::BlurFilter(b) => {
                            b.blur_x.to_f32().min(255.0) > 1.0
                                || b.blur_y.to_f32().min(255.0) > 1.0
                        }
                        _ => false,
                    };
                if blur_lands_in_place {
                    let ok = self.apply_filter_raw(
                        current_tex, w, h, (0, 0), (w, h),
                        dst_tex, (0, 0), &filter,
                    );
                    if ok {
                        // Result is already in entry.handle: drop any temp we were
                        // carrying and leave `current_owned` empty so step 3 is a
                        // no-op. On failure we keep it, and step 3 blits as before.
                        if let Some(prev) = current_owned.take() {
                            self.filter_tex_pool.release(prev);
                        }
                        current_tex = dst_tex;
                    }
                    continue;
                }
                let Some(next) = self.filter_tex_pool.acquire(w, h) else { break };
                let next_tex = next.texture;
                let ok = self.apply_filter_raw(
                    current_tex, w, h, (0, 0), (w, h),
                    next_tex, (0, 0), &filter,
                );
                if ok {
                    if let Some(prev) = current_owned.take() {
                        self.filter_tex_pool.release(prev);
                    }
                    current_tex = next_tex;
                    current_owned = Some(next);
                } else {
                    // Unsupported/failed filter — passthrough, return to pool.
                    self.filter_tex_pool.release(next);
                }
            }

            // Step 3: if the chain moved current off entry.handle, blit the
            // final temp back into entry.handle and return the temp to pool.
            if let Some(final_owned) = current_owned {
                let ft = final_owned.texture;
                self.blit_identity(ft, w, h, (0, 0), (w, h), dst_tex, (0, 0), w, h);
                self.filter_tex_pool.release(final_owned);
            }
        }
        // DIAG: these two accumulate INSIDE submit_frame, i.e. after the
        // snapshot block near the top of this function has already run, so they
        // cannot ride the same CUR -> LAST hand-off as the tick-side timers —
        // that would print the previous frame's number. Publish them here, where
        // the work is finished and `log_slow_frame` has not read anything yet.
        if loop_scissor_was_on {
            unsafe { glEnable(GL_SCISSOR_TEST) };
        }
        drop(cache_timer);
        PRIM_CACHE_LAST.store(
            PRIM_CACHE_CUR.swap(0, std::sync::atomic::Ordering::Relaxed),
            std::sync::atomic::Ordering::Relaxed,
        );
        PRIM_RTBIND_LAST.store(
            PRIM_RTBIND_CUR.swap(0, std::sync::atomic::Ordering::Relaxed),
            std::sync::atomic::Ordering::Relaxed,
        );
        // Same hand-off for the round-2 split. MKTEX is the odd one: it fills up
        // during `render_base`, which runs earlier in the same `player.render()`,
        // so publishing it here still catches THIS frame.
        for (cur, last) in [
            (&PRIM_CONTENT_CUR, &PRIM_CONTENT_LAST),
            (&PRIM_FILTCHAIN_CUR, &PRIM_FILTCHAIN_LAST),
            (&PRIM_MKTEX_CUR, &PRIM_MKTEX_LAST),
            (&PRIM_MKTEX_N_CUR, &PRIM_MKTEX_N_LAST),
        ] {
            last.store(
                cur.swap(0, std::sync::atomic::Ordering::Relaxed),
                std::sync::atomic::Ordering::Relaxed,
            );
        }

        // Drain GL errors once per second, plus a one-line heartbeat with
        // running counters every 2 seconds. Quiet otherwise.
        self.frame_count = self.frame_count.wrapping_add(1);
        // Diagnostic heartbeat: full counters every 60 frames (~1 s), plus a
        // 1-byte-cheap per-frame tick so the LAST frame before a crash is
        // visible in the log. The previous 120-frame cadence left a ~2 s
        // window of total silence around the jetpack crash.
        //
        // Note about RAM: the previous "WARN low ram" alert was misleading.
        // `svcGetInfo(UsedMemorySize)` returns the heap RESERVED by the
        // applet (set once at crt0), not the heap actually consumed by
        // malloc. It barely moves, so a 99% ratio at boot is normal and the
        // warning fired every 30 frames for nothing. Removed.
        if self.frame_count % 60 == 0 {
            // Wall-clock FPS over the last 60-frame window. armGetSystemTick
            // runs at ~19.2 MHz so the resolution is ~50 ns — way more than
            // FPS needs. We log "—" on the very first heartbeat since we
            // don't have a previous tick to subtract from.
            let now_tick = unsafe { ruffle_tick_now() };
            let tick_freq = unsafe { ruffle_tick_freq() };
            // A window the console spent asleep or behind the HOME menu is not a
            // measurement. Dropping the previous tick sends this one down the
            // "—" path below, which is also what keeps it out of the worst-case
            // slot a bug report now carries.
            if HEARTBEAT_RESET.swap(false, std::sync::atomic::Ordering::Relaxed) {
                self.heartbeat_tick = 0;
            }
            let fps_str = if self.heartbeat_tick != 0 && tick_freq > 0 {
                let dt_ticks = now_tick.saturating_sub(self.heartbeat_tick);
                if dt_ticks > 0 {
                    // 60 frames over `dt_ticks` ticks at `tick_freq` Hz =
                    // 60 * tick_freq / dt_ticks frames per second. Multiply
                    // by 10 then format as "X.Y" to get one decimal place
                    // without pulling in float formatting.
                    let fps_x10 = (60u64 * tick_freq * 10) / dt_ticks;
                    std::format!("{}.{}", fps_x10 / 10, fps_x10 % 10)
                } else {
                    std::string::String::from("inf")
                }
            } else {
                std::string::String::from("—")
            };
            self.heartbeat_tick = now_tick;
            // Read + clear the tick/render time accumulators populated by
            // render_frame_with_dt in lib.rs. Convert from system ticks
            // (~19.2 MHz) to milliseconds across the 60-frame window. Mean
            // per-frame time = total_ms / 60. Helps localise the bottleneck:
            //   tick=high render=low → AVM1/game-logic CPU bound
            //   tick=low  render=high → GL/draw-call bound
            //   tick=high render=high → both contribute (shape register etc)
            let tick_total_ticks = crate::TICK_TICKS_ACCUM.swap(0, Ordering::Relaxed);
            let render_total_ticks = crate::RENDER_TICKS_ACCUM.swap(0, Ordering::Relaxed);
            let tick_max_ticks = crate::TICK_TICKS_MAX.swap(0, Ordering::Relaxed);
            let render_max_ticks = crate::RENDER_TICKS_MAX.swap(0, Ordering::Relaxed);
            let (tick_ms, render_ms, tick_max_ms, render_max_ms) = if tick_freq > 0 {
                (
                    (tick_total_ticks * 1000) / tick_freq,
                    (render_total_ticks * 1000) / tick_freq,
                    (tick_max_ticks * 1000) / tick_freq,
                    (render_max_ticks * 1000) / tick_freq,
                )
            } else {
                (0, 0, 0, 0)
            };
            let cache_max = self.cache_entries_max_window;
            self.cache_entries_max_window = 0;
            let draw_calls = self.draw_calls_this_window;
            self.draw_calls_this_window = 0;
            let (pushmask, amask, maskeddraw, maskshape) = (
                self.push_mask_window, self.alpha_mask_window,
                self.masked_draw_window, self.mask_shape_draw_window,
            );
            let blend = self.blend_window;
            self.push_mask_window = 0;
            self.alpha_mask_window = 0;
            self.masked_draw_window = 0;
            self.mask_shape_draw_window = 0;
            self.blend_window = 0;
            let (ram_used, ram_total) = query_ram();
            let live_s = LIVE_GPU_SHAPES.load(Ordering::Relaxed);
            let live_d = LIVE_GPU_DRAWS.load(Ordering::Relaxed);
            let v_used_mb = self.vertex_arena.in_use_bytes() / (1024 * 1024);
            let v_peak_mb = self.vertex_arena.peak_in_use / (1024 * 1024);
            let i_used_mb = self.index_arena.in_use_bytes() / (1024 * 1024);
            let i_peak_mb = self.index_arena.peak_in_use / (1024 * 1024);
            self.arena_record_update(v_peak_mb as u64, i_peak_mb as u64);
            if self.frame_count % 300 == 0 {
                self.log_atlas_idle();
                let (gotos, run, skipped, list_skipped) = ruffle_core::take_inner_goto_stats();
                let subtrees_skipped = ruffle_core::take_subtrees_skipped();
                let enter_skipped = ruffle_core::take_enter_skipped();
                if gotos > 0 || subtrees_skipped > 0 || enter_skipped > 0 {
                    log_str(&std::format!(
                        "gotos: f{} last 300 frames: {} inner gotos ({} without touching the orphan list), orphans walked {} skipped {}, unchanged subtrees passed over {}, quiet subtrees passed over in enterFrame {}\n",
                        self.frame_count, gotos, list_skipped, run, skipped, subtrees_skipped, enter_skipped
                    ));
                }
                // Which event sounds were started, and how often: a sound
                // restarted every frame, or one asked to loop forever.
                let starts = ruffle_core::backend::audio::take_sound_start_stats();
                if !starts.is_empty() {
                    let total: u32 = starts.iter().map(|s| s.1).sum();
                    let top: std::vec::Vec<std::string::String> = starts
                        .iter()
                        .take(5)
                        .map(|(id, n, loops)| std::format!("#{:x} x{} loops={}", id, n, loops))
                        .collect();
                    log_str(&std::format!(
                        "sounds: f{} last 300 frames: {} starts; top {}\n",
                        self.frame_count,
                        total,
                        top.join(", ")
                    ));
                }
                self.log_evict();
                // Live memory by call stack, every 1200 frames (heapprof build).
                #[cfg(feature = "heapprof")]
                if self.frame_count % 1200 == 0 {
                    crate::heapprof::dump("sdmc:/switch/FlashNX/heapprof.bin");
                    log_str(&std::format!("heapprof: f{} written\n", self.frame_count));
                }
                // GPU objects against nvdrv's bookkeeping ceiling (~2 KB per
                // handle in its transfer memory, see nouveau.c).
                log_str(&std::format!(
                    "gpu: f{} nvmap handles {} (bookkeeping {} MB), shape regions waiting for the GPU {}+{} KB in {}+{} batches, full-arena waits {}+{}\n",
                    self.frame_count,
                    unsafe { flashnx_nvmap_live() },
                    unsafe { __nx_nv_transfermem_size } / (1024 * 1024),
                    self.vertex_arena.quarantined_bytes / 1024,
                    self.index_arena.quarantined_bytes / 1024,
                    self.vertex_arena.quarantine.len(),
                    self.index_arena.quarantine.len(),
                    self.vertex_arena.quarantine_waits,
                    self.index_arena.quarantine_waits,
                ));
                // dlmalloc's wholly free segments back to the top region.
                let trimmed = crate::dl_trim();
                if trimmed > 0 {
                    log_str(&std::format!(
                        "dl: f{} gave back {} MB of empty segments\n",
                        self.frame_count,
                        trimmed / (1024 * 1024)
                    ));
                }
                // How the heap is split: big blocks at the top, newlib below.
                {
                    let mut h = [0u64; 8];
                    unsafe { flashnx_heap_stats(h.as_mut_ptr()) };
                    let mb = |b: u64| b / (1024 * 1024);
                    log_str(&std::format!(
                        "heap: f{} top region {}MB ({}MB in {} blocks, {} holes), biggest free {}MB, room below {}MB, big refused {} (newlib served {})\n",
                        self.frame_count, mb(h[0]), mb(h[1]), h[2], h[3], mb(h[4]), mb(h[5]), h[6], h[7]
                    ));
                }
                // Which ActionScript 3 functions took the time (as3prof.on).
                let as3 = ruffle_core::as3prof_report(25);
                if !as3.is_empty() {
                    log_str(&std::format!("as3prof: f{} last 300 frames\n{}", self.frame_count, as3));
                }
                // AVM2 JIT: compiled, rejected (why), handed back (which op).
                let jit = ruffle_core::jit_report();
                if !jit.is_empty() {
                    log_str(&std::format!("jit: f{} {}\n", self.frame_count, jit));
                }
                // How many AVM2 ops ran, their pairs, callee sizes (opcount only).
                let ops = ruffle_core::avm2ops_report(300);
                if !ops.is_empty() {
                    log_str(&std::format!("avm2ops: f{} last 300 frames\n{}", self.frame_count, ops));
                }
                // What is alive: movies, display objects, AVM2 objects per
                // class (empty unless Ruffle is built with flashnx_instr).
                let census = ruffle_core::census_report();
                if !census.is_empty() {
                    log_str(&std::format!("census: f{} {}\n", self.frame_count, census));
                }
                // BitmapData.threshold calls that changed nothing, and so no
                // longer send their bitmap back to the GPU.
                let (thr_calls, thr_unchanged) = ruffle_core::take_threshold_stats();
                let thr_memo = ruffle_core::take_threshold_memo_hits();
                if thr_calls > 0 {
                    log_str(&std::format!(
                        "threshold: f{} last 300 frames: {} calls, {} changed nothing ({} answered without scanning)\n",
                        self.frame_count, thr_calls, thr_unchanged, thr_memo
                    ));
                }
                // Standalone textures by origin, in MB (atlases are on the
                // atlasIdle line).
                log_str(&std::format!(
                    "textures: f{} stTexMB={}\n",
                    self.frame_count,
                    standalone_breakdown()
                ));
            }
            let v_frag = self.vertex_arena.free.len();
            let i_frag = self.index_arena.free.len();
            // Actual CPU clock (MHz) + dock state, so we can read whether
            // CpuBoostMode is holding the A57 at 1785 MHz during heavy AVM1
            // scenes (the water lake) — confirming if any CPU headroom remains.
            let cpu_mhz = unsafe { ruffle_cpu_clock_hz() } / 1_000_000;
            let gpu_mhz = unsafe { ruffle_gpu_clock_hz() } / 1_000_000;
            let drift = unsafe { flashnx_clock_drift() };
            let docked = unsafe { ruffle_is_docked() } != 0;
            // Skin temperature, battery temperature, battery charge. The whole
            // reason the power setting and this probe ship together: nobody has
            // ever run a raised clock long enough on this console to see a
            // thermal plateau, so a guard written today would be a threshold
            // nobody has calibrated.
            //
            // Read `skin` against the numbers Horizon itself uses: 53000 (fan
            // pinned at 60% in handheld), 58000 (60 s sleep timer), 61000 (10 s
            // timer), 63000 (immediate sleep). The handheld response is SLEEP,
            // never throttling, so the failure this is watching for is the
            // console dropping the player out mid-game, not damage.
            //
            // `bat` is in per-mille and is the only route to a power figure at
            // all: psm exposes no instantaneous current, and the I2C helper that
            // does live inside a sysmodule with its own NPDM. Two readings far
            // enough apart give %/h, and ~16 Wh nominal turns that into watts.
            let skin_mc = unsafe { flashnx_skin_temp_mc() };
            let batt_mc = unsafe { flashnx_batt_temp_mc() };
            let batt_pm = unsafe { flashnx_batt_permille() };
            // What the small-object cache HOLDS, next to what it has handed
            // out. `slabMB` is the second of those and was the only one
            // reported, so the tens of megabytes the cache keeps between its
            // free lists never appeared anywhere in a bug report's memory
            // picture. Printed as n/max because the ceiling is the cache's
            // other silent failure: at 16/16 it stops growing and every new
            // small block falls back to newlib, and that path sets no flag —
            // `!STARVED` covers the refusal, nothing covered the cap.
            let (slab_chunks, slab_chunks_max) = crate::slab_chunks();
            // `heap=` comes from mallinfo, which walks every free list of
            // newlib's heap: ~10 ms a call once the heap is fragmented
            // (sampling profiler on Super Mario 63, 2026-09-23), so reading it
            // on every heartbeat was a hitch every 60 frames in every game.
            // Read on the first heartbeat and every 30th after it; the ones in
            // between repeat that reading.
            let heap_used = {
                use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
                static HEAP_USED: AtomicU64 = AtomicU64::new(0);
                if (self.frame_count / 60) % 30 == 1 || HEAP_USED.load(Relaxed) == 0 {
                    let used = unsafe { ruffle_heap_used() };
                    HEAP_USED.store(used, Relaxed);
                    used
                } else {
                    HEAP_USED.load(Relaxed)
                }
            };
            let msg = std::format!(
                "f{}: fps={} cpu={}MHz gpu={}MHz drift={} skin={} batC={} bat={} dock={} tick={}ms render={}ms dc/win={} shapes={}(live {}) draws_live={} arena_v={}MB/peak{}MB(frag {}) arena_i={}MB/peak{}MB(frag {}) arenaDropV={} arenaDropI={} bitmaps={} atlases={} bigMB={}/{} bigA/F/D={}/{}/{} bdMB={} bitmap_draws={} offscreen={} sync={} filter={} fpool={} stTex={} otpool={}/{}MB pushmask={} amask={} blend={} maskeddraw={} maskshape={} tickMax={}ms rndMax={}ms cacheMax={} ram={}MB/{}MB heap={}MB slabMB={} slabChunks={}/{}{} dlMB={}/{}({}) drawbox={} maxalpha={:.2}\n",
                self.frame_count,
                fps_str,
                cpu_mhz,
                gpu_mhz,
                drift,
                skin_mc,
                batt_mc,
                batt_pm,
                docked,
                tick_ms,
                render_ms,
                draw_calls,
                self.shapes_registered,
                live_s,
                live_d,
                v_used_mb, v_peak_mb, v_frag,
                i_used_mb, i_peak_mb, i_frag,
                self.vertex_arena.alloc_failures,
                self.index_arena.alloc_failures,
                self.bitmaps_registered,
                self.atlases.iter().filter(|a| a.texture != 0).count(), // LIVE atlases (#56b)
                self.big_atlas_live_bytes / (1024 * 1024),
                self.big_atlas_peak_bytes / (1024 * 1024),
                self.big_atlas_alloc_total,
                self.big_atlas_free_total,
                self.big_atlas_dropped_total,
                ruffle_core::bitmap::bitmap_data::bitmapdata_live_bytes() / (1024 * 1024),
                self.bitmap_draws_emitted,
                self.render_offscreen_calls,
                self.resolve_sync_calls,
                self.apply_filter_calls,
                self.filter_tex_pool.len(),
                LIVE_STANDALONE.load(std::sync::atomic::Ordering::Relaxed),
                self.offscreen_temp_pool.len(),
                self.offscreen_temp_pool_bytes / (1024 * 1024),
                pushmask,
                amask,
                blend,
                maskeddraw,
                maskshape,
                tick_max_ms,
                render_max_ms,
                cache_max,
                ram_used / (1024 * 1024),
                ram_total / (1024 * 1024),
                heap_used / (1024 * 1024),
                crate::slab_bytes() / (1024 * 1024),
                slab_chunks,
                slab_chunks_max,
                // Sticky: no contiguous 32 MB was available at some point, so
                // small blocks are falling back to newlib. See `region_grow_failed`.
                if crate::region_grow_failed() { "!STARVED" } else { "" },
                // dlmalloc: MB in use / MB of segments held (segments).
                crate::dl_segments().0 / (1024 * 1024),
                crate::dl_segments().1 / (1024 * 1024),
                crate::dl_segments().2,
                match self.draw_extent.take() {
                    Some((x0, y0, x1, y1)) => std::format!(
                        "{:.0},{:.0}..{:.0},{:.0}", x0, y0, x1, y1
                    ),
                    None => std::string::String::from("(nothing drawn)"),
                },
                core::mem::replace(&mut self.draw_max_alpha, 0.0),
            );
            let mut bytes = msg.into_bytes();
            bytes.push(0);
            unsafe { ruffle_log_cstr(bytes.as_ptr() as *const _) };
        } else if self.frame_count % 10 == 0 {
            // Tight tick every 10 frames so we know "we made it to f3170"
            // even when the heartbeat hasn't fired. Very short payload.
            let msg = std::format!("·f{}\n", self.frame_count);
            let mut bytes = msg.into_bytes();
            bytes.push(0);
            unsafe { ruffle_log_cstr(bytes.as_ptr() as *const _) };
        }
        if self.frame_count % 60 == 0 {
            unsafe {
                let mut err = glGetError();
                while err != GL_NO_ERROR {
                    let name = match err {
                        GL_INVALID_ENUM => "GL_INVALID_ENUM",
                        GL_INVALID_VALUE => "GL_INVALID_VALUE",
                        GL_INVALID_OPERATION => "GL_INVALID_OPERATION",
                        GL_OUT_OF_MEMORY => "GL_OUT_OF_MEMORY",
                        GL_INVALID_FRAMEBUFFER_OPERATION => "GL_INVALID_FRAMEBUFFER_OPERATION",
                        _ => "GL_UNKNOWN",
                    };
                    let msg = std::format!("gl err: 0x{:04X} ({})\n", err, name);
                    let mut bytes = msg.into_bytes();
                    bytes.push(0);
                    ruffle_log_cstr(bytes.as_ptr() as *const _);
                    err = glGetError();
                }
            }
        }

        // Screen filter (issue #65): capture the frame into our own target so the
        // resolve below can run it through a shader. Done HERE, before the clear,
        // so the clear and every mask land in the captured frame. Falling back to
        // false leaves the original path byte for byte.
        let filter_mode = screen_filter();
        let filtered = filter_mode != 0 && self.begin_screen_filter();
        let (phys_w, phys_h) = self.physical_dims();

        unsafe {
            glViewport(0, 0, phys_w as GLsizei, phys_h as GLsizei);
            glClearColor(
                clear.r as GLfloat / 255.0,
                clear.g as GLfloat / 255.0,
                clear.b as GLfloat / 255.0,
                clear.a as GLfloat / 255.0,
            );
            glClearStencil(0);
            glClear(GL_COLOR_BUFFER_BIT | GL_STENCIL_BUFFER_BIT);

            glEnable(GL_BLEND);
            glBlendFunc(GL_SRC_ALPHA, GL_ONE_MINUS_SRC_ALPHA);
            glDisable(GL_STENCIL_TEST);
            glColorMask(GL_TRUE, GL_TRUE, GL_TRUE, GL_TRUE);
            glStencilMask(0xFF);
        }
        self.mask = MaskState::default();
        // Anything outside our render path (Ruffle internals, our own
        // overlay path) may have touched GL state since the last frame's
        // closing reset. Drop the cache so the first use_* below
        // unconditionally re-binds.
        self.gl_state.invalidate();

        // The game's own display list, and nothing else, carries the free zoom
        // (issue #101). The clear above and the filter resolve below stay at
        // screen scale, as do the pointer and the pause panel drawn after.
        self.game_layer = true;
        commands.execute(self);
        self.game_layer = false;

        unsafe {
            glUseProgram(0);
            glBindVertexArray(0);
            glBindTexture(GL_TEXTURE_2D, 0);
            glDisable(GL_STENCIL_TEST);
            glColorMask(GL_TRUE, GL_TRUE, GL_TRUE, GL_TRUE);
        }
        // Resolve the captured frame onto the real framebuffer through the filter
        // shader. After this the target is the screen again, so the cursor overlay
        // and the pause panel drawn later stay crisp and unfiltered.
        if filtered {
            self.end_screen_filter(filter_mode);
        }
        // Mirror the actual GL state we just wrote so the cache stays
        // truthful for any post-frame work (e.g. the cursor overlay).
        self.gl_state.invalidate();

        // Close out the per-frame breakdown: delta vs the top-of-frame
        // snapshot. Cumulative counters use wrapping_sub (exact); window
        // counters use saturating_sub so the 1-in-60 heartbeat frame (which
        // zeroed them mid-frame) clamps to 0 instead of printing garbage.
        let s = self.frame_snapshot;
        self.last_frame = FrameBreakdown {
            draw_calls: self.draw_calls_this_window.saturating_sub(s.draw_calls),
            offscreen: self.render_offscreen_calls.wrapping_sub(s.offscreen),
            filter: self.apply_filter_calls.wrapping_sub(s.filter),
            resolve: self.resolve_sync_calls.wrapping_sub(s.resolve),
            bmp_uploads: self.bitmaps_registered.wrapping_sub(s.bmp_uploads),
            shape_regs: self.shapes_registered.wrapping_sub(s.shape_regs),
            blend: self.blend_window.saturating_sub(s.blend),
            pushmask: self.push_mask_window.saturating_sub(s.pushmask),
            masked_draw: self.masked_draw_window.saturating_sub(s.masked_draw),
            cache_entries: frame_cache_entries,
            filter_chains: filter_chains_run as u32,
            filter_budget: self.filter_budget_this_frame,
        };
        // Publish this frame's blend timing/counts. Swapped HERE (end of submit)
        // and not at the top like PRIM_*, because blends happen inside this
        // function — a top-of-frame swap would publish the previous frame and
        // read as ~0 (which is exactly why every existing sub-timer shows zero).
        use std::sync::atomic::Ordering as AtomOrd;
        BLEND_TICKS_FRAME.store(BLEND_TICKS_CUR.swap(0, AtomOrd::Relaxed), AtomOrd::Relaxed);
        BLEND_N_TRIVIAL_FRAME
            .store(BLEND_N_TRIVIAL_CUR.swap(0, AtomOrd::Relaxed), AtomOrd::Relaxed);
        BLEND_N_COMPLEX_FRAME
            .store(BLEND_N_COMPLEX_CUR.swap(0, AtomOrd::Relaxed), AtomOrd::Relaxed);
        RT_BIND_FRAME.store(RT_BIND_CUR.swap(0, AtomOrd::Relaxed), AtomOrd::Relaxed);
        // Texture traffic of the whole frame, from the tick's GC drops to the
        // main command list above.
        for (cur, last) in [
            (&TEX_MAKE_N_CUR, &TEX_MAKE_N_LAST),
            (&TEX_MAKE_T_CUR, &TEX_MAKE_T_LAST),
            (&TEX_DEL_N_CUR, &TEX_DEL_N_LAST),
            (&TEX_DEL_T_CUR, &TEX_DEL_T_LAST),
            (&TEX_EVICT_N_CUR, &TEX_EVICT_N_LAST),
            (&TEX_BF_T_CUR, &TEX_BF_T_LAST),
        ] {
            last.store(cur.swap(0, AtomOrd::Relaxed), AtomOrd::Relaxed);
        }
        {
            let now = boc_stats_now();
            for i in 0..BOC_STATS {
                self.boc_frame[i] = if i == BOC_HELD_N || i == BOC_HELD_BYTES {
                    now[i]
                } else {
                    now[i].wrapping_sub(self.boc_prev[i])
                };
            }
            self.boc_prev = now;
        }
        {
            let (a, f, at, ft, sm) = crate::alloc_counters();
            ALLOC_D_FRAME.store(a.saturating_sub(ALLOC_N_LAST.swap(a, AtomOrd::Relaxed)), AtomOrd::Relaxed);
            FREE_D_FRAME.store(f.saturating_sub(FREE_N_LAST.swap(f, AtomOrd::Relaxed)), AtomOrd::Relaxed);
            ALLOC_T_FRAME.store(at.saturating_sub(ALLOC_T_LAST.swap(at, AtomOrd::Relaxed)), AtomOrd::Relaxed);
            FREE_T_FRAME.store(ft.saturating_sub(FREE_T_LAST.swap(ft, AtomOrd::Relaxed)), AtomOrd::Relaxed);
            SMALL_D_FRAME.store(sm.saturating_sub(SMALL_N_LAST.swap(sm, AtomOrd::Relaxed)), AtomOrd::Relaxed);
        }
    }

    fn create_empty_texture(
        &mut self,
        width: NonZeroU32,
        height: NonZeroU32,
    ) -> Result<BitmapHandle, Error> {
        // Standalone (FBO-attachable) texture — Ruffle hands this to
        // `render_offscreen` / cache_entries for cacheAsBitmap + filtered
        // display objects, then draws it back via `render_bitmap`.
        // Ruffle calls this from `render_base`, once per object whose
        // post-filter pixel size changed, which for anything rotating is nearly
        // every frame (20 to 40 calls a frame on Mario 63's world 8). Each one
        // used to cost ~450 us in libdrm; see "Texture churn" at the top of the
        // file for why it no longer does.
        let _pt = PrimTimer::new(&PRIM_MKTEX_CUR);
        PRIM_MKTEX_N_CUR.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let standalone = make_standalone_texture(width.get(), height.get())
            .ok_or(Error::TooLarge)?
            .tagged(ST_CACHE);
        self.bitmaps_registered = self.bitmaps_registered.wrapping_add(1);
        Ok(BitmapHandle(Arc::new(StandaloneBitmap(Arc::new(standalone)))))
    }

    fn register_bitmap(&mut self, bitmap: Bitmap<'_>) -> Result<BitmapHandle, Error> {
        let _pt = PrimTimer::new(&PRIM_BMPUP_CUR);
        // Big-surface budget (Super Bowser World cinematic OOM, #56b follow-up).
        // Checked with the CHEAP dims accessor, BEFORE `bitmap_to_rgba_bytes`
        // materialises the (here 8.5 MB) pixel buffer — that transient Vec is the
        // exact allocation that failed at the crash. Past the budget we hand back
        // a zero-resource `DroppedBitmap`: the surface renders invisible but the
        // app keeps running (and stays diagnosable) instead of aborting.
        let (dw, dh) = (bitmap.width(), bitmap.height());
        if is_big_surface(dw, dh) {
            let want = dw as u64 * dh as u64 * 4;
            if self.big_atlas_live_bytes.saturating_add(want) > BIG_ATLAS_BUDGET_BYTES {
                self.big_atlas_dropped_total = self.big_atlas_dropped_total.wrapping_add(1);
                if self.big_atlas_dropped_total <= 4 {
                    let msg = std::format!(
                        "register_bitmap: GPU OVER BUDGET, invisible {}x{} (live={}MB budget={}MB dropped={}) — collision unaffected\n",
                        dw, dh,
                        self.big_atlas_live_bytes / (1024 * 1024),
                        BIG_ATLAS_BUDGET_BYTES / (1024 * 1024),
                        self.big_atlas_dropped_total,
                    );
                    let mut b = msg.into_bytes();
                    b.push(0);
                    unsafe { ruffle_log_cstr(b.as_ptr() as *const _) };
                }
                return Ok(BitmapHandle(Arc::new(DroppedBitmap { width: dw, height: dh })));
            }
        }
        let Some((bytes, w, h)) = bitmap_to_rgba_bytes(&bitmap) else {
            return Err(Error::UnknownType);
        };
        // Small bitmaps stay atlas-backed: shape bitmap fills (see
        // `DrawKind::Bitmap`) look up by `as_switch_bitmap` which requires the
        // atlas variant, and packing many small bitmaps into one texture is
        // what keeps Tegra's texture count sane. Keeping ALL bitmaps standalone
        // broke the SMWF sky (a shape with a JPEG fill → no atlas variant) — so
        // the atlas path is the default.
        if let Some(meta) = self.pack_into_atlas(&bytes, w, h, false) {
            self.bitmaps_registered = self.bitmaps_registered.wrapping_add(1);
            return Ok(BitmapHandle(Arc::new(meta)));
        }
        // Too big for the 2048² atlas. Returning Err(TooLarge) here used to make
        // Ruffle's `BitmapRawDataWrapper::bitmap_handle` (which `.expect()`s a
        // handle) PANIC — haunt-the-house's 3400×1600 BitmapData.draw crashed
        // the app (panic → worker-thread TLS fault, see exception.cpp backtrace).
        // Give it a standalone GL texture instead (good up to GL_MAX ≈ 16384,
        // and FBO-attachable — exactly what BitmapData.draw wants), with the
        // pixels uploaded. As a shape FILL it'd fall back to solid (no atlas
        // variant), but it never crashes.
        //
        // No texture at all (GPU memory exhausted, or over GL_MAX): hand back
        // the same invisible `DroppedBitmap` as the big-surface budget above,
        // not `Err`. `BitmapData::bitmap_handle` in Ruffle `expect`s this call,
        // so an `Err` for one 32x38 sprite panicked Super Smash Flash 2 thirty
        // seconds into its first fight (2026-09-30), once the GPU had nothing
        // left. One sprite missing beats losing the session.
        let Some(standalone) = make_standalone_texture(w, h) else {
            return Ok(BitmapHandle(Arc::new(DroppedBitmap { width: w, height: h })));
        };
        let standalone = standalone.tagged(ST_BITMAPDATA);
        unsafe {
            glBindTexture(GL_TEXTURE_2D, standalone.texture);
            glPixelStorei(GL_UNPACK_ALIGNMENT, 1);
            glTexSubImage2D(
                GL_TEXTURE_2D, 0, 0, 0,
                w as GLsizei, h as GLsizei,
                GL_RGBA, GL_UNSIGNED_BYTE, bytes.as_ptr() as *const _,
            );
            glBindTexture(GL_TEXTURE_2D, 0);
        }
        // Also reached when there was no GPU memory for a new atlas: a texture of
        // the bitmap's own size is far more likely to fit than 16 MB of atlas.
        self.warn_once(b"register_bitmap: >2048 bitmap or no room for an atlas -> standalone texture (no crash)\n\0");
        self.bitmaps_registered = self.bitmaps_registered.wrapping_add(1);
        Ok(BitmapHandle(Arc::new(StandaloneBitmap(Arc::new(standalone)))))
    }

    fn register_bitmap_reloadable(
        &mut self,
        bitmap: Bitmap<'_>,
        reload: std::rc::Rc<dyn Fn() -> Option<Bitmap<'static>>>,
    ) -> Result<BitmapHandle, Error> {
        let (dw, dh) = (bitmap.width(), bitmap.height());
        // Bigger than an atlas: the standalone path, kept for ever as before.
        if !self.evict_on || dw == 0 || dh == 0 || dw > ATLAS_SIZE || dh > ATLAS_SIZE {
            return self.register_bitmap(bitmap);
        }
        // A big one still answers to the big-surface budget (register_bitmap
        // hands back an invisible DroppedBitmap past it).
        if is_big_surface(dw, dh)
            && self.big_atlas_live_bytes.saturating_add(dw as u64 * dh as u64 * 4)
                > BIG_ATLAS_BUDGET_BYTES
        {
            return self.register_bitmap(bitmap);
        }
        let Some((bytes, w, h)) = bitmap_to_rgba_bytes(&bitmap) else {
            return Err(Error::UnknownType);
        };
        let slot = std::rc::Rc::new(ReloadSlot {
            place: Cell::new(None),
            ticket: core::cell::RefCell::new(None),
            width: w,
            height: h,
            last_draw: Cell::new(self.frame_count.max(1)),
            failed_at: Cell::new(0),
            reload,
        });
        {
            let _pt = PrimTimer::new(&PRIM_BMPUP_CUR);
            if !self.pack_reloadable(&bytes, w, h, &slot) {
                drop(bytes);
                return self.register_bitmap(bitmap);
            }
        }
        self.bitmaps_registered = self.bitmaps_registered.wrapping_add(1);
        Ok(BitmapHandle(Arc::new(ReloadableBitmap(slot))))
    }

    fn update_texture(
        &mut self,
        handle: &BitmapHandle,
        bitmap: Bitmap<'_>,
        region: PixelRegion,
    ) -> Result<(), Error> {
        let _pt = PrimTimer::new(&PRIM_BMPUP_CUR);
        // Budget-dropped big surface: no texture to update, silently succeed.
        if as_dropped_bitmap(handle).is_some() {
            return Ok(());
        }
        let rgba = bitmap.to_rgba();
        let w = region.x_max.saturating_sub(region.x_min);
        let h = region.y_max.saturating_sub(region.y_min);
        if w == 0 || h == 0 {
            return Ok(());
        }
        // Standalone texture: hold the sub-region for its GL texture.
        if let Some(standalone) = as_standalone_bitmap(handle) {
            // The source `rgba` buffer has full-bitmap-width rows. When the
            // dirty `region` is narrower than the bitmap, each row must skip
            // `rgba.width()` px, not `w` — packing rows contiguously at width
            // `w` makes every row drift (diagonal shear / stripes, Icy Tower's
            // gauge and whole-frame skew on partial-width BitmapData updates).
            // The stride is normalised here instead of at upload time, so the
            // held copy is tightly packed and the flush stays trivial.
            self.hold_upload(
                standalone.0.texture, Some(standalone.0.clone()), 0,
                region.x_min, region.y_min, w, h,
                rgba.width(), region.x_min, region.y_min, rgba.data(),
            );
            return Ok(());
        }
        let Some(switch_bitmap) = as_switch_bitmap(handle) else {
            return Err(Error::UnknownHandle(handle.clone()));
        };
        let atlas = match self.atlases.get(switch_bitmap.atlas_index) {
            Some(a) => a,
            None => return Err(Error::UnknownHandle(handle.clone())),
        };
        // Compute the atlas-space pixel offset from the atlas' ACTUAL dims —
        // right-sized dedicated atlases aren't 2048² (#42 regression).
        let base_x = (switch_bitmap.u0 * atlas.width as f32).round() as u32;
        let base_y = (switch_bitmap.v0 * atlas.height as f32).round() as u32;
        let atlas_index = switch_bitmap.atlas_index;
        self.hold_upload(
            0, None, atlas_index,
            base_x + region.x_min, base_y + region.y_min, w, h,
            rgba.width(), region.x_min, region.y_min, rgba.data(),
        );
        Ok(())
    }

    fn create_context3d(
        &mut self,
        profile: Context3DProfile,
    ) -> Result<Box<dyn Context3D>, Error> {
        // Stage3D, issue #88. See `backend/context3d.rs` for what the subset
        // covers; a game that needs more than it gets a visibly wrong picture
        // rather than the loading screen it used to sit on for ever.
        log(b"context3d: creating a Stage3D context (GL subset)\n\0");
        Ok(Box::new(crate::backend::context3d::SwitchContext3D::new(
            profile,
        )))
    }

    fn debug_info(&self) -> Cow<'static, str> {
        Cow::Borrowed(
            "Renderer: SwitchRenderBackend (phase 1.3 — shapes, bitmaps, lines, gradients, masks)",
        )
    }

    fn name(&self) -> &'static str {
        "switch-mesa-gl"
    }

    fn set_quality(&mut self, _quality: StageQuality) {}

    fn compile_pixelbender_shader(
        &mut self,
        shader: PixelBenderShader,
    ) -> Result<PixelBenderShaderHandle, Error> {
        // FlashNX: see NoopPixelBenderShader. We can't GL-compile PixelBender, but
        // we hold the parsed shader so AVM2 construction succeeds; execution
        // (run_pixelbender_shader) still errs and the renderer skips ShaderFilter.
        Ok(PixelBenderShaderHandle(std::sync::Arc::new(
            NoopPixelBenderShader { shader },
        )))
    }

    fn run_pixelbender_shader(
        &mut self,
        _handle: PixelBenderShaderHandle,
        _arguments: &[PixelBenderShaderArgument],
        _target: &PixelBenderTarget,
    ) -> Result<PixelBenderOutput, Error> {
        Err(Error::Unimplemented("Pixel bender shader".into()))
    }

    fn resolve_sync_handle(
        &mut self,
        handle: Box<dyn SyncHandle>,
        with_rgba: RgbaBufRead,
    ) -> Result<(), Error> {
        let _pt = PrimTimer::new(&PRIM_RESOLVE_CUR);
        // Reading pixels back counts as reading a texture.
        self.flush_pending_upload();
        // The only sync handles we produce are `BitmapDataSyncHandle` (from
        // BitmapData.draw()). Read the rendered dirty region back from its temp
        // texture (PREMULTIPLIED — Ruffle's BitmapData CPU pixels are
        // premultiplied, matching wgpu's raw GPU readback) and hand it to
        // Ruffle's copy closure.
        let sh = Box::<dyn Any>::downcast::<BitmapDataSyncHandle>(handle)
            .map_err(|_| Error::Unimplemented("resolve_sync_handle: unknown handle".into()))?;
        self.resolve_sync_calls = self.resolve_sync_calls.wrapping_add(1);
        let (rw, rh) = (sh.w, sh.h);
        if rw == 0 || rh == 0 {
            return Ok(());
        }
        // A dead texture (freed atlas / standalone) reads as id 0 — never
        // FBO-attach that, Mesa dereferences a null renderbuffer surface and the
        // app takes a DataAbort. Ruffle just keeps its existing CPU pixels.
        if sh.texture == 0 {
            return Ok(());
        }
        let buf = if sh.premult {
            // Atlas source: STRAIGHT alpha, and Ruffle wants PREMULTIPLIED. Convert
            // on the GPU with the same blit that seeds an offscreen composite,
            // rather than by hand on the CPU buffer — hand-rolled alpha maths on
            // this path is what produced the offroaders speckle. The scratch is
            // used and handed straight back, never given to Ruffle, so unlike the
            // old code no pooled temp can outlive this call.
            let Some(scratch) = self.acquire_offscreen_temp(rw, rh) else {
                return Ok(());
            };
            let scratch_id = scratch.texture;
            let ok = self.blit_premult(
                sh.texture, sh.tex_w, sh.tex_h, (sh.x, sh.y), (rw, rh),
                scratch_id, (0, 0), rw, rh,
            );
            let buf = if ok {
                self.readback_region_straight(scratch_id, 0, 0, rw, rh)
            } else {
                std::vec![0u8; (rw as usize) * (rh as usize) * 4]
            };
            self.offscreen_temp_retired.push(scratch);
            if !ok {
                return Ok(());
            }
            buf
        } else {
            self.readback_region_straight(sh.texture, sh.x, sh.y, rw, rh)
        };
        with_rgba(&buf, rw * 4);
        Ok(())
    }
}

// ─── CommandHandler ───────────────────────────────────────────────────────────

impl CommandHandler for SwitchRenderBackend {
    fn render_bitmap(
        &mut self,
        bitmap: BitmapHandle,
        transform: Transform,
        smoothing: bool,
        pixel_snapping: PixelSnapping,
    ) {
        if self.mask.writing {
            self.mask_shape_draw_window = self.mask_shape_draw_window.saturating_add(1);
        } else if self.mask.depth > 0 {
            self.masked_draw_window = self.masked_draw_window.saturating_add(1);
        }
        // Budget-dropped big surface (#56b OOM guard): no texture, draw nothing.
        if as_dropped_bitmap(&bitmap).is_some() {
            return;
        }
        // Standalone (FBO-backed) variant: own GL texture, full [0,1]² UV.
        // Used to draw cacheAsBitmap / filter / BitmapData results back onto
        // the stage.
        if let Some(standalone) = as_standalone_bitmap(&bitmap) {
            let tex = standalone.0.texture;
            let w = standalone.0.width as f32;
            let h = standalone.0.height as f32;
            let mut m = transform.matrix;
            pixel_snapping.apply(&mut m);
            let scaled = Matrix {
                a: m.a * w,
                b: m.b * w,
                c: m.c * h,
                d: m.d * h,
                tx: m.tx,
                ty: m.ty,
            };
            self.note_draw_extent(&scaled);
            let world = self.world_matrix(&scaled);
            let mult = transform.color_transform.mult_rgba_normalized();
            let add = transform.color_transform.add_rgba_normalized();
            let uv_remap = [0.0, 0.0, 1.0, 1.0];
            self.bitmap_render_count = self.bitmap_render_count.wrapping_add(1);
            // A cacheAsBitmap or filter result is still a Bitmap as far as the
            // movie is concerned, and Flash filtered it by the same flag, so it
            // goes through `sampler_for` like any other bitmap. It needs no UV
            // inset: it owns its texture and CLAMP_TO_EDGE already keeps a
            // NEAREST sample inside it.
            // `premult = true`: this texture holds premultiplied alpha, so the
            // colour transform has to be unmultiplied around (and skipped
            // entirely on fully transparent texels). Without that, `u_add` lands
            // on the whole quad — the grey rectangle around Mario 63's meteors.
            // See the `BITMAP_FRAG` doc comment.
            self.use_bitmap(
                &world, &mult, &add, tex, &uv_remap, self.sampler_for(smoothing), true,
            );
            self.gl_state.bind_vao(self.bitmap_vao);
            self.draw_calls_this_window = self.draw_calls_this_window.saturating_add(1);
            // Standalone cache textures store PREMULTIPLIED alpha (the offscreen
            // render uses `glBlendFuncSeparate(ONE, ONE_MINUS_SRC_ALPHA)` for the
            // alpha channel + the glow shader outputs `color * alpha`). The
            // straight-alpha blend used for atlas bitmaps multiplies alpha a
            // second time, producing alpha² output — too faint for filter
            // results like DropShadow. Switch to premultiplied "over" blend
            // for the standalone draw, then restore.
            //
            // "Restore" has to mean whatever the ENCLOSING pass was using, not
            // an unconditional straight-alpha blend. Inside an offscreen pass
            // (`render_commands_to_texture`) the enclosing blend is the SEPARATE
            // one that accumulates alpha additively, and putting plain
            // `glBlendFunc` back there reverted the alpha channel to `a²` for
            // every remaining draw of that pass.
            //
            // Reached whenever a cached object is drawn INSIDE another cache
            // entry's content pass, i.e. a filtered object under a filtered
            // ancestor. Found while chasing the grey rectangle rather than from
            // a report, so no game is named here: Mario 63 has 47 such nested
            // placements, but in 5-1FrontGFX and 5-9BackGFX, NOT in the 8-13
            // backdrop this fix came from. Correct on its own terms, untested
            // against a specific symptom.
            let restore_separate = self.offscreen_dims.is_some();
            unsafe {
                glBlendFunc(GL_ONE, GL_ONE_MINUS_SRC_ALPHA);
                glDrawArrays(GL_TRIANGLES, 0, 6);
                if restore_separate {
                    glBlendFuncSeparate(
                        GL_SRC_ALPHA, GL_ONE_MINUS_SRC_ALPHA,
                        GL_ONE, GL_ONE_MINUS_SRC_ALPHA,
                    );
                } else {
                    glBlendFunc(GL_SRC_ALPHA, GL_ONE_MINUS_SRC_ALPHA);
                }
            }
            return;
        }
        let Some(switch_bitmap) = as_switch_bitmap(&bitmap) else {
            self.warn_once(b"cmd: render_bitmap with non-Switch BitmapHandle\n\0");
            return;
        };
        let Some(atlas) = self.atlases.get(switch_bitmap.atlas_index) else {
            return;
        };
        atlas.last_draw_frame.set(self.frame_count.max(1));
        let tex = atlas.texture;
        let mut m = transform.matrix;
        pixel_snapping.apply(&mut m);
        let w = switch_bitmap.width as f32;
        let h = switch_bitmap.height as f32;
        let scaled = Matrix {
            a: m.a * w,
            b: m.b * w,
            c: m.c * h,
            d: m.d * h,
            tx: m.tx,
            ty: m.ty,
        };
        self.note_draw_extent(&scaled);
        let world = self.world_matrix(&scaled);
        let mult = transform.color_transform.mult_rgba_normalized();
        self.draw_max_alpha = self.draw_max_alpha.max(mult[3]);
        let add = transform.color_transform.add_rgba_normalized();
        let uv_remap = [
            switch_bitmap.u0,
            switch_bitmap.v0,
            switch_bitmap.u1 - switch_bitmap.u0,
            switch_bitmap.v1 - switch_bitmap.v0,
        ];
        self.bitmap_render_count = self.bitmap_render_count.wrapping_add(1);
        // No UV inset here, unlike the shape fill: `a_uv` comes straight off the
        // static unit quad and is only ever interpolated at covered sample
        // points, so it stays strictly inside (0,1) and a NEAREST sample lands in
        // [x, x+w-1] of the slot. The 1 px replicated ATLAS_PAD is what makes
        // that safe at the very edge, and it is the reason this path can be left
        // alone while the shape shader, whose clamp really does reach 1.0, cannot.
        // `premult = false`: atlas slots hold STRAIGHT alpha, so `c * mult + add`
        // is already the Flash formula here and the straight-alpha blend below
        // supplies the coverage. Nothing to undo.
        self.use_bitmap(
            &world, &mult, &add, tex, &uv_remap, self.sampler_for(smoothing), false,
        );
        self.gl_state.bind_vao(self.bitmap_vao);
        self.draw_calls_this_window = self.draw_calls_this_window.saturating_add(1);
        unsafe {
            glDrawArrays(GL_TRIANGLES, 0, 6);
        }
    }

    fn render_stage3d(&mut self, bitmap: BitmapHandle, transform: Transform) {
        // The Stage3D layer is its own command, separate from `render_bitmap`,
        // and this used to be a no-op left over from having no Context3D at all
        // (#88). With one, the movie rendered its whole scene into our back
        // buffer every frame and we dropped it on the floor: 17520 triangles a
        // second drawn, none of them shown, and a game whose 2D interface
        // appeared over an empty grey field.
        //
        // The back buffer is a plain standalone bitmap, so drawing it is drawing
        // a bitmap — no pixel snapping, since it is already at device resolution
        // and the stage transform places it.
        //
        // Smoothing is passed as TRUE, and that is now load-bearing rather than
        // decorative: `render_bitmap` used to throw the flag away, so this call
        // said `false` to mean "nothing to smooth". With AUTO honouring the flag,
        // `false` would point-sample an entire 3D scene.
        //
        // Flipped vertically on the way: a framebuffer writes with its origin at
        // the bottom left, while a texture uploaded from an image is stored top
        // row first, and every other texture we sample came from an upload. So
        // the 3D scene arrives upside down unless the quad is flipped here — in
        // the composite, where the flip belongs, rather than in the AGAL shaders,
        // where it would also invert triangle winding and break culling.
        let Some(standalone) = as_standalone_bitmap(&bitmap) else {
            self.warn_once(b"cmd: render_stage3d with an unexpected handle\n\0");
            return;
        };
        let h = standalone.0.height as f32;
        let m = transform.matrix;
        let flipped = Transform {
            matrix: Matrix {
                a: m.a,
                b: m.b,
                c: -m.c,
                d: -m.d,
                tx: m.tx + swf::Twips::from_pixels((m.c * h) as f64),
                ty: m.ty + swf::Twips::from_pixels((m.d * h) as f64),
            },
            color_transform: transform.color_transform,
            perspective_projection: transform.perspective_projection,
        };
        self.render_bitmap(bitmap, flipped, true, PixelSnapping::Never);
    }

    fn render_shape(&mut self, shape: ShapeHandle, transform: Transform) {
        let Some(switch_shape) = as_switch_shape(&shape) else {
            self.warn_once(b"cmd: render_shape with non-Switch ShapeHandle\n\0");
            return;
        };
        // Bail out on a non-finite transform: AS code occasionally produces
        // NaN scales/translations that would propagate into the shader and
        // crash the driver mid-sample.
        if !transform.matrix.a.is_finite()
            || !transform.matrix.b.is_finite()
            || !transform.matrix.c.is_finite()
            || !transform.matrix.d.is_finite()
        {
            return;
        }
        self.note_draw_extent(&transform.matrix);
        let world = self.world_matrix(&transform.matrix);
        if world.iter().any(|v| !v.is_finite()) {
            return;
        }
        let mult = transform.color_transform.mult_rgba_normalized();
        self.draw_max_alpha = self.draw_max_alpha.max(mult[3]);
        let add = transform.color_transform.add_rgba_normalized();
        // RELIABLE mask counters: only count once we're certain this shape
        // actually issues geometry (past all early-returns). `mask_shape` now
        // means "a mask shape that really draws into the stencil"; if it's ~0
        // while maskee draws are high, mask shapes produce no geometry.
        let ndraws = switch_shape.0.draws.len() as u32;
        if ndraws > 0 {
            if self.mask.writing {
                self.mask_shape_draw_window = self.mask_shape_draw_window.saturating_add(1);
            } else if self.mask.depth > 0 {
                self.masked_draw_window = self.masked_draw_window.saturating_add(1);
            }
        }
        for draw in &switch_shape.0.draws {
            match &draw.kind {
                DrawKind::Solid => {
                    self.use_solid(&world, &mult, &add);
                }
                DrawKind::Gradient {
                    texture_index,
                    local_matrix,
                    gradient_kind,
                    spread,
                    focal,
                } => {
                    let tex = switch_shape.0.gradient_textures[*texture_index];
                    self.use_gradient(
                        &world,
                        &mult,
                        &add,
                        tex,
                        local_matrix,
                        *gradient_kind,
                        *spread,
                        *focal,
                    );
                }
                DrawKind::Bitmap {
                    atlas_index,
                    uv_remap,
                    uv_inset,
                    local_matrix,
                    is_smoothed,
                    is_repeating,
                    standalone,
                    slot,
                } => {
                    if local_matrix.iter().any(|v| !v.is_finite()) {
                        continue;
                    }
                    let mut remap = *uv_remap;
                    // >2048 fill: sample its own texture. Otherwise the atlas.
                    let tex = if let Some(s) = standalone {
                        s.texture
                    } else if let Some(slot) = slot {
                        // Evictable image: wherever it is now, decoded again
                        // if it was evicted. Nothing drawn if that fails.
                        let Some(place) = self.ensure_resident(slot) else {
                            continue;
                        };
                        let Some(atlas) = self.atlases.get(place.atlas_index) else {
                            continue;
                        };
                        if atlas.texture == 0 {
                            continue;
                        }
                        atlas.last_draw_frame.set(self.frame_count.max(1));
                        remap = [place.u0, place.v0, place.u1 - place.u0, place.v1 - place.v0];
                        atlas.texture
                    } else {
                        let Some(atlas) = self.atlases.get(*atlas_index) else {
                            continue;
                        };
                        atlas.last_draw_frame.set(self.frame_count.max(1));
                        atlas.texture
                    };
                    self.bitmap_draws_emitted = self.bitmap_draws_emitted.wrapping_add(1);
                    self.use_shape_bitmap(
                        &world,
                        &mult,
                        &add,
                        tex,
                        local_matrix,
                        &remap,
                        uv_inset,
                        *is_repeating,
                        self.sampler_for(*is_smoothed),
                    );
                }
            }
            // Single VAO for all shape draws — it points at the arena VBO
            // and IBO. base_vertex shifts each fetched index by the byte
            // offset of this draw's vertices, expressed as a vertex count
            // (stride 24 bytes = 6 f32 per vertex).
            let stride_bytes = 6 * core::mem::size_of::<f32>() as GLintptr;
            let base_vertex = (draw.vbo_offset / stride_bytes) as GLint;
            self.gl_state.bind_vao(self.shape_vao);
            self.draw_calls_this_window = self.draw_calls_this_window.saturating_add(1);
            unsafe {
                glDrawElementsBaseVertex(
                    GL_TRIANGLES,
                    draw.num_indices,
                    GL_UNSIGNED_INT,
                    draw.ibo_offset as *const _,
                    base_vertex,
                );
            }
        }
    }

    fn render_alpha_mask(
        &mut self,
        maskee_commands: CommandList,
        mask_commands: CommandList,
    ) {
        // Soft alpha/luminance mask (the kind stencil masking can't express).
        // Render maskee + mask into two offscreen textures sized to the current
        // target, composite maskee × mask.alpha into a third, then draw that
        // back over the stage. All three textures share the "row 0 = Flash top"
        // offscreen layout, so the combine pass needs no Y handling; only the
        // final draw-back (proven standalone-bitmap path) flips for the main FB.
        self.alpha_mask_window = self.alpha_mask_window.saturating_add(1);
        // We have a single shared offscreen FBO; recursing into it (when this
        // mask is itself nested inside a cache entry / blend / outer mask)
        // would reset the outer target's color attachment mid-render. Degrade
        // to an inline unmasked draw in that case — the outer render stays
        // correct. The common top-level case (offscreen_dims == None) is fully
        // handled.
        if self.offscreen_dims.is_some() {
            maskee_commands.execute(self);
            return;
        }
        let (w, h) = self.current_target_dims();
        if w == 0 || h == 0 {
            maskee_commands.execute(self);
            return;
        }
        // Acquire all three textures up front so we can fall back to drawing the
        // maskee unmasked (better than vanishing) if the pool/GL is exhausted.
        let acquired = (
            self.filter_tex_pool.acquire(w, h),
            self.filter_tex_pool.acquire(w, h),
            self.filter_tex_pool.acquire(w, h),
        );
        let (maskee, mask, result) = match acquired {
            (Some(a), Some(b), Some(c)) => (a, b, c),
            (a, b, c) => {
                if let Some(t) = a { self.filter_tex_pool.release(t); }
                if let Some(t) = b { self.filter_tex_pool.release(t); }
                if let Some(t) = c { self.filter_tex_pool.release(t); }
                maskee_commands.execute(self);
                return;
            }
        };
        let transparent = Color { r: 0, g: 0, b: 0, a: 0 };
        let mk_ok = self.render_commands_to_texture(maskee.texture, w, h, maskee_commands, Some(transparent));
        let ms_ok = self.render_commands_to_texture(mask.texture, w, h, mask_commands, Some(transparent));
        if mk_ok && ms_ok
            && self.composite_alpha_mask(maskee.texture, mask.texture, result.texture, w, h)
        {
            self.draw_fullscreen_texture(result.texture, w, h, || unsafe {
                glBlendEquation(GL_FUNC_ADD);
                glBlendFunc(GL_ONE, GL_ONE_MINUS_SRC_ALPHA);
            });
        } else if mk_ok {
            // Composite failed but the maskee rendered — show it unmasked.
            self.draw_fullscreen_texture(maskee.texture, w, h, || unsafe {
                glBlendEquation(GL_FUNC_ADD);
                glBlendFunc(GL_ONE, GL_ONE_MINUS_SRC_ALPHA);
            });
        }
        self.filter_tex_pool.release(maskee);
        self.filter_tex_pool.release(mask);
        self.filter_tex_pool.release(result);
    }

    fn draw_rect(&mut self, color: Color, matrix: Matrix) {
        if self.mask.writing {
            self.mask_shape_draw_window = self.mask_shape_draw_window.saturating_add(1);
        }
        let r = color.r as f32 / 255.0;
        let g = color.g as f32 / 255.0;
        let b = color.b as f32 / 255.0;
        let a = color.a as f32 / 255.0;
        #[rustfmt::skip]
        let quad: [f32; 36] = [
            0.0, 0.0, r, g, b, a,
            1.0, 0.0, r, g, b, a,
            1.0, 1.0, r, g, b, a,
            0.0, 0.0, r, g, b, a,
            1.0, 1.0, r, g, b, a,
            0.0, 1.0, r, g, b, a,
        ];
        let world = self.world_matrix(&matrix);
        const IDENT_MULT: [f32; 4] = [1.0, 1.0, 1.0, 1.0];
        const IDENT_ADD: [f32; 4] = [0.0, 0.0, 0.0, 0.0];
        self.use_solid(&world, &IDENT_MULT, &IDENT_ADD);
        self.gl_state.bind_vao(self.rect_vao);
        self.draw_calls_this_window = self.draw_calls_this_window.saturating_add(1);
        unsafe {
            glBindBuffer(GL_ARRAY_BUFFER, self.rect_vbo);
            glBufferData(
                GL_ARRAY_BUFFER,
                core::mem::size_of_val(&quad) as GLsizeiptr,
                quad.as_ptr() as *const _,
                GL_DYNAMIC_DRAW,
            );
            glDrawArrays(GL_TRIANGLES, 0, 6);
        }
    }

    fn draw_line(&mut self, color: Color, matrix: Matrix) {
        let r = color.r as f32 / 255.0;
        let g = color.g as f32 / 255.0;
        let b = color.b as f32 / 255.0;
        let a = color.a as f32 / 255.0;
        #[rustfmt::skip]
        let line: [f32; 12] = [
            0.0, 0.0, r, g, b, a,
            1.0, 0.0, r, g, b, a,
        ];
        let world = self.world_matrix(&matrix);
        const IDENT_MULT: [f32; 4] = [1.0, 1.0, 1.0, 1.0];
        const IDENT_ADD: [f32; 4] = [0.0, 0.0, 0.0, 0.0];
        self.use_solid(&world, &IDENT_MULT, &IDENT_ADD);
        self.gl_state.bind_vao(self.line_vao);
        self.draw_calls_this_window = self.draw_calls_this_window.saturating_add(1);
        unsafe {
            glLineWidth(1.0);
            glBindBuffer(GL_ARRAY_BUFFER, self.line_vbo);
            glBufferData(
                GL_ARRAY_BUFFER,
                core::mem::size_of_val(&line) as GLsizeiptr,
                line.as_ptr() as *const _,
                GL_DYNAMIC_DRAW,
            );
            glDrawArrays(GL_LINES, 0, 2);
        }
    }

    fn draw_line_rect(&mut self, color: Color, matrix: Matrix) {
        let r = color.r as f32 / 255.0;
        let g = color.g as f32 / 255.0;
        let b = color.b as f32 / 255.0;
        let a = color.a as f32 / 255.0;
        #[rustfmt::skip]
        let lines: [f32; 48] = [
            0.0, 0.0, r, g, b, a,  1.0, 0.0, r, g, b, a,
            1.0, 0.0, r, g, b, a,  1.0, 1.0, r, g, b, a,
            1.0, 1.0, r, g, b, a,  0.0, 1.0, r, g, b, a,
            0.0, 1.0, r, g, b, a,  0.0, 0.0, r, g, b, a,
        ];
        let world = self.world_matrix(&matrix);
        const IDENT_MULT: [f32; 4] = [1.0, 1.0, 1.0, 1.0];
        const IDENT_ADD: [f32; 4] = [0.0, 0.0, 0.0, 0.0];
        self.use_solid(&world, &IDENT_MULT, &IDENT_ADD);
        self.gl_state.bind_vao(self.line_rect_vao);
        self.draw_calls_this_window = self.draw_calls_this_window.saturating_add(1);
        unsafe {
            glLineWidth(1.0);
            glBindBuffer(GL_ARRAY_BUFFER, self.line_rect_vbo);
            glBufferData(
                GL_ARRAY_BUFFER,
                core::mem::size_of_val(&lines) as GLsizeiptr,
                lines.as_ptr() as *const _,
                GL_DYNAMIC_DRAW,
            );
            glDrawArrays(GL_LINES, 0, 8);
        }
    }

    fn push_mask(&mut self) {
        self.mask_push();
    }
    fn activate_mask(&mut self) {
        self.mask_activate();
    }
    fn deactivate_mask(&mut self) {
        self.mask_deactivate();
    }
    fn pop_mask(&mut self) {
        self.mask_pop();
    }

    fn blend(&mut self, commands: CommandList, blend_mode: RenderBlendMode) {
        // Classify per wgpu's `BlendType`:
        //  - Normal/Layer  → just inline the group (source-over is the default
        //    blend, and drawing primitives sequentially is exactly the group's
        //    composite). No extra texture.
        //  - Add/Subtract/Screen ("trivial") → render the group into a temp,
        //    then draw it back with the matching GL blend state, so the group
        //    composites with the backdrop as a unit (no per-primitive double
        //    accumulation).
        //  - Multiply/Lighten/Darken/Difference/Invert/Overlay/HardLight
        //    ("complex") → snapshot the backdrop, render the group into a temp,
        //    then a shader composites the two straight onto the target.
        //  - Alpha/Erase need real layer tracking (group alpha vs the enclosing
        //    layer); Shader is PixelBender (unsupported). Fall back to inline.
        let mode = match blend_mode {
            RenderBlendMode::Builtin(m) => m,
            RenderBlendMode::Shader(_) => {
                commands.execute(self);
                return;
            }
        };

        // NOTE: the trivial Add/Subtract/Screen path below now runs even when
        // nested inside another offscreen render (a BitmapData.draw with a
        // blendMode, e.g. offroaders' boost trail: `screenBD.draw(carEffectBD,
        // m, ct, "add")`). It renders the group into a pooled temp, then
        // RE-ATTACHES the enclosing offscreen target (the temp render detaches
        // our shared FBO's colour attachment) and composites with the matching
        // GL blend state. Complex blends still degrade when nested — they need a
        // backdrop snapshot + a second offscreen target our single FBO can't
        // provide (guarded just before the complex path below).

        // 0..=6 must match the u_blend_mode switch in COMPLEX_BLEND_FRAG.
        let complex_mode: i32 = match mode {
            BlendMode::Multiply => 0,
            BlendMode::Lighten => 1,
            BlendMode::Darken => 2,
            BlendMode::Difference => 3,
            BlendMode::Invert => 4,
            BlendMode::Overlay => 5,
            BlendMode::HardLight => 6,
            // Non-complex modes handled below.
            BlendMode::Normal | BlendMode::Layer | BlendMode::Alpha | BlendMode::Erase => {
                commands.execute(self);
                return;
            }
            BlendMode::Add | BlendMode::Subtract | BlendMode::Screen => {
                let _bt = PrimTimer::new(&BLEND_TICKS_CUR);
                BLEND_N_TRIVIAL_CUR.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let (w, h) = self.current_target_dims();
                // Some(..) iff we're nested inside an enclosing offscreen render
                // (BitmapData.draw / cache entry). Captured BEFORE the temp
                // render below, which temporarily repoints these.
                let outer_target = self.offscreen_target_tex;
                let Some(temp) = (if w == 0 || h == 0 { None } else { self.filter_tex_pool.acquire(w, h) }) else {
                    commands.execute(self);
                    return;
                };
                let transparent = Color { r: 0, g: 0, b: 0, a: 0 };
                if self.render_commands_to_texture(temp.texture, w, h, commands, Some(transparent)) {
                    self.blend_window = self.blend_window.saturating_add(1);
                    // Nested: the temp render left our shared FBO's colour
                    // attachment detached. Re-attach the enclosing offscreen
                    // target + its viewport so the composite below lands on it
                    // (top-level leaves the main framebuffer bound — no-op).
                    if let Some(outer) = outer_target {
                        unsafe {
                            RT_BIND_CUR.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                            glBindFramebuffer(GL_FRAMEBUFFER, self.offscreen_fbo);
                            glFramebufferTexture2D(GL_FRAMEBUFFER, GL_COLOR_ATTACHMENT0, GL_TEXTURE_2D, outer, 0);
                            glViewport(0, 0, w as GLsizei, h as GLsizei);
                        }
                    }
                    let m = mode;
                    self.draw_fullscreen_texture(temp.texture, w, h, move || unsafe {
                        // Premultiplied group temp. Alpha channel always uses
                        // "over"; RGB uses the mode-specific factors/equation.
                        match m {
                            BlendMode::Add => {
                                glBlendEquationSeparate(GL_FUNC_ADD, GL_FUNC_ADD);
                                glBlendFuncSeparate(GL_ONE, GL_ONE, GL_ONE, GL_ONE_MINUS_SRC_ALPHA);
                            }
                            BlendMode::Subtract => {
                                glBlendEquationSeparate(GL_FUNC_REVERSE_SUBTRACT, GL_FUNC_ADD);
                                glBlendFuncSeparate(GL_ONE, GL_ONE, GL_ONE, GL_ONE_MINUS_SRC_ALPHA);
                            }
                            // Screen: out = src + dst*(1 - src).
                            _ => {
                                glBlendEquationSeparate(GL_FUNC_ADD, GL_FUNC_ADD);
                                glBlendFuncSeparate(GL_ONE, GL_ONE_MINUS_SRC_COLOR, GL_ONE, GL_ONE_MINUS_SRC_ALPHA);
                            }
                        }
                    });
                    // Nested: draw_fullscreen_texture reset the blend to the
                    // main-pass over-blend. Restore the offscreen accumulation
                    // blend (set by render_commands_to_texture) so the enclosing
                    // render's remaining draws keep compositing correctly, and
                    // invalidate cached GL state since we poked it raw.
                    if outer_target.is_some() {
                        unsafe {
                            glEnable(GL_BLEND);
                            glBlendEquationSeparate(GL_FUNC_ADD, GL_FUNC_ADD);
                            glBlendFuncSeparate(
                                GL_SRC_ALPHA, GL_ONE_MINUS_SRC_ALPHA,
                                GL_ONE, GL_ONE_MINUS_SRC_ALPHA,
                            );
                        }
                        self.gl_state.invalidate();
                    }
                }
                self.filter_tex_pool.release(temp);
                return;
            }
        };

        // Complex blends snapshot the backdrop into one temp and the group into
        // another, then composite both. Nested inside an offscreen render our
        // single shared FBO can't juggle the extra targets without corrupting
        // the enclosing target, so degrade to an inline (Normal) composite —
        // rare, and only for Multiply/Overlay/etc., never the trivial path above.
        if self.offscreen_dims.is_some() {
            commands.execute(self);
            return;
        }

        // Complex path: snapshot the backdrop + render the group, then composite.
        let _bt = PrimTimer::new(&BLEND_TICKS_CUR);
        BLEND_N_COMPLEX_CUR.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let (w, h) = self.current_target_dims();
        let flip = if self.offscreen_dims.is_some() { 0.0 } else { 1.0 };
        let parent = if w == 0 || h == 0 { None } else { self.filter_tex_pool.acquire(w, h) };
        let current = if w == 0 || h == 0 { None } else { self.filter_tex_pool.acquire(w, h) };
        let (parent, current) = match (parent, current) {
            (Some(p), Some(c)) => (p, c),
            (a, b) => {
                if let Some(t) = a { self.filter_tex_pool.release(t); }
                if let Some(t) = b { self.filter_tex_pool.release(t); }
                commands.execute(self);
                return;
            }
        };
        // Snapshot the current target's colour into `parent` (1:1, so it's
        // sampled straight regardless of target Y orientation). Reads from the
        // currently-bound framebuffer (the main FB here) into the texture bound
        // on the active unit, so pin the active unit to 0 first.
        unsafe {
            glActiveTexture(GL_TEXTURE0);
            glBindTexture(GL_TEXTURE_2D, parent.texture);
            glCopyTexSubImage2D(GL_TEXTURE_2D, 0, 0, 0, 0, 0, w as GLsizei, h as GLsizei);
            glBindTexture(GL_TEXTURE_2D, 0);
        }
        let transparent = Color { r: 0, g: 0, b: 0, a: 0 };
        if self.render_commands_to_texture(current.texture, w, h, commands, Some(transparent)) {
            self.blend_window = self.blend_window.saturating_add(1);
            // 7 falls through the shader's `return s;` = a plain Normal composite;
            // 8 paints the group's alpha in red (see the flags above).
            // 7 falls through the shader's `return s;` = a plain Normal composite.
            let m = if FORCE_NORMAL_COMPLEX_BLEND { 7 } else { complex_mode };
            self.composite_complex_to_current(parent.texture, current.texture, w, h, m, flip);
        }
        self.filter_tex_pool.release(parent);
        self.filter_tex_pool.release(current);
    }
}

impl Drop for SwitchRenderBackend {
    fn drop(&mut self) {
        unsafe {
            glDeleteBuffers(1, &self.rect_vbo);
            glDeleteVertexArrays(1, &self.rect_vao);
            glDeleteBuffers(1, &self.bitmap_vbo);
            glDeleteVertexArrays(1, &self.bitmap_vao);
            glDeleteBuffers(1, &self.atlas_vbo);
            glDeleteVertexArrays(1, &self.atlas_vao);
            glDeleteBuffers(1, &self.line_vbo);
            glDeleteVertexArrays(1, &self.line_vao);
            glDeleteBuffers(1, &self.line_rect_vbo);
            glDeleteVertexArrays(1, &self.line_rect_vao);
            glDeleteVertexArrays(1, &self.shape_vao);
            if self.sampler_sharp != 0 {
                glDeleteSamplers(1, &self.sampler_sharp);
            }
            if self.offscreen_fbo != 0 {
                glDeleteFramebuffers(1, &self.offscreen_fbo);
            }
            if self.filter_fbo != 0 {
                glDeleteFramebuffers(1, &self.filter_fbo);
            }
            if self.offscreen_depth_stencil != 0 {
                glDeleteRenderbuffers(1, &self.offscreen_depth_stencil);
            }
            // vertex_arena / index_arena released via their Drop impls.
            // Programs freed by their respective Drop impls.
        }
    }
}
