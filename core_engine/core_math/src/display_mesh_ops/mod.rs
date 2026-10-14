//! Display-mesh (post-DC) modifier ops — pure, ordered fold over triangle buffers.
//!
//! **Not** `mesh_ops::MeshOp` (that is poly HalfEdge edit). This is Object-Mode display polish
//! after Dual Contouring / surface extraction (see `MODIFIER_SYSTEM_PLAN.md`).
//!
//! Multi-eval: same `DisplayMeshOp` list → hash / fold / tests. No UI, no GPU here.

use glam::Vec3;
use serde::{Deserialize, Serialize};
use std::hash::{Hash, Hasher};

mod adjacency;
pub use adjacency::build_vertex_adjacency;
mod normals;

mod op_impls;
mod modifier_fold;
pub use modifier_fold::{
    apply_display_mesh_op, apply_display_mesh_op_ctx, fold_display_mesh, fold_display_mesh_cached,
    fold_display_mesh_with_ctx, DisplayMeshPrefixCache,
};
pub use op_impls::{
    build_target_edge_length, mesh_analytical_feature_project, mesh_avg_edge_length,
    mesh_flat_relax_sdf_snap, mesh_vertex_importance, sample_sdf_edge_shape_map,
    sample_sdf_vertex_curvature,
};
#[cfg(test)]
use op_impls::weld_by_distance;

// ============================================================
// Triangle display mesh (CPU)
// ============================================================

/// Indexed triangle mesh used for post-DC display fold (positions + normals + indices).
#[derive(Clone, Debug, Default)]
pub struct DisplayMesh {
    pub positions: Vec<Vec3>,
    pub normals: Vec<Vec3>,
    pub indices: Vec<u32>, // triangle list, 3 per face
}

impl DisplayMesh {
    pub fn empty() -> Self {
        Self::default()
    }

    pub fn triangle_count(&self) -> usize {
        self.indices.len() / 3
    }

    pub fn vertex_count(&self) -> usize {
        self.positions.len()
    }

    /// Build from AZRA DC `Mesh` (quads → tris).
    pub fn from_dc_mesh(mesh: &crate::mesh::Mesh) -> Self {
        let positions = mesh.vertices.clone();
        let normals = if mesh.normals.len() == mesh.vertices.len() {
            mesh.normals.clone()
        } else {
            vec![Vec3::Y; mesh.vertices.len()]
        };
        let mut indices = Vec::with_capacity(mesh.quads.len() * 6);
        for q in &mesh.quads {
            indices.extend_from_slice(&[q[0], q[1], q[2], q[0], q[2], q[3]]);
        }
        Self {
            positions,
            normals,
            indices,
        }
    }

    /// From flat raster vertex format `[px,py,pz,nx,ny,nz,u,v]` + triangle indices.
    pub fn from_raster(verts: &[[f32; 8]], indices: &[u32]) -> Self {
        let mut positions = Vec::with_capacity(verts.len());
        let mut normals = Vec::with_capacity(verts.len());
        for v in verts {
            positions.push(Vec3::new(v[0], v[1], v[2]));
            normals.push(Vec3::new(v[3], v[4], v[5]));
        }
        Self {
            positions,
            normals,
            indices: indices.to_vec(),
        }
    }

    /// To raster vertex format for GPU upload.
    pub fn to_raster(&self) -> (Vec<[f32; 8]>, Vec<u32>) {
        let mut verts = Vec::with_capacity(self.positions.len());
        for i in 0..self.positions.len() {
            let p = self.positions[i];
            let n = self.normals.get(i).copied().unwrap_or(Vec3::Y);
            verts.push([p.x, p.y, p.z, n.x, n.y, n.z, 0.0, 0.0]);
        }
        (verts, self.indices.clone())
    }
}

// ============================================================
// Authoring record (UI / serde / undo) + runtime op
// ============================================================

/// Decimate quality / guidance mode.
///
/// Inspired by industry tools:
/// - **Master** ≈ ZBrush Decimation Master (importance map + adaptive collapse)
/// - **Quality** ≈ Blender Decimate → Collapse (QEM + shape)
/// - **Planar** ≈ Blender Decimate → Planar (melt flats only)
/// - **Fast** = quick flat-first length cost
/// - **Field** = Quality + optional SDF curvature assist
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Hash, Default)]
#[repr(u8)]
pub enum DecimateMode {
    /// Length / flatness — interactive scrub.
    Fast = 0,
    /// QEM + flat-first dihedral (Blender Collapse family).
    Quality = 1,
    /// Quality + SDF \|H\| assist (needs BodyField; else Quality).
    Field = 2,
    /// Only dissolve near-coplanar regions (Blender Planar family). Angle = crease_angle_deg.
    Planar = 3,
    /// Decimation Master–style: mesh importance map + QEM + strong flat-first.
    #[default]
    Master = 4,
}

impl DecimateMode {
    pub fn label(self) -> &'static str {
        match self {
            DecimateMode::Fast => "Fast",
            DecimateMode::Quality => "Quality (Collapse)",
            DecimateMode::Field => "Field (SDF assist)",
            DecimateMode::Planar => "Planar",
            DecimateMode::Master => "Master (adaptive)",
        }
    }

    pub fn all() -> &'static [DecimateMode] {
        &[
            DecimateMode::Master,
            DecimateMode::Quality,
            DecimateMode::Planar,
            DecimateMode::Fast,
            DecimateMode::Field,
        ]
    }
}

/// Full Decimate controls (authoring + runtime). All new fields are `#[serde(default)]`-safe.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq)]
pub struct DecimateParams {
    /// Keep fraction of triangles (0.05..=1.0).
    pub ratio: f32,
    #[serde(default)]
    pub mode: DecimateMode,
    /// Dihedral angle (degrees): edges sharper than this are treated as creases.
    /// Lower = protect more edges. Default 40°.
    #[serde(default = "default_crease_angle")]
    pub crease_angle_deg: f32,
    /// How hard to protect mesh creases (0 = ignore dihedral, 1 = default, 2 = strong).
    #[serde(default = "default_one")]
    pub crease_strength: f32,
    /// How hard to protect open boundary edges (0 = off, 1 = default).
    #[serde(default = "default_one")]
    pub boundary_strength: f32,
    /// SDF curvature weight multiplier in Field mode (0 = ignore field H, 1 = default).
    #[serde(default = "default_one")]
    pub field_strength: f32,
    /// SDF sample radius as multiple of average edge length (0.25..=2.0). Default 0.75.
    #[serde(default = "default_field_radius")]
    pub field_radius: f32,
    /// Stop collapsing when QEM edge cost exceeds this (0 = ratio-only stop).
    /// World-ish units squared error; try 1e-4..1e-2 on typical body scale.
    #[serde(default)]
    pub max_error: f32,
    /// **Principle #1 strength:** how hard to melt flat surfaces first (0 = off, 1 = mild, 2.5 = strong).
    /// Higher → fewer tris on flats, more retained on curvature.
    #[serde(default = "default_planar_bias")]
    pub planar_bias: f32,
    /// **AZRA edge-shape protect** (0 = off). Uses SDF anisotropic curvature + graph
    /// distance-to-feature so main hard edges stay straight / soft-K edges stay dense.
    /// Only meaningful in Field mode with a BodyField.
    #[serde(default)]
    pub edge_shape_strength: f32,
    /// **Flat starve** (0 = off). Extra melt power on planar / low-feature regions so the
    /// tri budget floods into edge bands. Pairs with `edge_shape_strength`.
    #[serde(default)]
    pub flat_starve: f32,
    /// Soft-feature thickness in world units (soft-K / bevel). Widens distance-to-edge band.
    /// 0 = auto from mesh avg edge.
    #[serde(default)]
    pub soft_feature_world: f32,
    /// **Min edge length** (world units, 0 = off). Prefer collapsing shorter edges when
    /// chain turn is below `curve_angle_deg` — kills dense zippers on straight edges while
    /// keeping shape. Pairs with edge-shape.
    #[serde(default)]
    pub min_edge_len: f32,
    /// **Curve angle budget** (degrees, 0 = off). Keep vertices where the edge chain turns
    /// more than this (round surfaces / soft-K rings). Straight runs collapse freely under
    /// `min_edge_len`. Typical 8–15°.
    #[serde(default)]
    pub curve_angle_deg: f32,
    /// Use **target edge-length field** ℓ(x) (built from edge protect + min edge). Prefer
    /// collapse when current edge ≪ ℓ; protect when edge is already near ℓ on features.
    #[serde(default)]
    pub use_edge_length_field: bool,
    /// After fold: Laplacian on flats + SDF project (kill fans, hold surface).
    #[serde(default)]
    pub flat_relax: bool,
}

fn default_crease_angle() -> f32 {
    35.0
}
fn default_one() -> f32 {
    1.0
}
fn default_planar_bias() -> f32 {
    2.5
}
fn default_field_radius() -> f32 {
    0.75
}

impl Default for DecimateParams {
    fn default() -> Self {
        Self {
            ratio: 0.5,
            // Master: Decimation Master–style importance + flat-first (best for DC display).
            mode: DecimateMode::Master,
            crease_angle_deg: 35.0,
            crease_strength: 1.5,
            boundary_strength: 1.0,
            field_strength: 0.5,
            field_radius: 0.75,
            max_error: 0.0,
            planar_bias: 2.5,
            edge_shape_strength: 0.0,
            flat_starve: 0.0,
            soft_feature_world: 0.0,
            min_edge_len: 0.0,
            curve_angle_deg: 0.0,
            use_edge_length_field: false,
            flat_relax: false,
        }
    }
}

impl DecimateParams {
    pub fn clamped(self) -> Self {
        Self {
            ratio: self.ratio.clamp(0.05, 1.0),
            mode: self.mode,
            crease_angle_deg: self.crease_angle_deg.clamp(1.0, 120.0),
            crease_strength: self.crease_strength.clamp(0.0, 4.0),
            boundary_strength: self.boundary_strength.clamp(0.0, 4.0),
            field_strength: self.field_strength.clamp(0.0, 4.0),
            field_radius: self.field_radius.clamp(0.15, 3.0),
            max_error: self.max_error.max(0.0),
            planar_bias: self.planar_bias.clamp(0.0, 4.0),
            edge_shape_strength: self.edge_shape_strength.clamp(0.0, 4.0),
            flat_starve: self.flat_starve.clamp(0.0, 4.0),
            soft_feature_world: self.soft_feature_world.max(0.0),
            min_edge_len: self.min_edge_len.max(0.0),
            curve_angle_deg: self.curve_angle_deg.clamp(0.0, 45.0),
            use_edge_length_field: self.use_edge_length_field,
            flat_relax: self.flat_relax,
        }
    }

    pub fn hash_into(self, h: &mut impl Hasher) {
        hash_f32(self.ratio, h);
        (self.mode as u8).hash(h);
        hash_f32(self.crease_angle_deg, h);
        hash_f32(self.crease_strength, h);
        hash_f32(self.boundary_strength, h);
        hash_f32(self.field_strength, h);
        hash_f32(self.field_radius, h);
        hash_f32(self.max_error, h);
        hash_f32(self.planar_bias, h);
        hash_f32(self.edge_shape_strength, h);
        hash_f32(self.flat_starve, h);
        hash_f32(self.soft_feature_world, h);
        hash_f32(self.min_edge_len, h);
        hash_f32(self.curve_angle_deg, h);
        self.use_edge_length_field.hash(h);
        self.flat_relax.hash(h);
    }
}

/// Optional context for field-guided mesh ops (post-DC fold).
#[derive(Clone, Copy, Default)]
pub struct DisplayMeshFoldCtx<'a> {
    /// Body-local SDF program — same frame as DC mesh positions.
    pub field: Option<&'a crate::field_eval::BodyField>,
}

/// Runtime op — only enabled ops are compiled into the fold list.
#[derive(Clone, Debug, PartialEq)]
pub enum DisplayMeshOp {
    /// Reflect geometry across axis planes through `center` (axis bits: 1=X, 2=Y, 4=Z).
    Mirror {
        axis_bits: u8,
        center: Vec3,
        weld: bool,
        weld_threshold: f32,
    },
    SmoothGeometry {
        iterations: u32,
        factor: f32,
    },
    SmoothNormals,
    /// Keep ~`params.ratio` of triangles. See [`DecimateMode`] / [`DecimateParams`].
    Decimate { params: DecimateParams },
}

/// Authoring card for body mesh stack (serde-safe, UI-owned fields).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct DisplayMeshModRecord {
    pub id: u32,
    pub enabled: bool,
    pub expanded: bool,
    pub kind: DisplayMeshModKind,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub enum DisplayMeshModKind {
    Mirror {
        axis_bits: u8,
        center: Vec3,
        weld: bool,
        weld_threshold: f32,
    },
    SmoothGeometry {
        iterations: u32,
        factor: f32,
    },
    SmoothNormals,
    /// Keep fraction of triangles + feature controls. Serde: flat fields for back-compat.
    Decimate {
        ratio: f32,
        #[serde(default)]
        mode: DecimateMode,
        #[serde(default = "default_crease_angle")]
        crease_angle_deg: f32,
        #[serde(default = "default_one")]
        crease_strength: f32,
        #[serde(default = "default_one")]
        boundary_strength: f32,
        #[serde(default = "default_one")]
        field_strength: f32,
        #[serde(default = "default_field_radius")]
        field_radius: f32,
        #[serde(default)]
        max_error: f32,
        #[serde(default = "default_planar_bias")]
        planar_bias: f32,
    },
    /// Preset: Smooth Geometry then Smooth Normals (compiles to two runtime ops).
    ClayPolish {
        iterations: u32,
        factor: f32,
    },
}

impl DisplayMeshModKind {
    /// Pack Decimate authoring fields into runtime params.
    pub fn decimate_params(&self) -> Option<DecimateParams> {
        match self {
            DisplayMeshModKind::Decimate {
                ratio,
                mode,
                crease_angle_deg,
                crease_strength,
                boundary_strength,
                field_strength,
                field_radius,
                max_error,
                planar_bias,
            } => Some(
                DecimateParams {
                    ratio: *ratio,
                    mode: *mode,
                    crease_angle_deg: *crease_angle_deg,
                    crease_strength: *crease_strength,
                    boundary_strength: *boundary_strength,
                    field_strength: *field_strength,
                    field_radius: *field_radius,
                    max_error: *max_error,
                    planar_bias: *planar_bias,
                    ..DecimateParams::default()
                }
                .clamped(),
            ),
            _ => None,
        }
    }
}

impl DisplayMeshModRecord {
    pub fn new_mirror() -> Self {
        Self {
            id: 0,
            enabled: true,
            expanded: true,
            kind: DisplayMeshModKind::Mirror {
                axis_bits: 1, // X
                center: Vec3::ZERO,
                weld: true,
                weld_threshold: 1e-4,
            },
        }
    }

    pub fn new_smooth_geometry() -> Self {
        Self {
            id: 0,
            enabled: true,
            expanded: true,
            kind: DisplayMeshModKind::SmoothGeometry {
                iterations: 3,
                factor: 0.5,
            },
        }
    }

    pub fn new_smooth_normals() -> Self {
        Self {
            id: 0,
            enabled: true,
            expanded: true,
            kind: DisplayMeshModKind::SmoothNormals,
        }
    }

    pub fn new_decimate() -> Self {
        let p = DecimateParams::default();
        Self {
            id: 0,
            enabled: true,
            expanded: true,
            kind: DisplayMeshModKind::Decimate {
                ratio: p.ratio,
                mode: p.mode,
                crease_angle_deg: p.crease_angle_deg,
                crease_strength: p.crease_strength,
                boundary_strength: p.boundary_strength,
                field_strength: p.field_strength,
                field_radius: p.field_radius,
                max_error: p.max_error,
                planar_bias: p.planar_bias,
            },
        }
    }

    pub fn new_clay_polish() -> Self {
        Self {
            id: 0,
            enabled: true,
            expanded: true,
            kind: DisplayMeshModKind::ClayPolish {
                iterations: 5,
                factor: 0.55,
            },
        }
    }

    pub fn label(&self) -> &'static str {
        match self.kind {
            DisplayMeshModKind::Mirror { .. } => "Mirror",
            DisplayMeshModKind::SmoothGeometry { .. } => "Smooth Geometry",
            DisplayMeshModKind::SmoothNormals => "Smooth Normals",
            DisplayMeshModKind::Decimate { .. } => "Decimate",
            DisplayMeshModKind::ClayPolish { .. } => "Clay Polish",
        }
    }

    /// Expand one authoring card into zero or more runtime ops.
    pub fn to_ops(&self) -> Vec<DisplayMeshOp> {
        if !self.enabled {
            return Vec::new();
        }
        match &self.kind {
            DisplayMeshModKind::Mirror {
                axis_bits,
                center,
                weld,
                weld_threshold,
            } => vec![DisplayMeshOp::Mirror {
                axis_bits: *axis_bits,
                center: *center,
                weld: *weld,
                weld_threshold: *weld_threshold,
            }],
            DisplayMeshModKind::SmoothGeometry {
                iterations,
                factor,
            } => vec![DisplayMeshOp::SmoothGeometry {
                iterations: *iterations,
                factor: *factor,
            }],
            DisplayMeshModKind::SmoothNormals => vec![DisplayMeshOp::SmoothNormals],
            DisplayMeshModKind::Decimate { .. } => {
                let params = self
                    .kind
                    .decimate_params()
                    .unwrap_or_default();
                vec![DisplayMeshOp::Decimate { params }]
            }
            // Preset recipe (P4): smooth geo then recompute normals.
            DisplayMeshModKind::ClayPolish {
                iterations,
                factor,
            } => vec![
                DisplayMeshOp::SmoothGeometry {
                    iterations: (*iterations).max(1),
                    factor: factor.clamp(0.0, 1.0),
                },
                DisplayMeshOp::SmoothNormals,
            ],
        }
    }
}

/// Compile authoring list → runtime ops (enabled only, order preserved; presets expand).
pub fn compile_display_mesh_stack(records: &[DisplayMeshModRecord]) -> Vec<DisplayMeshOp> {
    records.iter().flat_map(|r| r.to_ops()).collect()
}

// ============================================================
// Hash (mesh-stack identity — separate from body field / bake hash)
// ============================================================

pub fn hash_display_mesh_stack(records: &[DisplayMeshModRecord]) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    records.len().hash(&mut h);
    for r in records {
        r.id.hash(&mut h);
        r.enabled.hash(&mut h);
        hash_kind(&r.kind, &mut h);
    }
    h.finish()
}

fn hash_kind(k: &DisplayMeshModKind, h: &mut impl Hasher) {
    match k {
        DisplayMeshModKind::Mirror {
            axis_bits,
            center,
            weld,
            weld_threshold,
        } => {
            0u32.hash(h);
            axis_bits.hash(h);
            hash_vec3(*center, h);
            weld.hash(h);
            hash_f32(*weld_threshold, h);
        }
        DisplayMeshModKind::SmoothGeometry {
            iterations,
            factor,
        } => {
            1u32.hash(h);
            iterations.hash(h);
            hash_f32(*factor, h);
        }
        DisplayMeshModKind::SmoothNormals => {
            2u32.hash(h);
        }
        DisplayMeshModKind::Decimate {
            ratio,
            mode,
            crease_angle_deg,
            crease_strength,
            boundary_strength,
            field_strength,
            field_radius,
            max_error,
            planar_bias,
        } => {
            3u32.hash(h);
            DecimateParams {
                ratio: *ratio,
                mode: *mode,
                crease_angle_deg: *crease_angle_deg,
                crease_strength: *crease_strength,
                boundary_strength: *boundary_strength,
                field_strength: *field_strength,
                field_radius: *field_radius,
                max_error: *max_error,
                planar_bias: *planar_bias,
                ..DecimateParams::default()
            }
            .hash_into(h);
        }
        DisplayMeshModKind::ClayPolish {
            iterations,
            factor,
        } => {
            4u32.hash(h);
            iterations.hash(h);
            hash_f32(*factor, h);
        }
    }
}

fn hash_vec3(v: Vec3, h: &mut impl Hasher) {
    hash_f32(v.x, h);
    hash_f32(v.y, h);
    hash_f32(v.z, h);
}

fn hash_f32(v: f32, h: &mut impl Hasher) {
    v.to_bits().hash(h);
}

#[cfg(test)]
mod tests;
