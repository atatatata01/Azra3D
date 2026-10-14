//! AZRA_NURBSDIFF — automated end-to-end **NURBS tool-path** visual verification harness.
//!
//! Modeled on [`crate::visdiff`] (materials `AZRA_VISDIFF` precedent). Owner STATE RESET
//! 2026-08-01: **no NURBS tool milestone is done without a harness shot**. `core_math` unit
//! tests alone never count again — that is how "done" shipped broken.
//!
//! Run: `AZRA_NURBSDIFF=<out_dir> cargo run -p app` (from `core_engine/`).
//! The app drives ITSELF via production APIs, captures viewport screenshots, exits.
//!
//! ## Frame plan (P0 baseline)
//!
//! | Frame | Action |
//! |---|---|
//! | 30 | Create NURBS cube (`NurbsSolid`) · frame camera |
//! | 80 | Fillet one vertical edge r=0.2 · settle |
//! | 140 | Shot `01_vertical_r02` |
//! | 160 | Fillet adjacent top edge r=0.2 · settle |
//! | 220 | Shot `02_vertical_then_top_r02` |
//! | 240 | Exit |
//!
//! Later milestones append shots (primitives, junction both-orders, remove fillet, …)
//! without renaming the P0 pair — they are the regression baseline.
//!
//! Console prints `[NURBSDIFF]` markers per step. Zero cost when env unset.

use glam::{Quat, Vec3};

use crate::{AzraApp, DirtyFlags, ModelingContext, NurbsWorkMode, ViewportRenderMode};
use core_math::nurbs_fillet::{
    apply_edges_fillet_batch_radius, azra_edges_from_occt_mids, Radius,
};
use core_math::scene_graph::NodePayload;

impl AzraApp {
    /// One tick of the NURBS harness state machine. Called from `update()` only when
    /// `AZRA_NURBSDIFF` is set (single env check at call site — zero cost otherwise).
    pub(crate) fn nurbsdiff_tick(&mut self) {
        let out_dir = match std::env::var("AZRA_NURBSDIFF") {
            Ok(d) if !d.is_empty() => std::path::PathBuf::from(d),
            _ => return,
        };
        self.nurbsdiff_frame += 1;
        let f = self.nurbsdiff_frame;

        let shot = |app: &mut AzraApp, name: &str| {
            let p = out_dir.join(format!("{name}.png"));
            eprintln!(
                "[NURBSDIFF] frame {} shot -> {}",
                app.nurbsdiff_frame,
                p.display()
            );
            app.visdiff_shot = Some(p);
        };

        match f {
            // ---- Setup: empty scene → NURBS cube solid ---------------------------------
            10 => {
                std::fs::create_dir_all(&out_dir).ok();
                // Clear default SDF prims so the shot is pure NURBS.
                self.scene.primitives.clear();
                self.create_nurbs_surface("cube");
                // Frame camera on the unit cube (half-extents 0.5) — 3/4 view toward +X+Y+Z corner.
                self.viewport.camera.target = Vec3::ZERO;
                self.viewport.camera.distance = 3.2;
                // Yaw/pitch-ish orbit: look from (+X,+Y,+Z) toward origin.
                let yaw = std::f32::consts::FRAC_PI_4;
                let pitch = -0.45f32;
                self.viewport.camera.orbit =
                    Quat::from_rotation_y(yaw) * Quat::from_rotation_x(pitch);
                self.render.viewport_mode = ViewportRenderMode::Hybrid;
                self.modeling_context = ModelingContext::Nurbs;
                self.nurbs_work_mode = NurbsWorkMode::Edge;
                self.mark_dirty(DirtyFlags::RENDER | DirtyFlags::MESH);
                eprintln!("[NURBSDIFF] created NURBS cube · camera framed");
            }
            // ---- Fillet vertical edge (FreeCAD edge 6 mid_occt = (1,1,0.5)) ------------
            60 => {
                match self.nurbsdiff_apply_edge_fillet_occt_mids(
                    &[[1.0, 1.0, 0.5]],
                    0.2,
                    "vertical r=0.2",
                ) {
                    Ok(n) => eprintln!("[NURBSDIFF] vertical fillet ok · faces={n}"),
                    Err(e) => {
                        eprintln!("[NURBSDIFF] FAIL vertical fillet: {e}");
                        std::process::exit(1);
                    }
                }
            }
            140 => shot(self, "01_vertical_r02"),
            // ---- Adjacent top edge (FreeCAD edge 5 mid_occt = (1,0.5,1)) ---------------
            // Sequential path — this is the owner junction repro (gap/gouge until P3).
            160 => {
                // Sequential path — owner repro for P3 junction (vertical then top).
                match self.nurbsdiff_apply_edge_fillet_occt_mids(
                    &[[1.0, 0.5, 1.0]],
                    0.2,
                    "adjacent top r=0.2",
                ) {
                    Ok(n) => {
                        self.nurbsdiff_seq_ok = true;
                        eprintln!("[NURBSDIFF] sequential top fillet ok · faces={n}");
                    }
                    Err(e) => {
                        self.nurbsdiff_seq_ok = false;
                        eprintln!("[NURBSDIFF] WARN sequential top fillet: {e}");
                    }
                }
            }
            220 => shot(self, "02_vertical_then_top_r02"),
            // ---- Plasticity-class top loop (4 top edges, FreeCAD top_loop_r02) --------
            240 => {
                self.nurbsdiff_clear_nurbs_nodes();
                self.create_nurbs_surface("cube");
                self.viewport.camera.distance = 3.2;
                let yaw = std::f32::consts::FRAC_PI_4;
                let pitch = -0.45f32;
                self.viewport.camera.orbit =
                    Quat::from_rotation_y(yaw) * Quat::from_rotation_x(pitch);
                self.mark_dirty(DirtyFlags::RENDER | DirtyFlags::MESH);
                eprintln!("[NURBSDIFF] fresh cube for top-loop");
            }
            250 => {
                // FreeCAD top loop mids: edges 1,5,9,11
                match self.nurbsdiff_apply_edge_fillet_occt_mids(
                    &[
                        [0.0, 0.5, 1.0],
                        [1.0, 0.5, 1.0],
                        [0.5, 0.0, 1.0],
                        [0.5, 1.0, 1.0],
                    ],
                    0.2,
                    "top_loop r=0.2",
                ) {
                    Ok(n) => {
                        self.nurbsdiff_loop_ok = true;
                        eprintln!("[NURBSDIFF] top_loop fillet ok · faces={n}");
                    }
                    Err(e) => {
                        self.nurbsdiff_loop_ok = false;
                        eprintln!("[NURBSDIFF] WARN top_loop: {e}");
                    }
                }
            }
            300 => shot(self, "07_top_loop_r02"),
            // ---- P1 solid primitives (NurbsSolid, not legacy NurbsBody) -----------------
            320 => {
                self.nurbsdiff_clear_nurbs_nodes();
                self.create_nurbs_surface("cylinder");
                self.viewport.camera.distance = 4.5;
                self.mark_dirty(DirtyFlags::RENDER | DirtyFlags::MESH);
                eprintln!("[NURBSDIFF] created NURBS cylinder solid");
            }
            360 => shot(self, "03_cylinder_solid"),
            380 => {
                match self.nurbsdiff_apply_cylinder_rim(0, 2, 0.15) {
                    Ok(n) => {
                        self.nurbsdiff_rim_ok = true;
                        eprintln!("[NURBSDIFF] cylinder top rim fillet ok · faces={n}");
                    }
                    Err(e) => {
                        self.nurbsdiff_rim_ok = false;
                        eprintln!("[NURBSDIFF] WARN cylinder rim fillet: {e}");
                    }
                }
            }
            440 => shot(self, "04_cylinder_rim_r015"),
            460 => {
                self.nurbsdiff_clear_nurbs_nodes();
                self.create_nurbs_surface("sphere");
                self.viewport.camera.distance = 3.5;
                self.mark_dirty(DirtyFlags::RENDER | DirtyFlags::MESH);
                eprintln!("[NURBSDIFF] created NURBS sphere solid");
            }
            500 => shot(self, "05_sphere_solid"),
            520 => {
                self.nurbsdiff_clear_nurbs_nodes();
                self.create_nurbs_surface("torus");
                self.viewport.camera.distance = 4.0;
                self.mark_dirty(DirtyFlags::RENDER | DirtyFlags::MESH);
                eprintln!("[NURBSDIFF] created NURBS torus solid");
            }
            560 => shot(self, "06_torus_solid"),
            600 => {
                let seq = if self.nurbsdiff_seq_ok { "OK" } else { "FAIL" };
                let rim = if self.nurbsdiff_rim_ok { "OK" } else { "FAIL" };
                let loop_ = if self.nurbsdiff_loop_ok { "OK" } else { "FAIL" };
                eprintln!(
                    "[NURBSDIFF] complete — sequential={seq} top_loop={loop_} rim={rim} · exiting 0"
                );
                std::process::exit(0);
            }
            _ => {}
        }
    }

    fn nurbsdiff_clear_nurbs_nodes(&mut self) {
        use core_math::scene_graph::NodeId;
        let root = self.scene.scene_graph.root();
        let kids: Vec<NodeId> = self.scene.scene_graph.children(root).to_vec();
        for nid in kids {
            let is_nurbs = matches!(
                self.scene.scene_graph.get(nid).map(|n| &n.payload),
                Some(NodePayload::NurbsSolid { .. } | NodePayload::NurbsBody { .. })
            );
            if is_nurbs {
                self.scene.scene_graph.remove(nid);
            }
        }
        self.nurbs_sel.clear_sub();
    }

    fn nurbsdiff_apply_cylinder_rim(
        &mut self,
        wall_fi: usize,
        wall_edge: u8,
        radius: f32,
    ) -> Result<usize, String> {
        use core_math::nurbs_fillet::apply_cylinder_rim_fillet_to_solid;
        let solid = self
            .nurbsdiff_solid_nid()
            .ok_or_else(|| "no NurbsSolid".to_string())?;
        let n = {
            let node = self
                .scene
                .scene_graph
                .get_mut(solid)
                .ok_or_else(|| "missing solid".to_string())?;
            match &mut node.payload {
                NodePayload::NurbsSolid { faces, .. } => {
                    let next_id = faces.iter().map(|f| f.id.0).max().unwrap_or(0) + 1;
                    apply_cylinder_rim_fillet_to_solid(
                        faces, wall_fi, wall_edge, radius, next_id,
                    )
                    .map_err(|e| format!("{e:?}"))?;
                    faces.len()
                }
                _ => return Err("not NurbsSolid".into()),
            }
        };
        self.nurbs_sync_mesh_cache(solid);
        self.mark_dirty(DirtyFlags::RENDER | DirtyFlags::MESH);
        Ok(n)
    }

    /// Resolve the active NURBS solid node (first selected, else first NurbsSolid in graph).
    fn nurbsdiff_solid_nid(&self) -> Option<core_math::scene_graph::NodeId> {
        if let Some(nid) = self.nurbs_solid_root() {
            return Some(nid);
        }
        // Walk scene graph for first NurbsSolid.
        let root = self.scene.scene_graph.root();
        let mut stack = vec![root];
        while let Some(nid) = stack.pop() {
            if matches!(
                self.scene.scene_graph.get(nid).map(|n| &n.payload),
                Some(NodePayload::NurbsSolid { .. })
            ) {
                return Some(nid);
            }
            for &c in self.scene.scene_graph.children(nid) {
                stack.push(c);
            }
        }
        None
    }

    /// Map FreeCAD/OCCT edge midpoints → AZRA edge tags, apply const-radius fillet
    /// via the same kernel path the tool uses (batch or sequential re-batch).
    fn nurbsdiff_apply_edge_fillet_occt_mids(
        &mut self,
        occt_mids: &[[f32; 3]],
        radius: f32,
        label: &str,
    ) -> Result<usize, String> {
        use core_math::nurbs_fillet::apply_edges_fillet_sequential_const;
        let solid = self
            .nurbsdiff_solid_nid()
            .ok_or_else(|| "no NurbsSolid in scene".to_string())?;
        let (edges, next_id, n_before, has_prior) = {
            let node = self
                .scene
                .scene_graph
                .get(solid)
                .ok_or_else(|| "missing solid node".to_string())?;
            let faces = match &node.payload {
                NodePayload::NurbsSolid { faces, .. } => faces,
                _ => return Err("node is not NurbsSolid".into()),
            };
            let edges = azra_edges_from_occt_mids(faces, occt_mids, 0.08);
            if edges.is_empty() {
                return Err(format!(
                    "{label}: could not map OCCT mids {occt_mids:?} to solid edges"
                ));
            }
            let next_id = faces.iter().map(|f| f.id.0).max().unwrap_or(0) + 1;
            let has_prior = faces.iter().any(|f| f.origin.fillet_between().is_some());
            (edges, next_id, faces.len(), has_prior)
        };
        eprintln!(
            "[NURBSDIFF] {label}: edges={edges:?} r={radius} faces_before={n_before} prior={has_prior}"
        );
        let n_faces = {
            let node = self
                .scene
                .scene_graph
                .get_mut(solid)
                .ok_or_else(|| "missing solid node".to_string())?;
            match &mut node.payload {
                NodePayload::NurbsSolid { faces, .. } => {
                    if has_prior {
                        apply_edges_fillet_sequential_const(faces, &edges, radius, next_id)
                            .map_err(|e| format!("{label}: sequential {e:?}"))?;
                    } else {
                        apply_edges_fillet_batch_radius(
                            faces,
                            &edges,
                            &Radius::Const(radius),
                            next_id,
                        )
                        .map_err(|e| format!("{label}: kernel {e:?}"))?;
                    }
                    faces.len()
                }
                _ => return Err("node is not NurbsSolid".into()),
            }
        };
        self.nurbs_sync_mesh_cache(solid);
        self.mark_dirty(DirtyFlags::RENDER | DirtyFlags::MESH);
        Ok(n_faces)
    }
}
