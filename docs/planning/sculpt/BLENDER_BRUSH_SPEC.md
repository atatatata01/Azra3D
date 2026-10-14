# Blender Sculpt Brush Feel Spec (oracle-derived, re-written in our own words — no GPL code)

Oracle: `C:\Program Files\Blender Foundation\Blender 5.0\blender.exe` (v5.0.1; 4.5 also present).
Sources: Blender `source/blender/editors/sculpt_paint/mesh/brushes/{grab,snake_hook,pinch,clay_strips}.cc` + `BKE_brush_curve_strength`.

## 0. Shared foundations
- **Default curve = SMOOTH** `3p²−2p³`, `p=1−dist/radius`. Our `falloff_curve_only` already matches all presets (Smooth/Smoother/Sphere/Root/Sharp/Linear/InvSquare). NO change to curve.
- Per-vertex weight = `curve(dist/r) * bstrength * automask * texture * hardness_remap * frontface`. Our `fall*strength` + `hardness_remap_e` matches.
- **Elastic rule (crux):** GRAB, SNAKE_HOOK, THUMB, ELASTIC, POSE, ROTATE sculpt from **pre-stroke ORIGINAL positions** + accumulated `grab_delta = location − last_location` (≈ cursor − stroke_start). `v = orig_pos + factor(orig_dist)*grab_delta`. Dragging back un-deforms → elastic feel. Our `ElasticGrab` (voxel_sculpt.rs:1049) already does this for Move.

## 1. GRAB (our Move) — mostly aligned
- Uniform `grab_delta` vector, falloff-weighted, all verts move same direction. Sphere falloff (Tube=view-plane project).
- Dragged (no dab spacing), original coords. No autosmooth/plane.
- **Gap:** Blender scales drag by `bstrength`; ours is strength-independent 1:1 (decide: recommend strength scales drag so soft grab possible). Else aligned.

## 2. PINCH — needs stroke-axis basis
- Build `stroke_xz` = [stroke tangent, surface normal]. Pull each vert toward the stroke **axis line** by keeping only tangent+normal components of `(center−pos)` (drop bitangent). `translation *= curve*bstrength`. Neg strength = bulge.
- Dab-spaced ~10%, current coords.
- **Gap:** ours (voxel_sculpt.rs:4003) is axisymmetric radial gather with no stroke direction. Add stroke_xz basis, gather only in-plane component. Keep trailing relax.

## 3. CLAY STRIPS — parabolic z + box tip + tight area normal
- Area plane pushed proud along area normal by offset. Brush-local box `mat` scaled by radius, `tip_scale_x` stretch.
- Per vert: `local = mat_inv*pos`; `xy_fac = curve(len(local.xy))` (square-ish, tip_roundness ~0.18); `z_fac = max(0, local_z*(1−local_z))` **parabolic, peak z=0.5**; `factor=xy_fac*z_fac*bstrength`; `pos += plane_normal*bstrength*radius * factor`.
- Area normal from **normal_radius_factor≈1.35 × radius sub-sphere** (tighter than footprint, stabler). Plane anchored at cursor since 2.81.
- Dab spacing 8–10%, implicit light autosmooth.
- **Gaps vs ours (line 3931/3963):** (a) no parabolic z-band → add `z(1−z)`; (b) no square/elongated tip → add brush-local box + x-stretch (wire existing soft-rectangle alpha line 439); (c) area normal from too-wide sphere → use 1.35 sub-sphere; (d) no autosmooth → light `relax_region`; (e) verify ~10% spacing.

## 4. SNAKE HOOK — NOT IMPLEMENTED (no VoxelBrush variant)
Three superimposed effects over ORIGINAL coords (like Grab), on a **capsule** (source→dest):
1. Tube pull: `translation = grab_delta * factor`.
2. Pinch: `pinch = 2*(0.5 − crease_pinch_factor)*|grab_delta|/radius`; squeeze `(pos−sculpt_co)` toward tube axis by `pinch*factor` (view-plane if Tube). **crease_pinch default 0.5 = OFF**; >0.5 narrows tip, <0.5 inflates (volume preserve). Growing pull auto-narrows tip into spike.
3. Rake: rotate `(pos−sculpt_co)` by `pow(rake_rotation, factor)` per-vert so tube bends along stroke curvature (curved horns).
- **To add:** new `VoxelBrush::SnakeHook` on `ElasticGrab` machinery + capsule AABB (reuse `grab_instances_disjoint`) + pinch term (expose `crease_pinch` default 0.5) + rake. NO relax (would smear tube).

## 5. Concrete voxel_sculpt.rs changes
- Curve/hardness: none.
- Move: decide strength-scales-drag (recommend yes).
- Pinch: stroke-axis basis, in-plane pull, keep relax.
- Clay Strips: parabolic z + box tip x-stretch + 1.35 area-normal + light autosmooth + 10% spacing.
- Snake Hook: new variant on ElasticGrab + capsule + pinch + rake, no relax.

## Oracle plan
Headless sculpt oracle (bevel pattern, docs/planning/poly/*_ref.py): apply each brush via `bpy.ops.sculpt` to subdiv plane/sphere, export vertex positions, diff vs our voxel-meshed output.

## Session log — 2026-07-23 (fix/sculpt-feel-perf)

### STEP 1 — Geometric oracle BUILT + committed (13465ab)
`oracle/sculpt_ref.py` drives a REAL `bpy.ops.sculpt.brush_stroke` and exports deformed geometry
+ a displacement-profile signature. Two hard-won headless findings (both in `oracle/README.md`):
- Blender 5.0.1 sculpt strokes **hard-crash (EXCEPTION_ACCESS_VIOLATION) under `--background`**
  (no GPU/PBVH). The oracle therefore runs in a live app-timer window that self-quits (~2s flash).
- Blender 5.0 renamed `sculpt_tool` → **`sculpt_brush_type`** and ships only "Draw"; the old name
  silently no-ops → every brush comes out as Draw (the first pass hit this — all peak_dz identical).
Captured Blender 5.0.1 refs: draw/grab/pinch/clay_strips/clay/snake_hook. `oracle/sculpt_diff.py`
gates on SHAPE (normalized profile corr + peak-bin offset) since voxel≠mesh forbids vertex parity.

Oracle signatures (grid for grab/pinch/draw, UV-sphere for clay — a flat plane nets ZERO clay):
- GRAB: `max_lat 1.67`, pure in-plane pull scaling with strength (→ confirms Move should scale by strength).
- PINCH: `cross_dy` sign FLIPS across the stroke line (+y verts move −y, −y verts move +y) — the
  cross-stroke squeeze toward the stroke AXIS. Peak ±0.045. Confirms the stroke-axis basis.
- CLAY_STRIPS `max_dr 0.032`, broad + mid-depth-peaked radial deposit vs round CLAY `max_dr 0.008`
  (tight, center-peaked) — confirms the parabolic normal-band + wider footprint.
- SNAKE_HOOK: strongest radial+lateral pull (`max_dr 0.84`, `dlat 0.27`) concentrated at the leading
  edge (tube tip) vs GRAB's uniform footprint.

### STEP 2 — Brushes (oracle-gated, one commit each)
1. **Move/Grab — SHIPPED (855422b).** Shared Strength now scales the view-plane grab drag
   (`input_sculpt.rs:4143`), matching Blender bstrength (was inert). Elastic pristine-restore
   unchanged. Oracle: grab pull scales linearly with strength → matched by construction (screen-space
   drag; no numeric voxel gate needed).
2. **Pinch — SHIPPED (35ed96f).** Replaced the axisymmetric radial gather with the stroke-axis basis:
   gather uses the BITANGENT (tangent×normal) so only the cross-stroke component squeezes toward the
   stroke line (matches `pinch_ref.json` cross_dy sign flip). ZERO tangent → axisymmetric fallback =
   the legacy golden-ref math, so `golden_parity` stays bit-exact. Plumbed the unit stroke tangent to
   the GPU via a new `BrushParams.stroke_tan` vec4; mirrored in `voxel_brush.wgsl`. **graphics 61/61
   (GPU parity + naga green), core_math golden_parity green.**
3. **Clay Strips — SHIPPED (bc9ff30).** ClayBuildup converge fill now weights deposit by z·(1−z)·4
   along the normal (peak mid-depth) → the flat-slab-tapering-at-both-ends profile `clay_strips_ref`
   shows. Mirrored across CPU-stamp / CPU-legacy / GPU (golden + GPU parity green). **Honestly
   DEFERRED**: §3b brush-local box tip + x-stretch and §3c 1.35× area-normal sub-sphere (need
   per-stamp brush-local-matrix + a separate area-normal sample pass — large plumbing); §3d autosmooth
   already exists as the `auto_smooth` toggle (forcing a second relax for one brush = duplicate path).
4. **Snake Hook — NOT SHIPPED (deliberate, No-Hacks).** The defining trait (a moving-anchor TUBE with
   axis pinch + rake) requires extending the mesh-grab GPU-displacement model — the fixed-anchor block
   translate in `gbuffer.wgsl` + the CPU release voxelizer (`grab_density_displace_*`) + symmetry — to
   a moving-anchor capsule with a `crease_pinch` axis-squeeze and a rake rotation, consistently across
   all three. That is multi-file and could not be landed + verified to the review bar in this session.
   Adding the `VoxelBrush::SnakeHook` variant routed through the existing Move path would ship a
   **Move-clone mislabeled as Snake Hook** — exactly the "workaround that pretends to be the feature"
   CLAUDE.md forbids — so the stub was reverted. `snake_hook_ref.json` is captured + committed so the
   next session is oracle-gated from the first line. NEXT: extend MeshGrab to `snake_hook` mode
   (anchor follows cursor; `crease_pinch` default 0.5=off squeezes gathered verts toward the tube axis
   by `2*(0.5−pinch)*|drag|/r`; rake rotates the gather source tracking drag-direction change) in the
   GPU vertex shader + the CPU revoxelize + symmetry together, then add the enum variant (END) + UI.

## Session log — 2026-07-23 (fix/sculpt-feel-perf) — FULL Blender brush set in SURFACE mode

Owner directive: the density-only greying of Pinch/Clay/Crease/Scrape/SnakeHook/Smudge in SURFACE
mode was wrong — Blender's Pinch/Clay/Snake Hook ARE vertex-displacement brushes and belong in
surface mode. Implemented the full set as real per-vertex displacement in
`core_engine/app/src/voxel_surface_sculpt.rs` (`voxel_surface_apply_dabs` + a dedicated Snake Hook
path), reusing the shared falloff/curve/spacing.

### Shared math (No-Hacks: ONE implementation)
- Extracted `pinch_axis_pull(rel, normal, tangent)` to `core_math::voxel_sculpt` (§2 stroke-axis
  basis: drop normal component, keep the cross-stroke bitangent term; zero-tangent → axisymmetric).
  The density Pinch gather (`voxel_sculpt.rs` Advect arm) now CALLS it instead of inlining the same
  math — one path for both the density stamp and the surface dab.

### Brushes now USABLE in surface mode (were greyed)
- **Pinch** — stroke-axis squeeze via `pinch_axis_pull`; `translation = −pull·curve·strength·sgn`.
- **Clay / ClayBuildup** — area plane (1.35×radius sub-sphere: mean vertex pos + mean vertex normal)
  pushed proud by `0.20·r`; DEPOSITS below-plane verts outward toward the plane, weighted by
  `curve · parabolic z(1−z)·4` (peak mid-depth = the flat-slab profile). Ctrl carves.
- **Scrape** — inverse Clay: one-sided flatten of proud verts to the area plane (no proud offset).
- **Crease** — Draw(−area-normal groove) + `pinch_axis_pull` squeeze, curve² (sharp). Ctrl = ridge.
- **Snake Hook** — NEW `VoxelBrush::SnakeHook` (enum END, save-format safe; CPU-only, no GPU op-code).
  Moving-anchor CAPSULE tube over PRISTINE (first-touch `surface_stroke_snap`) coords: tube pull
  `disp = drag·curve`, plus `crease_pinch` axis squeeze `2·(0.5−pinch)·|drag|/r·curve` (Pinch Amount
  slider, default 0.5 = off). Symmetry-mirrored. NO relax. One `UndoKind::Surface` entry via the
  existing snapshot. Latched anchor cleared on release/stroke-start. Greyed in VOXEL mode (tooltip:
  surface-only) — fail-closed, no fake density stamp.

### Still greyed in surface mode (honest, with tooltip)
- **Fill** — inverse-Scrape volumetric cavity DEPOSIT; a vertex push would be Draw, not a cavity fill.
- **Smudge** — density taffy warp; its vertex-drag analog IS the Move soft-round grab already in
  surface mode, so exposing it would duplicate Move (No-Hacks: one path).

### Oracle gate (docs/planning/sculpt/oracle/azra_surface_profile.py — a self-contained transcription
of the surface kernels applied to each Blender ref's REST mesh + the sculpt_ref.py stroke; diffed
via sculpt_diff.py). SHAPE-based (voxel/mesh ≠ Blender mesh):
- **Pinch** `cross_dy` (the cross-stroke squeeze sign-flip signature): shape **corr 0.757 (FAIR)**,
  peak-bin Δ=2, raw max-dev 0.058. The squeeze toward the stroke axis is present and correctly
  signed; the cross-section peak sits 2 bins in from Blender's.
- **Clay Strips** `dr_radial`: shape **corr 0.324**, peak-bin Δ=5, raw max-dev 0.030. Now deposits
  outward (was corr 0.0 / project-to-plane); residual = Blender's ELONGATED SQUARE tip spreads the
  deposit into an outer ring/strip along the sweep, while our footprint is round (the §3b box tip +
  x-stretch is still DEFERRED — needs a per-dab brush-local matrix). Direction + parabolic band match.
- **Snake Hook**: tube-pull MAGNITUDE matches closely — **`max_lat` 1.59 vs Blender 1.67 (~5%)**.
  The lateral DISTRIBUTION differs (`cross_lat` corr −0.13): Blender concentrates the pull at the
  leading edge / tube TIP (rake), ours is capsule-uniform. RAKE is DEFERRED (needs a per-vertex
  partial of the drag-direction change tracked across frames). NOTE the harness applies `z_drag`
  literally along +Z (max_dz 0.40) whereas Blender's captured stroke is almost purely lateral
  (max_dz≈0) — the real app projects the drag onto the view plane (like Blender), so app behaviour is
  closer to the ref than this harness's literal-z approximation overstates.

### Tests
- `cargo build -p app` GREEN. `cargo test -p graphics` 61/61 (golden_parity + naga). `cargo test -p
  core_math` 614 pass / 2 fail — BOTH pre-existing on HEAD ada1c02 (`grab_density_large_pull_keeps_
  solid_core` [prompt-exempt] AND `clay_buildup_packs_toward_proud_plane` [also failing on clean
  HEAD, verified by git-stash]); zero NEW failures from this work.

### DEFERRED (documented, not faked)
- Clay Strips §3b elongated square tip + x-stretch (round footprint ships; parabolic slab is in).
- Snake Hook rake (curved-horn drag-tracking); tube + axis-pinch ship.

### Session log — 2026-07-23 (fix/sculpt-feel-perf): ClayBuildup band zero-at-contact regression
- CORRECTION to the note above: `clay_buildup_packs_toward_proud_plane` was NOT pre-existing. It PASSES
  at c431d33 and FAILS at bc9ff30 (verified by checkout). bc9ff30's `z·(1−z)·4` parabola is 0 where z
  clamps to 1 — the CONTACT face / cursor point the test samples — so ClayBuildup deposited ~nothing
  where you sculpt (only mid-depth), and felt weak at the cursor in-app.
- Old weight (all 3 paths): `z = clamp((off-plane)/off,0,1); z_fac = clamp(4·z·(1−z),0,1)` (parabola,
  peak z=0.5, ZERO at z=0 and z=1).
- New weight: `z = clamp((off-plane)/off,0,1); s = clamp(z/0.6,0,1); z_fac = s·s·(3−2s)` (smoothstep
  plateau). z→1 is the contact face (cursor), z→0 the proud plane; z_fac ≈ 1 across z∈[0.6,1] and
  tapers softly to 0 by the proud plane. This is the broad flat-topped slab clay_strips_ref.json shows
  (tapers toward the plane edge, strong across the working depth) but is NON-ZERO/strong at the contact
  face — Blender's parabola operates over a brush-LOCAL box of mesh verts, so porting it literally into
  a volumetric surface→plane FILL wrongly zeroed the cursor cell. Zero-offset degenerate → passthrough.
- Mirrored identically: CPU stamp_plan converge (voxel_sculpt.rs:4024), CPU legacy golden-ref
  (voxel_sculpt.rs:4365), GPU voxel_brush.wgsl converge (:338). One batched WGSL edit.
- Tests: core_math `clay_buildup_packs_toward_proud_plane` NOW PASSES, plus `clay_adds_material_and_
  clamps` + `clay_buildup_adds_and_is_additive_only` green; only `grab_density_large_pull_keeps_solid_
  core` still fails (pre-existing, prompt-exempt). graphics 61/61 (golden_parity + GPU parity + naga).
  `cargo build -p app` green. Oracle not run (needs a live Blender 5.0.1 window, not --background).

## Session log — 2026-08-19 — SOURCE-VERIFIED Draw/strength/cursor parity (owner: "understand Blender sculpting code, do not go with feeling")
Fetched Blender 3.6 source (paint_stroke.cc, sculpt.cc, sculpt_brush_types.cc) and ported
three mechanisms VERBATIM into the surface kernel paths (poly + voxel Surface mode):

1. **Spacing-overlap strength attenuation** (`paint_stroke_integrate_overlap` +
   `paint_stroke_overlapped_curve`, gated in Blender by BRUSH_SPACE_ATTEN, default ON):
   per-dab strength × `1 / max_phase Σ_k falloff(|x − k·spacing|)`. THIS is what makes a
   Blender pass deposit ≈ strength × radius at any spacing. Ported as
   `core_math stroke_overlap_factor(spacing_in_radius_units, hardness, preset)`
   (Blender's `h = spacing%/50` on the same [-1,1] curve domain; 10 phases). Applied by
   BOTH stroke producers to Draw/Erase/Inflate ONLY — Blender's brush_strength gives
   Smooth `alpha·pressure·feather` (no overlap) and Grab `root_alpha·feather`.
   Gate: `pass_deposit_is_spacing_independent` (core_math).

2. **Blender-exact Draw/Inflate amplitudes** (SCULPT_do_draw_brush / do_inflate):
   Draw per-dab offset = `normal × radius × bstrength`, per-vertex `offset × fade` —
   the kernel's old `0.15` gain was an unprincipled stand-in for the overlap factor and
   is DELETED (amp = s·r; s carries overlap). Inflate = `vd.no × fade × radius` with
   brush_strength `0.25×` inflate / `0.125×` deflate — ported exactly (was 0.10 flat).
   Consequence: pass deposit is spacing-independent; a raw strength-1.0 dab without
   producer attenuation displaces a full radius (Blender does the same at alpha 1 +
   SPACE_ATTEN off) — kernel-direct tests must use moderate strengths.

3. **Area-normal deposit direction** (`calc_sculpt_normal` over the
   `normal_radius_factor` sub-sphere): poly Draw/Erase now displace along the AREA
   normal resolved by the SAME `accumulate_area_frames` pre-pass the plane brushes use
   (fail-OPEN to the pick normal on a missing frame — direction quality degrades, the
   dab never dies). The raw per-dab pick normal scattered deposit directions dab-to-dab
   = the owner's "strokes break the surface" class. Also fixed the pick normal itself:
   NEAREST live vert (was FIRST id in bucket order + silent Vec3::Y).

4. **Surface-projected cursor** (`paint_cursor.cc` / SCULPT_cursor_geometry_info_update):
   poly sculpt now raycasts the surface every hover frame (`poly_update_cursor_hit`,
   same budgeted pick as the stroke) and the ring draws ON the mesh with the surface
   normal (+ inner 50% ring); ray miss → flat 2D circle, exactly Blender's fallback.
   Pre-first-stroke hover included (Object-mode poly target gate).

NOT ported (deliberate): per-tool pressure curves (Clay pow4), Clay's `(1+overlap)/2`
(our Clay/plane brushes keep their own oracle-gated constants), feather, density-path
overlap (owner-tuned layer/build knobs there).
