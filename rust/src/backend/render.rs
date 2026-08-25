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
pub const SCREEN_ITEMS: &[&str] = &["AFFICHAGE", "ROTATION", "ZOOM", "FILTRE"];

// ── Unified modal style ────────────────────────────────────────────────────
// One look for every centered popup. Before this, each modal hard-coded its own
// width / height / font scales / dim alpha / colors, so they all drifted apart
// (#20). Now they share these constants and the `draw_modal_frame` helper, and
// only their *body* (rows, warnings, game name) differs. Panels size their
// HEIGHT to the row count; the WIDTH picks one of two tiers below.
//
// Two width tiers, on purpose — a single width made short pickers look bloated
// (a 4-item OPTIONS panel as wide as the screen). Long content (game names,
// profile titles) shrinks to fit the standard width instead of forcing every
// panel wide.
//
/// Standard picker width — pause, OPTIONS, sort, language, key dropdown, lists.
/// Matches the in-game pause panel, the reference Jonathan liked.
const MODAL_W: f32 = 520.0;
/// Wide panel — only where the body genuinely needs the room: the two-column
/// TOUCHES editor and the danger confirms (long warning / URL lines).
const MODAL_W_WIDE: f32 = 720.0;
/// Dim backdrop alpha (ARGB). The danger variant is darker to sell "stop".
const MODAL_DIM: u32 = 0xB0_00_00_00;
const MODAL_DIM_DANGER: u32 = 0xCC_00_00_00;
/// Panel fill + border — calm navy vs danger red.
const MODAL_BG: u32 = 0xF0_14_20_38;
const MODAL_BG_DANGER: u32 = 0xF0_40_10_18;
const MODAL_BORDER: u32 = 0xFFFFFF;
const MODAL_BORDER_DANGER: u32 = 0xFF6060;
/// Text scales — shared so font sizes never drift between modals again.
const MODAL_TITLE_SCALE: f32 = 3.0;
const MODAL_SUB_SCALE: f32 = 2.0;
const MODAL_ROW_SCALE: f32 = 2.5;
const MODAL_FOOTER_SCALE: f32 = 2.0;
/// Text colors.
const MODAL_TITLE_COL: u32 = 0xFFFFFF;
const MODAL_TITLE_COL_DANGER: u32 = 0xFFD740; // amber title on the red panels
const MODAL_SUB_COL: u32 = 0xAABFD8;
const MODAL_ROW_COL: u32 = 0xCCCCCC;
const MODAL_ROW_SEL_COL: u32 = 0xFFD740; // amber cursor row
const MODAL_FOOTER_COL: u32 = 0x99AABB;
/// Vertical metrics. A modal is PAD_TOP (title[+subtitle]) + rows*ROW_H +
/// PAD_BOTTOM (footer). Fixed-height confirm modals override the total.
const MODAL_ROW_H: f32 = 52.0;
/// Space above the first row WITH a subtitle (title + subtitle band) vs WITHOUT
/// (title only) — the tight one drops the empty subtitle gap so a no-subtitle
/// modal (e.g. the language picker) doesn't waste ~50 px under its title.
const MODAL_PAD_TOP: f32 = 140.0;
const MODAL_PAD_TOP_TIGHT: f32 = 90.0;
const MODAL_PAD_BOTTOM: f32 = 60.0;
/// Row layout: text left padding from the panel edge, and how far left the
/// ">" cursor sits from that text.
const MODAL_ROW_X: f32 = 80.0;
const MODAL_CURSOR_DX: f32 = 30.0;

/// Glide slots for the lists that draw their own rows instead of going through
/// `draw_modal_rows`.
const GLIDE_KEY_LANG: u32 = 50;
const GLIDE_KEY_BUG: u32 = 51;
const GLIDE_KEY_KEYS: u32 = 53;
const GLIDE_KEY_COVER: u32 = 55;
// 52 and 54 used to be the folder picker's; it derives its own keys from the
// path it is showing, so a step into another folder snaps instead of sliding.

/// Geometry of a drawn modal frame, returned by `draw_modal_frame` so the caller
/// can lay its body (rows or free text) inside the shared chrome.
#[derive(Clone, Copy)]
struct ModalFrame {
    x: f32,
    y: f32,
    w: f32,
    /// Distance from `y` to the first body row — `MODAL_PAD_TOP` with a subtitle,
    /// `MODAL_PAD_TOP_TIGHT` without (no empty subtitle gap).
    pad_top: f32,
}

impl ModalFrame {
    /// Y of the first body row (just below the title/subtitle band).
    fn rows_top(&self) -> f32 {
        self.y + self.pad_top
    }
    /// Left edge of row text.
    fn rows_left(&self) -> f32 {
        self.x + MODAL_ROW_X
    }
    /// Horizontal space available for a row's text before the right edge.
    fn rows_avail(&self) -> f32 {
        self.w - MODAL_ROW_X * 1.5
    }
}

/// Truncate `s` to at most `max_chars` characters, appending "…" when cut.
fn truncate_tail(s: &str, max_chars: usize) -> std::string::String {
    if s.chars().count() > max_chars && max_chars > 1 {
        let mut t: std::string::String = s.chars().take(max_chars - 1).collect();
        t.push('…');
        t
    } else {
        s.to_string()
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
/// Which grid tile is opening its cover, which one is closing, and when the
/// changeover started. `(selection, previous, tick)`.
///
/// The obvious implementation reads the animated selection FRAME and opens
/// whatever tile it is near. That is wrong in two dimensions: on a diagonal the
/// frame flies past the two tiles that share a row or a column with the ends,
/// and they blink open as it goes by. The player sees the cursor "touch" games
/// it never selected. Only two tiles are ever involved in a move, so name them.
static GRID_COVER_ANIM: Mutex<(usize, usize, u64)> = Mutex::new((usize::MAX, usize::MAX, 0));

/// How long a grid cover takes to open or close, in milliseconds. Matched to
/// the selection frame's own travel so the art and the cursor settle together.
const GRID_COVER_MS: u64 = 190;

/// `(opening, closing, t)` for this frame: which tile is revealing its art,
/// which is folding back, and how far along. `t` runs 0..1 and is smoothstepped
/// by the caller.
fn grid_cover_phase(selection: usize) -> (usize, usize, f32) {
    let now = unsafe { ruffle_tick_now() };
    let freq = unsafe { ruffle_tick_freq() }.max(1);
    let Ok(mut g) = GRID_COVER_ANIM.lock() else {
        return (selection, usize::MAX, 1.0);
    };
    if g.0 != selection {
        // First ever draw opens instantly rather than animating from nothing.
        let prev = if g.0 == usize::MAX { usize::MAX } else { g.0 };
        *g = (selection, prev, now);
    }
    let elapsed_ms = now.saturating_sub(g.2) * 1000 / freq;
    let t = (elapsed_ms as f32 / GRID_COVER_MS as f32).clamp(0.0, 1.0);
    (g.0, g.1, t)
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
    if bitmap_glyph(ch).is_some() {
        6.0 * scale
    } else if (ch as u32) >= 0x80 {
        CJK_ADVANCE_UNITS * scale
    } else {
        // Unknown ASCII: drawn blank, but it still advances the pen.
        6.0 * scale
    }
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

/// Punctuation that may not start a line: it belongs to the character before
/// it. Without this a Chinese title breaks right before its comma.
fn cjk_no_line_start(c: char) -> bool {
    matches!(c,
        '\u{3001}' | '\u{3002}' | '\u{FF0C}' | '\u{FF0E}' | '\u{FF01}' | '\u{FF1F}'
        | '\u{FF1A}' | '\u{FF1B}' | '\u{FF09}' | '\u{FF3D}' | '\u{FF5D}' | '\u{300D}'
        | '\u{300F}' | '\u{3011}' | '\u{3015}' | '\u{2019}' | '\u{201D}'
        | ',' | '.' | '!' | '?' | ':' | ';' | ')' | ']' | '}'
    )
}

/// The mirror image: an opening bracket may not end a line.
fn cjk_no_line_end(c: char) -> bool {
    matches!(c,
        '\u{FF08}' | '\u{FF3B}' | '\u{FF5B}' | '\u{300C}' | '\u{300E}' | '\u{3010}'
        | '\u{3014}' | '\u{2018}' | '\u{201C}'
        | '(' | '[' | '{'
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

const FACTS_VALUE: u32 = 0xAABFD8;
const FACTS_MUTED: u32 = 0x7A8CA6;

// Scrollbar palette, one copy for the seven places that spelled it out inline.
// They drifted exactly where an inline copy always drifts: the Flashpoint
// gallery's track came out white where every other one is slate, and three of
// the seven clamp their thumb 4 px shorter than the rest. Differences nobody
// decided, only inherited from whichever site happened to be open that day. The
// thumb is the selection amber on purpose: on every screen it is the cursor,
// seen from the edge of the page.
const SCROLLBAR_W: f32 = 4.0;
const SCROLLBAR_MIN_THUMB: f32 = 24.0;
const SCROLLBAR_TRACK: u32 = 0x40_99_AA_BB;
const SCROLLBAR_THUMB: u32 = 0xFF_FF_D7_40;


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
        }
    }

    /// Allocate `size` bytes (rounded up to `self.align`). First-fit. Returns
    /// the byte offset, or `None` if the arena is full.
    fn alloc(&mut self, size: GLsizeiptr) -> Option<GLintptr> {
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

    fn upload(&self, offset: GLintptr, data: &[u8]) {
        unsafe {
            glBindBuffer(self.target, self.gl_id);
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
static PENDING_ATLAS_RELEASE: Mutex<Vec<usize>> = Mutex::new(Vec::new());

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
}
impl Drop for AtlasTicket {
    fn drop(&mut self) {
        if let Ok(mut q) = PENDING_ATLAS_RELEASE.lock() {
            q.push(self.atlas_index);
        }
    }
}

use swf::{BlendMode, Color, GradientSpread};

use crate::ffi::gl::*;
use crate::query_ram;

extern "C" {
    fn ruffle_log_cstr(msg: *const core::ffi::c_char);
    /// Monotonic tick counter (armGetSystemTick). Used for FPS heartbeat.
    fn ruffle_tick_now() -> u64;
    /// Tick frequency in Hz (~19.2 MHz on Switch). Constant after boot.
    fn ruffle_tick_freq() -> u64;
    /// Actual current CPU clock in Hz (clkrst). 0 if unavailable. Lets the
    /// heartbeat confirm whether CpuBoostMode is holding the A57 at 1785 MHz.
    fn ruffle_cpu_clock_hz() -> u32;
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
static PRIM_OFF_PIX_LAST: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

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
}

impl Atlas {
    fn new(size: u32) -> Self {
        Self::new_wh(size, size)
    }

    /// Allocate a `width × height` atlas texture. Shared atlases are square
    /// (`ATLAS_SIZE²`); a bitmap too large to share gets a right-sized dedicated
    /// atlas (issue #56b) so a 1824×1174 surface costs ~8.5 MB, not a full 16 MB
    /// 2048² — halving the memory of games that spam big offscreen surfaces.
    fn new_wh(width: u32, height: u32) -> Self {
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
                core::ptr::null(),
            );
            glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, GL_LINEAR as GLint);
            glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER, GL_LINEAR as GLint);
            glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_WRAP_S, GL_CLAMP_TO_EDGE as GLint);
            glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_WRAP_T, GL_CLAMP_TO_EDGE as GLint);
            glBindTexture(GL_TEXTURE_2D, 0);
        }
        Self {
            texture: tex,
            width,
            height,
            shelves: Vec::new(),
            live: 0,
        }
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
    }

    /// Try to allocate a `w×h` region (plus padding). Returns the content
    /// origin (without padding).
    fn pack(&mut self, w: u32, h: u32) -> Option<(u32, u32)> {
        let w_full = w + 2 * ATLAS_PAD;
        let h_full = h + 2 * ATLAS_PAD;
        if w_full > self.width || h_full > self.height {
            return None;
        }
        for shelf in &mut self.shelves {
            if shelf.height >= h_full && shelf.used_w + w_full <= self.width {
                let x = shelf.used_w + ATLAS_PAD;
                let y = shelf.y + ATLAS_PAD;
                shelf.used_w += w_full;
                return Some((x, y));
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
        Some((ATLAS_PAD, next_y + ATLAS_PAD))
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
}

impl Drop for StandaloneTexture {
    fn drop(&mut self) {
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
    BitmapHandle(Arc::new(StandaloneBitmap(Arc::new(StandaloneTexture {
        texture,
        width,
        height,
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
            ruffle_log_cstr(
                b"make_standalone_texture: glTexImage2D failed (GPU OOM?), skipped\n\0".as_ptr()
                    as *const _,
            );
            return None;
        }
        glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, GL_LINEAR as GLint);
        glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER, GL_LINEAR as GLint);
        glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_WRAP_S, GL_CLAMP_TO_EDGE as GLint);
        glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_WRAP_T, GL_CLAMP_TO_EDGE as GLint);
        glBindTexture(GL_TEXTURE_2D, 0);
    }
    Some(StandaloneTexture { texture: tex, width, height })
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
        /// 3x3 column-major matrix mapping `a_pos` (shape pixels) to UV
        /// in [0, 1] of the source bitmap. Pre-inverted by
        /// `swf_bitmap_to_gl_matrix`.
        local_matrix: [GLfloat; 9],
        #[allow(dead_code)]
        is_smoothed: bool,
        is_repeating: bool,
        /// Set for fills whose source bitmap is too big for the 2048² atlas
        /// (e.g. Mario Combat's >2048 sky/floor): the standalone GL texture to
        /// sample instead of `atlas_index`. Holds the `Arc` so the texture
        /// outlives this draw (its `Drop` deletes the GL texture). Without this
        /// the fill fell back to `Solid` and rendered as a white block.
        standalone: Option<Arc<StandaloneTexture>>,
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
    last_vao: Cell<GLuint>,
}

impl GlStateCache {
    /// Forget what we cached. Call at submit_frame start (after we know
    /// any external glXxx calls have potentially mutated state) and at end
    /// (where we reset GL to zero anyway).
    fn invalidate(&self) {
        self.last_program.set(0);
        self.last_texture.set(0);
        self.last_wrap_mode.set(-1);
        self.last_vao.set(0);
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
        self.current_frame = frame;
        let keep_from = frame.saturating_sub(FILTER_POOL_TTL_FRAMES - 1);
        for bucket in self.buckets.values_mut() {
            let before = bucket.len();
            bucket.retain(|(_, f)| *f >= keep_from); // dropped entries free their GL texture
            self.pooled -= before - bucket.len();
        }
        self.buckets.retain(|_, v| !v.is_empty());
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

const BITMAP_FRAG: &[u8] = b"#version 330 core\n\
in vec2 v_uv;\n\
out vec4 frag_color;\n\
uniform sampler2D u_tex;\n\
uniform vec4 u_mult;\n\
uniform vec4 u_add;\n\
void main() {\n\
    vec4 c = texture(u_tex, v_uv);\n\
    frag_color = clamp(c * u_mult + u_add, 0.0, 1.0);\n\
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
uniform int u_wrap_mode;\n\
void main() {\n\
    vec2 local = (u_wrap_mode == 1) ? fract(v_uv_local) : clamp(v_uv_local, 0.0, 1.0);\n\
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

pub fn screen_filter() -> u8 {
    SCREEN_FILTER.load(std::sync::atomic::Ordering::Relaxed)
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
            match (bitmap_meta, standalone) {
                // Common case: the fill bitmap is atlas-packed.
                (Some(meta), _) => DrawKind::Bitmap {
                    atlas_index: meta.atlas_index,
                    uv_remap: [meta.u0, meta.v0, meta.u1 - meta.u0, meta.v1 - meta.v0],
                    local_matrix,
                    is_smoothed: b.is_smoothed,
                    is_repeating: b.is_repeating,
                    standalone: None,
                },
                // >2048 fill: sample its standalone texture directly (full UV).
                (None, Some(tex)) => DrawKind::Bitmap {
                    atlas_index: 0,
                    uv_remap: [0.0, 0.0, 1.0, 1.0],
                    local_matrix,
                    is_smoothed: b.is_smoothed,
                    is_repeating: b.is_repeating,
                    standalone: Some(tex),
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

/// Cached cover texture for a library game (v1.2.0 JOUER grid). Looked up by
/// `.swf` basename; a cover is decoded + uploaded once on first display and
/// kept for the backend's lifetime (the GL context outlives the library UI).
/// `Default` = no cover image found → the grid draws a generated tile.
#[derive(Clone, Copy)]
enum CoverTex {
    Image { tex: GLuint, w: u32, h: u32 },
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

/// Split `msg` into lines no wider than `max_w` at `scale`, breaking on spaces.
/// A single word too wide for a line (a URL, a curl error blob, a space-less
/// CJK run) is HARD-chopped instead of running off both edges of the screen.
///
/// Widths, not character counts. Every caller had a box in pixels and divided
/// it by 6 units a character to get one, which is only true of the bitmap font:
/// a shared-font glyph advances 8, so a Chinese message -- a Flashpoint title,
/// a name typed in at RENOMMER (issue #75) -- was wrapped for a box a third
/// narrower than the one it got drawn in, and overflowed it.
fn wrap_words(msg: &str, max_w: f32, scale: f32) -> std::vec::Vec<std::string::String> {
    // Floor at the old eight-character minimum so a degenerate width cannot
    // produce a line per character.
    let max = max_w.max(48.0 * scale);
    let space_w = char_advance(' ', scale);
    let word_w = |w: &str| -> f32 { w.chars().map(|c| char_advance(c, scale)).sum() };
    let mut lines: std::vec::Vec<std::string::String> = std::vec::Vec::new();
    let mut cur = std::string::String::new();
    let mut cur_w = 0.0f32;
    for word in msg.split_whitespace() {
        let ww = word_w(word);
        if ww > max {
            if !cur.is_empty() {
                lines.push(core::mem::take(&mut cur));
                cur_w = 0.0;
            }
            let mut chunk = std::string::String::new();
            let mut chunk_w = 0.0f32;
            for c in word.chars() {
                // Look before pushing: the line is closed when the NEXT
                // character would overflow, so a line always holds at least one
                // character and never exceeds the box.
                let cw = char_advance(c, scale);
                if !chunk.is_empty() && chunk_w + cw > max {
                    lines.push(core::mem::take(&mut chunk));
                    chunk_w = 0.0;
                }
                chunk.push(c);
                chunk_w += cw;
            }
            if !chunk.is_empty() {
                cur_w = chunk_w;
                cur = chunk;
            }
            continue;
        }
        if cur.is_empty() {
            cur.push_str(word);
            cur_w = ww;
        } else if cur_w + space_w + ww <= max {
            cur.push(' ');
            cur.push_str(word);
            cur_w += space_w + ww;
        } else {
            lines.push(core::mem::take(&mut cur));
            cur.push_str(word);
            cur_w = ww;
        }
    }
    if !cur.is_empty() {
        lines.push(cur);
    }
    lines
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
fn marker_present(name: &str) -> bool {
    std::path::Path::new(&std::format!("sdmc:/switch/FlashNX/{}", name)).exists()
}

impl SwitchRenderBackend {
    /// Full backend for a game: the mega-arenas are sized for the worst SWF we
    /// know of (see the BufferArena block at the top of this file).
    pub fn new(width: u32, height: u32) -> Option<Self> {
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
            (ARENA_VBO_SIZE, ARENA_IBO_SIZE)
        };
        Self::new_sized(width, height, vbo, ibo)
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
            offscreen_temp_pool: Vec::new(),
            offscreen_temp_retired: Vec::new(),
            offscreen_temp_pool_bytes: 0,
            gl_state: GlStateCache::default(),
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
            heartbeat_tick: 0,
            draw_calls_this_window: 0,
            push_mask_window: 0,
            alpha_mask_window: 0,
            masked_draw_window: 0,
            mask_shape_draw_window: 0,
            cache_entries_max_window: 0,
            render_offscreen_calls: 0,
            apply_filter_calls: 0,
            resolve_sync_calls: 0,
            filters_seen_mask: AtomicU16::new(0),
            bitmap_render_count: 0,
            atlases: Vec::new(),
            vertex_arena,
            index_arena,
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
        unsafe {
            glGetIntegerv(GL_FRAMEBUFFER_BINDING, &mut prev_fbo);
            glGetIntegerv(GL_VIEWPORT, prev_vp.as_mut_ptr());
            RT_BIND_CUR.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            glBindFramebuffer(GL_FRAMEBUFFER, self.offscreen_fbo);
            glFramebufferTexture2D(GL_FRAMEBUFFER, GL_COLOR_ATTACHMENT0, GL_TEXTURE_2D, tex, 0);
        }
        self.ensure_offscreen_depth_stencil(tex_w, tex_h);
        unsafe {
            let status = glCheckFramebufferStatus(GL_FRAMEBUFFER);
            if status != GL_FRAMEBUFFER_COMPLETE {
                let msg = std::format!("offscreen: FBO incomplete 0x{:04X} ({}x{})\n", status, tex_w, tex_h);
                let mut b = msg.into_bytes();
                b.push(0);
                ruffle_log_cstr(b.as_ptr() as *const _);
                glBindFramebuffer(GL_FRAMEBUFFER, prev_fbo as GLuint);
                return false;
            }
            glViewport(0, 0, tex_w as GLsizei, tex_h as GLsizei);
            glClearStencil(0);
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
            glFramebufferTexture2D(GL_FRAMEBUFFER, GL_COLOR_ATTACHMENT0, GL_TEXTURE_2D, 0, 0);
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
        unsafe {
            glGetIntegerv(GL_FRAMEBUFFER_BINDING, &mut prev_fbo);
            glGetIntegerv(GL_VIEWPORT, prev_vp.as_mut_ptr());
            RT_BIND_CUR.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            glBindFramebuffer(GL_FRAMEBUFFER, self.filter_fbo);
            glFramebufferTexture2D(GL_FRAMEBUFFER, GL_COLOR_ATTACHMENT0, GL_TEXTURE_2D, dst_tex, 0);
        }
        unsafe {
            let status = glCheckFramebufferStatus(GL_FRAMEBUFFER);
            if status != GL_FRAMEBUFFER_COMPLETE {
                let msg = std::format!("filter pass: FBO incomplete 0x{:04X}\n", status);
                let mut b = msg.into_bytes();
                b.push(0);
                ruffle_log_cstr(b.as_ptr() as *const _);
                glBindFramebuffer(GL_FRAMEBUFFER, prev_fbo as GLuint);
                return false;
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
        setup_uniforms();
        unsafe {
            glBindVertexArray(self.bitmap_vao);
            glDrawArrays(GL_TRIANGLES, 0, 6);
            glBindVertexArray(0);
            glBindTexture(GL_TEXTURE_2D, 0);
            glEnable(GL_BLEND);
            glFramebufferTexture2D(GL_FRAMEBUFFER, GL_COLOR_ATTACHMENT0, GL_TEXTURE_2D, 0, 0);
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
        if let Some(i) = self
            .offscreen_temp_pool
            .iter()
            .position(|t| t.tex.width == w && t.tex.height == h)
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
                .position(|t| t.width == w && t.height == h)
            {
                return Some(self.offscreen_temp_retired.swap_remove(i));
            }
        }
        make_standalone_texture(w, h)
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
        self.use_bitmap(&world, &IDENT_MULT, &IDENT_ADD, tex, &uv_remap);
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

    fn use_bitmap(
        &self,
        world: &[GLfloat; 9],
        mult: &[f32; 4],
        add: &[f32; 4],
        tex: GLuint,
        uv_remap: &[f32; 4],
    ) {
        // Sampler binding (u_tex = 0) set once at program link; no per-draw
        // glUniform1i(u_tex) needed here.
        self.gl_state.use_program(self.bitmap_prog.program);
        self.gl_state.bind_texture_unit0(tex);
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
        is_repeating: bool,
    ) {
        // Atlas texture parameters are set once at atlas creation; no per-
        // draw glTexParameteri (avoids per-frame state churn that bisection
        // on 2026-05-24 implicated in a Mario 63 driver-side issue).
        // u_wrap_mode and u_tex sampler are routed through the GL state
        // cache so identical-state runs of draws (very common for atlas
        // bitmap fills) only hit the driver once.
        self.gl_state.use_program(self.shape_bitmap_prog.program);
        self.gl_state.bind_texture_unit0(tex);
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
            if let Some((x, y)) = atlas.pack(width, height) {
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
                    ticket: Some(Arc::new(AtlasTicket { atlas_index: idx })),
                });
            }
        }
        // No room in any shared atlas. A bitmap large in either axis gets its own
        // RIGHT-SIZED atlas (~the bitmap's bytes, not a full 16 MB 2048²) so games
        // that spam big offscreen surfaces use ~half the memory (issue #56b). It
        // stays atlas-backed (shape fills keep working — unlike the standalone
        // path). Small bitmaps still get a shared 2048² so more can pack into it.
        let big = width > ATLAS_SIZE / 2 || height > ATLAS_SIZE / 2;
        let mut atlas = if big {
            Atlas::new_wh(width + 2 * ATLAS_PAD, height + 2 * ATLAS_PAD)
        } else {
            Atlas::new(ATLAS_SIZE)
        };
        let Some((x, y)) = atlas.pack(width, height) else {
            return None;
        };
        atlas.upload_region_padded(x, y, width, height, pixels);
        atlas.live = 1;
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
        Some(SwitchBitmapHandle {
            atlas_index: new_atlas_index,
            u0: x as f32 / aw,
            v0: y as f32 / ah,
            u1: (x + width) as f32 / aw,
            v1: (y + height) as f32 / ah,
            width,
            height,
            ticket: Some(Arc::new(AtlasTicket { atlas_index: new_atlas_index })),
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
        let msg = std::format!(
            "SLOW f{} {}us (tick {}us render {}us) swf={} gc={} gcUs={} gcMB={} alloc={}/{}us({}%sm) free={}/{}us heap={}%  rtBind={} primOffs={}us primBmp={}us primRes={}us dc={} offs={} filt={}({}chains) resolve={} bmpUp={} shpReg={} blend={} pmask={} mdraw={} cacheEnt={} | offN={} offPix={} alloc={}us render={}us readback={}us upload={}us | blendUs={} ({}% of render) blendTriv={} blendCx={}\n",
            self.frame_count,
            total_us, tick_us, render_us,
            swf_frames, gc_name, gc_us, gc_alloc / (1024 * 1024), d_alloc, alloc_us, small_pct, d_free, free_us, heap_pct, rt_binds,
            prim_offs, prim_bmp, prim_res,
            fb.draw_calls, fb.offscreen, fb.filter, fb.filter_chains,
            fb.resolve, fb.bmp_uploads, fb.shape_regs,
            fb.blend, fb.pushmask, fb.masked_draw, fb.cache_entries,
            off_n, off_pix, off_alloc, off_render, off_readback, off_upload,
            blend_us, blend_pct, blend_n_triv, blend_n_cx,
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

    // (free helper for `draw_text`'s batching — see `push_text_quad` below)

    /// Draw a string. ASCII + accented Latin + Cyrillic come from the embedded
    /// 5x7 pixel font (see `GLYPHS`), folded to uppercase. Any other codepoint
    /// (CJK etc.) falls back to the shared-font glyph atlas (`draw_atlas_glyph`)
    /// as a full-width cell. `x`, `y` are top-left in screen pixels; each lit
    /// bitmap pixel becomes a `scale × scale` solid rect (same path as
    /// `draw_rect`). Unknown ASCII renders as blank space.
    pub fn draw_text(&mut self, x: f32, y: f32, scale: f32, text: &str, color: swf::Color) {
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

    /// Measure rendered width of `text` in pixels at the given scale. Mirrors
    /// `draw_text`'s per-char advance EXACTLY (bitmap-font chars = 6 units,
    /// CJK = full-width cell) so centring lines up with what's drawn — and so
    /// it works without the (lazy) atlas existing yet.
    /// `text`, shortened with a trailing ellipsis until it fits `max_w`.
    ///
    /// Flashpoint keeps a game's full published name, so titles run long
    /// ("Scooby-Doo: Mayan Monster Mayhem Episode 4 - The Temple of Lost Souls").
    /// A column that lets them run does not just look untidy, it draws over
    /// whatever sits beside it. Cuts by CHARACTER, which is what the 5x7 pixel
    /// font measures in anyway.
    pub fn fit_text(&self, text: &str, scale: f32, max_w: f32) -> std::string::String {
        if self.measure_text(text, scale) <= max_w {
            return text.to_string();
        }
        const ELL: &str = "...";
        let ell_w = self.measure_text(ELL, scale);
        let budget = max_w - ell_w;
        if budget <= 0.0 {
            return std::string::String::new();
        }
        let mut out = std::string::String::new();
        let mut w = 0.0;
        for ch in text.chars() {
            // `char_advance`, not `measure_text(&ch.to_string())`: the old form
            // allocated a String per character of every ellipsised label.
            let cw = char_advance(ch, scale);
            if w + cw > budget {
                break;
            }
            out.push(ch);
            w += cw;
        }
        out.push_str(ELL);
        out
    }

    /// Split `text` over at most two lines that each fit `max_w`, breaking on a
    /// space. The detail panels show the game you are looking at, so its name is
    /// worth two lines rather than an ellipsis: the list beside them is where a
    /// title gets cut, because there it only has to be recognisable.
    /// The second line is still ellipsised if even two lines are not enough.
    /// Wrap `text` onto at most `max_lines` lines of `max_w`, ellipsising only the
    /// last one.
    ///
    /// `wrap_text_2` is this with `max_lines` hard-wired to two, which is why
    /// BANDE cut "Scooby-Doo: Mayan Monster Mayhem Episode 4 - The Temple of Lost
    /// Souls" mid-title with 220 px of empty column under it: the limit was the
    /// helper's, not the layout's.
    pub fn wrap_text_n(
        &self,
        text: &str,
        scale: f32,
        max_w: f32,
        max_lines: usize,
    ) -> std::vec::Vec<std::string::String> {
        let mut out: std::vec::Vec<std::string::String> = std::vec::Vec::new();
        let mut rest = text.trim().to_string();
        while !rest.is_empty() && out.len() + 1 < max_lines {
            if self.measure_text(&rest, scale) <= max_w {
                break;
            }
            let Some((end, next)) = self.wrap_point(&rest, scale, max_w) else {
                break; // one unbroken word: the tail below cuts it
            };
            out.push(rest[..end].trim_end().to_string());
            rest = rest[next..].trim_start().to_string();
        }
        if !rest.is_empty() {
            out.push(self.fit_text(&rest, scale, max_w));
        }
        out
    }

    pub fn wrap_text_2(&self, text: &str, scale: f32, max_w: f32) -> (std::string::String, std::string::String) {
        if self.measure_text(text, scale) <= max_w {
            return (text.to_string(), std::string::String::new());
        }
        // Longest first line that still fits, ending on a break opportunity.
        let Some((end, next)) = self.wrap_point(text, scale, max_w) else {
            // One unbroken word: cut it rather than overflow.
            return (self.fit_text(text, scale, max_w), std::string::String::new());
        };
        let rest = text[next..].trim_start();
        (
            text[..end].trim_end().to_string(),
            self.fit_text(rest, scale, max_w),
        )
    }

    pub fn measure_text(&self, text: &str, scale: f32) -> f32 {
        let mut w = 0.0;
        for ch in text.chars() {
            w += char_advance(ch, scale);
        }
        w
    }

    /// Drop characters from the MIDDLE until `text` fits `max_w`.
    ///
    /// From the middle because these are game names, and what tells two of them
    /// apart lives at the END at least as often as at the start: cutting the
    /// tail turned the four "Scooby-Doo: Mayan Monster Mayhem Episode N - ..."
    /// into four identical rows.
    ///
    /// A character budget can only be turned into a width by assuming one width
    /// per character, and there are two: 6 units for the bitmap font, 8 for
    /// anything drawn from the shared font. Callers that assumed 6 overflowed
    /// their box by a third the moment a title was Chinese (issue #75).
    fn fit_text_mid(&self, text: &str, scale: f32, max_w: f32) -> std::string::String {
        if self.measure_text(text, scale) <= max_w {
            return text.to_string();
        }
        let chars: std::vec::Vec<char> = text.chars().collect();
        let ell_w = char_advance('\u{2026}', scale);
        // Prefix sums: `prefix[i]` is the width of the first `i` characters, so
        // testing a candidate head and tail is two lookups instead of two sums.
        // This runs per visible row of a list, and re-summing per candidate made
        // a long name in a narrow row quadratic.
        let mut prefix = std::vec::Vec::with_capacity(chars.len() + 1);
        prefix.push(0.0f32);
        for c in &chars {
            let w = prefix[prefix.len() - 1] + char_advance(*c, scale);
            prefix.push(w);
        }
        let total = prefix[chars.len()];
        // Give back one character at a time, splitting what is left between the
        // two ends, until the whole thing plus the ellipsis fits.
        for keep in (0..chars.len()).rev() {
            let head = keep / 2;
            let tail = keep - head;
            if prefix[head] + ell_w + (total - prefix[chars.len() - tail]) <= max_w {
                let mut out: std::string::String = chars[..head].iter().collect();
                out.push('\u{2026}');
                out.extend(chars[chars.len() - tail..].iter());
                return out;
            }
        }
        std::string::String::new()
    }

    /// Last break opportunity in `text` whose line still fits `max_w`, as
    /// `(byte index the line ends at, byte index the next line starts at)`.
    ///
    /// Latin breaks on spaces. Chinese, Japanese and Korean are written without
    /// any, so a title in one of those scripts offered no break at all: it was
    /// ellipsised onto a single line under a panel with room for three (visible
    /// as soon as issue #75 let a player type one in). In those scripts the
    /// break sits between any two characters, minus the closing punctuation
    /// that may not open a line and the opening brackets that may not close one.
    ///
    /// Widths accumulate as the scan walks the string rather than re-measuring
    /// every prefix: with a break candidate at every single character, the
    /// prefix-per-candidate version was quadratic in the title length.
    fn wrap_point(&self, text: &str, scale: f32, max_w: f32) -> Option<(usize, usize)> {
        let mut best: Option<(usize, usize)> = None;
        let mut prev: Option<(usize, char)> = None;
        // Width of `text[..i]` for the `i` the loop is looking at.
        let mut w = 0.0f32;
        for (i, ch) in text.char_indices() {
            if let Some((pi, pc)) = prev {
                // (line ends here, next line starts here, width of that line).
                // A space is eaten by the break; a CJK break keeps both
                // characters, so the line runs up to `i` included.
                let point = if pc == ' ' {
                    Some((pi, i, w - char_advance(pc, scale)))
                } else if (is_cjk_wrappable(pc) || is_cjk_wrappable(ch))
                    && ch != ' '
                    && !cjk_no_line_start(ch)
                    && !cjk_no_line_end(pc)
                {
                    Some((i, i, w))
                } else {
                    None
                };
                if let Some((end, next, line_w)) = point {
                    if end == 0 {
                        // Never emit an empty line: it would consume no input
                        // and the caller's loop would not terminate.
                    } else if line_w <= max_w {
                        best = Some((end, next));
                    } else {
                        // Prefixes only grow, so nothing further out fits either.
                        break;
                    }
                }
            }
            w += char_advance(ch, scale);
            prev = Some((i, ch));
        }
        best
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
        self.use_bitmap(&world, &mult, &NO_ADD, tex, &PASSTHROUGH_UV);
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


    /// Draw the pause-modal overlay on top of whatever's already in the
    /// framebuffer. The caller is expected to have re-rendered the paused
    /// game state (via `Player::render`) so this overlay sits over a frozen
    /// snapshot of the game, not a blank screen.
    ///
    /// `selected` indexes `MENU_ITEMS`. The cursor `>` is drawn on the
    /// selected row; the selected label is rendered in yellow, others in
    /// white. Help line at the bottom describes the buttons.
    pub fn draw_menu_overlay(&mut self, selected: usize) {
        // Shared modal chrome: game name in the subtitle slot (mirrors the
        // OPTIONS modal). Held in a local so `as_deref()` can feed the frame.
        let lc = crate::loc::s();
        let game = crate::library::active_display_name();
        let frame = self.draw_modal_frame(
            MODAL_W,
            MENU_ITEMS.len(),
            None,
            false,
            lc.pause_title,
            game.as_deref(),
            Some(lc.pause_footer),
        );

        // Localized labels, same order/count as the MENU_ITEMS contract C++
        // relies on for pause-menu navigation. Cursor speed moved into the
        // TOUCHES sub-menu (#20 Option 1) and the three screen settings into
        // ECRAN, so neither is a top-level item anymore.
        let items = [
            lc.menu_resume,
            lc.menu_keys,
            lc.menu_screen,
            lc.menu_restart,
            lc.menu_quit,
        ];
        debug_assert_eq!(items.len(), MENU_ITEMS.len());
        self.draw_modal_rows(&frame, selected, &items);

        unsafe {
            glUseProgram(0);
            glBindVertexArray(0);
        }
        self.gl_state.invalidate();
    }

    /// Draw the ECRAN sub-panel: the three settings that change how the game
    /// sits on the screen, each carrying its live value.
    ///
    /// Every row previews on the frozen frame behind the panel — C++ re-renders
    /// it after each press rather than `continue`ing — so you see the crop, the
    /// quarter turn or the scanlines on that particular game before going back
    /// to playing. That shared behaviour is the reason these three belong on one
    /// panel and QUITTER does not.
    pub fn draw_screen_menu(&mut self, selected: usize) {
        let lc = crate::loc::s();
        let game = crate::library::active_display_name();
        let frame = self.draw_modal_frame(
            MODAL_W,
            SCREEN_ITEMS.len(),
            None,
            false,
            lc.menu_screen,
            game.as_deref(),
            Some(lc.pause_footer),
        );
        let display_label = std::format!(
            "{}: {}",
            lc.set_display_mode,
            crate::loc::display_mode_label(crate::keymap::display_mode()),
        );
        let rotation_label = std::format!(
            "{}: {}",
            lc.set_rotation,
            crate::loc::rotation_label(crate::keymap::rotation()),
        );
        let zoom_label = std::format!("{}: {} %", lc.set_zoom, game_zoom_percent());
        let filter_label = std::format!(
            "{}: {}",
            lc.set_screen_filter,
            crate::loc::screen_filter_label(crate::keymap::screen_filter()),
        );
        let items = [
            display_label.as_str(),
            rotation_label.as_str(),
            zoom_label.as_str(),
            filter_label.as_str(),
        ];
        debug_assert_eq!(items.len(), SCREEN_ITEMS.len());
        self.draw_modal_rows(&frame, selected, &items);

        unsafe {
            glUseProgram(0);
            glBindVertexArray(0);
        }
        self.gl_state.invalidate();
    }

    /// Draw `text` with a dark outline around it, for the one case where text
    /// has to sit directly on the game's picture with no plate under it: it must
    /// stay readable over a white sky and over a black cave, and any plate wide
    /// enough to guarantee that would hide the thing being looked at.
    ///
    /// Four offset copies rather than a single drop shadow, because a shadow
    /// only rescues the two edges it falls on.
    fn draw_text_outlined(&mut self, x: f32, y: f32, scale: f32, text: &str, color: swf::Color) {
        let o = (scale * 0.9).max(2.0);
        let edge = swf::Color::from_rgba(0xE6_00_00_00);
        for (dx, dy) in [(-o, 0.0), (o, 0.0), (0.0, -o), (0.0, o)] {
            self.draw_text(x + dx, y + dy, scale, text, edge);
        }
        self.draw_text(x, y, scale, text, color);
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
        let lc = crate::loc::s();
        let (vw, vh) = (self.dimensions.width as f32, self.dimensions.height as f32);
        unsafe {
            glEnable(GL_BLEND);
            glBlendFunc(GL_SRC_ALPHA, GL_ONE_MINUS_SRC_ALPHA);
        }
        // The WHOLE framing state, not just the percentage: the offset is half
        // of what is being set, it is what a rotation resets, and it is what
        // lands in the game's `.prefs` as `panx` / `pany`. Showing only the
        // magnification meant the other half could move without anything on
        // screen saying so.
        let (ox, oy) = game_pan();
        let head = std::format!("{}  {} %    X {}    Y {}", lc.set_zoom, percent, ox, oy);
        let hs = 2.2;
        let hw = self.measure_text(&head, hs);
        self.draw_text_outlined(
            (vw - hw) * 0.5,
            14.0,
            hs,
            &head,
            swf::Color::from_rgb(0xFFD740, 255),
        );

        // What every button does now. Fitted rather than counted, because this
        // line is translated and some languages run long.
        let ls = 1.6;
        let legend = self.fit_text(lc.zoom_legend, ls, vw - 40.0);
        let lw = self.measure_text(&legend, ls);
        self.draw_text_outlined(
            (vw - lw) * 0.5,
            vh - 14.0 - 7.0 * ls,
            ls,
            &legend,
            swf::Color::from_rgb(0xFFFFFF, 255),
        );

        unsafe {
            glUseProgram(0);
            glBindVertexArray(0);
        }
        self.gl_state.invalidate();
    }

    /// Draw a centred horizontal strip of tab "chips" (labels), highlighting the
    /// `active` one in amber. Centred on the screen (which is the modal centre).
    /// Used for the editor's player tabs, layer sub-tabs and modifier strip (#57)
    /// so every mode is VISIBLE rather than hidden behind a button you must guess.
    fn draw_chip_strip(&mut self, labels: &[&str], active: usize, y: f32, scale: f32, h: f32) {
        const PAD_X: f32 = 22.0;
        const GAP: f32 = 18.0;
        let widths: std::vec::Vec<f32> = labels
            .iter()
            .map(|l| self.measure_text(l, scale) + 2.0 * PAD_X)
            .collect();
        let total: f32 =
            widths.iter().sum::<f32>() + GAP * labels.len().saturating_sub(1) as f32;
        let mut x = (self.dimensions.width as f32 - total) * 0.5;
        for (i, lbl) in labels.iter().enumerate() {
            let w = widths[i];
            let cap = Matrix {
                a: w, b: 0.0, c: 0.0, d: h,
                tx: swf::Twips::from_pixels(x as f64),
                ty: swf::Twips::from_pixels(y as f64),
            };
            let (bg, fg) = if i == active {
                (0xFFD740u32, 0x1A1A1Au32)
            } else {
                (0x2A3340u32, MODAL_ROW_COL)
            };
            <Self as CommandHandler>::draw_rect(self, swf::Color::from_rgb(bg, 255), cap);
            let lw = self.measure_text(lbl, scale);
            self.draw_text(
                x + (w - lw) * 0.5,
                y + (h - 7.0 * scale) * 0.5,
                scale,
                lbl,
                swf::Color::from_rgb(fg, 255),
            );
            x += w + GAP;
        }
    }

    /// Largest scale at or below `want` that fits `text` in a `w`-wide box, or
    /// None when even scale 1 overflows — the renderer's way of saying "this
    /// shape is too small to label", which is true of the SL/SR rail buttons.
    fn label_scale(&self, text: &str, w: f32, want: f32) -> Option<f32> {
        let full = self.measure_text(text, want);
        if full <= w {
            return Some(want);
        }
        let s = want * w / full;
        if s >= 1.0 {
            Some(s)
        } else {
            None
        }
    }

    /// TOUCHES pad view — the keymap editor.
    ///
    /// Draws the controller from `keymap::PAD_SLOTS` (positioned controls in
    /// abstract units) with one value chip per control, in two columns flanking
    /// the picture, and lights the selected control in BOTH places at once: the
    /// chip you are reading and the button you would press.
    ///
    /// That pairing is the point of the screen. The list this replaced could
    /// tell you a binding but never where the button was, and it showed eight of
    /// twenty-five at a time — so reading one keymap took four screens, times
    /// five combo layers, times two players. Nothing here scrolls.
    ///
    /// `bindings` is parallel to `PAD_SLOTS`: `(binding, is_modifier)`. A slot
    /// flagged `is_modifier` sends no key of its own  either it is the open
    /// layer's own modifier, or it is a modifier everywhere because its layer has
    /// a binding (see `keymap::slot_is_modifier`). Those rows are drawn dimmed
    /// with the word MODIFIER where the key would be, and their button goes teal
    /// on the picture, so a combo layer looks like what it is instead of being a
    /// word in a tab strip.
    pub fn draw_touches_pad(
        &mut self,
        selection: usize,
        bindings: &[(Option<std::string::String>, bool)],
        player: u8,
        subtab: usize,
    ) {
        use crate::keymap::{Nub, PadIcon};
        // Amber is the cursor everywhere in this app. Teal marks a button that
        // is a MODIFIER rather than a key  the same teal the keyboard picker
        // uses for "in play", and the same meaning: this one is spoken for.
        const SHELL_COL: u32 = 0xFF_1E2735;
        const ICON_COL: u32 = 0xFF_3A4657;
        const ICON_NUB_COL: u32 = 0xFF_5A6A80;
        const ICON_SEL_COL: u32 = 0xFF_FFD740;
        const ICON_SEL_NUB: u32 = 0xFF_1A1A1A;
        const ICON_MOD_COL: u32 = 0xFF_2E6E63;
        const ICON_MOD_NUB: u32 = 0xFF_58C0AE;
        const ICON_LOCK_COL: u32 = 0xFF_2A3038;
        const CHIP_COL: u32 = 0xFF_2A3340;
        const CHIP_LOCK_COL: u32 = 0xFF_222A34;
        const SEP_COL: u32 = 0xFF_4A5568;
        const LOCK_TXT_COL: u32 = 0x6E7B8C;
        const SEL_WASH: u32 = 0x33_FF_D7_40;

        let lc = crate::loc::s();
        const PANEL_W: f32 = 1180.0;
        const SIDE_MARGIN: f32 = 26.0;
        // Title + the two chip strips above, the legend below. Both bands are
        // fixed pixels, so the pad takes whatever is left — which is exactly
        // what the scale below is measured from.
        const HEAD_H: f32 = 150.0;
        const FOOT_H: f32 = 56.0;

        let vw = self.dimensions.width as f32;
        let vh = self.dimensions.height as f32;
        // The same clamp `draw_modal_frame` applies, run BEFORE the call: the
        // panel's height depends on the scale, and the scale depends on the
        // width it will actually get. A turned picture gives a 720-wide viewport,
        // and a pad laid out for 1180 would run off both edges of it.
        let w = PANEL_W.min(vw - 40.0);
        let inner_w = w - 2.0 * SIDE_MARGIN;
        let avail_h = (vh - 40.0 - HEAD_H - FOOT_H).max(80.0);
        // ONE scale taken from BOTH axes. The keyboard could scale on width
        // alone because it is a block of rows; this is a picture of an object,
        // and letting the axes disagree would stretch the controller.
        let u = (inner_w / crate::keymap::PAD_UNITS_W).min(avail_h / crate::keymap::PAD_UNITS_H);
        let pad_w = crate::keymap::PAD_UNITS_W * u;
        let pad_h = crate::keymap::PAD_UNITS_H * u;

        let frame = self.draw_modal_frame(
            PANEL_W,
            0,
            Some(HEAD_H + pad_h + FOOT_H),
            false,
            lc.keys_title,
            None,
            Some(lc.keys_footer),
        );
        // Two visible tab rows, as the list had: nothing about the mode you are
        // editing should need a guess-press to discover (issue #55/#57).
        self.draw_chip_strip(
            &["P1", "P2"],
            if player == 2 { 1 } else { 0 },
            frame.y + 52.0,
            2.2,
            38.0,
        );
        self.draw_chip_strip(
            &["NORMAL", "ZL", "ZR", "L", "R"],
            subtab.min(4),
            frame.y + 98.0,
            1.9,
            34.0,
        );

        let ox = frame.x + (frame.w - pad_w) * 0.5;
        let oy = frame.y + HEAD_H;
        // Units -> pixels. Every rect below goes through these two, so the table
        // in keymap.rs is the only place the layout is written down.
        let px = |ux: f32| ox + ux * u;
        let py = |uy: f32| oy + uy * u;

        // ── The shell ────────────────────────────────────────────────────
        // Grips first, body over them, one colour: it is a silhouette, and a
        // seam between the three would read as three objects.
        for &(sx, sy, sw, sh, r) in crate::keymap::PAD_SHELL {
            self.draw_round_rect(px(sx), py(sy), sw * u, sh * u, r * u, SHELL_COL);
        }

        // ── Minus, locked ────────────────────────────────────────────────
        {
            let (mx, my, mw, mh) = crate::keymap::PAD_MINUS;
            let (x, y, d) = (px(mx), py(my), mw * u);
            self.draw_round_rect(x, y, d, mh * u, d * 0.5, ICON_LOCK_COL);
            if let Some(s) = self.label_scale("-", d - 3.0, 1.6) {
                let lw = self.measure_text("-", s);
                self.draw_text(
                    x + (d - lw) * 0.5,
                    y + (mh * u - 7.0 * s) * 0.5,
                    s,
                    "-",
                    swf::Color::from_rgb(ICON_NUB_COL, 255),
                );
            }
        }

        // ── The controls on the picture ──────────────────────────────────
        // The five slots of one stick share a rect on purpose, so a stick must
        // be drawn ONCE — with the nub of whichever of the five is selected.
        // Drawn per-slot they would overdraw each other, and the last one in the
        // table would always win. Same reason the selected control is drawn in a
        // second pass: the d-pad arms meet, and the one under the cursor has to
        // be the one on top.
        let sel_slot = crate::keymap::PAD_SLOTS.get(selection);
        for pass in 0..2 {
            for (i, slot) in crate::keymap::PAD_SLOTS.iter().enumerate() {
                let is_mod = bindings.get(i).map(|b| b.1).unwrap_or(false);
                let is_sel = i == selection;
                if is_sel != (pass == 1) {
                    continue;
                }
                let (ix, iy, iw, ih) = slot.icon;
                let (x, y, w, h) = (px(ix), py(iy), iw * u, ih * u);
                let (body, nub) = if is_sel {
                    (ICON_SEL_COL, ICON_SEL_NUB)
                } else if is_mod {
                    (ICON_MOD_COL, ICON_MOD_NUB)
                } else {
                    (ICON_COL, ICON_NUB_COL)
                };
                match slot.shape {
                    PadIcon::Stick(n) => {
                        // Only the first slot of the group draws the ring, and
                        // only when the cursor is elsewhere: a selected slot owns
                        // its stick and draws it in pass 1 with its own nub.
                        let group_head = crate::keymap::PAD_SLOTS
                            .iter()
                            .position(|o| {
                                matches!(o.shape, PadIcon::Stick(_)) && o.icon == slot.icon
                            })
                            .unwrap_or(i);
                        let owner = sel_slot
                            .filter(|s| matches!(s.shape, PadIcon::Stick(_)) && s.icon == slot.icon)
                            .is_some();
                        if (owner && !is_sel) || (!owner && i != group_head) {
                            continue;
                        }
                        let n = if is_sel { n } else { Nub::Press };
                        self.draw_round_rect(x, y, w, h, w * 0.5, body);
                        let nd = w * 0.52;
                        let off = w * 0.20;
                        let (dx, dy) = match n {
                            Nub::Up => (0.0, -off),
                            Nub::Down => (0.0, off),
                            Nub::Left => (-off, 0.0),
                            Nub::Right => (off, 0.0),
                            Nub::Press => (0.0, 0.0),
                        };
                        self.draw_round_rect(
                            x + w * 0.5 + dx - nd * 0.5,
                            y + h * 0.5 + dy - nd * 0.5,
                            nd,
                            nd,
                            nd * 0.5,
                            nub,
                        );
                    }
                    PadIcon::Disc => {
                        self.draw_round_rect(x, y, w, h, w.min(h) * 0.5, body);
                    }
                    PadIcon::Slab => {
                        self.draw_round_rect(x, y, w, h, w.min(h) * 0.34, body);
                    }
                }
                // The glyph, when the shape is big enough to carry one. The rail
                // buttons are 0.7 units wide and never are; they are named by
                // their chip and found by lighting up.
                if !matches!(slot.shape, PadIcon::Stick(_)) {
                    // Selected wins. The cursor should never be on a modifier
                    // row -- menu.rs steps over them and refuses them to a
                    // finger -- but a layer can change under a resting cursor,
                    // and a label that turned invisible for one frame would be
                    // a worse answer than one that stays readable.
                    let txt = if is_sel {
                        0x1A1A1A
                    } else if is_mod {
                        0xEAF7F3
                    } else {
                        0xE8EEF6
                    };
                    if let Some(s) = self.label_scale(slot.glyph, w - 4.0, 2.0) {
                        let lw = self.measure_text(slot.glyph, s);
                        self.draw_text(
                            x + (w - lw) * 0.5,
                            y + (h - 7.0 * s) * 0.5,
                            s,
                            slot.glyph,
                            swf::Color::from_rgb(txt, 255),
                        );
                    }
                }
            }
        }

        // ── The value chips ──────────────────────────────────────────────
        // Backgrounds first, then the eased wash, then every label: the wash has
        // to sit ON the chip it marks and UNDER the text it marks it for, and a
        // chip drawn after it would paint over it.
        let chip_h = crate::keymap::PAD_SLOTS
            .first()
            .map(|s| s.chip.3 * u)
            .unwrap_or(0.0);
        let chip_r = (chip_h * 0.25).min(5.0);
        let badge_w = 3.4 * u;
        let txt_scale = (chip_h / 16.0).clamp(1.0, 2.2);
        let mut cells: std::vec::Vec<(f32, f32, f32, f32)> =
            std::vec::Vec::with_capacity(crate::keymap::PAD_SLOTS.len());
        for (i, slot) in crate::keymap::PAD_SLOTS.iter().enumerate() {
            let locked = bindings.get(i).map(|b| b.1).unwrap_or(false);
            let (cx, cy, cw, ch) = slot.chip;
            let (x, y, w, h) = (px(cx), py(cy), cw * u, ch * u);
            cells.push((x, y, w, h));
            self.draw_round_rect(x, y, w, h, chip_r, if locked { CHIP_LOCK_COL } else { CHIP_COL });
            // A hairline instead of a second filled box behind the badge: the
            // separator says "name | value" for the price of one rect, and this
            // panel draws twenty-five of everything.
            self.draw_overlay_rect(x + badge_w, y + h * 0.22, 2.0, h * 0.56, SEP_COL);
        }

        let now = unsafe { ruffle_tick_now() };
        if let Some(slot) = sel_slot {
            let (cx, cy, cw, ch) = slot.chip;
            // Eased on BOTH axes with one key, like the language grid: the
            // cursor crosses columns here, and a jump from the left column to
            // the right is the move that most needs to be followed by eye.
            let bx = eased_list_x(px(cx), GLIDE_KEY_KEYS, now);
            let by = eased_list_y(py(cy), GLIDE_KEY_KEYS, now);
            // Drawn, not masked: `draw_selection_bar` cuts its corners with the
            // PAGE colour, which over a chip on a modal would leave four notches
            // of the wrong navy in the chip itself.
            self.draw_round_rect(bx, by, cw * u, ch * u, chip_r, SEL_WASH);
        }

        for (i, slot) in crate::keymap::PAD_SLOTS.iter().enumerate() {
            let (cx, cy, cw, ch) = slot.chip;
            let (x, y, w, h) = (px(cx), py(cy), cw * u, ch * u);
            let locked = bindings.get(i).map(|b| b.1).unwrap_or(false);
            let is_sel = i == selection;
            let col = swf::Color::from_rgb(
                if locked {
                    LOCK_TXT_COL
                } else if is_sel {
                    MODAL_ROW_SEL_COL
                } else {
                    MODAL_ROW_COL
                },
                255,
            );
            // Badge: what you press. Centred in its half so the arrows line up
            // down the column instead of drifting with the word beside them.
            if let Some(s) = self.label_scale(slot.glyph, badge_w - 10.0, txt_scale) {
                let lw = self.measure_text(slot.glyph, s);
                self.draw_text(
                    x + (badge_w - lw) * 0.5,
                    y + (h - 7.0 * s) * 0.5,
                    s,
                    slot.glyph,
                    swf::Color::from_rgb(
                        if locked {
                            LOCK_TXT_COL
                        } else if is_sel {
                            MODAL_ROW_SEL_COL
                        } else {
                            0xFFFFFF
                        },
                        255,
                    ),
                );
            }
            // Value: what it does. Shrinks rather than truncating, the way every
            // other box in this app does — a binding cut to "RIGHT CLI" is worse
            // than a small one.
            // Not the stored binding: it is still in the keymap, and it will
            // work again the day the layer is emptied, but showing it here would
            // promise a key press that `main.cpp` mutes.
            let value = if locked {
                std::borrow::Cow::Borrowed(lc.keys_modifier)
            } else {
                bindings
                    .get(i)
                    .and_then(|b| b.0.as_deref())
                    .map(crate::keymap::flash_key_display)
                    .unwrap_or(std::borrow::Cow::Borrowed(lc.none))
            };
            let avail = w - badge_w - 18.0;
            // Floor of 1: below that a 5x7 bitmap font is mush, so the rule the
            // keyboard picker already follows applies here — shrink to 1, then
            // let it run. At scale 1 the value column takes thirty-odd
            // characters and the longest binding name is half that.
            let s = self.label_scale(&value, avail, txt_scale).unwrap_or(1.0);
            self.draw_text(
                x + badge_w + 12.0,
                y + (h - 7.0 * s) * 0.5,
                s,
                &value,
                col,
            );
        }
        ui_cells_publish(ui_screen_kind(), cells);

        unsafe {
            glUseProgram(0);
            glBindVertexArray(0);
        }
        self.gl_state.invalidate();
    }

    /// TOUCHES keyboard picker (issue #55) — shown when the user presses A on a
    /// control of the pad. Draws a real PC keyboard from `keymap::KEYBOARD` (positioned keys,
    /// numpad on the RIGHT) with the key at `sel_key_idx` highlighted amber. Keys
    /// already bound to another button in the current map (`used`) get a teal cap
    /// so the user sees a key is already in use (they can still pick it). The user
    /// navigates geometrically (menu.rs). QWERTY is fixed for every language.
    pub fn draw_touches_keyboard(
        &mut self,
        button_name: &str,
        sel_key_idx: usize,
        used: &std::collections::BTreeSet<std::string::String>,
    ) {
        let keys = crate::keymap::KEYBOARD;
        const KEY_H: f32 = 48.0;
        const KEY_GAP: f32 = 6.0;
        const KEY_SCALE: f32 = 2.0;
        const SIDE_MARGIN: f32 = 30.0;
        // Wider than the 720 modal so the main block + right-hand numpad both fit.
        const PANEL_W: f32 = 1120.0;

        // The frame clamps its width to the screen, so a turned picture gives a
        // narrower panel; the key HEIGHT has to follow or the keys come out thin
        // and tall, and the panel taller than it needs to be.
        let vw = self.dimensions.width as f32;
        let shrink = ((vw - 40.0) / PANEL_W).clamp(0.45, 1.0);
        let key_h = KEY_H * shrink;
        let key_gap = KEY_GAP * shrink;
        let key_scale = KEY_SCALE * shrink;
        // Header and footer bands shrink WITH the keys. Scaling the key block
        // alone left the rows running past the bottom of the panel and over the
        // "A:OK  B:ANNULER" line, because the header still reserved its landscape
        // 108 px while the block below it had got shorter.
        let head_h = 108.0 * shrink.max(0.7);
        let foot_h = 54.0 * shrink.max(0.7);
        let n_rows = crate::keymap::KEYBOARD_ROWS_N as f32;
        let panel_h = head_h + n_rows * (key_h + key_gap) + foot_h;
        let title = std::format!("{} ->", button_name);
        let frame = self.draw_modal_frame(
            PANEL_W,
            0,
            Some(panel_h),
            false,
            &title,
            None,
            Some(crate::loc::s().keys_dropdown_footer),
        );

        let inner_w = frame.w - 2.0 * SIDE_MARGIN;
        let unit_w = inner_w / crate::keymap::KEYBOARD_UNITS_W;
        let origin_x = frame.x + SIDE_MARGIN;
        let top_y = frame.y + head_h;

        // The caps are hit-testable now. The rects were already being computed
        // here and thrown away at the end of each iteration; they are exactly
        // the numbers a tap needs, and the layout is responsive (`shrink`), so
        // they could never have been constants somewhere else.
        let mut cells: std::vec::Vec<(f32, f32, f32, f32)> =
            std::vec::Vec::with_capacity(keys.len());
        for (i, &(name, row, kx, kw)) in keys.iter().enumerate() {
            let x = origin_x + kx * unit_w;
            let key_w = kw * unit_w - KEY_GAP;
            let y = top_y + row as f32 * (key_h + key_gap);
            cells.push((x, y, key_w, key_h));
            let cap = Matrix {
                a: key_w, b: 0.0, c: 0.0, d: key_h,
                tx: swf::Twips::from_pixels(x as f64),
                ty: swf::Twips::from_pixels(y as f64),
            };
            let (cap_col, txt_col) = if i == sel_key_idx {
                (0xFFD740u32, 0x1A1A1Au32) // amber cap, near-black label (cursor)
            } else if used.contains(name) {
                (0x2E6E63u32, 0xEAF7F3u32) // teal cap = already bound elsewhere
            } else {
                (0x2A3340u32, MODAL_ROW_COL) // slate cap, light label
            };
            <Self as CommandHandler>::draw_rect(self, swf::Color::from_rgb(cap_col, 255), cap);

            // Centre the (only-if-needed shrunk) label in the cap.
            let label = crate::keymap::keyboard_label(name);
            let lw_full = self.measure_text(&label, key_scale);
            let scale = if lw_full > key_w - 8.0 {
                (key_scale * (key_w - 8.0) / lw_full).max(1.0)
            } else {
                key_scale
            };
            let lw = self.measure_text(&label, scale);
            self.draw_text(
                x + (key_w - lw) * 0.5,
                y + (key_h - 7.0 * scale) * 0.5,
                scale,
                &label,
                swf::Color::from_rgb(txt_col, 255),
            );
        }
        ui_cells_publish(ui_screen_kind(), cells);

        unsafe {
            glUseProgram(0);
            glBindVertexArray(0);
        }
        self.gl_state.invalidate();
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
        self.use_bitmap(&world, &mult, &add, tex, &uv_remap);
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
        self.use_bitmap(&world, &mult, &add, tex, &uv_remap);
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
        self.library_clear();
        let vw = self.dimensions.width as f32;
        let vh = self.dimensions.height as f32;

        let title = crate::loc::s().empty_title;
        let scale_title = 6.0;
        let title_w = self.measure_text(title, scale_title);
        // Drop shadow on the title — dark navy offset (4, 4) under the white.
        self.draw_text(
            (vw - title_w) * 0.5 + 4.0,
            vh * 0.30 + 4.0,
            scale_title,
            title,
            swf::Color::from_rgb(0x000000, 255),
        );
        self.draw_text(
            (vw - title_w) * 0.5,
            vh * 0.30,
            scale_title,
            title,
            swf::Color::from_rgb(0xFFD740, 255),
        );

        let lc = crate::loc::s();
        let lines = [lc.empty_l1, lc.empty_l2, lc.empty_l3];
        let scale_msg = 2.5;
        let mut y = vh * 0.48;
        for line in &lines {
            let w = self.measure_text(line, scale_msg);
            self.draw_text(
                (vw - w) * 0.5,
                y,
                scale_msg,
                line,
                swf::Color::from_rgb(0xCCCCCC, 255),
            );
            y += 40.0;
        }

        // Footer: Y opens DISTANT mode (so a user with empty SD can
        // still import via archive.org without needing to drop files
        // on SD first); - exits .nro.
        const HELP_SCALE: f32 = 2.0;
        let help = crate::loc::s().empty_footer;
        let help_w = self.measure_text(help, HELP_SCALE);
        self.draw_text(
            (vw - help_w) * 0.5,
            vh - 60.0,
            HELP_SCALE,
            help,
            swf::Color::from_rgb(0x99AABB, 255),
        );
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
        let rect = |s: &mut Self, rx: f32, ry: f32, rw: f32, rh: f32, col: swf::Color| {
            if rw <= 0.0 || rh <= 0.0 {
                return;
            }
            let m = Matrix {
                a: rw, b: 0.0, c: 0.0, d: rh,
                tx: swf::Twips::from_pixels(rx as f64),
                ty: swf::Twips::from_pixels(ry as f64),
            };
            <Self as CommandHandler>::draw_rect(s, col, m);
        };
        // Dim the four panes outside the window (darkens the list behind so the
        // bright window pops). Shrinks to nothing as the window fills the screen.
        let dim = swf::Color::from_rgba(0x88_00_00_00);
        rect(self, 0.0, 0.0, vw, y, dim); // top
        rect(self, 0.0, y + h, vw, vh - (y + h), dim); // bottom
        rect(self, 0.0, y, x, h, dim); // left
        rect(self, x + w, y, vw - (x + w), h, dim); // right
        // Bright border around the window.
        let col = swf::Color::from_rgb(0xFFD740, 255);
        let b = 4.0;
        rect(self, x - b, y - b, w + 2.0 * b, b, col); // top
        rect(self, x - b, y + h, w + 2.0 * b, b, col); // bottom
        rect(self, x - b, y, b, h, col); // left
        rect(self, x + w, y, b, h, col); // right
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
            CoverTex::Image { tex, w: iw, h: ih } if iw > 0 && ih > 0 => {
                // Black backdrop (the letterbox bars).
                self.draw_overlay_rect(x, y, w, h, 0xFF_00_00_00);
                // Fit the cover inside (w,h) keeping its aspect, centred.
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
                // No cover: the colour chip + initials fill the window.
                self.draw_overlay_rect(x, y, w, h, 0xFF_00_00_00 | color_chip);
                let initials: std::string::String = display_name.chars().take(3).collect();
                let isc = (h / 36.0).clamp(3.0, 14.0);
                let tw = self.measure_text(&initials, isc);
                self.draw_text(
                    x + (w - tw) * 0.5,
                    y + (h - 7.0 * isc) * 0.5,
                    isc,
                    &initials,
                    swf::Color::from_rgb(0xFFFFFF, 255),
                );
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
        const N: usize = 8;
        let two_pi = 2.0 * core::f32::consts::PI;
        let freq = unsafe { ruffle_tick_freq() } as f32;
        let phase = if freq > 0.0 { (now as f32 / freq) * 6.0 } else { 0.0 };
        let dot = (radius * 0.34).max(5.0);
        for i in 0..N {
            let a = two_pi * (i as f32) / (N as f32);
            let dx = cx + radius * approx_sin(a + core::f32::consts::FRAC_PI_2); // cos
            let dy = cy + radius * approx_sin(a);
            let b = (approx_sin(phase - a) * 0.5 + 0.5).clamp(0.0, 1.0);
            let alpha = (40.0 + b * 215.0) as u32;
            let rgba = (alpha << 24) | 0x00_FF_FF_FF;
            let m = Matrix {
                a: dot, b: 0.0, c: 0.0, d: dot,
                tx: swf::Twips::from_pixels((dx - dot * 0.5) as f64),
                ty: swf::Twips::from_pixels((dy - dot * 0.5) as f64),
            };
            <Self as CommandHandler>::draw_rect(self, swf::Color::from_rgba(rgba), m);
        }
    }

    /// Loading panel content for the IMPORTER async fetch: the URL/item title
    /// above a centred spinner. The caller fills the window with an opaque panel
    /// first (so the URL list behind is replaced, not seen through).
    pub fn draw_loading_panel(&mut self, title: &str, now: u64) {
        let vw = self.dimensions.width as f32;
        let vh = self.dimensions.height as f32;
        let ts = 2.5;
        // The 48-character line this has always used, as the width it occupies.
        let t = self.fit_text_mid(title, ts, 48.0 * 6.0 * ts);
        let tw = self.measure_text(&t, ts);
        self.draw_text(
            (vw - tw) * 0.5,
            vh * 0.5 - 96.0,
            ts,
            &t,
            swf::Color::from_rgb(0xFFFFFF, 255),
        );
        self.draw_spinner(vw * 0.5, vh * 0.5 + 8.0, 30.0, now);
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

    /// Draw the shared chrome of a centered modal: dim backdrop, panel rect,
    /// border, centered title, optional centered subtitle, optional centered
    /// footer. Returns the panel geometry so the caller lays out its body via
    /// [`ModalFrame`]. The height is auto-sized to `rows` unless `fixed_h` is
    /// given; `danger=true` switches to the red destructive-action theme.
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
        unsafe {
            glEnable(GL_BLEND);
            glBlendFunc(GL_SRC_ALPHA, GL_ONE_MINUS_SRC_ALPHA);
            glDisable(GL_STENCIL_TEST);
        }
        let vw = self.dimensions.width as f32;
        let vh = self.dimensions.height as f32;

        // Translate-immune backdrop so the panel can scale/drop in (modal-open
        // pop) without the dim sliding off an edge.
        self.fill_screen_dim(if danger { MODAL_DIM_DANGER } else { MODAL_DIM });

        // Never wider than the screen it sits on. Panel widths are written for a
        // 1280-wide viewport, and the logical viewport is 720 wide while the
        // picture is turned a quarter: the keyboard asks for 1120 and ran off
        // both edges. Clamping HERE fixes every panel at once, including the ones
        // written after this, and is a no-op at 1280.
        let w = width.min(vw - 2.0 * 20.0);
        // No subtitle → tighten the title-to-rows gap (drop the empty subtitle
        // band) so we don't waste vertical space (e.g. the language picker).
        let pad_top = if subtitle.is_some() { MODAL_PAD_TOP } else { MODAL_PAD_TOP_TIGHT };
        let h = fixed_h
            .unwrap_or(pad_top + rows.max(1) as f32 * MODAL_ROW_H + MODAL_PAD_BOTTOM);
        let x = (vw - w) * 0.5;
        let y = (vh - h) * 0.5;
        let panel = Matrix {
            a: w, b: 0.0, c: 0.0, d: h,
            tx: swf::Twips::from_pixels(x as f64),
            ty: swf::Twips::from_pixels(y as f64),
        };
        let (bg, border) = if danger {
            (MODAL_BG_DANGER, MODAL_BORDER_DANGER)
        } else {
            (MODAL_BG, MODAL_BORDER)
        };
        // Rounded, and DRAWN rounded rather than masked: a modal sits over the
        // dimmed library, so cutting its corners with a flat colour would show the
        // wrong thing there. The border is inset by a pixel and drawn the same way,
        // which keeps it on the curve.
        const MODAL_R: f32 = 12.0;
        self.draw_round_rect(x - 1.0, y - 1.0, w + 2.0, h + 2.0, MODAL_R + 1.0, 0xFF_00_00_00 | border);
        self.draw_round_rect(x, y, w, h, MODAL_R, bg);
        let _ = panel;

        // Title — shrinks to fit so a long title (e.g. a confirm question, or a
        // long-language string) never overflows the panel edges.
        let title_col = if danger { MODAL_TITLE_COL_DANGER } else { MODAL_TITLE_COL };
        let avail = w - MODAL_ROW_X;
        let tw_full = self.measure_text(title, MODAL_TITLE_SCALE);
        let tscale = if tw_full > avail {
            MODAL_TITLE_SCALE * avail / tw_full
        } else {
            MODAL_TITLE_SCALE
        };
        let tw = self.measure_text(title, tscale);
        self.draw_text(
            x + (w - tw) * 0.5,
            y + 25.0,
            tscale,
            title,
            swf::Color::from_rgb(title_col, 255),
        );

        // Optional subtitle (e.g. the game name). Shrinks to fit rather than
        // truncating with "…" — so the standard 520 width shows the whole name,
        // just smaller, instead of cutting it off (Jonathan's no-truncation ask).
        if let Some(sub) = subtitle {
            let avail = w - MODAL_ROW_X;
            let sw_full = self.measure_text(sub, MODAL_SUB_SCALE);
            let scale = if sw_full > avail {
                MODAL_SUB_SCALE * avail / sw_full
            } else {
                MODAL_SUB_SCALE
            };
            let sw = self.measure_text(sub, scale);
            self.draw_text(
                x + (w - sw) * 0.5,
                y + 75.0,
                scale,
                sub,
                swf::Color::from_rgb(MODAL_SUB_COL, 255),
            );
        }

        // Optional footer — shrinks to fit too (an extra hint like "X: delete"
        // can push it past the panel width, especially in longer languages).
        if let Some(f) = footer {
            let fw_full = self.measure_text(f, MODAL_FOOTER_SCALE);
            let fscale = if fw_full > avail {
                MODAL_FOOTER_SCALE * avail / fw_full
            } else {
                MODAL_FOOTER_SCALE
            };
            let fw = self.measure_text(f, fscale);
            self.draw_text(
                x + (w - fw) * 0.5,
                y + h - 38.0,
                fscale,
                f,
                swf::Color::from_rgb(MODAL_FOOTER_COL, 255),
            );
        }

        // `h` stays local — consumed by the panel rect + footer position above;
        // callers lay their body out from y + fixed offsets, so it isn't returned.
        ModalFrame { x, y, w, pad_top }
    }

    /// Draw a vertical list of selectable rows inside a modal `frame`, with the
    /// shared ">" cursor + amber selection. A too-wide row is shrunk to fit.
    /// `selection == usize::MAX` draws no cursor (read-only lists).
    /// `key` distinguishes one list from another for the glide below: two lists
    /// sharing it would have the cursor slide across the gap between them
    /// instead of appearing where it belongs. Callers pass their modal kind.
    fn draw_modal_rows(&mut self, frame: &ModalFrame, selection: usize, rows: &[&str]) {
        // Which screen these rows belong to, for the touch table. Read from the
        // one place that knows -- `library::render` stamps it every frame --
        // rather than threaded through five call sites that would each have to
        // be told something they have no other use for.
        let touch_kind = ui_screen_kind();
        let left = frame.rows_left();
        let avail = frame.rows_avail();
        let top = frame.rows_top();
        // The cursor GLIDES between rows, and a bar travels with it.
        //
        // The `>` used to jump from row to row with nothing in between, in every
        // modal in the app, while REGLAGES right next door had had a gliding
        // highlight since v1.2.0. Same helper, same easing: the movement is now
        // the app's, not each screen's.
        //
        // `usize::MAX` is the "no selection" convention some callers use for a
        // read-only list; it must not be turned into a bar somewhere off-screen.
        if selection < rows.len() {
            let now = unsafe { ruffle_tick_now() };
            // The glide key is the SCREEN, not a number the caller picked.
            // `draw_library_options` alone is used by three different modals,
            // and they all passed the same constant, so the cursor flew in from
            // whichever one you had open last -- a row of a panel that is no
            // longer on screen. One key per screen is the honest mapping, and it
            // is already computed: it is the id the touch table is tagged with.
            //
            // It also snaps rather than glides on the frame a modal opens: the
            // stamp is 0 while the panel scales in, and a key change snaps. A
            // cursor that is simply THERE when the panel arrives is right.
            let hy = eased_list_y(top + selection as f32 * MODAL_ROW_H, touch_kind, now);
            let bar_x = left - MODAL_CURSOR_DX - 10.0;
            let bar_w = (frame.x + frame.w - 28.0 - bar_x).max(0.0);
            self.draw_selection_bar(bar_x, hy - 9.0, bar_w, MODAL_ROW_H - 12.0, 6.0);
            self.draw_text(
                left - MODAL_CURSOR_DX,
                hy,
                MODAL_ROW_SCALE,
                ">",
                swf::Color::from_rgb(MODAL_ROW_SEL_COL, 255),
            );
        }
        let mut cells: std::vec::Vec<(f32, f32, f32, f32)> =
            std::vec::Vec::with_capacity(rows.len());
        for (i, row) in rows.iter().enumerate() {
            let y = top + i as f32 * MODAL_ROW_H;
            let is_sel = i == selection;
            let color = swf::Color::from_rgb(
                if is_sel { MODAL_ROW_SEL_COL } else { MODAL_ROW_COL },
                255,
            );
            let w = self.measure_text(row, MODAL_ROW_SCALE);
            let sc = if w > avail { MODAL_ROW_SCALE * avail / w } else { MODAL_ROW_SCALE };
            self.draw_text(left, y, sc, row, color);
            // The whole width of the panel, not the width of the text: a row is
            // a target, and aiming at four letters with a thumb is not a target.
            cells.push((
                frame.x + 8.0,
                y - 10.0,
                (frame.w - 16.0).max(0.0),
                MODAL_ROW_H,
            ));
        }
        ui_cells_publish(touch_kind, cells);
    }

    /// Top navbar (v1.2.0) — tab strip switched with the L/R shoulder buttons.
    /// `active` indexes JOUER(0) / IMPORTER(1) / REGLAGES(2). Drawn last, over
    /// the top of every tab-home screen, by `library::render`.
    pub fn draw_navbar(&mut self, active: usize) {
        let vw = self.dimensions.width as f32;
        let lc = crate::loc::s();
        let tabs = [lc.tab_play, lc.tab_import, lc.tab_settings];

        let nav_y = 4.0_f32;
        let nav_h = 34.0_f32;
        // Background bar (semi-opaque dark navy) spanning the full width.
        let bar = Matrix {
            a: vw, b: 0.0, c: 0.0, d: nav_h,
            tx: swf::Twips::from_pixels(0.0),
            ty: swf::Twips::from_pixels(nav_y as f64),
        };
        <Self as CommandHandler>::draw_rect(self, swf::Color::from_rgba(0xE0_10_16_28), bar);

        // L / R chevrons at the edges — hint that the shoulders switch tabs.
        let edge_scale = 2.0;
        let label_y = nav_y + 8.0;
        let edge_col = swf::Color::from_rgb(0x88AACC, 255);
        self.draw_text(14.0, label_y, edge_scale, "L", edge_col);
        let r_w = self.measure_text("R", edge_scale);
        self.draw_text(vw - 14.0 - r_w, label_y, edge_scale, "R", edge_col);

        // Tab labels, centered as a group with even gaps.
        let scale = 2.0;
        let gap = 48.0;
        let widths = [
            self.measure_text(tabs[0], scale),
            self.measure_text(tabs[1], scale),
            self.measure_text(tabs[2], scale),
        ];
        let total: f32 = widths.iter().sum::<f32>() + gap * (tabs.len() as f32 - 1.0);
        let mut x = (vw - total) * 0.5;
        // Hit boxes: the full HEIGHT of the strip and half the gap on each side,
        // because a label two characters wide is not a target for a thumb.
        let mut cells = [(0.0f32, 0.0f32, 0.0f32, 0.0f32); 3];
        {
            let mut hx = x;
            for k in 0..tabs.len() {
                cells[k] = (hx - gap * 0.5, nav_y, widths[k] + gap, nav_h);
                hx += widths[k] + gap;
            }
        }
        navbar_publish(cells);
        for (i, t) in tabs.iter().enumerate() {
            let is_active = i == active;
            let color = if is_active {
                swf::Color::from_rgb(0xFFD740, 255)
            } else {
                swf::Color::from_rgb(0x99AABB, 255)
            };
            if is_active {
                // Underline the active tab.
                let ul = Matrix {
                    a: widths[i] + 8.0, b: 0.0, c: 0.0, d: 3.0,
                    tx: swf::Twips::from_pixels((x - 4.0) as f64),
                    ty: swf::Twips::from_pixels((nav_y + nav_h - 4.0) as f64),
                };
                <Self as CommandHandler>::draw_rect(self, swf::Color::from_rgb(0xFFD740, 255), ul);
            }
            self.draw_text(x, label_y, scale, t, color);
            x += widths[i] + gap;
        }

        unsafe {
            glUseProgram(0);
            glBindVertexArray(0);
        }
        self.gl_state.invalidate();
    }

    /// Small version label in the bottom-right corner of the launcher UI (drawn
    /// on the tab-home screens, after the navbar). The version string's single
    /// source of truth is `crate::bugreport::APP_VERSION`.
    pub fn draw_version_badge(&mut self) {
        let vw = self.dimensions.width as f32;
        let vh = self.dimensions.height as f32;
        let scale = 1.5;
        let color = swf::Color::from_rgb(0x66788A, 255);
        // Author credit, bottom-LEFT. Baked into the UI so a rebuilt/rebranded
        // copy carries it too (removing it is an active MIT-attribution
        // violation). ASCII only — the 5x7 font folds it.
        self.draw_text(14.0, vh - 22.0, scale, "Jonathan8520", color);
        // Version, bottom-RIGHT.
        let label = std::format!("V{}", crate::bugreport::APP_VERSION);
        let w = self.measure_text(&label, scale);
        self.draw_text(vw - w - 14.0, vh - 22.0, scale, &label, color);
        unsafe {
            glUseProgram(0);
            glBindVertexArray(0);
        }
        self.gl_state.invalidate();
    }

    /// Multi-file launch indicator (v1.3.0): a centered label near the bottom of
    /// the launch/loading reveal, e.g. "MULTI-FILE (6)", telling the user this
    /// game pulls in companion SWFs from its `.files/` folder. The caller gates
    /// when it shows (launch reveal only, once the cover fills the screen).
    pub fn draw_multifile_badge(&mut self, label: &str, count: i32) {
        let vw = self.dimensions.width as f32;
        let vh = self.dimensions.height as f32;
        let text = std::format!("{} ({})", label, count);
        let scale = 1.8;
        let tw = self.measure_text(&text, scale);
        self.draw_text(
            (vw - tw) * 0.5,
            vh - 48.0,
            scale,
            &text,
            swf::Color::from_rgb(0xFFFFFF, 255),
        );
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
        let (vw, vh) = (self.dimensions.width as f32, self.dimensions.height as f32);
        // TWO HORIZONTAL RULES, no sides.
        //
        // A box was the obvious shape and it was wrong. BANDE and ETAGERE scroll
        // SIDEWAYS and let their tiles run off both edges on purpose -- that
        // bleed is what says there is more to the left and right -- so vertical
        // borders cut straight through the artwork, and BANDE's position rail
        // sat outside the box it was supposed to be in.
        //
        // These two lines sit in the gaps every layout already leaves, so
        // nothing is ever crossed whatever scrolls past horizontally.
        //
        // The top one: the banner ends at y102 and the earliest content starts
        // at y124 (BANDE's hero), so y110 is clear in all four.
        //
        // The bottom one is the tight one, and GRILLE sets it: its facts line
        // ends at y660 and the footer starts at y678. y670 sits between them.
        // The footer cannot be pushed down to make more room -- the corner
        // stamps are six pixels under it -- so what little was needed came from
        // raising the name and the facts, as little as would do.
        const TOP_Y: f32 = 110.0;
        let bot_y = vh - 50.0;
        const M: f32 = 20.0;
        const B: f32 = 2.0;
        const ACCENT: u32 = 0xFF_FF_D7_40;
        unsafe {
            glEnable(GL_BLEND);
            glBlendFunc(GL_SRC_ALPHA, GL_ONE_MINUS_SRC_ALPHA);
        }
        // The plaque interrupts the top rule, so it is drawn in two pieces with
        // a gap for the text rather than a line through the word.
        let ls = 1.8;
        let label = self.fit_text(name, ls, vw - 200.0);
        let lw = self.measure_text(&label, ls);
        let gap_x0 = M + 14.0;
        let gap_x1 = gap_x0 + lw + 24.0;
        self.draw_overlay_rect(M, TOP_Y, (gap_x0 - M).max(0.0), B, ACCENT);
        self.draw_overlay_rect(gap_x1, TOP_Y, (vw - M - gap_x1).max(0.0), B, ACCENT);
        self.draw_overlay_rect(M, bot_y, vw - 2.0 * M, B, ACCENT);
        // An opaque plate under the word, in the page's own navy.
        //
        // The plaque sits at the very top of the scrolling band, so a cover
        // riding up under it turned amber text on a white logo into nothing at
        // all. A legend on a frame is not a caption over the content: it needs
        // its own ground, exactly like the gap it interrupts.
        let ly = TOP_Y - 7.0 * ls * 0.5 + B * 0.5;
        self.draw_overlay_rect(
            gap_x0,
            ly - 3.0,
            (gap_x1 - gap_x0).max(0.0),
            7.0 * ls + 6.0,
            0xFF_14_20_38,
        );
        self.draw_text(
            gap_x0 + 12.0,
            ly,
            ls,
            &label,
            swf::Color::from_rgb(0xFFD740, 255),
        );
        unsafe {
            glUseProgram(0);
            glBindVertexArray(0);
        }
        self.gl_state.invalidate();
    }

    /// Draw a cover in the box `(x, y, w, h)`, zoomed out by `t`.
    ///
    /// At `t = 0` the drawn rect IS the box, so `draw_textured_rect_cover` crops
    /// to fill it and the grid stays aligned. At `t = 1` the rect carries the
    /// image's own aspect and fits inside the box, so its UV remap degenerates
    /// to the whole image and NOTHING is cropped: the art is finally seen whole.
    /// In between it is one continuous zoom-out, never a switch between two
    /// modes — which is the difference between this and a tile that pops.
    ///
    /// Lifted out of ETAGERE, where it has always been what makes the selected
    /// shelf tile read as "this one". ETAGERE keeps its own copy: it adds a
    /// per-cover resting floor for art that would need blowing up past 1.25x to
    /// fill the box, which is a shelf concern and not a grid one.
    fn draw_cover_zoomed_out(
        &mut self,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        tex: GLuint,
        iw: u32,
        ih: u32,
        t: f32,
        alpha: f32,
    ) {
        if iw == 0 || ih == 0 {
            return;
        }
        let a = iw as f32 / ih as f32;
        let bx = w / h;
        let (fit_w, fit_h) = if a > bx { (w, w / a) } else { (h * a, h) };
        let t = t.clamp(0.0, 1.0);
        let dw = w + (fit_w - w) * t;
        let dh = h + (fit_h - h) * t;
        self.draw_textured_rect_cover(
            x + (w - dw) * 0.5,
            y + (h - dh) * 0.5,
            dw,
            dh,
            tex,
            iw,
            ih,
            alpha,
        );
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
                            CoverTex::Image { tex, w, h }
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
        if let Some(CoverTex::Image { tex, .. }) = evicted {
            if tex != 0 {
                unsafe { glDeleteTextures(1, &tex) };
            }
        }
        resolved
    }

    /// Resolve + cache a game's cover texture by basename. Decodes/uploads on
    /// first use; returns `Default` when there's no cover image (caller draws
    /// the generated tile).
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
                        CoverTex::Image { tex, w, h }
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
                                CoverTex::Image { tex, w: sw, h: sh }
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
        let vw = self.dimensions.width as f32;
        // Compact, fully below the navbar strip (y 4..38), scaled to a small
        // target height so it doesn't dominate the screen.
        let banner_y = 46.0;
        if banner_tex != 0 && banner_w > 0 && banner_h > 0 {
            // 56, not 72. The header band was 81% bare page colour: the banner is
            // 720×144 so the old target resolved to exactly 0.5 → 360×72, and the
            // count line sat alone on a full-width row under it. At 56 the logo
            // draws 280×56 and the count moves into the space beside it, which
            // gives the content band 30 px back without losing either element.
            let target_h = 56.0;
            let scale = (target_h / banner_h as f32).min((vw - 64.0) / banner_w as f32);
            let draw_w = banner_w as f32 * scale;
            let draw_h = banner_h as f32 * scale;
            let draw_x = (vw - draw_w) * 0.5;
            self.draw_textured_rect(draw_x, banner_y, draw_w, draw_h, banner_tex);
        } else {
            let title = "FLASHNX";
            let st = 3.0;
            let tw = self.measure_text(title, st);
            self.draw_text(
                (vw - tw) * 0.5,
                banner_y + 16.0,
                st,
                title,
                swf::Color::from_rgb(0xFFD740, 255),
            );
        }

        // Same slot whether filtering or not, so the header height never moves.
        // The open folder (issue #68) takes that same slot when no search is
        // running: it narrows the list exactly as a search does, and the player
        // is owed the same one line saying why they are not seeing everything.
        // A search wins the slot because a search also ignores the folder.
        let folder_open = HOME_FOLDER.lock().ok().and_then(|g| g.clone());
        let sub = match (filter, folder_open.as_deref()) {
            // BOTH, when both narrow. A search runs INSIDE the open shelf, so a
            // line naming only the search claims a scope it does not have: on a
            // six-game shelf it read "0 / 214 - FILTRE: sonic" and the player
            // concluded their library had no match, one ZR away from it.
            // `total_unfiltered` is the shelf's size, not the library's — see
            // the caller.
            // A search INSIDE a shelf: the denominator is the shelf, because
            // that is what the search looked at. The shelf's own name is on the
            // frame around the games, so it is not repeated here.
            (Some(f), Some(_)) if !f.trim().is_empty() => std::format!(
                "{} / {} - {}: {}",
                shown,
                total_unfiltered,
                crate::loc::s().files_filter,
                f,
            ),
            (Some(f), _) if !f.trim().is_empty() => {
                crate::loc::count_line(shown, total_unfiltered, filter, || {
                    crate::loc::games_count(shown)
                })
            }
            // A shelf with no search: against the WHOLE LIBRARY, which is the
            // only comparison left worth making. It used to read "85 / 85" --
            // the shelf measured against itself, two identical numbers that
            // answered nothing. "85 / 87" says how much of the library this is.
            (_, Some(_)) => std::format!(
                "{} / {}",
                shown,
                HOME_LIBRARY_TOTAL
                    .load(core::sync::atomic::Ordering::Relaxed)
                    .max(shown),
            ),
            // Nothing else on the home says folders exist, and the buttons that
            // walk them are on the back of the console. So the count line names
            // them, but ONLY while the library actually has one -- a hint for a
            // feature you are not using is noise on every other player's screen.
            _ if HOME_HAS_FOLDERS.load(core::sync::atomic::Ordering::Relaxed) => std::format!(
                "{}    ZL/ZR: {}",
                crate::loc::games_count(shown),
                crate::loc::s().home_folder,
            ),
            _ => crate::loc::games_count(shown),
        };
        // In the banner's LEFT flank, vertically centred in the 46..102 band,
        // rather than centred on its own row below. This line is also the only
        // indicator that a filter is active, so it stays visible in every layout —
        // it is not decoration that could be dropped to save the row.
        let ss = 1.8;
        // Truncated to the flank. Centred on its own row this line could not
        // collide with anything; beside the logo it can, and a long search term
        // ran its last characters straight over the banner's left edge — about
        // 25 characters was enough. The budget is x40 to the logo's edge less a
        // 16 px gutter, and the logo is drawn centred at 280 px wide.
        let logo_left = (vw - 280.0) * 0.5;
        let sub = self.fit_text(&sub, ss, logo_left - 16.0 - 40.0);
        self.draw_text(40.0, 67.7, ss, &sub, swf::Color::from_rgb(0xAABFD8, 255));

        // Right flank: the current layout, mirroring the count line (#98). This
        // is not the active sort that used to sit here and was removed as noise:
        // the sort is one Y away and named in the modal that sets it, while the
        // layout is changed from SETTINGS, two screens away, and the four
        // layouts name themselves nowhere else — you can see that a grid is a
        // grid, but not that it is called GRID.
        let view = crate::loc::home_view_label(crate::loc::home_view());
        let vwid = self.measure_text(view, ss);
        self.draw_text(
            vw - 40.0 - vwid,
            67.7,
            ss,
            view,
            swf::Color::from_rgb(0x7A8CA6, 255),
        );
    }

    /// Gold diamond on a dark chip, the favourite marker. The UI font has no star
    /// glyph, so the shape is drawn directly. Shared by every JOUER layout so a
    /// favourite is marked the same way wherever you look at it.
    fn draw_favorite_mark(&mut self, x: f32, y: f32, chip: f32) {
        let chip_m = Matrix {
            a: chip, b: 0.0, c: 0.0, d: chip,
            tx: swf::Twips::from_pixels(x as f64),
            ty: swf::Twips::from_pixels(y as f64),
        };
        <Self as CommandHandler>::draw_rect(self, swf::Color::from_rgba(0xB0_00_00_00), chip_m);
        // A unit square rotated 45 degrees by the matrix, centred on the chip.
        let cx = x + chip * 0.5;
        let cy = y + chip * 0.5;
        let sz = chip * 0.5;
        let cs = 0.70710678_f32;
        let diamond = Matrix {
            a: sz * cs, b: sz * cs, c: -sz * cs, d: sz * cs,
            tx: swf::Twips::from_pixels(cx as f64),
            ty: swf::Twips::from_pixels((cy - sz * cs) as f64),
        };
        <Self as CommandHandler>::draw_rect(self, swf::Color::from_rgb(0xFFD740, 255), diamond);
    }

    /// Filled rounded rectangle, built from horizontal slivers.
    ///
    /// Unlike `round_corners`, this draws the shape instead of masking a square
    /// one with the page colour, so it works over anything — a modal sits on a
    /// dimmed screenshot of the library, where painting a "background" colour into
    /// the corners would show the wrong thing.
    fn draw_round_rect(&mut self, x: f32, y: f32, w: f32, h: f32, r: f32, rgba: u32) {
        let r = r.min(w * 0.5).min(h * 0.5).max(0.0);
        if r <= 0.5 {
            self.draw_overlay_rect(x, y, w, h, rgba);
            return;
        }
        // Middle block, full width.
        self.draw_overlay_rect(x, y + r, w, h - 2.0 * r, rgba);
        // Caps: one sliver per row, inset by the circle's horizontal offset.
        let steps = r.ceil() as i32;
        for i in 0..steps {
            let dy = i as f32;
            let inset = r - (r * r - (r - dy) * (r - dy)).max(0.0).sqrt();
            let sw = w - 2.0 * inset;
            if sw <= 0.0 {
                continue;
            }
            self.draw_overlay_rect(x + inset, y + dy, sw, 1.0, rgba);
            self.draw_overlay_rect(x + inset, y + h - dy - 1.0, sw, 1.0, rgba);
        }
    }

    /// Round the corners of the rect just drawn at (x,y,w,h), by painting the
    /// four corner notches in the page colour.
    ///
    /// A cut-out rather than a rounded-rect shader: every cover here is an opaque
    /// textured quad on the launcher's flat background, so masking the corners is
    /// indistinguishable from rounding them and costs a handful of rects instead
    /// of a second bitmap program in the frame path. It DOES assume that flat
    /// background — this belongs to the library chrome, not to game rendering.
    fn round_corners(&mut self, x: f32, y: f32, w: f32, h: f32, r: f32) {
        // EXACTLY the page colour from `library_clear`: glClearColor(0.078, 0.125,
        // 0.219) is 0x14/0x20/0x38.
        self.round_corners_on(x, y, w, h, r, 0xFF_14_20_38);
    }

    /// Same, over a surface that is NOT the page.
    ///
    /// The notch colour has to be whatever is actually behind the rect, and that
    /// is not always the page: the cover picker clears with
    /// `draw_library_dim_backdrop` (0x0A0F1A) and lays each cell on 0xFF0B1222, so
    /// rounding its thumbnails with the page navy left four visibly lighter
    /// patches in every cell — the exact smudge this function's own comment warns
    /// about.
    fn round_corners_on(&mut self, x: f32, y: f32, w: f32, h: f32, r: f32, bg: u32) {
        let r = r.min(w * 0.5).min(h * 0.5);
        if r <= 0.5 {
            return;
        }
        let bg_col = bg;
        // One sliver per pixel row of the corner: the notch is the part of the
        // square outside the quarter circle, so its width shrinks as we descend.
        let steps = r.ceil() as i32;
        for i in 0..steps {
            let dy = i as f32;
            let inset = r - (r * r - (r - dy) * (r - dy)).max(0.0).sqrt();
            if inset <= 0.0 {
                continue;
            }
            let yt = y + dy;
            let yb = y + h - dy - 1.0;
            self.draw_overlay_rect(x, yt, inset, 1.0, bg_col);
            self.draw_overlay_rect(x + w - inset, yt, inset, 1.0, bg_col);
            self.draw_overlay_rect(x, yb, inset, 1.0, bg_col);
            self.draw_overlay_rect(x + w - inset, yb, inset, 1.0, bg_col);
        }
    }

    /// The amber wash painted behind the active row of a vertical list.
    ///
    /// Written out by hand on five screens, and the copies had already drifted:
    /// the IMPORTER history bar and the archive.org file bar are meant to read as
    /// the same object on the same 50 px pitch, yet one ends 4 px further right
    /// and sits 2 px lower than the other. Nobody chose that; it is what five
    /// independent `Matrix` literals cost. The tint was five literals too, spelled
    /// two different ways, so any future change to it - fading the bar under an
    /// open modal, a non-amber theme - was a five-site edit where missing one site
    /// stays invisible until someone opens that one screen.
    ///
    /// `radius > 0.0` cuts the corners, which paints the PAGE colour and is
    /// therefore only valid over a `library_clear` background, never on a modal.
    fn draw_selection_bar(&mut self, x: f32, y: f32, w: f32, h: f32, radius: f32) {
        // Amber at 20%: light enough that the row's own text stays the brightest
        // thing in it, which is why the marker is a wash and not a solid fill.
        const SEL_BAR_TINT: u32 = 0x33_FF_D7_40;
        self.draw_overlay_rect(x, y, w, h, SEL_BAR_TINT);
        if radius > 0.0 {
            self.round_corners(x, y, w, h, radius);
        }
    }

    /// Draw a game's cover fitted INSIDE the box (x,y,w,h), centred, aspect kept,
    /// and return the rect it actually occupies.
    ///
    /// No letterbox bars: a detail panel shows one cover at a time, so the frame
    /// can take the shape of the image instead of the image being forced into the
    /// frame. That is the whole reason these layouts tolerate cover art the grid
    /// cannot — and returning the rect lets the caller put the title right under
    /// the picture rather than under an oversized box with a hole in it.
    /// Falls back to the colour chip + initials, which fills a 4:3 slot.
    fn draw_cover_fitted(
        &mut self,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        caption_h: f32,
        basename: &str,
        display_name: &str,
        color_chip: u32,
    ) -> (f32, f32, f32, f32) {
        // Full resolution, like the launch reveal: these panels are large enough
        // that a tile thumbnail would show as mush.
        let (iw, ih, tex) = match self.cover_full_for(basename) {
            CoverTex::Image { tex, w: iw, h: ih } if iw > 0 && ih > 0 => (iw as f32, ih as f32, Some(tex)),
            _ => (4.0, 3.0, None),
        };
        // The art is fitted into the box MINUS the caption, not into the whole
        // box. Fitting into `h` meant a cover as tall as its panel took all of it
        // and the caption then landed entirely BELOW the box — in LISTE that put
        // the facts line at y668..680 straight through the footer at y678.
        // `caption_h` is documented as a reserve, so it has to be subtracted
        // before the fit and not merely used to bias the centring.
        let art_h = (h - caption_h).max(1.0);
        let box_aspect = w / art_h;
        let img_aspect = iw / ih;
        let (mut dw, mut dh) = if img_aspect > box_aspect {
            (w, w / img_aspect)
        } else {
            (art_h * img_aspect, art_h)
        };
        // Never blow a cover up more than 2x. Some logos are 156 px wide; filling
        // a 340 px box with one turns it to mush, and small-but-sharp reads better
        // than large-and-soft.
        if tex.is_some() && dw > iw * 2.0 {
            dw = iw * 2.0;
            dh = ih * 2.0;
        }
        // The ART PLUS ITS CAPTION is centred as one block, which is why the
        // caption's height is an argument. Centring the art alone dropped the
        // caption into the middle of nowhere; anchoring to the top just moved the
        // hole underneath. A short, wide cover cannot fill a tall panel, so the
        // leftover space is split evenly above and below, where it reads as
        // margin instead of as something missing.
        let dx = x + (w - dw) * 0.5;
        let dy = y + ((h - dh - caption_h) * 0.5).max(0.0);
        match tex {
            Some(t) => self.draw_textured_rect(dx, dy, dw, dh, t),
            None => {
                self.draw_overlay_rect(dx, dy, dw, dh, 0xFF_00_00_00 | color_chip);
                let initials: std::string::String = display_name.chars().take(3).collect();
                let isc = (dh / 36.0).clamp(3.0, 14.0);
                let tw = self.measure_text(&initials, isc);
                self.draw_text(
                    dx + (dw - tw) * 0.5,
                    dy + (dh - 7.0 * isc) * 0.5,
                    isc,
                    &initials,
                    swf::Color::from_rgb(0xFFFFFF, 255),
                );
            }
        }
        (dx, dy, dw, dh)
    }

    /// One-line facts about a game for the detail panels: what a tile never had
    /// room for. Playtime only appears once there is some, so a game you have
    /// never played shows nothing rather than a zero.
    /// The facts about a game, as coloured segments in draw order — the single
    /// source for all four JOUER layouts.
    ///
    /// There used to be TWO of these: a three-space string here, and a
    /// `//`-separated one with its own vocabulary ("SWF V10", a translated word
    /// after the playtime) written inline in the gallery. The same information,
    /// spelled two ways in two screens, which is what Jonathan noticed.
    ///
    /// Three things the old string could not say:
    ///
    /// - Version and compression are ONE group. They are not two facts, they are
    ///   one — what the file is — and the eye groups "SWF 10 CWS" whatever the
    ///   spacing, which is precisely why three spaces failed: a space INSIDE a
    ///   fact and spaces BETWEEN facts differ only in quantity, and quantity is
    ///   not something an eye ranks. Joined, a space means one thing and the bar
    ///   means the other.
    /// - The engine is ALWAYS named. The absence of "AS3" used to mean AVM1 —
    ///   information encoded as nothing, unreadable by definition — and it made
    ///   the line change its group COUNT from game to game. Now the flag changes
    ///   a group's CONTENT, so nothing ever moves sideways.
    /// - Groups are JOINED, never appended by hand. The old code pushed "   AS3"
    ///   onto the string, which is how an unknown size (Flashpoint hits, where
    ///   `format_size_pretty` returns "") produced a line starting with three
    ///   spaces that `measure_text` counted, throwing the centring off by 16 px
    ///   in the one screen where you import.
    fn game_facts(entry: &crate::library::Entry) -> std::vec::Vec<(std::string::String, u32)> {
        fn sep(out: &mut std::vec::Vec<(std::string::String, u32)>) {
            // Emitted by the JOIN, so a leading or orphan separator cannot happen.
            if !out.is_empty() {
                out.push((std::string::String::from(" | "), FACTS_MUTED));
            }
        }
        let mut out: std::vec::Vec<(std::string::String, u32)> = std::vec::Vec::with_capacity(8);

        let size = format_size_pretty(entry.size_bytes);
        if !size.is_empty() {
            out.push((size, FACTS_VALUE));
        }

        sep(&mut out);
        out.push((
            std::format!("SWF {} {}", entry.swf_version, entry.compression_label),
            FACTS_VALUE,
        ));

        // Named, not flagged. Both engines get the SAME colour as every other
        // fact on the line: the amber one said "careful with this game", and
        // that is no longer true of AS3 in particular.
        sep(&mut out);
        out.push((
            std::string::String::from(if entry.is_as3 { "AS3" } else { "AVM1" }),
            FACTS_VALUE,
        ));

        // The only genuinely optional group, and it is LAST: a game you have
        // never played has no playtime, it does not have a playtime of zero. Its
        // absence therefore cannot leave a hole or shift anything to its right.
        let secs = crate::playtime::get(&entry.basename);
        if secs >= 60 {
            sep(&mut out);
            out.push((
                std::format!("{}H{:02}", secs / 3600, (secs % 3600) / 60),
                FACTS_VALUE,
            ));
            // The word travels with the number: "0H02" alone does not say what it
            // counts. The gallery already had it and the other three did not; it
            // belongs in the muted voice, like a unit.
            out.push((std::format!(" {}", crate::loc::s().played_label), FACTS_MUTED));
        }
        out
    }

    /// Width of a facts line, so the centred layouts place it the way they
    /// already placed a plain string.
    fn facts_width(&self, facts: &[(std::string::String, u32)], scale: f32) -> f32 {
        facts.iter().map(|(t, _)| self.measure_text(t, scale)).sum()
    }

    /// Draw a facts line at `x`, `y` (top-left), one `draw_text` per segment.
    ///
    /// Segment by segment rather than two overlaid strings. The overlay trick is
    /// cheaper by a few calls, but it silently depends on both layers advancing
    /// identically — pure ASCII, non-ASCII last — and the day someone translates
    /// a label or inserts one in the middle, the layers drift apart with no error
    /// at all. Eight GL calls is on the order of 60 us against a 16.6 ms frame,
    /// which is the right price for an invariant that cannot quietly break.
    ///
    /// The pen is ROUNDED at draw time while the accumulator stays exact: the bar
    /// is two pixels of glyph, and an unrounded x rasterises it over one column or
    /// two depending on the fraction, so the three bars on one line would not come
    /// out the same thickness. On a pixel-art UI that is the loudest defect there is.
    fn draw_facts(
        &mut self,
        x: f32,
        y: f32,
        scale: f32,
        facts: &[(std::string::String, u32)],
    ) {
        let mut pen = x;
        for (text, color) in facts {
            self.draw_text(pen.round(), y, scale, text, swf::Color::from_rgb(*color, 255));
            pen += self.measure_text(text, scale);
        }
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
            self.draw_page_footer(crate::loc::s().list_footer);
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
            // Measured, not guessed. 88 was a fixed reserve for a caption that is
            // 64.6 px on one title line and 90.6 on two; since `draw_cover_fitted`
            // centres with `(h - dh - caption_h) * 0.5`, the common case pushed
            // the art 11.7 px up for nothing. Wrapped once here and reused below,
            // so the two cannot disagree either.
            let (l1, l2) = self.wrap_text_2(&e.display_name, 2.4, box_w);
            let lines = if l2.is_empty() { 1.0 } else { 2.0 };
            let caption_h = 18.0 + lines * 26.0 + 8.0 + 12.6;
            panel = self.draw_cover_fitted(
                box_x, box_y, box_w, box_h, caption_h,
                &e.basename, &e.display_name, e.color_chip,
            );
            self.round_corners(panel.0, panel.1, panel.2, panel.3, 8.0);
            if crate::favorites::is_favorite(&e.basename) {
                self.draw_favorite_mark(panel.0 + 6.0, panel.1 + 6.0, 26.0);
            }

            let mut ty = panel.1 + panel.3 + 18.0;
            let ts = 2.4;
            for line in [l1.as_str(), l2.as_str()] {
                if line.is_empty() {
                    continue;
                }
                let lw = self.measure_text(line, ts);
                self.draw_text(
                    box_x + ((box_w - lw) * 0.5).max(0.0),
                    ty,
                    ts,
                    line,
                    swf::Color::from_rgb(0xFFD740, 255),
                );
                ty += 26.0;
            }
            ty += 8.0;
            let facts = Self::game_facts(e);
            let fs = 1.8;
            let fw = self.facts_width(&facts, fs);
            self.draw_facts(box_x + ((box_w - fw) * 0.5).max(0.0), ty, fs, &facts);
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
            // Rounded only here: this bar is a narrow column over the page navy,
            // where a cut corner reads as deliberate. The full-width bars have no
            // margin to spend on one.
            self.draw_selection_bar(list_x, hl_y, list_w, ROW_H - 6.0, 6.0);
        }
        for (idx, e) in entries.iter().enumerate() {
            let y = TOP + idx as f32 * ROW_H - scroll_px;
            if y + ROW_H < band_top || y > band_bot {
                continue;
            }

            // Colour chip: the same per-game hue the grid falls back to, kept here
            // so a game stays recognisable between the two layouts.
            let chip = Matrix {
                a: 6.0, b: 0.0, c: 0.0, d: ROW_H - 14.0,
                tx: swf::Twips::from_pixels((list_x + 6.0) as f64),
                ty: swf::Twips::from_pixels((y + 4.0) as f64),
            };
            <Self as CommandHandler>::draw_rect(
                self, swf::Color::from_rgb(e.color_chip, 255), chip,
            );
            let col = if idx == selection {
                swf::Color::from_rgb(0xFFD740, 255)
            } else {
                swf::Color::from_rgb(0xCCCCCC, 255)
            };
            // Favourites also sort to the top, so the marker confirms WHY a row is
            // up there rather than announcing something new.
            let fav = crate::favorites::is_favorite(&e.basename);
            let text_x = if fav {
                self.draw_favorite_mark(list_x + 20.0, y + 6.0, ROW_H - 12.0);
                list_x + 20.0 + (ROW_H - 12.0) + 8.0
            } else {
                list_x + 22.0
            };
            // Truncated to the column, never to the panel beside it.
            let label = self.fit_text(&e.display_name, 2.0, list_x + list_w - text_x - 12.0);
            // y+10, not y+9: the 14 px glyph then centres on y+17, which is where
            // the highlight bar and the colour chip already centre. The text was
            // the only one of the three out of line.
            self.draw_text(text_x, y + 10.0, 2.0, &label, col);
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

        self.draw_page_footer(crate::loc::s().list_footer);

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
    fn draw_page_footer(&mut self, text: &str) {
        let vw = self.dimensions.width as f32;
        let vh = self.dimensions.height as f32;
        let w = self.measure_text(text, 2.0);
        self.draw_text((vw - w) * 0.5, vh - 42.0, 2.0, text, swf::Color::from_rgb(0x99AABB, 255));
    }

    /// Border width of the selection frame. It is drawn OUTSIDE the tile it
    /// marks, so a caller that rounds the corners afterwards has to feed the same
    /// number back in, which is why it is named instead of a `4.0` typed out four
    /// times per site and once more in the round_corners call.
    const SEL_FRAME_B: f32 = 4.0;

    /// The breathing amber cursor drawn around the selected tile of a grid.
    ///
    /// Three screens draw it - the JOUER grid, the JAQUETTE picker and the
    /// Flashpoint gallery - and each carried its own copy of the amber ramp and of
    /// the four border bars. They were already only ALMOST the same: the growth on
    /// a cursor move differs, and what gets rounded afterwards differs. That is
    /// exactly how a copy keeps the old look after the cursor is restyled and
    /// nobody notices for a whole release.
    fn draw_pulse_frame(&mut self, x: f32, y: f32, w: f32, h: f32, pulse: f32) {
        let p = (pulse * 0.5) + 0.5;
        let g = (0xC0 as f32 + (0xFF - 0xC0) as f32 * p) as u32;
        let col = swf::Color::from_rgb((0xFF << 16) | (g << 8) | 0x30, 255);
        let b = Self::SEL_FRAME_B;
        let bars = [
            (x - b, y - b, w + 2.0 * b, b), // top
            (x - b, y + h, w + 2.0 * b, b), // bottom
            (x - b, y, b, h),               // left
            (x + w, y, b, h),               // right
        ];
        for (bx, by, bw, bh) in bars {
            let m = Matrix {
                a: bw, b: 0.0, c: 0.0, d: bh,
                tx: swf::Twips::from_pixels(bx as f64),
                ty: swf::Twips::from_pixels(by as f64),
            };
            <Self as CommandHandler>::draw_rect(self, col, m);
        }
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
        let msg = crate::loc::s().cover_none;
        let sc = 2.5;
        let w = self.measure_text(msg, sc);
        let vw = self.dimensions.width as f32;
        self.draw_text(
            (vw - w) * 0.5,
            (band_top + band_bot) * 0.5 - 7.0 * sc * 0.5,
            sc,
            msg,
            swf::Color::from_rgb(0x7A8CA6, 255),
        );
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
        self.draw_overlay_rect(x, top, SCROLLBAR_W, h, SCROLLBAR_TRACK);
        self.draw_overlay_rect(x, top + (h - thumb_h) * t, SCROLLBAR_W, thumb_h, SCROLLBAR_THUMB);
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
            self.draw_page_footer(crate::loc::s().list_footer);
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
            hero = self.draw_cover_fitted(
                HERO_X, HERO_Y, HERO_W, HERO_H, 0.0,
                &e.basename, &e.display_name, e.color_chip,
            );
            self.round_corners(hero.0, hero.1, hero.2, hero.3, 10.0);
            if crate::favorites::is_favorite(&e.basename) {
                self.draw_favorite_mark(hero.0 + 8.0, hero.1 + 8.0, 28.0);
            }

            let mut ty = HERO_Y + 6.0;
            let ts = 2.8;
            // Three lines here, not two: the column is 580 px wide and the cover
            // row does not start until y468, so a long title had a third line's
            // worth of empty space under it and was ellipsised anyway.
            for line in self.wrap_text_n(&e.display_name, ts, vw - COL_X - 48.0, 3) {
                self.draw_text(COL_X, ty, ts, &line, swf::Color::from_rgb(0xFFD740, 255));
                ty += 32.0;
            }
            let facts = Self::game_facts(e);
            self.draw_facts(COL_X, ty + 14.0, 1.8, &facts);
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
            match cover {
                CoverTex::Image { tex, w, h } => {
                    self.draw_cover_zoomed_out(x, ry, rw, rh, tex, w, h, b, 1.0);
                }
                CoverTex::Default => {
                    self.draw_overlay_rect(x, ry, rw, rh, 0xFF_00_00_00 | e.color_chip);
                    let initials: std::string::String = e.display_name.chars().take(3).collect();
                    let isc = 3.0;
                    let iw = self.measure_text(&initials, isc);
                    self.draw_text(
                        x + (rw - iw) * 0.5,
                        ry + (rh - 7.0 * isc) * 0.5,
                        isc,
                        &initials,
                        swf::Color::from_rgb(0xFFFFFF, 255),
                    );
                }
            }
            self.round_corners(x, ry, rw, rh, 6.0);
            if crate::favorites::is_favorite(&e.basename) {
                self.draw_favorite_mark(x + 5.0, ry + 5.0, 18.0);
            }
            // Veil measured against the EASED selection, so it lifts and settles
            // with the slide instead of switching at the moment the index changes.
            let d = (i as f32 - sel_pos).abs().min(1.0);
            let alpha = (d * 0x70 as f32) as u32;
            if alpha > 0 {
                self.draw_overlay_rect(x, ry, rw, rh, (alpha << 24) | 0x0C_10_18);
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
            let fy = ROW_Y;
            let fw = ROW_W;
            let fh = ROW_H;
            const B: f32 = 3.0;
            const SEL: u32 = 0xFF_FF_D7_40;
            self.draw_overlay_rect(fx - B, fy - B, fw + 2.0 * B, B, SEL);
            self.draw_overlay_rect(fx - B, fy + fh, fw + 2.0 * B, B, SEL);
            self.draw_overlay_rect(fx - B, fy, B, fh, SEL);
            self.draw_overlay_rect(fx + fw, fy, B, fh, SEL);
            self.round_corners(fx - B, fy - B, fw + 2.0 * B, fh + 2.0 * B, 8.0);
        }

        // Position rail: 77 covers scrolled with nothing anywhere saying where in
        // them you are. Same idiom as the other horizontal layout, in the empty
        // band under the row.
        if total > 5 {
            let track_x = 56.0;
            let track_w = vw - 112.0;
            self.draw_overlay_rect(track_x, 620.0, track_w, 4.0, 0x55_2A_36_48);
            let tw = (track_w * (vw / pitch) / total as f32).max(56.0);
            let t = (sel_pos / (total - 1) as f32).clamp(0.0, 1.0);
            self.draw_overlay_rect(track_x + (track_w - tw) * t, 616.0, tw, 12.0, 0xFF_FF_D7_40);
        }

        self.draw_page_footer(crate::loc::s().list_footer);
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
            self.draw_page_footer(crate::loc::s().list_footer);
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
        const PLAQUE: u32 = 0xFF_0E_16_24;
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
                Some(CoverTex::Image { tex, w: iw, h: ih }) if iw > 0 && ih > 0 => {
                    // Neutral plaque, never the colour chip: the hero's backdrop
                    // must not change hue as the selection moves.
                    self.draw_overlay_rect(x, y, w, h, PLAQUE);
                    let a = iw as f32 / ih as f32;
                    let bx = W0 / H0;
                    let (fit_w, fit_h) = if a > bx { (w, w / a) } else { (h * a, h) };
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
                    let t = t_rest.max(b);
                    // At t=0 the rect IS the box, so `draw_textured_rect_cover`
                    // crops to fill; at t=1 the rect carries the image's aspect and
                    // its UV remap degenerates to the whole image. One continuous
                    // zoom-out, never a switch between two modes.
                    let dw = w + (fit_w - w) * t;
                    let dh = h + (fit_h - h) * t;
                    self.draw_textured_rect_cover(
                        x + (w - dw) * 0.5, y + (h - dh) * 0.5, dw, dh, tex, iw, ih,
                        1.0,
                    );
                }
                _ => {
                    self.draw_overlay_rect(x, y, w, h, 0xFF_00_00_00 | e.color_chip);
                    let initials: std::string::String = e.display_name.chars().take(3).collect();
                    let isc = 4.0 + 1.6 * b;
                    let tw = self.measure_text(&initials, isc);
                    self.draw_text(
                        x + (w - tw) * 0.5,
                        y + (h - 7.0 * isc) * 0.5,
                        isc,
                        &initials,
                        swf::Color::from_rgb(0xFFFFFF, 255),
                    );
                }
            }
            let veil = (0x66 as f32 * (i as f32 - sel_pos).abs().min(1.0)) as u32;
            if veil > 0 {
                self.draw_overlay_rect(x, y, w, h, (veil << 24) | VEIL);
            }
            // After the veil, so the notches end up page-coloured and not tinted.
            self.round_corners(x, y, w, h, 10.0 * sc);
            if crate::favorites::is_favorite(&e.basename) {
                self.draw_favorite_mark(x + 10.0 * sc, y + 10.0 * sc, 22.0 * sc);
            }
        }

        // ── Light under the shelf ──
        // A dim rule, then a lit segment per tile whose brightness and thickness
        // follow the same bump. At rest exactly one segment is lit under the active
        // cover; mid-slide two are half-lit and the light hands over. A fixed slot
        // bar would be the one element not derived from the eased value.
        self.draw_overlay_rect(24.0, 380.0, 1232.0, 2.0, 0xFF_22_30_40);
        for &i in &visible {
            let b = bump(i);
            if b <= 0.01 {
                continue;
            }
            let (x, _, w, _) = rect_of(i, b);
            let a = (b * 255.0) as u32;
            self.draw_overlay_rect(x, 380.0, w, 3.0 + 7.0 * b, (a << 24) | 0x00_FF_D7_40);
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
            let (l1, l2) = self.wrap_text_2(&e.display_name, 3.2, cap_w);
            // 414, not 388: the lit segment above is `3.0 + 7.0 * b` tall from
            // y=380, so at full selection it reaches y=390 and the title's first
            // pixel row landed INSIDE it -- the name looked stuck to the light
            // under the active cover. The clearance has to be measured against
            // the bar's GROWN height, not its resting 3 px, which is what made
            // this only show on the selected tile. The extra room past mere
            // clearance is free: the page had ~170 px of nothing between the rail
            // and the footer.
            let mut ty = 414.0;
            for line in [l1.as_str(), l2.as_str()] {
                if line.is_empty() {
                    continue;
                }
                self.draw_text(cap_x, ty, 3.2, line, swf::Color::from_rgb(0xFFD740, 255));
                ty += 34.0;
            }
            let facts = Self::game_facts(e);
            self.draw_facts(cap_x, ty + 12.0, 2.0, &facts);
        }

        // Position rail — the row shows about five games out of many, so where you
        // are in the library needs saying somewhere.
        if n > 5 {
            // Thin, and close under the text rather than stranded at the bottom of
            // the screen: it says where you are in the row, so it belongs with the
            // row's caption.
            // Follows the caption down. A two-line title ends its facts line at
            // y=506, so the old 503 would have been drawn THROUGH it.
            self.draw_overlay_rect(cap_x, 548.0, cap_w, 3.0, 0x55_2A_36_48);
            let tw = (cap_w * (1280.0 / PITCH) / n as f32).max(56.0);
            let tx = cap_x + (cap_w - tw) * (sel_pos / (n - 1) as f32).clamp(0.0, 1.0);
            self.draw_overlay_rect(tx, 547.0, tw, 5.0, 0xFF_FF_D7_40);
        }

        self.draw_page_footer(crate::loc::s().list_footer);
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
        let vh = self.dimensions.height as f32;
        let pulse = selection_pulse(phase_ticks);

        // Banner — compact, fully below the navbar strip (y 4..38). Scaled to a
        // small target height so it doesn't dominate the screen (was full 720x144).
        self.draw_home_header(banner_tex, banner_w, banner_h, entries.len(), filter, total_unfiltered);

        if entries.is_empty() {
            // GRILLE is the validated screen of the four; the guard is placed
            // after the header and before any tile work, so a non-empty library
            // reaches exactly the code it did before.
            self.draw_home_empty(126.0, 630.0);
            self.draw_page_footer(crate::loc::s().list_footer);
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
        let (cover_open, cover_close, cover_t) = grid_cover_phase(selection);
        for (idx, &(tx, tw, trow)) in tiles.iter().enumerate() {
            if trow < lo_row || trow > hi_row {
                continue;
            }
            let ty = TOP + trow as f32 * pitch - scroll_px;
            // Skip tiles fully outside the band (cheap reject before draw).
            if ty + ROW_IMG_H < band_top || ty > band_bot {
                continue;
            }
            let th = ROW_IMG_H;
            // How SELECTED this tile is, from the EASED frame rather than from
            // the index: the frame is already animated between the old cell and
            // the new one, so reading it gives the zoom-out below a continuous
            // 0..1 with no state of its own and no pop. One cell away = 0.
            // Exactly two tiles move: the one being selected opens, the one
            // being left folds back. Everything else is 0, whatever the
            // travelling frame passes over.
            let b = {
                let u = if idx == cover_open {
                    cover_t
                } else if idx == cover_close {
                    1.0 - cover_t
                } else {
                    0.0
                };
                u * u * (3.0 - 2.0 * u) // smoothstep, as ETAGERE's bump uses
            };
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

            match cover {
                CoverTex::Image { tex, w, h } if fade >= 1.0 => {
                    // Crop-to-fill the uniform cell (object-fit: cover) so the
                    // grid stays aligned regardless of the cover's native aspect.
                    self.draw_cover_zoomed_out(tx, ty, tw, th, tex, w, h, b, 1.0);
                }
                CoverTex::Image { tex, w, h } => {
                    // Mid-fade: the generated tile underneath, the cover over it.
                    let bg = Matrix {
                        a: tw, b: 0.0, c: 0.0, d: th,
                        tx: swf::Twips::from_pixels(tx as f64),
                        ty: swf::Twips::from_pixels(ty as f64),
                    };
                    <Self as CommandHandler>::draw_rect(
                        self,
                        swf::Color::from_rgb(entries[idx].color_chip, 255),
                        bg,
                    );
                    self.draw_cover_zoomed_out(tx, ty, tw, th, tex, w, h, b, fade);
                }
                CoverTex::Default => {
                    let bg = Matrix {
                        a: tw, b: 0.0, c: 0.0, d: th,
                        tx: swf::Twips::from_pixels(tx as f64),
                        ty: swf::Twips::from_pixels(ty as f64),
                    };
                    <Self as CommandHandler>::draw_rect(
                        self,
                        swf::Color::from_rgb(entries[idx].color_chip, 255),
                        bg,
                    );
                    let initials: std::string::String =
                        entries[idx].display_name.chars().take(3).collect();
                    let isc = 4.0;
                    let iw = self.measure_text(&initials, isc);
                    self.draw_text(
                        tx + (tw - iw) * 0.5,
                        ty + (th - 7.0 * isc) * 0.5,
                        isc,
                        &initials,
                        swf::Color::from_rgb(0xFFFFFF, 255),
                    );
                }
            }

            // Not the selected one: its frame is drawn after, and rounding the
            // tile here too would trap the tile's dark notches inside that frame.
            if idx != selection {
                self.round_corners(tx, ty, tw, th, 6.0);
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

            // Favorite marker, top-left. The UI font has
            // no "*"/star glyph, so we draw the shape directly: a gold diamond on
            // a dark chip (the chip gives contrast on bright covers). Favorites are
            // also pinned to the top of the gallery (library::sort_entries).
            if crate::favorites::is_favorite(&entries[idx].basename) {
                let chip = 24.0;
                let cx0 = tx + 4.0;
                let cy0 = ty + 4.0;
                let chip_m = Matrix {
                    a: chip, b: 0.0, c: 0.0, d: chip,
                    tx: swf::Twips::from_pixels(cx0 as f64),
                    ty: swf::Twips::from_pixels(cy0 as f64),
                };
                <Self as CommandHandler>::draw_rect(self, swf::Color::from_rgba(0xB0_00_00_00), chip_m);
                // Gold diamond = unit square rotated 45° via the matrix, centered
                // on the chip. (a+c)/2 = 0 so tx = center x; ty = center y - (b+d)/2.
                let cx = cx0 + chip * 0.5;
                let cy = cy0 + chip * 0.5;
                let sz = 12.0_f32;
                let cs = 0.70710678_f32; // cos/sin 45°
                let diamond = Matrix {
                    a: sz * cs, b: sz * cs, c: -sz * cs, d: sz * cs,
                    tx: swf::Twips::from_pixels(cx as f64),
                    ty: swf::Twips::from_pixels((cy - sz * cs) as f64),
                };
                <Self as CommandHandler>::draw_rect(self, swf::Color::from_rgb(0xFFD740, 255), diamond);
            }
        }

        // Single eased selection frame, drawn last and still inside the scissor
        // so it clips with its tile when partially scrolled. `pop` briefly
        // inflates it right after a move for a little tactile "snap"; `pulse`
        // keeps the existing breathing brightness.
        if !tiles.is_empty() {
            let grow = pop * 5.0;
            let fx = frame_x - grow;
            let fy = frame_y - scroll_px - grow;
            let fw = frame_w + 2.0 * grow;
            let fh = ROW_IMG_H + 2.0 * grow;
            self.draw_pulse_frame(fx, fy, fw, fh, pulse);
            // Rounded once, frame included, so the cursor matches the tiles it
            // travels between.
            let b = Self::SEL_FRAME_B;
            self.round_corners(fx - b, fy - b, fw + 2.0 * b, fh + 2.0 * b, 8.0);
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

        // Selected-game info line (name + size · version · engine).
        if let Some(entry) = entries.get(selection) {
            let nsc = 2.5;
            // Measured, not counted: the name gets the full screen width and
            // each character is charged what `draw_text` will actually advance.
            // A budget derived from a flat 6 px let a Chinese title run off both
            // ends of the screen.
            let name = self.fit_text_mid(&entry.display_name, nsc, vw - 60.0);
            let nw = self.measure_text(&name, nsc);
            self.draw_text(
                (vw - nw) * 0.5,
                // Raised from vh-96 to open the band under the facts line: the
                // shelf's bottom rule has to fit between them and the footer,
                // and the footer cannot move down -- the corner stamps sit six
                // pixels below it. The tile rows end at y588, so there is room
                // above and none below. Six pixels, no more than the rule needs.
                vh - 102.0,
                nsc,
                &name,
                swf::Color::from_rgb(0xFFFFFF, 255),
            );
            // The same formatter as the other three layouts. This screen used to
            // build its own string with "//" separators and "SWF V10" — the V says
            // nothing "SWF" does not.
            //
            // Raised from vh-66 with the name above it: this line used to end at
            // y668 against a footer starting at y678, ten pixels with the
            // shelf's bottom rule to fit inside. It now ends at y660, the rule
            // sits at y670, and the footer has not moved.
            let facts = Self::game_facts(entry);
            let isc = 2.0;
            let iw = self.facts_width(&facts, isc);
            self.draw_facts((vw - iw) * 0.5, vh - 74.0, isc, &facts);
        }

        self.draw_page_footer(crate::loc::s().list_footer);

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

    /// OPTIONS modal — small panel showing the game name + per-game options.
    /// v1: only TOUCHES + RETOUR.
    pub fn draw_library_options(
        &mut self,
        game_display_name: &str,
        selection: usize,
        options: &[&str],
    ) {
        let lc = crate::loc::s();
        let frame = self.draw_modal_frame(
            MODAL_W,
            options.len(),
            None,
            false,
            lc.options_title,
            Some(game_display_name),
            Some(lc.options_footer),
        );
        self.draw_modal_rows(&frame, selection, options);

        unsafe {
            glUseProgram(0);
            glBindVertexArray(0);
        }
        self.gl_state.invalidate();
    }

    /// Progress of the games-folder move (#79): a title, a bar, and a plain
    /// count of entries.
    ///
    /// Not the download panel, which this first reused: that one is headed
    /// TÉLÉCHARGEMENT, formats its numbers as bytes — "104 B / 111 B" for a
    /// hundred files — and offers a cancel that does not exist here, because a
    /// half-cancelled rename sweep is worse than a finished one.
    pub fn draw_library_move_progress(&mut self, title: &str, done: usize, total: usize) {
        self.library_clear();
        let vw = self.dimensions.width as f32;
        let vh = self.dimensions.height as f32;

        let scale_t = 4.0;
        let tw = self.measure_text(title, scale_t);
        self.draw_text(
            (vw - tw) * 0.5,
            vh * 0.30,
            scale_t,
            title,
            swf::Color::from_rgb(0xFFD740, 255),
        );

        // Bar. Empty at 0 of 0 rather than full: a move that has not started
        // should not look finished.
        const BAR_W: f32 = 720.0;
        const BAR_H: f32 = 26.0;
        let x = (vw - BAR_W) * 0.5;
        let y = vh * 0.48;
        let frac = if total == 0 {
            0.0
        } else {
            (done as f32 / total as f32).clamp(0.0, 1.0)
        };
        self.draw_selection_bar(x - 3.0, y - 3.0, BAR_W + 6.0, BAR_H + 6.0, 5.0);
        let fill = Matrix {
            a: BAR_W * frac, b: 0.0, c: 0.0, d: BAR_H,
            tx: swf::Twips::from_pixels(x as f64),
            ty: swf::Twips::from_pixels(y as f64),
        };
        <Self as CommandHandler>::draw_rect(self, swf::Color::from_rgb(0xFFD740, 255), fill);

        let label = std::format!("{} / {}", done, total);
        let scale_c = 2.0;
        let lw = self.measure_text(&label, scale_c);
        self.draw_text(
            (vw - lw) * 0.5,
            y + BAR_H + 26.0,
            scale_c,
            &label,
            swf::Color::from_rgb(0xCCCCCC, 255),
        );

        unsafe {
            glUseProgram(0);
            glBindVertexArray(0);
        }
        self.gl_state.invalidate();
    }

    /// Folder picker (RÉGLAGES → DOSSIER JEUX, #79).
    ///
    /// Its own entry point rather than `draw_library_options`, which titles every
    /// panel "OPTIONS" and prints a two-action footer. Neither fits here: the
    /// panel is not about a game, and the picker has a third action — create a
    /// folder — that nobody discovers unless it is written down.
    ///
    /// `danger` switches to the warning frame — used for the confirmation, which
    /// is about to rename a whole library and cannot be undone with B.
    pub fn draw_library_folder_picker(
        &mut self,
        title: &str,
        path: &str,
        // ABSOLUTE index into `rows`, not into the visible window.
        selection: usize,
        // The WHOLE list. The panel windows it itself so the rows can travel in
        // pixels; handing it a pre-cut slice is what made the listing change
        // wholesale under a cursor that had moved one line.
        rows: &[&str],
        footer: &str,
        danger: bool,
        // How many leading rows are ACTIONS rather than folders.
        actions: usize,
        // How many leading rows stay PINNED at the top instead of scrolling.
        // The directory tree pins "CHOISIR"/"REMONTER" — they act on the folder
        // you are in, so scrolling them away leaves no way to act on it.
        pinned: usize,
        // First scrolling row on screen, counted from `rows[pinned]`.
        scroll: usize,
        // Rows the panel is tall enough for, pinned ones included.
        visible: usize,
    ) {
        let pinned = pinned.min(rows.len());
        let n_scroll = rows.len() - pinned;
        let vis_scroll = visible.saturating_sub(pinned).min(n_scroll);
        let shown = pinned + vis_scroll;
        // The wide frame: folder names and paths are long, and the narrow panel
        // shrank them to fit rather than giving them room.
        let frame = self.draw_modal_frame(
            MODAL_W_WIDE,
            shown,
            None,
            danger,
            title,
            Some(path),
            Some(footer),
        );

        // Actions and folders are not the same kind of row, and reading them as
        // one list was the complaint: "CHOISIR" and "REMONTER" act on the folder
        // you are in, while the rest are places you can go. They get the accent
        // colour and a rule beneath them; the listing stays neutral grey.
        let left = frame.rows_left();
        let avail = frame.rows_avail();
        let top = frame.rows_top();
        let now = unsafe { ruffle_tick_now() };
        // Both glides are keyed on the PATH, because a glide is only honest
        // while the rows keep meaning the same thing. Walking into a folder
        // replaces every row at once; sliding across that change would show the
        // new listing streaking past under a cursor travelling to a row it was
        // never on. A new key snaps instead.
        let mut h: u32 = 2166136261;
        for b in path.as_bytes() {
            h ^= *b as u32;
            h = h.wrapping_mul(16777619);
        }
        // Top two bits set them apart from each other and from the small
        // hand-assigned keys the rest of the app uses.
        let key_cursor = 0x8000_0000 | (h & 0x00FF_FFFF);
        let key_scroll = 0xC000_0000 | (h & 0x00FF_FFFF);
        // Eased pixel offset for the scrolling part. The caller's integer stays
        // the source of truth; only the drawing catches up to it over a few
        // frames.
        let max_off = n_scroll.saturating_sub(vis_scroll) as f32 * MODAL_ROW_H;
        let scroll_off = eased_scroll_px(
            (scroll as f32 * MODAL_ROW_H).min(max_off),
            key_scroll,
            now,
        );
        // Where a row ends up on screen: pinned rows sit still, the rest ride
        // the offset below them.
        let row_y = |i: usize| -> f32 {
            if i < pinned {
                top + i as f32 * MODAL_ROW_H
            } else {
                top + i as f32 * MODAL_ROW_H - scroll_off
            }
        };
        let band_top = top + pinned as f32 * MODAL_ROW_H - MODAL_ROW_H * 0.34;
        let band_bot = top + shown as f32 * MODAL_ROW_H - MODAL_ROW_H * 0.34;
        // Gliding bar, under the rows, same as every other list in the app.
        if selection < rows.len() {
            let hy = eased_list_y(row_y(selection), key_cursor, now);
            let bar_x = left - MODAL_CURSOR_DX - 10.0;
            let bar_w = (frame.x + frame.w - 28.0 - bar_x).max(0.0);
            // Clipped for a scrolling row so the bar cannot outrun the band while
            // it eases; a pinned row is never outside it.
            if selection >= pinned {
                self.set_clip(frame.x, band_top, frame.w, band_bot - band_top);
            }
            self.draw_selection_bar(bar_x, hy - 9.0, bar_w, MODAL_ROW_H - 12.0, 6.0);
            if selection >= pinned {
                self.clear_clip();
            }
        }
        // Absolute index space, and only for the rows actually inside the panel:
        // an off-screen row must not take a tap aimed at the row that replaced it.
        let mut cells: std::vec::Vec<(f32, f32, f32, f32)> =
            std::vec![(0.0, 0.0, 0.0, 0.0); rows.len()];
        let first = pinned + (scroll_off / MODAL_ROW_H).floor().max(0.0) as usize;
        let last = (first + vis_scroll + 2).min(rows.len());
        // The scrolling part only. `total`/`visible` are counted in the same
        // space as `scroll`, i.e. from `rows[pinned]`, so the caller gets back an
        // offset it can store without translating it.
        row_view_publish(RowView {
            key: key_scroll,
            kind: ui_screen_kind(),
            band_top,
            band_bot,
            row_h: MODAL_ROW_H,
            scroll_px: scroll_off,
            max_off,
            total: n_scroll as u32,
            visible: vis_scroll as u32,
            base: pinned as u32,
        });
        let mut draw_row = |s: &mut Self, i: usize| {
            let y = row_y(i);
            let is_sel = i == selection;
            let is_action = i < actions;
            let color = swf::Color::from_rgb(
                if is_sel {
                    MODAL_ROW_SEL_COL
                } else if is_action {
                    0xE8C36A // dimmer amber: an action, but not the cursor
                } else {
                    MODAL_ROW_COL
                },
                255,
            );
            if i < pinned || (y >= band_top - 1.0 && y + MODAL_ROW_H <= band_bot + MODAL_ROW_H * 0.34)
            {
                cells[i] = (
                    frame.x + 8.0,
                    y - 10.0,
                    (frame.w - 16.0).max(0.0),
                    MODAL_ROW_H,
                );
            }
            if is_sel {
                s.draw_text(left - MODAL_CURSOR_DX, y, MODAL_ROW_SCALE, ">", color);
            }
            let row = rows[i];
            let w = s.measure_text(row, MODAL_ROW_SCALE);
            let sc = if w > avail { MODAL_ROW_SCALE * avail / w } else { MODAL_ROW_SCALE };
            s.draw_text(left, y, sc, row, color);
        };
        for i in 0..pinned {
            draw_row(self, i);
        }
        self.set_clip(frame.x, band_top, frame.w, band_bot - band_top);
        for i in first..last {
            draw_row(self, i);
        }
        self.clear_clip();
        ui_cells_publish(ui_screen_kind(), cells);
        // Separator, only when there is something on both sides of it.
        if actions > 0 && rows.len() > actions {
            let rule_y = row_y(actions) - MODAL_ROW_H * 0.30;
            // A rule that belongs to a scrolling row travels with it, so it only
            // draws while that row is inside the band.
            if actions <= pinned || (rule_y >= band_top && rule_y <= band_bot) {
                let rule = Matrix {
                    a: avail, b: 0.0, c: 0.0, d: 1.0,
                    tx: swf::Twips::from_pixels(left as f64),
                    ty: swf::Twips::from_pixels(rule_y as f64),
                };
                <Self as CommandHandler>::draw_rect(self, swf::Color::from_rgba(0x60_99_AA_BB), rule);
            }
        }

        unsafe {
            glUseProgram(0);
            glBindVertexArray(0);
        }
        self.gl_state.invalidate();
    }

    /// Per-URL options modal (IMPORTER, `+` on a row). Same frame as the
    /// per-game OPTIONS modal, but with an info block above the actions: the
    /// row's short label alone doesn't tell you which URL you're about to edit
    /// or delete, nor when you saved it. `info` lines are pre-formatted
    /// `LABEL : value` pairs; the full URL goes last (wrapped, dimmer) since
    /// it's the long one.
    pub fn draw_library_url_options(
        &mut self,
        title: &str,
        selection: usize,
        options: &[&str],
        info: &[std::string::String],
        url: &str,
        footer: &str,
    ) {
        const INFO_SCALE: f32 = 1.6;
        let info_line_h = 7.0 * INFO_SCALE + 9.0;
        let mut url_lines = wrap_words(url, MODAL_W_WIDE - 80.0, INFO_SCALE);
        // Two lines of URL is plenty to recognise it; more would push the
        // actions off the panel.
        if url_lines.len() > 2 {
            url_lines.truncate(2);
            if let Some(last) = url_lines.last_mut() {
                last.push('…');
            }
        }
        let block_h = (info.len() + url_lines.len()) as f32 * info_line_h + 18.0;
        // Reserve the info block by asking the frame for extra "rows" worth of
        // height, then draw the real rows below it.
        let extra_rows = (block_h / MODAL_ROW_H).ceil() as usize;
        let frame = self.draw_modal_frame(
            MODAL_W_WIDE,
            options.len() + extra_rows,
            None,
            false,
            title,
            None,
            Some(footer),
        );
        let left = frame.rows_left();
        let mut y = frame.rows_top();
        for line in info {
            self.draw_text(left, y, INFO_SCALE, line, swf::Color::from_rgb(0xCCCCCC, 255));
            y += info_line_h;
        }
        for line in &url_lines {
            self.draw_text(left, y, INFO_SCALE, line, swf::Color::from_rgb(0x8899AA, 255));
            y += info_line_h;
        }
        // Actions, offset past the reserved block.
        let rows_top = frame.rows_top() + extra_rows as f32 * MODAL_ROW_H;
        let avail = frame.rows_avail();
        for (i, row) in options.iter().enumerate() {
            let ry = rows_top + i as f32 * MODAL_ROW_H;
            let is_sel = i == selection;
            let color = swf::Color::from_rgb(
                if is_sel { MODAL_ROW_SEL_COL } else { MODAL_ROW_COL },
                255,
            );
            if is_sel {
                self.draw_text(left - MODAL_CURSOR_DX, ry, MODAL_ROW_SCALE, ">", color);
            }
            let w = self.measure_text(row, MODAL_ROW_SCALE);
            let sc = if w > avail { MODAL_ROW_SCALE * avail / w } else { MODAL_ROW_SCALE };
            self.draw_text(left, ry, sc, row, color);
        }

        unsafe {
            glUseProgram(0);
            glBindVertexArray(0);
        }
        self.gl_state.invalidate();
    }

    /// Generic centered list modal (title + subtitle + rows + footer), same
    /// look as `draw_library_options` but with the strings passed in. Used by
    /// the community-profile picker (#20). `subtitle` may be empty. `wide` picks
    /// the 720 tier for content-heavy lists (profile names, before/after diffs)
    /// that look cramped at the standard 520; short lists pass `false`.
    pub fn draw_library_list_modal(
        &mut self,
        title: &str,
        subtitle: &str,
        selection: usize,
        options: &[&str],
        footer: &str,
        wide: bool,
    ) {
        let sub = if subtitle.is_empty() { None } else { Some(subtitle) };
        let frame = self.draw_modal_frame(
            if wide { MODAL_W_WIDE } else { MODAL_W },
            options.len(),
            None,
            false,
            title,
            sub,
            Some(footer),
        );
        self.draw_modal_rows(&frame, selection, options);

        unsafe {
            glUseProgram(0);
            glBindVertexArray(0);
        }
        self.gl_state.invalidate();
    }

    /// Destructive-confirm modal for OPTIONS > SUPPRIMER. Bigger / redder
    /// than `draw_library_options` because the action is irreversible.
    pub fn draw_library_delete_confirm(
        &mut self,
        game_display_name: &str,
        basename: &str,
    ) {
        // Fixed-height danger frame (red theme, amber title + shared footer).
        // Wide tier — the warning lines are long.
        let lc = crate::loc::s();
        let frame = self.draw_modal_frame(
            MODAL_W_WIDE,
            0,
            Some(360.0),
            true,
            lc.del_title,
            None,
            Some(lc.del_footer),
        );

        // Game name + basename, centered under the title.
        const NAME_SCALE: f32 = 2.5;
        // Width, not a character count: this is the modal that asks "delete
        // this?", so the name spilling outside the frame is the worst possible
        // place for it. See `fit_text_mid`.
        let display = self.fit_text_mid(game_display_name, NAME_SCALE, frame.w - MODAL_ROW_X);
        let dw = self.measure_text(&display, NAME_SCALE);
        self.draw_text(
            frame.x + (frame.w - dw) * 0.5,
            frame.y + 105.0,
            NAME_SCALE,
            &display,
            swf::Color::from_rgb(0xFFFFFF, 255),
        );

        const SUB_SCALE: f32 = 1.5;
        let bn = std::format!("[{}]", basename);
        let bnw = self.measure_text(&bn, SUB_SCALE);
        self.draw_text(
            frame.x + (frame.w - bnw) * 0.5,
            frame.y + 145.0,
            SUB_SCALE,
            &bn,
            swf::Color::from_rgb(0xCCAAAA, 255),
        );

        // Three warning lines (the last is red — irreversible).
        const WARN_SCALE: f32 = 2.0;
        for (i, (line, col)) in [
            (lc.del_l1, 0xFFEEDDu32),
            (lc.del_l2, 0xFFEEDD),
            (lc.del_l3, 0xFF9090),
        ]
        .iter()
        .enumerate()
        {
            let w = self.measure_text(line, WARN_SCALE);
            self.draw_text(
                frame.x + (frame.w - w) * 0.5,
                frame.y + 195.0 + i as f32 * 30.0,
                WARN_SCALE,
                line,
                swf::Color::from_rgb(*col, 255),
            );
        }

        unsafe {
            glUseProgram(0);
            glBindVertexArray(0);
        }
        self.gl_state.invalidate();
    }

    // ── Phase 3.7: DISTANT mode screens ────────────────────────────────

    /// IMPORTER tab — the saved-URL list. Row 0 is the "+ add" row (PINNED
    /// first, so it stays reachable however long the history gets); rows 1.. are
    /// `labels`/`hosts`/`installed`, already filtered + sorted by the caller.
    /// `selection` is a ROW index. A = launch (or add a URL), + = per-URL
    /// options, Y = sort, - = search.
    pub fn draw_library_distant_list(
        &mut self,
        selection: usize,
        // Readable name per URL (item id / file name) — the raw URL is unusable
        // as a row label past a couple of entries.
        labels: &[&str],
        // Host tag drawn dimmed to the right of each label ("" = none).
        hosts: &[&str],
        // True = a direct `.swf` (one file, A downloads it) vs an archive.org
        // item (A opens its file list). They behave differently on A, so the row
        // says which it is instead of leaving the user to guess from the URL.
        direct: &[bool],
        // (files on SD, total files) per URL. `None` total = never opened, so
        // the count is unknown and the row shows nothing rather than a guess.
        progress: &[(u32, Option<u32>)],
        // Favorited URLs: gold diamond, and pinned to the top by the caller —
        // same treatment a favorited game gets in the JOUER gallery.
        favorite: &[bool],
        // The two pinned action rows, in the order they are drawn.
        search_label: &str,
        add_label: &str,
        // Topmost visible row. A real value from the screen state (not derived
        // from `selection`) so a touch drag can scroll without moving the cursor.
        scroll_offset: usize,
        // Active search, echoed in the sub-line with the match count.
        filter: Option<&str>,
        // URL count BEFORE filtering, for the "3 / 21" sub-line.
        total_unfiltered: usize,
        // True only when this is the ACTIVE IMPORTER home (not a reveal-window
        // underlay): then it drives the shared scroll/hover animation and
        // publishes row geometry for the touch layer. An underlay must do
        // NEITHER — the file list drawn on top owns those singletons that frame.
        interactive: bool,
    ) {
        self.library_clear();
        let vw = self.dimensions.width as f32;

        // Header.
        let header = crate::loc::s().dist_title;
        let hs = 4.0;
        let hw = self.measure_text(header, hs);
        self.draw_text(
            (vw - hw) * 0.5,
            70.0,
            hs,
            header,
            swf::Color::from_rgb(0xFFD740, 255),
        );

        // Sub-line: "3 / 21 - FILTRE: mario" while searching, else the count —
        // with a long history you need to know how much of it you're looking at.
        let lc = crate::loc::s();
        // With nothing saved yet, "0" is a fact nobody needed. The line says what
        // the two rows below it are for instead -- which this page has not done
        // since it stopped being an archive.org URL box, and the strings written
        // for that job went unread in loc.rs ever since.
        let empty = total_unfiltered == 0 && filter.map_or(true, |f| f.trim().is_empty());
        let sub = if empty {
            std::borrow::Cow::Borrowed(lc.dist_empty_hint)
        } else {
            std::borrow::Cow::Owned(crate::loc::count_line(
                labels.len(),
                total_unfiltered,
                filter,
                || lc.dist_count.replace("{}", &total_unfiltered.to_string()),
            ))
        };
        // Shrunk to fit rather than truncated: the hint is a sentence, and a
        // sentence cut in half is worse than a small one. It is the longest thing
        // this header ever draws, and longest of all in German.
        let sub_scale = {
            let full = self.measure_text(&sub, 2.0);
            if full > vw - 80.0 { 2.0 * (vw - 80.0) / full } else { 2.0 }
        };
        let sub_w = self.measure_text(&sub, sub_scale);
        self.draw_text(
            (vw - sub_w) * 0.5,
            118.0,
            sub_scale,
            &sub,
            swf::Color::from_rgb(0xAABFD8, 255),
        );

        let total = labels.len() + crate::library::IMPORTER_PINNED_ROWS;
        // 10 rows: the last one ends at 660, clear of the footer at vh-42. Taken
        // from `library`, not re-typed: the scroll clamp and the reveal box are
        // computed there from the same three numbers, and a comment asking two
        // files to stay in step is not a mechanism.
        const VISIBLE: usize = crate::library::IMPORTER_VISIBLE_ROWS;
        let row_h = crate::library::IMPORTER_ROW_H;
        let top = crate::library::IMPORTER_ROW_TOP;
        let left = 80.0;
        let scale = 2.0;

        // Smooth scroll + gliding hover, same machinery as the archive.org file
        // list and the JOUER gallery (shared anim/view/cache singletons — only
        // one of those screens renders per frame). This list used to PAGE, which
        // is why it alone felt like it snapped: the rows now ease toward
        // `scroll_offset` and the highlight bar slides between rows.
        // The rule only exists when there is something under it, so on a fresh
        // library nothing shifts and the page is simply two rows.
        let gap = if labels.is_empty() {
            0.0
        } else {
            crate::library::IMPORTER_SECTION_GAP
        };
        // Content-space y of row `i`. One pitch throughout, plus a constant step
        // for the rows below the rule -- the drag-scroll and the touch cells are
        // built on the pitch, so it must stay the same on both sides.
        let row_y = |i: usize| {
            top + i as f32 * row_h
                + if i >= crate::library::IMPORTER_PINNED_ROWS { gap } else { 0.0 }
        };
        let band_top = top - 8.0;
        let band_bot = top + VISIBLE as f32 * row_h + gap;
        let target_scroll = scroll_offset as f32 * row_h;
        let target_hover = row_y(selection);
        let mut scroll_px = target_scroll;
        let mut hover_y = target_hover;
        // An underlay must not touch the animation: the file list drawn over it
        // owns these singletons for this frame.
        if interactive {
            let touch_scroll = gallery_touch_scroll_read();
            if let Ok(mut a) = gallery_anim().lock() {
                let now = unsafe { ruffle_tick_now() };
                if !a.inited {
                    a.inited = true;
                    a.last_tick = now;
                    a.last_sel = selection;
                    a.scroll_px = target_scroll;
                    a.sel_x = 0.0;
                    a.sel_y = target_hover;
                    a.sel_w = 0.0;
                    a.pop = 0.0;
                } else {
                    let freq = unsafe { ruffle_tick_freq() } as f32;
                    let dt = if freq > 0.0 {
                        (now.saturating_sub(a.last_tick) as f32 / freq).min(0.1)
                    } else {
                        1.0 / 60.0
                    };
                    a.last_tick = now;
                    a.last_sel = selection;
                    a.scroll_px = ease_to(a.scroll_px, target_scroll, dt, 16.0);
                    a.sel_y = ease_to(a.sel_y, target_hover, dt, 18.0);
                }
                // A finger on the screen overrides the eased value 1:1.
                if let Some(px) = touch_scroll {
                    a.scroll_px = px;
                }
                scroll_px = a.scroll_px;
                hover_y = a.sel_y;
            }
        }

        // Clip the rows to their band so a mid-glide row can't bleed over the
        // sub-line or the footer.
        self.set_clip(0.0, band_top, vw, band_bot - band_top);

        let hy = hover_y - scroll_px;
        let bar_x = left - 40.0;
        // Radius 6, like JOUER's list: this screen draws straight onto the page
        // (`library_clear`), so `round_corners` paints the right colour into the
        // notches. The square bar was the odd one out of the four list screens.
        self.draw_selection_bar(bar_x, hy - 6.0, vw - bar_x - 56.0, row_h - 12.0, 6.0);
        self.draw_text(left - 34.0, hy, scale, ">", swf::Color::from_rgb(0xFFD740, 255));

        for i in 0..total {
            let y = row_y(i) - scroll_px;
            // Cheap cull: skip rows fully outside the band.
            if y + row_h < band_top - 8.0 || y > band_bot + 8.0 {
                continue;
            }
            let is_sel = i == selection;
            let color = if is_sel {
                swf::Color::from_rgb(0xFFD740, 255)
            } else {
                swf::Color::from_rgb(0xCCCCCC, 255)
            };
            if i < crate::library::IMPORTER_PINNED_ROWS {
                // The two actions — teal when not selected so they stand out
                // from the URLs below. One colour for both: they are one kind of
                // thing (do something) against another (a source you saved).
                let c = if is_sel {
                    color
                } else {
                    swf::Color::from_rgb(0x88CC99, 255)
                };
                let lbl = if i == 0 { search_label } else { add_label };
                self.draw_text(left, y, scale, lbl, c);
                // The search row carries a source tag in the same column and the
                // same dimmed style a URL row uses for its host, so "where does
                // this come from" is answered the same way down the whole list.
                // Not localised: it is the name of the project.
                {
                    let src = if i == 0 { "FLASHPOINT" } else { "ARCHIVE.ORG" };
                    let tw = self.measure_text(src, 1.5);
                    self.draw_text(
                        vw - 60.0 - tw,
                        y + 5.0,
                        1.5,
                        src,
                        swf::Color::from_rgb(0x66788C, 255),
                    );
                }
                continue;
            }
            let k = i - crate::library::IMPORTER_PINNED_ROWS;
            // Right-hand metadata first (host, then count), so the label knows
            // how much room is left before it truncates.
            let mut right = vw - 60.0;
            let host = hosts.get(k).copied().unwrap_or("");
            if !host.is_empty() {
                let hw = self.measure_text(host, 1.5);
                self.draw_text(
                    right - hw,
                    y + 5.0,
                    1.5,
                    host,
                    swf::Color::from_rgb(0x66788C, 255),
                );
                right -= hw + 20.0;
            }
            // "4/13" = how much of this source is already on SD. Green once it's
            // complete, amber while partial, grey at zero. This replaces the old
            // all-or-nothing OK badge, which only ever lit up at 100% and so said
            // nothing at all about a 13-file archive.org item.
            let count = match progress.get(k).copied() {
                Some((have, Some(total))) => Some((
                    std::format!("{}/{}", have, total),
                    if total > 0 && have >= total {
                        swf::Color::from_rgb(0x66DD66, 255)
                    } else if have > 0 {
                        swf::Color::from_rgb(0xFFB740, 255)
                    } else {
                        swf::Color::from_rgb(0x778899, 255)
                    },
                )),
                // Total unknown (never opened) but files from it ARE on SD:
                // still worth saying so, with the total left as "?".
                Some((have, None)) if have > 0 => Some((
                    std::format!("{}/?", have),
                    swf::Color::from_rgb(0xFFB740, 255),
                )),
                _ => None,
            };
            if let Some((txt, c)) = count {
                let cw = self.measure_text(&txt, 1.6);
                self.draw_text(right - cw, y + 4.0, 1.6, &txt, c);
                right -= cw + 20.0;
            }
            // Type tag: a direct `.swf` downloads on A, an item opens a file
            // list. Fixed slot on the left so the labels stay aligned.
            let is_direct = direct.get(k).copied().unwrap_or(false);
            let (tag, tag_c) = if is_direct {
                ("SWF", swf::Color::from_rgb(0x7FB3FF, 255))
            } else {
                ("LIST", swf::Color::from_rgb(0xB08CE0, 255))
            };
            self.draw_text(left, y + 2.0, 1.6, tag, tag_c);
            let mut ux = left + self.measure_text("LIST", 1.6) + 12.0;
            // Favorite marker: the same gold diamond the gallery puts on a
            // favorited cover (the bitmap font has no star glyph).
            if favorite.get(k).copied().unwrap_or(false) {
                let sz = 11.0_f32;
                let cs = 0.70710678_f32; // cos/sin 45°
                let diamond = Matrix {
                    a: sz * cs, b: sz * cs, c: -sz * cs, d: sz * cs,
                    tx: swf::Twips::from_pixels((ux + sz * 0.5) as f64),
                    ty: swf::Twips::from_pixels((y + row_h * 0.5 - 12.0 - sz * cs) as f64),
                };
                <Self as CommandHandler>::draw_rect(
                    self,
                    swf::Color::from_rgb(0xFFD740, 255),
                    diamond,
                );
                ux += sz + 12.0;
            }
            let shown = self.fit_text_mid(labels[k], scale, right - ux);
            self.draw_text(ux, y, scale, &shown, color);
        }

        // The rule between the two halves: what you can do, and what you have
        // saved. Drawn inside the clip and in content space, so it travels with
        // the rows instead of hanging in the header when the list scrolls.
        if !labels.is_empty() {
            let sep_y = top + crate::library::IMPORTER_PINNED_ROWS as f32 * row_h
                + (gap - 24.0) * 0.5
                - scroll_px;
            if sep_y > band_top && sep_y < band_bot {
                const RULE_COL: u32 = 0xFF_364356;
                let head = lc.dist_sources;
                let hw = self.measure_text(head, 1.5);
                let cx = vw * 0.5;
                let x0 = left - 40.0;
                let x1 = vw - 56.0;
                // Broken around the heading, like a legend on a frame: an
                // unbroken rule with a word floating over it reads as two things
                // that happen to overlap.
                self.draw_overlay_rect(x0, sep_y, (cx - hw * 0.5 - 16.0 - x0).max(0.0), 2.0, RULE_COL);
                let right_x = cx + hw * 0.5 + 16.0;
                self.draw_overlay_rect(right_x, sep_y, (x1 - right_x).max(0.0), 2.0, RULE_COL);
                self.draw_text(
                    cx - hw * 0.5,
                    sep_y - 5.0,
                    1.5,
                    head,
                    swf::Color::from_rgb(0x8FA3BC, 255),
                );
            }
        }

        self.clear_clip();

        // A search that matched nothing would otherwise be a blank page with a
        // lone "+ add" row — say so.
        if labels.is_empty() && filter.map_or(false, |f| !f.trim().is_empty()) {
            let none = lc.cover_none;
            let nw = self.measure_text(none, 2.0);
            self.draw_text(
                (vw - nw) * 0.5,
                top + row_h * 2.0,
                2.0,
                none,
                swf::Color::from_rgb(0x99AABB, 255),
            );
        }

        // Publish row geometry + the live scroll for the touch layer (drag to
        // scroll, tap to select / activate). Only when interactive: an underlay
        // would clobber the windowed list's metrics. `y` is content-space
        // (pre-scroll), matching what `gallery_hit_test` expects.
        if interactive {
            let mut cells: std::vec::Vec<GalleryCell> = std::vec::Vec::with_capacity(total);
            for i in 0..total {
                cells.push(GalleryCell {
                    row: i as u32,
                    cx: vw * 0.5,
                    x: 0.0,
                    y: row_y(i),
                    w: vw,
                    h: row_h,
                });
            }
            if let Ok(mut g) = gallery_cache().lock() {
                *g = (cells, total as u32);
            }
            if let Ok(mut v) = gallery_view().lock() {
                *v = GalleryView {
                    scroll_px,
                    pitch: row_h,
                    band_top,
                    band_bot,
                    rows_total: total as u32,
                    rows_visible: VISIBLE as u32,
                    horizontal: false,
                    off_min: 0.0,
                    off_max: 0.0,
                };
            }
        }

        // Tracking the eased pixel scroll.
        self.draw_scrollbar(vw - 40.0, top, VISIBLE as f32 * row_h, scroll_px, row_h, VISIBLE, total);

        self.draw_page_footer(crate::loc::s().dist_list_footer);

        unsafe {
            glUseProgram(0);
            glBindVertexArray(0);
        }
        self.gl_state.invalidate();
    }

    /// List of remote files (one row per `RemoteFile`). Mirrors the local
    /// `draw_library_list` layout but skips the per-file color chip /
    /// metadata panel — remote files only have name + size to show.
    /// `downloaded` is the set of basenames already pulled this session
    /// (drawn with a green `OK` prefix so the user knows what's done).
    pub fn draw_library_distant_files(
        &mut self,
        selection: usize,
        scroll_offset: usize,
        files: &[crate::net::RemoteFile],
        visible_rows: usize,
        downloaded: &[std::string::String],
        filter: Option<&str>,
        total_unfiltered: usize,
        // Active reveal-window clip (x,y,w,h) or None in steady state. The rows
        // get a JOUER-style glide + a band scissor INTERSECTED with this window so
        // the smooth scroll doesn't bleed past the header/footer while the reveal
        // still clips the whole list to its opening rectangle.
        outer_clip: Option<(f32, f32, f32, f32)>,
    ) {
        self.library_clear();
        let vw = self.dimensions.width as f32;

        // Header.
        let title = crate::loc::s().files_title;
        let scale_t = 4.0;
        let tw = self.measure_text(title, scale_t);
        self.draw_text(
            (vw - tw) * 0.5 + 3.0,
            30.0 + 3.0,
            scale_t,
            title,
            swf::Color::from_rgb(0x000000, 255),
        );
        self.draw_text(
            (vw - tw) * 0.5,
            30.0,
            scale_t,
            title,
            swf::Color::from_rgb(0xFFD740, 255),
        );

        // Sub-line shows filter status: "23/3633 — FILTRE: mario" when
        // filter is active, "3633 FICHIER(S) .SWF TROUVE(S)" otherwise.
        // The pixel font now renders parentheses, so "FILE(S) FOUND" is fine
        // across locales; the count template lives in loc.rs.
        let sub = crate::loc::count_line(files.len(), total_unfiltered, filter, || {
            crate::loc::files_found(files.len())
        });
        let scale_s = 2.0;
        let sw = self.measure_text(&sub, scale_s);
        self.draw_text(
            (vw - sw) * 0.5,
            85.0,
            scale_s,
            &sub,
            swf::Color::from_rgb(0xAABFD8, 255),
        );

        // Rows.
        const ROW_SCALE: f32 = 2.5;
        const ROW_SPACING: f32 = 50.0;
        let rows_top_y = 150.0;
        let rows_left_x = 80.0;
        let total = files.len();
        let pitch = ROW_SPACING;
        let rows_total = total as u32;
        // Content-space cells for touch hit-test (full row width; `y` is
        // pre-scroll -> screen y = `y - scroll_px`).
        let mut cells: std::vec::Vec<GalleryCell> = std::vec::Vec::with_capacity(total);
        for i in 0..total {
            cells.push(GalleryCell {
                row: i as u32,
                cx: vw * 0.5,
                x: 0.0,
                y: rows_top_y + i as f32 * pitch,
                w: vw,
                h: pitch,
            });
        }
        if let Ok(mut g) = gallery_cache().lock() {
            *g = (cells, rows_total);
        }
        // Ease a pixel scroll toward `scroll_offset` (JOUER-style glide), reusing
        // the shared anim/view/cache singletons (only one gallery screen renders
        // per frame). A row list has no box frame to glide, so only the scroll is
        // eased; the selection marker/colour just rides its row.
        let band_top = rows_top_y - 8.0;
        let band_bot = rows_top_y + visible_rows as f32 * pitch;
        let target_scroll = scroll_offset as f32 * pitch;
        // Selected row's content-space y — an eased highlight bar glides toward
        // it so the "hover" slides from row to row (like JOUER's frame), instead
        // of the `>` marker snapping.
        let target_hover_y = rows_top_y + selection as f32 * pitch;
        let touch_scroll = gallery_touch_scroll_read();
        let mut scroll_px = target_scroll;
        let mut hover_y = target_hover_y;
        if let Ok(mut a) = gallery_anim().lock() {
            let now = unsafe { ruffle_tick_now() };
            if !a.inited {
                a.inited = true;
                a.last_tick = now;
                a.last_sel = selection;
                a.scroll_px = target_scroll;
                a.sel_x = 0.0;
                a.sel_y = target_hover_y;
                a.sel_w = 0.0;
                a.pop = 0.0;
            } else {
                let freq = unsafe { ruffle_tick_freq() } as f32;
                let dt = if freq > 0.0 {
                    (now.saturating_sub(a.last_tick) as f32 / freq).min(0.1)
                } else {
                    1.0 / 60.0
                };
                a.last_tick = now;
                a.last_sel = selection;
                a.scroll_px = ease_to(a.scroll_px, target_scroll, dt, 16.0);
                a.sel_y = ease_to(a.sel_y, target_hover_y, dt, 18.0);
            }
            if let Some(px) = touch_scroll {
                a.scroll_px = px;
            }
            scroll_px = a.scroll_px;
            hover_y = a.sel_y;
        }
        if let Ok(mut v) = gallery_view().lock() {
            *v = GalleryView {
                scroll_px,
                pitch,
                band_top,
                band_bot,
                rows_total,
                rows_visible: visible_rows as u32,
                horizontal: false,
                off_min: 0.0,
                off_max: 0.0,
            };
        }

        // Band scissor for the rows, INTERSECTED with the caller's reveal window
        // (if any) so the smooth scroll clips at the header/footer AND the reveal
        // rectangle both. Header above was drawn under the caller's clip already.
        let band = (0.0f32, band_top, vw, band_bot - band_top);
        let rows_clip = match outer_clip {
            Some((ox, oy, ow, oh)) => {
                let x0 = band.0.max(ox);
                let y0 = band.1.max(oy);
                let x1 = (band.0 + band.2).min(ox + ow);
                let y1 = (band.1 + band.3).min(oy + oh);
                (x0, y0, (x1 - x0).max(0.0), (y1 - y0).max(0.0))
            }
            None => band,
        };
        self.set_clip(rows_clip.0, rows_clip.1, rows_clip.2, rows_clip.3);

        // Gliding hover highlight behind the selected row (eased sel_y -> the bar
        // slides from row to row like JOUER's frame). Drawn before the rows.
        if total > 0 {
            let hy = hover_y - scroll_px;
            self.draw_selection_bar(
                rows_left_x - 40.0,
                hy - 8.0,
                vw - rows_left_x - 20.0,
                ROW_SPACING - 12.0,
                6.0,
            );
        }

        for abs_idx in 0..total {
            let y = rows_top_y + abs_idx as f32 * pitch - scroll_px;
            // Cheap cull: skip rows fully outside the band.
            if y + ROW_SPACING < band_top - 8.0 || y > band_bot + 8.0 {
                continue;
            }
            let f = &files[abs_idx];
            let is_sel = abs_idx == selection;
            let color = if is_sel {
                swf::Color::from_rgb(0xFFFFFF, 255)
            } else {
                swf::Color::from_rgb(0xCCCCCC, 255)
            };
            // OK badge for files already downloaded this session.
            let is_downloaded = downloaded.iter().any(|n| n == &f.name);
            let badge_w = if is_downloaded {
                let badge = "OK";
                let bw = self.measure_text(badge, 2.0);
                // Bright green tint so it pops over the amber/grey rows.
                self.draw_text(rows_left_x, y + 4.0, 2.0, badge, swf::Color::from_rgb(0x66DD66, 255));
                bw + 12.0
            } else {
                0.0
            };
            let name_x = rows_left_x + badge_w;
            // Truncate filename to fit. Each row = filename + size on
            // the right edge.
            let size_str = format_size_pretty(f.size_bytes);
            let size_w = self.measure_text(&size_str, ROW_SCALE);
            let size_x = vw - 80.0 - size_w;
            let max_name_w = size_x - name_x - 20.0;
            let mut display = f.name.clone();
            // ~6 px per char at ROW_SCALE * 6 (5+1 spacing).
            let char_w = 6.0 * ROW_SCALE;
            let max_chars = (max_name_w / char_w) as usize;
            if display.chars().count() > max_chars && max_chars > 1 {
                display = display.chars().take(max_chars - 1).collect();
                display.push('…');
            }
            self.draw_text(name_x, y, ROW_SCALE, &display, color);
            self.draw_text(size_x, y, ROW_SCALE, &size_str, color);
        }

        // Restore the caller's clip (reveal window or none) for the scrollbar.
        match outer_clip {
            Some((ox, oy, ow, oh)) => self.set_clip(ox, oy, ow, oh),
            None => self.clear_clip(),
        }

        // Scrollbar if needed, tracking the eased pixel scroll.
        if total > visible_rows {
            let bar_x = vw - 30.0;
            let bar_top_y = rows_top_y;
            let bar_h_total = visible_rows as f32 * ROW_SPACING;
            let bar_h_thumb = (bar_h_total * visible_rows as f32 / total as f32).max(20.0);
            let max_scroll_px = rows_total.saturating_sub(visible_rows as u32) as f32 * pitch;
            let progress = if max_scroll_px > 0.0 { (scroll_px / max_scroll_px).clamp(0.0, 1.0) } else { 0.0 };
            let thumb_y = bar_top_y + (bar_h_total - bar_h_thumb) * progress;
            let track = Matrix {
                a: 4.0, b: 0.0, c: 0.0, d: bar_h_total,
                tx: swf::Twips::from_pixels(bar_x as f64),
                ty: swf::Twips::from_pixels(bar_top_y as f64),
            };
            <Self as CommandHandler>::draw_rect(self, swf::Color::from_rgba(0x40_99AABB), track);
            let thumb = Matrix {
                a: 4.0, b: 0.0, c: 0.0, d: bar_h_thumb,
                tx: swf::Twips::from_pixels(bar_x as f64),
                ty: swf::Twips::from_pixels(thumb_y as f64),
            };
            <Self as CommandHandler>::draw_rect(self, swf::Color::from_rgb(0xFFD740, 255), thumb);
        }

        self.draw_page_footer(crate::loc::s().files_footer);
        unsafe {
            glUseProgram(0);
            glBindVertexArray(0);
        }
        self.gl_state.invalidate();
    }

    /// Download in flight — big title, filename, progress bar, footer.
    /// `bytes_total = 0` means Content-Length wasn't known at the start;
    /// show an indeterminate bar in that case (just a slim animated
    /// marker; for v1 we just show "??.?? / ??" until total arrives).
    pub fn draw_library_distant_downloading(
        &mut self,
        file_name: &str,
        bytes_done: u64,
        bytes_total: u64,
    ) {
        self.library_clear();
        let vw = self.dimensions.width as f32;
        let vh = self.dimensions.height as f32;

        let title = crate::loc::s().dl_title;
        let scale_t = 5.0;
        let tw = self.measure_text(title, scale_t);
        self.draw_text(
            (vw - tw) * 0.5 + 4.0,
            vh * 0.18 + 4.0,
            scale_t,
            title,
            swf::Color::from_rgb(0x000000, 255),
        );
        self.draw_text(
            (vw - tw) * 0.5,
            vh * 0.18,
            scale_t,
            title,
            swf::Color::from_rgb(0xFFD740, 255),
        );

        // Filename (truncated if needed).
        let scale_n = 2.0;
        let mut display = file_name.to_string();
        let max_chars = 56usize;
        if display.chars().count() > max_chars && max_chars > 1 {
            display = display.chars().take(max_chars - 1).collect();
            display.push('…');
        }
        let nw = self.measure_text(&display, scale_n);
        self.draw_text(
            (vw - nw) * 0.5,
            vh * 0.34,
            scale_n,
            &display,
            swf::Color::from_rgb(0xCCCCCC, 255),
        );

        // Progress bar (centred 800x40, fill amber, track navy).
        const BAR_W: f32 = 800.0;
        const BAR_H: f32 = 40.0;
        let bar_x = (vw - BAR_W) * 0.5;
        let bar_y = vh * 0.50;
        let track = Matrix {
            a: BAR_W, b: 0.0, c: 0.0, d: BAR_H,
            tx: swf::Twips::from_pixels(bar_x as f64),
            ty: swf::Twips::from_pixels(bar_y as f64),
        };
        <Self as CommandHandler>::draw_rect(self, swf::Color::from_rgb(0x142038, 255), track);
        <Self as CommandHandler>::draw_line_rect(self, swf::Color::from_rgb(0xFFFFFF, 255), track);

        let frac = if bytes_total > 0 {
            (bytes_done as f32 / bytes_total as f32).clamp(0.0, 1.0)
        } else {
            0.0
        };
        if frac > 0.0 {
            let fill = Matrix {
                a: BAR_W * frac, b: 0.0, c: 0.0, d: BAR_H,
                tx: swf::Twips::from_pixels(bar_x as f64),
                ty: swf::Twips::from_pixels(bar_y as f64),
            };
            <Self as CommandHandler>::draw_rect(self, swf::Color::from_rgb(0xFFD740, 255), fill);
        }

        // % + bytes label below the bar.
        let scale_p = 2.5;
        let label = if bytes_total > 0 {
            std::format!(
                "{}%   {} / {}",
                (frac * 100.0) as u32,
                format_size_pretty(bytes_done),
                format_size_pretty(bytes_total),
            )
        } else {
            std::format!("{} ...", format_size_pretty(bytes_done))
        };
        let pw = self.measure_text(&label, scale_p);
        self.draw_text(
            (vw - pw) * 0.5,
            bar_y + BAR_H + 20.0,
            scale_p,
            &label,
            swf::Color::from_rgb(0xFFFFFF, 255),
        );

        // Footer.
        const HELP_SCALE: f32 = 2.0;
        let help = crate::loc::s().dl_footer;
        let help_w = self.measure_text(help, HELP_SCALE);
        self.draw_text(
            (vw - help_w) * 0.5,
            vh - 42.0,
            HELP_SCALE,
            help,
            swf::Color::from_rgb(0x99AABB, 255),
        );
        unsafe {
            glUseProgram(0);
            glBindVertexArray(0);
        }
        self.gl_state.invalidate();
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
        self.library_clear();
        let vw = self.dimensions.width as f32;
        let vh = self.dimensions.height as f32;

        let scale_t = 5.0;
        let tw = self.measure_text(title, scale_t);
        self.draw_text(
            (vw - tw) * 0.5 + 4.0,
            vh * 0.22 + 4.0,
            scale_t,
            title,
            swf::Color::from_rgb(0x000000, 255),
        );
        self.draw_text(
            (vw - tw) * 0.5,
            vh * 0.22,
            scale_t,
            title,
            swf::Color::from_rgb(title_rgb, 255),
        );

        // Word-wrapped, centred. `wrap_words` measures the line and hard-chops
        // any single word too wide for it.
        let scale_m = 2.0;
        // The 60-character line this notice has always used, expressed as the
        // width it actually occupies at `scale_m`.
        const WRAP_W: f32 = 60.0 * 6.0 * 2.0;
        let lines = wrap_words(msg, WRAP_W, scale_m);
        let mut y = vh * 0.42;
        for line in &lines {
            let w = self.measure_text(line, scale_m);
            self.draw_text(
                (vw - w) * 0.5,
                y,
                scale_m,
                line,
                swf::Color::from_rgb(0xCCCCCC, 255),
            );
            y += 30.0;
        }

        self.draw_page_footer(footer);
        unsafe {
            glUseProgram(0);
            glBindVertexArray(0);
        }
        self.gl_state.invalidate();
    }

    /// Dim backdrop used when the menu module's TOUCHES editor is on top of
    /// the library (pre-launch keymap edit). Quick black fill — no library
    /// content underneath, no Ruffle render — just a flat backdrop so
    /// `menu::draw` sits on something solid.
    /// Settings modal (Plus from the library). Caller has already cleared
    /// the screen via `draw_library_dim_backdrop`. `entries` are localized
    /// labels in fixed order (default controls / language / back).
    /// REGLAGES — a full-screen navbar TAB page (v1.2.0), not a floating modal:
    /// clears its own background, draws a top header + a centered entry list +
    /// footer. The navbar is drawn over the top afterwards by `library::render`.
    pub fn draw_library_settings(&mut self, selection: usize, entries: &[&str]) {
        self.library_clear();
        let vw = self.dimensions.width as f32;
        let vh = self.dimensions.height as f32;

        // Header (below the navbar strip).
        const TITLE_SCALE: f32 = 4.0;
        let header = crate::loc::s().settings_title;
        let header_w = self.measure_text(header, TITLE_SCALE);
        self.draw_text(
            (vw - header_w) * 0.5,
            90.0,
            TITLE_SCALE,
            header,
            swf::Color::from_rgb(0xFFD740, 255),
        );
        // Thin underline accent under the header.
        let rule = Matrix {
            a: 360.0, b: 0.0, c: 0.0, d: 2.0,
            tx: swf::Twips::from_pixels(((vw - 360.0) * 0.5) as f64),
            ty: swf::Twips::from_pixels(150.0),
        };
        <Self as CommandHandler>::draw_rect(self, swf::Color::from_rgba(0x80_99_AA_BB), rule);

        // Centered entry list with a gliding selection highlight (v1.2.0).
        const OPT_SCALE: f32 = 3.0;
        // Center the block vertically between the header rule and the footer so it
        // stays balanced whatever the entry count (a row was added: PSEUDO #20).
        let region_top = 185.0;
        let region_bottom = vh - 70.0;
        // Rows tighten rather than overflow. At the comfortable 66 px the list
        // outgrew its region the moment an eighth row arrived (DOSSIER JEUX,
        // #79) and the last entry landed on top of the footer hint. Dividing the
        // region instead keeps every row on screen, and keeps doing so for the
        // next row someone adds.
        let row_h = (66.0f32).min((region_bottom - region_top) / entries.len().max(1) as f32);
        let block_h = entries.len() as f32 * row_h;
        let top_y = (region_top + ((region_bottom - region_top) - block_h) * 0.5).max(region_top);

        let target_hy = top_y + selection as f32 * row_h;
        let now_hl = unsafe { ruffle_tick_now() };
        let hy = eased_list_y(target_hy, 2, now_hl);
        // Wide enough for the LONGEST row, not a fixed 460 px. Rows carry their
        // value now — "DOSSIER JEUX : /ROMS/FLASHNX" — and a fixed bar let the
        // text hang out of its own highlight. Sized on the widest entry rather
        // than the selected one so the bar keeps still as the cursor moves.
        let widest = entries
            .iter()
            .map(|e| self.measure_text(e, OPT_SCALE))
            .fold(0.0f32, f32::max);
        let bar_w = (widest + 56.0).clamp(460.0, vw - 80.0);
        self.draw_selection_bar((vw - bar_w) * 0.5, hy - 8.0, bar_w, row_h - 16.0, 6.0);
        // Tappable rows, the width of the bar they light up.
        ui_cells_publish(
            ui_screen_kind(),
            (0..entries.len())
                .map(|i| ((vw - bar_w) * 0.5, top_y + i as f32 * row_h - 8.0, bar_w, row_h))
                .collect(),
        );
        // Cursor at the eased y, x aligned to the selected entry's centering.
        if let Some(sel) = entries.get(selection) {
            let sel_ow = self.measure_text(sel, OPT_SCALE);
            let sel_x = (vw - sel_ow) * 0.5;
            self.draw_text(sel_x - 40.0, hy, OPT_SCALE, ">", swf::Color::from_rgb(0xFFD740, 255));
        }

        for (i, opt) in entries.iter().enumerate() {
            let y = top_y + i as f32 * row_h;
            let is_sel = i == selection;
            let color = if is_sel {
                swf::Color::from_rgb(0xFFD740, 255)
            } else {
                swf::Color::from_rgb(0xCCCCCC, 255)
            };
            let ow = self.measure_text(opt, OPT_SCALE);
            let x = (vw - ow) * 0.5;
            self.draw_text(x, y, OPT_SCALE, opt, color);
        }

        // Footer.
        const HELP_SCALE: f32 = 2.0;
        let help = crate::loc::s().settings_footer;
        let help_w = self.measure_text(help, HELP_SCALE);
        self.draw_text(
            (vw - help_w) * 0.5,
            vh - 42.0,
            HELP_SCALE,
            help,
            swf::Color::from_rgb(0x99AABB, 255),
        );

        unsafe {
            glUseProgram(0);
            glBindVertexArray(0);
        }
        self.gl_state.invalidate();
    }

    /// Cover picker (OPTIONS > JAQUETTE, v1.2.0). Shows Flashpoint candidate
    /// covers as a THUMBNAIL GRID (loaded progressively, one per frame). A
    /// non-empty `msg` with no candidates shows a notice instead.
    pub fn draw_library_cover_picker(
        &mut self,
        game_name: &str,
        selection: usize,
        titles: &[&str],
        urls: &[&str],
        msg: &str,
        header_title: &str,
        footer: &str,
    ) {
        unsafe {
            glEnable(GL_BLEND);
            glBlendFunc(GL_SRC_ALPHA, GL_ONE_MINUS_SRC_ALPHA);
            glDisable(GL_STENCIL_TEST);
        }
        let vw = self.dimensions.width as f32;
        let vh = self.dimensions.height as f32;

        const PANEL_W: f32 = 980.0;
        let cols = crate::library::COVER_PICKER_COLS;
        let n = urls.len();

        if n == 0 {
            // Empty: a compact notice panel (covers off / no results / error).
            let panel_h = 240.0;
            let panel_x = (vw - PANEL_W) * 0.5;
            let panel_y = (vh - panel_h) * 0.5;
            let panel = Matrix {
                a: PANEL_W, b: 0.0, c: 0.0, d: panel_h,
                tx: swf::Twips::from_pixels(panel_x as f64),
                ty: swf::Twips::from_pixels(panel_y as f64),
            };
            <Self as CommandHandler>::draw_rect(self, swf::Color::from_rgba(0xF0_14_20_38), panel);
            <Self as CommandHandler>::draw_line_rect(self, swf::Color::from_rgb(0xFFFFFF, 255), panel);
            let title = header_title;
            let tw = self.measure_text(title, 3.0);
            self.draw_text(panel_x + (PANEL_W - tw) * 0.5, panel_y + 30.0, 3.0, title, swf::Color::from_rgb(0xFFFFFF, 255));
            let m = if msg.is_empty() { crate::loc::s().cover_none } else { msg };
            let shown = self.fit_text_mid(m, 2.0, PANEL_W - 120.0);
            let mw = self.measure_text(&shown, 2.0);
            self.draw_text(panel_x + (PANEL_W - mw) * 0.5, panel_y + 120.0, 2.0, &shown, swf::Color::from_rgb(0xAABFD8, 255));
            let help = footer;
            let hw = self.measure_text(help, 2.0);
            self.draw_text(panel_x + (PANEL_W - hw) * 0.5, panel_y + panel_h - 36.0, 2.0, help, swf::Color::from_rgb(0x99AABB, 255));
            unsafe {
                glUseProgram(0);
                glBindVertexArray(0);
            }
            self.gl_state.invalidate();
            return;
        }

        // Grid geometry.
        const MARGIN: f32 = 40.0;
        const CELL_GAP: f32 = 16.0;
        const THUMB_H: f32 = 120.0;
        let inner_w = PANEL_W - MARGIN * 2.0;
        let cell_w = (inner_w - CELL_GAP * (cols as f32 - 1.0)) / cols as f32;
        let rows = (n + cols - 1) / cols;
        let grid_h = rows as f32 * (THUMB_H + CELL_GAP);
        let panel_h = 110.0 + grid_h + 84.0;
        let panel_x = (vw - PANEL_W) * 0.5;
        let panel_y = (vh - panel_h) * 0.5;
        let panel = Matrix {
            a: PANEL_W, b: 0.0, c: 0.0, d: panel_h,
            tx: swf::Twips::from_pixels(panel_x as f64),
            ty: swf::Twips::from_pixels(panel_y as f64),
        };
        <Self as CommandHandler>::draw_rect(self, swf::Color::from_rgba(0xF0_14_20_38), panel);
        <Self as CommandHandler>::draw_line_rect(self, swf::Color::from_rgb(0xFFFFFF, 255), panel);

        // Title + game-name subtitle.
        let title = header_title;
        let tw = self.measure_text(title, 3.0);
        self.draw_text(panel_x + (PANEL_W - tw) * 0.5, panel_y + 26.0, 3.0, title, swf::Color::from_rgb(0xFFFFFF, 255));
        let gn = self.fit_text_mid(game_name, 2.0, 44.0 * 6.0 * 2.0);
        let sw = self.measure_text(&gn, 2.0);
        self.draw_text(panel_x + (PANEL_W - sw) * 0.5, panel_y + 70.0, 2.0, &gn, swf::Color::from_rgb(0xFFD740, 255));

        // Phase from the system tick for a subtle selection pulse.
        let now_t = unsafe { ruffle_tick_now() };
        let phase_s = (now_t as f64) / (unsafe { ruffle_tick_freq() } as f64);
        let pulse = approx_sin(phase_s as f32 * (2.0 * core::f32::consts::PI / 1.6));

        // Finish at most one async logo download this frame (never blocks).
        self.pump_thumbnail_load();

        let grid_top = panel_y + 110.0;
        let grid_left = panel_x + MARGIN;
        // The eased frame position, computed BEFORE the tiles so each tile can
        // ask whether the frame is over IT rather than over the selected index.
        // Keyed to the index, the tile being moved TO kept square corners for
        // the whole glide while the one being left rounded up instantly -- both
        // of them wrong for as long as the movement lasted.
        let (frame_x, frame_y) = if selection < n {
            let tx = grid_left + (selection % cols) as f32 * (cell_w + CELL_GAP);
            let ty = grid_top + (selection / cols) as f32 * (THUMB_H + CELL_GAP);
            (
                eased_list_x(tx, GLIDE_KEY_COVER, now_t),
                eased_list_y(ty, GLIDE_KEY_COVER, now_t),
            )
        } else {
            (0.0, 0.0)
        };
        // This panel published NOTHING, which left the OPTIONS modal's table --
        // six rows across the middle of the screen -- standing behind it. Tapping
        // a thumbnail resolved against those rows and fetched the wrong cover.
        // The hit test refuses a table that is not the live screen's now, but the
        // grid may as well answer the finger it was silently mis-answering.
        ui_cells_publish(
            ui_screen_kind(),
            (0..n)
                .map(|i| {
                    (
                        grid_left + (i % cols) as f32 * (cell_w + CELL_GAP),
                        grid_top + (i / cols) as f32 * (THUMB_H + CELL_GAP),
                        cell_w,
                        THUMB_H,
                    )
                })
                .collect(),
        );
        for i in 0..n {
            let col = (i % cols) as f32;
            let row = (i / cols) as f32;
            let cx = grid_left + col * (cell_w + CELL_GAP);
            let cy = grid_top + row * (THUMB_H + CELL_GAP);
            // Cell backdrop (so pending / failed thumbs still show a tile).
            let bg = Matrix {
                a: cell_w, b: 0.0, c: 0.0, d: THUMB_H,
                tx: swf::Twips::from_pixels(cx as f64),
                ty: swf::Twips::from_pixels(cy as f64),
            };
            <Self as CommandHandler>::draw_rect(self, swf::Color::from_rgba(0xFF_0B_12_22), bg);

            match self.thumb_for(urls[i]) {
                Some(ThumbTex::Image { tex, w, h }) => {
                    self.draw_textured_rect_cover(cx, cy, cell_w, THUMB_H, tex, w, h, 1.0);
                    // Not on the selected tile: its corners are covered by the
                    // frame below, which is rounded instead. Rounding both left
                    // four notches trapped inside the gold border — the artefact
                    // GRILLE already documents avoiding.
                    // Under the frame right now? Then leave the corners square,
                    // because the frame covers them. Measured against the eased
                    // position, so the answer changes as the frame travels.
                    let under = (cx - frame_x).abs() < 1.0 && (cy - frame_y).abs() < 1.0;
                    if !under {
                        self.round_corners_on(cx, cy, cell_w, THUMB_H, 6.0, 0xFF_0B_12_22);
                    }
                }
                Some(ThumbTex::Failed) => {
                    let q = "?";
                    let qw = self.measure_text(q, 4.0);
                    self.draw_text(cx + (cell_w - qw) * 0.5, cy + THUMB_H * 0.5 - 14.0, 4.0, q, swf::Color::from_rgb(0x55_66_77, 255));
                }
                None => {
                    let d = "...";
                    let dw = self.measure_text(d, 3.0);
                    self.draw_text(cx + (cell_w - dw) * 0.5, cy + THUMB_H * 0.5 - 10.0, 3.0, d, swf::Color::from_rgb(0x7A8A9C, 255));
                }
            }

        }

        // The selection frame, drawn AFTER the tiles and at an eased position,
        // so it travels to the tile the cursor moved to instead of appearing on
        // it. Out of the loop for that reason: inside, it could only ever be at
        // one tile's exact coordinates.
        //
        // Both axes, because this is a grid: the cursor moves sideways as often
        // as it moves down, and a frame that only slid vertically would look
        // broken half the time.
        if selection < n {
            self.draw_pulse_frame(frame_x, frame_y, cell_w, THUMB_H, pulse);
            // The rounded corners are painted with the PANEL colour, so while
            // the frame travels they land as four dark chips on whatever
            // thumbnail it happens to be over. Only once it has arrived is
            // there a tile underneath for them to belong to.
            let tx = grid_left + (selection % cols) as f32 * (cell_w + CELL_GAP);
            let ty = grid_top + (selection / cols) as f32 * (THUMB_H + CELL_GAP);
            if (frame_x - tx).abs() < 1.0 && (frame_y - ty).abs() < 1.0 {
                let b = Self::SEL_FRAME_B;
                self.round_corners_on(
                    frame_x - b, frame_y - b, cell_w + 2.0 * b, THUMB_H + 2.0 * b, 8.0,
                    0xFF_0B_12_22,
                );
            }
        }

        // Selected candidate title under the grid.
        if let Some(t) = titles.get(selection) {
            let shown = self.fit_text_mid(t, 2.0, PANEL_W - 80.0);
            let sw2 = self.measure_text(&shown, 2.0);
            self.draw_text(panel_x + (PANEL_W - sw2) * 0.5, panel_y + panel_h - 66.0, 2.0, &shown, swf::Color::from_rgb(0xCCCCCC, 255));
        }

        // Footer.
        let help = footer;
        let hw = self.measure_text(help, 2.0);
        self.draw_text(panel_x + (PANEL_W - hw) * 0.5, panel_y + panel_h - 34.0, 2.0, help, swf::Color::from_rgb(0x99AABB, 255));

        unsafe {
            glUseProgram(0);
            glBindVertexArray(0);
        }
        self.gl_state.invalidate();
    }

    /// Full-page, scrollable cover gallery for Flashpoint search results
    /// (IMPORTER > X). Unlike the JAQUETTE picker (a centered modal sized for a
    /// handful of candidates), this fills the screen like a tab page and scrolls
    /// — `scroll_row` is the first visible row. Thumbnails load progressively
    /// from `urls` via the same `thumb_for` cache as the cover picker.
    pub fn draw_library_fp_gallery(
        &mut self,
        query: &str,
        selection: usize,
        scroll_row: usize,
        titles: &[&str],
        urls: &[&str],
        installed: &[bool],
        msg: &str,
        header_title: &str,
        footer: &str,
    ) {
        self.library_clear();
        unsafe {
            glEnable(GL_BLEND);
            glBlendFunc(GL_SRC_ALPHA, GL_ONE_MINUS_SRC_ALPHA);
            glDisable(GL_STENCIL_TEST);
        }
        let vw = self.dimensions.width as f32;
        let vh = self.dimensions.height as f32;
        let cols = crate::library::FP_GALLERY_COLS;
        let rows_visible = crate::library::FP_GALLERY_ROWS;
        let n = urls.len();

        // Header (title + the search query) and footer.
        let tw = self.measure_text(header_title, 3.0);
        self.draw_text((vw - tw) * 0.5, 36.0, 3.0, header_title, swf::Color::from_rgb(0xFFFFFF, 255));
        if !query.is_empty() {
            let q = self.fit_text_mid(query, 2.0, 60.0 * 6.0 * 2.0);
            let qw = self.measure_text(&q, 2.0);
            self.draw_text((vw - qw) * 0.5, 80.0, 2.0, &q, swf::Color::from_rgb(0xFFD740, 255));
        }
        let hw = self.measure_text(footer, 2.0);
        self.draw_text((vw - hw) * 0.5, vh - 34.0, 2.0, footer, swf::Color::from_rgb(0x99AABB, 255));

        if n == 0 {
            // Word-wrapped: this doubles as the ERROR surface for a failed
            // search, and those messages are sentences — drawn as one centered
            // line they ran off both edges of the screen.
            let m = if msg.is_empty() { crate::loc::s().cover_none } else { msg };
            const MS: f32 = 2.5;
            let lines = wrap_words(m, vw - 80.0, MS);
            let mut my = vh * 0.5 - 12.0 - (lines.len().saturating_sub(1) as f32) * 17.0;
            for line in &lines {
                let mw = self.measure_text(line, MS);
                self.draw_text((vw - mw) * 0.5, my, MS, line, swf::Color::from_rgb(0xAABFD8, 255));
                my += 34.0;
            }
            unsafe {
                glUseProgram(0);
                glBindVertexArray(0);
            }
            self.gl_state.invalidate();
            return;
        }

        const MARGIN: f32 = 40.0;
        const GAP: f32 = 16.0;
        const LABEL_H: f32 = 22.0;
        let grid_top = 116.0;
        let grid_bottom = vh - 52.0;
        let inner_w = vw - MARGIN * 2.0;
        let cell_w = (inner_w - GAP * (cols as f32 - 1.0)) / cols as f32;
        let row_h = (grid_bottom - grid_top) / rows_visible as f32;
        let thumb_h = (row_h - LABEL_H - GAP).max(40.0);

        let phase_s = (unsafe { ruffle_tick_now() } as f64) / (unsafe { ruffle_tick_freq() } as f64);
        let pulse = approx_sin(phase_s as f32 * (2.0 * core::f32::consts::PI / 1.6));

        // Finish at most one async logo download this frame (never blocks).
        self.pump_thumbnail_load();

        // Smooth-scrolled grid (JOUER-style glide + touch), replacing the old
        // whole-row paging. The input layer still tracks a discrete first row
        // (`scroll_row`) + tile index (`selection`); here we ease a pixel scroll
        // toward that row and a selection frame toward the active tile, reusing
        // the JOUER gallery's shared anim/view/cache singletons (only one gallery
        // screen renders per frame, so sharing them is safe). A scissor clips the
        // band so partially-scrolled rows don't bleed over the header/footer.
        let rows_total = ((n + cols - 1) / cols) as u32;
        let pitch = row_h;
        // Content-space cells for touch hit-test + input 2D nav (`y` is pre-scroll;
        // on-screen y = `y - scroll_px`). Hit region = the thumbnail rect.
        let mut cells: std::vec::Vec<GalleryCell> = std::vec::Vec::with_capacity(n);
        for i in 0..n {
            let col = (i % cols) as f32;
            let row = (i / cols) as u32;
            let cx = MARGIN + col * (cell_w + GAP);
            cells.push(GalleryCell {
                row,
                cx: cx + cell_w * 0.5,
                x: cx,
                y: grid_top + row as f32 * pitch,
                w: cell_w,
                h: thumb_h,
            });
        }
        if let Ok(mut g) = gallery_cache().lock() {
            *g = (cells, rows_total);
        }
        // Clip band sits 16px ABOVE the first row so the top row's selection
        // frame (which overhangs ~4px, more on a pop) isn't clipped by the header
        // (same headroom JOUER uses). Still below the query subtitle (~y94).
        let band_top = grid_top - 16.0;
        let band_bot = grid_bottom;
        let target_scroll = scroll_row as f32 * pitch;
        let sel_col = (selection % cols) as f32;
        let sel_row_u = (selection / cols) as u32;
        let target_sel_x = MARGIN + sel_col * (cell_w + GAP);
        let target_sel_y = grid_top + sel_row_u as f32 * pitch;
        let mut scroll_px = target_scroll;
        let mut frame_x = target_sel_x;
        let mut frame_y = target_sel_y;
        let mut frame_w = cell_w;
        let mut pop = 0.0f32;
        let touch_scroll = gallery_touch_scroll_read();
        if let Ok(mut a) = gallery_anim().lock() {
            let now = unsafe { ruffle_tick_now() };
            if !a.inited {
                a.inited = true;
                a.last_tick = now;
                a.last_sel = selection;
                a.sel_x = target_sel_x;
                a.sel_y = target_sel_y;
                a.sel_w = cell_w;
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
                    a.pop = 1.0;
                    a.last_sel = selection;
                }
                // Softer/slower than JOUER's 18/16: the FP grid is heavier to
                // draw (backdrop + thumb + label per tile) so its frame rate dips
                // during navigation; a longer time-constant draws more in-between
                // frames, so the glide reads smoothly even at ~40 fps.
                a.sel_x = ease_to(a.sel_x, target_sel_x, dt, 11.0);
                a.sel_y = ease_to(a.sel_y, target_sel_y, dt, 11.0);
                a.sel_w = ease_to(a.sel_w, cell_w, dt, 11.0);
                a.scroll_px = ease_to(a.scroll_px, target_scroll, dt, 11.0);
                a.pop = ease_to(a.pop, 0.0, dt, 10.0);
            }
            if let Some(px) = touch_scroll {
                a.scroll_px = px;
            }
            scroll_px = a.scroll_px;
            frame_x = a.sel_x;
            frame_y = a.sel_y;
            frame_w = a.sel_w;
            pop = a.pop;
        }
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

        // Clip to the grid band through `set_clip`, the one place the Y flip is
        // written down. The copy that used to sit here read `vh` from fifty lines
        // away, so moving the band and forgetting this line clipped the wrong
        // half of the screen with nothing to warn at compile time.
        self.set_clip(0.0, band_top, vw, band_bot - band_top);

        // Same hover as the JOUER grid: the cover of the tile being selected
        // FOLDS OUT to its natural aspect while the one being left folds back,
        // and the selected tile keeps square corners because the frame covers
        // them. Exactly two tiles move; every other one is 0, whatever the
        // travelling frame passes over.
        let (cover_open, cover_close, cover_t) = grid_cover_phase(selection);

        for i in 0..n {
            let col = (i % cols) as f32;
            let row = (i / cols) as u32;
            let cx = MARGIN + col * (cell_w + GAP);
            let cy = grid_top + row as f32 * pitch - scroll_px;
            // Cheap cull: skip tiles fully outside the band.
            if cy + thumb_h < band_top - 8.0 || cy > band_bot + 8.0 {
                continue;
            }

            // A loaded cover fills the cell, so only pending/failed tiles need a
            // backdrop (skipping it for loaded tiles halves the per-tile draws,
            // which keeps the frame rate up so the glide stays smooth).
            match self.thumb_for(urls[i]) {
                Some(ThumbTex::Image { tex, w, h }) => {
                    let b = {
                        let u = if i == cover_open {
                            cover_t
                        } else if i == cover_close {
                            1.0 - cover_t
                        } else {
                            0.0
                        };
                        u * u * (3.0 - 2.0 * u) // smoothstep, as JOUER uses
                    };
                    self.draw_cover_zoomed_out(cx, cy, cell_w, thumb_h, tex, w, h, b, 1.0);
                    // Not the selected tile: its corners are covered by the
                    // frame, which is rounded instead. Rounding both leaves four
                    // notches trapped inside the gold border.
                    if i != selection {
                        self.round_corners(cx, cy, cell_w, thumb_h, 6.0);
                    }
                }
                other => {
                    let bg = Matrix {
                        a: cell_w, b: 0.0, c: 0.0, d: thumb_h,
                        tx: swf::Twips::from_pixels(cx as f64),
                        ty: swf::Twips::from_pixels(cy as f64),
                    };
                    <Self as CommandHandler>::draw_rect(self, swf::Color::from_rgba(0xFF_0B_12_22), bg);
                    if matches!(other, Some(ThumbTex::Failed)) {
                        let q = "?";
                        let qw = self.measure_text(q, 4.0);
                        self.draw_text(cx + (cell_w - qw) * 0.5, cy + thumb_h * 0.5 - 14.0, 4.0, q, swf::Color::from_rgb(0x55_66_77, 255));
                    } else {
                        let d = "...";
                        let dw = self.measure_text(d, 3.0);
                        self.draw_text(cx + (cell_w - dw) * 0.5, cy + thumb_h * 0.5 - 10.0, 3.0, d, swf::Color::from_rgb(0x7A8A9C, 255));
                    }
                }
            }

            // "OK" badge (top-right) for games already in the local library.
            if installed.get(i).copied().unwrap_or(false) {
                let bw = 32.0;
                let bh = 18.0;
                let bx = cx + cell_w - bw - 4.0;
                let by = cy + 4.0;
                let badge = Matrix {
                    a: bw, b: 0.0, c: 0.0, d: bh,
                    tx: swf::Twips::from_pixels(bx as f64),
                    ty: swf::Twips::from_pixels(by as f64),
                };
                <Self as CommandHandler>::draw_rect(self, swf::Color::from_rgba(0xF0_2E_8B_57), badge);
                let okw = self.measure_text("OK", 1.5);
                self.draw_text(bx + (bw - okw) * 0.5, by + 3.0, 1.5, "OK", swf::Color::from_rgb(0xFFFFFF, 255));
            }
            // Per-cell title (truncated to the cell width).
            if let Some(t) = titles.get(i) {
                let ls = 1.5;
                let shown = self.fit_text_mid(t, ls, cell_w);
                let lw = self.measure_text(&shown, ls);
                let col_txt = if i == selection { 0xFFFFFF } else { 0x9FB0C2 };
                self.draw_text(cx + (cell_w - lw) * 0.5, cy + thumb_h + 5.0, ls, &shown, swf::Color::from_rgb(col_txt, 255));
            }
        }

        // Eased selection frame (drawn last, inside the scissor; `pop` inflates
        // it briefly on a cursor move for a tactile snap). Pulsing gold, glided.
        {
            let grow = pop * 4.0;
            let fx = frame_x - grow;
            let fy = frame_y - scroll_px - grow;
            let fw = frame_w + 2.0 * grow;
            let fh = thumb_h + 2.0 * grow;
            self.draw_pulse_frame(fx, fy, fw, fh, pulse);
            // Rounded once, frame included, so the cursor matches the tiles it
            // travels between. This was the whole difference with the JOUER
            // grid: the tiles were already rounded here, the cursor around them
            // was not, so the one square-cornered thing on screen was the thing
            // the eye follows.
            let b = Self::SEL_FRAME_B;
            self.round_corners(fx - b, fy - b, fw + 2.0 * b, fh + 2.0 * b, 8.0);
        }

        self.clear_clip();

        // Scrollbar (right edge) when there's more than one screenful, tracking
        // the eased pixel scroll.
        if rows_total > rows_visible as u32 {
            let track_x = vw - 14.0;
            let track_y = grid_top;
            let track_h = grid_bottom - grid_top;
            let track = Matrix {
                a: 4.0, b: 0.0, c: 0.0, d: track_h,
                tx: swf::Twips::from_pixels(track_x as f64),
                ty: swf::Twips::from_pixels(track_y as f64),
            };
            <Self as CommandHandler>::draw_rect(self, swf::Color::from_rgba(0x40_FF_FF_FF), track);
            let frac = rows_visible as f32 / rows_total as f32;
            let thumb_h2 = (track_h * frac).max(24.0);
            let max_scroll_px = rows_total.saturating_sub(rows_visible as u32) as f32 * pitch;
            let pos = if max_scroll_px > 0.0 { (scroll_px / max_scroll_px).clamp(0.0, 1.0) } else { 0.0 };
            let thumb_y = track_y + (track_h - thumb_h2) * pos;
            let bar = Matrix {
                a: 4.0, b: 0.0, c: 0.0, d: thumb_h2,
                tx: swf::Twips::from_pixels(track_x as f64),
                ty: swf::Twips::from_pixels(thumb_y as f64),
            };
            <Self as CommandHandler>::draw_rect(self, swf::Color::from_rgb(0xFFD740, 255), bar);
        }

        unsafe {
            glUseProgram(0);
            glBindVertexArray(0);
        }
        self.gl_state.invalidate();
    }

    /// Flashpoint details popup (`+` on a gallery tile): full title (word-wrapped)
    /// + developer / publisher / release date (rows skipped when unknown) +
    /// download size. The caller draws the dim backdrop first.
    pub fn draw_library_fp_details(
        &mut self,
        title: &str,
        developer: &str,
        publisher: &str,
        release_date: &str,
        size_bytes: u64,
        // The game's logo. Already in the thumbnail cache (the gallery behind
        // this popup loaded it), so drawing it here costs nothing extra.
        cover_url: &str,
        // The game's blurb; empty when none was published or the fetch failed.
        description: &str,
    ) {
        unsafe {
            glEnable(GL_BLEND);
            glBlendFunc(GL_SRC_ALPHA, GL_ONE_MINUS_SRC_ALPHA);
            glDisable(GL_STENCIL_TEST);
        }
        let vw = self.dimensions.width as f32;
        let vh = self.dimensions.height as f32;
        let lc = crate::loc::s();

        const PANEL_W: f32 = 840.0;
        // Cover column on the left; the title + info rows sit beside it.
        const COVER_W: f32 = 180.0;
        const COVER_H: f32 = 135.0;
        let cover = match self.thumb_for(cover_url) {
            Some(ThumbTex::Image { tex, w, h }) if tex != 0 => Some((tex, w, h)),
            _ => None,
        };
        let text_x0 = 40.0 + if cover.is_some() { COVER_W + 24.0 } else { 0.0 };
        let title_scale = 2.5;
        let title_lines = wrap_words(title, PANEL_W - text_x0 - 40.0, title_scale);
        let mut title_lines = title_lines;
        if title_lines.is_empty() {
            title_lines.push(std::string::String::from("?"));
        }

        // Blurb, wrapped and capped: this is a popup, not a reader. Newlines in
        // the source text are collapsed by the word wrapper.
        const DESC_SCALE: f32 = 1.6;
        const DESC_MAX_LINES: usize = 7;
        let mut desc_lines = wrap_words(description, PANEL_W - 80.0, DESC_SCALE);
        if desc_lines.len() > DESC_MAX_LINES {
            desc_lines.truncate(DESC_MAX_LINES);
            if let Some(last) = desc_lines.last_mut() {
                last.push('…');
            }
        }

        // Info rows (label, value) — skip unknown fields; size always shown.
        let size_val = if size_bytes > 0 {
            format_size_pretty(size_bytes)
        } else {
            std::string::String::from("?")
        };
        let mut rows: std::vec::Vec<(&str, std::string::String)> = std::vec::Vec::new();
        if !developer.is_empty() {
            rows.push((lc.fp_details_dev, developer.to_string()));
        }
        if !publisher.is_empty() {
            rows.push((lc.fp_details_publisher, publisher.to_string()));
        }
        if !release_date.is_empty() {
            rows.push((lc.fp_details_date, release_date.to_string()));
        }
        rows.push((lc.fp_details_size, size_val));

        let title_line_h = 7.0 * title_scale + 10.0;
        let row_h = 40.0;
        let desc_line_h = 7.0 * DESC_SCALE + 9.0;
        // The text column (title + rows) and the cover sit side by side, so the
        // block is as tall as the taller of the two.
        let text_block_h = title_lines.len() as f32 * title_line_h + 24.0
            + rows.len() as f32 * row_h;
        let block_h = if cover.is_some() {
            text_block_h.max(COVER_H + 12.0)
        } else {
            text_block_h
        };
        let desc_block_h = if desc_lines.is_empty() {
            0.0
        } else {
            desc_lines.len() as f32 * desc_line_h + 20.0
        };
        let panel_h = 60.0 + block_h + desc_block_h + 64.0;
        let panel_x = (vw - PANEL_W) * 0.5;
        let panel_y = (vh - panel_h) * 0.5;
        let panel = Matrix {
            a: PANEL_W, b: 0.0, c: 0.0, d: panel_h,
            tx: swf::Twips::from_pixels(panel_x as f64),
            ty: swf::Twips::from_pixels(panel_y as f64),
        };
        <Self as CommandHandler>::draw_rect(self, swf::Color::from_rgba(0xF0_14_20_38), panel);
        <Self as CommandHandler>::draw_line_rect(self, swf::Color::from_rgb(0xFFFFFF, 255), panel);

        // Header.
        let hdr = lc.fp_details_title;
        let hw = self.measure_text(hdr, 2.0);
        self.draw_text(panel_x + (PANEL_W - hw) * 0.5, panel_y + 22.0, 2.0, hdr, swf::Color::from_rgb(0xFFD740, 255));

        // Cover on the left, aspect-preserved inside its slot.
        let block_top = panel_y + 60.0;
        if let Some((tex, cw, ch)) = cover {
            self.draw_textured_rect_cover(
                panel_x + 40.0,
                block_top,
                COVER_W,
                COVER_H,
                tex,
                cw,
                ch,
                1.0,
            );
        }

        // Title, left-aligned beside the cover (centring it would leave it
        // floating away from the rows it belongs to).
        let text_x = panel_x + text_x0;
        let text_w = PANEL_W - text_x0 - 40.0;
        let mut y = block_top;
        for line in &title_lines {
            self.draw_text(text_x, y, title_scale, line, swf::Color::from_rgb(0xFFFFFF, 255));
            y += title_line_h;
        }
        y += 24.0;

        // Info rows: "LABEL : value", truncated to the text column.
        let row_scale = 2.0;
        for (label, value) in &rows {
            let line = self.fit_text_mid(&std::format!("{} : {}", label, value), row_scale, text_w);
            self.draw_text(text_x, y, row_scale, &line, swf::Color::from_rgb(0xCCCCCC, 255));
            y += row_h;
        }

        // Blurb, full panel width under the cover + facts.
        if !desc_lines.is_empty() {
            let mut dy = block_top + block_h + 20.0;
            for line in &desc_lines {
                self.draw_text(
                    panel_x + 40.0,
                    dy,
                    DESC_SCALE,
                    line,
                    swf::Color::from_rgb(0xAABFD8, 255),
                );
                dy += desc_line_h;
            }
        }

        // Footer.
        let fw = self.measure_text(lc.fp_details_footer, 2.0);
        self.draw_text(panel_x + (PANEL_W - fw) * 0.5, panel_y + panel_h - 36.0, 2.0, lc.fp_details_footer, swf::Color::from_rgb(0x99AABB, 255));

        unsafe {
            glUseProgram(0);
            glBindVertexArray(0);
        }
        self.gl_state.invalidate();
    }

    /// Centered modal list for the JOUER sort picker (Y). `options` are the sort
    /// labels; `selection` highlights the active one. Self-contained (dims behind).
    pub fn draw_library_sort_modal(
        &mut self,
        selection: usize,
        options: &[&str],
        title: &str,
        footer: &str,
        dir_label: &str,
    ) {
        let frame = self.draw_modal_frame(
            MODAL_W,
            options.len(),
            None,
            false,
            title,
            Some(""), // reserve the subtitle band; we draw our own teal line below
            Some(footer),
        );
        // Direction indicator (toggled with X) — teal, in the subtitle slot.
        let dw = self.measure_text(dir_label, MODAL_SUB_SCALE);
        self.draw_text(
            frame.x + (frame.w - dw) * 0.5,
            frame.y + 75.0,
            MODAL_SUB_SCALE,
            dir_label,
            swf::Color::from_rgb(0x66DDCC, 255),
        );
        self.draw_modal_rows(&frame, selection, options);

        unsafe {
            glUseProgram(0);
            glBindVertexArray(0);
        }
        self.gl_state.invalidate();
    }

    /// Bug-report game picker (RÉGLAGES → SIGNALER UN BUG). A full-page
    /// scrollable list of game names — pick which `.swf` is broken. Mirrors the
    /// DistantFiles list layout (header + rows + scrollbar + footer).
    pub fn draw_library_bug_picker(
        &mut self,
        selection: usize,
        scroll_offset: usize,
        names: &[&str],
        visible_rows: usize,
        title: &str,
        footer: &str,
    ) {
        self.library_clear();
        let vw = self.dimensions.width as f32;

        // Header (drop shadow + amber, like the other list screens).
        let scale_t = 4.0;
        let tw = self.measure_text(title, scale_t);
        self.draw_text((vw - tw) * 0.5 + 3.0, 30.0 + 3.0, scale_t, title, swf::Color::from_rgb(0x000000, 255));
        self.draw_text((vw - tw) * 0.5, 30.0, scale_t, title, swf::Color::from_rgb(0xFFD740, 255));

        // Rows.
        const ROW_SCALE: f32 = 2.5;
        const ROW_SPACING: f32 = 50.0;
        let rows_top_y = 150.0;
        let rows_left_x = 80.0;
        let total = names.len();
        let now = unsafe { ruffle_tick_now() };
        // The list SLIDES to its new page instead of jumping a whole row at a
        // time. `scroll_off` is the eased pixel offset the rows are drawn
        // against; a scissor over the band turns the row arriving at each edge
        // into something that visibly slides in rather than appearing.
        let band_top = rows_top_y - 12.0;
        let band_bot = rows_top_y + visible_rows as f32 * ROW_SPACING - 4.0;
        let max_off = total.saturating_sub(visible_rows) as f32 * ROW_SPACING;
        let scroll_off = eased_scroll_px(
            (scroll_offset as f32 * ROW_SPACING).min(max_off),
            GLIDE_KEY_BUG,
            now,
        );
        let first = (scroll_off / ROW_SPACING).floor().max(0.0) as usize;
        let end = (first + visible_rows + 2).min(total);
        row_view_publish(RowView {
            key: GLIDE_KEY_BUG,
            kind: ui_screen_kind(),
            band_top,
            band_bot,
            row_h: ROW_SPACING,
            scroll_px: scroll_off,
            max_off,
            total: total as u32,
            visible: visible_rows as u32,
            base: 0,
        });
        self.set_clip(0.0, band_top, vw, band_bot - band_top);
        // Gliding bar + cursor, drawn under the rows, in the same eased space.
        //
        // The GLIDE is eased on top of the eased scroll, not instead of it. They
        // are two different movements: the list travelling to a new page, and
        // the cursor travelling to a new row. Dropping the second when the first
        // arrived left the bar jumping from row to row inside a list that slid
        // smoothly underneath it -- the two halves of one movement disagreeing,
        // which is the exact complaint the glide was added for.
        if selection < total {
            // Eased in CONTENT space, THEN scrolled -- not the other way round.
            // Easing the already-scrolled position eases the same movement
            // twice: during a scroll the rows travel at the scroll speed while
            // the bar chases them one step behind, which is the exact lag the
            // glide exists to remove. In content space the bar rides the scroll
            // exactly, and eases only when the SELECTION moves.
            let hy = eased_list_y(rows_top_y + selection as f32 * ROW_SPACING, GLIDE_KEY_BUG, now)
                - scroll_off;
            let bar_x = rows_left_x - 40.0;
            self.draw_selection_bar(bar_x, hy - 8.0, vw - bar_x - 40.0, ROW_SPACING - 12.0, 6.0);
            self.draw_text(
                rows_left_x - 30.0,
                hy,
                ROW_SCALE,
                ">",
                swf::Color::from_rgb(0xFFD740, 255),
            );
        }
        // Rows are published in ABSOLUTE index space -- the ones scrolled out of
        // view get a zero-size rect -- so a hit is directly the index the input
        // handler and the selection already speak in.
        let mut cells: std::vec::Vec<(f32, f32, f32, f32)> = std::vec![(0.0, 0.0, 0.0, 0.0); total];
        for abs_idx in first..end {
            let y = rows_top_y + abs_idx as f32 * ROW_SPACING - scroll_off;
            // The scissor decides what is SEEN; this decides what is TOUCHABLE.
            // A row caught halfway through an edge would otherwise take a tap
            // aimed at the header or at the row below it.
            if y >= rows_top_y - 1.0 && y + ROW_SPACING <= band_bot + 12.0 {
                cells[abs_idx] = (
                    rows_left_x - 40.0,
                    y - 8.0,
                    vw - (rows_left_x - 40.0) - 40.0,
                    ROW_SPACING,
                );
            }
            let is_sel = abs_idx == selection;
            let color = if is_sel {
                swf::Color::from_rgb(0xFFD740, 255)
            } else {
                swf::Color::from_rgb(0xCCCCCC, 255)
            };
            // Truncate the name to the row WIDTH, not to a character count: a
            // character is 6 units of pen here and 8 for anything drawn from
            // the shared font, so a Chinese title budgeted in characters ran a
            // third past the row and under the scrollbar.
            let display = self.fit_text(names[abs_idx], ROW_SCALE, vw - rows_left_x * 2.0);
            self.draw_text(rows_left_x, y, ROW_SCALE, &display, color);
        }
        self.clear_clip();

        ui_cells_publish(ui_screen_kind(), cells);

        // Scrollbar if needed.
        if total > visible_rows {
            let bar_x = vw - 30.0;
            let bar_top_y = rows_top_y;
            let bar_h_total = visible_rows as f32 * ROW_SPACING;
            let bar_h_thumb = (bar_h_total * visible_rows as f32 / total as f32).max(20.0);
            // Follows the EASED offset, so the thumb travels with the rows
            // instead of snapping a page ahead of them.
            let progress = if max_off > 0.0 { (scroll_off / max_off).clamp(0.0, 1.0) } else { 0.0 };
            let thumb_y = bar_top_y + (bar_h_total - bar_h_thumb) * progress;
            let track = Matrix {
                a: 4.0, b: 0.0, c: 0.0, d: bar_h_total,
                tx: swf::Twips::from_pixels(bar_x as f64),
                ty: swf::Twips::from_pixels(bar_top_y as f64),
            };
            <Self as CommandHandler>::draw_rect(self, swf::Color::from_rgba(0x40_99AABB), track);
            let thumb = Matrix {
                a: 4.0, b: 0.0, c: 0.0, d: bar_h_thumb,
                tx: swf::Twips::from_pixels(bar_x as f64),
                ty: swf::Twips::from_pixels(thumb_y as f64),
            };
            <Self as CommandHandler>::draw_rect(self, swf::Color::from_rgb(0xFFD740, 255), thumb);
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
        unsafe {
            glEnable(GL_BLEND);
            glBlendFunc(GL_SRC_ALPHA, GL_ONE_MINUS_SRC_ALPHA);
            glDisable(GL_STENCIL_TEST);
        }
        let vw = self.dimensions.width as f32;
        let vh = self.dimensions.height as f32;

        const SCALE: f32 = 2.5;
        const PAD: f32 = 28.0;
        const H: f32 = 64.0;
        let text_w = self.measure_text(msg, SCALE).max(1.0);
        let max_w = vw - 80.0;
        // Shrink the text if the banner would be wider than the screen allows.
        let (scale, w) = if text_w + PAD * 2.0 > max_w {
            (SCALE * (max_w - PAD * 2.0) / text_w, max_w)
        } else {
            (SCALE, text_w + PAD * 2.0)
        };
        let x = (vw - w) * 0.5;
        let y = vh - H - 40.0;

        // (background ARGB, border RGB) per kind.
        let (bg, border) = match kind {
            1 => (0xF0_3A_12_12u32, 0xFF5040u32), // error  — red
            2 => (0xF0_10_28_3Au32, 0x2196F3u32), // info   — blue
            _ => (0xF0_12_30_1Eu32, 0x4CAF50u32), // success — green
        };
        let panel = Matrix {
            a: w, b: 0.0, c: 0.0, d: H,
            tx: swf::Twips::from_pixels(x as f64),
            ty: swf::Twips::from_pixels(y as f64),
        };
        <Self as CommandHandler>::draw_rect(self, swf::Color::from_rgba(bg), panel);
        <Self as CommandHandler>::draw_line_rect(self, swf::Color::from_rgb(border, 255), panel);

        let tw = self.measure_text(msg, scale);
        self.draw_text(
            x + (w - tw) * 0.5,
            y + (H - 7.0 * scale) * 0.5,
            scale,
            msg,
            swf::Color::from_rgb(0xFFFFFF, 255),
        );

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

    /// Language picker: a grid of pixel flags, each with its native name under it
    /// (issue: the list grew long). `selection` indexes `loc::PICKER_LANGS`;
    /// `languages` is the parallel list of native names. The active language keeps
    /// a teal tint even when the cursor is elsewhere.
    pub fn draw_library_language_picker(&mut self, selection: usize, languages: &[&str]) {
        let lc = crate::loc::s();
        const COLS: usize = 3;
        const CELL_W: f32 = 210.0;
        const CELL_H: f32 = 104.0;
        const FLAG_W: f32 = 96.0;
        const FLAG_H: f32 = 64.0;
        let n = languages.len();
        let rows_n = n.div_ceil(COLS);
        let grid_w = COLS as f32 * CELL_W;
        let grid_h = rows_n as f32 * CELL_H;
        let fixed_h = MODAL_PAD_TOP_TIGHT + grid_h + MODAL_PAD_BOTTOM;
        let frame = self.draw_modal_frame(
            MODAL_W_WIDE,
            0,
            Some(fixed_h),
            false,
            lc.lang_title,
            None,
            Some(lc.lang_footer),
        );
        let active = crate::loc::current().index();
        let grid_left = frame.x + (frame.w - grid_w) * 0.5;
        let grid_top = frame.rows_top();
        // ONE tint, eased in both axes and drawn before the cells: this is a
        // grid, so the cursor moves sideways as often as it moves down, and a
        // tint that only slid vertically would have looked broken half the time.
        if selection < n {
            let now = unsafe { ruffle_tick_now() };
            let tx = grid_left + (selection % COLS) as f32 * CELL_W;
            let ty = grid_top + (selection / COLS) as f32 * CELL_H;
            let hx = eased_list_x(tx, GLIDE_KEY_LANG, now);
            let hy = eased_list_y(ty, GLIDE_KEY_LANG, now);
            // Radius 0: `round_corners` paints the PAGE colour, and this sits on
            // a modal panel — cutting here would leave four navy notches on it.
            self.draw_selection_bar(hx + 4.0, hy, CELL_W - 8.0, CELL_H - 6.0, 0.0);
        }
        ui_cells_publish(
            ui_screen_kind(),
            (0..n)
                .map(|i| {
                    (
                        grid_left + (i % COLS) as f32 * CELL_W,
                        grid_top + (i / COLS) as f32 * CELL_H,
                        CELL_W,
                        CELL_H,
                    )
                })
                .collect(),
        );
        for (i, name) in languages.iter().enumerate() {
            let col = i % COLS;
            let row = i / COLS;
            let cell_x = grid_left + col as f32 * CELL_W;
            let cell_y = grid_top + row as f32 * CELL_H;
            let is_sel = i == selection;
            let is_active = i == active;
            let fx = cell_x + (CELL_W - FLAG_W) * 0.5;
            let fy = cell_y + 6.0;
            if let Some(&l) = crate::loc::PICKER_LANGS.get(i) {
                self.draw_flag(l, fx, fy, FLAG_W, FLAG_H);
            }
            let border = if is_sel {
                MODAL_ROW_SEL_COL
            } else if is_active {
                0x66DDCC
            } else {
                0x3A4450
            };
            self.flag_stroke(fx, fy, FLAG_W, FLAG_H, border);
            // Native name under the flag, centred and shrunk to fit the cell.
            let name_col = if is_sel {
                MODAL_ROW_SEL_COL
            } else if is_active {
                0x66DDCC
            } else {
                MODAL_ROW_COL
            };
            let base = 2.0;
            let maxw = CELL_W - 16.0;
            let full = self.measure_text(name, base);
            let scale = if full > maxw { base * maxw / full } else { base };
            let nw = self.measure_text(name, scale);
            self.draw_text(
                cell_x + (CELL_W - nw) * 0.5,
                fy + FLAG_H + 10.0,
                scale,
                name,
                swf::Color::from_rgb(name_col, 255),
            );
        }

        unsafe {
            glUseProgram(0);
            glBindVertexArray(0);
        }
        self.gl_state.invalidate();
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

    /// Confirm removing a URL from the DISTANT history (X on DistantIdle).
    /// Shows the URL + a confirmation prompt; reuses the red "danger" theme.
    pub fn draw_library_history_delete_confirm(&mut self, url: &str) {
        self.library_clear();
        // Fixed-height danger frame (wide tier — the URL line is long); footer
        // reuses the generic "A: DELETE   B: CANCEL".
        let lc = crate::loc::s();
        let frame = self.draw_modal_frame(
            MODAL_W_WIDE,
            0,
            Some(300.0),
            true,
            lc.histdel_title,
            None,
            Some(lc.del_footer),
        );

        // The URL, truncated to the panel width.
        const URL_SCALE: f32 = 2.0;
        let budget = ((frame.w - 60.0) / (6.0 * URL_SCALE)) as usize;
        let display = truncate_tail(url, budget);
        let uw = self.measure_text(&display, URL_SCALE);
        self.draw_text(
            frame.x + (frame.w - uw) * 0.5,
            frame.y + 110.0,
            URL_SCALE,
            &display,
            swf::Color::from_rgb(0xFFFFFF, 255),
        );

        // Confirmation prompt.
        let mw = self.measure_text(lc.histdel_msg, MODAL_SUB_SCALE);
        self.draw_text(
            frame.x + (frame.w - mw) * 0.5,
            frame.y + 165.0,
            MODAL_SUB_SCALE,
            lc.histdel_msg,
            swf::Color::from_rgb(0xFFEEDD, 255),
        );

        unsafe {
            glUseProgram(0);
            glBindVertexArray(0);
        }
        self.gl_state.invalidate();
    }

    pub fn draw_library_dim_backdrop(&mut self) {
        unsafe {
            glDisable(GL_STENCIL_TEST);
            glClearColor(0.04, 0.06, 0.10, 1.0);
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
    fn mask_push(&mut self) {
        self.push_mask_window = self.push_mask_window.saturating_add(1);
        self.mask.writing = true;
        self.mask.depth = self.mask.depth.saturating_add(1);
        unsafe {
            glEnable(GL_STENCIL_TEST);
            // Mask shape writes stencil only (no color): increment coverage.
            glColorMask(GL_FALSE, GL_FALSE, GL_FALSE, GL_FALSE);
            glStencilMask(0xFF);
            glStencilFunc(GL_ALWAYS, 0, 0xFF);
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
            glStencilFunc(GL_ALWAYS, 0, 0xFF);
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
                glStencilFunc(GL_ALWAYS, 0, 0xFF);
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
/// Phase of the shared selection pulse, in [-1, 1], from a tick count.
///
/// Every cursor screen re-derived `sin(2*PI*t / 1.6)` with its own copy of the
/// period, so the three cursors were in step only by luck: a screen that typed a
/// different period would breathe out of time with the rest, for no reason a
/// reader of either site could see.
fn selection_pulse(ticks: u64) -> f32 {
    let secs = ticks as f64 / (unsafe { ruffle_tick_freq() } as f64);
    approx_sin(secs as f32 * (2.0 * core::f32::consts::PI / 1.6))
}

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
        let bitmap_fill_count = mesh
            .draws
            .iter()
            .filter(|d| matches!(d.draw_type, DrawType::Bitmap(_)))
            .count();
        let resolve_bitmaps = bitmap_fill_count <= PER_SHAPE_BITMAP_BUDGET;
        for draw in &mesh.draws {
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
        }

        let mut draws: Vec<GpuDraw> = Vec::with_capacity(mesh.draws.len());
        for (idx, draw) in mesh.draws.iter().enumerate() {
            let meta_ref = bitmap_metas[idx].as_ref();
            let standalone = bitmap_standalones[idx].clone();
            if let Some(mut gpu) = upload_draw(
                draw,
                &gradient_textures,
                meta_ref,
                standalone,
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
        PRIM_OFF_PIX_CUR.fetch_add(
            (tex_w as u64) * (tex_h as u64),
            std::sync::atomic::Ordering::Relaxed,
        );
        let temp = {
            let _t = PrimTimer::new(&PRIM_OFF_ALLOC_CUR);
            self.acquire_offscreen_temp(tex_w, tex_h)?
        };
        let temp_id = temp.texture;

        // 1. Seed temp with the BitmapData's current content (premultiplied).
        {
            let _t = PrimTimer::new(&PRIM_OFF_READBACK_CUR);
            match backing {
                Backing::Standalone(s_tex) => {
                    // Standalone already stores premultiplied — straight copy.
                    self.blit_identity(
                        s_tex, tex_w, tex_h, (0, 0), (tex_w, tex_h),
                        temp_id, (0, 0), tex_w, tex_h,
                    );
                }
                Backing::Atlas { tex, base_x, base_y, atlas_w, atlas_h } => {
                    // Atlas stores STRAIGHT alpha — premultiply it into temp.
                    self.blit_premult(
                        tex, atlas_w, atlas_h, (base_x, base_y), (tex_w, tex_h),
                        temp_id, (0, 0), tex_w, tex_h,
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
                Backing::Standalone(s_tex) => {
                    self.blit_identity(
                        temp_id, tex_w, tex_h, (0, 0), (tex_w, tex_h),
                        s_tex, (0, 0), tex_w, tex_h,
                    );
                }
                Backing::Atlas { tex, base_x, base_y, .. } => {
                    self.blit_unpremult(
                        temp_id, tex_w, tex_h, (0, 0), (tex_w, tex_h),
                        tex, (base_x as i32, base_y as i32), tex_w, tex_h,
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
                tex_w, tex_h,
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
            if dst_atlas {
                self.blit_unpremult(temp_dst.texture, fw, fh, (0, 0), (fw, fh), dst_tex, (dx, dy), fw, fh)
            } else {
                self.blit_identity(temp_dst.texture, fw, fh, (0, 0), (fw, fh), dst_tex, (dx, dy), fw, fh)
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
                self.vertex_arena.free_region(f.vbo_offset, f.vbo_size);
                self.index_arena.free_region(f.ibo_offset, f.ibo_size);
            }
        }
        // Drain atlas releases (issue #56b): a dropped bitmap's AtlasTicket enqueued
        // its atlas index; decrement the live count and free the 16 MB texture when
        // it reaches 0, so re-cached large offscreen surfaces don't leak to OOM.
        {
            let mut pending = PENDING_ATLAS_RELEASE.lock().unwrap();
            for idx in pending.drain(..) {
                if let Some(atlas) = self.atlases.get_mut(idx) {
                    if atlas.texture != 0 {
                        atlas.live = atlas.live.saturating_sub(1);
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
        let mut filter_chains_run: usize = 0;
        for entry in cache_entries {
            let Some(standalone) = as_standalone_bitmap(&entry.handle) else {
                self.warn_once(b"cache_entry: non-standalone handle (skipped)\n\0");
                continue;
            };
            let dst_tex = standalone.0.texture;
            let w = standalone.0.width;
            let h = standalone.0.height;

            // Step 1: render the content into entry.handle (ALWAYS — see above).
            self.render_commands_to_texture(dst_tex, w, h, entry.commands, Some(entry.clear));
            if entry.filters.is_empty() {
                continue;
            }
            // Over the per-frame filter budget → leave this entry unfiltered for
            // this frame: text/shape is still present (step 1), just without the
            // bevel/glow border this frame.
            if filter_chains_run >= FILTER_CHAINS_PER_FRAME_BUDGET {
                continue;
            }
            filter_chains_run += 1;

            // Step 2: filter chain using the (now bounded) FilterTexturePool.
            // The first source is entry.handle.texture itself; each successful
            // filter writes into a fresh pool temp and the previous owned temp
            // is released. acquire() can return None (pool/​GL exhaustion guard)
            // — we break and keep whatever we have, so we never feed a 0 texture
            // to the shaders (the old crash).
            let mut current_tex = dst_tex;
            let mut current_owned: Option<StandaloneTexture> = None;
            for filter in entry.filters {
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
            let v_frag = self.vertex_arena.free.len();
            let i_frag = self.index_arena.free.len();
            // Actual CPU clock (MHz) + dock state, so we can read whether
            // CpuBoostMode is holding the A57 at 1785 MHz during heavy AVM1
            // scenes (the water lake) — confirming if any CPU headroom remains.
            let cpu_mhz = unsafe { ruffle_cpu_clock_hz() } / 1_000_000;
            let docked = unsafe { ruffle_is_docked() } != 0;
            let msg = std::format!(
                "f{}: fps={} cpu={}MHz dock={} tick={}ms render={}ms dc/win={} shapes={}(live {}) draws_live={} arena_v={}MB/peak{}MB(frag {}) arena_i={}MB/peak{}MB(frag {}) arenaDropV={} arenaDropI={} bitmaps={} atlases={} bigMB={}/{} bigA/F/D={}/{}/{} bdMB={} bitmap_draws={} offscreen={} sync={} filter={} fpool={} otpool={}/{}MB pushmask={} amask={} blend={} maskeddraw={} maskshape={} tickMax={}ms rndMax={}ms cacheMax={} ram={}MB/{}MB heap={}MB slabMB={}{} drawbox={} maxalpha={:.2}\n",
                self.frame_count,
                fps_str,
                cpu_mhz,
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
                unsafe { ruffle_heap_used() } / (1024 * 1024),
                crate::slab_bytes() / (1024 * 1024),
                // Sticky: no contiguous 32 MB was available at some point, so
                // small blocks are falling back to newlib. See `region_grow_failed`.
                if crate::region_grow_failed() { "!STARVED" } else { "" },
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
        let standalone = make_standalone_texture(width.get(), height.get())
            .ok_or(Error::TooLarge)?;
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
        if let Some(meta) = self.pack_into_atlas(&bytes, w, h) {
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
        // variant), but it never crashes. Genuine GL OOM / over GL_MAX still
        // returns TooLarge (Ruffle handles a None handle there without us
        // forcing it through the expect path on every frame).
        let Some(standalone) = make_standalone_texture(w, h) else {
            return Err(Error::TooLarge);
        };
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
        self.warn_once(b"register_bitmap: >2048 bitmap -> standalone texture (no crash)\n\0");
        self.bitmaps_registered = self.bitmaps_registered.wrapping_add(1);
        Ok(BitmapHandle(Arc::new(StandaloneBitmap(Arc::new(standalone)))))
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
        _smoothing: bool,
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
            self.use_bitmap(&world, &mult, &add, tex, &uv_remap);
            self.gl_state.bind_vao(self.bitmap_vao);
            self.draw_calls_this_window = self.draw_calls_this_window.saturating_add(1);
            // Standalone cache textures store PREMULTIPLIED alpha (the offscreen
            // render uses `glBlendFuncSeparate(ONE, ONE_MINUS_SRC_ALPHA)` for the
            // alpha channel + the glow shader outputs `color * alpha`). The
            // straight-alpha blend used for atlas bitmaps multiplies alpha a
            // second time, producing alpha² output — too faint for filter
            // results like DropShadow. Switch to premultiplied "over" blend
            // for the standalone draw, then restore.
            unsafe {
                glBlendFunc(GL_ONE, GL_ONE_MINUS_SRC_ALPHA);
                glDrawArrays(GL_TRIANGLES, 0, 6);
                glBlendFunc(GL_SRC_ALPHA, GL_ONE_MINUS_SRC_ALPHA);
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
        self.use_bitmap(&world, &mult, &add, tex, &uv_remap);
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
        // a bitmap — no smoothing, no pixel snapping, since it is already at
        // device resolution and the stage transform places it.
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
        self.render_bitmap(bitmap, flipped, false, PixelSnapping::Never);
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
                    local_matrix,
                    is_smoothed: _,
                    is_repeating,
                    standalone,
                } => {
                    if local_matrix.iter().any(|v| !v.is_finite()) {
                        continue;
                    }
                    // >2048 fill: sample its own texture. Otherwise the atlas.
                    let tex = if let Some(s) = standalone {
                        s.texture
                    } else {
                        let Some(atlas) = self.atlases.get(*atlas_index) else {
                            continue;
                        };
                        atlas.texture
                    };
                    self.bitmap_draws_emitted = self.bitmap_draws_emitted.wrapping_add(1);
                    self.use_shape_bitmap(
                        &world,
                        &mult,
                        &add,
                        tex,
                        local_matrix,
                        uv_remap,
                        *is_repeating,
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
