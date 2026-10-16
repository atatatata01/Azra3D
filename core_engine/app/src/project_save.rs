//! Project save — from_app() conversion + save_scene / save_scene_as / check_auto_save.

use crate::AzraApp;
use super::project::*;

fn scene_imported_meshes(
    graph: &core_math::scene_graph::SceneGraph,
    cache: &render::mesh_cache::MeshCache,
) -> Vec<ImportedMeshRecord> {
    let referenced: std::collections::HashSet<_> = graph.referenced_mesh_cache_ids().collect();
    cache.imported_data().filter(|(id, _, _, _)| referenced.contains(id))
        .map(|(id, vertices, indices, triangle_count)| ImportedMeshRecord {
            cache_id: id.0, vertices: vertices.clone(), indices: indices.clone(), triangle_count,
        }).collect()
}

#[cfg(test)]
mod mesh_persistence_tests {
    use super::*;

    #[test]
    #[ignore = "Requires AZRA_INSPECT_PROJECT pointing to a saved scene"]
    fn inspect_saved_material_assignment() {
        let path = std::env::var("AZRA_INSPECT_PROJECT").expect("saved scene path is required");
        let project = load_binary(std::path::Path::new(&path)).unwrap();
        eprintln!("PROJECT {path}: version={} shading={:?} pt_bounces={} saved_graphs={} saved_channels={}",
            project.version, project.render.shading_mode, project.render.pt_max_bounces,
            project.node_graphs.len(), project.material_channels.len());
        if let Some(graph) = project.scene_graph.as_ref() {
            for (node, material, name, data) in graph.materials() {
                eprintln!("MATERIAL node={} id={} name={name:?} references={:?} albedo_slot={}",
                    node.0, material.0, graph.material_use_counts(material), data.albedo_map.tex_id);
            }
            for (node, value) in graph.all_nodes() {
                match &value.payload {
                    core_math::scene_graph::NodePayload::SdfBody { primitives, .. } => {
                        for primitive in primitives {
                            eprintln!("SDF body={} prim={} material={}", node.0, primitive.prim_uid, primitive.material_id.0);
                        }
                    }
                    core_math::scene_graph::NodePayload::PolygonalBody { mesh, .. } => {
                        let mut materials = std::collections::BTreeMap::new();
                        for face in &mesh.faces {
                            if face.half_edge != core_math::half_edge::NO_EDGE {
                                *materials.entry(face.material_id).or_insert(0usize) += 1;
                            }
                        }
                        eprintln!("POLYGON node={} faces={} materials={materials:?}", node.0, mesh.faces.len());
                    }
                    _ => {}
                }
            }
        }
        for record in &project.node_graphs {
            let (owner, graph) = match serde_json::from_str::<MaterialGraphEnvelope>(&record.graph_json) {
                Ok(envelope) => (format!("material {}", envelope.material_id), envelope.graph),
                Err(_) => (format!("primitive {}", record.prim_uid), serde_json::from_str::<crate::shader_nodes::NodeGraph>(&record.graph_json).unwrap()),
            };
            let nodes: Vec<_> = graph.nodes.iter().map(|node| (node.id, &node.node_type)).collect();
            eprintln!("GRAPH owner={owner} nodes={nodes:?} connections={:?}", graph.connections);
        }
    }

    #[test]
    fn runtime_preview_meshes_never_enter_saved_mesh_records() {
        let mut graph = core_math::scene_graph::SceneGraph::new();
        let mut cache = render::mesh_cache::MeshCache::new();
        let source = cache.alloc_id();
        let preview = cache.alloc_id();
        let vertices = vec![[0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0],
            [1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 1.0, 0.0],
            [0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 1.0]];
        let indices = vec![0, 1, 2];
        cache.insert_imported(source, (vertices.clone(), indices.clone()), 1);
        cache.insert_imported(preview, (vertices.clone(), indices.clone()), 1);
        let root = graph.root();
        for name in ["original", "shared instance"] {
            graph.add_child(root, core_math::scene_graph::NodePayload::ImportedMesh {
                name: name.into(), mesh_cache_id: source, materials: Vec::new(),
            });
        }
        for _ in 0..3 {
            let records = scene_imported_meshes(&graph, &cache);
            assert_eq!(records.len(), 1);
            assert_eq!(records[0].cache_id, source.0);
            assert_eq!(records[0].vertices, vertices);
            assert_eq!(records[0].indices, indices);
            let encoded = bincode::serialize(&records).unwrap();
            let restored: Vec<ImportedMeshRecord> = bincode::deserialize(&encoded).unwrap();
            assert_eq!(restored.len(), 1);
            assert_ne!(restored[0].cache_id, preview.0);
            let graph_bytes = bincode::serialize(&graph).unwrap();
            graph = bincode::deserialize(&graph_bytes).unwrap();
            cache = render::mesh_cache::MeshCache::new();
            for record in restored {
                cache.insert_imported(
                    core_math::scene_graph::MeshCacheId(record.cache_id),
                    (record.vertices, record.indices),
                    record.triangle_count,
                );
            }
            cache.insert_imported(preview, (vertices.clone(), indices.clone()), 1);
        }
    }
}

impl ProjectFile {
    pub(crate) fn from_app(app: &AzraApp) -> Result<Self, String> {
        let material_slots = app.gpu.as_ref().map(|gpu| {
            gpu.material_textures.export_slots(&gpu.device, &gpu.queue)
        }).transpose()?.unwrap_or_default();
        let euler = app.viewport.camera.orbit.to_euler(glam::EulerRot::YXZ);
        // Collect imported mesh data for persistence
        let imported_meshes = scene_imported_meshes(&app.scene.scene_graph, &app.cache.mesh_cache);
        Ok(Self {
            version: 7,
            scene: SceneData {
                primitives: app.scene.primitives.clone(),
                layers: vec![],
                objects: vec![],
                lights: app.scene.scene_graph.lights().map(|(_, l)| LightData {
                    id: l.id, name: l.name.clone(), light_type: l.light_type,
                    azimuth: l.azimuth, elevation: l.elevation,
                    intensity: l.intensity, color: l.color, ambient: l.ambient,
                    ambient_color: l.ambient_color, ambient_from_hdr: l.ambient_from_hdr,
                    position: l.position.to_array(), range: l.range,
                    spot_inner_deg: l.spot_inner_deg, spot_outer_deg: l.spot_outer_deg,
                    shadow_softness: l.shadow_softness, visible: l.visible, locked: l.locked,
                    link_mode: l.link_mode.clone(), link_targets: l.link_targets.clone(),
                    light_group: l.light_group.clone(),
                }).collect(),
                next_prim_id: app.scene.next_prim_id,
                next_layer_id: 1,
                next_object_id: 1,
                next_light_id: app.scene.next_light_id,  // Persisted for ID continuity
                next_node_name_id: app.scene.next_node_name_id,
            },
            camera: CameraState {
                yaw: euler.0, pitch: euler.1, roll: euler.2,
                distance: app.viewport.camera.distance, target: app.viewport.camera.target.to_array(),
                ortho: app.render.ortho,
            },
            render: RenderSettings {
                shading_mode: app.render.shading_mode,
                resolution_scale: app.render.resolution_scale,
                render_quality: app.render.render_quality,
                exposure: app.render.exposure,
                bloom_enabled: app.render.bloom_enabled,
                bloom_threshold: app.render.bloom_threshold,
                bloom_intensity: app.render.bloom_intensity,
                ssr_enabled: app.render.ssr_enabled,
                fog_enabled: app.render.fog_enabled,
                fog_density: app.render.fog_density,
                fog_height_falloff: app.render.fog_height_falloff,
                fog_scatter: app.render.fog_scatter,
                fog_color: app.render.fog_color,
                upsample_enabled: app.render.upsample_enabled,
                rt_bounces: app.render.rt_bounces,
                rt_shadow_samples: app.render.rt_shadow_samples,
                pt_max_bounces: app.render.pt_max_bounces,
                pt_samples_per_frame: app.render.pt_samples_per_frame,
                bg_color_top: app.render.bg_color_top,
                bg_color_bottom: app.render.bg_color_bottom,
                fov_deg: app.render.fov_deg,
                env_brightness: app.render.env_brightness,
                env_contrast: app.render.env_contrast,
                env_saturation: app.render.env_saturation,
                env_blur: app.render.env_blur,
                pt_dof_aperture: app.render.pt_dof_aperture,
                pt_dof_falloff: app.render.pt_dof_falloff,
                pt_denoise: app.render.pt_denoise,
                denoise_strength: app.render.denoise_strength,
                denoise_radius: app.render.denoise_radius,
                fog_phase_g: app.render.fog_phase_g,
                fog_absorption: app.render.fog_absorption,
                fog_noise_scale: app.render.fog_noise_scale,
                fog_noise_strength: app.render.fog_noise_strength,
                fog_max_distance: app.render.fog_max_distance,
                fog_shadow_steps: app.render.fog_shadow_steps,
                voxel_cache_res: app.render.voxel_cache_res,
                adaptive_resolution: app.cache.body_vol_cache.use_adaptive_resolution,
                atlas_xy_resolution: app.render.atlas_xy_resolution,
                worker_gpu_enabled: app.worker_gpus.iter()
                    .map(|w| (w.adapter_name.clone(), w.enabled))
                    .collect(),
                sdf_edge_aa_enabled: app.render.sdf_edge_aa_enabled,
                fxaa_enabled: app.render.fxaa_enabled,
                matcap_bg_color: app.render.matcap_bg_color,
                matcap_bg_gradient: app.render.matcap_bg_gradient,
                matcap_bg_gradient_top: app.render.matcap_bg_gradient_top,
                matcap_rotation: app.render.matcap_rotation,
                matcap_edge_enabled: app.render.matcap_edge_enabled,
                matcap_edge_color: app.render.matcap_edge_color,
                matcap_edge_thickness: app.render.matcap_edge_thickness,
                matcap_edge_threshold: app.render.matcap_edge_threshold,
            },
            editor: EditorSettings {
                snap_mode: app.viewport.snap_mode,
                snap_grid: app.viewport.snap_grid,
                snap_rotation_deg: app.viewport.snap_rotation_deg,
                grid_cell_size: app.viewport.grid_cell_size,
                grid_opacity: app.viewport.grid_opacity,
                grid_visible: app.viewport.grid_visible,
                wp_mode: app.viewport.wp_mode,
                wp_settings: WpSettingsData {
                    spacing: app.viewport.wp_settings.spacing,
                    color: app.viewport.wp_settings.color,
                    opacity: app.viewport.wp_settings.opacity,
                    orientation: app.viewport.wp_settings.orientation,
                    origin: app.viewport.wp_settings.origin.to_array(),
                },
                wp_saved: app.viewport.wp_saved.iter().map(|s| SavedWpData {
                    name: s.name.clone(),
                    origin: s.origin.to_array(),
                    normal: s.normal.to_array(),
                }).collect(),
                wp_locked_origin: app.viewport.wp_locked_origin.to_array(),
                wp_locked_normal: app.viewport.wp_locked_normal.to_array(),
            },
            scene_graph: Some(app.scene.scene_graph.clone()),
            imported_meshes,
            // v7: persist every VoxelBody node's grid via the side-channel (node grid is #[serde(skip)]).
            voxel_grids: app.scene.scene_graph.all_nodes().filter_map(|(nid, n)| {
                if let core_math::scene_graph::NodePayload::VoxelBody { grid, .. } = &n.payload {
                    Some(VoxelNodeRecord { node_id: nid.0, grid: grid.to_record() })
                } else { None }
            }).collect(),
            compositor: Some(app.compositor.clone()),
            // v8: persist each PolygonalBody's authored CustomData (uv/crease/seam/sharp) via the
            // in-blob #[serde(default)] poly_layers field — the runtime sidecar (cache.poly_custom_data)
            // is NOT serialized. Mirrors the voxel_grids collection EXACTLY: iterate the runtime store,
            // keep only NON-EMPTY sidecars (an unauthored scene emits NO records → file identical to
            // pre-v8), key each by node_id. Reattached on load (project_load.rs).
            poly_layers: app.cache.poly_custom_data.iter()
                .filter(|(_, cd)| !cd.is_empty())
                .map(|(nid, cd)| {
                    let (uv, crease, seam, sharp, groups, group_meta) = cd.to_layer_tuples();
                    PolyLayerRecord {
                        node_id: nid.0,
                        uv,
                        crease,
                        seam,
                        sharp,
                        groups,
                        group_meta,
                    }
                })
                .collect(),
            // Smart Select tags ride their OWN `FL4PTAGS` tail section, NOT `poly_layers` — a
            // field on that in-blob record was tried twice and corrupted every existing `.fl4`
            // (bincode is positional). Only bodies that actually carry a tag emit a record, so a
            // scene that never ran a tagging op still saves byte-identically to before.
            // FL4PIVT: node ids whose Custom pivot is node-local (PivotConfig.local is
            // serde(skip) — the in-blob bool of 2026-08-18 broke every older .fl4).
            // Only Custom-mode pivots matter to the migration; others never read the flag.
            pivot_local_nodes: {
                let mut ids: Vec<u64> = app
                    .scene
                    .scene_graph
                    .all_ids()
                    .filter(|&nid| {
                        app.scene.scene_graph.get(nid).map_or(false, |n| {
                            n.pivot.mode == core_math::scene_graph::PivotMode::Custom
                                && n.pivot.local
                        })
                    })
                    .map(|nid| nid.0)
                    .collect();
                ids.sort_unstable();
                ids
            },
            poly_op_tags: app.cache.poly_custom_data.iter()
                .filter(|(_, cd)| !cd.op_created.is_empty())
                .map(|(nid, cd)| {
                    let mut op_created: Vec<u32> = cd.op_created.iter().map(|h| h.0).collect();
                    op_created.sort_unstable(); // deterministic bytes for identical scene state
                    crate::project::PolyOpTagRecord { node_id: nid.0, op_created }
                })
                .collect(),
            // P1+P2: persist each CurveExtrude prim's shell/rim + open/thickness/tube via the
            // side-channel (all #[serde(skip)] on the variant). Walk every SdfBody's prim array; emit
            // ONLY prims with a non-default param (an untouched scene → no records → file identical to
            // pre-P1). Keyed by prim_uid.
            curve_params: {
                let mut recs = Vec::new();
                for (_, n) in app.scene.scene_graph.all_nodes() {
                    if let core_math::scene_graph::NodePayload::SdfBody { primitives, .. } = &n.payload {
                        for p in primitives {
                            if let core_math::primitives::SdfShape::CurveExtrude { wall_t, rim, open, thickness, tube, .. } = &p.shape {
                                if wall_t.abs() > 1e-6 || rim.abs() > 1e-6 || *open || thickness.abs() > 1e-6 {
                                    recs.push(CurveParamRecord {
                                        prim_uid: p.prim_uid, wall_t: *wall_t, rim: *rim,
                                        open: *open, thickness: *thickness, tube: *tube,
                                    });
                                }
                            }
                        }
                    }
                }
                recs
            },
            // Body post-DC mesh modifier stacks (payload field is #[serde(skip)]).
            body_mesh_modifiers: app.scene.scene_graph.all_nodes().filter_map(|(nid, n)| {
                if let core_math::scene_graph::NodePayload::SdfBody { mesh_modifiers, .. } = &n.payload {
                    if mesh_modifiers.is_empty() {
                        None
                    } else {
                        Some(crate::project::BodyMeshModRecord {
                            node_id: nid.0,
                            modifiers: mesh_modifiers.clone(),
                        })
                    }
                } else {
                    None
                }
            }).collect(),
            // Body field modifier stacks (Volume Mirror / Bevel).
            body_field_modifiers: app.scene.scene_graph.all_nodes().filter_map(|(nid, n)| {
                if let core_math::scene_graph::NodePayload::SdfBody { field_modifiers, .. } = &n.payload {
                    if field_modifiers.is_empty() {
                        None
                    } else {
                        Some(crate::project::BodyFieldModsSide {
                            node_id: nid.0,
                            modifiers: field_modifiers.clone(),
                        })
                    }
                } else {
                    None
                }
            }).collect(),
            // Mesh Terrain tile registry (NodeId identity — not outliner names).
            terrain_tiles: {
                let ig = app.terrain_world.island_group.map(|n| n.0).unwrap_or(0);
                let half = app.terrain_world.half_window;
                let stream = app.terrain_world.stream_enabled;
                app.terrain_world
                    .tiles
                    .iter()
                    .map(|(&(tx, tz), &nid)| crate::project::TerrainTileRecord {
                        node_id: nid.0,
                        tx,
                        tz,
                        island_group: ig,
                        half_window: half,
                        stream_enabled: stream,
                    })
                    .collect()
            },
            // Parametric poly primitives — the `PolygonalBody.source` side-channel.
            poly_params: app.poly_collect_param_records(),
            // Reconstruct MaterialLibrary from graph Material nodes for backward compatibility.
            // v6+ files also store materials in the graph; this is for v5 readers.
            material_library: {
                let mut lib = crate::MaterialLibrary::default();
                let mut max_id = 0u32;
                for (_, mid, _, data) in app.scene.scene_graph.materials() {
                    lib.materials.push(crate::SceneMaterial { id: mid.0, mat: data.clone() });
                    max_id = max_id.max(mid.0);
                }
                lib.next_id = max_id;
                Some(lib)
            },
            // Unified per-body child_order (skip-serialized field on SdfBody). Collected here so
            // `save_binary` can append it to the `FL4ORDER` trailing section. Only NON-DEFAULT orders
            // are emitted (collect_body_child_orders skips empty ones); the loader self-heals the rest.
            child_orders: app.scene.scene_graph.collect_body_child_orders(),
            // #2: persist every USED albedo-map layer as PNG via the FL4MATTX tail section. The GPU
            // array is the runtime source (pixels retained CPU-side in slot_pixels); export_slots()
            // returns slots 1..used_layers in order, so record.slot = i+1. Empty when no maps are
            // loaded → the file has no material section (identical to a pre-#2 save).
            material_maps: material_slots.into_iter().enumerate().filter_map(|(i, px)| {
                    crate::material_textures::encode_map_png(&px)
                        .map(|png| crate::project::MaterialMapRecord { slot: (i as u32) + 1, png })
                }).collect(),
            // Multi-map channels: the `#[serde(skip)]` normal/roughness/metallic TexMaps ride the
            // FL4MATX2 tail (their PIXELS already ride FL4MATTX with every other used slot above).
            material_channels: collect_material_channels(&app.scene.scene_graph),
            // Per-project environment (FL4ENVIR tail): the currently-installed HDRI's PATH
            // (relative to the project dir when possible) + intensity + rotation — never the
            // pixels. None when no HDRI is loaded → no section written (byte-compat).
            env: app.env_source_path.as_ref().map(|hdr| crate::project::EnvRecord {
                path: crate::project::env_path_for_save(hdr, app.project.project_path.as_deref()),
                brightness: app.render.env_brightness,
                rotation_deg: app.render.env_rotation_deg,
            }),
            // Per-light volumetric ("god ray") params ride the FL4LTVOL tail — only ENABLED lights
            // emit a record, so a scene with no volumetric lights writes NO section (byte-compat).
            light_volumetrics: collect_light_volumetrics(&app.scene.scene_graph),
            // Per-light RAY-TRACED SHADOW params (Stage 4) ride the FL4LTRT tail — only lights whose
            // RT params were AUTHORED (differ from default) emit a record (byte-compat with older saves).
            light_rt: collect_light_rt(&app.scene.scene_graph),
            // Procedural shader node graphs ride FL4NODES (newest tail — after FL4LTRTS). Empty when
            // no prim has a graph → no section (byte-compat).
            node_graphs: collect_node_graphs(app),
            // Material panel docs ride FL4MPNLS (newest tail — after FL4NODES). Empty when no
            // material has an authored panel doc → no section (byte-compat).
            material_panel_docs: crate::material_panel::collect_panel_docs(&app.ui.material_panel_docs),
            // Authored Auto Smooth angles ride FL4PNORM (newest tail — after FL4MPNLS). Only
            // bodies that authored one emit a record → no section, byte-compat, for every scene
            // that never touched Shade Smooth / Shade Flat. The per-edge `sharp` marks need no
            // record here: they already ride the in-blob `poly_layers` above.
            poly_normals: app
                .cache
                .poly_custom_data
                .iter()
                .filter_map(|(nid, cd)| {
                    cd.auto_smooth_deg.map(|deg| crate::project::PolyNormalRecord {
                        node_id: nid.0,
                        auto_smooth_deg: deg,
                    })
                })
                .collect(),
            // Per-poly-modifier #[serde(skip)] flags ride FL4PMODF (newest tail — after
            // FL4PPRM). Only non-default flags emit a record → byte-compat when unused.
            poly_mod_flags: collect_poly_mod_flags(&app.scene.scene_graph),
            // Smooth mask feather rides its own FL4PSMTH tail (newest section).
            poly_smooth: collect_poly_smooth(&app.scene.scene_graph),
            poly_remesh: collect_poly_remesh(&app.scene.scene_graph),
            poly_remesh_collapse: collect_poly_remesh_collapse(&app.scene.scene_graph),
            poly_remesh_skip_relax: collect_poly_remesh_skip_relax(&app.scene.scene_graph),
            poly_remesh_skip_seal: collect_poly_remesh_skip_seal(&app.scene.scene_graph),
            poly_remesh_drop_res: collect_poly_remesh_drop_res(&app.scene.scene_graph),
            bake_settings: collect_bake_settings(app),
            bake_images: collect_bake_images(app),
            bake_bit16: collect_bake_bit16(app),
            bake_cage: collect_bake_cage(app),
            bake_udim: collect_bake_udim(app),
            bake_gpu_rays: collect_bake_gpu_rays(app),
            poly_bevel_var: collect_poly_bevel_var(&app.scene.scene_graph),
            poly_bend: collect_poly_bend(&app.scene.scene_graph),
            poly_sculpt: collect_poly_sculpt(&app.scene.scene_graph),
        })
    }
}

/// Collect FL4PMODF records: every poly modifier whose `#[serde(skip)]` flags differ from
/// default (currently just Bevel `bevel_consume`), keyed by (node_id, stack index).
pub(crate) fn collect_poly_mod_flags(
    sg: &core_math::scene_graph::SceneGraph,
) -> Vec<crate::project::PolyModFlagRecord> {
    let mut recs = Vec::new();
    for (nid, node) in sg.poly_bodies() {
        if let core_math::scene_graph::NodePayload::PolygonalBody { modifiers, .. } = &node.payload {
            for (mi, m) in modifiers.iter().enumerate() {
                if m.bevel_consume {
                    recs.push(crate::project::PolyModFlagRecord {
                        node_id: nid.0,
                        mod_index: mi as u32,
                        bevel_consume: true,
                    });
                }
            }
        }
    }
    recs
}

/// Collect FL4PSMTH records: every modifier carrying `#[serde(skip)]` mask state that differs from
/// the default, keyed by (node_id, stack index).
///
/// Emitted only when the modifier actually HAS a geometry channel: without one the feather is
/// inert and the channel params are unread, so persisting them would add a section — and cost
/// byte-compat with older builds — to carry values that change nothing.
pub(crate) fn collect_poly_smooth(
    sg: &core_math::scene_graph::SceneGraph,
) -> Vec<crate::project::PolySmoothRecord> {
    let def = core_math::scene_graph::PolyModifier::default();
    let def_ch = core_math::geometry_channel::GeometryChannel::default();
    let mut recs = Vec::new();
    for (nid, node) in sg.poly_bodies() {
        if let core_math::scene_graph::NodePayload::PolygonalBody { modifiers, .. } = &node.payload {
            for (mi, m) in modifiers.iter().enumerate() {
                if m.geo_channel.is_all() {
                    continue;
                }
                let non_default = m.smooth_falloff != def.smooth_falloff
                    || m.geo_channel.group_id != def_ch.group_id
                    || m.geo_channel.weight_min != def_ch.weight_min
                    || m.geo_channel.weight_max != def_ch.weight_max;
                if non_default {
                    recs.push(crate::project::PolySmoothRecord {
                        node_id: nid.0,
                        mod_index: mi as u32,
                        smooth_falloff: m.smooth_falloff,
                        group_id: m.geo_channel.group_id,
                        weight_min: m.geo_channel.weight_min,
                        weight_max: m.geo_channel.weight_max,
                    });
                }
            }
        }
    }
    recs
}

/// Collect FL4PRMSH records: every modifier whose Voxel Remesh params differ from the defaults,
/// keyed by (node_id, stack index).
///
/// Unlike [`collect_poly_smooth`] there is NO `geo_channel.is_all()` gate: a Remesh modifier's
/// resolution is authored state regardless of whether it also carries a geometry channel. The
/// non-default filter is what keeps unused scenes byte-identical — a stack that never touched a
/// Remesh slider emits no records, so `save_binary` writes no `FL4PRMSH` section at all.
///
/// Non-Remesh modifiers hold the same field defaults, so they are naturally filtered out without
/// this needing to know the modifier kind.
pub(crate) fn collect_poly_remesh(
    sg: &core_math::scene_graph::SceneGraph,
) -> Vec<crate::project::PolyRemeshRecord> {
    let def = core_math::scene_graph::PolyModifier::default();
    let mut recs = Vec::new();
    for (nid, node) in sg.poly_bodies() {
        if let core_math::scene_graph::NodePayload::PolygonalBody { modifiers, .. } = &node.payload {
            for (mi, m) in modifiers.iter().enumerate() {
                let non_default = m.remesh_resolution != def.remesh_resolution
                    || m.remesh_smooth != def.remesh_smooth
                    || m.remesh_padding != def.remesh_padding
                    || m.remesh_project != def.remesh_project
                    || m.remesh_union_islands != def.remesh_union_islands;
                if non_default {
                    recs.push(crate::project::PolyRemeshRecord {
                        node_id: nid.0,
                        mod_index: mi as u32,
                        resolution: m.remesh_resolution,
                        smooth: m.remesh_smooth,
                        padding: m.remesh_padding,
                        project: m.remesh_project,
                        union_islands: m.remesh_union_islands,
                    });
                }
            }
        }
    }
    recs
}

/// Collect FL4SCLP records: every Sculpt modifier with stored deltas (or a bumped gen).
pub(crate) fn collect_poly_sculpt(
    sg: &core_math::scene_graph::SceneGraph,
) -> Vec<crate::project::PolySculptRecord> {
    use core_math::scene_graph::PolyModifierType;
    let mut recs = Vec::new();
    for (nid, node) in sg.poly_bodies() {
        if let core_math::scene_graph::NodePayload::PolygonalBody { modifiers, .. } = &node.payload {
            for (mi, m) in modifiers.iter().enumerate() {
                if m.mod_type != PolyModifierType::Sculpt {
                    continue;
                }
                if m.sculpt_deltas.is_empty() && m.sculpt_delta_gen == 0 {
                    continue;
                }
                recs.push(crate::project::PolySculptRecord {
                    node_id: nid.0,
                    mod_index: mi as u32,
                    gen: m.sculpt_delta_gen,
                    deltas: m
                        .sculpt_deltas
                        .iter()
                        .map(|(i, d)| (*i, [d.x, d.y, d.z]))
                        .collect(),
                });
            }
        }
    }
    recs
}

/// Collect FL4PBVAR records: every modifier with bevel VARIABLE WIDTH or AUTO MERGE enabled, keyed
/// by (node_id, stack index).
///
/// The gate is "either toggle is on" rather than a full non-default comparison: both off IS the
/// pre-feature state, so a stack that never touched them emits no records and `save_binary` writes no
/// `FL4PBVAR` section at all — such files stay byte-identical to pre-feature saves. `bevel_width_min`
/// rides along even at its default, because once Variable Width is on that value is authored state
/// (it is the small end of the band the user set).
pub(crate) fn collect_poly_bevel_var(
    sg: &core_math::scene_graph::SceneGraph,
) -> Vec<crate::project::PolyBevelVarRecord> {
    let mut recs = Vec::new();
    for (nid, node) in sg.poly_bodies() {
        if let core_math::scene_graph::NodePayload::PolygonalBody { modifiers, .. } = &node.payload {
            for (mi, m) in modifiers.iter().enumerate() {
                if m.bevel_var_width != 0 || m.bevel_auto_merge {
                    recs.push(crate::project::PolyBevelVarRecord {
                        node_id: nid.0,
                        mod_index: mi as u32,
                        var_width: m.bevel_var_width,
                        width_min: m.bevel_width_min,
                        auto_merge: m.bevel_auto_merge,
                    });
                }
            }
        }
    }
    recs
}

/// Collect FL4PBEND records: every Bend modifier whose params differ from the defaults, keyed by
/// (node_id, stack index).
///
/// Gated on `mod_type == Bend`, unlike [`collect_poly_remesh`] which relies on non-Bend modifiers
/// holding the field defaults. That trick does not work here: `bend_angle_deg` defaults to a
/// NON-zero 45°, so every modifier in the scene matches the default and the filter would be a pure
/// no-op — but the moment a user typed 45 into a Bend, the record would be dropped and the angle
/// would load back as 45 anyway. Keying on the type keeps that accident impossible to reintroduce.
///
/// The non-default filter is what keeps unused scenes byte-identical: a stack with no Bend, or one
/// whose Bend is still untouched, emits no records, so `save_binary` writes no `FL4PBEND` section
/// at all.
pub(crate) fn collect_poly_bend(
    sg: &core_math::scene_graph::SceneGraph,
) -> Vec<crate::project::PolyBendRecord> {
    use core_math::scene_graph::PolyModifierType;
    let def = core_math::scene_graph::PolyModifier::default();
    let mut recs = Vec::new();
    for (nid, node) in sg.poly_bodies() {
        if let core_math::scene_graph::NodePayload::PolygonalBody { modifiers, .. } = &node.payload {
            for (mi, m) in modifiers.iter().enumerate() {
                if m.mod_type != PolyModifierType::Bend {
                    continue;
                }
                let non_default = m.bend_angle_deg != def.bend_angle_deg
                    || m.bend_axis != def.bend_axis
                    || m.bend_dir_deg != def.bend_dir_deg
                    || m.bend_limit_lo != def.bend_limit_lo
                    || m.bend_limit_hi != def.bend_limit_hi
                    || m.bend_center != def.bend_center;
                if non_default {
                    recs.push(crate::project::PolyBendRecord {
                        node_id: nid.0,
                        mod_index: mi as u32,
                        angle_deg: m.bend_angle_deg,
                        axis: m.bend_axis,
                        dir_deg: m.bend_dir_deg,
                        limit_lo: m.bend_limit_lo,
                        limit_hi: m.bend_limit_hi,
                        center: m.bend_center.to_array(),
                    });
                }
            }
        }
    }
    recs
}

pub(crate) fn collect_poly_remesh_drop_res(
    sg: &core_math::scene_graph::SceneGraph,
) -> Vec<crate::project::PolyRemeshDropResRecord> {
    let mut recs = Vec::new();
    for (nid, node) in sg.poly_bodies() {
        if let core_math::scene_graph::NodePayload::PolygonalBody { modifiers, .. } = &node.payload {
            for (mi, m) in modifiers.iter().enumerate() {
                if m.remesh_drop_res_drag {
                    recs.push(crate::project::PolyRemeshDropResRecord {
                        node_id: nid.0,
                        mod_index: mi as u32,
                        drop_res_drag: true,
                    });
                }
            }
        }
    }
    recs
}

pub(crate) fn collect_poly_remesh_skip_seal(
    sg: &core_math::scene_graph::SceneGraph,
) -> Vec<crate::project::PolyRemeshSkipSealRecord> {
    let mut recs = Vec::new();
    for (nid, node) in sg.poly_bodies() {
        if let core_math::scene_graph::NodePayload::PolygonalBody { modifiers, .. } = &node.payload {
            for (mi, m) in modifiers.iter().enumerate() {
                if m.remesh_skip_seal_drag {
                    recs.push(crate::project::PolyRemeshSkipSealRecord {
                        node_id: nid.0,
                        mod_index: mi as u32,
                        skip_seal_drag: true,
                    });
                }
            }
        }
    }
    recs
}

pub(crate) fn collect_poly_remesh_skip_relax(
    sg: &core_math::scene_graph::SceneGraph,
) -> Vec<crate::project::PolyRemeshSkipRelaxRecord> {
    let mut recs = Vec::new();
    for (nid, node) in sg.poly_bodies() {
        if let core_math::scene_graph::NodePayload::PolygonalBody { modifiers, .. } = &node.payload {
            for (mi, m) in modifiers.iter().enumerate() {
                if m.remesh_skip_relax_drag {
                    recs.push(crate::project::PolyRemeshSkipRelaxRecord {
                        node_id: nid.0,
                        mod_index: mi as u32,
                        skip_relax_drag: true,
                    });
                }
            }
        }
    }
    recs
}

pub(crate) fn collect_poly_remesh_collapse(
    sg: &core_math::scene_graph::SceneGraph,
) -> Vec<crate::project::PolyRemeshCollapseRecord> {
    let mut recs = Vec::new();
    for (nid, node) in sg.poly_bodies() {
        if let core_math::scene_graph::NodePayload::PolygonalBody { modifiers, .. } = &node.payload {
            for (mi, m) in modifiers.iter().enumerate() {
                if m.remesh_collapse {
                    recs.push(crate::project::PolyRemeshCollapseRecord {
                        node_id: nid.0,
                        mod_index: mi as u32,
                        collapse: true,
                    });
                }
            }
        }
    }
    recs
}

/// Collect FL4BAKE records: only bodies whose settings differ from default.
pub(crate) fn collect_bake_settings(app: &AzraApp) -> Vec<crate::project::BakeSettingsRecord> {
    use crate::mesh_map_bake::BakeSource;
    let mut recs = Vec::new();
    let mut keys: Vec<_> = app.ui.mesh_map_settings.keys().copied().collect();
    keys.sort_by_key(|n| n.0);
    for nid in keys {
        let Some(s) = app.ui.mesh_map_settings.get(&nid) else {
            continue;
        };
        if s.is_default() {
            continue;
        }
        let (high_nids, match_by_name) = match &s.source {
            BakeSource::SelfMesh => (Vec::new(), false),
            BakeSource::HighPoly { nids, match_by_name } => {
                (nids.iter().map(|n| n.0).collect(), *match_by_name)
            }
        };
        recs.push(crate::project::BakeSettingsRecord {
            node_id: nid.0,
            enabled_maps: s.enabled_maps,
            res_global: s.res_global,
            supersample: s.supersample,
            padding_px: s.padding_px,
            source_kind: s.source.kind(),
            high_nids,
            match_by_name,
            max_front: s.max_front,
            max_rear: s.max_rear,
            ao_rays: s.ao_rays,
            curv_radius: s.curv_radius,
        });
    }
    recs
}

/// Collect FL4BAKE image-registry records (G4). Empty → no images in the tail.
pub(crate) fn collect_bake_images(app: &AzraApp) -> Vec<crate::project::BakedImageRecord> {
    app.ui.baked_images.to_records()
}

/// Collect FL4B16 records: only bodies that opted into 16-bit vector maps.
pub(crate) fn collect_bake_bit16(app: &AzraApp) -> Vec<crate::project::BakeBit16Record> {
    let mut recs = Vec::new();
    let mut keys: Vec<_> = app.ui.mesh_map_settings.keys().copied().collect();
    keys.sort_by_key(|n| n.0);
    for nid in keys {
        let Some(s) = app.ui.mesh_map_settings.get(&nid) else {
            continue;
        };
        if s.bit16 {
            recs.push(crate::project::BakeBit16Record {
                node_id: nid.0,
                bit16: 1,
            });
        }
    }
    recs
}

/// Collect FL4CAGE records: only bodies that picked a mesh cage.
pub(crate) fn collect_bake_cage(app: &AzraApp) -> Vec<crate::project::BakeCageRecord> {
    let mut recs = Vec::new();
    let mut keys: Vec<_> = app.ui.mesh_map_settings.keys().copied().collect();
    keys.sort_by_key(|n| n.0);
    for nid in keys {
        let Some(s) = app.ui.mesh_map_settings.get(&nid) else {
            continue;
        };
        if !s.cage_nids.is_empty() {
            recs.push(crate::project::BakeCageRecord {
                node_id: nid.0,
                cage_nids: s.cage_nids.iter().map(|n| n.0).collect(),
            });
        }
    }
    recs
}

/// Collect FL4UDIM: on-flag per body + tile numbers for non-1001 images.
pub(crate) fn collect_bake_udim(app: &AzraApp) -> crate::project::BakeUdimTail {
    use crate::mesh_map_bake::udim::UDIM_1001;
    let mut flags = Vec::new();
    let mut keys: Vec<_> = app.ui.mesh_map_settings.keys().copied().collect();
    keys.sort_by_key(|n| n.0);
    for nid in keys {
        let Some(s) = app.ui.mesh_map_settings.get(&nid) else {
            continue;
        };
        if s.udim {
            flags.push(crate::project::BakeUdimFlagRecord {
                node_id: nid.0,
                udim: 1,
            });
        }
    }
    let mut tiles: Vec<crate::project::BakeUdimTileRecord> = app
        .ui
        .baked_images
        .images
        .iter()
        .filter(|e| e.udim != 0 && e.udim != UDIM_1001)
        .map(|e| crate::project::BakeUdimTileRecord {
            name: e.name.clone(),
            udim: e.udim,
        })
        .collect();
    tiles.sort_by(|a, b| a.name.cmp(&b.name));
    crate::project::BakeUdimTail { flags, tiles }
}

/// Collect FL4GRAY: only bodies that opted into GPU rays.
pub(crate) fn collect_bake_gpu_rays(app: &AzraApp) -> Vec<crate::project::BakeGpuRayRecord> {
    let mut recs = Vec::new();
    let mut keys: Vec<_> = app.ui.mesh_map_settings.keys().copied().collect();
    keys.sort_by_key(|n| n.0);
    for nid in keys {
        let Some(s) = app.ui.mesh_map_settings.get(&nid) else {
            continue;
        };
        if s.gpu_rays {
            recs.push(crate::project::BakeGpuRayRecord {
                node_id: nid.0,
                gpu_rays: 1,
            });
        }
    }
    recs
}

/// Collect FL4NODES records: prim graphs + **outliner Material graphs**.
///
/// Wire shape stays `{ prim_uid, graph_json }` (bincode-stable with PR0). Material ownership is
/// encoded **inside** `graph_json` as an envelope:
/// `{ "azra_owner": "material", "material_id": N, "graph": <NodeGraph> }` with `prim_uid = 0`.
/// Legacy prim graphs keep plain `NodeGraph` JSON + real `prim_uid`.
pub(crate) fn collect_node_graphs(app: &AzraApp) -> Vec<crate::project::NodeGraphRecord> {
    let mut recs = Vec::new();
    // Material-owned (primary).
    for (&mid, graph) in &app.ui.material_node_graphs {
        let mut g = graph.clone();
        g.wgsl_cache = None;
        match serde_json::to_string(&MaterialGraphEnvelope {
            azra_owner: "material".into(),
            material_id: mid,
            graph: g,
        }) {
            Ok(graph_json) => recs.push(crate::project::NodeGraphRecord {
                prim_uid: 0,
                graph_json,
            }),
            Err(e) => eprintln!("[Save] FL4NODES: material {mid} JSON failed: {e}"),
        }
    }
    // Prim-owned (legacy override).
    for (&prim_uid, graph) in &app.ui.node_graphs {
        let mut g = graph.clone();
        g.wgsl_cache = None;
        match serde_json::to_string(&g) {
            Ok(graph_json) => recs.push(crate::project::NodeGraphRecord {
                prim_uid,
                graph_json,
            }),
            Err(e) => eprintln!("[Save] FL4NODES: prim {prim_uid} JSON failed: {e}"),
        }
    }
    // Deterministic order: materials first (prim_uid 0), then by prim_uid / material id in JSON.
    recs.sort_by(|a, b| a.prim_uid.cmp(&b.prim_uid).then_with(|| a.graph_json.cmp(&b.graph_json)));
    recs
}

/// Envelope for material-owned graphs inside FL4NODES `graph_json` (keeps record layout stable).
#[derive(serde::Serialize, serde::Deserialize)]
pub(crate) struct MaterialGraphEnvelope {
    pub azra_owner: String,
    pub material_id: u32,
    pub graph: crate::shader_nodes::NodeGraph,
}

/// Collect the FL4LTRT records: every graph light whose RT-shadow params were AUTHORED (differ from
/// `LightRtShadow::default()` = enabled+radius0). Pure fn — shared by `ProjectFile::from_app` and the
/// round-trip tests. An untouched light emits no record → a scene that never touched RT shadows is
/// byte-identical to a pre-Stage-4 save. (A light turned RT OFF, or given a radius, DOES differ from
/// default → its record persists so the hybrid authoring survives save→load, the DCC norm.)
pub(crate) fn collect_light_rt(
    sg: &core_math::scene_graph::SceneGraph,
) -> Vec<crate::project::LightRtRecord> {
    sg.lights()
        .filter(|(_, l)| l.rt_shadow != core_math::scene_graph::LightRtShadow::default())
        .map(|(_, l)| crate::project::LightRtRecord {
            light_id: l.id,
            enabled: l.rt_shadow.enabled,
            radius: l.rt_shadow.radius,
        })
        .collect()
}

/// Collect the FL4LTVOL records: every graph light whose volumetric scattering is ENABLED.
/// Pure fn — shared by `ProjectFile::from_app` and the round-trip tests. Disabled lights emit no
/// record so a scene without volumetric lights is byte-identical to a pre-volumetric save.
pub(crate) fn collect_light_volumetrics(
    sg: &core_math::scene_graph::SceneGraph,
) -> Vec<crate::project::LightVolRecord> {
    sg.lights()
        // Emit a record for any light whose volumetric params were AUTHORED (differ from default),
        // not just enabled ones — so density/softness/zone authored then toggled OFF survive
        // save→load (DCC norm: Blender persists disabled-modifier params). All-default (untouched)
        // lights still emit nothing → a scene that never touched volumetric stays byte-identical to
        // a pre-volumetric save.
        .filter(|(_, l)| l.volumetric != core_math::scene_graph::LightVolumetric::default())
        .map(|(_, l)| crate::project::LightVolRecord {
            light_id: l.id,
            enabled: l.volumetric.enabled,
            density: l.volumetric.density,
            softness: l.volumetric.softness,
            zone: l.volumetric.zone,
        })
        .collect()
}

/// Collect the FL4MATX2 records: every graph material's SET (tex_id != 0) non-albedo map channel.
/// Pure fn — shared by `ProjectFile::from_app` and the round-trip tests (dc_baseline `project_with`).
pub(crate) fn collect_material_channels(
    sg: &core_math::scene_graph::SceneGraph,
) -> Vec<crate::project::MaterialChannelRecord> {
    use core_math::pbr_material::MapKind;
    let mut recs = Vec::new();
    for (_, mid, _, data) in sg.materials() {
        for kind in MapKind::DATA_SLOTS {
            let m = data.map(kind);
            if m.tex_id != 0 {
                // green_dx is runtime-only on TexMap (bincode field-trap) — ride the record's
                // projection high bit for the Normal channel (see MATX2_PROJ_GREEN_DX_BIT).
                let green_bit = if kind == MapKind::Normal && m.green_dx {
                    crate::project::MATX2_PROJ_GREEN_DX_BIT
                } else {
                    0
                };
                // high_precision is likewise runtime-only — persist the REAL flag (bit 9)
                // instead of the loader guessing by kind. The guess flagged every 8-bit
                // Bump/Displacement as dual-plane, so the shader read `slot+1` — an
                // unrelated map — as the low bytes ("bump maps look weird on ALL
                // materials" after load, 2026-08-07).
                let hp_bit = if m.high_precision {
                    crate::project::MATX2_PROJ_HIGH_PREC_BIT
                } else {
                    0
                };
                // Triplanar per-plane rotations (runtime-only on TexMap, same trap):
                // 2 bits each at record bits 10-15.
                let rot_bits = (((m.rot_x as u32) & 3)
                    | (((m.rot_y as u32) & 3) << 2)
                    | (((m.rot_z as u32) & 3) << 4))
                    << crate::project::MATX2_PROJ_ROT_SHIFT;
                recs.push(crate::project::MaterialChannelRecord {
                    material_id: mid.0,
                    kind: kind.wire_tag(),
                    tex_id: m.tex_id,
                    strength: m.strength,
                    uv_scale: m.uv_scale,
                    projection: m.projection | green_bit | hp_bit | rot_bits,
                    blend: m.blend,
                });
            }
        }
    }
    // HashMap iteration = random order — sort for a deterministic file (byte-stable saves).
    recs.sort_by_key(|r| (r.material_id, r.kind));
    recs
}

impl AzraApp {
    pub(crate) fn save_scene(&mut self) {
        // Block overwrite-save when file was loaded from a newer version
        if let Some(ref reason) = self.project.save_blocked_reason {
            self.toast(&format!("Save blocked: {}", reason));
            return;
        }
        // NOTE: sync_flat_to_graph removed — all mutations now go through
        // write_prim() / mirror_prim_to_graph() which keep graph in sync.
        if let Some(ref path) = self.project.project_path.clone() {
            self.voxel_sync_node_mesh(); // persist the live sculpt, not the stale initial mesh
            let project = match ProjectFile::from_app(self) {
                Ok(project) => project,
                Err(error) => { self.toast(&format!("Save failed: {error}")); return; }
            };
            let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("fl4");
            let result = if ext == "json" {
                save_json(path, &project)
            } else {
                save_binary(path, &project)
            };
            match result {
                Ok(()) => {
                    self.project.scene_modified = false;
                    self.toast("Saved");
                }
                Err(e) => self.toast(&format!("Save failed: {}", e)),
            }
        } else {
            self.save_scene_as();
        }
    }

    pub(crate) fn save_scene_as(&mut self) {
        let dialog = rfd::FileDialog::new()
            .set_title("Save Scene As")
            .add_filter("AZRA Scene", &["fl4"])
            .add_filter("JSON Scene", &["json"]);
        if let Some(path) = dialog.save_file() {
            self.project.project_path = Some(path.clone());
            self.add_recent_file(path);
            // Save As always writes current version — clear any future-version block
            self.project.save_blocked_reason = None;
            self.save_scene();
        }
    }

    /// Save the current scene as the default startup scene.
    ///
    /// The file is a template: it is reloaded by `load_default_scene_at_startup` and
    /// is not the current document (`project_path` stays whatever the user had).
    pub(crate) fn save_as_default_scene(&mut self) {
        let path = match crate::project::default_scene_path() {
            Ok(path) => path,
            Err(error) => {
                self.toast(&format!("Save default failed: {error}"));
                return;
            }
        };
        if let Err(error) = crate::project::ensure_default_scene_dir(&path) {
            self.toast(&format!("Save default failed: {error}"));
            return;
        }
        self.save_default_scene_to(&path);
    }

    /// Serialize the current scene and atomically replace `path`.
    /// Every failure toasts `Save default failed: ` plus the underlying error
    /// (serialize, permissions, missing directory, rename).
    pub(crate) fn save_default_scene_to(&mut self, path: &std::path::Path) {
        self.voxel_sync_node_mesh(); // persist the live sculpt, not the stale initial mesh
        let project = match ProjectFile::from_app(self) {
            Ok(project) => project,
            Err(error) => {
                self.toast(&format!("Save default failed: {error}"));
                return;
            }
        };
        match save_binary(path, &project) {
            Ok(()) => self.toast("Saved as default scene"),
            Err(e) => self.toast(&format!("Save default failed: {e}")),
        }
    }

    /// Delete the default startup scene so the app opens with an empty scene.
    pub(crate) fn reset_default_scene(&mut self) {
        let path = match crate::project::default_scene_path() {
            Ok(path) => path,
            Err(error) => {
                self.toast(&format!("Reset failed: {error}"));
                return;
            }
        };
        if path.exists() {
            match std::fs::remove_file(&path) {
                Ok(()) => self.toast("Default scene reset"),
                Err(e) => self.toast(&format!("Reset failed: {e}")),
            }
        } else {
            self.toast("No default scene to reset");
        }
    }

    /// Interval for silent auto-save. Must match `next_idle_timer_wake_ms` (B3b) or the
    /// event-driven idle path never wakes `check_auto_save` without mouse input.
    pub(crate) const AUTO_SAVE_INTERVAL_SECS: u64 = 120;

    /// Auto-save check — call in update loop
    pub(crate) fn check_auto_save(&mut self) {
        if self.project.scene_modified
            && self.project.last_auto_save.elapsed()
                > std::time::Duration::from_secs(Self::AUTO_SAVE_INTERVAL_SECS)
        {
            let path = auto_save_path(&self.project.project_path);
            // `voxel_sync_node_mesh` → `materialize_cpu_from_atlas` does a BLOCKING full GPU→CPU readback of
            // the sparse brick arena (device.poll(Wait)). At sparse res (≥256³, and ESPECIALLY after subtract
            // strokes that expose a huge interior → a multi-hundred-MB arena) that readback is a 5–30s UI
            // FREEZE — unacceptable to fire off the silent 120s auto-save timer. Skip the heavy sculpt
            // materialize here; the live sculpt is captured on EXPLICIT save/export (user-initiated, expects a
            // brief wait). When the grid is dense/small (cheap) or already in sync, the sync runs as before.
            // (Proper fix: make the readback async so even explicit save never blocks — tracked follow-up.)
            let sculpt_heavy = self.sculpt.voxel.as_ref()
                .map_or(false, |v| v.cpu_grid_stale && v.grid.density.is_sparse());
            if !sculpt_heavy { self.voxel_sync_node_mesh(); }
            if self.has_active_episode() || self.cache.graph_live_dirty { return; }
            let project = match ProjectFile::from_app(self) {
                Ok(project) => project,
                Err(error) => {
                    eprintln!("[AutoSave] {error}");
                    self.project.last_auto_save = std::time::Instant::now();
                    return;
                }
            };
            if save_binary(&path, &project).is_ok() {
                self.toast(if sculpt_heavy { "Auto-saved (sculpt: Ctrl+S to capture latest)" } else { "Auto-saved" });
            }
            self.project.last_auto_save = std::time::Instant::now();
        }
    }
}

#[cfg(test)]
mod default_scene_tests {
    use super::*;

    fn scratch() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "azra-savedef-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn marker_present(app: &AzraApp) -> bool {
        app.scene.scene_graph.all_nodes().any(|(_, node)| {
            matches!(
                &node.payload,
                core_math::scene_graph::NodePayload::Group { name } if name == "SAVEDEF-MARKER"
            )
        })
    }

    #[test]
    fn save_default_round_trips_and_stays_untitled() {
        let dir = scratch();
        let path = dir.join("default_scene.fl4");
        let mut app = AzraApp::new_headless();
        let root = app.scene.scene_graph.root();
        app.scene.scene_graph.add_child(
            root,
            core_math::scene_graph::NodePayload::Group {
                name: "SAVEDEF-MARKER".into(),
            },
        );
        app.viewport.camera.distance = 42.5;
        app.ui.toasts.clear();
        app.save_default_scene_to(&path);

        let toast = app.ui.toasts.last().map(|t| t.message.clone()).unwrap_or_default();
        assert_eq!(toast, "Saved as default scene", "{toast}");
        assert!(path.is_file(), "default scene file was not written");
        assert!(
            app.project.project_path.is_none(),
            "save-default must not retarget the current document"
        );

        let mut loaded = AzraApp::new_headless();
        assert!(
            (loaded.viewport.camera.distance - 42.5).abs() > 1.0,
            "fresh app must not already be the saved scene"
        );
        assert!(!marker_present(&loaded));
        assert!(loaded.load_startup_scene_from(&path).expect("load startup scene"));
        assert!((loaded.viewport.camera.distance - 42.5).abs() < 1e-4);
        assert!(marker_present(&loaded), "saved group did not reload");
        assert!(loaded.project.project_path.is_none());
        assert!(!loaded.project.scene_modified);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn save_default_failure_toasts_the_real_error() {
        let dir = scratch();
        let mut app = AzraApp::new_headless();
        let distance = app.viewport.camera.distance;
        let bad = dir.join("missing-parent").join("default_scene.fl4");
        app.ui.toasts.clear();
        app.save_default_scene_to(&bad);
        let toast = app.ui.toasts.last().map(|t| t.message.clone()).unwrap_or_default();
        assert!(
            toast.starts_with("Save default failed:"),
            "missing directory must toast, got {toast:?}"
        );
        assert!(
            toast.contains("Create ")
                && (toast.contains("os error") || toast.contains("No such") || toast.contains("directory")),
            "toast must carry the OS error, got {toast}"
        );
        assert!(!bad.exists());

        let garbage = dir.join("garbage.fl4");
        std::fs::write(&garbage, b"this is not an fl4").unwrap();
        let err = app
            .load_startup_scene_from(&garbage)
            .expect_err("corrupt default scene must fail");
        assert!(
            err.contains("Deserialize") || err.contains("error"),
            "load error must name the decode failure, got {err}"
        );
        assert!((app.viewport.camera.distance - distance).abs() < 1e-4);
        assert!(!marker_present(&app));
        assert!(app.project.project_path.is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn default_scene_path_is_the_azra_data_file() {
        match crate::project::default_scene_path() {
            Ok(path) => {
                assert_eq!(
                    path.file_name().and_then(|s| s.to_str()),
                    Some("default_scene.fl4")
                );
                assert_eq!(
                    path.parent().and_then(|d| d.file_name()).and_then(|s| s.to_str()),
                    Some("Azra")
                );
                assert!(path.is_absolute(), "{}", path.display());
            }
            Err(error) => {
                assert!(
                    error.contains("local data directory"),
                    "unresolved data dir must say why, got {error}"
                );
            }
        }
    }
}
