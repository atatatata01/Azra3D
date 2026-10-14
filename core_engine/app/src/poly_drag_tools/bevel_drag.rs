// bevel_drag.rs — pure relocation from poly_drag_tools.rs
// Poly mesh bevel modal (Ctrl+B). AZRA_BEVEL_DUMP + BEVEL.md references stay here.

use std::collections::HashSet;
use std::sync::Arc;
use eframe::egui;
use glam::Vec3;
use core_math::half_edge::{VertexId, HalfEdgeId, FaceId, NO_FACE, NO_EDGE, NO_TWIN};
use core_math::scene_graph::NodePayload;
use crate::{
    AzraApp, ModelingContext, PolyWorkMode, PolyTool, DirtyFlags,
    ActiveTool, PolyExtrudeDrag, PolyInsetDrag, PolyBevelDrag, PolyShrinkFattenDrag,
    PolyTransformState, TransformKind, AxisConstraint, FalloffKind,
    world_to_screen,
};

/// Carry a bevel session's sidecar across ONE bevel rebuild.
///
/// MOVED 2026-08-05 to `crate::poly::op_tags::carry_op_tags` — the identical policy is now the
/// shared carry for the bevel, inset and extrude installs, so it lives with the rest of the
/// Smart Select tagging rules instead of inside the bevel tool. Re-exported here because the
/// bevel install and its tests are the original callers.
pub(crate) use crate::poly::op_tags::carry_op_tags as bevel_carry_custom;

/// True when `v` has at least one live half-edge leaving it that bounds a face (a bevelable corner).
/// Matches core `vertex_face_outgoings` non-empty — plane corners pass; wire verts fail.
fn vertex_has_face_corner(mesh: &core_math::half_edge::HalfEdgeMesh, v: VertexId) -> bool {
    for i in 0..mesh.half_edges.len() as u32 {
        let h = HalfEdgeId(i);
        if !mesh.is_edge_live(h) {
            continue;
        }
        if mesh.he(h).face == NO_FACE {
            continue;
        }
        if mesh.he_source(h) == v {
            return true;
        }
    }
    false
}

impl AzraApp {
    // ================================================================
    // Bevel System 1: POLY MESH BEVEL — Blender-style interactive modal tool
    // ================================================================
    // PRIMARY bevel system. Ctrl+B activates modal: drag X=width, drag Y=profile shape,
    // scroll=segments, LMB/Enter=confirm, Esc/RMB=cancel.
    // Regenerates ACTUAL mesh topology via core_math::bevel_op::bevel_edges_offset() on every mouse
    // move (the offset-rail + WeldMap v1 path that superseded the frozen bevel_edges_ex).
    // This is the production bevel that creates real geometry.

    /// Snapshot the runtime sidecar RE-KEYED onto a bevel session's `original_mesh`.
    ///
    /// `original_mesh` is usually the node's current mesh (plain Ctrl+B), but Unbevel rewinds to a
    /// PRE-bevel mesh, whose ids have nothing to do with the sidecar's. Remapping by position makes
    /// both cases correct with one path: positions survive renumbering, ids never do.
    /// Returns `None` when there is no sidecar (nothing authored) — the install then clears the
    /// entry rather than leaving stale keys behind.
    fn poly_bevel_capture_base_custom(
        &self,
        nid: core_math::scene_graph::NodeId,
        original: &core_math::half_edge::HalfEdgeMesh,
    ) -> Option<core_math::poly_mesh::CustomData> {
        let cd = self.cache.poly_custom_data.get(&nid)?;
        let cur = self.poly_node_mesh(nid)?;
        Some(cd.remap_by_position(&cur, original))
    }

    /// Activate interactive bevel. Ctrl+B enters modal bevel mode immediately.
    pub(crate) fn poly_activate_bevel(&mut self) {
        if self.tool.current.is_poly_bevel() {
            self.poly_confirm_bevel();
            return;
        }
        // A live PARAMETRIC primitive has no authored edges to bevel — its rounding is a spec
        // parameter (owner 2026-08-04: "in object mode B lets you change the parametric bevel of
        // the editable object"). Same key, same meaning ("bevel this"), representation chosen by
        // what is selected. Declines for authored meshes and for shapes with no round, so the
        // normal edge bevel below is untouched.
        if self.poly_work_mode == PolyWorkMode::Object && self.primitive_round_modal_begin() {
            return;
        }
        // New bevel session dismisses any prior Adjust Bevel panel (ids/before are stale).
        self.ui.poly_bevel_adjust = None;
        let nid = match self.sel.usel.active_node() { Some(n) => n, None => {
            self.toast("Select a poly object first");
            return;
        }};

        // VERTEX BEVEL: Ctrl+B in Vertex mode truncates/rounds selected vertices (Blender
        // affect=VERTICES) instead of converting them to incident edges. Same modal drag.
        // Empty selection must NOT fall through to the Modo-style all-edge path — that left
        // Vertex mode feeling broken ("I pressed Ctrl+B and the whole mesh edged").
        if self.poly_work_mode == PolyWorkMode::Vertex {
            if self.poly_sel.selected_vertices.is_empty() {
                self.toast("Vertex Bevel: select one or more vertices first");
                return;
            }
            self.poly_activate_vertex_bevel(nid);
            return;
        }

        // Save vertex selection before any domain conversion (vertex→edge).
        let saved_verts = self.poly_sel.selected_vertices.clone();

        // Group mode: faces are the only source of truth. ALWAYS re-derive edges from the
        // currently highlighted faces — never reuse leftover selected_edges from a previous
        // group (owner: "highlight good, action on another group").
        let group_mode = self.poly_work_mode == PolyWorkMode::Group;
        let edges: std::collections::HashSet<u32> = if group_mode {
            let from_faces = self.poly_edges_of_selected_faces();
            // Drop any stale edge/vert sets immediately; tool carries its own edge set.
            self.poly_group_selection_sync();
            if from_faces.is_empty() {
                self.toast("Bevel: select a polygroup first");
                return;
            }
            from_faces
        } else {
            // Auto-switch to Edge mode if needed (Face/Vertex → Edge with conversion).
            if self.poly_work_mode != PolyWorkMode::Edge {
                self.poly_switch_work_mode(PolyWorkMode::Edge);
            }
            // If mode conversion left no edges, select edges connected to saved vertices
            if self.poly_sel.selected_edges.is_empty() && !saved_verts.is_empty() {
                if let Some(node) = self.scene.scene_graph.get(nid) {
                    if let NodePayload::PolygonalBody { mesh, .. } = &node.payload {
                        for i in 0..mesh.half_edges.len() as u32 {
                            let he = mesh.he(HalfEdgeId(i));
                            if he.face == NO_FACE { continue; }
                            if he.twin != NO_TWIN && i > he.twin.0 { continue; }
                            let src = mesh.he_source(HalfEdgeId(i)).0;
                            let dst = he.vertex.0;
                            if saved_verts.contains(&src) || saved_verts.contains(&dst) {
                                self.poly_sel.selected_edges.insert(i);
                            }
                        }
                    }
                }
            }
            // Modo implicit full selection for SAFE bevel only: empty selection bevels EVERY edge.
            // Resolve into the drag's own `edges` set — do NOT write into poly_sel.
            if self.poly_sel.selected_edges.is_empty() {
                self.poly_edges_or_all(nid).into_iter().collect()
            } else {
                self.poly_sel.selected_edges.clone()
            }
        };
        if edges.is_empty() {
            self.toast("No edges available to bevel");
            return;
        }

        let original_mesh = {
            let node = match self.scene.scene_graph.get(nid) { Some(n) => n, None => return };
            match &node.payload {
                NodePayload::PolygonalBody { mesh, .. } => mesh.clone(),
                _ => return,
            }
        };

        // Miter honesty check (fires ONCE on arm, never per drag-frame). TWO selected edges meeting at a
        // re-beveled (degree>3) vertex now miter correctly via build_boundary/offset_meet (STEP 2b in
        // bevel_op.rs). What is still unimplemented is the N-WAY miter: THREE+ selected edges meeting at one
        // re-beveled vertex (Blender's M_ADJ/M_POLY vmesh over 3+ boundverts). The per-frame guard in
        // poly_bevel_regenerate() rejects that if it opens holes; warn once so it doesn't read as "nothing
        // happens". A pristine degree-3 corner miters cleanly and is NOT warned.
        {
            let mut incident: std::collections::HashMap<u32, u32> = std::collections::HashMap::new();
            for &e in &edges {
                let he = original_mesh.he(HalfEdgeId(e));
                if he.face == NO_FACE && he.twin == NO_TWIN { continue; }
                for v in [original_mesh.he_source(HalfEdgeId(e)).0, he.vertex.0] {
                    *incident.entry(v).or_insert(0) += 1;
                }
            }
            let has_nway_miter = incident.iter().any(|(&v, &cnt)| {
                cnt >= 3 && original_mesh.vertex_fan(VertexId(v)).len() > 3
            });
            if has_nway_miter {
                self.toast("3+ edges meet at an already-beveled corner (N-way miter) — bevel them fewer at a time for now");
            }
        }

        // PURE-BOUNDARY selection = honest no-op (not a silent one). A boundary/open-face edge has only
        // one incident face; Blender's own edge bevel is a NO-OP on boundary edges (verified oracle,
        // BEVEL.md §6 #25 — it even crashes the raw bmesh kernel), so AZRA skips them too (STEP 0). If
        // EVERY selected edge is boundary, nothing will change — tell the user why instead of leaving it
        // looking broken. A selection that also contains ANY interior edge bevels fine (those edges), so
        // only warn when the whole selection is boundary. (Owner report: "bevel doesn't work on open faces".)
        {
            let is_boundary = |e: u32| {
                let he = original_mesh.he(HalfEdgeId(e));
                he.twin == NO_TWIN || he.face == NO_FACE || original_mesh.he(he.twin).face == NO_FACE
            };
            if !edges.is_empty() && edges.iter().all(|&e| is_boundary(e)) {
                self.toast("Boundary (open-face) edges can't be beveled — Blender skips them too. Select an interior edge.");
            }
        }

        self.begin_episode("Bevel Edge");

        let start_mouse = self.viewport.viewport_mouse;

        self.set_poly_tool(PolyTool::BevelEdge);
        self.start_operation(ActiveTool::PolyBevel(PolyBevelDrag {
            node_id: nid,
            original_custom: self.poly_bevel_capture_base_custom(nid, &original_mesh),
            original_mesh,
            edges,
            verts: std::collections::HashSet::new(),
            start_mouse,
            width: 0.0,
            width_base: 0.0,
            segments: 3,
            shape: 0.5, // circular fillet — matches Blender's default Superellipse profile (was 1.0 = convex bulge)
            scroll_accum: 0.0,
            clamp: true, // Blender's "Clamp Overlap" ON by default; C toggles it off to go wider
            warned_flat_force: false,
            warned_clamp_force: false,
            warned_install_reject: false,
            consume: false,
            is_unbevel: false,
            ctrl_was_down: false,
            // Seeded as already-down so ARMING the tool can never fire the toggle: if the
            // activation chord itself carries SHIFT (rebindable), frame 1 sees no rising edge.
            shift_was_down: true,
        }));
        // Tool Parameters dock follows the live modal (owner: Ctrl+B → bevel options).
        self.ui2.active_tool = "Bevel".into();
        self.ui2.active_pillar = "poly".into();

        self.toast("Bevel: drag=width, ALT+drag=shape, scroll=segments (>1 = round), SHIFT=consume, C=clamp, LMB/Enter=confirm");
    }

    /// Unbevel / resize bevel (Ctrl+Alt+B).
    ///
    /// **Preferred path (exact):** uses `poly_bevel_history` saved when the last Bevel was confirmed
    /// on this body — restores the pre-bevel mesh and re-enters live Bevel at the same edges/params.
    /// Drag width, scroll segments, width→0 removes. Correct orientation always.
    ///
    /// **Fallback:** topology-guess collapse to a sharp mesh only (no auto re-bevel — that caused
    /// the wrong-orientation pyramid). User can Ctrl+B after if needed.
    pub(crate) fn poly_activate_unbevel(&mut self) {
        if self.tool.current.is_poly_bevel() {
            self.poly_confirm_bevel();
            return;
        }
        if self.modeling_context != ModelingContext::Polygonal
            || self.poly_work_mode == PolyWorkMode::Object
        {
            self.toast("Unbevel: Tab into poly edit mode first");
            return;
        }
        let nid = match self.sel.usel.active_node() {
            Some(n) => n,
            None => {
                self.toast("Unbevel: select a poly object first");
                return;
            }
        };
        if !matches!(
            self.scene.scene_graph.get(nid).map(|n| &n.payload),
            Some(NodePayload::PolygonalBody { .. })
        ) {
            self.toast("Unbevel: active object is not a poly mesh");
            return;
        }

        // ── Exact history path (stack top for this body) ───────────────────
        // Prefer history always when present — topology-guess on multi-seg corner strips
        // was installing collapsed pyramids (owner repro 2026-07-30).
        //
        // STALENESS GATE (owner data-loss repro 2026-08-05): restoring the snapshot is only
        // legal while the live mesh is still EXACTLY the post-bevel result this entry
        // produced. If the user modeled after confirming the bevel, the restore would
        // silently wipe that work. Stale entries are refused, NOT dropped — Ctrl+Z back to
        // the post-bevel state makes them valid again.
        // SCOPING GATE (owner 2026-08-09: "should just affect the selected segment"): the
        // history entry covers the WHOLE edge set the last bevel was run on, so restoring it
        // would resize/remove every one of those bevels — not the one the user picked. An
        // explicit selection is an explicit scope: send it to the reverse-math path, which
        // reconstructs the sharp edge for exactly that strip and leaves the rest baked.
        // No selection = no scope stated → history (exact, and still the better default).
        let scoped = !self.poly_sel.selected_edges.is_empty()
            || !self.poly_sel.selected_faces.is_empty();
        if let Some(hist) = self
            .poly_bevel_history
            .iter()
            .rev()
            .find(|h| h.node_id == nid)
            .filter(|_| !scoped)
            .cloned()
        {
            let live_sig = self
                .scene
                .scene_graph
                .get(nid)
                .and_then(|n| match &n.payload {
                    NodePayload::PolygonalBody { mesh, .. } => Some(mesh.content_signature()),
                    _ => None,
                });
            if live_sig == Some(hist.post_signature) {
                self.poly_activate_unbevel_from_history(hist);
                return;
            }
            // Mesh edited since the bevel → the snapshot is stale. Fall through to the
            // REVERSE-MATH path (owner 2026-08-05: unbevel must work on the baked mesh):
            // it reconstructs the sharp edge from the current geometry, so every edit made
            // after the bevel is kept.
        }

        // ── Polygroup path: no selection → use latest Bevel group faces ───
        // Group mode: ignore leftover edges — faces are the selection (or fill from last bevel group).
        if self.poly_work_mode == PolyWorkMode::Group {
            self.poly_group_selection_sync();
        }
        let mut group_bevel_params: Option<core_math::poly_mesh::PolyGroupBevelParams> = None;
        if self.poly_sel.selected_faces.is_empty() && self.poly_sel.selected_edges.is_empty() {
            if let Some(gid) = self.poly_latest_bevel_group(nid) {
                let faces = self.poly_group_faces(nid, gid);
                if !faces.is_empty() {
                    group_bevel_params = self.poly_group_bevel_params(nid, gid);
                    // Sticky Group: fill face selection for the strip, do not leave Group mode.
                    self.poly_prefer_work_mode(PolyWorkMode::Face);
                    self.poly_sel.selected_faces = faces.into_iter().collect();
                    self.poly_group_selection_sync();
                    self.toast("Unbevel: using last Bevel polygroup faces");
                }
            }
        }

        // ── Fallback: topology guess → sharp only (gated; refuses garbage) ───
        self.poly_activate_unbevel_topology_fallback(nid, group_bevel_params);
    }

    fn poly_activate_unbevel_from_history(&mut self, hist: crate::PolyBevelHistory) {
        let nid = hist.node_id;

        // Validate BEFORE any mutation: drop edge/vert ids that no longer exist on the
        // snapshot (stale half-edge ids after stacked bevels / free-list churn) — an empty
        // set would silently no-op regenerate. Bailing here must leave the live mesh
        // untouched (the old order swapped the mesh first, so this bail REPLACED the live
        // mesh with the stale snapshot — destructive on its own).
        let base = Arc::new((*hist.original_mesh).clone());
        let n_he = base.half_edges.len() as u32;
        let n_v = base.vertices.len() as u32;
        let edges: std::collections::HashSet<u32> =
            hist.edges.iter().copied().filter(|&e| e < n_he).collect();
        let verts: std::collections::HashSet<u32> =
            hist.verts.iter().copied().filter(|&v| v < n_v).collect();
        if edges.is_empty() && verts.is_empty() {
            self.toast(
                "Unbevel: history edges no longer match mesh — confirm bevels with Enter; \
                 Esc and re-bevel if needed",
            );
            return;
        }

        // Clear selection before mesh swap.
        self.poly_sel.selected_faces.clear();
        self.poly_sel.selected_edges.clear();
        self.poly_sel.selected_vertices.clear();
        self.poly_sel.hovered_face = None;
        self.poly_sel.hovered_edge = None;
        self.poly_sel.hovered_vertex = None;
        self.poly_active_vertex = None;

        self.begin_episode("Unbevel / Resize Bevel");

        // Restore deep-cloned pre-bevel geometry as the live base; regenerate re-applies at hist params.
        if let Some(node) = self.scene.scene_graph.get_mut(nid) {
            if let NodePayload::PolygonalBody { mesh, .. } = &mut node.payload {
                *mesh = base.clone();
            }
        }
        self.poly_sync_mesh_cache(nid);

        // Sticky Group: tool stores edges/verts on the drag only — do not pollute poly_sel
        // edge/vert sets while mode stays Group (would desync from face highlight).
        let stay_group = self.poly_work_mode == PolyWorkMode::Group;
        if !hist.verts.is_empty() {
            self.poly_prefer_work_mode(PolyWorkMode::Vertex);
            if !stay_group {
                self.poly_sel.selected_vertices = hist.verts.clone();
            }
        } else {
            self.poly_prefer_work_mode(PolyWorkMode::Edge);
            if !stay_group {
                self.poly_sel.selected_edges = hist.edges.clone();
            }
        }

        let start_mouse = self.viewport.viewport_mouse;
        self.set_poly_tool(PolyTool::BevelEdge);
        // (ids already validated against `base` at the top, before any mutation)
        self.start_operation(ActiveTool::PolyBevel(PolyBevelDrag {
            node_id: nid,
            original_custom: self.poly_bevel_capture_base_custom(nid, &base),
            original_mesh: base,
            edges,
            verts,
            start_mouse,
            width: hist.width,
            width_base: hist.width,
            segments: hist.segments,
            shape: hist.shape,
            scroll_accum: 0.0,
            clamp: hist.clamp,
            warned_flat_force: false,
            warned_clamp_force: false,
            warned_install_reject: false,
            consume: false,
            is_unbevel: true,
            // Chord was Ctrl+Alt+B — treat Ctrl as already down so remove needs a fresh press.
            ctrl_was_down: true,
            // Seeded as already-down so ARMING the tool can never fire the toggle: if the
            // activation chord itself carries SHIFT (rebindable), frame 1 sees no rising edge.
            shift_was_down: true,
        }));
        self.ui2.active_tool = "Unbevel".into();
        self.ui2.active_pillar = "poly".into();
        self.poly_bevel_regenerate();
        self.toast(&format!(
            "Unbevel: drag=width · Ctrl=remove full bevel · scroll=segments · Enter=confirm · Esc=cancel (was {:.3})",
            hist.width
        ));
    }

    fn poly_activate_unbevel_topology_fallback(
        &mut self,
        nid: core_math::scene_graph::NodeId,
        group_params: Option<core_math::poly_mesh::PolyGroupBevelParams>,
    ) {
        let mesh = {
            let node = match self.scene.scene_graph.get(nid) {
                Some(n) => n,
                None => return,
            };
            match &node.payload {
                NodePayload::PolygonalBody { mesh, .. } => mesh.clone(),
                _ => return,
            }
        };

        let result = if !self.poly_sel.selected_faces.is_empty() {
            let faces: std::collections::HashSet<u32> =
                self.poly_sel.selected_faces.iter().copied().collect();
            core_math::unbevel::unbevel_from_faces(&mesh, &faces)
        } else if !self.poly_sel.selected_edges.is_empty() {
            let edges: std::collections::HashSet<u32> =
                self.poly_sel.selected_edges.iter().copied().collect();
            core_math::unbevel::unbevel_from_edges(&mesh, &edges)
        } else {
            self.toast(
                "Unbevel: select the bevel strip (its faces, or an edge on it) — reverse \
                 math reconstructs the sharp edge from that selection",
            );
            return;
        };

        let Some(result) = result else {
            self.toast(
                "Unbevel: reverse math refused — the collapse would break the mesh. \
                 Adjust the strip selection, or Ctrl+Z back to the bevel.",
            );
            return;
        };

        let sharp = Arc::new(result.mesh);
        // Second gate at install site (core already gates; keep app honest if core loosens).
        if sharp.is_manifold().is_err() {
            self.toast("Unbevel: refused non-manifold sharp collapse");
            return;
        }
        let edges = result.sharp_edges;
        if edges.is_empty() {
            self.toast("Unbevel: sharp collapse produced no edges");
            return;
        }

        self.poly_sel.selected_faces.clear();
        self.poly_sel.selected_edges.clear();
        self.poly_sel.selected_vertices.clear();
        self.poly_sel.hovered_face = None;
        self.poly_sel.hovered_edge = None;
        self.poly_sel.hovered_vertex = None;
        self.poly_active_vertex = None;

        self.begin_episode("Unbevel (sharp only)");

        if let Some(node) = self.scene.scene_graph.get_mut(nid) {
            if let NodePayload::PolygonalBody { mesh, .. } = &mut node.payload {
                *mesh = sharp.clone();
            }
        }
        self.poly_sync_mesh_cache(nid);

        // Sticky Group: sharp edges live only on the PolyBevel drag — not in poly_sel, so a
        // later face highlight cannot disagree with the edge set used by the next action.
        self.poly_prefer_work_mode(PolyWorkMode::Edge);
        let n_he = sharp.half_edges.len() as u32;
        let edges: std::collections::HashSet<u32> =
            edges.into_iter().filter(|&e| e < n_he).collect();
        if self.poly_work_mode != PolyWorkMode::Group {
            self.poly_sel.selected_edges = edges.clone();
        } else {
            self.poly_group_selection_sync();
        }

        // Sharp only — do NOT auto re-bevel at full width (wrong-orientation pyramid).
        // Polygroup-stored params restore segments/shape/clamp so a re-drag matches the original profile.
        let (segments, shape, clamp, hint) = match &group_params {
            Some(p) => (
                p.segments.max(1),
                p.shape,
                p.clamp,
                " · params from polygroup",
            ),
            None => (1, 0.5, true, ""),
        };
        self.start_operation(ActiveTool::PolyBevel(PolyBevelDrag {
            node_id: nid,
            original_custom: self.poly_bevel_capture_base_custom(nid, &sharp),
            original_mesh: sharp,
            edges,
            verts: std::collections::HashSet::new(),
            start_mouse: self.viewport.viewport_mouse,
            width: 0.0,
            width_base: 0.0,
            segments,
            shape,
            scroll_accum: 0.0,
            clamp,
            warned_flat_force: false,
            warned_clamp_force: false,
            warned_install_reject: false,
            consume: false,
            is_unbevel: true,
            ctrl_was_down: true,
            // Seeded as already-down so ARMING the tool can never fire the toggle: if the
            // activation chord itself carries SHIFT (rebindable), frame 1 sees no rising edge.
            shift_was_down: true,
        }));
        self.set_poly_tool(PolyTool::BevelEdge);
        self.ui2.active_tool = "Unbevel".into();
        self.ui2.active_pillar = "poly".into();
        self.toast(&format!(
            "Unbevel: sharp only (no session history{hint}). Drag to re-bevel · Ctrl=remove+exit · Esc=cancel."
        ));
    }

    /// Activate a VERTEX bevel modal (Blender affect=VERTICES): the selected vertices are truncated
    /// (seg=1) or rounded (seg>1). Same drag controls as the edge bevel — `poly_bevel_regenerate`
    /// branches on the non-empty `verts` set.
    fn poly_activate_vertex_bevel(&mut self, nid: core_math::scene_graph::NodeId) {
        let original_mesh = {
            let node = match self.scene.scene_graph.get(nid) { Some(n) => n, None => return };
            match &node.payload { NodePayload::PolygonalBody { mesh, .. } => mesh.clone(), _ => return }
        };
        let n_v = original_mesh.vertices.len() as u32;
        // Bevelable = at least one face corner at V. Open plane corners are allowed (core cuts the
        // corner via face re-trace, no 3D cap). Wire/isolated verts (no face) stay filtered out.
        // Do NOT use `vertex_fan().len() < 3` — fan is boundary-incomplete and rejected every
        // plane corner (owner: "not able to bevel a vertex on a plane").
        let mut verts = std::collections::HashSet::new();
        let mut skipped_wire = 0u32;
        for &v in &self.poly_sel.selected_vertices {
            if v >= n_v {
                continue;
            }
            if !vertex_has_face_corner(&original_mesh, VertexId(v)) {
                skipped_wire += 1;
                continue;
            }
            verts.insert(v);
        }
        if verts.is_empty() {
            if skipped_wire > 0 {
                self.toast(
                    "Vertex Bevel: selected verts have no face corner (wire / isolated) — \
                     pick a vertex on a face (plane corners and solid corners both work)",
                );
            } else {
                self.toast("Vertex Bevel: select one or more vertices first");
            }
            return;
        }
        // Seed a visible starting width so arming is not a silent "width=0 until drag" no-op.
        // 15% of the shortest incident edge — under clamp's half-edge cap (0.5×). Drag still
        // overrides from the arm mouse via the absolute width formula (same as edge bevel).
        // Scan face corners (not incomplete boundary fan) so plane verts seed correctly.
        let mut shortest = f32::INFINITY;
        for &v in &verts {
            let vp = original_mesh.vert(VertexId(v)).position;
            for i in 0..original_mesh.half_edges.len() as u32 {
                let h = HalfEdgeId(i);
                if !original_mesh.is_edge_live(h) || original_mesh.he(h).face == NO_FACE {
                    continue;
                }
                if original_mesh.he_source(h) != VertexId(v) {
                    continue;
                }
                let far = original_mesh.vert(original_mesh.he(h).vertex).position;
                let len = (far - vp).length();
                if len > 1e-6 {
                    shortest = shortest.min(len);
                }
                let pe = original_mesh.he(h).prev;
                if pe != NO_EDGE && pe.0 != u32::MAX {
                    let pe_far = original_mesh.vert(original_mesh.he_source(pe)).position;
                    let plen = (pe_far - vp).length();
                    if plen > 1e-6 {
                        shortest = shortest.min(plen);
                    }
                }
            }
        }
        let seed_w = if shortest.is_finite() {
            (shortest * 0.15).max(1e-4)
        } else {
            0.05
        };
        self.begin_episode("Bevel Vertex");
        let start_mouse = self.viewport.viewport_mouse;
        self.set_poly_tool(PolyTool::BevelEdge);
        self.start_operation(ActiveTool::PolyBevel(PolyBevelDrag {
            node_id: nid,
            original_custom: self.poly_bevel_capture_base_custom(nid, &original_mesh),
            original_mesh,
            edges: std::collections::HashSet::new(),
            verts,
            start_mouse,
            width: seed_w,
            width_base: seed_w,
            segments: 3,
            shape: 0.5,
            scroll_accum: 0.0,
            clamp: true,
            warned_flat_force: false,
            warned_clamp_force: false,
            warned_install_reject: false,
            consume: false,
            is_unbevel: false,
            ctrl_was_down: false,
            // Seeded as already-down so ARMING the tool can never fire the toggle: if the
            // activation chord itself carries SHIFT (rebindable), frame 1 sees no rising edge.
            shift_was_down: true,
        }));
        self.ui2.active_tool = "Bevel Vertices".into();
        self.ui2.active_pillar = "poly".into();
        // Install the seed so geometry is visible immediately (width>0.001).
        // width_base = seed so the next frame's poly_update_bevel_drag (dx≈0) keeps it.
        self.poly_bevel_regenerate();
        let skip_note = if skipped_wire > 0 {
            format!(" ({skipped_wire} wire verts skipped)")
        } else {
            String::new()
        };
        self.toast(&format!(
            "Vertex Bevel{skip_note}: drag=width, ALT+drag=shape, scroll=segments (>1 = round), \
             C=clamp, LMB/Enter=confirm"
        ));
    }

    /// Update interactive bevel: recompute mesh from original + current width.
    /// ALT-gate (Blender/Modo feel): mouse-X always drives width; mouse-Y adjusts the profile
    /// shape/tension ONLY while `shape_mod` (ALT) is held. With it up the drag is width-only, so
    /// the shape no longer changes unintentionally while setting width. (This was SHIFT until
    /// 2026-08-09, when SHIFT became the Consume Geometry toggle — owner directive.)
    pub(crate) fn poly_update_bevel_drag(&mut self, mouse: egui::Pos2, shape_mod: bool) {
        let (start, prev_width, width_base, bshape) = {
            let d = match self.tool.current.as_poly_bevel() { Some(d) => d, None => return };
            (d.start_mouse, d.width, d.width_base, d.shape)
        };
        let dx = mouse.x - start.x;
        let dy = mouse.y - start.y;
        // width = base + |dx|*scale. Base is 0 for a normal edge arm (same as the old absolute
        // formula). Base is the seed for vertex bevel so dx=0 on the first post-arm frame does
        // NOT write width=0 and snap the mesh back (2026-08-10 owner repro).
        let scale = self.viewport.camera.distance * 0.002;
        let width = (width_base + dx.abs() * scale).max(0.0);
        // Drag Y sets the profile shape ONLY while ALT is held: 0 = flat chamfer, 0.5 = round
        // fillet, 1 = convex bulge. Without ALT the shape holds at its current value (width-only drag).
        let new_shape = if shape_mod { (0.5 - dy * 0.003).clamp(0.0, 1.0) } else { bshape };
        if (width - prev_width).abs() < 0.0001 && (new_shape - bshape).abs() < 0.01 { return; }
        if let Some(d) = self.tool.current.as_poly_bevel_mut() {
            d.width = width;
            d.shape = new_shape;
        }
        self.poly_bevel_regenerate();
    }

    /// Rebuild the beveled mesh from the ORIGINAL + the current (width, segments, shape) drag state, in
    /// place. THE single producer for both the width/shape drag and the scroll-segments hotkey, so the
    /// round bevel updates live on scroll too. Restores the original mesh when width collapses to ~0.
    pub(crate) fn poly_bevel_regenerate(&mut self) {
        let (nid, original, base_cd, edges, verts, width, segments, shape, clamp, consume) = {
            let d = match self.tool.current.as_poly_bevel() { Some(d) => d, None => return };
            (d.node_id, d.original_mesh.clone(), d.original_custom.clone(), d.edges.clone(), d.verts.clone(), d.width, d.segments, d.shape, d.clamp, d.consume)
        };
        // REPRO CAPTURE (`AZRA_BEVEL_DUMP=1`): write the exact PRE-bevel mesh + selection to
        // %TEMP%/azra_bevel_repro.json, once per drag. Guessing the user's topology from a
        // screenshot sent three oracle runs down the wrong shape (BEVEL.md §11 2026-08-02); this
        // hands the real input to both a Rust gate and the Blender oracle. Off by default.
        if std::env::var_os("AZRA_BEVEL_DUMP").is_some() {
            {
                let path = std::env::temp_dir().join("azra_bevel_repro.json");
                if !path.exists() {
                    let verts_json: Vec<String> = original
                        .vertices
                        .iter()
                        .map(|v| format!("[{:.6},{:.6},{:.6}]", v.position.x, v.position.y, v.position.z))
                        .collect();
                    let faces_json: Vec<String> = (0..original.faces.len() as u32)
                        .map(|f| {
                            let ids: Vec<String> = original
                                .face_vertices(core_math::half_edge::FaceId(f))
                                .iter()
                                .map(|v| v.0.to_string())
                                .collect();
                            format!("[{}]", ids.join(","))
                        })
                        .collect();
                    // Selected edges as vertex-index PAIRS (stable across kernels, unlike he ids).
                    let pairs: Vec<String> = edges
                        .iter()
                        .filter(|&&e| e < original.half_edges.len() as u32)
                        .map(|&e| {
                            let h = core_math::half_edge::HalfEdgeId(e);
                            format!("[{},{}]", original.he_source(h).0, original.he(h).vertex.0)
                        })
                        .collect();
                    let json = format!(
                        "{{\n \"width\": {width},\n \"segments\": {segments},\n \"shape\": {shape},\n \"clamp\": {clamp},\n \"verts\": [{}],\n \"faces\": [{}],\n \"selected_edge_vert_pairs\": [{}]\n}}\n",
                        verts_json.join(","),
                        faces_json.join(","),
                        pairs.join(",")
                    );
                    let _ = std::fs::write(&path, json);
                    eprintln!("[BEVEL] repro dumped: {}", path.display());
                }
            }
        }
        if width > 0.001 {
            // Pure offset-rail + WeldMap path (core_math::bevel_op) — reads the mesh via the public API
            // and rebuilds via from_polygons; half_edge.rs stays frozen. segments>1 rounds the profile
            // (isolated edges; shared corners fall back to a flat, watertight chamfer — v2 blends corners).
            // `clamp` is the user-toggled Clamp Overlap (C key): OFF lets the bevel go wider.
            let params = core_math::bevel_op::BevelParams { width, segments, shape, clamp };
            // Drop stale edge/vert ids (e.g. after unbevel rebuild) so bevel never indexes OOB.
            let n_he = original.half_edges.len() as u32;
            let n_v = original.vertices.len() as u32;
            let edges: std::collections::HashSet<u32> =
                edges.into_iter().filter(|&e| e < n_he).collect();
            let verts: std::collections::HashSet<u32> =
                verts.into_iter().filter(|&v| v < n_v).collect();
            if verts.is_empty() && edges.is_empty() {
                return;
            }
            // AUTHORED EDGE WEIGHT applies to the LIVE bevel too, not only the Bevel modifier.
            // A weight is a percentage of Width (owner 2026-08-11), and Ctrl+B is where most
            // bevels are actually made — wiring it only into the modifier stack left painting
            // weights looking like it did nothing in the common workflow. Weights-only policy:
            // `per_edge_limit: false` so the drag keeps the clamp regime it has today and the
            // weight only ever scales the width down. No painted edges → `None` → the untouched
            // call, bit-identical.
            let weight_policy = {
                let auth = self.poly_authored_channels(nid, &original);
                let mut weights = std::collections::HashMap::new();
                for &e in &edges {
                    let he = HalfEdgeId(e);
                    if let Some(w) = auth.authored_weight_of_edge(&original, he) {
                        weights.insert(e, w);
                        let twin = original.he(he).twin;
                        if twin != NO_TWIN {
                            weights.insert(twin.0, w);
                        }
                    }
                }
                (!weights.is_empty()).then(|| core_math::bevel_op::BevelWidthPolicy {
                    width_min: 0.0,
                    driver: core_math::bevel_op::BevelWidthDriver::Uniform,
                    per_edge_limit: false,
                    weights,
                })
            };
            // VERTEX bevel when the drag carries vertices (affect=VERTICES); else the edge bevel.
            let new_mesh = if !verts.is_empty() {
                core_math::bevel_op::bevel_vertices_offset(&original, &verts, &params)
            } else if consume {
                // CONSUME GEOMETRY (HUD toggle — same semantics as the Bevel modifier's).
                // Preferred path: FUSE (Maya / Offset-Edge-Loop-Cut style) — dissolve rings
                // the fillet covers, CUT the stripe boundary at exactly `width` along each
                // rail, loft the profile between the two boundary loops. The boundaries ARE
                // the bevel rims: full width, zero influence from consumed geometry.
                // Always re-derived from `original` — non-destructive during the drag, so
                // narrowing the width brings consumed rings back; confirm bakes the result.
                // Fallback chain (fail closed): fuse out of scope (open chain / corners /
                // non-quad walls) → v2 absorb + offset bevel → plain clamped bevel.
                if let Some(fused) =
                    core_math::bevel_absorb::consume_fuse_bevel(&original, &edges, &params)
                {
                    // Reset the last-bevel record so HUD width readouts and the Smart
                    // Select strip tagging below don't consume a stale bevel_edges_offset
                    // record (the fuse achieved the width exactly and tags nothing).
                    core_math::bevel_op::record_external_bevel(width, segments);
                    fused
                } else if let Some(cut) =
                    // Boolean-rim topology (retraced n-gons / support edges): sweep a wedge
                    // along the rim and SUBTRACT it — consumes everything in its path
                    // whatever the topology (owner EAT.fl4). Honours segments/shape since
                    // 2026-08-09: the sweep's cross-section IS the consume profile, so this
                    // path rounds exactly like the fuse instead of always cutting flat.
                    core_math::bevel_absorb::consume_wedge_cut(&original, &edges, &params)
                {
                    core_math::bevel_op::record_external_bevel(width, segments);
                    cut
                } else {
                    match core_math::bevel_absorb::absorb_for_bevel(&original, &edges, width) {
                        Some(absorbed) => {
                            let live: std::collections::HashSet<u32> = edges
                                .iter()
                                .copied()
                                .filter(|&e| {
                                    (e as usize) < absorbed.half_edges.len()
                                        && absorbed.is_edge_live(HalfEdgeId(e))
                                })
                                .collect();
                            core_math::bevel_op::bevel_edges_offset_var(
                                &absorbed, &live, &params, weight_policy.as_ref(),
                            )
                        }
                        None => core_math::bevel_op::bevel_edges_offset_var(
                            &original, &edges, &params, weight_policy.as_ref(),
                        ),
                    }
                }
            } else {
                core_math::bevel_op::bevel_edges_offset_var(
                    &original, &edges, &params, weight_policy.as_ref(),
                )
            };
            // Console log (visible in the `cargo run` terminal) — mirrors Blender's op console feedback.
            let boundary_of = |m: &core_math::half_edge::HalfEdgeMesh| (0..m.half_edges.len() as u32)
                .filter(|&i| m.he(HalfEdgeId(i)).twin == NO_TWIN && m.he(HalfEdgeId(i)).face != NO_FACE)
                .count();
            let orig_bnd = boundary_of(&original);
            let bnd = boundary_of(&new_mesh);
            let orig_loops = core_math::bevel_op::boundary_loop_count(&original);
            let new_loops = core_math::bevel_op::boundary_loop_count(&new_mesh);
            let manifold = new_mesh.is_manifold().is_ok();
            // CONSOLE_AUDIT B4: per-step drag logs only under AZRA_PERF_SPAM (13 lines/width drag).
            if crate::perf_spam_on() {
                eprintln!("[BEVEL] edges={} width={:.3} segments={} shape={:.2} clamp={} -> verts {}->{} faces {}->{} boundary={}(loops {}->{}) manifold={}",
                    edges.len(), width, segments, shape, if clamp { "on" } else { "off" },
                    original.vertices.len(), new_mesh.vertices.len(),
                    original.faces.len(), new_mesh.faces.len(), bnd, orig_loops, new_loops, manifold);
            }
            // Never install a broken result. THE guard is boundary LOOP count, not boundary EDGE count:
            // a legitimate bevel of an interior edge whose endpoint is on an OPEN rim RESHAPES the rim,
            // so its boundary EDGE count grows (Blender's output does the same) — the old `bnd <= orig_bnd`
            // form wrongly rejected every open-mesh bevel that touched the boundary ("bevel doesn't work on
            // open faces"). `bevel_result_installable` = is_manifold (accepts clean OPEN meshes, catches
            // non-manifold/bowtie tears) AND no NEW boundary loop (a spurious hole). Closed meshes are
            // unchanged (loops 0 iff no boundary edges). Keep the last good frame; the next drag retries.
            // (BEVEL.md §8 guard, adapted for open meshes 2026-07-17.)
            let ok = core_math::bevel_op::bevel_result_installable(&original, &new_mesh);
            if ok {
                // TAG THE SEGMENT EDGES THIS BEVEL CREATED (owner 2026-08-04: "all those
                // segments created with bevel should be tagged so the bevel modifier does not
                // bevel those edges"). `last_bevel_new_face_range` is the bevel's own record
                // of the strips/caps/seals it built, and on this path the full rebuild uses
                // `from_polygons`, which preserves face order -- so the range indexes the
                // installed mesh directly. Written into the sidecar so it OUTLIVES the
                // operation: the owner bevels destructively and adds a Bevel modifier later.
                // ONLY THE ROUND'S INTERIOR EDGES (owner 2026-08-04: "we need to keep side
                // edge from bevel, only the round segment"). An edge with a created face on
                // BOTH sides runs ALONG the round -- between two segments -- and re-beveling
                // it would flatten the round. An edge with a created face on ONE side is the
                // RIM where the round meets the original surface: a real corner the user
                // still wants beveled. Same rule the cutter case uses, so the two agree.
                // THE STRIP, NOT THE CORNER FILL (owner 2026-08-04). Every edge of the band
                // running ALONG the beveled edge is tangent to the round -- both the segment
                // edges inside it and its two long RIMS where it meets the original faces
                // ("those 2 too") -- so all of them are tagged. The CORNER FILL at the ends
                // is excluded: its boundary edges are the "side edges" that must keep
                // beveling. `last_bevel_strip_range` is the bevel's own strip/cap split.
                let created: Vec<HalfEdgeId> =
                    match core_math::bevel_op::last_bevel_strip_range() {
                        Some((f0, f1)) => {
                            // INTERIOR OF THE STRIP ONLY (owner 2026-08-05: "bevel still
                            // fails, all those segments need to bevel", marking the profile
                            // arcs at each END of the round). Tagging every edge of a strip
                            // face swept those ends in - but they are real CREASES where the
                            // round meets the side face, and they must bevel.
                            //
                            // Tagging the strip boundary is also unnecessary: the round's
                            // interior segments AND its long rims are all tangent-continuous,
                            // so the geometric rule already skips them. Only the ends are
                            // creases, and geometry correctly leaves those beveled. So tag
                            // strictly what geometry cannot see - strip-to-strip edges.
                            let hi = f1.min(new_mesh.faces.len() as u32);
                            let in_strip = |f: core_math::half_edge::FaceId| {
                                f.0 >= f0 && f.0 < hi
                            };
                            let mut v = Vec::new();
                            for f in f0..hi {
                                let fid = core_math::half_edge::FaceId(f);
                                if new_mesh.face(fid).half_edge == NO_EDGE { continue; }
                                for he in new_mesh.face_half_edges(fid) {
                                    let tw = new_mesh.he(he).twin;
                                    if tw == NO_TWIN { continue; }
                                    let tf = new_mesh.he(tw).face;
                                    if tf != NO_FACE && in_strip(tf) {
                                        v.push(he);
                                    }
                                }
                            }
                            v
                        }
                        None => Vec::new(),
                    };
                // CARRY THE SIDECAR ACROSS THE RENUMBERING (same contract as the boolean install,
                // `poly/boolean.rs`). The rebuild invalidates every id, so the sidecar must be
                // re-keyed from the DRAG-START base — never accumulated onto whatever the previous
                // frame (or a previous bevel session) left behind. Without this, a second bevel's
                // install left the first bevel's `op_created` bits pointing at unrelated edges:
                // the old tags were lost AND fresh edges were mis-tagged, which is the owner's
                // "bevel another edge first and the tags break". Positions survive; ids do not.
                let remapped =
                    bevel_carry_custom(base_cd.as_ref(), &original, &new_mesh, &created);
                if let Some(node) = self.scene.scene_graph.get_mut(nid) {
                    if let NodePayload::PolygonalBody { mesh, .. } = &mut node.payload {
                        *mesh = Arc::new(new_mesh);
                    }
                }
                match remapped {
                    Some(cd) => { self.cache.poly_custom_data.insert(nid, cd); }
                    None => { self.cache.poly_custom_data.remove(&nid); }
                }
                // Round blocked: multi-edge / bevel-on-beveled corners still force seg→1 (BEVEL.md).
                // Toast once per drag so "scroll does nothing" is not a silent mystery.
                if segments > 1 && core_math::bevel_op::last_bevel_forced_flat() {
                    let warn = if let Some(d) = self.tool.current.as_poly_bevel_mut() {
                        if !d.warned_flat_force {
                            d.warned_flat_force = true;
                            true
                        } else {
                            false
                        }
                    } else {
                        false
                    };
                    if warn {
                        self.toast(
                            "Round clamped to flat chamfer — this corner topology can't round yet \
                             (bevel fewer edges at once, or bevel clean corners first)",
                        );
                    }
                }
                // Clamp Overlap is binding: further mouse drag raises HUD width but geometry stays
                // stuck (thin walls / short faces — common on channels). Toast once; C disables clamp.
                if clamp && core_math::bevel_op::last_bevel_width_was_clamped() {
                    let warn = if let Some(d) = self.tool.current.as_poly_bevel_mut() {
                        if !d.warned_clamp_force {
                            d.warned_clamp_force = true;
                            true
                        } else {
                            false
                        }
                    } else {
                        false
                    };
                    if warn {
                        let eff = core_math::bevel_op::last_bevel_effective_width();
                        self.toast(&format!(
                            "Width clamped at {eff:.3} (Clamp Overlap ON) — press C to go wider, \
                             or drag less on thin faces"
                        ));
                    }
                }
            } else {
                eprintln!("[BEVEL] REJECTED (kept previous frame): manifold={manifold} loops {orig_loops}->{new_loops} boundary {orig_bnd}->{bnd}");
                // Owner: "nothing happens" while dragging — the guard keeps the last good frame
                // with only a console line. Toast once so vertex/edge bevel rejects are visible.
                let warn = if let Some(d) = self.tool.current.as_poly_bevel_mut() {
                    if !d.warned_install_reject {
                        d.warned_install_reject = true;
                        true
                    } else {
                        false
                    }
                } else {
                    false
                };
                if warn {
                    let kind = if !verts.is_empty() { "vertex" } else { "edge" };
                    self.toast(&format!(
                        "Bevel {kind} rejected (tear / new hole) — kept previous frame. \
                         Try a smaller width, fewer verts/edges, or Clamp ON"
                    ));
                }
            }
        } else {
            // Width collapsed to ~0: the mesh reverts to the drag-start snapshot, so the sidecar
            // must revert with it — the ids in the cache belong to the last installed bevel.
            if let Some(node) = self.scene.scene_graph.get_mut(nid) {
                if let NodePayload::PolygonalBody { mesh, .. } = &mut node.payload {
                    *mesh = original;
                }
            }
            match base_cd {
                Some(cd) => { self.cache.poly_custom_data.insert(nid, cd); }
                None => { self.cache.poly_custom_data.remove(&nid); }
            }
        }
        self.poly_sync_mesh_cache(nid); // re-tessellate LIVE so the surface follows the edit each frame
        self.mark_dirty(DirtyFlags::RENDER);
    }

    /// Adjust bevel segment count via scroll wheel (contextual: only fires while the bevel tool owns the
    /// drag — see the `is_poly_bevel()` branch in the scroll dispatch). Rebuilds the mesh live when the
    /// count actually changes so the round updates on scroll, not only on the next mouse move.
    pub(crate) fn poly_bevel_scroll_segments(&mut self, delta: f32) {
        let changed = if let Some(d) = self.tool.current.as_poly_bevel_mut() {
            let before = d.segments;
            d.scroll_accum += delta;
            let threshold = 40.0; // ~1 wheel notch per segment (was 120 — too sluggish)
            while d.scroll_accum > threshold {
                d.scroll_accum -= threshold;
                d.segments = (d.segments + 1).min(32);
            }
            while d.scroll_accum < -threshold {
                d.scroll_accum += threshold;
                d.segments = d.segments.saturating_sub(1).max(1);
            }
            d.segments != before
        } else { false };
        if changed { self.poly_bevel_regenerate(); }
    }

    /// Toggle Clamp Overlap (C key / HUD checkbox). Flips the flag on the active bevel drag and
    /// rebuilds via the single producer so the width limit updates live. With clamp OFF the user can
    /// bevel wider than the geometry would otherwise allow (Blender's "Clamp Overlap").
    pub(crate) fn poly_bevel_toggle_clamp(&mut self) {
        let new_state = if let Some(d) = self.tool.current.as_poly_bevel_mut() {
            d.clamp = !d.clamp;
            Some(d.clamp)
        } else { None };
        if let Some(on) = new_state {
            self.poly_bevel_regenerate();
            self.toast(if on { "Bevel: Clamp Overlap ON" } else { "Bevel: Clamp Overlap OFF (wider)" });
        }
    }

    /// Toggle Consume Geometry (HUD checkbox) — CAD-style: the bevel eats blocking edge loops
    /// instead of sticking at the clamp. Same single-producer shape as `poly_bevel_toggle_clamp`.
    pub(crate) fn poly_bevel_toggle_consume(&mut self) {
        let new_state = if let Some(d) = self.tool.current.as_poly_bevel_mut() {
            d.consume = !d.consume;
            Some(d.consume)
        } else { None };
        if let Some(on) = new_state {
            self.poly_bevel_regenerate();
            self.toast(if on {
                "Bevel: Consume Geometry ON (eats blocking loops — quad rows only)"
            } else {
                "Bevel: Consume Geometry OFF"
            });
        }
    }

    /// Set the exact bevel width from the HUD numeric field. Routes through the single producer.
    /// Clamped to >= 0; the regenerate restores the original mesh when width collapses to ~0.
    pub(crate) fn poly_bevel_set_width(&mut self, width: f32) {
        let changed = if let Some(d) = self.tool.current.as_poly_bevel_mut() {
            let w = width.max(0.0);
            let ch = (w - d.width).abs() > 1e-6;
            d.width = w;
            // Re-anchor drag: further mouse motion adds on top of the HUD value, not the old seed.
            d.width_base = w;
            d.start_mouse = self.viewport.viewport_mouse;
            ch
        } else { false };
        if changed { self.poly_bevel_regenerate(); }
    }

    /// Set the exact segment count from the HUD numeric field (1..=32). Routes through the single producer.
    pub(crate) fn poly_bevel_set_segments(&mut self, segments: u32) {
        let changed = if let Some(d) = self.tool.current.as_poly_bevel_mut() {
            let s = segments.clamp(1, 32);
            let ch = s != d.segments;
            d.segments = s;
            ch
        } else { false };
        if changed { self.poly_bevel_regenerate(); }
    }

    /// Set the exact profile shape from the HUD numeric field (0..=1). Routes through the single producer.
    pub(crate) fn poly_bevel_set_shape(&mut self, shape: f32) {
        let changed = if let Some(d) = self.tool.current.as_poly_bevel_mut() {
            let s = shape.clamp(0.0, 1.0);
            let ch = (s - d.shape).abs() > 1e-6;
            d.shape = s;
            ch
        } else { false };
        if changed { self.poly_bevel_regenerate(); }
    }

    /// Unbevel session: Ctrl tap removes the full bevel (restore pre-bevel mesh) and exits the tool.
    pub(crate) fn poly_unbevel_remove_full_and_exit(&mut self) {
        let Some(d) = self.tool.current.as_poly_bevel() else {
            return;
        };
        if !d.is_unbevel {
            return;
        }
        let nid = d.node_id;
        let original = d.original_mesh.clone();
        let base_cd = d.original_custom.clone();

        // Install sharp / pre-bevel mesh.
        if let Some(node) = self.scene.scene_graph.get_mut(nid) {
            if let NodePayload::PolygonalBody { mesh, .. } = &mut node.payload {
                *mesh = original;
            }
        }
        // Sidecar follows the mesh it belongs to (see `poly_cancel_bevel_drag`).
        match base_cd {
            Some(cd) => { self.cache.poly_custom_data.insert(nid, cd); }
            None => { self.cache.poly_custom_data.remove(&nid); }
        }
        // Pop the stack entry we just fully removed (this body, top-most match).
        if let Some(i) = self
            .poly_bevel_history
            .iter()
            .rposition(|h| h.node_id == nid)
        {
            self.poly_bevel_history.remove(i);
        }

        self.commit_episode();

        self.poly_sel.selected_edges.clear();
        self.poly_sel.selected_faces.clear();
        self.poly_sel.selected_vertices.clear();
        self.poly_sel.hovered_edge = None;
        self.poly_sel.hovered_vertex = None;
        self.poly_sel.hovered_face = None;
        self.bevel_pv.active = false;
        self.bevel_pv.stored_edges.clear();
        self.bevel_pv.edge_count = 0;

        self.poly_sync_mesh_cache(nid);
        self.tool.current = ActiveTool::Idle;
        self.poly_tool = PolyTool::Idle;
        self.mark_dirty(DirtyFlags::RENDER);
        self.toast("Bevel removed");
    }

    /// Confirm bevel — keep current mesh state, clean up tool, arm Adjust Bevel redo panel.
    pub(crate) fn poly_confirm_bevel(&mut self) {
        let nid = if let Some(d) = self.tool.current.as_poly_bevel() { d.node_id } else { return };
        let width = if let Some(d) = self.tool.current.as_poly_bevel() { d.width } else { 0.0 };
        // [Codex P2] Detect a no-op bevel (all selected edges were boundary / all verts skipped →
        // core returned the mesh unchanged) so we don't claim "Bevel applied" where nothing happened.
        let (no_op, is_vert) = if let Some(d) = self.tool.current.as_poly_bevel() {
            let is_vert = !d.verts.is_empty();
            let no_op = self.scene.scene_graph.get(nid).and_then(|n| match &n.payload {
                NodePayload::PolygonalBody { mesh, .. } => Some(mesh.vertices.len() == d.original_mesh.vertices.len()),
                _ => None,
            }).unwrap_or(false);
            (no_op, is_vert)
        } else { (false, false) };

        if width < 0.001 {
            self.poly_cancel_bevel_drag();
            self.toast("Bevel cancelled (zero width)");
            return;
        }
        if no_op {
            self.poly_cancel_bevel_drag();
            self.toast(if is_vert {
                "No bevelable vertices (wire/isolated verts have no face corner)"
            } else {
                "No bevelable edges (boundary/open edges are skipped)"
            });
            return;
        }

        // Post-bevel mesh signature — the staleness gate for Unbevel history AND the adjust
        // panel: both restore/re-run from the pre-bevel snapshot, which is only legal while
        // the live mesh is still exactly this result. 0 (node lookup failed) fails closed.
        let post_sig = self
            .tool
            .current
            .as_poly_bevel()
            .and_then(|d| self.scene.scene_graph.get(d.node_id))
            .and_then(|n| match &n.payload {
                NodePayload::PolygonalBody { mesh, .. } => Some(mesh.content_signature()),
                _ => None,
            })
            .unwrap_or(0);

        // Seed Blender-style redo panel BEFORE clearing the tool (inset pattern) so width /
        // segments / shape stay adjustable without stacking (always re-run from `before`).
        let adjust = self.tool.current.as_poly_bevel().map(|d| crate::types::PolyBevelAdjust {
            nid: d.node_id,
            before: d.original_mesh.clone(),
            edges: d.edges.clone(),
            verts: d.verts.clone(),
            width: d.width,
            segments: d.segments,
            shape: d.shape,
            clamp: d.clamp,
            needs_settle: false,
            post_signature: post_sig,
        });
        // Push / replace exact pre-bevel snapshot for Unbevel (stack — stacked bevels peel LIFO).
        let (bw, bseg, bshape, bclamp) = if let Some(d) = self.tool.current.as_poly_bevel() {
            // Deep-clone mesh so later Arc::make_mut on the scene never mutates history.
            let original_owned = (*d.original_mesh).clone();
            let entry = crate::PolyBevelHistory {
                node_id: d.node_id,
                original_mesh: Arc::new(original_owned),
                edges: d.edges.clone(),
                verts: d.verts.clone(),
                width: d.width,
                segments: d.segments,
                shape: d.shape,
                clamp: d.clamp,
                post_signature: post_sig,
            };
            // Re-confirm of an Unbevel session replaces the stack top for this body
            // (don't stack a duplicate of the same peel). Fresh bevel always pushes.
            if d.is_unbevel {
                if let Some(i) = self
                    .poly_bevel_history
                    .iter()
                    .rposition(|h| h.node_id == d.node_id)
                {
                    self.poly_bevel_history[i] = entry;
                } else {
                    self.poly_bevel_history.push(entry);
                }
            } else {
                self.poly_bevel_history.push(entry);
            }
            // Cap stack so long sessions don't retain every mesh forever.
            const MAX_BEVEL_HISTORY: usize = 16;
            if self.poly_bevel_history.len() > MAX_BEVEL_HISTORY {
                let drop_n = self.poly_bevel_history.len() - MAX_BEVEL_HISTORY;
                self.poly_bevel_history.drain(0..drop_n);
            }
            (d.width, d.segments, d.shape, d.clamp)
        } else {
            (width, 1, 0.5, true)
        };

        self.commit_episode();

        self.poly_sel.selected_edges.clear();
        self.poly_sel.selected_faces.clear();
        self.poly_sel.selected_vertices.clear();
        self.poly_active_vertex = None;
        // Also drop the hover highlight so the just-beveled edge doesn't stay lit (Blender deselects
        // after the op — the owner's report). Hover re-arms on the next mouse move as normal.
        self.poly_sel.hovered_edge = None;
        self.poly_sel.hovered_vertex = None;
        self.poly_sel.hovered_face = None;
        // Group mode remains Group after confirm — selection is empty until the user picks again.
        self.poly_group_selection_sync();

        self.bevel_pv.active = false;
        self.bevel_pv.stored_edges.clear();
        self.bevel_pv.edge_count = 0;

        self.poly_sync_mesh_cache(nid);
        // AUTO_POLYGROUPS: tag new strip/cap faces + persist tool params on group meta.
        self.poly_auto_group_last_bevel(nid, bw, bseg, bshape, bclamp);
        self.tool.current = ActiveTool::Idle;
        self.poly_tool = PolyTool::Idle;
        self.ui.poly_bevel_adjust = adjust;
        self.mark_dirty(DirtyFlags::RENDER);

        let w_str = format!("{:.3}", width);
        self.toast(&format!(
            "Bevel applied (width: {w_str}) — polygroup saved · Unbevel Ctrl+Alt+B"
        ));
    }

    /// DEBUG: export the active polygonal mesh to `<Desktop>/azra_bevel_debug.obj` (Shift+O) so its exact
    /// vertices/faces can be loaded into Blender and cross-referenced. No hardcoded path — uses %USERPROFILE%.
    pub(crate) fn poly_export_debug_obj(&mut self) {
        let nid = match self.sel.usel.active_node() { Some(n) => n, None => { self.toast("No active object"); return; } };
        let mesh = match self.scene.scene_graph.get(nid).map(|n| &n.payload) {
            Some(NodePayload::PolygonalBody { mesh, .. }) => mesh.clone(),
            _ => { self.toast("Active object is not a poly mesh"); return; }
        };
        let mut s = String::from("# AZRA poly debug export\no azra_debug\n");
        for v in &mesh.vertices {
            s.push_str(&format!("v {:.6} {:.6} {:.6}\n", v.position.x, v.position.y, v.position.z));
        }
        for fi in 0..mesh.faces.len() as u32 {
            let fv = mesh.face_vertices(FaceId(fi));
            if fv.len() < 3 { continue; }
            s.push('f');
            for x in &fv { s.push_str(&format!(" {}", x.0 + 1)); }
            s.push('\n');
        }
        let path = std::env::var("USERPROFILE")
            .map(|p| format!("{p}\\Desktop\\azra_bevel_debug.obj"))
            .unwrap_or_else(|_| "azra_bevel_debug.obj".into());
        match std::fs::write(&path, s) {
            Ok(_) => self.toast(&format!("Exported {} verts to {}", mesh.vertices.len(), path)),
            Err(e) => self.toast(&format!("Export failed: {e}")),
        }
    }

    /// Cancel interactive bevel — restore original mesh directly (no undo pop).
    pub(crate) fn poly_cancel_bevel_drag(&mut self) {
        let (nid, original_mesh, base_cd) = match self.tool.current.as_poly_bevel() {
            Some(d) => (d.node_id, d.original_mesh.clone(), d.original_custom.clone()),
            None => {
                self.tool.current = ActiveTool::Idle;
                self.poly_tool = PolyTool::Idle;
                return;
            }
        };

        // Restore original mesh directly — safer than popping global undo
        if let Some(node) = self.scene.scene_graph.get_mut(nid) {
            if let NodePayload::PolygonalBody { mesh, .. } = &mut node.payload {
                *mesh = original_mesh;
            }
        }
        // The sidecar is keyed by the ids of whatever the last frame installed; restoring the mesh
        // without restoring it would leave stale keys on a mesh that never had them.
        match base_cd {
            Some(cd) => { self.cache.poly_custom_data.insert(nid, cd); }
            None => { self.cache.poly_custom_data.remove(&nid); }
        }

        // Discard the episode snapshot since we're cancelling
        self.cancel_episode();

        self.bevel_pv.active = false;
        self.bevel_pv.stored_edges.clear();
        self.bevel_pv.edge_count = 0;
        self.tool.current = ActiveTool::Idle;
        self.poly_tool = PolyTool::Idle;
        self.poly_sync_mesh_cache(nid);
        self.mark_dirty(DirtyFlags::RENDER);
    }
}

#[cfg(test)]
mod bevel_sidecar_tests {
    //! Regression: Smart Select tags must survive a SECOND bevel.
    //!
    //! Owner report (2026-08-05): "tags for smart select are not persistent when I bevel another
    //! edge first ... it breaks tags from a previous bevel." The tags live in `CustomData.op_created`,
    //! keyed by `HalfEdgeId`; a bevel rebuilds the mesh and renumbers every id, so the first bevel's
    //! tags land on unrelated edges of the second bevel's mesh unless the sidecar is re-keyed by
    //! POSITION at install time. These tests exercise `bevel_carry_custom` — the exact function the
    //! install calls — not a re-implementation of it.
    use super::bevel_carry_custom;
    use core_math::half_edge::{FaceId, HalfEdgeId, HalfEdgeMesh, NO_EDGE};
    use core_math::poly_mesh::CustomData;
    use glam::Vec3;
    use std::collections::HashSet;

    fn params(width: f32) -> core_math::bevel_op::BevelParams {
        core_math::bevel_op::BevelParams { width, segments: 3, shape: 0.5, clamp: true }
    }

    /// The install's own "which edges did this bevel create" rule (strip range → its half-edges).
    fn strip_edges(new_mesh: &HalfEdgeMesh) -> Vec<HalfEdgeId> {
        let Some((f0, f1)) = core_math::bevel_op::last_bevel_strip_range() else { return Vec::new() };
        let hi = f1.min(new_mesh.faces.len() as u32);
        let mut v = Vec::new();
        for f in f0..hi {
            let fid = FaceId(f);
            if new_mesh.face(fid).half_edge == NO_EDGE { continue; }
            v.extend(new_mesh.face_half_edges(fid));
        }
        v
    }

    /// Quantized (source, destination) position key — the identity that survives renumbering.
    fn pos_key(m: &HalfEdgeMesh, h: HalfEdgeId) -> ((i64, i64, i64), (i64, i64, i64)) {
        let q = |p: Vec3| {
            ((p.x * 1e5).round() as i64, (p.y * 1e5).round() as i64, (p.z * 1e5).round() as i64)
        };
        (q(m.vert(m.he_source(h)).position), q(m.vert(m.he(h).vertex).position))
    }

    fn tagged_positions(m: &HalfEdgeMesh, cd: &CustomData) -> HashSet<((i64, i64, i64), (i64, i64, i64))> {
        cd.op_created.iter().filter(|&&h| m.is_edge_live(h)).map(|&h| pos_key(m, h)).collect()
    }

    /// Pick two edges of the cube that do NOT share a vertex, so bevel B cannot legitimately
    /// consume bevel A's geometry.
    fn two_disjoint_edges(m: &HalfEdgeMesh) -> (u32, u32) {
        let a = 0u32;
        let ah = HalfEdgeId(a);
        let (a0, a1) = (m.he_source(ah).0, m.he(ah).vertex.0);
        for i in 1..m.half_edges.len() as u32 {
            let h = HalfEdgeId(i);
            if !m.is_edge_live(h) { continue; }
            let (b0, b1) = (m.he_source(h).0, m.he(h).vertex.0);
            if b0 != a0 && b0 != a1 && b1 != a0 && b1 != a1 {
                return (a, i);
            }
        }
        panic!("no disjoint edge pair on a cube");
    }

    /// Run bevel A, then bevel B, and return (mesh after A, sidecar after A, mesh after B, sidecar after B).
    fn two_bevels() -> (HalfEdgeMesh, CustomData, HalfEdgeMesh, CustomData) {
        let cube = HalfEdgeMesh::new_box(Vec3::ONE);
        let (ea, eb) = two_disjoint_edges(&cube);

        let sel_a: HashSet<u32> = [ea].into_iter().collect();
        let mesh_a = core_math::bevel_op::bevel_edges_offset(&cube, &sel_a, &params(0.15));
        let created_a = strip_edges(&mesh_a);
        assert!(!created_a.is_empty(), "bevel A created no strip faces — test setup is wrong");
        let cd_a = bevel_carry_custom(None, &cube, &mesh_a, &created_a)
            .expect("bevel A must produce a sidecar");

        // Bevel B targets an edge of the ORIGINAL cube that bevel A did not touch. Locate it in
        // mesh_a by position (its id was renumbered).
        let want = pos_key(&cube, HalfEdgeId(eb));
        let eb_in_a = (0..mesh_a.half_edges.len() as u32)
            .map(HalfEdgeId)
            .find(|&h| mesh_a.is_edge_live(h) && pos_key(&mesh_a, h) == want)
            .expect("bevel B's edge must survive bevel A untouched");

        let sel_b: HashSet<u32> = [eb_in_a.0].into_iter().collect();
        let mesh_b = core_math::bevel_op::bevel_edges_offset(&mesh_a, &sel_b, &params(0.15));
        let created_b = strip_edges(&mesh_b);
        let cd_b = bevel_carry_custom(Some(&cd_a), &mesh_a, &mesh_b, &created_b)
            .expect("bevel B must produce a sidecar");
        (mesh_a, cd_a, mesh_b, cd_b)
    }

    #[test]
    fn second_bevel_preserves_first_bevels_tags_by_position() {
        let (mesh_a, cd_a, mesh_b, cd_b) = two_bevels();
        let before = tagged_positions(&mesh_a, &cd_a);
        let after = tagged_positions(&mesh_b, &cd_b);
        assert!(!before.is_empty());

        // Every edge bevel A tagged that still EXISTS in mesh_b (bevel B was on a disjoint edge, so
        // essentially all of them do) must still be tagged — by position, not by id.
        let live_b: HashSet<_> = (0..mesh_b.half_edges.len() as u32)
            .map(HalfEdgeId)
            .filter(|&h| mesh_b.is_edge_live(h))
            .map(|h| pos_key(&mesh_b, h))
            .collect();
        let survivors: Vec<_> = before.iter().filter(|k| live_b.contains(k)).collect();
        assert!(
            survivors.len() * 10 >= before.len() * 9,
            "bevel B destroyed bevel A's geometry: {}/{} survive",
            survivors.len(),
            before.len()
        );
        let lost: Vec<_> = survivors.iter().filter(|k| !after.contains(**k)).collect();
        assert!(
            lost.is_empty(),
            "first bevel's tags were lost after a second bevel: {} of {} edges untagged \
             (before={}, after={})",
            lost.len(),
            survivors.len(),
            before.len(),
            after.len()
        );
    }

    /// The bug, pinned: carrying the sidecar BY ID (the pre-fix install behaviour) both loses the
    /// first bevel's tags and mis-tags unrelated edges. If this ever stops failing, the ids became
    /// stable and `bevel_carry_custom`'s remap could be revisited.
    #[test]
    fn carrying_tags_by_id_is_wrong() {
        let (mesh_a, cd_a, mesh_b, cd_b) = two_bevels();
        let before = tagged_positions(&mesh_a, &cd_a);

        // Pre-fix behaviour: keep cd_a's raw ids, stamp the new strip on top.
        let mut naive = cd_a.clone();
        for &h in &strip_edges(&mesh_b) {
            naive.op_created.set(h, true);
        }
        let naive_pos = tagged_positions(&mesh_b, &naive);
        let fixed_pos = tagged_positions(&mesh_b, &cd_b);

        assert_ne!(
            naive_pos, fixed_pos,
            "id-carry and position-carry agree — the renumbering this fix exists for did not happen"
        );
        let lost: Vec<_> = before.iter().filter(|k| !naive_pos.contains(*k)).collect();
        assert!(
            !lost.is_empty(),
            "id-carry did not lose any of bevel A's tags — expected staleness"
        );
    }
}
