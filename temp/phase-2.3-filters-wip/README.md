# Phase 2.3 — Flash filters (WIP, parked 2026-05-25)

Travail de portage du backend filter Ruffle (wgpu → GL natif Switch) entamé 2026-05-25 soir. **~5 h de code**, 1624 lignes de patch, compile clean en release. **Pas merge sur main** parce que le test hardware Mario 63 a révélé 3 bugs bloquants qu'il faut corriger avant.

Le code est dans [phase-2.3.patch](phase-2.3.patch). Pour le réappliquer plus tard :

```bash
git apply temp/phase-2.3-filters-wip/phase-2.3.patch
```

(le `.patch` est un `git diff HEAD --` sur `rust/src/ffi/gl.rs` + `rust/src/backend/render.rs`. Il s'applique tant que ces fichiers n'ont pas trop bougé.)

## Ce que le patch contient

**[rust/src/ffi/gl.rs](../../rust/src/ffi/gl.rs)** (+31 lignes) :
- FBO bindings : `glGenFramebuffers`, `glDeleteFramebuffers`, `glBindFramebuffer`, `glFramebufferTexture2D`, `glCheckFramebufferStatus`, `glGetIntegerv`
- Constantes `GL_FRAMEBUFFER`, `GL_COLOR_ATTACHMENT0`, `GL_FRAMEBUFFER_COMPLETE`, `GL_FRAMEBUFFER_BINDING`, `GL_VIEWPORT`, `GL_TEXTURE1`
- `glUniformMatrix4fv`, `glUniform2f`

**[rust/src/backend/render.rs](../../rust/src/backend/render.rs)** (+1384 lignes) :

| Bloc | Rôle |
|---|---|
| `StandaloneTexture` + `StandaloneBitmap` + `as_standalone_bitmap` | 2e variant de `BitmapHandle` : texture GL standalone (pas atlas), FBO-attachable, Drop = glDeleteTextures |
| `FilterTexturePool` | Pool de StandaloneTexture par `(w, h)`. Acquire/release. Pas utilisé en cache_entries (cause perf, voir bug #2) |
| `make_standalone_texture` | Alloc fresh RGBA8 transparent + clamp + linear |
| `NoOpSyncHandle` | `impl SyncHandle for ()` minimal pour retourner depuis `render_offscreen`/`apply_filter` |
| `offscreen_dims` + `offscreen_fbo` + `filter_tex_pool` fields | État backend pour FBO réutilisable + override world_matrix |
| `world_matrix_for` (top-level) | Généralise world_matrix avec dims custom + origin + flip_y. FBO utilise flip_y=false (texel(0,0) = top Flash pour sampling cohérent) |
| `render_commands_to_texture` | Bind FBO, save/restore state, replay CommandList avec viewport offscreen |
| `draw_filter_pass` | Helper générique : unit quad → FBO + closure setup_uniforms. Disable blend, blit + restore |
| `blit_identity` | Copy via ColorMatrix avec matrice identité |
| `render_offscreen` (vraie impl) | Wrap autour render_commands_to_texture avec clear transparent |
| `submit_frame` cache_entries | Loop : render initial → chain filters → identity-blit final → entry.handle |

**4 shaders portés WGSL → GLSL 330 core** (faithful) :

| Shader | Source WGSL | Ce qu'il fait |
|---|---|---|
| `FILTER_VERT` | `shader_filter_common.wgsl` | Unit quad → NDC, UV remap depuis u_src_uv |
| `COLOR_MATRIX_FRAG` | `color_matrix.wgsl` | 4×5 matrix multiply en mode unpremultiplied, re-premultiply final |
| `BLUR_VERT` + `BLUR_FRAG` | `blur.wgsl` | Séparable Gaussian, pre-shifted center loop, fused fractional last-pair |
| `GLOW_VERT` + `GLOW_FRAG` | `glow.wgsl` | inner/outer × knockout × composite_source variants. 2 textures (source + blurred) |

**Dispatcher** :
- `apply_color_matrix_filter` (gère source==dest via temp pool)
- `apply_blur_filter` + helper réutilisable `run_blur_to_temp` (ping-pong H/V × num_passes)
- `apply_glow_or_drop_shadow` (Glow normal = offset (0,0), DropShadow = offset (-cos·dist, -sin·dist))
- `apply_filter` (impl trait) match sur `Filter::*` → un de ces helpers
- `is_filter_supported` retourne true pour ColorMatrix/Blur/Glow/DropShadow
- `is_offscreen_supported` retourne true

## Bugs identifiés sur hardware (à fixer avant ré-application)

### Bug #1 — Boutons Mario 63 invisibles (CRITIQUE)

Mario 63 utilise **`Bevel`** filter dans ses menus (taille typique 234x147, 286x176, 297x203). On ne l'implémente pas, donc notre dispatcher retourne `None`. Notre boucle cache_entries fait :

```rust
let res = self.apply_filter(...);
if res.is_none() {
    chain_ok = false;
    break;
}
```

→ Quand un cache_entry contient Bevel dans son filter chain, on skip **toute l'entry** (`continue`). Le sprite n'est jamais blit dans `entry.handle.texture` → invisible à l'écran.

**Fix** : passthrough silencieux. Si un filter retourne None, garder `current_handle` inchangé et essayer le filter suivant. Le sprite reste visible (sans ce filter), les filters supportés s'appliquent quand même.

```rust
let res = self.apply_filter(current_handle.clone(), ..., filter);
if res.is_some() {
    current_handle = next_handle;
}
// else : filter not supported, skip — pas de break
```

### Bug #2 — Performance : 30 fps → 5 fps

Heartbeat hardware f840 : `fps=5.5 render=7460ms/60frames = 124ms/frame`. Cause : `make_standalone_texture` dans la boucle cache_entries = `glGenTextures + glTexImage2D + glDeleteTextures` × 2-3 fois × chaque cache_entry × chaque frame. Sur Mesa Switch ça flush la queue à chaque appel.

À cela s'ajoute `glGetIntegerv(GL_FRAMEBUFFER_BINDING)` + `glGetIntegerv(GL_VIEWPORT)` dans chaque `draw_filter_pass` qui force un sync CPU-GPU.

**Fixes possibles** (à combiner) :

1. **Utiliser `FilterTexturePool` dans la boucle cache_entries**. Blocage actuel : on wrappe `StandaloneTexture` dans `Arc<StandaloneBitmap>` pour pouvoir la passer comme `BitmapHandle` au dispatcher `apply_filter`. Le `Drop` de `StandaloneTexture` détruit la GL texture → pas de release au pool. Solutions :
   - Avoir un type `PoolBorrowedTexture` qui release au lieu de delete dans Drop, et expose une méthode pour wrap en BitmapHandle temporaire.
   - Ou écrire un parallel set de helpers `apply_X_filter_tex(src_tex, src_w, src_h, src_pt, src_sz, dst_tex, ...)` qui prennent des textures raw (au lieu de BitmapHandle) → utilisé depuis cache_entries.

2. **Mirror Rust-side du FBO binding + viewport** dans `SwitchRenderBackend` (cf. `GlStateCache`) pour éliminer les `glGetIntegerv`. Track le binding courant dans un `Cell<GLuint>` + viewport `Cell<[GLint;4]>`. Restore quand on revient au main framebuffer (set binding=0, viewport=full).

3. **Réduire le nombre de FBO binds** : si la séquence est `render init → filter1 → filter2 → identity blit`, chaque étape fait bind+unbind. Optimiser pour batch.

### Bug #3 — Text cassé (à diagnostiquer)

User report : "tout cassé ce qui est text". Pas de diag clair. Hypothèses à tester en isolation :

1. **C'est juste Bug #1** : le text était dans des cache entries avec Bevel ou un autre filter non-supporté → entry skip → text invisible. Fix Bug #1 et regarder si c'est résolu.
2. **Y-flip incorrect** : on render avec flip_y=false dans `render_commands_to_texture`. Si Mario 63 utilise un fontrendering qui assume flip_y=true, le glyph orientation peut être inversée. Test : flip dans la convention main framebuffer, voir si ça aide.
3. **ColorMatrix unpremultiply bug** : le shader divise `src.rgb / src.a` (assume premultiplied alpha source). Si le text est rendu non-premultiplié et qu'on applique une ColorMatrix par-dessus, la division peut produire des couleurs > 1 qui sont clamped. Vérifier la convention alpha dans Mario 63 cache textures.

Méthodologie suggérée : flip `is_filter_supported = true` pour UN seul filter type à la fois, observer le comportement, isoler.

## Stratégie de réapplication recommandée

1. Apply patch : `git apply temp/phase-2.3-filters-wip/phase-2.3.patch`
2. Fix Bug #1 (5 min) : la boucle cache_entries ne break plus sur None
3. Build + test hardware avec **`is_filter_supported = false`** + **`is_offscreen_supported = false`** d'abord, confirmer pas de régression
4. Flip `is_offscreen_supported = true` seul. Test : Mario 63 doit charger pareil qu'avant (rien ne change visuellement, juste les cache_entries entrent mais sans filter chain qui se déclenche puisque is_filter_supported = false)
5. Flip `is_filter_supported` à true uniquement pour `ColorMatrixFilter`. Test sur hardware. Si OK → étape suivante. Si bug → diagnostic isolé.
6. Idem Blur, puis Glow, puis DropShadow.
7. À chaque étape valider perf via heartbeat (fps + render ms).
8. Fix Bug #2 (pool dans cache_entries) en // ou avant l'étape 5 si la perf chute trop tôt.

## Pourquoi pas commit sur une branche WIP au lieu de patch ?

L'utilisateur a explicitement demandé "mettons les de côté dans un dossier du projet". Le patch + README est plus visible qu'une branche, et survit aux opérations git agressives. Si tu préfères une branche tu peux toujours faire :

```bash
git checkout -b wip/phase-2.3-filters
git apply temp/phase-2.3-filters-wip/phase-2.3.patch
git add rust/src/
git commit -m "Phase 2.3 WIP : filtres Flash (3 bugs hardware à fixer)"
```
