// core_engine/app/src/workspace.rs
// Workspace system — data-driven toolbar rendering, workspace switching, panel customization.

use eframe::egui;
use core_math::sdf_volume::BrushType;
use crate::{
    AzraApp, WorkMode, GizmoMode, ModelingContext, ToolMode,
    PolyWorkMode, NurbsWorkMode, PolyTool, NurbsTool,
    CreateOp, ToolButtonId, tool_button_meta,
    WorkspacePanel, PanelDock, COMMAND_DEFS, DirtyFlags,
};

/// Section label names indexed by Label(u8).
const LABEL_NAMES: &[&str] = &["MODE", "SHAPE", "OP", "TOOL", "BRUSH", "EDIT", "CREATE"];

impl AzraApp {
    // ================================================================
    // Render a single ToolButton
    // ================================================================

    /// Render a toolbar button. Returns true if clicked.
    pub(crate) fn render_tool_button(&mut self, ui: &mut egui::Ui, btn: ToolButtonId, ctrl: bool) -> bool {
        use ToolButtonId::*;

        // Handle visual-only elements
        match btn {
            Separator => { ui.separator(); return false; }
            Label(idx) => {
                let name = LABEL_NAMES.get(idx as usize).copied().unwrap_or("???");
                // Pick label color based on context
                let col = match idx {
                    0 if self.modeling_context == ModelingContext::Polygonal =>
                        egui::Color32::from_rgb(100, 200, 255),
                    0 if self.modeling_context == ModelingContext::Nurbs =>
                        egui::Color32::from_rgb(200, 140, 255),
                    5 | 6 if self.modeling_context == ModelingContext::Polygonal =>
                        egui::Color32::from_rgb(100, 200, 255),
                    5 | 6 if self.modeling_context == ModelingContext::Nurbs =>
                        egui::Color32::from_rgb(200, 140, 255),
                    _ => egui::Color32::from_gray(90),
                };
                ui.label(egui::RichText::new(name).size(9.0).color(col));
                return false;
            }
            _ => {}
        }

        let meta = tool_button_meta(btn);

        // Determine active state
        let active = match btn {
            ModeObject => matches!(self.tool.work_mode, WorkMode::Object | WorkMode::Vertex | WorkMode::Edge | WorkMode::Face),
            ModeSculpt => self.tool.work_mode == WorkMode::Sculpt,
            ModeFace => self.tool.work_mode == WorkMode::Face,
            ShapeSphere   => self.tool.mode.armed_shape_idx() == Some(0),
            ShapeBox      => self.tool.mode.armed_shape_idx() == Some(1),
            ShapeCylinder => self.tool.mode.armed_shape_idx() == Some(2),
            ShapeTorus    => self.tool.mode.armed_shape_idx() == Some(3),
            ShapeCone     => self.tool.mode.armed_shape_idx() == Some(4),
            ShapeCapsule  => self.tool.mode.armed_shape_idx() == Some(5),
            ShapeStair    => self.tool.mode.armed_shape_idx() == Some(6),
            ShapeSpiral   => self.tool.mode.armed_shape_idx() == Some(7),
                        ShapePolyline => self.tool.current.is_clip_curve_draw()
                && self.tool.current.as_clip_curve_draw().map_or(false, |s| s.create_prim),
            ShapeSpine    => self.tool.current.is_spine_point_edit(),
            ToolBoolCut   => self.tool.mode.is_bool_cut() || self.tool.bool_cut.is_some(),
            OpSubtract  => self.tool.create_op == CreateOp::Subtract,
            OpUnion     => self.tool.create_op == CreateOp::Union,
            OpIntersect => self.tool.create_op == CreateOp::Intersect,
            OpSlice     => self.tool.create_op == CreateOp::Slice,
            GizmoMove   => self.tool.gizmo_mode == GizmoMode::Translate,
            GizmoRotate => self.tool.gizmo_mode == GizmoMode::Rotate,
            GizmoScale  => self.tool.gizmo_mode == GizmoMode::Scale,
            BrushAdd     => self.brush.brush_type == BrushType::Add,
            BrushCarve   => self.brush.brush_type == BrushType::Subtract,
            BrushSmooth  => self.brush.brush_type == BrushType::Smooth,
            BrushFlatten => self.brush.brush_type == BrushType::Flatten,
            BrushInflate => self.brush.brush_type == BrushType::Inflate,
            BrushPinch   => self.brush.brush_type == BrushType::Pinch,
            BrushClay    => self.brush.brush_type == BrushType::Clay,
            BrushCrease  => self.brush.brush_type == BrushType::Crease,
            BrushTrim    => self.brush.brush_type == BrushType::Trim,
            PolyModeObj => self.poly_work_mode == PolyWorkMode::Object,
            PolyModeVtx => self.poly_work_mode == PolyWorkMode::Vertex,
            PolyModeEdg => self.poly_work_mode == PolyWorkMode::Edge,
            PolyModeFce => self.poly_work_mode == PolyWorkMode::Face,
            PolyExtrude => self.poly_tool == PolyTool::ExtrudeFace,
            PolyBevel   => self.poly_tool == PolyTool::BevelEdge,
            PolyLoopCut => self.poly_tool == PolyTool::LoopCut,
            PolyKnife   => self.poly_tool == PolyTool::Knife,
            PolyMerge   => self.poly_tool == PolyTool::MergeVertices,
            PolyInset   => self.poly_tool == PolyTool::InsetFace,
            NurbsModeObj => self.nurbs_work_mode == NurbsWorkMode::Object,
            NurbsModeCv  => self.nurbs_work_mode == NurbsWorkMode::Vertex,
            NurbsModeSrf => self.nurbs_work_mode == NurbsWorkMode::Face,
            NurbsKnotU   => self.nurbs_tool == NurbsTool::InsertKnotU,
            NurbsKnotV   => self.nurbs_tool == NurbsTool::InsertKnotV,
            NurbsWeight  => self.nurbs_tool == NurbsTool::AdjustWeight,
            NurbsLoft    => self.nurbs_tool == NurbsTool::Loft,
            NurbsRevolve => self.nurbs_tool == NurbsTool::Revolve,
            _ => false,
        };

        // Determine accent color
        let accent = match btn {
            ModeObject | ModeSculpt | GizmoMove | GizmoRotate | GizmoScale
            | BrushAdd | BrushCarve | BrushSmooth | BrushFlatten | BrushInflate
            | BrushPinch | BrushClay | BrushCrease | BrushTrim =>
                egui::Color32::from_rgba_unmultiplied(30, 136, 229, 45),
            ModeFace | ShapeSphere | ShapeBox | ShapeCylinder | ShapeTorus
            | ShapeCone | ShapeCapsule | ShapeStair | ShapeSpiral
            | ShapePolyline | ShapeSpine | ToolBoolCut =>
                egui::Color32::from_rgba_unmultiplied(255, 150, 0, 60),
            OpSubtract => {
                let c = CreateOp::Subtract.color();
                egui::Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), 40)
            }
            OpUnion => {
                let c = CreateOp::Union.color();
                egui::Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), 40)
            }
            OpIntersect => {
                let c = CreateOp::Intersect.color();
                egui::Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), 40)
            }
            OpSlice => {
                let c = CreateOp::Slice.color();
                egui::Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), 40)
            }
            PolyModeObj | PolyModeVtx | PolyModeEdg | PolyModeFce
            | PolyExtrude | PolyBevel | PolyLoopCut | PolyKnife | PolyMerge | PolyInset =>
                egui::Color32::from_rgba_unmultiplied(100, 200, 255, 50),
            NurbsModeObj | NurbsModeCv | NurbsModeSrf
            | NurbsKnotU | NurbsKnotV | NurbsWeight | NurbsLoft | NurbsRevolve =>
                egui::Color32::from_rgba_unmultiplied(200, 140, 255, 50),
            _ => egui::Color32::TRANSPARENT,
        };

        let fill = if active { accent } else { egui::Color32::TRANSPARENT };

        // Special rendering for boolean op buttons (colored text)
        let button = match btn {
            OpSubtract | OpUnion | OpIntersect | OpSlice => {
                let op = match btn {
                    OpSubtract => CreateOp::Subtract,
                    OpUnion => CreateOp::Union,
                    OpIntersect => CreateOp::Intersect,
                    _ => CreateOp::Slice,
                };
                egui::Button::new(egui::RichText::new(meta.icon).color(op.color()))
                    .min_size(egui::vec2(meta.width, meta.height))
                    .fill(fill)
            }
            _ => {
                egui::Button::new(meta.icon)
                    .min_size(egui::vec2(meta.width, meta.height))
                    .fill(fill)
            }
        };

        let clicked = ui.add(button).on_hover_text(meta.tooltip).clicked();
        if clicked {
            self.handle_tool_button(btn, ctrl);
        }
        clicked
    }

    // ================================================================
    // Handle ToolButton click
    // ================================================================

    /// Enter the Sculpt layout. The pie / mode button ALWAYS switches into the sculpt room —
    /// entering must never be blocked (that was the regression: the type router toasted "coming
    /// soon" instead of switching).
    ///
    /// Targeting is selection-aware:
    ///   - polygonal mesh → 3D-Coat Surface (writes the mesh; no Sculpt modifier)
    ///   - voxel object   → live voxel sculpt (Surface is a separate outliner / palette action)
    ///   - SDF            → toast; do not steal a leftover voxel onto the SDF
    pub(crate) fn enter_sculpt_mode(&mut self) {
        use core_math::scene_graph::NodePayload;

        // 1) ALWAYS switch into the sculpt layout — never gate this on selection.
        self.render.viewport_mode = crate::ViewportRenderMode::Hybrid;
        self.tool.work_mode = WorkMode::Sculpt;

        let sel = self.sel.usel.active_node().or_else(|| self.poly_active_body());
        match sel.and_then(|n| self.scene.scene_graph.get(n).map(|node| (n, &node.payload))) {
            Some((nid, NodePayload::PolygonalBody { .. })) => {
                self.begin_surface_sculpt(nid);
            }
            Some((nid, NodePayload::VoxelBody { .. })) => {
                self.voxel_enter_edit(nid);
            }
            Some((_, NodePayload::SdfBody { .. })) => {
                self.toast("Voxelizing the selected SDF to sculpt it is coming — pick a mesh or a voxel");
            }
            _ if self.sculpt.voxel.is_none() => {
                self.toast("Sculpt room \u{2014} add a subject with the \u{25CF} Sphere / \u{25A0} Box buttons");
            }
            _ => {}
        }

        self.mark_dirty(DirtyFlags::RENDER);
    }

    fn handle_tool_button(&mut self, btn: ToolButtonId, ctrl: bool) {
        use ToolButtonId::*;
        match btn {
            // Mode switches
            ModeObject => {
                self.tool.work_mode = WorkMode::Object;
                // Deactivate the live voxel to its stable display mesh. Without this the voxel
                // rendered off live chunks the switch never repainted → "disappears until I move
                // the camera".
                self.restore_sdf_room_render_state();
                self.mark_dirty(DirtyFlags::RENDER);
                if self.cache.use_body_vol_cache {
                    self.cache.body_vol_cache.mark_all_bodies_dirty(&self.scene.scene_graph);
                    self.trigger_body_vol_bake();
                }
            }
            ModeSculpt => { self.enter_sculpt_mode(); }
            ModeFace => {
                if self.tool.work_mode == WorkMode::Face {
                    self.tool.work_mode = WorkMode::Object;
                } else {
                    self.tool.work_mode = WorkMode::Face;
                    self.toast("Face Push/Pull: hover a Box face, drag to extrude");
                }
                self.mark_dirty(DirtyFlags::RENDER);
            }
            // Shape creation (footprint-drag presets 0–7). ShapeFrenchCurve is a legacy
            // workspace-JSON slot (feature removed) → arms Box preset.
            ShapeSphere | ShapeBox | ShapeCylinder | ShapeTorus
            | ShapeCone | ShapeCapsule | ShapeStair | ShapeSpiral | ShapeFrenchCurve => {
                let si = match btn {
                    ShapeSphere => 0,
                    ShapeBox | ShapeFrenchCurve => 1,
                    ShapeCylinder => 2, ShapeTorus => 3,
                    ShapeCone => 4, ShapeCapsule => 5, ShapeStair => 6, ShapeSpiral => 7,
                    _ => 0,
                };
                if ctrl {
                    crate::ops::create::instant_origin::run(self, si);
                } else if self.tool.mode.armed_shape_idx() == Some(si) {
                    self.set_mode(ToolMode::Select);
                    self.toast("Tool dropped");
                } else {
                    if self.tool.work_mode == WorkMode::Face {
                        self.tool.work_mode = WorkMode::Object;
                        self.sel.edit_target = None;
                        self.sel.selected_faces.clear();
                    }
                    // Create arms exclusive mode — clears Bevel and other sticky modes.
                    self.set_mode(ToolMode::CreateArmed { shape_idx: si });
                    self.toast(&format!("{} tool — draw in viewport, Q to drop",
                        Self::shape_preset(si).label()));
                }
            }
            // Path tools — dedicated authoring flows (not CreateArmed indices)
            ShapePolyline => {
                if self.tool.current.is_clip_curve_draw()
                    && self.tool.current.as_clip_curve_draw().map_or(false, |s| s.create_prim)
                {
                    // Toggle off
                    self.set_mode(ToolMode::Select);
                    self.toast("Polyline tool dropped");
                } else {
                    if self.tool.work_mode == WorkMode::Face {
                        self.tool.work_mode = WorkMode::Object;
                        self.sel.edit_target = None;
                        self.sel.selected_faces.clear();
                    }
                    self.set_mode(ToolMode::Select);
                    self.start_polyline_draw();
                }
            }
            ShapeSpine => {
                if self.tool.current.is_spine_point_edit() {
                    self.spine_exit_tool();
                    self.toast("Spine tool finished");
                } else {
                    if self.tool.work_mode == WorkMode::Face {
                        self.tool.work_mode = WorkMode::Object;
                        self.sel.edit_target = None;
                        self.sel.selected_faces.clear();
                    }
                    self.set_mode(ToolMode::Select);
                    // Create seed sphere and enter exclusive point-edit (creation path).
                    self.add_primitive_immediate(Self::spine_preset(), glam::Vec3::ZERO);
                }
            }
            ToolBoolCut => {
                if self.tool.mode.is_bool_cut() {
                    self.set_mode(ToolMode::Select);
                    self.toast("Bool Cut dropped");
                } else {
                    if self.tool.work_mode == WorkMode::Face {
                        self.tool.work_mode = WorkMode::Object;
                        self.sel.edit_target = None;
                        self.sel.selected_faces.clear();
                    }
                    self.set_mode(ToolMode::BoolCut);
                    self.toast("Bool Cut — LMB drag footprint on surface, release for depth, click to confirm");
                }
            }
            // Boolean ops
            OpSubtract  => self.set_create_op(CreateOp::Subtract),
            OpUnion     => self.set_create_op(CreateOp::Union),
            OpIntersect => self.set_create_op(CreateOp::Intersect),
            OpSlice     => self.set_create_op(CreateOp::Slice),
            // Gizmo modes
            GizmoMove   => self.tool.gizmo_mode = GizmoMode::Translate,
            GizmoRotate => self.tool.gizmo_mode = GizmoMode::Rotate,
            GizmoScale  => self.tool.gizmo_mode = GizmoMode::Scale,
            // Convert to clay
            ConvertToClay => {
                if !self.scene.primitives.is_empty() {
                    self.transaction("Convert to Clay", |s| {
                        s.cache.cpu_volume.bake_from_primitives(&s.scene.primitives);
                        // Clear graph first (source of truth), then rebuild flat cache
                        let body_nids: Vec<_> = s.scene.scene_graph.sdf_bodies()
                            .map(|(nid, _)| nid).collect();
                        for nid in body_nids {
                            if let Some(prims) = s.scene.scene_graph.body_primitives_mut(nid) {
                                prims.clear();
                            }
                        }
                        s.sync_flat_cache();
                        debug_assert_eq!(
                            s.scene.primitives.len(), 0,
                            "ConvertToClay: flat cache should be empty after graph clear"
                        );
                        s.deselect_all();
                        s.tool.work_mode = WorkMode::Sculpt;
                        s.mark_dirty(DirtyFlags::SCENE);
                        s.toast("Baked to voxel clay \u{2014} now sculpt!");
                    });
                }
            }
            // Sculpt brushes
            BrushAdd     => { self.brush.brush_type = BrushType::Add; self.activate_sdf_sculpt_brush(BrushType::Add); },
            BrushCarve   => { self.brush.brush_type = BrushType::Subtract; self.activate_sdf_sculpt_brush(BrushType::Subtract); },
            BrushSmooth  => { self.brush.brush_type = BrushType::Smooth; self.activate_sdf_sculpt_brush(BrushType::Smooth); },
            BrushFlatten => { self.brush.brush_type = BrushType::Flatten; self.activate_sdf_sculpt_brush(BrushType::Flatten); },
            BrushInflate => { self.brush.brush_type = BrushType::Inflate; self.activate_sdf_sculpt_brush(BrushType::Inflate); },
            BrushPinch   => { self.brush.brush_type = BrushType::Pinch; self.activate_sdf_sculpt_brush(BrushType::Pinch); },
            BrushClay    => { self.brush.brush_type = BrushType::Clay; self.activate_sdf_sculpt_brush(BrushType::Clay); },
            BrushCrease  => { self.brush.brush_type = BrushType::Crease; self.activate_sdf_sculpt_brush(BrushType::Crease); },
            BrushTrim    => { self.brush.brush_type = BrushType::Trim; self.activate_sdf_sculpt_brush(BrushType::Trim); },
            // Poly sub-modes
            PolyModeObj => self.poly_work_mode = PolyWorkMode::Object,
            PolyModeVtx => self.poly_work_mode = PolyWorkMode::Vertex,
            PolyModeEdg => self.poly_work_mode = PolyWorkMode::Edge,
            PolyModeFce => self.poly_work_mode = PolyWorkMode::Face,
            // Poly tools (toggle on re-click). set_poly_tool drops G/R/S when arming.
            PolyExtrude => {
                if self.poly_tool == PolyTool::ExtrudeFace {
                    self.poly_tool = PolyTool::Idle;
                } else {
                    self.set_poly_tool(PolyTool::ExtrudeFace);
                }
            }
            PolyBevel   => { if self.poly_tool == PolyTool::BevelEdge { self.poly_tool = PolyTool::Idle; } else { self.poly_activate_bevel(); } },
            PolyLoopCut => {
                if self.poly_tool == PolyTool::LoopCut {
                    self.poly_tool = PolyTool::Idle;
                } else {
                    self.set_poly_tool(PolyTool::LoopCut);
                }
            }
            PolyKnife   => self.poly_activate_knife(),
            PolyMerge   => {
                if self.poly_tool == PolyTool::MergeVertices {
                    self.poly_tool = PolyTool::Idle;
                } else {
                    self.set_poly_tool(PolyTool::MergeVertices);
                }
            }
            PolyInset   => {
                if self.poly_tool == PolyTool::InsetFace {
                    self.poly_tool = PolyTool::Idle;
                } else {
                    self.set_poly_tool(PolyTool::InsetFace);
                }
            }
            // Poly mesh creation — ops + cursor (INTEGRATION_RULES §0)
            PolyCreateBox => {
                let p = crate::ops::sdf::add_spawn_point(self);
                crate::ops::poly::mesh_create::box_mesh(self, p);
            }
            PolyCreatePlane => {
                let p = crate::ops::sdf::add_spawn_point(self);
                crate::ops::poly::mesh_create::plane(self, p);
            }
            PolyCreateSphere => {
                let p = crate::ops::sdf::add_spawn_point(self);
                crate::ops::poly::mesh_create::sphere(self, p);
            }
            PolyCreateCylinder => {
                let p = crate::ops::sdf::add_spawn_point(self);
                crate::ops::poly::mesh_create::cylinder(self, p);
            }
            PolyCreateTorus => {
                let p = crate::ops::sdf::add_spawn_point(self);
                crate::ops::poly::mesh_create::torus(self, p);
            }
            PolyCreateCone => {
                let p = crate::ops::sdf::add_spawn_point(self);
                crate::ops::poly::mesh_create::cone(self, p);
            }
            // NURBS sub-modes
            NurbsModeObj => self.nurbs_switch_work_mode(NurbsWorkMode::Object),
            NurbsModeCv  => self.nurbs_switch_work_mode(NurbsWorkMode::Vertex),
            NurbsModeSrf => self.nurbs_switch_work_mode(NurbsWorkMode::Face),
            // NURBS tools (toggle on re-click)
            NurbsKnotU   => self.nurbs_tool = if self.nurbs_tool == NurbsTool::InsertKnotU { NurbsTool::Idle } else { NurbsTool::InsertKnotU },
            NurbsKnotV   => self.nurbs_tool = if self.nurbs_tool == NurbsTool::InsertKnotV { NurbsTool::Idle } else { NurbsTool::InsertKnotV },
            NurbsWeight  => self.nurbs_tool = if self.nurbs_tool == NurbsTool::AdjustWeight { NurbsTool::Idle } else { NurbsTool::AdjustWeight },
            NurbsLoft    => self.nurbs_tool = if self.nurbs_tool == NurbsTool::Loft { NurbsTool::Idle } else { NurbsTool::Loft },
            NurbsRevolve => self.nurbs_tool = if self.nurbs_tool == NurbsTool::Revolve { NurbsTool::Idle } else { NurbsTool::Revolve },
            // NURBS surface creation
            NurbsCreatePlane    => self.create_nurbs_surface("plane"),
            NurbsCreateCylinder => self.create_nurbs_surface("cylinder"),
            NurbsCreateSphere   => self.create_nurbs_surface("sphere"),
            NurbsCreateTorus    => self.create_nurbs_surface("torus"),
            // Generic command
            Command(idx) => {
                if let Some(cmd) = COMMAND_DEFS.get(idx as usize) {
                    self.execute_command_by_id(cmd.id);
                }
            }
            Separator | Label(_) => {}
        }
    }

    /// When a sculpt brush button is clicked:
    /// - If already in voxel Sculpt mode, just switch brush type
    /// - If in SDF Object mode with primitives, auto-bake SDF → voxel and enter Sculpt mode
    fn activate_sdf_sculpt_brush(&mut self, _bt: BrushType) {
        // If already in voxel sculpt mode, just update brush — done
        if self.tool.work_mode == WorkMode::Sculpt {
            self.sdf_sculpt_active = true;
            return;
        }
        // Auto-convert SDF → voxel if we have primitives
        if self.modeling_context == ModelingContext::Sdf && !self.scene.primitives.is_empty() {
            self.transaction("Enter Sculpt", |s| {
                let t0 = std::time::Instant::now();
                s.cache.cpu_volume.bake_from_primitives(&s.scene.primitives);
                let elapsed = t0.elapsed();
                // Store original visibility for potential restore
                s.sculpt_source_visible = Some(s.scene.primitives.iter().map(|p| p.visible).collect());
                // Hide SDF prims — voxel grid replaces them
                for i in 0..s.scene.primitives.len() { s.write_prim(i, |p| p.visible = false); }
                s.tool.work_mode = WorkMode::Sculpt;
                s.sdf_sculpt_active = true;
                s.mark_dirty(DirtyFlags::RENDER);
                let res = s.cache.cpu_volume.resolution[0];
                s.toast(&format!("SDF → Sculpt Grid ({}³) — {:.1}ms", res, elapsed.as_secs_f64() * 1000.0));
            });
        }
    }

    /// Restore the SDF-room render state on LEAVING the Sculpt room. (The historical
    /// `force_volume_only` flag flip is gone — the flag was deleted with the legacy volume
    /// display path; the only remaining work is deactivating the live voxel.)
    pub(crate) fn restore_sdf_room_render_state(&mut self) {
        // Leaving Sculpt: deactivate the live voxel to its stable Object-Mode display mesh FIRST
        // (flush its sculpt into the node, free the live per-chunk meshes, re-arm RENDER). In Object
        // mode it then renders as a plain mesh node instead of off the live chunks, which the bare
        // mode-switch didn't repaint — the "voxel disappears until I move the camera" bug. No-op if
        // no voxel is being edited; selection is kept so re-entering Sculpt reloads the same voxel.
        self.voxel_leave_edit();
        self.poly_sculpt_leave_edit();
        if self.tool.work_mode == WorkMode::Sculpt {
            self.tool.work_mode = WorkMode::Object;
        }
        self.arm_default_selection_tool();
        // P0.2-full GUARD 7: Object Mode is unified raster — SDF renders from its BAKED TRIANGLE
        // MESH alongside voxel + poly. The crash class (analytical march fused with voxel raster)
        // is eliminated structurally by the resolve_render_mode/episode-latch guards.
    }

    /// Exit sculpt mode: restore SDF prims visibility, keep voxel data
    pub(crate) fn exit_sculpt_mode(&mut self) {
        if let Some(vis) = self.sculpt_source_visible.take() {
            for (i, &v) in vis.iter().enumerate() {
                self.write_prim(i, |p| p.visible = v);
            }
        }
        self.tool.work_mode = WorkMode::Object;
        self.restore_sdf_room_render_state();
        self.sdf_sculpt_active = false;
        self.mark_dirty(DirtyFlags::RENDER);
    }

    /// Discard sculpt work and restore original SDF
    pub(crate) fn discard_sculpt_restore_sdf(&mut self) {
        if let Some(vis) = self.sculpt_source_visible.take() {
            for (i, &v) in vis.iter().enumerate() {
                self.write_prim(i, |p| p.visible = v);
            }
        }
        // Clear voxel grid
        let res = self.cache.cpu_volume.resolution[0];
        let bounds = self.cache.cpu_volume.grid_bounds;
        self.cache.cpu_volume = core_math::sdf_volume::SdfVolume::new_empty(res, bounds);
        self.tool.work_mode = WorkMode::Object;
        self.restore_sdf_room_render_state();
        self.sdf_sculpt_active = false;
        self.mark_dirty(DirtyFlags::RENDER);
        self.toast("Sculpt discarded — SDF restored");
    }

    // ================================================================
    // Contextual Tool Properties (Modo-style, bottom of left panel)
    // ================================================================

    pub(crate) fn render_tool_properties(&mut self, ui: &mut egui::Ui) {
        let pw = ui.available_width();
        let small = pw < 70.0;

        ui.separator();
        ui.label(egui::RichText::new("Properties").small().color(egui::Color32::from_rgb(140, 140, 160)));

        match self.tool.work_mode {
            WorkMode::Sculpt => {
                // NEW voxel sculpting room controls (left panel). The legacy SDF brush
                // UI (Standard/Smooth + Grid picker) is retired.
                // Stacked, short-label layout: every child must stay narrow, because
                // the panel uses vertical_centered (sizes to the WIDEST child) — a wide
                // row or long string stretched the panel and left a black gap.
                ui.colored_label(egui::Color32::from_rgb(120, 200, 255), egui::RichText::new("🧊 Voxel").small().strong());
                if ui.small_button("● Sphere").clicked() { self.new_voxel_sphere(); }
                if ui.small_button("■ Box").clicked() { self.new_voxel_box(); }
                if ui.small_button("⬡ Cyl").on_hover_text("Voxel Cylinder").clicked() { self.new_voxel_cylinder(); }
                if ui.small_button("Clear").clicked() { self.clear_voxels(); }
                ui.separator();
                if ui.small_button("↶ Undo").on_hover_text("Ctrl+Z").clicked() { self.undo(); }
                if ui.small_button("↷ Redo").on_hover_text("Ctrl+Y").clicked() { self.redo(); }
                // Move/rotate/scale the voxel object: select it + drop to Object mode so
                // the gizmo (G/R/S) is available; sculpting follows the transform on return.
                if self.sculpt.voxel.is_some()
                    && ui.small_button("✥ Transform").on_hover_text("Select in Object mode to move/rotate/scale (G/R/S)").clicked()
                {
                    if let Some(n) = self.sculpt.voxel.as_ref().map(|v| v.node) {
                        self.select_scene_node(n);
                        self.tool.work_mode = WorkMode::Object;
                        self.mark_dirty(DirtyFlags::RENDER);
                    }
                }
                ui.separator();
                // Deferred actions — collected inside the &mut v borrow, run after it ends.
                let mut act_remesh_to: Option<u32> = None;
                let mut act_open_remesh = false;
                let mut act_gpu_parity = false;
                let mut act_brush_parity = false;
                if let Some(v) = self.sculpt.voxel.as_mut() {
                    use core_math::voxel_sculpt::VoxelBrush;
                    ui.spacing_mut().slider_width = 60.0; // keep sliders (with value box) within the strip
                    for (b, name, tip) in [
                        (VoxelBrush::Draw, "Draw", "Deposit material along the surface normal"),
                        (VoxelBrush::Erase, "Erase", "Carve material away"),
                        (VoxelBrush::Smooth, "Smooth", "Relax toward the local average (also: hold Shift with any brush)"),
                        (VoxelBrush::Flatten, "Flatten", "Push the surface toward the contact plane"),
                        (VoxelBrush::Clay, "Clay", "Even buildup — fills cavities faster than ridges"),
                        (VoxelBrush::Scrape, "Scrape", "One-directional flatten — shave material above the plane (Ctrl flips side)"),
                        (VoxelBrush::Crease, "Crease", "Sharp narrow groove / panel line (Ctrl raises a ridge)"),
                        (VoxelBrush::Fill, "Fill", "Deposit only into cavities below the plane (Ctrl flips side)"),
                        (VoxelBrush::ClayBuildup, "Build", "Blocky planar buildup — hard-surface base forms / plates"),
                        (VoxelBrush::Inflate, "Inflate", "Puff the surface outward along its normal (Ctrl deflates)"),
                        (VoxelBrush::Pinch, "Pinch", "Pull material toward the stroke to sharpen edges (Ctrl bulges)"),
                        (VoxelBrush::Move, "Move", "Grab the surface and drag it along the cursor (Ctrl reverses)"),
                    ] {
                        if ui.selectable_label(v.brush == b, name).on_hover_text(tip).clicked() { v.set_active_brush(b); }
                    }
                    ui.add(egui::Slider::new(&mut v.radius, 0.05..=1.0).text("R").step_by(0.005))
                        .on_hover_text("Radius");
                    ui.add(egui::Slider::new(&mut v.strength, 0.05..=1.0).text("S").step_by(0.01))
                        .on_hover_text("Strength");
                    ui.add(egui::Slider::new(&mut v.lazy, 0.0..=1.0).text("Lazy").step_by(0.01))
                        .on_hover_text("Stroke stabilizer (LazyMouse) — higher = smoother & laggier, 0 = off");
                    ui.small("LMB·⇧·⌃").on_hover_text("LMB draw · Shift smooth · Ctrl erase");
                    ui.separator();
                    // Resolution / remesh (3D-Coat style). Live readout + stacked presets
                    // (one per line — a horizontal row would widen this vertical_centered strip).
                    ui.small(format!("Res {}³ · {} tris", v.grid.res, v.tris));
                    for r in [32u32, 64, 128, 192] {
                        if ui.selectable_label(v.grid.res == r, format!("{}³", r))
                            .on_hover_text("Remesh to this resolution").clicked() { act_remesh_to = Some(r); }
                    }
                    if ui.small_button("⟳ Remesh…").on_hover_text("Choose polygon count (Ctrl+R)").clicked() {
                        act_open_remesh = true;
                    }
                    ui.separator();
                    // Sculpt profiler — splits the per-stroke CPU cost into brush vs mesh so we can
                    // see the real bottleneck at this resolution. Toggle off = no readout.
                    ui.checkbox(&mut v.profile.enabled, "⏱ Profile")
                        .on_hover_text("Show per-phase sculpt timing (brush writes vs Surface-Nets re-mesh)");
                    ui.checkbox(&mut v.gpu_mesh, "⚡ GPU mesh")
                        .on_hover_text("Mesh dirty chunks on the GPU (voxel_mesh.wgsl) instead of the CPU. Watch the ⏱ mesh ms drop. Off = CPU fallback.");
                    ui.add_enabled(v.gpu_mesh, egui::Checkbox::new(&mut v.gpu_resident_mesh, "⚡ resident mesh"))
                        .on_hover_text("EXPERIMENTAL (Phase 6). Meshed chunks stay in GPU buffers drawn via draw_indexed_indirect — NO per-chunk CPU readback, removing the ~5000 chunks/sec ceiling that causes fast-stroke chunk-patch tearing. Watch `dirty` drop to ~0 on fast strokes. NOTE: while ON the CPU mesh_cache is stale — toggle OFF before saving/exporting to bake the surface. Off = readback path.");
                    ui.checkbox(&mut v.gpu_brush, "⚡ GPU brush")
                        .on_hover_text("Apply the 8 in-place brush ops on the GPU (voxel_brush.wgsl). Watch the ⏱ brush ms drop. Smooth/Inflate/Pinch stay CPU. Off = CPU fallback.");
                    ui.add_enabled(v.gpu_brush, egui::Checkbox::new(&mut v.gpu_brush_atlas, "⚡ atlas-resident"))
                        .on_hover_text("EXPERIMENTAL (Phase 5a). Sparse grids only: the brush writes the GPU atlas in place — no per-frame CPU box round-trip — and the CPU grid syncs once on release. Kills the large-brush stutter. Pinch falls back to the box path. Off = box-round-trip GPU brush.");
                    if v.profile.enabled {
                        let p = v.profile;
                        ui.small(format!("brush {:.1}ms (pk {:.1}) ×{}", p.brush_ms, p.brush_peak, p.stamps))
                            .on_hover_text("apply_brush (density writes) this frame · peak · stamps");
                        ui.small(format!("mesh  {:.1}ms (pk {:.1}) ×{}ch", p.mesh_ms, p.mesh_peak, p.chunks))
                            .on_hover_text("Surface-Nets re-mesh of dirty chunks this frame (CPU command-recording cost) · peak · chunks");
                        // MESH-bound vs DRAW-bound diagnostic: GPU time of the mesh passes vs the whole frame.
                        // gpu small + frame large = DRAW-bound (12k draw calls) → pooled-buffer/multi-draw fix.
                        // gpu large = MESH-bound → work-budget / less-mesh fix. Read these WHILE sculpting 1024³.
                        ui.small(egui::RichText::new(format!("gpu-mesh {:.2}ms • frame {:.1}ms", p.mesh_gpu_ms, p.frame_ms))
                            .color(egui::Color32::from_rgb(120, 200, 255)))
                            .on_hover_text("gpu-mesh = GPU time of the two mesh compute passes (mesher's own timestamp). frame = whole previous frame (wall). gpu-mesh small but frame large ⇒ DRAW-bound (too many draw calls), not mesh-bound.");
                        // DIAGNOSTIC for the fast-stroke tearing: `dirty` = chunks still waiting to
                        // re-mesh (grows large = mesher can't keep up); `stamps raw→used` (raw >> used =
                        // the stroke is being subsampled → gaps). Read these WHILE sculpting fast.
                        ui.small(egui::RichText::new(format!("dirty {} • cap {} • stamps {}→{}", p.dirty_backlog, v.resident_cap, p.stamps_raw, p.stamps))
                            .color(if p.dirty_backlog > 200 { egui::Color32::from_rgb(255,120,80) } else { egui::Color32::from_gray(150) }))
                            .on_hover_text("dirty = chunk re-mesh backlog (high = mesh can't keep up); cap = chunks meshed per frame (auto-tunes to keep frames <24ms); stamps raw→used (raw >> used = subsampled → gaps)");
                        if ui.small_button("⟲ reset peaks").clicked() {
                            v.profile.brush_peak = 0.0; v.profile.mesh_peak = 0.0;
                        }
                        if ui.small_button("GPU parity").on_hover_text(
                            "DEBUG: mesh the grid on GPU (voxel_mesh.wgsl) and compare counts + vertex positions to the CPU mesher").clicked() {
                            act_gpu_parity = true;
                        }
                        if ui.small_button("Brush parity").on_hover_text(
                            "DEBUG: apply each of the 11 brush ops on GPU (voxel_brush.wgsl) vs CPU; report worst |Δdensity|").clicked() {
                            act_brush_parity = true;
                        }
                        // Persistent result (the toast is too brief to read). Green-ish if OK.
                        if !v.parity_msg.is_empty() {
                            let col = if v.parity_msg.contains("OK") {
                                egui::Color32::from_rgb(120, 200, 120)
                            } else {
                                egui::Color32::from_rgb(220, 140, 80)
                            };
                            ui.colored_label(col, egui::RichText::new(&v.parity_msg).small());
                        }
                    }
                } else {
                    ui.weak("Pick base");
                }
                if let Some(r) = act_remesh_to { self.voxel_resample_to(r); }
                if act_open_remesh { self.voxel_open_remesh(); }
                if act_gpu_parity { self.voxel_gpu_parity_check(); }
                if act_brush_parity { self.voxel_gpu_brush_parity(); }
                self.draw_voxel_remesh_popup(ui.ctx());
                self.draw_poly_remesh_popup(ui.ctx());
            }

            WorkMode::Object => {
                if self.sdf_sculpt_active && self.modeling_context == ModelingContext::Sdf {
                    // SDF Sculpt brush properties
                    ui.label(egui::RichText::new(format!("Sculpt: {}", self.sdf_sculpt_op.label())).small().strong().color(egui::Color32::from_rgb(200, 160, 80)));
                    ui.add(egui::Slider::new(&mut self.sdf_sculpt_radius, 0.01..=2.0).text(if small { "R" } else { "Radius" }).step_by(0.01));
                    ui.add(egui::Slider::new(&mut self.sdf_sculpt_strength, 0.01..=1.0).text(if small { "S" } else { "Strength" }).step_by(0.01));
                    // Op selector
                    {
                        use core_math::primitives::SculptOp;
                        let cur = self.sdf_sculpt_op;
                        egui::ComboBox::from_id_salt("sdf_sculpt_op_tp")
                            .width(pw - 8.0)
                            .selected_text(cur.label())
                            .show_ui(ui, |ui| {
                                for op in [SculptOp::Push, SculptOp::Pull, SculptOp::Flatten, SculptOp::Inflate, SculptOp::Pinch, SculptOp::Crease] {
                                    if ui.selectable_label(op == cur, op.label()).clicked() {
                                        self.sdf_sculpt_op = op;
                                    }
                                }
                            });
                    }
                    if ui.small_button("Disable Sculpt").clicked() {
                        self.sdf_sculpt_active = false;
                    }
                } else {
                    let gizmo_label = match self.tool.gizmo_mode {
                        GizmoMode::Translate => "Move",
                        GizmoMode::Rotate => "Rotate",
                        GizmoMode::Scale => "Scale",
                        GizmoMode::Elongate => "Elongate",
                    };
                    ui.label(egui::RichText::new(gizmo_label).small().strong().color(egui::Color32::from_rgb(200, 200, 220)));
                }
            }

            WorkMode::Vertex | WorkMode::Edge | WorkMode::Face => {
                let tool_name = match self.poly_tool {
                    PolyTool::Idle => "Select",
                    PolyTool::ExtrudeFace => "Extrude",
                    PolyTool::BevelEdge => "Bevel",
                    PolyTool::LoopCut => "Loop Cut",
                    PolyTool::Knife => "Knife",
                    PolyTool::MergeVertices => "Merge",
                    PolyTool::InsetFace => "Inset",
                    PolyTool::Dice => "Dice",
                };
                ui.label(egui::RichText::new(tool_name).small().strong().color(egui::Color32::from_rgb(200, 200, 220)));
                ui.label(egui::RichText::new(format!("{:?}", self.poly_work_mode)).small().color(egui::Color32::from_rgb(160, 160, 180)));

                // Active bevel drag: show interactive sliders
                let mut bevel_slider_changed = false;
                if let Some(d) = self.tool.current.as_poly_bevel_mut() {
                    // [Codex P1] Capture .changed() so an edit to a slider regenerates the mesh (previously
                    // only mouse-drag/scroll regenerated → the mesh disagreed with the numeric settings).
                    bevel_slider_changed |= ui.add(egui::Slider::new(&mut d.width, 0.001..=2.0).text(if small { "W" } else { "Width" }).step_by(0.001)).changed();
                    bevel_slider_changed |= ui.add(egui::Slider::new(&mut d.segments, 1..=8).text(if small { "S" } else { "Segments" })).changed();
                    bevel_slider_changed |= ui.add(egui::Slider::new(&mut d.shape, 0.0..=1.0).text(if small { "P" } else { "Profile" }).step_by(0.01)).changed();
                    // [Codex P2] Profile semantics: 0 = flat chamfer, 0.5 = round (circular) fillet, 1 = convex.
                    let profile_label = if d.shape < 0.25 { "Chamfer" } else if d.shape < 0.75 { "Round" } else { "Convex" };
                    ui.label(egui::RichText::new(profile_label).small().color(egui::Color32::from_rgb(160, 160, 180)));
                }
                if bevel_slider_changed { self.poly_bevel_regenerate(); }
                // Active extrude drag: show offset
                else if let Some(d) = self.tool.current.as_poly_extrude() {
                    ui.label(egui::RichText::new(format!("Offset: {:.3}", d.accumulated_offset)).small().color(egui::Color32::from_rgb(200, 180, 100)));
                }
                // Active inset drag: amount + the Blender mode it is running in (set before starting).
                else if let Some(d) = self.tool.current.as_poly_inset() {
                    let amt = d.accumulated_amount;
                    ui.label(egui::RichText::new(format!("Amount: {:.3}", amt)).small().color(egui::Color32::from_rgb(200, 180, 100)));
                    let mode = if self.poly_inset_individual { "Individual" } else { "Region" };
                    ui.label(egui::RichText::new(mode).small().color(egui::Color32::from_rgb(150, 150, 160)));
                }
                // Inset (Face mode, idle): Blender's Region ("group faces", the I default) vs Individual.
                else if self.poly_work_mode == PolyWorkMode::Face
                    && self.modeling_context == ModelingContext::Polygonal
                    && self.tool.current.is_idle()
                    && !self.poly_sel.selected_faces.is_empty()
                {
                    ui.checkbox(&mut self.poly_inset_individual, if small { "Indiv" } else { "Inset: Individual" })
                        .on_hover_text("Off = Region (Blender's I default): the selection is ONE region, only its outer boundary is inset.\nOn = Individual (I,I): every selected face gets its own ring.");
                }
                // Loop cut: count slider + Live/Selected/Curve toggles (drag=slide, scroll=count,
                // S=paired, RMB/Enter=confirm, Esc=cancel).
                else if self.poly_tool == PolyTool::LoopCut {
                    ui.add(egui::Slider::new(&mut self.loop_cut_count, 1..=20).text(if small { "N" } else { "Count" }));
                    ui.checkbox(&mut self.loop_cut_live, if small { "Live" } else { "Live geometry" })
                        .on_hover_text("On: slide the real cut (WYSIWYG). Off: yellow preview until confirm.");
                    // Same toggles as Tool Parameters — the two must not disagree.
                    if ui
                        .checkbox(&mut self.loop_cut_selected, if small { "Sel" } else { "Selected" })
                        .on_hover_text("On: selected edges drive multi-ring / partial cuts.\nOff: free full loop from hover only (selection ignored; walks n-gons).")
                        .changed()
                    {
                        if self.tool.current.is_poly_loop_cut_slide() {
                            self.poly_loop_cut_reseed_session();
                        }
                        self.mark_dirty(DirtyFlags::RENDER);
                    }
                    if ui
                        .checkbox(&mut self.loop_cut_follow_curvature, if small { "Curve" } else { "Follow curvature" })
                        .on_hover_text("On: place the new loop on the arc through the crossed edges' endpoint normals, so it follows a rounded form instead of flattening it. Flat faces are unaffected.")
                        .changed()
                    {
                        let c = self.loop_cut_effective_curvature();
                        if let Some(d) = self.tool.current.as_poly_loop_cut_slide_mut() { d.curvature = c; }
                        if self.loop_cut_live { self.poly_loop_cut_regenerate(); }
                        self.mark_dirty(DirtyFlags::RENDER);
                    }
                }
                // Active transform: show axis constraint
                else if let Some(ts) = self.tool.current.as_poly_transform() {
                    let axis_str = match ts.axis {
                        crate::AxisConstraint::None => "Free",
                        crate::AxisConstraint::X => "X",
                        crate::AxisConstraint::Y => "Y",
                        crate::AxisConstraint::Z => "Z",
                        crate::AxisConstraint::PlaneX => "YZ",
                        crate::AxisConstraint::PlaneY => "XZ",
                        crate::AxisConstraint::PlaneZ => "XY",
                    };
                    ui.label(egui::RichText::new(format!("Axis: {}", axis_str)).small().color(egui::Color32::from_rgb(160, 160, 180)));
                }
            }
        }
    }

    // ================================================================
    // Render workspace panels
    // ================================================================

    /// Render all panels in the active workspace.
    pub(crate) fn draw_workspace_panels(&mut self, ctx: &egui::Context) {
        let panels: Vec<WorkspacePanel> = self.workspace_mgr.active().panels.clone();
        let ctrl = ctx.input(|i| i.modifiers.command);
        let edit_mode = self.workspace_mgr.edit_mode;
        let mut removals: Vec<(usize, usize)> = Vec::new(); // (panel_idx, btn_idx)

        for (pi, panel) in panels.iter().enumerate() {
            if panel.collapsed { continue; }
            let render_panel_inner = |app: &mut AzraApp, ui: &mut egui::Ui, removals: &mut Vec<(usize, usize)>| {
                // Edit mode: panel header with dock controls
                if edit_mode {
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new(&panel.name).strong().small().color(egui::Color32::from_rgb(200, 200, 200)));
                        if ui.small_button("L").on_hover_text("Dock Left").clicked() {
                            if let Some(p) = app.workspace_mgr.active_mut().panels.get_mut(pi) {
                                p.dock = PanelDock::Left;
                            }
                        }
                        if ui.small_button("R").on_hover_text("Dock Right").clicked() {
                            if let Some(p) = app.workspace_mgr.active_mut().panels.get_mut(pi) {
                                p.dock = PanelDock::Right;
                            }
                        }
                        if ui.small_button("F").on_hover_text("Float").clicked() {
                            if let Some(p) = app.workspace_mgr.active_mut().panels.get_mut(pi) {
                                p.dock = PanelDock::Float { x: 100.0, y: 100.0 };
                            }
                        }
                        if !panel.builtin_default && ui.small_button("Del").on_hover_text("Delete Panel").clicked() {
                            app.workspace_mgr.active_mut().panels.remove(pi);
                            app.workspace_mgr.save();
                            return;
                        }
                    });
                    ui.separator();
                }
                // Render buttons
                for (bi, &btn) in panel.buttons.iter().enumerate() {
                    if edit_mode {
                        ui.horizontal(|ui| {
                            app.render_tool_button(ui, btn, ctrl);
                            if ui.small_button("x").on_hover_text("Remove button").clicked() {
                                removals.push((pi, bi));
                            }
                        });
                    } else {
                        app.render_tool_button(ui, btn, ctrl);
                    }
                }
                // Edit mode: add button at bottom
                if edit_mode {
                    ui.separator();
                    if ui.small_button("+ Add").on_hover_text("Add button to panel").clicked() {
                        app.ui.ws_add_popup = Some(pi);
                        app.ui.ws_add_search.clear();
                    }
                }
                // Contextual Tool Properties (Modo-style, bottom of left panel)
                if !edit_mode {
                    app.render_tool_properties(ui);
                }
            };

            match &panel.dock {
                PanelDock::Left => {
                    // Widen the tool strip for the voxel sculpt controls, and keep the
                    // width AUTHORITATIVE (resizable only in edit mode) so egui can't
                    // persist a stale over-wide width that leaves a black gap beside the
                    // content (the "black bar" bug).
                    let sculpt = self.tool.work_mode == WorkMode::Sculpt;
                    let pid = if sculpt {
                        egui::Id::new(format!("{}__voxfw", panel.id))
                    } else {
                        egui::Id::new(&panel.id)
                    };
                    let mut sp = egui::SidePanel::left(pid);
                    if sculpt {
                        // exact_width PINS the panel: egui can't auto-expand to content
                        // (the source of the black gap) nor restore a stale persisted width.
                        sp = sp.exact_width(150.0);
                    } else {
                        sp = sp.resizable(edit_mode).default_width(panel.width).width_range(42.0..=132.0);
                    }
                    sp.show(ctx, |ui| {
                            egui::ScrollArea::vertical().show(ui, |ui| {
                                ui.vertical_centered(|ui| {
                                    render_panel_inner(self, ui, &mut removals);
                                });
                            });
                        });
                }
                PanelDock::Right => {
                    egui::SidePanel::right(egui::Id::new(&panel.id))
                        .resizable(true)
                        .default_width(panel.width)
                        .width_range(42.0..=120.0)
                        .show(ctx, |ui| {
                            egui::ScrollArea::vertical().show(ui, |ui| {
                                ui.vertical_centered(|ui| {
                                    render_panel_inner(self, ui, &mut removals);
                                });
                            });
                        });
                }
                PanelDock::Float { x, y } => {
                    egui::Area::new(egui::Id::new(&panel.id))
                        .default_pos(egui::pos2(*x, *y))
                        .movable(true)
                        .order(egui::Order::Middle)
                        .show(ctx, |ui| {
                            egui::Frame::popup(ui.style())
                                .inner_margin(4.0)
                                .show(ui, |ui| {
                                    render_panel_inner(self, ui, &mut removals);
                                });
                        });
                }
            }
        }

        // Apply deferred removals (reverse order to keep indices valid)
        if !removals.is_empty() {
            removals.sort_by(|a, b| b.1.cmp(&a.1).then(b.0.cmp(&a.0)));
            for (pi, bi) in removals {
                if let Some(panel) = self.workspace_mgr.active_mut().panels.get_mut(pi) {
                    if bi < panel.buttons.len() {
                        panel.buttons.remove(bi);
                    }
                }
            }
            self.workspace_mgr.save();
        }

        // Add Button popup
        self.draw_add_button_popup(ctx);

        // Workspace tab context menu
        self.draw_workspace_context_menu(ctx);
    }

    // ================================================================
    // Add Button popup (searchable catalog of all ToolButtonId variants)
    // ================================================================
    fn draw_add_button_popup(&mut self, ctx: &egui::Context) {
        let Some(panel_idx) = self.ui.ws_add_popup else { return };

        let mut open = true;
        egui::Window::new("Add Button")
            .open(&mut open)
            .default_width(180.0)
            .default_height(300.0)
            .collapsible(false)
            .show(ctx, |ui| {
                ui.text_edit_singleline(&mut self.ui.ws_add_search);
                ui.separator();
                egui::ScrollArea::vertical().max_height(250.0).show(ui, |ui| {
                    let search = self.ui.ws_add_search.to_lowercase();
                    for (cat, buttons) in Self::button_catalog() {
                        let filtered: Vec<_> = buttons.iter()
                            .filter(|(_, name)| search.is_empty() || name.to_lowercase().contains(&search))
                            .collect();
                        if filtered.is_empty() { continue; }
                        ui.label(egui::RichText::new(cat).strong().small().color(egui::Color32::from_gray(140)));
                        for &(btn_id, name) in &filtered {
                            let meta = tool_button_meta(*btn_id);
                            if ui.button(format!("{} {}", meta.icon, name)).clicked() {
                                if let Some(panel) = self.workspace_mgr.active_mut().panels.get_mut(panel_idx) {
                                    panel.buttons.push(*btn_id);
                                }
                                self.workspace_mgr.save();
                                self.ui.ws_add_popup = None;
                                return;
                            }
                        }
                        ui.add_space(4.0);
                    }
                    // Visual elements
                    ui.label(egui::RichText::new("Visual").strong().small().color(egui::Color32::from_gray(140)));
                    if ui.button("--- Separator").clicked() {
                        if let Some(panel) = self.workspace_mgr.active_mut().panels.get_mut(panel_idx) {
                            panel.buttons.push(ToolButtonId::Separator);
                        }
                        self.workspace_mgr.save();
                        self.ui.ws_add_popup = None;
                    }
                    for (idx, name) in LABEL_NAMES.iter().enumerate() {
                        if !search.is_empty() && !name.to_lowercase().contains(&search) { continue; }
                        if ui.button(format!("Label: {}", name)).clicked() {
                            if let Some(panel) = self.workspace_mgr.active_mut().panels.get_mut(panel_idx) {
                                panel.buttons.push(ToolButtonId::Label(idx as u8));
                            }
                            self.workspace_mgr.save();
                            self.ui.ws_add_popup = None;
                        }
                    }
                });
            });
        if !open { self.ui.ws_add_popup = None; }
    }

    /// Returns categorized list of all tool buttons for the add-button catalog.
    fn button_catalog() -> Vec<(&'static str, Vec<(ToolButtonId, &'static str)>)> {
        use ToolButtonId::*;
        vec![
            ("Mode", vec![(ModeObject, "Object"), (ModeSculpt, "Sculpt"), (ModeFace, "Face")]),
            ("SDF Shape", vec![
                (ShapeSphere, "Sphere"), (ShapeBox, "Box"), (ShapeCylinder, "Cylinder"),
                (ShapeTorus, "Torus"), (ShapeCone, "Cone"), (ShapeCapsule, "Capsule"),
                (ShapeStair, "Stair"), (ShapeSpiral, "Spiral"),
                (ShapePolyline, "Polyline"), (ShapeSpine, "Spine / Pipe"),
                (ToolBoolCut, "Bool Cut"),
            ]),
            ("Boolean Op", vec![
                (OpSubtract, "Subtract"), (OpUnion, "Union"), (OpIntersect, "Intersect"), (OpSlice, "Slice"),
            ]),
            ("Gizmo", vec![
                (GizmoMove, "Move"), (GizmoRotate, "Rotate"), (GizmoScale, "Scale"),
            ]),
            ("Brush", vec![
                (BrushAdd, "Add Clay"), (BrushCarve, "Carve"), (BrushSmooth, "Smooth"),
                (BrushFlatten, "Flatten"), (BrushInflate, "Inflate"),
                (BrushPinch, "Pinch"), (BrushClay, "Clay"), (BrushCrease, "Crease"), (BrushTrim, "Trim"),
            ]),
            ("Convert", vec![(ConvertToClay, "Convert to Clay")]),
            ("Poly Create", vec![
                (PolyCreateBox, "Box"), (PolyCreatePlane, "Plane"), (PolyCreateSphere, "Sphere"),
                (PolyCreateCylinder, "Cylinder"), (PolyCreateTorus, "Torus"), (PolyCreateCone, "Cone"),
            ]),
            ("NURBS Mode", vec![
                (NurbsModeObj, "Object"), (NurbsModeCv, "CV"), (NurbsModeSrf, "Surface"),
            ]),
            ("NURBS Edit", vec![
                (NurbsKnotU, "Knot U"), (NurbsKnotV, "Knot V"), (NurbsWeight, "Weight"),
                (NurbsLoft, "Loft"), (NurbsRevolve, "Revolve"),
            ]),
            ("NURBS Create", vec![
                (NurbsCreatePlane, "Plane"), (NurbsCreateCylinder, "Cylinder"),
                (NurbsCreateSphere, "Sphere"), (NurbsCreateTorus, "Torus"),
            ]),
        ]
    }

    // ================================================================
    // Workspace tab context menu (right-click)
    // ================================================================
    fn draw_workspace_context_menu(&mut self, ctx: &egui::Context) {
        let Some(ws_idx) = self.ui.ws_context_menu else { return };
        if ws_idx >= self.workspace_mgr.workspaces.len() {
            self.ui.ws_context_menu = None;
            return;
        }

        let mut close = false;
        egui::Area::new(egui::Id::new("ws_ctx_menu"))
            .order(egui::Order::Foreground)
            .default_pos(ctx.input(|i| i.pointer.hover_pos().unwrap_or(egui::pos2(100.0, 30.0))))
            .show(ctx, |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    let ws = &self.workspace_mgr.workspaces[ws_idx];
                    ui.label(egui::RichText::new(&ws.name).strong().small());
                    ui.separator();
                    if ui.button("Duplicate").clicked() {
                        let mut dup = self.workspace_mgr.workspaces[ws_idx].clone();
                        // Strip ".copy" / " (copy)" chains and numeric suffix to get base name
                        let mut base = dup.name.clone();
                        while base.ends_with(".copy") { base.truncate(base.len() - 5); }
                        while base.ends_with(" (copy)") { base.truncate(base.len() - 7); }
                        if let Some(dp) = base.rfind('.') {
                            if base[dp + 1..].chars().all(|c| c.is_ascii_digit()) && dp + 1 < base.len() {
                                base.truncate(dp);
                            }
                        }
                        let existing: std::collections::HashSet<&str> = self.workspace_mgr.workspaces.iter().map(|w| w.name.as_str()).collect();
                        dup.name = (1u32..).map(|i| format!("{}.{:03}", base, i)).find(|c| !existing.contains(c.as_str())).unwrap();
                        dup.builtin = false;
                        self.workspace_mgr.workspaces.push(dup);
                        self.workspace_mgr.save();
                        close = true;
                    }
                    if self.workspace_mgr.workspaces[ws_idx].builtin {
                        if ui.button("Reset to Default").clicked() {
                            self.workspace_mgr.workspaces[ws_idx] = match ws_idx {
                                0 => crate::Workspace::sdf_modeling(),
                                1 => crate::Workspace::sculpting(),
                                2 => crate::Workspace::poly_modeling(),
                                3 => crate::Workspace::nurbs(),
                                _ => crate::Workspace::sdf_modeling(),
                            };
                            self.workspace_mgr.save();
                            close = true;
                        }
                    }
                    if !self.workspace_mgr.workspaces[ws_idx].builtin && self.workspace_mgr.workspaces.len() > 1 {
                        if ui.button("Delete").clicked() {
                            self.workspace_mgr.workspaces.remove(ws_idx);
                            if self.workspace_mgr.active_idx >= self.workspace_mgr.workspaces.len() {
                                self.workspace_mgr.active_idx = 0;
                            }
                            self.workspace_mgr.save();
                            close = true;
                        }
                    }
                    if ui.button("New Panel (Float)").clicked() {
                        let new_id = format!("custom_panel_{}", self.workspace_mgr.active().panels.len() + 1);
                        self.workspace_mgr.active_mut().panels.push(WorkspacePanel {
                            id: new_id,
                            name: "Custom".into(),
                            buttons: vec![],
                            dock: PanelDock::Float { x: 200.0, y: 200.0 },
                            width: 52.0,
                            collapsed: false,
                            builtin_default: false,
                        });
                        self.workspace_mgr.save();
                        close = true;
                    }
                    // Close on click outside
                    if ui.input(|i| i.pointer.any_click()) && !ui.rect_contains_pointer(ui.max_rect()) {
                        close = true;
                    }
                });
            });
        if close { self.ui.ws_context_menu = None; }
        // Also close on Escape
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.ui.ws_context_menu = None;
        }
    }
}
