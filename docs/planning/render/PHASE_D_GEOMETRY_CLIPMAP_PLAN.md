# Geometry Clipmap — full implementation plan (AZRA)

**Source read in full, 2026-08-05:** Losasso & Hoppe, *Geometry Clipmaps: Terrain
Rendering Using Nested Regular Grids*, SIGGRAPH 2004 (8 pages, draft PDF from
https://hhoppe.com/geomclipmap.pdf). Every numbered claim below carries its
section reference so the next dev can check me against the paper rather than
against my memory of it.

Companion decision doc: `PHASE_D_TWO_TRACK_GEOMETRY.md` (cluster DAG for general
meshes, clipmap for heightfields — both, not either/or).

---

## 0. RULE CONFLICT CHECK (CLAUDE.md requires this before starting)

Two rules must be resolved by the owner before code lands. Neither blocks writing
this plan; both change its scope.

### 0.1 "DO NOT build a Terrain primitive" — `AZRA_MASTER_ROADMAP.md:25` + §5

The roadmap ratified *"not a new pillar / no new NodePayload variant / no new LOD
scheme"* for terrain, on the grounds that **Phase D cluster LOD IS the terrain
LOD**, and the authored workflow is *many local image-displaced SDF prims*
(memory `project_terrain_image_displacement`).

A geometry clipmap **is** a new LOD scheme. The conflict is real.

My reading of which side is wrong: the roadmap's premise ("cluster LOD is the
terrain LOD") is the part that has now been tested and did not hold — the
2026-08-05 session spent itself on terrain-specific cluster artifacts, and the
paper explains structurally why (§1 Limitations: prior LOD schemes need per-vertex
refinement criteria and adjacency tracking; the clipmap's crack-freeness is by
construction, the DAG's is by invariant). But the roadmap is owner-ratified and
I do not get to overturn it by inference.

**Scope split that keeps both true, and what I recommend:**
- The clipmap is a **renderer**, not a scene-object type. No `NodePayload`
  variant, no new pillar, no authoring UI, no save-format change.
- Its input is a **heightfield source** — v1: the demo scene's `terrain_height`
  pure fn; v2: an imported heightmap texture. It does *not* consume authored SDF
  prims and does not compete with the image-displaced-prim workflow.
- Under that framing the roadmap ban ("no Terrain *primitive*") is honoured and
  "no new LOD scheme" is the only clause bent — and bent for a render path the
  roadmap's own justification no longer covers.

**Owner call required:** accept the scope split above, or drop the clipmap track.
Do not start §3 until this is answered.

### 0.2 Display Invariant (amended 2026-08-03)

*"Never two depth conventions over one frame … Still FORBIDDEN: a partial setup
where some objects are clustered and some plain-hardware-rastered, racing over
shared pixels."*

The clipmap is **compliant** and this needs stating explicitly because the
sentence above looks like it bans it:
- The clipmap terrain is hardware raster into the **same** G-buffer, **same**
  depth attachment, **same** depth encoding, same pass as every other mesh.
- The forbidden case is *two rasterizers with different depth conventions*
  fighting for pixels (the `4f1a30a` device-loss class). Clipmap + clustered +
  plain mesh are three *geometry sources* feeding **one** rasterizer with **one**
  depth convention — the legal case the amendment was written to permit.
- Requirement carried into §5: the clipmap draw MUST be inside the existing
  G-buffer pass in `raster_gbuffer.rs`, never its own pass with its own targets.

---

## 1. What the paper actually specifies (the parts a summary loses)

Numbered so §3–§7 can cite them. Level indices: **𝑙 = 1 is coarsest, 𝑙 = m is
finest** (§3) — the opposite of the intuitive ordering, and getting it backwards
inverts every constraint below. Grid spacing 𝑔ₗ = 2⁻ˡ.

**P1 — Vertex is `(x, y, z, z_c)` (§3).** `z_c` is the height at the same (x,y)
**in the next-coarser level**, stored *per vertex*, not derived at draw time.
It is the entire mechanism behind the morph (P4).

**P2 — Four region types per level (§3, Fig. 2).**
- `clip_region(l)` — the world extent of the n×n data actually resident.
- `active_region(l)` — the n×n square *centred on the viewer* we wish to render.
- `render_region(l)` = `active_region(l)` − `active_region(l+1)` — a **hollow
  frame**, not a filled square. This is what gets drawn.
- `transition_region(l)` — the outer band of `render_region(l)`, width `w`.
- `active_region(m+1)` is defined empty, so the finest level is a filled square.

**P3 — Four hard constraints (§5).** Violating any of them is a crack.
1. `clip_region(l+1) ⊆ clip_region(l) ⊖ 1` — nested, with one grid unit of
   margin because coarse-to-fine prediction needs the neighbours.
2. `active_region(l) ⊆ clip_region(l)` — never render data you don't have.
3. **The perimeter of `active_region(l)` must lie on EVEN vertices** — this is
   what makes the boundary with level 𝑙−1 watertight, because even vertices of
   level 𝑙 are exactly the vertices of 𝑙−1.
4. `active_region(l+1) ⊆ active_region(l) ⊖ 2` — the render frame must be at
   least **2 grid units** wide so a continuous transition can exist.

**P4 — Morph, exactly (§6.2).** `z' = (1−α)·z + α·z_c`, with
`α = max(αx, αy)` and

```
αx = clamp( ( |x − vx^l| − (  (xmax − xmin)/2  − w − 1 ) ) / w , 0, 1 )
```

where `(vx^l, vy^l)` is the **continuous** (sub-grid, fractional) viewer position
in level 𝑙's grid, and `xmin/xmax` are the integer extents of
`active_region(l)`. α is 0 everywhere except the transition band, ramping
linearly to 1 at the outer perimeter. **It is a function of position, not of
time** — that is why there is no temporal popping and no per-frame animation
state. ~10 VS instructions.

**P5 — Transition width (§6.2).** `w = n/10` grid units works; smaller and level
boundaries become visible, larger and fine detail is thrown away. When the finer
active region crowds in: `w = min(n/10, min_width(l))`, and `min_width(l) ≥ 2` by
P3.4.

**P6 — Morph removes gaps but NOT T-junctions (§6.2).** T-junctions on the level
boundary still drop pixels in rasterization. The paper's fix is **zero-area
(degenerate) triangles stitched along the render-region boundaries**. Two separate
mechanisms; shipping only the morph leaves pinhole cracks.

**P7 — Render order (§6.1).** Cropping runs **coarse-to-fine**; rendering runs
**fine-to-coarse**, deliberately, to feed hardware occlusion culling / early-Z the
near geometry first. Directly composes with our D2 HZB.

**P8 — Sizing (§4).** Screen-space triangle size

```
s = 1.25 · W / ( n · tan(φ/2) )      pixels
```

Paper's default: W=640, φ=90° ⇒ n=255 ⇒ s≈3 px. Solving for n at AZRA-relevant
settings (`n` rounded to 2^k−1):

| W (px) | φ | s target | n required |
|---|---|---|---|
| 640 | 90° | 3 px | 255 (paper) |
| 1920 | 90° | 3 px | 799 → **1023** |
| 1920 | 60° | 3 px | 1385 → **2047** |
| 2560 | 90° | 4 px | 800 → **1023** |
| 3840 | 90° | 4 px | 1200 → **2047** |

**Consequence we must plan for:** n grows linearly with W and with 1/tan(φ/2).
The paper says so explicitly ("clipmap size n must grow as the field of view
narrows") and accepts it because it chose viewer-centred regions so the view can
rotate instantly. At n=1023 the vertex cost is ~m·n² ≈ 11 M verts of *footprint*
before frustum culling. §6 addresses this.

**P9 — Height-based level suppression (§4).** Looking straight down from high
above makes triangles sub-pixel and aliased. Fix: read viewer height above terrain
from the finest valid level, and **set `active_region(l)` empty whenever viewer
height > 0.4·n·gₗ**. Cheap, essential, and the single easiest thing to omit.

**P10 — Toroidal addressing (§3, §5).** Arrays are indexed mod n in x and y, so a
viewer step never copies old data — only the newly exposed **L-shaped** strip is
refilled. Applies to vertex data *and* to the per-level textures.

**P11 — Update budget and graceful degradation (§5).** Update levels
**coarse-to-fine**, stop once total updated samples exceed n². Levels that fall
behind have their clip region lag, which crops their active region (P3.2) until it
is empty. Effect: fast motion loses high-frequency detail and **rendering load
goes down as the viewer speeds up**. This is a designed feature, not a fallback.

**P12 — Upsampling filter (§5).** Tensor-product 4-point interpolatory
subdivision, mask **(−1/16, 9/16, 9/16, −1/16)** [Dyn et al. 1987 / Kobbelt 1996].
Its **C¹ smoothness is load-bearing** — §8 says a non-C¹ predictor produces
surface crease artifacts under synthesis.

**P13 — Normal maps (§3, §6.3).** Per level, **twice the geometry resolution**
(one normal per vertex is too blurry), 8 bit/channel, computed on the fly from the
geometry at update time, toroidally addressed. **Hardware mipmapping is DISABLED**
— texture LOD uses the *same* α transition as the geometry. Reason (§6.3): with
hardware mipmaps the mip level lags the clipmap level and you get visible
"advancing fronts" sweeping over the terrain during motion. Anisotropic filtering
replaces the orientation-dependence that mipmaps would have given.

**P14 — Frustum culling (§6.4).** Keep per-level `zmin/zmax`. Extrude each of the
4 sub-rectangles by `[zmin, zmax]` into an AABB, intersect with the 4-sided
frustum pyramid, project the convex result to the XY plane, and **crop the
rectangle to the bounding rect of that projection**. ~3× reduction at 90° fov.
Note this is *cropping the drawn rectangle*, not rejecting it whole.

**P15 — Compression (§7).** Residual pyramid `R_l = T_l − U(T̂_{l−1})` (residual
against the *reconstructed* coarse level, so error does not accumulate), PTC
coder, 60–100×. **All levels quantized at the SAME absolute accuracy** — the
obvious optimisation of scaling residuals by 2^(l−m) is *geometrically* correct
but *visually wrong*, because the normal map and z-colouring are derived from the
decompressed geometry and show the quantization. Downsample filter D is an
optimised 11×11 chosen so `U(D(T_l))` is the best L² fit to `T_l`.

**P16 — Synthesis (§8).** Uncorrelated Gaussian noise added to the upsampled
coarse level; **per-level noise variance = variance of the real residuals `R_l`**;
values drawn from a precomputed **50×50** table indexed by `mod` on vertex
coordinates (large enough to hide banding). Must be spatially deterministic.

**P17 — Precision (§9).** ≤11 levels is fine in f32. Beyond that, transform the
viewpoint and view matrix into **each level's local frame** (in f64) before
uploading.

**P18 — Measured baseline (§9).** m=11, n=255, 640×480, φ=90°, Radeon 9800XT:
**120 fps, 59 M∆/s**, update budget one full n×n level = 21–26 ms of CPU work
(normal-map computation alone was 11 ms of it — see §4.3, we do it on the GPU).
Memory: 16·m·n² = 11 MB vertices + 8·m·n² = 6 MB normals. Screen-space error
99.9th percentile **< 1 pixel**.

**P19 — Explicitly out of scope for the clipmap (§1).** *"Buildings, vegetation,
and other objects that populate the environment are rendered separately using
other LOD techniques."* The paper itself draws the two-track line we drew
yesterday. Trees, rocks and grass in the demo scene stay on the cluster/instance
path.

**P20 — The one accuracy limitation (§1, §9).** The terrain is assumed to have
**bounded spectral density**. A tall needle-like feature morphs into view slightly
late. Fine for real terrain; relevant to us because our `terrain_height` includes a
**ridged** mountain term and a **canyon carve** — both of which are exactly the
"artificial cliff" case §9 calls out as the thing that skews max-error. See §7 G4.

---

## 2. What we change vs. the 2004 paper, and why

The paper is written against 2004 hardware and says so in three places. Each
modernisation below cites the paper's own forward-looking sentence, so this is
following the authors, not overriding them.

| # | Paper (2004) | AZRA (wgpu 22, 2026) | Paper's own note |
|---|---|---|---|
| M1 | Heights live in **vertex buffers**; CPU rebuilds indices every frame for toroidal access | Heights live in a **`R32Float` 2D texture array** (one layer per level), sampled in the **vertex shader**; one static index/footprint mesh reused by every level via instancing | §6.1: *"the 2D toroidal access requires CPU recomputation of vertex indices every frame … this will go away shortly"*; §10: *"use vertex textures … thereby greatly simplifying implementation"* |
| M2 | `z_c` **stored per vertex** | `z_c` = bilinear sample of level 𝑙−1's texture layer at the same world XY. Both layers are resident by P3.1, so this is exact, and it removes half the vertex payload | direct consequence of M1 |
| M3 | Synthesis / decompression on the **CPU**, uploaded | Procedural source evaluated by a **compute shader straight into the level texture**; no CPU round trip, no upload, no `n²` budget pressure | §8: *"We had hoped to implement the synthesis process using GPU pixel shaders … it is unfortunately not yet exposed"* |
| M4 | Normal map computed on CPU (11 ms of the 21 ms budget, P18) | Compute pass over the L-shaped dirty strip, writing an `RG8Snorm` array at 2× resolution | §10: *"it will soon be possible to perform all steps (except decompression) using the GPU"* |
| M5 | PTC-compressed residual pyramid (P15) | **Deferred to v2.** v1's source is procedural (M3) so there is nothing to decompress. Needed only when we import real heightmaps | not a disagreement — a scope cut |
| M6 | 4 rectangles per render region (§6.1, Fig. 3) | **12 blocks + 4 fix-up strips + 1 interior L-trim + outer degenerate ring** | ⚠️ **NOT from this paper** — from the GPU-clipmap follow-up (Asirvatham & Hoppe, *GPU Gems 2* ch. 2, 2005). Adopted because per-block culling is finer-grained than P14's rectangle cropping and every block is the same m×m mesh, which suits M1's instancing. Flagged so nobody thinks the paper said it. |

Everything **not** in this table is implemented as the paper specifies. In
particular P3 (constraints), P4 (α), P5 (w = n/10), P6 (degenerate stitch), P7
(fine-to-coarse), P9 (height suppression), P11 (budget/degradation), P12
(−1/16, 9/16 filter), P13 (2× normals, **no mipmaps**) are all normative here.

---

## 3. Data model

```rust
// core_engine/render/src/clipmap.rs  (new)

pub const CLIPMAP_N: u32 = 255;        // per P8 — configurable, must be 2^k − 1
pub const CLIPMAP_LEVELS: u32 = 11;    // m, per P18/P17 (>11 needs f64 per-level frames)

pub struct ClipmapLevel {
    /// Grid spacing g_l in world units. g_l = base * 2^(m-l).
    spacing: f32,
    /// Integer world-grid origin of clip_region(l), in units of `spacing`.
    /// Toroidal: texel (i,j) of the layer holds world sample (origin + (i,j)) mod n.
    clip_origin: IVec2,
    /// active_region(l) in the same integer grid — after cropping (P3.2/P3.4).
    active_min: IVec2,
    active_max: IVec2,
    /// P14 frustum culling bounds, refreshed on update.
    z_min: f32,
    z_max: f32,
    /// Set when clip_origin moved this frame and the L-strip is not yet filled.
    dirty_strips: ArrayVec<[IRect; 2]>,
}

pub struct ClipmapSystem {
    levels: [ClipmapLevel; CLIPMAP_LEVELS as usize],
    /// R32Float, n x n x m. Height. Toroidal.
    heights: wgpu::Texture,
    /// RG8Snorm, 2n x 2n x m (P13). Toroidal. Tangent-space XY, Z reconstructed.
    normals: wgpu::Texture,
    /// One shared footprint mesh set (M6): block, fixup-h, fixup-v, trim-L, degenerate ring.
    footprints: FootprintMeshes,
    /// Per-draw params, one slot per (level, footprint instance).
    instance_buffer: wgpu::Buffer,
    /// Generation counter — bind groups compare and invalidate (CLAUDE.md GPU lifetime rule).
    generation: u64,
    valid: bool,   // fails closed: false ⇒ terrain simply not drawn, never garbage
}
```

Memory at n=255, m=11: heights 4·255²·11 = **2.9 MB**; normals 2·510²·11 = **5.7 MB**.
At n=1023 (P8, 1080p @ 90°): 46 MB + 92 MB — which is why §6 exists.

**Nothing here is serialized.** No `.fl4` change, no `NodePayload` variant, no
`HalfEdgeMesh` field. Per §0.1 the clipmap is a renderer over a heightfield
source; the *source* (a procedural fn id, or later a heightmap asset reference)
is the only thing that could ever need saving, and v1's source is a compile-time
demo-scene function.

---

## 4. Per-frame algorithm

Mirrors §3's "per-frame algorithm" list, mapped onto AZRA's frame order
(`poll_background_tasks` → egui panels → `viewport_render`).

### 4.1 Desired active regions — in `poll_background_tasks`
For each level 𝑙: desired `active_region(l)` = the n·gₗ square centred on the
viewer XY (P2/§4). Then P9: if `viewer_height_above_terrain > 0.4 · n · gₗ`, set
it **empty**. Viewer height is read from the finest level whose clip region
contains the viewer — one texel fetch, cached CPU-side from the last update.

### 4.2 Clipmap update — in `poll_background_tasks` (Frame Ordering Contract)
Coarse-to-fine (P11). For each level, snap the desired clip origin to the level
grid, diff against `clip_origin`, and emit the **L-shaped** dirty strips (P10) —
at most 2 rectangles per level. Then, per strip:
1. dispatch the **height synthesis** compute (M3) writing `heights[layer=l]`
   toroidally, seeded from the upsampled level 𝑙−1 using the exact
   (−1/16, 9/16, 9/16, −1/16) tensor-product filter (P12) plus the deterministic
   noise table (P16, 50×50, indexed by `mod` on integer world coords);
2. dispatch the **normal** compute (M4) over the corresponding 2× region;
3. accumulate `z_min/z_max` for P14 (a small reduction, or a conservative
   analytic bound from the synthesis function — v1 uses the analytic bound).

Stop when accumulated updated samples exceed n² (P11). Levels not reached keep
their stale `clip_origin`; §4.3's crop then shrinks their active region, and the
degradation is the paper's designed behaviour, not a bug.

> **Frame Ordering Contract:** texture *creation* (resize on an `n` change) is a
> resource mutation and belongs here, behind a deferred flag, never in a panel
> callback. `generation += 1` on any recreate; bind groups compare.

### 4.3 Crop and render — inside the existing G-buffer pass
```
// coarse-to-fine — P3.2 and P3.4
for l in 1..=m {
    active[l] = active[l] ∩ clip[l];
    active[l] = active[l] ∩ (active[l-1] ⊖ 2);
    snap_perimeter_to_even(&mut active[l]);        // P3.3 — watertightness
}
// fine-to-coarse — P7, feeds early-Z / D2 HZB
for l in (1..=m).rev() {
    render_region[l] = active[l] − active[l+1];
    for block in decompose(render_region[l]) {     // M6
        if let Some(cropped) = frustum_crop(block, z_min[l], z_max[l]) {   // P14
            emit_draw(l, cropped);
        }
    }
    emit_degenerate_ring(l);                        // P6
}
```
Draws are appended to the **existing G-buffer render pass** in
`raster_gbuffer.rs` (§0.2). One pipeline, one bind group; per-draw state
(`level`, block offset, α parameters) rides the instance buffer.

### 4.4 Vertex shader (the whole LOD mechanism, ~20 instructions)
```wgsl
// world XY from the footprint's unit grid + this block's offset & level spacing
let world_xy = block_origin + vec2<f32>(pos_grid) * spacing;

// P4 — alpha from the CONTINUOUS viewer grid position; position-based, not time-based
let d  = abs(world_xy - viewer_xy) / spacing;              // in grid units
let a  = clamp((d - (half_extent - w - 1.0)) / w, vec2(0.0), vec2(1.0));
let alpha = max(a.x, a.y);

// M1/M2 — toroidal sample of this level and the next-coarser level
let z   = sample_toroidal(heights, l,     world_xy);
let z_c = sample_toroidal(heights, l - 1, world_xy);       // = the paper's per-vertex z_c
let z_morphed = mix(z, z_c, alpha);                        // P4

// P13 — the SAME alpha is handed to the FS to blend normal layers l and l-1.
out.alpha = alpha;
```

### 4.5 Fragment shader
Sample `normals[l]` and `normals[l-1]`, `mix` by `alpha` (P13/§6.3). **Sampler has
no mipmaps** (P13) and uses anisotropic filtering. Writes the standard 7 G-buffer
outputs — all of them, per the G-Buffer Contract.

---

## 5. Integration points (exact files)

| Concern | File | Change |
|---|---|---|
| System + regions + update scheduling | `core_engine/render/src/clipmap.rs` | **new** |
| Height synthesis + normals compute | `core_engine/graphics/src/clipmap_update.wgsl` | **new** |
| Terrain VS/FS | `core_engine/graphics/src/clipmap_draw.wgsl` | **new** |
| BGL entries (single source — CLAUDE.md) | `core_engine/graphics/src/bgl.rs` | `clipmap_update_bgl_entries()`, `clipmap_draw_bgl_entries()` |
| Pipeline creation, background-thread warmup | `PipelineRegistry` + `WarmupManifest` | register both |
| Per-frame update | `app/src/app_poll_pipeline.rs` (`poll_background_tasks`) | call `ClipmapSystem::update` |
| Draw | `app/src/raster_gbuffer.rs` | emit clipmap draws **inside** the existing G-buffer pass, **before** the mesh loop (fine-to-coarse already puts the near terrain first — P7) |
| Dev toggles | `app/src/win_tweaks.rs` | "Geometry Clipmap" section: on/off, level count, `n`, wireframe, show-α, pin-level, disable-morph |
| Demo scene source | `app/src/stress_test/demo_scene.rs` | expose `terrain_height` to the WGSL synthesis (port the fbm + ridged + canyon carve verbatim; **CPU/GPU parity test required**, §7 G1) |

**No shader-region hazard:** the two new WGSL files are standalone, not part of
`sdf_shared_all()`, so they do not invalidate the 4+ SDF pipeline cache entries
(the 25–43 s recompile). Deliberate.

---

## 6. The `n` problem (P8) — decided up front, not discovered later

P8 says n must scale with resolution and with narrow fov. Naïvely at 4K/60° that
is n≈2047, m=11 → 185 MB of height texture and ~46 M footprint vertices. Three
levers, in the order we should pull them:

1. **Drop `s` from 3 px to 4–5 px.** P18 measured the 99.9th-percentile
   screen-space error at **< 1 px** with s=3 (Table 2) — there is genuine slack.
   s=5 at 4K/90° gives n=1023 (46 MB), and the error stays sub-pixel-ish because
   error scales with s, not with n directly (`ê_l = e_l · s / g_l`, §9).
2. **Cut `m` to what the world actually spans.** The paper needs 11 levels for a
   216,000×93,600 dataset. AZRA's demo landscape is ~2 km. With a 0.25 m finest
   spacing, m=8 covers 32 km at level 1. m=8 is a **27 % memory cut** for free.
3. **Only then** consider frustum-adapted region placement — which the paper
   explicitly *rejected* (§4) because it breaks instant view rotation. Do not do
   this without owner sign-off; it trades a real interaction property for memory.

Decision for v1: **n = 511, m = 8, s ≈ 4 px at 1440p/90°.** 11 MB heights +
22 MB normals. Revisit only if G2 fails.

---

## 7. Gates (measured, in order — no gate skipped, per the Phase D pattern)

**G0 — parity.** WGSL `terrain_height` matches the Rust `terrain_height` to
< 1e-4 over 10⁵ sampled points. *Unit test in `render`, reading back the compute
output.* Without this, every later gate measures the wrong terrain.

**G1 — watertight, statically.** Camera parked at 30 positions across the demo
landscape (canyon rim, mountain flank, flat ground, high altitude). For each:
render to an offscreen target, then assert **zero background pixels** inside the
terrain silhouette. Reuses the `assert_no_holes`/pixel-diff harness from D4.
*This is the gate the cluster DAG could not pass on terrain; the clipmap should
pass it by construction, and if it does not, a P3 constraint is being violated —
check P3.3 (even perimeter) first.*

**G2 — no pop, dynamically.** Fly a fixed 20-second path through the canyon,
capture every frame, and assert the **max per-pixel depth delta between
consecutive frames** stays under a threshold calibrated on a
morph-disabled control run. Disabling the morph in dev tweaks must make this gate
*fail loudly* — otherwise the morph is not actually wired.

**G3 — frame cost.** Terrain-only frame time at 1440p vs today's demo-scene
terrain mesh. Target: **≤ 2.0 ms** and *constant* — the paper's headline property
is a steady rate independent of terrain roughness (§1). Measure over the canyon
(rough) and the flats (smooth) and assert the two are within 15 % of each other.
A large gap means something content-adaptive leaked in.

**G4 — the P20 case.** The canyon carve and ridged peaks are the "artificial
cliff" the paper flags. Verify the morph-in of a cliff edge is not objectionably
late; if it is, that is a *content* fix (soften the carve) or an `s` reduction,
**not** a special case in the shader. Record which.

**G5 — degradation.** Warp the camera at increasing speed; confirm updated-sample
count stays ≤ n² per frame, finer levels crop to empty, and **frame time goes
down, not up** (P11). If frame time rises with speed, the budget is not enforced.

**G6 — memory.** Measured VRAM matches the §3 arithmetic within 10 %. Constant
over a 5-minute flight (this is the property that makes clipmaps streamable).

**Regression check between every commit** (standing owner instruction): D1/D2
counters and the demo-scene frame time, before and after.

---

## 8. Sequencing

| Step | Content | Gate |
|---|---|---|
| C0 | **Owner answers §0.1.** No code before this. | — |
| C1 | `ClipmapSystem` skeleton, textures, generation counter, `valid` fails-closed, dev toggle that draws nothing | builds; zero frame-cost delta when off |
| C2 | Synthesis compute (M3) + P12 upsample + P16 noise; CPU/GPU parity | **G0** |
| C3 | Footprint meshes (M6) + VS world placement, **no morph, no normals** — flat-shaded, cracks expected and visible | renders; cracks confirm the transition work is real |
| C4 | Regions + P3 constraints + crop + fine-to-coarse order (P7) | **G1** minus the morph band |
| C5 | Morph (P4/P5) + degenerate stitch (P6) | **G1**, **G2** |
| C6 | Normals (M4/P13, 2×, no mipmaps) + α blend in FS | visual; G1 re-run |
| C7 | Frustum crop (P14) + height suppression (P9) + update budget (P11) | **G3**, **G5** |
| C8 | Tune `n`/`m`/`s` (§6); measure | **G4**, **G6** |

C1–C3 are independently useful even if the track is later dropped: C2's GPU
terrain evaluator is the same code the SDF `volume_bake.wgsl` image-sample work
(memory `project_terrain_image_displacement` T1) needs.

## 9. Deferred (v2+, explicitly not in this plan)

- **P15 compression** — only needed for imported real heightmaps (M5).
- Imported-heightmap source at all (v1 is procedural).
- **P17** per-level f64 frames — only if m > 11.
- Runtime terrain modification / sculpting into the clipmap (paper §10 future
  work). Note this would collide with §0.1's authoring model and needs its own
  owner decision.
- Spherical domain, GPU decompression, terrain collision.

---

## 10. Open items carried from the cluster track

The clipmap does **not** close these; they belong to D4 and stay open
(`PHASE_D4_CLUSTER_RASTER_PLAN.md`):
- Cluster coverage holes on the dense-stress bodies — **never measured on
  non-terrain assets since the double-draw fix (`168ee24`)**. Moving terrain off
  the cluster path is what makes that measurement honest.
- Whether the 219 frustum-rejected clusters are genuinely off-screen.
- `viewport_h` / `tan_half_fov` parity between the GPU cut params and the CPU
  replay.
