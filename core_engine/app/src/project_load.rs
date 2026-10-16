//! Project load — apply_to_app() + open_scene / load_scene_from_path.

use std::path::Path;
use crate::{AzraApp, DirtyFlags,
            SavedWorkPlane, WorkPlaneSettings, SdfGpuVolume};
use super::project::*;
use core_math::scene_graph::{SceneGraph, NodeId, NodePayload};

/// Feature gate: when true, prims are stored body-local (v5+).
use crate::BODY_LOCAL_PRIMS;

/// Migrate v4 (world-space prims) to v5 (body-local prims).
/// For each SdfBody, subtract body.origin from every prim.position and mirror_center.
fn migrate_v4_to_v5(sg: &mut SceneGraph) {
    // Collect all body NodeIds first to avoid borrow issues
    let body_nids: Vec<NodeId> = sg.all_nodes()
        .filter_map(|(nid, node)| {
            matches!(&node.payload, NodePayload::SdfBody { .. }).then_some(nid)
        })
        .collect();

    for nid in body_nids {
        let origin = match sg.get(nid).map(|n| &n.payload) {
            Some(NodePayload::SdfBody { origin, .. }) => *origin,
            _ => continue,
        };
        // Skip if origin is zero (effectively already local)
        if origin.length_squared() < 1e-10 { continue; }

        if let Some(node) = sg.get_mut(nid) {
            if let NodePayload::SdfBody { primitives, .. } = &mut node.payload {
                for prim in primitives.iter_mut() {
                    prim.position -= origin;
                    prim.mirror_center -= origin;
                }
            }
        }
    }
}

/// Current file format version. Must match the version written by `ProjectFile::from_app()`.
///
/// STAYS AT 7 even though we added the unified `child_order` feature. `child_order` is
/// `#[serde(skip)]`, so the bincode blob is BYTE-IDENTICAL to v7 — the ordering travels in an
/// OPTIONAL, self-delimiting `FL4ORDER` trailing section appended after the blob (see
/// `project.rs save_binary`/`load_binary`). Bumping the version would make a real v7-era AZRA trip
/// its future-version guard for a file whose blob it can still read perfectly, for zero benefit.
/// The MAGIC trailing section — not the version number — is the format-extension signal.
const CURRENT_SAVE_VERSION: u32 = 7;

/// Reattach FL4NODES graphs into `ui.material_node_graphs` and/or `ui.node_graphs` (load-side).
/// Material envelopes: `{ azra_owner: "material", material_id, graph }` with `prim_uid == 0`.
/// Legacy plain NodeGraph JSON uses `prim_uid`. Pads input_defaults; skips corrupt JSON.
pub(crate) fn reattach_node_graphs(
    app: &mut AzraApp,
    recs: &[crate::project::NodeGraphRecord],
) {
    for rec in recs {
        // Prefer material envelope (primary owner).
        if let Ok(env) =
            serde_json::from_str::<crate::project_save::MaterialGraphEnvelope>(&rec.graph_json)
        {
            if env.azra_owner == "material" && env.material_id != 0 {
                let mut graph = env.graph;
                pad_graph_defaults(&mut graph);
                graph.wgsl_cache = None;
                app.ui
                    .material_node_graphs
                    .insert(env.material_id, graph);
                continue;
            }
        }
        // Legacy / prim-owned plain NodeGraph.
        let mut graph = match serde_json::from_str::<crate::shader_nodes::NodeGraph>(&rec.graph_json)
        {
            Ok(g) => g,
            Err(e) => {
                eprintln!(
                    "[Load] FL4NODES: bad graph JSON for prim_uid {} — skipped ({e})",
                    rec.prim_uid
                );
                continue;
            }
        };
        pad_graph_defaults(&mut graph);
        graph.wgsl_cache = None;
        if rec.prim_uid != 0 {
            if let Some(p) = app
                .scene
                .primitives
                .iter_mut()
                .find(|p| p.prim_uid == rec.prim_uid)
            {
                let wgsl = graph.generate_wgsl();
                p.node_graph = Some(wgsl);
            }
            app.ui.node_graphs.insert(rec.prim_uid, graph);
        }
    }
}

fn pad_graph_defaults(graph: &mut crate::shader_nodes::NodeGraph) {
    for node in &mut graph.nodes {
        let need = crate::shader_nodes::Node::input_defs(&node.node_type).len();
        while node.input_defaults.len() < need {
            node.input_defaults.push(crate::shader_nodes::InputDefault {
                value_f32: 0.0,
                value_vec3: [0.0; 3],
            });
        }
    }
}

/// Reattach persisted FL4MATX2 map-channel records to graph materials (load-side pure fn —
/// shared by `apply_to_app` and the round-trip tests). Unknown `kind` tags and dangling
/// material ids are skipped (forward/backward tolerant); Albedo tags are ignored (its TexMap
/// is a serialized blob field — an FL4MATX2 albedo record would double-write it).
pub(crate) fn apply_material_channels(
    sg: &mut core_math::scene_graph::SceneGraph,
    recs: &[crate::project::MaterialChannelRecord],
) {
    use core_math::pbr_material::{MapKind, TexMap};
    for r in recs {
        let Some(kind) = MapKind::from_wire_tag(r.kind) else { continue };
        if kind == MapKind::Albedo { continue; }
        if let Some(mat) = sg.material_by_id_mut(core_math::primitives::MaterialId(r.material_id)) {
            *mat.map_mut(kind) = TexMap {
                tex_id: r.tex_id,
                strength: r.strength,
                uv_scale: r.uv_scale,
                // Flag bits ride the record's high bits (green_dx at bit 8) — strip them so the
                // TexMap field stays a plain code; zero decodes to OpenGL green (pre-flag saves).
                projection: r.projection & 0xFF,
                green_dx: kind == MapKind::Normal
                    && (r.projection & crate::project::MATX2_PROJ_GREEN_DX_BIT) != 0,
                absolute: false,
                // 0.0 means the record predates the blend field (serde default). Those saves
                // rendered with the shader's hardcoded 4.0, so that is what they must load as -
                // taking 0.0 literally would flat-average all three planes and blur them.
                blend: if r.blend > 0.0 { r.blend } else { 4.0 },
                // 16-bit dual-plane flag: read the PERSISTED bit (record bit 9), never guess
                // by kind. The old `matches!(kind, Bump | Displacement)` guess flagged every
                // 8-bit bump as dual-plane, making the shader read `tex_id+1` — an UNRELATED
                // neighboring map — as the low bytes: "bump maps look weird on ALL materials"
                // after every save/load (2026-08-07). Zero = 8-bit = old-save behavior.
                high_precision: (r.projection & crate::project::MATX2_PROJ_HIGH_PREC_BIT) != 0,
                // Triplanar per-plane rotations from record bits 10-15 (runtime TexMap
                // fields are serde(skip) — the bincode field trap). Zero = no rotation.
                rot_x: ((r.projection >> crate::project::MATX2_PROJ_ROT_SHIFT) & 3) as u8,
                rot_y: ((r.projection >> (crate::project::MATX2_PROJ_ROT_SHIFT + 2)) & 3) as u8,
                rot_z: ((r.projection >> (crate::project::MATX2_PROJ_ROT_SHIFT + 4)) & 3) as u8,
                // Anything not persisted in the record takes the TexMap default — which
                // MUST decode to old-save behavior (the packed-word red line).
                ..TexMap::default()
            };
        }
    }
}

impl ProjectFile {
    pub(crate) fn apply_to_app(&self, app: &mut AzraApp) {
        // Project replacement is a mesh-ownership boundary. Drop old mesh + PT
        // caches before restored IDs are inserted; saved MeshCacheIds can
        // otherwise alias a prior scene's BLAS / layout signatures.
        app.cache.mesh_cache = render::mesh_cache::MeshCache::new();
        app.reset_pt_accel_state();

        // Loading a scene: reset the material texture array in the next poll (kills cross-scene bleed),
        // then re-upload THIS scene's persisted maps (#2) in slot order so each material's saved tex_id
        // lines up with no remap. Decode the FL4MATTX PNGs here (CPU); poll does the GPU reset+upload.
        app.cache.material_array_reset_pending = true;
        let mut restored_ok_slots: std::collections::HashSet<u32> = std::collections::HashSet::new();
        {
            // SLOT IDENTITY IS LOAD-BEARING: materials' saved `tex_id`s index these uploads by
            // POSITION (upload N lands in slot N). A dropped record (failed PNG decode) or a slot
            // gap must therefore substitute a WHITE placeholder, never shift later slots — the old
            // `filter_map` skip silently re-mapped every subsequent material onto the wrong image
            // (crash-audit fix, 2026-07-18). Slots are also clamped: a corrupt record with a huge
            // slot value must not allocate gigabytes (user-generated input hardening).
            const MAX_RESTORE_SLOTS: u32 = 512; // 512 × 4 MB = 2 GB — already beyond any real scene
            let mut max_slot = 0u32;
            for r in &self.material_maps {
                if (1..=MAX_RESTORE_SLOTS).contains(&r.slot) { max_slot = max_slot.max(r.slot); }
                else { eprintln!("[Load] FL4MATTX record with invalid slot {} — skipped", r.slot); }
            }
            let mut by_slot: Vec<Option<Vec<u8>>> = vec![None; max_slot as usize];
            for r in &self.material_maps {
                if (1..=MAX_RESTORE_SLOTS).contains(&r.slot) {
                    let decoded = crate::material_textures::decode_map_png(&r.png);
                    if decoded.is_some() {
                        restored_ok_slots.insert(r.slot);
                    }
                    by_slot[(r.slot - 1) as usize] = decoded;
                }
            }
            let white_layer = || {
                let n = crate::material_textures::MAP_RES;
                vec![255u8; (n * n * 4) as usize]
            };
            let restore: Vec<Vec<u8>> = by_slot.into_iter().enumerate().map(|(i, o)| {
                o.unwrap_or_else(|| {
                    eprintln!("[Load] material map slot {} failed to decode — white placeholder keeps later slots aligned", i + 1);
                    white_layer()
                })
            }).collect();
            app.cache.material_maps_restore = if restore.is_empty() { None } else { Some(restore) };
        }
        // Future-version guard: file saved by a newer AZRA — loading is best-effort
        // but overwrite-save is blocked to prevent permanent data loss.
        if self.version > CURRENT_SAVE_VERSION {
            eprintln!(
                "[Load] WARNING: File saved by newer version (v{}) — current is v{}. \
                 Loading may lose data. Save disabled for this file.",
                self.version, CURRENT_SAVE_VERSION
            );
            app.project.save_blocked_reason = Some(format!(
                "File from newer version (v{}). Use Save As to avoid data loss.",
                self.version
            ));
        } else {
            app.project.save_blocked_reason = None;
        }

        // Scene — prim/light loading deferred to graph processing below
        app.scene.next_prim_id = self.scene.next_prim_id;
        app.scene.next_light_id = self.scene.next_light_id;
        app.scene.next_node_name_id = self.scene.next_node_name_id;

        // Camera
        app.viewport.camera.orbit = glam::Quat::from_euler(glam::EulerRot::YXZ,
            self.camera.yaw, self.camera.pitch, self.camera.roll);
        app.viewport.camera.distance = self.camera.distance;
        app.viewport.camera.target = glam::Vec3::from_array(self.camera.target);
        app.render.ortho = self.camera.ortho;

        // Render
        app.render.shading_mode = self.render.shading_mode;
        app.render.resolution_scale = self.render.resolution_scale;
        app.render.render_quality = self.render.render_quality;
        app.render.exposure = if (self.render.exposure - 0.1).abs() < 0.001 {
            1.0 // migrate old default (0.1 was a bug — BUG-008)
        } else {
            self.render.exposure
        };
        app.render.bloom_enabled = self.render.bloom_enabled;
        app.render.bloom_threshold = self.render.bloom_threshold;
        app.render.bloom_intensity = self.render.bloom_intensity;
        app.render.ssr_enabled = self.render.ssr_enabled;
        app.render.fog_enabled = self.render.fog_enabled;
        app.render.fog_density = self.render.fog_density;
        app.render.fog_height_falloff = self.render.fog_height_falloff;
        app.render.fog_scatter = self.render.fog_scatter;
        app.render.fog_color = self.render.fog_color;
        app.render.upsample_enabled = self.render.upsample_enabled;
        app.render.rt_bounces = self.render.rt_bounces;
        app.render.rt_shadow_samples = self.render.rt_shadow_samples;
        app.render.pt_max_bounces = self.render.pt_max_bounces;
        app.render.pt_samples_per_frame = self.render.pt_samples_per_frame;
        app.render.bg_color_top = self.render.bg_color_top;
        app.render.bg_color_bottom = self.render.bg_color_bottom;
        app.render.fov_deg = self.render.fov_deg;
        app.render.env_brightness = self.render.env_brightness;
        app.render.env_contrast = self.render.env_contrast;
        app.render.env_saturation = self.render.env_saturation;
        app.render.env_blur = self.render.env_blur;
        app.render.pt_dof_aperture = self.render.pt_dof_aperture;
        app.render.pt_dof_falloff = self.render.pt_dof_falloff;
        app.render.pt_denoise = self.render.pt_denoise;
        app.render.denoise_strength = self.render.denoise_strength;
        app.render.denoise_radius = self.render.denoise_radius;
        app.render.fog_phase_g = self.render.fog_phase_g;
        app.render.fog_absorption = self.render.fog_absorption;
        app.render.fog_noise_scale = self.render.fog_noise_scale;
        app.render.fog_noise_strength = self.render.fog_noise_strength;
        app.render.fog_max_distance = self.render.fog_max_distance;
        app.render.fog_shadow_steps = self.render.fog_shadow_steps;
        app.render.voxel_cache_res = self.render.voxel_cache_res;
        // Apply loaded voxel cache resolution to cpu_volume + GPU volume
        {
            let res = app.render.voxel_cache_res;
            let bounds = app.cache.cpu_volume.grid_bounds;
            app.cache.cpu_volume = core_math::sdf_volume::SdfVolume::new_empty(res, bounds);
            if let Some(gpu) = &mut app.gpu {
                // Allocate 1³ dummy when body_vol_cache is active (saves 12-96MB).
                // Sculpt mode entry re-creates at full resolution.
                let gpu_vol_res = if app.cache.use_body_vol_cache { [1, 1, 1] } else { [res, res, res] };
                gpu.gpu_volume = SdfGpuVolume::new(&gpu.device, gpu_vol_res);
                gpu.bind_groups.bind_group = None;
                gpu.bind_groups.deferred_bg = None;
                gpu.bind_groups.pt_bg = None;
            }
        }
        // Apply adaptive resolution setting
        app.cache.body_vol_cache.use_adaptive_resolution = self.render.adaptive_resolution;
        // (VolumeAtlas rebuild-on-load REMOVED 2026-07-13 — the atlas is no longer created;
        // the saved atlas_xy_resolution is still restored for forward/backward file compat.)
        app.render.atlas_xy_resolution = self.render.atlas_xy_resolution;
        app.render.sdf_edge_aa_enabled = self.render.sdf_edge_aa_enabled;
        app.render.fxaa_enabled = self.render.fxaa_enabled;
        app.render.matcap_bg_color = self.render.matcap_bg_color;
        app.render.matcap_bg_gradient = self.render.matcap_bg_gradient;
        app.render.matcap_bg_gradient_top = self.render.matcap_bg_gradient_top;
        app.render.matcap_rotation = self.render.matcap_rotation;
        app.render.matcap_edge_enabled = self.render.matcap_edge_enabled;
        app.render.matcap_edge_color = self.render.matcap_edge_color;
        app.render.matcap_edge_thickness = self.render.matcap_edge_thickness;
        app.render.matcap_edge_threshold = self.render.matcap_edge_threshold;
        app.render.worker_gpu_enabled = self.render.worker_gpu_enabled.clone();
        // Apply loaded worker enabled state to live worker GPUs
        for w in &mut app.worker_gpus {
            if let Some((_, enabled)) = app.render.worker_gpu_enabled.iter()
                .find(|(name, _)| *name == w.adapter_name)
            {
                w.enabled = *enabled;
            }
        }
        if let Some(d) = &mut app.job_dispatcher {
            d.rebuild_enabled_workers(&app.worker_gpus);
        }

        // Editor
        app.viewport.snap_mode = self.editor.snap_mode;
        app.viewport.snap_grid = self.editor.snap_grid;
        app.viewport.snap_rotation_deg = self.editor.snap_rotation_deg;
        app.viewport.grid_cell_size = self.editor.grid_cell_size;
        app.viewport.grid_opacity = self.editor.grid_opacity;
        app.viewport.grid_visible = self.editor.grid_visible;
        app.viewport.wp_mode = self.editor.wp_mode;
        app.viewport.wp_settings = WorkPlaneSettings {
            spacing: self.editor.wp_settings.spacing,
            color: self.editor.wp_settings.color,
            opacity: self.editor.wp_settings.opacity,
            orientation: self.editor.wp_settings.orientation,
            origin: glam::Vec3::from_array(self.editor.wp_settings.origin),
        };
        app.viewport.wp_saved = self.editor.wp_saved.iter().map(|s| SavedWorkPlane {
            name: s.name.clone(),
            origin: glam::Vec3::from_array(s.origin),
            normal: glam::Vec3::from_array(s.normal),
        }).collect();
        // Locked workplane origin/normal (M88)
        app.viewport.wp_locked_origin = glam::Vec3::from_array(self.editor.wp_locked_origin);
        app.viewport.wp_locked_normal = glam::Vec3::from_array(self.editor.wp_locked_normal);

        // Restore serialized mesh data before regenerating missing display meshes.
        // Advance the allocator past every graph/sidecar ID first so a newly
        // allocated mesh cannot collide with a saved ID.
        let mut max_cache_id = self
            .imported_meshes
            .iter()
            .map(|rec| rec.cache_id)
            .max()
            .unwrap_or(0);
        if let Some(graph) = &self.scene_graph {
            for (_, node) in graph.all_nodes() {
                let cache_id = match &node.payload {
                    NodePayload::SdfBody { mesh_cache_id, .. }
                    | NodePayload::PolygonalBody { mesh_cache_id, .. }
                    | NodePayload::NurbsBody { mesh_cache_id, .. }
                    | NodePayload::NurbsSolid { mesh_cache_id, .. }
                    | NodePayload::VoxelBody { mesh_cache_id, .. }
                    | NodePayload::SdfObject { mesh_cache_id, .. } => *mesh_cache_id,
                    NodePayload::ImportedMesh { mesh_cache_id, .. } => Some(*mesh_cache_id),
                    _ => None,
                };
                if let Some(id) = cache_id {
                    max_cache_id = max_cache_id.max(id.0);
                }
            }
        }
        app.cache
            .mesh_cache
            .ensure_next_id_at_least(max_cache_id.saturating_add(1));
        for rec in &self.imported_meshes {
            let cid = core_math::scene_graph::MeshCacheId(rec.cache_id);
            app.cache.mesh_cache.insert_imported(
                cid,
                (rec.vertices.clone(), rec.indices.clone()),
                rec.triangle_count,
            );
        }

        // Scene graph (v2+)
        if let Some(ref graph) = self.scene_graph {
            app.scene.scene_graph = graph.clone();
            // FL4PIVT reattach BEFORE the pivot migration: flagged nodes are already
            // node-local (post-fix saves) and must not be converted a second time;
            // unflagged Custom pivots are legacy world-space and the migration converts
            // them — the exact old-file semantic the skip-default (false) encodes.
            for nid_raw in &self.pivot_local_nodes {
                let nid = core_math::scene_graph::NodeId(*nid_raw);
                if let Some(node) = app.scene.scene_graph.get_mut(nid) {
                    node.pivot.local = true;
                }
            }
            app.scene.scene_graph.migrate_custom_pivots_to_local();
            // The graph was just REPLACED, so every cache keyed by NodeId or mesh signature now
            // describes nodes that no longer exist — and the loaded scene can reuse the same
            // NodeIds for different geometry. `new_scene` has always dropped these; this path
            // dropped only `poly_custom_data` (below), which left `poly_mesh_gpu_key` able to
            // early-out the upload of a loaded mesh and `poly_display_key_memo` able to answer
            // with the PREVIOUS scene's cache key. Shared with `new_scene` so the two paths
            // cannot drift again. Must run BEFORE the display-mesh regeneration below.
            app.reset_derived_scene_caches();
            // v7: reattach voxel grids to their VoxelBody nodes (the node's grid is #[serde(skip)] →
            // loaded as a placeholder, replaced here from the side-channel by node_id). Old v6 files
            // have no voxel_grids → their (ImportedMesh) voxels load as static meshes (no auto-convert).
            for rec in &self.voxel_grids {
                if let Some(n) = app.scene.scene_graph.get_mut(NodeId(rec.node_id)) {
                    if let NodePayload::VoxelBody { grid, .. } = &mut n.payload {
                        *grid = core_math::voxel_sculpt::VoxelGrid::from_record(&rec.grid);
                    }
                }
            }
            // v7: regenerate each loaded VoxelBody's display mesh from its grid —
            // reuses restored sidecar entry when present; otherwise grid fallback.
            app.regen_voxel_display_meshes();
            // v8: reattach authored poly CustomData layers to their bodies via the runtime sidecar
            // store (cache.poly_custom_data), keyed by node_id — EXACTLY mirroring the voxel_grids
            // reattach above. A pre-v8 file has an empty poly_layers (#[serde(default)]) → no entries
            // added → every body loads with empty CustomData (forward-compat). Only nodes that are
            // PolygonalBodies get a sidecar (a stale node_id is silently skipped — never a crash).
            // HEAL pre-f2ab88e poly meshes: the old symmetrize/mirror left unpaired twin half-edges
            // (open "slits" — the surface looked closed but is_manifold failed, which silently killed
            // bevel via bevel_result_installable and broke edge loops). repair_unpaired_twins() is a
            // no-op on healthy meshes, so running it once at load retroactively fixes saved scenes.
            {
                let ids: Vec<NodeId> = app.scene.scene_graph.all_ids().collect();
                for nid in ids {
                    if let Some(n) = app.scene.scene_graph.get_mut(nid) {
                        if let NodePayload::PolygonalBody { mesh, .. } = &mut n.payload {
                            std::sync::Arc::make_mut(mesh).repair_unpaired_twins();
                        }
                    }
                }
            }
            app.cache.poly_custom_data.clear();
            for rec in &self.poly_layers {
                let nid = NodeId(rec.node_id);
                if matches!(app.scene.scene_graph.get(nid).map(|n| &n.payload),
                            Some(NodePayload::PolygonalBody { .. })) {
                    app.cache.poly_custom_data.insert(
                        nid,
                        core_math::poly_mesh::CustomData::from_layer_tuples(
                            &rec.uv,
                            &rec.crease,
                            &rec.seam,
                            &rec.sharp,
                            &rec.groups,
                            &rec.group_meta,
                        ),
                    );
                }
            }
            // FL4PNORM: reattach each body's authored Auto Smooth angle onto the sidecar. Runs
            // AFTER the poly_layers loop so it lands on the same `CustomData` (and creates one for
            // a body whose ONLY authored state is the angle). A stale node_id / non-poly node is
            // skipped, never a crash — same forward-tolerance as every other side-channel.
            for rec in &self.poly_normals {
                let nid = NodeId(rec.node_id);
                if matches!(app.scene.scene_graph.get(nid).map(|n| &n.payload),
                            Some(NodePayload::PolygonalBody { .. })) {
                    app.cache
                        .poly_custom_data
                        .entry(nid)
                        .or_insert_with(core_math::poly_mesh::CustomData::new)
                        .auto_smooth_deg = Some(rec.auto_smooth_deg.clamp(0.0, 180.0));
                }
            }
            // FL4PTAGS: reattach each body's Smart Select `op_created` tags onto the sidecar. Same
            // placement rule as FL4PNORM above — AFTER the poly_layers loop, so it lands on the
            // same `CustomData` (and creates one for a body whose ONLY authored state is tags).
            //
            // Without this the tags were SESSION-ONLY: every operation-created edge came back
            // untagged after a reload, and Smart Select — which fails closed — beveled them all
            // (`SIDECAR_SURVIVES_BOOLEAN.md` §OPEN, owner TEST_06). Ids are half-edge ids on the
            // SAVED base mesh, which is the mesh just loaded from the same file, so they reattach
            // directly. Out-of-range ids are dropped: `repair_unpaired_twins()` above can retire a
            // half-edge, and a tag must never land on a stranger.
            for rec in &self.poly_op_tags {
                let nid = NodeId(rec.node_id);
                let live = match app.scene.scene_graph.get(nid).map(|n| &n.payload) {
                    Some(NodePayload::PolygonalBody { mesh, .. }) => mesh.clone(),
                    _ => continue,
                };
                let cd = app
                    .cache
                    .poly_custom_data
                    .entry(nid)
                    .or_insert_with(core_math::poly_mesh::CustomData::new);
                for &h in &rec.op_created {
                    let he = core_math::half_edge::HalfEdgeId(h);
                    if live.is_edge_live(he) {
                        cd.op_created.set(he, true);
                    }
                }
            }
            // FL4PMODF: reattach per-poly-modifier #[serde(skip)] flags (Bevel Consume
            // Geometry) by (node_id, stack index). The stack itself came from the blob, so
            // the index is stable for this file. Dangling id / index / non-poly node →
            // skipped, never a crash — same forward-tolerance as every other side-channel.
            for rec in &self.poly_mod_flags {
                let nid = NodeId(rec.node_id);
                if let Some(n) = app.scene.scene_graph.get_mut(nid) {
                    if let NodePayload::PolygonalBody { modifiers, .. } = &mut n.payload {
                        if let Some(m) = modifiers.get_mut(rec.mod_index as usize) {
                            m.bevel_consume = rec.bevel_consume;
                        }
                    }
                }
            }
            // FL4PSMTH: reattach the Smooth mask feather, same keying and same forward-tolerance
            // as FL4PMODF above. Absent section → every Smooth keeps the default falloff.
            for rec in &self.poly_smooth {
                let nid = NodeId(rec.node_id);
                if let Some(n) = app.scene.scene_graph.get_mut(nid) {
                    if let NodePayload::PolygonalBody { modifiers, .. } = &mut n.payload {
                        if let Some(m) = modifiers.get_mut(rec.mod_index as usize) {
                            m.smooth_falloff = rec.smooth_falloff;
                            m.geo_channel.group_id = rec.group_id;
                            m.geo_channel.weight_min = rec.weight_min;
                            m.geo_channel.weight_max = rec.weight_max;
                        }
                    }
                }
            }
            // FL4PRMSH: reattach the Voxel Remesh params, same keying and same forward-tolerance
            // as FL4PMODF / FL4PSMTH above. Absent section → every modifier keeps the field
            // defaults, which is exactly the pre-FL4PRMSH behaviour (and was the bug: before this
            // section existed there was no carrier at all, so authored params were lost on load).
            for rec in &self.poly_remesh {
                let nid = NodeId(rec.node_id);
                if let Some(n) = app.scene.scene_graph.get_mut(nid) {
                    if let NodePayload::PolygonalBody { modifiers, .. } = &mut n.payload {
                        if let Some(m) = modifiers.get_mut(rec.mod_index as usize) {
                            m.remesh_resolution = rec.resolution;
                            m.remesh_smooth = rec.smooth;
                            m.remesh_padding = rec.padding;
                            m.remesh_project = rec.project;
                            m.remesh_union_islands = rec.union_islands;
                        }
                    }
                }
            }
            // FL4PBVAR: reattach bevel variable width, same keying and same forward-tolerance as
            // FL4PMODF / FL4PSMTH / FL4PRMSH above. Absent section → `bevel_var_width` stays 0,
            // which is the uniform-width bevel every pre-feature file described.
            for rec in &self.poly_bevel_var {
                let nid = NodeId(rec.node_id);
                if let Some(n) = app.scene.scene_graph.get_mut(nid) {
                    if let NodePayload::PolygonalBody { modifiers, .. } = &mut n.payload {
                        if let Some(m) = modifiers.get_mut(rec.mod_index as usize) {
                            m.bevel_var_width = rec.var_width;
                            m.bevel_width_min = rec.width_min;
                            m.bevel_auto_merge = rec.auto_merge;
                        }
                    }
                }
            }
            // FL4PBEND: reattach Bend params, same keying and same forward-tolerance as the
            // sections above. Absent section → the `bend_*` serde defaults stand, which for a Bend
            // modifier is the untouched 45°/X/full-band state that is exactly why no record was
            // written.
            for rec in &self.poly_bend {
                let nid = NodeId(rec.node_id);
                if let Some(n) = app.scene.scene_graph.get_mut(nid) {
                    if let NodePayload::PolygonalBody { modifiers, .. } = &mut n.payload {
                        if let Some(m) = modifiers.get_mut(rec.mod_index as usize) {
                            m.bend_angle_deg = rec.angle_deg;
                            m.bend_axis = rec.axis;
                            m.bend_dir_deg = rec.dir_deg;
                            m.bend_limit_lo = rec.limit_lo;
                            m.bend_limit_hi = rec.limit_hi;
                            m.bend_center = glam::Vec3::from_array(rec.center);
                        }
                    }
                }
            }
            for rec in &self.poly_sculpt {
                let nid = NodeId(rec.node_id);
                if let Some(n) = app.scene.scene_graph.get_mut(nid) {
                    if let NodePayload::PolygonalBody { modifiers, .. } = &mut n.payload {
                        if let Some(m) = modifiers.get_mut(rec.mod_index as usize) {
                            m.sculpt_delta_gen = rec.gen;
                            m.sculpt_deltas = rec
                                .deltas
                                .iter()
                                .map(|(i, d)| (*i, glam::Vec3::from_array(*d)))
                                .collect();
                        }
                    }
                }
            }
            for rec in &self.poly_remesh_collapse {
                let nid = NodeId(rec.node_id);
                if let Some(n) = app.scene.scene_graph.get_mut(nid) {
                    if let NodePayload::PolygonalBody { modifiers, .. } = &mut n.payload {
                        if let Some(m) = modifiers.get_mut(rec.mod_index as usize) {
                            m.remesh_collapse = rec.collapse;
                        }
                    }
                }
            }
            for rec in &self.poly_remesh_skip_relax {
                let nid = NodeId(rec.node_id);
                if let Some(n) = app.scene.scene_graph.get_mut(nid) {
                    if let NodePayload::PolygonalBody { modifiers, .. } = &mut n.payload {
                        if let Some(m) = modifiers.get_mut(rec.mod_index as usize) {
                            m.remesh_skip_relax_drag = rec.skip_relax_drag;
                        }
                    }
                }
            }
            for rec in &self.poly_remesh_skip_seal {
                let nid = NodeId(rec.node_id);
                if let Some(n) = app.scene.scene_graph.get_mut(nid) {
                    if let NodePayload::PolygonalBody { modifiers, .. } = &mut n.payload {
                        if let Some(m) = modifiers.get_mut(rec.mod_index as usize) {
                            m.remesh_skip_seal_drag = rec.skip_seal_drag;
                        }
                    }
                }
            }
            for rec in &self.poly_remesh_drop_res {
                let nid = NodeId(rec.node_id);
                if let Some(n) = app.scene.scene_graph.get_mut(nid) {
                    if let NodePayload::PolygonalBody { modifiers, .. } = &mut n.payload {
                        if let Some(m) = modifiers.get_mut(rec.mod_index as usize) {
                            m.remesh_drop_res_drag = rec.drop_res_drag;
                        }
                    }
                }
            }
            // FL4BAKE: reattach per-body bake settings. Absent section → defaults.
            // Dangling node ids are skipped (the body may have been deleted).
            app.ui.mesh_map_settings.clear();
            for rec in &self.bake_settings {
                let nid = NodeId(rec.node_id);
                if app.scene.scene_graph.get(nid).is_none() {
                    continue;
                }
                let s = crate::mesh_map_bake::BakeSettings {
                    enabled_maps: rec.enabled_maps,
                    res_global: rec.res_global,
                    supersample: rec.supersample,
                    padding_px: rec.padding_px,
                    source: crate::mesh_map_bake::BakeSource::from_wire(
                        rec.source_kind,
                        rec.high_nids.clone(),
                        rec.match_by_name,
                    ),
                    max_front: rec.max_front,
                    max_rear: rec.max_rear,
                    cage_nids: Vec::new(),
                    udim: false,
                    gpu_rays: false,
                    ao_rays: rec.ao_rays,
                    curv_radius: rec.curv_radius,
                    bit16: false,
                }
                .clamp();
                if !s.is_default() {
                    app.ui.mesh_map_settings.insert(nid, s);
                }
            }
            // G4: reconnect registry metadata to FL4MATTX slots. Missing pixels ⇒
            // stale badge, never drop the entry (Auto would silently go Live).
            app.ui.baked_images = crate::mesh_map_bake::reconnect_baked_images(
                &self.bake_images,
                |s| restored_ok_slots.contains(&s),
                |id| app.scene.scene_graph.get(NodeId(id)).is_some(),
            );
            for rec in &self.bake_bit16 {
                let nid = NodeId(rec.node_id);
                if app.scene.scene_graph.get(nid).is_none() {
                    continue;
                }
                if rec.bit16 == 0 {
                    continue;
                }
                let mut s = app.bake_settings_for(nid);
                s.bit16 = true;
                app.store_bake_settings(nid, s);
            }
            for rec in &self.bake_cage {
                let nid = NodeId(rec.node_id);
                if app.scene.scene_graph.get(nid).is_none() {
                    continue;
                }
                if rec.cage_nids.is_empty() {
                    continue;
                }
                let mut s = app.bake_settings_for(nid);
                s.cage_nids = rec.cage_nids.iter().copied().map(NodeId).collect();
                app.store_bake_settings(nid, s);
            }
            for rec in &self.bake_udim.flags {
                let nid = NodeId(rec.node_id);
                if app.scene.scene_graph.get(nid).is_none() {
                    continue;
                }
                if rec.udim == 0 {
                    continue;
                }
                let mut s = app.bake_settings_for(nid);
                s.udim = true;
                app.store_bake_settings(nid, s);
            }
            for rec in &self.bake_udim.tiles {
                if rec.udim < 1001 {
                    continue;
                }
                if let Some(e) = app
                    .ui
                    .baked_images
                    .images
                    .iter_mut()
                    .find(|e| e.name == rec.name)
                {
                    e.udim = rec.udim;
                }
            }
            for rec in &self.bake_gpu_rays {
                let nid = NodeId(rec.node_id);
                if app.scene.scene_graph.get(nid).is_none() {
                    continue;
                }
                if rec.gpu_rays == 0 {
                    continue;
                }
                let mut s = app.bake_settings_for(nid);
                s.gpu_rays = true;
                app.store_bake_settings(nid, s);
            }
            // Reattach body display-mesh modifier stacks (#[serde(skip)] on SdfBody).
            for rec in &self.body_mesh_modifiers {
                let nid = NodeId(rec.node_id);
                if let Some(n) = app.scene.scene_graph.get_mut(nid) {
                    if let NodePayload::SdfBody { mesh_modifiers, .. } = &mut n.payload {
                        *mesh_modifiers = rec.modifiers.clone();
                    }
                }
            }
            // Reattach body field stacks, then seed any body that still has hardwired
            // mirror/bevel but an empty stack (pre-P2 files).
            for rec in &self.body_field_modifiers {
                let nid = NodeId(rec.node_id);
                if let Some(n) = app.scene.scene_graph.get_mut(nid) {
                    if let NodePayload::SdfBody { field_modifiers, .. } = &mut n.payload {
                        *field_modifiers = rec.modifiers.clone();
                    }
                }
            }
            {
                let ids: Vec<NodeId> = app.scene.scene_graph.all_ids().collect();
                for nid in ids {
                    app.seed_and_sync_body_field_stack(nid);
                }
            }
            // Reattach parametric poly primitives (`PolygonalBody.source` is #[serde(skip)]).
            // A pre-parametric file has an empty `poly_params` → every poly body loads as
            // `Authored`, which is correct: its geometry never was derived from a spec.
            if !self.poly_params.is_empty() {
                app.poly_restore_param_records(&self.poly_params);
            }
            // P1+P2: reattach CurveExtrude shell/rim + open/thickness/tube (the variant fields are
            // #[serde(skip)], loaded as default) from the side-channel, keyed by prim_uid. A pre-P1
            // file has empty curve_params (#[serde(default)]) → every curve stays solid/closed
            // (forward-compat).
            if !self.curve_params.is_empty() {
                use std::collections::HashMap;
                let map: HashMap<u32, (f32, f32, bool, f32, bool)> =
                    self.curve_params.iter().map(|r| (r.prim_uid, (r.wall_t, r.rim, r.open, r.thickness, r.tube))).collect();
                let ids: Vec<NodeId> = app.scene.scene_graph.all_ids().collect();
                for nid in ids {
                    if let Some(n) = app.scene.scene_graph.get_mut(nid) {
                        if let NodePayload::SdfBody { primitives, .. } = &mut n.payload {
                            for p in primitives.iter_mut() {
                                if let core_math::primitives::SdfShape::CurveExtrude { wall_t, rim, open, thickness, tube, .. } = &mut p.shape {
                                    if let Some(&(w, r, o, t, tb)) = map.get(&p.prim_uid) {
                                        *wall_t = w; *rim = r; *open = o; *thickness = t; *tube = tb;
                                    }
                                }
                            }
                        }
                    }
                }
            }
            // Migrate v4 world-space prim positions to v5 body-local
            if BODY_LOCAL_PRIMS && self.version <= 4 {
                migrate_v4_to_v5(&mut app.scene.scene_graph);
            }
            // Unified child_order: apply any orders carried by the FL4ORDER side-channel (binary path),
            // then SELF-HEAL every body. Bodies the side-channel doesn't cover — a genuine v7 file, a
            // JSON file (child_order is #[serde(skip)] for JSON too), or a body that had the default
            // order — synthesise the legacy prims-then-bodies order, so old files bake IDENTICALLY.
            // `child_orders` is empty for v7/JSON → apply_body_child_orders is pure self-heal there.
            app.scene.scene_graph.apply_body_child_orders(&self.child_orders);
            // Rebuild flat primitives cache from scene graph
            app.sync_flat_cache();
            // If graph has Light nodes (v3+), lights are already in the graph — nothing to do.
            let has_graph_lights = app.scene.scene_graph.all_nodes()
                .any(|(_, n)| matches!(&n.payload, NodePayload::Light(_)));
            if !has_graph_lights {
                // Old file: migrate lights from serialized array into graph
                let root = app.scene.scene_graph.root();
                for l in &self.scene.lights {
                    let data = core_math::scene_graph::LightData {
                        id: l.id, name: l.name.clone(), light_type: l.light_type,
                        azimuth: l.azimuth, elevation: l.elevation,
                        intensity: l.intensity, color: l.color, ambient: l.ambient,
                        ambient_color: l.ambient_color, ambient_from_hdr: l.ambient_from_hdr,
                        position: glam::Vec3::from_array(l.position), range: l.range,
                        spot_inner_deg: l.spot_inner_deg, spot_outer_deg: l.spot_outer_deg,
                        shadow_softness: l.shadow_softness, visible: l.visible, locked: l.locked,
                        link_mode: l.link_mode.clone(), link_targets: l.link_targets.clone(),
                        light_group: l.light_group.clone(),
                        volumetric: Default::default(), // reattached from FL4LTVOL after graph build
                        rt_shadow: Default::default(),  // reattached from FL4LTRT after graph build
                    };
                    app.scene.scene_graph.add_child(root, NodePayload::Light(data));
                }
            }
        } else {
            // v1 file: need flat primitives so build_graph_from_flat can read them
            app.scene.primitives = self.scene.primitives.clone();
            app.build_graph_from_flat();
            // Migrate lights into graph
            let root = app.scene.scene_graph.root();
            for l in &self.scene.lights {
                let data = core_math::scene_graph::LightData {
                    id: l.id, name: l.name.clone(), light_type: l.light_type,
                    azimuth: l.azimuth, elevation: l.elevation,
                    intensity: l.intensity, color: l.color, ambient: l.ambient,
                    ambient_color: l.ambient_color, ambient_from_hdr: l.ambient_from_hdr,
                    position: glam::Vec3::from_array(l.position), range: l.range,
                    spot_inner_deg: l.spot_inner_deg, spot_outer_deg: l.spot_outer_deg,
                    shadow_softness: l.shadow_softness, visible: l.visible, locked: l.locked,
                    link_mode: l.link_mode.clone(), link_targets: l.link_targets.clone(),
                    light_group: l.light_group.clone(),
                    volumetric: Default::default(), // reattached from FL4LTVOL after graph build
                    rt_shadow: Default::default(),  // reattached from FL4LTRT after graph build
                };
                app.scene.scene_graph.add_child(root, NodePayload::Light(data));
            }
        }
        // Migrate legacy ChamferUnion/ChamferSubtract body_ops → Union/Subtract + Chamfer profile
        app.scene.scene_graph.normalize_body_ops();
        // Safety: recalculate IDs to prevent stale collisions after deserialization
        app.scene.scene_graph.recalculate_next_id();
        app.recalculate_next_prim_id();
        // Duplicate-light-id self-heal + next_light_id bump (heal calls the recalc):
        // old saves (incl. the former id-0 default light) can carry colliding light ids,
        // which make lights unreachable/undeletable via light_nid (first-match-by-id).
        app.heal_duplicate_light_ids();
        // Reattach per-light volumetric ("god ray") params from the FL4LTVOL tail, keyed by light id.
        // Runs AFTER heal_duplicate_light_ids so ids are unique/stable. Records for ids not present
        // (e.g. a deleted light) are silently skipped (forward-tolerant). Empty on every older file.
        for rec in &self.light_volumetrics {
            if let Some(l) = app.scene.light_by_id_mut(rec.light_id) {
                l.volumetric = core_math::scene_graph::LightVolumetric {
                    enabled: rec.enabled,
                    density: rec.density,
                    softness: rec.softness,
                    zone: rec.zone,
                };
            }
        }
        // Reattach per-light RAY-TRACED SHADOW params from the FL4LTRT tail (Stage 4), keyed by light
        // id. Same discipline as the volumetric reattach above — runs after heal, ids not present are
        // skipped. Absent on every pre-Stage-4 file → lights keep LightRtShadow::default() (enabled,
        // radius 0), i.e. they follow the global toggle exactly as Stage 3 did.
        for rec in &self.light_rt {
            if let Some(l) = app.scene.light_by_id_mut(rec.light_id) {
                l.rt_shadow = core_math::scene_graph::LightRtShadow {
                    enabled: rec.enabled,
                    radius: rec.radius,
                };
            }
        }
        app.recalculate_next_node_name_id();
        // Validate graph integrity
        let errors = app.scene.scene_graph.validate();
        if !errors.is_empty() {
            eprintln!("[WARNING] Scene graph validation after load ({} issues):", errors.len());
            for e in &errors { eprintln!("  - {}", e); }
        }
        // Point body nids at first body if any — do NOT force-create Body.001 on
        // an intentionally empty saved scene (parity with new_scene empty start).
        if let Some((nid, _)) = app.scene.scene_graph.sdf_bodies().next() {
            app.scene.sdf_body_nid = nid;
            if app.scene.scene_graph.body_primitives(app.scene.active_body_nid).is_none() {
                app.scene.active_body_nid = nid;
            }
        } else {
            app.scene.sdf_body_nid = core_math::scene_graph::NodeId(0);
            app.scene.active_body_nid = core_math::scene_graph::NodeId(0);
        }

        // Upload GPU buffers for imported meshes (CPU restore happened earlier)
        if let Some(gpu) = &app.gpu {
            app.cache.mesh_cache.upload_pending(&gpu.device, &gpu.queue);
        }

        // Compositor (v4+, defaults for older files)
        app.compositor = self.compositor.clone().unwrap_or_default();

        // Migrate mat_lib materials to graph Material nodes (v5 → v6).
        // If the graph already has Material nodes (v6+ file), skip migration.
        {
            let legacy_mat_lib = self.material_library.clone().unwrap_or_default();
            let has_graph_materials = app.scene.scene_graph.material_count() > 0;
            if !has_graph_materials && !legacy_mat_lib.materials.is_empty() {
                let root = app.scene.scene_graph.root();
                for sm in &legacy_mat_lib.materials {
                    let mid = core_math::primitives::MaterialId(sm.id);
                    app.scene.scene_graph.add_child(root, NodePayload::Material {
                        id: mid,
                        name: sm.mat.name.clone(),
                        data: sm.mat.clone(),
                    });
                }
                eprintln!("[Load] Migrated {} mat_lib materials to graph nodes", legacy_mat_lib.materials.len());
            }
            // NOTE (2026-07-17, owner refinement): no auto "Default Material" fallback anymore —
            // materials exist ONLY when explicitly added (Add → Material / migration / saved in the
            // file). Zero materials is a valid state everywhere: resolvers fall back to embedded
            // materials, the outliner renders no Materials section, combos offer "➕ New Material".
        }

        // Multi-map channels (FL4MATX2): reattach the `#[serde(skip)]` normal/roughness/metallic
        // TexMaps to their graph materials by MaterialId. Runs AFTER the graph install + mat_lib
        // migration so every referenced material node exists. The pixels these tex_ids point at are
        // restored by the FL4MATTX slot re-upload queued at the top of this function.
        apply_material_channels(&mut app.scene.scene_graph, &self.material_channels);

        // Reset ALL transient state
        app.sel = Default::default();
        app.sel.usel = Default::default();
        app.render.pt_accumulated_frames = 0;
        app.modeling_context = crate::ModelingContext::Sdf;
        app.tool.work_mode = crate::WorkMode::Object;
        app.poly_work_mode = crate::PolyWorkMode::Object;
        app.poly_sel = Default::default();
        app.tool.current = crate::ActiveTool::Idle;
        app.tool.mode = crate::ToolMode::Select;
        app.outliner = Default::default();
        app.history = crate::UndoHistory::new(app.history.max_entries);
        // Mesh Terrain: prefer durable side-channel (NodeId + lattice key); fall back to graph scan.
        // Never identity by outliner display name (rename must not drop tiles).
        app.apply_terrain_tiles_side_channel(&self.terrain_tiles);
        // Procedural node graphs: clear then reattach from FL4NODES **in the same load path**
        // before returning to the frame loop (rebuild reads only `ui.node_graphs` — see dirty.rs).
        // Order is mandatory: clear → reattach → shader_dirty (do not rebuild between clear and fill).
        app.ui.node_graphs.clear();
        app.ui.material_node_graphs.clear();
        reattach_node_graphs(app, &self.node_graphs);
        // Material panel docs (FL4MPNLS). Absent section (every older file) → empty store; the
        // panel derives docs on demand from PbrMaterial + graph (P0 migration, fail-soft).
        crate::material_panel::reattach_panel_docs(
            &mut app.ui.material_panel_docs,
            &self.material_panel_docs,
        );
        // G4 LOAD MIGRATION (R2: "read doc -> build graph -> from then on graph wins").
        // Every material with a stored doc gets its graph built/refreshed FROM the doc
        // ONCE, here — old files (Fast-only materials with no graph, pre-G4 graphs
        // without channel_meta/texture_space) become projection-backed on load. Sync
        // only: NO bake enqueues (N bakes at load was a measured freeze risk; the
        // block below still bakes at most one material when maps weren't restored).
        // The projection memo is cleared with the stores — a loaded scene's
        // material_generation could coincidentally match a stale entry.
        app.ui.mp_doc_memo.clear();
        {
            let mids: Vec<u32> = app.ui.material_panel_docs.keys().copied().collect();
            for mid_u in mids {
                app.mp_sync_doc_to_graph(core_math::primitives::MaterialId(mid_u));
            }
        }
        // PR-VIS: only re-bake if maps were NOT restored from FL4MATTX (already have pixels).
        // Spawning N full bakes on every load was a freeze risk; maps usually ride FL4MATTX.
        let maps_restored = app.cache.material_maps_restore.as_ref().map(|v| !v.is_empty()).unwrap_or(false);
        if !maps_restored {
            let mat_ids: Vec<u32> = app.ui.material_node_graphs.keys().copied().collect();
            // At most one material bake on load (async); user can re-Apply for others.
            if let Some(&mid) = mat_ids.first() {
                app.enqueue_material_node_graph_bake(core_math::primitives::MaterialId(mid));
            }
        }
        app.shader_dirty = true;
        app.mark_dirty(DirtyFlags::RENDER);
        app.scene.active_body_nid = app.scene.sdf_body_nid;
        app.scene.isolated_body_nid = None;
        app.render.viewport_mode = crate::ViewportRenderMode::Hybrid;
        // Texture / environment data — reset stale caches (HDR/matcap come from external files)
        app.env_hdr_data.clear();
        app.env_hdr_mip_data.clear();
        app.env_hdr_width = 0;
        app.env_hdr_height = 0;
        // env_ibl: invalidate in-flight env workers + swap the GPU buffer in poll
        // (Frame Ordering — this runs from a panel/menu path).
        app.env_load_gen = app.env_load_gen.wrapping_add(1);
        app.env_prefilter_queue.clear();
        app.env_clear_pending = true;
        // Rotation is authored env state (persisted via FL4ENVIR, applied by
        // restore_env_from_record AFTER this reset). A file with no env record must not
        // inherit the previous session's rotation.
        app.render.env_rotation_deg = 0.0;
        app.matcap_data.clear();
        app.matcap_width = 0;
        app.matcap_height = 0;
        app.tex_atlas_data.clear();
        app.tex_atlas_meta.clear();
        app.tex_atlas_next_id = 1;
        app.tex_atlas_dirty = false;
        app.cache.rebake_hash = 0;
        app.cache.body_vol_cache.invalidate_all();
        // Pre-sync resolution/bounds so trigger_body_vol_bake → set_params
        // sees matching values and skips the bulk re-invalidation + mark_all_bodies_dirty.
        {
            let res = app.cache.cpu_volume.resolution[0];
            let bounds = app.cache.cpu_volume.grid_bounds;
            app.cache.body_vol_cache.pre_sync_params(res, bounds);
        }
        app.cache.use_body_vol_cache = true;
        // Bool edge cache
        app.bool_edges.clear();
        app.bool_edge_hovered = None;
        app.bool_edge_dirty = true;
        // Tool / sculpt / macro transient state
        app.cache.suppress_body_vol_dirty = false;
        app.cache.body_vol_cache.suppress_bake_dispatch = false;
        app.poly_tool = crate::PolyTool::Idle;
        app.knife_points.clear();
        app.knife_snap_pos = None;
        app.sdf_sculpt_active = false;
        app.sdf_brush_preview_pos = None;
        // WORLD-space point from the outgoing scene — meaningless in the loaded one.
        app.poly_prop_origin = None;
        app.poly_prop_handle_hovered = false;
        app.sculpt.stroke_active = false;
        app.sculpt.current_stroke.clear();
        // CANCEL any in-flight ASYNC Move bake + discard its pending stack (review P2): the whole
        // pre-load scene is being replaced — landing the bake after this point would write into the
        // surviving pre-load grid and push an undo entry onto the freshly-reset stacks below.
        app.voxel_cancel_move_bake();
        app.voxel_undo_reset();
        app.macro_recording = false;
        app.macro_buffer.clear();
        app.nurbs_work_mode = crate::NurbsWorkMode::Object;
        app.bevel_pv = Default::default();

        // CR-70 FIX: Do NOT set DirtyFlags::SCENE here — that triggers
        // process_dirty → trigger_body_vol_bake → trigger_bake for ALL bodies
        // in a single frame, causing a GPU work burst that exceeds the ~2s TDR timeout.
        // Instead: set the prim buffer flags manually and stagger body bakes
        // over multiple frames (2 bodies/frame via post_load_warmup in app_poll).
        app.cache.prim_buf_stale = true;
        app.cache.prim_buf_dirty_body = None; // structural change → full serialize
        app.mark_dirty(DirtyFlags::LIGHT | DirtyFlags::RENDER | DirtyFlags::MESH);
        // Collect body NodeIds for staggered bake dispatch
        let body_nids: Vec<_> = app.scene.scene_graph.sdf_bodies()
            .map(|(nid, _)| nid).collect();
        let body_count = body_nids.len();
        app.cache.post_load_bodies = body_nids;
        // Warmup frames: enough to drain all bodies at 2/frame + a few extra
        app.cache.post_load_warmup = ((body_count as u32 + 1) / 2 + 3).max(3);
        // Crash diagnostic: log post-load state for debugging silent crashes
        eprintln!("[ProjectLoad] complete — bodies={} prims={} active_body={:?} gpu={} cached_prim_bytes={} warmup_frames={}",
            body_count,
            app.scene.primitives.len(),
            app.scene.active_body_nid,
            app.gpu.is_some(),
            app.cache.cached_prim_bytes.len(),
            app.cache.post_load_warmup);
    }
}

impl AzraApp {
    /// Open scene: spawn the native file picker on a background thread
    /// so the render loop keeps running (CR-70b: prevents TDR from blocking
    /// the main thread for seconds while the file dialog is open).
    pub(crate) fn open_scene(&mut self) {
        // Don't spawn a second dialog if one is already open
        if self.project.pending_file_open.is_some() { return; }
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let result = rfd::FileDialog::new()
                .set_title("Open Scene")
                .add_filter("AZRA Scene", &["fl4"])
                .add_filter("JSON Scene", &["json"])
                .add_filter("All Files", &["*"])
                .pick_file();
            let _ = tx.send(result);
        });
        self.project.pending_file_open = Some(rx);
    }

    /// Poll the background file-open dialog. Called once per frame from update().
    pub(crate) fn poll_pending_file_open(&mut self) {
        let rx = match &self.project.pending_file_open {
            Some(rx) => rx,
            None => return,
        };
        match rx.try_recv() {
            Ok(Some(path)) => {
                self.project.pending_file_open = None;
                self.load_scene_from_path(&path);
            }
            Ok(None) => {
                // User cancelled the file dialog
                self.project.pending_file_open = None;
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {
                // Dialog still open — keep polling
            }
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                // Thread died without sending — shouldn't happen
                self.project.pending_file_open = None;
            }
        }
    }

    /// Load `path` as the startup template.
    ///
    /// Unlike `load_scene_from_path`, this does not bind `project_path`, does not
    /// add a recent file, and leaves the session untitled. `Ok(false)` means the
    /// file is absent (empty start is correct). `Err` is the real IO or decode
    /// error; the caller toasts it and the in-memory scene is unchanged because
    /// `apply_to_app` runs only after a successful read.
    pub(crate) fn load_startup_scene_from(&mut self, path: &Path) -> Result<bool, String> {
        if !path.exists() {
            return Ok(false);
        }
        if !path.is_file() {
            return Err(format!("{} is not a file", path.display()));
        }
        let project = load_binary(path)?;
        project.apply_to_app(self);
        self.restore_env_from_record(project.env.as_ref(), path.parent());
        // Template, not the current document. Ctrl+S stays Save As.
        self.project.project_path = None;
        self.project.scene_modified = false;
        self.project.last_auto_save = std::time::Instant::now();
        Ok(true)
    }

    /// Read `%LOCALAPPDATA%/Azra/default_scene.fl4` when it exists.
    /// Returns true only when a scene was applied. Failures toast the real error
    /// and leave the empty startup scene in place.
    pub(crate) fn load_default_scene_at_startup(&mut self) -> bool {
        let path = match crate::project::default_scene_path() {
            Ok(path) => path,
            Err(error) => {
                let msg = format!("Open default scene failed: {error}");
                eprintln!("[DefaultScene] {msg}");
                self.toast(&msg);
                return false;
            }
        };
        match self.load_startup_scene_from(&path) {
            Ok(loaded) => {
                if loaded {
                    eprintln!("[DefaultScene] loaded {}", path.display());
                }
                loaded
            }
            Err(error) => {
                let msg = format!("Open default scene failed: {error}");
                eprintln!("[DefaultScene] {msg}");
                self.toast(&msg);
                false
            }
        }
    }

    pub(crate) fn load_scene_from_path(&mut self, path: &Path) {
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("fl4");
        let result = if ext == "json" {
            load_json(path)
        } else {
            load_binary(path)
        };
        match result {
            Ok(project) => {
                let t_load = std::time::Instant::now();
                project.apply_to_app(self);
                // Per-project environment (FL4ENVIR): runs AFTER apply_to_app — the gen bump +
                // env clear it queued make the kicked worker the sole valid load. Relative paths
                // resolve against THIS file's directory; a missing file toasts + keeps the
                // default env (never fails the load).
                self.restore_env_from_record(project.env.as_ref(), path.parent());
                let load_ms = t_load.elapsed().as_secs_f64() * 1000.0;
                eprintln!("[ProjectLoad] apply_to_app: {:.1}ms", load_ms);
                self.project.project_path = Some(path.to_path_buf());
                self.project.scene_modified = false;
                self.project.last_auto_save = std::time::Instant::now();
                self.add_recent_file(path.to_path_buf());
                let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("scene");
                self.toast(&format!("Opened: {}", name));
            }
            Err(e) => {
                // Console too, not only a toast — headless harnesses (AZRA_OPEN) and crash
                // triage need the load error on stderr (a swallowed toast cost a debugging
                // detour on 2026-08-18).
                eprintln!("[ProjectLoad] OPEN FAILED for {}: {e}", path.display());
                self.toast(&format!("Open failed: {}", e));
            }
        }
    }
}
