//! game-music-emu (cpp/third_party/gme), handed to our Ruffle so the
//! CrossBridge build of libgme a game carries runs natively in its place
//! (`flashnx_gme` in our Ruffle, which says why).
//!
//! An emulator crosses as the address of its `Music_Emu`; libgme's error
//! strings are static C strings, copied out.

use core::ffi::{c_char, c_int, c_long, c_void, CStr};
use ruffle_core::flashnx_gme::{
    self, GmeApi, KIND_GBS, KIND_KSS, KIND_NSF, KIND_NSFE, KIND_SAP, KIND_SPC,
};

type Emu = *mut c_void;

extern "C" {
    static gme_gbs_type: *const c_void;
    static gme_kss_type: *const c_void;
    static gme_nsf_type: *const c_void;
    static gme_nsfe_type: *const c_void;
    static gme_sap_type: *const c_void;
    static gme_spc_type: *const c_void;

    fn gme_new_emu(kind: *const c_void, sample_rate: c_int) -> Emu;
    fn gme_open_data(data: *const c_void, size: c_long, out: *mut Emu, sample_rate: c_int)
        -> *const c_char;
    fn gme_delete(emu: Emu);
    fn gme_load_data(emu: Emu, data: *const c_void, size: c_long) -> *const c_char;
    fn gme_start_track(emu: Emu, index: c_int) -> *const c_char;
    fn gme_play(emu: Emu, count: c_int, out: *mut i16) -> *const c_char;
    fn gme_track_ended(emu: Emu) -> c_int;
    fn gme_tell(emu: Emu) -> c_int;
    fn gme_seek(emu: Emu, msec: c_int) -> *const c_char;
    fn gme_set_tempo(emu: Emu, tempo: f64);
    fn gme_set_fade(emu: Emu, start_msec: c_int);
    fn gme_set_stereo_depth(emu: Emu, depth: f64);
    fn gme_track_count(emu: Emu) -> c_int;
    fn gme_track_info(emu: Emu, out: *mut *mut c_void, track: c_int) -> *const c_char;
    fn gme_free_info(info: *mut c_void);
    fn gme_ignore_silence(emu: Emu, ignore: c_int);
    fn gme_voice_count(emu: Emu) -> c_int;
    fn gme_mute_voice(emu: Emu, index: c_int, mute: c_int);
    fn gme_mute_voices(emu: Emu, mask: c_int);
    fn gme_warning(emu: Emu) -> *const c_char;
    fn gme_type(emu: Emu) -> *const c_void;
}

fn type_of(kind: u8) -> *const c_void {
    unsafe {
        match kind {
            KIND_GBS => gme_gbs_type,
            KIND_KSS => gme_kss_type,
            KIND_NSF => gme_nsf_type,
            KIND_NSFE => gme_nsfe_type,
            KIND_SAP => gme_sap_type,
            KIND_SPC => gme_spc_type,
            _ => core::ptr::null(),
        }
    }
}

fn kind_of(emu: Emu) -> u8 {
    let t = unsafe { gme_type(emu) };
    [KIND_GBS, KIND_KSS, KIND_NSF, KIND_NSFE, KIND_SAP, KIND_SPC]
        .into_iter()
        .find(|&k| type_of(k) == t)
        .unwrap_or(0)
}

fn err(e: *const c_char) -> Option<String> {
    if e.is_null() {
        None
    } else {
        Some(unsafe { CStr::from_ptr(e) }.to_string_lossy().into_owned())
    }
}

fn emu(e: usize) -> Emu {
    e as Emu
}

fn new_emu(kind: u8, sample_rate: i32) -> usize {
    let t = type_of(kind);
    if t.is_null() {
        return 0;
    }
    unsafe { gme_new_emu(t, sample_rate) as usize }
}

fn open_data(data: &[u8], sample_rate: i32) -> Result<(usize, u8), String> {
    let mut out: Emu = core::ptr::null_mut();
    let e = unsafe {
        gme_open_data(data.as_ptr().cast(), data.len() as c_long, &mut out, sample_rate)
    };
    match err(e) {
        Some(e) => Err(e),
        None if out.is_null() => Err("gme_open_data: no emulator".into()),
        None => Ok((out as usize, kind_of(out))),
    }
}

fn track_info(e: usize, track: i32) -> Option<String> {
    let mut info: *mut c_void = core::ptr::null_mut();
    let r = err(unsafe { gme_track_info(emu(e), &mut info, track) });
    if !info.is_null() {
        unsafe { gme_free_info(info) };
    }
    r
}

/// Called once at start-up; `gmenative.off` leaves the CrossBridge build.
pub fn install(enabled: bool) {
    flashnx_gme::set_api(GmeApi {
        new_emu,
        open_data,
        delete: |e| unsafe { gme_delete(emu(e)) },
        load_data: |e, data| err(unsafe { gme_load_data(emu(e), data.as_ptr().cast(), data.len() as c_long) }),
        start_track: |e, track| err(unsafe { gme_start_track(emu(e), track) }),
        play: |e, out| err(unsafe { gme_play(emu(e), out.len() as c_int, out.as_mut_ptr()) }),
        track_ended: |e| unsafe { gme_track_ended(emu(e)) != 0 },
        tell: |e| unsafe { gme_tell(emu(e)) },
        seek: |e, msec| err(unsafe { gme_seek(emu(e), msec) }),
        set_tempo: |e, tempo| unsafe { gme_set_tempo(emu(e), tempo) },
        set_fade: |e, start| unsafe { gme_set_fade(emu(e), start) },
        set_stereo_depth: |e, depth| unsafe { gme_set_stereo_depth(emu(e), depth) },
        track_count: |e| unsafe { gme_track_count(emu(e)) },
        track_info,
        ignore_silence: |e, ignore| unsafe { gme_ignore_silence(emu(e), ignore as c_int) },
        voice_count: |e| unsafe { gme_voice_count(emu(e)) },
        mute_voice: |e, index, mute| unsafe { gme_mute_voice(emu(e), index, mute as c_int) },
        mute_voices: |e, mask| unsafe { gme_mute_voices(emu(e), mask) },
        warning: |e| err(unsafe { gme_warning(emu(e)) }),
    });
    flashnx_gme::set_enabled(enabled);
}
