//! Image-based lighting CPU core — split-sum IBL for the PBR viewport (2026-07-18).
//!
//! The environment lives in ONE storage buffer (binding 21, `env_buffer`) with the layout the
//! shaders have always used: `[L0: w*h*3] [L1: w/2*h/2*3] … [L6]`, now followed by 27 floats of
//! pre-convolved SH9 irradiance. Mip semantics changed from "Gaussian blur" to the split-sum
//! FIRST term: mips 1..6 are GGX-prefiltered on the GPU (`env_prefilter.wgsl`, roughness = mip/6,
//! one mip per poll frame — Frame Budget). This module owns everything CPU-side:
//!   - box-downsampled placeholder mip chain (uploaded immediately so nothing renders black
//!     while the GPU prefilter runs over the next 6 frames)
//!   - SH9 irradiance projection (cosine-convolved; the split-sum diffuse term)
//!   - the split-sum SECOND term: the GGX BRDF LUT (generated once on a startup worker thread,
//!     uploaded to `brdf_lut_buffer`, deferred binding 39)
//!   - env file decode (.hdr via the existing `parse_hdr`, .exr/LDR via the `image` crate)
//!   - the env-load worker + poll-side install (Frame Ordering: all GPU mutations in poll)
//!
//! Mip-dimension convention is EXACTLY the WGSL `sample_env_at_level` loop:
//! `w_{k+1} = max(w_k / 2, 1)` for all 7 levels (no early stop — the old CPU chain stopped at
//! dims < 4, leaving the shader to index past the buffer for small maps; this chain always
//! produces what the shader addresses).

use crate::*;
use eframe::wgpu;

/// Must match `ENV_MIP_LEVELS` in `sdf_lighting.wgsl`.
pub(crate) const ENV_MIP_LEVELS: u32 = 7;
/// BRDF LUT resolution (x = N·V, y = roughness). Fed to the shader via
/// `GpuUniforms.env_lut_res` — 0 until the worker upload lands (shader uses the
/// analytic Karis approximation meanwhile, so there is never a black/wrong gap).
pub(crate) const BRDF_LUT_RES: u32 = 128;
/// GGX importance samples per LUT texel. The integrand is smooth; 256 Hammersley
/// samples put the LUT within ~1e-3 of a 4096-sample reference.
pub(crate) const BRDF_LUT_SAMPLES: u32 = 256;
/// Hard cap on accepted environment width (8K equirect = 384 MB of f32 mips — beyond
/// this is disproportionate for a viewport IBL source). User-generated input guard.
pub(crate) const ENV_MAX_WIDTH: u32 = 8192;

// ============================================================
// Mip chain (box placeholder — GPU GGX prefilter overwrites 1..6)
// ============================================================

/// Dimensions of mip `level` for a `w × h` level 0, matching the WGSL offset loop.
pub(crate) fn env_mip_dims(w: u32, h: u32, level: u32) -> (u32, u32) {
    let mut mw = w;
    let mut mh = h;
    for _ in 0..level {
        mw = (mw / 2).max(1);
        mh = (mh / 2).max(1);
    }
    (mw, mh)
}

/// Float count of the full 7-level mip chain (RGB triplets).
pub(crate) fn env_chain_len_floats(w: u32, h: u32) -> usize {
    (0..ENV_MIP_LEVELS)
        .map(|l| {
            let (mw, mh) = env_mip_dims(w, h, l);
            (mw * mh * 3) as usize
        })
        .sum()
}

// ============================================================
// Section K: env luminance CDF for PT importance sampling
// ============================================================
// Buffer layout (f32):
//   [0]=width [1]=height [2]=total_weight [3]=valid (1/0)
//   [4 .. 4+h) = marginal_cdf[h]   (ends ~1)
//   [4+h .. 4+h+h*w) = row_cdf[h*w]  (each row ends ~1)
// Cap resolution so 8K HDRIs don't allocate multi-MB CDFs.

/// Max CDF width/height (downsample source if larger).
pub(crate) const ENV_CDF_MAX_W: u32 = 512;
pub(crate) const ENV_CDF_MAX_H: u32 = 256;

/// Empty/invalid CDF header (always-bindable dummy for PT).
pub(crate) fn empty_env_cdf() -> Vec<f32> {
    vec![0.0, 0.0, 0.0, 0.0]
}

/// Build a luminance×sinθ CDF from equirect level-0 RGB floats.
/// `level0` length must be ≥ w*h*3.
pub(crate) fn build_env_luminance_cdf(level0: &[f32], w: u32, h: u32) -> Vec<f32> {
    if w == 0 || h == 0 || level0.len() < (w * h * 3) as usize {
        return empty_env_cdf();
    }
    // Downsample to CDF cap (box average of source texels).
    let cw = w.min(ENV_CDF_MAX_W).max(1);
    let ch = h.min(ENV_CDF_MAX_H).max(1);
    let mut func = vec![0.0f32; (cw * ch) as usize];
    let mut total = 0.0f32;
    for cy in 0..ch {
        let y0 = (cy * h) / ch;
        let y1 = (((cy + 1) * h) / ch).max(y0 + 1).min(h);
        let theta = std::f32::consts::PI * (cy as f32 + 0.5) / ch as f32;
        let sin_theta = theta.sin().max(1e-5);
        for cx in 0..cw {
            let x0 = (cx * w) / cw;
            let x1 = (((cx + 1) * w) / cw).max(x0 + 1).min(w);
            let mut sum = 0.0f32;
            let mut n = 0u32;
            for y in y0..y1 {
                for x in x0..x1 {
                    let i = ((y * w + x) * 3) as usize;
                    let r = level0[i];
                    let g = level0[i + 1];
                    let b = level0[i + 2];
                    sum += 0.2126 * r + 0.7152 * g + 0.0722 * b;
                    n += 1;
                }
            }
            let lum = (sum / n.max(1) as f32).max(0.0);
            let wgt = lum * sin_theta;
            func[(cy * cw + cx) as usize] = wgt;
            total += wgt;
        }
    }
    if total < 1e-8 {
        return empty_env_cdf();
    }

    // Marginal + conditional CDFs
    let mut marginal = vec![0.0f32; ch as usize];
    let mut row_cdf = vec![0.0f32; (cw * ch) as usize];
    let mut marg_acc = 0.0f32;
    for y in 0..ch as usize {
        let row = &func[y * cw as usize..(y + 1) * cw as usize];
        let row_sum: f32 = row.iter().sum();
        // Conditional CDF for this row
        let mut acc = 0.0f32;
        if row_sum > 1e-12 {
            for x in 0..cw as usize {
                acc += row[x] / row_sum;
                row_cdf[y * cw as usize + x] = acc;
            }
            // Force last to 1
            row_cdf[y * cw as usize + cw as usize - 1] = 1.0;
        } else {
            for x in 0..cw as usize {
                row_cdf[y * cw as usize + x] = (x + 1) as f32 / cw as f32;
            }
        }
        marg_acc += row_sum / total;
        marginal[y] = marg_acc;
    }
    if let Some(last) = marginal.last_mut() {
        *last = 1.0;
    }

    let mut out = Vec::with_capacity(4 + ch as usize + (cw * ch) as usize);
    out.push(cw as f32);
    out.push(ch as f32);
    out.push(total);
    out.push(1.0); // valid
    out.extend_from_slice(&marginal);
    out.extend_from_slice(&row_cdf);
    out
}

/// Upload CDF into `pt_env_cdf_buffer` and invalidate PT bind groups.
pub(crate) fn upload_pt_env_cdf(gpu: &mut crate::GpuCtx, cdf: &[f32]) {
    let bytes: Vec<u8> = cdf.iter().flat_map(|v| v.to_le_bytes()).collect();
    let size = (bytes.len() as u64).max(16);
    gpu.buffers.pt_env_cdf_buffer = Some(gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("pt_env_cdf"),
        size,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    }));
    if let Some(ref buf) = gpu.buffers.pt_env_cdf_buffer {
        if !bytes.is_empty() {
            gpu.queue.write_buffer(buf, 0, &bytes);
        }
    }
    gpu.bind_groups.pt_bg = None;
    gpu.bind_groups.pt_bg_half = None;
}

/// 2×2 box downsample of an RGB f32 equirect level (odd dims clamp the second tap).
fn box_downsample(src: &[f32], w: u32, h: u32) -> (Vec<f32>, u32, u32) {
    let nw = (w / 2).max(1);
    let nh = (h / 2).max(1);
    let mut out = vec![0.0f32; (nw * nh * 3) as usize];
    for y in 0..nh {
        for x in 0..nw {
            let sx0 = (x * 2).min(w - 1) as usize;
            let sy0 = (y * 2).min(h - 1) as usize;
            let sx1 = (x * 2 + 1).min(w - 1) as usize;
            let sy1 = (y * 2 + 1).min(h - 1) as usize;
            for c in 0..3usize {
                let s = src[(sy0 * w as usize + sx0) * 3 + c]
                    + src[(sy0 * w as usize + sx1) * 3 + c]
                    + src[(sy1 * w as usize + sx0) * 3 + c]
                    + src[(sy1 * w as usize + sx1) * 3 + c];
                out[(y as usize * nw as usize + x as usize) * 3 + c] = s * 0.25;
            }
        }
    }
    (out, nw, nh)
}

/// CPU-side environment package: the full placeholder chain + SH9 irradiance,
/// laid out exactly as the GPU buffer expects (`[chain][sh9]`).
pub(crate) struct EnvCpuData {
    /// `[L0 .. L6]` concatenated RGB floats (L0 = source, 1..6 = box placeholders).
    pub(crate) chain: Vec<f32>,
    /// 27 floats: pre-convolved irradiance SH9 (rgb-interleaved per coefficient).
    /// `Σ sh[i] · Y_i(n)` yields the diffuse env term directly (same scale as the
    /// radiance the old last-mip lookup produced).
    pub(crate) sh9: [f32; 27],
}

/// Build the placeholder chain + SH9 from a decoded level 0.
pub(crate) fn build_env_cpu(level0: &[f32], w: u32, h: u32) -> EnvCpuData {
    assert_eq!(level0.len(), (w * h * 3) as usize, "env level0 size mismatch");
    let mut chain = Vec::with_capacity(env_chain_len_floats(w, h));
    chain.extend_from_slice(level0);
    let mut prev = level0.to_vec();
    let (mut cw, mut ch) = (w, h);
    let mut sh_src: Option<(Vec<f32>, u32, u32)> = None;
    for level in 1..ENV_MIP_LEVELS {
        let (down, nw, nh) = box_downsample(&prev, cw, ch);
        chain.extend_from_slice(&down);
        prev = down;
        cw = nw;
        ch = nh;
        // Project SH from the first level at or below ~64k texels (cheap + already prefiltered
        // enough that the 9-coefficient fit loses nothing).
        if sh_src.is_none() && (cw * ch) <= 65536 {
            sh_src = Some((prev.clone(), cw, ch));
        }
        let _ = level;
    }
    let (sh_pixels, sw, shh) = sh_src.unwrap_or_else(|| (prev.clone(), cw, ch));
    let sh9 = project_sh9(&sh_pixels, sw, shh);
    EnvCpuData { chain, sh9 }
}

// ============================================================
// SH9 irradiance (split-sum diffuse term)
// ============================================================

/// Real SH basis constants (bands 0..2), matching `env_sh_irradiance` in sdf_lighting.wgsl.
const SH_C0: f32 = 0.282095; // Y00
const SH_C1: f32 = 0.488603; // Y1-1, Y10, Y11
const SH_C2: f32 = 1.092548; // Y2-2, Y2-1, Y21
const SH_C3: f32 = 0.315392; // Y20
const SH_C4: f32 = 0.546274; // Y22

fn sh_basis(d: [f32; 3]) -> [f32; 9] {
    let (x, y, z) = (d[0], d[1], d[2]);
    [
        SH_C0,
        SH_C1 * y,
        SH_C1 * z,
        SH_C1 * x,
        SH_C2 * x * y,
        SH_C2 * y * z,
        SH_C3 * (3.0 * z * z - 1.0),
        SH_C2 * x * z,
        SH_C4 * (x * x - y * y),
    ]
}

/// Project an RGB equirect radiance map onto SH9 and convolve with the clamped-cosine
/// kernel, normalized so `eval_sh9` returns the cosine-weighted average incoming
/// radiance (a uniform environment of value V yields exactly V for every normal —
/// the same scale the old "sample the blurriest mip" diffuse term had).
pub(crate) fn project_sh9(pixels: &[f32], w: u32, h: u32) -> [f32; 27] {
    let mut acc = [0.0f64; 27];
    let mut weight_sum = 0.0f64;
    let (wf, hf) = (w as f32, h as f32);
    for y in 0..h as usize {
        // Equirect texel solid angle ∝ sin(theta); constants cancel in the normalization.
        let v = (y as f32 + 0.5) / hf;
        let theta = v * std::f32::consts::PI;
        let sin_t = theta.sin().max(0.0);
        if sin_t <= 0.0 {
            continue;
        }
        for x in 0..w as usize {
            let u = (x as f32 + 0.5) / wf;
            // Matches the WGSL mapping: u = (phi + PI) / 2PI, v = theta / PI,
            // dir = (cos(phi)·sinθ, cosθ, sin(phi)·sinθ).
            let phi = u * 2.0 * std::f32::consts::PI - std::f32::consts::PI;
            let dir = [phi.cos() * sin_t, theta.cos(), phi.sin() * sin_t];
            let basis = sh_basis(dir);
            let idx = (y * w as usize + x) * 3;
            let dw = sin_t as f64;
            weight_sum += dw;
            for i in 0..9 {
                let b = basis[i] as f64 * dw;
                acc[i * 3] += pixels[idx] as f64 * b;
                acc[i * 3 + 1] += pixels[idx + 1] as f64 * b;
                acc[i * 3 + 2] += pixels[idx + 2] as f64 * b;
            }
        }
    }
    if weight_sum <= 0.0 {
        return [0.0; 27];
    }
    // Radiance coefficients L_i (basis-orthonormality: L_i = ∫ L·Y_i dω, with
    // dω = 4π · sinθ / Σ sinθ over the equirect grid).
    let domega = 4.0 * std::f64::consts::PI / weight_sum;
    let mut l = [0.0f32; 27];
    for i in 0..27 {
        l[i] = (acc[i] * domega) as f32;
    }
    // Cosine convolution Â_l / π → irradiance/π = cosine-weighted average radiance.
    const A0: f32 = 1.0; //  π/π
    const A1: f32 = 2.0 / 3.0; // (2π/3)/π
    const A2: f32 = 0.25; //  (π/4)/π
    let band = [A0, A1, A1, A1, A2, A2, A2, A2, A2];
    let mut out = [0.0f32; 27];
    for i in 0..9 {
        for c in 0..3 {
            out[i * 3 + c] = l[i * 3 + c] * band[i];
        }
    }
    out
}

/// CPU mirror of the WGSL `env_sh_irradiance` (tests + parity).
pub(crate) fn eval_sh9(sh: &[f32; 27], n: [f32; 3]) -> [f32; 3] {
    let basis = sh_basis(n);
    let mut out = [0.0f32; 3];
    for i in 0..9 {
        for c in 0..3 {
            out[c] += sh[i * 3 + c] * basis[i];
        }
    }
    [out[0].max(0.0), out[1].max(0.0), out[2].max(0.0)]
}

// ============================================================
// BRDF LUT (split-sum second term)
// ============================================================

fn radical_inverse_vdc(mut bits: u32) -> f32 {
    bits = (bits << 16) | (bits >> 16);
    bits = ((bits & 0x5555_5555) << 1) | ((bits & 0xAAAA_AAAA) >> 1);
    bits = ((bits & 0x3333_3333) << 2) | ((bits & 0xCCCC_CCCC) >> 2);
    bits = ((bits & 0x0F0F_0F0F) << 4) | ((bits & 0xF0F0_F0F0) >> 4);
    bits = ((bits & 0x00FF_00FF) << 8) | ((bits & 0xFF00_FF00) >> 8);
    bits as f32 * 2.328_306_4e-10
}

/// Karis split-sum second term: ∫ BRDF·cosθ with F factored as F0·A + B.
/// Returns `res*res*2` floats, row = roughness, column = N·V (both `(i+0.5)/res`).
/// Texel (x,y) → (scale A, bias B).
pub(crate) fn generate_brdf_lut(res: usize, samples: u32) -> Vec<f32> {
    let mut out = vec![0.0f32; res * res * 2];
    for ty in 0..res {
        let roughness = (ty as f32 + 0.5) / res as f32;
        let a = (roughness * roughness).max(1e-4); // α = r²
        // Schlick-GGX geometry k for IBL (Karis): k = α/2.
        let k = a / 2.0;
        for tx in 0..res {
            let ndv = ((tx as f32 + 0.5) / res as f32).max(1e-3);
            let v = [(1.0 - ndv * ndv).max(0.0).sqrt(), 0.0, ndv];
            let mut acc_a = 0.0f32;
            let mut acc_b = 0.0f32;
            for s in 0..samples {
                let xi = [(s as f32 + 0.5) / samples as f32, radical_inverse_vdc(s)];
                // GGX importance-sampled half vector (N = +Z).
                let phi = 2.0 * std::f32::consts::PI * xi[0];
                let cos_th = ((1.0 - xi[1]) / (1.0 + (a * a - 1.0) * xi[1])).sqrt();
                let sin_th = (1.0 - cos_th * cos_th).max(0.0).sqrt();
                let hv = [phi.cos() * sin_th, phi.sin() * sin_th, cos_th];
                let vdh = v[0] * hv[0] + v[1] * hv[1] + v[2] * hv[2];
                let l = [
                    2.0 * vdh * hv[0] - v[0],
                    2.0 * vdh * hv[1] - v[1],
                    2.0 * vdh * hv[2] - v[2],
                ];
                let ndl = l[2];
                if ndl <= 0.0 {
                    continue;
                }
                let ndh = hv[2].max(0.0);
                let vdh = vdh.max(0.0);
                let g = (ndl / (ndl * (1.0 - k) + k)) * (ndv / (ndv * (1.0 - k) + k));
                let g_vis = g * vdh / (ndh * ndv).max(1e-6);
                let fc = (1.0 - vdh).powi(5);
                acc_a += (1.0 - fc) * g_vis;
                acc_b += fc * g_vis;
            }
            let inv = 1.0 / samples as f32;
            out[(ty * res + tx) * 2] = acc_a * inv;
            out[(ty * res + tx) * 2 + 1] = acc_b * inv;
        }
    }
    out
}

// ============================================================
// Env decode (worker-side)
// ============================================================

/// Decode an environment file to linear RGB f32 triplets. `.hdr` uses the existing
/// Radiance RGBE parser; `.exr` (and any LDR the `image` crate knows) go through
/// `image::open` — LDR input is sRGB-decoded to linear.
pub(crate) fn decode_env_file(path: &std::path::Path) -> Result<(Vec<f32>, u32, u32), String> {
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    let (pixels, w, h) = if ext == "hdr" {
        let bytes = std::fs::read(path).map_err(|e| format!("read failed: {e}"))?;
        AzraApp::parse_hdr(&bytes).ok_or_else(|| "not a valid Radiance .hdr".to_string())?
    } else {
        let img = image::open(path).map_err(|e| format!("decode failed: {e}"))?;
        let w = img.width();
        let h = img.height();
        // `to_rgb32f` is linear for float sources (EXR); for 8-bit LDR sources the image
        // crate converts by scaling only, so apply the sRGB EOTF ourselves in that case.
        let is_ldr = img.color().bytes_per_pixel() <= 4 && !matches!(
            img,
            image::DynamicImage::ImageRgb32F(_) | image::DynamicImage::ImageRgba32F(_)
        );
        let mut pixels: Vec<f32> = img.to_rgb32f().into_raw();
        if is_ldr {
            for v in pixels.iter_mut() {
                let x = *v;
                *v = if x <= 0.04045 { x / 12.92 } else { ((x + 0.055) / 1.055).powf(2.4) };
            }
        }
        (pixels, w, h)
    };
    if w == 0 || h == 0 {
        return Err("empty image".into());
    }
    if w > ENV_MAX_WIDTH {
        return Err(format!("environment wider than {ENV_MAX_WIDTH}px — resize it first"));
    }
    Ok((pixels, w, h))
}

// ============================================================
// Worker → poll delivery + GPU install (Frame Ordering: all in poll)
// ============================================================

/// Result of an environment load worker (decode + placeholder chain + SH9 off-thread).
pub(crate) struct EnvLoadResult {
    /// Load generation at request time — a result from before a New/Load-project
    /// clear is dropped (stale-worker guard).
    pub(crate) generation: u64,
    pub(crate) outcome: Result<EnvLoaded, String>,
}

pub(crate) struct EnvLoaded {
    pub(crate) width: u32,
    pub(crate) height: u32,
    /// Level 0 only (kept CPU-side for save/duplicate parity with the old field use).
    pub(crate) level0: Vec<f32>,
    /// `[chain][sh9]` — the full buffer image.
    pub(crate) buffer_floats: Vec<f32>,
    pub(crate) label: String,
    /// The file this environment was decoded from — recorded on install as
    /// `env_source_path`, which the project save persists (FL4ENVIR side-channel).
    pub(crate) source_path: std::path::PathBuf,
}

/// The one BRDF LUT, computed once on a startup worker thread (`spawn_brdf_lut_worker`).
static BRDF_LUT: std::sync::OnceLock<Vec<f32>> = std::sync::OnceLock::new();

pub(crate) fn spawn_brdf_lut_worker() {
    std::thread::spawn(|| {
        let _ = BRDF_LUT.set(generate_brdf_lut(BRDF_LUT_RES as usize, BRDF_LUT_SAMPLES));
    });
}

impl AzraApp {
    /// Spawn the environment worker for `path` (decode + mips + SH9 off the frame thread)
    /// and deliver through `env_load_rx` → `poll_env_ibl`. All env loads — startup default,
    /// file dialog, library click, viewport drag-drop — go through here.
    pub(crate) fn request_env_load(&mut self, path: std::path::PathBuf) {
        let tx = self.env_load_tx.clone();
        let generation = self.env_load_gen;
        std::thread::spawn(move || {
            let outcome = decode_env_file(&path).map(|(pixels, w, h)| {
                let cpu = build_env_cpu(&pixels, w, h);
                let mut buffer_floats = cpu.chain;
                buffer_floats.extend_from_slice(&cpu.sh9);
                EnvLoaded {
                    width: w,
                    height: h,
                    level0: pixels,
                    buffer_floats,
                    label: path
                        .file_name()
                        .map(|s| s.to_string_lossy().to_string())
                        .unwrap_or_default(),
                    source_path: path.clone(),
                }
            });
            let _ = tx.send(EnvLoadResult { generation, outcome });
        });
    }

    /// Re-establish a loaded project's environment from its `FL4ENVIR` record (project.rs).
    /// Runs AFTER `apply_to_app` (which bumped `env_load_gen` + queued the env clear), so the
    /// kicked worker carries the NEW generation — an in-flight load from the previous scene is
    /// dropped by the existing generation guard. The authored settings apply unconditionally;
    /// a missing HDRI file keeps the default (cleared) environment and toasts the file name —
    /// it NEVER fails the project load.
    pub(crate) fn restore_env_from_record(
        &mut self,
        rec: Option<&crate::project::EnvRecord>,
        base_dir: Option<&std::path::Path>,
    ) {
        let Some(rec) = rec else { return };
        self.render.env_brightness = rec.brightness;
        self.render.env_rotation_deg = rec.rotation_deg;
        let resolved = crate::project::resolve_env_path(&rec.path, base_dir);
        if resolved.is_file() {
            self.request_env_load(resolved);
        } else {
            self.toast(&format!(
                "Environment HDRI not found: {} — keeping default environment", rec.path
            ));
        }
    }

    /// Poll-phase env pipeline: worker results → GPU install, deferred clear, BRDF LUT
    /// upload, and the budgeted GGX prefilter (ONE mip dispatch per frame).
    /// The ONLY place `env_buffer` / `brdf_lut_buffer` are (re)created (Frame Ordering).
    pub(crate) fn poll_env_ibl(&mut self) {
        // 1. Deferred clear (properties panel button / project new/load).
        if self.env_clear_pending {
            self.env_clear_pending = false;
            self.env_prefilter_queue.clear();
            self.env_hdr_data.clear();
            self.env_hdr_mip_data.clear();
            self.env_hdr_width = 0;
            self.env_hdr_height = 0;
            self.env_source_path = None; // no env → project saves carry no FL4ENVIR section
            if let Some(gpu) = self.gpu.as_mut() {
                gpu.buffers.env_buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("EnvData"),
                    size: 4,
                    usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                });
                // Section K: clear env CDF
                upload_pt_env_cdf(gpu, &empty_env_cdf());
                gpu.bind_groups.deferred_bg = None;
            // SSGI caches bind groups holding env_buffer, which was just replaced.
            if let Some(r) = gpu.ssgi_res.as_mut() { r.invalidate_bind_groups(); }
                gpu.bind_groups.pt_bg = None;
                gpu.bind_groups.pt_bg_half = None;
                gpu.bind_groups.ssr_bg = None; // SSR binds env_buffer (binding 21)
            }
            self.mark_dirty(DirtyFlags::RENDER);
        }

        // Section K: if HDRI is resident but CDF was never uploaded (env installed
        // before GPU / after GPU recreate), rebuild once.
        if self.env_hdr_width > 0 && !self.env_hdr_data.is_empty() {
            if let Some(gpu) = self.gpu.as_mut() {
                if gpu.buffers.pt_env_cdf_buffer.is_none() {
                    let cdf = build_env_luminance_cdf(
                        &self.env_hdr_data,
                        self.env_hdr_width,
                        self.env_hdr_height,
                    );
                    upload_pt_env_cdf(gpu, &cdf);
                }
            }
        }

        // 2. Worker results (drain fully — loads are rare one-shots).
        while let Ok(result) = self.env_load_rx.try_recv() {
            if result.generation != self.env_load_gen {
                continue; // stale: a project clear invalidated this load
            }
            match result.outcome {
                Ok(loaded) => {
                    let msg = format!("Environment: {} ({}x{})", loaded.label, loaded.width, loaded.height);
                    self.install_env(loaded);
                    self.toast(&msg);
                }
                Err(e) => self.toast(&format!("Environment load failed: {e}")),
            }
        }

        // 3. One-time BRDF LUT upload once the startup worker finishes.
        if !self.brdf_lut_uploaded {
            if let Some(lut) = BRDF_LUT.get() {
                if let Some(gpu) = self.gpu.as_mut() {
                    let bytes: Vec<u8> = lut.iter().flat_map(|v| v.to_le_bytes()).collect();
                    gpu.buffers.brdf_lut_buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor {
                        label: Some("BrdfLut"),
                        size: bytes.len() as u64,
                        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                        mapped_at_creation: false,
                    });
                    gpu.queue.write_buffer(&gpu.buffers.brdf_lut_buffer, 0, &bytes);
                    gpu.bind_groups.deferred_bg = None;
            // SSGI caches bind groups holding env_buffer, which was just replaced.
            if let Some(r) = gpu.ssgi_res.as_mut() { r.invalidate_bind_groups(); }
                    gpu.bind_groups.ssr_bg = None; // SSR binds brdf_lut_buffer (binding 39)
                    self.brdf_lut_uploaded = true;
                    self.mark_dirty(DirtyFlags::RENDER);
                }
            }
        }

        // 4. Budgeted GGX prefilter: ONE mip dispatch per frame (Frame Budget rule).
        self.dispatch_env_prefilter_step();
    }

    /// Install a decoded environment: replace `env_buffer` with `[chain][sh9]`, then queue
    /// the GPU GGX prefilter for mips 1..6. Runs in poll only.
    fn install_env(&mut self, loaded: EnvLoaded) {
        self.env_hdr_data = loaded.level0;
        self.env_hdr_mip_data = loaded.buffer_floats;
        self.env_hdr_width = loaded.width;
        self.env_hdr_height = loaded.height;
        self.env_source_path = Some(loaded.source_path); // what a project save persists (FL4ENVIR)
        // Section K: build luminance CDF from level-0 (before move into GPU install).
        let cdf = build_env_luminance_cdf(&self.env_hdr_data, loaded.width, loaded.height);
        if let Some(gpu) = self.gpu.as_mut() {
            let bytes: Vec<u8> = self.env_hdr_mip_data.iter().flat_map(|v| v.to_le_bytes()).collect();
            gpu.buffers.env_buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("EnvData"),
                size: bytes.len() as u64,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            gpu.queue.write_buffer(&gpu.buffers.env_buffer, 0, &bytes);
            upload_pt_env_cdf(gpu, &cdf);
            gpu.bind_groups.deferred_bg = None;
            // SSGI caches bind groups holding env_buffer, which was just replaced.
            if let Some(r) = gpu.ssgi_res.as_mut() { r.invalidate_bind_groups(); }
            gpu.bind_groups.pt_bg = None;
            gpu.bind_groups.pt_bg_half = None;
            gpu.bind_groups.ssr_bg = None; // SSR binds env_buffer (binding 21)
        }
        // GGX-prefilter mips 1..6 over the next frames (1 = lowest roughness first so
        // near-mirror surfaces sharpen soonest; box placeholders render meanwhile).
        self.env_prefilter_queue = (1..ENV_MIP_LEVELS).collect();
        self.mark_dirty(DirtyFlags::RENDER);
    }

    /// Dispatch at most ONE prefilter mip this frame. Own encoder + submit, bind group
    /// built fresh per dispatch (the primary-bake precedent — nothing cached, nothing
    /// to invalidate).
    fn dispatch_env_prefilter_step(&mut self) {
        if self.env_prefilter_queue.is_empty() || self.env_hdr_width == 0 {
            return;
        }
        let Some(gpu) = self.gpu.as_mut() else { return };
        crate::gpu_init::ensure_env_prefilter_pipeline(gpu);
        let (Some(pipeline), Some(bgl)) = (
            gpu.pipelines.env_prefilter_pipeline.as_ref(),
            gpu.bind_groups.env_prefilter_bgl.as_ref(),
        ) else {
            return;
        };
        let level = self.env_prefilter_queue.remove(0);
        let (w, h) = (self.env_hdr_width, self.env_hdr_height);
        let (dw, dh) = env_mip_dims(w, h, level);
        let mut offset = 0u32;
        for l in 0..level {
            let (mw, mh) = env_mip_dims(w, h, l);
            offset += mw * mh * 3;
        }
        // Roughness ramp 0→1 across mips 0..6; per-mip sample count trades cost for the
        // wider lobes (deep mips are tiny, so more samples there are nearly free).
        let roughness = level as f32 / (ENV_MIP_LEVELS - 1) as f32;
        let samples = match level {
            1 => 32u32,
            2 => 64,
            3 => 64,
            _ => 128,
        };
        let params: [u32; 8] = [
            w,
            h,
            offset,
            dw,
            dh,
            samples,
            roughness.to_bits(),
            0,
        ];
        let params_bytes: Vec<u8> = params.iter().flat_map(|v| v.to_le_bytes()).collect();
        if gpu.buffers.env_prefilter_params_buffer.is_none() {
            gpu.buffers.env_prefilter_params_buffer = Some(gpu.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("EnvPrefilterParams"),
                size: 32,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }));
        }
        let params_buf = gpu.buffers.env_prefilter_params_buffer.as_ref().unwrap();
        gpu.queue.write_buffer(params_buf, 0, &params_bytes);
        let bg = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("env_prefilter_bg"),
            layout: bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: params_buf.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: gpu.buffers.env_buffer.as_entire_binding() },
            ],
        });
        let mut enc = gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("env_prefilter") });
        {
            let mut cp = enc.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("env_prefilter"),
                timestamp_writes: None,
            });
            cp.set_pipeline(pipeline);
            cp.set_bind_group(0, &bg, &[]);
            cp.dispatch_workgroups(dw.div_ceil(8), dh.div_ceil(8), 1);
        }
        gpu.queue.submit(Some(enc.finish()));
        self.mark_dirty(DirtyFlags::RENDER); // repaint with the sharpened mip
    }
}

// ============================================================
// Tests
// ============================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// Uniform environment → SH irradiance equals the environment value for every
    /// normal (energy sanity for projection + basis + cosine convolution together).
    #[test]
    fn sh9_uniform_env_round_trips() {
        let (w, h) = (64u32, 32u32);
        let pixels = vec![0.7f32; (w * h * 3) as usize];
        let sh = project_sh9(&pixels, w, h);
        for n in [
            [0.0, 1.0, 0.0],
            [0.0, -1.0, 0.0],
            [1.0, 0.0, 0.0],
            [0.0, 0.0, -1.0],
            [0.577, 0.577, 0.577],
        ] {
            let e = eval_sh9(&sh, n);
            for c in 0..3 {
                assert!(
                    (e[c] - 0.7).abs() < 0.02,
                    "uniform env irradiance off at {n:?}: {e:?}"
                );
            }
        }
    }

    /// Top-lit environment: the up normal must receive much more than the down normal,
    /// and the DC term must sit between them (directionality sanity).
    #[test]
    fn sh9_top_light_is_directional() {
        let (w, h) = (64u32, 32u32);
        let mut pixels = vec![0.0f32; (w * h * 3) as usize];
        for y in 0..(h / 4) as usize {
            for x in 0..w as usize {
                let i = (y * w as usize + x) * 3;
                pixels[i] = 1.0;
                pixels[i + 1] = 1.0;
                pixels[i + 2] = 1.0;
            }
        }
        let sh = project_sh9(&pixels, w, h);
        let up = eval_sh9(&sh, [0.0, 1.0, 0.0])[0];
        let down = eval_sh9(&sh, [0.0, -1.0, 0.0])[0];
        assert!(up > down * 3.0, "top light not directional: up={up} down={down}");
        assert!(up > 0.05, "top light lost energy: up={up}");
    }

    /// BRDF LUT analytic corners (the split-sum second term):
    /// - roughness→0, N·V→1: near-mirror, full fresnel scale → A≈1, B≈0
    /// - everywhere: A + B ≤ 1 (energy conservation), A,B ≥ 0
    /// - grazing + rough: B (the bias/edge-tint term) grows vs the smooth corner.
    #[test]
    fn brdf_lut_known_values() {
        let res = 16usize;
        let lut = generate_brdf_lut(res, 128);
        let at = |tx: usize, ty: usize| (lut[(ty * res + tx) * 2], lut[(ty * res + tx) * 2 + 1]);
        // Smooth, head-on (last column ~ N·V≈1, first row ~ r≈0)
        let (a, b) = at(res - 1, 0);
        assert!(a > 0.9 && a <= 1.01, "smooth head-on A={a}");
        assert!(b.abs() < 0.05, "smooth head-on B={b}");
        // Energy + positivity everywhere
        for ty in 0..res {
            for tx in 0..res {
                let (a, b) = at(tx, ty);
                assert!(a >= 0.0 && b >= 0.0, "negative term at ({tx},{ty}): {a},{b}");
                assert!(a + b <= 1.02, "energy blown at ({tx},{ty}): {}", a + b);
            }
        }
        // Fresnel bias grows toward grazing on smooth surfaces
        let (_, b_grazing) = at(1, 0);
        let (_, b_headon) = at(res - 1, 0);
        assert!(b_grazing > b_headon, "grazing fresnel bias should dominate");
    }

    /// Chain layout: box placeholders occupy exactly the offsets the WGSL loop computes,
    /// and a constant level 0 stays constant through every level (prefilter mip-0 == source
    /// is by construction; this pins the placeholder levels too).
    #[test]
    fn env_chain_layout_and_constancy() {
        let (w, h) = (40u32, 20u32); // non-power-of-two on purpose
        let pixels = vec![0.25f32; (w * h * 3) as usize];
        let cpu = build_env_cpu(&pixels, w, h);
        assert_eq!(cpu.chain.len(), env_chain_len_floats(w, h));
        for v in cpu.chain.iter() {
            assert!((v - 0.25).abs() < 1e-5, "constant env drifted: {v}");
        }
        for c in cpu.sh9.iter().take(3) {
            // DC × Y00 ≈ 0.25 → coefficient ≈ 0.25 / 0.282095
            assert!((c * SH_C0 - 0.25).abs() < 0.01, "constant env SH DC off: {c}");
        }
        // 7 levels always exist, down to 1×1-floored dims
        let (mw, mh) = env_mip_dims(w, h, 6);
        assert!(mw >= 1 && mh >= 1);
    }
}
