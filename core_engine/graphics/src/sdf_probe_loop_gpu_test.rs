// GPU equivalence gate for the 2026-10-13 DF-full cold-compile fix: `calc_curvature` (11 taps)
// and the SDF-reflection normal (6 taps) were rewritten from N separate `scene_sdf` calls into
// ONE call site inside a loop, because the driver inlines every call and DF-full never finished
// a cold compile. The rewrite claims the SAME taps in the SAME order with the SAME summation.
// This test runs the PRODUCTION functions (sliced verbatim from the shipped WGSL sources)
// next to the ORIGINAL unrolled forms (kept here as the oracle) over a field with real
// curvature, several tap steps and thousands of points, and requires agreement.

use crate::rt_shadow_gpu_test::{try_device, upload};

/// Brace-matched slice of `fn <name>(` … `}` from a WGSL source (verbatim production text).
fn wgsl_fn(source: &str, name: &str) -> String {
    let start = source.find(&format!("fn {name}(")).unwrap_or_else(|| panic!("fn {name} not found"));
    let open = start + source[start..].find('{').expect("fn body");
    let mut depth = 0i32;
    for (offset, c) in source[open..].char_indices() {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return source[start..=open + offset].to_string();
                }
            }
            _ => {}
        }
    }
    panic!("unbalanced fn {name}");
}

/// The ORIGINAL unrolled forms (pre-2026-10-13), verbatim math — the oracle.
const ORACLE_WGSL: &str = r#"
fn calc_curvature_old(p: vec3<f32>, e: f32) -> f32 {
    let d = scene_sdf(p);
    let ge = clamp(e, 1e-4, 0.01);
    let gx = scene_sdf(p + vec3(ge,0.0,0.0)) - scene_sdf(p - vec3(ge,0.0,0.0));
    let gy = scene_sdf(p + vec3(0.0,ge,0.0)) - scene_sdf(p - vec3(0.0,ge,0.0));
    let gz = scene_sdf(p + vec3(0.0,0.0,ge)) - scene_sdf(p - vec3(0.0,0.0,ge));
    let gl = length(vec3(gx, gy, gz));
    let n = select(vec3(0.0, 0.0, 1.0), vec3(gx, gy, gz) / max(gl, 1e-6), gl > 1e-6);
    let tb = calc_curvature_tangent_basis(n);
    let t1 = tb[0];
    let t2 = tb[1];
    let lap = scene_sdf(p + e * t1) + scene_sdf(p - e * t1)
            + scene_sdf(p + e * t2) + scene_sdf(p - e * t2)
            - 4.0 * d;
    return lap / (e * e);
}
fn gradient_old(p: vec3<f32>, ne: f32) -> vec3<f32> {
    return vec3(
        scene_sdf(p + vec3(ne, 0.0, 0.0)) - scene_sdf(p - vec3(ne, 0.0, 0.0)),
        scene_sdf(p + vec3(0.0, ne, 0.0)) - scene_sdf(p - vec3(0.0, ne, 0.0)),
        scene_sdf(p + vec3(0.0, 0.0, ne)) - scene_sdf(p - vec3(0.0, 0.0, ne)));
}
// Test field with real convex, concave and flat regions: box minus sphere, union a sphere.
fn scene_sdf(p: vec3<f32>) -> f32 {
    let q = abs(p - vec3(0.1, -0.05, 0.0)) - vec3(0.45, 0.35, 0.3);
    let bx = length(max(q, vec3(0.0))) + min(max(q.x, max(q.y, q.z)), 0.0);
    let carve = length(p - vec3(0.35, 0.2, 0.15)) - 0.28;
    let ball = length(p + vec3(0.45, 0.1, -0.2)) - 0.25;
    return min(max(bx, -carve), ball);
}
@group(0) @binding(0) var<storage, read> points: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> out: array<vec4<f32>>;
@compute @workgroup_size(64)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = gid.x;
    if (i >= arrayLength(&points)) { return; }
    let p = points[i].xyz;
    let e = points[i].w;
    let g_new = sdf_central_gradient(p, 0.001);
    let g_old = gradient_old(p, 0.001);
    out[i * 2u] = vec4(calc_curvature(p, e), calc_curvature_old(p, e), length(g_new - g_old), 0.0);
    out[i * 2u + 1u] = vec4(g_new, 0.0);
}
"#;

#[test]
fn sdf_probe_loops_match_unrolled_originals() {
    let Some((device, queue)) = try_device() else {
        eprintln!("[SDF-PROBE-LOOP-GPU] no adapter — skipping");
        return;
    };
    let prim = include_str!("sdf_prim_eval.wgsl");
    let scene = include_str!("sdf_scene.wgsl");
    let source = [
        wgsl_fn(prim, "calc_curvature_tangent_basis"),
        wgsl_fn(prim, "calc_curvature"),
        wgsl_fn(scene, "sdf_central_diff_offset"),
        wgsl_fn(scene, "sdf_central_gradient"),
        ORACLE_WGSL.to_string(),
    ]
    .join("\n");

    // Deterministic points in [-0.9, 0.9]^3 (surface, interior, exterior), several tap steps:
    // the production epsilons (0.005/0.01/0.015) and the owner's large Cavity radius (0.19).
    let steps = [0.005f32, 0.01, 0.015, 0.19];
    let mut state = 0x9E37_79B9u32;
    let mut rand = || {
        state ^= state << 13;
        state ^= state >> 17;
        state ^= state << 5;
        (state as f32 / u32::MAX as f32) * 1.8 - 0.9
    };
    let points: Vec<[f32; 4]> = (0..4096).map(|i| [rand(), rand(), rand(), steps[i % steps.len()]]).collect();

    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("sdf_probe_loop_test"),
        source: wgpu::ShaderSource::Wgsl(source.into()),
    });
    let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some("sdf_probe_loop_test"),
        layout: None,
        module: &module,
        entry_point: Some("main"),
        compilation_options: Default::default(),
        cache: None,
    });
    let points_buf = upload(&device, &queue, &points, "probe points");
    let out_size = (points.len() * 2 * 16) as u64;
    let out_buf = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("probe out"), size: out_size,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC, mapped_at_creation: false,
    });
    let staging = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("probe staging"), size: out_size,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false,
    });
    let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: None,
        layout: &pipeline.get_bind_group_layout(0),
        entries: &[
            wgpu::BindGroupEntry { binding: 0, resource: points_buf.as_entire_binding() },
            wgpu::BindGroupEntry { binding: 1, resource: out_buf.as_entire_binding() },
        ],
    });
    let mut encoder = device.create_command_encoder(&Default::default());
    {
        let mut pass = encoder.begin_compute_pass(&Default::default());
        pass.set_pipeline(&pipeline);
        pass.set_bind_group(0, &bind_group, &[]);
        pass.dispatch_workgroups((points.len() as u32).div_ceil(64), 1, 1);
    }
    encoder.copy_buffer_to_buffer(&out_buf, 0, &staging, 0, out_size);
    queue.submit([encoder.finish()]);
    let (sender, receiver) = std::sync::mpsc::channel();
    staging.slice(..).map_async(wgpu::MapMode::Read, move |result| { let _ = sender.send(result); });
    device.poll(wgpu::PollType::Wait).expect("GPU device polling failed");
    receiver.recv().expect("probe readback channel").expect("probe map failed");
    let data: Vec<[f32; 4]> = bytemuck::cast_slice(&staging.slice(..).get_mapped_range()).to_vec();

    let mut worst_curv = 0.0f32;
    let mut worst_grad = 0.0f32;
    let mut nonzero_curv = 0usize;
    for (i, point) in points.iter().enumerate() {
        let [new, old, grad_diff, _] = data[i * 2];
        assert!(new.is_finite() && old.is_finite(), "non-finite curvature at {point:?}: {new} vs {old}");
        // Curvature divides a tap sum by e²: allow FMA-contraction differences relative to the
        // magnitudes the taps themselves carry (|f| <= ~1 here → 4 ulp-scale terms / e²).
        let tolerance = 4.0 * f32::EPSILON * (1.0 + old.abs()) / (point[3] * point[3]).min(1.0);
        worst_curv = worst_curv.max((new - old).abs() / tolerance);
        worst_grad = worst_grad.max(grad_diff);
        if old.abs() > 1.0 { nonzero_curv += 1; }
    }
    eprintln!("[SDF-PROBE-LOOP-GPU] worst curvature diff = {worst_curv:.3} × tolerance, worst gradient diff = {worst_grad:e}, {nonzero_curv} points with |curv| > 1");
    assert!(nonzero_curv > 400, "test field must exercise real curvature ({nonzero_curv} curved points)");
    assert!(worst_curv <= 1.0, "loop calc_curvature diverges from the unrolled original ({worst_curv:.3} × tolerance)");
    assert!(worst_grad <= 4.0 * f32::EPSILON, "loop gradient diverges from the unrolled original ({worst_grad:e})");
}
