//! `SwitchRenderBackend` — Ruffle `RenderBackend` impl backed by switch-mesa GL.
//!
//! Phase 1.3.2 (2026-05-23): real shape rendering pipeline.
//!   - `register_shape` runs `ShapeTessellator` (lyon) and uploads each
//!     resulting `Draw` to a VAO/VBO/IBO. Gradient and Bitmap fills are
//!     tessellated but their per-vertex color is used as-is (so a gradient
//!     fill will render as some flat-ish color until 1.3.2.e adds the real
//!     gradient shader).
//!   - `submit_frame` walks the `CommandList`. `RenderShape` redraws cached
//!     GPU meshes with the Flash affine matrix applied. `DrawRect` immediately
//!     draws a transformed unit-square. Other commands are no-ops (with a
//!     one-line log on first miss to surface gaps).
//!   - Single shader for everything: per-vertex (pos.xy, rgba) + uniform 3x3
//!     world matrix that combines (Flash affine) ∘ (pixels → NDC, Y flipped).
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
    Bitmap, BitmapHandle, BitmapHandleImpl, BitmapSource, PixelRegion, PixelSnapping, RgbaBufRead,
    SyncHandle,
};
use ruffle_render::commands::{CommandHandler, CommandList, RenderBlendMode};
use ruffle_render::error::Error;
use ruffle_render::matrix::Matrix;
use ruffle_render::pixel_bender::{PixelBenderShader, PixelBenderShaderHandle};
use ruffle_render::pixel_bender_support::PixelBenderShaderArgument;
use ruffle_render::quality::StageQuality;
use ruffle_render::shape_utils::DistilledShape;
use ruffle_render::tessellator::{DrawType, ShapeTessellator};
use ruffle_render::transform::Transform;
use swf::{BlendMode, Color};

use crate::ffi::gl::*;

extern "C" {
    fn ruffle_log_cstr(msg: *const core::ffi::c_char);
}

fn log(nul_terminated: &[u8]) {
    unsafe { ruffle_log_cstr(nul_terminated.as_ptr() as *const _) };
}

#[derive(Clone, Debug)]
struct SwitchBitmapHandle;
impl BitmapHandleImpl for SwitchBitmapHandle {}

/// Per-draw GPU resources for one tessellated `ruffle_render::tessellator::Draw`.
/// Owned by `GpuShape` and freed when the shape's last `Arc` reference drops.
struct GpuDraw {
    vao: GLuint,
    vbo: GLuint,
    ibo: GLuint,
    num_indices: GLsizei,
    /// `true` when this draw came from a solid-color fill. Gradient/Bitmap
    /// draws still render today (using their tessellator-assigned vertex
    /// colors as a fallback), but we tag them so 1.3.2.e can switch shaders.
    #[allow(dead_code)]
    is_solid_color: bool,
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

/// Cached GPU resources for a `register_shape`'d shape.
/// Wrapped in `Arc` and stored inside `SwitchShapeHandle`, so the GL handles
/// die when Ruffle drops the last `ShapeHandle` clone.
struct GpuShape {
    draws: Vec<GpuDraw>,
}

#[derive(Debug)]
struct SwitchShapeHandle(Arc<GpuShape>);
impl ShapeHandleImpl for SwitchShapeHandle {}

// `GpuShape` only contains GL handles (u32s) so it's structurally Send/Sync,
// but the GL context is single-threaded by design — we never actually move
// these across threads. Manual impls keep `ShapeHandleImpl: Any + Debug + ?`
// downstream code happy without forcing Send/Sync bounds.

impl std::fmt::Debug for GpuShape {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "GpuShape({} draws)", self.draws.len())
    }
}

pub struct SwitchRenderBackend {
    dimensions: ViewportDimensions,
    tessellator: ShapeTessellator,
    /// Solid-color program: vertex shader transforms `a_pos` by `u_world`
    /// and forwards `a_col` to the fragment shader.
    program: GLuint,
    u_world: GLint,
    /// Shared unit-square VAO/VBO used by `draw_rect` (and `draw_line_rect`).
    /// Vertices are (0,0), (1,0), (1,1), (0,1) with white color; per-call
    /// uniform color override happens via the matrix scaling + vertex color
    /// premultiplied at upload time. Today we just bind it and let the
    /// per-vertex white color come through.
    rect_vao: GLuint,
    rect_vbo: GLuint,
    /// Have we logged the "saw an unsupported command" warning yet?
    /// Avoids spamming nxlink on every frame.
    warned_unsupported: u32,
}

const VERT_SRC: &[u8] = b"#version 330 core\n\
layout(location = 0) in vec2 a_pos;\n\
layout(location = 1) in vec4 a_col;\n\
uniform mat3 u_world;\n\
out vec4 v_col;\n\
void main() {\n\
    vec3 p = u_world * vec3(a_pos, 1.0);\n\
    gl_Position = vec4(p.xy, 0.0, 1.0);\n\
    v_col = a_col;\n\
}\n\0";

const FRAG_SRC: &[u8] = b"#version 330 core\n\
in vec4 v_col;\n\
out vec4 frag_color;\n\
void main() {\n\
    frag_color = v_col;\n\
}\n\0";

impl SwitchRenderBackend {
    /// Build the backend. Requires a current GL context (we create shaders
    /// and buffers immediately). Returns `None` if shader compilation fails;
    /// the caller should bail rather than continue with a half-initialised
    /// renderer.
    pub fn new(width: u32, height: u32) -> Option<Self> {
        let program = build_program()?;
        let u_world = unsafe { glGetUniformLocation(program, b"u_world\0".as_ptr() as *const _) };
        let (rect_vao, rect_vbo) = build_rect_quad();
        Some(Self {
            dimensions: ViewportDimensions {
                width,
                height,
                scale_factor: 1.0,
            },
            tessellator: ShapeTessellator::new(),
            program,
            u_world,
            rect_vao,
            rect_vbo,
            warned_unsupported: 0,
        })
    }

    /// Compute the 3x3 column-major matrix that transforms a vertex (in
    /// *pixels*) by the Flash matrix `m` and then projects pixels → NDC.
    /// Result is what we send to the `u_world` mat3 uniform.
    fn world_matrix(&self, m: &Matrix) -> [GLfloat; 9] {
        let w = self.dimensions.width.max(1) as f32;
        let h = self.dimensions.height.max(1) as f32;

        // Flash 2x3 affine in column-major form, with tx/ty already in pixels:
        //   | a  c  tx |
        //   | b  d  ty |
        //   | 0  0  1  |
        let a = m.a;
        let b = m.b;
        let c = m.c;
        let d = m.d;
        let tx = m.tx.to_pixels() as f32;
        let ty = m.ty.to_pixels() as f32;

        // Pixels → NDC: x' = 2x/w - 1, y' = 1 - 2y/h (Y flipped because GL
        // is bottom-up).
        //   | 2/w  0    -1 |
        //   | 0    -2/h  1 |
        //   | 0    0     1 |
        //
        // Compose: NDC * Flash. Output is column-major as required by GLSL
        // mat3 (stored column-by-column).
        let sx = 2.0 / w;
        let sy = -2.0 / h;
        let col0 = [a * sx, b * sy, 0.0];
        let col1 = [c * sx, d * sy, 0.0];
        let col2 = [tx * sx - 1.0, ty * sy + 1.0, 1.0];
        [
            col0[0], col0[1], col0[2],
            col1[0], col1[1], col1[2],
            col2[0], col2[1], col2[2],
        ]
    }

    fn warn_once(&mut self, msg: &[u8]) {
        // Only warn on the first ~8 distinct unsupported commands to keep
        // nxlink output sane. Real "what commands appeared" tracking belongs
        // in a later iteration.
        if self.warned_unsupported < 8 {
            self.warned_unsupported += 1;
            log(msg);
        }
    }
}

fn build_program() -> Option<GLuint> {
    let vs = compile_shader(GL_VERTEX_SHADER, VERT_SRC)?;
    let fs = compile_shader(GL_FRAGMENT_SHADER, FRAG_SRC)?;
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

fn build_rect_quad() -> (GLuint, GLuint) {
    // Unit-square quad as two triangles, white per-vertex color.
    // Format: x, y, r, g, b, a (6 floats per vertex).
    #[rustfmt::skip]
    const QUAD: [f32; 36] = [
        0.0, 0.0,  1.0, 1.0, 1.0, 1.0,
        1.0, 0.0,  1.0, 1.0, 1.0, 1.0,
        1.0, 1.0,  1.0, 1.0, 1.0, 1.0,
        0.0, 0.0,  1.0, 1.0, 1.0, 1.0,
        1.0, 1.0,  1.0, 1.0, 1.0, 1.0,
        0.0, 1.0,  1.0, 1.0, 1.0, 1.0,
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

/// Upload one tessellated `Draw` to a freshly-created VAO/VBO/IBO triplet.
fn upload_draw(draw: &ruffle_render::tessellator::Draw) -> Option<GpuDraw> {
    // Interleaved: x, y, r, g, b, a (6 f32 per vertex).
    let mut verts: Vec<f32> = Vec::with_capacity(draw.vertices.len() * 6);
    for v in &draw.vertices {
        verts.push(v.x);
        verts.push(v.y);
        verts.push(v.color.r as f32 / 255.0);
        verts.push(v.color.g as f32 / 255.0);
        verts.push(v.color.b as f32 / 255.0);
        verts.push(v.color.a as f32 / 255.0);
    }
    if verts.is_empty() || draw.indices.is_empty() {
        return None;
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
    Some(GpuDraw {
        vao,
        vbo,
        ibo,
        num_indices: draw.indices.len() as GLsizei,
        is_solid_color: matches!(draw.draw_type, DrawType::Color),
    })
}

fn as_switch_shape(handle: &ShapeHandle) -> Option<&SwitchShapeHandle> {
    <dyn Any>::downcast_ref(&*handle.0)
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
        let mut draws = Vec::with_capacity(mesh.draws.len());
        for draw in &mesh.draws {
            if let Some(gpu) = upload_draw(draw) {
                draws.push(gpu);
            }
        }
        ShapeHandle(Arc::new(SwitchShapeHandle(Arc::new(GpuShape { draws }))))
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
            glClear(GL_COLOR_BUFFER_BIT);

            glEnable(GL_BLEND);
            glBlendFunc(GL_SRC_ALPHA, GL_ONE_MINUS_SRC_ALPHA);
            glUseProgram(self.program);
        }

        commands.execute(self);

        unsafe {
            glUseProgram(0);
            glBindVertexArray(0);
        }
    }

    fn create_empty_texture(
        &mut self,
        _width: NonZeroU32,
        _height: NonZeroU32,
    ) -> Result<BitmapHandle, Error> {
        Ok(BitmapHandle(Arc::new(SwitchBitmapHandle)))
    }

    fn register_bitmap(&mut self, _bitmap: Bitmap<'_>) -> Result<BitmapHandle, Error> {
        Ok(BitmapHandle(Arc::new(SwitchBitmapHandle)))
    }

    fn update_texture(
        &mut self,
        _handle: &BitmapHandle,
        _bitmap: Bitmap<'_>,
        _region: PixelRegion,
    ) -> Result<(), Error> {
        Ok(())
    }

    fn create_context3d(
        &mut self,
        _profile: Context3DProfile,
    ) -> Result<Box<dyn Context3D>, Error> {
        Err(Error::Unimplemented("createContext3D".into()))
    }

    fn debug_info(&self) -> Cow<'static, str> {
        Cow::Borrowed("Renderer: SwitchRenderBackend (phase 1.3.2 — shapes + rects)")
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

impl CommandHandler for SwitchRenderBackend {
    fn render_bitmap(
        &mut self,
        _bitmap: BitmapHandle,
        _transform: Transform,
        _smoothing: bool,
        _pixel_snapping: PixelSnapping,
    ) {
        self.warn_once(b"cmd: render_bitmap (skipped, 1.3.2.e)\n\0");
    }

    fn render_stage3d(&mut self, _bitmap: BitmapHandle, _transform: Transform) {
        self.warn_once(b"cmd: render_stage3d (skipped, no Context3D)\n\0");
    }

    fn render_shape(&mut self, shape: ShapeHandle, transform: Transform) {
        let Some(switch_shape) = as_switch_shape(&shape) else {
            self.warn_once(b"cmd: render_shape with non-Switch ShapeHandle\n\0");
            return;
        };
        let world = self.world_matrix(&transform.matrix);
        unsafe {
            glUniformMatrix3fv(self.u_world, 1, GL_FALSE, world.as_ptr());
            for draw in &switch_shape.0.draws {
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
        // The matrix scales/positions the unit square into the rect. We
        // multiply the per-vertex white color of the shared quad VBO by
        // the requested fill color via a per-rect color tint passed
        // through the same vertex stream. Simplest path: upload a 6-vertex
        // VBO per call. Cheap enough at the typical call rates (<10/frame).
        let r = color.r as f32 / 255.0;
        let g = color.g as f32 / 255.0;
        let b = color.b as f32 / 255.0;
        let a = color.a as f32 / 255.0;
        #[rustfmt::skip]
        let quad: [f32; 36] = [
            0.0, 0.0,  r, g, b, a,
            1.0, 0.0,  r, g, b, a,
            1.0, 1.0,  r, g, b, a,
            0.0, 0.0,  r, g, b, a,
            1.0, 1.0,  r, g, b, a,
            0.0, 1.0,  r, g, b, a,
        ];
        unsafe {
            glBindVertexArray(self.rect_vao);
            glBindBuffer(GL_ARRAY_BUFFER, self.rect_vbo);
            glBufferData(
                GL_ARRAY_BUFFER,
                core::mem::size_of_val(&quad) as GLsizeiptr,
                quad.as_ptr() as *const _,
                GL_DYNAMIC_DRAW,
            );
            let world = self.world_matrix(&matrix);
            glUniformMatrix3fv(self.u_world, 1, GL_FALSE, world.as_ptr());
            glDrawArrays(GL_TRIANGLES, 0, 6);
        }
    }

    fn draw_line(&mut self, _color: Color, _matrix: Matrix) {
        self.warn_once(b"cmd: draw_line (skipped, no GL_LINES path yet)\n\0");
    }

    fn draw_line_rect(&mut self, _color: Color, _matrix: Matrix) {
        self.warn_once(b"cmd: draw_line_rect (skipped)\n\0");
    }

    fn push_mask(&mut self) {
        self.warn_once(b"cmd: push_mask (skipped, no stencil yet)\n\0");
    }
    fn activate_mask(&mut self) {}
    fn deactivate_mask(&mut self) {}
    fn pop_mask(&mut self) {}

    fn blend(&mut self, commands: CommandList, _blend_mode: RenderBlendMode) {
        // Without a Blend mode change today we just inline the inner commands
        // — better than dropping them on the floor.
        let _ = BlendMode::Normal;
        commands.execute(self);
    }
}

impl Drop for SwitchRenderBackend {
    fn drop(&mut self) {
        unsafe {
            glDeleteBuffers(1, &self.rect_vbo);
            glDeleteVertexArrays(1, &self.rect_vao);
            glDeleteProgram(self.program);
        }
    }
}
