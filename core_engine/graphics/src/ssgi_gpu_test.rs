//! SSGI Gate 0 — headless harness, environment, and the CPU oracle.
//!
//! Plan: `docs/planning/render/SSGI_PLAN.md` §5 Gate 0.
//!
//! Why this exists in this shape: the adversarial review of revision 1 found that
//! every gate which could have caught a wrong energy formula passed *by
//! construction* (they asserted "delta is zero when nothing is hit", which is true
//! of any formula summed over hits). The fix is an **independent CPU oracle** —
//! re-derived here from the plan's §4.2, deliberately NOT calling the production
//! WGSL or the app's `env_ibl` helpers, because an oracle that shares code with the
//! thing it checks cannot catch a bug in the shared part. Gate 3 cross-checks the
//! GPU against this.
//!
//! Scene (analytic, so visibility has a ground truth independent of any
//! screen-space march): a white floor at y=0 meeting a saturated red wall at
//! z=-1. Every gathered ray either hits one of those two planes well inside the
//! frame or escapes to the environment — no grazing cases — so the screen-space
//! march the GPU will use should agree with the analytic answer closely, and a
//! large GPU/CPU divergence means a formula error, not march error.
//!
//! Everything here is CPU-only and adapter-free by design, so it runs in CI. The
//! GPU upload path lands in Gate 1 with the pass itself.

#![cfg(test)]

use half::f16;

// ── Scene constants ─────────────────────────────────────────────────────────

/// Cosine-weighted rays per pixel. Must match the shader's tap count.
const N_RAYS: u32 = 8;
/// Same spiral constant as `render/src/ssao.wgsl:55`.
const GOLDEN_ANGLE: f32 = 2.399_963_2;
const PI: f32 = std::f32::consts::PI;

/// Floor: plane y = 0, x in [-2,2], z in [-1,3]. Near-white dielectric.
const FLOOR_ALBEDO: [f32; 3] = [0.80, 0.80, 0.80];
/// Lit radiance the deferred pass would have written for the floor.
const FLOOR_HDR: [f32; 3] = [0.72, 0.72, 0.72];
/// Wall: plane z = -1, x in [-2,2], y in [0,2]. Saturated red.
const WALL_ALBEDO: [f32; 3] = [0.75, 0.05, 0.05];
/// Lit radiance for the wall — this is the light SSGI must bounce onto the floor.
const WALL_HDR: [f32; 3] = [0.90, 0.06, 0.06];

/// Geometry flag for ordinary lit geometry (`render/src/gbuffer.wgsl:929`).
const FLAG_GEOMETRY: f32 = 0.78;

// ── Analytic scene trace (the visibility ground truth) ──────────────────────

#[derive(Clone, Copy, Debug, PartialEq)]
enum Obj {
    Floor,
    Wall,
}

#[derive(Clone, Copy)]
struct Hit {
    t: f32,
    n: [f32; 3],
    obj: Obj,
}

fn dot3(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn norm3(v: [f32; 3]) -> [f32; 3] {
    let l = dot3(v, v).sqrt().max(1e-20);
    [v[0] / l, v[1] / l, v[2] / l]
}

fn add3(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}

fn mul3(a: [f32; 3], s: f32) -> [f32; 3] {
    [a[0] * s, a[1] * s, a[2] * s]
}

/// Nearest intersection with the floor/wall quads. `t_min` skips the origin
/// surface (the ray-origin self-intersection guard the shader gets from its
/// normal+ray bias).
fn trace(o: [f32; 3], d: [f32; 3], t_min: f32) -> Option<Hit> {
    let mut best: Option<Hit> = None;
    let mut keep = |h: Hit| {
        if h.t > t_min && best.map_or(true, |b| h.t < b.t) {
            best = Some(h);
        }
    };
    // Floor y = 0
    if d[1].abs() > 1e-6 {
        let t = -o[1] / d[1];
        if t > 0.0 {
            let p = add3(o, mul3(d, t));
            if p[0] >= -2.0 && p[0] <= 2.0 && p[2] >= -1.0 && p[2] <= 3.0 {
                keep(Hit { t, n: [0.0, 1.0, 0.0], obj: Obj::Floor });
            }
        }
    }
    // Wall z = -1
    if d[2].abs() > 1e-6 {
        let t = (-1.0 - o[2]) / d[2];
        if t > 0.0 {
            let p = add3(o, mul3(d, t));
            if p[0] >= -2.0 && p[0] <= 2.0 && p[1] >= 0.0 && p[1] <= 2.0 {
                keep(Hit { t, n: [0.0, 0.0, 1.0], obj: Obj::Wall });
            }
        }
    }
    best
}

fn obj_albedo(o: Obj) -> [f32; 3] {
    match o {
        Obj::Floor => FLOOR_ALBEDO,
        Obj::Wall => WALL_ALBEDO,
    }
}

fn obj_hdr(o: Obj) -> [f32; 3] {
    match o {
        Obj::Floor => FLOOR_HDR,
        Obj::Wall => WALL_HDR,
    }
}

// ── Environment: equirect level 0 + mip chain + SH9 tail ────────────────────

/// Matches the buffer layout `sample_env_at_level` walks
/// (`graphics/src/sdf_lighting.wgsl:226`):
/// `[level0 w*h*3][level1 w/2*h/2*3] … [level6][SH9 27]`, f32, RGB.
struct EnvMap {
    w: u32,
    h: u32,
    /// Level 0 only, RGB per texel.
    level0: Vec<f32>,
    /// The full packed buffer including the mip chain and the SH9 tail.
    buffer: Vec<f32>,
    /// The 9 RGB coefficients, already band-scaled (A0=1, A1=2/3, A2=1/4).
    sh9: [[f32; 3]; 9],
}

/// A clearly directional two-tone sky, so `env_sh_irradiance` differs between an
/// up-facing and a down-facing normal (the property `env_ibl.rs`'s own
/// `sh9_top_light_is_directional` test pins).
fn sky_radiance(d: [f32; 3]) -> [f32; 3] {
    sky_radiance_v(d, false)
}

/// `bright_horizon` adds an intense band low on the sky toward -z (the wall).
///
/// Exists to give the `min(S, applied)` clamp SIGNAL. With the plain sky, rays
/// that hit the wall point along a DIM horizon, so the raw radiance they block
/// never exceeds the SH-reconstructed hemisphere average and the clamp never
/// binds — which is why sabotaging it away left Gate 3 green. A bright band in
/// exactly the blocked directions inverts that.
fn sky_radiance_v(d: [f32; 3], bright_horizon: bool) -> [f32; 3] {
    let t = ((d[1] + 0.2) / 0.5).clamp(0.0, 1.0);
    let t = t * t * (3.0 - 2.0 * t); // smoothstep
    let ground = [0.15, 0.12, 0.10];
    let sky = [0.50, 0.70, 1.20];
    let mut c = [
        ground[0] + (sky[0] - ground[0]) * t,
        ground[1] + (sky[1] - ground[1]) * t,
        ground[2] + (sky[2] - ground[2]) * t,
    ];
    if bright_horizon {
        // Narrow, very bright band near the horizon on the -z side.
        let band = (1.0 - (d[1] / 0.25).abs()).max(0.0) * (-d[2]).max(0.0);
        for k in 0..3 {
            c[k] += 40.0 * band;
        }
    }
    c
}

/// Equirect texel direction, matching `sample_env_at_level`'s mapping:
/// `theta = acos(y)`, `phi = atan2(z, x)`, `u = (phi + PI)/2PI`, `v = theta/PI`.
fn equirect_dir(u: f32, v: f32) -> [f32; 3] {
    let theta = v * PI;
    let phi = u * 2.0 * PI - PI;
    let sin_t = theta.sin();
    [sin_t * phi.cos(), theta.cos(), sin_t * phi.sin()]
}

/// SH9 basis, identical constants to `sdf_lighting.wgsl:327-341`.
fn sh_basis(n: [f32; 3]) -> [f32; 9] {
    let (b0, b1, b2, b3, b4) = (0.282095, 0.488603, 1.092548, 0.315392, 0.546274);
    [
        b0,
        b1 * n[1],
        b1 * n[2],
        b1 * n[0],
        b2 * n[0] * n[1],
        b2 * n[1] * n[2],
        b3 * (3.0 * n[2] * n[2] - 1.0),
        b2 * n[0] * n[2],
        b4 * (n[0] * n[0] - n[1] * n[1]),
    ]
}

impl EnvMap {
    fn build(w: u32, h: u32) -> Self { Self::build_v(w, h, false) }

    fn build_v(w: u32, h: u32, bright_horizon: bool) -> Self {
        let mut level0 = vec![0.0f32; (w * h * 3) as usize];
        for y in 0..h {
            for x in 0..w {
                let u = (x as f32 + 0.5) / w as f32;
                let v = (y as f32 + 0.5) / h as f32;
                let c = sky_radiance_v(equirect_dir(u, v), bright_horizon);
                let i = ((y * w + x) * 3) as usize;
                level0[i] = c[0];
                level0[i + 1] = c[1];
                level0[i + 2] = c[2];
            }
        }

        // ── SH projection ──
        // L_lm = INTEGRAL L(d) Y_lm(d) dw, then band-scaled by A_l = Â_l/pi so that
        // eval(n) = (1/pi) INTEGRAL L cos dw = the cosine-weighted average radiance.
        // A0 = pi/pi = 1, A1 = (2pi/3)/pi = 2/3, A2 = (pi/4)/pi = 1/4 — the same
        // factors `app/src/env_ibl.rs:308-312` applies. Re-derived here on purpose:
        // an oracle that called the production projector could not catch a bug in it.
        let mut acc = [[0.0f32; 3]; 9];
        for y in 0..h {
            for x in 0..w {
                let u = (x as f32 + 0.5) / w as f32;
                let v = (y as f32 + 0.5) / h as f32;
                let d = equirect_dir(u, v);
                let theta = v * PI;
                // Solid angle of one equirect texel.
                let dw = (2.0 * PI / w as f32) * (PI / h as f32) * theta.sin();
                let i = ((y * w + x) * 3) as usize;
                let l = [level0[i], level0[i + 1], level0[i + 2]];
                let b = sh_basis(d);
                for k in 0..9 {
                    acc[k][0] += l[0] * b[k] * dw;
                    acc[k][1] += l[1] * b[k] * dw;
                    acc[k][2] += l[2] * b[k] * dw;
                }
            }
        }
        let band = [1.0f32, 2.0 / 3.0, 2.0 / 3.0, 2.0 / 3.0, 0.25, 0.25, 0.25, 0.25, 0.25];
        let mut sh9 = [[0.0f32; 3]; 9];
        for k in 0..9 {
            for c in 0..3 {
                sh9[k][c] = acc[k][c] * band[k];
            }
        }

        // ── Packed buffer: 7 levels then the SH tail ──
        // SSGI reads level 0 (raw radiance) and the SH tail only; mips 1..6 are
        // box-downsampled here rather than GGX-prefiltered, which is fine because
        // nothing in this harness samples them. Do not reuse this env for a
        // specular/SSR test without prefiltering them properly.
        let mut buffer = Vec::new();
        let mut cur = level0.clone();
        let (mut mw, mut mh) = (w, h);
        for lvl in 0..7u32 {
            buffer.extend_from_slice(&cur);
            if lvl == 6 {
                break;
            }
            let (nw, nh) = ((mw / 2).max(1), (mh / 2).max(1));
            let mut next = vec![0.0f32; (nw * nh * 3) as usize];
            for y in 0..nh {
                for x in 0..nw {
                    let mut s = [0.0f32; 3];
                    let mut n = 0.0f32;
                    for dy in 0..2 {
                        for dx in 0..2 {
                            let sx = (x * 2 + dx).min(mw - 1);
                            let sy = (y * 2 + dy).min(mh - 1);
                            let si = ((sy * mw + sx) * 3) as usize;
                            s[0] += cur[si];
                            s[1] += cur[si + 1];
                            s[2] += cur[si + 2];
                            n += 1.0;
                        }
                    }
                    let di = ((y * nw + x) * 3) as usize;
                    next[di] = s[0] / n;
                    next[di + 1] = s[1] / n;
                    next[di + 2] = s[2] / n;
                }
            }
            cur = next;
            mw = nw;
            mh = nh;
        }
        for k in 0..9 {
            buffer.push(sh9[k][0]);
            buffer.push(sh9[k][1]);
            buffer.push(sh9[k][2]);
        }

        EnvMap { w, h, level0, buffer, sh9 }
    }

    /// Raw radiance, bilinear on level 0 — the CPU twin of `sample_env_raw`.
    /// §4.2 requires RAW radiance here, not a prefiltered mip: mips 1..6 are
    /// GGX-convolved, and cosine-averaging one collapses it to a near-constant.
    fn sample_raw(&self, d: [f32; 3]) -> [f32; 3] {
        let dn = norm3(d);
        let theta = dn[1].clamp(-1.0, 1.0).acos();
        let phi = dn[2].atan2(dn[0]);
        let mut u = (phi + PI) / (2.0 * PI);
        u -= u.floor();
        let v = theta / PI;
        let fw = self.w as f32;
        let fh = self.h as f32;
        let fx = u * fw - 0.5;
        let fy = v * fh - 0.5;
        let ix0 = (fx.floor() + fw) as u32 % self.w;
        let iy0 = fy.floor().clamp(0.0, fh - 1.0) as u32;
        let ix1 = (ix0 + 1) % self.w;
        let iy1 = (iy0 + 1).min(self.h - 1);
        let tx = fx - fx.floor();
        let ty = (fy - fy.floor()).clamp(0.0, 1.0);
        let tap = |x: u32, y: u32| {
            let i = ((y * self.w + x) * 3) as usize;
            [self.level0[i], self.level0[i + 1], self.level0[i + 2]]
        };
        let (c00, c10, c01, c11) = (tap(ix0, iy0), tap(ix1, iy0), tap(ix0, iy1), tap(ix1, iy1));
        let mut out = [0.0f32; 3];
        for c in 0..3 {
            let a = c00[c] + (c10[c] - c00[c]) * tx;
            let b = c01[c] + (c11[c] - c01[c]) * tx;
            out[c] = a + (b - a) * ty;
        }
        out
    }

    /// CPU twin of `env_sh_irradiance` — cosine-weighted average radiance.
    fn sh_irradiance(&self, n: [f32; 3]) -> [f32; 3] {
        let b = sh_basis(norm3(n));
        let mut acc = [0.0f32; 3];
        for k in 0..9 {
            for c in 0..3 {
                acc[c] += self.sh9[k][c] * b[k];
            }
        }
        [acc[0].max(0.0), acc[1].max(0.0), acc[2].max(0.0)]
    }
}

// ── Shading helpers, mirrored from the shader ───────────────────────────────

/// Mirror of `oct_decode_bent` (`deferred_entry.wgsl:201`).
fn oct_decode(e: [f32; 2]) -> [f32; 3] {
    let mut n = [e[0], e[1], 1.0 - e[0].abs() - e[1].abs()];
    if n[2] < 0.0 {
        let sx = if n[0] >= 0.0 { 1.0 } else { -1.0 };
        let sy = if n[1] >= 0.0 { 1.0 } else { -1.0 };
        n = [(1.0 - n[1].abs()) * sx, (1.0 - n[0].abs()) * sy, n[2]];
    }
    norm3(n)
}

/// Inverse of the above — the encoding `ssao.wgsl:260` writes.
fn oct_encode(n: [f32; 3]) -> [f32; 2] {
    let n = norm3(n);
    let l1 = n[0].abs() + n[1].abs() + n[2].abs();
    let p = [n[0] / l1, n[1] / l1];
    if n[2] >= 0.0 {
        p
    } else {
        let sx = if p[0] >= 0.0 { 1.0 } else { -1.0 };
        let sy = if p[1] >= 0.0 { 1.0 } else { -1.0 };
        [(1.0 - p[1].abs()) * sx, (1.0 - p[0].abs()) * sy]
    }
}

/// The bent normal the harness bakes into the AO texture: the geometric normal
/// tilted AWAY from the wall, which is what GTAO produces beside an occluder.
/// Deliberately far from +Y so that using the geometric normal instead is a
/// measurable error (Gate 3 sabotage (b)).
fn harness_bent_normal() -> [f32; 3] {
    // Tilted TOWARD the wall (-z), not away. Two reasons: it is far enough from
    // +Y that geometric-instead-of-bent is a large measurable error, AND it points
    // most of the cosine hemisphere at the occluder, so the majority of rays are
    // blocked. That second property is what lets the min(S, applied) clamp bind —
    // S is a 1/N average over HITS, so a scene where one ray in eight hits dilutes
    // it below the hemisphere average no matter how bright the blocked direction is.
    norm3([0.0, 1.0, -0.9])
}

/// `render/src/ssao.wgsl:66-68` — deterministic per-pixel rotation, no frame term.
fn ign(px: f32, py: f32) -> f32 {
    let v = 52.982_918_9 * (0.067_110_56 * px + 0.005_837_15 * py).fract();
    v.fract()
}

/// `graphics/src/deferred_entry.wgsl:241-247`.
fn multi_bounce_ao(vis: f32, albedo: [f32; 3]) -> [f32; 3] {
    let mut out = [0.0f32; 3];
    for c in 0..3 {
        let a = 2.0404 * albedo[c] - 0.3324;
        let b = -4.7951 * albedo[c] + 0.6417;
        let cc = 2.7552 * albedo[c] + 0.6903;
        out[c] = vis.max(((vis * a + b) * vis + cc) * vis);
    }
    out
}

/// The cosine-weighted tap set, identical in construction to
/// `render/src/ssao.wgsl:140-162` so the GPU and this oracle draw the SAME
/// directions — otherwise Monte-Carlo noise, not formula error, would dominate
/// the Gate 3 comparison.
fn cosine_dirs(n: [f32; 3], hx: u32, hy: u32) -> Vec<[f32; 3]> {
    let helper = if n[1].abs() > 0.9 { [1.0, 0.0, 0.0] } else { [0.0, 1.0, 0.0] };
    let tangent = norm3([
        helper[1] * n[2] - helper[2] * n[1],
        helper[2] * n[0] - helper[0] * n[2],
        helper[0] * n[1] - helper[1] * n[0],
    ]);
    let bitangent = [
        n[1] * tangent[2] - n[2] * tangent[1],
        n[2] * tangent[0] - n[0] * tangent[2],
        n[0] * tangent[1] - n[1] * tangent[0],
    ];
    let rot = ign(hx as f32, hy as f32) * 2.0 * PI;
    (0..N_RAYS)
        .map(|i| {
            let fi = (i as f32 + 0.5) / N_RAYS as f32;
            let ang = i as f32 * GOLDEN_ANGLE + rot;
            let sin_t = fi.sqrt();
            let cos_t = (1.0 - fi).sqrt();
            let l = [ang.cos() * sin_t, ang.sin() * sin_t, cos_t];
            norm3([
                tangent[0] * l[0] + bitangent[0] * l[1] + n[0] * l[2],
                tangent[1] * l[0] + bitangent[1] * l[1] + n[1] * l[2],
                tangent[2] * l[0] + bitangent[2] * l[1] + n[2] * l[2],
            ])
        })
        .collect()
}

// ── Scene ───────────────────────────────────────────────────────────────────

struct Scene {
    w: u32,
    h: u32,
    ortho: bool,
    /// A real bent normal was written into the AO texture (Gate 3 term (b)).
    bent: bool,
    gu: crate::GpuUniforms,
    cam: [f32; 3],
    fwd: [f32; 3],
    right: [f32; 3],
    up: [f32; 3],
    fov_scale: f32,
    ortho_half_h: f32,
    env: Option<EnvMap>,
    /// rgba16f: xyz = world normal, w = ray-parameter depth (`gbuf_depth`).
    gbuf_normal: Vec<f16>,
    /// rgba16f: x = roughness, y = metallic, z = shadow*AO, w = opacity + em.
    gbuf_material: Vec<f16>,
    /// rgba16f: rgb = linear albedo, a = flags.
    gbuf_albedo: Vec<f16>,
    /// The deferred-lit result SSGI gathers from.
    hdr: Vec<f16>,
    /// Per-pixel object, `None` = background.
    obj: Vec<Option<Obj>>,
}

impl Scene {
    /// Per-pixel camera ray, matching `camera_ray()` in both projections.
    fn camera_ray(&self, px: u32, py: u32) -> ([f32; 3], [f32; 3]) {
        let aspect = self.w as f32 / self.h as f32;
        let uv_x = ((px as f32 + 0.5) / self.w as f32) * 2.0 - 1.0;
        let uv_y = -(((py as f32 + 0.5) / self.h as f32) * 2.0 - 1.0);
        if self.ortho {
            let o = add3(
                self.cam,
                add3(
                    mul3(self.right, uv_x * self.ortho_half_h * aspect),
                    mul3(self.up, uv_y * self.ortho_half_h),
                ),
            );
            (o, norm3(self.fwd))
        } else {
            let d = norm3(add3(
                self.fwd,
                add3(
                    mul3(self.right, uv_x * aspect * self.fov_scale),
                    mul3(self.up, uv_y * self.fov_scale),
                ),
            ));
            (self.cam, d)
        }
    }

    /// Project a world point to a pixel — the inverse of `camera_ray`, matching
    /// `ssr_project`. Returns `None` when off-screen or behind the camera.
    fn project(&self, p: [f32; 3]) -> Option<(u32, u32)> {
        let aspect = self.w as f32 / self.h as f32;
        let rel = [p[0] - self.cam[0], p[1] - self.cam[1], p[2] - self.cam[2]];
        let z = dot3(rel, self.fwd);
        let (sx, sy) = if self.ortho {
            (
                dot3(rel, self.right) / (self.ortho_half_h * aspect),
                dot3(rel, self.up) / self.ortho_half_h,
            )
        } else {
            if z <= 1e-4 {
                return None;
            }
            (
                dot3(rel, self.right) / (z * aspect * self.fov_scale),
                dot3(rel, self.up) / (z * self.fov_scale),
            )
        };
        if self.ortho && z <= 1e-4 {
            return None;
        }
        if sx.abs() >= 1.0 || sy.abs() >= 1.0 {
            return None;
        }
        let fx = (sx + 1.0) * 0.5 * self.w as f32 - 0.5;
        let fy = (1.0 - sy) * 0.5 * self.h as f32 - 0.5;
        let ix = fx.round();
        let iy = fy.round();
        if ix < 0.0 || iy < 0.0 || ix >= self.w as f32 || iy >= self.h as f32 {
            return None;
        }
        Some((ix as u32, iy as u32))
    }

    /// The quantity gbuf_normal.w stores: ray parameter along camera_ray().
    /// Perspective = |rel|; ortho = forward distance. Mirrors gbuf_depth().
    fn metric(&self, p: [f32; 3]) -> f32 {
        let rel = [p[0] - self.cam[0], p[1] - self.cam[1], p[2] - self.cam[2]];
        if self.ortho { dot3(rel, self.fwd) } else { dot3(rel, rel).sqrt() }
    }

    fn idx(&self, px: u32, py: u32) -> usize {
        (py * self.w + px) as usize
    }
}

fn build_scene(w: u32, h: u32, ortho: bool, with_env: bool) -> Scene {
    build_scene_v(w, h, ortho, with_env, false, false)
}

/// `bent` bakes a real octahedral bent normal into the AO texture; `bright_horizon`
/// swaps in an environment whose blocked directions are brighter than the
/// hemisphere average. Both exist to give Gate 3's untested terms signal.
fn build_scene_v(
    w: u32,
    h: u32,
    ortho: bool,
    with_env: bool,
    bent: bool,
    bright_horizon: bool,
) -> Scene {
    let mut cfg = crate::RenderConfig::default();
    cfg.width = w as usize;
    cfg.height = h as usize;
    // Frame the floor/wall corner from slightly above and to the side.
    cfg.cam_orbit = glam::Quat::from_rotation_y(0.35) * glam::Quat::from_rotation_x(-0.45);
    cfg.cam_dist = 4.0;
    cfg.cam_target = glam::Vec3::new(0.0, 0.35, 0.10);
    cfg.ortho = ortho;
    let env = if with_env { Some(EnvMap::build_v(32, 16, bright_horizon)) } else { None };
    cfg.env_width = env.as_ref().map_or(0, |e| e.w);
    cfg.env_height = env.as_ref().map_or(0, |e| e.h);
    cfg.env_brightness = 1.0;
    let gu = cfg.to_gpu_uniforms();

    let cam = [gu.cam_pos[0], gu.cam_pos[1], gu.cam_pos[2]];
    let fwd = [gu.cam_forward[0], gu.cam_forward[1], gu.cam_forward[2]];
    let right = [gu.cam_right[0], gu.cam_right[1], gu.cam_right[2]];
    let up = [gu.cam_up[0], gu.cam_up[1], gu.cam_up[2]];
    let fov_scale = if gu.cam_forward[3] <= 0.0 { 1.0 } else { gu.cam_forward[3] };
    // cam_pos.w carries the ortho half-height (0 = perspective).
    let ortho_half_h = gu.cam_pos[3];

    let n_px = (w * h) as usize;
    let mut s = Scene {
        w,
        h,
        ortho,
        bent,
        gu,
        cam,
        fwd,
        right,
        up,
        fov_scale,
        ortho_half_h,
        env,
        gbuf_normal: vec![f16::from_f32(0.0); n_px * 4],
        gbuf_material: vec![f16::from_f32(0.0); n_px * 4],
        gbuf_albedo: vec![f16::from_f32(0.0); n_px * 4],
        hdr: vec![f16::from_f32(0.0); n_px * 4],
        obj: vec![None; n_px],
    };

    for py in 0..h {
        for px in 0..w {
            let i = s.idx(px, py);
            let (o, d) = s.camera_ray(px, py);
            let Some(hit) = trace(o, d, 0.0) else {
                // Background: depth 0.0 is the sentinel every consumer tests.
                s.gbuf_albedo[i * 4 + 3] = f16::from_f32(0.0);
                continue;
            };
            let albedo = obj_albedo(hit.obj);
            let lit = obj_hdr(hit.obj);
            s.gbuf_normal[i * 4] = f16::from_f32(hit.n[0]);
            s.gbuf_normal[i * 4 + 1] = f16::from_f32(hit.n[1]);
            s.gbuf_normal[i * 4 + 2] = f16::from_f32(hit.n[2]);
            // Ray-parameter depth — the convention `gbuf_depth()` writes in BOTH
            // projections since the 2026-08-06 ortho fix.
            s.gbuf_normal[i * 4 + 3] = f16::from_f32(hit.t);
            s.gbuf_material[i * 4] = f16::from_f32(0.8); // roughness
            s.gbuf_material[i * 4 + 1] = f16::from_f32(0.0); // metallic (.y!)
            s.gbuf_material[i * 4 + 2] = f16::from_f32(1.0); // shadow*AO
            s.gbuf_material[i * 4 + 3] = f16::from_f32(1.0); // opacity, no emission
            for c in 0..3 {
                s.gbuf_albedo[i * 4 + c] = f16::from_f32(albedo[c]);
                s.hdr[i * 4 + c] = f16::from_f32(lit[c]);
            }
            s.gbuf_albedo[i * 4 + 3] = f16::from_f32(FLAG_GEOMETRY);
            s.hdr[i * 4 + 3] = f16::from_f32(1.0);
            s.obj[i] = Some(hit.obj);
        }
    }
    s
}

// ── The CPU oracle — SSGI_PLAN.md §4.2 ──────────────────────────────────────

/// Independent evaluation of the plan's delta at one full-res pixel.
///
/// AO is 1.0 everywhere in this harness (the production "SSAO disabled / white
/// fill" state), so `ssao_f = 1`, `mb = multi_bounce_ao(1, albedo) ~= 1`, and the
/// bent normal degenerates to the geometric normal — exactly what
/// `ao_bent_normal` returns when the AO texture carries no bent data. Gate 3
/// extends this once AO is live.
///
/// `env_occl` is the user-facing **HDRI Occlusion** strength (advanced HDR
/// setting, 0..1, default 1.0). It scales how much environment light SSGI is
/// allowed to REMOVE in directions it proved are blocked:
///   1.0 = energy-consistent — the sky the geometry occludes is subtracted, which
///         is what produces the signed delta (red gained, blue removed).
///   0.0 = additive only — bounce light is added and the sky is never removed,
///         which is knowingly non-physical but is the softer, brighter look and
///         the same shape the no-HDRI path already has.
/// Below 1.0 this deliberately breaks energy conservation; that is an artistic
/// control, the precedent being option 2 in `SSAO_VISIBILITY_FINDING.md`.
fn oracle_delta(s: &Scene, px: u32, py: u32, env_occl: f32) -> [f32; 3] {
    oracle_delta_r(s, px, py, env_occl, f32::INFINITY)
}

/// As `oracle_delta`, bounded by the march reach `radius` (world units).
fn oracle_delta_r(s: &Scene, px: u32, py: u32, env_occl: f32, radius: f32) -> [f32; 3] {
    let i = s.idx(px, py);
    let Some(_) = s.obj[i] else { return [0.0; 3] };

    let n = [
        s.gbuf_normal[i * 4].to_f32(),
        s.gbuf_normal[i * 4 + 1].to_f32(),
        s.gbuf_normal[i * 4 + 2].to_f32(),
    ];
    let depth = s.gbuf_normal[i * 4 + 3].to_f32();
    let albedo = [
        s.gbuf_albedo[i * 4].to_f32(),
        s.gbuf_albedo[i * 4 + 1].to_f32(),
        s.gbuf_albedo[i * 4 + 2].to_f32(),
    ];
    let metal = s.gbuf_material[i * 4 + 1].to_f32(); // .y — NOT .x
    let opacity = s.gbuf_material[i * 4 + 3].to_f32().min(1.0);
    let ssao_f = 1.0f32;
    let mb = multi_bounce_ao(ssao_f, albedo);
    let ao_d = 1.0f32; // raster receiver => deferred forced final_ao = 1.0

    // The normal deferred evaluated its env diffuse at. `ao_bent_normal` returns
    // the geometric normal when the AO texture carries no bent data, which is the
    // harness default; `bent` scenes bake a real one, and then geometric-vs-bent
    // becomes a measurable difference (Gate 3 term (b)).
    let n_s = if s.bent { oct_decode(oct_encode(harness_bent_normal())) } else { n };

    let (o, d) = s.camera_ray(px, py);
    let p = add3(o, mul3(d, depth));

    // Half-res texel this pixel belongs to — the ign rotation is keyed on it.
    let (hx, hy) = (px / 2, py / 2);
    let dirs = cosine_dirs(n_s, hx, hy);

    let mut l_sum = [0.0f32; 3];
    let mut s_sum = [0.0f32; 3];
    for dir in &dirs {
        // Bias off the origin surface the way the shader does.
        let origin = add3(p, mul3(n_s, 1e-3));
        let Some(hit) = trace(origin, *dir, 1e-4) else { continue };
        // The GPU march reaches `ssgi.radius` and no further. An oracle that
        // traces to infinity would report bounce light the shader is structurally
        // unable to gather, and the mismatch would look like a formula error.
        if hit.t > radius { continue; }
        // Screen-space GI can only gather what is on screen — and that constraint
        // applies to the whole ray PATH, not just its endpoint. The GPU march
        // breaks the instant a step projects outside the frame, so a hit reachable
        // only via an off-screen detour is unreachable in practice.
        //
        // Modelling this is what makes the oracle a model of the TECHNIQUE rather
        // than of idealised world-space GI. Without it the oracle over-reports at
        // exactly the probes whose rays sweep out of frame (measured: perspective
        // far probe, oracle -0.114 vs GPU 0.000), and that gap reads like a shader
        // bug when it is really the oracle describing a different algorithm.
        let hp = add3(origin, mul3(*dir, hit.t));
        let mut path_on_screen = true;
        for k in 1..=8 {
            let t = hit.t * (k as f32) / 8.0;
            if s.project(add3(origin, mul3(*dir, t))).is_none() {
                path_on_screen = false;
                break;
            }
        }
        let Some((hx, hy)) = s.project(hp) else { continue };
        if !path_on_screen {
            continue;
        }
        // NOTE — the oracle deliberately stops modelling the technique HERE.
        //
        // A screen-space march can only register a hit whose surface is the one
        // actually STORED at its pixel; a hit near another surface's silhouette
        // (the floor/wall junction) resolves to the wrong surface and is invisible
        // to it. Adding that check was tried: it moved the perspective numbers
        // substantially in BOTH directions (probe0 oracle 0.203 -> 0.096 against an
        // unchanged GPU 0.203), because reproducing it faithfully needs the march's
        // own pixel rounding and depth tolerance.
        //
        // That is the limit of the oracle idea: an oracle that models screen-space
        // resolvability exactly IS a screen-space march, and then it can no longer
        // catch a bug in the march. So it stays an analytic-visibility model, and
        // the residual perspective gap is interpreted, not asserted. See the Gate 3
        // status block below.
        // Backface: a valid hit faces against the ray.
        if dot3(hit.n, *dir) >= 0.0 {
            continue;
        }
        let lit = obj_hdr(hit.obj);
        for c in 0..3 {
            l_sum[c] += lit[c];
        }
        if let Some(env) = &s.env {
            let e = env.sample_raw(*dir);
            for c in 0..3 {
                s_sum[c] += e[c];
            }
        }
    }

    let inv_n = 1.0 / N_RAYS as f32;
    let applied = s.env.as_ref().map_or([0.0; 3], |e| e.sh_irradiance(n_s));
    let mut delta = [0.0f32; 3];
    for c in 0..3 {
        // Never remove more energy than deferred added (band-limiting guard).
        // C5: clamp includes ao·mb once — do not multiply again on subtraction.
        let s_c = (s_sum[c] * inv_n).min(ao_d * mb[c] * applied[c]);
        delta[c] = opacity
            * albedo[c]
            * (1.0 - metal)
            * (l_sum[c] * inv_n - env_occl * s_c);
    }
    delta
}

// ── Gate 0 ──────────────────────────────────────────────────────────────────

/// Floor probes at increasing distance from the red wall (which is at z = -1).
const PROBES: [[f32; 3]; 3] = [
    [0.0, 0.0, -0.90], // adjacent to the wall
    [0.0, 0.0, -0.30], // ~one wall-height away
    [0.0, 0.0, 1.20],  // far
];

#[test]
fn ssgi_gate0_harness_and_oracle_baseline() {
    // Sanity: multi_bounce_ao must reduce to 1 at full visibility, or the oracle's
    // "mb ~= 1" simplification silently biases every number below.
    for a in [FLOOR_ALBEDO, WALL_ALBEDO] {
        let mb = multi_bounce_ao(1.0, a);
        for c in 0..3 {
            assert!(
                (mb[c] - 1.0).abs() < 1e-3,
                "multi_bounce_ao(1.0, {a:?})[{c}] = {} — expected 1.0",
                mb[c]
            );
        }
    }

    for &ortho in &[false, true] {
        for &with_env in &[true, false] {
            let s = build_scene(320, 240, ortho, with_env);
            let label = format!(
                "{} / {}",
                if ortho { "ortho" } else { "persp" },
                if with_env { "HDRI" } else { "no-HDRI" }
            );

            // The environment must be directional, or the subtraction term is a
            // constant and Gate 3 cannot distinguish a wrong env function.
            if let Some(env) = &s.env {
                let up = env.sh_irradiance([0.0, 1.0, 0.0]);
                let down = env.sh_irradiance([0.0, -1.0, 0.0]);
                assert!(
                    up[2] > down[2] * 1.5,
                    "{label}: env not directional (up.b={} down.b={})",
                    up[2],
                    down[2]
                );
                // Layout check: 7 levels of 32x16 halving, then 27 SH floats.
                let expect = (32 * 16 + 16 * 8 + 8 * 4 + 4 * 2 + 2 * 1 + 1 + 1) * 3 + 27;
                assert_eq!(env.buffer.len(), expect, "{label}: env buffer layout");
            }

            for (k, probe) in PROBES.iter().enumerate() {
                let Some((px, py)) = s.project(*probe) else {
                    panic!("{label}: probe {k} {probe:?} is off-screen — reframe the camera");
                };
                let i = s.idx(px, py);
                assert_eq!(
                    s.obj[i],
                    Some(Obj::Floor),
                    "{label}: probe {k} did not land on the floor (got {:?})",
                    s.obj[i]
                );
                let base = [
                    s.hdr[i * 4].to_f32(),
                    s.hdr[i * 4 + 1].to_f32(),
                    s.hdr[i * 4 + 2].to_f32(),
                ];
                let d = oracle_delta(&s, px, py, 1.0);
                eprintln!(
                    "[SSGI G0] {label} probe{k} px=({px},{py}) base=[{:.4},{:.4},{:.4}] \
                     oracleΔ=[{:+.4},{:+.4},{:+.4}]",
                    base[0], base[1], base[2], d[0], d[1], d[2]
                );
            }
        }
    }
}

/// **HDRI Occlusion** (advanced HDR setting) must do exactly what it claims.
///
/// At strength 1.0 the delta is energy-consistent: the sky SSGI proves is blocked
/// gets subtracted, which is what makes the delta signed (red gained, blue
/// removed). At 0.0 the subtraction is disabled, so bounce light is added and the
/// environment is never removed — which must land on **exactly** the same numbers
/// the no-HDRI path already produces, because that path has `L_env = 0` for the
/// same reason. That equality is the whole justification for the control being a
/// single scalar rather than a second code path, so it is asserted, not assumed.
#[test]
fn ssgi_gate0_hdri_occlusion_zero_matches_no_hdri() {
    for &ortho in &[false, true] {
        let with = build_scene(320, 240, ortho, true);
        let without = build_scene(320, 240, ortho, false);
        let label = if ortho { "ortho" } else { "persp" };
        for (k, probe) in PROBES.iter().enumerate() {
            let (px, py) = with.project(*probe).expect("probe on screen");
            let full = oracle_delta(&with, px, py, 1.0);
            let off = oracle_delta(&with, px, py, 0.0);
            let no_env = oracle_delta(&without, px, py, 1.0);
            eprintln!(
                "[SSGI G0] {label} probe{k} occl=1 [{:+.4},{:+.4},{:+.4}]  \
                 occl=0 [{:+.4},{:+.4},{:+.4}]  no-HDRI [{:+.4},{:+.4},{:+.4}]",
                full[0], full[1], full[2], off[0], off[1], off[2], no_env[0], no_env[1], no_env[2]
            );
            for c in 0..3 {
                assert!(
                    (off[c] - no_env[c]).abs() < 1e-5,
                    "{label} probe{k} ch{c}: HDRI Occlusion 0 ({}) != no-HDRI ({})",
                    off[c],
                    no_env[c]
                );
                // Turning occlusion up may only ever remove light, never add it.
                assert!(
                    full[c] <= off[c] + 1e-5,
                    "{label} probe{k} ch{c}: occlusion 1.0 ({}) brighter than 0.0 ({})",
                    full[c],
                    off[c]
                );
            }
            // At full strength the blue channel must actually go negative near the
            // wall — if it does not, the subtraction is inert and the setting is a
            // no-op dressed as a control.
            if k == 0 {
                assert!(full[2] < -0.05, "{label}: env subtraction inert ({})", full[2]);
            }
        }
    }
}

/// The property Gate 2 will assert on the GPU, proven first on the oracle: the
/// floor picks up RED near the red wall, and it falls off with distance. If this
/// fails, the scene is not exercising GI and every later gate is measuring noise.
#[test]
fn ssgi_gate0_oracle_shows_directional_falloff() {
    for &ortho in &[false, true] {
        let s = build_scene(320, 240, ortho, true);
        let label = if ortho { "ortho" } else { "persp" };
        let mut reds = Vec::new();
        for probe in PROBES.iter() {
            let (px, py) = s.project(*probe).expect("probe on screen");
            let d = oracle_delta(&s, px, py, 1.0);
            reds.push(d[0] - 0.5 * (d[1] + d[2]));
        }
        eprintln!("[SSGI G0] {label} red-excess by distance: {reds:?}");
        assert!(
            reds[0] > 0.0,
            "{label}: no red bounce adjacent to the red wall ({})",
            reds[0]
        );
        // Non-increasing between adjacent probes, and a decisive drop across the
        // full range. Adjacent probes are allowed to TIE: with N_RAYS = 8 the hit
        // count is quantized, so two nearby probes can resolve the same number of
        // wall hits (measured: ortho probe0 0.3869 vs probe1 0.3831). Asserting a
        // strict drop between neighbours would be a camera-dependent coin flip —
        // the real property is that bleed falls off with distance from the wall.
        assert!(
            reds[0] >= reds[1] - 1e-4 && reds[1] >= reds[2] - 1e-4,
            "{label}: red bounce increases with distance from the wall: {reds:?}"
        );
        assert!(
            reds[0] > reds[2] * 1.5,
            "{label}: red bounce does not fall off across the probe range: {reds:?}"
        );
    }
}

// ── Gate 1: the real pipeline, dispatched on real hardware ──────────────────

fn try_device() -> Option<(wgpu::Device, wgpu::Queue)> {
    let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
        backends: wgpu::Backends::PRIMARY,
        ..Default::default()
    });
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        compatible_surface: None,
        force_fallback_adapter: false,
    })).ok()?;
    pollster::block_on(adapter.request_device(
        &wgpu::DeviceDescriptor {
            label: Some("ssgi_test_device"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::default(),
            memory_hints: wgpu::MemoryHints::default(),
            trace: wgpu::Trace::Off,
        },
    ))
    .ok()
}

fn upload_rgba16f(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    label: &str,
    w: u32,
    h: u32,
    texels: &[f16],
) -> wgpu::Texture {
    let tex = device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba16Float,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    let bytes: Vec<u8> = texels.iter().flat_map(|v| v.to_le_bytes()).collect();
    queue.write_texture(
        tex.as_image_copy(),
        &bytes,
        wgpu::ImageDataLayout { offset: 0, bytes_per_row: Some(w * 8), rows_per_image: Some(h) },
        wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
    );
    tex
}

fn read_delta(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    res: &crate::ssgi::SsgiResources,
) -> Vec<[f32; 4]> {
    let (w, h) = res.half_size;
    let row = w * 8;
    let padded = (row + 255) & !255;
    let buf = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("ssgi_read"),
        size: (padded * h) as u64,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut enc = device.create_command_encoder(&Default::default());
    enc.copy_texture_to_buffer(
        res.output_texture().as_image_copy(),
        wgpu::ImageCopyBuffer {
            buffer: &buf,
            layout: wgpu::ImageDataLayout {
                offset: 0,
                bytes_per_row: Some(padded),
                rows_per_image: Some(h),
            },
        },
        wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
    );
    queue.submit([enc.finish()]);
    let (tx, rx) = std::sync::mpsc::channel();
    buf.slice(..).map_async(wgpu::MapMode::Read, move |r| tx.send(r).unwrap());
    device.poll(wgpu::PollType::Wait).expect("GPU polling failed");
    rx.recv().expect("map channel").expect("map failed");
    let data = buf.slice(..).get_mapped_range();
    let mut out = Vec::with_capacity((w * h) as usize);
    for y in 0..h {
        let r = &data[(y * padded) as usize..][..row as usize];
        for x in 0..w {
            let t = &r[(x * 8) as usize..][..8];
            out.push([
                f16::from_le_bytes([t[0], t[1]]).to_f32(),
                f16::from_le_bytes([t[2], t[3]]).to_f32(),
                f16::from_le_bytes([t[4], t[5]]).to_f32(),
                f16::from_le_bytes([t[6], t[7]]).to_f32(),
            ]);
        }
    }
    drop(data);
    buf.unmap();
    out
}

/// Everything the SSGI bind group needs, kept alive across the dispatch.
struct GpuScene {
    _texs: Vec<wgpu::Texture>,
    views: Vec<wgpu::TextureView>,
    uniform: wgpu::Buffer,
    env: wgpu::Buffer,
}

fn upload_scene(device: &wgpu::Device, queue: &wgpu::Queue, s: &Scene) -> GpuScene {
    use wgpu::util::DeviceExt;
    let (w, h) = (s.w, s.h);
    let gn = upload_rgba16f(device, queue, "t_gn", w, h, &s.gbuf_normal);
    let gm = upload_rgba16f(device, queue, "t_gm", w, h, &s.gbuf_material);
    let ga = upload_rgba16f(device, queue, "t_ga", w, h, &s.gbuf_albedo);
    // raster_depth is a COPY of gbuf_normal in production (Hybrid mode) — which
    // is exactly what makes the `is_raster` receiver gate true for these pixels.
    let rd = upload_rgba16f(device, queue, "t_rd", w, h, &s.gbuf_normal);
    let hdr = upload_rgba16f(device, queue, "t_hdr", w, h, &s.hdr);
    // AO: half-res, r=1 with a NON-zero guide depth — the production
    // "SSAO disabled / white fill" state the CPU oracle also assumes.
    let (hw, hh) = ((w + 1) / 2, (h + 1) / 2);
    let mut ao = vec![f16::from_f32(0.0); (hw * hh * 4) as usize];
    let benc = oct_encode(harness_bent_normal());
    for i in 0..(hw * hh) as usize {
        let (hx, hy) = (i as u32 % hw, i as u32 / hw);
        // The guide depth must be the REAL half-res depth, not a placeholder.
        // Both ssao_factor and ao_bent_normal weight taps by
        // exp(-(|s.g - depth| / depth)^2 * 400): a constant 1.0 against a scene
        // depth of ~7 underflows that to zero, every tap is rejected, and
        // ao_bent_normal falls back to the GEOMETRIC normal. The bent normal was
        // being uploaded and then correctly discarded — which is why the GPU output
        // was byte-identical with and without it.
        let (cx, cy) = ((hx * 2).min(w - 1), (hy * 2).min(h - 1));
        let d = s.gbuf_normal[(((cy * w) + cx) * 4 + 3) as usize];
        ao[i * 4] = f16::from_f32(1.0); // AO factor: fully open
        ao[i * 4 + 1] = d;              // guide depth
        if s.bent && d.to_f32() > 0.0 {
            // b,a = octahedral bent normal. Zero here means "no bent normal" and
            // ao_bent_normal() falls back to the geometric one — which is why the
            // default harness could not tell the two apart.
            ao[i * 4 + 2] = f16::from_f32(benc[0]);
            ao[i * 4 + 3] = f16::from_f32(benc[1]);
        }
    }
    let ao_t = upload_rgba16f(device, queue, "t_ao", hw, hh, &ao);
    let uniform = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("t_uniform"),
        contents: s.gu.as_bytes(),
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
    });
    let env_floats: Vec<f32> = s.env.as_ref().map_or_else(|| vec![0.0; 4], |e| e.buffer.clone());
    let env = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("t_env"),
        contents: bytemuck::cast_slice(&env_floats),
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
    });
    let texs = vec![gn, gm, ga, rd, hdr, ao_t];
    let views = texs.iter().map(|t| t.create_view(&Default::default())).collect();
    GpuScene { _texs: texs, views, uniform, env }
}

/// THE Gate 1 acceptance gate: the real pipeline, dispatched, read back.
///
/// Acceptance 4 — a freshly created resource already reads zero, because the
/// composite samples it unconditionally and the pipeline may still be compiling
/// on that frame. Acceptance 1 — a zero constant produces EXACTLY zero, so the
/// composite is provably inert rather than inert-by-argument. Acceptance 2 — a
/// non-zero constant reaches exactly the receiver pixels and no others.
///
/// This is the evidence the plan's revision-1 review demanded: "neutral by
/// construction" is the reasoning that shipped six defects the first time.
#[test]
fn ssgi_gate1_composite_is_neutral_and_hits_only_receivers() {
    let Some((device, queue)) = try_device() else {
        eprintln!("ssgi_gate1: no GPU adapter — skipped");
        return;
    };
    let s = build_scene(320, 240, false, true);
    let g = upload_scene(&device, &queue, &s);
    let pipelines = crate::ssgi::SsgiPipelines::new(&device, None);
    let mut res = crate::ssgi::SsgiResources::new(&device, &queue, s.w, s.h);

    let fresh = read_delta(&device, &queue, &res);
    assert!(
        fresh.iter().all(|t| t[0] == 0.0 && t[1] == 0.0 && t[2] == 0.0),
        "a freshly created SSGI texture is not zero — the composite would add undefined \
         memory on every frame before the first dispatch"
    );

    let mut run = |res: &mut crate::ssgi::SsgiResources, konst: f32| {
        let params = crate::types::SsgiParams {
            half_size: [res.half_size.0, res.half_size.1],
            full_size: [s.w, s.h],
            intensity: 1.0,
            radius: 1.5,
            env_occlusion: 1.0,
            rays: N_RAYS,
            debug_flags: 1, // DBG_FORCE_CONST
            dbg_const: konst,
            frame_index: 0, probe_diffuse_w: 0.0,
        };
        let mut enc = device.create_command_encoder(&Default::default());
        crate::ssgi::dispatch(
            &mut enc, &device, &queue, &pipelines, res, &g.uniform,
            &g.views[4], &g.views[0], &g.views[1], &g.views[2], &g.views[3],
            &g.env, &g.views[5], &params, true, None,
        );
        queue.submit([enc.finish()]);
        read_delta(&device, &queue, res)
    };

    let zero = run(&mut res, 0.0);
    for (i, t) in zero.iter().enumerate() {
        assert!(
            t[0] == 0.0 && t[1] == 0.0 && t[2] == 0.0,
            "texel {i}: zero-constant dispatch produced {t:?} — the composite is not inert"
        );
    }

    const K: f32 = 0.25;
    let lit = run(&mut res, K);
    let (hw, _hh) = res.half_size;
    let (mut recv, mut rejected) = (0usize, 0usize);
    for (i, t) in lit.iter().enumerate() {
        let (hx, hy) = (i as u32 % hw, i as u32 / hw);
        // The full-res pixel this half texel represents (the gather's c = 2h).
        let (cx, cy) = (hx * 2, hy * 2);
        let is_geo = cx < s.w && cy < s.h && s.obj[s.idx(cx, cy)].is_some();
        if is_geo {
            recv += 1;
            assert!((t[0] - K).abs() < 1e-3, "receiver ({hx},{hy}) got {t:?}, expected {K}");
        } else {
            rejected += 1;
            assert!(
                t[0] == 0.0 && t[1] == 0.0 && t[2] == 0.0,
                "NON-receiver ({hx},{hy}) received {t:?} — the background/floor-grid class \
                 must stay bit-unchanged"
            );
        }
    }
    eprintln!("[SSGI G1] receivers={recv} rejected={rejected} (constant {K})");
    assert!(recv > 1000, "scene degenerate: only {recv} receiver texels");
    assert!(rejected > 100, "scene degenerate: only {rejected} rejected texels");

    let again = run(&mut res, K);
    assert_eq!(lit, again, "SSGI gather is not deterministic across runs");
}

/// Gate 2 + Gate 3: the REAL march, cross-checked against the CPU oracle.
///
/// Gate 2 asks whether light bounces at all and falls off with distance. Gate 3
/// asks whether the number is *right* — and that is the one the plan's revision-1
/// review forced into existence, because a zero-hit "delta is zero" assertion is
/// true of any formula, including a wrong one.
///
/// The oracle traces the scene analytically while the GPU marches in screen space,
/// so exact agreement is not expected; the scene is built so every gathered ray
/// either hits one of two planes well inside the frame or escapes cleanly, which
/// keeps march error small. A LARGE divergence means a formula error — a channel
/// swap, the wrong normal, a prefiltered mip instead of raw radiance — not march
/// error.
#[test]
fn ssgi_gate2_march_bounces_light_and_matches_oracle() {
    let Some((device, queue)) = try_device() else {
        eprintln!("ssgi_gate2: no GPU adapter — skipped");
        return;
    };
    for &ortho in &[false, true] {
        for &with_env in &[true, false] {
            let s = build_scene(320, 240, ortho, with_env);
            let g = upload_scene(&device, &queue, &s);
            let pipelines = crate::ssgi::SsgiPipelines::new(&device, None);
            let mut res = crate::ssgi::SsgiResources::new(&device, &queue, s.w, s.h);
            let label = format!(
                "{}/{}",
                if ortho { "ortho" } else { "persp" },
                if with_env { "HDRI" } else { "no-HDRI" }
            );

            let params = crate::types::SsgiParams {
                half_size: [res.half_size.0, res.half_size.1],
                full_size: [s.w, s.h],
                intensity: 1.0,
                // 4.0, not 3.0: the diagnostic showed the perspective far probe's
                // ONLY wall hit sits at t=2.946 — 98% of a 3.0 reach — where the
                // start bias and f16 depth quantisation make registration a coin
                // flip. That is the march's range boundary, not its arithmetic, and
                // a gate pinned to it would measure the wrong thing. At 4.0 the same
                // hit sits at 74% and the comparison tests the energy formula.
                radius: 4.0,
                env_occlusion: 1.0,
                rays: N_RAYS,
                debug_flags: 0, // the real march
                dbg_const: 0.0,
                frame_index: 0, probe_diffuse_w: 0.0,
            };
            let mut enc = device.create_command_encoder(&Default::default());
            crate::ssgi::dispatch(
                &mut enc, &device, &queue, &pipelines, &mut res, &g.uniform,
                &g.views[4], &g.views[0], &g.views[1], &g.views[2], &g.views[3],
                &g.env, &g.views[5], &params, true, None,
            );
            queue.submit([enc.finish()]);
            let out = read_delta(&device, &queue, &res);
            let (hw, _) = res.half_size;

            let mut reds = Vec::new();
            for (k, probe) in PROBES.iter().enumerate() {
                let (px, py) = s.project(*probe).expect("probe on screen");
                let (hx, hy) = (px / 2, py / 2);
                let gpu = out[(hy * hw + hx) as usize];
                // The gather shaded full pixel c = 2h, which may differ from the
                // probe's own pixel by one — evaluate the oracle at the SAME pixel.
                let cpu = oracle_delta_r(&s, hx * 2, hy * 2, 1.0, params.radius);
                eprintln!(
                    "[SSGI G2] {label} probe{k} gpu=[{:+.4},{:+.4},{:+.4}] cpu=[{:+.4},{:+.4},{:+.4}]",
                    gpu[0], gpu[1], gpu[2], cpu[0], cpu[1], cpu[2]
                );
                reds.push(gpu[0] - 0.5 * (gpu[1] + gpu[2]));
            }

            // ── Gate 2: light bounces, and it falls off with distance ──
            assert!(
                reds[0] > 0.0,
                "{label}: no red bounce adjacent to the red wall ({})",
                reds[0]
            );
            assert!(
                reds[0] > reds[2] * 1.5,
                "{label}: bleed does not fall off across the probe range: {reds:?}"
            );

            // ── Gate 3 status: PARTIAL, and deliberately not asserted yet ──────
            //
            // The near and mid probes agree with the CPU oracle to 4 decimals in
            // BOTH projections and BOTH env configurations — 10 of 12 probes — which
            // is what validates the energy formula end to end: bent normal, .y
            // metalness, opacity, raw env radiance, the min(S, applied) clamp and
            // the shared ray set would each break that agreement if wrong.
            //
            // The two that do NOT agree are the perspective FAR probe, where the
            // analytic trace finds exactly ONE contributing ray out of eight at long
            // range and the screen-space march finds none. Confirmed NOT to be:
            // step count (96 steps did not fix it), stride scheme (world-uniform did
            // not fix it), march reach (radius 4.0 did not fix it), or ray-path
            // visibility. Root cause still unknown.
            //
            // Asserting `GPU ~= CPU to 1e-3` here today would mean weakening the
            // tolerance until this passed, which is precisely the "gate that went
            // green because we moved it" failure this plan exists to prevent (R5/R7).
            // So the divergence is PRINTED and tracked, not asserted away. Gate 3 is
            // not closed until it is explained.
            let worst = (0..3)
                .map(|k| {
                    let (px, py) = s.project(PROBES[k]).unwrap();
                    let (hx, hy) = (px / 2, py / 2);
                    let g = out[(hy * hw + hx) as usize];
                    let c = oracle_delta_r(&s, hx * 2, hy * 2, 1.0, params.radius);
                    (0..3).map(|i| (g[i] - c[i]).abs()).fold(0.0f32, f32::max)
                })
                .fold(0.0f32, f32::max);
            eprintln!("[SSGI G2] {label} worst |GPU-CPU| across probes = {worst:.4}");

            // ORTHOGRAPHIC is asserted, and asserted TIGHTLY. There the march steps
            // uniformly in world space by construction (ssgi_point_at degenerates to
            // a lerp), so screen-space resolvability does not confound the
            // comparison and GPU/CPU agree to f16 noise — measured 0.0002. That
            // makes it the real Gate 3 gate: every term of §4.2 must be right for it
            // to hold, and the four mandated sabotages each break it.
            //
            // Perspective is printed rather than asserted, for the reason documented
            // at the oracle's visibility note — NOT because the tolerance was widened
            // until it passed.
            // SABOTAGE RESULTS (2026-08-06) — the four §5 Gate 3 sabotages were run
            // against this assertion. TWO of them left it GREEN, which means this
            // scene does not exercise those terms at all:
            //
            //   (a) metalness .x instead of .y ........ RED  (0.3646) — covered
            //   (b) geometric normal instead of bent .. GREEN — NOT COVERED
            //   (c) prefiltered mip instead of raw .... RED  (0.2269) — covered
            //   (d) drop the min(S, applied) clamp .... GREEN — NOT COVERED
            //
            // (b) is green because the harness uploads an AO texture with a ZERO
            // octahedral bent normal, so ao_bent_normal() correctly falls back to
            // the geometric normal and the two are identical here. Covering it needs
            // a real encoded bent normal in the AO texture AND the same decode in
            // the oracle.
            //
            // (d) is green because in this scene the clamp never binds: hits point
            // along the horizon where the sky is dim, so the raw radiance in blocked
            // directions stays below the SH-reconstructed hemisphere average.
            // Covering it needs an environment with a bright horizon band in the
            // direction of the wall.
            //
            // Until both are covered, Gate 3 is NOT closed — a gate that cannot fail
            // on a term is not testing that term (R7).
            if ortho {
                assert!(
                    worst < 1e-3,
                    "{label}: GPU/CPU disagree by {worst} — the energy formula is wrong                      (ortho has no resolvability excuse)"
                );
            }
        }
    }
}

/// The receiver predicate against EVERY real flag band, not just "geometry vs
/// background".
///
/// Motivated by VG-A: the owner reported the floor going grey under a constant
/// delta, which turned out to be a shadow on a floor MESH (a correct receiver) —
/// but the test above could not have told the difference, because its synthetic
/// scene only contained geometry and background. The bands that must be rejected
/// (floor/workplane 0.5, ghost 0.61, VFX 0.68/0.88, DC wire 0.74) were never
/// present, so a predicate that accepted all of them would still have passed.
///
/// Band table: `render/src/gbuffer.wgsl:929`.
#[test]
fn ssgi_gate1_receiver_predicate_rejects_every_non_geometry_band() {
    let Some((device, queue)) = try_device() else {
        eprintln!("ssgi_gate1_bands: no GPU adapter — skipped");
        return;
    };
    // (flag, must_receive)
    const BANDS: [(f32, bool, &str); 8] = [
        (0.0, false, "background"),
        (0.5, false, "floor / workplane grid"),
        (0.61, false, "ghost (Focus mode)"),
        (0.68, false, "VFX unlit"),
        (0.74, false, "DC wire edge"),
        (0.78, true, "geometry"),
        (0.88, false, "VFX unlit (selected)"),
        (1.0, true, "geometry, selected"),
    ];
    let mut s = build_scene(64, 64, false, true);
    let pipelines = crate::ssgi::SsgiPipelines::new(&device, None);

    for (flag, must_receive, name) in BANDS {
        // Paint the whole G-buffer with one band. Depth/normal/raster stay valid,
        // so the ONLY thing under test is the flag classification.
        for i in 0..(s.w * s.h) as usize {
            if s.obj[i].is_some() {
                s.gbuf_albedo[i * 4 + 3] = f16::from_f32(flag);
            }
        }
        let g = upload_scene(&device, &queue, &s);
        let mut res = crate::ssgi::SsgiResources::new(&device, &queue, s.w, s.h);
        let params = crate::types::SsgiParams {
            half_size: [res.half_size.0, res.half_size.1],
            full_size: [s.w, s.h],
            intensity: 1.0,
            radius: 1.5,
            env_occlusion: 1.0,
            rays: N_RAYS,
            debug_flags: 1, // force constant
            dbg_const: 0.5,
            frame_index: 0, probe_diffuse_w: 0.0,
        };
        let mut enc = device.create_command_encoder(&Default::default());
        crate::ssgi::dispatch(
            &mut enc, &device, &queue, &pipelines, &mut res, &g.uniform,
            &g.views[4], &g.views[0], &g.views[1], &g.views[2], &g.views[3],
            &g.env, &g.views[5], &params, true, None,
        );
        queue.submit([enc.finish()]);
        let out = read_delta(&device, &queue, &res);
        let hit = out.iter().filter(|t| t[0] > 0.01).count();
        eprintln!("[SSGI G1 bands] flag {flag:.2} ({name}): {hit} texels received");
        if must_receive {
            assert!(hit > 50, "band {flag} ({name}) must RECEIVE indirect light, got {hit} texels");
        } else {
            assert_eq!(hit, 0, "band {flag} ({name}) must be REJECTED, but {hit} texels received");
        }
    }
}

/// GATE 3 CLOSURE — the two terms the first sabotage round proved were untested.
///
/// A gate that cannot fail on a term is not testing that term (R7). This scene is
/// built so that both terms carry SIGNAL, and it asserts that signal exists before
/// asserting the GPU matches — otherwise it would silently regress back into the
/// green-on-sabotage state it exists to fix.
///
///   (b) bent normal — the AO texture carries a real octahedral bent normal tilted
///       away from the wall, so using the geometric normal instead is a measurable
///       error rather than a no-op.
///   (d) min(S, applied) clamp — the environment has a bright horizon band in the
///       blocked directions, so the raw radiance SSGI would subtract EXCEEDS the
///       SH-reconstructed hemisphere average and the clamp actually binds.
///
/// Orthographic only, for the reason in the Gate 2 status block: it is the
/// projection where screen-space resolvability does not confound the comparison.
#[test]
fn ssgi_gate3_bent_normal_and_clamp_are_exercised() {
    let Some((device, queue)) = try_device() else {
        eprintln!("ssgi_gate3: no GPU adapter — skipped");
        return;
    };
    let s = build_scene_v(320, 240, true, true, true, true);
    assert!(s.bent, "scene must carry a bent normal");

    // ── Prove the scene has SIGNAL for each term ──
    let (px, py) = s.project(PROBES[0]).unwrap();
    let (hx, hy) = (px / 2, py / 2);
    let (cx, cy) = (hx * 2, hy * 2);

    // (b) bent vs geometric must differ materially at the probe.
    let mut s_geo = build_scene_v(320, 240, true, true, false, true);
    s_geo.bent = false;
    let with_bent = oracle_delta_r(&s, cx, cy, 1.0, 4.0);
    let with_geo = oracle_delta_r(&s_geo, cx, cy, 1.0, 4.0);
    let bent_signal = (0..3).map(|i| (with_bent[i] - with_geo[i]).abs()).fold(0.0f32, f32::max);
    eprintln!(
        "[SSGI G3] bent={with_bent:?} geo={with_geo:?} signal={bent_signal:.4}"
    );
    assert!(
        bent_signal > 0.01,
        "scene does not distinguish bent from geometric normal (signal {bent_signal}) —          sabotage (b) would pass green again"
    );

    // (d) the clamp must actually bind: unclamped != clamped.
    let i = s.idx(cx, cy);
    let n_s = oct_decode(oct_encode(harness_bent_normal()));
    let albedo = [
        s.gbuf_albedo[i * 4].to_f32(),
        s.gbuf_albedo[i * 4 + 1].to_f32(),
        s.gbuf_albedo[i * 4 + 2].to_f32(),
    ];
    let applied = s.env.as_ref().unwrap().sh_irradiance(n_s);
    let mb = multi_bounce_ao(1.0, albedo);
    let mut raw = [0.0f32; 3];
    let dirs = cosine_dirs(n_s, hx, hy);
    let (o, d) = s.camera_ray(cx, cy);
    let p = add3(o, mul3(d, s.gbuf_normal[i * 4 + 3].to_f32()));
    let mut hits = 0;
    for dir in &dirs {
        let origin = add3(p, mul3(n_s, 1e-3));
        if let Some(h) = trace(origin, *dir, 1e-4) {
            if h.t <= 4.0 && s.project(add3(origin, mul3(*dir, h.t))).is_some() {
                hits += 1;
                let e = s.env.as_ref().unwrap().sample_raw(*dir);
                for k in 0..3 {
                    raw[k] += e[k];
                }
            }
        }
    }
    let inv_n = 1.0 / dirs.len() as f32;
    let binds = (0..3).any(|k| raw[k] * inv_n > mb[k] * applied[k] + 1e-4);
    eprintln!(
        "[SSGI G3] hits={hits} raw/N={:?} applied*mb={:?} clamp_binds={binds}",
        raw.map(|v| v * inv_n),
        (0..3).map(|k| mb[k] * applied[k]).collect::<Vec<_>>()
    );
    assert!(
        binds,
        "the min(S, applied) clamp never binds in this scene — sabotage (d) would          pass green again"
    );

    // ── Now the actual cross-check ──
    let g = upload_scene(&device, &queue, &s);
    let pipelines = crate::ssgi::SsgiPipelines::new(&device, None);
    let mut res = crate::ssgi::SsgiResources::new(&device, &queue, s.w, s.h);
    let params = crate::types::SsgiParams {
        half_size: [res.half_size.0, res.half_size.1],
        full_size: [s.w, s.h],
        intensity: 1.0,
        radius: 4.0,
        env_occlusion: 1.0,
        rays: N_RAYS,
        debug_flags: 0,
        dbg_const: 0.0,
        frame_index: 0, probe_diffuse_w: 0.0,
    };
    let mut enc = device.create_command_encoder(&Default::default());
    crate::ssgi::dispatch(
        &mut enc, &device, &queue, &pipelines, &mut res, &g.uniform,
        &g.views[4], &g.views[0], &g.views[1], &g.views[2], &g.views[3],
        &g.env, &g.views[5], &params, true, None,
    );
    queue.submit([enc.finish()]);
    let out = read_delta(&device, &queue, &res);
    let (hw, _) = res.half_size;

    let mut worst = 0.0f32;
    for (k, probe) in PROBES.iter().enumerate() {
        let (px, py) = s.project(*probe).unwrap();
        let (hx, hy) = (px / 2, py / 2);
        let gpu = out[(hy * hw + hx) as usize];
        let cpu = oracle_delta_r(&s, hx * 2, hy * 2, 1.0, params.radius);
        eprintln!(
            "[SSGI G3] probe{k} gpu=[{:+.4},{:+.4},{:+.4}] cpu=[{:+.4},{:+.4},{:+.4}]",
            gpu[0], gpu[1], gpu[2], cpu[0], cpu[1], cpu[2]
        );
        for i in 0..3 {
            // RELATIVE error. This scene deliberately runs a 40x horizon band to
            // make the clamp bind, so channel values reach ~3 and an absolute
            // tolerance would be measuring f16 storage rather than the formula.
            worst = worst.max((gpu[i] - cpu[i]).abs() / cpu[i].abs().max(0.1));
        }
    }
    eprintln!("[SSGI G3] worst relative |GPU-CPU| = {:.4}", worst);
    // 3%: comfortably above the residual from the GPU's 4-tap bent-normal upsample
    // (the oracle uses the exact encoded normal), and far below every sabotage —
    // the smallest of which moves the result by ~7%. Verified by re-running all
    // four against this assertion; see the sabotage table in the Gate 2 block.
    assert!(
        worst < 0.03,
        "GPU/CPU disagree by {worst} relative with bent normal + binding clamp"
    );
}
