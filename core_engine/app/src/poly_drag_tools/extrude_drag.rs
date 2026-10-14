// extrude_drag.rs — pure relocation from poly_drag_tools.rs
// Extrude drag (E / Alt+E / edge / vertex).

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

/// Mesh → `(positions, face rings)` soup for the boolean repro dumps.
///
/// INDEX-PRESERVING (2026-08-04): renumbering in face-visit order fed the kernel a
/// RE-INDEXED variant on replay, and an exact boolean's tessellation depends on input
/// ordering — so replays exercised an easier case than the app. Keep the mesh's own
/// vertex order.
fn repro_mesh_soup(m: &core_math::half_edge::HalfEdgeMesh) -> (Vec<[f32; 3]>, Vec<Vec<usize>>) {
    let mut verts: Vec<[f32; 3]> = Vec::new();
    let mut remap = std::collections::HashMap::new();
    for i in 0..m.vertices.len() as u32 {
        let v = VertexId(i);
        if m.vert(v).half_edge == NO_EDGE {
            continue;
        }
        let p = m.vert(v).position;
        remap.insert(i, verts.len());
        verts.push([p.x, p.y, p.z]);
    }
    let mut faces: Vec<Vec<usize>> = Vec::new();
    for i in 0..m.faces.len() as u32 {
        let fid = FaceId(i);
        if m.face(fid).half_edge == NO_EDGE {
            continue;
        }
        faces.push(
            m.face_vertices(fid)
                .iter()
                .filter_map(|v| remap.get(&v.0).copied())
                .collect(),
        );
    }
    (verts, faces)
}

/// Write a boolean-input repro pair to `%TEMP%/<name>`.
///
/// TWO distinct files by design. `azra_extrude_repro.json` rolls on every through-cut;
/// `azra_extrude_repro_FAIL.json` is written ONLY on a kernel refusal. Both used to share
/// one path, so the next successful cut overwrote the failing capture — the rig destroyed
/// exactly the case it exists to preserve (owner log 2026-08-09: the `boundary=10` refusal
/// was gone by the time it was read, replaced by the success that followed it).
fn write_extrude_repro(
    name: &str,
    target: &core_math::half_edge::HalfEdgeMesh,
    cutter: &core_math::half_edge::HalfEdgeMesh,
) {
    let (tv, tf) = repro_mesh_soup(target);
    let (cv, cf) = repro_mesh_soup(cutter);
    let path = std::env::temp_dir().join(name);
    let json = format!(
        "{{\"target_verts\":{tv:?},\"target_faces\":{tf:?},\"cutter_verts\":{cv:?},\"cutter_faces\":{cf:?}}}"
    );
    if std::fs::write(&path, json).is_ok() {
        eprintln!("[EXTRUDE-PT] wrote repro {}", path.display());
    }
}

impl AzraApp {
    /// Start an extrude drag: create topology with zero offset, arm for mouse dragging.
    /// `along_normals` = Blender Alt+E Extrude Along Normals (per-vertex normals on the cap).
    /// Routes by work mode: Face → region, Edge → boundary-edge extrude, Vertex → wire verts.
    pub(crate) fn poly_start_extrude_drag(&mut self, mouse: egui::Pos2) {
        match self.poly_work_mode {
            PolyWorkMode::Edge => self.poly_start_extrude_edges_drag(mouse),
            PolyWorkMode::Vertex => self.poly_start_extrude_verts_drag(mouse),
            _ => self.poly_start_extrude_drag_ex(mouse, false),
        }
    }

    /// Alt+E — Extrude Faces Along Normals (same topology as E, different drag axis per vertex).
    pub(crate) fn poly_start_extrude_along_normals_drag(&mut self, mouse: egui::Pos2) {
        self.poly_start_extrude_drag_ex(mouse, true);
    }

    /// Edge mode E: Blender extrude_edges_move — new quads on boundary edges, slide new verts.
    pub(crate) fn poly_start_extrude_edges_drag(&mut self, mouse: egui::Pos2) {
        let nid = match self.sel.usel.active_node() { Some(n) => n, None => return };
        let edges: HashSet<u32> = self.poly_sel.selected_edges.clone();
        if edges.is_empty() {
            self.toast("Extrude: select edges first");
            return;
        }
        if !matches!(
            self.scene.scene_graph.get(nid).map(|n| &n.payload),
            Some(NodePayload::PolygonalBody { .. })
        ) {
            self.toast("Extrude: active object is not a poly mesh");
            return;
        }
        // Average normal of faces adjacent to selected edges (direction to slide).
        let mut dir = Vec3::ZERO;
        if let Some(node) = self.scene.scene_graph.get(nid) {
            if let NodePayload::PolygonalBody { mesh, .. } = &node.payload {
                for &eid in &edges {
                    if eid as usize >= mesh.half_edges.len() { continue; }
                    let he = mesh.he(HalfEdgeId(eid));
                    if he.face != NO_FACE {
                        dir += mesh.compute_face_normal(he.face);
                    } else if he.twin != NO_TWIN && mesh.he(he.twin).face != NO_FACE {
                        dir += mesh.compute_face_normal(mesh.he(he.twin).face);
                    }
                }
            }
        }
        let dir = dir.normalize_or_zero();
        if dir.length_squared() < 0.25 {
            self.toast("Extrude: no bevelable/face-adjacent edges");
            return;
        }
        self.begin_episode("Extrude Edges");
        let use_chosen = self.tweak.extrude_use_chosen_material;
        let chosen_mid = self.tweak.extrude_chosen_material_id;
        let pre_face_n = self
            .poly_node_mesh(nid)
            .map(|m| m.faces.len())
            .unwrap_or(0);
        let mut new_verts: Vec<(u32, Vec3)> = Vec::new();
        if let Some(node) = self.scene.scene_graph.get_mut(nid) {
            if let NodePayload::PolygonalBody { mesh, .. } = &mut node.payload {
                let mesh = Arc::make_mut(mesh);
                let created = mesh.extrude_edges(&edges, Vec3::ZERO);
                if created.is_empty() {
                    // episode still open — cancel
                } else {
                    for v in created {
                        new_verts.push((v.0, dir));
                    }
                    // Chosen-material override on new fin faces (default already inherits source).
                    if use_chosen {
                        for fi in pre_face_n..mesh.faces.len() {
                            if mesh.face(FaceId(fi as u32)).half_edge != NO_EDGE {
                                mesh.face_mut(FaceId(fi as u32)).material_id = chosen_mid;
                            }
                        }
                    }
                }
            }
        }
        if new_verts.is_empty() {
            self.cancel_episode();
            self.toast("Extrude Edges: nothing extrudable in the selection");
            return;
        }
        let orig_sel_faces = self.poly_sel.selected_faces.clone();
        let orig_sel_edges = self.poly_sel.selected_edges.clone();
        let orig_sel_vertices = self.poly_sel.selected_vertices.clone();
        self.poly_sel.selected_faces.clear();
        self.poly_sel.selected_edges.clear();
        self.poly_sel.selected_vertices.clear();
        // Select the new EDGES and STAY in Edge mode.
        //
        // Owner 2026-07-30: "when I duplicate edge I only see vertex." This used to select just the
        // lifted verts AND force `poly_work_mode = Vertex`, so an edge extrude dumped the user into
        // Vertex mode looking at dots. An edge extrude's result is an EDGE — select that, in the
        // mode the user chose.
        //
        // The rim is every half-edge whose BOTH endpoints are lifted verts, canonicalised by
        // min(i, twin) so the twin does not double-add. Verts are selected too because the shared
        // transform gathers movable vertices from the selection.
        let lifted: HashSet<u32> = new_verts.iter().map(|&(v, _)| v).collect();
        let mut rim: Vec<u32> = Vec::new();
        if let Some(node) = self.scene.scene_graph.get(nid) {
            if let NodePayload::PolygonalBody { mesh, .. } = &node.payload {
                let mut seen: HashSet<u32> = HashSet::new();
                for i in 0..mesh.half_edges.len() as u32 {
                    let he = HalfEdgeId(i);
                    if !mesh.is_edge_live(he) { continue; }
                    let dst = mesh.he(he).vertex.0;
                    let src = mesh.he_source(he).0;
                    if !lifted.contains(&dst) || !lifted.contains(&src) { continue; }
                    let twin = mesh.he(he).twin;
                    let canon = if twin != NO_TWIN { i.min(twin.0) } else { i };
                    if seen.insert(canon) { rim.push(canon); }
                }
            }
        }
        for &(vid, _) in &new_verts {
            self.poly_sel.selected_vertices.insert(vid);
        }
        if rim.is_empty() {
            // No rim edge formed — leave the user on the verts rather than an empty selection.
            // Sticky Group: never leave polygroup selection mode for an op side-effect.
            self.poly_prefer_work_mode(PolyWorkMode::Vertex);
        } else {
            for &eid in &rim {
                self.poly_sel.selected_edges.insert(eid);
            }
            // STAY in Edge mode when that was the user mode — do not leave Group.
        }
        self.poly_sync_mesh_cache(nid);

        // HAND OFF TO GRAB — do not run a bespoke one-axis drag.
        //
        // Owner 2026-07-30: "the extrude edge slice uses a random move behavior, it should just use
        // the grab and all its features." Correct, and it is also the CLAUDE.md "two code paths for
        // one operation" rule: the `PolyExtrude` drag re-implemented movement along ONE fixed
        // averaged normal, so it had no axis constraint, no plane constraint, no numeric entry, no
        // falloff — and critically no Action Center, because pivot resolution lives in
        // `poly_start_transform` (:853-870) which that path never called.
        //
        // `poly_start_transform(Grab)` owns all of it and reads the selection set just above, so the
        // extruded rim is the subject. X/Y/Z constrain, Shift+axis gives the plane, typing gives
        // numeric offset.
        //
        // UNDO: `begin_episode` auto-commits (undo.rs:804), so "Extrude Edges" closes here and Grab
        // opens its own. That is the correct split — the topology add is a completed fact once the
        // rim exists, and Esc-cancelling the MOVE must leave the extruded edge rather than silently
        // deleting geometry the user can already see. Blender's E-then-cancel behaves the same way.
        self.set_poly_tool(PolyTool::ExtrudeFace);
        self.poly_start_transform(TransformKind::Grab, mouse);
        let _ = (dir, orig_sel_faces, orig_sel_edges, orig_sel_vertices);
        self.ui2.active_tool = "Extrude".into();
        self.ui2.active_pillar = "poly".into();
        self.toast(
            "Extrude Edges → Grab: X/Y/Z · Shift=axis · Angle lock+Snap=° · type offset · Ctrl snap · LMB/Enter · Esc",
        );
    }

    /// Vertex mode E: Blender extrude_vertices_move — wire edges + slide new verts.
    pub(crate) fn poly_start_extrude_verts_drag(&mut self, mouse: egui::Pos2) {
        let nid = match self.sel.usel.active_node() { Some(n) => n, None => return };
        let verts: HashSet<u32> = self.poly_sel.selected_vertices.clone();
        if verts.is_empty() {
            self.toast("Extrude: select vertices first");
            return;
        }
        if !matches!(
            self.scene.scene_graph.get(nid).map(|n| &n.payload),
            Some(NodePayload::PolygonalBody { .. })
        ) {
            self.toast("Extrude: active object is not a poly mesh");
            return;
        }
        // Direction: average of incident face normals, else vertex normal, else +Y.
        let mut dir = Vec3::ZERO;
        if let Some(node) = self.scene.scene_graph.get(nid) {
            if let NodePayload::PolygonalBody { mesh, .. } = &node.payload {
                for &vid in &verts {
                    let fan = mesh.vertex_fan(VertexId(vid));
                    let mut nsum = Vec3::ZERO;
                    for he in fan {
                        let f = mesh.he(he).face;
                        if f != NO_FACE {
                            nsum += mesh.compute_face_normal(f);
                        }
                    }
                    if nsum.length_squared() > 1e-8 {
                        dir += nsum.normalize_or_zero();
                    } else {
                        dir += mesh.vert(VertexId(vid)).normal;
                    }
                }
            }
        }
        let dir = dir.normalize_or_zero();
        let dir = if dir.length_squared() > 0.25 { dir } else { Vec3::Y };
        self.begin_episode("Extrude Vertices");
        let mut new_verts: Vec<(u32, Vec3)> = Vec::new();
        if let Some(node) = self.scene.scene_graph.get_mut(nid) {
            if let NodePayload::PolygonalBody { mesh, .. } = &mut node.payload {
                let mesh = Arc::make_mut(mesh);
                let created = mesh.extrude_vertices(&verts, Vec3::ZERO);
                for v in created {
                    new_verts.push((v.0, dir));
                }
            }
        }
        if new_verts.is_empty() {
            self.cancel_episode();
            self.toast("Extrude Vertices: nothing to extrude");
            return;
        }
        let orig_sel_faces = self.poly_sel.selected_faces.clone();
        let orig_sel_edges = self.poly_sel.selected_edges.clone();
        let orig_sel_vertices = self.poly_sel.selected_vertices.clone();
        self.poly_sel.selected_faces.clear();
        self.poly_sel.selected_edges.clear();
        self.poly_sel.selected_vertices.clear();
        for &(vid, _) in &new_verts {
            self.poly_sel.selected_vertices.insert(vid);
        }
        self.poly_sync_mesh_cache(nid);
        // HAND OFF TO GRAB — same reasoning as the Edge path above (owner: "should just use the grab
        // and all its features"). Vertex extrude's result IS vertices, so the selection is correct
        // as-is; only the movement needed to stop being a bespoke one-direction drag.
        self.set_poly_tool(PolyTool::ExtrudeFace);
        self.poly_start_transform(TransformKind::Grab, mouse);
        let _ = (dir, new_verts, orig_sel_faces, orig_sel_edges, orig_sel_vertices);
        self.ui2.active_tool = "Extrude".into();
        self.ui2.active_pillar = "poly".into();
        self.toast(
            "Extrude Vertices → Grab: X/Y/Z axis · Shift+axis plane · type offset · Ctrl snap · LMB/Enter confirm · Esc cancel",
        );
    }

    fn poly_start_extrude_drag_ex(&mut self, mouse: egui::Pos2, along_normals: bool) {
        let nid = match self.sel.usel.active_node() { Some(n) => n, None => return };
        // Modo implicit full selection: an empty selection extrudes EVERY face (SAFE). The `_or_all`
        // resolve happens HERE, at drag start, where the selection snapshot is taken.
        let faces: Vec<u32> = self.poly_faces_or_all(nid);
        if faces.is_empty() { return; }
        // Refuse before episode when active node is not a poly mesh (empty episode otherwise).
        if !matches!(
            self.scene.scene_graph.get(nid).map(|n| &n.payload),
            Some(NodePayload::PolygonalBody { .. })
        ) {
            self.toast("Extrude: active object is not a poly mesh");
            return;
        }
        let ep_label = if along_normals { "Extrude Along Normals" } else { "Extrude Face" };
        self.begin_episode(ep_label);
        // Push-through needs the PRE-extrude mesh; grab the Arc before make_mut CoWs it away.
        let pre_mesh = self.scene.scene_graph.get(nid).and_then(|node| match &node.payload {
            NodePayload::PolygonalBody { mesh, .. } => Some(mesh.clone()),
            _ => None,
        });

        // Along Normals defaults to INDIVIDUAL (planar walls). Grouped is opt-in via G mid-drag,
        // or per-user via right-click "Set as default" on the Mode buttons (user_defaults).
        // Force planar defaults ON so Along Normals never invents bent caps (owner FORCE STRAITH FACE).
        let along_normals_grouped =
            along_normals && app_types::user_defaults::get_bool("poly.extrude_along_normals_grouped").unwrap_or(false);
        let force_planar =
            along_normals && app_types::user_defaults::get_bool("poly.extrude_force_planar").unwrap_or(true);
        let use_chosen = self.tweak.extrude_use_chosen_material;
        let chosen_mid = self.tweak.extrude_chosen_material_id;
        let (face_normals, new_face_ids, mut vert_normals) = {
            let Some(node) = self.scene.scene_graph.get_mut(nid) else {
                self.cancel_episode();
                return;
            };
            let NodePayload::PolygonalBody { mesh, .. } = &mut node.payload else {
                self.cancel_episode();
                return;
            };
            let mesh = Arc::make_mut(mesh);
            Self::poly_extrude_build_topology(mesh, &faces, along_normals, along_normals_grouped)
        };
        if new_face_ids.is_empty() {
            self.cancel_episode();
            return;
        }
        if force_planar {
            // Face-constant normals — no blended corner bend on the slide vectors.
            vert_normals = Self::poly_extrude_face_constant_normals(
                // need mesh for face rings — re-read after topo build
                &self.poly_node_mesh(nid).expect("mesh after extrude topo"),
                &face_normals,
            );
        }
        // SMART SELECT: what this extrude ALLOCATED. Live-now vs pre_mesh is an exact
        // created-face set — works at zero offset where every wall is still degenerate.
        let created_faces = match (&pre_mesh, self.poly_node_mesh(nid)) {
            (Some(pre), Some(post)) => crate::poly::op_tags::faces_created(pre, &post),
            _ => Vec::new(),
        };
        // Material: default = walls already inherit source face mat in the kernel.
        // Toggle: paint chosen material onto newly created faces (walls / volume caps).
        if use_chosen && !created_faces.is_empty() {
            if let Some(node) = self.scene.scene_graph.get_mut(nid) {
                if let NodePayload::PolygonalBody { mesh, .. } = &mut node.payload {
                    let mesh = Arc::make_mut(mesh);
                    for &fid in &created_faces {
                        if (fid as usize) < mesh.faces.len()
                            && mesh.face(FaceId(fid)).half_edge != NO_EDGE
                        {
                            mesh.face_mut(FaceId(fid)).material_id = chosen_mid;
                        }
                    }
                }
            }
        }

        // EXCANCELSEL: snapshot the pre-tool selection BEFORE we overwrite it.
        let orig_sel_faces = self.poly_sel.selected_faces.clone();
        let orig_sel_edges = self.poly_sel.selected_edges.clone();
        let orig_sel_vertices = self.poly_sel.selected_vertices.clone();

        self.poly_sel.selected_faces.clear();
        for &f in &new_face_ids { self.poly_sel.selected_faces.insert(f); }
        self.poly_sync_mesh_cache(nid);

        self.start_operation(ActiveTool::PolyExtrude(PolyExtrudeDrag {
            node_id: nid, face_normals, new_face_ids,
            start_mouse: mouse, accumulated_offset: 0.0,
            along_normals,
            along_normals_grouped,
            force_planar,
            vert_normals,
            orig_sel_faces, orig_sel_edges, orig_sel_vertices,
            pre_mesh,
            orig_faces: faces.clone(),
            world_axis: false,
            shift_was_down: false,
            alt_was_down: true,
            edge_axis: None,
            created_faces,
        }));
        self.ui2.active_tool = if along_normals {
            "Extrude Along Normals".into()
        } else {
            "Extrude".into()
        };
        self.ui2.active_pillar = "poly".into();
        if along_normals {
            self.toast(
                "Extrude Along Normals: Individual · Force planar ON · G=Group · I=Individual · drag · LMB confirm",
            );
        } else {
            self.toast(
                "Extrude: drag · Shift=world axis lock · Alt=free grab · N=along normals · D=edge dir · Ctrl snap · LMB confirm",
            );
        }
    }

    /// Face-constant slide vectors: every vertex of a cap uses that face's frozen normal.
    /// Shared verts (grouped shell) average the incident face normals, then re-normalize.
    /// Owner "force straight face" — no corner blend that twists the face.
    fn poly_extrude_face_constant_normals(
        mesh: &core_math::half_edge::HalfEdgeMesh,
        face_normals: &[(u32, Vec3)],
    ) -> Vec<(u32, Vec3)> {
        use std::collections::HashMap;
        let mut acc: HashMap<u32, Vec3> = HashMap::new();
        for &(fid, n) in face_normals {
            let n = n.normalize_or_zero();
            if n.length_squared() < 0.25 {
                continue;
            }
            if (fid as usize) >= mesh.faces.len()
                || mesh.face(FaceId(fid)).half_edge == NO_EDGE
            {
                continue;
            }
            for &v in &mesh.face_vertices(FaceId(fid)) {
                *acc.entry(v.0).or_insert(Vec3::ZERO) += n;
            }
        }
        acc.into_iter()
            .filter_map(|(vid, n)| {
                let un = n.normalize_or_zero();
                if un.length_squared() > 0.25 {
                    Some((vid, un))
                } else {
                    None
                }
            })
            .collect()
    }

    /// Project each cap onto the plane through its centroid with frozen face normal.
    /// Makes knife-cut n-gons / blended slides land as true planar faces.
    fn poly_extrude_flatten_caps(
        mesh: &mut core_math::half_edge::HalfEdgeMesh,
        face_normals: &[(u32, Vec3)],
    ) {
        // ONE ring buffer for the whole loop: this runs every drag frame under the
        // Along-Normals force-planar default, and `face_vertices` allocates per face.
        let mut vs: Vec<VertexId> = Vec::new();
        for &(fid, n) in face_normals {
            let n = n.normalize_or_zero();
            if n.length_squared() < 0.25 {
                continue;
            }
            if (fid as usize) >= mesh.faces.len()
                || mesh.face(FaceId(fid)).half_edge == NO_EDGE
            {
                continue;
            }
            mesh.face_vertices_into(FaceId(fid), &mut vs);
            if vs.len() < 3 {
                continue;
            }
            let c: Vec3 = vs.iter().map(|&v| mesh.vert(v).position).sum::<Vec3>()
                / vs.len() as f32;
            for &v in &vs {
                let p = mesh.vert(v).position;
                let d = (p - c).dot(n);
                mesh.vert_mut(v).position = p - n * d;
            }
        }
    }

    /// Build extrude topology on `mesh` (already the live body mesh at zero offset).
    ///
    /// - Region (`!along_normals`, or `along_normals && grouped`): one connected shell.
    /// - Individual (`along_normals && !grouped`): each face alone along its normal.
    ///
    /// Returns `(face_normals, new_face_ids, vert_normals)`.
    fn poly_extrude_build_topology(
        mesh: &mut core_math::half_edge::HalfEdgeMesh,
        faces: &[u32],
        along_normals: bool,
        grouped: bool,
    ) -> (Vec<(u32, Vec3)>, Vec<u32>, Vec<(u32, Vec3)>) {
        let mut face_normals = Vec::new();
        let mut new_face_ids = Vec::new();
        let mut vert_normals: Vec<(u32, Vec3)> = Vec::new();

        let individual = along_normals && !grouped;
        if individual {
            // INDIVIDUAL — each face alone (owner TEST-021: planar walls inward).
            let mut faces_sorted = faces.to_vec();
            faces_sorted.sort_unstable();
            // Dedup by vertex id AS WE GO instead of push-then-dedup. Same values as the
            // old trailing `seen.insert()` pass — cap ring writes overwrite (last face in
            // sorted order wins), the new-vertex sweep only fills gaps (`or_insert`, which
            // is what `!vert_normals.iter().any(..)` meant). Kills the O(N^2) membership
            // scan over a Vec that grew across every selected face.
            let mut seen = std::collections::HashMap::<u32, Vec3>::new();
            let mut ring: Vec<VertexId> = Vec::new();
            for &fid in &faces_sorted {
                if (fid as usize) >= mesh.faces.len()
                    || mesh.face(FaceId(fid)).half_edge == NO_EDGE
                {
                    continue;
                }
                let n = mesh.compute_face_normal(FaceId(fid)).normalize_or_zero();
                if n.length_squared() < 0.25 {
                    continue;
                }
                let before_vc = mesh.vertices.len() as u32;
                let cap = mesh.extrude_face(FaceId(fid), Vec3::ZERO);
                new_face_ids.push(cap.0);
                face_normals.push((cap.0, n));
                mesh.face_vertices_into(cap, &mut ring);
                for &v in &ring {
                    seen.insert(v.0, n);
                }
                for vid in before_vc..mesh.vertices.len() as u32 {
                    if mesh.vert(VertexId(vid)).half_edge != NO_EDGE {
                        seen.entry(vid).or_insert(n);
                    }
                }
            }
            vert_normals = seen.into_iter().collect();
            // Ascending vertex id — the old `HashMap::into_iter()` order was random per
            // run. Every consumer applies exactly one `+= n * delta` per unique vertex, so
            // order is inert to the result; sorting only makes it reproducible (and makes
            // the per-frame slide walk vertex storage in order).
            vert_normals.sort_unstable_by_key(|&(v, _)| v);
        } else {
            // REGION shell (plain E, or Along Normals + Grouped).
            for &fid in faces {
                face_normals.push((fid, mesh.compute_face_normal(FaceId(fid))));
            }
            let set: std::collections::HashSet<FaceId> =
                faces.iter().map(|&f| FaceId(f)).collect();
            let caps = mesh.extrude_region(&set, Vec3::ZERO);
            new_face_ids = caps.iter().map(|f| f.0).collect();
            if new_face_ids.len() == face_normals.len() {
                for (i, &nf) in new_face_ids.iter().enumerate() {
                    face_normals[i].0 = nf;
                }
            } else {
                face_normals = new_face_ids
                    .iter()
                    .map(|&nf| (nf, mesh.compute_face_normal(FaceId(nf))))
                    .collect();
            }
            // Blended per-cap-vertex normals (needed for Along Normals grouped slide and
            // for mid-drag N toggle into along-normals without rebuild).
            let mut acc: std::collections::HashMap<u32, Vec3> =
                std::collections::HashMap::new();
            let mut n_by_new: std::collections::HashMap<u32, Vec3> =
                std::collections::HashMap::new();
            for (i, &nf) in new_face_ids.iter().enumerate() {
                let n = if i < face_normals.len() {
                    face_normals[i].1
                } else {
                    mesh.compute_face_normal(FaceId(nf))
                };
                n_by_new.insert(nf, n);
            }
            for (&fid, n) in &n_by_new {
                for &v in &mesh.face_vertices(FaceId(fid)) {
                    *acc.entry(v.0).or_insert(Vec3::ZERO) += *n;
                }
            }
            for (vid, n) in acc {
                let un = n.normalize_or_zero();
                if un.length_squared() > 0.25 {
                    vert_normals.push((vid, un));
                }
            }
        }
        (face_normals, new_face_ids, vert_normals)
    }

    /// Mid-drag **G** / **I** while Extrude Along Normals is active: Grouped ⇄ Individual.
    /// Topology differs, so we restore `pre_mesh`, rebuild, and re-apply the current offset
    /// (same pattern as inset G/I). No-op if not in along-normals mode.
    pub(crate) fn poly_extrude_set_along_group(&mut self, grouped: bool) {
        let (nid, pre, faces, start_mouse, offset, along_normals, was_grouped, force_planar, world_axis, edge_axis, orig_sel_f, orig_sel_e, orig_sel_v, created_faces) =
            match self.tool.current.as_poly_extrude() {
                Some(d) => (
                    d.node_id,
                    d.pre_mesh.clone(),
                    d.orig_faces.clone(),
                    d.start_mouse,
                    d.accumulated_offset,
                    d.along_normals,
                    d.along_normals_grouped,
                    d.force_planar,
                    d.world_axis,
                    d.edge_axis,
                    d.orig_sel_faces.clone(),
                    d.orig_sel_edges.clone(),
                    d.orig_sel_vertices.clone(),
                    d.created_faces.clone(),
                ),
                None => return,
            };
        if !along_normals {
            self.toast("Extrude: N = Along Normals, then G = Group / I = Individual");
            return;
        }
        if was_grouped == grouped {
            self.toast(if grouped {
                "Extrude Along Normals: already Grouped"
            } else {
                "Extrude Along Normals: already Individual"
            });
            return;
        }
        let Some(pre) = pre else {
            self.toast("Extrude: cannot switch group mode (no pre-mesh)");
            return;
        };
        // Restore pre-extrude mesh, rebuild topology in the new mode.
        if let Some(node) = self.scene.scene_graph.get_mut(nid) {
            if let NodePayload::PolygonalBody { mesh, .. } = &mut node.payload {
                *mesh = pre.clone();
            }
        }
        let (face_normals, new_face_ids, mut vert_normals) = {
            let Some(node) = self.scene.scene_graph.get_mut(nid) else { return };
            let NodePayload::PolygonalBody { mesh, .. } = &mut node.payload else { return };
            let mesh = Arc::make_mut(mesh);
            Self::poly_extrude_build_topology(mesh, &faces, true, grouped)
        };
        if force_planar {
            if let Some(m) = self.poly_node_mesh(nid) {
                vert_normals = Self::poly_extrude_face_constant_normals(&m, &face_normals);
            }
        }
        if new_face_ids.is_empty() {
            // Put the drag state back on the restored mesh with zero offset.
            if let Some(d) = self.tool.current.as_poly_extrude_mut() {
                d.face_normals = face_normals;
                d.new_face_ids = new_face_ids;
                d.vert_normals = vert_normals;
                d.along_normals_grouped = grouped;
                d.force_planar = force_planar;
                d.accumulated_offset = 0.0;
            }
            self.toast("Extrude: nothing to extrude in that mode");
            return;
        }
        // Write drag state BEFORE apply_delta so force_planar flatten can read it.
        if let Some(d) = self.tool.current.as_poly_extrude_mut() {
            d.face_normals = face_normals.clone();
            d.new_face_ids = new_face_ids.clone();
            d.vert_normals = vert_normals.clone();
            d.along_normals_grouped = grouped;
            d.force_planar = force_planar;
            d.start_mouse = start_mouse;
            d.accumulated_offset = 0.0;
            d.world_axis = world_axis;
            d.edge_axis = edge_axis;
            d.orig_sel_faces = orig_sel_f;
            d.orig_sel_edges = orig_sel_e;
            d.orig_sel_vertices = orig_sel_v;
            d.created_faces = created_faces;
            d.pre_mesh = Some(pre);
            d.orig_faces = faces;
            d.along_normals = true;
        }
        // Re-apply current offset under Along Normals.
        if offset.abs() > 1e-7 {
            let avg = {
                let sum: Vec3 = face_normals.iter().map(|(_, n)| *n).sum();
                sum.normalize_or_zero()
            };
            self.poly_extrude_apply_delta(nid, &new_face_ids, avg, &vert_normals, true, offset);
            if let Some(d) = self.tool.current.as_poly_extrude_mut() {
                d.accumulated_offset = offset;
            }
        }
        self.poly_sel.selected_faces.clear();
        for &f in &new_face_ids {
            self.poly_sel.selected_faces.insert(f);
        }
        // Keep force_planar / along flags; remainder already written above.
        if let Some(d) = self.tool.current.as_poly_extrude_mut() {
            d.force_planar = force_planar;
            d.along_normals = true;
        }
        self.ui2.active_tool = "Extrude Along Normals".into();
        self.poly_sync_mesh_cache(nid);
        self.mark_dirty(DirtyFlags::RENDER);
        self.toast(if grouped {
            "Extrude Along Normals: Grouped (connected shell)"
        } else {
            "Extrude Along Normals: Individual (per-face)"
        });
    }

    /// Update extrude drag: slide top-face vertices along normals by mouse delta.
    /// Effective extrude direction for the drag state: edge-axis wins, then world-axis
    /// snap, then the raw region normal.
    fn extrude_effective_dir(avg: Vec3, world_axis: bool, edge_axis: Option<Vec3>) -> Vec3 {
        if let Some(e) = edge_axis {
            // Sign: keep the hemisphere of the region normal so drag polarity is stable.
            if e.dot(avg) < 0.0 { -e } else { e }
        } else if world_axis {
            Self::axis_snap(avg)
        } else {
            avg
        }
    }

    /// COPLANAR-FACE PUSH (owner algorithm 2026-08-04: "the tool needs to recognize each
    /// face that is flat with another one and push the face so the boolean comes clean...
    /// the faces that are extruded need to follow the normal of each edge, if we just
    /// extrude and move we lose the original shape").
    ///
    /// Any cutter face lying in the SAME plane as a target face is a coplanar contact — the
    /// hardest case for a boolean and the source of zero-thickness flaps. Each such face is
    /// pushed OUT along its OWN normal by `eps`, so the cutter always crosses the target
    /// cleanly. Vertices are then re-solved as the intersection of their incident face
    /// planes (not translated), which is what keeps the shape exact: a vertex shared by a
    /// pushed face and two untouched ones slides ALONG those two, so no face is distorted.
    fn push_coplanar_cutter_faces(
        cutter: &core_math::half_edge::HalfEdgeMesh,
        target: &core_math::half_edge::HalfEdgeMesh,
        eps: f32,
        surface_n: Vec3,
    ) -> core_math::half_edge::HalfEdgeMesh {
        use core_math::half_edge::{FaceId as F, VertexId as V, NO_EDGE as NE};
        let plane = |m: &core_math::half_edge::HalfEdgeMesh, f: F| -> (Vec3, f32) {
            let n = m.compute_face_normal(f).normalize_or_zero();
            let vs = m.face_vertices(f);
            let c = vs.iter().map(|&v| m.vert(v).position).sum::<Vec3>() / vs.len().max(1) as f32;
            (n, n.dot(c))
        };
        let mut tplanes: Vec<(Vec3, f32)> = Vec::new();
        for i in 0..target.faces.len() as u32 {
            let f = F(i);
            if target.face(f).half_edge == NE { continue; }
            let (n, d) = plane(target, f);
            if n != Vec3::ZERO { tplanes.push((n, d)); }
        }
        // Per cutter face: its plane, pushed outward when coplanar with any target face.
        let mut fplane: Vec<Option<(Vec3, f32)>> = vec![None; cutter.faces.len()];
        let mut pushed_any = false;
        for i in 0..cutter.faces.len() as u32 {
            let f = F(i);
            if cutter.face(f).half_edge == NE { continue; }
            let (n, d) = plane(cutter, f);
            if n == Vec3::ZERO { continue; }
            let coplanar = tplanes.iter().any(|&(tn, td)| {
                let dot = n.dot(tn);
                dot.abs() > 0.9995 && (d - if dot > 0.0 { td } else { -td }).abs() < eps * 4.0
            });
            // DIRECTION OF THE PUSH (owner 2026-08-04: "edge that never touched the
            // boolean" changed). A CAP (normal along the sweep) may be pushed OUT — that
            // only extends the cutter through the opening it is already making. A SIDE WALL
            // flush with a target wall must NOT be pushed out: that eats a sliver of the
            // neighbouring material the cut was never meant to reach. Shrink it inward
            // instead, so the worst case leaves the neighbour intact.
            // Caps lie in planes PARALLEL TO THE REGION SURFACE, so classify against the
            // surface normal — not the sweep. (Regression 0695335: the test compared the
            // face normal to the SWEEP with a 0.5 threshold; on a tilted face the entry
            // cap's normal makes angle `tilt` with the sweep, so past ~60° the cap was
            // misclassified as a side wall and shrank inward instead of clearing the
            // surface — the tilted cut stopped working.)
            // OWNER-RATIFIED CONFIGURATION (2026-08-04, build 5aeaec3/258a5b3 "works
            // best"): EVERY coplanar face pushes OUT by the full eps. Empirically the only
            // variant that lands the cut across flat/low-tilt/steep-tilt. The later
            // refinements all regressed live: inward shrink -> folds, every cut rejected;
            // token outward -> still near-coplanar slivers; exact-plane snap -> the snapped
            // face's EDGES still sit near target edges, Manifold slivers anyway (owner
            // console folds=10). Cost accepted: a flush neighbour is trimmed by eps
            // (diag*1e-3); the edge-collapse guard below still aborts degenerate pushes.
            let _ = surface_n;
            fplane[i as usize] = Some((n, if coplanar { pushed_any = true; d + eps } else { d }));
        }
        if !pushed_any {
            return cutter.clone(); // no coplanar contact — nothing to fix
        }
        // Re-solve each vertex from its incident (possibly pushed) face planes.
        let mut positions: Vec<Vec3> = Vec::new();
        let mut remap: std::collections::HashMap<u32, usize> = std::collections::HashMap::new();
        for i in 0..cutter.vertices.len() as u32 {
            let v = V(i);
            if cutter.vert(v).half_edge == NE { continue; }
            let p0 = cutter.vert(v).position;
            let mut planes: Vec<(Vec3, f32)> = Vec::new();
            for he in cutter.vertex_fan(v) {
                let f = cutter.he(he).face;
                if f == core_math::half_edge::NO_FACE { continue; }
                if let Some(pl) = fplane[f.0 as usize] {
                    if !planes.iter().any(|q| q.0.dot(pl.0) > 0.9995) { planes.push(pl); }
                }
            }
            let newp = if planes.len() >= 3 {
                // Pick the three most mutually independent normals and intersect them.
                let m = glam::Mat3::from_cols(planes[0].0, planes[1].0, planes[2].0).transpose();
                let rhs = Vec3::new(planes[0].1, planes[1].1, planes[2].1);
                if m.determinant().abs() > 1e-6 { m.inverse() * rhs } else { p0 }
            } else if planes.len() == 2 {
                // Edge vertex: slide along the line of the two planes only as far as needed.
                let (n0, d0) = planes[0];
                let (n1, d1) = planes[1];
                let dir = n0.cross(n1).normalize_or_zero();
                let m = glam::Mat3::from_cols(n0, n1, dir).transpose();
                let rhs = Vec3::new(d0, d1, dir.dot(p0));
                if m.determinant().abs() > 1e-6 { m.inverse() * rhs } else { p0 }
            } else if planes.len() == 1 {
                p0 + planes[0].0 * (planes[0].1 - planes[0].0.dot(p0))
            } else {
                p0
            };
            remap.insert(i, positions.len());
            positions.push(if newp.is_finite() { newp } else { p0 });
        }
        let mut faces: Vec<Vec<usize>> = Vec::new();
        for i in 0..cutter.faces.len() as u32 {
            let f = F(i);
            if cutter.face(f).half_edge == NE { continue; }
            let ring: Vec<usize> = cutter
                .face_vertices(f)
                .iter()
                .filter_map(|v| remap.get(&v.0).copied())
                .collect();
            if ring.len() >= 3 { faces.push(ring); }
        }
        // NO EDGE MAY COLLAPSE (owner 2026-08-04: "just one edge collapses that should
        // not"). The per-vertex plane solve can drive two neighbours onto each other when a
        // face is nearly parallel to its neighbour — that silently deletes an edge of the
        // cutter and therefore of the cut. If ANY edge loses more than a third of its
        // length, abandon the push entirely and cut with the original cutter: a coplanar
        // contact the kernel must work harder on is far better than a wrong shape.
        for i in 0..cutter.half_edges.len() as u32 {
            let he = core_math::half_edge::HalfEdgeId(i);
            if !cutter.is_edge_live(he) { continue; }
            let (a_id, b_id) = (cutter.he_source(he).0, cutter.he(he).vertex.0);
            let (Some(&ia), Some(&ib)) = (remap.get(&a_id), remap.get(&b_id)) else { continue };
            let before = cutter.vert(core_math::half_edge::VertexId(a_id)).position
                .distance(cutter.vert(core_math::half_edge::VertexId(b_id)).position);
            if before <= 1e-9 { continue; }
            if positions[ia].distance(positions[ib]) < before * 0.667 {
                eprintln!("[EXTRUDE-PT] coplanar push would collapse an edge — skipped");
                return cutter.clone();
            }
        }
        let out = core_math::half_edge::HalfEdgeMesh::from_polygons(&positions, &faces);
        // VOLUME SELF-CHECK (owner test-02, 2026-08-04, proven vs the Blender oracle): the
        // plane re-solve can fold little corner regions of the cutter inside-out. The mesh
        // then measures roughly the same but its RESOLVED solid is larger, and the subtract
        // removes too much (ours 0.715 vs Blender 0.824 on the same inputs). A push that
        // changes the cutter volume by more than 2% is geometrically wrong — fall back to
        // the un-pushed (ring-snapped, exactly-coplanar) cutter, which the kernel handles.
        {
            use core_math::mesh_boolean::mesh_signed_volume;
            let (v0, v1) = (mesh_signed_volume(cutter), mesh_signed_volume(&out));
            if v0.abs() > 1e-12 && ((v1 - v0) / v0).abs() > 0.02 {
                eprintln!(
                    "[EXTRUDE-PT] coplanar push distorted cutter volume ({v0:.5} -> {v1:.5}) — skipped"
                );
                return cutter.clone();
            }
        }
        if out.validate().is_ok() && out.is_manifold().is_ok() { out } else { cutter.clone() }
    }

    /// Dominant world axis of `n`, sign preserved (Shift world-align extrude).
    fn axis_snap(n: Vec3) -> Vec3 {
        let a = n.abs();
        if a.x >= a.y && a.x >= a.z {
            Vec3::new(n.x.signum(), 0.0, 0.0)
        } else if a.y >= a.z {
            Vec3::new(0.0, n.y.signum(), 0.0)
        } else {
            Vec3::new(0.0, 0.0, n.z.signum())
        }
    }

    /// Hold Shift = lock extrude direction to dominant WORLD axis of the region normal.
    /// Release Shift = back to region normal. Grab free-mode parity.
    ///
    /// Must run from the **viewport** update path (not only keyboard): `handle_keyboard`
    /// early-returns when only modifiers are held (`ui2_keyboard_idle` sees empty keys_down),
    /// so a Shift-only lock was dead (owner 2026-08-06: "extrude shift world axis does not work").
    pub(crate) fn poly_extrude_set_world_axis(&mut self, on: bool) {
        let (nid, normals, new_faces, prev_offset, along_normals, vert_normals, world_axis, edge_axis) = {
            let d = match self.tool.current.as_poly_extrude() {
                Some(d) => d,
                None => return,
            };
            (
                d.node_id,
                d.face_normals.clone(),
                d.new_face_ids.clone(),
                d.accumulated_offset,
                d.along_normals,
                d.vert_normals.clone(),
                d.world_axis,
                d.edge_axis,
            )
        };
        if along_normals {
            // World lock is region-mode only; don't thrash toast every frame while Shift held.
            return;
        }
        if world_axis == on {
            return;
        }
        // Edge-axis pick (D) still wins over world lock while active.
        if edge_axis.is_some() && on {
            return;
        }
        let avg = {
            let sum: Vec3 = normals.iter().map(|(_, n)| *n).sum();
            sum.normalize_or_zero()
        };
        if avg == Vec3::ZERO {
            return;
        }
        let old_dir = Self::extrude_effective_dir(avg, world_axis, edge_axis);
        let new_dir = Self::extrude_effective_dir(avg, on, edge_axis);
        if prev_offset.abs() > 1e-7 && (old_dir - new_dir).length_squared() > 1e-10 {
            self.poly_extrude_apply_delta(nid, &new_faces, old_dir, &vert_normals, false, -prev_offset);
            self.poly_extrude_apply_delta(nid, &new_faces, new_dir, &vert_normals, false, prev_offset);
            self.poly_sync_after_position_edit(nid);
            self.mark_dirty(DirtyFlags::RENDER);
        }
        if let Some(d) = self.tool.current.as_poly_extrude_mut() {
            d.world_axis = on;
            d.shift_was_down = on;
        }
        // Toast only on rising/falling edges (we already returned if state unchanged).
        self.toast(if on {
            "Extrude: World Axis (Shift held)"
        } else {
            "Extrude: Region Normal"
        });
    }

    /// Alt mid-drag — hand the cap to free-mode Grab (owner 2026-08-07: "pressing Alt
    /// should bring the free movement, same behavior as free mode grab").
    ///
    /// Same rule as the edge/vertex extrude start paths: no bespoke free-move grows inside
    /// this drag — `poly_start_transform(Grab)` already owns free movement, X/Y/Z
    /// re-constraints, numeric entry, snapping, Shift dominant-axis lock and the Action
    /// Center pivot, and it reads the cap faces we selected at drag start.
    ///
    /// The in-flight op is dropped to Idle DELIBERATELY, not via `start_operation`'s
    /// auto-confirm: the extrude confirm treats zero offset as cancel (would delete the
    /// topology out from under the grab) and a pushed-in cap as a push-through boolean
    /// (would punch a hole mid-gesture). The topology + offset applied so far stay on the
    /// mesh as-is; `begin_episode("Poly Grab")` inside `poly_start_transform` auto-commits
    /// the open extrude episode, so undo sees extrude, then move — the same split the
    /// edge-extrude hand-off ratified (Esc on the move keeps the extruded geometry).
    pub(crate) fn poly_extrude_free_grab_handoff(&mut self, mouse: egui::Pos2) {
        let (nid, created_faces, offset) = match self.tool.current.as_poly_extrude() {
            Some(d) => (d.node_id, d.created_faces.clone(), d.accumulated_offset),
            None => return,
        };
        // Smart Select tags normally stamp at the bespoke confirm, which never runs on this
        // path. Stamp here — but only with a real offset: at ~zero every wall is degenerate
        // and the angle-based tagger reads meaningless dihedrals (its doc contract).
        if offset.abs() > 1e-5 {
            self.poly_extrude_stamp_tags(nid, &created_faces);
        }
        self.tool.current = ActiveTool::Idle;
        self.poly_start_transform(TransformKind::Grab, mouse);
        match self.tool.current.as_poly_transform_mut() {
            Some(ts) => ts.free_mode = true,
            None => {
                // Nothing movable gathered — should not happen (caps are selected); the
                // extrude state is already committed-as-built, so just report and stop.
                self.toast("Extrude: could not start free grab");
                return;
            }
        }
        self.ui2.active_tool = "Extrude".into();
        self.toast(
            "Extrude → Free Grab: move freely · Shift=axis of motion · X/Y/Z constrain · Ctrl snap · LMB/Enter · Esc",
        );
    }

    pub(crate) fn poly_update_extrude_drag(
        &mut self,
        mouse: egui::Pos2,
        rect: egui::Rect,
        ctrl: bool,
        shift: bool,
        alt: bool,
    ) {
        // CONSOLE_AUDIT A1/A2: attribute drag cost. Once-per-frame guard: keyboard+viewport
        // both used to call this (order: keyboard first, then viewport with better mouse) —
        // that doubled sync/pick work. Viewport is now sole caller; guard stays as safety net.
        let _t_total = std::time::Instant::now();
        let mut delta_ms = 0.0f32;
        let mut sync_ms = 0.0f32;
        if self.extrude_update_frame == self.diag_frame && self.extrude_perf_calls > 0 {
            // Second call same frame: skip (would re-sync the same positions).
            self.extrude_perf_calls = self.extrude_perf_calls.saturating_add(1);
            return;
        }
        self.extrude_update_frame = self.diag_frame;

        // Fresh Alt press (not the Alt+E launch chord — `alt_was_down` starts true) = hand
        // the cap to free-mode Grab (owner 2026-08-07: "pressing Alt should bring the free
        // movement, same behavior as free mode grab").
        let alt_was_down = match self.tool.current.as_poly_extrude_mut() {
            Some(d) => {
                let was = d.alt_was_down;
                d.alt_was_down = alt;
                was
            }
            None => return,
        };
        if alt && !alt_was_down {
            self.poly_extrude_free_grab_handoff(mouse);
            return;
        }

        // Hold Shift = world-axis lock (must live here — viewport path always runs mid-drag).
        self.poly_extrude_set_world_axis(shift);

        // `face_normals` is NOT cloned: only its average was ever needed here, so it is
        // summed in place under the borrow. `new_face_ids` / `vert_normals` still are —
        // both are handed to `poly_extrude_apply_delta(&mut self, ..)` below, which itself
        // reads `self.tool.current`, so they cannot stay borrowed out of the drag state.
        let (nid, avg_normal, new_faces, start, prev_offset, along_normals, vert_normals, world_axis, edge_axis) = {
            let d = match self.tool.current.as_poly_extrude() { Some(d) => d, None => return };
            let sum: Vec3 = d.face_normals.iter().map(|(_, n)| *n).sum();
            (
                d.node_id,
                sum.normalize_or_zero(),
                d.new_face_ids.clone(),
                d.start_mouse,
                d.accumulated_offset,
                d.along_normals,
                d.vert_normals.clone(),
                d.world_axis,
                d.edge_axis,
            )
        };

        if avg_normal == Vec3::ZERO && !along_normals { return; }
        let avg_normal = Self::extrude_effective_dir(avg_normal, world_axis, edge_axis);

        let cam_pos = self.viewport.camera.target + self.viewport.camera.orbit * Vec3::new(0.0, 0.0, self.viewport.camera.distance);
        let aspect = rect.width() / rect.height().max(1.0);
        let ortho_hw = if self.render.ortho { self.viewport.camera.distance } else { 0.0 };

        let centroid = {
            let mut sum = Vec3::ZERO;
            let mut count = 0u32;
            if let Some(node) = self.scene.scene_graph.get(nid) {
                if let NodePayload::PolygonalBody { mesh, .. } = &node.payload {
                    // ONE ring buffer for the whole cap loop — `face_vertices` allocates a
                    // fresh Vec per face and this runs every drag frame.
                    let mut ring: Vec<VertexId> = Vec::new();
                    for &fid in &new_faces {
                        mesh.face_vertices_into(FaceId(fid), &mut ring);
                        for &v in &ring {
                            sum += mesh.vert(v).position;
                            count += 1;
                        }
                    }
                }
            }
            if count > 0 { sum / count as f32 } else { Vec3::ZERO }
        };

        // Screen projection axis: region average (same for Alt+E — measures offset amount only).
        let guide = if avg_normal.length_squared() > 0.25 { avg_normal } else { Vec3::Y };
        let s0 = world_to_screen(centroid, cam_pos, self.viewport.camera.orbit, rect, aspect, ortho_hw, self.render.fov_deg);
        let s1 = world_to_screen(centroid + guide * 0.5, cam_pos, self.viewport.camera.orbit, rect, aspect, ortho_hw, self.render.fov_deg);
        let new_offset = if let (Some(p0), Some(p1)) = (s0, s1) {
            let screen_dir = egui::vec2(p1.x - p0.x, p1.y - p0.y);
            let slen = (screen_dir.x * screen_dir.x + screen_dir.y * screen_dir.y).sqrt();
            if slen < 1.0 { prev_offset } else {
                let sn = egui::vec2(screen_dir.x / slen, screen_dir.y / slen);
                let dx = mouse.x - start.x;
                let dy = mouse.y - start.y;
                let projected = dx * sn.x + dy * sn.y;
                let raw = projected * self.viewport.camera.distance * self.tweak.grab_speed;
                if self.is_snap_active(ctrl) {
                    let grid = self.viewport.grid_cell_size.max(0.01);
                    (raw / grid).round() * grid
                } else { raw }
            }
        } else { prev_offset };

        let delta = new_offset - prev_offset;
        if delta.abs() > 1e-7 {
            let t_d = std::time::Instant::now();
            self.poly_extrude_apply_delta(nid, &new_faces, avg_normal, &vert_normals, along_normals, delta);
            delta_ms = t_d.elapsed().as_secs_f32() * 1000.0;
            // Topology already built at drag start — positions only (P3).
            let t_s = std::time::Instant::now();
            self.poly_sync_after_position_edit(nid);
            sync_ms = t_s.elapsed().as_secs_f32() * 1000.0;
            self.mark_dirty(DirtyFlags::RENDER);
        }

        if let Some(d) = self.tool.current.as_poly_extrude_mut() {
            d.accumulated_offset = new_offset;
        }

        // Accumulate for frame-end [EXTRUDE_PERF].
        self.extrude_perf_calls = self.extrude_perf_calls.saturating_add(1);
        self.extrude_perf_delta_ms += delta_ms;
        self.extrude_perf_sync_ms += sync_ms;
        self.extrude_perf_total_ms += _t_total.elapsed().as_secs_f32() * 1000.0;
    }

    /// The ONE place live-extrude cap vertices move. `along_normals` = per-vertex frozen
    /// normals (Alt+E), else rigid cap along the averaged region normal (E default).
    /// Shared by the per-frame drag update, Tool Params depth field, and N-mode toggle.
    ///
    /// When `force_planar` is set (Along Normals option), cap faces are re-projected onto
    /// their frozen plane after the slide so they stay straight (owner FORCE STRAITH FACE).
    pub(crate) fn poly_extrude_apply_delta(
        &mut self,
        nid: core_math::scene_graph::NodeId,
        new_faces: &[u32],
        avg_normal: Vec3,
        vert_normals: &[(u32, Vec3)],
        along_normals: bool,
        delta: f32,
    ) {
        let force_planar = self
            .tool
            .current
            .as_poly_extrude()
            .map(|d| d.force_planar && d.along_normals)
            .unwrap_or(false);
        let face_normals: Vec<(u32, Vec3)> = if force_planar {
            self.tool
                .current
                .as_poly_extrude()
                .map(|d| d.face_normals.clone())
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        self.poly_with_base_mesh_mut(nid, |mesh| {
            if along_normals && !vert_normals.is_empty() {
                // Alt+E: each cap vertex along its frozen normal (face-constant when force_planar).
                for &(vid, n) in vert_normals {
                    let v = VertexId(vid);
                    if (v.0 as usize) < mesh.vertices.len() {
                        mesh.vert_mut(v).position += n * delta;
                    }
                }
                if force_planar && !face_normals.is_empty() {
                    Self::poly_extrude_flatten_caps(mesh, &face_normals);
                }
            } else {
                // E default: unique cap verts once along averaged region normal (rigid cap).
                let mut moved: HashSet<VertexId> = HashSet::new();
                // ONE ring buffer for the whole cap loop (same reason as the drag-frame
                // centroid walk): this is the default-E per-frame slide.
                let mut ring: Vec<VertexId> = Vec::new();
                for &fid in new_faces {
                    mesh.face_vertices_into(FaceId(fid), &mut ring);
                    for &v in &ring {
                        if moved.insert(v) {
                            mesh.vert_mut(v).position += avg_normal * delta;
                        }
                    }
                }
            }
        });
    }

    /// Toggle Force planar mid-drag (Along Normals). Rebuilds slide vectors; when turning ON,
    /// snaps current caps to planar immediately (owner FORCE STRAITH FACE).
    pub(crate) fn poly_extrude_set_force_planar(&mut self, on: bool) {
        let (nid, face_normals) = match self.tool.current.as_poly_extrude() {
            Some(d) if d.along_normals => (d.node_id, d.face_normals.clone()),
            _ => return,
        };
        if self
            .tool
            .current
            .as_poly_extrude()
            .map(|d| d.force_planar == on)
            .unwrap_or(true)
        {
            return;
        }
        let new_verts = match self.poly_node_mesh(nid) {
            Some(m) => Self::poly_extrude_face_constant_normals(&m, &face_normals),
            None => return,
        };
        if let Some(d) = self.tool.current.as_poly_extrude_mut() {
            d.force_planar = on;
            d.vert_normals = new_verts;
        }
        if on {
            self.poly_with_base_mesh_mut(nid, |mesh| {
                Self::poly_extrude_flatten_caps(mesh, &face_normals);
            });
        }
        self.poly_sync_after_position_edit(nid);
        self.mark_dirty(DirtyFlags::RENDER);
        self.toast(if on {
            "Force planar faces: ON — caps stay straight"
        } else {
            "Force planar faces: OFF — free along normals"
        });
    }

    /// N during the extrude modal — toggle Region Normal ⇄ Along Normals without restarting
    /// the drag. Reverts the applied offset with the OLD mode, flips, reapplies with the NEW
    /// mode, so the accumulated scalar offset carries over exactly.
    pub(crate) fn poly_extrude_toggle_along_normals(&mut self) {
        // Release an active D edge-pick first (revert its offsets onto the region normal
        // so the mode math below starts from a consistent direction).
        {
            let st = self.tool.current.as_poly_extrude().map(|d| {
                (d.node_id, d.face_normals.clone(), d.new_face_ids.clone(),
                 d.accumulated_offset, d.vert_normals.clone(), d.world_axis, d.edge_axis)
            });
            if let Some((nid, normals, new_faces, prev_offset, vert_normals, world_axis, Some(_))) = st {
                let avg = {
                    let sum: Vec3 = normals.iter().map(|(_, n)| *n).sum();
                    sum.normalize_or_zero()
                };
                if avg != Vec3::ZERO {
                    let edge_axis = self.tool.current.as_poly_extrude().and_then(|d| d.edge_axis);
                    let old_dir = Self::extrude_effective_dir(avg, world_axis, edge_axis);
                    let new_dir = Self::extrude_effective_dir(avg, world_axis, None);
                    if prev_offset.abs() > 1e-7 {
                        self.poly_extrude_apply_delta(nid, &new_faces, old_dir, &vert_normals, false, -prev_offset);
                        self.poly_extrude_apply_delta(nid, &new_faces, new_dir, &vert_normals, false, prev_offset);
                        self.poly_sync_after_position_edit(nid);
                    }
                    if let Some(d) = self.tool.current.as_poly_extrude_mut() { d.edge_axis = None; }
                }
            }
        }
        let (nid, normals, new_faces, prev_offset, along_normals, vert_normals) = {
            let d = match self.tool.current.as_poly_extrude() { Some(d) => d, None => return };
            (
                d.node_id,
                d.face_normals.clone(),
                d.new_face_ids.clone(),
                d.accumulated_offset,
                d.along_normals,
                d.vert_normals.clone(),
            )
        };
        let new_mode = !along_normals;
        let avg_normal = {
            let sum: Vec3 = normals.iter().map(|(_, n)| *n).sum();
            sum.normalize_or_zero()
        };
        // Region mode needs a usable averaged direction; refuse rather than collapse the cap.
        if !new_mode && avg_normal == Vec3::ZERO {
            self.toast("Extrude: no region normal (degenerate faces)");
            return;
        }
        if prev_offset.abs() > 1e-7 {
            self.poly_extrude_apply_delta(nid, &new_faces, avg_normal, &vert_normals, along_normals, -prev_offset);
            self.poly_extrude_apply_delta(nid, &new_faces, avg_normal, &vert_normals, new_mode, prev_offset);
            self.poly_sync_after_position_edit(nid);
            self.mark_dirty(DirtyFlags::RENDER);
        }
        let planar_after_n: Option<(core_math::scene_graph::NodeId, Vec<(u32, Vec3)>)> = {
            if let Some(d) = self.tool.current.as_poly_extrude_mut() {
                d.along_normals = new_mode;
                // N only flips the slide mode on the CURRENT topology. Region shells become
                // "grouped" along-normals; individual shells stay individual.
                if new_mode {
                    // Region topo from plain E: force grouped so G/I labels match reality.
                    if !along_normals {
                        d.along_normals_grouped = true;
                    }
                    // Owner FORCE STRAITH FACE: default planar when entering Along Normals.
                    d.force_planar = true;
                    Some((d.node_id, d.face_normals.clone()))
                } else {
                    d.force_planar = false;
                    None
                }
            } else {
                None
            }
        };
        if let Some((nid, face_normals)) = planar_after_n {
            let new_verts = self
                .poly_node_mesh(nid)
                .map(|m| Self::poly_extrude_face_constant_normals(&m, &face_normals));
            if let Some(vn) = new_verts {
                if let Some(d) = self.tool.current.as_poly_extrude_mut() {
                    d.vert_normals = vn;
                }
            }
            self.poly_with_base_mesh_mut(nid, |mesh| {
                Self::poly_extrude_flatten_caps(mesh, &face_normals);
            });
            self.poly_sync_after_position_edit(nid);
        }
        self.ui2.active_tool = if new_mode {
            "Extrude Along Normals".into()
        } else {
            "Extrude".into()
        };
        self.toast(if new_mode {
            "Extrude: Along Normals · Force planar ON · G=Group · I=Individual"
        } else {
            "Extrude: Region Normal"
        });
    }

    /// D mid-drag (owner 2026-08-03, Punch-It parity: "punch it I select an edge — we have
    /// the proper path to extrude perfectly"): lock the extrude direction to the mesh edge
    /// at the region boundary best aligned with the current direction. Press D again to
    /// release back to the region normal.
    /// D HELD (Punch-It get_edge_dir parity): pick the extrude direction from the edge
    /// nearest the cursor on the PRE-drag mesh (screen-space, knife-style). Called every
    /// frame while D is down; the last valid pick stays after release.
    pub(crate) fn poly_extrude_pick_edge_dir(&mut self, mouse: egui::Pos2, rect: egui::Rect) {
        let (nid, normals, new_faces, prev_offset, vert_normals, world_axis, edge_axis, pre) = {
            let d = match self.tool.current.as_poly_extrude() { Some(d) => d, None => return };
            (
                d.node_id,
                d.face_normals.clone(),
                d.new_face_ids.clone(),
                d.accumulated_offset,
                d.vert_normals.clone(),
                d.world_axis,
                d.edge_axis,
                d.pre_mesh.clone(),
            )
        };
        let Some(pre) = pre else { return };
        let avg = {
            let sum: Vec3 = normals.iter().map(|(_, n)| *n).sum();
            sum.normalize_or_zero()
        };
        if avg == Vec3::ZERO {
            return;
        }
        let world = self.scene.scene_graph.world_transform(nid);
        let cam_pos = self.viewport.camera.target
            + self.viewport.camera.orbit * Vec3::new(0.0, 0.0, self.viewport.camera.distance);
        let aspect = rect.width() / rect.height().max(1.0);
        let ortho_hw = if self.render.ortho { self.viewport.camera.distance } else { 0.0 };
        let orbit = self.viewport.camera.orbit;
        let fov = self.render.fov_deg;
        let m = &*pre;
        let mut best: Option<(f32, Vec3)> = None;
        for i in 0..m.half_edges.len() as u32 {
            let he = core_math::half_edge::HalfEdgeId(i);
            if !m.is_edge_live(he) {
                continue;
            }
            let tw = m.he(he).twin;
            if tw != core_math::half_edge::NO_TWIN && tw.0 < i {
                continue;
            }
            let a = world.transform_point3(m.vert(m.he_source(he)).position);
            let b = world.transform_point3(m.vert(m.he(he).vertex).position);
            let (Some(sa), Some(sb)) = (
                world_to_screen(a, cam_pos, orbit, rect, aspect, ortho_hw, fov),
                world_to_screen(b, cam_pos, orbit, rect, aspect, ortho_hw, fov),
            ) else {
                continue;
            };
            let ab = sb - sa;
            let t = ((mouse - sa).x * ab.x + (mouse - sa).y * ab.y)
                / (ab.x * ab.x + ab.y * ab.y).max(1e-10);
            let proj = egui::pos2(sa.x + ab.x * t.clamp(0.0, 1.0), sa.y + ab.y * t.clamp(0.0, 1.0));
            let d2 = (mouse - proj).length();
            if d2 > 40.0 {
                continue;
            }
            let dir = (m.vert(m.he(he).vertex).position - m.vert(m.he_source(he)).position)
                .normalize_or_zero();
            if dir == Vec3::ZERO || dir.dot(avg).abs() < 0.05 {
                continue; // perpendicular to travel — not a usable path (Punch It: dot==0 invalid)
            }
            if best.map_or(true, |(bd, _)| d2 < bd) {
                best = Some((d2, if dir.dot(avg) < 0.0 { -dir } else { dir }));
            }
        }
        let Some((_, new_axis)) = best else { return };
        if edge_axis == Some(new_axis) {
            return;
        }
        let old_dir = Self::extrude_effective_dir(avg, world_axis, edge_axis);
        let new_dir = Self::extrude_effective_dir(avg, world_axis, Some(new_axis));
        if prev_offset.abs() > 1e-7 && old_dir != new_dir {
            self.poly_extrude_apply_delta(nid, &new_faces, old_dir, &vert_normals, false, -prev_offset);
            self.poly_extrude_apply_delta(nid, &new_faces, new_dir, &vert_normals, false, prev_offset);
            self.poly_sync_after_position_edit(nid);
            self.mark_dirty(crate::DirtyFlags::RENDER);
        }
        if let Some(d) = self.tool.current.as_poly_extrude_mut() { d.edge_axis = Some(new_axis); }
        self.toast("Extrude: Edge Align (hold D + hover an edge; N resets)");
    }

    /// Weld the push-through's epsilon DEBRIS — the intrude tool's OWN cleanup pass.
    ///
    /// Owner 2026-08-11: *"my intrude from extrude tool no longer works properly, it's supposed to
    /// clean the plane on the side."* A through-cut leaves clusters of near-duplicate vertices
    /// where the cutter grazes an existing corner. They are far too small to see, but they pin the
    /// flat side planes into triangle fans, folded coplanar pairs and the occasional zero-area
    /// face — and because every cut takes the previous result as its input, the debris COMPOUNDS,
    /// which is why the tool degrades rather than failing outright.
    ///
    /// **Why this is here and not a tweak to the boolean kernel.** `manifold_boolean` already has
    /// `collapse_epsilon_debris`, but it is capped at `bbox·1e-5` and gated on preserving every
    /// input vertex. Measured on the owner's own capture, that epsilon (2.9e-5) welds *nothing*:
    /// the debris sits at 2.3e-4, an order of magnitude above it, and the debris verts ARE input
    /// verts of the next cut, so the preserve gate would refuse the weld even if the epsilon
    /// reached. Both limits are deliberate — "a boolean must not change geometry it did not cut" —
    /// and loosening them would change every boolean in the app. So the TOOL that knows it just
    /// pushed a prism through its own solid owns this pass, with its own setting.
    ///
    /// Fails CLOSED: any gate that trips returns the unwelded result rather than a damaged mesh.
    /// Authored edges (Smart Select tags / sharp / seam / crease) are protected and never welded —
    /// `keyed_on` is the mesh the sidecar's ids belong to (the node's base mesh, pre-cut).
    pub(crate) fn poly_extrude_merge_debris(
        &self,
        nid: core_math::scene_graph::NodeId,
        keyed_on: &core_math::half_edge::HalfEdgeMesh,
        result: core_math::half_edge::HalfEdgeMesh,
    ) -> core_math::half_edge::HalfEdgeMesh {
        use core_math::mesh_boolean::{boundary_half_edge_count, cleanup_mesh, mesh_signed_volume};

        let frac = self.tweak.extrude_debris_merge;
        if !(frac > 0.0) {
            return result;
        }
        let (bmin, bmax) = result.bounding_box();
        let thr = (bmax - bmin).length() * frac;
        if !(thr > 0.0) {
            return result;
        }
        // Authored intent, as endpoint POSITIONS — the weld rebuilds, so ids die but geometry
        // survives. Same currency `CleanupOptions::protected_edges` speaks.
        let protected: Vec<(Vec3, Vec3)> = self
            .poly_protected_edges(nid, keyed_on)
            .into_iter()
            .map(|h| {
                (
                    keyed_on.vert(keyed_on.he_source(h)).position,
                    keyed_on.vert(keyed_on.he(h).vertex).position,
                )
            })
            .collect();

        let vol0 = mesh_signed_volume(&result);
        let mut cand = result.clone();
        let welded = cand.merge_by_distance_ex(thr, &protected);
        if welded == 0 {
            return result;
        }
        // Welding removes the pins; the coplanar dissolve is what actually restores the side plane
        // to ONE face. Shared implementation with Mesh Cleanup on purpose (one dissolve in the
        // codebase) — only the THRESHOLD above is this tool's own.
        let cleaned = cleanup_mesh(&cand).unwrap_or(cand);
        let vol_ok = (mesh_signed_volume(&cleaned) - vol0).abs() <= (vol0.abs() * 5e-3).max(1e-9);
        let ok = cleaned.validate().is_ok()
            && cleaned.is_manifold().is_ok()
            && boundary_half_edge_count(&cleaned) == 0
            && cleaned.face_count() <= result.face_count()
            && vol_ok;
        if !ok {
            eprintln!(
                "[EXTRUDE-PT] debris merge REFUSED (thr={thr:.6}): valid={:?} manifold={:?} \
                 boundary={} f={}->{} vol_ok={vol_ok}",
                cleaned.validate().is_ok(), cleaned.is_manifold().is_ok(),
                boundary_half_edge_count(&cleaned), result.face_count(), cleaned.face_count()
            );
            return result;
        }
        eprintln!(
            "[EXTRUDE-PT] debris merge: thr={thr:.6} welded={welded} v={}->{} f={}->{}",
            result.vertex_count(), cleaned.vertex_count(),
            result.face_count(), cleaned.face_count()
        );
        cleaned
    }

    /// SketchUp push/pull parity — merge stacked coplanar planes on confirm (owner 2026-08-03).
    /// The geometry lives in the kernel (`HalfEdgeMesh::merge_coplanar_extrude_walls`,
    /// ops_dissolve.rs) so it is unit-testable; this wrapper only routes it through the base
    /// mesh. Runs INSIDE the confirm episode so one undo restores the whole extrude.
    fn poly_extrude_merge_stacked_walls(&mut self, nid: core_math::scene_graph::NodeId, cap_faces: &[u32]) -> (usize, usize) {
        self.poly_with_base_mesh_mut(nid, |mesh| {
            let caps: HashSet<FaceId> = cap_faces.iter().map(|&f| FaceId(f)).collect();
            mesh.merge_coplanar_extrude_walls(&caps)
        }).unwrap_or((0, 0))
    }

    /// CAD push-through (owner 2026-08-03, screenshots: "the tool works until I pass the
    /// last edge"): an INWARD extrude pushed TO or PAST the local far wall stops being a
    /// topological extrude — the swept volume must CUT the body (Plasticity/SketchUp
    /// push-pull), consuming the material the cap traveled through. At exact wall contact
    /// this is simultaneously the owner's "two faces perfectly on top of each other +
    /// subtract" case — the boolean's coincident-face provenance table resolves it.
    ///
    /// Detection: raycast each ORIGINAL cap-ring vertex (pre-drag position, reconstructed
    /// as current − n·offset) along the push direction against the PRE-drag mesh; if the
    /// travel reaches the nearest opposing surface (within eps), switch to the boolean
    /// path: subtract the swept prism (slight overshoot both ends so kissing faces cut
    /// cleanly) from the PRE-drag mesh and install that instead. One undo = pre-extrude.
    ///
    /// Returns true when the boolean path fully handled the commit (caller returns).
    /// Region-normal mode only; Along-Normals through-pushes keep the topological result.
    /// HEADLESS CORE of the push-through (owner 2026-08-04: "you will test the intrude").
    /// Everything from boundary-loop tracing to the installable gate, with ZERO app state:
    /// callable from an offline test with a loaded .fl4 so the EXACT app computation runs
    /// under instrumentation. The wrapper below feeds it from drag state and installs.
    pub(crate) fn push_through_core(
        pre_mesh: &core_math::half_edge::HalfEdgeMesh,
        cur: &core_math::half_edge::HalfEdgeMesh,
        new_faces: &[u32],
        face_n: Vec3,
        n: Vec3,
        offset: f32,
        force: bool,
    ) -> Result<core_math::half_edge::HalfEdgeMesh, String> {
        use core_math::mesh_boolean::{mesh_boolean, BoolOp};
        use core_math::poly_mesh::earclip_polygon;
        let push = -offset; // depth traveled INTO the solid, along -n
        let push_dir = -n;
        // ONE cutter from the region's OUTER BOUNDARY LOOP (owner algorithm 2026-08-04:
        // "take the selected face, clean up the face with the cut, extrude it, subtract").
        // Previously this built a SEPARATE prism per cap face and subtracted each in turn —
        // so a multi-face region produced prisms whose side walls were mutually FLUSH, and
        // every boolean after the first cut into coplanar overlapping segments.
        //
        // SMART BOUNDARY (owner TEST_020 2026-08-06): a multi-face / rim-touch / knifed region
        // used to fail closed with "not a single simple loop" and fall back to the raw
        // topological extrude (stacked slivers, open-looking pocket). We now:
        //   1. walk EVERY boundary cycle (multimap — pinched verts allowed),
        //   2. keep the longest cycle per connected component of cap faces,
        //   3. emit one prism ring per component (chained subtract).
        // Positions are the PRE-drag ones (current − n·offset; region mode moves the cap
        // rigidly, so this is exact).
        let rings: Vec<Vec<Vec3>> = {
            let mesh = cur;
            let live: Vec<u32> = new_faces
                .iter()
                .copied()
                .filter(|&f| {
                    (f as usize) < mesh.faces.len()
                        && mesh.face(FaceId(f)).half_edge != core_math::half_edge::NO_EDGE
                })
                .collect();
            if live.is_empty() {
                return Err("skip: no live cap faces".to_string());
            }
            // Connected components of caps (share an undirected edge).
            let mut adj: std::collections::HashMap<u32, Vec<u32>> = std::collections::HashMap::new();
            let live_set: std::collections::HashSet<u32> = live.iter().copied().collect();
            for &f in &live {
                for he in mesh.face_half_edges(FaceId(f)) {
                    let tw = mesh.he(he).twin;
                    if tw == core_math::half_edge::NO_TWIN {
                        continue;
                    }
                    let of = mesh.he(tw).face;
                    if of != core_math::half_edge::NO_FACE && live_set.contains(&of.0) && of.0 != f {
                        adj.entry(f).or_default().push(of.0);
                    }
                }
            }
            let mut remaining: std::collections::HashSet<u32> = live_set.clone();
            let mut components: Vec<Vec<u32>> = Vec::new();
            while let Some(&seed) = remaining.iter().next() {
                let mut stack = vec![seed];
                let mut comp = Vec::new();
                remaining.remove(&seed);
                while let Some(f) = stack.pop() {
                    comp.push(f);
                    for &nb in adj.get(&f).into_iter().flatten() {
                        if remaining.remove(&nb) {
                            stack.push(nb);
                        }
                    }
                }
                components.push(comp);
            }
            // Longest simple cycle(s) from a directed-edge multimap boundary of `faces`.
            let cycles_of = |faces: &[u32]| -> Vec<Vec<u32>> {
                let mut dir: std::collections::HashSet<(u32, u32)> = std::collections::HashSet::new();
                for &f in faces {
                    let vs = mesh.face_vertices(FaceId(f));
                    for k in 0..vs.len() {
                        dir.insert((vs[k].0, vs[(k + 1) % vs.len()].0));
                    }
                }
                // Multimap: pinched rim verts may have degree > 1 (TEST_020 edge case).
                let mut outs: std::collections::HashMap<u32, Vec<u32>> =
                    std::collections::HashMap::new();
                for &(a, b) in &dir {
                    if !dir.contains(&(b, a)) {
                        outs.entry(a).or_default().push(b);
                    }
                }
                if outs.is_empty() {
                    return Vec::new();
                }
                let mut unused: std::collections::HashSet<(u32, u32)> =
                    std::collections::HashSet::new();
                for (a, bs) in &outs {
                    for &b in bs {
                        unused.insert((*a, b));
                    }
                }
                let mut cycles: Vec<Vec<u32>> = Vec::new();
                while let Some(&(start_a, start_b)) = unused.iter().next() {
                    unused.remove(&(start_a, start_b));
                    let mut ring = vec![start_a];
                    let mut cur_v = start_b;
                    let mut ok = true;
                    for _ in 0..unused.len() + 2 {
                        ring.push(cur_v);
                        if cur_v == start_a {
                            break;
                        }
                        // Prefer an unused outgoing edge; close if we can return to start.
                        let choices = outs.get(&cur_v).cloned().unwrap_or_default();
                        let mut nxt = None;
                        for &b in &choices {
                            if b == start_a && ring.len() >= 3 {
                                nxt = Some(b);
                                unused.remove(&(cur_v, b));
                                break;
                            }
                        }
                        if nxt.is_none() {
                            for &b in &choices {
                                if unused.remove(&(cur_v, b)) {
                                    nxt = Some(b);
                                    break;
                                }
                            }
                        }
                        match nxt {
                            Some(b) => cur_v = b,
                            None => {
                                ok = false;
                                break;
                            }
                        }
                    }
                    if ok && ring.len() >= 4 && ring.first() == ring.last() {
                        ring.pop(); // drop duplicate close
                        if ring.len() >= 3 {
                            cycles.push(ring);
                        }
                    }
                }
                cycles
            };
            let mut all_loops: Vec<Vec<u32>> = Vec::new();
            for comp in &components {
                let mut cycles = cycles_of(comp);
                // Prefer the longest cycle (outer boundary); drop tiny/spurious ones.
                cycles.sort_by_key(|c| std::cmp::Reverse(c.len()));
                if let Some(outer) = cycles.into_iter().next() {
                    all_loops.push(outer);
                } else {
                    // Last resort: each cap face's own ring (prism per face).
                    for &f in comp {
                        let vs = mesh.face_vertices(FaceId(f));
                        if vs.len() >= 3 {
                            all_loops.push(vs.iter().map(|v| v.0).collect());
                        }
                    }
                }
            }
            if all_loops.is_empty() {
                return Err("skip: region boundary is not a single simple loop".to_string());
            }
            let mut rings_out: Vec<Vec<Vec3>> = Vec::new();
            for loop_ids in all_loops {
                let pts: Vec<Vec3> = loop_ids
                    .iter()
                    .map(|&v| mesh.vert(core_math::half_edge::VertexId(v)).position - n * offset)
                    .collect();
                if pts.len() >= 3 {
                    rings_out.push(pts);
                }
            }
            if rings_out.is_empty() {
                return Err("skip: could not read boundary positions".to_string());
            }
            rings_out
        };

        // EXACT-RING SNAP (parity with Blender EXACT): the reconstruction above lands
        // ~1e-6 off the true pre-drag positions (float round-trip through the drag), and
        // that offset is what creates epsilon-thin sliver walls in the subtract. Every cap
        // ring vertex IS an original mesh vertex — snap each to its nearest pre-mesh
        // vertex so the prism sides are EXACTLY coplanar/incident with the original faces
        // (the case the kernel resolves cleanly; proven by flush_prism_subtract e=0).
        let rings: Vec<Vec<Vec3>> = {
            let m = pre_mesh;
            let live: Vec<Vec3> = (0..m.vertices.len() as u32)
                .map(core_math::half_edge::VertexId)
                .filter(|&v| m.vert(v).half_edge != core_math::half_edge::NO_EDGE)
                .map(|v| m.vert(v).position)
                .collect();
            rings
                .into_iter()
                .map(|ring| {
                    // Scale the snap radius to the RING (owner 2026-08-04 "basic flat cut
                    // collapses edges"): a fixed push-relative eps could exceed short ring
                    // edges and snap two ring verts onto the SAME mesh vertex, collapsing
                    // prism sides. Cap at 20% of the shortest ring edge and refuse
                    // duplicate targets outright.
                    let m = ring.len();
                    let mut min_edge = f32::MAX;
                    for k in 0..m {
                        min_edge = min_edge.min(ring[k].distance(ring[(k + 1) % m]));
                    }
                    // Depth-independent (owner test-05: two drags of different depths must
                    // build the IDENTICAL clamped prism; a push-scaled eps made depth 3.5
                    // snap differently from 2.5 and fail where 2.5 succeeded).
                    let snap_eps = (min_edge * 0.05).max(1e-5);
                    let snapped: Vec<Vec3> = ring
                        .iter()
                        .map(|&p| {
                            let mut best = p;
                            let mut best_d = snap_eps * snap_eps;
                            for &q in &live {
                                let d = p.distance_squared(q);
                                if d < best_d {
                                    best_d = d;
                                    best = q;
                                }
                            }
                            best
                        })
                        .collect();
                    // Any two ring verts landing on one target = collapse — keep original.
                    let mut dup = false;
                    for i in 0..m {
                        for j in (i + 1)..m {
                            if snapped[i].distance_squared(snapped[j]) < 1e-12 {
                                dup = true;
                            }
                        }
                    }
                    if dup { ring } else { snapped }
                })
                .collect()
        };

        // Local thickness: nearest pre-mesh surface along the push, from ring verts +
        // centroids (rays start a hair inside so the cap's own plane never self-hits).
        // Ring-scale (depth-independent) epsilon — see the snap_eps note above.
        let eps = {
            let (mut mn, mut mx) = (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN));
            for ring in &rings {
                for p in ring {
                    mn = mn.min(*p);
                    mx = mx.max(*p);
                }
            }
            ((mx - mn).length().max(1e-3) * 1e-3).max(1e-4)
        };
        let mut min_t = f32::MAX;
        let mut max_t = 0.0f32;
        {
            let m = pre_mesh;
            let mut cast = |origin: Vec3| {
                let ro = origin + push_dir * eps;
                for i in 0..m.faces.len() as u32 {
                    let fid = FaceId(i);
                    if m.face(fid).half_edge == core_math::half_edge::NO_EDGE {
                        continue;
                    }
                    let fv = m.face_vertices(fid);
                    if fv.len() < 3 {
                        continue;
                    }
                    let pts: Vec<Vec3> = fv.iter().map(|&v| m.vert(v).position).collect();
                    let fnorm = m.compute_face_normal(fid);
                    for t in earclip_polygon(&pts, fnorm) {
                        if let Some(hit_t) = crate::poly_selection::ray_triangle(
                            ro,
                            push_dir,
                            pts[t[0] as usize],
                            pts[t[1] as usize],
                            pts[t[2] as usize],
                        ) {
                            if hit_t > eps {
                                min_t = min_t.min(hit_t + eps);
                                max_t = max_t.max(hit_t + eps);
                            }
                        }
                    }
                }
            };
            for ring in &rings {
                let mut centroid = Vec3::ZERO;
                for p in ring {
                    cast(*p);
                    centroid += *p;
                }
                cast(centroid / ring.len().max(1) as f32);
            }
        }
        let through = push + eps >= min_t;
        if !through && !force {
            return Err(format!(
                "inside wall: push={push:.4} min_t={min_t:.4} — topological pocket kept"
            ));
        }
        // CLAMP TO THE FARTHEST EXIT (owner test-05, 2026-08-04: full pierce "completely
        // off the island" must work). Any depth beyond the last surface the sweep crosses
        // produces IDENTICAL geometry — the material is consumed to the exit either way —
        // but the extra prism length grazes far-side faces flush and manufactures the
        // epsilon-debris folds that made deep drags refuse. Cut with the shortest prism
        // that does the same job.
        let push = if max_t > 0.0 { push.min(max_t) } else { push };
        eprintln!("[EXTRUDE-PT] THROUGH: push={push:.4} min_t={min_t:.4} max_t={max_t:.4} rings={} — boolean path", rings.len());

        // Through (or kissing): subtract the swept prism(s) from the PRE-drag mesh.
        let overshoot = (push * 0.01).max(eps * 4.0);
        // Distance to travel ALONG the sweep so the entry cap clears the (possibly tilted)
        // surface plane by `overshoot`. cos = |n · face_n| — 1.0 on a flat face.
        let entry_lift = {
            let cos = n.dot(face_n).abs().max(0.15); // 0.15 caps a ~81° grazing sweep
            overshoot / cos
        };
        let end_over = if through { overshoot } else { 0.0 };
        let mut result = pre_mesh.clone();
        for ring in &rings {
            let m = ring.len();
            if m < 3 {
                continue;
            }
            // PUNCH-IT PUSH (MACHIN3 set_push_and_pull_amount, studied 2026-08-03): widen
            // the cutter laterally per boundary vertex along push_dir = inward_normal x
            // boundary_tangent, scaled by sqrt(region area). The prism sides then OVERLAP
            // the surrounding walls instead of kissing them — flush/sliver boolean cases
            // cannot occur by construction. (Their unit = sqrt(area)*1e-5, users dial
            // ~100 when needed; we bake that in.)
            let area = {
                let mut nv = Vec3::ZERO;
                for k in 0..m {
                    let (a, b) = (ring[k], ring[(k + 1) % m]);
                    nv += Vec3::new(
                        (a.y - b.y) * (a.z + b.z),
                        (a.z - b.z) * (a.x + b.x),
                        (a.x - b.x) * (a.y + b.y),
                    );
                }
                (nv.length() * 0.5).max(1e-12)
            };
            // DEFAULT 0 (correction 2026-08-04): Punch It ships pushed=0 and relies on the
            // exact kernel; baking a nonzero push made the cutter shave epsilon grooves
            // into every NEIGHBOUR wall on ordinary intrudes (owner: "overlapping
            // segments, breaks support edges"). Exact coplanarity via the ring snap is
            // the correct default; push stays available as an escape hatch via env.
            let push_off = std::env::var("AZRA_EXTRUDE_PUSH")
                .ok()
                .and_then(|v| v.parse::<f32>().ok())
                .map(|k| area.sqrt() * 1e-5 * k)
                .unwrap_or(0.0);
            let ring_pushed: Vec<Vec3> = (0..m)
                .map(|k| {
                    let prev = ring[(k + m - 1) % m];
                    let next = ring[(k + 1) % m];
                    let fwd = (next - prev).normalize_or_zero();
                    let pd = (-n).cross(fwd).normalize_or_zero();
                    ring[k] + pd * push_off
                })
                .collect();
            let mut pos: Vec<Vec3> = Vec::with_capacity(2 * m);
            for p in &ring_pushed {
                // TILTED-FACE GEOMETRY (owner 2026-08-04: "on a flat surface it works, on a
                // tilted surface it fails — trace where the difference is"). The entry cap
                // must (a) clear the surface plane and (b) keep the prism's sides PARALLEL
                // to the sweep, or the opening lands offset from the region the user drew.
                // Travelling along `face_n` satisfies (a) but breaks (b) — it shears the
                // prism sideways by `overshoot * tan(tilt)`. Travelling along `n` keeps (b)
                // but on a tilted face clears the plane by only `overshoot * (n·face_n)`.
                // Correct: move along `n`, scaled so the plane clearance is exactly
                // `overshoot`. On a flat face n·face_n = 1 and this is the old behaviour —
                // which is precisely why flat worked and tilted did not.
                pos.push(*p + n * entry_lift);
            }
            for p in &ring_pushed {
                pos.push(*p - n * (push + end_over)); // through: slightly past; pocket: exact
            }
            // Cap winding: the face ring is CCW seen along +n (its outward normal), so the
            // +n cap keeps ring order, the -n cap reverses, sides pair them.
            let tris = earclip_polygon(&ring.iter().map(|p| *p).collect::<Vec<_>>(), n);
            if tris.is_empty() {
                return Err("degenerate ring — ear-clip failed".to_string());
            }
            let mut faces: Vec<Vec<usize>> = Vec::with_capacity(tris.len() * 2 + m);
            for t in &tris {
                faces.push(vec![t[0] as usize, t[1] as usize, t[2] as usize]);
            }
            for t in &tris {
                faces.push(vec![t[2] as usize + m, t[1] as usize + m, t[0] as usize + m]);
            }
            for k in 0..m {
                let j = (k + 1) % m;
                faces.push(vec![j, k, k + m, j + m]);
            }
            let cutter = core_math::half_edge::HalfEdgeMesh::from_polygons(&pos, &faces);
            if cutter.validate().is_err() || cutter.is_manifold().is_err() {
                return Err("degenerate cutter — keep the topological result".to_string());
            }
            // Push cutter faces that are FLAT AGAINST a target face off that plane, so the
            // boolean never has to resolve a coplanar contact (owner algorithm).
            let cut_eps = {
                let (mut mn, mut mx) = (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN));
                for p in &pos { mn = mn.min(*p); mx = mx.max(*p); }
                ((mx - mn).length().max(1e-6) * 1e-3).max(1e-5)
            };
            let cutter = if std::env::var("AZRA_NO_COPUSH").is_ok() { cutter.clone() } else { Self::push_coplanar_cutter_faces(&cutter, pre_mesh, cut_eps, face_n) };
            // ALWAYS capture the exact boolean inputs (success or failure) — the owner
            // iterates fast; offline replay vs the Blender oracle needs every run.
            write_extrude_repro("azra_extrude_repro.json", &result, &cutter);
            result = match mesh_boolean(&result, &cutter, BoolOp::Difference) {
                Some(r) => r,
                None => {
                    use core_math::mesh_boolean::{boundary_half_edge_count, mesh_signed_volume};
                    eprintln!(
                        "[EXTRUDE-PT] kernel refused: target boundary={} folds={} vol={:.5} | cutter vol={:.5}",
                        boundary_half_edge_count(&result),
                        result.count_folded_coplanar_pairs(),
                        mesh_signed_volume(&result),
                        mesh_signed_volume(&cutter),
                    );
                    // Exact repro dump for offline kernel replay. SEPARATE file from the
                    // rolling capture above — a refusal must survive the next successful cut.
                    write_extrude_repro("azra_extrude_repro_FAIL.json", &result, &cutter);
                    return Err("kernel refused — see log above".to_string());
                }
            };
        }
        {
            let bt = (0..result.faces.len() as u32)
                .filter(|&i| {
                    let fid = FaceId(i);
                    result.face(fid).half_edge != core_math::half_edge::NO_EDGE
                        && result.face_vertices(fid).len() == 3
                })
                .count();
            eprintln!(
                "[EXTRUDE-PT] boolean out: v={} f={} tris={} folds={} rings={}",
                result.vertex_count(), result.face_count(), bt,
                result.count_folded_coplanar_pairs(), rings.len()
            );
        }
        if !crate::poly::boolean::result_installable(&result) {
            eprintln!(
                "[EXTRUDE-PT] NOT installable: faces={} valid={:?} edge_manifold={:?} boundary={} folds={}",
                result.face_count(),
                result.validate().err(),
                result.is_edge_manifold().err(),
                core_math::mesh_boolean::boundary_half_edge_count(&result),
                result.count_folded_coplanar_pairs()
            );
            return Err("boolean result NOT installable".to_string());
        }
        Ok(result)
    }

    fn poly_extrude_try_push_through(&mut self, nid: core_math::scene_graph::NodeId, offset: f32, force: bool) -> bool {
        let (along_normals, face_normals, new_faces, world_axis, edge_axis) = match self.tool.current.as_poly_extrude() {
            Some(d) => (d.along_normals, d.face_normals.clone(), d.new_face_ids.clone(), d.world_axis, d.edge_axis),
            None => return false,
        };
        if along_normals || offset >= -1e-6 {
            eprintln!("[EXTRUDE-PT] skip: along_normals={along_normals} offset={offset:.4} (outward/along)");
            return false; // outward or along-normals: topological extrude is correct
        }
        let n = {
            let sum: Vec3 = face_normals.iter().map(|(_, fnorm)| *fnorm).sum();
            sum.normalize_or_zero()
        };
        if n == Vec3::ZERO {
            return false;
        }
        // TWO distinct directions (owner 2026-08-04: "it should use the same orientation of
        // the cut"): `face_n` is the SURFACE normal of the region, `n` is the direction the
        // cut travels (region normal, world axis, or a picked edge). They differ whenever
        // the face is TILTED relative to the chosen direction. The sweep follows `n`; the
        // entry offset that lifts the cutter clear of the surface must follow `face_n`,
        // otherwise on a tilted face that lift pokes out sideways and shears into the
        // neighbouring faces — the extra segments around the region boundary.
        let face_n = n;
        let n = Self::extrude_effective_dir(n, world_axis, edge_axis);
        let push = -offset; // depth traveled INTO the solid, along -n
        let push_dir = -n;

        // PRE-drag mesh: owned by the drag state (captured at extrude start). Poly undo
        // episodes carry no scene-graph snapshot — live console proved it ("skip: no
        // pre-drag snapshot") — so this reads the drag state, not undo.
        let pre_mesh = match self.tool.current.as_poly_extrude().and_then(|d| d.pre_mesh.clone()) {
            Some(m) => m,
            None => {
                eprintln!("[EXTRUDE-PT] skip: drag state has no pre-mesh");
                return false;
            }
        };

        let Some(cur) = self.scene.scene_graph.get(nid).and_then(|node| match &node.payload {
            NodePayload::PolygonalBody { mesh, .. } => Some(mesh.clone()),
            _ => None,
        }) else {
            return false;
        };
        let result = match Self::push_through_core(&pre_mesh, &cur, &new_faces, face_n, n, offset, force)
            .map(|r| self.poly_extrude_merge_debris(nid, &cur, r))
        {
            Ok(r) => r,
            Err(msg) => {
                eprintln!("[EXTRUDE-PT] {msg}");
                if msg.starts_with("inside wall") || msg.starts_with("skip:") {
                    // Pocket / not-applicable: the topological extrude IS the right result.
                    return false;
                }
                // THROUGH-cut refused (owner rule 2026-08-04: cut correctly or say no).
                // The raw topological extrude at through-depth is garbage — walls punched
                // through the island, "bad geo where the cut happens" (test-05). Restore
                // the pre-drag mesh instead of keeping it. Handled: caller must not
                // continue into merge/commit.
                self.poly_cancel_extrude_drag();
                self.toast("Intrude: cut could not be made cleanly — mesh unchanged (Ctrl+Z not needed)");
                return true;
            }
        };

        {
            let tris = (0..result.faces.len() as u32)
                .filter(|&i| {
                    let fid = FaceId(i);
                    result.face(fid).half_edge != core_math::half_edge::NO_EDGE
                        && result.face_vertices(fid).len() == 3
                })
                .count();
            let folds = result.count_folded_coplanar_pairs();
            eprintln!(
                "[EXTRUDE-PT] installed: v={} f={} tris={} folds={} manifold={}",
                result.vertex_count(),
                result.face_count(),
                tris,
                folds,
                result.is_manifold().is_ok()
            );
            // SELF-EVIDENCE (owner 2026-08-04): when the installed mesh is not clean, write
            // it out verbatim. The boolean INPUTS were already dumped; having the INSTALLED
            // mesh too makes app-vs-kernel divergence diagnosable without the owner
            // relaying anything.
            if folds > 0 || tris > result.face_count() / 2 {
                let mut verts: Vec<[f32; 3]> = Vec::new();
                let mut remap = std::collections::HashMap::new();
                let mut fs: Vec<Vec<usize>> = Vec::new();
                for i in 0..result.faces.len() as u32 {
                    let fid = FaceId(i);
                    if result.face(fid).half_edge == core_math::half_edge::NO_EDGE { continue; }
                    fs.push(result.face_vertices(fid).iter().map(|v| {
                        *remap.entry(v.0).or_insert_with(|| {
                            let p = result.vert(*v).position;
                            verts.push([p.x, p.y, p.z]);
                            verts.len() - 1
                        })
                    }).collect());
                }
                let path = std::env::temp_dir().join("azra_extrude_installed.json");
                let json = format!("{{\"target_verts\":{verts:?},\"target_faces\":{fs:?},\"cutter_verts\":[],\"cutter_faces\":[]}}");
                if std::fs::write(&path, json).is_ok() {
                    eprintln!("[EXTRUDE-PT] NOT CLEAN — wrote installed mesh {}", path.display());
                }
            }
        }
        // BOOL-STALECD IS OVER (same carry as the boolean install in poly/boolean.rs): the
        // boolean rebuilt the topology, so per-id sidecar entries point at strangers — but
        // POSITIONS survive for everything the cut did not touch. Remap the sidecar onto the
        // result BEFORE installing it; unmatched entries (the intruded region) stay at their
        // defaults — fail closed, never guessed. Dropping it wholesale is what wiped every
        // Smart Select tag on an intrude.
        let remapped = self
            .cache
            .poly_custom_data
            .get(&nid)
            .zip(self.scene.scene_graph.get(nid).and_then(|node| match &node.payload {
                NodePayload::PolygonalBody { mesh, .. } => Some(mesh.clone()),
                _ => None,
            }))
            .map(|(cd, old)| cd.remap_by_position(&old, &result));
        // Install over the current (topological) state; the pending episode still snapshots
        // the PRE-extrude mesh, so ONE undo restores exactly the pre-drag shape.
        if let Some(node) = self.scene.scene_graph.get_mut(nid) {
            if let NodePayload::PolygonalBody { mesh, .. } = &mut node.payload {
                *mesh = std::sync::Arc::new(result);
            }
        }
        match remapped {
            Some(cd) => {
                self.cache.poly_custom_data.insert(nid, cd);
            }
            None => {
                self.cache.poly_custom_data.remove(&nid);
            }
        }
        // Cut invalidated the drag's face/edge/vert ids — clear rather than point at strangers.
        self.poly_sel.selected_faces.clear();
        self.poly_sel.selected_edges.clear();
        self.poly_sel.selected_vertices.clear();
        self.commit_episode();
        self.poly_sync_mesh_cache(nid);
        self.poly_flush_mesh_dirty();
        self.mark_dirty(DirtyFlags::RENDER);
        self.toast("Extrude: pushed through — material cut");
        self.tool.current = ActiveTool::Idle;
        self.poly_tool = PolyTool::Idle;
        true
    }

    /// Cancel extrude drag: restore mesh state from episode snapshot.
    pub(crate) fn poly_cancel_extrude_drag(&mut self) {
        // EXCANCELSEL: read the pre-tool selection out of the drag state before it's lost.
        let restore = self.tool.current.as_poly_extrude()
            .map(|d| (d.orig_sel_faces.clone(), d.orig_sel_edges.clone(), d.orig_sel_vertices.clone()));
        if let Some(entry) = self.pending_episode.take() {
            if let Some(sg) = entry.payload.scene_graph_snapshot() {
                self.scene.scene_graph = sg;
            }
        }
        self.tool.current = ActiveTool::Idle;
        self.poly_tool = PolyTool::Idle;
        if let Some((f, e, v)) = restore {
            self.poly_sel.selected_faces = f;
            self.poly_sel.selected_edges = e;
            self.poly_sel.selected_vertices = v;
        } else {
            self.poly_sel.selected_faces.clear();
            self.poly_sel.selected_edges.clear();
            self.poly_sel.selected_vertices.clear();
        }
        // The cancel restored the scene graph from the snapshot; re-tessellate so the surface reflects
        // it (updates now sync LIVE, so the old deferred poly_mesh_dirty flag is never set here).
        self.poly_sync_all_meshes();
        self.mark_dirty(DirtyFlags::RENDER);
    }

    /// Confirm extrude drag — commit episode and clean up tool state.
    /// Zero offset = cancel (restore pre-drag mesh, no undo entry).
    /// Re-run a confirmed extrude from its PRE-drag mesh at a new depth. Same pipeline as
    /// the live confirm (extrude region -> move caps -> push-through boolean OR stacked-wall
    /// cleanup), driven entirely from stored state so the redo panel never stacks extrudes.
    /// Returns false and leaves the scene untouched when the new depth cannot be committed.
    fn poly_extrude_rerun(&mut self, a: &crate::types::PolyExtrudeAdjust) -> bool {
        use core_math::half_edge::FaceId;
        if a.offset.abs() < 1e-6 {
            // Zero depth = the pre-extrude mesh, exactly.
            if let Some(node) = self.scene.scene_graph.get_mut(a.nid) {
                if let NodePayload::PolygonalBody { mesh, .. } = &mut node.payload {
                    *mesh = a.before.clone();
                }
            }
            return true;
        }
        let pre = &*a.before;
        let set: std::collections::HashSet<FaceId> =
            a.faces.iter().map(|&f| FaceId(f)).collect();
        let face_n = {
            let mut sum = Vec3::ZERO;
            for &f in &a.faces {
                if (f as usize) < pre.faces.len() {
                    sum += pre.compute_face_normal(FaceId(f));
                }
            }
            sum.normalize_or_zero()
        };
        if face_n == Vec3::ZERO {
            return false;
        }
        let mut cur = pre.clone();
        let caps = cur.extrude_region(&set, Vec3::ZERO);
        if caps.is_empty() {
            return false;
        }
        let cap_ids: Vec<u32> = caps.iter().map(|f| f.0).collect();
        let mut moved: std::collections::HashSet<u32> = std::collections::HashSet::new();
        for &c in &caps {
            for v in cur.face_vertices(c) {
                if moved.insert(v.0) {
                    let p = cur.vert(v).position;
                    cur.vert_mut(v).position = p + a.dir * a.offset;
                }
            }
        }
        // Through-cut? Same boolean the live confirm runs. `dir * offset` moved the caps,
        // so the push-through sees the identical geometry it would have seen live.
        // Same debris pass as the live confirm — the redo panel must re-run the WHOLE pipeline or
        // scrubbing depth would produce a different mesh than the drag did.
        let punched = Self::push_through_core(pre, &cur, &cap_ids, face_n, a.dir, -a.offset, false)
            .ok()
            .map(|r| self.poly_extrude_merge_debris(a.nid, &cur, r));
        let punched_ok = punched.is_some();
        let installed = match punched {
            Some(res) => res,
            None => cur,
        };
        let did_boolean = {
            let mut m = installed;
            if let Some(node) = self.scene.scene_graph.get_mut(a.nid) {
                if let NodePayload::PolygonalBody { mesh, .. } = &mut node.payload {
                    std::mem::swap(Arc::make_mut(mesh), &mut m);
                }
            }
            false
        };
        let _ = did_boolean;
        // SMART SELECT, matching the live confirm. Only on the topological path: the
        // push-through REBUILDS the mesh, so "live now, not live in `pre`" stops meaning
        // "created by this extrude" and would tag strangers. Fail closed — tag nothing there.
        if !punched_ok {
            if let Some(post) = self.poly_node_mesh(a.nid) {
                let created = crate::poly::op_tags::faces_created(pre, &post);
                self.poly_extrude_stamp_tags(a.nid, &created);
            }
        }
        // Stacked-wall cleanup. Intrude-only is retired (owner 2026-08-17: outward
        // inner-wall / tray extrude must merge adjacent planes too). Off remains.
        let run_cleanup = a.wall_cleanup != 0;
        if run_cleanup {
            self.poly_extrude_merge_stacked_walls(a.nid, &cap_ids);
        }
        self.poly_invalidate_pick_cache();
        self.poly_request_display_sync(a.nid);
        self.mark_dirty(DirtyFlags::RENDER);
        true
    }

    /// The last extrude is adjustable while its node is still the active one and no modal
    /// extrude is running.
    pub(crate) fn poly_extrude_adjust_active(&self) -> bool {
        let Some(a) = self.ui.poly_extrude_adjust.as_ref() else { return false };
        if self.tool.current.is_poly_extrude() || self.tool.current.is_poly_edge_extrude() {
            return false;
        }
        // Another tool armed => that tool owns the panel (owner 2026-08-04). The redo panel
        // describes the op you finished, never the tool you just picked.
        if !matches!(self.poly_tool, crate::PolyTool::Idle | crate::PolyTool::ExtrudeFace) {
            return false;
        }
        self.scene.scene_graph.get(a.nid).is_some() && self.sel.usel.active_node() == Some(a.nid)
    }

    /// Blender F9 redo panel for Extrude (owner 2026-08-04: "the tool parameter needs to
    /// stay on after the user accepts so we can change setting"). Every change re-runs from
    /// the PRE-extrude snapshot; release settles into ONE undo entry.
    pub(crate) fn poly_extrude_adjust_panel(&mut self, ui: &mut egui::Ui) -> bool {
        let mut a = match self.ui.poly_extrude_adjust.clone() {
            Some(a) => a,
            None => return false,
        };
        if self.scene.scene_graph.get(a.nid).is_none()
            || self.sel.usel.active_node() != Some(a.nid)
        {
            self.ui.poly_extrude_adjust = None;
            return false;
        }
        let mut changed = false;
        let mut released = false;
        let mut close = false;
        ui.label(
            egui::RichText::new("Last \u{b7} Extrude")
                .size(11.0)
                .color(egui::Color32::from_rgb(160, 170, 190)),
        );
        ui.add_space(2.0);
        let r = ui.add(
            egui::DragValue::new(&mut a.offset)
                .speed(0.01)
                .prefix("Depth  "),
        );
        changed |= r.changed();
        released |= r.drag_stopped() || r.lost_focus();
        egui::ComboBox::from_id_source("extrude_adjust_cleanup")
            .selected_text(if a.wall_cleanup == 0 { "Off" } else { "On" })
            .show_ui(ui, |ui| {
                for (v, lbl) in [(0u8, "Off"), (2, "On")] {
                    if ui.selectable_label(
                        (a.wall_cleanup == 0) == (v == 0),
                        lbl,
                    ).clicked() && a.wall_cleanup != v {
                        a.wall_cleanup = v;
                        changed = true;
                        released = true;
                    }
                }
            });
        ui.label(
            egui::RichText::new("Wall cleanup")
                .size(10.0)
                .color(egui::Color32::from_rgb(140, 148, 165)),
        );
        ui.separator();
        if ui.button("Done").clicked() {
            close = true;
        }

        if changed {
            a.needs_settle = true;
            self.tweak.extrude_wall_cleanup = a.wall_cleanup;
            if !self.poly_extrude_rerun(&a) {
                // Depth not committable (e.g. a refused through-cut): keep the pre-extrude
                // mesh rather than showing a broken result -- fail closed, never a lie.
                if let Some(node) = self.scene.scene_graph.get_mut(a.nid) {
                    if let NodePayload::PolygonalBody { mesh, .. } = &mut node.payload {
                        *mesh = a.before.clone();
                    }
                }
                self.poly_request_display_sync(a.nid);
                self.mark_dirty(DirtyFlags::RENDER);
            }
        }
        self.ui.poly_extrude_adjust = Some(a.clone());
        if released {
            self.poly_request_display_sync(a.nid);
            if a.needs_settle {
                self.transaction_snapshot_only("Adjust Extrude");
                if let Some(adj) = self.ui.poly_extrude_adjust.as_mut() {
                    adj.needs_settle = false;
                }
            }
        }
        if close {
            self.poly_request_display_sync(a.nid);
            self.ui.poly_extrude_adjust = None;
        }
        true
    }

    pub(crate) fn poly_confirm_extrude_drag(&mut self) {
        let (nid, offset, new_faces) = match self.tool.current.as_poly_extrude() {
            Some(d) => (
                d.node_id,
                d.accumulated_offset,
                d.new_face_ids.clone(),
            ),
            None => return,
        };
        // REDO PANEL (owner 2026-08-04): capture everything needed to re-run this extrude
        // from its pre-drag mesh, so Tool Parameters stays live after the accept.
        self.ui.poly_extrude_adjust = self.tool.current.as_poly_extrude().and_then(|d| {
            let before = d.pre_mesh.clone()?;
            let avg = {
                let mut sum = Vec3::ZERO;
                for (_, n) in &d.face_normals { sum += *n; }
                sum.normalize_or_zero()
            };
            let dir = Self::extrude_effective_dir(avg, d.world_axis, d.edge_axis);
            (dir != Vec3::ZERO && !d.orig_faces.is_empty()).then(|| crate::types::PolyExtrudeAdjust {
                nid: d.node_id,
                faces: d.orig_faces.clone(),
                before,
                dir,
                offset: d.accumulated_offset,
                wall_cleanup: self.tweak.extrude_wall_cleanup,
                needs_settle: false,
            })
        });
        if offset.abs() < 1e-6 {
            self.poly_cancel_extrude_drag();
            return;
        }
        // ACCEPT TRACE (owner 2026-08-04: "look where the mesh changes after accepting").
        // Two things can rewrite the mesh on confirm that never run during the drag: the
        // push-through boolean, and the stacked-wall merge. Log the mesh either side of both.
        let stat = |app: &Self| -> (usize, usize) {
            app.poly_node_mesh(nid)
                .map(|m| (m.vertex_count(), m.face_count()))
                .unwrap_or((0, 0))
        };
        let before = stat(self);
        eprintln!("[ACCEPT] drag result: v={} f={} offset={offset:.4}", before.0, before.1);
        // SMART SELECT tags go in HERE — on the settled drag result, where the created faces are
        // still addressable by id and the seams finally have their real dihedral. Both later
        // rewrites preserve them: the push-through boolean carries the sidecar with
        // `remap_by_position`, and the stacked-wall cleanup only dissolves (survivors keep ids).
        let created_faces = self
            .tool
            .current
            .as_poly_extrude()
            .map(|d| d.created_faces.clone())
            .unwrap_or_default();
        self.poly_extrude_stamp_tags(nid, &created_faces);
        // CAD push-through: an inward extrude that reached/passed the far wall commits as a
        // swept-prism SUBTRACT instead (handles the coincident-wall case too). Fully
        // self-contained when it fires.
        if self.poly_extrude_try_push_through(nid, offset, false) {
            let after = stat(self);
            eprintln!("[ACCEPT] -> boolean path: v={} f={} (was v={} f={})",
                after.0, after.1, before.0, before.1);
            return;
        }
        // Stacked-plane cleanup BEFORE the episode commits — one undo = raw pre-extrude state.
        // Owner 2026-08-17 (tray screenshots): an OUTWARD inner-wall extrude must also
        // merge the new walls into the neighbouring rim planes. "Intrude only" left those
        // stacked slivers in place. Off still disables the pass.
        let run_cleanup = self.tweak.extrude_wall_cleanup != 0;
        let (merged, flush_partial) = if run_cleanup {
            let r = self.poly_extrude_merge_stacked_walls(nid, &new_faces);
            let after = stat(self);
            if after != before {
                eprintln!("[ACCEPT] -> wall cleanup merged {} wall(s): v={} f={} (was v={} f={})                      — set Tool Parameters > Wall cleanup = Off to keep the raw extrude",
                    r.0, after.0, after.1, before.0, before.1);
            } else {
                eprintln!("[ACCEPT] -> wall cleanup: no change");
            }
            r
        } else {
            eprintln!("[ACCEPT] -> wall cleanup disabled");
            (0, 0)
        };
        // Walls flush-overlapping a neighbor face only PARTIALLY cannot be dissolved (it
        // folds the face — owner extrude.fl4). Resolve with the real boolean: subtract the
        // swept prism from the PRE-drag mesh (forced even though still inside the wall).
        if flush_partial > 0 {
            eprintln!("[EXTRUDE-PT] {flush_partial} flush-partial wall(s) — forcing boolean pocket");
            if self.poly_extrude_try_push_through(nid, offset, true) {
                return;
            }
            eprintln!("[EXTRUDE-PT] forced boolean failed — keeping topological result");
        }
        if merged > 0 {
            self.poly_sync_mesh_cache(nid);
            self.mark_dirty(DirtyFlags::RENDER);
            self.toast(&format!("Extrude: merged {merged} stacked plane(s)"));
        }
        self.commit_episode();
        self.poly_flush_mesh_dirty();
        // AUTO_POLYGROUPS: tag extruded cap faces.
        let faces: Vec<core_math::half_edge::FaceId> =
            new_faces.into_iter().map(core_math::half_edge::FaceId).collect();
        let _ = self.poly_auto_group_faces(nid, core_math::poly_mesh::PolyGroupKind::Extrude, faces);
        self.tool.current = ActiveTool::Idle;
        self.poly_tool = PolyTool::Idle;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn axis_snap_picks_dominant_world_axis_preserving_sign() {
        // Nearly-Y region normal → pure +Y world lock.
        let d = AzraApp::axis_snap(Vec3::new(0.1, 0.9, 0.05).normalize());
        assert!((d - Vec3::Y).length() < 1e-5, "expected +Y, got {d:?}");
        // Negative dominant Z.
        let d = AzraApp::axis_snap(Vec3::new(0.2, 0.1, -0.95).normalize());
        assert!((d - Vec3::NEG_Z).length() < 1e-5, "expected -Z, got {d:?}");
    }

    #[test]
    fn extrude_effective_dir_edge_beats_world() {
        let avg = Vec3::Y;
        let edge = Vec3::X;
        let d = AzraApp::extrude_effective_dir(avg, true, Some(edge));
        assert!((d - Vec3::X).length() < 1e-5, "edge axis must win over world lock");
        let d = AzraApp::extrude_effective_dir(avg, true, None);
        assert!((d - Vec3::Y).length() < 1e-5, "world lock snaps region Y → +Y");
        let tilted = Vec3::new(0.3, 0.9, 0.1).normalize();
        let d = AzraApp::extrude_effective_dir(tilted, false, None);
        assert!((d - tilted).length() < 1e-5, "region mode keeps raw normal");
    }
}
