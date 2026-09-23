#include <stdio.h>
#include <stdlib.h>
#include "private.h"

#ifdef DEBUG
#	define TRACE(x...) printf("nouveau: " x)
#	define CALLED() TRACE("CALLED: %s\n", __PRETTY_FUNCTION__)
#else
#	define TRACE(x...)
# define CALLED()
#endif

static inline unsigned bo_map_hash(struct nouveau_bo *bo)
{
	return bo->handle % BO_MAP_NUM_BUCKETS;
}

static inline struct nouveau_client_bo_map_entry *bo_map_lookup(struct nouveau_client_bo_map *bomap, struct nouveau_bo *bo)
{
	struct nouveau_client_bo_map_entry *ent;
	for (ent = bomap->buckets[bo_map_hash(bo)]; ent; ent = ent->next)
		if (ent->bo_handle == bo->handle)
			break;
	return ent;
}

void
cli_map_free(struct nouveau_client *client)
{
	struct nouveau_client_bo_map *bomap = &nouveau_client(client)->bomap;
	unsigned i;

	// Free all buckets
	for (i = 0; i < BO_MAP_NUM_BUCKETS+1; i ++) {
		struct nouveau_client_bo_map_entry *ent, *next;
		for (ent = bomap->buckets[i]; ent; ent = next) {
			next = ent->next;
			free(ent);
		}
	}
}

struct drm_nouveau_gem_pushbuf_bo *
cli_kref_get(struct nouveau_client *client, struct nouveau_bo *bo)
{
	struct nouveau_client_bo_map *bomap = &nouveau_client(client)->bomap;
	struct nouveau_client_bo_map_entry *ent = bo_map_lookup(bomap, bo);
	struct drm_nouveau_gem_pushbuf_bo *kref = NULL;
	if (ent)
		kref = ent->kref;
	return kref;
}

struct nouveau_pushbuf *
cli_push_get(struct nouveau_client *client, struct nouveau_bo *bo)
{
	struct nouveau_client_bo_map *bomap = &nouveau_client(client)->bomap;
	struct nouveau_client_bo_map_entry *ent = bo_map_lookup(bomap, bo);
	struct nouveau_pushbuf *push = NULL;
	if (ent)
		push = ent->push;
	return push;
}

static struct nouveau_client_bo_map_entry *bo_map_get_free(struct nouveau_client_bo_map *bomap)
{
	// Try to find an entry first in the bucket of free entries,
	// and if said bucket is empty then allocate a new entry
	struct nouveau_client_bo_map_entry *ent = bomap->buckets[BO_MAP_NUM_BUCKETS];
	if (ent)
		bomap->buckets[BO_MAP_NUM_BUCKETS] = ent->next;
	else
		ent = malloc(sizeof(*ent));
	return ent;
}

void
cli_kref_set(struct nouveau_client *client, struct nouveau_bo *bo,
             struct drm_nouveau_gem_pushbuf_bo *kref,
             struct nouveau_pushbuf *push)
{
	struct nouveau_client_bo_map *bomap = &nouveau_client(client)->bomap;
	struct nouveau_client_bo_map_entry *ent = bo_map_lookup(bomap, bo);

	TRACE("setting 0x%x <-- {%p,%p}\n", bo->handle, kref, push);

	if (!ent) {
		// Do nothing if the user wanted to free the entry anyway
		if (!kref && !push)
			return;

		// Try to get a free entry for this bo
		ent = bo_map_get_free(bomap);
		if (!ent) {
			// Shouldn't we panic here?
			TRACE("panic: out of memory\n");
			return;
		}

		// Add entry to bucket list
		unsigned hash = bo_map_hash(bo);
		ent->next = bomap->buckets[hash];
		if (ent->next)
			ent->next->prev_next = &ent->next;
		ent->prev_next = &bomap->buckets[hash];
		ent->bo_handle = bo->handle;
		bomap->buckets[hash] = ent;
	}

	if (kref || push) {
		// Update the entry
		ent->kref = kref;
		ent->push = push;
	}
	else {
		// Unlink the entry, and put it in the bucket of free entries
		*ent->prev_next = ent->next;
		if (ent->next)
			ent->next->prev_next = ent->prev_next;
		ent->next = bomap->buckets[BO_MAP_NUM_BUCKETS];
		bomap->buckets[BO_MAP_NUM_BUCKETS] = ent;
	}
}
