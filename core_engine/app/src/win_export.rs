// win_export.rs — SDF export window (OBJ, voxel, Dual Contouring mesh)
// Extracted from ui_windows.rs
//
// GATE 4 (DC consolidation): the SDF arm no longer runs its own CPU `mesh::dual_contouring` over
// the flat prim list (a SEPARATE algorithm — export never matched the viewport). It now goes
// through the shared materialize job (`mesh_presentation::materialize_sdf_body_mesh`): the same
// GPU DC output the display shows, per body, at the body's display resolution by default. The
// panel only QUEUES the request (`ui.export_pending`); the GPU work runs in poll
// (`process_export_pending` — Frame Ordering Contract), then the OBJ write happens on a thread.
use eframe::egui;
use crate::*;
use core_math::mesh;

impl AzraApp {
    // ================================================================
    // 7. Export Window (OBJ mesh export)
    // ================================================================
    pub(crate) fn draw_export_window(&mut self, ctx: &egui::Context) {
        if !self.ui.show_export_window { return; }
        let mut do_export = false;
        let mut do_browse = false;
        let export_toast: Option<String> = None;
        egui::Window::new("\u{1f53a} Export Mesh (OBJ)")
            .collapsible(false).resizable(false).default_width(380.0)
            .open(&mut self.ui.show_export_window).show(ctx, |ui| {

            ui.heading("Dual Contouring \u{2192} OBJ");
            ui.add_space(6.0);

            // GATE 4: default = the DISPLAY mesh at each body's display resolution
            // (export == what the viewport shows). Slider becomes a res OVERRIDE.
            ui.checkbox(&mut self.ui.export_display_res, "Use display resolution (match viewport)");

            // Resolution SLIDER (16-512) — override only
            ui.add_enabled_ui(!self.ui.export_display_res, |ui| {
                ui.label("Grid Resolution (override):");
                ui.add(egui::Slider::new(&mut self.ui.export_resolution, 16..=512)
                    .logarithmic(true)
                    .text("cells"));
            });

            // Quick-pick buttons underneath
            ui.horizontal(|ui| {
                ui.weak("Quick:");
                for &r in &[32u32, 64, 128, 256, 512] {
                    if ui.small_button(format!("{}", r)).clicked() {
                        self.ui.export_resolution = r;
                    }
                }
            });

            // Live estimate
            let res = self.ui.export_resolution as u64;
            let est_verts = res * res / 3; // conservative estimate for typical scenes
            let est_tris = est_verts * 2;
            let quality = if res <= 32 { "Draft" } else if res <= 64 { "Low" }
                else if res <= 128 { "Medium" } else if res <= 256 { "High" } else { "Ultra" };
            ui.weak(format!("{}\u{b3} cells \u{2014} est. ~{}k verts (~{}k tris) \u{2014} {}", res, est_verts/1000, est_tris/1000, quality));

            // Cell size indicator
            let (bmin, bmax) = mesh::compute_scene_bounds(&self.scene.primitives);
            let extent = bmax - bmin;
            let max_extent = extent.max_element();
            let cell_size = max_extent / self.ui.export_resolution as f32;
            ui.weak(format!("Cell size: {:.3} units (extent: {:.2}x{:.2}x{:.2})", cell_size, extent.x, extent.y, extent.z));
            ui.add_space(4.0);

            // GATE 4: same GPU DC as viewport, then offline Export Quality LOD fold
            // (soft-K / cylinder evenness + smooth normals). Live B2 is not used for export.
            ui.weak("GPU DC + export quality LOD (even spacing / soft-K bevel topo + normals).");
            ui.weak("Live Adaptive B2 does not affect this bake.");
            ui.add_space(6.0);

            // Save path
            ui.separator();
            ui.label("Save Location:");
            ui.horizontal(|ui| {
                ui.add(egui::TextEdit::singleline(&mut self.ui.export_path)
                    .desired_width(240.0)
                    .hint_text("azra_export.obj"));
                if ui.button("\u{1f4c2} Browse...").clicked() {
                    do_browse = true;
                }
            });
            if self.ui.export_path.is_empty() {
                ui.weak("Leave empty \u{2192} saves next to .exe");
            }
            ui.add_space(8.0);

            // Export buttons
            let has_sdf_prims = self.scene.primitives.iter().any(|p| p.visible);
            let has_poly = self.scene.scene_graph.poly_bodies().any(|(_, n)| n.visible);
            let has_anything = has_sdf_prims || has_poly;
            let is_exporting = self.ui.export_rx.is_some() || self.ui.export_pending.is_some();
            ui.horizontal(|ui| {
                ui.add_enabled_ui(has_anything && !is_exporting, |ui| {
                    let label = if is_exporting { "\u{23f3} Exporting\u{2026}" } else { "\u{1f4e6} Export OBJ" };
                    if ui.button(label).clicked() { do_export = true; }
                });
            });
            if is_exporting {
                ui.weak("Running in background \u{2014} UI stays responsive");
            } else if has_poly && !has_sdf_prims {
                ui.weak("Poly meshes will be exported directly (no DC needed).");
            } else if !has_anything {
                ui.weak("No visible primitives to export.");
            }

            // Last export status
            if let Some(stats) = self.ui.last_mesh_stats {
                ui.add_space(4.0);
                ui.label(format!("Last export: {} verts, {} faces", stats.0, stats.1));
            }
            if let Some(ref status) = self.ui.export_status {
                ui.colored_label(
                    if status.starts_with('\u{2705}') { egui::Color32::from_rgb(100, 220, 100) }
                    else { egui::Color32::from_rgb(255, 100, 100) },
                    status.as_str()
                );
            }
        });

        // Native file dialog (runs outside the window closure to avoid borrow issues)
        if do_browse {
            if let Some(path) = rfd::FileDialog::new()
                .set_title("Save OBJ Mesh")
                .add_filter("Wavefront OBJ", &["obj"])
                .set_file_name(&format!("azra_{}res.obj", self.ui.export_resolution))
                .save_file()
            {
                self.ui.export_path = path.to_string_lossy().to_string();
            }
        }

        if do_export && self.ui.export_rx.is_none() && self.ui.export_pending.is_none() {
            let filename = if self.ui.export_path.is_empty() {
                format!("azra_export_{}res.obj", self.ui.export_resolution)
            } else {
                self.ui.export_path.clone()
            };

            // Collect poly mesh data (must happen on main thread before spawn)
            let mut poly_objs: Vec<(String, Vec<[f32; 8]>, Vec<u32>)> = Vec::new();
            // Collect ids first: the export raster needs &mut self (it may evaluate the modifier
            // stack), which cannot be held while iterating the graph.
            let poly_ids: Vec<(core_math::scene_graph::NodeId, String)> = self
                .scene
                .scene_graph
                .poly_bodies()
                .filter(|(_, n)| n.visible)
                .filter_map(|(nid, n)| match &n.payload {
                    core_math::scene_graph::NodePayload::PolygonalBody { name, .. } => {
                        Some((nid, name.clone()))
                    }
                    _ => None,
                })
                .collect();
            for (nid, name) in poly_ids {
                {
                    // WHAT YOU SEE IS WHAT YOU EXPORT — modifier stack + authored normals.
                    let Some((verts, indices)) = self.poly_export_raster(nid) else { continue };
                    if !verts.is_empty() {
                        // Apply world transform to positions
                        let xform = self.scene.scene_graph.world_transform(nid);
                        let transformed: Vec<[f32; 8]> = verts.iter().map(|v| {
                            let p = xform * glam::Vec4::new(v[0], v[1], v[2], 1.0);
                            let n_mat = glam::Mat3::from_mat4(xform).inverse().transpose();
                            let n = n_mat * glam::Vec3::new(v[3], v[4], v[5]);
                            let nn = n.normalize_or_zero();
                            [p.x, p.y, p.z, nn.x, nn.y, nn.z, v[6], v[7]]
                        }).collect();
                        poly_objs.push((name.clone(), transformed, indices));
                    }
                }
            }

            // GATE 4: QUEUE the export — the SDF bodies are materialized in poll
            // (`process_export_pending`) through the shared materialize job, because GPU
            // buffer creation is banned during egui panel draws (Frame Ordering Contract).
            let res_override = if self.ui.export_display_res {
                None // per-body display resolution — export == display (the Gate-4 GO metric)
            } else {
                Some(self.ui.export_resolution)
            };
            self.ui.export_pending = Some(crate::mesh_presentation::PendingExport {
                filename, poly_objs, res_override,
            });
            self.ui.export_status = Some(format!("\u{23f3} Exporting\u{2026}"));
        }
        if let Some(msg) = export_toast { self.toast(&msg); }
    }
}
