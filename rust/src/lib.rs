//! flash-for-switch — Ruffle Flash player port to Nintendo Switch.
//!
//! Compiles as a no_std staticlib that the devkitPro C++ wrapper in ../cpp/
//! links against to produce the final .nro.
//!
//! Phase 0: hello triangle — `ruffle_render_frame` just clears the screen red,
//! proving the FFI path Rust -> C -> libnx/EGL -> switch-mesa -> GPU works.
//! Phase 1: switch to a std-via-newlib custom target so we can pull in
//! ruffle_core (which requires std), and implement the backend traits below.

#![no_std]

mod backend;
mod ffi;
mod player;

use core::ffi::{c_float, c_int, c_uint};
use core::panic::PanicInfo;

extern "C" {
    fn glClearColor(r: c_float, g: c_float, b: c_float, a: c_float);
    fn glClear(mask: c_uint);
}

const GL_COLOR_BUFFER_BIT: c_uint = 0x0000_4000;

#[no_mangle]
pub extern "C" fn ruffle_init() -> c_int {
    0
}

#[no_mangle]
pub extern "C" fn ruffle_render_frame() {
    unsafe {
        glClearColor(1.0, 0.0, 0.0, 1.0);
        glClear(GL_COLOR_BUFFER_BIT);
    }
}

#[no_mangle]
pub extern "C" fn ruffle_shutdown() {}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    loop {}
}
