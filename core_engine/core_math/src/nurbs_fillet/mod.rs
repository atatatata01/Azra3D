//! Planar-to-planar rolling-ball fillet for NURBS solids (PLAN N4 + N5).
//!
//! Cube edge (Î¸ = 90Â°): exact quarter-cylinder, rational quadratic cross-section.
//! Cube vertex (equal r, orthogonal, **Uniform**): spherical octant corner patch (N5).
//! Gates: |P âˆ’ axis| == r (cylinder), |P âˆ’ centre| == r (sphere), G1 at seams.
//!
//! ## Design
//!
//! - Corners are handled by edge count ([`EdgeValence`]), one routine per count, not one
//!   `count == 3` formula.
//! - Corner sign mix is first-class ([`CornerMix`]): the solid edges meeting at a vertex are all
//!   convex (or all concave), or a mixture. The spherical octant is the **Uniform** case only;
//!   mixed corners (L-bracket, boss on plate) need other patches.
//! - Partial results are first-class (`FilletOutcome` + `faulty_vertices`).
//! - Radius is a **law** (`Radius`), not only a scalar: `Const` is wired first, the enum stays.
//! - Any unhandled `(EdgeValence, CornerMix)` becomes `Unsupported`, never a wrong surface.

use crate::nurbs::{NurbsFace, NurbsSurface};
use glam::Vec3;

pub(crate) const W_MID: f32 = std::f32::consts::FRAC_1_SQRT_2; // cos(45Â°) for exact quarter-circle

// -- Corner classification types ---------------------------------------------
// Dispatch still uses the Uniform octant leaf; these enums exist so a corner is never decided by a
// single predicate and so `Unsupported` can say exactly what was refused.

/// Sign mix of the solid edges meeting at a vertex, seen from a filleted edge (or from the vertex
/// as a whole): are the neighbours convex/concave like that edge, or a mixture?
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CornerMix {
    /// Every valid edge at the vertex has the same sign (cube corner; also all-concave).
    Uniform,
    /// Mixed signs and the filleted edge shares its sign with some, but not all, of the others
    /// (for a whole-vertex query: both signs present).
    Majority,
    /// Mixed signs and the filleted edge is the lone odd one out (or has no valid dihedral).
    Minority,
    /// Too few valid edges meet at the vertex to form a corner (open boundary).
    Open,
    /// Not determined from the geometry available.
    Undetermined,
}

/// Number of solid edges meeting at a corner; each count is a separate routine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EdgeValence {
    Single,
    Pair,
    Triple,
    Many(u8),
    Degenerate,
}

/// How a fillet surface is obtained: closed-form geometry, or a numeric rolling-ball march.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SolveStrategy {
    Analytic,
    NumericRollingBall,
}

/// Radius specification along an edge: a scalar today, with laws modelled from the start.
#[derive(Debug, Clone, PartialEq)]
pub enum Radius {
    Const(f32),
    Linear { r0: f32, r1: f32 },
    /// Samples `(u_along_edge, radius)` for interpolation.
    Law(Vec<(f32, f32)>),
}

impl Radius {
    pub fn as_const(&self) -> Option<f32> {
        match self {
            Radius::Const(r) => Some(*r),
            _ => None,
        }
    }

    /// Peak / representative radius for UI clamps and zero-checks.
    /// Linear → max(r0,r1); Law → max sample r; empty Law → 0.
    pub fn peak(&self) -> f32 {
        match self {
            Radius::Const(r) => *r,
            Radius::Linear { r0, r1 } => r0.max(*r1),
            Radius::Law(samples) => samples
                .iter()
                .map(|(_, r)| *r)
                .fold(0.0f32, f32::max),
        }
    }

    /// True when the law is effectively zero (remove-fillet path).
    pub fn is_near_zero(&self) -> bool {
        self.peak() < 1e-4
    }

    /// Scale all radii so peak becomes `new_peak` (preserves Linear/Law shape).
    /// Const → Const(new_peak). Empty Law stays empty.
    pub fn with_peak(&self, new_peak: f32) -> Radius {
        let new_peak = new_peak.max(0.0);
        match self {
            Radius::Const(_) => Radius::Const(new_peak),
            Radius::Linear { r0, r1 } => {
                let old = r0.max(*r1).max(1e-6);
                let s = new_peak / old;
                Radius::Linear {
                    r0: (*r0 * s).max(0.0),
                    r1: (*r1 * s).max(0.0),
                }
            }
            Radius::Law(samples) if samples.is_empty() => Radius::Law(vec![]),
            Radius::Law(samples) => {
                let old = samples
                    .iter()
                    .map(|(_, r)| *r)
                    .fold(0.0f32, f32::max)
                    .max(1e-6);
                let s = new_peak / old;
                Radius::Law(
                    samples
                        .iter()
                        .map(|(t, r)| (*t, (*r * s).max(0.0)))
                        .collect(),
                )
            }
        }
    }

    /// Evaluate at parameter t âˆˆ [0,1] along the edge (Const ignores t).
    pub fn at(&self, t: f32) -> f32 {
        match self {
            Radius::Const(r) => *r,
            Radius::Linear { r0, r1 } => r0 + (r1 - r0) * t.clamp(0.0, 1.0),
            Radius::Law(samples) => {
                if samples.is_empty() {
                    return 0.0;
                }
                let t = t.clamp(0.0, 1.0);
                // Piecewise linear on sorted u
                let mut s = samples.clone();
                s.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
                if t <= s[0].0 {
                    return s[0].1;
                }
                for w in s.windows(2) {
                    if t <= w[1].0 {
                        let u0 = w[0].0;
                        let u1 = w[1].0;
                        let a = if (u1 - u0).abs() < 1e-12 {
                            0.0
                        } else {
                            (t - u0) / (u1 - u0)
                        };
                        return w[0].1 + (w[1].1 - w[0].1) * a;
                    }
                }
                s.last().map(|p| p.1).unwrap_or(0.0)
            }
        }
    }
}

/// Result of a batch fillet that may be only partly applied.
#[derive(Debug, Clone)]
pub struct FilletOutcome {
    /// Faces after the op (may be partial if `faulty_vertices` is non-empty).
    pub faces: Vec<NurbsFace>,
    /// Vertices that failed, with the reason (never silent).
    pub faulty_vertices: Vec<(Vec3, FilletError)>,
}

impl FilletOutcome {
    pub fn is_complete(&self) -> bool {
        self.faulty_vertices.is_empty()
    }
}

#[derive(Debug, Clone)]
pub enum FilletError {
    NotPlanar,
    NotOrthogonal,
    RadiusTooLarge { max: f32 },
    DegenerateEdge,
    BadEdgeIndex,
    /// Rolling-ball march failed..
    RollTraceFailed,
    /// No valid starting solution..
    NoSeedSolution,
    /// Result self-intersects..
    SelfIntersecting,
    /// Unhandled corner configuration — refuse, do not invent a surface.
    Unsupported {
        kind: EdgeValence,
        state: CornerMix,
    },
    /// Deprecated: non-monotone laws are handled by per-station bent rails
    /// ([`shrink_face_edge_law`]). Kept for ABI/toast match; no longer returned.
    NonMonotoneRadiusLaw,
    /// Three (or more) filleted edges meet with **unequal** radii at a vertex.
    ///
    /// Equal-r Uniform → spherical octant. Unequal → cylinders with free ends only
    /// (no invented multi-radius corner). Soft fault on the partial outcome; geometry
    /// still builds edge fillets. A per-vertex variable-radius blend is planned work (Tier 1.10).
    UnequalRadiusCorner {
        radii: [f32; 3],
    },
    /// One or both support faces are non-planar and no analytic curved path matched.
    ///
    /// Planar–planar is the main solver. Analytic **cylinder + plane rim** (toroid) is
    /// available via [`fillet_cylinder_plane_rim`]. General free-form rolling ball is Tier 2.6.
    CurvedFace {
        /// Short reason for toasts / tests (`"non-planar support"`, `"no cylinder+plane rim"`, …).
        reason: &'static str,
    },
}

/// Result of filleting one solid edge: updated face A, face B, and new fillet face.
#[derive(Clone, Debug)]
pub struct PlanarFilletResult {
    pub face_a: NurbsSurface,
    pub face_b: NurbsSurface,
    pub fillet: NurbsSurface,
    pub radius: f32,
    pub axis_origin: Vec3,
    pub axis_dir: Vec3,
}

mod cap;
mod oracle;
mod solve;

pub use cap::*;
pub use oracle::*;
pub use solve::*;

#[cfg(test)]
mod tests;
