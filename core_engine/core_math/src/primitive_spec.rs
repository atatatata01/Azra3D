//! Parametric poly primitives — the single source of truth for Add-Mesh geometry.
//!
//! Plan: `docs/planning/poly/PARAMETRIC_PRIMITIVES.md` (owner-requested 2026-08-04).
//!
//! # Why this module exists
//!
//! Before this, poly primitive parameters ("16 sides") lived only in an ephemeral UI struct
//! (`PolyCreateAdjust`) and were discarded the moment the F9 redo panel dismissed — a poly cube
//! could never be re-parameterized, while an SDF prim always could. `PrimitiveSpec` closes that
//! asymmetry: it is a serializable description of a primitive, and `build()` is the ONE function
//! that turns it into geometry.
//!
//! # Relationship to `half_edge::builders`
//!
//! The legacy `HalfEdgeMesh::new_box/new_cylinder/...` constructors remain the canonical simple
//! form — 332 call sites depend on them, many on their exact vertex ORDERING (cube corner indices
//! are hard-coded across the bevel/knife/unbevel test suites). They are NOT duplicated here: each
//! `build()` delegates to them whenever the spec is at default parameters, and only runs the
//! generalized path when a parameter actually demands it. The `default_matches_legacy_*` tests
//! assert the two agree, so the paths cannot silently diverge.

use crate::half_edge::HalfEdgeMesh;
use glam::Vec3;
use serde::{Deserialize, Serialize};

// ============================================================
// Shared parameter types
// ============================================================

/// How to close the flat end of a revolved primitive (circle, cylinder, cone).
///
/// SERIALIZED — append new variants at the END only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CapFill {
    /// No cap — leaves a boundary edge loop (a tube / open disc).
    None,
    /// One n-gon spanning the whole rim. Blender's default; keeps the rim a single clean edge
    /// loop that `edge_select::edge_loop` can walk all the way around.
    NGon,
    /// Triangle fan from a center vertex. Denser, but every face is a triangle.
    TriFan,
}

impl Default for CapFill {
    fn default() -> Self {
        CapFill::NGon
    }
}

/// What to do with the two open ends of a partial sweep.
///
/// SERIALIZED — append new variants at the END only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ArcClose {
    /// Leave the sweep open — a shell/ribbon with boundary edges. Legal (manifold-with-boundary),
    /// but the object is not solid.
    Open,
    /// Flat radial faces at both ends — the "slice of pie" look. Default.
    Wall,
}

impl Default for ArcClose {
    fn default() -> Self {
        ArcClose::Wall
    }
}

/// Partial sweep around the axis of revolution — "remove a slice of the pie" (owner 2026-08-04).
///
/// Deliberately shared by every revolved primitive rather than being a cylinder-only `angle`
/// field: a half-torus and a half-cylinder are the same concept, and a per-primitive copy would
/// be duplicated math.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ArcSpec {
    /// Degrees swept. Clamped to `MIN_SWEEP..=360`. 360 == full revolution (the seam welds).
    pub sweep_deg: f32,
    /// Degrees the sweep start is rotated by.
    pub start_deg: f32,
    /// How the two open ends are handled when `sweep_deg < 360`.
    pub close: ArcClose,
}

impl ArcSpec {
    pub const MIN_SWEEP: f32 = 1.0;

    pub const FULL: ArcSpec = ArcSpec {
        sweep_deg: 360.0,
        start_deg: 0.0,
        close: ArcClose::Wall,
    };

    /// True when this is a complete revolution — the ring closes and `close` is irrelevant.
    ///
    /// The full case MUST stay byte-identical to the legacy builders, so the comparison is
    /// deliberately `>=` against a value just under 360 rather than an exact float equality.
    pub fn is_full(&self) -> bool {
        self.sweep_deg >= 360.0 - 1e-4
    }

    /// Sweep in radians, clamped to the legal domain.
    pub fn sweep_rad(&self) -> f32 {
        self.sweep_deg
            .clamp(Self::MIN_SWEEP, 360.0)
            .to_radians()
    }

    pub fn start_rad(&self) -> f32 {
        self.start_deg.to_radians()
    }

    /// Number of ring VERTICES for `sides` segments.
    ///
    /// A full ring closes, so N segments need N vertices. A partial arc is open, so N segments
    /// need N+1 vertices. `sides` always counts segments across the SWEPT portion, which keeps
    /// tessellation density constant while the user drags the sweep slider (Blender's behavior).
    pub fn ring_verts(&self, sides: usize) -> usize {
        if self.is_full() {
            sides
        } else {
            sides + 1
        }
    }

    /// Angle of ring vertex `i`.
    pub fn angle_at(&self, i: usize, sides: usize) -> f32 {
        let sweep = self.sweep_rad();
        let denom = if self.is_full() { sides } else { sides.max(1) } as f32;
        self.start_rad() + sweep * (i as f32) / denom
    }
}

impl Default for ArcSpec {
    fn default() -> Self {
        ArcSpec::FULL
    }
}

/// Rounded corners / rim bevel (owner 2026-08-04).
///
/// Applied by running the existing bevel over the primitive's hard edges — see
/// `PrimitiveSpec::round_edges()`. There is deliberately no second corner-rounding math path;
/// `docs/planning/poly/BEVEL.md` is the knowledge base and must be read before changing this.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct RoundSpec {
    /// Bevel radius. `0.0` == off, and off is asserted byte-identical to the un-rounded mesh.
    pub radius: f32,
    /// Arc resolution of the round. Clamped `1..=MAX_SEGMENTS`.
    pub segments: u32,
}

impl RoundSpec {
    pub const MAX_SEGMENTS: u32 = 16;

    pub const OFF: RoundSpec = RoundSpec {
        radius: 0.0,
        segments: 2,
    };

    pub fn is_off(&self) -> bool {
        self.radius <= 1e-6
    }
}

impl Default for RoundSpec {
    fn default() -> Self {
        RoundSpec::OFF
    }
}

// ============================================================
// Clamp domain — ONE place, per plan §3
// ============================================================

/// Segments around a revolved primitive.
pub const MIN_SIDES: u32 = 3;
/// A partial arc is open, so two segments are enough to be non-degenerate.
pub const MIN_SIDES_ARC: u32 = 2;
pub const MAX_SIDES: u32 = 256;
/// Linear subdivisions (height segments, plane subdivisions, sphere rings).
pub const MIN_SUBDIV: u32 = 1;
pub const MAX_SUBDIV: u32 = 256;
/// A sphere needs at least two rings to have a body at all.
pub const MIN_RINGS: u32 = 2;
/// Ico subdivision 6 is ~1.3M triangles. Hard cap, not a suggestion.
pub const MAX_ICO_SUBDIV: u32 = 5;
pub const MIN_DIM: f32 = 0.001;

fn clamp_sides(sides: u32, arc: &ArcSpec) -> u32 {
    let lo = if arc.is_full() { MIN_SIDES } else { MIN_SIDES_ARC };
    sides.clamp(lo, MAX_SIDES)
}

fn clamp_subdiv(n: u32) -> u32 {
    n.clamp(MIN_SUBDIV, MAX_SUBDIV)
}

// ============================================================
// Per-primitive specs
// ============================================================

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct BoxSpec {
    /// Full size (not half-extents) on each axis.
    pub size: Vec3,
    pub seg_x: u32,
    pub seg_y: u32,
    pub seg_z: u32,
    pub round: RoundSpec,
}

impl Default for BoxSpec {
    fn default() -> Self {
        Self {
            size: Vec3::splat(2.0),
            seg_x: 1,
            seg_y: 1,
            seg_z: 1,
            round: RoundSpec::OFF,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct PlaneSpec {
    pub width: f32,
    pub depth: f32,
    pub subdivs_w: u32,
    pub subdivs_d: u32,
}

impl Default for PlaneSpec {
    fn default() -> Self {
        Self {
            width: 2.0,
            depth: 2.0,
            subdivs_w: 1,
            subdivs_d: 1,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct CircleSpec {
    pub radius: f32,
    pub sides: u32,
    pub fill: CapFill,
    pub arc: ArcSpec,
}

impl Default for CircleSpec {
    fn default() -> Self {
        Self {
            radius: 1.0,
            sides: 32,
            fill: CapFill::NGon,
            arc: ArcSpec::FULL,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct UvSphereSpec {
    pub radius: f32,
    /// Segments around the Y axis.
    pub segments: u32,
    /// Bands from pole to pole.
    pub rings: u32,
    pub arc: ArcSpec,
}

impl Default for UvSphereSpec {
    fn default() -> Self {
        Self {
            radius: 1.0,
            segments: 32,
            rings: 16,
            arc: ArcSpec::FULL,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct IcoSphereSpec {
    pub radius: f32,
    pub subdivisions: u32,
}

impl Default for IcoSphereSpec {
    fn default() -> Self {
        Self {
            radius: 1.0,
            subdivisions: 2,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct CylinderSpec {
    pub radius: f32,
    pub height: f32,
    pub sides: u32,
    /// Divisions along the height.
    pub vsegments: u32,
    pub cap_fill: CapFill,
    pub arc: ArcSpec,
    pub round: RoundSpec,
}

impl Default for CylinderSpec {
    fn default() -> Self {
        Self {
            radius: 1.0,
            height: 2.0,
            sides: 32,
            vsegments: 1,
            cap_fill: CapFill::NGon,
            arc: ArcSpec::FULL,
            round: RoundSpec::OFF,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ConeSpec {
    pub radius_bottom: f32,
    /// `> 0` turns the cone into a frustum (truncated cone).
    pub radius_top: f32,
    pub height: f32,
    pub sides: u32,
    pub vsegments: u32,
    pub cap_fill: CapFill,
    pub arc: ArcSpec,
}

impl Default for ConeSpec {
    fn default() -> Self {
        Self {
            radius_bottom: 1.0,
            radius_top: 0.0,
            height: 2.0,
            sides: 32,
            vsegments: 1,
            cap_fill: CapFill::NGon,
            arc: ArcSpec::FULL,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct TorusSpec {
    pub major_r: f32,
    pub minor_r: f32,
    pub major_segs: u32,
    pub minor_segs: u32,
    /// Sweep of the MAJOR revolution (a partial torus / elbow).
    pub arc: ArcSpec,
}

impl Default for TorusSpec {
    fn default() -> Self {
        Self {
            major_r: 1.0,
            minor_r: 0.25,
            major_segs: 32,
            minor_segs: 16,
            arc: ArcSpec::FULL,
        }
    }
}

/// A parametric primitive. One typed struct per shape — deliberately NOT a flat bag of every
/// field, so illegal combinations are unrepresentable and `match` is exhaustive.
///
/// SERIALIZED — append new variants at the END only.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum PrimitiveSpec {
    Box(BoxSpec),
    Plane(PlaneSpec),
    Circle(CircleSpec),
    UvSphere(UvSphereSpec),
    IcoSphere(IcoSphereSpec),
    Cylinder(CylinderSpec),
    Cone(ConeSpec),
    Torus(TorusSpec),
}

impl PrimitiveSpec {
    /// The ONE geometry producer. Total — clamps internally, never fails, never returns a
    /// wrong-topology mesh (plan §5: fail closed, never fake).
    pub fn build(&self) -> HalfEdgeMesh {
        match self {
            PrimitiveSpec::Box(s) => build_box(s),
            PrimitiveSpec::Plane(s) => build_plane(s),
            PrimitiveSpec::Circle(s) => build_circle(s),
            PrimitiveSpec::UvSphere(s) => build_uv_sphere(s),
            PrimitiveSpec::IcoSphere(s) => build_ico_sphere(s),
            PrimitiveSpec::Cylinder(s) => build_cylinder(s),
            PrimitiveSpec::Cone(s) => build_cone(s),
            PrimitiveSpec::Torus(s) => build_torus(s),
        }
    }

    /// Display name, also the default object name at creation.
    pub fn label(&self) -> &'static str {
        match self {
            PrimitiveSpec::Box(_) => "Box",
            PrimitiveSpec::Plane(_) => "Plane",
            PrimitiveSpec::Circle(_) => "Circle",
            PrimitiveSpec::UvSphere(_) => "Sphere",
            PrimitiveSpec::IcoSphere(_) => "Ico Sphere",
            PrimitiveSpec::Cylinder(_) => "Cylinder",
            PrimitiveSpec::Cone(_) => "Cone",
            PrimitiveSpec::Torus(_) => "Torus",
        }
    }

    /// Stable identifier, used by menus/ops and by the create-defaults memory.
    pub fn kind_id(&self) -> &'static str {
        match self {
            PrimitiveSpec::Box(_) => "box",
            PrimitiveSpec::Plane(_) => "plane",
            PrimitiveSpec::Circle(_) => "circle",
            PrimitiveSpec::UvSphere(_) => "sphere",
            PrimitiveSpec::IcoSphere(_) => "icosphere",
            PrimitiveSpec::Cylinder(_) => "cylinder",
            PrimitiveSpec::Cone(_) => "cone",
            PrimitiveSpec::Torus(_) => "torus",
        }
    }

    /// Default spec for a `kind_id`. `None` for an unknown id — callers must not invent geometry.
    pub fn from_kind_id(kind: &str) -> Option<Self> {
        Some(match kind {
            "box" => PrimitiveSpec::Box(BoxSpec::default()),
            "plane" => PrimitiveSpec::Plane(PlaneSpec::default()),
            "circle" => PrimitiveSpec::Circle(CircleSpec::default()),
            "sphere" => PrimitiveSpec::UvSphere(UvSphereSpec::default()),
            "icosphere" => PrimitiveSpec::IcoSphere(IcoSphereSpec::default()),
            "cylinder" => PrimitiveSpec::Cylinder(CylinderSpec::default()),
            "cone" => PrimitiveSpec::Cone(ConeSpec::default()),
            "torus" => PrimitiveSpec::Torus(TorusSpec::default()),
            _ => return None,
        })
    }

    /// Every kind, in menu order.
    pub const ALL_KINDS: [&'static str; 8] = [
        "box",
        "plane",
        "circle",
        "sphere",
        "icosphere",
        "cylinder",
        "cone",
        "torus",
    ];
}

// ============================================================
// Drag-editable counts — the data behind the viewport chords
// ============================================================

/// A local axis of the primitive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Axis3 {
    X,
    Y,
    Z,
}

/// A segment count a viewport drag can edit.
///
/// Every variant names a REAL field of exactly one spec — there is no catch-all, so a shape that
/// gains a new count cannot silently fall through to the wrong one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SegField {
    SegX,
    SegY,
    SegZ,
    SubdivW,
    SubdivD,
    /// Segments around the axis of revolution (cylinder/cone/circle `sides`, sphere `segments`,
    /// torus `major_segs`).
    Radial,
    /// Divisions along the axis of revolution (cylinder/cone `vsegments`).
    Axial,
    /// Pole-to-pole bands on a UV sphere.
    Rings,
    /// Segments around a torus's tube cross-section.
    MinorRing,
    IcoSubdiv,
}

impl PrimitiveSpec {
    /// Which count a drag along `axis` (the primitive's LOCAL axis) should edit.
    ///
    /// `None` means that axis subdivides nothing on this shape — e.g. a plane has no thickness, so
    /// dragging along local Y must be a no-op rather than quietly editing something else.
    ///
    /// This lives here, next to the specs, so the axis→field policy is stated ONCE. Putting it in
    /// the input handler would mean a second exhaustive match that could drift from this one.
    pub fn seg_for_axis(&self, axis: Axis3) -> Option<SegField> {
        use Axis3::*;
        use PrimitiveSpec as P;
        Some(match (self, axis) {
            (P::Box(_), X) => SegField::SegX,
            (P::Box(_), Y) => SegField::SegY,
            (P::Box(_), Z) => SegField::SegZ,
            (P::Plane(_), X) => SegField::SubdivW,
            (P::Plane(_), Z) => SegField::SubdivD,
            (P::Plane(_), Y) => return None, // a plane has no thickness
            (P::Circle(_), X | Z) => SegField::Radial,
            (P::Circle(_), Y) => return None,
            (P::UvSphere(_), Y) => SegField::Rings,
            (P::UvSphere(_), X | Z) => SegField::Radial,
            (P::IcoSphere(_), _) => SegField::IcoSubdiv, // uniform: no axis split
            (P::Cylinder(_), Y) => SegField::Axial,
            (P::Cylinder(_), X | Z) => SegField::Radial,
            (P::Cone(_), Y) => SegField::Axial,
            (P::Cone(_), X | Z) => SegField::Radial,
            (P::Torus(_), Y) => SegField::MinorRing,
            (P::Torus(_), X | Z) => SegField::Radial,
        })
    }

    /// Current value of a count, or `None` if this shape has no such field.
    pub fn seg_get(&self, f: SegField) -> Option<u32> {
        use PrimitiveSpec as P;
        use SegField as F;
        Some(match (self, f) {
            (P::Box(s), F::SegX) => s.seg_x,
            (P::Box(s), F::SegY) => s.seg_y,
            (P::Box(s), F::SegZ) => s.seg_z,
            (P::Plane(s), F::SubdivW) => s.subdivs_w,
            (P::Plane(s), F::SubdivD) => s.subdivs_d,
            (P::Circle(s), F::Radial) => s.sides,
            (P::UvSphere(s), F::Radial) => s.segments,
            (P::UvSphere(s), F::Rings) => s.rings,
            (P::IcoSphere(s), F::IcoSubdiv) => s.subdivisions,
            (P::Cylinder(s), F::Radial) => s.sides,
            (P::Cylinder(s), F::Axial) => s.vsegments,
            (P::Cone(s), F::Radial) => s.sides,
            (P::Cone(s), F::Axial) => s.vsegments,
            (P::Torus(s), F::Radial) => s.major_segs,
            (P::Torus(s), F::MinorRing) => s.minor_segs,
            _ => return None,
        })
    }

    /// Set a count, clamped to this field's legal domain. No-op if the field does not apply.
    pub fn seg_set(&mut self, f: SegField, v: u32) {
        use PrimitiveSpec as P;
        use SegField as F;
        // Clamp at the point of edit AND inside `build()` — the UI must never show a value the
        // rebuild would silently change under the user.
        let radial = |arc: &ArcSpec, v: u32| clamp_sides(v, arc);
        match (self, f) {
            (P::Box(s), F::SegX) => s.seg_x = clamp_subdiv(v),
            (P::Box(s), F::SegY) => s.seg_y = clamp_subdiv(v),
            (P::Box(s), F::SegZ) => s.seg_z = clamp_subdiv(v),
            (P::Plane(s), F::SubdivW) => s.subdivs_w = clamp_subdiv(v),
            (P::Plane(s), F::SubdivD) => s.subdivs_d = clamp_subdiv(v),
            (P::Circle(s), F::Radial) => s.sides = radial(&s.arc, v),
            (P::UvSphere(s), F::Radial) => s.segments = radial(&s.arc, v),
            (P::UvSphere(s), F::Rings) => s.rings = v.clamp(MIN_RINGS, MAX_SUBDIV),
            (P::IcoSphere(s), F::IcoSubdiv) => s.subdivisions = v.min(MAX_ICO_SUBDIV),
            (P::Cylinder(s), F::Radial) => s.sides = radial(&s.arc, v),
            (P::Cylinder(s), F::Axial) => s.vsegments = clamp_subdiv(v),
            (P::Cone(s), F::Radial) => s.sides = radial(&s.arc, v),
            (P::Cone(s), F::Axial) => s.vsegments = clamp_subdiv(v),
            (P::Torus(s), F::Radial) => s.major_segs = radial(&s.arc, v),
            (P::Torus(s), F::MinorRing) => s.minor_segs = v.clamp(MIN_SIDES, MAX_SIDES),
            _ => {}
        }
    }

    /// Every count this shape exposes — the Alt+Shift "all segments" set.
    pub fn seg_fields(&self) -> &'static [SegField] {
        use PrimitiveSpec as P;
        use SegField as F;
        match self {
            P::Box(_) => &[F::SegX, F::SegY, F::SegZ],
            P::Plane(_) => &[F::SubdivW, F::SubdivD],
            P::Circle(_) => &[F::Radial],
            P::UvSphere(_) => &[F::Radial, F::Rings],
            P::IcoSphere(_) => &[F::IcoSubdiv],
            P::Cylinder(_) => &[F::Radial, F::Axial],
            P::Cone(_) => &[F::Radial, F::Axial],
            P::Torus(_) => &[F::Radial, F::MinorRing],
        }
    }

    /// The rounding parameters, for shapes that have them (Box, Cylinder). `None` elsewhere, so
    /// the bevel chords fall through to normal handling rather than inventing a meaning.
    pub fn round(&self) -> Option<RoundSpec> {
        match self {
            PrimitiveSpec::Box(s) => Some(s.round),
            PrimitiveSpec::Cylinder(s) => Some(s.round),
            _ => None,
        }
    }

    pub fn round_mut(&mut self) -> Option<&mut RoundSpec> {
        match self {
            PrimitiveSpec::Box(s) => Some(&mut s.round),
            PrimitiveSpec::Cylinder(s) => Some(&mut s.round),
            _ => None,
        }
    }

    /// Largest legal round radius for this shape — beyond it the offset faces cross.
    /// Mirrors the clamp inside `build()` so the drag can bound itself identically.
    pub fn round_max_radius(&self) -> f32 {
        let extent = match self {
            PrimitiveSpec::Box(s) => s.size.min_element(),
            PrimitiveSpec::Cylinder(s) => (s.radius * 2.0).min(s.height),
            _ => 0.0,
        };
        (extent * 0.5 - MIN_DIM).max(0.0)
    }
}

// ============================================================
// Viewport handles — draggable knobs for the continuous dimensions
// ============================================================

/// A dimension a viewport handle can drag.
///
/// Continuous values only. Integer counts stay on `Ctrl+R`, because a handle that snaps between
/// whole numbers reads as broken rather than precise.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PrimHandle {
    SizeX,
    SizeY,
    SizeZ,
    /// Main radius: circle/sphere radius, cylinder radius, cone bottom radius, torus major.
    Radius,
    /// Cone top radius (0 = pointed cone, >0 = frustum).
    RadiusTop,
    Height,
    /// Torus tube radius.
    Minor,
    /// Round / bevel radius (Box + Cylinder).
    Round,
}

/// One handle: where it sits in the body's LOCAL space, which way dragging it grows the value,
/// and what the value currently is.
#[derive(Debug, Clone, Copy)]
pub struct HandleDef {
    pub handle: PrimHandle,
    /// Local-space position of the knob.
    pub anchor: Vec3,
    /// Local-space direction in which dragging INCREASES the value (unit length).
    pub axis: Vec3,
    pub value: f32,
}

impl PrimitiveSpec {
    /// Handles to draw for this primitive, in local space.
    ///
    /// Anchors sit ON the surface the value controls, so the knob is always where the user would
    /// reach for it. Kept here beside the specs — deriving handle placement in the UI layer would
    /// mean a second exhaustive match that could drift from the geometry.
    pub fn handle_defs(&self) -> Vec<HandleDef> {
        use PrimitiveSpec as P;
        let h = |handle, anchor, axis: Vec3, value| HandleDef {
            handle,
            anchor,
            axis: axis.normalize_or_zero(),
            value,
        };
        match self {
            P::Box(s) => {
                let e = s.size * 0.5;
                let mut v = vec![
                    h(PrimHandle::SizeX, Vec3::new(e.x, 0.0, 0.0), Vec3::X, s.size.x),
                    h(PrimHandle::SizeY, Vec3::new(0.0, e.y, 0.0), Vec3::Y, s.size.y),
                    h(PrimHandle::SizeZ, Vec3::new(0.0, 0.0, e.z), Vec3::Z, s.size.z),
                ];
                // Round knob rides the +++ corner and grows as it is pulled INWARD along the
                // corner diagonal — the direction the fillet actually eats into the solid.
                v.push(h(
                    PrimHandle::Round,
                    e,
                    -Vec3::ONE,
                    s.round.radius,
                ));
                v
            }
            P::Plane(s) => vec![
                h(PrimHandle::SizeX, Vec3::new(s.width * 0.5, 0.0, 0.0), Vec3::X, s.width),
                h(PrimHandle::SizeZ, Vec3::new(0.0, 0.0, s.depth * 0.5), Vec3::Z, s.depth),
            ],
            P::Circle(s) => vec![h(
                PrimHandle::Radius,
                Vec3::new(s.radius, 0.0, 0.0),
                Vec3::X,
                s.radius,
            )],
            P::UvSphere(s) => vec![h(
                PrimHandle::Radius,
                Vec3::new(s.radius, 0.0, 0.0),
                Vec3::X,
                s.radius,
            )],
            P::IcoSphere(s) => vec![h(
                PrimHandle::Radius,
                Vec3::new(s.radius, 0.0, 0.0),
                Vec3::X,
                s.radius,
            )],
            P::Cylinder(s) => {
                let hh = s.height * 0.5;
                vec![
                    h(PrimHandle::Radius, Vec3::new(s.radius, 0.0, 0.0), Vec3::X, s.radius),
                    h(PrimHandle::Height, Vec3::new(0.0, hh, 0.0), Vec3::Y, s.height),
                    // Rim knob: pulled down-and-inward from the top rim.
                    h(
                        PrimHandle::Round,
                        Vec3::new(s.radius, hh, 0.0),
                        Vec3::new(-1.0, -1.0, 0.0),
                        s.round.radius,
                    ),
                ]
            }
            P::Cone(s) => {
                let hh = s.height * 0.5;
                vec![
                    h(
                        PrimHandle::Radius,
                        Vec3::new(s.radius_bottom, -hh, 0.0),
                        Vec3::X,
                        s.radius_bottom,
                    ),
                    h(
                        PrimHandle::RadiusTop,
                        Vec3::new(s.radius_top.max(0.02), hh, 0.0),
                        Vec3::X,
                        s.radius_top,
                    ),
                    h(PrimHandle::Height, Vec3::new(0.0, hh, 0.0), Vec3::Y, s.height),
                ]
            }
            P::Torus(s) => vec![
                h(PrimHandle::Radius, Vec3::new(s.major_r, 0.0, 0.0), Vec3::X, s.major_r),
                h(
                    PrimHandle::Minor,
                    Vec3::new(s.major_r + s.minor_r, 0.0, 0.0),
                    Vec3::X,
                    s.minor_r,
                ),
            ],
        }
    }

    /// Set the value a handle drives, clamped to the same domain `build()` enforces.
    pub fn handle_set(&mut self, handle: PrimHandle, v: f32) {
        use PrimHandle as H;
        use PrimitiveSpec as P;
        let dim = |x: f32| x.max(MIN_DIM);
        match (self, handle) {
            (P::Box(s), H::SizeX) => s.size.x = dim(v),
            (P::Box(s), H::SizeY) => s.size.y = dim(v),
            (P::Box(s), H::SizeZ) => s.size.z = dim(v),
            (P::Box(s), H::Round) => {
                let max = (s.size.min_element() * 0.5 - MIN_DIM).max(0.0);
                s.round.radius = v.clamp(0.0, max);
            }
            (P::Plane(s), H::SizeX) => s.width = dim(v),
            (P::Plane(s), H::SizeZ) => s.depth = dim(v),
            (P::Circle(s), H::Radius) => s.radius = dim(v),
            (P::UvSphere(s), H::Radius) => s.radius = dim(v),
            (P::IcoSphere(s), H::Radius) => s.radius = dim(v),
            (P::Cylinder(s), H::Radius) => s.radius = dim(v),
            (P::Cylinder(s), H::Height) => s.height = dim(v),
            (P::Cylinder(s), H::Round) => {
                let max = ((s.radius * 2.0).min(s.height) * 0.5 - MIN_DIM).max(0.0);
                s.round.radius = v.clamp(0.0, max);
            }
            (P::Cone(s), H::Radius) => s.radius_bottom = dim(v),
            (P::Cone(s), H::RadiusTop) => s.radius_top = v.max(0.0),
            (P::Cone(s), H::Height) => s.height = dim(v),
            (P::Torus(s), H::Radius) => s.major_r = dim(v),
            (P::Torus(s), H::Minor) => {
                // Clamped against the major radius, exactly as `build()` does — the knob must not
                // be draggable into a self-intersecting hub.
                s.minor_r = v.clamp(MIN_DIM, s.major_r * 0.999);
            }
            _ => {}
        }
    }
}

impl PrimHandle {
    pub fn label(&self) -> &'static str {
        match self {
            PrimHandle::SizeX => "Size X",
            PrimHandle::SizeY => "Size Y",
            PrimHandle::SizeZ => "Size Z",
            PrimHandle::Radius => "Radius",
            PrimHandle::RadiusTop => "Top Radius",
            PrimHandle::Height => "Height",
            PrimHandle::Minor => "Minor Radius",
            PrimHandle::Round => "Round",
        }
    }
}

// ============================================================
// MeshSource — where a PolygonalBody's geometry comes from
// ============================================================

/// Source of truth for a `PolygonalBody`'s geometry.
///
/// # The invariant
///
/// When this is `Primitive`, the body's `mesh` is a DERIVED CACHE — `spec` is the truth and the
/// mesh is whatever `spec.build()` produced. When it is `Authored`, the mesh IS the truth.
///
/// # Why `built_sig` exists
///
/// ~80 call sites across the app mutate a body's mesh directly (bevel, boolean, knife, extrude,
/// slide, …). Requiring every one of them to also clear the spec would be exactly the "next dev
/// needs tribal knowledge" failure the architecture rules forbid — and one missed site means a
/// slider drag silently obliterates the user's edits.
///
/// Instead the spec records the `content_signature()` of the mesh it produced. Any reader can then
/// ask [`MeshSource::live_spec`] whether the mesh still matches, in O(1), with no cooperation from
/// the mutating code. A body whose mesh drifted is simply no longer parametric — which is the
/// truth, detected structurally rather than maintained by hand.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum MeshSource {
    /// The mesh is the truth: imported, or a primitive the user has applied/edited.
    Authored,
    /// The mesh is derived from `spec`. `built_sig` is the `content_signature()` of the mesh
    /// `spec.build()` produced, used to detect external edits.
    Primitive { spec: PrimitiveSpec, built_sig: u64 },
}

impl Default for MeshSource {
    fn default() -> Self {
        MeshSource::Authored
    }
}

impl MeshSource {
    /// Build `spec` and return both the mesh and the `MeshSource` describing it. THE constructor —
    /// pairing a spec with a signature by hand is how they drift apart.
    pub fn build(spec: PrimitiveSpec) -> (HalfEdgeMesh, MeshSource) {
        let mesh = spec.build();
        let built_sig = mesh.content_signature();
        (mesh, MeshSource::Primitive { spec, built_sig })
    }

    /// The spec, but ONLY if `mesh` is still the geometry it produced.
    ///
    /// Returns `None` for an authored body AND for a parametric body whose mesh has since been
    /// edited by any other tool. Callers use this rather than matching on the variant directly, so
    /// a stale spec can never drive the UI or a rebuild.
    pub fn live_spec(&self, mesh: &HalfEdgeMesh) -> Option<PrimitiveSpec> {
        match self {
            MeshSource::Authored => None,
            MeshSource::Primitive { spec, built_sig } => {
                (mesh.content_signature() == *built_sig).then_some(*spec)
            }
        }
    }

    /// True if this claims to be parametric, regardless of whether the mesh still matches.
    /// Use `live_spec` for anything user-visible; this is for diagnostics.
    pub fn claims_primitive(&self) -> bool {
        matches!(self, MeshSource::Primitive { .. })
    }
}

// ============================================================
// Builders
// ============================================================

fn build_plane(s: &PlaneSpec) -> HalfEdgeMesh {
    HalfEdgeMesh::new_plane(
        s.width.max(MIN_DIM),
        s.depth.max(MIN_DIM),
        clamp_subdiv(s.subdivs_w),
        clamp_subdiv(s.subdivs_d),
    )
}

fn build_box(s: &BoxSpec) -> HalfEdgeMesh {
    let size = Vec3::new(
        s.size.x.max(MIN_DIM),
        s.size.y.max(MIN_DIM),
        s.size.z.max(MIN_DIM),
    );
    let (sx, sy, sz) = (
        clamp_subdiv(s.seg_x),
        clamp_subdiv(s.seg_y),
        clamp_subdiv(s.seg_z),
    );

    // The unsegmented box delegates to the legacy builder. Its hand-written 8-vertex ordering is
    // load-bearing: cube corner indices are hard-coded across the bevel/knife/unbevel suites. The
    // grid path below produces the same SHAPE at 1x1x1 but a different vertex ORDER, so it is not
    // a drop-in — `box_grid_matches_legacy_at_one_segment` gates that they stay geometrically
    // equal.
    let mesh = if sx == 1 && sy == 1 && sz == 1 {
        HalfEdgeMesh::new_box(size * 0.5)
    } else {
        build_box_grid(size, sx, sy, sz)
    };

    apply_round(
        mesh,
        &s.round,
        size.min_element(),
        crate::primitive_round::RoundEdges::AllHard,
    )
}

/// Subdivided box: a welded lattice shell. Interior lattice points are never emitted, and shared
/// corner/edge vertices are emitted once, so the result is a closed manifold with no doubles.
fn build_box_grid(size: Vec3, sx: u32, sy: u32, sz: u32) -> HalfEdgeMesh {
    use std::collections::HashMap;

    let half = size * 0.5;
    let (nx, ny, nz) = (sx as usize, sy as usize, sz as usize);

    let mut positions: Vec<Vec3> = Vec::new();
    let mut lattice: HashMap<(usize, usize, usize), usize> = HashMap::new();

    // Emit-on-first-touch so a corner shared by three faces becomes ONE vertex.
    let vert = |i: usize, j: usize, k: usize, positions: &mut Vec<Vec3>, lattice: &mut HashMap<(usize, usize, usize), usize>| -> usize {
        *lattice.entry((i, j, k)).or_insert_with(|| {
            let p = Vec3::new(
                -half.x + size.x * (i as f32 / nx as f32),
                -half.y + size.y * (j as f32 / ny as f32),
                -half.z + size.z * (k as f32 / nz as f32),
            );
            positions.push(p);
            positions.len() - 1
        })
    };

    let mut faces: Vec<Vec<usize>> = Vec::new();

    // Winding matches `new_box`'s outward convention face by face.
    // -Y bottom: [(i,k), (i+1,k), (i+1,k+1), (i,k+1)]
    for i in 0..nx {
        for k in 0..nz {
            let q = [
                vert(i, 0, k, &mut positions, &mut lattice),
                vert(i + 1, 0, k, &mut positions, &mut lattice),
                vert(i + 1, 0, k + 1, &mut positions, &mut lattice),
                vert(i, 0, k + 1, &mut positions, &mut lattice),
            ];
            faces.push(q.to_vec());
        }
    }
    // +Y top: reversed relative to bottom.
    for i in 0..nx {
        for k in 0..nz {
            let q = [
                vert(i, ny, k + 1, &mut positions, &mut lattice),
                vert(i + 1, ny, k + 1, &mut positions, &mut lattice),
                vert(i + 1, ny, k, &mut positions, &mut lattice),
                vert(i, ny, k, &mut positions, &mut lattice),
            ];
            faces.push(q.to_vec());
        }
    }
    // -Z back
    for i in 0..nx {
        for j in 0..ny {
            let q = [
                vert(i, j + 1, 0, &mut positions, &mut lattice),
                vert(i + 1, j + 1, 0, &mut positions, &mut lattice),
                vert(i + 1, j, 0, &mut positions, &mut lattice),
                vert(i, j, 0, &mut positions, &mut lattice),
            ];
            faces.push(q.to_vec());
        }
    }
    // +Z front
    for i in 0..nx {
        for j in 0..ny {
            let q = [
                vert(i + 1, j + 1, nz, &mut positions, &mut lattice),
                vert(i, j + 1, nz, &mut positions, &mut lattice),
                vert(i, j, nz, &mut positions, &mut lattice),
                vert(i + 1, j, nz, &mut positions, &mut lattice),
            ];
            faces.push(q.to_vec());
        }
    }
    // -X left
    for j in 0..ny {
        for k in 0..nz {
            let q = [
                vert(0, j + 1, k + 1, &mut positions, &mut lattice),
                vert(0, j + 1, k, &mut positions, &mut lattice),
                vert(0, j, k, &mut positions, &mut lattice),
                vert(0, j, k + 1, &mut positions, &mut lattice),
            ];
            faces.push(q.to_vec());
        }
    }
    // +X right
    for j in 0..ny {
        for k in 0..nz {
            let q = [
                vert(nx, j + 1, k, &mut positions, &mut lattice),
                vert(nx, j + 1, k + 1, &mut positions, &mut lattice),
                vert(nx, j, k + 1, &mut positions, &mut lattice),
                vert(nx, j, k, &mut positions, &mut lattice),
            ];
            faces.push(q.to_vec());
        }
    }

    HalfEdgeMesh::from_polygons(&positions, &faces)
}

fn build_circle(s: &CircleSpec) -> HalfEdgeMesh {
    let r = s.radius.max(MIN_DIM);
    let arc = s.arc;
    let sides = clamp_sides(s.sides, &arc) as usize;
    let nv = arc.ring_verts(sides);

    let mut positions: Vec<Vec3> = Vec::with_capacity(nv + 2);
    for i in 0..nv {
        let a = arc.angle_at(i, sides);
        positions.push(Vec3::new(r * a.cos(), 0.0, r * a.sin()));
    }

    // A partial arc that is not walled has no interior to fill — it is a bare polyline, which is
    // not a mesh. Fail closed: emit the open fan only when the arc is walled or full.
    let closed_region = arc.is_full() || arc.close == ArcClose::Wall;

    let mut faces: Vec<Vec<usize>> = Vec::new();
    match s.fill {
        CapFill::None => {}
        CapFill::NGon if closed_region => {
            // +Y outward == decreasing theta, matching the cylinder's top cap.
            let mut ring: Vec<usize> = (0..nv).collect();
            if !arc.is_full() {
                // Walled partial disc: the center closes the wedge.
                ring.push(positions.len());
                positions.push(Vec3::ZERO);
            }
            ring.reverse();
            faces.push(ring);
        }
        CapFill::TriFan if closed_region => {
            let c = positions.len();
            positions.push(Vec3::ZERO);
            let segs = if arc.is_full() { nv } else { nv - 1 };
            for i in 0..segs {
                let n = (i + 1) % nv;
                faces.push(vec![c, n, i]);
            }
        }
        _ => {}
    }

    HalfEdgeMesh::from_polygons(&positions, &faces)
}

fn build_uv_sphere(s: &UvSphereSpec) -> HalfEdgeMesh {
    let r = s.radius.max(MIN_DIM);
    let arc = s.arc;
    let segs = clamp_sides(s.segments, &arc);
    let rings = s.rings.clamp(MIN_RINGS, MAX_SUBDIV);

    if arc.is_full() {
        return HalfEdgeMesh::new_uv_sphere(r, segs, rings);
    }

    // Partial sweep: the ring no longer closes, so poles become fans over an open arc and the two
    // cut meridians need walls. Built as a lat/long grid with explicit pole vertices.
    let segs = segs as usize;
    let rings = rings as usize;
    let nv = arc.ring_verts(segs);

    let mut positions: Vec<Vec3> = Vec::new();
    let top = positions.len();
    positions.push(Vec3::new(0.0, r, 0.0));
    let band_base = positions.len();
    for ring in 1..rings {
        let phi = std::f32::consts::PI * ring as f32 / rings as f32;
        let y = r * phi.cos();
        let rr = r * phi.sin();
        for i in 0..nv {
            let a = arc.angle_at(i, segs);
            positions.push(Vec3::new(rr * a.cos(), y, rr * a.sin()));
        }
    }
    let bottom = positions.len();
    positions.push(Vec3::new(0.0, -r, 0.0));

    let at = |ring: usize, i: usize| band_base + (ring - 1) * nv + i;
    let mut faces: Vec<Vec<usize>> = Vec::new();

    // Top fan — wound to agree with the quad bands (see the winding note in `new_uv_sphere`).
    for i in 0..nv - 1 {
        faces.push(vec![top, at(1, i + 1), at(1, i)]);
    }
    // Quad bands.
    for ring in 1..rings - 1 {
        for i in 0..nv - 1 {
            faces.push(vec![
                at(ring, i),
                at(ring, i + 1),
                at(ring + 1, i + 1),
                at(ring + 1, i),
            ]);
        }
    }
    // Bottom fan.
    for i in 0..nv - 1 {
        faces.push(vec![at(rings - 1, i + 1), bottom, at(rings - 1, i)]);
    }

    if arc.close == ArcClose::Wall {
        // Each cut meridian is a half-disc: a fan from the polar axis. The axis is the segment
        // between the two poles, so the wall is a single planar polygon per side.
        let mut start_wall: Vec<usize> = Vec::with_capacity(rings + 1);
        start_wall.push(top);
        for ring in 1..rings {
            start_wall.push(at(ring, 0));
        }
        start_wall.push(bottom);
        let mut end_wall: Vec<usize> = Vec::with_capacity(rings + 1);
        end_wall.push(top);
        for ring in 1..rings {
            end_wall.push(at(ring, nv - 1));
        }
        end_wall.push(bottom);
        end_wall.reverse();
        faces.push(start_wall);
        faces.push(end_wall);
    }

    HalfEdgeMesh::from_polygons(&positions, &faces)
}

fn build_ico_sphere(s: &IcoSphereSpec) -> HalfEdgeMesh {
    let r = s.radius.max(MIN_DIM);
    let subdiv = s.subdivisions.min(MAX_ICO_SUBDIV);

    // Base icosahedron.
    let t = (1.0 + 5.0f32.sqrt()) * 0.5;
    let mut verts: Vec<Vec3> = vec![
        Vec3::new(-1.0, t, 0.0),
        Vec3::new(1.0, t, 0.0),
        Vec3::new(-1.0, -t, 0.0),
        Vec3::new(1.0, -t, 0.0),
        Vec3::new(0.0, -1.0, t),
        Vec3::new(0.0, 1.0, t),
        Vec3::new(0.0, -1.0, -t),
        Vec3::new(0.0, 1.0, -t),
        Vec3::new(t, 0.0, -1.0),
        Vec3::new(t, 0.0, 1.0),
        Vec3::new(-t, 0.0, -1.0),
        Vec3::new(-t, 0.0, 1.0),
    ];
    let mut tris: Vec<[usize; 3]> = vec![
        [0, 11, 5], [0, 5, 1], [0, 1, 7], [0, 7, 10], [0, 10, 11],
        [1, 5, 9], [5, 11, 4], [11, 10, 2], [10, 7, 6], [7, 1, 8],
        [3, 9, 4], [3, 4, 2], [3, 2, 6], [3, 6, 8], [3, 8, 9],
        [4, 9, 5], [2, 4, 11], [6, 2, 10], [8, 6, 7], [9, 8, 1],
    ];

    // Loop subdivision on the unit sphere: split each edge at its midpoint, dedup by edge key so
    // adjacent triangles share the new vertex (no cracks, no doubles).
    use std::collections::HashMap;
    for _ in 0..subdiv {
        let mut midpoint: HashMap<(usize, usize), usize> = HashMap::new();
        let mut next: Vec<[usize; 3]> = Vec::with_capacity(tris.len() * 4);
        for tri in &tris {
            let mut mid = [0usize; 3];
            for e in 0..3 {
                let (a, b) = (tri[e], tri[(e + 1) % 3]);
                let key = if a < b { (a, b) } else { (b, a) };
                mid[e] = *midpoint.entry(key).or_insert_with(|| {
                    let p = ((verts[a] + verts[b]) * 0.5).normalize_or_zero();
                    verts.push(p);
                    verts.len() - 1
                });
            }
            next.push([tri[0], mid[0], mid[2]]);
            next.push([mid[0], tri[1], mid[1]]);
            next.push([mid[2], mid[1], tri[2]]);
            next.push([mid[0], mid[1], mid[2]]);
        }
        tris = next;
    }

    let positions: Vec<Vec3> = verts
        .iter()
        .map(|v| v.normalize_or_zero() * r)
        .collect();
    HalfEdgeMesh::from_triangles(&positions, &tris)
}

fn build_cylinder(s: &CylinderSpec) -> HalfEdgeMesh {
    let r = s.radius.max(MIN_DIM);
    let h = s.height.max(MIN_DIM);
    let arc = s.arc;
    let sides = clamp_sides(s.sides, &arc);
    let vseg = clamp_subdiv(s.vsegments);

    let mesh = if arc.is_full() && vseg == 1 && s.cap_fill == CapFill::NGon {
        HalfEdgeMesh::new_cylinder(r, h, sides)
    } else {
        revolve(&RevolveJob {
            profile: vec![(r, -h * 0.5), (r, h * 0.5)],
            vseg: vseg as usize,
            sides: sides as usize,
            arc,
            cap_fill: s.cap_fill,
        })
    };

    // Only the cap rims — rounding the barrel seams too would melt the silhouette instead of
    // easing the rim, which is not what "rounded cylinder" means.
    apply_round(
        mesh,
        &s.round,
        (r * 2.0).min(h),
        crate::primitive_round::RoundEdges::CapRims,
    )
}

fn build_cone(s: &ConeSpec) -> HalfEdgeMesh {
    let rb = s.radius_bottom.max(MIN_DIM);
    let rt = s.radius_top.max(0.0);
    let h = s.height.max(MIN_DIM);
    let arc = s.arc;
    let sides = clamp_sides(s.sides, &arc);
    let vseg = clamp_subdiv(s.vsegments);

    if arc.is_full() && vseg == 1 && rt <= 0.0 && s.cap_fill == CapFill::TriFan {
        return HalfEdgeMesh::new_cone(rb, h, sides);
    }

    revolve(&RevolveJob {
        profile: vec![(rb, -h * 0.5), (rt, h * 0.5)],
        vseg: vseg as usize,
        sides: sides as usize,
        arc,
        cap_fill: s.cap_fill,
    })
}

fn build_torus(s: &TorusSpec) -> HalfEdgeMesh {
    let major = s.major_r.max(MIN_DIM);
    // A minor radius at or past the major radius self-intersects at the hub. Clamp is the domain
    // definition, applied here at the single entry point (plan §3).
    let minor = s.minor_r.clamp(MIN_DIM, major * 0.999);
    let arc = s.arc;
    let ms = clamp_sides(s.major_segs, &arc);
    let ns = s.minor_segs.clamp(MIN_SIDES, MAX_SIDES);

    if arc.is_full() {
        return HalfEdgeMesh::new_torus(major, minor, ms, ns);
    }

    let (ms, ns) = (ms as usize, ns as usize);
    let nv = arc.ring_verts(ms);
    let mut positions: Vec<Vec3> = Vec::with_capacity(nv * ns);
    for i in 0..nv {
        let theta = arc.angle_at(i, ms);
        let (ct, st) = (theta.cos(), theta.sin());
        for j in 0..ns {
            let phi = std::f32::consts::TAU * j as f32 / ns as f32;
            let (cp, sp) = (phi.cos(), phi.sin());
            positions.push(Vec3::new(
                (major + minor * cp) * ct,
                minor * sp,
                (major + minor * cp) * st,
            ));
        }
    }

    let mut faces: Vec<Vec<usize>> = Vec::new();
    for i in 0..nv - 1 {
        for j in 0..ns {
            let nj = (j + 1) % ns;
            faces.push(vec![
                i * ns + j,
                (i + 1) * ns + j,
                (i + 1) * ns + nj,
                i * ns + nj,
            ]);
        }
    }

    if arc.close == ArcClose::Wall {
        // Cap the tube cross-section at both cut ends with an n-gon disc.
        let start: Vec<usize> = (0..ns).collect();
        let last = (nv - 1) * ns;
        let end: Vec<usize> = (last..last + ns).rev().collect();
        faces.push(start);
        faces.push(end);
    }

    HalfEdgeMesh::from_polygons(&positions, &faces)
}

// ============================================================
// Revolve kernel — shared by cylinder and cone
// ============================================================

/// A profile revolved around the Y axis. `profile` is `(radius, y)` from bottom to top;
/// `vseg` linearly subdivides between consecutive profile points.
struct RevolveJob {
    profile: Vec<(f32, f32)>,
    vseg: usize,
    sides: usize,
    arc: ArcSpec,
    cap_fill: CapFill,
}

fn revolve(job: &RevolveJob) -> HalfEdgeMesh {
    let arc = job.arc;
    let sides = job.sides;
    let nv = arc.ring_verts(sides);
    let full = arc.is_full();

    // Expand the profile by vseg.
    let mut rows: Vec<(f32, f32)> = Vec::new();
    for w in job.profile.windows(2) {
        let (r0, y0) = w[0];
        let (r1, y1) = w[1];
        for v in 0..job.vseg {
            let t = v as f32 / job.vseg as f32;
            rows.push((r0 + (r1 - r0) * t, y0 + (y1 - y0) * t));
        }
    }
    rows.push(*job.profile.last().unwrap());

    // A row at radius 0 is a single apex vertex, not a ring — otherwise every apex "ring" would
    // be `nv` coincident vertices, which is a doubles bug that only shows up after a merge.
    let mut positions: Vec<Vec3> = Vec::new();
    let mut row_base: Vec<usize> = Vec::with_capacity(rows.len());
    let mut row_is_apex: Vec<bool> = Vec::with_capacity(rows.len());
    for &(r, y) in &rows {
        row_base.push(positions.len());
        if r <= 1e-6 {
            positions.push(Vec3::new(0.0, y, 0.0));
            row_is_apex.push(true);
        } else {
            for i in 0..nv {
                let a = arc.angle_at(i, sides);
                positions.push(Vec3::new(r * a.cos(), y, r * a.sin()));
            }
            row_is_apex.push(false);
        }
    }

    let ring_at = |row: usize, i: usize| -> usize {
        if row_is_apex[row] {
            row_base[row]
        } else {
            row_base[row] + i
        }
    };
    let seg_count = if full { sides } else { sides.max(1) };

    let mut faces: Vec<Vec<usize>> = Vec::new();
    for row in 0..rows.len() - 1 {
        for s in 0..seg_count {
            let t = (s + 1) % nv;
            let (lo, hi) = (row, row + 1);
            match (row_is_apex[lo], row_is_apex[hi]) {
                // Normal band.
                (false, false) => faces.push(vec![
                    ring_at(lo, s),
                    ring_at(hi, s),
                    ring_at(hi, t),
                    ring_at(lo, t),
                ]),
                // Converging to a top apex.
                (false, true) => faces.push(vec![ring_at(lo, s), ring_at(hi, 0), ring_at(lo, t)]),
                // Diverging from a bottom apex.
                (true, false) => faces.push(vec![ring_at(lo, 0), ring_at(hi, s), ring_at(hi, t)]),
                // Two consecutive apexes = a degenerate profile span; skip rather than emit a
                // zero-area face (fail closed).
                (true, true) => {}
            }
        }
    }

    // Axis vertices for a walled partial sweep, created ONCE and shared by both caps and both
    // walls. Letting each of them push its own center would leave four coincident-but-distinct
    // vertices on the axis; `from_polygons` twins by vertex INDEX, not by coordinate, so the seam
    // would silently fail to weld and the slice would come out non-manifold.
    let walled = !full && arc.close == ArcClose::Wall;
    let axis_vert: Vec<usize> = if walled {
        (0..rows.len())
            .map(|row| {
                if row_is_apex[row] {
                    row_base[row]
                } else {
                    positions.push(Vec3::new(0.0, rows[row].1, 0.0));
                    positions.len() - 1
                }
            })
            .collect()
    } else {
        Vec::new()
    };
    let axis_at = |row: usize| -> Option<usize> {
        if walled {
            Some(axis_vert[row])
        } else {
            None
        }
    };

    // Caps at the two ends, only where the end is an actual ring.
    let last = rows.len() - 1;
    if !row_is_apex[0] {
        push_cap(
            &mut positions,
            &mut faces,
            row_base[0],
            nv,
            rows[0].1,
            job.cap_fill,
            arc,
            false,
            axis_at(0),
        );
    }
    if !row_is_apex[last] {
        push_cap(
            &mut positions,
            &mut faces,
            row_base[last],
            nv,
            rows[last].1,
            job.cap_fill,
            arc,
            true,
            axis_at(last),
        );
    }

    // Radial walls closing a partial sweep.
    if walled {
        push_arc_walls(&mut faces, &rows, &row_base, &row_is_apex, &axis_vert, nv);
    }

    HalfEdgeMesh::from_polygons(&positions, &faces)
}

/// Cap one end ring. `up` selects the outward direction (+Y for the top ring, -Y for the bottom).
#[allow(clippy::too_many_arguments)]
fn push_cap(
    positions: &mut Vec<Vec3>,
    faces: &mut Vec<Vec<usize>>,
    base: usize,
    nv: usize,
    y: f32,
    fill: CapFill,
    arc: ArcSpec,
    up: bool,
    axis: Option<usize>,
) {
    let full = arc.is_full();
    if !full && arc.close == ArcClose::Open {
        // An open sweep has no enclosed rim to cap.
        return;
    }
    match fill {
        CapFill::None => {}
        CapFill::NGon => {
            let mut ring: Vec<usize> = (base..base + nv).collect();
            if !full {
                // Close the wedge through the SHARED axis vertex.
                ring.push(axis.expect("a walled partial sweep must supply an axis vertex"));
            }
            // Bottom cap (-Y outward) traverses increasing theta; top is the reverse.
            if up {
                ring.reverse();
            }
            faces.push(ring);
        }
        CapFill::TriFan => {
            let c = axis.unwrap_or_else(|| {
                positions.push(Vec3::new(0.0, y, 0.0));
                positions.len() - 1
            });
            let segs = if full { nv } else { nv - 1 };
            for i in 0..segs {
                let n = base + (i + 1) % nv;
                if up {
                    faces.push(vec![c, n, base + i]);
                } else {
                    faces.push(vec![c, base + i, n]);
                }
            }
        }
    }
}

/// The two flat radial faces of a pie slice — one per cut meridian, spanning the full profile.
///
/// Each wall walks UP the profile at its meridian and back DOWN the axis. The start meridian's
/// outward normal points against the sweep direction and the end meridian's points with it, so
/// the two walls are wound oppositely — verified by `walled_partial_sweep_is_closed`.
fn push_arc_walls(
    faces: &mut Vec<Vec<usize>>,
    rows: &[(f32, f32)],
    row_base: &[usize],
    row_is_apex: &[bool],
    axis_vert: &[usize],
    nv: usize,
) {
    let mut build_wall = |edge: usize, reverse: bool| {
        let mut ring: Vec<usize> = Vec::with_capacity(rows.len() * 2);
        // Outer boundary: the profile at this meridian, bottom → top.
        for row in 0..rows.len() {
            ring.push(if row_is_apex[row] {
                row_base[row]
            } else {
                row_base[row] + edge
            });
        }
        // Inner boundary: back down the shared axis. An apex row IS its own axis vertex, so the
        // dedup below collapses the duplicate rather than emitting a zero-length edge.
        for row in (0..rows.len()).rev() {
            ring.push(axis_vert[row]);
        }
        ring.dedup();
        while ring.len() > 1 && ring.first() == ring.last() {
            ring.pop();
        }
        if ring.len() >= 3 {
            if reverse {
                ring.reverse();
            }
            faces.push(ring);
        }
    };
    build_wall(0, true);
    build_wall(nv - 1, false);
}

// ============================================================
// Rounding
// ============================================================

/// Round the primitive's edges via the existing bevel — never a second corner-rounding path.
///
/// `max_extent` is the tightest dimension the radius may not exceed; beyond it the offset faces
/// cross and the mesh self-intersects. Clamping here (the single entry point) is the definition of
/// the valid domain, not a bandaid — the UI slider is bounded to the same range.
fn apply_round(
    mesh: HalfEdgeMesh,
    round: &RoundSpec,
    max_extent: f32,
    which: crate::primitive_round::RoundEdges,
) -> HalfEdgeMesh {
    if round.is_off() {
        return mesh;
    }
    let radius = round.radius.min(max_extent * 0.5 - MIN_DIM).max(0.0);
    if radius <= 1e-6 {
        return mesh;
    }
    let segments = round.segments.clamp(1, RoundSpec::MAX_SEGMENTS);
    crate::primitive_round::round_edges(mesh, which, radius, segments).0
}

/// Positions of every live vertex, in id order. Test helper — `HalfEdgeMesh` has no live-vertex
/// iterator, and out-of-module code cannot read the free lists.
#[cfg(test)]
fn live_positions(m: &HalfEdgeMesh) -> Vec<Vec3> {
    use crate::half_edge::VertexId;
    (0..m.vertices.len() as u32)
        .map(VertexId)
        .filter(|&v| m.is_vert_live(v))
        .map(|v| m.vert(v).position)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_sane(m: &HalfEdgeMesh, what: &str) {
        assert!(m.face_count() > 0, "{what}: no faces");
        assert!(
            m.validate().is_ok(),
            "{what}: validate failed: {:?}",
            m.validate()
        );
    }

    #[test]
    fn every_default_spec_builds_a_valid_mesh() {
        for kind in PrimitiveSpec::ALL_KINDS {
            let spec = PrimitiveSpec::from_kind_id(kind).unwrap();
            let m = spec.build();
            assert_sane(&m, kind);
        }
    }

    /// Every CLOSED primitive must be wound outward — signed volume > 0.
    ///
    /// Two shipped primitives were not: the full torus (all arcs) and a TriFan-capped cone, both
    /// of which route to the hand-written `HalfEdgeMesh::new_torus`/`new_cone` while every other
    /// configuration goes through `revolve`. An inside-out mesh is wrong for face normals,
    /// OBJ/glTF export, boolean operand orientation and mesh→SDF conversion, and it is invisible
    /// in the viewport because shading is double-sided — which is why it survived. Volume is the
    /// cheap total check: it cannot be fooled by a locally-consistent-but-reversed patch.
    #[test]
    fn every_closed_primitive_is_wound_outward() {
        use crate::mesh_boolean::{boundary_half_edge_count, mesh_signed_volume};
        let mut cases: Vec<(String, PrimitiveSpec)> = Vec::new();
        for kind in PrimitiveSpec::ALL_KINDS {
            let spec = PrimitiveSpec::from_kind_id(kind).unwrap();
            cases.push((format!("{kind} (default)"), spec));
        }
        // Cap-fill and arc variants — `build_cone`/`build_torus` route full+TriFan+vseg1 to the
        // hand-written builders and everything else to `revolve`, so both branches must be swept.
        for f in [CapFill::NGon, CapFill::TriFan, CapFill::None] {
            cases.push((
                format!("cone cap={f:?}"),
                PrimitiveSpec::Cone(ConeSpec { cap_fill: f, ..Default::default() }),
            ));
            cases.push((
                format!("cone frustum cap={f:?}"),
                PrimitiveSpec::Cone(ConeSpec { radius_top: 0.5, cap_fill: f, ..Default::default() }),
            ));
            cases.push((
                format!("cylinder cap={f:?}"),
                PrimitiveSpec::Cylinder(CylinderSpec { cap_fill: f, ..Default::default() }),
            ));
        }
        cases.push((
            "cone vseg=3".into(),
            PrimitiveSpec::Cone(ConeSpec { vsegments: 3, ..Default::default() }),
        ));
        cases.push((
            "torus minor_segs=8".into(),
            PrimitiveSpec::Torus(TorusSpec { minor_segs: 8, ..Default::default() }),
        ));
        let mut inverted: Vec<String> = Vec::new();
        for (name, spec) in &cases {
            let m = spec.build();
            let v = mesh_signed_volume(&m);
            // An open primitive (plane, circle, uncapped tube) has no enclosed volume, so the
            // test says nothing about it — skip rather than assert something meaningless.
            if boundary_half_edge_count(&m) > 0 {
                continue;
            }
            eprintln!("{name:32} vol={v:+.5}");
            if v <= 0.0 {
                inverted.push(format!("{name} (vol {v:+.5})"));
            }
        }
        assert!(
            inverted.is_empty(),
            "primitives built INSIDE-OUT: {inverted:?}"
        );
    }

    #[test]
    fn unknown_kind_id_yields_no_spec() {
        assert!(PrimitiveSpec::from_kind_id("teapot").is_none());
    }

    #[test]
    fn kind_id_round_trips() {
        for kind in PrimitiveSpec::ALL_KINDS {
            let spec = PrimitiveSpec::from_kind_id(kind).unwrap();
            assert_eq!(spec.kind_id(), kind);
        }
    }

    // ---- default == legacy (the paths must not diverge) ----

    #[test]
    fn default_box_matches_legacy() {
        let a = build_box(&BoxSpec::default());
        let b = HalfEdgeMesh::new_box(Vec3::ONE);
        assert_eq!(a.vertex_count(), b.vertex_count());
        assert_eq!(a.face_count(), b.face_count());
    }

    #[test]
    fn box_grid_matches_legacy_at_one_segment() {
        // The grid path is a strict generalization: at 1x1x1 it must produce the same SHAPE as the
        // legacy hand-written box (vertex order differs, which is why the legacy path is kept).
        let grid = build_box_grid(Vec3::splat(2.0), 1, 1, 1);
        let legacy = HalfEdgeMesh::new_box(Vec3::ONE);
        assert_eq!(grid.vertex_count(), legacy.vertex_count(), "vertex count");
        assert_eq!(grid.face_count(), legacy.face_count(), "face count");
        assert_sane(&grid, "box grid 1x1x1");

        let mut a: Vec<[i32; 3]> = live_positions(&grid)
            .into_iter()
            .map(|p| {
                [
                    (p.x * 1000.0).round() as i32,
                    (p.y * 1000.0).round() as i32,
                    (p.z * 1000.0).round() as i32,
                ]
            })
            .collect();
        let mut b: Vec<[i32; 3]> = live_positions(&legacy)
            .into_iter()
            .map(|p| {
                [
                    (p.x * 1000.0).round() as i32,
                    (p.y * 1000.0).round() as i32,
                    (p.z * 1000.0).round() as i32,
                ]
            })
            .collect();
        a.sort();
        b.sort();
        assert_eq!(a, b, "same vertex set");
    }

    #[test]
    fn default_cylinder_matches_legacy() {
        let a = build_cylinder(&CylinderSpec::default());
        let b = HalfEdgeMesh::new_cylinder(1.0, 2.0, 32);
        assert_eq!(a.vertex_count(), b.vertex_count());
        assert_eq!(a.face_count(), b.face_count());
    }

    #[test]
    fn default_sphere_and_torus_match_legacy() {
        let s = build_uv_sphere(&UvSphereSpec::default());
        let sl = HalfEdgeMesh::new_uv_sphere(1.0, 32, 16);
        assert_eq!(s.vertex_count(), sl.vertex_count());
        assert_eq!(s.face_count(), sl.face_count());

        let t = build_torus(&TorusSpec::default());
        let tl = HalfEdgeMesh::new_torus(1.0, 0.25, 32, 16);
        assert_eq!(t.vertex_count(), tl.vertex_count());
        assert_eq!(t.face_count(), tl.face_count());
    }

    // ---- segmentation ----

    #[test]
    fn segmented_box_is_valid_and_welded() {
        for (sx, sy, sz) in [(2, 1, 1), (2, 3, 4), (5, 5, 5)] {
            let m = build_box(&BoxSpec {
                seg_x: sx,
                seg_y: sy,
                seg_z: sz,
                ..Default::default()
            });
            assert_sane(&m, &format!("box {sx}x{sy}x{sz}"));
            // Shell of an (sx,sy,sz) lattice: total minus interior.
            let total = (sx + 1) * (sy + 1) * (sz + 1);
            let interior = sx.saturating_sub(1) * sy.saturating_sub(1) * sz.saturating_sub(1);
            assert_eq!(
                m.vertex_count() as u32,
                total - interior,
                "box {sx}x{sy}x{sz} vertex count (welded, no doubles)"
            );
            assert_eq!(
                m.face_count() as u32,
                2 * (sx * sy + sy * sz + sx * sz),
                "box {sx}x{sy}x{sz} face count"
            );
        }
    }

    #[test]
    fn cylinder_vsegments_add_bands() {
        let base = build_cylinder(&CylinderSpec {
            sides: 8,
            vsegments: 1,
            ..Default::default()
        });
        let seg = build_cylinder(&CylinderSpec {
            sides: 8,
            vsegments: 3,
            ..Default::default()
        });
        assert_sane(&seg, "cylinder vseg=3");
        assert_eq!(seg.vertex_count(), base.vertex_count() + 8 * 2);
        assert_eq!(seg.face_count(), base.face_count() + 8 * 2);
    }

    #[test]
    fn cone_top_radius_makes_a_frustum() {
        let cone = build_cone(&ConeSpec {
            sides: 8,
            ..Default::default()
        });
        let frustum = build_cone(&ConeSpec {
            sides: 8,
            radius_top: 0.5,
            ..Default::default()
        });
        assert_sane(&cone, "cone");
        assert_sane(&frustum, "frustum");
        // The apex becomes a full ring plus a cap.
        assert_eq!(frustum.vertex_count(), cone.vertex_count() + 7);
    }

    #[test]
    fn ico_sphere_subdivision_quadruples_faces() {
        let mut prev = 0;
        for sub in 0..=3u32 {
            let m = build_ico_sphere(&IcoSphereSpec {
                radius: 1.0,
                subdivisions: sub,
            });
            assert_sane(&m, &format!("ico {sub}"));
            let expect = 20 * 4usize.pow(sub);
            assert_eq!(m.face_count(), expect, "ico {sub} face count");
            assert!(m.face_count() > prev);
            prev = m.face_count();
            for p in live_positions(&m) {
                let d = p.length();
                assert!((d - 1.0).abs() < 1e-4, "ico {sub}: vertex off the sphere: {d}");
            }
        }
    }

    #[test]
    fn ico_subdivision_is_hard_capped() {
        let m = build_ico_sphere(&IcoSphereSpec {
            radius: 1.0,
            subdivisions: 99,
        });
        assert_eq!(m.face_count(), 20 * 4usize.pow(MAX_ICO_SUBDIV));
    }

    // ---- arc ----

    #[test]
    fn full_sweep_is_identical_to_no_arc() {
        // The regression guard from plan §3.1: a partial-arc code path must not perturb the full
        // case. Same counts AND same vertex positions, in order.
        let arced = build_cylinder(&CylinderSpec {
            sides: 12,
            arc: ArcSpec {
                sweep_deg: 360.0,
                start_deg: 0.0,
                close: ArcClose::Wall,
            },
            ..Default::default()
        });
        let plain = HalfEdgeMesh::new_cylinder(1.0, 2.0, 12);
        assert_eq!(arced.vertex_count(), plain.vertex_count());
        assert_eq!(arced.face_count(), plain.face_count());
        for (a, b) in live_positions(&arced).into_iter().zip(live_positions(&plain)) {
            assert!(
                a.distance(b) < 1e-5,
                "full sweep diverged: {a:?} vs {b:?}"
            );
        }
    }

    #[test]
    fn partial_sweep_is_valid_for_every_revolved_primitive() {
        let arc = ArcSpec {
            sweep_deg: 90.0,
            start_deg: 0.0,
            close: ArcClose::Wall,
        };
        assert_sane(
            &build_cylinder(&CylinderSpec { sides: 8, arc, ..Default::default() }),
            "cylinder 90deg",
        );
        assert_sane(
            &build_cone(&ConeSpec { sides: 8, arc, ..Default::default() }),
            "cone 90deg",
        );
        assert_sane(
            &build_cone(&ConeSpec { sides: 8, radius_top: 0.4, arc, ..Default::default() }),
            "frustum 90deg",
        );
        assert_sane(
            &build_torus(&TorusSpec { major_segs: 8, arc, ..Default::default() }),
            "torus 90deg",
        );
        assert_sane(
            &build_uv_sphere(&UvSphereSpec { segments: 8, rings: 6, arc, ..Default::default() }),
            "sphere 90deg",
        );
        assert_sane(
            &build_circle(&CircleSpec { sides: 8, arc, ..Default::default() }),
            "circle 90deg",
        );
    }

    #[test]
    fn walled_partial_sweep_is_closed() {
        // A pie slice is a solid: every edge must be shared by exactly two faces.
        let m = build_cylinder(&CylinderSpec {
            sides: 8,
            arc: ArcSpec { sweep_deg: 120.0, start_deg: 0.0, close: ArcClose::Wall },
            ..Default::default()
        });
        assert_sane(&m, "walled cylinder");
        assert!(m.is_manifold().is_ok(), "pie slice non-manifold: {:?}", m.is_manifold());
        // `is_manifold` accepts a CLEAN OPEN mesh, so it alone cannot prove solidity — a walled
        // pie slice must additionally have zero boundary loops.
        assert_eq!(
            crate::bevel_op::boundary_loop_count(&m),
            0,
            "walled pie slice must be a closed solid"
        );
    }

    #[test]
    fn open_partial_sweep_has_boundary_but_is_valid() {
        let m = build_cylinder(&CylinderSpec {
            sides: 8,
            cap_fill: CapFill::None,
            arc: ArcSpec { sweep_deg: 120.0, start_deg: 0.0, close: ArcClose::Open },
            ..Default::default()
        });
        assert_sane(&m, "open cylinder shell");
        assert_eq!(m.face_count(), 8, "open shell is just the wall quads");
    }

    #[test]
    fn arc_start_offset_rotates_without_changing_topology() {
        let a = build_cylinder(&CylinderSpec {
            sides: 8,
            arc: ArcSpec { sweep_deg: 90.0, start_deg: 0.0, close: ArcClose::Wall },
            ..Default::default()
        });
        let b = build_cylinder(&CylinderSpec {
            sides: 8,
            arc: ArcSpec { sweep_deg: 90.0, start_deg: 45.0, close: ArcClose::Wall },
            ..Default::default()
        });
        assert_eq!(a.vertex_count(), b.vertex_count());
        assert_eq!(a.face_count(), b.face_count());
    }

    #[test]
    fn sides_counts_swept_segments_so_density_holds() {
        // Blender behavior: dragging the sweep down must not thin the tessellation.
        let quarter = build_cylinder(&CylinderSpec {
            sides: 8,
            cap_fill: CapFill::None,
            arc: ArcSpec { sweep_deg: 90.0, start_deg: 0.0, close: ArcClose::Open },
            ..Default::default()
        });
        assert_eq!(quarter.face_count(), 8, "8 segments across the swept 90deg");
    }

    // ---- clamps ----

    #[test]
    fn degenerate_params_clamp_instead_of_producing_junk() {
        let m = build_cylinder(&CylinderSpec {
            radius: -5.0,
            height: 0.0,
            sides: 0,
            vsegments: 0,
            ..Default::default()
        });
        assert_sane(&m, "degenerate cylinder");

        let t = build_torus(&TorusSpec {
            major_r: 1.0,
            minor_r: 99.0, // would self-intersect through the hub
            ..Default::default()
        });
        assert_sane(&t, "over-fat torus");
        for p in live_positions(&t) {
            assert!(p.is_finite(), "non-finite vertex");
        }

        let b = build_box(&BoxSpec {
            size: Vec3::ZERO,
            ..Default::default()
        });
        assert_sane(&b, "zero box");
    }

    #[test]
    fn huge_segment_counts_are_bounded() {
        let m = build_cylinder(&CylinderSpec {
            sides: 100_000,
            vsegments: 100_000,
            ..Default::default()
        });
        assert!(m.vertex_count() <= (MAX_SIDES as usize + 1) * (MAX_SUBDIV as usize + 2));
    }

    // ---- rounding ----

    #[test]
    fn round_off_is_byte_identical() {
        let plain = build_box(&BoxSpec::default());
        let off = build_box(&BoxSpec {
            round: RoundSpec { radius: 0.0, segments: 4 },
            ..Default::default()
        });
        assert_eq!(plain.vertex_count(), off.vertex_count());
        assert_eq!(plain.face_count(), off.face_count());
        assert_eq!(live_positions(&plain), live_positions(&off));
    }

    #[test]
    fn rounded_box_gains_geometry_and_stays_valid() {
        let plain = build_box(&BoxSpec::default());
        let round = build_box(&BoxSpec {
            round: RoundSpec { radius: 0.2, segments: 3 },
            ..Default::default()
        });
        assert_sane(&round, "rounded box");
        assert!(
            round.face_count() > plain.face_count(),
            "rounding added no geometry ({} -> {})",
            plain.face_count(),
            round.face_count()
        );
    }

    #[test]
    fn round_radius_clamps_below_self_intersection() {
        // Radius far past half the smallest dimension must not fold the mesh inside out.
        let m = build_box(&BoxSpec {
            size: Vec3::new(2.0, 2.0, 2.0),
            round: RoundSpec { radius: 50.0, segments: 3 },
            ..Default::default()
        });
        assert_sane(&m, "over-rounded box");
        for p in live_positions(&m) {
            assert!(
                p.abs().max_element() <= 1.001,
                "rounded vertex escaped the box: {p:?}"
            );
        }
    }

    #[test]
    fn rounded_cylinder_stays_valid() {
        let m = build_cylinder(&CylinderSpec {
            sides: 16,
            round: RoundSpec { radius: 0.15, segments: 2 },
            ..Default::default()
        });
        assert_sane(&m, "rounded cylinder");
    }

    // ---- serde ----

    // ---- drag-editable counts ----

    #[test]
    fn every_seg_field_a_shape_advertises_is_gettable_and_settable() {
        for kind in PrimitiveSpec::ALL_KINDS {
            let mut spec = PrimitiveSpec::from_kind_id(kind).unwrap();
            for &f in spec.seg_fields() {
                assert!(
                    spec.seg_get(f).is_some(),
                    "{kind}: advertises {f:?} but seg_get returns None"
                );
                spec.seg_set(f, 7);
                let got = spec.seg_get(f).unwrap();
                assert!(
                    got > 0,
                    "{kind}: {f:?} set to 7 produced {got}"
                );
                // Whatever the clamp did, the result must still build.
                assert!(spec.build().face_count() > 0, "{kind}: {f:?}=7 built nothing");
            }
        }
    }

    #[test]
    fn seg_for_axis_only_names_fields_the_shape_actually_has() {
        for kind in PrimitiveSpec::ALL_KINDS {
            let spec = PrimitiveSpec::from_kind_id(kind).unwrap();
            for axis in [Axis3::X, Axis3::Y, Axis3::Z] {
                if let Some(f) = spec.seg_for_axis(axis) {
                    assert!(
                        spec.seg_fields().contains(&f),
                        "{kind}: axis {axis:?} maps to {f:?}, which is not in seg_fields()"
                    );
                    assert!(spec.seg_get(f).is_some(), "{kind}: {f:?} not gettable");
                }
            }
        }
    }

    #[test]
    fn a_plane_has_no_thickness_axis() {
        let p = PrimitiveSpec::Plane(PlaneSpec::default());
        assert_eq!(p.seg_for_axis(Axis3::Y), None);
        assert_eq!(p.seg_for_axis(Axis3::X), Some(SegField::SubdivW));
        assert_eq!(p.seg_for_axis(Axis3::Z), Some(SegField::SubdivD));
    }

    #[test]
    fn cylinder_axis_mapping_is_radial_vs_height() {
        let c = PrimitiveSpec::Cylinder(CylinderSpec::default());
        assert_eq!(c.seg_for_axis(Axis3::Y), Some(SegField::Axial));
        assert_eq!(c.seg_for_axis(Axis3::X), Some(SegField::Radial));
        assert_eq!(c.seg_for_axis(Axis3::Z), Some(SegField::Radial));
    }

    #[test]
    fn seg_set_clamps_instead_of_producing_junk() {
        let mut c = PrimitiveSpec::Cylinder(CylinderSpec::default());
        c.seg_set(SegField::Radial, 0);
        assert_eq!(c.seg_get(SegField::Radial), Some(MIN_SIDES));
        c.seg_set(SegField::Radial, 99_999);
        assert_eq!(c.seg_get(SegField::Radial), Some(MAX_SIDES));
        assert!(c.build().validate().is_ok());

        let mut i = PrimitiveSpec::IcoSphere(IcoSphereSpec::default());
        i.seg_set(SegField::IcoSubdiv, 99);
        assert_eq!(i.seg_get(SegField::IcoSubdiv), Some(MAX_ICO_SUBDIV));
    }

    #[test]
    fn seg_set_on_a_foreign_field_is_a_no_op() {
        // A plane has no radial count; setting one must not corrupt the plane.
        let mut p = PrimitiveSpec::Plane(PlaneSpec::default());
        let before = p;
        p.seg_set(SegField::Radial, 64);
        assert_eq!(p, before);
    }

    #[test]
    fn only_box_and_cylinder_expose_rounding() {
        for kind in PrimitiveSpec::ALL_KINDS {
            let spec = PrimitiveSpec::from_kind_id(kind).unwrap();
            let has = spec.round().is_some();
            assert_eq!(
                has,
                matches!(kind, "box" | "cylinder"),
                "{kind}: round() presence is wrong"
            );
            assert_eq!(has, spec.round_max_radius() > 0.0, "{kind}: max radius disagrees");
        }
    }

    #[test]
    fn round_max_radius_matches_what_build_would_clamp_to() {
        // Driving the radius to the advertised max must still yield a valid, non-inverted mesh —
        // otherwise the drag would let the user push past the safe range.
        for spec in [
            PrimitiveSpec::Box(BoxSpec::default()),
            PrimitiveSpec::Cylinder(CylinderSpec { sides: 16, ..Default::default() }),
        ] {
            let mut s = spec;
            let max = s.round_max_radius();
            s.round_mut().unwrap().radius = max;
            s.round_mut().unwrap().segments = 2;
            let m = s.build();
            assert!(m.validate().is_ok(), "{:?} at max round: {:?}", s.label(), m.validate());
            assert_eq!(crate::bevel_op::boundary_loop_count(&m), 0, "must stay closed at max round");
        }
    }

    // ---- MeshSource / the derived-mesh invariant ----

    #[test]
    fn mesh_source_build_pairs_spec_with_its_signature() {
        for kind in PrimitiveSpec::ALL_KINDS {
            let spec = PrimitiveSpec::from_kind_id(kind).unwrap();
            let (mesh, source) = MeshSource::build(spec);
            assert_eq!(
                source.live_spec(&mesh),
                Some(spec),
                "{kind}: freshly built mesh must be recognized as its own spec's output"
            );
        }
    }

    #[test]
    fn an_edited_mesh_stops_being_parametric() {
        // THE structural guard: ~80 sites mutate a body's mesh without telling anyone. None of
        // them has to cooperate — the signature check notices.
        let spec = PrimitiveSpec::Box(BoxSpec::default());
        let (mut mesh, source) = MeshSource::build(spec);
        assert!(source.live_spec(&mesh).is_some());

        // Simulate any tool nudging a vertex.
        use crate::half_edge::VertexId;
        mesh.vert_mut(VertexId(0)).position.x += 0.25;

        assert_eq!(
            source.live_spec(&mesh),
            None,
            "an externally edited mesh must NOT report a live spec — a slider drag would \
             rebuild it and destroy the edit"
        );
        assert!(
            source.claims_primitive(),
            "the stale claim is still readable so the UI can explain itself"
        );
    }

    #[test]
    fn authored_never_reports_a_spec() {
        let mesh = HalfEdgeMesh::new_box(Vec3::ONE);
        assert_eq!(MeshSource::Authored.live_spec(&mesh), None);
        assert!(!MeshSource::Authored.claims_primitive());
    }

    #[test]
    fn rebuilding_from_an_edited_spec_restores_liveness() {
        let (_, _) = MeshSource::build(PrimitiveSpec::Cylinder(CylinderSpec::default()));
        let edited = PrimitiveSpec::Cylinder(CylinderSpec {
            sides: 6,
            ..Default::default()
        });
        let (mesh, source) = MeshSource::build(edited);
        assert_eq!(source.live_spec(&mesh), Some(edited));
        assert_eq!(mesh.face_count(), 6 + 2, "6 sides + 2 caps");
    }

    #[test]
    fn mesh_source_round_trips_through_bincode() {
        let (_, source) = MeshSource::build(PrimitiveSpec::Torus(TorusSpec::default()));
        let bytes = bincode::serialize(&source).unwrap();
        let back: MeshSource = bincode::deserialize(&bytes).unwrap();
        assert_eq!(source, back);
    }

    #[test]
    fn spec_round_trips_through_bincode() {
        for kind in PrimitiveSpec::ALL_KINDS {
            let spec = PrimitiveSpec::from_kind_id(kind).unwrap();
            let bytes = bincode::serialize(&spec).unwrap();
            let back: PrimitiveSpec = bincode::deserialize(&bytes).unwrap();
            assert_eq!(spec, back, "{kind}");
        }
    }
}
