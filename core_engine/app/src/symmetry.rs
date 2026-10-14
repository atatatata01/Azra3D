// core_engine/app/src/symmetry.rs
// Universal Symmetry Tool — plane visual, toggle, axis switching
//
// The symmetry plane is a first-class scene object shared across all pipelines.
// All plane math lives on SymmetryPlane in types.rs; this module handles
// viewport rendering, hotkey handling, and tool operations.
use eframe::egui;
use glam::Vec3;
use crate::{DirtyFlags, AzraApp, SymmetrySide, world_to_screen};

impl AzraApp {
    // ---------------------------------------------------------------
    // Toggle & axis switching
    // ---------------------------------------------------------------

    /// Toggle global symmetry on/off (Ctrl+M).
    /// If enabling and no plane has been set, defaults to X axis through
    /// the selected object's origin (or world origin).
    pub(crate) fn toggle_symmetry(&mut self) {
        self.transaction("Toggle Symmetry", |s| {
            let center = s.symmetry_default_center();
            let sym = &mut s.viewport.symmetry;
            sym.enabled = !sym.enabled;
            if sym.enabled {
                if sym.normal.length_squared() < 0.5 {
                    sym.normal = Vec3::X;
                }
                sym.origin = center;
                s.toast("Symmetry ON");
            } else {
                s.toast("Symmetry OFF");
            }
        });
    }

    /// Switch symmetry axis (called when X/Y/Z pressed during symmetry active).
    #[allow(dead_code)]
    pub(crate) fn set_symmetry_axis(&mut self, axis: u8) {
        let center = self.symmetry_default_center();
        let want_n = match axis { 0 => Vec3::X, 1 => Vec3::Y, _ => Vec3::Z };
        let sym = &self.viewport.symmetry;
        // Already this axis at the same center → no empty undo.
        if (sym.normal - want_n).length_squared() < 1e-12
            && (sym.origin - center).length_squared() < 1e-12
        {
            return;
        }
        self.transaction("Set Symmetry Axis", |s| {
            s.viewport.symmetry.set_axis(axis, center);
            let name = match axis { 0 => "X", 1 => "Y", _ => "Z" };
            s.toast(&format!("Symmetry axis: {}", name));
        });
    }

    /// Flip the symmetry plane normal (swap positive/negative sides).
    #[allow(dead_code)]
    pub(crate) fn flip_symmetry_normal(&mut self) {
        self.transaction("Flip Symmetry Normal", |s| {
            let sym = &mut s.viewport.symmetry;
            sym.normal = -sym.normal;
            sym.source_side = match sym.source_side {
                SymmetrySide::Positive => SymmetrySide::Negative,
                SymmetrySide::Negative => SymmetrySide::Positive,
            };
            s.toast("Symmetry normal flipped");
        });
    }

    /// Cycle radial symmetry count: off → 2 → 3 → 4 → 6 → 8 → off
    pub(crate) fn symmetry_cycle_radial(&mut self) {
        self.transaction("Cycle Radial Symmetry", |s| {
            let rc = s.viewport.symmetry.radial_count;
            let new_rc = match rc { 0 => 2, 2 => 3, 3 => 4, 4 => 6, 6 => 8, _ => 0 };
            s.viewport.symmetry.radial_count = new_rc;
            if new_rc == 0 {
                s.toast("Radial symmetry OFF");
            } else {
                if !s.viewport.symmetry.enabled { s.viewport.symmetry.enabled = true; }
                s.toast(&format!("Radial: {}-way", new_rc));
            }
            s.mark_dirty(DirtyFlags::RENDER);
        });
    }

    /// Move all prims of an SDF body to the other side of its mirror plane(s).
    /// Used by SDF body Mirror panel Flip / context menu (not poly Alt+X pie).
    pub(crate) fn flip_body_mirror(
        &mut self,
        body_nid: core_math::scene_graph::NodeId,
        flags: u8,
        center: Vec3,
    ) {
        if flags == 0 {
            self.toast("No mirror axes enabled");
            return;
        }
        self.transaction("Flip Mirror", |s| {
            let indices: Vec<usize> = s
                .scene
                .prim_body_map
                .iter()
                .enumerate()
                .filter(|(_, (nid, _))| *nid == body_nid)
                .map(|(i, _)| i)
                .collect();
            for i in indices {
                if i >= s.scene.primitives.len() {
                    continue;
                }
                s.write_prim(i, |p| {
                    for axis in 0..3 {
                        if (flags >> axis) & 1 != 0 {
                            let c = center[axis];
                            p.position[axis] = 2.0 * c - p.position[axis];
                        }
                    }
                });
            }
            s.mark_dirty(DirtyFlags::RENDER);
            s.toast("Mirror flipped");
        });
    }

    /// Set symmetry plane from current workplane.
    #[allow(dead_code)]
    pub(crate) fn symmetry_from_workplane(&mut self) {
        let wp = self.viewport.wp_resolved;
        self.viewport.symmetry.origin = wp.origin;
        self.viewport.symmetry.normal = wp.normal;
        self.viewport.symmetry.enabled = true;
        self.toast("Symmetry from workplane");
    }

    // ---------------------------------------------------------------
    // Destructive symmetry operations (Sprint 5)
    // ---------------------------------------------------------------

    /// Core poly symmetrize / cut on the given PolygonalBody `nid`, in the mesh's LOCAL space. ONE
    /// path for the Ctrl+Shift+K/D actions and the Alt+X pie (`sym_pie.rs`). `cut` = delete the far
    /// (-normal) half only; otherwise keep the +normal half and mirror it across the plane, welding
    /// the seam at `merge_threshold`. Poly-only: no-op with a toast for any other object type. The
    /// caller passes the node EXPLICITLY (the pie computed its plane from that exact mesh — resolving
    /// active_node() here would apply A's plane to whatever is selected at release).
    pub(crate) fn poly_symmetrize_op(&mut self, nid: core_math::scene_graph::NodeId, origin: Vec3, normal: Vec3, cut: bool) {
        // SMART SELECT: hold the PRE-symmetrize mesh so the sidecar can be re-keyed across the
        // mirror before this op stamps its own seam (same contract as the bevel/boolean/inset
        // installs). Safe to remap by POSITION here — symmetrize does not move the kept half's
        // vertices, it only adds the mirrored ones, so no existing tag can be lost to a moved
        // vertex (the trap that rules position-remap out for extrude/hinge).
        let old = self.poly_node_mesh(nid);
        let Some(base) = old.clone() else { self.toast("Select a polygon mesh"); return; };
        let thr = self.viewport.symmetry.merge_threshold;

        // Build the result on a CLONE first, so an outcome that destroys the object can be REFUSED
        // instead of committed. The kept half is the +normal side, and a plane taken from a selected
        // FACE is TANGENT to the surface — the whole body then sits on ONE side, so one of the two
        // flick directions keeps the side with nothing on it and `delete_half` drops every face.
        // Committing that wiped the mesh and still toasted "Symmetrized"; refuse instead, and leave
        // no empty undo entry behind.
        let mut next = (*base).clone();
        if cut {
            // Bisect first so straddlers split cleanly; delete_half_eps drops the
            // −side AND any on-plane-only faces (face-selected Alt+X plane).
            let eps = thr.max(1e-5);
            next.bisect_plane(origin, normal, eps);
            next.delete_half_eps(origin, normal, eps);
        } else {
            next.symmetrize(origin, normal, true, thr);
        }
        if next.face_count() == 0 {
            self.toast(if cut {
                "Cut half: the whole mesh is on the discarded side — flick the other way"
            } else {
                "Symmetrize: nothing on the kept side — flick the other way"
            });
            return;
        }

        self.transaction(if cut { "Cut Half" } else { "Symmetrize" }, move |s| {
            if let Some(node) = s.scene.scene_graph.get_mut(nid) {
                if let core_math::scene_graph::NodePayload::PolygonalBody { mesh, .. } = &mut node.payload {
                    *std::sync::Arc::make_mut(mesh) = next;
                }
            }
            // The MIDDLE LINE: tag the weld seam out of the bevel (owner 2026-08-05:
            // "symmetrize middle line should be tag to no bevel"). Symmetrize only — Cut Half
            // leaves an OPEN boundary, whose edges have no dihedral and are a different case.
            if !cut {
                if let (Some(old), Some(new)) = (old.as_deref(), s.poly_node_mesh(nid)) {
                    let seam = crate::poly::op_tags::edges_on_plane(
                        &new,
                        origin,
                        normal,
                        thr.max(1e-5),
                    );
                    if !seam.is_empty() {
                        eprintln!("[SYMMETRIZE] tagged {} seam edges out of bevel", seam.len());
                    }
                    match crate::poly::op_tags::carry_op_tags(
                        s.cache.poly_custom_data.get(&nid),
                        old,
                        &new,
                        &seam,
                    ) {
                        Some(cd) => { s.cache.poly_custom_data.insert(nid, cd); }
                        None => { s.cache.poly_custom_data.remove(&nid); }
                    }
                }
            }
            // Face-plane Alt+X leaves selection pointing at freed face ids — clear so a follow-up
            // Ctrl+X delete does not hit stale ids / empty the mesh by accident.
            s.poly_sel.selected_faces.clear();
            s.poly_sel.selected_edges.clear();
            s.poly_sel.selected_vertices.clear();
            s.poly_invalidate_pick_cache();
            s.poly_request_display_sync(nid);
            s.mark_dirty(DirtyFlags::RENDER);
            s.toast(if cut { "Cut half" } else { "Symmetrized" });
        });
    }

    /// Alt+X pie `i` mode: CUT the original to the kept (+normal) half AND add a separate mirrored
    /// INSTANCE object (the reflection of that half across the {origin,normal} plane). LOCAL space.
    pub(crate) fn poly_mirror_instance(&mut self, nid: core_math::scene_graph::NodeId, origin: Vec3, normal: Vec3) {
        let is_poly = matches!(
            self.scene.scene_graph.get(nid).map(|n| &n.payload),
            Some(core_math::scene_graph::NodePayload::PolygonalBody { .. })
        );
        if !is_poly { self.toast("Select a polygon mesh"); return; }
        // Build the kept half + its reflection BEFORE the transaction: an empty kept half would
        // otherwise blank the original AND add an empty "_mirror" node (same tangent-plane case
        // `poly_symmetrize_op` refuses — see the note there).
        let built = match self.scene.scene_graph.get(nid).map(|n| &n.payload) {
            Some(core_math::scene_graph::NodePayload::PolygonalBody { mesh, material, name, .. }) => {
                let thr = self.viewport.symmetry.merge_threshold.max(1e-5);
                let mut half = (**mesh).clone();
                half.bisect_plane(origin, normal, thr);
                half.delete_half_eps(origin, normal, thr); // keep +normal; drop on-plane faces
                let mut inst = half.clone();
                inst.flip_across(origin, normal);        // reflection = the instance
                Some((half, inst, material.clone(), format!("{}_mirror", name)))
            }
            _ => None,
        };
        if built.as_ref().map_or(true, |(half, ..)| half.face_count() == 0) {
            self.toast("Mirror instance: nothing on the kept side — flick the other way");
            return;
        }
        self.transaction("Mirror Instance", move |s| {
            if let Some((half, inst, material, iname)) = built {
                if let Some(node) = s.scene.scene_graph.get_mut(nid) {
                    if let core_math::scene_graph::NodePayload::PolygonalBody { mesh, .. } = &mut node.payload {
                        *std::sync::Arc::make_mut(mesh) = half; // original → kept half
                    }
                }
                s.poly_request_display_sync(nid);
                let parent = s.scene.scene_graph.root();
                let new_nid = s.scene.scene_graph.add_child(parent, core_math::scene_graph::NodePayload::PolygonalBody {
                    name: iname,
                    mesh: std::sync::Arc::new(inst),
                    source: Default::default(),
                    mesh_cache_id: None,
                    material,
                    modifiers: Vec::new(),
                });
                s.poly_request_display_sync(new_nid);
            }
            s.mark_dirty(DirtyFlags::RENDER);
            s.toast("Cut + mirror instance");
        });
    }

    /// Alt+X pie `m` mode: add a live Mirror modifier (across world axis 0=X/1=Y/2=Z) to the body's
    /// modifier stack (non-destructive).
    pub(crate) fn poly_add_mirror_modifier(&mut self, nid: core_math::scene_graph::NodeId, axis: u8) {
        let is_poly = matches!(
            self.scene.scene_graph.get(nid).map(|n| &n.payload),
            Some(core_math::scene_graph::NodePayload::PolygonalBody { .. })
        );
        if !is_poly { self.toast("Select a polygon mesh"); return; }
        self.transaction("Mirror Modifier", |s| {
            if let Some(node) = s.scene.scene_graph.get_mut(nid) {
                if let core_math::scene_graph::NodePayload::PolygonalBody { modifiers, .. } = &mut node.payload {
                    let mut m = core_math::scene_graph::PolyModifier::new(core_math::scene_graph::PolyModifierType::Mirror);
                    // Pie passes axis INDEX 0/1/2; disk/UI encoding is bitmask X=1,Y=2,Z=4.
                    m.mirror_axis = core_math::half_edge::MirrorAxes::from_axis_index(axis).bits();
                    modifiers.push(m);
                }
            }
            s.poly_request_display_sync(nid);
            s.mark_dirty(DirtyFlags::RENDER);
            s.toast("Mirror modifier added");
        });
    }

    /// Symmetrize: delete the negative-side geometry and mirror the positive
    /// side across the symmetry plane. Works on the selected poly body.
    #[allow(dead_code)]
    pub(crate) fn symmetry_symmetrize(&mut self) {
        if !self.viewport.symmetry.enabled { self.toast("Enable symmetry first"); return; }
        let nid = match self.sel.usel.active_node() { Some(n) => n, None => { self.toast("No object selected"); return; } };
        let sym = self.viewport.symmetry.clone();
        self.poly_symmetrize_op(nid, sym.origin, sym.normal, false);
    }

    /// Instant Mirror: create a mirrored duplicate of the selected poly body.
    #[allow(dead_code)]
    pub(crate) fn symmetry_instant_mirror(&mut self) {
        let sym = self.viewport.symmetry.clone();
        if !sym.enabled { self.toast("Enable symmetry first"); return; }
        let nid = match self.sel.usel.active_node() { Some(n) => n, None => { self.toast("No object selected"); return; } };
        self.transaction("Instant Mirror", |s| {
            let axis_index = if sym.normal.x.abs() > sym.normal.y.abs() && sym.normal.x.abs() > sym.normal.z.abs() { 0u8 }
                else if sym.normal.y.abs() > sym.normal.z.abs() { 1u8 }
                else { 2u8 };
            let axes = core_math::half_edge::MirrorAxes::from_axis_index(axis_index);

            if let Some(node) = s.scene.scene_graph.get(nid) {
                if let core_math::scene_graph::NodePayload::PolygonalBody { mesh, material, name, .. } = &node.payload {
                    let mirrored = mesh.mirrored(axes, true, sym.merge_threshold);
                    let parent = s.scene.scene_graph.root();
                    let new_nid = s.scene.scene_graph.add_child(parent, core_math::scene_graph::NodePayload::PolygonalBody {
                        name: format!("{}_mirror", name),
                        mesh: std::sync::Arc::new(mirrored),
                        source: Default::default(),
                        mesh_cache_id: None,
                        material: material.clone(),
                        modifiers: Vec::new(),
                    });
                    s.poly_request_display_sync(new_nid);
                }
            }
            s.mark_dirty(DirtyFlags::RENDER);
            s.toast("Instant Mirror created");
        });
    }

    /// Flip: reflect all vertices of the selected poly body across the symmetry plane.
    #[allow(dead_code)]
    pub(crate) fn symmetry_flip(&mut self) {
        let sym = self.viewport.symmetry.clone();
        if !sym.enabled { self.toast("Enable symmetry first"); return; }
        let nid = match self.sel.usel.active_node() { Some(n) => n, None => { self.toast("No object selected"); return; } };
        self.transaction("Flip", |s| {
            if let Some(node) = s.scene.scene_graph.get_mut(nid) {
                if let core_math::scene_graph::NodePayload::PolygonalBody { mesh, .. } = &mut node.payload {
                    let mesh = std::sync::Arc::make_mut(mesh);
                    mesh.flip_across(sym.origin, sym.normal);
                }
            }
            s.poly_request_display_sync(nid);
            s.mark_dirty(DirtyFlags::RENDER);
            s.toast("Flipped across symmetry plane");
        });
    }

    /// Delete Half: delete all geometry on the negative side of the symmetry plane.
    #[allow(dead_code)]
    pub(crate) fn symmetry_delete_half(&mut self) {
        if !self.viewport.symmetry.enabled { self.toast("Enable symmetry first"); return; }
        let nid = match self.sel.usel.active_node() { Some(n) => n, None => { self.toast("No object selected"); return; } };
        let sym = self.viewport.symmetry.clone();
        self.poly_symmetrize_op(nid, sym.origin, sym.normal, true);
    }

    /// Default center for symmetry plane: selected object origin, or world origin.
    fn symmetry_default_center(&self) -> Vec3 {
        if let Some(&idx) = self.selected_prim_indices_unified().first() {
            if let Some(p) = self.scene.primitives.get(idx) {
                return p.position;
            }
        }
        Vec3::ZERO
    }

    // ---------------------------------------------------------------
    // Viewport drawing
    // ---------------------------------------------------------------

    /// Draw the symmetry plane visual in the viewport.
    pub(crate) fn draw_symmetry_plane(&self, painter: &egui::Painter, rect: egui::Rect) {
        let sym = &self.viewport.symmetry;
        if !sym.enabled || !sym.visible { return; }

        let cam_pos = self.viewport.camera.target + self.viewport.camera.orbit * Vec3::new(0.0, 0.0, self.viewport.camera.distance);
        let aspect = rect.width() / rect.height().max(1.0);
        let ortho_hw = if self.render.ortho { self.viewport.camera.distance } else { 0.0 };

        // Auto-size plane to scene bounding radius + 20% padding
        let half = self.scene_bound_radius().max(1.0) * 1.2;
        let (tu, tv) = sym.tangent_frame();

        // Four corners of the plane quad
        let corners_3d = [
            sym.origin + tu * half + tv * half,
            sym.origin - tu * half + tv * half,
            sym.origin - tu * half - tv * half,
            sym.origin + tu * half - tv * half,
        ];
        let corners_2d: Vec<egui::Pos2> = corners_3d.iter()
            .filter_map(|&c| world_to_screen(c, cam_pos, self.viewport.camera.orbit, rect, aspect, ortho_hw, self.render.fov_deg))
            .collect();

        // Amber/orange plane fill (#FF9900 at 15% opacity)
        let plane_fill = egui::Color32::from_rgba_unmultiplied(255, 153, 0, 38);
        let plane_border = egui::Color32::from_rgba_unmultiplied(255, 153, 0, 180);

        if corners_2d.len() == 4 {
            // Filled quad via mesh
            let mesh_pts: Vec<egui::epaint::Vertex> = corners_2d.iter().map(|p| egui::epaint::Vertex {
                pos: *p, uv: egui::epaint::WHITE_UV, color: plane_fill,
            }).collect();
            let indices = vec![0u32, 1, 2, 0, 2, 3];
            let mesh = egui::epaint::Mesh { indices, vertices: mesh_pts, texture_id: egui::TextureId::default() };
            painter.add(egui::Shape::mesh(mesh));

            // Border edges (2px bright line)
            for i in 0..4 {
                painter.line_segment(
                    [corners_2d[i], corners_2d[(i + 1) % 4]],
                    egui::Stroke::new(2.0, plane_border),
                );
            }
        }

        // Normal arrow (colored by axis: R=X, G=Y, B=Z)
        let arrow_len = half * 0.25;
        let arrow_end = sym.origin + sym.normal * arrow_len;
        let axis_color = if sym.normal.x.abs() > 0.9 {
            egui::Color32::from_rgb(240, 80, 80)
        } else if sym.normal.y.abs() > 0.9 {
            egui::Color32::from_rgb(130, 220, 80)
        } else if sym.normal.z.abs() > 0.9 {
            egui::Color32::from_rgb(80, 140, 240)
        } else {
            plane_border // custom axis: amber
        };

        if let (Some(sp), Some(ep)) = (
            world_to_screen(sym.origin, cam_pos, self.viewport.camera.orbit, rect, aspect, ortho_hw, self.render.fov_deg),
            world_to_screen(arrow_end, cam_pos, self.viewport.camera.orbit, rect, aspect, ortho_hw, self.render.fov_deg),
        ) {
            painter.arrow(sp, ep - sp, egui::Stroke::new(2.5, axis_color));
        }

        // Center dot
        if let Some(cp) = world_to_screen(sym.origin, cam_pos, self.viewport.camera.orbit, rect, aspect, ortho_hw, self.render.fov_deg) {
            painter.circle_filled(cp, 5.0, plane_border);
            painter.circle_stroke(cp, 5.0, egui::Stroke::new(1.5, egui::Color32::WHITE));

            // "+"/"-" labels on each side
            let side_offset = tu * (half * 0.3);
            let pos_pt = sym.origin + side_offset;
            let neg_pt = sym.origin - side_offset;
            if let Some(pp) = world_to_screen(pos_pt, cam_pos, self.viewport.camera.orbit, rect, aspect, ortho_hw, self.render.fov_deg) {
                painter.text(pp, egui::Align2::CENTER_CENTER, "+",
                    egui::FontId::proportional(14.0),
                    egui::Color32::from_rgba_unmultiplied(255, 200, 100, 140));
            }
            if let Some(np) = world_to_screen(neg_pt, cam_pos, self.viewport.camera.orbit, rect, aspect, ortho_hw, self.render.fov_deg) {
                painter.text(np, egui::Align2::CENTER_CENTER, "−",
                    egui::FontId::proportional(14.0),
                    egui::Color32::from_rgba_unmultiplied(255, 200, 100, 100));
            }
        }

        // Axis label in screen space (near top of the plane)
        let label_pt = sym.origin + tv * (half * 0.9);
        if let Some(lp) = world_to_screen(label_pt, cam_pos, self.viewport.camera.orbit, rect, aspect, ortho_hw, self.render.fov_deg) {
            let axis_name = if sym.normal.x.abs() > 0.9 { "X"
            } else if sym.normal.y.abs() > 0.9 { "Y"
            } else if sym.normal.z.abs() > 0.9 { "Z"
            } else { "Custom" };
            let label = if sym.enabled { format!("Symmetry: {}", axis_name) } else { "Symmetry (off)".to_string() };
            painter.text(lp + egui::vec2(0.0, -8.0), egui::Align2::CENTER_BOTTOM,
                &label, egui::FontId::proportional(11.0), plane_border);
        }
    }

    /// Status text for the header bar when symmetry is enabled.
    pub(crate) fn symmetry_status_text(&self) -> Option<String> {
        let sym = &self.viewport.symmetry;
        if !sym.enabled { return None; }
        let axis = if sym.normal.x.abs() > 0.9 { "X"
        } else if sym.normal.y.abs() > 0.9 { "Y"
        } else if sym.normal.z.abs() > 0.9 { "Z"
        } else { "Custom" };
        let side = match sym.source_side {
            SymmetrySide::Positive => "+",
            SymmetrySide::Negative => "−",
        };
        let radial = if sym.radial_count >= 2 {
            format!(" R{}", sym.radial_count)
        } else { String::new() };
        Some(format!("Sym {} [{}]{}", axis, side, radial))
    }
}
