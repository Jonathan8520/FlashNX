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
use ruffle_core::font::FontQuery;
use url::Url;

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
        self.inner.load_device_font(query, register)
    }

    fn sort_device_fonts(
        &self,
        query: &FontQuery,
        register: &mut dyn FnMut(FontDefinition),
    ) -> Vec<FontQuery> {
        self.inner.sort_device_fonts(query, register)
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
