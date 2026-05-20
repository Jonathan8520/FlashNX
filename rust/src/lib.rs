//! flash-for-switch — Ruffle Flash player port to Nintendo Switch.
//!
//! Compiles as a staticlib that the devkitPro C++ wrapper in ../cpp/
//! links against to produce the final .nro.
//!
//! Phase 0:   ruffle_render_frame = glClear red. Proved Rust→C→libnx/EGL→
//!            switch-mesa→Tegra X1 on real hardware (2026-05-20).
//! Phase 0.5: ruffle_render_frame compiles GLSL + draws an RGB triangle.
//!            Validated on hardware (2026-05-21).
//! Phase 1.1: std is now pulled in via -Z build-std on the existing tier-3
//!            target (its spec already says os=horizon, which stdlib supports
//!            via the 3DS cfg branches). No custom target JSON needed.
//! Phase 1.2: pull ruffle_core, neutralize cpal/reqwest, implement RenderBackend.

// stdlib is built for an "unrecognized" platform (we forced family=unix +
// env=newlib + os=horizon onto a target spec that doesn't officially declare
// them). Opt in to the resulting restricted std (some APIs return errors).
#![feature(restricted_std)]

mod backend;
mod ffi;
mod player;

use core::ffi::{c_char, c_int};
use core::ptr;

use ffi::gl::*;

extern "C" {
    fn ruffle_log_cstr(msg: *const c_char);
}

const VERT_SRC: &[u8] = b"#version 330 core\n\
layout(location = 0) in vec2 a_pos;\n\
layout(location = 1) in vec3 a_col;\n\
out vec3 v_col;\n\
void main() {\n\
    v_col = a_col;\n\
    gl_Position = vec4(a_pos, 0.0, 1.0);\n\
}\n\0";

const FRAG_SRC: &[u8] = b"#version 330 core\n\
in vec3 v_col;\n\
out vec4 frag_color;\n\
void main() {\n\
    frag_color = vec4(v_col, 1.0);\n\
}\n\0";

#[rustfmt::skip]
const TRIANGLE: [f32; 15] = [
    // pos        // color (RGB)
     0.0,  0.6,   1.0, 0.0, 0.0,
    -0.6, -0.6,   0.0, 1.0, 0.0,
     0.6, -0.6,   0.0, 0.0, 1.0,
];

struct GpuState {
    program: GLuint,
    vao: GLuint,
    #[allow(dead_code)] // kept for shutdown / future glDeleteBuffers
    vbo: GLuint,
}

static mut GPU: Option<GpuState> = None;

#[no_mangle]
pub extern "C" fn ruffle_init() -> c_int {
    // bisect-G (2026-05-21): patched RandomState fixed HashMap. Now retry
    // the original Phase 1.2 goal: construct PlayerBuilder. If still crashes,
    // ruffle_core has its own lazy thread_local somewhere (e.g., tracing,
    // gc-arena), and we'll need more patches or a different TLS strategy.
    let _builder = ruffle_core::PlayerBuilder::new();
    let banner = std::format!(
        "bisect-G: PlayerBuilder OK (size={})\n",
        std::mem::size_of::<ruffle_core::PlayerBuilder>(),
    );
    let mut bytes: std::vec::Vec<u8> = banner.into_bytes();
    bytes.push(0);
    unsafe { ruffle_log_cstr(bytes.as_ptr() as *const c_char); }

    let program = match build_program() {
        Some(p) => p,
        None => return -1,
    };

    let mut vao: GLuint = 0;
    let mut vbo: GLuint = 0;
    unsafe {
        glGenVertexArrays(1, &mut vao);
        glBindVertexArray(vao);

        glGenBuffers(1, &mut vbo);
        glBindBuffer(GL_ARRAY_BUFFER, vbo);
        glBufferData(
            GL_ARRAY_BUFFER,
            core::mem::size_of_val(&TRIANGLE) as GLsizeiptr,
            TRIANGLE.as_ptr() as *const _,
            GL_STATIC_DRAW,
        );

        let stride = (5 * core::mem::size_of::<f32>()) as GLsizei;
        glEnableVertexAttribArray(0);
        glVertexAttribPointer(0, 2, GL_FLOAT, GL_FALSE, stride, ptr::null());
        glEnableVertexAttribArray(1);
        glVertexAttribPointer(
            1,
            3,
            GL_FLOAT,
            GL_FALSE,
            stride,
            (2 * core::mem::size_of::<f32>()) as *const _,
        );

        glBindVertexArray(0);
        glBindBuffer(GL_ARRAY_BUFFER, 0);

        GPU = Some(GpuState { program, vao, vbo });
    }

    log(b"ruffle_init: triangle pipeline ready\n\0");
    0
}

#[no_mangle]
pub extern "C" fn ruffle_render_frame() {
    unsafe {
        glClearColor(0.05, 0.05, 0.1, 1.0);
        glClear(GL_COLOR_BUFFER_BIT);

        if let Some(gpu) = (*ptr::addr_of!(GPU)).as_ref() {
            glUseProgram(gpu.program);
            glBindVertexArray(gpu.vao);
            glDrawArrays(GL_TRIANGLES, 0, 3);
            glBindVertexArray(0);
            glUseProgram(0);
        }
    }
}

#[no_mangle]
pub extern "C" fn ruffle_shutdown() {}

// getrandom 0.3 custom backend. Phase 1.2 stub: LCG seeded from a static
// counter. Insecure but enough for game RNG (Mario 63 wiggles, particle
// effects). Phase 2 will route to libnx `csrngGetRandomBytes` for real
// entropy via Switch's `csrng` service.
#[no_mangle]
pub unsafe extern "Rust" fn __getrandom_v03_custom(
    dest: *mut u8,
    len: usize,
) -> Result<(), getrandom::Error> {
    use core::sync::atomic::{AtomicU64, Ordering};
    static SEED: AtomicU64 = AtomicU64::new(0x9E3779B97F4A7C15);
    let mut state = SEED.load(Ordering::Relaxed);
    for i in 0..len {
        // xorshift64* — fast, deterministic, NOT cryptographic.
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        *dest.add(i) = (state.wrapping_mul(0x2545F4914F6CDD1D) >> 32) as u8;
    }
    SEED.store(state, Ordering::Relaxed);
    Ok(())
}

fn build_program() -> Option<GLuint> {
    let vs = compile_shader(GL_VERTEX_SHADER, VERT_SRC)?;
    let fs = compile_shader(GL_FRAGMENT_SHADER, FRAG_SRC)?;
    unsafe {
        let program = glCreateProgram();
        glAttachShader(program, vs);
        glAttachShader(program, fs);
        glLinkProgram(program);

        let mut status: GLint = 0;
        glGetProgramiv(program, GL_LINK_STATUS, &mut status);
        if status == 0 {
            log_info_log(program, true);
            glDeleteShader(vs);
            glDeleteShader(fs);
            return None;
        }

        glDeleteShader(vs);
        glDeleteShader(fs);
        Some(program)
    }
}

fn compile_shader(kind: GLenum, src_nul: &[u8]) -> Option<GLuint> {
    unsafe {
        let shader = glCreateShader(kind);
        // src_nul is NUL-terminated → length omitted (NULL length array).
        let src_ptr = src_nul.as_ptr() as *const GLchar;
        glShaderSource(shader, 1, &src_ptr, ptr::null());
        glCompileShader(shader);

        let mut status: GLint = 0;
        glGetShaderiv(shader, GL_COMPILE_STATUS, &mut status);
        if status == 0 {
            log(b"shader compile failed:\n\0");
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
            glGetProgramInfoLog(
                handle,
                buf.len() as GLsizei,
                &mut written,
                buf.as_mut_ptr() as *mut GLchar,
            );
        } else {
            glGetShaderInfoLog(
                handle,
                buf.len() as GLsizei,
                &mut written,
                buf.as_mut_ptr() as *mut GLchar,
            );
        }
        // Force NUL terminator at the last byte just in case.
        buf[buf.len() - 1] = 0;
        ruffle_log_cstr(buf.as_ptr() as *const c_char);
    }
}

fn log(msg_nul: &[u8]) {
    unsafe {
        ruffle_log_cstr(msg_nul.as_ptr() as *const c_char);
    }
}

