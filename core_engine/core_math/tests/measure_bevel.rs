//! Live-like bevel cost (modifier stack = all edges, every slider frame).
//!
//! ```
//! cargo test -p core_math --release --test measure_bevel -- --ignored --nocapture
//! ```

use core_math::bevel_op::{bevel_edges_offset, bevel_edges_offset_mut, BevelParams};
use core_math::half_edge::{HalfEdgeId, HalfEdgeMesh, NO_FACE};
use glam::Vec3;
use std::collections::HashSet;

fn med5(mut f: impl FnMut()) -> f64 {
    let mut v: Vec<f64> = (0..5)
        .map(|_| {
            let t = std::time::Instant::now();
            f();
            t.elapsed().as_secs_f64() * 1e3
        })
        .collect();
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v[2]
}

fn all_edges(m: &HalfEdgeMesh) -> HashSet<u32> {
    let mut s = HashSet::new();
    for i in 0..m.half_edges.len() as u32 {
        if m.he(HalfEdgeId(i)).face != NO_FACE {
            s.insert(i);
        }
    }
    s
}

fn one_loop(m: &HalfEdgeMesh) -> HashSet<u32> {
    for i in 0..m.half_edges.len() as u32 {
        let he = HalfEdgeId(i);
        if m.he(he).face == NO_FACE {
            continue;
        }
        let loop_e = m.select_edge_loop(he);
        if loop_e.len() >= 4 {
            return loop_e.iter().map(|h| h.0).collect();
        }
    }
    all_edges(m)
}

fn subdiv_n(mut m: HalfEdgeMesh, n: u32) -> HalfEdgeMesh {
    for _ in 0..n {
        let faces: HashSet<u32> = (0..m.faces.len() as u32)
            .filter(|&fi| m.is_face_live(core_math::half_edge::FaceId(fi)))
            .collect();
        m.subdivide_faces(&faces);
    }
    m
}

fn mesh_label(m: &HalfEdgeMesh) -> String {
    format!("{}v/{}f", m.vertex_count(), m.face_count())
}

/// What a Bevel-modifier slider frame pays: clone + all-edge offset (apply_one).
#[test]
#[ignore]
fn measure_bevel_live_frames() {
    let fixtures: Vec<(&str, HalfEdgeMesh)> = vec![
        ("cube", HalfEdgeMesh::new_box(Vec3::splat(0.5))),
        ("cube-sub2", subdiv_n(HalfEdgeMesh::new_box(Vec3::splat(0.5)), 2)),
        ("cube-sub3", subdiv_n(HalfEdgeMesh::new_box(Vec3::splat(0.5)), 3)),
        ("sphere-24x16", HalfEdgeMesh::new_uv_sphere(0.5, 24, 16)),
    ];
    for (name, mesh) in &fixtures {
        let all = all_edges(mesh);
        let loop_e = one_loop(mesh);
        eprintln!(
            "[BEVEL MESH] {name:<14} {}  all_he={} loop_he={}",
            mesh_label(mesh),
            all.len(),
            loop_e.len()
        );
        for segs in [1u32, 3, 5] {
            let p = BevelParams {
                width: 0.02,
                segments: segs,
                shape: 0.5,
                clamp: true,
            };
            let t_off = med5(|| {
                let _ = bevel_edges_offset(mesh, &all, &p);
            });
            let t_mut = med5(|| {
                let mut m = mesh.clone();
                bevel_edges_offset_mut(&mut m, &all, &p, None);
            });
            let t_loop = med5(|| {
                let _ = bevel_edges_offset(mesh, &loop_e, &p);
            });
            eprintln!(
                "[BEVEL LIVE] {name:<14} segs={segs}  all+clone={t_off:>7.2}ms  \
                 all+mut(incl clone)={t_mut:>7.2}ms  loop={t_loop:>7.2}ms"
            );
        }
    }
}

/// Width sweep: same as dragging the bevel slider (re-eval every notch).
#[test]
#[ignore]
fn measure_bevel_width_scrub() {
    let mesh = subdiv_n(HalfEdgeMesh::new_box(Vec3::splat(0.5)), 2);
    let all = all_edges(&mesh);
    let p = |w: f32| BevelParams {
        width: w,
        segments: 3,
        shape: 0.5,
        clamp: true,
    };
    let mut widths = Vec::new();
    let mut w = 0.005f32;
    while w <= 0.08 {
        widths.push(w);
        w += 0.005;
    }
    let per = med5(|| {
        for &wi in &widths {
            let _ = bevel_edges_offset(&mesh, &all, &p(wi));
        }
    });
    eprintln!(
        "[BEVEL SCRUB] cube-sub2 {} segs=3  {} widths  total={per:.1}ms  per-notch={:.2}ms",
        mesh_label(&mesh),
        widths.len(),
        per / widths.len() as f64
    );
}

/// Destructive Ctrl+B: each drag frame bevels the **selection** from the
/// session `original` (`poly_update_bevel_drag` → `poly_bevel_regenerate` →
/// `bevel_edges_offset`). Empty selection = all live edges (Modo implicit).
/// SHIFT consume tries fuse, then wedge-cut, then absorb+offset.
#[test]
#[ignore]
fn measure_bevel_destructive_drag() {
    let fixtures: Vec<(&str, HalfEdgeMesh)> = vec![
        ("cube", HalfEdgeMesh::new_box(Vec3::splat(0.5))),
        ("cube-sub2", subdiv_n(HalfEdgeMesh::new_box(Vec3::splat(0.5)), 2)),
        ("cube-sub3", subdiv_n(HalfEdgeMesh::new_box(Vec3::splat(0.5)), 3)),
        ("sphere-24x16", HalfEdgeMesh::new_uv_sphere(0.5, 24, 16)),
    ];
    for (name, mesh) in &fixtures {
        let all = all_edges(mesh);
        let loop1 = one_loop(mesh);
        let mut loops = loop1.clone();
        // Second loop: pick a half-edge not in the first loop.
        for i in 0..mesh.half_edges.len() as u32 {
            if loops.contains(&i) || mesh.he(HalfEdgeId(i)).face == NO_FACE {
                continue;
            }
            let extra = mesh.select_edge_loop(HalfEdgeId(i));
            if extra.len() >= 4 {
                for h in extra {
                    loops.insert(h.0);
                }
                break;
            }
        }
        let corner: HashSet<u32> = {
            let mut s = HashSet::new();
            if let Some(&e0) = loop1.iter().next() {
                s.insert(e0);
                let he = HalfEdgeId(e0);
                // Two more edges at one endpoint.
                let v = mesh.he(he).vertex;
                for h in mesh.vertex_fan(v).into_iter().take(3) {
                    s.insert(h.0);
                }
            }
            s
        };
        eprintln!(
            "[BEVEL DESTR MESH] {name:<14} {}  loop={} two_loops={} corner={} all={}",
            mesh_label(mesh),
            loop1.len(),
            loops.len(),
            corner.len(),
            all.len()
        );
        for (sel_name, sel) in [
            ("1-loop", &loop1),
            ("2-loops", &loops),
            ("corner", &corner),
            ("all-edge", &all),
        ] {
            for segs in [1u32, 3, 5] {
                let p = BevelParams {
                    width: 0.02,
                    segments: segs,
                    shape: 0.5,
                    clamp: true,
                };
                let t = med5(|| {
                    let _ = bevel_edges_offset(mesh, sel, &p);
                });
                eprintln!(
                    "[BEVEL DESTR] {name:<14} {sel_name:<8} segs={segs}  from_original={t:>7.2}ms  sel_he={}",
                    sel.len()
                );
            }
        }
        let verts: HashSet<u32> = [0u32].into_iter().collect();
        for segs in [1u32, 3, 5] {
            let p = BevelParams {
                width: 0.02,
                segments: segs,
                shape: 0.5,
                clamp: true,
            };
            let t = med5(|| {
                let _ = core_math::bevel_op::bevel_vertices_offset(mesh, &verts, &p);
            });
            eprintln!(
                "[BEVEL DESTR] {name:<14} vtx-1    segs={segs}  from_original={t:>7.2}ms"
            );
        }
        // Consume (SHIFT) on a typical loop — same fallback chain as regenerate.
        let p = BevelParams {
            width: 0.02,
            segments: 3,
            shape: 0.5,
            clamp: true,
        };
        let t_fuse = med5(|| {
            let _ = core_math::bevel_absorb::consume_fuse_bevel(mesh, &loop1, &p);
        });
        let t_wedge = med5(|| {
            let _ = core_math::bevel_absorb::consume_wedge_cut(mesh, &loop1, &p);
        });
        let t_absorb = med5(|| {
            let _ = core_math::bevel_absorb::absorb_for_bevel(mesh, &loop1, p.width);
        });
        let t_consume = med5(|| {
            if core_math::bevel_absorb::consume_fuse_bevel(mesh, &loop1, &p).is_some() {
                return;
            }
            if core_math::bevel_absorb::consume_wedge_cut(mesh, &loop1, &p).is_some() {
                return;
            }
            match core_math::bevel_absorb::absorb_for_bevel(mesh, &loop1, p.width) {
                Some(absorbed) => {
                    let live: HashSet<u32> = loop1
                        .iter()
                        .copied()
                        .filter(|&e| {
                            (e as usize) < absorbed.half_edges.len()
                                && absorbed.is_edge_live(HalfEdgeId(e))
                        })
                        .collect();
                    let _ = bevel_edges_offset(&absorbed, &live, &p);
                }
                None => {
                    let _ = bevel_edges_offset(mesh, &loop1, &p);
                }
            }
        });
        eprintln!(
            "[BEVEL DESTR] {name:<14} consume  segs=3  chain={t_consume:>7.2}ms  \
             fuse={t_fuse:>7.2}ms  wedge={t_wedge:>7.2}ms  absorb={t_absorb:>7.2}ms"
        );
    }

    // Width scrub from original (Ctrl+B drag): same selection, many widths, no topo reuse.
    let mesh = subdiv_n(HalfEdgeMesh::new_box(Vec3::splat(0.5)), 2);
    let loop1 = one_loop(&mesh);
    let all = all_edges(&mesh);
    let mut widths = Vec::new();
    let mut w = 0.005f32;
    while w <= 0.08 {
        widths.push(w);
        w += 0.005;
    }
    let p = |wi: f32| BevelParams {
        width: wi,
        segments: 3,
        shape: 0.5,
        clamp: true,
    };
    let per_loop = med5(|| {
        for &wi in &widths {
            let _ = bevel_edges_offset(&mesh, &loop1, &p(wi));
        }
    });
    let per_all = med5(|| {
        for &wi in &widths {
            let _ = bevel_edges_offset(&mesh, &all, &p(wi));
        }
    });
    eprintln!(
        "[BEVEL DESTR SCRUB] cube-sub2 {} segs=3  {} widths  \
         1-loop total={per_loop:.1}ms per={:.2}ms  \
         all-edge total={per_all:.1}ms per={:.2}ms",
        mesh_label(&mesh),
        widths.len(),
        per_loop / widths.len() as f64,
        per_all / widths.len() as f64
    );

    // Remesh incremental can only fire if from_polygons keeps slot counts AND
    // index-aligned positions (untouched verts stay put). This is that probe.
    let mut prev: Option<HalfEdgeMesh> = None;
    for (i, &wi) in widths.iter().enumerate() {
        let out = bevel_edges_offset(&mesh, &loop1, &p(wi));
        if let Some(pr) = &prev {
            let same = pr.vertices.len() == out.vertices.len()
                && pr.half_edges.len() == out.half_edges.len()
                && pr.faces.len() == out.faces.len();
            let moved = if same {
                pr.vertices
                    .iter()
                    .zip(&out.vertices)
                    .filter(|(a, b)| a.position != b.position)
                    .count()
            } else {
                out.vertices.len()
            };
            eprintln!(
                "[BEVEL ID] cube-sub2 1-loop segs=3 w={wi:.3} f{i}  \
                 same_slots={}  {}v/{}he/{}f  moved={moved}/{}",
                u8::from(same),
                out.vertices.len(),
                out.half_edges.len(),
                out.faces.len(),
                out.vertices.len()
            );
        }
        prev = Some(out);
    }
}
