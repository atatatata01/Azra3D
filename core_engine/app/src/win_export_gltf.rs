// win_export_gltf.rs — glTF/FBX poly export window
// Extracted from ui_windows.rs
use eframe::egui;
use crate::*;

impl AzraApp {
    // ================================================================
    // 7b. glTF Export Window (all scene meshes)
    // ================================================================
    pub(crate) fn draw_gltf_export_window(&mut self, ctx: &egui::Context) {
        if !self.ui.show_gltf_export { return; }
        let mut do_export = false;
        let mut do_browse = false;

        egui::Window::new("\u{1f4e6} Export glTF (.glb)")
            .collapsible(false).resizable(false).default_width(380.0)
            .open(&mut self.ui.show_gltf_export).show(ctx, |ui| {

            ui.heading("Scene \u{2192} glTF 2.0 Binary");
            ui.add_space(4.0);

            // Count exportable meshes
            let poly_count = self.scene.scene_graph.poly_bodies()
                .filter(|(_, n)| n.visible).count();
            let nurbs_count = self.scene.scene_graph.nurbs_bodies()
                .filter(|(_, n)| n.visible).count();
            let sdf_count = self.scene.primitives.iter().filter(|p| p.visible).count();

            ui.label(format!("Poly bodies: {}", poly_count));
            ui.label(format!("NURBS bodies: {}", nurbs_count));
            let terrain_tiles = self.terrain_world.tiles.len();
            if terrain_tiles > 0 {
                ui.label(format!("Terrain tiles: {} (Shift+A → Export Terrain Tiles)", terrain_tiles));
            }
            if sdf_count > 0 {
                ui.weak(format!("SDF primitives: {} (use OBJ export for DC mesh)", sdf_count));
            }
            let total = poly_count + nurbs_count;

            ui.add_space(6.0);
            ui.separator();
            ui.label("Save Location:");
            ui.horizontal(|ui| {
                ui.add(egui::TextEdit::singleline(&mut self.ui.gltf_export_path)
                    .desired_width(240.0)
                    .hint_text("azra_scene.glb"));
                if ui.button("\u{1f4c2} Browse...").clicked() {
                    do_browse = true;
                }
            });
            if self.ui.gltf_export_path.is_empty() {
                ui.weak("Leave empty \u{2192} saves next to .exe");
            }
            ui.add_space(8.0);

            ui.add_enabled_ui(total > 0, |ui| {
                if ui.button("\u{1f4e6} Export glTF").clicked() { do_export = true; }
            });
            if total == 0 {
                ui.weak("No poly/NURBS/imported meshes to export.");
            }

            if let Some(ref status) = self.ui.gltf_export_status {
                ui.add_space(4.0);
                ui.colored_label(
                    if status.starts_with('\u{2705}') { egui::Color32::from_rgb(100, 220, 100) }
                    else { egui::Color32::from_rgb(255, 100, 100) },
                    status.as_str()
                );
            }
        });

        if do_browse {
            if let Some(path) = rfd::FileDialog::new()
                .set_title("Save glTF Binary")
                .add_filter("glTF Binary", &["glb"])
                .set_file_name("azra_scene.glb")
                .save_file()
            {
                self.ui.gltf_export_path = path.to_string_lossy().to_string();
            }
        }

        if do_export {
            use core_math::scene_graph::NodePayload;
            use io_bridge::gltf_export::{ExportMesh, ExportMaterial, export_glb};

            let filename = if self.ui.gltf_export_path.is_empty() {
                "azra_scene.glb".to_string()
            } else {
                self.ui.gltf_export_path.clone()
            };

            let mut export_meshes = Vec::new();

            // Collect poly bodies
            let poly_nodes: Vec<_> = self.scene.scene_graph.poly_bodies()
                .filter(|(_, n)| n.visible)
                .map(|(nid, _)| nid)
                .collect();
            for nid in poly_nodes {
                // WHAT YOU SEE IS WHAT YOU EXPORT — modifier stack + authored normals. Taken
                // BEFORE the graph borrow below, since it may evaluate the stack (&mut self).
                let Some((raster_verts, indices)) = self.poly_export_raster(nid) else { continue };
                if let Some(node) = self.scene.scene_graph.get(nid) {
                    if let NodePayload::PolygonalBody { name, material, .. } = &node.payload {
                        let positions: Vec<glam::Vec3> = raster_verts.iter()
                            .map(|v| glam::Vec3::new(v[0], v[1], v[2])).collect();
                        let normals: Vec<glam::Vec3> = raster_verts.iter()
                            .map(|v| glam::Vec3::new(v[3], v[4], v[5])).collect();
                        let uvs: Vec<[f32; 2]> = raster_verts.iter()
                            .map(|v| [v[6], v[7]]).collect();
                        export_meshes.push(ExportMesh {
                            name: name.clone(),
                            positions,
                            normals,
                            uvs,
                            indices,
                            material: ExportMaterial {
                                name: name.clone(),
                                albedo: [material.albedo.x, material.albedo.y, material.albedo.z, material.opacity],
                                roughness: material.roughness,
                                metallic: material.metallic,
                                emission: [
                                    material.emission_color.x * material.emission_intensity,
                                    material.emission_color.y * material.emission_intensity,
                                    material.emission_color.z * material.emission_intensity,
                                ],
                            },
                        });
                    }
                }
            }

            // Collect NURBS bodies (tessellated mesh)
            let nurbs_nodes: Vec<_> = self.scene.scene_graph.nurbs_bodies()
                .filter(|(_, n)| n.visible)
                .map(|(nid, _)| nid)
                .collect();
            for nid in nurbs_nodes {
                if let Some(node) = self.scene.scene_graph.get(nid) {
                    if let NodePayload::NurbsBody { name, surface, material, .. } = &node.payload {
                        let (raster_verts, indices) = surface.tessellate();
                        if raster_verts.is_empty() { continue; }
                        let positions: Vec<glam::Vec3> = raster_verts.iter()
                            .map(|v| glam::Vec3::new(v[0], v[1], v[2])).collect();
                        let normals: Vec<glam::Vec3> = raster_verts.iter()
                            .map(|v| glam::Vec3::new(v[3], v[4], v[5])).collect();
                        let uvs: Vec<[f32; 2]> = raster_verts.iter()
                            .map(|v| [v[6], v[7]]).collect();
                        export_meshes.push(ExportMesh {
                            name: name.clone(),
                            positions,
                            normals,
                            uvs,
                            indices,
                            material: ExportMaterial {
                                name: name.clone(),
                                albedo: [material.albedo.x, material.albedo.y, material.albedo.z, material.opacity],
                                roughness: material.roughness,
                                metallic: material.metallic,
                                emission: [
                                    material.emission_color.x * material.emission_intensity,
                                    material.emission_color.y * material.emission_intensity,
                                    material.emission_color.z * material.emission_intensity,
                                ],
                            },
                        });
                    }
                }
            }

            if export_meshes.is_empty() {
                self.ui.gltf_export_status = Some("\u{274c} No meshes to export".into());
            } else {
                match export_glb(&filename, &export_meshes) {
                    Ok((verts, faces)) => {
                        self.ui.gltf_export_status = Some(format!(
                            "\u{2705} Exported {} mesh(es): {} verts, {} tris \u{2192} {}",
                            export_meshes.len(), verts, faces, filename
                        ));
                    }
                    Err(e) => {
                        self.ui.gltf_export_status = Some(format!("\u{274c} {}", e));
                    }
                }
            }
        }
    }
}
