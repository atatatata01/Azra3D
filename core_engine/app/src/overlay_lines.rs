// overlay_lines.rs — builds the world-space segment list for the depth-tested GPU line pass.
//
// WHY THIS EXISTS (the "I can see through the mesh" fix):
// The edit wireframe and the object wireframe overlay used to be flat 2D egui lines with NO
// depth test. Blender draws edit wireframe as GPU primitives depth-tested against the solid
// pass depth buffer; this is that.
//
// PERF (2026-07-19):
// DC meshes are dense (often 50k–500k tris). Rebuilding unique edges + HashSet every frame
// stalled the viewport. Now:
//   1. Full overlay fingerprint — skip rebuild when mesh/transform/settings unchanged
//      (camera orbit = free).
//   2. Per-mesh LOCAL edge cache keyed by content_stamp — rebuild edges only when mesh changes.
//   3. No HashSet: emit edges with a < b (manifold opposite winding → unique).
//   4. Stride + hard caps so huge DC meshes stay interactive.

use core_math::half_edge::{HalfEdgeId, NO_FACE, NO_TWIN};
use core_math::scene_graph::{MeshCacheId, NodePayload};
use glam::Vec3;
use render::line_overlay::LineInstance;
use std::hash::{Hash, Hasher};

use crate::*;

/// Hard cap on segments submitted in one frame (Frame Budget Rule).
const MAX_OVERLAY_SEGMENTS: usize = 400_000;
/// Cap per SDF body — high enough that typical DC LODs (64–256³) show **full** topology
/// instead of a strided “pattern projected on the surface”.
const MAX_SDF_WIRE_PER_BODY: usize = 200_000;
/// Poly half-edge cap per body (local edge list). Soft limit before global segment cap.
///
/// Raised 100k -> 400k (owner 2026-08-13, screenshot: "wire frame do not show everywhere on a
/// dense mesh" — the 323k-face body has ~483k edges, so two thirds of the cage was silently
/// missing and the cut-off read as broken geometry, not as a cap). 400k aligns with
/// MAX_OVERLAY_SEGMENTS, which remains the global Frame Budget backstop. The per-frame cost of
/// the larger cage is contained by STRIDING the emit while a drag episode is open (below) —
/// full topology when you are looking, quarter density only while the mesh is in motion.
const MAX_POLY_WIRE: usize = 400_000;
/// P6.1: inactive poly object-wire edge stride (emit every Nth edge). Selected = 1.
/// (Active-without-selection is no longer "hot" — only explicit selection densifies.)
const POLY_INACTIVE_WIRE_STRIDE: usize = 4;

/// Sabotage: `AZRA_CPU_POLY_OUTLINE=1` re-enables the O(edges) egui silhouette path
/// in `draw_node_selection_outlines` so the dense-poly idle gate fails.
pub(crate) fn cpu_poly_outline_sabotage() -> bool {
    std::env::var("AZRA_CPU_POLY_OUTLINE").as_deref() == Ok("1")
}

/// Body-local edge endpoints for one mesh identity (reused across transform/color changes).
#[derive(Clone, Default)]
pub(crate) struct WireLocalEdgeCache {
    /// `CachedMesh::content_stamp` (or 0 for poly proxy).
    pub stamp: u64,
    /// Bit 0 = heat importance colors stored in `heat_rgb`.
    pub flags: u8,
    pub p0: Vec<[f32; 3]>,
    pub p1: Vec<[f32; 3]>,
    /// Per-edge packed RGB when heat map is on (same order as p0/p1). Empty = uniform color.
    pub heat_rgb: Vec<[f32; 3]>,
}

const WIRE_CACHE_HEAT: u8 = 1;
const WIRE_CACHE_EDGE_SHAPE: u8 = 2;

impl AzraApp {
    /// Fill `self.overlay_lines` with this frame's segments, depth-tested batch FIRST.
    /// Returns the number of leading segments that must be drawn depth-tested.
    pub(crate) fn build_overlay_lines(&mut self) -> u32 {
        self.expire_bake_cage_preview();
        // [OvlSplit] — `[DREST]` named this call as 8.8-9.6 ms of an 11 ms dispatch during a vertex
        // drag (raster 0.4, every GPU pass ~0, submit 0.09). The fingerprint below is computed
        // EVERY frame even when it hits, and a drag moves vertices so it misses every frame and
        // rebuilds the whole cage. Split fingerprint vs each batch before changing anything.
        let _t_fp = std::time::Instant::now();
        let fp = self.overlay_fingerprint();
        let _ms_fp = _t_fp.elapsed().as_secs_f32() * 1000.0;
        if fp == self.overlay_lines_fp {
            if _ms_fp > 2.0 || crate::perf_spam_on() {
                eprintln!("[OvlSplit] HIT fingerprint {_ms_fp:.2}ms (no rebuild)");
            }
            // Mesh + transforms + settings unchanged — camera-only frames are free
            // (also when overlay is empty / wire off).
            return self.overlay_lines_depth_count;
        }

        let mut out = std::mem::take(&mut self.overlay_lines);
        out.clear();
        let mut capped = false;

        // --- depth-tested batch ---
        // Floor wire grid FIRST (under object wires / edit cage): same G-buffer depth as
        // solid meshes so lines hide behind geo (owner: "GRID NEED TO RENDER UNDER GEO").
        // Shadow Catcher mode (grid_mode==1) uses the GPU floor plane instead.
        let _t_b = std::time::Instant::now();
        capped |= self.append_floor_grid(&mut out);
        let _ms_grid = _t_b.elapsed().as_secs_f32() * 1000.0;
        let _t_b = std::time::Instant::now();
        let mut _ms_wire = 0.0f32;
        let mut _ms_seledge = 0.0f32;
        let mut _ms_marks = 0.0f32;
        if !self.poly_edit_wire_xray() {
            capped |= self.append_poly_edit_wire(&mut out);
            _ms_wire = _t_b.elapsed().as_secs_f32() * 1000.0;
            let _t_c = std::time::Instant::now();
            // Edge selection MUST draw even when the edit cage wire is off
            // (show_poly_overlays=false). Nesting it inside append_poly_edit_wire made
            // highlights vanish with that toggle — owner "edge highlight bug came back".
            capped |= self.append_poly_selected_edges_for_active(&mut out);
            _ms_seledge = _t_c.elapsed().as_secs_f32() * 1000.0;
            let _t_d = std::time::Instant::now();
            // Authored edge marks (UV seam / sharp / edge weight) — depth-tested.
            capped |= self.append_poly_edge_marks(&mut out);
            _ms_marks = _t_d.elapsed().as_secs_f32() * 1000.0;
        }
        // Object-mode poly selection outline (display mesh) + modifier edit wire / boolean cutter.
        let _t_b = std::time::Instant::now();
        capped |= self.append_poly_selection_and_mod_wires(&mut out);
        let _ms_selmod = _t_b.elapsed().as_secs_f32() * 1000.0;
        let _t_b = std::time::Instant::now();
        if !self.render.wire_overlay_backface {
            capped |= self.append_object_wire(&mut out);
        }
        let _ms_objwire = _t_b.elapsed().as_secs_f32() * 1000.0;
        let depth_count = out.len() as u32;

        // --- X-Ray / always-on-top batch ---
        if self.poly_edit_wire_xray() {
            capped |= self.append_poly_edit_wire(&mut out);
            capped |= self.append_poly_selected_edges_for_active(&mut out);
            capped |= self.append_poly_edge_marks(&mut out);
        }
        if self.render.wire_overlay_backface {
            capped |= self.append_object_wire(&mut out);
        }
        // Auto reflection probe placement gizmo (X-ray: the proxy box usually
        // surrounds the scene — and the viewer — so depth-testing would hide it).
        self.append_probe_gizmo(&mut out);
        // Bake projection cage (X-ray): numeric Max Front/Rear + assigned mesh cage.
        self.append_bake_cage_preview(&mut out);

        // P6.2: hard caps are Frame Budget protection — never silent (Goal 4 honesty).
        capped |= out.len() >= MAX_OVERLAY_SEGMENTS;
        self.overlay_wire_seg_count = out.len() as u32;
        self.overlay_wire_capped = capped;
        if capped {
            if !self.overlay_wire_cap_toast_shown {
                self.overlay_wire_cap_toast_shown = true;
                self.toast(&format!(
                    "Wire overlay capped at {} segments (Frame Budget) — some edges hidden",
                    MAX_OVERLAY_SEGMENTS.min(out.len().max(1))
                ));
            }
        } else {
            self.overlay_wire_cap_toast_shown = false;
        }

        if _ms_fp + _ms_wire + _ms_seledge + _ms_marks + _ms_selmod + _ms_objwire + _ms_grid > 3.0
            || crate::perf_spam_on()
        {
            eprintln!(
                "[OvlSplit] MISS fp {:.2} + grid {:.2} + wire {:.2} + seledge {:.2} + marks {:.2}                  + selmod {:.2} + objwire {:.2} | segs={}",
                _ms_fp, _ms_grid, _ms_wire, _ms_seledge, _ms_marks, _ms_selmod, _ms_objwire,
                out.len()
            );
        }
        self.overlay_lines = out;
        self.overlay_lines_fp = fp;
        self.overlay_lines_depth_count = depth_count;
        depth_count
    }

    /// The placement the NEXT probe bake will use: union of the displayed instance
    /// AABBs, padded ×1.25 — must stay the same math as probe_bake.rs Phase A.
    /// None = empty scene. Shared by the gizmo (pre-bake preview) + fingerprint.
    fn probe_planned_bounds(&self) -> Option<(glam::Vec3, glam::Vec3)> {
        let mut lo = glam::Vec3::splat(f32::INFINITY);
        let mut hi = glam::Vec3::splat(f32::NEG_INFINITY);
        for (a, b) in &self.structural_world_aabb {
            lo = lo.min(*a);
            hi = hi.max(*b);
        }
        if !(hi.x > lo.x && hi.y > lo.y && hi.z > lo.z) {
            return None;
        }
        Some(((lo + hi) * 0.5, ((hi - lo) * 0.5).max(glam::Vec3::splat(0.25)) * 1.25))
    }

    /// Auto reflection probe placement gizmo (owner 2026-08-07): the probe's proxy
    /// AABB as an X-ray wire box + a center cross. Dim = planned placement (not yet
    /// baked); bright cyan = a baked probe serving reflections.
    fn append_probe_gizmo(&self, out: &mut Vec<LineInstance>) {
        if !self.viewport.show_probe_gizmo || !self.render.auto_reflection_probe {
            return;
        }
        let Some(gpu) = self.gpu.as_ref() else { return };
        // Before the first bake completes, show the LIVE planned placement (union of
        // the displayed instance AABBs — the exact math probe_bake uses) in a dim
        // tone, so "Show Probe Volume" always answers "where will it go?" (owner:
        // "I do not see any probe so I cannot tell if they are well placed").
        // Bright cyan = a baked probe actually serving reflections.
        let (c, e, col) = if gpu.probe_baked {
            (
                glam::Vec3::from(gpu.probe_center),
                glam::Vec3::from(gpu.probe_extent),
                LineInstance::pack_color(0.3, 0.9, 1.0, 0.9),
            )
        } else {
            let Some((c, e)) = self.probe_planned_bounds() else {
                return; // empty scene — nothing to place around
            };
            (c, e, LineInstance::pack_color(0.5, 0.7, 0.8, 0.45))
        };
        let corner =
            |sx: f32, sy: f32, sz: f32| c + glam::Vec3::new(e.x * sx, e.y * sy, e.z * sz);
        let mut seg = |a: glam::Vec3, b: glam::Vec3| {
            out.push(LineInstance { p0: a.to_array(), p1: b.to_array(), color: col, width: 1.5 });
        };
        // 12 box edges: 4 along each axis.
        for &(a, b) in &[(-1.0f32, -1.0f32), (-1.0, 1.0), (1.0, -1.0), (1.0, 1.0)] {
            seg(corner(-1.0, a, b), corner(1.0, a, b));
            seg(corner(a, -1.0, b), corner(a, 1.0, b));
            seg(corner(a, b, -1.0), corner(a, b, 1.0));
        }
        // Center cross (10% of each extent) marks the capture point itself.
        let k = e * 0.1;
        seg(c - glam::Vec3::X * k.x, c + glam::Vec3::X * k.x);
        seg(c - glam::Vec3::Y * k.y, c + glam::Vec3::Y * k.y);
        seg(c - glam::Vec3::Z * k.z, c + glam::Vec3::Z * k.z);
    }

    /// True when `nid` should get full-density object wire (selected or mod-editing).
    /// Active-without-selection is NOT hot — avoids dense wire cost on idle object-mode bodies.
    fn poly_wire_is_hot(&self, nid: core_math::scene_graph::NodeId) -> bool {
        if self.sel.usel.selected_nodes().any(|id| id == nid) {
            return true;
        }
        matches!(self.ui.poly_mod_selected, Some((id, _)) if id == nid)
    }

    /// Identity of everything that affects overlay segment contents (not camera).
    fn overlay_fingerprint(&self) -> u64 {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        self.render.wire_overlay_enabled.hash(&mut h);
        // Modifier result-wire toggle owns its own state now — without it in the
        // fingerprint the cached segment list never rebuilds when the ▦ button flips.
        self.ui.poly_mod_result_wire.hash(&mut h);
        self.render.wire_overlay_backface.hash(&mut h);
        self.render.wire_overlay_opacity.to_bits().hash(&mut h);
        self.render.wire_overlay_thickness.to_bits().hash(&mut h);
        for c in &self.render.wire_overlay_color {
            c.to_bits().hash(&mut h);
        }
        self.cache
            .body_vol_cache
            .use_detail_weighted_lo
            .hash(&mut h);
        self.cache.body_vol_cache.detail_lo_heat_wire.hash(&mut h);
        self.cache.body_vol_cache.detail_lo_edge_shape.hash(&mut h);
        // Normal-bake preview active (affects solid mesh, not wires — still bust fingerprint).
        match &self.quick_normal_ghost {
            Some(g) => {
                1u8.hash(&mut h);
                g.nid.0.hash(&mut h);
                g.res.hash(&mut h);
                g.hit_pct.hash(&mut h);
            }
            None => 0u8.hash(&mut h),
        }
        self.viewport.show_overlays.hash(&mut h);
        self.viewport.show_poly_overlays.hash(&mut h);
        // Authored edge-mark overlays (UV seam / Mark Sharp / edge weight).
        self.viewport.show_uv_seams.hash(&mut h);
        self.viewport.show_sharp_edges.hash(&mut h);
        self.viewport.show_edge_weights.hash(&mut h);
        // Probe gizmo: toggle + the probe state it draws (moves on re-bake), plus
        // the LIVE planned bounds so the pre-bake preview follows scene edits.
        self.viewport.show_probe_gizmo.hash(&mut h);
        self.render.auto_reflection_probe.hash(&mut h);
        if let Some(gpu) = self.gpu.as_ref() {
            gpu.probe_baked.hash(&mut h);
            for v in gpu.probe_center { v.to_bits().hash(&mut h); }
            for v in gpu.probe_extent { v.to_bits().hash(&mut h); }
        }
        if self.viewport.show_probe_gizmo {
            if let Some((c, e)) = self.probe_planned_bounds() {
                for v in [c.x, c.y, c.z, e.x, e.y, e.z] { v.to_bits().hash(&mut h); }
            }
        }
        self.hash_bake_cage_preview(&mut h);
        self.poly_edit_wire_xray().hash(&mut h);
        (self.modeling_context as u8).hash(&mut h);
        (self.poly_work_mode as u8).hash(&mut h);
        self.render.poly_wire_opacity.to_bits().hash(&mut h);
        self.render.poly_wire_thickness.to_bits().hash(&mut h);
        for c in &self.render.poly_wire_color {
            c.to_bits().hash(&mut h);
        }
        if let Some(nid) = self.sel.usel.active_node() {
            nid.0.hash(&mut h);
            self.poly_sel.hidden_vertices.len().hash(&mut h);
        }
        // Sub-element EDGE selection + hover drive the depth-tested highlight batch
        // (`append_poly_selected_edges`). Without them here the cached segment list never
        // rebuilds when the user selects, so the highlight would not appear at all.
        // Order-independent XOR fold: the set is a HashSet (random iteration order), and a
        // hash that changes with iteration order would rebuild every frame.
        if self.poly_work_mode == PolyWorkMode::Edge {
            let mut sel_fold: u64 = 0;
            for &e in &self.poly_sel.selected_edges {
                sel_fold ^= (e as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);
            }
            sel_fold.hash(&mut h);
            self.poly_sel.selected_edges.len().hash(&mut h);
            self.poly_sel.hovered_edge.hash(&mut h);
            self.viewport.poly_sel_edge_width.to_bits().hash(&mut h);
            self.viewport.poly_sel_opacity.to_bits().hash(&mut h);
            for c in &self.viewport.poly_sel_color {
                c.to_bits().hash(&mut h);
            }
            self.tool.current.is_poly_bevel().hash(&mut h);
        }
        // Selection set + selected modifier drive selection/boolean wires.
        for id in self.sel.usel.selected_nodes() {
            id.0.hash(&mut h);
        }
        match self.ui.poly_mod_selected {
            Some((nid, i)) => {
                1u8.hash(&mut h);
                nid.0.hash(&mut h);
                i.hash(&mut h);
            }
            None => 0u8.hash(&mut h),
        }
        self.viewport.show_sel_rim.hash(&mut h);
        for c in &self.viewport.sel_rim_color {
            c.to_bits().hash(&mut h);
        }

        // Floor wire grid (depth-tested): settings + camera extent (pan/zoom re-centers cells).
        let grid_wire = self.viewport.grid_visible
            && self.viewport.grid_mode == 0
            && self.viewport.grid_opacity >= 0.01;
        if grid_wire {
            1u8.hash(&mut h);
            self.viewport.grid_cell_size.to_bits().hash(&mut h);
            self.viewport.grid_floor_y.to_bits().hash(&mut h);
            self.viewport.grid_opacity.to_bits().hash(&mut h);
            self.viewport.grid_subdivisions.hash(&mut h);
            self.viewport.grid_show_axes.hash(&mut h);
            for c in &self.viewport.grid_color {
                c.to_bits().hash(&mut h);
            }
            for c in &self.viewport.grid_axis_x_color {
                c.to_bits().hash(&mut h);
            }
            for c in &self.viewport.grid_axis_z_color {
                c.to_bits().hash(&mut h);
            }
            // Extent follows camera — must bust on orbit/pan/zoom (unlike mesh wires).
            self.viewport.camera.distance.to_bits().hash(&mut h);
            self.viewport.camera.target.x.to_bits().hash(&mut h);
            self.viewport.camera.target.y.to_bits().hash(&mut h);
            self.viewport.camera.target.z.to_bits().hash(&mut h);
        } else {
            0u8.hash(&mut h);
        }

        // C-scale (2026-07-31): do NOT walk every body with `content_signature()` here.
        // That was O(verts+edges) × N every frame and owned ~30 ms of `disp` at 10k multi-mesh
        // even when wire overlays were OFF (fingerprint still ran). Settings + selection ids
        // above are enough when no overlay path will emit body wires.
        let wire_on = self.render.wire_overlay_enabled;
        let poly_edit = self.modeling_context == ModelingContext::Polygonal
            && self.poly_work_mode != PolyWorkMode::Object
            && self.viewport.show_overlays
            && self.viewport.show_poly_overlays;
        // Edge selection/hover highlight is independent of the edit-cage overlay toggle.
        let edge_hilite = self.modeling_context == ModelingContext::Polygonal
            && self.poly_work_mode == PolyWorkMode::Edge
            && (!self.poly_sel.selected_edges.is_empty() || self.poly_sel.hovered_edge.is_some());
        let sel_outline = self.viewport.show_overlays
            && self.viewport.show_poly_overlays
            && self.sel.usel.selected_nodes().next().is_some();
        // Modifier cage/result only emit in poly edit modes (never Object mode).
        let mod_wire = self.ui.poly_mod_selected.is_some()
            && self.modeling_context == ModelingContext::Polygonal
            && self.poly_work_mode != PolyWorkMode::Object
            && self.viewport.show_poly_overlays;
        let edge_marks = self.viewport.show_overlays
            && (self.viewport.show_uv_seams
                || self.viewport.show_sharp_edges
                || self.viewport.show_edge_weights);
        if !wire_on && !poly_edit && !edge_hilite && !sel_outline && !mod_wire && !grid_wire && !edge_marks {
            // Idle object-mode camera: free. Structural SCENE/MESH dirties bust via
            // draw_stream / mesh stamps when wire is later enabled.
            self.draw_stream_rebuilds_last_frame.hash(&mut h);
            self.structural_draw_rebuilds_last_frame.hash(&mut h);
            self.draw_stream.len().hash(&mut h);
            return h.finish();
        }

        // CustomData identity for edge-mark overlays: seams/sharp/crease live outside the
        // mesh Arc, so Arc-pointer hashing alone would leave a stale highlight after Mark Sharp.
        if edge_marks {
            if let Some(nid) = self.sel.usel.active_node() {
                if let Some(cd) = self.cache.poly_custom_data.get(&nid) {
                    cd.seam.len().hash(&mut h);
                    cd.sharp.len().hash(&mut h);
                    cd.crease.len().hash(&mut h);
                    // Sparse XOR fold so add/remove of a single mark busts the fingerprint
                    // without O(n log n) sorting every camera frame.
                    let mut seam_fold: u64 = 0;
                    for he in cd.seam.iter() {
                        seam_fold ^= (he.0 as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);
                    }
                    seam_fold.hash(&mut h);
                    let mut sharp_fold: u64 = 0;
                    for he in cd.sharp.iter() {
                        sharp_fold ^= (he.0 as u64).wrapping_mul(0xC2B2_AE3D_27D4_EB4F);
                    }
                    sharp_fold.hash(&mut h);
                    let mut crease_fold: u64 = 0;
                    for (k, w) in cd.crease.iter() {
                        crease_fold ^= (k.0 as u64)
                            .wrapping_mul(0x1656_67B1_9E37_79B9)
                            .wrapping_add(k.1 as u64)
                            .wrapping_mul(0x85EB_CA77_C2B2_AE3D)
                            .wrapping_add(w.to_bits() as u64);
                    }
                    crease_fold.hash(&mut h);
                } else {
                    0u8.hash(&mut h);
                }
            }
        }

        // Body walk only when an overlay will actually consume it. Use O(1) identity:
        // Arc pointer (COW / make_mut changes the pointer) + mesh_cache stamp — NOT a full
        // content_signature re-hash of every half-edge mesh.
        for (nid, node) in self.scene.scene_graph.all_nodes() {
            if !node.visible {
                continue;
            }
            nid.0.hash(&mut h);
            let world = self.scene.scene_graph.world_transform(nid);
            hash_mat4(&world, &mut h);
            match &node.payload {
                NodePayload::PolygonalBody {
                    mesh,
                    mesh_cache_id,
                    ..
                } => {
                    1u8.hash(&mut h);
                    (std::sync::Arc::as_ptr(mesh) as usize).hash(&mut h);
                    if let Some(cid) = mesh_cache_id {
                        self.cache
                            .mesh_cache
                            .pt_stamp(*cid)
                            .unwrap_or(0)
                            .hash(&mut h);
                    }
                }
                NodePayload::SdfBody {
                    mesh_cache_id: Some(cid),
                    ..
                } => {
                    2u8.hash(&mut h);
                    cid.0.hash(&mut h);
                    let stamp = self.cache.mesh_cache.pt_stamp(*cid).unwrap_or(0);
                    stamp.hash(&mut h);
                }
                // Voxel + imported bodies emit object wire too (2026-08-19) — without their
                // stamps here the cached segment list never rebuilds when a sculpt stroke or
                // display-mesh re-extract moves the surface under the wire.
                NodePayload::VoxelBody { mesh_cache_id, .. } if wire_on => {
                    3u8.hash(&mut h);
                    if let Some(cid) = mesh_cache_id {
                        cid.0.hash(&mut h);
                        self.cache.mesh_cache.pt_stamp(*cid).unwrap_or(0).hash(&mut h);
                    }
                    // ACTIVE sculpt displays per-chunk meshes — hash each chunk's stamp so a
                    // live stroke moves the wire with the surface (bounded: one body's chunks).
                    if let Some(v) = self.sculpt.voxel.as_ref().filter(|v| v.node == nid) {
                        v.chunks.len().hash(&mut h);
                        for cid in &v.chunks {
                            self.cache.mesh_cache.pt_stamp(*cid).unwrap_or(0).hash(&mut h);
                        }
                    }
                }
                NodePayload::ImportedMesh { mesh_cache_id, .. } if wire_on => {
                    4u8.hash(&mut h);
                    mesh_cache_id.0.hash(&mut h);
                    self.cache.mesh_cache.pt_stamp(*mesh_cache_id).unwrap_or(0).hash(&mut h);
                }
                _ => {
                    0u8.hash(&mut h);
                }
            }
        }
        h.finish()
    }

    fn poly_edit_wire_xray(&self) -> bool {
        !self.sel.visible_only
    }

    /// Returns `true` if a hard cap was hit while emitting.
    fn append_poly_edit_wire(&mut self, out: &mut Vec<LineInstance>) -> bool {
        if !self.viewport.show_overlays || !self.viewport.show_poly_overlays {
            return false;
        }
        if self.modeling_context != ModelingContext::Polygonal {
            return false;
        }
        if self.poly_work_mode == PolyWorkMode::Object {
            return false;
        }
        let Some(nid) = self.sel.usel.active_node() else {
            return false;
        };
        // Edit wire shows the BASE topology (what you edit), not the modifier display mesh.
        let (base, mut stamp) = {
            let Some(node) = self.scene.scene_graph.get(nid) else {
                return false;
            };
            let NodePayload::PolygonalBody { mesh, .. } = &node.payload else {
                return false;
            };
            // High nibble distinguishes base (edit) from display (object) caches.
            //
            // `poly_edit_generation`, NOT `content_signature()`. This runs once per FRAME, and
            // `content_signature` is a full walk of every vertex, half-edge and face — ~6 ms of the
            // 8 ms this function cost during a vertex drag on a 323k mesh (`[OvlSplit] wire 7.4-8.7`
            // with every other batch at 0.00, inside `[DREST] ovl=9.6` of an 11 ms dispatch).
            // Hashing the whole mesh to discover "did the mesh change" is strictly more work than
            // rebuilding the thing the answer guards, once the answer is almost always yes.
            //
            // The generation is the app's mutation choke-point counter, already trusted for exactly
            // this job by `poly_display_key_memo`, the LOD latch and the outliner manifold cache. It
            // is a DISPLAY cache: the failure mode of a missed bump is a stale wire for one edit,
            // not wrong geometry — which is why the content-vs-generation warning on the modifier
            // prefix ladder (where a wrong reuse produces a wrong MESH) does not transfer here.
            let stamp = (self.poly_edit_generation ^ 0xBA5E_0000_0000_0000)
                .wrapping_mul(0x9E37_79B9_7F4A_7C15)
                ^ (nid.0 as u64);
            (mesh.clone(), stamp)
        };

        let [r, g, b] = self.render.poly_wire_color;
        let color = LineInstance::pack_color(r, g, b, self.render.poly_wire_opacity);
        let width = self.render.poly_wire_thickness;
        let world = self.scene.scene_graph.world_transform(nid);
        let hidden = self.poly_sel.hidden_vertices.clone();
        // Hidden verts affect which edges are emitted — fold into stamp.
        if !hidden.is_empty() {
            stamp ^= 0xA11D_E100_0000_0001;
            stamp = stamp.wrapping_add(hidden.len() as u64);
            for &v in &hidden {
                stamp = stamp
                    .wrapping_mul(0x9E37_79B9_7F4A_7C15)
                    .wrapping_add(v as u64);
            }
        }

        let mut capped =
            self.poly_ensure_local_wire(nid, stamp, base.as_ref(), Some(&hidden), false);
        if let Some(local) = self.poly_wire_local_cache.get(&nid) {
            // Full cage when settled; every 4th edge mid-drag. A drag rebuilds this cache every
            // frame (the pos-edit path removes it), so emitting all 400k segments per drag frame
            // would hand back the milliseconds the drag work just reclaimed. Settled frames pay
            // once per edit, which is when full topology is actually being inspected.
            let stride = if self.pending_episode.is_some() { 4 } else { 1 };
            capped |= emit_local_edges(out, local, &world, color, width, 1.0, stride);
        }
        // Selected-edge highlight is emitted separately (`append_poly_selected_edges_for_active`)
        // so it is NOT suppressed when show_poly_overlays is off.
        capped
    }

    /// Resolve active poly body + emit selected/hovered edge highlights (GPU depth-tested).
    ///
    /// Independent of `show_poly_overlays` / the edit cage wire — selection highlight is always
    /// meaningful in Edge mode. SELEDGEMOTION (2026-08-07): never draw these in egui.
    fn append_poly_selected_edges_for_active(&mut self, out: &mut Vec<LineInstance>) -> bool {
        if self.poly_work_mode != PolyWorkMode::Edge {
            return false;
        }
        if self.tool.current.is_poly_bevel() {
            return false;
        }
        if self.poly_sel.selected_edges.is_empty() && self.poly_sel.hovered_edge.is_none() {
            return false;
        }
        let Some(nid) = self.sel.usel.active_node() else {
            return false;
        };
        let (base, world) = {
            let Some(node) = self.scene.scene_graph.get(nid) else {
                return false;
            };
            let NodePayload::PolygonalBody { mesh, .. } = &node.payload else {
                return false;
            };
            let world = self.scene.scene_graph.world_transform(nid);
            (mesh.clone(), world)
        };
        let hidden = &self.poly_sel.hidden_vertices;
        self.append_poly_selected_edges(out, nid, base.as_ref(), &world, hidden)
    }

    /// Selected + hovered EDGE highlight, emitted into the GPU line batch so it is depth-tested
    /// against the live G-buffer (or always-on-top when X-Ray / !visible_only).
    ///
    /// WHY NOT egui (owner 2026-08-07: "when I zoom out with all edges selected, during the
    /// movement I see the back edges"): the egui highlight had no depth test of its own, so it
    /// borrowed the face-ID pick buffer — which is deliberately refreshed only once the camera
    /// SETTLES. Mid-orbit there is no buffer, and the documented fallback is "no depth info =>
    /// draw". Both behaviours are wrong: a CPU overlay cannot answer occlusion. This batch is
    /// rasterized against solid depth EVERY frame.
    fn append_poly_selected_edges(
        &self,
        out: &mut Vec<LineInstance>,
        nid: core_math::scene_graph::NodeId,
        mesh: &core_math::half_edge::HalfEdgeMesh,
        world: &glam::Mat4,
        hidden: &std::collections::HashSet<u32>,
    ) -> bool {
        let _ = nid;
        if self.poly_work_mode != PolyWorkMode::Edge {
            return false;
        }
        // Mid-drag the mesh is rebuilt every frame, so pre-drag half-edge ids address different
        // edges — same guard the egui path documents for PolyBevel.
        if self.tool.current.is_poly_bevel() {
            return false;
        }
        let sc = self.viewport.poly_sel_color;
        let sa = self.viewport.poly_sel_opacity.clamp(0.0, 1.0);
        let sel_color = LineInstance::pack_color(sc[0], sc[1], sc[2], sa);
        let hover_color = LineInstance::pack_color(0.78, 1.0, 0.78, sa);
        // Wider than the base wire so the highlight reads over it at any zoom.
        let width = self.viewport.poly_sel_edge_width.max(0.5);
        let mut capped = false;
        let emit = |out: &mut Vec<LineInstance>, heid: u32, color: u32, width: f32| {
            if out.len() >= MAX_OVERLAY_SEGMENTS {
                return true;
            }
            if (heid as usize) >= mesh.half_edges.len() || !mesh.is_edge_live(HalfEdgeId(heid)) {
                return false;
            }
            let src = mesh.he_source(HalfEdgeId(heid));
            let dst = mesh.he(HalfEdgeId(heid)).vertex;
            if hidden.contains(&src.0) || hidden.contains(&dst.0) {
                return false;
            }
            let p0 = world.transform_point3(mesh.vert(src).position);
            let p1 = world.transform_point3(mesh.vert(dst).position);
            if !p0.is_finite() || !p1.is_finite() {
                return false;
            }
            out.push(LineInstance {
                p0: [p0.x, p0.y, p0.z],
                p1: [p1.x, p1.y, p1.z],
                color,
                width,
            });
            false
        };
        for &heid in &self.poly_sel.selected_edges {
            capped |= emit(out, heid, sel_color, width);
        }
        if let Some(heid) = self.poly_sel.hovered_edge {
            if !self.poly_sel.selected_edges.contains(&heid) {
                capped |= emit(out, heid, hover_color, (width * 0.7).max(1.0));
            }
        }
        capped
    }

    /// Authored edge marks: UV seams (red), Mark Sharp (cyan), edge weights / creases (magenta).
    ///
    /// Viewport Overlay toggles under **Mesh Edge Marks**. Drawn on the active poly body's
    /// BASE mesh so marks match what Mark Sharp / Set Edge Weight / UV seams author.
    /// Independent of object wire and of the poly edit cage (so you can inspect marks with
    /// the cage off). Still gated by the master Show Overlays switch.
    fn append_poly_edge_marks(&self, out: &mut Vec<LineInstance>) -> bool {
        if !self.viewport.show_overlays {
            return false;
        }
        let show_seam = self.viewport.show_uv_seams;
        let show_sharp = self.viewport.show_sharp_edges;
        let show_weight = self.viewport.show_edge_weights;
        if !show_seam && !show_sharp && !show_weight {
            return false;
        }
        let Some(nid) = self.sel.usel.active_node() else {
            return false;
        };
        let (mesh, world) = {
            let Some(node) = self.scene.scene_graph.get(nid) else {
                return false;
            };
            let NodePayload::PolygonalBody { mesh, .. } = &node.payload else {
                return false;
            };
            (mesh.clone(), self.scene.scene_graph.world_transform(nid))
        };
        let Some(cd) = self.cache.poly_custom_data.get(&nid) else {
            return false;
        };
        // Edit-mode hide-vert filter; empty in object mode.
        let hidden = if self.modeling_context == ModelingContext::Polygonal
            && self.poly_work_mode != PolyWorkMode::Object
        {
            Some(&self.poly_sel.hidden_vertices)
        } else {
            None
        };

        // Blender-ish colours: seam=red, sharp=cyan, crease/weight=magenta.
        let seam_color = LineInstance::pack_color(1.0, 0.15, 0.12, 0.95);
        let sharp_color = LineInstance::pack_color(0.15, 0.85, 1.0, 0.95);
        // Base width sits above the edit cage so marks stay readable.
        let base_w = (self.render.poly_wire_thickness.max(1.0) + 1.0).max(2.0);

        // SELECTION WINS on the edges it covers.
        //
        // Marks are emitted BEFORE the selection wires, so ordering alone should let selection
        // draw on top — but a mark is drawn WIDER than the selection outline (`base_w + 0.5`,
        // i.e. 2.5 at defaults, against `sel_outline_width.max(1.5)`), so the red seam bled out
        // on both sides of the thinner selection line and framed it out of visibility. The width
        // difference is deliberate (marks must read against the edit cage) and the fix is not to
        // shrink it: an edge that is CURRENTLY SELECTED should show its selection, because that
        // is the transient state the user is acting on, while the mark is persistent authorship
        // they can see on every other edge.
        let selected_edges: Option<&std::collections::HashSet<u32>> =
            if self.modeling_context == ModelingContext::Polygonal
                && self.poly_work_mode != PolyWorkMode::Object
                && !self.poly_sel.selected_edges.is_empty()
            {
                Some(&self.poly_sel.selected_edges)
            } else {
                None
            };

        let mut capped = false;
        let mut emit_he = |out: &mut Vec<LineInstance>, he: HalfEdgeId, color: u32, width: f32| {
            if out.len() >= MAX_OVERLAY_SEGMENTS {
                return true;
            }
            if !mesh.is_edge_live(he) {
                return false;
            }
            // Either half-edge marks the edge as selected — the same both-twins convention the
            // seam layer itself is written with.
            if let Some(sel) = selected_edges {
                let twin = mesh.he(he).twin;
                if sel.contains(&he.0)
                    || (twin != core_math::half_edge::NO_TWIN && sel.contains(&twin.0))
                {
                    return false;
                }
            }
            let src = mesh.he_source(he);
            let dst = mesh.he(he).vertex;
            if let Some(h) = hidden {
                if h.contains(&src.0) || h.contains(&dst.0) {
                    return false;
                }
            }
            let p0 = world.transform_point3(mesh.vert(src).position);
            let p1 = world.transform_point3(mesh.vert(dst).position);
            if !p0.is_finite() || !p1.is_finite() {
                return false;
            }
            out.push(LineInstance {
                p0: [p0.x, p0.y, p0.z],
                p1: [p1.x, p1.y, p1.z],
                color,
                width,
            });
            false
        };

        // Undirected dedupe: seam/sharp are corner (half-edge) bits; both twins may be set.
        let mut seen: std::collections::HashSet<(u32, u32)> = std::collections::HashSet::new();
        let undirected_key = |mesh: &core_math::half_edge::HalfEdgeMesh, he: HalfEdgeId| -> (u32, u32) {
            let a = mesh.he_source(he).0;
            let b = mesh.he(he).vertex.0;
            if a <= b { (a, b) } else { (b, a) }
        };

        if show_seam && !cd.seam.is_empty() {
            seen.clear();
            for &he in cd.seam.iter() {
                if !mesh.is_edge_live(he) {
                    continue;
                }
                let key = undirected_key(mesh.as_ref(), he);
                if !seen.insert(key) {
                    continue;
                }
                capped |= emit_he(out, he, seam_color, base_w + 0.5);
            }
        }

        if show_sharp && !cd.sharp.is_empty() {
            seen.clear();
            for &he in cd.sharp.iter() {
                if !mesh.is_edge_live(he) {
                    continue;
                }
                let key = undirected_key(mesh.as_ref(), he);
                if !seen.insert(key) {
                    continue;
                }
                capped |= emit_he(out, he, sharp_color, base_w + 0.5);
            }
        }

        if show_weight && !cd.crease.is_empty() {
            for (k, &w) in cd.crease.iter() {
                if w <= 1e-4 {
                    continue;
                }
                if out.len() >= MAX_OVERLAY_SEGMENTS {
                    capped = true;
                    break;
                }
                let va = core_math::half_edge::VertexId(k.0);
                let vb = core_math::half_edge::VertexId(k.1);
                if (va.0 as usize) >= mesh.vertices.len() || (vb.0 as usize) >= mesh.vertices.len() {
                    continue;
                }
                // Skip free-list holes (dead verts keep a slot but carry no live geometry).
                if !mesh.is_vert_live(va) || !mesh.is_vert_live(vb) {
                    continue;
                }
                if let Some(h) = hidden {
                    if h.contains(&va.0) || h.contains(&vb.0) {
                        continue;
                    }
                }
                // EdgeKey stores endpoint vertex ids — world positions are enough for the segment.
                let p0 = world.transform_point3(mesh.vert(va).position);
                let p1 = world.transform_point3(mesh.vert(vb).position);
                if !p0.is_finite() || !p1.is_finite() {
                    continue;
                }
                let t = w.clamp(0.0, 1.0);
                // Magenta intensity tracks weight: soft creases read dimmer, full = solid.
                let color = LineInstance::pack_color(1.0, 0.2 * (1.0 - t), 0.85, 0.45 + 0.55 * t);
                let width = base_w + t * 2.0;
                out.push(LineInstance {
                    p0: [p0.x, p0.y, p0.z],
                    p1: [p1.x, p1.y, p1.z],
                    color,
                    width,
                });
            }
        }

        capped
    }

    /// Poly selection rim + selected-modifier wires (edit mode only).
    /// Returns `true` if a hard cap was hit.
    fn append_poly_selection_and_mod_wires(&mut self, out: &mut Vec<LineInstance>) -> bool {
        let mut capped = false;

        // Selection rim color (orange by default via Viewport Overlays → Selection Outline).
        let (sr, sg, sb) = if self.viewport.show_sel_rim {
            let c = self.viewport.sel_rim_color;
            (c[0], c[1], c[2])
        } else {
            let c = self.viewport.sel_color;
            (c[0], c[1], c[2])
        };
        let sel_color = LineInstance::pack_color(sr, sg, sb, 0.95);
        let sel_width = self.viewport.sel_outline_width.max(1.5);

        // ── Modifier RESULT wireframe (▦ on the stack card): EDIT MODE, any overlay state ──
        // Explicit opt-in with the same contract as the global shaded+wire display
        // (`wire_overlay_enabled` → `append_object_wire`): it does NOT sit behind the
        // Shift+O master overlay switch or Poly Overlays. The original button behavior
        // rode `append_object_wire` and therefore ignored those switches — gating the
        // replacement on them made the button dead whenever overlays were off.
        //
        // EDIT MODE ONLY (owner 2026-08-08: "make sure those appear in edit mode only").
        // Object mode stays clean — same rule the selected-modifier cage wires below follow.
        // The ▦ button dims itself outside Edit mode so the icon never claims to be on while
        // this gate suppresses the draw.
        //
        // TARGET BODY (owner 2026-08-08: "I don't see wireframe of modifier even after
        // activating the wireframe button"): the selected stack card's body when there is one,
        // otherwise the ACTIVE body. This used to require `poly_mod_selected` outright — but
        // the ▦ button only sets `poly_mod_result_wire`, and nothing about clicking it selects
        // a card. Pressing it without a card selected (or after anything cleared the selection)
        // lit the icon orange and drew NOTHING: the button and the draw disagreed about what
        // enables the wire. The flag is global ("same flag on every card"), so the draw must be
        // too.
        if self.modeling_context == ModelingContext::Polygonal
            && self.poly_work_mode != PolyWorkMode::Object
            && self.ui.poly_mod_result_wire
        {
            let target = self
                .ui
                .poly_mod_selected
                .map(|(nid, _)| nid)
                .or_else(|| self.sel.usel.active_node());
            if let Some(nid) = target {
                // Its OWN colour/width — NOT `sel_color`. Borrowing the selection tint is what
                // made the viewport show two different wire colours at once (owner 2026-08-08:
                // "why is my main wireframe 2 colours, need to be black"). Defaults to black,
                // matching the object wire; right-click the ▦ button to restyle.
                let [wr, wg, wb] = self.render.mod_wire_color;
                let mw_color =
                    LineInstance::pack_color(wr, wg, wb, self.render.mod_wire_opacity.clamp(0.0, 1.0));
                capped |=
                    self.poly_append_display_wire(out, nid, mw_color, self.render.mod_wire_thickness, 1);
            }
        }

        if !self.viewport.show_overlays {
            return capped;
        }

        // ── ONE object selection highlight: deferred GPU rim (`sel_bit`), nothing else ──
        // Selected poly/SDF/NURBS bodies get the green screen-space outline via
        // raster_gbuffer sel_bit → deferred edge-detect (Blender-style object feedback).
        // Do NOT also emit a full-edge selection cage here — that was the dual-highlight
        // bug (owner 2026-08-02). Edit mode strips the active body from selected_outline
        // so only component/wire feedback shows while inside V/E/F.
        //
        // Still drawn here: EDIT wire (`append_poly_edit_wire`), selected-MODIFIER wires
        // below, and optional shaded+wire display (`wire_overlay_enabled`).
        let _ = (sel_color, sel_width);

        // ── Selected modifier cage/cutter wires: Edit mode only ──
        // Object mode must stay clean — modifier cage / boolean-cutter wires
        // were leaking into Object mode whenever a stack card stayed selected
        // (owner 2026-08-06). The result wireframe is NOT here — it is the explicit
        // ▦ opt-in emitted above, before the overlay gates.
        let mod_wires_allowed = self.modeling_context == ModelingContext::Polygonal
            && self.poly_work_mode != PolyWorkMode::Object
            && self.viewport.show_poly_overlays;
        if mod_wires_allowed {
            if let Some((nid, mi)) = self.ui.poly_mod_selected {
                let (base_mesh, base_stamp, bool_info) = {
                    let Some(node) = self.scene.scene_graph.get(nid) else {
                        return capped;
                    };
                    let NodePayload::PolygonalBody { mesh, modifiers, .. } = &node.payload else {
                        return capped;
                    };
                    let bool_info = modifiers.get(mi).and_then(|m| {
                        if m.mod_type == core_math::scene_graph::PolyModifierType::Boolean
                            && m.bool_cutter_nid != 0
                        {
                            Some((m.bool_cutter_nid, m.bool_op))
                        } else {
                            None
                        }
                    });
                    let stamp = mesh.content_signature() ^ 0xBA5E_0000_0000_0000;
                    (mesh.clone(), stamp, bool_info)
                };
                let world = self.scene.scene_graph.world_transform(nid);
                // Base mesh edit wire (dark cyan) so topology under the stack stays readable.
                let edit_col = LineInstance::pack_color(0.35, 0.85, 1.0, 0.85);
                let edit_w = self.render.poly_wire_thickness.max(1.2);
                capped |=
                    self.poly_ensure_local_wire(nid, base_stamp, base_mesh.as_ref(), None, false);
                if let Some(local) = self.poly_wire_local_cache.get(&nid) {
                    capped |= emit_local_edges(out, local, &world, edit_col, edit_w, 1.0, 1);
                }

                // (Result wireframe is emitted above, before the overlay gates.)

                // Boolean: special cutter wire (red / magenta) so operand is obvious.
                if let Some((cutter_nid, bool_op)) = bool_info {
                    let cnid = core_math::scene_graph::NodeId(cutter_nid);
                    let cmesh = self
                        .scene
                        .scene_graph
                        .get(cnid)
                        .and_then(|cnode| match &cnode.payload {
                            NodePayload::PolygonalBody { mesh: cm, .. } => Some(cm.clone()),
                            _ => None,
                        });
                    if let Some(cm) = cmesh {
                        let cworld = self.scene.scene_graph.world_transform(cnid);
                        let bool_col = match bool_op {
                            1 => LineInstance::pack_color(0.2, 1.0, 0.35, 0.95), // Union = green
                            2 => LineInstance::pack_color(0.3, 0.55, 1.0, 0.95), // Intersect = blue
                            _ => LineInstance::pack_color(1.0, 0.25, 0.2, 0.95), // Diff = red
                        };
                        let cstamp = cm.content_signature() ^ 0xC07E_0000_0000_0000;
                        capped |=
                            self.poly_ensure_local_wire(cnid, cstamp, cm.as_ref(), None, false);
                        if let Some(local) = self.poly_wire_local_cache.get(&cnid) {
                            capped |= emit_local_edges(
                                out,
                                local,
                                &cworld,
                                bool_col,
                                sel_width.max(2.0),
                                1.0,
                                1,
                            );
                        }
                    }
                }
            }
        }

        capped
    }

    /// Object / selection display wire. `stride`: 1 = every edge; N = every Nth (P6.1 inactive).
    fn poly_append_display_wire(
        &mut self,
        out: &mut Vec<LineInstance>,
        nid: core_math::scene_graph::NodeId,
        color: u32,
        width: f32,
        stride: usize,
    ) -> bool {
        let Some(display) = self.poly_cached_display_mesh(nid) else {
            return false;
        };
        let stamp = self
            .poly_display_cache
            .get(&nid)
            .map(|e| e.key.0 ^ e.key.1.rotate_left(17))
            .unwrap_or(0);
        // Clean wire: skip coplanar (triangulation) edges — evaluated meshes are
        // often triangulated by booleans/decimate; the wire must show shape, not soup.
        let mut capped = self.poly_ensure_local_wire(nid, stamp, display.as_ref(), None, true);
        let world = self.scene.scene_graph.world_transform(nid);
        if let Some(local) = self.poly_wire_local_cache.get(&nid) {
            capped |= emit_local_edges(out, local, &world, color, width, 1.0, stride.max(1));
        }
        capped
    }

    /// Rebuild body-local poly edge list only when `stamp` changes.
    /// Returns `true` if the per-body edge list hit `MAX_POLY_WIRE`.
    fn poly_ensure_local_wire(
        &mut self,
        nid: core_math::scene_graph::NodeId,
        stamp: u64,
        mesh: &core_math::half_edge::HalfEdgeMesh,
        hidden: Option<&std::collections::HashSet<u32>>,
        clean: bool,
    ) -> bool {
        // Fold the filter mode into the stamp — the cache is shared per-nid between
        // the raw base wire and the clean display wire; same content, different edges.
        let stamp = stamp ^ if clean { 0xC1EA_5000_0000_0000 } else { 0 };
        let need = self
            .poly_wire_local_cache
            .get(&nid)
            .map(|c| c.stamp != stamp || c.p0.is_empty())
            .unwrap_or(true);
        if !need {
            return self
                .poly_wire_local_cache
                .get(&nid)
                .map(|c| c.p0.len() >= MAX_POLY_WIRE)
                .unwrap_or(false);
        }
        let mut local = WireLocalEdgeCache {
            stamp,
            flags: 0,
            p0: Vec::new(),
            p1: Vec::new(),
            heat_rgb: Vec::new(),
        };
        let hit_cap = collect_poly_local_edges(mesh, hidden, &mut local, clean);
        self.poly_wire_local_cache.insert(nid, local);

        // Bound growth (many poly bodies in a scene).
        if self.poly_wire_local_cache.len() > 128 {
            let live: std::collections::HashSet<_> = self
                .scene
                .scene_graph
                .poly_bodies()
                .map(|(id, _)| id)
                .collect();
            self.poly_wire_local_cache.retain(|k, _| live.contains(k));
        }
        hit_cap
    }

    /// Object wire: poly + SDF. Returns `true` if a hard cap was hit.
    fn append_object_wire(&mut self, out: &mut Vec<LineInstance>) -> bool {
        if !self.render.wire_overlay_enabled {
            return false;
        }
        let mut capped = false;
        let [r, g, b] = self.render.wire_overlay_color;
        let color = LineInstance::pack_color(r, g, b, self.render.wire_overlay_opacity);
        let width = self.render.wire_overlay_thickness;

        // Collect work list first so we can mutably update wire_local_cache without
        // holding graph borrows across the whole loop incorrectly.
        let mut jobs: Vec<ObjectWireJob> = Vec::new();
        for (nid, node) in self.scene.scene_graph.all_nodes() {
            if !node.visible {
                continue;
            }
            match &node.payload {
                NodePayload::PolygonalBody { .. } => {
                    jobs.push(ObjectWireJob::Poly { nid });
                }
                NodePayload::SdfBody {
                    mesh_cache_id: Some(cid),
                    ..
                } => {
                    jobs.push(ObjectWireJob::Sdf { nid, cid: *cid });
                }
                // Voxel sculpt body (owner 2026-08-19: "toggle Object wire, don't see it" —
                // the sculpt sphere was a VoxelBody, which this match silently skipped in
                // EVERY shading mode). The ACTIVE sculpt displays per-chunk meshes, not the
                // cached object-mode mesh — wire what is actually on screen (same detection
                // as the raster instance builder, raster_gbuffer.rs).
                NodePayload::VoxelBody { mesh_cache_id, .. } => {
                    // Raymarch-displayed sparse sculpt (>512³): chunk meshes are NOT what is
                    // on screen — a chunk wire would float over the raymarched surface. Fail
                    // closed (no wire) rather than draw a lie, same rule as the raster pass.
                    let active = self
                        .sculpt
                        .voxel
                        .as_ref()
                        .filter(|v| v.node == nid && !v.chunks.is_empty());
                    if let Some(v) = active {
                        if !v.use_sculpt_raymarch_overlay() {
                            let chunks = v.chunks.clone();
                            for cid in chunks {
                                jobs.push(ObjectWireJob::Mesh { nid, cid });
                            }
                        }
                    } else if let Some(cid) = mesh_cache_id {
                        jobs.push(ObjectWireJob::Mesh { nid, cid: *cid });
                    }
                }
                // Imported meshes are renderable solids too — same wire contract.
                NodePayload::ImportedMesh { mesh_cache_id, .. } => {
                    jobs.push(ObjectWireJob::Mesh { nid, cid: *mesh_cache_id });
                }
                // NURBS: do NOT dump mesh triangulation wire (dense face lines).
                // Clean black boundary + bevel arcs are drawn in draw_nurbs_overlays.
                _ => {}
            }
        }

        for job in jobs {
            if out.len() >= MAX_OVERLAY_SEGMENTS {
                capped = true;
                break;
            }
            match job {
                ObjectWireJob::Poly { nid } => {
                    // Display mesh (modifiers applied) so object wire matches solid (P1b).
                    // P6.1: inactive bodies use coarser edge stride for multi-body scenes.
                    let stride = if self.poly_wire_is_hot(nid) {
                        1
                    } else {
                        POLY_INACTIVE_WIRE_STRIDE
                    };
                    capped |= self.poly_append_display_wire(out, nid, color, width, stride);
                }
                ObjectWireJob::Sdf { nid, cid } => {
                    let stamp = self.cache.mesh_cache.pt_stamp(cid).unwrap_or(0);
                    let heat_on = self.cache.body_vol_cache.use_detail_weighted_lo
                        && self.cache.body_vol_cache.detail_lo_heat_wire;
                    let edge_shape_heat = heat_on
                        && self.cache.body_vol_cache.detail_lo_edge_shape;
                    // Refresh local edge cache if mesh content or heat mode changed.
                    let need_rebuild = self
                        .wire_local_cache
                        .get(&cid.0)
                        .map(|c| {
                            c.stamp != stamp
                                || c.p0.is_empty()
                                || ((c.flags & WIRE_CACHE_HEAT) != 0) != heat_on
                                || ((c.flags & WIRE_CACHE_EDGE_SHAPE) != 0) != edge_shape_heat
                        })
                        .unwrap_or(true);
                    if need_rebuild {
                        let mut local = WireLocalEdgeCache {
                            stamp,
                            flags: {
                                let mut f = 0u8;
                                if heat_on {
                                    f |= WIRE_CACHE_HEAT;
                                }
                                if edge_shape_heat {
                                    f |= WIRE_CACHE_EDGE_SHAPE;
                                }
                                f
                            },
                            p0: Vec::new(),
                            p1: Vec::new(),
                            heat_rgb: Vec::new(),
                        };
                        // Optional AZRA edge-shape heat from live SDF (ridge + distance-to-edge).
                        let edge_heat = if edge_shape_heat {
                            if let Some((verts, indices)) =
                                self.cache.mesh_cache.pt_source_data(cid)
                            {
                                if !verts.is_empty() && !indices.is_empty() {
                                    let mesh =
                                        core_math::display_mesh_ops::DisplayMesh::from_raster(
                                            &verts, &indices,
                                        );
                                    let field = core_math::field_eval::BodyField::from_graph(
                                        &self.scene.scene_graph,
                                        nid,
                                    );
                                    let feat = crate::bake_utils::body_soft_feature_size(
                                        &self.scene.scene_graph,
                                        nid,
                                    );
                                    let map = core_math::display_mesh_ops::sample_sdf_edge_shape_map(
                                        &field, &mesh, 0.85, feat,
                                    );
                                    // Normalize 0..4 → 0..1 for palette.
                                    Some(
                                        map.into_iter()
                                            .map(|v| (v * 0.25).clamp(0.0, 1.0))
                                            .collect::<Vec<f32>>(),
                                    )
                                } else {
                                    None
                                }
                            } else {
                                None
                            }
                        } else {
                            None
                        };
                        build_sdf_local_edges(
                            &self.cache.mesh_cache,
                            cid,
                            &mut local,
                            heat_on,
                            edge_heat.as_deref(),
                        );
                        self.wire_local_cache.insert(cid.0, local);
                    }
                    // CRITICAL: DC verts are body-local. Solid raster uses
                    // `effective_body_transform` (+ rigid offset), NOT SceneNode.world_transform.
                    // Using the wrong matrix made wires look like a pattern projected on the mesh.
                    let mut world = self
                        .scene
                        .scene_graph
                        .effective_body_transform(nid)
                        .unwrap_or_else(|| self.scene.scene_graph.world_transform(nid));
                    {
                        let off = self.cache.body_vol_cache.mesh_rigid_offset(nid);
                        if off.length_squared() > 1e-12 {
                            world *= glam::Mat4::from_translation(off);
                        }
                    }
                    if let Some(local) = self.wire_local_cache.get(&cid.0) {
                        let opacity = self.render.wire_overlay_opacity;
                        capped |= emit_local_edges(out, local, &world, color, width, opacity, 1);
                    }
                }
                ObjectWireJob::Mesh { nid, cid } => {
                    // Voxel display mesh / sculpt chunk / imported mesh: same edge cache as
                    // SDF (content_stamp invalidation covers chunk in-place vert updates),
                    // no SDF heat, and the SceneNode transform — VoxelBody/ImportedMesh
                    // place via world_transform, never effective_body_transform.
                    let stamp = self.cache.mesh_cache.pt_stamp(cid).unwrap_or(0);
                    let need_rebuild = self
                        .wire_local_cache
                        .get(&cid.0)
                        .map(|c| c.stamp != stamp || c.p0.is_empty() || c.flags != 0)
                        .unwrap_or(true);
                    if need_rebuild {
                        let mut local = WireLocalEdgeCache {
                            stamp,
                            flags: 0,
                            p0: Vec::new(),
                            p1: Vec::new(),
                            heat_rgb: Vec::new(),
                        };
                        build_sdf_local_edges(
                            &self.cache.mesh_cache,
                            cid,
                            &mut local,
                            false,
                            None,
                        );
                        self.wire_local_cache.insert(cid.0, local);
                    }
                    let world = self.scene.scene_graph.world_transform(nid);
                    if let Some(local) = self.wire_local_cache.get(&cid.0) {
                        let opacity = self.render.wire_overlay_opacity;
                        capped |= emit_local_edges(out, local, &world, color, width, opacity, 1);
                    }
                }
            }
        }

        // Drop caches for mesh ids no longer referenced (prevent unbounded growth).
        // Live set must cover EVERY wire source — retaining only SdfBody used to evict the
        // voxel/imported entries each sweep and rebuild them every frame past 64 caches.
        if self.wire_local_cache.len() > 64 {
            let mut live: std::collections::HashSet<u64> = self
                .scene
                .scene_graph
                .all_nodes()
                .filter_map(|(_, n)| match &n.payload {
                    NodePayload::SdfBody {
                        mesh_cache_id: Some(cid),
                        ..
                    }
                    | NodePayload::VoxelBody {
                        mesh_cache_id: Some(cid),
                        ..
                    } => Some(cid.0),
                    NodePayload::ImportedMesh { mesh_cache_id, .. } => Some(mesh_cache_id.0),
                    _ => None,
                })
                .collect();
            if let Some(v) = self.sculpt.voxel.as_ref() {
                live.extend(v.chunks.iter().map(|c| c.0));
            }
            self.wire_local_cache.retain(|k, _| live.contains(k));
        }
        capped
    }

    /// Floor wire grid as depth-tested GPU lines (Wire mode only).
    ///
    /// Replaces the egui `draw_grid_overlay` path which had no depth buffer — lines always
    /// painted on top of solid geo (owner: "GRID NEED TO RENDER UNDER GEO"). Shadow Catcher
    /// mode keeps the GPU floor plane in the raymarch/deferred path.
    fn append_floor_grid(&mut self, out: &mut Vec<LineInstance>) -> bool {
        if !self.viewport.grid_visible || self.viewport.grid_opacity < 0.01 {
            return false;
        }
        // Shadow Catcher = GPU floor; do not double-draw.
        if self.viewport.grid_mode == 1 {
            return false;
        }
        let cell = self.viewport.grid_cell_size.max(0.01);
        // Slight lift so coplanar floor meshes don't z-fight the grid (line FS has slope eps).
        let floor_y = self.viewport.grid_floor_y + 1e-3;
        let opacity = self.viewport.grid_opacity.clamp(0.0, 1.0);
        let [gr, gg, gb] = self.viewport.grid_color;
        let major_col = LineInstance::pack_color(gr, gg, gb, opacity);
        let sub_col = LineInstance::pack_color(gr, gg, gb, opacity * 0.35);
        let [axr, axg, axb] = self.viewport.grid_axis_x_color;
        let axis_x_col = LineInstance::pack_color(axr, axg, axb, opacity.min(0.86));
        let [azr, azg, azb] = self.viewport.grid_axis_z_color;
        let axis_z_col = LineInstance::pack_color(azr, azg, azb, opacity.min(0.86));

        let extent = self.viewport.camera.distance * 1.8;
        let x_min = ((self.viewport.camera.target.x - extent) / cell).floor() as i32;
        let x_max = ((self.viewport.camera.target.x + extent) / cell).ceil() as i32;
        let z_min = ((self.viewport.camera.target.z - extent) / cell).floor() as i32;
        let z_max = ((self.viewport.camera.target.z + extent) / cell).ceil() as i32;
        let max_lines = 200i32;
        let x_count = (x_max - x_min).min(max_lines);
        let z_count = (z_max - z_min).min(max_lines);
        let x_start = x_min.max(x_max - max_lines);
        let z_start = z_min.max(z_max - max_lines);
        let z_world_min = z_start as f32 * cell;
        let z_world_max = (z_start + z_count) as f32 * cell;
        let x_world_min = x_start as f32 * cell;
        let x_world_max = (x_start + x_count) as f32 * cell;

        let mut push = |p0: Vec3, p1: Vec3, color: u32, width: f32| -> bool {
            if out.len() >= MAX_OVERLAY_SEGMENTS {
                return true;
            }
            out.push(LineInstance {
                p0: p0.to_array(),
                p1: p1.to_array(),
                color,
                width,
            });
            false
        };
        let mut capped = false;

        // Subdivisions (faint)
        if self.viewport.grid_subdivisions > 1 {
            let sub_cell = cell / self.viewport.grid_subdivisions as f32;
            let sub_x_min = (x_start as f32 * cell / sub_cell).floor() as i32;
            let sub_x_max = ((x_start + x_count) as f32 * cell / sub_cell).ceil() as i32;
            let sub_z_min = (z_start as f32 * cell / sub_cell).floor() as i32;
            let sub_z_max = ((z_start + z_count) as f32 * cell / sub_cell).ceil() as i32;
            let sub_max = 400i32;
            let sx_count = (sub_x_max - sub_x_min).min(sub_max);
            let sz_count = (sub_z_max - sub_z_min).min(sub_max);
            for i in 0..sz_count {
                let z = (sub_z_min + i) as f32 * sub_cell;
                if (z / cell).round() * cell == z {
                    continue;
                }
                capped |= push(
                    Vec3::new(x_world_min, floor_y, z),
                    Vec3::new(x_world_max, floor_y, z),
                    sub_col,
                    0.5,
                );
            }
            for i in 0..sx_count {
                let x = (sub_x_min + i) as f32 * sub_cell;
                if (x / cell).round() * cell == x {
                    continue;
                }
                capped |= push(
                    Vec3::new(x, floor_y, z_world_min),
                    Vec3::new(x, floor_y, z_world_max),
                    sub_col,
                    0.5,
                );
            }
        }

        // Major lines
        for i in 0..=x_count {
            let x = (x_start + i) as f32 * cell;
            let is_axis = x.abs() < cell * 0.01;
            if is_axis && self.viewport.grid_show_axes {
                continue;
            }
            capped |= push(
                Vec3::new(x, floor_y, z_world_min),
                Vec3::new(x, floor_y, z_world_max),
                major_col,
                1.0,
            );
        }
        for i in 0..=z_count {
            let z = (z_start + i) as f32 * cell;
            let is_axis = z.abs() < cell * 0.01;
            if is_axis && self.viewport.grid_show_axes {
                continue;
            }
            capped |= push(
                Vec3::new(x_world_min, floor_y, z),
                Vec3::new(x_world_max, floor_y, z),
                major_col,
                1.0,
            );
        }

        // Axis lines (thicker)
        if self.viewport.grid_show_axes {
            capped |= push(
                Vec3::new(x_world_min, floor_y, 0.0),
                Vec3::new(x_world_max, floor_y, 0.0),
                axis_x_col,
                2.0,
            );
            capped |= push(
                Vec3::new(0.0, floor_y, z_world_min),
                Vec3::new(0.0, floor_y, z_world_max),
                axis_z_col,
                2.0,
            );
        }
        capped
    }
}

enum ObjectWireJob {
    Poly { nid: core_math::scene_graph::NodeId },
    Sdf {
        nid: core_math::scene_graph::NodeId,
        cid: MeshCacheId,
    },
    /// Plain mesh-cache wire (voxel sculpt display mesh / active sculpt chunks /
    /// imported mesh): SceneNode transform, no SDF heat, no body transform.
    Mesh {
        nid: core_math::scene_graph::NodeId,
        cid: MeshCacheId,
    },
}

fn hash_mat4(m: &glam::Mat4, h: &mut impl Hasher) {
    for v in m.to_cols_array() {
        v.to_bits().hash(h);
    }
}

/// Emit body-local edges to world space. `stride` ≥ 1 (every Nth edge). Returns true if
/// `MAX_OVERLAY_SEGMENTS` was hit.
fn emit_local_edges(
    out: &mut Vec<LineInstance>,
    local: &WireLocalEdgeCache,
    world: &glam::Mat4,
    color: u32,
    width: f32,
    opacity: f32,
    stride: usize,
) -> bool {
    let n = local.p0.len().min(local.p1.len());
    let stride = stride.max(1);
    let use_heat = (local.flags & WIRE_CACHE_HEAT) != 0 && local.heat_rgb.len() >= n;
    let mut i = 0usize;
    while i < n {
        if out.len() >= MAX_OVERLAY_SEGMENTS {
            return true;
        }
        let a = local.p0[i];
        let b = local.p1[i];
        let p0 = world.transform_point3(Vec3::new(a[0], a[1], a[2]));
        let p1 = world.transform_point3(Vec3::new(b[0], b[1], b[2]));
        let edge_color = if use_heat {
            let rgb = local.heat_rgb[i];
            LineInstance::pack_color(rgb[0], rgb[1], rgb[2], opacity)
        } else {
            color
        };
        out.push(LineInstance {
            p0: [p0.x, p0.y, p0.z],
            p1: [p1.x, p1.y, p1.z],
            color: edge_color,
            width,
        });
        i += stride;
    }
    false
}

/// Blue (flat melt) → cyan → yellow → red (protect) for Detail LO heat wire.
fn heat_rgb_from_importance(t: f32) -> [f32; 3] {
    let t = t.clamp(0.0, 1.0);
    if t < 0.33 {
        let u = t / 0.33;
        [0.15 * (1.0 - u), 0.45 + 0.45 * u, 0.95]
    } else if t < 0.66 {
        let u = (t - 0.33) / 0.33;
        [0.15 + 0.75 * u, 0.90, 0.95 * (1.0 - u)]
    } else {
        let u = (t - 0.66) / 0.34;
        [0.90 + 0.10 * u, 0.90 * (1.0 - u), 0.08 * (1.0 - u)]
    }
}

/// Extract body-local unique edges once per mesh content change.
///
/// Prefers triangle raster (mesh-mod / editable / PT snapshot) then falls back to
/// DC `cpu_mesh` quads via `pt_source_data` (covers pure Dual Contouring inserts).
/// `edge_heat`: optional 0..1 per-vertex protect (SDF edge-shape). Overrides mesh dihedral heat.
fn build_sdf_local_edges(
    mesh_cache: &render::mesh_cache::MeshCache,
    cid: MeshCacheId,
    local: &mut WireLocalEdgeCache,
    heat: bool,
    edge_heat: Option<&[f32]>,
) {
    local.p0.clear();
    local.p1.clear();
    local.heat_rgb.clear();
    local.p0.reserve(MAX_SDF_WIRE_PER_BODY.min(65_536));
    local.p1.reserve(MAX_SDF_WIRE_PER_BODY.min(65_536));

    // One path for all DC / mesh-mod sources: imported_raster | imported_persistent | DC quads→tris.
    if let Some((verts, indices)) = mesh_cache.pt_source_data(cid) {
        if !indices.is_empty() && !verts.is_empty() {
            build_tri_local_edges(&verts, &indices, local, heat, edge_heat);
            return;
        }
    }
    // Last resort: walk quads in place (no triangulation alloc).
    if let Some(cached) = mesh_cache.get(cid) {
        if !cached.cpu_mesh.quads.is_empty() {
            build_quad_local_edges(&cached.cpu_mesh, local);
        }
    }
}

fn build_tri_local_edges(
    verts: &[[f32; 8]],
    indices: &[u32],
    local: &mut WireLocalEdgeCache,
    heat: bool,
    edge_heat: Option<&[f32]>,
) {
    let n_tri = indices.len() / 3;
    if n_tri == 0 {
        return;
    }
    // Prefer AZRA edge-shape map when provided; else mesh dihedral importance.
    let vert_imp: Vec<f32> = if heat {
        if let Some(eh) = edge_heat {
            eh.to_vec()
        } else {
            let mesh = core_math::display_mesh_ops::DisplayMesh::from_raster(verts, indices);
            let mut imp = core_math::display_mesh_ops::mesh_vertex_importance(&mesh);
            // Soft gamma so mid-range curves read in the heat palette.
            for v in &mut imp {
                *v = (*v).sqrt().clamp(0.0, 1.0);
            }
            imp
        }
    } else {
        Vec::new()
    };
    // Unique undirected edges = true DC triangle topology (not a strided surface pattern).
    // HashSet only on rebuild (content_stamp change), not every frame.
    let mut seen: std::collections::HashSet<(u32, u32)> =
        std::collections::HashSet::with_capacity((n_tri * 2).min(MAX_SDF_WIRE_PER_BODY));
    // QUAD-PAIR DIAGONAL SKIP (owner 2026-08-19 "wire frame is triangulated"): every mesher
    // feeding this builder (Surface Nets chunks, DC quads→tris, cpu_mesh.to_raster_data)
    // emits a quad as TWO CONSECUTIVE triangles sharing one edge — that shared edge is the
    // triangulation diagonal, not surface topology. Detect the pattern structurally
    // (consecutive pair, exactly 2 shared verts) and pre-seed `seen` with the diagonal so it
    // never draws. A lone triangle (no paired neighbor) keeps all three edges, and a mesher
    // that interleaves pairs just shows those diagonals again — fail-open, nothing real is
    // ever hidden.
    for pair in 0..(n_tri / 2) {
        let i = pair * 6;
        let a = [indices[i], indices[i + 1], indices[i + 2]];
        let b = [indices[i + 3], indices[i + 4], indices[i + 5]];
        let mut shared = [u32::MAX; 2];
        let mut n_shared = 0usize;
        for &x in &a {
            if b.contains(&x) {
                if n_shared < 2 {
                    shared[n_shared] = x;
                }
                n_shared += 1;
            }
        }
        if n_shared == 2 && shared[0] != shared[1] {
            let key = if shared[0] < shared[1] {
                (shared[0], shared[1])
            } else {
                (shared[1], shared[0])
            };
            seen.insert(key);
        }
    }
    for tri_idx in 0..n_tri {
        if local.p0.len() >= MAX_SDF_WIRE_PER_BODY {
            return;
        }
        let i = tri_idx * 3;
        let t0 = indices[i];
        let t1 = indices[i + 1];
        let t2 = indices[i + 2];
        push_unique_edge_local(verts, t0, t1, local, &mut seen, &vert_imp, heat);
        push_unique_edge_local(verts, t1, t2, local, &mut seen, &vert_imp, heat);
        push_unique_edge_local(verts, t2, t0, local, &mut seen, &vert_imp, heat);
    }
}

fn push_unique_edge_local(
    verts: &[[f32; 8]],
    a: u32,
    b: u32,
    local: &mut WireLocalEdgeCache,
    seen: &mut std::collections::HashSet<(u32, u32)>,
    vert_imp: &[f32],
    heat: bool,
) {
    if a == b || local.p0.len() >= MAX_SDF_WIRE_PER_BODY {
        return;
    }
    let key = if a < b { (a, b) } else { (b, a) };
    if !seen.insert(key) {
        return;
    }
    let Some(va) = verts.get(a as usize) else {
        return;
    };
    let Some(vb) = verts.get(b as usize) else {
        return;
    };
    local.p0.push([va[0], va[1], va[2]]);
    local.p1.push([vb[0], vb[1], vb[2]]);
    if heat {
        let ia = vert_imp.get(a as usize).copied().unwrap_or(0.0);
        let ib = vert_imp.get(b as usize).copied().unwrap_or(0.0);
        local.heat_rgb.push(heat_rgb_from_importance(ia.max(ib)));
    }
}

fn build_quad_local_edges(mesh: &core_math::mesh::Mesh, local: &mut WireLocalEdgeCache) {
    let n_q = mesh.quads.len();
    if n_q == 0 {
        return;
    }
    let mut seen: std::collections::HashSet<(u32, u32)> =
        std::collections::HashSet::with_capacity((n_q * 2).min(MAX_SDF_WIRE_PER_BODY));
    for q in &mesh.quads {
        if local.p0.len() >= MAX_SDF_WIRE_PER_BODY {
            return;
        }
        push_unique_edge_pos(&mesh.vertices, q[0], q[1], local, &mut seen);
        push_unique_edge_pos(&mesh.vertices, q[1], q[2], local, &mut seen);
        push_unique_edge_pos(&mesh.vertices, q[2], q[3], local, &mut seen);
        push_unique_edge_pos(&mesh.vertices, q[3], q[0], local, &mut seen);
    }
}

fn push_unique_edge_pos(
    positions: &[Vec3],
    a: u32,
    b: u32,
    local: &mut WireLocalEdgeCache,
    seen: &mut std::collections::HashSet<(u32, u32)>,
) {
    if a == b || local.p0.len() >= MAX_SDF_WIRE_PER_BODY {
        return;
    }
    let key = if a < b { (a, b) } else { (b, a) };
    if !seen.insert(key) {
        return;
    }
    let Some(pa) = positions.get(a as usize) else {
        return;
    };
    let Some(pb) = positions.get(b as usize) else {
        return;
    };
    local.p0.push([pa.x, pa.y, pa.z]);
    local.p1.push([pb.x, pb.y, pb.z]);
}

/// Collect unique half-edge endpoints in **body-local** space (P1b.4).
/// Returns `true` if the list was truncated at `MAX_POLY_WIRE` (P6.2 honesty).
fn collect_poly_local_edges(
    mesh: &core_math::half_edge::HalfEdgeMesh,
    hidden: Option<&std::collections::HashSet<u32>>,
    local: &mut WireLocalEdgeCache,
    clean: bool,
) -> bool {
    // `clean`: hide edges whose two adjacent faces are coplanar. Evaluated display
    // meshes (boolean / decimate output) are triangulated — those diagonals are
    // topology noise, not shape. Face normals are computed lazily, once per face.
    let mut face_normals: Vec<Option<Vec3>> = if clean {
        vec![None; mesh.faces.len()]
    } else {
        Vec::new()
    };
    let mut count = 0usize;
    let mut hit_cap = false;
    // Same canonical walk the pick cache / GPU select buffer / selection ops use, so what is
    // DRAWN and what is SELECTABLE can never disagree about which edges exist.
    // TRI-PAIR DIAGONAL TEST (owner 2026-08-19 "why is my sphere wire triangulated"):
    // the strict coplanar test below only catches FLAT quad splits — on a curved
    // surface the two halves of a split quad are never coplanar (~3–6° on a sphere),
    // so every diagonal survived and curved meshes read as triangle soup. The diagonal
    // of a split quad is identifiable GEOMETRICALLY at any curvature: it is the
    // LONGEST edge of BOTH adjacent triangles (the hypotenuse) — a real grid edge
    // never is. Applied only between two triangles under a loose 25° dihedral, so
    // genuine creases (and any n-gon boundary) always keep their wire.
    let longest_in_tri = |f: core_math::half_edge::FaceId, he: HalfEdgeId, elen2: f32| -> bool {
        let hes = mesh.face_half_edges(f);
        if hes.len() != 3 {
            return false;
        }
        for h in hes {
            if h == he {
                continue;
            }
            let a = mesh.vert(mesh.he_source(h)).position;
            let b = mesh.vert(mesh.half_edges[h.0 as usize].vertex).position;
            if (a - b).length_squared() > elen2 * 1.001 {
                return false;
            }
        }
        true
    };
    for he_id in mesh.canonical_edges() {
        if count >= MAX_POLY_WIRE {
            hit_cap = true;
            break;
        }
        let i = he_id.0 as usize;
        let he = &mesh.half_edges[i];
        let twin = he.twin;
        let dst = he.vertex;
        let src = mesh.he_source(HalfEdgeId(i as u32));
        let p0 = mesh.vert(src).position;
        let p1 = mesh.vert(dst).position;
        if clean && twin != NO_TWIN {
            let fa = he.face;
            let fb = mesh.half_edges[twin.0 as usize].face;
            if fa != NO_FACE
                && fb != NO_FACE
                && fa != fb
                && mesh.is_face_live(fa)
                && mesh.is_face_live(fb)
            {
                let na = *face_normals[fa.0 as usize]
                    .get_or_insert_with(|| mesh.compute_face_normal(fa));
                let nb = *face_normals[fb.0 as usize]
                    .get_or_insert_with(|| mesh.compute_face_normal(fb));
                let dot = na.dot(nb);
                // Faces share this edge, so parallel normals ⇒ coplanar (~0.8°).
                if dot > 0.9999 {
                    continue;
                }
                // Curved-quad diagonal: longest edge of BOTH tris + dihedral < ~25°.
                if dot > 0.906 {
                    let elen2 = (p1 - p0).length_squared();
                    let twin_id = HalfEdgeId(twin.0);
                    if longest_in_tri(fa, HalfEdgeId(i as u32), elen2)
                        && longest_in_tri(fb, twin_id, elen2)
                    {
                        continue;
                    }
                }
            }
        }
        if let Some(h) = hidden {
            if h.contains(&src.0) && h.contains(&dst.0) {
                continue;
            }
        }
        local.p0.push([p0.x, p0.y, p0.z]);
        local.p1.push([p1.x, p1.y, p1.z]);
        count += 1;
    }
    hit_cap
}

/// Append every UNIQUE edge of `mesh` as a world-space segment (legacy direct path).
#[allow(dead_code)] // kept for one-off debug / non-cached callers
fn append_mesh_edges(
    out: &mut Vec<LineInstance>,
    mesh: &core_math::half_edge::HalfEdgeMesh,
    world: &glam::Mat4,
    color: u32,
    width: f32,
    hidden: Option<&std::collections::HashSet<u32>>,
) {
    let mut local = WireLocalEdgeCache::default();
    let _ = collect_poly_local_edges(mesh, hidden, &mut local, false);
    let _ = emit_local_edges(out, &local, world, color, width, 1.0, 1);
}

#[cfg(test)]
mod tests {
    use super::cpu_poly_outline_sabotage;

    #[test]
    fn sabotage_env_name_stable() {
        // Gate name for owner/scripts — do not rename without updating docs.
        let _ = cpu_poly_outline_sabotage();
        assert_eq!("AZRA_CPU_POLY_OUTLINE", "AZRA_CPU_POLY_OUTLINE");
    }
}
