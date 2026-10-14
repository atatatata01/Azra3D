// convert.rs — pure relocation from poly_selection.rs
// Selection conversion between poly work modes + mode switch helpers.

use std::collections::{HashSet, VecDeque};
use eframe::egui;
use glam::Vec3;
use core_math::half_edge::{VertexId, HalfEdgeId, FaceId, NO_TWIN, NO_FACE, NO_EDGE};
use core_math::scene_graph::{NodeId, NodePayload};
use crate::{
    AzraApp, ModelingContext, PolyWorkMode, PolyTool, DirtyFlags,
    ActiveTool, SelectionShape,
    world_to_screen, world_segment_to_screen, dist_point_to_segment,
};

impl AzraApp {
    /// Convert selection from one poly sub-mode to another using half-edge topology.
    pub(crate) fn poly_convert_selection(&mut self, from: PolyWorkMode, to: PolyWorkMode) {
        let nid = match self.sel.usel.active_node() { Some(n) => n, None => return };
        let node = match self.scene.scene_graph.get(nid) { Some(n) => n, None => return };
        let mesh = match &node.payload {
            NodePayload::PolygonalBody { mesh, .. } => mesh,
            _ => return,
        };

        let (new_verts, new_edges, new_faces) = match (from, to) {
            (PolyWorkMode::Vertex, PolyWorkMode::Edge) => {
                let mut edges = HashSet::new();
                for he_idx in 0..mesh.half_edges.len() as u32 {
                    let he = mesh.he(HalfEdgeId(he_idx));
                    if he.face == NO_FACE { continue; }
                    if he.twin != NO_TWIN && he_idx > he.twin.0 { continue; }
                    let src = mesh.he_source(HalfEdgeId(he_idx)).0;
                    let dst = he.vertex.0;
                    if self.poly_sel.selected_vertices.contains(&src)
                        && self.poly_sel.selected_vertices.contains(&dst)
                    {
                        edges.insert(he_idx);
                    }
                }
                (HashSet::new(), edges, HashSet::new())
            }
            (PolyWorkMode::Vertex, PolyWorkMode::Face)
            | (PolyWorkMode::Vertex, PolyWorkMode::Group) => {
                let mut faces = HashSet::new();
                for fi in 0..mesh.faces.len() as u32 {
                    if mesh.face(FaceId(fi)).half_edge == NO_EDGE { continue; }
                    let verts = mesh.face_vertices(FaceId(fi));
                    if verts.iter().all(|v| self.poly_sel.selected_vertices.contains(&v.0)) {
                        faces.insert(fi);
                    }
                }
                (HashSet::new(), HashSet::new(), faces)
            }
            (PolyWorkMode::Edge, PolyWorkMode::Vertex) => {
                let mut verts = HashSet::new();
                let n_he = mesh.half_edges.len() as u32;
                for &he_idx in &self.poly_sel.selected_edges {
                    if he_idx >= n_he {
                        continue;
                    }
                    let src = mesh.he_source(HalfEdgeId(he_idx)).0;
                    let dst = mesh.he(HalfEdgeId(he_idx)).vertex.0;
                    verts.insert(src);
                    verts.insert(dst);
                }
                (verts, HashSet::new(), HashSet::new())
            }
            (PolyWorkMode::Edge, PolyWorkMode::Face)
            | (PolyWorkMode::Edge, PolyWorkMode::Group) => {
                let mut faces = HashSet::new();
                for fi in 0..mesh.faces.len() as u32 {
                    if mesh.face(FaceId(fi)).half_edge == NO_EDGE { continue; }
                    let face_hes = mesh.face_half_edges(FaceId(fi));
                    let all_selected = face_hes.iter().all(|&h| {
                        let twin = mesh.he(h).twin;
                        let canonical = if twin == NO_TWIN || h.0 < twin.0 { h.0 } else { twin.0 };
                        self.poly_sel.selected_edges.contains(&canonical)
                    });
                    if all_selected { faces.insert(fi); }
                }
                (HashSet::new(), HashSet::new(), faces)
            }
            (PolyWorkMode::Face, PolyWorkMode::Vertex)
            | (PolyWorkMode::Group, PolyWorkMode::Vertex) => {
                let mut verts = HashSet::new();
                let n_faces = mesh.faces.len() as u32;
                for &fi in &self.poly_sel.selected_faces {
                    // Stale ids after mesh rebuild (unbevel/bevel) — skip, never panic.
                    if fi >= n_faces || mesh.face(FaceId(fi)).half_edge == NO_EDGE {
                        continue;
                    }
                    for v in mesh.face_vertices(FaceId(fi)) {
                        verts.insert(v.0);
                    }
                }
                (verts, HashSet::new(), HashSet::new())
            }
            (PolyWorkMode::Face, PolyWorkMode::Edge)
            | (PolyWorkMode::Group, PolyWorkMode::Edge) => {
                let mut edges = HashSet::new();
                let n_faces = mesh.faces.len() as u32;
                for &fi in &self.poly_sel.selected_faces {
                    if fi >= n_faces || mesh.face(FaceId(fi)).half_edge == NO_EDGE {
                        continue;
                    }
                    for h in mesh.face_half_edges(FaceId(fi)) {
                        let twin = mesh.he(h).twin;
                        let canonical = if twin == NO_TWIN || h.0 < twin.0 { h.0 } else { twin.0 };
                        edges.insert(canonical);
                    }
                }
                (HashSet::new(), edges, HashSet::new())
            }
            // Face ↔ Group: same face selection set (no conversion).
            (PolyWorkMode::Face, PolyWorkMode::Group)
            | (PolyWorkMode::Group, PolyWorkMode::Face) => return,
            _ => return,
        };

        self.poly_sel.selected_vertices = new_verts;
        self.poly_sel.selected_edges = new_edges;
        self.poly_sel.selected_faces = new_faces;
    }

    /// Tool-driven mode hint: set `mode` only when the user is **not** in Group mode.
    ///
    /// Owner 2026-07-30: Group (polygroup selection) must stay until the user leaves via
    /// Tab pie / mode switcher. Unbevel, bevel arm, extrude, duplicate, etc. used to force
    /// Edge/Face/Vertex and dump the user out of GROUP after one modelling action.
    ///
    /// Intentional mode changes always go through [`Self::poly_switch_work_mode`].
    pub(crate) fn poly_prefer_work_mode(&mut self, mode: PolyWorkMode) {
        if self.poly_work_mode == PolyWorkMode::Group {
            return;
        }
        if self.poly_work_mode != mode {
            // Same reason as in `poly_switch_work_mode`: this is the OTHER door into component
            // editing (bevel arm, extrude, duplicate, …) and it must not leave a body claiming to
            // be parametric while a tool rewrites its topology.
            if !matches!(mode, PolyWorkMode::Object) {
                self.poly_auto_apply_before_component_edit();
            }
            self.poly_work_mode = mode;
        }
    }

    /// Group mode source-of-truth: **faces only**.
    ///
    /// Owner 2026-07-30: highlight showed the newly selected polygroup, but bevel/unbevel still
    /// ran on leftover `selected_edges` from a previous conversion (action hit another group).
    /// While in Group mode, edge/vert sets must stay empty so tools re-derive from faces.
    pub(crate) fn poly_group_selection_sync(&mut self) {
        if self.poly_work_mode != PolyWorkMode::Group {
            return;
        }
        self.poly_sel.selected_edges.clear();
        self.poly_sel.selected_vertices.clear();
        self.poly_active_vertex = None;
    }

    /// Undirected half-edges belonging to currently selected faces (for tools that need edges
    /// while Group mode keeps faces as the visible selection). Does not mutate selection.
    pub(crate) fn poly_edges_of_selected_faces(&self) -> HashSet<u32> {
        let mut edges = HashSet::new();
        let Some(nid) = self.sel.usel.active_node() else {
            return edges;
        };
        let Some(node) = self.scene.scene_graph.get(nid) else {
            return edges;
        };
        let NodePayload::PolygonalBody { mesh, .. } = &node.payload else {
            return edges;
        };
        let n_faces = mesh.faces.len() as u32;
        let n_he = mesh.half_edges.len() as u32;
        for &fi in &self.poly_sel.selected_faces {
            if fi >= n_faces || mesh.face(FaceId(fi)).half_edge == NO_EDGE {
                continue;
            }
            for h in mesh.face_half_edges(FaceId(fi)) {
                if h.0 >= n_he {
                    continue;
                }
                let twin = mesh.he(h).twin;
                let canon = if twin == NO_TWIN || h.0 < twin.0 {
                    h.0
                } else {
                    twin.0
                };
                edges.insert(canon);
            }
        }
        edges
    }

    /// Switch poly editing sub-mode with selection conversion and last-mode tracking.
    pub(crate) fn poly_switch_work_mode(&mut self, new_mode: PolyWorkMode) {
        // Object / Face / Edge / Vertex / Group are selection modes. If Surface or
        // Voxel sculpt is live, drop the brush first — otherwise LMB stays a stroke
        // (and Object is a no-op because enter-sculpt already set poly_work_mode Object).
        let sculpting = self.tool.work_mode == crate::WorkMode::Sculpt
            || self.sculpt.poly_edit.is_some()
            || self.sculpt.voxel.is_some();
        if sculpting {
            self.restore_sdf_room_render_state();
        }
        if new_mode == self.poly_work_mode {
            if sculpting {
                self.mark_dirty(DirtyFlags::RENDER);
                self.toast("Object Mode");
            }
            return;
        }

        let is_entering_edit = !matches!(new_mode, PolyWorkMode::Object);
        let was_in_edit = !matches!(self.poly_work_mode, PolyWorkMode::Object);

        if is_entering_edit && (self.sel.usel.active_node().is_none()
            || self.modeling_context != ModelingContext::Polygonal)
        {
            self.toast("Select a polygon mesh first");
            return;
        }

        // Owner 2026-08-06: changing selection type (Object / Vertex / Edge / Face / Group)
        // while the knife is armed MUST drop the knife — otherwise mode keys and the Tab pie
        // leave a half-drawn cut owning LMB, and the user thinks they are selecting.
        // Callers that re-arm the knife after a cut (free path stays sticky) set
        // `poly_tool = Knife` again after this switch.
        if self.poly_tool == PolyTool::Knife {
            self.poly_knife_cancel();
        }

        // A parametric primitive stops being parametric the moment you edit its topology:
        // otherwise the next slider drag would rebuild the mesh from the spec and silently
        // destroy the user's vertex/edge/face work. Convert up front, undoably, and say so —
        // never silently, and never by refusing to enter edit mode.
        if is_entering_edit && self.poly_auto_apply_before_component_edit() > 0 {
            self.transaction_snapshot_only("Make Editable");
            self.toast("Primitive parameters applied — mesh is now editable");
        }

        // Convert selection if switching between edit sub-modes
        if was_in_edit && is_entering_edit {
            self.poly_convert_selection(self.poly_work_mode, new_mode);
        } else if !is_entering_edit {
            self.poly_clear_selection();
            self.poly_sel.hidden_vertices.clear();
        }

        // Save last edit sub-mode
        if matches!(
            self.poly_work_mode,
            PolyWorkMode::Vertex | PolyWorkMode::Edge | PolyWorkMode::Face | PolyWorkMode::Group
        ) {
            self.ui.last_poly_edit_mode = Some(self.poly_work_mode);
        }

        self.poly_work_mode = new_mode;
        // Entering Group mode: auto-show solid polygroup colours so the selector is usable.
        // Faces are the only selection domain — drop edge/vert leftovers from other modes.
        if new_mode == PolyWorkMode::Group {
            if !self.ui.poly_show_polygroups {
                self.ui.poly_show_polygroups = true;
            }
            self.poly_group_selection_sync();
        }
        // Deactivate bevel preview when leaving edge mode
        if new_mode != PolyWorkMode::Edge && self.bevel_pv.active {
            self.bevel_pv.active = false;
            self.bevel_pv.edge_count = 0;
            self.bevel_pv.stored_edges.clear();
        }
        self.mark_dirty(DirtyFlags::RENDER);
        self.toast(match new_mode {
            PolyWorkMode::Object => "Object Mode",
            PolyWorkMode::Vertex => "Vertex Mode",
            PolyWorkMode::Edge => "Edge Mode",
            PolyWorkMode::Face => "Face Mode",
            PolyWorkMode::Group => "Group Mode — click a face to select its polygroup",
        });
    }
}
