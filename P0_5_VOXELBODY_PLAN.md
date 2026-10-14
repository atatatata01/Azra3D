# P0.5 — VoxelBody first-class node (implementation plan)
Active spec for MASTER_WORK_LIST P0.5. Delete/archive when shipped. Cite-checked against source (2026-06-19).
Replaces the `ImportedMesh` masquerade (`input_sculpt.rs:297`) + single global `self.voxel` (`main.rs:1070`) with a graph-owned node → multiple independently-editable voxel objects, each Tab-to-edit, mirroring SDF's `active_body_nid` (`scene_state.rs:24`).

## Guiding decisions
- **D1** Heavy data lives in the **graph node**, not app state. `NodePayload::VoxelBody` owns the `VoxelGrid`. `self.voxel` becomes a *transient working set* for one `active_voxel_nid` (like `self.primitives` vs `SdfBody.primitives`).
- **D2** During a stroke the **GPU atlas is still authority** (CLAUDE.md inversion); the node's grid is the persistent CPU cache, `cpu_grid_stale` gates readback. Exit-edit & save flush atlas→node grid (`materialize_cpu_from_atlas`, `input_sculpt.rs:1186`).
- **D3** Voxel **undo stays its own path** (GPU brick snapshot / CPU delta), app-global, reset on enter-edit. Per-object undo OUT OF SCOPE.
- **D4** **No blanket serde** on `VoxelGrid`/`DensityStorage`/`BrickGrid` (only `Clone` today; `BrickGrid` holds live GPU-sync fields `dirty/idx_lo/idx_hi/gc_dirty/free`, `voxel_sculpt.rs:424`). Persist via a compact side-channel record (occupied bricks only); node grid field is `#[serde(skip)]`.
- **D5** One node type, density-agnostic (`DensityStorage` already abstracts Dense<256³ / Sparse≥256³).

## Payload (add to `NodePayload`, scene_graph.rs:256, after ImportedMesh)
`VoxelBody { name, grid #[serde(skip)], mesh_cache_id: Option<MeshCacheId>, origin/rotation/scale, material }`.
Node OWNS: name, grid (density+res+bounds+iso), Object-Mode display mesh id, transform, material.
TRANSIENT in `VoxelObject` working set (active edit only): chunks/chunk_tris/dirty_chunks, stroke_aabb, atlas_stroke, cpu_grid_stale, profile, pending_rebuild. Brush/radius/strength = **tool-global**, not per-object.

## active-edit target
`active_voxel_nid: Option<NodeId>` in scene_state.rs. Invariant `voxel.is_some() ⟺ active_voxel_nid.is_some()`.
- `voxel_enter_edit(nid)`: exit any current → read node grid → build GPU atlas + chunk slots (`voxel_build_all_chunks`) + `voxel_undo_reset` → set active + work_mode=Sculpt.
- `voxel_exit_edit()`: flush atlas→node grid → update display mesh (`voxel_sync_node_mesh`) → tear down chunks + atlas → clear active.
- Multi-voxel: each node owns its grid; non-active render from cached mesh (no atlas/chunks); only ONE sculpt-active (single GPU atlas). Atlas teardown/rebuild on switch via **deferred flag in poll_background_tasks** (Frame Ordering Contract) + generation counter.

## Save/load (RISKIEST) — v6 → v7
- Current bug: grid is **never saved** (only the baked mesh as ImportedMeshRecord) → loading a v6 voxel = static mesh, grid gone. P0.5 fixes this.
- Add `VoxelGridRecord{ node_id, res, bounds, iso, payload: Dense(Vec<f32>) | Sparse{tags, bricks:Vec<(u32,Vec<f32>)>} }` + `VoxelGrid::to_record()/from_record()` (pure CPU, core_math). `from_record` resets live GPU-sync fields fresh (`gc_dirty=true`).
- `ProjectFile.voxel_grids: Vec<VoxelGridRecord>` (`#[serde(default)]`). Node grid `#[serde(skip)]`; side-channel carries it; reattach by node_id on load. Route serialize through the existing 8MB-stack thread.
- Bump version 6→7. **v6 ImportedMesh voxels load static (NO auto-convert** — grid never existed); optional honest toast. Forward-version guard already blocks v7-in-v6.
- Mandatory round-trip test: sculpt → save → load → grid `PartialEq` equal → continue.

## Render
- Object Mode (non-active VoxelBody): render cached `mesh_cache_id` like any mesh (add a `VoxelBody` arm to the raster draw match).
- Sculpt Edit (active): unchanged two strategies — raster chunks ≤512³ / atlas raymarch >512³ (Room/Display Invariant, never fused).
- Detection changes: `is_voxel` (`raster_render.rs:423,725`), raymarch gate (`viewport_render.rs:320`), overlay-skip (`draw_selection.rs:383`) switch from `self.voxel.node==nid` to `matches!(payload, VoxelBody{..}) && active_voxel_nid==Some(nid)` for the chunked path. Picking path preserved verbatim (selection-P0).

## Selection / outliner / edit-entry
- Selects as `SceneItemId::Node(nid)` (no new variant). Picking returns the real VoxelBody node id.
- Outliner: add `VoxelBody` arm with a `VOX` row (extend `draw_outliner_node_row`, don't duplicate); active-highlight via `active_voxel_nid`.
- `enter_sculpt_mode` (`workspace.rs:189`): `SelKind::Voxel` = "selected node is a VoxelBody" → `voxel_enter_edit(nid)`. `voxel_create` builds the node then enters edit + auto-selects.

## Step sequence (each builds green & ships; 🔴 high / 🟡 med / 🟢 low risk)
- **Step 0 🟢 (FIRST):** `VoxelGridRecord`/`VoxelGridPayload` + `VoxelGrid::to_record()/from_record()` in core_math + round-trip unit test (dense 64³ + sparse 256³). `cargo test -p core_math`. No app wiring, no behavior change. De-risks save/load first.
- **Step 1 🟢:** add `NodePayload::VoxelBody` variant (grid `#[serde(skip)]`, `default_voxel_grid()`); fix all exhaustive match arms (compiler-driven, passthrough/`_`). Nothing creates it yet.
- **Step 2 🟢:** `active_voxel_nid` field + `voxel_enter_edit/exit_edit` stubs wrapping today's behaviour.
- **Step 3 🟡:** `voxel_create` builds a `VoxelBody` node (not ImportedMesh), grid in node; working set borrows. Auto-select.
- **Step 4 🔴:** render swap — VoxelBody arms in raster draw + picking + raymarch gate. Verify select works (P0), two voxels visible.
- **Step 5 🟡:** outliner `VOX` row + Tab-to-edit via `voxel_enter_edit`.
- **Step 6 🟡:** real enter/exit working-set load+flush + multi-voxel switch (deferred poll + generation counter).
- **Step 7 🔴:** save/load v6→v7 (records, reattach, no auto-convert). Mandatory round-trip + old-file tests.
- **Step 8 🟢:** cleanup — `VoxelObject` no longer owns heavy data; delete dead single-mesh save path.

## Out of scope: per-object persistent undo; multi-atlas caching across objects; SDF→voxel / mesh-surface sculpt.
