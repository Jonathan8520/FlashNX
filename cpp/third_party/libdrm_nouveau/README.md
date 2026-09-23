# libdrm_nouveau (FlashNX copy)

The layer between Mesa's nouveau driver and the Switch GPU services, built into
FlashNX in place of devkitPro's `-ldrm_nouveau`.

- Upstream: https://github.com/devkitPro/libdrm_nouveau at commit
  `137265185c95cf58ce94c7afecba14f895dac2a7` (2020-05-11). That is the source of
  the installed `switch-libdrm_nouveau 1.0.1-2` package: its four objects define
  the same symbols, and its headers match the ones in
  `$DEVKITPRO/portlibs/switch/include`, which this copy still compiles against so
  Mesa's view of every structure stays the same.
- License: MIT, in each source file's header.
- `config.h` is empty on purpose: `bufctx.c` includes it and upstream's build
  provides none.
- Built with `-DNDEBUG`, like the package.

## What FlashNX changes

Only `nouveau.c` and `private.h`, and only to add a buffer-object cache: freed
colour-tiled BOs of at most 1 MiB are kept and handed back to the next request of
the same size, alignment, kind and coherence. The block comment above
`nouveau_bo_del` in `nouveau.c` explains why and what makes a recycled BO
indistinguishable from a fresh one.

If a fresh allocation fails, the cache is emptied and the allocation retried
once, so parked blocks can never be why one fails. The same holds outside
libdrm: the Rust global allocator (`rust/src/lib.rs`, `reclaim_gpu_cache`) calls
`flashnx_boc_trim` and retries when newlib refuses a block, since the parked
blocks live in that same heap.

Three functions are exported for the Rust side:

- `flashnx_boc_set(int on)`. The cache is on by default. Turning it off empties
  it. Nothing calls it in the shipped code; it is what the 2026-09-23
  measurement build used to alternate on and off within one session.
- `uint64_t flashnx_boc_trim(void)` empties it, keeps it on, and returns the
  bytes given back. The renderer calls it each time a backend is created (a
  game starting, restarting, or leaving), and the Rust allocator when the heap
  is full.
- `flashnx_boc_stats(uint64_t *out, int n)` returns running counters, then the
  number of entries and the bytes held.

Every other line is upstream's.
