// core_engine/core_math/src/sdf_shapes.rs
// SDF Shape types — geometric primitives with distance functions
use glam::{Vec2, Vec3};
use serde::{Serialize, Deserialize};
use crate::curves::{CurveProfile, sdf_polygon_2d, bevel_2d_profiled, dist_to_open_polyline_2d, dist_to_polyline_3d};

/// Bevel mode for each individual box edge.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub enum BevelMode { #[default] None, Round, Chamfer }

/// Which SDF field algorithm to use when a box has bevel radii.
///
/// **PerEdge** (default): tri-planar independent edges — best when only *some* edges are
/// beveled (click-drag one edge). Corners use a rho-sphere blend.
/// **Uniform**: exact `sdRoundBox` from the max active edge radius — best when *all* edges
/// share one radius (clean corners, no tri-planar residual).
///
/// Packed into GPU mode-bits bits 26–27 (serde default = PerEdge for old saves).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u8)]
pub enum BoxBevelAlgo {
    #[default]
    PerEdge = 0,
    Uniform = 1,
}

impl BoxBevelAlgo {
    pub fn label(self) -> &'static str {
        match self {
            Self::PerEdge => "Per-Edge",
            Self::Uniform => "Uniform",
        }
    }
    pub fn hint(self) -> &'static str {
        match self {
            Self::PerEdge => "Independent edges — best for selected/single edges",
            Self::Uniform => "Exact round-box — best when all edges share one radius",
        }
    }
    pub fn as_u8(self) -> u8 {
        self as u8
    }
    pub fn from_u8(v: u8) -> Self {
        match v {
            1 => Self::Uniform,
            _ => Self::PerEdge,
        }
    }
}

/// Per-edge bevel for a Box SDF.
/// 12 edges indexed as: 0-3=bottom, 4-7=top, 8-11=vertical (matches box_bevel_edges order).
///   Bot: 0=(z<0), 1=(z>0), 2=(x<0), 3=(x>0)
///   Top: 4=(z<0), 5=(z>0), 6=(x<0), 7=(x>0)
///   Vert: 8=(x<0,z<0), 9=(x>0,z<0), 10=(x<0,z>0), 11=(x>0,z>0)
/// `up_axis`: selects which local axis is "up" for edge classification.
///   0=Y (default), 1=X, 2=Z. Permutes coordinates before SDF evaluation.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BoxBevel {
    pub radii: [f32; 12],
    pub modes: [BevelMode; 12],
    pub up_axis: u8,  // 0=Y (default), 1=X, 2=Z
    /// Field algorithm (PerEdge vs Uniform). Old saves default PerEdge.
    #[serde(default)]
    pub algo: BoxBevelAlgo,
}
impl Default for BoxBevel {
    fn default() -> Self {
        Self {
            radii: [0.0; 12],
            modes: [BevelMode::None; 12],
            up_axis: 0,
            algo: BoxBevelAlgo::PerEdge,
        }
    }
}

impl BoxBevel {
    /// Max radius among edges that have a non-None mode (for Uniform algo).
    pub fn active_max_radius(&self) -> f32 {
        let mut r = 0.0f32;
        for i in 0..12 {
            if self.modes[i] != BevelMode::None {
                r = r.max(self.radii[i]);
            }
        }
        r
    }
}

/// Per-edge bevel for a Cylinder SDF.
/// Two edge rings: top (circular edge at +Y) and bottom (circular edge at -Y).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CylinderBevel {
    pub top: f32,
    pub bot: f32,
    pub top_mode: BevelMode,
    pub bot_mode: BevelMode,
}
impl Default for CylinderBevel {
    fn default() -> Self { Self { top: 0.0, bot: 0.0, top_mode: BevelMode::None, bot_mode: BevelMode::None } }
}
impl CylinderBevel {
    pub fn has_any(&self) -> bool {
        (self.top > 0.001 && self.top_mode != BevelMode::None)
            || (self.bot > 0.001 && self.bot_mode != BevelMode::None)
    }
}

/// Sampled signed-distance grid produced by converting a polygon mesh (LAB feature
/// poly→SDF, docs/planning/lab/poly-mesh-sdf-operand.md). Stored in the SHAPE's local,
/// scale-normalized frame (same space `SdfShape::distance` receives). The grid box is NOT
/// necessarily centered on the shape origin: destructive converts center it
/// (`bounds_min = -half`), while LINKED converts author it in the source poly's local frame
/// (`bounds_min` = the poly-local AABB min) so the prim transform can equal the poly
/// transform exactly. Trilinear-sampled with a clamp+euclidean extension outside the box
/// (continuous upper bound; the mesh surface is strictly inside). `Arc` keeps
/// undo/GPU-serialize clones cheap (grids are MB-scale).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MeshSdfData {
    pub res: u32,               // grid resolution per axis (cubic)
    pub bounds_min: Vec3,       // shape-local min corner of the grid box
    pub cell: f32,              // grid cell spacing in shape-local units
    pub dist: std::sync::Arc<Vec<f32>>, // res^3 signed distances, x + y*res + z*res*res
}

impl MeshSdfData {
    /// Shape-local max corner of the grid box.
    pub fn bounds_max(&self) -> Vec3 {
        self.bounds_min + Vec3::splat(self.cell * (self.res.max(1) - 1) as f32)
    }

    /// Component-wise max |coordinate| over the grid box corners — a conservative symmetric
    /// half-extent around the SHAPE ORIGIN. Correct for both centered (destructive) and
    /// off-center (linked, poly-local) grids; for centered grids it equals the old half_extent.
    pub fn corner_abs_max(&self) -> Vec3 {
        self.bounds_min.abs().max(self.bounds_max().abs())
    }

    /// Trilinear signed-distance sample at a shape-local point. Mirrors the WGSL
    /// `sample_mesh_grid` (volume_bake.wgsl) for CPU/GPU parity: clamp-sample inside the
    /// box + add the euclidean distance to the box for points outside it.
    pub fn sample(&self, p: Vec3) -> f32 {
        let res = self.res;
        if res < 2 || self.cell <= 0.0 || self.dist.len() < (res as usize).pow(3) {
            return 1e6;
        }
        let hi = (res - 1) as f32;
        let g = (p - self.bounds_min) / self.cell;
        let g_cl = g.clamp(Vec3::ZERO, Vec3::splat(hi));
        let d_out = (g - g_cl).length() * self.cell;
        let g0 = g_cl.floor().min(Vec3::splat(hi - 1.0));
        let f = (g_cl - g0).clamp(Vec3::ZERO, Vec3::ONE);
        let (x0, y0, z0) = (g0.x as u32, g0.y as u32, g0.z as u32);
        let at = |x: u32, y: u32, z: u32| -> f32 {
            self.dist[(x + y * res + z * res * res) as usize]
        };
        let c00 = at(x0, y0, z0) * (1.0 - f.x) + at(x0 + 1, y0, z0) * f.x;
        let c10 = at(x0, y0 + 1, z0) * (1.0 - f.x) + at(x0 + 1, y0 + 1, z0) * f.x;
        let c01 = at(x0, y0, z0 + 1) * (1.0 - f.x) + at(x0 + 1, y0, z0 + 1) * f.x;
        let c11 = at(x0, y0 + 1, z0 + 1) * (1.0 - f.x) + at(x0 + 1, y0 + 1, z0 + 1) * f.x;
        let c0 = c00 * (1.0 - f.y) + c10 * f.y;
        let c1 = c01 * (1.0 - f.y) + c11 * f.y;
        c0 * (1.0 - f.z) + c1 * f.z + d_out
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum SdfShape {
    Sphere { radius: f32 },
    Box { half_extents: Vec3, bevel: BoxBevel },
    Cylinder { radius: f32, half_height: f32 },
    Torus { major_radius: f32, minor_radius: f32 },
    Cone { radius: f32, half_height: f32 },
    Capsule { radius: f32, half_height: f32 },
    Stair { half_extents: Vec3, num_steps: u32 },
    SpiralStair { radius: f32, half_height: f32, num_steps: u32, step_width: f32 },
    Billboard { width: f32, height: f32, face_camera: bool },
    /// Extruded 2D curve profile. The profile polygon is evaluated in the prim's local XZ plane
    /// (Y = extrusion axis). The CurveProfile stores the 2D vertices; the 3D plane frame is
    /// used by the clip curve system but for the shape variant, extrusion is always along local Y.
    /// `wall_t` (shell/hollow wall thickness, 0 = solid) and `rim` (extrude-cap silhouette bevel:
    /// >0 round, <0 chamfer, 0 = hard cap; magnitude = radius) are P1 additions. `open`/`thickness`/
    /// `tube` are P2: when `open`, the profile is treated as an OPEN polyline (no winding fill) offset
    /// by `thickness` in-plane — a flat ribbon/wall (rectangular cross-section, extruded ±half_depth)
    /// or, when `tube`, a round cross-section of radius `thickness/2` swept along the path (half_depth
    /// ignored). Closed mode (`open=false`) is the P1 filled-polygon behavior, unchanged.
    /// ALL of `wall_t`/`rim`/`open`/`thickness`/`tube` are `#[serde(skip)]` — the bincode blob stays
    /// BYTE-IDENTICAL to every pre-P1 `.fl4` (no field-trap), and the authored values persist OUT OF
    /// BAND via the `curve_params` side-channel on `ProjectFile` (keyed by `prim_uid`), EXACTLY
    /// mirroring `VoxelBody.grid`/`voxel_grids`. In-memory `Clone` copies them, so undo/redo and
    /// duplicate keep them.
    CurveExtrude {
        profile: CurveProfile,
        half_depth: f32,
        #[serde(skip)] wall_t: f32,
        #[serde(skip)] rim: f32,
        #[serde(skip)] open: bool,
        #[serde(skip)] thickness: f32,
        #[serde(skip)] tube: bool,
    },
    /// LAB poly→SDF: a polygon mesh converted to a sampled distance grid, participating in the
    /// body's boolean chain like any primitive. `source_poly` = LIVE-MODIFIER link: when Some,
    /// the grid follows that PolygonalBody node (transform every frame, geometry re-convert
    /// debounced on edit; see lab_mesh_sdf.rs). None = static (destructive convert / unlinked).
    /// NOTE: adding this field changed the variant's bincode encoding — .fl4 files saved with
    /// MeshVolume prims BEFORE 2026-07-14 won't load (lab-only feature; re-convert instead).
    MeshVolume { grid: std::sync::Arc<MeshSdfData>, source_poly: Option<crate::scene_graph::NodeId> },
    /// Slice / half-space cutter: an INFINITE plane through the prim origin with normal = local +Z.
    /// The SDF in prim-local space is simply `p.z` (signed distance to the plane). Oriented by the
    /// prim's rotation gizmo and positioned by its translation; there are no shape params (the
    /// transform is the plane). Used as a boolean cutter (Subtract/Intersect) to slice a body along
    /// a plane. Appended after MeshVolume; never insert before it.
    HalfSpace,
    /// 3D spine / pipe (Phase A): free polyline in prim-local space with constant round-tube radius.
    /// `points` are prim-local; `closed` wraps last→first when true and len≥3. Shape type id = 12.
    /// APPENDED LAST — bincode tags variants by index (see MeshVolume note). Later phases add
    /// per-point radius, Bezier joins, custom profile, shell, path-array caps.
    SpinePipe {
        points: Vec<Vec3>,
        /// Global base tube radius. Effective radius at control point `i` =
        /// `radius * point_scale[i]` (missing scale → 1.0).
        radius: f32,
        closed: bool,
        /// Per-point corner type, parallel to `points` (missing → Sharp). Same packing as
        /// `CurveVertex::bevel_radius`: 0 = Sharp, >0 = **Bezier** (smooth handles; mag is a
        /// soft auto-length hint), <0 = Chamfer (mag=setback).
        #[serde(default)]
        joins: Vec<f32>,
        /// Per-point **radius scale** (point parameter). S in the spine tool edits this for the
        /// selection — not spatial scale of the whole curve. Default / missing = 1.0.
        #[serde(default)]
        point_scale: Vec<f32>,
        /// Incoming Bezier handle offset from the control point (prim-local).
        /// `ZERO` / missing → auto when join is Bezier, else linear (Sharp).
        #[serde(default)]
        handles_in: Vec<Vec3>,
        /// Outgoing Bezier handle offset from the control point (prim-local).
        /// `ZERO` / missing → auto when join is Bezier, else linear (Sharp).
        #[serde(default)]
        handles_out: Vec<Vec3>,
    },
}

impl SdfShape {
    pub fn distance(&self, p: Vec3) -> f32 {
        match self {
            SdfShape::Sphere { radius } => p.length() - radius,
            SdfShape::Box { half_extents, bevel } => {
                // Exact Euclidean fast path — mirrors the GPU dispatch (volume_bake.wgsl:614-616,
                // sdf_prim_eval.wgsl, GLSL): when the packed mode bits are all zero (every edge
                // mode None AND up_axis == 0), the GPU evaluates `sdf_box` =
                // length(max(q,0)) + min(max-component, 0). The old CPU tri-planar form
                // (max of three 2D plane distances) UNDERESTIMATES the true distance in
                // multi-positive corner regions by up to ~22% — the tracked CPU/GPU box eval
                // divergence. Same op order as the WGSL for bit-level parity.
                if bevel.up_axis == 0 && bevel.modes.iter().all(|m| *m == BevelMode::None) {
                    let q = p.abs() - *half_extents;
                    return q.max(Vec3::ZERO).length() + q.x.max(q.y.max(q.z)).min(0.0);
                }
                let max_r = half_extents.x.min(half_extents.y).min(half_extents.z);
                // Permute coordinates based on bevel up_axis (0=Y, 1=X, 2=Z)
                let (bp, bh) = match bevel.up_axis {
                    1 => (Vec3::new(p.y, p.x, p.z), Vec3::new(half_extents.y, half_extents.x, half_extents.z)),
                    2 => (Vec3::new(p.x, p.z, p.y), Vec3::new(half_extents.x, half_extents.z, half_extents.y)),
                    _ => (p, *half_extents),
                };
                // Uniform algo: exact sdRoundBox from max active radius (best when all edges equal).
                // KEEP LINE-PARALLEL with GPU sdf_box_per_edge algo==1 branch.
                if bevel.algo == BoxBevelAlgo::Uniform {
                    let r = bevel.active_max_radius().min(max_r).max(0.0);
                    if r < 1e-6 {
                        let q = bp.abs() - bh;
                        return q.max(Vec3::ZERO).length() + q.x.max(q.y.max(q.z)).min(0.0);
                    }
                    let q = bp.abs() - bh + Vec3::splat(r);
                    return q.max(Vec3::ZERO).length() + q.x.max(q.y.max(q.z)).min(0.0) - r;
                }
                // Per-edge path: tri-planar + rho corner term (best for independent edges).
                let qx = bp.x.abs() - bh.x;
                let qy = bp.y.abs() - bh.y;
                let qz = bp.z.abs() - bh.z;
                let xi: usize = if bp.x > 0.0 { 1 } else { 0 };
                let zi: usize = if bp.z > 0.0 { 1 } else { 0 };
                let base_y: usize = if bp.y > 0.0 { 4 } else { 0 };
                let e_xz = 8 + xi + zi * 2;
                let e_xy = base_y + 2 + xi;
                let e_yz = base_y + zi;
                let bevel_2d = |a: f32, b: f32, r: f32, m: BevelMode| -> f32 {
                    let r = r.min(max_r);
                    match m {
                        BevelMode::None    => { let q=Vec2::new(a,b); q.max(Vec2::ZERO).length()+q.x.max(q.y).min(0.0) }
                        BevelMode::Round   => { let q=Vec2::new(a+r,b+r); q.max(Vec2::ZERO).length()+q.x.max(q.y).min(0.0)-r }
                        BevelMode::Chamfer => {
                            if a >= b + r && b + r >= 0.0 { Vec2::new(a, b + r).length() }
                            else if b >= a + r && a + r >= 0.0 { Vec2::new(a + r, b).length() }
                            else { a.max(b).max((a+b+r)*std::f32::consts::FRAC_1_SQRT_2) }
                        }
                    }
                };
                let d_xz = bevel_2d(qx, qz, bevel.radii[e_xz], bevel.modes[e_xz]);
                let d_xy = bevel_2d(qx, qy, bevel.radii[e_xy], bevel.modes[e_xy]);
                let d_yz = bevel_2d(qy, qz, bevel.radii[e_yz], bevel.modes[e_yz]);
                let eff = |e: usize| -> f32 {
                    if bevel.modes[e] == BevelMode::None { 0.0 } else { bevel.radii[e].min(max_r).max(0.0) }
                };
                let rho = eff(e_xz).min(eff(e_xy)).min(eff(e_yz));
                let qc = Vec3::new(qx + rho, qy + rho, qz + rho);
                let d_corner = qc.max(Vec3::ZERO).length() + qc.x.max(qc.y.max(qc.z)).min(0.0) - rho;
                d_xz.max(d_xy).max(d_yz).max(d_corner)
            }
            SdfShape::Cylinder { radius, half_height } => {
                let d_r = (p.x * p.x + p.z * p.z).sqrt() - radius;
                let d_h = p.y.abs() - half_height;
                Vec2::new(d_r.max(0.0), d_h.max(0.0)).length() + d_r.max(d_h).min(0.0)
            }
            SdfShape::Torus { major_radius, minor_radius } => {
                let q = Vec2::new((p.x * p.x + p.z * p.z).sqrt() - major_radius, p.y);
                q.length() - minor_radius
            }
            SdfShape::Cone { radius, half_height } => {
                let q_len = (p.x * p.x + p.z * p.z).sqrt();
                let h = *half_height;
                let r = *radius;
                let norm_f = (4.0 * h * h + r * r).sqrt();
                let d_side = (2.0 * h * q_len + r * (p.y - h)) / norm_f;
                d_side.max(p.y.abs() - h)
            }
            SdfShape::Capsule { radius, half_height } => {
                let closest = Vec3::new(0.0, p.y.clamp(-*half_height, *half_height), 0.0);
                (p - closest).length() - radius
            }
            SdfShape::Stair { half_extents: he, num_steps } => {
                // Box with stepped cutouts: steps go from -Z to +Z, rising from -Y to +Y
                let qx = p.x.abs() - he.x;
                let qy = p.y.abs() - he.y;
                let qz = p.z.abs() - he.z;
                let d_box = Vec3::new(qx.max(0.0), qy.max(0.0), qz.max(0.0)).length()
                    + qx.max(qy).max(qz).min(0.0);
                let _n = (*num_steps).max(1) as f32;
                let step_h = he.y * 2.0 / _n;
                let step_d = he.z * 2.0 / _n;
                // Find which step region we're in
                let mut cut = f32::MAX;
                for i in 0..(*num_steps).max(1) {
                    let y_top = -he.y + (i as f32 + 1.0) * step_h;
                    let z_front = -he.z + (i as f32 + 1.0) * step_d;
                    // Region above step AND in front → subtract
                    let c = (p.y - y_top).max(z_front - p.z);
                    cut = cut.min(c);
                }
                d_box.max(cut)
            }
            SdfShape::Billboard { width, height, .. } => {
                // Thin slab: box with near-zero depth
                let he = Vec3::new(*width * 0.5, *height * 0.5, 0.005);
                let q = Vec3::new(p.x.abs() - he.x, p.y.abs() - he.y, p.z.abs() - he.z);
                Vec3::new(q.x.max(0.0), q.y.max(0.0), q.z.max(0.0)).length()
                    + q.x.max(q.y).max(q.z).min(0.0)
            }
            SdfShape::SpiralStair { radius, half_height, num_steps, step_width } => {
                let n = (*num_steps).max(1) as f32;
                let step_angle = std::f32::consts::TAU / n;
                let step_h = *half_height * 2.0 / n;
                let r = (p.x * p.x + p.z * p.z).sqrt();
                let angle = p.z.atan2(p.x); // -PI..PI
                // Map angle to step index
                let norm_angle = (angle + std::f32::consts::PI) / step_angle;
                let base_idx = norm_angle.floor() as i32;
                let mut d = f32::MAX;
                // Check 3 neighboring steps
                for offset in -1..=1 {
                    let si = base_idx + offset;
                    let a = si as f32 * step_angle - std::f32::consts::PI;
                    let y_base = -*half_height + si as f32 * step_h;
                    // Rotate p into step-local frame
                    let ca = a.cos(); let sa = a.sin();
                    let local_x = ca * p.x + sa * p.z - *radius * 0.5;
                    let local_z = -sa * p.x + ca * p.z;
                    let local_y = p.y - y_base - step_h * 0.5;
                    // Step slab as a box
                    let he_step = Vec3::new(*radius * 0.5, step_h * 0.5, *step_width * 0.5);
                    let q = Vec3::new(local_x.abs() - he_step.x, local_y.abs() - he_step.y, local_z.abs() - he_step.z);
                    let step_d = Vec3::new(q.x.max(0.0), q.y.max(0.0), q.z.max(0.0)).length()
                        + q.x.max(q.y).max(q.z).min(0.0);
                    d = d.min(step_d);
                }
                // Central column
                let col_r = r - 0.08;
                let col_h = p.y.abs() - *half_height;
                let column = Vec2::new(col_r.max(0.0), col_h.max(0.0)).length()
                    + col_r.max(col_h).min(0.0);
                d.min(column)
            }
            SdfShape::CurveExtrude { profile, half_depth, wall_t, rim, open, thickness, tube } => {
                // Evaluate in local XZ plane (Y = extrusion axis), matching standard prim convention.
                let uv = Vec2::new(p.x, p.z);
                if *open && *thickness > 1e-5 {
                    // OPEN polyline (P2): unsigned distance to the open path (no winding fill).
                    let d_path = dist_to_open_polyline_2d(uv, &profile.verts);
                    if *tube {
                        // Round cross-section swept along the planar path: the 3D distance to the
                        // 1D path curve is sqrt(inplane² + axis²); subtract the tube radius. Round
                        // end-caps come for free (path endpoints are the nearest feature there).
                        Vec2::new(d_path, p.y).length() - *thickness * 0.5
                    } else {
                        // Flat ribbon / wall: in-plane band of width `thickness`, extruded ±half_depth.
                        let d_band = d_path - *thickness * 0.5;
                        if *half_depth > 0.001 {
                            let cap = p.y.abs() - *half_depth;
                            let rim_prof = if *rim > 0.0 { 1.0 } else { 0.0 };
                            bevel_2d_profiled(d_band, cap, rim.abs(), rim_prof, 45.0)
                        } else {
                            d_band
                        }
                    }
                } else {
                    // CLOSED (P1): filled polygon. Corner round/chamfer is baked into `sdf_polygon_2d`
                    // (boundary expansion).
                    let mut d = sdf_polygon_2d(uv, &profile.verts);
                    if *half_depth > 0.001 {
                        // RIM: round/chamfer the extrude-cap silhouette edge instead of a hard max.
                        // rim sign = mode (+round / −chamfer), magnitude = radius; 0 ⇒ hard cap.
                        let cap = p.y.abs() - *half_depth;
                        let rim_prof = if *rim > 0.0 { 1.0 } else { 0.0 };
                        d = bevel_2d_profiled(d, cap, rim.abs(), rim_prof, 45.0);
                    }
                    // SHELL: hollow INWARD only (outer surface at d=0 unchanged → bounding contract
                    // preserved). d_wall = max(d, -d - wall_t) = solid ∩ ¬(inner core offset by wall_t).
                    if *wall_t > 1e-4 { d = d.max(-d - *wall_t); }
                    d
                }
            }
            SdfShape::MeshVolume { grid, .. } => grid.sample(p),
            // Infinite plane through the origin, normal = local +Z. The caller passes the
            // prim-local sample point, so the signed distance to the plane is just p.z.
            SdfShape::HalfSpace => p.z,
            // Round tube along a 3D polyline with **per-point radius** (lerped along segments).
            // Phase B/E: expand Sharp/Bezier/Chamfer (+ handles) into the centerline first.
            SdfShape::SpinePipe {
                points, radius, closed, joins, point_scale, handles_in, handles_out,
            } => {
                crate::curves::dist_to_spine_pipe_3d_joined(
                    p, points, *radius, point_scale, *closed, joins, handles_in, handles_out,
                )
            }
        }
    }
    /// SDF distance using per-edge-group continuous bevel profiles and chamfer angles.
    /// Each profile: -1.0 = concave scoop, 0.0 = chamfer, +1.0 = round fillet.
    /// Each chamfer_angle: degrees 0-90 (45 = standard symmetric chamfer).
    /// Only Box and Cylinder are affected; all other shapes delegate to `distance()`.
    pub fn distance_profiled(&self, p: Vec3,
        prof_top: f32, prof_bot: f32, prof_vert: f32,
        angle_top: f32, angle_bot: f32, angle_vert: f32,
    ) -> f32 {
        match self {
            SdfShape::Box { half_extents, bevel } => {
                let max_r = half_extents.x.min(half_extents.y).min(half_extents.z);
                let (bp, bh) = match bevel.up_axis {
                    1 => (Vec3::new(p.y, p.x, p.z), Vec3::new(half_extents.y, half_extents.x, half_extents.z)),
                    2 => (Vec3::new(p.x, p.z, p.y), Vec3::new(half_extents.x, half_extents.z, half_extents.y)),
                    _ => (p, *half_extents),
                };
                let qx = bp.x.abs() - bh.x;
                let qy = bp.y.abs() - bh.y;
                let qz = bp.z.abs() - bh.z;
                let xi: usize = if bp.x > 0.0 { 1 } else { 0 };
                let zi: usize = if bp.z > 0.0 { 1 } else { 0 };
                let base_y: usize = if bp.y > 0.0 { 4 } else { 0 };
                let e_xz = 8 + xi + zi * 2;
                let e_xy = base_y + 2 + xi;
                let e_yz = base_y + zi;
                let bevel_2d_prof = |a: f32, b: f32, r: f32, prof: f32, angle_deg: f32| -> f32 {
                    let r = r.min(max_r);
                    if r <= 0.0 { return a.max(b); }
                    let theta = angle_deg.to_radians();
                    let (st, ct) = theta.sin_cos();
                    let d_chamfer = a.max(b).max(st * (a + r) + ct * b);
                    if prof >= 0.0 {
                        let q = Vec2::new((a + r).max(0.0), (b + r).max(0.0));
                        let d_round = q.length() + (a + r).max(b + r).min(0.0) - r;
                        d_chamfer * (1.0 - prof) + d_round * prof
                    } else {
                        let u = Vec2::new((r + a).max(0.0), (r + b).max(0.0));
                        let d_concave = a.max(b) - (r - u.length());
                        d_chamfer * (1.0 + prof) + d_concave * (-prof)
                    }
                };
                let edge_prof = |e: usize| -> f32 {
                    if e >= 8 { prof_vert } else if e >= 4 { prof_top } else { prof_bot }
                };
                let edge_angle = |e: usize| -> f32 {
                    if e >= 8 { angle_vert } else if e >= 4 { angle_top } else { angle_bot }
                };
                let d_xz = bevel_2d_prof(qx, qz, bevel.radii[e_xz], edge_prof(e_xz), edge_angle(e_xz));
                let d_xy = bevel_2d_prof(qx, qy, bevel.radii[e_xy], edge_prof(e_xy), edge_angle(e_xy));
                let d_yz = bevel_2d_prof(qy, qz, bevel.radii[e_yz], edge_prof(e_yz), edge_angle(e_yz));
                d_xz.max(d_xy).max(d_yz)
            }
            // All other shapes: no profiled variant, delegate
            _ => self.distance(p),
        }
    }

    pub fn label(&self) -> &str {
        match self { SdfShape::Sphere{..}=>"Sphere", SdfShape::Box{..}=>"Box", SdfShape::Cylinder{..}=>"Cylinder",
                     SdfShape::Torus{..}=>"Torus", SdfShape::Cone{..}=>"Cone", SdfShape::Capsule{..}=>"Capsule",
                     SdfShape::Stair{..}=>"Stair", SdfShape::SpiralStair{..}=>"Spiral", SdfShape::Billboard{..}=>"Billboard",
                     SdfShape::CurveExtrude{..}=>"Curve", SdfShape::MeshVolume{..}=>"Mesh",
                     SdfShape::HalfSpace=>"Slice", SdfShape::SpinePipe{..}=>"Spine" }
    }
    pub fn icon(&self) -> &str {
        match self { SdfShape::Sphere{..}=>"○", SdfShape::Box{..}=>"□", SdfShape::Cylinder{..}=>"⊓",
                     SdfShape::Torus{..}=>"◎", SdfShape::Cone{..}=>"△", SdfShape::Capsule{..}=>"⊂",
                     SdfShape::Stair{..}=>"≡", SdfShape::SpiralStair{..}=>"⊘", SdfShape::Billboard{..}=>"▭",
                     SdfShape::CurveExtrude{..}=>"⌒", SdfShape::MeshVolume{..}=>"◈",
                     SdfShape::HalfSpace=>"▱", SdfShape::SpinePipe{..}=>"∿" }
    }
}

/// Default spine: one seed point → solid sphere of `radius` until the user Shift+clicks
/// more points into a tube. No pre-shaped S; authoring is point-edit first, then Space/Enter
/// to object mode for G/R/S on the whole prim.
pub fn spine_pipe_preset() -> SdfShape {
    SdfShape::SpinePipe {
        points: vec![Vec3::ZERO],
        radius: 0.12,
        closed: false,
        joins: vec![0.0],
        point_scale: vec![1.0],
        handles_in: vec![Vec3::ZERO],
        handles_out: vec![Vec3::ZERO],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shape_distance_sphere() {
        let shape = SdfShape::Sphere { radius: 0.8 };
        let d = shape.distance(Vec3::ZERO);
        assert!((d - (-0.8)).abs() < 0.001);
        let d2 = shape.distance(Vec3::new(0.8, 0.0, 0.0));
        assert!(d2.abs() < 0.001);
    }

    #[test]
    fn shape_distance_box() {
        let shape = SdfShape::Box {
            half_extents: Vec3::new(0.5, 0.3, 0.7),
            bevel: BoxBevel::default(),
        };
        let d = shape.distance(Vec3::ZERO);
        assert!(d < 0.0, "origin should be inside box, got d={}", d);
        assert!((d - (-0.3)).abs() < 0.01, "should be ~-0.3 from Y face, got d={}", d);
    }

    // ---------- BEVEL-NOTCH property gates (2026-07-17) ----------
    // The corner-term fix in the beveled Box path. These lock: (a) all-Round equal
    // radii == exact sdRoundBox; (b) |grad| <= 1+eps over randomized mixed configs
    // (march-safe Lipschitz bound); (c) sign parity with the OLD tri-planar-max field
    // everywhere off the corner blend neighborhoods + bounded deviation inside them;
    // (d) shadow-march probe: the new field is >= the old field everywhere and
    // strictly larger on the old C0 corner ridges (quantifies the stripe fix).

    fn bevel_box(he: Vec3, radii: [f32; 12], modes: [BevelMode; 12], up: u8) -> SdfShape {
        SdfShape::Box { half_extents: he, bevel: BoxBevel { radii, modes, up_axis: up, algo: BoxBevelAlgo::PerEdge } }
    }

    /// The PRE-FIX field (tri-planar hard max, no corner term) — reference for (c)/(d).
    fn old_tri_planar(he: Vec3, radii: &[f32; 12], modes: &[BevelMode; 12], p: Vec3) -> f32 {
        let max_r = he.x.min(he.y).min(he.z);
        let (qx, qy, qz) = (p.x.abs() - he.x, p.y.abs() - he.y, p.z.abs() - he.z);
        let xi: usize = if p.x > 0.0 { 1 } else { 0 };
        let zi: usize = if p.z > 0.0 { 1 } else { 0 };
        let base_y: usize = if p.y > 0.0 { 4 } else { 0 };
        let (e_xz, e_xy, e_yz) = (8 + xi + zi * 2, base_y + 2 + xi, base_y + zi);
        let b2 = |a: f32, b: f32, r: f32, m: BevelMode| -> f32 {
            let r = r.min(max_r);
            match m {
                BevelMode::None => { let q = Vec2::new(a, b); q.max(Vec2::ZERO).length() + q.x.max(q.y).min(0.0) }
                BevelMode::Round => { let q = Vec2::new(a + r, b + r); q.max(Vec2::ZERO).length() + q.x.max(q.y).min(0.0) - r }
                BevelMode::Chamfer => a.max(b).max((a + b + r) * std::f32::consts::FRAC_1_SQRT_2),
            }
        };
        b2(qx, qz, radii[e_xz], modes[e_xz])
            .max(b2(qx, qy, radii[e_xy], modes[e_xy]))
            .max(b2(qy, qz, radii[e_yz], modes[e_yz]))
    }

    fn sd_round_box(p: Vec3, he: Vec3, r: f32) -> f32 {
        let q = p.abs() - he + Vec3::splat(r);
        q.max(Vec3::ZERO).length() + q.x.max(q.y.max(q.z)).min(0.0) - r
    }

    /// Deterministic LCG so the "randomized" configs are reproducible.
    fn lcg(state: &mut u64) -> f32 {
        *state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        ((*state >> 33) as f32) / (u32::MAX >> 1) as f32
    }

    #[test]
    fn bevel_notch_a_uniform_round_reduces_to_sdroundbox() {
        let he = Vec3::new(0.9, 0.6, 0.7);
        let r = 0.2f32;
        let shape = bevel_box(he, [r; 12], [BevelMode::Round; 12], 0);
        let mut s = 0x5DEECE66Du64;
        for _ in 0..20_000 {
            let p = Vec3::new(
                (lcg(&mut s) * 2.0 - 1.0) * 2.0,
                (lcg(&mut s) * 2.0 - 1.0) * 2.0,
                (lcg(&mut s) * 2.0 - 1.0) * 2.0,
            );
            let d = shape.distance(p);
            let rb = sd_round_box(p, he, r);
            assert!((d - rb).abs() < 1e-6, "p={p:?} d={d} sdRoundBox={rb}");
        }
    }

    #[test]
    fn bevel_notch_b_gradient_bounded_over_random_mixed_configs() {
        let he = Vec3::new(0.8, 0.55, 0.65);
        let mut s = 0xB01D_FACEu64;
        let modes_pool = [BevelMode::None, BevelMode::Round, BevelMode::Chamfer];
        let mut worst = 0.0f32;
        for _cfg in 0..12 {
            let mut radii = [0.0f32; 12];
            let mut modes = [BevelMode::None; 12];
            for e in 0..12 {
                radii[e] = lcg(&mut s) * 0.4;
                modes[e] = modes_pool[(lcg(&mut s) * 3.0) as usize % 3];
            }
            let up = (lcg(&mut s) * 3.0) as u8 % 3;
            let shape = bevel_box(he, radii, modes, up);
            let h = 1e-3f32;
            for _ in 0..4000 {
                let p = Vec3::new(
                    (lcg(&mut s) * 2.0 - 1.0) * 1.8,
                    (lcg(&mut s) * 2.0 - 1.0) * 1.8,
                    (lcg(&mut s) * 2.0 - 1.0) * 1.8,
                );
                let gx = shape.distance(p + Vec3::X * h) - shape.distance(p - Vec3::X * h);
                let gy = shape.distance(p + Vec3::Y * h) - shape.distance(p - Vec3::Y * h);
                let gz = shape.distance(p + Vec3::Z * h) - shape.distance(p - Vec3::Z * h);
                let g = Vec3::new(gx, gy, gz).length() / (2.0 * h);
                worst = worst.max(g);
                // Central differences straddling a C0 crease report up to ~1.05 even for an
                // exactly-1-Lipschitz field (the two half-samples land in different linear
                // pieces). The EXACT march-safety criterion is the pairwise check below.
                assert!(g <= 1.05, "grad {g} > 1+slop at p={p:?} radii={radii:?} modes={modes:?}");
                // Exact 1-Lipschitz property (no finite-difference ambiguity): random step.
                let step = Vec3::new(lcg(&mut s) - 0.5, lcg(&mut s) - 0.5, lcg(&mut s) - 0.5) * 0.3;
                if step.length() > 1e-4 {
                    let df = (shape.distance(p + step) - shape.distance(p)).abs();
                    assert!(
                        df <= step.length() * (1.0 + 1e-4) + 1e-6,
                        "Lipschitz violation: |df|={df} > |step|={} at p={p:?} radii={radii:?} modes={modes:?}",
                        step.length()
                    );
                }
            }
        }
        eprintln!("[bevel-notch b] worst sampled |grad| = {worst}");
    }

    #[test]
    fn bevel_notch_c_sign_parity_off_corner_neighborhoods() {
        let he = Vec3::new(0.9, 0.6, 0.7);
        // The stripe-repro config: mixed radii, mixed modes.
        let mut radii = [0.0f32; 12];
        let mut modes = [BevelMode::None; 12];
        for e in 0..12 { radii[e] = 0.05 + 0.02 * e as f32; modes[e] = if e % 2 == 0 { BevelMode::Round } else { BevelMode::Chamfer }; }
        let shape = bevel_box(he, radii, modes, 0);
        let max_rad = 0.27f32; // largest radius in the config
        let mut s = 0xC0FFEEu64;
        let mut worst_shift = 0.0f32;
        for _ in 0..40_000 {
            let p = Vec3::new(
                (lcg(&mut s) * 2.0 - 1.0) * 1.6,
                (lcg(&mut s) * 2.0 - 1.0) * 1.6,
                (lcg(&mut s) * 2.0 - 1.0) * 1.6,
            );
            let new_d = shape.distance(p);
            let old_d = old_tri_planar(he, &radii, &modes, p);
            assert!(new_d >= old_d - 1e-6, "new field must dominate old (lower-bound chain) p={p:?}");
            // Corner blend neighborhood = ball around each of the 8 corners.
            let corner = Vec3::new(he.x * p.x.signum(), he.y * p.y.signum(), he.z * p.z.signum());
            let in_blend = (p - corner).length() < 2.5 * max_rad;
            if !in_blend {
                assert!(
                    new_d.signum() == old_d.signum() || old_d.abs() < 1e-5,
                    "sign flip OFF the corner blend region p={p:?} old={old_d} new={new_d}"
                );
            } else if new_d.signum() != old_d.signum() {
                // Quantify surface shift inside the blend region: |old_d| bounds the distance
                // from the flipped point to the OLD surface.
                worst_shift = worst_shift.max(old_d.abs());
            }
        }
        // Theory: bulge shave <= (sqrt(3/2)-1)*rho, rho = min octant radius <= 0.27.
        assert!(worst_shift <= 0.2247 * max_rad + 1e-3, "corner surface shift {worst_shift} exceeds bound");
        eprintln!("[bevel-notch c] max corner surface shift = {worst_shift} (bound {})", 0.2247 * max_rad);
    }

    #[test]
    fn bevel_notch_d_shadow_march_probe_ridge_steps_improve() {
        // A ray grazing past the corner along the old field's plane-tie ridge — the
        // configuration the deferred soft_shadow march hits on carved surfaces. The
        // old field's min step collapses toward the sqrt(2/3) underestimate; the new
        // field must never step smaller than the old and must beat it on the ridge.
        let he = Vec3::splat(0.5);
        let r = 0.15f32;
        let shape = bevel_box(he, [r; 12], [BevelMode::Round; 12], 0);
        let corner = Vec3::splat(0.5);
        let dir = Vec3::new(1.0, 1.0, 1.0).normalize();
        let mut min_ratio = f32::INFINITY;
        let mut radii = [r; 12];
        // also probe a mixed-radii ridge
        radii[8] = 0.05;
        let mixed = bevel_box(he, radii, [BevelMode::Round; 12], 0);
        for i in 0..200 {
            let t = 0.02 + i as f32 * 0.01;
            let p = corner + dir * t; // marching out the corner diagonal (the ridge)
            for sh in [&shape, &mixed] {
                let new_d = sh.distance(p);
                let (radii_ref, modes_ref) = match sh {
                    SdfShape::Box { bevel, .. } => (&bevel.radii, &bevel.modes),
                    _ => unreachable!(),
                };
                let old_d = old_tri_planar(he, radii_ref, modes_ref, p);
                assert!(new_d >= old_d - 1e-6, "march step regressed at t={t}");
                if old_d > 1e-4 { min_ratio = min_ratio.min(new_d / old_d); }
            }
        }
        // Far-field on the diagonal the improvement tends to sqrt(3)/sqrt(2) ~ 1.2247;
        // assert we actually gained on the ridge (the stripe mechanism).
        let far = corner + dir * 2.0;
        let gain = shape.distance(far) / old_tri_planar(he, &[r; 12], &[BevelMode::Round; 12], far);
        assert!(gain > 1.15, "expected >=15% larger march steps on the corner ridge, got {gain}");
        assert!(min_ratio >= 1.0 - 1e-6);
        eprintln!("[bevel-notch d] ridge far-field step gain = {gain}, min new/old ratio = {min_ratio}");
    }

    /// Bake an analytic sphere SDF into a MeshSdfData grid, then verify the shape's
    /// trilinear sampler reproduces it: centre inside (<0), surface ≈0, exterior >0
    /// and the clamp+euclidean extension keeps growing past the grid box.
    #[test]
    fn mesh_volume_samples_baked_sphere() {
        let res = 32u32;
        let half = 1.5f32;               // grid half-extent (centered)
        let cell = 2.0 * half / (res as f32 - 1.0);
        let bounds_min = Vec3::splat(-half);
        let sphere_r = 0.8f32;
        let mut dist = vec![0.0f32; (res as usize).pow(3)];
        for z in 0..res { for y in 0..res { for x in 0..res {
            let p = bounds_min + Vec3::new(x as f32, y as f32, z as f32) * cell;
            dist[(x + y * res + z * res * res) as usize] = p.length() - sphere_r;
        }}}
        let grid = MeshSdfData { res, bounds_min, cell, dist: std::sync::Arc::new(dist) };
        let shape = SdfShape::MeshVolume { grid: std::sync::Arc::new(grid), source_poly: None };

        // Away from the r=0 cusp, trilinear sampling tracks the analytic SDF within ~1 cell.
        // (At the exact centre the cusp makes interpolation read ~-r + √3·cell/2 — expected,
        // so we probe at 0.3 along X instead: true dist = 0.3 - 0.8 = -0.5.)
        assert!((shape.distance(Vec3::new(0.3, 0.0, 0.0)) - (-0.5)).abs() < cell,
            "mid-interior should track analytic SDF, got {}", shape.distance(Vec3::new(0.3, 0.0, 0.0)));
        assert!(shape.distance(Vec3::ZERO) < -0.6, "centre should still read well inside");
        assert!(shape.distance(Vec3::new(sphere_r, 0.0, 0.0)).abs() < cell,
            "surface should read ~0, got {}", shape.distance(Vec3::new(sphere_r, 0.0, 0.0)));
        assert!(shape.distance(Vec3::new(1.2, 0.0, 0.0)) > 0.0, "exterior should be positive");
        // Far outside the grid box: clamp+extension keeps distance growing monotonically.
        let d_edge = shape.distance(Vec3::new(half, 0.0, 0.0));
        let d_far  = shape.distance(Vec3::new(half + 1.0, 0.0, 0.0));
        assert!(d_far > d_edge, "extension past the box must keep growing ({d_far} !> {d_edge})");
    }

    #[test]
    fn mesh_volume_label_and_bounds() {
        let grid = MeshSdfData {
            res: 4, bounds_min: Vec3::splat(-0.5), cell: 1.0 / 3.0,
            dist: std::sync::Arc::new(vec![1.0; 64]),
        };
        assert_eq!(grid.corner_abs_max(), Vec3::splat(0.5));
        let shape = SdfShape::MeshVolume { grid: std::sync::Arc::new(grid), source_poly: None };
        assert_eq!(shape.label(), "Mesh");
    }
}
