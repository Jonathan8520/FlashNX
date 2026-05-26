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
use ruffle_render::filters::Filter;
use ruffle_render::matrix::Matrix;
use ruffle_render::pixel_bender::{PixelBenderShader, PixelBenderShaderHandle};
use ruffle_render::pixel_bender_support::PixelBenderShaderArgument;
use ruffle_render::quality::StageQuality;
use ruffle_render::shape_utils::{DistilledShape, GradientType};
use ruffle_render::tessellator::{DrawType, Gradient, ShapeTessellator};
use ruffle_render::transform::Transform;
use std::sync::atomic::{AtomicU16, AtomicUsize, Ordering};
use std::sync::Mutex;

/// Pause-menu labels rendered by `draw_menu_overlay`. C++ maps the selected
/// index from this slice to an action (Resume / Touches / Restart / Quit).
/// Keep the order in sync with the `MENU_*` constants in `cpp/src/main.cpp`.
pub const MENU_ITEMS: &[&str] = &["REPRENDRE", "TOUCHES", "REDEMARRER", "QUITTER"];

/// 5×7 pixel glyphs for the pause menu. ASCII art keeps the data
/// hand-editable: each row is exactly 5 chars wide, ' ' = off, anything
/// else = on. `draw_text` upper-cases input before lookup, so we only
/// carry one case. Unknown chars render as blank (the cursor still
/// advances). Add more entries here if a future label needs new
/// characters.
type Glyph = [&'static str; 7];
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
    ('=', ["     ", "     ", "#####", "     ", "#####", "     ", "     "]),
    ('>', ["#    ", " #   ", "  #  ", "   # ", "  #  ", " #   ", "#    "]),
    (':', ["     ", "  #  ", "  #  ", "     ", "  #  ", "  #  ", "     "]),
    ('.', ["     ", "     ", "     ", "     ", "     ", " ##  ", " ##  "]),
    ('/', ["    #", "    #", "   # ", "  #  ", " #   ", "#    ", "#    "]),
];

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
const ARENA_VBO_SIZE: GLsizeiptr = 64 * 1024 * 1024;  // 64 MB
// IBO bumped to 32 MB after the first arena test (jetpack run 2026-05-25):
// after ~5 minutes of Mario 63 the index arena peaked at 13 MB (81 %) —
// uncomfortably close to OOM. 32 MB gives us 2× headroom for longer
// sessions and more index-heavy levels.
const ARENA_IBO_SIZE: GLsizeiptr = 32 * 1024 * 1024;  // 32 MB
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
    /// Failed-allocation diagnostic: when we ran out, log it once.
    oom_warned: bool,
}

impl BufferArena {
    fn new(target: GLenum, capacity: GLsizeiptr, align: GLsizeiptr) -> Self {
        let mut gl_id: GLuint = 0;
        unsafe {
            glGenBuffers(1, &mut gl_id);
            glBindBuffer(target, gl_id);
            glBufferData(target, capacity, core::ptr::null(), GL_DYNAMIC_DRAW);
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
        if !self.oom_warned {
            self.oom_warned = true;
            let msg = std::format!(
                "ARENA OOM: target=0x{:04X} capacity={} requested={} peak_in_use={}\n",
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
use swf::{BlendMode, Color, GradientSpread};

use crate::ffi::gl::*;
use crate::query_ram;

extern "C" {
    fn ruffle_log_cstr(msg: *const core::ffi::c_char);
    /// Monotonic tick counter (armGetSystemTick). Used for FPS heartbeat.
    fn ruffle_tick_now() -> u64;
    /// Tick frequency in Hz (~19.2 MHz on Switch). Constant after boot.
    fn ruffle_tick_freq() -> u64;
}

fn log(nul_terminated: &[u8]) {
    unsafe { ruffle_log_cstr(nul_terminated.as_ptr() as *const _) };
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
}

impl Atlas {
    fn new(size: u32) -> Self {
        let mut tex: GLuint = 0;
        unsafe {
            glGenTextures(1, &mut tex);
            glBindTexture(GL_TEXTURE_2D, tex);
            glPixelStorei(GL_UNPACK_ALIGNMENT, 1);
            glTexImage2D(
                GL_TEXTURE_2D,
                0,
                GL_RGBA8 as GLint,
                size as GLsizei,
                size as GLsizei,
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
            width: size,
            height: size,
            shelves: Vec::new(),
        }
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

    fn upload_region(&self, x: u32, y: u32, w: u32, h: u32, pixels: &[u8]) {
        unsafe {
            glBindTexture(GL_TEXTURE_2D, self.texture);
            glPixelStorei(GL_UNPACK_ALIGNMENT, 1);
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
        // Build a (w+2) × (h+2) buffer with edge replication.
        let pw = (w + 2) as usize;
        let ph = (h + 2) as usize;
        let mut buf = vec![0u8; pw * ph * 4];
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
}
impl BitmapHandleImpl for SwitchBitmapHandle {}

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
        /// owned by the atlas, not per-draw.
        atlas_index: usize,
        /// Atlas-space UV remap (origin.x, origin.y, scale.x, scale.y).
        uv_remap: [f32; 4],
        /// 3x3 column-major matrix mapping `a_pos` (shape pixels) to UV
        /// in [0, 1] of the source bitmap. Pre-inverted by
        /// `swf_bitmap_to_gl_matrix`.
        local_matrix: [GLfloat; 9],
        #[allow(dead_code)]
        is_smoothed: bool,
        #[allow(dead_code)]
        is_repeating: bool,
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

// ─── Stencil mask state ───────────────────────────────────────────────────────

#[derive(Default, Clone, Copy)]
struct MaskState {
    /// Nesting depth: 0 = no mask, N = drawing the Nth maskee.
    depth: u32,
    /// 0 when no mask, otherwise the stencil reference value drawn maskees
    /// must equal.
    active_value: u8,
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
    /// Diagnostic counters: how many shapes/bitmaps registered so far.
    shapes_registered: u32,
    bitmaps_registered: u32,
    bitmap_draws_emitted: u32,
    bitmap_render_count: u32,
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
    /// How many times Ruffle has called `render_offscreen` since boot —
    /// non-zero means something on stage uses `cacheAsBitmap` or a filter.
    /// Logged every heartbeat so we can correlate spikes with crashes.
    render_offscreen_calls: u32,
    /// How many times Ruffle has called `apply_filter` since boot.
    apply_filter_calls: u32,
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
            let Some(meta) = bitmap_meta else {
                return Some(GpuDraw {
                    vbo_offset,
                    vbo_size,
                    ibo_offset,
                    ibo_size,
                    num_indices: draw.indices.len() as GLsizei,
                    kind: DrawKind::Solid,
                });
            };
            // `b.matrix` maps `a_pos` (shape pixels) to UV in [0,1] of the
            // source bitmap. The shader composes with `u_uv_remap` to land
            // in the atlas sub-rect.
            let local_matrix = [
                b.matrix[0][0], b.matrix[0][1], b.matrix[0][2],
                b.matrix[1][0], b.matrix[1][1], b.matrix[1][2],
                b.matrix[2][0], b.matrix[2][1], b.matrix[2][2],
            ];
            DrawKind::Bitmap {
                atlas_index: meta.atlas_index,
                uv_remap: [meta.u0, meta.v0, meta.u1 - meta.u0, meta.v1 - meta.v0],
                local_matrix,
                is_smoothed: b.is_smoothed,
                is_repeating: b.is_repeating,
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

// ─── Backend implementation ───────────────────────────────────────────────────

impl SwitchRenderBackend {
    pub fn new(width: u32, height: u32) -> Option<Self> {
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

        let solid = build_solid_program()?;
        let bitmap_prog = build_bitmap_program()?;
        let shape_bitmap_prog = build_shape_bitmap_program()?;
        let gradient_prog = build_gradient_program()?;

        let (rect_vao, rect_vbo) = build_solid_quad();
        let (bitmap_vao, bitmap_vbo) = build_bitmap_quad();
        let (line_vao, line_vbo) = build_line_segment();
        let (line_rect_vao, line_rect_vbo) = build_line_rect();

        // Mega-buffer arena for all shape draws — see the BufferArena
        // comment block at the top of this file for the rationale.
        let vertex_arena = BufferArena::new(
            GL_ARRAY_BUFFER,
            ARENA_VBO_SIZE,
            ARENA_VBO_ALIGN,
        );
        let index_arena = BufferArena::new(
            GL_ELEMENT_ARRAY_BUFFER,
            ARENA_IBO_SIZE,
            ARENA_IBO_ALIGN,
        );
        let shape_vao = build_shape_arena_vao(vertex_arena.gl_id, index_arena.gl_id);

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
            gl_state: GlStateCache::default(),
            rect_vao,
            rect_vbo,
            bitmap_vao,
            bitmap_vbo,
            line_vao,
            line_vbo,
            line_rect_vao,
            line_rect_vbo,
            mask: MaskState::default(),
            warned_unsupported: 0,
            frame_count: 0,
            shapes_registered: 0,
            bitmaps_registered: 0,
            bitmap_draws_emitted: 0,
            heartbeat_tick: 0,
            draw_calls_this_window: 0,
            render_offscreen_calls: 0,
            apply_filter_calls: 0,
            filters_seen_mask: AtomicU16::new(0),
            bitmap_render_count: 0,
            atlases: Vec::new(),
            vertex_arena,
            index_arena,
            shape_vao,
        })
    }

    /// Build the 3x3 column-major matrix that combines (Flash 2x3 affine)
    /// with (pixels → NDC, Y flipped). Sent as the `u_world` uniform.
    fn world_matrix(&self, m: &Matrix) -> [GLfloat; 9] {
        let w = self.dimensions.width.max(1) as f32;
        let h = self.dimensions.height.max(1) as f32;
        let a = m.a;
        let b = m.b;
        let c = m.c;
        let d = m.d;
        let tx = m.tx.to_pixels() as f32;
        let ty = m.ty.to_pixels() as f32;
        let sx = 2.0 / w;
        let sy = -2.0 / h;
        [
            a * sx,
            b * sy,
            0.0,
            c * sx,
            d * sy,
            0.0,
            tx * sx - 1.0,
            ty * sy + 1.0,
            1.0,
        ]
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
        for (idx, atlas) in self.atlases.iter_mut().enumerate() {
            if let Some((x, y)) = atlas.pack(width, height) {
                atlas.upload_region_padded(x, y, width, height, pixels);
                let inv = 1.0 / ATLAS_SIZE as f32;
                return Some(SwitchBitmapHandle {
                    atlas_index: idx,
                    u0: x as f32 * inv,
                    v0: y as f32 * inv,
                    u1: (x + width) as f32 * inv,
                    v1: (y + height) as f32 * inv,
                    width,
                    height,
                });
            }
        }
        // No room — allocate a new atlas (16 MB GPU texture).
        let new_atlas_index = self.atlases.len();
        let msg = std::format!(
            "atlas: allocating #{} (16 MB) for {}x{}\n",
            new_atlas_index, width, height,
        );
        let mut bytes = msg.into_bytes();
        bytes.push(0);
        unsafe { ruffle_log_cstr(bytes.as_ptr() as *const _) };
        let mut atlas = Atlas::new(ATLAS_SIZE);
        let Some((x, y)) = atlas.pack(width, height) else {
            return None;
        };
        atlas.upload_region_padded(x, y, width, height, pixels);
        self.atlases.push(atlas);
        let inv = 1.0 / ATLAS_SIZE as f32;
        Some(SwitchBitmapHandle {
            atlas_index: new_atlas_index,
            u0: x as f32 * inv,
            v0: y as f32 * inv,
            u1: (x + width) as f32 * inv,
            v1: (y + height) as f32 * inv,
            width,
            height,
        })
    }

    fn warn_once(&mut self, msg: &[u8]) {
        if self.warned_unsupported < 8 {
            self.warned_unsupported += 1;
            log(msg);
        }
    }

    /// Draw a small white crosshair at the given screen pixel position.
    /// Intended to be called *after* `submit_frame` has returned so it
    /// overlays the player's rendering rather than getting cleared away.
    /// Re-binds the GL state we'd left in a fresh state at end of submit.
    pub fn draw_cursor_overlay(&mut self, x: f32, y: f32, clicked: bool) {
        const BAR_W: f32 = 24.0;
        const BAR_H: f32 = 4.0;
        // Red when clicked, white otherwise. Helps confirm clicks register.
        let color = if clicked {
            swf::Color::from_rgb(0xFF1744, 255)
        } else {
            swf::Color::from_rgb(0xFFFFFF, 255)
        };
        // Horizontal bar centred on (x, y).
        let h_mat = Matrix {
            a: BAR_W,
            b: 0.0,
            c: 0.0,
            d: BAR_H,
            tx: swf::Twips::from_pixels((x - BAR_W * 0.5) as f64),
            ty: swf::Twips::from_pixels((y - BAR_H * 0.5) as f64),
        };
        // Vertical bar.
        let v_mat = Matrix {
            a: BAR_H,
            b: 0.0,
            c: 0.0,
            d: BAR_W,
            tx: swf::Twips::from_pixels((x - BAR_H * 0.5) as f64),
            ty: swf::Twips::from_pixels((y - BAR_W * 0.5) as f64),
        };
        unsafe {
            glEnable(GL_BLEND);
            glBlendFunc(GL_SRC_ALPHA, GL_ONE_MINUS_SRC_ALPHA);
            glDisable(GL_STENCIL_TEST);
        }
        // Reuse CommandHandler's draw_rect path. It binds program + VAO and
        // uploads a fresh dynamic quad each call.
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

    /// Draw an ASCII string with the embedded 5x7 pixel font (see `GLYPHS`).
    /// `x`, `y` are top-left in screen pixels. Each lit glyph pixel becomes
    /// a `scale × scale` solid rect drawn via the same path as `draw_rect`.
    /// Unknown chars render as blank space.
    pub fn draw_text(&mut self, x: f32, y: f32, scale: f32, text: &str, color: swf::Color) {
        let mut cur_x = x;
        for ch in text.chars() {
            // Uppercase fold — our font only carries A-Z.
            let lookup = ch.to_ascii_uppercase();
            if let Some((_, pattern)) = GLYPHS.iter().find(|(c, _)| *c == lookup) {
                for (row_idx, row_str) in pattern.iter().enumerate() {
                    for (col_idx, pixel) in row_str.chars().enumerate() {
                        if pixel != ' ' {
                            let px = cur_x + col_idx as f32 * scale;
                            let py = y + row_idx as f32 * scale;
                            let mat = Matrix {
                                a: scale,
                                b: 0.0,
                                c: 0.0,
                                d: scale,
                                tx: swf::Twips::from_pixels(px as f64),
                                ty: swf::Twips::from_pixels(py as f64),
                            };
                            <Self as CommandHandler>::draw_rect(self, color, mat);
                        }
                    }
                }
            }
            // Advance by 6 px (5-wide glyph + 1-px gap), scaled.
            cur_x += 6.0 * scale;
        }
    }

    /// Measure rendered width of `text` in pixels at the given scale.
    /// Lets the menu centre items horizontally.
    pub fn measure_text(&self, text: &str, scale: f32) -> f32 {
        text.chars().count() as f32 * 6.0 * scale
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
        // Re-bind blend / disable stencil, same as the cursor overlay.
        unsafe {
            glEnable(GL_BLEND);
            glBlendFunc(GL_SRC_ALPHA, GL_ONE_MINUS_SRC_ALPHA);
            glDisable(GL_STENCIL_TEST);
        }
        let vw = self.dimensions.width as f32;
        let vh = self.dimensions.height as f32;

        // Full-screen dim backdrop (50 % black). Hides the game so the
        // user's eye snaps to the menu.
        let backdrop = Matrix {
            a: vw,
            b: 0.0,
            c: 0.0,
            d: vh,
            tx: swf::Twips::from_pixels(0.0),
            ty: swf::Twips::from_pixels(0.0),
        };
        // 50 % black backdrop (AA=0x80).
        <Self as CommandHandler>::draw_rect(self, swf::Color::from_rgba(0x80_00_00_00), backdrop);

        // Centred panel — sized so 1280x720 fits 3 items + title comfortably.
        const PANEL_W: f32 = 520.0;
        const PANEL_H: f32 = 380.0;
        let panel_x = (vw - PANEL_W) * 0.5;
        let panel_y = (vh - PANEL_H) * 0.5;
        let panel = Matrix {
            a: PANEL_W,
            b: 0.0,
            c: 0.0,
            d: PANEL_H,
            tx: swf::Twips::from_pixels(panel_x as f64),
            ty: swf::Twips::from_pixels(panel_y as f64),
        };
        // Dark blue-ish panel background (alpha 240/255 → mostly opaque).
        // Dark navy panel at ~94% alpha so a hint of the paused frame shows
        // through. AARRGGBB = F0 14 20 38.
        <Self as CommandHandler>::draw_rect(self, swf::Color::from_rgba(0xF0_14_20_38), panel);
        // 1-px-style border via line rect at panel scale.
        let border = Matrix {
            a: PANEL_W,
            b: 0.0,
            c: 0.0,
            d: PANEL_H,
            tx: swf::Twips::from_pixels(panel_x as f64),
            ty: swf::Twips::from_pixels(panel_y as f64),
        };
        <Self as CommandHandler>::draw_line_rect(
            self,
            swf::Color::from_rgb(0xFFFFFF, 255),
            border,
        );

        // Title.
        const TITLE_SCALE: f32 = 5.0;
        let title = "PAUSE";
        let title_w = self.measure_text(title, TITLE_SCALE);
        let title_x = panel_x + (PANEL_W - title_w) * 0.5;
        let title_y = panel_y + 30.0;
        self.draw_text(
            title_x,
            title_y,
            TITLE_SCALE,
            title,
            swf::Color::from_rgb(0xFFFFFF, 255),
        );

        // Menu items.
        const ITEM_SCALE: f32 = 3.0;
        const ITEM_SPACING: f32 = 50.0;
        let items_y = panel_y + 130.0;
        let item_color_selected = swf::Color::from_rgb(0xFFD740, 255); // amber
        let item_color_normal = swf::Color::from_rgb(0xCCCCCC, 255);
        // Pre-measure the longest item so all rows share a left margin.
        let longest = MENU_ITEMS
            .iter()
            .map(|s| s.chars().count())
            .max()
            .unwrap_or(0) as f32;
        let block_w = (longest + 2.0) * 6.0 * ITEM_SCALE; // 2 chars left padding for ">  "
        let items_x = panel_x + (PANEL_W - block_w) * 0.5;
        for (i, item) in MENU_ITEMS.iter().enumerate() {
            let y = items_y + i as f32 * ITEM_SPACING;
            let color = if i == selected {
                item_color_selected
            } else {
                item_color_normal
            };
            if i == selected {
                self.draw_text(items_x, y, ITEM_SCALE, ">", color);
            }
            // Always render label at the same x (cursor occupies first slot).
            let label_x = items_x + 2.0 * 6.0 * ITEM_SCALE;
            self.draw_text(label_x, y, ITEM_SCALE, item, color);
        }

        // Footer help line.
        const HELP_SCALE: f32 = 2.0;
        let help = "A:OK   B:ANNULER   HAUT/BAS:NAV";
        let help_w = self.measure_text(help, HELP_SCALE);
        let help_x = panel_x + (PANEL_W - help_w) * 0.5;
        let help_y = panel_y + PANEL_H - 40.0;
        self.draw_text(
            help_x,
            help_y,
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

    /// TOUCHES list screen — the keymap editor. Shows up to `visible_rows`
    /// entries of `bindings` (Switch-button name + current Flash-key
    /// binding) starting at `scroll_offset`. The row at `selection` is
    /// highlighted in amber with a `>` cursor.
    pub fn draw_touches_list(
        &mut self,
        selection: usize,
        scroll_offset: usize,
        bindings: &[(&str, Option<std::string::String>)],
        visible_rows: usize,
    ) {
        unsafe {
            glEnable(GL_BLEND);
            glBlendFunc(GL_SRC_ALPHA, GL_ONE_MINUS_SRC_ALPHA);
            glDisable(GL_STENCIL_TEST);
        }
        let vw = self.dimensions.width as f32;
        let vh = self.dimensions.height as f32;

        // Same backdrop + panel framing as the pause menu — visually links
        // the two screens.
        let backdrop = Matrix {
            a: vw, b: 0.0, c: 0.0, d: vh,
            tx: swf::Twips::from_pixels(0.0),
            ty: swf::Twips::from_pixels(0.0),
        };
        <Self as CommandHandler>::draw_rect(self, swf::Color::from_rgba(0x80_00_00_00), backdrop);

        const PANEL_W: f32 = 720.0;
        const PANEL_H: f32 = 600.0;
        let panel_x = (vw - PANEL_W) * 0.5;
        let panel_y = (vh - PANEL_H) * 0.5;
        let panel = Matrix {
            a: PANEL_W, b: 0.0, c: 0.0, d: PANEL_H,
            tx: swf::Twips::from_pixels(panel_x as f64),
            ty: swf::Twips::from_pixels(panel_y as f64),
        };
        <Self as CommandHandler>::draw_rect(self, swf::Color::from_rgba(0xF0_14_20_38), panel);
        <Self as CommandHandler>::draw_line_rect(
            self,
            swf::Color::from_rgb(0xFFFFFF, 255),
            panel,
        );

        // Title.
        const TITLE_SCALE: f32 = 4.0;
        let title = "TOUCHES";
        let title_w = self.measure_text(title, TITLE_SCALE);
        self.draw_text(
            panel_x + (PANEL_W - title_w) * 0.5,
            panel_y + 25.0,
            TITLE_SCALE,
            title,
            swf::Color::from_rgb(0xFFFFFF, 255),
        );

        // Rows.
        const ROW_SCALE: f32 = 3.0;
        const ROW_SPACING: f32 = 50.0;
        let rows_top_y = panel_y + 130.0;
        let rows_left_x = panel_x + 80.0;
        // Right column = binding value, aligned to a fixed x.
        let value_col_x = panel_x + 360.0;

        let total = bindings.len();
        let end = (scroll_offset + visible_rows).min(total);
        for (visible_idx, abs_idx) in (scroll_offset..end).enumerate() {
            let (btn, binding) = &bindings[abs_idx];
            let y = rows_top_y + visible_idx as f32 * ROW_SPACING;
            let is_sel = abs_idx == selection;
            let color = if is_sel {
                swf::Color::from_rgb(0xFFD740, 255) // amber
            } else {
                swf::Color::from_rgb(0xCCCCCC, 255)
            };
            if is_sel {
                self.draw_text(rows_left_x - 30.0, y, ROW_SCALE, ">", color);
            }
            self.draw_text(rows_left_x, y, ROW_SCALE, btn, color);
            let value_str = binding
                .as_deref()
                .unwrap_or("(aucune)");
            // Brackets around the value to suggest "editable field".
            let bracketed = std::format!("[ {} ]", value_str);
            self.draw_text(value_col_x, y, ROW_SCALE, &bracketed, color);
        }

        // Scroll indicator on the right edge if the list is longer than
        // what's visible.
        if total > visible_rows {
            let bar_x = panel_x + PANEL_W - 30.0;
            let bar_top_y = rows_top_y;
            let bar_h_total = visible_rows as f32 * ROW_SPACING;
            let bar_h_thumb = (bar_h_total * visible_rows as f32 / total as f32).max(20.0);
            let progress = scroll_offset as f32 / (total - visible_rows) as f32;
            let thumb_y = bar_top_y + (bar_h_total - bar_h_thumb) * progress;
            // Bar track (faint).
            let track = Matrix {
                a: 4.0, b: 0.0, c: 0.0, d: bar_h_total,
                tx: swf::Twips::from_pixels(bar_x as f64),
                ty: swf::Twips::from_pixels(bar_top_y as f64),
            };
            <Self as CommandHandler>::draw_rect(self, swf::Color::from_rgba(0x40_99AABB), track);
            // Thumb.
            let thumb = Matrix {
                a: 4.0, b: 0.0, c: 0.0, d: bar_h_thumb,
                tx: swf::Twips::from_pixels(bar_x as f64),
                ty: swf::Twips::from_pixels(thumb_y as f64),
            };
            <Self as CommandHandler>::draw_rect(self, swf::Color::from_rgb(0xFFD740, 255), thumb);
        }

        // Footer.
        const HELP_SCALE: f32 = 2.0;
        let help = "A:EDITER   B:RETOUR   HAUT/BAS:NAV";
        let help_w = self.measure_text(help, HELP_SCALE);
        self.draw_text(
            panel_x + (PANEL_W - help_w) * 0.5,
            panel_y + PANEL_H - 40.0,
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

    /// TOUCHES dropdown — shown when the user presses A on a list row. Lists
    /// the available Flash-key options; A on a row commits, B cancels.
    pub fn draw_touches_dropdown(
        &mut self,
        button_name: &str,
        selection: usize,
        options: &[&str],
    ) {
        unsafe {
            glEnable(GL_BLEND);
            glBlendFunc(GL_SRC_ALPHA, GL_ONE_MINUS_SRC_ALPHA);
            glDisable(GL_STENCIL_TEST);
        }
        let vw = self.dimensions.width as f32;
        let vh = self.dimensions.height as f32;

        // Full-screen dim (slightly deeper than the list backdrop so the
        // dropdown reads as a modal-over-modal).
        let backdrop = Matrix {
            a: vw, b: 0.0, c: 0.0, d: vh,
            tx: swf::Twips::from_pixels(0.0),
            ty: swf::Twips::from_pixels(0.0),
        };
        <Self as CommandHandler>::draw_rect(self, swf::Color::from_rgba(0xB0_00_00_00), backdrop);

        const PANEL_W: f32 = 480.0;
        let row_h: f32 = 40.0;
        let panel_h = 130.0 + options.len() as f32 * row_h + 60.0;
        let panel_x = (vw - PANEL_W) * 0.5;
        let panel_y = (vh - panel_h) * 0.5;
        let panel = Matrix {
            a: PANEL_W, b: 0.0, c: 0.0, d: panel_h,
            tx: swf::Twips::from_pixels(panel_x as f64),
            ty: swf::Twips::from_pixels(panel_y as f64),
        };
        <Self as CommandHandler>::draw_rect(self, swf::Color::from_rgba(0xF0_14_20_38), panel);
        <Self as CommandHandler>::draw_line_rect(
            self,
            swf::Color::from_rgb(0xFFFFFF, 255),
            panel,
        );

        // Title: "A → ?" (Switch button → which Flash key).
        const TITLE_SCALE: f32 = 3.0;
        let title = std::format!("{} ->", button_name);
        let title_w = self.measure_text(&title, TITLE_SCALE);
        self.draw_text(
            panel_x + (PANEL_W - title_w) * 0.5,
            panel_y + 25.0,
            TITLE_SCALE,
            &title,
            swf::Color::from_rgb(0xFFFFFF, 255),
        );

        // Options.
        const OPT_SCALE: f32 = 2.5;
        let opts_top_y = panel_y + 110.0;
        let opts_left_x = panel_x + 100.0;
        for (i, opt) in options.iter().enumerate() {
            let y = opts_top_y + i as f32 * row_h;
            let is_sel = i == selection;
            let color = if is_sel {
                swf::Color::from_rgb(0xFFD740, 255)
            } else {
                swf::Color::from_rgb(0xCCCCCC, 255)
            };
            if is_sel {
                self.draw_text(opts_left_x - 30.0, y, OPT_SCALE, ">", color);
            }
            self.draw_text(opts_left_x, y, OPT_SCALE, opt, color);
        }

        // Footer.
        const HELP_SCALE: f32 = 2.0;
        let help = "A:OK   B:ANNULER";
        let help_w = self.measure_text(help, HELP_SCALE);
        self.draw_text(
            panel_x + (PANEL_W - help_w) * 0.5,
            panel_y + panel_h - 35.0,
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

        let title = "AUCUN JEU";
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

        let lines = [
            "DEPOSEZ DES FICHIERS .SWF DANS",
            "SDMC:/RUFFLE/   OU   SDMC:/SWITCH/RUFFLE/",
            "PUIS REDEMARREZ FLASHNX.",
        ];
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

        // Footer: only QUITTER works in the empty state.
        const HELP_SCALE: f32 = 2.0;
        let help = "-:QUITTER";
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

    /// Main library list. Reads `entries` (snapshot — caller drops the
    /// Mutex before this call), draws the title banner + N visible rows +
    /// metadata panel + footer help. Animations (cursor pulse, selection
    /// pulse) driven by `phase_ticks` (system ticks since the library
    /// opened — see `library::State::anim_origin_ticks`).
    #[allow(clippy::too_many_arguments)]
    pub fn draw_library_list(
        &mut self,
        selection: usize,
        scroll_offset: usize,
        entries: &[crate::library::Entry],
        visible_rows: usize,
        banner_tex: GLuint,
        banner_w: u32,
        banner_h: u32,
        phase_ticks: u64,
    ) {
        self.library_clear();
        let vw = self.dimensions.width as f32;
        let vh = self.dimensions.height as f32;

        // Phase in seconds (tick_freq ≈ 19.2 MHz on Switch — same as Ruffle's
        // pacing clock). We pass it through `sinf`-style math without
        // pulling in libm; (phase % period) is enough for visual pulses.
        let phase_s = (phase_ticks as f64) / (unsafe { ruffle_tick_freq() } as f64);
        // Sine via Taylor — pulls in no extra deps, accurate enough for
        // ±5 % amplitude visual pulses. Period 1.6 s reads as "subtle
        // breathing" rather than "annoying flash".
        let pulse = approx_sin(phase_s as f32 * (2.0 * core::f32::consts::PI / 1.6));

        // ── Banner (or ASCII title fallback) ─────────────────────────
        let banner_y = 24.0;
        if banner_tex != 0 && banner_w > 0 && banner_h > 0 {
            // Centre at 720×144 (asset spec). If banner is larger we down-
            // scale to fit the viewport with a 32-px side margin.
            let max_w = vw - 64.0;
            let scale = (max_w / banner_w as f32).min(1.0);
            let draw_w = banner_w as f32 * scale;
            let draw_h = banner_h as f32 * scale;
            let draw_x = (vw - draw_w) * 0.5;
            self.draw_textured_rect(draw_x, banner_y, draw_w, draw_h, banner_tex);
        } else {
            // ASCII fallback — same look as the empty-state title but
            // smaller so it still leaves room for the list.
            let title = "FLASHNX";
            let scale_title = 5.0;
            let title_w = self.measure_text(title, scale_title);
            // Drop shadow.
            self.draw_text(
                (vw - title_w) * 0.5 + 3.0,
                banner_y + 30.0 + 3.0,
                scale_title,
                title,
                swf::Color::from_rgb(0x000000, 255),
            );
            self.draw_text(
                (vw - title_w) * 0.5,
                banner_y + 30.0,
                scale_title,
                title,
                swf::Color::from_rgb(0xFFD740, 255),
            );
        }

        // ── Game list ───────────────────────────────────────────────────
        const ROW_SCALE: f32 = 3.0;
        const ROW_SPACING: f32 = 50.0;
        const CHIP_PAD: f32 = 12.0;
        const CHIP_SIZE: f32 = 18.0;
        let rows_top_y = banner_y + 170.0;
        let rows_left_x = 80.0;
        let chip_x = rows_left_x;
        let label_x = rows_left_x + CHIP_SIZE + CHIP_PAD * 2.0;

        let total = entries.len();
        let end = (scroll_offset + visible_rows).min(total);
        for (visible_idx, abs_idx) in (scroll_offset..end).enumerate() {
            let entry = &entries[abs_idx];
            let y = rows_top_y + visible_idx as f32 * ROW_SPACING;
            let is_sel = abs_idx == selection;

            // Color chip — small square in the per-game hash color.
            let chip_color = swf::Color::from_rgb(entry.color_chip, 255);
            let chip_mat = Matrix {
                a: CHIP_SIZE, b: 0.0, c: 0.0, d: CHIP_SIZE,
                tx: swf::Twips::from_pixels(chip_x as f64),
                ty: swf::Twips::from_pixels((y + 4.0) as f64),
            };
            <Self as CommandHandler>::draw_rect(self, chip_color, chip_mat);

            // Label color: amber for selection (pulsing), light grey otherwise.
            let color = if is_sel {
                // Pulse amber [0xFFD740] ↔ [0xFFEC8B]. Linear blend on each
                // channel; 0.5 (pulse+1)/2 stays in [0,1].
                let p = (pulse * 0.5) + 0.5;
                let r = (0xFF as f32 + (0xFF - 0xFF) as f32 * p) as u32;
                let g = (0xD7 as f32 + (0xEC - 0xD7) as f32 * p) as u32;
                let b = (0x40 as f32 + (0x8B - 0x40) as f32 * p) as u32;
                swf::Color::from_rgb((r << 16) | (g << 8) | b, 255)
            } else {
                swf::Color::from_rgb(0xCCCCCC, 255)
            };
            if is_sel {
                // Animated cursor — `►` shape, x position breathes by a few
                // pixels each cycle.
                let cursor_dx = pulse * 4.0;
                self.draw_text(label_x - 36.0 + cursor_dx, y, ROW_SCALE, ">", color);
            }
            // Truncate display name if it would overflow the visible area
            // (1280 - label_x - margin). 22 chars at scale 3 ≈ 396 px.
            let max_chars = 24usize;
            let display = if entry.display_name.chars().count() > max_chars {
                let mut t: std::string::String = entry.display_name.chars().take(max_chars - 1).collect();
                t.push('…');
                t
            } else {
                entry.display_name.clone()
            };
            self.draw_text(label_x, y, ROW_SCALE, &display, color);
        }

        // Scrollbar on the right edge if needed.
        if total > visible_rows {
            let bar_x = vw - 30.0;
            let bar_top_y = rows_top_y;
            let bar_h_total = visible_rows as f32 * ROW_SPACING;
            let bar_h_thumb = (bar_h_total * visible_rows as f32 / total as f32).max(20.0);
            let progress = scroll_offset as f32 / (total - visible_rows) as f32;
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

        // ── Metadata panel (bottom strip) ──────────────────────────────
        if let Some(entry) = entries.get(selection) {
            let panel_y = vh - 130.0;
            let panel_x = 40.0;
            let panel_w = vw - 80.0;
            let panel_h = 70.0;
            let panel_mat = Matrix {
                a: panel_w, b: 0.0, c: 0.0, d: panel_h,
                tx: swf::Twips::from_pixels(panel_x as f64),
                ty: swf::Twips::from_pixels(panel_y as f64),
            };
            <Self as CommandHandler>::draw_rect(
                self,
                swf::Color::from_rgba(0xC0_20_2C_40),
                panel_mat,
            );

            // Display name (larger).
            self.draw_text(
                panel_x + 20.0,
                panel_y + 10.0,
                3.0,
                &entry.display_name,
                swf::Color::from_rgb(0xFFFFFF, 255),
            );
            // Sub-line: size · compression · version · dims.
            let size_str = format_size_pretty(entry.size_bytes);
            let dims_str = match (entry.width_px, entry.height_px) {
                (Some(w), Some(h)) => std::format!("{}X{}", w, h),
                _ => std::string::String::from("?X?"),
            };
            let meta = std::format!(
                "{} // SWF V{} {} // {}",
                size_str,
                entry.swf_version,
                entry.compression_label,
                dims_str,
            );
            self.draw_text(
                panel_x + 20.0,
                panel_y + 42.0,
                2.0,
                &meta,
                swf::Color::from_rgb(0xAABFD8, 255),
            );

            // Tiny basename in lower-right (so the user always sees the
            // physical filename — matters when display_name diverges in
            // Phase 3.4.bis RENOMMER).
            let bn_str = std::format!("[{}]", entry.basename);
            let bn_w = self.measure_text(&bn_str, 1.5);
            self.draw_text(
                panel_x + panel_w - bn_w - 20.0,
                panel_y + panel_h - 22.0,
                1.5,
                &bn_str,
                swf::Color::from_rgb(0x7A8A9C, 255),
            );
        }

        // Footer.
        const HELP_SCALE: f32 = 2.0;
        let help = "A:JOUER   X:OPTIONS   -:QUITTER   HAUT/BAS:NAV";
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

    /// OPTIONS modal — small panel showing the game name + per-game options.
    /// v1: only TOUCHES + RETOUR.
    pub fn draw_library_options(
        &mut self,
        game_display_name: &str,
        selection: usize,
        options: &[&str],
    ) {
        unsafe {
            glEnable(GL_BLEND);
            glBlendFunc(GL_SRC_ALPHA, GL_ONE_MINUS_SRC_ALPHA);
            glDisable(GL_STENCIL_TEST);
        }
        let vw = self.dimensions.width as f32;
        let vh = self.dimensions.height as f32;

        let backdrop = Matrix {
            a: vw, b: 0.0, c: 0.0, d: vh,
            tx: swf::Twips::from_pixels(0.0),
            ty: swf::Twips::from_pixels(0.0),
        };
        <Self as CommandHandler>::draw_rect(self, swf::Color::from_rgba(0xB0_00_00_00), backdrop);

        const PANEL_W: f32 = 520.0;
        let row_h: f32 = 50.0;
        let panel_h = 180.0 + options.len() as f32 * row_h + 60.0;
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
        const TITLE_SCALE: f32 = 3.0;
        let header = "OPTIONS";
        let header_w = self.measure_text(header, TITLE_SCALE);
        self.draw_text(
            panel_x + (PANEL_W - header_w) * 0.5,
            panel_y + 25.0,
            TITLE_SCALE,
            header,
            swf::Color::from_rgb(0xFFFFFF, 255),
        );
        // Game name (sub-title).
        const SUB_SCALE: f32 = 2.0;
        // Truncate game name to fit the panel.
        let max_chars = 28usize;
        let sub = if game_display_name.chars().count() > max_chars {
            let mut t: std::string::String = game_display_name.chars().take(max_chars - 1).collect();
            t.push('…');
            t
        } else {
            game_display_name.to_string()
        };
        let sub_w = self.measure_text(&sub, SUB_SCALE);
        self.draw_text(
            panel_x + (PANEL_W - sub_w) * 0.5,
            panel_y + 75.0,
            SUB_SCALE,
            &sub,
            swf::Color::from_rgb(0xAABFD8, 255),
        );

        // Options list.
        const OPT_SCALE: f32 = 2.5;
        let opts_top_y = panel_y + 140.0;
        let opts_left_x = panel_x + 120.0;
        for (i, opt) in options.iter().enumerate() {
            let y = opts_top_y + i as f32 * row_h;
            let is_sel = i == selection;
            let color = if is_sel {
                swf::Color::from_rgb(0xFFD740, 255)
            } else {
                swf::Color::from_rgb(0xCCCCCC, 255)
            };
            if is_sel {
                self.draw_text(opts_left_x - 30.0, y, OPT_SCALE, ">", color);
            }
            self.draw_text(opts_left_x, y, OPT_SCALE, opt, color);
        }

        // Footer.
        const HELP_SCALE: f32 = 2.0;
        let help = "A:OK   B:RETOUR";
        let help_w = self.measure_text(help, HELP_SCALE);
        self.draw_text(
            panel_x + (PANEL_W - help_w) * 0.5,
            panel_y + panel_h - 38.0,
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

    /// Dim backdrop used when the menu module's TOUCHES editor is on top of
    /// the library (pre-launch keymap edit). Quick black fill — no library
    /// content underneath, no Ruffle render — just a flat backdrop so
    /// `menu::draw` sits on something solid.
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
    // We support depth up to 255 via the 8-bit stencil buffer; nested masks
    // bitwise-OR their values (1, 2, 4, ...).
    fn mask_push(&mut self) {
        self.mask.depth = self.mask.depth.saturating_add(1);
        unsafe {
            glEnable(GL_STENCIL_TEST);
            glClearStencil(self.mask.active_value as GLint);
            glClear(GL_STENCIL_BUFFER_BIT);
            // Draw mask into stencil only.
            glColorMask(GL_FALSE, GL_FALSE, GL_FALSE, GL_FALSE);
            glStencilMask(0xFF);
            let next_value = (self.mask.active_value | (1u8 << ((self.mask.depth - 1).min(7)))) & 0xFF;
            glStencilFunc(GL_ALWAYS, next_value as GLint, 0xFF);
            glStencilOp(GL_KEEP, GL_KEEP, GL_REPLACE);
        }
    }

    fn mask_activate(&mut self) {
        // Mask shape just finished writing to stencil. Switch to drawing
        // maskee, gated on stencil == new_value.
        self.mask.active_value =
            self.mask.active_value | (1u8 << ((self.mask.depth.saturating_sub(1)).min(7)));
        unsafe {
            glColorMask(GL_TRUE, GL_TRUE, GL_TRUE, GL_TRUE);
            glStencilMask(0);
            glStencilFunc(GL_EQUAL, self.mask.active_value as GLint, 0xFF);
            glStencilOp(GL_KEEP, GL_KEEP, GL_KEEP);
        }
    }

    fn mask_deactivate(&mut self) {
        // Maskee finished — but we may be inside a nested mask, so don't
        // disable stencil yet. Switch back to "draw mask" mode to allow
        // erasing/popping.
        unsafe {
            glColorMask(GL_FALSE, GL_FALSE, GL_FALSE, GL_FALSE);
            glStencilMask(0xFF);
            glStencilFunc(GL_ALWAYS, self.mask.active_value as GLint, 0xFF);
            glStencilOp(GL_KEEP, GL_KEEP, GL_REPLACE);
        }
    }

    fn mask_pop(&mut self) {
        // Undo this mask bit.
        if self.mask.depth > 0 {
            self.mask.active_value &= !(1u8 << ((self.mask.depth - 1).min(7)));
            self.mask.depth -= 1;
        }
        unsafe {
            glColorMask(GL_TRUE, GL_TRUE, GL_TRUE, GL_TRUE);
            if self.mask.depth == 0 {
                glDisable(GL_STENCIL_TEST);
            } else {
                glStencilMask(0);
                glStencilFunc(GL_EQUAL, self.mask.active_value as GLint, 0xFF);
                glStencilOp(GL_KEEP, GL_KEEP, GL_KEEP);
            }
        }
    }
}

/// Format a byte count as a short pretty string ("3 KB", "15 MB"). Picks
/// the largest unit that keeps the integer part ≤ 999. KiB-style (1024)
/// instead of decimal because that's what hbmenu / fsadm show for files.
fn format_size_pretty(bytes: u64) -> std::string::String {
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
        self.dimensions = dimensions;
        unsafe {
            glViewport(0, 0, dimensions.width as GLsizei, dimensions.height as GLsizei);
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
        let bitmap_fill_count = mesh
            .draws
            .iter()
            .filter(|d| matches!(d.draw_type, DrawType::Bitmap(_)))
            .count();
        let resolve_bitmaps = bitmap_fill_count <= PER_SHAPE_BITMAP_BUDGET;
        for draw in &mesh.draws {
            let meta = if resolve_bitmaps {
                if let DrawType::Bitmap(b) = &draw.draw_type {
                    bitmap_source
                        .bitmap_handle(b.bitmap_id, self)
                        .and_then(|h| as_switch_bitmap(&h).cloned())
                } else {
                    None
                }
            } else {
                None
            };
            bitmap_metas.push(meta);
        }

        let mut draws: Vec<GpuDraw> = Vec::with_capacity(mesh.draws.len());
        for (idx, draw) in mesh.draws.iter().enumerate() {
            let meta_ref = bitmap_metas[idx].as_ref();
            if let Some(mut gpu) = upload_draw(
                draw,
                &gradient_textures,
                meta_ref,
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
        _handle: BitmapHandle,
        commands: CommandList,
        quality: StageQuality,
        bounds: PixelRegion,
    ) -> Option<Box<dyn SyncHandle>> {
        // Filter rendering deferred (Phase 2.3). Returning None makes Ruffle
        // skip cache+filter for filtered display objects — but we still want
        // to know HOW OFTEN this gets called, because a sudden surge means
        // either a `cacheAsBitmap` clip just appeared on stage or a filtered
        // sprite (e.g. Mario 63 rocket-nozzle glow) is now being rendered.
        self.render_offscreen_calls = self.render_offscreen_calls.wrapping_add(1);
        // Log first 10 calls + every 60th after, so a spike is visible
        // without flooding nxlink.
        let n = self.render_offscreen_calls;
        if n <= 10 || n % 60 == 0 {
            let cmd_len = commands.commands.len();
            let (ram_used, ram_total) = query_ram();
            let msg = std::format!(
                "render_offscreen #{}: bounds={}x{} (origin {},{}) quality={:?} cmds={} ram={}MB/{}MB\n",
                n,
                bounds.width(),
                bounds.height(),
                bounds.x_min,
                bounds.y_min,
                quality,
                cmd_len,
                ram_used / (1024 * 1024),
                ram_total / (1024 * 1024),
            );
            let mut bytes = msg.into_bytes();
            bytes.push(0);
            unsafe { ruffle_log_cstr(bytes.as_ptr() as *const _) };
        }
        None
    }

    fn apply_filter(
        &mut self,
        _source: BitmapHandle,
        source_point: (u32, u32),
        source_size: (u32, u32),
        _destination: BitmapHandle,
        dest_point: (i32, i32),
        filter: Filter,
    ) -> Option<Box<dyn SyncHandle>> {
        // Filter passes are not implemented yet (Phase 2.3). We log every
        // call so when Mario 63 crashes after equipping the rocket nozzle
        // we can see which filter was being requested and how often.
        self.apply_filter_calls = self.apply_filter_calls.wrapping_add(1);
        let (_, name) = filter_variant_ordinal(&filter);
        let n = self.apply_filter_calls;
        if n <= 20 || n % 60 == 0 {
            let msg = std::format!(
                "apply_filter #{}: kind={} src={}x{}@({},{}) dst@({},{})\n",
                n,
                name,
                source_size.0,
                source_size.1,
                source_point.0,
                source_point.1,
                dest_point.0,
                dest_point.1,
            );
            let mut bytes = msg.into_bytes();
            bytes.push(0);
            unsafe { ruffle_log_cstr(bytes.as_ptr() as *const _) };
        }
        None
    }

    fn is_filter_supported(&self, filter: &Filter) -> bool {
        // We log each Filter variant the first time Ruffle queries it, so a
        // single line tells us the entire palette of filters the current
        // .swf actually exercises (vs. what the format theoretically allows).
        let (ord, name) = filter_variant_ordinal(filter);
        let bit = 1u16 << ord;
        let prev = self.filters_seen_mask.fetch_or(bit, Ordering::Relaxed);
        if prev & bit == 0 {
            let msg = std::format!("is_filter_supported: {} (first sighting)\n", name);
            let mut bytes = msg.into_bytes();
            bytes.push(0);
            unsafe { ruffle_log_cstr(bytes.as_ptr() as *const _) };
        }
        false
    }

    fn is_offscreen_supported(&self) -> bool {
        false
    }

    fn submit_frame(
        &mut self,
        clear: Color,
        commands: CommandList,
        _cache_entries: Vec<BitmapCacheEntry>,
    ) {
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
            let (tick_ms, render_ms) = if tick_freq > 0 {
                (
                    (tick_total_ticks * 1000) / tick_freq,
                    (render_total_ticks * 1000) / tick_freq,
                )
            } else {
                (0, 0)
            };
            let draw_calls = self.draw_calls_this_window;
            self.draw_calls_this_window = 0;
            let (ram_used, ram_total) = query_ram();
            let live_s = LIVE_GPU_SHAPES.load(Ordering::Relaxed);
            let live_d = LIVE_GPU_DRAWS.load(Ordering::Relaxed);
            let v_used_mb = self.vertex_arena.in_use_bytes() / (1024 * 1024);
            let v_peak_mb = self.vertex_arena.peak_in_use / (1024 * 1024);
            let i_used_mb = self.index_arena.in_use_bytes() / (1024 * 1024);
            let i_peak_mb = self.index_arena.peak_in_use / (1024 * 1024);
            let v_frag = self.vertex_arena.free.len();
            let i_frag = self.index_arena.free.len();
            let msg = std::format!(
                "f{}: fps={} tick={}ms render={}ms dc/win={} shapes={}(live {}) draws_live={} arena_v={}MB/peak{}MB(frag {}) arena_i={}MB/peak{}MB(frag {}) bitmaps={} atlases={} bitmap_draws={} offscreen={} filter={} ram={}MB/{}MB\n",
                self.frame_count,
                fps_str,
                tick_ms,
                render_ms,
                draw_calls,
                self.shapes_registered,
                live_s,
                live_d,
                v_used_mb, v_peak_mb, v_frag,
                i_used_mb, i_peak_mb, i_frag,
                self.bitmaps_registered,
                self.atlases.len(),
                self.bitmap_draws_emitted,
                self.render_offscreen_calls,
                self.apply_filter_calls,
                ram_used / (1024 * 1024),
                ram_total / (1024 * 1024),
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

        unsafe {
            glViewport(
                0,
                0,
                self.dimensions.width as GLsizei,
                self.dimensions.height as GLsizei,
            );
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

        commands.execute(self);

        unsafe {
            glUseProgram(0);
            glBindVertexArray(0);
            glBindTexture(GL_TEXTURE_2D, 0);
            glDisable(GL_STENCIL_TEST);
            glColorMask(GL_TRUE, GL_TRUE, GL_TRUE, GL_TRUE);
        }
        // Mirror the actual GL state we just wrote so the cache stays
        // truthful for any post-frame work (e.g. the cursor overlay).
        self.gl_state.invalidate();
    }

    fn create_empty_texture(
        &mut self,
        width: NonZeroU32,
        height: NonZeroU32,
    ) -> Result<BitmapHandle, Error> {
        // Atlas-backed empty texture: matches the long-standing behavior
        // that Mario 63's BitmapData usage depends on. The Owned/FBO
        // variant is kept in the codebase for future filter targets but
        // we currently keep is_offscreen_supported = false so Ruffle
        // never asks for one.
        let pixels = vec![0u8; (width.get() * height.get() * 4) as usize];
        let Some(meta) = self.pack_into_atlas(&pixels, width.get(), height.get()) else {
            return Err(Error::TooLarge);
        };
        self.bitmaps_registered = self.bitmaps_registered.wrapping_add(1);
        Ok(BitmapHandle(Arc::new(meta)))
    }

    fn register_bitmap(&mut self, bitmap: Bitmap<'_>) -> Result<BitmapHandle, Error> {
        let Some((bytes, w, h)) = bitmap_to_rgba_bytes(&bitmap) else {
            return Err(Error::UnknownType);
        };
        let Some(meta) = self.pack_into_atlas(&bytes, w, h) else {
            return Err(Error::TooLarge);
        };
        self.bitmaps_registered = self.bitmaps_registered.wrapping_add(1);
        Ok(BitmapHandle(Arc::new(meta)))
    }

    fn update_texture(
        &mut self,
        handle: &BitmapHandle,
        bitmap: Bitmap<'_>,
        region: PixelRegion,
    ) -> Result<(), Error> {
        let Some(switch_bitmap) = as_switch_bitmap(handle) else {
            return Err(Error::UnknownHandle(handle.clone()));
        };
        let rgba = bitmap.to_rgba();
        let w = region.x_max.saturating_sub(region.x_min);
        let h = region.y_max.saturating_sub(region.y_min);
        if w == 0 || h == 0 {
            return Ok(());
        }
        let atlas = match self.atlases.get(switch_bitmap.atlas_index) {
            Some(a) => a,
            None => return Err(Error::UnknownHandle(handle.clone())),
        };
        // Compute the atlas-space pixel offset for the bitmap.
        let base_x = (switch_bitmap.u0 * ATLAS_SIZE as f32).round() as u32;
        let base_y = (switch_bitmap.v0 * ATLAS_SIZE as f32).round() as u32;
        atlas.upload_region(base_x + region.x_min, base_y + region.y_min, w, h, rgba.data());
        Ok(())
    }

    fn create_context3d(
        &mut self,
        _profile: Context3DProfile,
    ) -> Result<Box<dyn Context3D>, Error> {
        Err(Error::Unimplemented("createContext3D".into()))
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
        _shader: PixelBenderShader,
    ) -> Result<PixelBenderShaderHandle, Error> {
        Err(Error::Unimplemented(
            "Pixel bender shader compilation".into(),
        ))
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
        _handle: Box<dyn SyncHandle>,
        _with_rgba: RgbaBufRead,
    ) -> Result<(), Error> {
        Err(Error::Unimplemented("Sync handle resolution".into()))
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
        let world = self.world_matrix(&scaled);
        let mult = transform.color_transform.mult_rgba_normalized();
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

    fn render_stage3d(&mut self, _bitmap: BitmapHandle, _transform: Transform) {
        self.warn_once(b"cmd: render_stage3d (skipped, no Context3D)\n\0");
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
        let world = self.world_matrix(&transform.matrix);
        if world.iter().any(|v| !v.is_finite()) {
            return;
        }
        let mult = transform.color_transform.mult_rgba_normalized();
        let add = transform.color_transform.add_rgba_normalized();
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
                } => {
                    if local_matrix.iter().any(|v| !v.is_finite()) {
                        continue;
                    }
                    let Some(atlas) = self.atlases.get(*atlas_index) else {
                        continue;
                    };
                    let tex = atlas.texture;
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
        _maskee_commands: CommandList,
        _mask_commands: CommandList,
    ) {
        self.warn_once(b"cmd: render_alpha_mask (skipped)\n\0");
    }

    fn draw_rect(&mut self, color: Color, matrix: Matrix) {
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

    fn blend(&mut self, commands: CommandList, _blend_mode: RenderBlendMode) {
        // Real blend modes (multiply, screen, etc.) need offscreen
        // framebuffer compositing — out of scope here. Inline the inner
        // commands so we at least see them rather than dropping them.
        let _ = BlendMode::Normal;
        commands.execute(self);
    }
}

impl Drop for SwitchRenderBackend {
    fn drop(&mut self) {
        unsafe {
            glDeleteBuffers(1, &self.rect_vbo);
            glDeleteVertexArrays(1, &self.rect_vao);
            glDeleteBuffers(1, &self.bitmap_vbo);
            glDeleteVertexArrays(1, &self.bitmap_vao);
            glDeleteBuffers(1, &self.line_vbo);
            glDeleteVertexArrays(1, &self.line_vao);
            glDeleteBuffers(1, &self.line_rect_vbo);
            glDeleteVertexArrays(1, &self.line_rect_vao);
            glDeleteVertexArrays(1, &self.shape_vao);
            // vertex_arena / index_arena released via their Drop impls.
            // Programs freed by their respective Drop impls.
        }
    }
}
