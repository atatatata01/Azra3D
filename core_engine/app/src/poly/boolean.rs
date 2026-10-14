//! Poly mesh boolean (X pie) — cutter → targets execution.
//!
//! Kernel: `core_math::mesh_boolean` (BSP CSG, closed meshes only). Selection semantics:
//!   * Object mode (multi-select): **ACTIVE** poly object = TARGET (carved / kept);
//!     other selected poly objects = CUTTERS. Matches Join (Ctrl+J) and Blender Bool Tool
//!     (active is the object that receives the result). When several cutters are selected,
//!     the first non-active in selection order is the cutter (v1: one cutter per invoke).
//!   * Object mode (single select): selected body = cutter; targets = every other
//!     PolygonalBody whose world AABB intersects the cutter's.
//!   * Edit mode (sub-elements): the mesh island(s) containing the selection = cutter
//!     (extracted as a temp mesh); targets = the remainder of the same mesh (if it
//!     intersects) + every other intersecting PolygonalBody.
//! Everything runs in WORLD space (both meshes transformed out, result transformed back
//! into the target's local frame). One `transaction()` per pie invocation. A target whose
//! boolean fails the install guard (kernel None / validate() error / new boundary edges)
//! is left untouched — mirrors the bevel install guard in `poly_bevel_regenerate`.

use std::collections::{HashMap, HashSet};

use std::sync::Arc;
use glam::{Mat4, Vec3};
use core_math::half_edge::{HalfEdgeMesh, FaceId, HalfEdgeId, NO_EDGE};
use core_math::mesh_boolean::{mesh_boolean, boundary_half_edge_count, keep_operand_ids, BoolOp};
use core_math::scene_graph::{NodeId, NodePayload};
use crate::poly::bool_material::FaceMatPlan;
use crate::{AzraApp, DirtyFlags, ModelingContext, PolyWorkMode, SceneItemId};

// ============================================================
// Pure helpers (module-local)
// ============================================================

/// Copy of `mesh` with every vertex position mapped through `m` (normals recomputed).
/// Searched for an existing whole-mesh transform helper: none found (only per-point
/// `transform_point3` call sites) — smallest new helper. `pub(crate)` so the Join and the
/// edit-mode Shift+A merge (creation.rs) share this exact bake, no duplication.
pub(crate) fn transformed(mesh: &HalfEdgeMesh, m: Mat4) -> HalfEdgeMesh {
    let mut out = mesh.clone();
    for v in out.vertices.iter_mut() {
        v.position = m.transform_point3(v.position);
    }
    out.recompute_normals();
    out
}

/// World-space AABB: local bbox corners pushed through the node's world matrix.
fn world_aabb(mesh: &HalfEdgeMesh, m: Mat4) -> (Vec3, Vec3) {
    let (lmin, lmax) = mesh.bounding_box();
    let mut wmin = Vec3::splat(f32::MAX);
    let mut wmax = Vec3::splat(f32::MIN);
    for i in 0..8u32 {
        let c = Vec3::new(
            if i & 1 == 0 { lmin.x } else { lmax.x },
            if i & 2 == 0 { lmin.y } else { lmax.y },
            if i & 4 == 0 { lmin.z } else { lmax.z },
        );
        let p = m.transform_point3(c);
        wmin = wmin.min(p);
        wmax = wmax.max(p);
    }
    (wmin, wmax)
}

fn aabb_overlap(a: (Vec3, Vec3), b: (Vec3, Vec3)) -> bool {
    a.0.cmple(b.1).all() && b.0.cmple(a.1).all()
}

/// Extract a face subset (`invert = false`) or its complement (`invert = true`) as a new
/// standalone mesh, rebuilt via `from_polygons` (the `bevel_op.rs` pattern). Returns None
/// when the subset is empty.
/// Test-only alias (diag_fl4_island_bool) — same extraction the island plan uses.
#[cfg(test)]
pub(crate) fn mesh_from_faces_diag(mesh: &HalfEdgeMesh, faces: &HashSet<u32>) -> Option<HalfEdgeMesh> {
    mesh_from_faces(mesh, faces, false)
}

fn mesh_from_faces(mesh: &HalfEdgeMesh, faces: &HashSet<u32>, invert: bool) -> Option<HalfEdgeMesh> {
    let mut positions: Vec<Vec3> = Vec::new();
    let mut remap: HashMap<u32, usize> = HashMap::new();
    let mut rings: Vec<Vec<usize>> = Vec::new();
    let mut ring_mats: Vec<u32> = Vec::new();
    for i in 0..mesh.faces.len() as u32 {
        let fid = FaceId(i);
        if mesh.face(fid).half_edge == NO_EDGE {
            continue; // freed slot
        }
        if faces.contains(&i) == invert {
            continue;
        }
        let ring: Vec<usize> = mesh
            .face_vertices(fid)
            .iter()
            .map(|v| {
                *remap.entry(v.0).or_insert_with(|| {
                    positions.push(mesh.vert(*v).position);
                    positions.len() - 1
                })
            })
            .collect();
        if ring.len() >= 3 {
            // `from_polygons` resets every face to material 0 — snapshot the source ring's
            // material and write it back after the rebuild (same carry as `merge_mesh_list`).
            ring_mats.push(mesh.face(fid).material_id);
            rings.push(ring);
        }
    }
    if rings.is_empty() {
        None
    } else {
        let mut out = HalfEdgeMesh::from_polygons(&positions, &rings);
        for (i, &mid) in ring_mats.iter().enumerate() {
            if i < out.faces.len() {
                out.faces[i].material_id = mid;
            }
        }
        Some(out)
    }
}

/// Concatenate two meshes into one (disjoint components) via a merged polygon soup.
/// `pub(crate)` — the single canonical mesh-concat helper, shared by Join and the edit-mode
/// Shift+A merge (creation.rs). Faces of `a` come first, then `b` (append order = selection tail).
/// Per-face `material_id` is carried through: `from_polygons` resets every face to material 0,
/// so we snapshot each source ring's material and write it back afterwards (face i == ring i,
/// since from_polygons appends exactly one face per len>=3 ring, in order, into a fresh mesh).
/// Coincident verts across the two meshes are NOT welded (correct join/concat semantics).
pub(crate) fn merge_meshes(a: &HalfEdgeMesh, b: &HalfEdgeMesh) -> HalfEdgeMesh {
    merge_mesh_list(&[a, b])
}

/// N-way form of [`merge_meshes`] — the SAME concatenation, done in ONE `from_polygons` pass.
///
/// Folding `merge_meshes` over N meshes is O(N²): every step rebuilds the whole accumulated
/// soup. Scatter produces up to `core_math::scatter::MAX_SCATTER_INSTANCES` islands at once,
/// where that is the difference between instant and unusable. `merge_meshes` delegates here so
/// there is still exactly ONE concatenation implementation.
pub(crate) fn merge_mesh_list(meshes: &[&HalfEdgeMesh]) -> HalfEdgeMesh {
    let mut positions: Vec<Vec3> = Vec::new();
    let mut rings: Vec<Vec<usize>> = Vec::new();
    let mut ring_mats: Vec<u32> = Vec::new();
    for &mesh in meshes {
        let mut remap: HashMap<u32, usize> = HashMap::new();
        for i in 0..mesh.faces.len() as u32 {
            let fid = FaceId(i);
            if mesh.face(fid).half_edge == NO_EDGE {
                continue;
            }
            let ring: Vec<usize> = mesh
                .face_vertices(fid)
                .iter()
                .map(|v| {
                    *remap.entry(v.0).or_insert_with(|| {
                        positions.push(mesh.vert(*v).position);
                        positions.len() - 1
                    })
                })
                .collect();
            if ring.len() >= 3 {
                ring_mats.push(mesh.face(fid).material_id);
                rings.push(ring);
            }
        }
    }
    let mut out = HalfEdgeMesh::from_polygons(&positions, &rings);
    for (i, &mid) in ring_mats.iter().enumerate() {
        if i < out.faces.len() {
            out.faces[i].material_id = mid;
        }
    }
    out
}

/// Carry a per-face material plan built on the boolean result `res` (indexed by `res`'s `FaceId`)
/// onto `installed`. `res` was either installed as-is (`merged_from_parts == false`) or placed
/// FIRST in a [`merge_mesh_list`] concatenation, whose rings are each part's live faces (ring ≥ 3)
/// in face-id order — so `res`'s k-th such face is merged face k. `face_map` then takes the
/// pre-install mesh's faces to `installed`'s ([`keep_operand_ids`]). A face the plan does not
/// cover (an unreached island, the kept cutter) resolves to its own material under every policy.
fn carry_mat_plan(
    res: &HalfEdgeMesh,
    merged_from_parts: bool,
    plan: &[FaceMatPlan],
    face_map: &[FaceId],
    installed: &HalfEdgeMesh,
) -> Vec<FaceMatPlan> {
    let mut out: Vec<FaceMatPlan> = installed
        .faces
        .iter()
        .map(|f| FaceMatPlan { own: f.material_id, inherit: f.material_id, from_cutter: false })
        .collect();
    let mut merged_k = 0usize;
    for (i, p) in plan.iter().enumerate().take(res.faces.len()) {
        let f = FaceId(i as u32);
        if res.face(f).half_edge == NO_EDGE {
            continue;
        }
        let pre = if merged_from_parts {
            if res.face_vertices(f).len() < 3 {
                continue;
            }
            merged_k += 1;
            merged_k - 1
        } else {
            i
        };
        match face_map.get(pre) {
            Some(&nf) if nf != core_math::half_edge::NO_FACE => out[nf.0 as usize] = *p,
            _ => {}
        }
    }
    out
}

/// Partition `faces` into connected islands (shared-vertex connectivity), in ascending
/// face-index order.
///
/// Searched for an existing loose-parts splitter before writing this: none found
/// (`poly_boolean_island_faces` floods ONE island from a selection, `unbevel::grow_face_component`
/// is core_math-private and seed-driven, `separate.rs` splits by SELECTION not by connectivity).
/// So this reuses `AzraApp::bfs_linked_vertices` — the exact flood fill the cutter island already
/// uses — rather than adding a second connectivity walk. One BFS per component, each vertex
/// stamped once: O(faces + verts), not O(faces × islands).
fn face_islands(mesh: &HalfEdgeMesh, faces: &HashSet<u32>) -> Vec<HashSet<u32>> {
    let no_hidden: HashSet<u32> = HashSet::new();
    let mut vert_island: HashMap<u32, usize> = HashMap::new();
    let mut out: Vec<HashSet<u32>> = Vec::new();
    // HashSet iteration is random order; sort so the island numbering is reproducible.
    let mut ordered: Vec<u32> = faces.iter().copied().collect();
    ordered.sort_unstable();
    for f in ordered {
        let ring = mesh.face_vertices(FaceId(f));
        let Some(first) = ring.first() else { continue };
        // A face belongs to exactly one component — any vertex membership decides (the same
        // rule `poly_boolean_island_faces` relies on).
        let id = match vert_island.get(&first.0) {
            Some(&i) => i,
            None => {
                let seeds: Vec<u32> = ring.iter().map(|v| v.0).collect();
                let i = out.len();
                for v in crate::AzraApp::bfs_linked_vertices(mesh, &seeds, &no_hidden) {
                    vert_island.insert(v, i);
                }
                out.push(HashSet::new());
                i
            }
        };
        out[id].insert(f);
    }
    out
}

/// Install guard shared by every target (mirrors the bevel install guard):
/// never install a kernel result that is non-manifold or opened new holes.
pub(crate) fn result_installable(res: &HalfEdgeMesh) -> bool {
    // validate() = twin/cycle; is_manifold() = the checks it is blind to (non-manifold edge, degenerate/
    // self-loop face, stale vertex.half_edge). The coplanar-boolean mesh-break twins cleanly and passes
    // validate() + boundary==0 — is_manifold() is what rejects it (2026-07-07 audit #1/#2).
    res.face_count() > 0
        && res.validate().is_ok()
        // Edge-manifold: bowtie VERTICES are permitted (Blender-legal vertex-touch boolean;
        // owner test-02 2026-08-04). Non-manifold EDGES still reject.
        && res.is_edge_manifold().is_ok()
        && boundary_half_edge_count(res) == 0
        // NEVER install zero-thickness folded flaps (owner console 2026-08-04): a folded
        // result is edge-consistent, so every check above passes it — but installing it
        // POISONS the body. The very next boolean then reports `target boundary=8`, is
        // refused as open input, and each following op degrades the shape further. Keeping
        // the pre-op mesh is always better than installing geometry that breaks the tool.
        && res.count_folded_coplanar_pairs() == 0
}

// ============================================================
// Plan (read phase output)
// ============================================================

struct BoolPlan {
    /// Cutter mesh in WORLD space.
    cutter_world: HalfEdgeMesh,
    /// Object-mode cutter node (deleted after success unless keep-cutter). None = island cutter.
    cutter_node: Option<NodeId>,
    /// Island cutter source: (node, island faces in WORLD space kept for keep-cutter re-append,
    /// remainder mesh in LOCAL space or None when the island was the whole mesh).
    island: Option<IslandPlan>,
    /// Whole-body targets: (nid, world matrix). Never contains the island source node.
    targets: Vec<(NodeId, Mat4)>,
}

struct IslandPlan {
    nid: NodeId,
    world: Mat4,
    /// Cutter island in LOCAL space (re-appended on keep-cutter).
    island_local: HalfEdgeMesh,
    /// The remainder islands the cutter's AABB actually REACHES, fused into one LOCAL-space
    /// mesh — the boolean target. None = the cutter reaches no other island in this body.
    remainder_target: Option<HalfEdgeMesh>,
    /// The remainder islands the cutter cannot reach, in LOCAL space. Never fed to the kernel;
    /// concatenated back onto the result so the body keeps them unchanged.
    remainder_passthrough: Option<HalfEdgeMesh>,
    /// True when the source mesh has ANY live face outside the cutter island. False = the island
    /// WAS the whole mesh, so consuming the cutter deletes the node.
    remainder_exists: bool,
}

impl AzraApp {
    /// Selected PolygonalBody nodes in SELECTION ORDER (UnifiedSelection is an IndexSet —
    /// insertion-ordered; first selected = cutter).
    fn selected_poly_nodes(&self) -> Vec<NodeId> {
        self.sel
            .usel
            .selected_nodes()
            .filter(|&nid| {
                self.scene
                    .scene_graph
                    .get(nid)
                    .map(|n| matches!(n.payload, NodePayload::PolygonalBody { .. }))
                    .unwrap_or(false)
            })
            .collect()
    }

    pub(crate) fn poly_node_mesh(&self, nid: NodeId) -> Option<Arc<HalfEdgeMesh>> {
        match &self.scene.scene_graph.get(nid)?.payload {
            NodePayload::PolygonalBody { mesh, .. } => Some(mesh.clone()),
            _ => None,
        }
    }

    /// True when there is a poly sub-element selection on the active poly node.
    fn poly_boolean_sub_selection_active(&self) -> bool {
        self.poly_work_mode != PolyWorkMode::Object
            && self.sel.usel.active_node().map_or(false, |nid| self.poly_node_mesh(nid).is_some())
            && (!self.poly_sel.selected_vertices.is_empty()
                || !self.poly_sel.selected_edges.is_empty()
                || !self.poly_sel.selected_faces.is_empty())
    }

    /// Gate for opening the X boolean pie: something must be actionable.
    pub(crate) fn poly_boolean_has_candidates(&self) -> bool {
        if self.modeling_context != ModelingContext::Polygonal {
            return false;
        }
        self.poly_boolean_sub_selection_active() || !self.selected_poly_nodes().is_empty()
    }

    /// Island faces containing the current sub-element selection (flood fill across
    /// half-edge connectivity — reuses `bfs_linked_vertices` from poly_selection.rs).
    /// NOTE: `PolySelection` stores sub-elements in `HashSet`s (no insertion order), so
    /// "the island of the FIRST-selected element" is not recoverable — ALL islands touched
    /// by the selection form the cutter (multi-component cutters are fine for the BSP).
    fn poly_boolean_island_faces(&self, nid: NodeId) -> Option<HashSet<u32>> {
        let mesh = self.poly_node_mesh(nid)?;
        let mut seeds: Vec<u32> = self.poly_sel.selected_vertices.iter().copied().collect();
        for &e in &self.poly_sel.selected_edges {
            if (e as usize) < mesh.half_edges.len() {
                seeds.push(mesh.he_source(HalfEdgeId(e)).0);
                seeds.push(mesh.he(HalfEdgeId(e)).vertex.0);
            }
        }
        for &f in &self.poly_sel.selected_faces {
            if (f as usize) < mesh.faces.len() && mesh.face(FaceId(f)).half_edge != NO_EDGE {
                for v in mesh.face_vertices(FaceId(f)) {
                    seeds.push(v.0);
                }
            }
        }
        if seeds.is_empty() {
            return None;
        }
        // Island extraction is purely topological — hidden elements do not break islands.
        let no_hidden: HashSet<u32> = HashSet::new();
        let island_verts = Self::bfs_linked_vertices(&mesh, &seeds, &no_hidden);
        let mut faces: HashSet<u32> = HashSet::new();
        for i in 0..mesh.faces.len() as u32 {
            let fid = FaceId(i);
            if mesh.face(fid).half_edge == NO_EDGE {
                continue;
            }
            // A face belongs to exactly one connected component — any vertex membership decides.
            if mesh
                .face_vertices(fid)
                .first()
                .map_or(false, |v| island_verts.contains(&v.0))
            {
                faces.insert(i);
            }
        }
        if faces.is_empty() { None } else { Some(faces) }
    }

    /// Read phase: resolve cutter + targets. Returns None (with a toast) when nothing to do.
    fn poly_boolean_plan(&mut self) -> Option<BoolPlan> {
        if self.modeling_context != ModelingContext::Polygonal {
            return None;
        }

        // ---- Edit-mode island cutter ----
        if self.poly_boolean_sub_selection_active() {
            let nid = self.sel.usel.active_node()?;
            let mesh = self.poly_node_mesh(nid)?;
            let island_faces = match self.poly_boolean_island_faces(nid) {
                Some(f) => f,
                None => {
                    self.toast("Boolean: no island under the selection");
                    return None;
                }
            };
            let island_local = match mesh_from_faces(&mesh, &island_faces, false) {
                Some(m) => m,
                None => {
                    self.toast("Boolean: could not extract the selected island");
                    return None;
                }
            };
            let world = self.scene.scene_graph.world_transform(nid);
            let cutter_world = transformed(&island_local, world);
            let cutter_aabb = world_aabb(&island_local, world);

            // The remainder is split PER ISLAND and AABB-filtered against the cutter — the same
            // rule already applied to other BODIES twenty lines down.
            //
            // Fusing every non-cutter island into ONE target mesh let an island the cut cannot
            // even reach decide the whole run. The owner's `EDGE SELECT PROBLEM, BOLEAN
            // PROBLEM.fl4` is exactly that shape: a closed 16-face yellow cutter, a closed
            // 74-face box, and a THIRD 146-face OPEN shell ~3 units away in -Z carrying 36
            // boundary half-edges. The fused target inherited those 36 edges, so the
            // closed-input preflight refused the run ("target is open — fill holes or Solidify
            // first") even though cutter and box were both perfectly closed. Islands outside
            // the cutter's AABB are carried through untouched instead of being fed to a solid
            // CSG kernel that has no business seeing them.
            let remainder_faces: HashSet<u32> = (0..mesh.faces.len() as u32)
                .filter(|&i| mesh.face(FaceId(i)).half_edge != NO_EDGE)
                .filter(|i| !island_faces.contains(i))
                .collect();
            let remainder_exists = !remainder_faces.is_empty();
            let mut target_faces: HashSet<u32> = HashSet::new();
            let mut through_faces: HashSet<u32> = HashSet::new();
            for isl_faces in face_islands(&mesh, &remainder_faces) {
                let reaches = mesh_from_faces(&mesh, &isl_faces, false)
                    .map_or(false, |im| aabb_overlap(world_aabb(&im, world), cutter_aabb));
                if reaches {
                    target_faces.extend(isl_faces);
                } else {
                    through_faces.extend(isl_faces);
                }
            }
            let remainder_target = mesh_from_faces(&mesh, &target_faces, false);
            let remainder_passthrough = mesh_from_faces(&mesh, &through_faces, false);
            let remainder_is_target = remainder_target.is_some();

            // Other poly bodies whose world AABB intersects the cutter island.
            let mut targets: Vec<(NodeId, Mat4)> = Vec::new();
            let others: Vec<NodeId> = self
                .scene
                .scene_graph
                .poly_bodies()
                .map(|(n, _)| n)
                .filter(|&n| n != nid)
                .collect();
            for other in others {
                if let Some(om) = self.poly_node_mesh(other) {
                    let ow = self.scene.scene_graph.world_transform(other);
                    if aabb_overlap(world_aabb(&om, ow), cutter_aabb) {
                        targets.push((other, ow));
                    }
                }
            }

            if !remainder_is_target && targets.is_empty() {
                self.toast("Boolean: cutter island intersects nothing");
                return None;
            }
            return Some(BoolPlan {
                cutter_world,
                cutter_node: None,
                island: Some(IslandPlan {
                    nid,
                    world,
                    island_local,
                    remainder_target,
                    remainder_passthrough,
                    remainder_exists,
                }),
                targets,
            });
        }

        // ---- Object-mode: active = TARGET, other selected = CUTTER ----
        // (Was first-selected = cutter. That inverted Subtract when the user selected the
        // base first — Difference looked empty / like Intersect. Join already uses active
        // as the survivor; match that + Blender Bool Tool.)
        let selected = self.selected_poly_nodes();
        if selected.is_empty() {
            self.toast("Boolean: select a polygonal object first");
            return None;
        }

        let (cutter_nid, targets): (NodeId, Vec<(NodeId, Mat4)>) = if selected.len() >= 2 {
            // TARGET = active poly among selection, else last selected (most recent click).
            let target_nid = match self.sel.usel.active_node() {
                Some(n) if selected.contains(&n) => n,
                _ => *selected.last().unwrap(),
            };
            // CUTTER = first non-target in selection order (v1: one cutter).
            let Some(&cutter_nid) = selected.iter().find(|&&n| n != target_nid) else {
                self.toast("Boolean: need a second poly object as the cutter");
                return None;
            };
            let tw = self.scene.scene_graph.world_transform(target_nid);
            (cutter_nid, vec![(target_nid, tw)])
        } else {
            // Single selection: that body is the cutter; stamp into every AABB hit.
            let cutter_nid = selected[0];
            let cutter_mesh = self.poly_node_mesh(cutter_nid)?;
            let cutter_mat = self.scene.scene_graph.world_transform(cutter_nid);
            let cutter_aabb = world_aabb(&cutter_mesh, cutter_mat);
            let mut targets: Vec<(NodeId, Mat4)> = Vec::new();
            let others: Vec<NodeId> = self
                .scene
                .scene_graph
                .poly_bodies()
                .map(|(n, _)| n)
                .filter(|&n| n != cutter_nid)
                .collect();
            for other in others {
                if let Some(om) = self.poly_node_mesh(other) {
                    let ow = self.scene.scene_graph.world_transform(other);
                    if aabb_overlap(world_aabb(&om, ow), cutter_aabb) {
                        targets.push((other, ow));
                    }
                }
            }
            (cutter_nid, targets)
        };

        if targets.is_empty() {
            self.toast("Boolean: no intersecting poly targets (select cutter + active target)");
            return None;
        }

        let cutter_mesh = self.poly_node_mesh(cutter_nid)?;
        let cutter_mat = self.scene.scene_graph.world_transform(cutter_nid);
        let cutter_world = transformed(&cutter_mesh, cutter_mat);
        Some(BoolPlan {
            cutter_world,
            cutter_node: Some(cutter_nid),
            island: None,
            targets,
        })
    }

    /// Run the chosen boolean op: cutter → each target, ONE undo entry for everything
    /// (all target installs + cutter deletion), via the transaction API.
    pub(crate) fn poly_boolean_execute(&mut self, op: BoolOp) {
        let keep_cutter = self.ui.bool_pie_keep_cutter;
        let plan = match self.poly_boolean_plan() {
            Some(p) => p,
            None => return,
        };

        // ---- Compute phase (pure reads — kernel runs BEFORE the transaction so a
        //      fully-failed run creates no undo entry).
        // ALWAYS target-first (A op B): A = target, B = cutter → Subtract = A − B.
        //
        // Solid BSP CSG requires CLOSED manifolds on BOTH inputs (BOOLEAN_V1.md). Preflight
        // names which mesh is open so "subtract cube from open shape" is not a silent failure.
        let cutter_open = boundary_half_edge_count(&plan.cutter_world) > 0;
        if cutter_open {
            self.toast(
                "Boolean: cutter has open edges (holes / non-solid). \
                 Use a closed volume as the cutter, or fill the holes first.",
            );
            return;
        }
        // Returns the result mesh + the half-edges THIS run created on it (ids valid on the
        // returned mesh; `transformed` preserves ids). Carried per-run — a thread-local here
        // once let the island run (which executes LAST) clobber the whole-body runs' edge
        // lists before the transaction stamped them.
        // Cutter-material policy for THIS run (kept adjustable afterwards by the redo panel).
        let mat_mode = self.ui.bool_cutter_material;
        let run = |target_world_mesh: &HalfEdgeMesh|
            -> Result<(HalfEdgeMesh, Vec<HalfEdgeId>, Vec<FaceMatPlan>), &'static str> {
            if boundary_half_edge_count(target_world_mesh) > 0 {
                return Err(
                    "target is open (boundary edges) — solid boolean needs a closed mesh; \
                     fill holes or Solidify first",
                );
            }
            // ID-STABLE (POLY-STALE-IDS): the result comes back numbered on the TARGET's ids
            // (`transformed` keeps ids), so everything the cut did not consume keeps the id the
            // selection / hidden set / hover / active vertex / sidecar hold for it — and `prov`,
            // `created` and the material plan below are all indexed on those final ids.
            let Some((res, prov)) = core_math::mesh_boolean::mesh_boolean_keeping_ids(
                target_world_mesh,
                &plan.cutter_world,
                op,
            ) else {
                return Err("kernel rejected (empty / volume gate / degenerate)");
            };
            // The edges this cut CREATED, recorded on the result while we still know. Bevel
            // Smart Select reads this later to skip a cylinder's seams, and it must survive
            // a destructive boolean -- which is the owner's actual workflow (his stack holds
            // only a Bevel; the cut was applied in edit mode).
            let mut created: Vec<core_math::half_edge::HalfEdgeId> = Vec::new();
            {
                let v = &mut created;
                // ONLY THE CUTTER'S OWN TESSELLATION (owner 2026-08-04: "when I did a
                // boolean I feel it re-tags all edges, not just the cylinder ones"). He is
                // right: `edge_is_new` marks every edge absent from the inputs, which also
                // catches ORIGINAL edges the cut merely SPLIT -- they are new by identity
                // but not new geometry, and tagging them stopped real corners beveling.
                // `edge_is_cutter_interior` needs the cutter on BOTH sides, so the rim where
                // the hole meets the surface stays beveled -- the same rule the bevel uses.
                for i in 0..res.half_edges.len() as u32 {
                    let he = core_math::half_edge::HalfEdgeId(i);
                    if res.is_edge_live(he) && prov.edge_is_cutter_interior(&res, he) {
                        v.push(he);
                    }
                }
                // PLUS the flat SUPPORT edges the cut created (owner 2026-08-05: "support
                // segment needs to be ignored by the bevel modifier with smart select").
                // These fell out of the tag set when it was narrowed to cutter tessellation:
                // a support edge has the flat cut face on both sides, so
                // edge_is_cutter_interior never matches it. And the GEOMETRY rule cannot
                // catch it either - it defers ALL flat edges to tags, because a knife cut
                // is geometrically identical and must keep beveling. new_flat_edges is
                // exactly "created by this cut AND flat", which distinguishes the two by
                // provenance: knife edges are not boolean-created, so they stay untagged
                // and bevel; support edges are, so they are skipped.
                for e in prov.new_flat_edges(&res) {
                    let he = core_math::half_edge::HalfEdgeId(e);
                    v.push(he);
                    let tw = res.he(he).twin;
                    if tw != core_math::half_edge::NO_TWIN {
                        v.push(tw);
                    }
                }
            }
            if !result_installable(&res) {
                return Err("result not a closed manifold");
            }
            // MATERIALS: the kernel rebuilds through `from_polygons`, which stamps material 0 on
            // every face — so without this the carved body loses ALL per-face materials (owner
            // 2026-08-07). Assign from the input face each result face lies on, and keep BOTH
            // candidates so the cutter policy stays adjustable after the destructive op.
            let mut res = res;
            let mat_plan = crate::poly::bool_material::plan_result_materials(
                &res,
                target_world_mesh,
                &plan.cutter_world,
                &prov,
            );
            crate::poly::bool_material::apply_material_plan(&mut res, &mat_plan, mat_mode);
            Ok((res, created, mat_plan))
        };

        // Whole-body targets: (nid, new LOCAL mesh, created edges — ids valid on that mesh).
        let mut installs: Vec<(NodeId, HalfEdgeMesh, Vec<HalfEdgeId>, Vec<FaceMatPlan>)> =
            Vec::new();
        let mut failed: usize = 0;
        let mut last_fail: Option<&'static str> = None;
        for &(nid, world) in &plan.targets {
            let Some(t_mesh) = self.poly_node_mesh(nid) else {
                failed += 1;
                last_fail = Some("missing target mesh");
                continue;
            };
            let t_world_mesh = transformed(&t_mesh, world);
            match run(&t_world_mesh) {
                Ok((res_world, created, mat_plan)) => installs.push((
                    nid,
                    transformed(&res_world, world.inverse()),
                    created,
                    mat_plan,
                )),
                Err(why) => {
                    failed += 1;
                    last_fail = Some(why);
                }
            }
        }

        // Island-remainder target / island source-mesh rewrite.
        // (None = leave the island source node untouched; None mesh = delete node.)
        // `cut` carries the boolean's own created edges when a boolean actually ran on this
        // node: (donor mesh the ids are valid on, edge ids). The FINAL mesh can be a
        // `merge_meshes` rebuild (keep-cutter) that renumbers every id, so the stamp goes
        // through position matching against the donor, not raw ids.
        struct IslandNewMesh {
            mesh: HalfEdgeMesh,
            cut: Option<(HalfEdgeMesh, Vec<HalfEdgeId>)>,
            /// Per-face material plan, indexed on `mesh` — carried from the boolean result through
            /// the concatenation and the id renumbering by [`carry_mat_plan`]; every tail part
            /// (unreached islands, the kept cutter) resolves to its own material.
            mat_plan: Vec<FaceMatPlan>,
        }
        let mut island_install: Option<(NodeId, Option<IslandNewMesh>)> = None;
        if let Some(isl) = &plan.island {
            match (&isl.remainder_target, isl.remainder_exists) {
                (Some(rem), _) => {
                    let rem_world = transformed(rem, isl.world);
                    match run(&rem_world) {
                        Ok((res_world, created, mat_plan)) => {
                            let res_local = transformed(&res_world, isl.world.inverse());
                            // Result FIRST (mat_plan indexes it), then the islands the cut could
                            // not reach, then the cutter island when it is kept.
                            let mut parts: Vec<&HalfEdgeMesh> = vec![&res_local];
                            if let Some(p) = &isl.remainder_passthrough {
                                parts.push(p);
                            }
                            if keep_cutter {
                                parts.push(&isl.island_local);
                            }
                            let merged_from_parts = parts.len() > 1;
                            let assembled = if merged_from_parts {
                                merge_mesh_list(&parts)
                            } else {
                                res_local.clone()
                            };
                            // ID-STABLE (POLY-STALE-IDS): `rem` was extracted from the body, so
                            // the run's ids are rem's, and the concatenation renumbers anyway —
                            // renumber the assembled mesh onto the BODY's own ids instead, and
                            // carry the plan through the same face map.
                            let (final_mesh, mat_plan) = match self.poly_node_mesh(isl.nid) {
                                Some(body) => {
                                    let (m, face_map) = keep_operand_ids(&body, &assembled);
                                    let p = carry_mat_plan(&res_local, merged_from_parts, &mat_plan, &face_map, &m);
                                    (m, p)
                                }
                                None => (assembled, mat_plan),
                            };
                            island_install = Some((
                                isl.nid,
                                Some(IslandNewMesh {
                                    mesh: final_mesh,
                                    cut: Some((res_local, created)),
                                    mat_plan,
                                }),
                            ));
                        }
                        Err(why) => {
                            failed += 1;
                            last_fail = Some(why);
                        }
                    }
                }
                (None, true) => {
                    // The cutter reaches no island in this body — nothing here is carved, so
                    // the remainder is exactly the unreached (passthrough) set. Just detach the
                    // cutter island, and only when at least one real target succeeded (the
                    // cutter is consumed by a successful apply, never destroyed by a failed run).
                    if !keep_cutter && !installs.is_empty() {
                        if let Some(rem) = &isl.remainder_passthrough {
                            // The passthrough was EXTRACTED (renumbered): back onto the body's ids.
                            let mesh = match self.poly_node_mesh(isl.nid) {
                                Some(body) => keep_operand_ids(&body, rem).0,
                                None => rem.clone(),
                            };
                            island_install = Some((
                                isl.nid,
                                Some(IslandNewMesh {
                                    mesh,
                                    cut: None,
                                    mat_plan: Vec::new(),
                                }),
                            ));
                        }
                    }
                }
                (None, false) => {
                    // The island WAS the whole mesh — deleting the cutter deletes the node.
                    if !keep_cutter && !installs.is_empty() {
                        island_install = Some((isl.nid, None));
                    }
                }
            }
        }

        let succeeded = installs.len()
            + island_install.is_some() as usize;
        if installs.is_empty() && island_install.is_none() {
            let detail = last_fail.unwrap_or("no target produced a valid closed mesh");
            self.toast(&format!(
                "Boolean {}: {detail} — nothing changed \
                 (active poly = target that keeps the result, other selected = cutter)",
                op_label(op)
            ));
            return;
        }

        // ---- Write phase: ONE undo entry for all installs + cutter deletion.
        let delete_cutter_node = match plan.cutter_node {
            Some(nid) if !keep_cutter && !installs.is_empty() => Some(nid),
            _ => None,
        };
        // The body that SURVIVES holding the result. Deleting the cutter can leave the active
        // node dangling, and with no active poly body the shell falls back to Object mode — so a
        // boolean run from inside edit mode kicked the user out (owner 2026-08-02: "using a
        // boolean operation in edit mode should not finish in the object mode"). Re-selecting the
        // survivor keeps `active_poly_nid` valid, so `poly_work_mode` (never touched here) stays
        // in effect. Join already did this; execute did not.
        let keep_nid = installs
            .first()
            .map(|(nid, _, _, _)| *nid)
            .or_else(|| island_install.as_ref().and_then(|(nid, m)| m.as_ref().map(|_| *nid)));
        let tx_name = format!("Poly Boolean {}", op_label(op));
        // Redo-panel payload: every body the boolean wrote + its per-face material plan, so the
        // cutter-material policy stays adjustable AFTER this destructive op (pure re-stamp).
        let mut adjust_targets: Vec<(NodeId, Vec<FaceMatPlan>)> = installs
            .iter()
            .map(|(nid, _, _, plan)| (*nid, plan.clone()))
            .collect();
        if let Some((nid, Some(inst))) = island_install.as_ref().map(|(n, m)| (*n, m.as_ref())) {
            if !inst.mat_plan.is_empty() {
                adjust_targets.push((nid, inst.mat_plan.clone()));
            }
        }
        self.transaction(&tx_name, |s| {
            for (nid, new_mesh, created, _mat_plan) in installs {
                // BOOL-STALECD IS OVER: carry the sidecar across instead of dropping it.
                // The ids are stale, but POSITIONS are not for geometry the cut did not
                // touch, so `remap_by_position` re-keys every layer onto the new mesh and
                // leaves cut-created elements at their defaults. Dropping it is what made
                // UVs / seams / creases / polygroups vanish on every boolean.
                let remapped = s
                    .cache
                    .poly_custom_data
                    .get(&nid)
                    .zip(s.poly_node_mesh(nid))
                    .map(|(cd, old)| cd.remap_by_position(&old, &new_mesh));
                match remapped {
                    Some(mut cd) => {
                        // This cut's OWN created edges — ids valid on `new_mesh` directly.
                        for &he in &created {
                            cd.op_created.set(he, true);
                        }
                        s.cache.poly_custom_data.insert(nid, cd);
                    }
                    // No sidecar, or the old mesh was unreadable: fall back to the old
                    // behaviour rather than leaving stale ids in place.
                    None => {
                        s.cache.poly_custom_data.remove(&nid);
                    }
                }
                if let Some(node) = s.scene.scene_graph.get_mut(nid) {
                    if let NodePayload::PolygonalBody { mesh, .. } = &mut node.payload {
                        *mesh = Arc::new(new_mesh);
                    }
                }
                s.poly_request_display_sync(nid);
            }
            if let Some((nid, new_mesh)) = island_install {
                match new_mesh {
                    Some(inst) => {
                        // Same remap-then-stamp carry as the whole-body install above — the
                        // island branch used to `remove(&nid)` here (BOOL-STALECD), which is
                        // what wiped EVERY tag on an edit-mode "alternative cut" (owner
                        // test_014 2026-08-05). Unmatched entries (geometry the cut consumed,
                        // or edges that went to the other island) stay at defaults: fail
                        // closed, never guessed onto a stranger.
                        let remapped = s
                            .cache
                            .poly_custom_data
                            .get(&nid)
                            .zip(s.poly_node_mesh(nid))
                            .map(|(cd, old)| cd.remap_by_position(&old, &inst.mesh));
                        match remapped {
                            Some(mut cd) => {
                                if let Some((donor, created)) = &inst.cut {
                                    // Position-matched: keep-cutter's merge_meshes rebuild
                                    // renumbers ids, so raw-id stamping would tag strangers.
                                    cd.stamp_op_created_by_position(donor, created, &inst.mesh);
                                }
                                s.cache.poly_custom_data.insert(nid, cd);
                            }
                            None => {
                                s.cache.poly_custom_data.remove(&nid);
                            }
                        }
                        if let Some(node) = s.scene.scene_graph.get_mut(nid) {
                            if let NodePayload::PolygonalBody { mesh, .. } = &mut node.payload {
                                *mesh = Arc::new(inst.mesh);
                            }
                        }
                        s.poly_request_display_sync(nid);
                    }
                    None => s.poly_boolean_delete_node(nid),
                }
            }
            if let Some(nid) = delete_cutter_node {
                s.poly_boolean_delete_node(nid);
            }
            // Keep the surviving body ACTIVE so an edit-mode boolean stays in edit mode.
            if let Some(nid) = keep_nid {
                s.select_single_unified(SceneItemId::Node(nid));
            }
            // Selection resets after a boolean by policy (the cut consumed or created what was
            // picked). Ids that survive — hidden set, hover, active vertex, sidecar — name the same
            // geometry: every install above is renumbered onto the body's ids.
            s.poly_sel.selected_vertices.clear();
            s.poly_sel.selected_edges.clear();
            s.poly_sel.selected_faces.clear();
            s.poly_invalidate_pick_cache();
            s.mark_dirty(DirtyFlags::RENDER);
        });

        // Open the "Last · Boolean" redo panel (Blender F9 style). The boolean itself is
        // destructive and is NOT re-run — only `face.material_id` is re-stamped from the stored
        // plan, which is exactly what makes the choice adjustable after the fact.
        if !adjust_targets.is_empty() {
            self.ui.poly_boolean_adjust = Some(crate::poly::bool_material::PolyBooleanAdjust {
                op_label: op_label(op).to_string(),
                targets: adjust_targets,
                mode: mat_mode,
                needs_settle: false,
            });
        }

        if failed > 0 {
            self.toast(&format!(
                "Boolean {}: {} applied, {} target(s) skipped (non-manifold result)",
                op_label(op), succeeded, failed
            ));
        } else {
            self.toast(&format!("Boolean {}: {} target(s) updated", op_label(op), succeeded));
        }
    }

    /// Remove a poly node + its caches + selection references (the "Delete Node"
    /// transaction body from input_keyboard.rs, reused for cutter deletion — called
    /// INSIDE an enclosing transaction, so it creates no undo entry of its own).
    fn poly_boolean_delete_node(&mut self, nid: NodeId) {
        if let Some(node) = self.scene.scene_graph.get(nid) {
            if let NodePayload::PolygonalBody { mesh_cache_id, .. } = &node.payload {
                if let Some(cid) = *mesh_cache_id {
                    self.cache.mesh_cache.remove(cid);
                }
            }
        }
        self.cache.body_vol_cache.remove_body(nid);
        self.scene.scene_graph.remove(nid);
        // Repair the PRIMARY pointer too, not just the active one (owner soak BUG-2, 2026-07-29).
        //
        // `delete_body` (scene_actions.rs) reassigns `sdf_body_nid` when it removes the node that
        // pointer names; this path did not, so removing the primary body here left it dangling.
        // Anything that later trusts it — including `active_body_nid = sdf_body_nid` on the very
        // next line — inherits a dead id, and `add_prim_to_graph` then falls into its recovery
        // path and builds a FRESH body, orphaning every existing prim.
        if nid == self.scene.sdf_body_nid {
            let root = self.scene.scene_graph.root();
            let top_bodies = self.scene.scene_graph.child_bodies(root);
            self.scene.sdf_body_nid = match top_bodies.first() {
                Some(&first) => first,
                None => self.ensure_sdf_body(),
            };
        }
        if self.scene.active_body_nid == nid {
            self.scene.active_body_nid = self.scene.sdf_body_nid;
        }
        if self.scene.isolated_body_nid == Some(nid) {
            self.scene.isolated_body_nid = None;
        }
        self.deselect_items(&[SceneItemId::Node(nid)]);
    }

    // ============================================================
    // Join (Ctrl+J) — merge selected poly bodies into the active one.
    // Cousin of the boolean above: also a destructive multi-body poly op, so it reuses the same
    // private helpers (transformed / merge_meshes / poly_boolean_delete_node / world_transform).
    // ============================================================

    /// Gate for the Join action: object-mode poly context with >= 2 selected poly bodies.
    pub(crate) fn poly_join_has_candidates(&self) -> bool {
        self.modeling_context == ModelingContext::Polygonal
            && self.poly_work_mode == PolyWorkMode::Object
            && self.selected_poly_nodes().len() >= 2
    }

    /// Blender Ctrl+J: merge every other selected PolygonalBody INTO the active one.
    /// The active node is the TARGET — it survives and keeps its own transform, name, and modifiers.
    /// Each source's geometry is baked from its world transform into the target's local space, then
    /// the source node is deleted. Per-face materials are preserved (merge_meshes carries material_id).
    /// Coincident verts are NOT welded (Blender parity — run Merge-by-Distance separately). ONE undo.
    pub(crate) fn poly_join(&mut self) {
        if !self.poly_join_has_candidates() { return; }
        let selected = self.selected_poly_nodes();
        // Target = active node when it is one of the selected poly bodies, else the first selected.
        let target = match self.sel.usel.active_node() {
            Some(n) if selected.contains(&n) => n,
            _ => selected[0],
        };
        let sources: Vec<NodeId> = selected.iter().copied().filter(|&n| n != target).collect();
        if sources.is_empty() { return; }

        let target_mesh = match self.poly_node_mesh(target) { Some(m) => m, None => return };
        let target_world_inv = self.scene.scene_graph.world_transform(target).inverse();

        // Read phase: bake each source into target-local and fold into one mesh.
        let mut merged: HalfEdgeMesh = (*target_mesh).clone();
        let mut joined = 0usize;
        for &src in &sources {
            let src_mesh = match self.poly_node_mesh(src) { Some(m) => m, None => continue };
            let to_target_local = target_world_inv * self.scene.scene_graph.world_transform(src);
            let src_in_target = transformed(&src_mesh, to_target_local);
            merged = merge_meshes(&merged, &src_in_target);
            joined += 1;
        }
        if joined == 0 { return; }

        // Write phase: ONE undo entry — install merged mesh into target, delete the source nodes.
        self.transaction("Join", |s| {
            // Same rebuild, same carry as Add to Mesh: the TARGET keeps its geometry but not its
            // ids, so its UVs / seams / creases / polygroups / Smart Select tags have to ride the
            // position remap. (The sources' layers are not carried — each source has its own
            // sidecar entry keyed to its own node, and that node is about to be deleted.)
            s.poly_install_rebuilt_mesh(target, merged, &[]);
            for &src in &sources {
                s.poly_boolean_delete_node(src);
            }
            s.select_single_unified(SceneItemId::Node(target));
            // Sub-element ids are meaningless after the rebuild.
            s.poly_sel.selected_vertices.clear();
            s.poly_sel.selected_edges.clear();
            s.poly_sel.selected_faces.clear();
            s.poly_invalidate_pick_cache();
            s.mark_dirty(DirtyFlags::RENDER);
        });
        self.toast(&format!("Joined {} bodies into 1", joined + 1));
    }

    /// Blender Shift+A in EDIT mode: append a freshly-built primitive INTO the active poly body's
    /// mesh (one datablock) instead of spawning a new body. The added island becomes the sole
    /// sub-selection so it can be grabbed immediately (Blender parity) and is NOT welded to the
    /// existing mesh. Placement priority:
    /// 1. Poly sub-selection (face/edge/vert) — center + normal, sit ON surface (no intersection)
    /// 2. Locked workplane origin + normal
    /// 3. Body local origin
    /// ONE undo entry.
    pub(crate) fn poly_add_primitive_to_body(
        &mut self,
        nid: NodeId,
        prim_mesh: HalfEdgeMesh,
        kind: &str,
        half_along_y: f32,
    ) {
        let body_mesh = match self.poly_node_mesh(nid) { Some(m) => m, None => return };
        let half_y = half_along_y.max(0.0);

        // Placement priority (match create_poly_primitive): locked WP first, then selection, else origin.
        let placed = if self.poly_workplane_is_active_set() {
            let wp = &self.viewport.wp_resolved;
            let n = wp.normal.normalize_or_zero();
            let rot = if n.length_squared() < 1e-8 {
                glam::Quat::IDENTITY
            } else {
                glam::Quat::from_rotation_arc(Vec3::Y, n)
            };
            let origin = wp.origin + n * half_y;
            let world_placement = Mat4::from_rotation_translation(rot, origin);
            let to_local = self.scene.scene_graph.world_transform(nid).inverse();
            transformed(&prim_mesh, to_local * world_placement)
        } else if let Some((point, normal)) = self.poly_selection_place_frame() {
            let n = normal.normalize_or_zero();
            let rot = if n.length_squared() < 1e-8 {
                glam::Quat::IDENTITY
            } else {
                glam::Quat::from_rotation_arc(Vec3::Y, n)
            };
            let origin = point + n * half_y;
            let world_placement = Mat4::from_rotation_translation(rot, origin);
            let to_local = self.scene.scene_graph.world_transform(nid).inverse();
            transformed(&prim_mesh, to_local * world_placement)
        } else {
            prim_mesh
        };
        let _ = kind;

        // body faces first in merge → new island is the face-index tail of the dense rebuild.
        let old_faces = body_mesh
            .faces
            .iter()
            .filter(|f| f.half_edge != NO_EDGE)
            .count();
        let merged = merge_meshes(&body_mesh, &placed);

        let mut new_faces: Vec<u32> = Vec::new();
        let mut new_verts: HashSet<u32> = HashSet::new();
        let mut new_edges: Vec<u32> = Vec::new();
        for f in old_faces..merged.faces.len() {
            let fid = FaceId(f as u32);
            if merged.face(fid).half_edge == NO_EDGE {
                continue;
            }
            new_faces.push(f as u32);
            for v in merged.face_vertices(fid) {
                new_verts.insert(v.0);
            }
            let start = merged.face(fid).half_edge;
            let mut he = start;
            loop {
                let twin = merged.he(he).twin;
                if twin == core_math::half_edge::NO_TWIN || he.0 <= twin.0 {
                    new_edges.push(he.0);
                }
                he = merged.he(he).next;
                if he == start || he == NO_EDGE {
                    break;
                }
            }
        }
        let new_verts: Vec<u32> = new_verts.into_iter().collect();

        // Always land in Face mode with the FULL island selected so Scale / S has a target
        // (owner 2026-08-07: WP create left empty selection).
        self.transaction("Add to Mesh", |s| {
            // `merge_meshes` is a `from_polygons` REBUILD — the body's own ids are renumbered
            // even though none of its geometry moved, so the sidecar has to be re-keyed here or
            // the existing mesh silently loses (and mis-aims) every authored layer and tag.
            s.poly_install_rebuilt_mesh(nid, merged, &[]);
            s.poly_sel.selected_vertices.clear();
            s.poly_sel.selected_edges.clear();
            s.poly_sel.selected_faces.clear();
            for f in &new_faces {
                s.poly_sel.selected_faces.insert(*f);
            }
            for v in &new_verts {
                s.poly_sel.selected_vertices.insert(*v);
            }
            for e in &new_edges {
                s.poly_sel.selected_edges.insert(*e);
            }
            s.poly_work_mode = PolyWorkMode::Face;
            s.poly_invalidate_pick_cache();
            s.mark_dirty(DirtyFlags::RENDER);
        });
        self.toast(&format!(
            "Added {kind} to mesh — island selected{}",
            if self.poly_workplane_is_active_set() {
                " · Scale ready"
            } else {
                ""
            }
        ));
    }
}

fn op_label(op: BoolOp) -> &'static str {
    match op {
        BoolOp::Union => "Union",
        BoolOp::Difference => "Subtract",
        BoolOp::Intersect => "Intersect",
    }
}

impl AzraApp {
    /// Cut: imprint edge loops where cutter meets target — keep ALL of the target volume,
    /// but add the intersection surface as internal faces (Diff ∪ Intersect of target by
    /// cutter = original A with a seam where the shapes met).
    pub(crate) fn poly_boolean_execute_cut(&mut self) {
        let keep_cutter = self.ui.bool_pie_keep_cutter;
        let plan = match self.poly_boolean_plan() {
            Some(p) => p,
            None => return,
        };

        let run_cut = |target_world_mesh: &HalfEdgeMesh| -> Option<HalfEdgeMesh> {
            let diff = mesh_boolean(target_world_mesh, &plan.cutter_world, BoolOp::Difference)?;
            let inter = mesh_boolean(target_world_mesh, &plan.cutter_world, BoolOp::Intersect)?;
            // Union of (A−B) and (A∩B) reconstructs A with the intersection interface kept
            // as internal faces → edge loops along the contact surface.
            // ID-STABLE (POLY-STALE-IDS): the concatenation numbers from scratch; renumber it onto
            // the target's ids (`transformed` keeps them) so held ids keep naming the same geometry.
            let merged = keep_operand_ids(target_world_mesh, &merge_meshes(&diff, &inter)).0;
            if result_installable(&merged) {
                Some(merged)
            } else {
                None
            }
        };

        let mut installs: Vec<(NodeId, HalfEdgeMesh)> = Vec::new();
        let mut failed: usize = 0;
        for &(nid, world) in &plan.targets {
            let Some(t_mesh) = self.poly_node_mesh(nid) else {
                failed += 1;
                continue;
            };
            let t_world_mesh = transformed(&t_mesh, world);
            match run_cut(&t_world_mesh) {
                Some(res_world) => installs.push((nid, transformed(&res_world, world.inverse()))),
                None => failed += 1,
            }
        }

        if installs.is_empty() {
            self.toast(if failed > 0 {
                "Cut: no target accepted the imprint"
            } else {
                "Cut: nothing to cut"
            });
            return;
        }

        let n_ok = installs.len();
        let delete_cutter_node = match plan.cutter_node {
            Some(nid) if !keep_cutter && !installs.is_empty() => Some(nid),
            _ => None,
        };
        self.transaction("Boolean Cut", |s| {
            for (nid, new_mesh) in installs {
                // CARRY THE SIDECAR (owner 2026-08-11: "smart select after boolean cut still
                // forgets the tag on geo that was not touched"). Cut installs a
                // `merge_meshes(diff, inter)` REBUILD, so every id is new — and this install
                // wrote the mesh alone, leaving `poly_custom_data` keyed by the OLD mesh's ids.
                // That is worse than a clear: the surviving entries land on unrelated edges, so
                // untouched geometry loses its Smart Select tag while strangers gain one.
                // Subtract/Union/Intersect (`poly_boolean_execute`) have carried since
                // BOOL-STALECD; Cut never did.
                //
                // `created` is EMPTY on purpose: the imprint seam is what the user asked this op
                // to draw, and tagging it would stop him beveling the loops he just made. Fail
                // closed — a tag is never invented for geometry whose intent we are guessing at.
                s.poly_install_rebuilt_mesh(nid, new_mesh, &[]);
            }
            if let Some(cn) = delete_cutter_node {
                s.poly_boolean_delete_node(cn);
            }
            s.poly_sel.selected_faces.clear();
            s.poly_sel.selected_edges.clear();
            s.poly_sel.selected_vertices.clear();
            s.poly_invalidate_pick_cache();
            s.mark_dirty(DirtyFlags::RENDER);
        });
        let msg = if failed > 0 {
            format!("Cut: {n_ok} target(s), {failed} refused")
        } else {
            format!("Cut: imprinted {n_ok} target(s)")
        };
        self.toast(&msg);
    }
}

#[cfg(test)]
mod stale_id_tests {
    use crate::{AzraApp, SceneItemId};
    use core_math::half_edge::{FaceId, HalfEdgeId, HalfEdgeMesh, VertexId};
    use core_math::mesh_boolean::BoolOp;
    use core_math::scene_graph::{NodeId, NodePayload};
    use glam::Vec3;

    fn add_body(app: &mut AzraApp, name: &str, mesh: HalfEdgeMesh) -> NodeId {
        let root = app.scene.scene_graph.root();
        app.scene.scene_graph.add_child(
            root,
            NodePayload::PolygonalBody {
                name: name.into(),
                mesh: std::sync::Arc::new(mesh),
                mesh_cache_id: None,
                material: core_math::primitives::PbrMaterial::default(),
                modifiers: Vec::new(),
                source: core_math::primitive_spec::MeshSource::Authored,
            },
        )
    }

    /// POLY-STALE-IDS: the REAL whole-body boolean commit (X-pie Difference), then the frame's
    /// prune. Ids held for geometry the cut never touched — hidden vertex, hover, active vertex +
    /// history, an id-keyed sidecar tag — name the SAME geometry afterwards. The old commit
    /// installed a from-scratch-numbered result, so these ids stayed in range and named strangers.
    #[test]
    fn boolean_commit_keeps_held_ids_on_the_same_geometry() {
        let mut target = HalfEdgeMesh::new_box(Vec3::ONE);
        target.subdivide_faces(&(0..target.faces.len() as u32).collect());
        let mut cutter = HalfEdgeMesh::new_box(Vec3::splat(0.5));
        for v in cutter.vertices.iter_mut() {
            v.position += Vec3::ONE; // takes the +X+Y+Z corner; the -X side is untouched
        }
        cutter.recompute_normals();

        let far = |m: &HalfEdgeMesh, v: VertexId| m.vert(v).position.x < -0.5;
        let far_v: Vec<VertexId> = (0..target.vertices.len() as u32)
            .map(VertexId)
            .filter(|&v| target.is_vert_live(v) && far(&target, v))
            .collect();
        let far_f = (0..target.faces.len() as u32)
            .map(FaceId)
            .find(|&f| target.face_vertices(f).iter().all(|&v| far(&target, v)))
            .expect("a -X face");
        let (hid, hov, act) = (far_v[0], far_v[1], far_v[2]);
        let tagged: HalfEdgeId = target.face_half_edges(far_f)[0];
        let pos_of = |m: &HalfEdgeMesh, v: VertexId| m.vert(v).position;
        let ends = |m: &HalfEdgeMesh, h: HalfEdgeId| (m.vert(m.he_source(h)).position, m.vert(m.he(h).vertex).position);
        let (hid_p, hov_p, act_p, tagged_ends) = (pos_of(&target, hid), pos_of(&target, hov), pos_of(&target, act), ends(&target, tagged));

        let mut app = AzraApp::new_headless();
        app.modeling_context = crate::ModelingContext::Polygonal;
        let t = add_body(&mut app, "Target", target.clone());
        let c = add_body(&mut app, "Cutter", cutter);
        app.select_single_unified(SceneItemId::Node(c));
        app.toggle_select_unified(SceneItemId::Node(t));
        assert_eq!(app.sel.usel.active_node(), Some(t), "target must be active");
        app.poly_sel.hidden_vertices.insert(hid.0);
        app.poly_sel.hovered_vertex = Some(hov.0);
        app.poly_active_vertex = Some(act.0);
        app.poly_vert_history.push(act.0);
        app.cache.poly_custom_data.entry(t).or_default().op_created.set(tagged, true);

        app.poly_boolean_execute(BoolOp::Difference); // ← the real X-pie entry
        app.prune_poly_sel_to_live(); // what the frame runs before the overlay draw

        let m = app.poly_node_mesh(t).expect("target body");
        assert_ne!(m.face_count(), target.face_count(), "the cut must happen");
        assert!(m.validate().is_ok(), "{:?}", m.validate());
        assert!(app.poly_sel.hidden_vertices.contains(&hid.0), "hidden vertex dropped");
        assert_eq!(pos_of(&m, hid), hid_p, "hidden id names a different vertex after the boolean");
        assert_eq!(app.poly_sel.hovered_vertex.map(|v| pos_of(&m, VertexId(v))), Some(hov_p), "hover moved");
        assert_eq!(app.poly_active_vertex.map(|v| pos_of(&m, VertexId(v))), Some(act_p), "active vertex moved");
        assert!(app.poly_vert_history.iter().all(|&v| pos_of(&m, VertexId(v)) == act_p), "history moved");
        let cd = app.cache.poly_custom_data.get(&t).expect("sidecar");
        assert!(cd.op_created.get(tagged), "sidecar tag lost");
        assert_eq!(ends(&m, tagged), tagged_ends, "sidecar tag now sits on a different edge");
    }
}
