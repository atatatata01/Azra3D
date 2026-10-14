// Placement / library drop / Shift+A create helpers.
use eframe::egui;
use crate::*;

impl FourthLayerApp {
    /// Mockup Shift+A add popup — disabled. Shift+A uses legacy `draw_add_popup` only
    /// (owner: the stacked dual-menu was wrong; keep the lower/legacy one).
    #[allow(dead_code)]
    pub(crate) fn ui2_add_menu(&mut self, _ctx: &egui::Context) {
        self.ui2.add_menu_open = false;
    }

    /// Screen → world under the drop/cursor: workplane, else scene surface, else Y=0 ground.
    pub(crate) fn ui2_world_under_cursor(&self, screen: egui::Pos2) -> glam::Vec3 {
        let rect = self.viewport.viewport_rect;
        if rect.width() < 4.0 || rect.height() < 4.0 {
            return glam::Vec3::ZERO;
        }
        if let Some(p) = self.ray_workplane_hit(screen, rect) {
            return p;
        }
        let (o, d) = self.cam_ray(screen, rect);
        if let Some((p, _)) = self.raycast_scene_surface(o, d) {
            return p;
        }
        self.ray_plane_hit(screen, rect, glam::Vec3::ZERO, glam::Vec3::Y)
            .unwrap_or(glam::Vec3::ZERO)
    }

    /// Move the just-created poly body (active node) to `world` if it has a node transform.
    pub(crate) fn ui2_place_active_node(&mut self, world: glam::Vec3) {
        if let Some(nid) = self.sel.usel.active_node() {
            if let Some(n) = self.scene.scene_graph.get_mut(nid) {
                n.local_position = world;
                self.mark_dirty(DirtyFlags::SCENE | DirtyFlags::RENDER);
            }
        }
    }

    /// Gate 7: apply a real library asset dropped onto the viewport
    /// (material / texture → object under drop cursor, HDRI → env, MatCap → viewport matcap).
    ///
    /// Must pick at `drop.screen_pos` — `tool.hovered_prim` is always `None` during a library
    /// drag because the viewport widget is not hovered while the pointer owns the library drag.
    pub(crate) fn ui2_library_place(
        &mut self,
        drop: &crate::library_panel::LibDrop,
        ctx: &egui::Context,
    ) {
        if drop.item_idx >= self.library.items.len() {
            self.toast("Library item missing — rescan assets");
            return;
        }
        match drop.category {
            crate::ContentCategory::Materials | crate::ContentCategory::Textures => {
                let is_folder_mat = drop.category == crate::ContentCategory::Materials
                    && self
                        .library
                        .items
                        .get(drop.item_idx)
                        .map_or(false, |i| !i.is_individual_file);
                if is_folder_mat {
                    // Full PBR graph material on the body under the drop point (raycast + GPU pick).
                    self.drop_library_material_at(drop.item_idx, drop.screen_pos, ctx);
                    return;
                }
                // Individual texture / legacy embedded apply: prim under cursor, else selection.
                if let Some(prim_idx) = self
                    .prim_under_screen(drop.screen_pos)
                    .or_else(|| self.active_prim_index_unified())
                {
                    self.apply_library_material(drop.item_idx, prim_idx);
                } else {
                    self.toast("Drop on an object (or select one) to apply material");
                }
            }
            crate::ContentCategory::HDRIs => {
                self.apply_library_hdri(drop.item_idx);
            }
            crate::ContentCategory::MatCaps => {
                self.apply_library_matcap(drop.item_idx);
            }
            crate::ContentCategory::Bodies => {
                let world = self.ui2_world_under_cursor(drop.screen_pos);
                let _ = self.insert_library_body_item(drop.item_idx, Some(world));
            }
        }
    }

    /// Execute a Shift+A menu choice through the real creation APIs.
    /// Placement: workplane center when locked, else world origin (owner rule).
    pub(crate) fn ui2_add_create(&mut self, key: &str) {
        let at = crate::ops::sdf::add_spawn_point(self);
        match key {
            // SDF — ops::* (node-ready)
            "sdf:sphere" => {
                crate::ops::sdf::sphere::run(self, at);
                self.ui2.active_pillar = "sdf".into();
                self.ui2.active_tool = "Sphere".into();
            }
            "sdf:box" => {
                crate::ops::sdf::box_shape::run(self, at);
                self.ui2.active_pillar = "sdf".into();
                self.ui2.active_tool = "Box".into();
            }
            "sdf:cylinder" => {
                crate::ops::sdf::cylinder::run(self, at);
                self.ui2.active_pillar = "sdf".into();
                self.ui2.active_tool = "Cylinder".into();
            }
            "sdf:torus" => {
                crate::ops::sdf::torus::run(self, at);
                self.ui2.active_pillar = "sdf".into();
                self.ui2.active_tool = "Torus".into();
            }
            "sdf:cone" => {
                crate::ops::sdf::cone::run(self, at);
                self.ui2.active_pillar = "sdf".into();
                self.ui2.active_tool = "Cone".into();
            }
            "sdf:capsule" => {
                crate::ops::sdf::capsule::run(self, at);
                self.ui2.active_pillar = "sdf".into();
                self.ui2.active_tool = "Capsule".into();
            }
            // Poly meshes — ops::poly::mesh_create
            "poly:box" => {
                crate::ops::poly::mesh_create::box_mesh(self, at);
                self.ui2.active_pillar = "poly".into();
                self.ui2.active_tool = "Cube".into();
            }
            "poly:plane" => {
                crate::ops::poly::mesh_create::plane(self, at);
                self.ui2.active_pillar = "poly".into();
                self.ui2.active_tool = "Plane".into();
            }
            "poly:circle" => {
                crate::ops::poly::mesh_create::circle(self, at);
                self.ui2.active_pillar = "poly".into();
                self.ui2.active_tool = "Circle".into();
            }
            "poly:sphere" => {
                crate::ops::poly::mesh_create::sphere(self, at);
                self.ui2.active_pillar = "poly".into();
                self.ui2.active_tool = "Sphere".into();
            }
            "poly:icosphere" => {
                crate::ops::poly::mesh_create::ico_sphere(self, at);
                self.ui2.active_pillar = "poly".into();
                self.ui2.active_tool = "Ico Sphere".into();
            }
            "poly:cylinder" => {
                crate::ops::poly::mesh_create::cylinder(self, at);
                self.ui2.active_pillar = "poly".into();
                self.ui2.active_tool = "Cylinder".into();
            }
            "poly:cone" => {
                crate::ops::poly::mesh_create::cone(self, at);
                self.ui2.active_pillar = "poly".into();
                self.ui2.active_tool = "Cone".into();
            }
            "poly:torus" => {
                crate::ops::poly::mesh_create::torus(self, at);
                self.ui2.active_pillar = "poly".into();
                self.ui2.active_tool = "Torus".into();
            }
            // ── Lights ──
            "light:dir" => { self.add_light(LightType::Directional); }
            "light:point" => { self.add_light(LightType::Point); }
            "light:spot" => { self.add_light(LightType::Spot); }
            // ── Empty / body ──
            "empty" | "sdf_body" => {
                self.create_new_sdf_body();
            }
            _ => {
                self.toast(&format!("Unknown add item: {key}"));
            }
        }
    }
}
