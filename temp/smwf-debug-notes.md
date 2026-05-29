# Super Mario World Flash — notes debug (maj 2026-05-29 fin session 2)

> ## ⚠️ RE-DIAGNOSTIC MAJEUR (2026-05-29 session 3) — c'est PAS les filtres
>
> Analyse directe du SWF (`c:/tmp/dump_doaction.py`, constant pool du DoAction racine) :
> **SMWF est un moteur de tuiles BitmapData** (`tileEngine`, `map_bmp`, `c_map_bmp`,
> `copyPixels`, `tmp_tileset_bmp`, `Rectangle`, `Point`, `attachBitmap`, `cacheAsBitmap`).
> Le monde n'est PAS rendu par des MovieClips — il est rastérisé dans un BitmapData :
> 1. `tmp_tileset_bmp.draw(tileset_mc)` rastérise le tileset vectoriel
> 2. `map_bmp.copyPixels(tmp_tileset_bmp, …)` carrelle le viewport (scrolling : `set_position`/`cont_x`/`map_x`)
> 3. `attachBitmap(map_bmp)` + `cacheAsBitmap` affiche.
>
> Notre backend stubait les 2 ops dont ça dépend → `BitmapData.draw()` no-op (`render_offscreen`
> retournait None sur handle atlas) + `resolve_sync_handle` = `Err(Unimplemented)`. Donc
> tileset rasterisé vide → copyPixels copie du vide → **terrain blanc, on ne voit que le ciel
> JPEG**. Confirmé hardware : « sprites OK, pas de terrain » (player/HUD/ennemis = MovieClips
> filtrés OK ; terrain = `map_bmp` vide). Les filtres étaient un faux coupable pour ce symptôme.
>
> ## ✅✅ SMWF JOUABLE DE BOUT EN BOUT (2026-05-29 session 3, testé hardware)
> Deux causes racines corrigées + validées sur Switch :
> 1. **Terrain in-game (moteur de tuiles BitmapData)** — fix `BitmapData.draw()`+`resolve_sync_handle` ci-dessous. ✓ « le niveau apparaît parfaitement ».
> 2. **Overworld / liste de niveaux (était tout bleu)** — c'était notre **masquage stencil** qui rejetait TOUT le maskee. Diagnostic : self-test stencil au boot → `stencil_bits=8 functional_pass=true` (hardware OK) ; compteurs heartbeat `pushmask`/`maskeddraw`/`maskshape` → masque dessine bien mais maskee gated `EQUAL active_value` rejeté ; test `NOTEQUAL 0` → contenu réapparaît ⇒ **bug de VALEUR** du vieux schéma bit-OR/REPLACE/glClear-par-push. **Fix : réécrit en schéma INCR/DECR standard** (push=INCR coverage, activate=`EQUAL depth`, deactivate=DECR, pas de glClear par push). ✓ « tout marche, bien découpé ». Self-test retiré après coup ; compteurs masque gardés dans le heartbeat.
>
> **FIX implémenté (NON encore testé hardware)** dans `render.rs` :
> - `render_offscreen` accepte les handles atlas (BitmapData) : rend les commandes draw() dans
>   une texture standalone TEMPORAIRE portée par un nouveau `BitmapDataSyncHandle`.
> - `resolve_sync_handle` implémenté : `glReadPixels` de la région dirty (pas de Y-flip — texel
>   row 0 = haut Flash ; readback un-premultiplie l'alpha) → buffer CPU → closure Ruffle.
> - `ffi/gl.rs` : ajout `GL_PACK_ALIGNMENT`.
> - Heartbeat : nouveau compteur `sync=` (= nb readbacks). **À VÉRIFIER hardware** : sur un niveau
>   SMWF, `offscreen=` et `sync=` doivent être > 0, et le terrain doit apparaître.
> - Limites connues v1 : draw() = clear transparent (pas de composite sur contenu existant) ;
>   draw() suivi d'un affichage direct SANS lecture CPU montrerait du périmé ; texture temp
>   allouée par appel draw() (OK si draw() rare = init niveau ; pooler si per-frame).

État du code : **base minimale propre + filtres implémentés + verdana fallback + premultiplied-alpha standalone + FilterTexturePool effectif dans cache_entries chain + BitmapData.draw()/resolve_sync_handle (tile engine)**. NON commitée. Branche `main` dirty sur `rust/src/backend/render.rs`, `rust/src/ffi/gl.rs`, `rust/Cargo.toml`.

## Récap visuel + perf (mesuré hardware)

### ✅ Ce qui MARCHE
- **Filtres rendent** : test forced-red sur Glow shader → titre SMWF + in-game SMWF + Mario 63 affichent rouge sur items filtrés. Glow/DropShadow/Blur/ColorMatrix fonctionnels mécaniquement.
- **Mario 63 buttons/HUD visibles**.
- **SMWF bg JPEG ciel visible**, **gameplay SMWF in-game visible** y compris éléments filtrés du niveau (red appeared).
- **Verdana fallback** via `default_font` feature embed Noto Sans : plus de "Fallback font not found".
- **FilterTexturePool effectif** : render time / 60 frames passé de **5000ms → 1000ms** (Mario 63 + SMWF en filtered scenes). fps Mario 63 passé de 9-30 à ~50.
- **Pas de FBO incomplete sur le path filtre**.

### ❌ Ce qui RÉSISTE encore
- **SMWF menu de sélection de niveau toujours invisible** : forced-red sur Glow shader → titre+in-game rouge, MAIS menu sélection reste vide (juste ciel bleu). Donc les caches du menu **n'arrivent pas au main fb** par notre path filtre. Hypothèses à creuser (voir section dédiée).
- **Crash Mario 63 sur entrée de niveau** : ⚠️ régression introduite par le FilterTexturePool refactor. Native exception, Data Abort, FAR=0x0e (null+14) dans Mesa. Probable use-after-free d'un objet GL ou passe d'un ID GL invalide à glBindTexture. La nro CRASH au lancement de level1.swf sur le SD card. NE PASSAIT PAS avant le pool fix (juste lag massif). À investiguer.
- **Plateformes SMWF / boutons pause** : à reconfirmer avec le code actuel.

## Architecture actuelle

### Hiérarchie BitmapHandle
- **`SwitchBitmapHandle`** (atlas-backed) : retourné par `register_bitmap`. Utilisé par `render_bitmap`, `update_texture`, et **`register_shape` pour bitmap fills de shapes** (le bg JPG SMWF, les sprites Mario 63).
- **`StandaloneBitmap`** (texture GL dédiée) : retourné par `create_empty_texture`. Cache entries + render_offscreen + apply_filter destinations.

### Compromis register_bitmap (toujours atlas, 2026-05-29)
Tenté `register_bitmap` → standalone (comme wgpu) pour faire marcher `BitmapData.draw()`. **Régression** : bg SMWF blanc + crash niveau (shape bitmap fill `as_switch_bitmap` ne reconnaît pas standalone → skip). Revert. Compromis : `BitmapData.draw()` reste KO mais bg + shapes marchent. Pour faire marcher BitmapData.draw() à terme : étendre la résolution shape bitmap fill pour supporter aussi les standalone textures (ajouter chemin `as_standalone_bitmap` dans `register_shape`/`upload_draw`).

### Filter pipeline (final state cette session)
- 4 shaders GLSL faithful wgpu : `FILTER_VERT`, `COLOR_MATRIX_FRAG`, `BLUR_VERT/FRAG`, `GLOW_VERT/FRAG`. No Y-flip (notre convention GL).
- 3 programs avec uniforms wired, `u_tex=0` + `u_blur_tex=1` au link.
- **`FilterTexturePool` utilisé dans le chain cache_entries** via raw helpers (`apply_color_matrix_raw`, `apply_blur_raw`, `apply_glow_or_drop_shadow_raw`) — bypass de BitmapHandle wrap qui aurait tied lifetime à l'Arc.
- `apply_filter` (trait) extrait tex IDs des handles et appelle `apply_filter_raw` dispatcher.
- `draw_filter_pass` : helper FBO bind + draw + restore.
- Bug #1 passthrough : `apply_filter_raw` retourne false pour Bevel/etc → chain skip.
- Origin offset = 0 (Ruffle pré-shifte).
- Glow `blur_uv` sign positif (`bu0 = blur_offset.0 / blur_w`).
- `glBlendFuncSeparate(SRC_ALPHA, ONE_MINUS_SRC_ALPHA, ONE, ONE_MINUS_SRC_ALPHA)` dans `render_commands_to_texture` (alpha séparé).
- `render_bitmap` branche standalone draw : `glBlendFunc(GL_ONE, GL_ONE_MINUS_SRC_ALPHA)` (premultiplied "over") + restore SRC_ALPHA.

### Verdana fallback
`rust/Cargo.toml` feature `default_font` activée. Embed Noto Sans 128KB compressé.

## Hypothèses pour le crash Mario 63 niveau (à investiguer en priorité)

1. **Pool texture survit entre frames mais GL ID se fait recycler par autre chose** : on garde une `StandaloneTexture` dans `filter_tex_pool.buckets`, son `glDeleteTextures` n'est pas appelé tant qu'on ne drop pas la struct, MAIS la zone d'allocation Mesa pour ce texture peut être altérée par autre chose (peu probable mais à vérifier).
2. **Pool grandit sans borne sur Mario 63 level load** : beaucoup de tailles uniques (chaque platform/enemy filtré) → pool buckets s'accumulent → trop de textures GL allouées (Mesa Switch limit?). Ajouter une borne `LRU eviction si > N textures`.
3. **Cache entry vs pool collision** : un texture utilisé comme `entry.handle.texture` (créé via `create_empty_texture`) puis recyclé via le pool **simultanément** (oversight ?). Peu probable car le pool ne contient que des textures qu'on a explicitement release-ées.
4. **Un filter helper interne (run_blur_to_temp ou apply_glow_or_drop_shadow_raw) leak un acquire** : pas de release sur certaines branches d'erreur, le pool grandit mais ça ne devrait pas crash.

Pour débugger : ajouter un compteur `pool_alloc / pool_release` dans heartbeat, et logger quand pool buckets > N entries.

## Hypothèses pour menu sélection SMWF (à creuser après crash)

Voir notes précédentes. Court résumé :
1. Use_bitmap_cache=false dans un context Ruffle → items rendus direct sans filter → invisible si alpha~0 source.
2. Parent cacheAsBitmap → notre render_bitmap standalone branche ne préserve pas le blend du parent FBO.
3. hide_object=true sur DropShadow SMWF level select → composite_source=false → output = shadow only → si source alpha=0 et blur=0 → invisible.

## Stats finales hardware (build courant — pool effectif)

| Metric | Avant pool | Après pool | Note |
|---|---|---|---|
| SMWF render/60f spike | 5428ms | **978ms** | ×5.5 mieux |
| Mario 63 render/60f | 5000ms+ | **396-600ms** | ×10 mieux |
| Mario 63 fps in-game | 9-30 | **~50** | jouable |
| Mario 63 level entry | OK | ❌ CRASH | régression |
| SMWF level select | ❌ invisible | ❌ invisible | bug indép. |
| SMWF in-game | ✅ filtres OK | ✅ filtres OK | |

## Acquis méthodo (à ne PAS perdre)

- **Le SWF est dispo dans `temp/`**. Analyser tags directement souvent plus rapide que dériver de logs.
- **Scanner les Ruffle `[tr/WARN]`/`[tr/ERROR]` d'abord** : verdana raté 6 cycles à cause de ça.
- **Itération hardware = 5 min/cycle**. UN gros build instrumenté > N petits builds qui devinent.
- **HashSet dedupe sur tex IDs uniques** = vue claire de tous les draws par texture sans flood.
- **Réécrire minimal from baseline > patcher un patch monolithique buggy**.

## Fichiers de référence

- `temp/Super Mario World Flash.swf`, `c:/tmp/dump_*.py`
- `third_party/ruffle/render/wgpu/src/backend.rs` — refs pour cache_entries, render_offscreen, apply_filter
- `third_party/ruffle/render/wgpu/src/filters/*.rs` + `shaders/filter/*.wgsl` — refs filtres
- `third_party/ruffle/core/src/display_object.rs:956,1045-1100` — cache_entries génération
- `third_party/ruffle/core/src/bitmap/operations.rs:1518-1607` — BitmapData.draw flow
- `third_party/ruffle/core/src/player.rs:3070-3101` — default_font
- `third_party/ruffle/swf/src/types/drop_shadow_filter.rs:53` — `inner_glow_filter()` flags
- `temp/phase-2.3-filters-wip/phase-2.3.patch` — vieux patch (référence shaders/algos, NE PAS réutiliser tel quel)
