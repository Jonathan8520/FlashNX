// See flashnx_bigheap.h. Pure bookkeeping over unit indexes: no locking, no
// allocation, nothing written inside a block.
#include "flashnx_bigheap.h"

#include <string.h>

void bh_init(bigheap *h, uintptr_t end, uint32_t units, uint32_t *len_at, bh_run *runs,
             uint32_t runs_cap) {
    h->base = end - (uintptr_t)units * BH_UNIT;
    h->units = units;
    h->bottom = units;
    h->len_at = len_at;
    memset(len_at, 0, (size_t)units * sizeof(uint32_t));
    h->runs = runs;
    h->nruns = 0;
    h->runs_cap = runs_cap;
    h->used = 0;
    h->blocks = 0;
}

uintptr_t bh_low(const bigheap *h) {
    return h->base + (uintptr_t)h->bottom * BH_UNIT;
}

// Lowest unit the region may grow down to without crossing `floor`.
static uint32_t floor_unit(const bigheap *h, uintptr_t floor) {
    if (floor <= h->base) {
        return 0;
    }
    uintptr_t u = (floor - h->base + BH_UNIT - 1) / BH_UNIT;
    return u > h->units ? h->units : (uint32_t)u;
}

static void run_remove(bigheap *h, uint32_t i) {
    memmove(&h->runs[i], &h->runs[i + 1], (size_t)(h->nruns - i - 1) * sizeof(bh_run));
    h->nruns--;
}

static int run_insert(bigheap *h, uint32_t i, uint32_t start, uint32_t len) {
    if (h->nruns >= h->runs_cap) {
        return -1;
    }
    memmove(&h->runs[i + 1], &h->runs[i], (size_t)(h->nruns - i) * sizeof(bh_run));
    h->runs[i].start = start;
    h->runs[i].len = len;
    h->nruns++;
    return 0;
}

// Unit index of the block `p` starts, or -1.
static int64_t block_unit(const bigheap *h, const void *p) {
    uintptr_t a = (uintptr_t)p;
    if (a < bh_low(h) || a >= h->base + (uintptr_t)h->units * BH_UNIT || (a - h->base) % BH_UNIT) {
        return -1;
    }
    uint32_t u = (uint32_t)((a - h->base) / BH_UNIT);
    return h->len_at[u] ? (int64_t)u : -1;
}

void *bh_alloc(bigheap *h, size_t size, uintptr_t floor) {
    if (size == 0) {
        size = 1;
    }
    if (size > (size_t)h->units * BH_UNIT) {
        return NULL;
    }
    uint32_t need = (uint32_t)((size + BH_UNIT - 1) / BH_UNIT);

    // Best fit among the holes; on a tie the highest one, so the low end of
    // the region stays free to be handed back to newlib.
    int64_t best = -1;
    for (uint32_t i = 0; i < h->nruns; i++) {
        const bh_run *r = &h->runs[i];
        if (r->len < need) {
            continue;
        }
        if (best < 0 || r->len < h->runs[best].len ||
            (r->len == h->runs[best].len && r->start > h->runs[best].start)) {
            best = i;
        }
    }

    uint32_t start;
    if (best >= 0) {
        // The top of the hole: what is left stays at the low end.
        bh_run *r = &h->runs[best];
        start = r->start + r->len - need;
        r->len -= need;
        if (r->len == 0) {
            run_remove(h, (uint32_t)best);
        }
    } else {
        // Grow down, taking the hole at the bottom of the region if there is one.
        uint32_t low = (h->nruns && h->runs[0].start == h->bottom) ? h->runs[0].len : 0;
        uint32_t extra = need - low; // > 0: that hole did not fit on its own
        uint32_t fu = floor_unit(h, floor);
        if (h->bottom < fu || h->bottom - fu < extra) {
            return NULL;
        }
        start = h->bottom - extra;
        if (low) {
            run_remove(h, 0);
        }
        h->bottom = start;
    }
    h->len_at[start] = need;
    h->used += need;
    h->blocks++;
    return (void *)(h->base + (uintptr_t)start * BH_UNIT);
}

int bh_free(bigheap *h, void *p) {
    int64_t found = block_unit(h, p);
    if (found < 0) {
        return -1;
    }
    uint32_t u = (uint32_t)found;
    uint32_t len = h->len_at[u];
    h->len_at[u] = 0;
    h->used -= len;
    h->blocks--;

    // First run starting after u.
    uint32_t lo = 0, hi = h->nruns;
    while (lo < hi) {
        uint32_t mid = (lo + hi) / 2;
        if (h->runs[mid].start < u) {
            lo = mid + 1;
        } else {
            hi = mid;
        }
    }
    uint32_t i = lo;
    uint32_t s = u, l = len;
    if (i > 0 && h->runs[i - 1].start + h->runs[i - 1].len == s) {
        s = h->runs[i - 1].start;
        l += h->runs[i - 1].len;
        i--;
        run_remove(h, i);
    }
    if (i < h->nruns && h->runs[i].start == s + l) {
        l += h->runs[i].len;
        run_remove(h, i);
    }
    if (s == h->bottom) {
        // A hole at the low end: give it back by raising the boundary.
        h->bottom += l;
        return 0;
    }
    // Cannot fail with runs_cap >= units / 2 + 1 (holes are separated by
    // blocks). If it ever did, these units would only be lost, not reused.
    (void)run_insert(h, i, s, l);
    return 0;
}

size_t bh_size(const bigheap *h, const void *p) {
    int64_t u = block_unit(h, p);
    return u < 0 ? 0 : (size_t)h->len_at[u] * BH_UNIT;
}

size_t bh_largest(const bigheap *h, uintptr_t floor) {
    uint32_t best = 0;
    for (uint32_t i = 0; i < h->nruns; i++) {
        if (h->runs[i].len > best) {
            best = h->runs[i].len;
        }
    }
    uint32_t fu = floor_unit(h, floor);
    uint32_t low = (h->nruns && h->runs[0].start == h->bottom) ? h->runs[0].len : 0;
    uint32_t down = (h->bottom > fu ? h->bottom - fu : 0) + low;
    if (down > best) {
        best = down;
    }
    return (size_t)best * BH_UNIT;
}
