use core_math::nurbs::NurbsSurface;
use core_math::nurbs_fillet::{apply_edges_fillet_batch, cube_single_top_edge_matching_freecad5};

#[test]
fn mesh_no_corner_after_single_edge() {
    let mut faces = NurbsSurface::new_box_nurbs_faces(0.5, 0.5, 0.5);
    let e = cube_single_top_edge_matching_freecad5(&faces);
    apply_edges_fillet_batch(&mut faces, &[e], 0.2, 100).unwrap();
    let corner = glam::Vec3::new(0.5, 0.5, 0.5);
    let r = 0.2f32;
    let fc = glam::Vec3::new(0.5 - r, 0.5 - r, 0.5);
    for f in &faces {
        let (verts, idx) = f.tessellate();
        let mut near_corner_outside = 0u32;
        let mut near_corner_any = 0u32;
        for v in &verts {
            let p = glam::Vec3::new(v[0], v[1], v[2]);
            if (p - corner).length() < 0.08 {
                near_corner_any += 1;
                let d = ((p.x - fc.x).powi(2) + (p.y - fc.y).powi(2)).sqrt();
                // on front plane-ish
                if (p.z - 0.5).abs() < 0.02 && d > r + 0.01 {
                    near_corner_outside += 1;
                    eprintln!("{} VERT outside free-end: p={p:?} d={d}", f.surface.name);
                }
            }
        }
        // Also check tri centroids
        let mut bad_c = 0u32;
        for tri in idx.chunks(3) {
            let p = |i: u32| {
                let v = &verts[i as usize];
                glam::Vec3::new(v[0], v[1], v[2])
            };
            let c = (p(tri[0]) + p(tri[1]) + p(tri[2])) / 3.0;
            if (c - corner).length() < 0.12 && (c.z - 0.5).abs() < 0.02 {
                let d = ((c.x - fc.x).powi(2) + (c.y - fc.y).powi(2)).sqrt();
                if d > r + 0.015 {
                    bad_c += 1;
                    eprintln!("{} CENTROID outside: c={c:?} d={d}", f.surface.name);
                }
            }
        }
        eprintln!(
            "{} verts={} tris={} near_corner_verts={near_corner_any} outside_verts={near_corner_outside} bad_centroids={bad_c} trims={}",
            f.surface.name, verts.len(), idx.len()/3, f.trims.len()
        );
        assert_eq!(near_corner_outside, 0, "{} has mesh verts in free-end wedge", f.surface.name);
        assert_eq!(bad_c, 0, "{} has centroids in free-end wedge", f.surface.name);
    }
}
