//! UiBackend impl — bridges Ruffle's UI callbacks to FlashNX.
//!
//! Almost everything is delegated to ruffle_core's `NullUiBackend`. The one
//! piece we actually implement is the virtual-keyboard hook. Ruffle's focus
//! tracker calls `open_virtual_keyboard()` the instant an editable TextField
//! gains focus (mouse click, re-click on an already-focused field, or Tab —
//! AVM1 and AVM2 alike) and `close_virtual_keyboard()` when focus leaves it.
//! See `focus_tracker.rs::update_virtual_keyboard`.
//!
//! We cannot raise the Switch software keyboard from inside these hooks: they
//! fire deep in the player's update loop within a GC mutation context, and
//! `swkbdShow` is a blocking applet that must run on the main thread. So we
//! only record the request in an atomic. The C++ game loop polls it once per
//! frame via `ruffle_keyboard_take_request`, suspends the game, runs swkbd
//! configured to match the field, and feeds the result back through
//! `ruffle_keyboard_submit` as ordinary text events.

use core::sync::atomic::{AtomicBool, Ordering};

use ruffle_core::backend::ui::{
    DialogResultFuture, FileFilter, FontDefinition, FullscreenError, LanguageIdentifier,
    MouseCursor, NullUiBackend, UiBackend,
};
use ruffle_core::font::{FontFileData, FontQuery, FontType};
use url::Url;

// In-game device fonts (issue #54). Ruffle renders a running SWF's dynamic text
// itself and asks the UiBackend for device fonts. NullUiBackend supplies none,
// so any glyph outside Ruffle's bundled Latin "Noto Sans" subset — every CJK
// character, and any font name we don't provide — renders BLANK in-game (the UI
// menus are unaffected: those use our own draw_text/glyphs.rs path). We feed
// Ruffle the Switch's own shared fonts (the same source glyphs.rs uses) so the
// FontSet's per-glyph fallback can resolve Latin/JP/CJK in a running game.
extern "C" {
    // cpp/src/ruffle_bridge.cpp — DECRYPTED TTF/OTF for a PlSharedFontType.
    // Returns a pointer into pl's shared memory (mapped for the whole process,
    // so the slice is effectively 'static) + size, or null / 0 on failure.
    fn ruffle_shared_font(kind: core::ffi::c_int, out_size: *mut u32) -> *const u8;
}

/// Switch shared fonts exposed as device fonts, in fallback order. Standard
/// (Latin + Japanese) leads as the main font; the CJK packs cover the glyphs it
/// lacks. `PlSharedFontType`: 0=Standard, 1=ChineseSimplified, 2=ExtChinese-
/// Simplified, 3=ChineseTraditional, 4=Korean.
const SHARED_DEVICE_FONTS: &[(&str, core::ffi::c_int)] = &[
    ("FlashNX Standard", 0),
    ("FlashNX Chinese", 1),
    ("FlashNX Chinese Ext", 2),
    ("FlashNX Chinese Traditional", 3),
    ("FlashNX Korean", 4),
];

/// Borrow a Switch shared font as a `'static` byte slice (pl keeps the decrypted
/// data mapped for the whole process), or None if pl can't provide it.
fn shared_font_slice(kind: core::ffi::c_int) -> Option<&'static [u8]> {
    let mut len: u32 = 0;
    let ptr = unsafe { ruffle_shared_font(kind, &mut len) };
    if ptr.is_null() || len == 0 {
        return None;
    }
    Some(unsafe { core::slice::from_raw_parts(ptr, len as usize) })
}

/// Pending "open the software keyboard" request. Set by `open_virtual_keyboard`
/// when an editable TextField takes focus; cleared either by
/// `close_virtual_keyboard` (the field lost focus before C++ got to it) or by
/// the C++ loop consuming it through `take_keyboard_request`.
static KEYBOARD_OPEN_REQUEST: AtomicBool = AtomicBool::new(false);

/// Record that an editable field wants the keyboard. Idempotent.
fn request_keyboard_open() {
    KEYBOARD_OPEN_REQUEST.store(true, Ordering::SeqCst);
}

/// Drop a pending request (focus left the field).
fn cancel_keyboard_request() {
    KEYBOARD_OPEN_REQUEST.store(false, Ordering::SeqCst);
}

/// Consume the pending request, if any. Returns `true` at most once per
/// `open_virtual_keyboard` call. Polled by the C++ game loop each frame.
pub fn take_keyboard_request() -> bool {
    KEYBOARD_OPEN_REQUEST.swap(false, Ordering::SeqCst)
}

/// UiBackend for FlashNX. Delegates everything to `NullUiBackend` except the
/// virtual-keyboard hooks, which forward to the atomic request flag above.
pub struct SwitchUiBackend {
    inner: NullUiBackend,
}

impl SwitchUiBackend {
    pub fn new() -> Self {
        Self {
            inner: NullUiBackend::new(),
        }
    }
}

impl UiBackend for SwitchUiBackend {
    fn mouse_visible(&self) -> bool {
        self.inner.mouse_visible()
    }

    fn set_mouse_visible(&mut self, visible: bool) {
        self.inner.set_mouse_visible(visible)
    }

    fn set_mouse_cursor(&mut self, cursor: MouseCursor) {
        self.inner.set_mouse_cursor(cursor)
    }

    fn clipboard_content(&mut self) -> String {
        self.inner.clipboard_content()
    }

    fn set_clipboard_content(&mut self, content: String) {
        self.inner.set_clipboard_content(content)
    }

    fn set_fullscreen(&mut self, is_full: bool) -> Result<(), FullscreenError> {
        self.inner.set_fullscreen(is_full)
    }

    fn display_root_movie_download_failed_message(
        &self,
        invalid_swf: bool,
        fetched_error: String,
    ) {
        self.inner
            .display_root_movie_download_failed_message(invalid_swf, fetched_error)
    }

    fn message(&self, message: &str) {
        self.inner.message(message)
    }

    fn open_virtual_keyboard(&self) {
        request_keyboard_open();
    }

    fn close_virtual_keyboard(&self) {
        cancel_keyboard_request();
    }

    fn language(&self) -> LanguageIdentifier {
        self.inner.language()
    }

    fn display_unsupported_video(&self, url: Url) {
        self.inner.display_unsupported_video(url)
    }

    fn load_device_font(&self, query: &FontQuery, register: &mut dyn FnMut(FontDefinition)) {
        // Back the requested name with the Standard shared font so a single-font
        // lookup (text not routed through the sorted fallback path) still finds
        // glyphs instead of nothing.
        if let Some(bytes) = shared_font_slice(0) {
            register(FontDefinition::FontFile {
                name: query.name.clone(),
                is_bold: query.is_bold,
                is_italic: query.is_italic,
                data: FontFileData::new(bytes),
                index: 0,
            });
        }
    }

    fn sort_device_fonts(
        &self,
        _query: &FontQuery,
        register: &mut dyn FnMut(FontDefinition),
    ) -> Vec<FontQuery> {
        // Register the shared fonts and hand back an ordered fallback chain;
        // FontSet::resolve_glyph walks it per character (Latin/JP from Standard,
        // CJK from the Chinese/Korean packs). This is what makes a running game's
        // Chinese text render instead of coming out blank (#54).
        let mut chain = Vec::new();
        for (name, kind) in SHARED_DEVICE_FONTS {
            if let Some(bytes) = shared_font_slice(*kind) {
                register(FontDefinition::FontFile {
                    name: (*name).to_string(),
                    is_bold: false,
                    is_italic: false,
                    data: FontFileData::new(bytes),
                    index: 0,
                });
                chain.push(FontQuery::new(
                    FontType::Device,
                    (*name).to_string(),
                    false,
                    false,
                ));
            }
        }
        chain
    }

    fn display_file_open_dialog(
        &mut self,
        filters: Vec<FileFilter>,
    ) -> Option<DialogResultFuture> {
        self.inner.display_file_open_dialog(filters)
    }

    fn display_file_save_dialog(
        &mut self,
        file_name: String,
        title: String,
    ) -> Option<DialogResultFuture> {
        self.inner.display_file_save_dialog(file_name, title)
    }

    fn close_file_dialog(&mut self) {
        self.inner.close_file_dialog()
    }
}
