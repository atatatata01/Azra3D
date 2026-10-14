# Manifold boolean kernel evaluation — decision

| Field | Value |
|---|---|
| **Date** | 2026-08-02 |
| **Worktree** | `.claude/worktrees/boolean-manifold-eval` (`feat/boolean-manifold-eval`, off master `f66ef1e`) |
| **Disjoint from** | `kernel-locality` / locality spike |
| **Spike crate** | `core_engine/manifold_bool_spike` (no production wiring) |
| **Candidate** | [`manifold-csg`](https://crates.io/crates/manifold-csg) 0.3.3 → C++ [elalish/manifold](https://github.com/elalish/manifold) v3.5.1 via `manifold-csg-sys` |
| **Existing kernel** | `core_math::mesh_boolean` — **not deleted** |

---

## Step 0 (shipped) — `new_uv_sphere` watertightness

| Item | Status |
|---|---|
| Gate | `boundary_half_edge_count(new_uv_sphere(..)) == 0` |
| Tests | `new_uv_sphere_is_watertight_at_shipped_defaults` + `new_uv_sphere_is_watertight_across_the_ui_range` |
| Tracker | **SPHERE-NOTCLOSED closed** in `COMPLETE_BUG_TRACKER.md` |
| Probe | `op_bench --probe` box−sphere16/32 → `closed_b=true`, `OK` |

**Note:** On current master tip the sphere was already watertight (the open-seam audit finding no longer reproduces). Step 0 **locks the contract** so regression cannot silently re-break every sphere boolean.

---

## Step 1 — Spike results (RTX 4090 host, release, single-threaded Manifold)

Command: `cargo build -p manifold_bool_spike --release` then `target/release/manifold_bool_spike.exe`  
Log: `core_engine/manifold_bool_spike/SPIKE_RUN.log`

Build note: default `parallel` (TBB) **fails on Windows** under deep `OUT_DIR` paths (`Filename too long` while FetchContent-cloning oneTBB). Spike uses `default-features = false` (no TBB). Production may re-enable `parallel` with a short OUT_DIR / prebuilt lib.

### Matrix

| Case | faces_b | mesh_boolean | Manifold | Notes |
|---|---|---|---|---|
| control box−box diff | 6 | **0.7 ms** OK faces=9 vol=7.657 | **0.3 ms** OK tri=24 vol=7.657 | Exact volume match |
| box−sphere16×8 diff | 128 | **7.9 ms** OK vol=3.6719 | **0.5 ms** OK vol=3.6719 | Exact volume match |
| box−sphere32×16 diff | 512 | **80.6 ms** OK vol=3.6510 | **1.4 ms** OK vol=3.6508 | ~50× faster; vol Δ ~0.0002 |
| sphere16−box diff | 6 | **6.8 ms** OK vol=3.5189 | **0.6 ms** OK vol=3.5417 | ~11×; small vol Δ (tessellation / FP) |
| box−sphere16 union | 128 | **7.9 ms** OK vol=4.5201 | **0.5 ms** OK vol=4.5201 | Exact volume match |
| **box−CC L4 (audit 1 536f)** | **1536** | **None** (631 ms) | **4.0 ms** OK tri=1614 vol=3.541 closed | **Ours fails; Manifold green** |
| box−CC L5 | 6144 | **None** (1.8 s) | **14.9 ms** OK vol=3.543 | Same pattern |
| **box−CC L6 (audit 24 576f)** | **24576** | **277.9 s** faces=**6** vol=**4.096** | **57.4 ms** OK tri=24846 vol=3.543 | **Ours “succeeds” with WRONG geometry** (uncut box: vol = 1.6³ = 4.096, carve dissolved). Matches audit “281 s + 6-face wrong output.” Manifold correct carve. |

All Manifold results: `boundary=0`, `is_manifold()=ok`, `native_vol > 0` → **PASS** on Blender-style solid invariants.

### Correctness on every audit failing case

| Audit case | mesh_boolean | Manifold |
|---|---|---|
| box−sphere16/32 (was open-mesh None when SPHERE-NOTCLOSED open) | OK after Step 0 | OK, faster |
| box − 1 536-face CC solid | **None** | **OK ~4 ms** |
| box − 24 576-face CC solid | **Wrong** (6 faces, full volume) in ~278 s | **OK ~57 ms** |

### Speed

- Easy cases: Manifold **~10–50×** faster than mesh_boolean.
- Dense curved: Manifold **~4 800×** faster on L6 (57 ms vs 278 s) and **succeeds** where ours returns garbage.
- Numbers are single-threaded Manifold; enabling TBB later is pure upside on multi-core.

### Mesh round-trip quality

| Property | After Manifold path | Notes |
|---|---|---|
| Closed / manifold | Yes | |
| N-gon faces | **Lost** — always triangles | Existing Blender-oracle flat cases prefer n-gons; need post-boolean dissolve polish (we already have dissolve helpers for mesh_boolean) or accept tris for curved v1 |
| `material_id` | **Lost** | Same as current `from_polygons` install (**BOOL-MATLOSS** already open) |
| `poly_custom_data` / element IDs | **Lost** | Same class as **BOOL-STALECD** |
| Vertex positions | f64 in kernel → f32 on re-ingest | Acceptable for viewport poly; keep f64 path if CAD-scale precision needed later |

Manifold’s `original_id` / run metadata exists for tracking operands through booleans — useful for a future material remap, **not used in this spike**.

### Dependency weight

| Layer | What | Cost |
|---|---|---|
| `manifold-csg` | Safe Rust API | Small |
| `manifold-csg-sys` | cmake clone of manifold v3.5.1 + Clipper2 | **Heavy first build**; needs CMake + C++ toolchain |
| optional TBB | `parallel` feature | Avoided on Windows path-length issues; re-enable with short OUT_DIR or prebuilt `MANIFOLD_CSG_LIB_DIR` |
| Runtime | static-linked into spike binary (no extra DLL dance in this config) | |

**CI implication:** either cache the C++ build, or supply prebuilt libs via `MANIFOLD_CSG_LIB_DIR` (crate supports offline path).

### License

| Component | License |
|---|---|
| `manifold-csg` / `manifold-csg-sys` | **Apache-2.0 OR MIT** |
| elalish/manifold (upstream) | **Apache-2.0** |
| Clipper2 (transitive) | Boost / permissive (upstream Manifold dep) |

**Compatible** with shipping AZRA (no GPL/AGPL infection).

---

## Decision: **ADOPT (wrap)** — **WIRED 2026-08-02 (Round 2)**

| Option | Verdict | Why |
|---|---|---|
| **Reject** | No | Fixes every production-density failure the audit called unfixable; 10–5000× speed. |
| **Adopt raw** | No | Need a thin adapter preserving `HalfEdgeMesh` API + material remap; not a drop-in of Manifold types into the app. |
| **Wrap** | **Yes — shipped** | `mesh_boolean(a,b,op)` signature unchanged; default body = Manifold; BSP A/B via `AZRA_BSP_BOOLEAN=1`. |

### Round 2 production wrap (this tree)

| Item | Location |
|---|---|
| Default kernel | `mesh_boolean_manifold::manifold_boolean` |
| A/B env | `AZRA_BSP_BOOLEAN=1` → legacy `robust_boolean` + `bsp_boolean` |
| Module | `core_math/src/mesh_boolean_manifold.rs` |
| Materials | Nearest coplanar/nearest input-face transfer after polish (see module docs table) |
| Element IDs | Lost (rebuild) — same class as BSP `from_polygons` |
| N-gons | `merge_coplanar` + `post_boolean_polish` on modest outputs |
| Tests | All `mesh_boolean::*` + Manifold audit/material tests green |

### REPLACE-RULE (BSP deletion)

BSP (`robust_boolean` / `bsp_boolean`) stays in tree for A/B. **Delete only after:**

1. Full `cargo test -p core_math` green (Manifold default)  
2. Blender oracle / poly boolean suite green  
3. **One owner soak session** sign-off  

Do **not** delete in Round 2 — flag only (comments in `mesh_boolean.rs` header + this doc).

### Explicit non-goals of this spike

- No app wiring, no UI change, no deletion of `mesh_boolean` BSP code.
- No TBB/parallel enablement (Windows path-length risk documented).
- No Blender Exact face-layout match on dense curved cutters (Blender itself leaves tris; invariants only).

---

## Reproduce

```bat
cd core_engine
:: needs cmake + MSVC on PATH (vcvars64)
cargo build -p manifold_bool_spike --release
target\release\manifold_bool_spike.exe
```

Step 0 gate:

```bat
cargo test -p core_math --lib new_uv_sphere_is_watertight
```
