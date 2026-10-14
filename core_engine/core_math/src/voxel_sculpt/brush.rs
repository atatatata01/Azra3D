// voxel_sculpt/brush.rs — brush types, falloff curves, tip alpha, stroke recording,
// elastic grab state. Split from voxel_sculpt.rs (pure relocation).

use glam::Vec3;

/// Brush operations. All operate on density (material amount), never on signed distance.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum VoxelBrush {
    /// Deposit material (3D-Coat "Grow"/"Clay"). Works even on empty space.
    Draw,
    /// Remove material (carve).
    Erase,
    /// Relax toward the local average (reduces noise/steps).
    Smooth,
    /// Push the surface toward the brush plane (build up / cut down to flat).
    Flatten,
    /// Deposit toward a plane set slightly PROUD of the contact point — fills cavities
    /// faster than ridges, so volume builds up EVEN instead of lumpy (the organic workhorse).
    Clay,
    /// One-directional Flatten: only SHAVE material on one side of the contact plane
    /// (hard-surface planar trim). Invert flips the cut side.
    Scrape,
    /// Narrow, high-contrast subtract along the stroke → sharp grooves / panel lines
    /// (ZBrush Dam_Standard). Invert raises a ridge instead of cutting.
    Crease,
    /// Inverse Scrape: only DEPOSITS into cavities below the contact plane (fills pits / dents).
    Fill,
    /// ZBrush Clay Buildup — hard planar clay strips (proud plane + pack-to-plane, no soft mud).
    ClayBuildup,
    /// Puff volume — thickens the surface shell along the density gradient. Invert deflates.
    Inflate,
    /// Pull material tangentially toward the stroke axis → sharpens edges/creases (advection;
    /// gathers from a pre-stamp snapshot + relaxes the region after). Invert bulges outward.
    Pinch,
    /// MOVE / GRAB: grab the surface and translate it as a COHERENT BLOCK toward the cursor (Blender
    /// Grab / ZBrush Move / 3D-Coat). Per voxel, gather the pre-grab density from `p - drag * fall`, but
    /// the LIVE grab uses a PLATEAU falloff (`grab_plateau_falloff`): the inner core of the brush moves by
    /// the FULL drag (weight 1 → the grabbed region keeps its shape and shifts TOGETHER) and only a thin
    /// rim shell smoothly ramps the displacement back to 0 at the boundary — so it reads as repositioning
    /// a feature, not a taffy stretch. Its soft-falloff sibling is `Smudge` (continuous taper); the only
    /// difference between the two live brushes is the falloff curve. Semi-Lagrangian GATHER from the
    /// frozen pristine snapshot → order-independent, deterministic, and fully reversible (drag back to
    /// the anchor restores pristine). No trailing relax (a relax would diffuse the moved material — the
    /// old smear). CPU-forced (not in the GPU kernel). NEW variants append at the END so the GPU op-code
    /// ordering (voxel_brush.wgsl) stays in lockstep.
    Move,
    /// SMUDGE / DRAG: the soft, continuous-falloff sibling of Move/Grab. It runs the EXACT same
    /// reversible path (restore-pristine + cumulative semi-Lagrangian gather, `elastic_grab_preview`)
    /// but with the smooth `falloff_weight` curve instead of Move's rigid PLATEAU — so the displacement
    /// tapers CONTINUOUSLY from the centre and the surface STRETCHES like taffy / drags detail along the
    /// cursor, rather than translating as a coherent block. This is the legacy "Move" feel, kept as its
    /// own tool. CPU-forced and driven SOLELY by the live elastic-grab path (it never enters the
    /// `StampPlan`/GPU op-code pipeline); like Move it carries no per-voxel deposit and ignores the
    /// surface normal. Appended at the END to preserve enum discriminant / GPU op-code ordering — it has
    /// no GPU op-code (CPU-only, like Move).
    Smudge,
    /// SNAKE HOOK: pull the surface out into a moving-anchor TUBE (Blender Snake Hook / ZBrush
    /// SnakeHook). Unlike Move (fixed anchor, coherent block), the anchor FOLLOWS the cursor so a
    /// growing spike is drawn out along the drag; the affected region is a CAPSULE (stroke-start →
    /// cursor). Displacement is computed over the PRE-STROKE ORIGINAL positions (elastic, like Grab):
    /// per vertex `pos = orig + grab_delta*factor`, plus an optional axis PINCH (`crease_pinch`
    /// slider, default 0.5 = OFF) that squeezes gathered verts toward the tube axis so the tip
    /// narrows into a horn. SURFACE-mode only (pure vertex displacement over the surface undo
    /// snapshot); in VOXEL mode it is greyed with a tooltip (no density version built). NO relax
    /// (would smear the tube). CPU-only, like Move/Smudge — NO GPU op-code. Appended at the END so
    /// enum discriminant / GPU op-code ordering stays in lockstep (save-format safety).
    SnakeHook,
}

impl VoxelBrush {
    /// True for the plane/normal-relative brushes whose deposit reads the stamp `normal`
    /// (Flatten/Clay/Scrape/Crease/Fill/ClayBuildup/Pinch). These are the brushes for which
    /// `BrushDirection::Surface` benefits from a PER-STAMP surface normal (sampled from the evolving
    /// density field at each interpolated stamp) instead of one frame-wide normal — so a Clay/Build
    /// stroke follows the local surface as the stroke deposits material (the "clay buildup feel").
    /// The normal-IGNORING brushes (Draw/Erase/Smooth/Inflate) and Move (drags along its cursor delta,
    /// not the normal) return false — their per-stamp normal is irrelevant, so leave it untouched.
    #[inline]
    pub fn is_direction_aware(self) -> bool {
        matches!(
            self,
            VoxelBrush::Flatten | VoxelBrush::Clay | VoxelBrush::Scrape | VoxelBrush::Crease
                | VoxelBrush::Fill | VoxelBrush::ClayBuildup | VoxelBrush::Pinch
        )
    }

    /// Map this shipped brush onto the Phase-0 `BuiltinBrush` identity used by the recipe data
    /// model (`crate::brush`). One-to-one; the discriminant order matches so the GPU op-code order
    /// stays in lockstep. This is the single converter the producer + `apply_brush` shim use.
    #[inline]
    pub fn to_builtin(self) -> crate::brush::BuiltinBrush {
        use crate::brush::BuiltinBrush as B;
        match self {
            VoxelBrush::Draw => B::Draw,
            VoxelBrush::Erase => B::Erase,
            VoxelBrush::Smooth => B::Smooth,
            VoxelBrush::Flatten => B::Flatten,
            VoxelBrush::Clay => B::Clay,
            VoxelBrush::Scrape => B::Scrape,
            VoxelBrush::Crease => B::Crease,
            VoxelBrush::Fill => B::Fill,
            VoxelBrush::ClayBuildup => B::ClayBuildup,
            VoxelBrush::Inflate => B::Inflate,
            VoxelBrush::Pinch => B::Pinch,
            VoxelBrush::Move => B::Move,
            // Smudge is the soft-falloff grab — the SAME underlying coherent gather as Move (its
            // plateau-vs-smooth distinction lives only in the live `elastic_grab_preview` curve choice),
            // so in the stateless recipe model it IS a Move. It never reaches the live recipe path (the
            // live path forks it to `elastic_grab_preview` and returns early); this mapping only keeps the
            // recipe/parity model total.
            VoxelBrush::Smudge => B::Move,
            // Snake Hook is a CPU-only surface grab (never enters the density recipe/GPU path — the
            // live path forks it to the surface tube displacement and returns early). Map to Move to
            // keep the recipe/parity model total, exactly like Smudge.
            VoxelBrush::SnakeHook => B::Move,
        }
    }
}

/// Auto Mask mode — feature-driven protect mask (ZBrush-style). Writes into the same protect mask
/// as Mask Paint so density stamps multiply falloff by `(1 - mask)`. Runs PRE-stamp from the live
/// density field so the same dab already respects the feature protection.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
#[repr(u8)]
pub enum AutoMaskMode {
    /// Off — no automatic mask writes.
    #[default]
    Off = 0,
    /// Protect high |curvature| (peaks + cavities).
    Curvature = 1,
    /// Protect convex ridges / peaks (positive density Laplacian).
    Peak = 2,
    /// Protect concave cavities (negative density Laplacian).
    Cavity = 3,
    /// Protect flat regions (low |curvature|) — sculpt edges only.
    Flat = 4,
}

impl AutoMaskMode {
    pub const ALL: [AutoMaskMode; 5] = [
        AutoMaskMode::Off,
        AutoMaskMode::Curvature,
        AutoMaskMode::Peak,
        AutoMaskMode::Cavity,
        AutoMaskMode::Flat,
    ];

    #[inline]
    pub fn as_u32(self) -> u32 { self as u32 }

    #[inline]
    pub fn from_u32(v: u32) -> Self {
        match v {
            1 => AutoMaskMode::Curvature,
            2 => AutoMaskMode::Peak,
            3 => AutoMaskMode::Cavity,
            4 => AutoMaskMode::Flat,
            _ => AutoMaskMode::Off,
        }
    }

    #[inline]
    pub fn label(self) -> &'static str {
        match self {
            AutoMaskMode::Off => "Off",
            AutoMaskMode::Curvature => "Curv",
            AutoMaskMode::Peak => "Peak",
            AutoMaskMode::Cavity => "Cavity",
            AutoMaskMode::Flat => "Flat",
        }
    }

    #[inline]
    pub fn is_on(self) -> bool { !matches!(self, AutoMaskMode::Off) }
}

/// Default brush hardness (0 = soft Blender "Smooth" falloff, 1 = sharp pointed core). 0 is the
/// production default: Blender's sculpt "Smooth" curve (smoothstep 3e^2 - 2e^3, captured in
/// docs/planning/sculpt/oracle/falloff_presets_ref.json), so a fresh tool deposits with the soft
/// shoulder + full core Blender users feel out of the box. This is the value a fresh `VoxelObject` starts with;
/// the slider lets the user dial toward a tight, sharp core. NOTE: kept at 0 so the GPU parity-test's
/// packed `box_min.w = 0` (bitcast 0u32 -> 0.0_f32) matches a fresh tool's default exactly.
pub const DEFAULT_BRUSH_HARDNESS: f32 = 0.0;

/// Blender sculpt falloff family (F1b). `e = 1 - dist/radius` (1 centre, 0 rim) — same convention as
/// `falloff_weight` and `docs/planning/sculpt/oracle/falloff_presets_ref.json`.
/// `Blend` keeps the F1a hardness knob (Smooth↔e^8). Other presets ignore hardness (except where noted).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
#[repr(u32)]
pub enum FalloffPreset {
    /// Blender SMOOTH — `3e² − 2e³` (production default).
    #[default]
    Smooth = 0,
    /// Blender SMOOTHER — quintic smoothstep.
    Smoother = 1,
    /// Blender SPHERE — `sqrt(1 − d²)` with `d = 1 − e`.
    Sphere = 2,
    /// Blender ROOT — `sqrt(e)`.
    Root = 3,
    /// Blender SHARP — `e³`.
    Sharp = 4,
    /// Blender LIN — `e`.
    Linear = 5,
    /// Blender INVSQUARE — `e²`.
    InvSquare = 6,
    /// Blender CONSTANT — full strength inside the sphere.
    Constant = 7,
    /// F1a hardness blend: Smooth at h=0 → `e^8` at h=1.
    Blend = 8,
}

impl FalloffPreset {
    pub const ALL: [FalloffPreset; 9] = [
        Self::Smooth, Self::Smoother, Self::Sphere, Self::Root, Self::Sharp,
        Self::Linear, Self::InvSquare, Self::Constant, Self::Blend,
    ];
    pub fn label(self) -> &'static str {
        match self {
            Self::Smooth => "Smooth",
            Self::Smoother => "Smoother",
            Self::Sphere => "Sphere",
            Self::Root => "Root",
            Self::Sharp => "Sharp",
            Self::Linear => "Linear",
            Self::InvSquare => "InvSq",
            Self::Constant => "Constant",
            Self::Blend => "Blend",
        }
    }
    /// GPU pack id (fits in `box_dim.w`).
    #[inline]
    pub fn as_u32(self) -> u32 { self as u32 }
    #[inline]
    pub fn from_u32(v: u32) -> Self {
        match v {
            1 => Self::Smoother,
            2 => Self::Sphere,
            3 => Self::Root,
            4 => Self::Sharp,
            5 => Self::Linear,
            6 => Self::InvSquare,
            7 => Self::Constant,
            8 => Self::Blend,
            _ => Self::Smooth,
        }
    }
}

/// Greyscale brush-tip alpha map (ZBrush / 3D-Coat style). Multiplies radial falloff in the stamp
/// plane: white = full strength, black = no effect. Sampled in brush-local UV over the disk.
#[derive(Clone, Debug)]
pub struct BrushTipAlpha {
    pub width: u32,
    pub height: u32,
    /// Row-major luminance 0..1, length = width * height.
    pub alpha: Vec<f32>,
}

impl BrushTipAlpha {
    /// Bilinear sample. `u,v` in [-1,1] brush-disk coords (centre = 0). Outside the map → 0.
    #[inline]
    pub fn sample(&self, u: f32, v: f32) -> f32 {
        let w = self.width.max(1);
        let h = self.height.max(1);
        if self.alpha.len() < (w * h) as usize { return 1.0; }
        // Map [-1,1] → texel space; clamp to edge (tips usually black-bordered).
        let fx = ((u * 0.5 + 0.5) * w as f32 - 0.5).clamp(0.0, (w - 1) as f32);
        let fy = ((v * 0.5 + 0.5) * h as f32 - 0.5).clamp(0.0, (h - 1) as f32);
        let x0 = fx.floor() as u32;
        let y0 = fy.floor() as u32;
        let x1 = (x0 + 1).min(w - 1);
        let y1 = (y0 + 1).min(h - 1);
        let tx = fx - x0 as f32;
        let ty = fy - y0 as f32;
        let at = |x: u32, y: u32| self.alpha[(y * w + x) as usize];
        let a00 = at(x0, y0);
        let a10 = at(x1, y0);
        let a01 = at(x0, y1);
        let a11 = at(x1, y1);
        let a0 = a00 + (a10 - a00) * tx;
        let a1 = a01 + (a11 - a01) * tx;
        (a0 + (a1 - a0) * ty).clamp(0.0, 1.0)
    }

    /// Sample with mid-point contrast (`1` = identity, `<1` softens, `>1` punches).
    #[inline]
    pub fn sample_contrast(&self, u: f32, v: f32, contrast: f32) -> f32 {
        let a = self.sample(u, v);
        let c = contrast.clamp(0.05, 4.0);
        if (c - 1.0).abs() < 1e-4 { return a; }
        ((a - 0.5) * c + 0.5).clamp(0.0, 1.0)
    }

    /// Build from 8-bit greyscale (or luma of RGB) pixels, row-major.
    pub fn from_luma_u8(width: u32, height: u32, pixels: &[u8]) -> Option<Self> {
        let n = (width as usize).checked_mul(height as usize)?;
        if width == 0 || height == 0 || pixels.len() < n { return None; }
        let alpha: Vec<f32> = pixels[..n].iter().map(|&p| p as f32 / 255.0).collect();
        Some(Self { width, height, alpha })
    }

    /// Procedural **Square Clay** alpha — soft-edged square stamp (ZBrush Clay Buildup style).
    /// `res` texels/side (64–256). `edge_soft` 0..1 (higher = softer rim).
    pub fn square_clay(res: u32, edge_soft: f32) -> Self {
        let res = res.clamp(32, 256);
        let soft = edge_soft.clamp(0.02, 0.45);
        let mut alpha = Vec::with_capacity((res * res) as usize);
        let inv = 1.0 / res as f32;
        // Half-extent of solid square core in [-1,1] UV (leave margin for soft edge + circular crop).
        let half = 0.72;
        let outer = half + soft;
        for y in 0..res {
            for x in 0..res {
                let u = (x as f32 + 0.5) * inv * 2.0 - 1.0;
                let v = (y as f32 + 0.5) * inv * 2.0 - 1.0;
                // Circular crop so tips stay disk-compatible with radial falloff.
                let r = (u * u + v * v).sqrt();
                if r > 1.0 {
                    alpha.push(0.0);
                    continue;
                }
                // Chebyshev distance to square centre → 0 inside, rises at edges.
                let d = u.abs().max(v.abs());
                let a = if d <= half {
                    1.0
                } else if d >= outer {
                    0.0
                } else {
                    let t = (d - half) / (outer - half).max(1e-6);
                    // Smoothstep rim
                    let s = t * t * (3.0 - 2.0 * t);
                    1.0 - s
                };
                // Slight radial vignette so corners don't hard-clip the disk.
                let vignette = if r > 0.85 {
                    let t = ((r - 0.85) / 0.15).clamp(0.0, 1.0);
                    1.0 - t * t * (3.0 - 2.0 * t)
                } else {
                    1.0
                };
                alpha.push((a * vignette).clamp(0.0, 1.0));
            }
        }
        Self { width: res, height: res, alpha }
    }

    /// Procedural **Thin Line / Sharp Crease** alpha — vertical hard stripe (ZBrush Dam Standard style).
    /// `half_width` is core half-width in [-1,1] UV (≈0.04–0.10). Soft rim keeps the line from aliasing.
    pub fn thin_line_crease(res: u32, half_width: f32) -> Self {
        let res = res.clamp(32, 256);
        let half = half_width.clamp(0.02, 0.20);
        let soft = (half * 0.85).clamp(0.02, 0.12);
        let outer = half + soft;
        let mut alpha = Vec::with_capacity((res * res) as usize);
        let inv = 1.0 / res as f32;
        for y in 0..res {
            for x in 0..res {
                let u = (x as f32 + 0.5) * inv * 2.0 - 1.0;
                let v = (y as f32 + 0.5) * inv * 2.0 - 1.0;
                let r = (u * u + v * v).sqrt();
                if r > 1.0 {
                    alpha.push(0.0);
                    continue;
                }
                // Thin vertical stripe (Follow Stroke rotates the tip frame).
                let d = u.abs();
                let a = if d <= half {
                    1.0
                } else if d >= outer {
                    0.0
                } else {
                    let t = (d - half) / (outer - half).max(1e-6);
                    let s = t * t * (3.0 - 2.0 * t);
                    1.0 - s
                };
                // Soft-cap the ends along the stroke axis so the dab doesn't square-off.
                let end = v.abs();
                let end_fade = if end > 0.82 {
                    let t = ((end - 0.82) / 0.18).clamp(0.0, 1.0);
                    1.0 - t * t * (3.0 - 2.0 * t)
                } else {
                    1.0
                };
                let disk = if r > 0.92 {
                    let t = ((r - 0.92) / 0.08).clamp(0.0, 1.0);
                    1.0 - t * t * (3.0 - 2.0 * t)
                } else {
                    1.0
                };
                alpha.push((a * end_fade * disk).clamp(0.0, 1.0));
            }
        }
        Self { width: res, height: res, alpha }
    }

    // ── Blender-style essential sculpt alphas (stock Blender brushes are procedural; ──
    // ── we bake matching greyscale tip discs so Azra ships a usable default library). ──

    /// Soft round disc — Blender default Draw / Smooth tip (smoothstep radial).
    pub fn soft_round(res: u32) -> Self {
        Self::from_fn(res, |u, v| {
            let r = (u * u + v * v).sqrt();
            if r >= 1.0 {
                0.0
            } else {
                let e = 1.0 - r;
                e * e * (3.0 - 2.0 * e) // smoothstep
            }
        })
    }

    /// Hard round disc — full strength with sharp rim (Blender hard Draw).
    pub fn hard_round(res: u32) -> Self {
        Self::from_fn(res, |u, v| {
            let r = (u * u + v * v).sqrt();
            if r >= 0.92 {
                if r >= 1.0 {
                    0.0
                } else {
                    let t = ((r - 0.92) / 0.08).clamp(0.0, 1.0);
                    1.0 - t * t * (3.0 - 2.0 * t)
                }
            } else {
                1.0
            }
        })
    }

    /// Soft ellipse (stretched circle) — Blender Clay / Flatten elongated tip.
    pub fn soft_ellipse(res: u32, aspect: f32) -> Self {
        let aspect = aspect.clamp(0.25, 4.0);
        Self::from_fn(res, |u, v| {
            let r = ((u * aspect).powi(2) + v * v).sqrt();
            if r >= 1.0 {
                0.0
            } else {
                let e = 1.0 - r;
                e * e * (3.0 - 2.0 * e)
            }
        })
    }

    /// Horizontal clay strip (soft rectangle) — Blender Clay Strips feel.
    pub fn clay_strips(res: u32) -> Self {
        Self::from_fn(res, |u, v| {
            let r = (u * u + v * v).sqrt();
            if r > 1.0 {
                return 0.0;
            }
            // Flat strip along V, soft on U.
            let du = (u.abs() - 0.35).max(0.0) / 0.45;
            let dv = (v.abs() - 0.75).max(0.0) / 0.25;
            let edge = (du * du + dv * dv).sqrt().clamp(0.0, 1.0);
            let a = if edge <= 0.0 {
                1.0
            } else if edge >= 1.0 {
                0.0
            } else {
                1.0 - edge * edge * (3.0 - 2.0 * edge)
            };
            let vignette = if r > 0.88 {
                let t = ((r - 0.88) / 0.12).clamp(0.0, 1.0);
                1.0 - t * t * (3.0 - 2.0 * t)
            } else {
                1.0
            };
            a * vignette
        })
    }

    /// Soft blob / cloud — multi-lobe for Blender Blob brush feel.
    pub fn soft_blob(res: u32) -> Self {
        Self::from_fn(res, |u, v| {
            let r = (u * u + v * v).sqrt();
            if r >= 1.0 {
                return 0.0;
            }
            // Three soft lobes
            let l0 = (-((u - 0.15).powi(2) + (v - 0.10).powi(2)) * 6.0).exp();
            let l1 = (-((u + 0.18).powi(2) + (v + 0.05).powi(2)) * 7.0).exp();
            let l2 = (-((u - 0.05).powi(2) + (v + 0.22).powi(2)) * 8.0).exp();
            let body = (l0 * 0.55 + l1 * 0.35 + l2 * 0.30).clamp(0.0, 1.0);
            let rim = {
                let e = (1.0 - r).clamp(0.0, 1.0);
                e * e * (3.0 - 2.0 * e)
            };
            (body * 0.85 + rim * 0.15).clamp(0.0, 1.0) * rim
        })
    }

    /// Annular ring — for scrape/trim edge work.
    pub fn ring(res: u32) -> Self {
        Self::from_fn(res, |u, v| {
            let r = (u * u + v * v).sqrt();
            if r >= 1.0 || r < 0.15 {
                return 0.0;
            }
            // Peak around r=0.55
            let d = ((r - 0.55) / 0.35).abs().clamp(0.0, 1.0);
            1.0 - d * d * (3.0 - 2.0 * d)
        })
    }

    /// Soft noise sponge — organic alpha (hash noise).
    pub fn soft_noise(res: u32, seed: u32) -> Self {
        Self::from_fn(res, |u, v| {
            let r = (u * u + v * v).sqrt();
            if r >= 1.0 {
                return 0.0;
            }
            let rim = {
                let e = (1.0 - r).clamp(0.0, 1.0);
                e * e * (3.0 - 2.0 * e)
            };
            // Value noise from hash of cell
            let n = |x: f32, y: f32| -> f32 {
                let ix = (x * 8.0 + 100.0) as i32;
                let iy = (y * 8.0 + 100.0) as i32;
                let mut h = (ix as u32)
                    .wrapping_mul(374761393)
                    .wrapping_add((iy as u32).wrapping_mul(668265263))
                    .wrapping_add(seed.wrapping_mul(0x9E37_79B9));
                h = (h ^ (h >> 13)).wrapping_mul(1274126177);
                h = h ^ (h >> 16);
                (h as f32) * (1.0 / 4_294_967_295.0)
            };
            let fx = u * 0.5 + 0.5;
            let fy = v * 0.5 + 0.5;
            let n0 = n(fx, fy);
            let n1 = n(fx * 2.1 + 0.3, fy * 2.1);
            let noise = n0 * 0.65 + n1 * 0.35;
            (0.35 + noise * 0.65) * rim
        })
    }

    /// V-groove (diagonal crease) — panel-line variant.
    pub fn v_groove(res: u32) -> Self {
        Self::from_fn(res, |u, v| {
            let r = (u * u + v * v).sqrt();
            if r >= 1.0 {
                return 0.0;
            }
            // Distance to diagonal line u=v
            let d = ((u - v).abs() * std::f32::consts::FRAC_1_SQRT_2).clamp(0.0, 1.0);
            let half = 0.08;
            let soft = 0.12;
            let a = if d <= half {
                1.0
            } else if d >= half + soft {
                0.0
            } else {
                let t = (d - half) / soft;
                1.0 - t * t * (3.0 - 2.0 * t)
            };
            let rim = if r > 0.9 {
                let t = ((r - 0.9) / 0.1).clamp(0.0, 1.0);
                1.0 - t * t * (3.0 - 2.0 * t)
            } else {
                1.0
            };
            a * rim
        })
    }

    /// Dot cluster — multi-dot stamp for stipple / texture.
    pub fn dots(res: u32) -> Self {
        Self::from_fn(res, |u, v| {
            let r = (u * u + v * v).sqrt();
            if r >= 1.0 {
                return 0.0;
            }
            let dots = [
                (0.0, 0.0, 0.22),
                (0.35, 0.25, 0.14),
                (-0.30, 0.28, 0.13),
                (0.28, -0.32, 0.12),
                (-0.25, -0.22, 0.11),
            ];
            let mut a = 0.0f32;
            for &(cx, cy, rad) in &dots {
                let d = ((u - cx).powi(2) + (v - cy).powi(2)).sqrt() / rad;
                if d < 1.0 {
                    let e = 1.0 - d;
                    a = a.max(e * e * (3.0 - 2.0 * e));
                }
            }
            let rim = if r > 0.92 {
                let t = ((r - 0.92) / 0.08).clamp(0.0, 1.0);
                1.0 - t * t * (3.0 - 2.0 * t)
            } else {
                1.0
            };
            a * rim
        })
    }

    /// Generic disc sampler: `f(u,v)` with u,v in [-1,1], disc exterior forced to 0.
    pub fn from_fn(res: u32, f: impl Fn(f32, f32) -> f32) -> Self {
        let res = res.clamp(32, 256);
        let mut alpha = Vec::with_capacity((res * res) as usize);
        let inv = 1.0 / res as f32;
        for y in 0..res {
            for x in 0..res {
                let u = (x as f32 + 0.5) * inv * 2.0 - 1.0;
                let v = (y as f32 + 0.5) * inv * 2.0 - 1.0;
                alpha.push(f(u, v).clamp(0.0, 1.0));
            }
        }
        Self {
            width: res,
            height: res,
            alpha,
        }
    }
}

/// Orthonormal tangent frame for a unit (or near-unit) normal — brush-tip UV axes in the stamp plane.
#[inline]
pub fn brush_tip_basis(normal: Vec3) -> (Vec3, Vec3) {
    let n = normal.normalize_or_zero();
    if n.length_squared() < 1e-12 {
        return (Vec3::X, Vec3::Y);
    }
    let a = if n.x.abs() < 0.9 { Vec3::X } else { Vec3::Y };
    let t = n.cross(a).normalize_or_zero();
    let b = n.cross(t);
    (t, b)
}

/// Radial brush weight curve — how the per-voxel strength falls from 1 at the brush centre to 0 at
/// the rim (argument `e = 1 - t`, where `t = dist/radius` ∈ [0,1]). Always 0..1, monotonically
/// non-decreasing in `e`, with `weight(0) == 0` (rim) and `weight(1) == 1` (centre) except Constant
/// which is 1 for all interior samples. CPU and GPU (`voxel_brush.wgsl`) must stay op-aligned.
#[inline]
pub fn falloff_weight(e: f32, hardness: f32) -> f32 {
    falloff_weight_preset(e, hardness, FalloffPreset::Smooth)
}

/// Blender-style hardness remap (sculpt brush hardness).
///
/// `e` = inside-ness (1 centre, 0 rim). `hardness` 0 = soft (curve over full radius),
/// 1 = hard (full-strength core to the rim, razor edge).
///
/// Matches Blender: full strength while `dist/radius < hardness`, then curve over the outer shell.
/// Applies to **every** falloff preset — not only Blend.
#[inline]
pub fn hardness_remap_e(e: f32, hardness: f32) -> f32 {
    let x = e.clamp(0.0, 1.0);
    let h = hardness.clamp(0.0, 1.0);
    if h <= 1e-6 {
        return x;
    }
    if h >= 0.999 {
        return if x > 1e-6 { 1.0 } else { 0.0 };
    }
    // Core plateau: e >= (1 − h)  ⇔  dist/radius <= h
    let core = 1.0 - h;
    if x >= core {
        1.0
    } else {
        (x / core).clamp(0.0, 1.0)
    }
}

/// Preset curve on already-remapped inside-ness `x` (no hardness — hardness applied first).
#[inline]
fn falloff_curve_only(x: f32, preset: FalloffPreset) -> f32 {
    let x = x.clamp(0.0, 1.0);
    match preset {
        FalloffPreset::Smooth => x * x * (3.0 - 2.0 * x),
        FalloffPreset::Smoother => {
            let x2 = x * x;
            let x3 = x2 * x;
            x3 * (10.0 + x * (-15.0 + 6.0 * x))
        }
        FalloffPreset::Sphere => {
            let d = 1.0 - x;
            (1.0 - d * d).max(0.0).sqrt()
        }
        FalloffPreset::Root => x.sqrt(),
        FalloffPreset::Sharp => x * x * x,
        FalloffPreset::Linear => x,
        FalloffPreset::InvSquare => x * x,
        FalloffPreset::Constant => if x > 0.0 { 1.0 } else { 0.0 },
        // Blend: Smooth at soft rim, sharper (e^8) shoulder — extra punch after hardness plateau.
        FalloffPreset::Blend => {
            let soft = x * x * (3.0 - 2.0 * x);
            let sharp = x.powf(8.0);
            // Mid blend (hardness already made the core); keep some curve variety.
            soft * 0.35 + sharp * 0.65
        }
    }
}

/// Preset-aware falloff with **Blender-style hardness on every preset**.
/// `hardness` 0 = soft full-radius curve; 1 = hard plateau to the rim.
#[inline]
pub fn falloff_weight_preset(e: f32, hardness: f32, preset: FalloffPreset) -> f32 {
    let x = hardness_remap_e(e, hardness);
    falloff_curve_only(x, preset)
}

/// Blender's SPACING-OVERLAP strength attenuation — verbatim port of
/// `paint_stroke_integrate_overlap` + `paint_stroke_overlapped_curve`
/// (paint_stroke.cc, Blender 3.6; gated there by BRUSH_SPACE_ATTEN, default ON for
/// sculpt): dabs land `spacing` apart, so any surface point is covered by ~1/spacing
/// overlapping dabs — Blender divides the per-dab strength by the PEAK summed falloff of
/// that dab train, which is exactly what makes a stroke's total deposit
/// `≈ strength × radius` regardless of spacing. Without it, halving spacing doubles the
/// deposit (the "nothing feels like Blender" class).
///
/// `spacing` is in RADIUS units (AZRA's `stroke_spacing`; Blender's `h = spacing%/50` on
/// the same [-1,1] curve domain). Returns 1.0 at spacing ≥ 2 radii (no overlap), else
/// `1 / max_phase Σ_k falloff(|x − k·spacing|)` sampled at 10 phases like Blender.
pub fn stroke_overlap_factor(spacing: f32, hardness: f32, preset: FalloffPreset) -> f32 {
    let h = spacing.max(0.002);
    if h >= 2.0 {
        return 1.0; // Blender: spacing ≥ 100% of diameter → no attenuation
    }
    let n = (2.0 / h).ceil() as usize; // dab count covering the [-1, 1] curve domain
    let m = 10usize; // Blender samples 10 phases
    let mut max = 0.0f32;
    for i in 0..m {
        let x0 = (i as f32 / m as f32) - 1.0;
        let mut sum = 0.0f32;
        for k in 0..=n {
            let xx = (x0 + k as f32 * h).abs();
            if xx < 1.0 {
                sum += falloff_weight_preset(1.0 - xx, hardness, preset);
            }
        }
        if sum > max {
            max = sum;
        }
    }
    if max <= 1e-6 {
        1.0
    } else {
        (1.0 / max).min(1.0)
    }
}

/// Distance fraction `t = dist/radius` where the falloff drops to HALF strength — the brush
/// overlay's inner "50%" ring (ZBrush focal-shift ring / Blender's inner cursor circle).
/// Bisects the ACTUAL curve (`falloff_weight_preset`) so the indicator can never drift from
/// the stamp math. `None` for Constant (full strength to the rim — no crossing to show).
pub fn falloff_half_radius_t(hardness: f32, preset: FalloffPreset) -> Option<f32> {
    if matches!(preset, FalloffPreset::Constant) {
        return None;
    }
    // w(1 − t) is monotonically non-increasing in t, w(0)=1 centre, w(1)=0 rim → one crossing.
    let (mut lo, mut hi) = (0.0f32, 1.0f32);
    for _ in 0..24 {
        let mid = 0.5 * (lo + hi);
        if falloff_weight_preset(1.0 - mid, hardness, preset) >= 0.5 {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    Some(0.5 * (lo + hi))
}

/// Soft round brush-tip falloff (classic DCC tip image): `w = (1 − t²)²` with `t = dist/radius`.
/// Centre = 1, smooth round shoulder to 0 at the rim — no hard plateau. Used by Move mesh-grab
/// (GPU `gbuffer.wgsl`, CPU `MeshGrab::plateau`, bake worker) so live preview and commit match.
#[inline]
pub fn grab_soft_round_falloff(dist: f32, radius: f32) -> f32 {
    let t = (dist / radius.max(1e-6)).clamp(0.0, 1.0);
    let e = 1.0 - t * t;
    e * e
}

/// STROKE-AXIS PINCH basis (BLENDER_BRUSH_SPEC §2), shared by the density Pinch gather and the
/// Surface-mode Pinch/Crease vertex dab so there is ONE implementation of the geometry.
///
/// Given the vertex/sample offset from the dab centre `rel = pos − center`, the surface `normal`,
/// and the UNIT stroke `tangent`, return the in-plane vector to squeeze ALONG (pointing away from
/// the stroke axis line, toward the vertex). Squeezing a point back along `−pinch_axis_pull()`
/// moves it toward the stroke axis → a crease that runs along the stroke.
///
/// - First drop the normal component of `rel` (stay in the tangent plane).
/// - With a non-degenerate `tangent`, keep ONLY the cross-stroke (bitangent) component: material
///   lying along the stroke line stays put, only the sideways spread is gathered.
/// - `tangent ≈ 0` (first frame / no motion) → axisymmetric fallback (the whole in-plane vector),
///   so a single dab still pinches radially. Matches the density path's ZERO-tangent branch.
///
/// Returns `Vec3::ZERO` when there is nothing to pull (vertex on the axis / degenerate). Callers
/// scale the UNIT direction (`ret.normalize_or_zero()`) by their own falloff·strength.
#[inline]
pub fn pinch_axis_pull(rel: glam::Vec3, normal: glam::Vec3, tangent: glam::Vec3) -> glam::Vec3 {
    let tang_full = rel - normal * rel.dot(normal); // in-plane component
    if tangent.length_squared() > 1e-10 {
        let bitan = tangent.cross(normal).normalize_or_zero();
        if bitan.length_squared() > 1e-10 {
            return bitan * tang_full.dot(bitan); // keep only cross-stroke part
        }
    }
    tang_full
}

/// Optional firmer core for density Smudge/legacy plateau path. `hardness` 0 → soft-only,
/// 1 → wide rigid core (0.55–0.95). Move mesh path prefers [`grab_soft_round_falloff`].
#[inline]
pub fn grab_plateau_core(hardness: f32) -> f32 {
    0.55 + 0.40 * hardness.clamp(0.0, 1.0)
}

#[inline]
pub fn grab_plateau_falloff(t: f32, hardness: f32) -> f32 {
    // hardness ~0 → soft round; hardness →1 → plateau core + soft rim.
    let t = t.clamp(0.0, 1.0);
    let soft = {
        let e = 1.0 - t * t;
        e * e
    };
    let h = hardness.clamp(0.0, 1.0);
    if h < 1e-3 { return soft; }
    let core = grab_plateau_core(h);
    let plateau = if t <= core {
        1.0
    } else {
        let u = ((t - core) / (1.0 - core).max(1e-6)).clamp(0.0, 1.0);
        1.0 - u * u * (3.0 - 2.0 * u)
    };
    soft * (1.0 - h) + plateau * h
}

/// How the per-stamp DEPOSIT DIRECTION (the `normal` fed to plane/normal-relative brushes —
/// Flatten/Clay/Scrape/Fill/ClayBuildup/Pinch) is SOURCED. This is purely the choice of *which
/// vector* becomes the stamp normal; it does NOT change any brush kernel. Brushes that ignore the
/// normal (Draw/Erase/Smooth/Inflate) are unaffected, and Move drags along its own cursor delta
/// (`move_prev_pos`), never the normal, so it is exempt entirely. TOOL state — carries across body
/// switches like brush/falloff/symmetry.
///
/// Modes mirror 3D-Coat / ZBrush / Blender sculpt-plane families:
/// Surface, Normal, View, Brush, Fixed Axis, Average Normal, Original Normal, Tangent.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Hash)]
#[repr(u32)]
pub enum BrushDirection {
    /// **Surface Direction** — live surface normal re-sampled each stamp along the stroke
    /// (follows the evolving clay; Blender continuous Area Plane). DEFAULT (Blender stroke
    /// model 2026-07-23): deposit follows the local form, so a stroke wraps a sphere instead
    /// of pushing in the first-contact direction.
    #[default]
    Surface = 0,
    /// **Normal** — surface normal at the current cursor hit only (this frame’s raycast),
    /// not re-sampled per interpolated dab. Stable within a frame, updates as the cursor moves.
    Normal = 1,
    /// **View Direction** — toward the camera (`-ray_dir`). Deposit faces the screen.
    View = 2,
    /// **Brush Direction** — stroke path direction as the plane normal (along the dab motion).
    /// Falls back to surface if the stroke has not moved yet.
    Brush = 3,
    /// **Fixed Axis X** — world +X.
    AxisX = 4,
    /// **Fixed Axis Y** — world +Y.
    AxisY = 5,
    /// **Fixed Axis Z** — world +Z.
    AxisZ = 6,
    /// **Average Normal** — running average of surface normals during the stroke
    /// (Blender Area Plane / soft average under the brush path).
    Average = 7,
    /// **Original Normal** — surface normal latched at the first click of the stroke
    /// (ZBrush-style locked plane, explicit option — no longer the default: a frozen frame
    /// is exactly "the brush does not follow the surface").
    FirstPoint = 8,
    /// **Tangent Direction** — stroke path × surface (binormal of the stroke on the skin).
    /// Useful for creases/panel lines running across the stroke. Falls back to surface if
    /// stroke or surface is degenerate.
    Tangent = 9,
}

/// Inputs for [`BrushDirection::resolve`] — all already in voxel-local space and unit-length where possible.
#[derive(Clone, Copy, Debug)]
pub struct BrushDirContext {
    /// Live surface normal at the contact (or stamp) point.
    pub surface: Vec3,
    /// Toward camera (`-ray_dir`).
    pub view: Vec3,
    /// Latched first-click normal (Original).
    pub first_point: Option<Vec3>,
    /// Running average normal for the stroke (Average).
    pub average: Option<Vec3>,
    /// Stroke path direction (current − previous), unit or zero.
    pub stroke_dir: Option<Vec3>,
}

impl BrushDirection {
    /// All modes in UI order (industry labels).
    pub const ALL: [BrushDirection; 10] = [
        BrushDirection::Surface,
        BrushDirection::Normal,
        BrushDirection::View,
        BrushDirection::Brush,
        BrushDirection::Average,
        BrushDirection::FirstPoint,
        BrushDirection::Tangent,
        BrushDirection::AxisX,
        BrushDirection::AxisY,
        BrushDirection::AxisZ,
    ];

    /// Short UI label.
    #[inline]
    pub fn label(self) -> &'static str {
        match self {
            BrushDirection::Surface => "Surface",
            BrushDirection::Normal => "Normal",
            BrushDirection::View => "View",
            BrushDirection::Brush => "Brush",
            BrushDirection::Average => "Average",
            BrushDirection::FirstPoint => "Original",
            BrushDirection::Tangent => "Tangent",
            BrushDirection::AxisX => "X",
            BrushDirection::AxisY => "Y",
            BrushDirection::AxisZ => "Z",
        }
    }

    /// Longer hover tip.
    #[inline]
    pub fn tip(self) -> &'static str {
        match self {
            BrushDirection::Surface => "Surface Direction — density normal under the tip each dab (pre-brush skin; not gbuf/view).",
            BrushDirection::Normal => "Normal — density normal at cursor hit this frame (pre-brush field).",
            BrushDirection::View => "View Direction — toward the camera; deposit faces the screen.",
            BrushDirection::Brush => "Brush Direction — along the stroke path (dab motion).",
            BrushDirection::Average => "Average Normal — SPATIAL area under brush (Avg region × radius). Flattens curves.",
            BrushDirection::FirstPoint => "Original Normal — first click of the stroke (locked plane). Use Smooth normal to soften.",
            BrushDirection::Tangent => "Tangent Direction — stroke × surface (across the stroke on the skin).",
            BrushDirection::AxisX => "Fixed Axis X — world +X.",
            BrushDirection::AxisY => "Fixed Axis Y — world +Y.",
            BrushDirection::AxisZ => "Fixed Axis Z — world +Z.",
        }
    }

    /// True when this mode needs live per-stamp surface re-sample (Surface Direction).
    #[inline]
    pub fn wants_live_surface(self) -> bool {
        matches!(self, BrushDirection::Surface)
    }

    /// True when this mode needs a running average updated each stamp.
    #[inline]
    pub fn wants_average(self) -> bool {
        matches!(self, BrushDirection::Average)
    }

    /// True when this mode needs stroke path direction (Brush / Tangent).
    #[inline]
    pub fn wants_stroke_dir(self) -> bool {
        matches!(self, BrushDirection::Brush | BrushDirection::Tangent)
    }

    /// True when first-click latch should be recorded.
    #[inline]
    pub fn wants_first_point_latch(self) -> bool {
        matches!(self, BrushDirection::FirstPoint)
    }

    /// Resolve deposit direction for one stamp. Result is the stamp normal for plane/normal brushes.
    #[inline]
    pub fn resolve(self, ctx: BrushDirContext) -> Vec3 {
        let surface = ctx.surface.normalize_or_zero();
        let view = ctx.view.normalize_or_zero();
        let stroke = ctx.stroke_dir.map(|d| d.normalize_or_zero()).unwrap_or(Vec3::ZERO);
        match self {
            BrushDirection::Surface | BrushDirection::Normal => {
                if surface.length_squared() > 1e-12 { surface } else { Vec3::Y }
            }
            BrushDirection::View => {
                if view.length_squared() > 1e-12 { view } else { surface }
            }
            BrushDirection::FirstPoint => {
                ctx.first_point.map(|n| n.normalize_or_zero()).filter(|n| n.length_squared() > 1e-12)
                    .unwrap_or(surface)
            }
            BrushDirection::Average => {
                ctx.average.map(|n| n.normalize_or_zero()).filter(|n| n.length_squared() > 1e-12)
                    .unwrap_or(surface)
            }
            BrushDirection::Brush => {
                if stroke.length_squared() > 1e-12 { stroke } else { surface }
            }
            BrushDirection::Tangent => {
                // Binormal of stroke on the surface: stroke × surface (or surface × stroke).
                if stroke.length_squared() > 1e-12 && surface.length_squared() > 1e-12 {
                    let t = stroke.cross(surface).normalize_or_zero();
                    if t.length_squared() > 1e-12 { t } else { surface }
                } else {
                    surface
                }
            }
            BrushDirection::AxisX => Vec3::X,
            BrushDirection::AxisY => Vec3::Y,
            BrushDirection::AxisZ => Vec3::Z,
        }
    }

    /// Backward-compatible resolve (surface, view, first_point only). Prefer [`resolve`] with
    /// [`BrushDirContext`] for Average / Brush / Tangent.
    #[inline]
    pub fn resolve_simple(self, surface: Vec3, view: Vec3, first_point: Option<Vec3>) -> Vec3 {
        self.resolve(BrushDirContext {
            surface,
            view,
            first_point,
            average: None,
            stroke_dir: None,
        })
    }

    /// Map this tool-state direction onto the recipe-side `brush::Direction` slot.
    #[inline]
    pub fn to_recipe(self) -> crate::brush::Direction {
        use crate::brush::Direction as D;
        match self {
            BrushDirection::Surface | BrushDirection::Normal => D::SurfaceNormal,
            BrushDirection::View => D::ViewDirection,
            BrushDirection::FirstPoint | BrushDirection::Average => D::FirstPoint,
            BrushDirection::Brush | BrushDirection::Tangent => D::SurfaceNormal, // resolved producer-side
            BrushDirection::AxisX => D::AxisX,
            BrushDirection::AxisY => D::AxisY,
            BrushDirection::AxisZ => D::AxisZ,
        }
    }

    #[inline]
    pub fn as_u32(self) -> u32 { self as u32 }

    #[inline]
    pub fn from_u32(v: u32) -> Self {
        match v {
            0 => BrushDirection::Surface,
            1 => BrushDirection::Normal,
            2 => BrushDirection::View,
            3 => BrushDirection::Brush,
            4 => BrushDirection::AxisX,
            5 => BrushDirection::AxisY,
            6 => BrushDirection::AxisZ,
            7 => BrushDirection::Average,
            9 => BrushDirection::Tangent,
            _ => BrushDirection::FirstPoint, // 8 and unknown
        }
    }
}

/// Dirty voxel AABB (inclusive) for incremental re-meshing / upload.
#[derive(Clone, Copy, Debug)]
pub struct VoxelDirty {
    pub min: [u32; 3],
    pub max: [u32; 3],
}

/// Records the pre-stroke density of every voxel a drag stroke MODIFIES, so undo can restore exactly
/// the changed voxels. Dedup is SPARSE — a per-brick 4096-bit visited mask (`HashMap<brick, [u64;64]>`,
/// 512 B/brick) keyed off the same 16³ bricks the density uses — because a DENSE `Vec<bool>` would be
/// res³ (134 MB at 512³, allocated + zeroed every stroke), the original high-res blocker. A cached
/// last-brick mask keeps dedup HASH-FREE within a brick (brush sweeps contiguous voxels → the common
/// case); only a brick crossing (~1 in 16 writes) touches the map. The accumulated `changes` ARE the
/// undo delta (first-touch value wins across the whole stroke).
pub struct StrokeRecorder {
    res: u32,
    brick_res: u32,
    masks: std::collections::HashMap<u32, [u64; 64]>, // brick index → 4096-bit visited mask
    last_bi: u32,             // brick currently cached in `last_mask` (u32::MAX = none)
    last_mask: [u64; 64],     // cached mask for `last_bi` — hash-free dedup within a brick
    last_dirty: bool,         // `last_mask` has bits not yet written back to `masks`
    changes: Vec<(u32, f32)>, // (flat index, pre-stroke density)
}

impl Default for StrokeRecorder {
    fn default() -> Self {
        StrokeRecorder {
            res: 0, brick_res: 0, masks: std::collections::HashMap::new(),
            last_bi: u32::MAX, last_mask: [0u64; 64], last_dirty: false, changes: Vec::new(),
        }
    }
}

impl StrokeRecorder {
    /// Start a new stroke against a `res³` grid: reset dedup state. O(touched bricks), not O(res³).
    pub fn begin(&mut self, res: u32) {
        self.res = res;
        self.brick_res = (res + 15) / 16; // BRICK = 16
        self.masks.clear();
        self.last_bi = u32::MAX;
        self.last_mask = [0u64; 64];
        self.last_dirty = false;
        self.changes.clear();
    }

    /// Record `pre` (density before the write) for voxel `idx` the FIRST time it is modified this
    /// stroke. Subsequent touches are ignored, preserving the earliest (pre-stroke) value.
    #[inline]
    pub fn record(&mut self, idx: u32, pre: f32) {
        let r = self.res;
        if r == 0 { return; } // begin() not called
        let (x, y, z) = (idx % r, (idx / r) % r, idx / (r * r));
        let br = self.brick_res;
        let bi = (x / 16) + (y / 16) * br + (z / 16) * br * br;
        let li = (x & 15) + (y & 15) * 16 + (z & 15) * 256; // 0..4096 bit within the brick
        if bi != self.last_bi {
            // Leaving a brick: write its mask back, then load the target brick's (or zero if new).
            if self.last_dirty && self.last_bi != u32::MAX {
                self.masks.insert(self.last_bi, self.last_mask);
            }
            self.last_bi = bi;
            self.last_mask = self.masks.get(&bi).copied().unwrap_or([0u64; 64]);
            self.last_dirty = false;
        }
        let (w, b) = ((li >> 6) as usize, li & 63);
        if self.last_mask[w] & (1u64 << b) == 0 {
            self.last_mask[w] |= 1u64 << b;
            self.last_dirty = true;
            self.changes.push((idx, pre));
        }
    }

    pub fn is_empty(&self) -> bool { self.changes.is_empty() }

    /// Number of distinct voxels recorded so far this stroke (first-touch deduped). Used to detect
    /// whether a `record()` call was a NEW first touch vs a duplicate (count delta).
    pub fn len(&self) -> usize { self.changes.len() }

    /// Take the accumulated delta (leaves `changes` empty; dedup masks are reset by the next `begin`).
    pub fn take(&mut self) -> Vec<(u32, f32)> { std::mem::take(&mut self.changes) }

    /// Drop any uncommitted changes without a full dedup reset (the next `begin` clears the masks).
    pub fn clear(&mut self) { self.changes.clear(); }
}

/// ELASTIC GRAB (3D-Coat / Blender Grab) state for the Move brush. The Move brush no longer advects
/// the density field incrementally per-stamp (which compounded each frame → felt like "creating
/// voxels"). Instead the grabbed region is captured ONCE at stroke start, and every drag frame the
/// region is RESTORED to its pristine state and re-deformed by the CUMULATIVE drag (anchor → cursor),
/// falloff-weighted. Because each frame re-derives the deform from the pristine snapshot, dragging the
/// cursor back UNDOES the stretch elastically (true grab-pull, not an accumulating smear). On release
/// the field already holds the final deform; the pristine→final delta is recorded as ONE undo step.
///
/// CPU-only (Move is already CPU-forced). The pristine map is first-touch: a voxel's pristine value is
/// captured the first frame the (growing) deform region reaches it, AFTER that frame's restore pass —
/// so it is always the true pre-grab value, never a half-deformed one.
#[derive(Default)]
pub struct ElasticGrab {
    /// Grab anchor: the surface contact point at stroke start (voxel-LOCAL space). The cumulative drag
    /// is `cursor - anchor`; the deform always measures from here so it is reversible.
    pub anchor: Vec3,
    /// Brush radius latched at stroke start (the grab footprint; the drag tapers to 0 at this radius).
    pub radius: f32,
    /// Falloff hardness latched at stroke start (shared radial curve).
    pub hardness: f32,
    /// View-plane normal latched at stroke start (voxel-LOCAL): the camera ray direction through the
    /// grab point. The per-frame drag is the cursor ray intersected with the plane (`anchor`, `plane_n`),
    /// so the grab follows the mouse in the VIEW PLANE ("direct line between grab point and cursor using
    /// the camera angle") instead of chasing the deforming surface — a true grab pull, not a smear.
    pub plane_n: Vec3,
    /// Surface normal at the grab contact (voxel-LOCAL), latched at stroke start. Used by the brush
    /// ring overlay so the starting circle lies in the surface plane of the grab region (3D-Coat style),
    /// not flat in screen space. Independent of `plane_n` (view-plane for drag).
    pub surface_n: Vec3,
    /// Pristine pre-grab density per touched voxel (flat index → value), captured first-touch.
    pub(super) pristine: std::collections::HashMap<u32, f32>, // visibility widened for the voxel_sculpt file split (same-module access before)
    /// True once `begin` has latched a grab (so a stray drag frame without a start is ignored).
    pub active: bool,
}

impl ElasticGrab {
    /// Latch a fresh grab at `anchor` with brush `radius`/`hardness`, the view-plane normal
    /// `plane_n` (camera ray dir through the grab point, voxel-LOCAL), and the surface normal
    /// `surface_n` at the contact (for the oriented starting ring). Clears any prior pristine map.
    pub fn begin(&mut self, anchor: Vec3, radius: f32, hardness: f32, plane_n: Vec3, surface_n: Vec3) {
        self.anchor = anchor;
        self.radius = radius;
        self.hardness = hardness;
        self.plane_n = plane_n.normalize_or_zero();
        self.surface_n = surface_n.normalize_or_zero();
        self.pristine.clear();
        self.active = true;
    }

    /// True if a grab is in progress.
    pub fn is_active(&self) -> bool { self.active }

    /// Number of voxels captured pristine so far (== the size of the eventual undo delta).
    pub fn len(&self) -> usize { self.pristine.len() }

    /// End the grab, returning the pristine→(current) undo delta the caller records as ONE step.
    /// `grid` must be the post-deform grid (the live field after the last preview). The delta is the
    /// list of each touched voxel's PRE-grab value — replaying it through `restore_delta` reverts the
    /// whole grab. Resets state.
    pub fn finish(&mut self) -> Vec<(u32, f32)> {
        self.active = false;
        let delta: Vec<(u32, f32)> = self.pristine.drain().collect();
        delta
    }

    /// Drop the grab without producing a delta (cancelled / no movement).
    pub fn cancel(&mut self) { self.active = false; self.pristine.clear(); }
}

#[cfg(test)]
mod falloff_half_radius_tests {
    use super::*;

    /// The inner-ring solver must return the true 50%-strength distance of the ACTUAL curve.
    #[test]
    fn half_radius_matches_curve() {
        for &preset in &[
            FalloffPreset::Smooth,
            FalloffPreset::Smoother,
            FalloffPreset::Sphere,
            FalloffPreset::Root,
            FalloffPreset::Sharp,
            FalloffPreset::Linear,
            FalloffPreset::InvSquare,
            FalloffPreset::Blend,
        ] {
            for &h in &[0.0f32, 0.2, 0.5, 0.9] {
                let t = falloff_half_radius_t(h, preset)
                    .unwrap_or_else(|| panic!("{preset:?} h={h} must have a 50% crossing"));
                let w = falloff_weight_preset(1.0 - t, h, preset);
                assert!(
                    (w - 0.5).abs() < 1e-3,
                    "{preset:?} h={h}: w({t}) = {w}, expected 0.5"
                );
            }
        }
        // Smoothstep is symmetric: 50% strength sits exactly at half radius when soft.
        let t = falloff_half_radius_t(0.0, FalloffPreset::Smooth).unwrap();
        assert!((t - 0.5).abs() < 1e-3, "soft Smooth 50% point should be t=0.5, got {t}");
    }

    /// Constant = full strength to the rim — no inner ring to draw.
    #[test]
    fn constant_has_no_half_radius() {
        assert!(falloff_half_radius_t(0.0, FalloffPreset::Constant).is_none());
        assert!(falloff_half_radius_t(0.7, FalloffPreset::Constant).is_none());
    }

    /// Hardness pushes the 50% point outward (harder core = wider full-strength plateau).
    #[test]
    fn hardness_widens_the_core() {
        let soft = falloff_half_radius_t(0.0, FalloffPreset::Smooth).unwrap();
        let hard = falloff_half_radius_t(0.8, FalloffPreset::Smooth).unwrap();
        assert!(hard > soft, "hardness 0.8 core {hard} must be wider than soft {soft}");
    }
}

#[cfg(test)]
mod stroke_overlap_tests {
    use super::*;

    /// The Blender contract (paint_stroke_integrate_overlap): a stroke pass deposits
    /// `≈ strength × radius` REGARDLESS of spacing — per-dab strength × the summed
    /// overlap of the dab train must be ~constant across spacings.
    #[test]
    fn pass_deposit_is_spacing_independent() {
        let (h, preset) = (0.0f32, FalloffPreset::Smooth);
        let deposit = |spacing: f32| -> f32 {
            let f = stroke_overlap_factor(spacing, h, preset);
            // Peak summed falloff of a dab train at this spacing (the same integral
            // the factor divides by, evaluated independently on a fine phase grid).
            let mut max = 0.0f32;
            for p in 0..50 {
                let x0 = (p as f32 / 50.0) - 1.0;
                let mut sum = 0.0;
                let n = (2.0 / spacing).ceil() as usize;
                for k in 0..=n {
                    let xx = (x0 + k as f32 * spacing).abs();
                    if xx < 1.0 {
                        sum += falloff_weight_preset(1.0 - xx, h, preset);
                    }
                }
                max = max.max(sum);
            }
            f * max
        };
        let d1 = deposit(0.05);
        let d2 = deposit(0.10);
        let d3 = deposit(0.30);
        for (a, b) in [(d1, d2), (d2, d3), (d1, d3)] {
            let ratio = a / b;
            assert!(
                (0.8..=1.25).contains(&ratio),
                "pass deposit must be ~spacing-independent: {d1} / {d2} / {d3}"
            );
        }
    }

    /// Tighter spacing → smaller per-dab factor; huge spacing → no attenuation.
    #[test]
    fn factor_monotone_and_bounded() {
        let f_dense = stroke_overlap_factor(0.05, 0.0, FalloffPreset::Smooth);
        let f_default = stroke_overlap_factor(0.10, 0.0, FalloffPreset::Smooth);
        let f_sparse = stroke_overlap_factor(0.50, 0.0, FalloffPreset::Smooth);
        assert!(f_dense < f_default && f_default < f_sparse, "{f_dense} {f_default} {f_sparse}");
        assert!(f_dense > 0.0 && f_sparse <= 1.0);
        assert_eq!(stroke_overlap_factor(2.5, 0.0, FalloffPreset::Smooth), 1.0);
    }
}
