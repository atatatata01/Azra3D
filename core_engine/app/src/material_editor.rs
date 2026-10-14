// core_engine/app/src/material_editor.rs
// ============================================================
// Unified PBR material editor widget
// ============================================================
//
// ONE shared widget for the base PBR surface (albedo tint, roughness, metallic) plus the
// albedo TEXTURE MAP row (load from file / clear / strength / Cube-Mapping toggle) and a
// one-click "Apply AI maps → material" button. Called from the SDF, voxel, (and future poly /
// nurbs) material panels — kills the 4× duplicated base-PBR block (Single Responsibility + Reuse).
//
// The widget is PURE UI: it mutates the borrowed `&mut PbrMaterial` for scalar fields and reports
// intent (load a file, apply AI maps, clear the map) back through `MaterialEditOutcome`. The caller
// owns transactions/undo and the deferred texture upload (Frame Ordering Contract) — this widget
// never touches the GPU.

use eframe::egui;
use glam::Vec3;
use core_math::primitives::{PbrMaterial, ShaderType};

// --- sRGB <-> linear helpers (local copy; the canonical ones in properties_sdf_material are private) ---
// pub(crate): also used by properties_material_node.rs (the graph-material editor).
fn srgb_to_linear(c: f32) -> f32 {
    if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
}
fn linear_to_srgb(c: f32) -> f32 {
    if c <= 0.0031308 { c * 12.92 } else { 1.055 * c.powf(1.0 / 2.4) - 0.055 }
}
pub(crate) fn linear_vec3_to_color32(v: Vec3) -> egui::Color32 {
    egui::Color32::from_rgb(
        (linear_to_srgb(v.x).clamp(0.0, 1.0) * 255.0) as u8,
        (linear_to_srgb(v.y).clamp(0.0, 1.0) * 255.0) as u8,
        (linear_to_srgb(v.z).clamp(0.0, 1.0) * 255.0) as u8,
    )
}
pub(crate) fn color32_to_linear_vec3(c: egui::Color32) -> Vec3 {
    Vec3::new(
        srgb_to_linear(c.r() as f32 / 255.0),
        srgb_to_linear(c.g() as f32 / 255.0),
        srgb_to_linear(c.b() as f32 / 255.0),
    )
}

/// What the caller must act on after the widget draws. Multiple may fire in one frame.
#[derive(Default)]
pub(crate) struct MaterialEditOutcome {
    /// A scalar field changed (albedo/rough/metal/strength/projection) — caller commits an undo step.
    pub(crate) changed: bool,
    /// A drag started this frame (begin an episode for slider drags).
    pub(crate) drag_started: bool,
    /// User clicked "Load albedo map…" → caller opens a file picker + queues a decode/upload.
    pub(crate) load_albedo_map: bool,
    /// User clicked Load/Replace on Normal / Roughness / Metalness (data maps).
    pub(crate) load_data_map: Option<core_math::pbr_material::MapKind>,
    /// User clicked "Apply AI maps" → caller routes `ai.ai_tex_maps_png[0]` (albedo) into the map.
    pub(crate) apply_ai_maps: bool,
    /// User cleared the albedo map → caller sets `albedo_map = TexMap::default()`.
    pub(crate) clear_albedo_map: bool,
    /// Short description for the undo entry of whatever changed this frame.
    pub(crate) desc: &'static str,
}

impl MaterialEditOutcome {
    /// Merge another outcome (e.g. base PBR + data-map slots drawn separately).
    pub(crate) fn merge(&mut self, other: MaterialEditOutcome) {
        self.changed |= other.changed;
        self.drag_started |= other.drag_started;
        self.load_albedo_map |= other.load_albedo_map;
        if other.load_data_map.is_some() {
            self.load_data_map = other.load_data_map;
        }
        self.apply_ai_maps |= other.apply_ai_maps;
        self.clear_albedo_map |= other.clear_albedo_map;
        if !other.desc.is_empty() && (other.changed || other.drag_started || other.load_data_map.is_some()) {
            self.desc = other.desc;
        }
    }
}

/// Draw the shared base-PBR + albedo-map editor for one material.
///
/// `ai_albedo_available` enables the "Apply AI maps" button (true when `ai.ai_tex_maps_png[0]`
/// holds a generated albedo PNG). The widget mutates `mat` in place for scalar edits and reports
/// map-related intents through the returned outcome.
///
/// `albedo_thumb`: thumbnail of the loaded albedo map (`AzraApp::map_slot_thumb`) — drawn as a
/// clickable 48px image (click = replace via the load flow). `None` (no GPU / dangling slot)
/// falls back to the text row.
///
/// `uv_allowed`: whether the UV projection option is selectable. SDF/voxel geometry carries no
/// UVs — UV there samples a single texel (honest choice per MATERIALS_UX §E4: the option is
/// DISABLED with an explanatory hover, not silently remapped).
pub(crate) fn draw_pbr_material(
    ui: &mut egui::Ui,
    mat: &mut PbrMaterial,
    ai_albedo_available: bool,
    albedo_thumb: Option<egui::TextureId>,
    uv_allowed: bool,
) -> MaterialEditOutcome {
    let mut out = MaterialEditOutcome::default();

    // --- Base color (albedo tint) ---
    ui.label(egui::RichText::new("Base Properties").strong().size(11.0));
    let mut col = linear_vec3_to_color32(mat.albedo);
    ui.horizontal(|ui| {
        ui.label("Base Color:");
        if ui.color_edit_button_srgba(&mut col).changed() {
            mat.albedo = color32_to_linear_vec3(col);
            out.drag_started = true; out.changed = true; out.desc = "Albedo";
        }
    });
    let r = ui.add(egui::Slider::new(&mut mat.roughness, 0.0..=1.0).text("Roughness"));
    if r.drag_started() { out.drag_started = true; }
    if r.changed() { out.changed = true; out.desc = "Roughness"; }
    let r = ui.add(egui::Slider::new(&mut mat.metallic, 0.0..=1.0).text("Metalness"));
    if r.drag_started() { out.drag_started = true; }
    if r.changed() { out.changed = true; out.desc = "Metalness"; }
    // Opacity = Master Material Opacity socket (same material). Realtime: deferred alpha mix.
    // True glass/SSS/caustics need the path-tracer evaluator (see draw_shader_mode_and_advanced).
    let r = ui
        .add(egui::Slider::new(&mut mat.opacity, 0.0..=1.0).text("Opacity"))
        .on_hover_text(
            "Realtime: soft transparency. Master Material Opacity is the same value. \
             Glass IOR / SSS / caustics are path-trace quality (preview only in Hybrid).",
        );
    if r.drag_started() { out.drag_started = true; }
    if r.changed() { out.changed = true; out.desc = "Opacity"; }

    // --- Albedo texture map row ---
    ui.add_space(4.0);
    ui.label(egui::RichText::new("Albedo Map").strong().size(11.0));
    ui.horizontal(|ui| {
        if mat.albedo_map.is_set() {
            // Owner ask (E4): every loaded map shows the actual IMAGE. Clickable = replace.
            if let Some(tid) = albedo_thumb {
                if ui.add(egui::ImageButton::new(
                        egui::load::SizedTexture::new(tid, egui::vec2(48.0, 48.0))))
                    .on_hover_text("Albedo map — click to replace")
                    .clicked()
                {
                    out.load_albedo_map = true;
                }
            } else {
                // No backing pixels (no GPU / dangling slot after a scene switch) — text fallback.
                ui.label(egui::RichText::new(format!("🎨 slot {}", mat.albedo_map.tex_id)).small());
            }
            if ui.button("Clear").on_hover_text("Remove the albedo map (scalar tint only)").clicked() {
                out.clear_albedo_map = true; out.changed = true; out.desc = "Clear albedo map";
            }
        } else {
            if ui.button("Load albedo map…").on_hover_text("Load an image as the albedo texture").clicked() {
                out.load_albedo_map = true;
            }
        }
    });

    if mat.albedo_map.is_set() {
        let r = ui.add(egui::Slider::new(&mut mat.albedo_map.strength, 0.0..=1.0).text("Map Strength"));
        if r.drag_started() { out.drag_started = true; }
        if r.changed() { out.changed = true; out.desc = "Map strength"; }
        let r = ui.add(egui::Slider::new(&mut mat.albedo_map.uv_scale, 0.05..=10.0).text("Tiling").logarithmic(true));
        if r.drag_started() { out.drag_started = true; }
        if r.changed() { out.changed = true; out.desc = "Map tiling"; }
        // Per-map projection (owner ask E4: each map owns its projection; Box/triplanar default).
        if draw_projection_combo(ui, ("albedo_projection", 0u32), &mut mat.albedo_map.projection, uv_allowed) {
            out.changed = true; out.desc = "Map projection";
        }
    }

    // --- AI maps one-click ---
    ui.add_space(2.0);
    ui.add_enabled_ui(ai_albedo_available, |ui| {
        if ui.button("✨ Apply AI maps → material")
            .on_hover_text("Route the AI-generated albedo map into this material")
            .clicked()
        {
            out.apply_ai_maps = true;
        }
    });
    if !ai_albedo_available {
        ui.label(egui::RichText::new("(generate a texture in the AI panel first)").small().weak());
    }

    out
}

/// Shader mode + advanced lobes (SSS / Glass / Clearcoat / Sheen).
///
/// **One material, two evaluators:** fields on `PbrMaterial` (Master / Properties). Hybrid packs
/// them into `RasterMaterial` (320 B) for a cheap G-buffer approx; full glass/SSS/caustics = PT.
///
/// `id_salt` disambiguates egui IDs when multiple materials are on screen.
pub(crate) fn draw_shader_mode_and_advanced(
    ui: &mut egui::Ui,
    mat: &mut PbrMaterial,
    id_salt: u64,
) -> MaterialEditOutcome {
    let mut out = MaterialEditOutcome::default();

    ui.add_space(4.0);
    ui.horizontal(|ui| {
        ui.label("Shader:");
        let st_label = match mat.shader_type {
            ShaderType::StandardPBR => "Standard PBR",
            ShaderType::Subsurface => "Subsurface",
            ShaderType::Glass => "Glass",
            ShaderType::VFX => "VFX/Unlit",
            ShaderType::Advanced => "Advanced",
        };
        egui::ComboBox::from_id_salt(("shader_type", id_salt))
            .width(120.0)
            .selected_text(st_label)
            .show_ui(ui, |ui| {
                for (st, lbl) in [
                    (ShaderType::StandardPBR, "Standard PBR"),
                    (ShaderType::Subsurface, "Subsurface"),
                    (ShaderType::Glass, "Glass"),
                    (ShaderType::VFX, "VFX/Unlit"),
                    (ShaderType::Advanced, "Advanced"),
                ] {
                    if ui.selectable_label(mat.shader_type == st, lbl).clicked() && mat.shader_type != st {
                        mat.shader_type = st;
                        out.drag_started = true;
                        out.changed = true;
                        out.desc = "Shader type";
                    }
                }
            });
    });
    ui.label(
        egui::RichText::new(
            "Realtime PBR ≈ interactive preview · Path Trace = ground truth (glass / SSS / caustics).",
        )
        .small()
        .weak()
        .italics(),
    );

    let stype = mat.shader_type;
    egui::CollapsingHeader::new(egui::RichText::new("Advanced Properties").size(11.0))
        .id_salt(("adv_props", id_salt))
        .default_open(matches!(
            stype,
            ShaderType::Subsurface | ShaderType::Glass | ShaderType::VFX | ShaderType::Advanced
        ))
        .show(ui, |ui| {
            let r = ui.add(egui::Slider::new(&mut mat.normal_strength, 0.0..=2.0).text("Normal Str"));
            if r.drag_started() {
                out.drag_started = true;
            }
            if r.changed() {
                out.changed = true;
                out.desc = "Normal strength";
            }

            // Subsurface (authored now; full random-walk SSS = path tracer)
            if matches!(stype, ShaderType::Subsurface | ShaderType::Advanced) {
                ui.add_space(2.0);
                ui.label(
                    egui::RichText::new("Subsurface")
                        .strong()
                        .size(10.0)
                        .color(egui::Color32::from_rgb(200, 120, 80)),
                );
                ui.label(
                    egui::RichText::new("Preview swatch + PT. Realtime Hybrid = approximate later.")
                        .small()
                        .weak(),
                );
                let r = ui.add(egui::Slider::new(&mut mat.sss_radius, 0.0..=5.0).text("SSS Radius"));
                if r.drag_started() {
                    out.drag_started = true;
                }
                if r.changed() {
                    out.changed = true;
                    out.desc = "SSS radius";
                }
                let mut sc = linear_vec3_to_color32(mat.sss_color);
                ui.horizontal(|ui| {
                    ui.label("SSS Color:");
                    if ui.color_edit_button_srgba(&mut sc).changed() {
                        mat.sss_color = color32_to_linear_vec3(sc);
                        out.drag_started = true;
                        out.changed = true;
                        out.desc = "SSS color";
                    }
                });
            }

            // Glass
            if matches!(stype, ShaderType::Glass | ShaderType::Advanced) {
                ui.add_space(2.0);
                ui.label(
                    egui::RichText::new("Glass")
                        .strong()
                        .size(10.0)
                        .color(egui::Color32::from_rgb(100, 180, 220)),
                );
                ui.label(
                    egui::RichText::new(
                        "Use Opacity for soft transparency now. IOR / refraction / caustics = path trace.",
                    )
                    .small()
                    .weak(),
                );
                let r = ui.add(egui::Slider::new(&mut mat.ior, 1.0..=3.0).text("IOR"));
                if r.drag_started() {
                    out.drag_started = true;
                }
                if r.changed() {
                    out.changed = true;
                    out.desc = "IOR";
                }
                let mut gc = linear_vec3_to_color32(mat.glass_tint);
                ui.horizontal(|ui| {
                    ui.label("Glass Tint:");
                    if ui.color_edit_button_srgba(&mut gc).changed() {
                        mat.glass_tint = color32_to_linear_vec3(gc);
                        out.drag_started = true;
                        out.changed = true;
                        out.desc = "Glass tint";
                    }
                });
            }

            // Clearcoat (all except pure VFX)
            if !matches!(stype, ShaderType::VFX) {
                ui.add_space(2.0);
                ui.label(
                    egui::RichText::new("Clearcoat")
                        .strong()
                        .size(10.0)
                        .color(egui::Color32::from_rgb(140, 180, 140)),
                );
                let r = ui.add(egui::Slider::new(&mut mat.clearcoat, 0.0..=1.0).text("Clearcoat"));
                if r.drag_started() {
                    out.drag_started = true;
                }
                if r.changed() {
                    out.changed = true;
                    out.desc = "Clearcoat";
                }
                let r = ui.add(
                    egui::Slider::new(&mut mat.clearcoat_roughness, 0.0..=1.0).text("CC Rough"),
                );
                if r.drag_started() {
                    out.drag_started = true;
                }
                if r.changed() {
                    out.changed = true;
                    out.desc = "CC roughness";
                }
            }

            // Anisotropy
            if matches!(stype, ShaderType::StandardPBR | ShaderType::Advanced) {
                ui.add_space(2.0);
                ui.label(
                    egui::RichText::new("Anisotropy")
                        .strong()
                        .size(10.0)
                        .color(egui::Color32::from_rgb(160, 140, 200)),
                );
                let r = ui.add(egui::Slider::new(&mut mat.anisotropy, -1.0..=1.0).text("Anisotropy"));
                if r.drag_started() {
                    out.drag_started = true;
                }
                if r.changed() {
                    out.changed = true;
                    out.desc = "Anisotropy";
                }
                let r = ui.add(
                    egui::Slider::new(&mut mat.anisotropy_rotation, 0.0..=1.0).text("Aniso Rot"),
                );
                if r.drag_started() {
                    out.drag_started = true;
                }
                if r.changed() {
                    out.changed = true;
                    out.desc = "Aniso rotation";
                }
            }

            // Sheen
            ui.add_space(2.0);
            ui.label(
                egui::RichText::new("Sheen")
                    .strong()
                    .size(10.0)
                    .color(egui::Color32::from_rgb(180, 150, 120)),
            );
            let r = ui.add(egui::Slider::new(&mut mat.sheen, 0.0..=1.0).text("Sheen"));
            if r.drag_started() {
                out.drag_started = true;
            }
            if r.changed() {
                out.changed = true;
                out.desc = "Sheen";
            }
            let r = ui.add(egui::Slider::new(&mut mat.sheen_tint, 0.0..=1.0).text("Sheen Tint"));
            if r.drag_started() {
                out.drag_started = true;
            }
            if r.changed() {
                out.changed = true;
                out.desc = "Sheen tint";
            }
        });

    out
}

/// Map slots shared by every material editor (SDF prim, poly body, graph material).
/// Normal · Roughness · Metalness · Emission · Bump · Displacement · SSS Color.
///
/// `thumbs`: optional 48px previews in `MapKind::DATA_SLOTS` order (resolved by the
/// caller via `map_slot_thumb` — this widget stays pure UI).
/// Clears and strength edits mutate `mat` in place; Load/Replace sets `out.load_data_map`.
pub(crate) fn draw_data_map_slots(
    ui: &mut egui::Ui,
    mat: &mut PbrMaterial,
    thumbs: [Option<egui::TextureId>; 7],
    uv_allowed: bool,
) -> MaterialEditOutcome {
    use core_math::pbr_material::MapKind;
    let mut out = MaterialEditOutcome::default();
    let kinds = MapKind::DATA_SLOTS;

    ui.add_space(4.0);
    ui.label(egui::RichText::new("Map Slots").strong().size(11.0));
    ui.label(
        egui::RichText::new(
            "Normal · Roughness · Metalness · Emission · Bump · Displacement · SSS Color",
        )
        .small()
        .weak(),
    );

    for (i, kind) in kinds.iter().enumerate() {
        let thumb = thumbs[i];
        let map = mat.map_mut(*kind);
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(kind.label()).small());
            if map.is_set() {
                if let Some(tid) = thumb {
                    if ui
                        .add(egui::ImageButton::new(egui::load::SizedTexture::new(
                            tid,
                            egui::vec2(48.0, 48.0),
                        )))
                        .on_hover_text(format!("{} map — click to replace", kind.label()))
                        .clicked()
                    {
                        out.load_data_map = Some(*kind);
                    }
                } else {
                    ui.label(
                        egui::RichText::new(format!("\u{1F5BC} slot {}", map.tex_id)).small(),
                    );
                    if ui.small_button("Replace\u{2026}").clicked() {
                        out.load_data_map = Some(*kind);
                    }
                }
                if ui.small_button("Clear").clicked() {
                    *map = Default::default();
                    out.changed = true;
                    out.desc = "Clear map";
                }
            } else if ui
                .small_button(format!("Load {}\u{2026}", kind.label()))
                .on_hover_text(match kind {
                    MapKind::Emission => "Load a color or grayscale emission map",
                    MapKind::Bump => "Load a height/bump map (grayscale) — perturbs shading normals",
                    MapKind::Displacement => {
                        "Load a displacement map (grayscale) — moves mesh vertices along the normal"
                    }
                    MapKind::Normal => "Load a tangent-space normal map",
                    MapKind::Roughness => "Load a grayscale roughness map",
                    MapKind::Metallic => "Load a grayscale metalness map",
                    MapKind::Albedo => "Load an albedo texture",
                    MapKind::SssColor => {
                        "Load a subsurface color map (sRGB) — replaces the SSS color by strength"
                    }
                })
                .clicked()
            {
                out.load_data_map = Some(*kind);
            }
        });
        if map.is_set() {
            // Displacement strength is world-space amplitude (allow >1 for large scenes).
            let max_s = if *kind == MapKind::Displacement { 2.0 } else { 1.0 };
            let r = ui.add(
                egui::Slider::new(&mut map.strength, 0.0..=max_s).text(match kind {
                    MapKind::Displacement => "Displacement Height",
                    MapKind::Bump => "Bump Strength",
                    MapKind::Emission => "Emission Map Strength",
                    _ => "Map Strength",
                }),
            );
            if r.drag_started() {
                out.drag_started = true;
            }
            if r.changed() {
                out.changed = true;
                out.desc = "Map strength";
            }
            // Normal maps are always triplanar (shader ignores projection — no tangent basis).
            // Bump uses screen-space derivatives so projection still affects height sampling.
            if *kind == MapKind::Normal {
                ui.label(
                    egui::RichText::new("Projection: Box (triplanar) — normal maps need no UVs")
                        .small()
                        .weak(),
                );
                // Green-channel convention. Auto-detected at import (filename / source layout);
                // manual override for unmarked sources — a wrong setting shows the bumps lit
                // inverted along one axis (grooves read as ridges).
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("Green channel:").small());
                    let mut dx = map.green_dx;
                    egui::ComboBox::from_id_salt(("nmap_green_convention", kind.wire_tag()))
                        .selected_text(if dx { "DirectX (Y\u{2212})" } else { "OpenGL (Y+)" })
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut dx, false, "OpenGL (Y+)").on_hover_text(
                                "ambientCG / Blender / glTF convention — green points up the image",
                            );
                            ui.selectable_value(&mut dx, true, "DirectX (Y\u{2212})").on_hover_text(
                                "Unreal / 3ds Max convention — green points down the image",
                            );
                        });
                    if dx != map.green_dx {
                        map.green_dx = dx;
                        out.changed = true;
                        out.desc = "Normal green channel";
                    }
                });
            } else if draw_projection_combo(
                ui,
                ("data_map_projection", kind.wire_tag()),
                &mut map.projection,
                uv_allowed,
            ) {
                out.changed = true;
                out.desc = "Map projection";
            }
        }
    }
    out
}

/// The ONE per-map projection combo (shared widget albedo row + the graph editor's
/// Normal/Roughness/Metallic rows + the "All maps" convenience). Box (triplanar) is the default
/// and always available; UV is gated by `uv_allowed` (SDF/voxel geometry has no UVs); Planar /
/// Sphere are the announced `MapProjection` extension points. Returns true when `proj` changed.
pub(crate) fn draw_projection_combo(
    ui: &mut egui::Ui,
    id_salt: impl std::hash::Hash,
    proj: &mut u32,
    uv_allowed: bool,
) -> bool {
    use core_math::pbr_material::MapProjection;
    let cur = MapProjection::from_u32(*proj);
    let mut sel = cur;
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Projection:").small());
        egui::ComboBox::from_id_salt(id_salt)
            .selected_text(sel.label())
            .show_ui(ui, |ui| {
                // Driven by MapProjection::ALL so a new variant cannot be left out of the
                // picker. (This list used to end in disabled "Planar (soon)" / "Sphere (soon)"
                // placeholders — they are implemented now and became real entries.)
                for proj in MapProjection::ALL {
                    if proj == MapProjection::Uv {
                        ui.add_enabled_ui(uv_allowed, |ui| {
                            ui.selectable_value(&mut sel, MapProjection::Uv, MapProjection::Uv.label())
                                .on_hover_text(if uv_allowed {
                                    "Sample by mesh UV coordinates"
                                } else {
                                    "Needs polygon UVs — SDF/voxel geometry has none (assign to a poly mesh to enable)"
                                });
                        });
                    } else {
                        ui.selectable_value(&mut sel, proj, proj.label())
                            .on_hover_text(match proj {
                                MapProjection::Box =>
                                    "Blends 3 axis-aligned samples by surface normal — the \
                                     general-purpose choice, no UVs needed",
                                MapProjection::Sphere =>
                                    "Longitude/latitude around the body origin — best on \
                                     ball-like shapes; pinches at the poles",
                                MapProjection::Cylinder =>
                                    "Wraps around the Y axis — best on tubes and columns; seams \
                                     at the top and bottom caps",
                                MapProjection::Planar =>
                                    "One flat projection down the body's Z axis — decal style; \
                                     stretches on faces parallel to it",
                                MapProjection::Uv => "",
                            });
                    }
                }
            });
    });
    if sel != cur { *proj = sel.as_u32(); true } else { false }
}

impl crate::AzraApp {
    /// Thumbnail of a texture-array map slot for UI display (`None` = no backing pixels — caller
    /// shows a text fallback, never indexes). `is_data` picks the decode that recovers the source
    /// image's bytes for normal/roughness/metallic maps (stored pre-encoded — see
    /// material_textures.rs). Cache-hit cheap; miss is O(THUMB_RES²) (safe in the draw path).
    pub(crate) fn map_slot_thumb(
        &mut self,
        ctx: &egui::Context,
        slot: u32,
        is_data: bool,
    ) -> Option<egui::TextureId> {
        let arr = &self.gpu.as_ref()?.material_textures;
        self.cache.map_thumb_cache.get_or_create(ctx, arr, slot, is_data)
    }
}

impl crate::AzraApp {
    /// Resolve an albedo-map edit outcome for an SDF prim (embedded material). Handles the file
    /// picker / AI-map load (deferred upload via `cache.material_map_loads`) and the immediate clear.
    /// The clear sets `pctx.changed` so the caller's existing transaction + mirror flow runs.
    pub(crate) fn handle_albedo_map_outcome(
        &mut self,
        outcome: &MaterialEditOutcome,
        idx: usize,
        prim_uid: u32,
        body_nid: core_math::scene_graph::NodeId,
        pctx: &mut crate::properties_sdf::PropsContext,
    ) {
        use crate::material_textures::{MapTarget, PendingMapLoad};

        if outcome.clear_albedo_map {
            if idx < self.scene.primitives.len() {
                self.scene.primitives[idx].material.albedo_map = Default::default();
                pctx.changed = true;
                if pctx.desc.is_empty() { pctx.desc = "Clear albedo map".into(); }
            }
            return;
        }

        if outcome.load_albedo_map {
            // Deferred dialog (file_dialog.rs — rfd modal-in-frame crash-class fix):
            // picker + decode run on a worker thread; the PendingMapLoad is queued in poll.
            self.request_map_file_dialog(
                core_math::pbr_material::MapKind::Albedo,
                MapTarget::Prim(body_nid, prim_uid),
            );
        }

        // Normal / Roughness / Metalness maps (same deferred picker path as albedo).
        if let Some(kind) = outcome.load_data_map {
            self.request_map_file_dialog(kind, MapTarget::Prim(body_nid, prim_uid));
        }

        if outcome.apply_ai_maps {
            // ai_tex_maps_png[0] = albedo PNG bytes (see ai_texture.rs map ordering).
            let albedo_png = self.ai.ai_tex_maps_png[0].clone();
            if !albedo_png.is_empty() {
                if let Some(pixels) = crate::material_textures::decode_to_map(&albedo_png) {
                    self.cache.material_map_loads.push(PendingMapLoad {
                        target: MapTarget::Prim(body_nid, prim_uid),
                        kind: core_math::pbr_material::MapKind::Albedo,
                        pixels,
                        strength: 1.0,
                        name: "AI Albedo".into(),
                        high_precision: false,
                        pixels_lo: Vec::new(),
                        green_dx: false,
                    });
                }
            }
        }
    }

    /// Body-material (voxel / poly / nurbs) variant of the albedo-map outcome handler. The clear is
    /// applied by the caller's own write-back (the snapshot `mat` already had it zeroed); here we only
    /// queue the deferred file/AI upload, targeting the body node's embedded material.
    pub(crate) fn handle_body_albedo_map_outcome(
        &mut self,
        outcome: &MaterialEditOutcome,
        body_nid: core_math::scene_graph::NodeId,
    ) {
        use crate::material_textures::{MapTarget, PendingMapLoad};
        if outcome.load_albedo_map {
            // Deferred dialog (file_dialog.rs — rfd modal-in-frame crash-class fix).
            self.request_map_file_dialog(
                core_math::pbr_material::MapKind::Albedo,
                MapTarget::BodyMaterial(body_nid),
            );
        }
        if let Some(kind) = outcome.load_data_map {
            self.request_map_file_dialog(kind, MapTarget::BodyMaterial(body_nid));
        }
        if outcome.apply_ai_maps {
            let albedo_png = self.ai.ai_tex_maps_png[0].clone();
            if !albedo_png.is_empty() {
                if let Some(pixels) = crate::material_textures::decode_to_map(&albedo_png) {
                    self.cache.material_map_loads.push(PendingMapLoad {
                        target: MapTarget::BodyMaterial(body_nid),
                        kind: core_math::pbr_material::MapKind::Albedo,
                        pixels,
                        strength: 1.0,
                        name: "AI Albedo".into(),
                        high_precision: false,
                        pixels_lo: Vec::new(),
                        green_dx: false,
                    });
                }
            }
        }
    }

    /// Write a freshly-uploaded texture-array `slot` into a material's map channel (called from
    /// poll, one frame after the load was queued). `kind` picks the channel (`PbrMaterial::map_mut`
    /// — albedo today for embedded targets, all four for graph materials). Returns true if a
    /// material was updated. Mirrors the prim to the graph for the embedded-prim case so the GPU
    /// prim buffer picks it up.
    pub(crate) fn apply_map_slot(
        &mut self,
        target: crate::material_textures::MapTarget,
        kind: core_math::pbr_material::MapKind,
        slot: u32,
        strength: f32,
    ) -> bool {
        self.apply_map_slot_ex(target, kind, slot, strength, false, false)
    }

    /// Like [`apply_map_slot`], with the 16-bit RG-pack flag for height maps and the
    /// normal-map green-channel convention (`green_dx` — ignored by non-Normal kinds).
    pub(crate) fn apply_map_slot_ex(
        &mut self,
        target: crate::material_textures::MapTarget,
        kind: core_math::pbr_material::MapKind,
        slot: u32,
        strength: f32,
        high_precision: bool,
        green_dx: bool,
    ) -> bool {
        self.apply_map_slot_precision(target, kind, slot, strength, high_precision, green_dx, false)
    }

    pub(crate) fn apply_map_slot_precision(
        &mut self,
        target: crate::material_textures::MapTarget,
        kind: core_math::pbr_material::MapKind,
        slot: u32,
        strength: f32,
        high_precision: bool,
        green_dx: bool,
        raw_precision: bool,
    ) -> bool {
        use crate::material_textures::MapTarget;
        use core_math::scene_graph::NodePayload;
        match target {
            MapTarget::Graph(mid) => {
                let has_graph = self.ui.material_node_graphs.contains_key(&mid.0);
                if let Some(m) = self.scene.scene_graph.material_by_id_mut(mid) {
                    let map = m.map_mut(kind);
                    map.tex_id = slot;
                    if !(has_graph && matches!(kind, core_math::pbr_material::MapKind::Bump)) {
                        map.strength = strength;
                    }
                    map.high_precision = high_precision;
                    map.green_dx = green_dx;
                    map.raw_precision = high_precision && raw_precision;
                    // Graph / panel layered bake is an absolute Mult-stack composite — display
                    // with REPLACE so base×noise1×noise2×… is not tint-multiplied again.
                    // Do NOT reset uv_scale / projection here — Mult enqueue already set them
                    // from panel Texture Space so Image tiling survives when Noise is added.
                    if has_graph && matches!(kind, core_math::pbr_material::MapKind::Albedo) {
                        map.absolute = true;
                    }
                    // Absolute Mult-stack bake owns albedo — ALWAYS kill GPU live-noise
                    // (gbuffer does mix(base, noise_color, noise) = REPLACE, not Mult). Leaving
                    // noise_pattern on after a Mult bake made "Mult noise" look like pure B&W
                    // noise (image wiped) or pure map (if live later cleared without Mult composite).
                    // Simple Noise→Albedo live path re-enables via apply_live_gpu_noise when needed.
                    if has_graph && matches!(kind, core_math::pbr_material::MapKind::Albedo) {
                        m.noise_pattern.enabled = false;
                        m.noise_pattern.amount = 0.0;
                        m.albedo = glam::Vec3::ONE;
                    }
                    // Material-panel P3: a per-layer Load… claims this slot id
                    // (writes it into the pending layer + re-lowers the stack).
                    self.mp_apply_layer_load(mid, kind, slot);
                    return true;
                }
                // Quiet diagnostic (kept): a bake landing on a MISSING material node is a real
                // wiring bug — log it, never silently drop the maps.
                let existing: Vec<u32> = self.scene.scene_graph.materials().map(|(_, id, _, _)| id.0).collect();
                eprintln!("[MaterialMap] bake result for {:?} {:?} DROPPED — material not in graph (existing ids: {:?})", mid, kind, existing);
                false
            }
            MapTarget::Prim(_body_nid, prim_uid) => {
                if let Some(idx) = self.prim_index_by_uid(prim_uid) {
                    {
                        let map = self.scene.primitives[idx].material.map_mut(kind);
                        map.tex_id = slot;
                        map.strength = strength;
                        map.high_precision = high_precision;
                        map.green_dx = green_dx;
                        map.raw_precision = high_precision && raw_precision;
                    }
                    // Mirror flat cache → graph so the prim buffer + volume preview see the map.
                    self.mirror_prim_to_graph(idx);
                    return true;
                }
                false
            }
            MapTarget::BodyMaterial(nid) => {
                if let Some(node) = self.scene.scene_graph.get_mut(nid) {
                    let mat = match &mut node.payload {
                        NodePayload::VoxelBody { material, .. } => Some(material),
                        NodePayload::PolygonalBody { material, .. } => Some(material),
                        NodePayload::NurbsBody { material, .. } => Some(material),
                        _ => None,
                    };
                    if let Some(mat) = mat {
                        let map = mat.map_mut(kind);
                        map.tex_id = slot;
                        map.strength = strength;
                        map.high_precision = high_precision;
                        map.green_dx = green_dx;
                        map.raw_precision = high_precision && raw_precision;
                        return true;
                    }
                }
                false
            }
        }
    }
}
