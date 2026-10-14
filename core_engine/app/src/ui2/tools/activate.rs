//! Tool palette dispatch only — scene work lives in `crate::ops::*` (one file per op).

use crate::*;
use super::super::*; // ToolPillar
use core_math::voxel_sculpt::VoxelBrush;

impl FourthLayerApp {
    /// One-shot commands from the tools list — thin dispatch to ops.
    pub(crate) fn ui2_tool_command(&mut self, pillar: ToolPillar, name: &str) {
        match (pillar, name) {
            (_, "Duplicate") => crate::ops::edit::duplicate::run(self),
            (_, "Create Instance") => crate::ops::instance::create::run(self),
            (_, "Focus Selected") | (_, "Focus") => crate::ops::edit::frame_selected::run(self),
            (_, "Export Mesh") | (_, "Export OBJ") => {
                crate::ops::edit::export_mesh_window::run(self)
            }
            (_, "Select All") => crate::ops::poly::select_all_toggle::run(self),
            (_, "Invert") => crate::ops::poly::invert_selection::run(self),
            (_, "Grow") => crate::ops::poly::grow_selection::run(self),
            (_, "Shrink") => crate::ops::poly::shrink_selection::run(self),
            (_, "Select Linked") => crate::ops::poly::select_linked::run(self),
            (_, "Hide Selected") => crate::ops::poly::hide_selected::run(self),
            (_, "Hide Unselected") => crate::ops::poly::hide_unselected::run(self),
            (_, "Unhide All") => crate::ops::poly::unhide_all::run(self),
            (_, "Cycle Op") => crate::ops::bool_op::cycle::run(self),
            (_, "Join Meshes") => crate::ops::poly::join::run(self),
            (_, "Merge") | (_, "Merge Vertices") => crate::ops::poly::merge_vertices::run(self),
            (_, "Merge by Distance") => crate::ops::poly::merge_vertices::by_distance(self),
            (_, "Extrude") => crate::ops::poly::extrude::run(self),
            (_, "Inset") => crate::ops::poly::inset::run(self),
            (_, "Solidify") => crate::ops::poly::solidify::run(self),
            (_, "Fill") if pillar != ToolPillar::Sculpt => crate::ops::poly::fill::run(self),
            (_, "Subdivide") => crate::ops::poly::subdivide::run(self),
            (_, "Delete Faces") => crate::ops::poly::delete_faces::run(self),
            (_, "Delete Vertices") => crate::ops::poly::delete_vertices::run(self),
            (_, "Delete Edges") => crate::ops::poly::delete_edges::run(self),
            (_, "Dissolve Faces") | (_, "Dissolve Edges") | (_, "Dissolve Vertices") => {
                crate::ops::poly::dissolve::run(self)
            }
            (_, "Bridge Loops") => crate::ops::poly::bridge::run(self),
            (_, "Recalculate Normals") => crate::ops::poly::recalculate_normals::run(self),
            (_, "Flip Normals") => crate::ops::poly::flip_normals::run(self),
            (_, "Loop Cut") => crate::ops::poly::loop_cut::run(self),
            (_, "Bevel") => crate::ops::poly::bevel_activate::run(self),
            (_, "Bevel Vertices") => {
                if self.poly_work_mode != crate::PolyWorkMode::Vertex {
                    self.poly_switch_work_mode(crate::PolyWorkMode::Vertex);
                }
                crate::ops::poly::bevel_activate::run(self);
            }
            (_, "Toggle Seam") => self.poly_mark_seam_apply(),
            (_, "Mark Seam") => self.poly_set_seam(true),
            (_, "Clear Seam") => self.poly_set_seam(false),
            (_, "Dice Faces") => self.poly_activate_dice(),
            // ── Sculpt commands (live VoxelObject) ──────────────────────────
            (ToolPillar::Sculpt, "Reset Brush") => self.ui2_sculpt_reset_brush(),
            (ToolPillar::Sculpt, "Smooth All") => self.ui2_sculpt_smooth_all(),
            (ToolPillar::Sculpt, "Voxelize") => self.ui2_sculpt_voxelize(),
            (ToolPillar::Sculpt, "Remesh") => {
                if self.sculpt.voxel.is_some() {
                    self.voxel_open_remesh();
                } else {
                    self.toast("No voxel object to remesh");
                }
            }
            (ToolPillar::Sculpt, "Invert Mask") => self.ui2_sculpt_mask_invert(),
            (ToolPillar::Sculpt, "Clear Mask") => self.ui2_sculpt_mask_clear(),
            other => self.toast(&format!("{} — not implemented", other.1)),
        }
    }

    /// Palette click → active-tool bus + ops dispatch (placement under cursor).
    pub(crate) fn ui2_tool_activate(&mut self, pillar: ToolPillar, name: &str) {
        self.ui2.active_pillar = match pillar {
            ToolPillar::Sdf => "sdf",
            ToolPillar::Poly => "poly",
            ToolPillar::Sculpt => "sculpt",
            ToolPillar::Nurbs => "nurbs",
        }
        .into();
        self.ui2.active_tool = match name {
            "Clay Buildup" => "ClayBuildup".into(),
            "Snake Hook" => "SnakeHook".into(),
            other => other.to_owned(),
        };

        let at = crate::ops::sdf::add_spawn_point(self);
        match (pillar, name) {
            (ToolPillar::Sdf, "Box") => crate::ops::sdf::box_shape::run(self, at),
            (ToolPillar::Sdf, "Sphere") => crate::ops::sdf::sphere::run(self, at),
            (ToolPillar::Sdf, "Cylinder") => crate::ops::sdf::cylinder::run(self, at),
            (ToolPillar::Sdf, "Torus") => crate::ops::sdf::torus::run(self, at),
            (ToolPillar::Sdf, "Cone") => crate::ops::sdf::cone::run(self, at),
            (ToolPillar::Sdf, "Capsule") => crate::ops::sdf::capsule::run(self, at),
            (ToolPillar::Sdf, "Stair") => crate::ops::sdf::stair::run(self, at),
            (ToolPillar::Sdf, "Spiral Stair") => crate::ops::sdf::spiral_stair::run(self, at),
            (ToolPillar::Sdf, "Polyline") => crate::ops::sdf::polyline::run(self, at),
            (ToolPillar::Sdf, "Draw Polyline") => crate::ops::sdf::draw_polyline::run(self),
            (ToolPillar::Sdf, "Plane") => crate::ops::sdf::plane_poly::run(self, at),
            (ToolPillar::Sdf, "Union") => crate::ops::bool_op::set_union::run(self),
            (ToolPillar::Sdf, "Subtract") => crate::ops::bool_op::set_subtract::run(self),
            (ToolPillar::Sdf, "Intersect") => crate::ops::bool_op::set_intersect::run(self),
            (ToolPillar::Sdf, "Slice") => crate::ops::bool_op::set_slice::run(self),
            (ToolPillar::Sdf, "Move") => {
                self.tool.gizmo_mode = crate::GizmoMode::Translate;
            }
            (ToolPillar::Sdf, "Rotate") => {
                self.tool.gizmo_mode = crate::GizmoMode::Rotate;
            }
            (ToolPillar::Sdf, "Scale") => {
                self.tool.gizmo_mode = crate::GizmoMode::Scale;
            }
            (ToolPillar::Poly, "Cube") => crate::ops::poly::mesh_create::box_mesh(self, at),
            (ToolPillar::Poly, "Plane") => crate::ops::poly::mesh_create::plane(self, at),
            (ToolPillar::Poly, "Circle") => crate::ops::poly::mesh_create::circle(self, at),
            (ToolPillar::Poly, "Sphere") => crate::ops::poly::mesh_create::sphere(self, at),
            (ToolPillar::Poly, "Ico Sphere") => crate::ops::poly::mesh_create::ico_sphere(self, at),
            (ToolPillar::Poly, "Cylinder") => crate::ops::poly::mesh_create::cylinder(self, at),
            (ToolPillar::Poly, "Cone") => crate::ops::poly::mesh_create::cone(self, at),
            (ToolPillar::Poly, "Torus") => crate::ops::poly::mesh_create::torus(self, at),
            (ToolPillar::Poly, "Duplicate") => crate::ops::edit::duplicate::run(self),
            (ToolPillar::Poly, "Merge") => crate::ops::poly::merge_vertices::run(self),
            (ToolPillar::Poly, "Merge by Distance") => {
                crate::ops::poly::merge_vertices::by_distance(self)
            }
            (ToolPillar::Poly, "Loop Cut") => crate::ops::poly::loop_cut::run(self),
            (ToolPillar::Poly, "Bevel") => crate::ops::poly::bevel_activate::run(self),
            // Vertex bevel (affect=VERTICES): ensure Vertex work mode so Ctrl+B's branch fires.
            (ToolPillar::Poly, "Bevel Vertices") => {
                if self.poly_work_mode != crate::PolyWorkMode::Vertex {
                    self.poly_switch_work_mode(crate::PolyWorkMode::Vertex);
                }
                crate::ops::poly::bevel_activate::run(self);
            }
            (ToolPillar::Poly, "Mark Seam") => self.poly_mark_seam_apply(),
            (ToolPillar::Poly, "Extrude") => crate::ops::poly::extrude::run(self),
            (ToolPillar::Poly, "Inset") => crate::ops::poly::inset::run(self),
            // ── Sculpt: every ready palette entry maps to VoxelBrush (or mask mode) ──
            (ToolPillar::Sculpt, brush_name) => {
                self.ui2_sculpt_activate_brush(brush_name);
            }
            _ => {}
        }
    }

    /// Map palette label → `VoxelBrush` / mask paint and arm Sculpt work mode.
    /// Single source of truth for palette + (later) ctxbar brush name.
    pub(crate) fn ui2_sculpt_activate_brush(&mut self, name: &str) {
        self.tool.work_mode = crate::WorkMode::Sculpt;
        // Mask is a mode flag on the stroke path, not a VoxelBrush discriminant.
        if name == "Mask" {
            if let Some(v) = self.sculpt.voxel.as_mut() {
                v.mask_paint = true;
            }
            self.ui2.active_tool = "Mask".into();
            self.toast("Mask paint — Ctrl clears");
            return;
        }
        let brush = match name {
            "Draw" => VoxelBrush::Draw,
            "Erase" => VoxelBrush::Erase,
            "Clay" => VoxelBrush::Clay,
            "Clay Buildup" | "ClayBuildup" => VoxelBrush::ClayBuildup,
            "Fill" => VoxelBrush::Fill,
            "Inflate" => VoxelBrush::Inflate,
            "Crease" => VoxelBrush::Crease,
            "Smooth" => VoxelBrush::Smooth,
            "Flatten" => VoxelBrush::Flatten,
            "Scrape" => VoxelBrush::Scrape,
            "Pinch" => VoxelBrush::Pinch,
            "Grab" | "Move" => VoxelBrush::Move,
            "Smudge" => VoxelBrush::Smudge,
            "Snake Hook" | "SnakeHook" => VoxelBrush::SnakeHook,
            _ => {
                // Not-ready tools should not reach here (palette dims them).
                return;
            }
        };
        if let Some(v) = self.sculpt.voxel.as_mut() {
            v.brush = brush;
            v.mask_paint = false;
        }
        self.sculpt.poly_brush = Some(brush);
        // Keep active_tool in sync with palette highlight keys.
        self.ui2.active_tool = match brush {
            VoxelBrush::ClayBuildup => "ClayBuildup".into(),
            VoxelBrush::SnakeHook => "SnakeHook".into(),
            VoxelBrush::Move => "Grab".into(),
            other => format!("{other:?}"),
        };
        // Debug-style names for Draw etc. become "Draw" via format! — good.
        // But Debug for Draw is "Draw" — OK. For Erase "Erase" — OK.
    }

    fn ui2_sculpt_reset_brush(&mut self) {
        if let Some(v) = self.sculpt.voxel.as_mut() {
            v.radius = 0.2;
            v.strength = 0.5;
            v.falloff = 0.0;
            v.falloff_preset = core_math::voxel_sculpt::FalloffPreset::Smooth;
            v.brush_add = true;
            v.mask_paint = false;
            self.toast("Brush reset (radius / strength / falloff)");
        } else {
            self.toast("No active voxel sculpt");
        }
    }

    fn ui2_sculpt_smooth_all(&mut self) {
        if self.sculpt.voxel.is_none() {
            self.toast("No active voxel sculpt");
            return;
        }
        self.voxel_smooth_all(crate::input_sculpt::SMOOTH_ALL_ITERS);
        self.toast("Smooth All");
    }

    fn ui2_sculpt_voxelize(&mut self) {
        if self.sculpt.voxel.is_none() {
            self.toast("No active voxel sculpt");
            return;
        }
        if self.voxel_flush_pending_move() {
            self.toast("Voxelized pending surface moves");
        } else {
            self.toast("Nothing to voxelize (no pending surface moves)");
        }
    }

    fn ui2_sculpt_mask_invert(&mut self) {
        if let Some(v) = self.sculpt.voxel.as_mut() {
            v.grid.invert_mask();
            self.mark_dirty(DirtyFlags::RENDER);
            self.toast("Mask inverted");
        } else {
            self.toast("No active voxel sculpt");
        }
    }

    fn ui2_sculpt_mask_clear(&mut self) {
        if let Some(v) = self.sculpt.voxel.as_mut() {
            v.grid.clear_mask();
            self.mark_dirty(DirtyFlags::RENDER);
            self.toast("Mask cleared");
        } else {
            self.toast("No active voxel sculpt");
        }
    }
}
