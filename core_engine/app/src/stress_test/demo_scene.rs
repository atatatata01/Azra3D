//! D1/D4 demo game-level scene — `AZRA_DEMO_SCENE=1` (or `=N` trees).
//!
//! Game-level landscape: mountains, canyon cliffs, gravel, rocks, 5 tree types,
//! 4 grass clusters, high-res shadows + RT shadows, volumetric fog, full PBR maps.
//! Deterministic LCG. B.4 shared meshes per family.

use crate::*;
use core_math::primitives::PbrMaterial;
use core_math::scene_graph::{LightType, NodePayload};
use std::sync::Arc;

// Deterministic scattering via the suite's shared LCG (`super::Lcg`) — same scene every run.
use super::Lcg;

// ── Terrain field (v4): rolling base + ridged mountains + carved canyon ──────────
// Everything (mesh, tree/rock/grass placement, texture blending) reads these pure
// fns, so the world is self-consistent and identical every run.

fn hash01(ix: i32, iz: i32) -> f32 {
    let mut h = (ix as i64).wrapping_mul(374761393) ^ (iz as i64).wrapping_mul(668265263);
    h = (h ^ (h >> 13)).wrapping_mul(1274126177);
    ((h ^ (h >> 16)) & 0xffff) as f32 / 65535.0
}

fn value_noise(x: f32, z: f32) -> f32 {
    let (ix, iz) = (x.floor() as i32, z.floor() as i32);
    let (fx, fz) = (x - x.floor(), z - z.floor());
    let (sx, sz) = (fx * fx * (3.0 - 2.0 * fx), fz * fz * (3.0 - 2.0 * fz));
    let a = hash01(ix, iz);
    let b = hash01(ix + 1, iz);
    let c = hash01(ix, iz + 1);
    let d = hash01(ix + 1, iz + 1);
    a + (b - a) * sx + (c - a) * sz + (a - b - c + d) * sx * sz
}

fn fbm(x: f32, z: f32, octaves: u32) -> f32 {
    let (mut f, mut amp, mut sum, mut norm) = (1.0f32, 1.0f32, 0.0f32, 0.0f32);
    for _ in 0..octaves {
        sum += value_noise(x * f, z * f) * amp;
        norm += amp;
        f *= 2.03;
        amp *= 0.5;
    }
    sum / norm.max(1e-6)
}

/// Ridged multifractal — sharp crests instead of round blobs (real mountains).
fn ridged(x: f32, z: f32, octaves: u32) -> f32 {
    let (mut f, mut amp, mut sum, mut norm) = (1.0f32, 1.0f32, 0.0f32, 0.0f32);
    for _ in 0..octaves {
        let n = 1.0 - (value_noise(x * f, z * f) * 2.0 - 1.0).abs();
        sum += n * n * amp;
        norm += amp;
        f *= 2.07;
        amp *= 0.5;
    }
    sum / norm.max(1e-6)
}

fn smoothstep(a: f32, b: f32, t: f32) -> f32 {
    let t = ((t - a) / (b - a)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Canyon centre-line X at a given Z (a winding river course).
fn canyon_center_x(z: f32) -> f32 {
    (z * 0.017).sin() * 38.0 + (z * 0.0061).cos() * 16.0
}

/// Signed lateral distance from the canyon centre-line.
fn canyon_dist(x: f32, z: f32) -> f32 {
    (x - canyon_center_x(z)).abs()
}

/// World height at (x, z). Rolling floor + mountain range (ridged, masked to the
/// outer band) − a carved canyon with a flat gravel bed through the middle.
pub(crate) fn terrain_height(x: f32, z: f32) -> f32 {
    let base = (fbm(x * 0.011, z * 0.011, 3) - 0.5) * 9.0;
    let detail = (fbm(x * 0.09, z * 0.09, 2) - 0.5) * 1.6;
    // Mountains rise toward the rim (radial mask) — peaks ~70 units.
    let r = (x * x + z * z).sqrt();
    let mtn_mask = smoothstep(52.0, 118.0, r);
    let mountains = ridged(x * 0.0075, z * 0.0075, 5).powf(1.35) * 78.0 * mtn_mask;
    let mut h = base + detail + mountains;
    // Canyon: steep walls, flat bed, only across the inner basin.
    let d = canyon_dist(x, z);
    let half_w = 17.0;
    if d < half_w {
        let t = d / half_w; // 0 centre … 1 rim
        let carve = (1.0 - t * t).powf(0.75) * 30.0 * (1.0 - mtn_mask * 0.55);
        h -= carve;
        if d < 5.0 {
            // gravel bed — flatten the last few units
            let bed = h;
            h = bed * 0.25 + (bed - 1.0) * 0.75;
        }
    }
    h
}

/// Gradient magnitude of the height field (≈ tan of slope angle).
pub(crate) fn terrain_slope(x: f32, z: f32) -> f32 {
    const E: f32 = 1.25;
    let hx = terrain_height(x + E, z) - terrain_height(x - E, z);
    let hz = terrain_height(x, z + E) - terrain_height(x, z - E);
    ((hx / (2.0 * E)).powi(2) + (hz / (2.0 * E)).powi(2)).sqrt()
}

// ── Procedural material maps (albedo + normal + roughness) ──────────────────────
// Real texture-array pressure + per-pixel normal/roughness sampling, with zero
// asset files. Uploaded through the production `PendingMapLoad` path so the maps
// exercise exactly the runtime the file loader feeds. TRIPLANAR projection: needs
// only position+normal, so clustered bodies (whose DAG pools carry uv=0) texture
// correctly too — the D3 uv gap does not bite here.

#[derive(Clone, Copy, PartialEq)]
pub(crate) enum TexStyle {
    Rock,
    Cliff,
    Gravel,
    Bark,
    Foliage,
    DarkFoliage,
    AutumnFoliage,
    Ground,
    Meadow,
    DryGrass,
    TallGrass,
}

/// Style-specific surface height in [0,1] over a tiling uv — drives both the
/// normal map (derivative) and the albedo variation, so they always agree.
fn style_height(style: TexStyle, u: f32, v: f32) -> f32 {
    let (su, sv) = (u * 8.0, v * 8.0);
    match style {
        // Blocky stone with cracks: ridged noise gives sharp crevices.
        TexStyle::Rock | TexStyle::Cliff => {
            let crack = ridged(su * 1.7, sv * 1.7, 3);
            fbm(su * 3.0, sv * 3.0, 3) * 0.55 + crack * 0.45
        }
        TexStyle::Gravel => {
            let pebble = ridged(su * 22.0, sv * 22.0, 3).powf(2.2);
            fbm(su * 8.0, sv * 8.0, 2) * 0.4 + pebble * 0.6
        }
        // Vertical fibrous bark: stretched noise + banding along v.
        TexStyle::Bark => {
            let fibre = fbm(su * 9.0, sv * 0.9, 3);
            let band = ((su * 22.0).sin() * 0.5 + 0.5) * 0.35;
            fibre * 0.65 + band
        }
        // Leaf clumps: clustered blobs.
        TexStyle::Foliage | TexStyle::DarkFoliage | TexStyle::AutumnFoliage => {
            let clump = fbm(su * 5.0, sv * 5.0, 2);
            let leaf = ridged(su * 13.0, sv * 13.0, 2);
            clump * 0.6 + leaf * 0.4
        }
        // Soil + pebbles.
        TexStyle::Ground | TexStyle::Meadow => {
            let soil = fbm(su * 6.0, sv * 6.0, 4);
            let pebble = ridged(su * 17.0, sv * 17.0, 2).powf(3.0);
            soil * 0.75 + pebble * 0.25
        }
        TexStyle::DryGrass | TexStyle::TallGrass => {
            let blade = ((su * 40.0).sin().abs() * (sv * 12.0).sin().abs()).sqrt();
            fbm(su * 4.0, sv * 4.0, 2) * 0.5 + blade * 0.5
        }
    }
}

/// Base + accent colour per style (linear-ish sRGB bytes).
fn style_colors(style: TexStyle) -> ([f32; 3], [f32; 3]) {
    match style {
        TexStyle::Rock => ([0.46, 0.44, 0.42], [0.24, 0.22, 0.21]),
        TexStyle::Cliff => ([0.38, 0.36, 0.34], [0.18, 0.16, 0.15]),
        TexStyle::Gravel => ([0.52, 0.48, 0.40], [0.32, 0.28, 0.22]),
        TexStyle::Bark => ([0.34, 0.23, 0.14], [0.16, 0.10, 0.06]),
        TexStyle::Foliage => ([0.16, 0.40, 0.15], [0.07, 0.22, 0.08]),
        TexStyle::DarkFoliage => ([0.08, 0.22, 0.10], [0.04, 0.12, 0.05]),
        TexStyle::AutumnFoliage => ([0.55, 0.32, 0.08], [0.35, 0.12, 0.04]),
        TexStyle::Ground => ([0.36, 0.40, 0.24], [0.20, 0.17, 0.11]),
        TexStyle::Meadow => ([0.22, 0.48, 0.18], [0.12, 0.30, 0.10]),
        TexStyle::DryGrass => ([0.48, 0.42, 0.18], [0.32, 0.28, 0.10]),
        TexStyle::TallGrass => ([0.18, 0.42, 0.16], [0.10, 0.28, 0.08]),
    }
}

/// (albedo, normal, roughness) RGBA8 square maps. Normal + roughness come back
/// PRE-ENCODED for the sRGB array (the data-map contract).
fn gen_style_maps(style: TexStyle, side: usize) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
    let n = side * side * 4;
    let (mut albedo, mut normal, mut rough) = (vec![255u8; n], vec![255u8; n], vec![255u8; n]);
    let (base, accent) = style_colors(style);
    let inv = 1.0 / side as f32;
    let e = inv; // derivative step = one texel
    for y in 0..side {
        for x in 0..side {
            let (u, v) = (x as f32 * inv, y as f32 * inv);
            let h = style_height(style, u, v);
            let i = (y * side + x) * 4;
            // Albedo: base→accent by height, plus a little hue drift.
            let drift = value_noise(u * 24.0, v * 24.0) * 0.12 - 0.06;
            for c in 0..3 {
                let col = base[c] * h + accent[c] * (1.0 - h) + drift;
                albedo[i + c] = (col.clamp(0.0, 1.0) * 255.0) as u8;
            }
            // Normal: tangent-space from the height derivative.
            let dhdu = (style_height(style, u + e, v) - style_height(style, u - e, v)) / (2.0 * e);
            let dhdv = (style_height(style, u, v + e) - style_height(style, u, v - e)) / (2.0 * e);
            let strength = match style {
                TexStyle::Rock | TexStyle::Cliff => 0.06,
                TexStyle::Gravel => 0.08,
                TexStyle::Bark => 0.05,
                TexStyle::Foliage | TexStyle::DarkFoliage | TexStyle::AutumnFoliage => 0.035,
                TexStyle::Ground | TexStyle::Meadow => 0.04,
                TexStyle::DryGrass | TexStyle::TallGrass => 0.03,
            };
            let (nx, ny, nz) = (-dhdu * strength, -dhdv * strength, 1.0);
            let len = (nx * nx + ny * ny + nz * nz).sqrt().max(1e-6);
            normal[i] = ((nx / len * 0.5 + 0.5) * 255.0) as u8;
            normal[i + 1] = ((ny / len * 0.5 + 0.5) * 255.0) as u8;
            normal[i + 2] = ((nz / len * 0.5 + 0.5) * 255.0) as u8;
            // Roughness: cavities rougher than peaks (+ per-style floor).
            let r = match style {
                TexStyle::Rock | TexStyle::Cliff => 0.62 + (1.0 - h) * 0.32,
                TexStyle::Gravel => 0.78 + (1.0 - h) * 0.18,
                TexStyle::Bark => 0.70 + (1.0 - h) * 0.25,
                TexStyle::Foliage | TexStyle::DarkFoliage | TexStyle::AutumnFoliage => {
                    0.55 + (1.0 - h) * 0.30
                }
                TexStyle::Ground | TexStyle::Meadow => 0.75 + (1.0 - h) * 0.20,
                TexStyle::DryGrass | TexStyle::TallGrass => 0.65 + (1.0 - h) * 0.25,
            };
            let rb = (r.clamp(0.0, 1.0) * 255.0) as u8;
            rough[i] = rb;
            rough[i + 1] = rb;
            rough[i + 2] = rb;
        }
    }
    crate::material_textures::encode_data_map_in_place(&mut normal);
    crate::material_textures::encode_data_map_in_place(&mut rough);
    (albedo, normal, rough)
}

/// Demo-scene primitive builder.
///
/// PORTED at the 2026-08-05 merge round: `poly_build_mesh` + the flat `PolyCreateAdjust` bag
/// (`kind: String` + segments/rings/radius/height) were replaced by the typed `PrimitiveSpec`
/// when parametric primitives landed. The four kinds this scene uses are mapped to the exact
/// constructors the old string dispatch reached, so the demo geometry is unchanged:
///   box      -> `new_box(splat(r))`      — BoxSpec.size is FULL extent, hence `r * 2.0`
///   sphere   -> `new_uv_sphere(r, seg, rings.max(2))`
///   cylinder -> `new_cylinder(r, h, seg)` (the old vsegments was always 1 here)
///   cone     -> `new_cone(r, h, seg)`     — a point, i.e. `radius_top = 0`
fn build(kind: &str, segments: u32, rings: u32, radius: f32, height: f32) -> Option<core_math::half_edge::HalfEdgeMesh> {
    use core_math::primitive_spec::{
        BoxSpec, ConeSpec, CylinderSpec, PrimitiveSpec, UvSphereSpec,
    };
    let seg = segments.clamp(3, 256);
    let rings = rings.clamp(1, 256);
    let r = radius.max(0.001);
    let h = height.max(0.001);
    let spec = match kind {
        "box" => PrimitiveSpec::Box(BoxSpec { size: Vec3::splat(r * 2.0), ..Default::default() }),
        "sphere" => PrimitiveSpec::UvSphere(UvSphereSpec {
            radius: r,
            segments: seg,
            rings: rings.max(2),
            ..Default::default()
        }),
        "cylinder" => PrimitiveSpec::Cylinder(CylinderSpec {
            radius: r,
            height: h,
            sides: seg,
            vsegments: 1,
            ..Default::default()
        }),
        "cone" => PrimitiveSpec::Cone(ConeSpec {
            radius_bottom: r,
            radius_top: 0.0,
            height: h,
            sides: seg,
            vsegments: 1,
            ..Default::default()
        }),
        _ => return None,
    };
    Some(spec.build())
}

/// D2 C6 — interior occlusion gate scene (`AZRA_DEMO_INTERIOR=1`): an 8×8 grid of
/// walled rooms, each stuffed with instanced clutter, camera INSIDE the center room.
/// Nearly everything is behind a wall → the HZB gate demands ≥90% of the
/// occlusion-eligible (in-frustum) instances culled (`occCull=a/b` on the scoreboard).
pub(crate) fn spawn_demo_interior_scene(app: &mut AzraApp) -> Vec<String> {
    let mut results = vec!["=== Interior occlusion scene (8x8 rooms) ===".into()];
    app.tool.work_mode = WorkMode::Object;
    app.poly_work_mode = PolyWorkMode::Object;
    app.modeling_context = ModelingContext::Polygonal;
    let root = app.scene.scene_graph.root();
    let mk_mat = |rgb: [f32; 3]| {
        let mut m = PbrMaterial::default();
        m.albedo = Vec3::new(rgb[0], rgb[1], rgb[2]);
        m.roughness = 0.9;
        m
    };
    let mut spawn = |app: &mut AzraApp, name: &str, mesh: Arc<core_math::half_edge::HalfEdgeMesh>, material: PbrMaterial, placements: &[(Vec3, Vec3)]| {
        let mut shared_cid = None;
        for (i, (pos, scale)) in placements.iter().enumerate() {
            let nid = app.scene.scene_graph.add_child(
                root,
                NodePayload::PolygonalBody {
                    name: format!("{name}.{i:03}"),
                    mesh: mesh.clone(),
                    mesh_cache_id: shared_cid,
                    material: material.clone(),
                    modifiers: Vec::new(),
                    // Demo-scene mesh built from a spec but not kept live-parametric.
                    source: core_math::primitive_spec::MeshSource::Authored,
                },
            );
            if let Some(node) = app.scene.scene_graph.get_mut(nid) {
                node.local_position = *pos;
                node.local_scale = *scale;
            }
            app.poly_sync_mesh_cache(nid);
            if shared_cid.is_none() {
                shared_cid = match app.scene.scene_graph.get(nid).map(|n| &n.payload) {
                    Some(NodePayload::PolygonalBody { mesh_cache_id: Some(c), .. }) => Some(*c),
                    _ => None,
                };
            }
        }
    };
    let Some(wall) = build("box", 1, 1, 0.5, 1.0) else {
        results.push("FAIL: wall mesh".into());
        return results;
    };
    let wall = Arc::new(wall);
    let Some(clutter) = build("sphere", 24, 16, 0.8, 2.0) else {
        results.push("FAIL: clutter mesh".into());
        return results;
    };
    // 8×8 rooms, 10 units apart, solid 4-wall + ceiling boxes; ~40 clutter spheres each.
    const ROOMS: i32 = 8;
    const PITCH: f32 = 10.0;
    let mut rng = Lcg::new(0x1273_0035);
    let mut walls = Vec::new();
    let mut things = Vec::new();
    for rz in 0..ROOMS {
        for rx in 0..ROOMS {
            let cx = (rx - ROOMS / 2) as f32 * PITCH;
            let cz = (rz - ROOMS / 2) as f32 * PITCH;
            // floor-to-ceiling walls (leave a doorway gap on +X so rooms connect)
            walls.push((Vec3::new(cx, 2.0, cz - PITCH * 0.5), Vec3::new(PITCH, 4.0, 0.4)));
            walls.push((Vec3::new(cx - PITCH * 0.5, 2.0, cz), Vec3::new(0.4, 4.0, PITCH)));
            walls.push((Vec3::new(cx, 4.2, cz), Vec3::new(PITCH, 0.4, PITCH))); // ceiling
            for _ in 0..40 {
                let s = rng.range_f32(0.2, 0.6);
                things.push((
                    Vec3::new(
                        cx + rng.range_f32(-4.0, 4.0),
                        s + rng.range_f32(0.0, 2.5),
                        cz + rng.range_f32(-4.0, 4.0),
                    ),
                    Vec3::new(s, s, s),
                ));
            }
        }
    }
    spawn(app, "Wall", wall.clone(), mk_mat([0.6, 0.58, 0.55]), &walls);
    spawn(app, "Floor", wall, mk_mat([0.4, 0.4, 0.42]), &[(Vec3::new(0.0, -0.25, 0.0), Vec3::new(ROOMS as f32 * PITCH, 0.5, ROOMS as f32 * PITCH))]);
    spawn(app, "Clutter", Arc::new(clutter), mk_mat([0.7, 0.4, 0.3]), &things);
    let has_sun = app.scene.scene_graph.lights().any(|(_, l)| l.light_type == LightType::Directional);
    if !has_sun {
        app.add_light(LightType::Directional);
    }
    // Camera inside the center room, eye height.
    app.cache.poly_lod_auto = false;
    app.deselect_all();
    app.viewport.camera.target = Vec3::new(0.0, 1.6, 0.0);
    app.viewport.camera.distance = 3.0;
    app.draw_stream_stale = true;
    app.structural_draw_stale = true;
    app.ensure_structural_draw_stream();
    app.mark_dirty(DirtyFlags::SCENE | DirtyFlags::MESH | DirtyFlags::RENDER);
    app.render.viewport_mode = ViewportRenderMode::Hybrid;
    results.push(format!(
        "{} walls + {} clutter — streamN={} (gate: occCull a/b ≥ 0.90 while orbiting inside)",
        walls.len(),
        things.len(),
        app.draw_stream.len()
    ));
    results
}
/// Poll step for the demo harness: once a family's SEED body has its uploaded map
/// slots (assigned a frame after the `PendingMapLoad` drains), copy those `TexMap`s
/// onto the rest of the family. One texture upload per map; every sibling material
/// just points at the same array slot. No-op unless a demo scene queued groups.
pub(crate) fn poll_demo_tex_propagate(app: &mut AzraApp) {
    if app.cache.demo_tex_groups.is_empty() {
        return;
    }
    use core_math::pbr_material::MapKind;
    const KINDS: [MapKind; 3] = [MapKind::Albedo, MapKind::Normal, MapKind::Roughness];
    let groups = std::mem::take(&mut app.cache.demo_tex_groups);
    let mut still_waiting = Vec::new();
    let mut propagated = 0usize;
    for (seed, rest) in groups {
        // Seed ready ⇔ the albedo slot was assigned (0 = unset sentinel).
        let maps = match app.scene.scene_graph.get(seed).map(|n| &n.payload) {
            Some(NodePayload::PolygonalBody { material, .. })
                if material.map(MapKind::Albedo).tex_id != 0 =>
            {
                Some([
                    *material.map(MapKind::Albedo),
                    *material.map(MapKind::Normal),
                    *material.map(MapKind::Roughness),
                ])
            }
            _ => None,
        };
        let Some(maps) = maps else {
            still_waiting.push((seed, rest));
            continue;
        };
        for nid in &rest {
            if let Some(NodePayload::PolygonalBody { material, .. }) =
                app.scene.scene_graph.get_mut(*nid).map(|n| &mut n.payload)
            {
                for (k, m) in KINDS.iter().zip(maps.iter()) {
                    *material.map_mut(*k) = *m;
                }
                propagated += 1;
            }
        }
    }
    app.cache.demo_tex_groups = still_waiting;
    if propagated > 0 {
        // Structural list caches RasterMaterial bytes — force a re-gather so the
        // new slots reach the GPU material buffer.
        app.structural_draw_stale = true;
        app.mark_dirty(DirtyFlags::SCENE | DirtyFlags::RENDER);
        eprintln!("[DemoTex] propagated map slots to {propagated} bodies");
    }
}

/// Spawn game-level landscape: mountains, cliffs, gravel, 5 trees, 4 grass types,
/// full PBR, high-res / RT shadows, volumetric fog.
/// `trees` scales population (default 400).
pub(crate) fn spawn_demo_terrain_scene(app: &mut AzraApp, trees: usize) -> Vec<String> {
    let mut results = vec![format!("=== Demo game-level v5 (trees={trees}) ===")];
    app.tool.work_mode = WorkMode::Object;
    app.poly_work_mode = PolyWorkMode::Object;
    app.modeling_context = ModelingContext::Polygonal;
    let root = app.scene.scene_graph.root();

    // Material: colour + roughness live here; the maps arrive via PendingMapLoad.
    // TRIPLANAR (projection bit0 = 0) with a world-space tile size, so clustered
    // bodies (uv=0 DAG pools) and source meshes texture identically.
    let mat_mapped = |rgb: [f32; 3], rough: f32, tile: f32| {
        let mut m = PbrMaterial::default();
        m.albedo = Vec3::new(rgb[0], rgb[1], rgb[2]);
        m.roughness = rough;
        for kind in [
            core_math::pbr_material::MapKind::Albedo,
            core_math::pbr_material::MapKind::Normal,
            core_math::pbr_material::MapKind::Roughness,
        ] {
            let map = m.map_mut(kind);
            map.uv_scale = tile;
            map.projection = 0; // triplanar
            map.strength = 1.0;
        }
        m
    };
    let mat_plain = |rgb: [f32; 3], rough: f32| {
        let mut m = PbrMaterial::default();
        m.albedo = Vec3::new(rgb[0], rgb[1], rgb[2]);
        m.roughness = rough;
        m
    };

    // Families whose bodies share ONE mesh (B.4) and ONE texture set. Returns the
    // spawned node ids so the caller can wire the texture group.
    let mut spawn_family = |app: &mut AzraApp,
                            name: &str,
                            mesh: Arc<core_math::half_edge::HalfEdgeMesh>,
                            material: PbrMaterial,
                            placements: &[(Vec3, Vec3)]|
     -> Vec<core_math::scene_graph::NodeId> {
        let mut shared_cid = None;
        let mut ids = Vec::with_capacity(placements.len());
        for (i, (pos, scale)) in placements.iter().enumerate() {
            let nid = app.scene.scene_graph.add_child(
                root,
                NodePayload::PolygonalBody {
                    name: format!("{name}.{i:04}"),
                    mesh: mesh.clone(),
                    mesh_cache_id: shared_cid,
                    material: material.clone(),
                    modifiers: Vec::new(),
                    // Demo-scene mesh built from a spec but not kept live-parametric.
                    source: core_math::primitive_spec::MeshSource::Authored,
                },
            );
            if let Some(node) = app.scene.scene_graph.get_mut(nid) {
                node.local_position = *pos;
                node.local_scale = *scale;
            }
            app.poly_sync_mesh_cache(nid);
            if shared_cid.is_none() {
                shared_cid = match app.scene.scene_graph.get(nid).map(|n| &n.payload) {
                    Some(NodePayload::PolygonalBody { mesh_cache_id: Some(c), .. }) => Some(*c),
                    _ => None,
                };
            }
            ids.push(nid);
        }
        ids
    };

    // ── Meshes (each family = one shared high-res mesh) ──────────────────────────
    const HALF: f32 = 130.0;
    // Terrain: 420² quads ≈ 353k tris — dense enough to be CLUSTERED (>50k).
    let terrain_mesh = {
        const N: usize = 420;
        let step = (HALF * 2.0) / N as f32;
        let mut positions = Vec::with_capacity((N + 1) * (N + 1));
        for iz in 0..=N {
            for ix in 0..=N {
                let x = -HALF + ix as f32 * step;
                let z = -HALF + iz as f32 * step;
                positions.push(Vec3::new(x, terrain_height(x, z), z));
            }
        }
        let mut faces = Vec::with_capacity(N * N);
        for iz in 0..N {
            for ix in 0..N {
                let a = iz * (N + 1) + ix;
                faces.push(vec![a, a + 1, a + N + 2, a + N + 1]);
            }
        }
        core_math::half_edge::HalfEdgeMesh::from_polygons(&positions, &faces)
    };
    let Some(sphere_hi) = build("sphere", 48, 32, 0.8, 2.0) else {
        results.push("FAIL: sphere mesh".into());
        return results;
    };
    let sphere_hi = Arc::new(sphere_hi);
    let Some(cone_hi) = build("cone", 24, 1, 0.6, 1.0) else {
        results.push("FAIL: cone mesh".into());
        return results;
    };
    let cone_hi = Arc::new(cone_hi);
    let Some(trunk_m) = build("cylinder", 12, 1, 0.12, 1.0) else {
        results.push("FAIL: trunk mesh".into());
        return results;
    };
    let trunk_m = Arc::new(trunk_m);
    let Some(grass_m) = build("cone", 5, 1, 0.05, 0.5) else {
        results.push("FAIL: grass mesh".into());
        return results;
    };
    let grass_tuft = Arc::new(grass_m);
    let Some(cone_med) = build("cone", 16, 1, 0.55, 1.0) else {
        results.push("FAIL: cone med".into());
        return results;
    };
    let cone_med = Arc::new(cone_med);
    let Some(trunk_thin) = build("cylinder", 10, 1, 0.08, 1.2) else {
        results.push("FAIL: thin trunk".into());
        return results;
    };
    let trunk_thin = Arc::new(trunk_thin);
    let Some(cliff_m) = build("box", 1, 1, 0.5, 1.0) else {
        results.push("FAIL: cliff box".into());
        return results;
    };
    let cliff_m = Arc::new(cliff_m);
    let Some(grass_blade) = build("cone", 4, 1, 0.03, 0.85) else {
        results.push("FAIL: grass blade".into());
        return results;
    };
    let grass_blade = Arc::new(grass_blade);
    let Some(grass_clump) = build("cone", 6, 1, 0.08, 0.4) else {
        results.push("FAIL: grass clump".into());
        return results;
    };
    let grass_clump = Arc::new(grass_clump);
    let Some(grass_patch) = build("sphere", 8, 6, 0.15, 0.2) else {
        results.push("FAIL: grass patch".into());
        return results;
    };
    let grass_patch = Arc::new(grass_patch);

    // ── Populations, all seated on the field and slope/altitude aware ────────────
    let n = trees.max(1);
    let mut rng = Lcg::new(0xD15CE4E);
    let treeline = 34.0f32;

    let mut conifer_trunks = Vec::new();
    let mut conifer_tiers = Vec::new();
    let mut broad_trunks = Vec::new();
    let mut broad_canopies = Vec::new();
    let mut pine_trunks = Vec::new();
    let mut pine_tiers = Vec::new();
    let mut willow_trunks = Vec::new();
    let mut willow_canopies = Vec::new();
    let mut dead_trunks = Vec::new();
    let mut dead_snags = Vec::new();
    let mut placed = 0usize;
    let mut attempts = 0usize;
    while placed < n && attempts < n * 40 {
        attempts += 1;
        let x = rng.range_f32(-HALF * 0.86, HALF * 0.86);
        let z = rng.range_f32(-HALF * 0.86, HALF * 0.86);
        let h = terrain_height(x, z);
        let slope = terrain_slope(x, z);
        // Trees avoid cliffs, the canyon bed and the bare peaks.
        if slope > 0.85 || h > treeline || canyon_dist(x, z) < 9.0 {
            continue;
        }
        // Thin out with altitude and steepness.
        let density = (1.0 - (h / treeline).clamp(0.0, 1.0) * 0.55) * (1.0 - slope * 0.5);
        if rng.range_f32(0.0, 1.0) > density {
            continue;
        }
        placed += 1;
        let roll = rng.range_f32(0.0, 1.0);
        let alt = (h / treeline).clamp(0.0, 1.0);
        // 5 tree types by altitude + roll
        if alt > 0.55 || (alt > 0.35 && roll < 0.45) {
            let s = rng.range_f32(0.9, 2.1) * (1.0 - alt * 0.35);
            conifer_trunks.push((Vec3::new(x, h + 0.5 * s, z), Vec3::new(s, s, s)));
            for (y, w) in [(1.5, 2.5), (2.5, 1.9), (3.4, 1.15)] {
                conifer_tiers.push((Vec3::new(x, h + y * s, z), Vec3::new(w * s, 1.6 * s, w * s)));
            }
        } else if roll < 0.22 {
            let s = rng.range_f32(1.0, 2.4);
            pine_trunks.push((
                Vec3::new(x, h + 0.55 * s, z),
                Vec3::new(s * 0.85, s * 1.15, s * 0.85),
            ));
            for (y, w) in [(1.8, 1.6), (2.9, 1.1), (3.8, 0.7)] {
                pine_tiers.push((Vec3::new(x, h + y * s, z), Vec3::new(w * s, 1.3 * s, w * s)));
            }
        } else if roll < 0.40 {
            let s = rng.range_f32(0.7, 1.6);
            willow_trunks.push((
                Vec3::new(x, h + 0.5 * s, z),
                Vec3::new(s * 0.9, s * 1.1, s * 0.9),
            ));
            willow_canopies.push((
                Vec3::new(x, h + 2.0 * s, z),
                Vec3::new(2.4 * s, 1.2 * s, 2.4 * s),
            ));
            willow_canopies.push((
                Vec3::new(x + 0.4 * s, h + 1.6 * s, z - 0.3 * s),
                Vec3::new(1.6 * s, 1.0 * s, 1.6 * s),
            ));
        } else if roll < 0.52 {
            let s = rng.range_f32(0.8, 1.9);
            dead_trunks.push((
                Vec3::new(x, h + 0.55 * s, z),
                Vec3::new(s * 0.7, s * 1.2, s * 0.7),
            ));
            if rng.range_f32(0.0, 1.0) < 0.55 {
                dead_snags.push((
                    Vec3::new(x + 0.3 * s, h + 1.8 * s, z),
                    Vec3::new(0.5 * s, 0.35 * s, 0.5 * s),
                ));
            }
        } else {
            let s = rng.range_f32(0.8, 1.8);
            broad_trunks.push((Vec3::new(x, h + 0.6 * s, z), Vec3::new(s, 1.4 * s, s)));
            broad_canopies.push((
                Vec3::new(x, h + 2.5 * s, z),
                Vec3::new(1.7 * s, 1.5 * s, 1.7 * s),
            ));
        }
    }

    // Rocks: scree on steep ground, boulders in the canyon bed, scatter elsewhere.
    let n_rock_target = (n * 3).clamp(600, 24_000);
    let mut rocks = Vec::with_capacity(n_rock_target);
    let mut ra = 0usize;
    while rocks.len() < n_rock_target && ra < n_rock_target * 20 {
        ra += 1;
        let x = rng.range_f32(-HALF * 0.92, HALF * 0.92);
        let z = rng.range_f32(-HALF * 0.92, HALF * 0.92);
        let h = terrain_height(x, z);
        let slope = terrain_slope(x, z);
        let in_canyon = canyon_dist(x, z) < 15.0;
        let chance = if in_canyon { 0.9 } else { 0.18 + slope * 0.55 };
        if rng.range_f32(0.0, 1.0) > chance {
            continue;
        }
        let s = if in_canyon && rng.range_f32(0.0, 1.0) < 0.12 {
            rng.range_f32(1.8, 5.0) // canyon boulders
        } else {
            rng.range_f32(0.12, 0.9)
        };
        rocks.push((
            Vec3::new(x, h + s * 0.30, z),
            Vec3::new(
                s * rng.range_f32(0.7, 1.5),
                s * rng.range_f32(0.4, 0.95),
                s * rng.range_f32(0.7, 1.5),
            ),
        ));
    }

    // Cliffs on steep faces
    let n_cliff_target = (n / 2).clamp(80, 2_500);
    let mut cliffs = Vec::with_capacity(n_cliff_target);
    let mut ca = 0usize;
    while cliffs.len() < n_cliff_target && ca < n_cliff_target * 40 {
        ca += 1;
        let x = rng.range_f32(-HALF * 0.9, HALF * 0.9);
        let z = rng.range_f32(-HALF * 0.9, HALF * 0.9);
        let h = terrain_height(x, z);
        let slope = terrain_slope(x, z);
        if slope < 1.1 || h < 12.0 {
            continue;
        }
        let s = rng.range_f32(1.5, 5.5);
        cliffs.push((
            Vec3::new(x, h + s * 0.35, z),
            Vec3::new(s * rng.range_f32(0.8, 2.2), s * rng.range_f32(1.2, 2.8), s * 0.35),
        ));
    }

    // Gravel bed in canyon
    let n_gravel_target = (n * 2).clamp(400, 12_000);
    let mut gravel = Vec::with_capacity(n_gravel_target);
    let mut ga_g = 0usize;
    while gravel.len() < n_gravel_target && ga_g < n_gravel_target * 25 {
        ga_g += 1;
        let z = rng.range_f32(-HALF * 0.9, HALF * 0.9);
        let cx = canyon_center_x(z);
        let x = cx + rng.range_f32(-6.5, 6.5);
        if canyon_dist(x, z) > 7.0 {
            continue;
        }
        let h = terrain_height(x, z);
        let s = rng.range_f32(0.08, 0.45);
        gravel.push((
            Vec3::new(x, h + s * 0.2, z),
            Vec3::new(s * rng.range_f32(0.8, 1.6), s * 0.35, s * rng.range_f32(0.8, 1.6)),
        ));
    }

    // 4 grass cluster types
    let n_grass_each = (n * 12).clamp(1_200, 50_000);
    let mut meadow = Vec::with_capacity(n_grass_each);
    let mut dry = Vec::with_capacity(n_grass_each);
    let mut tall = Vec::with_capacity(n_grass_each);
    let mut tuft = Vec::with_capacity(n_grass_each);
    for bucket in [&mut meadow, &mut dry, &mut tall, &mut tuft] {
        let mut ga = 0usize;
        while bucket.len() < n_grass_each && ga < n_grass_each * 10 {
            ga += 1;
            let x = rng.range_f32(-HALF * 0.8, HALF * 0.8);
            let z = rng.range_f32(-HALF * 0.8, HALF * 0.8);
            let h = terrain_height(x, z);
            if terrain_slope(x, z) > 0.7 || h > treeline * 0.95 || canyon_dist(x, z) < 6.0 {
                continue;
            }
            let s = rng.range_f32(0.45, 1.6);
            bucket.push((Vec3::new(x, h + 0.22 * s, z), Vec3::new(s, s, s)));
        }
    }

    // Clouds above the range.
    let mut clouds = Vec::new();
    for _ in 0..14 {
        let x = rng.range_f32(-HALF, HALF);
        let z = rng.range_f32(-HALF, HALF);
        let y = rng.range_f32(78.0, 104.0);
        let s = rng.range_f32(12.0, 26.0);
        clouds.push((Vec3::new(x, y, z), Vec3::new(s, s * 0.32, s * rng.range_f32(0.6, 0.9))));
        clouds.push((
            Vec3::new(x + s * 0.5, y + 1.6, z + s * 0.2),
            Vec3::new(s * 0.6, s * 0.24, s * 0.5),
        ));
    }

    // ── Spawn + texture groups ──────────────────────────────────────────────────
    let mut tex_jobs: Vec<(TexStyle, Vec<core_math::scene_graph::NodeId>)> = Vec::new();

    let terrain_ids = spawn_family(
        app,
        "Terrain",
        Arc::new(terrain_mesh),
        mat_mapped([0.34, 0.38, 0.24], 0.95, 6.0),
        &[(Vec3::ZERO, Vec3::ONE)],
    );
    // Two-track: heightfield is clipmap-eligible, not cluster-DAG. Register the
    // property at spawn (block_cid) — do not rely on the body name string.
    if let Some(&nid) = terrain_ids.first() {
        if let Some(NodePayload::PolygonalBody {
            mesh_cache_id: Some(c),
            ..
        }) = app.scene.scene_graph.get(nid).map(|n| &n.payload)
        {
            let cid = *c;
            app.cache.cluster_bake.block_cid(cid);
        }
    }
    tex_jobs.push((TexStyle::Ground, terrain_ids));

    let mut rock_ids = spawn_family(
        app,
        "Rock",
        sphere_hi.clone(),
        mat_mapped([0.48, 0.46, 0.44], 0.95, 1.1),
        &rocks,
    );
    let n_rock = rock_ids.len();

    // Cliffs + gravel before trees
    let cliff_ids = spawn_family(
        app,
        "Cliff",
        cliff_m,
        mat_mapped([0.40, 0.38, 0.36], 0.92, 2.5),
        &cliffs,
    );
    let n_cliff = cliff_ids.len();
    tex_jobs.push((TexStyle::Cliff, cliff_ids));

    let gravel_ids = spawn_family(
        app,
        "Gravel",
        sphere_hi.clone(),
        mat_mapped([0.50, 0.46, 0.38], 0.9, 0.8),
        &gravel,
    );
    let n_gravel = gravel_ids.len();
    tex_jobs.push((TexStyle::Gravel, gravel_ids));

    let mut bark_ids = spawn_family(
        app,
        "Conifer.Trunk",
        trunk_m.clone(),
        mat_mapped([0.33, 0.22, 0.13], 0.9, 0.7),
        &conifer_trunks,
    );
    bark_ids.extend(spawn_family(
        app,
        "Broad.Trunk",
        trunk_m.clone(),
        mat_mapped([0.36, 0.25, 0.15], 0.9, 0.7),
        &broad_trunks,
    ));
    bark_ids.extend(spawn_family(
        app,
        "Pine.Trunk",
        trunk_thin.clone(),
        mat_mapped([0.30, 0.20, 0.12], 0.92, 0.65),
        &pine_trunks,
    ));
    bark_ids.extend(spawn_family(
        app,
        "Willow.Trunk",
        trunk_thin,
        mat_mapped([0.38, 0.28, 0.16], 0.88, 0.7),
        &willow_trunks,
    ));
    bark_ids.extend(spawn_family(
        app,
        "Dead.Trunk",
        trunk_m,
        mat_mapped([0.28, 0.24, 0.20], 0.95, 0.8),
        &dead_trunks,
    ));
    tex_jobs.push((TexStyle::Bark, bark_ids));

    let mut foliage_ids = spawn_family(
        app,
        "Conifer",
        cone_hi,
        mat_mapped([0.11, 0.31, 0.14], 0.85, 1.4),
        &conifer_tiers,
    );
    foliage_ids.extend(spawn_family(
        app,
        "Broadleaf",
        sphere_hi.clone(),
        mat_mapped([0.19, 0.43, 0.17], 0.8, 1.4),
        &broad_canopies,
    ));
    tex_jobs.push((TexStyle::Foliage, foliage_ids));

    tex_jobs.push((
        TexStyle::DarkFoliage,
        spawn_family(
            app,
            "Pine",
            cone_med,
            mat_mapped([0.07, 0.20, 0.09], 0.88, 1.3),
            &pine_tiers,
        ),
    ));
    tex_jobs.push((
        TexStyle::AutumnFoliage,
        spawn_family(
            app,
            "Willow",
            sphere_hi.clone(),
            mat_mapped([0.48, 0.30, 0.08], 0.82, 1.5),
            &willow_canopies,
        ),
    ));
    // Dead snags use rock maps (dry gray wood)
    rock_ids.extend(spawn_family(
        app,
        "Dead.Snag",
        sphere_hi.clone(),
        mat_mapped([0.32, 0.28, 0.24], 0.95, 1.0),
        &dead_snags,
    ));

    // 4 grass types with full mapped materials
    let n_meadow = spawn_family(
        app,
        "Grass.Meadow",
        grass_clump,
        mat_mapped([0.24, 0.48, 0.18], 0.88, 0.9),
        &meadow,
    );
    tex_jobs.push((TexStyle::Meadow, n_meadow.clone()));
    let n_dry = spawn_family(
        app,
        "Grass.Dry",
        grass_tuft.clone(),
        mat_mapped([0.46, 0.40, 0.16], 0.9, 0.85),
        &dry,
    );
    tex_jobs.push((TexStyle::DryGrass, n_dry.clone()));
    let n_tall = spawn_family(
        app,
        "Grass.Tall",
        grass_blade,
        mat_mapped([0.18, 0.42, 0.15], 0.86, 0.7),
        &tall,
    );
    tex_jobs.push((TexStyle::TallGrass, n_tall.clone()));
    let n_tuft = spawn_family(
        app,
        "Grass.Tuft",
        grass_patch,
        mat_mapped([0.28, 0.44, 0.18], 0.9, 1.0),
        &tuft,
    );
    // reuse meadow maps for tuft via Ground-ish - use Meadow style on tuft
    tex_jobs.push((TexStyle::Meadow, n_tuft.clone()));
    let n_grass = n_meadow.len() + n_dry.len() + n_tall.len() + n_tuft.len();

    let cloud_ids = spawn_family(
        app,
        "Cloud",
        sphere_hi,
        mat_plain([0.93, 0.94, 0.96], 0.6),
        &clouds,
    );
    let n_cloud = cloud_ids.len();
    rock_ids.shrink_to_fit();
    tex_jobs.push((TexStyle::Rock, rock_ids));

    // Queue ONE texture set per family: the seed body gets the PendingMapLoad, the
    // rest of the family inherits the assigned array slot in poll (one upload per
    // map, thousands of materials pointing at it).
    for (style, ids) in tex_jobs {
        let Some((&seed, rest)) = ids.split_first() else { continue };
        let (albedo, normal, rough) = gen_style_maps(style, 1024);
        for (kind, pixels) in [
            (core_math::pbr_material::MapKind::Albedo, albedo),
            (core_math::pbr_material::MapKind::Normal, normal),
            (core_math::pbr_material::MapKind::Roughness, rough),
        ] {
            app.cache.material_map_loads.push(crate::material_textures::PendingMapLoad {
                target: crate::material_textures::MapTarget::BodyMaterial(seed),
                kind,
                pixels,
                strength: 1.0,
                name: "DemoProcedural".into(),
                high_precision: false,
                pixels_lo: Vec::new(),
                // Procedurally generated against the shader's historical (DirectX) green read.
                green_dx: kind == core_math::pbr_material::MapKind::Normal,
            });
        }
        if !rest.is_empty() {
            app.cache.demo_tex_groups.push((seed, rest.to_vec()));
        }
    }

    // Sun (only if the scene has none) + volumetric fog.
    let has_sun = app
        .scene
        .scene_graph
        .lights()
        .any(|(_, l)| l.light_type == LightType::Directional);
    if !has_sun {
        app.add_light(LightType::Directional);
    }
    app.render.fog_enabled = true;
    app.render.fog_density = 0.014;
    app.render.fog_height_falloff = 0.08;
    app.render.fog_scatter = 0.92;
    app.render.fog_max_distance = 280.0;
    // High-res cascade shadows + RT shadows for cinematic game-level look
    app.render.shadow_map_resolution = 2048;
    app.render.rt_shadows = true;
    app.render.shadow_pcf_softness = 0.65;

    app.cache.poly_lod_auto = false;
    app.deselect_all();
    app.viewport.camera.target = Vec3::new(0.0, 6.0, 0.0);
    app.viewport.camera.distance = 120.0;

    app.draw_stream_stale = true;
    app.structural_draw_stale = true;
    app.ensure_structural_draw_stream();
    app.mark_dirty(DirtyFlags::SCENE | DirtyFlags::MESH | DirtyFlags::RENDER);
    app.render.viewport_mode = ViewportRenderMode::Hybrid;

    results.push(format!(
        "game-level v5 | terrain 420² (~353k tris) + cliffs + gravel | trees: conifer={} pine={} broad={} willow={} dead={} |          rocks={n_rock} cliffs={n_cliff} gravel={n_gravel} grass={n_grass} clouds={n_cloud} |          sun+RT shadows 2k + volumetric fog | full PBR maps — streamN={}",
        conifer_trunks.len(),
        pine_trunks.len(),
        broad_trunks.len(),
        willow_trunks.len(),
        dead_trunks.len(),
        app.draw_stream.len()
    ));

    results
}
