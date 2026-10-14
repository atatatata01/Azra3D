//! Circle-knife repro triage (2026-08-07): owner reports "KNIFE CIRCLE FAIL TO DO, GOT
//! SEGMENT AFTER CUT" — the C (circle) shape knife leaves stray segments/edges.
//!
//! Run:  cargo test -p app diag_circle -- --ignored --nocapture
//!
//! Measures, per poly body, the four shapes "stray segment" can take:
//!   * boundary (unpaired) half-edges — an open rim where the cut failed to close
//!   * loose / wire edges — a live vertex whose fan carries no live face, or a
//!     face-less half-edge whose twin is also face-less
//!   * zero-area sliver faces
//!   * valence-2 collinear pin vertices (leftover densify samples on a straight run)
//! plus the standard census (validate/manifold/arity/repeated rings/coincident verts).
#[cfg(test)]
mod diag_circle_knife {
    use crate::project::io::load_binary;
    use core_math::half_edge::{FaceId, HalfEdgeId, VertexId, NO_FACE, NO_TWIN};
    use core_math::scene_graph::NodePayload;
    use std::collections::{BTreeMap, HashMap, HashSet};

    fn report(path: &str) {
        eprintln!("\n================ {path} ================");
        let pf = match load_binary(std::path::Path::new(path)) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("LOAD FAILED: {e}");
                return;
            }
        };
        let Some(sg) = pf.scene_graph.as_ref() else {
            eprintln!("no scene graph");
            return;
        };
        for (nid, node) in sg.all_nodes() {
            let NodePayload::PolygonalBody { name, mesh, .. } = &node.payload else {
                continue;
            };
            eprintln!(
                "\n--- '{name}' nid={nid:?}  v={} f={} e={}",
                mesh.vertex_count(),
                mesh.face_count(),
                mesh.edge_count()
            );
            eprintln!("  validate  = {:?}", mesh.validate().err());
            eprintln!("  manifold  = {:?}", mesh.is_manifold().err());

            // ---- face arity + repeated-vertex rings ----
            let mut arity: BTreeMap<usize, usize> = BTreeMap::new();
            let mut repeated: Vec<(u32, usize, usize)> = Vec::new();
            let mut slivers: Vec<(u32, f32)> = Vec::new();
            let mut area_sum = 0.0f32;
            for fi in 0..mesh.faces.len() as u32 {
                let f = FaceId(fi);
                if !mesh.is_face_live(f) {
                    continue;
                }
                let vs = mesh.face_vertices(f);
                *arity.entry(vs.len()).or_insert(0) += 1;
                let uniq: HashSet<u32> = vs.iter().map(|v| v.0).collect();
                if uniq.len() < vs.len() {
                    repeated.push((fi, vs.len(), uniq.len()));
                }
                // Newell area
                let mut n = glam::Vec3::ZERO;
                for i in 0..vs.len() {
                    let a = mesh.vert(vs[i]).position;
                    let b = mesh.vert(vs[(i + 1) % vs.len()]).position;
                    n += a.cross(b);
                }
                let area = 0.5 * n.length();
                area_sum += area;
                slivers.push((fi, area));
            }
            eprintln!("  face arity hist: {arity:?}");
            let mean_area = if slivers.is_empty() { 1.0 } else { area_sum / slivers.len() as f32 };
            let mut tiny: Vec<&(u32, f32)> =
                slivers.iter().filter(|(_, a)| *a < mean_area * 1e-3).collect();
            tiny.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap());
            eprintln!(
                "  SLIVER faces (<0.1% of mean area {mean_area:.6}): {}",
                tiny.len()
            );
            for (f, a) in tiny.iter().take(10) {
                let vs = mesh.face_vertices(FaceId(*f));
                eprintln!(
                    "    face {f} area={a:.9} ring={:?}",
                    vs.iter().map(|v| v.0).collect::<Vec<_>>()
                );
            }
            // ---- full ring dump for the worst face (the "stray segment" carrier) ----
            if let Some(&(wf, ring, uniq)) =
                repeated.iter().max_by_key(|(_, r, _)| *r)
            {
                eprintln!("  WORST FACE {wf}: ring={ring} unique={uniq}");
                let vs = mesh.face_vertices(FaceId(wf));
                let mut seen: HashMap<u32, usize> = HashMap::new();
                for (i, v) in vs.iter().enumerate() {
                    let c = seen.entry(v.0).or_insert(0);
                    *c += 1;
                    eprintln!(
                        "    [{i:3}] v{:<4} x{} {:.4?}",
                        v.0,
                        c,
                        mesh.vert(*v).position
                    );
                }
            }
            if !repeated.is_empty() {
                eprintln!("  REPEATED-VERTEX RINGS (slit/keyhole or damage): {}", repeated.len());
                for (f, ring, uniq) in repeated.iter().take(10) {
                    eprintln!("    face {f}: ring={ring} unique={uniq}");
                }
            }

            // ---- boundary (unpaired) half-edges ----
            let mut boundary = 0usize;
            let mut bsample: Vec<(u32, u32)> = Vec::new();
            for i in 0..mesh.half_edges.len() as u32 {
                let h = HalfEdgeId(i);
                if mesh.he(h).face == NO_FACE {
                    continue;
                }
                if mesh.he(h).twin == NO_TWIN {
                    boundary += 1;
                    if bsample.len() < 12 {
                        bsample.push((mesh.he_source(h).0, mesh.he(h).vertex.0));
                    }
                }
            }
            eprintln!("  boundary (unpaired) half-edges: {boundary}");
            for (s, d) in &bsample {
                let ps = mesh.vert(VertexId(*s)).position;
                let pd = mesh.vert(VertexId(*d)).position;
                eprintln!("    v{s}{ps:.3?} -> v{d}{pd:.3?}");
            }

            // ---- loose / wire geometry ----
            // (a) live vertex whose fan touches no live face at all = a floating point.
            // (b) half-edge whose face is NO_FACE and whose twin's face is also NO_FACE = wire edge.
            let mut orphan_verts: Vec<u32> = Vec::new();
            for i in 0..mesh.vertices.len() as u32 {
                let v = VertexId(i);
                if !mesh.is_vert_live(v) {
                    continue;
                }
                let fan = mesh.vertex_fan(v);
                let has_face = fan.iter().any(|&h| {
                    let f = mesh.he(h).face;
                    f != NO_FACE && mesh.is_face_live(f)
                });
                if !has_face {
                    orphan_verts.push(i);
                }
            }
            let mut wire_edges: Vec<(u32, u32)> = Vec::new();
            for i in 0..mesh.half_edges.len() as u32 {
                let h = HalfEdgeId(i);
                if mesh.he(h).face != NO_FACE {
                    continue;
                }
                let t = mesh.he(h).twin;
                let twin_faceless = t == NO_TWIN || mesh.he(t).face == NO_FACE;
                if twin_faceless && (t == NO_TWIN || t.0 > i) {
                    wire_edges.push((mesh.he_source(h).0, mesh.he(h).vertex.0));
                }
            }
            eprintln!(
                "  LOOSE: orphan verts={} wire edges={}",
                orphan_verts.len(),
                wire_edges.len()
            );
            for v in orphan_verts.iter().take(10) {
                eprintln!("    orphan v{v} at {:.4?}", mesh.vert(VertexId(*v)).position);
            }
            for (s, d) in wire_edges.iter().take(10) {
                eprintln!(
                    "    wire v{s}{:.4?} -> v{d}{:.4?}",
                    mesh.vert(VertexId(*s)).position,
                    mesh.vert(VertexId(*d)).position
                );
            }

            // ---- valence-2 collinear pin vertices (leftover densify samples) ----
            let mut pins: Vec<(u32, f32)> = Vec::new();
            for i in 0..mesh.vertices.len() as u32 {
                let v = VertexId(i);
                if !mesh.is_vert_live(v) {
                    continue;
                }
                let fan = mesh.vertex_fan(v);
                if fan.len() != 2 {
                    continue;
                }
                let a = mesh.he(fan[0]).vertex;
                let b = mesh.he(fan[1]).vertex;
                let p = mesh.vert(v).position;
                let da = (mesh.vert(a).position - p).normalize_or_zero();
                let db = (mesh.vert(b).position - p).normalize_or_zero();
                let dev = 1.0 + da.dot(db); // 0 = perfectly collinear
                if dev < 1e-3 {
                    pins.push((i, dev));
                }
            }
            eprintln!("  VALENCE-2 COLLINEAR pins: {}", pins.len());
            for (v, d) in pins.iter().take(10) {
                eprintln!("    pin v{v} dev={d:.2e} at {:.4?}", mesh.vert(VertexId(*v)).position);
            }

            // ---- coincident vertices ----
            let (mut lo, mut hi) = (glam::Vec3::splat(f32::MAX), glam::Vec3::splat(f32::MIN));
            for i in 0..mesh.vertices.len() as u32 {
                let v = VertexId(i);
                if !mesh.is_vert_live(v) {
                    continue;
                }
                lo = lo.min(mesh.vert(v).position);
                hi = hi.max(mesh.vert(v).position);
            }
            let diag = if hi.x >= lo.x { (hi - lo).length().max(1e-6) } else { 1.0 };
            let q = (diag * 1e-4).max(1e-6);
            let mut buckets: HashMap<(i64, i64, i64), Vec<u32>> = HashMap::new();
            for i in 0..mesh.vertices.len() as u32 {
                let v = VertexId(i);
                if !mesh.is_vert_live(v) {
                    continue;
                }
                let p = mesh.vert(v).position;
                buckets
                    .entry((
                        (p.x / q).round() as i64,
                        (p.y / q).round() as i64,
                        (p.z / q).round() as i64,
                    ))
                    .or_default()
                    .push(i);
            }
            let dups: Vec<_> = buckets.values().filter(|v| v.len() > 1).collect();
            eprintln!(
                "  bbox diag={diag:.4}  COINCIDENT vertex clusters (<= {q:.6}): {}",
                dups.len()
            );
            for c in dups.iter().take(10) {
                eprintln!("    {:?} at {:.4?}", c, mesh.vert(VertexId(c[0])).position);
            }
        }
    }

    #[test]
    #[ignore]
    fn diag_circle_knife_files() {
        report(r"C:\Users\Proprio\Desktop\KNIFE CIRCLE CUT FAIL.fl4");
    }
}
