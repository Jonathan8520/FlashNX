//! Stage3D on the Switch GL backend (issue #88).
//!
//! Ruffle implements Context3D on its wgpu backend only; webgl, canvas and (so
//! far) ours all answered `Unimplemented`, so a Stage3D game got no context, no
//! picture, and — before the local patch in `stage3d_object.rs` — not even an
//! error it could react to. Angry Birds Cheetos sat on its loading screen for
//! exactly that reason.
//!
//! What is implemented here is the subset a Starling-based game uses, which is
//! most of what Flash games do with Stage3D: a back buffer, vertex and index
//! buffers, textures, AGAL programs, blending, scissor and `drawTriangles`.
//! Cube maps, mip levels, anti-aliasing and multi-surface render targets are
//! not: they are logged once and ignored rather than faked, so a game that
//! needs them shows something wrong instead of pretending.
//!
//! The shader half costs us nothing: `naga-agal` already translates AGAL into
//! naga IR (it is backend-agnostic, wgpu just happened to be its only consumer),
//! and naga emits GLSL ES from there. The binding layout it produces is fixed —
//! vertex constants at binding 0, fragment constants at 1, samplers from 2,
//! textures from 10, all in group 0 — which is what `binding_map` below mirrors
//! onto uniform-block bindings and texture units.

use std::any::Any;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use ruffle_render::backend::{
    BufferUsage, Context3D, Context3DBlendFactor, Context3DCommand, Context3DCompareMode,
    Context3DProfile, Context3DTextureFormat, Context3DTriangleFace, Context3DVertexBufferFormat,
    IndexBuffer, ProgramType, ShaderModule, Texture, VertexBuffer,
};
use ruffle_render::bitmap::{BitmapHandle, BitmapHandleImpl};
use ruffle_render::error::Error;

use naga_agal::{ParsedBytecode, VertexAttributeFormat};

use crate::ffi::gl::*;

const MAX_ATTRS: usize = 8;
const MAX_TEXTURES: usize = 8;
/// AGAL gives a vertex program 128 constant registers and a fragment program 28,
/// each a vec4. The uniform buffers are sized once, at that maximum.
const VERTEX_CONSTANTS: usize = 128;
const FRAGMENT_CONSTANTS: usize = 28;

fn log(msg: &str) {
    let mut bytes = msg.as_bytes().to_vec();
    bytes.push(0);
    unsafe { ruffle_log_cstr(bytes.as_ptr() as *const _) };
}

extern "C" {
    fn ruffle_log_cstr(msg: *const core::ffi::c_char);
}

// ── Resources ────────────────────────────────────────────────────────────────

struct Gl3dVertexBuffer {
    buffer: GLuint,
    data32_per_vertex: u8,
}
impl VertexBuffer for Gl3dVertexBuffer {}
impl Drop for Gl3dVertexBuffer {
    fn drop(&mut self) {
        unsafe { glDeleteBuffers(1, &self.buffer) };
    }
}

struct Gl3dIndexBuffer {
    buffer: GLuint,
}
impl IndexBuffer for Gl3dIndexBuffer {}
impl Drop for Gl3dIndexBuffer {
    fn drop(&mut self) {
        unsafe { glDeleteBuffers(1, &self.buffer) };
    }
}

/// A disposed buffer keeps no GL object: Ruffle swaps these in on `dispose()`
/// and may still hand them to us afterwards, so they have to be harmless.
struct DisposedBuffer;
impl VertexBuffer for DisposedBuffer {}
impl IndexBuffer for DisposedBuffer {}

pub(crate) struct Gl3dTexture {
    texture: GLuint,
    width: u32,
    height: u32,
}
impl std::fmt::Debug for Gl3dTexture {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Gl3dTexture({}, {}x{})", self.texture, self.width, self.height)
    }
}
impl Texture for Gl3dTexture {
    fn width(&self) -> u32 {
        self.width
    }
    fn height(&self) -> u32 {
        self.height
    }
}
impl Drop for Gl3dTexture {
    fn drop(&mut self) {
        unsafe { glDeleteTextures(1, &self.texture) };
    }
}

/// The AGAL pair, kept unlinked: a program can only be built once the vertex
/// attribute FORMATS are known, and those arrive with `SetVertexBufferAt`,
/// after the shaders. Same reason the wgpu backend defers it.
struct Gl3dShaders {
    vertex: ParsedBytecode,
    fragment: ParsedBytecode,
    /// One linked program per attribute layout seen so far.
    programs: RefCell<Vec<([Option<VertexAttributeFormat>; MAX_ATTRS], Rc<Gl3dProgram>)>>,
}
impl ShaderModule for Gl3dShaders {}

struct Gl3dProgram {
    program: GLuint,
    /// Sampler uniform location per texture unit, when the shader uses it.
    samplers: [GLint; MAX_TEXTURES],
}
impl Drop for Gl3dProgram {
    fn drop(&mut self) {
        unsafe { glDeleteProgram(self.program) };
    }
}

// ── The context ──────────────────────────────────────────────────────────────

pub(crate) struct SwitchContext3D {
    profile: Context3DProfile,
    /// Back buffer: what the movie draws into and what the 2D stage then shows.
    back_buffer: Option<BackBuffer>,
    /// Set once the movie has drawn something worth showing.
    should_render: bool,
    fbo: GLuint,
    vao: GLuint,
    /// Uniform buffers for the vertex / fragment program constants.
    ubo: [GLuint; 2],
    constants: [Vec<f32>; 2],
    constants_dirty: [bool; 2],
    shaders: Option<Rc<Gl3dShaders>>,
    attrs: [Option<(Rc<dyn VertexBuffer>, Context3DVertexBufferFormat, u32)>; MAX_ATTRS],
    textures: [Option<Rc<dyn Texture>>; MAX_TEXTURES],
    /// Currently rendering into this texture instead of the back buffer.
    render_target: Option<Rc<dyn Texture>>,
    warned: u32,
    /// Per-second counters. Whether the 3D path draws at all is invisible from
    /// the outside — a game can look half-empty because our subset dropped its
    /// geometry, or because that part of it was never 3D to begin with, and the
    /// two need opposite fixes.
    stats: Stage3DStats,
}

#[derive(Default)]
struct Stage3DStats {
    frames: u32,
    draws: u32,
    triangles: u64,
    programs: u32,
    textures: u32,
    to_texture: u32,
    dropped_no_program: u32,
}

struct BackBuffer {
    /// The GL name, kept for framebuffer attachment. Ownership belongs to the
    /// handle below, which is what frees it.
    texture: GLuint,
    /// A plain standalone bitmap, so every existing 2D draw path can show the
    /// 3D picture with no changes at all.
    handle: BitmapHandle,
    width: u32,
    height: u32,
}

impl SwitchContext3D {
    pub(crate) fn new(profile: Context3DProfile) -> Self {
        let mut fbo: GLuint = 0;
        let mut vao: GLuint = 0;
        let mut ubo: [GLuint; 2] = [0, 0];
        unsafe {
            glGenFramebuffers(1, &mut fbo);
            glGenVertexArrays(1, &mut vao);
            glGenBuffers(2, ubo.as_mut_ptr());
            for (i, size) in [VERTEX_CONSTANTS, FRAGMENT_CONSTANTS].iter().enumerate() {
                glBindBuffer(GL_UNIFORM_BUFFER, ubo[i]);
                glBufferData(
                    GL_UNIFORM_BUFFER,
                    (size * 4 * 4) as GLsizeiptr,
                    core::ptr::null(),
                    GL_DYNAMIC_DRAW,
                );
            }
            glBindBuffer(GL_UNIFORM_BUFFER, 0);
        }
        Self {
            profile,
            back_buffer: None,
            should_render: false,
            fbo,
            vao,
            ubo,
            constants: [
                std::vec![0.0; VERTEX_CONSTANTS * 4],
                std::vec![0.0; FRAGMENT_CONSTANTS * 4],
            ],
            constants_dirty: [true, true],
            shaders: None,
            attrs: Default::default(),
            textures: Default::default(),
            render_target: None,
            warned: 0,
            stats: Stage3DStats::default(),
        }
    }

    fn warn(&mut self, msg: &str) {
        if self.warned < 12 {
            self.warned += 1;
            log(msg);
        }
    }

    /// Bind the current target (back buffer, or a texture) and set the viewport.
    fn bind_target(&mut self) {
        let (tex, w, h) = match (&self.render_target, &self.back_buffer) {
            (Some(t), _) => {
                let Some(gt) = (t.as_ref() as &dyn Any).downcast_ref::<Gl3dTexture>() else {
                    return;
                };
                (gt.texture, gt.width, gt.height)
            }
            (None, Some(bb)) => (bb.texture, bb.width, bb.height),
            (None, None) => return,
        };
        unsafe {
            glBindFramebuffer(GL_FRAMEBUFFER, self.fbo);
            glFramebufferTexture2D(GL_FRAMEBUFFER, GL_COLOR_ATTACHMENT0, GL_TEXTURE_2D, tex, 0);
            glViewport(0, 0, w as GLsizei, h as GLsizei);
        }
    }

    /// Link the AGAL pair for the attribute layout in effect, reusing the
    /// program if this layout has been seen before.
    fn program_for_current_attrs(&mut self) -> Option<Rc<Gl3dProgram>> {
        let shaders = self.shaders.clone()?;
        let mut formats: [Option<VertexAttributeFormat>; MAX_ATTRS] = Default::default();
        for (i, slot) in self.attrs.iter().enumerate() {
            formats[i] = slot.as_ref().map(|(_, fmt, _)| match fmt {
                Context3DVertexBufferFormat::Float1 => VertexAttributeFormat::Float1,
                Context3DVertexBufferFormat::Float2 => VertexAttributeFormat::Float2,
                Context3DVertexBufferFormat::Float3 => VertexAttributeFormat::Float3,
                Context3DVertexBufferFormat::Float4 => VertexAttributeFormat::Float4,
                Context3DVertexBufferFormat::Bytes4 => VertexAttributeFormat::Bytes4,
            });
        }
        if let Some((_, prog)) = shaders
            .programs
            .borrow()
            .iter()
            .find(|(f, _)| formats_eq(f, &formats))
        {
            return Some(prog.clone());
        }
        let prog = Rc::new(build_program(&shaders, &formats)?);
        self.stats.programs += 1;
        shaders.programs.borrow_mut().push((formats, prog.clone()));
        Some(prog)
    }

    fn upload_constants(&mut self) {
        for i in 0..2 {
            if !self.constants_dirty[i] {
                continue;
            }
            self.constants_dirty[i] = false;
            unsafe {
                glBindBuffer(GL_UNIFORM_BUFFER, self.ubo[i]);
                glBufferSubData(
                    GL_UNIFORM_BUFFER,
                    0,
                    (self.constants[i].len() * 4) as GLsizeiptr,
                    self.constants[i].as_ptr() as *const _,
                );
                glBindBuffer(GL_UNIFORM_BUFFER, 0);
            }
        }
        unsafe {
            glBindBufferBase(GL_UNIFORM_BUFFER, 0, self.ubo[0]);
            glBindBufferBase(GL_UNIFORM_BUFFER, 1, self.ubo[1]);
        }
    }

    /// Point the vertex attributes at their buffers, in the layout the program
    /// was linked for.
    fn bind_attributes(&self) {
        unsafe { glBindVertexArray(self.vao) };
        for (i, slot) in self.attrs.iter().enumerate() {
            let loc = i as GLuint;
            match slot {
                Some((buffer, format, offset)) => {
                    let Some(vb) = (buffer.as_ref() as &dyn Any).downcast_ref::<Gl3dVertexBuffer>()
                    else {
                        unsafe { glDisableVertexAttribArray(loc) };
                        continue;
                    };
                    let (size, ty, normalised) = match format {
                        Context3DVertexBufferFormat::Float1 => (1, GL_FLOAT, 0),
                        Context3DVertexBufferFormat::Float2 => (2, GL_FLOAT, 0),
                        Context3DVertexBufferFormat::Float3 => (3, GL_FLOAT, 0),
                        Context3DVertexBufferFormat::Float4 => (4, GL_FLOAT, 0),
                        // AGAL's bytes4 is four unsigned bytes normalised to
                        // 0..1 — Starling packs vertex colours this way.
                        Context3DVertexBufferFormat::Bytes4 => (4, GL_UNSIGNED_BYTE, 1),
                    };
                    let stride = vb.data32_per_vertex as GLsizei * 4;
                    unsafe {
                        glBindBuffer(GL_ARRAY_BUFFER, vb.buffer);
                        glEnableVertexAttribArray(loc);
                        glVertexAttribPointer(
                            loc,
                            size,
                            ty,
                            normalised,
                            stride,
                            (*offset as usize * 4) as *const _,
                        );
                    }
                }
                None => unsafe { glDisableVertexAttribArray(loc) },
            }
        }
    }

    fn bind_textures(&self, program: &Gl3dProgram) {
        for (unit, slot) in self.textures.iter().enumerate() {
            let loc = program.samplers[unit];
            if loc < 0 {
                continue;
            }
            let tex = slot
                .as_ref()
                .and_then(|t| (t.as_ref() as &dyn Any).downcast_ref::<Gl3dTexture>())
                .map(|t| t.texture)
                .unwrap_or(0);
            unsafe {
                glActiveTexture(GL_TEXTURE0 + unit as GLenum);
                glBindTexture(GL_TEXTURE_2D, tex);
                glUniform1i(loc, unit as GLint);
            }
        }
        unsafe { glActiveTexture(GL_TEXTURE0) };
    }

    /// Put GL back the way the 2D renderer expects to find it.
    fn restore_2d_state(&self) {
        unsafe {
            glBindVertexArray(0);
            glBindBuffer(GL_ARRAY_BUFFER, 0);
            glBindBuffer(GL_ELEMENT_ARRAY_BUFFER, 0);
            glBindFramebuffer(GL_FRAMEBUFFER, 0);
            glDisable(GL_DEPTH_TEST);
            glDisable(GL_CULL_FACE);
            glDisable(GL_SCISSOR_TEST);
            glUseProgram(0);
        }
    }
}

fn formats_eq(
    a: &[Option<VertexAttributeFormat>; MAX_ATTRS],
    b: &[Option<VertexAttributeFormat>; MAX_ATTRS],
) -> bool {
    a.iter().zip(b.iter()).all(|(x, y)| match (x, y) {
        (None, None) => true,
        (Some(x), Some(y)) => core::mem::discriminant(x) == core::mem::discriminant(y),
        _ => false,
    })
}

/// AGAL -> naga -> GLSL ES -> a linked GL program.
fn build_program(
    shaders: &Gl3dShaders,
    formats: &[Option<VertexAttributeFormat>; MAX_ATTRS],
) -> Option<Gl3dProgram> {
    let (vertex_src, _, v_blocks) =
        agal_to_glsl(&shaders.vertex, formats, naga::ShaderStage::Vertex)?;
    let (fragment_src, f_samplers, f_blocks) =
        agal_to_glsl(&shaders.fragment, formats, naga::ShaderStage::Fragment)?;
    let program = link_program(&vertex_src, &fragment_src)?;

    // Names come from naga's reflection, never from a guess: it renames globals
    // freely, and a sampler we fail to find is a texture that never gets bound —
    // which draws the geometry with nothing on it and reads, on screen, as a
    // missing element rather than as an error.
    let mut samplers = [-1; MAX_TEXTURES];
    unsafe {
        for (name, binding) in v_blocks.iter().chain(f_blocks.iter()) {
            let c = std::format!("{name}\0");
            let idx = glGetUniformBlockIndex(program, c.as_ptr() as *const _);
            if idx != GL_INVALID_INDEX {
                glUniformBlockBinding(program, idx, *binding);
            }
        }
        glUseProgram(program);
        for (name, unit) in f_samplers.iter() {
            if *unit as usize >= MAX_TEXTURES {
                continue;
            }
            let c = std::format!("{name}\0");
            samplers[*unit as usize] = glGetUniformLocation(program, c.as_ptr() as *const _);
        }
        glUseProgram(0);
    }
    Some(Gl3dProgram { program, samplers })
}

/// Returns the GLSL source, the sampler uniforms as (name, texture unit), and
/// the uniform blocks as (name, binding). Both lists come from naga's own
/// reflection, so they survive any renaming it does.
type GlslWithBindings = (
    std::string::String,
    std::vec::Vec<(std::string::String, u32)>,
    std::vec::Vec<(std::string::String, u32)>,
);

fn agal_to_glsl(
    parsed: &ParsedBytecode,
    formats: &[Option<VertexAttributeFormat>; MAX_ATTRS],
    stage: naga::ShaderStage,
) -> Option<GlslWithBindings> {
    let module = match naga_agal::agal_to_naga(parsed, formats) {
        Ok(m) => m,
        Err(e) => {
            log(&std::format!("context3d: AGAL translation failed: {e:?}\n"));
            return None;
        }
    };
    let mut validator = naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::empty(),
    );
    let info = match validator.validate(&module) {
        Ok(i) => i,
        Err(e) => {
            log(&std::format!("context3d: naga validation failed: {e:?}\n"));
            return None;
        }
    };
    // Bindings mirror what `naga-agal` emits: constants at 0 and 1, textures
    // from 10 mapped onto texture units 0..7.
    let mut binding_map = naga::back::glsl::BindingMap::default();
    binding_map.insert(naga::ResourceBinding { group: 0, binding: 0 }, 0);
    binding_map.insert(naga::ResourceBinding { group: 0, binding: 1 }, 1);
    for i in 0..MAX_TEXTURES as u32 {
        binding_map.insert(
            naga::ResourceBinding {
                group: 0,
                binding: naga_agal::TEXTURE_START_BIND_INDEX + i,
            },
            i as u8,
        );
        binding_map.insert(
            naga::ResourceBinding {
                group: 0,
                binding: naga_agal::TEXTURE_SAMPLER_START_BIND_INDEX + i,
            },
            i as u8,
        );
    }
    let options = naga::back::glsl::Options {
        version: naga::back::glsl::Version::Embedded {
            version: 310,
            is_webgl: false,
        },
        writer_flags: naga::back::glsl::WriterFlags::empty(),
        binding_map,
        zero_initialize_workgroup_memory: false,
    };
    let pipeline_options = naga::back::glsl::PipelineOptions {
        shader_stage: stage,
        entry_point: "main".to_string(),
        multiview: None,
    };
    let mut out = std::string::String::new();
    let mut writer = match naga::back::glsl::Writer::new(
        &mut out,
        &module,
        &info,
        &options,
        &pipeline_options,
        naga::proc::BoundsCheckPolicies::default(),
    ) {
        Ok(w) => w,
        Err(e) => {
            log(&std::format!("context3d: GLSL writer refused the module: {e:?}\n"));
            return None;
        }
    };
    let reflection = match writer.write() {
        Ok(r) => r,
        Err(e) => {
            log(&std::format!("context3d: GLSL emission failed: {e:?}\n"));
            return None;
        }
    };
    // Map each reflected global back to the binding `naga-agal` gave it, which
    // is what says which texture unit or block it is.
    let binding_of = |handle: naga::Handle<naga::GlobalVariable>| -> Option<u32> {
        module.global_variables[handle]
            .binding
            .as_ref()
            .map(|b| b.binding)
    };
    let mut samplers = std::vec::Vec::new();
    for (name, mapping) in reflection.texture_mapping.iter() {
        if let Some(binding) = binding_of(mapping.texture) {
            if binding >= naga_agal::TEXTURE_START_BIND_INDEX {
                samplers.push((name.clone(), binding - naga_agal::TEXTURE_START_BIND_INDEX));
            }
        }
    }
    let mut blocks = std::vec::Vec::new();
    for (handle, name) in reflection.uniforms.iter() {
        if let Some(binding) = binding_of(*handle) {
            if binding <= 1 {
                blocks.push((name.clone(), binding));
            }
        }
    }
    Some((out, samplers, blocks))
}

fn link_program(vertex_src: &str, fragment_src: &str) -> Option<GLuint> {
    let vs = compile_shader(GL_VERTEX_SHADER, vertex_src)?;
    let fs = compile_shader(GL_FRAGMENT_SHADER, fragment_src)?;
    unsafe {
        let program = glCreateProgram();
        glAttachShader(program, vs);
        glAttachShader(program, fs);
        glLinkProgram(program);
        glDeleteShader(vs);
        glDeleteShader(fs);
        let mut ok: GLint = 0;
        glGetProgramiv(program, GL_LINK_STATUS, &mut ok);
        if ok == 0 {
            let mut buf = [0u8; 512];
            let mut len: GLsizei = 0;
            glGetProgramInfoLog(program, 511, &mut len, buf.as_mut_ptr() as *mut _);
            log(&std::format!(
                "context3d: program link failed: {}\n",
                std::string::String::from_utf8_lossy(&buf[..len.max(0) as usize]),
            ));
            glDeleteProgram(program);
            return None;
        }
        Some(program)
    }
}

fn compile_shader(kind: GLenum, src: &str) -> Option<GLuint> {
    unsafe {
        let shader = glCreateShader(kind);
        let ptr = src.as_ptr() as *const GLchar;
        let len = src.len() as GLint;
        glShaderSource(shader, 1, &ptr, &len);
        glCompileShader(shader);
        let mut ok: GLint = 0;
        glGetShaderiv(shader, GL_COMPILE_STATUS, &mut ok);
        if ok == 0 {
            let mut buf = [0u8; 512];
            let mut out_len: GLsizei = 0;
            glGetShaderInfoLog(shader, 511, &mut out_len, buf.as_mut_ptr() as *mut _);
            log(&std::format!(
                "context3d: shader compile failed: {}\n",
                std::string::String::from_utf8_lossy(&buf[..out_len.max(0) as usize]),
            ));
            glDeleteShader(shader);
            return None;
        }
        Some(shader)
    }
}

fn blend_factor(factor: Context3DBlendFactor) -> GLenum {
    match factor {
        Context3DBlendFactor::Zero => GL_ZERO,
        Context3DBlendFactor::One => GL_ONE,
        Context3DBlendFactor::SourceColor => GL_SRC_COLOR,
        Context3DBlendFactor::OneMinusSourceColor => GL_ONE_MINUS_SRC_COLOR,
        Context3DBlendFactor::SourceAlpha => GL_SRC_ALPHA,
        Context3DBlendFactor::OneMinusSourceAlpha => GL_ONE_MINUS_SRC_ALPHA,
        Context3DBlendFactor::DestinationColor => GL_DST_COLOR,
        Context3DBlendFactor::OneMinusDestinationColor => GL_ONE_MINUS_DST_COLOR,
        Context3DBlendFactor::DestinationAlpha => GL_DST_ALPHA,
        Context3DBlendFactor::OneMinusDestinationAlpha => GL_ONE_MINUS_DST_ALPHA,
    }
}

impl Context3D for SwitchContext3D {
    fn profile(&self) -> Context3DProfile {
        self.profile
    }

    fn bitmap_handle(&self) -> BitmapHandle {
        // Ruffle asks for this before `configureBackBuffer` in some orders, so
        // there has to be an answer: a 1x1 texture that draws nothing, rather
        // than a handle with a zero GL name (Mesa dereferences that and dies).
        self.back_buffer.as_ref().map(|bb| bb.handle.clone()).unwrap_or_else(|| {
            let mut tex: GLuint = 0;
            unsafe {
                glGenTextures(1, &mut tex);
                glBindTexture(GL_TEXTURE_2D, tex);
                glTexImage2D(
                    GL_TEXTURE_2D, 0, GL_RGBA8 as GLint, 1, 1, 0,
                    GL_RGBA, GL_UNSIGNED_BYTE, core::ptr::null(),
                );
                glBindTexture(GL_TEXTURE_2D, 0);
            }
            crate::backend::render::standalone_bitmap_from_texture(tex, 1, 1)
        })
    }

    fn should_render(&self) -> bool {
        self.should_render && self.back_buffer.is_some()
    }

    fn disposed_index_buffer_handle(&self) -> Rc<dyn IndexBuffer> {
        Rc::new(DisposedBuffer)
    }

    fn disposed_vertex_buffer_handle(&self) -> Rc<dyn VertexBuffer> {
        Rc::new(DisposedBuffer)
    }

    fn create_index_buffer(&mut self, _usage: BufferUsage, num_indices: u32) -> Box<dyn IndexBuffer> {
        let mut buffer: GLuint = 0;
        unsafe {
            glGenBuffers(1, &mut buffer);
            glBindBuffer(GL_ELEMENT_ARRAY_BUFFER, buffer);
            glBufferData(
                GL_ELEMENT_ARRAY_BUFFER,
                (num_indices as usize * 2) as GLsizeiptr,
                core::ptr::null(),
                GL_DYNAMIC_DRAW,
            );
            glBindBuffer(GL_ELEMENT_ARRAY_BUFFER, 0);
        }
        Box::new(Gl3dIndexBuffer { buffer })
    }

    fn create_vertex_buffer(
        &mut self,
        _usage: BufferUsage,
        num_vertices: u32,
        data_32_per_vertex: u8,
    ) -> Rc<dyn VertexBuffer> {
        let mut buffer: GLuint = 0;
        unsafe {
            glGenBuffers(1, &mut buffer);
            glBindBuffer(GL_ARRAY_BUFFER, buffer);
            glBufferData(
                GL_ARRAY_BUFFER,
                (num_vertices as usize * data_32_per_vertex as usize * 4) as GLsizeiptr,
                core::ptr::null(),
                GL_DYNAMIC_DRAW,
            );
            glBindBuffer(GL_ARRAY_BUFFER, 0);
        }
        Rc::new(Gl3dVertexBuffer {
            buffer,
            data32_per_vertex: data_32_per_vertex,
        })
    }

    fn create_texture(
        &mut self,
        width: u32,
        height: u32,
        _format: Context3DTextureFormat,
        _optimize_for_render_to_texture: bool,
        _streaming_levels: u32,
    ) -> Result<Rc<dyn Texture>, Error> {
        let mut texture: GLuint = 0;
        unsafe {
            glGenTextures(1, &mut texture);
            if texture == 0 {
                return Err(Error::Unimplemented("Stage3D texture allocation".into()));
            }
            glBindTexture(GL_TEXTURE_2D, texture);
            glTexImage2D(
                GL_TEXTURE_2D, 0, GL_RGBA8 as GLint,
                width as GLsizei, height as GLsizei, 0,
                GL_RGBA, GL_UNSIGNED_BYTE, core::ptr::null(),
            );
            glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, GL_LINEAR as GLint);
            glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER, GL_LINEAR as GLint);
            glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_WRAP_S, GL_CLAMP_TO_EDGE as GLint);
            glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_WRAP_T, GL_CLAMP_TO_EDGE as GLint);
            glBindTexture(GL_TEXTURE_2D, 0);
        }
        self.stats.textures += 1;
        Ok(Rc::new(Gl3dTexture { texture, width, height }))
    }

    fn create_cube_texture(
        &mut self,
        _size: u32,
        _format: Context3DTextureFormat,
        _optimize_for_render_to_texture: bool,
        _streaming_levels: u32,
    ) -> Result<Rc<dyn Texture>, Error> {
        // Not faked: a game that needs a cube map should fail visibly here
        // rather than sample a blank 2D texture and look subtly wrong.
        Err(Error::Unimplemented("Stage3D cube textures".into()))
    }

    fn upload_shaders(
        &mut self,
        module: &RefCell<Option<Rc<dyn ShaderModule>>>,
        vertex_shader_agal: Vec<u8>,
        fragment_shader_agal: Vec<u8>,
    ) -> Result<(), naga_agal::AgalError> {
        let vertex = naga_agal::parse_bytecode(&vertex_shader_agal)?;
        let fragment = naga_agal::parse_bytecode(&fragment_shader_agal)?;
        *module.borrow_mut() = Some(Rc::new(Gl3dShaders {
            vertex,
            fragment,
            programs: RefCell::new(std::vec::Vec::new()),
        }));
        Ok(())
    }

    fn process_command(&mut self, command: Context3DCommand<'_>) {
        match command {
            Context3DCommand::ConfigureBackBuffer {
                width, height, anti_alias, ..
            } => {
                if anti_alias > 1 {
                    self.warn("context3d: anti-aliasing requested, rendering without it\n");
                }
                if width == 0 || height == 0 {
                    return;
                }
                let mut texture: GLuint = 0;
                unsafe {
                    glGenTextures(1, &mut texture);
                    if texture == 0 {
                        return;
                    }
                    glBindTexture(GL_TEXTURE_2D, texture);
                    glTexImage2D(
                        GL_TEXTURE_2D, 0, GL_RGBA8 as GLint,
                        width as GLsizei, height as GLsizei, 0,
                        GL_RGBA, GL_UNSIGNED_BYTE, core::ptr::null(),
                    );
                    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, GL_LINEAR as GLint);
                    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER, GL_LINEAR as GLint);
                    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_WRAP_S, GL_CLAMP_TO_EDGE as GLint);
                    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_WRAP_T, GL_CLAMP_TO_EDGE as GLint);
                    glBindTexture(GL_TEXTURE_2D, 0);
                }
                self.back_buffer = Some(BackBuffer {
                    handle: crate::backend::render::standalone_bitmap_from_texture(
                        texture, width, height,
                    ),
                    texture,
                    width,
                    height,
                });
                log(&std::format!("context3d: back buffer {width}x{height}\n"));
            }

            Context3DCommand::Clear { red, green, blue, alpha, .. } => {
                self.bind_target();
                unsafe {
                    glDisable(GL_SCISSOR_TEST);
                    glClearColor(red as GLfloat, green as GLfloat, blue as GLfloat, alpha as GLfloat);
                    glClear(GL_COLOR_BUFFER_BIT);
                }
            }

            Context3DCommand::SetRenderToTexture { texture, .. } => {
                self.stats.to_texture += 1;
                self.render_target = Some(texture);
                self.bind_target();
            }
            Context3DCommand::SetRenderToBackBuffer => {
                self.render_target = None;
                self.bind_target();
            }

            Context3DCommand::UploadToIndexBuffer { buffer, start_offset, data } => {
                if let Some(ib) = (buffer as &dyn Any).downcast_ref::<Gl3dIndexBuffer>() {
                    unsafe {
                        glBindBuffer(GL_ELEMENT_ARRAY_BUFFER, ib.buffer);
                        glBufferSubData(
                            GL_ELEMENT_ARRAY_BUFFER,
                            (start_offset * 2) as GLintptr,
                            data.len() as GLsizeiptr,
                            data.as_ptr() as *const _,
                        );
                        glBindBuffer(GL_ELEMENT_ARRAY_BUFFER, 0);
                    }
                }
            }

            Context3DCommand::UploadToVertexBuffer {
                buffer, start_vertex, data32_per_vertex, data,
            } => {
                if let Some(vb) = (buffer.as_ref() as &dyn Any).downcast_ref::<Gl3dVertexBuffer>() {
                    unsafe {
                        glBindBuffer(GL_ARRAY_BUFFER, vb.buffer);
                        glBufferSubData(
                            GL_ARRAY_BUFFER,
                            (start_vertex * data32_per_vertex as usize * 4) as GLintptr,
                            data.len() as GLsizeiptr,
                            data.as_ptr() as *const _,
                        );
                        glBindBuffer(GL_ARRAY_BUFFER, 0);
                    }
                }
            }

            Context3DCommand::SetVertexBufferAt { index, buffer, buffer_offset } => {
                if let Some(slot) = self.attrs.get_mut(index as usize) {
                    *slot = buffer.map(|(b, fmt)| (b, fmt, buffer_offset));
                }
            }

            Context3DCommand::SetShaders { module } => {
                self.shaders = module.and_then(|m| {
                    let any = m as Rc<dyn Any>;
                    any.downcast::<Gl3dShaders>().ok()
                });
            }

            Context3DCommand::SetProgramConstantsFromVector {
                program_type, first_register, matrix_raw_data_column_major,
            } => {
                let idx = match program_type {
                    ProgramType::Vertex => 0,
                    ProgramType::Fragment => 1,
                };
                let start = first_register as usize * 4;
                let dst = &mut self.constants[idx];
                for (i, v) in matrix_raw_data_column_major.iter().enumerate() {
                    if start + i < dst.len() {
                        dst[start + i] = *v;
                    }
                }
                self.constants_dirty[idx] = true;
            }

            Context3DCommand::SetTextureAt { sampler, texture, cube } => {
                if cube {
                    self.warn("context3d: cube sampler requested, left unbound\n");
                }
                if let Some(slot) = self.textures.get_mut(sampler as usize) {
                    *slot = texture;
                }
            }

            Context3DCommand::CopyBitmapToTexture { source, source_width, source_height, dest, .. } => {
                if let Some(t) = (dest.as_ref() as &dyn Any).downcast_ref::<Gl3dTexture>() {
                    unsafe {
                        glBindTexture(GL_TEXTURE_2D, t.texture);
                        glPixelStorei(GL_UNPACK_ALIGNMENT, 1);
                        glTexSubImage2D(
                            GL_TEXTURE_2D, 0, 0, 0,
                            source_width.min(t.width) as GLsizei,
                            source_height.min(t.height) as GLsizei,
                            GL_RGBA, GL_UNSIGNED_BYTE, source.as_ptr() as *const _,
                        );
                        glBindTexture(GL_TEXTURE_2D, 0);
                    }
                }
            }

            Context3DCommand::DrawTriangles { index_buffer, first_index, num_triangles } => {
                let Some(ib) = (index_buffer as &dyn Any).downcast_ref::<Gl3dIndexBuffer>() else {
                    return;
                };
                let Some(program) = self.program_for_current_attrs() else {
                    self.stats.dropped_no_program += 1;
                    return;
                };
                self.bind_target();
                self.upload_constants();
                unsafe { glUseProgram(program.program) };
                self.bind_attributes();
                self.bind_textures(&program);
                // A negative count means "the whole buffer" in Flash.
                let count = if num_triangles < 0 {
                    // The index buffer's own length is not tracked per draw;
                    // Ruffle always passes a real count for Starling, so this
                    // is a safety net rather than a path.
                    0
                } else {
                    num_triangles as usize * 3
                };
                if count == 0 {
                    return;
                }
                unsafe {
                    glBindBuffer(GL_ELEMENT_ARRAY_BUFFER, ib.buffer);
                    glDrawElements(
                        GL_TRIANGLES,
                        count as GLsizei,
                        GL_UNSIGNED_SHORT,
                        (first_index * 2) as *const _,
                    );
                }
                self.should_render = true;
                self.stats.draws += 1;
                self.stats.triangles += count as u64 / 3;
            }

            Context3DCommand::SetCulling { face } => unsafe {
                match face {
                    Context3DTriangleFace::None => glDisable(GL_CULL_FACE),
                    Context3DTriangleFace::Back => {
                        glEnable(GL_CULL_FACE);
                        glCullFace(GL_BACK);
                    }
                    Context3DTriangleFace::Front => {
                        glEnable(GL_CULL_FACE);
                        glCullFace(GL_FRONT);
                    }
                    Context3DTriangleFace::FrontAndBack => {
                        glEnable(GL_CULL_FACE);
                        glCullFace(GL_FRONT_AND_BACK);
                    }
                }
            },

            Context3DCommand::SetColorMask { red, green, blue, alpha } => unsafe {
                glColorMask(red as GLboolean, green as GLboolean, blue as GLboolean, alpha as GLboolean);
            },

            Context3DCommand::SetDepthTest { depth_mask, pass_compare_mode } => unsafe {
                // No depth attachment on the back buffer: a 2D framework asks
                // for the test but never relies on it, so honour the disable
                // and ignore the rest.
                match pass_compare_mode {
                    Context3DCompareMode::Always => glDisable(GL_DEPTH_TEST),
                    _ => glDisable(GL_DEPTH_TEST),
                }
                let _ = depth_mask;
            },

            Context3DCommand::SetBlendFactors { source_factor, destination_factor } => unsafe {
                glEnable(GL_BLEND);
                glBlendFunc(blend_factor(source_factor), blend_factor(destination_factor));
            },

            Context3DCommand::SetScissorRectangle { rect } => unsafe {
                match rect {
                    Some(r) => {
                        glEnable(GL_SCISSOR_TEST);
                        glScissor(
                            r.x_min.to_pixels() as GLint,
                            r.y_min.to_pixels() as GLint,
                            r.width().to_pixels() as GLsizei,
                            r.height().to_pixels() as GLsizei,
                        );
                    }
                    None => glDisable(GL_SCISSOR_TEST),
                }
            },

            // Stencil actions, sampler states, reference values and the like:
            // Starling sets some of them and depends on none. Named as a group
            // rather than faked one by one.
            _ => {
                self.warn("context3d: unsupported command ignored (stencil / sampler state)\n");
            }
        }
    }

    fn present(&mut self) {
        self.restore_2d_state();
        self.stats.frames += 1;
        if self.stats.frames >= 60 {
            log(&std::format!(
                "context3d: {} frames — {} draws, {} triangles, {} programs, {} textures, \
                 {} render-to-texture, {} draws dropped (no program)\n",
                self.stats.frames,
                self.stats.draws,
                self.stats.triangles,
                self.stats.programs,
                self.stats.textures,
                self.stats.to_texture,
                self.stats.dropped_no_program,
            ));
            self.stats = Stage3DStats::default();
        }
    }
}

impl Drop for SwitchContext3D {
    fn drop(&mut self) {
        unsafe {
            glDeleteFramebuffers(1, &self.fbo);
            glDeleteVertexArrays(1, &self.vao);
            glDeleteBuffers(2, self.ubo.as_ptr());
        }
    }
}
