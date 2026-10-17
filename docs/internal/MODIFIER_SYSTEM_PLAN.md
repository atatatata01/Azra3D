# Modifier System Plan — AZRA

**Status:** PLAN (revised after codebase audit + UI function)  
**Date:** 2026-07-19 (rev: multi-eval + codebase evaluation + **UI function first / look later**)  
**Owner:** Dim  
**Related:** `MODIFIER_STACK_PIPELINE.md`, `VOLUME_ADR.md`, `VIEWPORT_MODEL.md`, `GPU_INVALIDATION_MATRIX.md`,  
`core_math/src/field_eval.rs`, `app/src/mesh_presentation.rs`, `app/src/bake_utils.rs`, `core_math/src/mesh_ops/`

---

## 0. Codebase evaluation (2026-07-19)

Audit of this plan against the live tree. Verdicts: **KEEP** / **FIX** / **BLOCKER**.

### 0.1 What the plan got right (KEEP)

| Claim | Code evidence |
|-------|----------------|
| Prim stack CPU full, GPU one-mod | `primitives.rs::world_distance` stack loop; `sync_modifiers_to_gpu_fields` picks one; `volume_bake.wgsl` single `prim_mod_type` |
| Body only hardwired mirror + bevel | `NodePayload::SdfBody { mirror_flags, mirror_sign, mirror_center, bevel_radius, … }` — no `Vec` stack |
| Poly stack shape + partial impl | `PolyModifier` / `apply_poly_modifiers` in `mesh_sync.rs`; Decimate/Bevel stubs |
| Bake identity already hashes prim modifiers | `bake_utils::hash_prim_slice` hashes `modifier` + full `modifiers` vec + sculpt_layers |
| Display = DC triangles | `cache_state::DcMeshRoute` + `mesh_presentation::SdfMeshPresentation`; analytical march deleted |
| Domain split field vs mesh is sound | Bake path = field; Object Mode draw = mesh slot / mesh_cache |
| Pure compile + multi-eval fits Rust culture here | `BodyField` is already `Send` “field program”; `mesh_ops` already pure typed ops; brush POD pattern |

### 0.2 Critical corrections (FIX — plan was wrong or incomplete)

| Issue | Reality in code | Plan change |
|-------|-----------------|-------------|
| **Body mirror ≠ domain fold** | `field_eval::eval_scene_body_mirrored` = **TRUE UNION over 2^n reflections** (replaces legacy one-sided fold). GPU bake must match. | Volume Mirror semantics = **union of reflections**, not `abs` fold. Bounds via `expand_aabb_for_mirror`. |
| **`BodyField` already is the field multi-eval spine** | `BodyField` / `FieldScope` / `FieldChild` + `bake_body_grid_field` + scope-aware GPU serialize | Do **not** invent a parallel `CompiledBodyField` from scratch. **Extend `FieldScope`** (or wrap it). R0 = extract/parallelize around `BodyField`, not replace. |
| **Name collisions** | `mesh_ops::MeshOp` = poly HalfEdge operators; `field_eval::FieldEvaluator` = tool query trait | Plan names: **`DisplayMeshOp`** / **`PostDcOp`** (not `MeshOp`); do not redefine `FieldEvaluator`. Module: prefer `mod_eval` only for display-mesh fold **or** extend existing modules. |
| **Production DC is GPU-resident** | Default routes: `DenseResident` / `SparseTiledResident` — mesh lives in GPU slot, not a free CPU `MeshBuf` | P1 “instant mesh fold on cached MeshBuf” is **false** for production path. Need **GPU compute post-DC** and/or `mesh_presentation::materialize` (readback — not free). |
| **CPU grid bake is serial today** | `bake_body_grid_field` nested `for` loops; `BodyField` is `Send` ready for rayon | Rayon multi-eval is valid next step, not existing. |
| **GPU bake is primary truth for display field** | `volume_bake.wgsl` + hot serialize; CPU grid is fallback/oracle | “CPU multi-eval honesty only” for multi-mod prims leaves **viewport wrong** unless GPU also runs full stack or CPU bake is forced. P3 must update **WGSL + serialize** or force CPU fill when stack depth > 1. |
| **Smooth/mirror kernels on HalfEdge ≠ DC mesh** | `HalfEdgeMesh::smooth_laplacian` / `mirrored` in frozen `half_edge.rs`; DC output is `Mesh` / GPU buffers | Post-DC ops work on **`Mesh` / raster `[f32;8]`**, not HalfEdge. Share *algorithms*, not half_edge APIs. Poly stack stays separate. |
| **`mirror_sign` exists** | Hashed + serialized on body | Any Volume Mirror authoring must include sign bits, not only flags/center. |

### 0.3 Blockers for naïve P1 (BLOCKER until addressed)

1. **Resident draw path** never goes through a CPU `fold_mesh_stack` today.  
   - **Option A (recommended for “instant”):** GPU compute passes on resident vertex/index buffers (mirror, smooth, normals).  
   - **Option B:** On mesh-stack edit, `materialize_sdf_body_mesh` → CPU fold → re-upload as `CachedCpu` or temp slot (works, **not** free; OK for MVP correctness).  
   - **Option C:** Force `CpuChunkedOracle` when body has mesh mods (dev-only feel).

2. **`SdfMeshPresentation` / Gate 4** already owns “one place mesh is drawn/picked/exported from”. Mesh stack **must** plug into presentation + materialize, not a third draw path.

3. **Serde on `SdfBody`:** `child_order` lesson (`#[serde(skip)]` + side-channel). New `Vec` fields need the same discipline or proven trailing `#[serde(default)]` — verify before in-blob.

### 0.4 What to build *on* (reuse map)

| Existing | Role in plan |
|----------|----------------|
| `core_math::field_eval::BodyField` | **Canonical body field program** — extend for extra field ops; multi-eval = `eval_or` / grid fill |
| `bake_utils::bake_body_grid_field` | CPU grid backend; add rayon later |
| `bake_utils::body_content_id` / `hash_body_source` | Field identity — fold new field stack params here |
| `bake_utils::expand_aabb_for_mirror` | Bounds capability for volume mirror |
| `volume_bake.wgsl` + serialize hot | GPU field backend — must stay parity with `BodyField` |
| `mesh_presentation.rs` | **Only** materialize/display facade for mesh stack output |
| `sdf_gpu_mesh/*` | Resident buffers target for GPU DisplayMeshOps |
| `poly/mesh_sync::apply_poly_modifiers` | Precedent for ordered mesh fold (poly domain only) |
| `core_math::mesh_ops` | Poly **edit** ops — **do not overload**; sibling pattern only |
| `core_math::mesh::Mesh` | CPU triangle/quad type for post-DC CPU path |
| `primitives::Modifier` + `modifiers.rs` | Prim authoring + pure warps — compile into prim eval, keep |

### 0.5 Overall verdict

| Area | Sense? |
|------|--------|
| Product: field vs mesh domains | **Yes** |
| Product: Mesh Mirror default vs Volume Mirror | **Yes**, but Volume = true union (code), Mesh = post-DC |
| Rust multi-eval pure ops | **Yes**, if built **as extension of BodyField + display mesh fold**, not greenfield fork |
| P0 honesty | **Yes**, low risk |
| P1 as written (CPU MeshBuf always free) | **No** — rewrite around resident GPU or materialize |
| P2 body field stack | **Yes** — mostly formalize hardwired mirror/bevel into stack **on top of FieldScope** |
| P3 prim multi-mod | **Yes but harder** — GPU bake parity required for Object Mode |
| Naming `MeshOp` / new `FieldEvaluator` | **No** — collisions |

**Ship order after audit:** P0 → **R0 extend BodyField + display-mesh op module (new name)** → **P1 mesh stack via presentation/materialize or GPU post** → P2 field authoring on FieldScope → P3 prim/WGSL honesty.

---

## 1. Goal

One coherent modifier system that:

1. Matches how AZRA displays geometry (**triangles from bake/DC**, not analytical raymarch).
2. Puts each modifier in the **right domain** (field vs mesh) so results are predictable.
3. Gives **body-level** control (today: prim stack + poly stack; body only has hardwired mirror/bevel).
4. Stays **fast**: mesh-only edits must not re-bake; field edits re-bake only when needed.
5. Stops lying in the UI (no silent no-ops; no “stack of 5” when GPU only runs 1).
6. **Is structured for Rust strengths: pure ops, exhaustive enums, one stack → many evaluation backends (scalar / batch / parallel / hash / bounds / GPU POD).**

**Non-goal (v1):** node-graph modifiers, every Blender modifier, putting every type in every domain, dyn TraitObject on the bake hot path.

---

## 1. Current state (audit)

### 1.1 Three disconnected systems

| System | Storage | Eval | GPU honesty |
|--------|---------|------|-------------|
| **SDF prim stack** | `SdfPrimitive.modifiers: Vec<Modifier>` | Full stack on CPU (`world_distance`) | **`sync_modifiers_to_gpu_fields` → ONE non-mirror mod** |
| **Body hardwired** | `SdfBody { mirror_*, bevel_radius }` | Fold + body bevel in bake | Bake params, not a stack |
| **Poly stack** | `PolygonalBody.modifiers: Vec<PolyModifier>` | `apply_poly_modifiers` | N/A (CPU mesh → raster) |

### 1.2 SDF prim types (`ModifierType`)

Live on CPU: Mirror, Twist, Taper, Shear, Smooth, Displace, Sculpt (legacy), Spherize, Shell, Emboss, Deboss, Array.  
Sculpt layers live separately (`sculpt_layers`) — keep that split.

### 1.3 Poly types (`PolyModifierType`)

| Type | Status |
|------|--------|
| Subdivide, Mirror, Solidify, Array, Smooth, Triangulate | Implemented |
| Decimate, Bevel | Declared, **stub** (UI gated) |

### 1.4 Critical gaps

1. **No body field stack** — only hardwired mirror + bevel.
2. **No body mesh stack post-DC** — Smooth geo / Smooth normals / Decimate / Clay Polish missing.
3. **GPU prim stack dishonest** — multi-mod prims wrong on bake path.
4. **No domain policy** — Mirror/Displace could be field *or* mesh without rules.
5. **Dirty coupling** — mesh polish wrongly tied to re-bake.
6. **Eval is monolithic** — `world_distance` embeds stack logic; bake/hash/bounds/UI each re-implement or skip pieces. Hard to multi-eval cleanly.

### 1.5 Display truth (constraint)

SDF Object Mode = **baked field → Dual Contouring → triangle mesh**.  
Mesh post-DC modifiers sit on the **real display path**. Field modifiers sit on the **bake path**.

---

## 2. Product decisions (locked)

Change only via explicit owner override.

### 2.1 Two domains, not “everywhere”

```
[prim field stack]  → boolean body eval
        ↓
[body field stack]  → bake volume (re-bake on change)
        ↓
   Dual Contouring / surface extract
        ↓
[body mesh stack]   → post-DC triangle ops (no re-bake)
        ↓
   Raster / export
```

Poly keeps its own stack on `PolygonalBody`. Do **not** merge poly into SDF body in v1.

> **Correction 2026-08-12 (owner-directed).** The matrix below assigned Bend to the FIELD side with
> "No" mesh version in v1. **Bend shipped as a POLY MESH modifier** (`PolyModifierType::Bend` →
> `HalfEdgeMesh::bend_deform`), because that is the stack the owner asked for and the one every
> other recent modifier landed in. No SDF field Bend exists; `ModifierType` still has only
> Twist / Taper on the field side. The row is corrected in place — do not "restore" it.

### 2.2 Domain policy matrix

| Modifier | Domain | Default placement | Dual placement? |
|----------|--------|-------------------|-----------------|
| **Mirror** | Mesh default; Field optional | **Body MESH** | Yes: **Body FIELD** = volume symmetry |
| **Array** | Field | Prim or Body FIELD | No mesh array v1 |
| **Twist / Taper / Shear** | Field | Prim FIELD / Body FIELD later | No |
| **Bend** | ~~Field~~ **Mesh (shipped 2026-08-12)** | **Poly stack** (`PolyModifierType::Bend`) | Field version not built |
| **Displace / noise** | Field | Prim FIELD (+ mask) | **No** mesh displace v1 |
| **Smooth distance** | Field | Prim/Body FIELD | Name ≠ mesh smooth |
| **Shell / Emboss / Deboss / Spherize** | Field | Prim FIELD | No |
| **Bevel body** | Field | Body FIELD (migrate `bevel_radius`) | No |
| **Boolean** | Field | Existing body_op / prim ops | Not a stack card v1 |
| **Smooth geometry** | Mesh | Body MESH | No |
| **Smooth normals** | Mesh | Body MESH | No |
| **Decimate** | Mesh | Body MESH | No |
| **Clay Polish–like** | Mesh preset | Body MESH recipe | Not a field clone |
| **Subdivide / Solidify / Triangulate** | Mesh / Poly | Poly stack | Body mesh later if needed |

**Naming:** `Mirror` (mesh) vs `Mirror Volume` (field); `Smooth Geometry` vs `Smooth Surface`.

### 2.3 Mirror (aligned with live code)

| Mode | When | Invalidation | Semantics in code |
|------|------|--------------|-------------------|
| **Mesh Mirror (default)** | Display symmetry | Mesh stack only — **no field bake** | Reflect DC triangles / GPU verts; weld optional |
| **Volume Mirror (explicit)** | True volume symmetry | Field dirty → re-bake + DC | **`eval_scene_body_mirrored` TRUE UNION over 2^n combos** — NOT legacy one-sided `abs` fold. See `field_eval.rs`. |

Badge mesh-only: **“mesh only — not in volume”**.  
Body authoring must keep **`mirror_sign`** with flags/center.  
Half-AABB bake + mesh mirror = phase 5+ perf, not MVP.  
**“Instant” Mesh Mirror** requires GPU post-pass on resident slot **or** accept materialize cost (see §0.3).

### 2.4 Complexity policy

- Domain-filtered +Add menus.
- No dual placement unless sémantique differs and labels differ.

---

## 3. Rust multi-eval architecture (core design)

This section is the **implementation spine**. Product phases (P0–P6) build *on top of* it.  
Goal: write each modifier **once** as pure data + pure functions; plug the **same stack** into many evaluation contexts without copy-paste.

### 3.1 Why Rust is the right shape here

| Rust strength | How the modifier system uses it |
|---------------|----------------------------------|
| **Algebraic enums + exhaustive `match`** | Closed op sets (`FieldOpKind`, `MeshOpKind`) — compiler forces every backend to handle every op |
| **Zero-cost pure functions** | `fn warp(p) -> p`, `fn alter(d, p) -> d` — no vtable on hot path |
| **`Send + Sync` data** | Stacks are plain POD-ish records → free `rayon` over voxels / verts |
| **Newtypes / phantom domains** | `FieldStack` vs `MeshStack` cannot be mixed at compile time |
| **Traits as *capabilities*, not god-objects** | Small traits: `HashParams`, `ExpandAabb`, `SampleField`, `ApplyMesh` |
| **Compile once, eval many** | `CompiledFieldEval` built from authoring stack; reused for bake, tests, bounds, hash |
| **No GC / predictable alloc** | Mesh fold can reuse buffers; grid fill writes into preallocated `&mut [f32]` |
| **Feature-split crates** | Pure math in `core_math`; UI/serde wiring in `app` — tests run without GPU |

**Anti-pattern to avoid:** one giant `match` inside `world_distance` *and* another in bake *and* another in hash. That is today’s debt.

### 3.2 Layer split (authoring vs runtime) — **reuse BodyField**

```
┌─────────────────────────────────────────────────────────┐
│  AUTHORING (serde, UI, undo)                            │
│  FieldModRecord / DisplayMeshModRecord                  │
│  (+ migrate body mirror_*/bevel_radius into records)    │
└────────────────────────┬────────────────────────────────┘
                         │ compile
                         ▼
┌─────────────────────────────────────────────────────────┐
│  FIELD RUNTIME (EXISTING + extend)                      │
│  BodyField { root: FieldScope { mirror, bevel, kids } } │
│  Prim eval: SdfPrimitive::world_distance / Modifier     │
│  Send → bake workers / tools                            │
└────────────────────────┬────────────────────────────────┘
         ┌───────────────┼───────────────┬────────────────┐
         ▼               ▼               ▼                ▼
   BodyField::eval_or  bake_body_grid  volume_bake.wgsl  body_content_id
   (scalar / tools)    _field (+rayon) (GPU primary)     (hash)
                         │
                         ▼ DC (resident / chunked / readback)
┌─────────────────────────────────────────────────────────┐
│  DISPLAY MESH RUNTIME (NEW — name carefully)            │
│  enum DisplayMeshOp { Mirror, SmoothGeo, SmoothNormals }│
│  fold on CPU Mesh / GPU buffers — NOT mesh_ops::MeshOp  │
└────────────────────────┬────────────────────────────────┘
                         ▼
              mesh_presentation (draw / pick / export)
```

**Rules:**

1. Authoring types never evaluated on bake workers.
2. **Field multi-eval = `BodyField` (+ prim stack inside prims)**. New body field ops extend `FieldScope` / scope close, not a second field interpreter.
3. **Display mesh ops** are a separate enum (`DisplayMeshOp`) — zero collision with `core_math::mesh_ops` (poly HalfEdge).
4. Do **not** add a second trait named `FieldEvaluator` (already tools API in `field_eval.rs`).
5. Hash field changes only through `hash_body_source` / `hash_prim_slice` (already folds modifiers).

### 3.4 Field ops — staged pure API

Field modifiers are not one function. They have **stages** (matches today’s prim eval order):

```rust
#[derive(Clone, Debug)]
pub enum FieldOp {
    // Domain fold / warp (change query point BEFORE shape/boolean sample)
    VolumeMirror { axis_bits: u8, sign_bits: u8, center: Vec3 },
    Twist { strength: f32, axis: u8 },
    Taper { strength: f32, axis: u8, end_scale: f32 },
    Shear { strength: f32, axis: u8, drive: f32 },
    // Surface alters (change distance AFTER base field sample)
    SmoothSurface { amount: f32 },
    Displace { amplitude: f32, frequency: f32 },
    BodyBevel { radius: f32 },   // often post-boolean body
    Shell { thickness: f32 },
    // …
}

/// Stage classification — used by compile_stack + exhaustive backend checks.
impl FieldOp {
    pub fn stage(&self) -> FieldStage { /* Warp | Surface | PostBoolean */ }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum FieldStage { Warp, Surface, PostBoolean }
```

**Pure stage functions** (free fns or methods — no `self` mutation):

```rust
/// Warp query point (identity if op is surface-only).
pub fn field_warp(op: &FieldOp, p: Vec3) -> Vec3 { match op { … } }

/// Alter distance at original/local p (identity if op is warp-only).
pub fn field_alter(op: &FieldOp, p: Vec3, d: f32) -> f32 { match op { … } }
```

**Scalar multi-eval** (one point — tests, debug, picking helpers):

```rust
pub fn eval_field_stack(ops: &[FieldOp], p: Vec3, base: impl Fn(Vec3) -> f32) -> f32 {
    let mut q = p;
    for op in ops.iter().filter(|o| o.stage() == FieldStage::Warp) {
        q = field_warp(op, q);
    }
    let mut d = base(q);
    for op in ops.iter().filter(|o| o.stage() != FieldStage::Warp) {
        d = field_alter(op, q, d); // or p vs q per op contract — document per variant
    }
    d
}
```

**Body pipeline composition** (Rust = functions compose):

```rust
// base = prim boolean chain at point
// body_field = compiled body field stack
// one closure, many callers
let sample = |p: Vec3| eval_field_stack(&body_ops, p, |q| body_boolean_sdf(q, &prims));
```

### 3.5 Batch + parallel multi-eval (Rust win)

Same ops, **grid backend** — no second implementation of Twist/Mirror math:

```rust
pub struct GridEvalRequest<'a> {
    pub ops: &'a [FieldOp],
    pub bounds_min: Vec3,
    pub bounds_max: Vec3,
    pub res: u32,                 // samples per axis
    pub base: &'a dyn Fn(Vec3) -> f32,  // or generic F: Fn(Vec3)->f32 + Sync
}

/// Fills `out` with res^3 distances. Prefer rayon over z-slices / bricks.
pub fn eval_field_stack_grid<F>(req: &GridEvalRequest<'_, F>, out: &mut [f32])
where
    F: Fn(Vec3) -> f32 + Sync,
{
    // parallel over z or linear index
    // each thread: p = grid_to_world(i); out[i] = eval_field_stack(req.ops, p, &req.base);
}
```

**Design rules for batch:**

| Rule | Why |
|------|-----|
| Ops are `Clone + Send + Sync` | Share `&[FieldOp]` across rayon threads |
| Base evaluator is `Sync` | Body boolean can be read-only graph snapshot |
| Output is `&mut [f32]` pre-sized | No per-voxel alloc |
| Optional **brick/chunk** entry point | Reuse dirty-brick bake; same `eval_field_stack` per sample |
| **No RNG without seed in op** | Displace must be pure for hash + parallel |

**Today’s bake job** should eventually call `eval_field_stack_grid` (CPU path) or pack the **same** `FieldOp` list to GPU. One math source.

### 3.6 Capability traits (small, orthogonal)

Prefer **capability traits** over one `Modifier` trait with 20 methods:

```rust
pub trait HashParams {
    fn hash_params<H: std::hash::Hasher>(&self, h: &mut H);
}

pub trait ExpandAabb {
    /// How this op grows evaluation/bake bounds (Mirror volume expands, Smooth surface expands by amount, …).
    fn expand_aabb(&self, b: Aabb) -> Aabb;
}

pub trait FieldSample {
    fn warp(&self, p: Vec3) -> Vec3 { p }
    fn alter(&self, p: Vec3, d: f32) -> f32 { d }
}

pub trait MeshPass {
    fn apply(&self, mesh: MeshBuf) -> MeshBuf;
    /// true if topology (index count / connectivity) can change — affects caching strategy
    fn changes_topology(&self) -> bool;
}
```

Implement via `match` on the enum in one place each:

```rust
impl HashParams for FieldOp { fn hash_params<H: Hasher>(&self, h: &mut H) { match self { … } } }
impl ExpandAabb for FieldOp { … }
// MeshOp: HashParams + MeshPass
```

**Multi-eval = different free functions / backends that only depend on these capabilities:**

| Backend | Uses |
|---------|------|
| `field_source_hash(stack)` | `HashParams` only |
| `bake_bounds(base_aabb, stack)` | `ExpandAabb` fold |
| `eval_field_stack` / `_grid` | warp + alter |
| `fold_mesh_stack` | `MeshPass` |
| `to_gpu_field_pod` (later) | explicit pack match |

Adding a new op = **one enum variant** + arms in hash / aabb / warp|alter or mesh apply. Compiler lists missing arms.

### 3.7 Mesh multi-eval

Mesh domain is **pass-based**, not sample-based:

```rust
#[derive(Clone, Debug)]
pub enum MeshOp {
    Mirror { axis_bits: u8, center: Vec3, weld: bool, weld_threshold: f32 },
    SmoothGeometry { iterations: u32, factor: f32 },
    SmoothNormals,
    Decimate { /* phase 4 */ },
}

/// Triangle soup / indexed mesh used by DC output + raster.
#[derive(Clone)]
pub struct MeshBuf {
    pub positions: Vec<Vec3>,
    pub normals: Vec<Vec3>,
    pub indices: Vec<u32>,
    // uvs later
}

pub fn fold_mesh_stack(ops: &[MeshOp], mut mesh: MeshBuf) -> MeshBuf {
    for op in ops {
        mesh = mesh_apply(op, mesh);
    }
    mesh
}

pub fn mesh_apply(op: &MeshOp, mesh: MeshBuf) -> MeshBuf {
    match op {
        MeshOp::Mirror { .. } => mesh_mirror(mesh, …),
        MeshOp::SmoothGeometry { iterations, factor } => mesh_smooth_laplacian(mesh, *iterations, *factor),
        MeshOp::SmoothNormals => mesh_recompute_normals(mesh),
        …
    }
}
```

**Parallelism:**

- Smooth Laplacian: parallelize over verts (read prev positions, write next) — double-buffer `Vec<Vec3>`.
- Normals: parallel face contrib + atomic/add into vert bins, or two-pass.
- Mirror: allocate `2 * n`, parallel copy + flip.

**Reuse:** poly stack should call the **same** `mesh_smooth_laplacian` / `mesh_mirror` free functions (extract from `apply_poly_modifiers` over time). One kernel, two stacks.

### 3.8 Compiled evaluators (one object, many uses)

```rust
/// Built when authoring stack or prims change. Stored next to body runtime cache.
pub struct CompiledBodyField {
    pub ops: FieldStack,
    pub field_hash: u64,       // precomputed
    pub aabb_expand: Aabb,     // optional cached expand of unit cube / last base
}

impl CompiledBodyField {
    pub fn sample(&self, p: Vec3, base: impl Fn(Vec3) -> f32) -> f32 {
        eval_field_stack(&self.ops.ops, p, base)
    }

    pub fn fill_grid<F: Fn(Vec3) -> f32 + Sync>(
        &self, bounds: Aabb, res: u32, base: F, out: &mut [f32]
    ) {
        eval_field_stack_grid(/* … */);
    }
}

pub struct CompiledBodyMesh {
    pub ops: MeshStack,
    pub mesh_hash: u64,
}

impl CompiledBodyMesh {
    pub fn apply(&self, dc_mesh: MeshBuf) -> MeshBuf {
        fold_mesh_stack(&self.ops.ops, dc_mesh)
    }
}
```

**Cache keying (multi-eval friendly):**

```text
display_mesh_key = (body_id, field_hash, mesh_hash)
dc_mesh_key      = (body_id, field_hash)           // reusable across mesh-stack edits
volume_key       = (body_id, field_hash, res, bounds)
```

Mesh slider scrub → recompile mesh stack (µs) → `apply` on cached DC mesh → upload.  
No field re-eval.

### 3.9 Prim stack refactor direction

Today: logic inlined in `SdfPrimitive::world_distance` + broken GPU sync.

**Target:**

```rust
// authoring still on SdfPrimitive.modifiers: Vec<Modifier>
fn compile_prim_field(p: &SdfPrimitive) -> FieldStack { … }

impl SdfPrimitive {
    pub fn world_distance(&self, p: Vec3) -> f32 {
        let ops = compile_prim_field(self); // or cache on prim if hot
        eval_field_stack(&ops.ops, p, |q| self.shape_distance_only(q))
    }
}
```

Bake honesty (Phase 3): grid fill uses **identical** `compile_prim_field` + body boolean + body field stack.  
GPU either:

- samples via CPU multi-eval into grid (simplest honesty), or  
- gets a **POD pack of the same FieldOp list** (shared layout doc, not a second math dialect forever).

`sync_modifiers_to_gpu_fields` single-slot path becomes **deprecated** once grid multi-eval is source of truth.

### 3.10 Module layout (suggested)

```
core_math/src/mod_eval/
  mod.rs           // FieldDomain, MeshDomain, OpStack, compile helpers
  field_op.rs      // FieldOp enum + warp/alter + stage
  mesh_op.rs       // MeshOp enum + mesh_apply
  field_eval.rs    // eval_field_stack, eval_field_stack_grid (rayon)
  mesh_fold.rs     // fold_mesh_stack, kernels (mirror, smooth, normals)
  hash.rs          // HashParams impls + stack_hash()
  aabb.rs          // ExpandAabb fold
  gpu_pod.rs       // optional later: pack FieldOp → [f32; N] / structured buffer

app/src/
  body_mesh_modifiers.rs   // wire CompiledBodyMesh into DC → raster
  // properties UI reads authoring records only
```

**Tests live next to pure math** (`core_math` tests): no wgpu, no egui.

```rust
#[test]
fn twist_then_displace_matches_staged_eval() { … }

#[test]
fn grid_eval_matches_scalar_at_cell_centers() { … }

#[test]
fn mesh_mirror_preserves_area_scale_2() { … }

#[test]
fn mesh_stack_hash_stable() { … }
```

### 3.11 Multi-eval checklist (every new op)

When adding an op, the PR must complete this table (or CI-ish review):

| Hook | Required |
|------|----------|
| Enum variant + serde authoring record | yes |
| `FieldStage` / mesh topology flag | yes |
| `warp` and/or `alter` **or** `mesh_apply` | yes |
| `HashParams` arm | yes |
| `ExpandAabb` arm (field) | yes if bounds change |
| Unit test scalar or mesh | yes |
| Grid parity test if field | yes if used in bake |
| UI +Add domain filter | yes |
| GPU POD arm | only if GPU path still used for that op |

**Missing arm = compile error** (exhaustive match). That is the Rust lever — do not use `_ => {}` on field/mesh op matches in backends.

### 3.12 What we explicitly do *not* do

| Reject | Why |
|--------|-----|
| `dyn FieldModifier` on bake inner loop | Vtable + harder inlining; enums win for closed sets |
| One mega-trait `Modifier` for field+mesh | Forces fake methods; breaks domain firewall |
| Evaluating authoring records on worker threads | UI flags / non-Send traps |
| Random unseeded noise in Displace | Breaks hash + parallel determinism |
| Duplicating Twist math in WGSL “for speed” without parity tests | Re-creates GPU honesty debt |

GPU can come later as **another backend** over the same `FieldOp` list — not a fork of product logic.

---

## 4. Target data model (authoring)

### 4.1 Body-level stacks

On `NodePayload::SdfBody` (serde-safe — CLAUDE.md bincode rules; side-channel if needed):

```rust
/// Authoring — UI/undo. Compiled to FieldStack / MeshStack on change.
field_modifiers: Vec<FieldModRecord>,
mesh_modifiers:  Vec<MeshModRecord>,
```

**Migration:**

| Today | Tomorrow |
|-------|----------|
| `mirror_flags` / `mirror_sign` / `mirror_center` | Seed VolumeMirror field record **or** shortcut writing stack |
| `bevel_radius` | Seed BodyBevel field record |

One source of truth after migration.

### 4.2 Authoring records (v1)

```rust
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FieldModRecord {
    pub id: u32,
    pub enabled: bool,
    pub expanded: bool,
    pub kind: FieldModAuthoringKind, // serde-stable enum
    // params…
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MeshModRecord {
    pub id: u32,
    pub enabled: bool,
    pub expanded: bool,
    pub kind: MeshModAuthoringKind,
    // params…
}
```

`compile_field_records(&[FieldModRecord]) -> FieldStack`  
`compile_mesh_records(&[MeshModRecord]) -> MeshStack`

Prim stack remains `Vec<Modifier>` until optionally migrated to `FieldModRecord` sharing the same `FieldOp` compile path.

### 4.3 Pipeline (authoritative)

```
1. compile prim FieldStacks (per prim)
2. body boolean chain sample/base
3. compile body FieldStack → eval (scalar or grid)
4. bake volume (hash = field hashes)
5. DC → MeshBuf
6. compile body MeshStack → fold_mesh_stack
7. Raster / export / pick display mesh
```

Hashes:

- `field_source_hash` → bake + DC  
- `mesh_stack_hash` → mesh fold only  

---

## 5. Phased plan

Phases deliver product value **while** installing the multi-eval spine early (avoid rewriting mesh/field twice).

### Phase 0 — Honesty & gates (S)

| Task | Detail |
|------|--------|
| P0.1 | This plan = domain + multi-eval source of truth. |
| P0.2 | Prim UI: warn if multiple non-mirror mods (until P3). |
| P0.3 | Poly: keep stubs gated; no silent `_ => {}`. |
| P0.4 | Audit dirty field vs mesh write paths. |

---

### Phase 0.5 — Multi-eval scaffold on **existing** field + new display-mesh (S/M) ★

| Task | Detail |
|------|--------|
| R0.1 | **Field:** document + thin wrappers around `BodyField::eval_or`; optional rayon in `bake_body_grid_field` (keep semantics). |
| R0.2 | **Display:** new module e.g. `core_math/src/display_mesh_ops.rs` with `DisplayMeshOp` + `fold_display_mesh` on `mesh::Mesh` or `(positions, normals, indices)`. **Not** `mesh_ops::MeshOp`. |
| R0.3 | Hash helper for display stack (separate from `body_content_id`). |
| R0.4 | Parity: `BodyField` scalar vs grid cell centers (already mostly true — lock tests). |
| R0.5 | No UI yet. |

**Exit:** display-mesh fold unit-tested; field path still single `BodyField` interpreter.

---

### Phase 1 — Body display-mesh stack MVP (M) ★ first user win

| Task | Detail |
|------|--------|
| P1.1 | Authoring `mesh_modifiers` on body (serde-safe / side-channel). |
| P1.2 | **Presentation integration:** mesh stack applies inside/after `mesh_presentation` materialize **or** as GPU post on resident slot. |
| P1.3 | **P1 path choice (pick one for MVP):** **B** materialize→CPU fold→upload (correct, slower) **or** **A** GPU compute mirror/smooth on resident buffers (fast). Document choice in PR. |
| P1.4 | `DisplayMeshOp::Mirror` (default +Add). |
| P1.5 | `SmoothGeometry` + `SmoothNormals` on same buffer type. |
| P1.6 | Mesh edit → **no** `body_content_id` change / no volume bake; only display rematerialize or GPU post. |
| P1.7 | UI Body → Mesh Modifiers. |
| P1.8 | Undo on authoring records. |
| P1.9 | Mesh-only badge when no Volume Mirror. |
| P1.10 | Tests: fold kernels; integration: bake job count = 0 on mesh-only edit. |

**Exit:** Mesh Mirror works on **the same presentation path** draw/pick use; field bake stays idle.

---

### Phase 2 — Body FIELD stack as **FieldScope authoring** (migrate hardwired)

| Task | Detail |
|------|--------|
| P2.1 | Authoring records that **write** `mirror_*` / `bevel_radius` (or replace them with one source of truth). |
| P2.2 | `BodyField::from_graph` already reads mirror+bevel — keep as runtime. |
| P2.3 | Volume Mirror UI must expose true-union behavior (and `mirror_sign`). |
| P2.4 | GPU: already in bake params / scope open — stay parity with `field_eval`. |
| P2.5 | Any *new* body field op beyond mirror/bevel: add to `FieldScope` eval + `volume_bake.wgsl` + `hash_body_source` + bounds together. |
| P2.6 | UI Field Modifiers section. |

**Exit:** No dual state; Mesh Mirror ≠ Volume Mirror dirty behavior verified.

---

### Phase 3 — Prim stack → same FieldOp multi-eval

| Task | Detail |
|------|--------|
| P3.1 | `compile_prim_field` → shared `FieldOp` list. |
| P3.2 | `world_distance` thin wrapper over `eval_field_stack`. |
| P3.3 | Bake samples full prim stack (CPU multi-eval honesty **or** GPU POD of full list). |
| P3.4 | Kill single-slot `sync_modifiers_to_gpu_fields` lie. |
| P3.5 | Tests: Twist+Displace scalar == grid. |

**Recommended path:** CPU grid multi-eval as ground truth for bake when stack depth > 1; optimize GPU later with POD + parity tests.

---

### Phase 4 — Mesh polish (Decimate + Clay Polish)

| Task | Detail |
|------|--------|
| P4.1 | `MeshOp::Decimate` kernel in `mesh_fold`. |
| P4.2 | Clay Polish = **preset** compiling to `[SmoothGeometry×N, SmoothNormals]` (authoring sugar). |
| P4.3 | Share kernels with poly stack where possible. |

---

### Phase 5 — More body field ops + optional half-AABB

Body Displace, Smooth Surface, Array, Twist — each as new `FieldOp` variant + multi-eval checklist.  
Optional: half bounds + mesh mirror perf path.

---

### Phase 6 — Poly stack completion (parallel)

Wire/gate Bevel & Decimate; route Smooth/Mirror through `mod_eval` kernels.

---

## 6. Dirty / invalidation contract

| Change | Marks | Rebuild |
|--------|-------|---------|
| Prim / body **field** authoring | field dirty | compile field → bake → DC → mesh fold |
| Body **mesh** authoring only | mesh dirty | compile mesh → fold on cached DC |
| Material only | RENDER | none geometry |

Update `GPU_INVALIDATION_MATRIX.md` with `field_hash` / `mesh_hash` keys.

---

## 7. UI / UX — **function first** (look later)

**Priority now:** what the user can **do** and **where** (Body vs Prim, which stack, which ops, dirty/undo).  
**Deferred:** custom Blender skin, colors, fancy card chrome, drag-reorder polish, icons. Reuse existing egui lists/cards until then.

---

### 7.1 Functional problem: Body vs Prim

| | Body | Prim |
|--|------|------|
| Role | Boolean result, bake, DC mesh, display | One CSG piece |
| Field modifiers | Volume Mirror, Body Bevel, (later global Displace…) | Twist, local Mirror, Array, Emboss… |
| Mesh modifiers (post-DC) | Mirror, Smooth Geometry, Smooth Normals, Decimate… | **None** |
| Selection today | `draw_props_body` | `draw_props_sdf` / shape modifiers |

Mixing both into one flat stack is **functionally wrong** (eval order + ownership unclear).

---

### 7.2 Functional UX decision ★ LOCKED

**Selection-driven stacks (Option A)** — content follows what is selected:

| Selection | Functional UI shows |
|-----------|---------------------|
| **Prim** | **Local field stack only** (+ way to jump to parent body) |
| **Body** (no prim) | **Body field stack** + **Body mesh stack** (two sections) |
| **Poly** | Existing poly stack (same ops rules) |
| Multi-select | Active / first only (v1) |

**Rejected (functional reasons, not look):**
- **B** Nested all-prims under body → too much state, unclear focus for undo/dirty  
- **C** Per-card target Body|Prim → eval order / hash nightmare  
- **D** Separate Pieces|Body tabs → optional later; not required if selection is clear  

---

### 7.3 Functional requirements (must work)

Independent of look:

1. **Add / remove / reorder / enable** modifiers in the correct stack for the selection.  
2. **All parameters editable** for that modifier (axis, strength, mask, weld, etc.) — can stay simple sliders.  
3. **+ Add list filtered by scope** — never offer Mesh Mirror on a prim; never offer Twist on body mesh.  
4. **Prim → no mesh section.**  
5. **Body field change → volume dirty / re-bake.**  
6. **Body mesh change → mesh dirty only (no bake).**  
7. **Undo** on add/remove/param (transaction / episode as today).  
8. **Navigate Body ↔ Prim:** at least one clear action (select parent body / open body modifiers) when a prim is selected.  
9. **Honest warnings (text is enough):** mesh-only mirror; GPU 1-mod limit until P3.  
10. **Migrate body hardwired** mirror/bevel into body **field** list when field stack ships (one place to edit, no duplicate controls).

---

### 7.4 What appears where (canonical behavior)

#### Prim selected
- Stack: **local field** (`SdfPrimitive.modifiers` today).  
- +Add: Twist, Taper, Shear, Smooth, Displace, Mirror, Array, Shell, Spherize, Emboss, Deboss (no post-DC mesh types).  
- Action: **Select / edit parent body** (name button or “Body modifiers…”).  
- Optional text: body has N field / M mesh mods (counts only — not a second full stack).

#### Body selected
- Section **Field:** Volume Mirror, Body Bevel (+ later body-level field ops).  
- Section **Mesh:** Mirror (default), Smooth Geometry, Smooth Normals (+ later Decimate, Clay Polish).  
- Mesh Mirror without Volume Mirror → short note: mesh only, not in volume.

#### Poly selected
- One stack; only implemented types in +Add (stubs gated).

---

### 7.5 + Add catalogs (functional filter)

| Scope | Allowed types |
|-------|----------------|
| Prim field | Local deform / surface / generate (existing prim `ModifierType` set) |
| Body field | Volume Mirror, Body Bevel, (later body field catalog) |
| Body mesh | DisplayMesh ops only |
| Poly | Implemented poly types only |

Domain matrix §2.2 is the source of truth for “allowed”.

---

### 7.6 Implementation (function, minimal UI)

Wire through existing properties paths first; **no new visual system required for v1**.

| Scope | Where to bind (today → target) |
|-------|--------------------------------|
| Prim field | Keep / extend `properties_sdf_shape` stack **or** thin extract that only reuses current card loop |
| Body field | Move hardwired controls from `draw_props_body` into a stack list when P2 lands |
| Body mesh | New list under `draw_props_body` when P1 lands |
| Poly | Keep `properties_poly` until shared extract is worth it |

Shared code **only when it reduces duplication of behavior** (add/remove/reorder/enable + dirty), not for aesthetics:

```text
// optional thin helpers — not a design system
mod_ui_scope → which stack + which add list
apply_mod_list_edit → transaction + dirty field vs mesh
```

**Look pass (later, not blocking):** custom card chrome, colors, pipeline strip, drag-reorder, icons, Blender-like panel skin. Track as **LOOK-1** after U1/U2 functional.

---

### 7.7 Functional UI phases

| Phase | Functionality | Look |
|-------|---------------|------|
| **U0** | Prim stack: enable flag if missing; honest multi-mod warning text | none |
| **U1** | Body **mesh** list: add/remove/reorder/params; mesh dirty only; jump from prim → body | none |
| **U2** | Body **field** list; migrate hardwired mirror/bevel; field dirty | none |
| **U3** | Warnings for bake honesty (P3) | none |
| **U4** | Decimate / Clay Polish entries when kernels exist | none |
| **LOOK-1** | Shared visual chrome, polish | **only then** |

---

### 7.8 Decision summary (function only)

| Question | Answer |
|----------|--------|
| Look / custom Blender panel now? | **No — later (LOOK-1)** |
| Body + Prim UX? | **Selection-driven stacks** |
| Mesh mods on prim? | **No** |
| Field local on prim? | **Yes** |
| Volume + mesh on body? | **Yes, two sections** |
| One mixed stack? | **No** |
| What ships with P1? | Working body mesh list + correct dirty, not pretty cards |

---

## 8. File touch map

| Area | Location |
|------|----------|
| Field multi-eval | extend `field_eval::BodyField` / bake_utils — not a greenfield fork |
| Display mesh ops | `core_math/src/display_mesh_ops.rs` (name TBD; **not** `mesh_ops::MeshOp`) |
| Authoring body stacks | `scene_graph.rs` + project side-channel if needed |
| Presentation / mesh apply | `mesh_presentation.rs`, `sdf_gpu_mesh/*` |
| Poly fold | `app/src/poly/mesh_sync.rs` |
| **UI panel** | **`app/src/ui_modifiers/*` (new)** + `properties.rs` dispatch |
| Prim bind | migrate off `properties_sdf_shape.rs` modifier block |
| Body bind | migrate hardwired from `properties_sdf.rs::draw_props_body` |
| Poly bind | `properties_poly.rs` → shared chrome |
| Prim thin wrap | `primitives.rs` |

**Red lines:** half_edge frozen; bincode discipline; no dual display tech; no third draw path outside `mesh_presentation`.

---

## 9. Testing plan (multi-eval focused)

| Level | Cases |
|-------|-------|
| Unit scalar | Each FieldOp warp/alter identity + known values |
| Unit mesh | Mirror winding; smooth topology; normals finite |
| **Parity** | `eval_field_stack` vs `eval_field_stack_grid` at centers |
| **Hash** | Param change flips hash; enable/disable stable |
| **AABB** | Volume mirror expands; smooth surface expands by amount |
| Integration | Mesh edit → 0 bake jobs; field edit → ≥1 |
| Load | Old bevel/mirror bodies match |

---

## 10. Risks & mitigations

| Risk | Mitigation |
|------|------------|
| Dual Mirror UX | Default mesh; badge; different labels |
| Serde body fields | Side-channel / proven defaults |
| Mesh seam | Weld + threshold |
| Resident GPU path skips fold | Single display-mesh build site |
| Scope creep | Checklist + phases |
| Accidental math fork WGSL | Parity tests gate GPU POD |
| Rayon over `Fn` capture | Snapshot prim data `Arc`/owned bake request (already job-style) |

---

## 11. Success metrics

1. **Multi-eval:** new field op works in scalar + grid from one `match` without app changes beyond compile/UI.  
2. **P1:** Mesh Mirror instant; bake counter flat.  
3. **P2–3:** Body/prim field stacks bake-honest; no single-slot lie.  
4. **P4:** Decimate + Clay Polish preset.  
5. **Exhaustiveness:** no `_ => {}` on `FieldOp` / `MeshOp` in backends.

---

## 12. Execution order

```
P0 honesty
  → R0 / P0.5  mod_eval scaffold (scalar + grid + mesh fold + hash)  ★ Rust spine
  → P1         body mesh stack (Mirror, Smooth*)                     ★ ship feel
  → P2         body field stack + bake hooks multi-eval
  → P3         prim compile → same FieldOp path
  → P4         Decimate + Clay Polish preset
  → P5         more FieldOps + optional half-AABB
  // parallel: P6 poly → shared mesh kernels
```

**First vertical slices:**

1. **R0:** `FieldOp::VolumeMirror` + grid/scalar parity test (no UI).  
2. **P1:** Body + Mesh Mirror visible in app, zero rebake.

---

## 13. Open decisions

| ID | Question | Default |
|----|----------|---------|
| D1 | Mesh Mirror weld default? | ON |
| D2 | Export applies mesh stack? | YES |
| D3 | Hardwired mirror UI after P2? | Stack-only |
| D4 | Decimate algo? | Simple first |
| D5 | Half-AABB bake? | Phase 5+ |
| D6 | Serde body vecs? | Follow child_order lesson |
| D7 | Bake multi-eval CPU vs GPU first? | **CPU multi-eval honesty first** (rayon); GPU POD later |
| D8 | `dyn Fn` vs generic `F: Fn` on grid eval? | Generic + Sync for monomorphized bake jobs |

---

## 14. One-line summary

> **Extend `BodyField` for field multi-eval (true-union volume mirror already lives there); add `DisplayMeshOp` post-DC via `mesh_presentation`/GPU (not a second MeshOp); Mesh Mirror default for display speed; never fork field math between CPU grid and `volume_bake.wgsl`.**
