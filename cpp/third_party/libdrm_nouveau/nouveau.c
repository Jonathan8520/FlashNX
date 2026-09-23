/*
 * Copyright 2012 Red Hat Inc.
 *
 * Permission is hereby granted, free of charge, to any person obtaining a
 * copy of this software and associated documentation files (the "Software"),
 * to deal in the Software without restriction, including without limitation
 * the rights to use, copy, modify, merge, publish, distribute, sublicense,
 * and/or sell copies of the Software, and to permit persons to whom the
 * Software is furnished to do so, subject to the following conditions:
 *
 * The above copyright notice and this permission notice shall be included in
 * all copies or substantial portions of the Software.
 *
 * THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
 * IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
 * FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT.  IN NO EVENT SHALL
 * THE COPYRIGHT HOLDER(S) OR AUTHOR(S) BE LIABLE FOR ANY CLAIM, DAMAGES OR
 * OTHER LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE,
 * ARISING FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR
 * OTHER DEALINGS IN THE SOFTWARE.
 *
 * Authors: Ben Skeggs
 */

#include <stdio.h>
#include <stdlib.h>
#include <stdint.h>
#include <string.h>
#include <strings.h>
#include <stdbool.h>
#include <assert.h>
#include <errno.h>
#include <fcntl.h>
#include <malloc.h>

#include "libdrm_lists.h"
#include "libdrm_atomics.h"
#include "nouveau_drm.h"
#include "nouveau.h"
#include "private.h"

#include "nvif/class.h"
#include "nvif/cl0080.h"
#include "nvif/ioctl.h"
#include "nvif/unpack.h"

#ifdef DEBUG
#	define TRACE(x...) printf("nouveau: " x)
#	define CALLED() TRACE("CALLED: %s\n", __PRETTY_FUNCTION__)
#else
#	define TRACE(x...)
# define CALLED()
#endif

/* FlashNX buffer-object cache, defined next to nouveau_bo_del below. */
static void boc_flush(void);

/* Unused
int
nouveau_object_mthd(struct nouveau_object *obj,
		    uint32_t mthd, void *data, uint32_t size)
{
	return 0;
}
*/

/* Unused
void
nouveau_object_sclass_put(struct nouveau_sclass **psclass)
{
}
*/

/* Unused
int
nouveau_object_sclass_get(struct nouveau_object *obj,
			  struct nouveau_sclass **psclass)
{
	return 0;
}
*/

int
nouveau_object_mclass(struct nouveau_object *obj,
		      const struct nouveau_mclass *mclass)
{
  // TODO: Only used for VP3 firmware upload
	CALLED();
	return 0;
}

/* NVGPU_IOCTL_CHANNEL_ALLOC_OBJ_CTX */
int
nouveau_object_new(struct nouveau_object *parent, uint64_t handle,
		   uint32_t oclass, void *data, uint32_t length,
		   struct nouveau_object **pobj)
{
	struct nouveau_object *obj;
	CALLED();

	if (!(obj = calloc(1, sizeof(*obj))))
		return -ENOMEM;

	if (oclass == NOUVEAU_FIFO_CHANNEL_CLASS)
	{
		struct nouveau_fifo *fifo;
		if (!(fifo = calloc(1, sizeof(*fifo)))) {
			free(obj);
			return -ENOMEM;
		}
		fifo->object = parent;
		fifo->channel = 0;
		fifo->pushbuf = 0;
		obj->data = fifo;
		obj->length = sizeof(*fifo);
	}

	obj->parent = parent;
	obj->oclass = oclass;
	*pobj = obj;
	return 0;
}

/* NVGPU_IOCTL_CHANNEL_FREE_OBJ_CTX */
void
nouveau_object_del(struct nouveau_object **pobj)
{
	CALLED();
	if (!pobj)
		return;

	struct nouveau_object *obj = *pobj;
	if (!obj)
		return;

	if (obj->data)
		free(obj->data);
	free(obj);
	*pobj = NULL;
}

void
nouveau_drm_del(struct nouveau_drm **pdrm)
{
	CALLED();
	struct nouveau_drm *drm = *pdrm;
	free(drm);
	*pdrm = NULL;
}

int
nouveau_drm_new(int fd, struct nouveau_drm **pdrm)
{
	CALLED();
	struct nouveau_drm *drm;
	if (!(drm = calloc(1, sizeof(*drm)))) {
		return -ENOMEM;
	}

	drm->fd = fd;
	drm->version = 0x01000202;
	*pdrm = drm;
	return 0;
}

int
nouveau_device_new(struct nouveau_object *parent, int32_t oclass,
		   void *data, uint32_t size, struct nouveau_device **pdev)
{
	struct nouveau_drm *drm = nouveau_drm(parent);
	struct nouveau_device_priv *nvdev;
	Result rc;
	CALLED();

	if (!(nvdev = calloc(1, sizeof(*nvdev))))
		return -ENOMEM;
	*pdev = &nvdev->base;
	nvdev->base.object.parent = &drm->client;
	nvdev->base.object.handle = ~0ULL;
	nvdev->base.object.oclass = NOUVEAU_DEVICE_CLASS;
	nvdev->base.object.length = ~0;

	rc = nvInitialize();
	if (R_SUCCEEDED(rc))
	{
		rc = nvFenceInit();
		if (R_SUCCEEDED(rc))
		{
			rc = nvMapInit();
			if (R_SUCCEEDED(rc))
			{
				rc = nvGpuInit();
				if (R_SUCCEEDED(rc))
				{
					const nvioctl_gpu_characteristics* info = nvGpuGetCharacteristics();
					nvdev->base.chipset = info->arch; // should be 0x120 (NVGPU_GPU_ARCH_GM200)
					rc = nvAddressSpaceCreate(&nvdev->addr_space, info->big_page_size);
					if (R_FAILED(rc))
						nvGpuExit();
				}
				if (R_FAILED(rc))
					nvMapExit();
			}
			if (R_FAILED(rc))
				nvFenceExit();
		}
		if (R_FAILED(rc))
			nvExit();
	}

	if (R_FAILED(rc))
	{
		free(nvdev);
		return -rc;
	}

	return 0;
}

void
nouveau_device_del(struct nouveau_device **pdev)
{
	CALLED();
	struct nouveau_device_priv *nvdev = nouveau_device(*pdev);

	if (nvdev) {
		/* FlashNX: the cached BOs are mapped in this address space. */
		boc_flush();
		nvAddressSpaceClose(&nvdev->addr_space);
		nvGpuExit();
		nvMapExit();
		nvFenceExit();
		nvExit();
		free(nvdev->client);
		free(nvdev);
		*pdev = NULL;
	}
}

int
nouveau_getparam(struct nouveau_device *dev, uint64_t param, uint64_t *value)
{
	/* NOUVEAU_GETPARAM_PTIMER_TIME = NVGPU_GPU_IOCTL_GET_GPU_TIME */
	int ret = 0;
	if (param == NOUVEAU_GETPARAM_GRAPH_UNITS)
		*value = (16 << 8) | 4;
	else if (param == NOUVEAU_GETPARAM_PCI_DEVICE)
		*value = 0; // dummy
	else
		ret = -EINVAL;
	return ret;
}

/* Unused
int
nouveau_setparam(struct nouveau_device *dev, uint64_t param, uint64_t value)
{
	return 0;
}
*/

int
nouveau_client_new(struct nouveau_device *dev, struct nouveau_client **pclient)
{
	struct nouveau_device_priv *nvdev = nouveau_device(dev);
	struct nouveau_client_priv *pcli;
	int id = 0, i, ret = -ENOMEM;
	uint32_t *clients;
	CALLED();

	mutexLock(&nvdev->lock);

	for (i = 0; i < nvdev->nr_client; i++) {
		id = ffs(nvdev->client[i]) - 1;
		if (id >= 0)
			goto out;
	}

	clients = realloc(nvdev->client, sizeof(uint32_t) * (i + 1));
	if (!clients)
		goto unlock;
	nvdev->client = clients;
	nvdev->client[i] = 0;
	nvdev->nr_client++;

out:
	pcli = calloc(1, sizeof(*pcli));
	if (pcli) {
		nvdev->client[i] |= (1 << id);
		pcli->base.device = dev;
		pcli->base.id = (i * 32) + id;
		ret = 0;
	}

	*pclient = &pcli->base;

unlock:
	mutexUnlock(&nvdev->lock);
	return ret;
}

void
nouveau_client_del(struct nouveau_client **pclient)
{
	struct nouveau_client_priv *pcli = nouveau_client(*pclient);
	struct nouveau_device_priv *nvdev;
	CALLED();
	if (pcli) {
		int id = pcli->base.id;
		nvdev = nouveau_device(pcli->base.device);
		mutexLock(&nvdev->lock);
		nvdev->client[id / 32] &= ~(1 << (id % 32));
		mutexUnlock(&nvdev->lock);
		cli_map_free(&pcli->base);
		free(pcli);
	}
}

static int
nouveau_bo_fence_wait(struct nouveau_bo *bo, uint32_t access)
{
	CALLED();
	struct nouveau_bo_priv *nvbo = nouveau_bo(bo);
	int ret = 0;

	if ((s32)nvbo->fence.id >= 0) {
		TRACE("waiting on fence {%d,%u}\n", (int)nvbo->fence.id, nvbo->fence.value);
		Result res = nvFenceWait(&nvbo->fence, (access & NOUVEAU_BO_NOBLOCK) ? 0 : -1);
		if (R_FAILED(res))
			ret = -EAGAIN;
		else {
			// Reset the fence since we're done with it.
			nvbo->fence.id = -1;
			nvbo->fence.value = 0;

			// TODO: Check for NOUVEAU_BO_WR - maybe we're supposed to flush cache?
		}
	}

	if (ret == 0)
		nvbo->access = 0;
	return ret;
}

/* ---------------------------------------------------------------------------
 * FlashNX: buffer-object cache
 *
 * Every texture Mesa's nvc0 driver creates gets its own buffer object, and on
 * the Switch making one is a memalign, three nvmap ioctls, a GPU address-space
 * map (each ioctl an IPC to nvservices) and a memset over uncached memory;
 * freeing one waits on its fence with NO timeout, which right after a pushbuf
 * flush means waiting for the GPU to finish that whole submission, then two
 * more IPCs. Measured on Super Mario 63 level 8-13: 58 of each per frame,
 * ~420 us per create and ~190 us per delete, about 37 ms of a 91 ms frame.
 *
 * Rotating filtered objects change their texture size every frame, so reusing
 * GL textures of the same size barely helps (23 % measured). But the driver
 * already rounds a texture's storage up to whole tiles (64-byte pitch, rows to
 * 8/16/32/64/128), so those changing sizes land in a handful of BO sizes: a
 * pool keyed by BO bytes was simulated at 99 % reuse on the same frames.
 *
 * So a freed BO is kept here and handed to the next request of the same size,
 * alignment, kind and coherence. A recycled BO must be indistinguishable from a
 * fresh one, which is what keeps this invisible above libdrm:
 *  - it is only handed out once the GPU is done with it (its fence passed), so
 *    the CPU-side reset below cannot race a GPU still reading it;
 *  - it is zeroed like a fresh one, and its fence and access are reset;
 *  - only colour-tiled BOs (kind 0xfe, generic 16Bx2) of at most 1 MiB, never
 *    one whose nvmap id left the process (name_get: display buffers) and never
 *    a CONTIG one. No compressible kinds, so no compression tags to worry about.
 * Freed entries idle for BOC_MAX_AGE_MS go back to the heap, and the cache is
 * flushed before the address space closes.
 * ------------------------------------------------------------------------- */
#define BOC_MAX_ENTRIES   128
#define BOC_MAX_BYTES     (16u << 20)
#define BOC_MAX_BO_BYTES  (1u << 20)
#define BOC_MAX_AGE_MS    3000
#define BOC_AGE_OUT_BATCH 4

enum {
	BOC_ST_NEW_N, BOC_ST_NEW_T, BOC_ST_HIT, BOC_ST_ZERO_T,
	BOC_ST_DEL_N, BOC_ST_DEL_T, BOC_ST_DEL_CACHED, BOC_ST_EVICT,
	BOC_ST_BUSY, BOC_ST_IPC,
	BOC_ST_NEW_FAIL,    /* nouveau_bo_new gave up */
	BOC_ST_RETRY_OK,    /* ... or succeeded only after emptying the cache */
	BOC_ST_COUNT
};

static Mutex boc_lock;
static struct nouveau_bo_priv *boc[BOC_MAX_ENTRIES]; /* oldest first */
static int boc_n;
static uint64_t boc_bytes;
static uint64_t boc_st[BOC_ST_COUNT];
/* The highest value seen reached on one syncpoint. Fences on a syncpoint are
 * monotonic, so anything at or below it is known done without an IPC. */
static bool boc_done_valid;
static uint32_t boc_done_id;
static uint32_t boc_done_value;
/* 1 = cache on (what ships). The instr build flips it for its A/B. */
static int boc_on = 1;

static uint64_t
boc_ms_to_ticks(uint64_t ms)
{
	return ms * armGetSystemTickFreq() / 1000;
}

/* Is the GPU done with this BO? No IPC when a later fence on the same syncpoint
 * is already known to have passed; otherwise one nvFenceWait with no timeout
 * (a single IPC when the fence has passed, a few when it has not). Everything
 * here assumes one GL thread, which is FlashNX's; the lock is kept anyway. */
static bool
boc_fence_done(struct nouveau_bo_priv *nvbo, bool may_ipc)
{
	uint32_t id = nvbo->fence.id, value = nvbo->fence.value;

	if ((s32)id < 0)
		return true; /* never submitted */
	if (boc_done_valid && id == boc_done_id && (s32)(value - boc_done_value) <= 0)
		return true;
	if (!may_ipc)
		return false;
	boc_st[BOC_ST_IPC]++;
	if (R_FAILED(nvFenceWait(&nvbo->fence, 0)))
		return false;
	if (!boc_done_valid || id != boc_done_id || (s32)(value - boc_done_value) > 0) {
		boc_done_valid = true;
		boc_done_id = id;
		boc_done_value = value;
	}
	return true;
}

/* The original nouveau_bo_del, minus the fence IPC when the fence is already
 * known to have passed (only while the cache is on, so the "off" windows of the
 * A/B are exactly the old code). */
static void
boc_destroy(struct nouveau_bo_priv *nvbo)
{
	struct nouveau_bo *bo = &nvbo->base;
	struct nouveau_device_priv *nvdev = nouveau_device(bo->device);

	if (!(boc_on && boc_fence_done(nvbo, false)))
		nouveau_bo_fence_wait(bo, 0);
	nvAddressSpaceUnmap(&nvdev->addr_space, bo->offset);
	nvMapClose(&nvbo->map);
	if (nvbo->map_addr)
		free(nvbo->map_addr);
	free(nvbo);
}

static void
boc_remove_at(int i)
{
	boc_bytes -= boc[i]->base.size;
	memmove(&boc[i], &boc[i + 1], (size_t)(boc_n - i - 1) * sizeof(boc[0]));
	boc_n--;
}

static void
boc_evict_oldest(void)
{
	struct nouveau_bo_priv *nvbo = boc[0];
	boc_remove_at(0);
	boc_st[BOC_ST_EVICT]++;
	boc_destroy(nvbo);
}

/* Free a few entries nobody asked for in BOC_MAX_AGE_MS. Called with the lock. */
static void
boc_age_out(void)
{
	uint64_t now = armGetSystemTick(), max_age = boc_ms_to_ticks(BOC_MAX_AGE_MS);
	int freed = 0;

	while (boc_n > 0 && freed < BOC_AGE_OUT_BATCH && now - boc[0]->cached_at > max_age) {
		boc_evict_oldest();
		freed++;
	}
}

static bool
boc_cacheable(struct nouveau_bo_priv *nvbo)
{
	return nvbo->kind == NvKind_Generic_16BX2
		&& nvbo->base.size <= BOC_MAX_BO_BYTES
		&& nvbo->map_addr != NULL
		&& !nvbo->shared
		&& !(nvbo->base.flags & NOUVEAU_BO_CONTIG);
}

/* A cached BO of this exact shape whose GPU work is done, or NULL. */
static struct nouveau_bo_priv *
boc_take(struct nouveau_device *dev, uint64_t size, uint32_t align, uint32_t kind,
         uint32_t flags)
{
	struct nouveau_bo_priv *found = NULL;
	int i;

	mutexLock(&boc_lock);
	boc_age_out();
	for (i = 0; i < boc_n; i++) {
		struct nouveau_bo_priv *nvbo = boc[i];
		if (nvbo->base.device != dev || nvbo->base.size != size || nvbo->align != align
		    || nvbo->kind != kind
		    || ((nvbo->base.flags ^ flags) & NOUVEAU_BO_COHERENT))
			continue;
		if (!boc_fence_done(nvbo, true)) {
			/* Stop at the first match still in flight. The array is in
			 * release order, not fence order, so a later entry could be
			 * done, but each probe of an unpassed fence costs a couple of
			 * IPCs and a wait: past one, a miss is cheaper. */
			boc_st[BOC_ST_BUSY]++;
			break;
		}
		boc_remove_at(i);
		found = nvbo;
		break;
	}
	mutexUnlock(&boc_lock);
	return found;
}

/* Keep a freed BO. False when it cannot be kept and must be destroyed. */
static bool
boc_put(struct nouveau_bo_priv *nvbo)
{
	bool kept = false;

	if (!boc_on || !boc_cacheable(nvbo))
		return false;
	mutexLock(&boc_lock);
	boc_age_out();
	while (boc_n > 0 && (boc_n >= BOC_MAX_ENTRIES
	                     || boc_bytes + nvbo->base.size > BOC_MAX_BYTES))
		boc_evict_oldest();
	if (boc_n < BOC_MAX_ENTRIES && boc_bytes + nvbo->base.size <= BOC_MAX_BYTES) {
		nvbo->cached_at = armGetSystemTick();
		boc[boc_n++] = nvbo;
		boc_bytes += nvbo->base.size;
		kept = true;
	}
	mutexUnlock(&boc_lock);
	return kept;
}

static void
boc_flush(void)
{
	mutexLock(&boc_lock);
	while (boc_n > 0)
		boc_evict_oldest();
	/* Forget what was known about the syncpoint: a new channel could reuse
	 * its id from a lower count, and fences would then read as passed early. */
	boc_done_valid = false;
	mutexUnlock(&boc_lock);
}

/* Give everything parked back to the heap, and keep caching afterwards. For
 * the points where the working set changes wholesale (a game starting or being
 * left), and for any allocation that finds the heap full: the parked blocks
 * live in that same heap. Returns the bytes given back. */
uint64_t
flashnx_boc_trim(void)
{
	uint64_t held;

	mutexLock(&boc_lock);
	held = boc_bytes;
	mutexUnlock(&boc_lock);
	boc_flush();
	return held;
}

/* A BO could not be made, even after emptying the cache. Mesa turns this into
 * GL_OUT_OF_MEMORY and nothing else records why, so say which step failed and
 * how big the request was, the first times. */
static void
boc_alloc_failed(const char *step, uint64_t size, uint32_t kind, Result rc)
{
	uint64_t n = ++boc_st[BOC_ST_NEW_FAIL];

	if (n <= 20 || n % 500 == 0)
		printf("libdrm: BO alloc failed at %s: %llu KB kind 0x%x rc=0x%x (cache held %d, failures=%llu)\n",
		       step, (unsigned long long)(size / 1024), (unsigned)kind, (unsigned)rc,
		       boc_n, (unsigned long long)n);
}

/* Turn the cache on or off. Off empties it, so an "off" window starts from
 * exactly the old behaviour. */
void
flashnx_boc_set(int on)
{
	on = !!on;
	if (on == boc_on)
		return;
	if (!on)
		boc_flush();
	boc_on = on;
}

/* Running totals (ticks for the _T entries), then entries and bytes held. */
void
flashnx_boc_stats(uint64_t *out, int n)
{
	int i;
	mutexLock(&boc_lock);
	for (i = 0; i < n && i < BOC_ST_COUNT; i++)
		out[i] = boc_st[i];
	if (n > BOC_ST_COUNT)
		out[BOC_ST_COUNT] = (uint64_t)boc_n;
	if (n > BOC_ST_COUNT + 1)
		out[BOC_ST_COUNT + 1] = boc_bytes;
	mutexUnlock(&boc_lock);
}

static void
nouveau_bo_del(struct nouveau_bo *bo)
{
	CALLED();
	struct nouveau_bo_priv *nvbo = nouveau_bo(bo);
	uint64_t t0 = armGetSystemTick();

	boc_st[BOC_ST_DEL_N]++;
	if (boc_put(nvbo)) {
		boc_st[BOC_ST_DEL_CACHED]++;
	} else {
		/* Under the lock like every other boc_destroy, since it reads the
		 * syncpoint tracker. */
		mutexLock(&boc_lock);
		boc_destroy(nvbo);
		mutexUnlock(&boc_lock);
	}
	boc_st[BOC_ST_DEL_T] += armGetSystemTick() - t0;
}

int
nouveau_bo_new(struct nouveau_device *dev, uint32_t flags, uint32_t align,
	       uint64_t size, union nouveau_bo_config *config,
	       struct nouveau_bo **pbo)
{
	CALLED();
	struct nouveau_device_priv *nvdev = nouveau_device(dev);
	uint64_t t0 = armGetSystemTick();

	boc_st[BOC_ST_NEW_N]++;
	if (align < 0x1000)
		align = 0x1000;
	size = (size + 0xFFF) &~ 0xFFF;

	NvKind kind = NvKind_Pitch;
	if (config)
		kind = (NvKind)config->nvc0.memtype;

	/* FlashNX: a cached BO of the same shape, reset to exactly what a fresh
	 * one would be (see the cache above). */
	if (boc_on && kind == NvKind_Generic_16BX2 && size <= BOC_MAX_BO_BYTES
	    && !(flags & NOUVEAU_BO_CONTIG)) {
		struct nouveau_bo_priv *hit = boc_take(dev, size, align, kind, flags);
		if (hit) {
			struct nouveau_bo *hbo = &hit->base;
			uint64_t z0 = armGetSystemTick();
			memset(hit->map_addr, 0, hbo->size);
			boc_st[BOC_ST_ZERO_T] += armGetSystemTick() - z0;
			atomic_set(&hit->refcnt, 1);
			hit->fence.id = UINT32_MAX;
			hit->fence.value = 0;
			hit->access = 0;
			hit->name = 0;
			hbo->flags = flags;
			hbo->map = NULL;
			if (config)
				hbo->config = *config;
			else
				memset(&hbo->config, 0, sizeof(hbo->config));
			*pbo = hbo;
			boc_st[BOC_ST_HIT]++;
			boc_st[BOC_ST_NEW_T] += armGetSystemTick() - t0;
			return 0;
		}
	}

	struct nouveau_bo_priv *nvbo = calloc(1, sizeof(*nvbo));
	struct nouveau_bo *bo = &nvbo->base;
	Result rc;

	if (!nvbo)
		return -ENOMEM;

	TRACE("Allocating BO of size %ld, align %d, flags 0x%x and kind 0x%x\n", size, align, flags, kind);
	/* FlashNX: if anything below fails, what the cache holds idle is worth
	 * less than this BO, and it lives in the same heap: empty it and retry
	 * once, so the cache can never be why an allocation failed. */
	void* mem;
	for (int attempt = 0;; attempt++) {
		bool retry = attempt == 0 && boc_n > 0;

		mem = memalign(0x1000, size);
		if (!mem)
		{
			TRACE("Out of memory\n");
			if (retry) { boc_flush(); continue; }
			boc_alloc_failed("memalign", size, kind, 0);
			free(nvbo);
			return -ENOMEM;
		}

		rc = nvMapCreate(&nvbo->map, mem, size, align, kind, false);
		if (R_FAILED(rc))
		{
			TRACE("Failed to create nvmap object (%x)\n", rc);
			free(mem);
			if (retry) { boc_flush(); continue; }
			boc_alloc_failed("nvMapCreate", size, kind, rc);
			free(nvbo);
			return -rc;
		}

		rc = nvAddressSpaceMap(&nvdev->addr_space, nvMapGetHandle(&nvbo->map), !(flags & NOUVEAU_BO_COHERENT), kind, &bo->offset);
		if (R_FAILED(rc))
		{
			TRACE("Failed to map object to address space (%x)\n", rc);
			nvMapClose(&nvbo->map);
			free(mem);
			if (retry) { boc_flush(); continue; }
			boc_alloc_failed("nvAddressSpaceMap", size, kind, rc);
			free(nvbo);
			return -rc;
		}
		if (attempt > 0)
			boc_st[BOC_ST_RETRY_OK]++;
		break;
	}

	atomic_set(&nvbo->refcnt, 1);
	bo->device = dev;
	bo->handle = nvMapGetHandle(&nvbo->map);
	bo->size = size;
	bo->flags = flags;
	nvbo->map_addr = mem;
	nvbo->fence.id = UINT32_MAX;
	nvbo->align = align;
	nvbo->kind = kind;
	memset(nvbo->map_addr, 0, bo->size);

	if (config)
		bo->config = *config;
	*pbo = bo;
	boc_st[BOC_ST_NEW_T] += armGetSystemTick() - t0;
	return 0;
}

/* Unused
static int
nouveau_bo_wrap_locked(struct nouveau_device *dev, uint32_t handle,
		       struct nouveau_bo **pbo, int name)
{
	return 0;
}

static void
nouveau_bo_make_global(struct nouveau_bo_priv *nvbo)
{
}

int
nouveau_bo_wrap(struct nouveau_device *dev, uint32_t handle,
		struct nouveau_bo **pbo)
{
	// NV30-only
	CALLED();
	return 0;
}
*/

int
nouveau_bo_name_ref(struct nouveau_device *dev, uint32_t name,
		    struct nouveau_bo **pbo)
{
	CALLED();
	struct nouveau_device_priv *nvdev = nouveau_device(dev);
	struct nouveau_bo_priv *nvbo = calloc(1, sizeof(*nvbo));
	struct nouveau_bo *bo = &nvbo->base;
	Result rc;

	rc = nvMapLoadRemote(&nvbo->map, name);
	if (R_FAILED(rc))
	{
		TRACE("Failed to load nvmap object (%x)\n", rc);
		free(nvbo);
		return -rc;
	}

	u32 handle = nvMapGetHandle(&nvbo->map);
	NvKind kind = NvKind_Generic_16BX2; // NvKind_C32_2C or NvKind_C32_2CRA could be used here, but they need special support that nouveau seems to lack.
	rc = nvAddressSpaceMap(&nvdev->addr_space, handle, true, kind, &bo->offset);
	if (R_FAILED(rc))
	{
		TRACE("Failed to map named buffer (%x)\n", rc);
		nvMapClose(&nvbo->map);
		free(nvbo);
		return -rc;
	}

	atomic_set(&nvbo->refcnt, 1);
	bo->device = dev;
	bo->handle = handle;
	bo->size = nvMapGetSize(&nvbo->map);
	bo->flags = NOUVEAU_BO_GART;
	nvbo->fence.id = UINT32_MAX;
	nvbo->shared = true; /* FlashNX: another process's memory, never recycle */
	*pbo = bo;

	bo->config.nvc0.memtype = kind;
	bo->config.nvc0.tile_mode = 0x040;
	return 0;
}

int
nouveau_bo_name_get(struct nouveau_bo *bo, uint32_t *name)
{
	CALLED();
	struct nouveau_bo_priv *nvbo = nouveau_bo(bo);

	/* FlashNX: another process can now reach this memory (display buffers
	 * handed to vi), so it must never be recycled under a new owner. */
	nvbo->shared = true;
	*name = nvMapGetId(&nvbo->map);
	return 0;
}

void
nouveau_bo_ref(struct nouveau_bo *bo, struct nouveau_bo **pref)
{
	CALLED();
	struct nouveau_bo *ref = *pref;
	if (bo) {
		atomic_inc(&nouveau_bo(bo)->refcnt);
	}
	if (ref) {
		if (atomic_dec_and_test(&nouveau_bo(ref)->refcnt))
			nouveau_bo_del(ref);
	}
	*pref = bo;
}

int
nouveau_bo_prime_handle_ref(struct nouveau_device *dev, int prime_fd,
			    struct nouveau_bo **bo)
{
	CALLED();
	return -ENOSYS;
}

int
nouveau_bo_set_prime(struct nouveau_bo *bo, int *prime_fd)
{
	CALLED();
	return -ENOSYS;
}

int
nouveau_bo_get_syncpoint(struct nouveau_bo *bo, unsigned int *out_threshold)
{
	CALLED();
	struct nouveau_bo_priv *nvbo = nouveau_bo(bo);

	if (out_threshold)
		*out_threshold = nvbo->fence.value;

	return nvbo->fence.id;
}

int
nouveau_bo_wait(struct nouveau_bo *bo, uint32_t access,
		struct nouveau_client *client)
{
	CALLED();
	struct nouveau_bo_priv *nvbo = nouveau_bo(bo);
	struct nouveau_pushbuf *push;

	if (!(access & NOUVEAU_BO_RDWR))
		return 0;

	push = cli_push_get(client, bo);
	if (push && push->channel)
		nouveau_pushbuf_kick(push, push->channel);

	if (!(nvbo->access & NOUVEAU_BO_WR) && !(access & NOUVEAU_BO_WR))
		return 0;

	return nouveau_bo_fence_wait(bo, access);
}

int
nouveau_bo_map(struct nouveau_bo *bo, uint32_t access,
	       struct nouveau_client *client)
{
	CALLED();
	struct nouveau_bo_priv *nvbo = nouveau_bo(bo);
	bo->map = nvbo->map_addr;
	return nouveau_bo_wait(bo, access, client);
}

void
nouveau_bo_unmap(struct nouveau_bo *bo)
{
	CALLED();
	bo->map = NULL;
}
