//! BSP node, plane/polygon primitives, mesh↔soup conversion, legacy bsp_boolean.
//!
//! Pure relocation of the csg.js solid-BSP path from mesh_boolean.

use glam::Vec3;
use crate::half_edge::{FaceId, HalfEdgeMesh, NO_EDGE};
use super::{
    boundary_half_edge_count, polygons_to_mesh, BoolOp, PLANE_EPS, MAX_BSP_DEPTH, MAX_BSP_POLYS,
};

// ============================================================
// Plane / Polygon
// ============================================================

#[derive(Clone, Debug)]
pub(super) struct Plane {
    pub(super) normal: Vec3,
    pub(super) w: f32,
}

impl Plane {
    pub(super) fn flip(&mut self) {
        self.normal = -self.normal;
        self.w = -self.w;
    }
}

/// Provenance: which input solid a polygon came from. Immutable through the whole BSP —
/// unlike `plane`/`verts`, `invert()` does NOT touch it, so the ORIGINAL A/B outward-normal
/// relationship survives all the csg.js inverts for the coplanar-overlap resolution (M1).
pub(super) const SRC_A: u8 = 0;
pub(super) const SRC_B: u8 = 1;

#[derive(Clone, Debug)]
pub(super) struct Polygon {
    pub(super) verts: Vec<Vec3>,
    pub(super) plane: Plane,
    /// SRC_A or SRC_B — set once when the input mesh is converted, inherited by split children.
    pub(super) source: u8,
    /// The source solid's OUTWARD normal at this face, captured at creation and NEVER flipped by
    /// `invert()`. The csg.js difference/intersect flips B's polygons, so the *stored* `plane.normal`
    /// no longer reflects original orientation; `orig_normal` does. Used only to classify coplanar
    /// A/B overlaps (drop-both vs keep-one) — see `resolve_coincident_faces`.
    pub(super) orig_normal: Vec3,
}

impl Polygon {
    /// Build a polygon and its plane (Newell's method — area-weighted, sign-correct for
    /// slightly non-planar rings; equals the cross product for triangles).
    /// Returns None for degenerate (near-zero-area) rings. Source defaults to SRC_A;
    /// callers that know the provenance set it via `with_source` / inherit from a parent.
    pub(super) fn new(verts: Vec<Vec3>) -> Option<Self> {
        if verts.len() < 3 {
            return None;
        }
        let mut n = Vec3::ZERO;
        let mut centroid = Vec3::ZERO;
        for i in 0..verts.len() {
            let a = verts[i];
            let b = verts[(i + 1) % verts.len()];
            n += Vec3::new(
                (a.y - b.y) * (a.z + b.z),
                (a.z - b.z) * (a.x + b.x),
                (a.x - b.x) * (a.y + b.y),
            );
            centroid += a;
        }
        let normal = n.normalize_or_zero();
        if normal == Vec3::ZERO {
            return None;
        }
        centroid /= verts.len() as f32;
        let w = normal.dot(centroid);
        Some(Self { verts, plane: Plane { normal, w }, source: SRC_A, orig_normal: normal })
    }

    /// Stamp provenance (source solid + its immutable outward normal). Called once per input face.
    pub(super) fn with_source(mut self, source: u8) -> Self {
        self.source = source;
        self.orig_normal = self.plane.normal; // input meshes are outward-wound → plane normal IS outward
        self
    }

    /// A split child: inherit the parent's provenance (same source solid, same original orientation —
    /// a coplanar fragment of the parent face).
    pub(super) fn inherit(mut self, parent: &Polygon) -> Self {
        self.source = parent.source;
        self.orig_normal = parent.orig_normal;
        self
    }

    pub(super) fn flip(&mut self) {
        self.verts.reverse();
        self.plane.flip();
        // source + orig_normal are provenance — deliberately NOT flipped.
    }

    /// Translate the polygon by `t` (verts + plane). Used for the local-frame BSP: translation only
    /// shifts a plane's `w` by `normal·t` (the normal is unchanged), so this is exact re: the plane.
    pub(super) fn translated(&self, t: Vec3) -> Polygon {
        Polygon {
            verts: self.verts.iter().map(|v| *v + t).collect(),
            plane: Plane { normal: self.plane.normal, w: self.plane.w + self.plane.normal.dot(t) },
            source: self.source,
            orig_normal: self.orig_normal,
        }
    }
}

// Vertex classification bits (csg.js).
pub(super) const COPLANAR: u8 = 0;
pub(super) const FRONT: u8 = 1;
pub(super) const BACK: u8 = 2;
pub(super) const SPANNING: u8 = 3;

/// Split `poly` by `plane` into the four output lists (csg.js `Plane.splitPolygon`).
/// Coplanar polygons go front/back by normal agreement (standard BSP coplanar assignment).
pub(super) fn split_polygon(
    plane: &Plane,
    poly: Polygon,
    coplanar_front: &mut Vec<Polygon>,
    coplanar_back: &mut Vec<Polygon>,
    front: &mut Vec<Polygon>,
    back: &mut Vec<Polygon>,
) {
    let mut polygon_type: u8 = 0;
    let mut types: Vec<u8> = Vec::with_capacity(poly.verts.len());
    for v in &poly.verts {
        let t = plane.normal.dot(*v) - plane.w;
        let ty = if t < -PLANE_EPS {
            BACK
        } else if t > PLANE_EPS {
            FRONT
        } else {
            COPLANAR
        };
        polygon_type |= ty;
        types.push(ty);
    }

    match polygon_type {
        COPLANAR => {
            if plane.normal.dot(poly.plane.normal) > 0.0 {
                coplanar_front.push(poly);
            } else {
                coplanar_back.push(poly);
            }
        }
        FRONT => front.push(poly),
        BACK => back.push(poly),
        _ => {
            let n = poly.verts.len();
            let mut f: Vec<Vec3> = Vec::with_capacity(n + 1);
            let mut b: Vec<Vec3> = Vec::with_capacity(n + 1);
            for i in 0..n {
                let j = (i + 1) % n;
                let ti = types[i];
                let tj = types[j];
                let vi = poly.verts[i];
                let vj = poly.verts[j];
                if ti != BACK {
                    f.push(vi);
                }
                if ti != FRONT {
                    b.push(vi);
                }
                if (ti | tj) == SPANNING {
                    let denom = plane.normal.dot(vj - vi);
                    if denom.abs() > f32::MIN_POSITIVE {
                        let t = (plane.w - plane.normal.dot(vi)) / denom;
                        let v = vi.lerp(vj, t.clamp(0.0, 1.0));
                        f.push(v);
                        b.push(v);
                    }
                }
            }
            if let Some(p) = Polygon::new(f) {
                front.push(p.inherit(&poly));
            }
            if let Some(p) = Polygon::new(b) {
                back.push(p.inherit(&poly));
            }
        }
    }
}

// ============================================================
// BSP node (csg.js `Node`)
// ============================================================

pub(super) struct BspNode {
    pub(super) plane: Option<Plane>,
    pub(super) front: Option<Box<BspNode>>,
    pub(super) back: Option<Box<BspNode>>,
    pub(super) polygons: Vec<Polygon>,
}

impl BspNode {
    pub(super) fn new(polygons: Vec<Polygon>) -> Self {
        let mut node = Self { plane: None, front: None, back: None, polygons: Vec::new() };
        if !polygons.is_empty() {
            node.build(polygons, 0);
        }
        node
    }

    /// Convert solid space to empty space and vice versa.
    pub(super) fn invert(&mut self) {
        for p in &mut self.polygons {
            p.flip();
        }
        if let Some(plane) = &mut self.plane {
            plane.flip();
        }
        if let Some(f) = &mut self.front {
            f.invert();
        }
        if let Some(b) = &mut self.back {
            b.invert();
        }
        std::mem::swap(&mut self.front, &mut self.back);
    }

    /// Return `polygons` with everything INSIDE this BSP solid removed.
    pub(super) fn clip_polygons(&self, polygons: Vec<Polygon>) -> Vec<Polygon> {
        let plane = match &self.plane {
            Some(p) => p,
            None => return polygons,
        };
        let mut front: Vec<Polygon> = Vec::new();
        let mut back: Vec<Polygon> = Vec::new();
        for poly in polygons {
            // Coplanar-front joins the front set, coplanar-back the back set.
            let mut cf: Vec<Polygon> = Vec::new();
            let mut cb: Vec<Polygon> = Vec::new();
            split_polygon(plane, poly, &mut cf, &mut cb, &mut front, &mut back);
            front.append(&mut cf);
            back.append(&mut cb);
        }
        let mut front = match &self.front {
            Some(f) => f.clip_polygons(front),
            None => front,
        };
        let back = match &self.back {
            Some(b) => b.clip_polygons(back),
            None => Vec::new(), // no back subtree = solid interior → dropped
        };
        front.extend(back);
        front
    }

    /// Remove all polygons of `self` that are inside the `bsp` solid.
    pub(super) fn clip_to(&mut self, bsp: &BspNode) {
        self.polygons = bsp.clip_polygons(std::mem::take(&mut self.polygons));
        if let Some(f) = &mut self.front {
            f.clip_to(bsp);
        }
        if let Some(b) = &mut self.back {
            b.clip_to(bsp);
        }
    }

    pub(super) fn all_polygons(&self) -> Vec<Polygon> {
        let mut out = self.polygons.clone();
        if let Some(f) = &self.front {
            out.extend(f.all_polygons());
        }
        if let Some(b) = &self.back {
            out.extend(b.all_polygons());
        }
        out
    }

    /// Insert polygons into the tree (splitter = first polygon's plane, csg.js style).
    pub(super) fn build(&mut self, polygons: Vec<Polygon>, depth: usize) {
        if polygons.is_empty() {
            return;
        }
        // Depth backstop (audit #12): on a degenerate (linear) tree, stop splitting and keep the tail at
        // this node rather than recursing once per polygon into a stack overflow. Never hit by real meshes.
        if depth >= MAX_BSP_DEPTH {
            if self.plane.is_none() {
                self.plane = Some(polygons[0].plane.clone());
            }
            self.polygons.extend(polygons);
            return;
        }
        if self.plane.is_none() {
            self.plane = Some(polygons[0].plane.clone());
        }
        let plane = self.plane.clone().expect("plane set above");
        let mut front: Vec<Polygon> = Vec::new();
        let mut back: Vec<Polygon> = Vec::new();
        for poly in polygons {
            // Both coplanar lists accumulate on THIS node.
            let mut cf: Vec<Polygon> = Vec::new();
            let mut cb: Vec<Polygon> = Vec::new();
            split_polygon(&plane, poly, &mut cf, &mut cb, &mut front, &mut back);
            self.polygons.append(&mut cf);
            self.polygons.append(&mut cb);
        }
        if !front.is_empty() {
            self.front.get_or_insert_with(|| Box::new(Self { plane: None, front: None, back: None, polygons: Vec::new() })).build(front, depth + 1);
        }
        if !back.is_empty() {
            self.back.get_or_insert_with(|| Box::new(Self { plane: None, front: None, back: None, polygons: Vec::new() })).build(back, depth + 1);
        }
    }
}

// ============================================================
// HalfEdgeMesh → polygon soup
// ============================================================

/// Fan-triangulate every live face into planar `Polygon`s.
/// Returns None if the mesh is open (boundary half-edges) or yields no polygons —
/// solid CSG is only defined on closed surfaces.
pub(super) fn mesh_to_polygons(mesh: &HalfEdgeMesh) -> Option<Vec<Polygon>> {
    if boundary_half_edge_count(mesh) > 0 {
        return None;
    }
    let mut polys: Vec<Polygon> = Vec::new();
    for i in 0..mesh.faces.len() {
        let fid = FaceId(i as u32);
        if mesh.face(fid).half_edge == NO_EDGE {
            continue; // freed slot
        }
        let verts = mesh.face_vertices(fid);
        if verts.len() < 3 {
            continue;
        }
        let pts: Vec<Vec3> = verts.iter().map(|&v| mesh.vert(v).position).collect();
        // EAR-CLIP triangulation (not naive fan). A prior boolean/extrude leaves NON-CONVEX faces
        // (e.g. the L-shaped hexagon of a corner-bitten cube); fanning one from vertex 0 folds some
        // triangles BACK-WOUND (normal flips), and those become spurious flipped fragments that the
        // robust classifier keeps → a same-winding non-manifold (the sequential-cut failure). Ear
        // clipping keeps every triangle correctly wound and convex (safe for split_polygon + winding).
        let mut nrm = Vec3::ZERO;
        for k in 0..pts.len() {
            let a = pts[k];
            let b = pts[(k + 1) % pts.len()];
            nrm += Vec3::new((a.y - b.y) * (a.z + b.z), (a.z - b.z) * (a.x + b.x), (a.x - b.x) * (a.y + b.y));
        }
        let nrm = nrm.normalize_or_zero();
        if nrm == Vec3::ZERO {
            continue;
        }
        for t in crate::poly_mesh::earclip_polygon(&pts, nrm) {
            if let Some(p) = Polygon::new(vec![pts[t[0] as usize], pts[t[1] as usize], pts[t[2] as usize]]) {
                polys.push(p);
            }
        }
    }
    if polys.is_empty() { None } else { Some(polys) }
}


/// Legacy csg.js solid-BSP boolean — see `robust_boolean` for why it is only a fallback now.
pub(super) fn bsp_boolean(a: &HalfEdgeMesh, b: &HalfEdgeMesh, op: BoolOp) -> Option<HalfEdgeMesh> {
    // LOCAL FRAME (audit #7): the BSP classifies with a fixed WORLD epsilon (PLANE_EPS=1e-5), but f32
    // resolution at coordinate magnitude C is ~C*1.2e-7 — beyond ~C=100 an on-plane point is classified
    // randomly FRONT/BACK and the SAME two solids that boolean cleanly at the origin crack far away.
    // Fix: run the whole BSP centered on the combined AABB (small coords → the epsilon is meaningful),
    // then shift the result back. Booleans are translation-invariant (matches Blender).
    let (amin, amax) = a.bounding_box();
    let (bmin, bmax) = b.bounding_box();
    let origin = (amin.min(bmin) + amax.max(bmax)) * 0.5;
    let shift = -origin;

    let pa: Vec<Polygon> = mesh_to_polygons(a)?.iter().map(|p| p.translated(shift).with_source(SRC_A)).collect();
    let pb: Vec<Polygon> = mesh_to_polygons(b)?.iter().map(|p| p.translated(shift).with_source(SRC_B)).collect();

    // Refuse absurd inputs cleanly rather than an O(n²) BSP blowup / stack overflow (audit #12).
    if pa.len() + pb.len() > MAX_BSP_POLYS {
        return None;
    }

    let mut na = BspNode::new(pa);
    let mut nb = BspNode::new(pb);

    match op {
        BoolOp::Union => {
            na.clip_to(&nb);
            nb.clip_to(&na);
            nb.invert();
            nb.clip_to(&na);
            nb.invert();
            na.build(nb.all_polygons(), 0);
        }
        BoolOp::Difference => {
            na.invert();
            na.clip_to(&nb);
            nb.clip_to(&na);
            nb.invert();
            nb.clip_to(&na);
            nb.invert();
            na.build(nb.all_polygons(), 0);
            na.invert();
        }
        BoolOp::Intersect => {
            na.invert();
            nb.clip_to(&na);
            nb.invert();
            na.clip_to(&nb);
            nb.clip_to(&na);
            na.build(nb.all_polygons(), 0);
            na.invert();
        }
    }

    let polys = na.all_polygons();
    if polys.is_empty() {
        return None;
    }
    let mut mesh = polygons_to_mesh(&polys, op)?;
    if mesh.face_count() == 0 {
        return None;
    }
    // Shift the local-frame result back to world space (translation leaves normals unchanged).
    for v in mesh.vertices.iter_mut() {
        v.position += origin;
    }
    Some(mesh)
}

