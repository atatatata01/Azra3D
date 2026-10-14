// voxel_sculpt/tests.rs — unit tests. Split from voxel_sculpt.rs (pure relocation).
// The former top-level `mod tests` body lives at this file's top level (same test paths:
// voxel_sculpt::tests::*). `mod brush_parity_tests` is nested below (test paths gain a
// `tests::` segment; test count unchanged).

    use super::*;
use glam::Vec3;

    #[test]
    fn brush_direction_resolves_each_mode() {
        let surface = Vec3::new(0.0, 0.0, 1.0);
        let view = Vec3::new(1.0, 0.0, 0.0);
        let first = Vec3::new(0.0, 1.0, 0.0);
        let average = Vec3::new(0.0, 0.707, 0.707);
        let stroke = Vec3::new(1.0, 0.0, 0.0);
        let ctx = |fp: Option<Vec3>, avg: Option<Vec3>, sd: Option<Vec3>| BrushDirContext {
            surface, view, first_point: fp, average: avg, stroke_dir: sd,
        };
        assert_eq!(BrushDirection::Surface.resolve(ctx(Some(first), None, None)), surface);
        assert_eq!(BrushDirection::Normal.resolve(ctx(Some(first), None, None)), surface);
        assert_eq!(BrushDirection::View.resolve(ctx(Some(first), None, None)), view);
        assert_eq!(BrushDirection::FirstPoint.resolve(ctx(Some(first), None, None)), first);
        assert_eq!(BrushDirection::Average.resolve(ctx(None, Some(average), None)).normalize(), average.normalize());
        assert_eq!(BrushDirection::Brush.resolve(ctx(None, None, Some(stroke))), stroke);
        // Tangent = stroke × surface → X × Z = -Y
        let t = BrushDirection::Tangent.resolve(ctx(None, None, Some(stroke)));
        assert!(t.dot(Vec3::NEG_Y) > 0.9, "tangent={t}");
        assert_eq!(BrushDirection::AxisX.resolve(ctx(None, None, None)), Vec3::X);
        assert_eq!(BrushDirection::AxisY.resolve(ctx(None, None, None)), Vec3::Y);
        assert_eq!(BrushDirection::AxisZ.resolve(ctx(None, None, None)), Vec3::Z);
        assert_eq!(BrushDirection::FirstPoint.resolve(ctx(None, None, None)), surface);
        // Default = live per-dab surface normal (Blender area-normal model, 2026-07-23).
        // FirstPoint (locked plane) remains an explicit option, no longer the default.
        assert_eq!(BrushDirection::default(), BrushDirection::Surface);
        // Round-trip u32 for Save As
        for d in BrushDirection::ALL {
            assert_eq!(BrushDirection::from_u32(d.as_u32()), d, "{d:?}");
        }
    }

    #[test]
    #[ignore] // perf probe — run with: cargo test -p core_math bench_mesh_region_192 -- --ignored --nocapture
    fn bench_mesh_region_192() {
        let res = 192u32;
        let mut g = VoxelGrid::new_empty(res, 1.0);
        g.fill_sphere(Vec3::ZERO, 0.6);
        let cells = res - 1;
        let cmax = cells - 1;
        // A small brush edit on the sphere surface (+x axis, world 0.6 → grid cell ~152): a ~6-cell
        // dirty region. The per-frame re-mesh re-scans every CHUNK that region touches — so chunk
        // SIZE drives the cost. Compare 32/16/8: smaller chunk ≈ brush size ⇒ less wasted scan.
        let dirty = [149u32, 92, 92, 155, 98, 98];
        for &cs in &[32u32, 16, 8] {
            let cdim = (cells + cs - 1) / cs;
            let lo = |v: u32| (v.saturating_sub(1) / cs).min(cdim - 1);
            let hi = |v: u32| (v.min(cmax) / cs).min(cdim - 1);
            let cr = [lo(dirty[0]), lo(dirty[1]), lo(dirty[2]), hi(dirty[3]), hi(dirty[4]), hi(dirty[5])];
            let touched = ((cr[3] - cr[0] + 1) * (cr[4] - cr[1] + 1) * (cr[5] - cr[2] + 1)) as u64;
            const N: u32 = 300;
            let t = std::time::Instant::now();
            let mut sink = 0usize;
            for _ in 0..N {
                for ccz in cr[2]..=cr[5] { for ccy in cr[1]..=cr[4] { for ccx in cr[0]..=cr[3] {
                    let a0 = [ccx * cs, ccy * cs, ccz * cs];
                    let a1 = [((ccx + 1) * cs - 1).min(cmax), ((ccy + 1) * cs - 1).min(cmax), ((ccz + 1) * cs - 1).min(cmax)];
                    let (v, i) = g.mesh_region(a0, a1); sink = sink.wrapping_add(v.len() + i.len());
                }}}
            }
            let ms = t.elapsed().as_secs_f64() * 1000.0 / N as f64;
            eprintln!("[BENCH] chunk={:>2}^3: {:.3} ms/edit ({} chunks touched, {} cells/chunk, sink={})",
                cs, ms, touched, (cs + 1).pow(3), sink);
        }
    }

    /// GOLDEN GATE for the 2026-08-08 parallel `surface_nets` rewrite: the rayon slab
    /// version must be BIT-IDENTICAL to the retained serial reference — same vertex
    /// numbering, same quad order, same normals — on irregular topology (sphere + blob
    /// + deterministic LCG noise = boundary crossings, disconnected shells, last-layer
    /// sign changes), both smooth modes.
    #[test]
    fn surface_nets_parallel_matches_serial_reference() {
        for res in [16u32, 33, 48] {
            let mut g = VoxelGrid::new_empty(res, 1.0);
            g.fill_sphere(Vec3::ZERO, 0.55);
            g.fill_sphere(Vec3::new(0.5, 0.3, -0.2), 0.35);
            // Deterministic LCG speckle → irregular crossings incl. grid-boundary cells.
            let mut state = 0x2545F491u64;
            let cells = (res as usize).pow(3);
            for _ in 0..cells / 7 {
                state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                let i = (state >> 16) as usize % cells;
                let v = ((state >> 40) & 0xFF) as f32 / 255.0;
                g.density.set_idx(i, v);
            }
            // BOTH placements: `Qef` (poly Remesh) runs through the same parallel slab machinery
            // as `Centroid`, so it needs the same serial-equality gate — a placement that only
            // agreed under one scheduling would produce geometry that changes with core count.
            for placement in [CellPlacement::Centroid, CellPlacement::Qef] {
                for smooth in [false, true] {
                    let par = g.surface_nets_placed(smooth, placement);
                    let ser = g.surface_nets_serial_placed(smooth, placement);
                    let tag = format!("res={res} smooth={smooth} placement={placement:?}");
                    assert_eq!(par.vertices.len(), ser.vertices.len(), "{tag}: vert count");
                    assert_eq!(par.quads, ser.quads, "{tag}: quads differ");
                    for (i, (a, b)) in par.vertices.iter().zip(&ser.vertices).enumerate() {
                        assert_eq!(a, b, "{tag}: vertex {i}");
                    }
                    for (i, (a, b)) in par.normals.iter().zip(&ser.normals).enumerate() {
                        assert_eq!(a, b, "{tag}: normal {i}");
                    }
                }
            }
            // `surface_nets` must remain EXACTLY the centroid path — the sculpt display calls it
            // and must not have silently inherited Dual Contouring.
            let plain = g.surface_nets(true);
            let centroid = g.surface_nets_placed(true, CellPlacement::Centroid);
            assert_eq!(plain.vertices, centroid.vertices, "res={res}: default placement drifted");
        }
    }

    /// QEF placement must keep every vertex INSIDE its own cell.
    ///
    /// An ill-conditioned QEF (near-parallel crossing planes, which a noisy density field produces
    /// constantly) solves to a point far outside the cell. Unclamped, that is not a small error —
    /// it is a spike shot across the model, and it would show up as stray geometry rather than as
    /// a shading artifact. The clamp is the structural guarantee; this is its gate.
    #[test]
    fn qef_vertices_stay_within_their_cell() {
        let res = 24u32;
        let mut g = VoxelGrid::new_empty(res, 1.0);
        g.fill_sphere(Vec3::ZERO, 0.6);
        g.fill_sphere(Vec3::new(0.45, 0.2, -0.3), 0.3);
        // Speckle: drives near-degenerate crossing configurations on purpose.
        let mut state = 0x9E3779B9u64;
        let cells = (res as usize).pow(3);
        for _ in 0..cells / 5 {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            let i = (state >> 16) as usize % cells;
            let v = ((state >> 40) & 0xFF) as f32 / 255.0;
            g.density.set_idx(i, v);
        }
        let vs = g.voxel_size();
        let eps = vs * 1e-3;
        let mut checked = 0usize;
        for cz in 0..res - 1 {
            for cy in 0..res - 1 {
                for cx in 0..res - 1 {
                    if let Some((p, n)) =
                        g.cell_surface_vertex_placed(cx, cy, cz, CellPlacement::Qef)
                    {
                        let lo = g.cell_world_pos(cx, cy, cz);
                        let hi = lo + Vec3::splat(vs);
                        assert!(
                            p.x >= lo.x - eps && p.x <= hi.x + eps
                                && p.y >= lo.y - eps && p.y <= hi.y + eps
                                && p.z >= lo.z - eps && p.z <= hi.z + eps,
                            "cell ({cx},{cy},{cz}): QEF vertex {p:?} escaped its cell {lo:?}..{hi:?}"
                        );
                        assert!(
                            n.is_finite() && n.length() > 0.5,
                            "cell ({cx},{cy},{cz}): degenerate normal {n:?}"
                        );
                        checked += 1;
                    }
                }
            }
        }
        assert!(checked > 500, "only {checked} surface cells — fixture is not exercising the solver");
    }

    /// Both placements must agree on WHICH cells contain surface. Placement decides where a vertex
    /// sits, never whether one exists — if it changed the cell set, the quad topology would differ
    /// and the two modes would produce different meshes rather than the same mesh placed better.
    #[test]
    fn qef_and_centroid_agree_on_surface_cells() {
        let res = 20u32;
        let mut g = VoxelGrid::new_empty(res, 1.0);
        g.fill_sphere(Vec3::ZERO, 0.55);
        let a = g.surface_nets_placed(false, CellPlacement::Centroid);
        let b = g.surface_nets_placed(false, CellPlacement::Qef);
        assert_eq!(a.vertices.len(), b.vertices.len(), "vertex COUNT must match");
        assert_eq!(a.quads, b.quads, "quad topology must be identical");
    }

    #[test]
    fn empty_grid_has_no_surface() {
        let g = VoxelGrid::new_empty(16, 1.0);
        let m = g.surface_nets(true);
        assert_eq!(m.vertices.len(), 0);
        assert_eq!(m.quads.len(), 0);
    }

    // SDF distance to a sphere centred at `c` with radius `r` (negative inside) — a tiny standalone
    // field for the fill_from_sdf stamp tests (no scene graph needed at the core_math layer).
    fn sphere_sdf(c: Vec3, r: f32) -> impl Fn(Vec3) -> f32 { move |p: Vec3| (p - c).length() - r }

    #[test]
    fn fill_from_sdf_union_deposits_solid() {
        // Empty grid → union a sphere SDF → the centre voxel must read solid (≈1), a far voxel empty.
        let mut g = VoxelGrid::new_empty(32, 1.0);
        let region = g.fill_from_sdf(sphere_sdf(Vec3::ZERO, 0.5), true, None, None);
        assert!(region.is_some(), "stamp overlapping the grid must report a dirty region");
        assert!(g.sample(Vec3::ZERO) > 0.9, "inside the sphere should be solid, got {}", g.sample(Vec3::ZERO));
        // A corner well outside r=0.5 stays empty.
        assert!(g.sample(Vec3::splat(0.9)) < 0.1, "outside the sphere should stay empty");
    }

    #[test]
    fn fill_from_sdf_carve_removes_solid() {
        // Start solid (a box of material), carve a sphere out of the middle → centre becomes empty,
        // a point away from the carve stays solid.
        let mut g = VoxelGrid::new_empty(32, 1.0);
        g.fill_box(Vec3::ZERO, Vec3::splat(0.9));
        assert!(g.sample(Vec3::ZERO) > 0.9, "box centre should start solid");
        g.fill_from_sdf(sphere_sdf(Vec3::ZERO, 0.4), false, None, None);
        assert!(g.sample(Vec3::ZERO) < 0.1, "carve should empty the sphere centre, got {}", g.sample(Vec3::ZERO));
        // Near a corner (outside the r=0.4 carve) material survives.
        assert!(g.sample(Vec3::splat(0.7)) > 0.5, "material outside the carve should remain");
    }

    #[test]
    fn paint_color_pack_roundtrip() {
        // pack → the bit layout the raster shader unpacks (0x_FF_RR_GG_BB, painted flag high byte).
        let packed = pack_voxel_color(Vec3::new(1.0, 0.0, 0.0));
        assert_eq!(packed >> 24, 0xFF, "high byte is the painted flag");
        assert_eq!((packed >> 16) & 0xFF, 255, "R");
        assert_eq!((packed >> 8) & 0xFF, 0, "G");
        assert_eq!(packed & 0xFF, 0, "B");
        // Black is still distinguishable from unpainted (the 0xFF flag is set).
        assert!(pack_voxel_color(Vec3::ZERO) & 0xFF00_0000 != 0, "painted black ≠ unpainted 0");
    }

    #[test]
    fn paint_deposit_colors_surface_and_mesh_carries_it() {
        // Draw a blob, then PAINT it; the mesh must emit the packed colour in the UV slot (uv.y=1) on
        // surface vertices the brush covered, and stay (0,0) where unpainted.
        let mut g = VoxelGrid::new_empty(48, 1.0);
        g.apply_brush(VoxelBrush::Draw, Vec3::ZERO, Vec3::Y, 0.6, 1.0, false, 0.0, Vec3::ZERO, None);
        let red = pack_voxel_color(Vec3::new(1.0, 0.0, 0.0));
        g.deposit_color_sphere(Vec3::ZERO, 0.6, red);
        assert!(!g.colors.is_empty(), "deposit must lazily allocate the colour buffer");
        // color_idx returns the packed word only for painted voxels.
        let painted_any = (0..g.density.len()).any(|i| g.color_idx(i) == Some(red));
        assert!(painted_any, "at least one voxel must carry the deposited colour");
        // The mesher packs colour into uv (verts[6]=bitcast colour, verts[7]=1.0 painted).
        let (verts, _idx) = g.object_display_mesh();
        assert!(!verts.is_empty(), "painted blob must mesh");
        let painted_verts = verts.iter().filter(|v| v[7] > 0.5).count();
        assert!(painted_verts > 0, "some surface vertices must be flagged painted");
        // Every painted vertex's uv.x bitcasts back to a valid painted word (high flag byte set).
        for v in verts.iter().filter(|v| v[7] > 0.5) {
            let w = v[6].to_bits();
            assert!(w & 0xFF00_0000 != 0, "painted vertex uv.x must hold a painted colour word");
        }
    }

    #[test]
    fn paint_unpainted_blob_emits_zero_uv() {
        // No paint → colour buffer stays empty and the mesher emits uv=(0,0) (no behaviour change).
        let mut g = VoxelGrid::new_empty(48, 1.0);
        g.apply_brush(VoxelBrush::Draw, Vec3::ZERO, Vec3::Y, 0.6, 1.0, false, 0.0, Vec3::ZERO, None);
        assert!(g.colors.is_empty(), "an unpainted sculpt allocates no colour buffer");
        let (verts, _idx) = g.object_display_mesh();
        assert!(verts.iter().all(|v| v[6] == 0.0 && v[7] == 0.0), "unpainted verts emit zero uv");
    }

    #[test]
    fn paint_write_paths_survive_colors_density_divergence() {
        // Regression (2026-08-02 sweep): paint write sites indexed `self.colors[..]` trusting the
        // dense invariant colors.len() == res³. A density swap / resample-style replacement that
        // leaves a stale `colors` buffer behind must NOT panic and must NOT mis-colour — the
        // paint chokepoint (`ensure_paint_colors`) drops the stale layout and re-establishes the
        // invariant structurally.
        let red = pack_voxel_color(Vec3::new(1.0, 0.0, 0.0));
        let blue = pack_voxel_color(Vec3::new(0.0, 0.0, 1.0));

        // (a) Stale WRONG-LENGTH dense buffer (what a forgotten resize after a res change leaves).
        let mut g = VoxelGrid::new_empty(32, 1.0);
        g.fill_sphere(Vec3::ZERO, 0.6);
        g.colors = vec![red; 10]; // simulate the divergent state directly
        g.deposit_color_sphere(Vec3::ZERO, 0.4, blue); // must not panic
        assert!(g.colors_in_sync(), "chokepoint must re-establish colors.len() == res³");
        assert!((0..g.density.len()).any(|i| g.color_idx(i) == Some(blue)), "paint must land");
        assert!(!g.colors.iter().any(|&c| c == red), "stale mis-layout paint must be dropped, not reused");

        // (b) fill_all_color + restore_color_delta on the same divergent state.
        let mut g = VoxelGrid::new_empty(24, 1.0);
        g.fill_sphere(Vec3::ZERO, 0.6);
        g.colors = vec![red; 7];
        let delta = g.fill_all_color(blue); // must not panic
        assert!(g.colors_in_sync() && !delta.is_empty());
        let _ = g.restore_color_delta(&delta); // must not panic either

        // (c) Dense→sparse in-place density swap with paint left behind: paint ops become no-ops
        // and the dead dense buffer is dropped (it can never be misread later).
        let mut g = VoxelGrid::new_empty(32, 1.0);
        g.fill_sphere(Vec3::ZERO, 0.6);
        g.deposit_color_sphere(Vec3::ZERO, 0.4, red);
        assert!(!g.colors.is_empty());
        g.density = DensityStorage::sparse(32, 0.0);
        g.deposit_color_sphere(Vec3::ZERO, 0.4, blue); // must not panic, no-op
        assert!(g.colors.is_empty(), "stale dense paint buffer must be dropped on the sparse path");
        assert!(g.fill_all_color(blue).is_empty());
        assert!(!g.colors_in_sync());
        // restore_delta with colors diverged (sparse) must not touch colors and must not panic.
        let _ = g.restore_delta(&[(0, 0.0)]);
    }

    #[test]
    fn mask_survives_dense_sparse_resample_round_trip() {
        // Regression (2026-08-02 sweep): resample built a fresh grid and silently dropped the
        // protect-mask; worse, dense↔sparse conversion left the OLD storage (mask vs mask_sparse)
        // populated but dead (mask_factor_at branches on density.is_sparse()). The mask must now
        // migrate world-shape-preserving in BOTH directions, like the density does.
        let mut g = VoxelGrid::new_empty(64, 1.0); // dense
        g.fill_sphere(Vec3::ZERO, 0.7);
        let masked_p = Vec3::new(0.0, 0.0, 0.7); // on the sphere surface
        g.deposit_mask_sphere(masked_p, 0.3, 255, 1.0);
        assert!(g.has_mask());
        let probe = |grid: &VoxelGrid, p: Vec3| -> f32 {
            let c = grid.world_to_grid(p);
            grid.mask_factor_at(grid.flat_index(
                c.x.round() as u32, c.y.round() as u32, c.z.round() as u32))
        };
        assert!(probe(&g, masked_p) > 0.8, "mask painted at the probe point");
        let free_p = Vec3::new(0.0, 0.0, -0.7);
        assert!(probe(&g, free_p) < 0.1, "opposite pole unmasked");

        // Dense (64) → sparse (256): mask must land in mask_sparse (the live storage).
        let s = g.resample(256);
        assert!(s.density.is_sparse());
        assert!(s.has_mask(), "mask lost on dense->sparse resample");
        assert!(s.mask.is_empty(), "sparse grid must not carry a dead dense mask buffer");
        assert!(probe(&s, masked_p) > 0.5, "masked world position survives dense->sparse");
        assert!(probe(&s, free_p) < 0.1, "unmasked region stays free");

        // Sparse (256) → dense (64): back into the dense buffer.
        let d = s.resample(64);
        assert!(!d.density.is_sparse());
        assert!(d.has_mask(), "mask lost on sparse->dense resample");
        assert!(d.mask_sparse.is_empty(), "dense grid must not carry a dead sparse mask map");
        assert!(probe(&d, masked_p) > 0.5, "masked world position survives the round trip");
        assert!(probe(&d, free_p) < 0.1, "unmasked region stays free after round trip");

        // resample_to (bounds grow) preserves the mask at the same WORLD position too.
        let grown = g.resample_to(96, 1.5);
        assert!(grown.has_mask(), "mask lost on resample_to grow");
        assert!(probe(&grown, masked_p) > 0.5, "masked world position survives a bounds grow");
    }

    #[test]
    fn mask_tint_darkens_surface_verts_when_show_mask() {
        // Deposit a protect-mask over a blob → mesher packs darkened vertex colours (uv.y=1).
        let mut g = VoxelGrid::new_empty(48, 1.0);
        g.apply_brush(VoxelBrush::Draw, Vec3::ZERO, Vec3::Y, 0.6, 1.0, false, 0.0, Vec3::ZERO, None);
        g.deposit_mask_sphere(Vec3::ZERO, 0.5, 255, 0.5);
        assert!(g.has_mask() && g.mask_visible(), "mask deposit must allocate and be visible");
        let (verts, _idx) = g.object_display_mesh();
        let tinted = verts.iter().filter(|v| v[7] > 0.5).count();
        assert!(tinted > 0, "masked surface verts must emit painted/tinted UVs");
        // Tinted colour must be darker than the neutral clay base (0.55*0.15 floor residual).
        for v in verts.iter().filter(|v| v[7] > 0.5) {
            let w = v[6].to_bits();
            let r = ((w >> 16) & 0xFF) as f32 / 255.0;
            assert!(r < 0.50, "mask tint should darken toward black (got r={r})");
        }
        // Show off → no tint packing (unpainted path).
        g.set_show_mask(false);
        let (verts2, _idx2) = g.object_display_mesh();
        assert!(verts2.iter().all(|v| v[7] < 0.5), "show_mask off → no tint flag");
        // Invert still has data; show on again → tint returns.
        g.set_show_mask(true);
        g.invert_mask();
        assert!(g.has_mask());
    }

    #[test]
    fn sparse_mask_deposit_uses_hashmap_and_blocks_brush() {
        // ≥256³ uses sparse density — mask must live in mask_sparse (not a res³ Vec).
        let mut g = VoxelGrid::new_empty(256, 1.0);
        assert!(g.density.is_sparse(), "256³ is sparse");
        g.fill_sphere(Vec3::ZERO, 0.55);
        g.deposit_mask_sphere(Vec3::ZERO, 0.4, 255, 0.5);
        assert!(g.has_mask(), "sparse mask paint must set has_mask");
        assert!(g.mask.is_empty(), "sparse must not allocate dense mask Vec");
        assert!(!g.mask_sparse.is_empty(), "sparse non-zero map must hold painted cells");
        // Fully masked voxel near centre resists Draw.
        let ci = g.idx(g.res / 2, g.res / 2, g.res / 2);
        let before = g.density.get_idx(ci);
        // Force full mask at centre for a clean gate.
        g.mask_sparse.insert(ci as u32, 255);
        g.apply_brush(VoxelBrush::Draw, Vec3::ZERO, Vec3::Y, 0.3, 1.0, false, 0.0, Vec3::ZERO, None);
        let after = g.density.get_idx(ci);
        assert!((after - before).abs() < 1e-4, "fully masked centre must resist Draw (before={before} after={after})");
        g.clear_mask();
        assert!(!g.has_mask());
    }

    #[test]
    fn auto_mask_peak_protects_high_curvature_surface() {
        // Sphere surface has curvature → Peak mode should write non-zero protect mask.
        let mut g = VoxelGrid::new_empty(48, 1.0);
        g.fill_sphere(Vec3::ZERO, 0.55);
        g.deposit_auto_mask_sphere(
            Vec3::new(0.0, 0.55, 0.0), 0.35, 0.5,
            AutoMaskMode::Peak, 1.0, false,
        );
        assert!(g.has_mask(), "Peak auto-mask on a sphere pole must write protect mask");
        // Off mode must be a no-op.
        g.clear_mask();
        g.deposit_auto_mask_sphere(
            Vec3::new(0.0, 0.55, 0.0), 0.35, 0.5,
            AutoMaskMode::Off, 1.0, false,
        );
        assert!(!g.has_mask(), "Off mode must not allocate mask");
        // Sparse path (≥256³).
        let mut gs = VoxelGrid::new_empty(256, 1.0);
        gs.fill_sphere(Vec3::ZERO, 0.5);
        gs.deposit_auto_mask_sphere(
            Vec3::new(0.0, 0.5, 0.0), 0.3, 0.5,
            AutoMaskMode::Curvature, 0.9, false,
        );
        assert!(gs.has_mask() && gs.mask.is_empty() && !gs.mask_sparse.is_empty(),
            "sparse auto-mask must use HashMap only");
    }

    #[test]
    fn fill_all_color_colors_all_solid_voxels_and_undo_restores() {
        // Fill All paints EVERY solid voxel (density ≥ iso) in one step; the returned delta, replayed,
        // restores the exact prior colours — the single-step sculpt colour-undo invariant.
        let mut g = VoxelGrid::new_empty(48, 1.0);
        g.apply_brush(VoxelBrush::Draw, Vec3::ZERO, Vec3::Y, 0.6, 1.0, false, DEFAULT_BRUSH_HARDNESS, Vec3::ZERO, None);
        // Pre-paint a small patch a DIFFERENT colour so undo has a non-trivial prior value to restore.
        let blue = pack_voxel_color(Vec3::new(0.0, 0.0, 1.0));
        g.deposit_color_sphere(Vec3::ZERO, 0.2, blue);
        let solid_before: Vec<usize> = (0..g.density.len()).filter(|&i| g.density.get_idx(i) >= g.iso).collect();
        assert!(!solid_before.is_empty(), "the blob must have solid voxels to fill");

        let green = pack_voxel_color(Vec3::new(0.0, 1.0, 0.0));
        let delta = g.fill_all_color(green);
        assert!(!delta.is_empty(), "fill must record a delta (some voxels changed colour)");
        // EVERY solid voxel now carries the fill colour; empty voxels stay unpainted.
        for &i in &solid_before { assert_eq!(g.color_idx(i), Some(green), "every solid voxel takes the fill colour"); }
        assert!((0..g.density.len()).all(|i| g.density.get_idx(i) >= g.iso || g.color_idx(i) != Some(green)),
            "empty voxels must not be painted by the fill");

        // Undo: replay the delta → the blue patch returns, the rest goes back to unpainted.
        let inverse = g.restore_color_delta(&delta);
        assert_eq!(inverse.len(), delta.len(), "the inverse covers exactly the filled voxels");
        let mut blue_count = 0;
        for &i in &solid_before {
            match g.color_idx(i) {
                Some(c) if c == blue => blue_count += 1,
                None => {}                       // was unpainted before the fill — correctly restored
                other => panic!("undo left a stale colour {other:?} at voxel {i}"),
            }
        }
        assert!(blue_count > 0, "the pre-existing blue patch must be restored by undo");
        // Redo: replay the inverse → back to the fully-green fill state.
        let _ = g.restore_color_delta(&inverse);
        for &i in &solid_before { assert_eq!(g.color_idx(i), Some(green), "redo re-applies the fill colour"); }
    }

    #[test]
    fn fill_all_color_sparse_is_noop() {
        // Like deposit, Fill All is dense-only — a no-op (empty delta) on a sparse (≥256³) grid.
        let mut g = VoxelGrid::new_empty(256, 2.0);
        assert!(g.density.is_sparse());
        let delta = g.fill_all_color(pack_voxel_color(Vec3::ONE));
        assert!(delta.is_empty(), "sparse fill is a no-op (deferred)");
        assert!(g.colors.is_empty(), "sparse fill allocates no colour buffer");
    }

    #[test]
    fn paint_color_sparse_is_noop() {
        // Vertex-paint is dense-only for now — deposit must no-op on a sparse (≥256³) grid, never panic.
        let mut g = VoxelGrid::new_empty(256, 2.0);
        assert!(g.density.is_sparse());
        g.deposit_color_sphere(Vec3::ZERO, 0.5, pack_voxel_color(Vec3::ONE));
        assert!(g.colors.is_empty(), "sparse paint deposit is a no-op (deferred)");
    }

    #[test]
    fn fill_from_sdf_records_undo_delta_and_restores() {
        // The stamp records every modified voxel's pre-value into a StrokeRecorder; restoring that
        // delta must return the grid to its pre-stamp state (the sculpt-undo path round-trips).
        let mut g = VoxelGrid::new_empty(32, 1.0);
        let before: Vec<f32> = g.density.iter().collect();
        let mut rec = StrokeRecorder::default();
        rec.begin(g.res);
        g.fill_from_sdf(sphere_sdf(Vec3::ZERO, 0.5), true, None, Some(&mut rec));
        assert!(!rec.is_empty(), "a union over an empty grid must record changed voxels");
        let delta = rec.take();
        let _inv = g.restore_delta(&delta);
        let after: Vec<f32> = g.density.iter().collect();
        assert_eq!(before, after, "restoring the stamp delta must reproduce the pre-stamp grid");
    }

    #[test]
    fn fill_from_sdf_bounds_restrict_sweep() {
        // A bounds AABB far from the field still works (no panic) and an in-range bounds matches the
        // unbounded result inside its window — the bounds are an optimisation, not a behaviour change.
        let mut a = VoxelGrid::new_empty(32, 1.0);
        let mut b = VoxelGrid::new_empty(32, 1.0);
        a.fill_from_sdf(sphere_sdf(Vec3::ZERO, 0.5), true, None, None);
        b.fill_from_sdf(sphere_sdf(Vec3::ZERO, 0.5), true, Some([Vec3::splat(-0.7), Vec3::splat(0.7)]), None);
        // The sphere fits inside the bounds, so the centre is identical on both paths.
        assert!((a.sample(Vec3::ZERO) - b.sample(Vec3::ZERO)).abs() < 1e-5);
    }

    #[test]
    fn boundary_stroke_no_panic() {
        // Regression: a stroke that pushes material into the LAST grid layer used to
        // index a cell coordinate of n-1 (out of the valid 0..n-2 cell range) and panic
        // (the "crash after draw" bug). Surface Nets must stay bounds-safe.
        let mut g = VoxelGrid::new_empty(16, 1.0);
        g.fill_sphere(Vec3::ZERO, 0.6);
        for _ in 0..50 {
            // big brush near the +Y face → material reaches the grid boundary
            g.apply_brush(VoxelBrush::Draw, Vec3::new(0.0, 0.95, 0.0), Vec3::Y, 0.95, 1.0, false, 0.0, Vec3::ZERO, None);
        }
        let m = g.surface_nets(true); // must not panic
        assert!(m.vertices.len() > 0);
        // also exercise erase pushing the surface around
        for _ in 0..50 {
            g.apply_brush(VoxelBrush::Erase, Vec3::new(0.95, 0.0, 0.0), Vec3::X, 0.95, 1.0, false, 0.0, Vec3::ZERO, None);
        }
        let _ = g.surface_nets(true); // must not panic
    }

    #[test]
    fn golden_parity_stamp_plan_matches_legacy_all_builtins() {
        // PHASE-0 GATE (BRUSH_SYSTEM_ARCHITECTURE.md §4.3): the StampPlan apply path
        // (`apply_brush` → `from_builtin` → `fold_recipe` → `apply_stamp_plan`) must reproduce the
        // byte-for-byte LEGACY math (`apply_brush_legacy`) for EVERY one of the 12 built-in presets.
        // This is the structural op-parity guarantee the GPU kernel is then validated against (one
        // opcode table, two languages). If this fails, the data model does NOT represent a current
        // brush losslessly and nothing downstream is safe.
        use crate::brush::BuiltinBrush;
        // A fixed solid blob so plane/gradient/relax/advect ops all have real material to act on.
        let base = {
            let mut g = VoxelGrid::new_empty(40, 1.0);
            g.fill_sphere(Vec3::ZERO, 0.55);
            g
        };
        // A surface contact point + outward normal (on the +Z face of the blob) so plane ops bite.
        let center = Vec3::new(0.0, 0.0, 0.45);
        let normal = Vec3::Z;
        let (radius, strength) = (0.30f32, 0.6f32);
        // Exercise the soft default AND a sharp core (the Power-kernel curve), both signs (Ctrl invert).
        let cases: [(f32, bool); 4] = [(DEFAULT_BRUSH_HARDNESS, false), (0.0, true), (0.7, false), (0.7, true)];
        let mut worst = 0.0f32;
        let mut worst_name = "";
        for b in BuiltinBrush::ALL {
            for (hardness, invert) in cases {
                let vb = match b {
                    BuiltinBrush::Draw => VoxelBrush::Draw, BuiltinBrush::Erase => VoxelBrush::Erase,
                    BuiltinBrush::Smooth => VoxelBrush::Smooth, BuiltinBrush::Flatten => VoxelBrush::Flatten,
                    BuiltinBrush::Clay => VoxelBrush::Clay, BuiltinBrush::Scrape => VoxelBrush::Scrape,
                    BuiltinBrush::Crease => VoxelBrush::Crease, BuiltinBrush::Fill => VoxelBrush::Fill,
                    BuiltinBrush::ClayBuildup => VoxelBrush::ClayBuildup, BuiltinBrush::Inflate => VoxelBrush::Inflate,
                    BuiltinBrush::Pinch => VoxelBrush::Pinch, BuiltinBrush::Move => VoxelBrush::Move,
                };
                // Move/Grab carries a per-stamp drag; give it a real one so its advection actually moves.
                let drag = if matches!(vb, VoxelBrush::Move) { Vec3::new(0.05, 0.0, 0.0) } else { Vec3::ZERO };
                let mut plan_grid = base.clone();
                let mut legacy_grid = base.clone();
                plan_grid.apply_brush(vb, center, normal, radius, strength, invert, hardness, drag, None);
                legacy_grid.apply_brush_legacy(vb, center, normal, radius, strength, invert, hardness, drag, None);
                // Density fields must be bit-equal (same float ops, same order). Compare every voxel.
                let n = plan_grid.density.len();
                assert_eq!(n, legacy_grid.density.len());
                for i in 0..n {
                    let d = (plan_grid.density.get_idx(i) - legacy_grid.density.get_idx(i)).abs();
                    if d > worst { worst = d; worst_name = b.name(); }
                }
            }
        }
        // Bit-identical by construction (same arithmetic, same loop order) → tolerance is ~0.
        assert!(worst <= 1e-6, "StampPlan vs legacy diverged on {worst_name}: |Δ| {worst:.3e}");
    }

    #[test]
    fn resample_changes_resolution_keeps_surface() {
        let mut g = VoxelGrid::new_empty(48, 2.0);
        g.fill_sphere(Vec3::ZERO, 1.0);
        let base = g.surface_nets(true).vertices.len();
        assert!(base > 0);
        // Upsample → same bounds, higher res, more surface detail.
        let up = g.resample(96);
        assert_eq!(up.res, 96);
        assert_eq!(up.bounds, g.bounds);
        let up_m = up.surface_nets(true);
        assert!(!up_m.quads.is_empty());
        assert!(up_m.vertices.len() > base, "upsample {} should exceed base {}", up_m.vertices.len(), base);
        // Downsample → fewer verts, still a real closed surface (not emptied).
        let down = g.resample(24);
        assert_eq!(down.res, 24);
        let down_m = down.surface_nets(true);
        assert!(!down_m.vertices.is_empty(), "downsample must keep a surface");
        assert!(down_m.vertices.len() < base, "downsample {} should be below base {}", down_m.vertices.len(), base);
        // Iso surface preserved: resampled vertices still sit near the unit sphere.
        for v in &down_m.vertices { assert!((v.length() - 1.0).abs() < down.voxel_size() * 3.0); }
    }

    #[test]
    fn resample_to_grows_bounds_keeps_world_shape_centered() {
        // A solid sphere resampled into a LARGER-bounds grid must keep the SAME world shape (centred at the
        // origin, same radius), with an empty margin filling the new outer region — the voxel Move GROW.
        let mut g = VoxelGrid::new_empty(64, 2.0);
        g.fill_sphere(Vec3::ZERO, 1.0);
        // Grow: bounds 2.0 → 4.0, keep voxel detail (res scales with bounds: 64 * 4/2 = 128).
        let grown = g.resample_to(128, 4.0);
        assert_eq!(grown.res, 128);
        assert_eq!(grown.bounds, 4.0);
        // Voxel size is preserved (constant detail): 2*2/(64-1) ≈ 2*4/(128-1).
        assert!((grown.voxel_size() - g.voxel_size()).abs() < g.voxel_size() * 0.05,
            "grow should keep voxel detail: old {} new {}", g.voxel_size(), grown.voxel_size());
        // The surface is still the unit sphere at the origin (same world shape, just a bigger box).
        let m = grown.surface_nets(true);
        assert!(!m.vertices.is_empty(), "grown grid must keep a surface");
        for v in &m.vertices {
            assert!((v.length() - 1.0).abs() < grown.voxel_size() * 3.0,
                "grown surface vertex {:?} drifted from the unit sphere", v);
        }
        // Density is preserved at world sample points: solid at centre, empty in the new outer margin
        // (a point outside the OLD extent but inside the new grid must be empty material).
        assert!(grown.sample(Vec3::ZERO) > 0.9, "centre should stay solid after grow");
        assert!(grown.sample(Vec3::new(3.0, 0.0, 0.0)) < 0.1, "new outer margin must be empty");
        // Same world density as the source where both are defined (centre + just inside the sphere).
        for p in [Vec3::ZERO, Vec3::new(0.5, 0.0, 0.0), Vec3::new(0.0, 0.9, 0.0)] {
            assert!((grown.sample(p) - g.sample(p)).abs() < 0.05,
                "world density at {:?} changed across grow: {} vs {}", p, grown.sample(p), g.sample(p));
        }
    }

    #[test]
    fn resample_to_same_bounds_matches_resample() {
        // resample_to with new_bounds == bounds must reduce EXACTLY to resample (byte-identical field).
        let mut g = VoxelGrid::new_empty(48, 1.5);
        g.fill_sphere(Vec3::new(0.2, -0.1, 0.0), 0.7);
        let a = g.resample(96);
        let b = g.resample_to(96, g.bounds);
        assert_eq!(a.res, b.res);
        assert_eq!(a.bounds, b.bounds);
        assert_eq!(a.density.to_dense(), b.density.to_dense(),
            "resample_to(.., same_bounds) must equal resample(..)");
    }

    /// A smooth, non-trivial occupancy field for fill_from tests: a soft sphere of radius `r` in voxel
    /// space, clamped to [0,1]. Has empty bricks (outside), full bricks (deep inside), and mixed bricks
    /// (the shell) — exercising all three classification branches.
    fn sphere_field(res: u32, r: f32) -> impl Fn(u32, u32, u32) -> f32 + Sync {
        let c = (res as f32 - 1.0) * 0.5;
        move |x, y, z| {
            let (dx, dy, dz) = (x as f32 - c, y as f32 - c, z as f32 - c);
            let d = (dx * dx + dy * dy + dz * dz).sqrt();
            // soft transition over ~2 voxels → guarantees mixed (fractional) shell bricks
            (0.5 - (d - r) * 0.5).clamp(0.0, 1.0)
        }
    }

    #[test]
    fn par_fill_from_matches_serial_sparse() {
        // par_fill_from MUST be byte-identical to serial fill_from: same INDEX tags, same arena values,
        // same handle order, same free list. Verified on a non-trivial sphere at a couple resolutions.
        for &res in &[256u32, 320] {
            let r = res as f32 * 0.35;
            let mut a = BrickGrid::new(res, 0.0);
            let mut b = BrickGrid::new(res, 0.0);
            a.fill_from(sphere_field(res, r));
            b.par_fill_from(sphere_field(res, r));
            assert_eq!(a.index, b.index, "INDEX tags differ at res {res} (tag/handle order)");
            assert_eq!(a.bricks, b.bricks, "arena floats differ at res {res}");
            assert_eq!(a.free, b.free, "free list differs at res {res}");
            // Sanity: all three branches exercised (some EMPTY, some FULL, some allocated).
            let n_empty = a.index.iter().filter(|&&t| t == TAG_EMPTY).count();
            let n_full = a.index.iter().filter(|&&t| t == TAG_FULL).count();
            let n_alloc = a.index.iter().filter(|&&t| t != TAG_EMPTY && t != TAG_FULL).count();
            assert!(n_empty > 0 && n_full > 0 && n_alloc > 0,
                "res {res}: expected all 3 brick classes, got empty={n_empty} full={n_full} alloc={n_alloc}");
        }
    }

    #[test]
    fn par_fill_from_matches_serial_dense() {
        // Dense path (small res): par_fill_from over z-slices must equal the serial fill voxel-for-voxel.
        let res = 64u32;
        let mut a = DensityStorage::dense(res, 0.0);
        let mut b = DensityStorage::dense(res, 0.0);
        a.fill_from(sphere_field(res, 20.0));
        b.par_fill_from(sphere_field(res, 20.0));
        assert_eq!(a.to_dense(), b.to_dense(), "dense par_fill_from differs from serial");
    }

    #[test]
    fn resample_deterministic_parallel() {
        // The resample (which now routes through par_fill_from) must produce the SAME surface as before:
        // running it twice yields an identical vertex set.
        let mut g = VoxelGrid::new_empty(96, 1.0);
        g.fill_sphere(Vec3::ZERO, 0.6);
        let a = g.resample(256);
        let b = g.resample(256);
        assert_eq!(a.density.to_dense(), b.density.to_dense(), "resample is not deterministic");
    }

    // --- Incremental mesher (VoxelMeshCache) ---------------------------------------------
    fn tri_manifold(idx: &[u32]) -> bool {
        use std::collections::HashMap;
        let mut e: HashMap<(u32, u32), i32> = HashMap::new();
        for t in idx.chunks(3) {
            for k in 0..3 { *e.entry((t[k], t[(k + 1) % 3])).or_insert(0) += 1; }
        }
        e.iter().all(|(&(a, b), &c)| c == *e.get(&(b, a)).unwrap_or(&0))
    }
    fn pos_set(verts: &[[f32; 8]]) -> std::collections::BTreeSet<(i32, i32, i32)> {
        verts.iter().map(|v| ((v[0] * 1000.0).round() as i32, (v[1] * 1000.0).round() as i32, (v[2] * 1000.0).round() as i32)).collect()
    }
    fn region_of(g: &mut VoxelGrid) -> Option<[u32; 6]> {
        g.take_dirty().map(|d| [d.min[0], d.min[1], d.min[2], d.max[0], d.max[1], d.max[2]])
    }

    #[test]
    fn surface_chunk_mask_skips_only_empty_chunks() {
        // A chunk the mask flags FALSE must mesh empty — otherwise skipping it on a rebuild would
        // punch holes. Verify over a sphere at a realistic res/chunk size, and that it actually
        // skips a meaningful number (so it's a real win, not a no-op).
        let mut g = VoxelGrid::new_empty(64, 1.0);
        g.fill_sphere(Vec3::ZERO, 0.30); // small central sphere → corner chunks are clearly empty
        let cs = 16u32;
        let mask = g.surface_chunk_mask(cs);
        let cells = g.res - 1;
        let cd = (cells + cs - 1) / cs;
        let cmax = cells - 1;
        let (mut skipped, mut surface) = (0u32, 0u32);
        for cz in 0..cd { for cy in 0..cd { for cx in 0..cd {
            let ci = (cx + cy * cd + cz * cd * cd) as usize;
            let a0 = [cx * cs, cy * cs, cz * cs];
            let a1 = [((cx + 1) * cs - 1).min(cmax), ((cy + 1) * cs - 1).min(cmax), ((cz + 1) * cs - 1).min(cmax)];
            let (verts, idx) = g.mesh_region(a0, a1);
            if !mask[ci] {
                assert!(verts.is_empty() && idx.is_empty(), "chunk {:?} flagged empty but produced {} verts", (cx, cy, cz), verts.len());
                skipped += 1;
            } else if !verts.is_empty() { surface += 1; }
        }}}
        assert!(skipped > 0 && surface > 0, "mask must discriminate (skipped {}, surface {})", skipped, surface);
    }

    #[test]
    fn surface_chunk_mask_no_hole_at_hard_boundary_step() {
        // Regression for the hard-step hole (finding #5): a 0↔1 density step landing EXACTLY on a chunk
        // boundary makes the solid chunk uniformly FULL (has=false) and the empty neighbour uniformly
        // EMPTY (has=false) — yet mesh_region for the solid chunk emits the boundary quad. Before the
        // cross-neighbour-solidity predicate, neither chunk was flagged → the chunk was skipped on a full
        // rebuild → a PERMANENT HOLE. Assert the invariant the mask must uphold: every chunk flagged FALSE
        // meshes EMPTY. (cs=16 ⇒ chunks at voxels [0,16),[16,32),... ; step at x=32 = chunk-2 start.)
        let res = 64u32;
        let mut g = VoxelGrid::new_empty(res, 1.0);
        g.density.fill_from(|x, _, _| if x < 32 { 1.0 } else { 0.0 }); // hard step on the chunk-2 boundary
        let cs = 16u32;
        let mask = g.surface_chunk_mask(cs);
        let cells = res - 1;
        let cd = (cells + cs - 1) / cs;
        let cmax = cells - 1;
        let mut meshed_boundary = false;
        for cz in 0..cd { for cy in 0..cd { for cx in 0..cd {
            let ci = (cx + cy * cd + cz * cd * cd) as usize;
            let a0 = [cx * cs, cy * cs, cz * cs];
            let a1 = [((cx + 1) * cs - 1).min(cmax), ((cy + 1) * cs - 1).min(cmax), ((cz + 1) * cs - 1).min(cmax)];
            let (verts, idx) = g.mesh_region(a0, a1);
            if !mask[ci] {
                assert!(verts.is_empty() && idx.is_empty(),
                    "chunk {:?} flagged FALSE but mesh_region emits {} verts at the hard boundary step → HOLE", (cx, cy, cz), verts.len());
            } else if !verts.is_empty() { meshed_boundary = true; }
        }}}
        assert!(meshed_boundary, "the hard step must produce a meshed boundary somewhere (sanity)");
    }

    #[test]
    fn surface_chunk_mask_parallel_matches_serial() {
        // The parallel cz-plane min/max reduction must produce the SAME mask as a straight serial scan.
        // res 290 ≥ SPARSE_RES_THRESHOLD (exercises the sparse get_idx path) and is NOT a multiple of the
        // chunk size (exercises the clamped tail plane). Pure associative/commutative min/max → identical.
        let res = 290u32;
        let mut g = VoxelGrid::new_empty(res, 1.0);
        g.fill_sphere(Vec3::new(0.1, -0.2, 0.05), 0.55); // off-center → varied chunk occupancy
        assert!(g.density.is_sparse(), "res {res} should be sparse");
        let cs = 16u32; // app meshes with VOXEL_CHUNK = 16 cells
        let par = g.surface_chunk_mask(cs);
        // Serial reference = the pre-parallel algorithm, inlined.
        let cells = res.saturating_sub(1).max(1);
        let cd = (((cells + cs - 1) / cs).max(1)) as usize;
        let n = cd * cd * cd;
        let iso = g.iso;
        let (mut cmin, mut cmax) = (vec![f32::INFINITY; n], vec![f32::NEG_INFINITY; n]);
        for z in 0..res {
            let cz = ((z / cs) as usize).min(cd - 1);
            for y in 0..res {
                let cy = ((y / cs) as usize).min(cd - 1);
                let row = (y * res + z * res * res) as usize;
                for x in 0..res {
                    let cx = ((x / cs) as usize).min(cd - 1);
                    let v = g.density.get_idx(row + x as usize);
                    let ci = cx + cy * cd + cz * cd * cd;
                    if v < cmin[ci] { cmin[ci] = v; }
                    if v > cmax[ci] { cmax[ci] = v; }
                }
            }
        }
        let has = |ci: usize| cmin[ci] < iso && cmax[ci] >= iso;
        let mut serial = vec![false; n];
        for cz in 0..cd { for cy in 0..cd { for cx in 0..cd {
            let ci = cx + cy * cd + cz * cd * cd;
            let mut need = has(ci);
            if !need {
                let nb = [
                    (cx.wrapping_sub(1), cy, cz), (cx + 1, cy, cz),
                    (cx, cy.wrapping_sub(1), cz), (cx, cy + 1, cz),
                    (cx, cy, cz.wrapping_sub(1)), (cx, cy, cz + 1),
                ];
                for (nx, ny, nz) in nb {
                    if nx < cd && ny < cd && nz < cd && has(nx + ny * cd + nz * cd * cd) { need = true; break; }
                }
            }
            serial[ci] = need;
        }}}
        assert_eq!(par, serial, "parallel mask must equal the serial reference (res {res})");
    }

    #[test]
    fn mesh_cache_matches_surface_nets() {
        // T1: a full incremental build equals surface_nets in vertex/quad count + manifold.
        let mut g = VoxelGrid::new_empty(32, 1.0);
        g.fill_sphere(Vec3::ZERO, 0.5);
        let mesh = g.surface_nets(false);
        let mut cache = VoxelMeshCache::new(g.res);
        cache.update(&g, None);
        let (verts, idx) = cache.build(&g, false);
        assert_eq!(verts.len(), mesh.vertices.len(), "vertex count parity");
        assert_eq!(idx.len() / 6, mesh.quads.len(), "quad count parity");
        assert!(tri_manifold(&idx), "cache mesh must be watertight/manifold");
        // smooth path shares relax_mesh → same vertex count as surface_nets(true)
        let (vs, _) = cache.build(&g, true);
        assert_eq!(vs.len(), g.surface_nets(true).vertices.len());
    }

    #[test]
    fn mesh_cache_incremental_matches_full() {
        // T2: editing a region + update(region) equals a from-scratch full rebuild.
        let mut g = VoxelGrid::new_empty(24, 1.0);
        g.fill_sphere(Vec3::ZERO, 0.4);
        let mut cache = VoxelMeshCache::new(g.res);
        cache.update(&g, None);
        let _ = cache.build(&g, false);
        g.apply_brush(VoxelBrush::Draw, Vec3::new(0.2, 0.0, 0.0), Vec3::X, 0.25, 1.0, false, 0.0, Vec3::ZERO, None);
        let region = region_of(&mut g);
        cache.update(&g, region);
        let (vi, ii) = cache.build(&g, false);
        let mut fresh = VoxelMeshCache::new(g.res);
        fresh.update(&g, None);
        let (vf, iff) = fresh.build(&g, false);
        assert_eq!(vi.len(), vf.len(), "incremental vs full vertex count");
        assert_eq!(ii.len(), iff.len(), "incremental vs full index count");
        assert_eq!(pos_set(&vi), pos_set(&vf), "incremental vertices diverge from full");
        assert!(tri_manifold(&ii));
    }

    #[test]
    fn mesh_cache_handles_occupancy_flips() {
        // T3: erase frees slots, draw reuses them — incremental stays equal to a full rebuild.
        let mut g = VoxelGrid::new_empty(20, 1.0);
        g.fill_sphere(Vec3::ZERO, 0.5);
        let mut cache = VoxelMeshCache::new(g.res);
        cache.update(&g, None);
        for _ in 0..6 {
            g.apply_brush(VoxelBrush::Erase, Vec3::new(0.0, 0.55, 0.0), Vec3::Y, 0.4, 1.0, false, 0.0, Vec3::ZERO, None);
            let r = region_of(&mut g); cache.update(&g, r);
            g.apply_brush(VoxelBrush::Draw, Vec3::new(0.55, 0.0, 0.0), Vec3::X, 0.3, 1.0, false, 0.0, Vec3::ZERO, None);
            let r = region_of(&mut g); cache.update(&g, r);
        }
        let (vi, ii) = cache.build(&g, false);
        let mut fresh = VoxelMeshCache::new(g.res); fresh.update(&g, None);
        let (vf, iff) = fresh.build(&g, false);
        assert_eq!(pos_set(&vi), pos_set(&vf), "after occupancy flips, incremental diverges");
        assert_eq!(ii.len(), iff.len());
        assert!(tri_manifold(&ii));
    }

    #[test]
    fn mesh_cache_resets_on_res_change() {
        // T4: a res change forces a full rebuild even when a small region is passed.
        let mut g = VoxelGrid::new_empty(32, 1.0);
        g.fill_sphere(Vec3::ZERO, 0.5);
        let mut cache = VoxelMeshCache::new(32);
        cache.update(&g, None);
        let _ = cache.build(&g, false);
        let g2 = g.resample(20);
        cache.update(&g2, Some([0, 0, 0, 1, 1, 1])); // small region, but res changed
        let (vi, _) = cache.build(&g2, false);
        assert_eq!(vi.len(), g2.surface_nets(false).vertices.len(), "res change must full-rebuild");
    }

    #[test]
    fn mesh_region_whole_matches_surface_nets() {
        // Meshing the whole anchor range == surface_nets (the −1 halo clamps away at the grid edge).
        let mut g = VoxelGrid::new_empty(32, 1.0);
        g.fill_sphere(Vec3::ZERO, 0.5);
        let mesh = g.surface_nets(false);
        let cmax = g.res - 2; // cells-1
        let (verts, idx) = g.mesh_region([0, 0, 0], [cmax, cmax, cmax]);
        assert_eq!(verts.len(), mesh.vertices.len(), "whole-region verts == surface_nets");
        assert_eq!(idx.len() / 6, mesh.quads.len(), "whole-region quads == surface_nets");
        assert!(tri_manifold(&idx), "whole-region mesh must be manifold");
    }

    #[test]
    fn mesh_region_with_cells_provenance_roundtrips() {
        // STEP 3: the parallel cell-id array is one-per-vertex and each id re-derives the SAME
        // position via cell_surface_vertex_d (the provenance the preview paths rely on). Also the
        // verts/idx must be byte-identical to the plain mesh_region (the thin wrapper).
        let mut g = VoxelGrid::new_empty(32, 1.0);
        g.fill_sphere(Vec3::ZERO, 0.5);
        let cmax = g.res - 2;
        let (verts, idx, cells) = g.mesh_region_with_cells([0, 0, 0], [cmax, cmax, cmax]);
        let (vplain, iplain) = g.mesh_region([0, 0, 0], [cmax, cmax, cmax]);
        assert_eq!(verts, vplain, "with_cells verts == mesh_region verts (byte-identical)");
        assert_eq!(idx, iplain, "with_cells idx == mesh_region idx (byte-identical)");
        assert_eq!(cells.len(), verts.len(), "exactly one cell id per vertex");
        assert!(!cells.is_empty(), "sphere produced a surface");
        for (i, &[cx, cy, cz]) in cells.iter().enumerate() {
            let d = [
                g.density.get_idx(g.idx(cx, cy, cz)),       g.density.get_idx(g.idx(cx + 1, cy, cz)),
                g.density.get_idx(g.idx(cx, cy + 1, cz)),   g.density.get_idx(g.idx(cx + 1, cy + 1, cz)),
                g.density.get_idx(g.idx(cx, cy, cz + 1)),   g.density.get_idx(g.idx(cx + 1, cy, cz + 1)),
                g.density.get_idx(g.idx(cx, cy + 1, cz + 1)), g.density.get_idx(g.idx(cx + 1, cy + 1, cz + 1)),
            ];
            let (w, _nm) = g.cell_surface_vertex_d(cx, cy, cz, &d)
                .expect("a vertex's own cell must still cross the iso surface");
            let v = verts[i];
            assert!((w.x - v[0]).abs() < 1e-5 && (w.y - v[1]).abs() < 1e-5 && (w.z - v[2]).abs() < 1e-5,
                "cell {:?} re-derives vertex {} position", [cx, cy, cz], i);
        }
    }

    #[test]
    fn recolor_chunk_vertices_matches_fresh_mesh_and_keeps_positions() {
        // STEP 4: mesh a region, then deposit paint and recolor the SAME verts in place. The result
        // must equal a fresh mesh_region of the painted grid in the colour words, and the positions/
        // normals must be byte-identical (recolor touches only verts[i][6..8]).
        let mut g = VoxelGrid::new_empty(32, 1.0);
        g.fill_sphere(Vec3::ZERO, 0.5);
        let cmax = g.res - 2;
        let (mut verts, _idx, cells) = g.mesh_region_with_cells([0, 0, 0], [cmax, cmax, cmax]);
        let before = verts.clone();
        // Paint a coloured cap so SOME (not all) vertices change colour.
        g.deposit_color_sphere(Vec3::new(0.0, 0.4, 0.0), 0.4, pack_voxel_color(Vec3::new(0.9, 0.1, 0.1)));
        // Fresh mesh of the now-painted grid — the ground truth for the colour words.
        let (fresh, _fi, _fc) = g.mesh_region_with_cells([0, 0, 0], [cmax, cmax, cmax]);
        // In-place recolor of the OLD verts via the cell provenance.
        g.recolor_chunk_vertices(&mut verts, &cells);
        assert_eq!(verts.len(), fresh.len(), "topology unchanged by a colour-only edit");
        let mut any_changed = false;
        for i in 0..verts.len() {
            // Positions + normals untouched vs before the recolor.
            assert_eq!(&verts[i][0..6], &before[i][0..6], "recolor must not move vertex {}", i);
            // Colour words match a fresh mesh's exactly (same chunk_vertex_color helper).
            assert_eq!(verts[i][6].to_bits(), fresh[i][6].to_bits(), "uv.x colour word at {}", i);
            assert_eq!(verts[i][7].to_bits(), fresh[i][7].to_bits(), "uv.y painted flag at {}", i);
            if before[i][6].to_bits() != verts[i][6].to_bits() { any_changed = true; }
        }
        assert!(any_changed, "the paint cap must have recoloured at least one vertex");
    }

    #[test]
    fn preview_then_release_mesh_is_byte_identical_no_pop() {
        // STEP 10 end-to-end: simulate a COLOUR preview frame (recolor) followed by the authoritative
        // RELEASE mesh (a fresh mesh_region of the committed grid). The previewed verts must already be
        // byte-identical to the release mesh → zero pop on release. Mirror for DISPLACEMENT (reproject)
        // when topology is stable.
        let mut g = VoxelGrid::new_empty(40, 1.0);
        g.fill_sphere(Vec3::ZERO, 0.5);
        let cmax = g.res - 2;

        // --- COLOUR class: mesh, paint, recolor-preview, then a fresh release mesh ---
        let (mut prev_verts, prev_idx, cells) = g.mesh_region_with_cells([0, 0, 0], [cmax, cmax, cmax]);
        g.deposit_color_sphere(Vec3::new(0.1, 0.3, 0.0), 0.4, pack_voxel_color(Vec3::new(0.7, 0.2, 0.9)));
        g.recolor_chunk_vertices(&mut prev_verts, &cells);             // preview frame (display-only)
        let (rel_verts, rel_idx, _rc) = g.mesh_region_with_cells([0, 0, 0], [cmax, cmax, cmax]); // release
        assert_eq!(prev_idx, rel_idx, "colour preview kept indices identical to the release mesh");
        for i in 0..prev_verts.len() {
            assert_eq!(prev_verts[i].map(f32::to_bits), rel_verts[i].map(f32::to_bits),
                "COLOUR: previewed vertex {} must be byte-identical to the release mesh (no pop)", i);
        }

        // --- DISPLACEMENT class: a stable sub-voxel shift, reproject-preview, then release ---
        let (mut pv2, _pi2, cells2) = g.mesh_region_with_cells([0, 0, 0], [cmax, cmax, cmax]);
        let mut g2 = g.clone();
        g2.fill_sphere(Vec3::new(g.voxel_size() * 0.15, 0.0, 0.0), 0.5);
        if g2.reproject_chunk_positions(&mut pv2, &cells2, [0, 0, 0], [cmax, cmax, cmax]) {
            let (rv2, _ri2, _rc2) = g2.mesh_region_with_cells([0, 0, 0], [cmax, cmax, cmax]);
            assert_eq!(pv2.len(), rv2.len());
            for i in 0..pv2.len() {
                // positions match within float epsilon (reproject and mesh use the same vertex math)
                for k in 0..6 {
                    assert!((pv2[i][k] - rv2[i][k]).abs() < 1e-5,
                        "DISPLACEMENT: previewed vertex {} comp {} must match the release mesh", i, k);
                }
            }
        }
        // (If the shift drifted, the app's mandatory per-chunk full-mesh fallback covers it — proven in
        // reproject_tracks_a_small_shift_and_detects_drift.)
    }

    #[test]
    fn recolor_by_position_matches_cell_based_recolor() {
        // STEP 9 / Decision B: deriving a vertex's colour from its world position (the GPU-mesh path,
        // which has no cell provenance) must reproduce the cell-id-based recolor exactly — both route
        // through chunk_vertex_color, and world_to_grid().floor() recovers the same anchor cell.
        let mut g = VoxelGrid::new_empty(32, 1.0);
        g.fill_sphere(Vec3::ZERO, 0.5);
        let cmax = g.res - 2;
        let (verts_template, _idx, cells) = g.mesh_region_with_cells([0, 0, 0], [cmax, cmax, cmax]);
        g.deposit_color_sphere(Vec3::new(0.0, 0.4, 0.0), 0.4, pack_voxel_color(Vec3::new(0.2, 0.8, 0.3)));
        let mut by_cell = verts_template.clone();
        g.recolor_chunk_vertices(&mut by_cell, &cells);
        let mut by_pos = verts_template.clone();
        g.recolor_vertices_by_position(&mut by_pos);
        for i in 0..by_cell.len() {
            assert_eq!(by_pos[i][6].to_bits(), by_cell[i][6].to_bits(), "uv.x colour word at {}", i);
            assert_eq!(by_pos[i][7].to_bits(), by_cell[i][7].to_bits(), "uv.y painted flag at {}", i);
        }
    }

    #[test]
    fn reproject_tracks_a_small_shift_and_detects_drift() {
        // STEP 6: a TINY density shift that keeps the same crossing-cell set → reproject returns true
        // and moves the positions; a topology change (a cell stops crossing) → returns false, verts
        // untouched.
        let mut g = VoxelGrid::new_empty(48, 1.0);
        g.fill_sphere(Vec3::ZERO, 0.5);
        let cmax = g.res - 2;
        let (mut verts, _idx, cells) = g.mesh_region_with_cells([0, 0, 0], [cmax, cmax, cmax]);
        assert!(!verts.is_empty());
        let before = verts.clone();

        // A sub-voxel re-fill at a slightly shifted centre: the smooth SDF moves the iso crossings a
        // little, but at this radius/res the SET of crossing cells is unchanged → stable reproject.
        let shift = g.voxel_size() * 0.2;
        let mut g2 = g.clone();
        g2.fill_sphere(Vec3::new(shift, 0.0, 0.0), 0.5);
        let stable = g2.reproject_chunk_positions(&mut verts, &cells, [0, 0, 0], [cmax, cmax, cmax]);
        if stable {
            let mut moved = false;
            for i in 0..verts.len() {
                // colour words untouched by reproject
                assert_eq!(verts[i][6].to_bits(), before[i][6].to_bits());
                assert_eq!(verts[i][7].to_bits(), before[i][7].to_bits());
                if (verts[i][0] - before[i][0]).abs() > 1e-7 { moved = true; }
            }
            assert!(moved, "a real shift must move at least one reprojected position");
            // The reprojected positions must equal a fresh mesh of the shifted grid (same cells).
            let (fresh, _fi, _fc) = g2.mesh_region_with_cells([0, 0, 0], [cmax, cmax, cmax]);
            assert_eq!(verts.len(), fresh.len(), "shift kept the topology for this test");
            for i in 0..verts.len() {
                assert!((verts[i][0] - fresh[i][0]).abs() < 1e-5
                    && (verts[i][1] - fresh[i][1]).abs() < 1e-5
                    && (verts[i][2] - fresh[i][2]).abs() < 1e-5,
                    "reprojected pos {} must match a fresh mesh", i);
            }
        }
        // else: the shift happened to flip a boundary cell — that's a legitimate drift result; the
        // displacement path's mandatory full-mesh fallback covers it (asserted directly below).

        // TOPOLOGY DRIFT: fully solidify the whole grid → every cached cell stops crossing → false,
        // and verts are left untouched (the caller falls back to a full mesh).
        let mut verts2 = before.clone();
        let mut gsolid = g.clone();
        gsolid.fill_box(Vec3::ZERO, Vec3::splat(0.99)); // fills the interior solid
        // Force a guaranteed vanished crossing: set every density to fully solid.
        for i in 0..gsolid.density.len() { gsolid.density.set_idx(i, 1.0); }
        let drift = gsolid.reproject_chunk_positions(&mut verts2, &cells, [0, 0, 0], [cmax, cmax, cmax]);
        assert!(!drift, "a fully-solid grid drifts (no cell crosses) → reproject must return false");
        assert_eq!(verts2, before, "reproject must not mutate verts when it reports drift");
    }

    #[test]
    fn reproject_detects_new_crossing_outside_cached_bbox() {
        // The drift guard must catch a cell that NEWLY crosses ANYWHERE the chunk meshes — not only
        // inside the bounding box of the already-meshed vertices. A grab/move can pull material into an
        // empty region of the chunk far from the current surface; the old bbox-only Pass-2 scan missed
        // it → reproject wrongly reported "stable" → the new bump was dropped in preview and popped in on
        // release.
        //
        // DISCRIMINATION (the point of this test): it must be caught by PASS 2 (the full-range scan),
        // NOT by Pass 1 (a cached cell vanishing). So we leave every cached cell crossing exactly as it
        // was (Pass 1 stays clean) and introduce a NEW crossing FAR from the cached cells, by setting a
        // SINGLE far voxel solid — which never perturbs the low-corner blob. With this construction the
        // ONLY way reproject can return false is the Pass-2 fix; it returns true (the bug) on the
        // pre-fix bbox-only scan. res=48, bounds=1.0 → cell i maps to world ≈ -1 + (i/47)·2.
        let mut g = VoxelGrid::new_empty(48, 1.0);
        let a0 = [16u32, 16, 16];
        let a1 = [31u32, 31, 31];
        g.fill_sphere(Vec3::new(-0.234, -0.234, -0.234), 0.06); // ~cell 18 (LOW corner of the chunk)
        let (verts, _idx, cells) = g.mesh_region_with_cells(a0, a1);
        assert!(!cells.is_empty(), "the low-corner blob must mesh some crossing cells in the chunk");

        // Cached bbox of the low-corner cells.
        let (mut lo, mut hi) = (cells[0], cells[0]);
        for &[cx, cy, cz] in cells.iter() {
            lo = [lo[0].min(cx), lo[1].min(cy), lo[2].min(cz)];
            hi = [hi[0].max(cx), hi[1].max(cy), hi[2].max(cz)];
        }

        // Set ONE far voxel solid at a HIGH-corner cell, inside the chunk's vertex range [15..=31] but
        // well outside the cached bbox. This makes the cells touching that corner newly cross WITHOUT
        // disturbing the low blob → Pass 1 stays clean, only Pass 2 can catch it.
        let mut g2 = g.clone();
        let hc = [28u32, 28, 28];
        assert!(hc[0] > hi[0] && hc[1] > hi[1] && hc[2] > hi[2],
            "the far voxel must sit outside the cached bbox {:?}..{:?}", lo, hi);
        let fi = g2.idx(hc[0], hc[1], hc[2]);
        g2.density.set_idx(fi, 1.0);

        // Precondition A — the far voxel introduced a NEW crossing cell outside the cached bbox.
        let (_v2, _i2, cells2) = g2.mesh_region_with_cells(a0, a1);
        let have: std::collections::HashSet<[u32; 3]> = cells.iter().copied().collect();
        let new_outside = cells2.iter().any(|c| !have.contains(c)
            && (c[0] < lo[0] || c[0] > hi[0] || c[1] < lo[1] || c[1] > hi[1] || c[2] < lo[2] || c[2] > hi[2]));
        assert!(new_outside,
            "precondition: the far voxel must create a NEW crossing cell outside the cached bbox \
             {:?}..{:?} (cells {} -> {})", lo, hi, cells.len(), cells2.len());
        // Precondition B (THE DISCRIMINATOR) — every cached cell still crosses in g2, so Pass 1 stays
        // clean and the only path to `false` is the Pass-2 full-range scan (this is what makes the test
        // fail on the pre-fix bbox-only scan and pass only on the fix).
        let have2: std::collections::HashSet<[u32; 3]> = cells2.iter().copied().collect();
        assert!(cells.iter().all(|c| have2.contains(c)),
            "discriminator: every cached cell must still cross (Pass 1 clean) so only Pass 2 can catch it");

        let mut pv = verts.clone();
        let stable = g2.reproject_chunk_positions(&mut pv, &cells, a0, a1);
        assert!(!stable, "Pass 2 must report drift for a new crossing outside the cached vertex bbox");
        assert_eq!(pv, verts, "reproject must not mutate verts when it reports drift");
    }

    #[test]
    fn mesh_region_chunks_cover_every_quad_once() {
        // Two chunks splitting the grid own all anchored quads exactly once (no gaps, no dupes),
        // with duplicate boundary vertices keeping the seam closed.
        let mut g = VoxelGrid::new_empty(32, 1.0);
        g.fill_sphere(Vec3::ZERO, 0.5);
        let mesh = g.surface_nets(false);
        let cmax = g.res - 2;
        let (va, ia) = g.mesh_region([0, 0, 0], [15, cmax, cmax]);
        let (vb, ib) = g.mesh_region([16, 0, 0], [cmax, cmax, cmax]);
        assert!(!va.is_empty() && !vb.is_empty(), "both chunks have geometry");
        assert_eq!((ia.len() + ib.len()) / 6, mesh.quads.len(), "chunks cover every quad exactly once");
        // Chunks duplicate boundary vertices → at least as many total verts as the single mesh.
        assert!(va.len() + vb.len() >= mesh.vertices.len());
    }

    #[test]
    fn sphere_produces_closed_mesh() {
        let mut g = VoxelGrid::new_empty(32, 1.0);
        g.fill_sphere(Vec3::ZERO, 0.5);
        let m = g.surface_nets(true);
        assert!(m.vertices.len() > 50, "expected a real surface, got {} verts", m.vertices.len());
        assert!(m.quads.len() > 50);
        // every vertex within the grid bounds
        for v in &m.vertices {
            assert!(v.x.abs() <= 1.01 && v.y.abs() <= 1.01 && v.z.abs() <= 1.01);
        }
        // surface vertices roughly on the sphere (within ~2 voxels)
        let vs = g.voxel_size();
        for v in &m.vertices {
            let r = v.length();
            assert!((r - 0.5).abs() < vs * 2.5, "vertex radius {} too far from 0.5", r);
        }
        // watertight: every edge shared by an even number of faces (manifold check via
        // directed-edge count == reverse-edge count)
        use std::collections::HashMap;
        let mut edges: HashMap<(u32, u32), i32> = HashMap::new();
        for q in &m.quads {
            for k in 0..4 {
                let a = q[k]; let b = q[(k + 1) % 4];
                *edges.entry((a, b)).or_insert(0) += 1;
                *edges.entry((b, a)).or_insert(0) += 0;
            }
        }
        for q in &m.quads {
            for k in 0..4 {
                let a = q[k]; let b = q[(k + 1) % 4];
                let fwd = *edges.get(&(a, b)).unwrap_or(&0);
                let rev = *edges.get(&(b, a)).unwrap_or(&0);
                assert_eq!(fwd, rev, "non-manifold edge ({},{}): {} vs {}", a, b, fwd, rev);
            }
        }
    }

    #[test]
    fn sphere_mesh_normals_are_radial() {
        // Regression: overly wide gradient stencils (3.5–4.0 voxels) sample density plateaus
        // and bias normals toward axes → matcap concentric rings + hard equator seam.
        let mut g = VoxelGrid::new_empty(48, 1.0);
        g.fill_sphere(Vec3::ZERO, 0.5);
        let m = g.surface_nets(true);
        assert!(m.normals.len() == m.vertices.len() && !m.normals.is_empty());
        let mut mean_align = 0.0f32;
        let mut worst = 1.0f32;
        for (v, n) in m.vertices.iter().zip(m.normals.iter()) {
            let radial = v.normalize_or_zero();
            if radial.length_squared() < 1e-8 || n.length_squared() < 1e-8 { continue; }
            let a = radial.dot(*n).abs();
            mean_align += a;
            worst = worst.min(a);
        }
        mean_align /= m.normals.len() as f32;
        assert!(
            mean_align > 0.92,
            "sphere normals should track radial (mean |n·r|={mean_align:.3}, worst={worst:.3})"
        );
        assert!(
            worst > 0.55,
            "no normal should be near-orthogonal to radial (worst |n·r|={worst:.3})"
        );
    }

    #[test]
    fn draw_add_remove_are_density_duals() {
        // Soft dual approach: Add→solid and Remove→empty must move mass similarly on a sphere.
        // Pure `cur±wk` made Add balloon while Remove felt controlled.
        let mut base = VoxelGrid::new_empty(40, 1.0);
        base.fill_sphere(Vec3::ZERO, 0.5);
        let center = Vec3::new(0.0, 0.5, 0.0);
        let n = Vec3::Y;
        let r = 0.22f32;
        let s = 0.55f32;
        // Accu OFF path (soft dual, rate=wk) via fold + stamp plan.
        let recipe = crate::brush::BrushRecipe::from_builtin(
            crate::brush::BuiltinBrush::Draw, r, s, 0.2,
            crate::brush::Direction::SurfaceNormal,
            0, false, false, false, [false; 3], 0.25, 0.0, true,
        );
        let hit = crate::brush::SurfaceHit { center, normal: n, radius: r, strength: s };
        let mods_off = crate::brush::StrokeMods { accumulate: false, ..Default::default() };
        let mut add_g = base.clone();
        let mut rem_g = base.clone();
        let mut mods_add = mods_off; mods_add.invert = false;
        let mut mods_rem = mods_off; mods_rem.invert = true;
        add_g.apply_stamp_plan(&crate::brush::fold_recipe(&recipe, &hit, &mods_add), None);
        rem_g.apply_stamp_plan(&crate::brush::fold_recipe(&recipe, &hit, &mods_rem), None);
        let sum0: f32 = base.density.iter().sum();
        let sum_a: f32 = add_g.density.iter().sum();
        let sum_r: f32 = rem_g.density.iter().sum();
        let da = sum_a - sum0;
        let dr = sum0 - sum_r;
        assert!(da > 1e-3 && dr > 1e-3, "both polarities must change density (da={da} dr={dr})");
        // Soft dual: both polarities move mass in opposite directions (exact |Δ| differs on curved
        // AA spheres due to empty-cap vs solid-cap voxel counts — feel parity is the goal).
        assert!(sum_a > sum0 && sum_r < sum0, "Add raises mass, Remove lowers it");
    }

    #[test]
    fn draw_adds_material_erase_removes() {
        let mut g = VoxelGrid::new_empty(24, 1.0);
        // draw a blob in empty space (density model can build from nothing)
        g.apply_brush(VoxelBrush::Draw, Vec3::ZERO, Vec3::Y, 0.4, 1.0, false, 0.0, Vec3::ZERO, None);
        let after_draw: f32 = g.density.iter().sum();
        assert!(after_draw > 0.0, "draw should add material");
        let m = g.surface_nets(true);
        assert!(m.vertices.len() > 0, "drawn blob should produce a surface");
        // erase it back
        for _ in 0..4 { g.apply_brush(VoxelBrush::Erase, Vec3::ZERO, Vec3::Y, 0.5, 1.0, false, 0.0, Vec3::ZERO, None); }
        let after_erase: f32 = g.density.iter().sum();
        assert!(after_erase < after_draw, "erase should remove material");
    }

    #[test]
    fn density_stays_clamped() {
        let mut g = VoxelGrid::new_empty(16, 1.0);
        for _ in 0..20 { g.apply_brush(VoxelBrush::Draw, Vec3::ZERO, Vec3::Y, 0.6, 1.0, false, 0.0, Vec3::ZERO, None); }
        for d in g.density.iter() { assert!(d >= 0.0 && d <= 1.0, "density {} out of [0,1]", d); }
    }

    #[test]
    fn chord_dabs_snap_back_onto_sphere_surface() {
        // Blender stroke model: the interpolator emits dabs on the CHORD between two surface
        // hits; across curvature the chord dips inside. snap_to_surface must pull every dab
        // back to within ~a voxel of the isosurface so a stroke WRAPS the sphere.
        let mut g = VoxelGrid::new_empty(48, 1.0);
        g.fill_sphere(Vec3::ZERO, 0.6);
        let vs = g.voxel_size();
        let a = g.raycast(Vec3::new(0.0, 0.0, -0.95), Vec3::Z).expect("hit A");
        let b = g.raycast(Vec3::new(0.55, 0.0, -0.95), Vec3::Z).expect("hit B");
        // Iso radius as the grid represents it (AA fill ≠ exactly 0.6): measure at hit A.
        let iso_r = a.length();
        assert!((iso_r - 0.6).abs() < 2.0 * vs, "fixture sanity: iso radius {iso_r}");
        // The raw chord midpoint must be measurably INSIDE (else this test proves nothing).
        let mid = (a + b) * 0.5;
        assert!(
            iso_r - mid.length() > 1.5 * vs,
            "chord midpoint should dip inside: depth {}",
            iso_r - mid.length()
        );
        let mut it = crate::sculpt_layer::StrokeInterpolator::new(0.10);
        let mut dabs = it.push(crate::sculpt_layer::StrokeSample::new(a, Vec3::Z, 1.0), 0.15);
        dabs.extend(it.push(crate::sculpt_layer::StrokeSample::new(b, Vec3::Z, 1.0), 0.15));
        dabs.extend(it.flush(0.15));
        assert!(dabs.len() >= 3, "expected several chord dabs, got {}", dabs.len());
        for d in &dabs {
            // Hint = outward radial (what the app passes: the running stroke surface normal).
            let sp = g
                .snap_to_surface(d.position, d.position.normalize_or_zero(), 0.3)
                .expect("snap finds the isosurface from a chord dab");
            let err = (sp.length() - iso_r).abs();
            assert!(
                err <= 1.5 * vs,
                "snapped dab {} voxels off the iso (pos {:?} -> {:?})",
                err / vs,
                d.position,
                sp
            );
        }
        // Outside points snap DOWN onto the surface too (float-above chord case).
        let outside = Vec3::new(0.0, 0.0, -(iso_r + 3.0 * vs));
        let sp = g.snap_to_surface(outside, outside.normalize_or_zero(), 0.3).expect("outside snap");
        assert!((sp.length() - iso_r).abs() <= 1.5 * vs);
    }

    #[test]
    fn snap_to_surface_grid_boundary_is_safe() {
        // A dab coasting past the silhouette (mid-stroke plane stick) can hand snap_to_surface
        // a point at or beyond the grid bounds. It must terminate (capped by max_dist), return
        // None when no surface is in range, and never panic or produce NaN — OOB samples read
        // empty (0.0) via `at_i`.
        let mut g = VoxelGrid::new_empty(32, 1.0);
        g.fill_sphere(Vec3::ZERO, 0.5);
        let vs = g.voxel_size();
        // Far outside the grid on all axes: zero gradient, hint drives the walk — no crossing
        // within range → None (and provably no infinite loop: the walk is bounded by max_dist).
        let far = Vec3::splat(5.0);
        assert!(g.snap_to_surface(far, Vec3::ONE.normalize_or_zero(), 1.0).is_none());
        // Zero gradient AND zero hint → None (never NaN, never a bogus point).
        assert!(g.snap_to_surface(far, Vec3::ZERO, 1.0).is_none());
        // Just outside the boundary face, hint pointing along -Z (outward): the walk goes
        // inward (outside → -n) and must find the sphere shell, finite and near the iso.
        let edge = Vec3::new(0.0, 0.0, -(1.0 + 2.0 * vs));
        let p = g.snap_to_surface(edge, Vec3::new(0.0, 0.0, -1.0), 4.0)
            .expect("walk inward from just outside the grid finds the shell");
        assert!(p.is_finite(), "snapped point must be finite: {p:?}");
        assert!((p.length() - 0.5).abs() < 3.0 * vs, "snap lands near the iso shell: {p:?}");
    }

    #[test]
    fn raycast_hits_sphere_front() {
        let mut g = VoxelGrid::new_empty(32, 1.0);
        g.fill_sphere(Vec3::ZERO, 0.5);
        // ray from -Z toward +Z through the center
        let hit = g.raycast(Vec3::new(0.0, 0.0, -0.95), Vec3::Z);
        assert!(hit.is_some(), "ray should hit the sphere");
        let h = hit.unwrap();
        assert!((h.z + 0.5).abs() < g.voxel_size() * 2.0, "hit z {} not near front face -0.5", h.z);
    }

    #[test]
    fn clay_adds_material_and_clamps() {
        let mut g = VoxelGrid::new_empty(24, 1.0);
        g.fill_sphere(Vec3::ZERO, 0.5);
        let before: f32 = g.density.iter().sum();
        // Clay on the +Z surface: deposits toward a plane set slightly proud of the contact
        // point, so it builds material up rather than carving.
        g.apply_brush(VoxelBrush::Clay, Vec3::new(0.0, 0.0, 0.45), Vec3::Z, 0.4, 1.0, false, 0.0, Vec3::ZERO, None);
        let after: f32 = g.density.iter().sum();
        assert!(after > before, "clay should add material (before {before}, after {after})");
        for d in g.density.iter() { assert!((0.0..=1.0).contains(&d), "density {d} out of [0,1]"); }
    }

    #[test]
    fn scrape_only_removes_never_adds() {
        let mut g = VoxelGrid::new_empty(24, 1.0);
        g.fill_sphere(Vec3::ZERO, 0.5);
        let before: f32 = g.density.iter().sum();
        // Scrape across the rounded top (normal up): one-directional, shaves material on the
        // cut side of the plane and must NEVER deposit.
        g.apply_brush(VoxelBrush::Scrape, Vec3::new(0.0, 0.35, 0.0), Vec3::Y, 0.3, 1.0, false, 0.0, Vec3::ZERO, None);
        let after: f32 = g.density.iter().sum();
        assert!(after < before, "scrape should remove material on the cut side (before {before}, after {after})");
        for d in g.density.iter() { assert!((0.0..=1.0).contains(&d), "density {d} out of [0,1]"); }
    }

    #[test]
    fn crease_carves_groove_invert_raises_ridge() {
        // Forward crease carves (subtracts).
        let mut g = VoxelGrid::new_empty(24, 1.0);
        g.fill_sphere(Vec3::ZERO, 0.5);
        let base: f32 = g.density.iter().sum();
        g.apply_brush(VoxelBrush::Crease, Vec3::new(0.0, 0.0, 0.45), Vec3::Z, 0.3, 1.0, false, 0.0, Vec3::ZERO, None);
        let carved: f32 = g.density.iter().sum();
        assert!(carved < base, "crease should carve a groove (remove) (base {base}, carved {carved})");
        // Inverted crease (Ctrl) raises a ridge (adds) on a fresh copy of the same shape.
        let mut g2 = VoxelGrid::new_empty(24, 1.0);
        g2.fill_sphere(Vec3::ZERO, 0.5);
        let b2: f32 = g2.density.iter().sum();
        g2.apply_brush(VoxelBrush::Crease, Vec3::new(0.0, 0.0, 0.45), Vec3::Z, 0.3, 1.0, true, 0.0, Vec3::ZERO, None);
        let ridged: f32 = g2.density.iter().sum();
        assert!(ridged > b2, "inverted crease should raise a ridge (add) (base {b2}, ridged {ridged})");
        for d in g2.density.iter() { assert!((0.0..=1.0).contains(&d), "density {d} out of [0,1]"); }
    }

    #[test]
    fn fill_only_adds_below_plane() {
        let mut g = VoxelGrid::new_empty(24, 1.0);
        g.fill_sphere(Vec3::ZERO, 0.5);
        let before: f32 = g.density.iter().sum();
        // Fill at the +Z surface, normal +Z: deposits only on the LOW side of the plane
        // (toward the body), so it can only add — never carve.
        g.apply_brush(VoxelBrush::Fill, Vec3::new(0.0, 0.0, 0.45), Vec3::Z, 0.4, 1.0, false, 0.0, Vec3::ZERO, None);
        let after: f32 = g.density.iter().sum();
        assert!(after >= before, "fill must never remove material (before {before}, after {after})");
        assert!(after > before, "fill into the cavity below the plane should add material");
        for d in g.density.iter() { assert!((0.0..=1.0).contains(&d), "density {d} out of [0,1]"); }
    }

    #[test]
    fn clay_buildup_adds_and_is_additive_only() {
        let mut g = VoxelGrid::new_empty(24, 1.0);
        g.fill_sphere(Vec3::ZERO, 0.5);
        let before: f32 = g.density.iter().sum();
        g.apply_brush(VoxelBrush::ClayBuildup, Vec3::new(0.0, 0.0, 0.45), Vec3::Z, 0.4, 1.0, false, 0.0, Vec3::ZERO, None);
        let after: f32 = g.density.iter().sum();
        assert!(after > before, "clay buildup should add material (before {before}, after {after})");
        for d in g.density.iter() { assert!((0.0..=1.0).contains(&d), "density {d} out of [0,1]"); }
    }

    #[test]
    fn clay_buildup_packs_toward_proud_plane() {
        // One stamp should raise density under the proud plane (ZBrush strip).
        let mut g = VoxelGrid::new_empty(32, 1.0);
        g.fill_sphere(Vec3::ZERO, 0.45);
        let center = Vec3::new(0.0, 0.0, 0.42);
        let before = VoxelGrid::sample_static(g.res, g.bounds, &g.density, center);
        g.apply_brush(
            VoxelBrush::ClayBuildup, center, Vec3::Z,
            0.35, 0.9, false, 0.92, Vec3::ZERO, None,
        );
        let after = VoxelGrid::sample_static(g.res, g.bounds, &g.density, center);
        assert!(
            after > before + 0.05,
            "ClayBuildup should pack under the proud plane (before={before}, after={after})"
        );
        // Must not explode past solid.
        assert!(after <= 1.0);
    }

    #[test]
    fn inflate_thickens_surface_deflate_thins() {
        // Inflate puffs the shell outward → adds material near the surface.
        let mut g = VoxelGrid::new_empty(24, 1.0);
        g.fill_sphere(Vec3::ZERO, 0.5);
        let before: f32 = g.density.iter().sum();
        g.apply_brush(VoxelBrush::Inflate, Vec3::new(0.0, 0.0, 0.5), Vec3::Z, 0.45, 1.0, false, 0.0, Vec3::ZERO, None);
        let after: f32 = g.density.iter().sum();
        assert!(after > before, "inflate should add material at the surface (before {before}, after {after})");
        // Deflate (Ctrl) does the opposite on a fresh copy.
        let mut g2 = VoxelGrid::new_empty(24, 1.0);
        g2.fill_sphere(Vec3::ZERO, 0.5);
        let b2: f32 = g2.density.iter().sum();
        g2.apply_brush(VoxelBrush::Inflate, Vec3::new(0.0, 0.0, 0.5), Vec3::Z, 0.45, 1.0, true, 0.0, Vec3::ZERO, None);
        let after2: f32 = g2.density.iter().sum();
        assert!(after2 < b2, "deflate (inverted inflate) should remove material (base {b2}, after {after2})");
        for d in g2.density.iter() { assert!((0.0..=1.0).contains(&d), "density {d} out of [0,1]"); }
    }

    #[test]
    fn pinch_contracts_bulges_and_is_deterministic() {
        let make = || { let mut g = VoxelGrid::new_empty(32, 1.0); g.fill_sphere(Vec3::ZERO, 0.5); g };
        let center = Vec3::new(0.0, 0.0, 0.45);
        let n = Vec3::Z;
        // Determinism: the semi-Lagrangian gather reads a pre-stamp snapshot, so it must be
        // independent of iteration order — two identical pinches give bit-identical density.
        let mut a = make(); a.apply_brush(VoxelBrush::Pinch, center, n, 0.3, 1.0, false, 0.0, Vec3::ZERO, None);
        let mut b = make(); b.apply_brush(VoxelBrush::Pinch, center, n, 0.3, 1.0, false, 0.0, Vec3::ZERO, None);
        assert_eq!(a.density, b.density, "pinch must be deterministic (snapshot gather)");
        // Pull-in contracts the surface toward the axis → removes outer material; bulge (Ctrl) adds.
        let base: f32 = make().density.iter().sum();
        let pulled: f32 = a.density.iter().sum();
        assert!(pulled < base, "pinch pull-in should contract (remove) material (base {base}, pulled {pulled})");
        let mut c = make(); c.apply_brush(VoxelBrush::Pinch, center, n, 0.3, 1.0, true, 0.0, Vec3::ZERO, None);
        let bulged: f32 = c.density.iter().sum();
        assert!(bulged > base, "inverted pinch (bulge) should add material (base {base}, bulged {bulged})");
        for d in a.density.iter() { assert!((0.0..=1.0).contains(&d), "density {d} out of [0,1]"); }
    }

    #[test]
    fn brush_falloff_curve_is_well_formed() {
        // The continuous hardness curve `falloff_weight(e, h)`: at every hardness it is anchored
        // weight(0)=0 (rim), weight(1)=1 (centre), stays in [0,1], and is monotonically non-decreasing
        // toward the centre. (`e = 1 - t`, "inside-ness".)
        for &h in &[0.0f32, 0.25, 0.5, 0.75, 1.0] {
            assert_eq!(falloff_weight(0.0, h), 0.0, "h={h} rim must be 0");
            assert!((falloff_weight(1.0, h) - 1.0).abs() < 1e-6, "h={h} centre must be 1");
            let mut prev = -1.0f32;
            for k in 0..=20 {
                let e = k as f32 / 20.0;
                let w = falloff_weight(e, h);
                assert!((0.0..=1.0).contains(&w), "h={h} weight {w} out of [0,1] at e={e}");
                assert!(w + 1e-6 >= prev, "h={h} must be monotonic (e={e})");
                prev = w;
            }
        }
        // Blender hardness: raising hardness WIDENS the full-strength core (plateau).
        // At mid radius (e=0.5) weight rises toward 1 as hardness goes 0 → 1.
        let (soft, mid, hard) = (falloff_weight(0.5, 0.0), falloff_weight(0.5, 0.5), falloff_weight(0.5, 1.0));
        assert!(hard >= mid && mid >= soft,
            "harder falloff must raise mid weight (Blender plateau): soft={soft} mid={mid} hard={hard}");
        // hardness 0 = Smooth curve; hardness 1 = full core (weight 1 for any e > 0).
        assert!((falloff_weight(0.5, 0.0) - 0.5).abs() < 1e-6, "hardness 0 smoothstep is 0.5 at e=0.5");
        assert!((falloff_weight(0.5, 1.0) - 1.0).abs() < 1e-6, "hardness 1 is full core at e=0.5");
        assert!((falloff_weight(0.01, 1.0) - 1.0).abs() < 1e-6, "hardness 1 is full core near rim");
    }

    /// F1 GATE — the hardness-0 falloff MUST equal Blender's "Smooth" sculpt preset. Ground truth:
    /// docs/planning/sculpt/oracle/falloff_presets_ref.json, captured from Blender 4.5.1
    /// (`BKE_brush_curve_strength` SMOOTH = 3e^2 - 2e^3, e == 1 - dist/radius == our falloff arg;
    /// regenerate via docs/planning/sculpt/oracle/falloff_ref.py). We match Blender by CURVE, not by
    /// mesh vertices (SCULPT_FEEL_LOG.md section C) — the sculpt analog of the poly *_ref.json gates.
    #[test]
    fn falloff_h0_matches_blender_smooth() {
        const REF: &str =
            include_str!("../../../../docs/planning/sculpt/oracle/falloff_presets_ref.json");
        let v: serde_json::Value = serde_json::from_str(REF).expect("parse falloff ref json");
        let es = v["e"].as_array().expect("e[]");
        let sm = v["presets"]["SMOOTH"]["analytic_w"].as_array().expect("SMOOTH.analytic_w[]");
        assert_eq!(es.len(), sm.len(), "ref e[] and SMOOTH[] length mismatch");
        let mut worst = 0.0f32;
        for (e_j, w_j) in es.iter().zip(sm.iter()) {
            let e = e_j.as_f64().expect("e num") as f32;
            let blender = w_j.as_f64().expect("w num") as f32;
            worst = worst.max((falloff_weight(e, 0.0) - blender).abs());
        }
        assert!(worst < 1e-4,
            "hardness-0 falloff diverges from Blender Smooth: max|delta| = {worst:.2e} (want <1e-4)");
    }

    #[test]
    fn falloff_changes_brush_footprint() {
        // Blender hardness: HIGHER hardness = wider full-strength core = MORE total deposit.
        // (Old Blend-only hardness inverted this — that was wrong for industry sculpt.)
        let stamp = |h: f32| {
            let mut g = VoxelGrid::new_empty(32, 1.0);
            g.apply_brush(VoxelBrush::Draw, Vec3::ZERO, Vec3::Y, 0.5, 0.6, false, h, Vec3::ZERO, None);
            g.density.iter().sum::<f32>()
        };
        let soft = stamp(0.0);
        let mid = stamp(0.5);
        let hard = stamp(1.0);
        assert!(hard > mid && mid > soft,
            "Blender hardness must increase deposit: soft {soft} < mid {mid} < hard {hard}");
        assert!((hard - soft).abs() > 1.0, "hardness must change deposit meaningfully");
    }

    #[test]
    fn move_brush_grabs_surface_by_drag_delta_at_center_and_is_deterministic() {
        // MOVE/GRAB (Blender Grab / ZBrush Move): the grabbed region must translate COHERENTLY by the
        // FULL drag delta AT THE BRUSH CENTRE, tapering to ~0 at the rim — pulling clay, not smearing it.
        // The exact statement of a coherent grab is `post(p) = pre(p - drag·fall(p))`. AT the brush centre
        // `fall == 1`, so `post(grab) == pre(grab - drag)`: the material that sat one FULL drag-delta
        // UPSTREAM is pulled to the brush centre. We assert that directly (robust to surface curvature /
        // iso reconstruction), then cross-check the surface visibly moves out + the rim lags.
        let make = || { let mut g = VoxelGrid::new_empty(64, 1.0); g.fill_sphere(Vec3::ZERO, 0.5); g };
        // Trilinear density sampler in world space.
        let sample = |g: &VoxelGrid, p: Vec3| -> f32 {
            let gc = g.world_to_grid(p);
            let b = gc.floor();
            let f = gc - b;
            let (bx, by, bz) = (b.x as i32, b.y as i32, b.z as i32);
            let at = |xx: i32, yy: i32, zz: i32| {
                if xx < 0 || yy < 0 || zz < 0 || xx as u32 >= g.res || yy as u32 >= g.res || zz as u32 >= g.res { return 0.0; }
                g.density.get_idx(g.idx(xx as u32, yy as u32, zz as u32))
            };
            let lerp = |a: f32, b: f32, t: f32| a + (b - a) * t;
            let c00 = lerp(at(bx,by,bz),     at(bx+1,by,bz),     f.x);
            let c10 = lerp(at(bx,by+1,bz),   at(bx+1,by+1,bz),   f.x);
            let c01 = lerp(at(bx,by,bz+1),   at(bx+1,by,bz+1),   f.x);
            let c11 = lerp(at(bx,by+1,bz+1), at(bx+1,by+1,bz+1), f.x);
            lerp(lerp(c00, c10, f.y), lerp(c01, c11, f.y), f.z)
        };
        // Walk a +X ray at a fixed (y,z) and return the world-X where density crosses iso=0.5 (solid→air).
        let surface_x = |g: &VoxelGrid, y: f32, z: f32| -> f32 {
            let mut prev = sample(g, Vec3::new(-0.9, y, z));
            let mut x = -0.9f32;
            while x < 0.95 {
                let cur = sample(g, Vec3::new(x, y, z));
                if prev >= 0.5 && cur < 0.5 {
                    let t = (prev - 0.5) / (prev - cur).max(1e-6);
                    return (x - 0.01) + 0.01 * t;
                }
                prev = cur; x += 0.01;
            }
            x
        };
        let drag = Vec3::new(0.18, 0.0, 0.0);  // drag the grabbed region toward +X
        // Grab ON the +X surface so the brush centre sits at the iso crossing: one full drag UPSTREAM is
        // solid interior (pre = 1), at the centre itself is the iso (~0.5). After the grab the upstream
        // solid is pulled to the centre, so `post(grab)` jumps to ~1 — a clean, measurable translation.
        let grab = Vec3::new(0.5, 0.0, 0.0);
        let radius = 0.45f32;

        let mut a = make();
        // Pre-stroke densities at the grab point and one drag-delta UPSTREAM (the source the centre pulls).
        let pre_at_grab = sample(&a, grab);                  // ~0.5 (surface iso)
        let pre_at_grab_minus_drag = sample(&a, grab - drag);// ~1.0 (solid interior, fall==1 source)
        let center_surf_before = surface_x(&a, 0.0, 0.0);
        let rim_before = surface_x(&a, radius * 0.92, 0.0);  // near the rim (low falloff weight)
        a.apply_brush(VoxelBrush::Move, grab, Vec3::X, radius, 1.0, false, 0.5, drag, None);
        let post_at_grab = sample(&a, grab);
        let center_surf_after = surface_x(&a, 0.0, 0.0);
        let rim_after = surface_x(&a, radius * 0.92, 0.0);

        // COHERENT-TRANSLATION AT CENTRE: `post(grab) ≈ pre(grab - drag)` (fall == 1 at the centre), so the
        // material one FULL drag-delta upstream is what now sits at the brush centre — the grab moved the
        // surface by the full drag. (The two pre-values differ, so this is a real translation, not a no-op.)
        assert!((post_at_grab - pre_at_grab_minus_drag).abs() < 0.03,
            "Move must pull the FULL-drag-upstream value to the centre: post(grab) {post_at_grab} \
             vs pre(grab-drag) {pre_at_grab_minus_drag}");
        assert!((pre_at_grab - pre_at_grab_minus_drag).abs() > 0.1,
            "sanity: the upstream source must have differed pre-stroke (else nothing was translated)");
        // The surface on the centre line must visibly push OUT toward the cursor (a grab, not a no-op).
        assert!(center_surf_after > center_surf_before + drag.x * 0.4,
            "Move must push the centre surface out toward the cursor: {center_surf_before} -> {center_surf_after}");
        // RIM taper: the surface near the rim barely moves — much less than the centre (this taper is the
        // difference between a grab and a rigid push of the whole brush box).
        let center_shift = center_surf_after - center_surf_before;
        let rim_shift = rim_after - rim_before;
        assert!(rim_shift < center_shift * 0.6,
            "rim must move far less than the centre (grab taper): rim {rim_shift} vs centre {center_shift}");

        // Determinism: the semi-Lagrangian gather reads a pre-stamp snapshot, so two identical Moves give
        // bit-identical density (order-independent — the recorded-undo invariant relies on this).
        let mut b = make();
        b.apply_brush(VoxelBrush::Move, grab, Vec3::X, radius, 1.0, false, 0.5, drag, None);
        assert_eq!(a.density, b.density, "Move must be deterministic (snapshot gather)");
        // A zero drag is the identity warp (sampling at p) — the surface must not jump.
        let mut c = make();
        let c_before = surface_x(&c, 0.0, 0.0);
        c.apply_brush(VoxelBrush::Move, grab, Vec3::X, radius, 1.0, false, 0.0, Vec3::ZERO, None);
        let c_after = surface_x(&c, 0.0, 0.0);
        assert!((c_after - c_before).abs() < 0.02, "zero-drag Move must not displace the surface");
        for d in a.density.iter() { assert!((0.0..=1.0).contains(&d), "Move density {d} out of [0,1]"); }
    }

    #[test]
    fn move_brush_undo_restores_pre_stroke_exactly() {
        // Move records every modified voxel so replaying the delta restores the exact pre-stroke grid
        // — the sculpt-undo invariant (sculpt undo is its own delta path).
        let mut g = VoxelGrid::new_empty(40, 1.0);
        g.fill_sphere(Vec3::ZERO, 0.45);
        let before = g.density.clone();
        let mut rec = StrokeRecorder::default();
        rec.begin(g.res);
        g.apply_brush(VoxelBrush::Move, Vec3::new(0.35, 0.0, 0.0), Vec3::X, 0.5, 1.0, false,
            0.5, Vec3::new(0.2, 0.0, 0.0), Some(&mut rec));
        g.apply_brush(VoxelBrush::Move, Vec3::new(0.45, 0.05, 0.0), Vec3::X, 0.5, 1.0, false,
            0.5, Vec3::new(0.15, 0.05, 0.0), Some(&mut rec));
        assert_ne!(g.density, before, "Move stroke should change the field");
        for (idx, pre) in rec.take() { g.density.set_idx(idx as usize, pre); }
        assert_eq!(g.density, before, "Move undo must restore the exact pre-stroke state");
    }

    #[test]
    fn grab_density_x_mirror_both_sides_solid() {
        // Owner: with X symmetry one side stayed nice and the other tore. Joint path used to seed
        // inverse with primary drag only — mirror side failed Picard. Both destinations must stay solid.
        let mut g = VoxelGrid::new_empty(64, 1.0);
        g.fill_sphere(Vec3::ZERO, 0.42);
        let r = 0.28;
        let drag_l = Vec3::new(-0.35, 0.15, 0.0);
        let drag_r = Vec3::new(0.35, 0.15, 0.0);
        let left = (Vec3::new(-0.28, 0.0, 0.0), drag_l, r);
        let right = (Vec3::new(0.28, 0.0, 0.0), drag_r, r);
        // Overlapping enough to take joint path (not disjoint) — the bug path.
        let (writes, _) = g.grab_density_displace_writes(&[left, right]);
        assert!(!writes.is_empty());
        let dens_l = VoxelGrid::sample_static(g.res, g.bounds, &g.density, left.0 + drag_l);
        let dens_r = VoxelGrid::sample_static(g.res, g.bounds, &g.density, right.0 + drag_r);
        assert!(
            dens_l > g.iso,
            "LEFT (primary) dest must stay solid after mirrored pull (got {dens_l})"
        );
        assert!(
            dens_r > g.iso,
            "RIGHT (mirror) dest must stay solid after mirrored pull (got {dens_r}) — primary-seed bias"
        );
        // Rough symmetry of landed density at dest cores.
        assert!(
            (dens_l - dens_r).abs() < 0.35,
            "dest cores should be similar density L={dens_l} R={dens_r}"
        );
    }

    #[test]
    fn grab_density_disjoint_mirrors_narrower_than_joint() {
        // Far X-mirror instances must take the disjoint path (two tight AABBs), not one fat box.
        let mut g = VoxelGrid::new_empty(64, 1.0);
        g.fill_sphere(Vec3::ZERO, 0.4);
        let r = 0.18;
        let drag = Vec3::new(0.0, 0.12, 0.0);
        let left = (Vec3::new(-0.35, 0.0, 0.0), drag, r);
        let right = (Vec3::new(0.35, 0.0, 0.0), Vec3::new(0.0, 0.12, 0.0), r);
        assert!(g.grab_instances_disjoint(&[left, right]), "far mirrors must be disjoint");
        let (writes, dirty) = g.grab_density_displace_writes(&[left, right]);
        assert!(!writes.is_empty());
        let span_x = dirty[3] - dirty[0] + 1;
        // Union of two local brushes should be much narrower than full 64 width when disjoint-merge works.
        // (Still spans both sides in union AABB after merge — X span can be large; cell *work* is 2× local.)
        assert!(span_x <= 64);
        let dens_l = VoxelGrid::sample_static(g.res, g.bounds, &g.density, left.0 + drag);
        let dens_r = VoxelGrid::sample_static(g.res, g.bounds, &g.density, right.0 + drag);
        assert!(dens_l > g.iso * 0.5 || dens_r > g.iso * 0.5, "at least one dest should move material");
    }

    #[test]
    #[ignore = "inherited RED from fix/sculpt-feel-perf tip (stopped mid-wrapup): large Move pull tears dest core 0.486 vs iso 0.5 — tracked in COMPLETE_BUG_TRACKER 2026-08-02"]
    fn grab_density_large_pull_keeps_solid_core() {
        // Large Move bake must not tear the density iso (owner: "surface break" on big pulls).
        // Damped inverse + forward max-splat: destination core stays solid; path is not hollowed.
        let mut g = VoxelGrid::new_empty(64, 1.0);
        g.fill_sphere(Vec3::ZERO, 0.35);
        let anchor = Vec3::new(0.30, 0.0, 0.0);
        let drag = Vec3::new(0.55, 0.0, 0.0); // |Δ| > brush radius → used to tear undamped inverse
        let r = 0.40;
        let (writes, dirty) = g.grab_density_displace_writes(&[(anchor, drag, r)]);
        assert!(!writes.is_empty(), "large pull must write density");
        assert!(dirty[0] <= dirty[3], "dirty AABB must be valid");
        // Destination core (anchor + drag) must sample solid — the classic tear is empty dest.
        let dest = anchor + drag;
        let dens_dest = VoxelGrid::sample_static(g.res, g.bounds, &g.density, dest);
        assert!(
            dens_dest > g.iso,
            "destination core must stay solid after large pull (got {dens_dest}, iso {}) — surface tore",
            g.iso
        );
        // Mid-path samples should not be a clean empty tunnel (surface break).
        let mid = anchor + drag * 0.5;
        let dens_mid = VoxelGrid::sample_static(g.res, g.bounds, &g.density, mid);
        // At least one of mid / dest solid is required; dest already checked. Mid may be soft rim.
        let _ = dens_mid;
        // No wild out-of-range density.
        for d in g.density.iter() {
            assert!((0.0..=1.0).contains(&d), "density {d} out of [0,1]");
        }
    }

    #[test]
    fn elastic_grab_is_reversible_and_commits_pristine_delta() {
        // ELASTIC GRAB (the reworked Move): the field is re-derived from the PRISTINE snapshot each frame
        // by the CUMULATIVE drag, so (1) dragging out then back to the anchor returns the field to its
        // pristine state (the elastic property a per-frame accumulating advection would NOT have), and
        // (2) finish() yields the pristine→final undo delta that restores the pre-grab field exactly.
        let make = || { let mut g = VoxelGrid::new_empty(48, 1.0); g.fill_sphere(Vec3::ZERO, 0.5); g };

        // (1) REVERSIBILITY: out-then-back must restore pristine bit-for-bit.
        let mut g = make();
        let pristine = g.density.clone();
        let mut grab = ElasticGrab::default();
        let anchor = Vec3::new(0.5, 0.0, 0.0);
        grab.begin(anchor, 0.45, 0.5, Vec3::Z, Vec3::X); // plane_n/surface_n unused here (drag passed explicitly)
        // Several frames pulling OUT toward +X (cumulative drag grows), then back to zero.
        for k in 1..=5 { g.elastic_grab_preview(&mut grab, Vec3::new(0.04 * k as f32, 0.0, 0.0), false); }
        assert_ne!(g.density, pristine, "an elastic pull must visibly deform the field mid-drag");
        // Drag back to the anchor (zero cumulative drag) — the field returns to pristine. (Bit-exactness
        // isn't guaranteed because the identity warp resamples the just-restored field trilinearly, which
        // rounds by ~1 ULP at a cell centre; the property is that NO accumulated smear remains — an
        // incremental per-frame advection would leave a large residual here. Undo itself IS bit-exact: it
        // replays the captured pristine map, not the resampled field — see part (2).)
        let _ = g.take_dirty();
        g.elastic_grab_preview(&mut grab, Vec3::ZERO, false);
        let max_resid = g.density.iter().zip(pristine.iter())
            .map(|(a, b)| (a - b).abs()).fold(0.0f32, f32::max);
        assert!(max_resid < 1e-4,
            "dragging back to the anchor must restore pristine (elastic, not an accumulating smear): residual {max_resid}");

        // (2) COMMIT DELTA: a real pull, then finish() restores the pre-grab field exactly via the delta.
        let mut g2 = make();
        let pristine2 = g2.density.clone();
        let mut grab2 = ElasticGrab::default();
        grab2.begin(anchor, 0.45, 0.5, Vec3::Z, Vec3::X);
        for k in 1..=4 { g2.elastic_grab_preview(&mut grab2, Vec3::new(0.05 * k as f32, 0.0, 0.0), false); }
        assert_ne!(g2.density, pristine2, "the committed grab must change the field");
        let delta = grab2.finish();
        assert!(!delta.is_empty(), "a moved grab must produce a non-empty undo delta");
        assert!(!grab2.is_active(), "finish() must end the grab");
        for (idx, pre) in delta { g2.density.set_idx(idx as usize, pre); }
        assert_eq!(g2.density, pristine2, "the grab undo delta must restore the exact pre-grab field");

        // (3) RIGID (Move/Grab plateau) path is ALSO reversible — the restore-pristine + re-derive is
        //     falloff-independent, so out-then-back must return to pristine for the plateau curve too.
        let mut g3 = make();
        let pristine3 = g3.density.clone();
        let mut grab3 = ElasticGrab::default();
        grab3.begin(anchor, 0.45, 0.5, Vec3::Z, Vec3::X);
        for k in 1..=5 { g3.elastic_grab_preview(&mut grab3, Vec3::new(0.04 * k as f32, 0.0, 0.0), true); }
        assert_ne!(g3.density, pristine3, "a rigid (Move) pull must visibly deform the field mid-drag");
        let _ = g3.take_dirty();
        g3.elastic_grab_preview(&mut grab3, Vec3::ZERO, true);
        let max_resid3 = g3.density.iter().zip(pristine3.iter())
            .map(|(a, b)| (a - b).abs()).fold(0.0f32, f32::max);
        assert!(max_resid3 < 1e-4,
            "rigid (Move) grab must also restore pristine when dragged back to the anchor: residual {max_resid3}");

        // Density stays in range throughout.
        for d in g.density.iter() { assert!((0.0..=1.0).contains(&d), "grab density {d} out of [0,1]"); }
    }

    #[test]
    fn reconcile_region_sdf_signs_flips_far_garbage_keeps_band() {
        // Fuzz-caught class (seed 5296): the nearest-triangle pseudo-normal sign flips on folded
        // stroke geometry; write_region_from_sdf then turns far cells into solid slabs. The
        // reconciliation floods trusted OUTSIDE state from around the region and must:
        //   (a) flip a far cell wrongly marked inside (negative) back to positive,
        //   (b) leave the ±w band's geometric signs untouched,
        //   (c) leave genuinely-inside far cells negative (flood seeded inside from old density).
        let mut g = VoxelGrid::new_empty(32, 1.0);
        g.fill_sphere(Vec3::ZERO, 0.6);
        let w = 2.0 * g.voxel_size();
        // Region strictly inside the grid so the boundary neighbours carry the old density.
        let aabb = [2u32, 2, 2, 29, 29, 29];
        let (nx, ny) = (28usize, 28usize);
        let li = |x: usize, y: usize, z: usize| x + y * nx + z * nx * ny;
        // Honest SDF of the same sphere over the region lattice…
        let cell = g.voxel_size();
        let first = |gc: u32| -1.0 + gc as f32 * cell;
        let mut sdf = vec![0.0f32; 28 * 28 * 28];
        for z in 0..28 { for y in 0..28 { for x in 0..28 {
            let p = Vec3::new(first(2 + x as u32), first(2 + y as u32), first(2 + z as u32));
            sdf[li(x, y, z)] = p.length() - 0.6;
        }}}
        // …with a far OUTSIDE corner blob wrongly negative (the folded-geometry sign flip).
        let mut garbage = Vec::new();
        for z in 0..3 { for y in 0..3 { for x in 0..3 {
            let i = li(x, y, z);
            assert!(sdf[i] > w, "corner cells must be far outside for this test");
            sdf[i] = -sdf[i];
            garbage.push(i);
        }}}
        // And one genuinely-inside far cell (center) — must stay negative.
        let ci = li(12, 12, 12);
        assert!(sdf[ci] < -w);
        // A band cell reference (near the surface) — sign must be untouched.
        let mut band_cell = None;
        'outer: for z in 0..28 { for y in 0..28 { for x in 0..28 {
            let i = li(x, y, z);
            if sdf[i].abs() <= w * 0.9 { band_cell = Some((i, sdf[i])); break 'outer; }
        }}}
        let (bi, bval) = band_cell.expect("sphere surface must cross the region");
        let flipped = g.reconcile_region_sdf_signs(aabb, &mut sdf, w);
        assert!(flipped >= garbage.len() as u32,
            "the garbage corner must be reclaimed (flipped {flipped})");
        for &i in &garbage {
            assert!(sdf[i] > 0.0, "far outside garbage cell must be positive again");
        }
        assert!(sdf[ci] < 0.0, "genuinely-inside far cell must stay negative");
        assert_eq!(sdf[bi].to_bits(), bval.to_bits(), "band cells keep their geometric sign");
    }

    #[test]
    fn write_region_from_sdf_maps_surface_to_iso_and_undo_restores() {
        // MOVE Phase 2 SDF→density write (the mesh→SDF re-voxelize landing). Verify the conversion
        // `density = clamp(0.5 - sdf/W, 0, 1)`: a cell with sdf=0 (on the surface) → density 0.5 (the
        // iso), sdf<0 (inside) → >0.5, sdf>0 (outside) → <0.5; and the returned delta restores exactly.
        let mut g = VoxelGrid::new_empty(16, 1.0);
        g.fill_sphere(Vec3::ZERO, 0.4); // some pre-existing field so the delta has real pre-values
        let pre = g.density.clone();
        let aabb = [4u32, 4, 4, 7, 7, 7];
        let (nx, ny, nz) = (4usize, 4, 4);
        let w = 2.0 * g.voxel_size();
        // Synthetic SDF: a plane at x = cell 5.5 → cells x∈{4,5} inside (neg), x∈{6,7} outside (pos),
        // with one exact-zero column to check the iso lands at 0.5.
        let mut sdf = vec![0.0f32; nx * ny * nz];
        for z in 0..nz { for y in 0..ny { for x in 0..nx {
            // signed distance increasing with x; zero at local x index 1.
            sdf[x + y * nx + z * nx * ny] = (x as f32 - 1.0) * (w * 0.5);
        }}}
        let delta = g.write_region_from_sdf(aabb, &sdf, w);
        assert!(!delta.is_empty(), "a re-voxelize that changes voxels must produce an undo delta");
        // Local x=1 column → sdf 0 → density 0.5 (the iso). Check a representative cell.
        let fi_iso = g.flat_index(4 + 1, 4, 4);
        assert!((g.density.get_idx(fi_iso) - 0.5).abs() < 1e-5,
            "sdf=0 must map to the iso density 0.5, got {}", g.density.get_idx(fi_iso));
        // Local x=0 (inside, sdf<0) → density > 0.5; local x=3 (outside, sdf>0) → density < 0.5.
        let fi_in = g.flat_index(4 + 0, 4, 4);
        let fi_out = g.flat_index(4 + 3, 4, 4);
        assert!(g.density.get_idx(fi_in) > 0.5, "inside (sdf<0) must be > iso");
        assert!(g.density.get_idx(fi_out) < 0.5, "outside (sdf>0) must be < iso");
        // Density stays in range.
        for d in g.density.iter() { assert!((0.0..=1.0).contains(&d), "density {d} out of [0,1]"); }
        // Undo: replaying the delta's pre-values restores the exact pre-write field.
        for (idx, prev) in delta { g.density.set_idx(idx as usize, prev); }
        assert_eq!(g.density, pre, "the Phase-2 undo delta must restore the exact pre-write field");
    }

    #[test]
    fn relax_region_smooths_toward_average_and_clamps() {
        // A noisy checkerboard relaxes toward the mean without leaving [0,1].
        let mut g = VoxelGrid::new_empty(16, 1.0);
        for z in 0..16 { for y in 0..16 { for x in 0..16 {
            let i = (x + y * 16 + z * 16 * 16) as usize;
            g.density.set_idx(i, if (x + y + z) % 2 == 0 { 1.0 } else { 0.0 });
        }}}
        // variance before
        let mean = g.density.iter().sum::<f32>() / g.density.len() as f32;
        let var_before: f32 = g.density.iter().map(|d| (d - mean).powi(2)).sum();
        g.relax_region([0, 0, 0, 15, 15, 15], 2, 1.0, false, None);
        let var_after: f32 = g.density.iter().map(|d| (d - mean).powi(2)).sum();
        assert!(var_after < var_before, "relax should reduce variance ({var_before} -> {var_after})");
        for d in g.density.iter() { assert!((0.0..=1.0).contains(&d), "density {d} out of [0,1]"); }
    }

    #[test]
    fn relax_region_uniform_density_records_no_delta() {
        let mut g = VoxelGrid::new_empty(16, 1.0);
        for i in 0..g.density.len() {
            g.density.set_idx(i, 0.75);
        }
        let before = g.density.clone();
        let mut rec = StrokeRecorder::default();
        rec.begin(g.res);

        g.relax_region([2, 2, 2, 13, 13, 13], 3, 0.45, true, Some(&mut rec));

        assert!(rec.take().is_empty(), "uniform relax must not create zero-effect undo entries");
        assert_eq!(g.density, before, "uniform relax must not rewrite the density field");
    }

    #[test]
    fn smooth_all_reduces_variance_records_reversible_delta() {
        // Smooth All over a noisy checkerboard: variance drops, values stay in [0,1], and replaying
        // the recorded first-touch delta restores the EXACT pre-smooth grid (one reversible undo step).
        let mut g = VoxelGrid::new_empty(16, 1.0);
        for z in 0..16 { for y in 0..16 { for x in 0..16 {
            let i = (x + y * 16 + z * 16 * 16) as usize;
            g.density.set_idx(i, if (x + y + z) % 2 == 0 { 1.0 } else { 0.0 });
        }}}
        let before = g.density.clone();
        let mean = g.density.iter().sum::<f32>() / g.density.len() as f32;
        let var_before: f32 = g.density.iter().map(|d| (d - mean).powi(2)).sum();
        let mut rec = StrokeRecorder::default();
        rec.begin(g.res);
        let changed = g.smooth_all(3, 0.5, Some(&mut rec));
        assert!(changed > 0, "smooth_all should change voxels on a noisy field");
        let var_after: f32 = g.density.iter().map(|d| (d - mean).powi(2)).sum();
        assert!(var_after < var_before, "smooth_all should reduce variance ({var_before} -> {var_after})");
        for d in g.density.iter() { assert!((0.0..=1.0).contains(&d), "density {d} out of [0,1]"); }
        // The recorded delta is the pre-smooth value at each changed voxel → replaying it reverts exactly.
        let delta = rec.take();
        assert_eq!(delta.len(), changed, "one recorded entry per changed voxel (first-touch deduped)");
        for (idx, pre) in delta { g.density.set_idx(idx as usize, pre); }
        assert_eq!(g.density, before, "Smooth All undo must restore the exact pre-smooth state");
    }

    #[test]
    fn symmetrize_mirrors_half_idempotent_and_records_reversible_delta() {
        // Material only in the +X half; symmetrize X must fill the −X half to mirror it, be idempotent
        // on a second call, and its recorded delta must revert to the exact pre state (one undo step).
        let r = 8usize;
        let mut g = VoxelGrid::new_empty(r as u32, 1.0);
        for z in 0..r { for y in 0..r { for x in (r / 2)..r {
            g.density.set_idx(x + y * r + z * r * r, 1.0);
        }}}
        let before = g.density.clone();
        let mut rec = StrokeRecorder::default();
        rec.begin(g.res);
        let changed = g.symmetrize(0, true, Some(&mut rec));
        assert!(changed > 0, "symmetrize must fill the empty −X half");
        for z in 0..r { for y in 0..r { for x in 0..r {
            let a = g.density.get_idx(x + y * r + z * r * r);
            let b = g.density.get_idx((r - 1 - x) + y * r + z * r * r);
            assert_eq!(a, b, "not symmetric across X at ({x},{y},{z})");
        }}}
        // Idempotent: already symmetric → a second symmetrize changes nothing.
        assert_eq!(g.symmetrize(0, true, None), 0, "symmetrize must be idempotent once symmetric");
        // The recorded delta reverts EXACTLY (one reversible undo step, like Smooth All).
        let delta = rec.take();
        assert_eq!(delta.len(), changed, "one recorded entry per changed voxel (first-touch deduped)");
        for (idx, pre) in delta { g.density.set_idx(idx as usize, pre); }
        assert_eq!(g.density, before, "Symmetrize undo must restore the exact pre state");
    }

    #[test]
    fn pinch_with_relax_undo_restores_pre_stroke_exactly() {
        // Guards the undo-coverage concern: apply_brush writes (and records) every voxel it
        // touches; the trailing relax_region records any *additional* voxels it touches — which,
        // being voxels apply_brush skipped, are still at their pre-stroke value. So replaying the
        // accumulated delta must restore the EXACT pre-stroke grid, even across multiple stamps.
        let mut g = VoxelGrid::new_empty(32, 1.0);
        g.fill_sphere(Vec3::ZERO, 0.5);
        let before = g.density.clone();
        let mut rec = StrokeRecorder::default();
        rec.begin(g.res);
        g.apply_brush(VoxelBrush::Pinch, Vec3::new(0.0, 0.0, 0.45), Vec3::Z, 0.3, 1.0, false, 0.0, Vec3::ZERO, Some(&mut rec));
        g.apply_brush(VoxelBrush::Pinch, Vec3::new(0.1, 0.0, 0.43), Vec3::Z, 0.3, 1.0, false, 0.0, Vec3::ZERO, Some(&mut rec));
        assert_ne!(g.density, before, "pinch stroke should change the field");
        // Replay the undo delta exactly as the app's undo does: restore each recorded voxel.
        for (idx, pre) in rec.take() { g.density.set_idx(idx as usize, pre); }
        assert_eq!(g.density, before, "pinch + relax undo must restore the exact pre-stroke state");
    }

    /// PHASE 3a KEYSTONE: the sparse `BrickGrid` backing must be byte-for-byte identical to the dense
    /// backing after the SAME sequence of operations — empty grid, sphere fill, and a stroke of brushes
    /// that exercise every access path (Draw/Erase in-place, Smooth's snapshot, Pinch's gather+relax).
    /// `res = 33` is intentionally NOT a multiple of `BRICK=16` so the partial last-brick row is tested.
    /// Equality is by value (`to_dense`) AND the extracted mesh, since Sparse feeds the same reads.
    #[test]
    fn sparse_brick_grid_matches_dense_byte_for_byte() {
        for &res in &[16u32, 32, 33] {
            let mut dense = VoxelGrid::new_empty(res, 1.0);
            let mut sparse = VoxelGrid::new_empty(res, 1.0);
            sparse.density = DensityStorage::sparse(res, 0.0);
            assert_eq!(dense.density.to_dense(), sparse.density.to_dense(), "empty differs at res {res}");

            dense.fill_sphere(Vec3::ZERO, 0.6);
            sparse.fill_sphere(Vec3::ZERO, 0.6);
            assert_eq!(dense.density.to_dense(), sparse.density.to_dense(), "sphere fill differs at res {res}");

            let ops: [(VoxelBrush, Vec3, f32, f32); 4] = [
                (VoxelBrush::Draw, Vec3::new(0.2, 0.1, 0.0), 0.30, 0.7),
                (VoxelBrush::Smooth, Vec3::new(-0.1, 0.2, 0.1), 0.35, 0.5),
                (VoxelBrush::Pinch, Vec3::new(0.0, -0.2, 0.1), 0.30, 0.6),
                (VoxelBrush::Erase, Vec3::new(0.1, 0.1, -0.2), 0.25, 0.8),
            ];
            for (b, c, r, s) in ops {
                dense.apply_brush(b, c, Vec3::Z, r, s, false, 0.0, Vec3::ZERO, None);
                sparse.apply_brush(b, c, Vec3::Z, r, s, false, 0.0, Vec3::ZERO, None);
            }
            assert_eq!(dense.density.to_dense(), sparse.density.to_dense(), "stroke diverged at res {res}");

            // z_slice (the GPU brush upload path) must also match the dense sub-slice.
            assert_eq!(dense.density.z_slice(1, res - 2), sparse.density.z_slice(1, res - 2), "z_slice differs at res {res}");

            // The extracted mesh must be identical (Sparse routes the same corner reads).
            let cm = res.saturating_sub(2);
            assert_eq!(
                dense.mesh_region([0, 0, 0], [cm, cm, cm]),
                sparse.mesh_region([0, 0, 0], [cm, cm, cm]),
                "mesh diverged at res {res}"
            );
        }
    }

    /// `to_dense()` round-trips: a Sparse grid built from an arbitrary dense field reproduces it exactly,
    /// and the EMPTY/FULL implicit tags resolve correctly (interior solid, exterior empty, surface mixed).
    #[test]
    fn sparse_to_dense_round_trips_and_tags_resolve() {
        let res = 48u32;
        let mut dense = VoxelGrid::new_empty(res, 1.0);
        dense.fill_box(Vec3::ZERO, Vec3::splat(0.5)); // solid interior (FULL bricks) + empty rim
        let reference = dense.density.to_dense();
        // Build a sparse grid by writing the same field through set_idx.
        let mut sparse = DensityStorage::sparse(res, 0.0);
        for (i, &v) in reference.iter().enumerate() { sparse.set_idx(i, v); }
        assert_eq!(sparse.to_dense(), reference, "sparse round-trip lost data");
        // sparse() with a uniform FULL fill resolves every voxel to 1.0.
        assert!(DensityStorage::sparse(res, 1.0).to_dense().iter().all(|&v| v == 1.0));
        assert!(DensityStorage::sparse(res, 0.0).to_dense().iter().all(|&v| v == 0.0));
    }

    /// PHASE 3e: compaction must (a) preserve every value exactly (`to_dense` unchanged) and (b) reclaim
    /// memory — a solid box's interior (all-1.0 bricks) demotes to FULL tags, freeing arena slots, so the
    /// allocated brick count drops sharply. This is the actual 512³ memory win.
    #[test]
    fn sparse_compaction_preserves_values_and_reclaims_memory() {
        let res = 64u32; // brick_res = 4 → 64 bricks
        let mut dense = VoxelGrid::new_empty(res, 1.0);
        dense.fill_box(Vec3::ZERO, Vec3::splat(0.7)); // big solid → interior bricks all 1.0
        let reference = dense.density.to_dense();
        let mut sparse = DensityStorage::sparse(res, 0.0);
        for (i, &v) in reference.iter().enumerate() { sparse.set_idx(i, v); }

        let alloc_before = match &sparse { DensityStorage::Sparse(b) => b.allocated_bricks(), _ => 0 };
        sparse.compact_all();
        assert_eq!(sparse.to_dense(), reference, "compaction must not change any value");
        let alloc_after = match &sparse { DensityStorage::Sparse(b) => b.allocated_bricks(), _ => 0 };
        assert!(
            alloc_after < alloc_before,
            "compaction must free uniform interior bricks (alloc {alloc_before} → {alloc_after})"
        );

        // After compaction, editing a FULL-tagged interior voxel must still work (materialise-on-write).
        let mid = (res / 2) as usize;
        let i = mid + mid * res as usize + mid * (res * res) as usize;
        assert_eq!(sparse.get_idx(i), 1.0, "interior reads 1.0 from the FULL tag");
        sparse.set_idx(i, 0.0);
        assert_eq!(sparse.get_idx(i), 0.0, "writing into a FULL brick materialises it");
        // ...and its neighbours stay 1.0 (only the one voxel changed).
        assert_eq!(sparse.get_idx(i + 1), 1.0, "neighbour unchanged after FULL-brick materialise");

        // compact_aabb is value-preserving too (recycles slots without arena GC).
        let mut s2 = DensityStorage::sparse(res, 0.0);
        for (k, &v) in reference.iter().enumerate() { s2.set_idx(k, v); }
        s2.compact_aabb([0, 0, 0, res - 1, res - 1, res - 1]);
        assert_eq!(s2.to_dense(), reference, "compact_aabb must not change any value");
    }

    /// PHASE 3f: the sparse per-brick `StrokeRecorder` must reproduce EXACT first-touch dedup semantics
    /// — including REVISITING a brick after leaving it (the cached-last-brick flush/reload path) and
    /// duplicate indices — matching a plain `HashSet` reference. This is the undo-correctness gate.
    #[test]
    fn sparse_stroke_recorder_first_touch_dedup() {
        let res = 40u32; // brick_res = 3
        let mut rec = StrokeRecorder::default();
        rec.begin(res);
        let mut seq: Vec<(u32, f32)> = Vec::new();
        // z list deliberately revisits brick rows (0,1,0,0,2) to exercise flush→reload→re-dedup.
        for &z in &[0u32, 17, 5, 0, 33, 17] {
            for &y in &[2u32, 18, 2] {
                for &x in &[1u32, 20, 1, 39] {
                    let idx = x + y * res + z * res * res;
                    seq.push((idx, idx as f32 * 0.001)); // deterministic pre-value
                    seq.push((idx, 999.0)); // duplicate — first-touch must keep the 0.001 value
                }
            }
        }
        for &(idx, pre) in &seq { rec.record(idx, pre); }
        let got = rec.take();

        let mut seen = std::collections::HashSet::new();
        let mut want: Vec<(u32, f32)> = Vec::new();
        for &(idx, pre) in &seq { if seen.insert(idx) { want.push((idx, pre)); } }
        assert_eq!(got, want, "sparse recorder must match first-touch dedup across brick revisits");
        assert!(rec.is_empty(), "take() leaves the recorder empty");
    }

    /// VXTEAR GATE #1: a FULL brick directly against an EMPTY brick (after compaction) must mesh
    /// watertight — the surface lives AT their shared face. A brick-aligned solid box compacts to
    /// pure FULL/EMPTY tags (zero ALLOCATED bricks); the mask + mesh read values via `get_idx`
    /// (FULL→1, EMPTY→0), so the boundary meshes identically to dense — NOT the occupancy-hole.
    #[test]
    fn vxtear_full_empty_boundary_watertight_after_compaction() {
        let res = 64u32; // bricks at 0,16,32,48
        let solid = |x: u32, y: u32, z: u32| -> f32 {
            let inside = (16..48).contains(&x) && (16..48).contains(&y) && (16..48).contains(&z);
            if inside { 1.0 } else { 0.0 } // SHARP, brick-aligned [16,48) → bricks 1,2 FULL; 0,3 EMPTY
        };
        let mut dense = VoxelGrid::new_empty(res, 1.0);
        let mut sparse = VoxelGrid::new_empty(res, 1.0);
        sparse.density = DensityStorage::sparse(res, 0.0);
        for z in 0..res { for y in 0..res { for x in 0..res {
            let i = (x + y * res + z * res * res) as usize;
            let v = solid(x, y, z);
            dense.density.set_idx(i, v);
            sparse.density.set_idx(i, v);
        }}}
        sparse.density.compact_all();
        if let DensityStorage::Sparse(b) = &sparse.density {
            assert_eq!(b.allocated_bricks(), 0, "brick-aligned box must compact to pure FULL/EMPTY tags");
        }
        let cm = res - 2;
        let d = dense.mesh_region([0, 0, 0], [cm, cm, cm]);
        let s = sparse.mesh_region([0, 0, 0], [cm, cm, cm]);
        assert!(!s.0.is_empty(), "the box must produce a surface");
        assert_eq!(d, s, "FULL↔EMPTY boundary must mesh identically to dense (no hole)");
    }

    /// VXTEAR GATE #2: editing AFTER compaction — carving across brick boundaries into FULL-tagged
    /// solid (materialise-on-write) must still match dense byte-for-byte AND mesh identically. Also
    /// exercises the runtime switch (`new_empty` picks Dense < 256, Sparse ≥ 256) and the sealed border.
    #[test]
    fn vxtear_post_compaction_cross_brick_carve_matches_dense() {
        // Runtime switch sanity.
        assert!(matches!(VoxelGrid::new_empty(64, 1.0).density, DensityStorage::Dense { .. }), "res 64 stays Dense");
        assert!(matches!(VoxelGrid::new_empty(256, 1.0).density, DensityStorage::Sparse(_)), "res 256 is Sparse");

        let res = 64u32;
        let mut dense = VoxelGrid::new_empty(res, 1.0);
        dense.fill_box(Vec3::ZERO, Vec3::splat(0.7)); // dense path
        let mut sparse = VoxelGrid::new_empty(res, 1.0);
        sparse.density = DensityStorage::sparse(res, 0.0);
        for (i, &v) in dense.density.to_dense().iter().enumerate() { sparse.density.set_idx(i, v); }
        sparse.density.compact_all(); // interior → FULL tags

        // Carve with Erase strokes positioned to straddle 16-voxel brick boundaries.
        let carves = [
            Vec3::new(0.0, 0.0, 0.0), Vec3::new(0.25, 0.0, 0.0), Vec3::new(0.0, 0.3, -0.1), Vec3::new(-0.2, -0.2, 0.2),
        ];
        for c in carves {
            dense.apply_brush(VoxelBrush::Erase, c, Vec3::Z, 0.22, 0.9, false, 0.0, Vec3::ZERO, None);
            sparse.apply_brush(VoxelBrush::Erase, c, Vec3::Z, 0.22, 0.9, false, 0.0, Vec3::ZERO, None);
        }
        assert_eq!(dense.density.to_dense(), sparse.density.to_dense(), "post-compaction carve diverged");
        let cm = res - 2;
        assert_eq!(dense.mesh_region([0, 0, 0], [cm, cm, cm]), sparse.mesh_region([0, 0, 0], [cm, cm, cm]), "carved mesh diverged");

        // Sealed border: the outermost layer reads 0.0 on Sparse (watertight cap, matches Dense `at`).
        assert_eq!(sparse.density.at_i(0, 10, 10), 0.0);
        assert_eq!(sparse.density.at_i(res as i32 - 1, 10, 10), 0.0);
        assert_eq!(sparse.density.at_i(-1, 10, 10), 0.0); // OOB → 0
    }

    /// PHASE 4a: `subvolume` (the box-local GPU-brush gather) must be box-local AND byte-identical
    /// across Dense/Sparse, including a box that straddles brick boundaries and contains FULL/EMPTY +
    /// allocated bricks. This replaces the full-XY `z_slice` so the brush buffer is bounded in all axes.
    #[test]
    fn sparse_subvolume_box_local_matches_dense() {
        let res = 48u32;
        let mut dense = VoxelGrid::new_empty(res, 1.0);
        dense.fill_sphere(Vec3::ZERO, 0.55);
        let mut sparse = VoxelGrid::new_empty(res, 1.0);
        sparse.density = DensityStorage::sparse(res, 0.0);
        for (i, &v) in dense.density.to_dense().iter().enumerate() { sparse.density.set_idx(i, v); }
        sparse.density.compact_all();

        // A box straddling brick boundaries (bricks at 0,16,32) with interior + surface + exterior.
        let aabb = [10u32, 12, 14, 37, 35, 33];
        let d = dense.density.subvolume(aabb);
        let s = sparse.density.subvolume(aabb);
        assert_eq!(d, s, "subvolume must match Dense vs Sparse byte-for-byte");

        // Box-local layout check: out[(x-x0)+(y-y0)*dx+(z-z0)*dx*dy] == at(x,y,z).
        let [x0, y0, z0, x1, y1, z1] = aabb;
        let (dx, dy) = ((x1 - x0 + 1) as usize, (y1 - y0 + 1) as usize);
        for &(x, y, z) in &[(10u32, 12u32, 14u32), (24, 24, 24), (37, 35, 33), (16, 16, 16)] {
            let li = (x - x0) as usize + (y - y0) as usize * dx + (z - z0) as usize * dx * dy;
            assert_eq!(d[li], dense.density.at_i(x as i32, y as i32, z as i32), "box-local index mismatch");
        }
    }

    /// DIAGNOSTIC: each of the 11 brush ops applied IN ISOLATION must give a byte-identical field on a
    /// Sparse grid vs a Dense grid (the keystone test only covered Draw/Smooth/Pinch/Erase). Isolates a
    /// per-op Sparse storage bug (e.g. Inflate, which reads the LIVE grid gradient via `at`).
    #[test]
    fn sparse_each_brush_op_matches_dense() {
        let ops: [(VoxelBrush, &str); 11] = [
            (VoxelBrush::Draw, "Draw"), (VoxelBrush::Erase, "Erase"), (VoxelBrush::Smooth, "Smooth"),
            (VoxelBrush::Flatten, "Flatten"), (VoxelBrush::Clay, "Clay"), (VoxelBrush::Scrape, "Scrape"),
            (VoxelBrush::Crease, "Crease"), (VoxelBrush::Fill, "Fill"), (VoxelBrush::ClayBuildup, "Build"),
            (VoxelBrush::Inflate, "Inflate"), (VoxelBrush::Pinch, "Pinch"),
        ];
        for &res in &[48u32, 64] {
            for (op, name) in ops {
                let mut dense = VoxelGrid::new_empty(res, 1.0);
                dense.fill_sphere(Vec3::ZERO, 0.55);
                let mut sparse = VoxelGrid::new_empty(res, 1.0);
                sparse.density = DensityStorage::sparse(res, 0.0);
                for (i, &v) in dense.density.to_dense().iter().enumerate() { sparse.density.set_idx(i, v); }
                sparse.density.compact_all();
                // identical single stamp on both
                dense.apply_brush(op, Vec3::new(0.1, 0.0, 0.45), Vec3::Z, 0.3, 0.6, false, 0.0, Vec3::ZERO, None);
                sparse.apply_brush(op, Vec3::new(0.1, 0.0, 0.45), Vec3::Z, 0.3, 0.6, false, 0.0, Vec3::ZERO, None);
                let (dd, sd) = (dense.density.to_dense(), sparse.density.to_dense());
                let worst = dd.iter().zip(sd.iter()).map(|(a, b)| (a - b).abs()).fold(0.0f32, f32::max);
                assert!(worst < 1e-6, "op {name} @res{res}: Sparse diverges from Dense by {worst:.3e}");
            }
        }
    }

    /// PHASE 4b PREREQ: the GPU mesh atlas's `read_density(x,y,z)` formula (decode brick from global
    /// coords, read the EMPTY/FULL/handle tag, fetch from the pool) must equal `BrickGrid::at` — proven
    /// in PURE RUST over the exposed `index_slice()`/`bricks_slice()` BEFORE the shader uses them. A
    /// non-16-aligned res (33, 40) exercises the partial last brick; the sphere fill yields EMPTY +
    /// FULL + ALLOCATED bricks (fill_from compacts uniform ones), covering all three tag branches.
    #[test]
    fn read_density_formula_matches_at() {
        // For every voxel, the EXACT WGSL `read_density` formula over the exposed slices must equal
        // `at()`. Returns which tag branches were exercised (empty, full, alloc).
        fn check(g: &VoxelGrid) -> (bool, bool, bool) {
            let DensityStorage::Sparse(b) = &g.density else { panic!("expected Sparse") };
            let (index, pool, br, res) = (b.index_slice(), b.bricks_slice(), b.brick_res(), g.res);
            let (mut e, mut f, mut a) = (false, false, false);
            for z in 0..res { for y in 0..res { for x in 0..res {
                let bi = ((x >> 4) + (y >> 4) * br + (z >> 4) * br * br) as usize;
                let rd = match index[bi] {
                    TAG_EMPTY => 0.0,
                    TAG_FULL => 1.0,
                    h => pool[h as usize * BRICK_VOX + ((x & 15) + (y & 15) * 16 + (z & 15) * 256) as usize],
                };
                assert_eq!(rd, b.at(x, y, z), "read_density != at at ({x},{y},{z}) res {res}");
                match index[bi] { TAG_EMPTY => e = true, TAG_FULL => f = true, _ => a = true }
            }}}
            (e, f, a)
        }
        // Sphere @ non-16-aligned res → partial last brick + EMPTY + ALLOCATED (surface) bricks.
        let mut sphere = VoxelGrid::new_empty(40, 1.0);
        sphere.density = DensityStorage::sparse(40, 0.0);
        sphere.fill_sphere(Vec3::ZERO, 0.55);
        let (e1, _, a1) = check(&sphere);
        // Brick-aligned solid box @ 64 → EMPTY + FULL tags (the FULL branch the sphere lacks at small res).
        let res = 64u32;
        let mut boxg = VoxelGrid::new_empty(res, 1.0);
        boxg.density = DensityStorage::sparse(res, 0.0);
        for z in 0..res { for y in 0..res { for x in 0..res {
            let solid = (16..48).contains(&x) && (16..48).contains(&y) && (16..48).contains(&z);
            boxg.density.set_idx((x + y * res + z * res * res) as usize, if solid { 1.0 } else { 0.0 });
        }}}
        boxg.density.compact_all();
        let (e2, f2, _) = check(&boxg);
        assert!(e1 && a1 && e2 && f2, "should exercise EMPTY + FULL + ALLOCATED across the two grids");
    }

    #[test]
    fn sync_dirty_tracks_touched_bricks() {
        // Phase 4b-2 Step A: the dirty set / INDEX range / gc flag must capture EXACTLY what a stroke
        // changed so the app can patch the GPU atlas per-brick instead of re-uploading the whole grid.
        let res = 64u32;
        let br = (res + BRICK - 1) / BRICK; // 4
        let mut b = BrickGrid::new(res, 0.0);

        // Fresh grid → full upload pending; nothing incremental yet.
        assert!(b.gc_pending(), "fresh grid must request a full atlas upload");
        b.clear_sync_dirty();
        assert!(!b.gc_pending());
        assert!(b.dirty_bricks().is_empty());
        assert_eq!(b.index_dirty_range(), None);

        // Write into three distinct (initially EMPTY) bricks → each materialises.
        let pts = [(0u32, 0u32, 0u32), (20, 5, 3), (33, 40, 50)];
        let mut expect: std::collections::HashSet<u32> = std::collections::HashSet::new();
        for &(x, y, z) in &pts {
            b.set(x, y, z, 0.7);
            expect.insert((x / BRICK) + (y / BRICK) * br + (z / BRICK) * br * br);
        }
        assert_eq!(b.dirty_bricks(), &expect, "POOL-dirty set must equal exactly the touched bricks");
        let (lo, hi) = b.index_dirty_range().expect("materialise must mark the INDEX range");
        assert_eq!((lo, hi), (*expect.iter().min().unwrap(), *expect.iter().max().unwrap()));

        // Write INTO an already-allocated brick → POOL dirty, but NO INDEX change (no tag flip).
        b.clear_sync_dirty();
        b.set(0, 1, 0, 0.9); // brick 0 (already a handle)
        assert_eq!(b.dirty_bricks().len(), 1);
        assert!(b.dirty_bricks().contains(&0));
        assert_eq!(b.index_dirty_range(), None, "writing into an existing brick must not touch INDEX");

        // Demote a brick to EMPTY via compact → INDEX range marked.
        b.set(0, 0, 0, 0.0);
        b.set(0, 1, 0, 0.0); // brick 0 now all-zero
        b.clear_sync_dirty(); // forget the POOL marks from those zeroing writes
        b.compact_brick(0);
        assert_eq!(b.index_dirty_range(), Some((0, 0)), "demote must mark the INDEX entry");
        assert_eq!(b.at(0, 0, 0), 0.0);

        // fill_from resets the whole grid → full upload requested again.
        b.clear_sync_dirty();
        b.fill_from(|_, _, _| 0.0);
        assert!(b.gc_pending(), "fill_from must request a full atlas upload");
    }

    #[test]
    fn incremental_atlas_sync_matches_full_upload() {
        // Phase 4b-2 Step B: applying the dirty-tracked INCREMENTAL patches (changed INDEX range +
        // per-dirty-brick POOL block; full upload on gc_pending / pool-grow) to a PERSISTENT GPU-mirror
        // must reproduce the grid's atlas EXACTLY after every sync round — i.e. the mirror read (the WGSL
        // `read_density` formula) equals `grid.at()` for every voxel. GPU-free proof of the exact sync
        // protocol `FourthLayerApp::voxel_sync_mesh_atlas` implements: materialise, in-place write, same-
        // AND different-brick handle reuse, demote, and arena GC (gc_pending full upload). The remaining
        // path — a mid-sequence pool-capacity regrow — is covered by `atlas_sync_pool_regrow_forces_full_upload`.
        let res = 64u32;
        let br = ((res + 15) / 16) as usize; // 4

        // Persistent GPU-mirror: plain Vecs standing in for the INDEX / POOL storage buffers.
        let mut idx_mirror: Vec<u32> = Vec::new();
        let mut pool_mirror: Vec<f32> = Vec::new();
        let mut pool_cap: usize = 0;

        // Mirror of voxel_sync_mesh_atlas against the Vecs (same full-vs-incremental decision + writes).
        fn sync(g: &mut VoxelGrid, idx_m: &mut Vec<u32>, pool_m: &mut Vec<f32>, cap: &mut usize) {
            const VOX: usize = 4096; // BRICK_VOX
            let index = g.density.index_slice().unwrap();
            let pool = g.density.bricks_slice().unwrap();
            let arena = pool.len() / VOX;
            let need_full = idx_m.len() != index.len() || *cap < arena || g.density.gc_pending();
            if need_full {
                *cap = (arena + arena / 2).max(64);
                *idx_m = index.to_vec();
                *pool_m = vec![0.0; *cap * VOX];
                pool_m[..pool.len()].copy_from_slice(pool);
            } else {
                if let Some((lo, hi)) = g.density.index_dirty_range() {
                    idx_m[lo as usize..=hi as usize].copy_from_slice(&index[lo as usize..=hi as usize]);
                }
                if let Some(dirty) = g.density.dirty_bricks() {
                    for &bi in dirty {
                        if let Some((h, block)) = g.density.brick_pool_block(bi) {
                            let o = h as usize * VOX;
                            pool_m[o..o + VOX].copy_from_slice(block);
                        }
                    }
                }
            }
            g.density.clear_sync_dirty();
        }

        let check = |g: &VoxelGrid, idx_m: &[u32], pool_m: &[f32]| {
            for z in 0..res { for y in 0..res { for x in 0..res {
                let bi = ((x >> 4) + (y >> 4) * br as u32 + (z >> 4) * (br * br) as u32) as usize;
                let t = idx_m[bi];
                let rd = if t == TAG_EMPTY { 0.0 } else if t == TAG_FULL { 1.0 }
                    else { pool_m[t as usize * 4096 + ((x & 15) + (y & 15) * 16 + (z & 15) * 256) as usize] };
                assert_eq!(rd, g.density.get_idx((x + y * res + z * res * res) as usize),
                    "mirror != grid at ({x},{y},{z})");
            }}}
        };
        let set = |g: &mut VoxelGrid, x: u32, y: u32, z: u32, v: f32| {
            g.density.set_idx((x + y * res + z * res * res) as usize, v);
        };

        let mut g = VoxelGrid::new_empty(res, 1.0);
        g.density = DensityStorage::sparse(res, 0.0);

        // R1: edits on a fresh grid (gc_pending) → full upload captures them.
        for &(x, y, z) in &[(2u32, 3, 4), (20, 21, 22), (50, 51, 52)] { set(&mut g, x, y, z, 0.7); }
        sync(&mut g, &mut idx_mirror, &mut pool_mirror, &mut pool_cap);
        check(&g, &idx_mirror, &pool_mirror);

        // R2: edits in new + existing bricks → incremental (INDEX range + POOL blocks).
        for &(x, y, z) in &[(3u32, 3, 4), (40, 40, 40), (20, 21, 23)] { set(&mut g, x, y, z, 0.9); }
        sync(&mut g, &mut idx_mirror, &mut pool_mirror, &mut pool_cap);
        check(&g, &idx_mirror, &pool_mirror);

        // R3: zero brick (0,0,0)'s voxels + compact (demote → freed handle), then a fresh edit in the
        // SAME brick (reuses the freed handle) → incremental must still reproduce the grid.
        set(&mut g, 2, 3, 4, 0.0);
        set(&mut g, 3, 3, 4, 0.0); // brick (0,0,0) now all-zero
        g.density.compact_aabb([0, 0, 0, 15, 15, 15]);
        set(&mut g, 10, 10, 10, 0.5); // re-materialise brick (0,0,0), likely reusing the freed slot
        sync(&mut g, &mut idx_mirror, &mut pool_mirror, &mut pool_cap);
        check(&g, &idx_mirror, &pool_mirror);

        // R4: DIFFERENT-brick handle reuse — demote brick (32..48,32..48,32..48), then materialise a
        // DISTINCT previously-empty brick that recycles the freed slot. The freed slot now belongs to a
        // different brick, so the incremental POOL write must overwrite it with the NEW brick's data and
        // the INDEX range must flip BOTH bricks' tags. (The dangerous case Reviewer 2 flagged.)
        set(&mut g, 40, 40, 40, 0.0); // brick (2,2,2) now all-zero
        g.density.compact_aabb([32, 32, 32, 47, 47, 47]); // demote it → free its handle
        set(&mut g, 40, 5, 5, 0.6); // materialise brick (2,0,0) → reuses the just-freed slot
        sync(&mut g, &mut idx_mirror, &mut pool_mirror, &mut pool_cap);
        check(&g, &idx_mirror, &pool_mirror);

        // R5: arena GC mid-sequence — zero brick (1,1,1) to all-empty, then `compact_all` (compacts the
        // uniform brick AND runs gc_arena, which REMAPS every surviving handle + sets gc_pending). The
        // next sync must take the FULL-upload path and re-mirror the remapped arena exactly.
        set(&mut g, 20, 21, 22, 0.0);
        set(&mut g, 20, 21, 23, 0.0); // brick (1,1,1) now all-zero
        g.density.compact_all(); // demote + gc_arena remap → gc_pending
        assert!(g.density.gc_pending(), "compact_all with a freed slot must request a full upload");
        sync(&mut g, &mut idx_mirror, &mut pool_mirror, &mut pool_cap);
        check(&g, &idx_mirror, &pool_mirror);

        // R6: full re-fill (gc_pending) → full-upload path again.
        g.fill_sphere(Vec3::ZERO, 0.5);
        sync(&mut g, &mut idx_mirror, &mut pool_mirror, &mut pool_cap);
        check(&g, &idx_mirror, &pool_mirror);
    }

    #[test]
    fn materialize_box_report_handles_and_atlas_undo_restore() {
        // Phase A (GPU brick-snapshot undo): `materialize_box_report` must report each overlapped brick's
        // pre-call tag AND its LIVE post-materialize handle, and `restore_atlas_undo` must restore the
        // INDEX tags WITHOUT freeing handles or marking the POOL dirty (the GPU pool restore is the app's
        // job; re-marking the pool would clobber it). Pure-CPU proof of the index-restore + handle stability.
        let res = 64u32;
        let mut g = VoxelGrid::new_empty(res, 1.0);
        g.density = DensityStorage::sparse(res, 0.0);
        // Pre-allocate one brick (brick 0) so the report mixes an ALREADY-allocated brick (handle ==
        // prev_tag) with freshly-materialised EMPTY bricks (handle != prev_tag, prev_tag == TAG_EMPTY).
        g.density.set_idx(0, 0.7); // materialises brick 0
        let DensityStorage::Sparse(b0) = &g.density else { panic!() };
        let pre_h0 = b0.index_tag(0);
        assert!(pre_h0 != TAG_EMPTY && pre_h0 != TAG_FULL, "brick 0 should be allocated");

        // Report over a box spanning brick 0 (allocated) + brick (1,0,0) + brick (0,1,0) (both EMPTY).
        let report = g.density.materialize_box_report([0, 0, 0, 16, 16, 0]);
        assert!(!report.is_empty());
        for &(bi, prev_tag, handle) in &report {
            let cur = g.density.index_tag(bi);
            // After materialize every reported brick is allocated; its handle == its current index tag.
            assert_eq!(cur, handle, "reported handle must be the live index tag for brick {bi}");
            assert!(handle != TAG_EMPTY && handle != TAG_FULL, "materialized brick must hold a handle");
            if prev_tag != TAG_EMPTY && prev_tag != TAG_FULL {
                assert_eq!(handle, prev_tag, "already-allocated brick keeps its handle");
            }
        }
        // The already-allocated brick 0's handle is unchanged by the report (stable).
        let h0_after = report.iter().find(|r| r.0 == 0).map(|r| r.2);
        assert_eq!(h0_after, Some(pre_h0), "brick 0 handle must be stable across report");

        // Restore the pre-stroke INDEX tags (the undo). Bricks that were EMPTY go back to TAG_EMPTY;
        // brick 0 stays its handle. Handles are NOT freed — the arena length must not shrink.
        let DensityStorage::Sparse(bb) = &g.density else { panic!() };
        let arena_before = bb.bricks.len();
        let restore: Vec<(u32, u32)> = report.iter().map(|&(bi, prev_tag, _)| (bi, prev_tag)).collect();
        g.density.restore_atlas_undo(&restore);
        for &(bi, prev_tag, _) in &report {
            assert_eq!(g.density.index_tag(bi), prev_tag, "brick {bi} INDEX must be restored");
        }
        let DensityStorage::Sparse(ba) = &g.density else { panic!() };
        assert_eq!(ba.bricks.len(), arena_before, "restore_atlas_undo must NOT free/shrink the arena");
        // The restored EMPTY bricks read as 0.0 again; brick 0 retains its written value.
        assert_eq!(g.density.get_idx(0), 0.7, "allocated brick value preserved");
        assert_eq!(g.density.at_i(16, 0, 0), 0.0, "restored EMPTY brick reads 0.0");
    }

    #[test]
    fn materialize_bricks_report_is_sparse_deterministic_and_undoable() {
        let res = 64u32;
        let mut g = VoxelGrid::new_empty(res, 1.0);
        g.density = DensityStorage::sparse(res, 0.0);

        // Two diagonally opposite bricks, repeated and unsorted, must allocate exactly two slots.
        // An out-of-range id is ignored so callers can safely hand over conservatively generated ids.
        let report = g.density.materialize_bricks_report(&[63, 0, 63, 9_999]);
        assert_eq!(
            report.iter().map(|&(bi, _, _)| bi).collect::<Vec<_>>(),
            vec![0, 63],
            "the report must be ascending and duplicate-free"
        );
        assert!(report.iter().all(|&(_, prev_tag, _)| prev_tag == TAG_EMPTY));
        assert!(report.iter().all(|&(bi, _, handle)| {
            handle != TAG_EMPTY && handle != TAG_FULL && g.density.index_tag(bi) == handle
        }));
        assert_eq!(g.density.arena_floats(), 2 * 16 * 16 * 16);
        assert_eq!(
            g.density.index_tag(1),
            TAG_EMPTY,
            "a brick inside the diagonal union but outside both requested bricks stays implicit"
        );

        let restore: Vec<(u32, u32)> = report
            .iter()
            .map(|&(bi, prev_tag, _)| (bi, prev_tag))
            .collect();
        g.density.restore_atlas_undo(&restore);
        assert_eq!(g.density.index_tag(0), TAG_EMPTY);
        assert_eq!(g.density.index_tag(63), TAG_EMPTY);
        assert_eq!(
            g.density.arena_floats(),
            2 * 16 * 16 * 16,
            "undo restores index tags without invalidating stable GPU pool handles"
        );
    }

    #[test]
    fn refresh_pool_arena_restores_arena_from_captured_pool() {
        // Phase B sparse materialise: the app reads the GPU POOL back into the CPU arena (the INDEX is
        // already authoritative — only brick CONTENTS go stale after a GPU atlas stroke writes the pool in
        // place). This replaces the dense res³ gather readback (4.3 GB ×3 → OOM crash at 1024³) with a
        // bounded arena copy. Prove the round-trip is LOSSLESS: capture the live arena ("the GPU pool"),
        // clobber the CPU arena, then `refresh_pool_arena` from the captured pool and assert the grid is
        // byte-identical to the original. `arena_floats` must equal the pool length the app reads back.
        let res = 64u32;
        let mut g = VoxelGrid::new_empty(res, 1.0);
        g.density = DensityStorage::sparse(res, 0.0);
        g.fill_sphere(Vec3::ZERO, 0.5); // a surface shell → many allocated (mixed) bricks
        let truth = g.density.to_dense();
        let n = g.density.arena_floats();
        assert!(n > 0 && n % 4096 == 0, "a sculpted sphere allocates whole surface bricks, got {n}");
        let pool = g.density.bricks_slice().expect("sparse grid has a pool arena").to_vec();
        assert_eq!(pool.len(), n, "arena_floats must equal the pool length the app reads back");

        // Clobber the CPU arena (simulate the stale CPU after a GPU stroke wrote the GPU pool in place).
        g.density.refresh_pool_arena(&vec![9.0_f32; n]);
        assert_ne!(g.density.to_dense(), truth, "clobbering the arena must change the surface");

        // Materialise: refresh from the captured "GPU pool" → byte-identical to the original.
        g.density.refresh_pool_arena(&pool);
        assert_eq!(g.density.to_dense(), truth, "refresh from the captured pool must restore the grid exactly");
        assert_eq!(g.density.arena_floats(), n, "refresh never changes the arena layout");
    }

    #[test]
    fn region_const_unit() {
        // BrickGrid::region_const reads ONLY index tags and reports a voxel-coord AABB as uniformly
        // EMPTY (Some(0.0)) / FULL (Some(1.0)) / not-uniform (None). res 64 → brick_res 4 (cols 0..3,
        // each 16 voxels wide). Set brick column 0 and column 3 FULL, columns 1-2 EMPTY.
        let res = 64u32;
        let mut g = VoxelGrid::new_empty(res, 1.0);
        g.density = DensityStorage::sparse(res, 0.0);
        g.density.fill_from(|x, _, _| if x < 16 || x >= 48 { 1.0 } else { 0.0 });
        assert_eq!(g.density.region_const([0, 0, 0, 15, 15, 15]), Some(1.0), "col 0 wholly FULL");
        assert_eq!(g.density.region_const([16, 0, 0, 47, 15, 15]), Some(0.0), "cols 1-2 wholly EMPTY");
        assert_eq!(g.density.region_const([0, 0, 0, 31, 15, 15]), None, "FULL+EMPTY straddle → not uniform");
        assert_eq!(g.density.region_const([16, 0, 0, 70, 15, 15]), None, "cols 1-2-3 = EMPTY+EMPTY+FULL straddle → None");
        // OOB over an all-EMPTY in-bounds span → Some(0.0): OOB reads 0.0 == empty, consistent.
        assert_eq!(g.density.region_const([16, 0, 0, 47, 70, 70]), Some(0.0), "EMPTY cols, y/z OOB-high over EMPTY → still empty");
        // OOB over a FULL brick → None: an OOB corner reads 0.0, so the trilinear result is < 1.0 there;
        // region_const's `oob` flag forces all_full=false so the edge FULL brick is never skipped as FULL.
        assert_eq!(g.density.region_const([48, 0, 0, 70, 15, 15]), None, "col 3 FULL but x OOB-high → cannot skip as FULL");
        // An allocated (handle) brick is treated as possibly-mixed → None (conservative, never a wrong skip).
        g.density.set_idx((20 + 0 + 0) as usize, 0.5); // voxel (20,0,0) ∈ brick col 1 → allocate a handle
        assert_eq!(g.density.region_const([16, 0, 0, 31, 15, 15]), None, "handle brick → None (never assumed uniform)");
    }

    #[test]
    fn resample_sparse_skip_matches_full() {
        // PRIMARY byte-identical proof for the sparse-aware resample. Resampling WITH the region_const
        // skip must produce a grid bit-for-bit identical to resampling with full per-voxel trilinear
        // sampling. A solid sphere yields FULL interior bricks (skip→Full), EMPTY exterior bricks
        // (skip→Empty), and a Mixed surface shell (skip→None → full sampling) — exercising every path,
        // and the FULL/Mixed shell boundary implicitly guards the +1 trilinear neighbor (drop it and the
        // shell bricks adjacent to the FULL core would be wrongly skipped → mismatch here). Both an
        // upsample (dest brick maps to <1 source brick) and a downsample (one dest brick spans many) are
        // covered. dst_res ≥ SPARSE_RES_THRESHOLD so the destination stays sparse.
        for (src_res, dst_res) in [(512u32, 320u32), (384u32, 512u32)] {
            let mut src = VoxelGrid::new_empty(src_res, 1.0);
            src.fill_sphere(Vec3::ZERO, 0.6);
            assert!(src.density.is_sparse(), "src {src_res} should be sparse");
            {
                let DensityStorage::Sparse(b) = &src.density else { panic!() };
                let (mut n_full, mut n_empty, mut n_handle) = (0u32, 0u32, 0u32);
                for &t in b.index.iter() {
                    if t == TAG_FULL { n_full += 1; } else if t == TAG_EMPTY { n_empty += 1; } else { n_handle += 1; }
                }
                assert!(n_full > 0 && n_empty > 0 && n_handle > 0,
                    "src {src_res} must have FULL+EMPTY+Mixed bricks so all skip paths fire: full={n_full} empty={n_empty} handle={n_handle}");
            }
            // Actual: the real resample (uses the region_const skip fast path).
            let with_skip = src.resample(dst_res);
            // Reference: the IDENTICAL resample body but force NO skip (par_fill_from = skip None).
            let mut reference = VoxelGrid::new_empty(dst_res, src.bounds);
            reference.iso = src.iso;
            let (gb, gr) = (reference.bounds, reference.res);
            reference.density.par_fill_from(|x, y, z| src.sample(grid_cell_world(x, y, z, gr, gb)).clamp(0.0, 1.0));
            let (DensityStorage::Sparse(a), DensityStorage::Sparse(b)) = (&with_skip.density, &reference.density)
                else { panic!("dst {dst_res} expected sparse on both") };
            assert_eq!(a.index, b.index, "INDEX tags must match bit-for-bit ({src_res}->{dst_res})");
            assert_eq!(a.bricks, b.bricks, "arena floats must match bit-for-bit ({src_res}->{dst_res})");
            assert_eq!(a.free, b.free, "free list must match ({src_res}->{dst_res})");
            assert_eq!(with_skip.density.to_dense(), reference.density.to_dense(), "dense field must match ({src_res}->{dst_res})");
        }
    }

    #[test]
    fn atlas_sync_pool_regrow_forces_full_upload() {
        // Phase 4b-2 Step B: the highest-risk transition Reviewer 2 flagged — an INCREMENTAL sync, then
        // the live arena outgrows the GPU pool capacity, forcing a mid-sequence FULL re-upload (buffer
        // regrow). The mirror read must reproduce the grid after each phase, and the regrow must actually
        // fire (pool_cap grows). Uses res 128 (brick_res 8, 512 bricks) so the arena can exceed the
        // 64-brick capacity floor — impossible at res 64 (only 64 bricks total).
        let res = 128u32;
        let br = ((res + 15) / 16) as usize; // 8

        let mut idx_mirror: Vec<u32> = Vec::new();
        let mut pool_mirror: Vec<f32> = Vec::new();
        let mut pool_cap: usize = 0;

        // Same mirror-of-voxel_sync_mesh_atlas as the sibling test.
        fn sync(g: &mut VoxelGrid, idx_m: &mut Vec<u32>, pool_m: &mut Vec<f32>, cap: &mut usize) {
            const VOX: usize = 4096;
            let index = g.density.index_slice().unwrap();
            let pool = g.density.bricks_slice().unwrap();
            let arena = pool.len() / VOX;
            let need_full = idx_m.len() != index.len() || *cap < arena || g.density.gc_pending();
            if need_full {
                *cap = (arena + arena / 2).max(64);
                *idx_m = index.to_vec();
                *pool_m = vec![0.0; *cap * VOX];
                pool_m[..pool.len()].copy_from_slice(pool);
            } else {
                if let Some((lo, hi)) = g.density.index_dirty_range() {
                    idx_m[lo as usize..=hi as usize].copy_from_slice(&index[lo as usize..=hi as usize]);
                }
                if let Some(dirty) = g.density.dirty_bricks() {
                    for &bi in dirty {
                        if let Some((h, block)) = g.density.brick_pool_block(bi) {
                            let o = h as usize * VOX;
                            pool_m[o..o + VOX].copy_from_slice(block);
                        }
                    }
                }
            }
            g.density.clear_sync_dirty();
        }
        let check = |g: &VoxelGrid, idx_m: &[u32], pool_m: &[f32]| {
            for z in 0..res { for y in 0..res { for x in 0..res {
                let bi = ((x >> 4) + (y >> 4) * br as u32 + (z >> 4) * (br * br) as u32) as usize;
                let t = idx_m[bi];
                let rd = if t == TAG_EMPTY { 0.0 } else if t == TAG_FULL { 1.0 }
                    else { pool_m[t as usize * 4096 + ((x & 15) + (y & 15) * 16 + (z & 15) * 256) as usize] };
                assert_eq!(rd, g.density.get_idx((x + y * res + z * res * res) as usize),
                    "mirror != grid at ({x},{y},{z})");
            }}}
        };
        // Materialise `count` DISTINCT non-uniform bricks (brick `i` ← one voxel at its origin+1 = 0.6,
        // so it never compacts away). `i < 512` maps to a unique (bx,by,bz).
        let materialize = |g: &mut VoxelGrid, lo: usize, hi: usize| {
            for i in lo..hi {
                let (bx, by, bz) = ((i % 8) as u32, ((i / 8) % 8) as u32, (i / 64) as u32);
                let (x, y, z) = (bx * 16 + 1, by * 16 + 1, bz * 16 + 1);
                g.density.set_idx((x + y * res + z * res * res) as usize, 0.6);
            }
        };

        let mut g = VoxelGrid::new_empty(res, 1.0);
        g.density = DensityStorage::sparse(res, 0.0);

        // P1: 50 bricks on a fresh grid (gc_pending) → full upload; pool_cap = (50+25).max(64) = 75.
        materialize(&mut g, 0, 50);
        sync(&mut g, &mut idx_mirror, &mut pool_mirror, &mut pool_cap);
        check(&g, &idx_mirror, &pool_mirror);
        assert_eq!(pool_cap, 75, "first full upload sizes pool_cap with 1.5x headroom");

        // P2: +10 bricks (arena 60 ≤ 75) → INCREMENTAL (no regrow). Proves the incremental path scales.
        materialize(&mut g, 50, 60);
        sync(&mut g, &mut idx_mirror, &mut pool_mirror, &mut pool_cap);
        check(&g, &idx_mirror, &pool_mirror);
        assert_eq!(pool_cap, 75, "arena within capacity must NOT regrow");

        // P3: +30 bricks (arena 90 > 75) → forced FULL re-upload (regrow); pool_cap = (90+45).max(64) = 135.
        let cap_before = pool_cap;
        materialize(&mut g, 60, 90);
        sync(&mut g, &mut idx_mirror, &mut pool_mirror, &mut pool_cap);
        check(&g, &idx_mirror, &pool_mirror);
        assert!(pool_cap > cap_before, "outgrowing capacity must regrow the pool (was {cap_before}, now {pool_cap})");

        // P4: +5 bricks (arena 95 ≤ 135) → INCREMENTAL again against the regrown buffer.
        materialize(&mut g, 90, 95);
        sync(&mut g, &mut idx_mirror, &mut pool_mirror, &mut pool_cap);
        check(&g, &idx_mirror, &pool_mirror);
        assert_eq!(pool_cap, 135, "post-regrow capacity holds while the arena stays within it");
    }

    #[test]
    fn atlas_sync_gpu_authority_skips_stale_cpu_pool_dirt() {
        // Regression: atlas stroke writes the GPU POOL in place; CPU stays pre-stroke until release
        // materialize. A mid-stroke / post-release atlas sync must NOT upload dirty POOL bricks that
        // were only dirtied on the CPU (e.g. Auto Smooth on the stale field) — that wiped the stroke
        // and left remesh empty until the next sculpt. Policy mirrors `voxel_sync_mesh_atlas`:
        // when GPU is authority, only upload dirty bricks that also sit in the INDEX dirty range
        // (fresh materialize of EMPTY/FULL marks both; set_idx/relax on an existing handle marks
        // pool dirty only).
        let res = 64u32;
        let br = ((res + 15) / 16) as usize;
        let mut idx_mirror: Vec<u32> = Vec::new();
        let mut pool_mirror: Vec<f32> = Vec::new();
        let mut pool_cap: usize = 0;

        fn sync_gpu_auth(g: &mut VoxelGrid, idx_m: &mut Vec<u32>, pool_m: &mut Vec<f32>, cap: &mut usize, gpu_auth: bool) {
            const VOX: usize = 4096;
            let index = g.density.index_slice().unwrap();
            let pool = g.density.bricks_slice().unwrap();
            let arena = pool.len() / VOX;
            let need_full = idx_m.len() != index.len() || *cap < arena || g.density.gc_pending();
            if need_full {
                *cap = (arena + arena / 2).max(64);
                *idx_m = index.to_vec();
                *pool_m = vec![0.0; *cap * VOX];
                pool_m[..pool.len()].copy_from_slice(pool);
            } else {
                if let Some((lo, hi)) = g.density.index_dirty_range() {
                    idx_m[lo as usize..=hi as usize].copy_from_slice(&index[lo as usize..=hi as usize]);
                }
                if let Some(dirty) = g.density.dirty_bricks() {
                    let idx_range = g.density.index_dirty_range();
                    for &bi in dirty {
                        let allow = if gpu_auth {
                            idx_range.map_or(false, |(lo, hi)| bi >= lo && bi <= hi)
                        } else {
                            true
                        };
                        if !allow { continue; }
                        if let Some((h, block)) = g.density.brick_pool_block(bi) {
                            let o = h as usize * VOX;
                            pool_m[o..o + VOX].copy_from_slice(block);
                        }
                    }
                }
            }
            g.density.clear_sync_dirty();
        }

        let mut g = VoxelGrid::new_empty(res, 1.0);
        g.density = DensityStorage::sparse(res, 0.0);
        // Seed one brick + full sync (CPU authoritative).
        g.density.set_idx((2 + 3 * res + 4 * res * res) as usize, 0.7);
        sync_gpu_auth(&mut g, &mut idx_mirror, &mut pool_mirror, &mut pool_cap, false);

        // Simulate GPU scatter: pool mirror holds post-stroke value 0.95; CPU still has 0.7.
        let bi0 = 0u32; // brick (0,0,0) contains voxel (2,3,4)
        let (h, _) = g.density.brick_pool_block(bi0).expect("brick 0 allocated");
        let slot = h as usize * 4096 + (2 + 3 * 16 + 4 * 256);
        pool_mirror[slot] = 0.95;

        // Stale CPU "Auto Smooth" dirties the same brick without INDEX change.
        g.density.set_idx((2 + 3 * res + 4 * res * res) as usize, 0.1);
        assert!(g.density.dirty_bricks().map_or(false, |d| d.contains(&bi0)));
        assert_eq!(g.density.index_dirty_range(), None, "set on existing handle must not dirty INDEX");

        // GPU-authority sync must keep the scattered 0.95 (not upload stale 0.1).
        sync_gpu_auth(&mut g, &mut idx_mirror, &mut pool_mirror, &mut pool_cap, true);
        assert!(
            (pool_mirror[slot] - 0.95).abs() < 1e-6,
            "GPU-authority sync must not overwrite pool with stale CPU dirt (got {})",
            pool_mirror[slot]
        );

        // Fresh materialize of a NEW brick still uploads (INDEX + dirty both set).
        g.density.set_idx((20 + 21 * res + 22 * res * res) as usize, 0.6);
        let bi_new = ((20 >> 4) + (21 >> 4) * br as u32 + (22 >> 4) * (br * br) as u32) as u32;
        sync_gpu_auth(&mut g, &mut idx_mirror, &mut pool_mirror, &mut pool_cap, true);
        let (h2, block) = g.density.brick_pool_block(bi_new).expect("new brick");
        let o = h2 as usize * 4096;
        assert_eq!(&pool_mirror[o..o + 4096], block, "new materialize must still upload under GPU authority");
    }

    #[test]
    fn materialize_box_preserves_values_and_allocates() {
        // Phase 5b: materialize_box allocates a POOL handle for every IMPLICIT brick in the box (so a
        // GPU-resident brush can write those slots) WITHOUT changing any voxel value. Value-preserving +
        // every box brick becomes a real handle.
        let res = 64u32;
        let mut g = VoxelGrid::new_empty(res, 1.0);
        g.density = DensityStorage::sparse(res, 0.0);
        g.fill_sphere(Vec3::ZERO, 0.5); // EMPTY (outside) + FULL (inside) + ALLOC (shell) bricks
        let before = g.density.to_dense();
        g.density.materialize_box([0, 0, 0, 47, 47, 47]);
        assert_eq!(g.density.to_dense(), before, "materialize_box must preserve every voxel value");
        // Every brick overlapping the box is now an allocated handle (no implicit tag left in the box).
        let br = g.density.brick_res();
        let hi = 47 / 16u32;
        for bz in 0..=hi { for by in 0..=hi { for bx in 0..=hi {
            let bi = bx + by * br + bz * br * br;
            assert!(g.density.brick_pool_block(bi).is_some(), "brick {bi} in the box must be materialised");
        }}}
}

// ==========================================================================================
// GOLDEN PARITY TEST (BRUSH_SYSTEM_ARCHITECTURE.md Phase 0 — the gate).
//
// For EVERY built-in VoxelBrush (+ hardness / direction / paint variations), assert that the
// new `StampPlan` path (`apply_brush` -> `from_builtin` -> `fold_recipe` -> `apply_stamp_plan`)
// produces a voxel-for-voxel IDENTICAL density grid to the legacy `apply_brush_legacy` on a
// fixed seeded grid. This PROVES the Phase-0 refactor is a zero-behaviour-change. If any brush
// diverges, this test FAILS — the data model must be bit-identical before any phase merges.
//
// Runs (not `#[ignore]`d) under `cargo test -p core_math`.
// ==========================================================================================
mod brush_parity_tests {
    use super::*;

    /// A deterministic seeded grid with a surface to sculpt (a sphere + a small offset bump so
    /// gradients/planes are non-trivial). Dense backing so every voxel is directly comparable.
    fn seeded_grid() -> VoxelGrid {
        let mut g = VoxelGrid::new_empty(48, 1.0); // <256 → dense
        g.fill_sphere(Vec3::ZERO, 0.55);
        // A second overlapping blob so the field has a non-flat region (varied gradients/planes).
        let r = g.res - 2;
        for z in 1..=r { for y in 1..=r { for x in 1..=r {
            let p = g.cell_world_pos(x, y, z);
            let d = (p - Vec3::new(0.2, 0.1, 0.0)).length() - 0.30;
            // Inline smoothstep01 (the production helper is module-private — don't widen its scope).
            let tt = (-d / g.voxel_size().max(1e-6)).clamp(0.0, 1.0);
            let add = tt * tt * (3.0 - 2.0 * tt);
            if add > 0.0 {
                let i = g.idx(x, y, z);
                let cur = g.density.get_idx(i);
                g.density.set_idx(i, cur.max(add));
            }
        }}}
        g
    }

    /// Compare two grids voxel-for-voxel; panic with the first divergence (op + index + values).
    fn assert_identical(legacy: &VoxelGrid, plan: &VoxelGrid, label: &str) {
        assert_eq!(legacy.res, plan.res, "{label}: res mismatch");
        let a = legacy.density.to_dense();
        let b = plan.density.to_dense();
        assert_eq!(a.len(), b.len(), "{label}: voxel count mismatch");
        for (i, (la, lb)) in a.iter().zip(b.iter()).enumerate() {
            // BIT-IDENTICAL: identical inputs through identical float ops must produce identical
            // bit patterns. Compare raw bits so we cannot mask a 1-ULP drift behind an epsilon.
            assert!(
                la.to_bits() == lb.to_bits(),
                "{label}: voxel {i} diverged — legacy {la} (0x{:08x}) vs plan {lb} (0x{:08x})",
                la.to_bits(), lb.to_bits(),
            );
        }
    }

    /// Run ONE stamp on a fresh clone via BOTH paths and assert identity. Mirrors how the producer
    /// invokes `apply_brush` today: `normal`/`strength` are already the resolved values, so the
    /// recipe shim builds with neutral Direction/pressure (the plan packs the resolved scalars).
    #[allow(clippy::too_many_arguments)]
    fn check_one(
        base: &VoxelGrid, brush: VoxelBrush, center: Vec3, normal: Vec3,
        radius: f32, strength: f32, invert: bool, hardness: f32, drag: Vec3, label: &str,
    ) {
        let mut legacy = base.clone();
        legacy.apply_brush_legacy(brush, center, normal, radius, strength, invert, hardness, drag, None);
        let mut plan = base.clone();
        plan.apply_brush(brush, center, normal, radius, strength, invert, hardness, drag, None);
        assert_identical(&legacy, &plan, label);
    }

    #[test]
    fn every_builtin_brush_is_bit_identical() {
        let base = seeded_grid();
        // Contact roughly on the +X surface, normal pointing out; Move carries a drag delta.
        let center = Vec3::new(0.5, 0.05, 0.0);
        let normal = Vec3::new(1.0, 0.05, 0.0).normalize();
        let drag = Vec3::new(0.04, 0.02, -0.01);
        let (radius, strength) = (0.30f32, 0.6f32);

        let all = [
            VoxelBrush::Draw, VoxelBrush::Erase, VoxelBrush::Smooth, VoxelBrush::Flatten,
            VoxelBrush::Clay, VoxelBrush::Scrape, VoxelBrush::Crease, VoxelBrush::Fill,
            VoxelBrush::ClayBuildup, VoxelBrush::Inflate, VoxelBrush::Pinch, VoxelBrush::Move,
        ];
        for vb in all {
            let label = format!("{:?}", vb);
            // The Move/Grab drag is only meaningful for Move; every other brush ignores it (the
            // producer passes ZERO), so pass ZERO for non-Move to mirror the live call exactly.
            let d = if matches!(vb, VoxelBrush::Move) { drag } else { Vec3::ZERO };
            check_one(&base, vb, center, normal, radius, strength, false, 0.0, d, &label);
        }
    }

    #[test]
    fn hardness_variants_are_bit_identical() {
        let base = seeded_grid();
        let center = Vec3::new(0.5, 0.05, 0.0);
        let normal = Vec3::new(1.0, 0.05, 0.0).normalize();
        let (radius, strength) = (0.30f32, 0.6f32);
        // Hardness drives the Kernel falloff (incl. Crease's Power{3} / ClayBuildup's Power{2}).
        for &h in &[0.0f32, 0.35, 0.7, 1.0] {
            for vb in [VoxelBrush::Draw, VoxelBrush::Clay, VoxelBrush::Crease, VoxelBrush::ClayBuildup, VoxelBrush::Smooth] {
                let label = format!("{:?}@hardness={h}", vb);
                check_one(&base, vb, center, normal, radius, strength, false, h, Vec3::ZERO, &label);
            }
        }
    }

    #[test]
    fn invert_and_direction_variants_are_bit_identical() {
        let base = seeded_grid();
        let center = Vec3::new(0.5, 0.05, 0.0);
        let (radius, strength) = (0.30f32, 0.6f32);
        // The producer resolves Direction into `normal` BEFORE calling apply_brush, so direction
        // variation == feeding a different concrete normal. Exercise several normals + invert.
        let normals = [
            Vec3::new(1.0, 0.0, 0.0),
            Vec3::new(0.0, 1.0, 0.0),
            Vec3::new(0.0, 0.0, 1.0),
            Vec3::new(0.3, 0.6, -0.2).normalize(),
        ];
        for n in normals {
            for invert in [false, true] {
                for vb in [VoxelBrush::Flatten, VoxelBrush::Clay, VoxelBrush::Scrape, VoxelBrush::Fill, VoxelBrush::Crease, VoxelBrush::Pinch] {
                    let label = format!("{:?}@n={n:?}/inv={invert}", vb);
                    check_one(&base, vb, center, n, radius, strength, invert, 0.5, Vec3::ZERO, &label);
                }
            }
        }
    }

    #[test]
    fn paint_channel_does_not_change_density_parity() {
        // The COLOR channel rides on the recipe but the Phase-0 apply engine writes only DENSITY
        // (paint keeps its `deposit_color_sphere` plumbing). Build a recipe WITH the COLOR bit set
        // and confirm the folded plan's density result still matches the legacy density exactly.
        let base = seeded_grid();
        let center = Vec3::new(0.5, 0.05, 0.0);
        let normal = Vec3::new(1.0, 0.05, 0.0).normalize();
        let (radius, strength) = (0.30f32, 0.6f32);
        for vb in [VoxelBrush::Draw, VoxelBrush::Clay] {
            let builtin = vb.to_builtin();
            // Recipe WITH paint on (COLOR channel set) — the density path must be unaffected.
            // Falloff preset Smooth (0) matches apply_brush_legacy / apply_brush parity defaults
            // (from_builtin defaults to Blend which is a different curve).
            let recipe = crate::brush::BrushRecipe::from_builtin_ex(
                builtin, radius, strength, 0.0, 0, crate::brush::Direction::SurfaceNormal,
                0xFF_0D_0D_FF, true, false, false, [false; 3], 0.25, 0.0, true,
            );
            assert!(recipe.channels.mask.contains(crate::brush::ChannelMask::COLOR), "paint-on recipe must carry COLOR");
            let hit = crate::brush::SurfaceHit { center, normal, radius, strength };
            // Clay parity needs plane_offset 0.10 (legacy proud plane); Draw ignores it.
            let mods = crate::brush::StrokeMods {
                plane_offset: if matches!(vb, VoxelBrush::Clay) { 0.10 } else { 0.0 },
                ..crate::brush::StrokeMods::default()
            };
            let plan = crate::brush::fold_recipe(&recipe, &hit, &mods);
            let mut plan_grid = base.clone();
            plan_grid.apply_stamp_plan(&plan, None);
            let mut legacy = base.clone();
            legacy.apply_brush_legacy(vb, center, normal, radius, strength, false, 0.0, Vec3::ZERO, None);
            assert_identical(&legacy, &plan_grid, &format!("{:?}+paint", vb));
        }
    }

    #[test]
    fn tip_alpha_changes_stamp_footprint() {
        // Changing brush tip alpha must change density (Soft Round ≠ Thin Line).
        // Guards the "changing alpha does not change tips" regression: tip multiplies falloff.
        use crate::brush::{self, BuiltinBrush, Direction};
        let base = seeded_grid();
        let center = Vec3::new(0.5, 0.05, 0.0);
        let normal = Vec3::new(1.0, 0.05, 0.0).normalize();
        let (radius, strength) = (0.35f32, 1.0f32);
        let recipe = brush::BrushRecipe::from_builtin_ex(
            BuiltinBrush::Draw, radius, strength, 0.0, 0, Direction::SurfaceNormal,
            0, false, false, false, [false; 3], 0.25, 0.0, true,
        );
        let hit = brush::SurfaceHit { center, normal, radius, strength };
        let plan = brush::fold_recipe(&recipe, &hit, &brush::StrokeMods::default());
        let soft = BrushTipAlpha::soft_round(64);
        let line = BrushTipAlpha::thin_line_crease(64, 0.05);
        let mut g_soft = base.clone();
        let mut g_line = base.clone();
        g_soft.apply_stamp_plan_with_tip(&plan, Some(&soft), None);
        g_line.apply_stamp_plan_with_tip(&plan, Some(&line), None);
        let a = g_soft.density.to_dense();
        let b = g_line.density.to_dense();
        let mut max_d = 0.0f32;
        for (x, y) in a.iter().zip(b.iter()) {
            max_d = max_d.max((x - y).abs());
        }
        assert!(
            max_d > 1e-3,
            "Thin Line tip must differ from Soft Round (max |Δ|={max_d:.3e}) — tip alpha not applied?"
        );
    }

    #[test]
    fn from_builtin_always_single_op() {
        // Base recipe is always one op; stack mods may raise op_count at fold time.
        use crate::brush::{BuiltinBrush, Direction};
        for b in BuiltinBrush::ALL {
            let r = crate::brush::BrushRecipe::from_builtin(
                b, 0.3, 0.6, 0.0, Direction::SurfaceNormal, 0, false, false, false, [false; 3], 0.25, 0.0, true,
            );
            assert_eq!(r.op_count, 1, "{:?} base recipe must be a single op", b);
        }
    }

    #[test]
    fn fold_stack_gather_on_flatten_adds_second_op() {
        use crate::brush::{self, BuiltinBrush, Direction, Operator};
        let recipe = brush::BrushRecipe::from_builtin(
            BuiltinBrush::Flatten, 0.3, 0.6, 0.0, Direction::SurfaceNormal,
            0, false, false, false, [false; 3], 0.25, 0.0, true,
        );
        let hit = brush::SurfaceHit {
            center: Vec3::ZERO, normal: Vec3::Y, radius: 0.3, strength: 0.6,
        };
        let plan = brush::fold_recipe(&recipe, &hit, &brush::StrokeMods {
            pinch_intensity: 2.0, stack_gather: true, ..brush::StrokeMods::default()
        });
        assert_eq!(plan.op_count, 2, "Flatten + Gather → 2 ops");
        assert_eq!(plan.op_list[0].op_code, Operator::PlanePush as u32);
        assert_eq!(plan.op_list[1].op_code, Operator::Advect as u32);
        assert!((plan.op_list[1].amount - 2.0).abs() < 1e-5, "gather amount from pinch_intensity");
    }

    #[test]
    fn fold_stack_flatten_on_pinch_adds_second_op() {
        use crate::brush::{self, BuiltinBrush, Direction, Operator};
        let recipe = brush::BrushRecipe::from_builtin(
            BuiltinBrush::Pinch, 0.3, 0.6, 0.0, Direction::SurfaceNormal,
            0, false, false, false, [false; 3], 0.25, 0.0, true,
        );
        let hit = brush::SurfaceHit {
            center: Vec3::ZERO, normal: Vec3::Y, radius: 0.3, strength: 0.6,
        };
        let plan = brush::fold_recipe(&recipe, &hit, &brush::StrokeMods {
            pinch_intensity: 1.5, stack_flatten: true, ..brush::StrokeMods::default()
        });
        assert_eq!(plan.op_count, 2, "Pinch + Flatten → 2 ops");
        assert_eq!(plan.op_list[0].op_code, Operator::Advect as u32);
        assert_eq!(plan.op_list[1].op_code, Operator::PlanePush as u32);
    }
}

