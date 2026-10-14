// core_engine/core_math/src/modifiers.rs
// Modifier System — domain warps, surface alterations, volume masks, blend profiles
use glam::Vec3;
use serde::{Serialize, Deserialize};
// rem_euclid used by DisplaceHeightMap UV wrap

// ============================================================
// Blend Falloff — position-dependent k modulation
// ============================================================

/// Axis-aligned direction codes for blend falloff mask.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[repr(u8)]
pub enum FalloffAxis {
    #[default]
    None = 0,
    PosX = 1, NegX = 2,
    PosY = 3, NegY = 4,
    PosZ = 5, NegZ = 6,
}

impl FalloffAxis {
    pub fn to_vec3(self) -> Vec3 {
        match self {
            Self::None => Vec3::ZERO,
            Self::PosX => Vec3::X,  Self::NegX => Vec3::NEG_X,
            Self::PosY => Vec3::Y,  Self::NegY => Vec3::NEG_Y,
            Self::PosZ => Vec3::Z,  Self::NegZ => Vec3::NEG_Z,
        }
    }
    pub fn code(self) -> u32 { self as u32 }
    pub fn from_code(c: u32) -> Self {
        match c { 1=>Self::PosX, 2=>Self::NegX, 3=>Self::PosY, 4=>Self::NegY, 5=>Self::PosZ, 6=>Self::NegZ, _=>Self::None }
    }
}

/// 3 blend-K curve shapes. User picks a type and drags the handle (`curve_power`) to reshape.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u32)]
pub enum BlendKCurve {
    /// Straight ramp 0→1.
    Linear = 0,
    /// Hermite smoothstep (default soft blend).
    #[default]
    Smooth = 1,
    /// Power curve: `t^power` — drag handle to bias hard/soft.
    Power = 2,
}

impl BlendKCurve {
    pub fn label(self) -> &'static str {
        match self {
            Self::Linear => "Linear",
            Self::Smooth => "Smooth",
            Self::Power => "Power",
        }
    }
    pub fn all() -> &'static [BlendKCurve] {
        &[Self::Linear, Self::Smooth, Self::Power]
    }
    pub fn from_u32(v: u32) -> Self {
        match v {
            0 => Self::Linear,
            2 => Self::Power,
            _ => Self::Smooth,
        }
    }
}

/// Position-dependent blend falloff. When active, k is modulated by a
/// directional curve: `k_local = k * curve(dot(p - origin, direction) / length)`.
///
/// **Protect walls** (`wall_protect > 0`): fades k near cutter side faces.
/// **Curve** (`curve_type` + `curve_power`): 3 shapes; drag power to reshape the profile.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BlendFalloff {
    pub direction: Vec3,      // normalized direction vector (zero = inactive)
    pub origin_offset: f32,   // signed offset from prim center along direction (world units)
    pub length: f32,          // ramp distance from k=0 to k=full (world units)
    /// Distance from lateral walls over which k ramps 0→1. `0` = off.
    #[serde(default)]
    pub wall_protect: f32,
    /// Which wall pair to protect: 0=Auto, 1=X, 2=Y, 3=Z.
    #[serde(default)]
    pub wall_protect_axis: u32,
    /// Blend K curve type (Linear / Smooth / Power).
    #[serde(default)]
    pub curve_type: BlendKCurve,
    /// Power handle for [`BlendKCurve::Power`] (also mild bias for Smooth). Default 2.0.
    #[serde(default = "default_curve_power")]
    pub curve_power: f32,
}

fn default_curve_power() -> f32 {
    2.0
}

impl Default for BlendFalloff {
    fn default() -> Self {
        Self {
            direction: Vec3::ZERO,
            origin_offset: 0.0,
            length: 1.0,
            wall_protect: 0.0,
            wall_protect_axis: 0,
            curve_type: BlendKCurve::Smooth,
            curve_power: 2.0,
        }
    }
}

impl BlendFalloff {
    pub fn is_active(&self) -> bool {
        self.direction.length_squared() > 0.001 && self.length > 0.001
    }

    pub fn wall_protect_active(&self) -> bool {
        self.wall_protect > 0.001
    }

    /// Map raw 0..1 parameter through the selected blend curve.
    #[inline]
    pub fn eval_curve(t: f32, curve: BlendKCurve, power: f32) -> f32 {
        let t = t.clamp(0.0, 1.0);
        match curve {
            BlendKCurve::Linear => t,
            BlendKCurve::Smooth => {
                // Optional mild power bias: remap through smoothstep then power
                let s = t * t * (3.0 - 2.0 * t);
                let p = power.clamp(0.25, 8.0);
                if (p - 2.0).abs() < 0.01 {
                    s
                } else {
                    // p=2 is identity-ish on smooth; other values bias mid
                    s.powf(p * 0.5)
                }
            }
            BlendKCurve::Power => t.powf(power.clamp(0.25, 8.0)),
        }
    }

    /// Evaluate the directional falloff multiplier at world-space point `p`.
    /// Returns [0,1]: 0 = hard (no blend), 1 = full blend.
    pub fn evaluate(&self, p: Vec3, prim_pos: Vec3) -> f32 {
        if !self.is_active() {
            return 1.0;
        }
        let dir = self.direction.normalize_or_zero();
        let origin = prim_pos + dir * self.origin_offset;
        let t = ((p - origin).dot(dir) / self.length).clamp(0.0, 1.0);
        Self::eval_curve(t, self.curve_type, self.curve_power)
    }

    /// Curve 0→1 as `wall_dist` goes 0→margin (hard at wall, full blend inside).
    #[inline]
    pub fn wall_protect_factor(wall_dist: f32, margin: f32) -> f32 {
        Self::wall_protect_factor_curve(wall_dist, margin, BlendKCurve::Smooth, 2.0)
    }

    #[inline]
    pub fn wall_protect_factor_curve(
        wall_dist: f32,
        margin: f32,
        curve: BlendKCurve,
        power: f32,
    ) -> f32 {
        if margin <= 0.001 {
            return 1.0;
        }
        let t = (wall_dist / margin).clamp(0.0, 1.0);
        Self::eval_curve(t, curve, power)
    }
}

// ============================================================
// Modifier System — domain warps + surface alterations
// ============================================================

#[derive(Clone, Copy, Debug, PartialEq, Default, Serialize, Deserialize)]
pub enum ModifierType {
    #[default]
    None,
    Twist,      // domain warp: rotate around axis proportional to position along axis
    Taper,      // domain warp: scale non-axis components by linear function of axis position
    Smooth,     // surface alter: offset distance by smooth amount (erosion/dilation)
    Displace,   // surface alter: procedural 3D noise displacement (strength=amplitude, param2=frequency)
    Mirror,     // domain fold: axis bits in `axis` (1=X,2=Y,4=Z), center from mask.center
    Shear,      // domain warp: skew along axis (strength=amount, axis=affected, param2=drive axis 0=Y,1=X,2=Z)
    Sculpt,     // surface alter: sculpt brush stamp (strength=intensity, param2=SculptOp, mask=brush region)
    // NOTE: bincode tags variants by position — only ever APPEND below this line (see CLAUDE.md).
    Spherize,   // surface alter: morph result toward a sphere (strength=morph 0..1, param2=radius, <=0=auto)
    Shell,      // surface alter: onion / hollow shell — abs(d)-thickness (strength=wall half-thickness)
    Emboss,     // surface alter: raised relief — d -= strength * mask_w (mask shape = stamp primitive)
    Deboss,     // surface alter: recessed relief — d += strength * mask_w (mask shape = stamp primitive)
    /// Linear array: `strength` = count (≥2), `mask.center` = per-copy offset (prim-local / body-local).
    /// Eval = min of N translated copies. GPU bake expands to N serialized prims.
    Array,
}

impl ModifierType {
    pub fn label(&self) -> &'static str {
        match self { Self::None => "None", Self::Twist => "Twist", Self::Taper => "Taper",
                     Self::Smooth => "Smooth", Self::Displace => "Displace",
                     Self::Mirror => "Mirror", Self::Shear => "Shear", Self::Sculpt => "Sculpt",
                     Self::Spherize => "Spherize", Self::Shell => "Shell",
                     Self::Emboss => "Emboss", Self::Deboss => "Deboss",
                     Self::Array => "Array" }
    }
    pub fn is_domain_warp(&self) -> bool { matches!(self, Self::Twist | Self::Taper | Self::Mirror | Self::Shear) }
    pub fn is_surface_alter(&self) -> bool { matches!(self, Self::Smooth | Self::Displace | Self::Sculpt | Self::Spherize | Self::Shell | Self::Emboss | Self::Deboss) }
    /// Multi-copy placement (not a domain warp or surface alter).
    pub fn is_array(&self) -> bool { matches!(self, Self::Array) }
}

/// Soft-boolean join type. **Append-only** (bincode + GPU u32 tag).
///
/// | Tag | Name       | Math                         | Curve editor |
/// |-----|------------|------------------------------|--------------|
/// | 0   | Polynomial | classic poly smin (Smooth)   | no           |
/// | 1   | Chamfer    | exact 45° chamfer_min        | no           |
/// | 2   | Circular   | circular fillet (legacy)     | no           |
/// | 3   | Curve      | poly smin + user transfer    | yes          |
///
/// Owner 2026-07-27: Smooth + Chamfer are fixed presets (no curve). Curve is the
/// only mode that samples the multipoint profile. Circular stays for old saves.
#[derive(Clone, Copy, Debug, PartialEq, Default, Serialize, Deserialize)]
pub enum BlendProfile {
    #[default]
    #[serde(alias = "Cubic")]
    #[serde(alias = "Exponential")]
    Polynomial, // soft round — UI "Smooth" (exact smin, no curve)
    Chamfer,    // 45° flat bevel (exact, no curve)
    #[serde(alias = "Round")]
    Circular, // circular fillet (legacy; hidden in UI)
    /// User-editable blend transfer curve (LUT on poly-smin mix weight).
    Curve,
}


/// Convert a tangent-space NORMAL map into a HEIGHT (bump / displacement) map.
///
/// **Why this exists** (owner 2026-07-29): the path tracer samples albedo / roughness / metallic
/// but never perturbs the shading normal, so a normal map that reads correctly in the Hybrid
/// viewport contributes *nothing* in PT. Height maps go through geometry (displacement) or the
/// G-buffer bump path instead, so converting lets one authored map work in both renderers.
///
/// **Method — Poisson-style integration by relaxation.** A tangent normal encodes the surface
/// SLOPE: `(dh/dx, dh/dy) = (-n.x/n.z, -n.y/n.z)`. Recovering `h` means integrating that gradient
/// field, which has no closed form on a general image — naive row-then-column integration
/// accumulates drift and produces visible banding along whichever axis is summed first.
///
/// Iterative relaxation avoids that: each pass sets every pixel to the average of what its four
/// neighbours *predict* it should be given the local slopes, so error spreads outward instead of
/// accumulating along one direction. It converges toward the least-squares height field, which is
/// the standard result for this problem (the same idea as a Poisson solve, done in-place).
///
/// `input` is RGBA8 with the usual tangent-normal encoding (`rgb = n * 0.5 + 0.5`). Returns a
/// single-channel height in 0..1, normalised to use the full range — an un-normalised result is
/// usually so flat it looks like nothing happened.
///
/// `iterations` trades accuracy for time; ~64 is enough for typical detail maps, and cost is
/// `O(w * h * iterations)`.
pub fn normal_map_to_height(input: &[u8], w: usize, h: usize, iterations: u32) -> Vec<f32> {
    if w == 0 || h == 0 || input.len() < w * h * 4 {
        return Vec::new();
    }
    // Slopes from the encoded normal. z is clamped away from 0: a normal lying exactly in the
    // tangent plane implies infinite slope, which would blow up the relaxation.
    let mut dx = vec![0.0f32; w * h];
    let mut dy = vec![0.0f32; w * h];
    for i in 0..w * h {
        let nx = input[i * 4] as f32 / 127.5 - 1.0;
        let ny = input[i * 4 + 1] as f32 / 127.5 - 1.0;
        let nz = (input[i * 4 + 2] as f32 / 127.5 - 1.0).max(0.05);
        dx[i] = -nx / nz;
        dy[i] = -ny / nz;
    }

    let mut height = vec![0.0f32; w * h];
    let mut next = vec![0.0f32; w * h];
    let idx = |x: usize, y: usize| y * w + x;
    for _ in 0..iterations {
        for y in 0..h {
            for x in 0..w {
                // Wrap at the edges: these maps tile, and clamping would pin the border to a
                // constant and drag the whole field toward it.
                let xm = (x + w - 1) % w;
                let xp = (x + 1) % w;
                let ym = (y + h - 1) % h;
                let yp = (y + 1) % h;
                // Each neighbour predicts this pixel's height from its own height plus the slope
                // between them; average the four predictions.
                let l = height[idx(xm, y)] + dx[idx(xm, y)];
                let r = height[idx(xp, y)] - dx[idx(x, y)];
                let u = height[idx(x, ym)] + dy[idx(x, ym)];
                let d = height[idx(x, yp)] - dy[idx(x, y)];
                next[idx(x, y)] = (l + r + u + d) * 0.25;
            }
        }
        std::mem::swap(&mut height, &mut next);
    }

    // Normalise to 0..1. A flat input yields a flat output rather than a divide-by-zero.
    let (mut lo, mut hi) = (f32::INFINITY, f32::NEG_INFINITY);
    for v in &height {
        lo = lo.min(*v);
        hi = hi.max(*v);
    }
    let span = hi - lo;
    if span < 1e-6 {
        return vec![0.5; w * h];
    }
    height.iter().map(|v| (v - lo) / span).collect()
}

/// One knot on the blend-profile curve (Photoshop-style: sharp corner vs soft smooth).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct BlendCurveKnot {
    pub x: f32,
    pub y: f32,
    /// `true` = soft (smooth cubic through the knot), `false` = sharp (corner / linear).
    #[serde(default = "default_knot_soft")]
    pub soft: bool,
}

fn default_knot_soft() -> bool {
    true
}

impl BlendCurveKnot {
    pub fn new(x: f32, y: f32, soft: bool) -> Self {
        Self { x, y, soft }
    }
    pub fn soft_xy(x: f32, y: f32) -> Self {
        Self::new(x, y, true)
    }
    pub fn sharp_xy(x: f32, y: f32) -> Self {
        Self::new(x, y, false)
    }
}

/// Untagged helper: legacy saves store `[x,y]`; new format stores `{x,y,soft}`.
#[derive(Deserialize)]
#[serde(untagged)]
enum BlendKnotDe {
    /// Legacy plain pair
    Pair([f32; 2]),
    /// Current knot with soft flag
    Full(BlendCurveKnot),
}

impl From<BlendKnotDe> for BlendCurveKnot {
    fn from(v: BlendKnotDe) -> Self {
        match v {
            BlendKnotDe::Pair([x, y]) => BlendCurveKnot::soft_xy(x, y),
            BlendKnotDe::Full(k) => k,
        }
    }
}

/// Deserialize the knot list, tolerating the legacy `[x,y]` pair form.
///
/// **The bincode constraint (do not remove the `is_human_readable` branch).** The legacy form is
/// read via `#[serde(untagged)]`, and untagged is implemented by buffering into a self-describing
/// value — i.e. it calls `Deserializer::deserialize_any`. **Bincode does not implement
/// `deserialize_any` at all** (there are no type tags in the stream to recover), so it returns
/// `"Bincode does not support the serde::Deserializer::deserialize_any method"` and the WHOLE
/// `.fl4` load fails. `BlendCurve` is a field on `SdfPrimitive`, so that meant every project file
/// containing any SDF primitive serialized fine and then refused to load.
///
/// `is_human_readable()` is the distinction serde provides for exactly this: true for JSON (the
/// blend-curve presets in `~/.azra/blend_curve_presets/*.json`, which is where the legacy pair
/// form actually lives), false for bincode. So JSON keeps the untagged migration and bincode reads
/// the struct form directly — which is the only form bincode ever wrote, since the pair form
/// predates `BlendCurve` being serialized to `.fl4` at all.
fn deserialize_blend_knots<'de, D>(deserializer: D) -> Result<Vec<BlendCurveKnot>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::Deserialize as _;
    if deserializer.is_human_readable() {
        let raw = Vec::<BlendKnotDe>::deserialize(deserializer)?;
        Ok(raw.into_iter().map(BlendCurveKnot::from).collect())
    } else {
        // Binary formats: no untagged, no `deserialize_any`. Read the struct form directly.
        Vec::<BlendCurveKnot>::deserialize(deserializer)
    }
}

/// Editable blend-profile transfer curve: knots in 0..1, sorted by x.
/// Soft knots use cubic Hermite; sharp knots are corners (like Photoshop curves).
/// GPU receives a 4-sample downsample via [`BlendCurve::to_lut4`].
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BlendCurve {
    /// Knots sorted by x. Always at least 2 points (anchors at x=0 and x=1).
    #[serde(deserialize_with = "deserialize_blend_knots")]
    pub points: Vec<BlendCurveKnot>,
}

impl Default for BlendCurve {
    fn default() -> Self {
        Self::preset(BlendProfile::Polynomial)
    }
}

impl BlendCurve {
    pub const MAX_POINTS: usize = 12;

    /// Default knots for a profile type (Reset Curve / when switching to Curve).
    /// Smooth/Chamfer/Circular presets seed the Curve editor only — they do not
    /// affect Smooth/Chamfer runtime math (those use exact smin/chamfer_min).
    pub fn preset(profile: BlendProfile) -> Self {
        let points = match profile {
            // Identity linear = pure poly-smin (starting point for Curve mode).
            BlendProfile::Polynomial => vec![
                BlendCurveKnot::soft_xy(0.0, 0.0),
                BlendCurveKnot::soft_xy(1.0, 1.0),
            ],
            // Chamfer-like: sharp mid = flatter cross-section feel when used as Curve seed
            BlendProfile::Chamfer => vec![
                BlendCurveKnot::sharp_xy(0.0, 0.0),
                BlendCurveKnot::sharp_xy(0.5, 0.5),
                BlendCurveKnot::sharp_xy(1.0, 1.0),
            ],
            // Soft S-curve (legacy "round" feel) — good Curve starting profile
            BlendProfile::Circular | BlendProfile::Curve => vec![
                BlendCurveKnot::soft_xy(0.0, 0.0),
                BlendCurveKnot::soft_xy(0.25, 0.45),
                BlendCurveKnot::soft_xy(0.5, 0.75),
                BlendCurveKnot::soft_xy(0.75, 0.92),
                BlendCurveKnot::soft_xy(1.0, 1.0),
            ],
        };
        Self { points }
    }

    pub fn normalize(&mut self) {
        if self.points.len() < 2 {
            *self = Self::default();
            return;
        }
        for p in &mut self.points {
            p.x = p.x.clamp(0.0, 1.0);
            p.y = p.y.clamp(0.0, 1.0);
        }
        self.points
            .sort_by(|a, b| a.x.partial_cmp(&b.x).unwrap_or(std::cmp::Ordering::Equal));
        // Force end anchors
        self.points[0].x = 0.0;
        let n = self.points.len();
        self.points[n - 1].x = 1.0;
        // Dedup x
        let mut i = 1;
        while i < self.points.len() {
            if (self.points[i].x - self.points[i - 1].x).abs() < 1e-4 {
                self.points.remove(i);
            } else {
                i += 1;
            }
        }
        if self.points.len() < 2 {
            *self = Self::default();
        }
    }

    /// Tangent (dy/dx) at knot `i`. Sharp → 0 (corner). Soft → central difference.
    fn tangent_at(&self, i: usize) -> f32 {
        let pts = &self.points;
        let n = pts.len();
        if n < 2 || !pts[i].soft {
            return 0.0;
        }
        if i == 0 {
            let dx = pts[1].x - pts[0].x;
            if dx.abs() < 1e-6 {
                return 0.0;
            }
            return (pts[1].y - pts[0].y) / dx;
        }
        if i + 1 >= n {
            let dx = pts[n - 1].x - pts[n - 2].x;
            if dx.abs() < 1e-6 {
                return 0.0;
            }
            return (pts[n - 1].y - pts[n - 2].y) / dx;
        }
        let dx = pts[i + 1].x - pts[i - 1].x;
        if dx.abs() < 1e-6 {
            return 0.0;
        }
        (pts[i + 1].y - pts[i - 1].y) / dx
    }

    /// Sample y at t∈[0,1]. Soft knots: cubic Hermite; sharp: linear corners.
    pub fn eval(&self, t: f32) -> f32 {
        let t = t.clamp(0.0, 1.0);
        let pts = &self.points;
        if pts.is_empty() {
            return t;
        }
        if pts.len() == 1 {
            return pts[0].y;
        }
        if t <= pts[0].x {
            return pts[0].y;
        }
        if t >= pts[pts.len() - 1].x {
            return pts[pts.len() - 1].y;
        }
        for i in 0..pts.len() - 1 {
            let a = pts[i];
            let b = pts[i + 1];
            if t > b.x {
                continue;
            }
            let dx = b.x - a.x;
            if dx.abs() < 1e-6 {
                return a.y;
            }
            let u = ((t - a.x) / dx).clamp(0.0, 1.0);
            // Both sharp → pure linear (Photoshop corner segment)
            if !a.soft && !b.soft {
                return a.y + (b.y - a.y) * u;
            }
            // Cubic Hermite; sharp end has zero tangent → flat approach into corner
            let m0 = self.tangent_at(i) * dx;
            let m1 = self.tangent_at(i + 1) * dx;
            let u2 = u * u;
            let u3 = u2 * u;
            let h00 = 2.0 * u3 - 3.0 * u2 + 1.0;
            let h10 = u3 - 2.0 * u2 + u;
            let h01 = -2.0 * u3 + 3.0 * u2;
            let h11 = u3 - u2;
            return (h00 * a.y + h10 * m0 + h01 * b.y + h11 * m1).clamp(0.0, 1.0);
        }
        pts.last().map(|p| p.y).unwrap_or(t)
    }

    /// Downsample to 4 y-values at x = 0, 1/3, 2/3, 1 for GPU.
    pub fn to_lut4(&self) -> [f32; 4] {
        [
            self.eval(0.0),
            self.eval(1.0 / 3.0),
            self.eval(2.0 / 3.0),
            self.eval(1.0),
        ]
    }

    /// Add a soft interior point (default soft like Photoshop).
    pub fn add_point(&mut self, x: f32, y: f32) {
        self.add_point_soft(x, y, true);
    }

    pub fn add_point_soft(&mut self, x: f32, y: f32, soft: bool) {
        if self.points.len() >= Self::MAX_POINTS {
            return;
        }
        self.points.push(BlendCurveKnot::new(
            x.clamp(0.02, 0.98),
            y.clamp(0.0, 1.0),
            soft,
        ));
        self.normalize();
    }

    pub fn remove_point(&mut self, index: usize) {
        if self.points.len() <= 2 {
            return;
        }
        // Keep end anchors
        if index == 0 || index + 1 >= self.points.len() {
            return;
        }
        self.points.remove(index);
    }

    pub fn toggle_soft(&mut self, index: usize) {
        if index < self.points.len() {
            self.points[index].soft = !self.points[index].soft;
        }
    }

    pub fn set_soft(&mut self, index: usize, soft: bool) {
        if index < self.points.len() {
            self.points[index].soft = soft;
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Default, Serialize, Deserialize)]
pub enum MaskShape {
    #[default]
    None,       // no mask → weight = 1.0 everywhere
    Sphere,     // spherical falloff from center
    Box,        // box-shaped falloff
    Gradient,   // linear gradient along direction
    Noise3D,    // 3D noise volume
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct VolumeMask {
    pub shape: MaskShape,
    pub center: Vec3,       // local-space center (relative to primitive)
    pub extents: Vec3,      // sphere: .x=radius; box: half_extents; gradient: direction*length
    pub feather: f32,       // edge softness 0.0–1.0
    pub invert: bool,       // flip mask (1.0 - weight)
    // 3D Noise settings (used when shape == Noise3D)
    #[serde(default = "default_noise_freq")]
    pub noise_frequency: f32,   // spatial frequency (default 2.0)
    #[serde(default = "default_noise_octaves")]
    pub noise_octaves: u32,     // fBm octaves 1-8 (default 4)
    #[serde(default = "default_noise_lacunarity")]
    pub noise_lacunarity: f32,  // frequency multiplier per octave (default 2.0)
    #[serde(default = "default_noise_gain")]
    pub noise_gain: f32,        // amplitude multiplier per octave (default 0.5)
    #[serde(default = "default_noise_threshold")]
    pub noise_threshold: f32,   // 0-1: below=0, above=1 (with feather as transition width)
    #[serde(default)]
    pub noise_offset: Vec3,     // 3D offset for animation / variation
}

fn default_noise_freq() -> f32 { 2.0 }
fn default_noise_octaves() -> u32 { 4 }
fn default_noise_lacunarity() -> f32 { 2.0 }
fn default_noise_gain() -> f32 { 0.5 }
fn default_noise_threshold() -> f32 { 0.5 }

impl Default for VolumeMask {
    fn default() -> Self {
        Self { shape: MaskShape::None, center: Vec3::ZERO,
               extents: Vec3::splat(1.0), feather: 0.3, invert: false,
               noise_frequency: 2.0, noise_octaves: 4, noise_lacunarity: 2.0,
               noise_gain: 0.5, noise_threshold: 0.5, noise_offset: Vec3::ZERO }
    }
}

impl VolumeMask {
    pub fn evaluate(&self, local_p: Vec3) -> f32 {
        match self.shape {
            MaskShape::None => 1.0,
            MaskShape::Sphere => {
                let d = (local_p - self.center).length();
                let r = self.extents.x.max(0.001);
                let w = smoothstep_f32(r, r * (1.0 - self.feather.max(0.001)), d);
                if self.invert { 1.0 - w } else { w }
            }
            MaskShape::Box => {
                let diff = local_p - self.center;
                let ext = self.extents.max(Vec3::splat(0.001));
                let q = Vec3::new((diff.x / ext.x).abs(), (diff.y / ext.y).abs(), (diff.z / ext.z).abs());
                let d = q.x.max(q.y).max(q.z);  // 0..1 inside, >1 outside
                let w = smoothstep_f32(1.0, 1.0 - self.feather.max(0.001), d);
                if self.invert { 1.0 - w } else { w }
            }
            MaskShape::Gradient => {
                let dir = self.extents.normalize_or_zero();
                let len = self.extents.length();
                if len < 0.001 { return 1.0; }
                let t = ((local_p - self.center).dot(dir) / len).clamp(0.0, 1.0);
                let w = t * t * (3.0 - 2.0 * t); // smoothstep
                if self.invert { 1.0 - w } else { w }
            }
            MaskShape::Noise3D => {
                let p = (local_p - self.center + self.noise_offset) * self.noise_frequency;
                let n = fbm_noise_configurable(p, self.noise_octaves, self.noise_lacunarity, self.noise_gain);
                // n is in ~[-1,1], remap to [0,1]
                let n01 = (n * 0.5 + 0.5).clamp(0.0, 1.0);
                // Apply threshold with feather as transition width
                let half = self.feather.max(0.001) * 0.5;
                let lo = (self.noise_threshold - half).max(0.0);
                let hi = (self.noise_threshold + half).min(1.0);
                let w = smoothstep_f32(lo, hi, n01);
                if self.invert { 1.0 - w } else { w }
            }
        }
    }
}

pub fn smoothstep_f32(edge0: f32, edge1: f32, x: f32) -> f32 {
    if (edge1 - edge0).abs() < 1e-6 { return if x < edge0 { 0.0 } else { 1.0 }; }
    let t = ((x - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Grayscale height field for image-driven Displace.
/// CPU: bilinear sample in `world_distance`. GPU bake: packed via
/// `serialize_height_maps_for_bake` (hot[92] + binding 8).
///
/// T3: keep the full image here; packers may call [`Self::downsampled_to_max_side`]
/// so draft bakes upload a coarse mip and settle uses full resolution.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DisplaceHeightMap {
    pub w: u32,
    pub h: u32,
    /// Row-major grayscale 0..=255.
    pub pixels: Vec<u8>,
    /// Source path for re-load / display (optional).
    #[serde(default)]
    pub path: String,
}

/// Max long-side for height maps packed during draft bake (T3 progressive refine).
/// Full settle packs the stored map (loader caps at `LOAD_HEIGHT_MAP_MAX_SIDE`).
pub const DRAFT_HEIGHT_MIP_MAX_SIDE: u32 = 64;
/// Loader long-side cap — larger than pre-T3 512 so one big EXR can refine on settle.
pub const LOAD_HEIGHT_MAP_MAX_SIDE: u32 = 2048;

impl DisplaceHeightMap {
    /// Bilinear sample in [0,1] UV (wraps). Returns height in [0,1].
    pub fn sample(&self, u: f32, v: f32) -> f32 {
        if self.w == 0 || self.h == 0 || self.pixels.is_empty() {
            return 0.5;
        }
        let w = self.w as f32;
        let h = self.h as f32;
        let mut uf = u.rem_euclid(1.0) * w;
        let mut vf = v.rem_euclid(1.0) * h;
        // Sample at pixel centers
        uf = (uf - 0.5).rem_euclid(w);
        vf = (vf - 0.5).rem_euclid(h);
        let x0 = uf.floor() as u32 % self.w;
        let y0 = vf.floor() as u32 % self.h;
        let x1 = (x0 + 1) % self.w;
        let y1 = (y0 + 1) % self.h;
        let fx = uf.fract();
        let fy = vf.fract();
        let p = |x: u32, y: u32| -> f32 {
            let i = (y * self.w + x) as usize;
            self.pixels.get(i).copied().unwrap_or(128) as f32 / 255.0
        };
        let a = p(x0, y0) * (1.0 - fx) + p(x1, y0) * fx;
        let b = p(x0, y1) * (1.0 - fx) + p(x1, y1) * fx;
        a * (1.0 - fy) + b * fy
    }

    /// Project local point to UV using modifier axis (0=Y→XZ, 1=X→YZ, 2=Z→XY) and frequency.
    pub fn uv_from_local(sp: Vec3, axis: u8, freq: f32) -> (f32, f32) {
        let f = freq.max(0.1);
        match axis {
            1 => (sp.y * f * 0.5 + 0.5, sp.z * f * 0.5 + 0.5), // X axis → YZ
            2 => (sp.x * f * 0.5 + 0.5, sp.y * f * 0.5 + 0.5), // Z axis → XY
            _ => (sp.x * f * 0.5 + 0.5, sp.z * f * 0.5 + 0.5), // Y axis → XZ
        }
    }

    /// Return a box-filtered copy with both dimensions ≤ `max_side` (T3 draft mip).
    /// `max_side == 0` or already small enough → clone of self.
    pub fn downsampled_to_max_side(&self, max_side: u32) -> Self {
        if max_side == 0 || self.w == 0 || self.h == 0 || self.pixels.is_empty() {
            return self.clone();
        }
        if self.w <= max_side && self.h <= max_side {
            return self.clone();
        }
        let mut w = self.w;
        let mut h = self.h;
        let mut pixels = self.pixels.clone();
        // Repeated 2× box until under cap (cheap, stable for power-of-two and odd sizes).
        while w > max_side || h > max_side {
            let nw = (w + 1) / 2;
            let nh = (h + 1) / 2;
            let mut next = vec![0u8; (nw * nh) as usize];
            for y in 0..nh {
                for x in 0..nw {
                    let x0 = x * 2;
                    let y0 = y * 2;
                    let x1 = (x0 + 1).min(w - 1);
                    let y1 = (y0 + 1).min(h - 1);
                    let i00 = (y0 * w + x0) as usize;
                    let i10 = (y0 * w + x1) as usize;
                    let i01 = (y1 * w + x0) as usize;
                    let i11 = (y1 * w + x1) as usize;
                    let sum = pixels.get(i00).copied().unwrap_or(0) as u32
                        + pixels.get(i10).copied().unwrap_or(0) as u32
                        + pixels.get(i01).copied().unwrap_or(0) as u32
                        + pixels.get(i11).copied().unwrap_or(0) as u32;
                    next[(y * nw + x) as usize] = (sum / 4) as u8;
                }
            }
            w = nw;
            h = nh;
            pixels = next;
            if w <= 1 && h <= 1 {
                break;
            }
        }
        Self {
            w,
            h,
            pixels,
            path: self.path.clone(),
        }
    }
}

#[cfg(test)]
mod height_map_tests {
    use super::*;

    #[test]
    fn downsampled_to_max_side_halves_until_cap() {
        let mut pixels = vec![0u8; 16 * 16];
        pixels[0] = 255;
        pixels[15] = 128;
        let hm = DisplaceHeightMap {
            w: 16,
            h: 16,
            pixels,
            path: String::new(),
        };
        let d = hm.downsampled_to_max_side(4);
        assert!(d.w <= 4 && d.h <= 4, "got {}×{}", d.w, d.h);
        assert_eq!(d.pixels.len(), (d.w * d.h) as usize);
        // Full when already under cap
        let same = d.downsampled_to_max_side(64);
        assert_eq!(same.w, d.w);
        assert_eq!(same.h, d.h);
        // max_side 0 = no change
        let full = hm.downsampled_to_max_side(0);
        assert_eq!(full.w, 16);
        assert_eq!(full.h, 16);
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Modifier {
    pub mod_type: ModifierType,
    pub strength: f32,      // twist: radians per unit, taper: scale factor, smooth: offset, Array: count
    pub axis: u8,           // 0=Y, 1=X, 2=Z  (Array circle: rotation axis)
    pub param2: f32,        // taper: end_scale; Array: <0.5 linear, ≥0.5 circular; Displace: frequency / UV scale
    pub mask: VolumeMask,   // Array linear: per-copy step; Array circle: first-copy offset from center
    /// Image height for Displace (None = procedural FBM noise).
    #[serde(default)]
    pub height_map: Option<DisplaceHeightMap>,
}

impl Default for Modifier {
    fn default() -> Self {
        Self {
            mod_type: ModifierType::None,
            strength: 0.0,
            axis: 0,
            param2: 0.5,
            mask: VolumeMask::default(),
            height_map: None,
        }
    }
}

/// Array placement mode encoded in `Modifier::param2` / fields (no new bincode variant).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ArrayMode {
    /// Copy i at `position + i * offset` (i = 0..count).
    Linear { count: u32, offset: Vec3 },
    /// Copy i on a circle around `position` (empty center):  
    /// `position + Rot(axis, 2π·i/count) * radius_vec` (i = 0..count).
    Circular { count: u32, radius_vec: Vec3, axis: u8 },
}

impl Modifier {
    pub fn has_image_displace(&self) -> bool {
        self.mod_type == ModifierType::Displace && self.height_map.is_some()
    }

    pub fn is_active(&self) -> bool { self.mod_type != ModifierType::None }

    /// Array is circular when param2 ≥ 0.5.
    pub fn array_is_circular(&self) -> bool {
        self.mod_type == ModifierType::Array && self.param2 >= 0.5
    }

    /// Full array placement (linear or circular). None if not an active Array.
    pub fn array_mode(&self) -> Option<ArrayMode> {
        if self.mod_type != ModifierType::Array || !self.is_active() {
            return None;
        }
        let count = self.strength.round().max(2.0) as u32;
        if self.param2 >= 0.5 {
            Some(ArrayMode::Circular {
                count,
                radius_vec: self.mask.center,
                axis: self.axis.min(2),
            })
        } else {
            Some(ArrayMode::Linear {
                count,
                offset: self.mask.center,
            })
        }
    }

    /// Legacy: linear (count, offset). Prefer `array_mode()`.
    pub fn array_params(&self) -> Option<(u32, Vec3)> {
        match self.array_mode()? {
            ArrayMode::Linear { count, offset } => Some((count, offset)),
            ArrayMode::Circular { count, radius_vec, .. } => Some((count, radius_vec)),
        }
    }

    /// Build a linear Array modifier.
    pub fn array_linear(count: u32, offset: Vec3) -> Self {
        let mut m = Self::default();
        m.mod_type = ModifierType::Array;
        m.strength = count.max(2) as f32;
        m.param2 = 0.0; // linear
        m.mask.center = offset;
        m
    }

    /// Build a circular Array modifier (empty center; axis 0=Y, 1=X, 2=Z).
    pub fn array_circular(count: u32, radius_vec: Vec3, axis: u8) -> Self {
        let mut m = Self::default();
        m.mod_type = ModifierType::Array;
        m.strength = count.max(2) as f32;
        m.param2 = 1.0; // circular
        m.axis = axis.min(2);
        m.mask.center = radius_vec;
        m
    }
}

/// Rotate `v` around world axis index (0=Y, 1=X, 2=Z) by `angle` radians.
pub fn rotate_around_axis(v: Vec3, axis: u8, angle: f32) -> Vec3 {
    let (s, c) = angle.sin_cos();
    match axis {
        1 => {
            // X
            Vec3::new(v.x, c * v.y - s * v.z, s * v.y + c * v.z)
        }
        2 => {
            // Z
            Vec3::new(c * v.x - s * v.y, s * v.x + c * v.y, v.z)
        }
        _ => {
            // Y (default)
            Vec3::new(c * v.x + s * v.z, v.y, -s * v.x + c * v.z)
        }
    }
}

// ============================================================
// Domain warp helpers (pub(crate) — used by SdfPrimitive::world_distance)
// ============================================================

pub(crate) fn apply_twist(p: Vec3, angle_per_unit: f32, axis: u8) -> Vec3 {
    match axis {
        1 => { // X axis
            let a = p.x * angle_per_unit;
            let (s, c) = a.sin_cos();
            Vec3::new(p.x, c * p.y - s * p.z, s * p.y + c * p.z)
        }
        2 => { // Z axis
            let a = p.z * angle_per_unit;
            let (s, c) = a.sin_cos();
            Vec3::new(c * p.x - s * p.y, s * p.x + c * p.y, p.z)
        }
        _ => { // Y axis (default)
            let a = p.y * angle_per_unit;
            let (s, c) = a.sin_cos();
            Vec3::new(c * p.x - s * p.z, p.y, s * p.x + c * p.z)
        }
    }
}

pub(crate) fn apply_taper(p: Vec3, strength: f32, axis: u8, end_scale: f32) -> Vec3 {
    let axis_val = match axis { 1 => p.x, 2 => p.z, _ => p.y };
    let t = axis_val * strength;
    let s = (1.0 + t * (end_scale - 1.0)).max(0.01);
    match axis {
        1 => Vec3::new(p.x, p.y / s, p.z / s),
        2 => Vec3::new(p.x / s, p.y / s, p.z),
        _ => Vec3::new(p.x / s, p.y, p.z / s),
    }
}

/// Shear: skew coordinates along one axis driven by another.
/// `axis` = affected axis, `param2` encodes drive axis (0=Y,1=X,2=Z).
pub(crate) fn apply_shear(p: Vec3, strength: f32, axis: u8, drive_axis: f32) -> Vec3 {
    let drive = drive_axis as u8;
    let drive_val = match drive { 1 => p.x, 2 => p.z, _ => p.y };
    let offset = drive_val * strength;
    match axis {
        1 => Vec3::new(p.x + offset, p.y, p.z),  // shear X by drive axis
        2 => Vec3::new(p.x, p.y, p.z + offset),  // shear Z by drive axis
        _ => Vec3::new(p.x, p.y + offset, p.z),  // shear Y by drive axis
    }
}

/// Sculpt operations for the Sculpt modifier type.
/// Stored as f32 in `param2`: 0=Push, 1=Pull, 2=Flatten, 3=Inflate, 4=Pinch, 5=Crease.
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub enum SculptOp {
    #[default]
    Push,       // Grow surface outward (add material)
    Pull,       // Shrink surface inward (remove material)
    Flatten,    // Level surface toward mask center plane
    Inflate,    // Expand uniformly along all directions
    Pinch,      // Contract toward the modifier axis line
    Crease,     // Create a groove along the axis direction
}

impl SculptOp {
    pub fn from_f32(v: f32) -> Self {
        match v as u32 { 0 => Self::Push, 1 => Self::Pull, 2 => Self::Flatten,
                         3 => Self::Inflate, 4 => Self::Pinch, 5 => Self::Crease, _ => Self::Push }
    }
    pub fn to_f32(self) -> f32 {
        match self { Self::Push => 0.0, Self::Pull => 1.0, Self::Flatten => 2.0,
                     Self::Inflate => 3.0, Self::Pinch => 4.0, Self::Crease => 5.0 }
    }
    pub fn label(&self) -> &'static str {
        match self { Self::Push => "Push", Self::Pull => "Pull", Self::Flatten => "Flatten",
                     Self::Inflate => "Inflate", Self::Pinch => "Pinch", Self::Crease => "Crease" }
    }
    pub const ALL: [SculptOp; 6] = [Self::Push, Self::Pull, Self::Flatten, Self::Inflate, Self::Pinch, Self::Crease];
    pub fn inverted(self) -> Self {
        match self { Self::Push => Self::Pull, Self::Pull => Self::Push, other => other }
    }
}

/// Apply a sculpt modifier to a signed distance value.
/// `p` is the local-space evaluation point, `d` is the current SDF distance,
/// `strength` is the sculpt intensity, `op` is the sculpt operation type,
/// `mask_w` is the volume mask weight (0..1), `axis` selects the sculpt direction.
pub(crate) fn apply_sculpt(p: Vec3, d: f32, strength: f32, op: SculptOp, mask_w: f32, axis: u8, mask_center: Vec3) -> f32 {
    if mask_w < 1e-6 { return d; }
    let s = strength * mask_w;
    match op {
        SculptOp::Push => d - s,            // grow outward: reduce distance
        SculptOp::Pull => d + s,            // shrink inward: increase distance
        SculptOp::Flatten => {
            // Flatten toward a plane perpendicular to axis at mask_center
            let axis_val = match axis { 1 => p.x - mask_center.x, 2 => p.z - mask_center.z, _ => p.y - mask_center.y };
            let plane_d = axis_val;     // signed distance to the plane
            // Lerp d toward plane_d (flatten effect)
            d + (plane_d.abs().min(strength) * -plane_d.signum()) * mask_w * 0.5
        }
        SculptOp::Inflate => {
            // Inflate: push outward proportional to proximity to surface
            let proximity = (-d.abs() * 4.0).exp(); // stronger near surface
            d - s * proximity
        }
        SculptOp::Pinch => {
            // Pinch: contract toward the axis line through mask_center
            let (dx, dz) = match axis {
                1 => (p.y - mask_center.y, p.z - mask_center.z),  // pinch toward X axis
                2 => (p.x - mask_center.x, p.y - mask_center.y),  // pinch toward Z axis
                _ => (p.x - mask_center.x, p.z - mask_center.z),  // pinch toward Y axis
            };
            let lateral_dist = (dx * dx + dz * dz).sqrt();
            // Pull toward axis: reduce distance when far from axis
            d + lateral_dist * s * 0.3
        }
        SculptOp::Crease => {
            // Crease: create a groove along the axis direction
            let (dx, dz) = match axis {
                1 => (p.y - mask_center.y, p.z - mask_center.z),
                2 => (p.x - mask_center.x, p.y - mask_center.y),
                _ => (p.x - mask_center.x, p.z - mask_center.z),
            };
            let lateral = (dx * dx + dz * dz).sqrt();
            // V-shaped groove: add distance proportional to how close to axis
            let groove = (s.abs() - lateral).max(0.0);
            d + groove * mask_w * strength.signum() * 0.5
        }
    }
}

/// SDF of a 2D circle extruded along one axis (infinite cylinder or capped cylinder).
/// Returns positive outside the cylinder, negative inside.
/// `extrude_axis`: 0=Y (XZ plane), 1=X (YZ plane), 2=Z (XY plane).
/// `half_depth`: half-depth along axis (0.0 = infinite extrusion).
pub(crate) fn sdf_extruded_circle(p: Vec3, radius: f32, extrude_axis: u8, half_depth: f32) -> f32 {
    let (d2d, axis_dist) = match extrude_axis {
        1 => ((p.y * p.y + p.z * p.z).sqrt() - radius, p.x.abs()),
        2 => ((p.x * p.x + p.y * p.y).sqrt() - radius, p.z.abs()),
        _ => ((p.x * p.x + p.z * p.z).sqrt() - radius, p.y.abs()),
    };
    if half_depth > 0.001 {
        d2d.max(axis_dist - half_depth)
    } else {
        d2d
    }
}

// ============================================================
// Noise helpers (local to this module, used by VolumeMask::evaluate)
// ============================================================

/// 3D value noise for displacement modifier (hash-based, no texture lookup)
fn mask_noise_3d_local(p: Vec3) -> f32 {
    let i = Vec3::new(p.x.floor(), p.y.floor(), p.z.floor());
    let f = p - i;
    let u = f * f * (Vec3::splat(3.0) - f * 2.0); // smoothstep
    let hash = |x: f32, y: f32, z: f32| -> f32 {
        let n = (x * 127.1 + y * 311.7 + z * 74.7).sin() * 43758.5453;
        n - n.floor()
    };
    let a = hash(i.x, i.y, i.z);
    let b = hash(i.x + 1.0, i.y, i.z);
    let c = hash(i.x, i.y + 1.0, i.z);
    let d = hash(i.x + 1.0, i.y + 1.0, i.z);
    let e = hash(i.x, i.y, i.z + 1.0);
    let f_v = hash(i.x + 1.0, i.y, i.z + 1.0);
    let g = hash(i.x, i.y + 1.0, i.z + 1.0);
    let h = hash(i.x + 1.0, i.y + 1.0, i.z + 1.0);
    let x1 = a + (b - a) * u.x;
    let x2 = c + (d - c) * u.x;
    let x3 = e + (f_v - e) * u.x;
    let x4 = g + (h - g) * u.x;
    let y1 = x1 + (x2 - x1) * u.y;
    let y2 = x3 + (x4 - x3) * u.y;
    y1 + (y2 - y1) * u.z
}

/// Configurable fBm: custom lacunarity + gain for noise mask
fn fbm_noise_configurable(p: Vec3, octaves: u32, lacunarity: f32, gain: f32) -> f32 {
    let mut val = 0.0f32;
    let mut amp = 1.0f32;
    let mut freq = 1.0f32;
    let mut max_amp = 0.0f32;
    for _ in 0..octaves.min(8) {
        val += amp * (mask_noise_3d_local(p * freq) * 2.0 - 1.0);
        max_amp += amp;
        freq *= lacunarity;
        amp *= gain;
    }
    if max_amp > 0.001 { val / max_amp } else { 0.0 }
}


#[cfg(test)]
mod blend_curve_format_tests {

    /// A FLAT normal map (all normals straight up) must integrate to a FLAT height field.
    /// This is the case a broken integrator most often gets wrong — drift shows up as a ramp.
    #[test]
    fn normal_to_height_flat_input_is_flat() {
        let (w, h) = (32usize, 32usize);
        // Tangent normal (0,0,1) -> encoded (128,128,255).
        let input: Vec<u8> = (0..w * h).flat_map(|_| [128u8, 128, 255, 255]).collect();
        let out = super::normal_map_to_height(&input, w, h, 32);
        assert_eq!(out.len(), w * h);
        let (lo, hi) = out.iter().fold((f32::INFINITY, f32::NEG_INFINITY),
            |(l, g), v| (l.min(*v), g.max(*v)));
        assert!(hi - lo < 1e-3,
            "a flat normal map produced height range {lo}..{hi} - the integrator is drifting,              which shows up as a false ramp across the surface");
    }

    /// A SINE height field must be recovered from its own gradient, with the peak where the
    /// author put it.
    ///
    /// The obvious test — a constant +X slope — is UNSOLVABLE on a tiling domain: height would
    /// have to rise across the full width and simultaneously match itself at the wrap seam. The
    /// least-squares answer to a contradictory field is a flat surface, and that is exactly what
    /// the integrator returned (0.5 everywhere). It was right and the test was wrong.
    ///
    /// A sine is periodic, so its gradient integrates cleanly and the expected answer is known in
    /// closed form — which also lets this assert the PHASE, catching a sign flip that would turn
    /// bumps into dents.
    #[test]
    fn normal_to_height_recovers_a_sine_with_the_correct_phase() {
        let (w, h) = (64usize, 8usize);
        let enc = |v: f32| ((v + 1.0) * 127.5).round().clamp(0.0, 255.0) as u8;
        // h(x) = sin(2*pi*x/w)  =>  dh/dx = k*cos(...), and dh/dx = -nx/nz, so nx = -dh/dx * nz.
        let k = std::f32::consts::TAU / w as f32;
        let mut input = vec![0u8; w * h * 4];
        for y in 0..h {
            for x in 0..w {
                let slope = k * (k * x as f32).cos() * 8.0; // scaled so the slope is measurable
                let nz = 1.0f32;
                let n = glam::Vec3::new(-slope, 0.0, nz).normalize();
                let i = (y * w + x) * 4;
                input[i] = enc(n.x);
                input[i + 1] = enc(n.y);
                input[i + 2] = enc(n.z);
                input[i + 3] = 255;
            }
        }
        let out = super::normal_map_to_height(&input, w, h, 512);

        let row = h / 2;
        // sin peaks at x = w/4 and troughs at x = 3w/4.
        let peak = out[row * w + w / 4];
        let trough = out[row * w + 3 * w / 4];
        assert!(peak > trough + 0.5,
            "recovered sine has peak {peak} at x=w/4 and trough {trough} at x=3w/4 - they must be              clearly separated (normalised range is 0..1). Too close means the field collapsed;              inverted means the sign of dh/dx is wrong and bumps would render as dents.");
    }

    /// Degenerate inputs must not panic or produce garbage lengths — this runs on user-supplied
    /// images, so a malformed one is a matter of when, not if.
    #[test]
    fn normal_to_height_handles_degenerate_input() {
        assert!(super::normal_map_to_height(&[], 0, 0, 8).is_empty());
        assert!(super::normal_map_to_height(&[0; 4], 8, 8, 8).is_empty(), "short buffer must bail");
        // A normal lying in the tangent plane (z = 0) would be an infinite slope; must stay finite.
        let input: Vec<u8> = (0..16 * 16).flat_map(|_| [255u8, 128, 128, 255]).collect();
        let out = super::normal_map_to_height(&input, 16, 16, 16);
        assert!(out.iter().all(|v| v.is_finite()), "z~0 normals produced non-finite heights");
    }

    use super::{BlendCurve, BlendCurveKnot};

    /// `BlendCurve` MUST survive a bincode round trip.
    ///
    /// It is a field on `SdfPrimitive`, so a failure here means every `.fl4` containing any SDF
    /// primitive writes successfully and then cannot be read back — silent, total data loss that
    /// only shows up when the user reopens their project. It shipped once (an `#[serde(untagged)]`
    /// helper needs `deserialize_any`, which bincode does not implement), and the only reason it
    /// was ever noticed is that 13 unrelated save tests started failing.
    #[test]
    fn blend_curve_round_trips_through_bincode() {
        let c = BlendCurve::default();
        let bytes = bincode::serialize(&c).expect("serialize");
        let back: BlendCurve = bincode::deserialize(&bytes)
            .expect("BlendCurve must be readable by bincode - it is a field on SdfPrimitive, so \
                    a failure here makes every .fl4 with an SDF primitive unloadable");
        assert_eq!(back.points.len(), c.points.len(), "knot count changed across the round trip");
        for (a, b) in c.points.iter().zip(back.points.iter()) {
            assert_eq!(a, b, "knot changed across the round trip");
        }
    }

    /// Non-default curves too — a default that happens to round-trip proves little if the
    /// encoding is length- or value-sensitive.
    #[test]
    fn edited_blend_curve_round_trips_through_bincode() {
        let mut c = BlendCurve::default();
        c.points = vec![
            BlendCurveKnot::new(0.0, 0.0, false),
            BlendCurveKnot::new(0.25, 0.7, true),
            BlendCurveKnot::new(0.6, 0.2, false),
            BlendCurveKnot::new(1.0, 1.0, true),
        ];
        let bytes = bincode::serialize(&c).expect("serialize");
        let back: BlendCurve = bincode::deserialize(&bytes).expect("deserialize");
        assert_eq!(back.points, c.points, "edited knots must survive verbatim, including `soft`");
    }

    /// The JSON path must KEEP reading the legacy `[x,y]` pair form — that is what the untagged
    /// helper exists for, and the blend-curve presets on disk use it. Fixing bincode by deleting
    /// the migration would trade one data-loss bug for another.
    #[test]
    fn json_still_accepts_the_legacy_pair_form() {
        let legacy = r#"{"points":[[0.0,0.0],[0.5,0.8],[1.0,1.0]]}"#;
        let c: BlendCurve = serde_json::from_str(legacy)
            .expect("legacy [x,y] presets must still load - this is why the untagged helper exists");
        assert_eq!(c.points.len(), 3);
        assert_eq!(c.points[1].x, 0.5);
        assert_eq!(c.points[1].y, 0.8);
        assert!(c.points[1].soft, "a legacy pair has no `soft` flag and must default to soft");
    }

    /// And the current JSON struct form, so the two coexist.
    #[test]
    fn json_accepts_the_current_struct_form() {
        let cur = r#"{"points":[{"x":0.0,"y":0.0,"soft":false},{"x":1.0,"y":1.0,"soft":true}]}"#;
        let c: BlendCurve = serde_json::from_str(cur).expect("current form must load");
        assert_eq!(c.points.len(), 2);
        assert!(!c.points[0].soft, "explicit soft=false must be preserved, not defaulted");
    }
}
