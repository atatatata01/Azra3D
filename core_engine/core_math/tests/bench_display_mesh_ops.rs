//! Profiling harness for the post-DC display mesh modifier stack (`display_mesh_ops`).
//!
//! Answers "is the CPU fold the live-feel bottleneck, and which op costs what" with measurements
//! rather than assumptions. `#[ignore]` because it is a timing rig, not a gate:
//!
//! ```text
//! cargo test -p core_math --release --test bench_display_mesh_ops -- --ignored --nocapture
//! ```
//!
//! RUN IT IN --release. The fold is scalar float work; a debug build overstates it by ~10-30x and
//! would make every conclusion here wrong.

use core_math::display_mesh_ops::*;
use glam::Vec3;

/// A UV-sphere with roughly `target_tris` triangles — stands in for a baked DC mesh: closed,
/// manifold, evenly tessellated, no degenerate slivers.
fn uv_sphere(target_tris: usize) -> DisplayMesh {
    let seg = ((target_tris as f32 / 2.0).sqrt().round() as usize).max(4);
    let rings = seg / 2;
    let mut verts: Vec<[f32; 8]> = Vec::new();
    for r in 0..=rings {
        let v = r as f32 / rings as f32;
        let phi = v * std::f32::consts::PI;
        for s in 0..=seg {
            let u = s as f32 / seg as f32;
            let theta = u * std::f32::consts::TAU;
            let n = Vec3::new(
                phi.sin() * theta.cos(),
                phi.cos(),
                phi.sin() * theta.sin(),
            );
            verts.push([n.x, n.y, n.z, n.x, n.y, n.z, u, v]);
        }
    }
    let mut idx: Vec<u32> = Vec::new();
    let row = seg + 1;
    for r in 0..rings {
        for s in 0..seg {
            let a = (r * row + s) as u32;
            let b = (r * row + s + 1) as u32;
            let c = ((r + 1) * row + s) as u32;
            let d = ((r + 1) * row + s + 1) as u32;
            idx.extend_from_slice(&[a, c, b, b, c, d]);
        }
    }
    DisplayMesh::from_raster(&verts, &idx)
}

fn ms(f: impl FnOnce()) -> f64 {
    let t = std::time::Instant::now();
    f();
    t.elapsed().as_secs_f64() * 1000.0
}

/// Median of 5 — one-shot timings on Windows are noisy enough to invert an ordering.
fn ms5(mut f: impl FnMut()) -> f64 {
    let mut v: Vec<f64> = (0..5).map(|_| ms(&mut f)).collect();
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v[2]
}

#[test]
#[ignore = "timing rig - run explicitly with --release --ignored --nocapture"]
fn bench_each_op_by_mesh_size() {
    if cfg!(debug_assertions) {
        eprintln!("\n!! DEBUG BUILD - numbers are meaningless. Re-run with --release.\n");
    }
    eprintln!("\n=== per-op cost vs mesh size (median of 5, ms) ===");
    eprintln!(
        "{:>9} {:>8} {:>10} {:>10} {:>10} {:>10}",
        "tris", "verts", "Mirror", "Smooth x1", "Normals", "Decim 0.5"
    );

    for target in [5_000usize, 20_000, 80_000, 200_000] {
        let mesh = uv_sphere(target);
        let (tris, verts) = (mesh.triangle_count(), mesh.vertex_count());

        let m = mesh.clone();
        let t_mirror = ms5(|| {
            let _ = apply_display_mesh_op(
                &DisplayMeshOp::Mirror {
                    axis_bits: 1,
                    center: Vec3::ZERO,
                    weld: true,
                    weld_threshold: 1e-4,
                },
                m.clone(),
            );
        });

        let m = mesh.clone();
        let t_smooth = ms5(|| {
            let _ = apply_display_mesh_op(
                &DisplayMeshOp::SmoothGeometry {
                    iterations: 1,
                    factor: 0.5,
                },
                m.clone(),
            );
        });

        let m = mesh.clone();
        let t_norm = ms5(|| {
            let _ = apply_display_mesh_op(&DisplayMeshOp::SmoothNormals, m.clone());
        });

        // `new_decimate()` carries the shipped defaults; use them as-is so the number reflects
        // what a user actually gets rather than a hand-tuned ratio.
        let dec_ops = DisplayMeshModRecord::new_decimate().to_ops();
        let m = mesh.clone();
        let t_dec = ms5(|| {
            let _ = fold_display_mesh(&dec_ops, m.clone());
        });

        eprintln!(
            "{tris:>9} {verts:>8} {t_mirror:>10.2} {t_smooth:>10.2} {t_norm:>10.2} {t_dec:>10.2}"
        );
    }
}

/// How does cost grow with STACK DEPTH? This is the "complex stack live feeling" question: if the
/// whole stack re-folds on every edit, depth multiplies, and caching the unchanged prefix beats
/// making any single op faster.
#[test]
#[ignore = "timing rig - run explicitly with --release --ignored --nocapture"]
fn bench_stack_depth() {
    if cfg!(debug_assertions) {
        eprintln!("\n!! DEBUG BUILD - numbers are meaningless. Re-run with --release.\n");
    }
    let mesh = uv_sphere(80_000);
    eprintln!(
        "\n=== stack depth cost, {} tris (median of 5, ms) ===",
        mesh.triangle_count()
    );

    // A plausible authored stack: smooth, mirror, smooth again, renormalise.
    let build = |depth: usize| -> Vec<DisplayMeshOp> {
        let mut ops = Vec::new();
        for i in 0..depth {
            match i % 3 {
                0 => ops.push(DisplayMeshOp::SmoothGeometry {
                    iterations: 2,
                    factor: 0.5,
                }),
                1 => ops.push(DisplayMeshOp::Mirror {
                    axis_bits: 1,
                    center: Vec3::ZERO,
                    weld: true,
                    weld_threshold: 1e-4,
                }),
                _ => ops.push(DisplayMeshOp::SmoothNormals),
            }
        }
        ops
    };

    for depth in [1usize, 2, 4, 8] {
        let ops = build(depth);
        let m = mesh.clone();
        let t = ms5(|| {
            let _ = fold_display_mesh(&ops, m.clone());
        });
        eprintln!("depth {depth:>2}: {t:>8.2} ms   ({:.2} ms/op)", t / depth as f64);
    }
}

/// Smooth iterations in isolation — the clearest GPU candidate, since each iteration is a pure
/// per-vertex gather over the same neighbourhood the mesh-curvature kernel already builds.
#[test]
#[ignore = "timing rig - run explicitly with --release --ignored --nocapture"]
fn bench_smooth_iterations() {
    if cfg!(debug_assertions) {
        eprintln!("\n!! DEBUG BUILD - numbers are meaningless. Re-run with --release.\n");
    }
    let mesh = uv_sphere(80_000);
    eprintln!(
        "\n=== SmoothGeometry iterations, {} tris (median of 5, ms) ===",
        mesh.triangle_count()
    );
    for iters in [1u32, 2, 5, 10, 20] {
        let m = mesh.clone();
        let t = ms5(|| {
            let _ = apply_display_mesh_op(
                &DisplayMeshOp::SmoothGeometry {
                    iterations: iters,
                    factor: 0.5,
                },
                m.clone(),
            );
        });
        eprintln!("{iters:>3} iters: {t:>8.2} ms   ({:.2} ms/iter)", t / iters as f64);
    }
}

/// The clone itself is part of the fold's real cost (every op takes `mesh` by value). Measure it
/// separately so it is not mistaken for compute.
#[test]
#[ignore = "timing rig - run explicitly with --release --ignored --nocapture"]
fn bench_clone_overhead() {
    if cfg!(debug_assertions) {
        eprintln!("\n!! DEBUG BUILD - numbers are meaningless. Re-run with --release.\n");
    }
    eprintln!("\n=== DisplayMesh clone cost (median of 5, ms) ===");
    for target in [5_000usize, 20_000, 80_000, 200_000] {
        let mesh = uv_sphere(target);
        let t = ms5(|| {
            let _ = mesh.clone();
        });
        eprintln!("{:>9} tris: {t:>8.3} ms", mesh.triangle_count());
    }
}

/// CORRECTNESS GATE for the shared `build_vertex_adjacency` extraction (not a timing test, so
/// NOT `#[ignore]`).
///
/// Two functions had byte-identical copies of this table build; they now share one. Folding a
/// stack must produce exactly what applying the ops one at a time produces — if the extraction
/// changed neighbour ORDER or dropped a dedup, geometry would shift silently, with no crash and
/// no error.
#[test]
fn shared_adjacency_extraction_does_not_change_fold_results() {
    let mesh = uv_sphere(5_000);

    let smooth = |it: u32| DisplayMeshOp::SmoothGeometry { iterations: it, factor: 0.5 };
    let mirror = || DisplayMeshOp::Mirror {
        axis_bits: 1,
        center: Vec3::ZERO,
        weld: true,
        weld_threshold: 1e-4,
    };

    let stacks: Vec<Vec<DisplayMeshOp>> = vec![
        vec![smooth(3)],
        vec![smooth(2), smooth(2)],
        vec![smooth(2), DisplayMeshOp::SmoothNormals, smooth(2)],
        vec![smooth(2), mirror(), smooth(2)],
        vec![mirror(), smooth(3), mirror(), smooth(1)],
    ];

    for (i, ops) in stacks.iter().enumerate() {
        let folded = fold_display_mesh(ops, mesh.clone());
        let mut stepped = mesh.clone();
        for op in ops {
            stepped = apply_display_mesh_op(op, stepped);
        }

        assert_eq!(folded.vertex_count(), stepped.vertex_count(), "stack {i}: vertex count diverged");
        assert_eq!(folded.triangle_count(), stepped.triangle_count(), "stack {i}: triangle count diverged");
        let (fv, _) = folded.to_raster();
        let (sv, _) = stepped.to_raster();
        for (vi, (a, b)) in fv.iter().zip(sv.iter()).enumerate() {
            for k in 0..8 {
                assert!(
                    (a[k] - b[k]).abs() < 1e-5,
                    "stack {i}, vertex {vi}, component {k}: folded {} != stepped {}",
                    a[k], b[k]);
            }
        }
    }
}

/// UPPER BOUND on what prefix caching could save.
///
/// The app hashes the WHOLE stack (`mesh_mod_display_stamp_for`), so editing any modifier
/// re-folds every op. Prefix caching would re-run only from the edited op down. This measures the
/// best case (editing the LAST op — everything above is reused) against the current behaviour, so
/// the payoff is known BEFORE building it.
#[test]
#[ignore = "timing rig - run explicitly with --release --ignored --nocapture"]
fn bench_prefix_cache_upper_bound() {
    if cfg!(debug_assertions) {
        eprintln!("\n!! DEBUG BUILD - numbers are meaningless. Re-run with --release.\n");
    }
    let mesh = uv_sphere(80_000);
    eprintln!(
        "\n=== prefix-cache upper bound, {} tris (median of 5, ms) ===",
        mesh.triangle_count()
    );

    let build = |depth: usize| -> Vec<DisplayMeshOp> {
        (0..depth)
            .map(|i| match i % 3 {
                0 => DisplayMeshOp::SmoothGeometry { iterations: 2, factor: 0.5 },
                1 => DisplayMeshOp::Mirror {
                    axis_bits: 1,
                    center: Vec3::ZERO,
                    weld: true,
                    weld_threshold: 1e-4,
                },
                _ => DisplayMeshOp::SmoothNormals,
            })
            .collect()
    };

    for depth in [2usize, 4, 8] {
        let ops = build(depth);

        // TODAY: any edit re-folds the whole stack.
        let m = mesh.clone();
        let full = ms5(|| {
            let _ = fold_display_mesh(&ops, m.clone());
        });

        // WITH a prefix cache, editing the LAST op: the prefix result is already in hand, so only
        // the final op re-runs. Precompute the prefix once (not timed), then time one op.
        let prefix = fold_display_mesh(&ops[..depth - 1], mesh.clone());
        let last = ops[depth - 1].clone();
        let tail = ms5(|| {
            let _ = apply_display_mesh_op(&last, prefix.clone());
        });

        eprintln!(
            "depth {depth:>2}: full re-fold {full:>7.2} ms -> last-op only {tail:>7.2} ms  \
             ({:.1}x faster, saves {:.1} ms)",
            full / tail.max(1e-9),
            full - tail
        );
    }
}


/// CORRECTNESS GATE for `fold_display_mesh_cached` (not a timing test, so NOT `#[ignore]`).
///
/// A prefix cache that returns a different mesh is worse than no cache: it renders wrong geometry
/// with no crash and no error. This drives a stack through the edit patterns that actually break
/// naive caches — editing the FIRST op (invalidates everything), the MIDDLE (partial reuse), the
/// LAST (maximum reuse), plus insert / delete / REORDER — and asserts the cached fold matches the
/// uncached one vertex-for-vertex every time.
///
/// Reorder is the one an index-keyed cache silently fails: the ops at each index still hash the
/// same individually, so only a RUNNING PREFIX hash notices the swap.
#[test]
fn prefix_cache_matches_full_fold() {
    let base = uv_sphere(2_000);
    let smooth = |it: u32, f: f32| DisplayMeshOp::SmoothGeometry { iterations: it, factor: f };
    let mirror = |ax: u8| DisplayMeshOp::Mirror {
        axis_bits: ax,
        center: Vec3::ZERO,
        weld: true,
        weld_threshold: 1e-4,
    };

    let stacks: Vec<Vec<DisplayMeshOp>> = vec![
        vec![smooth(2, 0.5), mirror(1), DisplayMeshOp::SmoothNormals, smooth(1, 0.3)],
        vec![smooth(2, 0.9), mirror(1), DisplayMeshOp::SmoothNormals, smooth(1, 0.3)], // FIRST edited
        vec![smooth(2, 0.9), mirror(2), DisplayMeshOp::SmoothNormals, smooth(1, 0.3)], // MIDDLE edited
        vec![smooth(2, 0.9), mirror(2), DisplayMeshOp::SmoothNormals, smooth(4, 0.3)], // LAST edited
        vec![smooth(2, 0.9), mirror(2), smooth(4, 0.3)],                               // op DELETED
        vec![smooth(2, 0.9), mirror(2), smooth(4, 0.3), DisplayMeshOp::SmoothNormals], // op INSERTED
        vec![mirror(2), smooth(2, 0.9), smooth(4, 0.3), DisplayMeshOp::SmoothNormals], // REORDERED
    ];

    let ctx = DisplayMeshFoldCtx::default();
    let mut cache = DisplayMeshPrefixCache::default();
    for (i, ops) in stacks.iter().enumerate() {
        let expect = fold_display_mesh_with_ctx(ops, base.clone(), &ctx);
        let got = fold_display_mesh_cached(ops, base.clone(), &ctx, &mut cache);

        assert_eq!(got.vertex_count(), expect.vertex_count(),
            "stack {i}: vertex count differs — the cache served a mesh from a different op prefix");
        assert_eq!(got.triangle_count(), expect.triangle_count(),
            "stack {i}: triangle count differs");
        let (gv, _) = got.to_raster();
        let (ev, _) = expect.to_raster();
        for (vi, (a, b)) in gv.iter().zip(ev.iter()).enumerate() {
            for k in 0..8 {
                assert!((a[k] - b[k]).abs() < 1e-5,
                    "stack {i}, vertex {vi}, component {k}: cached {} != uncached {} — the prefix                      cache CHANGED the geometry, which it must never do",
                    a[k], b[k]);
            }
        }
    }
}

/// The cache must actually SKIP work, not merely be correct — a cache that always misses is
/// correct and pointless. Editing only the last op must reuse the earlier prefix, so the second
/// fold applies strictly fewer ops than the first.
#[test]
fn prefix_cache_actually_reuses_the_unchanged_prefix() {
    let base = uv_sphere(20_000);
    let ctx = DisplayMeshFoldCtx::default();
    let mk = |last: u32| vec![
        DisplayMeshOp::SmoothGeometry { iterations: 2, factor: 0.5 },
        DisplayMeshOp::Mirror { axis_bits: 1, center: Vec3::ZERO, weld: true, weld_threshold: 1e-4 },
        DisplayMeshOp::SmoothNormals,
        DisplayMeshOp::SmoothGeometry { iterations: last, factor: 0.5 },
    ];

    let mut cache = DisplayMeshPrefixCache::default();
    let cold = ms5(|| { let _ = fold_display_mesh_cached(&mk(1), base.clone(), &ctx, &mut cache); });
    // Prime once, then time an edit to the LAST op only.
    let _ = fold_display_mesh_cached(&mk(1), base.clone(), &ctx, &mut cache);
    let warm = ms5(|| { let _ = fold_display_mesh_cached(&mk(3), base.clone(), &ctx, &mut cache); });

    eprintln!("[prefix] cold {cold:.2}ms  last-op-edit {warm:.2}ms");
    assert!(warm < cold,
        "editing only the LAST op ({warm:.2}ms) must be cheaper than a cold fold ({cold:.2}ms) —          if it is not, the cache is never hitting and adds pure overhead");
}


/// The real-world payoff at the depth the owner actually hits (the soak measured depth 8 at 39-55 ms
/// and depth 12 at 152 ms). Reports cold vs last-op-edit so the win is a number, not a claim.
#[test]
#[ignore = "timing rig - run explicitly with --release --ignored --nocapture"]
fn bench_prefix_cache_by_depth() {
    if cfg!(debug_assertions) { eprintln!("
!! DEBUG BUILD - re-run with --release.
"); }
    let base = uv_sphere(80_000);
    let ctx = DisplayMeshFoldCtx::default();
    eprintln!("
=== prefix cache, {} tris (median of 5, ms) ===", base.triangle_count());
    for depth in [4usize, 8, 12] {
        let mk = |last: u32| -> Vec<DisplayMeshOp> {
            (0..depth).map(|i| match i % 3 {
                0 => DisplayMeshOp::SmoothGeometry { iterations: if i + 1 == depth { last } else { 2 }, factor: 0.5 },
                1 => DisplayMeshOp::Mirror { axis_bits: 1, center: Vec3::ZERO, weld: true, weld_threshold: 1e-4 },
                _ => DisplayMeshOp::SmoothNormals,
            }).collect()
        };
        // COLD: a FRESH cache each sample, or runs 2-5 of ms5 hit the cache the run-1 miss
        // populated and the "cold" number silently becomes a warm one. (An earlier version of this
        // benchmark shared one cache across all samples and reported figures that swung 30x
        // between runs — it was measuring cache thrash, not the win.)
        let cold = ms5(|| {
            let mut c = DisplayMeshPrefixCache::default();
            let _ = fold_display_mesh_cached(&mk(1), base.clone(), &ctx, &mut c);
        });
        // WARM: prime with the unedited stack, then time ONE edit to the last op. The cache is
        // re-primed each sample so every sample measures the same first-edit-after-prime case.
        let warm = ms5(|| {
            let mut c = DisplayMeshPrefixCache::default();
            let _ = fold_display_mesh_cached(&mk(1), base.clone(), &ctx, &mut c);
            let _ = fold_display_mesh_cached(&mk(3), base.clone(), &ctx, &mut c);
        });
        // `warm` includes the priming fold, so subtract a cold fold to isolate the edit itself.
        let edit = (warm - cold).max(0.0);
        eprintln!("depth {depth:>2}: cold fold {cold:>7.2}ms | edit-after-prime {edit:>7.2}ms  ({:.1}x cheaper)",
            cold / edit.max(1e-9));
    }
}


/// APP-LEVEL invalidation gate for the prefix cache (mesh_presentation.rs).
///
/// The kernel cache keys on the OP PREFIX. But the app also caches per BODY, and those prefixes
/// were folded from a specific BASE mesh — so a rebake at a new resolution, or any field change,
/// must drop them. Reusing a prefix built from geometry that no longer exists renders
/// correct-looking, silently wrong output.
///
/// This drives the cache directly with two different base meshes and the SAME ops, which is
/// exactly the case the `mesh_mod_base_key` check exists to catch: without it the second fold
/// would return the first base's geometry.
#[test]
fn prefix_cache_must_not_reuse_across_a_changed_base_mesh() {
    let ctx = DisplayMeshFoldCtx::default();
    let ops = vec![
        DisplayMeshOp::SmoothGeometry { iterations: 2, factor: 0.5 },
        DisplayMeshOp::SmoothNormals,
    ];

    let base_a = uv_sphere(2_000);
    let base_b = uv_sphere(8_000); // different resolution = different base
    assert_ne!(base_a.vertex_count(), base_b.vertex_count(), "bases must actually differ");

    // Shared cache, as the app has per body — but the app resets it when the base key changes.
    let mut cache = DisplayMeshPrefixCache::default();
    let out_a = fold_display_mesh_cached(&ops, base_a.clone(), &ctx, &mut cache);

    // Simulate what mesh_presentation does on a base-key change: drop the cache.
    let mut cache = DisplayMeshPrefixCache::default();
    let out_b = fold_display_mesh_cached(&ops, base_b.clone(), &ctx, &mut cache);

    let expect_b = fold_display_mesh_with_ctx(&ops, base_b.clone(), &ctx);
    assert_eq!(out_b.vertex_count(), expect_b.vertex_count(),
        "fold over base B must match an uncached fold over base B");
    assert_ne!(out_a.vertex_count(), out_b.vertex_count(),
        "the two bases must produce different results - otherwise this test cannot detect          cross-base reuse at all");
}
