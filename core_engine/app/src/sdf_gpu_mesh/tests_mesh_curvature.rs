//! MESH_CURVATURE_PLAN M1 — per-vertex curvature from the MESH's own face normals.
//!
//! The gate that matters: on a CUBE, a face-interior vertex must read ~0 (its incident faces all
//! agree) while a corner vertex must read high (its faces point three different ways). The field
//! path this replaces cannot make that distinction reliably near seams, which is the defect the
//! owner reported.

use eframe::wgpu;
use wgpu::util::DeviceExt;

use super::tests_dc::headless_device;

/// Build an indexed unit cube: 8 shared corners, 12 triangles. Vertices are 8 f32 (pos, nrm, uv)
/// to match the DC layout the kernel reads. The stored normal is deliberately GARBAGE — the
/// kernel must derive geometry from POSITIONS only, and a wrong answer here would prove it is
/// reading the (field-derived) normal instead.
fn cube_mesh() -> (Vec<f32>, Vec<u32>) {
    let p = [
        [-1.0f32, -1.0, -1.0], [1.0, -1.0, -1.0], [1.0, 1.0, -1.0], [-1.0, 1.0, -1.0],
        [-1.0, -1.0, 1.0], [1.0, -1.0, 1.0], [1.0, 1.0, 1.0], [-1.0, 1.0, 1.0],
    ];
    let mut verts = Vec::with_capacity(8 * 8);
    for c in p {
        verts.extend_from_slice(&c);
        verts.extend_from_slice(&[9.0, 9.0, 9.0]); // poison normal — must never be read
        verts.extend_from_slice(&[0.0, 0.0]);
    }
    #[rustfmt::skip]
    let idx: Vec<u32> = vec![
        0,2,1, 0,3,2,   4,5,6, 4,6,7,   0,1,5, 0,5,4,
        3,7,6, 3,6,2,   0,4,7, 0,7,3,   1,2,6, 1,6,5,
    ];
    (verts, idx)
}

/// Vertex → incident-triangle table, densely filled from slot 0 with `0xFFFFFFFF` padding —
/// exactly the layout the kernel's scan loop expects (it stops at the first empty slot).
fn build_adjacency(idx: &[u32], vert_count: usize, stride: usize) -> Vec<u32> {
    let mut adj = vec![u32::MAX; vert_count * stride];
    let mut fill = vec![0usize; vert_count];
    for (ti, tri) in idx.chunks_exact(3).enumerate() {
        for &v in tri {
            let v = v as usize;
            if fill[v] < stride {
                adj[v * stride + fill[v]] = ti as u32;
                fill[v] += 1;
            }
        }
    }
    adj
}

/// M1 GATE — on a CUBE every vertex is a corner where three faces meet, so all 8 must read HIGH
/// curvature. The stored per-vertex normal is POISONED with (9,9,9) in the fixture: the kernel
/// must derive face normals from POSITIONS, and reading the stored one cannot pass.
#[test]
fn mesh_curvature_cube_corners_read_high_faces_read_flat() {
    let (verts, idx) = cube_mesh();
    let Some(data) = run_mesh_curvature(&verts, &idx, 8) else {
        eprintln!("[MeshCurv] no GPU adapter — skipping");
        return;
    };
    let vert_count = verts.len() / 8;
    let mags: Vec<f32> = (0..vert_count).map(|v| data[v * 4]).collect();
    eprintln!("[MeshCurv] cube corner magnitudes: {mags:?}");
    for (v, m) in mags.iter().enumerate() {
        assert!(
            *m > 0.1,
            "cube corner {v} must read HIGH curvature (three faces disagree) — got {m}. Near 0              means no adjacency, or the stored field-derived normal is being read."
        );
        assert!(m.is_finite(), "corner {v} produced a non-finite value ({m})");
    }
    for v in 0..vert_count {
        assert!(
            data[v * 4 + 1] > 0.0,
            "cube vertex {v} is CONVEX — signed curvature must be positive, got {}",
            data[v * 4 + 1]
        );
    }
}

/// NON-VACUITY companion: a COPLANAR quad must read ~0. Without it, the cube test alone would
/// pass on a kernel returning a constant — the failure mode that let the earlier "gift-wrap
/// cross" ship.
#[test]
fn mesh_curvature_flat_quad_reads_zero() {
    let mut verts: Vec<f32> = Vec::new();
    for c in [[-1.0f32, 0.0, -1.0], [1.0, 0.0, -1.0], [1.0, 0.0, 1.0], [-1.0, 0.0, 1.0]] {
        verts.extend_from_slice(&c);
        verts.extend_from_slice(&[9.0, 9.0, 9.0]);
        verts.extend_from_slice(&[0.0, 0.0]);
    }
    let idx: Vec<u32> = vec![0, 1, 2, 0, 2, 3];
    let Some(data) = run_mesh_curvature(&verts, &idx, 8) else {
        eprintln!("[MeshCurv] no GPU adapter — skipping");
        return;
    };
    let mags: Vec<f32> = (0..verts.len() / 8).map(|v| data[v * 4]).collect();
    eprintln!("[MeshCurv] FLAT quad magnitudes: {mags:?}");
    for (v, m) in mags.iter().enumerate() {
        assert!(
            *m < 0.01,
            "a COPLANAR quad vertex must read ~0 curvature — got {m} at vertex {v}"
        );
    }
}


/// Run the kernel over an arbitrary mesh and return the raw 4-wide output. Shared by the tests
/// below so each one is just geometry + assertions.
fn run_mesh_curvature(verts: &[f32], idx: &[u32], stride: usize) -> Option<Vec<f32>> {
    run_mesh_curvature_smoothed(verts, idx, stride, 0)
}

/// M3: run seed → N diffuse steps (ping-ponged) → measure. `smooth_iters == 0` measures against
/// the immediate one-ring, i.e. the M1/M2 behaviour.
fn run_mesh_curvature_smoothed(
    verts: &[f32], idx: &[u32], stride: usize, smooth_iters: u32,
) -> Option<Vec<f32>> {
    let (_gpu_lock, device, queue) = headless_device()?;
    let vert_count = verts.len() / 8;
    let adj = build_adjacency(idx, vert_count, stride);
    let mk = |label: &str, data: &[u8], usage: wgpu::BufferUsages| {
        device.create_buffer_init(&wgpu::util::BufferInitDescriptor { label: Some(label), contents: data, usage })
    };
    let st = wgpu::BufferUsages::STORAGE;
    let vbuf = mk("v", bytemuck::cast_slice(verts), st);
    let ibuf = mk("i", bytemuck::cast_slice(idx), st);
    let abuf = mk("a", bytemuck::cast_slice(&adj), st);
    #[repr(C)]
    #[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
    struct P { vert_count: u32, tri_count: u32, adj_stride: u32, smooth_iters: u32 }
    let params = mk("p", bytemuck::bytes_of(&P {
        vert_count: vert_count as u32, tri_count: (idx.len() / 3) as u32,
        adj_stride: stride as u32, smooth_iters,
    }), wgpu::BufferUsages::UNIFORM);
    let nrm_bytes = (vert_count * 4 * 4) as u64;
    // M4: the GPU-built adjacency table needs its own read_write buffer plus per-vertex fill
    // counters. `abuf` (the CPU-built table) stays bound at slot 3 so the two can be compared.
    let adj_rw = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("adj_rw"), size: ((vert_count * stride * 4).max(4)) as u64,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false });
    let adj_fill = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("adj_fill"), size: ((vert_count * 4).max(4)) as u64,
        usage: wgpu::BufferUsages::STORAGE, mapped_at_creation: false });
    let nrm_a = device.create_buffer(&wgpu::BufferDescriptor { label: Some("nrm_a"), size: nrm_bytes,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false });
    let nrm_b = device.create_buffer(&wgpu::BufferDescriptor { label: Some("nrm_b"), size: nrm_bytes,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false });
    let out_bytes = (vert_count * 4 * 4) as u64;
    let out = device.create_buffer(&wgpu::BufferDescriptor { label: None, size: out_bytes,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC, mapped_at_creation: false });
    let staging = device.create_buffer(&wgpu::BufferDescriptor { label: None, size: out_bytes,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor { label: None,
        source: wgpu::ShaderSource::Wgsl(graphics::build_mesh_curvature_wgsl().into()) });
    let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor { label: None,
        entries: &graphics::bgl::mesh_curvature_bgl_entries() });
    let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: None,
        bind_group_layouts: &[&bgl], push_constant_ranges: &[] });
    let mkpipe = |ep: &str| device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some(ep), layout: Some(&pl), module: &shader, entry_point: Some(ep),
        compilation_options: Default::default(), cache: None });
    let pipe_seed = mkpipe("mesh_curvature_seed");
    let pipe_diff = mkpipe("mesh_curvature_diffuse");
    let pipe_meas = mkpipe("mesh_curvature_measure");
    let bg = device.create_bind_group(&wgpu::BindGroupDescriptor { label: None, layout: &bgl, entries: &[
        wgpu::BindGroupEntry { binding: 0, resource: params.as_entire_binding() },
        wgpu::BindGroupEntry { binding: 1, resource: vbuf.as_entire_binding() },
        wgpu::BindGroupEntry { binding: 2, resource: ibuf.as_entire_binding() },
        wgpu::BindGroupEntry { binding: 3, resource: abuf.as_entire_binding() },
        wgpu::BindGroupEntry { binding: 4, resource: out.as_entire_binding() },
        wgpu::BindGroupEntry { binding: 5, resource: nrm_a.as_entire_binding() },
        wgpu::BindGroupEntry { binding: 6, resource: nrm_b.as_entire_binding() },
        wgpu::BindGroupEntry { binding: 7, resource: adj_rw.as_entire_binding() },
        wgpu::BindGroupEntry { binding: 8, resource: adj_fill.as_entire_binding() },
    ] });
    let groups = ((vert_count as u32) + 63) / 64;
    let mut enc = device.create_command_encoder(&Default::default());
    {
        let mut cp = enc.begin_compute_pass(&Default::default());
        cp.set_bind_group(0, &bg, &[]);
        cp.set_pipeline(&pipe_seed);
        cp.dispatch_workgroups(groups, 1, 1);
    }
    // Ping-pong: each diffuse step writes B, then B is copied back to A so the next step — and
    // finally the measure pass — reads the accumulated neighbourhood from A.
    for _ in 0..smooth_iters {
        {
            let mut cp = enc.begin_compute_pass(&Default::default());
            cp.set_bind_group(0, &bg, &[]);
            cp.set_pipeline(&pipe_diff);
            cp.dispatch_workgroups(groups, 1, 1);
        }
        enc.copy_buffer_to_buffer(&nrm_b, 0, &nrm_a, 0, nrm_bytes);
    }
    {
        let mut cp = enc.begin_compute_pass(&Default::default());
        cp.set_bind_group(0, &bg, &[]);
        cp.set_pipeline(&pipe_meas);
        cp.dispatch_workgroups(groups, 1, 1);
    }
    enc.copy_buffer_to_buffer(&out, 0, &staging, 0, out_bytes);
    queue.submit(Some(enc.finish()));
    let slice = staging.slice(..);
    slice.map_async(wgpu::MapMode::Read, |_| {});
    device.poll(wgpu::PollType::Wait).expect("GPU device polling failed");
    let data: Vec<f32> = bytemuck::cast_slice(&slice.get_mapped_range()).to_vec();
    staging.unmap();
    Some(data)
}

/// M2 GATE — AREA WEIGHTING. A vertex where one LARGE face meets a tiny SLIVER must be reported
/// as nearly flat, because ~all of its actual surface is the large face.
///
/// This is the test the cube CANNOT provide: a unit cube's triangles are all equal-area, so
/// weighted and unweighted give identical answers there (verified — the M1 numbers did not move
/// when weighting landed). Without this test, M2 would be unverified.
///
/// Unweighted, the sliver would carry 1/3 of the vote and report substantial curvature on what is
/// essentially a flat surface — noise exactly where DC produces slivers, i.e. at boolean seams,
/// which is where the owner reported the map looking wrong.
#[test]
fn mesh_curvature_area_weighting_sliver_does_not_outvote_large_face() {
    // Shared vertex 0 at the origin. Two large coplanar triangles in the y=0 plane, plus one
    // TINY triangle tilted steeply out of it. Area-weighted, the tilt is negligible.
    let mut verts: Vec<f32> = Vec::new();
    for c in [
        [0.0f32, 0.0, 0.0],      // 0: the vertex under test
        [4.0, 0.0, 0.0],         // 1  \ large, in-plane
        [4.0, 0.0, 4.0],         // 2  /
        [0.0, 0.0, 4.0],         // 3  large, in-plane
        [0.02, 0.02, -0.02],     // 4: sliver tip, steeply out of plane
        [0.03, 0.0, -0.01],      // 5: sliver base
    ] {
        verts.extend_from_slice(&c);
        verts.extend_from_slice(&[9.0, 9.0, 9.0]); // poisoned normal — must not be read
        verts.extend_from_slice(&[0.0, 0.0]);
    }
    let idx: Vec<u32> = vec![0, 1, 2, 0, 2, 3, 0, 4, 5];
    let Some(data) = run_mesh_curvature(&verts, &idx, 8) else {
        eprintln!("[MeshCurv] no GPU adapter — skipping");
        return;
    };
    let m = data[0];
    eprintln!("[MeshCurv] large+sliver vertex magnitude = {m}");
    assert!(
        m < 0.05,
        "a vertex whose surface is ~all one large flat face must read ~flat — got {m}. A high          value means the tiny sliver is outvoting the large face, i.e. area weighting is not          applied and the map will be noisy wherever DC emits slivers (boolean seams)."
    );
    assert!(m.is_finite(), "sliver vertex produced a non-finite value");
}

/// M3 GATE — RADIUS MUST WIDEN THE MEASUREMENT BASELINE.
///
/// This is the test for the owner's actual complaint: a gently curved surface reads as FLAT, so
/// only sharp edges ever appear and the map looks binary. The reference image (smooth midtone
/// gradients across whole shell panels) is wide-baseline curvature.
///
/// A shallow dome changes its normal by only ~2° per vertex step, so the local signal is
/// `1 - cos(2°) ≈ 0.0006` — invisible. Diffusing the NORMALS first means the comparison happens
/// against a neighbourhood several vertices wide, where the normal has turned much further.
///
/// Asserts the RELATIONSHIP, not a magic number: more iterations must produce a strictly larger
/// reading on the same geometry. A kernel that ignores `smooth_iters` returns identical values
/// and fails here — which is exactly what a "radius does nothing" bug looks like.
#[test]
fn mesh_curvature_radius_widens_baseline_on_a_gentle_dome() {
    // A shallow dome: a grid of vertices on a large sphere, so adjacent faces differ only
    // slightly. Deliberately gentle — a sharp shape would saturate and hide the effect.
    const N: usize = 12;          // grid resolution
    const R: f32 = 40.0;          // big radius => very gentle curvature
    const SPAN: f32 = 6.0;        // patch extent
    let mut verts: Vec<f32> = Vec::new();
    for j in 0..N {
        for i in 0..N {
            let x = (i as f32 / (N - 1) as f32 - 0.5) * SPAN;
            let z = (j as f32 / (N - 1) as f32 - 0.5) * SPAN;
            let y = R - (R * R - x * x - z * z).max(0.0).sqrt(); // dome height
            verts.extend_from_slice(&[x, y, z]);
            verts.extend_from_slice(&[9.0, 9.0, 9.0]); // poisoned normal
            verts.extend_from_slice(&[0.0, 0.0]);
        }
    }
    let mut idx: Vec<u32> = Vec::new();
    for j in 0..N - 1 {
        for i in 0..N - 1 {
            let a = (j * N + i) as u32;
            let b = a + 1;
            let c = a + N as u32;
            let d = c + 1;
            idx.extend_from_slice(&[a, c, b, b, c, d]);
        }
    }
    // Centre vertex of the patch — fully surrounded, so its neighbourhood is well defined.
    let centre = ((N / 2) * N + N / 2) as usize;

    let mut readings: Vec<(u32, f32)> = Vec::new();
    for iters in [0u32, 2, 6] {
        let Some(data) = run_mesh_curvature_smoothed(&verts, &idx, 12, iters) else {
            eprintln!("[MeshCurv] no GPU adapter — skipping");
            return;
        };
        readings.push((iters, data[centre * 4]));
    }
    eprintln!("[MeshCurv] gentle dome, centre vertex by smoothing iterations: {readings:?}");

    for (it, v) in &readings {
        assert!(v.is_finite(), "iters={it} produced a non-finite reading ({v})");
    }
    // The relationship IS the feature: widening the baseline must strengthen the signal.
    assert!(
        readings[1].1 > readings[0].1,
        "2 smoothing iterations must read HIGHER than 0 on a gentle dome — got {:.6} vs {:.6}.          Equal values mean `smooth_iters` is being ignored, i.e. Radius does nothing.",
        readings[1].1, readings[0].1
    );
    assert!(
        readings[2].1 > readings[1].1,
        "6 iterations must read higher than 2 — got {:.6} vs {:.6}",
        readings[2].1, readings[1].1
    );
    // Non-vacuity: the widest reading must be a usable MIDTONE, not numerical dust. That is the
    // whole point — a gentle dome should be visible in the map at all.
    assert!(
        readings[2].1 > 1e-4,
        "the widest baseline still reads {:.8} on a dome — too faint to appear in a map, so the          'gentle curvature is invisible' complaint would remain",
        readings[2].1
    );
}

/// M4 GATE — the GPU-BUILT adjacency table must match the CPU-built one.
///
/// Adjacency CANNOT be built on the CPU in production: the index COUNT lives only in the
/// GPU-written indirect args (`resident.rs` states "NO mesh readback" as a contract), so building
/// it CPU-side would mean a readback per mesh generation — reintroducing exactly the stall this
/// feature exists to avoid.
///
/// The M1-M3 gates all fed a CPU-built table, so they prove the ALGORITHM but say nothing about
/// the GPU builder. This runs the real clear+fill passes and compares against the CPU table on
/// the same mesh.
///
/// Order within a vertex list is deliberately NOT compared: the fill pass claims slots with an
/// atomic, so whichever triangle lands first wins. That is fine because every consumer SUMS over
/// the list. Both sides are sorted before comparing so the test asserts the SET, which is the
/// property that actually matters.
#[test]
fn mesh_curvature_gpu_adjacency_matches_cpu() {
    let Some((_gpu_lock, device, queue)) = headless_device() else {
        eprintln!("[MeshCurv] no GPU adapter - skipping");
        return;
    };
    let (verts, idx) = cube_mesh();
    const STRIDE: usize = 8;
    let vert_count = verts.len() / 8;
    let tri_count = idx.len() / 3;
    let cpu_adj = build_adjacency(&idx, vert_count, STRIDE);

    let mk = |label: &str, data: &[u8], usage: wgpu::BufferUsages| {
        device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some(label), contents: data, usage })
    };
    let st = wgpu::BufferUsages::STORAGE;
    let vbuf = mk("v", bytemuck::cast_slice(&verts), st);
    let ibuf = mk("i", bytemuck::cast_slice(&idx), st);
    let abuf = mk("a", bytemuck::cast_slice(&cpu_adj), st);
    #[repr(C)]
    #[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
    struct P { vert_count: u32, tri_count: u32, adj_stride: u32, smooth_iters: u32 }
    let params = mk("p", bytemuck::bytes_of(&P {
        vert_count: vert_count as u32, tri_count: tri_count as u32,
        adj_stride: STRIDE as u32, smooth_iters: 0,
    }), wgpu::BufferUsages::UNIFORM);

    let n4 = (vert_count * 4 * 4) as u64;
    let adj_bytes = (vert_count * STRIDE * 4) as u64;
    let mkbuf = |label: &str, size: u64, extra: wgpu::BufferUsages| device.create_buffer(
        &wgpu::BufferDescriptor { label: Some(label), size, usage: st | extra,
                                  mapped_at_creation: false });
    let out = mkbuf("out", n4, wgpu::BufferUsages::COPY_SRC);
    let nrm_a = mkbuf("na", n4, wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::COPY_DST);
    let nrm_b = mkbuf("nb", n4, wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::COPY_DST);
    let adj_rw = mkbuf("adj_rw", adj_bytes, wgpu::BufferUsages::COPY_SRC);
    let adj_fill = mkbuf("adj_fill", (vert_count * 4) as u64, wgpu::BufferUsages::empty());
    let staging = device.create_buffer(&wgpu::BufferDescriptor { label: Some("stage"),
        size: adj_bytes,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false });

    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor { label: None,
        source: wgpu::ShaderSource::Wgsl(graphics::build_mesh_curvature_wgsl().into()) });
    let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor { label: None,
        entries: &graphics::bgl::mesh_curvature_bgl_entries() });
    let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: None,
        bind_group_layouts: &[&bgl], push_constant_ranges: &[] });
    let mkpipe = |ep: &str| device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some(ep), layout: Some(&pl), module: &shader, entry_point: Some(ep),
        compilation_options: Default::default(), cache: None });
    let pipe_clear = mkpipe("mesh_curvature_adj_clear");
    let pipe_fill = mkpipe("mesh_curvature_adj_fill");
    let bg = device.create_bind_group(&wgpu::BindGroupDescriptor { label: None, layout: &bgl,
        entries: &[
            wgpu::BindGroupEntry { binding: 0, resource: params.as_entire_binding() },
            wgpu::BindGroupEntry { binding: 1, resource: vbuf.as_entire_binding() },
            wgpu::BindGroupEntry { binding: 2, resource: ibuf.as_entire_binding() },
            wgpu::BindGroupEntry { binding: 3, resource: abuf.as_entire_binding() },
            wgpu::BindGroupEntry { binding: 4, resource: out.as_entire_binding() },
            wgpu::BindGroupEntry { binding: 5, resource: nrm_a.as_entire_binding() },
            wgpu::BindGroupEntry { binding: 6, resource: nrm_b.as_entire_binding() },
            wgpu::BindGroupEntry { binding: 7, resource: adj_rw.as_entire_binding() },
            wgpu::BindGroupEntry { binding: 8, resource: adj_fill.as_entire_binding() },
        ] });

    let mut enc = device.create_command_encoder(&Default::default());
    {
        let mut cp = enc.begin_compute_pass(&Default::default());
        cp.set_bind_group(0, &bg, &[]);
        cp.set_pipeline(&pipe_clear);
        cp.dispatch_workgroups(((vert_count as u32) + 63) / 64, 1, 1);
    }
    {
        let mut cp = enc.begin_compute_pass(&Default::default());
        cp.set_bind_group(0, &bg, &[]);
        cp.set_pipeline(&pipe_fill);
        cp.dispatch_workgroups(((tri_count as u32) + 63) / 64, 1, 1);
    }
    enc.copy_buffer_to_buffer(&adj_rw, 0, &staging, 0, adj_bytes);
    queue.submit(Some(enc.finish()));
    let slice = staging.slice(..);
    slice.map_async(wgpu::MapMode::Read, |_| {});
    device.poll(wgpu::PollType::Wait).expect("GPU device polling failed");
    let gpu_adj: Vec<u32> = bytemuck::cast_slice(&slice.get_mapped_range()).to_vec();
    staging.unmap();

    for v in 0..vert_count {
        let mut cpu: Vec<u32> = cpu_adj[v * STRIDE..(v + 1) * STRIDE].iter().copied()
            .filter(|t| *t != u32::MAX).collect();
        let mut gpu: Vec<u32> = gpu_adj[v * STRIDE..(v + 1) * STRIDE].iter().copied()
            .filter(|t| *t != u32::MAX).collect();
        cpu.sort_unstable();
        gpu.sort_unstable();
        assert_eq!(gpu, cpu,
            "vertex {v}: GPU-built adjacency must hold the SAME incident triangles as the \
             CPU-built table (both sorted first - slot order is atomic-dependent by design)");
    }
    // Non-vacuity: two empty tables would compare equal and prove nothing.
    let total: usize = gpu_adj.iter().filter(|t| **t != u32::MAX).count();
    assert!(total >= tri_count * 3,
        "GPU adjacency recorded only {total} entries for {tri_count} triangles - each triangle \
         must register with all 3 of its vertices");
    eprintln!("[MeshCurv] GPU adjacency matches CPU across {vert_count} vertices ({total} entries)");
}
