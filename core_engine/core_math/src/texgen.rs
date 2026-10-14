// core_engine/core_math/src/texgen.rs
// Procedural Tileable PBR Texture Generator
// Zero AI dependencies — pure math on CPU
// Generates: Albedo, Normal, Roughness, Metalness, Height maps
// Export: TGA (universal 3D pipeline format)

use std::f32::consts::TAU;

// ============================================================
// Configuration
// ============================================================

#[derive(Clone, Debug)]
pub enum TexPattern {
    Scratches {
        density: f32,      // 0.1-1.0
        depth: f32,        // 0.0-1.0
        direction: f32,    // degrees
        length: f32,       // 0.1-1.0
        width: f32,        // 0.001-0.02
    },
    Rust {
        amount: f32,       // 0.0-1.0
        pitting: f32,      // 0.0-1.0
        color_var: f32,    // 0.0-1.0
    },
    BrushedMetal {
        direction: f32,    // degrees
        fineness: f32,     // 1.0-50.0
        depth: f32,        // 0.0-1.0
    },
    Noise {
        octaves: u32,      // 1-8
        scale: f32,        // 1.0-20.0
        contrast: f32,     // 0.5-2.0
    },
    VoronoiCracks {
        cell_scale: f32,   // 2.0-20.0
        edge_width: f32,   // 0.01-0.1
        depth: f32,        // 0.0-1.0
    },
    WornPaint {
        wear: f32,         // 0.0-1.0
        paint_color: [f32; 3],
        under_color: [f32; 3],
        chip_scale: f32,   // 1.0-10.0
    },
}

#[derive(Clone, Debug)]
pub struct TexGenConfig {
    pub width: u32,
    pub height: u32,
    pub pattern: TexPattern,
    pub base_color: [f32; 3],
    pub base_roughness: f32,
    pub base_metalness: f32,
    pub normal_strength: f32,
    pub seed: u32,
}

impl Default for TexGenConfig {
    fn default() -> Self {
        Self {
            width: 2048, height: 2048,
            pattern: TexPattern::Scratches {
                density: 0.5, depth: 0.6, direction: 45.0, length: 0.4, width: 0.005,
            },
            base_color: [0.5, 0.5, 0.5],
            base_roughness: 0.4, base_metalness: 0.9,
            normal_strength: 1.5, seed: 42,
        }
    }
}

// ============================================================
// Output Maps
// ============================================================

pub struct PbrMaps {
    pub albedo: Vec<[u8; 3]>,
    pub normal: Vec<[u8; 3]>,
    pub roughness: Vec<u8>,
    pub metalness: Vec<u8>,
    pub height: Vec<u8>,
    pub width: u32,
    pub height_px: u32,
}

// ============================================================
// Noise Primitives (Tileable)
// ============================================================

fn hash21(ix: i32, iy: i32, seed: u32) -> f32 {
    let mut h = (ix as u32).wrapping_mul(374761393)
        .wrapping_add((iy as u32).wrapping_mul(668265263))
        .wrapping_add(seed.wrapping_mul(1274126177));
    h = (h ^ (h >> 13)).wrapping_mul(1103515245);
    h = h ^ (h >> 16);
    (h & 0x7FFFFFFF) as f32 / 0x7FFFFFFF as f32
}

fn hash22(ix: i32, iy: i32, seed: u32) -> (f32, f32) {
    (hash21(ix, iy, seed), hash21(ix, iy, seed.wrapping_add(7919)))
}

fn value_noise_tile(x: f32, y: f32, period: i32, seed: u32) -> f32 {
    let ix = x.floor() as i32;
    let iy = y.floor() as i32;
    let fx = x - x.floor();
    let fy = y - y.floor();
    let sx = fx * fx * (3.0 - 2.0 * fx);
    let sy = fy * fy * (3.0 - 2.0 * fy);
    let wrap = |v: i32| ((v % period) + period) % period;
    let c00 = hash21(wrap(ix), wrap(iy), seed);
    let c10 = hash21(wrap(ix + 1), wrap(iy), seed);
    let c01 = hash21(wrap(ix), wrap(iy + 1), seed);
    let c11 = hash21(wrap(ix + 1), wrap(iy + 1), seed);
    let a = c00 + (c10 - c00) * sx;
    let b = c01 + (c11 - c01) * sx;
    a + (b - a) * sy
}

fn fbm_tile(x: f32, y: f32, octaves: u32, base_period: i32, seed: u32) -> f32 {
    let mut value = 0.0f32;
    let mut amplitude = 0.5f32;
    let mut freq = 1.0f32;
    let mut period = base_period;
    for i in 0..octaves.min(8) {
        value += amplitude * value_noise_tile(x * freq, y * freq, period, seed.wrapping_add(i * 31337));
        amplitude *= 0.5;
        freq *= 2.0;
        period *= 2;
    }
    value
}

fn voronoi_tile(x: f32, y: f32, period: i32, seed: u32) -> (f32, f32) {
    let ix = x.floor() as i32;
    let iy = y.floor() as i32;
    let fx = x - x.floor();
    let fy = y - y.floor();
    let wrap = |v: i32| ((v % period) + period) % period;
    let mut min_d = 999.0f32;
    let mut min_d2 = 999.0f32;
    for dy in -1..=1 {
        for dx in -1..=1 {
            let (ox, oy) = hash22(wrap(ix + dx), wrap(iy + dy), seed);
            let vx = dx as f32 + ox - fx;
            let vy = dy as f32 + oy - fy;
            let d = (vx * vx + vy * vy).sqrt();
            if d < min_d { min_d2 = min_d; min_d = d; }
            else if d < min_d2 { min_d2 = d; }
        }
    }
    (min_d, min_d2 - min_d)
}

fn dist_to_segment(px: f32, py: f32, ax: f32, ay: f32, bx: f32, by: f32) -> f32 {
    let dx = bx - ax;
    let dy = by - ay;
    let len_sq = dx * dx + dy * dy;
    if len_sq < 1e-10 { return ((px - ax).powi(2) + (py - ay).powi(2)).sqrt(); }
    let t = (((px - ax) * dx + (py - ay) * dy) / len_sq).clamp(0.0, 1.0);
    ((px - ax - t * dx).powi(2) + (py - ay - t * dy).powi(2)).sqrt()
}

// ============================================================
// Pattern Generators → height map [0,1] (0=deep, 1=surface)
// ============================================================

fn gen_scratches(w: u32, h: u32, density: f32, depth: f32, dir_deg: f32, length: f32, width: f32, seed: u32) -> Vec<f32> {
    let n = (density * 200.0) as usize;
    let dir_rad = dir_deg * TAU / 360.0;
    let len = length * 0.5;
    let half_w = width * 0.5;
    let mut scratches = Vec::with_capacity(n);
    for i in 0..n {
        let cx = hash21(i as i32, 0, seed);
        let cy = hash21(i as i32, 1, seed);
        let angle = dir_rad + (hash21(i as i32, 2, seed) - 0.5) * 1.0;
        let l = len * (0.5 + hash21(i as i32, 3, seed) * 0.5);
        let (dx, dy) = (angle.cos() * l, angle.sin() * l);
        scratches.push((cx - dx, cy - dy, cx + dx, cy + dy));
    }
    let mut heights = vec![1.0f32; (w * h) as usize];
    for py in 0..h {
        let v = py as f32 / h as f32;
        for px in 0..w {
            let u = px as f32 / w as f32;
            let mut min_dist = 1.0f32;
            for &(x0, y0, x1, y1) in &scratches {
                for ty in -1i32..=1 { for tx in -1i32..=1 {
                    let d = dist_to_segment(u, v, x0+tx as f32, y0+ty as f32, x1+tx as f32, y1+ty as f32);
                    min_dist = min_dist.min(d);
                }}
            }
            let sv = (1.0 - (min_dist / half_w).min(1.0)).max(0.0);
            heights[(py * w + px) as usize] = 1.0 - sv * sv * depth;
        }
    }
    heights
}

fn gen_rust(w: u32, h: u32, amount: f32, pitting: f32, _cv: f32, seed: u32) -> Vec<f32> {
    let bp = 8i32;
    let mut heights = vec![1.0f32; (w * h) as usize];
    for py in 0..h { let v = py as f32 / h as f32;
        for px in 0..w { let u = px as f32 / w as f32;
            let patch = fbm_tile(u * 6.0, v * 6.0, 5, bp * 6, seed);
            let rust_mask = ((patch - (1.0 - amount) * 0.5) * 3.0).clamp(0.0, 1.0);
            let (cell_d, _) = voronoi_tile(u * 15.0, v * 15.0, 15, seed.wrapping_add(100));
            let pit = (1.0 - cell_d * 3.0).max(0.0) * pitting;
            let detail = fbm_tile(u * 25.0, v * 25.0, 3, bp * 25, seed.wrapping_add(200));
            heights[(py * w + px) as usize] = (1.0 - rust_mask * (pit * 0.6 + detail * 0.3 + 0.1)).clamp(0.0, 1.0);
        }
    }
    heights
}

fn gen_brushed(w: u32, h: u32, dir_deg: f32, fineness: f32, depth: f32, seed: u32) -> Vec<f32> {
    let dr = dir_deg * TAU / 360.0;
    let (cd, sd) = (dr.cos(), dr.sin());
    let period = (fineness as i32).max(4);
    let mut heights = vec![1.0f32; (w * h) as usize];
    for py in 0..h { let v = py as f32 / h as f32;
        for px in 0..w { let u = px as f32 / w as f32;
            let along = u * cd + v * sd;
            let across = -u * sd + v * cd;
            let groove = value_noise_tile(across * fineness, along * 0.5, period, seed);
            let fine = value_noise_tile(across * fineness * 3.0, along * 2.0, period * 3, seed.wrapping_add(50));
            heights[(py * w + px) as usize] = (1.0 - (groove * 0.7 + fine * 0.3) * depth).clamp(0.0, 1.0);
        }
    }
    heights
}

fn gen_noise(w: u32, h: u32, octaves: u32, scale: f32, contrast: f32, seed: u32) -> Vec<f32> {
    let period = (scale as i32).max(2);
    (0..h).flat_map(|py| { let v = py as f32 / h as f32;
        (0..w).map(move |px| { let u = px as f32 / w as f32;
            ((fbm_tile(u * scale, v * scale, octaves, period, seed) - 0.5) * contrast + 0.5).clamp(0.0, 1.0)
        })
    }).collect()
}

fn gen_voronoi_cracks(w: u32, h: u32, cell_scale: f32, edge_width: f32, depth: f32, seed: u32) -> Vec<f32> {
    let period = (cell_scale as i32).max(2);
    (0..h).flat_map(|py| { let v = py as f32 / h as f32;
        (0..w).map(move |px| { let u = px as f32 / w as f32;
            let (_, ed) = voronoi_tile(u * cell_scale, v * cell_scale, period, seed);
            let crack = (1.0 - (ed / edge_width).min(1.0)).max(0.0);
            1.0 - crack * crack * depth
        })
    }).collect()
}

fn gen_worn_paint(w: u32, h: u32, wear: f32, chip_scale: f32, seed: u32) -> Vec<f32> {
    let period = (chip_scale as i32).max(2);
    (0..h).flat_map(|py| { let v = py as f32 / h as f32;
        (0..w).map(move |px| { let u = px as f32 / w as f32;
            let n1 = fbm_tile(u * chip_scale, v * chip_scale, 4, period, seed);
            let n2 = fbm_tile(u * chip_scale * 2.5, v * chip_scale * 2.5, 3, period * 2, seed.wrapping_add(77));
            let combined = n1 * 0.7 + n2 * 0.3;
            let threshold = 0.3 + wear * 0.45;
            ((combined - threshold) / 0.03).clamp(0.0, 1.0)
        })
    }).collect()
}

// ============================================================
// Height → Normal Map (Sobel, tileable wrapping)
// ============================================================

fn height_to_normal(heights: &[f32], w: u32, h: u32, strength: f32) -> Vec<[f32; 3]> {
    let (ww, hh) = (w as i32, h as i32);
    (0..hh).flat_map(|py| {
        (0..ww).map(move |px| {
            let s = |dx: i32, dy: i32| -> f32 {
                heights[(((py+dy)%hh+hh)%hh * ww + ((px+dx)%ww+ww)%ww) as usize]
            };
            let dx = (s(1,-1) + 2.0*s(1,0) + s(1,1)) - (s(-1,-1) + 2.0*s(-1,0) + s(-1,1));
            let dy = (s(-1,1) + 2.0*s(0,1) + s(1,1)) - (s(-1,-1) + 2.0*s(0,-1) + s(1,-1));
            let (nx, ny, nz) = (-dx * strength, -dy * strength, 1.0);
            let len = (nx*nx + ny*ny + nz*nz).sqrt();
            [nx/len*0.5+0.5, ny/len*0.5+0.5, nz/len*0.5+0.5]
        })
    }).collect()
}

// ============================================================
// Main Generation Entry Point
// ============================================================

pub fn generate_pbr_maps(config: &TexGenConfig) -> PbrMaps {
    let (w, h, seed) = (config.width, config.height, config.seed);
    let n = (w * h) as usize;

    let heights = match &config.pattern {
        TexPattern::Scratches { density, depth, direction, length, width } =>
            gen_scratches(w, h, *density, *depth, *direction, *length, *width, seed),
        TexPattern::Rust { amount, pitting, color_var } =>
            gen_rust(w, h, *amount, *pitting, *color_var, seed),
        TexPattern::BrushedMetal { direction, fineness, depth } =>
            gen_brushed(w, h, *direction, *fineness, *depth, seed),
        TexPattern::Noise { octaves, scale, contrast } =>
            gen_noise(w, h, *octaves, *scale, *contrast, seed),
        TexPattern::VoronoiCracks { cell_scale, edge_width, depth } =>
            gen_voronoi_cracks(w, h, *cell_scale, *edge_width, *depth, seed),
        TexPattern::WornPaint { wear, chip_scale, .. } =>
            gen_worn_paint(w, h, *wear, *chip_scale, seed),
    };

    let normals_f = height_to_normal(&heights, w, h, config.normal_strength);

    let mut albedo = vec![[0u8; 3]; n];
    let mut roughness_out = vec![0u8; n];
    let mut metalness_out = vec![0u8; n];
    let mut height_out = vec![0u8; n];
    let mut normal_out = vec![[0u8; 3]; n];

    let to_u8 = |v: f32| (v.clamp(0.0, 1.0) * 255.0) as u8;

    for i in 0..n {
        let hv = heights[i];
        let (mut r, mut g, mut b) = (config.base_color[0], config.base_color[1], config.base_color[2]);
        let (mut rough, mut metal) = (config.base_roughness, config.base_metalness);

        match &config.pattern {
            TexPattern::Scratches { .. } => {
                let sc = 1.0 - hv;
                r += sc * 0.15; g += sc * 0.15; b += sc * 0.15;
                rough += sc * 0.3;
            }
            TexPattern::Rust { color_var, .. } => {
                let rust = 1.0 - hv;
                let rv = 0.55 + hash21((i % w as usize) as i32, (i / w as usize) as i32, seed+500) * color_var * 0.2;
                r = r*(1.0-rust) + rv*rust; g = g*(1.0-rust) + 0.22*rust; b = b*(1.0-rust) + 0.06*rust;
                rough = rough*(1.0-rust) + 0.95*rust; metal = metal*(1.0-rust) + 0.05*rust;
            }
            TexPattern::BrushedMetal { .. } => { rough += (1.0 - hv) * 0.2; }
            TexPattern::Noise { .. } => { rough += (hv - 0.5) * 0.3; }
            TexPattern::VoronoiCracks { .. } => {
                let cr = 1.0 - hv;
                r *= 1.0-cr*0.5; g *= 1.0-cr*0.5; b *= 1.0-cr*0.5; rough += cr*0.4;
            }
            TexPattern::WornPaint { paint_color, under_color, .. } => {
                r = under_color[0]*(1.0-hv) + paint_color[0]*hv;
                g = under_color[1]*(1.0-hv) + paint_color[1]*hv;
                b = under_color[2]*(1.0-hv) + paint_color[2]*hv;
                rough = 0.8*(1.0-hv) + 0.4*hv;
                metal = 0.9*(1.0-hv) + 0.0*hv;
            }
        }

        albedo[i] = [to_u8(r), to_u8(g), to_u8(b)];
        roughness_out[i] = to_u8(rough.clamp(0.0, 1.0));
        metalness_out[i] = to_u8(metal.clamp(0.0, 1.0));
        height_out[i] = to_u8(hv);
        normal_out[i] = [to_u8(normals_f[i][0]), to_u8(normals_f[i][1]), to_u8(normals_f[i][2])];
    }

    PbrMaps { albedo, normal: normal_out, roughness: roughness_out, metalness: metalness_out,
              height: height_out, width: w, height_px: h }
}

// ============================================================
// TGA Export (zero dependencies, universal 3D format)
// ============================================================

fn write_tga_header(f: &mut std::fs::File, w: u32, h: u32, bpp: u8) -> Result<(), String> {
    use std::io::Write;
    let header: [u8; 18] = [
        0, 0, if bpp == 8 { 3 } else { 2 }, 0,0,0,0,0, 0,0,0,0,
        (w & 0xFF) as u8, ((w >> 8) & 0xFF) as u8,
        (h & 0xFF) as u8, ((h >> 8) & 0xFF) as u8,
        bpp, 0x20,
    ];
    f.write_all(&header).map_err(|e| e.to_string())
}

fn save_tga_rgb(path: &str, data: &[[u8; 3]], w: u32, h: u32) -> Result<(), String> {
    use std::io::Write;
    let mut f = std::fs::File::create(path).map_err(|e| format!("{}: {}", path, e))?;
    write_tga_header(&mut f, w, h, 24)?;
    let mut buf = vec![0u8; (w * h * 3) as usize];
    for (i, px) in data.iter().enumerate() {
        buf[i*3] = px[2]; buf[i*3+1] = px[1]; buf[i*3+2] = px[0]; // TGA = BGR
    }
    f.write_all(&buf).map_err(|e| e.to_string())
}

fn save_tga_gray(path: &str, data: &[u8], w: u32, h: u32) -> Result<(), String> {
    use std::io::Write;
    let mut f = std::fs::File::create(path).map_err(|e| format!("{}: {}", path, e))?;
    write_tga_header(&mut f, w, h, 8)?;
    f.write_all(data).map_err(|e| e.to_string())
}

pub fn export_pbr_maps(maps: &PbrMaps, prefix: &str) -> Result<Vec<String>, String> {
    let (w, h) = (maps.width, maps.height_px);
    let mut paths = Vec::new();
    let names = ["albedo", "normal", "roughness", "metalness", "height"];
    for name in &names {
        let p = format!("{}_{}.tga", prefix, name);
        match *name {
            "albedo" => save_tga_rgb(&p, &maps.albedo, w, h)?,
            "normal" => save_tga_rgb(&p, &maps.normal, w, h)?,
            "roughness" => save_tga_gray(&p, &maps.roughness, w, h)?,
            "metalness" => save_tga_gray(&p, &maps.metalness, w, h)?,
            "height" => save_tga_gray(&p, &maps.height, w, h)?,
            _ => {}
        }
        paths.push(p);
    }
    Ok(paths)
}

// ============================================================
// Presets
// ============================================================

impl TexGenConfig {
    pub fn preset_scratched_metal() -> Self {
        Self { pattern: TexPattern::Scratches { density:0.6, depth:0.7, direction:30.0, length:0.35, width:0.004 },
               base_color: [0.7,0.7,0.72], base_roughness:0.25, base_metalness:0.95, normal_strength:2.0, ..Self::default() }
    }
    pub fn preset_rusty_iron() -> Self {
        Self { pattern: TexPattern::Rust { amount:0.6, pitting:0.7, color_var:0.5 },
               base_color: [0.15,0.12,0.1], base_roughness:0.4, base_metalness:0.8, normal_strength:1.5, ..Self::default() }
    }
    pub fn preset_brushed_steel() -> Self {
        Self { pattern: TexPattern::BrushedMetal { direction:0.0, fineness:20.0, depth:0.5 },
               base_color: [0.75,0.75,0.78], base_roughness:0.2, base_metalness:0.95, normal_strength:1.0, ..Self::default() }
    }
    pub fn preset_cracked_stone() -> Self {
        Self { pattern: TexPattern::VoronoiCracks { cell_scale:8.0, edge_width:0.04, depth:0.8 },
               base_color: [0.35,0.32,0.28], base_roughness:0.85, base_metalness:0.0, normal_strength:2.0, ..Self::default() }
    }
    pub fn preset_worn_military() -> Self {
        Self { pattern: TexPattern::WornPaint { wear:0.5, paint_color:[0.15,0.2,0.1], under_color:[0.3,0.28,0.25], chip_scale:5.0 },
               base_color: [0.15,0.2,0.1], base_roughness:0.5, base_metalness:0.0, normal_strength:1.5, ..Self::default() }
    }
    pub fn preset_names() -> &'static [(&'static str, &'static str)] {
        &[("scratched_metal","🔪 Scratched Metal"),("rusty_iron","🔧 Rusty Iron"),
          ("brushed_steel","✨ Brushed Steel"),("cracked_stone","🪨 Cracked Stone"),
          ("worn_military","🎖 Worn Military Paint")]
    }
    pub fn from_preset(name: &str) -> Self {
        match name {
            "scratched_metal" => Self::preset_scratched_metal(),
            "rusty_iron" => Self::preset_rusty_iron(),
            "brushed_steel" => Self::preset_brushed_steel(),
            "cracked_stone" => Self::preset_cracked_stone(),
            "worn_military" => Self::preset_worn_military(),
            _ => Self::default(),
        }
    }
}
