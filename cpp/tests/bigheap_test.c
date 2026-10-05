// PC stress test for cpp/src/flashnx_bigheap.c. Not part of the .nro.
//
//   gcc -O2 -Wall -I cpp/include cpp/src/flashnx_bigheap.c cpp/tests/bigheap_test.c -o bigheap_test
//
// Random allocations and frees over a fake heap, with a "newlib break" that
// moves up and down underneath the region. After every operation the whole
// state is checked: holes sorted, inside the region and never adjacent to each
// other, no unit owned twice, the counters exact. Every block is filled with a
// pattern of its own and checked when freed, so an overlap shows up as
// corruption even if the bookkeeping missed it.
#include "flashnx_bigheap.h"

#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#define UNITS ((uint32_t)((256u << 20) / BH_UNIT)) // 256 MB of fake heap
#define MAXLIVE 4096

static uint32_t len_tab[UNITS];
static bh_run runs[UNITS / 2 + 2];
static unsigned char owner[UNITS]; // 0 free, 1 block (test's own map)

typedef struct {
    unsigned char *p;
    size_t size;
    uint32_t tag;
} live_t;
static live_t live[MAXLIVE];
static int nlive;

static uint64_t rng = 88172645463325252ull;
static uint64_t next(void) {
    rng ^= rng << 13;
    rng ^= rng >> 7;
    rng ^= rng << 17;
    return rng;
}

static void fail(const char *what, long step) {
    printf("FAIL at step %ld: %s\n", step, what);
    exit(1);
}

static void fill(live_t *b) {
    // First and last 4 KB of every unit, cheap but enough to catch overlaps.
    for (size_t off = 0; off < b->size; off += BH_UNIT) {
        size_t n = b->size - off < 4096 ? b->size - off : 4096;
        memset(b->p + off, (int)(b->tag & 0xff), n);
    }
    b->p[b->size - 1] = (unsigned char)(b->tag & 0xff);
}

static int intact(const live_t *b) {
    for (size_t off = 0; off < b->size; off += BH_UNIT) {
        size_t n = b->size - off < 4096 ? b->size - off : 4096;
        for (size_t i = 0; i < n; i++) {
            if (b->p[off + i] != (unsigned char)(b->tag & 0xff)) {
                return 0;
            }
        }
    }
    return b->p[b->size - 1] == (unsigned char)(b->tag & 0xff);
}

static void check(bigheap *h, uintptr_t floor, long step) {
    memset(owner, 0, sizeof(owner));
    uint32_t used = 0;
    for (int i = 0; i < nlive; i++) {
        uintptr_t a = (uintptr_t)live[i].p;
        if ((a - h->base) % BH_UNIT) fail("misaligned block", step);
        uint32_t u = (uint32_t)((a - h->base) / BH_UNIT);
        uint32_t n = h->len_at[u];
        if (!n) fail("live block has no length", step);
        if ((size_t)n * BH_UNIT < live[i].size) fail("block smaller than asked", step);
        if (u < h->bottom || u + n > h->units) fail("block outside the region", step);
        if (bh_size(h, live[i].p) != (size_t)n * BH_UNIT) fail("bh_size wrong", step);
        for (uint32_t k = u; k < u + n; k++) {
            if (owner[k]) fail("two blocks share a unit", step);
            owner[k] = 1;
        }
        used += n;
    }
    if (used != h->used) fail("used counter wrong", step);
    if ((uint32_t)nlive != h->blocks) fail("block counter wrong", step);
    uint32_t prev_end = 0;
    uint32_t holes = 0;
    for (uint32_t i = 0; i < h->nruns; i++) {
        bh_run r = h->runs[i];
        if (!r.len) fail("empty hole", step);
        if (r.start < h->bottom || r.start + r.len > h->units) fail("hole outside region", step);
        if (i && r.start <= prev_end) fail("holes unsorted or adjacent", step);
        if (r.start == h->bottom) fail("hole at the bottom not given back", step);
        for (uint32_t k = r.start; k < r.start + r.len; k++) {
            if (owner[k]) fail("hole overlaps a block", step);
            owner[k] = 2;
        }
        holes += r.len;
        prev_end = r.start + r.len;
    }
    // Every unit of the region is a block or a hole.
    for (uint32_t k = h->bottom; k < h->units; k++) {
        if (!owner[k]) fail("unit in region is neither block nor hole", step);
    }
    if (used + holes != h->units - h->bottom) fail("region does not add up", step);
    if (bh_low(h) < floor && h->bottom != h->units) fail("region below the floor", step);
}

int main(void) {
    unsigned char *mem = malloc((size_t)UNITS * BH_UNIT + BH_UNIT);
    if (!mem) {
        printf("no memory for the fake heap\n");
        return 1;
    }
    uintptr_t base = ((uintptr_t)mem + BH_UNIT - 1) & ~(BH_UNIT - 1);
    uintptr_t end = base + (uintptr_t)UNITS * BH_UNIT;
    bigheap h;
    bh_init(&h, end, UNITS, len_tab, runs, UNITS / 2 + 2);
    if (h.base != base) fail("base", 0);

    uintptr_t floor = base + 16 * BH_UNIT;
    long ok_allocs = 0, refused = 0;
    for (long step = 1; step <= 300000; step++) {
        uint64_t r = next();
        // The break moves, but never above the region (that is the sbrk wrap's job).
        if (r % 97 == 0) {
            uintptr_t low = bh_low(&h);
            uintptr_t span = low > base ? low - base : 0;
            floor = base + (span ? (next() % (span + 1)) : 0);
        }
        int do_alloc = nlive == 0 || (nlive < MAXLIVE && (r >> 8) % 100 < 52);
        if (do_alloc) {
            size_t size;
            uint64_t k = (r >> 16) % 100;
            if (k < 60) size = 1 + (next() % (512 * 1024));                // GPU textures, Vec
            else if (k < 90) size = 512 * 1024 + (next() % (8u << 20));    // big buffers
            else size = (8u << 20) + (next() % (32u << 20));               // atlases, chunks
            unsigned char *p = bh_alloc(&h, size, floor);
            if (p) {
                if ((uintptr_t)p < floor) fail("block below the floor", step);
                live[nlive].p = p;
                live[nlive].size = size;
                live[nlive].tag = (uint32_t)step;
                fill(&live[nlive]);
                nlive++;
                ok_allocs++;
            } else {
                refused++;
                // A refusal must be honest: nothing that big was available.
                if (bh_largest(&h, floor) >= size) fail("refused although bh_largest fits", step);
            }
        } else {
            int i = (int)(next() % (uint64_t)nlive);
            if (!intact(&live[i])) fail("block content corrupted", step);
            if (bh_free(&h, live[i].p) != 0) fail("bh_free refused a live block", step);
            if (bh_free(&h, live[i].p) == 0) fail("double free accepted", step);
            live[i] = live[--nlive];
        }
        check(&h, floor, step);
    }
    // Free everything: the region must vanish entirely.
    while (nlive) {
        if (!intact(&live[nlive - 1])) fail("corrupted at the end", -1);
        if (bh_free(&h, live[nlive - 1].p) != 0) fail("final free", -1);
        nlive--;
        check(&h, floor, -1);
    }
    if (h.bottom != h.units || h.nruns || h.used || h.blocks) fail("region not empty at the end", -1);
    // Foreign pointers are refused.
    if (bh_free(&h, mem) == 0 || bh_free(&h, (void *)(end - 1)) == 0) fail("foreign pointer accepted", -1);
    printf("OK: %ld allocations, %ld refusals, 300000 steps, every state checked\n", ok_allocs, refused);
    free(mem);
    return 0;
}
