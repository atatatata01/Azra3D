use super::*;

const NODE_MATERIAL_FALLBACK: &str = "fn eval_node_material(hit_prim: u32, p: vec3<f32>, n: vec3<f32>) -> MatResult {\n    return eval_material(hit_prim, p, n);\n}";

/// Process post-processing WGSL with LDR output format substitution.
/// `ldr_format` is the WGSL storage texture format string (e.g. "rgba16float", "rgba8unorm").
///
/// SSR quality pass (2026-07-18): pass_ssr subtracts the deferred IBL specular
/// by hit confidence, so it needs the EXACT env chain + split-sum envelope the
/// deferred used. Both are extracted (not duplicated — the PT-tri precedent):
/// the ENV region of sdf_lighting.wgsl (sample_env_mip et al., env_data binding
/// 21 declared inline because sdf_lighting declares it outside its regions) and
/// the ENV_BRDF region of deferred_entry.wgsl (env_brdf + brdf_lut binding 39).
/// `unpack_rgb` is injected like PT-tri injects safe_dir (region dependency
/// declared in the sdf header, 4 lines).
///
/// Phase C1 (PBR_REFINE_HARDENING_PLAN): also extract AO_UPSAMPLE so pass_ssr
/// multiplies the IBL subtract / probe/traced add by the same
/// `specular_occlusion(ndv, ssao_factor, rough)` deferred applied. `ssao_in`@38
/// is declared inline (home declaration sits outside the region).
pub fn process_post_process_wgsl(ldr_format: &str) -> String {
    let base = POST_PROCESS_WGSL.replace("{{LDR_FORMAT}}", ldr_format);
    let env = extract_region(SDF_LIGHTING_WGSL, "ENV");
    let env_brdf = extract_region(DEFERRED_ENTRY_WGSL, "ENV_BRDF");
    let ao = extract_region(DEFERRED_ENTRY_WGSL, "AO_UPSAMPLE");
    format!(
        "{base}\n\
         // ── SSR env access (extracted from sdf_lighting.wgsl / deferred_entry.wgsl) ──\n\
         @group(0) @binding(21) var<storage, read> env_data: array<f32>;\n\
         @group(0) @binding(38) var ssao_in: texture_2d<f32>;\n\
         fn unpack_rgb(packed: u32) -> vec3<f32> {{\n\
             return vec3<f32>(\n\
                 f32((packed >> 16u) & 0xFFu) / 255.0,\n\
                 f32((packed >> 8u) & 0xFFu) / 255.0,\n\
                 f32(packed & 0xFFu) / 255.0\n\
             );\n\
         }}\n\
         {env}\n{env_brdf}\n{ao}\n"
    )
}

// ============================================================
// Split shader builders — one shader module per entry point
// Each produces a smaller WGSL source for faster GPU compilation
// ============================================================

/// Lightweight `apply_boolean_blend`: single-tap hash noise on **k** (no FBM / no recursive
/// inlining). Amp/freq sliders actually affect the soft blend zone without the TDR-causing
/// full noisy-blend chain. Octaves/type are accepted for ABI parity but not multi-sampled.
const BLEND_STUB_FN: &str = r#"
fn bn_hash31(p: vec3<f32>) -> f32 {
    var p3 = fract(p * 0.1031);
    p3 += dot(p3, p3.yzx + 33.33);
    return fract((p3.x + p3.y) * p3.z);
}
fn apply_boolean_blend(result: f32, d: f32, op: u32, k: f32, bp: u32, sd: f32,
    p: vec3<f32>, amp: f32, freq: f32, octaves: u32, noise_type: u32) -> f32 {
    var k_use = k;
    // Real noise only when amp>0 and octaves is a count (1..16). Curve LUT packs 0..1 floats
    // into these slots — bitcast octave field is huge, so noise stays off.
    if (amp > 0.001 && octaves >= 1u && octaves <= 16u && k > 0.0) {
        let cell = floor(p * max(freq, 0.01));
        let n = bn_hash31(cell) * 2.0 - 1.0; // [-1,1]
        k_use = max(k * (1.0 + n * amp), 0.0);
        return apply_boolean(result, d, op, k_use, bp, sd);
    }
    // Curve LUT mode: the four noise slots hold y-samples at x=0,1/3,2/3,1 as plain f32.
    let lut = vec4(amp, freq, bitcast<f32>(octaves), bitcast<f32>(noise_type));
    return apply_boolean_lut(result, d, op, k_use, bp, sd, 1.0, lut);
}
"#;

/// Neutralize the heavy noisy blend chain in WGSL shaders.
///
/// Strips the `// #region NOISY_BLEND` block (full FBM blend) and injects a lightweight
/// `apply_boolean_blend` that only hash-modulates k — so Amplitude/Frequency sliders work
/// without Naga exploding into TDR-inducing FBM inlines.
///
/// `fn fbm_wgsl` is kept for surface noise modifiers.
pub fn neutralize_blend_noise_wgsl(wgsl: &str) -> String {
    // Strip the NOISY_BLEND region atomically — removes all 4 heavy functions at once
    let mut src = strip_regions(wgsl, &["NOISY_BLEND"]);
    // Inject lightweight blend that still honors amp/freq
    src.push_str(BLEND_STUB_FN);
    src
}

/// Backward-compatible alias for `neutralize_blend_noise_wgsl`.
/// Used by volume_bake consumers that don't go through `build_shared_wgsl`.
pub fn stub_out_blend_noise_wgsl(wgsl: &str) -> String {
    neutralize_blend_noise_wgsl(wgsl)
}

/// Concatenate all 5 split SDF files into the full shared source.
/// Equivalent to the old monolithic sdf_shared.wgsl.
fn sdf_shared_all() -> String {
    let mut s = String::with_capacity(16384);
    s.push_str(SDF_HEADER_WGSL);  s.push('\n');
    s.push_str(SDF_PRIM_EVAL_WGSL); s.push('\n');
    s.push_str(SDF_SCENE_WGSL);   s.push('\n');
    s.push_str(SDF_MATERIAL_WGSL); s.push('\n');
    s.push_str(SDF_LIGHTING_WGSL);
    s
}

/// Build the shared WGSL prefix (types + bindings + functions) with template substitution.
/// Note: HOT/COLD stride is hardcoded in the shader (80u / 224u). Only NODE_MATERIAL_FN remains templated.
fn build_shared_wgsl(node_wgsl: Option<&str>) -> String {
    let code = node_wgsl.unwrap_or(NODE_MATERIAL_FALLBACK);
    let src = sdf_shared_all()
        .replace("{{FLOATS_PER_PRIM}}", &FLOATS_PER_PRIM.to_string())
        .replace("{{NODE_MATERIAL_FN}}", code);
    neutralize_blend_noise_wgsl(&src)
}

/// Strip `// #region NAME` ... `// #endregion NAME` blocks from WGSL source.
/// Any region whose name appears in `exclude` is removed entirely.
fn strip_regions(wgsl: &str, exclude: &[&str]) -> String {
    let mut result = String::with_capacity(wgsl.len());
    let mut skip_depth = 0u32;
    for line in wgsl.lines() {
        let trimmed = line.trim();
        if let Some(name) = trimmed.strip_prefix("// #region ") {
            if exclude.contains(&name.trim()) {
                skip_depth += 1;
                continue;
            }
        } else if let Some(name) = trimmed.strip_prefix("// #endregion ") {
            let _ = name; // suppress unused warning
            if skip_depth > 0 {
                skip_depth -= 1;
                continue;
            }
        }
        if skip_depth == 0 {
            result.push_str(line);
            result.push('\n');
        }
    }
    result
}

/// Validate that region stripping didn't leave dangling references to region-scoped
/// symbols. Called after every `strip_regions()` to catch bugs at build time.
/// Panics with a clear message identifying the broken symbol and shader label.
fn validate_no_dangling_region_refs(source: &str, label: &str) {
    // Symbols that live exclusively inside #region RAY_CULL.
    // If the region was stripped, NONE of these should appear in the source.
    // If the region was kept, ALL of these should be both declared and used.
    const CULL_SYMBOLS: &[(&str, &str)] = &[
        ("g_cull_mask",              "var<private> g_cull_mask"),
        ("scene_sdf_culled",         "fn scene_sdf_culled"),
        ("scene_culled_analytical",  "fn scene_culled_analytical"),
    ];
    for &(usage, decl_prefix) in CULL_SYMBOLS {
        if source.contains(usage) && !source.contains(decl_prefix) {
            panic!(
                "[SHADER BUILD] '{}' references '{}' but it is not declared — \
                 region stripping removed the declaration but left a reference!",
                label, usage
            );
        }
    }
}

/// Regions excluded from fog shader (not reachable from pass_fog entry point).
/// SCENE_EVAL kept so fog_shadow can use scene_sdf() → voxel O(1) when cached.
const FOG_EXCLUDE: &[&str] = &[
    "DNA", "LEGACY_TEXTURE", "LEGACY_NOISE", "CURVATURE",
    "SCENE_QUERY", "SHADOW_AO", "RNG_TRACE",
    "MATERIAL", "SHADING", "GBUF_BINDINGS", "LIGHTING", "ATLAS", "ENV",
    "RAY_CULL",
];

// (PT_EXCLUDE + build_pathtrace_wgsl REMOVED 2026-07-16 — the analytical PT entry
//  is deleted; build_pathtrace_tri_wgsl below is the only PT build.)

// (build_raymarch_wgsl / build_raymarch_march_wgsl + RM_EXCLUDE/MARCH_EXCLUDE
//  REMOVED 2026-07-14 — the WGSL analytical SDF march display path is stripped.
//  The RAY_CULL region in sdf_scene.wgsl was consumed only by those two builders.)

/// Build WGSL for the lite raymarch shader (floor + background only).
/// Self-contained — no sdf_shared.wgsl, no SDF functions. Compiles instantly.
pub fn build_raymarch_lite_wgsl() -> String {
    RAYMARCH_LITE_WGSL.to_string()
}

/// Regions excluded from the selected-prim OUTLINE shader: everything the march kernel
/// strips PLUS the scene-level machinery — the outline entry marches individual prims via
/// `eval_prim` only (no scene_sdf, no cull mask, no G-buffer outputs, no scene queries).
const PRIM_OUTLINE_EXCLUDE: &[&str] = &[
    "DNA", "LEGACY_TEXTURE", "LEGACY_NOISE", "CURVATURE",
    "SHADOW_AO", "RNG_TRACE",
    "MATERIAL", "SHADING", "LIGHTING", "ATLAS", "ENV",
    "BVH_EVAL",
    "SCENE_QUERY", "SCENE_EVAL", "RAY_CULL", "GBUF_BINDINGS", "SDF_REFLECT",
];

/// Build WGSL for the selected-prim outline mask pass (owner feature: GPU-precise
/// per-prim selection contour + pale cutter volumes). `sdf_shared` header + `eval_prim`
/// stack + the small outline entry — the silhouette uses the exact production SDF eval,
/// so every shape/modifier/bevel/elongation matches the baked surface bit-for-bit.
pub fn build_prim_outline_wgsl() -> String {
    let shared = build_shared_wgsl(None);
    let trimmed = strip_regions(&shared, PRIM_OUTLINE_EXCLUDE);
    let result = format!("{}\n\n{}", trimmed, PRIM_OUTLINE_ENTRY_WGSL);
    validate_no_dangling_region_refs(&result, "prim_outline");
    result
}

/// Regions excluded from deferred shader (full version with material DNA).
/// SHADOW_AO kept — deferred computes shadow/AO for march kernel pixels (shadow_ao == 1.0).
const DF_EXCLUDE: &[&str] = &[
    "RNG_TRACE",
    "RAY_CULL",
];

/// Regions excluded from deferred-lite shader (no material eval, no scene_sdf).
/// Used at startup for instant viewport; full DF compiled lazily in background.
const DF_LITE_EXCLUDE: &[&str] = &[
    "DNA", "LEGACY_TEXTURE", "LEGACY_NOISE", "CURVATURE",
    "SCENE_QUERY", "SCENE_EVAL", "SHADOW_AO", "RNG_TRACE",
    "MATERIAL", "ATLAS",
    "RAY_CULL", "SDF_REFLECT",
];

/// Build WGSL source for the deferred shading entry point only (full version).
/// Dead-code trimmed: strips raytrace/shadow SDF code (~400 lines).
/// Shadow/AO step counts reduced vs. sdf_shared.wgsl defaults to limit Naga-inlined
/// SDF evaluations and prevent execution TDR on the WGSL path.
pub fn build_deferred_wgsl(node_wgsl: Option<&str>) -> String {
    let shared = build_shared_wgsl(node_wgsl);
    let trimmed = strip_regions(&shared, DF_EXCLUDE);
    let mut result = format!("{}\n\n{}", trimmed, DEFERRED_ENTRY_WGSL);
    // Reduce shadow/AO step counts for WGSL deferred path.
    // Naga inlines every scene_sdf() call, so 32 shadow steps × N prims = massive code.
    // GLSL BVH path is unaffected (has real function calls, computes shadow/AO during march).
    // soft_shadow: 16/24/32 → 6/8/10  (each step = full inline scene_sdf)
    result = result.replace(
        "select(16u, select(24u, 32u, config.render_quality >= 3u), config.render_quality >= 2u)",
        "select(6u, select(8u, 10u, config.render_quality >= 3u), config.render_quality >= 2u)",
    );
    // calc_ao: 3/5/8 → 2/3/4
    result = result.replace(
        "select(3u, select(5u, 8u, config.render_quality >= 3u), config.render_quality >= 2u)",
        "select(2u, select(3u, 4u, config.render_quality >= 3u), config.render_quality >= 2u)",
    );
    validate_no_dangling_region_refs(&result, "deferred");
    result
}

/// Build WGSL source for deferred-lite: G-buffer values only, no material DNA/blended eval.
/// Compiles near-instantly since it has zero scene_sdf dependencies.
pub fn build_deferred_lite_wgsl() -> String {
    let shared = build_shared_wgsl(None);
    let trimmed = strip_regions(&shared, DF_LITE_EXCLUDE);
    let entry = strip_regions(DEFERRED_ENTRY_WGSL, &["DEFERRED_MATERIAL", "SHADOW_AO", "SDF_REFLECT", "LIT_BRANCH"]);
    let result = format!("{}\n\n{}", trimmed, entry);
    validate_no_dangling_region_refs(&result, "deferred_lite");
    result
}

/// Build WGSL source for the volumetric fog entry point only.
/// Dead-code trimmed: strips ~750 lines unreachable from pass_fog.
pub fn build_fog_wgsl(node_wgsl: Option<&str>) -> String {
    let shared = build_shared_wgsl(node_wgsl);
    let trimmed = strip_regions(&shared, FOG_EXCLUDE);
    let result = format!("{}\n\n{}", trimmed, FOG_ENTRY_WGSL);
    validate_no_dangling_region_refs(&result, "fog");
    result
}

/// Build WGSL for the TRIANGLE path tracer (Phase 2 — the production PT).
/// Header (GpuUniforms + camera_ray + output bindings) + the LIGHTING and ENV
/// regions of sdf_lighting.wgsl (light accessors + sample_env — extracted, not
/// duplicated) + the TLAS/BLAS triangle traversal entry. Deliberately does NOT
/// include sdf_prim_eval / sdf_scene / sdf_material — the analytical SDF march
/// is banned from PT (owner directive 2026-07-16); PT traces the raster
/// triangles only. The env/light buffer bindings (21/26) are declared inline
/// because sdf_lighting.wgsl declares them OUTSIDE its regions.
pub fn build_pathtrace_tri_wgsl() -> String {
    build_pathtrace_tri_wgsl_with_mode(None)
}

pub fn build_pathtrace_tri_wgsl_with_mode(interactive: Option<bool>) -> String {
    let header = SDF_HEADER_WGSL
        .replace("{{FLOATS_PER_PRIM}}", &FLOATS_PER_PRIM.to_string());
    let lighting = extract_region(SDF_LIGHTING_WGSL, "LIGHTING");
    let env = extract_region(SDF_LIGHTING_WGSL, "ENV");
    let source = format!(
        "{header}\n\
         @group(0) @binding(21) var<storage, read> env_data: array<f32>;\n\
         @group(0) @binding(26) var<storage, read> light_data: array<f32>;\n\
         fn safe_dir(v: vec3<f32>) -> vec3<f32> {{ let l = length(v); if (l < 0.0001) {{ return vec3(0.0, 1.0, 0.0); }} return v / l; }}\n\
         {lighting}\n{env}\n{precision}\n{progressive}\n{entry}",
        precision = include_str!("material_precision.wgsl"),
        progressive = include_str!("pt_progressive.wgsl"),
        entry = PATHTRACE_TRI_ENTRY_WGSL
    );
    let Some(interactive) = interactive else { return source };
    let declaration = "override PT_INTERACTIVE: bool = false;";
    assert_eq!(source.matches(declaration).count(), 1, "canonical PT mode declaration changed");
    let mut source = source.replace(declaration, &format!("const PT_INTERACTIVE: bool = {interactive};"));
    loop {
        let module = naga::front::wgsl::parse_str(&source).unwrap_or_else(|error| {
            panic!("PT mode specialization parse: {}", error.emit_to_string(&source))
        });
        let mut edits = Vec::new();
        for (_, function) in module.functions.iter() {
            collect_pt_static_branches(&function.body, function, &module, &source, &mut edits);
        }
        for entry in &module.entry_points {
            collect_pt_static_branches(&entry.function.body, &entry.function, &module, &source, &mut edits);
        }
        if edits.is_empty() {
            let mut reachable = vec![false; module.functions.len()];
            let mut pending = Vec::new();
            for entry in &module.entry_points {
                collect_pt_function_calls(&entry.function.body, &mut pending);
            }
            while let Some(handle) = pending.pop() {
                if std::mem::replace(&mut reachable[handle.index()], true) {
                    continue;
                }
                collect_pt_function_calls(&module.functions[handle].body, &mut pending);
            }
            for (handle, _) in module.functions.iter() {
                if !reachable[handle.index()] {
                    let range = pt_complete_block_span(&source, module.functions.get_span(handle).to_range().expect("PT function source span"));
                    edits.push((range, String::new()));
                }
            }
            edits.sort_by_key(|(range, _)| std::cmp::Reverse(range.start));
            for (range, replacement) in edits {
                source.replace_range(range, &replacement);
            }
            return source;
        }
        edits.sort_by_key(|(range, _)| std::cmp::Reverse(range.start));
        for (range, replacement) in edits {
            source.replace_range(range, &replacement);
        }
    }
}

fn pt_complete_block_span(source: &str, mut range: std::ops::Range<usize>) -> std::ops::Range<usize> {
    let bytes = source.as_bytes();
    let mut index = range.start;
    let mut braces = 0usize;
    let mut comments = 0usize;
    let mut line_comment = false;
    while index < bytes.len() && (index < range.end || braces > 0 || comments > 0 || line_comment) {
        let pair = bytes.get(index..index + 2);
        if line_comment {
            line_comment = bytes[index] != b'\n';
        } else if comments > 0 {
            if pair == Some(b"/*") {
                comments += 1;
                index += 1;
            } else if pair == Some(b"*/") {
                comments -= 1;
                index += 1;
            }
        } else if pair == Some(b"//") {
            line_comment = true;
            index += 1;
        } else if pair == Some(b"/*") {
            comments = 1;
            index += 1;
        } else if bytes[index] == b'{' {
            braces += 1;
        } else if bytes[index] == b'}' {
            braces = braces.checked_sub(1).expect("PT source span brace balance");
        }
        index += 1;
    }
    assert_eq!(braces, 0, "PT source span incomplete block");
    assert_eq!(comments, 0, "PT source span incomplete comment");
    range.end = index;
    range
}

fn pt_static_bool(function: &naga::Function, module: &naga::Module, handle: naga::Handle<naga::Expression>) -> Option<bool> {
    match function.expressions[handle] {
        naga::Expression::Literal(naga::Literal::Bool(value)) => Some(value),
        naga::Expression::Constant(constant) => match module.global_expressions[module.constants[constant].init] {
            naga::Expression::Literal(naga::Literal::Bool(value)) => Some(value),
            _ => None,
        },
        naga::Expression::Unary { op: naga::UnaryOperator::LogicalNot, expr } => pt_static_bool(function, module, expr).map(|value| !value),
        naga::Expression::Binary { op, left, right } => {
            let left = pt_static_bool(function, module, left);
            let right = pt_static_bool(function, module, right);
            match (op, left, right) {
                (naga::BinaryOperator::LogicalAnd, Some(false), _) | (naga::BinaryOperator::LogicalAnd, _, Some(false)) => Some(false),
                (naga::BinaryOperator::LogicalAnd, Some(true), Some(true)) => Some(true),
                (naga::BinaryOperator::LogicalOr, Some(true), _) | (naga::BinaryOperator::LogicalOr, _, Some(true)) => Some(true),
                (naga::BinaryOperator::LogicalOr, Some(false), Some(false)) => Some(false),
                _ => None,
            }
        }
        _ => None,
    }
}

fn collect_pt_static_branches(
    block: &naga::Block,
    function: &naga::Function,
    module: &naga::Module,
    source: &str,
    edits: &mut Vec<(std::ops::Range<usize>, String)>,
) {
    for (statement, span) in block.span_iter() {
        match statement {
            naga::Statement::If { condition, accept, reject } => {
                if let Some(value) = pt_static_bool(function, module, *condition) {
                    let selected = if value { accept } else { reject };
                    let range = selected.span_iter().filter_map(|(_, span)| span.to_range())
                        .reduce(|left, right| left.start.min(right.start)..left.end.max(right.end));
                    let body = range.map_or("", |mut range| {
                        let remaining = &source[range.end..];
                        let whitespace = remaining.len() - remaining.trim_start().len();
                        if remaining.as_bytes().get(whitespace) == Some(&b';') {
                            range.end += whitespace + 1;
                        }
                        &source[range]
                    });
                    let branch_range = pt_complete_block_span(source, span.to_range().expect("PT branch source span"));
                    edits.push((branch_range, format!("{{\n{body}\n}}")));
                } else {
                    collect_pt_static_branches(accept, function, module, source, edits);
                    collect_pt_static_branches(reject, function, module, source, edits);
                }
            }
            naga::Statement::Block(body) => collect_pt_static_branches(body, function, module, source, edits),
            naga::Statement::Loop { body, continuing, .. } => {
                collect_pt_static_branches(body, function, module, source, edits);
                collect_pt_static_branches(continuing, function, module, source, edits);
            }
            naga::Statement::Switch { cases, .. } => {
                for case in cases {
                    collect_pt_static_branches(&case.body, function, module, source, edits);
                }
            }
            _ => {}
        }
    }
}

fn collect_pt_function_calls(block: &naga::Block, pending: &mut Vec<naga::Handle<naga::Function>>) {
    for statement in block.iter() {
        match statement {
            naga::Statement::Call { function, .. } => pending.push(*function),
            naga::Statement::Block(body) => collect_pt_function_calls(body, pending),
            naga::Statement::If { accept, reject, .. } => {
                collect_pt_function_calls(accept, pending);
                collect_pt_function_calls(reject, pending);
            }
            naga::Statement::Loop { body, continuing, .. } => {
                collect_pt_function_calls(body, pending);
                collect_pt_function_calls(continuing, pending);
            }
            naga::Statement::Switch { cases, .. } => {
                for case in cases {
                    collect_pt_function_calls(&case.body, pending);
                }
            }
            _ => {}
        }
    }
}

/// Build WGSL for the RT projected-shadows visibility pass (RT_SHADOWS_DESIGN Stage 1).
/// Header (GpuUniforms + camera_ray + output_texture) + the PT_TRAVERSAL region of
/// pathtrace_tri_entry.wgsl (BVH structs/bindings 42-47 + pt_occluded — EXTRACTED, not
/// duplicated: one traversal, one CPU oracle) + the LIGHTING region of sdf_lighting.wgsl
/// (light accessors) + the rt_shadow entry. Declares light_data(26) + gbuf_normal(12)
/// inline (sdf_lighting declares them OUTSIDE its regions), plus safe_dir (light_dir_v's
/// dependency) — the exact PT-tri pattern. The PT-only symbols the traversal span carries
/// (PtMaterial, pt_accum binding 27, pt_materials binding 48, the RNG) are unused by the
/// entry and Naga dead-code-strips them; rt_shadow_bgl_entries binds only 0/1/12/26/42-47.
pub fn build_rt_shadow_wgsl() -> String {
    let header = SDF_HEADER_WGSL
        .replace("{{FLOATS_PER_PRIM}}", &FLOATS_PER_PRIM.to_string());
    let traversal = extract_region(PATHTRACE_TRI_ENTRY_WGSL, "PT_TRAVERSAL");
    let lighting = extract_region(SDF_LIGHTING_WGSL, "LIGHTING");
    format!(
        "{header}\n\
         @group(0) @binding(26) var<storage, read> light_data: array<f32>;\n\
         fn safe_dir(v: vec3<f32>) -> vec3<f32> {{ let l = length(v); if (l < 0.0001) {{ return vec3(0.0, 1.0, 0.0); }} return v / l; }}\n\
         {precision}\n{traversal}\n{lighting}\n{entry}",
        precision = include_str!("material_precision.wgsl"),
        entry = RT_SHADOW_ENTRY_WGSL
    )
}

/// Build the half-res screen-space GI shader (SSGI_PLAN.md §4.9).
///
/// Header (GpuUniforms + unpack_rgb + camera_ray + `output_texture`@0) + the two
/// regions whose functions the delta must reproduce EXACTLY, then the entry.
///
/// Both extractions are load-bearing rather than convenience. SSGI's output is an
/// energy DELTA against what `deferred_entry.wgsl` already applied, so it has to
/// evaluate the *same* `env_sh_irradiance` and the *same* `ssao_factor` /
/// `ao_bent_normal` / `multi_bounce_ao` — hand-copied twins would drift apart
/// silently and un-conserve energy months later, which is why the SSR pass
/// extracts rather than duplicates too (`process_post_process_wgsl`).
///
/// `env_data`(21) and `ssao_in`(38) are declared inline because their home files
/// declare them OUTSIDE any region (`sdf_lighting.wgsl:63`,
/// `deferred_entry.wgsl:59`), so extraction does not carry them. No LIGHTING
/// region, so no `safe_dir` injection is needed; `unpack_rgb` comes free from the
/// header. The header's `voxel_grid`(2) / `prim_data`(3) / `tex_data`(4) globals
/// are unreferenced here and Naga dead-code-strips them — `ssgi_bgl_entries()`
/// binds only what the entry actually uses (the `rt_shadow_bgl_entries` precedent).
pub fn build_ssgi_wgsl() -> String {
    let header = SDF_HEADER_WGSL.replace("{{FLOATS_PER_PRIM}}", &FLOATS_PER_PRIM.to_string());
    let env = extract_region(SDF_LIGHTING_WGSL, "ENV");
    let ao = extract_region(DEFERRED_ENTRY_WGSL, "AO_UPSAMPLE");
    format!(
        "{header}\n\
         @group(0) @binding(21) var<storage, read> env_data: array<f32>;\n\
         @group(0) @binding(38) var ssao_in: texture_2d<f32>;\n\
         {env}\n{ao}\n{entry}",
        entry = SSGI_ENTRY_WGSL
    )
}

/// Build the GPU material-map bake shader (GPU_MATERIAL_BAKE_PLAN.md P1).
///
/// `node_wgsl` is `NodeGraph::generate_wgsl()` — the graph's own compiled
/// `eval_node_material(hit_prim, p, n) -> MatResult` plus the helper fns it needs. That output is
/// SELF-CONTAINED (no `tex_atlas` / `eval_scene_at` / `prim_*` references), which is why this
/// pass needs no SDF header: `MatResult` is declared in `material_bake.wgsl` itself rather than
/// pulled from `sdf_material.wgsl`, whose declaration drags in the whole prim/lighting stack.
///
/// CALLER CONTRACT: only pass a graph that `node_graph_bake::graph_is_gpu_bakeable` accepts.
/// Graphs containing `ImageTexture` or the surface analyzers compile fine here but evaluate a
/// white placeholder / synthetic height field, i.e. plausible-but-wrong pixels.
///
/// `hash31` — NOT part of `node_wgsl`. `NodeGraph::generate_wgsl()`'s own doc comment
/// (material_bake.wgsl:24) claims the generated function is "SELF-CONTAINED (no `tex_atlas` /
/// `eval_scene_at` / `prim_*` references)", which is true for scene/prim bindings but turned out
/// to be false for this one helper: Noise/Voronoi/BrickTexture codegen (all on the
/// `graph_is_gpu_bakeable` whitelist — `shader_nodes/graph.rs` calls `hash31` from `sn_grad3`'s
/// gradient-Perlin lattice and from Voronoi's cell hashing) assumes `hash31` is supplied by
/// whatever shader the generated code is spliced into, the same way `sdf_prim_eval.wgsl` and
/// `render/src/gbuffer.wgsl` each define their own copy for their own callers. Discovered by the
/// P1 GPU/CPU parity test actually exercising a Noise node — the hand-written stub in this file's
/// own `validate_material_bake_shader` test never calls `hash31`, so pure parse+validate on the
/// stub could not catch a missing-identifier error that only a REAL generated graph hits.
///
/// MUST match `node_graph_bake::hash31` (app/src/node_graph_bake.rs) EXACTLY — the CPU oracle's
/// hash, NOT `sdf_prim_eval.wgsl`'s `hash31` (an unrelated SDF-noise hash that happens to share
/// the name) or `sdf_prim_eval.wgsl`'s `hash3d_gpu` (a third, `sin`-based, formula). A first pass
/// of this fix copied `sdf_prim_eval.wgsl`'s `hash31` on name-match alone; that shader parses and
/// validates fine but produces a DIFFERENT noise lattice, which showed up as a large but
/// non-obviously-wrong albedo divergence (mean 14/255, max 60/255) in the parity gate rather than
/// a compile error — exactly the "shares the same wrong convention" trap
/// `tests_gpu_curvature.rs`'s header warns a bare GPU==CPU test cannot catch on its own; only
/// re-deriving from the CPU source line by line caught it here. The CPU formula:
/// ```text
/// n = dot(p, (127.1, 311.7, 74.7))
/// n = fract(n * 0.1031)
/// n *= n + 33.33
/// n *= n + n
/// return abs(fract(n))
/// ```
const MATERIAL_BAKE_HASH31: &str = "fn hash31(p: vec3<f32>) -> f32 {\n    var n = dot(p, vec3(127.1, 311.7, 74.7));\n    n = fract(n * 0.1031);\n    n = n * (n + 33.33);\n    n = n * (n + n);\n    return abs(fract(n));\n}\n";

/// G3 — MusgraveTexture calls `fbm_wgsl` in generated node WGSL. Material bake previously
/// injected only `hash31`; volume bake already had `fbm_wgsl` (do not confuse greps).
/// Helpers match `volume_bake.wgsl` surface-noise forms (wgsl_hash / noise_3d_wgsl / fbm_wgsl).
const MATERIAL_BAKE_FBM: &str = r#"
fn wgsl_hash(p: vec3<f32>) -> f32 {
    var q = fract(p * 0.1031);
    q += vec3(dot(q, vec3(q.y + 33.33, q.z + 33.33, q.x + 33.33)));
    return fract((q.x + q.y) * q.z);
}
fn noise_3d_wgsl(p: vec3<f32>) -> f32 {
    let i = floor(p); let f = fract(p);
    let u = f * f * (3.0 - 2.0 * f);
    let n000 = wgsl_hash(i); let n100 = wgsl_hash(i + vec3(1.0,0.0,0.0));
    let n010 = wgsl_hash(i + vec3(0.0,1.0,0.0)); let n110 = wgsl_hash(i + vec3(1.0,1.0,0.0));
    let n001 = wgsl_hash(i + vec3(0.0,0.0,1.0)); let n101 = wgsl_hash(i + vec3(1.0,0.0,1.0));
    let n011 = wgsl_hash(i + vec3(0.0,1.0,1.0)); let n111 = wgsl_hash(i + vec3(1.0,1.0,1.0));
    let nx00 = mix(n000, n100, u.x);
    let nx10 = mix(n010, n110, u.x);
    let nx01 = mix(n001, n101, u.x);
    let nx11 = mix(n011, n111, u.x);
    let nxy0 = mix(nx00, nx10, u.y);
    let nxy1 = mix(nx01, nx11, u.y);
    return mix(nxy0, nxy1, u.z);
}
fn fbm_wgsl(p: vec3<f32>, octaves: u32) -> f32 {
    var val = 0.0; var amp = 0.5; var fp = p;
    for (var o = 0u; o < octaves; o++) {
        val += amp * (noise_3d_wgsl(fp) * 2.0 - 1.0);
        fp *= 2.0; amp *= 0.5;
    }
    return val;
}
"#;

pub fn build_material_bake_wgsl(node_wgsl: &str) -> String {
    build_material_graph_wgsl(node_wgsl, MATERIAL_BAKE_WGSL)
}

pub fn build_material_graph_wgsl(node_wgsl: &str, entry: &str) -> String {
    format!(
        "{transfer}{hash}{fbm}{node_wgsl}\n{entry}",
        transfer = include_str!("material_transfer.wgsl"),
        hash = MATERIAL_BAKE_HASH31,
        fbm = MATERIAL_BAKE_FBM,
        entry = entry
    )
}

pub fn build_material_resize_wgsl() -> String {
    format!("{}\n{}", include_str!("material_transfer.wgsl"), include_str!("material_resize.wgsl"))
}

/// The DC-mesh curvature kernel (MESH_CURVATURE_PLAN M1). Self-contained: it reads only the
/// mesh's own vertex/index buffers plus a vertex→triangle adjacency table, so unlike the field
/// curvature kernel it needs no SDF header, no prim stream and no scene bindings.
pub fn build_mesh_curvature_wgsl() -> String {
    MESH_CURVATURE_WGSL.to_string()
}

/// Build WGSL for the RT projected-shadow spatial bilateral denoise (RT_SHADOWS_DESIGN Stage 2).
/// Self-contained (no SDF header) — the source is complete on its own.
pub fn build_rt_shadow_denoise_wgsl() -> String {
    RT_SHADOW_DENOISE_WGSL.to_string()
}

/// Build WGSL for the RT projected-shadow temporal reprojection accumulation (RT_SHADOWS_DESIGN
/// Stage 2). Self-contained (no SDF header).
pub fn build_rt_shadow_temporal_wgsl() -> String {
    RT_SHADOW_TEMPORAL_WGSL.to_string()
}

/// Extract the content of a single named `// #region NAME` block from WGSL source.
/// Returns only the lines between the opening and closing markers.
/// Nested regions inside are included as-is (they were already stripped if unwanted).
fn extract_region(wgsl: &str, name: &str) -> String {
    let mut result = String::new();
    let mut inside = false;
    let open_tag = format!("// #region {}", name);
    let close_tag = format!("// #endregion {}", name);
    for line in wgsl.lines() {
        let trimmed = line.trim();
        if trimmed == open_tag { inside = true; continue; }
        if trimmed == close_tag { inside = false; continue; }
        if inside { result.push_str(line); result.push('\n'); }
    }
    result
}

// (build_sdf_eval_block DELETED 2026-07-17 — its ONLY consumer was build_volume_preview_wgsl.)

/// Extract SDF function library from volume_bake.wgsl for reuse in brick shaders.
///
/// Returns everything between the first prim accessor function (`fn prim_f(`) and the
/// entry point section marker (`// Bake entry point`). This includes: prim data accessors,
/// scope marker helpers, quaternion rotation, all SDF shapes, noise, modifiers, boolean ops.
///
/// Blend noise is neutralized. Bindings and BakeParams struct are EXCLUDED (consumers
/// declare their own bindings and must include BakeParams in their template).
fn extract_bake_sdf_lib() -> String {
    let src = neutralize_blend_noise_wgsl(VOLUME_BAKE_WGSL);
    // Start: first prim accessor function (after bindings + BakeParams struct)
    let fn_start = src.find("fn prim_f(")
        .expect("[brick_fill] prim_f not found in volume_bake.wgsl");
    // End: just before the entry point section
    let entry_marker = "// Bake entry point";
    let section_end = if let Some(marker_pos) = src.find(entry_marker) {
        // Walk back to find the "// ====" separator line above
        let before = &src[..marker_pos];
        if let Some(sep_pos) = before.rfind("// ====") {
            before[..sep_pos].rfind('\n').map(|p| p + 1).unwrap_or(sep_pos)
        } else {
            before.rfind('\n').map(|p| p + 1).unwrap_or(marker_pos)
        }
    } else {
        src.len()
    };
    // The blend stub is appended at the end of `src` by neutralize_blend_noise_wgsl,
    // which falls outside the [fn_start..section_end] range. Append it explicitly.
    let mut lib = src[fn_start..section_end].to_string();
    lib.push_str(BLEND_STUB_FN);
    lib
}

/// Build WGSL source for the brick fill shader.
///
/// Injects SDF evaluation functions from volume_bake.wgsl into the brick_fill.wgsl
/// template. The template provides its own bindings (brick_pool, brick_list) and
/// entry point (brick_fill). The SDF functions are identical to volume_bake.wgsl.
pub fn build_brick_fill_wgsl() -> String {
    let sdf_lib = extract_bake_sdf_lib();
    BRICK_FILL_WGSL
        .replace("// {{BAKE_SDF_LIB}}", &sdf_lib)
}

// (build_volume_preview_wgsl DELETED 2026-07-17 — the VP display pipeline was stripped
// 2026-07-13 and no caller compiled this shader; it hosted the analytical per-body
// material read (read_body_material). SDF display is triangles-only from the baked
// DC mesh via the raster pass — Display Invariant.)

/// Build WGSL source for the sculpt voxel raymarch shader (Plan B1).
/// Self-contained — no SDF eval injection, no template substitution. Isolated to the
/// sculpt atlas; never references the analytical SDF/BVH/PT path.
pub fn build_sculpt_voxel_raymarch_wgsl() -> String {
    SCULPT_VOXEL_RAYMARCH_WGSL.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn naga_parse(label: &str, src: &str) {
        match naga::front::wgsl::parse_str(src) {
            Ok(_) => eprintln!("[NAGA] {} parsed OK", label),
            Err(e) => {
                let path = std::env::temp_dir().join(format!("azra_{}_fail.wgsl", label));
                let _ = std::fs::write(&path, src);
                panic!("[NAGA] {} parse error:\n{}\nShader dumped to {:?}", label, e.emit_to_string(src), path);
            }
        };
    }

    /// P1 GATE (GPU_MATERIAL_BAKE_PLAN.md) — the assembled material-bake shader must COMPILE.
    ///
    /// `build_material_bake_wgsl` concatenates the graph's generated `eval_node_material` with
    /// `material_bake.wgsl`. Concatenation always "succeeds", so without a parse check the P1
    /// wiring could look done while producing a shader that fails at `create_shader_module` —
    /// which surfaces as a runtime panic, not a build error.
    ///
    /// Uses a hand-written stand-in for the generated function (the real codegen lives in the
    /// `app` crate, which cannot be a dependency of `graphics`). It matches the real signature
    /// and return type exactly, so it proves the ENTRY side of the seam: bindings, MatResult
    /// field access, the sRGB/scalar encoders, and the workgroup entry all type-check together.
    /// M1 GATE (MESH_CURVATURE_PLAN) — the mesh-curvature kernel must PARSE **and VALIDATE**.
    ///
    /// Parse-only is not enough: naga's `parse_str` accepts a call with the wrong argument count
    /// (proven while building `validate_material_bake_shader`, and the same "voxel_mesh lesson"
    /// the env-prefilter test documents). Only the validator matches what `create_shader_module`
    /// does at runtime, where a failure is a panic with no Option to catch.
    #[test]
    fn validate_mesh_curvature_shader() {
        let src = build_mesh_curvature_wgsl();
        let module = naga::front::wgsl::parse_str(&src)
            .unwrap_or_else(|e| panic!("[NAGA-PARSE] mesh_curvature:
{}", e.emit_to_string(&src)));
        let mut v = naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        );
        if let Err(e) = v.validate(&module) {
            let path = std::env::temp_dir().join("azra_mesh_curvature_fail.wgsl");
            let _ = std::fs::write(&path, &src);
            panic!("[NAGA-VALIDATE] mesh_curvature: {:?}
Shader dumped to {:?}", e, path);
        }
        eprintln!("[NAGA] mesh_curvature validated OK");
    }

    #[test]
    fn validate_material_bake_shader() {
        let stub_generated = "fn eval_node_material(hit_prim: u32, p: vec3<f32>, n: vec3<f32>) -> MatResult {
    var result: MatResult;
    result.color = vec3<f32>(p.x * 0.5 + 0.5, p.y * 0.5 + 0.5, 0.25);
    result.roughness = 0.4;
    result.metallic = 0.0;
    result.opacity = 1.0;
    result.emission = vec3<f32>(0.0, 0.0, 0.0);
    return result;
}
fn eval_node_height(hit_prim: u32, p: vec3<f32>, n: vec3<f32>) -> f32 { return p.x; }
fn eval_node_normal(hit_prim: u32, p: vec3<f32>, n: vec3<f32>) -> vec3<f32> { return n; }
";
        // PARSE + VALIDATE, not parse-only. `parse_str` checks SYNTAX; it happily accepts a
        // call with the wrong argument count. Verified by sabotage: changing the entry to
        // `eval_node_material(0u, p)` (2 args instead of 3) still parsed clean and this test
        // passed — the same trap `validate_env_prefilter_shader` documents as "the voxel_mesh
        // lesson". Only the validator catches it, and only the validator matches what
        // `create_shader_module` does at runtime.
        let src = build_material_bake_wgsl(stub_generated);
        let module = naga::front::wgsl::parse_str(&src)
            .unwrap_or_else(|e| panic!("[NAGA-PARSE] material_bake:
{}", e.emit_to_string(&src)));
        let mut v = naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        );
        if let Err(e) = v.validate(&module) {
            let path = std::env::temp_dir().join("azra_material_bake_fail.wgsl");
            let _ = std::fs::write(&path, &src);
            panic!("[NAGA-VALIDATE] material_bake: {:?}
Shader dumped to {:?}", e, path);
        }
        eprintln!("[NAGA] material_bake validated OK");
        for src in [
            build_material_graph_wgsl(stub_generated, include_str!("material_resident.wgsl")),
            build_material_resize_wgsl(),
            include_str!("poly_curvature_cache.wgsl").to_owned(),
        ] {
            let module = naga::front::wgsl::parse_str(&src)
                .unwrap_or_else(|error| panic!("{}", error.emit_to_string(&src)));
            v.validate(&module).expect("resident material shader validation");
        }
    }

    #[test]
    fn validate_probe_shader() {
        // Auto reflection probe capture + mip reduce: parse + VALIDATE (the voxel_mesh
        // lesson — parse-only lets a validation error crash at runtime).
        let src = crate::PROBE_WGSL;
        assert!(src.contains("fn probe_capture("), "probe_capture entry missing");
        // Cross-face (seamless cube-sampled) mip reduce — replaced the per-face 2D
        // box reduce whose per-face mip drift painted seam lines (2026-08-07).
        assert!(src.contains("fn probe_mip_reduce_cube("), "probe_mip_reduce_cube entry missing");
        assert!(src.contains("fn probe_sh_project("), "probe_sh_project entry missing");
        let module = naga::front::wgsl::parse_str(src)
            .unwrap_or_else(|e| panic!("[NAGA-PARSE] probe:\n{}", e.emit_to_string(src)));
        let mut v = naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        );
        if let Err(e) = v.validate(&module) {
            panic!("[NAGA-VALIDATE] probe: {:?}", e);
        }
        eprintln!("[NAGA] probe validated OK");
    }

    #[test]
    fn validate_env_prefilter_shader() {
        // Env GGX prefilter (split-sum IBL): parse + validate (create_shader_module validates,
        // parse-only would let a validation error crash at runtime — the voxel_mesh lesson).
        let module = naga::front::wgsl::parse_str(crate::ENV_PREFILTER_WGSL)
            .unwrap_or_else(|e| panic!("[NAGA-PARSE] env_prefilter:\n{}", e.emit_to_string(crate::ENV_PREFILTER_WGSL)));
        let mut v = naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        );
        if let Err(e) = v.validate(&module) {
            panic!("[NAGA-VALIDATE] env_prefilter: {:?}", e);
        }
        eprintln!("[NAGA] env_prefilter validated OK");
    }

    #[test]
    fn validate_instance_cull_shader() {
        // Phase D1 GPU instance culling: parse + VALIDATE (create_shader_module validates —
        // parse-only lets a validation error crash at runtime, the voxel_mesh lesson).
        let src = crate::INSTANCE_CULL_WGSL;
        assert!(src.contains("fn cs_reset_args("), "cs_reset_args entry missing");
        assert!(src.contains("fn cs_frustum_cull("), "cs_frustum_cull entry missing");
        let module = naga::front::wgsl::parse_str(src)
            .unwrap_or_else(|e| panic!("[NAGA-PARSE] instance_cull:\n{}", e.emit_to_string(src)));
        let mut v = naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        );
        if let Err(e) = v.validate(&module) {
            panic!("[NAGA-VALIDATE] instance_cull: {:?}", e);
        }
        eprintln!("[NAGA] instance_cull validated OK");
    }

    #[test]
    fn validate_cluster_cut_shader() {
        // Phase D4 cluster cut: parse + VALIDATE (the voxel_mesh lesson).
        let src = crate::CLUSTER_CUT_WGSL;
        assert!(src.contains("fn cs_cluster_reset("), "cs_cluster_reset missing");
        assert!(src.contains("fn cs_cluster_cut("), "cs_cluster_cut missing");
        let module = naga::front::wgsl::parse_str(src)
            .unwrap_or_else(|e| panic!("[NAGA-PARSE] cluster_cut:\n{}", e.emit_to_string(src)));
        let mut v = naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        );
        if let Err(e) = v.validate(&module) {
            panic!("[NAGA-VALIDATE] cluster_cut: {:?}", e);
        }
        eprintln!("[NAGA] cluster_cut validated OK");
    }

    #[test]
    fn validate_hzb_build_shader() {
        // Phase D2 HZB pyramid build: parse + VALIDATE (the voxel_mesh lesson).
        let src = crate::HZB_BUILD_WGSL;
        assert!(src.contains("fn cs_hzb_seed("), "cs_hzb_seed entry missing");
        assert!(src.contains("fn cs_hzb_downsample("), "cs_hzb_downsample entry missing");
        let module = naga::front::wgsl::parse_str(src)
            .unwrap_or_else(|e| panic!("[NAGA-PARSE] hzb_build:\n{}", e.emit_to_string(src)));
        let mut v = naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        );
        if let Err(e) = v.validate(&module) {
            panic!("[NAGA-VALIDATE] hzb_build: {:?}", e);
        }
        eprintln!("[NAGA] hzb_build validated OK");
    }

    #[test]
    fn validate_df_full_shader() {
        let src = build_deferred_wgsl(None);
        eprintln!("[TEST] DF-full: {} lines", src.lines().count());
        naga_parse("df_full", &src);
    }

    #[test]
    fn validate_voxel_mesh_shader() {
        // GPU Surface Nets (sculpt Phase 1b) — two compute entry points (sn_vertex, sn_quads).
        // NOTE: parse_str only PARSES; create_shader_module also VALIDATES, so run the validator
        // here too (parse-only let a validation error through and crashed the app at runtime).
        eprintln!("[TEST] voxel_mesh: {} lines", VOXEL_MESH_WGSL.lines().count());
        let module = naga::front::wgsl::parse_str(VOXEL_MESH_WGSL)
            .unwrap_or_else(|e| panic!("[NAGA-PARSE] voxel_mesh:\n{}", e.emit_to_string(VOXEL_MESH_WGSL)));
        let mut v = naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        );
        if let Err(e) = v.validate(&module) {
            panic!("[NAGA-VALIDATE] voxel_mesh: {:?}", e);
        }
        eprintln!("[NAGA] voxel_mesh validated OK");
    }

    #[test]
    fn validate_sdf_mesh_shader() {
        // GPU QEF Dual Contouring (Phase G, shared mesher) — two compute entry points
        // (dc_vertex, dc_quads). Parse + validate (parse-only misses validation errors).
        eprintln!("[TEST] sdf_mesh: {} lines", SDF_MESH_WGSL.lines().count());
        let module = naga::front::wgsl::parse_str(SDF_MESH_WGSL)
            .unwrap_or_else(|e| panic!("[NAGA-PARSE] sdf_mesh:\n{}", e.emit_to_string(SDF_MESH_WGSL)));
        let mut v = naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        );
        if let Err(e) = v.validate(&module) {
            panic!("[NAGA-VALIDATE] sdf_mesh: {:?}", e);
        }
        eprintln!("[NAGA] sdf_mesh validated OK");
    }

    #[test]
    fn validate_voxel_brush_shader() {
        // GPU brush (sculpt Phase 2) — parse + validate (parse-only misses validation errors).
        eprintln!("[TEST] voxel_brush: {} lines", VOXEL_BRUSH_WGSL.lines().count());
        let module = naga::front::wgsl::parse_str(VOXEL_BRUSH_WGSL)
            .unwrap_or_else(|e| panic!("[NAGA-PARSE] voxel_brush:\n{}", e.emit_to_string(VOXEL_BRUSH_WGSL)));
        let mut v = naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        );
        if let Err(e) = v.validate(&module) {
            panic!("[NAGA-VALIDATE] voxel_brush: {:?}", e);
        }
        eprintln!("[NAGA] voxel_brush validated OK");
    }

    #[test]
    fn validate_voxel_brush_gather_shader() {
        // GPU-resident brush (Phase 5a) — atlas->box gather. Parse + validate (parse-only misses errors).
        let module = naga::front::wgsl::parse_str(VOXEL_BRUSH_GATHER_WGSL)
            .unwrap_or_else(|e| panic!("[NAGA-PARSE] voxel_brush_gather:\n{}", e.emit_to_string(VOXEL_BRUSH_GATHER_WGSL)));
        let mut v = naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        );
        if let Err(e) = v.validate(&module) {
            panic!("[NAGA-VALIDATE] voxel_brush_gather: {:?}", e);
        }
        eprintln!("[NAGA] voxel_brush_gather validated OK");
    }

    #[test]
    fn validate_voxel_brush_scatter_shader() {
        // GPU-resident brush (Phase 5a) — box->atlas scatter. Parse + validate.
        let module = naga::front::wgsl::parse_str(VOXEL_BRUSH_SCATTER_WGSL)
            .unwrap_or_else(|e| panic!("[NAGA-PARSE] voxel_brush_scatter:\n{}", e.emit_to_string(VOXEL_BRUSH_SCATTER_WGSL)));
        let mut v = naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        );
        if let Err(e) = v.validate(&module) {
            panic!("[NAGA-VALIDATE] voxel_brush_scatter: {:?}", e);
        }
        eprintln!("[NAGA] voxel_brush_scatter validated OK");
    }

    #[test]
    fn validate_df_lite_shader() {
        let src = build_deferred_lite_wgsl();
        eprintln!("[TEST] DF-lite: {} lines", src.lines().count());
        naga_parse("df_lite", &src);
    }

    #[test]
    fn validate_pt_tri_shader() {
        // Triangle PT (Phase 2 — the production PT). Full validate: this shader
        // dispatches against real scene buffers; a validation error here is a
        // guaranteed runtime device-loss (the Phase 0 lesson).
        let src = build_pathtrace_tri_wgsl();
        eprintln!("[TEST] PT-tri: {} lines", src.lines().count());
        let module = naga::front::wgsl::parse_str(&src).unwrap_or_else(|e| {
            let path = std::env::temp_dir().join("azra_pt_tri_fail.wgsl");
            let _ = std::fs::write(&path, &src);
            panic!("[NAGA-PARSE] pt_tri:\n{}\nShader dumped to {:?}", e.emit_to_string(&src), path)
        });
        let mut v = naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        );
        if let Err(e) = v.validate(&module) {
            let path = std::env::temp_dir().join("azra_pt_tri_fail.wgsl");
            let _ = std::fs::write(&path, &src);
            panic!("[NAGA-VALIDATE] pt_tri: {:?}\nShader dumped to {:?}", e, path);
        }
        eprintln!("[NAGA] pt_tri validated OK");
    }

    #[test]
    fn validate_rt_shadow_shader() {
        // RT projected shadows (RT_SHADOWS_DESIGN Stage 1). Full validate: this shader
        // dispatches against the live PT-scene buffers + G-buffer; a validation error here
        // is a guaranteed runtime device-loss (the PT-tri lesson). Also asserts the shared
        // PT_TRAVERSAL region extracted cleanly (pt_occluded present, entry present).
        let src = build_rt_shadow_wgsl();
        eprintln!("[TEST] RT-shadow: {} lines", src.lines().count());
        assert!(src.contains("fn pt_occluded("), "PT_TRAVERSAL region extraction failed (no pt_occluded)");
        assert!(src.contains("fn rt_shadow("), "rt_shadow entry missing");
        assert!(src.contains("fn num_lights("), "LIGHTING region extraction failed");
        let module = naga::front::wgsl::parse_str(&src).unwrap_or_else(|e| {
            let path = std::env::temp_dir().join("azra_rt_shadow_fail.wgsl");
            let _ = std::fs::write(&path, &src);
            panic!("[NAGA-PARSE] rt_shadow:\n{}\nShader dumped to {:?}", e.emit_to_string(&src), path)
        });
        let mut v = naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        );
        if let Err(e) = v.validate(&module) {
            let path = std::env::temp_dir().join("azra_rt_shadow_fail.wgsl");
            let _ = std::fs::write(&path, &src);
            panic!("[NAGA-VALIDATE] rt_shadow: {:?}\nShader dumped to {:?}", e, path);
        }
        eprintln!("[NAGA] rt_shadow validated OK");
    }

    #[test]
    fn validate_ssgi_shader() {
        // Screen-space GI (SSGI_PLAN.md). Full parse+validate, not parse-only:
        // parse_str accepts a call with the wrong argument count, so a broken
        // extraction can look clean (the voxel_mesh lesson, see the note above
        // validate_node_material_shader).
        //
        // The contains() asserts are the load-bearing part. A region-tag slip
        // yields an EMPTY extraction, and because SSGI's delta is defined as a
        // cancellation against what deferred applied, silently losing the env or
        // AO terms would still COMPILE and still produce a plausible-looking
        // image — it would just quietly stop conserving energy. That is exactly
        // the failure class a GPU-only test cannot see.
        let src = build_ssgi_wgsl();
        eprintln!("[TEST] SSGI: {} lines", src.lines().count());
        // ENV region (sdf_lighting.wgsl)
        assert!(src.contains("fn env_sh_irradiance("), "ENV region extraction failed");
        assert!(src.contains("fn sample_env_raw("), "ENV region extraction failed (sample_env_raw)");
        // AO_UPSAMPLE region (deferred_entry.wgsl)
        assert!(src.contains("fn ssao_factor("), "AO_UPSAMPLE region extraction failed");
        assert!(src.contains("fn ao_bent_normal("), "AO_UPSAMPLE region extraction failed (bent normal)");
        assert!(src.contains("fn multi_bounce_ao("), "AO_UPSAMPLE region extraction failed (multi_bounce)");
        // Bindings the extracted code needs but its home files declare outside regions.
        assert!(src.contains("var<storage, read> env_data"), "env_data(21) not declared inline");
        assert!(src.contains("var ssao_in"), "ssao_in(38) not declared inline");
        // Entry points.
        assert!(src.contains("fn ssgi_gather("), "ssgi entry missing");
        assert!(src.contains("fn ssgi_blur_h("), "ssgi blur_h missing");
        assert!(src.contains("fn ssgi_blur_v("), "ssgi blur_v missing");
        let module = naga::front::wgsl::parse_str(&src).unwrap_or_else(|e| {
            let path = std::env::temp_dir().join("azra_ssgi_fail.wgsl");
            let _ = std::fs::write(&path, &src);
            panic!("[NAGA-PARSE] ssgi:\n{}\nShader dumped to {:?}", e.emit_to_string(&src), path)
        });
        let mut v = naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        );
        if let Err(e) = v.validate(&module) {
            let path = std::env::temp_dir().join("azra_ssgi_fail.wgsl");
            let _ = std::fs::write(&path, &src);
            panic!("[NAGA-VALIDATE] ssgi: {:?}\nShader dumped to {:?}", e, path);
        }
        eprintln!("[NAGA] ssgi validated OK");
    }

    /// The AO_UPSAMPLE region tag added to `deferred_entry.wgsl` for SSGI must be
    /// EXTRACTION-only, exactly like ENV_BRDF: if it ever lands in an exclude
    /// list, the deferred/fog/prim-outline builds silently lose `ssao_factor` and
    /// friends. Cheap to assert, and the failure would otherwise surface as a
    /// runtime device loss in an unrelated pass.
    #[test]
    fn ao_upsample_region_is_extraction_only() {
        for (name, src) in [
            ("deferred", build_deferred_wgsl(None)),
            ("deferred_lite", build_deferred_lite_wgsl()),
        ] {
            assert!(
                src.contains("fn ssao_factor("),
                "{name}: AO_UPSAMPLE was stripped — it must never appear in an exclude list"
            );
            assert!(src.contains("fn multi_bounce_ao("), "{name}: multi_bounce_ao stripped");
        }
    }

    #[test]
    fn validate_rt_shadow_denoise_shaders() {
        // RT projected-shadow denoise passes (RT_SHADOWS_DESIGN Stage 2). Both dispatch against
        // live full-res textures — a validation error is a runtime device-loss. Validate both.
        for (name, src) in [
            ("rt_shadow_denoise", build_rt_shadow_denoise_wgsl()),
            ("rt_shadow_temporal", build_rt_shadow_temporal_wgsl()),
        ] {
            let module = naga::front::wgsl::parse_str(&src).unwrap_or_else(|e| {
                let path = std::env::temp_dir().join(format!("azra_{name}_fail.wgsl"));
                let _ = std::fs::write(&path, &src);
                panic!("[NAGA-PARSE] {name}:\n{}\nShader dumped to {:?}", e.emit_to_string(&src), path)
            });
            let mut v = naga::valid::Validator::new(
                naga::valid::ValidationFlags::all(),
                naga::valid::Capabilities::all(),
            );
            if let Err(e) = v.validate(&module) {
                let path = std::env::temp_dir().join(format!("azra_{name}_fail.wgsl"));
                let _ = std::fs::write(&path, &src);
                panic!("[NAGA-VALIDATE] {name}: {:?}\nShader dumped to {:?}", e, path);
            }
            eprintln!("[NAGA] {name} validated OK");
        }
    }

    #[test]
    fn validate_pt_reproject_shader() {
        // PT temporal reprojection (P3.5). Full validate — dispatches against
        // live textures every camera-move frame in PT mode.
        let src = crate::PT_REPROJECT_WGSL;
        let module = naga::front::wgsl::parse_str(src).unwrap_or_else(|e| {
            panic!("[NAGA-PARSE] pt_reproject:\n{}", e.emit_to_string(src))
        });
        let mut v = naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        );
        if let Err(e) = v.validate(&module) {
            panic!("[NAGA-VALIDATE] pt_reproject: {:?}", e);
        }
        eprintln!("[NAGA] pt_reproject validated OK");
    }

    #[test]
    fn validate_pt_restir_reproject_shader() {
        // Section I: ReSTIR reservoir warp under camera motion.
        let src = crate::PT_RESTIR_REPROJECT_WGSL;
        let module = naga::front::wgsl::parse_str(src).unwrap_or_else(|e| {
            panic!("[NAGA-PARSE] pt_restir_reproject:\n{}", e.emit_to_string(src))
        });
        let mut v = naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        );
        if let Err(e) = v.validate(&module) {
            panic!("[NAGA-VALIDATE] pt_restir_reproject: {:?}", e);
        }
        eprintln!("[NAGA] pt_restir_reproject validated OK");
    }

    #[test]
    fn validate_pt_denoise_shader() {
        // Section J / P3.6 SVGF-lite denoise (albedo + history + à-trous).
        let src = crate::PT_DENOISE_WGSL;
        let module = naga::front::wgsl::parse_str(src).unwrap_or_else(|e| {
            panic!("[NAGA-PARSE] pt_denoise:\n{}", e.emit_to_string(src))
        });
        let mut v = naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        );
        if let Err(e) = v.validate(&module) {
            panic!("[NAGA-VALIDATE] pt_denoise: {:?}", e);
        }
        eprintln!("[NAGA] pt_denoise validated OK");
    }

    #[test]
    fn validate_pt_upscale_shader() {
        let src = crate::PT_UPSCALE_WGSL;
        let module = naga::front::wgsl::parse_str(src)
            .unwrap_or_else(|e| panic!("[NAGA-PARSE] pt_upscale:\n{}", e.emit_to_string(src)));
        let mut v = naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        );
        if let Err(e) = v.validate(&module) {
            panic!("[NAGA-VALIDATE] pt_upscale: {:?}", e);
        }
        eprintln!("[NAGA] pt_upscale validated OK");
    }

    // (validate_pt_shader REMOVED 2026-07-16 — the analytical PT entry it validated
    //  is deleted; validate_pt_tri_shader above covers the production triangle PT.)

    // (validate_raymarch_shader / validate_raymarch_march_shader REMOVED 2026-07-14 —
    //  the WGSL analytical march display path is stripped.)

    #[test]
    fn validate_prim_outline_shader() {
        let src = build_prim_outline_wgsl();
        eprintln!("[TEST] Prim-outline: {} lines", src.lines().count());
        naga_parse("prim_outline", &src);
    }

    #[test]
    fn validate_fog_shader() {
        let src = build_fog_wgsl(None);
        eprintln!("[TEST] Fog: {} lines", src.lines().count());
        naga_parse("fog", &src);
        assert!(src.contains("fn pass_fog("), "full fog entry missing");
        assert!(src.contains("fn pass_fog_lite("), "P2 analytic Lite entry missing");
        assert!(src.contains("fn fog_optical_depth_height("), "Lite optical-depth helper missing");
        assert!(src.contains("fn fog_shadow_depth("), "P3 depth shadow missing");
        assert!(src.contains("fn fog_shadow_sdf("), "P3 SDF shadow missing");
        assert!(src.contains("fog_has_vol_lights"), "P3 vol CPU flag missing");
    }

    #[test]
    fn validate_blend_noise_neutralization() {
        // Verify sdf_shared.wgsl path
        let shared = super::build_shared_wgsl(None);
        assert!(!shared.contains("fn smin_noisy("), "smin_noisy must be removed from shared WGSL");
        assert!(!shared.contains("fn smax_noisy("), "smax_noisy must be removed from shared WGSL");
        assert!(!shared.contains("fn eval_blend_noise("), "eval_blend_noise must be removed from shared WGSL");
        assert!(shared.contains("fn apply_boolean_blend("), "apply_boolean_blend stub must be present");
        assert!(shared.contains("fn apply_boolean("), "apply_boolean must be present");
        assert!(shared.contains("fn fbm_wgsl("), "fbm_wgsl must be kept (used by surface modifiers)");
        // Verify the stub delegates to apply_boolean / apply_boolean_lut (curve path)
        let blend_pos = shared.find("fn apply_boolean_blend(").unwrap();
        let after_blend = &shared[blend_pos..blend_pos+700.min(shared.len()-blend_pos)];
        assert!(
            after_blend.contains("return apply_boolean(") || after_blend.contains("return apply_boolean_lut("),
            "lightweight blend must call apply_boolean or apply_boolean_lut"
        );
        assert!(
            after_blend.contains("bn_hash31") || after_blend.contains("amp"),
            "lightweight blend should honor amplitude"
        );
        eprintln!("[TEST] Blend noise neutralization verified in shared WGSL");

        // Verify volume_bake.wgsl path
        let bake = super::neutralize_blend_noise_wgsl(super::VOLUME_BAKE_WGSL);
        assert!(!bake.contains("fn smin_noisy("), "smin_noisy must be removed from bake WGSL");
        assert!(!bake.contains("fn eval_blend_noise("), "eval_blend_noise must be removed from bake WGSL");
        assert!(bake.contains("fn apply_boolean_blend("), "bake must have blend stub");
        assert!(bake.contains("fn fbm_wgsl("), "bake must keep fbm_wgsl (used by surface modifiers)");
        // Parse + VALIDATE (S2.2): parse-only misses binding/type errors that
        // create_shader_module rejects at runtime (the voxel_mesh precedent). Covers the
        // SparseBricks output branch (brick_index @binding(5), pool arena write).
        let module = naga::front::wgsl::parse_str(&bake)
            .unwrap_or_else(|e| panic!("[NAGA-PARSE] bake_neutralized:\n{}", e.emit_to_string(&bake)));
        let mut v = naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        );
        if let Err(e) = v.validate(&module) {
            panic!("[NAGA-VALIDATE] bake_neutralized: {:?}", e);
        }
        eprintln!("[NAGA] bake_neutralized validated OK");
        eprintln!("[TEST] Blend noise neutralization verified in bake WGSL");
    }

    #[test]
    fn validate_brick_fill_shader() {
        let src = super::build_brick_fill_wgsl();
        eprintln!("[TEST] BrickFill: {} lines", src.lines().count());
        naga_parse("brick_fill", &src);
    }

    #[test]
    fn validate_sculpt_voxel_raymarch_shader() {
        let src = super::build_sculpt_voxel_raymarch_wgsl();
        eprintln!("[TEST] SculptVoxelRaymarch: {} lines", src.lines().count());
        naga_parse("sculpt_voxel_raymarch", &src);
    }

    #[test]
    fn validate_post_process_shader() {
        // Post-process module (tonemap/bloom/SSR/upsample entries). Parse + VALIDATE
        // both LDR format variants — pass_ssr now pulls the extracted ENV/ENV_BRDF
        // regions, so a region-tag slip here would be a runtime module rejection.
        for fmt in ["rgba16float", "rgba8unorm"] {
            let src = process_post_process_wgsl(fmt);
            let module = naga::front::wgsl::parse_str(&src).unwrap_or_else(|e| {
                let path = std::env::temp_dir().join("azra_post_process_fail.wgsl");
                let _ = std::fs::write(&path, &src);
                panic!("[NAGA-PARSE] post_process({}):\n{}\nShader dumped to {:?}", fmt, e.emit_to_string(&src), path)
            });
            let mut v = naga::valid::Validator::new(
                naga::valid::ValidationFlags::all(),
                naga::valid::Capabilities::all(),
            );
            if let Err(e) = v.validate(&module) {
                let path = std::env::temp_dir().join("azra_post_process_fail.wgsl");
                let _ = std::fs::write(&path, &src);
                panic!("[NAGA-VALIDATE] post_process({}): {:?}\nShader dumped to {:?}", fmt, e, path);
            }
            // The SSR delta-composite contract depends on extractions landing:
            assert!(src.contains("fn env_brdf("), "ENV_BRDF region extraction missing");
            assert!(src.contains("fn sample_env_mip("), "ENV region extraction missing");
            // C1: specular occlusion uses the same AO_UPSAMPLE helpers as deferred.
            assert!(src.contains("fn ssao_factor("), "AO_UPSAMPLE region extraction missing");
            assert!(src.contains("fn specular_occlusion("), "specular_occlusion missing from AO_UPSAMPLE");
            // Layout guard: this module keeps its own copy of GpuUniforms — a missing
            // field block makes every later field silently read the wrong bytes (the
            // 2026-07-18 SSR bug: the copy skipped the LOD/atlas blocks, so
            // ssr_intensity read foveated_enabled == 0 and SSR output was all-zero).
            // The WGSL struct span must equal the Rust struct size exactly.
            let rust_size = std::mem::size_of::<crate::GpuUniforms>() as u32;
            let mut checked = false;
            for (_h, ty) in module.types.iter() {
                if ty.name.as_deref() == Some("GpuUniforms") {
                    if let naga::TypeInner::Struct { span, .. } = ty.inner {
                        assert_eq!(
                            span, rust_size,
                            "post_process GpuUniforms copy is {} bytes but Rust is {} — a field block is missing/extra",
                            span, rust_size
                        );
                        checked = true;
                    }
                }
            }
            assert!(checked, "GpuUniforms struct not found in post_process module");
            eprintln!("[NAGA] post_process({}) validated OK (uniforms span {} bytes)", fmt, rust_size);
        }
    }

    #[test]
    fn validate_composite_blend_shader() {
        let src = include_str!("composite_blend.wgsl");
        eprintln!("[TEST] CompositeBlend: {} lines", src.lines().count());
        naga_parse("composite_blend", src);
    }

    #[test]
    fn measure_assembled_shader_sizes() {
        let shaders: Vec<(&str, String)> = vec![
            ("RM-lite",  build_raymarch_lite_wgsl()),
            ("DF-full",  build_deferred_wgsl(None)),
            ("DF-lite",  build_deferred_lite_wgsl()),
            ("Fog",      build_fog_wgsl(None)),
            // The production PT pipelines compile the static specializations; the
            // unspecialized source carries `override`s that SPIR-V output cannot size.
            ("PT-full",  build_pathtrace_tri_wgsl_with_mode(Some(false))),
            ("PT-inter", build_pathtrace_tri_wgsl_with_mode(Some(true))),
        ];
        eprintln!("\n{:12} {:>6} {:>8}  {:>10}  scene_sdf  scene_analytical", "Shader", "Lines", "WGSL KB", "SPIR-V KB");
        eprintln!("{:-<80}", "");
        for (name, src) in &shaders {
            let lines = src.lines().count();
            let wgsl_kb = src.len() as f64 / 1024.0;
            let sdf_calls = src.matches("scene_sdf(").count();
            let analytical = src.matches("scene_analytical(").count();
            // Measure Naga SPIR-V output size (what the GPU driver actually compiles)
            let spv_kb = match naga::front::wgsl::parse_str(src) {
                Ok(module) => {
                    let info = naga::valid::Validator::new(
                        naga::valid::ValidationFlags::empty(),
                        naga::valid::Capabilities::all(),
                    ).validate(&module).unwrap();
                    let spv = naga::back::spv::write_vec(
                        &module, &info,
                        &naga::back::spv::Options {
                            lang_version: (1, 0),
                            ..Default::default()
                        },
                        Some(&naga::back::spv::PipelineOptions {
                            shader_stage: naga::ShaderStage::Compute,
                            entry_point: module.entry_points[0].name.clone(),
                        }),
                    ).unwrap();
                    (spv.len() * 4) as f64 / 1024.0 // SPIR-V words → bytes → KB
                }
                Err(_) => 0.0,
            };
            eprintln!("{:12} {:>6} {:>8.1}  {:>10.1}  {:>9}  {:>16}", name, lines, wgsl_kb, spv_kb, sdf_calls, analytical);
        }
        eprintln!();
    }
}
