//! Owner repro triage (2026-08-11): `EDGE SELECT PROBLEM, BOLEAN PROBLEM.fl4`.
//!
//! Run:  cargo test -p app diag_edgesel_bool -- --ignored --nocapture
//!
//! Two reported symptoms, one file:
//!   * "I was trying to subtract the yellow piece and it did not work" — the boolean
//!     preflight rejects on OPEN meshes (`boundary_half_edge_count > 0`) for BOTH
//!     operands, and the install guard rejects a non-closed-manifold RESULT. This dump
//!     answers which gate the saved geometry would trip, per body, before any UI is
//!     involved.
//!   * "edge select works well at first, then degrades" — dump the per-body edge
//!     topology health (boundary edges, non-manifold edges, duplicate/zero-length
//!     edges, freed-slot density) since a degrading pick is usually a degrading
//!     half-edge structure, not a degrading input path.
#[cfg(test)]
mod diag_edgesel_bool {
    use crate::project::io::load_binary;
    use core_math::half_edge::{FaceId, HalfEdgeId, HalfEdgeMesh, NO_EDGE, NO_FACE, NO_TWIN};
    use core_math::scene_graph::NodePayload;
    use std::collections::HashMap;

    const FILE: &str = r"C:\Users\Proprio\Desktop\EDGE SELECT PROBLEM, BOLEAN PROBLEM.fl4";

    /// Boundary (open) half-edges: live, has a face, no twin.
    fn boundary_count(m: &HalfEdgeMesh) -> usize {
        (0..m.half_edges.len() as u32)
            .map(HalfEdgeId)
            .filter(|&h| m.is_edge_live(h) && m.he(h).face != NO_FACE && m.he(h).twin == NO_TWIN)
            .count()
    }

    /// Face -> island id (connected components over faces, via vertex sharing).
    fn island_of_face(m: &HalfEdgeMesh) -> HashMap<u32, usize> {
        let nf = m.faces.len();
        let mut owner: HashMap<u32, usize> = HashMap::new();
        let mut parent: Vec<usize> = (0..nf).collect();
        fn find(p: &mut Vec<usize>, mut x: usize) -> usize {
            while p[x] != x {
                p[x] = p[p[x]];
                x = p[x];
            }
            x
        }
        let mut live: Vec<usize> = Vec::new();
        for i in 0..nf {
            let fid = FaceId(i as u32);
            if m.face(fid).half_edge == NO_EDGE {
                continue;
            }
            live.push(i);
            for v in m.face_vertices(fid) {
                match owner.get(&v.0) {
                    Some(&other) => {
                        let (a, b) = (find(&mut parent, i), find(&mut parent, other));
                        if a != b {
                            parent[a] = b;
                        }
                    }
                    None => {
                        owner.insert(v.0, i);
                    }
                }
            }
        }
        let mut ids: HashMap<usize, usize> = HashMap::new();
        let mut out = HashMap::new();
        for i in live {
            let r = find(&mut parent, i);
            let n = ids.len();
            let id = *ids.entry(r).or_insert(n);
            out.insert(i as u32, id);
        }
        out
    }

    /// Connected components over faces, via vertex sharing.
    fn island_count(m: &HalfEdgeMesh) -> usize {
        let nf = m.faces.len();
        let mut owner: HashMap<u32, usize> = HashMap::new(); // vertex -> island
        let mut parent: Vec<usize> = (0..nf).collect();
        fn find(p: &mut Vec<usize>, mut x: usize) -> usize {
            while p[x] != x {
                p[x] = p[p[x]];
                x = p[x];
            }
            x
        }
        let mut live: Vec<usize> = Vec::new();
        for i in 0..nf {
            let fid = FaceId(i as u32);
            if m.face(fid).half_edge == NO_EDGE {
                continue;
            }
            live.push(i);
            for v in m.face_vertices(fid) {
                match owner.get(&v.0) {
                    Some(&other) => {
                        let (a, b) = (find(&mut parent, i), find(&mut parent, other));
                        if a != b {
                            parent[a] = b;
                        }
                    }
                    None => {
                        owner.insert(v.0, i);
                    }
                }
            }
        }
        let mut roots = std::collections::HashSet::new();
        for i in live {
            let r = find(&mut parent, i);
            roots.insert(r);
        }
        roots.len()
    }

    /// Half-edge slot health: how much of the arrays is dead (freed) space.
    fn slot_health(m: &HalfEdgeMesh) -> (usize, usize, usize, usize, usize, usize) {
        let he_total = m.half_edges.len();
        let he_live = (0..he_total as u32)
            .map(HalfEdgeId)
            .filter(|&h| m.is_edge_live(h))
            .count();
        let f_total = m.faces.len();
        let f_live = (0..f_total as u32)
            .filter(|&i| m.face(FaceId(i)).half_edge != NO_EDGE)
            .count();
        let v_total = m.vertices.len();
        let v_live = m.vertex_count();
        (he_total, he_live, f_total, f_live, v_total, v_live)
    }

    #[test]
    #[ignore]
    fn diag_edgesel_bool_dump() {
        eprintln!("================ {FILE} ================");
        let pf = match load_binary(std::path::Path::new(FILE)) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("LOAD FAILED: {e}");
                return;
            }
        };
        eprintln!(
            "version={}  poly_layers={}  imported={}",
            pf.version,
            pf.poly_layers.len(),
            pf.imported_meshes.len()
        );
        let Some(sg) = pf.scene_graph.as_ref() else {
            eprintln!("no scene graph");
            return;
        };

        // Which material is YELLOW? The owner named the cutter by colour.
        eprintln!("\n---- materials (graph nodes) ----");
        for (nid, node) in sg.all_nodes() {
            if let NodePayload::Material { id, name, data } = &node.payload {
                let c = data.albedo;
                let yellow = c.x > 0.35 && c.y > 0.35 && c.z < 0.35;
                eprintln!(
                    "  mat nid={nid:?} id={} '{name}' albedo=({:.3},{:.3},{:.3}) {}",
                    id.0,
                    c.x,
                    c.y,
                    c.z,
                    if yellow { "<= YELLOW" } else { "" }
                );
            }
        }

        let mut polys: Vec<(String, &HalfEdgeMesh)> = Vec::new();
        for (nid, node) in sg.all_nodes() {
            match &node.payload {
                NodePayload::PolygonalBody {
                    name,
                    mesh,
                    modifiers,
                    ..
                } => {
                    let (het, hel, ft, fl, vt, vl) = slot_health(mesh);
                    let bnd = boundary_count(mesh);
                    eprintln!(
                        "\n--- POLY '{name}' nid={nid:?}  v={vl}/{vt} f={fl}/{ft} he={hel}/{het}"
                    );
                    eprintln!("  validate     = {:?}", mesh.validate().err());
                    eprintln!("  manifold     = {:?}", mesh.is_manifold().err());
                    eprintln!(
                        "  BOUNDARY he  = {bnd}   <= boolean preflight rejects if > 0 (OPEN MESH)"
                    );
                    eprintln!("  islands      = {}", island_count(mesh));
                    eprintln!("  visible      = {}", node.visible);
                    if modifiers.is_empty() {
                        eprintln!("  modifiers    : (none)");
                    }
                    for (i, md) in modifiers.iter().enumerate() {
                        eprintln!(
                            "  mod[{i}] {:?} enabled={} (bevel w={} segs={} smart={})",
                            md.mod_type,
                            md.enabled,
                            md.bevel_width,
                            md.bevel_segments,
                            md.bevel_smart_select
                        );
                    }
                    polys.push((name.clone(), mesh));
                }
                NodePayload::SdfBody { name, .. } => {
                    eprintln!("\n--- SDF BODY '{name}' nid={nid:?}");
                }
                NodePayload::VoxelBody { name, .. } => {
                    eprintln!("\n--- VOXEL BODY '{name}' nid={nid:?}");
                }
                _ => {}
            }
        }

        // ---- Per-island breakdown: THE question for a 1-body / 3-island file ----
        // The X-pie edit-mode path extracts the selected island as the CUTTER and the
        // remainder as the TARGET, then rejects EITHER if it has boundary edges. So the
        // gate that fires depends on which island is open.
        for (name, mesh) in &polys {
            eprintln!("\n================ ISLANDS of '{name}' ================");
            let iof = island_of_face(mesh);
            let n_isl = iof.values().copied().max().map_or(0, |m| m + 1);
            for isl in 0..n_isl {
                let faces: Vec<u32> = iof
                    .iter()
                    .filter(|(_, &v)| v == isl)
                    .map(|(&f, _)| f)
                    .collect();
                // boundary half-edges belonging to this island's faces
                let mut bnd = 0usize;
                let mut verts: std::collections::HashSet<u32> = Default::default();
                let mut mn = glam::Vec3::splat(f32::MAX);
                let mut mx = glam::Vec3::splat(f32::MIN);
                let mut mats: std::collections::BTreeSet<u32> = Default::default();
                for &f in &faces {
                    let fid = FaceId(f);
                    mats.insert(mesh.face(fid).material_id);
                    for v in mesh.face_vertices(fid) {
                        verts.insert(v.0);
                        let p = mesh.vert(v).position;
                        mn = mn.min(p);
                        mx = mx.max(p);
                    }
                    let start = mesh.face(fid).half_edge;
                    let mut h = start;
                    loop {
                        if mesh.he(h).twin == NO_TWIN {
                            bnd += 1;
                        }
                        h = mesh.he(h).next;
                        if h == start || h == NO_EDGE {
                            break;
                        }
                    }
                }
                eprintln!(
                    "  island[{isl}] faces={} verts={} BOUNDARY_he={bnd} {}  mats={mats:?}",
                    faces.len(),
                    verts.len(),
                    if bnd == 0 { "CLOSED ✓" } else { "OPEN ✗" }
                );
                eprintln!(
                    "      aabb min=({:.3},{:.3},{:.3}) max=({:.3},{:.3},{:.3})",
                    mn.x, mn.y, mn.z, mx.x, mx.y, mx.z
                );
            }
            eprintln!("\n  --- island-cutter boolean simulation (cutter=island i, target=remainder) ---");
            for isl in 0..n_isl {
                let cutter_bnd: usize = {
                    let mut b = 0;
                    for (&f, &v) in iof.iter() {
                        if v != isl {
                            continue;
                        }
                        let start = mesh.face(FaceId(f)).half_edge;
                        let mut h = start;
                        loop {
                            if mesh.he(h).twin == NO_TWIN {
                                b += 1;
                            }
                            h = mesh.he(h).next;
                            if h == start || h == NO_EDGE {
                                break;
                            }
                        }
                    }
                    b
                };
                let rem_bnd: usize = {
                    let mut b = 0;
                    for (&f, &v) in iof.iter() {
                        if v == isl {
                            continue;
                        }
                        let start = mesh.face(FaceId(f)).half_edge;
                        let mut h = start;
                        loop {
                            if mesh.he(h).twin == NO_TWIN {
                                b += 1;
                            }
                            h = mesh.he(h).next;
                            if h == start || h == NO_EDGE {
                                break;
                            }
                        }
                    }
                    b
                };
                let verdict = if cutter_bnd > 0 {
                    format!("REJECT — toast \"cutter has open edges\" ({cutter_bnd} boundary he)")
                } else if rem_bnd > 0 {
                    format!("REJECT — toast \"target is open\" ({rem_bnd} boundary he)")
                } else {
                    "would run the kernel".to_string()
                };
                eprintln!("  cutter=island[{isl}] -> {verdict}");
            }
        }

        // ---- Boolean preflight simulation: every ordered pair (target, cutter) ----
        eprintln!("\n================ BOOLEAN PREFLIGHT (world-space omitted; local test) ================");
        for (ti, (tname, tmesh)) in polys.iter().enumerate() {
            for (ci, (cname, cmesh)) in polys.iter().enumerate() {
                if ti == ci {
                    continue;
                }
                let tb = boundary_count(tmesh);
                let cb = boundary_count(cmesh);
                let gate = if cb > 0 {
                    format!("REJECT @cutter-open ({cb} boundary he)")
                } else if tb > 0 {
                    format!("REJECT @target-open ({tb} boundary he)")
                } else {
                    "passes preflight".to_string()
                };
                eprintln!("  Subtract  target='{tname}'  cutter='{cname}'  -> {gate}");
            }
        }
    }
}
