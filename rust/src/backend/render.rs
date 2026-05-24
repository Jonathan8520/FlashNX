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
use ruffle_render::matrix::Matrix;
use ruffle_render::pixel_bender::{PixelBenderShader, PixelBenderShaderHandle};
use ruffle_render::pixel_bender_support::PixelBenderShaderArgument;
use ruffle_render::quality::StageQuality;
use ruffle_render::shape_utils::{DistilledShape, GradientType};
use ruffle_render::tessellator::{DrawType, Gradient, ShapeTessellator};
use ruffle_render::transform::Transform;
use swf::{BlendMode, Color, GradientSpread};

use crate::ffi::gl::*;
use crate::query_ram;

extern "C" {
    fn ruffle_log_cstr(msg: *const core::ffi::c_char);
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
    vao: GLuint,
    vbo: GLuint,
    ibo: GLuint,
    num_indices: GLsizei,
    kind: DrawKind,
}

impl Drop for GpuDraw {
    fn drop(&mut self) {
        unsafe {
            glDeleteBuffers(1, &self.vbo);
            glDeleteBuffers(1, &self.ibo);
            glDeleteVertexArrays(1, &self.vao);
        }
    }
}

struct GpuShape {
    draws: Vec<GpuDraw>,
    gradient_textures: Vec<GLuint>,
}

impl Drop for GpuShape {
    fn drop(&mut self) {
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

#[derive(Default)]
struct MaskState {
    /// Nesting depth: 0 = no mask, N = drawing the Nth maskee.
    depth: u32,
    /// 0 when no mask, otherwise the stencil reference value drawn maskees
    /// must equal.
    active_value: u8,
}

// ─── The backend ──────────────────────────────────────────────────────────────

pub struct SwitchRenderBackend {
    dimensions: ViewportDimensions,
    tessellator: ShapeTessellator,

    solid: SolidProgram,
    bitmap_prog: BitmapProgram,
    shape_bitmap_prog: ShapeBitmapProgram,
    gradient_prog: GradientProgram,

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
    /// Pool of texture atlases. New atlases get appended when current is
    /// full. Bitmaps are packed into these instead of getting individual
    /// GL textures.
    atlases: Vec<Atlas>,
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

    let mut vao: GLuint = 0;
    let mut vbo: GLuint = 0;
    let mut ibo: GLuint = 0;
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
        glGenBuffers(1, &mut ibo);
        glBindBuffer(GL_ELEMENT_ARRAY_BUFFER, ibo);
        glBufferData(
            GL_ELEMENT_ARRAY_BUFFER,
            (draw.indices.len() * core::mem::size_of::<u32>()) as GLsizeiptr,
            draw.indices.as_ptr() as *const _,
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
        glBindBuffer(GL_ELEMENT_ARRAY_BUFFER, 0);
    }

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
                    vao,
                    vbo,
                    ibo,
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
        vao,
        vbo,
        ibo,
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
        let solid = build_solid_program()?;
        let bitmap_prog = build_bitmap_program()?;
        let shape_bitmap_prog = build_shape_bitmap_program()?;
        let gradient_prog = build_gradient_program()?;

        let (rect_vao, rect_vbo) = build_solid_quad();
        let (bitmap_vao, bitmap_vbo) = build_bitmap_quad();
        let (line_vao, line_vbo) = build_line_segment();
        let (line_rect_vao, line_rect_vbo) = build_line_rect();

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
            bitmap_render_count: 0,
            atlases: Vec::new(),
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
        unsafe {
            glUseProgram(self.solid.program);
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
        unsafe {
            glUseProgram(self.bitmap_prog.program);
            glUniformMatrix3fv(self.bitmap_prog.u_world, 1, GL_FALSE, world.as_ptr());
            glUniform4f(self.bitmap_prog.u_mult, mult[0], mult[1], mult[2], mult[3]);
            glUniform4f(self.bitmap_prog.u_add, add[0], add[1], add[2], add[3]);
            glUniform4f(
                self.bitmap_prog.u_uv_remap,
                uv_remap[0], uv_remap[1], uv_remap[2], uv_remap[3],
            );
            glActiveTexture(GL_TEXTURE0);
            glBindTexture(GL_TEXTURE_2D, tex);
            glUniform1i(self.bitmap_prog.u_tex, 0);
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
        unsafe {
            glUseProgram(self.shape_bitmap_prog.program);
            glUniformMatrix3fv(self.shape_bitmap_prog.u_world, 1, GL_FALSE, world.as_ptr());
            glUniform4f(self.shape_bitmap_prog.u_mult, mult[0], mult[1], mult[2], mult[3]);
            glUniform4f(self.shape_bitmap_prog.u_add, add[0], add[1], add[2], add[3]);
            glUniformMatrix3fv(self.shape_bitmap_prog.u_uv, 1, GL_FALSE, uv_matrix.as_ptr());
            glUniform4f(
                self.shape_bitmap_prog.u_uv_remap,
                uv_remap[0], uv_remap[1], uv_remap[2], uv_remap[3],
            );
            // u_wrap_mode: 0 = clamp (default for non-repeating fills),
            // 1 = fract (for tile/repeat fills like Mario 63 ground).
            glUniform1i(
                self.shape_bitmap_prog.u_wrap_mode,
                if is_repeating { 1 } else { 0 },
            );
            glActiveTexture(GL_TEXTURE0);
            glBindTexture(GL_TEXTURE_2D, tex);
            glUniform1i(self.shape_bitmap_prog.u_tex, 0);
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
        unsafe {
            glUseProgram(self.gradient_prog.program);
            glUniformMatrix3fv(self.gradient_prog.u_world, 1, GL_FALSE, world.as_ptr());
            glUniform4f(self.gradient_prog.u_mult, mult[0], mult[1], mult[2], mult[3]);
            glUniform4f(self.gradient_prog.u_add, add[0], add[1], add[2], add[3]);
            glUniformMatrix3fv(self.gradient_prog.u_grad_local, 1, GL_FALSE, local_matrix.as_ptr());
            glUniform1i(self.gradient_prog.u_grad_kind, kind);
            glUniform1i(self.gradient_prog.u_grad_spread, spread);
            glUniform1f(self.gradient_prog.u_grad_focal, focal);
            glActiveTexture(GL_TEXTURE0);
            glBindTexture(GL_TEXTURE_2D, tex);
            glUniform1i(self.gradient_prog.u_tex, 0);
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
        atlas.upload_region(x, y, width, height, pixels);
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
            if let Some(mut gpu) = upload_draw(draw, &gradient_textures, meta_ref) {
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
                draws.push(gpu);
            }
        }

        self.shapes_registered = self.shapes_registered.wrapping_add(1);

        ShapeHandle(Arc::new(SwitchShapeHandle(Arc::new(GpuShape {
            draws,
            gradient_textures,
        }))))
    }

    fn render_offscreen(
        &mut self,
        _handle: BitmapHandle,
        _commands: CommandList,
        _quality: StageQuality,
        _bounds: PixelRegion,
    ) -> Option<Box<dyn SyncHandle>> {
        None
    }

    fn submit_frame(
        &mut self,
        clear: Color,
        commands: CommandList,
        _cache_entries: Vec<BitmapCacheEntry>,
    ) {
        // Drain GL errors once per second, plus a one-line heartbeat with
        // running counters every 2 seconds. Quiet otherwise.
        self.frame_count = self.frame_count.wrapping_add(1);
        if self.frame_count % 120 == 0 {
            let (ram_used, ram_total) = query_ram();
            let msg = std::format!(
                "f{}: shapes={} bitmaps={} atlases={} bitmap_draws={} ram={}MB/{}MB\n",
                self.frame_count,
                self.shapes_registered,
                self.bitmaps_registered,
                self.atlases.len(),
                self.bitmap_draws_emitted,
                ram_used / (1024 * 1024),
                ram_total / (1024 * 1024),
            );
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

        commands.execute(self);

        unsafe {
            glUseProgram(0);
            glBindVertexArray(0);
            glBindTexture(GL_TEXTURE_2D, 0);
            glDisable(GL_STENCIL_TEST);
            glColorMask(GL_TRUE, GL_TRUE, GL_TRUE, GL_TRUE);
        }
    }

    fn create_empty_texture(
        &mut self,
        width: NonZeroU32,
        height: NonZeroU32,
    ) -> Result<BitmapHandle, Error> {
        // Allocate a fully-transparent region in the atlas. Pixel data of
        // size W*H*4, all zeros. Caller will fill via update_texture.
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
        unsafe {
            glBindVertexArray(self.bitmap_vao);
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
            unsafe {
                glBindVertexArray(draw.vao);
                glDrawElements(
                    GL_TRIANGLES,
                    draw.num_indices,
                    GL_UNSIGNED_INT,
                    core::ptr::null(),
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
        unsafe {
            glBindVertexArray(self.rect_vao);
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
        unsafe {
            glLineWidth(1.0);
            glBindVertexArray(self.line_vao);
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
        unsafe {
            glLineWidth(1.0);
            glBindVertexArray(self.line_rect_vao);
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
            // Programs freed by their respective Drop impls.
        }
    }
}
