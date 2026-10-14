// core_engine/app/src/poly_selection/mod.rs
// Polygon sub-element picking, selection, hide/unhide, linked selection, conversion.
// Pure relocation of poly_selection.rs (god-files split).

mod pick;
mod paint;
mod convert;
#[cfg(test)]
mod tests_pick_grid; // the screen pick grid must index EVERY projected element (no u16 cliff)

use std::collections::{HashSet, VecDeque};
use eframe::egui;
use glam::Vec3;
use core_math::half_edge::{VertexId, FaceId, NO_EDGE};
use core_math::scene_graph::{NodeId, NodePayload};
use crate::{
    AzraApp, ModelingContext, PolyWorkMode, PolyTool, DirtyFlags,
    ActiveTool, SelectionShape,
    world_to_screen, world_segment_to_screen, dist_point_to_segment,
};

// ============================================================
// Ray-triangle intersection (Möller–Trumbore)
// ============================================================

pub(crate) fn ray_triangle(
    ray_o: Vec3, ray_d: Vec3,
    v0: Vec3, v1: Vec3, v2: Vec3,
) -> Option<f32> {
    let e1 = v1 - v0;
    let e2 = v2 - v0;
    let h = ray_d.cross(e2);
    let a = e1.dot(h);
    if a.abs() < 1e-8 { return None; }
    let f = 1.0 / a;
    let s = ray_o - v0;
    let u = f * s.dot(h);
    if !(0.0..=1.0).contains(&u) { return None; }
    let q = s.cross(e1);
    let v = f * ray_d.dot(q);
    if v < 0.0 || u + v > 1.0 { return None; }
    let t = f * e2.dot(q);
    if t > 1e-5 { Some(t) } else { None }
}

/// Nearest ray/face hit on ONE half-edge mesh, in world space. Returns `(t, face_id)`.
///
/// **Triangulation is `earclip_polygon`, never a fan.** A triangle fan over a CONCAVE n-gon
/// emits triangles OUTSIDE the polygon, so a click in a concave notch (a cylinder cap with an
/// inner boundary, an L-shaped face) reported a hit on a face it never covered — and could then
/// fall through to a wall behind it. The solid raster (`poly_mesh::raster_data_ngon`), the face
/// overlay (`overlay_faces.rs`) and the GPU face-id pick buffer (`poly_select_gpu.rs`) all
/// ear-clip, so this makes the CPU pick path agree with what is drawn.
///
/// `hidden` is the vertex-centric hidden set of THAT mesh (pass an empty set for a body whose
/// hidden state we don't own — hidden state is per-active-body).
pub(crate) fn ray_pick_mesh_face(
    mesh: &core_math::half_edge::HalfEdgeMesh,
    world: glam::Mat4,
    ray_o: Vec3,
    ray_d: Vec3,
    hidden: &HashSet<u32>,
) -> Option<(f32, u32)> {
    let mut best: Option<(f32, u32)> = None;
    for i in 0..mesh.faces.len() as u32 {
        if let Some(t) = ray_face_hit(mesh, world, FaceId(i), ray_o, ray_d, hidden) {
            if best.map_or(true, |(bt, _)| t < bt) { best = Some((t, i)); }
        }
    }
    best
}

/// Ray vs ONE face — the exact per-face test `ray_pick_mesh_face` runs (earclip, never a
/// fan; see that fn's doc for why). Shared with the sculpt grid-DDA pick
/// (`poly/sculpt_pick.rs`, ZB_RESPONSIVE_SCULPT_PLAN PR-4) so the two traversals can
/// never disagree on the math — ONE face test, two visit orders.
pub(crate) fn ray_face_hit(
    mesh: &core_math::half_edge::HalfEdgeMesh,
    world: glam::Mat4,
    fid: FaceId,
    ray_o: Vec3,
    ray_d: Vec3,
    hidden: &HashSet<u32>,
) -> Option<f32> {
    if mesh.face(fid).half_edge == NO_EDGE { return None; }
    let ring = mesh.face_vertices(fid);
    if ring.len() < 3 { return None; }
    if ring.iter().any(|v| hidden.contains(&v.0)) { return None; }
    let local: Vec<Vec3> = ring.iter().map(|&v| mesh.vert(v).position).collect();
    let face_n = mesh.compute_face_normal(fid);
    let mut best: Option<f32> = None;
    for tri in core_math::poly_mesh::earclip_polygon(&local, face_n) {
        let a = world.transform_point3(local[tri[0] as usize]);
        let b = world.transform_point3(local[tri[1] as usize]);
        let c = world.transform_point3(local[tri[2] as usize]);
        if let Some(t) = ray_triangle(ray_o, ray_d, a, b, c) {
            if best.map_or(true, |bt| t < bt) { best = Some(t); }
        }
    }
    best
}

/// Even-odd (ray-crossing) point-in-polygon test in screen space — the Lasso region predicate.
/// `poly` is the freehand drag path (implicitly closed start→end). <3 points ⇒ never inside.
pub(crate) fn point_in_polygon(pt: egui::Pos2, poly: &[egui::Pos2]) -> bool {
    let n = poly.len();
    if n < 3 { return false; }
    let mut inside = false;
    let mut j = n - 1;
    for i in 0..n {
        let pi = poly[i];
        let pj = poly[j];
        if (pi.y > pt.y) != (pj.y > pt.y)
            && pt.x < (pj.x - pi.x) * (pt.y - pi.y) / (pj.y - pi.y) + pi.x
        {
            inside = !inside;
        }
        j = i;
    }
    inside
}

// ============================================================
// Screen-space ray from mouse position
// ============================================================

/// Unproject a screen point into a world-space ray. MUST use the SAME vertical FOV as the GPU camera
/// (`Mat4::perspective_rh(fov_deg)`) and `world_to_screen` — the `tan(fov/2)` factor on the right/up
/// components is the inverse of the projection's `f = 1/tan(fov/2)`. Hardcoding 90° here (the old bug)
/// skewed every raycast pick (face select, knife) away from the cursor whenever the FOV slider ≠ 90°.
pub(crate) fn mouse_to_ray(
    mouse: egui::Pos2, rect: egui::Rect,
    cam_pos: Vec3, cam_orbit: glam::Quat, aspect: f32, ortho_hw: f32, fov_deg: f32,
) -> (Vec3, Vec3) {
    let fwd = (cam_orbit * Vec3::NEG_Z).normalize();
    let right = (cam_orbit * Vec3::X).normalize();
    let up = (cam_orbit * Vec3::Y).normalize();
    let ndc_x = ((mouse.x - rect.min.x) / rect.width()) * 2.0 - 1.0;
    let ndc_y = -(((mouse.y - rect.min.y) / rect.height()) * 2.0 - 1.0);

    if ortho_hw > 0.0 {
        let origin = cam_pos + right * (ndc_x * ortho_hw * aspect) + up * (ndc_y * ortho_hw);
        (origin, fwd)
    } else {
        let tan_half = (fov_deg.to_radians() * 0.5).tan();
        let dir = (fwd + right * ndc_x * aspect * tan_half + up * ndc_y * tan_half).normalize();
        (cam_pos, dir)
    }
}

// ============================================================
// Sub-element picking (with screen-space grid acceleration)
// ============================================================

/// Hash camera + viewport state into a u64 for cache invalidation.
pub(crate) fn pick_cam_key(cam_pos: Vec3, cam_orbit: glam::Quat, rect: egui::Rect, ortho_hw: f32) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    cam_pos.x.to_bits().hash(&mut h);
    cam_pos.y.to_bits().hash(&mut h);
    cam_pos.z.to_bits().hash(&mut h);
    cam_orbit.x.to_bits().hash(&mut h);
    cam_orbit.y.to_bits().hash(&mut h);
    cam_orbit.z.to_bits().hash(&mut h);
    cam_orbit.w.to_bits().hash(&mut h);
    rect.min.x.to_bits().hash(&mut h);
    rect.min.y.to_bits().hash(&mut h);
    rect.max.x.to_bits().hash(&mut h);
    rect.max.y.to_bits().hash(&mut h);
    ortho_hw.to_bits().hash(&mut h);
    h.finish()
}

pub(super) const PICK_GRID_COLS: u32 = 24;
pub(super) const PICK_GRID_ROWS: u32 = 16;

impl AzraApp {
    // ============================================================
    // Modo "implicit full selection" — the ONE shared `_or_all` helper set.
    //
    // Modo runs a tool on the WHOLE mesh when NOTHING is selected. AZRA adopts this, but ONLY for SAFE
    // ops (To Sphere / Poke / Inset / Extrude / Bevel / Circle / Flip Normals) — a SAFE op opts in by
    // reading its target set through the matching `_or_all` below. DESTRUCTIVE / COLLAPSE / PAIR ops
    // (Delete, Dissolve/Ctrl+X, Delete-Only-Faces, Merge, Connect Vertex Path, Bridge) MUST NEVER call
    // these: for them an empty selection stays a clean no-op, or an empty Delete would wipe the mesh and
    // an empty Merge would collapse it to a point. The safety split lives HERE, in one place: an op is
    // safe iff it resolves through this set. Sorted output keeps every op deterministic (HashSet order
    // is random). Freed / dead slots are skipped so "all" means every LIVE element.
    // ============================================================

    /// Borrow the active body's mesh if `nid` is a `PolygonalBody`.
    fn poly_mesh_of(&self, nid: NodeId) -> Option<&core_math::half_edge::HalfEdgeMesh> {
        match self.scene.scene_graph.get(nid).map(|n| &n.payload) {
            Some(NodePayload::PolygonalBody { mesh, .. }) => Some(mesh),
            _ => None,
        }
    }

    /// **SAFE-op only** FACE resolver: selected faces if any, else EVERY live face.
    ///
    /// Use for ops that **add or reshape** geometry and never remove volume (extrude, inset,
    /// poke, flip normals, circle, …). **Do not** call from Ctrl+X / dissolve / delete — those
    /// must require an explicit selection or empty→all would wipe the whole mesh.
    pub(crate) fn poly_faces_or_all(&self, nid: NodeId) -> Vec<u32> {
        if !self.poly_sel.selected_faces.is_empty() {
            let mut v: Vec<u32> = self.poly_sel.selected_faces.iter().copied().collect();
            v.sort_unstable();
            return v;
        }
        let mesh = match self.poly_mesh_of(nid) { Some(m) => m, None => return Vec::new() };
        (0..mesh.faces.len() as u32)
            .filter(|&f| mesh.face(FaceId(f)).half_edge != NO_EDGE)
            .collect()
    }

    /// **SAFE-op only** VERTEX resolver: selected verts if any, else every live vertex.
    /// Do not use for Ctrl+X / dissolve / delete.
    pub(crate) fn poly_verts_or_all(&self, nid: NodeId) -> Vec<u32> {
        if !self.poly_sel.selected_vertices.is_empty() {
            let mut v: Vec<u32> = self.poly_sel.selected_vertices.iter().copied().collect();
            v.sort_unstable();
            return v;
        }
        let mesh = match self.poly_mesh_of(nid) { Some(m) => m, None => return Vec::new() };
        (0..mesh.vertices.len() as u32)
            .filter(|&i| mesh.vert(VertexId(i)).half_edge != NO_EDGE)
            .collect()
    }

    /// **SAFE-op only** EDGE resolver: selected edges if any, else one live half-edge per undirected
    /// edge. Do not use for Ctrl+X / dissolve / delete (would dissolve the whole mesh when empty).
    pub(crate) fn poly_edges_or_all(&self, nid: NodeId) -> Vec<u32> {
        if !self.poly_sel.selected_edges.is_empty() {
            let mut v: Vec<u32> = self.poly_sel.selected_edges.iter().copied().collect();
            v.sort_unstable();
            return v;
        }
        let mesh = match self.poly_mesh_of(nid) { Some(m) => m, None => return Vec::new() };
        mesh.canonical_edges().map(|he| he.0).collect()
    }
}

#[cfg(test)]
mod pick_tests {
    use super::*;
    use core_math::half_edge::HalfEdgeMesh;
    use glam::{Mat4, Vec3};

    /// The OLD triangulation the CPU pick used, kept only to prove the bug it caused.
    fn fan_hit(mesh: &HalfEdgeMesh, world: Mat4, o: Vec3, d: Vec3) -> Option<(f32, u32)> {
        let mut best: Option<(f32, u32)> = None;
        for i in 0..mesh.faces.len() as u32 {
            let fid = FaceId(i);
            if mesh.face(fid).half_edge == NO_EDGE { continue; }
            let ring = mesh.face_vertices(fid);
            if ring.len() < 3 { continue; }
            let p: Vec<Vec3> = ring.iter().map(|&v| world.transform_point3(mesh.vert(v).position)).collect();
            for j in 1..p.len() - 1 {
                if let Some(t) = ray_triangle(o, d, p[0], p[j], p[j + 1]) {
                    if best.map_or(true, |(bt, _)| t < bt) { best = Some((t, i)); }
                }
            }
        }
        best
    }

    /// U / horseshoe n-gon in the z=0 plane, CCW seen from +Z. The notch (1<x<2, 1<y<3)
    /// is OUTSIDE the polygon but INSIDE its convex hull.
    fn horseshoe() -> HalfEdgeMesh {
        let pts = [
            Vec3::new(0.0, 0.0, 0.0), Vec3::new(3.0, 0.0, 0.0), Vec3::new(3.0, 3.0, 0.0),
            Vec3::new(2.0, 3.0, 0.0), Vec3::new(2.0, 1.0, 0.0), Vec3::new(1.0, 1.0, 0.0),
            Vec3::new(1.0, 3.0, 0.0), Vec3::new(0.0, 3.0, 0.0),
        ];
        HalfEdgeMesh::from_polygons(&pts, &[vec![0, 1, 2, 3, 4, 5, 6, 7]])
    }

    fn down_ray(x: f32, y: f32) -> (Vec3, Vec3) {
        (Vec3::new(x, y, 5.0), Vec3::new(0.0, 0.0, -1.0))
    }

    #[test]
    fn concave_face_pick_respects_the_polygon_not_its_hull() {
        let mesh = horseshoe();
        let none = HashSet::new();

        // (a) genuinely inside the U's left prong — must hit.
        let (o, d) = down_ray(0.5, 2.0);
        assert_eq!(ray_pick_mesh_face(&mesh, Mat4::IDENTITY, o, d, &none).map(|(_, f)| f), Some(0));

        // (b) in the NOTCH: outside the polygon, inside its convex hull — must MISS.
        let (o, d) = down_ray(1.5, 1.2);
        assert_eq!(ray_pick_mesh_face(&mesh, Mat4::IDENTITY, o, d, &none), None,
                   "ear-clip must not cover the concave notch");
        // …and this is exactly what the old fan got wrong (regression guard).
        assert!(fan_hit(&mesh, Mat4::IDENTITY, o, d).is_some(),
                "fan triangulation used to (wrongly) hit here — the bug this fix removes");
    }

    #[test]
    fn face_pick_takes_the_globally_nearest_body_not_the_active_one() {
        // Body A: a far WALL at z = -5. Nothing of A sits at z ≈ 0 under the cursor — the
        // ray flies through the shell's hole (the owner's repro geometry, minimised).
        let wall = HalfEdgeMesh::from_polygons(
            &[Vec3::new(-9.0, -9.0, -5.0), Vec3::new(9.0, -9.0, -5.0),
              Vec3::new(9.0, 9.0, -5.0), Vec3::new(-9.0, 9.0, -5.0)],
            &[vec![0, 1, 2, 3]],
        );
        // Body B: the disc sitting IN the hole at z = 0, nearer to the camera.
        let disc = HalfEdgeMesh::from_polygons(
            &[Vec3::new(-1.0, -1.0, 0.0), Vec3::new(1.0, -1.0, 0.0),
              Vec3::new(1.0, 1.0, 0.0), Vec3::new(-1.0, 1.0, 0.0)],
            &[vec![0, 1, 2, 3]],
        );
        let none = HashSet::new();
        let (o, d) = down_ray(0.0, 0.0);

        let a = ray_pick_mesh_face(&wall, Mat4::IDENTITY, o, d, &none);
        let b = ray_pick_mesh_face(&disc, Mat4::IDENTITY, o, d, &none);
        assert!(a.is_some(), "the far wall IS hit — that is why active-body-only picking failed");
        assert!(b.is_some());

        // The scene-wide loop in `poly_hover_pick` keeps the GLOBAL nearest.
        let winner = if b.unwrap().0 < a.unwrap().0 { "disc" } else { "wall" };
        assert_eq!(winner, "disc", "nearer body in the hole must win over the active body's far wall");
    }
}
