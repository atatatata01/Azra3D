//! Auto UV unwrap + UV layout viewport for polygonal bodies.
//!
//! UVs live on the runtime `CustomData` sidecar (`cache.poly_custom_data`), never as a
//! `HalfEdgeMesh` field (save-format red line). Persistence rides `PolyLayerRecord` on save.

use core_math::half_edge::{FaceId, HalfEdgeId, NO_FACE, NO_TWIN};
use core_math::scene_graph::{NodeId, NodePayload};
use core_math::uv_unwrap::{apply_unwrap_to_custom_data, smart_uv_unwrap, UvUnwrapParams};
use eframe::egui;
use glam::Vec2;
use std::collections::HashSet;

use crate::{DirtyFlags, FourthLayerApp, PolyWorkMode};

impl FourthLayerApp {
    /// UV Unwrap Tool Parameters — only while that tool is armed.
    ///
    /// Selecting a polygonal body is not enough. `poly_auto_uv_unwrap` / Pack UVs set
    /// `ui2.active_tool` to `"UV Unwrap"`; picking another tool (Box, Sculpt, Mesh Maps, …)
    /// replaces it.
    pub(crate) fn uv_unwrap_adjust_active(&self) -> bool {
        self.ui2.active_tool == "UV Unwrap" && self.poly_active_body().is_some()
    }

    /// Tool Parameters section for UV Unwrap — the three settings that decide the result, plus
    /// the measured quality of the last run so the user can see whether it is usable.
    pub(crate) fn draw_tool_params_uv_unwrap(&mut self, ui: &mut egui::Ui) {
        use crate::{ui_fields as f, ui_theme as t};
        // `last` is optional: the tool can be armed before a successful unwrap.
        let last = self.ui.uv_unwrap_adjust;
        let (mut angle, mut margin, mut cone) = (
            self.ui.uv_unwrap_angle_deg,
            self.ui.uv_unwrap_margin,
            self.ui.uv_unwrap_cone_deg,
        );
        ui.label(
            egui::RichText::new("UV Unwrap")
                .size(11.0)
                .color(t::TEXT_SECONDARY),
        );
        let mut changed = false;
        let mut pack = false;
        let mut method = core_math::uv_unwrap::UvUnwrapMethod::from_u32(self.ui.uv_unwrap_method);
        f::section(ui, "live/UvUnwrap/params", "UV Unwrap", None, |ui| {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("Method").size(10.0).color(t::TEXT_DIM));
                for m in [
                    core_math::uv_unwrap::UvUnwrapMethod::Angle,
                    core_math::uv_unwrap::UvUnwrapMethod::Seams,
                ] {
                    if ui
                        .selectable_label(method == m, m.label())
                        .on_hover_text(m.hint())
                        .clicked()
                        && method != m
                    {
                        method = m;
                        changed = true;
                    }
                }
            });
            // Angle is meaningless in Seams mode - hide it rather than show a dead control.
            if method == core_math::uv_unwrap::UvUnwrapMethod::Angle {
            changed |= f::num(ui, "Angle", &mut angle, 1.0, 1.0..=180.0, None)
                .on_hover_text(
                    "Split into a new island when the angle between neighbouring face normals \
                     exceeds this. Lower = more islands, flatter each.",
                )
                .changed();
            }
            changed |= f::num(ui, "Max Cone", &mut cone, 1.0, 5.0..=89.0, None)
                .on_hover_text(
                    "Largest angle a face may deviate from its island's average normal.\n\
                     This is what stops a curved surface (a cylinder, a rounded stone) growing \
                     one island that wraps around and FOLDS ONTO ITSELF — measured 39% of texels \
                     doubly-used on a cylinder before this existed. Never above 89°: past 90° the \
                     projection folds by definition.",
                )
                .changed();
            changed |= f::num(ui, "Margin", &mut margin, 0.002, 0.0..=0.15, None)
                .on_hover_text("Padding around and between packed islands, in UV units.")
                .changed();
            match last {
                Some(l) => {
                    ui.label(
                        egui::RichText::new(format!("{} islands · {} corners", l.islands, l.corners))
                            .size(10.0)
                            .color(t::TEXT_DIM),
                    )
                    .on_hover_text(
                        "1 island on a closed shape means the projection folded onto itself —                          lower Max Cone.",
                    );
                }
                None => {
                    ui.label(
                        egui::RichText::new("not unwrapped yet")
                            .size(10.0)
                            .color(t::TEXT_DIM),
                    );
                }
            }
            ui.horizontal(|ui| {
                if ui.button(if last.is_some() { "Unwrap again" } else { "Unwrap" }).clicked() {
                    changed = true;
                }
                if ui
                    .button("Pack")
                    .on_hover_text(
                        "Repack the EXISTING islands into the map without re-unwrapping - keeps                          the layout you authored and only re-places it.",
                    )
                    .clicked()
                {
                    pack = true;
                }
            });
        });
        if pack {
            self.ui.uv_unwrap_margin = margin;
            self.poly_pack_uvs();
            return;
        }
        if !changed {
            return;
        }
        self.ui.uv_unwrap_angle_deg = angle;
        self.ui.uv_unwrap_margin = margin;
        self.ui.uv_unwrap_cone_deg = cone;
        self.ui.uv_unwrap_method = method.as_u32();
        self.poly_auto_uv_unwrap();
    }

    /// **Pack** — repack the EXISTING UV islands, without re-unwrapping.
    ///
    /// Distinct from Unwrap on purpose: this preserves the layout the user authored (or imported)
    /// and only re-places its islands, so it is safe to run on a hand-tuned unwrap. Islands are
    /// found by UV connectivity — see `pack_existing_uvs`.
    pub(crate) fn poly_pack_uvs(&mut self) {
        let Some(nid) = self.poly_active_body() else {
            self.toast("Pack UVs: select a polygonal body first");
            return;
        };
        let Some(mesh) = self.scene.scene_graph.get(nid).and_then(|n| match &n.payload {
            NodePayload::PolygonalBody { mesh, .. } => Some(mesh.clone()),
            _ => None,
        }) else {
            return;
        };
        let Some(uv) = self
            .cache
            .poly_custom_data
            .get(&nid)
            .map(|cd| cd.uv.clone())
            .filter(|u| !u.is_empty())
        else {
            self.toast("Pack UVs: this body has no UVs yet - run UV Unwrap first");
            return;
        };
        let packed = core_math::uv_unwrap::pack_existing_uvs(
            mesh.as_ref(),
            &uv,
            self.ui.uv_unwrap_margin,
        );
        if packed.is_empty() {
            self.toast("Pack UVs: nothing to pack");
            return;
        }
        let n = packed.len();
        self.transaction_snapshot_only("Pack UVs");
        {
            let cd = self.poly_cd_mut(nid);
            // Replace UVs only. Seams are the AUTHOR'S and describe where islands may split -
            // packing moves islands, it does not redefine them.
            for (he, p) in &packed {
                cd.uv.set(*he, *p);
            }
        }
        self.poly_request_display_sync(nid);
        self.mark_dirty(DirtyFlags::RENDER);
        self.ui.show_uv_layout = true;
        self.ui2.active_tool = "UV Unwrap".into();
        self.toast(&format!("Pack UVs: repacked {n} corners"));
    }

    /// Smart UV Project on the active poly body (angle-based charts → pack into [0,1]²).
    pub(crate) fn poly_auto_uv_unwrap(&mut self) {
        let Some(nid) = self.poly_active_body() else {
            self.toast("UV Unwrap: select a polygonal body first");
            return;
        };
        // Arm before the unwrap so Tool Parameters shows these settings, not last-create / catalog.
        self.ui2.active_tool = "UV Unwrap".into();
        let Some(mesh) = self.scene.scene_graph.get(nid).and_then(|n| match &n.payload {
            NodePayload::PolygonalBody { mesh, .. } => Some(mesh.clone()),
            _ => None,
        }) else {
            return;
        };
        if mesh.face_count() == 0 {
            self.toast("UV Unwrap: mesh has no faces");
            return;
        }

        let params = UvUnwrapParams {
            method: core_math::uv_unwrap::UvUnwrapMethod::from_u32(self.ui.uv_unwrap_method),
            angle_limit_deg: self.ui.uv_unwrap_angle_deg,
            margin: self.ui.uv_unwrap_margin,
            max_cone_deg: self.ui.uv_unwrap_cone_deg,
        };
        // Honour the user's marked seams (key `C` / Mark Island Seams). Without this the unwrap
        // computed charts from geometry alone and the seam tool could not affect the result.
        let authored_seams = self.cache.poly_custom_data.get(&nid).map(|cd| cd.seam.clone());
        let result = core_math::uv_unwrap::smart_uv_unwrap_with_seams(
            mesh.as_ref(),
            &params,
            authored_seams.as_ref(),
        );
        if result.uvs.is_empty() {
            self.toast("UV Unwrap: produced no UVs");
            return;
        }

        // CustomData is snapshotted by transaction (undo path includes poly_custom_data).
        let islands = result.island_count;
        let corners = result.uvs.len();
        self.transaction_snapshot_only("UV Unwrap");
        {
            let cd = self.poly_cd_mut(nid);
            apply_unwrap_to_custom_data(cd, &result);
        }
        // Unwrap authors seam bits — bust edge-mark overlay cache if UV Seams is on.
        self.overlay_lines_fp = u64::MAX;
        self.poly_request_display_sync(nid);
        self.mark_dirty(DirtyFlags::RENDER);
        let marked = authored_seams.as_ref().map_or(0, |b| b.iter().count());
        let extra = if result.cone_cuts > 0 {
            format!(" (+{} cuts the Max Cone had to add)", result.cone_cuts)
        } else {
            String::new()
        };
        self.toast(&format!(
            "UV Unwrap [{}]: {islands} island(s), {corners} corners, {marked} marked seams{extra}",
            params.method.label()
        ));
        // Open the UV layout so the user can see the result (like Blender after unwrap).
        self.ui.show_uv_layout = true;
        // Arm the redo panel so the settings are reachable AFTER the fact (Blender F9), which is
        // the only place they have ever been editable.
        self.ui.uv_unwrap_adjust = Some(crate::types::UvUnwrapAdjust {
            islands,
            corners: corners as u32,
        });
    }

    // ── Smart UV seam (plain C — no armed tool) ────────────────────────────────

    /// Smart UV seam action (default **C**).
    ///
    /// * **Face island selected** → mark every boundary edge of the selection as a seam.
    /// * **Edges selected** (or hovered edge) → toggle seam on each (off if already seamed).
    /// * Works in Edge **or** Face edit mode — no Mark Seam tool to arm first.
    pub(crate) fn poly_mark_seam_apply(&mut self) {
        if self.modeling_context != crate::ModelingContext::Polygonal
            || self.poly_work_mode == PolyWorkMode::Object
        {
            return;
        }
        // Face island takes priority when faces are selected (Face mode or leftover selection).
        if !self.poly_sel.selected_faces.is_empty() {
            self.poly_mark_island_boundary_seams();
            return;
        }
        // Edges: explicit selection, else hover.
        let mut edges: Vec<u32> = self.poly_sel.selected_edges.iter().copied().collect();
        if edges.is_empty() {
            if let Some(h) = self.poly_sel.hovered_edge {
                edges.push(h);
            }
        }
        if edges.is_empty() {
            self.toast("Seam (C): select edges, or a face island for its boundary");
            return;
        }
        self.poly_toggle_seams_on_edges(&edges);
    }

    /// Mark or clear seams on the given half-edge ids (both twins written).
    pub(crate) fn poly_set_seam(&mut self, on: bool) {
        let what = if on { "Mark Seam" } else { "Clear Seam" };
        let edges: Vec<u32> = self.poly_sel.selected_edges.iter().copied().collect();
        if edges.is_empty() {
            self.toast(&format!("{what}: select edges first"));
            return;
        }
        self.poly_write_seams(&edges, on, what);
    }

    /// Toggle seam on each undirected edge: if either side is already a seam → clear both;
    /// otherwise mark both. Matches "C on unmark → mark, C on seam → remove".
    pub(crate) fn poly_toggle_seams_on_edges(&mut self, edges: &[u32]) {
        let Some(nid) = self.poly_active_body() else {
            self.toast("Mark Seam: select a poly object first");
            return;
        };
        let Some(mesh) = self.scene.scene_graph.get(nid).and_then(|n| match &n.payload {
            NodePayload::PolygonalBody { mesh, .. } => Some(mesh.clone()),
            _ => None,
        }) else {
            return;
        };
        let cd_ro = self.poly_cd(nid);
        let mut to_on: Vec<(HalfEdgeId, HalfEdgeId)> = Vec::new();
        let mut to_off: Vec<(HalfEdgeId, HalfEdgeId)> = Vec::new();
        let mut seen: HashSet<(u32, u32)> = HashSet::new();
        for &e in edges {
            let h = HalfEdgeId(e);
            if !mesh.is_edge_live(h) {
                continue;
            }
            let a = mesh.he_source(h).0;
            let b = mesh.he(h).vertex.0;
            let key = if a <= b { (a, b) } else { (b, a) };
            if !seen.insert(key) {
                continue;
            }
            let twin = mesh.he(h).twin;
            let is_seam = cd_ro
                .map(|cd| {
                    cd.seam.get(h)
                        || (twin != NO_TWIN && mesh.is_edge_live(twin) && cd.seam.get(twin))
                })
                .unwrap_or(false);
            if is_seam {
                to_off.push((h, twin));
            } else {
                to_on.push((h, twin));
            }
        }
        if to_on.is_empty() && to_off.is_empty() {
            self.toast("Mark Seam: no live edges in selection");
            return;
        }
        let n_on = to_on.len();
        let n_off = to_off.len();
        self.transaction("Toggle Seam", |s| {
            {
                let cd = s.poly_cd_mut(nid);
                for (h, twin) in &to_on {
                    cd.seam.set(*h, true);
                    if *twin != NO_TWIN {
                        cd.seam.set(*twin, true);
                    }
                }
                for (h, twin) in &to_off {
                    cd.seam.set(*h, false);
                    if *twin != NO_TWIN {
                        cd.seam.set(*twin, false);
                    }
                }
            }
            s.overlay_lines_fp = u64::MAX;
            s.poly_request_display_sync(nid);
            s.mark_dirty(DirtyFlags::RENDER);
        });
        // Ensure the seam overlay is on so the toggle is visible.
        if !self.viewport.show_uv_seams {
            self.viewport.show_uv_seams = true;
            self.viewport.show_overlays = true;
            self.overlay_lines_fp = u64::MAX;
        }
        match (n_on, n_off) {
            (0, n) => self.toast(&format!("Clear Seam: {n} edge{}", if n == 1 { "" } else { "s" })),
            (n, 0) => self.toast(&format!("Mark Seam: {n} edge{}", if n == 1 { "" } else { "s" })),
            (a, b) => self.toast(&format!("Seam: marked {a}, cleared {b}")),
        }
    }

    /// Write seam bits on edges (both twins). Shared by menu Mark/Clear Seam.
    fn poly_write_seams(&mut self, edges: &[u32], on: bool, what: &str) {
        let Some(nid) = self.poly_active_body() else {
            self.toast(&format!("{what}: select a poly object first"));
            return;
        };
        let Some(mesh) = self.scene.scene_graph.get(nid).and_then(|n| match &n.payload {
            NodePayload::PolygonalBody { mesh, .. } => Some(mesh.clone()),
            _ => None,
        }) else {
            return;
        };
        let mut pairs: Vec<(HalfEdgeId, HalfEdgeId)> = Vec::new();
        let mut seen: HashSet<(u32, u32)> = HashSet::new();
        for &e in edges {
            let h = HalfEdgeId(e);
            if !mesh.is_edge_live(h) {
                continue;
            }
            let a = mesh.he_source(h).0;
            let b = mesh.he(h).vertex.0;
            let key = if a <= b { (a, b) } else { (b, a) };
            if !seen.insert(key) {
                continue;
            }
            pairs.push((h, mesh.he(h).twin));
        }
        if pairs.is_empty() {
            self.toast(&format!("{what}: selection has no live edges"));
            return;
        }
        let n = pairs.len();
        self.transaction(what, |s| {
            {
                let cd = s.poly_cd_mut(nid);
                for (h, twin) in &pairs {
                    cd.seam.set(*h, on);
                    if *twin != NO_TWIN {
                        cd.seam.set(*twin, on);
                    }
                }
            }
            s.overlay_lines_fp = u64::MAX;
            s.poly_request_display_sync(nid);
            s.mark_dirty(DirtyFlags::RENDER);
        });
        if !self.viewport.show_uv_seams {
            self.viewport.show_uv_seams = true;
            self.viewport.show_overlays = true;
            self.overlay_lines_fp = u64::MAX;
        }
        self.toast(&format!(
            "{what}: {n} edge{}",
            if n == 1 { "" } else { "s" }
        ));
    }

    /// Selected faces form an island — mark every boundary edge (one face in selection, twin
    /// face out or open) as a UV seam. Always SET (not toggle), so re-pressing is idempotent.
    pub(crate) fn poly_mark_island_boundary_seams(&mut self) {
        let Some(nid) = self.poly_active_body() else {
            self.toast("Mark Seam: select a poly object first");
            return;
        };
        let Some(mesh) = self.scene.scene_graph.get(nid).and_then(|n| match &n.payload {
            NodePayload::PolygonalBody { mesh, .. } => Some(mesh.clone()),
            _ => None,
        }) else {
            return;
        };
        let faces: HashSet<u32> = self.poly_sel.selected_faces.iter().copied().collect();
        if faces.is_empty() {
            self.toast("Mark Seam: select a face island first");
            return;
        }
        let mut boundary: Vec<u32> = Vec::new();
        let mut seen: HashSet<(u32, u32)> = HashSet::new();
        for &fi in &faces {
            let fid = FaceId(fi);
            if !mesh.is_face_live(fid) {
                continue;
            }
            for he in mesh.face_half_edges(fid) {
                let twin = mesh.he(he).twin;
                let other = if twin == NO_TWIN {
                    NO_FACE
                } else {
                    mesh.he(twin).face
                };
                let is_boundary = other == NO_FACE || !faces.contains(&other.0);
                if !is_boundary {
                    continue;
                }
                let a = mesh.he_source(he).0;
                let b = mesh.he(he).vertex.0;
                let key = if a <= b { (a, b) } else { (b, a) };
                if seen.insert(key) {
                    boundary.push(he.0);
                }
            }
        }
        if boundary.is_empty() {
            self.toast("Mark Seam: selection has no boundary (whole shell?)");
            return;
        }
        self.poly_write_seams(&boundary, true, "Mark Island Seam");
    }

    /// Digest of authored UVs for the poly display cache key.
    /// Without this, unwrap would leave the stale GPU mesh (no UVs in the vertex buffer).
    pub(crate) fn poly_uv_key(&self, nid: NodeId) -> Option<u64> {
        use std::hash::{Hash, Hasher};
        let cd = self.cache.poly_custom_data.get(&nid)?;
        if cd.uv.is_empty() {
            return None;
        }
        let mut h = std::collections::hash_map::DefaultHasher::new();
        // Count + sorted corner ids + quantized UV bits.
        let mut entries: Vec<(u32, u32, u32)> = cd
            .uv
            .iter()
            .map(|(k, v)| (k.0, v.x.to_bits(), v.y.to_bits()))
            .collect();
        entries.sort_unstable();
        entries.len().hash(&mut h);
        for e in entries {
            e.hash(&mut h);
        }
        Some(h.finish())
    }

    /// The viewport rect the 3D view may use — the RIGHT portion when the UV editor is open.
    ///
    /// Returned so callers that place 3D content can respect the split instead of drawing under
    /// the UV pane.
    pub(crate) fn uv_split_3d_rect(&self, rect: egui::Rect) -> egui::Rect {
        if !self.ui.show_uv_layout {
            return rect;
        }
        let w = rect.width() * self.ui.uv_split_frac.clamp(0.15, 0.85);
        egui::Rect::from_min_max(
            egui::pos2(rect.min.x + w, rect.min.y),
            rect.max,
        )
    }

    pub(crate) fn uv_layout_left_pane(&self) -> egui::Rect {
        let host = self.ui.uv_host_rect;
        if !host.is_positive() {
            return egui::Rect::NOTHING;
        }
        let frac = self.ui.uv_split_frac.clamp(0.15, 0.85);
        let w = host.width() * frac;
        egui::Rect::from_min_max(host.min, egui::pos2(host.min.x + w, host.max.y))
    }

    fn uv_square_in_pane(pane: egui::Rect) -> egui::Rect {
        let pad = 24.0_f32;
        let side = (pane.width().min(pane.height()) - pad * 2.0).max(32.0);
        let origin = egui::pos2(
            pane.center().x - side * 0.5,
            pane.center().y - side * 0.5,
        );
        egui::Rect::from_min_size(origin, egui::vec2(side, side))
    }

    fn uv_from_screen(uv_rect: egui::Rect, pos: egui::Pos2) -> Vec2 {
        let side = uv_rect.width().max(1.0);
        Vec2::new(
            (pos.x - uv_rect.left()) / side,
            (uv_rect.bottom() - pos.y) / side,
        )
    }

    fn uv_point_in_poly(p: Vec2, pts: &[Vec2]) -> bool {
        if pts.len() < 3 {
            return false;
        }
        let mut inside = false;
        let n = pts.len();
        let mut j = n - 1;
        for i in 0..n {
            let (pi, pj) = (pts[i], pts[j]);
            if (pi.y > p.y) != (pj.y > p.y) {
                let t = (p.y - pi.y) / (pj.y - pi.y);
                let xint = pi.x + t * (pj.x - pi.x);
                if p.x < xint {
                    inside = !inside;
                }
            }
            j = i;
        }
        inside
    }

    fn uv_poly_area2(pts: &[Vec2]) -> f32 {
        let n = pts.len();
        if n < 3 {
            return 0.0;
        }
        let mut a = 0.0f32;
        for i in 0..n {
            let p = pts[i];
            let q = pts[(i + 1) % n];
            a += p.x * q.y - q.x * p.y;
        }
        a.abs()
    }

    fn uv_face_poly(
        mesh: &core_math::half_edge::HalfEdgeMesh,
        cd: &core_math::poly_mesh::CustomData,
        fi: u32,
    ) -> Option<Vec<Vec2>> {
        let hes = mesh.face_half_edges(FaceId(fi));
        if hes.len() < 3 {
            return None;
        }
        let mut pts = Vec::with_capacity(hes.len());
        for he in &hes {
            pts.push(*cd.uv.get(*he)?);
        }
        Some(pts)
    }

    /// Face under the cursor in the UV square, or `None`.
    pub(crate) fn uv_pick_face_at(&self, pos: egui::Pos2) -> Option<(NodeId, u32)> {
        let pane = self.uv_layout_left_pane();
        if !pane.contains(pos) {
            return None;
        }
        let uv_rect = Self::uv_square_in_pane(pane);
        let uv = Self::uv_from_screen(uv_rect, pos);
        let nid = self.poly_active_body()?;
        let mesh = self.scene.scene_graph.get(nid).and_then(|n| match &n.payload {
            NodePayload::PolygonalBody { mesh, .. } => Some(mesh.clone()),
            _ => None,
        })?;
        let cd = self.poly_cd(nid)?;
        let mut best: Option<(u32, f32)> = None;
        for fi in 0..mesh.faces.len() as u32 {
            if !mesh.is_face_live(FaceId(fi)) {
                continue;
            }
            let Some(pts) = Self::uv_face_poly(mesh.as_ref(), cd, fi) else {
                continue;
            };
            if !Self::uv_point_in_poly(uv, &pts) {
                continue;
            }
            let area = Self::uv_poly_area2(&pts);
            if best.map_or(true, |(_, a)| area < a) {
                best = Some((fi, area));
            }
        }
        best.map(|(fi, _)| (nid, fi))
    }

    fn uv_at_src(
        mesh: &core_math::half_edge::HalfEdgeMesh,
        cd: &core_math::poly_mesh::CustomData,
        he: HalfEdgeId,
    ) -> Option<Vec2> {
        let face = mesh.he(he).face;
        if face == NO_FACE {
            return None;
        }
        let hes = mesh.face_half_edges(face);
        let idx = hes.iter().position(|&h| h == he)?;
        let prev = hes[(idx + hes.len() - 1) % hes.len()];
        cd.uv.get(prev).copied()
    }

    fn uv_twins_welded(
        mesh: &core_math::half_edge::HalfEdgeMesh,
        cd: &core_math::poly_mesh::CustomData,
        he: HalfEdgeId,
    ) -> bool {
        if cd.seam.get(he) {
            return false;
        }
        let twin = mesh.he(he).twin;
        if twin == NO_TWIN || !mesh.is_edge_live(twin) {
            return false;
        }
        if cd.seam.get(twin) {
            return false;
        }
        let adj = mesh.he(twin).face;
        if adj == NO_FACE {
            return false;
        }
        let Some(a0) = Self::uv_at_src(mesh, cd, he) else {
            return false;
        };
        let Some(a1) = cd.uv.get(he).copied() else {
            return false;
        };
        let Some(b0) = Self::uv_at_src(mesh, cd, twin) else {
            return false;
        };
        let Some(b1) = cd.uv.get(twin).copied() else {
            return false;
        };
        const EPS2: f32 = 1e-8;
        (a0 - b1).length_squared() <= EPS2 && (a1 - b0).length_squared() <= EPS2
    }

    /// UV-connected faces (welded UV edges, seams are cuts). Same island the 3D view must show.
    pub(crate) fn uv_island_faces(
        mesh: &core_math::half_edge::HalfEdgeMesh,
        cd: &core_math::poly_mesh::CustomData,
        seed: u32,
    ) -> HashSet<u32> {
        let mut out = HashSet::new();
        if !mesh.is_face_live(FaceId(seed)) {
            return out;
        }
        let mut stack = vec![seed];
        out.insert(seed);
        while let Some(fi) = stack.pop() {
            for he in mesh.face_half_edges(FaceId(fi)) {
                if !Self::uv_twins_welded(mesh, cd, he) {
                    continue;
                }
                let adj = mesh.he(mesh.he(he).twin).face.0;
                if out.insert(adj) {
                    stack.push(adj);
                }
            }
        }
        out
    }

    /// Write face selection and mirror it into the 3D viewport (same `poly_sel`).
    pub(crate) fn uv_apply_face_selection(&mut self, faces: &[u32], additive: bool) {
        let Some(nid) = self.poly_active_body() else {
            return;
        };
        if self.modeling_context != crate::ModelingContext::Polygonal {
            self.modeling_context = crate::ModelingContext::Polygonal;
        }
        if self.tool.work_mode == crate::WorkMode::Sculpt {
            self.restore_sdf_room_render_state();
        }
        if self.poly_work_mode == PolyWorkMode::Object {
            self.poly_switch_work_mode(PolyWorkMode::Face);
        }
        if !additive {
            self.poly_sel.selected_faces.clear();
            self.poly_sel.selected_vertices.clear();
            self.poly_sel.selected_edges.clear();
        }
        self.poly_sel.selected_faces.extend(faces.iter().copied());
        if let Some(&seed) = faces.first() {
            self.poly_push_face_history(seed);
        }
        if let Some(node) = self.scene.scene_graph.get(nid) {
            if let NodePayload::PolygonalBody { mesh, .. } = &node.payload {
                match self.poly_work_mode {
                    PolyWorkMode::Vertex => {
                        for &fi in faces {
                            for v in mesh.face_vertices(FaceId(fi)) {
                                self.poly_sel.selected_vertices.insert(v.0);
                            }
                        }
                    }
                    PolyWorkMode::Edge => {
                        for &fi in faces {
                            for he in mesh.face_half_edges(FaceId(fi)) {
                                if mesh.is_edge_live(he) {
                                    self.poly_sel.selected_edges.insert(he.0);
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        self.mark_dirty(DirtyFlags::RENDER);
    }

    pub(crate) fn handle_uv_layout_input(
        &mut self,
        ctx: &egui::Context,
        pos: egui::Pos2,
        response: &egui::Response,
    ) {
        let pane = self.uv_layout_left_pane();
        let drop_bar = egui::Rect::from_min_size(
            egui::pos2(pane.left() + 8.0, pane.top() + 6.0),
            egui::vec2(300.0, 26.0),
        );
        if drop_bar.contains(pos) {
            return;
        }
        let shift = ctx.input(|i| i.modifiers.shift);
        if response.double_clicked_by(egui::PointerButton::Primary) {
            if let Some((nid, fi)) = self.uv_pick_face_at(pos) {
                let island = {
                    let mesh = self.scene.scene_graph.get(nid).and_then(|n| match &n.payload {
                        NodePayload::PolygonalBody { mesh, .. } => Some(mesh.clone()),
                        _ => None,
                    });
                    let cd = self.poly_cd(nid);
                    match (mesh, cd) {
                        (Some(m), Some(cd)) => Self::uv_island_faces(m.as_ref(), cd, fi),
                        _ => {
                            let mut s = HashSet::new();
                            s.insert(fi);
                            s
                        }
                    }
                };
                let faces: Vec<u32> = island.into_iter().collect();
                self.uv_apply_face_selection(&faces, shift);
            }
            return;
        }
        if response.clicked_by(egui::PointerButton::Primary) {
            if let Some((_, fi)) = self.uv_pick_face_at(pos) {
                self.uv_apply_face_selection(&[fi], shift);
            } else if !shift {
                self.uv_apply_face_selection(&[], false);
            }
        }
        if let Some((_, fi)) = self.uv_pick_face_at(pos) {
            self.poly_sel.hovered_face = Some(fi);
        }
    }

    fn uv_face_is_highlighted(
        &self,
        mesh: &core_math::half_edge::HalfEdgeMesh,
        fi: u32,
    ) -> bool {
        if self.poly_sel.selected_faces.contains(&fi) {
            return true;
        }
        match self.poly_work_mode {
            PolyWorkMode::Vertex => mesh
                .face_vertices(FaceId(fi))
                .iter()
                .any(|v| self.poly_sel.selected_vertices.contains(&v.0)),
            PolyWorkMode::Edge => mesh.face_half_edges(FaceId(fi)).iter().any(|he| {
                self.poly_sel.selected_edges.contains(&he.0)
                    || self.poly_sel.selected_edges.contains(&mesh.he(*he).twin.0)
            }),
            _ => false,
        }
    }

    /// Draw the UV editor as a viewport SPLIT (left pane), with a draggable divider.
    pub(crate) fn draw_uv_layout_split(&mut self, ui: &mut egui::Ui, rect: egui::Rect) {
        if !self.ui.show_uv_layout {
            return;
        }
        let frac = self.ui.uv_split_frac.clamp(0.15, 0.85);
        let w = rect.width() * frac;
        let left = egui::Rect::from_min_max(rect.min, egui::pos2(rect.min.x + w, rect.max.y));
        self.draw_uv_layout_overlay(ui, left);

        // Divider: thin, draggable, and it must not steal clicks from either pane.
        let bar = egui::Rect::from_min_max(
            egui::pos2(left.max.x - 3.0, rect.min.y),
            egui::pos2(left.max.x + 3.0, rect.max.y),
        );
        let resp = ui.interact(
            bar,
            ui.id().with("uv_split_divider"),
            egui::Sense::drag(),
        );
        ui.painter().rect_filled(
            egui::Rect::from_min_max(
                egui::pos2(left.max.x - 1.0, rect.min.y),
                egui::pos2(left.max.x + 1.0, rect.max.y),
            ),
            0.0,
            if resp.hovered() || resp.dragged() {
                crate::ui_theme::ACCENT
            } else {
                crate::ui_theme::SEAM
            },
        );
        if resp.hovered() || resp.dragged() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
        }
        if resp.dragged() && rect.width() > 1.0 {
            self.ui.uv_split_frac =
                (frac + resp.drag_delta().x / rect.width()).clamp(0.15, 0.85);
        }
    }

    /// Draw the 2D UV layout over the viewport (Blender UV Editor style).
    pub(crate) fn draw_uv_layout_overlay(&mut self, ui: &mut egui::Ui, rect: egui::Rect) {
        if !self.ui.show_uv_layout {
            return;
        }

        let pad = 24.0_f32;
        let side = (rect.width().min(rect.height()) - pad * 2.0).max(32.0);
        let origin = egui::pos2(
            rect.center().x - side * 0.5,
            rect.center().y - side * 0.5,
        );
        let uv_rect = egui::Rect::from_min_size(origin, egui::vec2(side, side));

        let nid = self.poly_active_body().or_else(|| {
            self.sel.usel.active_node().filter(|&n| {
                matches!(
                    self.scene.scene_graph.get(n).map(|n| &n.payload),
                    Some(NodePayload::PolygonalBody { .. })
                )
            })
        });
        let resolved = self.resolve_uv_underlay_live(nid);
        let ctx = ui.ctx().clone();
        let underlay_tid = self.cache.uv_underlay_cache.texture(
            &ctx,
            resolved,
            self.gpu.as_ref().map(|g| &g.material_textures),
        );
        // V-up UV square vs Y-down egui image.
        let uv_flip = egui::Rect::from_min_max(egui::pos2(0.0, 1.0), egui::pos2(1.0, 0.0));

        let painter = ui.painter().clone();
        painter.rect_filled(rect, 0.0, egui::Color32::from_rgb(42, 42, 42));
        painter.image(underlay_tid, uv_rect, uv_flip, egui::Color32::WHITE);

        let grid_col = egui::Color32::from_rgba_unmultiplied(255, 255, 255, 28);
        let major = egui::Color32::from_rgba_unmultiplied(255, 255, 255, 50);
        for i in 0..=4 {
            let t = i as f32 / 4.0;
            let x = uv_rect.left() + t * side;
            let y = uv_rect.top() + t * side;
            let stroke = if i == 0 || i == 4 {
                egui::Stroke::new(1.5, major)
            } else {
                egui::Stroke::new(1.0, grid_col)
            };
            painter.line_segment(
                [egui::pos2(x, uv_rect.top()), egui::pos2(x, uv_rect.bottom())],
                stroke,
            );
            painter.line_segment(
                [egui::pos2(uv_rect.left(), y), egui::pos2(uv_rect.right(), y)],
                stroke,
            );
        }
        painter.rect_stroke(
            uv_rect,
            0.0,
            egui::Stroke::new(1.5, egui::Color32::from_rgb(120, 120, 120)), egui::StrokeKind::Middle,
        );
        if matches!(
            resolved,
            crate::mesh_map_bake::UvUnderlayResolved::Missing
        ) {
            painter.text(
                egui::pos2(uv_rect.left() + 8.0, uv_rect.top() + 8.0),
                egui::Align2::LEFT_TOP,
                "underlay missing",
                egui::FontId::proportional(11.0),
                egui::Color32::from_rgb(255, 176, 32),
            );
        }

        if nid.is_none() {
            painter.text(
                uv_rect.center(),
                egui::Align2::CENTER_CENTER,
                "Select a poly body\nthen UV \u{2192} Unwrap",
                egui::FontId::proportional(14.0),
                egui::Color32::from_gray(140),
            );
            self.draw_uv_underlay_dropdown(ui, rect);
            return;
        }
        let nid = nid.unwrap();

        let Some(mesh) = self.scene.scene_graph.get(nid).and_then(|n| match &n.payload {
            NodePayload::PolygonalBody { mesh, .. } => Some(mesh.clone()),
            _ => None,
        }) else {
            self.draw_uv_underlay_dropdown(ui, rect);
            return;
        };
        let Some(cd) = self.poly_cd(nid) else {
            painter.text(
                uv_rect.center(),
                egui::Align2::CENTER_CENTER,
                "No UVs — run UV Unwrap (Ctrl+U)",
                egui::FontId::proportional(14.0),
                egui::Color32::from_gray(140),
            );
            self.draw_uv_underlay_dropdown(ui, rect);
            return;
        };
        if cd.uv.is_empty() {
            painter.text(
                uv_rect.center(),
                egui::Align2::CENTER_CENTER,
                "No UVs — run UV Unwrap (Ctrl+U)",
                egui::FontId::proportional(14.0),
                egui::Color32::from_gray(140),
            );
            self.draw_uv_underlay_dropdown(ui, rect);
            return;
        }

        let to_screen = |uv: Vec2| -> egui::Pos2 {
            // V grows up in UV space → flip Y for screen.
            egui::pos2(
                uv_rect.left() + uv.x.clamp(0.0, 1.0) * side,
                uv_rect.bottom() - uv.y.clamp(0.0, 1.0) * side,
            )
        };

        let fill = egui::Color32::from_rgba_unmultiplied(90, 90, 95, 48);
        let edge = egui::Color32::from_rgb(200, 200, 200);
        let sel_fill = egui::Color32::from_rgba_unmultiplied(255, 170, 60, 90);
        let sel_edge = egui::Color32::from_rgb(255, 180, 60);
        let mut drawn = 0u32;

        for fi in 0..mesh.faces.len() as u32 {
            if !mesh.is_face_live(FaceId(fi)) {
                continue;
            }
            let hes = mesh.face_half_edges(FaceId(fi));
            if hes.len() < 3 {
                continue;
            }
            let mut pts: Vec<egui::Pos2> = Vec::with_capacity(hes.len());
            let mut missing = false;
            for he in &hes {
                match cd.uv.get(*he) {
                    Some(uv) => pts.push(to_screen(*uv)),
                    None => {
                        missing = true;
                        break;
                    }
                }
            }
            if missing || pts.len() < 3 {
                continue;
            }
            let selected = self.uv_face_is_highlighted(mesh.as_ref(), fi);
            painter.add(egui::Shape::convex_polygon(
                pts.clone(),
                if selected { sel_fill } else { fill },
                egui::Stroke::NONE,
            ));
            painter.add(egui::Shape::closed_line(
                pts,
                egui::Stroke::new(1.0, if selected { sel_edge } else { edge }),
            ));
            drawn += 1;
        }

        // Seam edges (cyan) — draw the face-ring edge whose HE is tagged as seam.
        let seam_col = egui::Color32::from_rgb(80, 200, 255);
        for &he in cd.seam.iter() {
            if !mesh.is_edge_live(he) {
                continue;
            }
            let e = mesh.he(he);
            if e.face == core_math::half_edge::NO_FACE {
                continue;
            }
            // Corner UV is stored on the HE (destination vertex of that HE). The previous
            // HE on the face holds the UV of the source vertex.
            let Some(uv_dst) = cd.uv.get(he).copied() else {
                continue;
            };
            let mut uv_src: Option<Vec2> = None;
            let hes = mesh.face_half_edges(e.face);
            if let Some(idx) = hes.iter().position(|&h| h == he) {
                let prev = hes[(idx + hes.len() - 1) % hes.len()];
                uv_src = cd.uv.get(prev).copied();
            }
            if let Some(a) = uv_src {
                painter.line_segment(
                    [to_screen(a), to_screen(uv_dst)],
                    egui::Stroke::new(1.5, seam_col),
                );
            }
        }

        painter.text(
            egui::pos2(rect.left() + 10.0, rect.bottom() - 12.0),
            egui::Align2::LEFT_BOTTOM,
            format!("{drawn} faces \u{00B7} {} UV corners", cd.uv.len()),
            egui::FontId::proportional(11.0),
            egui::Color32::from_gray(160),
        );
        self.draw_uv_underlay_dropdown(ui, rect);
    }

    fn draw_uv_underlay_dropdown(&mut self, ui: &mut egui::Ui, rect: egui::Rect) {
        let bar = egui::Rect::from_min_size(
            egui::pos2(rect.left() + 8.0, rect.top() + 6.0),
            egui::vec2(300.0, 26.0),
        );
        let options = self.uv_underlay_options();
        let mut src = self.ui.uv_underlay.clone();
        ui.allocate_ui_at_rect(bar, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                ui.label(
                    egui::RichText::new("Underlay")
                        .size(11.0)
                        .color(egui::Color32::from_gray(200)),
                );
                let label = src.label();
                egui::ComboBox::from_id_salt("uv_underlay_src")
                    .selected_text(label)
                    .width(210.0)
                    .show_ui(ui, |ui| {
                        for opt in &options {
                            ui.selectable_value(&mut src, opt.clone(), opt.label());
                        }
                    });
            });
        });
        self.ui.uv_underlay = src;
    }
}

#[cfg(test)]
mod tool_params_active_tool_tests {
    use core_math::half_edge::HalfEdgeMesh;
    use core_math::scene_graph::NodePayload;
    use glam::Vec3;

    use crate::AzraApp;

    fn cube() -> HalfEdgeMesh {
        let p = vec![
            Vec3::new(-1.0, -1.0, -1.0),
            Vec3::new(1.0, -1.0, -1.0),
            Vec3::new(1.0, -1.0, 1.0),
            Vec3::new(-1.0, -1.0, 1.0),
            Vec3::new(-1.0, 1.0, -1.0),
            Vec3::new(1.0, 1.0, -1.0),
            Vec3::new(1.0, 1.0, 1.0),
            Vec3::new(-1.0, 1.0, 1.0),
        ];
        let f = vec![
            vec![0, 1, 2, 3],
            vec![7, 6, 5, 4],
            vec![4, 5, 1, 0],
            vec![6, 7, 3, 2],
            vec![7, 4, 0, 3],
            vec![5, 6, 2, 1],
        ];
        HalfEdgeMesh::from_polygons(&p, &f)
    }

    fn app_with_cube() -> (AzraApp, core_math::scene_graph::NodeId) {
        let mut app = AzraApp::new_headless();
        let root = app.scene.scene_graph.root();
        let nid = app.scene.scene_graph.add_child(
            root,
            NodePayload::PolygonalBody {
                name: "Probe".into(),
                mesh: std::sync::Arc::new(cube()),
                mesh_cache_id: None,
                material: core_math::primitives::PbrMaterial::default(),
                modifiers: Vec::new(),
                source: core_math::primitive_spec::MeshSource::Authored,
            },
        );
        app.select_single_unified(crate::SceneItemId::Node(nid));
        (app, nid)
    }

    #[test]
    fn uv_click_selects_face_in_viewport_too() {
        let (mut app, nid) = app_with_cube();
        app.modeling_context = crate::ModelingContext::Polygonal;
        app.poly_auto_uv_unwrap();
        let seed = 0u32;
        app.uv_apply_face_selection(&[seed], false);
        assert!(
            app.poly_sel.selected_faces.contains(&seed),
            "UV face pick must write poly_sel so the 3D viewport shows it"
        );
        assert_eq!(
            app.poly_work_mode,
            crate::PolyWorkMode::Face,
            "UV select must enter Face so the mesh highlights in the 3D view"
        );
        let _ = nid;
    }

    #[test]
    fn uv_double_click_selects_island() {
        let (mut app, nid) = app_with_cube();
        app.modeling_context = crate::ModelingContext::Polygonal;
        app.poly_auto_uv_unwrap();
        let cd = app.poly_cd(nid).expect("uvs");
        let mesh = match &app.scene.scene_graph.get(nid).unwrap().payload {
            NodePayload::PolygonalBody { mesh, .. } => mesh.clone(),
            _ => panic!("poly"),
        };
        let island = AzraApp::uv_island_faces(mesh.as_ref(), cd, 0);
        assert!(
            !island.is_empty(),
            "unwrap must produce at least one UV island"
        );
        let faces: Vec<u32> = island.iter().copied().collect();
        app.uv_apply_face_selection(&faces, false);
        assert_eq!(app.poly_sel.selected_faces.len(), faces.len());
        for f in &faces {
            assert!(app.poly_sel.selected_faces.contains(f));
        }
    }

    #[test]
    fn tool_params_idle_poly_does_not_show_uv_or_maps() {
        let (app, _) = app_with_cube();
        assert!(
            app.poly_active_body().is_some(),
            "fixture must have a selected poly"
        );
        assert!(
            !app.uv_unwrap_adjust_active(),
            "selecting a poly must not open UV Unwrap Tool Parameters"
        );
        assert!(
            !app.tool_params_mesh_maps_active(),
            "selecting a poly must not open Mesh Maps Tool Parameters"
        );
    }

    #[test]
    fn unwrap_arms_uv_unwrap_as_active_tool() {
        let (mut app, _) = app_with_cube();
        app.poly_auto_uv_unwrap();
        assert_eq!(app.ui2.active_tool, "UV Unwrap");
        assert!(app.uv_unwrap_adjust_active());
        assert!(!app.tool_params_mesh_maps_active());
    }

    #[test]
    fn bake_arms_mesh_maps_as_active_tool() {
        let (mut app, _) = app_with_cube();
        app.poly_auto_uv_unwrap();
        app.poly_request_mesh_map_bake();
        assert_eq!(app.ui2.active_tool, "Mesh Maps");
        assert!(app.tool_params_mesh_maps_active());
        assert!(
            !app.uv_unwrap_adjust_active(),
            "Bake Mesh Maps must replace UV Unwrap as the armed tool"
        );
    }

    #[test]
    fn surface_sculpt_does_not_keep_uv_as_tool_params() {
        let (mut app, nid) = app_with_cube();
        app.poly_auto_uv_unwrap();
        assert!(app.uv_unwrap_adjust_active());
        app.begin_surface_sculpt(nid);
        assert_eq!(app.tool.work_mode, crate::WorkMode::Sculpt);
        assert!(
            !app.uv_unwrap_adjust_active(),
            "Surface sculpt must not leave UV Unwrap as Tool Parameters"
        );
        assert!(!app.tool_params_mesh_maps_active());
        assert_eq!(app.ui2.active_tool, "Sculpt");
    }
}
