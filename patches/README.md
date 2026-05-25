# Patches Ruffle (third_party/ruffle)

Patches locales appliquées au submodule `third_party/ruffle/`. Doivent être
ré-appliquées après tout `git submodule update --remote`.

## Application

```bash
# Depuis la racine du projet :
cd third_party/ruffle
for p in ../../patches/*.patch; do
    git apply "$p"
done
```

## Liste

### 0001-mario63-zero-scale-hit-test.patch

**Fix Phase 2.4.a — Toad château manquant (issue #6906).**

Ajoute un guard zero-determinant matrix dans `hit_test_bounds` et
`hit_test_shape` de `core/src/display_object.rs`. Sans ce patch, Mario 63
considère un placeholder MC zero-scale du château comme "hittable", ce qui
casse la chaîne de logique : Toad NPC pas instancié, Mario qui flotte dans
le vide, progression bloquée.

À soumettre comme PR upstream pour ressusciter et fermer #6906 pour de bon.
Le patch est aussi utile à tout autre frontend Ruffle (Web, desktop) — la
parité Flash Player Adobe est correcte.
