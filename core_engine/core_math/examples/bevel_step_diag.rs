// Dump rail/arc endpoints by hacking through public API: compare arc span at both ends of strip
use core_math::half_edge::*;
use core_math::bevel_op::{bevel_edges_offset, BevelParams};
use glam::Vec3;
use std::collections::HashSet;

fn owner_step() -> (HalfEdgeMesh, u32) {
    let p: Vec<Vec3> = vec![
        Vec3::new(-0.5, -0.5, -0.5), Vec3::new(0.5, -0.5, -0.5),
        Vec3::new(0.5, -0.5, 0.5),   Vec3::new(-0.5, -0.5, 0.5),
        Vec3::new(-0.5, 0.5, -0.5),  Vec3::new(0.5, 0.5, -0.5),
        Vec3::new(0.5, 0.5, 0.5),    Vec3::new(-0.5, 0.5, 0.5),
        Vec3::new(0.5, -0.5, 1.847281),  Vec3::new(-0.5, 0.5, 1.847281),
        Vec3::new(-0.5, -0.5, 1.847281), Vec3::new(0.5, 0.5, 1.847281),
        Vec3::new(0.5, 1.683461, 0.5),   Vec3::new(-0.5, 1.683461, 1.847281),
        Vec3::new(0.5, 1.683461, 1.847281), Vec3::new(-0.5, 1.683461, 0.5),
    ];
    let f: Vec<Vec<usize>> = vec![
        vec![1, 2, 3, 0],   vec![6, 5, 4, 7],   vec![5, 1, 0, 4],
        vec![9, 10, 8, 11], vec![4, 0, 3, 7],   vec![6, 2, 1, 5],
        vec![15, 13, 14, 12], vec![3, 10, 9, 7], vec![2, 8, 10, 3],
        vec![6, 11, 8, 2],  vec![7, 15, 12, 6], vec![9, 13, 15, 7],
        vec![11, 14, 13, 9], vec![6, 12, 14, 11],
    ];
    let m = HalfEdgeMesh::from_polygons(&p, &f);
    let mut edge = None;
    for i in 0..m.half_edges.len() as u32 {
        let h = HalfEdgeId(i);
        let (s,d)=(m.he_source(h).0, m.he(h).vertex.0);
        if (s==7&&d==6)||(s==6&&d==7) { edge=Some(i); break; }
    }
    (m, edge.unwrap())
}

fn main() {
    let (m, e) = owner_step();
    let he = HalfEdgeId(e);
    let a = m.vert(m.he_source(he)).position;
    let b = m.vert(m.he(he).vertex).position;
    println!("edge {:?} -> {:?}", a, b);
    let sel: HashSet<u32> = std::iter::once(e).collect();
    let r = bevel_edges_offset(&m, &sel, &BevelParams{width:0.15, segments:5, shape:0.5, clamp:true});

    // At each endpoint of original edge, collect nearby new verts and measure span of profile
    for (label, ep) in [("A", a), ("B", b)] {
        let mut near: Vec<Vec3> = r.vertices.iter().map(|v| v.position)
            .filter(|p| (*p - ep).length() < 0.35)
            .collect();
        // project off the edge axis
        let axis = (b-a).normalize();
        near.sort_by(|p,q| {
            let tp = (*p-ep - axis*(*p-ep).dot(axis)).length();
            let tq = (*q-ep - axis*(*q-ep).dot(axis)).length();
            tp.partial_cmp(&tq).unwrap()
        });
        let spans: Vec<f32> = near.iter().map(|p| (*p-ep - axis*(*p-ep).dot(axis)).length()).collect();
        let max_span = spans.iter().cloned().fold(0.0f32, f32::max);
        let min_span = spans.iter().cloned().fold(f32::MAX, f32::min);
        println!("end {label} ep={ep:?}: { } near verts, radial span min={min_span:.4} max={max_span:.4}", near.len());
        for p in near.iter().take(12) {
            let rad = (*p-ep - axis*(*p-ep).dot(axis)).length();
            println!("  {:?} rad={rad:.4}", p);
        }
    }
}
