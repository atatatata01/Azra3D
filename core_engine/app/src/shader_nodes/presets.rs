// shader_nodes/presets.rs
use super::node_type::NodeType;
use super::graph::NodeGraph;
use super::ops::{MathBinaryOp, NoiseBlendCurve, NoiseType, ProjectionStyle};

pub struct MaterialPresets;

impl MaterialPresets {
    /// Catalog of one-click material graphs (Properties → Load Preset…).
    pub fn list() -> Vec<(&'static str, &'static str, fn() -> NodeGraph)> {
        vec![
            (
                "Pipeline Proof",
                "TexCoord → Noise → ColorRamp → Albedo (+ roughness). Proves material-built-from-nodes.",
                Self::pipeline_proof,
            ),
            (
                "Worn Gunmetal",
                "Dark industrial steel: soft base variation, fine scratches, dirt, high metal, roughness + normal.",
                Self::worn_gunmetal,
            ),
            (
                "Old Gold",
                "Antique worn gold: warm bronze→gold range, soft pits, high metal, matte-brushed roughness.",
                Self::old_gold,
            ),
        ]
    }

    /// Minimal working graph: noise-driven albedo + roughness.
    fn pipeline_proof() -> NodeGraph {
        let mut g = NodeGraph {
            nodes: vec![],
            connections: vec![],
            next_id: 0,
            wgsl_cache: None,
            bake_res: 256,
            shader_type: 0,
            texture_space: Default::default(),
            generator_mode: 0,
            channel_meta: Default::default(),
        };
        let tc = g.add_node(NodeType::TexCoord, [40.0, 120.0]);
        let noise = g.add_node(NodeType::Noise, [220.0, 100.0]);
        let ramp = g.add_node(NodeType::ColorRamp, [420.0, 100.0]);
        let out = g.add_node(NodeType::MaterialOutput, [640.0, 140.0]);
        if let Some(n) = g.find_node_mut(noise) {
            n.input_defaults[1].value_f32 = 4.0; // scale
            n.input_defaults[2].value_f32 = 3.0; // octaves
        }
        if let Some(r) = g.find_node_mut(ramp) {
            r.ramp_color0 = [0.15, 0.12, 0.10];
            r.ramp_color1 = [0.85, 0.55, 0.25];
            r.ramp_pos0 = 0.0;
            r.ramp_pos1 = 1.0;
        }
        g.connect(tc, 0, noise, 0); // position -> noise
        g.connect(noise, 0, ramp, 0); // noise -> ramp fac
        g.connect(ramp, 0, out, 0); // color -> albedo
        g.connect(noise, 0, out, 1); // noise -> roughness
        g
    }

    /// Worn industrial gunmetal matching a brushed / scratched dark steel reference.
    ///
    /// Graph (shader tree):
    ///   base FBM → ColorRamp (dark cold steel) ─┐
    ///   scratch Ridged (streaky) ── MixRGB ─────┼→ albedo
    ///   dirt FBM blotches ── MixRGB ────────────┘
    ///   roughness: polish 0.22 ↔ scratch 0.62 ↔ dirt 0.78
    ///   metallic: 0.96 clean ↔ 0.70 dirt
    ///   height = scratch + fine → Height + HeightToNormal → Normal
    fn worn_gunmetal() -> NodeGraph {
        let mut g = NodeGraph {
            nodes: vec![],
            connections: vec![],
            next_id: 0,
            wgsl_cache: None,
            bake_res: 512, // high quality for library bake
            shader_type: 0,
            texture_space: Default::default(),
            generator_mode: 0,
            channel_meta: Default::default(),
        };

        // ── Generators ──────────────────────────────────────────────
        let base = g.add_node(NodeType::Noise, [40.0, 40.0]);
        let scratch = g.add_node(NodeType::Noise, [40.0, 220.0]);
        let fine = g.add_node(NodeType::Noise, [40.0, 400.0]);
        let dirt = g.add_node(NodeType::Noise, [40.0, 580.0]);

        if let Some(n) = g.find_node_mut(base) {
            n.label = "Base Variation".into();
            n.noise_type = NoiseType::Fbm;
            n.projection = ProjectionStyle::Triplanar;
            n.input_defaults[1].value_f32 = 3.5; // scale
            n.input_defaults[2].value_f32 = 4.0; // octaves
            n.input_defaults[3].value_f32 = 2.0;
            n.input_defaults[4].value_f32 = 0.5;
            n.input_defaults[6].value_f32 = 1.0; // seed
            n.noise_curve = NoiseBlendCurve::Smooth;
            n.noise_black = 0.15;
            n.noise_white = 0.85;
        }
        if let Some(n) = g.find_node_mut(scratch) {
            n.label = "Scratches".into();
            // Ridged multi-fractal = sharp metallic scratch ridges
            n.noise_type = NoiseType::Ridged;
            n.projection = ProjectionStyle::Triplanar;
            n.input_defaults[1].value_f32 = 18.0;
            n.input_defaults[2].value_f32 = 5.0;
            n.input_defaults[3].value_f32 = 2.2;
            n.input_defaults[4].value_f32 = 0.5;
            n.input_defaults[5].value_f32 = 0.55; // distortion = streaky
            n.input_defaults[6].value_f32 = 7.0;
            // Keep mid/high ridges (visible lines, not sparse dots)
            n.noise_curve = NoiseBlendCurve::Levels;
            n.noise_curve_k = 1.15;
            n.noise_black = 0.25;
            n.noise_white = 0.95;
        }
        if let Some(n) = g.find_node_mut(fine) {
            n.label = "Fine Scratches".into();
            n.noise_type = NoiseType::Fbm;
            n.projection = ProjectionStyle::Triplanar;
            n.input_defaults[1].value_f32 = 55.0;
            n.input_defaults[2].value_f32 = 3.0;
            n.input_defaults[5].value_f32 = 0.8; // more streak
            n.input_defaults[6].value_f32 = 3.0;
            n.noise_curve = NoiseBlendCurve::Power;
            n.noise_curve_k = 1.8;
            n.noise_black = 0.4;
            n.noise_white = 1.0;
        }
        if let Some(n) = g.find_node_mut(dirt) {
            n.label = "Dirt / Wear".into();
            // Soft blotches (NOT cellular honeycomb)
            n.noise_type = NoiseType::Fbm;
            n.projection = ProjectionStyle::Triplanar;
            n.input_defaults[1].value_f32 = 2.8;
            n.input_defaults[2].value_f32 = 3.0;
            n.input_defaults[6].value_f32 = 11.0;
            n.noise_curve = NoiseBlendCurve::Levels;
            n.noise_curve_k = 0.9;
            n.noise_black = 0.45; // only darker pockets
            n.noise_white = 1.0;
        }

        // Combine scratch + fine into one wear mask
        let scratch_sum = g.add_node(NodeType::MathBinary, [220.0, 280.0]);
        if let Some(m) = g.find_node_mut(scratch_sum) {
            m.label = "Scratch + Fine".into();
            m.math_binary_op = MathBinaryOp::Add;
        }
        let scratch_clamp = g.add_node(NodeType::Clamp, [360.0, 280.0]);
        if let Some(c) = g.find_node_mut(scratch_clamp) {
            c.label = "Scratch Clamp".into();
            c.input_defaults[1].value_f32 = 0.0; // min
            c.input_defaults[2].value_f32 = 1.0; // max
        }

        // ── Albedo path ─────────────────────────────────────────────
        let base_ramp = g.add_node(NodeType::ColorRamp, [280.0, 40.0]);
        if let Some(r) = g.find_node_mut(base_ramp) {
            r.label = "Gunmetal Range".into();
            // Dark cold steel (matches reference ball)
            r.ramp_color0 = [0.09, 0.095, 0.10];
            r.ramp_color1 = [0.22, 0.235, 0.25];
            r.ramp_pos0 = 0.0;
            r.ramp_pos1 = 1.0;
        }

        let scratch_col = g.add_node(NodeType::ConstantColor, [280.0, 160.0]);
        if let Some(c) = g.find_node_mut(scratch_col) {
            c.label = "Scratch Tint".into();
            c.ramp_color0 = [0.38, 0.39, 0.41];
        }

        let dirt_col = g.add_node(NodeType::ConstantColor, [280.0, 360.0]);
        if let Some(c) = g.find_node_mut(dirt_col) {
            c.label = "Dirt Tint".into();
            c.ramp_color0 = [0.05, 0.048, 0.045];
        }

        let mix_scratch = g.add_node(NodeType::MixRgb, [520.0, 80.0]);
        if let Some(m) = g.find_node_mut(mix_scratch) {
            m.label = "Add Scratches".into();
            m.input_defaults[0].value_f32 = 0.2;
        }
        let mix_dirt = g.add_node(NodeType::MixRgb, [720.0, 140.0]);
        if let Some(m) = g.find_node_mut(mix_dirt) {
            m.label = "Add Dirt".into();
            m.input_defaults[0].value_f32 = 0.15;
        }

        let scratch_amt = g.add_node(NodeType::MathBinary, [360.0, 180.0]);
        if let Some(m) = g.find_node_mut(scratch_amt) {
            m.label = "Scratch × 0.32".into();
            m.math_binary_op = MathBinaryOp::Multiply;
            m.input_defaults[1].value_f32 = 0.32;
        }
        let dirt_amt = g.add_node(NodeType::MathBinary, [360.0, 440.0]);
        if let Some(m) = g.find_node_mut(dirt_amt) {
            m.label = "Dirt × 0.22".into();
            m.math_binary_op = MathBinaryOp::Multiply;
            m.input_defaults[1].value_f32 = 0.22;
        }

        // ── Roughness path ──────────────────────────────────────────
        let r_polish = g.add_node(NodeType::ConstantValue, [520.0, 340.0]);
        if let Some(v) = g.find_node_mut(r_polish) {
            v.label = "R Polish".into();
            v.const_float = 0.22; // shiny dome areas
        }
        let r_scratch = g.add_node(NodeType::ConstantValue, [520.0, 420.0]);
        if let Some(v) = g.find_node_mut(r_scratch) {
            v.label = "R Scratch".into();
            v.const_float = 0.62;
        }
        let r_dirt = g.add_node(NodeType::ConstantValue, [520.0, 500.0]);
        if let Some(v) = g.find_node_mut(r_dirt) {
            v.label = "R Dirt".into();
            v.const_float = 0.78;
        }
        let mix_r1 = g.add_node(NodeType::MixFloat, [720.0, 380.0]);
        if let Some(m) = g.find_node_mut(mix_r1) {
            m.label = "Rough + Scratch".into();
        }
        let mix_r2 = g.add_node(NodeType::MixFloat, [900.0, 420.0]);
        if let Some(m) = g.find_node_mut(mix_r2) {
            m.label = "Rough + Dirt".into();
        }

        // ── Metallic path ───────────────────────────────────────────
        let m_clean = g.add_node(NodeType::ConstantValue, [520.0, 580.0]);
        if let Some(v) = g.find_node_mut(m_clean) {
            v.label = "Metal Clean".into();
            v.const_float = 0.96;
        }
        let m_dirty = g.add_node(NodeType::ConstantValue, [520.0, 660.0]);
        if let Some(v) = g.find_node_mut(m_dirty) {
            v.label = "Metal Dirty".into();
            v.const_float = 0.70;
        }
        let mix_m = g.add_node(NodeType::MixFloat, [720.0, 600.0]);
        if let Some(m) = g.find_node_mut(mix_m) {
            m.label = "Metallic Blend".into();
        }

        // ── Height (bump) — wire Height only; Hybrid builds normal from height ──
        let h_scale = g.add_node(NodeType::MathBinary, [520.0, 740.0]);
        if let Some(m) = g.find_node_mut(h_scale) {
            m.label = "Scratch Height".into();
            m.math_binary_op = MathBinaryOp::Multiply;
            m.input_defaults[1].value_f32 = 0.65;
        }
        let fine_scale = g.add_node(NodeType::MathBinary, [520.0, 820.0]);
        if let Some(m) = g.find_node_mut(fine_scale) {
            m.label = "Fine Height".into();
            m.math_binary_op = MathBinaryOp::Multiply;
            m.input_defaults[1].value_f32 = 0.25;
        }
        let h_add = g.add_node(NodeType::MathBinary, [720.0, 780.0]);
        if let Some(m) = g.find_node_mut(h_add) {
            m.label = "Height Sum".into();
            m.math_binary_op = MathBinaryOp::Add;
        }

        // ── Master Material ─────────────────────────────────────────
        let out = g.add_node(NodeType::MaterialOutput, [1100.0, 280.0]);

        // Generators → scratch mask
        g.connect(scratch, 0, scratch_sum, 0);
        g.connect(fine, 0, scratch_sum, 1);
        g.connect(scratch_sum, 0, scratch_clamp, 0);
        g.connect(base, 0, base_ramp, 0);
        g.connect(scratch_clamp, 0, scratch_amt, 0);
        g.connect(dirt, 0, dirt_amt, 0);

        // Albedo: base → +scratches → +dirt
        // MixRgb: 0=Factor, 1=Color1, 2=Color2
        g.connect(base_ramp, 0, mix_scratch, 1);
        g.connect(scratch_col, 0, mix_scratch, 2);
        g.connect(scratch_amt, 0, mix_scratch, 0);

        g.connect(mix_scratch, 0, mix_dirt, 1);
        g.connect(dirt_col, 0, mix_dirt, 2);
        g.connect(dirt_amt, 0, mix_dirt, 0);

        g.connect(mix_dirt, 0, out, NodeGraph::MO_ALBEDO);

        // Roughness: MixFloat A=0, B=1, Factor=2
        g.connect(r_polish, 0, mix_r1, 0);
        g.connect(r_scratch, 0, mix_r1, 1);
        g.connect(scratch_clamp, 0, mix_r1, 2);
        g.connect(mix_r1, 0, mix_r2, 0);
        g.connect(r_dirt, 0, mix_r2, 1);
        g.connect(dirt, 0, mix_r2, 2);
        g.connect(mix_r2, 0, out, NodeGraph::MO_ROUGHNESS);

        // Metallic
        g.connect(m_clean, 0, mix_m, 0);
        g.connect(m_dirty, 0, mix_m, 1);
        g.connect(dirt, 0, mix_m, 2);
        g.connect(mix_m, 0, out, NodeGraph::MO_METALLIC);

        // Height + Height→Normal (bake finite-diffs height buffer → proper purple normal)
        g.connect(scratch_clamp, 0, h_scale, 0);
        g.connect(fine, 0, fine_scale, 0);
        g.connect(h_scale, 0, h_add, 0);
        g.connect(fine_scale, 0, h_add, 1);
        g.connect(h_add, 0, out, NodeGraph::MO_HEIGHT);

        let h2n = g.add_node(NodeType::HeightToNormal, [900.0, 760.0]);
        if let Some(n) = g.find_node_mut(h2n) {
            n.label = "Height → Normal".into();
            n.input_defaults[1].value_f32 = 1.8; // strength
            n.input_defaults[2].value_f32 = 0.012; // epsilon
        }
        g.connect(h_add, 0, h2n, 0);
        g.connect(h2n, 0, out, NodeGraph::MO_NORMAL);

        g
    }

    /// Antique / old gold matching a warm worn gold sphere reference.
    ///
    /// Graph:
    ///   base FBM → ColorRamp (dark bronze → bright gold)
    ///   pits Cellular (sparse) → darker flecks + roughness bumps
    ///   micro FBM → soft brushed wear
    ///   metallic high (~0.95), roughness mid matte-brushed (0.35–0.65)
    fn old_gold() -> NodeGraph {
        let mut g = NodeGraph {
            nodes: vec![],
            connections: vec![],
            next_id: 0,
            wgsl_cache: None,
            bake_res: 512,
            shader_type: 0,
            texture_space: Default::default(),
            generator_mode: 0,
            channel_meta: Default::default(),
        };

        // ── Generators ──────────────────────────────────────────────
        let base = g.add_node(NodeType::Noise, [40.0, 40.0]);
        let pits = g.add_node(NodeType::Noise, [40.0, 220.0]);
        let micro = g.add_node(NodeType::Noise, [40.0, 400.0]);
        let wear = g.add_node(NodeType::Noise, [40.0, 580.0]);

        if let Some(n) = g.find_node_mut(base) {
            n.label = "Gold Base".into();
            n.noise_type = NoiseType::Fbm;
            n.projection = ProjectionStyle::Triplanar;
            n.input_defaults[1].value_f32 = 3.0;
            n.input_defaults[2].value_f32 = 4.0;
            n.input_defaults[3].value_f32 = 2.0;
            n.input_defaults[4].value_f32 = 0.5;
            n.input_defaults[6].value_f32 = 2.0;
            n.noise_curve = NoiseBlendCurve::Smooth;
            n.noise_black = 0.12;
            n.noise_white = 0.88;
        }
        if let Some(n) = g.find_node_mut(pits) {
            n.label = "Pits / Flecks".into();
            // Sparse dark flecks (not honeycomb — high black point)
            n.noise_type = NoiseType::Cellular;
            n.projection = ProjectionStyle::Triplanar;
            n.input_defaults[1].value_f32 = 14.0;
            n.input_defaults[2].value_f32 = 2.0;
            n.input_defaults[6].value_f32 = 5.0;
            n.noise_curve = NoiseBlendCurve::Power;
            n.noise_curve_k = 2.2;
            n.noise_black = 0.55;
            n.noise_white = 1.0;
        }
        if let Some(n) = g.find_node_mut(micro) {
            n.label = "Micro Brush".into();
            n.noise_type = NoiseType::Fbm;
            n.projection = ProjectionStyle::Triplanar;
            n.input_defaults[1].value_f32 = 28.0;
            n.input_defaults[2].value_f32 = 3.0;
            n.input_defaults[5].value_f32 = 0.4;
            n.input_defaults[6].value_f32 = 9.0;
            n.noise_curve = NoiseBlendCurve::Linear;
        }
        if let Some(n) = g.find_node_mut(wear) {
            n.label = "Soft Wear".into();
            n.noise_type = NoiseType::Fbm;
            n.projection = ProjectionStyle::Triplanar;
            n.input_defaults[1].value_f32 = 2.2;
            n.input_defaults[2].value_f32 = 3.0;
            n.input_defaults[6].value_f32 = 13.0;
            n.noise_curve = NoiseBlendCurve::Levels;
            n.noise_curve_k = 0.95;
            n.noise_black = 0.35;
            n.noise_white = 0.95;
        }

        // ── Albedo ──────────────────────────────────────────────────
        let gold_ramp = g.add_node(NodeType::ColorRamp, [280.0, 40.0]);
        if let Some(r) = g.find_node_mut(gold_ramp) {
            r.label = "Bronze → Gold".into();
            // Dark antique bronze → warm bright gold (linear PBR metal F0 range)
            r.ramp_color0 = [0.28, 0.16, 0.05];
            r.ramp_color1 = [0.78, 0.55, 0.18];
            r.ramp_pos0 = 0.0;
            r.ramp_pos1 = 1.0;
        }

        let pit_col = g.add_node(NodeType::ConstantColor, [280.0, 200.0]);
        if let Some(c) = g.find_node_mut(pit_col) {
            c.label = "Pit Tint".into();
            // Dark oxidized flecks
            c.ramp_color0 = [0.12, 0.08, 0.04];
        }

        let wear_col = g.add_node(NodeType::ConstantColor, [280.0, 360.0]);
        if let Some(c) = g.find_node_mut(wear_col) {
            c.label = "Wear Tint".into();
            // Slightly duller gold in soft wear pockets
            c.ramp_color0 = [0.35, 0.22, 0.08];
        }

        let pit_amt = g.add_node(NodeType::MathBinary, [280.0, 280.0]);
        if let Some(m) = g.find_node_mut(pit_amt) {
            m.label = "Pits × 0.28".into();
            m.math_binary_op = MathBinaryOp::Multiply;
            m.input_defaults[1].value_f32 = 0.28;
        }
        let wear_amt = g.add_node(NodeType::MathBinary, [280.0, 440.0]);
        if let Some(m) = g.find_node_mut(wear_amt) {
            m.label = "Wear × 0.20".into();
            m.math_binary_op = MathBinaryOp::Multiply;
            m.input_defaults[1].value_f32 = 0.20;
        }

        let mix_pits = g.add_node(NodeType::MixRgb, [520.0, 80.0]);
        if let Some(m) = g.find_node_mut(mix_pits) {
            m.label = "Add Pits".into();
        }
        let mix_wear = g.add_node(NodeType::MixRgb, [720.0, 140.0]);
        if let Some(m) = g.find_node_mut(mix_wear) {
            m.label = "Add Wear".into();
        }

        // ── Roughness (matte brushed antique — not mirror) ──────────
        let r_polish = g.add_node(NodeType::ConstantValue, [520.0, 340.0]);
        if let Some(v) = g.find_node_mut(r_polish) {
            v.label = "R Polish".into();
            v.const_float = 0.32;
        }
        let r_micro = g.add_node(NodeType::ConstantValue, [520.0, 420.0]);
        if let Some(v) = g.find_node_mut(r_micro) {
            v.label = "R Micro".into();
            v.const_float = 0.55;
        }
        let r_pit = g.add_node(NodeType::ConstantValue, [520.0, 500.0]);
        if let Some(v) = g.find_node_mut(r_pit) {
            v.label = "R Pit".into();
            v.const_float = 0.72;
        }
        let mix_r1 = g.add_node(NodeType::MixFloat, [720.0, 380.0]);
        if let Some(m) = g.find_node_mut(mix_r1) {
            m.label = "Rough + Micro".into();
        }
        let mix_r2 = g.add_node(NodeType::MixFloat, [900.0, 420.0]);
        if let Some(m) = g.find_node_mut(mix_r2) {
            m.label = "Rough + Pits".into();
        }

        // ── Metallic ────────────────────────────────────────────────
        let m_clean = g.add_node(NodeType::ConstantValue, [520.0, 580.0]);
        if let Some(v) = g.find_node_mut(m_clean) {
            v.label = "Metal Clean".into();
            v.const_float = 0.96;
        }
        let m_worn = g.add_node(NodeType::ConstantValue, [520.0, 660.0]);
        if let Some(v) = g.find_node_mut(m_worn) {
            v.label = "Metal Worn".into();
            v.const_float = 0.82;
        }
        let mix_m = g.add_node(NodeType::MixFloat, [720.0, 600.0]);
        if let Some(m) = g.find_node_mut(mix_m) {
            m.label = "Metallic Blend".into();
        }

        // ── Height ──────────────────────────────────────────────────
        let h_pits = g.add_node(NodeType::MathBinary, [520.0, 740.0]);
        if let Some(m) = g.find_node_mut(h_pits) {
            m.label = "Pit Height".into();
            m.math_binary_op = MathBinaryOp::Multiply;
            m.input_defaults[1].value_f32 = 0.55;
        }
        let h_micro = g.add_node(NodeType::MathBinary, [520.0, 820.0]);
        if let Some(m) = g.find_node_mut(h_micro) {
            m.label = "Micro Height".into();
            m.math_binary_op = MathBinaryOp::Multiply;
            m.input_defaults[1].value_f32 = 0.22;
        }
        let h_add = g.add_node(NodeType::MathBinary, [720.0, 780.0]);
        if let Some(m) = g.find_node_mut(h_add) {
            m.label = "Height Sum".into();
            m.math_binary_op = MathBinaryOp::Add;
        }

        let out = g.add_node(NodeType::MaterialOutput, [1100.0, 280.0]);

        // Wire albedo
        g.connect(base, 0, gold_ramp, 0);
        g.connect(pits, 0, pit_amt, 0);
        g.connect(wear, 0, wear_amt, 0);

        g.connect(gold_ramp, 0, mix_pits, 1);
        g.connect(pit_col, 0, mix_pits, 2);
        g.connect(pit_amt, 0, mix_pits, 0);

        g.connect(mix_pits, 0, mix_wear, 1);
        g.connect(wear_col, 0, mix_wear, 2);
        g.connect(wear_amt, 0, mix_wear, 0);
        g.connect(mix_wear, 0, out, NodeGraph::MO_ALBEDO);

        // Roughness
        g.connect(r_polish, 0, mix_r1, 0);
        g.connect(r_micro, 0, mix_r1, 1);
        g.connect(micro, 0, mix_r1, 2);
        g.connect(mix_r1, 0, mix_r2, 0);
        g.connect(r_pit, 0, mix_r2, 1);
        g.connect(pits, 0, mix_r2, 2);
        g.connect(mix_r2, 0, out, NodeGraph::MO_ROUGHNESS);

        // Metallic
        g.connect(m_clean, 0, mix_m, 0);
        g.connect(m_worn, 0, mix_m, 1);
        g.connect(wear, 0, mix_m, 2);
        g.connect(mix_m, 0, out, NodeGraph::MO_METALLIC);

        // Height + Height→Normal
        g.connect(pits, 0, h_pits, 0);
        g.connect(micro, 0, h_micro, 0);
        g.connect(h_pits, 0, h_add, 0);
        g.connect(h_micro, 0, h_add, 1);
        g.connect(h_add, 0, out, NodeGraph::MO_HEIGHT);

        let h2n = g.add_node(NodeType::HeightToNormal, [900.0, 760.0]);
        if let Some(n) = g.find_node_mut(h2n) {
            n.label = "Height → Normal".into();
            n.input_defaults[1].value_f32 = 1.5;
            n.input_defaults[2].value_f32 = 0.015;
        }
        g.connect(h_add, 0, h2n, 0);
        g.connect(h2n, 0, out, NodeGraph::MO_NORMAL);

        g
    }
}
