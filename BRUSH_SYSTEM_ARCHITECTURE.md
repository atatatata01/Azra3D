# BRUSH_SYSTEM_ARCHITECTURE.md

## AZRA Composable Voxel Brush — Final Architecture

**Status:** Authoritative. Supersedes the `VoxelBrush` enum once Phase 4 lands.
**Scope:** The density-voxel sculpt subsystem (`voxel_sculpt.rs`, `VoxelObject`, `VoxelAtlasGpu`, GPU/CPU brush paths). This is the *sculpt* subsystem — GPU-authority during strokes, its own undo path — NOT the SDF body system.

---

## 0. THE DECISION (read first)

**Chosen paradigm: HYBRID — "Typed Slots with a short Operator Stack."**
The winning move, on which three of four judges converged: take **Component Slot Composition's** always-valid fixed-slot frame (the top pick on arch-fit, cpu-gpu-perf, and ux-composability) and graft **Data-Driven Spec's** single concession — make the **Operator slot a short ordered stack (≤4)** instead of a single value (the top pick on impl-incrementalism). Everything else (Kernel, Direction, Channels, Modulators) stays a fixed, unordered, always-valid slot.

This is not a split-the-difference compromise. Two independent judges (ux-composability, impl-incrementalism) explicitly identified this exact hybrid as the correct answer, and it resolves the one real expressiveness gap each front-runner had:
- Pure Slots **cannot sequence two density ops** (no "Clay then auto-Smooth in one stamp" — the single most-requested sculpt composition). The Operator stack fixes that.
- Data-Spec's **N-naive-redispatch multi-op model is architecturally unsound** against the engine's pre-stamp snapshot invariant (caught by the cpu-gpu-perf judge — see §4.3). The slot frame + a *single-dispatch in-kernel op-list* fixes that.

**The load-bearing principle (all four designs converged on it, all four judges affirmed it):**
> **Composability lives in flat uniform DATA that a CPU fold compiles per-stamp. The GPU runs ONE bounded, pre-compiled uber-kernel. Uniqueness NEVER lives in code — it lives in a POD struct uploaded as a uniform.**

The codebase already proves this pattern: `voxel_brush.wgsl` is *already* a `switch op()` uber-kernel reading `op` from `aabb_max.w` and `falloff_code` from `box_min.w`. We are generalizing what already ships, not inventing a new GPU execution model.

**What we explicitly REJECT (and why):**
- **Per-brush WGSL codegen** — 25–43s recompile per custom brush, pipeline-cache invalidation, violates "Shader Changes Are Expensive." Dead on arrival.
- **On-GPU bytecode interpreter / register machine (Node Graph)** — a per-voxel dynamic loop over a `program[]` buffer. The node-graph judge scored this 3/10 on incrementalism and 5/10 on perf: its core viability is *admitted-deferred to an unrun 512³ benchmark*, it needs 2 new storage bindings (the 6-site BGL hazard), and it exposes a DAG/register-budget to a sculptor who just wants red clay. Bounded-known beats unbounded-unproven.
- **N naive re-dispatches reading each other's writes (Data-Spec multi-op)** — collides with the pre-stamp per-frame snapshot invariant (`voxel_brush.wgsl` reads a *frozen* snapshot so parallel threads never see in-flight writes). `Fill → Relax` as two dispatches would smooth the *pre-fill* surface. We keep Data-Spec's *uniform-packing* and *phasing* genius but NOT its multi-dispatch mechanism.

---

## 1. CHOSEN ARCHITECTURE — Data Model + Behavior Primitives

A brush is a fixed-shape `struct` of typed slots. Field order = evaluation order (the contract CPU and GPU share). Only the Operator slot is an ordered stack; every other slot is a single always-valid value.

```rust
// core_math/src/brush/mod.rs   (NEW module — pure data, no GPU, no engine refs)

/// A composed brush. Fixed typed slots interpreted by the fixed apply engine.
/// Every field is Copy/serializable. The user can NEVER build an invalid brush:
/// there is no free-form ordering except the bounded operator stack.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BrushRecipe {
    pub uid:        BrushUid,       // stable u64 — presets, undo labels, scene refs
    pub name:       String,
    pub builtin:    bool,           // shipped preset vs user-authored
    pub version:    u16,            // recipe schema version (serde migration)

    // SLOT 1 — KERNEL (radial weight curve w(t), t=dist/radius ∈ [0,1))
    pub kernel:     Kernel,

    // SLOT 2 — DIRECTION (plane normal / advection axis; resolved to vec3 CPU-side)
    pub direction:  Direction,

    // SLOT 3 — CHANNELS (which fields this brush writes — the "N channels")
    pub channels:   ChannelSet,

    // SLOT 4 — OPERATOR STACK (the ONE ordered axis; 1..=MAX_OPS, default 1)
    pub ops:        OpStack,        // SmallVec<[OpEntry; 4]>

    // SLOT 5 — MODULATORS (fold into radius/strength/stamp-set BEFORE dispatch)
    pub modulators: Modulators,

    // Base scalars (today's two sliders)
    pub base_radius:   f32,
    pub base_strength: f32,
}

pub const MAX_OPS: usize = 4;   // hard cap — bounds the in-kernel op-list (see §4)
```

### Behavior Primitive Set

**SLOT 1 — `Kernel`** (the falloff axis — already exists as `BrushFalloff`/`falloff_code`):
```rust
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[repr(u32)]
pub enum Kernel {
    Smooth   = 0,                 // smoothstep01(1-t)  — today's default
    Linear   = 1,                 // 1-t
    Constant = 2,                 // 1.0 (hard edge)
    Sphere   = 3,                 // sqrt(1-t²) rounded cap
    Power    = 4,                 // (1-t)^exp  → exp=2==ClayBuildup, exp=3==Crease
}
// `Kernel::Power` carries its exponent in KernelParams.exp (kept flat for POD).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize, Default)]
pub struct KernelParams { pub exp: f32, pub hardness: f32 }
```

**SLOT 2 — `Direction`** (resolved to a concrete `vec3 plane_n` in `input_sculpt`, which owns camera/stroke state — the GPU stays camera-agnostic, adds ZERO branches):
```rust
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[repr(u32)]
pub enum Direction {
    SurfaceNormal = 0,   // today's behavior
    ViewDirection = 1,   // camera forward
    FirstPoint    = 2,   // normal latched at stroke start (steady plane)
    AxisX = 3, AxisY = 4, AxisZ = 5,
}
```

**SLOT 3 — `ChannelSet`** (multi-field: sculpt AND paint in one stamp):
```rust
bitflags! {
    #[derive(Serialize, Deserialize)]
    pub struct ChannelMask: u32 {
        const DENSITY   = 1;
        const COLOR     = 2;
        const ROUGHNESS = 4;
        const DISPLACE  = 8;
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize, Default)]
pub struct ChannelSet {
    pub mask:          ChannelMask,
    pub color:         u32,   // RGBA8 when COLOR set
    pub roughness:     f32,
    pub density_scale: f32,   // per-channel weight (1.0 default; 0.0 == pure paint)
    pub color_scale:   f32,
}
```

**SLOT 4 — `OpStack` / `OpEntry`** (the ONE ordered axis — the grafted Data-Spec idea):
```rust
pub type OpStack = SmallVec<[OpEntry; MAX_OPS]>;

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct OpEntry {
    pub op:           Operator,
    pub amount:       f32,        // per-op strength multiplier (default 1.0)
    pub plane_offset: f32,        // ×radius; Clay = +0.10 (was hardcoded), default 0.0
    pub side:         PlaneSide,  // Both | Above | Below (folds Scrape & Fill into one op)
    pub blend:        OpBlend,    // how this op's delta combines with prior op output
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[repr(u32)]
pub enum Operator {                       // op_code — the frozen CPU↔GPU contract
    Add        = 0,   // Draw
    Subtract   = 1,   // Erase / Crease (Crease = Subtract + Kernel::Power{3})
    Relax      = 2,   // Smooth   (reads snapshot)
    PlanePush  = 3,   // Flatten  (push toward plane, bidirectional)
    PlaneFill  = 4,   // Clay / Fill / ClayBuildup (deposit on `side`, optional shave)
    PlaneClip  = 5,   // Scrape   (subtract on `side`)
    Inflate    = 6,   // gradient puff (reads snapshot)
    Advect     = 7,   // Pinch (tangential) / Move (drag-vector) — see AdvectSrc
}
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)] #[repr(u32)]
pub enum PlaneSide { Both = 0, Above = 1, Below = 2 }
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)] #[repr(u32)]
pub enum OpBlend  { Accumulate = 0, Max = 1, Min = 2, Replace = 3 }
// Advect carries its source in OpEntry via a packed flag bit (Tangential|DragVector).
```
> **Why Operator is a closed enum (the deliberate asymmetry, affirmed by every judge):** Operators are the ONE slot that reads neighbours / snapshot / gradient and MUST exist bit-identical on CPU and GPU. An open-ended operator = arbitrary shader codegen = TDR. A closed family of 8 is a jump table Naga handles fine — it's exactly what ships today at 11 arms. Kernel/Direction/Channel/Modulator are freely composable; Operator is *chosen from a fixed family but sequenced* (≤4). The creative space is the cross-product (5 kernels × 6 directions × channel combos × 8 ops × ordering), which is thousands of brushes on one shader.

**SLOT 5 — `Modulators`** (pre-dispatch folds; never touch a voxel — live entirely in `input_sculpt`):
```rust
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Default)]
pub struct Modulators {
    pub pressure:   PressureRoute,   // None | Strength | Radius | Both
    pub symmetry:   SymmetryFlags,   // X|Y|Z mirror planes (existing)
    pub spacing:    f32,             // stroke resample spacing (×radius)
    pub lazy:       f32,             // lazy-mouse rope smoothing 0..1
    pub allow_invert: bool,          // Ctrl flips sign
}
```

### The compiled per-stamp POD — `StampPlan` (the bridge to the GPU)

The slot fold produces ONE flat POD per stamp at the single-producer site. This is the ONLY thing the GPU sees. It is a *richer version of today's `(op, center, normal, radius, strength, sgn, aabb)` tuple* — nothing downstream knows slots existed.

```rust
// core_math/src/brush/plan.rs   — #[repr(C)], bytemuck Pod/Zeroable, GPU uniform
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub struct StampPlan {
    pub center:      [f32; 4],   // .w = radius
    pub plane_n:     [f32; 4],   // .w = strength   (Direction resolved CPU-side → vec3)
    pub color:       [f32; 4],   // RGBA when COLOR set
    pub kernel_code: u32,
    pub kernel_exp:  f32,
    pub chan_mask:   u32,
    pub sgn:         f32,
    pub op_count:    u32,                 // 1..=MAX_OPS
    pub op_list:     [PackedOp; MAX_OPS], // the in-kernel op sequence (single dispatch)
    pub _pad:        [u32; 2],
}
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub struct PackedOp {           // 4×u32 = 16 bytes
    pub op_code:      u32,       // Operator
    pub flags:        u32,       // PlaneSide | OpBlend | AdvectSrc | shave bit
    pub amount:       f32,
    pub plane_offset: f32,
}
```

---

## 2. MAPPING — Today's 11 Brushes + In-Flight Features as Compositions

Every shipped brush becomes a **built-in preset** = a `BrushRecipe` literal with a single-entry op stack. The math is byte-identical to today's `match brush` arm — re-expressed, not rewritten.

| Brush | Kernel | Direction | Channels | `ops[0]` |
|---|---|---|---|---|
| **Draw** | Smooth | SurfaceNormal | DENSITY | `Add` |
| **Erase** | Smooth | SurfaceNormal | DENSITY | `Subtract` |
| **Smooth** | Smooth | — | DENSITY | `Relax` |
| **Flatten** | Smooth | SurfaceNormal | DENSITY | `PlanePush{side:Both}` |
| **Clay** | Smooth | SurfaceNormal | DENSITY | `PlaneFill{offset:0.10, side:Below, shave:true}` |
| **Scrape** | Smooth | SurfaceNormal | DENSITY | `PlaneClip{side:Above}` |
| **Crease** | **Power{exp:3}** | SurfaceNormal | DENSITY | `Subtract` |
| **Fill** | Smooth | SurfaceNormal | DENSITY | `PlaneFill{offset:0, side:Below, shave:false}` |
| **ClayBuildup** | **Power{exp:2}** | SurfaceNormal | DENSITY | `PlaneFill{offset:0.10, side:Below, shave:false}` |
| **Inflate** | Smooth | SurfaceNormal | DENSITY | `Inflate` |
| **Pinch** | Smooth | SurfaceNormal | DISPLACE | `Advect{Tangential}` |
| **Move** *(CPU-only)* | Smooth | — | DISPLACE | `Advect{DragVector}` |

**In-flight features map to slots — they were always orthogonal:**
- **Paint-Color** → add `COLOR` to `channels.mask` on *any* recipe. Pure paint = `density_scale:0` + COLOR. Generalizes today's dense-only `deposit_color_sphere` into a channel any op carries.
- **Pressure** → `modulators.pressure = Both|Radius|Strength`. Folds into the single-producer radius/strength block exactly where it lives today. Now per-brush data, not a global toggle.
- **Falloff** → it *is* the Kernel slot. The falloff slider becomes `Kernel::Power{exp}` on a slider. Crease's cube and ClayBuildup's square stop being per-op hacks — they're `exp=3` / `exp=2` data.
- **Move** → `Advect{DragVector}`, inherits the existing `u32::MAX`/CPU-only contract (no new "GPU can't do this" case invented).
- **Direction** → it *is* the Direction slot. Surface stays the default → existing behavior unchanged.

**Net result:** 11 brushes collapse to **8 operators + 5 kernels + a channel tail** — *fewer* total shader arms than today, because Crease/ClayBuildup fold into Power-kernel + base op. The TDR surface shrinks while expressiveness grows.

---

## 3. UI — How a User Builds a Unique Brush

Extend `draw_voxel_sculpt_panel` (`ui_panels.rs:411`). Two modes via a top toggle: **Quick** (today's flat picker — unchanged for muscle memory; just swaps in a built-in preset) and **Edit** (the composer). The active brush is always a `BrushRecipe`.

**Edit mode — slot cards top→bottom (UI order = pipeline order, so the panel teaches the engine):**

```
┌─ BRUSH: "Red Clay*"        [Save] [Save As ▾] [Duplicate] [Reset] ─┐
│  ◆ Falloff    [ Power ▾ ]  exp ──●──  hard ──●──  (live curve)     │
│  ◆ Direction  [ Surface ▾ ]                                        │
│  ◆ Channels   [✓]Density  [✓]Color ■red  [ ]Rough  [ ]Displace    │
│  ◆ Operators  ┌───────────────────────────────────────────────┐  │
│               │ ≡ PlaneFill   amt●─ side[Below▾] off●─ blend▾  ✕│  │
│               │ [+ Add operator ▾]            (≤4)              │  │
│               └───────────────────────────────────────────────┘  │
│  ◆ Modulators  Pressure[Radius▾]  Sym[X][Y][Z]  Lazy●─ Spacing●─  │
└───────────────────────────────────────────────────────────────────┘
[ Preset Library ▾ ]   Draw  Clay  Scrape …  + your saved brushes
```

**Interaction rules (the key UX win — most representable brushes are valid by construction):**
- **Swap a slot** = change a dropdown (Falloff: Smooth→Power). Instant.
- **Add/remove a channel** = checkbox. A bitset never reorders → no invalid state.
- **Operator stack** is the ONLY reorder surface: drag the `≡` handle, `[+ Add operator]` appends (≤4), `✕` removes (min 1 enforced). Reorder is meaningful (`PlaneFill → Relax` = build-then-smooth in one stamp).
- **No reorder UI anywhere else** — slots are fixed position. The user *cannot* sequence a modulator between two ops, cannot build a "does nothing" brush from bad ordering. This is a feature: near-zero error states.
- **Three honest guardrails** (the only invalid combos, surfaced visibly not hidden):
  1. **Advection ops own the stamp** — `Advect{Tangential|DragVector}` can't be stacked with anything; UI disables `+ Add` and greys siblings with a tooltip ("Move/Pinch can't be layered").
  2. **Empty op stack forbidden** — min 1 enforced (no "brush does nothing").
  3. **Color on sparse/atlas** — if the color-on-atlas scatter isn't wired for the active resolution, the COLOR checkbox shows a one-time warning ("color unavailable >512³ this build") and masks the bit. Visible defer, never silent drop.
- **Live preview:** every edit rebuilds the `BrushRecipe`, recomputes `cost_hint`, redraws a 2D radial-profile strip + optional scratch-sphere swatch. Cheap — same `Kernel::weight` math, no shader recompile.
- **Presets/Library:** built-ins are non-deletable seeds; `[Save As]` writes to the user library (§5); right-click user brushes → rename/delete/duplicate. Editing a recipe is **tool state, not scene state** → not an undo transaction (consistent with sculpt's own undo path). Loading a brush just sets the active tool.

---

## 4. CPU + GPU EXECUTION — The Decisive Part

**Strategy name: "Slot-fold → single flat `StampPlan` POD → ONE uber-kernel with a bounded in-kernel op-list, single dispatch per stamp."**

This is the synthesis of the two highest-scoring GPU stories: Slots' *fewer-arms bounded switch* + Layer-Stack's *flat POD + capped `op_list` array*, while explicitly avoiding Data-Spec's unsound N-redispatch and Node-Graph's unproven per-voxel VM.

### 4.1 The CPU fold (runs ONCE per stamp at the single-producer site, ~input_sculpt.rs:1748)

```rust
// core_math/src/brush/eval.rs — the ONLY new code on the hot path
pub fn fold_recipe(r: &BrushRecipe, hit: &SurfaceHit, mods: &StrokeMods) -> StampPlan {
    let mut p = StampPlan::seed(hit);                 // center, radius, strength=1
    // SLOT 5 modulators fold radius/strength HERE (pressure → radius, exactly as today)
    r.modulators.apply(&mut p, mods);
    // SLOT 2 direction resolved to a concrete vec3 — GPU stays camera-agnostic
    p.plane_n = r.direction.resolve(hit, mods);
    // SLOT 1 kernel
    p.kernel_code = r.kernel as u32;
    p.kernel_exp  = r.kernel_params.exp;
    // SLOT 3 channels
    p.chan_mask = r.channels.mask.bits();
    p.color     = unpack_rgba(r.channels.color);
    // SLOT 4 operator stack → flat op_list (single dispatch consumes it)
    p.op_count = r.ops.len() as u32;
    for (i, e) in r.ops.iter().enumerate() {
        p.op_list[i] = PackedOp::from(e);
    }
    p
}
```
Fold cost: arithmetic on ~40 floats, once per stamp. Negligible, well inside the 12ms budget.

### 4.2 The GPU kernel — single dispatch, bounded loop (NO combinatorial blowup)

`voxel_brush.wgsl` keeps its current structure. The single `switch op()` becomes a **bounded loop over `op_count ≤ 4`**, each iteration a `switch op_code` over the 8-arm jump table that *already ships*. The op-list rides in the existing stamp UBO (extended with `MAX_OPS` packed ops — `Direction` adds zero bindings because it's pre-resolved to `plane_n`).

```wgsl
// per voxel:
let t = dist / radius;
let w = kernel_weight(plan.kernel_code, plan.kernel_exp, t) * strength;  // 5-arm switch
var d = density_in;                                  // current density (this voxel)
for (var i = 0u; i < plan.op_count; i = i + 1u) {    // BOUNDED: i < 4
    let pop = plan.op_list[i];
    let delta = apply_operator(pop.op_code, d, w, plan.plane_n, snapshot_sample(g),
                               pop.amount, pop.flags, pop.plane_offset);  // 8-arm switch
    d = blend_op(pop.flags_blend, d, delta);         // Accumulate/Max/Min/Replace
}
density_out = d;
if ((plan.chan_mask & COLOR) != 0u)  { color_out = blend(color_out, plan.color, w * color_scale); }
if ((plan.chan_mask & ROUGH) != 0u)  { rough_out = blend(rough_out, plan.roughness, w); }
```

**Why this does NOT blow up the shader (named, structural reasons):**
1. **One pipeline, compiled once at warmup.** A 1-op Draw and a 4-op designer brush produce the *same* SPIR-V — only the uniform `StampPlan` differs. Zero recompiles per brush. Respects "Shader Changes Are Expensive" + `WarmupManifest`.
2. **The loop is `i < op_count ≤ 4` — bounded and dynamic over DATA.** Naga does NOT unroll it into the exponential inline chain that caused the noisy-blend TDR. `apply_operator` is one function with an 8-arm `switch` (a jump table), not a recursive call chain. This is categorically different from the 17-inlined-`scene_sdf` pathology.
3. **The op-list is fixed-size (`array<PackedOp, 4>`) in the UBO** — no storage-buffer binding added (vs Node-Graph's +2 bindings / 6-site BGL churn). Params pack into the extended stamp UBO that already carries `op` + `normal`.
4. **`apply_operator` reads the pre-stamp snapshot ONCE** (the engine's existing per-frame frozen snapshot). Within a stamp, sequential ops on `d` operate on the *live per-voxel value `d`* for point-ops (Add/Sub/PlaneFill) — correct, because `d` is this thread's own register, not a cross-thread read. **Neighbourhood ops (Relax/Inflate/Advect) read the frozen snapshot** — which is consistent and deterministic. This is the critical fix over Data-Spec: we do `Fill → Relax` in ONE dispatch where Relax reads the snapshot for its neighbourhood term but blends against the just-filled `d` for its center, so it actually smooths toward the filled surface. (The honest limitation: a neighbourhood op's *neighbour* values are pre-stamp, not post-fill — documented in §8. This is strictly better than two dispatches, which see nothing of the fill at all.)

### 4.3 The CPU path mirrors it EXACTLY (op-parity is structural)

`apply_brush` runs the identical loop: `for op in plan.op_list[0..op_count] { for voxel in aabb { match op_code {...} } }` — wait, no: to match the GPU's per-voxel semantics, the CPU loop is **per-voxel-outer, per-op-inner**, identical to the WGSL above. One opcode table, two languages, same `StampPlan`. A build-time **golden parity test** (grafted from Node-Graph's best idea) asserts every `Operator`/`Kernel` arm is handled in both, and runs each built-in preset voxel-for-voxel CPU-vs-GPU-readback on a fixed grid before any phase merges.

### 4.4 Ties to GPU-authority + per-frame budget

- **GPU-authority preserved:** atlas-resident path stays truth. The op-list rides the existing UBO; the brush still writes the atlas in place and `mark_dirty(DirtyFlags::RENDER)` so the raymarch re-dispatches that frame.
- **CPU-only escape (Move/Pinch + the authority cliff — a first-class requirement, NOT a footnote, per the arch-fit judge):** A recipe whose op stack contains a CPU-only op (`Advect{DragVector}`) sets `plan.needs_cpu = true`. The **drag-start authority latch (~input_sculpt.rs:1697) selects the CPU/box path for the whole stroke** — latched once, never flipped mid-stroke. This is the *one* place GPU-authority demotes to CPU; it is surfaced in the editor ("CPU brush") and is the existing Move contract, not new debt. The synthesizer-flagged risk (all four designs buried this) is resolved by making it explicit and visible.
- **Per-frame budget:** `brush_cost` (input_sculpt.rs:1833) becomes a **derived sum over the op stack**, replacing the `match brush` table:
  ```rust
  fn cost_hint(r: &BrushRecipe) -> f32 {
      let mut c = 0.0;
      for e in &r.ops {
          c += match e.op { Add|Subtract => 1.0, PlaneFill|PlaneClip|PlanePush => 1.0,
                            Relax|Inflate => 3.0, Advect => 4.0 };
      }
      if r.channels.mask.contains(ChannelMask::COLOR) { c += 0.5; }
      c
  }
  ```
  A 4-op stack costs ~4× → the existing 4M-voxel subsample drops more stamps → strokes get *chunkier*, not slower-than-budget. **This cost is bounded-known** (it feeds the existing subsample), unlike the interpreter/redispatch risks the judges flagged as unbounded-unproven. Snapshot allocation is gated on `ops contains {Relax|Inflate|Advect}` — same set as today's `matches!`, now derived.
- **`cost_hint` MUST be calibrated against measured cost at 512³** (the "works on my machine constants" rule). This calibration is a *precondition* for Phase 3 merge, not a follow-up — but it calibrates a *bounded known kernel*, not a viability gamble.

---

## 5. SERIALIZATION + PRESETS

**`BrushRecipe` is serde JSON — brushes are TOOL config, never scene data.** They must NOT enter the bincode save-format-v6 path (bincode enum variants are positional; a brush library is exactly the wrong thing there).

- **User library:** `%LOCALAPPDATA%/Azra/brushes/<uid>.brush.json` (one file per brush) + `index.json` (name→uid, tags, equipped slot). Uses `dirs`/`%LOCALAPPDATA%` per No-Hardcoded-Paths.
- **Built-in presets are code, not files** — the 12 mappings in §2 are `BrushRecipe::draw()`, `::clay()`, … constructed in Rust. A fresh install has them with no disk library; an app update can improve a built-in with no migration.
- **Forward-compat:** every field `#[serde(default)]`. JSON uses **string discriminants** (`"op":"PlaneFill"`), so adding a new `Operator`/`Kernel` variant never reorders anything (sidesteps the bincode-positional trap entirely). Unknown variant on load → that op flagged `Unsupported`, brush loads greyed rather than corrupting. `version: u16` drives `BrushRecipe::migrate()`.
- **Op-code discriminants are append-only** (`#[repr(u32)]`) — they ARE the GPU contract; shifting them rots the WGSL switch AND saved recipes. Same discipline as the bincode append-only rule, applied to `op_code`.

**Scene save-format impact — minimal, additive, no `NodePayload` churn:**
- A `VoxelObject` stores `last_brush: Option<BrushUid>` (a *reference*, not the recipe) so a project remembers its active brush. Optional `embedded_brushes: Vec<BrushRecipe>` lets a shared `.azra` carry custom brushes.
- Both are `#[serde(default)]` fields **inside the existing voxel-body blob** — they add NO new bincode enum variant and do NOT reorder `NodePayload` (`VoxelBody` stays last). v6 files load with `last_brush=None`, `embedded_brushes=[]`.
- Missing referenced brush on load → fall back to nearest built-in by `op_code` + a toast ("brush 'Red Clay' not found, using Clay"). Never crash.

**Starter preset library:** the 12 built-ins from §2, seeded in code, non-deletable, shown first in the Library dropdown.

---

## 6. PHASED ROADMAP

Invariant for EVERY phase: the shipped Draw…Pinch + the in-flight paint-color, pressure, falloff, Move keep working at every commit. `cargo build -p app` + golden parity test after each. Each phase independently shippable and reversible.

### ★ FIRST MILESTONE — Phase 0: `StampPlan` + `BrushRecipe` behind the enum (pure refactor, zero behavior change)

Add the `brush` module: `BrushRecipe`, `StampPlan`, `Kernel`/`Operator`/`ChannelSet`/`Modulators`. Implement `BrushRecipe::from_builtin(VoxelBrush) -> BrushRecipe` (the 12 mappings) and `fold_recipe(...) -> StampPlan`. Route the single-producer site through `from_builtin → fold_recipe → StampPlan`; CPU `apply_brush` and the GPU kernel start consuming `StampPlan` instead of the loose tuple — **reproducing today's exact math** (single op, `op_count==1`). Paint/pressure/Move keep their current plumbing. **Golden parity test: `StampPlan` path == legacy path voxel-for-voxel on a fixed grid, CPU and GPU readback.** Ship. No UI change. *This proves the data model represents every current brush losslessly and is the safe foundation for everything after.*

### Phase 1 — Promote hardcoded constants into the plan (still enum-selected, still single-op)
Move Clay's `0.10`, Crease's cube, ClayBuildup's square out of the match arms into `OpEntry.plane_offset` / `Kernel::Power{exp}`. Falloff becomes a real knob (expose the in-flight slider → `kernel_exp`). Pressure reads from `modulators`. Direction resolves to `plane_n`. Externally identical; internally data-driven. The op switch shrinks (Crease/ClayBuildup fold in) — re-run parity test to guard the two rewritten arms.

### Phase 2 — Edit-mode UI, single-op composer + library
Ship the slot-card panel (§3) restricted to `ops.len()==1` + all modulators/channels. Users author falloff/direction/pressure/paint/symmetry combos and **save presets**. `brush_library.rs` (load/save JSON). Color becomes the COLOR channel (dense backend = today's `deposit_color_sphere`, gated). **This delivers most user value with ZERO dispatch-loop change** — the kernel still runs one op.

### Phase 3 — The operator stack (the in-kernel op-list) — FEATURE-FLAGGED
Lift `ops.len()==1`. Extend the GPU kernel to the bounded `op_count` loop (§4.2) and the CPU mirror. Update `brush_cost` to the per-op sum. Gate behind `brush_stack_enabled` (default OFF) until the 512³ `cost_hint` calibration + parity test are green, so a stacking regression CANNOT break shipped single-op brushes. UI enables the operator-stack list + the three guardrails (§3).

### Phase 4 — Retire the enum
Replace the stored `VoxelBrush` with `BrushRecipe` as the active brush. Demote `VoxelBrush` to an internal `op_code`/preset-author helper, then **delete `from_builtin` adapter** (avoid the two-paths-rot anti-pattern — the adapter must die, not linger). Add `last_brush`/`embedded_brushes` scene fields with serde defaults. Update `COMPLETE_BUG_TRACKER.md`.

---

## 7. WORKED EXAMPLE — "Clay + paint red + surface normal + pressure radius + sharp falloff"

**User actions (Edit mode):** Duplicate **Clay** → "Red Clay". Falloff → **Power**, exp→3 (sharp). Direction → **Surface** (default). Channels → check **Color**, swatch → red. Modulators → Pressure → **Radius**. Leave the single `PlaneFill` op. `[Save As]` → `red_clay.brush.json`.

**Resulting recipe:**
```rust
BrushRecipe {
    uid: BrushUid(0x7c1a..), name: "Red Clay".into(), builtin: false, version: 1,
    kernel:    Kernel::Power, kernel_params: KernelParams{ exp: 3.0, hardness: 0.6 },
    direction: Direction::SurfaceNormal,
    channels:  ChannelSet {
        mask: ChannelMask::DENSITY | ChannelMask::COLOR,
        color: 0xFF0D0DFF, density_scale: 1.0, color_scale: 1.0, roughness: 0.0,
    },
    ops: smallvec![ OpEntry {
        op: Operator::PlaneFill, amount: 1.0, plane_offset: 0.10,
        side: PlaneSide::Below, blend: OpBlend::Accumulate,  // shave bit in flags
    }],
    modulators: Modulators { pressure: PressureRoute::Radius, symmetry: NONE,
                             spacing: 0.25, lazy: 0.0, allow_invert: true },
    base_radius: 0.08, base_strength: 0.5,
}
```

**Execution of one stamp (GPU atlas path, dense enough for color):**
1. **Single producer** — hit → `fold_recipe`: pressure scales `radius *= pen.force` (Radius route; strength untouched); `plane_n = hit.normal`; `kernel_code=Power, kernel_exp=3`; `chan_mask = DENSITY|COLOR`, `color=red`; `op_count=1`, `op_list[0]=(PlaneFill, side:Below|shave, amount:1, offset:0.10)`. → ONE `StampPlan`.
2. **Budget** — `cost_hint = PlaneFill(1.0) + color(0.5) = 1.5`; cheap → most stamps survive subsample.
3. **Dispatch** — single GPU dispatch over the AABB. Per voxel: `t=dist/r`; `w = (1-t)^3 · strength` (sharp); the bounded loop runs `op_count=1` → `PlaneFill` fills density below the +0.10r-proud plane and shaves above; because COLOR is set, the atlas color slot blends toward red by `w`. Atlas is truth → `mark_dirty(RENDER)` → raymarch re-dispatches this frame.
4. **Undo** — atlas brick snapshot captures density AND color first-touch (per-channel mask), restored atomically. Sculpt's own undo path; no `transaction()`.

A brush nobody hardcoded, built from slots, running on the default GPU path with **zero recompilation** — the uniqueness is 40 floats of uniform.

---

## 8. TOP RISKS + MITIGATIONS

1. **In-kernel op-list changes the hot kernel's inner loop (the highest-risk GPU edit in the design).**
   *Mitigation:* Bounded `op_count ≤ 4`, fixed-size UBO array (no new binding), loop is dynamic-over-data (Naga does not unroll). Feature-flagged (`brush_stack_enabled`, Phase 3) so single-op brushes are untouched until proven. Golden parity test CPU-vs-GPU before merge. **Precondition: 512³ benchmark of the op-loop kernel before deleting the legacy switch** — this is a gate, not a follow-up.

2. **Neighbourhood-op semantics within a multi-op stamp (the snapshot subtlety the Data-Spec design got *wrong*).**
   *Risk:* In `Fill → Relax`, Relax's neighbour samples come from the *pre-stamp frozen snapshot*, not the just-filled surface. *Mitigation:* This is documented and *strictly better than the rejected two-dispatch model* (which sees the fill not at all). Relax blends against the live per-voxel `d` (which IS filled) for its center term. For users needing true post-fill neighbourhoods, that's a *second stroke* — the same limitation ZBrush has. Surfaced in the operator tooltip. We do NOT pretend it re-snapshots mid-stamp (which would cost a readback per op).

3. **GPU-authority demotes to CPU for Advect (Move/Pinch) brushes — the authority inversion all four designs buried.**
   *Mitigation:* Made first-class: `plan.needs_cpu` set in the fold, the drag-start authority latch (~1697) selects CPU/box for the whole stroke (latched once, never mid-stroke flip), and the editor labels it "CPU brush." This is the *existing* Move contract, surfaced — not new debt. UI forbids stacking Advect with other ops.

4. **`cost_hint` mis-calibration → budget blowout or over-chunky strokes ("works on my machine").**
   *Mitigation:* `cost_hint` calibrated against *measured* per-op cost at 512³ as a Phase-3 merge precondition. The multiplicative cost is bounded-known (feeds the existing 4M subsample), not unbounded. Cap stack at `MAX_OPS=4`.

5. **Color-on-atlas is new GPU scatter work (the one real channel gap).**
   *Mitigation:* Until the atlas color scatter ships, the COLOR bit is *masked with a one-time visible warning* (">512³ this build"), never silently dropped. Dense path uses today's `deposit_color_sphere` unchanged. Tracked as a separate follow-up task, not blocking the brush system.

6. **Extending the stamp UBO touches the BGL (the 6-site binding chore from memory).**
   *Mitigation:* Direction resolves CPU-side → zero new *bindings* (only UBO struct fields). The op-list is a fixed array in the existing UBO, not a storage buffer. This is a struct-layout change to ONE uniform, not 6 new binding sites — the cheapest possible GPU surface change, far below Node-Graph's +2 storage bindings.

7. **Adapter (`from_builtin`) lingering = two code paths rotting independently.**
   *Mitigation:* Roadmap explicitly *deletes* the adapter in Phase 4. The golden parity test guards both paths only during the migration window; once the enum is retired the test guards the opcode table directly.

---

**Bottom line:** The winning paradigm is **typed slots (always-valid, non-overwhelming) with one bounded ordered operator stack (the single composition that matters — sequenced ops)**, compiled per-stamp to a flat `StampPlan` POD, run by ONE bounded uber-kernel in a single dispatch. It scored top or tied-top on all four judge lenses, requires the least new GPU surface of any composable design, makes the buried GPU-authority-demotion question a first-class explicit requirement, and reaches most user value at Phase 2 with zero dispatch-loop change. The composability is in uniform data, never in code — which is the only shape that survives AZRA's Naga-inlining, frame-budgeted, GPU-authority engine.
---

## 9. Extensions folded in from BRUSH_SYSTEM_REQUIREMENTS.md (asks that arrived after the design)

These owner asks slot cleanly into the model — they are DATA on existing slots, not new code paths.

### 9.1 Modulator / jitter matrix (random size / rotation / strength, each its own amount + pressure)
`Modulators` gains a per-parameter dynamics struct. Every driven scalar = `base × pressureMap(force) × (1 ± jitter·rand)`:
```rust
pub struct ParamDyn  { pub pressure: PressureCurve, pub jitter: f32 } // jitter 0..1 = its own slider
pub struct Modulators {
    pub size: ParamDyn, pub strength: ParamDyn, pub rotation: ParamDyn, pub spacing: ParamDyn,
    // existing: symmetry, lazy, allow_invert
    pub seed: u32,   // per-stroke seed — jitter RNG is SEEDED + deterministic (undo/redo replay)
}
```
- Jitter resolves in the CPU `fold_recipe` per stamp via a seeded RNG keyed by `(stroke_seed, stamp_index)` → reproducible under undo/redo. NEVER ad-hoc `rand()` (it would break undo). The GPU only sees the already-jittered StampPlan scalars → zero GPU cost, no new branches.

### 9.2 Rotation parameter + StampPlan
Add `rotation: f32` (radians around `plane_n`) to `StampPlan`, folded from base + pressure + jitter (9.1). Orients the alpha (9.3) and any directional kernel. Zero cost for round brushes (matters only when an alpha is bound).

### 9.3 PNG brush alpha / detail (the spatial kernel)
The Kernel slot gains an optional bound alpha: `weight = falloff(t) × alpha(rotated_uv)`, where `rotated_uv` maps the footprint into the brush's oriented frame (`plane_n` + `rotation`). ONE bound alpha texture (a uniform/bindless slot — NOT a shader-per-alpha, so no explosion); the CPU path bilinear-samples a CPU copy for op-parity. Alphas live in a user folder + a built-in set; a recipe references its alpha by id (serialization §5). Greyscale = intensity; tile vs single-stamp; invert/contrast as KernelParams. Single-stamp (advanced by Spacing) is the priority.

### 9.4 Pinch / Smooth as ADDABLE operators ("Pinch should be an option")
Already native: any recipe stacks `Relax` (Smooth) or `Advect{Tangential}` (Pinch) in its op stack (≤4) with its own `amount` (= per-behavior strength) and inherits the modulator matrix (pressure/jitter). "Add Pinch with strength + pressure to my Clay" = append one OpEntry — the canonical Phase-3 worked example. (Advect stays stack-exclusive + CPU-only per §3/§8.3.)

### Roadmap placement
- 9.1 / 9.2 (jitter + rotation) → **Phase 2** (Modulators UI) — pure CPU-fold, no kernel change.
- 9.3 (PNG alpha) → **Phase 2.5** — one bound sampler+texture (generation-counted), one CPU copy.
- 9.4 (addable Pinch/Smooth) → **Phase 3** (the operator stack) — the acceptance-test composition.

---

## 10. Input completeness — SculptGL input audit cross-walk (the missing inputs + feel contract)

**Source:** a full audit of Stéphane Ginier's *SculptGL* brush set (`SculptBase`, `Brush`, `Inflate`, `Pinch`, `Crease`, `Flatten`, `Smooth`, `Paint`, `Twist`, `LocalScale`, `Move`, `Drag`, `Picking`). Every SculptGL brush reduces to ONE skeleton:

```
weight = falloff(t) × alpha(uv) × mask[v] × intensity × pressure ;  apply along DIRECTION via OPERATION
```

That skeleton **is** this document's slot model (Kernel × Channels × Modulators, Direction, Operator) — confirming the design. The audit found the model already covers ~90% of SculptGL's inputs (Kernel=§1, Direction=§2, plane ops=Flatten/Clay/Scrape, Relax=Smooth, Advect=Move/Pinch, alpha=§9.3, pressure/jitter=§9.1, rotation=§9.2). The items below are the inputs SculptGL **uses that this design did not yet expose**, plus the stroke-feel contract. Like §9, the first three are DATA on the existing Modulators slot — not new code paths. The fourth (Twist/Scale) is the ONE genuine new-operator decision and is flagged as such.

### 10.1 Universal per-voxel MASK field (Modulators input, NOT a brush) — **highest composability payoff**
In SculptGL the per-vertex mask (`mAr[ind+2]`) is the single most-reused input: **every** brush multiplies its weight by it, so "mask a region, then any brush respects it" is free. AZRA removed the voxel *Mask brush* (commit `f19769d`, deferred to surface mode) — but masking belongs back as a **per-voxel scalar field that the `StampPlan` weight multiplies by**, never as a brush:
```
weight ×= mask_field.sample(voxel)        // 0 = protected, 1 = full effect
```
- It is a **Modulator**, resolved into the per-voxel weight exactly where `mAr[ind+2]` sits in SculptGL — one multiply, applies to every Operator for free.
- Storage: a sparse per-voxel channel alongside density in the atlas (or a coarse mask grid). Painting the mask is itself a recipe whose `channels.mask = MASK` and op `Replace` — i.e. the mask brush is just another composed brush, not a special case.
- This resolves the "voxel masking was dropped" gap structurally: masking becomes an input every brush reads, not a one-off tool.

### 10.2 Curvature / cavity modulator — **the input that goes BEYOND SculptGL (the differentiator)**
Neither SculptGL nor this design exposes surface curvature as a weight input. Adding it to `Modulators` unlocks brushes neither tool has natively:
```
weight ×= cavity_response(curvature, mode)   // mode: Cavity | Peaks | Off ;  curvature from the pre-stamp gradient
```
- **Cavity brush** (deposit/paint only in concavities → instant wear, grime, panel-line dirt), **Peaks brush** (edges/ridges only), curvature-gated sharpen.
- Cheap: curvature comes from the SDF gradient AZRA already evaluates for `Inflate`/normals; it reads the **pre-stamp frozen snapshot** (same determinism rule as Relax/Inflate — §4.2). CPU↔GPU parity via the same `apply_operator` snapshot-sample path; no new binding.
- This is the highest-leverage *new* input — it's the one thing that makes AZRA brushes SculptGL can't build.

### 10.3 Front-face cull (Modulators bool)
SculptGL's `_culling` / `getFrontVertices` deforms only camera-facing geometry (skips back-face bleed on thin features). One Modulator bool resolved CPU-side against `dot(voxel_normal, eye) ≤ 0`; zero new GPU surface (folds into the per-voxel weight as a 0/1 gate, eye dir already available to the fold).
```rust
pub struct Modulators { /* …9.1 fields… */ pub front_face_only: bool, pub mask_enabled: bool, pub curvature: CurvatureMode }
```

### 10.4 Twist + Scale operators — **the ONE genuine closed-family addition (a real decision, not a freebie)**
SculptGL ships **Twist** (rotate the picked set around the view axis, driven by mouse arc) and **LocalScale** (radial inflate/deflate from center, driven by horizontal drag). The current 8-operator family has neither. Both are vector-field cousins of `Advect`:
```rust
// extends the closed Operator enum — APPEND ONLY (op_code discriminants are the GPU contract, §5)
Twist  = 8,   // rotate (v − center) around plane_n by  angle·weight   (CPU-only, drag-driven, stack-exclusive like Advect)
Scale  = 9,   // displace along (v − center) by  delta·weight          (CPU-only, drag-driven, stack-exclusive)
```
- **Trade-off (must be acknowledged, per §1):** the Operator slot is deliberately a *closed* family because it's the one slot that must exist bit-identical on CPU+GPU — every added arm widens the Naga jump table / TDR surface. Twist/Scale are worth it (they're staple sculpt gestures) but they are NOT "free data" like 10.1–10.3. They inherit the `Advect` contract: `needs_cpu = true`, stack-exclusive, latched to the CPU/box path at drag-start (§4.4, §8.3). Ship them behind the same Phase-3 op-stack flag, golden-parity-tested.

### 10.5 Stencil / lock-position interaction
SculptGL's `_lockPosition` anchors the footprint and lets the user drag to position/rotate a bound alpha (stencil placement) instead of stamping. With §9.2 rotation + §9.3 alpha already in the model, the only missing piece is the **interaction**: a lock toggle that, while held, maps cursor drag → `StampPlan.rotation` + alpha UV offset rather than advancing the stroke. Pure `input_sculpt` stroke-state; no kernel change. Roadmap → Phase 2.5 (with alpha).

### 10.6 Stroke FEEL contract (acceptance criteria — feel ≠ inputs)
The audit isolated *why* SculptGL strokes feel good. These are **non-negotiable acceptance criteria** for the brush system, independent of composability:
1. **Path-spacing resample (the #1 feel lever).** SculptGL interpolates evenly-spaced stamps along the *cursor path* at `spacing·radius` (`SculptBase.sculptStroke`), so fast flicks don't gap and slow drags don't pile. AZRA MUST resample the stroke path — never stamp per-frame at the mouse position (that makes deposit track mouse *speed* → blobby strokes). `Modulators.spacing` must drive a real path-interpolation loop at the single-producer site, not just scale a per-frame stamp.
2. **Quartic falloff, flat tangents at both ends** (`3t⁴−4t³+1`, == `Kernel::Smooth`) as the default — zero rim-pop. Keep it the default kernel.
3. **Deform/deposit from the frozen pre-stamp snapshot per stroke** (no intra-stroke compounding) — already the engine invariant (§4.2); the feel contract makes it explicit: strokes must be re-derived, not accumulated frame-over-frame.
4. **Pressure routed to size and/or strength** (§9.1) with the smooth-quartic default — present; verify the route actually reaches the fold.

### Roadmap placement (additions)
- **10.1 mask field** → **Phase 2** (a Modulator + a `MASK` channel; mask-painting is itself a recipe). Highest payoff-per-risk after the composer.
- **10.2 curvature/cavity** → **Phase 2** modulator fold (CPU) → **Phase 3** GPU parity via the snapshot-sample path. The differentiator — prioritize.
- **10.3 front-face cull** → **Phase 2** (one Modulator bool).
- **10.4 Twist/Scale operators** → **Phase 3** (op-stack flag), Advect/CPU-only contract, parity-tested. The only item needing a kernel arm.
- **10.5 stencil lock** → **Phase 2.5** (with alpha).
- **10.6 feel contract** → **Phase 0/1 precondition** — path-spacing resample and snapshot-deposit are verified/enforced BEFORE any composer UI ships (a janky-feeling composable brush is worse than a good fixed one).
