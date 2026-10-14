//! Node evaluation (eval_node) + projection + noise primitives.
//!
//! Pure relocation from graph_eval — no logic change.
//! Socket/graph plumbing and SDF field helpers stay in parent `mod.rs`.

use super::{
    def_f, def_v, generator_sample_pos, input_val, is_input_connected, sn_field_h,
    sdf_ambient_occlusion, sdf_curvature_masks, sdf_curvature_masks_ex, sdf_direction_mask,
    sdf_position_01, sdf_thickness, sdf_to_mask, Val, SdfFieldSources,
};
use super::super::texture_cache::sample_map_rgb;
use super::super::{ImageMapSources, BAKE_MAX_OCTAVES};
use crate::material_textures::map_side_len;
use crate::shader_nodes::{
    apply_noise_blend_curve, MathBinaryOp, MathUnaryOp, Node, NodeGraph, NodeId, NodeType,
    NoiseType, ProjectionStyle, VectorMathOp,
};
use core_math::pbr_material::MapKind;
use std::collections::HashMap;


pub(super) fn eval_node(
    graph: &NodeGraph,
    node: &Node,
    p: [f32; 3],
    n: [f32; 3],
    hit_prim: f32,
    images: &ImageMapSources,
    sdf: &SdfFieldSources,
    max_noise_octaves: u32,
    outs: &mut HashMap<(NodeId, usize), Val>,
) {
    let id = node.id;
    // Read all inputs first (immutable), then write outputs — avoids E0502 dual borrow.
    macro_rules! f_in {
        ($idx:expr, $fb:expr) => {
            input_val(graph, id, $idx, Val::F(def_f(node, $idx, $fb)), outs).as_f()
        };
    }
    macro_rules! v_in {
        ($idx:expr, $fb:expr) => {
            input_val(graph, id, $idx, Val::V(def_v(node, $idx, $fb)), outs).as_v()
        };
    }

    // MUTED = BYPASS (Blender `M`): every output is INPUT 0 passed through, so the chain
    // stays alive and downstream nodes read the un-modified upstream value. This must live
    // HERE, in the evaluator — a mute that only dims the drawing lies about what renders.
    // (`extract_live_gpu_noise` bails to this CPU path when any node is muted — both-paths rule.)
    if node.muted && node.node_type != NodeType::MaterialOutput {
        let v = input_val(graph, id, 0, Val::F(def_f(node, 0, 0.0)), outs);
        for out_idx in 0..4usize {
            outs.insert((id, out_idx), v.clone());
        }
        return;
    }

    // G8 structure nodes. Reroute = raw input-0 passthrough on every output (the same
    // contract as mute — colours survive because the RAW upstream Val is forwarded).
    // Frame has no sockets so it can never be in `order`; the arm keeps the match total.
    if matches!(node.node_type, NodeType::Reroute) {
        let v = input_val(graph, id, 0, Val::F(def_f(node, 0, 0.0)), outs);
        for out_idx in 0..4usize {
            outs.insert((id, out_idx), v.clone());
        }
        return;
    }
    if matches!(node.node_type, NodeType::Frame) {
        return;
    }

    let produced: Vec<(usize, Val)> = match node.node_type {
        // Handled by the early returns above; arms keep the match total.
        NodeType::Reroute | NodeType::Frame => vec![],
        NodeType::TexCoord => vec![(0, Val::V(p)), (1, Val::V(n))],
        NodeType::ObjectInfo => vec![(0, Val::F(hit_prim))],
        NodeType::Noise => {
            // Projection is baked into the node (default Triplanar) — no separate projection plug required.
            // **CPU** (not GPU): cost ≈ pixels × octaves × Perlin.
            let pos = generator_sample_pos(graph, node, 0, p, outs);
            // Free-form UI values — only avoid zero scale and runaway octaves.
            let scale = {
                let s = f_in!(1, 4.0);
                if s.abs() < 1e-6 {
                    1e-6_f32.copysign(if s < 0.0 { -1.0 } else { 1.0 })
                } else {
                    s
                }
            };
            let oct = (f_in!(2, 4.0).round() as i32)
                .max(1) as u32;
            let oct = oct.min(BAKE_MAX_OCTAVES).min(max_noise_octaves.max(1));
            let lac = f_in!(3, 2.0);
            let lac = if lac.abs() < 1e-4 { 2.0 } else { lac };
            let gain = f_in!(4, 0.5);
            let distortion = f_in!(5, 0.0);
            let seed = f_in!(6, 0.0);
            let sample = |dom: [f32; 3]| {
                let sp = [
                    dom[0] + seed * 1.0,
                    dom[1] + seed * 0.6180339887,
                    dom[2] + seed * 1.7320508075,
                ];
                eval_noise(
                    node.noise_type,
                    sp,
                    scale,
                    oct,
                    lac,
                    gain,
                    distortion,
                    node.noise_tile,
                )
            };
            let raw = sample_projected(node, pos, n, sample);
            // Advanced: levels / smooth / power reshape of the 0..1 noise.
            let val = apply_noise_blend_curve(
                raw,
                node.noise_curve,
                node.noise_black,
                node.noise_white,
                node.noise_curve_k,
            );
            vec![(0, Val::F(val))]
        }
        NodeType::Voronoi => {
            let pos = generator_sample_pos(graph, node, 0, p, outs);
            let scale = f_in!(1, 5.0);
            let rnd = f_in!(2, 1.0).clamp(0.0, 1.0);
            let sample = |dom: [f32; 3]| {
                voronoi_f1([dom[0] * scale, dom[1] * scale, dom[2] * scale], rnd)
            };
            let (d, cell, col) = sample_projected_triple(node, pos, n, sample);
            vec![(0, Val::F(d)), (1, Val::F(cell)), (2, Val::V(col))]
        }
        NodeType::MathBinary => {
            let a = f_in!(0, 0.0);
            let b = f_in!(1, 0.0);
            let r = match node.math_binary_op {
                MathBinaryOp::Add => a + b,
                MathBinaryOp::Subtract => a - b,
                MathBinaryOp::Multiply => a * b,
                MathBinaryOp::Divide => if b.abs() < 1e-8 { 0.0 } else { a / b },
                MathBinaryOp::Power => a.abs().powf(b),
                MathBinaryOp::Min => a.min(b),
                MathBinaryOp::Max => a.max(b),
                MathBinaryOp::Modulo => if b.abs() < 1e-8 { 0.0 } else { a % b },
                MathBinaryOp::Atan2 => a.atan2(b),
                MathBinaryOp::Compare => if a > b { 1.0 } else { 0.0 },
                MathBinaryOp::Step => if b < a { 0.0 } else { 1.0 }, // WGSL step(edge,x): edge=a, x=b
            };
            vec![(0, Val::F(r))]
        }
        NodeType::MathUnary => {
            let v = f_in!(0, 0.0);
            let r = match node.math_unary_op {
                MathUnaryOp::Abs => v.abs(),
                MathUnaryOp::Negate => -v,
                MathUnaryOp::Sqrt => v.abs().sqrt(),
                MathUnaryOp::Sin => v.sin(),
                MathUnaryOp::Cos => v.cos(),
                MathUnaryOp::Fract => v.fract(),
                MathUnaryOp::Floor => v.floor(),
                MathUnaryOp::Ceil => v.ceil(),
                MathUnaryOp::Saturate => v.clamp(0.0, 1.0),
                MathUnaryOp::OneMinus => 1.0 - v,
                MathUnaryOp::Sign => v.signum(),
                MathUnaryOp::Exp => v.exp(),
                MathUnaryOp::Log => v.max(1e-6).ln(),
                MathUnaryOp::Round => v.round(),
                MathUnaryOp::Truncate => v.trunc(),
            };
            vec![(0, Val::F(r))]
        }
        NodeType::Remap => {
            let v = f_in!(0, 0.0);
            let from_min = f_in!(1, 0.0);
            let from_max = f_in!(2, 1.0);
            let to_min = f_in!(3, 0.0);
            let to_max = f_in!(4, 1.0);
            let t = ((v - from_min) / (from_max - from_min).max(1e-4)).clamp(0.0, 1.0);
            vec![(0, Val::F(to_min + (to_max - to_min) * t))]
        }
        NodeType::ColorRamp => {
            let fac = f_in!(0, 0.5);
            let p0 = node.ramp_pos0;
            let p1 = node.ramp_pos1;
            let t = ((fac - p0) / (p1 - p0).max(1e-4)).clamp(0.0, 1.0);
            let c0 = node.ramp_color0;
            let c1 = node.ramp_color1;
            vec![(
                0,
                Val::V([
                    c0[0] + (c1[0] - c0[0]) * t,
                    c0[1] + (c1[1] - c0[1]) * t,
                    c0[2] + (c1[2] - c0[2]) * t,
                ]),
            )]
        }
        NodeType::MixRgb => {
            use crate::shader_nodes::mix_blend_over;
            let fac = f_in!(0, 0.5).clamp(0.0, 1.0);
            let c1 = v_in!(1, [0.5, 0.5, 0.5]);
            let c2 = v_in!(2, [1.0, 1.0, 1.0]);
            // Photoshop over: result = mix(base, op(base, layer), fac).
            // MUST match the WGSL MixRgb codegen in shader_nodes/graph.rs
            // (`mix_blend_channel` is the single formula source).
            let out = mix_blend_over(node.mix_blend, c1, c2, fac);
            vec![(0, Val::V(out))]
        }
        NodeType::BrightnessContrast => {
            let col = v_in!(0, [0.5, 0.5, 0.5]);
            let bright = f_in!(1, 0.0);
            let contrast = f_in!(2, 0.0);
            let mut o = [0.0; 3];
            for i in 0..3 {
                o[i] = ((col[i] - 0.5) * (1.0 + contrast) + 0.5 + bright).clamp(0.0, 1.0);
            }
            vec![(0, Val::V(o))]
        }
        NodeType::SeparateXyz => {
            let v = v_in!(0, [0.0, 0.0, 0.0]);
            vec![(0, Val::F(v[0])), (1, Val::F(v[1])), (2, Val::F(v[2]))]
        }
        NodeType::CombineXyz => {
            vec![(0, Val::V([f_in!(0, 0.0), f_in!(1, 0.0), f_in!(2, 0.0)]))]
        }
        NodeType::Triplanar => {
            let pos = v_in!(0, p);
            let nrm = v_in!(1, n);
            let sharp = f_in!(2, 4.0);
            let scale = f_in!(3, 1.0);
            let w = [
                nrm[0].abs().powf(sharp),
                nrm[1].abs().powf(sharp),
                nrm[2].abs().powf(sharp),
            ];
            let s = (w[0] + w[1] + w[2]).max(1e-4);
            vec![
                (0, Val::V([w[0] / s, w[1] / s, w[2] / s])),
                (1, Val::V([pos[1] * scale, pos[2] * scale, 0.0])),
                (2, Val::V([pos[0] * scale, pos[2] * scale, 0.0])),
                (3, Val::V([pos[0] * scale, pos[1] * scale, 0.0])),
            ]
        }
        NodeType::PlanarMapping => {
            let pos = v_in!(0, p);
            let axis = f_in!(1, 0.0) as u32;
            let scale = f_in!(2, 1.0);
            let uv = match axis {
                1 => [pos[0] * scale, pos[2] * scale, 0.0],
                2 => [pos[1] * scale, pos[2] * scale, 0.0],
                _ => [pos[0] * scale, pos[1] * scale, 0.0],
            };
            vec![(0, Val::V(uv))]
        }
        NodeType::BoxMapping | NodeType::CylindricalMapping | NodeType::SphericalMapping => {
            let pos = v_in!(0, p);
            let scale = f_in!(1, 1.0);
            let mut v = vec![(0, Val::V([pos[0] * scale, pos[1] * scale, 0.0]))];
            if matches!(node.node_type, NodeType::BoxMapping) {
                v.push((1, Val::F(0.0)));
            }
            v
        }
        NodeType::UVTransform => {
            let uv = v_in!(0, [0.0, 0.0, 0.0]);
            let ox = f_in!(1, 0.0);
            let oy = f_in!(2, 0.0);
            let rot = f_in!(3, 0.0);
            let sx = f_in!(4, 1.0);
            let sy = f_in!(5, 1.0);
            let c = rot.cos();
            let s = rot.sin();
            let rx = uv[0] * c - uv[1] * s;
            let ry = uv[0] * s + uv[1] * c;
            vec![(0, Val::V([rx * sx + ox, ry * sy + oy, 0.0]))]
        }
        NodeType::TextureSample => {
            let pos = generator_sample_pos(graph, node, 0, p, outs);
            let pat = f_in!(1, 0.0) as u32;
            let sample = |uv: [f32; 3]| match pat {
                1 => [uv[0], uv[1], 0.0],
                2 => {
                    let bx = if (uv[0] * 5.0).fract() < 0.9
                        && (uv[1] * 5.0
                            + if (uv[1] * 2.5).fract() > 0.5 {
                                0.5
                            } else {
                                0.0
                            })
                        .fract()
                            < 0.9
                    {
                        1.0
                    } else {
                        0.0
                    };
                    [bx, bx, bx]
                }
                _ => {
                    let ch = (if (uv[0] * 2.0).fract() > 0.5 { 1.0 } else { 0.0 })
                        + (if (uv[1] * 2.0).fract() > 0.5 { 1.0 } else { 0.0 });
                    let c = if (ch as u32) % 2 == 0 { 0.0 } else { 1.0 };
                    [c, c, c]
                }
            };
            let col = sample_projected_vec3(node, pos, n, sample);
            vec![(0, Val::V(col)), (1, Val::F(1.0))]
        }
        NodeType::GradientTexture => {
            let pos = generator_sample_pos(graph, node, 0, p, outs);
            let mode = f_in!(1, 0.0) as u32;
            let sample = |uv: [f32; 3]| {
                match mode {
                    1 => {
                        let dx = uv[0] - 0.5;
                        let dy = uv[1] - 0.5;
                        (dx * dx + dy * dy).sqrt()
                    }
                    2 => (uv[0] * uv[0] + uv[1] * uv[1]).sqrt(),
                    _ => uv[0],
                }
                .clamp(0.0, 1.0)
            };
            let val = sample_projected(node, pos, n, sample);
            vec![(0, Val::F(val)), (1, Val::V([val, val, val]))]
        }
        NodeType::WaveTexture => {
            let pos = generator_sample_pos(graph, node, 0, p, outs);
            let scale = f_in!(1, 5.0);
            let dist = f_in!(2, 0.0);
            let prof = f_in!(3, 0.0) as u32;
            let sample = |uv: [f32; 3]| {
                let t = (uv[0] + uv[1]) * scale + dist * (uv[0] * std::f32::consts::PI).sin();
                match prof {
                    1 => t.fract(),
                    2 => (t.fract() * 2.0 - 1.0).abs(),
                    _ => (t * std::f32::consts::TAU).sin() * 0.5 + 0.5,
                }
            };
            let val = sample_projected(node, pos, n, sample);
            vec![(0, Val::F(val)), (1, Val::V([val, val, val]))]
        }
        NodeType::MusgraveTexture => {
            let pos = generator_sample_pos(graph, node, 0, p, outs);
            let scale = f_in!(1, 5.0);
            let detail = f_in!(2, 2.0).clamp(1.0, 8.0) as u32;
            let dim = f_in!(3, 2.0);
            let lac = f_in!(4, 2.0);
            let sample = |dom: [f32; 3]| {
                let mut freq = 1.0f32;
                let mut amp = 1.0f32;
                let mut sum = 0.0f32;
                for _ in 0..detail {
                    sum += fbm3(
                        [dom[0] * scale * freq, dom[1] * scale * freq, dom[2] * scale * freq],
                        1,
                    ) * amp;
                    amp *= freq.max(0.001).powf(-dim);
                    freq *= lac;
                }
                sum
            };
            let val = sample_projected(node, pos, n, sample);
            vec![(0, Val::F(val))]
        }
        NodeType::BrickTexture => {
            let pos = generator_sample_pos(graph, node, 0, p, outs);
            let scale = f_in!(1, 5.0);
            let mortar = f_in!(2, 0.02);
            let row_off = f_in!(3, 0.5);
            let sample = |uv: [f32; 3]| {
                let bv = [uv[0] * scale, uv[1] * scale];
                let row = bv[1].floor();
                let off = row_off * if (row * 0.5).fract() > 0.5 { 1.0 } else { 0.0 };
                let bx = (bv[0] + off).fract();
                let by = bv[1].fract();
                if bx > mortar && bx < 1.0 - mortar && by > mortar && by < 1.0 - mortar {
                    1.0
                } else {
                    0.0
                }
            };
            let mx = sample_projected(node, pos, n, sample);
            vec![
                (0, Val::V([mx, mx * 0.3, mx * 0.2])),
                (1, Val::F(mx)),
            ]
        }
        NodeType::Checker => {
            let pos = generator_sample_pos(graph, node, 0, p, outs);
            let c1 = v_in!(1, [0.8, 0.8, 0.8]);
            let c2 = v_in!(2, [0.2, 0.2, 0.2]);
            let scale = f_in!(3, 5.0);
            // Parity via integer floor sum — MUST match the WGSL arm bit-for-bit
            // (two's-complement & 1 handles negative cells the same on both sides).
            let sample = |dom: [f32; 3]| -> f32 {
                let sum = (dom[0] * scale).floor() as i64
                    + (dom[1] * scale).floor() as i64
                    + (dom[2] * scale).floor() as i64;
                (sum & 1) as f32
            };
            let fac = sample_projected(node, pos, n, sample);
            let col = [
                c1[0] * (1.0 - fac) + c2[0] * fac,
                c1[1] * (1.0 - fac) + c2[1] * fac,
                c1[2] * (1.0 - fac) + c2[2] * fac,
            ];
            vec![(0, Val::V(col)), (1, Val::F(fac))]
        }
        NodeType::Gamma => {
            let c = v_in!(0, [1.0, 1.0, 1.0]);
            let g = f_in!(1, 1.0).max(1e-4);
            vec![(0, Val::V([
                c[0].max(0.0).powf(g),
                c[1].max(0.0).powf(g),
                c[2].max(0.0).powf(g),
            ]))]
        }
        NodeType::MaterialOutput => vec![],
        NodeType::ConstantColor => {
            // Solid color stored on ramp_color0 (shared param panel field).
            vec![(0, Val::V(node.ramp_color0))]
        }
        NodeType::ImageTexture => {
            // Sample the material's map slot (same tex_id Properties / library import write).
            // Domain uses the node's built-in projection so Image Texture = Noise / Gradient.
            let pos = generator_sample_pos(graph, node, 0, p, outs);
            let tag = node.image_map_kind;
            // Pinned source slot (panel P3 layers) wins over the material's map
            // slot — the slot may hold the baked composite of this very graph.
            let pinned = if node.image_tex_id != 0 {
                images
                    .by_tex
                    .get(&node.image_tex_id)
                    .filter(|v| map_side_len(v).is_some())
            } else {
                None
            };
            // Color maps (albedo / emission / sss color — `!is_data()`) live as sRGB in the
            // array; Mult must multiply in linear (Photoshop). Data-map kinds (roughness
            // etc.) use the inverse pre-encode path.
            // tag 0 / pinned panel ImageTexture (image_tex_id) → treat as sRGB color.
            let as_srgb_color = match MapKind::from_wire_tag(tag) {
                None => true,
                Some(_) if node.image_tex_id != 0 => true, // panel-pinned color layer
                Some(k) => !k.is_data(),
            };
            let sample = |dom: [f32; 3]| -> [f32; 3] {
                if let Some(px) = pinned.map(|v| v.as_slice()).or_else(|| images.get(tag)) {
                    // Bake plane is [-1,1]²; map UVs are [0,1] with Repeat.
                    let u = dom[0] * 0.5 + 0.5;
                    let v = dom[1] * 0.5 + 0.5;
                    sample_map_rgb(px, u, v, as_srgb_color)
                } else {
                    [1.0, 1.0, 1.0]
                }
            };
            let col = sample_projected_vec3(node, pos, n, sample);
            vec![(0, Val::V(col)), (1, Val::F(1.0))]
        }
        NodeType::ConstantValue => vec![(0, Val::F(node.const_float))],
        NodeType::Invert => {
            let c = v_in!(0, [0.5, 0.5, 0.5]);
            // Match WGSL: Normal-channel panel Invert flips XY only (keep blue/Z out).
            let normal_map_invert = crate::material_panel::lower::parse_panel_tag(&node.label)
                .map(|(k, role, _, _)| {
                    k == crate::material_panel::model::ChannelKind::Normal as u32 && role == "inv"
                })
                .unwrap_or(false);
            if normal_map_invert {
                vec![(0, Val::V([1.0 - c[0], 1.0 - c[1], c[2]]))]
            } else {
                vec![(0, Val::V([1.0 - c[0], 1.0 - c[1], 1.0 - c[2]]))]
            }
        }
        NodeType::ColorCorrect => {
            let mut c = v_in!(0, [0.5, 0.5, 0.5]);
            let hue = f_in!(1, 0.0);
            let sat = f_in!(2, 1.0);
            let val = f_in!(3, 1.0);
            let gamma = f_in!(4, 1.0).max(0.001);
            let exposure = f_in!(5, 0.0);
            let contrast = f_in!(6, 0.0);
            let exp2 = 2.0f32.powf(exposure);
            for i in 0..3 {
                c[i] = ((c[i] * exp2 - 0.5) * (1.0 + contrast) + 0.5).clamp(0.0, 1.0);
            }
            let luma = 0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2];
            for i in 0..3 {
                c[i] = (luma + (c[i] - luma) * sat) * val;
            }
            // YIQ hue rotate
            let ang = hue * std::f32::consts::TAU;
            let (ca, sa) = (ang.cos(), ang.sin());
            let i_comp = 0.596 * c[0] - 0.274 * c[1] - 0.322 * c[2];
            let q_comp = 0.211 * c[0] - 0.523 * c[1] + 0.312 * c[2];
            let ir = i_comp * ca - q_comp * sa;
            let qr = i_comp * sa + q_comp * ca;
            c = [
                (luma + 0.956 * ir + 0.621 * qr).clamp(0.0, 1.0),
                (luma - 0.272 * ir - 0.647 * qr).clamp(0.0, 1.0),
                (luma - 1.106 * ir + 1.703 * qr).clamp(0.0, 1.0),
            ];
            let inv_g = 1.0 / gamma;
            for i in 0..3 {
                c[i] = c[i].max(0.0).powf(inv_g);
            }
            vec![(0, Val::V(c))]
        }
        NodeType::Curvature => {
            let pos = if is_input_connected(graph, id, 0) {
                v_in!(0, p)
            } else {
                p
            };
            let eps = f_in!(1, 0.02).max(1e-4);
            let contrast = f_in!(2, 1.0).max(0.001);
            let strength = f_in!(3, 1.0);
            // NO curvature source → fail closed (0), never the synthetic `sn_field_h` demo
            // pattern this branch used to paint. That fallback produced a plausible-looking
            // procedural curvature with no relationship to ANY geometry — indistinguishable from
            // a working generator at a glance, which is the failure mode `SurfaceDirection` below
            // already documents and rejects. A polygonal body reaches the real path through
            // `curv_caches` (measured on the half-edge mesh), not through a fake field.
            let (mask, _, _, _) = if sdf.has_curvature_source() {
                sdf_curvature_masks(sdf, pos, eps, 0, 0, 0.0, 0.2, contrast, strength)
            } else {
                (0.0, 0.0, 0.0, 0.0)
            };
            vec![(0, Val::F(mask))]
        }
        NodeType::SdfCurvature => {
            let pos = if is_input_connected(graph, id, 0) {
                v_in!(0, p)
            } else {
                p
            };
            let eps = f_in!(1, 0.03).max(1e-4);
            let grow = f_in!(2, 0.0);
            let band = f_in!(3, 0.15).max(1e-4);
            let contrast = f_in!(4, 1.0).max(0.001);
            let strength = f_in!(5, 1.0);
            let uid = node.sdf_prim_uid;
            // Trailing `v` on panel tags (`sgv`/`cxv`/`ccv`) → vertex splat grid.
            let vertex_mode = crate::material_panel::lower::parse_panel_tag(&node.label)
                .and_then(|(.., f)| f)
                .is_some_and(|f| f.ends_with('v'));
            let (mask, curv, conv, conc) = sdf_curvature_masks_ex(
                sdf,
                pos,
                eps,
                uid,
                node.sdf_curv_region,
                grow,
                band,
                contrast,
                strength,
                vertex_mode,
            );
            vec![
                (0, Val::F(mask)),
                (1, Val::F(curv)),
                (2, Val::F(conv)),
                (3, Val::F(conc)),
            ]
        }
        NodeType::PrimSdfMask => {
            let pos = if is_input_connected(graph, id, 0) {
                v_in!(0, p)
            } else {
                p
            };
            let grow = f_in!(1, 0.0);
            let soft = f_in!(2, 0.08).max(1e-5);
            let contrast = f_in!(3, 1.0);
            let strength = f_in!(4, 1.0);
            let invert = f_in!(5, 0.0) > 0.5;
            let uid = node.sdf_prim_uid;
            let d = sdf.sdf_at(pos, uid);
            let mask = sdf_to_mask(d, grow, soft, contrast, strength, invert);
            let inside = if d - grow < 0.0 { 1.0 } else { 0.0 };
            vec![
                (0, Val::F(mask)),
                (1, Val::F(d - grow)),
                (2, Val::F(inside)),
            ]
        }
        NodeType::Cavity => {
            let pos = if is_input_connected(graph, id, 0) {
                v_in!(0, p)
            } else {
                p
            };
            let eps = f_in!(1, 0.02).max(1e-4);
            let scale = f_in!(2, 1.0).max(0.01);
            let pos = [pos[0] * scale, pos[1] * scale, pos[2] * scale];
            let vertex_mode = crate::material_panel::lower::parse_panel_tag(&node.label)
                .and_then(|(.., f)| f)
                .is_some_and(|f| f.ends_with('v'));
            // Same fail-closed rule as `Curvature` above — no source, no cavity, never a
            // synthetic stand-in.
            let (_, _, _, concave) = if sdf.has_curvature_source() {
                sdf_curvature_masks_ex(sdf, pos, eps, 0, 2, 0.0, 0.2, 1.0, 1.0, vertex_mode)
            } else {
                (0.0, 0.0, 0.0, 0.0)
            };
            vec![(0, Val::F(concave))]
        }
        NodeType::SurfaceDirection => {
            let pos = if is_input_connected(graph, id, 0) {
                v_in!(0, p)
            } else {
                p
            };
            let eps = f_in!(1, 0.02).max(1e-4);
            let grow = f_in!(2, 0.0);
            let band = f_in!(3, 0.2).max(1e-4);
            let contrast = f_in!(4, 1.0).max(0.001);
            let strength = f_in!(5, 1.0);
            let axis = crate::shader_nodes::nodes::surface_direction::axis_vec(node.sdf_curv_region);
            let (mask, dot) = if sdf.has_field() {
                // Real surface normal from the body SDF gradient.
                sdf_direction_mask(
                    sdf,
                    pos,
                    eps,
                    node.sdf_prim_uid,
                    axis,
                    grow,
                    band,
                    contrast,
                    strength,
                )
            } else {
                // NO SDF FIELD → fail closed (0), never a plausible constant. The evaluator's `n`
                // is the flat bake quad's constant [0,0,1] here, so `dot(n, axis)` would paint a
                // uniform card that LOOKS like a working directional mask — exactly the
                // synthetic-field lie this codebase already carries twice. The panel gates
                // Direction on `has_sdf` so the user cannot author this state; a graph hand-built
                // in the node editor without a body field gets black, which reads as "no data".
                (0.0, 0.0)
            };
            vec![(0, Val::F(mask)), (1, Val::F(dot))]
        }
        NodeType::AmbientOcclusion => {
            let pos = if is_input_connected(graph, id, 0) {
                v_in!(0, p)
            } else {
                p
            };
            // P5: real body SDF AO when a field is present. Do NOT use the evaluator's
            // flat-bake `n` (constant [0,0,1]) — `sdf_ambient_occlusion` recovers the
            // surface normal from the SDF gradient, matching Direction/Curvature.
            // No field → 1.0 (unoccluded); never the synthetic sn_field_h demo pattern.
            let ao = if sdf.has_field() {
                sdf_ambient_occlusion(sdf, pos, 0.02, node.sdf_prim_uid, 0.0, 0.2)
            } else {
                1.0
            };
            vec![(0, Val::F(ao))]
        }
        NodeType::Slope => {
            let nrm = v_in!(0, n);
            // Prefer Z-up (bake normal) then Y-up (world).
            let up = if nrm[2].abs() >= nrm[1].abs() {
                nrm[2]
            } else {
                nrm[1]
            };
            let s = up.clamp(-1.0, 1.0).acos() / std::f32::consts::PI;
            vec![(0, Val::F(s))]
        }
        NodeType::HeightToNormal => {
            let str = f_in!(1, 1.0);
            let eps = f_in!(2, 0.02).max(1e-4);
            vec![(0, Val::V(sample_height_normal(graph, node, 0, p, n, hit_prim,
                images, sdf, max_noise_octaves, [0.0, 0.0, 1.0], str, eps)))]
        }
        NodeType::VectorMath => {
            let a = v_in!(0, [0.0, 0.0, 0.0]);
            let b = v_in!(1, [0.0, 0.0, 0.0]);
            let sc = f_in!(2, 1.0);
            let (vec, scalar) = match node.vector_math_op {
                VectorMathOp::Add => ([a[0] + b[0], a[1] + b[1], a[2] + b[2]], 0.0),
                VectorMathOp::Subtract => ([a[0] - b[0], a[1] - b[1], a[2] - b[2]], 0.0),
                VectorMathOp::Multiply => ([a[0] * b[0], a[1] * b[1], a[2] * b[2]], 0.0),
                VectorMathOp::Scale => ([a[0] * sc, a[1] * sc, a[2] * sc], 0.0),
                VectorMathOp::Dot => {
                    let d = a[0] * b[0] + a[1] * b[1] + a[2] * b[2];
                    ([d, d, d], d)
                }
                VectorMathOp::Cross => (
                    [
                        a[1] * b[2] - a[2] * b[1],
                        a[2] * b[0] - a[0] * b[2],
                        a[0] * b[1] - a[1] * b[0],
                    ],
                    0.0,
                ),
                VectorMathOp::Length => {
                    let l = (a[0] * a[0] + a[1] * a[1] + a[2] * a[2]).sqrt();
                    ([l, l, l], l)
                }
                VectorMathOp::Normalize => {
                    let l = (a[0] * a[0] + a[1] * a[1] + a[2] * a[2]).sqrt().max(1e-8);
                    ([a[0] / l, a[1] / l, a[2] / l], 0.0)
                }
                VectorMathOp::Reflect => {
                    // reflect(I, N) = I - 2*dot(N,I)*N
                    let nl = (b[0] * b[0] + b[1] * b[1] + b[2] * b[2]).sqrt().max(1e-8);
                    let nn = [b[0] / nl, b[1] / nl, b[2] / nl];
                    let d = 2.0 * (nn[0] * a[0] + nn[1] * a[1] + nn[2] * a[2]);
                    ([a[0] - d * nn[0], a[1] - d * nn[1], a[2] - d * nn[2]], 0.0)
                }
                VectorMathOp::Distance => {
                    let dx = a[0] - b[0];
                    let dy = a[1] - b[1];
                    let dz = a[2] - b[2];
                    let d = (dx * dx + dy * dy + dz * dz).sqrt();
                    ([d, d, d], d)
                }
            };
            vec![(0, Val::V(vec)), (1, Val::F(scalar))]
        }
        NodeType::Clamp => {
            let v = f_in!(0, 0.5);
            let mn = f_in!(1, 0.0);
            let mx = f_in!(2, 1.0);
            vec![(0, Val::F(v.clamp(mn, mx)))]
        }
        NodeType::MixFloat => {
            let a = f_in!(0, 0.0);
            let b = f_in!(1, 1.0);
            let t = f_in!(2, 0.5);
            vec![(0, Val::F(a + (b - a) * t))]
        }
        NodeType::Smoothstep => {
            let e0 = f_in!(0, 0.0);
            let e1 = f_in!(1, 1.0);
            let v = f_in!(2, 0.5);
            let t = ((v - e0) / (e1 - e0).max(1e-4)).clamp(0.0, 1.0);
            let s = t * t * (3.0 - 2.0 * t);
            vec![(0, Val::F(s))]
        }
        NodeType::SdfThickness => {
            let pos = if is_input_connected(graph, id, 0) {
                v_in!(0, p)
            } else {
                p
            };
            let nrm = v_in!(1, n);
            let max_d = f_in!(2, 0.25).max(1e-4);
            let contrast = f_in!(3, 1.0).max(0.001);
            let strength = f_in!(4, 1.0);
            let th = if sdf.has_field() {
                sdf_thickness(sdf, pos, nrm, node.sdf_prim_uid, max_d)
            } else {
                1.0 // no field → thick (open)
            };
            let t = th.clamp(0.0, 1.0);
            let shaped = (((t - 0.5) * contrast + 0.5).clamp(0.0, 1.0) * strength).clamp(0.0, 1.0);
            vec![(0, Val::F(shaped))]
        }
        NodeType::SdfPosition => {
            let pos = if is_input_connected(graph, id, 0) {
                v_in!(0, p)
            } else {
                p
            };
            let axis = f_in!(1, 0.0) as u32;
            let (x, y, z) = sdf_position_01(sdf, pos, node.sdf_prim_uid);
            let col = match axis {
                1 => [x, x, x],
                2 => [y, y, y],
                3 => [z, z, z],
                _ => [x, y, z],
            };
            vec![
                (0, Val::V(col)),
                (1, Val::F(x)),
                (2, Val::F(y)),
                (3, Val::F(z)),
            ]
        }
    };

    // Trailing **Mask** float (default 1) multiplies every output of maskable nodes.
    let produced = if let Some(mi) = Node::mask_input_index(&node.node_type) {
        let m = f_in!(mi, 1.0);
        produced
            .into_iter()
            .map(|(oi, v)| {
                let v = match v {
                    Val::F(f) => Val::F(f * m),
                    Val::V(c) => Val::V([c[0] * m, c[1] * m, c[2] * m]),
                };
                (oi, v)
            })
            .collect()
    } else {
        produced
    };

    for (out_i, val) in produced {
        outs.insert((id, out_i), val);
    }
}

pub(super) fn sample_height_normal(
    graph: &NodeGraph, node: &Node, input: usize, p: [f32; 3], n: [f32; 3], hit_prim: f32,
    images: &ImageMapSources, sdf: &SdfFieldSources, max_noise_octaves: u32,
    base: [f32; 3], strength: f32, epsilon: f32,
) -> [f32; 3] {
    let order = graph.connection_source(node.id, input).and_then(|(source, _)| graph.topo_sort(source));
    let sample = |position| {
        let mut sampled = HashMap::new();
        if let Some(order) = &order {
            for &source in order {
                eval_node(graph, graph.find_node(source).unwrap(), position, n, hit_prim,
                    images, sdf, max_noise_octaves, &mut sampled);
            }
        }
        input_val(graph, node.id, input, Val::F(def_f(node, input, 0.0)), &sampled).as_f()
    };
    let dx = (sample([p[0] + epsilon, p[1], p[2]]) - sample([p[0] - epsilon, p[1], p[2]])) / (2.0 * epsilon);
    let dy = (sample([p[0], p[1] + epsilon, p[2]]) - sample([p[0], p[1] - epsilon, p[2]])) / (2.0 * epsilon);
    core_math::pbr_material::surface_bump_normal(base, dx, dy, strength)
}

// --- Built-in projection (default Triplanar on texture/noise generators) ---

/// 2D UV rotate (degrees) + offset — matches WGSL `sn_uv_xf`.
fn uv_xf(u: f32, v: f32, rot_deg: f32, off: [f32; 2]) -> [f32; 3] {
    let r = rot_deg * (std::f32::consts::PI / 180.0);
    let (c, s) = (r.cos(), r.sin());
    [u * c - v * s + off[0], u * s + v * c + off[1], 0.0]
}

/// Map position → sample domain for a single plane style (with per-side transforms from `node`).
fn project_domain(node: &Node, p: [f32; 3]) -> [f32; 3] {
    let style = node.projection;
    let oz = node.proj_off_z;
    let rz = node.proj_rot_z;
    match style {
        ProjectionStyle::None => p,
        ProjectionStyle::PlanarXY | ProjectionStyle::Triplanar => {
            // Triplanar handled in sample_projected*; PlanarXY uses Z-side controls.
            uv_xf(p[0], p[1], rz, oz)
        }
        ProjectionStyle::PlanarXZ => uv_xf(p[0], p[2], rz, oz),
        ProjectionStyle::PlanarYZ => uv_xf(p[1], p[2], rz, oz),
        ProjectionStyle::Box => {
            let ax = p[0].abs();
            let ay = p[1].abs();
            let az = p[2].abs();
            if ax >= ay && ax >= az {
                uv_xf(p[1], p[2], node.proj_rot_x, node.proj_off_x)
            } else if ay >= az {
                uv_xf(p[0], p[2], node.proj_rot_y, node.proj_off_y)
            } else {
                uv_xf(p[0], p[1], node.proj_rot_z, node.proj_off_z)
            }
        }
        ProjectionStyle::Cylindrical => {
            let a = p[2].atan2(p[0]);
            uv_xf(a * (1.0 / std::f32::consts::TAU), p[1], rz, oz)
        }
        ProjectionStyle::Spherical => {
            let len = (p[0] * p[0] + p[1] * p[1] + p[2] * p[2]).sqrt().max(1e-8);
            let d = [p[0] / len, p[1] / len, p[2] / len];
            uv_xf(
                d[2].atan2(d[0]) * (1.0 / std::f32::consts::TAU),
                d[1].clamp(-1.0, 1.0).acos() * (1.0 / std::f32::consts::PI),
                rz,
                oz,
            )
        }
    }
}

fn triplanar_weights(n: [f32; 3]) -> [f32; 3] {
    let w = [n[0].abs().powf(4.0), n[1].abs().powf(4.0), n[2].abs().powf(4.0)];
    let s = (w[0] + w[1] + w[2]).max(1e-4);
    [w[0] / s, w[1] / s, w[2] / s]
}

/// Skip near-zero triplanar axes (bake plane n=(0,0,1) → only Z — ~3× cheaper noise).
const TRIPLANAR_W_EPS: f32 = 1e-3;

fn sample_projected<F>(node: &Node, p: [f32; 3], n: [f32; 3], mut f: F) -> f32
where
    F: FnMut([f32; 3]) -> f32,
{
    if node.projection == ProjectionStyle::Triplanar {
        let w = triplanar_weights(n);
        let mut acc = 0.0f32;
        if w[0] > TRIPLANAR_W_EPS {
            acc += f(uv_xf(p[1], p[2], node.proj_rot_x, node.proj_off_x)) * w[0];
        }
        if w[1] > TRIPLANAR_W_EPS {
            acc += f(uv_xf(p[0], p[2], node.proj_rot_y, node.proj_off_y)) * w[1];
        }
        if w[2] > TRIPLANAR_W_EPS {
            acc += f(uv_xf(p[0], p[1], node.proj_rot_z, node.proj_off_z)) * w[2];
        }
        acc
    } else {
        f(project_domain(node, p))
    }
}

fn sample_projected_vec3<F>(node: &Node, p: [f32; 3], n: [f32; 3], mut f: F) -> [f32; 3]
where
    F: FnMut([f32; 3]) -> [f32; 3],
{
    if node.projection == ProjectionStyle::Triplanar {
        let w = triplanar_weights(n);
        let mut acc = [0.0f32; 3];
        if w[0] > TRIPLANAR_W_EPS {
            let cx = f(uv_xf(p[1], p[2], node.proj_rot_x, node.proj_off_x));
            acc[0] += cx[0] * w[0];
            acc[1] += cx[1] * w[0];
            acc[2] += cx[2] * w[0];
        }
        if w[1] > TRIPLANAR_W_EPS {
            let cy = f(uv_xf(p[0], p[2], node.proj_rot_y, node.proj_off_y));
            acc[0] += cy[0] * w[1];
            acc[1] += cy[1] * w[1];
            acc[2] += cy[2] * w[1];
        }
        if w[2] > TRIPLANAR_W_EPS {
            let cz = f(uv_xf(p[0], p[1], node.proj_rot_z, node.proj_off_z));
            acc[0] += cz[0] * w[2];
            acc[1] += cz[1] * w[2];
            acc[2] += cz[2] * w[2];
        }
        acc
    } else {
        f(project_domain(node, p))
    }
}

fn sample_projected_triple<F>(
    node: &Node,
    p: [f32; 3],
    n: [f32; 3],
    mut f: F,
) -> (f32, f32, [f32; 3])
where
    F: FnMut([f32; 3]) -> (f32, f32, [f32; 3]),
{
    if node.projection == ProjectionStyle::Triplanar {
        let w = triplanar_weights(n);
        let mut d_acc = 0.0f32;
        let mut c_acc = 0.0f32;
        let mut col = [0.0f32; 3];
        if w[0] > TRIPLANAR_W_EPS {
            let (dx, cx, colx) = f(uv_xf(p[1], p[2], node.proj_rot_x, node.proj_off_x));
            d_acc += dx * w[0];
            c_acc += cx * w[0];
            col[0] += colx[0] * w[0];
            col[1] += colx[1] * w[0];
            col[2] += colx[2] * w[0];
        }
        if w[1] > TRIPLANAR_W_EPS {
            let (dy, cy, coly) = f(uv_xf(p[0], p[2], node.proj_rot_y, node.proj_off_y));
            d_acc += dy * w[1];
            c_acc += cy * w[1];
            col[0] += coly[0] * w[1];
            col[1] += coly[1] * w[1];
            col[2] += coly[2] * w[1];
        }
        if w[2] > TRIPLANAR_W_EPS {
            let (dz, cz, colz) = f(uv_xf(p[0], p[1], node.proj_rot_z, node.proj_off_z));
            d_acc += dz * w[2];
            c_acc += cz * w[2];
            col[0] += colz[0] * w[2];
            col[1] += colz[1] * w[2];
            col[2] += colz[2] * w[2];
        }
        (d_acc, c_acc, col)
    } else {
        f(project_domain(node, p))
    }
}

// --- Noise (true gradient Perlin base + FBM; Value remains classic value noise) ---

fn hash31(p: [f32; 3]) -> f32 {
    // Integer-friendly hash (stable, no sin precision traps).
    let mut n = p[0] * 127.1 + p[1] * 311.7 + p[2] * 74.7;
    n = (n * 0.1031).fract();
    n *= n + 33.33;
    n *= n + n;
    n.fract().abs()
}

fn fade(t: f32) -> f32 {
    t * t * t * (t * (t * 6.0 - 15.0) + 10.0) // Perlin quintic
}

/// 12 edge-of-cube gradients (classic Perlin).
fn grad3_from_hash(h: f32) -> [f32; 3] {
    const G: [[f32; 3]; 12] = [
        [1.0, 1.0, 0.0],
        [-1.0, 1.0, 0.0],
        [1.0, -1.0, 0.0],
        [-1.0, -1.0, 0.0],
        [1.0, 0.0, 1.0],
        [-1.0, 0.0, 1.0],
        [1.0, 0.0, -1.0],
        [-1.0, 0.0, -1.0],
        [0.0, 1.0, 1.0],
        [0.0, -1.0, 1.0],
        [0.0, 1.0, -1.0],
        [0.0, -1.0, -1.0],
    ];
    let i = ((h * 12.0).floor() as usize) % 12;
    G[i]
}

fn wrap_cell(i: f32, period: i32) -> f32 {
    if period <= 0 {
        return i;
    }
    let p = period as f32;
    i.rem_euclid(p)
}

fn grad_dot(ix: f32, iy: f32, iz: f32, fx: f32, fy: f32, fz: f32) -> f32 {
    let g = grad3_from_hash(hash31([ix, iy, iz]));
    g[0] * fx + g[1] * fy + g[2] * fz
}

/// Classic 3D gradient Perlin → 0..1.
/// `period`: when `Some(n)` with n≥1, lattice wraps every n cells (tileable / seamless).
fn perlin3(p: [f32; 3]) -> f32 {
    perlin3_period(p, None)
}

fn perlin3_period(p: [f32; 3], period: Option<i32>) -> f32 {
    let x0 = p[0].floor();
    let y0 = p[1].floor();
    let z0 = p[2].floor();
    let fx = p[0] - x0;
    let fy = p[1] - y0;
    let fz = p[2] - z0;
    let (ix0, iy0, iz0, ix1, iy1, iz1) = if let Some(per) = period.filter(|&n| n > 0) {
        (
            wrap_cell(x0, per),
            wrap_cell(y0, per),
            wrap_cell(z0, per),
            wrap_cell(x0 + 1.0, per),
            wrap_cell(y0 + 1.0, per),
            wrap_cell(z0 + 1.0, per),
        )
    } else {
        (x0, y0, z0, x0 + 1.0, y0 + 1.0, z0 + 1.0)
    };
    let u = fade(fx);
    let v = fade(fy);
    let w = fade(fz);
    let n000 = grad_dot(ix0, iy0, iz0, fx, fy, fz);
    let n100 = grad_dot(ix1, iy0, iz0, fx - 1.0, fy, fz);
    let n010 = grad_dot(ix0, iy1, iz0, fx, fy - 1.0, fz);
    let n110 = grad_dot(ix1, iy1, iz0, fx - 1.0, fy - 1.0, fz);
    let n001 = grad_dot(ix0, iy0, iz1, fx, fy, fz - 1.0);
    let n101 = grad_dot(ix1, iy0, iz1, fx - 1.0, fy, fz - 1.0);
    let n011 = grad_dot(ix0, iy1, iz1, fx, fy - 1.0, fz - 1.0);
    let n111 = grad_dot(ix1, iy1, iz1, fx - 1.0, fy - 1.0, fz - 1.0);
    let x00 = n000 + (n100 - n000) * u;
    let x10 = n010 + (n110 - n010) * u;
    let x01 = n001 + (n101 - n001) * u;
    let x11 = n011 + (n111 - n011) * u;
    let y0v = x00 + (x10 - x00) * v;
    let y1v = x01 + (x11 - x01) * v;
    let n = y0v + (y1v - y0v) * w;
    (n * 0.5 + 0.5).clamp(0.0, 1.0)
}

/// Value noise (smooth hash lattice) — used for NoiseType::Value only.
fn noise3(p: [f32; 3]) -> f32 {
    noise3_period(p, None)
}

fn noise3_period(p: [f32; 3], period: Option<i32>) -> f32 {
    let i = [p[0].floor(), p[1].floor(), p[2].floor()];
    let f = [p[0].fract(), p[1].fract(), p[2].fract()];
    let u = [fade(f[0]), fade(f[1]), fade(f[2])];
    let mut n = 0.0;
    for dz in 0..2 {
        for dy in 0..2 {
            for dx in 0..2 {
                let raw = [i[0] + dx as f32, i[1] + dy as f32, i[2] + dz as f32];
                let cell = if let Some(per) = period.filter(|&n| n > 0) {
                    [
                        wrap_cell(raw[0], per),
                        wrap_cell(raw[1], per),
                        wrap_cell(raw[2], per),
                    ]
                } else {
                    raw
                };
                let g = hash31(cell);
                let wx = if dx == 0 { 1.0 - u[0] } else { u[0] };
                let wy = if dy == 0 { 1.0 - u[1] } else { u[1] };
                let wz = if dz == 0 { 1.0 - u[2] } else { u[2] };
                n += g * wx * wy * wz;
            }
        }
    }
    n
}

fn fbm3(p: [f32; 3], octaves: u32) -> f32 {
    fbm3_lac(p, octaves, 2.0, 0.5)
}

/// FBM of **gradient Perlin** (smooth clouds). Gain clamped so octaves actually contribute.
/// When `period` is set, each octave uses period* freq (freq must keep period integer — we
/// round) so the FBM stays tileable.
fn fbm3_lac(p: [f32; 3], octaves: u32, lac: f32, gain: f32) -> f32 {
    fbm3_lac_period(p, octaves, lac, gain, None)
}

fn fbm3_lac_period(
    p: [f32; 3],
    octaves: u32,
    lac: f32,
    gain: f32,
    period: Option<i32>,
) -> f32 {
    let mut v = 0.0;
    let mut a = 0.5;
    let mut pp = p;
    let mut amp_sum = 0.0;
    let mut freq = 1.0f32;
    let oct = octaves.clamp(1, BAKE_MAX_OCTAVES);
    let lac = lac.max(1.01);
    let gain = gain.clamp(0.15, 0.95);
    for _ in 0..oct {
        let per = period.map(|base| (base as f32 * freq).round().max(1.0) as i32);
        let n = perlin3_period(pp, per) * 2.0 - 1.0;
        v += a * n;
        amp_sum += a;
        pp = [pp[0] * lac, pp[1] * lac, pp[2] * lac];
        freq *= lac;
        a *= gain;
    }
    ((v / amp_sum.max(1e-4)) * 0.5 + 0.5).clamp(0.0, 1.0)
}

/// Lightweight simplex-ish (skew + gradient dots on tetra corners) — better than value-alias.
fn simplex3(p: [f32; 3]) -> f32 {
    const F3: f32 = 1.0 / 3.0;
    const G3: f32 = 1.0 / 6.0;
    let s = (p[0] + p[1] + p[2]) * F3;
    let i = [(p[0] + s).floor(), (p[1] + s).floor(), (p[2] + s).floor()];
    let t = (i[0] + i[1] + i[2]) * G3;
    let x0 = p[0] - (i[0] - t);
    let y0 = p[1] - (i[1] - t);
    let z0 = p[2] - (i[2] - t);
    // Corner contribution (simplified 4-corner blend of gradient noise at lattice).
    let n0 = grad_dot(i[0], i[1], i[2], x0, y0, z0);
    let n1 = grad_dot(i[0] + 1.0, i[1], i[2], x0 - 1.0 + G3, y0 + G3, z0 + G3);
    let n2 = grad_dot(i[0], i[1] + 1.0, i[2], x0 + G3, y0 - 1.0 + G3, z0 + G3);
    let n3 = grad_dot(i[0], i[1], i[2] + 1.0, x0 + G3, y0 + G3, z0 - 1.0 + G3);
    let t0 = (0.6 - x0 * x0 - y0 * y0 - z0 * z0).max(0.0);
    let t1 = (0.6
        - (x0 - 1.0 + G3).powi(2)
        - (y0 + G3).powi(2)
        - (z0 + G3).powi(2))
    .max(0.0);
    let t2 = (0.6
        - (x0 + G3).powi(2)
        - (y0 - 1.0 + G3).powi(2)
        - (z0 + G3).powi(2))
    .max(0.0);
    let t3 = (0.6
        - (x0 + G3).powi(2)
        - (y0 + G3).powi(2)
        - (z0 - 1.0 + G3).powi(2))
    .max(0.0);
    let n = 32.0
        * (t0 * t0 * t0 * t0 * n0
            + t1 * t1 * t1 * t1 * n1
            + t2 * t2 * t2 * t2 * n2
            + t3 * t3 * t3 * t3 * n3);
    (n * 0.5 + 0.5).clamp(0.0, 1.0)
}

fn cellular_f1(p: [f32; 3]) -> f32 {
    cellular_f1_period(p, None)
}

/// Worley F1. When `period` is set, cell indices wrap so the field tiles (no edge/cross seams).
fn cellular_f1_period(p: [f32; 3], period: Option<i32>) -> f32 {
    let ip = [p[0].floor(), p[1].floor(), p[2].floor()];
    let fp = [p[0].fract(), p[1].fract(), p[2].fract()];
    let mut best = 8.0f32;
    for dz in -1i32..=1 {
        for dy in -1i32..=1 {
            for dx in -1i32..=1 {
                let g = [dx as f32, dy as f32, dz as f32];
                let raw = [ip[0] + g[0], ip[1] + g[1], ip[2] + g[2]];
                let cell = if let Some(per) = period.filter(|&n| n > 0) {
                    [
                        wrap_cell(raw[0], per),
                        wrap_cell(raw[1], per),
                        wrap_cell(raw[2], per),
                    ]
                } else {
                    raw
                };
                let o = [
                    hash31(cell),
                    hash31([cell[0] + 17.0, cell[1], cell[2]]),
                    hash31([cell[0], cell[1] + 19.0, cell[2]]),
                ];
                let d = [
                    g[0] + o[0] - fp[0],
                    g[1] + o[1] - fp[1],
                    g[2] + o[2] - fp[2],
                ];
                let dist = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
                best = best.min(dist);
            }
        }
    }
    best.clamp(0.0, 1.0)
}

fn voronoi_f1(p: [f32; 3], randomness: f32) -> (f32, f32, [f32; 3]) {
    let ip = [p[0].floor(), p[1].floor(), p[2].floor()];
    let fp = [p[0].fract(), p[1].fract(), p[2].fract()];
    let mut best = 8.0f32;
    let mut cell_id = 0.0f32;
    let mut col = [0.5f32; 3];
    for dz in -1i32..=1 {
        for dy in -1i32..=1 {
            for dx in -1i32..=1 {
                let g = [dx as f32, dy as f32, dz as f32];
                let cell = [ip[0] + g[0], ip[1] + g[1], ip[2] + g[2]];
                let h = [
                    hash31(cell),
                    hash31([cell[0] + 19.0, cell[1], cell[2]]),
                    hash31([cell[0], cell[1] + 23.0, cell[2]]),
                ];
                let o = [
                    0.5 + (h[0] - 0.5) * randomness,
                    0.5 + (h[1] - 0.5) * randomness,
                    0.5 + (h[2] - 0.5) * randomness,
                ];
                let d = [
                    g[0] + o[0] - fp[0],
                    g[1] + o[1] - fp[1],
                    g[2] + o[2] - fp[2],
                ];
                let dist = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
                if dist < best {
                    best = dist;
                    cell_id = hash31(cell);
                    col = h;
                }
            }
        }
    }
    (best.clamp(0.0, 1.0), cell_id, col)
}

// Re-exported by parent as `pub(super)` for tests — `pub` so `pub(super) use` is legal.
pub fn eval_noise(
    kind: NoiseType,
    sp: [f32; 3],
    scale: f32,
    oct: u32,
    lac: f32,
    gain: f32,
    distortion: f32,
    tile: bool,
) -> f32 {
    // Scale = tile count when tiling (integer lattice period). Respect user scale as-is.
    let scale = if scale.abs() < 1e-6 {
        1e-6_f32.copysign(if scale < 0.0 { -1.0 } else { 1.0 })
    } else {
        scale
    };
    let period = if tile {
        Some(scale.abs().round().max(1.0) as i32)
    } else {
        None
    };
    // Tileable: domain sp is typically ~[0,1] or [-1,1]; map into [0, period) lattice.
    // Non-tile: classic p = sp * scale.
    let p = if tile {
        let per = period.unwrap() as f32;
        // Map common bake domain [-1,1] → [0, period) so edges match under Repeat.
        let ux = (sp[0] * 0.5 + 0.5).rem_euclid(1.0) * per;
        let uy = (sp[1] * 0.5 + 0.5).rem_euclid(1.0) * per;
        let uz = (sp[2] * 0.5 + 0.5).rem_euclid(1.0) * per;
        [ux, uy, uz]
    } else {
        [sp[0] * scale, sp[1] * scale, sp[2] * scale]
    };
    let lac = if lac.abs() < 1e-4 { 2.0 } else { lac };
    let gain = gain;
    let oct = oct.clamp(1, BAKE_MAX_OCTAVES);
    match kind {
        NoiseType::Value => noise3_period(p, period),
        NoiseType::Fbm => fbm3_lac_period(p, oct, lac, gain, period),
        NoiseType::White => {
            if let Some(per) = period {
                let ix = wrap_cell(p[0].floor(), per);
                let iy = wrap_cell(p[1].floor(), per);
                let iz = wrap_cell(p[2].floor(), per);
                hash31([ix, iy, iz])
            } else {
                hash31([p[0].floor(), p[1].floor(), p[2].floor()])
            }
        }
        NoiseType::Cellular => cellular_f1_period(p, period),
        NoiseType::Ridged => {
            let mut v = 0.0;
            let mut a = 0.5;
            let mut amp_sum = 0.0;
            let mut pp = p;
            let mut freq = 1.0f32;
            for _ in 0..oct {
                let per = period.map(|base| (base as f32 * freq).round().max(1.0) as i32);
                let n = perlin3_period(pp, per) * 2.0 - 1.0;
                v += (1.0 - n.abs()) * a;
                amp_sum += a;
                pp = [pp[0] * lac, pp[1] * lac, pp[2] * lac];
                freq *= lac;
                a *= gain;
            }
            (v / amp_sum.max(1e-4)).clamp(0.0, 1.0)
        }
        NoiseType::Billow => {
            let mut v = 0.0;
            let mut a = 0.5;
            let mut amp_sum = 0.0;
            let mut pp = p;
            let mut freq = 1.0f32;
            for _ in 0..oct {
                let per = period.map(|base| (base as f32 * freq).round().max(1.0) as i32);
                let n = perlin3_period(pp, per) * 2.0 - 1.0;
                v += n.abs() * a;
                amp_sum += a;
                pp = [pp[0] * lac, pp[1] * lac, pp[2] * lac];
                freq *= lac;
                a *= gain;
            }
            (v / amp_sum.max(1e-4)).clamp(0.0, 1.0)
        }
        NoiseType::DomainWarp => {
            let w = perlin3_period(p, period) * 2.0 - 1.0;
            let wp = [
                sp[0] + distortion * w,
                sp[1] + distortion * w,
                sp[2] + distortion * w,
            ];
            // Re-enter eval in same tile mode.
            eval_noise(
                NoiseType::Perlin,
                wp,
                scale,
                1,
                lac,
                gain,
                0.0,
                tile,
            )
        }
        NoiseType::Perlin => perlin3_period(p, period),
        NoiseType::Simplex => {
            // Simplex tile: fall back to periodic Perlin (true tileable simplex is heavier).
            if period.is_some() {
                perlin3_period(p, period)
            } else {
                simplex3(p)
            }
        }
        NoiseType::Musgrave => {
            let mut sum = 0.0;
            let mut freq = 1.0;
            let mut amp = 1.0;
            let mut amp_sum = 0.0;
            let dim = gain.max(0.15);
            for _ in 0..oct {
                let q = [p[0] * freq, p[1] * freq, p[2] * freq];
                let per = period.map(|base| (base as f32 * freq).round().max(1.0) as i32);
                sum += (perlin3_period(q, per) * 2.0 - 1.0) * amp;
                amp_sum += amp;
                amp *= freq.max(0.001).powf(-dim);
                freq *= lac;
            }
            ((sum / amp_sum.max(1e-4)) * 0.5 + 0.5).clamp(0.0, 1.0)
        }
    }
}

