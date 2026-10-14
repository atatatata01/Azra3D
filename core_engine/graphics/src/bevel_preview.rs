// core_engine/graphics/src/bevel_preview.rs
// SDF Bevel Preview — GPU structs, spatial grid builder, edge extraction.

use glam::{Vec3, UVec3, Mat4};

pub const BEVEL_PREVIEW_WGSL: &str = include_str!("bevel_preview.wgsl");

// ─────────────────────────────────────
// GPU-SIDE STRUCTURES (must match WGSL)
// ─────────────────────────────────────

#[repr(C)]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct GpuBevelEdge {
    pub face_normal_a: [f32; 3],
    pub face_offset_a: f32,
    pub face_normal_b: [f32; 3],
    pub face_offset_b: f32,
    pub edge_midpoint: [f32; 3],
    pub edge_half_length: f32,
    pub edge_direction: [f32; 3],
    pub dihedral_angle: f32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct GpuBevelParams {
    // Bevel parameters
    pub width: f32,
    pub depth: f32,
    pub shape: f32,
    pub profile_mode: u32,

    // Selection
    pub edge_count: u32,
    pub _pad0: u32,
    pub _pad1: u32,
    pub _pad2: u32,

    // Grid
    pub grid_dims: [u32; 3],
    pub grid_total_cells: u32,
    pub bbox_min: [f32; 3],
    pub _pad3: f32,
    pub bbox_max: [f32; 3],
    pub _pad4: f32,

    // Camera (mat4 = 16 floats each)
    pub view_proj: [f32; 16],
    pub view_proj_inv: [f32; 16],
    pub camera_pos: [f32; 3],
    pub max_march_steps: u32,

    // Viewport
    pub viewport_size: [f32; 2],
    pub near_plane: f32,
    pub far_plane: f32,

    // Lighting
    pub light_dir: [f32; 3],
    pub _pad5: f32,
    pub light_color: [f32; 3],
    pub ambient: f32,

    // Appearance
    pub bevel_color: [f32; 3],
    pub bevel_roughness: f32,

    // Tiling (S21.10): pixel offset of dispatched tile region
    pub tile_offset: [f32; 2],
    // Normal blending (S21.11): blend width at bevel/poly boundary
    pub normal_blend_width: f32,
    pub _pad6: f32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct GpuGridCell {
    pub offset: u32,
    pub count: u32,
}

/// Profile modes matching WGSL constants.
#[derive(Debug, Clone, Copy, PartialEq)]
#[repr(u32)]
pub enum BevelProfile {
    Chamfer = 0,
    Arc     = 1,
    Ogee    = 2,
    Step    = 3,
}

// ─────────────────────────────────────
// SPATIAL GRID BUILDER
// ─────────────────────────────────────

pub struct SpatialGrid {
    pub cells: Vec<GpuGridCell>,
    pub edge_indices: Vec<u32>,
    pub dims: UVec3,
}

pub fn build_spatial_grid(
    edges: &[GpuBevelEdge],
    bbox_min: Vec3,
    bbox_max: Vec3,
    bevel_width: f32,
    grid_resolution: u32,
) -> SpatialGrid {
    // Guard: zero resolution or no edges → empty grid (avoids division by zero / u32 underflow)
    if grid_resolution == 0 || edges.is_empty() {
        return SpatialGrid { cells: vec![], edge_indices: vec![], dims: UVec3::ZERO };
    }
    let extent = bbox_max - bbox_min;
    // Guard: degenerate bounds (single point / collinear) → NaN cell_size
    if extent.x <= f32::EPSILON || extent.y <= f32::EPSILON || extent.z <= f32::EPSILON {
        return SpatialGrid { cells: vec![], edge_indices: vec![], dims: UVec3::ZERO };
    }
    let dims = UVec3::splat(grid_resolution);
    let total_cells = (dims.x * dims.y * dims.z) as usize;
    let cell_size = extent / dims.as_vec3();

    let mut cell_lists: Vec<Vec<u32>> = vec![Vec::new(); total_cells];

    for (edge_idx, edge) in edges.iter().enumerate() {
        let midpoint = Vec3::from(edge.edge_midpoint);
        let half_len = edge.edge_half_length;
        let dir = Vec3::from(edge.edge_direction);

        let edge_start = midpoint - dir * half_len;
        let edge_end = midpoint + dir * half_len;
        let edge_min = edge_start.min(edge_end) - Vec3::splat(bevel_width);
        let edge_max = edge_start.max(edge_end) + Vec3::splat(bevel_width);

        let cell_min = world_to_cell(edge_min, bbox_min, cell_size, dims);
        let cell_max = world_to_cell(edge_max, bbox_min, cell_size, dims);

        for z in cell_min.z..=cell_max.z {
            for y in cell_min.y..=cell_max.y {
                for x in cell_min.x..=cell_max.x {
                    let ci = (x + y * dims.x + z * dims.x * dims.y) as usize;
                    if ci < total_cells {
                        cell_lists[ci].push(edge_idx as u32);
                    }
                }
            }
        }
    }

    let mut cells = Vec::with_capacity(total_cells);
    let mut edge_indices = Vec::new();
    let mut offset = 0u32;

    for list in &cell_lists {
        cells.push(GpuGridCell { offset, count: list.len() as u32 });
        edge_indices.extend_from_slice(list);
        offset += list.len() as u32;
    }

    SpatialGrid { cells, edge_indices, dims }
}

fn world_to_cell(pos: Vec3, bbox_min: Vec3, cell_size: Vec3, dims: UVec3) -> UVec3 {
    let normalized = ((pos - bbox_min) / cell_size).max(Vec3::ZERO);
    UVec3::new(
        (normalized.x as u32).min(dims.x - 1),
        (normalized.y as u32).min(dims.y - 1),
        (normalized.z as u32).min(dims.z - 1),
    )
}

// ─────────────────────────────────────
// EDGE EXTRACTION FROM HALF-EDGE MESH
// ─────────────────────────────────────

use core_math::half_edge::{HalfEdgeMesh, HalfEdgeId, NO_FACE};

/// Extract bevel edge data from selected edges of a HalfEdgeMesh.
/// Returns (gpu_edges, bbox_min, bbox_max) in world space.
pub fn extract_bevel_edges(
    mesh: &HalfEdgeMesh,
    world_transform: Mat4,
    selected_edges: &std::collections::HashSet<u32>,
) -> (Vec<GpuBevelEdge>, Vec3, Vec3) {
    let mut gpu_edges = Vec::with_capacity(selected_edges.len());
    let mut bbox_min = Vec3::splat(f32::MAX);
    let mut bbox_max = Vec3::splat(f32::MIN);

    let normal_mat = world_transform.inverse().transpose();

    for &edge_id in selected_edges {
        let he_id = HalfEdgeId(edge_id);
        let he = mesh.he(he_id);

        // Get adjacent faces
        let (f1, f2) = mesh.edge_faces(he_id);
        if f1 == NO_FACE { continue; } // Skip degenerate
        if f2 == NO_FACE { continue; } // Skip boundary edges (need 2 faces for bevel)

        // Vertex positions in world space
        let src_id = mesh.he_source(he_id);
        let dst_id = he.vertex;
        let src_local = mesh.vert(src_id).position;
        let dst_local = mesh.vert(dst_id).position;
        let src = world_transform.transform_point3(src_local);
        let dst = world_transform.transform_point3(dst_local);

        // Edge geometry
        let midpoint = (src + dst) * 0.5;
        let edge_vec = dst - src;
        let edge_len = edge_vec.length();
        if edge_len < 1e-6 { continue; }
        let direction = edge_vec / edge_len;

        // Face normals in world space (transform by inverse-transpose)
        let n1_local = mesh.face(f1).normal;
        let n2_local = mesh.face(f2).normal;
        let n1 = normal_mat.transform_vector3(n1_local).normalize_or_zero();
        let n2 = normal_mat.transform_vector3(n2_local).normalize_or_zero();

        // Plane offsets
        let offset_a = n1.dot(src);
        let offset_b = n2.dot(src);

        // Dihedral angle
        let dihedral = n1.dot(n2).clamp(-1.0, 1.0).acos();

        gpu_edges.push(GpuBevelEdge {
            face_normal_a: n1.to_array(),
            face_offset_a: offset_a,
            face_normal_b: n2.to_array(),
            face_offset_b: offset_b,
            edge_midpoint: midpoint.to_array(),
            edge_half_length: edge_len * 0.5,
            edge_direction: direction.to_array(),
            dihedral_angle: dihedral,
        });

        bbox_min = bbox_min.min(src).min(dst);
        bbox_max = bbox_max.max(src).max(dst);
    }

    (gpu_edges, bbox_min, bbox_max)
}

/// Build GpuBevelParams from current state.
pub fn build_params(
    width: f32,
    depth: f32,
    shape: f32,
    profile: BevelProfile,
    edge_count: u32,
    grid_dims: UVec3,
    bbox_min: Vec3,
    bbox_max: Vec3,
    view_proj: Mat4,
    camera_pos: Vec3,
    viewport_w: f32,
    viewport_h: f32,
) -> GpuBevelParams {
    let expanded_min = bbox_min - Vec3::splat(width * 1.5);
    let expanded_max = bbox_max + Vec3::splat(width * 1.5);

    GpuBevelParams {
        width,
        depth,
        shape,
        profile_mode: profile as u32,
        edge_count,
        _pad0: 0, _pad1: 0, _pad2: 0,
        grid_dims: grid_dims.to_array(),
        grid_total_cells: grid_dims.x * grid_dims.y * grid_dims.z,
        bbox_min: expanded_min.to_array(),
        _pad3: 0.0,
        bbox_max: expanded_max.to_array(),
        _pad4: 0.0,
        view_proj: view_proj.to_cols_array(),
        view_proj_inv: view_proj.inverse().to_cols_array(),
        camera_pos: camera_pos.to_array(),
        max_march_steps: 64,
        viewport_size: [viewport_w, viewport_h],
        near_plane: 0.01,
        far_plane: 100.0,
        light_dir: [0.3, -0.7, 0.5],
        _pad5: 0.0,
        light_color: [1.0, 0.98, 0.95],
        ambient: 0.15,
        bevel_color: [0.5, 0.5, 0.52],
        bevel_roughness: 0.4,
        tile_offset: [0.0, 0.0],
        normal_blend_width: 0.005,
        _pad6: 0.0,
    }
}

/// Project the bevel AABB to screen-space and return (tile_min, tile_max) in workgroup coords.
/// Workgroups are 8x8 pixels. Returns (min, max) inclusive workgroup indices.
pub fn compute_screen_tiles(
    bbox_min: Vec3,
    bbox_max: Vec3,
    view_proj: Mat4,
    viewport_w: f32,
    viewport_h: f32,
) -> ([u32; 2], [u32; 2]) {
    let corners = [
        Vec3::new(bbox_min.x, bbox_min.y, bbox_min.z),
        Vec3::new(bbox_max.x, bbox_min.y, bbox_min.z),
        Vec3::new(bbox_min.x, bbox_max.y, bbox_min.z),
        Vec3::new(bbox_max.x, bbox_max.y, bbox_min.z),
        Vec3::new(bbox_min.x, bbox_min.y, bbox_max.z),
        Vec3::new(bbox_max.x, bbox_min.y, bbox_max.z),
        Vec3::new(bbox_min.x, bbox_max.y, bbox_max.z),
        Vec3::new(bbox_max.x, bbox_max.y, bbox_max.z),
    ];

    let mut screen_min_x = viewport_w;
    let mut screen_min_y = viewport_h;
    let mut screen_max_x = 0.0f32;
    let mut screen_max_y = 0.0f32;
    let mut any_visible = false;

    for &c in &corners {
        let clip = view_proj * glam::Vec4::new(c.x, c.y, c.z, 1.0);
        if clip.w <= 0.0 {
            // Behind camera — conservatively assume full screen
            return ([0, 0], [((viewport_w as u32) + 7) / 8, ((viewport_h as u32) + 7) / 8]);
        }
        let ndc_x = clip.x / clip.w;
        let ndc_y = clip.y / clip.w;
        let sx = (ndc_x * 0.5 + 0.5) * viewport_w;
        let sy = (1.0 - (ndc_y * 0.5 + 0.5)) * viewport_h;
        screen_min_x = screen_min_x.min(sx);
        screen_min_y = screen_min_y.min(sy);
        screen_max_x = screen_max_x.max(sx);
        screen_max_y = screen_max_y.max(sy);
        any_visible = true;
    }

    if !any_visible {
        return ([0, 0], [0, 0]);
    }

    // Clamp to viewport and add margin (2 tiles for safety)
    let margin = 16.0;
    screen_min_x = (screen_min_x - margin).max(0.0);
    screen_min_y = (screen_min_y - margin).max(0.0);
    screen_max_x = (screen_max_x + margin).min(viewport_w);
    screen_max_y = (screen_max_y + margin).min(viewport_h);

    let tile_min_x = (screen_min_x as u32) / 8;
    let tile_min_y = (screen_min_y as u32) / 8;
    let tile_max_x = ((screen_max_x as u32) + 7) / 8;
    let tile_max_y = ((screen_max_y as u32) + 7) / 8;

    ([tile_min_x, tile_min_y], [tile_max_x, tile_max_y])
}
