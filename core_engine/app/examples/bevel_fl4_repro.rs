fn main() {
    use core_math::half_edge::*;
    use core_math::bevel_op::{bevel_edges_offset, BevelParams};
    use core_math::scene_graph::NodePayload;
    use glam::Vec3;
    use std::collections::HashSet;
    use std::path::Path;

    // edge_is_concave is crate-private; reimplement sign check here
    fn is_concave(mesh: &HalfEdgeMesh, h: HalfEdgeId) -> bool {
        let e = mesh.he(h);
        if e.twin == NO_TWIN || e.face == NO_FACE { return false; }
        let tf = mesh.he(e.twin).face;
        if tf == NO_FACE { return false; }
        let n1 = mesh.compute_face_normal(e.face).normalize_or_zero();
        let n2 = mesh.compute_face_normal(tf).normalize_or_zero();
        let edir = (mesh.vert(e.vertex).position - mesh.vert(mesh.he_source(h)).position).normalize_or_zero();
        n1.cross(n2).dot(edir) < 0.0
    }

    for fname in [
        r"C:\Users\Proprio\Desktop\bevel segement.fl4",
        r"C:\Users\Proprio\Desktop\BEVEL EDGE CASE.fl4",
    ] {
        let path = Path::new(fname);
        if !path.exists() { println!("missing {fname}"); continue; }
        println!("\n======== {} ========", path.file_name().unwrap().to_string_lossy());
        let proj = match app::project::load_binary(path) {
            Ok(p) => p,
            Err(e) => { println!("load err: {e}"); continue; }
        };
        let Some(sg) = proj.scene_graph else { println!("no graph"); continue; };
        for (nid, node) in sg.iter_nodes() {
            let NodePayload::PolygonalBody { mesh, name, .. } = &node.payload else { continue };
            println!("POLY {nid:?} '{name}' nv={} nf={}", mesh.vertex_count(), mesh.face_count());
            let mut seen = HashSet::new();
            let mut edges = Vec::new();
            for i in 0..mesh.half_edges.len() as u32 {
                let h = HalfEdgeId(i);
                if !mesh.is_edge_live(h) || mesh.he(h).twin == NO_TWIN || mesh.he(h).face == NO_FACE { continue; }
                let key = i.min(mesh.he(h).twin.0);
                if !seen.insert(key) { continue; }
                let d0 = mesh.vertex_fan(mesh.he_source(h)).len();
                let d1 = mesh.vertex_fan(mesh.he(h).vertex).len();
                let conc = is_concave(mesh, h);
                edges.push((key, d0, d1, conc));
            }
            println!("  interior undirected edges: {}", edges.len());
            for (e, d0, d1, conc) in &edges {
                if !conc { continue; }
                let sel: HashSet<u32> = std::iter::once(*e).collect();
                let r = bevel_edges_offset(mesh, &sel, &BevelParams {
                    width: 0.12, segments: 5, shape: 0.5, clamp: true,
                });
                let mut max_tri = 0usize;
                for vi in 0..r.vertex_count() as u32 {
                    let mut tris = 0usize;
                    for fi in 0..r.face_count() as u32 {
                        let vs = r.face_vertices(FaceId(fi));
                        if vs.len() == 3 && vs.iter().any(|v| v.0 == vi) { tris += 1; }
                    }
                    max_tri = max_tri.max(tris);
                }
                let open = (0..r.half_edges.len() as u32)
                    .filter(|&i| r.he(HalfEdgeId(i)).twin == NO_TWIN && r.he(HalfEdgeId(i)).face != NO_FACE)
                    .count();
                let ok = r.is_manifold().is_ok() && open == 0 && max_tri <= 2;
                println!(
                    "  e={e} deg={d0}/{d1} concave -> nv={} nf={} open={} man={:?} max_tri_fan={} {}",
                    r.vertex_count(), r.face_count(), open, r.is_manifold().is_ok(), max_tri,
                    if ok { "OK" } else { "BAD" }
                );
            }
        }
    }
}
