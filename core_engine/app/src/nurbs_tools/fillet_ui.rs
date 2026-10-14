// core_engine/app/src/nurbs_tools/fillet_ui.rs
// Interactive fillet modal, adjust-last panel, apply kernel (FaceOrigin re-solve travels here).

use std::sync::Arc;
use eframe::egui;
use glam::Vec3;
use core_math::scene_graph::{NodeId, NodePayload};
use crate::{
    AzraApp, ModelingContext, NurbsWorkMode, NurbsTool,
    ActiveTool, NurbsCvDrag, NurbsKeyFlags, DirtyFlags,
    SceneItemId, world_to_screen,
};

impl AzraApp {
    /// Ctrl+B — start interactive fillet (mouse-X = radius; Enter/LMB confirm; Esc cancel).
    pub(crate) fn nurbs_try_bevel(&mut self) {
        if self.modeling_context != ModelingContext::Nurbs {
            return;
        }
        if !self.tool.current.is_idle() {
            return;
        }
        if self.nurbs_sel.selected_edges.is_empty() {
            if !self.nurbs_sel.selected_faces.is_empty() {
                self.toast("Select an edge (Tab → Edge) then Ctrl+B · drag X for radius");
            } else {
                self.toast("Select an edge then Ctrl+B · drag X for radius");
            }
            return;
        }
        let solid = match self.nurbs_solid_root() {
            Some(s) => s,
            None => return,
        };
        // Snapshot faces for re-apply
        let faces_snapshot = match self.scene.scene_graph.get(solid).map(|n| &n.payload) {
            Some(NodePayload::NurbsSolid { faces, .. }) => faces.clone(),
            _ => {
                self.toast("Fillet needs a NURBS solid");
                return;
            }
        };
        // Selection (stable ids) → current list indices for the kernel.
        let sel: Vec<(usize, u8)> = self
            .nurbs_sel
            .selected_edges
            .iter()
            .filter_map(|&(id, ei)| {
                self.nurbs_face_index(solid, id)
                    .map(|fi| (fi as usize, ei))
            })
            .collect();
        if sel.is_empty() {
            self.toast("Fillet: selected edges no longer exist (solid changed)");
            return;
        }
        // OCCT tangent-chain (G1-collinear) then planar face-loop expand when ≥2
        // edges of the same planar face are selected (Plasticity top-loop feel).
        // Never auto-completes orthogonal triad verticals (clover path — rejected).
        use core_math::nurbs_fillet::{
            expand_planar_face_boundary_loop, expand_tangent_edge_chain,
            max_fillet_radius_for_edge,
        };
        let n_sel = sel.len();
        let chained = expand_tangent_edge_chain(&faces_snapshot, &sel);
        let expanded = expand_planar_face_boundary_loop(&faces_snapshot, &chained);
        let edges: Vec<(u32, u8)> = expanded
            .iter()
            .map(|&(fi, ei)| (fi as u32, ei))
            .collect();
        let chain_extra = edges.len().saturating_sub(n_sel);

        // Cylinder rim (wall edge 0/2 against planar cap) — analytic toroid path.
        let rim_only = edges.len() == 1 && {
            let (fi, ei) = edges[0];
            let fi = fi as usize;
            fi < faces_snapshot.len()
                && matches!(ei, 0 | 2)
                && core_math::nurbs_fillet::try_fit_cylinder(
                    faces_snapshot[fi].surface.as_ref(),
                )
                .is_some()
        };

        let mut max_radius = f32::MAX;
        let mut any_ok = false;
        let mut any_fail = false;
        if rim_only {
            let (fi, _) = edges[0];
            if let Some(fit) =
                core_math::nurbs_fillet::try_fit_cylinder(faces_snapshot[fi as usize].surface.as_ref())
            {
                max_radius = fit.radius.min(fit.half_height) * 0.95;
                any_ok = true;
            }
        } else {
            for &(fi, ei) in &edges {
                match max_fillet_radius_for_edge(&faces_snapshot, fi as usize, ei) {
                    Ok(m) => {
                        max_radius = max_radius.min(m);
                        any_ok = true;
                    }
                    Err(_) => any_fail = true,
                }
            }
        }
        if !any_ok {
            self.toast(
                "Fillet: need planar edge or cylinder rim (wall edge top/bottom)",
            );
            return;
        }
        if !max_radius.is_finite() || max_radius < 0.02 {
            max_radius = 0.5;
            if let Some(&(fi, _)) = edges.first() {
                let fa = fi as usize;
                if fa < faces_snapshot.len() {
                    let s = faces_snapshot[fa].surface.as_ref();
                    let (rows, cols) = s.grid_size();
                    if rows >= 2 && cols >= 2 {
                        let p00 = s.control_points[0][0].position;
                        let p10 = s.control_points[rows - 1][0].position;
                        let p01 = s.control_points[0][cols - 1].position;
                        max_radius = (p10 - p00).length().min((p01 - p00).length()) * 0.495;
                    }
                }
            }
        }
        // Plasticity-scale: no multi-edge artificial cap (was 0.35/sqrt(n) — far too small)
        max_radius = max_radius.max(0.02);

        // Start modest; drag can go to full max (match Plasticity large bevel)
        use core_math::nurbs_fillet::Radius;
        let peak = (max_radius * 0.2).clamp(0.01, max_radius);
        let radius = Radius::Const(peak);

        // New fillet op replaces any lingering adjust panel
        self.ui.nurbs_fillet_adjust = None;

        self.begin_episode("NURBS Edge Fillet");
        self.start_operation(ActiveTool::NurbsFillet(crate::NurbsFilletDrag {
            solid,
            edges: edges.clone(),
            start_mouse: egui::pos2(0.0, 0.0),
            radius,
            max_radius,
            faces_snapshot,
            num_buf: String::new(),
        }));
        self.nurbs_fillet_regenerate();
        let warn = if any_fail { " · some edges skipped" } else { "" };
        let chain = if chain_extra > 0 {
            format!(" · +{chain_extra} tangent")
        } else {
            String::new()
        };
        self.toast(&format!(
            "Fillet {} edge(s){chain}: drag X · V=Const/Lin/Law · N=stations · type · Enter · Esc (max {max_radius:.3}){warn}",
            edges.len()
        ));
    }

    /// Seed a multi-station monotone law with `n` samples peaking at `peak`.
    /// n=2 → start/end taper; n≥3 → start < mid < … < end (or symmetric hump for n=3).
    fn nurbs_fillet_seed_law(peak: f32, n: usize) -> core_math::nurbs_fillet::Radius {
        use core_math::nurbs_fillet::Radius;
        let peak = peak.max(0.005);
        let n = n.clamp(2, 8);
        if n == 2 {
            return Radius::Law(vec![(0.0, (peak * 0.5).max(0.005)), (1.0, peak)]);
        }
        // 3 stations default: gentle rise then slight drop is allowed (bent rail);
        // use monotone rise for predictable first entry.
        let mut samples = Vec::with_capacity(n);
        for i in 0..n {
            let t = i as f32 / (n - 1) as f32;
            // Ease-in: more radius toward the end (monotone non-decreasing).
            let frac = t * t * (3.0 - 2.0 * t); // smoothstep
            let r = peak * (0.35 + 0.65 * frac);
            samples.push((t, r.max(0.005)));
        }
        // Ensure exact peak at last sample
        if let Some(last) = samples.last_mut() {
            last.1 = peak;
        }
        Radius::Law(samples)
    }

    /// Cycle live law Const → Linear → Law → Const (peak-preserving). `V` during fillet modal.
    pub(crate) fn nurbs_fillet_toggle_linear(&mut self) {
        use core_math::nurbs_fillet::Radius;
        let Some(d) = self.tool.current.as_nurbs_fillet_mut() else {
            return;
        };
        let peak = d.radius.peak().clamp(0.0, d.max_radius).max(0.01);
        d.radius = match &d.radius {
            Radius::Const(_) => Radius::Linear {
                r0: (peak * 0.5).max(0.005),
                r1: peak,
            },
            Radius::Linear { .. } => Self::nurbs_fillet_seed_law(peak, 3),
            Radius::Law(_) => Radius::Const(peak),
        };
        d.num_buf.clear();
        let msg = match &d.radius {
            Radius::Const(r) => format!("Fillet Const r={r:.3}"),
            Radius::Linear { r0, r1 } => format!("Fillet Linear {r0:.3}→{r1:.3} (drag scales peak)"),
            Radius::Law(s) => format!(
                "Fillet Law {} stn peak={:.3} (drag scales · N=stations)",
                s.len(),
                peak
            ),
        };
        self.toast(&msg);
        self.nurbs_fillet_regenerate();
    }

    /// Cycle Law station count 2 → 3 → 4 → 2 (no-op unless currently Law). `N` during fillet.
    pub(crate) fn nurbs_fillet_cycle_law_stations(&mut self) {
        use core_math::nurbs_fillet::Radius;
        let Some(d) = self.tool.current.as_nurbs_fillet_mut() else {
            return;
        };
        let peak = d.radius.peak().clamp(0.0, d.max_radius).max(0.01);
        let n = match &d.radius {
            Radius::Law(s) => match s.len() {
                2 => 3,
                3 => 4,
                _ => 2,
            },
            // Not in Law yet — enter Law with 3 stations
            _ => {
                d.radius = Self::nurbs_fillet_seed_law(peak, 3);
                d.num_buf.clear();
                self.toast("Fillet Law 3 stn (N cycles stations · V cycles mode)");
                self.nurbs_fillet_regenerate();
                return;
            }
        };
        d.radius = Self::nurbs_fillet_seed_law(peak, n);
        d.num_buf.clear();
        self.toast(&format!("Fillet Law {n} stations peak={peak:.3}"));
        self.nurbs_fillet_regenerate();
    }

    /// Rebuild fillet from snapshot + current Radius law (poly-bevel style live preview).
    pub(crate) fn nurbs_fillet_regenerate(&mut self) {
        let (solid, edges, radius, faces_snapshot) = {
            let d = match self.tool.current.as_nurbs_fillet() {
                Some(d) => d,
                None => return,
            };
            (
                d.solid,
                d.edges.clone(),
                d.radius.clone(),
                d.faces_snapshot.clone(),
            )
        };
        // Restore clean solid faces every frame (never compound)
        if let Some(node) = self.scene.scene_graph.get_mut(solid) {
            if let NodePayload::NurbsSolid { faces, .. } = &mut node.payload {
                *faces = faces_snapshot.clone();
            } else {
                return;
            }
        }
        if radius.is_near_zero() {
            self.nurbs_sync_mesh_cache(solid);
            self.mark_dirty(DirtyFlags::RENDER | DirtyFlags::MESH);
            return;
        }

        // Batch from originals: full Radius (Const → corners; Linear → edges only, no spheres)
        // OR single cylinder rim → analytic toroid.
        // OR sequential (prior fillets in snapshot) → un-shrink + re-batch (P3).
        use core_math::nurbs_fillet::{
            apply_cylinder_rim_fillet_to_solid, apply_edges_fillet_batch_radius,
            apply_edges_fillet_sequential_const, FilletError, try_fit_cylinder,
        };
        let edge_usize: Vec<(usize, u8)> = edges
            .iter()
            .map(|&(fi, ei)| (fi as usize, ei))
            .collect();
        let result = {
            let node = self.scene.scene_graph.get_mut(solid);
            match node.map(|n| &mut n.payload) {
                Some(NodePayload::NurbsSolid { faces, .. }) => {
                    let next_id = faces.iter().map(|f| f.id.0).max().unwrap_or(0) + 1;
                    // Rim path: one wall edge 0/2 on a fitted cylinder.
                    if edge_usize.len() == 1 {
                        let (fi, ei) = edge_usize[0];
                        if matches!(ei, 0 | 2)
                            && fi < faces.len()
                            && try_fit_cylinder(faces[fi].surface.as_ref()).is_some()
                        {
                            let r = radius.peak();
                            return match apply_cylinder_rim_fillet_to_solid(
                                faces, fi, ei, r, next_id,
                            ) {
                                Ok(_) => {
                                    self.nurbs_sync_mesh_cache(solid);
                                    self.nurbs_prune_selection(solid);
                                    self.mark_dirty(DirtyFlags::RENDER | DirtyFlags::MESH);
                                }
                                Err(e) => {
                                    let msg = match &e {
                                        FilletError::RadiusTooLarge { max } => {
                                            format!("Fillet rim: radius too large (max {max:.3})")
                                        }
                                        other => format!("Fillet rim: {other:?}"),
                                    };
                                    self.toast(&msg);
                                    if let Some(node) = self.scene.scene_graph.get_mut(solid) {
                                        if let NodePayload::NurbsSolid { faces, .. } =
                                            &mut node.payload
                                        {
                                            if let Some(d) = self.tool.current.as_nurbs_fillet() {
                                                *faces = d.faces_snapshot.clone();
                                            }
                                        }
                                    }
                                    self.nurbs_sync_mesh_cache(solid);
                                    self.nurbs_prune_selection(solid);
                                    self.mark_dirty(DirtyFlags::RENDER | DirtyFlags::MESH);
                                }
                            };
                        }
                    }
                    // Sequential path when snapshot already has fillets (P3 junction).
                    let has_prior = faces.iter().any(|f| f.origin.fillet_between().is_some());
                    if has_prior {
                        // Const re-batch of prior+new; Linear/Law sequential deferred.
                        let r = radius.peak();
                        apply_edges_fillet_sequential_const(faces, &edge_usize, r, next_id)
                    } else {
                        apply_edges_fillet_batch_radius(faces, &edge_usize, &radius, next_id)
                    }
                }
                _ => Err(FilletError::DegenerateEdge),
            }
        };
        match result {
            Ok(out) if !out.faulty_vertices.is_empty() => {
                let n = out.faulty_vertices.len();
                self.toast(&format!(
                    "Fillet partial: {n} corner(s) failed — {}",
                    match &out.faulty_vertices[0].1 {
                        FilletError::Unsupported { kind, state } => {
                            format!("unsupported {kind:?}/{state:?}")
                        }
                        e => format!("{e:?}"),
                    }
                ));
            }
            Ok(_) => {}
            Err(e) => {
                let msg = match &e {
                    FilletError::Unsupported { kind, state } => format!(
                        "Fillet: unsupported corner {kind:?}/{state:?} (planar 90° AllSame only)"
                    ),
                    FilletError::NotOrthogonal => {
                        "Fillet: need ~90° planar edges (non-ortho not yet)".into()
                    }
                    FilletError::RadiusTooLarge { max } => {
                        format!("Fillet: radius too large (max {max:.3})")
                    }
                    FilletError::NonMonotoneRadiusLaw => {
                        "Fillet: radius law must be monotone (no mid-span peak/valley yet)".into()
                    }
                    FilletError::UnequalRadiusCorner { radii } => {
                        format!(
                            "Fillet: unequal radii at corner [{:.3},{:.3},{:.3}] — free ends (no multi-r sphere yet)",
                            radii[0], radii[1], radii[2]
                        )
                    }
                    FilletError::CurvedFace { reason } => {
                        format!("Fillet: curved face ({reason})")
                    }
                    FilletError::DegenerateEdge => {
                        "Fillet: edge failed (planar 90° only for now)".into()
                    }
                    other => format!("Fillet: {other:?}"),
                };
                self.toast(&msg);
                if let Some(node) = self.scene.scene_graph.get_mut(solid) {
                    if let NodePayload::NurbsSolid { faces, .. } = &mut node.payload {
                        if let Some(d) = self.tool.current.as_nurbs_fillet() {
                            *faces = d.faces_snapshot.clone();
                        }
                    }
                }
            }
        }
        self.nurbs_sync_mesh_cache(solid);
        self.nurbs_prune_selection(solid);
        self.mark_dirty(DirtyFlags::RENDER | DirtyFlags::MESH);
    }

    /// Shared endpoint of three edges, if any (for N5 corner detection).
    fn nurbs_common_vertex_of_edges(
        &self,
        faces: &[core_math::nurbs::NurbsFace],
        edges: &[(u32, u8)],
    ) -> Option<Vec3> {
        use core_math::nurbs_fillet::face_edge_endpoints;
        if edges.len() != 3 {
            return None;
        }
        let mut endpoints: Vec<Vec3> = Vec::new();
        for &(fi, ei) in edges {
            let fi = fi as usize;
            if fi >= faces.len() {
                return None;
            }
            let (a, b) = face_edge_endpoints(faces[fi].surface.as_ref(), ei).ok()?;
            endpoints.push(a);
            endpoints.push(b);
        }
        // A vertex common to all three edges appears (at least) three times among six endpoints
        const EPS2: f32 = 1e-8;
        for i in 0..endpoints.len() {
            let p = endpoints[i];
            let count = endpoints
                .iter()
                .filter(|q| (**q - p).length_squared() < EPS2)
                .count();
            if count >= 3 {
                // Verify each edge has an endpoint near p
                let mut ok = true;
                for &(fi, ei) in edges {
                    let (a, b) =
                        face_edge_endpoints(faces[fi as usize].surface.as_ref(), ei).ok()?;
                    if (a - p).length_squared() >= EPS2 && (b - p).length_squared() >= EPS2 {
                        ok = false;
                        break;
                    }
                }
                if ok {
                    return Some(p);
                }
            }
        }
        None
    }

    /// N5: apply equal-radius spherical-octant corner + 3 truncated edge fillets.
    fn nurbs_apply_symmetric_corner(
        &mut self,
        solid: NodeId,
        edges: &[(u32, u8)],
        vertex: Vec3,
        radius: f32,
    ) -> Result<(), String> {
        use core_math::nurbs_fillet::{
            apply_symmetric_corner_to_faces, fillet_symmetric_cube_vertex,
        };
        if edges.len() != 3 {
            return Err("corner needs exactly 3 edges".into());
        }
        let face_edges = [
            (edges[0].0 as usize, edges[0].1),
            (edges[1].0 as usize, edges[1].1),
            (edges[2].0 as usize, edges[2].1),
        ];
        let node = self
            .scene
            .scene_graph
            .get_mut(solid)
            .ok_or_else(|| "missing solid".to_string())?;
        match &mut node.payload {
            NodePayload::NurbsSolid { faces, .. } => {
                let result = fillet_symmetric_cube_vertex(faces, face_edges, vertex, radius)
                    .map_err(|e| format!("{e:?}"))?;
                let next_id = faces.iter().map(|f| f.id.0).max().unwrap_or(0) + 1;
                apply_symmetric_corner_to_faces(faces, &result, next_id);
                Ok(())
            }
            _ => Err("not a NURBS solid".into()),
        }
    }

    pub(crate) fn nurbs_fillet_update(&mut self, mouse: egui::Pos2) {
        let (start, max_r, prev_peak, num_buf) = {
            let d = match self.tool.current.as_nurbs_fillet() {
                Some(d) => d,
                None => return,
            };
            (
                d.start_mouse,
                d.max_radius,
                d.radius.peak(),
                d.num_buf.clone(),
            )
        };
        // Typed peak wins over mouse (poly G/R/S parity). Scales law via with_peak.
        if let Some(typed) = num_buf.parse::<f32>().ok().filter(|v| v.is_finite()) {
            let r = typed.clamp(0.0, max_r);
            if (r - prev_peak).abs() < 1e-6 {
                return;
            }
            if let Some(d) = self.tool.current.as_nurbs_fillet_mut() {
                d.radius = d.radius.with_peak(r);
            }
            self.nurbs_fillet_regenerate();
            return;
        }
        // First frame: latch start if still zeroed
        if let Some(d) = self.tool.current.as_nurbs_fillet_mut() {
            if d.start_mouse.x == 0.0 && d.start_mouse.y == 0.0 {
                d.start_mouse = mouse;
                return;
            }
        }
        let dx = mouse.x - start.x;
        // Faster drag scale so Plasticity-size radii are reachable without huge mouse travel
        let r = (dx.abs() * self.viewport.camera.distance * 0.006).clamp(0.0, max_r);
        if (r - prev_peak).abs() < 0.0015 {
            return;
        }
        if let Some(d) = self.tool.current.as_nurbs_fillet_mut() {
            d.radius = d.radius.with_peak(r);
        }
        self.nurbs_fillet_regenerate();
    }

    /// Numeric peak-radius entry during live fillet (digits / `.` / Backspace).
    pub(crate) fn nurbs_fillet_num_input(&mut self, ch: char) {
        {
            let Some(d) = self.tool.current.as_nurbs_fillet_mut() else {
                return;
            };
            if ch == '\x08' {
                d.num_buf.pop();
            } else if ch.is_ascii_digit() || ch == '.' {
                if ch == '.' && d.num_buf.contains('.') {
                    return;
                }
                if d.num_buf.len() < 12 {
                    d.num_buf.push(ch);
                }
            } else {
                return;
            }
            let max_r = d.max_radius;
            if let Ok(v) = d.num_buf.parse::<f32>() {
                if v.is_finite() {
                    let peak = v.clamp(0.0, max_r);
                    d.radius = d.radius.with_peak(peak);
                }
            }
        }
        self.nurbs_fillet_regenerate();
    }

    pub(crate) fn nurbs_fillet_confirm(&mut self) {
        use core_math::nurbs_fillet::{fillet_op_from_origins, Radius};
        let (tool_radius, solid, edges, max_radius, faces_before) =
            match self.tool.current.as_nurbs_fillet() {
                Some(d) => (
                    d.radius.clone(),
                    d.solid,
                    d.edges.clone(),
                    d.max_radius,
                    d.faces_snapshot.clone(),
                ),
                None => return,
            };
        if tool_radius.is_near_zero() {
            self.nurbs_fillet_cancel();
            return;
        }
        // Prefer law from FaceOrigin on the confirmed solid (matches what was applied).
        // Fall back to the tool's live Radius (Const or Linear).
        let radius = self
            .scene
            .scene_graph
            .get(solid)
            .and_then(|node| match &node.payload {
                NodePayload::NurbsSolid { faces, .. } => {
                    fillet_op_from_origins(&faces_before, faces).map(|op| op.radius)
                }
                _ => None,
            })
            .unwrap_or(tool_radius);
        self.commit_episode();
        self.tool.current = ActiveTool::Idle;
        // Adjust-last-op panel: re-solve from pre-fillet base with full Radius law.
        self.ui.nurbs_fillet_adjust = Some(crate::NurbsFilletAdjust {
            solid,
            faces_before,
            edges,
            radius: radius.clone(),
            max_radius,
            needs_settle: false,
        });
        // Drop stale UV-edge selection (ghost free-end triangle legs after notch).
        self.nurbs_clear_selection();
        self.nurbs_prune_selection(solid);
        let label = match &radius {
            Radius::Const(v) => format!("r={v:.3}"),
            Radius::Linear { r0, r1 } => format!("linear {r0:.3}→{r1:.3}"),
            Radius::Law(s) => format!("law {} stn peak={:.3}", s.len(), radius.peak()),
        };
        self.toast(&format!("Fillet confirmed {label} · Adjust panel open"));
    }

    pub(crate) fn nurbs_fillet_adjust_active(&self) -> bool {
        let Some(a) = self.ui.nurbs_fillet_adjust.as_ref() else {
            return false;
        };
        if self.tool.current.is_nurbs_fillet() {
            return false;
        }
        self.scene.scene_graph.get(a.solid).is_some()
            && self.sel.usel.active_node() == Some(a.solid)
    }

    /// Shared last-fillet UI + apply (Tool Parameters dock + legacy float).
    pub(crate) fn nurbs_fillet_adjust_panel(&mut self, ui: &mut eframe::egui::Ui) -> bool {
        use core_math::nurbs_fillet::{resolve_edges_fillet_from_base_radius, Radius};
        use eframe::egui;
        let mut a = match self.ui.nurbs_fillet_adjust.clone() {
            Some(a) => a,
            None => return false,
        };
        if self.scene.scene_graph.get(a.solid).is_none()
            || self.sel.usel.active_node() != Some(a.solid)
        {
            self.ui.nurbs_fillet_adjust = None;
            return false;
        }
        if self.tool.current.is_nurbs_fillet() {
            return false;
        }

        let mut changed = false;
        let mut released = false;
        let mut close = false;
        let max_r = a.max_radius.max(0.02);
        ui.label(
            egui::RichText::new("Last · Fillet")
                .size(11.0)
                .color(egui::Color32::from_rgb(160, 170, 190)),
        );
        ui.add_space(2.0);
        ui.horizontal(|ui| {
            ui.label("Law");
            let is_const = matches!(a.radius, Radius::Const(_));
            let is_linear = matches!(a.radius, Radius::Linear { .. });
            let is_law = matches!(a.radius, Radius::Law(_));
            if ui.selectable_label(is_const, "Const").clicked() && !is_const {
                let peak = a.radius.peak().clamp(0.0, max_r);
                a.radius = Radius::Const(peak);
                changed = true;
                released = true;
            }
            if ui.selectable_label(is_linear, "Linear").clicked() && !is_linear {
                let peak = a.radius.peak().clamp(0.0, max_r).max(0.01);
                a.radius = Radius::Linear {
                    r0: (peak * 0.5).max(0.005),
                    r1: peak,
                };
                changed = true;
                released = true;
            }
            if ui
                .selectable_label(is_law, "Law")
                .on_hover_text("Multi-station law — peak slider scales all samples")
                .clicked()
                && !is_law
            {
                let peak = a.radius.peak().clamp(0.0, max_r).max(0.01);
                a.radius = Self::nurbs_fillet_seed_law(peak, 3);
                changed = true;
                released = true;
            }
        });

        match &mut a.radius {
            Radius::Const(r) => {
                let resp = ui.add(
                    egui::Slider::new(r, 0.0..=max_r)
                        .text("Radius")
                        .logarithmic(false),
                );
                changed |= resp.changed();
                released |= resp.drag_stopped() || resp.lost_focus();
            }
            Radius::Linear { r0, r1 } => {
                let resp0 = ui.add(
                    egui::Slider::new(r0, 0.0..=max_r)
                        .text("R0 (start)")
                        .logarithmic(false),
                );
                let resp1 = ui.add(
                    egui::Slider::new(r1, 0.0..=max_r)
                        .text("R1 (end)")
                        .logarithmic(false),
                );
                changed |= resp0.changed() || resp1.changed();
                released |= resp0.drag_stopped()
                    || resp0.lost_focus()
                    || resp1.drag_stopped()
                    || resp1.lost_focus();
            }
            Radius::Law(samples) => {
                let peak = samples
                    .iter()
                    .map(|(_, r)| *r)
                    .fold(0.0f32, f32::max)
                    .max(1e-6);
                let mut peak_edit = peak.clamp(0.0, max_r);
                let resp = ui.add(
                    egui::Slider::new(&mut peak_edit, 0.0..=max_r)
                        .text("Peak (scale law)")
                        .logarithmic(false),
                );
                if resp.changed() {
                    let s = peak_edit / peak;
                    for (_, r) in samples.iter_mut() {
                        *r = (*r * s).max(0.0);
                    }
                    changed = true;
                }
                released |= resp.drag_stopped() || resp.lost_focus();
                ui.label(format!("{} station(s) · monotone only", samples.len()));
            }
        }

        ui.label(format!(
            "{} edge(s) · peak {:.3} · max {max_r:.3}",
            a.edges.len(),
            a.radius.peak()
        ));
        ui.separator();
        if ui
            .button("Remove fillet")
            .on_hover_text("Radius → 0 — restore solid before this fillet.")
            .clicked()
        {
            a.radius = Radius::Const(0.0);
            changed = true;
            released = true;
        }
        if ui.button("Done").clicked() {
            close = true;
        }

        if changed {
            a.needs_settle = true;
            let solid = a.solid;
            let edge_usize: Vec<(usize, u8)> =
                a.edges.iter().map(|&(fi, ei)| (fi as usize, ei)).collect();
            if a.radius.is_near_zero() {
                if let Some(node) = self.scene.scene_graph.get_mut(solid) {
                    if let NodePayload::NurbsSolid { faces, .. } = &mut node.payload {
                        *faces = a.faces_before.clone();
                    }
                }
            } else {
                let next_id = a
                    .faces_before
                    .iter()
                    .map(|f| f.id.0)
                    .max()
                    .unwrap_or(0)
                    + 1;
                match resolve_edges_fillet_from_base_radius(
                    &a.faces_before,
                    &edge_usize,
                    &a.radius,
                    next_id,
                ) {
                    Ok(out) => {
                        if let Some(node) = self.scene.scene_graph.get_mut(solid) {
                            if let NodePayload::NurbsSolid { faces, .. } = &mut node.payload {
                                *faces = out.faces;
                            }
                        }
                        if !out.faulty_vertices.is_empty() {
                            self.toast(&format!(
                                "Fillet partial: {} corner(s) failed",
                                out.faulty_vertices.len()
                            ));
                        }
                    }
                    Err(e) => {
                        use core_math::nurbs_fillet::FilletError;
                        let msg = match &e {
                            FilletError::NonMonotoneRadiusLaw => {
                                "Fillet: radius law must be monotone (no mid-span peak/valley yet)"
                                    .into()
                            }
                            FilletError::RadiusTooLarge { max } => {
                                format!("Fillet: radius too large (max {max:.3})")
                            }
                            other => format!("Fillet adjust failed: {other:?}"),
                        };
                        self.toast(&msg);
                    }
                }
            }
            self.nurbs_sync_mesh_cache(solid);
            self.mark_dirty(DirtyFlags::RENDER | DirtyFlags::MESH);
        }
        self.ui.nurbs_fillet_adjust = Some(a.clone());
        if released {
            if a.needs_settle {
                self.transaction_snapshot_only("Adjust Fillet");
                if let Some(adj) = self.ui.nurbs_fillet_adjust.as_mut() {
                    adj.needs_settle = false;
                }
            }
        }
        if close {
            self.ui.nurbs_fillet_adjust = None;
        }
        true
    }

    /// Legacy floating window (old UI only).
    pub(crate) fn draw_nurbs_fillet_adjust(&mut self, ctx: &eframe::egui::Context) {
        use eframe::egui;
        if std::env::var("AZRA_UI").ok().as_deref() == Some("v2") {
            return;
        }
        if !self.nurbs_fillet_adjust_active() {
            return;
        }
        egui::Window::new("Adjust Fillet")
            .id(egui::Id::new("nurbs_fillet_adjust"))
            .anchor(egui::Align2::LEFT_BOTTOM, [12.0, -12.0])
            .resizable(false)
            .collapsible(true)
            .show(ctx, |ui| {
                let _ = self.nurbs_fillet_adjust_panel(ui);
            });
    }

    pub(crate) fn nurbs_fillet_cancel(&mut self) {
        // Restore snapshot
        if let Some(d) = self.tool.current.as_nurbs_fillet() {
            let solid = d.solid;
            let snap = d.faces_snapshot.clone();
            if let Some(node) = self.scene.scene_graph.get_mut(solid) {
                if let NodePayload::NurbsSolid { faces, .. } = &mut node.payload {
                    *faces = snap;
                }
            }
            self.nurbs_sync_mesh_cache(solid);
            self.mark_dirty(DirtyFlags::RENDER | DirtyFlags::MESH);
        }
        if self.has_active_episode() {
            self.cancel_episode();
        }
        self.tool.current = ActiveTool::Idle;
        self.toast("Fillet cancelled");
    }

    /// Fillet one edge of a NurbsSolid: find orthogonal neighbour, apply kernel.
    pub(crate) fn nurbs_apply_edge_fillet(
        &mut self,
        solid: NodeId,
        face_a: u32,
        edge_a: u8,
        radius: f32,
    ) -> Result<(), String> {
        use core_math::nurbs_fillet::{
            apply_fillet_to_faces, face_edge_endpoints, face_outward_normal,
        };
        let node = self
            .scene
            .scene_graph
            .get(solid)
            .ok_or_else(|| "missing solid".to_string())?;
        let faces = match &node.payload {
            NodePayload::NurbsSolid { faces, .. } => faces,
            _ => return Err("not a NURBS solid".into()),
        };
        let fa = face_a as usize;
        if fa >= faces.len() {
            return Err("bad face index".into());
        }
        let (a0, a1) = face_edge_endpoints(faces[fa].surface.as_ref(), edge_a)
            .map_err(|e| format!("{e:?}"))?;
        let n0 = face_outward_normal(faces[fa].surface.as_ref()).map_err(|e| format!("{e:?}"))?;

        // Find neighbour face + edge sharing the same endpoints (within eps)
        const EPS2: f32 = 1e-6;
        let same_pt = |p: Vec3, q: Vec3| (p - q).length_squared() < EPS2;
        let mut found: Option<(usize, u8)> = None;
        for (fb, face) in faces.iter().enumerate() {
            if fb == fa {
                continue;
            }
            for eb in 0u8..4 {
                if let Ok((b0, b1)) = face_edge_endpoints(face.surface.as_ref(), eb) {
                    let match_fwd = same_pt(a0, b0) && same_pt(a1, b1);
                    let match_rev = same_pt(a0, b1) && same_pt(a1, b0);
                    if match_fwd || match_rev {
                        // Prefer orthogonal neighbour
                        if let Ok(n1) = face_outward_normal(face.surface.as_ref()) {
                            if n0.dot(n1).abs() < 0.15 {
                                found = Some((fb, eb));
                                break;
                            }
                        }
                    }
                }
            }
            if found.is_some() {
                break;
            }
        }
        let (face_b, edge_b) = found.ok_or_else(|| {
            "no orthogonal neighbour face for this edge (planar 90° only for now)".to_string()
        })?;

        // Mutate faces via payload
        let node = self
            .scene
            .scene_graph
            .get_mut(solid)
            .ok_or_else(|| "missing solid".to_string())?;
        match &mut node.payload {
            NodePayload::NurbsSolid { faces, .. } => {
                let next_id = faces.iter().map(|f| f.id.0).max().unwrap_or(0) + 1;
                apply_fillet_to_faces(faces, fa, edge_a, face_b, edge_b, radius, next_id)
                    .map_err(|e| format!("{e:?}"))?;
                Ok(())
            }
            _ => Err("not a NURBS solid".into()),
        }
    }
}
