//! flash-for-switch — Ruffle Flash player port to Nintendo Switch.
//!
//! Phase 0:    glClear red.
//! Phase 0.5:  GLSL triangle.
//! Phase 1.1:  stdlib via -Z build-std + 2 patches.
//! Phase 1.2:  ruffle_core links.
//! Phase 1.3:  full SwitchRenderBackend (shapes, bitmaps, lines, gradients,
//!             color transforms, masking).
//! Phase 1.4:  wire SwitchLogBackend and default Null backends for nav/ui/
//!             storage/audio/video into PlayerBuilder.
//! Phase 1.5:  build a real Player, tick + render each frame, and try to
//!             load `sdmc:/switch/ruffle/test.swf` if present.

#![feature(restricted_std)]

/// Counting global allocator (2026-08-25, periodic-spike hunt).
///
/// The GC phase probe showed Mario 63's castle spending ~48% of its wall clock
/// inside gc-arena's SWEEP, sweeping about 10 MB per cycle at an implausible
/// ~6 MB/s. A linear traversal cannot be that slow, so the suspicion is what
/// the traversal *calls*: `free()`. Nothing here defines an allocator, so Rust
/// hands every allocation to newlib's malloc, and newlib's is not fast.
///
/// This wrapper only counts — no clock is read per call, because two FFI reads
/// on every dealloc would cost more than the thing being measured. Pair the
/// per-frame dealloc count with the collector timing (`gcUs`) and the answer
/// falls out: 100k frees for 300 ms means ~3 us per free, which is newlib's
/// problem and therefore ours to fix; a low count means the cost is inside
/// gc-arena's own traversal, which is upstream's.
mod counting_alloc {
    use std::alloc::{GlobalAlloc, Layout, System};
    use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

    pub static ALLOC_N: AtomicU64 = AtomicU64::new(0);
    pub static DEALLOC_N: AtomicU64 = AtomicU64::new(0);
    pub static ALLOC_TICKS: AtomicU64 = AtomicU64::new(0);
    pub static DEALLOC_TICKS: AtomicU64 = AtomicU64::new(0);
    /// Allocations served from the region rather than newlib.
    pub static SMALL_N: AtomicU64 = AtomicU64::new(0);

    /// The ARM generic timer, read inline. Deliberately NOT `ruffle_tick_now()`:
    /// that is an FFI call, and paying one on both sides of every allocation
    /// would cost more than the allocation being measured.
    ///
    /// It must be `cntpct_el0`, the PHYSICAL counter — exactly what libnx's
    /// `armGetSystemTick` reads. Horizon traps `cntvct_el0` from EL0: reading
    /// it aborts on the very first Rust allocation.
    ///
    /// No `nomem`/`pure`: this must stay a real side-effecting read so the two
    /// calls around an allocation cannot be merged into one.
    #[inline(always)]
    #[cfg(feature = "instr")]
    fn now() -> u64 {
        let t: u64;
        unsafe { core::arch::asm!("mrs {}, cntpct_el0", out(reg) t, options(nostack)) };
        t
    }

    /// Without the `instr` feature the clock is never read and every tally
    /// below compiles away, so a release build pays nothing per allocation.
    /// That matters: at ~50 000 allocations per frame, two counter reads and
    /// three atomics each were measured at 1 to 3 % of the frame.
    #[cfg(not(feature = "instr"))]
    #[inline(always)]
    fn now() -> u64 {
        0
    }

    /// Add to a diagnostic counter, or nothing at all in a release build.
    #[inline(always)]
    fn tally(c: &AtomicU64, v: u64) {
        #[cfg(feature = "instr")]
        c.fetch_add(v, Ordering::Relaxed);
        #[cfg(not(feature = "instr"))]
        let _ = (c, v);
    }

    // ─── Small-object cache in front of newlib ────────────────────────────
    //
    // Why: measured 2026-08-25, Mario 63's castle does 48 870 mallocs and
    // 43 705 frees PER FRAME, costing 63.7 ms of a 110 ms frame — 58% of the
    // wall clock. newlib's `free()` degrades from 0.13 us at rest to 1.22 us
    // during a GC sweep at identical call counts: dlmalloc coalescing walking
    // a fragmented free list. Caching small blocks took the castle from 9.1 to
    // 13.1 fps and removed all 119 frames over 150 ms.
    //
    // ONE fixed region, reserved once, carved with a bump pointer, with a
    // per-class free list on top. Blocks that do not fit in the region go to
    // newlib exactly as before.
    //
    // Two earlier designs failed, and both failures are the reason this one
    // looks the way it does:
    //
    //  * A global bump region that never released anything held 279 MB after
    //    Super Smash Flash 2, and still held it while a later, lighter game
    //    ran. Retention has to be bounded.
    //
    //  * Per-slab release with 64 KB slabs ALIGNED to 64 KB fixed retention
    //    but turned a 256-byte request into a 64 KB aligned request. On SSF2,
    //    which legitimately sits at 2.7 GB, newlib could no longer satisfy
    //    that while a plain malloc(256) still could — and the code then
    //    aborted instead of degrading. It crashed on entering a fight.
    //
    // Hence: reserve once, at the first small allocation, while memory is
    // still plentiful, and NEVER ask the system for a large block again. The
    // region is bounded, so retention is bounded by construction, with no list
    // surgery and no per-slab bookkeeping.
    //
    // The invariant that makes it safe is a RANGE TEST, not the layout:
    // `dealloc` asks "is this pointer inside the region?". A block served by
    // newlib because the region was full is recognised as newlib's and handed
    // back to newlib. That is what lets the fallback exist at all — the
    // previous design could not fall back without corrupting memory.

    const GRAN: usize = 16;
    const CLASSES: usize = 16;
    const MAX_SMALL: usize = GRAN * CLASSES; // 256
    /// The cache GROWS. It reserves nothing up front and takes another chunk
    /// only when the previous one is used up.
    ///
    /// A fixed 32 MB region was tried and it was a regression, measured on
    /// Super Smash Flash 2: capped, the region saturated at once, the hit rate
    /// collapsed from 93% to 12%, `malloc` went from 0.16 to 0.43 us, and the
    /// heap share of the frame doubled. The game then died at 2894 MB where an
    /// uncapped cache had carried it to 3038 MB.
    ///
    /// The lesson is that those megabytes are not overhead. They are memory
    /// the game needs for its small objects either way; serving them from a
    /// pool instead of newlib is also what stops 850 000 allocations per frame
    /// from churning newlib's free list into confetti. Starving the cache
    /// makes the heap WORSE, not better.
    ///
    /// So: grow on demand, pay nothing when unused, and stop at a ceiling that
    /// is generous rather than cautious.
    const CHUNK: usize = 32 * 1024 * 1024;
    const MAX_CHUNKS: usize = 16; // 512 MB ceiling

    /// Size class for a layout, or `None` when newlib should handle it.
    /// Alignment above 16 goes to `System`: region blocks are only 16-aligned.
    #[inline(always)]
    fn class_of(l: Layout) -> Option<usize> {
        if l.align() > GRAN || l.size() == 0 || l.size() > MAX_SMALL {
            return None;
        }
        Some((l.size() + GRAN - 1) / GRAN - 1)
    }

    static LOCK: AtomicBool = AtomicBool::new(false);
    static HEADS: [AtomicUsize; CLASSES] = [const { AtomicUsize::new(0) }; CLASSES];
    /// The chunks we own, appended under LOCK and never moved or released.
    /// `NCHUNKS` is published with Release AFTER its bounds are stored, and
    /// read with Acquire, so a concurrent `dealloc` can never see a chunk
    /// count that outruns the bounds it would read.
    static CH_BASE: [AtomicUsize; MAX_CHUNKS] = [const { AtomicUsize::new(0) }; MAX_CHUNKS];
    static CH_END: [AtomicUsize; MAX_CHUNKS] = [const { AtomicUsize::new(0) }; MAX_CHUNKS];
    static NCHUNKS: AtomicUsize = AtomicUsize::new(0);
    /// Bump pointer inside the newest chunk.
    static BUMP: AtomicUsize = AtomicUsize::new(0);
    static BUMP_END: AtomicUsize = AtomicUsize::new(0);
    /// Bytes handed out of the region and not returned, reported as `slabMB`.
    pub static SLAB_BYTES: AtomicU64 = AtomicU64::new(0);
    /// Kill switch, set from C++ at boot when `sdmc:/switch/FlashNX/noalloc.on`
    /// exists — before the worker thread starts, so before any Rust allocation.
    /// With it on, the region is never reserved and every block goes to newlib:
    /// byte for byte the behaviour that predates this module, for A/B testing a
    /// game against it. Safe to flip at any time in principle, because
    /// `dealloc` decides by ADDRESS: blocks already carved keep being returned
    /// to the region, blocks served afterwards go back to newlib.
    pub static FORCE_OFF: AtomicBool = AtomicBool::new(false);
    /// Set once the region has failed to grow, so the diagnostic is printed a
    /// single time and also stays readable from a bug report.
    pub static GREW_FAILED: AtomicBool = AtomicBool::new(false);

    #[inline(always)]
    fn lock() {
        while LOCK
            .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            core::hint::spin_loop();
        }
    }
    #[inline(always)]
    fn unlock() {
        LOCK.store(false, Ordering::Release);
    }

    /// Take another chunk. Called under LOCK when the bump pointer runs out.
    /// Returns false when the ceiling is reached, the kill switch is on, or
    /// the system refuses — in every case the caller falls through to newlib,
    /// which is exactly the behaviour that predates this module.
    fn grow() -> bool {
        if FORCE_OFF.load(Ordering::Relaxed) {
            return false;
        }
        let n = NCHUNKS.load(Ordering::Relaxed);
        if n >= MAX_CHUNKS {
            return false;
        }
        let p = unsafe { System.alloc(Layout::from_size_align_unchecked(CHUNK, GRAN)) };
        if p.is_null() {
            // Loudly, once. This is the cache's one silent failure mode and it
            // is expensive: measured on Super Smash Flash 2, the fight-loading
            // frame served 96% of its 853 000 allocations from the region when
            // a chunk was available and 15% when one was not, which is a 16x
            // difference in allocator cost on the same work — and the run that
            // could not grow is the run that died. Without this line the only
            // symptom is a framerate that collapses for no visible reason.
            //
            // Note what it means: the system could not find 32 MB CONTIGUOUS,
            // not that it is out of memory. A run has died on a 256-byte
            // request at 2761 MB while another lived at 3038.
            // Raise a flag and nothing else. Logging here would be a deadlock:
            // `grow` runs while holding LOCK, and the log helper allocates, so
            // it would re-enter `alloc` and spin on the lock it already holds.
            // The heartbeat prints it, outside the lock, with the size.
            GREW_FAILED.store(true, Ordering::Relaxed);
            return false;
        }
        let base = p as usize;
        CH_BASE[n].store(base, Ordering::Relaxed);
        CH_END[n].store(base + CHUNK, Ordering::Relaxed);
        // Bounds first, count last: see the note on NCHUNKS.
        NCHUNKS.store(n + 1, Ordering::Release);
        BUMP.store(base, Ordering::Relaxed);
        BUMP_END.store(base + CHUNK, Ordering::Relaxed);
        true
    }

    /// True when `p` was carved from one of our chunks. Scans newest first,
    /// because the newest chunk is where the live blocks are concentrated.
    /// At most `MAX_CHUNKS` pairs of comparisons on data that stays in L1, and
    /// it does not care what the layout says — which is what lets blocks that
    /// newlib served (ceiling reached) be handed back to newlib correctly.
    #[inline(always)]
    fn in_region(p: *mut u8) -> bool {
        let a = p as usize;
        let n = NCHUNKS.load(Ordering::Acquire);
        let mut i = n;
        while i > 0 {
            i -= 1;
            if a >= CH_BASE[i].load(Ordering::Relaxed) && a < CH_END[i].load(Ordering::Relaxed) {
                return true;
            }
        }
        false
    }

    pub struct Counting;

    unsafe impl GlobalAlloc for Counting {
        unsafe fn alloc(&self, l: Layout) -> *mut u8 {
            let t0 = now();
            let p = match class_of(l) {
                Some(c) => {
                    lock();
                    let head = HEADS[c].load(Ordering::Relaxed) as *mut u8;
                    let p = if !head.is_null() {
                        // Pop. The freed block's first 8 bytes hold the link.
                        let next = unsafe { *(head as *mut *mut u8) };
                        HEADS[c].store(next as usize, Ordering::Relaxed);
                        tally(&SMALL_N, 1);
                        head
                    } else {
                        let want = (c + 1) * GRAN;
                        let mut b = BUMP.load(Ordering::Relaxed);
                        if b == 0 || b + want > BUMP_END.load(Ordering::Relaxed) {
                            // Current chunk exhausted (or none yet).
                            if grow() {
                                b = BUMP.load(Ordering::Relaxed);
                            } else {
                                b = 0;
                            }
                        }
                        if b != 0 {
                            BUMP.store(b + want, Ordering::Relaxed);
                            SLAB_BYTES.fetch_add(want as u64, Ordering::Relaxed);
                            tally(&SMALL_N, 1);
                            b as *mut u8
                        } else {
                            // Ceiling reached, or the system refused a chunk.
                            // Newlib serves it; `dealloc`'s range test will
                            // send it back to newlib. No abort, no corruption.
                            core::ptr::null_mut()
                        }
                    };
                    unlock();
                    if p.is_null() {
                        unsafe { System.alloc(l) }
                    } else {
                        p
                    }
                }
                None => unsafe { System.alloc(l) },
            };
            tally(&ALLOC_TICKS, now().wrapping_sub(t0));
            tally(&ALLOC_N, 1);
            p
        }

        unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
            let t0 = now();
            if in_region(p) {
                // Only region blocks come back here, and they were allocated
                // with this same layout, so the class is the one they were
                // carved for.
                let c = match class_of(l) {
                    Some(c) => c,
                    // Cannot happen: nothing outside a small class is ever
                    // carved from the region. Leak the block rather than
                    // corrupt a list if it somehow does.
                    None => {
                        tally(&DEALLOC_TICKS, now().wrapping_sub(t0));
                        tally(&DEALLOC_N, 1);
                        return;
                    }
                };
                lock();
                unsafe { *(p as *mut *mut u8) = HEADS[c].load(Ordering::Relaxed) as *mut u8 };
                HEADS[c].store(p as usize, Ordering::Relaxed);
                unlock();
            } else {
                unsafe { System.dealloc(p, l) };
            }
            tally(&DEALLOC_TICKS, now().wrapping_sub(t0));
            tally(&DEALLOC_N, 1);
        }

        unsafe fn realloc(&self, p: *mut u8, l: Layout, n: usize) -> *mut u8 {
            let new_l = unsafe { Layout::from_size_align_unchecked(n, l.align()) };
            // `System.realloc` is only valid when BOTH ends are newlib's. The
            // old end is decided by where the pointer lives, the new one by
            // whether it could be served from the region at all.
            if !in_region(p) && class_of(new_l).is_none() {
                let t0 = now();
                let q = unsafe { System.realloc(p, l, n) };
                tally(&ALLOC_TICKS, now().wrapping_sub(t0));
                tally(&ALLOC_N, 1);
                q
            } else {
                // No counters here: alloc/dealloc below tally themselves.
                let q = unsafe { self.alloc(new_l) };
                if !q.is_null() {
                    unsafe {
                        core::ptr::copy_nonoverlapping(p, q, core::cmp::min(l.size(), n));
                        self.dealloc(p, l);
                    }
                }
                q
            }
        }
    }
}

#[global_allocator]
static GLOBAL: counting_alloc::Counting = counting_alloc::Counting;

/// Total bytes held in slabs by the small-object cache.
pub(crate) fn slab_bytes() -> u64 {
    counting_alloc::SLAB_BYTES.load(std::sync::atomic::Ordering::Relaxed)
}

/// True once the small-object region has failed to obtain another 32 MB chunk.
///
/// This is NOT "out of memory" on its own: it means no CONTIGUOUS 32 MB was
/// available. A run has died on a 256-byte request at 2761 MB of heap while
/// another lived at 3038, so the wall is the largest usable block and not the
/// total.
///
/// It matters because the fallback is expensive and otherwise invisible.
/// Measured on Super Smash Flash 2's fight-loading frame: 96% of its 853 000
/// allocations served from the region when a chunk was available, 15% when it
/// was not, i.e. sixteen times the allocator cost on identical work — and the
/// run that could not grow is the run that died. Before this flag existed the
/// only symptom was a framerate collapsing for no stated reason.
pub(crate) fn region_grow_failed() -> bool {
    counting_alloc::GREW_FAILED.load(std::sync::atomic::Ordering::Relaxed)
}

/// Allocations, frees, and the system ticks spent inside each, since process
/// start. The `SLOW` line publishes the per-frame deltas.
pub(crate) fn alloc_counters() -> (u64, u64, u64, u64, u64) {
    use std::sync::atomic::Ordering;
    (
        counting_alloc::ALLOC_N.load(Ordering::Relaxed),
        counting_alloc::DEALLOC_N.load(Ordering::Relaxed),
        counting_alloc::ALLOC_TICKS.load(Ordering::Relaxed),
        counting_alloc::DEALLOC_TICKS.load(Ordering::Relaxed),
        counting_alloc::SMALL_N.load(Ordering::Relaxed),
    )
}

mod backend;
mod bugreport;
mod covers;
mod favorites;
mod ffi;
mod keymap;
mod library;
mod loc;
mod menu;
mod net;
mod player;
mod playtime;
mod profiles;
mod sd;
mod sources;
mod tags;

use core::ffi::{c_char, c_int};
use std::sync::{Arc, Mutex};

use ruffle_core::backend::navigator::NullExecutor;
use ruffle_core::events::{
    KeyDescriptor, KeyLocation, LogicalKey, MouseButton, NamedKey, PhysicalKey, PlayerEvent,
    TextControlCode,
};
use ruffle_core::tag_utils::SwfMovie;
use ruffle_core::config::Letterbox;
use ruffle_core::{FloatDuration, Player, PlayerBuilder, StageAlign, StageScaleMode};
use ruffle_core::external::{ExternalInterfaceProvider, Value as ExtValue};
use ruffle_core::context::UpdateContext;
use ruffle_render::backend::RenderBackend;

use backend::audio::SwitchAudioBackend;
use backend::log::SwitchLogBackend;
use backend::navigator::SidecarNavigator;
use backend::render::SwitchRenderBackend;
use backend::storage::SwitchStorageBackend;
use backend::tracing::SwitchTracingSubscriber;
use backend::ui::SwitchUiBackend;

extern "C" {
    fn ruffle_log_cstr(msg: *const c_char);
    fn ruffle_query_ram(used_out: *mut u64, total_out: *mut u64) -> c_int;
    /// Writes `msg` to `sdmc:/switch/ruffle-crash.log` AND nxlink stdout, then
    /// sleeps ~150 ms so the TCP buffer drains before abort() races us. Used
    /// only from the panic hook.
    fn ruffle_crash_dump(msg: *const c_char);
    /// Monotonic system tick counter (armGetSystemTick). Used to profile
    /// tick() vs render() time per frame.
    fn ruffle_tick_now() -> u64;
    /// System tick frequency in Hz (~19.2 MHz on Switch).
    fn ruffle_tick_freq() -> u64;
}

/// Per-frame tick/render time accumulators. Cleared by the render backend's
/// heartbeat code once per 60 frames. Stored as system-tick counts (not
/// ns/us) so we don't lose precision on each frame's addition.
pub(crate) static TICK_TICKS_ACCUM: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);
pub(crate) static RENDER_TICKS_ACCUM: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);
/// Worst single-frame tick / render time within the current heartbeat window
/// (system-tick counts). A periodic 1-frame stall (e.g. an HUD text updating
/// once/sec) is invisible in the window AVERAGE but shows up here as a spike.
pub(crate) static TICK_TICKS_MAX: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);
pub(crate) static RENDER_TICKS_MAX: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);


/// Snapshot of process RAM in bytes (used, total). Returns (0,0) if the
/// underlying svcGetInfo call fails.
pub(crate) fn query_ram() -> (u64, u64) {
    let mut used = 0u64;
    let mut total = 0u64;
    let rc = unsafe { ruffle_query_ram(&mut used as *mut _, &mut total as *mut _) };
    if rc == 0 { (used, total) } else { (0, 0) }
}

struct State {
    player: Arc<Mutex<Player>>,
    /// Drives loader futures spawned by the SidecarNavigator (multi-file games:
    /// loadMovie / GetURL-into-_levelN). Pumped once per frame in
    /// `render_frame_with_dt`, AFTER the player lock is dropped — the futures
    /// re-lock the player to install the loaded movie, so running them under
    /// our own guard would deadlock. The navigator holds a spawner into this
    /// pool; an empty pool's `run()` is a cheap no-op.
    executor: NullExecutor,
    /// Last reported cursor position in screen pixels. We track it so we can
    /// (a) overlay a visible crosshair after `Player::render()`, and (b) send
    /// it as the click position when `ruffle_handle_mouse_button` fires
    /// without a preceding move (e.g. touch tap).
    cursor_x: f32,
    cursor_y: f32,
    /// The STAGE point under that pointer, which is a different thing as soon as
    /// the free zoom is on (issue #101): the pair above says where to DRAW the
    /// crosshair, this one says what it is pointing AT. Equal at 100%.
    ///
    /// Two fields rather than one because the pointer lives in screen space --
    /// the stick moves it in physical pixels -- while the game only ever hears
    /// about stage coordinates. Collapsing them would either park the crosshair
    /// somewhere the stick did not put it, or hand the game a click from a place
    /// nothing was clicked.
    cursor_stage_x: f32,
    cursor_stage_y: f32,
    /// Last reported mouse-button state (left only for now). Used purely to
    /// tint the cursor overlay so the user gets feedback on click.
    cursor_clicked: bool,
}

static mut STATE: Option<State> = None;

/// INTERNAL render resolutions — MUST match UI_VIEWPORT_* / GAME_VIEWPORT_* in
/// cpp/src/main.cpp, which resizes the window surface at each transition
/// (`gl_context_resize`) so the display scaler upscales to the panel.
///
/// The UI renders at panel size (it is cheap, and it is text and thin lines where
/// upscaling looks bad). GAMES render lower: they are the real load, and the heavy
/// ones are fill-bound, so shrinking the surface shrinks the main pass AND every
/// full-stage offscreen temp — blend groups, alpha masks, cacheAsBitmap. See the
/// long comment at the C++ constants for the measurements.
const UI_VIEWPORT_W: u32 = 1280;
const UI_VIEWPORT_H: u32 = 720;
// Game viewport. Currently equal to the UI/panel size — see the long comment at
// GAME_VIEWPORT_* in cpp/src/main.cpp for why a global reduction was reverted.
const VIEWPORT_W: u32 = 1280;
const VIEWPORT_H: u32 = 720;

/// Candidate paths tried in order. We use a hardcoded list because
/// `std::fs::read_dir` on Horizon corrupts entry names — observed
/// 2026-05-24 on nightly stdlib: a 23-char filename came back missing its
/// first 2 bytes. Suspected dirent struct-layout mismatch between Rust's
/// Unix `dirent` model and devkitPro's newlib (alignment of d_reclen/d_type
/// vs d_name on aarch64). Until 1.5.c writes a libnx-direct file picker
/// (which avoids stdlib's dir reading entirely), we look for known names.
const SWF_CANDIDATES: &[&str] = &[
    "sdmc:/flashnx/test.swf",
    "sdmc:/flashnx/mario.swf",
    "sdmc:/ruffle/test.swf",
    "sdmc:/ruffle/mario.swf",
    "sdmc:/ruffle/Super_Mario_63_2010.swf",
    "sdmc:/switch/flashnx/test.swf",
    "sdmc:/switch/ruffle/test.swf",
];

/// Path supplied at runtime by `ruffle_set_swf_path` — the library UI calls
/// it with the path the user picked. Mutex<Option<String>> rather than
/// OnceLock so the user can come back to the library (via the pause menu's
/// QUITTER entry now wired to "back to library" not "exit .nro") and pick
/// a different game — second set replaces first.
static OVERRIDE_SWF_PATH: Mutex<Option<std::string::String>> = Mutex::new(None);

/// The real SD path of the SWF most recently loaded by
/// `find_and_load_swf_uncached` (override or candidate). `ensure_swf_loaded`
/// only keeps the synthetic `http://flashforswitch.local/<basename>` URL, but
/// the sidecar NavigatorBackend needs the actual on-disk directory to find a
/// multi-file game's sibling SWFs. Set on each successful read.
static LAST_SWF_REAL_PATH: Mutex<Option<std::string::String>> = Mutex::new(None);

/// On-SD path of the SWF currently loaded (the actual file, not the movie URL).
/// Lets the in-game profiles sub-menu (#20 Option 1) hash the running game for
/// catalog matching without a library entry. None before the first load.
pub(crate) fn last_swf_real_path() -> Option<std::string::String> {
    LAST_SWF_REAL_PATH.lock().ok().and_then(|g| g.clone())
}

/// Raw SWF bytes + synthesized URL. Populated by the first successful
/// `find_and_load_swf` and reused on every subsequent `ruffle_init` for the
/// SAME game (pause-menu REDEMARRER path) to avoid re-reading 15 MB from
/// SD after newlib heap fragments.
///
/// **Why the cache exists**: Mario 63 is 15.3 MB. The first `std::fs::read`
/// succeeds on a fresh heap, but after several minutes of play the heap
/// fragments (gc_arena does a lot of small allocs that scatter free
/// blocks). When we drop the Player on restart, the big SwfMovie chunk
/// frees but the heap can't satisfy another 15+ MB contiguous request —
/// `read_to_end` reports `OutOfMemory`. Caching avoids re-reading.
///
/// **Why Mutex<Option<>> not OnceLock**: back-to-library can pick a
/// DIFFERENT game from last session. We `ruffle_library_reset` the cache
/// before re-scanning so the new pick gets read fresh from SD. (Same-game
/// REDEMARRER still hits the cache because we DON'T reset between
/// REDEMARRER cycles — only between back-to-library cycles.)
static CACHED_SWF: Mutex<Option<(std::vec::Vec<u8>, std::string::String)>> =
    Mutex::new(None);

/// Called from C++ (cpp/src/swf_picker.cpp) before `ruffle_init`. Copies the
/// path into a Rust-owned String. Idempotent — only the first call sticks.
/// Returns 0 on success, non-zero on malformed input.
#[no_mangle]
pub extern "C" fn ruffle_set_swf_path(path: *const c_char) -> c_int {
    if path.is_null() {
        return -1;
    }
    // SAFETY: caller (swf_picker.cpp) passes a NUL-terminated C string. We
    // copy immediately so the caller's buffer can be freed.
    let s = unsafe { core::ffi::CStr::from_ptr(path) };
    let Ok(string) = s.to_str() else {
        return -2;
    };
    if let Ok(mut g) = OVERRIDE_SWF_PATH.lock() {
        *g = Some(std::string::String::from(string));
    }
    // Forwarder launch (main.cpp passes a `.swf` as argv) skips the library UI,
    // so `note_played` never runs and the pause modal's game-name subtitle would
    // be blank. If nothing has set the active game yet, derive it from this path
    // so the subtitle shows the right title. No-op on a normal library launch —
    // the library already set the active game before calling here.
    if crate::library::active_display_name().is_none() {
        crate::library::note_played_from_path(string);
    }
    0
}

/// Embedded fallback: a 43-byte SWF that just sets a red stage background.
/// Pulled from the upstream ruffle tree as a known-good reproducible target
/// when no `.swf` is found on the SD card.
const EMBEDDED_FALLBACK_SWF: &[u8] =
    include_bytes!("../../third_party/ruffle/swf/tests/swfs/SimpleRedBackground.swf");

/// Format an ExternalInterface `Value` compactly for logging (so a hardware
/// capture shows the exact Flash->JS call contract a game expects).
fn fmt_ext_value(v: &ExtValue) -> std::string::String {
    match v {
        ExtValue::Undefined => std::string::String::from("undefined"),
        ExtValue::Null => std::string::String::from("null"),
        ExtValue::Bool(b) => std::format!("{}", b),
        ExtValue::Number(n) => std::format!("{}", n),
        ExtValue::String(s) => std::format!("{:?}", s),
        ExtValue::Object(m) => {
            let inner: std::vec::Vec<std::string::String> =
                m.iter().map(|(k, val)| std::format!("{}:{}", k, fmt_ext_value(val))).collect();
            std::format!("{{{}}}", inner.join(","))
        }
        ExtValue::List(l) => {
            let inner: std::vec::Vec<std::string::String> = l.iter().map(fmt_ext_value).collect();
            std::format!("[{}]", inner.join(","))
        }
    }
}

/// An ExternalInterface provider that emulates the browser "container" a Flash
/// game expects on the other side of `ExternalInterface.call(...)`. Wiring ANY
/// provider makes `ExternalInterface.available` return true — which is itself
/// load-bearing: Disney/Yamago minigames (Agent P Strikes Back, Gravity Falls…)
/// take a DIFFERENT init path when they think they're inside their JS container
/// (`disneygames-iframe.js`) vs. standalone (where they fall back to an
/// unpublished dev API and dead-end on a blue stage).
///
/// For now this LOGS every call + every callback the game registers (the exact
/// contract we need to emulate) and returns best-effort benign values so the
/// game's API is more likely to init offline: a Disney URL for domain/site-lock
/// queries, `true` for readiness/registration probes, `null` otherwise. Once a
/// hardware capture shows the real message protocol, the responses here become
/// the actual container emulation (config, site-lock OK, LSO-backed saves).
struct ContainerInterface;

/// Callbacks the movie has registered for its container to call, lowercased.
static EI_CALLBACKS: Mutex<std::vec::Vec<std::string::String>> = Mutex::new(std::vec::Vec::new());
/// Container callbacks waiting to be made on the next frame, with the argument
/// the page would have passed (see `call_method`).
static EI_PENDING: Mutex<std::vec::Vec<(std::string::String, Option<std::string::String>)>> =
    Mutex::new(std::vec::Vec::new());

/// Queue a container callback, if the movie registered one by that name.
fn queue_container_callback(wanted: &str, arg: Option<&str>) {
    let known: std::vec::Vec<std::string::String> =
        EI_CALLBACKS.lock().map(|s| s.clone()).unwrap_or_default();
    // Match without case, call with the spelling the movie registered: Ruffle
    // looks these up case-sensitively.
    let Some(actual) = known.iter().find(|k| k.eq_ignore_ascii_case(wanted)) else {
        return;
    };
    if let Ok(mut q) = EI_PENDING.lock() {
        q.push((actual.clone(), arg.map(std::string::String::from)));
    }
}

impl ExternalInterfaceProvider for ContainerInterface {
    fn call_method(&self, _context: &mut UpdateContext<'_>, name: &str, args: &[ExtValue]) -> ExtValue {
        let args_str: std::vec::Vec<std::string::String> = args.iter().map(fmt_ext_value).collect();
        log_str(&std::format!("EI call: {}({})\n", name, args_str.join(", ")));
        let lname = name.to_ascii_lowercase();
        // gaforflash (Google Analytics for Flash) and similar libraries call
        // `ExternalInterface.call(<a raw JS <script> blob>)`, expecting the browser
        // to EVAL it and hand back a JS OBJECT (e.g. `{host, language, …}`) they then
        // read `.host` on. We can't run JS; the scalar-String answers below would make
        // them do `.host` on a String → AVM2 #1069, aborting the movie's construction
        // on a black stage (Pursuit of Hat 2, whose Preloader spins up a GATracker at
        // init). A real absent container makes `ExternalInterface.call` return null,
        // which these libs treat as "JS unavailable" and skip. A genuine container
        // query (the Disney message bus / site-lock) is always a short method name,
        // never a `<script>` blob — so bail to Null the moment the "name" is JS code.
        if lname.contains("<script") || name.contains('\n') {
            return ExtValue::Null;
        }
        // Disney minigame message bus: `disneyGamesSendMessage(<type>, …)`, where
        // the message type is the first arg and the game reads the return value
        // synchronously. Without a container answer the game reads volume as 0 and
        // renders muted (in-game volume toggle stuck off). Answer the audio probes.
        if lname.contains("sendmessage") {
            if let Some(ExtValue::String(msg)) = args.first() {
                let m = msg.to_ascii_lowercase();
                if m.contains("getvolume") {
                    return ExtValue::Number(1.0); // full (SoundTransform.volume is 0..1)
                }
                if m.contains("mute") {
                    return ExtValue::Bool(false); // not muted
                }
                if m.contains("pause") {
                    return ExtValue::Bool(false);
                }
            }
            return ExtValue::Null;
        }
        // Portal games (PopCap's Peggle, #100) do not start themselves: the SWF
        // tells the hosting page it is ready, and the page's JavaScript then
        // calls BACK into the movie — `onSessionStart`, then `onGameStart` — to
        // take it off its loading screen. With no page, the game finishes
        // loading, parks its content off-stage (measured: `clip_game` at x=542
        // for a 542-wide stage) and waits for ever behind a full-screen
        // preloader. Nobody had ever played the other half of this dialogue.
        //
        // Queued rather than called here: we are inside a call FROM the movie,
        // and re-entering ActionScript from an ExternalInterface handler is a
        // good way to corrupt an AVM already in flight. The frame loop drains it.
        // The dialogue runs in two beats, and each answer belongs to its own.
        // Answering both at the first beat is what threw #1006 in the bundled ad
        // API: `onGameStart` was called before the game had a level to start.
        if lname == "setswfisready" || lname == "swfisready" {
            queue_container_callback("onSessionStart", None);
        }
        // "Level built, mode 0, start level 0" — the page replies by starting the
        // game. No argument: the movie's own calls carry XML payloads, but the
        // handler on the other side takes none, and says so
        // (ArgumentError #1063, "Expected 0, got 1").
        if lname == "gameready" {
            queue_container_callback("onGameStart", None);
        }
        if lname.contains("domain")
            || lname.contains("location")
            || lname.contains("url")
            || lname.contains("referrer")
            || lname.contains("host")
        {
            ExtValue::String(std::string::String::from("http://play.lol.disney.com/"))
        } else if lname.contains("available")
            || lname.contains("isready")
            || lname.contains("ready")
            || lname.contains("init")
            || lname.contains("register")
        {
            ExtValue::Bool(true)
        } else {
            ExtValue::Null
        }
    }

    fn on_callback_available(&self, name: &str) {
        // Callbacks the game exposes TO the container (ExternalInterface.addCallback).
        // Logging these reveals the JS->Flash half of the bridge.
        log_str(&std::format!("EI callback registered by game: {}\n", name));
        // Kept VERBATIM: Ruffle looks callbacks up case-sensitively, so the name
        // we call back with has to be the one the movie registered — `onGameStart`,
        // not `ongamestart`, which it answers with "unknown internal interface".
        if let Ok(mut seen) = EI_CALLBACKS.lock() {
            seen.push(std::string::String::from(name));
        }
    }

    fn get_id(&self) -> Option<std::string::String> {
        None
    }
}

#[no_mangle]
pub extern "C" fn ruffle_init() -> c_int {
    // Pipe panics through nxlink so we don't die silently. `panic = "abort"`
    // means the hook fires once, then we're done — but at least the message
    // makes it out. We snapshot RAM at the moment of the crash too: lets us
    // distinguish a logic bug (Mario 63 unimplemented filter etc) from a
    // genuine OOM kill where the headroom collapsed in the last few frames.
    std::panic::set_hook(std::boxed::Box::new(|info| {
        let (used, total) = query_ram();
        let location = info
            .location()
            .map(|l| std::format!("{}:{}:{}", l.file(), l.line(), l.column()))
            .unwrap_or_else(|| std::string::String::from("<unknown>"));
        // Try to recover a string message (str or String); ignore anything
        // else (e.g. a custom panic payload).
        let payload = info.payload();
        let payload_msg = if let Some(s) = payload.downcast_ref::<&'static str>() {
            *s
        } else if let Some(s) = payload.downcast_ref::<std::string::String>() {
            s.as_str()
        } else {
            "<non-string panic payload>"
        };
        let free_mb = total.saturating_sub(used) / (1024 * 1024);
        let used_mb = used / (1024 * 1024);
        let total_mb = total / (1024 * 1024);
        let msg = std::format!(
            "\n=== PANIC ===\nat {}\nmsg: {}\nram: used={}MB total={}MB free={}MB\n=============\n",
            location, payload_msg, used_mb, total_mb, free_mb,
        );
        let mut bytes = msg.into_bytes();
        bytes.push(0);
        // Use crash_dump (file + stdout + 150 ms sleep) so the message
        // survives the imminent abort() — plain ruffle_log_cstr previously
        // got swallowed by the kernel TCP buffer when the process died
        // before nxlink finished sending.
        unsafe { ruffle_crash_dump(bytes.as_ptr() as *const c_char) };
        // Hand the CPU clock back before the abort. `panic = "abort"` means
        // nothing else downstream will get the chance, and closing a clkrst
        // session demonstrably does not release the rate on its own.
        crate::backend::render::apply_power_mode(0);
    }));

    log_str(&std::format!("phase 1.5: ruffle_init starting\n"));

    let _ = tracing::subscriber::set_global_default(SwitchTracingSubscriber::new());
    log(b"ruffle_init: tracing subscriber installed (INFO level)\n\0");
    // Say it out loud, because otherwise the zeros look like measurements.
    #[cfg(not(feature = "instr"))]
    log(b"alloc: instrumentation compiled out, alloc=/free=/%sm read zero\n\0");

    // Before the renderer, because the viewport below is chosen from it.
    crate::backend::render::set_game_rotation(pending_rotation());
    // The framing this game was left with. Re-clamped by `set_game_zoom`, which
    // is what keeps a framing saved at 400% from throwing the picture off the
    // screen if the percentage were ever read back smaller.
    {
        let (z, ox, oy) = pending_zoom();
        crate::backend::render::set_game_zoom(z, ox, oy, VIEWPORT_W as f32, VIEWPORT_H as f32);
    }
    let renderer = match SwitchRenderBackend::new(VIEWPORT_W, VIEWPORT_H) {
        Some(r) => r,
        None => {
            log(b"ruffle_init: SwitchRenderBackend::new failed\n\0");
            return -1;
        }
    };
    log(b"ruffle_init: renderer constructed\n\0");

    // SharedObject persistence — flat layout next to the .swf files
    // (Phase 3.4 / 2026-05-26 nuit revision). New saves go to
    // `sdmc:/flashnx/<basename>.<sol_name>.sol`; legacy saves under
    // `sdmc:/ruffle/saves/<host>/<basename>/<sol_name>.sol` (or the
    // brief intermediate `sdmc:/flashnx/saves/...`) are still read via
    // the backend's read-fallback path.
    // Follows the player's games folder (#79): a save belongs beside the game
    // it came from. Hardcoding the default here meant a moved library kept
    // writing its `.sol` files back into the old folder, so the games lived in
    // one place and their progress in another.
    let flat_root = std::path::PathBuf::from(crate::library::primary_root());
    let legacy_root = std::path::PathBuf::from("sdmc:/ruffle/saves");
    let storage = SwitchStorageBackend::new(flat_root, legacy_root);

    let mut builder = PlayerBuilder::new()
        .with_boxed_renderer(std::boxed::Box::new(renderer) as std::boxed::Box<dyn RenderBackend>)
        .with_audio(SwitchAudioBackend::new())
        // Software video decoder (#89). Ruffle's own, pure Rust: without it the
        // player gets the null backend and every embedded video renders as
        // nothing at all, which is the white scene the report describes.
        .with_video(ruffle_video_software::backend::SoftwareVideoBackend::new())
        .with_log(SwitchLogBackend::new())
        .with_storage(std::boxed::Box::new(storage))
        // Custom UI backend: its only job over NullUiBackend is to forward
        // Ruffle's open/close-virtual-keyboard hooks (fired when an editable
        // TextField gains/loses focus) to an atomic the C++ loop polls, so we
        // can raise the Switch software keyboard for in-game text entry.
        .with_ui(SwitchUiBackend::new())
        .with_autoplay(true)
        // Report emulated Flash Player version 99 instead of Ruffle's
        // default 32. Ruffle builds `$version`/`getVersion()` as
        // "<plat> <player_version>,0,0,0", and a whole class of ~2008-era
        // games sniff the version with a broken single-character read:
        //   ver = Number($version.charAt(4)); if (ver < 7) showError();
        // charAt(4) is the FIRST digit of the major, so anything with a
        // two-digit major (10+, and Ruffle's 32) yields "1"/"3" < 7 and
        // trips a bogus "Flash Player 7 Required" splash (issue #64,
        // uncle-sam.swf — fails on real Flash 10+ and ruffle.rs too). A
        // major of 99 gives charAt(4)="9" (>=7, satisfies the single-char
        // checks) while 99 >= any real numeric `major >= N` gate, so it
        // pleases both broken and correct sniffers. 99 is the max value
        // that keeps the first digit at 9 (255 -> "2" would re-break them).
        // Safe globally: every internal player_version comparison in Ruffle
        // core (`< 7`, `<= 10`, `>= 18`) puts 99 on the same side as the
        // old default 32, so nothing but the reported string changes.
        .with_player_version(Some(99))
        // Portrait viewport while the picture is turned, so Ruffle LAYS THE STAGE
        // OUT for the shape the player will actually see. Turning the finished
        // landscape picture instead would show a sideways letterbox with the game
        // still using a third of the screen, which is the very problem #78 is
        // about. The framebuffer stays landscape; the renderer maps one onto the
        // other.
        .with_viewport_dimensions(
            if crate::backend::render::rotation_swaps_axes() { VIEWPORT_H } else { VIEWPORT_W },
            if crate::backend::render::rotation_swaps_axes() { VIEWPORT_W } else { VIEWPORT_H },
            1.0,
        )
        // Stage scaling, chosen by the user in REGLAGES > AFFICHAGE (issues
        // #65, #69, #74: three players in three languages asking to lose the
        // black bars). ShowAll stays the default, which is what Flash does:
        // aspect preserved, bars where the SWF does not reach 16:9.
        //   ExactFit fills by distorting, and is reached FIRST because it is the
        //   only mode that still shows the whole game.
        //   NoBorder fills keeping the aspect and crops the overflow: nothing is
        //   deformed, but up to a quarter of a 4:3 game's height goes off screen.
        // `force=true` in EVERY mode, for two reasons: the user's choice must
        // win over a SWF that sets `Stage.scaleMode` itself, and it is what
        // blocks `noScale` (the failure mode that left small SWFs rendering at
        // native size in the top-left corner — observed 2026-05-26 on Super
        // Mario World Flash 480x320 and Flappy Bird 500x700). Trade-off, same
        // as before: a SWF with its own responsive layout runs as a fixed-size
        // canvas. The corner-rect failure mode was much worse than that.
        .with_scale_mode(
            match pending_display_mode() {
                1 => StageScaleMode::ExactFit,
                2 => StageScaleMode::NoBorder,
                _ => StageScaleMode::ShowAll,
            },
            true,
        )
        // Force the empty StageAlign — Flash default = centered. SWFs
        // (e.g. Mario Forever Flash, observed 2026-05-26) sometimes set
        // `Stage.align = "L"` via AS to stick rendering to the left
        // edge, which gives an awful "game crammed in the left half of
        // the screen with empty space on the right" look on our 16:9
        // viewport. `force=true` blocks the SWF from changing it.
        .with_align(StageAlign::empty(), true)
        // Force `Letterbox::On` — draws black bars around the SWF stage
        // rect ALWAYS (not just in fullscreen mode, which is the
        // `Fullscreen` default). Without this, off-stage content drawn
        // outside the SWF's declared bounds bleeds into the viewport —
        // observed 2026-05-26 on Flappy Bird where the off-screen
        // pipes / sprite-pool entities were visible left/right of the
        // playable area. Letterboxing clips the rendering to the stage
        // rect, giving us black bars + a clean playable zone.
        .with_letterbox(Letterbox::On);
    crate::net::log(&std::format!(
        "ruffle_init: audio + storage backends constructed (scale_mode={} + align=centered + letterbox=On all forced)\n",
        match pending_display_mode() {
            1 => "ExactFit",
            2 => "NoBorder",
            _ => "ShowAll",
        },
    ));

    // Look for a SWF on the SD card. First call populates `CACHED_SWF` so
    // subsequent ruffle_init invocations (e.g. menu REDEMARRER) skip the
    // expensive `std::fs::read` — see CACHED_SWF docs for the OOM reason.
    let (movie_bytes, source_label) = match ensure_swf_loaded() {
        Some(t) => t,
        None => {
            log(b"ruffle_init: no SWF available, using embedded fallback\n\0");
            (
                EMBEDDED_FALLBACK_SWF.to_vec(),
                std::string::String::from("http://flashforswitch.local/SimpleRedBackground.swf"),
            )
        }
    };

    // Load the user's keymap (sidecar → default → hardcoded fallback). Uses
    // the SWF basename to find a per-game sidecar like
    // `sdmc:/ruffle/Super_Mario_63_2010.swf.keymap.json`. Idempotent across
    // restarts so REDEMARRER doesn't reload a different keymap mid-session,
    // but re-initialises when back-to-library picks a different game.
    // Key the keymap by the on-SD FILE name (what the library OPTIONS > TOUCHES
    // editor uses), NOT the movie URL `source_label`: a downloaded game carries
    // a `<file>.base` sidecar whose launchCommand URL filename differs from the
    // SD filename, so deriving the basename from `source_label` made the in-game
    // and library TOUCHES editors load DIFFERENT keymap files (their controls
    // didn't match). LAST_SWF_REAL_PATH holds the actual SD path that was loaded.
    let keymap_basename = LAST_SWF_REAL_PATH
        .lock()
        .ok()
        .and_then(|g| g.clone())
        .and_then(|p| p.rsplit(['/', '\\']).next().map(std::string::String::from))
        .unwrap_or_else(|| {
            source_label
                .rsplit('/')
                .next()
                .unwrap_or("unknown.swf")
                .to_string()
        });
    // Open this game's log window. The ring is global and a session can hold
    // several games, so a bug report has to know WHICH game its log describes —
    // otherwise reporting the first of five games played would attach the last
    // one's log under the first one's name.
    crate::bugreport::begin_game_log(&keymap_basename);
    keymap::init_for_swf(&keymap_basename); // P1 + P2 bindings (issue #40) from one file
    // This game's screen filter, now that the active basename is known. Set on
    // EVERY launch, including back to 0, so a filtered game never leaves its
    // filter behind for the next one.
    crate::backend::render::set_screen_filter(keymap::screen_filter());
    // Same for the power mode, and for the same reason it is set on EVERY
    // launch including back to 0: one game asking for a raised clock must never
    // leave the next one running raised. The way back down also happens on the
    // way out of a game (flashnx_clocks_restore in main.cpp), so this is the
    // second of two guards, not the only one.
    crate::backend::render::apply_power_mode(keymap::power_mode());

    // Sidecar dir is needed BEFORE the movie is built (the HTML container's
    // FlashVars live in the tree, see below) and again after, for the navigator.
    let real_path = LAST_SWF_REAL_PATH
        .lock()
        .ok()
        .and_then(|g| g.clone());
    let sidecar_dir = sidecar_dir_for(real_path.as_deref());
    log_str(&std::format!(
        "ruffle_init: sidecar dir = {}\n",
        sidecar_dir.display()
    ));

    // HTML-wrapped game? Its container page carries both the FlashVars (below)
    // and the base every relative load must resolve against (the navigator).
    let container = container_html_for(&sidecar_dir, &source_label);

    match SwfMovie::from_data(&movie_bytes, source_label.clone(), None) {
        Ok(mut movie) => {
            log_str(&std::format!(
                "ruffle_init: SwfMovie parsed (version={}, dims={}x{}, frames={}, url={})\n",
                movie.version(),
                movie.width().to_pixels(),
                movie.height().to_pixels(),
                movie.num_frames(),
                movie.url(),
            ));
            // The screen filter spaces its scanlines on the GAME's own vertical
            // resolution, not the screen's: at one line per screen pixel the
            // pattern is finer than the eye resolves and reads as a flat dimming.
            crate::backend::render::set_stage_height(movie.height().to_pixels() as u32);
            // TEST (Agent P): the Disney minigame engine, in production mode
            // (see the Capabilities.playerType="PlugIn" change), reads FlashVars
            // that the browser container `disneygames-iframe.js` normally injects
            // to know where its API + configs live. Without them init() does
            // Loader.load(null) -> Error #2007. Synthesize them from the movie URL
            // (host-pathed layout the SidecarNavigator serves). Gated to Agent P.
            // `Capabilities.playerType` is a PER-GAME choice for the Disney
            // shells (see the note in Ruffle's `capabilities.rs`): Agent P needs
            // "PlugIn" or it launches an unpublished dev harness, while Tron
            // Uprising needs the default "StandAlone" or it takes the online
            // branch and dies dereferencing `api.dataStorageService` offline.
            // Opt in only the game that needs it.
            ruffle_core::set_plugin_player_type(
                source_label.contains("phf_spl_act_agentpstrikesback"),
            );
            if source_label.contains("phf_spl_act_agentpstrikesback") {
                let host_root = source_label
                    .splitn(4, '/')
                    .take(3)
                    .collect::<std::vec::Vec<_>>()
                    .join("/");
                let game_dir = source_label
                    .rsplit_once('/')
                    .map(|(d, _)| d.to_string())
                    .unwrap_or_else(|| source_label.clone());
                let gc = std::format!("{}/v1/game_container", host_root);
                let params: std::vec::Vec<(std::string::String, std::string::String)> = std::vec![
                    (std::string::String::from("id"), std::string::String::from("1864911")),
                    (std::string::String::from("game"), std::string::String::from("1864911")),
                    (std::string::String::from("gameDivId"), std::string::String::from("DisneyGame")),
                    (std::string::String::from("fileUrl"), source_label.clone()),
                    (std::string::String::from("type"), std::string::String::from("AS3")),
                    (std::string::String::from("gameConfigUrl"), std::format!("{}/game_config.xml", game_dir)),
                    (std::string::String::from("apiUrl"), std::format!("{}/swf/as3MinigameApi_2_5_6.swf", gc)),
                    (std::string::String::from("apiConfigUrl"), std::format!("{}/xml/minigameAPIConfig.xml", gc)),
                ];
                log_str(&std::format!("ruffle_init: injected Disney FlashVars: {:?}\n", params));
                movie.append_parameters(params);
            } else if let Some((html, _)) = container.as_ref() {
                // Generic HTML-wrapped game: the container page carries the
                // FlashVars literally, so read them off it instead of hardcoding
                // another game. Without them a loader SWF has no idea what to
                // load and sits on its splash forever (Dragon City / DCLoader).
                // Relative values need no rewriting here: the navigator resolves
                // them against the same page (see `with_document_base`).
                let mut vars = crate::sources::gamezip::flashvars_from_html(html);
                if vars.is_empty() {
                    // No FlashVars: this may still be a DISNEY container, which
                    // declares a JS `config` object that `disneygames-iframe.js`
                    // turns into the parameters in the browser. Read that object
                    // and build the same parameters ourselves. Without them the
                    // game's `loadConfig()` reaches `Loader.load(null)` and dies
                    // on TypeError #2007 before the first frame (Tron Uprising:
                    // Escape from Argon City).
                    let cfg = crate::sources::gamezip::disney_config_from_html(html);
                    if !cfg.is_empty() {
                        log_str(&std::format!(
                            "ruffle_init: Disney container config: {:?}\n",
                            cfg
                        ));
                        vars = crate::sources::gamezip::synthesize_disney_params(
                            &cfg,
                            &source_label,
                        );
                    }
                }
                if vars.is_empty() {
                    log_str("ruffle_init: container HTML found but carries no FlashVars\n");
                } else {
                    log_str(&std::format!(
                        "ruffle_init: injected {} FlashVars from container HTML: {:?}\n",
                        vars.len(),
                        vars
                    ));
                    movie.append_parameters(vars);
                }
            }
            // Load behaviour stays Ruffle's default (Streaming).
            //
            // It was briefly forced to `Delayed` for every movie under 64 MB, to
            // fix Learn to Fly 2 (#76). It worked, but the price was far too high:
            // a movie that sees itself fully loaded the moment it starts finishes
            // EVERY game's preloader instantly, so the loading screen an author
            // wrote -- often the only art on the first screen, and sometimes the
            // thing that gates the intro -- never plays anywhere. Fixing one game
            // by silently removing a stage from all of them is not a trade worth
            // making.
            //
            // The underlying problem is ours and narrower than the load mode: our
            // sidecar requests FAIL IN ZERO MILLISECONDS, so a failure callback is
            // delivered in the same host frame as the tick that started it, before
            // the root timeline has advanced past frame 1. A real server takes
            // ~100 ms, by which time the root is past frame 3 and the game's own
            // guard skips the `gotoAndStop(2)` that strands us. Giving failed
            // requests a plausible latency would fix #76 without touching how any
            // movie loads. See the issue before trying `Delayed` again.
            builder = builder.with_movie(movie);
        }
        Err(e) => {
            log_str(&std::format!(
                "ruffle_init: SwfMovie::from_data failed: {}\n",
                e
            ));
        }
    }

    // Multi-file SWF support: wire a sidecar NavigatorBackend so relative
    // loads (loadMovie / GetURL into _levelN) resolve to sibling files on the
    // SD card. `source_label` is the synthetic movie URL relative loads are
    // resolved against; `sidecar_dir` is `<game-dir>/<game-stem>.files`, derived
    // from the real on-disk path. `executor` (kept in State) drives the loader
    // futures the navigator spawns — pumped once per frame in render_frame_with_dt.
    let executor = NullExecutor::new();
    // Fresh executor, fresh parked-failure list: a delayed failure left over from
    // the previously played game must never wake into this one (#76).
    crate::backend::navigator::reset_parked_failures();
    let mut navigator =
        SidecarNavigator::new(executor.spawner(), source_label.clone(), sidecar_dir);
    if let Some((_, page_base)) = container.as_ref() {
        // Flash resolves relative URLs against the embedding DOCUMENT, not the
        // SWF. Only differs for HTML-wrapped games, where the movie sits in a
        // subdirectory of its page (Dragon City: page `.../dragoncity/`, movie
        // `.../dragoncity/flash/`) — without this every `assets/...` request
        // gained a spurious `flash/` and missed.
        log_str(&std::format!(
            "ruffle_init: navigator resolves relative loads against {}\n",
            page_base
        ));
        navigator = navigator.with_document_base(page_base.clone());
    }
    builder = builder.with_navigator(navigator);

    // Emulate the browser "container" side of ExternalInterface. This both LOGS
    // the Flash<->JS call contract (so we can see exactly what a container-based
    // game like Agent P Strikes Back asks for) and, by existing, makes
    // `ExternalInterface.available` return true — which changes how Disney/Yamago
    // minigames bootstrap their minigame API. Harmless for ordinary games (they
    // never call ExternalInterface).
    builder = builder.with_external_interface(std::boxed::Box::new(ContainerInterface));

    log(b"ruffle_init: calling PlayerBuilder::build()\n\0");
    let player = builder.build();
    log(b"ruffle_init: PlayerBuilder::build() returned\n\0");

    unsafe {
        STATE = Some(State {
            player,
            executor,
            cursor_x: VIEWPORT_W as f32 * 0.5,
            cursor_y: VIEWPORT_H as f32 * 0.5,
            cursor_stage_x: VIEWPORT_W as f32 * 0.5,
            cursor_stage_y: VIEWPORT_H as f32 * 0.5,
            cursor_clicked: false,
        });
    }
    0
}

/// Find the HTML container page of an HTML-wrapped game, inside its sidecar
/// tree, and return its bytes.
///
/// Flashpoint curates some games with `launchCommand` pointing at an
/// `index.html` (Disney minigames, Dragon City...). Extraction resolves the real
/// entry SWF out of that page, but the page ALSO holds the FlashVars the movie
/// needs, so we come back for it at play time. The tree mirrors the URL layout,
/// so the page sits at or above the entry SWF's own directory: for
/// `http://host/static/dragoncity/flash/DCLoader.swf` it is
/// `<game>.files/host/static/dragoncity/index.html`, one level up.
///
/// Reading it at PLAY time (rather than stashing it at download time) means
/// games already installed are covered too, with no re-download.
/// Returns the page bytes AND the URL of the directory holding it (with a
/// trailing `/`), which is the base relative FlashVars must resolve against.
fn container_html_for(
    sidecar_dir: &std::path::Path,
    movie_url: &str,
) -> Option<(std::vec::Vec<u8>, std::string::String)> {
    let rest = movie_url
        .strip_prefix("http://")
        .or_else(|| movie_url.strip_prefix("https://"))?;
    let rest = rest.split(['?', '#']).next()?;
    // Host + path segments, minus the entry SWF's own filename.
    let mut segs: std::vec::Vec<&str> = rest
        .split('/')
        .filter(|s| !s.is_empty() && *s != ".." && *s != ".")
        .collect();
    segs.pop()?; // drop the SWF filename -> its directory
    // Walk up from the SWF's directory looking for the container page.
    for _ in 0..4 {
        let mut cand = sidecar_dir.to_path_buf();
        for s in &segs {
            cand.push(s);
        }
        cand.push("index.html");
        let bytes =
            crate::sources::gamezip::read_file_bounded(&cand.to_string_lossy(), 256 * 1024);
        if let Some(b) = bytes {
            let base = std::format!("http://{}/", segs.join("/"));
            log_str(&std::format!(
                "ruffle_init: container HTML = {} ({} bytes, base {})\n",
                cand.display(),
                b.len(),
                base
            ));
            return Some((b, base));
        }
        if segs.pop().is_none() {
            break;
        }
    }
    None
}


/// Per-game sidecar directory for a multi-file game's sibling SWFs:
/// `<game-dir>/<game-stem>.files`. E.g. `sdmc:/flashnx/Foo.swf` ->
/// `sdmc:/flashnx/Foo.files`. Falls back to a shared dir if the real path is
/// unknown (embedded fallback SWF). Per-game (not flat) so two games can each
/// ship their own `top.swf` without colliding. `pub(crate)` so the download
/// flow (library.rs) writes companions to the SAME dir the navigator reads.
pub(crate) fn sidecar_dir_for(real_path: Option<&str>) -> std::path::PathBuf {
    use std::path::PathBuf;
    match real_path {
        Some(p) => {
            let pb = PathBuf::from(p);
            let parent = pb
                .parent()
                .map(|x| x.to_path_buf())
                .unwrap_or_else(|| PathBuf::from("sdmc:/flashnx"));
            let stem = pb.file_stem().and_then(|s| s.to_str()).unwrap_or("game");
            parent.join(std::format!("{}.files", stem))
        }
        None => PathBuf::from("sdmc:/flashnx/_shared.files"),
    }
}

/// Return the cached `(bytes, url)`, loading from disk on the first call
/// for the current cache slot. Subsequent calls (post-REDEMARRER) clone
/// the cached bytes without touching the SD — see `CACHED_SWF` docs for
/// why that matters. The cache is cleared by `ruffle_library_reset` so
/// back-to-library pick of a different game gets fresh bytes.
///
/// Returns None when no SWF candidate read succeeds at all; the caller
/// then falls back to the embedded red SWF.
fn ensure_swf_loaded() -> Option<(std::vec::Vec<u8>, std::string::String)> {
    if let Ok(g) = CACHED_SWF.lock() {
        if let Some(cached) = g.as_ref() {
            // The budget reset lives at the bottom of this function, on the
            // path that reads from the card — so REDEMARRER, which returns
            // here instead, never reset it. The counter is process-global, so
            // the restarted movie started already full and refused nearly
            // every bitmap: measured on Super Smash Flash 2, 5163 refusals,
            // 9 atlases instead of 84 and 852 bitmaps instead of 4947, which
            // is the extreme form of the missing-sprite report. Reset here too.
            ruffle_core::reset_bitmap_cache(cached.0.len() as u64);
            // ~15 MB clone — measured at ~30 ms on Switch CPU. Acceptable
            // overhead for the back-to-library use case; pause-menu
            // REDEMARRER still benefits from skipping the SD read.
            return Some(cached.clone());
        }
    }
    let (bytes, path) = find_and_load_swf_uncached()?;
    // Transparently unwrap 4399-style "loadBytes the real game" wrappers
    // (see `maybe_unwrap_embedded_game`). Done before caching so REDEMARRER /
    // back-to-library reuse the inner game bytes and skip re-parsing the shell.
    let bytes = match maybe_unwrap_embedded_game(&bytes) {
        Some(inner) => inner,
        None => bytes,
    };
    log_str(&std::format!(
        "ruffle_init: loaded {} bytes from {}\n",
        bytes.len(),
        path,
    ));
    // Size the decoded-bitmap budget against THIS movie and clear the previous
    // game's accounting. The counter is process-global, so without the reset a
    // second launch starts already "full" and refuses nearly every bitmap
    // (observed on Super Smash Flash 2: white sprites, varying run to run).
    ruffle_core::reset_bitmap_cache(bytes.len() as u64);
    // Ruffle's URL parser rejects "sdmc" as an IDN, so we synthesize an
    // http URL keyed by the basename. Stable across restarts → SharedObject
    // paths stay the same.
    let basename = path
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or("movie.swf");
    // Host-pathed Flashpoint games ship a `<game>.swf.base` sidecar holding the
    // original launchCommand URL (e.g. http://static.nickjr.com/.../game.swf).
    // Use it as the movie URL so relative loads (configuration.xml, data/*.xml,
    // assets/**/*.swf) resolve to `<host>/<path>` — exactly the layout the
    // host-aware SidecarNavigator + the extracted `.files/<host>/<path>` tree
    // expect. Without it the SWF runs under the flat `flashforswitch.local`
    // base and every relative asset 404s (Super Brawl 2 stuck on a loader,
    // 2026-06-14). Falls back to the synthetic URL for direct/single-file games.
    let base_path = std::format!("{}.base", path);
    let url = crate::sources::gamezip::read_file_bounded(&base_path, 4096)
        .and_then(|b| std::string::String::from_utf8(b).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| s.starts_with("http://") || s.starts_with("https://"))
        .unwrap_or_else(|| std::format!("http://flashforswitch.local/{}", basename));
    log_str(&std::format!("ruffle_init: movie base url = {}\n", url));
    let entry = (bytes, url);
    // Skip the restart cache for very large SWFs. The cache holds a SECOND full
    // copy of the movie (Ruffle's SwfMovie already owns its own decompressed
    // copy) purely to make pause-menu REDEMARRER skip the SD re-read. For an
    // ordinary game (Mario 63 = 15 MB) that second copy is cheap and worth it,
    // but a bitmap-heavy giant like Sonic RPG Ep.10 (284 MB uncompressed) plus
    // its ~1.6 GB of decoded atlases already crowds the ~3.2 GB title heap;
    // holding the redundant 284 MB tips it into OOM mid-load. For these we drop
    // the cache and let REDEMARRER re-read from SD (the pre-cache behaviour, and
    // only a risk for this rare class of game). `read_swf_file_bounded` is now
    // pre-sized so that re-read is as OOM-safe as we can make it.
    const CACHE_MAX: usize = 100 * 1024 * 1024;
    if entry.0.len() <= CACHE_MAX {
        if let Ok(mut g) = CACHED_SWF.lock() {
            *g = Some(entry.clone());
        }
    } else {
        log_str(&std::format!(
            "ruffle_init: SWF is {} bytes (> {} MB), skipping restart cache to save heap\n",
            entry.0.len(),
            CACHE_MAX / (1024 * 1024),
        ));
    }
    Some(entry)
}

/// Some Chinese game-portal SWFs are a tiny AS3 shell whose only job is to
/// `Loader.loadBytes()` the real game, which ships embedded as a
/// `DefineBinaryData` blob and is bound to a class name ending in
/// `…_gamefile` via `SymbolClass` (e.g. 4399's `prefor.System4399Manager`
/// wrapper, class `L4399Main_gamefile`).
///
/// Our AVM2 doesn't yet instantiate that loadBytes'd child's document class,
/// so the wrapper renders a frozen near-empty stage — observed on
/// `catmario.swf` (Cat Mario / Syobon Action): root advances thousands of
/// frames but only ~5 shapes ever register, giving the user a "red screen".
///
/// Detect that exact shape and transparently swap in the inner game SWF,
/// bypassing the loadBytes path entirely. Returns `Some(inner_bytes)` when an
/// embedded game SWF is found; `None` leaves ordinary SWFs untouched. The
/// `…gamefile` SymbolClass marker is specific enough that a normal standalone
/// game won't false-positive.
fn maybe_unwrap_embedded_game(bytes: &[u8]) -> Option<std::vec::Vec<u8>> {
    let buf = swf::decompress_swf(bytes).ok()?;
    let parsed = swf::parse_swf(&buf).ok()?;

    // 1. Find the character id that SymbolClass marks as the game payload.
    let mut game_id: Option<swf::CharacterId> = None;
    for tag in &parsed.tags {
        if let swf::Tag::SymbolClass(links) = tag {
            for link in links {
                if link
                    .class_name
                    .to_str_lossy(swf::UTF_8)
                    .to_ascii_lowercase()
                    .contains("gamefile")
                {
                    game_id = Some(link.id);
                }
            }
        }
    }
    let game_id = game_id?;

    // 2. Pull the matching DefineBinaryData and confirm it's itself an SWF
    //    (FWS uncompressed / CWS zlib / ZWS lzma).
    for tag in &parsed.tags {
        if let swf::Tag::DefineBinaryData(bin) = tag {
            let is_swf = bin.data.len() > 8
                && {
                    let sig = &bin.data[0..3];
                    sig == b"FWS" || sig == b"CWS" || sig == b"ZWS"
                };
            if bin.id == game_id && is_swf {
                log_str(&std::format!(
                    "unwrap: portal wrapper detected — loading embedded game directly (id={}, {} bytes)\n",
                    game_id,
                    bin.data.len(),
                ));
                return Some(bin.data.to_vec());
            }
        }
    }
    None
}

/// Read a SWF file into a `Vec`, bounded by `MAX`, using a chunked read loop.
/// NEVER use `std::fs::read` / `read_to_end` here: on Horizon the newlib glue
/// returns a spurious `OutOfMemory` on the single big read once the heap has
/// fragmented after a long play session. That is issues #62/#63: play one game
/// for a while, launch a DIFFERENT one (which clears `CACHED_SWF` and forces a
/// fresh disk read), this read failed -> `ensure_swf_loaded` returned `None` ->
/// the embedded red fallback SWF was shown. Same mitigation the cover / storage
/// / keymap loaders use (`covers::read_file_bounded` etc.).
///
/// We pre-size the buffer to the true on-disk length (via `seek`, since
/// `std::fs::metadata` is unreliable on Horizon — see gamezip.rs) so the chunked
/// read never grows the `Vec` by doubling; doubling up to hundreds of MB briefly
/// holds ~2x the final size and OOMs on a fragmented heap. That matters for the
/// rare pathologically large game: Sonic RPG Ep.10 is a 284 MB *uncompressed*
/// (FWS) SWF, which also drove the cap up from 64 MB to 320 MB. `try_reserve_exact`
/// fails gracefully (-> None -> caller logs) instead of aborting the process when
/// the heap can't satisfy the allocation. `MAX` still bounds a mistaken multi-GB
/// file (ordinary games are ~15 MB, e.g. Mario 63).
fn read_swf_file_bounded(path: &str) -> Option<std::vec::Vec<u8>> {
    use std::io::{Read, Seek, SeekFrom};
    const MAX: usize = 320 * 1024 * 1024;
    let mut f = std::fs::File::open(path).ok()?;
    let mut data: std::vec::Vec<u8> = std::vec::Vec::new();
    // Pre-reserve the exact on-disk size so the read below never reallocates.
    // Reject oversize files up front; fall back to a growing read if `seek`
    // is somehow unavailable for this file.
    if let Ok(end) = f.seek(SeekFrom::End(0)) {
        let size = end as usize;
        if size > MAX {
            return None;
        }
        f.seek(SeekFrom::Start(0)).ok()?;
        data.try_reserve_exact(size).ok()?;
    }
    let mut buf = [0u8; 64 * 1024];
    loop {
        match f.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                data.extend_from_slice(&buf[..n]);
                if data.len() > MAX {
                    return None;
                }
            }
            Err(_) => return None,
        }
    }
    Some(data)
}

/// Try the runtime override path (set by C++ via `ruffle_set_swf_path`)
/// first, then fall back to `SWF_CANDIDATES`. Returns the first file we
/// can successfully read.
/// Stage-scaling mode of the game we are ABOUT to launch, read from its
/// `<basename>.display` sidecar. Uses `OVERRIDE_SWF_PATH`, which C++ fills when
/// the user picks a game, because the player is built before the movie is read:
/// `LAST_SWF_REAL_PATH` and the keymap's active basename are both still empty at
/// that point. No override (candidate scan, forwarder without a path) → 0, the
/// letterboxed default.
/// Rotation of the game we are ABOUT to launch, read the same way and for the
/// same reason as `pending_display_mode`: the viewport has to be chosen before
/// the movie is read, and at that moment the keymap's active basename is still
/// empty. Reading it from the keymap instead silently gave 0 for every launch,
/// so neither the per-game value nor the global default was ever applied -- only
/// the live cycle in the pause menu worked.
fn pending_rotation() -> u8 {
    OVERRIDE_SWF_PATH
        .lock()
        .ok()
        .and_then(|g| g.clone())
        .and_then(|p| p.rsplit(['/', '\\']).next().map(std::string::String::from))
        .map(|b| keymap::rotation_for(&b))
        .unwrap_or_else(crate::loc::default_rotation)
}

/// The zoom + framing the game was left with, from its `.prefs` (issue #101).
/// Same shape as `pending_rotation`: the basename is only known through the
/// override path at this point in the launch.
fn pending_zoom() -> (u16, i32, i32) {
    let base = OVERRIDE_SWF_PATH
        .lock()
        .ok()
        .and_then(|g| g.clone())
        .and_then(|p| p.rsplit(['/', '\\']).next().map(std::string::String::from));
    match base {
        Some(b) => {
            let (ox, oy) = keymap::zoom_pan_for(&b);
            (keymap::zoom_for(&b), ox, oy)
        }
        None => (100, 0, 0),
    }
}

fn pending_display_mode() -> u8 {
    OVERRIDE_SWF_PATH
        .lock()
        .ok()
        .and_then(|g| g.clone())
        .and_then(|p| p.rsplit(['/', '\\']).next().map(std::string::String::from))
        .map(|b| keymap::display_mode_for(&b))
        .unwrap_or(0)
}

fn find_and_load_swf_uncached() -> Option<(std::vec::Vec<u8>, std::string::String)> {
    // Snapshot the override path so we don't hold the lock across the
    // (slow) file read.
    let override_path: Option<std::string::String> = OVERRIDE_SWF_PATH
        .lock()
        .ok()
        .and_then(|g| g.clone());
    if let Some(path) = override_path {
        match read_swf_file_bounded(&path) {
            Some(bytes) => {
                log_str(&std::format!("scan: using override path {}\n", path));
                if let Ok(mut g) = LAST_SWF_REAL_PATH.lock() {
                    *g = Some(path.clone());
                }
                return Some((bytes, path));
            }
            None => {
                // TERMINAL. Falling through to the candidate list here meant the
                // user picked one game and another one BOOTED -- with its
                // controls, its saves and its sidecar tree, because
                // LAST_SWF_REAL_PATH binds all three -- while the library banked
                // the playtime against the game they actually chose, and
                // ruffle_init still returned 0 so nothing said a word. Most
                // reachable through the HOME forwarder, whose argv is accepted
                // without checking the file exists, and games really do get
                // deleted.
                log_str(&std::format!(
                    "scan: override path {} unreadable - refusing to substitute another game\n",
                    path,
                ));
                return None;
            }
        }
    }
    // Only when NO override was set: the app was opened without a chosen game.
    for path in SWF_CANDIDATES {
        match read_swf_file_bounded(path) {
            Some(bytes) => {
                if let Ok(mut g) = LAST_SWF_REAL_PATH.lock() {
                    *g = Some(std::string::String::from(*path));
                }
                return Some((bytes, std::string::String::from(*path)));
            }
            None => {
                log_str(&std::format!("scan: {} not found or unreadable\n", path));
            }
        }
    }
    None
}

#[no_mangle]
pub extern "C" fn ruffle_render_frame() {
    // Back-compat entry: fall back to 1/60 if the C++ side didn't measure
    // elapsed time itself. New main.cpp uses ruffle_render_frame_dt instead.
    render_frame_with_dt(FloatDuration::from_secs(1.0 / 60.0));
}

#[no_mangle]
pub extern "C" fn ruffle_render_frame_dt(dt_us: u64) {
    let dt = FloatDuration::from_secs(dt_us as f64 / 1_000_000.0);
    render_frame_with_dt(dt);
}

/// How long one SWF frame lasts, in microseconds, clamped to a sane band.
///
/// The touch path (#87) uses it to hold a tap's click back until the movie has
/// actually run a frame with the new cursor position: at 24 fps under a 60 Hz
/// host loop, "one host frame later" is not enough — most host frames run no
/// SWF frame at all. Clamped so a movie declaring an absurd frame rate can't
/// make taps feel broken either way.
#[no_mangle]
pub extern "C" fn ruffle_frame_interval_us() -> u64 {
    const DEFAULT: u64 = 33_333;
    let state = unsafe {
        match (*core::ptr::addr_of_mut!(STATE)).as_mut() {
            Some(s) => s,
            None => return DEFAULT,
        }
    };
    let fps = match state.player.lock() {
        Ok(p) => p.frame_rate(),
        Err(_) => return DEFAULT,
    };
    if fps <= 0.0 || !fps.is_finite() {
        return DEFAULT;
    }
    ((1_000_000.0 / fps) as u64).clamp(16_000, 60_000)
}

/// Two levels of the display list, one line each: what is on stage, where, and
/// whether it is visible. Printed once a few seconds into a game, so a "black
/// screen" report carries the answer with it.
#[no_mangle]
pub extern "C" fn ruffle_dump_stage_children() {
    let state = unsafe {
        match (*core::ptr::addr_of_mut!(STATE)).as_mut() {
            Some(s) => s,
            None => return,
        }
    };
    if let Ok(mut p) = state.player.lock() {
        log_str("---- stage ----\n");
        p.dump_stage_children();
    }
}

/// Every AVM1 variable the movie holds, two levels deep.
///
/// A stuck level is a condition that never becomes true, and the terms of that
/// condition are ordinary variables. Printing them is the difference between
/// guessing and reading: on a game that would not finish its ninth stage, the
/// clear check was `enemy.hp <= 0 && enemy2.hp <= 0 && enemy3.hp <= 0`, and in
/// SWF 8 an `undefined` term makes that false for ever.
#[no_mangle]
pub extern "C" fn ruffle_dump_root_vars() {
    let state = unsafe {
        match (*core::ptr::addr_of_mut!(STATE)).as_mut() {
            Some(s) => s,
            None => return,
        }
    };
    if let Ok(mut p) = state.player.lock() {
        p.dump_root_vars();
    }
}

/// Hide `us` microseconds of wall clock from the movie (#87).
///
/// Frames only advance on the `dt` we hand to `tick()`, but `getTimer()` reads
/// the real clock, so every interval where we stop ticking — pause menu,
/// in-game keyboard, HOME menu — comes back as a jump for any game that drives
/// its simulation from `getTimer()` deltas. The C++ side measures the gap and
/// hands it here, so the resumed frame sees the clock it left off on.
#[no_mangle]
pub extern "C" fn ruffle_skip_paused_time(us: u64) {
    if us == 0 {
        return;
    }
    let state = unsafe {
        match (*core::ptr::addr_of_mut!(STATE)).as_mut() {
            Some(s) => s,
            None => return,
        }
    };
    if let Ok(mut p) = state.player.lock() {
        p.skip_paused_time(core::time::Duration::from_micros(us));
    }
}

fn render_frame_with_dt(dt: FloatDuration) {
    let state = unsafe {
        match (*core::ptr::addr_of_mut!(STATE)).as_mut() {
            Some(s) => s,
            None => return,
        }
    };

    let mut player = match state.player.lock() {
        Ok(p) => p,
        Err(_) => return,
    };
    // Profile tick (AVM1 advance + game logic + filter cache) vs render
    // (our backend dispatch: shape/bitmap/gradient draws to GL) so the
    // heartbeat in render.rs can show the breakdown — tells us whether
    // CPU (AVM) or GPU (draws) is the perf bottleneck in any given scene.
    // Play the container's half of the ExternalInterface dialogue, outside any
    // call from the movie (#100).
    let pending: std::vec::Vec<(std::string::String, Option<std::string::String>)> =
        match EI_PENDING.lock() {
            Ok(mut q) if !q.is_empty() => q.drain(..).collect(),
            _ => std::vec::Vec::new(),
        };
    for (name, arg) in pending {
        log_str(&std::format!(
            "EI container -> movie: {}({})\n",
            name,
            arg.as_deref().unwrap_or(""),
        ));
        let args: std::vec::Vec<ExtValue> = match arg {
            Some(a) => std::vec![ExtValue::String(a)],
            None => std::vec::Vec::new(),
        };
        player.call_internal_interface(&name, args);
    }
    use std::sync::atomic::Ordering;
    let t0 = unsafe { ruffle_tick_now() };
    player.tick(dt);
    let t1 = unsafe { ruffle_tick_now() };
    // Where the root timeline actually IS, once a second.
    //
    // A game that has gone quiet looks the same from the outside whatever the
    // reason: the frame counter keeps running, the scene keeps being drawn, and
    // the heartbeat's `tick` merely reads near zero. This says whether the root
    // is PARKED on a frame (a preloader waiting on something that will never
    // arrive, a menu with no script) or still advancing while its content does
    // nothing, which are different bugs with different owners. Costs one
    // `mutate_with` a second.
    {
        static LAST: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);
        let n = LAST.fetch_add(1, Ordering::Relaxed);
        if n % 60 == 0 {
            let (cur, total) = player.mutate_with_update_context(|context| {
                let root = context.stage.root_clip();
                match root.and_then(|r| r.as_movie_clip()) {
                    Some(mc) => (mc.current_frame() as i32, mc.frames_loaded()),
                    None => (0, 0),
                }
            });
            log_str(&std::format!("root: frame {}/{}\n", cur, total));
        }
    }
    player.render();
    let t2 = unsafe { ruffle_tick_now() };
    let tick_dt = t1.saturating_sub(t0);
    let render_dt = t2.saturating_sub(t1);
    TICK_TICKS_ACCUM.fetch_add(tick_dt, Ordering::Relaxed);
    RENDER_TICKS_ACCUM.fetch_add(render_dt, Ordering::Relaxed);
    TICK_TICKS_MAX.fetch_max(tick_dt, Ordering::Relaxed);
    RENDER_TICKS_MAX.fetch_max(render_dt, Ordering::Relaxed);

    // Slow-frame detector. A frame whose wall time (tick + render) blows the
    // FPS budget gets a one-line breakdown of what it did, so an FPS spike can
    // be attributed to its cause (offscreen filter passes, bitmap uploads,
    // shape tessellation, draw count, …) instead of being averaged away by the
    // 60-frame heartbeat. Fires only above threshold, so it stays silent during
    // smooth play and never floods nxlink — but catches every spike. 22 ms ≈
    // below 45 fps (a 60 fps frame is 16.7 ms).
    const SLOW_FRAME_US: u64 = 22_000;
    let tick_freq = unsafe { ruffle_tick_freq() };
    let slow_frame = if tick_freq > 0 {
        let total_us = (tick_dt.saturating_add(render_dt))
            .saturating_mul(1_000_000)
            / tick_freq;
        if total_us > SLOW_FRAME_US {
            let tick_us = (tick_dt.saturating_mul(1_000_000)) / tick_freq;
            let render_us = (render_dt.saturating_mul(1_000_000)) / tick_freq;
            Some((total_us, tick_us, render_us))
        } else {
            None
        }
    } else {
        None
    };

    // Overlay the cursor crosshair on top of whatever Ruffle drew. We pull
    // a `&mut SwitchRenderBackend` out of the Player by downcasting the
    // trait object — `RenderBackend: Any` so this is just a vtable check.
    // The per-game "show cursor" toggle (keymap) only hides this VISUAL — mouse
    // moves/clicks were already dispatched above, so pointer input still works.
    let cx = state.cursor_x;
    let cy = state.cursor_y;
    let clicked = state.cursor_clicked;
    let show_cursor = keymap::show_cursor();
    let renderer = player.renderer_mut();
    if let Some(backend) =
        <dyn std::any::Any>::downcast_mut::<SwitchRenderBackend>(renderer)
    {
        if let Some((total_us, tick_us, render_us)) = slow_frame {
            backend.log_slow_frame(total_us, tick_us, render_us);
        }
        if show_cursor {
            backend.draw_cursor_overlay(cx, cy, clicked);
        }
    }

    // Drive any loader futures the SidecarNavigator spawned this frame
    // (multi-file games: loadMovie / GetURL-into-_levelN). This MUST run with
    // the player lock released — the futures re-lock the player to install the
    // loaded movie, so pumping them under our own guard would deadlock.
    drop(player);
    // Release any fetch failure whose pretend round-trip has elapsed (#76),
    // BEFORE running the executor so it resolves in this same pass instead of
    // waiting another frame. The executor is `run_until_stalled`, which is why
    // the delay can't be built from a self-waking future — see
    // `backend::navigator::wake_due_failures`.
    crate::backend::navigator::wake_due_failures();
    state.executor.run();
}

/// Redraw the current Player state WITHOUT advancing AVM/animation by a
/// time step — used while the pause modal is open so the frame behind the
/// modal stays frozen but doesn't go black (the back buffer would otherwise
/// be stale after a swap). Also redraws the cursor overlay so input still
/// visually responds while paused.
#[no_mangle]
pub extern "C" fn ruffle_redraw_paused() {
    let state = unsafe {
        match (*core::ptr::addr_of_mut!(STATE)).as_mut() {
            Some(s) => s,
            None => return,
        }
    };
    let mut player = match state.player.lock() {
        Ok(p) => p,
        Err(_) => return,
    };
    player.render();
    let cx = state.cursor_x;
    let cy = state.cursor_y;
    let clicked = state.cursor_clicked;
    let show_cursor = keymap::show_cursor();
    let renderer = player.renderer_mut();
    if let Some(backend) =
        <dyn std::any::Any>::downcast_mut::<SwitchRenderBackend>(renderer)
    {
        if show_cursor {
            backend.draw_cursor_overlay(cx, cy, clicked);
        }
    }
}

/// Draw the pause-menu overlay on top of whatever's already in the
/// framebuffer. `selected` indexes into `render::MENU_ITEMS`. C++ calls
/// this right after `ruffle_redraw_paused` so the menu sits on top of a
/// frozen game frame, then `gl_context_swap`s.
#[no_mangle]
pub extern "C" fn ruffle_draw_menu(selected: c_int) {
    let state = unsafe {
        match (*core::ptr::addr_of_mut!(STATE)).as_mut() {
            Some(s) => s,
            None => return,
        }
    };
    let Ok(mut player) = state.player.lock() else {
        return;
    };
    let renderer = player.renderer_mut();
    if let Some(backend) =
        <dyn std::any::Any>::downcast_mut::<SwitchRenderBackend>(renderer)
    {
        let idx = selected.max(0) as usize;
        // Scale-in pop on open (v1.2.0): the pause menu is C++-owned and only
        // draws while paused, so a fresh open is "this draw does not directly
        // follow the previous one". The dim backdrop stays put
        // (fill_screen_dim); only the panel scales.
        //
        // Detected on the RENDER FRAME COUNTER, not wall clock. It used to compare
        // elapsed ticks against `freq / 8` — a hardcoded 125 ms, chosen as "a few
        // frames" at 60 fps. On a game slow enough that ONE frame exceeds 125 ms
        // (Dragon City sits near 6 fps = ~166 ms), every continuation frame looked
        // like a fresh open, so the pop restarted forever and the panel stayed
        // pinned at MODAL_OPEN_FROM. The counter advances once per paused redraw
        // (`ruffle_redraw_paused` -> `player.render()` -> `submit_frame`), so this
        // is frame-rate independent, and it also covers reopening after TOUCHES or
        // after a close drain, which take different C++ paths.
        let now = unsafe { ruffle_tick_now() };
        static LAST_MENU_FRAME: std::sync::atomic::AtomicU32 =
            std::sync::atomic::AtomicU32::new(u32::MAX);
        let f = backend.frame_count();
        let last = LAST_MENU_FRAME.swap(f, std::sync::atomic::Ordering::Relaxed);
        if last == u32::MAX || f.wrapping_sub(last) > 1 {
            backend::render::modal_open_begin();
        }
        let (scale, active, _done) = backend::render::modal_scale_step(now);
        if active {
            backend.set_ui_modal_scale(scale);
        } else {
            backend.clear_ui_transform();
        }
        backend.draw_menu_overlay(idx);
        backend.clear_ui_transform();
    }
}

/// Draw the ECRAN sub-panel (display mode / rotation / filter) over the frozen
/// game frame. `selected` indexes `render::SCREEN_ITEMS`.
///
/// Its own frame counter, deliberately: entering the sub-panel interrupts
/// `ruffle_draw_menu`'s run of consecutive frames, so this one's counter is
/// stale on the first call and the panel pops in — and leaving it makes the
/// main menu's counter stale in turn, so that pops back. No extra open/close
/// FFI is needed for either direction; the same "this draw does not follow the
/// previous one" rule the pause menu already uses carries both.
#[no_mangle]
pub extern "C" fn ruffle_draw_screen_menu(selected: c_int) {
    let state = unsafe {
        match (*core::ptr::addr_of_mut!(STATE)).as_mut() {
            Some(s) => s,
            None => return,
        }
    };
    let Ok(mut player) = state.player.lock() else {
        return;
    };
    let renderer = player.renderer_mut();
    if let Some(backend) =
        <dyn std::any::Any>::downcast_mut::<SwitchRenderBackend>(renderer)
    {
        let idx = selected.max(0) as usize;
        let now = unsafe { ruffle_tick_now() };
        static LAST_SCREEN_FRAME: std::sync::atomic::AtomicU32 =
            std::sync::atomic::AtomicU32::new(u32::MAX);
        let f = backend.frame_count();
        let last = LAST_SCREEN_FRAME.swap(f, std::sync::atomic::Ordering::Relaxed);
        if last == u32::MAX || f.wrapping_sub(last) > 1 {
            backend::render::modal_open_begin();
        }
        let (scale, active, _done) = backend::render::modal_scale_step(now);
        if active {
            backend.set_ui_modal_scale(scale);
        } else {
            backend.clear_ui_transform();
        }
        backend.draw_screen_menu(idx);
        backend.clear_ui_transform();
    }
}

/// Begin the pause-menu close pop (scale-out). C++ calls this on dismiss
/// (Resume / B / Minus), then keeps calling `ruffle_draw_menu_closing` until it
/// returns 1, so the menu shrinks away before the game resumes.
#[no_mangle]
pub extern "C" fn ruffle_menu_close_begin() {
    backend::render::modal_close_begin();
}

/// Draw the pause menu scaling OUT for one frame (over the frozen game the caller
/// re-rendered first). Returns 1 once the close pop has finished — the caller
/// then resumes the game — or 0 while still animating. The dim backdrop stays put
/// (fill_screen_dim); only the panel shrinks.
#[no_mangle]
pub extern "C" fn ruffle_draw_menu_closing(selected: c_int) -> c_int {
    let state = unsafe {
        match (*core::ptr::addr_of_mut!(STATE)).as_mut() {
            Some(s) => s,
            None => return 1,
        }
    };
    let Ok(mut player) = state.player.lock() else {
        return 1;
    };
    let renderer = player.renderer_mut();
    if let Some(backend) =
        <dyn std::any::Any>::downcast_mut::<SwitchRenderBackend>(renderer)
    {
        let now = unsafe { ruffle_tick_now() };
        let (scale, _active, close_done) = backend::render::modal_scale_step(now);
        if close_done {
            backend.clear_ui_transform();
            return 1;
        }
        backend.set_ui_modal_scale(scale);
        let idx = selected.max(0) as usize;
        backend.draw_menu_overlay(idx);
        backend.clear_ui_transform();
        0
    } else {
        1
    }
}

/// Pause-menu AFFICHAGE row: cycle the ACTIVE game's stage scaling, persist it,
/// and apply it to the running player at once. Applying it live is the point of
/// putting this in the pause menu rather than in the library: the frozen game
/// frame is re-rendered behind the panel every frame, so the player sees what
/// each mode costs on THIS game before committing to it. Cropping is what fill
/// trades for the black bars, and how much it crops depends entirely on the
/// game's aspect ratio. Issues #65, #69, #74.
#[no_mangle]
pub extern "C" fn ruffle_display_mode_cycle() {
    let next = (keymap::display_mode() + 1) % keymap::DISPLAY_MODE_COUNT;
    keymap::set_display_mode(next);
    let state = unsafe {
        match (*core::ptr::addr_of_mut!(STATE)).as_mut() {
            Some(s) => s,
            None => return,
        }
    };
    if let Ok(mut player) = state.player.lock() {
        // `respect_forced = false` inside, so this wins over the forced mode the
        // player was built with.
        player.set_scale_mode(match next {
            1 => StageScaleMode::ExactFit,
            2 => StageScaleMode::NoBorder,
            _ => StageScaleMode::ShowAll,
        });
    }
}

/// The zoom + framing the ECRAN panel was showing when ZOOM was opened, so `B`
/// can put it back. `None` when the framing mode is not running.
static ZOOM_SNAPSHOT: std::sync::Mutex<Option<(u16, i32, i32)>> = std::sync::Mutex::new(None);

/// Enter the framing mode (issue #101): remember what to go back to.
#[no_mangle]
pub extern "C" fn ruffle_zoom_begin() {
    if let Ok(mut s) = ZOOM_SNAPSHOT.lock() {
        let (ox, oy) = backend::render::game_pan();
        *s = Some((backend::render::game_zoom_percent(), ox, oy));
    }
}

/// Nudge the zoom by `d_percent` and the framing by `(dx, dy)` screen pixels.
/// Applied live, never persisted: the frozen frame behind is redrawn with it, so
/// the player is judging the real thing before committing.
#[no_mangle]
pub extern "C" fn ruffle_zoom_adjust(d_percent: c_int, dx: c_int, dy: c_int) {
    let cur = backend::render::game_zoom_percent() as i32;
    let next = (cur + d_percent).clamp(
        backend::render::ZOOM_MIN as i32,
        backend::render::ZOOM_MAX as i32,
    ) as u16;
    let (ox, oy) = backend::render::game_pan();
    backend::render::set_game_zoom(
        next,
        ox + dx,
        oy + dy,
        VIEWPORT_W as f32,
        VIEWPORT_H as f32,
    );
}

/// The live percentage, for the pinch: a two-finger spread has to change the
/// zoom IN PROPORTION to what it already is, or the same gesture would move it
/// by a fifth at 100% and by a twentieth at 500%.
#[no_mangle]
pub extern "C" fn ruffle_zoom_percent() -> c_int {
    backend::render::game_zoom_percent() as c_int
}

/// Back to an untouched picture. There is no other way to land exactly on 100%
/// once you have been nudging by one percent at a time.
#[no_mangle]
pub extern "C" fn ruffle_zoom_reset() {
    backend::render::set_game_zoom(100, 0, 0, VIEWPORT_W as f32, VIEWPORT_H as f32);
}

/// `A`: keep it, and write it to the game's `.prefs` so it is there next launch.
#[no_mangle]
pub extern "C" fn ruffle_zoom_commit() {
    let (ox, oy) = backend::render::game_pan();
    keymap::set_zoom(backend::render::game_zoom_percent(), ox, oy);
    if let Ok(mut s) = ZOOM_SNAPSHOT.lock() {
        *s = None;
    }
}

/// `B`: put back what the panel was showing on the way in, and persist nothing.
#[no_mangle]
pub extern "C" fn ruffle_zoom_cancel() {
    let snap = ZOOM_SNAPSHOT.lock().ok().and_then(|mut s| s.take());
    if let Some((z, ox, oy)) = snap {
        backend::render::set_game_zoom(z, ox, oy, VIEWPORT_W as f32, VIEWPORT_H as f32);
    }
}

/// Draw the framing legend over the frozen frame. Separate from the panel draw
/// because in this mode there IS no panel: the picture is what is being judged.
#[no_mangle]
pub extern "C" fn ruffle_draw_zoom_overlay() {
    let state = unsafe {
        match (*core::ptr::addr_of_mut!(STATE)).as_mut() {
            Some(s) => s,
            None => return,
        }
    };
    let mut player = match state.player.lock() {
        Ok(p) => p,
        Err(_) => return,
    };
    let percent = backend::render::game_zoom_percent();
    let renderer = player.renderer_mut();
    if let Some(backend) = <dyn std::any::Any>::downcast_mut::<SwitchRenderBackend>(renderer) {
        backend.draw_zoom_overlay(percent);
    }
}

/// Pause-menu ROTATION row: turn the picture a quarter clockwise and persist it.
///
/// Unlike the display mode this cannot be applied to a running player alone: the
/// stage has to be LAID OUT for the new shape, so the logical viewport is
/// swapped as well. Ruffle recomputes its stage transform from it, the renderer
/// keeps the framebuffer landscape, and the paused frame behind the panel is
/// redrawn turned -- so the player sees the result before committing, exactly
/// like the display and filter rows. Issue #78.
#[no_mangle]
pub extern "C" fn ruffle_rotation_cycle() {
    let next = (keymap::rotation() + 1) % keymap::ROTATION_COUNT;
    keymap::set_rotation(next);
    crate::backend::render::set_game_rotation(next);
    // Turning the picture re-frames it, so the framing offset goes back to
    // centred while the magnification stays (issue #101).
    //
    // Not a shortcut: the quarter-turn swaps the LOGICAL viewport, which makes
    // Ruffle lay the stage out afresh. Flappy Bird fitted into 1280x720 is 514
    // wide and pillarboxed; into 720x1280 it is 720 wide and letterboxed. The
    // picture is a different size in a different place, so a framing offset
    // measured in screen pixels no longer points at what it was pointing at,
    // and no transform of that offset can recover it -- turning the vector a
    // quarter with it would only be a prettier guess.
    //
    // The magnification survives because it answers a question about the GAME
    // ("this is too small to read"), which the turn does not change. The framing
    // answers a question about the layout, which it does.
    {
        let z = crate::backend::render::game_zoom_percent();
        crate::backend::render::set_game_zoom(z, 0, 0, VIEWPORT_W as f32, VIEWPORT_H as f32);
        keymap::set_zoom(z, 0, 0);
    }
    let state = unsafe {
        match (*core::ptr::addr_of_mut!(STATE)).as_mut() {
            Some(s) => s,
            None => return,
        }
    };
    if let Ok(mut player) = state.player.lock() {
        let swap = crate::backend::render::rotation_swaps_axes();
        let (w, h) = if swap {
            (VIEWPORT_H, VIEWPORT_W)
        } else {
            (VIEWPORT_W, VIEWPORT_H)
        };
        player.set_viewport_dimensions(ruffle_render::backend::ViewportDimensions {
            width: w,
            height: h,
            scale_factor: 1.0,
        });
    }
}

/// Pause-menu FILTRE row: cycle the ACTIVE game's screen filter and persist it.
/// Unlike the scaling mode this touches no Ruffle state at all: the filter is a
/// pass the render backend runs over the finished frame, so setting the flag is
/// enough and the next redraw (the paused one, behind the panel) shows it.
/// The other half of issue #65.
#[no_mangle]
pub extern "C" fn ruffle_screen_filter_cycle() {
    let next = (keymap::screen_filter() + 1) % keymap::SCREEN_FILTER_COUNT;
    keymap::set_screen_filter(next);
    crate::backend::render::set_screen_filter(next);
}

/// Pause-menu OVERCLOCK row: cycle the ACTIVE game's power mode and persist it.
///
/// What gets persisted is what the hardware actually accepted, not what was
/// asked for. A raise can be refused (clkrst unavailable, or psm reporting the
/// battery out of its Normal voltage state), and storing the request in that
/// case would leave the game marked HIGH forever while running at 1020, with
/// the row saying one thing and the console doing another.
#[no_mangle]
pub extern "C" fn ruffle_power_mode_cycle() {
    let next = (keymap::power_mode() + 1) % keymap::POWER_MODE_COUNT;
    let got = crate::backend::render::apply_power_mode(next);
    keymap::set_power_mode(got);
    // Say what was asked for AND what was granted. A refusal is silent on the
    // clkrst side by design, and without this line the only trace of a press
    // that did nothing is the absence of a `clocks:` line, which is not a trace.
    log_str(&std::format!(
        "menu: OVERCLOCK asked={} got={}{}\n",
        next,
        got,
        if next != got { " (REFUSED)" } else { "" },
    ));
}

/// Drop the current Player + renderer (Ruffle owns the SwitchRenderBackend,
/// so its Drop frees VAOs/VBOs/atlases/programs) and re-run `ruffle_init`
/// to load the SWF afresh. The C++-managed GL context stays alive across
/// this call. Used by the pause menu's "REDEMARRER" entry. Returns 0 on
/// success, non-zero on init failure.
#[no_mangle]
pub extern "C" fn ruffle_restart() -> c_int {
    log(b"ruffle_restart: tearing down current Player\n\0");
    unsafe {
        STATE = None;
    }
    log(b"ruffle_restart: re-initialising\n\0");
    ruffle_init()
}

/// Look up the Flash key bound to a Switch button by NAME (e.g. "A",
/// "StickLLeft"). Called from C++ once per binding at boot to fill its
/// runtime BINDINGS array. Returns the matching SK_* code, or `SK_NONE` if
/// the button is unbound in the active keymap. The name must be one of the
/// values listed in `keymap::FALLBACK_BINDINGS` (case sensitive). Caller
/// passes a NUL-terminated UTF-8 C string.
#[no_mangle]
pub extern "C" fn ruffle_keymap_lookup(name: *const c_char) -> c_int {
    if name.is_null() {
        return SK_NONE;
    }
    // SAFETY: caller guarantees NUL-terminated UTF-8.
    let s = unsafe { core::ffi::CStr::from_ptr(name) };
    let Ok(button) = s.to_str() else {
        return SK_NONE;
    };
    keymap::lookup(button).unwrap_or(SK_NONE)
}

/// Player-2 (issue #40) equivalent of [`ruffle_keymap_lookup`]: resolve a
/// controller-2 button name to its Flash `SK_*` code via the P2 keymap.
#[no_mangle]
pub extern "C" fn ruffle_keymap_lookup_p2(name: *const c_char) -> c_int {
    if name.is_null() {
        return SK_NONE;
    }
    // SAFETY: caller guarantees NUL-terminated UTF-8.
    let s = unsafe { core::ffi::CStr::from_ptr(name) };
    let Ok(button) = s.to_str() else {
        return SK_NONE;
    };
    keymap::lookup_p2(button).unwrap_or(SK_NONE)
}

/// Map a C++ modifier code (1=ZL, 2=ZR, 3=L, 4=R) to its name, or "" if invalid.
fn combo_mod_name(code: c_int) -> &'static str {
    match code {
        1 => "ZL",
        2 => "ZR",
        3 => "L",
        4 => "R",
        _ => "",
    }
}

/// Combo-layer (issue #57, per-modifier) resolution: the key `name` sends while
/// `mod_code` (1=ZL,2=ZR,3=L,4=R) is held on controller 1. `SK_NONE` = no override
/// for that button → C++ falls through to the base key.
#[no_mangle]
pub extern "C" fn ruffle_keymap_lookup_combo(mod_code: c_int, name: *const c_char) -> c_int {
    if name.is_null() {
        return SK_NONE;
    }
    // SAFETY: caller guarantees NUL-terminated UTF-8.
    let s = unsafe { core::ffi::CStr::from_ptr(name) };
    let Ok(button) = s.to_str() else {
        return SK_NONE;
    };
    keymap::lookup_combo(combo_mod_name(mod_code), button).unwrap_or(SK_NONE)
}

/// Player-2 counterpart of [`ruffle_keymap_lookup_combo`].
#[no_mangle]
pub extern "C" fn ruffle_keymap_lookup_combo_p2(mod_code: c_int, name: *const c_char) -> c_int {
    if name.is_null() {
        return SK_NONE;
    }
    // SAFETY: caller guarantees NUL-terminated UTF-8.
    let s = unsafe { core::ffi::CStr::from_ptr(name) };
    let Ok(button) = s.to_str() else {
        return SK_NONE;
    };
    keymap::lookup_combo_p2(combo_mod_name(mod_code), button).unwrap_or(SK_NONE)
}

/// 1 when `mod_code`'s combo layer is active (has a binding) for P1 → C++ treats
/// that shoulder as a modifier; 0 otherwise.
#[no_mangle]
pub extern "C" fn ruffle_keymap_combo_active(mod_code: c_int) -> c_int {
    keymap::combo_active(combo_mod_name(mod_code)) as c_int
}

/// Player-2 counterpart of [`ruffle_keymap_combo_active`].
#[no_mangle]
pub extern "C" fn ruffle_keymap_combo_active_p2(mod_code: c_int) -> c_int {
    keymap::combo_active_p2(combo_mod_name(mod_code)) as c_int
}

/// Per-game cursor-speed preset index for the active keymap, or -1 if unset.
/// C++ reads this at game launch to restore a speed saved for THIS game.
#[no_mangle]
pub extern "C" fn ruffle_keymap_cursor_speed() -> c_int {
    keymap::cursor_speed()
}

/// Persist the active game's cursor-speed preset into its keymap (the in-game
/// VITESSE cycle calls this so pointer speed is per-game). `idx < 0` clears it.
#[no_mangle]
pub extern "C" fn ruffle_keymap_set_cursor_speed(idx: c_int) {
    keymap::set_cursor_speed(idx);
}

// ── TOUCHES sub-screen FFI ────────────────────────────────────────────────
//
// Thin wrappers over `menu::*`. C++ owns the pause-main modal (Reprendre /
// Touches / Redemarrer / Quitter); when the user picks "Touches", it calls
// `ruffle_touches_open` and from then on forwards joycon down-edges via
// `ruffle_touches_input` until `ruffle_touches_active` returns 0 again
// (user pressed B to back out). Each frame, C++ calls `ruffle_touches_draw`
// over the frozen-game backdrop, then `ruffle_touches_consume_dirty` to
// know whether to refresh its runtime BINDINGS table.

#[no_mangle]
pub extern "C" fn ruffle_touches_open() {
    // In-game, the pause TOUCHES entry now opens the sub-menu (#20 Option 1):
    // edit / apply / share / revert / cursor speed. The library opens the editor
    // directly via `menu::open()`.
    menu::open_submenu();
    // Scale the panel in, same pop as the library / pause modals.
    backend::render::modal_open_begin();
}

/// Last in-game TOUCHES screen kind drawn (0 = none), so `ruffle_touches_draw`
/// fires the scale-in pop only on the frame a sub-screen first appears — the
/// in-game mirror of `library`'s LAST_MODAL_KIND. Reset on close so reopening
/// always pops fresh.
static LAST_TOUCHES_KIND: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

#[no_mangle]
pub extern "C" fn ruffle_touches_close() {
    menu::close();
    LAST_TOUCHES_KIND.store(0, std::sync::atomic::Ordering::Relaxed);
}

#[no_mangle]
pub extern "C" fn ruffle_touches_active() -> c_int {
    if menu::is_active() { 1 } else { 0 }
}

#[no_mangle]
pub extern "C" fn ruffle_touches_input(button_name: *const c_char) -> c_int {
    if button_name.is_null() {
        return 0;
    }
    let s = unsafe { core::ffi::CStr::from_ptr(button_name) };
    let Ok(b) = s.to_str() else { return 0 };
    if menu::input(b) { 1 } else { 0 }
}

#[no_mangle]
pub extern "C" fn ruffle_touches_consume_dirty() -> c_int {
    if menu::consume_dirty() { 1 } else { 0 }
}

/// Render the active TOUCHES screen on top of whatever's already in the
/// framebuffer. No-op when the sub-screen is inactive. Caller should call
/// `ruffle_redraw_paused` first so a frozen game frame sits underneath.
#[no_mangle]
pub extern "C" fn ruffle_touches_draw() {
    let state = unsafe {
        match (*core::ptr::addr_of_mut!(STATE)).as_mut() {
            Some(s) => s,
            None => return,
        }
    };
    let Ok(mut player) = state.player.lock() else { return };
    let renderer = player.renderer_mut();
    if let Some(backend) =
        <dyn std::any::Any>::downcast_mut::<SwitchRenderBackend>(renderer)
    {
        // Scale-in pop. Re-triggered on EVERY sub-screen transition (Menu ->
        // List -> Dropdown -> Profiles -> Preview ...), mirroring the on-cover
        // modals (library::render's modal_kind tracking) — without this, only the
        // first entry popped and every sub-screen snapped in. The dim backdrop
        // stays put (fill_screen_dim); only the panel scales.
        let now = unsafe { ruffle_tick_now() };
        let kind = menu::screen_kind();
        let last = LAST_TOUCHES_KIND.swap(kind, std::sync::atomic::Ordering::Relaxed);
        if kind != 0 && kind != last {
            backend::render::modal_open_begin();
        }
        let (scale, active, _done) = backend::render::modal_scale_step(now);
        if active {
            backend.set_ui_modal_scale(scale);
        } else {
            backend.clear_ui_transform();
        }
        // Tag the touch tables the panels below are about to publish. Same 70-89
        // band the library uses for these screens, so one routing rule covers
        // both entrances to the editor. Zero while the panel is scaling: the rows
        // are drawn somewhere other than where the table says they are.
        backend::render::set_ui_screen_kind(if active || kind == 0 { 0 } else { 70 + kind as u32 });
        menu::draw(backend, now);
        backend.clear_ui_transform();
    }
}

/// Touchscreen for the IN-GAME pause editor (TOUCHES). The launcher has had
/// `ruffle_library_touch` since v1.2.0; the pause panels drew the same lists
/// through the same renderers and simply had nothing feeding them, so the
/// on-screen keyboard could only be reached with a stick.
///
/// Drag a list to scroll it, tap a row to move the cursor, tap the same row
/// again to take it (which is `menu::input("A")`, not a copy of what A does).
/// The gesture itself is `render::row_touch_feed`, the same one the launcher
/// uses, so the two entrances to this editor cannot behave differently.
#[no_mangle]
pub extern "C" fn ruffle_menu_touch(x: f32, y: f32, pressed: c_int) {
    // Only while the editor owns the screen. The published table outlives the
    // panel that published it, and the last one standing before a game launched
    // was the launcher's.
    if !menu::is_active() {
        backend::render::row_touch_cancel();
        return;
    }
    // C++ hands us PHYSICAL screen pixels; the panel's rows were published in the
    // LOGICAL viewport, which is portrait while the picture is turned. Undo the
    // turn here, at the single point where the outside world's coordinates come
    // in, exactly as `ruffle_handle_mouse_move` does.
    //
    // The zoom is NOT undone, unlike there: `world_matrix` gates it on
    // `game_layer`, and the pause panel is drawn outside that layer. Undoing it
    // would move the panel's hit boxes with a zoom the panel never had.
    let pw = VIEWPORT_W as f32;
    let ph = VIEWPORT_H as f32;
    let (x, y) = match backend::render::game_rotation() {
        1 => (y, pw - x),
        2 => (pw - x, ph - y),
        3 => (ph - y, x),
        _ => (x, y),
    };
    match backend::render::row_touch_feed(x, y, pressed != 0) {
        backend::render::RowTouch::Tap(sx, sy) => {
            // The sub-screen that is up RIGHT NOW, in the same 70-89 band the
            // library uses. Read from the menu module rather than from the
            // render stamp: the stamp is written at draw time, and C++ serves
            // buttons before touch, so it lags by a frame on exactly the frame
            // that matters.
            let live = 70 + menu::screen_kind() as u32;
            if let Some(hit) = backend::render::ui_cells_hit(live, sx, sy) {
                if menu::touch_select(hit) {
                    menu::input("A");
                }
            }
        }
        // No Scrolled arm: the editor is a pad now and the keyboard picker was
        // never a list, so neither publishes a scrolling row view and a drag
        // over them has nothing to commit. Both answer taps, which is the arm
        // above.
        _ => {}
    }
}

// ── Library boot screen (Phase 3.4) ──────────────────────────────────────
//
// Standalone SwitchRenderBackend used by the pre-Ruffle library UI. Lives
// in its own slot because the Ruffle one is owned by `Player` and doesn't
// exist yet at boot. Once the user picks a game we drop this renderer so
// its GL resources (96 MB arena VBO/IBO, shader programs, banner texture)
// free up before `ruffle_init` builds Ruffle's own.

static LIBRARY_RENDERER: Mutex<Option<SwitchRenderBackend>> = Mutex::new(None);

#[no_mangle]
pub extern "C" fn ruffle_library_init() -> c_int {
    // The launcher NEVER turns, whatever the game the player just left was set
    // to. The rotation is a global in the renderer -- one place, so that the
    // game, its pause panel and its pointer cannot disagree -- and the price of
    // that choice is that returning from a turned game has to put it back. It
    // did not, so quitting Flappy Bird left the whole of FlashNX on its side.
    crate::backend::render::set_game_rotation(0);
    // Same for the zoom, and for the same reason: it is a global on the game's
    // layer, and the launcher is not the game.
    crate::backend::render::set_game_zoom(100, 0, 0, VIEWPORT_W as f32, VIEWPORT_H as f32);
    // Pick the UI language (settings.json → system language → English)
    // before anything draws.
    loc::init();
    // UI renders at panel size (the C++ side keeps the surface there until a
    // game launches), so the library stays sharp.
    let mut renderer = match SwitchRenderBackend::new_ui(UI_VIEWPORT_W, UI_VIEWPORT_H) {
        Some(r) => r,
        None => {
            log(b"library_init: SwitchRenderBackend::new failed\n\0");
            return -1;
        }
    };
    // Decode the embedded banner PNG and upload as a GL texture. On
    // failure (corrupt asset, OOM) we set tex=0 and the library falls back
    // to ASCII title — no fatal.
    if let Some((rgba, w, h)) = library::decode_banner() {
        let tex = renderer.upload_rgba_texture(&rgba, w, h);
        if tex != 0 {
            library::set_banner_texture(tex, w, h);
        }
    }
    if let Ok(mut slot) = LIBRARY_RENDERER.lock() {
        *slot = Some(renderer);
    }
    // Fresh renderer = empty glyph atlas, so the CJK font is cold again: re-arm the
    // language picker's loading panel (else it freezes on the re-upload after a game).
    library::note_renderer_reset();
    // Phase 3.7 — write embedded CA bundle to SD so libcurl can verify
    // TLS certs. Cheap & idempotent (no-op if already present at the
    // right size). Done here once per `.nro` boot, not per library cycle.
    net::boot_init();
    // Make sure the user-facing root dir exists. Downloads write here
    // and the SD scan list reads from it. Idempotent (errors on EEXIST
    // are silently swallowed by `create_dir_all`).
    let _ = std::fs::create_dir_all("sdmc:/flashnx");
    log(b"library_init: standalone renderer + banner ready\n\0");
    0
}

/// Push one `.swf` path onto the library's scan list. Called by
/// `swf_picker_run` (cpp/src/swf_picker.cpp) per file. Returns 0 on success.
#[no_mangle]
pub extern "C" fn ruffle_library_add_path(path: *const c_char, mtime: u64) -> c_int {
    if path.is_null() {
        return -1;
    }
    let s = unsafe { core::ffi::CStr::from_ptr(path) };
    let Ok(p) = s.to_str() else { return -2 };
    if library::add_path(p, mtime) { 0 } else { -3 }
}

/// Record one path from the SD scan's directory listing (games AND sidecars),
/// so the per-game sidecar probes resolve in memory instead of hitting the SD.
/// Called by `scan_dir_all` for every `readdir` entry, before any `add_path`
/// for that directory.
#[no_mangle]
pub extern "C" fn ruffle_library_note_file(path: *const c_char) {
    if path.is_null() {
        return;
    }
    let s = unsafe { core::ffi::CStr::from_ptr(path) };
    if let Ok(p) = s.to_str() {
        library::note_file(p);
    }
}

/// Bracket the SD scan: `begin` loads the cached scan metadata (so an unchanged
/// game costs no file access at all), `end` rewrites it if anything missed.
#[no_mangle]
pub extern "C" fn ruffle_library_scan_begin() {
    library::scan_begin();
}

#[no_mangle]
pub extern "C" fn ruffle_library_scan_end() {
    library::scan_end();
}

/// Mark a scanned directory as fully enumerated (or confirmed absent), so the
/// index may answer "this sidecar isn't there" without an SD probe.
#[no_mangle]
pub extern "C" fn ruffle_library_note_dir(dir: *const c_char) {
    if dir.is_null() {
        return;
    }
    let s = unsafe { core::ffi::CStr::from_ptr(dir) };
    if let Ok(d) = s.to_str() {
        library::note_dir(d);
    }
}

/// Transition the library from Inactive → List/Empty. Call after the SD
/// scan has populated all entries.
#[no_mangle]
pub extern "C" fn ruffle_library_open() {
    library::open();
}

#[no_mangle]
pub extern "C" fn ruffle_library_active() -> c_int {
    if library::is_active() { 1 } else { 0 }
}

#[no_mangle]
pub extern "C" fn ruffle_library_picked() -> c_int {
    if library::picked() { 1 } else { 0 }
}

/// Forward a Switch-button down-edge (e.g. "A", "Up", "Minus") to the
/// library state machine. Returns 1 if consumed, 0 otherwise.
#[no_mangle]
pub extern "C" fn ruffle_library_input(button_name: *const c_char) -> c_int {
    if button_name.is_null() {
        return 0;
    }
    let s = unsafe { core::ffi::CStr::from_ptr(button_name) };
    let Ok(b) = s.to_str() else { return 0 };
    if library::input(b) { 1 } else { 0 }
}

/// Forward the current touchscreen state to the JOUER gallery: drag to scroll,
/// tap a tile to select it, tap the selected tile again to launch. `pressed`
/// != 0 means a finger is down at `(x, y)` in screen px; 0 means no touch.
#[no_mangle]
pub extern "C" fn ruffle_library_touch(x: f32, y: f32, pressed: c_int) {
    library::touch(x, y, pressed != 0);
}

/// Render one library frame to the current GL framebuffer. C++ calls
/// `gl_context_swap` afterwards.
#[no_mangle]
pub extern "C" fn ruffle_library_render() {
    let Ok(mut slot) = LIBRARY_RENDERER.lock() else { return };
    let Some(backend) = slot.as_mut() else { return };
    library::render(backend);
}

/// Copy the selected SWF path into a C-owned buffer. Returns 0 on success.
/// -1 if no path was picked (user quit), -2 if `cap` is too small.
#[no_mangle]
pub extern "C" fn ruffle_library_selected_path(out: *mut c_char, cap: c_int) -> c_int {
    let Some(path) = library::selected_path() else { return -1 };
    let bytes = path.as_bytes();
    let needed = bytes.len() + 1; // +1 for NUL
    if (cap as usize) < needed {
        return -2;
    }
    unsafe {
        core::ptr::copy_nonoverlapping(bytes.as_ptr(), out as *mut u8, bytes.len());
        *out.add(bytes.len()) = 0;
    }
    0
}

/// Drop the standalone library renderer so its GL resources (~96 MB arena
/// + shader programs + banner texture) free BEFORE `ruffle_init` builds
/// Ruffle's own. Idempotent.
#[no_mangle]
pub extern "C" fn ruffle_library_shutdown() {
    if let Ok(mut slot) = LIBRARY_RENDERER.lock() {
        if slot.is_some() {
            log(b"library_shutdown: dropping standalone renderer\n\0");
        }
        *slot = None;
    }
}

/// Reset all per-game state so the next library cycle picks up a fresh
/// game cleanly: library entries cleared, keymap dropped (next
/// `init_for_swf` re-reads sidecar), CACHED_SWF + OVERRIDE_SWF_PATH
/// cleared (next `ruffle_init` re-reads the new pick from SD). Called by
/// C++ when the user picks QUITTER in the in-game pause menu and we
/// loop back to the library.
#[no_mangle]
pub extern "C" fn ruffle_library_reset() {
    log(b"library_reset: clearing entries / keymap / SWF cache / override\n\0");
    library::reset();
    keymap::reset();
    if let Ok(mut g) = CACHED_SWF.lock() {
        *g = None;
    }
    if let Ok(mut g) = OVERRIDE_SWF_PATH.lock() {
        *g = None;
    }
}

/// Switch button codes shared with `cpp/src/main.cpp`. Keep these in sync.
/// We map joycon → Flash key events; Mario 63 (and most AS2 Flash games)
/// use Space/Z for jump, Enter for start, arrows for movement.
pub(crate) const SK_NONE: c_int = 0;
pub(crate) const SK_SPACE: c_int = 1;
pub(crate) const SK_ENTER: c_int = 2;
pub(crate) const SK_ESCAPE: c_int = 3;
pub(crate) const SK_LEFT: c_int = 4;
pub(crate) const SK_RIGHT: c_int = 5;
pub(crate) const SK_UP: c_int = 6;
pub(crate) const SK_DOWN: c_int = 7;
pub(crate) const SK_Z: c_int = 8;
pub(crate) const SK_X: c_int = 9;
pub(crate) const SK_SHIFT: c_int = 10;
pub(crate) const SK_P: c_int = 11;
// Full alphabet — A-Z minus Z/X/P which already have constants. Order
// = alphabetical for readability; the numeric SK_* values are an
// opaque enum (only matters that they're unique and stable). Phase
// 3.3.bis (2026-05-26 nuit) — bumped from 12-key platformer subset to
// full keyboard so games binding to arbitrary letters (Flappy Bird
// "press A to jump", Mario Forever "press W to throw", etc.) work.
pub(crate) const SK_A: c_int = 12;
pub(crate) const SK_B: c_int = 13;
pub(crate) const SK_C: c_int = 14;
pub(crate) const SK_D: c_int = 15;
pub(crate) const SK_E: c_int = 16;
pub(crate) const SK_F: c_int = 17;
pub(crate) const SK_G: c_int = 18;
pub(crate) const SK_H: c_int = 19;
pub(crate) const SK_I: c_int = 20;
pub(crate) const SK_J: c_int = 21;
pub(crate) const SK_K: c_int = 22;
pub(crate) const SK_L: c_int = 23;
pub(crate) const SK_M: c_int = 24;
pub(crate) const SK_N: c_int = 25;
pub(crate) const SK_O: c_int = 26;
pub(crate) const SK_Q: c_int = 27;
pub(crate) const SK_R: c_int = 28;
pub(crate) const SK_S: c_int = 29;
pub(crate) const SK_T: c_int = 30;
pub(crate) const SK_U: c_int = 31;
pub(crate) const SK_V: c_int = 32;
pub(crate) const SK_W: c_int = 33;
pub(crate) const SK_Y: c_int = 34;
// Digits 0-9.
pub(crate) const SK_0: c_int = 35;
pub(crate) const SK_1: c_int = 36;
pub(crate) const SK_2: c_int = 37;
pub(crate) const SK_3: c_int = 38;
pub(crate) const SK_4: c_int = 39;
pub(crate) const SK_5: c_int = 40;
pub(crate) const SK_6: c_int = 41;
pub(crate) const SK_7: c_int = 42;
pub(crate) const SK_8: c_int = 43;
pub(crate) const SK_9: c_int = 44;
// Common non-letter keys.
pub(crate) const SK_TAB: c_int = 45;
pub(crate) const SK_BACKSPACE: c_int = 46;
pub(crate) const SK_CONTROL: c_int = 47;
pub(crate) const SK_ALT: c_int = 48;
// Pseudo-codes: NOT keyboard keys. A button bound to one of these fires a mouse
// click at the cursor instead of a key event — the C++ in-game loop routes them
// to `ruffle_handle_mouse_button` / `ruffle_handle_mouse_right`. `key_descriptor`
// returns None for them, so they never reach `ruffle_handle_key`.
pub(crate) const SK_MOUSE_LEFT: c_int = 49;
pub(crate) const SK_MOUSE_RIGHT: c_int = 50;
// Numpad digits 0-9 (Flash key codes 96-105, distinct from the top-row digits
// 48-57). Many 2-player Flash games hard-code P2 to the keypad (Bleach vs Naruto
// P2 = Numpad 1-6, KOF Wing). Added via PR #46 (YuQiyang).
pub(crate) const SK_NUMPAD0: c_int = 51;
pub(crate) const SK_NUMPAD1: c_int = 52;
pub(crate) const SK_NUMPAD2: c_int = 53;
pub(crate) const SK_NUMPAD3: c_int = 54;
pub(crate) const SK_NUMPAD4: c_int = 55;
pub(crate) const SK_NUMPAD5: c_int = 56;
pub(crate) const SK_NUMPAD6: c_int = 57;
pub(crate) const SK_NUMPAD7: c_int = 58;
pub(crate) const SK_NUMPAD8: c_int = 59;
pub(crate) const SK_NUMPAD9: c_int = 60;
// Function keys F1-F12 (issue #57 — games that hard-code F1..F12 for menus /
// hotkeys). Contiguous 61-72 so `key_descriptor` can range-match.
pub(crate) const SK_F1: c_int = 61;
pub(crate) const SK_F2: c_int = 62;
pub(crate) const SK_F3: c_int = 63;
pub(crate) const SK_F4: c_int = 64;
pub(crate) const SK_F5: c_int = 65;
pub(crate) const SK_F6: c_int = 66;
pub(crate) const SK_F7: c_int = 67;
pub(crate) const SK_F8: c_int = 68;
pub(crate) const SK_F9: c_int = 69;
pub(crate) const SK_F10: c_int = 70;
pub(crate) const SK_F11: c_int = 71;
pub(crate) const SK_F12: c_int = 72;
// Punctuation / symbol keys on the main block — so the visual keyboard picker
// (issue #55) can offer a full PC layout, and games reading `-`/`=`/etc. work.
pub(crate) const SK_MINUS: c_int = 73; // `-` (Minus)
pub(crate) const SK_EQUALS: c_int = 74; // `=`
pub(crate) const SK_LBRACKET: c_int = 75; // `[`
pub(crate) const SK_RBRACKET: c_int = 76; // `]`
pub(crate) const SK_SEMICOLON: c_int = 77; // `;`
pub(crate) const SK_QUOTE: c_int = 78; // `'`
pub(crate) const SK_COMMA: c_int = 79; // `,`
pub(crate) const SK_PERIOD: c_int = 80; // `.`
pub(crate) const SK_SLASH: c_int = 81; // `/`
pub(crate) const SK_BACKSLASH: c_int = 82; // `\`
pub(crate) const SK_BACKQUOTE: c_int = 83; // backquote
// Numpad operators (distinct keycodes from the main block). The picker's `+`
// maps here (there is no lone `+` physical key — it's Shift+= on a real board),
// which is also what most Flash zoom controls poll.
pub(crate) const SK_NUMPAD_ADD: c_int = 84; // `+`
pub(crate) const SK_NUMPAD_SUB: c_int = 85; // `-` (keypad)
pub(crate) const SK_NUMPAD_MUL: c_int = 86; // `*`
pub(crate) const SK_NUMPAD_DIV: c_int = 87; // `/` (keypad)
pub(crate) const SK_NUMPAD_DECIMAL: c_int = 88; // `.` (keypad)
pub(crate) const SK_NUMPAD_ENTER: c_int = 89; // keypad Enter
// Caps Lock (Flash keyCode 20). Niche, but some games gate a mechanic on it:
// "This is the Only Level" stage 24 (issue #61) opens the exit gate ONLY while
// Caps Lock is held. Ruffle maps PhysicalKey::CapsLock -> KeyCode 20 and, on a
// synthesised KeyDown, both add_key(20) and toggle_key(20) fire — so a button
// bound here drives Key.isDown(20) AND Key.isToggled(20) with no extra plumbing.
pub(crate) const SK_CAPSLOCK: c_int = 90;

// Pseudo-code, like the two mouse buttons: opens the console keyboard instead of
// sending a key. Ruffle raises the keyboard on its own when an editable field
// takes focus, but only for fields it recognises as focused — a game whose text
// box it does not track leaves the player with no way in at all. Bound to a
// button, this is that way in. `key_descriptor` returns None for it.
pub(crate) const SK_KEYBOARD: c_int = 91;

fn key_descriptor(code: c_int) -> Option<KeyDescriptor> {
    let (physical, logical) = match code {
        SK_SPACE => (PhysicalKey::Space, LogicalKey::Character(' ')),
        SK_ENTER => (PhysicalKey::Enter, LogicalKey::Named(NamedKey::Enter)),
        SK_ESCAPE => (PhysicalKey::Escape, LogicalKey::Named(NamedKey::Escape)),
        SK_LEFT => (PhysicalKey::ArrowLeft, LogicalKey::Named(NamedKey::ArrowLeft)),
        SK_RIGHT => (PhysicalKey::ArrowRight, LogicalKey::Named(NamedKey::ArrowRight)),
        SK_UP => (PhysicalKey::ArrowUp, LogicalKey::Named(NamedKey::ArrowUp)),
        SK_DOWN => (PhysicalKey::ArrowDown, LogicalKey::Named(NamedKey::ArrowDown)),
        SK_SHIFT => (PhysicalKey::ShiftLeft, LogicalKey::Named(NamedKey::Shift)),
        // A-Z (alphabetical). Each is a physical KeyX + logical char
        // 'x' (lowercase — Flash treats the logical key as the
        // unmodified char; Shift is handled separately).
        SK_A => (PhysicalKey::KeyA, LogicalKey::Character('a')),
        SK_B => (PhysicalKey::KeyB, LogicalKey::Character('b')),
        SK_C => (PhysicalKey::KeyC, LogicalKey::Character('c')),
        SK_D => (PhysicalKey::KeyD, LogicalKey::Character('d')),
        SK_E => (PhysicalKey::KeyE, LogicalKey::Character('e')),
        SK_F => (PhysicalKey::KeyF, LogicalKey::Character('f')),
        SK_G => (PhysicalKey::KeyG, LogicalKey::Character('g')),
        SK_H => (PhysicalKey::KeyH, LogicalKey::Character('h')),
        SK_I => (PhysicalKey::KeyI, LogicalKey::Character('i')),
        SK_J => (PhysicalKey::KeyJ, LogicalKey::Character('j')),
        SK_K => (PhysicalKey::KeyK, LogicalKey::Character('k')),
        SK_L => (PhysicalKey::KeyL, LogicalKey::Character('l')),
        SK_M => (PhysicalKey::KeyM, LogicalKey::Character('m')),
        SK_N => (PhysicalKey::KeyN, LogicalKey::Character('n')),
        SK_O => (PhysicalKey::KeyO, LogicalKey::Character('o')),
        SK_P => (PhysicalKey::KeyP, LogicalKey::Character('p')),
        SK_Q => (PhysicalKey::KeyQ, LogicalKey::Character('q')),
        SK_R => (PhysicalKey::KeyR, LogicalKey::Character('r')),
        SK_S => (PhysicalKey::KeyS, LogicalKey::Character('s')),
        SK_T => (PhysicalKey::KeyT, LogicalKey::Character('t')),
        SK_U => (PhysicalKey::KeyU, LogicalKey::Character('u')),
        SK_V => (PhysicalKey::KeyV, LogicalKey::Character('v')),
        SK_W => (PhysicalKey::KeyW, LogicalKey::Character('w')),
        SK_X => (PhysicalKey::KeyX, LogicalKey::Character('x')),
        SK_Y => (PhysicalKey::KeyY, LogicalKey::Character('y')),
        SK_Z => (PhysicalKey::KeyZ, LogicalKey::Character('z')),
        // 0-9.
        SK_0 => (PhysicalKey::Digit0, LogicalKey::Character('0')),
        SK_1 => (PhysicalKey::Digit1, LogicalKey::Character('1')),
        SK_2 => (PhysicalKey::Digit2, LogicalKey::Character('2')),
        SK_3 => (PhysicalKey::Digit3, LogicalKey::Character('3')),
        SK_4 => (PhysicalKey::Digit4, LogicalKey::Character('4')),
        SK_5 => (PhysicalKey::Digit5, LogicalKey::Character('5')),
        SK_6 => (PhysicalKey::Digit6, LogicalKey::Character('6')),
        SK_7 => (PhysicalKey::Digit7, LogicalKey::Character('7')),
        SK_8 => (PhysicalKey::Digit8, LogicalKey::Character('8')),
        SK_9 => (PhysicalKey::Digit9, LogicalKey::Character('9')),
        // Common modifier / control keys.
        SK_TAB => (PhysicalKey::Tab, LogicalKey::Named(NamedKey::Tab)),
        SK_BACKSPACE => (PhysicalKey::Backspace, LogicalKey::Named(NamedKey::Backspace)),
        SK_CONTROL => (PhysicalKey::ControlLeft, LogicalKey::Named(NamedKey::Control)),
        SK_ALT => (PhysicalKey::AltLeft, LogicalKey::Named(NamedKey::Alt)),
        // Caps Lock — Flash keyCode 20. Logical mapping drives the code; a held
        // button gives Key.isDown(20) (issue #61, "This is the Only Level" st.24).
        SK_CAPSLOCK => (PhysicalKey::CapsLock, LogicalKey::Named(NamedKey::CapsLock)),
        // Numpad 0-9 — the KeyLocation::Numpad below makes Ruffle emit the
        // distinct keypad codes (96-105) these games listen for.
        SK_NUMPAD0 => (PhysicalKey::Numpad0, LogicalKey::Character('0')),
        SK_NUMPAD1 => (PhysicalKey::Numpad1, LogicalKey::Character('1')),
        SK_NUMPAD2 => (PhysicalKey::Numpad2, LogicalKey::Character('2')),
        SK_NUMPAD3 => (PhysicalKey::Numpad3, LogicalKey::Character('3')),
        SK_NUMPAD4 => (PhysicalKey::Numpad4, LogicalKey::Character('4')),
        SK_NUMPAD5 => (PhysicalKey::Numpad5, LogicalKey::Character('5')),
        SK_NUMPAD6 => (PhysicalKey::Numpad6, LogicalKey::Character('6')),
        SK_NUMPAD7 => (PhysicalKey::Numpad7, LogicalKey::Character('7')),
        SK_NUMPAD8 => (PhysicalKey::Numpad8, LogicalKey::Character('8')),
        SK_NUMPAD9 => (PhysicalKey::Numpad9, LogicalKey::Character('9')),
        // Function keys F1-F12. Physical + logical are both the named key; no
        // printable char, so they never take the TextInput/keyPress path.
        SK_F1 => (PhysicalKey::F1, LogicalKey::Named(NamedKey::F1)),
        SK_F2 => (PhysicalKey::F2, LogicalKey::Named(NamedKey::F2)),
        SK_F3 => (PhysicalKey::F3, LogicalKey::Named(NamedKey::F3)),
        SK_F4 => (PhysicalKey::F4, LogicalKey::Named(NamedKey::F4)),
        SK_F5 => (PhysicalKey::F5, LogicalKey::Named(NamedKey::F5)),
        SK_F6 => (PhysicalKey::F6, LogicalKey::Named(NamedKey::F6)),
        SK_F7 => (PhysicalKey::F7, LogicalKey::Named(NamedKey::F7)),
        SK_F8 => (PhysicalKey::F8, LogicalKey::Named(NamedKey::F8)),
        SK_F9 => (PhysicalKey::F9, LogicalKey::Named(NamedKey::F9)),
        SK_F10 => (PhysicalKey::F10, LogicalKey::Named(NamedKey::F10)),
        SK_F11 => (PhysicalKey::F11, LogicalKey::Named(NamedKey::F11)),
        SK_F12 => (PhysicalKey::F12, LogicalKey::Named(NamedKey::F12)),
        // Punctuation — physical drives the Flash keyCode, logical the char.
        SK_MINUS => (PhysicalKey::Minus, LogicalKey::Character('-')),
        SK_EQUALS => (PhysicalKey::Equal, LogicalKey::Character('=')),
        SK_LBRACKET => (PhysicalKey::BracketLeft, LogicalKey::Character('[')),
        SK_RBRACKET => (PhysicalKey::BracketRight, LogicalKey::Character(']')),
        SK_SEMICOLON => (PhysicalKey::Semicolon, LogicalKey::Character(';')),
        SK_QUOTE => (PhysicalKey::Quote, LogicalKey::Character('\'')),
        SK_COMMA => (PhysicalKey::Comma, LogicalKey::Character(',')),
        SK_PERIOD => (PhysicalKey::Period, LogicalKey::Character('.')),
        SK_SLASH => (PhysicalKey::Slash, LogicalKey::Character('/')),
        SK_BACKSLASH => (PhysicalKey::Backslash, LogicalKey::Character('\\')),
        SK_BACKQUOTE => (PhysicalKey::Backquote, LogicalKey::Character('`')),
        // Numpad operators (KeyLocation::Numpad below gives them the keypad codes).
        SK_NUMPAD_ADD => (PhysicalKey::NumpadAdd, LogicalKey::Character('+')),
        SK_NUMPAD_SUB => (PhysicalKey::NumpadSubtract, LogicalKey::Character('-')),
        SK_NUMPAD_MUL => (PhysicalKey::NumpadMultiply, LogicalKey::Character('*')),
        SK_NUMPAD_DIV => (PhysicalKey::NumpadDivide, LogicalKey::Character('/')),
        SK_NUMPAD_DECIMAL => (PhysicalKey::NumpadDecimal, LogicalKey::Character('.')),
        SK_NUMPAD_ENTER => (PhysicalKey::NumpadEnter, LogicalKey::Named(NamedKey::Enter)),
        _ => return None,
    };
    let key_location = if (SK_NUMPAD0..=SK_NUMPAD9).contains(&code)
        || (SK_NUMPAD_ADD..=SK_NUMPAD_ENTER).contains(&code)
    {
        KeyLocation::Numpad
    } else {
        KeyLocation::Standard
    };
    Some(KeyDescriptor {
        physical_key: physical,
        logical_key: logical,
        key_location,
    })
}

/// Forward a key event from the C++ side. `code` is one of the `SK_*`
/// constants above. `down = true` for press, `false` for release.
#[no_mangle]
pub extern "C" fn ruffle_handle_key(code: c_int, down: bool) {
    if code == SK_NONE {
        return;
    }
    let Some(key) = key_descriptor(code) else {
        return;
    };
    let state = unsafe {
        match (*core::ptr::addr_of_mut!(STATE)).as_mut() {
            Some(s) => s,
            None => return,
        }
    };
    // Ruffle dispatches an AVM1 button `keyPress` (and `onClipEvent(keyPress)`)
    // for a PRINTABLE character from a TextInput event — only special keys
    // (arrows, Enter, Escape, …) come from KeyDown (see
    // `ButtonKeyCode::from_input_event` in ruffle_core::events). We synthesise
    // keys from controller buttons and emit ONLY KeyDown/KeyUp, so a button
    // bound to a letter/digit/space fed `Key.isDown` polling (movement, pickup)
    // but never fired a game's char keyPress handler — e.g. Scooby-Doo: Mayan
    // Monster Mayhem reads H (help) and S/T (switch inventory object) as
    // `keyPress` conditions, so they did nothing no matter which button you
    // mapped to them, while arrow movement + space-as-isDown worked. Emit the
    // matching TextInput right after the KeyDown, exactly as a real keyboard
    // would, so those keyPress handlers fire. Press only — TextInput has no up.
    let text_codepoint = if down {
        match &key.logical_key {
            LogicalKey::Character(c) if ('\u{20}'..='\u{7e}').contains(c) => Some(*c),
            _ => None,
        }
    } else {
        None
    };
    let event = if down {
        PlayerEvent::KeyDown { key }
    } else {
        PlayerEvent::KeyUp { key }
    };
    if let Ok(mut p) = state.player.lock() {
        p.handle_event(event);
        if let Some(codepoint) = text_codepoint {
            p.handle_event(PlayerEvent::TextInput { codepoint });
        }
    }
}

/// Move the virtual cursor to `(x, y)` in screen pixels and forward a
/// `MouseMove` event to the Player. Called from C++ when the right stick
/// is deflected or a touch event happens.
#[no_mangle]
pub extern "C" fn ruffle_handle_mouse_move(x: c_int, y: c_int) {
    let state = unsafe {
        match (*core::ptr::addr_of_mut!(STATE)).as_mut() {
            Some(s) => s,
            None => return,
        }
    };
    // The C++ side speaks in PHYSICAL screen pixels; the player thinks in the
    // logical viewport, which is portrait and turned. Undo the turn here, at the
    // single point where the outside world's coordinates come in -- anything
    // further downstream would have to know about rotation too.
    let px = x.clamp(0, VIEWPORT_W as c_int) as f32;
    let py = y.clamp(0, VIEWPORT_H as c_int) as f32;
    let pw = VIEWPORT_W as f32;
    let ph = VIEWPORT_H as f32;
    let unrotate = |ax: f32, ay: f32| match crate::backend::render::game_rotation() {
        1 => (ay, pw - ax),
        2 => (pw - ax, ph - ay),
        3 => (ph - ay, ax),
        _ => (ax, ay),
    };
    // Where to DRAW the crosshair: exactly where the stick put it.
    let (cx, cy) = unrotate(px, py);
    state.cursor_x = cx;
    state.cursor_y = cy;
    // What it POINTS AT: undo the free zoom first, in physical screen space,
    // because that is the order `world_matrix` composed it in (zoom after turn).
    // Skipping this is not a subtle error -- at 200% every click lands at half
    // its distance from the middle of the screen.
    let zp = crate::backend::render::game_zoom_percent();
    let (sx, sy) = if zp == 100 {
        (px, py)
    } else {
        let z = zp as f32 / 100.0;
        let (ox, oy) = crate::backend::render::game_pan();
        (
            (px - ox as f32 - pw * 0.5 * (1.0 - z)) / z,
            (py - oy as f32 - ph * 0.5 * (1.0 - z)) / z,
        )
    };
    let (gx, gy) = unrotate(sx, sy);
    state.cursor_stage_x = gx;
    state.cursor_stage_y = gy;
    if let Ok(mut p) = state.player.lock() {
        p.handle_event(PlayerEvent::MouseMove {
            x: gx as f64,
            y: gy as f64,
        });
    }
}

/// Click / release the left mouse button at the current cursor position.
/// `down = true` for press, `false` for release.
fn handle_mouse_button_impl(button: MouseButton, down: bool) {
    let state = unsafe {
        match (*core::ptr::addr_of_mut!(STATE)).as_mut() {
            Some(s) => s,
            None => return,
        }
    };
    state.cursor_clicked = down;
    // The STAGE point, not the screen one: see `cursor_stage_x`.
    let x = state.cursor_stage_x as f64;
    let y = state.cursor_stage_y as f64;
    if let Ok(mut p) = state.player.lock() {
        let event = if down {
            PlayerEvent::MouseDown {
                x,
                y,
                button,
                index: None,
            }
        } else {
            PlayerEvent::MouseUp { x, y, button }
        };
        p.handle_event(event);
    }
}

/// Left mouse button at the cursor. `down = true` press, `false` release.
#[no_mangle]
pub extern "C" fn ruffle_handle_mouse_button(down: bool) {
    handle_mouse_button_impl(MouseButton::Left, down);
}

/// Right mouse button at the cursor (a button bound to "Right click"). Lets Flash
/// games that use the right click (context actions, secondary fire) be played
/// without a physical mouse.
#[no_mangle]
pub extern "C" fn ruffle_handle_mouse_right(down: bool) {
    handle_mouse_button_impl(MouseButton::Right, down);
}

/// Returns 1 (clearing the flag) if Ruffle's focus tracker asked us to raise
/// the software keyboard since the last poll — i.e. an editable TextField just
/// gained focus (the user clicked it, re-clicked it, or it was focused by AS
/// code). Returns 0 otherwise. The C++ game loop polls this once per frame and,
/// when set, runs swkbd via `ruffle_keyboard_field` + `ruffle_keyboard_submit`.
#[no_mangle]
pub extern "C" fn ruffle_keyboard_take_request() -> c_int {
    if backend::ui::take_keyboard_request() {
        1
    } else {
        0
    }
}

/// Raise the same request by hand, from a button bound to `SK_KEYBOARD`. The
/// automatic path only fires for a field Ruffle tracks as focused; this one does
/// not care, which is the entire point of having it.
#[no_mangle]
pub extern "C" fn ruffle_keyboard_request_manual() {
    backend::ui::request_keyboard_manual();
}

/// Type `text` into the movie as if it came from a keyboard: for each character
/// a KeyDown, the matching TextInput, then a KeyUp.
///
/// This is the fallback when the manual keyboard opens with no editable field
/// focused, where `ruffle_keyboard_submit` has nothing to write into. TextInput
/// is what an EditText consumes and also what fires an AVM1 `keyPress` handler
/// (see the note in `ruffle_handle_key`), so a game with its own text box still
/// receives what was typed. Returns the number of characters sent.
#[no_mangle]
pub extern "C" fn ruffle_keyboard_type_text(text: *const c_char) -> c_int {
    if text.is_null() {
        return 0;
    }
    let Ok(s) = (unsafe { core::ffi::CStr::from_ptr(text) }).to_str() else {
        return 0;
    };
    let state = unsafe {
        match (*core::ptr::addr_of_mut!(STATE)).as_mut() {
            Some(s) => s,
            None => return 0,
        }
    };
    let Ok(mut p) = state.player.lock() else {
        return 0;
    };
    let mut sent = 0;
    for c in s.chars() {
        // The key half is best-effort: only characters that map to one of our
        // Switch key codes carry a KeyDown/KeyUp, the rest are text only.
        let key = char_key_code(c).and_then(key_descriptor);
        // With no field focused, the only consumer left is the AVM1 keyPress
        // handler, and the SWF format defines that over ASCII 32..126 only
        // (`ButtonKeyCode::from_input_event` refuses anything else). So a
        // character typed in Chinese lands nowhere on THIS path, and the count
        // has to say so: the keyboard accepts the input (issue #75), a game
        // with a real text field receives it through `ruffle_keyboard_submit`,
        // and here it is silently dropped. Counting it as sent would be a lie
        // the caller cannot check.
        let deliverable = key.is_some() || matches!(c as u32, 32..=126);
        if let Some(k) = key.clone() {
            p.handle_event(PlayerEvent::KeyDown { key: k });
        }
        p.handle_event(PlayerEvent::TextInput { codepoint: c });
        if let Some(k) = key {
            p.handle_event(PlayerEvent::KeyUp { key: k });
        }
        if deliverable {
            sent += 1;
        }
    }
    sent
}

/// Switch key code for a printable character, when one exists. Only the keys the
/// mapper already knows about: letters (typed unshifted), digits, space.
fn char_key_code(c: char) -> Option<c_int> {
    let code = match c.to_ascii_lowercase() {
        ' ' => SK_SPACE,
        '\n' | '\r' => SK_ENTER,
        'a' => SK_A, 'b' => SK_B, 'c' => SK_C, 'd' => SK_D, 'e' => SK_E,
        'f' => SK_F, 'g' => SK_G, 'h' => SK_H, 'i' => SK_I, 'j' => SK_J,
        'k' => SK_K, 'l' => SK_L, 'm' => SK_M, 'n' => SK_N, 'o' => SK_O,
        'p' => SK_P, 'q' => SK_Q, 'r' => SK_R, 's' => SK_S, 't' => SK_T,
        'u' => SK_U, 'v' => SK_V, 'w' => SK_W, 'x' => SK_X, 'y' => SK_Y,
        'z' => SK_Z,
        '0' => SK_0, '1' => SK_1, '2' => SK_2, '3' => SK_3, '4' => SK_4,
        '5' => SK_5, '6' => SK_6, '7' => SK_7, '8' => SK_8, '9' => SK_9,
        _ => return None,
    };
    Some(code)
}

/// Describe the focused editable TextField so C++ can configure swkbd to match
/// it. Pre-fills `out` with the field's current text (UTF-8, NUL-terminated,
/// truncated to `cap` on a char boundary), sets `*out_flags` (bit0 = password,
/// bit1 = multiline, bit2 = digits-only → numeric keypad) and `*out_max` (the
/// field's max char count, 0 = unlimited). Returns 1 if a focused editable
/// field exists, 0 otherwise (in which case the outputs are left untouched).
#[no_mangle]
pub extern "C" fn ruffle_keyboard_field(
    out: *mut c_char,
    cap: c_int,
    out_flags: *mut c_int,
    out_max: *mut c_int,
) -> c_int {
    if out.is_null() || cap < 1 {
        return 0;
    }
    let state = unsafe {
        match (*core::ptr::addr_of_mut!(STATE)).as_mut() {
            Some(s) => s,
            None => return 0,
        }
    };
    let Ok(mut p) = state.player.lock() else {
        return 0;
    };
    let info = p.mutate_with_update_context(|context| {
        let et = context.focus_tracker.get_as_edit_text()?;
        if !et.is_editable() {
            return None;
        }
        let text = et.text().to_utf8_lossy().into_owned();
        let mut flags: c_int = 0;
        if et.is_password() {
            flags |= 1;
        }
        if et.is_multiline() {
            flags |= 2;
        }
        // A digits-only `restrict` is a strong hint that the field wants a
        // numeric keypad (high-score initials are letters, but level passwords
        // and numeric entries are often restricted to 0-9).
        if let Some(r) = et.restrict() {
            let r = r.to_utf8_lossy();
            if !r.is_empty() && r.chars().all(|c| c.is_ascii_digit()) {
                flags |= 4;
            }
        }
        Some((text, flags, et.max_chars()))
    });
    let Some((text, flags, max_chars)) = info else {
        return 0;
    };
    // Pre-fill: truncate to cap-1 bytes ending on a UTF-8 char boundary.
    let bytes = text.as_bytes();
    let mut n = bytes.len().min(cap as usize - 1);
    while n > 0 && !text.is_char_boundary(n) {
        n -= 1;
    }
    unsafe {
        core::ptr::copy_nonoverlapping(bytes.as_ptr(), out as *mut u8, n);
        *out.add(n) = 0;
        if !out_flags.is_null() {
            *out_flags = flags;
        }
        if !out_max.is_null() {
            *out_max = max_chars;
        }
    }
    1
}

/// Replace the focused editable TextField's content with `text` (UTF-8, the
/// string swkbd returned). Routed through Ruffle's normal text events
/// (select-all, then per-character TextInput) so the SWF's change handlers fire
/// exactly as if the user had typed — many games update on TextInput/onChanged.
/// Newlines map to Enter (for multiline fields). Returns 0 on success, -1 if no
/// editable field is focused (avoids a stray select-all hitting the stage).
#[no_mangle]
pub extern "C" fn ruffle_keyboard_submit(text: *const c_char) -> c_int {
    if text.is_null() {
        return -1;
    }
    let s = unsafe { core::ffi::CStr::from_ptr(text) };
    let Ok(s) = s.to_str() else {
        return -1;
    };
    let state = unsafe {
        match (*core::ptr::addr_of_mut!(STATE)).as_mut() {
            Some(s) => s,
            None => return -1,
        }
    };
    let Ok(mut p) = state.player.lock() else {
        return -1;
    };
    let has_field = p.mutate_with_update_context(|context| {
        context
            .focus_tracker
            .get_as_edit_text()
            .is_some_and(|et| et.is_editable())
    });
    if !has_field {
        return -1;
    }
    // Select the whole field, then retype: the first inserted character
    // replaces the selection and the rest append. If `text` is empty we delete
    // the selection instead, clearing the field.
    p.handle_event(PlayerEvent::TextControl {
        code: TextControlCode::SelectAll,
    });
    if s.is_empty() {
        p.handle_event(PlayerEvent::TextControl {
            code: TextControlCode::Backspace,
        });
    } else {
        for c in s.chars() {
            match c {
                '\r' => {}
                '\n' => {
                    p.handle_event(PlayerEvent::TextControl {
                        code: TextControlCode::Enter,
                    });
                }
                _ => {
                    p.handle_event(PlayerEvent::TextInput { codepoint: c });
                }
            }
        }
    }
    0
}

#[no_mangle]
pub extern "C" fn ruffle_shutdown() {
    unsafe {
        STATE = None;
    }
    // The ExternalInterface tables belong to the MOVIE, not to the process, and
    // these two outlived it: the launcher tears the player down between games
    // and builds a new one, so game B started with game A's registered callback
    // names still listed and, worse, with anything still queued for A waiting to
    // be called on B's first frame. `queue_container_callback` matches by name
    // without case, so a name two games happen to share was enough to fire a
    // callback into a movie that never asked for it.
    if let Ok(mut c) = EI_CALLBACKS.lock() {
        c.clear();
    }
    if let Ok(mut q) = EI_PENDING.lock() {
        q.clear();
    }
}

#[no_mangle]
pub unsafe extern "Rust" fn __getrandom_v03_custom(
    dest: *mut u8,
    len: usize,
) -> Result<(), getrandom::Error> {
    use core::sync::atomic::{AtomicU64, Ordering};
    static SEED: AtomicU64 = AtomicU64::new(0x9E3779B97F4A7C15);
    let mut state = SEED.load(Ordering::Relaxed);
    for i in 0..len {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        *dest.add(i) = (state.wrapping_mul(0x2545F4914F6CDD1D) >> 32) as u8;
    }
    SEED.store(state, Ordering::Relaxed);
    Ok(())
}

fn log(msg_nul: &[u8]) {
    unsafe { ruffle_log_cstr(msg_nul.as_ptr() as *const c_char) };
}

fn log_str(s: &str) {
    let mut bytes = s.as_bytes().to_vec();
    bytes.push(0);
    unsafe { ruffle_log_cstr(bytes.as_ptr() as *const c_char) };
}

/// Turn the small-object cache off for the whole process. Called from C++ at
/// boot when `sdmc:/switch/FlashNX/noalloc.on` exists, before the worker thread
/// starts — so before any Rust allocation, and therefore before the region
/// would have been reserved. Exists so a game can be measured with and without
/// the cache without swapping builds.
#[no_mangle]
pub extern "C" fn ruffle_alloc_force_off() {
    counting_alloc::FORCE_OFF.store(true, std::sync::atomic::Ordering::Relaxed);
}
