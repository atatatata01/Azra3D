//! Mesh Terrain — Phase 1 island + Phase 2 resident-window scaffold (ADR-TERRAIN-001).
//!
//! Source of truth for tile size / dense res used by Create Terrain Island and
//! multi-tile streaming. See `docs/planning/TERRAIN_MESH_TERRAIN_ADR.md`
//! and `docs/planning/TERRAIN_SDF_VOXEL_REMESH_PLAN.md`.
//!
//! Phase 1 rules (locked):
//! - Dense tile only, res = [`DENSE_TILE_RES`] (128³).
//! - Pure SDF base (no height / image displace — L4).
//! - Volume = truth; display mesh from existing bake + chunked DC path.
//!
//! Phase 2 (scaffold):
//! - [`TerrainWorld`] tracks island group + tile map + camera resident window.
//! - Tiles are created on demand; non-resident tiles set `SceneNode.visible = false`
//!   (mesh/volume stay in `.fl4` — L6 monolithic until a tile store exists).

use std::collections::{HashMap, HashSet};

use glam::Vec3;
use core_math::primitives::{BooleanOp, BoxBevel, PbrMaterial, SdfPrimitive, SdfShape, BlendProfile};
use core_math::scene_graph::{NodeId, NodePayload, SceneGraph};

use crate::{DirtyFlags, FourthLayerApp, SceneItemId};

// ---------------------------------------------------------------------------
// L7 budgets (ADR-TERRAIN-001 / plan §12.9)
// ---------------------------------------------------------------------------

/// World-space tile edge length in meters (XY). Phase 1 island spans one tile.
pub const TILE_METERS: f32 = 64.0;

/// Dense bake resolution per tile axis. Only remeshable/exportable path.
/// 128³ ≈ 8.4 MB CPU grid; 256³ ≈ 67 MB (§12.9).
pub const DENSE_TILE_RES: u32 = 128;

/// Draft bake resolution while interacting (existing draft → promote path).
pub const DRAFT_TILE_RES: u32 = 64;

/// Near-ring bake res (Chebyshev dist = 1 from stream centre). Phase 3 near/far.
pub const RING1_TILE_RES: u32 = 64;

/// Far-ring bake res (Chebyshev dist ≥ 2). Still remeshable; coarser for fly-speed.
pub const RING2_TILE_RES: u32 = 32;

/// Target max resident tiles for Phase 2 (5×5). Documented early for HUD/planning.
pub const MAX_RESIDENT_TILES: u32 = 25;

/// Bake resolution for a resident tile at Chebyshev distance `dist` from the stream centre.
///
/// Phase 3 near/far quality: camera tile stays full dense; outer rings drop res so a
/// 5×5 refill stays within per-frame bake/DC caps (plan §12.9 fly-speed criterion).
#[inline]
pub fn terrain_res_for_chebyshev(dist: i32) -> u32 {
    match dist.abs() {
        0 => DENSE_TILE_RES,
        1 => RING1_TILE_RES,
        _ => RING2_TILE_RES,
    }
}

/// Min time between residency center switches (camera thrash / boundary jitter).
pub const STREAM_CENTER_COOLDOWN_MS: u64 = 300;

/// Prefer stay on current tile until camera is this many meters closer to the new
/// tile origin than the old (boundary deadband). ~⅜ of a half-tile at 64 m.
pub const STREAM_SWITCH_DEADBAND_M: f32 = 12.0;

/// Cell size implied by tile meters / dense res (~0.5 m at defaults).
#[inline]
pub fn terrain_cell_size() -> f32 {
    TILE_METERS / DENSE_TILE_RES as f32
}

/// World-space SW corner of tile `(tx, tz)` on the infinite lattice (L2).
/// Y is always 0 — height band is free inside the volume, lattice XY is world-locked.
#[inline]
pub fn tile_origin_world(tx: i32, tz: i32) -> Vec3 {
    Vec3::new(tx as f32 * TILE_METERS, 0.0, tz as f32 * TILE_METERS)
}

/// Body origin for tile `(tx, tz)` — Phase 1 centred the 0_0 tile at world origin,
/// so additional tiles are offset by integer multiples of [`TILE_METERS`].
#[inline]
pub fn tile_body_origin(tx: i32, tz: i32) -> Vec3 {
    Vec3::new(tx as f32 * TILE_METERS, 0.0, tz as f32 * TILE_METERS)
}

/// Tile indices containing a world XZ point (floor division).
/// Tile 0_0 covers XZ ∈ [-half, +half) when body origin is 0 (Phase 1 convention),
/// so we offset by half-extent before flooring.
#[inline]
pub fn world_to_tile(x: f32, z: f32) -> (i32, i32) {
    let h = tile_half_extent();
    (
        ((x + h) / TILE_METERS).floor() as i32,
        ((z + h) / TILE_METERS).floor() as i32,
    )
}

/// Keys of a square resident window centred on `center` with Chebyshev radius `half`.
/// `half=1` → 3×3 (9), `half=2` → 5×5 (25 = [`MAX_RESIDENT_TILES`]).
pub fn desired_resident_tiles(center: (i32, i32), half: i32) -> Vec<(i32, i32)> {
    let half = half.max(0);
    let mut out = Vec::with_capacity(((2 * half + 1) as usize).pow(2));
    for tz in (center.1 - half)..=(center.1 + half) {
        for tx in (center.0 - half)..=(center.0 + half) {
            out.push((tx, tz));
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Phase 2 runtime manager (not a NodePayload — D5)
// ---------------------------------------------------------------------------

/// Background terrain display remesh result (rayon → poll).
pub(crate) struct TerrainRemeshJobResult {
    pub body: NodeId,
    pub cid: core_math::scene_graph::MeshCacheId,
    pub source_stamp: u64,
    pub job_gen: u64,
    pub ms: f32,
    /// None = no surface / remesh failed (still clears in-flight).
    pub raster: Option<(Vec<[f32; 8]>, Vec<u32>, u32)>,
}

/// In-flight remesh job (one body at a time per body id).
pub(crate) struct PendingTerrainRemeshJob {
    pub body: NodeId,
    pub job_gen: u64,
    pub rx: std::sync::mpsc::Receiver<TerrainRemeshJobResult>,
}

/// Cloneable mesh payload for the remesh worker (no mesh_cache borrow across threads).
enum TerrainRemeshInput {
    Quads {
        vertices: Vec<Vec3>,
        normals: Vec<Vec3>,
        quads: Vec<[u32; 4]>,
    },
    Soup {
        positions: Vec<Vec3>,
        normals: Vec<Vec3>,
        tris: Vec<[u32; 3]>,
    },
}

/// App-side terrain stream state. Tiles are existing SdfBody nodes under a Group.
#[derive(Debug, Clone, Default)]
pub struct TerrainWorld {
    /// Island group (None until first Terrain Island create).
    pub island_group: Option<NodeId>,
    /// `(tx, tz)` → SdfBody node.
    pub tiles: HashMap<(i32, i32), NodeId>,
    /// Chebyshev half-window (1 = 3×3, 2 = 5×5). Default 1 for cheap Phase 2 tryouts.
    pub half_window: i32,
    /// Last **committed** residency centre (after cooldown + deadband).
    pub last_center: Option<(i32, i32)>,
    /// When the residency centre last changed (hysteresis).
    pub last_switch_at: Option<std::time::Instant>,
    /// When true, each frame updates visibility/residency around the camera.
    /// **Never creates tiles from the poll loop** — growth is Patch / Expand only.
    pub stream_enabled: bool,
    /// Settle remesh queue (D3): bodies waiting for post-bake display remesh.
    ///
    /// Value = mesh `content_stamp` when requested (0 if no mesh yet). Spawn only after
    /// stamp **advances** (fresh DC). Work runs on rayon — never blocks LMB.
    /// Runtime-only — not serialized.
    pub pending_display_remesh: HashMap<NodeId, u64>,
}

impl TerrainWorld {
    pub fn new() -> Self {
        Self {
            half_window: 1,
            ..Default::default()
        }
    }

    pub fn register_tile(&mut self, tx: i32, tz: i32, body: NodeId, group: NodeId) {
        self.island_group = Some(group);
        self.tiles.insert((tx, tz), body);
    }

    /// Lattice key for a body NodeId, if it is a registered terrain tile.
    pub fn tile_key_of(&self, body: NodeId) -> Option<(i32, i32)> {
        self.tiles
            .iter()
            .find(|(_, &nid)| nid == body)
            .map(|(&k, _)| k)
    }

    /// True if this body is a registered terrain tile (rename-safe; NodeId identity).
    pub fn contains_body(&self, body: NodeId) -> bool {
        self.tile_key_of(body).is_some()
    }

    pub fn clear(&mut self) {
        *self = Self::new();
    }

    /// Whether camera at `cam` may switch residency from `last_center` to `raw_center`.
    /// `force` bypasses cooldown + deadband (explicit Expand / Stream Toggle).
    pub fn may_switch_center(&self, cam: Vec3, raw_center: (i32, i32), force: bool) -> bool {
        if force {
            return true;
        }
        let Some(lc) = self.last_center else {
            return true;
        };
        if lc == raw_center {
            return false;
        }
        if let Some(t0) = self.last_switch_at {
            if t0.elapsed()
                < std::time::Duration::from_millis(STREAM_CENTER_COOLDOWN_MS)
            {
                return false;
            }
        }
        // Deadband: stay until camera is clearly closer to the new tile origin.
        let old_o = tile_body_origin(lc.0, lc.1);
        let new_o = tile_body_origin(raw_center.0, raw_center.1);
        let d_old = (cam.x - old_o.x).hypot(cam.z - old_o.z);
        let d_new = (cam.x - new_o.x).hypot(cam.z - new_o.z);
        d_new + STREAM_SWITCH_DEADBAND_M < d_old
    }

    /// How many tiles are currently `visible` in the graph (resident for draw).
    pub fn count_visible(&self, sg: &SceneGraph) -> usize {
        self.tiles
            .values()
            .filter(|&&nid| sg.get(nid).map(|n| n.visible).unwrap_or(false))
            .count()
    }

    /// Short HUD line: `tiles=9 vis=5 stream=on half=1 @ (0,0)`.
    pub fn status_line(&self, sg: &SceneGraph) -> String {
        let vis = self.count_visible(sg);
        let total = self.tiles.len();
        let stream = if self.stream_enabled { "on" } else { "off" };
        let center = self
            .last_center
            .map(|(x, z)| format!("@ ({x},{z})"))
            .unwrap_or_else(|| "@ —".into());
        format!("tiles={total} vis={vis} stream={stream} half={} {center}", self.half_window)
    }
}

/// Recover lattice key from body **origin** (set at create to [`tile_body_origin`]).
/// Tolerant of float noise. Identity for rename-safe recovery when no side-channel.
pub fn origin_to_tile_coords(origin: Vec3) -> Option<(i32, i32)> {
    // Y must be near 0 (terrain lattice is XZ).
    if origin.y.abs() > 0.5 {
        return None;
    }
    let tx = (origin.x / TILE_METERS).round() as i32;
    let tz = (origin.z / TILE_METERS).round() as i32;
    let expected = tile_body_origin(tx, tz);
    if (origin - expected).length() < 0.25 {
        Some((tx, tz))
    } else {
        None
    }
}

/// Body has a "BaseHill" seed prim — bootstrap marker for pre-side-channel files.
fn body_has_base_hill(sg: &SceneGraph, nid: NodeId) -> bool {
    match sg.get(nid).map(|n| &n.payload) {
        Some(NodePayload::SdfBody { primitives, .. }) => {
            primitives.iter().any(|p| p.name == "BaseHill")
        }
        _ => false,
    }
}

/// Rebuild a [`TerrainWorld`] from the graph **without** using display names as identity.
///
/// Recovery (pre-side-channel / missing records):
/// - SdfBody whose **origin** is on the terrain lattice AND has a `BaseHill` seed prim, OR
/// - origin lattice under a Group that already collected ≥1 such body (siblings share island).
///
/// Display names (`TerrainTile_*`) are **not** consulted — rename-safe.
pub fn scan_terrain_world_from_graph(sg: &SceneGraph) -> TerrainWorld {
    // Collect lattice-aligned candidates with bootstrap signals.
    let mut candidates: Vec<(NodeId, (i32, i32), Option<NodeId>)> = Vec::new();
    for (nid, node) in sg.all_nodes() {
        let NodePayload::SdfBody { origin, .. } = &node.payload else {
            continue;
        };
        let Some(key) = origin_to_tile_coords(*origin) else {
            continue;
        };
        // Require BaseHill (or name prefix only as *last-resort* soft signal for legacy files
        // that moved origin) — name alone is NOT enough to *include* after rename without hill.
        let soft_name = match &node.payload {
            NodePayload::SdfBody { name, .. } => is_terrain_tile_name(name),
            _ => false,
        };
        if !body_has_base_hill(sg, nid) && !soft_name {
            continue;
        }
        candidates.push((nid, key, node.parent));
    }
    if candidates.is_empty() {
        return TerrainWorld::new();
    }

    // Group by parent; pick densest island group.
    let mut by_group: HashMap<Option<NodeId>, Vec<((i32, i32), NodeId)>> = HashMap::new();
    for (nid, key, parent) in candidates {
        by_group.entry(parent).or_default().push((key, nid));
    }

    let mut best: Option<(NodeId, Vec<((i32, i32), NodeId)>, i32)> = None;
    for (parent, tiles) in by_group {
        let Some(gid) = parent else {
            continue;
        };
        // Soft name bonus only for island *group* display name (not tile identity).
        let name_bonus = sg
            .get(gid)
            .and_then(|n| match &n.payload {
                NodePayload::Group { name } if name.starts_with(TERRAIN_ISLAND_NAME_PREFIX) => {
                    Some(1i32)
                }
                _ => None,
            })
            .unwrap_or(0);
        let score = tiles.len() as i32 * 100 + name_bonus;
        let replace = match &best {
            None => true,
            Some((_, _, s)) => score > *s,
        };
        if replace {
            best = Some((gid, tiles, score));
        }
    }

    if best.is_none() {
        let root = sg.root();
        let mut tiles = Vec::new();
        for (nid, node) in sg.all_nodes() {
            let NodePayload::SdfBody { origin, .. } = &node.payload else {
                continue;
            };
            let Some(key) = origin_to_tile_coords(*origin) else {
                continue;
            };
            if !(body_has_base_hill(sg, nid)
                || matches!(&node.payload, NodePayload::SdfBody { name, .. } if is_terrain_tile_name(name)))
            {
                continue;
            }
            if node.parent == Some(root) || node.parent.is_none() {
                tiles.push((key, nid));
            }
        }
        if tiles.is_empty() {
            return TerrainWorld::new();
        }
        let mut tw = TerrainWorld::new();
        tw.island_group = Some(root);
        for (key, body) in tiles {
            tw.tiles.insert(key, body);
        }
        finalize_scanned_world(&mut tw);
        return tw;
    }

    let (gid, tiles, _) = best.unwrap();
    let mut tw = TerrainWorld::new();
    tw.island_group = Some(gid);
    for (key, body) in tiles {
        tw.tiles.insert(key, body);
    }
    finalize_scanned_world(&mut tw);
    tw
}

fn finalize_scanned_world(tw: &mut TerrainWorld) {
    let mut max_r = 0i32;
    for &(tx, tz) in tw.tiles.keys() {
        max_r = max_r.max(tx.abs()).max(tz.abs());
    }
    tw.half_window = max_r.clamp(0, 2);
    tw.stream_enabled = tw.tiles.len() > 1;
    tw.last_center = None; // force first sync after load
    tw.last_switch_at = None;
}

/// Name prefix for terrain tile SdfBodies (outliner + rebuild filter).
pub const TERRAIN_TILE_NAME_PREFIX: &str = "TerrainTile_";
/// Name prefix for terrain island Groups.
pub const TERRAIN_ISLAND_NAME_PREFIX: &str = "TerrainIsland";

/// Half-extent of one tile in XZ (meters). Phase-1 island body is local-centred at origin.
#[inline]
pub fn tile_half_extent() -> f32 {
    TILE_METERS * 0.5
}

/// Body-local prim-centre AABB for **any** terrain tile body.
///
/// Each tile body has its origin at [`tile_body_origin`]; geometry is authored in that
/// body-local frame, so the local clamp box is identical for every (tx, tz). For the
/// world-space footprint use [`tile_aabb_world`].
#[inline]
pub fn tile_aabb_body_local() -> (Vec3, Vec3) {
    let h = tile_half_extent();
    // Overlap apron ≥1 cell (L2) — keep prim centres slightly inside so their bulk
    // still samples the tile without exploding body bounds.
    let margin = terrain_cell_size();
    let xy = h - margin;
    let y = h; // full height band for caves
    (Vec3::new(-xy, -y, -xy), Vec3::new(xy, y, xy))
}

/// World-space AABB of tile `(tx, tz)` (body origin + body-local extents).
#[inline]
pub fn tile_aabb_world(tx: i32, tz: i32) -> (Vec3, Vec3) {
    let o = tile_body_origin(tx, tz);
    let (mn, mx) = tile_aabb_body_local();
    (o + mn, o + mx)
}

// ---------------------------------------------------------------------------
// L2 seam bake — lattice-locked bounds + dual-tile prim membership
// ---------------------------------------------------------------------------

/// Overlap apron in meters (≥1 cell). Each tile's bake grid extends this far past
/// the geometric half-extent so adjacent tiles share samples on the shared face.
#[inline]
pub fn tile_seam_apron_m() -> f32 {
    terrain_cell_size()
}

/// **Bake / DC lattice** body-local AABB for a terrain tile (L2).
///
/// Fixed to the world lattice (not per-body escape-grown prim AABB):
/// - XZ: ±(half + apron) so tiles **overlap ≥1 cell**
/// - Y: ±half (height band for caves / tunnels)
///
/// Use this for `body_bounds` on terrain tiles so adjacent DC meshes sample the same
/// lattice on the shared face (prevents guaranteed seam cracks — plan §12.6).
#[inline]
pub fn tile_lattice_bake_bounds_body_local() -> (Vec3, Vec3) {
    let h = tile_half_extent();
    let apron = tile_seam_apron_m();
    let xy = h + apron;
    (Vec3::new(-xy, -h, -xy), Vec3::new(xy, h, xy))
}

/// True if this body participates in Mesh Terrain seam bake (lattice origin + hill/name).
pub fn is_terrain_bake_body(sg: &SceneGraph, body: NodeId) -> bool {
    match sg.get(body).map(|n| &n.payload) {
        Some(NodePayload::SdfBody { origin, name, .. }) => {
            origin_to_tile_coords(*origin).is_some()
                && (body_has_base_hill(sg, body) || is_terrain_tile_name(name))
        }
        _ => false,
    }
}

/// Lattice key for a terrain bake body, if any.
pub fn terrain_bake_tile_key(sg: &SceneGraph, body: NodeId) -> Option<(i32, i32)> {
    match sg.get(body).map(|n| &n.payload) {
        Some(NodePayload::SdfBody { origin, .. }) if is_terrain_bake_body(sg, body) => {
            origin_to_tile_coords(*origin)
        }
        _ => None,
    }
}

/// 8-neighbour + self lattice keys (Chebyshev r=1).
pub fn terrain_neighbour_keys(tx: i32, tz: i32) -> [(i32, i32); 9] {
    [
        (tx - 1, tz - 1),
        (tx, tz - 1),
        (tx + 1, tz - 1),
        (tx - 1, tz),
        (tx, tz),
        (tx + 1, tz),
        (tx - 1, tz + 1),
        (tx, tz + 1),
        (tx + 1, tz + 1),
    ]
}

/// Map lattice key → body NodeId among terrain bake bodies in the graph.
fn terrain_bodies_by_key(sg: &SceneGraph) -> HashMap<(i32, i32), NodeId> {
    let mut map = HashMap::new();
    for (nid, node) in sg.all_nodes() {
        let NodePayload::SdfBody { origin, .. } = &node.payload else {
            continue;
        };
        if !is_terrain_bake_body(sg, nid) {
            continue;
        }
        if let Some(key) = origin_to_tile_coords(*origin) {
            map.insert(key, nid);
        }
    }
    map
}

/// Neighbour terrain body ids (not including `body` itself).
pub fn terrain_neighbour_body_ids(sg: &SceneGraph, body: NodeId) -> Vec<NodeId> {
    let Some(key) = terrain_bake_tile_key(sg, body) else {
        return Vec::new();
    };
    let map = terrain_bodies_by_key(sg);
    terrain_neighbour_keys(key.0, key.1)
        .into_iter()
        .filter(|&k| k != key)
        .filter_map(|k| map.get(&k).copied())
        .collect()
}

/// World-space AABB of a prim (position ± half-extents), with prim pos in **world**.
fn prim_world_aabb(p: &core_math::primitives::SdfPrimitive) -> (Vec3, Vec3) {
    let he = p.bounding_aabb_half_extents();
    (p.position - he, p.position + he)
}

fn aabb_overlap_xz(a_min: Vec3, a_max: Vec3, b_min: Vec3, b_max: Vec3) -> bool {
    a_min.x <= b_max.x && a_max.x >= b_min.x && a_min.z <= b_max.z && a_max.z >= b_min.z
}

/// Local Y of the BaseHill **ground top** (meters above body origin).
pub const BASE_HILL_TOP_Y: f32 = 2.0;

/// BaseHill solid extents: top at [`BASE_HILL_TOP_Y`], bottom **past the lattice floor**
/// so Dual Contouring never extracts an underside face (that was the dual-slab bug —
/// a deeper floating box only spread top/bottom farther apart).
#[inline]
pub fn base_hill_half_y() -> f32 {
    let (lat_mn, _) = tile_lattice_bake_bounds_body_local();
    let top = BASE_HILL_TOP_Y;
    let bottom = lat_mn.y - terrain_cell_size(); // one cell past bake floor
    ((top - bottom) * 0.5).max(1.0)
}

#[inline]
pub fn base_hill_center_y() -> f32 {
    let (lat_mn, _) = tile_lattice_bake_bounds_body_local();
    let top = BASE_HILL_TOP_Y;
    let bottom = lat_mn.y - terrain_cell_size();
    (top + bottom) * 0.5
}

/// Coarse display mesh while a resident tile waits for bake/DC (Phase 2 placeholder LOD).
///
/// **Top face only** over footprint + seam apron — never a thin dual-slab (top+bottom).
/// Replaced by DC/`insert_mesh_direct`.
pub fn terrain_placeholder_mesh() -> core_math::mesh::Mesh {
    let h = (tile_half_extent() + tile_seam_apron_m()).max(1.0);
    let y = BASE_HILL_TOP_Y;
    // Single upward face — unique verts, correct normals. No underside “second layer”.
    let vertices = vec![
        Vec3::new(-h, y, -h),
        Vec3::new(h, y, -h),
        Vec3::new(h, y, h),
        Vec3::new(-h, y, h),
    ];
    let normals = vec![Vec3::Y, Vec3::Y, Vec3::Y, Vec3::Y];
    let quads = vec![[0, 1, 2, 3]];
    core_math::mesh::Mesh {
        vertices,
        normals,
        quads,
    }
}

/// Collect prims for a terrain tile bake: **own** prims + **straddling** prims from
/// neighbour tiles whose AABB overlaps this tile's lattice volume (L2 dual membership).
///
/// Positions are returned in **this body's local frame** (BODY_LOCAL_PRIMS contract).
pub fn collect_terrain_bake_prims(sg: &SceneGraph, body: NodeId) -> Vec<core_math::primitives::SdfPrimitive> {
    use crate::bake_utils::collect_body_prims_recursive;

    let Some(key) = terrain_bake_tile_key(sg, body) else {
        return collect_body_prims_recursive(sg, body);
    };
    let body_origin = match sg.get(body).map(|n| &n.payload) {
        Some(NodePayload::SdfBody { origin, .. }) => *origin,
        _ => Vec3::ZERO,
    };
    let (lat_mn, lat_mx) = tile_lattice_bake_bounds_body_local();
    // Lattice volume in world space for this tile.
    let world_lat_mn = body_origin + lat_mn;
    let world_lat_mx = body_origin + lat_mx;

    let mut out = collect_body_prims_recursive(sg, body);
    let map = terrain_bodies_by_key(sg);

    for nk in terrain_neighbour_keys(key.0, key.1) {
        if nk == key {
            continue;
        }
        let Some(&nb) = map.get(&nk) else {
            continue;
        };
        let nb_origin = match sg.get(nb).map(|n| &n.payload) {
            Some(NodePayload::SdfBody { origin, .. }) => *origin,
            _ => continue,
        };
        // Neighbour prims are body-local to `nb`; convert to world then to `body` local.
        let nb_prims = collect_body_prims_recursive(sg, nb);
        for p in nb_prims {
            if !p.visible {
                continue;
            }
            let mut world_p = p.clone();
            // collect_body_prims_recursive already shifts nested bodies into nb's top frame
            // (body-local when BODY_LOCAL_PRIMS). World = nb_origin + local.
            world_p.position += nb_origin;
            let (wmn, wmx) = prim_world_aabb(&world_p);
            if !aabb_overlap_xz(wmn, wmx, world_lat_mn, world_lat_mx) {
                continue; // not straddling this tile
            }
            // Into target body-local.
            world_p.position -= body_origin;
            // Avoid double-adding identical prim_uids already owned (shouldn't happen across bodies).
            if out.iter().any(|o| o.prim_uid == world_p.prim_uid) {
                continue;
            }
            out.push(world_p);
        }
    }
    out
}

/// Clamp a body-local point into the tile AABB (prim **centre** constraint).
#[inline]
pub fn clamp_to_tile_aabb(p: Vec3, aabb_min: Vec3, aabb_max: Vec3) -> Vec3 {
    Vec3::new(
        p.x.clamp(aabb_min.x, aabb_max.x),
        p.y.clamp(aabb_min.y, aabb_max.y),
        p.z.clamp(aabb_min.z, aabb_max.z),
    )
}

/// Parse `TerrainTile_{tx}_{tz}` → tile indices.
///
/// **Display-name helper only** — not identity. Prefer [`TerrainWorld::tile_key_of`] /
/// [`origin_to_tile_coords`] / the `terrain_tiles` save side-channel.
pub fn parse_terrain_tile_name(name: &str) -> Option<(i32, i32)> {
    let rest = name.strip_prefix(TERRAIN_TILE_NAME_PREFIX)?;
    let mut it = rest.split('_');
    let tx: i32 = it.next()?.parse().ok()?;
    let tz: i32 = it.next()?.parse().ok()?;
    if it.next().is_some() {
        return None;
    }
    Some((tx, tz))
}

/// Soft display-name check (legacy / outliner defaults). Not identity.
#[inline]
pub fn is_terrain_tile_name(name: &str) -> bool {
    parse_terrain_tile_name(name).is_some()
}

// ---------------------------------------------------------------------------
// Pure graph construction (unit-testable without GPU / full app)
// ---------------------------------------------------------------------------

/// Result of spawning one Phase-1 island into a [`SceneGraph`].
#[derive(Debug, Clone, Copy)]
pub struct TerrainIslandIds {
    pub group: NodeId,
    pub tile_body: NodeId,
}

/// Build a base-hill prim for one dense tile (no image displace — L4).
///
/// Fills the **full tile footprint + 1-cell seam apron** so adjacent tiles' hills
/// **overlap** in world space (L2 stitch). A 0.45×TILE hill left ~6 m air gaps
/// between neighbours — patches looked like separate islands.
///
/// **Ground solid, single isosurface in the bake volume:**
/// - Top at [`BASE_HILL_TOP_Y`] (the terrain surface).
/// - Bottom **past the lattice floor** so Dual Contouring never sees an underside face.
///   A floating closed box (any thickness) extracts **top + bottom** → two layers;
///   making it deeper only spreads them (the regression users saw).
pub fn make_base_hill_prim(prim_uid: u32) -> SdfPrimitive {
    // half + apron: tile (0,0) covers X to +(32+cell); tile (1,0) covers from 64−(32+cell).
    // Overlap strip width = 2·cell ≥ 1 cell on the shared face.
    let half_xz = tile_half_extent() + tile_seam_apron_m();
    let half_y = base_hill_half_y();
    let mut p = SdfPrimitive::with_uid(
        "BaseHill".into(),
        SdfShape::Box {
            half_extents: Vec3::new(half_xz, half_y, half_xz),
            bevel: {
                let mut b = BoxBevel::default();
                // Mild top-edge round only — large side bevels would open gaps at the seam.
                b.radii = [0.0; 12];
                for i in 0..4 {
                    b.radii[4 + i] = 0.75; // top edges 4..7
                }
                b
            },
        },
        Vec3::new(0.0, base_hill_center_y(), 0.0),
        prim_uid,
    );
    p.operation = BooleanOp::Union { k: 0.0 };
    p.scale = Vec3::ONE;
    p
}

/// Insert one terrain tile SdfBody under an existing group.
pub fn insert_terrain_tile(
    graph: &mut SceneGraph,
    group: NodeId,
    tx: i32,
    tz: i32,
    prim_uid: u32,
) -> NodeId {
    let hill = make_base_hill_prim(prim_uid);
    let tile_body = graph.add_child(
        group,
        NodePayload::SdfBody {
            name: format!("{TERRAIN_TILE_NAME_PREFIX}{tx}_{tz}"),
            primitives: vec![hill],
            mesh_cache_id: None,
            body_op: BooleanOp::Union { k: 0.0 },
            origin: tile_body_origin(tx, tz),
            body_rotation: glam::Quat::IDENTITY,
            body_scale: Vec3::ONE,
            mirror_flags: 0,
            mirror_sign: 0,
            mirror_center: Vec3::ZERO,
            bevel_radius: 0.0,
            blend_profile: BlendProfile::default(),
            material: PbrMaterial::default(),
            child_order: Vec::new(),
            mesh_modifiers: Vec::new(),
            field_modifiers: Vec::new(),
        },
    );
    graph.ensure_body_child_order(tile_body);
    tile_body
}

/// Insert `TerrainIsland` Group + one `TerrainTile_0_0` SdfBody with base hill.
///
/// Pure graph write — caller assigns names/ids and wires app selection / bake.
pub fn insert_terrain_island_into_graph(
    graph: &mut SceneGraph,
    island_name: String,
    _tile_name: String,
    prim_uid: u32,
) -> TerrainIslandIds {
    let root = graph.root();
    let group = graph.add_child(
        root,
        NodePayload::Group {
            name: island_name,
        },
    );
    let tile_body = insert_terrain_tile(graph, group, 0, 0, prim_uid);
    TerrainIslandIds { group, tile_body }
}

/// Insert a square patch of tiles around (0,0) with Chebyshev radius `half`.
/// Returns group + map of keys → body (including 0_0).
pub fn insert_terrain_patch(
    graph: &mut SceneGraph,
    island_name: String,
    half: i32,
    mut next_prim_uid: u32,
) -> (NodeId, HashMap<(i32, i32), NodeId>, u32) {
    let root = graph.root();
    let group = graph.add_child(
        root,
        NodePayload::Group {
            name: island_name,
        },
    );
    let mut tiles = HashMap::new();
    for (tx, tz) in desired_resident_tiles((0, 0), half) {
        let body = insert_terrain_tile(graph, group, tx, tz, next_prim_uid);
        next_prim_uid = next_prim_uid.saturating_add(1);
        tiles.insert((tx, tz), body);
    }
    (group, tiles, next_prim_uid)
}

/// Append a horizontal cylinder **Subtract** through the base hill (tunnel demo).
/// Pure graph write for acceptance tests and future "demo carve" actions.
pub fn append_tunnel_cutter(graph: &mut SceneGraph, tile_body: NodeId, prim_uid: u32) -> bool {
    // Cylinder along +X through the mound centre — radius 2.5 m, length covers the hill.
    let mut cutter = SdfPrimitive::with_uid(
        "TunnelCutter".into(),
        SdfShape::Cylinder {
            radius: 2.5,
            half_height: TILE_METERS * 0.4,
        },
        Vec3::new(0.0, 0.5, 0.0), // slightly above body local origin (hill bulk)
        prim_uid,
    );
    // Default cylinder is +Y; rotate so axis is +X (tunnel through the mound).
    cutter.rotation_deg = Vec3::new(0.0, 0.0, 90.0);
    cutter.operation = BooleanOp::Subtract { k: 0.0 };
    {
        let Some(NodePayload::SdfBody { primitives, .. }) =
            graph.get_mut(tile_body).map(|n| &mut n.payload)
        else {
            return false;
        };
        primitives.push(cutter);
    }
    graph.ensure_body_child_order(tile_body);
    true
}

// ---------------------------------------------------------------------------
// App entry points
// ---------------------------------------------------------------------------

impl FourthLayerApp {
    /// True if `body` is a Mesh Terrain tile — **NodeId registry** first, then origin lattice
    /// recovery (never display-name alone).
    pub(crate) fn is_terrain_tile_body(&self, body: NodeId) -> bool {
        if self.terrain_world.contains_body(body) {
            return true;
        }
        // Session without registry yet: origin lattice + BaseHill bootstrap.
        match self.scene.scene_graph.get(body).map(|n| &n.payload) {
            Some(NodePayload::SdfBody { origin, .. }) => {
                origin_to_tile_coords(*origin).is_some() && body_has_base_hill(&self.scene.scene_graph, body)
            }
            _ => false,
        }
    }

    /// If `body` is a terrain tile, clamp a body-local prim centre into the tile AABB.
    /// Uses durable identity ([`Self::is_terrain_tile_body`]), not outliner names.
    pub(crate) fn clamp_prim_pos_for_terrain_body(
        &self,
        body: NodeId,
        pos: Vec3,
    ) -> Vec3 {
        if !self.is_terrain_tile_body(body) {
            return pos;
        }
        let (mn, mx) = tile_aabb_body_local();
        clamp_to_tile_aabb(pos, mn, mx)
    }

    /// Apply durable `terrain_tiles` side-channel from a loaded project (preferred).
    /// Falls back to graph scan when the side-channel is empty (pre-feature files).
    pub(crate) fn apply_terrain_tiles_side_channel(
        &mut self,
        records: &[crate::project::TerrainTileRecord],
    ) {
        if records.is_empty() {
            self.rebuild_terrain_world_from_scene();
            return;
        }
        let mut tw = TerrainWorld::new();
        let mut ig = None;
        let mut half = 1i32;
        let mut stream = false;
        for rec in records {
            let nid = NodeId(rec.node_id);
            // Skip dangling ids (deleted bodies).
            if self.scene.scene_graph.get(nid).is_none() {
                continue;
            }
            tw.tiles.insert((rec.tx, rec.tz), nid);
            if rec.island_group != 0 {
                ig = Some(NodeId(rec.island_group));
            }
            if rec.half_window > 0 {
                half = rec.half_window;
            }
            stream |= rec.stream_enabled;
        }
        if tw.tiles.is_empty() {
            self.rebuild_terrain_world_from_scene();
            return;
        }
        // Recover island group from parent if missing/invalid.
        if ig.is_none() || ig.is_some_and(|g| self.scene.scene_graph.get(g).is_none()) {
            if let Some((_, &body)) = tw.tiles.iter().next() {
                ig = self.scene.scene_graph.get(body).and_then(|n| n.parent);
            }
        }
        tw.island_group = ig;
        tw.half_window = half.clamp(0, 2);
        tw.stream_enabled = stream || tw.tiles.len() > 1;
        tw.last_center = None;
        tw.last_switch_at = None;
        self.terrain_world = tw;
        for &body in self.terrain_world.tiles.values() {
            self.cache
                .body_vol_cache
                .set_body_resolution_policy(body, DENSE_TILE_RES);
        }
        if self.terrain_world.stream_enabled {
            self.sync_terrain_resident_window(true);
        }
    }

    /// Latch terrain gauges for the Perf HUD (plan Phase 1 table — bake/DC/tris/dirty).
    /// Call once per frame while a TerrainWorld is non-empty.
    pub(crate) fn refresh_terrain_perf_gauges(&mut self) {
        if self.terrain_world.tiles.is_empty() {
            self.perf.terrain_tiles = 0;
            self.perf.terrain_vis = 0;
            self.perf.terrain_tris = 0;
            self.perf.terrain_dirty_bodies = 0;
            self.perf.terrain_dirty_regions = 0;
            return;
        }
        self.perf.terrain_tiles = self.terrain_world.tiles.len() as u32;
        self.perf.terrain_vis = self.terrain_world.count_visible(&self.scene.scene_graph) as u32;
        self.perf.terrain_last_dc_ms = self.perf.vol_dc_ms as f32;
        self.perf.terrain_pending_remesh =
            self.terrain_world.pending_display_remesh.len() as u32;
        let nids: Vec<_> = self.terrain_world.tiles.values().copied().collect();
        let (db, dr) = self.cache.body_vol_cache.terrain_dirty_counts(nids.iter().copied());
        self.perf.terrain_dirty_bodies = db;
        self.perf.terrain_dirty_regions = dr;
        let mut tris = 0u32;
        for nid in nids {
            if let Some(NodePayload::SdfBody {
                mesh_cache_id: Some(cid),
                ..
            }) = self.scene.scene_graph.get(nid).map(|n| &n.payload)
            {
                if let Some(m) = self.cache.mesh_cache.get(*cid) {
                    tris = tris.saturating_add(m.triangle_count);
                }
            }
        }
        self.perf.terrain_tris = tris;
    }

    /// After a bake lands: if it was a terrain tile, latch bake ms + toast.
    /// Full-res settle queues **async** display remesh (never draft / never blocks LMB).
    pub(crate) fn maybe_toast_terrain_bake(&mut self) {
        let Some((nid, ms)) = self.cache.body_vol_cache.take_last_finished_bake() else {
            return;
        };
        if !self.is_terrain_tile_body(nid) {
            return;
        }
        self.perf.terrain_last_bake_ms = ms;
        let name = self
            .scene
            .scene_graph
            .get(nid)
            .and_then(|n| match &n.payload {
                NodePayload::SdfBody { name, .. } => Some(name.clone()),
                _ => None,
            })
            .unwrap_or_else(|| format!("{nid:?}"));
        let tris = self
            .scene
            .scene_graph
            .get(nid)
            .and_then(|n| match &n.payload {
                NodePayload::SdfBody {
                    mesh_cache_id: Some(cid),
                    ..
                } => self
                    .cache
                    .mesh_cache
                    .get(*cid)
                    .map(|m| m.triangle_count),
                _ => None,
            });
        let tri_s = tris
            .map(|t| format!(", ~{t} tris"))
            .unwrap_or_default();
        self.toast(&format!(
            "Terrain {name}: bake {ms:.0} ms{tri_s} (target <100 ms draft)"
        ));
        // Async settle remesh after full-res only — draft bakes during drag must not queue.
        if !self.cache.body_vol_cache.is_draft_mode() {
            self.request_terrain_display_remesh(nid);
        }
    }

    /// Queue a settle remesh for a terrain tile's **display** mesh (volume stays truth).
    /// Work runs on rayon via [`Self::poll_terrain_display_remesh`] after the mesh stamp advances.
    pub(crate) fn request_terrain_display_remesh(&mut self, body: NodeId) {
        if !self.is_terrain_tile_body(body) {
            return;
        }
        let stamp = match self.scene.scene_graph.get(body).map(|n| &n.payload) {
            Some(NodePayload::SdfBody {
                mesh_cache_id: Some(cid),
                ..
            }) => self
                .cache
                .mesh_cache
                .get(*cid)
                .map(|m| m.content_stamp)
                .unwrap_or(0),
            _ => 0,
        };
        self.terrain_world
            .pending_display_remesh
            .insert(body, stamp);
        self.perf.terrain_pending_remesh =
            self.terrain_world.pending_display_remesh.len() as u32
                + self.terrain_remesh_jobs.len() as u32;
    }

    /// Apply finished remesh jobs + spawn at most one ready body per frame (off frame thread).
    pub(crate) fn poll_terrain_display_remesh(&mut self) {
        self.poll_terrain_remesh_results();
        self.spawn_ready_terrain_remesh_jobs();
        self.perf.terrain_pending_remesh = self.terrain_world.pending_display_remesh.len() as u32
            + self.terrain_remesh_jobs.len() as u32;
    }

    fn poll_terrain_remesh_results(&mut self) {
        let mut i = 0;
        while i < self.terrain_remesh_jobs.len() {
            match self.terrain_remesh_jobs[i].rx.try_recv() {
                Ok(result) => {
                    self.terrain_remesh_jobs.swap_remove(i);
                    let current_gen = self
                        .terrain_remesh_gen
                        .get(&result.body)
                        .copied()
                        .unwrap_or(0);
                    if result.job_gen != current_gen {
                        continue; // superseded
                    }
                    // Source mesh changed since spawn (newer DC) — re-queue settle.
                    let stamp_now = self.terrain_mesh_content_stamp(result.body);
                    if stamp_now != 0 && stamp_now != result.source_stamp {
                        self.terrain_world
                            .pending_display_remesh
                            .insert(result.body, result.source_stamp);
                        continue;
                    }
                    let Some((verts, indices, tris)) = result.raster else {
                        continue;
                    };
                    // Body / cache id still valid?
                    let still = matches!(
                        self.scene.scene_graph.get(result.body).map(|n| &n.payload),
                        Some(NodePayload::SdfBody {
                            mesh_cache_id: Some(c),
                            ..
                        }) if *c == result.cid
                    );
                    if !still {
                        continue;
                    }
                    self.cache.mesh_cache.update_editable_mesh(
                        result.cid,
                        (verts, indices),
                        tris,
                    );
                    self.wire_local_cache.remove(&result.cid.0);
                    self.perf.terrain_last_remesh_ms = result.ms;
                    self.mark_dirty(DirtyFlags::RENDER);
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {
                    i += 1;
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.terrain_remesh_jobs.swap_remove(i);
                }
            }
        }
    }

    fn spawn_ready_terrain_remesh_jobs(&mut self) {
        if self.sculpt.stroke_active || self.cache.body_vol_cache.is_draft_mode() {
            return;
        }
        // At most one spawn per frame (frame budget); jobs run in parallel on rayon once started.
        const MAX_SPAWN_PER_FRAME: usize = 1;
        let mut spawned = 0usize;
        let candidates: Vec<(NodeId, u64)> = self
            .terrain_world
            .pending_display_remesh
            .iter()
            .map(|(&b, &s)| (b, s))
            .collect();
        for (body, wait_stamp) in candidates {
            if spawned >= MAX_SPAWN_PER_FRAME {
                break;
            }
            if self.terrain_remesh_jobs.iter().any(|j| j.body == body) {
                continue; // already in flight
            }
            if self.cache.body_vol_cache.is_body_dirty(body)
                || self.cache.body_vol_cache.is_body_baking(body)
            {
                continue;
            }
            let stamp_now = self.terrain_mesh_content_stamp(body);
            if stamp_now == 0 || stamp_now == wait_stamp {
                continue; // wait for post-bake DC mesh
            }
            if self.spawn_terrain_remesh_job(body) {
                self.terrain_world.pending_display_remesh.remove(&body);
                spawned += 1;
            } else {
                // No mesh to remesh — drop the request.
                self.terrain_world.pending_display_remesh.remove(&body);
            }
        }
    }

    fn terrain_mesh_content_stamp(&self, body: NodeId) -> u64 {
        match self.scene.scene_graph.get(body).map(|n| &n.payload) {
            Some(NodePayload::SdfBody {
                mesh_cache_id: Some(cid),
                ..
            }) => self
                .cache
                .mesh_cache
                .get(*cid)
                .map(|m| m.content_stamp)
                .unwrap_or(0),
            _ => 0,
        }
    }

    /// Clone display mesh + spawn rayon remesh. Returns false if no mesh/surface.
    fn spawn_terrain_remesh_job(&mut self, body: NodeId) -> bool {
        let Some((cid, source_stamp, input)) =
            Self::remesh_input_from_mesh_cache(&self.cache.mesh_cache, body, &self.scene.scene_graph)
        else {
            return false;
        };
        let job_gen = {
            let g = self.terrain_remesh_gen.entry(body).or_insert(0);
            *g = g.wrapping_add(1);
            *g
        };
        let res = DENSE_TILE_RES.min(crate::poly::remesh::REMESH_RES_MAX);
        let (tx, rx) = std::sync::mpsc::channel();
        rayon::spawn(move || {
            let t0 = std::time::Instant::now();
            let he = match input {
                TerrainRemeshInput::Quads {
                    vertices,
                    normals,
                    quads,
                } => {
                    let mesh = core_math::mesh::Mesh {
                        vertices,
                        normals,
                        quads,
                    };
                    core_math::half_edge::HalfEdgeMesh::from_mesh(&mesh)
                }
                TerrainRemeshInput::Soup {
                    positions,
                    normals,
                    tris,
                } => core_math::half_edge::HalfEdgeMesh::from_triangle_soup(
                    &positions, &normals, &tris,
                ),
            };
            let raster = if he.face_count() == 0 {
                None
            } else {
                crate::poly::remesh::voxel_remesh_mesh(&he, res, true, 0.1, 1.0).map(|out| {
                    let (verts, indices) = out.to_raster_data();
                    let tris = (indices.len() / 3) as u32;
                    (verts, indices, tris)
                })
            };
            let ms = t0.elapsed().as_secs_f32() * 1000.0;
            let _ = tx.send(TerrainRemeshJobResult {
                body,
                cid,
                source_stamp,
                job_gen,
                ms,
                raster,
            });
        });
        self.terrain_remesh_jobs
            .push(PendingTerrainRemeshJob { body, job_gen, rx });
        true
    }

    fn remesh_input_from_mesh_cache(
        mesh_cache: &render::mesh_cache::MeshCache,
        body: NodeId,
        graph: &SceneGraph,
    ) -> Option<(
        core_math::scene_graph::MeshCacheId,
        u64,
        TerrainRemeshInput,
    )> {
        let cid = match graph.get(body).map(|n| &n.payload) {
            Some(NodePayload::SdfBody {
                mesh_cache_id: Some(c),
                ..
            }) => *c,
            _ => return None,
        };
        let cached = mesh_cache.get(cid)?;
        let stamp = cached.content_stamp;
        if !cached.cpu_mesh.quads.is_empty() && !cached.cpu_mesh.vertices.is_empty() {
            return Some((
                cid,
                stamp,
                TerrainRemeshInput::Quads {
                    vertices: cached.cpu_mesh.vertices.clone(),
                    normals: cached.cpu_mesh.normals.clone(),
                    quads: cached.cpu_mesh.quads.clone(),
                },
            ));
        }
        let (verts, indices) = mesh_cache.chunk_mesh_data(cid)?;
        if indices.len() < 3 || verts.is_empty() {
            return None;
        }
        let positions: Vec<Vec3> = verts
            .iter()
            .map(|v| Vec3::new(v[0], v[1], v[2]))
            .collect();
        let normals: Vec<Vec3> = verts
            .iter()
            .map(|v| Vec3::new(v[3], v[4], v[5]))
            .collect();
        let tris: Vec<[u32; 3]> = indices
            .chunks(3)
            .filter(|c| c.len() == 3)
            .map(|c| [c[0], c[1], c[2]])
            .collect();
        if tris.is_empty() {
            return None;
        }
        Some((
            cid,
            stamp,
            TerrainRemeshInput::Soup {
                positions,
                normals,
                tris,
            },
        ))
    }

    /// Shift+A / Geometry: create one Mesh Terrain island (Phase 1 MVP).
    ///
    /// - Group `TerrainIsland.NNN`
    /// - Child SdfBody `TerrainTile_0_0` with pure-SDF base hill
    /// - Dense bake target [`DENSE_TILE_RES`] (no height seed)
    pub(crate) fn create_terrain_island(&mut self) {
        self.transaction("Create Terrain Island", |s| {
            let id = s.scene.next_node_name_id.max(1);
            s.scene.next_node_name_id = id + 1;
            let island_name = format!("{TERRAIN_ISLAND_NAME_PREFIX}.{id:03}");
            let tile_name = format!("{TERRAIN_TILE_NAME_PREFIX}0_0");

            let prim_uid = s.scene.next_prim_id;
            s.scene.next_prim_id = s.scene.next_prim_id.saturating_add(1);

            let ids = insert_terrain_island_into_graph(
                &mut s.scene.scene_graph,
                island_name.clone(),
                tile_name.clone(),
                prim_uid,
            );

            // Pin dense 128³ — remesh/export path only (L3).
            s.cache
                .body_vol_cache
                .set_body_resolution_policy(ids.tile_body, DENSE_TILE_RES);
            s.pin_terrain_lattice_bounds(ids.tile_body);
            s.ensure_terrain_placeholder_display(ids.tile_body);

            s.terrain_world.clear();
            s.terrain_world
                .register_tile(0, 0, ids.tile_body, ids.group);
            s.terrain_world.stream_enabled = false;

            s.scene.sdf_body_nid = ids.tile_body;
            s.scene.active_body_nid = ids.tile_body;
            s.sync_flat_cache();
            s.select_single_unified(SceneItemId::Node(ids.tile_body));
            s.mark_dirty(DirtyFlags::SCENE | DirtyFlags::RENDER);
            s.mark_outliner_dirty();

            let cell = terrain_cell_size();
            s.toast(&format!(
                "{island_name} — {tile_name} @ {DENSE_TILE_RES}³ (~{cell:.2} m/cell). \
                 Subtract prims or sculpt to carve. Rebuild Mesh forces full-res bake."
            ));
        });
    }

    /// Create a 3×3 (or 5×5) terrain patch and enable camera resident streaming.
    pub(crate) fn create_terrain_patch(&mut self, half: i32) {
        let half = half.clamp(0, 2);
        self.transaction("Create Terrain Patch", |s| {
            let id = s.scene.next_node_name_id.max(1);
            s.scene.next_node_name_id = id + 1;
            let island_name = format!("{TERRAIN_ISLAND_NAME_PREFIX}.{id:03}");
            let start_uid = s.scene.next_prim_id;
            let (group, tiles, next_uid) = insert_terrain_patch(
                &mut s.scene.scene_graph,
                island_name.clone(),
                half,
                start_uid,
            );
            s.scene.next_prim_id = next_uid;

            s.terrain_world.clear();
            s.terrain_world.island_group = Some(group);
            s.terrain_world.half_window = half;
            s.terrain_world.stream_enabled = true;
            s.terrain_world.last_center = Some((0, 0));
            for (&key, &body) in &tiles {
                s.cache
                    .body_vol_cache
                    .set_body_resolution_policy(body, DENSE_TILE_RES);
                s.pin_terrain_lattice_bounds(body);
                s.ensure_terrain_placeholder_display(body);
                s.terrain_world.tiles.insert(key, body);
            }

            let center = tiles.get(&(0, 0)).copied().or_else(|| tiles.values().next().copied());
            if let Some(body) = center {
                s.scene.sdf_body_nid = body;
                s.scene.active_body_nid = body;
                s.select_single_unified(SceneItemId::Node(body));
            }
            s.sync_flat_cache();
            s.mark_dirty(DirtyFlags::SCENE | DirtyFlags::RENDER);
            s.mark_outliner_dirty();

            let n = tiles.len();
            let side = 2 * half + 1;
            s.toast(&format!(
                "{island_name} — {n} tiles ({side}\u{00D7}{side}) @ {DENSE_TILE_RES}³, stream ON. \
                 Fly: hide far tiles (drop dense grids + GPU mesh). Grow: Expand Terrain Here / larger Patch."
            ));
        });
    }

    /// Rebuild [`TerrainWorld`] from scene node names (after `.fl4` load / undo recovery).
    /// Visibility/grid residency only — **never creates tiles**.
    pub(crate) fn rebuild_terrain_world_from_scene(&mut self) {
        self.terrain_world = scan_terrain_world_from_graph(&self.scene.scene_graph);
        for &body in self.terrain_world.tiles.values() {
            self.cache
                .body_vol_cache
                .set_body_resolution_policy(body, DENSE_TILE_RES);
        }
        if self.terrain_world.stream_enabled {
            self.sync_terrain_resident_window(true);
        }
    }

    /// Toast current stream status (debug / owner HUD).
    pub(crate) fn toast_terrain_stream_status(&mut self) {
        if self.terrain_world.tiles.is_empty() {
            self.toast("Terrain stream — no tiles (create Island/Patch or load a scene)");
            return;
        }
        let line = self
            .terrain_world
            .status_line(&self.scene.scene_graph);
        self.toast(&format!("Terrain {line}"));
    }

    /// Toggle camera-driven resident window (Phase 2).
    pub(crate) fn toggle_terrain_stream(&mut self) {
        if self.terrain_world.tiles.is_empty() {
            // Recover from scene if the user loaded a .fl4 before this session field existed.
            self.rebuild_terrain_world_from_scene();
        }
        if self.terrain_world.tiles.is_empty() {
            self.toast("Terrain stream — create a Terrain Island / Patch first");
            return;
        }
        self.terrain_world.stream_enabled = !self.terrain_world.stream_enabled;
        let on = self.terrain_world.stream_enabled;
        if on {
            self.sync_terrain_resident_window(true);
        }
        let line = self
            .terrain_world
            .status_line(&self.scene.scene_graph);
        self.toast(&format!(
            "Terrain stream {} — {line}",
            if on { "ON" } else { "OFF" }
        ));
    }

    /// Poll / stream path: visibility + dense-grid drop + GPU mesh free **only**.
    ///
    /// **Never creates graph nodes and never calls `transaction`.** Creating tiles from
    /// the poll loop pushed camera-motion undo entries and could `commit_episode()` mid-drag.
    /// Growth is Patch 3×3/5×5 or [`Self::expand_terrain_here`] only.
    ///
    /// `force` bypasses hysteresis (Stream Toggle ON / post-load rebuild).
    pub(crate) fn sync_terrain_resident_window(&mut self, force: bool) {
        if !self.terrain_world.stream_enabled {
            return;
        }
        self.prune_stale_terrain_tiles();
        if self.terrain_world.tiles.is_empty() {
            self.terrain_world.stream_enabled = false;
            return;
        }
        if self.terrain_world.island_group.is_none() {
            return;
        }
        if let Some(gid) = self.terrain_world.island_group {
            if self.scene.scene_graph.get(gid).is_none() {
                self.terrain_world = scan_terrain_world_from_graph(&self.scene.scene_graph);
                for &body in self.terrain_world.tiles.values() {
                    self.cache
                        .body_vol_cache
                        .set_body_resolution_policy(body, DENSE_TILE_RES);
                }
                if self.terrain_world.tiles.is_empty() {
                    return;
                }
            }
        }

        let cam = self.viewport.camera.target;
        let raw_center = world_to_tile(cam.x, cam.z);
        if !self
            .terrain_world
            .may_switch_center(cam, raw_center, force)
        {
            // Cooldown / deadband: keep applying residency for last_center only if set.
            let Some(center) = self.terrain_world.last_center else {
                return;
            };
            self.apply_terrain_residency(center);
            return;
        }

        if self.terrain_world.last_center != Some(raw_center) {
            self.terrain_world.last_center = Some(raw_center);
            self.terrain_world.last_switch_at = Some(std::time::Instant::now());
        }
        self.apply_terrain_residency(raw_center);
    }

    /// Apply visibility + cold dense/GPU free for the window around `center`.
    fn apply_terrain_residency(&mut self, center: (i32, i32)) {
        let half = self.terrain_world.half_window.clamp(0, 2);
        let desired: HashSet<(i32, i32)> = desired_resident_tiles(center, half)
            .into_iter()
            .collect();

        let all_keys: Vec<(i32, i32)> = self.terrain_world.tiles.keys().copied().collect();
        for key in all_keys {
            let Some(&nid) = self.terrain_world.tiles.get(&key) else {
                continue;
            };
            let want = desired.contains(&key);
            let was_visible = self
                .scene
                .scene_graph
                .get(nid)
                .map(|n| n.visible)
                .unwrap_or(false);
            if let Some(node) = self.scene.scene_graph.get_mut(nid) {
                node.visible = want;
            }
            if want {
                // Lattice bounds so first bake never escape-grows off the world grid (L2).
                self.pin_terrain_lattice_bounds(nid);
                // Phase 3 near/far: camera tile full dense; outer rings coarser.
                let dist = (key.0 - center.0).abs().max((key.1 - center.1).abs());
                let res = terrain_res_for_chebyshev(dist);
                self.cache
                    .body_vol_cache
                    .set_body_resolution_policy(nid, res);
                if !self.cache.body_vol_cache.has_dense_grid(nid) {
                    self.cache.body_vol_cache.mark_body_dirty(nid);
                }
                // Placeholder LOD until DC display mesh exists (Phase 2 table).
                self.ensure_terrain_placeholder_display(nid);
            } else if was_visible
                || self.cache.body_vol_cache.has_dense_grid(nid)
                || self.cold_tile_has_gpu_mesh(nid)
            {
                self.release_cold_tile_resources(nid);
            }
        }
        self.mark_dirty(DirtyFlags::RENDER);
    }

    /// Pin `body_bounds` to the lattice AABB (no prim escape-grow).
    pub(crate) fn pin_terrain_lattice_bounds(&mut self, body: NodeId) {
        if !self.is_terrain_tile_body(body) && !is_terrain_bake_body(&self.scene.scene_graph, body)
        {
            return;
        }
        let lattice = tile_lattice_bake_bounds_body_local();
        self.cache.body_vol_cache.body_bounds.insert(body, lattice);
    }

    /// If the tile has no display mesh yet, insert a coarse box so the viewport is not empty
    /// while bake/DC is in flight. Real DC/`insert_mesh_direct` overwrites the same slot.
    pub(crate) fn ensure_terrain_placeholder_display(&mut self, body: NodeId) {
        let needs = match self.scene.scene_graph.get(body).map(|n| &n.payload) {
            Some(NodePayload::SdfBody {
                mesh_cache_id: None,
                ..
            }) => true,
            Some(NodePayload::SdfBody {
                mesh_cache_id: Some(cid),
                ..
            }) => self.cache.mesh_cache.get(*cid).is_none(),
            _ => false,
        };
        if !needs {
            return;
        }
        let cid = self.cache.mesh_cache.alloc_id();
        let mesh = terrain_placeholder_mesh();
        // source_hash = 0 (placeholder) — content bake/DC will replace with real hash.
        self.cache.mesh_cache.insert_mesh_direct(
            cid,
            mesh,
            0,
            render::mesh_cache::MeshLod::Preview,
        );
        if let Some(NodePayload::SdfBody { mesh_cache_id, .. }) =
            self.scene.scene_graph.get_mut(body).map(|n| &mut n.payload)
        {
            *mesh_cache_id = Some(cid);
        }
    }

    fn cold_tile_has_gpu_mesh(&self, nid: NodeId) -> bool {
        self.gpu
            .as_ref()
            .and_then(|g| g.sdf_mesh_pool.as_ref())
            .map_or(false, |p| p.body_vram_bytes(nid) > 0)
    }

    /// Drop dense CPU grid + SDF GPU mesh pool slot for a cold tile.
    /// Does **not** delete the scene node or clear authored prims.
    fn release_cold_tile_resources(&mut self, nid: NodeId) {
        self.cache.body_vol_cache.release_cold_tile_dense_grid(nid);
        // GPU mesh pool (VRAM) — separate from body_grids; free_slot zeros draw args.
        if let Some(gpu) = self.gpu.as_mut() {
            if let Some(pool) = gpu.sdf_mesh_pool.as_mut() {
                pool.free_slot(&gpu.queue, nid);
            }
        }
        // CPU mesh_cache slot for this body's display mesh (if any).
        let cid = self.scene.scene_graph.get_mut(nid).and_then(|node| {
            match &mut node.payload {
                NodePayload::SdfBody { mesh_cache_id, .. } => mesh_cache_id.take(),
                _ => None,
            }
        });
        if let Some(cid) = cid {
            self.cache.mesh_cache.remove(cid);
        }
    }

    /// User command: create tiles for the resident window around the **current camera**
    /// (and expand `half_window` coverage). Runs in a **transaction** — poll never does this.
    pub(crate) fn expand_terrain_here(&mut self) {
        if self.terrain_world.tiles.is_empty() {
            self.rebuild_terrain_world_from_scene();
        }
        if self.terrain_world.island_group.is_none() || self.terrain_world.tiles.is_empty() {
            self.toast("Expand Terrain — create a Terrain Island / Patch first");
            return;
        }
        let cam = self.viewport.camera.target;
        let center = world_to_tile(cam.x, cam.z);
        let half = self.terrain_world.half_window.clamp(0, 2);
        let desired = desired_resident_tiles(center, half);
        let missing: Vec<(i32, i32)> = desired
            .into_iter()
            .filter(|k| !self.terrain_world.tiles.contains_key(k))
            .collect();
        if missing.is_empty() {
            // Still force residency update for this camera cell.
            self.terrain_world.stream_enabled = true;
            self.sync_terrain_resident_window(true);
            self.toast("Expand Terrain — window already covered; residency refreshed");
            return;
        }
        self.expand_terrain_tiles_transaction(&missing);
        self.terrain_world.stream_enabled = true;
        self.sync_terrain_resident_window(true);
        self.toast(&format!(
            "Expand Terrain — +{} tile(s) around ({}, {})",
            missing.len(),
            center.0,
            center.1
        ));
    }

    /// Create missing tile bodies under the island group — **always** via `transaction`.
    /// Call only from explicit user actions ([`Self::expand_terrain_here`], never from poll).
    pub(crate) fn expand_terrain_tiles_transaction(&mut self, keys: &[(i32, i32)]) {
        if keys.is_empty() {
            return;
        }
        let Some(group) = self.terrain_world.island_group else {
            return;
        };
        if self.scene.scene_graph.get(group).is_none() {
            return;
        }
        let to_add: Vec<(i32, i32)> = keys
            .iter()
            .copied()
            .filter(|k| !self.terrain_world.tiles.contains_key(k))
            .collect();
        if to_add.is_empty() {
            return;
        }
        let n = to_add.len();
        self.transaction(&format!("Expand Terrain (+{n})"), |s| {
            let group = match s.terrain_world.island_group {
                Some(g) if s.scene.scene_graph.get(g).is_some() => g,
                _ => return,
            };
            for (tx, tz) in to_add {
                if s.terrain_world.tiles.contains_key(&(tx, tz)) {
                    continue;
                }
                let uid = s.scene.next_prim_id;
                s.scene.next_prim_id = s.scene.next_prim_id.saturating_add(1);
                let body = insert_terrain_tile(&mut s.scene.scene_graph, group, tx, tz, uid);
                s.cache
                    .body_vol_cache
                    .set_body_resolution_policy(body, DENSE_TILE_RES);
                s.terrain_world.tiles.insert((tx, tz), body);
            }
            s.sync_flat_cache();
            s.mark_dirty(DirtyFlags::SCENE | DirtyFlags::RENDER);
        });
    }

    /// Remove `terrain_world` entries whose NodeIds no longer exist in the graph.
    pub(crate) fn prune_stale_terrain_tiles(&mut self) {
        let dead: Vec<(i32, i32)> = self
            .terrain_world
            .tiles
            .iter()
            .filter(|(_, &nid)| self.scene.scene_graph.get(nid).is_none())
            .map(|(&k, _)| k)
            .collect();
        for k in dead {
            if let Some(nid) = self.terrain_world.tiles.remove(&k) {
                self.cache.body_vol_cache.remove_body(nid);
            }
        }
        if let Some(gid) = self.terrain_world.island_group {
            if self.scene.scene_graph.get(gid).is_none() {
                self.terrain_world.island_group = None;
            }
        }
        if self.terrain_world.tiles.is_empty() {
            self.terrain_world.island_group = None;
            self.terrain_world.stream_enabled = false;
            self.terrain_world.last_center = None;
        }
    }

    /// Subtract a horizontal cylinder through the active terrain tile (demo / acceptance).
    pub(crate) fn carve_terrain_demo_tunnel(&mut self) {
        let Some(body) = self.resolve_terrain_tile_body() else {
            self.toast("Demo Tunnel — select a TerrainTile_* body first");
            return;
        };
        self.transaction("Terrain demo tunnel", |s| {
            let uid = s.scene.next_prim_id;
            s.scene.next_prim_id = s.scene.next_prim_id.saturating_add(1);
            if !append_tunnel_cutter(&mut s.scene.scene_graph, body, uid) {
                s.toast("Demo Tunnel — body is not an SdfBody");
                return;
            }
            s.sync_flat_cache();
            s.rebake_no_undo();
            s.mark_dirty(DirtyFlags::SCENE | DirtyFlags::RENDER);
            s.mark_outliner_dirty();
            s.toast("Demo tunnel cutter added — bake/DC will update the mesh");
        });
    }

    /// Force full-res dense bake + optional Voxel Remesh of the **display** mesh.
    ///
    /// Volume remains source of truth (remesh is display polish only — plan D3).
    /// Remesh runs immediately on the current `mesh_cache` if present; bake dirties
    /// for a fresh DC extract. Remesh failure is non-fatal (toast + bake still runs).
    pub(crate) fn rebuild_terrain_mesh(&mut self) {
        let body = self.resolve_terrain_tile_body().or_else(|| {
            let nid = self.scene.active_body_nid;
            self.scene
                .scene_graph
                .get(nid)
                .and_then(|n| match &n.payload {
                    NodePayload::SdfBody { .. } => Some(nid),
                    _ => None,
                })
        });
        let Some(body) = body else {
            self.toast("Rebuild Mesh — select a terrain tile or SdfBody first");
            return;
        };
        let name = self
            .scene
            .scene_graph
            .get(body)
            .and_then(|n| match &n.payload {
                NodePayload::SdfBody { name, .. } => Some(name.clone()),
                _ => None,
            })
            .unwrap_or_else(|| format!("{body:?}"));

        let remesh_note = self.try_remesh_terrain_display(body);
        if let Some(ms_s) = remesh_note
            .split_whitespace()
            .find(|t| t.ends_with("ms"))
            .and_then(|t| t.trim_end_matches("ms").parse::<f32>().ok())
        {
            self.perf.terrain_last_remesh_ms = ms_s;
        }

        self.cache
            .body_vol_cache
            .set_body_resolution(body, DENSE_TILE_RES);
        self.mark_dirty(DirtyFlags::SCENE | DirtyFlags::RENDER);
        self.toast(&format!(
            "Terrain rebuild — {name} → {DENSE_TILE_RES}³ bake{remesh_note}"
        ));
    }

    /// Voxel-remesh the body's current display mesh (if any). Returns a short toast fragment.
    ///
    /// Display meshes arrive as either:
    /// - **DC quads** in `cpu_mesh` (`insert_mesh_direct`) — use [`HalfEdgeMesh::from_mesh`]
    /// - **triangle soup** in `imported_persistent` / raster (`update_editable_mesh`, GPU
    ///   readback) — `from_mesh` is a point cloud (empty faces); use [`from_triangle_soup`]
    ///   (same bridge as Convert → Mesh).
    fn try_remesh_terrain_display(&mut self, body: NodeId) -> String {
        let Some(cid) = (match self.scene.scene_graph.get(body).map(|n| &n.payload) {
            Some(NodePayload::SdfBody {
                mesh_cache_id: Some(c),
                ..
            }) => Some(*c),
            _ => None,
        }) else {
            return String::new();
        };
        let Some(he) = Self::half_edge_from_mesh_cache(&self.cache.mesh_cache, cid) else {
            return String::new();
        };
        if he.face_count() == 0 {
            return String::new();
        }
        let t0 = std::time::Instant::now();
        let Some(out) = crate::poly::remesh::voxel_remesh_mesh(
            &he,
            DENSE_TILE_RES.min(crate::poly::remesh::REMESH_RES_MAX),
            true,
            0.1,
            1.0,
        ) else {
            return " | remesh skipped (no surface)".into();
        };
        let ms = t0.elapsed().as_secs_f32() * 1000.0;
        let (verts, indices) = out.to_raster_data();
        let tris = (indices.len() / 3) as u32;
        self.cache
            .mesh_cache
            .update_editable_mesh(cid, (verts, indices), tris);
        self.wire_local_cache.remove(&cid.0);
        format!(" | remesh {ms:.0}ms → {tris} tris")
    }

    /// Build a half-edge mesh from mesh_cache (DC quads **or** imported triangle soup).
    fn half_edge_from_mesh_cache(
        mesh_cache: &render::mesh_cache::MeshCache,
        cid: core_math::scene_graph::MeshCacheId,
    ) -> Option<core_math::half_edge::HalfEdgeMesh> {
        use core_math::half_edge::HalfEdgeMesh;
        // Prefer DC topology when present (chunked insert_mesh_direct).
        if let Some(cached) = mesh_cache.get(cid) {
            if !cached.cpu_mesh.quads.is_empty() && !cached.cpu_mesh.vertices.is_empty() {
                let he = HalfEdgeMesh::from_mesh(&cached.cpu_mesh);
                if he.face_count() > 0 {
                    return Some(he);
                }
            }
        }
        // GPU-readback / editable path: positions + triangle indices only.
        let (verts, indices) = mesh_cache.chunk_mesh_data(cid)?;
        if indices.len() < 3 || verts.is_empty() {
            return None;
        }
        let positions: Vec<glam::Vec3> = verts
            .iter()
            .map(|v| glam::Vec3::new(v[0], v[1], v[2]))
            .collect();
        let normals: Vec<glam::Vec3> = verts
            .iter()
            .map(|v| glam::Vec3::new(v[3], v[4], v[5]))
            .collect();
        let tris: Vec<[u32; 3]> = indices
            .chunks(3)
            .filter(|c| c.len() == 3)
            .map(|c| [c[0], c[1], c[2]])
            .collect();
        if tris.is_empty() {
            return None;
        }
        let he = HalfEdgeMesh::from_triangle_soup(&positions, &normals, &tris);
        if he.face_count() == 0 {
            None
        } else {
            Some(he)
        }
    }

    /// Prefer selected/active body if it is a registered terrain tile (NodeId identity).
    fn resolve_terrain_tile_body(&self) -> Option<NodeId> {
        if let Some(nid) = self.sel.usel.active_node() {
            if self.is_terrain_tile_body(nid) {
                return Some(nid);
            }
        }
        for nid in self.sel.usel.selected_nodes() {
            if self.is_terrain_tile_body(nid) {
                return Some(nid);
            }
        }
        let active = self.scene.active_body_nid;
        if self.is_terrain_tile_body(active) {
            Some(active)
        } else {
            None
        }
    }

    /// True when Tool Properties should show the Terrain tile panel.
    fn terrain_tool_params_context(&self) -> bool {
        if self.resolve_terrain_tile_body().is_some() {
            return true;
        }
        if let Some(g) = self.terrain_world.island_group {
            if self.sel.usel.active_node() == Some(g)
                || self.sel.usel.selected_nodes().any(|n| n == g)
            {
                return true;
            }
        }
        // Active SDF body is a registered tile (even if selection is a prim under it).
        self.is_terrain_tile_body(self.scene.active_body_nid)
            || self.is_terrain_tile_body(self.scene.sdf_body_nid)
    }

    /// Tool Properties · Terrain — patch size, add tiles, stream, rebuild, export.
    /// Shift+A only adds a terrain island; all tile ops live here (owner).
    /// Returns true when this panel owns the draw (caller must not fall through).
    pub(crate) fn draw_tool_params_terrain(&mut self, ui: &mut egui::Ui) -> bool {
        use crate::ui_theme as t;
        if !self.terrain_tool_params_context() {
            return false;
        }

        let n_tiles = self.terrain_world.tiles.len();
        let stream = self.terrain_world.stream_enabled;
        let half = self.terrain_world.half_window.clamp(1, 2);

        ui.label(
            egui::RichText::new("Terrain")
                .size(11.0)
                .color(t::TEXT_SECONDARY),
        );
        ui.label(
            egui::RichText::new(format!(
                "{n_tiles} tile(s) · stream {}",
                if stream { "ON" } else { "OFF" }
            ))
            .size(10.0)
            .color(t::TEXT_FAINT),
        );
        ui.add_space(4.0);

        // ── Tile / residency ───────────────────────────────────────────────
        ui.label(egui::RichText::new("Tiles").size(10.0).color(t::TEXT_FAINT));
        let mut half_ui = half;
        let half_label = if half_ui == 1 { "3×3" } else { "5×5" };
        ui.horizontal(|ui| {
            ui.label("Window half");
            if ui
                .add(egui::Slider::new(&mut half_ui, 1..=2).text(half_label))
                .changed()
            {
                self.terrain_world.half_window = half_ui;
            }
        });
        let half_now = self.terrain_world.half_window.clamp(1, 2);
        let patch_btn = if half_now == 1 {
            "Add patch 3×3"
        } else {
            "Add patch 5×5"
        };
        ui.horizontal(|ui| {
            if ui
                .button(patch_btn)
                .on_hover_text("Create a tile patch around origin (stream ON)")
                .clicked()
            {
                self.create_terrain_patch(half_now);
            }
            if ui
                .button("Expand here")
                .on_hover_text("Grow tiles around the camera (undoable)")
                .clicked()
            {
                self.expand_terrain_here();
            }
        });

        ui.add_space(4.0);
        ui.horizontal(|ui| {
            let mut stream_ui = stream;
            if ui
                .checkbox(&mut stream_ui, "Stream residency")
                .on_hover_text("Show/hide tiles around camera (does not create tiles)")
                .changed()
            {
                if stream_ui != stream {
                    self.toggle_terrain_stream();
                }
            }
        });

        ui.add_space(4.0);
        ui.label(egui::RichText::new("Mesh").size(10.0).color(t::TEXT_FAINT));
        ui.horizontal(|ui| {
            if ui
                .button("Rebuild mesh")
                .on_hover_text("Full-res bake + display remesh for active terrain tile")
                .clicked()
            {
                self.rebuild_terrain_mesh();
            }
        });

        ui.add_space(4.0);
        ui.label(egui::RichText::new("Export").size(10.0).color(t::TEXT_FAINT));
        ui.horizontal(|ui| {
            if ui
                .button("glTF tiles…")
                .on_hover_text("Export visible terrain tile meshes as multi-mesh .glb")
                .clicked()
            {
                if let Some(path) = rfd::FileDialog::new()
                    .set_title("Export Terrain Tiles (glTF)")
                    .add_filter("glTF Binary", &["glb"])
                    .set_file_name("terrain_tiles.glb")
                    .save_file()
                {
                    let path_s = path.to_string_lossy().to_string();
                    match self.export_terrain_tiles_glb(&path_s, true) {
                        Ok((n, verts, faces)) => self.toast(&format!(
                            "Exported {n} tile(s): {verts} verts, {faces} tris → {path_s}"
                        )),
                        Err(e) => self.toast(&format!("Terrain export failed: {e}")),
                    }
                }
            }
            if ui
                .button("Height PNG…")
                .on_hover_text("Export selected tile top-surface as 16-bit height map")
                .clicked()
            {
                if let Some(path) = rfd::FileDialog::new()
                    .set_title("Export Terrain Height Map")
                    .add_filter("PNG", &["png"])
                    .set_file_name("terrain_height.png")
                    .save_file()
                {
                    let path_s = path.to_string_lossy().to_string();
                    match self.export_terrain_height_png(&path_s, 256) {
                        Ok((w, h)) => {
                            self.toast(&format!("Exported height map {w}×{h} → {path_s}"))
                        }
                        Err(e) => self.toast(&format!("Height export failed: {e}")),
                    }
                }
            }
        });

        ui.add_space(2.0);
        ui.label(
            egui::RichText::new("Shift+A → Terrain adds a new island. Tile ops stay here.")
                .size(9.0)
                .color(t::TEXT_FAINT),
        );
        true
    }

    // ── Phase 5 export ─────────────────────────────────────────────────────

    /// Export all (or visible-only) terrain tile display meshes as a multi-mesh `.glb`.
    /// Positions are **world space** (body origin applied). Source `.fl4` is unchanged.
    pub(crate) fn export_terrain_tiles_glb(
        &self,
        path: &str,
        visible_only: bool,
    ) -> Result<(usize, usize, usize), String> {
        use io_bridge::gltf_export::{export_glb, ExportMaterial, ExportMesh};

        let meshes = collect_terrain_export_meshes(
            &self.terrain_world,
            &self.scene.scene_graph,
            &self.cache.mesh_cache,
            visible_only,
        );
        if meshes.is_empty() {
            return Err(
                "No terrain tile meshes ready (create island, bake/rebuild, then export)".into(),
            );
        }
        let n = meshes.len();
        let export: Vec<ExportMesh> = meshes
            .into_iter()
            .map(|m| ExportMesh {
                name: m.name,
                positions: m.positions,
                normals: m.normals,
                uvs: m.uvs,
                indices: m.indices,
                material: ExportMaterial {
                    name: "terrain".into(),
                    albedo: [0.45, 0.55, 0.35, 1.0],
                    roughness: 0.85,
                    metallic: 0.0,
                    emission: [0.0; 3],
                },
            })
            .collect();
        let (verts, faces) = export_glb(path, &export)?;
        Ok((n, verts, faces))
    }

    /// Export a height pyramid sample of the selected terrain tile as 16-bit PNG.
    /// Samples top solid surface along +Y (classic engines / heightfield pipelines).
    pub(crate) fn export_terrain_height_png(
        &self,
        path: &str,
        res: u32,
    ) -> Result<(u32, u32), String> {
        let body = self
            .resolve_terrain_tile_body()
            .ok_or_else(|| "Select a terrain tile body first".to_string())?;
        let (w, h, heights) = sample_terrain_height_grid(
            &self.scene.scene_graph,
            body,
            res.max(8).min(1024),
        )
        .ok_or_else(|| "Failed to sample height field for tile".to_string())?;
        write_height_png16(path, w, h, &heights)?;
        Ok((w, h))
    }
}

// ---------------------------------------------------------------------------
// Phase 5 — export helpers (pure; testable without GPU)
// ---------------------------------------------------------------------------

/// World-space mesh payload for one terrain tile (glTF / game pipeline).
#[derive(Clone, Debug)]
pub struct TerrainExportMesh {
    pub name: String,
    pub positions: Vec<Vec3>,
    pub normals: Vec<Vec3>,
    pub uvs: Vec<[f32; 2]>,
    pub indices: Vec<u32>,
    pub tx: i32,
    pub tz: i32,
}

/// Collect display meshes for registered terrain tiles.
/// `visible_only`: skip nodes with `visible == false` (stream cold tiles).
pub fn collect_terrain_export_meshes(
    tw: &TerrainWorld,
    sg: &SceneGraph,
    mesh_cache: &render::mesh_cache::MeshCache,
    visible_only: bool,
) -> Vec<TerrainExportMesh> {
    let mut out = Vec::new();
    let mut keys: Vec<_> = tw.tiles.keys().copied().collect();
    keys.sort_by_key(|&(tx, tz)| (tz, tx));
    for (tx, tz) in keys {
        let Some(&nid) = tw.tiles.get(&(tx, tz)) else {
            continue;
        };
        let Some(node) = sg.get(nid) else {
            continue;
        };
        if visible_only && !node.visible {
            continue;
        }
        let (name, origin, cid) = match &node.payload {
            NodePayload::SdfBody {
                name,
                origin,
                mesh_cache_id: Some(cid),
                ..
            } => (name.clone(), *origin, *cid),
            _ => continue,
        };
        let Some(em) = mesh_cache_to_export_mesh(mesh_cache, cid, &name, origin, tx, tz) else {
            continue;
        };
        out.push(em);
    }
    out
}

fn mesh_cache_to_export_mesh(
    mesh_cache: &render::mesh_cache::MeshCache,
    cid: core_math::scene_graph::MeshCacheId,
    name: &str,
    origin: Vec3,
    tx: i32,
    tz: i32,
) -> Option<TerrainExportMesh> {
    // Prefer DC topology when present.
    if let Some(cached) = mesh_cache.get(cid) {
        if !cached.cpu_mesh.quads.is_empty() && !cached.cpu_mesh.vertices.is_empty() {
            let m = &cached.cpu_mesh;
            let positions: Vec<Vec3> = m.vertices.iter().map(|v| *v + origin).collect();
            let normals = if m.normals.len() == m.vertices.len() {
                m.normals.clone()
            } else {
                vec![Vec3::Y; m.vertices.len()]
            };
            let uvs = vec![[0.0, 0.0]; m.vertices.len()];
            let mut indices = Vec::with_capacity(m.quads.len() * 6);
            for q in &m.quads {
                indices.extend_from_slice(&[q[0], q[1], q[2], q[0], q[2], q[3]]);
            }
            if indices.len() >= 3 {
                return Some(TerrainExportMesh {
                    name: name.to_string(),
                    positions,
                    normals,
                    uvs,
                    indices,
                    tx,
                    tz,
                });
            }
        }
    }
    // GPU-readback / triangle soup path.
    let (verts, indices) = mesh_cache.chunk_mesh_data(cid)?;
    if indices.len() < 3 || verts.is_empty() {
        return None;
    }
    let positions: Vec<Vec3> = verts
        .iter()
        .map(|v| Vec3::new(v[0], v[1], v[2]) + origin)
        .collect();
    let normals: Vec<Vec3> = verts
        .iter()
        .map(|v| Vec3::new(v[3], v[4], v[5]))
        .collect();
    let uvs: Vec<[f32; 2]> = verts.iter().map(|v| [v[6], v[7]]).collect();
    Some(TerrainExportMesh {
        name: name.to_string(),
        positions,
        normals,
        uvs,
        indices: indices.clone(),
        tx,
        tz,
    })
}

/// Sample top solid surface heights over the tile AABB (world XZ).
/// Returns `(w, h, heights)` in meters (world Y). Empty cells use `y_min`.
pub fn sample_terrain_height_grid(
    sg: &SceneGraph,
    body: NodeId,
    res: u32,
) -> Option<(u32, u32, Vec<f32>)> {
    use core_math::field_eval::BodyField;

    let (origin, half) = match sg.get(body).map(|n| &n.payload) {
        Some(NodePayload::SdfBody { origin, .. }) => {
            let he = tile_half_extent() + tile_seam_apron_m();
            (*origin, he)
        }
        _ => return None,
    };
    let field = BodyField::from_graph(sg, body);
    let res = res.max(2);
    let y_min = origin.y - half;
    let y_max = origin.y + half;
    let x0 = origin.x - half;
    let z0 = origin.z - half;
    let dx = (2.0 * half) / res as f32;
    let dz = (2.0 * half) / res as f32;
    let mut heights = vec![y_min; (res * res) as usize];
    for j in 0..res {
        for i in 0..res {
            let x = x0 + (i as f32 + 0.5) * dx;
            let z = z0 + (j as f32 + 0.5) * dz;
            // BodyField is body-local when BODY_LOCAL_PRIMS — sample in local, store world Y.
            let local = Vec3::new(x - origin.x, 0.0, z - origin.z);
            let h_local = sample_top_surface_y(&field, local.x, local.z, -half, half);
            heights[(j * res + i) as usize] = origin.y + h_local;
        }
    }
    let _ = (y_min, y_max); // documentation of band
    Some((res, res, heights))
}

/// Binary-search the highest Y where SDF ≤ 0 along a vertical line (body-local).
fn sample_top_surface_y(field: &core_math::field_eval::BodyField, x: f32, z: f32, y_lo: f32, y_hi: f32) -> f32 {
    // Coarse scan top→bottom for first solid sample, then refine.
    let steps = 48;
    let mut last_air = y_hi;
    let mut found = false;
    let mut y_solid = y_lo;
    for s in 0..=steps {
        let t = s as f32 / steps as f32;
        let y = y_hi + (y_lo - y_hi) * t;
        let d = field.eval_or(Vec3::new(x, y, z), 100.0);
        if d <= 0.0 {
            found = true;
            y_solid = y;
            break;
        }
        last_air = y;
    }
    if !found {
        return y_lo;
    }
    // Refine between last_air and y_solid.
    let mut lo = y_solid;
    let mut hi = last_air;
    for _ in 0..12 {
        let mid = 0.5 * (lo + hi);
        if field.eval_or(Vec3::new(x, mid, z), 100.0) <= 0.0 {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    lo
}

/// Write heights as 16-bit grayscale PNG (min→0, max→65535).
pub fn write_height_png16(path: &str, w: u32, h: u32, heights: &[f32]) -> Result<(), String> {
    if heights.len() != (w as usize) * (h as usize) {
        return Err(format!(
            "height buffer size {} != {}×{}",
            heights.len(),
            w,
            h
        ));
    }
    let mut min_h = f32::MAX;
    let mut max_h = f32::MIN;
    for &v in heights {
        if v.is_finite() {
            min_h = min_h.min(v);
            max_h = max_h.max(v);
        }
    }
    if !min_h.is_finite() || !max_h.is_finite() || (max_h - min_h).abs() < 1e-8 {
        min_h = 0.0;
        max_h = 1.0;
    }
    let span = (max_h - min_h).max(1e-6);
    let mut img = image::ImageBuffer::<image::Luma<u16>, Vec<u16>>::new(w, h);
    for (i, &v) in heights.iter().enumerate() {
        let x = (i as u32) % w;
        let y = (i as u32) / w;
        let t = ((v - min_h) / span).clamp(0.0, 1.0);
        let u = (t * 65535.0).round() as u16;
        img.put_pixel(x, y, image::Luma([u]));
    }
    img.save(path).map_err(|e| format!("PNG write failed: {e}"))?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use core_math::scene_graph::NodePayload;

    #[test]
    fn cell_size_is_half_meter_at_defaults() {
        let c = terrain_cell_size();
        assert!((c - 0.5).abs() < 1e-5, "cell={c}");
    }

    #[test]
    fn sample_terrain_height_grid_over_base_hill() {
        let mut g = SceneGraph::new();
        let ids = insert_terrain_island_into_graph(
            &mut g,
            "TerrainIsland.exp".into(),
            "TerrainTile_0_0".into(),
            7,
        );
        let (w, h, heights) =
            sample_terrain_height_grid(&g, ids.tile_body, 16).expect("height sample");
        assert_eq!(w, 16);
        assert_eq!(h, 16);
        assert_eq!(heights.len(), 16 * 16);
        // Centre height should sit near the BaseHill top surface.
        let mid = heights[8 * 16 + 8];
        assert!(
            (mid - BASE_HILL_TOP_Y).abs() < 2.0,
            "centre height should be near ground top {BASE_HILL_TOP_Y}, mid={mid}"
        );
    }

    #[test]
    fn placeholder_is_single_top_face_not_dual_slab() {
        let m = terrain_placeholder_mesh();
        assert_eq!(m.quads.len(), 1, "placeholder must be top face only (not top+bottom slab)");
        assert_eq!(m.vertices.len(), 4);
        // All verts share the ground top Y.
        for v in &m.vertices {
            assert!(
                (v.y - BASE_HILL_TOP_Y).abs() < 1e-3,
                "placeholder Y should be top surface, got {}",
                v.y
            );
        }
    }

    #[test]
    fn base_hill_bottom_past_lattice_no_underside_isosurface() {
        let p = make_base_hill_prim(1);
        let SdfShape::Box { half_extents, .. } = p.shape else {
            panic!("box");
        };
        let top = p.position.y + half_extents.y;
        let bot = p.position.y - half_extents.y;
        let (lat_mn, lat_mx) = tile_lattice_bake_bounds_body_local();
        assert!(
            (top - BASE_HILL_TOP_Y).abs() < 1e-3,
            "top={top} want {BASE_HILL_TOP_Y}"
        );
        // Underside outside the bake volume → DC only extracts the ground top (one layer).
        assert!(
            bot < lat_mn.y,
            "bottom {bot} must be past lattice min Y {}",
            lat_mn.y
        );
        assert!(
            top < lat_mx.y,
            "top {top} must stay inside lattice max Y {}",
            lat_mx.y
        );
    }

    #[test]
    fn base_hill_field_single_surface_at_top() {
        use core_math::field_eval::BodyField;
        let mut g = SceneGraph::new();
        let ids = insert_terrain_island_into_graph(
            &mut g,
            "TerrainIsland.surf".into(),
            "TerrainTile_0_0".into(),
            99,
        );
        let field = BodyField::from_graph(&g, ids.tile_body);
        // Above top → air
        let d_air = field.eval_or(Vec3::new(0.0, BASE_HILL_TOP_Y + 2.0, 0.0), 100.0);
        assert!(d_air > 0.0, "above ground must be air, d={d_air}");
        // Just below top → solid
        let d_solid = field.eval_or(Vec3::new(0.0, BASE_HILL_TOP_Y - 1.0, 0.0), 100.0);
        assert!(d_solid < 0.0, "below ground top must be solid, d={d_solid}");
        // Mid-lattice deep bulk still solid (not a hollow shell)
        let d_deep = field.eval_or(Vec3::new(0.0, -10.0, 0.0), 100.0);
        assert!(d_deep < 0.0, "deep bulk must be solid, d={d_deep}");
    }

    #[test]
    fn write_height_png16_roundtrip_size() {
        let dir = std::env::temp_dir().join("azra_terrain_height_test.png");
        let path = dir.to_string_lossy().to_string();
        let heights = vec![0.0f32, 1.0, 2.0, 3.0];
        write_height_png16(&path, 2, 2, &heights).expect("write png");
        let meta = std::fs::metadata(&path).expect("file exists");
        assert!(meta.len() > 32, "png should be non-trivial");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn collect_terrain_export_meshes_empty_without_cache() {
        let mut g = SceneGraph::new();
        let ids = insert_terrain_island_into_graph(
            &mut g,
            "TerrainIsland.ex2".into(),
            "TerrainTile_0_0".into(),
            8,
        );
        let mut tw = TerrainWorld::new();
        tw.register_tile(0, 0, ids.tile_body, ids.group);
        let cache = render::mesh_cache::MeshCache::new();
        let meshes = collect_terrain_export_meshes(&tw, &g, &cache, false);
        assert!(
            meshes.is_empty(),
            "no mesh_cache → nothing to export yet"
        );
    }

    #[test]
    fn world_tile_roundtrip_origin() {
        // Phase-1 tile 0_0 is body-centred at world origin → covers [-32, +32).
        assert_eq!(world_to_tile(0.0, 0.0), (0, 0));
        assert_eq!(world_to_tile(31.9, 0.0), (0, 0));
        assert_eq!(world_to_tile(32.0, 0.0), (1, 0));
        assert_eq!(world_to_tile(-32.0, 0.0), (0, 0));
        assert_eq!(world_to_tile(-32.1, 0.0), (-1, 0));
        let o = tile_origin_world(2, -3);
        assert_eq!(o, Vec3::new(128.0, 0.0, -192.0));
        assert_eq!(tile_body_origin(1, 0), Vec3::new(64.0, 0.0, 0.0));
    }

    #[test]
    fn desired_resident_window_sizes() {
        let w0 = desired_resident_tiles((0, 0), 0);
        assert_eq!(w0, vec![(0, 0)]);
        let w1 = desired_resident_tiles((0, 0), 1);
        assert_eq!(w1.len(), 9);
        assert!(w1.contains(&(1, 1)) && w1.contains(&(-1, -1)));
        let w2 = desired_resident_tiles((5, -2), 2);
        assert_eq!(w2.len(), 25);
        assert!(w2.contains(&(5, -2)) && w2.contains(&(7, 0)) && w2.contains(&(3, -4)));
    }

    #[test]
    fn insert_patch_three_by_three() {
        let mut g = SceneGraph::new();
        let (group, tiles, next_uid) =
            insert_terrain_patch(&mut g, "TerrainIsland.009".into(), 1, 10);
        assert_eq!(tiles.len(), 9);
        assert_eq!(next_uid, 19);
        assert!(tiles.contains_key(&(0, 0)));
        assert!(tiles.contains_key(&(1, -1)));
        let body = tiles[&(1, 0)];
        assert_eq!(g.get(body).and_then(|n| n.parent), Some(group));
        match &g.get(body).unwrap().payload {
            NodePayload::SdfBody { name, origin, .. } => {
                assert_eq!(name, "TerrainTile_1_0");
                assert_eq!(*origin, tile_body_origin(1, 0));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn insert_island_structure_pure_sdf_no_displace() {
        let mut g = SceneGraph::new();
        let ids = insert_terrain_island_into_graph(
            &mut g,
            "TerrainIsland.001".into(),
            "TerrainTile_0_0".into(),
            42,
        );
        let group = g.get(ids.group).expect("group");
        match &group.payload {
            NodePayload::Group { name } => assert!(name.starts_with(TERRAIN_ISLAND_NAME_PREFIX)),
            other => panic!("expected Group, got {other:?}"),
        }
        let body = g.get(ids.tile_body).expect("tile");
        match &body.payload {
            NodePayload::SdfBody {
                name, primitives, ..
            } => {
                assert_eq!(name, "TerrainTile_0_0");
                assert_eq!(primitives.len(), 1);
                assert_eq!(primitives[0].prim_uid, 42);
                assert_eq!(primitives[0].name, "BaseHill");
                // L4: no image displace / multi-mod stack on Phase-1 base.
                assert!(
                    !primitives[0].needs_cpu_full_stack_bake(),
                    "Phase-1 base must stay on GPU/dense fast path (no image displace)"
                );
                match &primitives[0].shape {
                    SdfShape::Box { half_extents, .. } => {
                        let want = tile_half_extent() + tile_seam_apron_m();
                        assert!(
                            (half_extents.x - want).abs() < 1e-3,
                            "BaseHill must fill footprint+apron for stitch, got {} want {want}",
                            half_extents.x
                        );
                        let want_hy = base_hill_half_y();
                        assert!(
                            (half_extents.y - want_hy).abs() < 1e-3,
                            "BaseHill half_y should match lattice-past-floor solid, got {} want {want_hy}",
                            half_extents.y
                        );
                    }
                    other => panic!("expected Box hill, got {other:?}"),
                }
                // Top of ground near BASE_HILL_TOP_Y (center + half_y).
                let top = primitives[0].position.y + base_hill_half_y();
                assert!(
                    (top - BASE_HILL_TOP_Y).abs() < 1e-3,
                    "BaseHill top should be near {BASE_HILL_TOP_Y}, got {top}"
                );
                // Bottom past lattice floor — critical anti dual-slab invariant.
                let bot = primitives[0].position.y - base_hill_half_y();
                let (lat_mn, _) = tile_lattice_bake_bounds_body_local();
                assert!(
                    bot < lat_mn.y - 1e-3,
                    "BaseHill bottom must be past lattice floor (bot={bot} lat_min_y={})",
                    lat_mn.y
                );
            }
            other => panic!("expected SdfBody, got {other:?}"),
        }
        // Group is parent of tile.
        assert_eq!(
            g.get(ids.tile_body).and_then(|n| n.parent),
            Some(ids.group)
        );
    }

    #[test]
    fn dense_res_stays_below_sparse_gate() {
        // Sparse path starts at res ≥ 512; Phase 1 must stay dense-remeshable.
        assert!(DENSE_TILE_RES <= 256);
        assert!(DENSE_TILE_RES < 512);
    }

    #[test]
    fn parse_tile_name_display_only_and_body_local_aabb() {
        // Display-name helper still works for defaults / outliner labels.
        assert_eq!(parse_terrain_tile_name("TerrainTile_0_0"), Some((0, 0)));
        assert_eq!(parse_terrain_tile_name("TerrainTile_2_-1"), Some((2, -1)));
        assert_eq!(parse_terrain_tile_name("Body.001"), None);
        let (mn, mx) = tile_aabb_body_local();
        let p = clamp_to_tile_aabb(Vec3::new(1.0e6, 0.0, -1.0e6), mn, mx);
        assert!(p.x <= mx.x && p.x >= mn.x);
        assert!(p.z <= mx.z && p.z >= mn.z);
        let q = Vec3::new(1.0, 2.0, 3.0);
        assert_eq!(clamp_to_tile_aabb(q, mn, mx), q);
    }

    #[test]
    fn tile_aabb_world_uses_tx_tz() {
        let (mn0, mx0) = tile_aabb_world(0, 0);
        let (mn1, mx1) = tile_aabb_world(1, 0);
        // Shifted by TILE_METERS on X for tile (1,0).
        assert!((mn1.x - mn0.x - TILE_METERS).abs() < 1e-4);
        assert!((mx1.x - mx0.x - TILE_METERS).abs() < 1e-4);
        assert!((mn1.z - mn0.z).abs() < 1e-4);
        // Body-local is origin-centred for every tile.
        let (bl_mn, _) = tile_aabb_body_local();
        assert!((mn0.x - bl_mn.x).abs() < 1e-4); // tile 0 origin at 0
        let o1 = tile_body_origin(1, 0);
        assert!((mn1.x - (o1.x + bl_mn.x)).abs() < 1e-4);
    }

    #[test]
    fn origin_lattice_identity_survives_rename() {
        let mut g = SceneGraph::new();
        let (group, tiles, _) = insert_terrain_patch(&mut g, "TerrainIsland.020".into(), 1, 1);
        // Rename every tile body to something that parse_terrain_tile_name rejects.
        for &body in tiles.values() {
            if let Some(NodePayload::SdfBody { name, .. }) =
                g.get_mut(body).map(|n| &mut n.payload)
            {
                *name = "RenamedHill".into();
            }
        }
        // Origin lattice + BaseHill still recovers the world.
        let tw = scan_terrain_world_from_graph(&g);
        assert_eq!(tw.tiles.len(), 9, "rename must not drop tiles from scan");
        assert_eq!(tw.island_group, Some(group));
        for (&key, &body) in &tiles {
            assert_eq!(tw.tiles.get(&key), Some(&body));
        }
    }

    #[test]
    fn registry_contains_body_not_name() {
        let mut app = FourthLayerApp::new_headless();
        app.create_terrain_island();
        let body = app.scene.active_body_nid;
        assert!(app.is_terrain_tile_body(body));
        // Rename display name away from TerrainTile_* prefix.
        if let Some(NodePayload::SdfBody { name, .. }) =
            app.scene.scene_graph.get_mut(body).map(|n| &mut n.payload)
        {
            *name = "MyCustomName".into();
        }
        assert!(
            app.is_terrain_tile_body(body),
            "registry is NodeId-based — rename must not drop clamp identity"
        );
        // Clamp still applies.
        let clamped = app.clamp_prim_pos_for_terrain_body(body, Vec3::new(1.0e6, 0.0, 0.0));
        let (mn, mx) = tile_aabb_body_local();
        assert!(clamped.x <= mx.x && clamped.x >= mn.x);
    }

    #[test]
    fn scan_world_after_patch_then_empty_graph_clears() {
        let mut g = SceneGraph::new();
        let (group, tiles, _) = insert_terrain_patch(&mut g, "TerrainIsland.010".into(), 1, 1);
        let tw = scan_terrain_world_from_graph(&g);
        assert_eq!(tw.tiles.len(), 9);
        assert_eq!(tw.island_group, Some(group));
        assert!(tw.stream_enabled, "multi-tile scan enables stream");
        assert!(tiles.keys().all(|k| tw.tiles.contains_key(k)));

        // Simulate undo of Create Patch: full graph replace with empty scene.
        let g2 = SceneGraph::new();
        let tw2 = scan_terrain_world_from_graph(&g2);
        assert!(tw2.tiles.is_empty());
        assert!(tw2.island_group.is_none());
        assert!(!tw2.stream_enabled);
    }

    /// Owner repro class: Create Patch → Ctrl+Z must not leave terrain_world pointing at
    /// deleted NodeIds (heal via scan / rebuild after undo).
    #[test]
    fn create_patch_undo_heals_terrain_world() {
        let mut app = FourthLayerApp::new_headless();
        app.create_terrain_patch(1);
        assert_eq!(app.terrain_world.tiles.len(), 9, "patch registers 9 tiles");
        assert!(app.terrain_world.stream_enabled);
        // Every map entry must resolve in the live graph.
        for &nid in app.terrain_world.tiles.values() {
            assert!(
                app.scene.scene_graph.get(nid).is_some(),
                "live tile {nid:?} missing from graph"
            );
        }
        app.undo();
        // After undo, map is rebuilt from the restored (empty / pre-patch) graph.
        for &nid in app.terrain_world.tiles.values() {
            assert!(
                app.scene.scene_graph.get(nid).is_some(),
                "stale NodeId {nid:?} survived undo"
            );
        }
        // Typical empty start: no terrain tiles remain.
        assert!(
            app.terrain_world.tiles.is_empty(),
            "expected empty terrain_world after undoing sole patch, got {} tiles",
            app.terrain_world.tiles.len()
        );
    }

    /// Poll/sync must never grow the tile set (no undo spam / no mid-drag commit_episode).
    #[test]
    fn poll_sync_never_expands_tile_count() {
        let mut app = FourthLayerApp::new_headless();
        app.create_terrain_patch(1);
        assert_eq!(app.terrain_world.tiles.len(), 9);
        app.viewport.camera.target = Vec3::new(320.0, 0.0, 0.0);
        app.sync_terrain_resident_window(true); // force residency only
        assert_eq!(
            app.terrain_world.tiles.len(),
            9,
            "poll/sync path must not create tiles"
        );
        app.sync_terrain_resident_window(false);
        assert_eq!(app.terrain_world.tiles.len(), 9);
    }

    /// Explicit Expand Terrain Here is undoable; poll after undo does not re-spawn.
    #[test]
    fn expand_here_undoable_poll_does_not_reexpand() {
        let mut app = FourthLayerApp::new_headless();
        app.create_terrain_patch(1);
        assert_eq!(app.terrain_world.tiles.len(), 9);
        app.viewport.camera.target = Vec3::new(320.0, 0.0, 0.0); // ~tile (5,0)
        app.expand_terrain_here();
        let after = app.terrain_world.tiles.len();
        assert!(after > 9, "expand_here should add tiles, got {after}");
        app.undo();
        assert_eq!(app.terrain_world.tiles.len(), 9);
        // Poll-style sync must not re-expand.
        app.sync_terrain_resident_window(false);
        app.sync_terrain_resident_window(true);
        assert_eq!(
            app.terrain_world.tiles.len(),
            9,
            "after undo, sync must not re-create undid tiles"
        );
    }

    #[test]
    fn may_switch_center_deadband_and_cooldown() {
        let mut tw = TerrainWorld::new();
        tw.last_center = Some((0, 0));
        tw.last_switch_at = Some(std::time::Instant::now());
        // Just switched — cooldown blocks even a far camera.
        assert!(!tw.may_switch_center(Vec3::new(320.0, 0.0, 0.0), (5, 0), false));
        assert!(tw.may_switch_center(Vec3::new(320.0, 0.0, 0.0), (5, 0), true)); // force
        tw.last_switch_at = Some(
            std::time::Instant::now()
                - std::time::Duration::from_millis(STREAM_CENTER_COOLDOWN_MS + 50),
        );
        // On the boundary of tile 0 vs 1 — still closer to (0,0) origin → blocked by deadband.
        assert!(!tw.may_switch_center(Vec3::new(33.0, 0.0, 0.0), (1, 0), false));
        // Deep into tile 1 — allowed.
        assert!(tw.may_switch_center(Vec3::new(64.0, 0.0, 0.0), (1, 0), false));
    }

    /// Plan success criterion (editor): carve a tunnel through a hill — air inside
    /// the bore, solid remaining in the bulk (BodyField, no GPU).
    #[test]
    fn tunnel_carve_through_hill_air_inside_solid_bulk() {
        use core_math::field_eval::BodyField;

        let mut g = SceneGraph::new();
        let ids = insert_terrain_island_into_graph(
            &mut g,
            "TerrainIsland.001".into(),
            "TerrainTile_0_0".into(),
            1,
        );
        assert!(append_tunnel_cutter(&mut g, ids.tile_body, 2));

        let field = BodyField::from_graph(&g, ids.tile_body);
        assert!(!field.is_empty());

        // Tunnel axis sample (body-local origin / slight +Y) must be outside solid (d > 0).
        let d_air = field.eval_or(Vec3::new(0.0, 0.5, 0.0), 100.0);
        assert!(
            d_air > 0.05,
            "tunnel centre should be air (positive SDF), got {d_air}"
        );
        // Far along +X still in bore (cylinder half_height ≈ 25.6).
        let d_air2 = field.eval_or(Vec3::new(10.0, 0.5, 0.0), 100.0);
        assert!(
            d_air2 > 0.0,
            "tunnel along +X should stay air, got {d_air2}"
        );
        // Off to the side in the mound bulk — still solid (negative).
        let d_solid = field.eval_or(Vec3::new(0.0, -1.0, 12.0), 100.0);
        assert!(
            d_solid < -0.05,
            "hill bulk beside tunnel should stay solid (neg SDF), got {d_solid}"
        );
    }

    /// Phase 2b: HUD gauges latch tile/vis counts after island create + refresh.
    #[test]
    fn refresh_terrain_perf_gauges_latches_counts() {
        let mut app = FourthLayerApp::new_headless();
        app.refresh_terrain_perf_gauges();
        assert_eq!(app.perf.terrain_tiles, 0);
        assert_eq!(app.perf.terrain_vis, 0);

        app.create_terrain_island();
        app.refresh_terrain_perf_gauges();
        assert_eq!(app.perf.terrain_tiles, 1);
        assert_eq!(app.perf.terrain_vis, 1);
        // Placeholder LOD installs a coarse box — tris > 0 before DC.
        assert!(app.perf.terrain_tris > 0, "placeholder should contribute tris");
        assert!(app.perf.terrain_dirty_bodies >= 1);

        app.create_terrain_patch(1);
        app.refresh_terrain_perf_gauges();
        assert_eq!(app.perf.terrain_tiles, 9);
        assert!(app.perf.terrain_vis >= 1);
    }

    /// Remesh bridge accepts DC quads (insert_mesh_direct path).
    #[test]
    fn half_edge_from_mesh_cache_accepts_dc_quads() {
        let mut app = FourthLayerApp::new_headless();
        let cid = app.cache.mesh_cache.alloc_id();
        // Unit cube as quads (same shape DC would emit).
        let mesh = core_math::mesh::Mesh {
            vertices: vec![
                Vec3::new(-1.0, -1.0, -1.0),
                Vec3::new(1.0, -1.0, -1.0),
                Vec3::new(1.0, 1.0, -1.0),
                Vec3::new(-1.0, 1.0, -1.0),
                Vec3::new(-1.0, -1.0, 1.0),
                Vec3::new(1.0, -1.0, 1.0),
                Vec3::new(1.0, 1.0, 1.0),
                Vec3::new(-1.0, 1.0, 1.0),
            ],
            normals: vec![Vec3::Y; 8],
            quads: vec![
                [0, 1, 2, 3],
                [4, 5, 6, 7],
                [0, 1, 5, 4],
                [2, 3, 7, 6],
                [0, 3, 7, 4],
                [1, 2, 6, 5],
            ],
        };
        app.cache.mesh_cache.insert_mesh_direct(
            cid,
            mesh,
            0,
            render::mesh_cache::MeshLod::High,
        );
        let he = FourthLayerApp::half_edge_from_mesh_cache(&app.cache.mesh_cache, cid)
            .expect("DC quads must build half-edge");
        assert!(he.face_count() >= 6);
    }

    /// Remesh bridge accepts triangle-soup raster (GPU readback / update_editable_mesh).
    #[test]
    fn half_edge_from_mesh_cache_accepts_triangle_soup() {
        let mut app = FourthLayerApp::new_headless();
        let cid = app.cache.mesh_cache.alloc_id();
        // Two tris forming a quad (no cpu_mesh.quads — editable path).
        let verts: Vec<[f32; 8]> = vec![
            [-1.0, 0.0, -1.0, 0.0, 1.0, 0.0, 0.0, 0.0],
            [1.0, 0.0, -1.0, 0.0, 1.0, 0.0, 0.0, 0.0],
            [1.0, 0.0, 1.0, 0.0, 1.0, 0.0, 0.0, 0.0],
            [-1.0, 0.0, 1.0, 0.0, 1.0, 0.0, 0.0, 0.0],
        ];
        let indices = vec![0, 1, 2, 0, 2, 3];
        app.cache
            .mesh_cache
            .update_editable_mesh(cid, (verts, indices), 2);
        let he = FourthLayerApp::half_edge_from_mesh_cache(&app.cache.mesh_cache, cid)
            .expect("triangle soup must build half-edge");
        assert!(he.face_count() >= 1);
        // from_mesh alone would be empty faces — regression guard for the soup path.
        if let Some(cached) = app.cache.mesh_cache.get(cid) {
            assert!(
                cached.cpu_mesh.quads.is_empty(),
                "editable path stores empty quads"
            );
            let bare = core_math::half_edge::HalfEdgeMesh::from_mesh(&cached.cpu_mesh);
            assert_eq!(
                bare.face_count(),
                0,
                "from_mesh-only path must fail on soup so half_edge_from_mesh_cache is required"
            );
        }
    }

    /// Settle remesh waits for a newer mesh stamp (post-DC), then remeshes once.
    #[test]
    fn settle_remesh_waits_for_mesh_stamp_then_runs() {
        let mut app = FourthLayerApp::new_headless();
        app.create_terrain_island();
        let body = app.scene.active_body_nid;

        // Island create dirties the body for first bake — settle remesh waits for clean.
        app.cache.body_vol_cache.dirty_bodies.remove(&body);

        // Placeholder mesh is already installed at create — capture its stamp.
        let cid = match app.scene.scene_graph.get(body).map(|n| &n.payload) {
            Some(NodePayload::SdfBody {
                mesh_cache_id: Some(c),
                ..
            }) => *c,
            _ => panic!("expected placeholder mesh from create"),
        };
        let stamp0 = app.cache.mesh_cache.get(cid).unwrap().content_stamp;
        assert!(stamp0 > 0, "placeholder has content_stamp");

        // Request while only placeholder present — wait_stamp = stamp0; poll waits for DC.
        app.request_terrain_display_remesh(body);
        assert_eq!(
            app.terrain_world.pending_display_remesh.get(&body),
            Some(&stamp0)
        );
        app.poll_terrain_display_remesh();
        assert!(
            app.terrain_world.pending_display_remesh.contains_key(&body),
            "must wait for content_stamp advance (post-DC)"
        );

        // Simulate post-bake DC by re-inserting (new content_stamp).
        let mesh2 = core_math::mesh::Mesh {
            vertices: vec![
                Vec3::new(-2.0, -2.0, -2.0),
                Vec3::new(2.0, -2.0, -2.0),
                Vec3::new(2.0, 2.0, -2.0),
                Vec3::new(-2.0, 2.0, -2.0),
                Vec3::new(-2.0, -2.0, 2.0),
                Vec3::new(2.0, -2.0, 2.0),
                Vec3::new(2.0, 2.0, 2.0),
                Vec3::new(-2.0, 2.0, 2.0),
            ],
            normals: vec![Vec3::Y; 8],
            quads: vec![
                [0, 1, 2, 3],
                [4, 5, 6, 7],
                [0, 1, 5, 4],
                [2, 3, 7, 6],
                [0, 3, 7, 4],
                [1, 2, 6, 5],
            ],
        };
        app.cache.mesh_cache.insert_mesh_direct(
            cid,
            mesh2,
            1,
            render::mesh_cache::MeshLod::High,
        );
        let stamp2 = app.cache.mesh_cache.get(cid).unwrap().content_stamp;
        assert_ne!(stamp2, stamp0);

        // First poll: stamp ready → spawn rayon job (pending cleared, job in flight).
        app.poll_terrain_display_remesh();
        assert!(
            !app.terrain_world.pending_display_remesh.contains_key(&body),
            "spawn should take the pending entry"
        );
        assert!(
            !app.terrain_remesh_jobs.is_empty() || app.perf.terrain_last_remesh_ms > 0.0,
            "job should be in flight or already finished"
        );
        // Drain until result lands (rayon, ~80ms release / a few s debug).
        let t0 = std::time::Instant::now();
        while t0.elapsed() < std::time::Duration::from_secs(30) {
            app.poll_terrain_display_remesh();
            if app.terrain_remesh_jobs.is_empty() && app.perf.terrain_last_remesh_ms > 0.0 {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(
            app.terrain_remesh_jobs.is_empty(),
            "async remesh job should complete"
        );
        assert!(
            app.perf.terrain_last_remesh_ms > 0.0,
            "HUD remesh ms should latch"
        );
        assert!(app.cache.mesh_cache.get(cid).unwrap().triangle_count > 0);
    }

    /// Full-res bake toast queues async settle remesh; draft does not.
    #[test]
    fn bake_toast_queues_async_settle_only_when_full_res() {
        let mut app = FourthLayerApp::new_headless();
        app.create_terrain_island();
        let body = app.scene.active_body_nid;
        app.cache.body_vol_cache.set_draft_mode(true);
        app.cache.body_vol_cache.last_finished_bake = Some((body, 12.0));
        app.maybe_toast_terrain_bake();
        assert!(
            app.terrain_world.pending_display_remesh.is_empty(),
            "draft bake must not queue remesh"
        );
        app.cache.body_vol_cache.set_draft_mode(false);
        app.cache.body_vol_cache.last_finished_bake = Some((body, 40.0));
        app.maybe_toast_terrain_bake();
        assert!(
            app.terrain_world.pending_display_remesh.contains_key(&body),
            "full-res bake must queue async settle remesh"
        );
        assert!(app.perf.terrain_last_bake_ms > 0.0);
    }

    /// Stroke-active blocks remesh poll (never freeze LMB).
    #[test]
    fn settle_remesh_blocked_while_stroke_active() {
        let mut app = FourthLayerApp::new_headless();
        app.create_terrain_island();
        let body = app.scene.active_body_nid;
        let cid = app.cache.mesh_cache.alloc_id();
        let mesh = core_math::mesh::Mesh {
            vertices: vec![
                Vec3::new(-1.0, -1.0, -1.0),
                Vec3::new(1.0, -1.0, -1.0),
                Vec3::new(1.0, 1.0, -1.0),
                Vec3::new(-1.0, 1.0, -1.0),
                Vec3::new(-1.0, -1.0, 1.0),
                Vec3::new(1.0, -1.0, 1.0),
                Vec3::new(1.0, 1.0, 1.0),
                Vec3::new(-1.0, 1.0, 1.0),
            ],
            normals: vec![Vec3::Y; 8],
            quads: vec![
                [0, 1, 2, 3],
                [4, 5, 6, 7],
                [0, 1, 5, 4],
                [2, 3, 7, 6],
                [0, 3, 7, 4],
                [1, 2, 6, 5],
            ],
        };
        app.cache.mesh_cache.insert_mesh_direct(
            cid,
            mesh,
            0,
            render::mesh_cache::MeshLod::High,
        );
        if let Some(NodePayload::SdfBody { mesh_cache_id, .. }) =
            app.scene.scene_graph.get_mut(body).map(|n| &mut n.payload)
        {
            *mesh_cache_id = Some(cid);
        }
        // Simulate post-bake clean body; wait_stamp=0 so any mesh stamp > 0 is "new".
        app.cache.body_vol_cache.dirty_bodies.remove(&body);
        app.terrain_world.pending_display_remesh.insert(body, 0);
        app.sculpt.stroke_active = true;
        app.poll_terrain_display_remesh();
        assert!(
            app.terrain_world.pending_display_remesh.contains_key(&body),
            "must not remesh during LMB stroke"
        );
        app.sculpt.stroke_active = false;
        app.poll_terrain_display_remesh();
        // Spawned (pending removed) or still pending only if spawn failed — expect spawn.
        assert!(
            !app.terrain_world.pending_display_remesh.contains_key(&body)
                || !app.terrain_remesh_jobs.is_empty(),
            "after stroke ends, remesh should spawn"
        );
        // Don't wait for full remesh here — stamp-wait test covers completion.
        // Drop in-flight so we don't leak across tests (optional).
        let t0 = std::time::Instant::now();
        while !app.terrain_remesh_jobs.is_empty()
            && t0.elapsed() < std::time::Duration::from_secs(30)
        {
            app.poll_terrain_display_remesh();
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    /// Rebuild Terrain Mesh: remesh display (if present) + pin dense res + dirty bake.
    #[test]
    fn rebuild_terrain_mesh_remeshes_and_dirties() {
        let mut app = FourthLayerApp::new_headless();
        app.create_terrain_island();
        let body = app.scene.active_body_nid;
        assert!(app.is_terrain_tile_body(body));

        // Attach a solid cube so remesh has a surface.
        let cid = app.cache.mesh_cache.alloc_id();
        let mesh = core_math::mesh::Mesh {
            vertices: vec![
                Vec3::new(-2.0, -2.0, -2.0),
                Vec3::new(2.0, -2.0, -2.0),
                Vec3::new(2.0, 2.0, -2.0),
                Vec3::new(-2.0, 2.0, -2.0),
                Vec3::new(-2.0, -2.0, 2.0),
                Vec3::new(2.0, -2.0, 2.0),
                Vec3::new(2.0, 2.0, 2.0),
                Vec3::new(-2.0, 2.0, 2.0),
            ],
            normals: vec![Vec3::Y; 8],
            quads: vec![
                [0, 1, 2, 3],
                [4, 5, 6, 7],
                [0, 1, 5, 4],
                [2, 3, 7, 6],
                [0, 3, 7, 4],
                [1, 2, 6, 5],
            ],
        };
        app.cache.mesh_cache.insert_mesh_direct(
            cid,
            mesh,
            0,
            render::mesh_cache::MeshLod::High,
        );
        if let Some(NodePayload::SdfBody { mesh_cache_id, .. }) =
            app.scene.scene_graph.get_mut(body).map(|n| &mut n.payload)
        {
            *mesh_cache_id = Some(cid);
        }

        let note = app.try_remesh_terrain_display(body);
        assert!(
            note.contains("remesh") && note.contains("tris"),
            "expected remesh toast fragment, got {note:?}"
        );
        let after = app
            .cache
            .mesh_cache
            .get(cid)
            .map(|m| m.triangle_count)
            .unwrap_or(0);
        assert!(after > 0, "remesh should leave a non-empty display mesh");

        // Full rebuild also pins dense res (dirties bake).
        app.cache.body_vol_cache.dirty_bodies.clear(); // isolate
        app.rebuild_terrain_mesh();
        assert!(
            app.cache.body_vol_cache.dirty_bodies.contains(&body),
            "rebuild must dirty the tile for dense bake"
        );
    }

    #[test]
    fn terrain_res_ring_policy() {
        assert_eq!(terrain_res_for_chebyshev(0), DENSE_TILE_RES);
        assert_eq!(terrain_res_for_chebyshev(1), RING1_TILE_RES);
        assert_eq!(terrain_res_for_chebyshev(2), RING2_TILE_RES);
        assert_eq!(terrain_res_for_chebyshev(-3), RING2_TILE_RES);
        assert!(RING2_TILE_RES <= RING1_TILE_RES && RING1_TILE_RES <= DENSE_TILE_RES);
    }

    /// Stream centre gets full res; outer resident ring is coarser (policy, not user lock).
    #[test]
    fn stream_residency_applies_near_far_resolution() {
        let mut app = FourthLayerApp::new_headless();
        app.create_terrain_patch(2); // 5×5
        assert_eq!(app.terrain_world.tiles.len(), 25);
        app.viewport.camera.target = Vec3::ZERO;
        app.sync_terrain_resident_window(true);
        let c0 = *app.terrain_world.tiles.get(&(0, 0)).unwrap();
        let c1 = *app.terrain_world.tiles.get(&(1, 0)).unwrap();
        let c2 = *app.terrain_world.tiles.get(&(2, 0)).unwrap();
        assert_eq!(
            app.cache.body_vol_cache.body_resolution(c0),
            Some(DENSE_TILE_RES)
        );
        assert_eq!(
            app.cache.body_vol_cache.body_resolution(c1),
            Some(RING1_TILE_RES)
        );
        assert_eq!(
            app.cache.body_vol_cache.body_resolution(c2),
            Some(RING2_TILE_RES)
        );
        // User lock must not be set by stream policy.
        app.cache
            .body_vol_cache
            .set_body_resolution(c2, DENSE_TILE_RES);
        app.sync_terrain_resident_window(true);
        assert_eq!(
            app.cache.body_vol_cache.body_resolution(c2),
            Some(DENSE_TILE_RES),
            "user-locked res must stick across stream sync"
        );
    }

    /// Island create pins lattice bounds + installs placeholder display mesh.
    #[test]
    fn island_create_pins_lattice_and_placeholder_mesh() {
        let mut app = FourthLayerApp::new_headless();
        app.create_terrain_island();
        let body = app.scene.active_body_nid;
        let lattice = tile_lattice_bake_bounds_body_local();
        let bounds = app
            .cache
            .body_vol_cache
            .body_bounds
            .get(&body)
            .copied()
            .expect("lattice body_bounds pinned");
        assert!((bounds.0 - lattice.0).length() < 1e-4);
        assert!((bounds.1 - lattice.1).length() < 1e-4);

        let cid = match app.scene.scene_graph.get(body).map(|n| &n.payload) {
            Some(NodePayload::SdfBody {
                mesh_cache_id: Some(c),
                ..
            }) => *c,
            other => panic!("expected placeholder mesh_cache_id, got {other:?}"),
        };
        let cached = app
            .cache
            .mesh_cache
            .get(cid)
            .expect("placeholder in mesh_cache");
        assert!(
            !cached.cpu_mesh.quads.is_empty(),
            "placeholder must have display quads"
        );
        assert_eq!(cached.source_hash, 0, "placeholder uses source_hash=0");
    }

    /// Placeholder is a no-op when a real mesh is already attached.
    #[test]
    fn ensure_placeholder_skips_existing_mesh() {
        let mut app = FourthLayerApp::new_headless();
        app.create_terrain_island();
        let body = app.scene.active_body_nid;
        let cid0 = match app.scene.scene_graph.get(body).map(|n| &n.payload) {
            Some(NodePayload::SdfBody {
                mesh_cache_id: Some(c),
                ..
            }) => *c,
            _ => panic!("placeholder missing"),
        };
        app.ensure_terrain_placeholder_display(body);
        let cid1 = match app.scene.scene_graph.get(body).map(|n| &n.payload) {
            Some(NodePayload::SdfBody {
                mesh_cache_id: Some(c),
                ..
            }) => *c,
            _ => panic!("mesh_cache_id cleared"),
        };
        assert_eq!(cid0, cid1, "must not re-alloc placeholder over existing mesh");
    }

    /// Two adjacent tiles: SDF must stay solid across the shared boundary (stitched field).
    #[test]
    fn adjacent_tiles_field_solid_across_seam() {
        use core_math::field_eval::BodyField;

        let mut g = SceneGraph::new();
        let (_group, tiles, _) = insert_terrain_patch(&mut g, "TerrainIsland.stitch".into(), 1, 1);
        let b0 = tiles[&(0, 0)];
        let b1 = tiles[&(1, 0)];

        // Bake prims include neighbour hill (dual membership) — evaluate in tile0 local frame.
        let prims0 = collect_terrain_bake_prims(&g, b0);
        assert!(
            prims0.len() > 1,
            "tile0 must include neighbour BaseHill for seam; got {}",
            prims0.len()
        );
        let field0 = BodyField::from_prims(&prims0, 0, 0, Vec3::ZERO);
        // Shared face in world is at x = +half; tile0 local x = +half.
        let seam_x = tile_half_extent();
        for z in [-8.0_f32, 0.0, 8.0] {
            let d = field0.eval_or(Vec3::new(seam_x, 0.0, z), 100.0);
            assert!(
                d < -0.05,
                "tile0 field at seam ({seam_x},0,{z}) should be solid (neg), got {d}"
            );
        }
        let prims1 = collect_terrain_bake_prims(&g, b1);
        let field1 = BodyField::from_prims(&prims1, 0, 0, Vec3::ZERO);
        // Same world seam in tile1 local: x = -half.
        for z in [-8.0_f32, 0.0, 8.0] {
            let d = field1.eval_or(Vec3::new(-seam_x, 0.0, z), 100.0);
            assert!(
                d < -0.05,
                "tile1 field at seam (-{seam_x},0,{z}) should be solid (neg), got {d}"
            );
        }
    }

    /// Adjacent BaseHills must **overlap** in world XZ (no air gap between tiles).
    #[test]
    fn adjacent_base_hills_overlap_for_stitch() {
        let hill = make_base_hill_prim(1);
        let he = match &hill.shape {
            SdfShape::Box { half_extents, .. } => *half_extents,
            other => panic!("{other:?}"),
        };
        let o0 = tile_body_origin(0, 0);
        let o1 = tile_body_origin(1, 0);
        // World max X of tile0 hill vs world min X of tile1 hill.
        let t0_max = o0.x + hill.position.x + he.x;
        let t1_min = o1.x + hill.position.x - he.x;
        let overlap = t0_max - t1_min;
        assert!(
            overlap + 1e-4 >= terrain_cell_size(),
            "adjacent BaseHills must overlap ≥1 cell for stitch; gap/overlap={overlap} (negative = gap)"
        );
        // Same on Z for (0,0) vs (0,1).
        let o1z = tile_body_origin(0, 1);
        let t0_max_z = o0.z + hill.position.z + he.z;
        let t1_min_z = o1z.z + hill.position.z - he.z;
        let overlap_z = t0_max_z - t1_min_z;
        assert!(
            overlap_z + 1e-4 >= terrain_cell_size(),
            "Z-neighbour BaseHills must overlap ≥1 cell; overlap_z={overlap_z}"
        );
    }

    /// L2: lattice bake bounds overlap by ≥1 cell on shared faces.
    #[test]
    fn lattice_bake_bounds_overlap_one_cell() {
        let cell = terrain_cell_size();
        let (mn, mx) = tile_lattice_bake_bounds_body_local();
        let half = tile_half_extent();
        // Extends past geometric half by exactly the apron.
        assert!((mx.x - half - cell).abs() < 1e-5, "apron x+");
        assert!((-half - cell - mn.x).abs() < 1e-5, "apron x-");
        // Two adjacent tiles in world: tile0 body-local max.x and tile1 body-local min.x
        // relative to world share an overlap of 2*apron on the shared face midline... 
        // Tile0 origin 0, tile1 origin TILE_METERS. Shared plane at +half world.
        // Tile0 samples to +half+apron; tile1 (local -half-apron → world half-apron).
        // Overlap width = (half+apron) - (half-apron) = 2*apron ≥ 1 cell when apron≥0.5 cell.
        // We use apron = 1 cell → overlap 2 cells on the shared strip.
        let o0 = tile_body_origin(0, 0);
        let o1 = tile_body_origin(1, 0);
        let t0_max_x = o0.x + mx.x;
        let t1_min_x = o1.x + mn.x;
        let overlap = t0_max_x - t1_min_x;
        assert!(
            overlap + 1e-4 >= cell,
            "shared-face overlap {overlap} must be ≥1 cell ({cell})"
        );
    }

    /// L2: straddling prim on tile (1,0) is collected into tile (0,0) bake prim list.
    #[test]
    fn collect_terrain_bake_includes_straddling_neighbour_prim() {
        let mut g = SceneGraph::new();
        let (_group, tiles, _) = insert_terrain_patch(&mut g, "TerrainIsland.030".into(), 1, 1);
        let body0 = tiles[&(0, 0)];
        let body1 = tiles[&(1, 0)];
        assert!(is_terrain_bake_body(&g, body0));
        assert!(is_terrain_bake_body(&g, body1));

        // Place a subtract box on tile1, near the shared face so it overlaps tile0 lattice.
        // Tile1 body-local x ≈ -half + small → near left edge (shared with tile0).
        let half = tile_half_extent();
        let cell = terrain_cell_size();
        let uid = 9001u32;
        {
            let Some(NodePayload::SdfBody { primitives, .. }) =
                g.get_mut(body1).map(|n| &mut n.payload)
            else {
                panic!("tile1 not SdfBody");
            };
            let mut cutter = SdfPrimitive::with_uid(
                "SeamCutter".into(),
                SdfShape::Box {
                    half_extents: Vec3::new(4.0, 4.0, 4.0),
                    bevel: BoxBevel::default(),
                },
                // Deep into the shared apron toward tile0.
                Vec3::new(-half + cell * 0.25, 0.0, 0.0),
                uid,
            );
            cutter.operation = BooleanOp::Subtract { k: 0.0 };
            primitives.push(cutter);
        }
        g.ensure_body_child_order(body1);

        let prims0 = collect_terrain_bake_prims(&g, body0);
        assert!(
            prims0.iter().any(|p| p.prim_uid == uid),
            "tile0 bake must include straddling prim from tile1; got {} prims",
            prims0.len()
        );
        // Position on tile0 should be world-shifted into tile0 local.
        let p = prims0.iter().find(|p| p.prim_uid == uid).unwrap();
        // World pos ≈ tile1_origin + local = TILE_METERS + (-half + 0.25*cell)
        // Tile0 local = world - 0.
        let expected_x = TILE_METERS + (-half + cell * 0.25);
        assert!(
            (p.position.x - expected_x).abs() < 1e-3,
            "expected x≈{expected_x}, got {}",
            p.position.x
        );

        // Own-only collect must NOT include the neighbour prim.
        let own0 = crate::bake_utils::collect_body_prims_recursive(&g, body0);
        assert!(!own0.iter().any(|p| p.prim_uid == uid));
    }

    /// Dirty fan-out marks Chebyshev neighbour tiles.
    #[test]
    fn terrain_dirty_fanout_marks_neighbours() {
        let mut app = FourthLayerApp::new_headless();
        app.create_terrain_patch(1);
        let body0 = *app.terrain_world.tiles.get(&(0, 0)).expect("0,0");
        let body1 = *app.terrain_world.tiles.get(&(1, 0)).expect("1,0");
        app.cache.body_vol_cache.dirty_bodies.clear();
        app.cache
            .body_vol_cache
            .mark_body_dirty_terrain_fanout(body0, &app.scene.scene_graph);
        assert!(app.cache.body_vol_cache.dirty_bodies.contains(&body0));
        assert!(
            app.cache.body_vol_cache.dirty_bodies.contains(&body1),
            "neighbour (1,0) must be dirtied for seam re-bake"
        );
    }
}
