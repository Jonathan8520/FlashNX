// The top of the heap, for big blocks only (see flashnx_heap.c for why).
//
// A region that grows DOWN from the end of the heap in units of BH_UNIT bytes,
// while newlib's own heap grows up from the start through sbrk. Blocks are whole
// units, so no two blocks ever share a page. Nothing is ever written inside a
// block: the bookkeeping lives in two side tables. That matters, because libnx
// lends the pages of a thread stack to the kernel (svcMapMemory) and libdrm
// lends a buffer object's pages to the GPU; a header in the block would be read
// by one and clobbered by the other.
//
// Pure C, no locking, no libc: the caller locks. Also built on the PC by
// cpp/tests/bigheap_test.c.
#pragma once

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

#define BH_UNIT ((uintptr_t)16 * 1024)

typedef struct {
    uint32_t start; // first unit
    uint32_t len;   // in units, never 0
} bh_run;

typedef struct {
    uintptr_t base;      // address of unit 0 (BH_UNIT-aligned)
    uint32_t units;      // units in [base, base + units * BH_UNIT)
    uint32_t bottom;     // the region is [bottom, units); empty when bottom == units
    uint32_t *len_at;    // [units]: length of the block starting at a unit, else 0
    bh_run *runs;        // free runs inside the region, sorted by start
    uint32_t nruns;
    uint32_t runs_cap;
    uint32_t used;       // units held by blocks
    uint32_t blocks;     // live blocks
} bigheap;

// `end` and the tables come from the caller; `units` is how many fit below `end`
// (the caller bounds it by the start of the heap). `runs_cap` must be at least
// units / 2 + 1, which no pattern of blocks and holes can exceed.
void bh_init(bigheap *h, uintptr_t end, uint32_t units, uint32_t *len_at, bh_run *runs,
             uint32_t runs_cap);

// Lowest address the region occupies (== end when empty). Nothing below it is
// ours; newlib's break must stay at or under it.
uintptr_t bh_low(const bigheap *h);

// A block of at least `size` bytes, BH_UNIT-aligned, or NULL. The region may
// extend down to `floor` (newlib's current break) but never below it.
void *bh_alloc(bigheap *h, size_t size, uintptr_t floor);

// 0 on success, -1 if `p` is not the start of a live block (nothing changed).
int bh_free(bigheap *h, void *p);

// Bytes usable in the block starting at `p`, 0 if it is not one.
size_t bh_size(const bigheap *h, const void *p);

// Biggest block bh_alloc could hand out right now with this floor.
size_t bh_largest(const bigheap *h, uintptr_t floor);

#ifdef __cplusplus
}
#endif
