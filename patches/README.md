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
