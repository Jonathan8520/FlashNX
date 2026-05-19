//! AudioBackend impl — backed by Horizon's audren service.
//!
//! The `nx` Rust crate does NOT expose audren, so this calls into libnx C
//! functions via ffi::libnx. The C++ side owns audren initialization; this
//! module pushes PCM frames into the queue exposed by ../../cpp/src/audio.cpp.
//!
//! Phase 1: implement `ruffle_core::backend::audio::AudioBackend`.
