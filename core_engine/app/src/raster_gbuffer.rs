// ============================================================
// Hybrid Rasterization — G-buffer Raster Pass (Slice 19)
// ============================================================
//
// Byte-for-byte relocation of `dispatch_raster_pass` out of `raster_render.rs`.
// This is the G-buffer raster pass: it renders cached SDF/voxel/poly/NURBS body
// meshes into the G-buffer + depth (plus the 3 shadow cascades), submitted before
// the SDF compute command buffer. It stays an `impl AzraApp` method (NOT a free
// function): it reads `&self.cache.mesh_cache` / `&self.scene.scene_graph` while
// `let Some(gpu) = &mut self.gpu` is live — that only compiles because gpu/cache/
// scene are disjoint fields under one `&mut self` (see APP_CRATE_DEGOD_PLAN §C).

use eframe::wgpu;
use core_math::scene_graph::NodePayload;
use crate::SceneItemId;
use crate::body_volume_cache::BodyDisplayState;
use render::mesh_gpu::{InstanceData, RasterMaterial};
use render::camera::CameraUniforms;
use render::shadows::NUM_CASCADES;
// Shared free-fn draw helpers (Slice 18) — live in raster_common.rs; imported here so the
// unqualified call sites in dispatch_raster_pass keep resolving.
use crate::raster_common::{
    aabb_outside_frustum, draw_kind_is_special, draw_resident_pool, draw_sdf_pool_multi,
    draw_sdf_pool_slot, draw_sdf_resident_slot, draw_skip_main, draw_skip_shadow, frustum_planes,
    scene_bvh_collect_frustum_culled, world_aabb, OcclusionBuffer, DRAW_CULL_FRUSTUM,
    DRAW_CULL_OCCLUDED, DRAW_KIND_MESH, DRAW_KIND_SDF_POOLED, DRAW_KIND_SDF_RESIDENT,
    OCCLUSION_BUF_H, OCCLUSION_BUF_W,
    DRAW_KIND_VOXEL_RESIDENT, FRUSTUM_BVH_MAX_DRAWS, FRUSTUM_BVH_MIN_DRAWS,
};

/// When true, prim positions are stored in body-local space.
/// Prims must NOT be re-offset when baking the DC mesh (they are already local).
/// The instance transform must include body_rotation and body_scale so the mesh
/// is placed correctly in world space.
use crate::BODY_LOCAL_PRIMS;

impl super::AzraApp {
    /// Execute the rasterization render pass into the G-buffer.
    /// Creates and submits its own command buffer (submitted before the SDF compute buffer).
    /// Execute the rasterization render pass into the G-buffer.
    /// Returns true if any SDF body was rasterized (used by viewport_render
    /// to decide whether raster depth should be copied for compute early-exit).
    pub(crate) fn dispatch_raster_pass(&mut self, _width: u32, _height: u32) -> bool {
        // Display policy (surface-mode stream):
        //   res < 512  → triangle chunks (lights/materials, 3D-Coat-like)
        //   res ≥ 512 sparse → raymarch live (mesh remesh cannot keep up → tearing)
        //   mask/paint/move grab → force mesh when needed
        let sculpt_raymarch_overlay = self.sculpt.voxel.as_ref()
            .map_or(false, |v| v.use_sculpt_raymarch_overlay());
        // --- Phase 1: Structural draw list (transform-derived) + per-frame cull mask ---
        //
        // Gather + sort produce InstanceData / materials / kinds — stable until something
        // actually moves (SCENE|MESH), selection outline changes (sel_bit), or poly LOD swaps.
        // Camera orbit only rewrites the cull mask and submits; no rebuild, no re-sort, no
        // 800 KB content-hash over a freshly built array (C.7 still skips upload when stable).

        // AZRA_MASTER_ROADMAP Phase A6: O(S·D) once per frame, then O(1) per node.
        // Outline when selected OR a selected node is a descendant → mark each selected
        // node and walk *up* to root (ancestors own fused meshes).
        //
        // **Nested SdfBody (sub-body):** do NOT climb into a parent SdfBody. Nested bodies
        // bake into the parent's single mesh — marking the parent made the whole kit look
        // selected (owner: "when I select one subbody all the bodies is selected").
        // Sub-body volume is shown via prim-outline of that body's prims
        // (`selected_prim_indices_for_gpu` expands nested body picks).
        //
        // Active edit subject (poly V/E/F, sculpt, NURBS sub-select): suppress **that**
        // body's green sel_bit rim only — component highlights carry the state. Other
        // selected-but-not-active bodies still outline. Selection set itself is untouched
        // (P0: selection always works).
        let mut selected_outline: std::collections::HashSet<core_math::scene_graph::NodeId> =
            std::collections::HashSet::new();
        for sel_nid in self.sel.usel.selected_nodes() {
            use core_math::scene_graph::NodePayload;
            let payload = self.scene.scene_graph.get(sel_nid).map(|n| &n.payload);
            // Stop climbing into a parent SdfBody's fused mesh when the pick is a
            // nested sub-body or a Group/SdfObject (kitbash folder of sub-parts).
            let stop_before_parent_body = match payload {
                Some(NodePayload::SdfBody { .. }) => {
                    self.scene.scene_graph.is_sub_body(sel_nid)
                }
                Some(NodePayload::Group { .. }) | Some(NodePayload::SdfObject { .. }) => {
                    true
                }
                _ => false,
            };

            if stop_before_parent_body {
                // Sub-body / group: mark only the node itself (and descendant drawable
                // top-level bodies under a kitbash Group, if any). Prim-outline shows volume.
                selected_outline.insert(sel_nid);
                // Group of top-level-for-bake bodies (parent is Group, not SdfBody): outline
                // each drawable descendant body mesh.
                if matches!(
                    payload,
                    Some(NodePayload::Group { .. }) | Some(NodePayload::SdfObject { .. })
                ) {
                    for (nid, node) in self.scene.scene_graph.all_nodes() {
                        if !matches!(node.payload, NodePayload::SdfBody { .. }) {
                            continue;
                        }
                        if !self.scene.scene_graph.is_descendant_of(nid, sel_nid) {
                            continue;
                        }
                        // Drawable = no parent SdfBody (nested under body is fused).
                        if self.scene.scene_graph.parent_body(nid).is_none() {
                            selected_outline.insert(nid);
                        }
                    }
                }
                continue;
            }

            let mut cur = Some(sel_nid);
            while let Some(c) = cur {
                if !selected_outline.insert(c) {
                    break; // rest of ancestors already marked
                }
                cur = self.scene.scene_graph.get(c).and_then(|n| n.parent);
            }
        }
        if let Some(subject) = self.outline_suppressed_edit_subject() {
            selected_outline.remove(&subject);
        }

        // B.1 NodeId stream — O(drawables). Camera-only frames reuse (streamReb=0).
        self.ensure_structural_draw_stream();

        // sel_bit is baked into InstanceData — rebuild structural list when outline set changes.
        let outline_key = {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            let mut ids: Vec<u64> = selected_outline.iter().map(|n| n.0).collect();
            ids.sort_unstable();
            for id in ids {
                id.hash(&mut h);
            }
            h.finish()
        };
        if outline_key != self.structural_outline_key {
            self.structural_draw_stale = true;
        }

        let mut materials: Vec<RasterMaterial>;
        let mut draw_calls: Vec<(
            core_math::scene_graph::MeshCacheId,
            InstanceData,
            u8,
            Option<(u32, u32)>,
        )>;
        let mut draw_kind: Vec<u8>;
        let mut draw_kind_payload: Vec<u64>;
        let mut world_aabbs: Vec<(glam::Vec3, glam::Vec3)>;
        let mut any_sdf_rasterized: bool;

        // Material-content generation: graph materials bump this on every
        // `material_by_id_mut` (map install, PBR factor sliders, live noise, blend edits…).
        // Structural list caches `RasterMaterial` bytes — mismatch must rebuild even when
        // only RENDER is dirty (M0 class: roughness scrub invisible under static camera).
        let mat_gen = self.scene.scene_graph.material_generation();
        if !self.structural_draw_stale && self.structural_material_gen != mat_gen {
            self.structural_draw_stale = true;
        }
        // Safety net: first gather often runs before the SDF mesh lands (display_state
        // NeverBaked → skip). Mesh arrival only marks RENDER, so the empty list was reused
        // forever and Hybrid showed background only (AZRA_VISDIFF). If any top-level SDF
        // body now has a drawable mesh but the cached list is empty of SDF, force rebuild.
        if !self.structural_draw_stale && self.structural_draw_calls.is_empty() {
            for (nid, n) in self.scene.scene_graph.all_nodes() {
                if self.scene.scene_graph.parent_body(nid).is_some() {
                    continue;
                }
                if let NodePayload::SdfBody { mesh_cache_id, .. } = &n.payload {
                    if self.sdf_mesh_presentation(nid, *mesh_cache_id).has_any_mesh() {
                        self.structural_draw_stale = true;
                        break;
                    }
                }
            }
        }

        if !self.structural_draw_stale {
            // Camera-only (or any non-structural) frame: reuse cached array; zero cull mask.
            self.structural_draw_rebuilds_last_frame = 0;
            materials = std::mem::take(&mut self.structural_materials);
            draw_calls = std::mem::take(&mut self.structural_draw_calls);
            draw_kind = std::mem::take(&mut self.structural_draw_kind);
            draw_kind_payload = std::mem::take(&mut self.structural_draw_kind_payload);
            world_aabbs = std::mem::take(&mut self.structural_world_aabb);
            any_sdf_rasterized = self.structural_any_sdf;
            for e in &mut draw_calls {
                e.2 = 0;
            }
        } else {
            // Full gather + sort into local vecs (then cached at end of pass).
            self.structural_draw_rebuilds_last_frame = 1;
            materials = Vec::new();
            world_aabbs = Vec::new();
            // C-scale: dedupe materials by GPU content (same pattern as poly_draw_share).
            // 10k clones of one mat → one slot; red cube + blue cube → two slots.
            // NEVER assume slot 0 for all mono bodies (regression: every body painted first mat).
            let mut mat_share: std::collections::HashMap<u64, u32> =
                std::collections::HashMap::new();
            let mut push_mat = |materials: &mut Vec<RasterMaterial>, rm: RasterMaterial| -> u32 {
                use std::hash::{Hash, Hasher};
                let mut h = std::collections::hash_map::DefaultHasher::new();
                bytemuck::bytes_of(&rm).hash(&mut h);
                let key = h.finish();
                if let Some(&idx) = mat_share.get(&key) {
                    return idx;
                }
                let idx = materials.len() as u32;
                materials.push(rm);
                mat_share.insert(key, idx);
                idx
            };
            // (cid, instance, cull_flags, optional index subrange for multi-material poly)
            // cull_flags: DRAW_CULL_MAIN / DRAW_CULL_SHADOW (C.2) — not a bool.
            draw_calls = Vec::new();
            // C.8: parallel kind + payload (O(1) flags). Replaces HashMap probes on every pass.
            draw_kind = Vec::new();
            draw_kind_payload = Vec::new();
            any_sdf_rasterized = false;
            // PLAN_FOR_GROK_VIEWPORT_PERF P4: identical poly content → same MeshCacheId at draw
            // so Shift+D / shared LOD can multi-instance (edit path keeps per-body cache ids).
            let mut poly_draw_share: std::collections::HashMap<
                (u64, u64, u32),
                core_math::scene_graph::MeshCacheId,
            > = std::collections::HashMap::new();

            let stream_nids = self.draw_stream.clone();

            for nid in stream_nids {
            let Some(node) = self.scene.scene_graph.get(nid) else {
                continue;
            };
            if !node.visible {
                continue;
            }
            // C.5: dense 0-based body_index → gbuf_motion.r (same contract as selected_body_index).
            let bi = self.cache.body_index_map.get(&nid).copied().unwrap_or(0) as u32;
            // Selection flag: pack into material_idx bit 31 so the fragment shader can write flags=1.0.
            // Prim selection does NOT outline the body (gizmo + prim overlay). Active edit subject
            // (poly V/E/F, sculpt, NURBS sub-select) is already removed from `selected_outline`.
            let sel_bit: u32 = if selected_outline.contains(&nid) {
                0x80000000
            } else {
                0
            };
            match &node.payload {
                // SdfBody: per-body display decision via state machine
                NodePayload::SdfBody { mesh_cache_id, primitives, material: body_mat, origin, body_rotation, body_scale, .. } => {
                    // Canonical renderability: skip only a TRULY empty body (no own prims AND no sub-bodies).
                    // A top-level body whose prims all live in nested sub-bodies (e.g. after Group) has an
                    // EMPTY own `primitives` but is still fully defined by its recursive subtree bake — its
                    // resident slot / mesh holds the geometry. Deciding via `primitives.is_empty()` alone is
                    // what made "group everything into a sub-body → surface vanishes". Nested bodies are still
                    // skipped at line 393.
                    if !self.scene.scene_graph.body_has_renderable_content(nid) { continue; }
                    // Display Invariant: a NESTED body (parent is another SdfBody) is SUBSUMED into its
                    // top-level ancestor's subtree bake (collect_body_prims_recursive pulls its prims in).
                    // It must NEVER rasterize its own standalone mesh — doing so draws the geometry twice
                    // (once standalone here, once inside the parent's subtree mesh). Only top-level bodies
                    // (parent == Root) own a render-visible mesh.
                    if self.scene.scene_graph.parent_body(nid).is_some() { continue; }
                    // [BODYVIS] AZRA_MASTER_ROADMAP Phase A3: entire change-detection block is behind
                    // `perf_spam_on()` — format!/HashMap must not run on the idle hot path.
                    if crate::perf_spam_on() {
                        use crate::body_volume_cache::BodyDisplayState as Ds;
                        let ds = match self.cache.body_vol_cache.display_state(nid) {
                            Ds::Raster => "Raster",
                            Ds::RasterOldCache => "RasterOldCache",
                            Ds::TransformSafe => "TransformSafe",
                            Ds::NeverBaked => "NeverBaked",
                        };
                        let mesh_present =
                            mesh_cache_id.map_or(false, |c| self.cache.mesh_cache.get(c).is_some());
                        use std::cell::RefCell;
                        thread_local! {
                            static LAST: RefCell<std::collections::HashMap<String, (String, bool)>> =
                                RefCell::new(std::collections::HashMap::new());
                        }
                        let key = format!("{nid:?}");
                        let cur = (ds.to_string(), mesh_present);
                        let changed = LAST.with(|m| {
                            let mut m = m.borrow_mut();
                            if m.get(&key) != Some(&cur) {
                                m.insert(key, cur);
                                true
                            } else {
                                false
                            }
                        });
                        if changed {
                            eprintln!(
                                "[BODYVIS] body={nid:?} display={ds} mesh_present={mesh_present} prims={}",
                                primitives.len()
                            );
                        }
                    }
                    // Isolation: non-isolated bodies render as ghost context or normal (user toggle)
                    let is_outside_isolation = if let Some(iso) = self.scene.isolated_body_nid {
                        nid != iso && !self.scene.scene_graph.is_descendant_of(nid, iso)
                    } else { false };
                    // Ghost bit (bit 30): marks non-isolated bodies for desaturated rendering
                    let ghost_bit: u32 = if is_outside_isolation && self.isolate_context_ghost { 0x40000000 } else { 0 };
                    // Determine which mesh cache ID to draw.
                    // When volume baking is disabled (default), draw directly from
                    // the mesh cache — the display_state is only meaningful when
                    // the per-body voxel cache is active.
                    //
                    // Cache-first: non-active bodies always rasterize from mesh cache.
                    let is_active = nid == self.scene.active_body_nid;
                    // P0.2-full GUARD 6 (historical): the "skip active body from raster during a
                    // drag" continue was gated on SDF Edit Mode (sdf_edit_active — never wired,
                    // deleted 2026-07-13). In Object Mode there is NO march, so the active SDF body
                    // must keep rastering from its (TransformSafe) mesh during a drag — otherwise it
                    // vanishes for the whole drag. The dead skip is removed.
                    // G2: a body with a populated GPU-resident slot draws from that slot via
                    // draw_indexed_indirect — bypass the display_state gate (the slot's existence IS proof
                    // of a completed bake) and the mesh_cache presence check (resident mode never inserts
                    // into mesh_cache). The body keeps its mesh_cache_id (used only as the tuple's cid here).
                    // GATE 4: THE facade answers has-mesh/resident/pooled in one call (wraps the
                    // GATE 3 route + slot/pool/mesh_cache state) — lockstep with pick by construction.
                    let pres = self.sdf_mesh_presentation(nid, *mesh_cache_id);
                    let is_resident = pres.is_resident();
                    // GATE 4: presentation is the sole drawable verdict. CachedCpu (mesh_cache
                    // has a GPU mesh) must NOT re-gate on display_state — field-direct Resident
                    // bodies often sit in NeverBaked/TransformSafe while mesh_cache is already
                    // full (PT snapshot / readback). Requiring Raster here made Hybrid draw
                    // ZERO solids whenever presentation preferred CachedCpu (AZRA_VISDIFF M0:
                    // maps applied, mesh idx≈93k, empty viewport).
                    let draw_cid = if is_resident {
                        *mesh_cache_id
                    } else if matches!(pres, crate::mesh_presentation::SdfMeshPresentation::CachedCpu) {
                        *mesh_cache_id
                    } else if self.cache.use_body_vol_cache {
                        let display = self.cache.body_vol_cache.display_state(nid);
                        match display {
                            BodyDisplayState::Raster | BodyDisplayState::RasterOldCache | BodyDisplayState::TransformSafe => {
                                // Clean, stale, or transform-safe — draw mesh
                                // (TransformSafe: mesh valid, instance matrix has updated origin)
                                *mesh_cache_id
                            }
                            _ => {
                                // Non-active body: skip (never-baked → invisible until first bake).
                                // Active body during idle: also skip — will render analytically
                                // if no mesh exists (cached_all_meshes_ready=false → use_lite=false).
                                continue;
                            }
                        }
                    } else {
                        // Volume bake disabled — rasterize if mesh cache exists
                        *mesh_cache_id
                    };
                    let Some(cid) = draw_cid else { continue; };
                    if !is_resident && !pres.has_any_mesh() { continue; } // GATE 4: was mesh_cache.get(cid).is_none()
                    any_sdf_rasterized = true;
                    // GPU_CURVATURE_KERNEL_PLAN.md P2/P3: this body's instance reads the vertex-
                    // curvature carry (group 2) ONLY when a buffer for it is currently resident
                    // AND matches this mesh's CURRENT generation/vert_cap — a stale buffer (mesh
                    // re-baked since the curvature dispatch) must not be sampled, so the flag is
                    // gated on the SAME generation check `poll_vertex_curvature` uses to decide
                    // whether to re-dispatch, never merely "an entry exists". The channel-select
                    // bits forward the Generator layer's OWN output choice (`CurvVertexSlot::
                    // out_channel`: 1=signed/default, 2=convex, 3=concave) so the fragment shader
                    // samples the channel the panel actually asked for.
                    let (curv_bit, curv_params) = match self.body_has_current_vertex_curvature(nid) {
                        Some((2, inv, p, blend, as_mask)) => (
                            render::mesh_gpu::INSTANCE_FLAG_VERTEX_CURVATURE
                                | render::mesh_gpu::INSTANCE_FLAG_VERTEX_CURV_CONVEX
                                | if inv {
                                    render::mesh_gpu::INSTANCE_FLAG_VERTEX_CURV_INVERT
                                } else {
                                    0
                                }
                                | if as_mask {
                                    render::mesh_gpu::INSTANCE_FLAG_VERTEX_CURV_AS_MASK
                                } else {
                                    0
                                }
                                | render::mesh_gpu::pack_vertex_curv_blend(blend),
                            p,
                        ),
                        Some((3, inv, p, blend, as_mask)) => (
                            render::mesh_gpu::INSTANCE_FLAG_VERTEX_CURVATURE
                                | render::mesh_gpu::INSTANCE_FLAG_VERTEX_CURV_CONCAVE
                                | if inv {
                                    render::mesh_gpu::INSTANCE_FLAG_VERTEX_CURV_INVERT
                                } else {
                                    0
                                }
                                | if as_mask {
                                    render::mesh_gpu::INSTANCE_FLAG_VERTEX_CURV_AS_MASK
                                } else {
                                    0
                                }
                                | render::mesh_gpu::pack_vertex_curv_blend(blend),
                            p,
                        ),
                        Some((_, inv, p, blend, as_mask)) => (
                            render::mesh_gpu::INSTANCE_FLAG_VERTEX_CURVATURE
                                | if inv {
                                    render::mesh_gpu::INSTANCE_FLAG_VERTEX_CURV_INVERT
                                } else {
                                    0
                                }
                                | if as_mask {
                                    render::mesh_gpu::INSTANCE_FLAG_VERTEX_CURV_AS_MASK
                                } else {
                                    0
                                }
                                | render::mesh_gpu::pack_vertex_curv_blend(blend),
                            p,
                        ),
                        None => (0, [0.0, 1.0, 1.0, 1.0]),
                    };
                    let mat_idx = materials.len() as u32;
                    // Use the last additive (union) prim's material for the raster mesh:
                    // in a CSG stack the last union prim is typically the dominant visible
                    // surface contributor. Fall back to body-level material if no additive
                    // prims exist, or first prim as last resort.
                    // Resolve through the CANONICAL graph resolver (material_id → Material node,
                    // else embedded) — reading `p.material` directly here ignored assigned scene
                    // materials, so the raster/PBR surface never showed the user's material.
                    let pbr = primitives.iter().rev()
                        .find(|p| p.operation.is_additive())
                        .or(primitives.last())
                        .map(|p| self.scene.scene_graph.resolve_prim_material(p).clone())
                        .unwrap_or_else(|| body_mat.clone());
                    materials.push(RasterMaterial::from_pbr(&pbr));
                    // DC mesh vertices are in body-local space. The instance transform
                    // places the mesh at world position = body.origin with body_rotation
                    // and body_scale applied so rotation/scale changes take effect via
                    // the instance matrix alone (no remesh needed).
                    let instance = if BODY_LOCAL_PRIMS {
                        // Build TRS matrix directly from body_rotation (Quat) + body_scale, COMPOSED with
                        // any parent SdfObject's placement via effective_body_transform (top-level body =
                        // identity object → unchanged). RP6: shared normal-matrix via from_model_matrix.
                        let mut model = self.scene.scene_graph.effective_body_transform(nid)
                            .unwrap_or_else(|| glam::Mat4::from_scale_rotation_translation(*body_scale, *body_rotation, *origin));
                        // P1-#6b rigid-translate re-anchor (EXPLICIT): a rigid prim drag moves the
                        // grid bounds WITHOUT re-meshing, so the resident mesh pre-translates by
                        // the cache's accumulated rigid offset (zeroed by the next mesh at current
                        // bounds). NEVER derived from bounds deltas — escape-grow and settle-
                        // retighten move bounds without moving geometry, and the old derivation
                        // shifted the drawn mesh by those bogus deltas (the "sphere detaches from
                        // the gizmo at 128" regression).
                        // Rigid re-anchor applies to body-local display mesh (resident OR
                        // CachedCpu mesh-mod fold). Pick already always applies this offset;
                        // skipping it for CachedCpu desynced click vs drawn surface.
                        {
                            let off = self.cache.body_vol_cache.mesh_rigid_offset(nid);
                            if off.length_squared() > 0.0 {
                                model *= glam::Mat4::from_translation(off);
                            }
                        }
                        InstanceData::from_model_matrix(model, mat_idx | sel_bit | ghost_bit, bi)
                            .with_flags(curv_bit)
                            .with_curv_params(curv_params)
                    } else {
                        // Legacy: translation only
                        InstanceData::from_transform(
                            *origin,
                            glam::Vec3::ZERO,
                            glam::Vec3::ONE,
                            mat_idx | sel_bit | ghost_bit,
                            bi,
                        )
                        .with_flags(curv_bit)
                        .with_curv_params(curv_params)
                    };
                    let (kind, payload) = if is_resident {
                        // Stage B-1 pooled predicate — GATE 4: from the facade (same call pick uses).
                        (
                            if pres.is_pooled() {
                                DRAW_KIND_SDF_POOLED
                            } else {
                                DRAW_KIND_SDF_RESIDENT
                            },
                            nid.0,
                        )
                    } else {
                        (DRAW_KIND_MESH, 0)
                    };
                    draw_calls.push((cid, instance, 0u8, None));
                    draw_kind.push(kind);
                    draw_kind_payload.push(payload);
                }
                NodePayload::ImportedMesh { mesh_cache_id, materials: mats, .. } => {
                    // P-B: the voxel sculpt object renders as a grid of per-chunk meshes (only
                    // the chunk(s) a brush touched are re-meshed/re-uploaded). Other imported
                    // meshes keep the single-mesh path.
                    let is_voxel = self.sculpt.voxel.as_ref().map_or(false, |v| v.node == nid && !v.chunks.is_empty());
                    if !is_voxel && self.cache.mesh_cache.get(*mesh_cache_id).is_none() { continue; }
                    let mat_idx = materials.len() as u32;
                    let pbr = mats.first().cloned().unwrap_or_default();
                    materials.push(RasterMaterial::from_pbr(&pbr));
                    // RP6: build instance straight from the world matrix — no lossy round-trip.
                    let model = self.scene.scene_graph.world_transform(nid);
                    // MOVE GRAB: tag the grabbed voxel body so vs_main displaces its verts (live preview).
                    // Also tagged when DEFERRED moves are pending (surface mode) so the accumulated moves
                    // keep displaying after release until a Voxelize flush bakes them into the grid.
                    let grab_on = is_voxel && self.sculpt.voxel.as_ref()
                        .map_or(false, |v| v.mesh_grab.is_active() || !v.pending_move_grabs.is_empty());
                    // Paint + mask tint: UV slot holds packed colour words (INSTANCE_FLAG_VERTEX_COLOR).
                    let painted = is_voxel && self.sculpt.voxel.as_ref().map_or(false, |v| {
                        !v.grid.colors.is_empty() || v.grid.mask_visible()
                    });
                    let vflags = (if painted { render::mesh_gpu::INSTANCE_FLAG_VERTEX_COLOR } else { 0 })
                        | (if grab_on { render::mesh_gpu::INSTANCE_FLAG_GRAB_TARGET } else { 0 });
                    let instance = InstanceData::from_model_matrix(model, mat_idx | sel_bit, bi)
                        .with_flags(vflags);
                    if is_voxel {
                        // PLAN B: a SPARSE sculpt is displayed by the RAYMARCH (viewport_render), so add NO
                        // voxel chunk draws to the G-buffer/shadow list — neither resident NOR the stale
                        // not-yet-remeshed CPU-fallback chunks (those were the dark speckled patches showing
                        // THROUGH the raymarch while the mesh rebuilds). The mesh still feeds the separate
                        // PICKING pass + export. Dense (<256³) sculpts have no raymarch → keep drawing.
                        if !sculpt_raymarch_overlay {
                            if let Some(v) = self.sculpt.voxel.as_ref() {
                                // PAINT EXCEPTION: a resident GPU chunk buffer carries NO per-voxel colour
                                // (the GPU mesher emits none), so while colour is in play prefer the CPU
                                // mesh (which packs the painted colour the INSTANCE_FLAG_VERTEX_COLOR draw
                                // reads). The CPU mesher is force-run for this body in voxel_flush_dirty_chunks.
                                // MOVE GRAB: the live preview is the G-buffer vertex-shader displacement of the
                                // resident pool (see INSTANCE_FLAG_GRAB_TARGET + CameraUniforms.grab_*), so a
                                // grabbed body keeps drawing from the pool — NOT a CPU mesh. The only remaining
                                // PER-CHUNK gate is PAINT: a resident GPU chunk carries no per-voxel colour, so
                                // while colour is in play the body force-takes its CPU mesh (cpu_paint_path()).
                                let resident_base = v.gpu_resident_mesh && !v.cpu_paint_path();
                                for (i, &cid) in v.chunks.iter().enumerate() {
                                    // MOVE GRAB: chunks STAY resident through a grab. The live preview is the
                                    // G-buffer vertex-shader displacement of THIS exact pool mesh (gated by the
                                    // body's INSTANCE_FLAG_GRAB_TARGET + CameraUniforms.grab_*), NOT a CPU mesh
                                    // swap — so there is no per-chunk gate / vanish / stale-CPU-mesh failure mode.
                                    if crate::raster_common::voxel_chunk_draws_from_pool(self.gpu.as_ref(), resident_base, i) {
                                        // GPU-resident chunk (Phase 6): draw its persistent buffers via indirect
                                        // (chunk_tris is stale here; empty self-skips via GPU index_count=0).
                                        draw_calls.push((cid, instance, 0u8, None));
                                        draw_kind.push(DRAW_KIND_VOXEL_RESIDENT);
                                        draw_kind_payload.push(i as u64);
                                    } else if v.chunk_tris.get(i).copied().unwrap_or(0) > 0 {
                                        // Not-yet-resident chunk (incl. the whole surface before it's been
                                        // sculpted, and every chunk right after a resolution change) → its
                                        // existing CPU mesh. Without this the un-sculpted surface vanishes.
                                        draw_calls.push((cid, instance, 0u8, None));
                                        draw_kind.push(DRAW_KIND_MESH);
                                        draw_kind_payload.push(0);
                                    }
                                }
                            }
                        }
                    } else {
                        draw_calls.push((*mesh_cache_id, instance, 0u8, None));
                        draw_kind.push(DRAW_KIND_MESH);
                        draw_kind_payload.push(0);
                    }
                }
                NodePayload::VoxelBody { mesh_cache_id, material, .. } => {
                    // P0.5: voxel sculpt is a first-class node. The ACTIVE sculpt renders as per-chunk
                    // meshes (mirrors the old ImportedMesh voxel path — detection is still
                    // `self.sculpt.voxel.node == nid`); a non-active VoxelBody renders its cached display mesh.
                    let is_voxel = self.sculpt.voxel.as_ref().map_or(false, |v| v.node == nid && !v.chunks.is_empty());
                    let mat_idx = materials.len() as u32;
                    materials.push(RasterMaterial::from_pbr(material));
                    let model = self.scene.scene_graph.world_transform(nid);
                    // Sculpt-vertex-paint: flag the draw so the G-buffer shader reads the chunk mesh's UV
                    // slot as a packed paint colour (set ONLY when this voxel grid actually has painted
                    // voxels — every other mesh keeps real UVs). Active sculpt reads its live grid; a
                    // non-active VoxelBody reads the node grid.
                    let painted = if is_voxel {
                        self.sculpt.voxel.as_ref().map_or(false, |v| {
                            !v.grid.colors.is_empty() || v.grid.mask_visible()
                        })
                    } else if let NodePayload::VoxelBody { grid, .. } = &node.payload {
                        !grid.colors.is_empty() || grid.mask_visible()
                    } else { false };
                    // MOVE GRAB: tag the grabbed voxel body so vs_main displaces its verts (live preview).
                    // Also tagged when DEFERRED moves are pending (surface mode) so the accumulated moves
                    // keep displaying after release until a Voxelize flush bakes them into the grid.
                    let grab_on = is_voxel && self.sculpt.voxel.as_ref()
                        .map_or(false, |v| v.mesh_grab.is_active() || !v.pending_move_grabs.is_empty());
                    let vflags = (if painted { render::mesh_gpu::INSTANCE_FLAG_VERTEX_COLOR } else { 0 })
                        | (if grab_on { render::mesh_gpu::INSTANCE_FLAG_GRAB_TARGET } else { 0 });
                    let instance = InstanceData::from_model_matrix(model, mat_idx | sel_bit, bi).with_flags(vflags);
                    if is_voxel {
                        // Sparse → the raymarch overlay displays it; add no chunk draws (see ImportedMesh path).
                        if !sculpt_raymarch_overlay {
                            if let Some(v) = self.sculpt.voxel.as_ref() {
                                // PAINT EXCEPTION: a resident GPU chunk buffer carries NO per-voxel colour
                                // (the GPU mesher emits none), so while colour is in play prefer the CPU
                                // mesh (which packs the painted colour the INSTANCE_FLAG_VERTEX_COLOR draw
                                // reads). The CPU mesher is force-run for this body in voxel_flush_dirty_chunks.
                                // MOVE GRAB: the live preview is the G-buffer vertex-shader displacement of the
                                // resident pool (see INSTANCE_FLAG_GRAB_TARGET + CameraUniforms.grab_*), so a
                                // grabbed body keeps drawing from the pool — NOT a CPU mesh. The only remaining
                                // PER-CHUNK gate is PAINT: a resident GPU chunk carries no per-voxel colour, so
                                // while colour is in play the body force-takes its CPU mesh (cpu_paint_path()).
                                let resident_base = v.gpu_resident_mesh && !v.cpu_paint_path();
                                for (i, &cid) in v.chunks.iter().enumerate() {
                                    // MOVE GRAB: chunks STAY resident through a grab. The live preview is the
                                    // G-buffer vertex-shader displacement of THIS exact pool mesh (gated by the
                                    // body's INSTANCE_FLAG_GRAB_TARGET + CameraUniforms.grab_*), NOT a CPU mesh
                                    // swap — so there is no per-chunk gate / vanish / stale-CPU-mesh failure mode.
                                    if crate::raster_common::voxel_chunk_draws_from_pool(self.gpu.as_ref(), resident_base, i) {
                                        draw_calls.push((cid, instance, 0u8, None));
                                        draw_kind.push(DRAW_KIND_VOXEL_RESIDENT);
                                        draw_kind_payload.push(i as u64);
                                    } else if v.chunk_tris.get(i).copied().unwrap_or(0) > 0 {
                                        draw_calls.push((cid, instance, 0u8, None));
                                        draw_kind.push(DRAW_KIND_MESH);
                                        draw_kind_payload.push(0);
                                    }
                                }
                            }
                        }
                    } else if let Some(cid) = mesh_cache_id {
                        if self.cache.mesh_cache.get(*cid).is_some() {
                            draw_calls.push((*cid, instance, 0u8, None));
                            draw_kind.push(DRAW_KIND_MESH);
                            draw_kind_payload.push(0);
                        }
                    }
                }
                NodePayload::PolygonalBody {
                    mesh_cache_id: Some(cid),
                    material,
                    mesh,
                    modifiers,
                    ..
                } => {
                    let source_stamp = self.cache.mesh_cache.get(*cid).map_or(0, |mesh| mesh.content_stamp);
                    if let Some((atlas, pbr)) = self.gpu.as_ref().and_then(|gpu| gpu.mat_bake_pipe_cache.poly_curvature.override_for(nid, *cid, source_stamp)) {
                        let mat_idx = push_mat(&mut materials, RasterMaterial::from_pbr(pbr));
                        let model = self.scene.scene_graph.world_transform(nid);
                        draw_calls.push((atlas, InstanceData::from_model_matrix(model, mat_idx, bi), 0u8, None));
                        draw_kind.push(DRAW_KIND_MESH);
                        draw_kind_payload.push(0);
                        continue;
                    }
                    // PLAN_FOR_GROK_VIEWPORT_PERF P3: object-mode display LOD may swap cid.
                    let mut draw_cid = self.poly_draw_mesh_cache_id(nid, *cid);
                    // P4: share draw MeshCacheId across identical content+LOD so instance
                    // batching merges Shift+D / array-of-bodies that share topology.
                    // C-scale: empty modifier stack → MeshCacheId already identities content
                    // (B.4 share). Skip content_signature over the half-edge graph (was ~O(N)
                    // full mesh hash per poly body every frame at 10k).
                    let lod_level = self.poly_lod_cache.get(&nid).map(|s| s.level).unwrap_or(0);
                    let share_key = if modifiers.is_empty() {
                        (cid.0, 0u64, lod_level)
                    } else {
                        let content_key = self.poly_display_cache_key(nid, mesh, modifiers);
                        (content_key.0, content_key.1, lod_level)
                    };
                    draw_cid = *poly_draw_share.entry(share_key).or_insert(draw_cid);
                    let Some(cached) = self.cache.mesh_cache.get(draw_cid) else { continue };
                    let model = self.scene.scene_graph.world_transform(nid);
                    // Object-mode GPU selection outline (owner 2026-08-06): poly bodies OR
                    // sel_bit like SDF/NURBS so selected objects get the standard DCC green
                    // rim (Blender-style). Edit mode (V/E/F) still suppresses THIS body via
                    // `outline_suppressed_edit_subject` → no body rim while component
                    // highlights carry state. 2026-07-16 POLY-RIM DROP was aimed at grazing
                    // flare mis-attributed to object rim; sub-element feedback is separate.
                    // Multi-material: one draw per contiguous index range (sorted face mats).
                    // Mono: one draw with body-level resolve (first-face / embedded).
                    // LOD meshes are mono-material (decimate collapses ranges) — full path uses ranges.
                    if draw_cid == *cid {
                        if let Some(ranges) = cached.material_ranges.as_ref().filter(|r| r.len() > 1) {
                            for &(mat_id, first, count) in ranges {
                                if count == 0 {
                                    continue;
                                }
                                // Hidden Material nodes (outliner eye) fall back to body embedded.
                                let pbr = if mat_id > 0 {
                                    self.scene
                                        .scene_graph
                                        .material_data_if_visible(core_math::primitives::MaterialId(mat_id))
                                        .unwrap_or(material)
                                } else {
                                    material
                                };
                                let mat_idx =
                                    push_mat(&mut materials, RasterMaterial::from_pbr(pbr));
                                let instance = InstanceData::from_model_matrix(
                                    model,
                                    mat_idx | sel_bit,
                                    bi,
                                );
                                draw_calls.push((draw_cid, instance, 0u8, Some((first, count))));
                                draw_kind.push(DRAW_KIND_MESH);
                                draw_kind_payload.push(0);
                            }
                            continue;
                        }
                    }
                    // Mono path: resolve face/body material, then content-key into mat_share.
                    // Clones of the same mat collapse to one slot (10k win); distinct mats
                    // (red vs blue cube) get distinct slots — never hardcode slot 0.
                    let pbr = self.scene.scene_graph.resolve_poly_body_material(mesh, material);
                    let mat_idx = push_mat(&mut materials, RasterMaterial::from_pbr(pbr));
                    let instance =
                        InstanceData::from_model_matrix(model, mat_idx | sel_bit, bi);
                    draw_calls.push((draw_cid, instance, 0u8, None));
                    draw_kind.push(DRAW_KIND_MESH);
                    draw_kind_payload.push(0);
                }
                NodePayload::NurbsBody {
                    mesh_cache_id: Some(cid),
                    material,
                    ..
                }
                | NodePayload::NurbsSolid {
                    mesh_cache_id: Some(cid),
                    material,
                    ..
                } => {
                    if self.cache.mesh_cache.get(*cid).is_none() {
                        continue;
                    }
                    let mat_idx = materials.len() as u32;
                    materials.push(RasterMaterial::from_pbr(material));
                    // RP6: build instance straight from the world matrix — no lossy
                    // Mat4 → SRT → euler-deg → Quat → Mat4 round-trip.
                    let model = self.scene.scene_graph.world_transform(nid);
                    let instance = InstanceData::from_model_matrix(model, mat_idx | sel_bit, bi);
                    draw_calls.push((*cid, instance, 0u8, None));
                    draw_kind.push(DRAW_KIND_MESH);
                    draw_kind_payload.push(0);
                }
                _ => {}
            }
            } // for nid in stream_nids

            // C.6: sort once at structural rebuild — (special, cid, irange).
            // Cull is always 0 here; visible-before-culled is a no-op at rebuild time.
            // Camera frames must NOT re-sort (that would reorder InstanceData → force re-upload
            // and defeat the structural cache). Frustum holes may split same-cid runs; that is
            // accepted — the win is no gather/sort/hash on orbit.
            // C.6 sabotage: AZRA_NO_DRAW_SORT=1 skips sort at rebuild.
            let skip_draw_sort = std::env::var_os("AZRA_NO_DRAW_SORT").is_some();
            if !skip_draw_sort {
                let n = draw_calls.len();
                let mut order: Vec<usize> = (0..n).collect();
                order.sort_by(|&a, &b| {
                    let sa = draw_kind_is_special(draw_kind[a]);
                    let sb = draw_kind_is_special(draw_kind[b]);
                    match (sa, sb) {
                        (true, false) => std::cmp::Ordering::Less,
                        (false, true) => std::cmp::Ordering::Greater,
                        (true, true) => a.cmp(&b),
                        (false, false) => {
                            let (ca, _, _, ra) = &draw_calls[a];
                            let (cb, _, _, rb) = &draw_calls[b];
                            ca.0.cmp(&cb.0)
                                .then_with(|| ra.cmp(rb))
                                .then_with(|| a.cmp(&b))
                        }
                    }
                });
                if order.iter().enumerate().any(|(i, &o)| i != o) {
                    let old_draws = draw_calls.clone();
                    let old_kind = std::mem::take(&mut draw_kind);
                    let old_payload = std::mem::take(&mut draw_kind_payload);
                    draw_calls.clear();
                    draw_kind.clear();
                    draw_kind_payload.clear();
                    for &old_i in &order {
                        draw_calls.push(old_draws[old_i]);
                        draw_kind.push(old_kind[old_i]);
                        draw_kind_payload.push(old_payload[old_i]);
                    }
                }
            }

            self.structural_draw_stale = false;
            self.structural_material_gen = mat_gen;
            self.structural_outline_key = outline_key;
            self.structural_any_sdf = any_sdf_rasterized;

            // D1 prep: cache world AABBs with the structural list (once per SCENE/MESH).
            // Camera frames only plane-test these — no 8-corner transforms × N.
            world_aabbs.clear();
            world_aabbs.reserve(draw_calls.len());
            for (idx, (cid, inst, _, _)) in draw_calls.iter().enumerate() {
                if draw_kind_is_special(draw_kind[idx]) {
                    world_aabbs.push((glam::Vec3::ZERO, glam::Vec3::ZERO));
                    continue;
                }
                if let Some(cached) = self.cache.mesh_cache.get(*cid) {
                    let model = glam::Mat4::from_cols_array_2d(&inst.model_matrix);
                    world_aabbs.push(world_aabb(&model, cached.local_min, cached.local_max));
                } else {
                    world_aabbs.push((glam::Vec3::ZERO, glam::Vec3::ZERO));
                }
            }
        } // end structural rebuild

        // Safety: camera reuse path must keep AABB parity (structural rebuild always refills).
        if world_aabbs.len() != draw_calls.len() {
            world_aabbs.clear();
            world_aabbs.resize(draw_calls.len(), (glam::Vec3::ZERO, glam::Vec3::ZERO));
        }

        // Phase 2: GPU operations — always run (even with no draw calls) to clear
        // the G-buffer. Skipping the clear leaves stale raster data from previous
        // frames, causing the lite/full march shaders to early-exit on ghost pixels.
        //
        // MOVE-GRAB uniform: PER-INSTANCE WORLD-space anchors/drags (+ radii in .w), fed to the G-buffer
        // vertex shader so it displaces the grabbed body's resident-pool verts (the live preview).
        // Computed HERE — reading self.sculpt.voxel + self.scene — BEFORE the `&mut self.gpu` borrow below. The grab
        // stores anchor/drag/radii in voxel-LOCAL space; the body's world transform maps them to world (the
        // space vs_main displaces in). Radii scale by the body's uniform scale (voxel bodies are uniform-scaled).
        //
        // SURFACE MODE (deferred voxelize): the combined instance list = the ACTIVE grab's symmetry octants
        // (`mirror_instances`, with the active grab's radii) FOLLOWED BY every accumulated `pending_move_grabs`
        // entry (each with its OWN radii). The shader SUMS them, so a deferred Move release keeps displaying
        // (pending) AND multiple moves accumulate, with no re-voxelize until a Voxelize flush. PER-INSTANCE
        // radii (packed anchors[i].w = inner_r, drags[i].w = outer_r) let moves at different brush sizes
        // compose correctly. Capped at MAX_GRAB_INSTANCES (overflow is flushed before push — see the release).
        const MAX_GI: usize = render::camera::MAX_GRAB_INSTANCES;
        let grab_uniform: Option<([f32; 4], [[f32; 4]; MAX_GI], [[f32; 4]; MAX_GI])> = self.sculpt.voxel.as_ref().and_then(|v| {
            if !v.mesh_grab.is_active() && v.pending_move_grabs.is_empty() { return None; }
            let model = self.scene.scene_graph.world_transform(v.node);
            let scale = model.x_axis.truncate().length().max(1e-6);
            let mut anchors = [[0.0f32; 4]; MAX_GI];
            let mut drags = [[0.0f32; 4]; MAX_GI];
            let mut n = 0usize;
            // Local closure: pack ONE (local anchor, local drag, inner_r, outer_r) into world-space slot `n`.
            let mut push = |anchor: glam::Vec3, drag: glam::Vec3, inner_r: f32, outer_r: f32| {
                if n >= MAX_GI { return; }
                let wa = model.transform_point3(anchor);
                let wd = model.transform_vector3(drag);
                anchors[n] = [wa.x, wa.y, wa.z, inner_r * scale];
                drags[n] = [wd.x, wd.y, wd.z, outer_r * scale];
                n += 1;
            };
            // ACTIVE grab first (symmetry octants). n_active = count for mid-plane normalize in WGSL.
            let mut n_active = 0usize;
            if v.mesh_grab.is_active() {
                let g = &v.mesh_grab;
                for (a, d) in g.mirror_instances(v.symmetry) {
                    push(a, d, g.inner_r, g.outer_r);
                    n_active += 1;
                }
            }
            // Deferred moves (already symmetry-expanded); raw-summed after n_active in the shader.
            for pg in &v.pending_move_grabs { push(pg.anchor, pg.drag, pg.inner_r, pg.outer_r); }
            if n == 0 { return None; }
            // .z = total instances, .w = active stroke symmetry count (for mid-plane blend).
            Some(([0.0, 0.0, n as f32, n_active as f32], anchors, drags))
        });
        // Clipmap-blocked cids once per frame (before &mut gpu borrow — remap/dump need it).
        let clusters_on = self.render.dbg_clusters;
        let cluster_blocked = if clusters_on {
            self.cluster_blocked_cids()
        } else {
            std::collections::HashSet::new()
        };

        let Some(gpu) = &mut self.gpu else {
            // Put structural cache back before early-out (reuse path took it).
            self.structural_materials = materials;
            self.structural_draw_calls = draw_calls;
            self.structural_draw_kind = draw_kind;
            self.structural_draw_kind_payload = draw_kind_payload;
            self.structural_world_aabb = world_aabbs;
            return any_sdf_rasterized;
        };
        let device = gpu.device.clone();
        let queue = gpu.queue.clone();

        // Use the actual G-buffer texture size for the depth buffer to avoid
        // attachment size mismatch when viewport is resizing (G-buffers may
        // not have been recreated yet this frame).
        let (width, height) = if let Some(ref gn) = gpu.gbuf_normal {
            let s = gn.size();
            (s.width, s.height)
        } else {
            // G-buffers don't exist yet — skip this frame; they'll be created
            // by the SDF pipeline setup and we'll render next frame.
            self.structural_materials = materials;
            self.structural_draw_calls = draw_calls;
            self.structural_draw_kind = draw_kind;
            self.structural_draw_kind_payload = draw_kind_payload;
            self.structural_world_aabb = world_aabbs;
            return any_sdf_rasterized;
        };

        // Ensure depth buffer matches G-buffer size
        gpu.raster.ensure_depth(&device, width, height);

        // Phase B1/B2: probe mini-frames render a top-left s×s sub-rect (viewport/
        // scissor + matching camera aspect). 0 = main view (full G-buffer).
        let is_probe = self.probe_frame_side > 0;
        let probe_s = if is_probe {
            self.probe_frame_side.min(width).min(height).max(1)
        } else {
            0
        };
        let (cam_w, cam_h) = if is_probe {
            (probe_s, probe_s)
        } else {
            (width, height)
        };

        // Camera — clip planes scale with distance (ortho zoom-close must not eat geometry).
        let (near, far) =
            render::camera::orbit_clip_planes(self.viewport.camera.distance, self.render.ortho);
        let mut camera = CameraUniforms::from_orbit(
            self.viewport.camera.orbit, self.viewport.camera.distance, self.viewport.camera.target,
            cam_w as f32 / cam_h.max(1) as f32, self.render.fov_deg, near, far, self.render.ortho,
        );
        if let Some((gr, ga, gd)) = grab_uniform {
            camera.grab_radii = gr;
            camera.grab_anchors = ga;
            camera.grab_drags = gd;
        }
        // GPU DC-wire settings → camera (fs_gbuffer_wire + deferred dilation).
        // Color is used as-is (default black). Thickness in _pad.x, opacity in wire_color.a.
        let [wr, wg, wb] = self.render.wire_overlay_color;
        let wop = self.render.wire_overlay_opacity.clamp(0.0, 1.0);
        let wthick = self.render.wire_overlay_thickness.max(0.0);
        camera.wire_color = [wr, wg, wb, wop];
        // _pad.y = cluster debug view flag (gbuffer.wgsl palette branch).
        camera._pad = [wthick, if self.render.dbg_cluster_view { 1.0 } else { 0.0 }];
        // Probe capture frame → fs_gbuffer marks back-facing fragments in the body_index
        // target (probe-validity measurement, AUTO-PROBE-INSIDE-GEOMETRY).
        camera.frame_flags[0] = if is_probe { 1.0 } else { 0.0 };
        // Progressive PBR refine sub-pixel jitter (PROGRESSIVE_PBR_PLAN.md Phase 1):
        // translate clip space by the NDC jitter — every compute camera_ray()
        // subtracts the same offset (GpuUniforms.pbr_jitter), so the jittered
        // G-buffer and ray reconstruction agree. Zero when not accumulating, and
        // the un-jittered copy feeds overlay passes so wires never shimmer over
        // the stable accumulated image.
        let camera_nojitter = camera;
        let [jx, jy] = self.render.pbr_refine_jitter_ndc;
        if jx != 0.0 || jy != 0.0 {
            let j = glam::Mat4::from_translation(glam::Vec3::new(jx, jy, 0.0));
            let proj = glam::Mat4::from_cols_array_2d(&camera.proj);
            let vp = glam::Mat4::from_cols_array_2d(&camera.view_proj);
            camera.proj = (j * proj).to_cols_array_2d();
            camera.view_proj = (j * vp).to_cols_array_2d();
        }
        gpu.raster.upload_camera(&queue, &camera, &camera_nojitter);

        // A1 / B.3 / C.2 frustum culling. Fully outside frustum → DRAW_CULL_FRUSTUM (skip G-buffer
        // AND shadow cascades — no cascade tax). Resident voxel/SDF pool draws skipped here.
        // D1 prep: plane-test structural_world_aabb (no per-frame model×8-corner). Mid-band BVH
        // still used for 8..512; larger N uses parallel linear over cached AABBs (BVH rebuild
        // cost dominated at 10k — C.1 measure; GPU cull is full D1).
        // D4: remap MESH → CLUSTERED only for (cid, instance) pairs that the cut
        // will actually process this frame (stable top-8 cids × first 4 instances).
        // Overflow stays MESH — never special-without-draw (WP1 invisible-geometry fix).
        // WP11: args overflow → MESH fallback (never silent interior holes from cap drop).
        // Applied every frame so demotion is immediate even between structural rebuilds.
        //
        // Cluster debug view: never demote — palette needs the CLUSTERED multi_draw path
        // (production MESH verts have uv.y=0 and never take the gbuffer palette branch).
        let mesh_fb = self.render.cluster_overflow_mesh_fallback;
        let debug_view = self.render.dbg_cluster_view;
        if mesh_fb && !debug_view {
            for k in &mut draw_kind {
                if *k == crate::raster_common::DRAW_KIND_CLUSTERED {
                    *k = DRAW_KIND_MESH;
                }
            }
        }

        // WP13: poly Edit on the active body → force that mesh cid to source MESH
        // (same-frame continuous mesh; no dual cluster+wire; full recluster is D8).
        let edit_mesh_cids: std::collections::HashSet<core_math::scene_graph::MeshCacheId> = {
            let mut set = std::collections::HashSet::new();
            if self.modeling_context == crate::ModelingContext::Polygonal
                && self.poly_work_mode != crate::PolyWorkMode::Object
            {
                let active = self.scene.active_body_nid;
                if let Some(node) = self.scene.scene_graph.get(active) {
                    if let core_math::scene_graph::NodePayload::PolygonalBody {
                        mesh_cache_id: Some(c),
                        ..
                    } = &node.payload
                    {
                        set.insert(*c);
                    }
                }
            }
            set
        };
        if !edit_mesh_cids.is_empty() {
            for (i, (cid, _, _, _)) in draw_calls.iter().enumerate() {
                if edit_mesh_cids.contains(cid)
                    && draw_kind[i] == crate::raster_common::DRAW_KIND_CLUSTERED
                {
                    draw_kind[i] = DRAW_KIND_MESH;
                }
            }
        }

        // Stamp-fresh only: pool.entry.stamp must match mesh_cache (WP3).
        // Remap runs **every frame** when clusters can draw — not only structural rebuilds.
        // Rebuild-only remap left sticky MESH after WP11 fallback cleared (demote every
        // frame while mesh_fb; re-promote never ran until an unrelated rebuild).
        // `cluster_blocked` was built once above (Frame Budget: not per-cid all_nodes).
        // Debug view forces the CLUSTERED route even under mesh_fb (diagnostic override).
        if clusters_on
            && (!mesh_fb || debug_view)
            && !gpu.cluster_pool.entries.is_empty()
        {
            // Same slot order as ClusterCutSystem::stable_cid_slots.
            let mut slotted_cids: Vec<core_math::scene_graph::MeshCacheId> =
                render::cluster_cut::ClusterCutSystem::stable_cid_slots(&gpu.cluster_pool)
                    .map(|(_, (cid, _))| cid)
                    .collect();
            slotted_cids.retain(|cid| {
                if cluster_blocked.contains(cid) || edit_mesh_cids.contains(cid) {
                    return false;
                }
                let Some(entry) = gpu.cluster_pool.entries.get(cid) else {
                    return false;
                };
                self.cache.mesh_cache.pt_stamp(*cid) == Some(entry.stamp)
            });
            let slotted: std::collections::HashSet<_> = slotted_cids.iter().copied().collect();
            // First MAX_INSTANCES_PER_CID draw indices per cid (draw list order =
            // instance buffer order under GPU cull — same as cut fill).
            let mut inst_taken: std::collections::HashMap<
                core_math::scene_graph::MeshCacheId,
                u32,
            > = std::collections::HashMap::new();
            let mut n = 0u32;
            let mut n_cid_skip = 0u32;
            let mut n_inst_skip = 0u32;
            let mut n_edit_skip = 0u32;
            for (i, (cid, _, _, _)) in draw_calls.iter().enumerate() {
                if draw_kind[i] != DRAW_KIND_MESH {
                    // Already CLUSTERED (or other special) — still count clustered slots.
                    if draw_kind[i] == crate::raster_common::DRAW_KIND_CLUSTERED {
                        let taken = inst_taken.entry(*cid).or_insert(0);
                        *taken = taken.saturating_add(1);
                        n += 1;
                    }
                    continue;
                }
                if edit_mesh_cids.contains(cid) {
                    n_edit_skip += 1;
                    continue; // WP13: edit-mode active mesh stays MESH
                }
                if !gpu.cluster_pool.entries.contains_key(cid) {
                    continue;
                }
                if !slotted.contains(cid) {
                    n_cid_skip += 1;
                    continue; // overflow cid → stay MESH
                }
                let taken = inst_taken.entry(*cid).or_insert(0);
                if (*taken as usize) >= render::cluster_cut::MAX_INSTANCES_PER_CID {
                    n_inst_skip += 1;
                    continue; // overflow instances → stay MESH
                }
                *taken += 1;
                draw_kind[i] = crate::raster_common::DRAW_KIND_CLUSTERED;
                n += 1;
            }
            // Throttle: only log when routing changes or every ~2s under AZRA_CLUSTER_LOG.
            {
                use std::cell::Cell;
                thread_local! {
                    static LAST_N: Cell<u32> = const { Cell::new(u32::MAX) };
                    static LAST_T: Cell<Option<std::time::Instant>> = const { Cell::new(None) };
                }
                let changed = LAST_N.with(|c| {
                    let prev = c.get();
                    c.set(n);
                    prev != n
                });
                let due = LAST_T.with(|c| {
                    let now = std::time::Instant::now();
                    let due = c.get().map_or(true, |t| now.duration_since(t).as_secs_f32() >= 2.0);
                    if due || changed {
                        c.set(Some(now));
                    }
                    due
                });
                if changed || (due && std::env::var("AZRA_CLUSTER_LOG").is_ok_and(|v| v == "1")) {
                    eprintln!(
                        "[ClusterDraw] routed={n} inst_overflow_mesh={n_inst_skip} \
                         cid_overflow_mesh={n_cid_skip} edit_mesh={n_edit_skip} \
                         mesh_fb={} dbg_view={}",
                        u32::from(mesh_fb),
                        u32::from(debug_view),
                    );
                }
            }
        }

        // D1 GPU cull latch — decided BEFORE any CPU per-instance work so the whole
        // frustum/occlusion/pack chain can be skipped when the GPU owns visibility.
        // (Shadow encode still reads cull bits until D1 commit 5; all-zero bits =
        // "everything casts", which is correct, just unculled.)
        let gpu_cull_wanted = gpu.raster_cull.is_some() && self.render.dbg_gpu_cull;
        let _t_frustum = std::time::Instant::now();
        if self.render.frustum_cull && !gpu_cull_wanted {
            let vp = glam::Mat4::from_cols_array_2d(&camera.view_proj);
            let planes = frustum_planes(&vp);
            let mut culled = 0u32;
            let n_draw = draw_calls.len();
            let use_bvh =
                n_draw >= FRUSTUM_BVH_MIN_DRAWS && n_draw <= FRUSTUM_BVH_MAX_DRAWS;
            if use_bvh {
                use core_math::bvh::{build_scene_bvh, Aabb, SceneBvhBody};
                let mut bodies: Vec<SceneBvhBody> = Vec::with_capacity(n_draw);
                for (idx, (_, _, cull, _)) in draw_calls.iter().enumerate() {
                    if draw_skip_main(*cull) || draw_kind_is_special(draw_kind[idx]) {
                        continue;
                    }
                    let (wmin, wmax) = world_aabbs[idx];
                    if wmin == wmax {
                        continue; // empty / missing mesh
                    }
                    bodies.push(SceneBvhBody {
                        aabb: Aabb {
                            min: wmin.to_array(),
                            max: wmax.to_array(),
                        },
                        start_entry: idx as u32,
                        entry_count: 1,
                    });
                }
                let nodes = build_scene_bvh(&bodies);
                let culled_idxs = scene_bvh_collect_frustum_culled(&nodes, &planes);
                for idx in culled_idxs {
                    let i = idx as usize;
                    if i < draw_calls.len() && !draw_skip_main(draw_calls[i].2) {
                        draw_calls[i].2 = DRAW_CULL_FRUSTUM; // C.2: skip main + shadows
                        culled += 1;
                    }
                }
            } else if n_draw > FRUSTUM_BVH_MAX_DRAWS {
                // Large instance forest: parallel plane tests over cached world AABBs.
                use rayon::prelude::*;
                let flags: Vec<bool> = (0..n_draw)
                    .into_par_iter()
                    .map(|idx| {
                        if draw_skip_main(draw_calls[idx].2) || draw_kind_is_special(draw_kind[idx])
                        {
                            return false;
                        }
                        let (wmin, wmax) = world_aabbs[idx];
                        if wmin == wmax {
                            return false;
                        }
                        aabb_outside_frustum(&planes, wmin, wmax)
                    })
                    .collect();
                for (idx, out) in flags.into_iter().enumerate() {
                    if out {
                        draw_calls[idx].2 = DRAW_CULL_FRUSTUM;
                        culled += 1;
                    }
                }
            } else {
                for (idx, (_, _, cull, _)) in draw_calls.iter_mut().enumerate() {
                    if draw_skip_main(*cull) || draw_kind_is_special(draw_kind[idx]) {
                        continue;
                    }
                    let (wmin, wmax) = world_aabbs[idx];
                    if wmin == wmax {
                        continue;
                    }
                    if aabb_outside_frustum(&planes, wmin, wmax) {
                        *cull = DRAW_CULL_FRUSTUM; // C.2: skip main + shadows
                        culled += 1;
                    }
                }
            }
            self.render.last_frustum_culled = culled;
            self.render.last_frustum_total = draw_calls.len() as u32;
        } else {
            self.render.last_frustum_culled = 0;
            self.render.last_frustum_total = draw_calls.len() as u32;
        }

        // PLAN_FOR_GROK_VIEWPORT_PERF P4: software hierarchical occlusion (AABB Hi-Z).
        // Front-to-back fill of a low-res depth buffer; fully covered → shadow-only.
        //
        // B4 fix: O(N) candidate walk is ~8–10 ms at 100k. When a full pass is **expensive
        // and buys little** (low yield, or heavy pass on a highly-batched instance list),
        // skip the walk for SKIP_FRAMES then re-probe. `AZRA_CPU_OCCLUSION=1` enables Hi-Z
        // and **does not** bypass the skip (feel-check is with the flag ON).
        // AZRA_NO_HIZ=1 forces off entirely.
        let hiz_env_off = std::env::var_os("AZRA_NO_HIZ").is_some();
        // D1: CPU occlusion marks MAIN bits, which the GPU-cull encode ignores entirely —
        // the O(N) walk would be pure waste. Occlusion returns as GPU hi-z in D2.
        // A/B the regression (interior scenes) with AZRA_NO_GPU_CULL=1.
        // B2: probe frames skip CPU Hi-Z — wrong camera would pollute the skip latch.
        let hiz_want = self.render.occlusion_cull && !hiz_env_off && !gpu_cull_wanted && !is_probe;
        if hiz_want {
            let _occ_t0 = std::time::Instant::now();
            // Low-yield / low-value latch.
            const LOW_YIELD_MAX: f32 = 0.02; // ≤2% culled → "buys almost nothing"
            const SKIP_FRAMES: u32 = 30;
            const MIN_TESTED_FOR_LATCH: u32 = 64;
            // Cost latch: full pass already expensive — re-probe, don't pay every frame.
            // Covers 100k instance fields where front-to-back yield can be >2% but GPU
            // stays flat (batched draws; CPU cull doesn't shrink submits).
            const COST_LATCH_MS: f32 = 2.0;
            const COST_LATCH_MIN_TESTED: u32 = 512;

            // Same discipline as instance upload plane_key: any camera move invalidates
            // the skip latch so we never freeze 78k DRAW_CULL_OCCLUDED flags across orbit.
            let hiz_plane_key = {
                use std::hash::{Hash, Hasher};
                let mut h = std::collections::hash_map::DefaultHasher::new();
                for col in camera.view_proj {
                    for v in col {
                        v.to_bits().hash(&mut h);
                    }
                }
                draw_calls.len().hash(&mut h);
                h.finish()
            };
            if self.render.occlusion_skip_frames > 0
                && self.render.occlusion_latch_plane_key != 0
                && hiz_plane_key != self.render.occlusion_latch_plane_key
            {
                self.render.occlusion_skip_frames = 0;
                self.render.occlusion_latch_plane_key = 0;
                if std::env::var_os("AZRA_SCOREBOARD").is_some()
                    || std::env::var_os("AZRA_HIZ_LOG").is_some()
                {
                    eprintln!("[HIZ] latch invalidated (camera/plane-key change) → full pass");
                }
            }

            if self.render.occlusion_skip_frames > 0 {
                let left = self.render.occlusion_skip_frames;
                self.render.occlusion_skip_frames = left - 1;
                // Re-apply last full-pass cull flags? We cannot — draw_calls are rebuilt
                // fresh each frame with cull=0. Skip means "accept all as visible for
                // main" (safe overdraw), not "freeze culled". Camera-stable only.
                self.perf.occlusion_ms = _occ_t0.elapsed().as_secs_f32() * 1000.0;
                // Scoreboard / debug: confirm skip path is live (throttled).
                if std::env::var_os("AZRA_SCOREBOARD").is_some()
                    || std::env::var_os("AZRA_HIZ_LOG").is_some()
                {
                    use std::cell::Cell;
                    use std::time::Instant;
                    thread_local! { static LAST: Cell<Option<Instant>> = Cell::new(None); }
                    let now = Instant::now();
                    let fire = LAST.with(|c| {
                        let due = c.get().map(|t| now.duration_since(t).as_secs_f32() >= 2.0).unwrap_or(true);
                        if due { c.set(Some(now)); }
                        due
                    });
                    if fire {
                        eprintln!(
                            "[HIZ] skip left={} tested={} culled={} last_ms={:.2} (visible=all; safe)",
                            left,
                            self.render.last_occlusion_tested,
                            self.render.last_occlusion_culled,
                            self.perf.occlusion_ms,
                        );
                    }
                }
            } else {
                let vp = glam::Mat4::from_cols_array_2d(&camera.view_proj);
                let cam_pos = {
                    let o = self.viewport.camera.orbit;
                    let d = self.viewport.camera.distance;
                    let t = self.viewport.camera.target;
                    t + o * glam::Vec3::new(0.0, 0.0, d)
                };
                // Structural world AABBs only — no mesh_cache × 8-corner per draw.
                let mut candidates: Vec<(usize, f32, glam::Vec3, glam::Vec3)> = Vec::new();
                candidates.reserve(draw_calls.len().min(4096));
                for (idx, (_, _, cull, _)) in draw_calls.iter().enumerate() {
                    if draw_skip_main(*cull) || draw_kind_is_special(draw_kind[idx]) {
                        continue;
                    }
                    if idx >= world_aabbs.len() {
                        continue;
                    }
                    let (wmin, wmax) = world_aabbs[idx];
                    if wmin == wmax {
                        continue;
                    }
                    let center = (wmin + wmax) * 0.5;
                    let dist = (center - cam_pos).length_squared();
                    candidates.push((idx, dist, wmin, wmax));
                }
                // Avoid O(N log N) sort cost when N is huge and we only need front-to-back
                // approximate order — partial sort first K for occluder seed, then rest.
                // Full sort was a multi-ms tax by itself at 100k.
                const SEED_K: usize = 2048;
                if candidates.len() > SEED_K {
                    candidates.select_nth_unstable_by(SEED_K, |a, b| {
                        a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal)
                    });
                    let (near, far) = candidates.split_at_mut(SEED_K);
                    near.sort_unstable_by(|a, b| {
                        a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal)
                    });
                    // Far half: leave unsorted (still tested; order less critical once near seeds Hi-Z).
                    let _ = far;
                } else {
                    candidates.sort_unstable_by(|a, b| {
                        a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal)
                    });
                }
                let mut occ = OcclusionBuffer::new(OCCLUSION_BUF_W, OCCLUSION_BUF_H);
                let mut occluded = 0u32;
                let tested = candidates.len() as u32;
                for (idx, _, wmin, wmax) in candidates {
                    if occ.test_and_insert_aabb(&vp, wmin, wmax) {
                        draw_calls[idx].2 = DRAW_CULL_OCCLUDED;
                        occluded += 1;
                    }
                }
                self.render.last_occlusion_culled = occluded;
                self.render.last_occlusion_tested = tested;
                let pass_ms = _occ_t0.elapsed().as_secs_f32() * 1000.0;
                self.perf.occlusion_ms = pass_ms;
                let yield_frac = if tested > 0 {
                    occluded as f32 / tested as f32
                } else {
                    0.0
                };
                // Latch when: (low yield) OR (expensive pass on large candidate set —
                // 100k instance open field often has non-zero yield but still wastes 8ms
                // with flat GPU). AZRA_CPU_OCCLUSION does not disable this.
                let low_yield =
                    tested >= MIN_TESTED_FOR_LATCH && yield_frac <= LOW_YIELD_MAX;
                let expensive_large =
                    tested >= COST_LATCH_MIN_TESTED && pass_ms >= COST_LATCH_MS;
                let latch = low_yield || expensive_large;
                if latch {
                    self.render.occlusion_low_yield_streak =
                        self.render.occlusion_low_yield_streak.saturating_add(1);
                    self.render.occlusion_skip_frames = SKIP_FRAMES;
                    self.render.occlusion_latch_plane_key = hiz_plane_key;
                } else {
                    self.render.occlusion_low_yield_streak = 0;
                    self.render.occlusion_skip_frames = 0;
                    self.render.occlusion_latch_plane_key = 0;
                }
                if std::env::var_os("AZRA_SCOREBOARD").is_some()
                    || std::env::var_os("AZRA_HIZ_LOG").is_some()
                    || pass_ms > 3.0
                {
                    use std::cell::Cell;
                    use std::time::Instant;
                    thread_local! { static LAST: Cell<Option<Instant>> = Cell::new(None); }
                    let now = Instant::now();
                    let fire = LAST.with(|c| {
                        let due = c.get().map(|t| now.duration_since(t).as_secs_f32() >= 2.0).unwrap_or(true);
                        if due { c.set(Some(now)); }
                        due
                    });
                    if fire {
                        eprintln!(
                            "[HIZ] full tested={} culled={} yield={:.3}% pass_ms={:.2} latch={} \
                             (low_y={} cost_l={}) skip_next={}",
                            tested,
                            occluded,
                            yield_frac * 100.0,
                            pass_ms,
                            latch,
                            low_yield,
                            expensive_large,
                            self.render.occlusion_skip_frames,
                        );
                    }
                }
            }
        } else {
            self.render.last_occlusion_culled = 0;
            self.render.last_occlusion_tested = 0;
            self.render.occlusion_skip_frames = 0;
            self.render.occlusion_low_yield_streak = 0;
            self.render.occlusion_latch_plane_key = 0;
            self.perf.occlusion_ms = 0.0;
        }

        self.perf.raster_frustum_ms = _t_frustum.elapsed().as_secs_f32() * 1000.0;

        // C.6 submit packing: after frustum/occlusion, pack *visible before culled* within
        // (cid, irange) so multi-instance batches are not split by holes. This reorders the
        // *working* list only. Putback keeps packed order (no un-sort) so a stable camera
        // re-marks the same cull bits on the same indices — packing is then a no-op sort.
        // D1/T1: when view_proj fingerprint matches last upload, skip the O(N log N) pack
        // entirely (was ~1 ms at 100k even when order was already correct).
        // AZRA_NO_DRAW_SORT=1 skips packing (sabotage gate: draws explode).
        if self.structural_draw_rebuilds_last_frame != 0 {
            let mut summary = String::new();
            for (cid, _inst, _cull, range) in draw_calls.iter().take(8) {
                use std::fmt::Write as _;
                let idx_n = self
                    .cache
                    .mesh_cache
                    .get(*cid)
                    .and_then(|m| m.gpu_mesh.as_ref().map(|g| g.index_count))
                    .unwrap_or(0);
                let _ = write!(summary, " {cid:?}:idx={idx_n},rng={range:?}");
            }
            Self::poly_trace(&format!(
                "structural REBUILD draws={}{}",
                draw_calls.len(),
                summary
            ));
        }
        let _t_sort = std::time::Instant::now();
        let plane_key_pre = {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            for col in camera.view_proj {
                for v in col {
                    v.to_bits().hash(&mut h);
                }
            }
            draw_calls.len().hash(&mut h);
            h.finish()
        };
        // D1 GPU cull: the instance buffer + cull_meta hold ONE canonical order; the C.6
        // visible-first pack would break that index alignment (GPU decides visibility, so
        // packing buys nothing here). `gpu_cull_wanted` latched above the frustum block.
        // B2: probe frames keep the main-view packing order (never rewrite the latch key).
        let skip_draw_sort = is_probe
            || gpu_cull_wanted
            || std::env::var_os("AZRA_NO_DRAW_SORT").is_some()
            || (plane_key_pre == self.raster_upload_plane_key
                && self.structural_draw_rebuilds_last_frame == 0
                && self.raster_upload_plane_key != 0);
        if !skip_draw_sort {
            let n = draw_calls.len();
            let mut order: Vec<usize> = (0..n).collect();
            order.sort_by(|&a, &b| {
                let sa = draw_kind_is_special(draw_kind[a]);
                let sb = draw_kind_is_special(draw_kind[b]);
                match (sa, sb) {
                    (true, false) => std::cmp::Ordering::Less,
                    (false, true) => std::cmp::Ordering::Greater,
                    (true, true) => a.cmp(&b),
                    (false, false) => {
                        let (ca, _, culla, ra) = &draw_calls[a];
                        let (cb, _, cullb, rb) = &draw_calls[b];
                        ca.0.cmp(&cb.0)
                            .then_with(|| ra.cmp(rb))
                            .then_with(|| {
                                draw_skip_main(*culla).cmp(&draw_skip_main(*cullb))
                            })
                            .then_with(|| a.cmp(&b))
                    }
                }
            });
            if order.iter().enumerate().any(|(i, &o)| i != o) {
                let old_draws = draw_calls.clone();
                let old_kind = std::mem::take(&mut draw_kind);
                let old_payload = std::mem::take(&mut draw_kind_payload);
                let old_aabb = std::mem::take(&mut world_aabbs);
                draw_calls.clear();
                draw_kind.clear();
                draw_kind_payload.clear();
                world_aabbs.clear();
                for &old_i in &order {
                    draw_calls.push(old_draws[old_i]);
                    draw_kind.push(old_kind[old_i]);
                    draw_kind_payload.push(old_payload[old_i]);
                    world_aabbs.push(old_aabb[old_i]);
                }
            }
        }

        self.perf.raster_sort_ms = _t_sort.elapsed().as_secs_f32() * 1000.0;

        // Materials (C.7: skip write_buffer when content matches last upload)
        let _t_upload = std::time::Instant::now();
        let mat_up = gpu.raster.upload_materials(&device, &queue, &materials);
        self.render.last_material_uploads = if mat_up { 1 } else { 0 };
        // Material texture array: rebuild the bind group when the buffer was recreated (above set
        // bind_group=None) OR when the texture array's generation changed (grow → new texture view).
        let mat_tex_gen = gpu.material_textures.generation;
        if gpu.raster.mat_tex_gen != mat_tex_gen {
            gpu.raster.bind_group = None;
            gpu.raster.mat_tex_gen = mat_tex_gen;
        }
        // Capture BEFORE the rebuild: true when the raster material bind group is (re)built this
        // frame for ANY reason — first frame, texture regrow, or material-buffer realloc
        // (upload_materials nulls it on realloc). The shadow caster shares the SAME material
        // resources at group(1) for displacement parity, so it must rebuild on the identical
        // trigger or it would bind a freed buffer/texture view (GPU Resource Lifetime rule).
        let raster_mat_rebuilt = gpu.raster.bind_group.is_none();
        if gpu.raster.bind_group.is_none() {
            let mat_view = gpu.material_textures.view();
            let mat_sampler = gpu.material_textures.sampler();
            gpu.raster.rebuild_bind_group(&device, mat_view, gpu.material_textures.linear_view(), mat_sampler);
        }
        // Shadow caster's group(1) material bind group — so vs_shadow applies the SAME per-vertex
        // displacement as the G-buffer (a displaced surface must cast a displaced shadow, else the
        // shadow detaches from the bump). Same resources as the raster material bind group.
        if raster_mat_rebuilt || gpu.shadows.material_bind_group.is_none() {
            let mat_view = gpu.material_textures.view();
            let mat_sampler = gpu.material_textures.sampler();
            gpu.shadows.rebuild_material_bind_group(&device, &gpu.raster.material_buffer, mat_view, mat_sampler);
        }

        // --- Shadow maps ---
        gpu.shadows.ensure_textures(&device);
        // Resolution changes ("Shadow Map Resolution" quality control) are APPLIED in
        // poll_background_tasks (Frame Ordering Contract); here we only detect the
        // recreation: a generation mismatch means shadow_bind_group references the
        // destroyed textures → rebuild before anything samples them THIS frame
        // (generation counters, never next-frame timing).
        if gpu.raster.shadow_map_gen != gpu.shadows.generation {
            gpu.raster.shadow_bind_group = None;
            gpu.raster.shadow_map_gen = gpu.shadows.generation;
        }

        // Direction TOWARD the sun (scene convention; default el=55° → y=+0.82, up).
        // update_cascades takes this convention directly and derives the light-frame
        // propagation direction internally (SHADOWACNE-3, 2026-07-17c: passing this
        // vector to an API that expected the propagation direction put the shadow
        // camera BELOW the scene → every body self-shadowed by its own underside
        // silhouette — the smooth dark blob in PBR).
        let ld = self.scene.light_config.direction();
        let light_dir = glam::Vec3::new(ld[0], ld[1], ld[2]);

        // Sun existence, derived from the scene graph EVERY frame (source of truth — never a
        // cached flag). When the last sun is deleted, `sync_light_to_gpu` early-outs and
        // `light_config` retains the dead light's direction; without this gate the shadow
        // chain kept baking a stale directional shadow into gbuf_material.z, and the deferred
        // pass multiplied it into AMBIENT — dark streaky banding on every away-facing face
        // with ZERO lights in the scene (round-4 artifact, readback-proven in
        // render/src/shadows.rs::shadow_readback_zero_lights_full_ambient).
        // Honour BOTH visibility flags, matching `build_frame_lights_gpu` (owner 2026-07-29).
        // `LightData.visible` is the light's own checkbox; `SceneNode.visible` is the OUTLINER EYE.
        // Checking only the former left a hidden sun still casting cascade shadows — shadows from a
        // light contributing no illumination, which looks like a shadow bug rather than a
        // visibility one.
        let has_sun = self.scene.scene_graph.lights().any(|(nid, l)| {
            let node_visible = self.scene.scene_graph.get(nid).map_or(true, |n| n.visible);
            node_visible
                && l.visible
                && l.light_type == core_math::scene_graph::LightType::Directional
        });

        // A2: Path Trace has its own visibility (NEE/ReSTIR). Skip the 3 CSM depth
        // passes under PT, but KEEP full G-buffer raster (denoise guides, picking,
        // reproject normals). Force sun_enabled=false in shadow uniforms so
        // compute_shadow() returns 1.0 without sampling stale cascade maps.
        let skip_csm = self.render.shading_mode == crate::ShadingMode::PathTrace
            // Overdraw / cascade A/B: AZRA_NO_SHADOWS=1 skips all 3 CSM depth passes.
            || std::env::var_os("AZRA_NO_SHADOWS").is_some();
        // A1 (PBR_REFINE_HARDENING): while Leak-Proof RT sun owns the sun term
        // (post_flags bit 512 — same predicate as viewport_render), CSM cascades are
        // discarded in deferred. Skip cascade RENDER + sun-cone jitter so we don't
        // pay 3 depth passes + ladder realloc for maps nobody samples.
        // KEEP CSM when: probe mini-frames (force pbr_refine_active=false), RT-sun
        // opt-out, interaction (!pbr_refine_active), RT buffers not ready yet.
        let sun_owned_by_rt = self.render.pbr_refine_active
            && self.render.pbr_refine_rt_sun
            && gpu.pipelines.rt_shadow_pipeline.is_some()
            && gpu.buffers.pt_tlas_buffer.is_some()
            && gpu.buffers.pt_blas_buffer.is_some()
            && gpu.buffers.pt_tri_buffer.is_some()
            && gpu.buffers.pt_vert_buffer.is_some();
        let has_sun_for_csm = has_sun && !skip_csm && !sun_owned_by_rt;

        // Compute view/proj matrices for cascade computation — from the UN-JITTERED
        // camera: the cascade fit is texel-snapped for stability, and feeding it the
        // progressive-refine sub-pixel jitter would wiggle the fit for no benefit.
        let view_mat = glam::Mat4::from_cols_array_2d(&camera_nojitter.view);
        let proj_mat = glam::Mat4::from_cols_array_2d(&camera_nojitter.proj);
        // shadow_pcf_softness = PCF radius in texels ("Shadow Softness", 0=hard); the shader
        // derives its anti-acne normal offset from it (radius + 1 texel) — no retuning.
        // D1 commit 5: keep the cascade view-projs — each shadow cascade gets its own GPU
        // cull dispatch against its ortho frustum (never the camera-culled set: a caster
        // behind the camera must still cast).
        // ── Accurate refined shadows (Phase 2b, owner 2026-08-06 screenshot) ──
        // While the refine accumulator averages, each frame renders the cascades from a
        // sun direction jittered inside a small cone ("Sun Softness"), so the 1/N mean
        // converges to the PHYSICAL area-light penumbra of the geometry actually drawn
        // into the maps — wide far from contact, sharp at contact. This SUPERSEDES the
        // short-lived PCF tap rotation (rotating the square pattern swept a ~1.41×
        // annulus: it only widened the fixed blur and could never produce a geometric
        // penumbra — part of the owner's "shadows not accurate" smear). The rotation
        // uniform is fed 0 again; the shader keeps the capability.
        // Shading deliberately keeps the UN-jittered direction — N·L over ≤5° is
        // visually static, and jittering it would shimmer speculars for nothing.
        // A1: skip cone jitter when RT sun owns the term (cascades not rendered).
        let shadow_sun_dir = if self.render.pbr_refine_active
            && self.render.pbr_refine_sun_angle_deg > 0.0
            && !sun_owned_by_rt
        {
            // R3 low-discrepancy pair (decorrelated from the camera-jitter R2 pair)
            // → concentric disc → offset in the sun's tangent frame.
            let n = (self.render.pbr_refine_frames % 4096) as f64;
            const R3A: f64 = 0.819_172_513_396_164_4;
            const R3B: f64 = 0.671_043_606_703_789_2;
            let u = (0.5 + n * R3A).fract() as f32;
            let v = (0.5 + n * R3B).fract() as f32;
            let (rr, th) = (u.sqrt(), v * std::f32::consts::TAU);
            let up = if light_dir.y.abs() < 0.9 { glam::Vec3::Y } else { glam::Vec3::X };
            let t1 = light_dir.cross(up).normalize();
            let t2 = light_dir.cross(t1);
            let tan_a = self.render.pbr_refine_sun_angle_deg.min(30.0).to_radians().tan();
            (light_dir + (t1 * th.cos() + t2 * th.sin()) * (rr * tan_a)).normalize()
        } else {
            light_dir
        };
        // ── Shadow-cascade far plane: fitted to the CASTERS, not to the camera ──
        //
        // The cascades used to be handed the camera's render far plane, which is
        // `(camera.distance * 20).max(500.0)` — floored at 500 world units no matter
        // how small the scene is. Cascade 0 covers the first 8% of that, so a 2-unit
        // cube was being shadowed by a 168-unit-wide ortho box: at 8192² that is a
        // 2 cm texel, ~5 screen pixels at a normal inspection distance, and the
        // staircase the owner reported. It also never improved on zoom-in (the
        // 500 floor pins the split while the subject grows on screen), so crisp
        // close-up shadows were unreachable at ANY supported map size — 1:1 texels
        // at 3 units would have needed a 40,387² map.
        //
        // Fit instead to the furthest shadow-casting geometry along the view axis.
        // `structural_world_aabb` is the same per-instance world AABB set the GPU
        // culler uses, so "what casts" and "what is fitted" cannot drift apart.
        // Falls back to the render far plane when there is nothing to cast (empty
        // scene, or the AABB set has not been built yet).
        let shadow_far = {
            let eye = glam::Vec3::new(camera_nojitter.cam_pos[0], camera_nojitter.cam_pos[1], camera_nojitter.cam_pos[2]);
            // Camera forward from the view matrix: look_at_rh stores -forward in the
            // third row of the rotation block (the .z of the first three columns).
            let fwd = -glam::Vec3::new(view_mat.x_axis.z, view_mat.y_axis.z, view_mat.z_axis.z);
            let mut max_z = 0.0f32;
            for (lo, hi) in &self.structural_world_aabb {
                // Skipped/!visible entries are stored as a degenerate zero AABB.
                if lo == hi {
                    continue;
                }
                for i in 0..8 {
                    let c = glam::Vec3::new(
                        if i & 1 == 0 { lo.x } else { hi.x },
                        if i & 2 == 0 { lo.y } else { hi.y },
                        if i & 4 == 0 { lo.z } else { hi.z },
                    );
                    max_z = max_z.max((c - eye).dot(fwd));
                }
            }
            if max_z <= near {
                far // nothing casts (or AABBs not built) — keep the old behaviour
            } else {
                // 5% + 1 unit of slack so a caster exactly at the bound is not clipped
                // by the last cascade, then never exceed the render far plane.
                (max_z * 1.05 + 1.0).clamp(near + 1.0, far)
            }
        };
        let shadow_uniforms = gpu.shadows.update_cascades(
            &queue, view_mat, proj_mat, shadow_sun_dir, near, shadow_far, has_sun_for_csm,
            self.render.shadow_pcf_softness, 0.0,
            render::shadows::ShadowBias {
                normal_offset_texels: self.render.shadow_normal_offset,
                depth_bias_texels: self.render.shadow_depth_bias,
                slope_depth_bias_texels: self.render.shadow_slope_bias,
            },
        );

        // Rebuild shadow bind group (connects shadow textures to the G-buffer shader)
        if gpu.raster.shadow_bind_group.is_none() {
            gpu.raster.rebuild_shadow_bind_group(&device, &gpu.shadows);
        }

        // RP1 + D1/T1 pure key-driven instance upload:
        //   write ⇔ (structural rebuild this frame) || (plane_key != stored)
        // No sampled content-hash skip inside `upload_instances` (unsound after C.6 packing).
        let plane_key = plane_key_pre;
        // D1: under GPU cull the buffer order is canonical (no pack) — camera motion never
        // re-uploads instances (the T1 "camera-only frame uploads nothing" clause). The
        // (wanted && !ready) arm covers the first frame after the cull system appears:
        // instances + cull inputs upload together in the same working-list order.
        // B2: probe frames never re-upload instances or touch the plane_key latch —
        // transforms are view-independent; reordering would pollute the main view.
        let need_inst_upload = !is_probe
            && (self.structural_draw_rebuilds_last_frame != 0
                || (gpu_cull_wanted && self.render.cull_batches_ready == 0)
                || (!gpu_cull_wanted
                    && (plane_key != self.raster_upload_plane_key
                        || self.raster_upload_plane_key == 0)));
        let inst_up = if is_probe {
            false
        } else if !need_inst_upload && !draw_calls.is_empty() {
            // GPU instance buffer still matches this packed order / camera fingerprint.
            false
        } else if draw_calls.is_empty() {
            self.raster_upload_plane_key = plane_key;
            false
        } else {
            let instances: Vec<InstanceData> =
                draw_calls.iter().map(|(_, inst, _, _)| *inst).collect();
            // cull_bits retained for API compat; upload always writes when called.
            let wrote =
                gpu.raster
                    .upload_instances(&device, &queue, &instances, &[]);
            Self::poly_trace(&format!("instances UPLOAD n={} wrote={wrote}", instances.len()));
            self.raster_upload_plane_key = plane_key;
            wrote
        };
        self.render.last_instance_uploads = if inst_up { 1 } else { 0 };
        self.perf.raster_upload_ms = _t_upload.elapsed().as_secs_f32() * 1000.0;
        let instance_stride = std::mem::size_of::<InstanceData>() as u64;

        // D1: refresh the cull system's structural inputs (cull_meta / batch_meta) whenever
        // the structural list rebuilt — or on the first frame the cull system exists (it may
        // have been created by poll after the last rebuild). Uses the WORKING list (the
        // structural arrays are mem::take'n into it for the pass) — order is stable because
        // `gpu_cull_wanted` forced `skip_draw_sort` above. write_buffer only — creation/grow
        // happened in poll (`poll_instance_cull`); if this frame's list outgrew capacity
        // before poll caught up, skip (fail-closed: the CPU cull path still runs).
        if self.structural_draw_rebuilds_last_frame != 0
            || (gpu_cull_wanted && self.render.cull_batches_ready == 0)
        {
            if let Some(cull) = gpu.raster_cull.as_ref() {
                let n = draw_calls.len();
                let batches = crate::raster_common::mesh_instance_batches(
                    &draw_calls,
                    &draw_kind,
                );
                if n <= cull.instance_capacity && batches.len() <= cull.batch_capacity && n > 0 {
                    // Per-instance meta: world AABB + owning batch (sentinel u32::MAX =
                    // special kind, never drawn through the cull path).
                    let mut inst_batch = vec![u32::MAX; n];
                    for (bi, b) in batches.iter().enumerate() {
                        for k in b.first..(b.first + b.count) {
                            inst_batch[k as usize] = bi as u32;
                        }
                    }
                    let cull_meta: Vec<render::instance_cull::CullMetaGpu> = (0..n)
                        .map(|k| {
                            let (mn, mx) = world_aabbs[k];
                            render::instance_cull::CullMetaGpu {
                                aabb_min: mn.to_array(),
                                batch: inst_batch[k],
                                aabb_max: mx.to_array(),
                                _pad: 0,
                            }
                        })
                        .collect();
                    let batch_meta: Vec<render::instance_cull::BatchMetaGpu> = batches
                        .iter()
                        .map(|b| {
                            let (s, c) = self
                                .cache
                                .mesh_cache
                                .get(b.cid)
                                .and_then(|m| m.gpu_mesh.as_ref())
                                .map(|gm| b.irange.unwrap_or((0, gm.index_count)))
                                .unwrap_or((0, 0)); // no GPU mesh yet → 0-index no-op draw
                            render::instance_cull::BatchMetaGpu {
                                index_count: c,
                                first_index: s,
                                base_vertex: 0,
                                out_base: b.first,
                            }
                        })
                        .collect();
                    queue.write_buffer(&cull.cull_meta_buffer, 0, bytemuck::cast_slice(&cull_meta));
                    queue.write_buffer(&cull.batch_meta_buffer, 0, bytemuck::cast_slice(&batch_meta));
                    self.render.cull_batches_ready = batches.len() as u32;
                    self.cull_batches = batches;
                } else {
                    self.render.cull_batches_ready = 0;
                    self.cull_batches.clear();
                }
            }
        }

        // Stage B-1 pool draw prep (write_buffer only — no resource creation in the render phase):
        // zero every pool entry's instance_count, then re-enable exactly the pooled bodies that
        // passed THIS frame's filters (visibility / isolation / top-level / renderable-content),
        // writing their per-body InstanceData into the pool's shared instance buffer at their slot
        // index. A hidden body, or one whose mesh went dense this bake, stays instance_count=0 and
        // self-skips in the multi-draw — the per-body filter semantics are preserved exactly.
        if let Some(pool) = gpu.sdf_mesh_pool.as_mut() {
            pool.clear_frame_visibility(&queue);
            for (i, (&kind, &payload)) in draw_kind.iter().zip(draw_kind_payload.iter()).enumerate() {
                if kind == DRAW_KIND_SDF_POOLED && !draw_skip_main(draw_calls[i].2) {
                    let nid = core_math::scene_graph::NodeId(payload);
                    pool.mark_drawn(&queue, nid, &draw_calls[i].1);
                }
            }
        }

        // Encode all passes
        let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("raster_shadow_gbuffer"),
        });

        // Take GPU timestamp system for render pass timing
        let mut gpu_ts = gpu.gpu_ts.take();

        // --- D1 GPU instance cull (camera pass) ---
        // Active ⇔ system + inputs resident. Uploads the 128-B CullParams, then dispatches
        // args-reset + frustum cull; survivors land compacted in `visible_instances` and
        // the per-batch indirect args this frame's G-buffer encode reads.
        let gpu_cull_active = gpu_cull_wanted
            && self.render.cull_batches_ready as usize == self.cull_batches.len()
            && !self.cull_batches.is_empty()
            && !draw_calls.is_empty();
        // C4: set when the early pass ran in HZB mode → the late dispatch + second
        // G-buffer pass + HZB rebuild run after pass A. Sabotage AZRA_HZB_NO_LATE=1
        // keeps early-only (must pop on disocclusion).
        let mut hzb_two_pass = false;
        if gpu_cull_active {
            let planes_of = |vp_cols: &[[f32; 4]; 4]| {
                let vp = glam::Mat4::from_cols_array_2d(vp_cols);
                let planes = frustum_planes(&vp);
                let mut arr = [[0.0f32; 4]; 6];
                for (dst, p) in arr.iter_mut().zip(planes.iter()) {
                    *dst = p.to_array();
                }
                arr
            };
            // Sabotage switch for the D1 gate: mark everything visible.
            let passthrough =
                u32::from(std::env::var("AZRA_CULL_PASSTHROUGH").is_ok_and(|v| v == "1"));
            // D2/C4: HZB occlusion default ON (the late pass below makes it pop-free).
            // AZRA_HZB=0 or AZRA_NO_HIZ=1 → frustum-only. AZRA_HZB_NO_LATE=1 = sabotage
            // (early-only, must pop — proves the late pass is load-bearing).
            // B2: probe frames are frustum-only — HZB from a probe camera would early-cull
            // the main view for the rest of the bake round.
            let hzb_ready = !is_probe
                && self.render.dbg_hzb
                && gpu.hzb.as_ref().is_some_and(|h| h.valid);
            let (hzb_info, hzb_bind) = match gpu.hzb.as_ref() {
                Some(h) if hzb_ready => (
                    (h.viewport.0, h.viewport.1, h.size, h.mip_count),
                    Some((&h.full_view, h.generation)),
                ),
                Some(h) => ((0, 0, 1, 1), Some((&h.full_view, h.generation))),
                None => ((0, 0, 1, 1), None),
            };
            let base = render::instance_cull::CullParamsGpu {
                planes: [[0.0; 4]; 6],
                view_proj: [[0.0; 4]; 4],
                n_instances: draw_calls.len() as u32,
                n_batches: self.render.cull_batches_ready,
                passthrough,
                args_base: 0,
                visible_base: 0,
                mode: render::instance_cull::CULL_MODE_FRUSTUM_ONLY,
                viewport_w: hzb_info.0,
                viewport_h: hzb_info.1,
                hzb_size: hzb_info.2,
                mip_count: hzb_info.3,
                _pad: [0; 2],
            };
            let inst_gen = gpu.raster.instance_buffer_generation;
            let instance_buffer = &gpu.raster.instance_buffer;
            let sys = gpu
                .raster_cull
                .as_mut()
                .expect("gpu_cull_active implies raster_cull");
            // Slot 0 = camera; slot 1+c = shadow cascade c against its ortho frustum
            // (cascades are ALWAYS frustum-only — a camera-depth HZB is wrong there).
            sys.write_pass_params(&queue, 0, render::instance_cull::CullParamsGpu {
                planes: planes_of(&camera.view_proj),
                view_proj: camera.view_proj,
                mode: if hzb_ready {
                    render::instance_cull::CULL_MODE_EARLY
                } else {
                    render::instance_cull::CULL_MODE_FRUSTUM_ONLY
                },
                ..base
            });
            if has_sun_for_csm {
                for c in 0..NUM_CASCADES {
                    sys.write_pass_params(&queue, 1 + c, render::instance_cull::CullParamsGpu {
                        planes: planes_of(&shadow_uniforms.cascade_vp[c]),
                        ..base
                    });
                }
            }
            hzb_two_pass = hzb_ready && self.render.dbg_hzb_late;
            // C4: camera-late slot (retests early HZB-rejects vs the mid-frame pyramid).
            if hzb_ready {
                sys.write_pass_params(
                    &queue,
                    render::instance_cull::PASS_CAMERA_LATE,
                    render::instance_cull::CullParamsGpu {
                        planes: planes_of(&camera.view_proj),
                        view_proj: camera.view_proj,
                        ..base
                    },
                );
            }
            sys.ensure_bind_group(&device, instance_buffer, inst_gen, hzb_bind);
            let sys = gpu.raster_cull.as_ref().expect("just ensured");
            let n_passes = if has_sun_for_csm { 1 + NUM_CASCADES } else { 1 };
            let mut cp = enc.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("instance_cull"),
                timestamp_writes: None,
            });
            for p in 0..n_passes {
                cp.set_pipeline(&sys.pipeline_reset);
                cp.set_bind_group(0, sys.pass_bind_group(p), &[]);
                cp.dispatch_workgroups(self.render.cull_batches_ready.div_ceil(64), 1, 1);
                cp.set_pipeline(&sys.pipeline_cull);
                cp.dispatch_workgroups((draw_calls.len() as u32).div_ceil(64), 1, 1);
            }
        }

        // --- D4: cluster cut (camera + shadow cascades) — MUST encode BEFORE the
        // shadow + G-buffer passes: their multi-draws read this frame's args
        // (commit-4 originally sat after the G-buffer pass → one-frame-stale args).
        // One dispatch per (resident cid, pass); thread per (instance, cluster).
        // WP5: only when the draw path is on — no stealth cut cost with clusters off.
        if clusters_on {
        if let (Some(cut), false) = (gpu.cluster_cut.as_mut(), gpu.cluster_pool.entries.is_empty()) {
            // SPHERE tests need NORMALIZED planes (dist < -radius compares world
            // units). frustum_planes returns UNNORMALIZED Gribb-Hartmann rows —
            // fine for D1's AABB sign trick, but here the |n| factor inflated the
            // radius comparison and over-culled every cluster near a frustum edge:
            // the 2026-08-05 terrain holes (617 drawn vs 853 in the CPU cut).
            let normalize_planes = |m: &[[f32; 4]; 4]| {
                let planes = frustum_planes(&glam::Mat4::from_cols_array_2d(m));
                let mut arr = [[0.0f32; 4]; 6];
                for (dst, p) in arr.iter_mut().zip(planes.iter()) {
                    let v = p.to_array();
                    let len = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt().max(1e-12);
                    *dst = [v[0] / len, v[1] / len, v[2] / len, v[3] / len];
                }
                arr
            };
            let plane_arr = normalize_planes(&camera.view_proj);
            let tan_half_fov = 1.0 / camera.proj[1][1];
            let viewport_h = gpu.raster.depth_size.1.max(1) as f32;
            let passthrough = u32::from(self.render.dbg_cluster_passthrough);
            let threshold_px: f32 = std::env::var("AZRA_CLUSTER_THRESHOLD_PX")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(1.0);
            cut.zero_counters(&queue);
            // Per resident cid: structural instance slots (draw list order = the
            // instance buffer order under GPU cull).
            let mut dispatches: Vec<(core_math::scene_graph::MeshCacheId, u32, u32)> = Vec::new();
            for (slot, (cid, entry)) in
                render::cluster_cut::ClusterCutSystem::stable_cid_slots(&gpu.cluster_pool)
            {
                let mut inst_slots = [0u32; 4];
                let mut n_inst = 0u32;
                for (i, (c, _, _, _)) in draw_calls.iter().enumerate() {
                    if *c == cid
                        && (!draw_kind_is_special(draw_kind[i])
                            || draw_kind[i] == crate::raster_common::DRAW_KIND_CLUSTERED)
                        && (n_inst as usize) < render::cluster_cut::MAX_INSTANCES_PER_CID
                    {
                        inst_slots[n_inst as usize] = i as u32;
                        n_inst += 1;
                    }
                }
                if n_inst == 0 {
                    continue;
                }
                let base = render::cluster_cut::CutParamsGpu {
                    planes: plane_arr,
                    cam: [camera.cam_pos[0], camera.cam_pos[1], camera.cam_pos[2], tan_half_fov],
                    metrics: [
                        viewport_h,
                        threshold_px,
                        f32::from_bits(entry.n_clusters),
                        f32::from_bits(n_inst),
                    ],
                    instance_slots: inst_slots,
                    region: [0, 0, passthrough, 0], // bases/cap filled by write_region_params
                    // Camera: skip backface cone by default (meshlet cones on dense CC
                    // can drop front-facing islands → holes). Opt-in with AZRA_CLUSTER_CONE=1.
                    // Frustum stays unless AZRA_CLUSTER_NO_VIS=1.
                    flags: [
                        u32::from(!std::env::var("AZRA_CLUSTER_CONE").is_ok_and(|v| v == "1")),
                        u32::from(self.render.dbg_cluster_force_l0),
                        u32::from(std::env::var("AZRA_CLUSTER_NO_VIS").is_ok_and(|v| v == "1")),
                        0,
                    ],
                };
                cut.write_region_params(
                    &queue,
                    render::cluster_cut::ClusterCutSystem::region_id(slot, 0),
                    base,
                );
                // Cascade passes: SAME camera-based error metric (shadow LOD must
                // match the visual LOD), cascade frustum planes, cone skipped
                // (camera-backface is wrong for a light).
                //
                // WP8 audit (N/A as bug): `compute_cascade_vps` already builds each
                // cascade ortho with caster headroom — light eye 50 units back, near = -r
                // so casters between light and slice still land in the map
                // (`render/src/shadows.rs` compute_cascade_vps). Sphere cull against
                // these planes is therefore the same class as mesh cascade cull, not a
                // tight "receivers only" frustum that drops external casters.
                if has_sun_for_csm {
                    for c in 0..NUM_CASCADES {
                        let cp_arr = normalize_planes(&shadow_uniforms.cascade_vp[c]);
                        cut.write_region_params(
                            &queue,
                            render::cluster_cut::ClusterCutSystem::region_id(slot, 1 + c),
                            render::cluster_cut::CutParamsGpu {
                                planes: cp_arr,
                                flags: [1, u32::from(self.render.dbg_cluster_force_l0), 0, 0],
                                ..base
                            },
                        );
                    }
                }
                if std::env::var("AZRA_FLICKER_WATCH").is_ok_and(|v| v == "1") {
                    use std::cell::Cell;
                    thread_local! { static LS: Cell<u32> = Cell::new(u32::MAX); }
                    LS.with(|c| {
                        if c.get() != inst_slots[0] {
                            eprintln!(
                                "[FLICKER] {cid:?} first_instance {} -> {} (n_inst={n_inst}, draws={})",
                                c.get(), inst_slots[0], draw_calls.len()
                            );
                            c.set(inst_slots[0]);
                        }
                    });
                }
                dispatches.push((cid, entry.n_clusters, n_inst));
            }
            if !dispatches.is_empty() {
                let inst_gen = gpu.raster.instance_buffer_generation;
                cut.ensure_bind_groups(&gpu.device, &gpu.cluster_pool, &gpu.raster.instance_buffer, inst_gen);
                let n_passes = if has_sun_for_csm { 1 + NUM_CASCADES } else { 1 };
                // The arg RESET and the arg WRITE touch the same buffer. Interleaving
                // them as `reset, cut, reset, cut…` inside ONE compute pass leaves
                // their ordering to the driver: reset threads that land late zero args
                // the cut already wrote, and those clusters silently vanish for that
                // frame — nondeterministic per frame, which is exactly the terrain
                // flicker (only clustered geometry, present even with a static camera).
                //
                // Two fixes, both applied:
                //   1. With the COUNT path the reset is unnecessary — the draw reads
                //      only slots [0, count), every one of which the cut wrote this
                //      frame. Stale slots beyond `count` are never fetched.
                //   2. Without it (fixed-cap fallback), the resets run in their OWN
                //      compute pass, so the pass boundary orders them before the cut.
                if !cut.use_count {
                    let mut rp = enc.begin_compute_pass(&wgpu::ComputePassDescriptor {
                        label: Some("cluster_cut_reset"),
                        timestamp_writes: None,
                    });
                    rp.set_pipeline(&cut.pipeline_reset);
                    for (cid, _, _) in &dispatches {
                        for pass in 0..n_passes {
                            let Some(bg) = cut.pass_bind_group(*cid, pass) else { continue };
                            rp.set_bind_group(0, bg, &[]);
                            rp.dispatch_workgroups(
                                render::cluster_cut::MAX_VISIBLE_CLUSTERS_PER_CID.div_ceil(64),
                                1,
                                1,
                            );
                        }
                    }
                }
                let mut cp = enc.begin_compute_pass(&wgpu::ComputePassDescriptor {
                    label: Some("cluster_cut"),
                    timestamp_writes: None,
                });
                cp.set_pipeline(&cut.pipeline_cut);
                for (cid, n_clusters, n_inst) in &dispatches {
                    for pass in 0..n_passes {
                        let Some(bg) = cut.pass_bind_group(*cid, pass) else { continue };
                        cp.set_bind_group(0, bg, &[]);
                        cp.dispatch_workgroups((n_clusters * n_inst).div_ceil(64), 1, 1);
                    }
                }
                drop(cp);
                if cut.use_count {
                    cut.encode_count_copy(&mut enc); // before any counted multi-draw
                }
                cut.encode_counter_copy(&mut enc);
                // AZRA_FLICKER_WATCH: sample the camera region's draw list so a
                // frame-to-frame set change can be DETECTED, not eyeballed.
                if std::env::var("AZRA_FLICKER_WATCH").is_ok_and(|v| v == "1") {
                    if let Some((_, (cid0, _))) =
                        render::cluster_cut::ClusterCutSystem::stable_cid_slots(&gpu.cluster_pool).next()
                    {
                        let _ = cid0;
                        cut.encode_args_copy(&mut enc, render::cluster_cut::ClusterCutSystem::region_id(0, 0));
                    }
                }
            } // !dispatches.is_empty()
        } // if let cluster_cut
        } // clusters_on — no cut encode when D4 draw path is off (WP5)

        // --- Shadow depth passes (3 cascades) ---
        // No sun → skip rendering the cascades entirely. The maps then hold stale depth,
        // but compute_shadow() never samples them (ShadowData.flags.x == 0 gate) — and we
        // don't spend 3 full-scene depth passes on a light that doesn't exist.
        // A2: also skip under PathTrace (`has_sun_for_csm` false) — PT owns shadows.
        let _t_shadow = std::time::Instant::now();
        for c in 0..NUM_CASCADES {
            if !has_sun_for_csm { break; }
            let Some(ref shadow_view) = gpu.shadows.depth_views[c] else { continue };
            let Some(ref shadow_bg) = gpu.shadows.bind_groups[c] else { continue };

            // One slot per cascade — the profiler sums them. Timing only cascade 0 (the
            // pre-2026-08-06 behaviour) under-reported the pass by 3x, which is how an
            // 8192² shadow setting could dominate a frame while the HUD looked healthy.
            let slot = match c {
                0 => crate::gpu_timestamps::PASS_SHADOW,
                1 => crate::gpu_timestamps::PASS_SHADOW_1,
                _ => crate::gpu_timestamps::PASS_SHADOW_2,
            };
            let ts = gpu_ts.as_mut().map(|ts| ts.render_pass_writes(slot));

            let mut spass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some(&format!("shadow_cascade_{}", c)),
                color_attachments: &[],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: shadow_view,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: ts,
                occlusion_query_set: None,
            });

            spass.set_pipeline(&gpu.shadows.pipeline);
            spass.set_bind_group(0, shadow_bg, &[]);
            // group(1): material resources so vs_shadow applies the same displacement as the
            // G-buffer. Rebuilt every frame above (whenever the raster material bind group is),
            // so it is always Some here — the pipeline requires it, so bind unconditionally.
            spass.set_bind_group(1, gpu.shadows.material_bind_group.as_ref()
                .expect("shadow material bind group must be built before the cascade draw"), &[]);

            // POOL fix: draw ALL resident voxel chunks in ONE multi_draw over the shared pool (empty slots
            // have index_count=0 → self-skip). All resident chunks of the body share one InstanceData, so
            // bind the instance at ANY resident draw_call's offset. Non-resident draws keep the per-call path.
            let first_vox = draw_kind
                .iter()
                .position(|&k| k == DRAW_KIND_VOXEL_RESIDENT);
            draw_resident_pool(&mut spass, gpu, first_vox, instance_stride);
            // D1 commit 5: mesh casters via this cascade's OWN GPU-culled regions (pass 1+c).
            // The camera-culled set is never bound here — behind-camera casters still cast.
            if gpu_cull_active {
                if let Some(sys) = gpu.raster_cull.as_ref() {
                    for (bi, b) in self.cull_batches.iter().enumerate() {
                        if let Some(cached) = self.cache.mesh_cache.get(b.cid) {
                            if let Some(ref gpu_mesh) = cached.gpu_mesh {
                                spass.set_vertex_buffer(0, gpu_mesh.vertex_buffer.slice(..));
                                spass.set_vertex_buffer(
                                    1,
                                    sys.visible_buffer.slice(sys.visible_offset(1 + c, b.first)..),
                                );
                                spass.set_index_buffer(
                                    gpu_mesh.index_buffer.slice(..),
                                    gpu_mesh.index_format,
                                );
                                spass.draw_indexed_indirect(
                                    &sys.draw_args_buffer,
                                    sys.args_offset(1 + c, bi),
                                );
                            }
                        }
                    }
                }
            }
            // D4 commit 5: clustered shadow casters via this cascade's cut region
            // (pass 1+c — cascade frustum, camera error metric, cone skipped).
            // Replaces the commit-4 source-mesh exception in the SAME commit.
            if clusters_on {
                if let Some(cut) = gpu.cluster_cut.as_ref() {
                    for (slot, (cid, entry)) in
                        render::cluster_cut::ClusterCutSystem::stable_cid_slots(&gpu.cluster_pool)
                    {
                        let routed = draw_calls.iter().zip(draw_kind.iter()).any(
                            |((cc, _, _, _), &k)| {
                                *cc == cid && k == crate::raster_common::DRAW_KIND_CLUSTERED
                            },
                        );
                        if !routed {
                            continue;
                        }
                        let region =
                            render::cluster_cut::ClusterCutSystem::region_id(slot, 1 + c);
                        spass.set_vertex_buffer(0, entry.vertex_buffer.slice(..));
                        spass.set_vertex_buffer(1, gpu.raster.instance_buffer.slice(..));
                        spass.set_index_buffer(entry.index_buffer.slice(..), wgpu::IndexFormat::Uint32);
                        if cut.use_count {
                            spass.multi_draw_indexed_indirect_count(
                                &cut.args_buffer,
                                render::cluster_cut::ClusterCutSystem::args_byte_offset(region),
                                &cut.count_buffer,
                                render::cluster_cut::ClusterCutSystem::count_byte_offset(region),
                                render::cluster_cut::MAX_VISIBLE_CLUSTERS_PER_CID,
                            );
                        } else {
                            spass.multi_draw_indexed_indirect(
                                &cut.args_buffer,
                                render::cluster_cut::ClusterCutSystem::args_byte_offset(region),
                                render::cluster_cut::MAX_VISIBLE_CLUSTERS_PER_CID,
                            );
                        }
                    }
                }
            }
            // P4 multi-instance + C.2: skip DRAW_CULL_SHADOW (true frustum-out). Occlusion-only
            // (MAIN only) still casts. Batch consecutive same (cid, irange, !skip_shadow).
            let mut i = 0usize;
            while i < draw_calls.len() {
                if draw_kind_is_special(draw_kind[i]) {
                    i += 1;
                    continue;
                }
                if gpu_cull_active {
                    i += 1;
                    continue; // D1: drawn via the per-cascade indirect loop above
                }
                if draw_skip_shadow(draw_calls[i].2) {
                    i += 1;
                    continue;
                }
                let (cid, _, _, irange) = draw_calls[i];
                let mut j = i + 1;
                while j < draw_calls.len() {
                    if draw_kind_is_special(draw_kind[j]) {
                        break;
                    }
                    if draw_skip_shadow(draw_calls[j].2) {
                        break;
                    }
                    let (c2, _, _, r2) = draw_calls[j];
                    if c2 != cid || r2 != irange {
                        break;
                    }
                    j += 1;
                }
                let inst_count = (j - i) as u32;
                let byte_off = i as u64 * instance_stride;
                if let Some(cached) = self.cache.mesh_cache.get(cid) {
                    if let Some(ref gpu_mesh) = cached.gpu_mesh {
                        spass.set_vertex_buffer(0, gpu_mesh.vertex_buffer.slice(..));
                        spass.set_vertex_buffer(1, gpu.raster.instance_buffer.slice(byte_off..));
                        spass.set_index_buffer(gpu_mesh.index_buffer.slice(..), gpu_mesh.index_format);
                        let (s, c) = irange.unwrap_or((0, gpu_mesh.index_count));
                        if c > 0 && inst_count > 0 {
                            spass.draw_indexed(s..(s + c), 0, 0..inst_count);
                        }
                    }
                }
                i = j;
            }
        }
        self.perf.raster_shadow_enc_ms = _t_shadow.elapsed().as_secs_f32() * 1000.0;

        // --- G-buffer raster pass ---
        let _t_gbuf = std::time::Instant::now();
        let gn_view = gpu.gbuf_normal.as_ref().map(|t| t.create_view(&Default::default()));
        let gm_view = gpu.gbuf_material.as_ref().map(|t| t.create_view(&Default::default()));
        let ga_view = gpu.gbuf_albedo.as_ref().map(|t| t.create_view(&Default::default()));
        let gmot_view = gpu.gbuf_motion.as_ref().map(|t| t.create_view(&Default::default()));
        let depth_view = gpu.raster.depth_view.as_ref();

        let (Some(gn), Some(gm), Some(ga), Some(gmot), Some(dv)) = (&gn_view, &gm_view, &ga_view, &gmot_view, depth_view) else {
            // Put structural cache back (same as other early-outs).
            for e in &mut draw_calls {
                e.2 = 0;
            }
            self.structural_materials = materials;
            self.structural_draw_calls = draw_calls;
            self.structural_draw_kind = draw_kind;
            self.structural_draw_kind_payload = draw_kind_payload;
            self.structural_world_aabb = world_aabbs;
            return any_sdf_rasterized;
        };

        // GPU_CURVATURE_KERNEL_PLAN.md P2: build every SDF body's group-2 bind group BEFORE the
        // render pass opens — `wgpu::BindGroup` created inside the pass would be dropped while
        // `rpass` still borrows it (see `raster_common::CurvBind`'s doc comment). Built once per
        // pass, keyed by body NodeId; a body absent from this map draws with the shared dummy.
        // Uses `curvature_bind_group_for_body` (takes `&GpuCtx` only) rather than the `&self`
        // method — `gpu` is a disjoint `&mut GpuCtx` reborrow of `self.gpu` here, so an `&self`
        // call would conflict with it.
        let curv_bgs: std::collections::HashMap<core_math::scene_graph::NodeId, wgpu::BindGroup> =
            draw_kind
                .iter()
                .zip(draw_kind_payload.iter())
                .filter_map(|(&k, &p)| match k {
                    DRAW_KIND_SDF_RESIDENT => {
                        Some((core_math::scene_graph::NodeId(p), false))
                    }
                    DRAW_KIND_SDF_POOLED => Some((core_math::scene_graph::NodeId(p), true)),
                    _ => None,
                })
                .collect::<std::collections::HashSet<_>>()
                .into_iter()
                .filter_map(|(nid, pooled)| {
                    crate::raster_common::curvature_bind_group_for_body(gpu, nid, pooled)
                        .map(|bg| (nid, bg))
                })
                .collect();

        {
            let gbuf_ts = gpu_ts.as_mut().map(|ts| ts.render_pass_writes(crate::gpu_timestamps::PASS_RASTER_GBUF));
            let mut rpass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("gbuffer_raster"),
                // 4 color targets only — 32-byte/sample hardware limit prevents 7.
                // surface_id, curvature, emission written by SDF compute pass.
                color_attachments: &[
                    // location(0): normal + depth
                    Some(wgpu::RenderPassColorAttachment {
                        view: gn, resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color { r: 0.0, g: 0.0, b: 0.0, a: 0.0 }),
                            store: wgpu::StoreOp::Store,
                        },
                    }),
                    // location(1): material
                    Some(wgpu::RenderPassColorAttachment {
                        view: gm, resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color { r: 0.0, g: 0.0, b: 0.0, a: 0.0 }),
                            store: wgpu::StoreOp::Store,
                        },
                    }),
                    // location(2): albedo + flags
                    Some(wgpu::RenderPassColorAttachment {
                        view: ga, resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color { r: 0.0, g: 0.0, b: 0.0, a: 0.0 }),
                            store: wgpu::StoreOp::Store,
                        },
                    }),
                    // location(3): motion (body_index)
                    Some(wgpu::RenderPassColorAttachment {
                        view: gmot, resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color { r: 0.0, g: 0.0, b: 0.0, a: 0.0 }),
                            store: wgpu::StoreOp::Store,
                        },
                    }),
                ],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: dv,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: gbuf_ts,
                occlusion_query_set: None,
            });

            // B1: probe mini-frame — clip maps to top-left s×s only (matches deferred UV
            // when cam_right.w = s). Texels outside stay clear; capture never reads them.
            if is_probe {
                let s = probe_s as f32;
                rpass.set_viewport(0.0, 0.0, s, s, 0.0, 1.0);
                rpass.set_scissor_rect(0, 0, probe_s, probe_s);
            }

            rpass.set_pipeline(&gpu.raster.pipeline);
            if let Some(ref bg) = gpu.raster.bind_group {
                rpass.set_bind_group(0, bg, &[]);
            }
            if let Some(ref sbg) = gpu.raster.shadow_bind_group {
                rpass.set_bind_group(1, sbg, &[]);
            }
            // GPU_CURVATURE_KERNEL_PLAN.md P2: group 2 must ALWAYS be bound (pipeline validation
            // requirement) — default to the dummy; the per-body draws below rebind their own
            // buffer via `curv_bgs` when they have one.
            rpass.set_bind_group(2, &gpu.raster.curvature_dummy_bind_group, &[]);
            let curv_bind: crate::raster_common::CurvBind<'_> =
                Some((&gpu.raster.curvature_dummy_bind_group, &curv_bgs));

            // POOL fix: ALL resident voxel chunks in ONE multi_draw over the shared pool (empty slots
            // self-skip). All resident chunks share one InstanceData → bind the instance at any resident
            // draw_call's offset. Non-resident draws (CPU-mesh fallback, poly/NURBS) keep the per-call path.
            // PLAN B: skipped during an active sculpt stroke — the raymarch overlay shows the live surface.
            if !sculpt_raymarch_overlay {
                let first_vox = draw_kind
                    .iter()
                    .position(|&k| k == DRAW_KIND_VOXEL_RESIDENT);
                draw_resident_pool(&mut rpass, gpu, first_vox, instance_stride);
            }
            // Stage B-1: ALL pooled sparse SDF bodies in ONE multi_draw over the shared SdfMeshPool
            // (visibility pre-written above; empty/hidden slots self-skip via instance_count=0).
            // `false` = feature fallback → the loop below draws them per-slot from the SAME pool.
            let sdf_pool_handled = draw_sdf_pool_multi(&mut rpass, gpu, curv_bind);
            // P4 / C.6: multi-instance batching for mesh_cache draws that share (cid, irange).
            // Requires the sort above — consecutive-only merge. C.6 multi-mesh gate checks
            // last_instance_draws ≈ N distinct meshes (not N×M).
            // C.8: kind flags O(1) instead of HashMap probes.
            let mut draws = 0u32;
            let mut batches = 0u32;
            let mut merged = 0u32;
            let mut i = 0usize;
            while i < draw_calls.len() {
                let (cid, _, cull, irange) = draw_calls[i];
                if draw_skip_main(cull) {
                    i += 1;
                    continue; // frustum/occlusion: skip G-buffer
                }
                let kind = draw_kind[i];
                if kind == DRAW_KIND_VOXEL_RESIDENT {
                    i += 1;
                    continue; // drawn via the pool multi-draw above
                }
                // THE 2026-08-05 TERRAIN FLICKER ROOT CAUSE. CLUSTERED kind is not
                // voxel, not SDF, and not MESH — so it fell through every guard into
                // this CPU mesh path and the body was drawn TWICE: full source mesh
                // here + the cluster cut's multi-draw. Two nearly-coplanar surfaces
                // z-fought, and the per-frame winner varied with GPU draw order →
                // shimmer with a STATIC camera. (Pinned-L0 looked stable only because
                // L0 == source positions exactly: bit-identical depth made the first
                // draw win deterministically.) Found by the cluster debug view — the
                // palette showed as patches THROUGH the textured double-draw.
                if kind == crate::raster_common::DRAW_KIND_CLUSTERED {
                    i += 1;
                    continue; // drawn ONLY by the cluster multi-draw
                }
                if gpu_cull_active && kind == DRAW_KIND_MESH {
                    i += 1;
                    continue; // D1: encoded via the per-batch indirect loop below
                }
                let byte_off = i as u64 * instance_stride;
                if kind == DRAW_KIND_SDF_POOLED || kind == DRAW_KIND_SDF_RESIDENT {
                    let res_nid = core_math::scene_graph::NodeId(draw_kind_payload[i]);
                    if kind == DRAW_KIND_SDF_POOLED {
                        if !sdf_pool_handled {
                            draw_sdf_pool_slot(
                                &mut rpass,
                                gpu,
                                res_nid,
                                gpu.raster.instance_buffer.slice(byte_off..),
                                curv_bind,
                            );
                        }
                        i += 1;
                        continue;
                    }
                    // G2: SDF body's GPU-resident slot — indirect draw, no mesh_cache lookup.
                    draw_sdf_resident_slot(
                        &mut rpass,
                        gpu,
                        res_nid,
                        gpu.raster.instance_buffer.slice(byte_off..),
                        curv_bind,
                    );
                    i += 1;
                    continue;
                }
                // Batch consecutive visible mesh_cache draws with same mesh + range.
                let mut j = i + 1;
                while j < draw_calls.len() {
                    let (c2, _, cull2, r2) = draw_calls[j];
                    if draw_skip_main(cull2)
                        || draw_kind_is_special(draw_kind[j])
                        || c2 != cid
                        || r2 != irange
                    {
                        break;
                    }
                    j += 1;
                }
                let inst_count = (j - i) as u32;
                if inst_count > 1 {
                    batches += 1;
                    merged += inst_count - 1;
                }
                if let Some(cached) = self.cache.mesh_cache.get(cid) {
                    if let Some(ref gpu_mesh) = cached.gpu_mesh {
                        rpass.set_vertex_buffer(0, gpu_mesh.vertex_buffer.slice(..));
                        rpass.set_vertex_buffer(1, gpu.raster.instance_buffer.slice(byte_off..));
                        rpass.set_index_buffer(gpu_mesh.index_buffer.slice(..), gpu_mesh.index_format);
                        let (s, c) = irange.unwrap_or((0, gpu_mesh.index_count));
                        if c > 0 && inst_count > 0 {
                            rpass.draw_indexed(s..(s + c), 0, 0..inst_count);
                            draws += 1;
                            Self::poly_trace(&format!("encode gbuf {cid:?} idx={c} inst={inst_count}"));
                        }
                    }
                }
                i = j;
            }
            // D4: clustered multi-draws — ONE multi_draw_indexed_indirect per resident
            // clustered cid over its camera-early args region (unselected slots have
            // index_count=0 and self-skip — the SdfMeshPool pattern). first_instance in
            // each arg points at the structural InstanceData slot (feature enabled).
            if clusters_on {
                if let Some(cut) = gpu.cluster_cut.as_ref() {
                    let mut cluster_md_draws = 0u32;
                    for (slot, (cid, entry)) in
                        render::cluster_cut::ClusterCutSystem::stable_cid_slots(&gpu.cluster_pool)
                    {
                        // Only cids actually routed this frame (kind remapped).
                        let routed = draw_calls
                            .iter()
                            .zip(draw_kind.iter())
                            .any(|((c, _, _, _), &k)| {
                                *c == cid && k == crate::raster_common::DRAW_KIND_CLUSTERED
                            });
                        if !routed {
                            continue;
                        }
                        let region = render::cluster_cut::ClusterCutSystem::region_id(slot, 0);
                        // Debug view: same args, unshared per-cluster-colored stream
                        // (WP14: only when has_debug_stream; else production verts until fill).
                        if self.render.dbg_cluster_view && entry.has_debug_stream {
                            rpass.set_vertex_buffer(0, entry.debug_vertex_buffer.slice(..));
                            rpass.set_index_buffer(
                                entry.debug_index_buffer.slice(..),
                                wgpu::IndexFormat::Uint32,
                            );
                            {
                                use std::cell::Cell;
                                thread_local! {
                                    static LAST: Cell<Option<std::time::Instant>> =
                                        const { Cell::new(None) };
                                }
                                let due = LAST.with(|c| {
                                    let now = std::time::Instant::now();
                                    let due = c
                                        .get()
                                        .map_or(true, |t| now.duration_since(t).as_secs_f32() >= 2.0);
                                    if due {
                                        c.set(Some(now));
                                    }
                                    due
                                });
                                if due {
                                    eprintln!(
                                        "[ClusterDebugView] BOUND cid={cid:?} n_clusters={} \
                                         (_pad.y + uv.y≥2 palette)",
                                        entry.n_clusters
                                    );
                                }
                            }
                        } else {
                            if self.render.dbg_cluster_view && !entry.has_debug_stream {
                                use std::cell::Cell;
                                thread_local! {
                                    static LAST: Cell<Option<std::time::Instant>> =
                                        const { Cell::new(None) };
                                }
                                let due = LAST.with(|c| {
                                    let now = std::time::Instant::now();
                                    let due = c
                                        .get()
                                        .map_or(true, |t| now.duration_since(t).as_secs_f32() >= 2.0);
                                    if due {
                                        c.set(Some(now));
                                    }
                                    due
                                });
                                if due {
                                    eprintln!(
                                        "[ClusterDebugView] STUB cid={cid:?} — waiting for \
                                         ensure_debug_stream (toggle fills next poll)"
                                    );
                                }
                            }
                            rpass.set_vertex_buffer(0, entry.vertex_buffer.slice(..));
                            rpass.set_index_buffer(
                                entry.index_buffer.slice(..),
                                wgpu::IndexFormat::Uint32,
                            );
                        }
                        rpass.set_vertex_buffer(1, gpu.raster.instance_buffer.slice(..));
                        if cut.use_count {
                            rpass.multi_draw_indexed_indirect_count(
                                &cut.args_buffer,
                                render::cluster_cut::ClusterCutSystem::args_byte_offset(region),
                                &cut.count_buffer,
                                render::cluster_cut::ClusterCutSystem::count_byte_offset(region),
                                render::cluster_cut::MAX_VISIBLE_CLUSTERS_PER_CID,
                            );
                        } else {
                            rpass.multi_draw_indexed_indirect(
                                &cut.args_buffer,
                                render::cluster_cut::ClusterCutSystem::args_byte_offset(region),
                                render::cluster_cut::MAX_VISIBLE_CLUSTERS_PER_CID,
                            );
                        }
                        draws += 1;
                        cluster_md_draws += 1;
                    }
                    if self.render.dbg_cluster_view && cluster_md_draws == 0 {
                        use std::cell::Cell;
                        thread_local! {
                            static LAST: Cell<Option<std::time::Instant>> =
                                const { Cell::new(None) };
                        }
                        let due = LAST.with(|c| {
                            let now = std::time::Instant::now();
                            let due = c
                                .get()
                                .map_or(true, |t| now.duration_since(t).as_secs_f32() >= 2.0);
                            if due {
                                c.set(Some(now));
                            }
                            due
                        });
                        if due {
                            let pool_n = gpu.cluster_pool.entries.len();
                            let has_any_dbg = gpu
                                .cluster_pool
                                .entries
                                .values()
                                .any(|e| e.has_debug_stream);
                            eprintln!(
                                "[ClusterDebugView] NO multi_draw (palette invisible) \
                                 pool={pool_n} has_debug={has_any_dbg} mesh_fb={} — need \
                                 resident DAG + CLUSTERED route (dense 1x9 / poly ≥50k tris; \
                                 demo landscape has 0 candidates)",
                                u32::from(self.render.cluster_overflow_mesh_fallback),
                            );
                        }
                    }
                } else if self.render.dbg_cluster_view {
                    use std::cell::Cell;
                    thread_local! {
                        static LAST: Cell<Option<std::time::Instant>> =
                            const { Cell::new(None) };
                    }
                    let due = LAST.with(|c| {
                        let now = std::time::Instant::now();
                        let due = c
                            .get()
                            .map_or(true, |t| now.duration_since(t).as_secs_f32() >= 2.0);
                        if due {
                            c.set(Some(now));
                        }
                        due
                    });
                    if due {
                        eprintln!(
                            "[ClusterDebugView] cut system not created yet — wait for \
                             [ClusterGpu] resident (bake + upload)"
                        );
                    }
                }
            } else if self.render.dbg_cluster_view {
                use std::cell::Cell;
                thread_local! {
                    static LAST: Cell<Option<std::time::Instant>> =
                        const { Cell::new(None) };
                }
                let due = LAST.with(|c| {
                    let now = std::time::Instant::now();
                    let due = c
                        .get()
                        .map_or(true, |t| now.duration_since(t).as_secs_f32() >= 2.0);
                    if due {
                        c.set(Some(now));
                    }
                    due
                });
                if due {
                    eprintln!(
                        "[ClusterDebugView] Clusters OFF — enable Tweaks→Clusters or \
                         AZRA_CLUSTERS=1 (debug view alone cannot paint)"
                    );
                }
            }
            // D1: per-batch indirect encode — the GPU wrote instance_count per batch; the CPU
            // encodes exactly one draw per batch regardless of instance count (the T1 contract).
            // Zero-survivor batches self-skip via instance_count=0; a mesh whose GPU buffers
            // haven't landed yet had index_count=0 baked into its args (no-op draw).
            if gpu_cull_active {
                if let Some(sys) = gpu.raster_cull.as_ref() {
                    for (bi, b) in self.cull_batches.iter().enumerate() {
                        if let Some(cached) = self.cache.mesh_cache.get(b.cid) {
                            if let Some(ref gpu_mesh) = cached.gpu_mesh {
                                rpass.set_vertex_buffer(0, gpu_mesh.vertex_buffer.slice(..));
                                rpass.set_vertex_buffer(
                                    1,
                                    sys.visible_buffer.slice(sys.visible_offset(0, b.first)..),
                                );
                                rpass.set_index_buffer(
                                    gpu_mesh.index_buffer.slice(..),
                                    gpu_mesh.index_format,
                                );
                                rpass.draw_indexed_indirect(&sys.draw_args_buffer, sys.args_offset(0, bi));
                                draws += 1;
                                if b.count > 1 {
                                    batches += 1;
                                    merged += b.count - 1;
                                }
                            }
                        }
                    }
                }
            }
            self.render.last_instance_draws = draws;
            self.render.last_instance_batches = batches;
            self.render.last_instance_merged = merged;
        }

        // --- D2: HZB max-pyramid build from THIS frame's depth ---
        // Runs after the G-buffer pass wrote depth. Next frame's early cull (and, from
        // C4 on, this frame's late cull) sample it. write_buffer + bind groups here are
        // fine (creation happened in poll_hzb); encode_build sets `valid`.
        // B2: probe mini-frames leave the main-view pyramid untouched (probe depth is
        // a different camera and would early-cull the real view for the rest of the round).
        if !is_probe && !std::env::var("AZRA_NO_GPU_CULL").is_ok_and(|v| v == "1") {
            if let (Some(hzb), Some(depth_view)) = (gpu.hzb.as_mut(), gpu.raster.depth_view.as_ref()) {
                hzb.write_level_params(&queue);
                hzb.ensure_bind_groups(&device, depth_view, gpu.raster.depth_generation);
                hzb.encode_build(&mut enc);
            }
        }

        // --- C4: late cull + second G-buffer pass (two-pass HZB disocclusion) ---
        // The pyramid above was built from pass A's depth; instances the early pass
        // rejected by (prev-frame) HZB retest against it and the survivors draw in a
        // LoadOp::Load render pass. Then the pyramid rebuilds once more so NEXT
        // frame's early pass sees the complete depth (skipping this feeds an
        // over-cull feedback loop).
        if hzb_two_pass {
            if let Some(sys) = gpu.raster_cull.as_ref() {
                let mut cp = enc.begin_compute_pass(&wgpu::ComputePassDescriptor {
                    label: Some("instance_cull_late"),
                    timestamp_writes: None,
                });
                cp.set_pipeline(&sys.pipeline_reset);
                cp.set_bind_group(0, sys.pass_bind_group(render::instance_cull::PASS_CAMERA_LATE), &[]);
                cp.dispatch_workgroups(self.render.cull_batches_ready.div_ceil(64), 1, 1);
                cp.set_pipeline(&sys.pipeline_late);
                cp.dispatch_workgroups((draw_calls.len() as u32).div_ceil(64), 1, 1);
            }
            {
                let mut lpass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("gbuffer_raster_late"),
                    color_attachments: &[
                        Some(wgpu::RenderPassColorAttachment {
                            view: gn, resolve_target: None,
                            ops: wgpu::Operations { load: wgpu::LoadOp::Load, store: wgpu::StoreOp::Store },
                        }),
                        Some(wgpu::RenderPassColorAttachment {
                            view: gm, resolve_target: None,
                            ops: wgpu::Operations { load: wgpu::LoadOp::Load, store: wgpu::StoreOp::Store },
                        }),
                        Some(wgpu::RenderPassColorAttachment {
                            view: ga, resolve_target: None,
                            ops: wgpu::Operations { load: wgpu::LoadOp::Load, store: wgpu::StoreOp::Store },
                        }),
                        Some(wgpu::RenderPassColorAttachment {
                            view: gmot, resolve_target: None,
                            ops: wgpu::Operations { load: wgpu::LoadOp::Load, store: wgpu::StoreOp::Store },
                        }),
                    ],
                    depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                        view: dv,
                        depth_ops: Some(wgpu::Operations { load: wgpu::LoadOp::Load, store: wgpu::StoreOp::Store }),
                        stencil_ops: None,
                    }),
                    timestamp_writes: None,
                    occlusion_query_set: None,
                });
                lpass.set_pipeline(&gpu.raster.pipeline);
                if let Some(ref bg) = gpu.raster.bind_group {
                    lpass.set_bind_group(0, bg, &[]);
                }
                if let Some(ref sbg) = gpu.raster.shadow_bind_group {
                    lpass.set_bind_group(1, sbg, &[]);
                }
                lpass.set_bind_group(2, &gpu.raster.curvature_dummy_bind_group, &[]);
                if let Some(sys) = gpu.raster_cull.as_ref() {
                    const LATE: usize = render::instance_cull::PASS_CAMERA_LATE;
                    for (bi, b) in self.cull_batches.iter().enumerate() {
                        if let Some(cached) = self.cache.mesh_cache.get(b.cid) {
                            if let Some(ref gpu_mesh) = cached.gpu_mesh {
                                lpass.set_vertex_buffer(0, gpu_mesh.vertex_buffer.slice(..));
                                lpass.set_vertex_buffer(
                                    1,
                                    sys.visible_buffer.slice(sys.visible_offset(LATE, b.first)..),
                                );
                                lpass.set_index_buffer(
                                    gpu_mesh.index_buffer.slice(..),
                                    gpu_mesh.index_format,
                                );
                                lpass.draw_indexed_indirect(&sys.draw_args_buffer, sys.args_offset(LATE, bi));
                            }
                        }
                    }
                }
            }
            // HZB rebuild #2: fold pass B's depth in for next frame's early cull.
            if let Some(hzb) = gpu.hzb.as_mut() {
                hzb.encode_build(&mut enc);
            }
        }

        // --- GPU DC-wire pass (hardware triangle edges) ---
        // Settings: color / opacity / thickness (0 = off) / backfaces — all connected.
        // Probe faces capture scene radiance only — never wire overlay into the cube.
        let want_gpu_wire = !is_probe
            && (self.render.shading_mode == crate::ShadingMode::Wireframe
                || self.render.wire_overlay_enabled)
            && self.render.wire_overlay_thickness > 0.001
            && self.render.wire_overlay_opacity > 0.001;
        if want_gpu_wire {
            let wire_pipe = if self.render.wire_overlay_backface {
                gpu.raster.wire_pipeline.as_ref()
            } else {
                gpu.raster
                    .wire_pipeline_front
                    .as_ref()
                    .or(gpu.raster.wire_pipeline.as_ref())
            };
            // POLY BODIES ARE EXCLUDED FROM THIS PASS (owner 2026-08-07: "the wireframe
            // is looking very bad, it triangulated — just use the same wireframe from
            // edit"). This is POLYGON_MODE_LINE over the TRIANGLE stream, so it can only
            // ever draw triangle edges: every quad shows its tessellation diagonal and a
            // beveled cube reads as a triangulated mess. That is the correct and only
            // possible wire for a DC/voxel mesh (a genuine triangle soup with no authored
            // topology — hence "DC wire"), but a poly body HAS real quad/n-gon topology
            // and an exact edge wire already exists: `collect_poly_local_edges` on the
            // half-edge mesh, drawn through the line overlay. Edit mode has always used
            // it; the object/modifier-result wire routes through the SAME path
            // (`poly_append_display_wire` → display mesh), so excluding poly here makes
            // both modes show identical, real edges instead of two disagreeing wires.
            let poly_wire_cids: std::collections::HashSet<u64> = self
                .scene
                .scene_graph
                .poly_bodies()
                .filter_map(|(_, n)| match &n.payload {
                    core_math::scene_graph::NodePayload::PolygonalBody { mesh_cache_id, .. } => {
                        mesh_cache_id.map(|c| c.0)
                    }
                    _ => None,
                })
                .collect();
            if let Some(wire_pipe) = wire_pipe {
                let mut wpass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("gbuffer_wire"),
                    color_attachments: &[
                        Some(wgpu::RenderPassColorAttachment {
                            view: gn,
                            resolve_target: None,
                            ops: wgpu::Operations {
                                load: wgpu::LoadOp::Load,
                                store: wgpu::StoreOp::Store,
                            },
                        }),
                        Some(wgpu::RenderPassColorAttachment {
                            view: gm,
                            resolve_target: None,
                            ops: wgpu::Operations {
                                load: wgpu::LoadOp::Load,
                                store: wgpu::StoreOp::Store,
                            },
                        }),
                        Some(wgpu::RenderPassColorAttachment {
                            view: ga,
                            resolve_target: None,
                            ops: wgpu::Operations {
                                load: wgpu::LoadOp::Load,
                                store: wgpu::StoreOp::Store,
                            },
                        }),
                        Some(wgpu::RenderPassColorAttachment {
                            view: gmot,
                            resolve_target: None,
                            ops: wgpu::Operations {
                                load: wgpu::LoadOp::Load,
                                store: wgpu::StoreOp::Store,
                            },
                        }),
                    ],
                    depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                        view: dv,
                        depth_ops: Some(wgpu::Operations {
                            load: wgpu::LoadOp::Load,
                            store: wgpu::StoreOp::Store,
                        }),
                        stencil_ops: None,
                    }),
                    timestamp_writes: None,
                    occlusion_query_set: None,
                });
                wpass.set_pipeline(wire_pipe);
                if let Some(ref bg) = gpu.raster.bind_group {
                    wpass.set_bind_group(0, bg, &[]);
                }
                if let Some(ref sbg) = gpu.raster.shadow_bind_group {
                    wpass.set_bind_group(1, sbg, &[]);
                }
                // GPU_CURVATURE_KERNEL_PLAN.md P2: same pipeline layout as the main G-buffer pass
                // (shares `pipeline_layout` in RasterPipeline::new) — group 2 must be bound here
                // too, or this pass's draws fail validation. Wire is a debug/overlay view of the
                // same vertices; it does not need curvature to be numerically correct (only
                // `fs_gbuffer` shades from it), but the binding is mandatory regardless.
                wpass.set_bind_group(2, &gpu.raster.curvature_dummy_bind_group, &[]);
                let curv_bind: crate::raster_common::CurvBind<'_> =
                    Some((&gpu.raster.curvature_dummy_bind_group, &curv_bgs));
                if !sculpt_raymarch_overlay {
                    let first_vox = draw_kind
                        .iter()
                        .position(|&k| k == DRAW_KIND_VOXEL_RESIDENT);
                    draw_resident_pool(&mut wpass, gpu, first_vox, instance_stride);
                }
                let sdf_pool_wire = draw_sdf_pool_multi(&mut wpass, gpu, curv_bind);
                // P4: same multi-instance batching as G-buffer for wire overlay.
                let mut i = 0usize;
                while i < draw_calls.len() {
                    let (cid, _, cull, irange) = draw_calls[i];
                    if draw_skip_main(cull) {
                        i += 1;
                        continue;
                    }
                    let kind = draw_kind[i];
                    if kind == DRAW_KIND_VOXEL_RESIDENT {
                        i += 1;
                        continue;
                    }
                    // WP6: CLUSTERED is drawn only by the cluster multi-draw (G-buffer).
                    // Drawing source mesh here re-introduced dense dual-path cost / mismatch.
                    if kind == crate::raster_common::DRAW_KIND_CLUSTERED {
                        i += 1;
                        continue;
                    }
                    // REVERTED 2026-08-07 (owner: "now I have no wire frame"). Excluding
                    // poly here removed the ONLY VISIBLE object wire: the line-overlay
                    // replacement emits its segments into the DEPTH-TESTED batch, where a
                    // wire lying exactly on its own surface z-fights and disappears. The
                    // edit wire looks fine only because it is drawn in the X-RAY batch
                    // (`poly_edit_wire_xray`). Re-enable this skip ONLY together with a
                    // depth bias for object wire lines (emit_local_edges has width/opacity
                    // but NO bias parameter today) or by moving it to the x-ray batch.
                    let _ = &poly_wire_cids;
                    let byte_off = i as u64 * instance_stride;
                    if kind == DRAW_KIND_SDF_POOLED || kind == DRAW_KIND_SDF_RESIDENT {
                        let res_nid = core_math::scene_graph::NodeId(draw_kind_payload[i]);
                        if kind == DRAW_KIND_SDF_POOLED {
                            if !sdf_pool_wire {
                                draw_sdf_pool_slot(
                                    &mut wpass,
                                    gpu,
                                    res_nid,
                                    gpu.raster.instance_buffer.slice(byte_off..),
                                    curv_bind,
                                );
                            }
                            i += 1;
                            continue;
                        }
                        draw_sdf_resident_slot(
                            &mut wpass,
                            gpu,
                            res_nid,
                            gpu.raster.instance_buffer.slice(byte_off..),
                            curv_bind,
                        );
                        i += 1;
                        continue;
                    }
                    let mut j = i + 1;
                    while j < draw_calls.len() {
                        let (c2, _, cull2, r2) = draw_calls[j];
                        if draw_skip_main(cull2)
                            || draw_kind_is_special(draw_kind[j])
                            || c2 != cid
                            || r2 != irange
                        {
                            break;
                        }
                        j += 1;
                    }
                    let inst_count = (j - i) as u32;
                    if let Some(cached) = self.cache.mesh_cache.get(cid) {
                        if let Some(ref gpu_mesh) = cached.gpu_mesh {
                            wpass.set_vertex_buffer(0, gpu_mesh.vertex_buffer.slice(..));
                            wpass.set_vertex_buffer(1, gpu.raster.instance_buffer.slice(byte_off..));
                            wpass.set_index_buffer(
                                gpu_mesh.index_buffer.slice(..),
                                gpu_mesh.index_format,
                            );
                            let (s, c) = irange.unwrap_or((0, gpu_mesh.index_count));
                            if c > 0 && inst_count > 0 {
                                wpass.draw_indexed(s..(s + c), 0, 0..inst_count);
                            }
                        }
                    }
                    i = j;
                }
            }
        }

        self.perf.raster_gbuf_enc_ms = _t_gbuf.elapsed().as_secs_f32() * 1000.0;

        // C5: counters → staging (≤1 in flight; camera passes wrote them this frame).
        if gpu_cull_active {
            if let Some(sys) = gpu.raster_cull.as_ref() {
                sys.encode_counter_copy(&mut enc);
            }
        }

        // D2 one-shot proof (AZRA_HZB_DUMP=1): copy an 8×8 mip out and log min/max —
        // max-pyramid sanity (background=1.0 far must dominate, geometry <1.0 must
        // survive the reduce). Diagnostic-only, fires once, blocking poll is fine here.
        let hzb_dump = std::env::var("AZRA_HZB_DUMP").is_ok_and(|v| v == "1")
            && !self.render.hzb_dumped
            && gpu.hzb.as_ref().is_some_and(|h| h.valid);
        let hzb_dump_buf = if hzb_dump {
            let hzb = gpu.hzb.as_ref().expect("checked");
            let m = (0..hzb.mip_count).find(|&m| (hzb.size >> m) == 8).unwrap_or(0);
            let rb = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("hzb_dump"),
                size: 8 * 256, // 8 rows × 256-aligned (8 texels × 4B = 32 → padded)
                usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                mapped_at_creation: false,
            });
            enc.copy_texture_to_buffer(
                wgpu::ImageCopyTexture {
                    texture: &hzb.texture,
                    mip_level: m,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::ImageCopyBuffer {
                    buffer: &rb,
                    layout: wgpu::ImageDataLayout { offset: 0, bytes_per_row: Some(256), rows_per_image: Some(8) },
                },
                wgpu::Extent3d { width: 8, height: 8, depth_or_array_layers: 1 },
            );
            Some((rb, m))
        } else {
            None
        };

        // Submit before the SDF compute command buffer
        let _t_rsub = std::time::Instant::now();
        queue.submit(Some(enc.finish()));
        self.perf.raster_submit_ms = _t_rsub.elapsed().as_secs_f32() * 1000.0;

        // C5: drain/start the counter readback (non-blocking, 1–2 frames behind) and
        // surface it through the EXISTING occlusion scoreboard fields (occCull=a/b):
        // tested = occlusion-eligible (in-frustum), culled = net HZB-hidden this frame.
        if gpu_cull_active {
            if let Some(sys) = gpu.raster_cull.as_mut() {
                sys.poll_counters();
                let [tested, frustum, hzb_early, late, ..] = sys.last_counters;
                self.render.last_occlusion_tested = tested.saturating_sub(frustum);
                self.render.last_occlusion_culled = hzb_early.saturating_sub(late);
                self.render.hzb_late_recovered = late;
            }
        }

        // D4 commit 8 (AZRA_CLUSTER_CUT_DUMP=1, one-shot): read back the camera
        // region's args and verify the RENDERED cut — (a) the GPU-selected set is
        // exactly the CPU replay's set for this camera, (b) the union of selected
        // clusters is watertight (closed source ⇒ boundary edge set of the cut is
        // empty — the "watertight gate re-run on the rendered cut" clause).
        if std::env::var("AZRA_CLUSTER_CUT_DUMP").is_ok_and(|v| v == "1")
            && !self.render.hzb_dumped
            && gpu.cluster_cut.as_ref().is_some_and(|c| c.last_counters[4] > 0)
            && !gpu.cluster_pool.entries.is_empty()
        {
            self.render.hzb_dumped = true; // reuse the one-shot latch (diagnostic)
            let cut = gpu.cluster_cut.as_ref().expect("checked");
            if let Some((slot, (cid, entry))) =
                render::cluster_cut::ClusterCutSystem::stable_cid_slots(&gpu.cluster_pool).next()
            {
                let region = render::cluster_cut::ClusterCutSystem::region_id(slot, 0);
                let cap = render::cluster_cut::MAX_VISIBLE_CLUSTERS_PER_CID as u64;
                let rb = device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("cut_dump"),
                    size: cap * 20,
                    usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                    mapped_at_creation: false,
                });
                let mut enc2 = device.create_command_encoder(&Default::default());
                enc2.copy_buffer_to_buffer(
                    &cut.args_buffer,
                    render::cluster_cut::ClusterCutSystem::args_byte_offset(region),
                    &rb,
                    0,
                    cap * 20,
                );
                queue.submit(Some(enc2.finish()));
                let (tx, rx) = std::sync::mpsc::channel();
                rb.slice(..).map_async(wgpu::MapMode::Read, move |r| {
                    let _ = tx.send(r);
                });
                device.poll(wgpu::PollType::Wait).expect("GPU device polling failed");
                if rx.recv().map(|r| r.is_ok()).unwrap_or(false) {
                    let data = rb.slice(..).get_mapped_range();
                    let words: &[u32] = bytemuck::cast_slice(&data);
                    // Selected = args with index_count > 0; identify clusters by
                    // their unique index_offset (first_index in the args).
                    let mut gpu_sel: Vec<(u32, u32)> = Vec::new();
                    for a in words.chunks_exact(5) {
                        if a[0] > 0 {
                            gpu_sel.push((a[2], a[0])); // (first_index, index_count)
                        }
                    }
                    drop(data);
                    if let Some((_, dag)) = self.cache.cluster_bake.dags.get(&cid) {
                        let flat = render::cluster_gpu::flatten_clusters(dag);
                        let cam = render::cluster_gpu::CutCamera {
                            position: [camera.cam_pos[0], camera.cam_pos[1], camera.cam_pos[2]],
                            viewport_h: gpu.raster.depth_size.1.max(1) as f32,
                            tan_half_fov: 1.0 / camera.proj[1][1],
                        };
                        // Replay WITHOUT frustum/cone (error cut only) — GPU frustum/cone
                        // may remove more. World-space models required: identity replay
                        // lies for translated instances (dense-stress grid spacing 3.0).
                        // Models match cut fill: first MAX_INSTANCES of this cid in draw
                        // list order (same as instance_slots[i] → InstanceData.model_matrix).
                        let dump_tau: f32 = std::env::var("AZRA_CLUSTER_THRESHOLD_PX")
                            .ok()
                            .and_then(|v| v.parse().ok())
                            .unwrap_or(1.0);
                        let mut models: Vec<glam::Mat4> = Vec::new();
                        for (i, (c, inst, _, _)) in draw_calls.iter().enumerate() {
                            if *c != cid {
                                continue;
                            }
                            if draw_kind_is_special(draw_kind[i])
                                && draw_kind[i] != crate::raster_common::DRAW_KIND_CLUSTERED
                            {
                                continue;
                            }
                            if models.len() >= render::cluster_cut::MAX_INSTANCES_PER_CID {
                                break;
                            }
                            models.push(glam::Mat4::from_cols_array_2d(&inst.model_matrix));
                        }
                        if models.is_empty() {
                            eprintln!(
                                "[CUT_DUMP] {cid:?}: skip agreement — no draw instances for cid \
                                 (identity replay would lie if the body is translated)"
                            );
                        } else {
                            let replay: std::collections::BTreeSet<u32> =
                                render::cluster_gpu::cut_replay_world_union(
                                    &flat, &cam, dump_tau, &models,
                                )
                                .into_iter()
                                .collect();
                            let by_offset: std::collections::HashMap<u32, u32> = dag
                                .clusters
                                .iter()
                                .enumerate()
                                .map(|(i, c)| (c.index_offset, i as u32))
                                .collect();
                            let mut mismatches = 0;
                            let mut union_idx: Vec<u32> = Vec::new();
                            for &(fi, ic) in &gpu_sel {
                                match by_offset.get(&fi) {
                                    Some(&ci) if replay.contains(&ci) => {
                                        union_idx.extend_from_slice(
                                            &dag.indices[fi as usize..(fi + ic) as usize],
                                        );
                                    }
                                    _ => mismatches += 1,
                                }
                            }
                            // Watertight: closed source ⇒ the visible cut's boundary set is
                            // exactly the frustum seam… for a fully-in-frustum body it is
                            // EMPTY. Report the count (0 required when body fully visible).
                            let mut edge_count: std::collections::BTreeMap<(u32, u32), u32> =
                                Default::default();
                            for t in union_idx.chunks_exact(3) {
                                for (a, b) in [(t[0], t[1]), (t[1], t[2]), (t[2], t[0])] {
                                    *edge_count.entry((a.min(b), a.max(b))).or_insert(0) += 1;
                                }
                            }
                            let open_edges = edge_count.values().filter(|&&n| n == 1).count();
                            eprintln!(
                                "[CUT_DUMP] {cid:?}: gpu_selected={} replay={} mismatches={mismatches} \
                                 open_edges={open_edges} instances={} (clusters={})",
                                gpu_sel.len(),
                                replay.len(),
                                models.len(),
                                entry.n_clusters,
                            );
                        }
                    }
                }
            }
        }

        // D4: cluster-cut counter drain (non-blocking) + overflow policy (WP11) + logs.
        if let Some(cut) = gpu.cluster_cut.as_mut() {
            if !gpu.cluster_pool.entries.is_empty() {
                cut.poll_counters();
                // Copy stats before any further &mut cut uses (poll_args_hash).
                let tested = cut.last_counters.first().copied().unwrap_or(0);
                let frustum_n = cut.last_counters.get(1).copied().unwrap_or(0);
                let cone_n = cut.last_counters.get(2).copied().unwrap_or(0);
                let not_cut = cut.last_counters.get(3).copied().unwrap_or(0);
                let selected = cut.last_counters.get(4).copied().unwrap_or(0);
                let overflow = cut.last_counters.get(5).copied().unwrap_or(0);
                // Region counters start at word 8; region_id(slot0, pass0)=0 → camera multi_draw.
                let cam_selected = cut.last_counters.get(8).copied().unwrap_or(0);
                self.render.cluster_cut_selected = selected;
                self.render.cluster_cut_cam_selected = cam_selected;
                self.render.cluster_cut_overflow = overflow;
                // WP11: overflow OR incomplete cut partition → MESH (dense-stress
                // fragment islands had clOv=0 — pure cut holes). Sticky until clear.
                if overflow > 0 {
                    self.render.cluster_overflow_streak =
                        self.render.cluster_overflow_streak.saturating_add(1);
                    self.render.cluster_overflow_clear_streak = 0;
                    if self.render.cluster_overflow_streak == 1
                        || self.render.cluster_overflow_streak % 60 == 0
                    {
                        eprintln!(
                            "[ClusterCut] OVERFLOW={overflow} selected={selected} cap={} \
                             (dropped clusters = F-HOLE / fragment flicker)",
                            render::cluster_cut::MAX_VISIBLE_CLUSTERS_PER_CID
                        );
                    }
                    if !self.render.cluster_overflow_mesh_fallback {
                        self.render.cluster_overflow_mesh_fallback = true;
                        // D1 cull batches are rebuild-only: MESH demote without a
                        // structural rebuild leaves instances on the u32::MAX sentinel
                        // → invisible (main + shadows) until an unrelated edit.
                        self.structural_draw_stale = true;
                        eprintln!(
                            "[ClusterCut] WP11 MESH fallback ON (overflow={overflow} selected={selected})"
                        );
                    }
                } else if selected > 0 && self.render.cluster_coverage_probe_age == 0 {
                    // Throttled CPU partition probe (error cut only) — catches clOv=0 holes.
                    // Dense-stress owner repro: clSel~7k clOv=0 but sparse fragment islands.
                    self.render.cluster_coverage_probe_age = 45;
                    let thr: f32 = std::env::var("AZRA_CLUSTER_THRESHOLD_PX")
                        .ok()
                        .and_then(|v| v.parse().ok())
                        .unwrap_or(1.0);
                    let probe_cam = render::cluster_gpu::CutCamera {
                        position: [camera.cam_pos[0], camera.cam_pos[1], camera.cam_pos[2]],
                        viewport_h: gpu.raster.depth_size.1.max(1) as f32,
                        tan_half_fov: 1.0 / camera.proj[1][1].max(1e-6),
                    };
                    let mut probe_bad = 0u32;
                    let mut probe_sel = 0usize;
                    let mut probe_cid = None;
                    // Round-robin slotted cids — slot-0 only never trips holes on cid 2..8.
                    let slotted: Vec<_> =
                        render::cluster_cut::ClusterCutSystem::stable_cid_slots(&gpu.cluster_pool)
                            .map(|(_, (cid, _))| cid)
                            .collect();
                    if !slotted.is_empty() {
                        let i = (self.render.cluster_coverage_probe_rr as usize) % slotted.len();
                        self.render.cluster_coverage_probe_rr =
                            self.render.cluster_coverage_probe_rr.wrapping_add(1);
                        let cid = slotted[i];
                        if let Some((_, dag)) = self.cache.cluster_bake.dags.get(&cid) {
                            let flat = render::cluster_gpu::flatten_clusters(dag);
                            // World model of the first draw of this cid (dense-stress
                            // multi-body grid is offset, not identity).
                            let model = draw_calls
                                .iter()
                                .find(|(c, _, _, _)| *c == cid)
                                .map(|(_, inst, _, _)| {
                                    glam::Mat4::from_cols_array_2d(&inst.model_matrix)
                                })
                                .unwrap_or(glam::Mat4::IDENTITY);
                            let sel = render::cluster_gpu::cut_replay_world(
                                &flat, &probe_cam, thr, model,
                            );
                            probe_sel = sel.len();
                            probe_bad =
                                render::cluster_gpu::cut_partition_failures(dag, &sel);
                            probe_cid = Some(cid);
                        }
                    }
                    if probe_bad > 0 {
                        if !self.render.cluster_overflow_mesh_fallback {
                            eprintln!(
                                "[ClusterCut] COVERAGE HOLE probe: {probe_bad} L0 regions \
                                 unclaimed (cpu_sel={probe_sel} thr={thr}) → MESH \
                                 (cid={probe_cid:?})"
                            );
                        }
                        self.render.cluster_overflow_mesh_fallback = true;
                        self.render.cluster_overflow_clear_streak = 0;
                        // Same D1-batch rule as overflow ON — force structural rebuild.
                        self.structural_draw_stale = true;
                    } else if self.render.cluster_overflow_mesh_fallback {
                        // Partition clean + no overflow: drop MESH fallback sooner so
                        // projected-band fix can re-enable clustered LOD (was 30 probes
                        // × 45 frames ≈ 20s stuck on MESH after a single false hole).
                        self.render.cluster_overflow_clear_streak =
                            self.render.cluster_overflow_clear_streak.saturating_add(1);
                        if self.render.cluster_overflow_clear_streak >= 3 {
                            self.render.cluster_overflow_mesh_fallback = false;
                            self.render.cluster_overflow_streak = 0;
                            self.render.cluster_overflow_clear_streak = 0;
                            // Remap now runs every frame, but still force a structural
                            // rebuild so instance/kind caches stay consistent.
                            self.structural_draw_stale = true;
                            eprintln!(
                                "[ClusterCut] WP11 MESH fallback OFF (partition clean, \
                                 cpu_sel={probe_sel})"
                            );
                        }
                    }
                } else if self.render.cluster_overflow_mesh_fallback {
                    // Waiting for next probe window — do not clear on overflow alone.
                } else {
                    self.render.cluster_overflow_streak = 0;
                    self.render.cluster_overflow_clear_streak = 0;
                }
                if self.render.cluster_coverage_probe_age > 0 {
                    self.render.cluster_coverage_probe_age =
                        self.render.cluster_coverage_probe_age.saturating_sub(1);
                }
                if std::env::var("AZRA_FLICKER_WATCH").is_ok_and(|v| v == "1") {
                    if let Some((changed, n)) = cut.poll_args_hash() {
                        if changed {
                            eprintln!(
                                "[FLICKER] set CHANGED: {n} clusters | full={} geom={} of {} samples",
                                cut.args_changes, cut.geom_changes, cut.args_samples
                            );
                        } else if cut.args_samples % 60 == 0 {
                            eprintln!(
                                "[FLICKER] stable: {n} clusters | full={} geom={} of {} samples",
                                cut.args_changes, cut.geom_changes, cut.args_samples
                            );
                        }
                    }
                }
                if std::env::var("AZRA_CLUSTER_LOG").is_ok_and(|v| v == "1") {
                    use std::cell::Cell;
                    thread_local! { static LAST: Cell<Option<std::time::Instant>> = Cell::new(None); }
                    let due = LAST.with(|c| {
                        let now = std::time::Instant::now();
                        let due = c.get().map_or(true, |t| now.duration_since(t).as_secs_f32() >= 2.0);
                        if due { c.set(Some(now)); }
                        due
                    });
                    if due {
                        eprintln!(
                            "[ClusterCut] tested={tested} frustum={frustum_n} cone={cone_n} \
                             not_in_cut={not_cut} selected_all={selected} cam={cam_selected} \
                             overflow={overflow} mesh_fb={}",
                            u32::from(self.render.cluster_overflow_mesh_fallback)
                        );
                    }
                }
            }
        }

        // D2 dump: blocking one-shot map (diagnostic env only — never the hot path).
        if let Some((rb, m)) = hzb_dump_buf {
            self.render.hzb_dumped = true;
            let (tx, rx) = std::sync::mpsc::channel();
            rb.slice(..).map_async(wgpu::MapMode::Read, move |r| {
                let _ = tx.send(r);
            });
            device.poll(wgpu::PollType::Wait).expect("GPU device polling failed");
            if rx.recv().map(|r| r.is_ok()).unwrap_or(false) {
                let data = rb.slice(..).get_mapped_range();
                let (mut mn, mut mx, mut n_geo) = (f32::MAX, f32::MIN, 0u32);
                for row in 0..8usize {
                    for col in 0..8usize {
                        let off = row * 256 + col * 4;
                        let v = f32::from_le_bytes(data[off..off + 4].try_into().unwrap());
                        mn = mn.min(v);
                        mx = mx.max(v);
                        if v < 1.0 { n_geo += 1; }
                    }
                }
                eprintln!("[HZB_DUMP] mip={m} (8x8) min={mn:.4} max={mx:.4} texels<1.0={n_geo}/64");
            }
        }
        // Return GPU timestamp system (after submit — queue borrow released)
        gpu.gpu_ts = gpu_ts;

        // Restore structural cache: transform-stable, cull=0.
        // D1/T1: do NOT re-sort to cid order every frame — that forced packing to clone 100k
        // InstanceData on every idle camera frame. Keep packed (visible-first) order so the
        // next frame's packing is a no-op when the cull set is stable. Structural rebuild
        // still sorts by (special, cid, irange) once.
        for e in &mut draw_calls {
            e.2 = 0;
        }
        self.structural_materials = materials;
        self.structural_draw_calls = draw_calls;
        self.structural_draw_kind = draw_kind;
        self.structural_draw_kind_payload = draw_kind_payload;
        self.structural_world_aabb = world_aabbs;

        any_sdf_rasterized
    }
}

#[cfg(test)]
mod tests {
    //! Material G-buffer resolution regression tests (black-PBR bug, 2026-07-17).
    //! Headless: asserts the exact RasterMaterial resolution chain the SDF raster
    //! branch above uses (resolve_prim_material → RasterMaterial::from_pbr → albedo),
    //! without needing a GPU.
    use core_math::primitives::{PbrMaterial, SdfPrimitive, SdfShape};
    use core_math::scene_graph::{NodePayload, SceneGraph};
    use render::mesh_gpu::RasterMaterial;

    fn test_prim() -> SdfPrimitive {
        SdfPrimitive::new(
            "p".into(),
            SdfShape::Sphere { radius: 1.0 },
            glam::Vec3::ZERO,
        )
    }

    /// A default-material SDF prim (MaterialId::NONE, "no material input") must resolve to a
    /// NON-BLACK albedo in the raster G-buffer chain — a DCC never renders black for "no material".
    #[test]
    fn default_material_prim_resolves_non_black_albedo() {
        let sg = SceneGraph::new();
        let p = test_prim();
        let resolved = sg.resolve_prim_material(&p);
        let rm = RasterMaterial::from_pbr(resolved);
        let luma = 0.2126 * rm.albedo[0] + 0.7152 * rm.albedo[1] + 0.0722 * rm.albedo[2];
        assert!(luma > 0.1, "default material must be a visible neutral, got {:?}", rm.albedo);
        assert!(rm.albedo[3] > 0.99, "default material must be opaque");
    }

    /// A prim referencing a graph Material node must surface THAT material's albedo in the
    /// raster chain (the pre-fix code read the embedded `p.material` and ignored material_id).
    #[test]
    fn assigned_graph_material_reaches_raster_material() {
        let mut sg = SceneGraph::new();
        let red = PbrMaterial { albedo: glam::Vec3::new(0.9, 0.1, 0.1), ..Default::default() };
        let mid = sg.add_material("Red", red);
        let mut p = test_prim();
        p.material_id = mid;
        // Embedded material deliberately left at default — the graph material must win.
        let rm = RasterMaterial::from_pbr(sg.resolve_prim_material(&p));
        assert!((rm.albedo[0] - 0.9).abs() < 1e-6 && (rm.albedo[1] - 0.1).abs() < 1e-6,
            "graph material albedo must win over embedded default, got {:?}", rm.albedo);
    }

    /// A dangling material_id must fall back to the embedded material, never to black.
    #[test]
    fn dangling_material_id_falls_back_to_embedded() {
        let sg = SceneGraph::new();
        let mut p = test_prim();
        p.material_id = core_math::primitives::MaterialId(9999);
        let rm = RasterMaterial::from_pbr(sg.resolve_prim_material(&p));
        let luma = 0.2126 * rm.albedo[0] + 0.7152 * rm.albedo[1] + 0.0722 * rm.albedo[2];
        assert!(luma > 0.1, "dangling id must fall back to embedded default, got {:?}", rm.albedo);
    }

    /// The default scene's "Default Material" node itself must be non-black (owner report:
    /// "PBR surface is always black maybe because I don't have any material input").
    #[test]
    fn default_material_node_is_non_black() {
        let mut sg = SceneGraph::new();
        let mid = sg.add_material("Default Material", PbrMaterial::default());
        let (_, mat) = sg.material_by_id(mid).expect("material node present");
        assert!(mat.albedo.max_element() > 0.1);
        // Keep the graph borrow-checker honest about NodePayload::Material existence.
        assert!(sg.all_nodes().any(|(_, n)| matches!(n.payload, NodePayload::Material { .. })));
    }

    /// M0 follow-up sabotage: roughness factor edit with a "static camera" structural path
    /// must surface the new value in RasterMaterial after gen-driven rebuild.
    ///
    /// Simulates: structural list baked at gen G with roughness 0.2 → mutate via
    /// material_by_id_mut → gen G+1 → rebuild would pull 0.9. Without the gen gate the
    /// cached RasterMaterial would stay at 0.2 (the RENDER-only stale class).
    #[test]
    fn roughness_factor_edit_invalidates_structural_material_gen() {
        let mut sg = SceneGraph::new();
        let mid = sg.add_material(
            "Mat",
            PbrMaterial {
                roughness: 0.2,
                ..Default::default()
            },
        );
        let mut p = test_prim();
        p.material_id = mid;
        let gen_before = sg.material_generation();
        let rm0 = RasterMaterial::from_pbr(sg.resolve_prim_material(&p));
        assert!(
            (rm0.roughness_metallic[0] - 0.2).abs() < 1e-5,
            "initial structural material roughness"
        );

        // "Slider tick" — only material content, no SCENE/MESH dirty equivalent.
        {
            let m = sg.material_by_id_mut(mid).expect("mat");
            m.roughness = 0.9;
        }
        let gen_after = sg.material_generation();
        assert!(
            gen_after != gen_before,
            "generation must change so structural reuse cannot keep gen_before"
        );
        // Sabotage check: if reuse compared equal gens, this would still be 0.2.
        let rm1 = RasterMaterial::from_pbr(sg.resolve_prim_material(&p));
        assert!(
            (rm1.roughness_metallic[0] - 0.9).abs() < 1e-5,
            "after material_by_id_mut, resolved RasterMaterial must show new roughness (got {})",
            rm1.roughness_metallic[0]
        );
        // Gate the reuse predicate the raster path implements.
        let structural_material_gen = gen_before;
        assert!(
            structural_material_gen != sg.material_generation(),
            "structural list stamped at gen_before must rebuild"
        );
    }

    /// The shadow-bias defaults exist in TWO places — `render::shadows::ShadowBias` and
    /// `app_types::RenderSettings` — because neither crate depends on the other. This
    /// is the only crate that can see both, so the agreement is gated here.
    ///
    /// Why it matters: the sliders are only safe to ship because their defaults
    /// reproduce the pre-slider image exactly. If the two default sites drift, the
    /// production path (which reads RenderSettings) silently stops being neutral while
    /// render's own test still passes.
    #[test]
    fn shadow_bias_defaults_agree_across_crates() {
        let r = app_types::RenderSettings::default();
        let d = render::shadows::ShadowBias::default();
        assert_eq!(
            (r.shadow_normal_offset, r.shadow_depth_bias, r.shadow_slope_bias),
            (d.normal_offset_texels, d.depth_bias_texels, d.slope_depth_bias_texels),
            "RenderSettings shadow-bias defaults drifted from render::shadows::ShadowBias"
        );
    }
}
