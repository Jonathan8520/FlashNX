# Ruffle patches (third_party/ruffle)

Local patches applied to the `third_party/ruffle/` submodule. Must be
re-applied after any `git submodule update --remote`.

## Application

```bash
# From the project root:
cd third_party/ruffle
for p in ../../patches/*.patch; do
    git apply "$p"
done
```

## List

### 0001-mario63-zero-scale-hit-test.patch

**Fix Phase 2.4.a — missing Toad castle (issue #6906).**

Adds a zero-determinant matrix guard in `hit_test_bounds` and
`hit_test_shape` of `core/src/display_object.rs`. Without this patch, Mario 63
treats a zero-scale placeholder MC of the castle as "hittable", which
breaks the logic chain: Toad NPC not instantiated, Mario floating in
the void, progression blocked.

To be submitted as an upstream PR to revive and close #6906 for good.
The patch is also useful to any other Ruffle frontend (Web, desktop) — the
Adobe Flash Player parity is correct.

### 0002-pixelbender-shaderjob-run-noop.patch

**Keep PixelBender games alive when the renderer can't run shaders.**

The Switch GL backend (like Ruffle's webgl backend) does not implement
PixelBender. Our `compile_pixelbender_shader` returns a handle that just
carries the parsed shader (see `rust/src/backend/render.rs`), so AVM2
`Shader` / `ShaderData` / `ShaderFilter` construction succeeds and games
keep their input and frame logic. `run_pixelbender_shader` still fails, so
this patch turns `ShaderJob.start`'s `.expect("Failed to run shader")` into
a warn + no-op (the job leaves its target untouched) instead of aborting
the app. The renderer already skips `Filter::ShaderFilter`, so the only
visible difference is the absent shader effect. Validated on The Terminal
(failsafegames): enemies, shooting and the pause menu work; before, the
game built a `ShaderFilter` every frame in its enterFrame/click handlers,
so a hard error there silently broke all input.

### 0003-amf-cycle-serialize-crash.patch

**Stop the AMF serializer crashing/freezing on a cyclic SharedObject.**

`serialize_value` (`core/src/avm2/amf.rs`) fills its reference table
(`object_table`) only AFTER recursing into an object, so a SharedObject with a
circular reference recurses forever: a stack overflow (crash) for a simple
cycle, or exponential re-serialization (hang) for a branching one. This patch
detects cycles up front — a thread-local set of the objects currently on the
serialization stack, keyed by `as_ptr` — and returns `None` for a back-reference
instead of recursing, plus a depth backstop for pathological acyclic nesting.
The `.sol` save drops the cyclic back-pointer (slightly lossy) but is finite and
valid. Validated on Hemp Tycoon, which crashed when planting (it flushes a
cyclic save on every action): the game now plays and saves. Upstream master
still has no guard (checked 2026-06).

### Not carried as numbered patches

`ruffle-local.diff` is the full snapshot of `third_party/ruffle` and is the
only complete record. Several fixes live there without a numbered patch,
because their file already carries unrelated local changes that a standalone
`.patch` would duplicate. The notable ones:

**`core/src/character.rs` — decoded-bitmap budget, and `decode_or_stand_in`.**

The budget refuses a bitmap once the movie's decoded-bitmap allowance is
spent, so a huge SWF loses sprites instead of the process losing its heap.
`reset_bitmap_cache` re-sizes it per movie; FlashNX calls it from
`ensure_swf_loaded` on both the cached and uncached paths (a RESTART that
skipped it left the counter full and refused nearly every bitmap: 5163
refusals on Super Smash Flash 2, against 45 once fixed).

The budget subtracts the movie's own size, a proxy for heap pressure. For a
movie of 100 MB or more that proxy is what refuses, not the heap, so past the
budget such a movie keeps decoding while newlib can still hand out one 512 MB
block (`heap_has_room`, which calls `malloc` directly so a failed probe does
not set off FlashNX's allocator retry path). New Super Smash Flash (397 MB)
went from 219 refused images, its stage backgrounds among them, drawn white,
to none: heap peak 2.52 GB of 3.1 through fights, about 6 s more loading.
Smaller movies are untouched on purpose: Super Smash Flash 2 (1 MB) also
fills its large budget and enters a fight with about 98 MB of margin.

`decode_or_stand_in` exists because decoding allocates width x height x 4
bytes and is therefore among the first things to fail on an exhausted heap,
while three upstream call sites unwrap it: `library.rs`
(`instantiate_display_object`), `avm2/globals/flash/display/bitmap_data.rs`
(`fill_bitmap_data_from_symbol`) and `avm1/globals/bitmap_data.rs`. All three
now take a 1x1 transparent stand-in instead of killing the process. The
stand-in is deliberate and `None` is not an option: `clone_sprite` in
`avm1/globals/movie_clip.rs` unwraps that `Option`, so returning `None` would
merely move the panic to `duplicateMovieClip`.

**`core/src/bitmap/operations.rs` — backport of upstream ruffle#23870.**

`hit_test_point` treats an `alpha_threshold` of 0 as 1, as Flash does. With 0,
a fully transparent pixel counted as a hit, so Super Smash Flash 2's
collision bitmaps (vector terrain drawn into BitmapData, then point-tested)
were solid over their whole bounding box: characters stood on air beside the
drawn ledges (ruffle#19253, fixed upstream on 2026-06-21, after our base).

**Experiment switches (`graphic.rs`, `player.rs`), read from FlashNX markers.**
`set_lazy_shapes(false)` (`lazyshape.off`) registers static shapes at preload
as upstream does; `set_bitmap_cache(false)` (`bitmapcache.off`) ignores
cacheAsBitmap in the main render. Defaults unchanged. They ruled out both
paths for the SSF2 invisible floors before the upstream ticket was found.

**`core/src/frame_lifecycle.rs`, `display_object.rs`, `orphan_manager.rs`,
`display_object/movie_clip.rs` — inner gotos skip unchanged orphans.**

Follow-up: an orphan mark epoch (`ORPHAN_MARK_EPOCH`, bumped whenever a tree
other than the stage's gets marked or an object joins the orphan list).
After a frame-script pass that cleaned every dirty orphan with no mark
arriving meanwhile, gotos skip iterating the orphan list altogether until
the epoch moves (the list itself, ~800 weak references iterated twice per
goto, was 7.7 % + 1.8 % of an SSF2 level 3 frame). Full suite: 3351 passed,
the same two known failures.

Every AVM2 goto (no-op ones included, on purpose: Flash shows their side
effects) runs a nested construct + frame-script pass over the stage and over
the whole tree of every orphan. Super Smash Flash 2 keeps ~600-800 orphans
and fires 4 000 to 36 000 gotos per 300 frames in a fight: that walk was 54 %
of a fight frame, and fights slowed down one after the other (upstream
ruffle#24509, open, is the same wall on desktop). A `LIFECYCLE_DIRTY` flag now
sits on the top of each display tree: set by `mark_lifecycle_dirty` when
something in it changes frame (`run_frame_internal`, `run_goto`,
`set_current_frame`), gains a parent-child link (`set_parent`), queues a frame
script or a goto, loads a movie, or joins the orphan list; cleared just
before that orphan's frame scripts run. An inner goto skips the orphans whose
flag is clear: for them both walks were no-ops. Normal frames still walk
every orphan, and so does every inner goto with FlashNX's `gotoskip.off`
marker. Ruffle's own suite (host build, GNU toolchain): 1059 AVM2 and 702
AVM1 tests pass; the two failures are `stage3d_rotating_cube` (no GPU in the
harness) and `movieclip_hittest` (patch 0001, deliberate). On SSF2: 99.8 % of
orphan walks skipped, first fight loads in 6.4 s instead of 12.6-16.6 and
runs at 45-60 fps instead of 9-23.

**`core/src/display_object/graphic.rs` — static shapes registered on first
draw.**

Upstream registers every DefineShape with the renderer while the movie
preloads, which decodes and uploads every bitmap its fills use, whether the
shape is ever shown or not. Measured on Super Smash Flash 2 (`atlasIdle:`
lines): 79 of 81 atlases (1 049 MB) held images never drawn once, and still
553 MB after menus and a whole fight. `GraphicShared::render_handle` is now a
`OnceCell` filled by `Graphic::base_handle` the first time `render_self` needs
it, through the same `context.library` the re-tessellation path already
used. Morph shapes, glyphs and drawings were already lazy. Result on SSF2:
1-3 atlases in the menus instead of 81, 21 (281 MB) after three fights
instead of 88 (1 193 MB), no bitmap refused, heap 1.6-2.1 GB instead of 3.0.
Super Mario 63 unchanged to the eye, 60 fps, a few 40-58 ms frames where new
areas first appear.

**`core/src/display_object/movie_clip.rs`, `core/src/context.rs` — the
movie's own script timeout.**

Upstream reads the `ScriptLimits` tag's timeout and drops it, so every movie
is cut at 15 s. Flash honours it. Super Smash Flash 2 asks for 60 s, and its
fight-loading frame runs 14.8 to 15.6 s on the Switch: killed just short, a
black screen on entering every fight (ruffle#24726 is the same wall on
desktop, labelled as a performance issue). The update context now holds a
reference to the Player's timeout instead of a copy, and the tag raises it:
never below the 15 s default, capped at 60 s since the timeout is the only
bound on how long a stuck script freezes the console, largest value seen
wins so a SWF loaded later cannot lower it.

**`core/src/player.rs` — GC and frame-pacing probes.**

`flashnx_gc_probe` publishes, per host frame, the number of SWF frames the
tick actually ran, the collector phase, the arena's total allocation and the
microseconds spent inside the collector. Added to settle whether a periodic
28-frame stall was the collector or frame catch-up; it was neither, it was
newlib's `free`. Diagnostic only, and a candidate for removal once the
allocator work is finished.

**`core/src/avm1/object_reference.rs` + `display_object/movie_clip.rs` — the
clip-reference cache.**

Every time a MovieClip lands on the AVM1 stack (a register or `this` pushed,
a GetMember or GetVariable result), `MovieClipReference::try_from_stage_object`
rebuilt the clip's dotted path string, re-split it and allocated a new
reference, all of it garbage for the collector. Upstream master still does.
Each MovieClip now keeps the last reference built for it, reused while a
64-bit fingerprint of its path (the names up the parent chain, then the root's
level) is unchanged, while it was built for this very clip (`MovieClipData` is
cloned by `instantiate`), and while it still holds its weak link to the clip:
one that fell back to walking its path is never handed out again. Sharing a
reference is invisible because `Value` compares them by path, never by
pointer. On Super Mario 63's world 8 it took the frame from 57 to 37 ms and the
collector from 8.4 to 0.8 ms (A/B in one session, 2026-09-23). FlashNX turns
it off only for an `mcref.off` marker on the SD card.

**`core/src/avm1/activation.rs`, `display_object/container.rs` — AVM1
profiling, behind the `flashnx_instr` feature.**

Per-action exclusive time and count (a called function's body is charged to
its own actions), decode time, and child-by-name lookups. Compiled out of
release builds; the Switch dev build prints the hottest actions every 240
frames. It is what found the cache above.

**Super Smash Flash 2, October 2026: memory and speed.** Measured on the
console over a dozen sessions; the full suite (host build, GNU toolchain)
passes 4063 tests with the same two known failures after each step. The
Switch side (allocator, renderer) is in FlashNX itself, not in this diff.

- `core/src/character.rs`, `library.rs`, `display_object/movie_clip.rs`:
  a bitmap tag that lies inside its movie keeps a slice of the movie's bytes
  (`BitmapBytes::Movie`) instead of a copy, and a JPEG1 tag shares the movie's
  `JPEGTables` (glued at decode time). Every image of a loaded SWF used to be
  held twice: 97 MB by the 4th Classic level.
- `render/src/backend.rs` (`register_bitmap_reloadable`, default = keep for
  ever), `core/src/character.rs` (`BitmapCharacter::bitmap_handle` passes a
  closure that decodes the tag again, `flashnx_bitmap_residency` keeps the
  decoded-bitmap budget in step): the renderer may drop the texture of a SWF
  image nothing has drawn for a while and rebuild it on the next draw.
- `core/src/bitmap/operations.rs`, `bitmap_data.rs`: backport of upstream
  ruffle#24490 (copyPixels by rows). `threshold` no longer marks the bitmap
  dirty when it changed no pixel, and a bitmap thresholded onto itself
  remembers the calls that matched nothing (`ThresholdMemo`, valid while the
  bitmap's new `generation` counter has not moved; every pixel write path bumps
  it). SSF2 recolours each fighter every frame with one `threshold` per palette
  colour: up to 93 % of 157 000 calls in 300 frames changed nothing.
- `core/src/avm2/activation.rs`, `op.rs`, `value.rs`,
  `optimizer/type_aware.rs`: backports of upstream ruffle#23840, #24211 and
  #24660 (integer fast paths, Vector methods called directly), and
  `resolve_parameters` builds native arguments in a `SmallVec` instead of a
  `Vec` per call (the cheap half of ruffle#24542).
- `core/src/avm2/globals/flash/system/System.as`, `system.rs`,
  `core/src/player.rs`: `System.gc()` runs a full collection after the update,
  at most once every 10 s (as in upstream PR #23897, still open). Past 256 MB
  of live arena the collector also sleeps less and works faster per allocated
  byte (`GC_TIGHT_PACING`): the garbage waiting for a cycle swung SSF2's arena
  from 300 to 450 MB in late levels.
- `core/src/display_object.rs`, `display_object/stage.rs`,
  `loader_display.rs`, `avm2_button.rs`, `frame_lifecycle.rs`: an inner goto
  also passes over every unchanged subtree of the stage (the
  `LIFECYCLE_DIRTY` flag now marks the whole ancestor chain and is settled
  bottom-up at the end of `run_frame_scripts`; buttons always stay dirty).
  `stageskip.off` marker to compare.
- `core/src/display_object.rs`, `orphan_manager.rs`, `frame_lifecycle.rs`:
  the dirty orphans are queued by address when marked and an inner goto runs
  only those, in orphan-list order (the list is indexed by address and push
  order), instead of iterating ~800 weak references twice per goto. The
  cleanup at the end of an inner goto only runs when an orphan that only a
  cleanup removes (one a RemoveObject tag orphaned) was added. `nextFrame`
  went from 74-131 us to 39-50 us a call in an SSF2 fight.
- `core/src/flashnx_as3prof.rs` (with `function::exec`): per-function AS3
  timing, exclusive and inclusive, printed every 300 frames when FlashNX's
  `as3prof.on` marker is present; one relaxed load per call otherwise.
  `core/src/flashnx_census.rs` (feature `flashnx_census`, off by default):
  live AVM2 objects per class.

**Super Smash Flash 2, October 2026, continued: what a long session holds.**
Classic mode used to die at the first level of a second run; it now runs about
three and a half Classic runs in a row (SSF2's own staff advised restarting
every 3 to 6 matches even on Flash Player: its hitbox caches are never
cleared). Heap profiles on the console (`--memprof` build) drove every step.

- `core/src/library.rs`, `character.rs`, `avm2/class.rs`, `avm2/domain.rs`,
  `avm2/object/loaderinfo_object.rs`, `display_object/*.rs`, `player.rs`: a
  movie a `Loader` unloads is no longer kept for good. Its library is
  released: no longer traced, and at the end of every marking phase
  (gc-arena's finalization) it is kept whole while anything outside it still
  uses the movie (its AVM2 domain, its root clip, or an instance of one of
  its symbols, which shares the symbol's `shared` data), and dropped
  otherwise, with its symbol classes and sounds. Finalization runs to a fixed
  point, since a kept library can make another one's anchors live. Every
  collection goes through `Player::collect_debt` / `collect_full`, so no
  sweep follows a marking that skipped it. Same model as upstream PRs #22908
  and #23071 (open), smaller. `moviefree.off` marker to compare.
- `core/src/avm2/vtable.rs`, `class.rs`, `property.rs`, `property_map.rs`:
  a class's resolved traits were built twice with the same inputs (for its
  `Class` and its `ClassObject`); the second vtable now shares the first's
  map. `PropertyMap` keeps one namespace inline per name instead of two
  (56 bytes an entry instead of 96). About 280 MB less on SSF2.
- `core/common/src/tag_utils.rs` (`SharedBytes`, `SwfMovie::from_shared_data`),
  `core/src/avm2/bytearray.rs`, `avm2/globals/flash/utils/byte_array.rs`,
  `avm2/globals/flash/display/loader.rs`, `loader.rs`: a ByteArray's bytes can
  be shared, copy on write, with another ByteArray they are written into
  whole (`writeBytes` into an empty one, from 64 KB) and with the movie
  `Loader.loadBytes` loads from them. An uncompressed movie keeps its tags in
  that buffer (checked byte for byte). SSF2 keeps every DAT file it downloads
  as a ByteArray and loads the SWF inside: 212 MB of ByteArrays next to
  224 MB of movies, the same bytes. `uncompress` / `compress` also take the
  result as the storage at its exact size (the old storage kept its capacity
  and doubled from there: ~1.85x the data).
- `core/src/backend/audio.rs`, `audio/mixer.rs`, `audio/decoders.rs`,
  `display_object/movie_clip.rs`: the mixer references an embedded sound in
  its movie instead of copying it (`SoundBytes`), and forgets the sounds of a
  dropped library (`AudioBackend::unregister_sound`, default no-op).
- `core/src/bitmap/bitmap_data.rs`, `character.rs`,
  `avm2/globals/flash/display/bitmap.rs`, `bitmap_data.rs`: Flash Player
  10.1's "BitmapData single reference". Every BitmapData made from a library
  bitmap shares its decoded pixels (`SharedPixels`, held weakly by the
  symbol) until one of them changes a pixel; every writer goes through
  `unshare`. A bitmap placed by a goto used to be inflated and copied each
  time (~9 % of a 14-second SSF2 loading frame).
- `core/src/avm2/regexp.rs`: compiled patterns are shared by source and flags
  (`g` excluded), up to 512 of them. A regex literal builds a new RegExp each
  time it runs, and each compiled its pattern again: ~21 % of the same frame,
  and every discarded RegExp held its program until the next collection,
  which cannot run in the middle of a script.
- `core/src/avm2/dynamic_map.rs`, `avm2/object/script_object.rs`: every AS3
  object carried an empty dynamic-property table (48 bytes) and an empty
  bound-method `Vec` (24 bytes). Both are now allocated on first use, and
  the enumeration indices are 32 bits: about 48 bytes less per object, of
  which SSF2 keeps 1.7 million.

FlashNX's side of the same work (not in this diff): `cpp/third_party/
libdrm_nouveau/nouveau.c` gives nvdrv 32 MB of transfer memory instead of
libnx's 8 MB (`__nx_nv_transfermem_size`; every nvmap handle is booked there,
and past ~4000 of them `nvMapCreate` fails with 0x235C whatever the size, which
froze SSF2 after 36 minutes with 5600 handles live) and caches pitch staging
BOs too; `rust/src/lib.rs` keeps dlmalloc's segments apart so that an empty one
can go back to the heap (none did in SSF2: a few long-lived blocks pin each); `rust/src/heapprof.rs` and `scripts/heapprof_report.py` are
the heap profiler.

**Frame rate, October 2026.** Measured with the sampling profiler on the
console, one change at a time, each with an SD-card marker to turn it off.

- `core/src/player.rs`: the time spent in `render` event handlers counts in
  the cost of the frame that asked for them (`stage.invalidate()`), so
  `max_frames_per_tick` (ruffle#3068: catch up only when it can be afforded)
  sees it. Super Smash Flash 2 runs its whole engine in a `render` handler,
  which Ruffle fires once per image drawn, not once per frame (ruffle#9339):
  frames measured at 10 ms for ~43 real ones made Ruffle run 2 or 3 of them
  per tick, moving the timelines on without the engine, while the frame
  counter read 30 for an engine stepping ~17 times a second. Now one frame
  runs per tick and each gets its `render` event, as Flash Player slows down
  rather than skipping images. Marker `rendercost.off`. The frame
  accumulator is also published (`flashnx_frame_accumulator_us`) so the FPS
  counter is not rounded to whole frames (it read x1.03 at times).
- `core/src/bitmap/operations.rs`: `threshold` onto itself at the same place
  writes only the matching pixels, in a loop chosen once per row for the
  operation (SSF2's palette recolouring, ~8 % of its heavy frames).
- `core/src/avm2/array.rs`: growing a dense array's length keeps it dense, up
  to 65536 slots. avmplus (`ArrayObject::setLength`) only moves `m_length`;
  here the holes are slots, so `new Array(n)` with n > 32 went sparse at once
  and for good, and every access became a `BTreeMap` search. Box2DFlash
  allocates its pools that way and new arrays on every step: Fireboy &
  Watergirl 2's heavy frames at the start of a level went from 97.5 to
  91-94 ms (median of the first 300, three sessions). The rest of that
  frame is the interpreter itself.

- `core/src/display_object.rs`, `frame_lifecycle.rs`, `movie_clip.rs`: full
  frames pass over unchanged subtrees too. Every frame walked the whole
  display list three times (enter, construct, frame scripts), and each visit
  is pointer chasing through separate allocations: Agent P Strikes Back spent
  ~70 % of its frames there for ~3 % of ActionScript (ruffle#23315, #22192
  and #23876 show the same profile on desktop; #23876 proposes a "fully
  constructed" flag). Construction and frame scripts now skip clean
  subtrees as inner gotos already did. `enter_frame` skips quiet ones
  (`ENTER_QUIET`: no clip playing, no frame-skip flag, no queued tag,
  nothing marked since the last visit), set from the bottom up by the visit
  and cleared up to the top by every mark, `play()` and the frame-skip flag.
  Agent P went from 14.5 to ~60 images a second. Marker `frameskip.off`.

- `core/src/avm2/activation_jit.rs` (new), `activation.rs`, `method.rs`,
  `stack.rs`, `object/script_object.rs`: **a baseline JIT for AVM2 bytecode**
  (feature `flashnx_jit`, AArch64, the Switch build only). Fireboy & Watergirl
  2 runs 2.65 million AVM2 ops a frame at ~57 cycles each; the interpreter pays
  one shared, badly predicted indirect branch per op and keeps its stack
  pointer in memory. On its second call a method is compiled: the stack depth
  of every op is known, so stack slots are fixed offsets from x19; locals,
  constants, `Pop`, `Dup`, `Swap` and branches are a few instructions; every
  other op calls `h_op::<K>`, one instantiation per op kind that runs the
  interpreter's own handler and checks the depth it left (a mismatch hands the
  method back to the interpreter at the next op, for good). Inline fast paths:
  `getslot` on objects, `iftrue`/`iffalse` on a `Bool`, `+ - *` and the
  comparisons when both operands are `Number`s or both `int`s (same results as
  the interpreter's fast paths, `int` overflow and NaN included), `IncrementI`,
  `DecrementI`, `CoerceI`, `CoerceD`. The layout they read (tags of `Value`,
  payload at byte 8, `Gc` box to object data, the slots' fat pointer) is
  measured on real values before use; an object type not checked takes the
  helper. Methods with `try`/`catch`, `lookupswitch` or `Timestamp` stay
  interpreted. Code memory: 16 MB from libnx's `jitCreate` (CodeMemory, which
  hbloader allows on Mesosphere), `cpp/src/flashnx_jit.c`, reused by each new
  game (`jit_reset`). Fireboy 2's light temple: 13.0 to 16.9 images a second
  (same session, 16 window pairs), no AS3 error in six other games. Marker
  `jit.off`; `jit:` line every 300 frames.
  Then, the way avmplus' JIT calls an early-bound method (`emitTypedCall`,
  `coerceArgs`, the callee's `_implGPR`): a compiled `callmethod` enters a
  compiled callee directly (`fast_call`: same vtable lookup, frame, activation
  fields, call stack and cleanup as `exec`, taken only when the callee has no
  `arguments`/rest, gets as many arguments as parameters and they already
  have their parameter's type, or only need `int` to `Number` or `undefined`
  to `null`); 99.7 % of Fireboy's calls, -3.3 ms a frame. `getproperty` with
  an int index on a dense `Array` has its own helper (no dynamic dispatch, no
  borrow counting, `ArrayObject::flashnx_storage_unguarded`). `setslot`
  without coercion stores in place when the object is not black for the GC
  (color bits of the box header, located on two fresh arrays and confirmed by
  gc-arena's allocation chaining); a black object takes the helper, whose
  `Gc::write` runs the barrier. All together: 13.0 to 18.5 images a second
  (-23 ms a frame, same session).
- `core/src/avm2/activation.rs`, `function.rs`, `stack.rs`: **a cheaper AVM2
  call path** (`set_fast_calls`). A bytecode activation keeps its method
  instead of cloning an `Arc<SwfMovie>` per call (two atomic loops on the
  Switch's ARMv8.0 cores, no LSE), the receiver check is left to debug builds,
  and a new frame clears only its locals (the verifier rejects any pop without
  a push of the same frame, so the operand part is never read stale). -2.8 ms a
  frame on Fireboy 2. Marker `fastcall.off`.
- `core/src/avm2/property_map.rs`, `domain.rs`: backport of upstream
  ruffle#23253 (merged 2026-07-10, after our base). When a subclass declares a
  field with the same local name as a private or internal field of its
  superclass, a bracket lookup (`this["field"]`) now finds the subclass's one
  first, as Flash Player 32 does: `PropertyMap::insert` puts a new entry at the
  front of its name's list, application domains keep the old order
  (`insert_at_end`). Our `scope.rs` still uses `insert_with_namespace`
  (removed upstream by 5087eca85), which already appends, the order the fix
  keeps for scope caches. A game whose dropdowns failed to build (error #1009
  in their constructor) had a dead main menu; it now starts. The fix's five
  `property_priority*` tests are in the snapshot and pass.
- `core/src/flashnx_avm2ops.rs` (new): AVM2 opcode census (`avm2ops:` lines,
  feature `flashnx_opcount`, not in any default build: its pair table costs
  ~14 % of an AS3-bound frame).

FlashNX's side (not in this diff), in `rust/src/backend/render.rs`:

- Shape vertices are written with an unsynchronized `glMapBufferRange`. The
  Switch's Mesa (devkitPro's 20.1) keeps every buffer in GART and maps it
  with `NOUVEAU_BO_WR`, which waits for the GPU to be done with the whole
  buffer, except for a range never written before. Once freed regions were
  reused, catmario spent 62 % of its heavy frames in that wait. Freed regions
  now go back to the arena only after a fence put behind the last command
  that could read them. Marker `unsyncvbo.off`.
- `render_offscreen` copies only the region a `BitmapData.draw` can change
  (the `bounds` Ruffle passes, plus two pixels), not the whole bitmap in and
  out of its temp for every draw: catmario's end of level 1 went from 24 to
  ~60 images a second. Marker `offbounds.off`.
- Offscreen temps are reused by best fit (up to 4x the area) and made in
  64-pixel steps, instead of matching sizes exactly: GPU-object creation
  spikes in SSF2 fell by about 60 %.

### Tried and dropped (2026-10-06)

- **The small-object region at 1 GB instead of 512 MB.** SSF2 then ended with
  1023 MB there plus 656 MB in dlmalloc, against 511 + 856: a region chunk only
  serves its own size classes, so the small-block peak of a loading frame
  stayed reserved for good. Back to 512 MB.

### Tried and dropped (2026-09-24)

Measured with the sampling profiler (`cpp/src/prof.cpp`) in an in-session A/B
on Super Mario 63, 71 window pairs: 1.0 +- 0.9 ms, noise. Neither is in the
snapshot. Recorded so they are not tried again blind.

- **A child-name filter in `ChildContainer::get_name`** (a 256-bit bloom of the
  children's case-folded names, so a lookup for a name no child carries skips
  the scan). Killed by its invalidation: the rename epoch was global, and
  `set_default_instance_name` names every object a timeline places after
  attaching it, so every animated shape staled every filter several times a
  frame and the filter cost 2.5 % on its own. An epoch per parent (a counter in
  the parent's `DisplayObjectBase`, read by `child_by_name`, the only caller of
  `get_name`) would fix that, for about 0.8 ms at best.
- **A per-pick map of world bounds for `MovieClip::mouse_pick_avm1`** (one walk
  down per pick instead of one `world_bounds` per clip). Mario 63's tree is flat:
  filling the map cost as much as the calls it saved. Note that upstream master
  (`03fc070fa`, `hitArea`) has since removed that `world_bounds` check from the
  pick and calls `is_button_mode` on every clip instead, which allocates eight
  strings and runs seven property lookups each time: expect the pick to get
  slower, not faster, with the next Ruffle update.
