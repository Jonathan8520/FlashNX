// Big blocks live at the top of the heap, everything else at the bottom.
//
// Why: libdrm_nouveau takes every GPU buffer (textures, vertex arenas) with
// memalign(0x1000, size) from the same newlib heap as every small C object,
// and our Rust allocator took its big blocks there too. After an hour of Super
// Smash Flash 2 (2026-10-05) that heap had ~95 MB free but not one 16 MB run,
// and a 16 MB allocation killed the app, while 2000 textures had already been
// refused (invisible sprites). Small blocks nibble the holes big blocks leave.
//
// So page-aligned requests of BH_BIG bytes or more (GPU buffers, thread stacks,
// the Rust allocator's big blocks, its small-object chunks and its dlmalloc
// segments) go to a region that grows DOWN from the end of the heap
// (flashnx_bigheap.c), while newlib keeps growing UP through sbrk, which is
// stopped at the region's lowest address. The boundary moves both ways: a
// freed block at the bottom of the region hands its space back to newlib.
// The same layout as autorun's horizon-wine (native_heap.c), which met the
// same GPU-buffer fragmentation on libnx.
//
// newlib's malloc itself is untouched; only these entry points are wrapped
// (cpp/Makefile, -Wl,--wrap): _sbrk_r, _memalign_r, _free_r, _realloc_r,
// _malloc_usable_size_r, _mallinfo_r. The public names (memalign,
// aligned_alloc, posix_memalign, free, realloc, ...) all go through them.
// Everything runs under newlib's own recursive malloc lock. Nothing here logs:
// the log allocates, and it would do so under that lock.
#include <errno.h>
#include <malloc.h>
#include <stdint.h>
#include <string.h>
#include <sys/reent.h>

#include "flashnx_bigheap.h"

extern char *fake_heap_start, *fake_heap_end;
extern void *__real__sbrk_r(struct _reent *, ptrdiff_t);
extern void *__real__memalign_r(struct _reent *, size_t, size_t);
extern void __real__free_r(struct _reent *, void *);
extern void *__real__realloc_r(struct _reent *, void *, size_t);
extern size_t __real__malloc_usable_size_r(struct _reent *, void *);
extern struct mallinfo __real__mallinfo_r(struct _reent *);

// Page-aligned blocks at least this big go to the top. Smaller ones would
// waste up to a unit each and are not what fragments the heap.
#define BH_BIG ((size_t)256 * 1024)
// Enough units for a 4 GB heap.
#define BH_MAX_UNITS ((uint32_t)(((uint64_t)4 << 30) / BH_UNIT))

static uint32_t s_len_at[BH_MAX_UNITS];
static bh_run s_runs[BH_MAX_UNITS / 2 + 2];
static bigheap s_heap;
static int s_ready;
// Lowest address of the region, read without the lock by free(): a live block
// is always at or above it, and newlib's memory always below.
static uintptr_t s_low = UINTPTR_MAX;
static uintptr_t s_end;
// Counters for the heartbeat (flashnx_heap_stats).
static uint64_t s_refused;   // big requests the region could not serve
static uint64_t s_fallbacks; // of which newlib then served

static void init_locked(void) {
    if (s_ready) {
        return;
    }
    uintptr_t start = ((uintptr_t)fake_heap_start + BH_UNIT - 1) & ~(BH_UNIT - 1);
    uintptr_t end = (uintptr_t)fake_heap_end & ~(BH_UNIT - 1);
    if (end <= start) {
        return;
    }
    uint64_t units = (end - start) / BH_UNIT;
    if (units > BH_MAX_UNITS) {
        units = BH_MAX_UNITS;
    }
    bh_init(&s_heap, end, (uint32_t)units, s_len_at, s_runs, BH_MAX_UNITS / 2 + 2);
    s_end = end;
    __atomic_store_n(&s_low, end, __ATOMIC_RELEASE);
    s_ready = 1;
}

static void publish_low(void) {
    __atomic_store_n(&s_low, bh_low(&s_heap), __ATOMIC_RELEASE);
}

static int ours(const void *p) {
    uintptr_t a = (uintptr_t)p;
    return a >= __atomic_load_n(&s_low, __ATOMIC_ACQUIRE) && a < s_end;
}

void *__wrap__sbrk_r(struct _reent *r, ptrdiff_t incr) {
    void *res;
    __malloc_lock(r);
    init_locked();
    uintptr_t cur = (uintptr_t)__real__sbrk_r(r, 0);
    uintptr_t limit = s_ready ? bh_low(&s_heap) : (uintptr_t)fake_heap_end;
    if (incr > 0 && (cur > limit || (uintptr_t)incr > limit - cur)) {
        r->_errno = ENOMEM;
        res = (void *)-1;
    } else {
        res = __real__sbrk_r(r, incr);
    }
    __malloc_unlock(r);
    return res;
}

// A block from the top, or NULL. Caller holds the lock.
static void *big_alloc_locked(struct _reent *r, size_t size) {
    init_locked();
    if (!s_ready) {
        return NULL;
    }
    uintptr_t floor = (uintptr_t)__real__sbrk_r(r, 0);
    void *p = bh_alloc(&s_heap, size, floor);
    if (p) {
        publish_low();
    } else {
        s_refused++;
    }
    return p;
}

static int wants_top(size_t align, size_t size) {
    return align >= 4096 && align <= BH_UNIT && (align & (align - 1)) == 0 && size >= BH_BIG;
}

void *__wrap__memalign_r(struct _reent *r, size_t align, size_t size) {
    if (wants_top(align, size)) {
        __malloc_lock(r);
        void *p = big_alloc_locked(r, size);
        __malloc_unlock(r);
        if (p) {
            return p;
        }
        // The region could not grow; newlib may still have a hole that fits.
        void *q = __real__memalign_r(r, align, size);
        if (q) {
            __malloc_lock(r);
            s_fallbacks++;
            __malloc_unlock(r);
        }
        return q;
    }
    return __real__memalign_r(r, align, size);
}

void __wrap__free_r(struct _reent *r, void *p) {
    if (p && ours(p)) {
        __malloc_lock(r);
        int rc = bh_free(&s_heap, p);
        publish_low();
        __malloc_unlock(r);
        if (rc == 0) {
            return;
        }
        // Not the start of one of our blocks: not ours after all (cannot
        // happen for a pointer malloc returned). Leave it alone rather than
        // hand newlib an address inside our region.
        return;
    }
    __real__free_r(r, p);
}

size_t __wrap__malloc_usable_size_r(struct _reent *r, void *p) {
    if (p && ours(p)) {
        __malloc_lock(r);
        size_t n = bh_size(&s_heap, p);
        __malloc_unlock(r);
        return n;
    }
    return __real__malloc_usable_size_r(r, p);
}

void *__wrap__realloc_r(struct _reent *r, void *p, size_t size) {
    if (!p || !ours(p)) {
        return __real__realloc_r(r, p, size);
    }
    if (size == 0) {
        __wrap__free_r(r, p);
        return NULL;
    }
    __malloc_lock(r);
    size_t old = bh_size(&s_heap, p);
    __malloc_unlock(r);
    if (size <= old && size >= BH_BIG) {
        return p;
    }
    void *q = size >= BH_BIG ? __wrap__memalign_r(r, 4096, size) : _malloc_r(r, size);
    if (!q) {
        return NULL; // the old block stays valid, as realloc promises
    }
    memcpy(q, p, old < size ? old : size);
    __wrap__free_r(r, p);
    return q;
}

struct mallinfo __wrap__mallinfo_r(struct _reent *r) {
    struct mallinfo info = __real__mallinfo_r(r);
    __malloc_lock(r);
    if (s_ready) {
        size_t reserved = (size_t)(s_heap.units - s_heap.bottom) * BH_UNIT;
        size_t used = (size_t)s_heap.used * BH_UNIT;
        info.arena += reserved;
        info.uordblks += used;
        info.fordblks += reserved - used;
        info.ordblks += s_heap.nruns;
    }
    __malloc_unlock(r);
    return info;
}

// For the heartbeat: [0] region bytes, [1] bytes in blocks, [2] blocks,
// [3] holes, [4] biggest block the region could hand out now, [5] room between
// newlib's break and the region, [6] big requests refused, [7] of which newlib
// served. No allocation, no logging.
void flashnx_heap_stats(uint64_t *out) {
    struct _reent *r = _REENT;
    __malloc_lock(r);
    init_locked();
    if (s_ready) {
        uintptr_t floor = (uintptr_t)__real__sbrk_r(r, 0);
        uintptr_t low = bh_low(&s_heap);
        out[0] = (uint64_t)(s_heap.units - s_heap.bottom) * BH_UNIT;
        out[1] = (uint64_t)s_heap.used * BH_UNIT;
        out[2] = s_heap.blocks;
        out[3] = s_heap.nruns;
        out[4] = bh_largest(&s_heap, floor);
        out[5] = low > floor ? low - floor : 0;
    } else {
        memset(out, 0, 6 * sizeof(uint64_t));
    }
    out[6] = s_refused;
    out[7] = s_fallbacks;
    __malloc_unlock(r);
}
