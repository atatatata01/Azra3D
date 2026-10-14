//! TEMP diagnostic: edge-loop sizes on Desktop 1234.fl4 (owner mid-belt loop).
#[cfg(test)]
mod diag_edge_loop_1234 {
    use crate::project::io::load_binary;
    use core_math::edge_select;
    use core_math::half_edge::{HalfEdgeId, NO_FACE, NO_TWIN};
    use core_math::scene_graph::NodePayload;
    use std::collections::HashSet;

    #[test]
    #[ignore]
    fn diagnose_1234_edge_loops() {
        let path = r"C:\Users\Proprio\Desktop\1234.fl4";
        let pf = load_binary(std::path::Path::new(path)).expect("load");
        let sg = pf.scene_graph.as_ref().expect("sg");
        for (nid, node) in sg.all_nodes() {
            let NodePayload::PolygonalBody { name, mesh, .. } = &node.payload else {
                continue;
            };
            eprintln!(
                "=== '{name}' nid={nid:?} v={} f={} e={}",
                mesh.vertex_count(),
                mesh.face_count(),
                mesh.edge_count()
            );

            // Valence histogram
            let mut vh = std::collections::BTreeMap::new();
            for i in 0..mesh.vertices.len() as u32 {
                let fan = mesh.vertex_fan(core_math::half_edge::VertexId(i));
                if fan.is_empty() {
                    continue;
                }
                *vh.entry(fan.len()).or_insert(0usize) += 1;
            }
            eprintln!("  valence hist: {vh:?}");

            // Undirected edges: loop size histogram
            let mut seen = HashSet::new();
            let mut size_hist = std::collections::BTreeMap::new();
            let mut long_loops: Vec<(usize, u32, glam::Vec3, glam::Vec3)> = Vec::new();
            for i in 0..mesh.half_edges.len() as u32 {
                let h = HalfEdgeId(i);
                if mesh.he(h).face == NO_FACE {
                    continue;
                }
                let t = mesh.he(h).twin;
                let key = if t != NO_TWIN { i.min(t.0) } else { i };
                if !seen.insert(key) {
                    continue;
                }
                let lp = edge_select::edge_loop(mesh, h);
                let mut keys = HashSet::new();
                for &e in &lp {
                    let tt = mesh.he(e).twin;
                    keys.insert(if tt != NO_TWIN { e.0.min(tt.0) } else { e.0 });
                }
                let n = keys.len();
                *size_hist.entry(n).or_insert(0usize) += 1;
                if n >= 6 {
                    let s = mesh.he_source(h).0;
                    let d = mesh.he(h).vertex.0;
                    let ps = mesh.vert(core_math::half_edge::VertexId(s)).position;
                    let pd = mesh.vert(core_math::half_edge::VertexId(d)).position;
                    long_loops.push((n, key, ps, pd));
                }
            }
            eprintln!("  undirected edge-loop size hist: {size_hist:?}");
            long_loops.sort_by(|a, b| b.0.cmp(&a.0));
            long_loops.truncate(15);
            eprintln!("  longest loops (up to 15):");
            for (n, key, ps, pd) in &long_loops {
                eprintln!(
                    "    size={n} seed=he{key}  {ps:.3?} -- {pd:.3?}"
                );
            }
        }
    }
}
