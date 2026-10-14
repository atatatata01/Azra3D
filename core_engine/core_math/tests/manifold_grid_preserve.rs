// Repro: does a boolean Difference wipe the input's coplanar grid tessellation?
use core_math::half_edge::HalfEdgeMesh;
use core_math::mesh_boolean::{mesh_boolean, BoolOp};
use glam::Vec3;

/// Closed cube [-1,1]^3 with each face split into an n x n quad grid.
fn grid_cube(n: usize) -> HalfEdgeMesh {
    let mut pos: Vec<Vec3> = Vec::new();
    let mut faces: Vec<Vec<usize>> = Vec::new();
    let mut idx = std::collections::HashMap::new();
    let mut vid = |pos: &mut Vec<Vec3>, idx: &mut std::collections::HashMap<[i64;3], usize>, p: Vec3| {
        let k = [(p.x*1e5).round() as i64, (p.y*1e5).round() as i64, (p.z*1e5).round() as i64];
        *idx.entry(k).or_insert_with(|| { pos.push(p); pos.len()-1 })
    };
    // 6 faces: for each axis and sign, param over the other two axes.
    let axes = [(0usize,1usize,2usize), (1,2,0), (2,0,1)];
    for &(a, b, c) in &axes {
        for &s in &[1.0f32, -1.0] {
            for i in 0..n {
                for j in 0..n {
                    let f = |u: f32, v: f32| {
                        let mut p = [0.0f32; 3];
                        p[a] = s;
                        p[b] = -1.0 + 2.0*u;
                        p[c] = -1.0 + 2.0*v;
                        Vec3::new(p[0], p[1], p[2])
                    };
                    let (u0, u1) = (i as f32/n as f32, (i+1) as f32/n as f32);
                    let (v0, v1) = (j as f32/n as f32, (j+1) as f32/n as f32);
                    let quad = [f(u0,v0), f(u1,v0), f(u1,v1), f(u0,v1)];
                    let ids: Vec<usize> = quad.iter().map(|&p| vid(&mut pos, &mut idx, p)).collect();
                    // outward winding: flip when needed
                    let n0 = (quad[1]-quad[0]).cross(quad[2]-quad[0]);
                    let outward = {
                        let mut d = [0.0f32;3]; d[a] = s; Vec3::new(d[0],d[1],d[2])
                    };
                    if n0.dot(outward) >= 0.0 {
                        faces.push(ids);
                    } else {
                        faces.push(vec![ids[3], ids[2], ids[1], ids[0]]);
                    }
                }
            }
        }
    }
    HalfEdgeMesh::from_polygons(&pos, &faces)
}

#[test]
fn boolean_difference_keeps_grid_tessellation() {
    let cube = grid_cube(8);
    assert!(cube.is_manifold().is_ok(), "grid cube must be manifold");
    let v_in = cube.vertex_count();
    let f_in = cube.face_count();
    // Small notch cutter: a box that clips the top face near one corner.
    let mut cutter = HalfEdgeMesh::new_box(Vec3::new(0.15, 0.5, 0.15));
    for i in 0..cutter.vertices.len() as u32 {
        use core_math::half_edge::{VertexId, NO_EDGE};
        let v = VertexId(i);
        if cutter.vert(v).half_edge != NO_EDGE {
            cutter.vert_mut(v).position += Vec3::new(0.4, 1.0, 0.4);
        }
    }
    let res = mesh_boolean(&cube, &cutter, BoolOp::Difference).expect("boolean must succeed");
    eprintln!("[GRIDBOOL] in: v={v_in} f={f_in}  out: v={} f={}", res.vertex_count(), res.face_count());
    // A notch on the top face must LOCALLY change topology, never flatten the whole grid.
    // 8x8x6 grid cube has 386 verts; losing more than the top-face region means global wipe.
    assert!(
        res.vertex_count() > v_in / 2,
        "boolean wiped the input tessellation: {} -> {} verts",
        v_in, res.vertex_count()
    );
}
