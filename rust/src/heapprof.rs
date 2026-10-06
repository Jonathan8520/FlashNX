//! Heap profiler: who holds the memory that is alive right now.
//!
//! Why: Super Smash Flash 2 now runs its whole Classic mode and dies on a
//! second run with the heap genuinely full, ~1.3 GB of it small Rust blocks
//! (the small-object region plus dlmalloc), and no counter says whose. The
//! sampling profiler (cpp/src/prof.cpp) says where TIME goes; this says where
//! the BYTES are.
//!
//! How: one allocation is sampled every ~INTERVAL bytes allocated (a shared
//! countdown), its call stack taken from the frame-pointer chain and kept in a
//! fixed table until that block is freed. Every `dump` groups the live samples
//! by stack and writes them to `sdmc:/switch/FlashNX/heapprof.bin`;
//! `scripts/heapprof_report.py` turns that and the build's FlashNX.elf into a
//! ranking. A sample stands for max(its size, INTERVAL) bytes.
//!
//! Only in the `heapprof` build (`scripts/build.sh --memprof`, which also adds
//! the frame pointers this needs). Runs inside the global allocator, so it
//! never allocates: static tables, a spinlock, nothing else.

use core::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, AtomicU8, Ordering};

const INTERVAL: i64 = 64 * 1024;
const SLOTS: usize = 1 << 17;
const DEPTH: usize = 20;
const FILTER_BITS: u32 = 20;
const AGG: usize = 8192;

#[derive(Clone, Copy)]
struct Slot {
    ptr: usize, // 0 = empty
    weight: u64,
    depth: u8,
    frames: [u32; DEPTH],
}

const EMPTY: Slot = Slot { ptr: 0, weight: 0, depth: 0, frames: [0; DEPTH] };

struct Table(core::cell::UnsafeCell<[Slot; SLOTS]>);
unsafe impl Sync for Table {}
static TABLE: Table = Table(core::cell::UnsafeCell::new([EMPTY; SLOTS]));

#[derive(Clone, Copy)]
struct Bucket {
    used: bool,
    hash: u64,
    bytes: u64,
    count: u32,
    depth: u8,
    frames: [u32; DEPTH],
}
const NO_BUCKET: Bucket = Bucket { used: false, hash: 0, bytes: 0, count: 0, depth: 0, frames: [0; DEPTH] };
struct Buckets(core::cell::UnsafeCell<[Bucket; AGG]>);
unsafe impl Sync for Buckets {}
static BUCKETS: Buckets = Buckets(core::cell::UnsafeCell::new([NO_BUCKET; AGG]));

/// How many samples may sit in each filter cell: a free whose cell reads 0
/// was never sampled and skips the table (and its lock) altogether.
static FILTER: [AtomicU8; 1 << FILTER_BITS] = [const { AtomicU8::new(0) }; 1 << FILTER_BITS];
static LOCK: AtomicBool = AtomicBool::new(false);
static NEXT: AtomicI64 = AtomicI64::new(INTERVAL);
static LIVE: AtomicU64 = AtomicU64::new(0);
static DROPPED: AtomicU64 = AtomicU64::new(0);

extern "C" {
    /// Address 0 of the ELF (switch.ld), so `ret - base` is what addr2line knows.
    static _start: u8;
    /// End of the mapping that holds `sp` (cpp/src/ruffle_bridge.cpp), the
    /// ceiling of the frame-pointer walk.
    fn flashnx_stack_ceiling(sp: usize) -> usize;
    /// The SD writer every other file goes through (cpp/src/swf_picker.cpp).
    fn swf_picker_write_file(path: *const core::ffi::c_char, data: *const u8, len: u32) -> core::ffi::c_int;
}

#[inline(always)]
fn lock() {
    while LOCK.compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed).is_err() {
        core::hint::spin_loop();
    }
}
#[inline(always)]
fn unlock() {
    LOCK.store(false, Ordering::Release);
}

#[inline(always)]
fn mix(p: usize) -> u64 {
    (p as u64 >> 4).wrapping_mul(0x9E37_79B9_7F4A_7C15)
}
#[inline(always)]
fn filter_cell(p: usize) -> usize {
    (mix(p) >> (64 - FILTER_BITS)) as usize
}
#[inline(always)]
fn home(p: usize) -> usize {
    (mix(p ^ 0x5555) >> (64 - 17)) as usize & (SLOTS - 1)
}

/// The caller's return addresses, innermost first, as image offsets. Skips
/// this function and the allocator's own frames.
#[inline(never)]
fn capture(out: &mut [u32; DEPTH]) -> u8 {
    let (mut fp, sp): (usize, usize);
    unsafe {
        core::arch::asm!("mov {}, x29", out(reg) fp, options(nomem, nostack));
        core::arch::asm!("mov {}, sp", out(reg) sp, options(nomem, nostack));
    }
    let base = unsafe { &_start as *const u8 as usize };
    let ceil = unsafe { flashnx_stack_ceiling(sp) };
    let mut floor = sp;
    let mut n = 0usize;
    let mut skip = 2; // capture, on_alloc
    while n < DEPTH && fp & 7 == 0 && fp >= floor && fp.saturating_add(16) <= ceil {
        let next = unsafe { *(fp as *const usize) };
        let ret = unsafe { *((fp + 8) as *const usize) };
        if ret < base || ret - base >= 1 << 32 {
            break;
        }
        if skip > 0 {
            skip -= 1;
        } else {
            out[n] = (ret - base) as u32;
            n += 1;
        }
        floor = fp + 16;
        fp = next;
    }
    n as u8
}

/// A block of `size` bytes was just handed out at `p`.
#[inline(always)]
pub fn on_alloc(p: *mut u8, size: usize) {
    if p.is_null() {
        return;
    }
    let left = NEXT.fetch_sub(size as i64, Ordering::Relaxed) - size as i64;
    if left > 0 {
        return;
    }
    NEXT.store(INTERVAL, Ordering::Relaxed);
    record(p as usize, core::cmp::max(size as u64, INTERVAL as u64));
}

#[inline(never)]
fn record(p: usize, weight: u64) {
    let mut frames = [0u32; DEPTH];
    let depth = capture(&mut frames);
    lock();
    let t = unsafe { &mut *TABLE.0.get() };
    if LIVE.load(Ordering::Relaxed) as usize >= SLOTS * 3 / 4 {
        DROPPED.fetch_add(1, Ordering::Relaxed);
        unlock();
        return;
    }
    let mut i = home(p);
    loop {
        if t[i].ptr == 0 || t[i].ptr == p {
            if t[i].ptr == 0 {
                LIVE.fetch_add(1, Ordering::Relaxed);
                let c = &FILTER[filter_cell(p)];
                let v = c.load(Ordering::Relaxed);
                if v < 255 {
                    c.store(v + 1, Ordering::Relaxed);
                }
            }
            t[i] = Slot { ptr: p, weight, depth, frames };
            break;
        }
        i = (i + 1) & (SLOTS - 1);
    }
    unlock();
}

/// The block at `p` is being given back.
#[inline(always)]
pub fn on_free(p: *mut u8) {
    let p = p as usize;
    if p == 0 || FILTER[filter_cell(p)].load(Ordering::Relaxed) == 0 {
        return;
    }
    forget(p);
}

#[inline(never)]
fn forget(p: usize) {
    lock();
    let t = unsafe { &mut *TABLE.0.get() };
    let mut i = home(p);
    loop {
        if t[i].ptr == 0 {
            unlock();
            return; // filtered in by another sample sharing the cell
        }
        if t[i].ptr == p {
            break;
        }
        i = (i + 1) & (SLOTS - 1);
    }
    t[i] = EMPTY;
    LIVE.fetch_sub(1, Ordering::Relaxed);
    let c = &FILTER[filter_cell(p)];
    let v = c.load(Ordering::Relaxed);
    if v > 0 && v < 255 {
        c.store(v - 1, Ordering::Relaxed);
    }
    // Linear-probing removal: pull later entries of the run back into the hole
    // when their home slot does not lie strictly between the hole and them.
    let mut hole = i;
    let mut j = (i + 1) & (SLOTS - 1);
    while t[j].ptr != 0 {
        let h = home(t[j].ptr);
        let between = if hole <= j { hole < h && h <= j } else { hole < h || h <= j };
        if !between {
            t[hole] = t[j];
            t[j] = EMPTY;
            hole = j;
        }
        j = (j + 1) & (SLOTS - 1);
    }
    unlock();
}

fn stack_hash(depth: u8, frames: &[u32; DEPTH]) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325u64 ^ depth as u64;
    for f in &frames[..depth as usize] {
        h = (h ^ *f as u64).wrapping_mul(0x0100_0000_01b3);
    }
    h
}

/// Group the live samples by stack and write them to the SD card. Called from
/// the render heartbeat, outside the allocator; the aggregation runs under the
/// table lock without allocating, the serialization after it.
pub fn dump(path: &str) {
    lock();
    let t = unsafe { &*TABLE.0.get() };
    let b = unsafe { &mut *BUCKETS.0.get() };
    for x in b.iter_mut() {
        x.used = false;
    }
    let mut nb = 0usize;
    let mut total = 0u64;
    let mut other = (0u64, 0u32);
    for s in t.iter() {
        if s.ptr == 0 {
            continue;
        }
        total += s.weight;
        let h = stack_hash(s.depth, &s.frames);
        let mut i = (h as usize) & (AGG - 1);
        let mut placed = false;
        for _ in 0..AGG {
            if !b[i].used {
                if nb >= AGG * 3 / 4 {
                    break;
                }
                b[i] = Bucket { used: true, hash: h, bytes: 0, count: 0, depth: s.depth, frames: s.frames };
                nb += 1;
            }
            if b[i].hash == h && b[i].depth == s.depth && b[i].frames == s.frames {
                b[i].bytes += s.weight;
                b[i].count += 1;
                placed = true;
                break;
            }
            i = (i + 1) & (AGG - 1);
        }
        if !placed {
            other.0 += s.weight;
            other.1 += 1;
        }
    }
    unlock();
    // Serialize (allocating is fine here: we are not inside the allocator).
    let mut out: std::vec::Vec<u8> = std::vec::Vec::with_capacity(32 + nb * (13 + 4 * DEPTH));
    out.extend_from_slice(b"FNXHEAP1");
    out.extend_from_slice(&(INTERVAL as u64).to_le_bytes());
    out.extend_from_slice(&LIVE.load(Ordering::Relaxed).to_le_bytes());
    out.extend_from_slice(&DROPPED.load(Ordering::Relaxed).to_le_bytes());
    out.extend_from_slice(&total.to_le_bytes());
    out.extend_from_slice(&other.0.to_le_bytes());
    out.extend_from_slice(&(nb as u32).to_le_bytes());
    for x in b.iter() {
        if !x.used {
            continue;
        }
        out.extend_from_slice(&x.bytes.to_le_bytes());
        out.extend_from_slice(&x.count.to_le_bytes());
        out.push(x.depth);
        for f in &x.frames[..x.depth as usize] {
            out.extend_from_slice(&f.to_le_bytes());
        }
    }
    let mut cpath = std::vec::Vec::from(path.as_bytes());
    cpath.push(0);
    unsafe {
        swf_picker_write_file(cpath.as_ptr() as *const _, out.as_ptr(), out.len() as u32);
    }
}
