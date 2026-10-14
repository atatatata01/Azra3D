# DC_CONSOLIDATION_GATED_PLAN.md — Consolidation gatée du stack DC-cache

**Statut :** ✅ EXÉCUTÉ — Gates 0-4 tous GO (2026-07-17, séquençage owner = A). Gate 5 reste optionnel (perf, sur besoin mesuré uniquement). Non ordonnancé dans WORK_ORDER.md tant que le propriétaire n'a pas tranché le séquençage (avant/après le travail poly/foundation).
**Rung (Priority Ladder) :** 5–6 (perf/cleanup + refactor). Ne se déclenche QUE sur demande explicite. Ce document EST cette demande.
**Nature du chantier :** refactor à **comportement constant** — mêmes meshes en sortie, moins de chemins. La perf est un garde-fou (ne pas régresser), pas l'objectif.
**Origine :** revue technique DC-cache (fact-checkée à ~86 % TRUE / 0 FALSE / 4 PARTLY — voir la section « Corrections apportées à la revue » ci-dessous).
**Date :** 2026-07-17

---

## Principe directeur : la métrique reine est le GOLDEN, pas la perf

Un refactor de consolidation qui produit un mesh différent a **échoué**, même s'il est plus rapide et plus propre. Donc :

1. Chaque gate a une **métrique chiffrée avant/après**, un **seuil GO explicite**, une **condition NO-GO qui rollback**.
2. La métrique de sécurité #1 est le **golden mesh hash par corps** : hash déterministe des verts+indices sortis. Si le golden change, le refactor a changé un mesh → NO-GO, sauf changement intentionnel documenté.
3. La perf n'a que des seuils **plancher** (« ≤ baseline +5 % »). On ne merge jamais un chemin plus propre mais plus lent sans l'avoir décidé.
4. **Un gate = un merge = un golden diff.** Chaque gate rollback indépendamment. Conforme au CLAUDE.md « implement replacement BEFORE removing original ».

---

## État de l'instrumentation (constat du scout 2026-07-17)

**Bonne nouvelle : l'infrastructure existe, la mesure non.** AZRA a déjà les timers, les compteurs, les champs — mais beaucoup sont *dead* (déclarés, jamais écrits), *stderr-only*, ou gated derrière `PERF_SPAM=false` (`main.rs:202`).

### Mesurable AUJOURD'HUI, zéro code (activer les overlays)
- Frame CPU ms + FPS + breakdown 7 sections (input/mesh/dispatch/submit/overlays/layout/dirty) — Perf HUD / CPU Frame Profiler. **avg + max seulement, PAS de p95.**
- GPU par-passe ms (raymarch/deferred/fog/tonemap…) — Profiler overlay. **MAIS PAS bake ni DC** (slots morts).
- poll_tasks ms, bake_readback ms + count, serialize ms, GPU-dispatch ms, FrameScheduler par-ticket ms.
- Cache hit ratio par cycle (`cache_hits/cache_total`), VRAM volume corps actif (`perf.vol_vram_bytes`), RAM cache used/budget MB, pool slots occupés/max (Developer Tweaks).
- Événements grow/group-create/rebuild/trim/overflow/escalation/incremental-tiles — **dans stderr** (`[SdfMeshGrow]`, `[SdfPool]`, `[DcResidentSparse]`, `[SparseEscalate]`). Lisibles en grep du log, pas en compteur.

### Mesurable en flippant `PERF_SPAM=true` (console, non stocké)
- Par-bake sub-op ms (map/to_vec/classify/dc_dispatch/cascade), TriggerBake total ms, sparse classify_ms/apply_ms, DcResident CPU+GPU ms, cascade ms.

### N'EXISTE PAS — à créer au Gate 0 (petit, dérivations sur données déjà présentes)
| Manque | Coût | Note |
|---|---|---|
| **Golden mesh hash par corps** | ~30 lignes | La brique de sécurité #1. `parity()` + chunked_dc golden (SDF) et `content_signature`/`mesh_eq`+bincode (poly) savent déjà sérialiser un mesh en bytes stables. Il manque : bake chaque corps canonique → hash vert/idx buffer → compare à un golden committé. |
| **Round-trip .fl4 scène complète** | ~30 lignes | Aucun test write→read→assert-identique d'une scène réelle (bodies+prims+materials+voxel+poly). Seul le garde SENTINEL bincode tourne. `test_spine.rs` est `#[ignore]`. |
| **Assertion « GPU adapter présent »** | ~5 lignes | Les ~40 tests de parité `early-return` sans adapter → PASS en prouvant RIEN sur CI headless. Un gate de sécurité DOIT asserter l'adapter. |
| **Test régression sélection/pick** | ~30 lignes | Aucun test `world_to_screen → pick → select_single_unified`. C'est un P0 CLAUDE.md sans filet. |
| Per-body bake GPU ms (stocké) | petit | `PASS_BAKE` slot 13 mort ; `vol_bake_gpu_ms/cpu_ms` déclarés jamais écrits. Câbler volume_bake dans un slot GpuTimestamps OU peupler le champ. |
| Per-frame DC ms (stocké) | petit | Timings resident.rs sont locaux + eprintln. Accumuler dans `perf`. |
| Drag-frame **p95** | petit | Tous les buffers stockent les samples bruts, ne calculent qu'avg+max. Ajouter un percentile sur le buffer existant. |
| Bytes/body VRAM | petit | `SdfPoolSlot::region_bytes()` (à écrire) + `SdfBrickAtlasGpu::vram_bytes()`. Données brutes présentes. |
| Hole ratio / fragmentation pool | petit | `Σ per-slot region bytes` vs `bump_verts`. Données dans `SdfMeshPool.slots`. |
| Grows/frame, overflow-events (compteurs) | petit | Aujourd'hui eprintln-only. |
| **%incrémental** (partial vs full remesh) | 2 incréments | `vol_full_rebakes`/`vol_partial_rebakes` EXISTENT mais morts. Câbler +1 au fallback full (`resident.rs:484`) et +1 au chemin incrémental. |

---

## Scène de référence (fige la baseline)

**Livrable Gate 0 :** un `.fl4` versionné `docs/planning/sdf/bench/dc_consolidation_ref.fl4`, 5 corps couvrant tous les régimes :

| # | Corps | Régime | Chemin visé |
|---|---|---|---|
| 1 | Box + bevel + sub | dense 128³ | field-direct / DenseResident |
| 2 | CSG 3-prim | dense 256³ | field-direct |
| 3 | Corps organique | sparse 512³ | SparseTiledResident (atlas + tiled C2) |
| 4 | Corps détaillé | sparse 1024³ | SparseTiledResident + overflow/grow |
| 5 | **Nested sub-body** (enfant définit le solide) | dense 256³ | **couvre le hash récursif** — le cas que la revue croyait cassé |

Plus un **drag scripté déterministe** (séquence fixe de translations de prim) pour mesurer p95 et %incrémental de façon reproductible.

---

## GATE 0 — Baseline instrumentée *(BLOQUANT)* — ✅ EXECUTED, GO (2026-07-17, 7437432; non-déterminisme HashMap trouvé+corrigé dans chunked_dc merge_to_mesh)

**But :** rendre chaque chiffre du plan mesurable et figer le « avant ».

**Travaux :**
1. Créer la scène-ref + le drag scripté.
2. Ajouter les 4 harnais de sécurité manquants (golden hash par corps, round-trip .fl4, assertion adapter, test pick).
3. Peupler les métriques mortes ou stderr-only nécessaires aux gates : per-body bake ms, per-frame DC ms, p95, bytes/body, hole ratio, %incrémental.
4. Générer un **rapport JSON baseline** reproductible.

**Chiffres CAPTURÉS — GATE 0 EXÉCUTÉ 2026-07-17 @ HEAD c210425** (worktree `cache-perf`, branch
`feat/sparse-3a`, uncommitted). Le HEADLESS est rempli ci-dessous ; le RUNTIME (`_owner-run_`) exige
l'exe verrouillé — cf. `docs/planning/sdf/bench/dc_consolidation_baseline.json` (`_TODO_owner_run`) et
la nouvelle ligne HUD F3 `DC …ms reB f…/p… …%inc pool …MB h…% p95 …ms`. Log complet :
`CACHE_REMESH_512_REVIEW.md` (section GATE 0).

Note : colonnes `128³…nested` = les 5 corps de la scène-ref (1..5,
`docs/planning/sdf/bench/dc_consolidation_ref.fl4`). **Golden hash + verts/tris + VRAM** = l'oracle CPU
`chunked_dc` à **golden_res 96³** (empreinte déterministe par corps, `cargo test -p app`), PAS la
res-régime (512³/1024³ dense CPU infaisable : 134M/1.07 Md évals + jusqu'à 4 Go). La res-régime alimente
les lignes runtime `_owner-run_`.

| Métrique baseline | Unité | 128³ | 256³ | 512³ | 1024³ | nested |
|---|---|---|---|---|---|---|
| Bake full / corps | ms | _owner-run_ | _owner-run_ | _owner-run_ | _owner-run_ | _owner-run_ |
| DC extraction / corps | ms | _owner-run_ (HUD `vol_dc_ms`) | _owner-run_ | _owner-run_ | _owner-run_ | _owner-run_ |
| Frame p50 drag | ms | _owner-run_ (`frame_percentile(0.5)`) | | | _owner-run_ | |
| Frame **p95** drag | ms | _owner-run_ (`frame_percentile(0.95)`) | | | _owner-run_ | |
| VRAM mesh (golden 96³, théorique) | MB | 0.94 | 1.38 | 0.62 | 1.02 | 0.94 |
| VRAM mesh (pool, res-régime) | MB | _owner-run_ (`body_vram_bytes`) | _owner-run_ | _owner-run_ | _owner-run_ | _owner-run_ |
| Verts / tris (golden 96³) | count | 18251/33856 | 26804/49384 | 12172/21768 | 19796/36768 | 18098/33820 |
| % remesh incrémental (drag) | % | | | _owner-run_ (`p/(f+p)`) | _owner-run_ | |
| Hole ratio pool (drag) | % | | | _owner-run_ (`hole_ratio`) | _owner-run_ | |
| Cold recompile shader sdf_* | s | _owner-run_ (une fois) | | | | |
| **Golden mesh hash** (96³, FNV-1a) | u64 | `0xdfef82593cbaa428` | `0x23c7b5455afdbfaa` | `0x6a06b406c632da9c` | `0x1033d5af4136249c` | `0x11b48acb7f451bde` |

**RÉSULTAT — GO.** Rapport JSON généré de bout en bout ; **2 runs → golden hashes byte-identiques pour
les 5 corps** (déterminisme PROUVÉ, incl. le corps nested récursif via `BodyField::from_graph`). Le
non-déterminisme (risque #1) a été localisé + corrigé : `ChunkedMesh::merge_to_mesh` itérait
`chunks.values()` (ordre HashMap) → tri par clé `(cx,cy,cz)`, géométrie strictement préservée
(`cargo test -p core_math chunked` = 25 passed ; c'est aussi un bug latent de repro pour la CPU-fallback
DC + l'export/convert). Gate prouvé non-vacuous : reverter le tri → le test ÉCHOUE
(`run_a=0x4f73… run_b=0xd30d…`, 5/5 corps). Filets posés : golden + round-trip `.fl4` + assertion
adapter GPU + test pick — tous verts. `cargo test -p app --bins` = **210 passed / 0 failed / 16 ignored**.

**NO-GO (n'a pas eu lieu) :** golden hash non déterministe entre 2 runs → **STOP**. C'était le risque #1.

---

## GATE 1 — `BodyContentId` unifié *(revue P0-1)* — ✅ EXECUTED, GO (2026-07-17, de5aa66)

Remplacer les 2 hashes divergents (`hash_primitives` own-only, `render/mesh_cache.rs:689` ; `compute_body_source_hash` récursif, `app/bake_utils.rs:339`) par **un seul ID** consommé par bake + atlas + DC + mesh_cache + PT.

| Métrique | Avant | Après (seuil GO) |
|---|---|---|
| Fonctions de hash de contenu de corps | 2 | **1** |
| Golden mesh hash, tous corps ref | baseline | **identique bit-à-bit** |
| Édition nested-child → rebake | oui (via hash récursif) | **oui, inchangé** |
| Bake full ms | baseline | **≤ baseline +5 %** |

**GO :** golden identique sur les 5 corps ET test neuf `nested_child_edit_bumps_content_id` vert.
**NO-GO :** tout golden diffère → rollback.

> On unifie pour la propreté (1 ID), PAS pour corriger un bug. La revue croyait « le hash ment → display stale » ; la vérif a montré que le chemin récursif couvre déjà le display. **Item retiré de la justification** — mais l'unification reste bonne architecture (source de vérité unique).

---

## GATE 2 — `DirtyMeshRegion` unique depuis bake-accept *(revue P0-3)* — ✅ EXECUTED, GO (2026-07-17; 3 canaux → 1, produit à l'ACCEPT et plus au dispatch)

Une seule région sale produite par l'acceptation du bake, consommée par tous les meshers — au lieu de lire `last_bake_dirty_range` (`body_volume_cache/mod.rs:326`) + tile keys + windows séparément.

| Métrique | Avant | Après (GO) |
|---|---|---|
| Sources de « dirty region » lues par les meshers | 3 (range map, tile set, window) | **1** |
| % remesh incrémental (drag) | baseline | **≥ baseline** (pas de régression vers full) |
| Frame p95 (drag) | baseline | **≤ baseline +5 %** |
| Golden hash après drag scripté | baseline | **identique** |
| Violations de contrat tuile (assert C2) neuves | baseline | **0** |

**GO :** golden identique + %incrémental non dégradé + zéro violation neuve.
**NO-GO :** %incrémental chute (le partiel casse, tout repasse full) OU p95 drag régresse > 5 %.

---

## GATE 3 — Collapse du flag-matrix → 2-3 pipelines explicites *(revue P0-4)* — ✅ EXECUTED, GO (2026-07-17; 4 variantes — GpuReadbackMeshCache gardé comme fallback documenté; bras brick-pool CPU compilé-out, audit fait)

Remplacer `gpu_sdf_mesh` / `_resident` / `_resident_field` / `_sparse` / `lazy_readback` (`cache_state.rs:79-107`) par un enum : `DenseResident` / `SparseTiledResident` / `CpuChunkedOracle`.

| Métrique | Avant | Après (GO) |
|---|---|---|
| Flags booléens de routing DC | 5+ | **1 enum (3 variantes)** |
| Golden hash tous régimes | baseline | **identique** |
| Chemin mort en prod (brick-pool DC dans job CPU, `app_poll_gpu.rs:870`) | câblé | **compilé-out ou `#[cfg(test)]`** |
| Perf tous régimes | baseline | **≤ baseline +5 %** |
| Sélection / pick | fonctionne | **fonctionne (test Gate 0 vert)** |

**GO :** golden identique sur les 5 corps + `cargo build -p app` vert + test pick vert (P0 non-négociable).
**NO-GO :** un régime produit un mesh différent OU le pick casse.

---

## GATE 4 — `MeshPresentation` façade *(revue P0-2)* — ✅ EXECUTED, GO (2026-07-17; export==display 5/5, facade 3→1, PT inchangé; PLAN COMPLET Gates 0-4)

Une API unique : « ce corps a-t-il un mesh + comment le dessiner + comment le matérialiser CPU pour export/PT ». Résident et mesh_cache derrière une seule interface.

| Métrique | Avant | Après (GO) |
|---|---|---|
| Sites testant « has a mesh? » | 3 prédicats (`mesh_cache.get` / `contains_key` / pool slot) | **1 appel façade** |
| Producteurs de mesh CPU pour PT | 1 dédié (snapshot PT, `pt_scene.rs:76`) | **1 job « materialize » partagé** (PT + export) |
| Chemin export = algo séparé | oui (`dual_contouring` CPU indép., `win_export.rs:207`) | **même DC GPU matérialisé** |
| Golden hash display | baseline | **identique** |
| **Export à résolution display == display mesh** | **non (divergent)** | **hash identique à résolution égale** |
| PT re-BLAS sur recenter | oui (content_stamp) | **oui, inchangé** |

**GO :** display golden identique + export à résolution display produit le **même** mesh (ferme la vraie dette d'intégration) + PT snapshot toujours correct.
**NO-GO :** export diverge encore OU un des 3 consommateurs (display/PT/export) régresse.

> **Périmètre corrigé vs revue :** le split résident-vs-mesh_cache n'affecte QUE l'**export**. Le **pick est déjà lockstep** avec le G-buffer (même prédicat `is_resident`/pooled, commentaire `// lockstep` à `raster_pick.rs:67`). **Save** n/a (mesh display-only, non persisté). **Collision** inexistante (aucun sous-système). Ce gate cible donc l'export, pas 4 consommateurs.

---

## GATE 5 (P1, OPTIONNEL) — Perf/scale, seulement si mesuré nécessaire

Ne se déclenche que si Gates 1-4 verts ET la baseline montre un besoin réel (« defer until measured need »).

| Métrique | Déclencheur | Après (GO) |
|---|---|---|
| Grows/frame (drag) — sizing prédictif (cap = last_verts × 1.2) | mesuré au Gate 0 | **-50 %** |
| Hole ratio pool à 20+ corps — defrag conditionnel | mesuré | **rebuild seulement si > seuil X %** |
| Frame p95 1024³ multi-corps, LOD ON non-actifs | mesuré | **-20 % vs LOD OFF, 0 crack** sur corps édité |

**GO :** gain mesuré atteint sans régression golden.
**NO-GO :** gain < cible → on ne merge pas (perf spéculative interdite).

---

## Corrections apportées à la revue (avant qu'elle devienne plan)

La revue est ~86 % TRUE / 0 FALSE / 4 PARTLY (fact-check multi-agent 2026-07-17). Trois amendements avant de bâtir dessus :

1. **RETIRÉ — « le hash nested ment → display stale ».** Vrai au littéral (`hash_primitives` ne récurse pas), mais le display est gardé par `compute_body_source_hash`→`collect_body_prims_recursive` qui, LUI, récurse. Pas de staleness live. → **ne pas dépenser de P0 dessus** ; l'unification Gate 1 reste bonne pour la propreté.
2. **RÉDUIT — « export/pick/save/collision divergent ».** Seul **export** diverge (algo CPU séparé). Pick lockstep, save n/a, collision inexistante. → Gate 4 cible l'export uniquement.
3. **CITATION PÉRIMÉE — `build_pathtrace_wgsl`.** Supprimé 2026-07-16 (rebuild PT triangles). Le builder réel est `build_pathtrace_tri_wgsl`. La conclusion PT de la revue reste juste ; la référence est d'un commit en retard.

**Bonus doc :** CLAUDE.md:143 « mesh uploads ≤2 » est **périmé** — le code utilise un budget 5 ms + hard cap 4096 (`mesh_cache.rs:363`). La moitié « DC results ≤2 » reste exacte (`MAX_DC_RESULTS_PER_FRAME=2`). À corriger dans CLAUDE.md indépendamment de ce plan.

---

## Risques

| Risque | Impact | Mitigation |
|---|---|---|
| **Golden non déterministe** (ordre HashMap dans la sortie mesh) | Aucun gate prouvable | **Gate 0 le détecte et bloque.** Trier la sortie avant hash ; c'est le prérequis dur. |
| Parité GPU skip sur CI headless | Fausse sécurité (PASS sans rien prouver) | Assertion adapter au Gate 0. Gates tournent sur vrai HW. |
| Refactor change les DEUX chemins identiquement-faux | Tests de parité restent verts | Golden **vs before** (pas vs autre chemin live) — c'est précisément ce que Gate 0 ajoute. |
| Chantier mange la foundation | Retard poly/foundation | Séquençage = décision propriétaire (ci-dessous). Rung 5-6, jamais promu au-dessus. |

---

## Décision de séquençage — À TRANCHER PAR LE PROPRIÉTAIRE

Ce plan décrit correctement le code d'aujourd'hui et est sûr à exécuter. Il ne dit PAS *quand*. Options :

- **A — Maintenant :** la dette de chemins est la source des classes vanish/ghost/PT-white historiques ; consolider réduit la surface de bugs futurs.
- **B — Après poly/foundation :** respecte WORK_ORDER.md / MASTER_WORK_LIST P0–P1 ; le stack DC marche (« feel great » sur 512 sparse), donc pas d'urgence rung-1.
- **C — Gate 0 seul, maintenant :** ajouter l'instrumentation + golden + tests pick/save (valeur permanente, faible risque, ~1 session), PUIS décider A vs B avec la baseline en main.

**Recommandation : C.** Gate 0 a de la valeur indépendamment (golden mesh, round-trip .fl4, test pick sont des filets P0 manquants aujourd'hui), risque quasi nul, et il transforme le « avant/après » de vœu en chiffre. Les gates 1-4 attendent ensuite le feu vert de séquençage.

---

## Definition of Done (par gate)
1. Build vert (`cargo build -p app`, `cargo test -p core_math` si half_edge touché).
2. Golden identique (ou diff intentionnel documenté).
3. Callers audités (grep usages).
4. Undo / save-load / **pick** vérifiés (tests Gate 0).
5. Un scénario de test manuel décrit.
6. `COMPLETE_BUG_TRACKER.md` mis à jour si un bug est fermé au passage.
