//! TEMP diagnostic for 22.fl4 edge-loop-on-slice. Not permanent.
#[cfg(test)]
mod diag_edge_loop_22 {
    use crate::project::io::load_binary;
    use core_math::edge_select;
    use core_math::half_edge::{HalfEdgeId, NO_FACE, NO_TWIN};
    use core_math::scene_graph::NodePayload;
    use std::collections::HashSet;

    #[test]
    #[ignore]
    fn diagnose_22_slice_edge_loops() {
        let path = r"C:\Users\Proprio\Desktop\22.fl4";
        let pf = load_binary(std::path::Path::new(path)).expect("load");
        let sg = pf.scene_graph.as_ref().expect("sg");
        for (nid, node) in sg.all_nodes() {
            let NodePayload::PolygonalBody { name, mesh, .. } = &node.payload else { continue };
            eprintln!("=== '{name}' nid={nid:?} v={} f={} e={}", mesh.vertex_count(), mesh.face_count(), mesh.edge_count());

            // valence census (skip zero-fan = free / unused slots)
            let mut val_hist = std::collections::BTreeMap::new();
            for i in 0..mesh.vertices.len() as u32 {
                let fan = mesh.vertex_fan(core_math::half_edge::VertexId(i));
                if fan.is_empty() { continue; }
                *val_hist.entry(fan.len()).or_insert(0usize) += 1;
                if fan.len() != 3 && fan.len() != 4 {
                    let p = mesh.vert(core_math::half_edge::VertexId(i)).position;
                    eprintln!("  v{i} valence={} pos={:.3?}", fan.len(), p);
                    for &h in &fan {
                        let d = mesh.he(h).vertex.0;
                        let face = mesh.he(h).face;
                        eprintln!("    -> v{d} face={:?} len={}", d, if face != NO_FACE { mesh.face_half_edges(face).len() } else { 0 });
                    }
                }
            }
            eprintln!("  valence hist: {val_hist:?}");

            // undirected edges + loop sizes
            let mut seen = HashSet::new();
            let mut loop_sizes = std::collections::BTreeMap::new();
            let mut short_loops = Vec::new();
            for i in 0..mesh.half_edges.len() as u32 {
                let h = HalfEdgeId(i);
                if mesh.he(h).face == NO_FACE { continue; }
                let t = mesh.he(h).twin;
                let key = if t != NO_TWIN { i.min(t.0) } else { i };
                if !seen.insert(key) { continue; }
                let lp = edge_select::edge_loop(mesh, h);
                let mut keys = HashSet::new();
                for &e in &lp {
                    let tt = mesh.he(e).twin;
                    keys.insert(if tt != NO_TWIN { e.0.min(tt.0) } else { e.0 });
                }
                let n = keys.len();
                *loop_sizes.entry(n).or_insert(0usize) += 1;
                if n <= 3 {
                    let s = mesh.he_source(h).0;
                    let d = mesh.he(h).vertex.0;
                    let ps = mesh.vert(core_math::half_edge::VertexId(s)).position;
                    let pd = mesh.vert(core_math::half_edge::VertexId(d)).position;
                    short_loops.push((key, s, d, n, ps, pd, lp.len()));
                }
            }
            eprintln!("  undirected-edge loop-size hist: {loop_sizes:?}");
            eprintln!("  short loops (size<=3):");
            for (key, s, d, n, ps, pd, raw) in &short_loops {
                eprintln!("    he{key} v{s}->v{d} loop={n} raw={raw} p0={ps:.3?} p1={pd:.3?}");
            }

            // Find longest loops and dump their edge chains
            let mut best: Vec<(usize, u32, Vec<HalfEdgeId>)> = Vec::new();
            let mut seen2 = HashSet::new();
            for i in 0..mesh.half_edges.len() as u32 {
                let h = HalfEdgeId(i);
                if mesh.he(h).face == NO_FACE { continue; }
                let t = mesh.he(h).twin;
                let key = if t != NO_TWIN { i.min(t.0) } else { i };
                if !seen2.insert(key) { continue; }
                let lp = edge_select::edge_loop(mesh, h);
                let mut keys = HashSet::new();
                for &e in &lp {
                    let tt = mesh.he(e).twin;
                    keys.insert(if tt != NO_TWIN { e.0.min(tt.0) } else { e.0 });
                }
                best.push((keys.len(), key, lp));
            }
            best.sort_by(|a, b| b.0.cmp(&a.0));
            for (n, seed, lp) in best.iter().take(8) {
                eprintln!("  BEST loop size={n} seed=he{seed}:");
                for &e in lp {
                    let s = mesh.he_source(e).0;
                    let d = mesh.he(e).vertex.0;
                    let ps = mesh.vert(core_math::half_edge::VertexId(s)).position;
                    let pd = mesh.vert(core_math::half_edge::VertexId(d)).position;
                    let va = mesh.vertex_fan(core_math::half_edge::VertexId(s)).len();
                    let vb = mesh.vertex_fan(core_math::half_edge::VertexId(d)).len();
                    eprintln!("    v{s}(val{va}) -> v{d}(val{vb})  {ps:.3?} -> {pd:.3?}");
                }
            }

            // Slice-related: edges with ≥1 non-corner endpoint (original cube corners = v0..v7)
            let corner: HashSet<u32> = (0..8).collect();
            eprintln!("  edges with ≥1 non-corner endpoint (slice-related):");
            let mut seen3 = HashSet::new();
            for i in 0..mesh.half_edges.len() as u32 {
                let h = HalfEdgeId(i);
                if mesh.he(h).face == NO_FACE { continue; }
                let t = mesh.he(h).twin;
                let key = if t != NO_TWIN { i.min(t.0) } else { i };
                if !seen3.insert(key) { continue; }
                let s = mesh.he_source(h).0;
                let d = mesh.he(h).vertex.0;
                if corner.contains(&s) && corner.contains(&d) { continue; }
                let lp = edge_select::edge_loop(mesh, h);
                let mut keys = HashSet::new();
                for &e in &lp {
                    let tt = mesh.he(e).twin;
                    keys.insert(if tt != NO_TWIN { e.0.min(tt.0) } else { e.0 });
                }
                let ps = mesh.vert(core_math::half_edge::VertexId(s)).position;
                let pd = mesh.vert(core_math::half_edge::VertexId(d)).position;
                let va = mesh.vertex_fan(core_math::half_edge::VertexId(s)).len();
                let vb = mesh.vertex_fan(core_math::half_edge::VertexId(d)).len();
                eprintln!("    he{key} v{s}(val{va})-v{d}(val{vb}) loop={}  {ps:.3?}--{pd:.3?}", keys.len());
            }
        }
    }
}
