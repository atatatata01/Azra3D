//! Owner repro triage (2026-08-07): loads the owner's saved `.fl4` files and reports the
//! mesh health + modifier state that a screenshot cannot show.
//!
//! Run:  cargo test -p app diag_owner -- --ignored --nocapture
//!
//! Answers two questions directly:
//!   * BEVEL REGRSSION.fl4 — what are the bevel modifiers' ACTUAL params (did the
//!     2026-08-07 `bevel_limit_angle` default change reach this file?), and does the
//!     stored base mesh already carry damage?
//!   * KNIFE FAIL.fl4 / FAIL SLICE.fl4 — is the saved (post-slice) mesh non-manifold,
//!     self-touching, or duplicated, and WHERE.
#[cfg(test)]
mod diag_owner_repro {
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
            let NodePayload::PolygonalBody {
                name,
                mesh,
                modifiers,
                ..
            } = &node.payload
            else {
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

            // ---- modifier stack (the bevel-regression question) ----
            if modifiers.is_empty() {
                eprintln!("  modifiers: (none)");
            }
            for (i, m) in modifiers.iter().enumerate() {
                eprint!("  mod[{i}] {:?} enabled={}", m.mod_type, m.enabled);
                if m.mod_type == core_math::scene_graph::PolyModifierType::Bevel {
                    eprint!(
                        "  width={} segs={} shape={} clamp={} smart={} LIMIT_ANGLE={} angle={}",
                        m.bevel_width,
                        m.bevel_segments,
                        m.bevel_shape,
                        m.bevel_clamp,
                        m.bevel_smart_select,
                        m.bevel_limit_angle,
                        m.bevel_angle_deg
                    );
                }
                eprintln!();
            }

            // ---- face arity census (triangulation / n-gon shape) ----
            let mut arity: BTreeMap<usize, usize> = BTreeMap::new();
            let mut degenerate: Vec<(u32, usize, usize)> = Vec::new(); // (face, ring, unique)
            for fi in 0..mesh.faces.len() as u32 {
                let f = FaceId(fi);
                if !mesh.is_face_live(f) {
                    continue;
                }
                let vs = mesh.face_vertices(f);
                *arity.entry(vs.len()).or_insert(0) += 1;
                let uniq: HashSet<u32> = vs.iter().map(|v| v.0).collect();
                if uniq.len() < vs.len() {
                    degenerate.push((fi, vs.len(), uniq.len()));
                }
            }
            eprintln!("  face arity hist: {arity:?}");
            if !degenerate.is_empty() {
                eprintln!(
                    "  REPEATED-VERTEX RINGS (slit/keyhole or damage): {} faces",
                    degenerate.len()
                );
                for (f, ring, uniq) in degenerate.iter().take(10) {
                    eprintln!("    face {f}: ring={ring} unique={uniq}");
                }
            }

            // ---- boundary (open) edges: a solid must have none ----
            let mut boundary = 0usize;
            let mut boundary_sample: Vec<(u32, u32)> = Vec::new();
            for i in 0..mesh.half_edges.len() as u32 {
                let h = HalfEdgeId(i);
                if mesh.he(h).face == NO_FACE {
                    continue;
                }
                if mesh.he(h).twin == NO_TWIN {
                    boundary += 1;
                    if boundary_sample.len() < 10 {
                        boundary_sample.push((mesh.he_source(h).0, mesh.he(h).vertex.0));
                    }
                }
            }
            eprintln!("  boundary (unpaired) half-edges: {boundary}");
            for (s, d) in &boundary_sample {
                let ps = mesh.vert(VertexId(*s)).position;
                let pd = mesh.vert(VertexId(*d)).position;
                eprintln!("    v{s}{ps:.3?} -> v{d}{pd:.3?}");
            }

            // ---- coincident vertices (the merge-by-distance / duplicate-ring class) ----
            // Quantize to 1e-4 of the mesh diagonal; report buckets holding >1 live vert.
            let (mut lo, mut hi) = (glam::Vec3::splat(f32::MAX), glam::Vec3::splat(f32::MIN));
            for i in 0..mesh.vertices.len() as u32 {
                let v = VertexId(i);
                if !mesh.is_vert_live(v) {
                    continue;
                }
                lo = lo.min(mesh.vert(v).position);
                hi = hi.max(mesh.vert(v).position);
            }
            let diag = if hi.x >= lo.x {
                (hi - lo).length().max(1e-6)
            } else {
                1.0
            };
            let q = (diag * 1e-4).max(1e-6);
            let mut buckets: HashMap<(i64, i64, i64), Vec<u32>> = HashMap::new();
            for i in 0..mesh.vertices.len() as u32 {
                let v = VertexId(i);
                if !mesh.is_vert_live(v) {
                    continue;
                }
                let p = mesh.vert(v).position;
                let key = (
                    (p.x / q).round() as i64,
                    (p.y / q).round() as i64,
                    (p.z / q).round() as i64,
                );
                buckets.entry(key).or_default().push(i);
            }
            let dups: Vec<_> = buckets.values().filter(|v| v.len() > 1).collect();
            eprintln!(
                "  bbox diag={diag:.4}  COINCIDENT vertex clusters (<= {q:.6}): {}",
                dups.len()
            );
            for c in dups.iter().take(10) {
                let p = mesh.vert(VertexId(c[0])).position;
                eprintln!("    {:?} at {p:.4?}", c);
            }
        }
    }

    /// Full topology dump of the preserved KNIFE FAIL repro — every ring, every position.
    /// Run: cargo test -p app diag_knife_fail_topology -- --ignored --nocapture
    #[test]
    #[ignore]
    fn diag_knife_fail_topology() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            r"\..\..\docs\planning\poly\repro\knife_fail_rect_2026-08-07.fl4"
        );
        let pf = load_binary(std::path::Path::new(path)).expect("load repro");
        let sg = pf.scene_graph.as_ref().expect("graph");
        for (_nid, node) in sg.all_nodes() {
            let NodePayload::PolygonalBody { name, mesh, .. } = &node.payload else {
                continue;
            };
            eprintln!("=== {name} v={} f={}", mesh.vertex_count(), mesh.face_count());
            for i in 0..mesh.vertices.len() as u32 {
                let v = VertexId(i);
                if !mesh.is_vert_live(v) {
                    continue;
                }
                let p = mesh.vert(v).position;
                eprintln!("  v{i} = ({:.4}, {:.4}, {:.4})", p.x, p.y, p.z);
            }
            for fi in 0..mesh.faces.len() as u32 {
                let f = FaceId(fi);
                if !mesh.is_face_live(f) {
                    continue;
                }
                let vs: Vec<u32> = mesh.face_vertices(f).iter().map(|v| v.0).collect();
                eprintln!("  f{fi} ({}) = {vs:?}", vs.len());
            }
        }
    }

    /// SLICE.fl4 (owner 2026-08-07): a yellow-material PLANE that should slice the cube via
    /// the boolean pie's Cut. Reports each body's open/closed state and what `mesh_boolean`
    /// actually returns for plane-vs-cube — the pie path needs Difference AND Intersect.
    /// Run: cargo test -p app diag_slice_plane -- --ignored --nocapture
    #[test]
    #[ignore]
    fn diag_slice_plane_vs_cube() {
        use core_math::mesh_boolean::{boundary_half_edge_count, mesh_boolean, BoolOp};
        let path = r"C:\Users\Proprio\Desktop\SLICE.fl4";
        report(path);
        let pf = load_binary(std::path::Path::new(path)).expect("load SLICE.fl4");
        let sg = pf.scene_graph.as_ref().expect("graph");
        let mut bodies: Vec<(String, core_math::half_edge::HalfEdgeMesh)> = Vec::new();
        for (nid, node) in sg.all_nodes() {
            let kind = match &node.payload {
                NodePayload::PolygonalBody { name, .. } => format!("PolygonalBody '{name}'"),
                other => format!("{:?}", std::mem::discriminant(other)),
            };
            eprintln!(
                "node {nid:?} kind={kind} pos={:?} rot={:?} scale={:?} visible={}",
                node.local_position, node.local_rotation, node.local_scale, node.visible
            );
        }
        for (nid, node) in sg.all_nodes() {
            let NodePayload::PolygonalBody { name, mesh, .. } = &node.payload else {
                continue;
            };
            let world = sg.world_transform(nid);
            let world_mesh = crate::poly::boolean::transformed(mesh, world);
            let nb = boundary_half_edge_count(&world_mesh);
            let (lo, hi) = world_mesh.bounding_box();
            eprintln!(
                "body '{name}': v={} f={} boundary_he={} (open={}) world_bbox=({:.3},{:.3},{:.3})..({:.3},{:.3},{:.3})",
                world_mesh.vertex_count(),
                world_mesh.face_count(),
                nb,
                nb > 0,
                lo.x, lo.y, lo.z, hi.x, hi.y, hi.z
            );
            bodies.push((name.clone(), world_mesh));
        }
        // Island census: the plane and cube may live in ONE body (joined).
        for (name, m) in &bodies {
            let mut face_island: HashMap<u32, usize> = HashMap::new();
            let mut n_islands = 0usize;
            for fi in 0..m.faces.len() as u32 {
                let f = FaceId(fi);
                if !m.is_face_live(f) || face_island.contains_key(&fi) {
                    continue;
                }
                let isl = n_islands;
                n_islands += 1;
                let mut stack = vec![f];
                while let Some(cf) = stack.pop() {
                    if face_island.insert(cf.0, isl).is_some() {
                        continue;
                    }
                    for he in m.face_half_edges(cf) {
                        let tw = m.he(he).twin;
                        if tw != NO_TWIN {
                            let nf = m.he(tw).face;
                            if nf != NO_FACE && !face_island.contains_key(&nf.0) {
                                stack.push(nf);
                            }
                        }
                    }
                }
            }
            let mut per: BTreeMap<usize, (usize, Vec3Stats)> = BTreeMap::new();
            #[derive(Default)]
            struct Vec3Stats {
                lo: Option<glam::Vec3>,
                hi: Option<glam::Vec3>,
            }
            for (fi, isl) in &face_island {
                let e = per.entry(*isl).or_default();
                e.0 += 1;
                for v in m.face_vertices(FaceId(*fi)) {
                    let p = m.vert(v).position;
                    e.1.lo = Some(e.1.lo.map_or(p, |l| l.min(p)));
                    e.1.hi = Some(e.1.hi.map_or(p, |h| h.max(p)));
                }
            }
            eprintln!("'{name}': {n_islands} island(s)");
            for (isl, (nf, st)) in &per {
                eprintln!(
                    "  island {isl}: {nf} faces bbox={:?}..{:?}",
                    st.lo.unwrap_or_default(),
                    st.hi.unwrap_or_default()
                );
            }
        }
        // The pie's edit-mode island path: sheet island = cutter, cube island = target.
        // Reproduce exactly what `poly_boolean_execute_cut`'s `run_cut` does.
        let (_, m) = &bodies[0];
        let mut sheet: HashSet<u32> = HashSet::new();
        let mut cube: HashSet<u32> = HashSet::new();
        for fi in 0..m.faces.len() as u32 {
            let f = FaceId(fi);
            if !m.is_face_live(f) {
                continue;
            }
            let open = m
                .face_half_edges(f)
                .iter()
                .any(|&h| m.he(h).twin == NO_TWIN);
            // island 1 = the single open sheet face; everything else = cube
            if open {
                sheet.insert(fi);
            } else {
                cube.insert(fi);
            }
        }
        let cutter = crate::poly::boolean::mesh_from_faces_diag(m, &sheet).expect("sheet mesh");
        let target = crate::poly::boolean::mesh_from_faces_diag(m, &cube).expect("cube mesh");
        eprintln!(
            "cutter(sheet): v={} f={} boundary={}  target(cube): v={} f={} boundary={}",
            cutter.vertex_count(),
            cutter.face_count(),
            boundary_half_edge_count(&cutter),
            target.vertex_count(),
            target.face_count(),
            boundary_half_edge_count(&target)
        );
        for (op, label) in [(BoolOp::Difference, "Diff"), (BoolOp::Intersect, "Inter")] {
            match mesh_boolean(&target, &cutter, op) {
                Some(r) => eprintln!(
                    "  {label}(cube by sheet) -> v={} f={} validate={:?} manifold={:?} boundary={}",
                    r.vertex_count(),
                    r.face_count(),
                    r.validate().err(),
                    r.is_manifold().err(),
                    boundary_half_edge_count(&r)
                ),
                None => eprintln!("  {label}(cube by sheet) -> NONE (kernel refused)"),
            }
        }
    }

    #[test]
    #[ignore]
    fn diag_owner_files() {
        for p in [
            r"C:\Users\Proprio\Desktop\BEVEL REGRSSION.fl4",
            r"C:\Users\Proprio\Desktop\KNIFE FAIL.fl4",
            r"C:\Users\Proprio\Desktop\FAIL SLICE.fl4",
        ] {
            report(p);
        }
    }
}
