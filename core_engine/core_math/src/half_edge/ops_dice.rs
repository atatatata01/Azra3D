//! Face dice - even horizontal + vertical cuts on selected faces (grid subdivide).
//!
//! Rebuilds the whole mesh via `from_polygons_keeping_ids` (same cost class as
//! `subdivide_faces`). Neighbours of diced faces become n-gons where shared edges
//! pick up the new edge splits. Every vertex / edge / face the dice does not consume keeps
//! its id (POLY-STALE-IDS, 2026-10-13).

use super::*;
use glam::Vec3;
use std::collections::{HashMap, HashSet};

impl HalfEdgeMesh {
    /// Dice selected faces into a regular `segs_u x segs_v` grid of cells.
    ///
    /// * `segs_u` / `segs_v` - number of **segments** along the face U/V axes (min 1).
    /// * `rotation` - radians about the face normal (R key in the tool).
    ///
    /// U is taken from the longest edge of the face, then rotated in-plane.
    /// Quads use bilinear mapping. Other n-gons use the face UV AABB (OBB via rotation)
    /// and keep only cells whose center lies inside the face polygon.
    pub fn dice_faces(
        &mut self,
        face_ids: &HashSet<u32>,
        segs_u: u32,
        segs_v: u32,
        rotation: f32,
    ) {
        if face_ids.is_empty() {
            return;
        }
        let su = segs_u.clamp(1, 64) as usize;
        let sv = segs_v.clamp(1, 64) as usize;

        let mut positions: Vec<Vec3> = self.vertices.iter().map(|v| v.position).collect();
        let active_faces: Vec<FaceId> = (0..self.faces.len() as u32)
            .filter(|i| !self.free_faces.contains(i))
            .map(FaceId)
            .collect();

        let edge_key = |a: u32, b: u32| if a < b { (a, b) } else { (b, a) };
        let mut edge_splits: HashMap<(u32, u32), Vec<(f32, usize)>> = HashMap::new();

        let mut ensure_edge_points =
            |positions: &mut Vec<Vec3>, va: u32, vb: u32, n_seg: usize, from_a_to_b: bool| {
                if n_seg <= 1 {
                    return;
                }
                let key = edge_key(va, vb);
                let entry = edge_splits.entry(key).or_default();
                let a_is_min = va < vb;
                for k in 1..n_seg {
                    let t_along_ab = k as f32 / n_seg as f32;
                    let t_min_max = if a_is_min == from_a_to_b {
                        t_along_ab
                    } else {
                        1.0 - t_along_ab
                    };
                    if entry.iter().any(|(t, _)| (*t - t_min_max).abs() < 1e-5) {
                        continue;
                    }
                    let pa = positions[va as usize];
                    let pb = positions[vb as usize];
                    let p = pa.lerp(pb, t_along_ab);
                    let idx = positions.len();
                    positions.push(p);
                    entry.push((t_min_max, idx));
                }
                entry.sort_by(|a, b| {
                    a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal)
                });
            };

        for &fid in &active_faces {
            if !face_ids.contains(&fid.0) {
                continue;
            }
            let verts = self.face_vertices(fid);
            let n = verts.len();
            if n < 3 {
                continue;
            }
            let (axis_u, axis_v, _origin) = face_uv_basis(self, fid, rotation);
            for i in 0..n {
                let va = verts[i].0;
                let vb = verts[(i + 1) % n].0;
                let d = positions[vb as usize] - positions[va as usize];
                let du = d.dot(axis_u).abs();
                let dv = d.dot(axis_v).abs();
                if du >= dv {
                    ensure_edge_points(&mut positions, va, vb, su, true);
                } else {
                    ensure_edge_points(&mut positions, va, vb, sv, true);
                }
            }
        }

        let mut new_face_lists: Vec<Vec<usize>> = Vec::new();
        let mut new_face_mats: Vec<u32> = Vec::new();
        // Index-parallel to `new_face_lists`: the face each output polygon descends from. A
        // diced face's cells all name it — the first keeps its id, the rest are new.
        // Filled lazily: each iteration first stamps the polygons the PREVIOUS face pushed
        // (the per-face branches below `continue` from several places).
        let mut new_face_parent: Vec<Option<FaceId>> = Vec::new();
        let mut parent: Option<FaceId> = None;

        for &fid in &active_faces {
            new_face_parent.resize(new_face_lists.len(), parent);
            parent = Some(fid);
            let mat = self.face(fid).material_id;
            let verts = self.face_vertices(fid);
            let vert_idx: Vec<usize> = verts.iter().map(|v| v.0 as usize).collect();
            let n = vert_idx.len();

            if !face_ids.contains(&fid.0) {
                let mut ring: Vec<usize> = Vec::with_capacity(n + 8);
                for i in 0..n {
                    let va = verts[i].0;
                    let vb = verts[(i + 1) % n].0;
                    ring.push(vert_idx[i]);
                    let key = edge_key(va, vb);
                    if let Some(splits) = edge_splits.get(&key) {
                        if va < vb {
                            for &(_, idx) in splits {
                                ring.push(idx);
                            }
                        } else {
                            for &(_, idx) in splits.iter().rev() {
                                ring.push(idx);
                            }
                        }
                    }
                }
                new_face_lists.push(ring);
                new_face_mats.push(mat);
                continue;
            }

            if n < 3 {
                new_face_lists.push(vert_idx);
                new_face_mats.push(mat);
                continue;
            }

            let (axis_u, axis_v, origin) = face_uv_basis(self, fid, rotation);
            let uv: Vec<(f32, f32)> = vert_idx
                .iter()
                .map(|&vi| {
                    let p = positions[vi] - origin;
                    (p.dot(axis_u), p.dot(axis_v))
                })
                .collect();

            if n == 4 {
                dice_quad_bilinear(
                    &mut positions,
                    &mut new_face_lists,
                    &mut new_face_mats,
                    &vert_idx,
                    su,
                    sv,
                    mat,
                );
            } else {
                dice_ngon_uv_grid(
                    &mut positions,
                    &mut new_face_lists,
                    &mut new_face_mats,
                    &vert_idx,
                    &uv,
                    origin,
                    axis_u,
                    axis_v,
                    su,
                    sv,
                    mat,
                );
            }
        }

        // Soup index = vertex id (positions started as `self.vertices`), new cut points appended.
        new_face_parent.resize(new_face_lists.len(), parent);
        let pos_origin = self.id_indexed_origin(positions.len());
        *self = Self::from_polygons_keeping_ids(
            self,
            &positions,
            &pos_origin,
            &new_face_lists,
            &new_face_parent,
            &new_face_mats,
        );
        #[cfg(debug_assertions)]
        self.validate_mesh("dice_faces");
    }

    /// World-space line segments for the dice grid preview (before commit).
    pub fn dice_preview_lines(
        &self,
        face_ids: &HashSet<u32>,
        segs_u: u32,
        segs_v: u32,
        rotation: f32,
    ) -> Vec<(Vec3, Vec3)> {
        let su = segs_u.clamp(1, 64) as usize;
        let sv = segs_v.clamp(1, 64) as usize;
        let mut lines = Vec::new();
        for &fid_u in face_ids {
            let fid = FaceId(fid_u);
            if !self.is_face_live(fid) {
                continue;
            }
            let verts = self.face_vertices(fid);
            let n = verts.len();
            if n < 3 {
                continue;
            }
            let (axis_u, axis_v, origin) = face_uv_basis(self, fid, rotation);
            if n == 4 {
                let pa = self.vert(verts[0]).position;
                let pb = self.vert(verts[1]).position;
                let pc = self.vert(verts[2]).position;
                let pd = self.vert(verts[3]).position;
                let sample = |i: usize, j: usize| -> Vec3 {
                    let u = i as f32 / su as f32;
                    let v = j as f32 / sv as f32;
                    pa * ((1.0 - u) * (1.0 - v))
                        + pb * (u * (1.0 - v))
                        + pc * (u * v)
                        + pd * ((1.0 - u) * v)
                };
                for j in 0..=sv {
                    for i in 0..su {
                        lines.push((sample(i, j), sample(i + 1, j)));
                    }
                }
                for i in 0..=su {
                    for j in 0..sv {
                        lines.push((sample(i, j), sample(i, j + 1)));
                    }
                }
            } else {
                let uv: Vec<(f32, f32)> = verts
                    .iter()
                    .map(|&v| {
                        let p = self.vert(v).position - origin;
                        (p.dot(axis_u), p.dot(axis_v))
                    })
                    .collect();
                let mut umin = f32::INFINITY;
                let mut umax = f32::NEG_INFINITY;
                let mut vmin = f32::INFINITY;
                let mut vmax = f32::NEG_INFINITY;
                for &(u, v) in &uv {
                    umin = umin.min(u);
                    umax = umax.max(u);
                    vmin = vmin.min(v);
                    vmax = vmax.max(v);
                }
                if !umin.is_finite() || (umax - umin).abs() < 1e-8 || (vmax - vmin).abs() < 1e-8 {
                    continue;
                }
                let at = |i: usize, j: usize| -> Vec3 {
                    let u = umin + (umax - umin) * (i as f32 / su as f32);
                    let v = vmin + (vmax - vmin) * (j as f32 / sv as f32);
                    origin + axis_u * u + axis_v * v
                };
                for j in 0..=sv {
                    for i in 0..su {
                        let a = at(i, j);
                        let b = at(i + 1, j);
                        let mid = (a + b) * 0.5;
                        let (mu, mv) = ((mid - origin).dot(axis_u), (mid - origin).dot(axis_v));
                        if point_in_poly_2d((mu, mv), &uv) {
                            lines.push((a, b));
                        }
                    }
                }
                for i in 0..=su {
                    for j in 0..sv {
                        let a = at(i, j);
                        let b = at(i, j + 1);
                        let mid = (a + b) * 0.5;
                        let (mu, mv) = ((mid - origin).dot(axis_u), (mid - origin).dot(axis_v));
                        if point_in_poly_2d((mu, mv), &uv) {
                            lines.push((a, b));
                        }
                    }
                }
            }
        }
        lines
    }
}

fn face_uv_basis(mesh: &HalfEdgeMesh, fid: FaceId, rotation: f32) -> (Vec3, Vec3, Vec3) {
    let verts = mesh.face_vertices(fid);
    let n = verts.len();
    let mut origin = Vec3::ZERO;
    for &v in &verts {
        origin += mesh.vert(v).position;
    }
    origin /= n as f32;

    let normal = mesh.compute_face_normal(fid).normalize_or_zero();
    let mut best_u = Vec3::X;
    let mut best_len = 0.0f32;
    for i in 0..n {
        let a = mesh.vert(verts[i]).position;
        let b = mesh.vert(verts[(i + 1) % n]).position;
        let d = b - a;
        let len = d.length();
        if len > best_len {
            best_len = len;
            best_u = d;
        }
    }
    let mut axis_u = best_u.normalize_or_zero();
    if axis_u.length_squared() < 1e-12 {
        axis_u = if normal.x.abs() < 0.9 {
            normal.cross(Vec3::X).normalize_or_zero()
        } else {
            normal.cross(Vec3::Y).normalize_or_zero()
        };
    }
    let c = rotation.cos();
    let s = rotation.sin();
    let axis_u = axis_u * c + normal.cross(axis_u) * s + normal * normal.dot(axis_u) * (1.0 - c);
    let axis_u = axis_u.normalize_or_zero();
    let axis_v = normal.cross(axis_u).normalize_or_zero();
    (axis_u, axis_v, origin)
}

fn dice_quad_bilinear(
    positions: &mut Vec<Vec3>,
    faces: &mut Vec<Vec<usize>>,
    mats: &mut Vec<u32>,
    corner: &[usize],
    su: usize,
    sv: usize,
    mat: u32,
) {
    let a = corner[0];
    let b = corner[1];
    let c = corner[2];
    let d = corner[3];
    let pa = positions[a];
    let pb = positions[b];
    let pc = positions[c];
    let pd = positions[d];
    let mut grid: Vec<usize> = Vec::with_capacity((su + 1) * (sv + 1));
    for j in 0..=sv {
        let v = j as f32 / sv as f32;
        for i in 0..=su {
            let u = i as f32 / su as f32;
            let idx = if i == 0 && j == 0 {
                a
            } else if i == su && j == 0 {
                b
            } else if i == su && j == sv {
                c
            } else if i == 0 && j == sv {
                d
            } else {
                let p = pa * ((1.0 - u) * (1.0 - v))
                    + pb * (u * (1.0 - v))
                    + pc * (u * v)
                    + pd * ((1.0 - u) * v);
                let ix = positions.len();
                positions.push(p);
                ix
            };
            grid.push(idx);
        }
    }
    let row = su + 1;
    for j in 0..sv {
        for i in 0..su {
            let i00 = grid[j * row + i];
            let i10 = grid[j * row + i + 1];
            let i11 = grid[(j + 1) * row + i + 1];
            let i01 = grid[(j + 1) * row + i];
            faces.push(vec![i00, i10, i11, i01]);
            mats.push(mat);
        }
    }
}

fn point_in_poly_2d(p: (f32, f32), poly: &[(f32, f32)]) -> bool {
    let (x, y) = p;
    let n = poly.len();
    let mut inside = false;
    let mut j = n - 1;
    for i in 0..n {
        let (xi, yi) = poly[i];
        let (xj, yj) = poly[j];
        if ((yi > y) != (yj > y)) && (x < (xj - xi) * (y - yi) / (yj - yi + 1e-30) + xi) {
            inside = !inside;
        }
        j = i;
    }
    inside
}

fn dice_ngon_uv_grid(
    positions: &mut Vec<Vec3>,
    faces: &mut Vec<Vec<usize>>,
    mats: &mut Vec<u32>,
    vert_idx: &[usize],
    uv: &[(f32, f32)],
    origin: Vec3,
    axis_u: Vec3,
    axis_v: Vec3,
    su: usize,
    sv: usize,
    mat: u32,
) {
    let mut umin = f32::INFINITY;
    let mut umax = f32::NEG_INFINITY;
    let mut vmin = f32::INFINITY;
    let mut vmax = f32::NEG_INFINITY;
    for &(u, v) in uv {
        umin = umin.min(u);
        umax = umax.max(u);
        vmin = vmin.min(v);
        vmax = vmax.max(v);
    }
    if !umin.is_finite() || (umax - umin).abs() < 1e-8 || (vmax - vmin).abs() < 1e-8 {
        faces.push(vert_idx.to_vec());
        mats.push(mat);
        return;
    }
    let pad_u = (umax - umin) * 1e-4;
    let pad_v = (vmax - vmin) * 1e-4;
    umin += pad_u;
    umax -= pad_u;
    vmin += pad_v;
    vmax -= pad_v;

    let mut grid: Vec<Option<usize>> = vec![None; (su + 1) * (sv + 1)];
    let row = su + 1;
    for j in 0..=sv {
        let tv = j as f32 / sv as f32;
        let vv = vmin + (vmax - vmin) * tv;
        for i in 0..=su {
            let tu = i as f32 / su as f32;
            let uu = umin + (umax - umin) * tu;
            let mut snapped = None;
            for (k, &(pu, pv)) in uv.iter().enumerate() {
                if (pu - uu).abs() < 1e-4 && (pv - vv).abs() < 1e-4 {
                    snapped = Some(vert_idx[k]);
                    break;
                }
            }
            let idx = if let Some(s) = snapped {
                s
            } else {
                let p = origin + axis_u * uu + axis_v * vv;
                let ix = positions.len();
                positions.push(p);
                ix
            };
            grid[j * row + i] = Some(idx);
        }
    }

    let mut any = false;
    for j in 0..sv {
        for i in 0..su {
            let cu = umin + (umax - umin) * ((i as f32 + 0.5) / su as f32);
            let cv = vmin + (vmax - vmin) * ((j as f32 + 0.5) / sv as f32);
            if !point_in_poly_2d((cu, cv), uv) {
                continue;
            }
            let i00 = grid[j * row + i].unwrap();
            let i10 = grid[j * row + i + 1].unwrap();
            let i11 = grid[(j + 1) * row + i + 1].unwrap();
            let i01 = grid[(j + 1) * row + i].unwrap();
            faces.push(vec![i00, i10, i11, i01]);
            mats.push(mat);
            any = true;
        }
    }
    if !any {
        faces.push(vert_idx.to_vec());
        mats.push(mat);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use glam::Vec3;

    fn unit_quad() -> HalfEdgeMesh {
        let pos = vec![
            Vec3::new(0.0, 0.0, 0.0),
            Vec3::new(1.0, 0.0, 0.0),
            Vec3::new(1.0, 1.0, 0.0),
            Vec3::new(0.0, 1.0, 0.0),
        ];
        let faces = vec![vec![0usize, 1, 2, 3]];
        HalfEdgeMesh::from_polygons(&pos, &faces)
    }

    #[test]
    fn dice_quad_2x3_produces_six_faces() {
        let mut m = unit_quad();
        let mut sel = HashSet::new();
        sel.insert(0);
        m.dice_faces(&sel, 3, 2, 0.0);
        assert_eq!(m.face_count(), 6, "3x2 segments -> 6 cells");
        assert!(m.vertex_count() >= 12);
    }

    #[test]
    fn dice_1x1_is_noop_cell_count() {
        let mut m = unit_quad();
        let mut sel = HashSet::new();
        sel.insert(0);
        m.dice_faces(&sel, 1, 1, 0.0);
        assert_eq!(m.face_count(), 1);
    }

    /// POLY-STALE-IDS: an id held outside the mesh (selection, hidden set, hover, sidecar) for
    /// geometry the dice did not touch names the SAME geometry afterwards. The mesh is sparse (a
    /// deleted corner) and the diced face comes first, so the old compacting rebuild shifted
    /// every later face id and renumbered every half-edge.
    #[test]
    fn dice_keeps_ids_of_untouched_geometry() {
        use crate::half_edge::rebuild_ids_test_support::{assert_ids_name_same_geometry, grid};
        let mut m = grid(3);
        m.delete_vertex(VertexId(15)); // kills face 8 → dead slots before the op
        let before = m.clone();
        // Faces 4 (5,6,10,9) and 7 (9,10,14,13) share no edge with face 0 (0,1,5,4).
        let edges = before.face_half_edges(FaceId(4));
        m.dice_faces(&HashSet::from([0u32]), 2, 2, 0.0);
        assert!(m.validate().is_ok(), "{:?}", m.validate());
        assert_eq!(m.face_count(), before.face_count() + 3, "face 0 → 4 cells");
        assert_ids_name_same_geometry(&before, &m, &[VertexId(14), VertexId(10)], &edges, &[FaceId(4), FaceId(7)]);
        assert!(!m.is_vert_live(VertexId(15)), "a dead slot must stay dead, not come back loose");
    }
}
