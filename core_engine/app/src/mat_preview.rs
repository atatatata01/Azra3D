// core_engine/app/src/mat_preview.rs
// CPU-side material preview: raymarches a unit sphere with PBR shading
// to produce a 32x32 RGBA swatch for inline UI display.

use glam::Vec3;
use core_math::primitives::PbrMaterial;
use std::collections::HashMap;

const PREVIEW_SIZE: usize = 32;

/// Hash a PbrMaterial's visual properties for cache invalidation.
/// Only hashes fields that affect appearance (not name, quality, etc.).
pub(crate) fn material_hash(mat: &PbrMaterial) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    // Base color
    mat.albedo.x.to_bits().hash(&mut h);
    mat.albedo.y.to_bits().hash(&mut h);
    mat.albedo.z.to_bits().hash(&mut h);
    mat.roughness.to_bits().hash(&mut h);
    mat.metallic.to_bits().hash(&mut h);
    // Emission
    mat.emission_color.x.to_bits().hash(&mut h);
    mat.emission_color.y.to_bits().hash(&mut h);
    mat.emission_color.z.to_bits().hash(&mut h);
    mat.emission_intensity.to_bits().hash(&mut h);
    // Glass / SSS
    mat.ior.to_bits().hash(&mut h);
    mat.clearcoat.to_bits().hash(&mut h);
    mat.clearcoat_roughness.to_bits().hash(&mut h);
    mat.anisotropy.to_bits().hash(&mut h);
    mat.sheen.to_bits().hash(&mut h);
    // Subsurface
    mat.sss_radius.to_bits().hash(&mut h);
    mat.sss_color.x.to_bits().hash(&mut h);
    mat.sss_color.y.to_bits().hash(&mut h);
    mat.sss_color.z.to_bits().hash(&mut h);
    // Glass tint
    mat.glass_tint.x.to_bits().hash(&mut h);
    mat.glass_tint.y.to_bits().hash(&mut h);
    mat.glass_tint.z.to_bits().hash(&mut h);
    h.finish()
}

/// Renders a 32x32 PBR sphere preview. Returns RGBA u8 pixel data.
pub(crate) fn render_sphere_preview(mat: &PbrMaterial) -> Vec<u8> {
    let mut pixels = vec![0u8; PREVIEW_SIZE * PREVIEW_SIZE * 4];

    // Camera setup: orthographic, looking at unit sphere at origin
    let _cam_pos = Vec3::new(0.0, 0.0, 3.0);
    let light_dir = Vec3::new(0.5, 0.7, 0.6).normalize();
    let light_color = Vec3::new(1.0, 0.98, 0.95);
    let ambient = Vec3::new(0.08, 0.09, 0.12);

    // Fill light from opposite side (subtle)
    let fill_dir = Vec3::new(-0.4, 0.3, -0.5).normalize();
    let fill_color = Vec3::new(0.15, 0.18, 0.25);

    let sphere_radius = 0.85;

    for y in 0..PREVIEW_SIZE {
        for x in 0..PREVIEW_SIZE {
            // Normalized coords [-1, 1]
            let u = (x as f32 + 0.5) / PREVIEW_SIZE as f32 * 2.0 - 1.0;
            let v = -((y as f32 + 0.5) / PREVIEW_SIZE as f32 * 2.0 - 1.0); // flip Y

            // Orthographic ray: origin = (u, v, cam_z), dir = (0,0,-1)
            let r2 = u * u + v * v;
            let r2_scaled = r2 / (sphere_radius * sphere_radius);

            if r2_scaled > 1.0 {
                // Background: dark gradient
                let bg = 0.12 + 0.04 * (1.0 - (r2_scaled - 1.0).min(1.0));
                let c = (bg * 255.0) as u8;
                let idx = (y * PREVIEW_SIZE + x) * 4;
                pixels[idx] = c;
                pixels[idx + 1] = c;
                pixels[idx + 2] = c;
                pixels[idx + 3] = 255;
                continue;
            }

            // Sphere intersection (orthographic)
            let z = (1.0 - r2_scaled).sqrt();
            let normal = Vec3::new(u / sphere_radius, v / sphere_radius, z).normalize();
            let view_dir = Vec3::new(0.0, 0.0, 1.0);

            // PBR shading (simplified Cook-Torrance)
            let color = shade_pbr(mat, normal, view_dir, light_dir, light_color, fill_dir, fill_color, ambient);

            // Tonemap (ACES approximation) + gamma
            let mapped = aces_tonemap(color);
            let gamma = Vec3::new(
                mapped.x.powf(1.0 / 2.2),
                mapped.y.powf(1.0 / 2.2),
                mapped.z.powf(1.0 / 2.2),
            );

            let idx = (y * PREVIEW_SIZE + x) * 4;
            pixels[idx]     = (gamma.x.clamp(0.0, 1.0) * 255.0) as u8;
            pixels[idx + 1] = (gamma.y.clamp(0.0, 1.0) * 255.0) as u8;
            pixels[idx + 2] = (gamma.z.clamp(0.0, 1.0) * 255.0) as u8;
            pixels[idx + 3] = 255;
        }
    }

    pixels
}

fn shade_pbr(
    mat: &PbrMaterial,
    n: Vec3, v: Vec3,
    light_dir: Vec3, light_color: Vec3,
    fill_dir: Vec3, fill_color: Vec3,
    ambient: Vec3,
) -> Vec3 {
    let albedo = mat.albedo;
    let roughness = mat.roughness.clamp(0.04, 1.0);
    let metallic = mat.metallic.clamp(0.0, 1.0);
    let alpha = roughness * roughness;

    // F0: reflectance at normal incidence
    let f0 = Vec3::lerp(Vec3::splat(0.04), albedo, metallic);

    // Main light
    let main = evaluate_light(n, v, light_dir, light_color, albedo, f0, alpha, metallic);

    // Fill light
    let fill = evaluate_light(n, v, fill_dir, fill_color, albedo, f0, alpha, metallic);

    // Ambient (diffuse only)
    let amb = ambient * albedo * (1.0 - metallic * 0.7);

    // Emission
    let emission = mat.emission_color * mat.emission_intensity;

    // Clearcoat (additive specular layer)
    let clearcoat = if mat.clearcoat > 0.01 {
        let cc_alpha = mat.clearcoat_roughness.clamp(0.04, 1.0).powi(2);
        let h = (light_dir + v).normalize();
        let ndh = n.dot(h).max(0.0);
        let ndl = n.dot(light_dir).max(0.0);
        let ndv = n.dot(v).max(0.0);
        let d = ggx_d(ndh, cc_alpha);
        let g = smith_g(ndl, ndv, cc_alpha);
        let f = schlick_f(0.04, ndv);
        let spec = d * g * f / (4.0 * ndl.max(0.001) * ndv.max(0.001));
        light_color * spec * ndl * mat.clearcoat
    } else {
        Vec3::ZERO
    };

    // Fresnel rim (subtle edge brightening)
    let ndv = n.dot(v).max(0.0);
    let fresnel_rim = (1.0 - ndv).powi(4) * 0.15 * (1.0 - roughness);
    let rim = Vec3::splat(fresnel_rim);

    main + fill + amb + emission + clearcoat + rim
}

fn evaluate_light(
    n: Vec3, v: Vec3, l: Vec3, light_col: Vec3,
    albedo: Vec3, f0: Vec3, alpha: f32, metallic: f32,
) -> Vec3 {
    let ndl = n.dot(l).max(0.0);
    if ndl < 0.001 { return Vec3::ZERO; }

    let h = (l + v).normalize();
    let ndh = n.dot(h).max(0.0);
    let ndv = n.dot(v).max(0.0);
    let vdh = v.dot(h).max(0.0);

    // GGX distribution
    let d = ggx_d(ndh, alpha);
    // Smith geometry
    let g = smith_g(ndl, ndv, alpha);
    // Fresnel (Schlick vec3)
    let f = schlick_f_vec3(f0, vdh);

    let denom = 4.0 * ndl.max(0.001) * ndv.max(0.001);
    let specular = d * g * f / denom;

    // Diffuse (Lambert, energy-conserving)
    let ks = f;
    let kd = (Vec3::ONE - ks) * (1.0 - metallic);
    let diffuse = kd * albedo / std::f32::consts::PI;

    (diffuse + specular) * light_col * ndl
}

#[inline]
fn ggx_d(ndh: f32, alpha: f32) -> f32 {
    let a2 = alpha * alpha;
    let denom = ndh * ndh * (a2 - 1.0) + 1.0;
    a2 / (std::f32::consts::PI * denom * denom).max(1e-7)
}

#[inline]
fn smith_g(ndl: f32, ndv: f32, alpha: f32) -> f32 {
    let k = alpha / 2.0;
    let gl = ndl / (ndl * (1.0 - k) + k).max(1e-7);
    let gv = ndv / (ndv * (1.0 - k) + k).max(1e-7);
    gl * gv
}

#[inline]
fn schlick_f(f0: f32, cos_theta: f32) -> f32 {
    f0 + (1.0 - f0) * (1.0 - cos_theta).powi(5)
}

#[inline]
fn schlick_f_vec3(f0: Vec3, cos_theta: f32) -> Vec3 {
    f0 + (Vec3::ONE - f0) * (1.0 - cos_theta).powi(5)
}

#[inline]
fn aces_tonemap(c: Vec3) -> Vec3 {
    // Narkowicz ACES fit
    let a = 2.51;
    let b = 0.03;
    let c_ = 2.43;
    let d = 0.59;
    let e = 0.14;
    let tone = |x: f32| -> f32 {
        ((x * (a * x + b)) / (x * (c_ * x + d) + e)).clamp(0.0, 1.0)
    };
    Vec3::new(tone(c.x), tone(c.y), tone(c.z))
}

/// Cache for material preview textures.
pub(crate) struct MaterialPreviewCache {
    /// Map from material hash → egui texture handle
    pub(crate) textures: HashMap<u64, eframe::egui::TextureHandle>,
}

impl MaterialPreviewCache {
    pub(crate) fn new() -> Self {
        Self { textures: HashMap::new() }
    }

    /// Get or create a preview swatch for the given material.
    /// Returns the TextureId to display.
    pub(crate) fn get_or_create(
        &mut self,
        ctx: &eframe::egui::Context,
        mat: &PbrMaterial,
    ) -> eframe::egui::TextureId {
        let hash = material_hash(mat);
        if let Some(handle) = self.textures.get(&hash) {
            return handle.id();
        }

        // Render and cache
        let rgba = render_sphere_preview(mat);
        let ci = eframe::egui::ColorImage::from_rgba_unmultiplied(
            [PREVIEW_SIZE, PREVIEW_SIZE],
            &rgba,
        );
        let handle = ctx.load_texture(
            format!("mat_preview_{:016x}", hash),
            ci,
            eframe::egui::TextureOptions::LINEAR,
        );
        let id = handle.id();
        self.textures.insert(hash, handle);
        id
    }

    /// Evict stale entries. Call periodically to avoid unbounded growth.
    /// Keeps entries that match any of the provided hashes.
    #[allow(dead_code)]
    pub(crate) fn retain(&mut self, active_hashes: &[u64]) {
        if self.textures.len() > 64 {
            let set: std::collections::HashSet<u64> = active_hashes.iter().copied().collect();
            self.textures.retain(|k, _| set.contains(k));
        }
    }
}
