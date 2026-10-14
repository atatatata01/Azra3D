// ============================================================
// NURBS Curves and Surfaces
// ============================================================
//
// Non-Uniform Rational B-Spline evaluation with adaptive
// tessellation for GPU rendering.

use glam::{Vec2, Vec3};
use serde::{Serialize, Deserialize};

// ============================================================
// Data types
// ============================================================

/// Stable face id inside a `NurbsSolid` (never store Vec indices in selection).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize, Default)]
pub struct NurbsFaceId(pub u64);

/// Stable edge id inside a `NurbsSolid` (adjacency / selection).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize, Default)]
pub struct NurbsEdgeId(pub u64);

/// Stable vertex / CV id inside a `NurbsSolid`.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize, Default)]
pub struct NurbsVertexId(pub u64);

/// Parameter-space boundary of a face. Outer loop CCW, holes CW (PROMPT_TRIMMED_SURFACES).
/// Empty `trims` on the face = untrimmed full patch.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TrimLoop {
    pub curves: Vec<TrimCurve2d>,
    pub is_outer: bool,
}

/// Exact 2D curve in (u,v). Line + Arc cover fillet cases; Nurbs2d is escape hatch.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum TrimCurve2d {
    Line { a: Vec2, b: Vec2 },
    /// Rational quadratic arc in UV — EXACT. `t0`/`t1` are angles in radians from +U.
    Arc {
        centre: Vec2,
        radius: f32,
        t0: f32,
        t1: f32,
    },
    /// Escape hatch for later loft/boolean — do not use for fillets.
    Nurbs2d {
        cps: Vec<Vec3>,
        knots: Vec<f32>,
        degree: u8,
    },
}

/// How a face was produced — **editable direct modeling**, not a history tree (PLAN Decision 5 / §2b).
///
/// Re-solve is local: change radius → rebuild only faces whose `origin` names that op; move a base
/// face → re-solve every `Fillet`/`Corner` that lists it in `between`/`faces`.
///
/// **Serialize last on [`NurbsFace`]** while the format is unshipped. Bincode does not treat
/// `#[serde(default)]` as “missing field OK” the way JSON does — once `.fl4` files contain solids,
/// adding a field shifts every following byte. Land this **before** any save ships with a solid.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub enum FaceOrigin {
    /// Primitive / un-attributed geometry (box face, imported, etc.).
    Base,
    /// Constant-radius rolling-ball edge fillet (cylinder / general dihedral arc).
    Fillet {
        radius: f32,
        between: [NurbsFaceId; 2],
    },
    /// Equal-r spherical (or toroidal OnSame) corner at a triad.
    Corner {
        radius: f32,
        faces: [NurbsFaceId; 3],
    },
    /// Chamfer between two faces (reserved; not wired yet).
    Chamfer {
        distance: f32,
        between: [NurbsFaceId; 2],
    },
    // ── Append-only arms (bincode: never insert above this line) ──────────
    /// Linear variable-radius edge fillet (`Add(R1,R2,E)`). Appended last so
    /// existing serialized solids keep field layout for prior arms.
    FilletLinear {
        r0: f32,
        r1: f32,
        between: [NurbsFaceId; 2],
    },
    /// Sampled radius law along the edge (`Add(UandR)`). `(t, r)` with t∈[0,1].
    FilletLaw {
        samples: Vec<(f32, f32)>,
        between: [NurbsFaceId; 2],
    },
}

impl Default for FaceOrigin {
    fn default() -> Self {
        FaceOrigin::Base
    }
}

impl FaceOrigin {
    /// Support face ids for an edge fillet origin (`Fillet` / `FilletLinear` / `FilletLaw`).
    pub fn fillet_between(&self) -> Option<[NurbsFaceId; 2]> {
        match self {
            FaceOrigin::Fillet { between, .. }
            | FaceOrigin::FilletLinear { between, .. }
            | FaceOrigin::FilletLaw { between, .. } => Some(*between),
            _ => None,
        }
    }

    /// Radius law recovered for re-solve. Const uses stored `radius`; Linear/Law
    /// preserve the full law. Non-fillet origins → `None`.
    pub fn fillet_radius_law(&self) -> Option<crate::nurbs_fillet::Radius> {
        match self {
            FaceOrigin::Fillet { radius, .. } => {
                Some(crate::nurbs_fillet::Radius::Const(*radius))
            }
            FaceOrigin::FilletLinear { r0, r1, .. } => {
                Some(crate::nurbs_fillet::Radius::Linear { r0: *r0, r1: *r1 })
            }
            FaceOrigin::FilletLaw { samples, .. } => {
                Some(crate::nurbs_fillet::Radius::Law(samples.clone()))
            }
            _ => None,
        }
    }

    /// Peak / representative radius for UI labels (Linear → max(r0,r1), Law → max sample).
    pub fn display_radius(&self) -> Option<f32> {
        match self {
            FaceOrigin::Fillet { radius, .. } | FaceOrigin::Corner { radius, .. } => Some(*radius),
            FaceOrigin::FilletLinear { r0, r1, .. } => Some(r0.max(*r1)),
            FaceOrigin::FilletLaw { samples, .. } => {
                samples.iter().map(|p| p.1).fold(None, |a, r| {
                    Some(a.map(|m: f32| m.max(r)).unwrap_or(r))
                })
            }
            FaceOrigin::Chamfer { distance, .. } => Some(*distance),
            FaceOrigin::Base => None,
        }
    }
}

/// One face patch of a multi-face NURBS solid.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NurbsFace {
    pub id: NurbsFaceId,
    pub surface: std::sync::Arc<NurbsSurface>,
    pub material: crate::primitives::PbrMaterial,
    /// Parameter-space trims. Empty = full untrimmed patch (default).
    #[serde(default)]
    pub trims: Vec<TrimLoop>,
    /// Attribution for local re-solve (Decision 5). **Keep last** for bincode layout.
    #[serde(default)]
    pub origin: FaceOrigin,
}

impl NurbsFace {
    pub fn new(id: NurbsFaceId, surface: NurbsSurface) -> Self {
        Self {
            id,
            surface: std::sync::Arc::new(surface),
            material: crate::primitives::PbrMaterial::default(),
            trims: Vec::new(),
            origin: FaceOrigin::Base,
        }
    }

    pub fn with_origin(mut self, origin: FaceOrigin) -> Self {
        self.origin = origin;
        self
    }

    /// Tessellate for display: untrimmed fast path, or trim-aware mesh.
    pub fn tessellate(&self) -> (Vec<[f32; 8]>, Vec<u32>) {
        if self.trims.is_empty() {
            self.surface.tessellate()
        } else {
            crate::nurbs_trim::tessellate_trimmed(&self.surface, &self.trims)
        }
    }

    /// True if (u,v) is inside the kept region (respects outer loops + holes).
    pub fn uv_inside(&self, u: f32, v: f32) -> bool {
        if self.trims.is_empty() {
            return true;
        }
        crate::nurbs_trim::point_in_trims(Vec2::new(u, v), &self.trims)
    }
}

/// A single NURBS control point with rational weight.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ControlPoint {
    pub position: Vec3,
    pub weight: f32, // 1.0 = standard B-spline
}

impl ControlPoint {
    pub fn new(position: Vec3, weight: f32) -> Self {
        Self { position, weight }
    }

    /// Convert to homogeneous coordinates (wx, wy, wz, w).
    pub fn to_homogeneous(&self) -> [f32; 4] {
        [self.position.x * self.weight,
         self.position.y * self.weight,
         self.position.z * self.weight,
         self.weight]
    }

    /// Convert from homogeneous coordinates.
    pub fn from_homogeneous(h: [f32; 4]) -> Self {
        let w = h[3].max(1e-10);
        Self { position: Vec3::new(h[0] / w, h[1] / w, h[2] / w), weight: w }
    }
}

// ============================================================
// NURBS Curve
// ============================================================

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NurbsCurve {
    pub control_points: Vec<ControlPoint>,
    pub knots: Vec<f32>,
    pub degree: u32,
    pub name: String,
}

impl NurbsCurve {
    /// Evaluate point on curve at parameter t.
    pub fn evaluate(&self, t: f32) -> Vec3 {
        let h = de_boor(self.degree, &self.knots, &self.homogeneous_cps(), t);
        let w = h[3].max(1e-10);
        Vec3::new(h[0] / w, h[1] / w, h[2] / w)
    }

    /// Tangent vector at parameter t (via finite difference).
    pub fn derivative(&self, t: f32) -> Vec3 {
        let eps = 1e-4;
        let (t0, t1) = self.domain();
        let ta = (t - eps).max(t0);
        let tb = (t + eps).min(t1);
        let dt = tb - ta;
        if dt < 1e-8 { return Vec3::ZERO; }
        (self.evaluate(tb) - self.evaluate(ta)) / dt
    }

    /// Insert a knot at parameter t (Boehm's algorithm).
    pub fn insert_knot(&mut self, t: f32) {
        let n = self.control_points.len();
        let p = self.degree as usize;
        let k = find_span(self.degree, &self.knots, t);

        let mut new_cps = Vec::with_capacity(n + 1);
        let mut new_knots = Vec::with_capacity(self.knots.len() + 1);

        // Insert knot value
        for i in 0..=k {
            new_knots.push(self.knots[i]);
        }
        new_knots.push(t);
        for i in (k + 1)..self.knots.len() {
            new_knots.push(self.knots[i]);
        }

        // Compute new control points
        for i in 0..=n {
            if i <= k.saturating_sub(p) {
                new_cps.push(self.control_points[i].clone());
            } else if i > k {
                if i - 1 < n {
                    new_cps.push(self.control_points[i - 1].clone());
                }
            } else {
                let idx = i;
                if idx < n && idx > 0 {
                    let alpha = if self.knots[idx + p] != self.knots[idx] {
                        (t - self.knots[idx]) / (self.knots[idx + p] - self.knots[idx])
                    } else {
                        0.5
                    };
                    let p0 = &self.control_points[idx - 1];
                    let p1 = &self.control_points[idx];
                    new_cps.push(ControlPoint {
                        position: p0.position.lerp(p1.position, alpha),
                        weight: p0.weight + (p1.weight - p0.weight) * alpha,
                    });
                } else if idx < n {
                    new_cps.push(self.control_points[idx].clone());
                }
            }
        }

        self.control_points = new_cps;
        self.knots = new_knots;
    }

    /// Parameter domain [t_min, t_max].
    pub fn domain(&self) -> (f32, f32) {
        let p = self.degree as usize;
        (self.knots[p], self.knots[self.knots.len() - 1 - p])
    }

    /// Generate a uniform knot vector for n control points and given degree.
    pub fn uniform_knots(n_cps: usize, degree: u32) -> Vec<f32> {
        let p = degree as usize;
        let m = n_cps + p;
        let mut knots = Vec::with_capacity(m + 1);
        for i in 0..=m {
            if i <= p {
                knots.push(0.0);
            } else if i >= m - p {
                knots.push(1.0);
            } else {
                knots.push((i - p) as f32 / (m - 2 * p) as f32);
            }
        }
        knots
    }

    fn homogeneous_cps(&self) -> Vec<[f32; 4]> {
        self.control_points.iter().map(|cp| cp.to_homogeneous()).collect()
    }

    // ---- Generators ----

    /// Straight line from start to end (degree 1, 2 CPs).
    pub fn new_line(start: Vec3, end: Vec3) -> Self {
        Self {
            control_points: vec![
                ControlPoint::new(start, 1.0),
                ControlPoint::new(end, 1.0),
            ],
            knots: vec![0.0, 0.0, 1.0, 1.0],
            degree: 1,
            name: "Line".into(),
        }
    }

    /// Circle (rational NURBS, 9 CPs, degree 2).
    pub fn new_circle(radius: f32) -> Self {
        let w = std::f32::consts::FRAC_1_SQRT_2; // cos(45°)
        let r = radius;
        let cps = vec![
            ControlPoint::new(Vec3::new(r, 0.0, 0.0), 1.0),
            ControlPoint::new(Vec3::new(r, 0.0, r), w),
            ControlPoint::new(Vec3::new(0.0, 0.0, r), 1.0),
            ControlPoint::new(Vec3::new(-r, 0.0, r), w),
            ControlPoint::new(Vec3::new(-r, 0.0, 0.0), 1.0),
            ControlPoint::new(Vec3::new(-r, 0.0, -r), w),
            ControlPoint::new(Vec3::new(0.0, 0.0, -r), 1.0),
            ControlPoint::new(Vec3::new(r, 0.0, -r), w),
            ControlPoint::new(Vec3::new(r, 0.0, 0.0), 1.0), // closed
        ];
        let knots = vec![0.0, 0.0, 0.0, 0.25, 0.25, 0.5, 0.5, 0.75, 0.75, 1.0, 1.0, 1.0];
        Self { control_points: cps, knots, degree: 2, name: "Circle".into() }
    }

    /// Arc of given radius and angle range.
    pub fn new_arc(radius: f32, start_deg: f32, end_deg: f32) -> Self {
        let start_rad = start_deg.to_radians();
        let end_rad = end_deg.to_radians();
        let mid_rad = (start_rad + end_rad) * 0.5;
        let half_angle = (end_rad - start_rad) * 0.5;
        let w = half_angle.cos();
        let r = radius;

        let p0 = Vec3::new(r * start_rad.cos(), 0.0, r * start_rad.sin());
        let p2 = Vec3::new(r * end_rad.cos(), 0.0, r * end_rad.sin());
        // Tangent intersection point
        let p1 = Vec3::new(r * mid_rad.cos() / w, 0.0, r * mid_rad.sin() / w);

        Self {
            control_points: vec![
                ControlPoint::new(p0, 1.0),
                ControlPoint::new(p1, w),
                ControlPoint::new(p2, 1.0),
            ],
            knots: vec![0.0, 0.0, 0.0, 1.0, 1.0, 1.0],
            degree: 2,
            name: "Arc".into(),
        }
    }
}

// ============================================================
// NURBS Surface (tensor product)
// ============================================================

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NurbsSurface {
    /// Control point grid: `control_points[row_u][col_v]`.
    pub control_points: Vec<Vec<ControlPoint>>,
    pub knots_u: Vec<f32>,
    pub knots_v: Vec<f32>,
    pub degree_u: u32,
    pub degree_v: u32,
    pub name: String,
    /// Cached tessellation mesh_cache_id.
    pub mesh_cache_id: Option<crate::scene_graph::MeshCacheId>,
    /// Tessellation tolerance for adaptive subdivision.
    pub tess_tolerance: f32,
}

impl NurbsSurface {
    /// Evaluate point on surface at (u, v).
    pub fn evaluate(&self, u: f32, v: f32) -> Vec3 {
        // Tensor product: evaluate in V for each U row, then evaluate in U
        let rows_u = self.control_points.len();
        let mut u_points = Vec::with_capacity(rows_u);
        for row in &self.control_points {
            let h_row: Vec<[f32; 4]> = row.iter().map(|cp| cp.to_homogeneous()).collect();
            let h = de_boor(self.degree_v, &self.knots_v, &h_row, v);
            u_points.push(h);
        }
        let h = de_boor(self.degree_u, &self.knots_u, &u_points, u);
        let w = h[3].max(1e-10);
        Vec3::new(h[0] / w, h[1] / w, h[2] / w)
    }

    /// Partial derivatives ∂S/∂u and ∂S/∂v at (u, v) via finite differences.
    pub fn derivatives(&self, u: f32, v: f32) -> (Vec3, Vec3) {
        let eps = 1e-4;
        let (u0, u1) = self.domain_u();
        let (v0, v1) = self.domain_v();

        let du_a = (u - eps).max(u0);
        let du_b = (u + eps).min(u1);
        let dv_a = (v - eps).max(v0);
        let dv_b = (v + eps).min(v1);

        let du = if (du_b - du_a) > 1e-8 {
            (self.evaluate(du_b, v) - self.evaluate(du_a, v)) / (du_b - du_a)
        } else { Vec3::ZERO };

        let dv = if (dv_b - dv_a) > 1e-8 {
            (self.evaluate(u, dv_b) - self.evaluate(u, dv_a)) / (dv_b - dv_a)
        } else { Vec3::ZERO };

        (du, dv)
    }

    /// Surface normal at (u, v).
    pub fn normal(&self, u: f32, v: f32) -> Vec3 {
        let (du, dv) = self.derivatives(u, v);
        du.cross(dv).normalize_or_zero()
    }

    /// Insert knot in U direction.
    pub fn insert_knot_u(&mut self, u: f32) {
        // For each column (fixed V index), treat the U-row as a curve and insert
        let cols_v = self.control_points[0].len();
        for col in 0..cols_v {
            let mut curve = NurbsCurve {
                control_points: self.control_points.iter().map(|row| row[col].clone()).collect(),
                knots: self.knots_u.clone(),
                degree: self.degree_u,
                name: String::new(),
            };
            curve.insert_knot(u);
            // Write back
            if col == 0 {
                self.knots_u = curve.knots.clone();
                // Resize grid rows
                let new_rows = curve.control_points.len();
                self.control_points.resize(new_rows, vec![ControlPoint::new(Vec3::ZERO, 1.0); cols_v]);
            }
            for (row, cp) in curve.control_points.into_iter().enumerate() {
                if col < self.control_points[row].len() {
                    self.control_points[row][col] = cp;
                }
            }
        }
    }

    /// Insert knot in V direction.
    pub fn insert_knot_v(&mut self, v: f32) {
        let rows_u = self.control_points.len();
        for row in 0..rows_u {
            let mut curve = NurbsCurve {
                control_points: self.control_points[row].clone(),
                knots: self.knots_v.clone(),
                degree: self.degree_v,
                name: String::new(),
            };
            curve.insert_knot(v);
            if row == 0 {
                self.knots_v = curve.knots.clone();
            }
            self.control_points[row] = curve.control_points;
        }
    }

    /// Grid dimensions (rows_u, cols_v).
    pub fn grid_size(&self) -> (usize, usize) {
        let rows = self.control_points.len();
        let cols = if rows > 0 { self.control_points[0].len() } else { 0 };
        (rows, cols)
    }

    /// Parameter domain in U.
    pub fn domain_u(&self) -> (f32, f32) {
        let p = self.degree_u as usize;
        (self.knots_u[p], self.knots_u[self.knots_u.len() - 1 - p])
    }

    /// Parameter domain in V.
    pub fn domain_v(&self) -> (f32, f32) {
        let p = self.degree_v as usize;
        (self.knots_v[p], self.knots_v[self.knots_v.len() - 1 - p])
    }

    /// Adjust a single CV weight.
    pub fn set_weight(&mut self, row: usize, col: usize, weight: f32) {
        if row < self.control_points.len() && col < self.control_points[row].len() {
            self.control_points[row][col].weight = weight.max(0.01);
        }
    }

    /// Total number of control points.
    pub fn cv_count(&self) -> usize {
        self.control_points.iter().map(|r| r.len()).sum()
    }

    // ============================================================
    // Tessellation
    // ============================================================

    /// Uniform tessellation at fixed resolution.
    /// Returns `(vertices [f32;8], indices u32)` matching `Mesh::to_raster_data()` format.
    pub fn tessellate_uniform(&self, steps_u: u32, steps_v: u32) -> (Vec<[f32; 8]>, Vec<u32>) {
        let su = steps_u.max(1);
        let sv = steps_v.max(1);
        let (u0, u1) = self.domain_u();
        let (v0, v1) = self.domain_v();

        let mut verts = Vec::with_capacity(((su + 1) * (sv + 1)) as usize);
        let mut indices = Vec::new();

        for iu in 0..=su {
            let u = u0 + (u1 - u0) * iu as f32 / su as f32;
            for iv in 0..=sv {
                let v = v0 + (v1 - v0) * iv as f32 / sv as f32;
                let p = self.evaluate(u, v);
                let n = self.normal(u, v);
                let uv_u = iu as f32 / su as f32;
                let uv_v = iv as f32 / sv as f32;
                verts.push([p.x, p.y, p.z, n.x, n.y, n.z, uv_u, uv_v]);
            }
        }

        // Generate triangle indices
        for iu in 0..su {
            for iv in 0..sv {
                let a = iu * (sv + 1) + iv;
                let b = a + 1;
                let c = (iu + 1) * (sv + 1) + iv + 1;
                let d = (iu + 1) * (sv + 1) + iv;
                indices.push(a);
                indices.push(b);
                indices.push(c);
                indices.push(a);
                indices.push(c);
                indices.push(d);
            }
        }

        (verts, indices)
    }

    /// Adaptive tessellation: subdivide patches where curvature exceeds tolerance.
    /// Falls back to uniform with adaptive step size.
    pub fn tessellate(&self) -> (Vec<[f32; 8]>, Vec<u32>) {
        // Estimate steps from surface complexity
        let (rows, cols) = self.grid_size();
        let base_u = ((rows as f32 * 4.0) / self.tess_tolerance.max(0.01)).clamp(4.0, 128.0) as u32;
        let base_v = ((cols as f32 * 4.0) / self.tess_tolerance.max(0.01)).clamp(4.0, 128.0) as u32;
        self.tessellate_uniform(base_u, base_v)
    }

    /// Estimated triangle count for given resolution.
    pub fn estimated_triangle_count(&self, steps_u: u32, steps_v: u32) -> u32 {
        steps_u * steps_v * 2
    }

    // ============================================================
    // Generators
    // ============================================================

    /// Flat plane in XZ.
    pub fn new_plane(width: f32, depth: f32) -> Self {
        let hw = width * 0.5;
        let hd = depth * 0.5;
        let cps = vec![
            vec![ControlPoint::new(Vec3::new(-hw, 0.0, -hd), 1.0),
                 ControlPoint::new(Vec3::new(-hw, 0.0,  hd), 1.0)],
            vec![ControlPoint::new(Vec3::new( hw, 0.0, -hd), 1.0),
                 ControlPoint::new(Vec3::new( hw, 0.0,  hd), 1.0)],
        ];
        Self {
            control_points: cps,
            knots_u: vec![0.0, 0.0, 1.0, 1.0],
            knots_v: vec![0.0, 0.0, 1.0, 1.0],
            degree_u: 1,
            degree_v: 1,
            name: "Plane".into(),
            mesh_cache_id: None,
            tess_tolerance: 1.0,
        }
    }

    /// Degree-1 bilinear rectangle from four corner positions (order: 00, 01, 10, 11).
    pub fn new_quad(p00: Vec3, p01: Vec3, p10: Vec3, p11: Vec3, name: &str) -> Self {
        Self {
            control_points: vec![
                vec![ControlPoint::new(p00, 1.0), ControlPoint::new(p01, 1.0)],
                vec![ControlPoint::new(p10, 1.0), ControlPoint::new(p11, 1.0)],
            ],
            knots_u: vec![0.0, 0.0, 1.0, 1.0],
            knots_v: vec![0.0, 0.0, 1.0, 1.0],
            degree_u: 1,
            degree_v: 1,
            name: name.into(),
            mesh_cache_id: None,
            tess_tolerance: 1.0,
        }
    }

    /// Axis-aligned box as **six** NURBS face patches (Plasticity-style cube start).
    /// Half-extents `hx, hy, hz`. Returns `(face_name, surface)` for each side.
    ///
    /// Corner order for `new_quad(p00, p01, p10, p11)` is `control_points[row_u][col_v]`,
    /// so `dU = p10−p00`, `dV = p01−p00`, and `normal = dU × dV` must point **outward**
    /// (away from the box center). Top/Bottom/Front/Back swap p01↔p10 vs a naive
    /// "same U/V winding on every wall" layout so all six agree on outward normals.
    ///
    /// Prefer [`NurbsSolid::new_box`] for scene graph — one outliner node.
    pub fn new_box_faces(hx: f32, hy: f32, hz: f32) -> Vec<(String, Self)> {
        let hx = hx.max(1e-4);
        let hy = hy.max(1e-4);
        let hz = hz.max(1e-4);
        vec![
            (
                "Top".into(),
                // dU=+X, dV=+Z → n = +Y (outward). Swap p01/p10 vs naive layout.
                Self::new_quad(
                    Vec3::new(-hx, hy, -hz),
                    Vec3::new(hx, hy, -hz),
                    Vec3::new(-hx, hy, hz),
                    Vec3::new(hx, hy, hz),
                    "Top",
                ),
            ),
            (
                "Bottom".into(),
                // dU=+X, dV=−Z → n = −Y (outward).
                Self::new_quad(
                    Vec3::new(-hx, -hy, hz),
                    Vec3::new(hx, -hy, hz),
                    Vec3::new(-hx, -hy, -hz),
                    Vec3::new(hx, -hy, -hz),
                    "Bottom",
                ),
            ),
            (
                "Right".into(),
                // dU=+Y, dV=+Z → n = +X (outward) — already correct.
                Self::new_quad(
                    Vec3::new(hx, -hy, -hz),
                    Vec3::new(hx, -hy, hz),
                    Vec3::new(hx, hy, -hz),
                    Vec3::new(hx, hy, hz),
                    "Right",
                ),
            ),
            (
                "Left".into(),
                // dU=+Y, dV=−Z → n = −X (outward) — already correct.
                Self::new_quad(
                    Vec3::new(-hx, -hy, hz),
                    Vec3::new(-hx, -hy, -hz),
                    Vec3::new(-hx, hy, hz),
                    Vec3::new(-hx, hy, -hz),
                    "Left",
                ),
            ),
            (
                "Front".into(),
                // dU=+Y, dV=+X → n = +Z (outward). Swap p01/p10 vs naive layout.
                Self::new_quad(
                    Vec3::new(-hx, -hy, hz),
                    Vec3::new(-hx, hy, hz),
                    Vec3::new(hx, -hy, hz),
                    Vec3::new(hx, hy, hz),
                    "Front",
                ),
            ),
            (
                "Back".into(),
                // dU=+Y, dV=−X → n = −Z (outward). Swap p01/p10 vs naive layout.
                Self::new_quad(
                    Vec3::new(hx, -hy, -hz),
                    Vec3::new(hx, hy, -hz),
                    Vec3::new(-hx, -hy, -hz),
                    Vec3::new(-hx, hy, -hz),
                    "Back",
                ),
            ),
        ]
    }

    /// Six faces with stable [`NurbsFaceId`]s for a solid cube (ids 0..5).
    pub fn new_box_nurbs_faces(hx: f32, hy: f32, hz: f32) -> Vec<NurbsFace> {
        Self::new_box_faces(hx, hy, hz)
            .into_iter()
            .enumerate()
            .map(|(i, (name, mut surf))| {
                surf.name = name;
                NurbsFace::new(NurbsFaceId(i as u64), surf)
            })
            .collect()
    }

    /// Circular outer trim in UV for a bilinear plane whose world extent is
    /// `2*radius` on both axes (UV domain [0,1]² → centre 0.5, UV radius 0.5).
    /// Two semicircle arcs (full 2π collapses under arc-span normalize).
    fn circular_cap_outer_trim() -> Vec<TrimLoop> {
        let c = Vec2::new(0.5, 0.5);
        let r = 0.5;
        let pi = std::f32::consts::PI;
        vec![TrimLoop {
            is_outer: true,
            curves: vec![
                TrimCurve2d::Arc {
                    centre: c,
                    radius: r,
                    t0: 0.0,
                    t1: pi,
                },
                TrimCurve2d::Arc {
                    centre: c,
                    radius: r,
                    t0: pi,
                    t1: 2.0 * pi,
                },
            ],
        }]
    }

    /// Closed cylinder solid: wall + top/bottom planar caps with circular outer trim.
    ///
    /// Face ids: 0 = wall, 1 = top (+Y), 2 = bottom (−Y). Rim solid edges are wall
    /// UV edges 0 (bottom) and 2 (top) — app path uses rim-toroid kernel on those.
    pub fn new_cylinder_nurbs_faces(radius: f32, height: f32) -> Vec<NurbsFace> {
        let radius = radius.max(1e-4);
        let height = height.max(1e-4);
        let hh = height * 0.5;
        let mut wall = Self::new_cylinder(radius, height);
        wall.name = "Wall".into();

        // Cap plane size = 2*radius so UV circle radius 0.5 maps to world radius.
        // Top: outward +Y (same winding as box Top with hx=hz=radius).
        let mut top = Self::new_quad(
            Vec3::new(-radius, hh, -radius),
            Vec3::new(radius, hh, -radius),
            Vec3::new(-radius, hh, radius),
            Vec3::new(radius, hh, radius),
            "Top",
        );
        top.name = "Top".into();
        // Bottom: outward −Y (box Bottom winding).
        let mut bot = Self::new_quad(
            Vec3::new(-radius, -hh, radius),
            Vec3::new(radius, -hh, radius),
            Vec3::new(-radius, -hh, -radius),
            Vec3::new(radius, -hh, -radius),
            "Bottom",
        );
        bot.name = "Bottom".into();

        let trim = Self::circular_cap_outer_trim();
        let mut faces = vec![
            NurbsFace::new(NurbsFaceId(0), wall),
            NurbsFace::new(NurbsFaceId(1), top),
            NurbsFace::new(NurbsFaceId(2), bot),
        ];
        faces[1].trims = trim.clone();
        faces[2].trims = trim;
        faces
    }

    /// Closed sphere solid — single surface of revolution as one face (closed manifold).
    pub fn new_sphere_nurbs_faces(radius: f32) -> Vec<NurbsFace> {
        let mut s = Self::new_sphere(radius.max(1e-4));
        s.name = "Sphere".into();
        vec![NurbsFace::new(NurbsFaceId(0), s)]
    }

    /// Closed torus solid — single surface of revolution as one face.
    pub fn new_torus_nurbs_faces(major_r: f32, minor_r: f32) -> Vec<NurbsFace> {
        let mut s = Self::new_torus(major_r.max(1e-4), minor_r.max(1e-4));
        s.name = "Torus".into();
        vec![NurbsFace::new(NurbsFaceId(0), s)]
    }

    /// NURBS cylinder.
    pub fn new_cylinder(radius: f32, height: f32) -> Self {
        let circle = NurbsCurve::new_circle(radius);
        let hh = height * 0.5;
        let _n = circle.control_points.len();

        let bottom: Vec<ControlPoint> = circle.control_points.iter().map(|cp| {
            ControlPoint::new(cp.position + Vec3::new(0.0, -hh, 0.0), cp.weight)
        }).collect();
        let top: Vec<ControlPoint> = circle.control_points.iter().map(|cp| {
            ControlPoint::new(cp.position + Vec3::new(0.0, hh, 0.0), cp.weight)
        }).collect();

        Self {
            control_points: vec![bottom, top],
            knots_u: vec![0.0, 0.0, 1.0, 1.0],
            knots_v: circle.knots.clone(),
            degree_u: 1,
            degree_v: circle.degree,
            name: "Cylinder".into(),
            mesh_cache_id: None,
            tess_tolerance: 1.0,
        }
    }

    /// NURBS sphere (approximation using rational NURBS).
    pub fn new_sphere(radius: f32) -> Self {
        // Create by revolving a semicircle around Y axis
        let semicircle = NurbsCurve::new_arc(radius, -90.0, 90.0);
        Self::from_revolve(&semicircle, Vec3::Y, 360.0, 8)
    }

    /// NURBS torus.
    pub fn new_torus(major_r: f32, minor_r: f32) -> Self {
        // Create a circle of minor_r in the XY plane, offset by major_r along X
        let profile = NurbsCurve::new_circle(minor_r);
        // Offset CPs along X by major_r
        let mut offset_profile = profile.clone();
        for cp in &mut offset_profile.control_points {
            cp.position.x += major_r;
        }
        Self::from_revolve(&offset_profile, Vec3::Y, 360.0, 8)
    }

    /// Create a surface of revolution by revolving a profile curve around an axis.
    pub fn from_revolve(profile: &NurbsCurve, axis: Vec3, angle_deg: f32, segments: u32) -> Self {
        let segs = segments.max(3);
        let angle_rad = angle_deg.to_radians();
        let axis = axis.normalize_or_zero();

        // For each profile CP, create a circular arc in the plane perpendicular to axis
        let mut rows = Vec::new();

        for s in 0..=segs {
            let t = s as f32 / segs as f32;
            let theta = angle_rad * t;
            let cos_t = theta.cos();
            let sin_t = theta.sin();

            let mut row = Vec::new();
            for cp in &profile.control_points {
                // Rotate cp.position around axis by theta
                let p = cp.position;
                let dot = axis.dot(p);
                let cross = axis.cross(p);
                let rotated = p * cos_t + cross * sin_t + axis * dot * (1.0 - cos_t);
                row.push(ControlPoint::new(rotated, cp.weight));
            }
            rows.push(row);
        }

        let knots_u = NurbsCurve::uniform_knots(rows.len(), 2.min(rows.len() as u32 - 1));

        Self {
            control_points: rows,
            knots_u,
            knots_v: profile.knots.clone(),
            degree_u: 2.min(segments - 1),
            degree_v: profile.degree,
            name: "Revolve".into(),
            mesh_cache_id: None,
            tess_tolerance: 1.0,
        }
    }

    /// Create a surface by lofting through multiple profile curves.
    pub fn from_loft(curves: &[NurbsCurve]) -> Self {
        if curves.is_empty() {
            return Self::new_plane(1.0, 1.0);
        }

        let rows: Vec<Vec<ControlPoint>> = curves.iter().map(|c| c.control_points.clone()).collect();
        let n_rows = rows.len();

        // Use the first curve's knots for V direction
        let knots_v = curves[0].knots.clone();
        let degree_v = curves[0].degree;

        // U direction: uniform knots for the loft
        let degree_u = (n_rows as u32 - 1).min(3);
        let knots_u = NurbsCurve::uniform_knots(n_rows, degree_u);

        Self {
            control_points: rows,
            knots_u,
            knots_v,
            degree_u,
            degree_v,
            name: "Loft".into(),
            mesh_cache_id: None,
            tess_tolerance: 1.0,
        }
    }
}

// ============================================================
// De Boor algorithm (core B-spline evaluation)
// ============================================================

/// Find the knot span index containing parameter t.
fn find_span(degree: u32, knots: &[f32], t: f32) -> usize {
    let n = knots.len() - degree as usize - 2; // last valid span index
    if n == 0 { return 0; }
    if t >= knots[n + 1] { return n; }
    if t <= knots[degree as usize] { return degree as usize; }

    // Binary search
    let mut low = degree as usize;
    let mut high = n + 1;
    let mut mid = (low + high) / 2;
    while t < knots[mid] || t >= knots[mid + 1] {
        if t < knots[mid] { high = mid; } else { low = mid; }
        mid = (low + high) / 2;
        if low >= high { break; }
    }
    mid
}

/// De Boor's algorithm: evaluate a B-spline at parameter t.
/// `control_points` are in homogeneous form [wx, wy, wz, w].
fn de_boor(degree: u32, knots: &[f32], control_points: &[[f32; 4]], t: f32) -> [f32; 4] {
    let p = degree as usize;
    let n = control_points.len();
    if n == 0 { return [0.0; 4]; }
    if n == 1 { return control_points[0]; }

    let k = find_span(degree, knots, t);

    // Copy affected control points
    let mut d: Vec<[f32; 4]> = Vec::with_capacity(p + 1);
    for j in 0..=p {
        let idx = k.saturating_sub(p) + j;
        if idx < n {
            d.push(control_points[idx]);
        } else {
            d.push(control_points[n - 1]);
        }
    }

    // Triangular computation
    for r in 1..=p {
        for j in (r..=p).rev() {
            let i = k.saturating_sub(p) + j;
            let denom = knots.get(i + p + 1 - r).copied().unwrap_or(1.0)
                       - knots.get(i).copied().unwrap_or(0.0);
            let alpha = if denom.abs() > 1e-10 {
                (t - knots.get(i).copied().unwrap_or(0.0)) / denom
            } else {
                0.5
            };
            let one_minus = 1.0 - alpha;
            for c in 0..4 {
                d[j][c] = one_minus * d[j - 1][c] + alpha * d[j][c];
            }
        }
    }

    d[p]
}

// ============================================================
// Tests
// ============================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_evaluation() {
        let line = NurbsCurve::new_line(Vec3::ZERO, Vec3::X);
        let mid = line.evaluate(0.5);
        assert!((mid.x - 0.5).abs() < 1e-4);
        assert!(mid.y.abs() < 1e-4);
    }

    #[test]
    fn circle_is_closed() {
        let circle = NurbsCurve::new_circle(1.0);
        let (t0, t1) = circle.domain();
        let p0 = circle.evaluate(t0);
        let p1 = circle.evaluate(t1);
        assert!((p0 - p1).length() < 1e-3, "Circle should be closed");
    }

    #[test]
    fn circle_radius() {
        let circle = NurbsCurve::new_circle(2.0);
        let (t0, t1) = circle.domain();
        // Sample at several points, all should be ~2.0 from origin
        for i in 0..8 {
            let t = t0 + (t1 - t0) * i as f32 / 8.0;
            let p = circle.evaluate(t);
            let r = Vec3::new(p.x, 0.0, p.z).length();
            assert!((r - 2.0).abs() < 0.05, "Point at t={} has radius {}", t, r);
        }
    }

    #[test]
    fn plane_evaluation() {
        let plane = NurbsSurface::new_plane(2.0, 2.0);
        let center = plane.evaluate(0.5, 0.5);
        assert!(center.length() < 1e-3, "Center of plane should be near origin");

        let corner = plane.evaluate(0.0, 0.0);
        assert!((corner.x - (-1.0)).abs() < 1e-3);
        assert!((corner.z - (-1.0)).abs() < 1e-3);
    }

    #[test]
    fn plane_normal() {
        let plane = NurbsSurface::new_plane(2.0, 2.0);
        let n = plane.normal(0.5, 0.5);
        // Plane normal should point roughly along Y
        assert!(n.y.abs() > 0.9, "Plane normal Y = {}", n.y);
    }

    /// Gate: all six cube faces must have outward normals (away from origin).
    /// Catches mixed winding where some faces shade inside-out / fail backface cull.
    #[test]
    fn box_faces_normals_point_outward() {
        let faces = NurbsSurface::new_box_faces(0.5, 0.5, 0.5);
        assert_eq!(faces.len(), 6, "expected six faces");
        for (name, surf) in &faces {
            let p = surf.evaluate(0.5, 0.5);
            let n = surf.normal(0.5, 0.5);
            assert!(
                n.length() > 0.5,
                "face {name}: normal near zero (len={})",
                n.length()
            );
            // Center of a unit-ish face lies on the box surface; outward n · p > 0.
            let outward = n.dot(p);
            assert!(
                outward > 0.1,
                "face {name}: normal points inward (n={n:?}, p={p:?}, n·p={outward})"
            );
        }
    }

    #[test]
    fn tessellation_produces_geometry() {
        let plane = NurbsSurface::new_plane(2.0, 2.0);
        let (verts, indices) = plane.tessellate_uniform(4, 4);
        assert_eq!(verts.len(), 25); // 5×5 grid
        assert_eq!(indices.len(), 96); // 4×4×2 triangles × 3 indices
    }

    #[test]
    fn cylinder_solid_has_wall_and_two_caps() {
        let faces = NurbsSurface::new_cylinder_nurbs_faces(0.6, 2.0);
        assert_eq!(faces.len(), 3, "wall + top + bottom");
        assert_eq!(faces[0].id.0, 0);
        assert_eq!(faces[0].surface.name, "Wall");
        assert!(faces[0].trims.is_empty(), "wall is untrimmed");
        assert!(!faces[1].trims.is_empty(), "top has circular outer trim");
        assert!(!faces[2].trims.is_empty(), "bottom has circular outer trim");
        // Cap centres kept; corner of square plane outside circle discarded.
        assert!(faces[1].uv_inside(0.5, 0.5));
        assert!(!faces[1].uv_inside(0.01, 0.01));
        // Outward normals
        let n_top = faces[1].surface.normal(0.5, 0.5);
        assert!(n_top.y > 0.5, "top outward +Y");
        let n_bot = faces[2].surface.normal(0.5, 0.5);
        assert!(n_bot.y < -0.5, "bottom outward −Y");
    }

    #[test]
    fn sphere_torus_solid_single_face() {
        let s = NurbsSurface::new_sphere_nurbs_faces(0.8);
        assert_eq!(s.len(), 1);
        let t = NurbsSurface::new_torus_nurbs_faces(0.6, 0.2);
        assert_eq!(t.len(), 1);
    }

    #[test]
    fn cylinder_evaluation() {
        let cyl = NurbsSurface::new_cylinder(1.0, 2.0);
        let p = cyl.evaluate(0.0, 0.0);
        // Bottom ring at angle 0 should be at (1, -1, 0)
        assert!((p.y - (-1.0)).abs() < 0.1);
    }

    #[test]
    fn knot_insertion_preserves_shape() {
        let line = NurbsCurve::new_line(Vec3::ZERO, Vec3::X * 2.0);
        let before = line.evaluate(0.5);
        let mut line2 = line.clone();
        line2.insert_knot(0.5);
        let after = line2.evaluate(0.5);
        assert!((before - after).length() < 1e-3, "Knot insertion should preserve shape");
    }

    #[test]
    fn uniform_knots_valid() {
        let knots = NurbsCurve::uniform_knots(5, 3);
        assert_eq!(knots.len(), 9); // n + p + 1 = 5 + 3 + 1
        assert_eq!(knots[0], 0.0);
        assert_eq!(*knots.last().unwrap(), 1.0);
    }

    #[test]
    fn grid_size() {
        let plane = NurbsSurface::new_plane(2.0, 2.0);
        assert_eq!(plane.grid_size(), (2, 2));
    }

    #[test]
    fn cv_count() {
        let plane = NurbsSurface::new_plane(2.0, 2.0);
        assert_eq!(plane.cv_count(), 4);
    }
}
