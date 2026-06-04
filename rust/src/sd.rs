//! SD-card durability helper.
//!
//! On the Switch, `sdmc:` is a libnx fsdev mount whose writes are buffered:
//! data written through `std::fs` / newlib does not reach the physical card
//! until the device is committed (or the process exits cleanly). FlashNX can
//! run in two flavours that are separate OS processes — a library applet
//! (album/photo takeover) and a title-takeover application — and an
//! uncommitted write in one is invisible to the other. The user-visible
//! symptom was the URL history reading empty in one mode and then being
//! overwritten on the next add (see `library::save_history_to_sd`).
//!
//! Call [`commit`] right after every write we want to survive a mode switch
//! or an abrupt exit: URL history, settings, saves (`.sol`), keymap and meta
//! sidecars. It is a thin wrapper over `fsdevCommitDevice("sdmc")`.

use core::ffi::c_int;

extern "C" {
    /// Defined in `cpp/src/ruffle_bridge.cpp`. Returns 0 on success, -1 on
    /// failure (logged C++-side). Never panics.
    fn flashnx_commit_sd() -> c_int;
}

/// Flush buffered `sdmc:` writes to the physical card. Best-effort: a
/// failure is logged on the C++ side and otherwise ignored — the in-memory
/// state stays valid for the session.
pub fn commit() {
    unsafe {
        flashnx_commit_sd();
    }
}
