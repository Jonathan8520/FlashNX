//! flash-for-switch — Ruffle Flash player port to Nintendo Switch.
//!
//! Compiles as a no_std staticlib that the devkitPro C++ wrapper in ../cpp/
//! links against to produce the final .nro.
//!
//! Phase 0:   ruffle_render_frame = glClear red. Proved Rust→C→libnx/EGL→
//!            switch-mesa→Tegra X1 on real hardware (2026-05-20).
//! Phase 0.5: ruffle_render_frame compiles a GLSL program once, then draws a
//!            single colored triangle. Derisks that switch-mesa's GLSL compiler
//!            and the desktop-GL core-profile pipeline both work beyond glClear.
//! Phase 1:   switch to a std-via-newlib custom target so we can pull in
//!            ruffle_core (which requires std), and implement the backend
//!            traits in src/backend/.

#![no_std]

mod backend;
mod ffi;
mod player;

use core::ffi::{c_char, c_int};
use core::panic::PanicInfo;
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

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    loop {}
}
