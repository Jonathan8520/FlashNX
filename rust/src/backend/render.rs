//! RenderBackend impl — wgpu's GL backend running on switch-mesa.
//!
//! Phase 0: unused. main.cpp/gl_context.cpp owns the GL context and
//! lib.rs::ruffle_render_frame issues glClear directly.
//!
//! Phase 1 plan:
//!   - Pull wgpu (default features) but force GLES backend on Switch.
//!   - Build the wgpu Instance from the existing EGL display/context owned by
//!     C++ via wgpu_hal::gles::AdapterContext::new_external (raw GL handles).
//!   - Implement `ruffle_render::backend::RenderBackend` MVP methods:
//!       submit_frame, register_shape, register_bitmap,
//!       update_texture, viewport_dimensions.
//!
//! Pivot path if wgpu-on-mesa misbehaves: write a hand-rolled RenderBackend
//! directly against GLES2/GL4.3 (no wgpu layer). More code, fewer unknowns.
