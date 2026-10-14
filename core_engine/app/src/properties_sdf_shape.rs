//! SDF shape, modifiers, sculpt, boolean — extracted from properties_sdf.rs.

use eframe::egui;
use glam::Vec3;
use core_math::primitives::{
    SdfShape, BooleanOp, BevelMode, BlendFalloff, BlendProfile, FalloffAxis, ModifierType,
    MaskShape, SculptOp,
};
use crate::*;
use crate::properties_sdf::PropsContext;
use crate::ui_panels::draw_modifier_editor;

impl AzraApp {
    pub(crate) fn draw_props_sdf_shape(&mut self, ui: &mut egui::Ui, pctx: &mut PropsContext, idx: usize) {
        let is_inst = self.scene.primitives[idx].instance_master_id.is_some();
        #[allow(deprecated)]
        if is_inst { ui.set_enabled(false); }
        // LAB poly→SDF live-modifier actions (deferred past the &mut shape borrow below).
        let mut lab_unlink = false;
        let mut lab_refresh = false;
        // Source poly name for the linked-status line (read before the &mut borrow).
        let lab_link_name: Option<String> = match &self.scene.primitives[idx].shape {
            SdfShape::MeshVolume { source_poly: Some(nid), .. } =>
                Some(self.scene.scene_graph.get(*nid)
                    .map(|n| n.payload.display_name().to_string())
                    .unwrap_or_else(|| "(deleted)".into())),
            _ => None,
        };
        // ---- Shape ---- (disabled for instances)
        egui::CollapsingHeader::new(egui::RichText::new("🔷 Shape").strong().size(13.0).color(egui::Color32::from_rgb(30,136,229)))
            .default_open(true).show(ui, |ui| {
            ui.horizontal(|ui|{
                ui.label("Name:");
                // Route the rename through the pctx path so properties.rs mirrors it to the graph
                // (source of truth) and records ONE undo entry. A bare flat-cache write (review #3)
                // set no pctx flag → never mirrored, no undo, silently reverted by the next
                // sync_flat_cache and lost on save. gained_focus opens the episode; changed marks it.
                let r = ui.text_edit_singleline(&mut self.scene.primitives[idx].name);
                if r.gained_focus() { pctx.drag_started = true; }
                if r.changed() { pctx.changed = true; pctx.desc = "Rename".into(); }
            });
            match &mut self.scene.primitives[idx].shape {
                SdfShape::Sphere{radius} => {
                    let r=ui.add(egui::Slider::new(radius,0.05..=2.0).text("Radius"));
                    if r.drag_started(){pctx.drag_started=true;} if r.changed(){pctx.changed=true;pctx.desc="Resize".into();}
                }
                SdfShape::Box{half_extents:h, bevel} => {
                    for (v,l) in [(&mut h.x,"W"),(&mut h.y,"H"),(&mut h.z,"D")] {
                        let r=ui.add(egui::Slider::new(v,0.05..=2.0).text(l));
                        if r.drag_started(){pctx.drag_started=true;} if r.changed(){pctx.changed=true;pctx.desc="Resize".into();}
                    }
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        ui.label("Edge Bevels:");
                        ui.add_space(8.0);
                        ui.label(egui::RichText::new("Up:").small().weak());
                        for (ax, lbl) in [(0u8, "Y"), (1, "X"), (2, "Z")] {
                            if ui.selectable_label(bevel.up_axis == ax, lbl).clicked() && bevel.up_axis != ax {
                                bevel.up_axis = ax;
                                pctx.changed = true; pctx.desc = "Bevel axis".into();
                            }
                        }
                    });
                    // Algorithm: Per-Edge (independent) vs Uniform (exact round-box for all edges).
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new("Algo:").small().weak());
                        use core_math::sdf_shapes::BoxBevelAlgo;
                        for algo in [BoxBevelAlgo::PerEdge, BoxBevelAlgo::Uniform] {
                            let selected = bevel.algo == algo;
                            if ui
                                .selectable_label(selected, algo.label())
                                .on_hover_text(algo.hint())
                                .clicked()
                                && !selected
                            {
                                bevel.algo = algo;
                                pctx.changed = true;
                                pctx.desc = "Bevel algorithm".into();
                            }
                        }
                    });
                    // Per-group bevel max: each edge group is limited by the two
                    // half-extents of the face cross-section, not by all three axes.
                    // up_axis 0=Y, 1=X, 2=Z  →  top/bot faces are perpendicular to up
                    let (top_max, bot_max, vert_max) = {
                        let (a, b, c) = match bevel.up_axis {
                            1 => (h.x, h.y, h.z), // X is up → top/bot on YZ, vert on YZ
                            2 => (h.z, h.x, h.y), // Z is up → top/bot on XY, vert on XY
                            _ => (h.y, h.x, h.z), // Y is up → top/bot on XZ, vert on XZ
                        };
                        // top/bot edges lie on cross-section (b,c); vert edges also limited by (b,c)
                        // Additionally, vert edges can't exceed the up-axis half-extent
                        let face_min = b.min(c);
                        (face_min, face_min, face_min.min(a))
                    };
                    // Group labels change based on up_axis
                    let (top_lbl, bot_lbl, vert_lbl) = match bevel.up_axis {
                        1 => ("+X", "-X", "H"),  // X up
                        2 => ("+Z", "-Z", "H"),  // Z up
                        _ => ("Top", "Bot", "Vert"),
                    };
                    for (lbl, base, max_r) in [(top_lbl,4usize,top_max),(bot_lbl,0usize,bot_max),(vert_lbl,8usize,vert_max)] {
                        ui.horizontal(|ui|{
                            let mut group_r = bevel.radii[base];
                            let rr=ui.add(egui::Slider::new(&mut group_r,0.0..=max_r).text(lbl));
                            if rr.drag_started(){pctx.drag_started=true;}
                            if rr.changed(){
                                for ei in base..base+4 {
                                    bevel.radii[ei]=group_r;
                                    // Auto-set mode to Round when radius > 0 (mode=None ignores radius)
                                    if group_r > 0.0 && bevel.modes[ei]==BevelMode::None { bevel.modes[ei]=BevelMode::Round; }
                                    if group_r == 0.0 { bevel.modes[ei]=BevelMode::None; }
                                }
                                pctx.changed=true; pctx.desc="Bevel".into();
                            }
                            let mut ch_flag = bevel.modes[base]==BevelMode::Chamfer;
                            if ui.checkbox(&mut ch_flag,"Ch").changed(){
                                let m=if ch_flag{BevelMode::Chamfer}else{BevelMode::Round};
                                for ei in base..base+4 { bevel.modes[ei]=m; }
                                pctx.changed=true; pctx.desc="Bevel mode".into();
                            }
                        });
                    }
                    // Per-edge-group bevel profiles: concave ← chamfer → fillet
                    let p = &mut self.scene.primitives[idx];
                    let rp = ui.add(egui::Slider::new(&mut p.bevel_profile_top, -1.0..=1.0)
                        .text("Profile Top").step_by(0.01))
                        .on_hover_text("Concave (-1) → Chamfer (0) → Fillet (+1)");
                    if rp.drag_started() { pctx.drag_started = true; }
                    if rp.changed() { pctx.changed = true; pctx.desc = "Bevel profile".into(); }
                    let rp = ui.add(egui::Slider::new(&mut p.bevel_profile_bot, -1.0..=1.0)
                        .text("Profile Bot").step_by(0.01))
                        .on_hover_text("Concave (-1) → Chamfer (0) → Fillet (+1)");
                    if rp.drag_started() { pctx.drag_started = true; }
                    if rp.changed() { pctx.changed = true; pctx.desc = "Bevel profile".into(); }
                    let rp = ui.add(egui::Slider::new(&mut p.bevel_profile_vert, -1.0..=1.0)
                        .text("Profile Vert").step_by(0.01))
                        .on_hover_text("Concave (-1) → Chamfer (0) → Fillet (+1)");
                    if rp.drag_started() { pctx.drag_started = true; }
                    if rp.changed() { pctx.changed = true; pctx.desc = "Bevel profile".into(); }
                    // Per-edge-group chamfer angle (0°-90°)
                    let rp = ui.add(egui::Slider::new(&mut p.chamfer_angle_top, 1.0..=89.0)
                        .text("Angle Top").suffix("°").step_by(0.5))
                        .on_hover_text("Chamfer angle: 30°=shallow, 45°=standard, 80°=steep");
                    if rp.drag_started() { pctx.drag_started = true; }
                    if rp.changed() { pctx.changed = true; pctx.desc = "Chamfer angle".into(); }
                    let rp = ui.add(egui::Slider::new(&mut p.chamfer_angle_bot, 1.0..=89.0)
                        .text("Angle Bot").suffix("°").step_by(0.5))
                        .on_hover_text("Chamfer angle: 30°=shallow, 45°=standard, 80°=steep");
                    if rp.drag_started() { pctx.drag_started = true; }
                    if rp.changed() { pctx.changed = true; pctx.desc = "Chamfer angle".into(); }
                    let rp = ui.add(egui::Slider::new(&mut p.chamfer_angle_vert, 1.0..=89.0)
                        .text("Angle Vert").suffix("°").step_by(0.5))
                        .on_hover_text("Chamfer angle: 30°=shallow, 45°=standard, 80°=steep");
                    if rp.drag_started() { pctx.drag_started = true; }
                    if rp.changed() { pctx.changed = true; pctx.desc = "Chamfer angle".into(); }
                    // ---- Advanced: per-edge bevel radii (all 12 edges individually) ----
                    // Re-borrow the bevel: `p` above borrowed the prim for the profile/angle
                    // sliders, so reach back into the shape here for the per-edge controls.
                    if let SdfShape::Box { bevel, .. } = &mut self.scene.primitives[idx].shape {
                        egui::CollapsingHeader::new(egui::RichText::new("Advanced (per-edge)").small().weak())
                            .id_salt("box_bevel_per_edge")
                            .default_open(false)
                            .show(ui, |ui| {
                                ui.label(egui::RichText::new("Set each of the 12 box edges independently. Overrides the grouped sliders above.").small().weak());
                                // Edge layout (in the bevel's permuted frame; see BoxBevel doc):
                                //   Bot 0..4, Top 4..8 lie on the cross-section faces (clamped by face_min);
                                //   Vert 8..12 run along the up-axis (clamped by face_min.min(up half-extent)).
                                // Labels mirror the grouped slider names so they read consistently with up_axis.
                                let (top_grp, bot_grp) = match bevel.up_axis {
                                    1 => ("+X face", "-X face"),
                                    2 => ("+Z face", "-Z face"),
                                    _ => ("Top face", "Bottom face"),
                                };
                                // (group_label, slider_label, edge_index, max_radius)
                                let edge_rows: [(&str, &str, usize, f32); 12] = [
                                    (bot_grp, "near (a-)",  0, bot_max),
                                    (bot_grp, "far (a+)",   1, bot_max),
                                    (bot_grp, "left (b-)",  2, bot_max),
                                    (bot_grp, "right (b+)", 3, bot_max),
                                    (top_grp, "near (a-)",  4, top_max),
                                    (top_grp, "far (a+)",   5, top_max),
                                    (top_grp, "left (b-)",  6, top_max),
                                    (top_grp, "right (b+)", 7, top_max),
                                    ("Vertical", "b- a-",   8, vert_max),
                                    ("Vertical", "b+ a-",   9, vert_max),
                                    ("Vertical", "b- a+",  10, vert_max),
                                    ("Vertical", "b+ a+",  11, vert_max),
                                ];
                                let mut last_grp = "";
                                for (grp, lbl, ei, max_r) in edge_rows {
                                    if grp != last_grp {
                                        ui.add_space(2.0);
                                        ui.label(egui::RichText::new(grp).small().strong());
                                        last_grp = grp;
                                    }
                                    ui.horizontal(|ui| {
                                        let mut r_val = bevel.radii[ei];
                                        let rr = ui.add(egui::Slider::new(&mut r_val, 0.0..=max_r).text(lbl));
                                        if rr.drag_started() { pctx.drag_started = true; }
                                        if rr.changed() {
                                            bevel.radii[ei] = r_val;
                                            // Match grouped-slider semantics: a radius needs a non-None
                                            // mode to take effect, and 0 radius reverts the edge to sharp.
                                            if r_val > 0.0 && bevel.modes[ei] == BevelMode::None { bevel.modes[ei] = BevelMode::Round; }
                                            if r_val == 0.0 { bevel.modes[ei] = BevelMode::None; }
                                            pctx.changed = true; pctx.desc = "Bevel edge".into();
                                        }
                                        // Per-edge mode: Round (default) vs Chamfer.
                                        let mut ch_flag = bevel.modes[ei] == BevelMode::Chamfer;
                                        if ui.checkbox(&mut ch_flag, "Ch").changed() {
                                            bevel.modes[ei] = if ch_flag { BevelMode::Chamfer } else { BevelMode::Round };
                                            pctx.changed = true; pctx.desc = "Bevel edge mode".into();
                                        }
                                    });
                                }
                            });
                    }
                }
                SdfShape::Cylinder{radius,half_height}|SdfShape::Cone{radius,half_height}|SdfShape::Capsule{radius,half_height} => {
                    for (v,l) in [(&mut *radius,"Radius"),(&mut *half_height,"Height")] {
                        let r=ui.add(egui::Slider::new(v,0.05..=2.0).text(l));
                        if r.drag_started(){pctx.drag_started=true;} if r.changed(){pctx.changed=true;pctx.desc="Resize".into();}
                    }
                }
                SdfShape::Torus{major_radius,minor_radius} => {
                    for (v,l) in [(&mut *major_radius,"Major"),(&mut *minor_radius,"Minor")] {
                        let r=ui.add(egui::Slider::new(v,0.02..=2.0).text(l));
                        if r.drag_started(){pctx.drag_started=true;} if r.changed(){pctx.changed=true;pctx.desc="Resize".into();}
                    }
                }
                SdfShape::Stair{half_extents:h,num_steps} => {
                    for (v,l) in [(&mut h.x,"Width"),(&mut h.y,"Height"),(&mut h.z,"Depth")] {
                        let r=ui.add(egui::Slider::new(v,0.1..=5.0).text(l));
                        if r.drag_started(){pctx.drag_started=true;} if r.changed(){pctx.changed=true;pctx.desc="Resize".into();}
                    }
                    let mut ns = *num_steps as i32;
                    let r=ui.add(egui::Slider::new(&mut ns,2..=20).text("Steps"));
                    if r.changed(){*num_steps=ns.max(1) as u32;pctx.changed=true;pctx.desc="Steps".into();}
                    if r.drag_started(){pctx.drag_started=true;}
                    let step_h = h.y * 2.0 / (*num_steps).max(1) as f32;
                    ui.label(format!("Step height: {:.3}", step_h));
                }
                SdfShape::SpiralStair{radius,half_height,num_steps,step_width} => {
                    for (v,l,range) in [(&mut *radius,"Radius",0.1..=5.0),(&mut *half_height,"Height",0.1..=5.0),(&mut *step_width,"Step Width",0.05..=1.0)] {
                        let r=ui.add(egui::Slider::new(v,range).text(l));
                        if r.drag_started(){pctx.drag_started=true;} if r.changed(){pctx.changed=true;pctx.desc="Resize".into();}
                    }
                    let mut ns = *num_steps as i32;
                    let r=ui.add(egui::Slider::new(&mut ns,4..=32).text("Steps"));
                    if r.changed(){*num_steps=ns.max(1) as u32;pctx.changed=true;pctx.desc="Steps".into();}
                    if r.drag_started(){pctx.drag_started=true;}
                    let step_h = *half_height * 2.0 / (*num_steps).max(1) as f32;
                    ui.label(format!("Step height: {:.3}", step_h));
                }
                SdfShape::Billboard{width,height,face_camera} => {
                    let r=ui.add(egui::Slider::new(width,0.1..=10.0).text("Width"));
                    if r.drag_started(){pctx.drag_started=true;} if r.changed(){pctx.changed=true;pctx.desc="Resize".into();}
                    let r=ui.add(egui::Slider::new(height,0.1..=10.0).text("Height"));
                    if r.drag_started(){pctx.drag_started=true;} if r.changed(){pctx.changed=true;pctx.desc="Resize".into();}
                    if ui.checkbox(face_camera, "Face Camera").changed() {
                        pctx.changed=true; pctx.desc="Face Camera".into();
                    }
                }
                SdfShape::CurveExtrude { half_depth, wall_t, rim, profile, open, thickness, tube } => {
                    // ---- Open / Closed (P2) ----
                    // Closed = filled polygon (shell/rim apply). Open = polyline offset by `thickness`
                    // into a flat ribbon/wall or a round tube (no interior fill).
                    ui.horizontal(|ui| {
                        if ui.selectable_label(!*open, "Closed").on_hover_text("Filled extruded polygon").clicked() && *open {
                            *open = false; pctx.changed = true; pctx.desc = "Curve closed".into();
                        }
                        if ui.selectable_label(*open, "Open").on_hover_text("Open polyline with wall/tube thickness").clicked() && !*open {
                            *open = true;
                            if *thickness <= 1e-5 { *thickness = 0.2; } // sensible default so it's visible
                            pctx.changed = true; pctx.desc = "Curve open".into();
                        }
                    });

                    if *open {
                        // Wall (flat ribbon, uses Depth for extrude) vs Tube (round cross-section).
                        ui.horizontal(|ui| {
                            if ui.selectable_label(!*tube, "Wall").on_hover_text("Flat ribbon extruded by Depth").clicked() && *tube {
                                *tube = false; pctx.changed = true; pctx.desc = "Curve wall".into();
                            }
                            if ui.selectable_label(*tube, "Tube").on_hover_text("Round cross-section swept along the path").clicked() && !*tube {
                                *tube = true; pctx.changed = true; pctx.desc = "Curve tube".into();
                            }
                        });
                        let tr = ui.add(egui::Slider::new(thickness, 0.01..=2.0)
                            .text(if *tube { "Tube diameter" } else { "Wall thickness" }));
                        if tr.drag_started() { pctx.drag_started = true; }
                        if tr.changed() { pctx.changed = true; pctx.desc = "Curve thickness".into(); }
                    }

                    // Depth = extrude half-height. Drives the wall's Y extent; ignored for a tube.
                    let depth_enabled = !*open || !*tube;
                    let r = ui.add_enabled(depth_enabled, egui::Slider::new(half_depth,0.01..=5.0).text("Depth"));
                    if r.drag_started(){pctx.drag_started=true;} if r.changed(){pctx.changed=true;pctx.desc="Resize".into();}

                    // Corners + Shell only apply to CLOSED profiles (the open-path offset auto-rounds
                    // convex corners, and there is no solid interior to hollow). Rim still applies to a
                    // wall's extrude cap, so it stays available below.
                    if !*open {
                    // ---- Corners (per-vertex round/chamfer, packed in bevel_radius sign) ----
                    // P1 UI applies ONE treatment to ALL corners; per-vertex refinement lands with the
                    // point editor pass. Current mode/size derived from the profile verts.
                    ui.separator();
                    ui.label(egui::RichText::new("Corners").small().strong());
                    let cur_mode: u8 = if profile.verts.iter().any(|v| v.is_round()) { 1 }
                                       else if profile.verts.iter().any(|v| v.is_chamfer()) { 2 } else { 0 };
                    let mut cur_r = profile.verts.iter().map(|v| v.corner_radius()).fold(0.0f32, f32::max);
                    if cur_r <= 0.0 { cur_r = 0.1; }
                    let mut new_mode = cur_mode;
                    ui.horizontal(|ui| {
                        for (m, lbl) in [(0u8, "Sharp"), (1, "Round"), (2, "Chamfer")] {
                            if ui.selectable_label(cur_mode == m, lbl).clicked() { new_mode = m; }
                        }
                    });
                    let mut new_r = cur_r;
                    let rr = ui.add(egui::Slider::new(&mut new_r, 0.0..=1.0).text("Corner size"));
                    if rr.drag_started() { pctx.drag_started = true; }
                    let mode_changed = new_mode != cur_mode;
                    if mode_changed || rr.changed() {
                        let signed = match new_mode { 1 => new_r.abs(), 2 => -new_r.abs(), _ => 0.0 };
                        for v in profile.verts.iter_mut() { v.bevel_radius = signed; }
                        pctx.changed = true; pctx.desc = "Curve corners".into();
                    }

                    // ---- Shell (hollow wall thickness; 0 = solid) ----
                    ui.separator();
                    let sr = ui.add(egui::Slider::new(wall_t, 0.0..=1.0).text("Shell (wall)"));
                    if sr.drag_started() { pctx.drag_started = true; }
                    if sr.changed() { pctx.changed = true; pctx.desc = "Shell".into(); }
                    } // end closed-only (Corners + Shell)

                    // ---- Rim (extrude-cap silhouette bevel; sign = mode, magnitude = radius) ----
                    // Applies to closed caps and open-WALL caps; a tube is already round.
                    if !*open || !*tube {
                    ui.horizontal(|ui| {
                        let mut rim_r = rim.abs();
                        let rim_round = *rim >= 0.0;
                        let rr2 = ui.add(egui::Slider::new(&mut rim_r, 0.0..=1.0).text("Rim"));
                        if rr2.drag_started() { pctx.drag_started = true; }
                        let mut new_round = rim_round;
                        if ui.selectable_label(rim_round, "◜").on_hover_text("Round rim").clicked() { new_round = true; }
                        if ui.selectable_label(!rim_round, "◹").on_hover_text("Chamfer rim").clicked() { new_round = false; }
                        if rr2.changed() || new_round != rim_round {
                            *rim = if new_round { rim_r.abs() } else { -rim_r.abs() };
                            pctx.changed = true; pctx.desc = "Rim bevel".into();
                        }
                    });
                    } // end rim (closed or open-wall)
                    ui.label(egui::RichText::new("Profile shape edited via Curve tool").small().weak());
                }
                SdfShape::MeshVolume { grid, source_poly } => {
                    ui.label(format!("Mesh volume ({}³ grid)", grid.res));
                    if source_poly.is_some() {
                        ui.horizontal(|ui| {
                            ui.label(egui::RichText::new("\u{1F517} Linked to:").small());
                            ui.label(egui::RichText::new(lab_link_name.as_deref().unwrap_or("?"))
                                .small().strong());
                        });
                        ui.label(egui::RichText::new(
                            "Follows the poly's transform; re-converts on geometry edits.")
                            .small().weak());
                        ui.horizontal(|ui| {
                            if ui.small_button("\u{27F3} Refresh now")
                                .on_hover_text("Force a re-convert from the source poly").clicked() {
                                lab_refresh = true;
                            }
                            if ui.small_button("\u{2702} Make Static")
                                .on_hover_text("Break the live link — keep the current imprint").clicked() {
                                lab_unlink = true;
                            }
                        });
                    } else {
                        ui.label(egui::RichText::new("Static (converted / unlinked).").small().weak());
                    }
                }
                SdfShape::HalfSpace => {
                    ui.label(egui::RichText::new("Infinite slice plane").small());
                    ui.label(egui::RichText::new("Normal = local +Z. Rotate / move to orient the cut.").small().weak());
                    ui.label(egui::RichText::new("Set op to − Sub (or × Int) below; Blend rounds the cut.").small().weak());
                }
                SdfShape::SpinePipe { points, radius, closed, .. } => {
                    ui.label(egui::RichText::new(format!(
                        "3D Spine / Pipe — {}/{} points ({})",
                        points.len(),
                        core_math::curves::MAX_SPINE_POINTS,
                        if *closed { "closed" } else { "open" }
                    )).small());
                    let r = ui.add(egui::Slider::new(radius, 0.005..=1.0).text("Radius")
                        .clamping(egui::SliderClamping::Never));
                    if r.drag_started() { pctx.drag_started = true; }
                    if r.changed() { pctx.changed = true; pctx.desc = "Spine radius".into(); }
                    let mut cl = *closed;
                    if ui.checkbox(&mut cl, "Closed loop").changed() {
                        *closed = cl;
                        pctx.changed = true; pctx.desc = "Spine closed".into();
                    }
                    ui.label(egui::RichText::new(
                        "Ctrl+click add · Shift multi-select · RMB delete · S scale · R twist · hold B type · Space finish"
                    ).small().weak());
                }
            }
            // ---- Slice tools ----
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                if ui.add(egui::Button::new(egui::RichText::new("▱ Add Slice").small())
                        .fill(egui::Color32::from_rgb(60,40,40)))
                    .on_hover_text("Add a HalfSpace slice cutter (Subtract) to this body").clicked()
                {
                    pctx.add_slice = true;
                }
                if matches!(self.scene.primitives[idx].shape, SdfShape::HalfSpace) {
                    if ui.add(egui::Button::new(egui::RichText::new("⎘ Slice Duplicate").small())
                            .fill(egui::Color32::from_rgb(40,60,40)))
                        .on_hover_text("Split this body into two along the slice — each keeps one half, movable apart").clicked()
                    {
                        pctx.slice_duplicate = true;
                    }
                }
            });
            // Per-primitive subdivision (SDF surface rounding)
            ui.add_space(4.0);
            let mut sub = self.scene.primitives[idx].subdivision as i32;
            let r = ui.add(egui::Slider::new(&mut sub, 0..=6).text("Subdivision"))
                .on_hover_text("Edge rounding: 0=sharp, 6=very smooth");
            if r.drag_started() { pctx.drag_started = true; }
            if r.changed() {
                self.scene.primitives[idx].subdivision = sub as u32;
                pctx.changed = true; pctx.desc = "Subdivision".into();
            }
        });
        ui.add_space(2.0); ui.separator(); ui.add_space(2.0);
        // ---- Modifier Stack (shared with Properties → Modifiers tab) ----
        self.draw_props_prim_modifiers(ui, pctx, idx, false);
        ui.add_space(2.0); ui.separator(); ui.add_space(2.0);
        // ---- SDF Sculpt Brush ----
        egui::CollapsingHeader::new(egui::RichText::new("\u{1F3A8} SDF Sculpt Brush").strong().size(13.0).color(egui::Color32::from_rgb(200, 120, 60)))
            .default_open(false).show(ui, |ui| {
            ui.checkbox(&mut self.sdf_sculpt_active, "Enable Sculpt Brush");
            if self.sdf_sculpt_active {
                ui.label(egui::RichText::new("LMB drag on surface to sculpt").weak().italics().size(10.0));
                ui.label(egui::RichText::new("Shift=Flatten, Ctrl=Invert").weak().italics().size(10.0));

                // Sculpt Op selector
                let cur_op = self.sdf_sculpt_op;
                egui::ComboBox::from_id_salt("sdf_sculpt_op_sel")
                    .selected_text(cur_op.label())
                    .show_ui(ui, |ui| {
                        for op in &SculptOp::ALL {
                            if ui.selectable_label(*op == cur_op, op.label()).clicked() && *op != cur_op {
                                self.sdf_sculpt_op = *op;
                            }
                        }
                    });

                ui.add(egui::Slider::new(&mut self.sdf_sculpt_radius, 0.05..=3.0).text("Radius").logarithmic(true));
                ui.add(egui::Slider::new(&mut self.sdf_sculpt_strength, 0.01..=1.0).text("Strength"));

                // Brush falloff selector
                let cur_falloff_label = self.sdf_sculpt_falloff.label();
                let cur_falloff_clone = self.sdf_sculpt_falloff.clone();
                egui::ComboBox::from_id_salt("sdf_sculpt_falloff_sel")
                    .selected_text(cur_falloff_label)
                    .show_ui(ui, |ui| {
                        for f in &core_math::sculpt_layer::BrushFalloff::ALL {
                            if ui.selectable_label(*f == cur_falloff_clone, f.label()).clicked() && *f != cur_falloff_clone {
                                self.sdf_sculpt_falloff = f.clone();
                            }
                        }
                    });
            }
        });
        // ---- Sculpt Layers ----
        egui::CollapsingHeader::new(egui::RichText::new("\u{1F5BC} Sculpt Layers").strong().size(13.0).color(egui::Color32::from_rgb(200, 120, 60)))
            .default_open(false).show(ui, |ui| {
            // + New Layer button
            if ui.small_button("+ New Layer").clicked() {
                self.transaction_snapshot_only("Add Sculpt Layer");
                let layer_id = self.scene.primitives[idx].sculpt_layers.len() as u32 + 1;
                let name = format!("Layer {}", layer_id);
                self.scene.primitives[idx].sculpt_layers.push(
                    core_math::sculpt_layer::SculptLayer::new(name, layer_id));
                self.scene.primitives[idx].active_sculpt_layer = Some(self.scene.primitives[idx].sculpt_layers.len() - 1);
                pctx.changed = true; pctx.desc = "Add sculpt layer".into();
            }

            let n_layers = self.scene.primitives[idx].sculpt_layers.len();
            if n_layers == 0 {
                ui.weak("No sculpt layers. Enable brush to auto-create.");
            }

            let active = self.scene.primitives[idx].active_sculpt_layer;
            let mut remove_idx: Option<usize> = None;
            let mut clear_idx: Option<usize> = None;
            let mut new_active: Option<usize> = None;

            // Draw layers in reverse order (top layer first)
            for li in (0..n_layers).rev() {
                let is_active = active == Some(li);
                let frame_color = if is_active {
                    egui::Color32::from_rgb(200, 140, 30)
                } else {
                    egui::Color32::from_rgb(50, 50, 55)
                };
                egui::Frame::none().stroke(egui::Stroke::new(1.0, frame_color)).inner_margin(4.0).show(ui, |ui| {
                    ui.horizontal(|ui| {
                        // Eye toggle
                        let eye = if self.scene.primitives[idx].sculpt_layers[li].visible { "\u{1F441}" } else { "\u{1F441}\u{200D}\u{1F5E8}" };
                        if ui.small_button(eye).clicked() {
                            // Route through pctx so visibility toggles are undoable (write_prim alone
                            // skipped history). Flat mutate first; properties.rs snapshots + mirrors.
                            let vis = !self.scene.primitives[idx].sculpt_layers[li].visible;
                            self.scene.primitives[idx].sculpt_layers[li].visible = vis;
                            self.mark_dirty(DirtyFlags::RENDER);
                            pctx.changed = true; pctx.desc = "Toggle sculpt layer visibility".into();
                        }
                        // Name (click to select) — re-click active layer is a no-op.
                        let name = &self.scene.primitives[idx].sculpt_layers[li].name;
                        let label = if is_active {
                            egui::RichText::new(name).strong().color(egui::Color32::from_rgb(255, 200, 80))
                        } else {
                            egui::RichText::new(name)
                        };
                        if ui.selectable_label(is_active, label).clicked() && !is_active {
                            new_active = Some(li);
                        }
                        // Opacity
                        let mut op = self.scene.primitives[idx].sculpt_layers[li].opacity;
                        if ui.add(egui::DragValue::new(&mut op).range(0.0..=1.0).speed(0.01).max_decimals(2).suffix("")).changed() {
                            self.scene.primitives[idx].sculpt_layers[li].opacity = op;
                            self.mark_dirty(DirtyFlags::RENDER);
                            pctx.changed = true; pctx.desc = "Sculpt layer opacity".into();
                        }
                        // Cell count info
                        let cells = self.scene.primitives[idx].sculpt_layers[li].delta_grid.cell_count();
                        if cells > 0 {
                            ui.weak(format!("{}k", cells / 1000));
                        }
                    });
                    // Controls for active layer
                    if is_active {
                        ui.horizontal(|ui| {
                            // Blend mode
                            let cur_blend = self.scene.primitives[idx].sculpt_layers[li].blend_mode;
                            egui::ComboBox::from_id_salt(format!("sl_blend_{}", li))
                                .selected_text(cur_blend.label()).width(70.0)
                                .show_ui(ui, |ui| {
                                    for bm in &core_math::sculpt_layer::LayerBlendMode::ALL {
                                        if ui.selectable_label(*bm == cur_blend, bm.label()).clicked() && *bm != cur_blend {
                                            self.scene.primitives[idx].sculpt_layers[li].blend_mode = *bm;
                                            self.mark_dirty(DirtyFlags::RENDER);
                                            pctx.changed = true; pctx.desc = "Sculpt layer blend mode".into();
                                        }
                                    }
                                });
                            if ui.small_button("Clear").clicked() { clear_idx = Some(li); }
                            if ui.small_button("Delete").clicked() { remove_idx = Some(li); }
                        });
                    }
                });
            }

            if let Some(ni) = new_active {
                if self.scene.primitives[idx].active_sculpt_layer != Some(ni) {
                    self.scene.primitives[idx].active_sculpt_layer = Some(ni);
                    pctx.changed = true; pctx.desc = "Select sculpt layer".into();
                }
            }
            if let Some(ci) = clear_idx {
                // Already empty layer → no empty undo.
                let already_empty = self.scene.primitives[idx].sculpt_layers.get(ci)
                    .map(|l| l.delta_grid.cells.is_empty()).unwrap_or(true);
                if !already_empty {
                    self.transaction_snapshot_only("Clear Sculpt Layer");
                    self.scene.primitives[idx].sculpt_layers[ci].delta_grid.clear();
                    self.mark_dirty(DirtyFlags::RENDER);
                    pctx.changed = true; pctx.desc = "Clear sculpt layer".into();
                }
            }
            if let Some(ri) = remove_idx {
                self.transaction_snapshot_only("Delete Sculpt Layer");
                self.scene.primitives[idx].sculpt_layers.remove(ri);
                if self.scene.primitives[idx].active_sculpt_layer == Some(ri) {
                    self.scene.primitives[idx].active_sculpt_layer = if self.scene.primitives[idx].sculpt_layers.is_empty() { None } else { Some(0) };
                } else if let Some(a) = self.scene.primitives[idx].active_sculpt_layer {
                    if a > ri { self.scene.primitives[idx].active_sculpt_layer = Some(a - 1); }
                }
                self.mark_dirty(DirtyFlags::RENDER);
                pctx.changed = true; pctx.desc = "Delete sculpt layer".into();
            }

            // Migrate legacy sculpt modifiers to a layer
            let legacy_count = self.scene.primitives[idx].modifiers.iter()
                .filter(|m| m.mod_type == ModifierType::Sculpt).count();
            if legacy_count > 0 {
                ui.add_space(4.0);
                ui.label(egui::RichText::new(format!("{} legacy sculpt stamps in modifier stack", legacy_count)).weak().italics().size(10.0));
                if ui.small_button("Clear Legacy Stamps").clicked() {
                    self.scene.primitives[idx].modifiers.retain(|m| m.mod_type != ModifierType::Sculpt);
                    self.scene.primitives[idx].sync_modifiers_to_gpu_fields();
                    pctx.changed = true; pctx.desc = "Clear legacy sculpt stamps".into();
                }
            }
        });
        ui.add_space(2.0); ui.separator(); ui.add_space(2.0);
        // ---- Boolean ----
        let bool_default_open = self.scene.primitives.len() > 1;
        egui::CollapsingHeader::new(egui::RichText::new("⚙ Boolean").strong().size(13.0).color(egui::Color32::from_rgb(30,136,229)))
            .default_open(bool_default_open).show(ui, |ui| {
            // Base = first slot of the body's chain (resolve_child_order), not flat index 0.
            if self.scene.is_base_prim_flat(idx) {
                ui.weak("Base of chain — no boolean op");
            } else {
                let _cur_k = {
                    let k = self.scene.primitives[idx].operation.blend_k();
                    if k > 0.0 { k } else { 0.15 }
                };
                ui.horizontal(|ui|{
                    let is_u=matches!(&self.scene.primitives[idx].operation,BooleanOp::Union{..});
                    let u_fill = if is_u { egui::Color32::from_rgb(50,150,50) } else { egui::Color32::from_rgb(40,60,40) };
                    if ui.add(egui::Button::new(egui::RichText::new("+ Add").color(egui::Color32::from_rgb(140,220,140)).small())
                        .fill(u_fill).min_size(egui::vec2(0.0,20.0))).clicked(){pctx.new_bool_op=Some(BooleanOp::Union{k:0.0});}
                    let is_s=matches!(&self.scene.primitives[idx].operation,BooleanOp::Subtract{..});
                    let s_fill = if is_s { egui::Color32::from_rgb(150,50,50) } else { egui::Color32::from_rgb(60,40,40) };
                    if ui.add(egui::Button::new(egui::RichText::new("- Sub").color(egui::Color32::from_rgb(220,140,140)).small())
                        .fill(s_fill).min_size(egui::vec2(0.0,20.0))).clicked(){pctx.new_bool_op=Some(BooleanOp::Subtract{k:0.0});}
                    let is_i=matches!(&self.scene.primitives[idx].operation,BooleanOp::Intersect|BooleanOp::SmoothIntersect{..});
                    let i_fill = if is_i { egui::Color32::from_rgb(160,120,30) } else { egui::Color32::from_rgb(60,50,30) };
                    if ui.add(egui::Button::new(egui::RichText::new("x Int").color(egui::Color32::from_rgb(220,190,100)).small())
                        .fill(i_fill).min_size(egui::vec2(0.0,20.0))).clicked(){pctx.new_bool_op=Some(BooleanOp::Intersect);}
                    let is_seam=matches!(&self.scene.primitives[idx].operation,BooleanOp::Seam{..});
                    let seam_fill = if is_seam { egui::Color32::from_rgb(110,60,170) } else { egui::Color32::from_rgb(50,40,60) };
                    if ui.add(egui::Button::new(egui::RichText::new("~ Seam").color(egui::Color32::from_rgb(200,160,255)).small())
                        .fill(seam_fill).min_size(egui::vec2(0.0,20.0)))
                        .on_hover_text("Panel line / scribed groove (or raised bead) along the curve where this prim's surface crosses the body surface")
                        .clicked(){
                        // Preserve params when already a seam (polarity toggle lives below).
                        // Fresh-pick defaults come from the ONE shared source (ui_panels).
                        pctx.new_bool_op = Some(crate::ui_panels::seam_op_defaults(
                            &self.scene.primitives[idx].operation, None));
                    }
                    let is_ext=matches!(&self.scene.primitives[idx].operation,BooleanOp::Extrude{..});
                    let ext_fill = if is_ext { egui::Color32::from_rgb(50,120,70) } else { egui::Color32::from_rgb(40,55,45) };
                    if ui.add(egui::Button::new(egui::RichText::new("\u{25b4} Ext").color(egui::Color32::from_rgb(150,230,170)).small())
                        .fill(ext_fill).min_size(egui::vec2(0.0,20.0)))
                        .on_hover_text("Extrude: raise (strength > 0) or recess (strength < 0) the body surface within this prim's footprint")
                        .clicked(){
                        // Preserve params when already an extrude (polarity lives on the
                        // signed Strength slider below / the viewport handle).
                        pctx.new_bool_op = Some(crate::ui_panels::extrude_op_defaults(
                            &self.scene.primitives[idx].operation, None));
                    }
                });
                ui.colored_label(egui::Color32::from_gray(90), "A=Add  S=Sub  I=Int");
                let is_seam_op = matches!(&self.scene.primitives[idx].operation, BooleanOp::Seam { .. });
                if is_seam_op {
                    // ---- Seam parameters (also reachable from the X boolean pie) ----
                    if let BooleanOp::Seam { width, depth, profile, raised } =
                        &mut self.scene.primitives[idx].operation
                    {
                        let r = ui.add(egui::Slider::new(width, 0.005..=0.5).text("Width").logarithmic(true));
                        if r.drag_started() { pctx.drag_started = true; }
                        if r.changed() { pctx.changed = true; pctx.desc = "Seam width".into(); }
                        let r = ui.add(egui::Slider::new(depth, 0.002..=0.5).text("Depth").logarithmic(true));
                        if r.drag_started() { pctx.drag_started = true; }
                        if r.changed() { pctx.changed = true; pctx.desc = "Seam depth".into(); }
                        ui.horizontal(|ui| {
                            ui.label("Profile:");
                            let cur = match *profile { 1 => "U (round)", 2 => "Square", _ => "V (chamfer)" };
                            egui::ComboBox::from_id_salt("seam_profile").width(100.0).selected_text(cur).show_ui(ui, |ui| {
                                for (pv, lbl) in [(0u8, "V (chamfer)"), (1u8, "U (round)"), (2u8, "Square")] {
                                    if ui.selectable_label(*profile == pv, lbl).clicked() && *profile != pv {
                                        *profile = pv; pctx.changed = true; pctx.desc = "Seam profile".into();
                                    }
                                }
                            });
                        });
                        let mut r_flag = *raised;
                        if ui.checkbox(&mut r_flag, "Raised (bead)")
                            .on_hover_text("Off = cut a groove into the surface; On = raise a weld bead / tongue")
                            .changed()
                        {
                            *raised = r_flag; pctx.changed = true; pctx.desc = "Seam polarity".into();
                        }
                    }
                }
                // ---- Extrude parameters (footprint raise/recess; also reachable from the pie) ----
                let is_extrude_op = matches!(&self.scene.primitives[idx].operation, BooleanOp::Extrude { .. });
                if is_extrude_op {
                    if let BooleanOp::Extrude { strength, width } =
                        &mut self.scene.primitives[idx].operation
                    {
                        let smax = crate::ui_panels::EXTRUDE_STRENGTH_MAX;
                        let r = ui.add(egui::Slider::new(strength, -smax..=smax).text("Strength"))
                            .on_hover_text("+ raises the surface (extrude out), \u{2212} recesses it (extrude in)");
                        if r.drag_started() { pctx.drag_started = true; }
                        if r.changed() { pctx.changed = true; pctx.desc = "Extrude strength".into(); }
                        let r = ui.add(egui::Slider::new(width, 0.005..=0.5).text("Width").logarithmic(true));
                        if r.drag_started() { pctx.drag_started = true; }
                        if r.changed() { pctx.changed = true; pctx.desc = "Extrude width".into(); }
                    }
                }
                // Blend K — Union / Subtract / smooth Intersect. Dragging k>0 on a hard Intersect
                // upgrades it to SmoothIntersect via set_blend_k. (Seam/Extrude use their own Width
                // slider — the k slot carries width, so the generic slider is hidden for them.)
                if !is_seam_op && !is_extrude_op {
                    let mut k = self.scene.primitives[idx].operation.blend_k();
                    let k_max = self.tweak.blend_k_max.max(0.1);
                    let r = ui.add(
                        egui::Slider::new(&mut k, 0.0..=k_max)
                            .text("Blend")
                            .logarithmic(k_max > 2.0),
                    )
                    .on_hover_text(format!(
                        "Soft boolean radius (0 = hard). Max = {k_max:.1} (Dev Tweaks → Max blend-k)."
                    ));
                    if r.drag_started() { pctx.drag_started = true; }
                    if r.changed() {
                        crate::edge_detect::op_set_k_clamped(
                            &mut self.scene.primitives[idx].operation, k, k_max);
                        pctx.changed = true; pctx.desc = "Blend".into();
                    }
                }
                let is_smooth = !is_seam_op && !is_extrude_op && self.scene.primitives[idx].operation.blend_k() > 0.0;
                if is_smooth {
                    // ---- Blend Profile: Smooth | Chamfer only (curve feature removed) ----
                    ui.add_space(4.0);
                    ui.label(egui::RichText::new("Blend Profile").small().weak())
                        .on_hover_text(
                            "Cross-section of the soft boolean join.\n\
                             Smooth = soft round · Chamfer = flat 45°.",
                        );
                    ui.horizontal(|ui| {
                        let cur = self.scene.primitives[idx].blend_profile;
                        let cur_ui = match cur {
                            BlendProfile::Chamfer => BlendProfile::Chamfer,
                            _ => BlendProfile::Polynomial,
                        };
                        for (p, lbl) in crate::properties_sdf::BLEND_PROFILE_CHOICES {
                            if ui.selectable_label(cur_ui == p, lbl).clicked() && cur != p {
                                self.scene.primitives[idx].blend_profile = p;
                                pctx.changed = true;
                                pctx.desc = "Blend profile".into();
                            }
                        }
                    });
                    // ---- Blend Falloff Mask ----
                    ui.add_space(4.0);
                    ui.label(egui::RichText::new("Falloff Mask").small().weak());
                    ui.horizontal_wrapped(|ui| {
                        let cur_dir = self.scene.primitives[idx].blend_falloff.direction;
                        let is_off = cur_dir.length_squared() < 0.001;
                        for (ax, lbl) in [
                            (FalloffAxis::None,"Off"), (FalloffAxis::PosY,"↑ Top"), (FalloffAxis::NegY,"↓ Bot"),
                            (FalloffAxis::PosX,"+X"), (FalloffAxis::NegX,"-X"),
                            (FalloffAxis::PosZ,"+Z"), (FalloffAxis::NegZ,"-Z"),
                        ] {
                            let selected = if ax == FalloffAxis::None { is_off }
                                else { !is_off && (cur_dir.normalize_or_zero() - ax.to_vec3()).length() < 0.01 };
                            // Re-click active axis → no empty undo.
                            if ui.selectable_label(selected, lbl).clicked() && !selected {
                                if ax == FalloffAxis::None {
                                    self.scene.primitives[idx].blend_falloff.direction = Vec3::ZERO;
                                } else {
                                    let (dir, oo, l) = compute_falloff_preset(&self.scene.primitives[idx], ax);
                                    let old = self.scene.primitives[idx].blend_falloff.clone();
                                    self.scene.primitives[idx].blend_falloff = BlendFalloff {
                                        direction: dir,
                                        origin_offset: oo,
                                        length: l,
                                        wall_protect: old.wall_protect,
                                        wall_protect_axis: old.wall_protect_axis,
                                        curve_type: old.curve_type,
                                        curve_power: old.curve_power,
                                    };
                                }
                                pctx.changed = true; pctx.desc = "Falloff axis".into();
                            }
                        }
                    });
                    if self.scene.primitives[idx].blend_falloff.is_active() {
                        let fo = &mut self.scene.primitives[idx].blend_falloff;
                        let r = ui.add(egui::Slider::new(&mut fo.length, 0.01..=5.0).text("Length"));
                        if r.drag_started() { pctx.drag_started = true; }
                        if r.changed() { pctx.changed = true; pctx.desc = "Falloff length".into(); }
                        let r = ui.add(egui::Slider::new(&mut fo.origin_offset, -3.0..=3.0).text("Offset"));
                        if r.drag_started() { pctx.drag_started = true; }
                        if r.changed() { pctx.changed = true; pctx.desc = "Falloff offset".into(); }
                    }
                    // ---- Protect Walls (soft cut: hard side planes, soft interior) ----
                    ui.add_space(4.0);
                    ui.label(egui::RichText::new("Protect Walls").small().weak());
                    {
                        let default_m = core_math::sdf_math::wall_protect_default_margin(
                            &self.scene.primitives[idx],
                        );
                        let fo = &mut self.scene.primitives[idx].blend_falloff;
                        let mut on = fo.wall_protect > 0.001;
                        if ui
                            .checkbox(&mut on, "Hard side walls")
                            .on_hover_text(
                                "Fade blend K near the cutter's side faces so soft cuts don't melt \
                                 those planes. Full K only toward the cut center. \
                                 Margin = how far from the wall before K is full.",
                            )
                            .changed()
                        {
                            // Size-relative default (was fixed 0.15 — invisible on large cutters).
                            fo.wall_protect = if on { default_m } else { 0.0 };
                            pctx.changed = true;
                            pctx.desc = "Protect walls".into();
                        }
                        if fo.wall_protect > 0.001 {
                            // Target wall axis — Auto picks shortest extent as depth.
                            ui.horizontal_wrapped(|ui| {
                                ui.label(
                                    egui::RichText::new("Target wall")
                                        .small()
                                        .color(egui::Color32::from_gray(160)),
                                );
                                for (ax, lbl, tip) in [
                                    (0u32, "Auto", "Shortest half-extent = cut depth; protect the other two faces"),
                                    (1u32, "X", "Hard ±X walls (local)"),
                                    (2u32, "Y", "Hard ±Y walls (local)"),
                                    (3u32, "Z", "Hard ±Z walls (local)"),
                                ] {
                                    let sel = fo.wall_protect_axis == ax;
                                    if ui
                                        .selectable_label(sel, lbl)
                                        .on_hover_text(tip)
                                        .clicked()
                                        && !sel
                                    {
                                        fo.wall_protect_axis = ax;
                                        pctx.changed = true;
                                        pctx.desc = "Wall protect axis".into();
                                    }
                                }
                            });
                            let max_m = (default_m * 4.0).max(0.5).min(5.0);
                            let r = ui.add(
                                egui::Slider::new(&mut fo.wall_protect, 0.01..=max_m)
                                    .text("Wall margin")
                                    .logarithmic(true),
                            );
                            if r.drag_started() { pctx.drag_started = true; }
                            if r.changed() { pctx.changed = true; pctx.desc = "Wall protect margin".into(); }
                        }
                    }
                }
            }
        });
        ui.add_space(2.0); ui.separator(); ui.add_space(2.0);
        // LAB poly→SDF live-modifier actions (deferred — need &mut self after the UI borrows).
        if lab_refresh { self.lab_request_refresh_for_prim(idx); }
        if lab_unlink { self.lab_make_prim_static(idx); }
    }

    /// Displace image height slot: square thumbnail, Load…, Clear, library / OS drag-drop.
    /// Returns true when the height map was assigned or cleared (caller marks undo + rebake).
    pub(crate) fn draw_displace_image_slot(
        &mut self,
        ui: &mut egui::Ui,
        prim_idx: usize,
        mod_idx: usize,
    ) -> bool {
        use std::path::{Path, PathBuf};

        const SLOT: f32 = 56.0;
        let mut changed = false;
        let has_map = self.scene.primitives.get(prim_idx)
            .and_then(|p| p.modifiers.get(mod_idx))
            .map(|m| m.height_map.is_some())
            .unwrap_or(false);

        ui.add_space(2.0);
        ui.label(egui::RichText::new("Height Image").small().strong());
        ui.horizontal(|ui| {
            // ---- Thumbnail square ----
            let (rect, response) = ui.allocate_exact_size(
                egui::vec2(SLOT, SLOT),
                egui::Sense::click(),
            );
            let hovered = response.hovered();
            let payload_active = egui::DragAndDrop::payload::<LibraryDragPayload>(ui.ctx()).is_some();
            let drop_hover = hovered && payload_active;

            // Background
            let bg = if drop_hover {
                egui::Color32::from_rgb(50, 90, 140)
            } else if has_map {
                egui::Color32::from_rgb(28, 32, 40)
            } else {
                egui::Color32::from_rgb(36, 38, 44)
            };
            ui.painter().rect_filled(rect, 4.0, bg);
            let stroke = if drop_hover {
                egui::Stroke::new(2.0, egui::Color32::from_rgb(100, 180, 255))
            } else {
                egui::Stroke::new(1.0, egui::Color32::from_rgb(70, 75, 90))
            };
            ui.painter().rect_stroke(rect, 4.0, stroke, egui::StrokeKind::Middle);

            // Thumbnail or placeholder
            if has_map {
                if let Some(tid) = self.displace_hm_thumb_id(ui.ctx(), prim_idx, mod_idx) {
                    let pad = 2.0;
                    let img_rect = rect.shrink(pad);
                    ui.painter().image(
                        tid,
                        img_rect,
                        egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                        egui::Color32::WHITE,
                    );
                } else {
                    ui.painter().text(
                        rect.center(),
                        egui::Align2::CENTER_CENTER,
                        "\u{1F5BC}",
                        egui::FontId::proportional(18.0),
                        egui::Color32::from_gray(160),
                    );
                }
            } else {
                ui.painter().text(
                    rect.center(),
                    egui::Align2::CENTER_CENTER,
                    if drop_hover { "Drop" } else { "+" },
                    egui::FontId::proportional(if drop_hover { 12.0 } else { 22.0 }),
                    egui::Color32::from_gray(if drop_hover { 220 } else { 120 }),
                );
            }

            // Hover tooltip
            response.clone().on_hover_text(if has_map {
                "Click to replace height image · drop library texture here"
            } else {
                "Click to load · drag texture from Library"
            });

            // Click → file dialog (load or replace)
            if response.clicked() {
                if let Some(path) = rfd::FileDialog::new()
                    .set_title("Load Displace Height Image")
                    .add_filter("Image", &["png", "jpg", "jpeg", "tga", "bmp", "exr"])
                    .pick_file()
                {
                    if let Some(hm) = Self::load_displace_height_map(&path) {
                        if let Some(m) = self.scene.primitives.get_mut(prim_idx)
                            .and_then(|p| p.modifiers.get_mut(mod_idx))
                        {
                            m.height_map = Some(hm);
                            changed = true;
                            let name = path.file_name().map(|n| n.to_string_lossy().into_owned())
                                .unwrap_or_else(|| "image".into());
                            self.toast(&format!("Displace height: {}", name));
                        }
                    } else {
                        self.toast("Failed to load height image");
                    }
                }
            }

            // Library drag-drop onto square (clear payload so viewport material-drop does not also fire).
            if ui.ctx().input(|i| i.pointer.any_released()) && response.hovered() {
                if let Some(pl) = egui::DragAndDrop::take_payload::<LibraryDragPayload>(ui.ctx()) {
                    let item_idx = pl.item_idx;
                    if let Some(path) = self.resolve_library_height_path(item_idx) {
                        if let Some(hm) = Self::load_displace_height_map(&path) {
                            if let Some(m) = self.scene.primitives.get_mut(prim_idx)
                                .and_then(|p| p.modifiers.get_mut(mod_idx))
                            {
                                m.height_map = Some(hm);
                                changed = true;
                                let name = path.file_name().map(|n| n.to_string_lossy().into_owned())
                                    .unwrap_or_else(|| "image".into());
                                self.toast(&format!("Displace height: {}", name));
                            }
                        } else {
                            self.toast("Failed to load library image as height map");
                        }
                    } else {
                        self.toast("No image file in library item");
                    }
                }
            }

            // OS file drop onto square
            let dropped: Vec<PathBuf> = ui.ctx().input(|i| {
                if !response.hovered() {
                    return Vec::new();
                }
                i.raw.dropped_files.iter().filter_map(|f| f.path.clone()).collect()
            });
            if let Some(path) = dropped.into_iter().find(|p| {
                p.extension()
                    .and_then(|e| e.to_str())
                    .map(|e| {
                        let e = e.to_ascii_lowercase();
                        matches!(e.as_str(), "png" | "jpg" | "jpeg" | "tga" | "bmp" | "exr")
                    })
                    .unwrap_or(false)
            }) {
                if let Some(hm) = Self::load_displace_height_map(&path) {
                    if let Some(m) = self.scene.primitives.get_mut(prim_idx)
                        .and_then(|p| p.modifiers.get_mut(mod_idx))
                    {
                        m.height_map = Some(hm);
                        changed = true;
                        let name = path.file_name().map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_else(|| "image".into());
                        self.toast(&format!("Displace height: {}", name));
                    }
                } else {
                    self.toast("Failed to load dropped image");
                }
            }

            // Buttons column
            ui.vertical(|ui| {
                if ui
                    .small_button(if has_map { "Replace\u{2026}" } else { "Load\u{2026}" })
                    .on_hover_text("Open an image file as grayscale height")
                    .clicked()
                {
                    if let Some(path) = rfd::FileDialog::new()
                        .set_title("Load Displace Height Image")
                        .add_filter("Image", &["png", "jpg", "jpeg", "tga", "bmp", "exr"])
                        .pick_file()
                    {
                        if let Some(hm) = Self::load_displace_height_map(&path) {
                            if let Some(m) = self.scene.primitives.get_mut(prim_idx)
                                .and_then(|p| p.modifiers.get_mut(mod_idx))
                            {
                                m.height_map = Some(hm);
                                changed = true;
                                let name = path.file_name().map(|n| n.to_string_lossy().into_owned())
                                    .unwrap_or_else(|| "image".into());
                                self.toast(&format!("Displace height: {}", name));
                            }
                        } else {
                            self.toast("Failed to load height image");
                        }
                    }
                }
                if has_map {
                    if ui.small_button("Clear").on_hover_text("Remove image — use noise again").clicked()
                    {
                        if let Some(m) = self.scene.primitives.get_mut(prim_idx)
                            .and_then(|p| p.modifiers.get_mut(mod_idx))
                        {
                            m.height_map = None;
                            changed = true;
                            self.cache.displace_hm_thumbs.remove(&(prim_idx, mod_idx));
                            self.toast("Displace height cleared (noise)");
                        }
                    }
                    // Path label
                    if let Some(path) = self.scene.primitives.get(prim_idx)
                        .and_then(|p| p.modifiers.get(mod_idx))
                        .and_then(|m| m.height_map.as_ref())
                        .map(|hm| hm.path.clone())
                    {
                        if !path.is_empty() {
                            let short = Path::new(&path)
                                .file_name()
                                .map(|n| n.to_string_lossy().into_owned())
                                .unwrap_or(path);
                            ui.label(
                                egui::RichText::new(short)
                                    .small()
                                    .weak()
                                    .italics(),
                            );
                        }
                    }
                } else {
                    ui.label(
                        egui::RichText::new("noise")
                            .small()
                            .weak()
                            .italics(),
                    );
                }
            });
        });

        // Ensure thumb cache is warm after assignment (next frame also fine).
        if changed && self.scene.primitives.get(prim_idx)
            .and_then(|p| p.modifiers.get(mod_idx))
            .map(|m| m.height_map.is_some())
            .unwrap_or(false)
        {
            let _ = self.displace_hm_thumb_id(ui.ctx(), prim_idx, mod_idx);
        }

        changed
    }

    /// Load image file → grayscale `DisplaceHeightMap`.
    /// Long side capped at [`core_math::modifiers::LOAD_HEIGHT_MAP_MAX_SIDE`] (T3: 2048).
    /// Draft bakes pack a coarse mip (`DRAFT_HEIGHT_MIP_MAX_SIDE`); settle uses full.
    pub(crate) fn load_displace_height_map(path: &std::path::Path) -> Option<core_math::primitives::DisplaceHeightMap> {
        use core_math::modifiers::LOAD_HEIGHT_MAP_MAX_SIDE;
        let img = image::open(path).ok()?;
        let img = if img.width() > LOAD_HEIGHT_MAP_MAX_SIDE || img.height() > LOAD_HEIGHT_MAP_MAX_SIDE {
            img.resize(
                LOAD_HEIGHT_MAP_MAX_SIDE,
                LOAD_HEIGHT_MAP_MAX_SIDE,
                image::imageops::FilterType::Triangle,
            )
        } else {
            img
        };
        let gray = img.to_luma8();
        let w = gray.width();
        let h = gray.height();
        if w == 0 || h == 0 {
            return None;
        }
        Some(core_math::primitives::DisplaceHeightMap {
            w,
            h,
            pixels: gray.into_raw(),
            path: path.to_string_lossy().into_owned(),
        })
    }

    /// Prefer height/displacement maps from a library item; else first image file.
    fn resolve_library_height_path(&self, item_idx: usize) -> Option<std::path::PathBuf> {
        let item = self.library.items.get(item_idx)?;
        // Explicit height role first.
        if let Some((_, p)) = item.files.iter().find(|(r, _)| r == "height") {
            return Some(p.clone());
        }
        // Filename heuristics.
        for (_, p) in &item.files {
            let name = p.file_name()?.to_string_lossy();
            if crate::content_library::detect_texture_role(&name) == "height" {
                return Some(p.clone());
            }
        }
        // Any image (individual textures / packs without height).
        item.files.first().map(|(_, p)| p.clone())
    }

    /// egui texture id for the Displace height-map thumbnail (builds on miss).
    fn displace_hm_thumb_id(
        &mut self,
        ctx: &egui::Context,
        prim_idx: usize,
        mod_idx: usize,
    ) -> Option<egui::TextureId> {
        // Clone minimal data so we can mutably touch the thumb cache afterward.
        let (hm_w, hm_h, hm_path, hm_pixels) = {
            let hm = self.scene.primitives.get(prim_idx)
                .and_then(|p| p.modifiers.get(mod_idx))
                .and_then(|m| m.height_map.as_ref())?;
            (hm.w, hm.h, hm.path.clone(), hm.pixels.clone())
        };
        let sig = {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            hm_w.hash(&mut h);
            hm_h.hash(&mut h);
            hm_path.hash(&mut h);
            hm_pixels.len().hash(&mut h);
            if !hm_pixels.is_empty() {
                hm_pixels[0].hash(&mut h);
                hm_pixels[hm_pixels.len() / 2].hash(&mut h);
                hm_pixels[hm_pixels.len() - 1].hash(&mut h);
            }
            h.finish()
        };
        let key = (prim_idx, mod_idx);
        if let Some((s, handle)) = self.cache.displace_hm_thumbs.get(&key) {
            if *s == sig {
                return Some(handle.id());
            }
        }
        // Build ~64×64 grayscale thumb (nearest sample).
        const T: u32 = 64;
        let (out_w, out_h) = if hm_w >= T && hm_h >= T {
            (T, T)
        } else {
            (T.min(hm_w).max(1), T.min(hm_h).max(1))
        };
        let mut rgba = vec![0u8; (out_w * out_h * 4) as usize];
        for y in 0..out_h {
            for x in 0..out_w {
                let sx = ((x as u64 * hm_w as u64) / out_w as u64) as u32;
                let sy = ((y as u64 * hm_h as u64) / out_h as u64) as u32;
                let g = hm_pixels
                    .get((sy * hm_w + sx) as usize)
                    .copied()
                    .unwrap_or(128);
                let i = ((y * out_w + x) * 4) as usize;
                rgba[i] = g;
                rgba[i + 1] = g;
                rgba[i + 2] = g;
                rgba[i + 3] = 255;
            }
        }
        let ci = egui::ColorImage::from_rgba_unmultiplied(
            [out_w as usize, out_h as usize],
            &rgba,
        );
        let handle = ctx.load_texture(
            format!("displace_hm_{prim_idx}_{mod_idx}_{sig}"),
            ci,
            egui::TextureOptions::LINEAR,
        );
        let id = handle.id();
        self.cache.displace_hm_thumbs.insert(key, (sig, handle));
        Some(id)
    }

    /// Prim procedural modifier stack (Mirror / Array / Twist / …).
    /// Shared by the SDF shape panel and Properties → Modifiers (PRIMITIVE LEVEL).
    /// `default_open`: true on the dedicated Modifiers tab so the stack is visible immediately.
    pub(crate) fn draw_props_prim_modifiers(
        &mut self,
        ui: &mut egui::Ui,
        pctx: &mut PropsContext,
        idx: usize,
        default_open: bool,
    ) {
        if idx >= self.scene.primitives.len() {
            return;
        }
        egui::CollapsingHeader::new(
            egui::RichText::new("🪞 Modifiers")
                .strong()
                .size(13.0)
                .color(egui::Color32::from_rgb(30, 136, 229)),
        )
        .default_open(default_open)
        .show(ui, |ui| {
            // P3: multi-mod → full-stack CPU bake (BodyField); single non-mirror still GPU-ok.
            if self.scene.primitives[idx].needs_cpu_full_stack_bake() {
                ui.colored_label(
                    egui::Color32::from_rgb(120, 200, 255),
                    "Multi-mod stack → CPU full-stack bake (matches viewport math).",
                );
            }
            ui.horizontal(|ui| {
                egui::ComboBox::from_id_salt(("add_mod_combo", default_open as u8))
                    .selected_text("+ Add Modifier")
                    .width(140.0)
                    .show_ui(ui, |ui| {
                        for (name, mt) in crate::ui_panels::ADD_MODIFIER_TYPES {
                            if ui.button(*name).clicked() {
                                self.transaction_snapshot_only(&format!("Add {} modifier", name));
                                let new_mod = crate::ui_panels::default_modifier(*mt);
                                self.scene.primitives[idx].modifiers.push(new_mod);
                                self.scene.primitives[idx].sync_modifiers_to_gpu_fields();
                                pctx.changed = true;
                                pctx.desc = format!("Add {}", name);
                                ui.close_menu();
                            }
                        }
                    });
            });
            // Migrate legacy single-slot modifier/mirror into the stack once.
            if self.scene.primitives[idx].modifiers.is_empty()
                && !self.scene.primitives[idx].legacy_migrated
            {
                if self.scene.primitives[idx].mirror_flags != 0 {
                    let mut m = core_math::primitives::Modifier::default();
                    m.mod_type = ModifierType::Mirror;
                    m.axis = self.scene.primitives[idx].mirror_flags;
                    m.mask.center = self.scene.primitives[idx].mirror_center;
                    self.scene.primitives[idx].modifiers.push(m);
                }
                if self.scene.primitives[idx].modifier.is_active() {
                    let legacy_mod = self.scene.primitives[idx].modifier.clone();
                    self.scene.primitives[idx].modifiers.push(legacy_mod);
                }
                self.scene.primitives[idx].legacy_migrated = true;
            }
            let mod_count = self.scene.primitives[idx].modifiers.len();
            let mut remove_idx: Option<usize> = None;
            let mut swap_pair: Option<(usize, usize)> = None;
            for mi in 0..mod_count {
                let mt = self.scene.primitives[idx].modifiers[mi].mod_type;
                let mt_label = mt.label();
                let card_col = match mt {
                    ModifierType::Mirror => {
                        egui::Color32::from_rgba_unmultiplied(100, 200, 255, 25)
                    }
                    ModifierType::Twist | ModifierType::Taper | ModifierType::Shear => {
                        egui::Color32::from_rgba_unmultiplied(255, 180, 80, 20)
                    }
                    ModifierType::Smooth
                    | ModifierType::Displace
                    | ModifierType::Spherize
                    | ModifierType::Shell
                    | ModifierType::Emboss
                    | ModifierType::Deboss => {
                        egui::Color32::from_rgba_unmultiplied(100, 255, 150, 20)
                    }
                    ModifierType::Sculpt => {
                        egui::Color32::from_rgba_unmultiplied(255, 140, 60, 25)
                    }
                    ModifierType::Array => {
                        egui::Color32::from_rgba_unmultiplied(200, 160, 255, 25)
                    }
                    _ => egui::Color32::TRANSPARENT,
                };
                egui::Frame::none()
                    .fill(card_col)
                    .inner_margin(4.0)
                    .rounding(4.0)
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.strong(format!("{}. {}", mi + 1, mt_label));
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if ui.small_button("X").on_hover_text("Remove").clicked() {
                                    remove_idx = Some(mi);
                                }
                                if mi + 1 < mod_count {
                                    if ui.small_button("v").on_hover_text("Move down").clicked() {
                                        swap_pair = Some((mi, mi + 1));
                                    }
                                }
                                if mi > 0 {
                                    if ui.small_button("^").on_hover_text("Move up").clicked() {
                                        swap_pair = Some((mi, mi - 1));
                                    }
                                }
                            });
                        });
                        if mt == ModifierType::Mirror {
                            let flags = &mut self.scene.primitives[idx].modifiers[mi].axis;
                            ui.horizontal(|ui| {
                                let mut mx = (*flags & 1) != 0;
                                let mut my = (*flags & 2) != 0;
                                let mut mz = (*flags & 4) != 0;
                                if ui.checkbox(&mut mx, "X").changed() {
                                    *flags ^= 1;
                                    pctx.changed = true;
                                    pctx.desc = "Mirror X".into();
                                }
                                if ui.checkbox(&mut my, "Y").changed() {
                                    *flags ^= 2;
                                    pctx.changed = true;
                                    pctx.desc = "Mirror Y".into();
                                }
                                if ui.checkbox(&mut mz, "Z").changed() {
                                    *flags ^= 4;
                                    pctx.changed = true;
                                    pctx.desc = "Mirror Z".into();
                                }
                            });
                            let mc = &mut self.scene.primitives[idx].modifiers[mi].mask.center;
                            ui.horizontal(|ui| {
                                ui.label("Center:");
                                if ui
                                    .add(egui::DragValue::new(&mut mc.x).prefix("X:").speed(0.02))
                                    .changed()
                                {
                                    pctx.changed = true;
                                    pctx.desc = "Mirror Center".into();
                                }
                                if ui
                                    .add(egui::DragValue::new(&mut mc.y).prefix("Y:").speed(0.02))
                                    .changed()
                                {
                                    pctx.changed = true;
                                    pctx.desc = "Mirror Center".into();
                                }
                                if ui
                                    .add(egui::DragValue::new(&mut mc.z).prefix("Z:").speed(0.02))
                                    .changed()
                                {
                                    pctx.changed = true;
                                    pctx.desc = "Mirror Center".into();
                                }
                            });
                        } else if mt == ModifierType::Array {
                            let is_circle =
                                self.scene.primitives[idx].modifiers[mi].param2 >= 0.5;
                            let mut circle_ui = is_circle;
                            if ui.checkbox(&mut circle_ui, "Circular").changed() {
                                self.scene.primitives[idx].modifiers[mi].param2 =
                                    if circle_ui { 1.0 } else { 0.0 };
                                pctx.changed = true;
                                pctx.desc = if circle_ui {
                                    "Array → Circular".into()
                                } else {
                                    "Array → Linear".into()
                                };
                            }
                            let count_f =
                                &mut self.scene.primitives[idx].modifiers[mi].strength;
                            let mut count_i = count_f.round().max(2.0) as i32;
                            if ui
                                .add(egui::Slider::new(&mut count_i, 2..=50).text("Count"))
                                .changed()
                            {
                                *count_f = count_i as f32;
                                pctx.changed = true;
                                pctx.desc = "Array Count".into();
                            }
                            if circle_ui
                                || self.scene.primitives[idx].modifiers[mi].param2 >= 0.5
                            {
                                let ax = &mut self.scene.primitives[idx].modifiers[mi].axis;
                                ui.horizontal(|ui| {
                                    ui.label("Axis:");
                                    if ui.selectable_label(*ax == 0, "Y").clicked() {
                                        *ax = 0;
                                        pctx.changed = true;
                                        pctx.desc = "Array Axis".into();
                                    }
                                    if ui.selectable_label(*ax == 1, "X").clicked() {
                                        *ax = 1;
                                        pctx.changed = true;
                                        pctx.desc = "Array Axis".into();
                                    }
                                    if ui.selectable_label(*ax == 2, "Z").clicked() {
                                        *ax = 2;
                                        pctx.changed = true;
                                        pctx.desc = "Array Axis".into();
                                    }
                                });
                                let off =
                                    &mut self.scene.primitives[idx].modifiers[mi].mask.center;
                                ui.horizontal(|ui| {
                                    ui.label("Radius vec:");
                                    if ui
                                        .add(
                                            egui::DragValue::new(&mut off.x)
                                                .prefix("X:")
                                                .speed(0.02),
                                        )
                                        .changed()
                                    {
                                        pctx.changed = true;
                                        pctx.desc = "Array Radius".into();
                                    }
                                    if ui
                                        .add(
                                            egui::DragValue::new(&mut off.y)
                                                .prefix("Y:")
                                                .speed(0.02),
                                        )
                                        .changed()
                                    {
                                        pctx.changed = true;
                                        pctx.desc = "Array Radius".into();
                                    }
                                    if ui
                                        .add(
                                            egui::DragValue::new(&mut off.z)
                                                .prefix("Z:")
                                                .speed(0.02),
                                        )
                                        .changed()
                                    {
                                        pctx.changed = true;
                                        pctx.desc = "Array Radius".into();
                                    }
                                });
                            } else {
                                let off =
                                    &mut self.scene.primitives[idx].modifiers[mi].mask.center;
                                ui.horizontal(|ui| {
                                    ui.label("Offset:");
                                    if ui
                                        .add(
                                            egui::DragValue::new(&mut off.x)
                                                .prefix("X:")
                                                .speed(0.02),
                                        )
                                        .changed()
                                    {
                                        pctx.changed = true;
                                        pctx.desc = "Array Offset".into();
                                    }
                                    if ui
                                        .add(
                                            egui::DragValue::new(&mut off.y)
                                                .prefix("Y:")
                                                .speed(0.02),
                                        )
                                        .changed()
                                    {
                                        pctx.changed = true;
                                        pctx.desc = "Array Offset".into();
                                    }
                                    if ui
                                        .add(
                                            egui::DragValue::new(&mut off.z)
                                                .prefix("Z:")
                                                .speed(0.02),
                                        )
                                        .changed()
                                    {
                                        pctx.changed = true;
                                        pctx.desc = "Array Offset".into();
                                    }
                                });
                            }
                        } else {
                            let salt = format!("mod_{}", mi);
                            let (mod_changed, mod_drag) = draw_modifier_editor(
                                ui,
                                &mut self.scene.primitives[idx].modifiers[mi],
                                &salt,
                            );
                            if mod_drag {
                                pctx.drag_started = true;
                            }
                            if mod_changed {
                                pctx.changed = true;
                                pctx.desc = "Modifier".into();
                            }
                            if mt == ModifierType::Displace {
                                if self.draw_displace_image_slot(ui, idx, mi) {
                                    pctx.changed = true;
                                    pctx.desc = "Displace image".into();
                                }
                            }
                            if mt == ModifierType::Shear {
                                let drive =
                                    &mut self.scene.primitives[idx].modifiers[mi].param2;
                                ui.horizontal(|ui| {
                                    ui.label("Drive Axis:");
                                    if ui.selectable_label(*drive < 0.5, "Y").clicked()
                                        && *drive >= 0.5
                                    {
                                        *drive = 0.0;
                                        pctx.changed = true;
                                        pctx.desc = "Shear Drive".into();
                                    }
                                    if ui
                                        .selectable_label(
                                            *drive >= 0.5 && *drive < 1.5,
                                            "X",
                                        )
                                        .clicked()
                                        && !(*drive >= 0.5 && *drive < 1.5)
                                    {
                                        *drive = 1.0;
                                        pctx.changed = true;
                                        pctx.desc = "Shear Drive".into();
                                    }
                                    if ui.selectable_label(*drive >= 1.5, "Z").clicked()
                                        && *drive < 1.5
                                    {
                                        *drive = 2.0;
                                        pctx.changed = true;
                                        pctx.desc = "Shear Drive".into();
                                    }
                                });
                            }
                            if mt == ModifierType::Sculpt {
                                use core_math::primitives::SculptOp;
                                let p2 =
                                    &mut self.scene.primitives[idx].modifiers[mi].param2;
                                let cur_op = SculptOp::from_f32(*p2);
                                egui::ComboBox::from_id_salt(format!("sculpt_op_{}", mi))
                                    .selected_text(cur_op.label())
                                    .show_ui(ui, |ui| {
                                        for op in [
                                            SculptOp::Push,
                                            SculptOp::Pull,
                                            SculptOp::Flatten,
                                            SculptOp::Inflate,
                                            SculptOp::Pinch,
                                            SculptOp::Crease,
                                        ] {
                                            if ui
                                                .selectable_label(op == cur_op, op.label())
                                                .clicked()
                                                && op != cur_op
                                            {
                                                *p2 = op.to_f32();
                                                pctx.changed = true;
                                                pctx.desc = "Sculpt Op".into();
                                            }
                                        }
                                    });
                                let mc = &mut self.scene.primitives[idx].modifiers[mi]
                                    .mask
                                    .center;
                                ui.horizontal(|ui| {
                                    ui.label("Pos:");
                                    if ui
                                        .add(
                                            egui::DragValue::new(&mut mc.x)
                                                .prefix("X:")
                                                .speed(0.02),
                                        )
                                        .changed()
                                    {
                                        pctx.changed = true;
                                        pctx.desc = "Sculpt Pos".into();
                                    }
                                    if ui
                                        .add(
                                            egui::DragValue::new(&mut mc.y)
                                                .prefix("Y:")
                                                .speed(0.02),
                                        )
                                        .changed()
                                    {
                                        pctx.changed = true;
                                        pctx.desc = "Sculpt Pos".into();
                                    }
                                    if ui
                                        .add(
                                            egui::DragValue::new(&mut mc.z)
                                                .prefix("Z:")
                                                .speed(0.02),
                                        )
                                        .changed()
                                    {
                                        pctx.changed = true;
                                        pctx.desc = "Sculpt Pos".into();
                                    }
                                });
                                let ext = &mut self.scene.primitives[idx].modifiers[mi]
                                    .mask
                                    .extents;
                                if ui
                                    .add(
                                        egui::Slider::new(&mut ext.x, 0.01..=2.0)
                                            .text("Radius"),
                                    )
                                    .changed()
                                {
                                    ext.y = ext.x;
                                    ext.z = ext.x;
                                    pctx.changed = true;
                                    pctx.desc = "Sculpt Radius".into();
                                }
                            }
                            if self.scene.primitives[idx].modifiers[mi].mod_type
                                != ModifierType::None
                            {
                                let mask_types_list = [
                                    ("No Mask", MaskShape::None),
                                    ("Sphere", MaskShape::Sphere),
                                    ("Box", MaskShape::Box),
                                    ("Gradient", MaskShape::Gradient),
                                    ("Noise 3D", MaskShape::Noise3D),
                                ];
                                let cur_mask =
                                    self.scene.primitives[idx].modifiers[mi].mask.shape;
                                let cur_mask_label = mask_types_list
                                    .iter()
                                    .find(|t| t.1 == cur_mask)
                                    .map(|t| t.0)
                                    .unwrap_or("No Mask");
                                egui::ComboBox::from_id_salt(format!("mask_{}", mi))
                                    .selected_text(cur_mask_label)
                                    .show_ui(ui, |ui| {
                                        for (name, ms) in &mask_types_list {
                                            if ui
                                                .selectable_label(cur_mask == *ms, *name)
                                                .clicked()
                                                && cur_mask != *ms
                                            {
                                                self.scene.primitives[idx].modifiers[mi]
                                                    .mask
                                                    .shape = *ms;
                                                pctx.changed = true;
                                                pctx.desc = format!("Mask → {}", name);
                                            }
                                        }
                                    });
                                if self.scene.primitives[idx].modifiers[mi].mask.shape
                                    != MaskShape::None
                                {
                                    let m = &mut self.scene.primitives[idx].modifiers[mi]
                                        .mask;
                                    if ui
                                        .add(
                                            egui::Slider::new(&mut m.feather, 0.0..=1.0)
                                                .text("Feather"),
                                        )
                                        .changed()
                                    {
                                        pctx.changed = true;
                                        pctx.desc = "Mask Feather".into();
                                    }
                                    if ui.checkbox(&mut m.invert, "Invert").changed() {
                                        pctx.changed = true;
                                        pctx.desc = "Mask Invert".into();
                                    }
                                    if m.shape == MaskShape::Noise3D {
                                        if ui
                                            .add(
                                                egui::Slider::new(
                                                    &mut m.noise_frequency,
                                                    0.1..=20.0,
                                                )
                                                .text("Frequency")
                                                .logarithmic(true),
                                            )
                                            .changed()
                                        {
                                            pctx.changed = true;
                                            pctx.desc = "Noise Frequency".into();
                                        }
                                        let mut oct = m.noise_octaves as i32;
                                        if ui
                                            .add(
                                                egui::Slider::new(&mut oct, 1..=8)
                                                    .text("Octaves"),
                                            )
                                            .changed()
                                        {
                                            m.noise_octaves = oct as u32;
                                            pctx.changed = true;
                                            pctx.desc = "Noise Octaves".into();
                                        }
                                        if ui
                                            .add(
                                                egui::Slider::new(
                                                    &mut m.noise_lacunarity,
                                                    1.0..=4.0,
                                                )
                                                .text("Lacunarity"),
                                            )
                                            .changed()
                                        {
                                            pctx.changed = true;
                                            pctx.desc = "Noise Lacunarity".into();
                                        }
                                        if ui
                                            .add(
                                                egui::Slider::new(
                                                    &mut m.noise_gain,
                                                    0.1..=1.0,
                                                )
                                                .text("Gain"),
                                            )
                                            .changed()
                                        {
                                            pctx.changed = true;
                                            pctx.desc = "Noise Gain".into();
                                        }
                                        if ui
                                            .add(
                                                egui::Slider::new(
                                                    &mut m.noise_threshold,
                                                    0.0..=1.0,
                                                )
                                                .text("Threshold"),
                                            )
                                            .changed()
                                        {
                                            pctx.changed = true;
                                            pctx.desc = "Noise Threshold".into();
                                        }
                                        ui.horizontal(|ui| {
                                            ui.label("Offset");
                                            if ui
                                                .add(
                                                    egui::DragValue::new(
                                                        &mut m.noise_offset.x,
                                                    )
                                                    .speed(0.05)
                                                    .prefix("X:"),
                                                )
                                                .changed()
                                            {
                                                pctx.changed = true;
                                            }
                                            if ui
                                                .add(
                                                    egui::DragValue::new(
                                                        &mut m.noise_offset.y,
                                                    )
                                                    .speed(0.05)
                                                    .prefix("Y:"),
                                                )
                                                .changed()
                                            {
                                                pctx.changed = true;
                                            }
                                            if ui
                                                .add(
                                                    egui::DragValue::new(
                                                        &mut m.noise_offset.z,
                                                    )
                                                    .speed(0.05)
                                                    .prefix("Z:"),
                                                )
                                                .changed()
                                            {
                                                pctx.changed = true;
                                            }
                                        });
                                        if pctx.changed && pctx.desc.is_empty() {
                                            pctx.desc = "Noise Offset".into();
                                        }
                                    }
                                }
                            }
                        }
                    });
                ui.add_space(2.0);
            }
            if let Some(ri) = remove_idx {
                self.transaction_snapshot_only("Remove Modifier");
                self.scene.primitives[idx].modifiers.remove(ri);
                self.scene.primitives[idx].sync_modifiers_to_gpu_fields();
                pctx.changed = true;
                pctx.desc = "Remove Modifier".into();
            }
            if let Some((a, b)) = swap_pair {
                self.transaction_snapshot_only("Reorder Modifier");
                self.scene.primitives[idx].modifiers.swap(a, b);
                self.scene.primitives[idx].sync_modifiers_to_gpu_fields();
                pctx.changed = true;
                pctx.desc = "Reorder Modifier".into();
            }
            if pctx.changed && !self.scene.primitives[idx].modifiers.is_empty() {
                self.scene.primitives[idx].sync_modifiers_to_gpu_fields();
            }
            if mod_count == 0 {
                ui.weak("No modifiers. Use + Add to start.");
            }
        });
    }
}
