//! Owner repro triage (2026-08-11): `UNBEVEL TEST.fl4` — *"the yellow strip is a bevel I made
//! using bevel tool and Unbevel tool does not work"*.
//!
//! Run:  cargo test -p app diag_unbevel -- --ignored --nocapture
//!
//! The yellow strip is the Bevel POLYGROUP the bevel confirm writes (`PolyGroupKind::Bevel`),
//! so this dump reproduces exactly what `poly_activate_unbevel` would feed the reverse math:
//! the group's faces. It then reports which gate inside `unbevel_from_faces` refuses, since
//! the app collapses every refusal into one toast ("reverse math refused").
#[cfg(test)]
mod diag_unbevel {
    use crate::project::io::load_binary;
    use core_math::half_edge::{FaceId, HalfEdgeId, HalfEdgeMesh, NO_EDGE, NO_FACE, NO_TWIN};
    use core_math::scene_graph::NodePayload;
    use std::collections::HashSet;

    const FILE: &str = r"C:\Users\Proprio\Desktop\UNBEVEL TEST.fl4";

    fn live_faces(m: &HalfEdgeMesh) -> Vec<u32> {
        (0..m.faces.len() as u32)
            .filter(|&f| m.face(FaceId(f)).half_edge != NO_EDGE)
            .collect()
    }

    fn boundary_count(m: &HalfEdgeMesh) -> usize {
        (0..m.half_edges.len() as u32)
            .map(HalfEdgeId)
            .filter(|&h| m.is_edge_live(h) && m.he(h).face != NO_FACE && m.he(h).twin == NO_TWIN)
            .count()
    }

    #[test]
    #[ignore]
    fn diag_unbevel_dump() {
        let pf = load_binary(std::path::Path::new(FILE)).expect("load UNBEVEL TEST.fl4");
        let graph = pf.scene_graph.as_ref().expect("scene graph");

        println!("== poly layer records: {}", pf.poly_layers.len());
        for rec in &pf.poly_layers {
            println!(
                "   node {} : groups={} meta={}",
                rec.node_id,
                rec.groups.len(),
                rec.group_meta.len()
            );
            for m in &rec.group_meta {
                let n = rec.groups.iter().filter(|(_, g)| *g == m.id).count();
                println!(
                    "      group id={} kind={:?} name={:?} color={:?} faces={} bevel={:?}",
                    m.id, m.kind, m.name, m.color, n, m.bevel
                );
            }
        }

        for (nid, node) in graph.all_nodes() {
            let NodePayload::PolygonalBody { mesh, name, .. } = &node.payload else {
                continue;
            };
            let mesh: &HalfEdgeMesh = mesh;
            println!(
                "\n== body nid={} name={:?}: v={} f_live={} he={} boundary={} manifold={:?}",
                nid.0,
                name,
                mesh.vertices.len(),
                live_faces(mesh).len(),
                mesh.half_edges.len(),
                boundary_count(mesh),
                mesh.is_manifold().err()
            );

            let Some(rec) = pf.poly_layers.iter().find(|r| r.node_id == nid.0) else {
                println!("   (no poly layer record — no polygroups saved)");
                continue;
            };

            // Dump the mesh + each group's faces so the reverse math can be iterated in
            // core_math (seconds to build) instead of through the app crate (minutes).
            {
                let mut out = String::new();
                for v in &mesh.vertices {
                    out.push_str(&format!(
                        "v {} {} {}\n",
                        v.position.x, v.position.y, v.position.z
                    ));
                }
                for f in live_faces(mesh) {
                    let ring = mesh.face_vertices(FaceId(f));
                    out.push_str(&format!("f {}", f));
                    for v in ring {
                        out.push_str(&format!(" {}", v.0));
                    }
                    out.push('\n');
                }
                for meta in &rec.group_meta {
                    out.push_str(&format!("g {} {:?}", meta.id, meta.kind));
                    for (f, g) in &rec.groups {
                        if g == &meta.id {
                            out.push_str(&format!(" {}", f));
                        }
                    }
                    out.push('\n');
                }
                let p = std::env::temp_dir().join(format!("azra_unbevel_body_{}.txt", nid.0));
                std::fs::write(&p, out).ok();
                println!("   [dump] {}", p.display());
            }

            for meta in &rec.group_meta {
                let faces: HashSet<u32> = rec
                    .groups
                    .iter()
                    .filter(|(_, g)| *g == meta.id)
                    .map(|(f, _)| *f)
                    .collect();
                if faces.is_empty() {
                    continue;
                }
                println!(
                    "\n   -- group {} ({:?}) : {} faces -> unbevel_from_faces",
                    meta.id,
                    meta.kind,
                    faces.len()
                );
                let alive: HashSet<u32> = faces
                    .iter()
                    .copied()
                    .filter(|&f| {
                        (f as usize) < mesh.faces.len()
                            && mesh.face(FaceId(f)).half_edge != NO_EDGE
                    })
                    .collect();
                println!("      alive faces in group: {}", alive.len());
                match core_math::unbevel::unbevel_from_faces(mesh, &alive) {
                    None => println!("      RESULT: None (refused)"),
                    Some(r) => println!(
                        "      RESULT: v={} f={} sharp={} width={:.4} segs={} manifold={:?}",
                        r.mesh.vertices.len(),
                        r.mesh.face_count(),
                        r.sharp_edges.len(),
                        r.estimated_width,
                        r.estimated_segments,
                        r.mesh.is_manifold().err()
                    ),
                }
                // Also try the edge seed the user would produce by clicking a rail edge.
                let mut rails: HashSet<u32> = HashSet::new();
                for &f in &alive {
                    for h in mesh.face_half_edges(FaceId(f)) {
                        let he = mesh.he(h);
                        let outside = he.twin == NO_TWIN
                            || mesh.he(he.twin).face == NO_FACE
                            || !alive.contains(&mesh.he(he.twin).face.0);
                        if outside {
                            rails.insert(h.0);
                        }
                    }
                }
                println!("      strip boundary half-edges: {}", rails.len());
                if let Some(&one) = rails.iter().min() {
                    match core_math::unbevel::unbevel_from_edges(mesh, &HashSet::from([one])) {
                        None => println!("      edge-seed (1 rail) RESULT: None (refused)"),
                        Some(r) => println!(
                            "      edge-seed (1 rail) RESULT: v={} f={} sharp={}",
                            r.mesh.vertices.len(),
                            r.mesh.face_count(),
                            r.sharp_edges.len()
                        ),
                    }
                }
            }
        }
    }
}
