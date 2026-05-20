//! Minimal OpenGL FFI subset needed for Phase 0.5 (triangle).
//!
//! Targets desktop GL core profile (the EGL context in cpp/gl_context.cpp is
//! created with EGL_OPENGL_API + EGL_CONTEXT_OPENGL_CORE_PROFILE_BIT, GL 4.3).
//! All names match the standard libGL ABI exposed by switch-mesa.

#![allow(non_snake_case, non_camel_case_types, dead_code)]

use core::ffi::{c_char, c_float, c_int, c_uchar, c_uint, c_void};

pub type GLbitfield = c_uint;
pub type GLboolean = c_uchar;
pub type GLchar = c_char;
pub type GLenum = c_uint;
pub type GLfloat = c_float;
pub type GLint = c_int;
pub type GLsizei = c_int;
pub type GLsizeiptr = isize;
pub type GLuint = c_uint;

pub const GL_FALSE: GLboolean = 0;
pub const GL_TRUE: GLboolean = 1;

pub const GL_COLOR_BUFFER_BIT: GLbitfield = 0x0000_4000;

pub const GL_TRIANGLES: GLenum = 0x0004;
pub const GL_FLOAT: GLenum = 0x1406;

pub const GL_ARRAY_BUFFER: GLenum = 0x8892;
pub const GL_STATIC_DRAW: GLenum = 0x88E4;

pub const GL_FRAGMENT_SHADER: GLenum = 0x8B30;
pub const GL_VERTEX_SHADER: GLenum = 0x8B31;
pub const GL_COMPILE_STATUS: GLenum = 0x8B81;
pub const GL_LINK_STATUS: GLenum = 0x8B82;
pub const GL_INFO_LOG_LENGTH: GLenum = 0x8B84;

extern "C" {
    pub fn glClearColor(r: GLfloat, g: GLfloat, b: GLfloat, a: GLfloat);
    pub fn glClear(mask: GLbitfield);
    pub fn glViewport(x: GLint, y: GLint, w: GLsizei, h: GLsizei);
    pub fn glDrawArrays(mode: GLenum, first: GLint, count: GLsizei);

    pub fn glGenBuffers(n: GLsizei, buffers: *mut GLuint);
    pub fn glBindBuffer(target: GLenum, buffer: GLuint);
    pub fn glBufferData(target: GLenum, size: GLsizeiptr, data: *const c_void, usage: GLenum);

    pub fn glGenVertexArrays(n: GLsizei, arrays: *mut GLuint);
    pub fn glBindVertexArray(array: GLuint);
    pub fn glEnableVertexAttribArray(index: GLuint);
    pub fn glVertexAttribPointer(
        index: GLuint,
        size: GLint,
        ty: GLenum,
        normalized: GLboolean,
        stride: GLsizei,
        pointer: *const c_void,
    );

    pub fn glCreateShader(ty: GLenum) -> GLuint;
    pub fn glShaderSource(
        shader: GLuint,
        count: GLsizei,
        strings: *const *const GLchar,
        lengths: *const GLint,
    );
    pub fn glCompileShader(shader: GLuint);
    pub fn glGetShaderiv(shader: GLuint, pname: GLenum, params: *mut GLint);
    pub fn glGetShaderInfoLog(
        shader: GLuint,
        max_len: GLsizei,
        length: *mut GLsizei,
        info_log: *mut GLchar,
    );
    pub fn glDeleteShader(shader: GLuint);

    pub fn glCreateProgram() -> GLuint;
    pub fn glAttachShader(program: GLuint, shader: GLuint);
    pub fn glLinkProgram(program: GLuint);
    pub fn glGetProgramiv(program: GLuint, pname: GLenum, params: *mut GLint);
    pub fn glGetProgramInfoLog(
        program: GLuint,
        max_len: GLsizei,
        length: *mut GLsizei,
        info_log: *mut GLchar,
    );
    pub fn glUseProgram(program: GLuint);
}
