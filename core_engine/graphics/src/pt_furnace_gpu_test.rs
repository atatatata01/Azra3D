// White-furnace measurement for the triangle path tracer's surface BSDF (owner 2026-10-13:
// "the render is quite black — the HDR / metal ratio looks wrong"). A white surface inside a
// uniform white environment must reflect (almost) all of it: the directional albedo
// E[f·cos/pdf] for albedo 1 is the energy the material returns. Anything well below 1 renders
// DARK under an HDRI regardless of lighting. Runs the PRODUCTION functions (sliced verbatim from
// the shipped pathtrace_tri_entry.wgsl) through the production sampler, on the GPU.

use crate::rt_shadow_gpu_test::try_device;

pub(super) fn wgsl_fn(source: &str, name: &str) -> String {
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

/// (roughness, metalness, n·v) cases, and the measured directional albedo per case.
pub(crate) fn measure_directional_albedo(cases: &[[f32; 3]]) -> Option<Vec<f32>> {
    let (device, queue) = try_device()?;
    let pt = include_str!("pathtrace_tri_entry.wgsl");
    let mut source = String::from("const PT_PI: f32 = 3.14159265;\n");
    for name in [
        "pt_pcg_hash", "pt_rand", "pt_cosine_hemisphere", "pt_fresnel_schlick", "pt_ggx_D",
        "pt_smith_G2", "pt_eval_f_cos", "pt_pdf_bsdf", "pt_sample_bsdf",
    ] {
        source.push_str(&wgsl_fn(pt, name));
        source.push('\n');
    }
    source.push_str(r#"
@group(0) @binding(0) var<storage, read> cases: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> sums: array<f32>;
const SAMPLES: u32 = 4096u;
@compute @workgroup_size(64)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let lanes = 256u;
    let c = gid.x / lanes;
    if (c >= arrayLength(&cases)) { return; }
    let rough = cases[c].x;
    let metal = cases[c].y;
    let ndv = cases[c].z;
    let n = vec3(0.0, 0.0, 1.0);
    let v = vec3(sqrt(max(1.0 - ndv * ndv, 0.0)), 0.0, ndv);
    var seed = pt_pcg_hash(gid.x * 9781u + 12345u);
    var acc = 0.0;
    for (var i = 0u; i < SAMPLES; i++) {
        var pdf = 0.0;
        var f = vec3(0.0);
        let l = pt_sample_bsdf(n, v, vec3(1.0), rough, metal, &seed, &pdf, &f);
        if (pdf > 1e-8 && dot(l, n) > 0.0) { acc += f.g / pdf; }
    }
    sums[gid.x] = acc / f32(SAMPLES);
}
"#);
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("pt_furnace"), source: wgpu::ShaderSource::Wgsl(source.into()),
    });
    let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some("pt_furnace"), layout: None, module: &module, entry_point: Some("main"),
        compilation_options: Default::default(), cache: None,
    });
    let case_data: Vec<[f32; 4]> = cases.iter().map(|c| [c[0], c[1], c[2], 0.0]).collect();
    let cases_buf = crate::rt_shadow_gpu_test::upload(&device, &queue, &case_data, "furnace cases");
    let lanes = 256usize;
    let out_size = (cases.len() * lanes * 4) as u64;
    let out = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("furnace sums"), size: out_size,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC, mapped_at_creation: false,
    });
    let staging = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("furnace staging"), size: out_size,
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
        pass.dispatch_workgroups(((cases.len() * lanes) as u32).div_ceil(64), 1, 1);
    }
    encoder.copy_buffer_to_buffer(&out, 0, &staging, 0, out_size);
    queue.submit([encoder.finish()]);
    let (sender, receiver) = std::sync::mpsc::channel();
    staging.slice(..).map_async(wgpu::MapMode::Read, move |r| { let _ = sender.send(r); });
    device.poll(wgpu::PollType::Wait).expect("GPU device polling failed");
    receiver.recv().expect("furnace channel").expect("furnace map failed");
    let sums: Vec<f32> = bytemuck::cast_slice(&staging.slice(..).get_mapped_range()).to_vec();
    Some(sums.chunks(lanes).map(|c| c.iter().sum::<f32>() / lanes as f32).collect())
}

#[test]
fn pt_bsdf_white_furnace_report() {
    let mut cases = Vec::new();
    for &metal in &[0.0f32, 1.0] {
        for &rough in &[0.05f32, 0.25, 0.5, 0.75, 1.0] {
            for &ndv in &[1.0f32, 0.5, 0.2] {
                cases.push([rough, metal, ndv]);
            }
        }
    }
    let Some(albedo) = measure_directional_albedo(&cases) else {
        eprintln!("[PT-FURNACE] no adapter — skipping");
        return;
    };
    eprintln!("[PT-FURNACE] directional albedo of a WHITE surface (ideal ≈ 1.0):");
    for (c, a) in cases.iter().zip(&albedo) {
        eprintln!("[PT-FURNACE] metal={:.0} rough={:.2} n·v={:.1} → {:.3}", c[1], c[0], c[2], a);
        assert!(a.is_finite() && *a > 0.0, "non-physical furnace value");
    }
    // White METAL (F0 = 1): the estimator must equal the exact single-scattering integral of
    // the same BRDF (height-correlated Smith GGX), computed independently by hemisphere
    // quadrature on the CPU (`gpu oracle` 2026-10-13). This fails on BOTH 2026-10-13 bugs: the
    // Schlick k=(a+1)²/8 G (0.443 at rough 0.05, n·v 0.2) and the biased below-horizon cosine
    // re-draw (1.79 at rough 0.75, n·v 0.2 — energy created).
    let metal_oracle: [(f32, [f32; 3]); 5] = [
        (0.05, [0.999, 0.999, 0.999]),
        (0.25, [0.996, 0.988, 0.946]),
        (0.50, [0.916, 0.857, 0.848]),
        (0.75, [0.627, 0.663, 0.772]),
        (1.00, [0.307, 0.451, 0.642]),
    ];
    for (c, a) in cases.iter().zip(&albedo).filter(|(c, _)| c[1] == 1.0) {
        let (_, row) = metal_oracle.iter().find(|(r, _)| *r == c[0]).expect("oracle row");
        let expected = row[[1.0f32, 0.5, 0.2].iter().position(|x| *x == c[2]).expect("oracle column")];
        assert!(*a <= 1.0 + 0.02, "white metal creates energy: rough={} n·v={} → {a}", c[0], c[2]);
        assert!((a - expected).abs() < 0.02, "white metal rough={} n·v={}: {a} vs oracle {expected}", c[0], c[2]);
    }
}
