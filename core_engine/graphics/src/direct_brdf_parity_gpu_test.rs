// Raster-vs-PT DIRECT-LIGHT BRDF parity (RASTER-DIRECT-UNNORMALIZED, 2026-10-13).
//
// The deferred pass's direct lighting must be the path tracer's BRDF exactly: for any
// (n, v, l, albedo, roughness, metal), `albedo·direct_diffuse + direct_spec` (deferred_entry.wgsl)
// must equal `pt_eval_f_cos` (pathtrace_tri_entry.wgsl). Both sides are the PRODUCTION functions
// sliced verbatim from the shipped WGSL and evaluated on the GPU. Before the fix the deferred
// lobe was `albedo·N·L` (π× hot) and `F·D·N·L` (no Smith G, no 1/(4·N·L·N·V)) — this test would
// report ratios of π and 2–9.

use crate::pt_furnace_gpu_test::wgsl_fn;
use crate::rt_shadow_gpu_test::try_device;

/// Each case: (n·l tilt, n·v tilt, azimuth between them, roughness, metal, albedo).
fn eval_both(cases: &[[f32; 8]]) -> Option<Vec<[f32; 2]>> {
    let (device, queue) = try_device()?;
    let pt = include_str!("pathtrace_tri_entry.wgsl");
    let de = include_str!("deferred_entry.wgsl");
    let mut source = String::from("const PT_PI: f32 = 3.14159265;\n");
    for name in ["pt_fresnel_schlick", "pt_ggx_D", "pt_smith_G2", "pt_eval_f_cos"] {
        source.push_str(&wgsl_fn(pt, name));
        source.push('\n');
    }
    for name in ["direct_fresnel", "direct_ggx_d", "direct_smith_g2", "direct_alpha", "direct_spec", "direct_diffuse"] {
        source.push_str(&wgsl_fn(de, name));
        source.push('\n');
    }
    source.push_str(r#"
@group(0) @binding(0) var<storage, read> cases: array<array<f32, 8>>;
@group(0) @binding(1) var<storage, read_write> outv: array<vec2<f32>>;
@compute @workgroup_size(64)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = gid.x;
    if (i >= arrayLength(&cases)) { return; }
    let c = cases[i];
    let n = vec3(0.0, 0.0, 1.0);
    let l = vec3(sin(c[0]), 0.0, cos(c[0]));
    let v = vec3(sin(c[1]) * cos(c[2]), sin(c[1]) * sin(c[2]), cos(c[1]));
    let rough = c[3];
    let metal = c[4];
    let albedo = vec3(c[5], c[6], c[7]);
    let pt = pt_eval_f_cos(n, v, l, albedo, rough, metal);
    let f0 = mix(vec3(0.04), albedo, metal);
    let raster = albedo * direct_diffuse(n, v, l, f0, metal) + direct_spec(n, v, l, f0, direct_alpha(rough));
    outv[i] = vec2(pt.g, raster.g);
}
"#);
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("direct_brdf_parity"), source: wgpu::ShaderSource::Wgsl(source.into()),
    });
    let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some("direct_brdf_parity"), layout: None, module: &module, entry_point: Some("main"),
        compilation_options: Default::default(), cache: None,
    });
    let cases_buf = crate::rt_shadow_gpu_test::upload(&device, &queue, cases, "brdf cases");
    let out_size = (cases.len() * 8) as u64;
    let out = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("brdf out"), size: out_size,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC, mapped_at_creation: false,
    });
    let staging = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("brdf staging"), size: out_size,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false,
    });
    let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: None, layout: &pipeline.get_bind_group_layout(0),
        entries: &[
            wgpu::BindGroupEntry { binding: 0, resource: cases_buf.as_entire_binding() },
            wgpu::BindGroupEntry { binding: 1, resource: out.as_entire_binding() },
        ],
    });
    let mut encoder = device.create_command_encoder(&Default::default());
    {
        let mut pass = encoder.begin_compute_pass(&Default::default());
        pass.set_pipeline(&pipeline);
        pass.set_bind_group(0, &bind_group, &[]);
        pass.dispatch_workgroups((cases.len() as u32).div_ceil(64), 1, 1);
    }
    encoder.copy_buffer_to_buffer(&out, 0, &staging, 0, out_size);
    queue.submit([encoder.finish()]);
    let (sender, receiver) = std::sync::mpsc::channel();
    staging.slice(..).map_async(wgpu::MapMode::Read, move |r| { let _ = sender.send(r); });
    device.poll(wgpu::PollType::Wait).expect("GPU device polling failed");
    receiver.recv().expect("brdf channel").expect("brdf map failed");
    let v: Vec<[f32; 2]> = bytemuck::cast_slice(&staging.slice(..).get_mapped_range()).to_vec();
    Some(v)
}

#[test]
fn deferred_direct_brdf_equals_path_tracer_brdf() {
    let mut cases = Vec::new();
    for &tl in &[0.0f32, 0.6, 1.2] {
        for &tv in &[0.0f32, 0.5, 1.1] {
            for &az in &[0.0f32, 1.3, 3.0] {
                for &rough in &[0.03f32, 0.15, 0.5, 0.8, 1.0] {
                    for &metal in &[0.0f32, 0.5, 1.0] {
                        cases.push([tl, tv, az, rough, metal, 0.9, 0.5, 0.2]);
                    }
                }
            }
        }
    }
    let Some(out) = eval_both(&cases) else {
        eprintln!("[DIRECT-BRDF] no adapter — skipping");
        return;
    };
    for (c, o) in cases.iter().zip(&out) {
        let (pt, raster) = (o[0], o[1]);
        assert!(pt.is_finite() && raster.is_finite(), "non-finite BRDF for {c:?}");
        let tol = 1e-4 + 1e-4 * pt.abs();
        assert!(
            (pt - raster).abs() <= tol,
            "deferred direct BRDF != PT for (θl {}, θv {}, φ {}, rough {}, metal {}): PT {pt} raster {raster}",
            c[0], c[1], c[2], c[3], c[4]
        );
    }
}
