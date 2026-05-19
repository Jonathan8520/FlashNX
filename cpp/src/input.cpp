#include <switch.h>

// Phase 1: translate joycon/touch state to Flash-style mouse/key events
// and forward them to the Rust side via an FFI callback registered at init.
// For now, main.cpp handles input directly (exit on Plus button).
