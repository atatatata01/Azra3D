//! GATE 4 (DC consolidation) — `MeshPresentation` facade + the ONE shared "materialize a body's
//! mesh to CPU" job.
//!
//! Before this gate, three independent predicates answered "does this SDF body have a drawable
//! mesh?" at every consumer site (`mesh_cache.get(cid)`, `gpu.sdf_body_mesh.contains_key(nid)`,
//! `sdf_mesh_pool.slot(nid)`), and TWO independent producers materialized CPU meshes: the PT
//! snapshot pump (`pt_scene::pump_pt_sdf_snapshots` — reads back the SAME GPU DC the display
//! shows) and the export window (`win_export.rs` — a SEPARATE CPU `mesh::dual_contouring` over
//! the flat prim list: different algorithm, different bounds, different output → export never
//! matched the viewport).
//!
//! After this gate:
//! - **`SdfMeshPresentation`** is THE answer to "has mesh / from which store / how is it drawn".
//!   Derivation is a pure function (`derive_presentation`) fed by `AzraApp::sdf_mesh_presentation`
//!   — draw (raster_gbuffer), pick (raster_pick) and PT gather consume it, so they cannot drift.
//!   The route half stays `CacheState::dc_mesh_route` (Gate 3); this facade WRAPS it plus the
//!   per-body slot/pool/mesh_cache state.
//! - **Materialize** is ONE job with three arms, in strict preference order:
//!     1. sparse display readback  — temp-slot mesh dispatch from the body's OWN brick atlas
//!        (the PT pump's proven machinery, extracted here as `dispatch_sdf_display_readback`);
//!     2. dense display readback   — copy the body's display slot buffers (no dispatch at all);
//!     3. grid DC                  — CPU-bake the body field at the requested res, then the SAME
//!        GPU QEF-DC shader (`SdfMeshGpu::mesh_readback`), falling to the CPU `chunked_dc`
//!        oracle only when no GPU/mesher exists (the documented no-GPU fallback).
//!   The PT pump keeps its settle-gate/hash keying/one-per-poll bounding VERBATIM and calls arm
//!   1/2 through `dispatch_sdf_display_readback`; export calls `materialize_sdf_body_mesh` with a
//!   bounded blocking readback (`device.poll(Wait)` — the `dispatch_readback` precedent; export
//!   is a one-shot user action processed in poll phase, never per-frame).
//!   **Convert SDF→Poly (O5)** does NOT use this blocking path: it queues + async-settles
//!   (`convert::poll_convert_sdf_to_poly`) so the UI never freezes on `Wait`.
//!
//! Frame Ordering Contract: everything here that creates GPU buffers (temp slots, stagings) is
//! poll-phase-only — export requests are DEFERRED from the egui panel via
//! `UiState::export_pending` and processed in `poll_background_tasks`.

use eframe::wgpu;
use glam::Vec3;

use core_math::scene_graph::{MeshCacheId, NodeId, NodePayload, SceneGraph};

use crate::sdf_gpu_mesh::{readback_slot, PendingSdfReadback, SdfBodyMeshSlot, SdfMeshGpu};

// ================================================================================================
// The facade — "has a mesh?" answered ONCE.
// ================================================================================================

/// Where (and whether) an SDF body's drawable mesh lives. ONE display technique per body — the
/// variants are mutually exclusive by construction (resident wins: when the resident route is
/// active and the slot exists, the body is drawn/picked from the slot even if a stale/PT-snapshot
/// `mesh_cache` entry also exists).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum SdfMeshPresentation {
    /// No mesh anywhere — the body is not drawable (skipped by draw/pick; invisible until baked).
    None,
    /// GPU-resident slot (`GpuCtx.sdf_body_mesh`) — drawn via `draw_indexed_indirect`.
    /// `pooled` = the slot's CURRENT mesh lives in a region of the shared `SdfMeshPool`
    /// (multi-draw) AND the pool still holds that region — the exact lockstep predicate the
    /// G-buffer and pick gathers must share (Stage B-1).
    Resident { pooled: bool },
    /// CPU mesh in `render::mesh_cache` — drawn via the classic per-mesh raster upload.
    CachedCpu,
}

impl SdfMeshPresentation {
    #[inline]
    pub(crate) fn is_resident(self) -> bool {
        matches!(self, SdfMeshPresentation::Resident { .. })
    }
    #[inline]
    pub(crate) fn is_pooled(self) -> bool {
        matches!(self, SdfMeshPresentation::Resident { pooled: true })
    }
    #[inline]
    pub(crate) fn has_any_mesh(self) -> bool {
        !matches!(self, SdfMeshPresentation::None)
    }
}

/// PURE derivation — the facade's whole truth table, testable headlessly.
/// Inputs are the exact raw predicates the old call sites combined ad hoc:
/// - `route_resident`: `CacheState::dc_route_is_resident()` (Gate 3);
/// - `slot_present` / `slot_pooled`: `gpu.sdf_body_mesh` state for the body;
/// - `pool_region`: `gpu.sdf_mesh_pool.slot(nid).is_some()`;
/// - `cpu_mesh`: `mesh_cache.get(cid).is_some()`.
pub(crate) fn derive_presentation(
    route_resident: bool,
    slot_present: bool,
    slot_pooled: bool,
    pool_region: bool,
    cpu_mesh: bool,
) -> SdfMeshPresentation {
    if route_resident && slot_present {
        SdfMeshPresentation::Resident { pooled: slot_pooled && pool_region }
    } else if cpu_mesh {
        SdfMeshPresentation::CachedCpu
    } else {
        SdfMeshPresentation::None
    }
}

impl crate::AzraApp {
    /// GATE 4 — THE one "does this SDF body have a drawable mesh, and from which store?" call.
    /// Replaces the 3 ad-hoc predicates (`sdf_body_mesh.contains_key` / pool-slot presence /
    /// `mesh_cache.get`) at the draw, pick and PT-gather sites.
    pub(crate) fn sdf_mesh_presentation(
        &self,
        nid: NodeId,
        cid: Option<MeshCacheId>,
    ) -> SdfMeshPresentation {
        let (slot_present, slot_pooled, pool_region) = match self.gpu.as_ref() {
            Some(g) => (
                g.sdf_body_mesh.contains_key(&nid),
                g.sdf_body_mesh.get(&nid).map_or(false, |s| s.pooled),
                g.sdf_mesh_pool.as_ref().map_or(false, |p| p.slot(nid).is_some()),
            ),
            None => (false, false, false),
        };
        let cpu_mesh = cid.map_or(false, |c| self.cache.mesh_cache.get(c).is_some());
        // Post-DC fold (Detail LO and/or mesh-mod stack): draw folded CPU mesh (CachedCpu)
        // instead of the raw GPU-resident slot. Only when the fold stamp is CURRENT — if
        // bake/PT invalidated the stamp and re-sync has not landed, fall through to Resident
        // so drag stays live (otherwise CachedCpu freezes on the pre-edit fold).
        //
        // BUG FIX: previously only mesh-mod stacks forced CachedCpu, so Detail LO alone
        // folded into mesh_cache but Object Mode still drew raw Resident — LO appeared broken.
        // Also force CachedCpu while Quick Normal Bake preview is active on this body.
        let bake_preview = self
            .quick_normal_ghost
            .as_ref()
            .map(|g| g.nid == nid)
            .unwrap_or(false);
        if cpu_mesh
            && (bake_preview
                || (self.body_needs_display_fold(nid)
                    && self.cache.mesh_mod_display_stamp.contains_key(&nid)))
        {
            return SdfMeshPresentation::CachedCpu;
        }
        derive_presentation(
            self.cache.dc_route_is_resident(),
            slot_present,
            slot_pooled,
            pool_region,
            cpu_mesh,
        )
    }

    #[inline]
    pub(crate) fn body_has_mesh_modifiers(&self, nid: NodeId) -> bool {
        matches!(
            self.scene.scene_graph.get(nid).map(|n| &n.payload),
            Some(NodePayload::SdfBody { mesh_modifiers, .. }) if !mesh_modifiers.is_empty()
        )
    }

    /// True when compile would emit ≥1 runtime op (enabled cards).
    #[inline]
    pub(crate) fn body_has_active_mesh_modifiers(&self, nid: NodeId) -> bool {
        use core_math::display_mesh_ops::compile_display_mesh_stack;
        match self.scene.scene_graph.get(nid).map(|n| &n.payload) {
            Some(NodePayload::SdfBody { mesh_modifiers, .. }) if !mesh_modifiers.is_empty() => {
                !compile_display_mesh_stack(mesh_modifiers).is_empty()
            }
            _ => false,
        }
    }

    /// True when this **top-level SDF body** needs post-DC fold (Detail LO and/or mesh-mod stack).
    #[inline]
    pub(crate) fn body_needs_display_fold(&self, nid: NodeId) -> bool {
        if self.scene.scene_graph.is_sub_body(nid) {
            return false;
        }
        match self.scene.scene_graph.get(nid).map(|n| &n.payload) {
            Some(NodePayload::SdfBody { .. }) => {
                let detail = self.cache.body_vol_cache.use_detail_weighted_lo;
                detail || self.body_has_active_mesh_modifiers(nid)
            }
            _ => false,
        }
    }

    /// Facade twin for consumers that specifically need a CPU-materialized mesh in `mesh_cache`
    /// (the PT BLAS source): a RESIDENT body may ALSO have a landed PT snapshot here — that is
    /// not a contradiction of one-technique-per-view (PT is a different view of the same body).
    #[inline]
    pub(crate) fn sdf_cpu_mesh_present(&self, cid: MeshCacheId) -> bool {
        self.cache.mesh_cache.get(cid).is_some()
    }
}

// ================================================================================================
// The shared materialize job.
// ================================================================================================

/// GPU display-mesh readback source — arm 1/2 of the materialize job. Both arms return the SAME
/// async 3-staging-buffer contract (`PendingSdfReadback`); the PT pump polls it across frames,
/// export blocks on it once.
pub(crate) enum SdfDisplayReadbackSrc<'a> {
    /// Brick-resident (≥512) body: TEMPORARY mesh from the body's sparse atlas
    /// (never touches the display slot), then read the temp slot back.
    /// `lod_level` 0 = full surface mesh; 1+ = octree-coarser (required for PT at 1024³/2048³
    /// so BLAS/SSBO uploads don't device-lose).
    SparseAtlas {
        atlas: &'a graphics::SdfBrickAtlasGpu,
        res: u32,
        gmin: Vec3,
        gmax: Vec3,
        nid: NodeId,
        lod_level: u32,
    },
    /// Dense field-direct body: the display slot already holds the current mesh — copy its
    /// buffers, NO dispatch. Caller must have validated `display_slot_snapshot_ok`.
    DisplaySlot(&'a SdfBodyMeshSlot),
}

/// Is a dense display slot a valid monolithic CPU-readback source? (pooled/tiled slots keep
/// counters in pool regions; an LOD'd slot holds a coarsened mesh — neither is valid.)
/// Extracted verbatim from the PT pump's `slot_ok` predicate so export and PT agree.
#[inline]
pub(crate) fn display_slot_snapshot_ok(slot: &SdfBodyMeshSlot, res: u32) -> bool {
    slot.res == res && !slot.pooled && !slot.tiled && slot.lod_level == 0
}

/// Kick the display-mesh readback for either arm. POLL PHASE ONLY (creates buffers — Frame
/// Ordering Contract). This is the PT pump's dispatch block, extracted so export reads back the
/// SAME GPU DC output the display shows.
pub(crate) fn dispatch_sdf_display_readback(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    mesher: &SdfMeshGpu,
    src: SdfDisplayReadbackSrc<'_>,
) -> PendingSdfReadback {
    match src {
        SdfDisplayReadbackSrc::SparseAtlas {
            atlas,
            res,
            gmin,
            gmax,
            nid,
            lod_level,
        } => {
            // Temp slot must match atlas.res (assert inside mesh_into_slot_sparse*).
            let mut tmp = SdfBodyMeshSlot::new(device, res);
            if lod_level > 0 {
                // Coarser octree mesh for PT / export-light — not the full L0 surface soup.
                mesher.mesh_into_slot_sparse_lod(
                    device, queue, &mut tmp, atlas, gmin, gmax, lod_level, None,
                );
            } else {
                mesher.mesh_into_slot_sparse(
                    device, queue, &mut tmp, atlas, gmin, gmax, nid, None,
                );
            }
            // If sparse rejected (over bind limit), counters stay 0 → empty readback (safe).
            readback_slot(device, queue, &tmp)
        }
        SdfDisplayReadbackSrc::DisplaySlot(slot) => readback_slot(device, queue, slot),
    }
}

/// CPU-oracle bake+mesh of one body at `res` grid POINTS — the exact `dc_baseline::body_golden`
/// pipeline (recursive prim collect → mirror-aware bounds → pad → `BodyField` grid eval →
/// `ChunkedMesh` chunk 32, sharp). This is the production twin of the golden harness and the
/// no-GPU fallback of the materialize job. Returns the quad `Mesh` plus the padded grid bounds.
pub(crate) fn cpu_oracle_mesh(sg: &SceneGraph, nid: NodeId, res: u32) -> (core_math::mesh::Mesh, Vec3, Vec3) {
    use crate::bake_utils::{bake_body_grid_field, collect_body_prims_recursive,
                            compute_body_bake_bounds, pad_grid_bounds};
    let (mflags, mcenter, bevel) = match sg.get(nid).map(|n| &n.payload) {
        Some(NodePayload::SdfBody { mirror_flags, mirror_center, bevel_radius, .. }) => {
            (*mirror_flags, *mirror_center, *bevel_radius)
        }
        _ => (0u8, Vec3::ZERO, 0.0f32),
    };
    let prims = collect_body_prims_recursive(sg, nid);
    let (tmin, tmax) = compute_body_bake_bounds(&prims, mflags, mcenter, bevel);
    let (gmin, gmax) = pad_grid_bounds(tmin, tmax);
    let field = core_math::field_eval::BodyField::from_graph(sg, nid);
    let grid = bake_body_grid_field(&field, res, gmin, gmax);
    let mut cm = core_math::chunked_dc::ChunkedMesh::new(res - 1, 32, gmin, gmax, true);
    cm.extract_all(&grid);
    (cm.to_mesh(), gmin, gmax)
}

/// Convert a CPU quad `Mesh` into the GPU-DC CPU-mesh format: `[pos.xyz, normal.xyz, uv=0,0]`
/// verts + TRIANGLE indices (each quad split `a,b,c` / `a,c,d` — the diagonal the old export tri
/// branch used, and the one the raster upload uses).
pub(crate) fn mesh_to_verts8(mesh: &core_math::mesh::Mesh) -> (Vec<[f32; 8]>, Vec<u32>) {
    let verts: Vec<[f32; 8]> = mesh
        .vertices
        .iter()
        .zip(mesh.normals.iter())
        .map(|(p, n)| [p.x, p.y, p.z, n.x, n.y, n.z, 0.0, 0.0])
        .collect();
    let mut idx = Vec::with_capacity(mesh.quads.len() * 6);
    for q in &mesh.quads {
        idx.extend_from_slice(&[q[0], q[1], q[2], q[0], q[2], q[3]]);
    }
    (verts, idx)
}

/// Which arm produced a materialized mesh (reported in the export status + tests).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum SdfMaterializeArm {
    /// Read back from the body's live GPU display mesh (atlas temp-slot or display-slot copy) —
    /// hash-identical to what the viewport draws. Verts are BODY-LOCAL and predate any pending
    /// rigid offset, so the caller must apply `mesh_rigid_offset` like the draw/PT gathers do.
    DisplayReadback,
    /// CPU field bake at the requested res + the SAME GPU QEF-DC shader (blocking readback).
    GpuGridDc,
    /// CPU `chunked_dc` oracle (no GPU / no mesher) — the documented parity fallback.
    CpuOracle,
}

impl crate::AzraApp {
    /// GATE 4 — materialize ONE body's mesh to CPU (`[f32;8]` body-local verts, triangle
    /// indices). Shared by export (blocking, poll phase) and — via the extracted
    /// `dispatch_sdf_display_readback` — the PT snapshot pump.
    ///
    /// `res_override = None` → the body's DISPLAY resolution via the display-readback arm
    /// (export == display, the Gate-4 GO metric). `Some(res)` different from the display res →
    /// the grid-DC arm at that res through the SAME QEF-DC shader — never a different algorithm.
    ///
    /// POLL PHASE ONLY (creates temp GPU buffers). Blocking choice: a bounded `poll(Wait)` on a
    /// one-shot user action (the `dispatch_readback` precedent) — NOT the pump's async pattern,
    /// which exists for per-frame work.
    pub(crate) fn materialize_sdf_body_mesh(
        &mut self,
        nid: NodeId,
        res_override: Option<u32>,
    ) -> Option<(Vec<[f32; 8]>, Vec<u32>, SdfMaterializeArm)> {
        let (verts, indices, arm) = self.materialize_sdf_body_mesh_raw(nid, res_override)?;
        // Export-only quality fold (Path A style): even cylinder spacing + soft-K bevel
        // topology + smooth normals. Ignores live B2 identity skip — live viewport untouched.
        let (verts, indices) = self.fold_body_display_for_export(nid, verts, indices);
        Some((verts, indices, arm))
    }

    /// Raw display mesh **before** post-DC mesh modifiers (for base cache + fold-only rebuilds).
    pub(crate) fn materialize_sdf_body_mesh_raw(
        &mut self,
        nid: NodeId,
        res_override: Option<u32>,
    ) -> Option<(Vec<[f32; 8]>, Vec<u32>, SdfMaterializeArm)> {
        // Display source (hash, display res, grid bounds) — same accessors the PT pump keys on.
        let sparse_src = self.cache.body_vol_cache.pt_sparse_snapshot_src(nid);
        let field_src = self.cache.body_vol_cache.pt_field_snapshot_src(nid);
        let display = sparse_src.or(field_src);

        // ── Arm 1/2: display readback (default; only when no override fights the display res) ──
        if let Some((_hash, res, gmin, gmax)) = display {
            let use_display = res_override.map_or(true, |r| r == res);
            if use_display {
                if let (Some(g), Some(m)) = (self.gpu.as_ref(), self.cache.sdf_mesher.as_ref()) {
                    let src = if sparse_src.is_some() {
                        g.sdf_brick_atlases
                            .get(&nid)
                            .filter(|a| a.body_nid == Some(nid) && a.res == res)
                            .map(|atlas| SdfDisplayReadbackSrc::SparseAtlas {
                                atlas, res, gmin, gmax, nid, lod_level: 0,
                            })
                    } else {
                        g.sdf_body_mesh
                            .get(&nid)
                            .filter(|s| display_slot_snapshot_ok(s, res))
                            .map(SdfDisplayReadbackSrc::DisplaySlot)
                    };
                    if let Some(src) = src {
                        let pending = dispatch_sdf_display_readback(&g.device, &g.queue, m, src);
                        g.device.poll(wgpu::PollType::Wait).expect("GPU device polling failed");
                        let (verts, indices) = pending.take();
                        if !indices.is_empty() {
                            return Some((verts, indices, SdfMaterializeArm::DisplayReadback));
                        }
                        // Empty display mesh (e.g. mid-rebuild) → fall through to the grid arm.
                    }
                }
            }
        }

        // ── Arm 3: grid DC at the requested (or display, or default) res ──────────────────────
        let res = res_override
            .or(display.map(|(_, r, ..)| r))
            .unwrap_or(128)
            .clamp(16, 1024);
        // The GPU path needs the raw grid; bake it once, then prefer the GPU QEF-DC shader.
        if let (Some(g), Some(m)) = (self.gpu.as_ref(), self.cache.sdf_mesher.as_ref()) {
            use crate::bake_utils::{bake_body_grid_field, collect_body_prims_recursive,
                                    compute_body_bake_bounds, pad_grid_bounds};
            let sg = &self.scene.scene_graph;
            let (mflags, mcenter, bevel) = match sg.get(nid).map(|n| &n.payload) {
                Some(NodePayload::SdfBody { mirror_flags, mirror_center, bevel_radius, .. }) => {
                    (*mirror_flags, *mirror_center, *bevel_radius)
                }
                _ => (0u8, Vec3::ZERO, 0.0f32),
            };
            let prims = collect_body_prims_recursive(sg, nid);
            let (tmin, tmax) = compute_body_bake_bounds(&prims, mflags, mcenter, bevel);
            let (gmin, gmax) = pad_grid_bounds(tmin, tmax);
            let field = core_math::field_eval::BodyField::from_graph(sg, nid);
            let grid = bake_body_grid_field(&field, res, gmin, gmax);
            let (verts, indices) = m.mesh_readback(&g.device, &g.queue, &grid, res, gmin, gmax);
            if !indices.is_empty() {
                return Some((verts, indices, SdfMaterializeArm::GpuGridDc));
            }
            return None;
        }
        // No GPU/mesher → CPU chunked_dc oracle (the parity fallback).
        let (mesh, _gmin, _gmax) = cpu_oracle_mesh(&self.scene.scene_graph, nid, res);
        if mesh.quads.is_empty() {
            return None;
        }
        let (verts, indices) = mesh_to_verts8(&mesh);
        Some((verts, indices, SdfMaterializeArm::CpuOracle))
    }

    /// Detail Weighted LO (PoC): Field importance decimate after DC.
    /// Melts flats first; keeps soft curves / silhouette under a tri budget.
    /// Live path — no-op when flag off, Alt compare, or interactive draft.
    pub(crate) fn apply_detail_weighted_lo_mesh(
        &self,
        nid: NodeId,
        verts: Vec<[f32; 8]>,
        indices: Vec<u32>,
    ) -> (Vec<[f32; 8]>, Vec<u32>) {
        self.apply_detail_weighted_lo_mesh_inner(nid, verts, indices, /*quality_bake=*/ false)
    }

    /// Quality bake (Preprocess / export / settle): soft-K protect + even equalize + smooth normals.
    /// Always runs full offline settings (not interactive draft).
    pub(crate) fn apply_export_quality_lo_mesh(
        &self,
        nid: NodeId,
        verts: Vec<[f32; 8]>,
        indices: Vec<u32>,
    ) -> (Vec<[f32; 8]>, Vec<u32>) {
        self.apply_detail_weighted_lo_mesh_inner(nid, verts, indices, /*quality_bake=*/ true)
    }

    /// Shared Master-first fold. `quality_bake` = Preprocess/export/settle offline settings.
    fn apply_detail_weighted_lo_mesh_inner(
        &self,
        nid: NodeId,
        verts: Vec<[f32; 8]>,
        indices: Vec<u32>,
        export_quality: bool,
    ) -> (Vec<[f32; 8]>, Vec<u32>) {
        if !export_quality {
            if !self.cache.body_vol_cache.use_detail_weighted_lo {
                return (verts, indices);
            }
            // Hold-compare / live draft: pure DC (skip fold for speed / A/B raw).
            if self.cache.body_vol_cache.detail_lo_is_compare()
                || self.cache.body_vol_cache.detail_lo_is_drafting()
            {
                return (verts, indices);
            }
        }
        if indices.len() < 12 || verts.is_empty() {
            return (verts, indices);
        }
        use core_math::display_mesh_ops::{
            apply_display_mesh_op_ctx, mesh_avg_edge_length, DecimateMode, DecimateParams,
            DisplayMesh, DisplayMeshFoldCtx, DisplayMeshOp,
        };

        let field = core_math::field_eval::BodyField::from_graph(&self.scene.scene_graph, nid);
        // Export uses at least mid strength so soft-K / cylinder equalize is visible.
        let s = if export_quality {
            self.cache
                .body_vol_cache
                .detail_lo_strength
                .clamp(1.0, 2.5)
                .max(1.15)
        } else {
            self.cache.body_vol_cache.detail_lo_strength.clamp(0.5, 2.5)
        };
        let preset = self.cache.body_vol_cache.detail_lo_preset.min(2);
        let mesh0 = DisplayMesh::from_raster(&verts, &indices);
        let raw_tris = mesh0.triangle_count().max(1);
        // Target-tris mode: derive keep ratio from budget (honest LO comparison).
        // Export without Detail LO: mild keep (light equalize, keep density for bake).
        let ratio = if self.cache.body_vol_cache.detail_lo_target_tris > 0
            && (export_quality || self.cache.body_vol_cache.use_detail_weighted_lo)
        {
            (self.cache.body_vol_cache.detail_lo_target_tris as f32 / raw_tris as f32)
                .clamp(0.05, 0.95)
        } else if export_quality && !self.cache.body_vol_cache.use_detail_weighted_lo {
            // Offline equalize only — do not crush raw DC density.
            0.88
        } else {
            self.cache.body_vol_cache.detail_lo_keep_ratio
        };
        // Live: already at/under budget — skip fold (preserve full extract density).
        // Export: still run relax + normals even when keep≈1 (topology/normal polish).
        let skip_decimate = ratio >= 0.98;
        if skip_decimate && !export_quality {
            return (verts, indices);
        }
        // Preset bias (still pure mesh/field — no analytical rims).
        // 0 Balanced · 1 Soft walls (field/H) · 2 Sharp (crease + silhouette)
        // Export always biases soft walls — cylinders + blend-K read as even bevel bands.
        let (crease_mul, bound_mul, field_mul, planar_extra, crease_ang_mul) = if export_quality {
            (0.92, 1.20, 1.55, 0.48, 1.08)
        } else {
            match preset {
                0 => (1.0, 1.0, 1.0, 0.0, 1.0),
                1 => (0.95, 1.15, 1.40, 0.40, 1.05), // protect soft bands + outline
                _ => (1.40, 1.60, 0.90, 0.12, 0.82), // sharper dihedral + silhouette
            }
        };
        // Soft-K / bevel feature size → widen SDF curvature sample so fillets aren't melted.
        let feat = crate::bake_utils::body_soft_feature_size(&self.scene.scene_graph, nid);
        let avg_edge = mesh_avg_edge_length(&mesh0);
        let field_radius = {
            let base_r = (0.55 + 0.25 * s).clamp(0.35, 1.5);
            let mut r = (base_r * field_mul).clamp(0.35, 2.0);
            if feat > 1e-4 && avg_edge > 1e-6 {
                // Wider sample across soft walls for export (~half feature thickness).
                let feat_scale = if export_quality { 0.55 } else { 0.45 };
                let feat_r = (feat * feat_scale / avg_edge).clamp(0.5, 2.8);
                r = r.max(feat_r);
            }
            r.min(2.8)
        };
        // AZRA edge-shape: min-edge + curve-angle → even rings on cylinders / soft-K bands.
        // Export always on; live follows UI toggle.
        let edge_shape_on = export_quality || self.cache.body_vol_cache.detail_lo_edge_shape;
        let (edge_shape_strength, flat_starve, planar_boost) = if edge_shape_on {
            (
                (1.35 * s).clamp(0.8, 3.2),
                (1.55 * s).clamp(1.0, 3.5),
                if export_quality { 0.65f32 } else { 0.55f32 },
            )
        } else {
            (0.0, 0.0, 0.0)
        };
        // Auto min-edge base: soft-K / bevel thickness, else a few avg DC edges.
        // Export aims ~3–4 edges across soft feature (bevel strip density).
        let min_edge_len = if edge_shape_on {
            let scale = self
                .cache
                .body_vol_cache
                .detail_lo_min_edge_scale
                .clamp(0.35, 3.5);
            let base = if feat > 1e-4 {
                feat * if export_quality { 0.32 } else { 0.40 }
            } else if export_quality {
                // Cylinder / hard prim: target ~2 avg DC edges → even spacing
                avg_edge * 1.85
            } else {
                avg_edge * 2.0
            };
            (base * scale).max(avg_edge * 0.75)
        } else {
            0.0
        };
        let curve_angle_deg = if edge_shape_on {
            let base = self
                .cache
                .body_vol_cache
                .detail_lo_curve_angle
                .clamp(4.0, 24.0);
            // Export: slightly lower angle keep → denser rings on round sections
            if export_quality {
                (base * 0.85).clamp(5.0, 18.0)
            } else {
                base
            }
        } else {
            0.0
        };
        // Interactive scrub: pure Master (no SDF) — fast. Export always quality.
        let interactive = !export_quality && self.cache.body_vol_cache.detail_lo_interactive;
        let use_len_field = edge_shape_on
            && (export_quality || self.cache.body_vol_cache.detail_lo_edge_length_field)
            && !interactive
            && raw_tris <= 120_000;
        let use_flat_relax = edge_shape_on
            && (export_quality || self.cache.body_vol_cache.detail_lo_flat_relax)
            && !interactive;
        // Master is the quality workhorse. Stronger |H| on export for soft-K bands.
        let quality_field = if interactive {
            0.0
        } else if edge_shape_on {
            let base = if export_quality { 0.75 } else { 0.55 };
            (base * s * field_mul).clamp(0.25, 1.55)
        } else {
            (0.35 * s).clamp(0.15, 0.9)
        };
        let params = DecimateParams {
            ratio,
            mode: DecimateMode::Master,
            crease_angle_deg: ((30.0 / s.sqrt()) * crease_ang_mul).clamp(12.0, 45.0),
            crease_strength: (1.55 * s * crease_mul).clamp(0.6, 4.0),
            boundary_strength: (1.1 + 0.4 * s * bound_mul).clamp(0.5, 4.0),
            field_strength: quality_field,
            field_radius,
            max_error: 0.0,
            // Stronger planar melt than old Field path — Master thrives on clean flats.
            planar_bias: (2.8 + 0.85 * s + planar_extra + planar_boost * 0.5).clamp(1.8, 4.0),
            // No full edge-shape Dijkstra on Master path (edge_shape_strength ignored by Master).
            edge_shape_strength: 0.0,
            flat_starve: if interactive {
                flat_starve * 0.45
            } else {
                // Milder starve so Master keeps even rings on cylinders.
                let starve = if export_quality { 0.62 } else { 0.72 };
                (flat_starve * starve).clamp(0.5, 2.8)
            },
            soft_feature_world: feat,
            min_edge_len,
            curve_angle_deg: if interactive {
                0.0
            } else {
                curve_angle_deg
            },
            use_edge_length_field: use_len_field,
            flat_relax: use_flat_relax,
        };
        let ctx = DisplayMeshFoldCtx {
            field: if interactive || quality_field < 1e-6 {
                None
            } else {
                Some(&field)
            },
        };
        // ── Master-first multi-pass (Decimation Master style) ──
        //  1) Planar — melt coplanar bulk
        //  2) Master bulk — most of the budget with mesh importance
        //  3) Master final — target ratio + light SDF |H| + min-edge / angle
        //  4) Flat relax + smooth normals (export always; live quality when toggled)
        let mut mesh = mesh0;
        if !skip_decimate && mesh.triangle_count() > 900 {
            // Keep more tris than final ratio, but never invert clamp bounds when ratio > cap
            // (f32::clamp panics if min > max — crashed app on high Detail LO ratios).
            let planar_cap = 0.93f32;
            let planar_ratio = (params.ratio * 1.65)
                .min(planar_cap)
                .max(params.ratio.min(planar_cap));
            let planar = DecimateParams {
                ratio: planar_ratio,
                mode: DecimateMode::Planar,
                crease_angle_deg: (params.crease_angle_deg + 10.0).min(55.0),
                planar_bias: 0.0,
                field_strength: 0.0,
                edge_shape_strength: 0.0,
                flat_starve: 0.0,
                min_edge_len: params.min_edge_len,
                curve_angle_deg: 0.0,
                use_edge_length_field: false,
                ..params
            };
            mesh = apply_display_mesh_op_ctx(
                &DisplayMeshOp::Decimate { params: planar },
                mesh,
                &ctx,
            );
        }
        // Bulk Master (no field) — this is where Decimation Master wins.
        if !skip_decimate && !interactive {
            let bulk_cap = 0.90f32;
            let bulk_ratio = (params.ratio * 1.18)
                .min(bulk_cap)
                .max(params.ratio.min(bulk_cap));
            if bulk_ratio > params.ratio + 0.025 && mesh.triangle_count() > 600 {
                let bulk = DecimateParams {
                    ratio: bulk_ratio,
                    mode: DecimateMode::Master,
                    field_strength: 0.0, // pure mesh importance bulk
                    planar_bias: (params.planar_bias + 0.25).min(4.0),
                    min_edge_len: params.min_edge_len,
                    curve_angle_deg: params.curve_angle_deg * 0.85,
                    use_edge_length_field: false,
                    ..params
                };
                mesh = apply_display_mesh_op_ctx(
                    &DisplayMeshOp::Decimate { params: bulk },
                    mesh,
                    &DisplayMeshFoldCtx { field: None },
                );
            }
        }
        // Final Master to exact budget (+ light SDF assist when quality).
        let mut out = if skip_decimate {
            mesh
        } else {
            apply_display_mesh_op_ctx(
                &DisplayMeshOp::Decimate { params },
                mesh,
                &ctx,
            )
        };
        // Flat relax + SDF snap — even spacing on flats/cylinders; protect soft-K importance.
        // Export: 2 iters for nicer evenness (one-shot offline).
        if use_flat_relax && out.triangle_count() > 32 {
            use core_math::display_mesh_ops::{
                mesh_flat_relax_sdf_snap, mesh_vertex_importance,
            };
            let protect = mesh_vertex_importance(&out);
            let protect_u: Vec<f32> = protect.iter().map(|p| p * 4.0).collect();
            let iters = if export_quality { 2 } else { 1 };
            out = mesh_flat_relax_sdf_snap(
                out,
                &field,
                Some(&protect_u),
                iters,
                if export_quality { 0.42 } else { 0.38 },
                if export_quality { 3 } else { 2 },
            );
        }
        // Analytical feature project: crease chains → fitted line, flats → plane, + SDF pull.
        // Quality only. Cap cost on huge meshes.
        // Export: only when user flag on AND soft feature present or sharp preset — avoid
        // over-straightening pure cylinders when flag is off.
        let use_analytical = !interactive
            && self.cache.body_vol_cache.detail_lo_analytical_project
            && out.triangle_count() > 4
            && out.triangle_count() <= 40_000;
        if use_analytical {
            use core_math::display_mesh_ops::{
                mesh_analytical_feature_project, mesh_avg_edge_length,
            };
            let avg_e = mesh_avg_edge_length(&out);
            let max_move = avg_e.max(1e-4) * if export_quality { 1.6 } else { 2.0 };
            let crease_deg = if export_quality { 48.0 } else { 52.0 };
            out = mesh_analytical_feature_project(
                out,
                Some(&field),
                max_move,
                crease_deg,
            );
        }
        // Export always rebuilds smooth normals (bake/LOD readiness). Live follows toggle.
        let do_smooth_n = export_quality
            || (self.cache.body_vol_cache.detail_lo_smooth_normals && !interactive);
        if do_smooth_n {
            out = apply_display_mesh_op_ctx(
                &DisplayMeshOp::SmoothNormals,
                out,
                &ctx,
            );
        }
        let _ = edge_shape_strength; // strength still feeds soft-K field assist via field_mul
        if export_quality {
            eprintln!(
                "[ExportQualityLO] body={nid:?} raw_tris={raw_tris} out_tris={} keep={ratio:.2} \
                 soft_feat={feat:.4} min_edge={min_edge_len:.4} curve_ang={curve_angle_deg:.1} \
                 field={quality_field:.2} relax={use_flat_relax}",
                out.triangle_count()
            );
        }
        out.to_raster()
    }

    /// Display fold after DC: Detail Weighted LO (if on) then user mesh-modifier stack.
    /// Live path respects draft/compare skips.
    pub(crate) fn fold_body_display_after_dc(
        &self,
        nid: NodeId,
        verts: Vec<[f32; 8]>,
        indices: Vec<u32>,
    ) -> (Vec<[f32; 8]>, Vec<u32>) {
        let (verts, indices) = self.apply_detail_weighted_lo_mesh(nid, verts, indices);
        self.apply_body_mesh_modifiers(nid, verts, indices)
    }

    /// Quality bake fold (Preprocess / export / settle): offline soft-K + even spacing + normals.
    pub(crate) fn fold_body_display_for_export(
        &self,
        nid: NodeId,
        verts: Vec<[f32; 8]>,
        indices: Vec<u32>,
    ) -> (Vec<[f32; 8]>, Vec<u32>) {
        let (verts, indices) = self.apply_export_quality_lo_mesh(nid, verts, indices);
        self.apply_body_mesh_modifiers(nid, verts, indices)
    }

    /// Path B (MODIFIER_SYSTEM_PLAN): CPU-fold the body's display mesh stack after DC/readback.
    /// No-op when stack empty. Does not re-bake the field.
    pub(crate) fn apply_body_mesh_modifiers(
        &self,
        nid: NodeId,
        verts: Vec<[f32; 8]>,
        indices: Vec<u32>,
    ) -> (Vec<[f32; 8]>, Vec<u32>) {
        use core_math::display_mesh_ops::{
            compile_display_mesh_stack, DecimateMode, DisplayMesh,
            DisplayMeshFoldCtx, DisplayMeshOp,
        };
        let records = match self.scene.scene_graph.get(nid).map(|n| &n.payload) {
            Some(NodePayload::SdfBody { mesh_modifiers, .. }) if !mesh_modifiers.is_empty() => {
                mesh_modifiers.clone()
            }
            _ => return (verts, indices),
        };
        let ops = compile_display_mesh_stack(&records);
        if ops.is_empty() {
            return (verts, indices);
        }
        // Field-mode Decimate needs BodyField (body-local, same frame as DC mesh).
        let needs_field = ops.iter().any(|op| {
            matches!(
                op,
                DisplayMeshOp::Decimate {
                    params: core_math::display_mesh_ops::DecimateParams {
                        mode: DecimateMode::Field,
                        ..
                    },
                }
            )
        });
        let field = if needs_field {
            Some(core_math::field_eval::BodyField::from_graph(
                &self.scene.scene_graph,
                nid,
            ))
        } else {
            None
        };
        let ctx = DisplayMeshFoldCtx {
            field: field.as_ref(),
        };
        let mesh = DisplayMesh::from_raster(&verts, &indices);
        // Reuse the untouched PREFIX of the stack instead of re-folding every op on every edit.
        // Measured 28-42x cheaper per edit at depth 4-12 (bench_prefix_cache_by_depth).
        //
        // The cache is invalidated on `mesh_mod_base_key`, not just on the ops: the cached
        // prefixes were folded from a specific BASE mesh, so a rebake at a new resolution (or any
        // field change) must drop them. Without that check the fold would reuse prefixes built
        // from geometry that no longer exists — correct-looking, silently wrong.
        let base_key = self.mesh_mod_base_key(nid);
        let mut caches = self.cache.mesh_mod_prefix_cache.borrow_mut();
        let entry = caches.entry(nid).or_insert_with(|| (base_key, Default::default()));
        if entry.0 != base_key {
            *entry = (base_key, Default::default());
        }
        let out = core_math::display_mesh_ops::fold_display_mesh_cached(
            &ops, mesh, &ctx, &mut entry.1);
        out.to_raster()
    }

    /// Key for the **raw** DC base mesh (pre–Detail-LO / pre–mesh-mod fold).
    ///
    /// Includes field + effective extract resolution (oversample is baked into res).
    /// Does **not** include keep-ratio / strength — those only affect the display fold
    /// so slider scrub can re-fold without a full rebake.
    pub(crate) fn mesh_mod_base_key(&self, nid: NodeId) -> u64 {
        use std::hash::{Hash, Hasher};
        let field_hash = crate::bake_utils::body_content_id(
            &self.scene.scene_graph,
            nid,
            self.cache.body_vol_cache.lab_operand_gen,
        )
        .map_or(0, |id| id.0);
        let res = self.cache.body_vol_cache.effective_resolution(nid);
        let mut h = std::collections::hash_map::DefaultHasher::new();
        field_hash.hash(&mut h);
        res.hash(&mut h);
        // Whether extract was oversampled (res already encodes it when flag on, but keep
        // the flag so toggle off at same numeric res still invalidates base).
        self.cache.body_vol_cache.use_detail_weighted_lo.hash(&mut h);
        self.cache.body_vol_cache.detail_lo_adaptive_dc.hash(&mut h);
        ((self.cache.body_vol_cache.detail_lo_oversample * 100.0) as u32).hash(&mut h);
        h.finish()
    }

    /// Stamp for the **displayed** mesh: raw base + Detail LO fold params + mesh-mod stack.
    pub(crate) fn mesh_mod_display_stamp_for(
        &self,
        nid: NodeId,
        records: &[core_math::display_mesh_ops::DisplayMeshModRecord],
    ) -> u64 {
        use core_math::display_mesh_ops::hash_display_mesh_stack;
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        self.mesh_mod_base_key(nid).hash(&mut h);
        // Fold-only Detail LO params — changing these must re-fold without rebake.
        self.cache.body_vol_cache.use_detail_weighted_lo.hash(&mut h);
        self.cache.body_vol_cache.detail_lo_adaptive_dc.hash(&mut h);
        ((self.cache.body_vol_cache.detail_lo_keep_ratio * 1000.0) as u32).hash(&mut h);
        ((self.cache.body_vol_cache.detail_lo_strength * 100.0) as u32).hash(&mut h);
        self.cache.body_vol_cache.detail_lo_target_tris.hash(&mut h);
        self.cache.body_vol_cache.detail_lo_preset.hash(&mut h);
        self.cache.body_vol_cache.detail_lo_smooth_normals.hash(&mut h);
        self.cache.body_vol_cache.detail_lo_heat_wire.hash(&mut h);
        self.cache.body_vol_cache.detail_lo_edge_shape.hash(&mut h);
        ((self.cache.body_vol_cache.detail_lo_min_edge_scale * 100.0) as u32).hash(&mut h);
        ((self.cache.body_vol_cache.detail_lo_curve_angle * 10.0) as u32).hash(&mut h);
        self.cache
            .body_vol_cache
            .detail_lo_edge_length_field
            .hash(&mut h);
        self.cache.body_vol_cache.detail_lo_flat_relax.hash(&mut h);
        self.cache
            .body_vol_cache
            .detail_lo_analytical_project
            .hash(&mut h);
        self.cache.body_vol_cache.detail_lo_hold_compare.hash(&mut h);
        self.cache.body_vol_cache.detail_lo_live_draft.hash(&mut h);
        // Interactive scrub vs quality fold must be different stamps.
        self.cache.body_vol_cache.detail_lo_interactive.hash(&mut h);
        hash_display_mesh_stack(records).hash(&mut h);
        h.finish()
    }

    /// Active body display mesh stats (post-fold when Detail LO on).
    pub(crate) fn active_body_mesh_stats(&self) -> Option<(u32, u32)> {
        let nid = self
            .scene
            .scene_graph
            .top_level_body(self.scene.active_body_nid);
        let cid = match self.scene.scene_graph.get(nid).map(|n| &n.payload) {
            Some(NodePayload::SdfBody {
                mesh_cache_id: Some(c),
                ..
            }) => *c,
            _ => return None,
        };
        let entry = self.cache.mesh_cache.get(cid)?;
        let tris = entry.triangle_count;
        let verts = if let Some((v, _)) = self.cache.mesh_cache.chunk_mesh_data(cid) {
            v.len() as u32
        } else {
            entry.cpu_mesh.vertex_count() as u32
        };
        Some((verts, tris))
    }

    /// Raw (pre-fold) extract tri count for the active body, if `mesh_mod_base` is warm.
    pub(crate) fn active_body_raw_extract_tris(&self) -> Option<u32> {
        let nid = self
            .scene
            .scene_graph
            .top_level_body(self.scene.active_body_nid);
        let base_key = self.mesh_mod_base_key(nid);
        self.cache
            .mesh_mod_base
            .get(&nid)
            .filter(|(_, i, bk)| *bk == base_key && !i.is_empty())
            .map(|(_, i, _)| (i.len() / 3) as u32)
    }

    /// **Quick Normal Bake** — high→low normal transfer preview on the LO mesh.
    ///
    /// High source (priority): raw oversampled extract → DC `cpu_mesh` → grid rematerialize.  
    /// Low target: current folded Detail LO display mesh.  
    /// Rays cast from each LO vertex along ±normal hit the high mesh; high normals are written
    /// onto the LO verts so viewport shading shows the **bake**, not a dense wire overlay.
    /// Orbit / pan / zoom dismisses and restores LO fold normals.
    pub(crate) fn trigger_quick_normal(&mut self) {
        use crate::normal_bake::{apply_baked_normals_to_raster, bake_normals_high_to_low};
        use glam::Vec3;

        let nid = self
            .scene
            .scene_graph
            .top_level_body(self.scene.active_body_nid);
        let Some(cid) = (match self.scene.scene_graph.get(nid).map(|n| &n.payload) {
            Some(NodePayload::SdfBody {
                mesh_cache_id: Some(c),
                ..
            }) => Some(*c),
            _ => None,
        }) else {
            return;
        };

        // --- HIGH mesh (detail source) ---
        let mut source: &'static str = "raw";
        let mut res_label: u32 = self.cache.body_vol_cache.effective_resolution(nid);
        let mut high: Option<(Vec<[f32; 8]>, Vec<u32>)> = None;
        {
            let base_key = self.mesh_mod_base_key(nid);
            if let Some((v, i, bk)) = self.cache.mesh_mod_base.get(&nid) {
                if *bk == base_key && !v.is_empty() && !i.is_empty() {
                    high = Some((v.clone(), i.clone()));
                    source = "raw";
                }
            }
        }
        if high.is_none() {
            if let Some(cached) = self.cache.mesh_cache.get(cid) {
                if !cached.cpu_mesh.quads.is_empty() && !cached.cpu_mesh.vertices.is_empty() {
                    let (v, i) = cached.cpu_mesh.to_raster_data();
                    if !i.is_empty() {
                        high = Some((v, i));
                        source = "dc";
                    }
                }
            }
        }
        if high.is_none() {
            let res = res_label.max(64).min(256);
            if let Some((v, i, _)) = self.materialize_sdf_body_mesh_raw(nid, Some(res)) {
                if !i.is_empty() {
                    high = Some((v, i));
                    source = "grid";
                    res_label = res;
                }
            }
        }
        let Some((high_v, high_i)) = high else {
            return;
        };

        // --- LOW mesh (LO / display target) ---
        let low = self
            .cache
            .mesh_cache
            .pt_source_data(cid)
            .filter(|(v, i)| !v.is_empty() && !i.is_empty());
        let Some((mut low_v, low_i)) = low else {
            return;
        };
        if low_v.len() < 3 || low_i.len() < 3 {
            return;
        }

        // Unpack
        let high_pos: Vec<Vec3> = high_v
            .iter()
            .map(|v| Vec3::new(v[0], v[1], v[2]))
            .collect();
        let high_nrm: Vec<Vec3> = high_v
            .iter()
            .map(|v| Vec3::new(v[3], v[4], v[5]).normalize_or_zero())
            .collect();
        let low_pos: Vec<Vec3> = low_v
            .iter()
            .map(|v| Vec3::new(v[0], v[1], v[2]))
            .collect();
        let low_nrm: Vec<Vec3> = low_v
            .iter()
            .map(|v| Vec3::new(v[3], v[4], v[5]).normalize_or_zero())
            .collect();

        // Ray distance from body soft feature / avg edge.
        let feat = crate::bake_utils::body_soft_feature_size(&self.scene.scene_graph, nid);
        let mut avg_e = 0.0f32;
        let mut ec = 0u32;
        for t in low_i.chunks_exact(3).take(800) {
            let a = low_pos[t[0] as usize];
            let b = low_pos[t[1] as usize];
            let c = low_pos[t[2] as usize];
            avg_e += (a - b).length() + (b - c).length() + (c - a).length();
            ec += 3;
        }
        avg_e = (avg_e / ec.max(1) as f32).max(1e-4);
        let max_dist = if feat > 1e-4 {
            (feat * 2.5).max(avg_e * 4.0)
        } else {
            avg_e * 6.0
        };

        let baked = bake_normals_high_to_low(
            &high_pos, &high_i, &high_nrm, &low_pos, &low_nrm, max_dist,
        );
        let mut hits = 0u32;
        for i in 0..baked.len().min(low_nrm.len()) {
            if (baked[i] - low_nrm[i]).length_squared() > 1e-6 {
                hits += 1;
            }
        }
        let hit_pct = if low_nrm.is_empty() {
            0
        } else {
            ((hits as f32 / low_nrm.len() as f32) * 100.0).round() as u32
        };

        apply_baked_normals_to_raster(&mut low_v, &baked);
        let tri = (low_i.len() / 3) as u32;
        self.cache
            .mesh_cache
            .update_editable_mesh(cid, (low_v, low_i), tri);
        // Force CachedCpu so Object Mode draws the baked normals (not raw Resident).
        // Unique tag so ensure won't treat bake preview as plain LO fold stamp.
        const BAKE_STAMP_TAG: u64 = 0x4E4F524D_BA01;
        let stamp = match self.scene.scene_graph.get(nid).map(|n| &n.payload) {
            Some(NodePayload::SdfBody { mesh_modifiers, .. }) => {
                self.mesh_mod_display_stamp_for(nid, mesh_modifiers) ^ BAKE_STAMP_TAG
            }
            _ => BAKE_STAMP_TAG,
        };
        self.cache.mesh_mod_display_stamp.insert(nid, stamp);
        self.wire_local_cache.remove(&cid.0);

        let cam = &self.viewport.camera;
        self.quick_normal_ghost = Some(crate::QuickNormalGhost {
            nid,
            res: res_label,
            cam_orbit: cam.orbit,
            cam_dist: cam.distance,
            cam_target: cam.target,
            grace_frames: 10,
            source,
            low_tris: tri,
            high_tris: (high_i.len() / 3) as u32,
            hit_pct,
        });
        self.overlay_lines_fp = u64::MAX;
        self.mark_dirty(crate::DirtyFlags::RENDER);
    }

    /// Clear normal-bake preview when the camera moves (after grace) — restore LO fold normals.
    pub(crate) fn poll_quick_normal_dismiss(&mut self) {
        let Some(g) = self.quick_normal_ghost.as_mut() else {
            return;
        };
        if g.grace_frames > 0 {
            g.grace_frames -= 1;
            g.cam_orbit = self.viewport.camera.orbit;
            g.cam_dist = self.viewport.camera.distance;
            g.cam_target = self.viewport.camera.target;
            return;
        }
        let cam = &self.viewport.camera;
        let orbit_deg = cam.orbit.angle_between(g.cam_orbit).to_degrees();
        let dist_delta = (cam.distance - g.cam_dist).abs();
        let tgt_delta = (cam.target - g.cam_target).length();
        if orbit_deg > 1.5 || dist_delta > cam.distance.max(0.1) * 0.02 || tgt_delta > 0.02 {
            self.clear_quick_normal_bake();
        }
    }

    /// Restore LO fold normals and drop bake preview state.
    pub(crate) fn clear_quick_normal_bake(&mut self) {
        if self.quick_normal_ghost.is_none() {
            return;
        }
        self.quick_normal_ghost = None;
        // Re-fold Detail LO so normals return to proper LO (not baked high).
        self.cache.body_vol_cache.detail_lo_interactive = false;
        self.cache.body_vol_cache.detail_lo_quality_pending = false;
        self.refresh_detail_lo_displays_for(true);
        self.overlay_lines_fp = u64::MAX;
        self.mark_dirty(crate::DirtyFlags::RENDER);
    }

    /// Per-frame Detail LO runtime: hold-compare + settle-only live draft.
    ///
    /// **Alt is gated hard:** transform GRS, gizmo, create, elongate-sym, etc. already own Alt.
    /// Hold-compare only arms when the viewport is idle (no active tool) so Scale/Grab/Rotate
    /// keep their Alt modifiers.
    pub(crate) fn update_detail_lo_runtime_flags(&mut self, ctx: &eframe::egui::Context) {
        if !self.cache.body_vol_cache.use_detail_weighted_lo {
            if self.cache.body_vol_cache.detail_lo_live_draft
                || self.cache.body_vol_cache.detail_lo_hold_compare
            {
                self.cache.body_vol_cache.detail_lo_live_draft = false;
                self.cache.body_vol_cache.detail_lo_hold_compare = false;
            }
            return;
        }

        // Tools that use Alt as a modifier (must never steal for Detail LO compare).
        let tool_owns_alt = self.tool.current.is_gizmo()
            || self.tool.current.is_transform()
            || self.tool.current.is_bevel()
            || self.tool.current.is_bool_edge_bevel()
            || self.tool.current.is_falloff_adjust()
            || self.tool.current.is_face_push_pull()
            || self.tool.current.is_busy()
            || !matches!(self.tool.current, crate::ActiveTool::Idle);
        let tool_busy = tool_owns_alt;

        let alt = ctx.input(|i| i.modifiers.alt);
        // Hold-compare: Alt ONLY when idle + Detail LO on + not typing in UI.
        // Shortcut: plain Alt (no Shift/Ctrl) so Shift+Alt gizmo chords stay free.
        let pure_alt = ctx.input(|i| {
            i.modifiers.alt && !i.modifiers.shift && !i.modifiers.command && !i.modifiers.ctrl
        });
        let hold = pure_alt
            && alt
            && !ctx.wants_keyboard_input()
            && !tool_owns_alt
            && self.viewport.viewport_rect.contains(
                ctx.input(|i| i.pointer.hover_pos().unwrap_or(egui::Pos2::new(-1.0, -1.0))),
            );

        const SETTLE_MS: u128 = 280;
        let recent_edit = self
            .cache
            .body_vol_cache
            .last_dirty_time
            .map(|t| t.elapsed().as_millis() < SETTLE_MS)
            .unwrap_or(false);
        // Draft = skip Field *fold* only (extract res stays stable — no mid-scale rebake flip).
        let drafting = self.cache.body_vol_cache.detail_lo_settle_only
            && (tool_busy || recent_edit);

        let was_hold = self.cache.body_vol_cache.detail_lo_hold_compare;
        let was_draft = self.cache.body_vol_cache.detail_lo_live_draft;
        self.cache.body_vol_cache.detail_lo_hold_compare = hold;
        // Never clear draft because of hold — hold is independent and idle-only.
        self.cache.body_vol_cache.detail_lo_live_draft = drafting;

        // Hold toggled (idle only): re-fold display raw ↔ Detail LO (full quality, not scrub).
        if was_hold != self.cache.body_vol_cache.detail_lo_hold_compare {
            self.cache.body_vol_cache.detail_lo_interactive = false;
            self.cache.body_vol_cache.detail_lo_quality_pending = false;
            self.refresh_detail_lo_displays_for(true);
        }
        // Left interactive draft → quality fold settle.
        if was_draft && !self.cache.body_vol_cache.detail_lo_live_draft {
            self.cache.body_vol_cache.detail_lo_interactive = false;
            self.cache.body_vol_cache.detail_lo_quality_pending = false;
            self.refresh_detail_lo_displays_for(true);
        }
    }

    /// Raw (pre-fold) triangle mesh for a body: cached base → DC `cpu_mesh` → materialize.
    /// Prefer `cpu_mesh` over `imported_*` so we don't re-decimate an already folded display.
    fn raw_dc_mesh_for_fold(&mut self, nid: NodeId) -> Option<(Vec<[f32; 8]>, Vec<u32>)> {
        let base_key = self.mesh_mod_base_key(nid);
        if let Some((v, i, bk)) = self.cache.mesh_mod_base.get(&nid) {
            if *bk == base_key && !v.is_empty() && !i.is_empty() {
                return Some((v.clone(), i.clone()));
            }
        }
        let cid = match self.scene.scene_graph.get(nid).map(|n| &n.payload) {
            Some(NodePayload::SdfBody {
                mesh_cache_id: Some(c),
                ..
            }) => *c,
            _ => return None,
        };
        // Full Dual Contouring extract (pre–update_editable_mesh fold).
        if let Some(cached) = self.cache.mesh_cache.get(cid) {
            if !cached.cpu_mesh.quads.is_empty() && !cached.cpu_mesh.vertices.is_empty() {
                return Some(cached.cpu_mesh.to_raster_data());
            }
        }
        // Last resort: re-materialize (may hit GPU / CPU oracle).
        self.materialize_sdf_body_mesh_raw(nid, None)
            .map(|(v, i, _)| (v, i))
    }

    /// **Preprocess LO base** — capture raw oversampled extract into `mesh_mod_base`, then
    /// quality-fold once. After this, keep/strength/edge-shape/min-edge/ℓ/relax sliders only
    /// re-fold (no rematerialize / no field rebake).
    ///
    /// `active_only`: true = active top-level body only (fast); false = all top-level SDF bodies.
    /// Returns `(bodies_ok, active_raw_tris, active_display_tris)`.
    pub(crate) fn preprocess_detail_lo_base(&mut self, active_only: bool) -> (u32, u32, u32) {
        if !self.cache.body_vol_cache.use_detail_weighted_lo {
            // Enable without mass-dirty: we materialize immediately below.
            // Full toggle path (set_detail_weighted_lo) marks all bodies dirty → rebake spam.
            self.cache.body_vol_cache.use_detail_weighted_lo = true;
            self.cache
                .body_vol_cache
                .refresh_detail_lo_feature_floors(&self.scene.scene_graph);
        }
        // Quality fold — not draft / Alt raw / scrub interactive.
        self.cache.body_vol_cache.detail_lo_live_draft = false;
        self.cache.body_vol_cache.detail_lo_hold_compare = false;
        self.cache.body_vol_cache.detail_lo_interactive = false;
        self.cache.body_vol_cache.detail_lo_quality_pending = false;
        self.cache.body_vol_cache.detail_lo_last_scrub = None;

        let active = self
            .scene
            .scene_graph
            .top_level_body(self.scene.active_body_nid);
        let nids: Vec<NodeId> = if active_only {
            match self.scene.scene_graph.get(active).map(|n| &n.payload) {
                Some(NodePayload::SdfBody { .. }) => vec![active],
                _ => Vec::new(),
            }
        } else {
            self.scene
                .scene_graph
                .sdf_bodies()
                .filter(|(nid, _)| !self.scene.scene_graph.is_sub_body(*nid))
                .map(|(nid, _)| nid)
                .collect()
        };

        let mut bodies_ok = 0u32;
        let mut active_raw = 0u32;
        let mut active_disp = 0u32;

        for nid in nids {
            let Some(cid) = (match self.scene.scene_graph.get(nid).map(|n| &n.payload) {
                Some(NodePayload::SdfBody {
                    mesh_cache_id: Some(c),
                    ..
                }) => Some(*c),
                Some(NodePayload::SdfBody {
                    mesh_cache_id: None,
                    ..
                }) => {
                    let cid = self.cache.mesh_cache.alloc_id();
                    if let Some(n) = self.scene.scene_graph.get_mut(nid) {
                        if let NodePayload::SdfBody { mesh_cache_id, .. } = &mut n.payload {
                            *mesh_cache_id = Some(cid);
                        }
                    }
                    Some(cid)
                }
                _ => None,
            }) else {
                continue;
            };

            // Force extract at Detail LO effective res (oversample + floor).
            let res = self.cache.body_vol_cache.effective_resolution(nid);
            let raw = self
                .materialize_sdf_body_mesh_raw(nid, Some(res))
                .map(|(v, i, _)| (v, i))
                .or_else(|| self.raw_dc_mesh_for_fold(nid));
            let Some((raw_v, raw_i)) = raw else {
                continue;
            };
            if raw_i.is_empty() {
                continue;
            }

            let base_key = self.mesh_mod_base_key(nid);
            let raw_tris = (raw_i.len() / 3) as u32;
            self.cache
                .mesh_mod_base
                .insert(nid, (raw_v.clone(), raw_i.clone(), base_key));

            // Quality bake path (not live draft fold) — soft-K / even cylinder topology.
            let (fv, fi) = self.fold_body_display_for_export(nid, raw_v, raw_i);
            if fi.is_empty() {
                continue;
            }
            let disp_tris = (fi.len() / 3) as u32;
            self.cache
                .mesh_cache
                .update_editable_mesh(cid, (fv, fi), disp_tris);
            let stamp = match self.scene.scene_graph.get(nid).map(|n| &n.payload) {
                Some(NodePayload::SdfBody { mesh_modifiers, .. }) => {
                    self.mesh_mod_display_stamp_for(nid, mesh_modifiers)
                }
                _ => 0,
            };
            self.cache.mesh_mod_display_stamp.insert(nid, stamp);
            self.wire_local_cache.remove(&cid.0);
            bodies_ok += 1;
            if nid == active {
                active_raw = raw_tris;
                active_disp = disp_tris;
            }
            eprintln!(
                "[DetailLO] bake body={nid:?} raw_tris={raw_tris} → display_tris={disp_tris}"
            );
        }

        self.cache.body_vol_cache.detail_lo_preprocessed = bodies_ok > 0;
        if bodies_ok > 0 {
            self.overlay_lines_fp = u64::MAX;
            self.mark_dirty(crate::DirtyFlags::RENDER);
        }
        (bodies_ok, active_raw, active_disp)
    }

    /// True when active body has a warm `mesh_mod_base` matching current extract key.
    pub(crate) fn detail_lo_base_ready_for_active(&self) -> bool {
        if !self.cache.body_vol_cache.use_detail_weighted_lo {
            return false;
        }
        let nid = self
            .scene
            .scene_graph
            .top_level_body(self.scene.active_body_nid);
        let base_key = self.mesh_mod_base_key(nid);
        self.cache
            .mesh_mod_base
            .get(&nid)
            .map(|(_, i, bk)| *bk == base_key && !i.is_empty())
            .unwrap_or(false)
    }

    /// Slider scrub: fast interactive fold on **active body only**, then schedule quality settle.
    pub(crate) fn schedule_detail_lo_scrub_refresh(&mut self) {
        self.cache.body_vol_cache.detail_lo_interactive = true;
        self.cache.body_vol_cache.detail_lo_quality_pending = true;
        self.cache.body_vol_cache.detail_lo_last_scrub = Some(std::time::Instant::now());
        self.refresh_detail_lo_displays_for(true);
    }

    /// After scrub stops (~180ms): full quality fold (Field + edge-shape + relax) on active body.
    ///
    /// Live log (2026-07-21): this was unbracketed in `poll_background_tasks` and showed up as
    /// `rest(top)=450–2100ms` freezes. Hard-caps huge raw extracts; logs `[SPIKE] detail_lo_quality_settle`.
    pub(crate) fn poll_detail_lo_quality_settle(&mut self) {
        if !self.cache.body_vol_cache.detail_lo_quality_pending {
            return;
        }
        let Some(t0) = self.cache.body_vol_cache.detail_lo_last_scrub else {
            self.cache.body_vol_cache.detail_lo_quality_pending = false;
            return;
        };
        if t0.elapsed().as_millis() < 280 {
            return;
        }
        if self.prev_frame_wall_ms > 90.0 {
            return;
        }
        if self.tool.current.is_gizmo()
            || self.tool.current.is_transform()
            || self.tool.current.is_bevel()
            || self.tool.current.is_bool_edge_bevel()
        {
            return;
        }

        let nid = self
            .scene
            .scene_graph
            .top_level_body(self.scene.active_body_nid);
        let raw_tris = self
            .cache
            .mesh_mod_base
            .get(&nid)
            .map(|(_, i, _)| i.len() / 3)
            .unwrap_or(0);
        // Prefer Preprocess for huge extracts (avoids multi-second settle thrash).
        if raw_tris > 100_000 {
            self.cache.body_vol_cache.detail_lo_quality_pending = false;
            self.cache.body_vol_cache.detail_lo_last_scrub = None;
            eprintln!(
                "[DetailLO] quality settle skipped (raw_tris={} > 100k) — click Preprocess LO base",
                raw_tris
            );
            return;
        }

        self.cache.body_vol_cache.detail_lo_quality_pending = false;
        self.cache.body_vol_cache.detail_lo_interactive = false;
        self.cache.body_vol_cache.detail_lo_last_scrub = None;
        let t_fold = std::time::Instant::now();
        // Full quality bake fold (same as Preprocess), not interactive draft fold.
        self.refresh_detail_lo_quality_bake_for(true);
        let ms = t_fold.elapsed().as_secs_f64() * 1000.0;
        if ms > 40.0 {
            eprintln!(
                "[SPIKE] detail_lo_quality_settle: {:.1}ms (raw_tris={})",
                ms, raw_tris
            );
        }
    }

    /// Re-apply Detail LO (+ mesh mods) from cached raw base — **no field rebake**.
    /// `active_only`: true = active top-level body (slider scrub); false = all bodies.
    pub(crate) fn refresh_detail_lo_displays(&mut self) {
        self.refresh_detail_lo_displays_for(false);
    }

    pub(crate) fn refresh_detail_lo_displays_for(&mut self, active_only: bool) {
        // Scrub interactive: fast Master-only. Otherwise full quality bake fold.
        let quality = !self.cache.body_vol_cache.detail_lo_interactive;
        self.refresh_detail_lo_displays_inner(active_only, quality);
    }

    /// Full quality bake fold (same path as Preprocess) — for settle / post-edit.
    pub(crate) fn refresh_detail_lo_quality_bake_for(&mut self, active_only: bool) {
        self.cache.body_vol_cache.detail_lo_interactive = false;
        self.refresh_detail_lo_displays_inner(active_only, true);
    }

    fn refresh_detail_lo_displays_inner(&mut self, active_only: bool, quality_bake: bool) {
        let active = self
            .scene
            .scene_graph
            .top_level_body(self.scene.active_body_nid);
        let nids: Vec<NodeId> = if active_only {
            match self.scene.scene_graph.get(active).map(|n| &n.payload) {
                Some(NodePayload::SdfBody { .. }) => vec![active],
                _ => Vec::new(),
            }
        } else {
            self.scene
                .scene_graph
                .sdf_bodies()
                .filter(|(nid, _)| !self.scene.scene_graph.is_sub_body(*nid))
                .map(|(nid, _)| nid)
                .collect()
        };
        let mut any = false;
        for nid in nids {
            let Some(cid) = (match self.scene.scene_graph.get(nid).map(|n| &n.payload) {
                Some(NodePayload::SdfBody {
                    mesh_cache_id: Some(c),
                    ..
                }) => Some(*c),
                _ => None,
            }) else {
                continue;
            };
            // Prefer cached base without re-clone when key matches.
            let base_key = self.mesh_mod_base_key(nid);
            let raw = if let Some((v, i, bk)) = self.cache.mesh_mod_base.get(&nid) {
                if *bk == base_key && !v.is_empty() && !i.is_empty() {
                    Some((v.clone(), i.clone()))
                } else {
                    None
                }
            } else {
                None
            };
            let Some((raw_v, raw_i)) = raw.or_else(|| self.raw_dc_mesh_for_fold(nid)) else {
                continue;
            };
            if raw_i.is_empty() {
                continue;
            }
            self.cache
                .mesh_mod_base
                .insert(nid, (raw_v.clone(), raw_i.clone(), base_key));
            let (fv, fi) = if quality_bake {
                self.fold_body_display_for_export(nid, raw_v, raw_i)
            } else {
                self.fold_body_display_after_dc(nid, raw_v, raw_i)
            };
            if fi.is_empty() {
                continue;
            }
            let tri = (fi.len() / 3) as u32;
            self.cache
                .mesh_cache
                .update_editable_mesh(cid, (fv, fi), tri);
            let stamp = match self.scene.scene_graph.get(nid).map(|n| &n.payload) {
                Some(NodePayload::SdfBody { mesh_modifiers, .. }) => {
                    self.mesh_mod_display_stamp_for(nid, mesh_modifiers)
                }
                _ => 0,
            };
            self.cache.mesh_mod_display_stamp.insert(nid, stamp);
            self.wire_local_cache.remove(&cid.0);
            any = true;
        }
        if any {
            self.overlay_lines_fp = u64::MAX;
            self.mark_dirty(crate::DirtyFlags::RENDER);
        }
    }

    /// Drop folded display caches so the next ensure/sync re-materializes + re-folds.
    /// Call on resolution change, bake land, field change, undo.
    pub(crate) fn invalidate_mesh_mod_display(&mut self, nid: NodeId) {
        // Bust wire overlay caches so edges refresh with the new mesh.
        if let Some(NodePayload::SdfBody {
            mesh_cache_id: Some(cid),
            ..
        }) = self.scene.scene_graph.get(nid).map(|n| &n.payload)
        {
            self.wire_local_cache.remove(&cid.0);
        }
        self.cache.mesh_mod_base.remove(&nid);
        self.cache.mesh_mod_display_stamp.remove(&nid);
        self.cache.body_vol_cache.detail_lo_preprocessed = false;
        self.overlay_lines_fp = u64::MAX; // force rebuild next frame
    }

    /// Rebuild folded display mesh into `mesh_cache` (Detail LO and/or mesh-mod stack).
    ///
    /// `allow_materialize`: **false** on the per-frame ensure path — never block on GPU
    /// readback/DC. Uses `mesh_mod_base` or `cpu_mesh` only; returns false if cold (DC land
    /// will fold later). **true** for Preprocess / explicit refresh.
    ///
    /// `fast_fold`: force interactive Master-only fold (skip analytical / multi-pass Field).
    pub(crate) fn sync_body_mesh_mod_display(&mut self, nid: NodeId) -> bool {
        self.sync_body_mesh_mod_display_ex(nid, true, false)
    }

    pub(crate) fn sync_body_mesh_mod_display_ex(
        &mut self,
        nid: NodeId,
        allow_materialize: bool,
        fast_fold: bool,
    ) -> bool {
        let records = match self.scene.scene_graph.get(nid).map(|n| &n.payload) {
            Some(NodePayload::SdfBody { mesh_modifiers, .. }) => mesh_modifiers.clone(),
            _ => return false,
        };
        let has_mods = !records.is_empty() && self.body_has_active_mesh_modifiers(nid);
        let has_detail = self.cache.body_vol_cache.use_detail_weighted_lo;

        // Neither fold path → leave Resident mesh alone on ensure (no blocking rematerialize).
        if !has_mods && !has_detail {
            if !allow_materialize {
                return false;
            }
            self.invalidate_mesh_mod_display(nid);
            if let Some(NodePayload::SdfBody {
                mesh_cache_id: Some(cid),
                ..
            }) = self.scene.scene_graph.get(nid).map(|n| &n.payload)
            {
                let cid = *cid;
                if let Some((v, i, _)) = self.materialize_sdf_body_mesh_raw(nid, None) {
                    if !i.is_empty() {
                        let tri = (i.len() / 3) as u32;
                        self.cache.mesh_cache.update_editable_mesh(cid, (v, i), tri);
                        self.wire_local_cache.remove(&cid.0);
                        self.overlay_lines_fp = u64::MAX;
                    }
                }
            }
            return false;
        }

        let base_key = self.mesh_mod_base_key(nid);
        let was_interactive = self.cache.body_vol_cache.detail_lo_interactive;

        // Compute interactive + quality stamps. Ensure (fast_fold) accepts EITHER so we never
        // thrash quality→interactive→quality every frame (was an 800–1200ms death spiral).
        self.cache.body_vol_cache.detail_lo_interactive = true;
        let stamp_interactive = self.mesh_mod_display_stamp_for(nid, &records);
        self.cache.body_vol_cache.detail_lo_interactive = false;
        let stamp_quality = self.mesh_mod_display_stamp_for(nid, &records);
        // Mode we will actually fold under.
        if fast_fold {
            self.cache.body_vol_cache.detail_lo_interactive = true;
        } else {
            self.cache.body_vol_cache.detail_lo_interactive = was_interactive;
        }
        let stamp_want = if fast_fold || was_interactive {
            stamp_interactive
        } else {
            stamp_quality
        };

        let cid_opt = match self.scene.scene_graph.get(nid).map(|n| &n.payload) {
            Some(NodePayload::SdfBody { mesh_cache_id: Some(c), .. }) => Some(*c),
            _ => None,
        };
        if let Some(cid) = cid_opt {
            if self.cache.mesh_cache.get(cid).is_some() {
                if let Some(&stored) = self.cache.mesh_mod_display_stamp.get(&nid) {
                    let warm = if fast_fold {
                        // Per-frame ensure: any current-params fold is good enough.
                        // Quality settle upgrades interactive→quality; ensure must not reverse it.
                        stored == stamp_interactive || stored == stamp_quality
                    } else {
                        stored == stamp_want
                    };
                    if warm {
                        self.cache.body_vol_cache.detail_lo_interactive = was_interactive;
                        return false;
                    }
                }
            }
        }

        // Raw base: prefer mesh_mod_base (true DC extract). Never use mesh_cache as base when a
        // stamp already exists — that mesh is the *folded* display, not raw extract.
        let had_stamp = self.cache.mesh_mod_display_stamp.contains_key(&nid);
        let base_opt = if let Some((v, i, bk)) = self.cache.mesh_mod_base.get(&nid) {
            if *bk == base_key && !v.is_empty() && !i.is_empty() {
                Some((v.clone(), i.clone()))
            } else {
                None
            }
        } else {
            None
        };
        let base_opt = base_opt.or_else(|| {
            if had_stamp {
                // Would re-fold an already-folded mesh — refuse; wait for fresh base.
                return None;
            }
            let cid = cid_opt?;
            self.cache.mesh_cache.get(cid).and_then(|c| {
                if !c.cpu_mesh.quads.is_empty() && !c.cpu_mesh.vertices.is_empty() {
                    Some(c.cpu_mesh.to_raster_data())
                } else {
                    None
                }
            })
        });
        let (base_verts, base_indices) = if let Some(b) = base_opt {
            b
        } else if allow_materialize {
            let Some((v, i, _)) = self.materialize_sdf_body_mesh_raw(nid, None) else {
                self.cache.body_vol_cache.detail_lo_interactive = was_interactive;
                return false;
            };
            (v, i)
        } else {
            // Cold — DC land path will fold; do not block the frame.
            self.cache.body_vol_cache.detail_lo_interactive = was_interactive;
            return false;
        };

        // Ensure path: refuse multi-hundred-ms Master folds on huge raw extracts.
        // DC land / Preprocess / quality settle own those (ensure must stay frame-budgeted).
        if fast_fold {
            let raw_tris = base_indices.len() / 3;
            // ~80k+ Master (planar+final) routinely costs 0.5–1.5s on one core.
            if raw_tris > 80_000 {
                // Keep raw base warm so quality settle / Preprocess can fold later.
                if self
                    .cache
                    .mesh_mod_base
                    .get(&nid)
                    .map(|(_, _, bk)| *bk != base_key)
                    .unwrap_or(true)
                {
                    self.cache.mesh_mod_base.insert(
                        nid,
                        (base_verts, base_indices, base_key),
                    );
                }
                self.cache.body_vol_cache.detail_lo_interactive = was_interactive;
                return false;
            }
        }

        // Store raw base when missing or key-mismatched (never overwrite good raw with folded).
        if self
            .cache
            .mesh_mod_base
            .get(&nid)
            .map(|(_, _, bk)| *bk != base_key)
            .unwrap_or(true)
        {
            self.cache.mesh_mod_base.insert(
                nid,
                (base_verts.clone(), base_indices.clone(), base_key),
            );
        }

        let (verts, indices) = self.fold_body_display_after_dc(nid, base_verts, base_indices);
        // CRITICAL: stamp WHILE detail_lo_interactive still matches the fold we just ran.
        // Restoring the flag first used to stamp quality while content was interactive (or
        // vice-versa) → next ensure always mismatched → 800ms re-fold every frame.
        let stamp = self.mesh_mod_display_stamp_for(nid, &records);
        self.cache.body_vol_cache.detail_lo_interactive = was_interactive;
        if indices.is_empty() {
            return false;
        }

        let cid = match self.scene.scene_graph.get(nid).map(|n| &n.payload) {
            Some(NodePayload::SdfBody { mesh_cache_id: Some(c), .. }) => *c,
            Some(NodePayload::SdfBody { mesh_cache_id: None, .. }) => {
                let cid = self.cache.mesh_cache.alloc_id();
                if let Some(n) = self.scene.scene_graph.get_mut(nid) {
                    if let NodePayload::SdfBody { mesh_cache_id, .. } = &mut n.payload {
                        *mesh_cache_id = Some(cid);
                    }
                }
                cid
            }
            _ => return false,
        };

        let tri = (indices.len() / 3) as u32;
        self.cache
            .mesh_cache
            .update_editable_mesh(cid, (verts, indices), tri);
        self.cache.mesh_mod_display_stamp.insert(nid, stamp);
        self.wire_local_cache.remove(&cid.0);
        self.overlay_lines_fp = u64::MAX;
        true
    }

    /// Per-frame ensure: **fast** fold only, **no** blocking materialize, budgeted.
    /// Full quality comes from DC land, Preprocess, or quality-settle after scrub.
    pub(crate) fn ensure_body_mesh_mod_displays(&mut self) {
        // TDR / stall guard: after a long frame, never start a Master fold.
        if self.prev_frame_wall_ms > 90.0 {
            return;
        }
        let active = self
            .scene
            .scene_graph
            .top_level_body(self.scene.active_body_nid);
        // D1 / R2: only SDF bodies can need a display fold (`body_needs_display_fold`).
        // Walking `all_ids()` at 100k poly instances was ~6 ms of scoreboard `mesh=` for
        // pure "false" filters — use `sdf_bodies()` only. (Fixed independently on both
        // branches — 5735545 and e04e4a3; kept once at merge.)
        let mut nids: Vec<NodeId> = self
            .scene
            .scene_graph
            .sdf_bodies()
            .map(|(nid, _)| nid)
            .filter(|&nid| self.body_needs_display_fold(nid))
            .collect();
        // Active body first — selection stalls were 370–1500ms folding everything.
        nids.sort_by_key(|nid| if *nid == active { 0u8 } else { 1u8 });

        let t0 = std::time::Instant::now();
        const BUDGET_MS: u128 = 10;
        let mut any = false;
        let mut folded = 0u32;
        for nid in nids {
            // Hard cap: at most one body fold per frame; bail if setup already ate budget.
            if folded >= 1 || t0.elapsed().as_millis() > BUDGET_MS {
                break;
            }
            // allow_materialize=false, fast_fold=true
            let did = self.sync_body_mesh_mod_display_ex(nid, false, true);
            if did {
                any = true;
                folded += 1;
                // Just wrote interactive draft → one quality settle after ~180ms idle.
                // Do not re-arm when quality already matches (stops settle↔ensure thrash).
                if nid == active && self.cache.body_vol_cache.use_detail_weighted_lo {
                    let records = match self.scene.scene_graph.get(nid).map(|n| &n.payload) {
                        Some(NodePayload::SdfBody { mesh_modifiers, .. }) => {
                            mesh_modifiers.clone()
                        }
                        _ => Vec::new(),
                    };
                    let was = self.cache.body_vol_cache.detail_lo_interactive;
                    self.cache.body_vol_cache.detail_lo_interactive = false;
                    let q = self.mesh_mod_display_stamp_for(nid, &records);
                    self.cache.body_vol_cache.detail_lo_interactive = was;
                    if self.cache.mesh_mod_display_stamp.get(&nid) != Some(&q) {
                        self.cache.body_vol_cache.detail_lo_quality_pending = true;
                        self.cache.body_vol_cache.detail_lo_last_scrub =
                            Some(std::time::Instant::now());
                    }
                }
            } else if nid == active
                && self.cache.body_vol_cache.use_detail_weighted_lo
                && !self.cache.body_vol_cache.detail_lo_quality_pending
                && !self.cache.mesh_mod_display_stamp.contains_key(&nid)
            {
                // Ensure refused (e.g. raw >80k tris) with warm base and no display fold yet
                // → let quality settle own the first fold off the frame path.
                let base_warm = self
                    .cache
                    .mesh_mod_base
                    .get(&nid)
                    .map(|(_, i, bk)| *bk == self.mesh_mod_base_key(nid) && !i.is_empty())
                    .unwrap_or(false);
                if base_warm {
                    self.cache.body_vol_cache.detail_lo_quality_pending = true;
                    self.cache.body_vol_cache.detail_lo_last_scrub =
                        Some(std::time::Instant::now());
                }
            }
        }
        if any {
            self.mark_dirty(crate::DirtyFlags::RENDER);
        }
    }

    /// After undo/redo: mesh_modifiers may change while mesh_cache still holds the post-edit fold.
    /// Clear stamps/bases so the next ensure re-folds (or drops back to Resident when stack empty).
    pub(crate) fn invalidate_all_mesh_mod_displays(&mut self) {
        self.cache.mesh_mod_display_stamp.clear();
        self.cache.mesh_mod_base.clear();
        self.cache.body_vol_cache.detail_lo_preprocessed = false;
        self.wire_local_cache.clear();
        self.overlay_lines_fp = u64::MAX;
    }
}

// ================================================================================================
// Deferred export request — queued by the panel, processed in poll (Frame Ordering Contract).
// ================================================================================================

/// One queued OBJ export. Poly meshes are collected in the panel (CPU-only — legal there); the
/// SDF bodies are materialized in poll via the shared job.
pub(crate) struct PendingExport {
    pub(crate) filename: String,
    /// (name, world-space [f32;8] verts, tri indices) — already transformed by the panel.
    pub(crate) poly_objs: Vec<(String, Vec<[f32; 8]>, Vec<u32>)>,
    /// `None` = each body's display resolution (export == display); `Some(res)` = override
    /// through the same GPU QEF-DC pipeline.
    pub(crate) res_override: Option<u32>,
}

impl crate::AzraApp {
    /// Poll-phase half of the export: materialize every visible top-level SDF body through the
    /// SHARED job (bounded blocking readback — one-shot user action), transform to world, then
    /// hand poly+SDF objects to a writer thread. Call from `poll_background_tasks`.
    pub(crate) fn process_export_pending(&mut self) {
        let Some(req) = self.ui.export_pending.take() else { return; };
        // Same body enumeration rule as the G-buffer/PT gathers: visible, top-level, renderable.
        let nids: Vec<(NodeId, String, Vec3, glam::Quat, Vec3)> = self
            .scene
            .scene_graph
            .all_nodes()
            .filter_map(|(nid, node)| match &node.payload {
                NodePayload::SdfBody { name, origin, body_rotation, body_scale, .. }
                    if node.visible
                        && self.scene.scene_graph.parent_body(nid).is_none()
                        && self.scene.scene_graph.body_has_renderable_content(nid) =>
                {
                    Some((nid, name.clone(), *origin, *body_rotation, *body_scale))
                }
                _ => None,
            })
            .collect();

        let mut objs = req.poly_objs;
        for (nid, name, origin, rot, scale) in nids {
            let Some((verts, indices, arm)) = self.materialize_sdf_body_mesh(nid, req.res_override)
            else {
                eprintln!("[Export] body '{name}': no mesh could be materialized — skipped");
                continue;
            };
            // Body-local → WORLD (the transform rule the G-buffer/PT gathers use).
            let mut model = if crate::BODY_LOCAL_PRIMS {
                self.scene
                    .scene_graph
                    .effective_body_transform(nid)
                    .unwrap_or_else(|| glam::Mat4::from_scale_rotation_translation(scale, rot, origin))
            } else {
                glam::Mat4::from_translation(origin)
            };
            if arm == SdfMaterializeArm::DisplayReadback {
                // Rigid-translate re-anchor — same rule as the draw/PT gathers: a cached display
                // mesh predates any pending rigid prim-drag offset.
                let off = self.cache.body_vol_cache.mesh_rigid_offset(nid);
                if off.length_squared() > 0.0 {
                    model *= glam::Mat4::from_translation(off);
                }
            }
            let n_mat = glam::Mat3::from_mat4(model).inverse().transpose();
            let world: Vec<[f32; 8]> = verts
                .iter()
                .map(|v| {
                    let p = model * glam::Vec4::new(v[0], v[1], v[2], 1.0);
                    let n = (n_mat * Vec3::new(v[3], v[4], v[5])).normalize_or_zero();
                    [p.x, p.y, p.z, n.x, n.y, n.z, v[6], v[7]]
                })
                .collect();
            eprintln!(
                "[Export] body '{name}': {} verts / {} tris via {arm:?} (+ export quality LOD fold)",
                world.len(),
                indices.len() / 3
            );
            objs.push((name, world, indices));
        }

        // OBJ write on a thread (unchanged UX — the UI stays responsive during the file write).
        let (tx, rx) = std::sync::mpsc::channel();
        self.ui.export_rx = Some(rx);
        let filename = req.filename;
        std::thread::spawn(move || {
            let t0 = std::time::Instant::now();
            let file = match std::fs::File::create(&filename) {
                Ok(f) => f,
                Err(e) => {
                    let _ = tx.send(Err(format!("Failed to create file: {}", e)));
                    return;
                }
            };
            let mut w = std::io::BufWriter::new(file);
            match write_obj_objects(&mut w, &objs).and_then(|r| { use std::io::Write; w.flush()?; Ok(r) }) {
                Ok((0, _)) => { let _ = tx.send(Err("No geometry to export".into())); }
                Ok((tv, tf)) => {
                    let _ = tx.send(Ok((filename, tv as usize, tf as usize, t0.elapsed().as_secs_f32(), false)));
                }
                Err(e) => { let _ = tx.send(Err(format!("Write failed: {}", e))); }
            }
        });
    }
}

// ================================================================================================
// Shared OBJ writer — poly + SDF objects are now the SAME shape (name, [f32;8] verts, tri idx),
// written by ONE function (validity-testable without a GPU or a window).
// ================================================================================================

/// Write `(name, verts8, tri-indices)` objects as Wavefront OBJ (`v`/`vn`/`f a//a b//b c//c`).
/// Returns `(total_verts, total_faces)`. Indices are 0-based per object; offsets are handled here.
pub(crate) fn write_obj_objects(
    w: &mut impl std::io::Write,
    objs: &[(String, Vec<[f32; 8]>, Vec<u32>)],
) -> std::io::Result<(u64, u64)> {
    writeln!(w, "# AZRA Export")?;
    let mut total_verts = 0u64;
    let mut total_faces = 0u64;
    let mut vert_offset = 1u32; // OBJ indices are 1-based
    for (name, verts, indices) in objs {
        writeln!(w, "o {}", name)?;
        for v in verts {
            writeln!(w, "v {} {} {}", v[0], v[1], v[2])?;
        }
        for v in verts {
            writeln!(w, "vn {} {} {}", v[3], v[4], v[5])?;
        }
        for tri in indices.chunks(3) {
            if tri.len() == 3 {
                let (a, b, c) = (tri[0] + vert_offset, tri[1] + vert_offset, tri[2] + vert_offset);
                writeln!(w, "f {}//{} {}//{} {}//{}", a, a, b, b, c, c)?;
                total_faces += 1;
            }
        }
        total_verts += verts.len() as u64;
        vert_offset += verts.len() as u32;
    }
    Ok((total_verts, total_faces))
}

// ================================================================================================
// Tests — facade equivalence (headless), export file validity (headless), export == display (GPU).
// ================================================================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// FNV-1a mesh hash over vert f32-bits + indices (the dc_baseline golden convention, applied
    /// to the [f32;8]/tri format both materialize arms emit).
    fn hash_verts8(verts: &[[f32; 8]], indices: &[u32]) -> u64 {
        let mut h: u64 = 0xcbf29ce484222325;
        let mut eat = |v: u32| {
            for b in v.to_le_bytes() {
                h ^= b as u64;
                h = h.wrapping_mul(0x00000100000001b3);
            }
        };
        eat(verts.len() as u32);
        eat(indices.len() as u32);
        for v in verts {
            for f in v {
                eat(f.to_bits());
            }
        }
        for &i in indices {
            eat(i);
        }
        h
    }

    /// GATE 4 (b) — facade predicate equivalence: for EVERY combination of the raw inputs, the
    /// facade's answer equals the old 3-predicate union the call sites computed ad hoc:
    ///   is_resident  = route_resident && slot_present            (raster_gbuffer.rs / raster_pick.rs)
    ///   pooled       = is_resident && slot_pooled && pool_region (Stage B-1 lockstep predicate)
    ///   drawable_cpu = !is_resident && cpu_mesh                  (the mesh_cache presence check)
    ///   has_any      = is_resident || drawable_cpu
    #[test]
    fn gate4_facade_matches_legacy_predicate_union() {
        for mask in 0u32..32 {
            let route = mask & 1 != 0;
            let slot = mask & 2 != 0;
            let pooled_flag = mask & 4 != 0;
            let pool = mask & 8 != 0;
            let cpu = mask & 16 != 0;

            let legacy_resident = route && slot;
            let legacy_pooled = legacy_resident && pooled_flag && pool;
            let legacy_cpu_draw = !legacy_resident && cpu;
            let legacy_has_any = legacy_resident || legacy_cpu_draw;

            let pres = derive_presentation(route, slot, pooled_flag, pool, cpu);
            assert_eq!(pres.is_resident(), legacy_resident, "resident mismatch mask={mask:05b}");
            assert_eq!(pres.is_pooled(), legacy_pooled, "pooled mismatch mask={mask:05b}");
            assert_eq!(
                matches!(pres, SdfMeshPresentation::CachedCpu),
                legacy_cpu_draw,
                "cpu-draw mismatch mask={mask:05b}"
            );
            assert_eq!(pres.has_any_mesh(), legacy_has_any, "has-any mismatch mask={mask:05b}");
        }
    }

    /// GATE 4 (b, ref scene) — on the Gate-0 reference scene the CPU-oracle materialize agrees
    /// vert-for-vert/index-for-index with the golden harness's mesh (same pipeline, one source).
    #[test]
    fn gate4_cpu_materialize_matches_golden_pipeline() {
        let (sg, bodies, _mat) = crate::dc_baseline::build_ref_scene();
        for b in bodies.iter().take(2) {
            // two regimes keep the test fast; golden 5/5 is pinned by dc_gate0_golden_determinism
            let (golden_mesh, _hash, _mn, _mx) = crate::dc_baseline::body_golden(&sg, b.nid, 96);
            let (mesh, _gmin, _gmax) = cpu_oracle_mesh(&sg, b.nid, 96);
            assert_eq!(mesh.vertices, golden_mesh.vertices, "{}: verts diverge from golden pipeline", b.name);
            assert_eq!(mesh.quads, golden_mesh.quads, "{}: quads diverge from golden pipeline", b.name);
        }
    }

    /// GATE 4 (c) — export file validity: materialize a ref body on the CPU oracle, write OBJ via
    /// the shared writer, re-parse: v/vn counts match, every face index in bounds, positions
    /// finite, tri count > 0 and equal to the writer's report.
    #[test]
    fn gate4_export_obj_file_validity() {
        let (sg, bodies, _mat) = crate::dc_baseline::build_ref_scene();
        let b = &bodies[0];
        let (mesh, _gmin, _gmax) = cpu_oracle_mesh(&sg, b.nid, 64);
        let (verts, indices) = mesh_to_verts8(&mesh);
        assert!(!indices.is_empty(), "ref body produced no geometry");
        assert_eq!(indices.len() % 3, 0, "materialize must emit whole triangles");

        let objs = vec![(b.name.to_string(), verts.clone(), indices.clone())];
        let mut buf = Vec::new();
        let (tv, tf) = write_obj_objects(&mut buf, &objs).expect("obj write");
        assert_eq!(tv as usize, verts.len());
        assert_eq!(tf as usize, indices.len() / 3);

        // Round-trip parse.
        let text = String::from_utf8(buf).expect("obj is utf8");
        let mut v_count = 0usize;
        let mut vn_count = 0usize;
        let mut f_count = 0usize;
        for line in text.lines() {
            let mut it = line.split_whitespace();
            match it.next() {
                Some("v") => {
                    v_count += 1;
                    for _ in 0..3 {
                        let f: f32 = it.next().expect("v needs 3 comps").parse().expect("v comp parses");
                        assert!(f.is_finite(), "non-finite vertex position in OBJ");
                    }
                }
                Some("vn") => vn_count += 1,
                Some("f") => {
                    f_count += 1;
                    let mut n = 0;
                    for tok in it {
                        n += 1;
                        let vi: usize = tok.split("//").next().unwrap().parse().expect("face index parses");
                        assert!(vi >= 1 && vi <= verts.len(), "face index {vi} out of bounds 1..={}", verts.len());
                    }
                    assert_eq!(n, 3, "SDF export writes triangles");
                }
                _ => {}
            }
        }
        assert_eq!(v_count, verts.len());
        assert_eq!(vn_count, verts.len());
        assert_eq!(f_count, indices.len() / 3);
    }

    /// Order-CANONICAL mesh hash: triangles as position/normal bit-triples, each rotated so its
    /// lexicographically-smallest vertex leads (winding preserved), then sorted. Needed because
    /// the GPU DC emits verts/quads via atomic append — two dispatches of the SAME field produce
    /// the SAME triangles in a DIFFERENT order (confirmed live: identical counts, permuted
    /// order). Geometry-identical ⇔ canonical-hash-identical.
    fn canonical_hash(verts: &[[f32; 8]], indices: &[u32]) -> u64 {
        let key = |i: u32| -> [u32; 8] {
            let v = verts[i as usize];
            [v[0].to_bits(), v[1].to_bits(), v[2].to_bits(), v[3].to_bits(),
             v[4].to_bits(), v[5].to_bits(), v[6].to_bits(), v[7].to_bits()]
        };
        let mut tris: Vec<[[u32; 8]; 3]> = indices
            .chunks(3)
            .filter(|t| t.len() == 3)
            .map(|t| {
                let k = [key(t[0]), key(t[1]), key(t[2])];
                // rotate (not sort) so the smallest vertex leads — winding preserved.
                let lead = (0..3).min_by_key(|&i| k[i]).unwrap();
                [k[lead], k[(lead + 1) % 3], k[(lead + 2) % 3]]
            })
            .collect();
        tris.sort();
        let mut h: u64 = 0xcbf29ce484222325;
        for t in &tris {
            for v in t {
                for &u in v {
                    for b in u.to_le_bytes() {
                        h ^= b as u64;
                        h = h.wrapping_mul(0x00000100000001b3);
                    }
                }
            }
        }
        h
    }

    /// GATE 4 HEADLINE — export at display resolution produces the SAME mesh as the display.
    /// GPU test (`require_adapter` — panics without a device, per the Gate-0 assertion
    /// discipline; `#[ignore]` keeps headless CI green like `dc_gate0_gpu_cpu_golden_bridge`).
    /// Run: `cargo test -p app gate4_export_mesh_matches_display -- --ignored --nocapture`.
    ///
    /// display mesh  = the body's grid meshed into a display slot (`mesh_into_slot`, the G2
    ///                 populate path) then read back — what the viewport draws;
    /// export arm 2  = `dispatch_sdf_display_readback(DisplaySlot)` — the DEFAULT export path:
    ///                 a copy of the display buffers ⇒ must be BYTE-hash-identical;
    /// export arm 3  = `mesh_readback` over the same grid (the res-override pipeline) — same
    ///                 shader, same field ⇒ same TRIANGLE SET; compared with the order-canonical
    ///                 hash (GPU atomic-append emission order is dispatch-nondeterministic).
    #[test]
    #[ignore]
    fn gate4_export_mesh_matches_display_mesh_at_display_res() {
        use crate::bake_utils::{bake_body_grid_field, collect_body_prims_recursive,
                                compute_body_bake_bounds, pad_grid_bounds};
        let (_gpu_lock, device, queue) = crate::dc_baseline::require_adapter();
        let (sg, bodies, _mat) = crate::dc_baseline::build_ref_scene();
        let mesher = SdfMeshGpu::new(&device);
        let res = 96u32;

        for b in bodies.iter() {
            let prims = collect_body_prims_recursive(&sg, b.nid);
            let (tmin, tmax) = compute_body_bake_bounds(&prims, 0, Vec3::ZERO, 0.0);
            let (gmin, gmax) = pad_grid_bounds(tmin, tmax);
            let field = core_math::field_eval::BodyField::from_graph(&sg, b.nid);
            let grid = bake_body_grid_field(&field, res, gmin, gmax);

            // Display: mesh into a slot exactly like the G2 populate, read it back.
            let mut slot = SdfBodyMeshSlot::new(&device, res);
            mesher.mesh_into_slot(&device, &queue, &mut slot, &grid, gmin, gmax);
            let pending = readback_slot(&device, &queue, &slot);
            device.poll(wgpu::PollType::Wait).expect("GPU device polling failed");
            let (dv, di) = pending.take();
            assert!(!di.is_empty(), "{}: display slot produced no mesh", b.name);
            let display_hash = hash_verts8(&dv, &di);

            // Export arm 2 (display readback) — the default export path.
            assert!(display_slot_snapshot_ok(&slot, res), "{}: slot must be a valid snapshot source", b.name);
            let pending = dispatch_sdf_display_readback(
                &device, &queue, &mesher, SdfDisplayReadbackSrc::DisplaySlot(&slot));
            device.poll(wgpu::PollType::Wait).expect("GPU device polling failed");
            let (ev, ei) = pending.take();
            assert_eq!(
                hash_verts8(&ev, &ei), display_hash,
                "{}: export display-readback mesh != display mesh", b.name
            );

            // Export arm 3 (grid DC through the same shader) — the res-override path.
            let (gv, gi) = mesher.mesh_readback(&device, &queue, &grid, res, gmin, gmax);
            assert_eq!(gv.len(), dv.len(), "{}: grid-DC vert count != display", b.name);
            assert_eq!(gi.len(), di.len(), "{}: grid-DC index count != display", b.name);
            assert_eq!(
                canonical_hash(&gv, &gi), canonical_hash(&dv, &di),
                "{}: export grid-DC triangle set != display mesh at equal res (the divergence Gate 4 closes)",
                b.name
            );
            eprintln!("[GATE4] {}: display==export (verts={} tris={} hash=0x{display_hash:016x})",
                b.name, dv.len(), di.len() / 3);
        }
    }
}
