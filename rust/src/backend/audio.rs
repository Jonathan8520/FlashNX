//! `SwitchAudioBackend` — port of `CpalAudioBackend` from
//! `frontend-utils/src/backends/audio.rs`, wired to libnx's `audren` via the
//! C++ side in `cpp/src/audio.cpp`.
//!
//! The pattern is identical to cpal:
//!   1. `AudioMixer` does all the SWF audio work (decoding ADPCM/PCM, mixing
//!      sound instances, applying volume).
//!   2. A `mixer.proxy()` is stashed in a process-global slot so the C++
//!      audio worker thread can pull samples on its own cadence (audren
//!      frame events fire ~every 5 ms).
//!   3. The C++ thread calls back into Rust through `ruffle_audio_fill_buffer`,
//!      which invokes `proxy.mix::<i16>(buf)`. `audrvVoiceAddWaveBuf` then
//!      hands the filled PCM to the renderer.
//!
//! Format chosen: 48 kHz stereo i16 — Switch native audio rate, matches what
//! libnx `audrenSoftwareAudioRendererConfig` documents as the standard.

use core::ffi::{c_int, c_uint};
use std::sync::{Mutex, OnceLock};

use ruffle_core::backend::audio::{
    swf, AudioBackend, AudioMixer, AudioMixerProxy, DecodeError, RegisterError, SoundHandle,
    SoundInstanceHandle, SoundStreamInfo, SoundTransform,
};
use ruffle_core::impl_audio_mixer_backend;

/// Process-global slot for the mixer proxy. The C++ audio worker thread
/// pulls samples through here independently of any Player lock. Wrapped in
/// `Mutex<Option<…>>` so we can swap it out cleanly during shutdown without
/// risking the C++ side reading a freed proxy.
static AUDIO_PROXY: OnceLock<Mutex<Option<AudioMixerProxy>>> = OnceLock::new();

const OUTPUT_CHANNELS: u8 = 2;
const OUTPUT_SAMPLE_RATE: u32 = 48_000;

extern "C" {
    /// Bring audren up. Returns 0 on success, non-zero on failure. Idempotent;
    /// safe to call multiple times.
    fn ruffle_audio_init(sample_rate: c_uint, channels: c_uint) -> c_int;
    /// Tear audren down. Idempotent.
    fn ruffle_audio_shutdown();
    /// Start / pause the audren voice. The mixer keeps state independently;
    /// these only gate whether samples reach the speakers.
    fn ruffle_audio_play();
    fn ruffle_audio_pause();
}

pub struct SwitchAudioBackend {
    mixer: AudioMixer,
}

impl SwitchAudioBackend {
    pub fn new() -> Self {
        let mut mixer = AudioMixer::new(OUTPUT_CHANNELS, OUTPUT_SAMPLE_RATE);
        // Mario 63 plays many short SFX simultaneously plus MP3 music. The
        // mixer sums them in f32; if the sum exceeds [-1, 1] the i16 cast
        // saturates and we get audible crackle (observed 2026-05-24 even
        // with no underruns — voice_playing=1 throughout). Headroom of 0.5
        // is a conservative default; the SWF can still bump it via AS2.
        mixer.set_volume(0.5);
        // Stash the proxy in the global slot so the C++ side can pull samples.
        let slot = AUDIO_PROXY.get_or_init(|| Mutex::new(None));
        if let Ok(mut guard) = slot.lock() {
            *guard = Some(mixer.proxy());
        }
        let rc = unsafe {
            ruffle_audio_init(OUTPUT_SAMPLE_RATE as c_uint, OUTPUT_CHANNELS as c_uint)
        };
        if rc != 0 {
            // Audio failed to come up; mixer still works in the background
            // (calls return Ok, sounds just don't reach speakers). Log via
            // tracing so the user sees it in nxlink.
            tracing::warn!("audren init returned {} — audio will be silent", rc);
        }
        Self { mixer }
    }
}

impl Default for SwitchAudioBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for SwitchAudioBackend {
    fn drop(&mut self) {
        // Drop the proxy first so the C++ side stops pulling from a stale
        // mixer, then shut audren down.
        if let Some(slot) = AUDIO_PROXY.get() {
            if let Ok(mut guard) = slot.lock() {
                *guard = None;
            }
        }
        unsafe { ruffle_audio_shutdown() };
    }
}

impl AudioBackend for SwitchAudioBackend {
    impl_audio_mixer_backend!(mixer);

    fn play(&mut self) {
        unsafe { ruffle_audio_play() };
    }

    fn pause(&mut self) {
        unsafe { ruffle_audio_pause() };
    }
}

/// Called from the C++ audio worker thread to fill `len` interleaved i16
/// stereo samples (so the buffer holds `len/2` frames). No-op when no
/// SwitchAudioBackend is currently alive (returns leaving the buffer at
/// whatever value the caller initialised it to — typically zero).
#[no_mangle]
pub extern "C" fn ruffle_audio_fill_buffer(out: *mut i16, len: usize) {
    if out.is_null() || len == 0 {
        return;
    }
    // SAFETY: the C++ caller guarantees `out` points to at least `len`
    // contiguous i16s for the duration of the call. Audren wave buffers
    // are pinned in dedicated memory pools so they don't move under us.
    let buf = unsafe { core::slice::from_raw_parts_mut(out, len) };
    let Some(slot) = AUDIO_PROXY.get() else {
        return;
    };
    let Ok(mut guard) = slot.lock() else { return };
    let Some(proxy) = guard.as_mut() else { return };
    proxy.mix::<i16>(buf);
}
