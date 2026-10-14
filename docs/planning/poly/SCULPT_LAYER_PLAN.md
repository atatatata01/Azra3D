# PolySculpt Layer (Poly Modifier) — Plan v2 (2026-08-14)

**Owner ask:** Sculpt modifier after Remesh/Smooth in the poly stack (*modeling → Remesh → Smooth →
Sculpt*). **No dynamesh.** **Same brushes as the sculpt room** — same recipes, same feel, not a
parallel mesh-brush set.

**v2:** external review (Grok) audited v1 against the tree; every load-bearing correction was
re-verified in-tree before adoption. v1's three broken pillars — the perf story (stale prefix
hit), the display path (pos-edit), and "domain-neutral StampPlan consumer" — are fixed here.
**Do not implement from v1.** Queued behind the baker program.

**Naming:** this feature is **PolySculpt** everywhere in docs and UI code comments. "Sculpt" is
already three other things in-tree (SDF `ModifierType::Sculpt`, SDF `SculptLayer` delta grids, the
legacy `SculptBackend` trait). The enum variant may read `Sculpt` inside `PolyModifierType`, but
every doc/comment reference says PolySculpt to keep implementers off the SDF types.

---

## 0. Verified foundation (2026-08-14)

- Prefix ladder caches upstream prefixes (`apply_poly_modifiers_cached`) — the reason strokes can
  avoid re-running Remesh. **But see §1: the ladder as-is would serve a STALE full hit.**
- **A mesh-displacement brush kernel already exists**: `voxel_surface_sculpt.rs` —
  `surface_brush_supported` (Draw, Erase, Smooth, Flatten, Inflate, Move, Pinch, Clay/ClayBuildup,
  Scrape, Crease, SnakeHook; Fill/Smudge greyed) and `displace_verts_for_dabs` (:716), a pure
  `(verts, dabs, brush)` function. **This is the S0 kernel.** We do NOT write a third mapping
  table; along-normal shorthand from v1 is dead — Clay is area-plane + parabolic band, as surface
  mode already defines it.
- `StampPlan` is documented **voxel-local** with ONE producer inside `input_sculpt`
  (`WorkMode::Sculpt` + voxel body gate); `ChannelMask::DISPLACE` exists but is an unused tag.
  "Second StampPlan consumer" is therefore a REFACTOR GOAL (parked, §S4), not S0 reality. Feel
  parity is a **producer-sharing** problem: pressure, spacing, lazy mouse, symmetry all live in
  `input_sculpt` and must drive PolySculpt strokes too.
- Serialization rails: `PolyModifierType`'s LAST variant is **`Bend`** — `Sculpt` appends AFTER
  it. Exhaustive sites that must be updated in the same change: `PolyModifier::label()`
  (node.rs:968), `apply_one_poly_modifier` (mesh_sync.rs:3103), `all_poly_modifier_types()`
  (mesh_sync.rs:3989), and the MANUAL Add-Modifier table (properties_poly.rs:653 — omit it there
  and the feature is unreachable).
- Newest tail is **FL4PBEND** (append-newest-LAST, project/io.rs:322). `FL4SCLP` goes AFTER it,
  own record type; never widen an existing record (a widened old section fails the whole load).
- Rebase helper exists: `project_points_to_mesh` / `_filtered` (io_bridge/mesh_to_sdf.rs:265) —
  bounded nearest-surface snap with a miss signal. S2 reuses it; no third projector.
- Honest numbers: remesh ≈191 ms/eval on closed solids is the **incremental-lockout cost**
  (REMESHSOLIDNOINCR) — the thing the cached prefix avoids re-paying per stroke. Normals region
  recompute ≈4.3 ms at 20k seeds but 138 ms at 161k — a LARGE stamp is not the cheap case; stamp
  radius bounds the seed set. The v1 "~19 ms display" figure was a 4-vert drag split, not a
  stroke; do not budget from it.

## 1. The three contracts v1 got wrong

**C1 — Last-slot cache contract.** Deltas are sidecar state; the ladder hashes only modifier
fields, so a constant Sculpt slot = permanent full-ladder hit = **new strokes never apply**.
Contract: the PolySculpt slot is **never a full prefix hit**. Mechanism: clone the upstream
prefix output ONCE into a **working mesh** owned by the modifier's runtime state; strokes mutate
it in place; upstream prefix Arcs stay immutable (naive last-slot invalidation deep-clones the
remesh output every stroke because the prefix still holds the Arc). A delta-generation counter
folds into the Sculpt slot's prefix hash so undo/redo/load re-applies correctly.

**C2 — Display contract.** `poly_sync_after_position_edit` is a BASE-CAGE path and
`poly_mods_need_geometry_reapply` counts Remesh/Smooth as geometry → the intended stack always
takes the wipe-prefix + full-sync path. PolySculpt display = **in-place vertex patch of the
last-modifier display mesh** (`update_chunk_verts_in_place` family — same machinery voxel surface
mode uses), region normals via `recompute_normals_region` seeded by the stamp only.

**C3 — Feel contract.** Same brushes = same **producer** and same **kernel**: strokes enter
through `input_sculpt`'s stroke loop (extended to fire when the active poly body has an enabled
PolySculpt modifier — the same pressure/spacing/lazy-mouse/symmetry code paths), and apply through
`displace_verts_for_dabs`. `fold_recipe` byte-identity is a unit test of packing, NOT a feel
proof; it is not the parity gate.

## 2. Known costs named up front

- **Spatial query:** `HalfEdgeMesh` has no vertex-radius query (surface mode uses chunk AABBs).
  S0b ships a uniform-grid hash over the working mesh (rebuilt on rebase, updated on stroke), or
  accepts an O(V) scan behind a measured budget — decided by measurement in S0b, stated in the
  gate result.
- **Tagged stacks disable the ladder** (`poly_stack_needs_tagged_eval`): [Bevel(smart-select), …,
  Sculpt] gets NO remesh cache. Documented limitation in the modifier UI hint; not solved here.
- **Undo stream:** poly episode stream (`begin_episode` + sparse delta snapshot payload), NOT
  voxel `UndoKind::Surface`. One stream; mixing them is how voxel undo grew barrier cases.
- New modifier-UI warning ("editing above a PolySculpt layer will rebase it") has no existing
  pattern — it is real UI work in S2, not a one-liner.

## 3. Gates (S0 split per review — v1's S0 was 2–3 sessions in one)

### S0a — Type + stored-delta apply + cache rule *(no live brushing yet)*
- `PolyModifierType::Sculpt` after `Bend` + ALL exhaustive sites + Add-Modifier row.
- Runtime delta store; `apply_one_poly_modifier` arm = add stored deltas to upstream output.
- C1 cache rule (delta generation in the slot hash; never-full-hit proven).
- **Verify (headless):** inject deltas → display shows them; remesh eval count constant across N
  delta edits (the perf property); no-modifier no-op + toast; ladder test that a stale full-hit
  is IMPOSSIBLE (the v1 bug as a negative test).

### S0b — Working mesh + in-place display
- Clone-once working mesh (C1), in-place vert patch display (C2), stamp-bounded region normals,
  spatial query (grid hash vs measured O(V) — decide by numbers).
- **Verify:** stroke-sized delta edit at 323k body: display bytes update, upstream Arcs unmoved
  (pointer assert), frame cost measured and recorded (no inherited v1 numbers).

### S0c — Live strokes, three brushes
- Producer sharing (C3): `input_sculpt` stroke loop drives PolySculpt when the active poly body
  has an enabled PolySculpt modifier; Draw/Grab/Smooth via `displace_verts_for_dabs`.
- Undo episode per stroke (sparse snapshot).
- **Verify:** headless AZRA_SCULPTLAYER stamp → delta region + direction assert; pressure/spacing
  fed through the SAME producer code paths (call-site assert, not a feel claim).

### S1 — Persistence
- `FL4SCLP` tail AFTER FL4PBEND, own record; golden old-file fixture (no magic when unused);
  round-trip test modeled on `poly_remesh_params_round_trip`.

### S2 — Rebase on upstream change
- `project_points_to_mesh_filtered` re-splat; orphan count from its miss signal → loud toast
  ("N deltas lost"); modifier-UI warning for edits above a PolySculpt layer (new UI, budgeted as
  such).
- **Verify:** sculpt → change remesh voxel size → deltas survive within tolerance; fully-orphaned
  region reports its count (negative test).

### S3 — Brush breadth + feel parity
- Lift `surface_brush_supported` + hints as-is (Fill/Smudge stay greyed); Clay/Flatten/Pinch use
  surface-mode mappings verbatim.
- Parity bar: same recipe, same dab sequence → PolySculpt matches surface-mode displacement on a
  reference mesh (numeric compare), plus owner feel pass.

### S4 — Parked
Dynamesh (stack composition gives it later); GPU mesh kernel; `apply_stamp_plan_mesh` via
`ChannelMask::DISPLACE` (the "real second StampPlan consumer" refactor — only if/when surface
mode and PolySculpt should converge on one engine); region-raster display patch (shared perf
backlog item).
