//! Curvature for **polygonal** bodies: measure κ on the half-edge mesh, write it into the same
//! `res × res` `CurvGrid` the SDF path produces.
//!
//! Plan: `docs/planning/render/POLY_CURVATURE_PLAN.md` (P3 + P4). The measurement itself lives in
//! `core_math::mesh_curvature` (P1); this module is only the two **writers** that land it in the
//! bake domain, plus the frame-thread snapshot that feeds them.
//!
//! ## Why this produces a `CurvGrid` and nothing else
//!
//! `sdf_curvature_masks_ex` already tries `sdf.curv_caches` before it touches any field, keyed by
//! `CurvKey`. Producing a grid with the matching key therefore makes a poly body's Curvature layer
//! travel the *identical* downstream path as an SDF body's: same shaping, same compositor, same
//! map install, same Path Trace. Nothing in the evaluator, the node graph, the shaders or the save
//! format learns that poly curvature exists — which is why this feature needs no new render path
//! and no shader edit (FEATURE_RULES pre-flight Q4, plan §7).
//!
//! ## Two writers, chosen by how the map will actually be SAMPLED
//!
//! The measurement domain is the mesh; the storage domain must be whatever the fragment shader
//! reads, or the map lands somewhere the surface never looks. That is a real coupling and it is
//! resolved by reading `gbuffer.wgsl`, not by assumption:
//!
//! | Projection | Shader | This module writes |
//! |---|---|---|
//! | UV (`projection == 1`) | `sample_mat_uv` → `uv * tiling` | [`Writer::uv_raster`] — rasterise display-mesh triangles in UV space, κ interpolated |
//! | Box / triplanar (`0`) | `sample_mat_triplanar_b` → `rot(p.yz·t)`, `rot(p.xz·t)`, `rot(p.xy·t)` weighted by `|n|^blend` | [`Writer::triplanar_splat`] — splat per-vertex κ through those same three projections, with those same weights |
//!
//! **The triplanar writer deliberately does NOT copy `build_vertex_splat_curv_grid`'s projection.**
//! That function maps `u = (p[axis] − bmin) / ext`, i.e. AABB-normalised to 0..1, while the shader
//! samples raw *body-local coordinates times tiling* with repeat addressing. Those two agree only
//! for a body that happens to be a unit box at the origin. Copying it would have looked like reuse
//! and produced a map offset from where the surface reads it — plan P4's "verify, do not assume".
//! (The SDF path's own use of that formula is out of scope here and unchanged.)
//!
//! ## Threading
//!
//! [`PolyCurvSource`] is snapshotted on the **frame thread** (it needs `&HalfEdgeMesh` and
//! `&CustomData`, neither of which crosses to the bake worker) and moved by value into
//! `SdfFieldSources`. Everything below `build_grid` is pure CPU over flat buffers and runs on the
//! bake worker, where the rest of the material bake already runs — so there is no GPU dispatch, no
//! poll state machine, and nothing for the Frame Ordering Contract to govern.

use crate::gpu_curvature::{CurvGrid, CurvKey};
use crate::node_graph_bake::shape_curvature_channels;
use core_math::mesh_curvature::{
    average_edge_length_rings, smooth_iters_for_radius, vertex_curvature_rings, RingMesh,
};
use glam::{Vec2, Vec3};

/// Reinhard pre-scale applied to raw κ before shaping: `1/√3`, the κ of a **right-angle box
/// corner**, so that corner is the reference white point and everything gentler gets proportional
/// grey.
///
/// Two things this is deliberately NOT:
///
/// * **Not `CurvConstants::feature_display_scale()`** (`min_axis * 0.866`, what the SDF path uses).
///   That exists because a field Laplacian is in units of 1/length and needs a body-sized length to
///   become displayable. The mesh estimator is already dimensionless, so multiplying by body size
///   would make a 1000-unit body saturate everywhere — a size dependence the mesh path does not
///   have.
/// * **Not `1.0`**, which is what shipped first and was wrong. The shared shaping is
///   `smoothstep(clamp(curv · contrast · 4))` over `curv = x/(1+|x|)`, so at scale 1.0 it clamps at
///   **κ = 0.30** — a 90° edge (κ ≈ 0.5) and a 60° edge both read pure white, and anything under
///   κ = 0.05 reads black. The usable window was 0.05..0.30, i.e. a binary edge mask rather than
///   the broad grey gradient a curvature map is supposed to be. At `1/√3` the same sweep spans
///   0.12 → 0.37 → 0.63 → 0.84 → 1.0 across κ = 0.1 → 0.577.
const POLY_DISPLAY_SCALE: f32 = 0.577_350_3;

/// Texel bleed applied after the UV raster, as a fraction of resolution.
///
/// A UV bake that stops exactly at the island edge leaves the texels just outside it at zero, and
/// bilinear filtering then pulls that zero back across every seam as a dark rim. Every baker
/// dilates for this reason. 1.5 % of the map (4 texels at 256², 8 at 512²) covers the filter
/// footprint plus mip level 1 without bleeding one island into its neighbour.
const UV_DILATE_FRACTION: f32 = 0.015;

/// Scale on the normal-map derivative, calibrated so a hard crease in the normal map reads as
/// roughly the same κ as the equivalent hard edge in geometry.
///
/// A 90° crease swings `n.x` from −0.707 to +0.707 across it, so a one-texel derivative reads
/// ≈ 0.707; a geometric 90° edge measures κ ≈ 0.5 (see `mesh_curvature`'s cube gate). `0.7` puts
/// them within a few percent, which is what makes the two sources SUMMABLE — a mesh edge that also
/// carries a normal-map crease reinforces instead of one drowning the other.
const NORMAL_CURV_GAIN: f32 = 0.7;

/// How the map this grid feeds will be sampled — mirrored from the material's `TextureSpace` so
/// the writer fills the domain the fragment shader reads.
///
/// Field-for-field the subset of `sample_mat_triplanar_b` / `sample_mat_uv` inputs that affect
/// *where* a value lands. `offset` is deliberately absent: it has no `TexMap` field and is applied
/// per-layer in the subgraph, not by these samplers (the panel's own caption says so).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct PolySampleSpace {
    /// `MapProjection::as_u32()` — 0 = Box/triplanar, 1 = UV.
    pub projection: u32,
    /// `TextureSpace.tiling[0]`, the scalar the shader multiplies coordinates by.
    pub tiling: f32,
    /// `|n|^blend` triplanar weight exponent. Clamped exactly as the shader clamps it.
    pub blend: f32,
    /// Per-plane 90° rotations (X = YZ face, Y = XZ face, Z = XY face), matching `rot_pack`.
    pub rot: [u8; 3],
}

impl Default for PolySampleSpace {
    fn default() -> Self {
        Self {
            projection: 0,
            tiling: 1.0,
            blend: 4.0,
            rot: [0, 0, 0],
        }
    }
}

/// Frame-thread snapshot of one polygonal body's **post-modifier display mesh**.
///
/// Measuring the display mesh rather than the base mesh is not a preference — `poly_export_raster`
/// records that reading the base mesh silently dropped *"THE ENTIRE MODIFIER STACK"*, and a
/// curvature map that ignores the user's Bevel modifier is wrong in exactly that way.
#[derive(Clone, Debug, Default)]
pub(crate) struct PolyCurvSource {
    /// Flattened live face rings + positions (body-local, the bake domain).
    pub rings: RingMesh,
    /// Per-CORNER UV, parallel to `rings.face_ring`. Empty when the body has no authored UV layer.
    ///
    /// The pairing is the one `PolyMesh::raster_data_ngon_with_normals` documents and uses —
    /// *"a corner is the half-edge whose DESTINATION vertex is that corner"* — so corner `i` of a
    /// face carries `cd.uv[he_i]` and belongs to vertex `he_i.vertex`. Resolving it here, on the
    /// frame thread, against the same convention the renderer uses is what keeps the baked map and
    /// the drawn triangles aligned; a parallel array rebuilt on the worker could drift.
    pub corner_uv: Vec<Vec2>,
    pub space: PolySampleSpace,
    /// BAKED curvature mesh map (grey-centred RGBA8 + side), when `GeneratorMode` resolves to
    /// Baked. Present = do not measure; SAMPLE this instead. See `build_grid`.
    pub baked_curvature: Option<(u32, Vec<u8>)>,
    /// The material's tangent-space **normal map**, when it has one — the second curvature source
    /// (see [`Writer::add_normal_map_curvature`]). `None` means geometry is the only source, which
    /// on a low-poly body means the map can show its silhouette edges and nothing finer.
    pub normal_map: Option<NormalMapSrc>,
}

/// RGBA8 tangent-space normal map + the two facts needed to differentiate it correctly.
#[derive(Clone, Debug)]
pub(crate) struct NormalMapSrc {
    pub side: u32,
    pub px: Vec<u8>,
    /// `TexMap::green_dx` — DirectX green (Y down) rather than the OpenGL default (Y up).
    pub green_dx: bool,
}

impl PolyCurvSource {
    /// Nothing to measure — no live faces.
    pub fn is_empty(&self) -> bool {
        self.rings.face_ring.is_empty()
    }

    /// True when the UV raster writer is both possible and correct: the body has an authored UV
    /// layer AND the material actually samples by UV. A UV-unwrapped body set to Box projection
    /// is sampled triplanar-ly, so it must be BAKED triplanar-ly.
    pub fn uses_uv_writer(&self) -> bool {
        self.space.projection == 1 && self.corner_uv.len() == self.rings.face_ring.len()
    }

    /// Measure κ for this key's Radius and write it into a `res × res` grid.
    ///
    /// Bit-compatible with the SDF path's `CurvGrid`: same 4-wide `[mask, curv, convex, concave]`
    /// layout, same `region`, same key — so `sdf_curvature_masks_ex`'s existing fast path consumes
    /// it with no branch of its own.
    pub fn build_grid(&self, key: &CurvKey, res: u32) -> CurvGrid {
        let res = res.max(8);
        let n = (res * res) as usize;

        // BAKED FIRST — before the empty-geometry gate. Decoding a finished map needs the MAP,
        // not live rings: gating it behind `is_empty()` made the baked result depend on the very
        // measurement it exists to replace, so any body whose ring snapshot came up empty
        // returned black with a perfectly good map sitting in the slot.
        //
        // The map is grey-centred and auto-normalised (`mesh_map_bake::bake_curvature_pixels`),
        // so decoding is the inverse: `signed = v*2 - 1`. Radius/smoothing were baked in and are
        // deliberately NOT re-applied — re-smoothing a baked map would blur it twice.
        if let Some((side, px)) = self.baked_curvature.as_ref() {
            return curv_grid_from_baked(px, *side, key, res);
        }

        if self.is_empty() {
            return CurvGrid {
                res,
                data: vec![0.0; n * 4],
                region: key.region,
                key: *key,
            };
        }

        // Radius → smoothing iterations, through the mesh's own scale. `key.eps` is the panel
        // Radius socket verbatim (the same value the SDF path feeds `CurvConstants`).
        let avg_edge = average_edge_length_rings(&self.rings);
        let iters = smooth_iters_for_radius(avg_edge, key.eps);
        let kappa = vertex_curvature_rings(&self.rings, iters);

        // ── RAW κ first, shaping LAST ────────────────────────────────────────────────────────
        // The writer accumulates the raw signed measurement, not the four shaped channels, so the
        // two sources below can be SUMMED before `shape_curvature_channels` runs. Shaping each
        // source separately and combining afterwards would be wrong: the shaping is non-linear
        // (`x/(1+|x|)` then a clamped smoothstep), so `shape(a) + shape(b) != shape(a + b)` — two
        // half-strength features would each shape to near-black and their sum would stay near
        // black, when together they are one full-strength feature.
        let mut w = Writer::new(res);
        if self.uses_uv_writer() {
            self.uv_raster(&mut w, &kappa);
        } else {
            self.triplanar_raster(&mut w, &kappa);
        }
        w.finish_normalized();

        // ── Source 2: the NORMAL MAP (the Substance "per-pixel" curvature) ───────────────────
        // Geometric κ can only ever describe geometry the mesh HAS. On a game asset the surface
        // detail — cracks, dents, pores — lives in a baked normal map, and a low-poly mesh under
        // it is flat. Differentiating that map recovers exactly the band the mesh is missing.
        if let Some(nm) = self.normal_map.as_ref() {
            w.add_normal_map_curvature(nm, iters.max(1).min(16));
        }

        // Both writers leave gaps a bilinear fetch can straddle — the UV path at island borders,
        // the triplanar path wherever a tile edge cuts between triangles. Same remedy, same
        // bounded pass count.
        w.dilate(((res as f32 * UV_DILATE_FRACTION).round() as u32).clamp(1, 16));

        CurvGrid {
            res,
            data: w.into_shaped(key),
            region: key.region,
            key: *key,
        }
    }

    /// [`Self::build_grid`] plus the fraction of texels a writer actually WROTE.
    ///
    /// Coverage must be measured as "was this texel written", never as "is it non-zero": a
    /// curvature map is legitimately 0 across every flat face, so a non-zero count on a cube
    /// reports ~1 % and looks like catastrophic under-coverage when the map is in fact complete.
    /// The gates below assert on this number.
    #[cfg(test)]
    pub fn build_grid_probe(&self, key: &CurvKey, res: u32) -> (CurvGrid, f32) {
        let res = res.max(8);
        let mut w = Writer::new(res);
        if !self.is_empty() {
            let avg_edge = average_edge_length_rings(&self.rings);
            let iters = smooth_iters_for_radius(avg_edge, key.eps);
            let kappa = vertex_curvature_rings(&self.rings, iters);
            if self.uses_uv_writer() {
                self.uv_raster(&mut w, &kappa);
            } else {
                self.triplanar_raster(&mut w, &kappa);
            }
            w.finish_normalized();
            if let Some(nm) = self.normal_map.as_ref() {
                w.add_normal_map_curvature(nm, iters.max(1).min(16));
            }
            w.dilate(((res as f32 * UV_DILATE_FRACTION).round() as u32).clamp(1, 16));
        }
        let cov = w.coverage();
        let g = CurvGrid {
            res,
            data: w.into_shaped(key),
            region: key.region,
            key: *key,
        };
        (g, cov)
    }

    /// P3 — rasterise every display-mesh triangle into the map with UV as position.
    ///
    /// Triangulation goes through `earclip_polygon` on the 3D ring with the face normal — the SAME
    /// call `raster_data_ngon_with_normals` makes — so the triangles baked here are exactly the
    /// triangles drawn. Ear-clipping in UV space instead would disagree on any concave face and
    /// bake a subtly different surface than the one on screen.
    fn uv_raster(&self, w: &mut Writer, kappa: &[f32]) {
        let t = self.space.tiling;
        let mut pts: Vec<Vec3> = Vec::with_capacity(8);
        for fi in 0..self.rings.face_count() {
            let lo = self.rings.face_start[fi] as usize;
            let hi = self.rings.face_start[fi + 1] as usize;
            let ring = &self.rings.face_ring[lo..hi];
            if ring.len() < 3 {
                continue;
            }
            pts.clear();
            pts.extend(ring.iter().map(|&v| self.rings.positions[v as usize]));
            let normal = core_math::half_edge::newell_normal(&pts).normalize_or_zero();
            for tri in core_math::poly_mesh::earclip_polygon(&pts, normal) {
                let corner = |c: u32| -> (Vec2, f32) {
                    let local = c as usize;
                    let uv = self.corner_uv[lo + local] * t;
                    (uv, kappa[ring[local] as usize])
                };
                let (uv0, v0) = corner(tri[0]);
                let (uv1, v1) = corner(tri[1]);
                let (uv2, v2) = corner(tri[2]);
                w.raster_triangle(uv0, uv1, uv2, v0, v1, v2, 1.0);
            }
        }
    }

    /// P4 — normal-weighted box projection matching `sample_mat_triplanar_b` exactly.
    ///
    /// Each **triangle** is rasterised into all three planes at the shader's own coordinates
    /// (`p.yz·t`, `p.xz·t`, `p.xy·t`, each through the same 90° rotation) with the shader's own
    /// weights (`normalize(|n|^blend)` from the face normal), wrapped rather than clamped because
    /// the sampler uses repeat addressing.
    ///
    /// **Triangles, not vertices.** Point-splatting per vertex — the shape
    /// `build_vertex_splat_curv_grid` uses, because a field has no triangles to start from — wrote
    /// only a bilinear footprint per vertex and left the map mostly unwritten (measured 1.5 %
    /// covered on an 8×8×6 cube at 64²), which is the same structural under-coverage
    /// `CURVATURE_BOTH_MODES.md` spent three phases fighting. A poly body HAS triangles, so the
    /// coverage problem simply does not have to exist here: filling their interiors is both
    /// complete and more accurate than interpolating between splat footprints.
    fn triplanar_raster(&self, w: &mut Writer, kappa: &[f32]) {
        let t = self.space.tiling;
        let blend = self.space.blend.clamp(0.25, 32.0);
        let mut pts: Vec<Vec3> = Vec::with_capacity(8);
        for fi in 0..self.rings.face_count() {
            let lo = self.rings.face_start[fi] as usize;
            let hi = self.rings.face_start[fi + 1] as usize;
            let ring = &self.rings.face_ring[lo..hi];
            if ring.len() < 3 {
                continue;
            }
            pts.clear();
            pts.extend(ring.iter().map(|&v| self.rings.positions[v as usize]));
            let normal = core_math::half_edge::newell_normal(&pts).normalize_or_zero();
            if normal == Vec3::ZERO {
                continue;
            }
            // Shader weights, evaluated per face. The shader evaluates them per pixel from the
            // interpolated normal; within one flat face those agree exactly, and across a smooth
            // face they differ only as much as the face is curved.
            let a = normal.abs();
            let (wx, wy, wz) = (a.x.powf(blend), a.y.powf(blend), a.z.powf(blend));
            let sum = wx + wy + wz + 1.0e-4;
            let planes = [
                (wx / sum, 1usize, 2usize, self.space.rot[0]), // YZ
                (wy / sum, 0usize, 2usize, self.space.rot[1]), // XZ
                (wz / sum, 0usize, 1usize, self.space.rot[2]), // XY
            ];
            for tri in core_math::poly_mesh::earclip_polygon(&pts, normal) {
                for (weight, ui, vi, rot) in planes {
                    if weight < 1.0e-4 {
                        continue;
                    }
                    let corner = |c: u32| -> (Vec2, f32) {
                        let local = c as usize;
                        let p = pts[local];
                        let uv = rot_uv(Vec2::new(p[ui], p[vi]) * t, rot);
                        (uv, kappa[ring[local] as usize])
                    };
                    let (a0, v0) = corner(tri[0]);
                    let (a1, v1) = corner(tri[1]);
                    let (a2, v2) = corner(tri[2]);
                    w.raster_triangle(a0, a1, a2, v0, v1, v2, weight);
                }
            }
        }
    }
}

// ================================================================================================
// P5 — frame-thread snapshot + per-key grid preparation
// ================================================================================================

impl crate::AzraApp {
    /// Resolve a material's [`GeneratorMode`] against the body it is on.
    ///
    /// `Auto` means "Baked when there is something to sample, Live otherwise" — the only default
    /// that is right for both pillars without asking the user to know which one they are in.
    pub(crate) fn resolved_generator_mode(
        &self,
        mid: core_math::primitives::MaterialId,
    ) -> crate::material_panel::model::GeneratorMode {
        use crate::material_panel::model::GeneratorMode;
        let raw = self
            .ui
            .material_panel_docs
            .get(&mid.0)
            .map(|d| d.generator_mode)
            .unwrap_or(0);
        match GeneratorMode::from_u32(raw) {
            GeneratorMode::Auto => {
                let baked = self
                    .poly_curv_body()
                    .and_then(|b| {
                        self.baked_mesh_map(b, crate::mesh_map_bake::MeshMapKind::Curvature)
                    })
                    .is_some();
                if baked {
                    GeneratorMode::Baked
                } else {
                    GeneratorMode::Live
                }
            }
            m => m,
        }
    }

    /// Snapshot the selected poly body's post-modifier display mesh for curvature.
    ///
    /// `&mut self` because `poly_cached_display_mesh` may rebuild the display cache — this is the
    /// ONE choke point for "the mesh the user actually sees" (`mesh_sync.rs`), and going around it
    /// to the base mesh is the trap that silently dropped the whole modifier stack from exports.
    ///
    /// Returns `None` for an SDF body (which has a real field and needs none of this), for a body
    /// whose display mesh has no live faces, and when the material has no panel doc to read a
    /// texture space from.
    pub(crate) fn snapshot_poly_curv_source(
        &mut self,
        mid: core_math::primitives::MaterialId,
    ) -> Option<PolyCurvSource> {
        use core_math::half_edge::NO_EDGE;

        // These two bails used to be bare `?`, which is why "still nothing" was the literal
        // symptom: if either missed, the function returned before the diagnostic below and the
        // user got NO message at all — indistinguishable from the feature not existing.
        let Some(nid) = self.poly_curv_body() else {
            self.bake_note("Curvature: no polygonal body resolved — select the mesh (not the material) in the viewport or outliner");
            return None;
        };
        let Some(mesh) = self.poly_cached_display_mesh(nid) else {
            self.bake_note("Curvature: the body has no display mesh yet — its modifier stack has not evaluated");
            return None;
        };
        let rings = RingMesh::from_half_edge(&mesh);
        if rings.face_ring.is_empty() {
            self.bake_note("Curvature: the display mesh has no live faces");
            return None;
        }

        // Texture space decides WHICH writer is correct, so a missing doc is not a detail to
        // default past — without it we cannot know whether the map will be read by UV or
        // triplanar, and guessing wrong puts the curvature somewhere the surface never samples.
        // Texture space decides WHICH writer is correct, so it must be resolved, never bailed on.
        //
        // This used to be `material_panel_docs.get(&mid.0)?` — a hard fail that produced NO
        // curvature at all, silently, for any material without a panel doc. The panel doc is only
        // the authored intent; the thing the fragment shader actually samples is the material's
        // `TexMap` (`TextureSpace::apply_to_texmap` copies one onto the other), so that is the
        // correct fallback and the fallback can never be "give up".
        //
        // AUTO-SWITCH TO UV PROJECTION when the body has authored UVs and the material is still on
        // the untouched Box default (owner repro 2026-08-13: cube -> bevel -> UV Unwrap -> bake ->
        // add Curvature generator -> black). Both halves of the old behavior were individually
        // correct — the writer must bake in the space the material samples (uses_uv_writer), and a
        // fresh material defaults to Box — but together they made the DEFAULT path the broken one:
        // the user authored UVs and the map silently refused to use them, with the remedy buried
        // in a bake-note. Authoring UVs is an explicit statement of intent; acting on it here is a
        // VISIBLE state change (the doc's Texture Space panel now shows UV, and a toast says so),
        // not two paths quietly disagreeing. A user who explicitly wants Box can set it back —
        // the switch only fires when projection is Box AND authored UVs fully cover the mesh.
        {
            // Authored-UV probe: the same sidecar layer the corner_uv builder below reads
            // (`poly_custom_data[nid].uv`, HalfEdgeId-keyed) — non-empty means the user ran
            // UV Unwrap on this body.
            let has_full_uv = self
                .poly_curv_body()
                .and_then(|b| self.cache.poly_custom_data.get(&b))
                .map_or(false, |cd| !cd.uv.is_empty());
            if has_full_uv {
                if let Some(doc) = self.ui.material_panel_docs.get_mut(&mid.0) {
                    if doc.texture_space.projection == 0 {
                        doc.texture_space.projection = 1;
                        self.toast(
                            "Texture Space set to UV — authored UVs found (was Box/triplanar)",
                        );
                    }
                }
            }
        }
        let space = {
            let from_doc = self
                .ui
                .material_panel_docs
                .get(&mid.0)
                .map(|d| d.texture_space)
                .map(|ts| PolySampleSpace {
                    projection: ts.projection,
                    tiling: if ts.tiling[0].abs() > 1.0e-6 { ts.tiling[0] } else { 1.0 },
                    blend: ts.blend,
                    rot: [ts.rot_x, ts.rot_y, ts.rot_z],
                });
            from_doc
                .or_else(|| {
                    let (_, mat) = self.scene.scene_graph.material_by_id(mid)?;
                    let tm = mat.albedo_map;
                    Some(PolySampleSpace {
                        projection: tm.projection,
                        tiling: if tm.uv_scale.abs() > 1.0e-6 { tm.uv_scale } else { 1.0 },
                        blend: tm.blend,
                        rot: [0, 0, 0],
                    })
                })
                .unwrap_or_default()
        };

        // Per-corner UV, resolved against the emitter's documented association: a corner is the
        // half-edge whose DESTINATION vertex is that corner, and its UV is `cd.uv[he]`. Walking
        // `face_half_edges` in the same order `face_vertices` (which `RingMesh` used) walks makes
        // corner `k` of face `f` line up with `rings.face_ring[start[f] + k]` by construction.
        let corner_uv = self
            .cache
            .poly_custom_data
            .get(&nid)
            .filter(|cd| !cd.uv.is_empty())
            .map(|cd| {
                let mut out = Vec::with_capacity(rings.face_ring.len());
                let mut hes = Vec::with_capacity(8);
                for fi in 0..rings.face_count() {
                    let lo = rings.face_start[fi] as usize;
                    let hi = rings.face_start[fi + 1] as usize;
                    if hi == lo {
                        continue; // free / degenerate face — RingMesh skipped it too
                    }
                    let f = core_math::half_edge::FaceId(fi as u32);
                    if mesh.face(f).half_edge == NO_EDGE {
                        continue;
                    }
                    mesh.face_half_edges_into(f, &mut hes);
                    debug_assert_eq!(
                        hes.len(),
                        hi - lo,
                        "face_half_edges and face_vertices must walk the same ring"
                    );
                    for &he in &hes {
                        out.push(cd.uv.get(he).copied().unwrap_or(Vec2::ZERO));
                    }
                }
                out
            })
            .unwrap_or_default();

        // BAKED MODE: pull the already-baked curvature map instead of measuring the mesh.
        //
        // This is the performance difference the mode exists for: sampling a finished map is one
        // fetch per texel, where Live rebuilds a res^2 curvature grid for every distinct knob set
        // on every bake.
        let baked_curvature = if matches!(
            self.resolved_generator_mode(mid),
            crate::material_panel::model::GeneratorMode::Baked
        ) {
            self.baked_mesh_map(nid, crate::mesh_map_bake::MeshMapKind::Curvature)
                .and_then(|slot| {
                    let px = self.gpu.as_ref()?.material_textures.slot_rgba(slot)?;
                    let side = crate::material_textures::map_side_len(px)?;
                    Some((side, px.to_vec()))
                })
        } else {
            None
        };

        // The material's baked normal map, if any — the high-frequency curvature source. Read
        // from the SAME GPU slot the renderer samples (`material_textures::slot_rgba`), so the
        // curvature is differentiated from exactly the pixels the surface actually shows.
        let normal_map = (|| {
            let (_, mat) = self.scene.scene_graph.material_by_id(mid)?;
            let tex_id = mat.normal_map.tex_id;
            let green_dx = mat.normal_map.green_dx;
            let px = self.gpu.as_ref()?.material_textures.slot_rgba(tex_id)?;
            let side = crate::material_textures::map_side_len(px)?;
            Some(NormalMapSrc {
                side,
                px: px.to_vec(),
                green_dx,
            })
        })();

        let src = PolyCurvSource {
            rings,
            corner_uv,
            space,
            baked_curvature,
            normal_map,
        };

        // ── WHY IS IT BLACK? ────────────────────────────────────────────────────────────────
        // Four separate preconditions must hold before a single texel is written, and every one
        // of them failed SILENTLY: the layer simply baked black and nothing said which. Report
        // the resolved state once per distinct outcome (deduped — this runs on every live bake,
        // so an undeduped toast would be a stream).
        let diag = format!(
            "Curvature source: {} faces · {} UV corners · projection {} · normal map {}",
            src.rings.face_count(),
            src.corner_uv.len(),
            if src.space.projection == 1 { "UV" } else { "Box" },
            if src.normal_map.is_some() { "yes" } else { "no" },
        );
        let hint = if src.corner_uv.is_empty() {
            Some("no authored UVs — run Poly ▸ UV Unwrap")
        } else if src.corner_uv.len() != src.rings.face_ring.len() {
            // Should be impossible (both walks visit the same ring); if it ever fires, the UV
            // path is skipped and the cause must be visible rather than inferred from a black map.
            Some("UV corner count does not match the mesh — UV path skipped")
        } else if src.space.projection != 1 {
            Some("Projection is Box — set Texture Space ▸ Projection: UV")
        } else {
            None
        };
        let msg = match hint {
            Some(h) => format!("{diag} — {h}"),
            None => diag,
        };
        if hint.is_some() {
            self.bake_note(&msg);
        } else {
            // Healthy: record WITHOUT toasting, so a later failure still reads as a change.
            self.ui.last_bake_note = Some(msg);
        }

        // FAIL CLOSED, LOUDLY, when the map cannot be unique per surface point.
        //
        // A curvature map is not a tiling texture: each texel must correspond to ONE place on the
        // surface. `sample_mat_triplanar_b` fetches at `p.xy * tiling` with REPEAT addressing, so
        // a body larger than one tile reads the same texel at many different surface points, and
        // the bake writes many different surface points into the same texel. The result is a
        // meaningless average — which is exactly the "noise" this looked like, and it is a false
        // result rather than a missing one, so it must not ship silently (CLAUDE.md § No Hacks: a
        // documented guard that fails closed with a clear message is allowed; a plausible-looking
        // wrong map is not).
        //
        // BAKED MODE IS EXEMPT (owner 2026-08-15: "curvature mask and generator still do not work
        // with UV bake … in Substance all smart materials work with the curvature that is baked").
        // This guard is about the LIVE TRIPLANAR WRITER — it cannot hold a unique value per
        // surface point on a body larger than one tile. Baked mode does not use that writer at
        // all: `build_grid` returns `curv_grid_from_baked` before any writer runs, and the map is
        // sampled through the material's own UVs. Refusing here made a SUCCESSFUL bake unusable —
        // the bake path (`mesh_map_bake::mesh_map_source`) has no such guard, so the map existed
        // and the consumer threw it away. Two source builders, one guard, opposite answers: the
        // two-consumer trap this file has hit before.
        if src.baked_curvature.is_none() && !src.uses_uv_writer() {
            let (lo, hi) = src.rings.aabb();
            let ext = (hi - lo) * src.space.tiling.abs().max(1.0e-6);
            let span = ext.x.max(ext.y).max(ext.z);
            if span > 1.001 {
                self.toast(
                    "Curvature needs UV projection — Box (triplanar) tiles the map, so it cannot \
                     hold a unique value per surface point. Run Poly ▸ UV Unwrap, then set \
                     Texture Space ▸ Projection: UV.",
                );
                return None;
            }
        }

        Some(src)
    }
}

/// Ensure both curvature caches hold a poly-measured grid for every curvature key in `graph`.
///
/// Mirrors `prepare_vertex_splat_caches`' contract (idempotent, res-keyed, drops stale same-key
/// entries) but measures the mesh instead of a field.
///
/// **Both caches are filled with the same grid, deliberately.** The Texture/Vertex rung selects
/// which list a node reads (`curv_caches` vs `vertex_curv_caches`, via the trailing `v` on the
/// panel tag), and the panel picks the rung from UV presence. For a poly body the rung does not
/// change the measurement at all — κ is per-vertex either way — so a layer must produce the same
/// result whichever rung it happens to carry. Filling one list only would make the layer render
/// black for exactly the bodies whose UV state disagreed with the rung the doc was saved with.
pub(crate) fn prepare_poly_curv_caches(
    graph: &crate::shader_nodes::NodeGraph,
    sdf: &mut crate::node_graph_bake::SdfFieldSources,
    res: u32,
    notes: &mut crate::bake_diag::BakeNotes,
) {
    let Some(src) = sdf.poly_curv.clone() else {
        return; // not a poly body — the SDF path handles it
    };
    if src.is_empty() {
        notes.push("Curvature: the poly body snapshot has no faces");
        return;
    }
    let Some(keys) = crate::node_graph_bake::curvature_keys_for_graph(graph) else {
        // Ineligible graph (wired knob socket / non-unit Cavity scale / too many distinct knob
        // sets). This used to `return` silently, so an ineligible graph and a graph with no
        // curvature in it were indistinguishable — a black map with no explanation, which is
        // exactly the class `bake_diag` exists to end.
        notes.push(
            "Curvature: this graph cannot be precomputed (a Radius/Contrast socket is wired, or \
             there are too many distinct settings) — the layer contributes nothing",
        );
        return;
    };
    if keys.is_empty() {
        notes.push("Curvature: no curvature node found in the material graph");
        return;
    }
    for key in keys {
        let fresh = || src.build_grid(&key, res);
        for cache in [&mut sdf.curv_caches, &mut sdf.vertex_curv_caches] {
            if cache
                .iter()
                .any(|g| g.key.matches(&key) && g.res == res && !g.data.is_empty())
            {
                continue;
            }
            cache.retain(|g| !(g.key.matches(&key) && g.res != res));
            cache.push(fresh());
        }
    }
}

/// Decode a baked, grey-centred curvature map into a `CurvGrid`.
///
/// Point-sampled to `res` and run through the SAME `shape_curvature_channels` the measured path
/// uses, so Live and Baked produce the same four channels with the same meaning — switching mode
/// must change the COST, not the look.
fn curv_grid_from_baked(px: &[u8], side: u32, key: &CurvKey, res: u32) -> CurvGrid {
    let res = res.max(8);
    let n = (res * res) as usize;
    let mut data = vec![0.0f32; n * 4];
    if side < 1 {
        return CurvGrid { res, data, region: key.region, key: *key };
    }
    for y in 0..res {
        for x in 0..res {
            let sx = (x * side / res).min(side - 1);
            let sy = (y * side / res).min(side - 1);
            let si = ((sy * side + sx) * 4) as usize;
            if si + 3 >= px.len() {
                continue;
            }
            // Inverse of the bake encode: 0.5 = flat.
            let signed = px[si] as f32 / 127.5 - 1.0;
            let (m, c, cx, cc) = shape_curvature_channels(
                signed,
                1.0, // already normalised at bake time — do not re-scale
                key.region,
                key.contrast,
                key.strength,
            );
            let i = ((y * res + x) * 4) as usize;
            data[i] = m;
            data[i + 1] = c;
            data[i + 2] = cx;
            data[i + 3] = cc;
        }
    }
    CurvGrid { res, data, region: key.region, key: *key }
}

/// `triplanar_rot_uv` from `gbuffer.wgsl`, verbatim — rotate UV by `q` quarters.
///
/// Duplicated deliberately across the WGSL/Rust boundary (there is no shared language here); the
/// quarter cases are transcribed one-for-one so a change to either is visibly a change to both.
fn rot_uv(uv: Vec2, q: u8) -> Vec2 {
    match q & 3 {
        1 => Vec2::new(-uv.y, uv.x),
        2 => Vec2::new(-uv.x, -uv.y),
        3 => Vec2::new(uv.y, -uv.x),
        _ => uv,
    }
}

/// Accumulator over the `res × res` bake domain: one weighted **raw κ** per texel, plus a weight.
///
/// Scalar on purpose — see `build_grid` on why shaping must happen once, at the end, after every
/// source has been summed.
struct Writer {
    res: u32,
    acc: Vec<f32>,
    wsum: Vec<f32>,
}

impl Writer {
    fn new(res: u32) -> Self {
        let n = (res * res) as usize;
        Self {
            res,
            acc: vec![0.0; n],
            wsum: vec![0.0; n],
        }
    }

    #[inline]
    fn add(&mut self, x: u32, y: u32, weight: f32, v: f32) {
        let i = (y * self.res + x) as usize;
        self.wsum[i] += weight;
        self.acc[i] += v * weight;
    }

    /// Fill one triangle given in **texture** coordinates (already tiled), interpolating the four
    /// channels barycentrically and accumulating at `weight` (1.0 for the UV writer; the
    /// triplanar plane weight otherwise, so overlapping planes blend exactly as the shader does).
    ///
    /// Coverage is conservative: the scan box is expanded by one texel and a texel counts as
    /// covered when its centre is inside the triangle *or* when the triangle is thinner than a
    /// texel (`edge_eps`), so a sliver never drops out entirely and leaves a hole the dilate pass
    /// would have to invent a value for.
    fn raster_triangle(
        &mut self,
        a: Vec2,
        b: Vec2,
        c: Vec2,
        va: f32,
        vb: f32,
        vc: f32,
        weight: f32,
    ) {
        let r = self.res as f32;
        // Texel-space corners. Wrapping happens per-texel below so a triangle crossing the tile
        // boundary still lands (repeat addressing again).
        let pa = Vec2::new(a.x * r - 0.5, a.y * r - 0.5);
        let pb = Vec2::new(b.x * r - 0.5, b.y * r - 0.5);
        let pc = Vec2::new(c.x * r - 0.5, c.y * r - 0.5);
        let area = (pb.x - pa.x) * (pc.y - pa.y) - (pb.y - pa.y) * (pc.x - pa.x);
        if area.abs() < 1.0e-12 {
            return; // degenerate in UV (zero-area island) — nothing to fill
        }
        let min_x = pa.x.min(pb.x).min(pc.x).floor() as i64 - 1;
        let max_x = pa.x.max(pb.x).max(pc.x).ceil() as i64 + 1;
        let min_y = pa.y.min(pb.y).min(pc.y).floor() as i64 - 1;
        let max_y = pa.y.max(pb.y).max(pc.y).ceil() as i64 + 1;
        // A pathological UV layout (huge tiling, broken unwrap) must not turn into an unbounded
        // scan. One full map's worth of texels per triangle is already far beyond any sane island.
        let span = (max_x - min_x).max(max_y - min_y);
        if span > self.res as i64 * 4 {
            return;
        }
        let inv_area = 1.0 / area;
        // Barycentric coordinates of a point, and whether it is inside.
        //
        // The tolerance is a FIXED epsilon in barycentric units. It used to be
        // `1e-3 * max(|area|,1) * (1/|area|)`, which for a sub-texel triangle (`|area| < 1`)
        // evaluated to `1e-3 / |area|` — an unbounded tolerance that accepted samples far outside
        // the triangle. Combined with the ±1 texel scan box, every sub-texel triangle flooded a
        // 3×3 block with its own κ; on a dense mesh those blocks overlap and average into a
        // mottled field. That was the "noise": an aliasing artefact of the rasteriser, not the
        // measurement (the kernel's own gates were all passing).
        let bary = |p: Vec2| -> (f32, f32, f32) {
            let w0 = ((pb.x - p.x) * (pc.y - p.y) - (pb.y - p.y) * (pc.x - p.x)) * inv_area;
            let w1 = ((pc.x - p.x) * (pa.y - p.y) - (pc.y - p.y) * (pa.x - p.x)) * inv_area;
            (w0, w1, 1.0 - w0 - w1)
        };
        const TOL: f32 = 1.0e-6;
        let interp = |w: (f32, f32, f32)| -> f32 { va * w.0 + vb * w.1 + vc * w.2 };

        // SUPERSAMPLED coverage. `SS × SS` samples per texel; a texel accumulates with weight
        // proportional to the fraction of it the triangle actually covers, and the interpolated
        // value is averaged over the covered samples only. `finish_normalized`'s divide then makes
        // each texel a true coverage-weighted average of every triangle touching it — which is
        // what a supersampled bake produces, and what removes the aliasing above.
        const SS: i32 = 4;
        let step = 1.0 / SS as f32;
        let mut wrote_any = false;
        for py in min_y..=max_y {
            for px in min_x..=max_x {
                let mut acc = 0.0f32;
                let mut hits = 0i32;
                for sy in 0..SS {
                    for sx in 0..SS {
                        let p = Vec2::new(
                            px as f32 - 0.5 + (sx as f32 + 0.5) * step,
                            py as f32 - 0.5 + (sy as f32 + 0.5) * step,
                        );
                        let w = bary(p);
                        if w.0 < -TOL || w.1 < -TOL || w.2 < -TOL {
                            continue;
                        }
                        acc += interp(w);
                        hits += 1;
                    }
                }
                if hits == 0 {
                    continue;
                }
                let vals = acc / hits as f32;
                let cov = hits as f32 / (SS * SS) as f32;
                let x = (px.rem_euclid(self.res as i64)) as u32;
                let y = (py.rem_euclid(self.res as i64)) as u32;
                self.add(x, y, weight * cov, vals);
                wrote_any = true;
            }
        }

        // A triangle smaller than one subsample cell can miss every sample and vanish. Give it the
        // single texel containing its centroid, weighted by its true area so it contributes
        // proportionally instead of flooding a block — the old conservative path's mistake.
        if !wrote_any {
            let c = (pa + pb + pc) / 3.0;
            let x = ((c.x.round() as i64).rem_euclid(self.res as i64)) as u32;
            let y = ((c.y.round() as i64).rem_euclid(self.res as i64)) as u32;
            let area_cov = (area.abs() * 0.5).min(1.0);
            let vals = (va + vb + vc) / 3.0;
            self.add(x, y, weight * area_cov.max(1.0e-4), vals);
        }
    }

    /// Divide out the accumulated weights. Texels with no contribution stay exactly 0 so
    /// [`Self::dilate`] can tell "no data" from "measured zero" — a flat face legitimately reads
    /// κ = 0, and averaging it into empty space would erase real coverage information.
    fn finish_normalized(&mut self) {
        for i in 0..self.wsum.len() {
            if self.wsum[i] > 1.0e-8 {
                self.acc[i] /= self.wsum[i];
            }
        }
    }

    /// Bleed covered texels outward `passes` times, so bilinear filtering across a UV seam reads
    /// surface data instead of background.
    fn dilate(&mut self, passes: u32) {
        let res = self.res as i64;
        for _ in 0..passes {
            let src = self.acc.clone();
            let covered: Vec<bool> = self.wsum.iter().map(|w| *w > 1.0e-8).collect();
            let mut grew = false;
            for y in 0..res {
                for x in 0..res {
                    let i = (y * res + x) as usize;
                    if covered[i] {
                        continue;
                    }
                    let mut sum = 0.0f32;
                    let mut n = 0.0f32;
                    for dy in -1i64..=1 {
                        for dx in -1i64..=1 {
                            let nx = (x + dx).rem_euclid(res);
                            let ny = (y + dy).rem_euclid(res);
                            let j = (ny * res + nx) as usize;
                            if !covered[j] {
                                continue;
                            }
                            sum += src[j];
                            n += 1.0;
                        }
                    }
                    if n > 0.0 {
                        self.acc[i] = sum / n;
                        // Mark covered for the NEXT pass only — writing it now would let this
                        // pass cascade across the whole map in one sweep.
                        self.wsum[i] = 1.0e-6;
                        grew = true;
                    }
                }
            }
            if !grew {
                break; // fully covered (or nothing to grow from) — further passes are no-ops
            }
        }
    }

    /// Add per-pixel curvature differentiated from a tangent-space **normal map** — the source
    /// Substance Painter's per-pixel curvature algorithm uses, and the only one that can produce
    /// surface detail (cracks, dents, pores) on a low-poly mesh whose geometry is flat.
    ///
    /// ## The formula, and why the sign works out
    ///
    /// For a surface described by a height field `h(u,v)`, the tangent-space normal is
    /// `n ∝ (−∂h/∂u, −∂h/∂v, 1)`. So `n.x = −h_u`, and
    ///
    /// ```text
    /// ∂n.x/∂u + ∂n.y/∂v = −(h_uu + h_vv)
    /// ```
    ///
    /// which is positive exactly where `h` is concave-down, i.e. on a **convex** ridge. That is
    /// already this module's convention (convex positive), so it sums directly onto the geometric
    /// term with no sign correction — and a bump in the normal map reinforces a convex mesh edge
    /// rather than cancelling it.
    ///
    /// ## Why this needs no UV transform
    ///
    /// The normal map and this grid are the **same texture domain**: whatever coordinates the
    /// shader uses to fetch the normal map (`uv * tiling`, or the three triplanar projections) are
    /// the coordinates it uses to fetch the curvature map. A derivative taken in texel space
    /// therefore lands on exactly the surface points the detail came from, for both writers, with
    /// no projection maths at all. Where the two maps differ in resolution, the normal map is
    /// point-sampled at this grid's rate.
    ///
    /// `step` is the derivative half-width in texels, driven by the panel Radius — the standard
    /// "curvature radius" control on a normal-map-derived curvature (larger = wider, softer bands).
    ///
    /// Only texels already covered by geometry are touched: outside the UV islands there is no
    /// surface, and inventing curvature in the gutter is what [`Self::dilate`] is for.
    fn add_normal_map_curvature(&mut self, nm: &NormalMapSrc, step: u32) {
        if nm.side < 3 {
            return;
        }
        let side = nm.side as i64;
        let res = self.res as i64;
        // Green convention: GL (default) has +Y pointing up in tangent space; DirectX flips it.
        // Reading the wrong one negates only the v-derivative, which shows up as curvature that
        // is correct along u and inverted along v — plausible-looking directional banding rather
        // than an obvious failure, so it is worth honouring rather than assuming.
        let gy = if nm.green_dx { -1.0f32 } else { 1.0f32 };
        // Normal map texel for a grid texel (point sample; the maps usually share a resolution).
        let at = |gx: i64, gyi: i64| -> (f32, f32) {
            let sx = (gx * side / res).rem_euclid(side);
            let sy = (gyi * side / res).rem_euclid(side);
            let i = ((sy * side + sx) * 4) as usize;
            (
                nm.px[i] as f32 / 127.5 - 1.0,
                (nm.px[i + 1] as f32 / 127.5 - 1.0) * gy,
            )
        };
        let r = step.max(1) as i64;
        let inv = 1.0 / (2.0 * r as f32);
        for y in 0..res {
            for x in 0..res {
                let i = (y * res + x) as usize;
                if self.wsum[i] <= 1.0e-8 {
                    continue;
                }
                let dnx = at(x + r, y).0 - at(x - r, y).0;
                let dny = at(x, y + r).1 - at(x, y - r).1;
                self.acc[i] += (dnx + dny) * inv * NORMAL_CURV_GAIN;
            }
        }
    }

    /// Shape the accumulated raw κ into the 4-wide `[mask, curv, convex, concave]` layout the
    /// reader expects — ONCE, after every source has been summed (see `build_grid`).
    fn into_shaped(self, key: &CurvKey) -> Vec<f32> {
        let n = (self.res * self.res) as usize;
        let mut out = vec![0.0f32; n * 4];
        for i in 0..n {
            let (mask, curv, conv, conc) = shape_curvature_channels(
                self.acc[i],
                POLY_DISPLAY_SCALE,
                key.region,
                key.contrast,
                key.strength,
            );
            out[i * 4] = mask;
            out[i * 4 + 1] = curv;
            out[i * 4 + 2] = conv;
            out[i * 4 + 3] = conc;
        }
        out
    }

    /// Fraction of texels carrying real data. The gate form `CURVATURE_BOTH_MODES.md` mandates:
    /// coverage, never mere difference.
    #[cfg(test)]
    fn coverage(&self) -> f32 {
        let c = self.wsum.iter().filter(|w| **w > 1.0e-8).count();
        c as f32 / self.wsum.len() as f32
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core_math::half_edge::HalfEdgeMesh;

    fn key(region: u32, radius: f32) -> CurvKey {
        CurvKey {
            eps: radius,
            prim_uid: 0,
            region,
            grow: 0.0,
            band: 0.2,
            contrast: 1.0,
            strength: 1.0,
        }
    }

    /// Unit cube subdivided `n × n` per face, with a **box unwrap**: each of the 6 faces gets its
    /// own island in a 3 × 2 atlas, so UVs are non-overlapping and every texel maps to one surface
    /// point — the layout a real unwrap produces and the one the coverage gate needs.
    fn unwrapped_cube(n: usize) -> PolyCurvSource {
        let mut pos: Vec<Vec3> = Vec::new();
        let mut faces: Vec<Vec<usize>> = Vec::new();
        let mut face_uv: Vec<[Vec2; 4]> = Vec::new();
        let mut index = std::collections::HashMap::<(i64, i64, i64), usize>::new();
        let q = |x: f32| (x * 1.0e4).round() as i64;
        let sides = [
            (Vec3::new(-0.5, -0.5, 0.5), Vec3::X, Vec3::Y),
            (Vec3::new(0.5, -0.5, -0.5), -Vec3::X, Vec3::Y),
            (Vec3::new(0.5, -0.5, 0.5), -Vec3::Z, Vec3::Y),
            (Vec3::new(-0.5, -0.5, -0.5), Vec3::Z, Vec3::Y),
            (Vec3::new(-0.5, 0.5, 0.5), Vec3::X, -Vec3::Z),
            (Vec3::new(-0.5, -0.5, -0.5), Vec3::X, Vec3::Z),
        ];
        let s = 1.0 / n as f32;
        for (si, (o, du, dv)) in sides.iter().enumerate() {
            // Island origin in a 3x2 atlas, inset so islands do not touch.
            let iu = (si % 3) as f32 / 3.0;
            let iv = (si / 3) as f32 / 2.0;
            let (pad, sc) = (0.01, 1.0 / 3.0 - 0.02);
            let (pad_v, sc_v) = (0.01, 0.5 - 0.02);
            for i in 0..n {
                for j in 0..n {
                    let corners = [(0, 0), (1, 0), (1, 1), (0, 1)];
                    let mut ring = Vec::with_capacity(4);
                    let mut uvs = [Vec2::ZERO; 4];
                    for (k, (ci, cj)) in corners.iter().enumerate() {
                        let p = *o
                            + *du * ((i + ci) as f32 * s)
                            + *dv * ((j + cj) as f32 * s);
                        let kq = (q(p.x), q(p.y), q(p.z));
                        let id = *index.entry(kq).or_insert_with(|| {
                            pos.push(p);
                            pos.len() - 1
                        });
                        ring.push(id);
                        uvs[k] = Vec2::new(
                            iu + pad + ((i + ci) as f32 / n as f32) * sc,
                            iv + pad_v + ((j + cj) as f32 / n as f32) * sc_v,
                        );
                    }
                    faces.push(ring);
                    face_uv.push(uvs);
                }
            }
        }
        let mesh = HalfEdgeMesh::from_polygons(&pos, &faces);
        let rings = RingMesh::from_half_edge(&mesh);

        // Rebuild per-corner UVs against the RingMesh ordering. `from_polygons` may reorder a
        // face's ring relative to the input list, so match each emitted corner back to its
        // authored UV by vertex id rather than assuming index alignment.
        let mut corner_uv = vec![Vec2::ZERO; rings.face_ring.len()];
        for fi in 0..rings.face_count() {
            let lo = rings.face_start[fi] as usize;
            let hi = rings.face_start[fi + 1] as usize;
            if hi == lo {
                continue;
            }
            let ring = &rings.face_ring[lo..hi];
            // Find the authored face with the same vertex set.
            let src = faces
                .iter()
                .position(|f| {
                    f.len() == ring.len() && ring.iter().all(|v| f.contains(&(*v as usize)))
                })
                .expect("every emitted face came from an authored face");
            for (k, &v) in ring.iter().enumerate() {
                let at = faces[src].iter().position(|x| *x == v as usize).unwrap();
                corner_uv[lo + k] = face_uv[src][at];
            }
        }

        PolyCurvSource {
            rings,
            corner_uv,
            space: PolySampleSpace {
                projection: 1, // UV
                tiling: 1.0,
                blend: 4.0,
                rot: [0, 0, 0],
            },
            baked_curvature: None,
            normal_map: None,
        }
    }

    /// GATE (P3) — the UV writer COVERS the map and puts the cube's edges in it.
    ///
    /// Coverage, not difference: a writer that filled 20 % of the map would still "differ from
    /// black". The unwrap here occupies ~94 % of the atlas by construction, so anything under
    /// 0.8 means islands or triangles are being dropped.
    #[test]
    fn p3_uv_raster_covers_the_map_and_finds_the_edges() {
        let src = unwrapped_cube(6);
        assert!(src.uses_uv_writer(), "a UV-projected body with authored UVs takes the UV path");
        let (g, cov) = src.build_grid_probe(&key(1, 0.0), 128);
        assert!(
            cov > 0.8,
            "UV raster must fill the atlas — WRITTEN coverage, not non-zero coverage (got {cov})"
        );

        // Convex channel: the 12 edges must be present and the flat interiors must not be.
        let convex: Vec<f32> = (0..(g.res * g.res) as usize).map(|i| g.data[i * 4 + 2]).collect();
        let hot = convex.iter().filter(|v| **v > 0.5).count();
        let cold = convex.iter().filter(|v| **v < 0.05).count();
        assert!(hot > 0, "the cube's edges must appear in the convex channel");
        assert!(
            cold > hot,
            "flat interiors dominate a cube's surface area, so cold texels must outnumber hot \
             ({cold} cold vs {hot} hot) — otherwise the whole map saturated"
        );
    }

    /// GATE (P4) — the triplanar writer covers the map WITHOUT UVs, and is not merely the UV
    /// path in disguise.
    #[test]
    fn p4_triplanar_splat_covers_without_uvs() {
        let mut src = unwrapped_cube(8);
        src.corner_uv.clear();
        src.space.projection = 0; // Box
        assert!(!src.uses_uv_writer(), "no UVs → triplanar writer");
        let (g, cov) = src.build_grid_probe(&key(1, 0.0), 64);
        assert!(
            cov > 0.8,
            "a unit cube spans the full 0..1 triplanar tile, so the splat must cover most of the \
             map (coverage {cov})"
        );
        assert!(
            g.data.iter().all(|v| v.is_finite()),
            "no NaN/Inf may reach the compositor"
        );
    }

    /// GATE (P3/P4) — the grid is bit-compatible with what the reader expects.
    #[test]
    fn p3_grid_shape_matches_the_sdf_contract() {
        let src = unwrapped_cube(4);
        let k = key(2, 0.05);
        let g = src.build_grid(&k, 32);
        assert_eq!(g.res, 32);
        assert_eq!(g.data.len(), 32 * 32 * 4, "4-wide [mask, curv, convex, concave]");
        assert_eq!(g.region, k.region);
        assert!(g.key.matches(&k), "the grid must answer to the key it was built for");
    }

    /// GATE (P3) — Radius widens the band in the BAKED map, not just in the kernel.
    ///
    /// The M3 gate, re-run one level up: it is possible to have a correct kernel and still lose
    /// the effect at the writer (e.g. by clamping before shaping).
    #[test]
    fn p3_radius_widens_the_baked_band() {
        let src = unwrapped_cube(8);
        // Threshold RELATIVE to each map's own peak, matching `mesh_curvature`'s M3 gate.
        //
        // An absolute threshold measures the wrong thing: smoothing conserves total κ while
        // lowering the peak, so a wider band can have FEWER texels over a fixed level even though
        // it reaches further. (An absolute 0.25 passed only while `POLY_DISPLAY_SCALE = 1.0` made
        // the shaping clip, i.e. it was reading the saturation bug as success.)
        let band = |radius: f32| -> usize {
            let g = src.build_grid(&key(1, radius), 128);
            let conv: Vec<f32> = (0..(g.res * g.res) as usize)
                .map(|i| g.data[i * 4 + 2])
                .collect();
            let peak = conv.iter().cloned().fold(0.0f32, f32::max);
            assert!(peak > 0.0, "radius {radius} produced an empty map");
            conv.iter().filter(|v| **v > peak * 0.15).count()
        };
        let tight = band(0.0);
        let wide = band(0.5);
        assert!(
            wide > tight,
            "a larger Radius must reach more texels ({wide} vs {tight})"
        );
    }

    /// GATE (P4) — tiling is honoured, i.e. the writer really is filling the sampler's domain.
    ///
    /// At tiling 2 the cube's body-local span covers the tile twice, so features repeat and the
    /// map's content changes. If the writer ignored `tiling` (or normalised by the AABB like the
    /// SDF splat does), both grids would be identical — which is the bug this pins.
    #[test]
    fn p4_triplanar_respects_tiling() {
        let mut a = unwrapped_cube(8);
        a.corner_uv.clear();
        a.space.projection = 0;
        let mut b = a.clone();
        b.space.tiling = 2.0;
        let ga = a.build_grid(&key(1, 0.0), 64);
        let gb = b.build_grid(&key(1, 0.0), 64);
        let delta: f32 = ga
            .data
            .iter()
            .zip(gb.data.iter())
            .map(|(x, y)| (x - y).abs())
            .sum();
        assert!(
            delta > 1.0,
            "changing tiling must change where curvature lands (total delta {delta})"
        );
    }

    /// A flat tangent-space normal map carrying one convex ridge running along **v** (so the
    /// signal is entirely in `∂n.x/∂u`), `side × side`, GL green.
    ///
    /// The ridge is a triangular bump: `n.x` ramps −peak → +peak across `width` texels centred on
    /// `cx`, which is what a real crease looks like in a baked map.
    fn ridge_normal_map(side: u32, cx: u32, width: u32, peak: f32) -> NormalMapSrc {
        let mut px = vec![0u8; (side * side * 4) as usize];
        let enc = |v: f32| ((v + 1.0) * 127.5).clamp(0.0, 255.0) as u8;
        for y in 0..side {
            for x in 0..side {
                let d = x as i64 - cx as i64;
                let t = (d as f32 / width.max(1) as f32).clamp(-1.0, 1.0);
                let nx = t * peak;
                let i = ((y * side + x) * 4) as usize;
                px[i] = enc(nx);
                px[i + 1] = enc(0.0);
                px[i + 2] = enc(1.0);
                px[i + 3] = 255;
            }
        }
        NormalMapSrc {
            side,
            px,
            green_dx: false,
        }
    }

    /// GATE (Substance parity) — a normal map produces curvature on a body whose GEOMETRY is flat.
    ///
    /// This is the whole point of the second source. The fixture is a single flat quad: geometric
    /// κ is identically 0 everywhere on it, so any non-zero curvature in the result came from the
    /// normal map and nowhere else. Without this source a low-poly asset can only ever show its
    /// silhouette edges, which is what the owner reported as "far from a curvature map".
    #[test]
    fn substance_normal_map_gives_curvature_on_flat_geometry() {
        use core_math::half_edge::HalfEdgeMesh;
        let quad = HalfEdgeMesh::from_polygons(
            &[
                Vec3::new(0.0, 0.0, 0.0),
                Vec3::new(1.0, 0.0, 0.0),
                Vec3::new(1.0, 1.0, 0.0),
                Vec3::new(0.0, 1.0, 0.0),
            ],
            &[vec![0usize, 1, 2, 3]],
        );
        let rings = RingMesh::from_half_edge(&quad);
        let mut src = PolyCurvSource {
            corner_uv: vec![
                Vec2::new(0.0, 0.0),
                Vec2::new(1.0, 0.0),
                Vec2::new(1.0, 1.0),
                Vec2::new(0.0, 1.0),
            ],
            rings,
            space: PolySampleSpace {
                projection: 1,
                tiling: 1.0,
                blend: 4.0,
                rot: [0, 0, 0],
            },
            baked_curvature: None,
            normal_map: None,
        };
        // Corner UVs must follow the emitted ring order, not the authored order.
        let ring: Vec<u32> = src.rings.face_ring.clone();
        let authored = [
            Vec2::new(0.0, 0.0),
            Vec2::new(1.0, 0.0),
            Vec2::new(1.0, 1.0),
            Vec2::new(0.0, 1.0),
        ];
        src.corner_uv = ring.iter().map(|&v| authored[v as usize]).collect();

        // Geometry alone: a flat quad has NO curvature anywhere.
        let flat = src.build_grid(&key(1, 0.0), 64);
        let flat_max = flat.data.iter().cloned().fold(0.0f32, f32::max);
        assert!(
            flat_max < 1.0e-4,
            "a flat quad must bake identically black without a normal map (max {flat_max})"
        );

        // Same geometry + a normal map carrying one SHARP crease: it must appear, strongly.
        // Width 1 = the ramp spans 2 texels, i.e. `∂n.x/∂u ≈ 0.7` — the crease a hard 90° feature
        // bakes into a normal map, and by `NORMAL_CURV_GAIN`'s calibration it should land near the
        // κ of a geometric 90° edge.
        let row_of = |src: &PolyCurvSource| -> Vec<f32> {
            let g = src.build_grid(&key(1, 0.0), 64);
            (0..64)
                .map(|x| g.data[((32 * 64 + x) * 4 + 2) as usize])
                .collect()
        };
        src.normal_map = Some(ridge_normal_map(64, 32, 1, 0.7));
        let sharp = row_of(&src);
        let peak = sharp.iter().cloned().fold(0.0f32, f32::max);
        assert!(
            peak > 0.6,
            "a sharp normal-map crease must produce strong convex curvature on flat geometry \
             (peak {peak}) — this is the Substance per-pixel source, and without it a low-poly \
             body can only show its silhouette"
        );
        // …and it must be LOCALISED at the crease, not a wash across the whole quad.
        assert!(
            sharp[32] > sharp[4] * 3.0 + 0.05,
            "curvature must concentrate at the crease (centre {} vs edge {})",
            sharp[32],
            sharp[4]
        );

        // GRADIENT, not a binary mask: the same crease spread over 8 texels is a gentler slope and
        // must read proportionally weaker rather than clipping to the same white. This is the
        // property `POLY_DISPLAY_SCALE = 1.0` destroyed — at that scale both saturated to 1.0.
        src.normal_map = Some(ridge_normal_map(64, 32, 4, 0.7));
        let gentle = row_of(&src);
        let gentle_peak = gentle.iter().cloned().fold(0.0f32, f32::max);
        assert!(
            gentle_peak > 0.05 && gentle_peak < peak * 0.6,
            "a 4× gentler slope must read clearly weaker than the sharp crease, and still be \
             visible ({gentle_peak} vs {peak}) — equal values mean the shaping is clipping"
        );
    }

    /// GATE — the green-channel convention is honoured.
    ///
    /// DirectX green (Y down) inverts `∂n.y/∂v` only, so reading the wrong convention gives
    /// curvature that is right along u and inverted along v — directional banding that looks
    /// plausible rather than obviously broken. A v-running ridge must flip sign between the two.
    #[test]
    fn normal_map_green_convention_flips_the_v_derivative() {
        // Ridge along U this time: encode the ramp in n.y so it is the v-derivative that reads it.
        let build = |green_dx: bool| -> f32 {
            let side = 64u32;
            let mut px = vec![0u8; (side * side * 4) as usize];
            let enc = |v: f32| ((v + 1.0) * 127.5).clamp(0.0, 255.0) as u8;
            for y in 0..side {
                for x in 0..side {
                    let t = ((y as i64 - 32) as f32 / 4.0).clamp(-1.0, 1.0);
                    let i = ((y * side + x) * 4) as usize;
                    px[i] = enc(0.0);
                    px[i + 1] = enc(t * 0.7);
                    px[i + 2] = enc(1.0);
                    px[i + 3] = 255;
                }
            }
            let mut w = Writer::new(side);
            for i in 0..(side * side) as usize {
                w.wsum[i] = 1.0; // mark fully covered so the source applies everywhere
            }
            w.add_normal_map_curvature(&NormalMapSrc { side, px, green_dx }, 1);
            w.acc[(32 * side + 32) as usize]
        };
        let gl = build(false);
        let dx = build(true);
        assert!(gl.abs() > 1.0e-3, "the ridge must register at all (got {gl})");
        assert!(
            (gl + dx).abs() < 1.0e-5,
            "GL and DirectX green must give equal-and-opposite v curvature ({gl} vs {dx})"
        );
    }


    /// GATE (#6) — BAKED mode reads the map, and reads it back the way the bake wrote it.
    ///
    /// The mode exists for cost, not for looks: switching Live→Baked must change WHERE the signal
    /// comes from, never what it means. So the decode must be the exact inverse of
    /// `mesh_map_bake::bake_curvature_pixels`' grey-centred encode (`v = signed*0.5 + 0.5`), and
    /// the result must run through the SAME `shape_curvature_channels` the measured path uses.
    ///
    /// Also pins the thing that would silently ruin it: a baked map must NOT be re-normalised or
    /// re-smoothed on read. It was normalised once at bake time; doing it again would rescale a
    /// map whose peak no longer means what it did, and re-smoothing would blur it twice.
    #[test]
    fn baked_mode_decodes_the_map_it_was_given() {
        // Encode a known signed ramp exactly as the baker does.
        let side = 64u32;
        let mut px = vec![0u8; (side * side * 4) as usize];
        for y in 0..side {
            for x in 0..side {
                let signed = (x as f32 / (side - 1) as f32) * 2.0 - 1.0; // -1 .. +1 across u
                let v = ((signed * 0.5 + 0.5) * 255.0).round().clamp(0.0, 255.0) as u8;
                let i = ((y * side + x) * 4) as usize;
                px[i] = v;
                px[i + 1] = v;
                px[i + 2] = v;
                px[i + 3] = 255;
            }
        }
        let k = key(0, 0.0); // region 0 = |curv|
        let g = curv_grid_from_baked(&px, side, &k, side);
        assert_eq!(g.res, side);
        assert!(g.key.matches(&k));

        // Channel 1 is signed curvature: must reproduce the ramp, monotonically.
        let row = |x: u32| g.data[((side / 2 * side + x) * 4 + 1) as usize];
        let left = row(0);
        let mid = row(side / 2);
        let right = row(side - 1);
        assert!(left < -0.3, "the -1 end must decode negative (got {left})");
        assert!(right > 0.3, "the +1 end must decode positive (got {right})");
        assert!(mid.abs() < 0.1, "grey must decode as flat (got {mid})");
        assert!(left < mid && mid < right, "the ramp must stay monotonic");

        // A flat grey map decodes to flat everywhere — no invented detail from renormalising.
        let grey = vec![128u8; (side * side * 4) as usize];
        let gg = curv_grid_from_baked(&grey, side, &k, side);
        let peak = (0..(side * side) as usize)
            .map(|i| gg.data[i * 4 + 1].abs())
            .fold(0.0f32, f32::max);
        assert!(
            peak < 0.02,
            "a flat map must stay flat (peak {peak}) - re-normalising a baked map would amplify              its noise into fake detail"
        );
    }
    /// GATE (P3) — an empty body yields a defined, all-zero grid rather than a panic.
    #[test]
    fn p3_empty_source_is_zero_not_a_panic() {
        let src = PolyCurvSource::default();
        assert!(src.is_empty());
        let g = src.build_grid(&key(0, 0.02), 16);
        assert_eq!(g.data.len(), 16 * 16 * 4);
        assert!(g.data.iter().all(|v| *v == 0.0));
    }

    /// DIAGNOSTIC (not a gate) — dump baked curvature maps to PNG so they can be LOOKED at.
    ///
    /// Owner 2026-08-10: *"I SEE NOISE APPEAR BUT THIS IS FAR FROM CURVATURE MAP"*. Numeric gates
    /// pass while the map is visually wrong, so this renders the convex channel the way the
    /// reference does (white on black) for several mesh densities.
    ///
    /// `cargo test -p app --bin app dump_curvature_pngs -- --ignored --nocapture`
    #[test]
    #[ignore = "diagnostic: writes PNGs, run explicitly"]
    fn dump_curvature_pngs() {
        let dir = std::env::var("AZRA_CURV_DUMP").unwrap_or_else(|_| ".".into());
        let write = |name: &str, g: &CurvGrid, channel: usize| {
            let mut px = vec![0u8; (g.res * g.res) as usize];
            for i in 0..(g.res * g.res) as usize {
                px[i] = (g.data[i * 4 + channel].clamp(0.0, 1.0) * 255.0) as u8;
            }
            let path = format!("{dir}/{name}.png");
            image::save_buffer(
                &path,
                &px,
                g.res,
                g.res,
                image::ExtendedColorType::L8,
            )
            .expect("png write");
            let hot = px.iter().filter(|v| **v > 128).count();
            let mid = px.iter().filter(|v| **v > 20 && **v <= 128).count();
            let n = px.len();
            println!(
                "{name}: hot={:.1}% mid={:.1}% black={:.1}%",
                hot as f32 / n as f32 * 100.0,
                mid as f32 / n as f32 * 100.0,
                (n - hot - mid) as f32 / n as f32 * 100.0
            );
        };
        for n in [4usize, 16, 32] {
            let src = unwrapped_cube(n);
            write(&format!("uv_cube_{n}"), &src.build_grid(&key(1, 0.0), 256), 2);
            let mut tri = src.clone();
            tri.corner_uv.clear();
            tri.space.projection = 0;
            write(&format!("tri_cube_{n}"), &tri.build_grid(&key(1, 0.0), 256), 2);
        }

        // DENSE + DISPLACED — the owner's asset class. Surface detail at a frequency comparable to
        // the texel grid is where aliasing ("noise") would come from, if it does.
        let mut dense = unwrapped_cube(48);
        // SMOOTH, correlated surface detail — what a sculpted/scanned surface looks like.
        //
        // NOT a per-vertex hash: `sin(dot(p, k) * 43758)` gives adjacent vertices uncorrelated
        // displacement, i.e. a mesh that is genuinely jagged at vertex scale. Its curvature map is
        // *correctly* noise, so such a fixture can neither confirm nor refute a rasteriser
        // artefact — it looks like the bug whatever the writer does.
        let h = |p: Vec3| -> f32 {
            // Wavelength ~7 quads: detail the texel grid can resolve, which is the regime where
            // a rasteriser artefact would actually show.
            (p.x * 40.0).sin() * (p.y * 40.0).cos()
                + 0.5 * ((p.y * 83.0).sin() * (p.z * 83.0).cos())
        };
        for p in dense.rings.positions.iter_mut() {
            let n = p.normalize_or_zero();
            *p += n * h(*p) * 0.005;
        }
        write("dense_displaced", &dense.build_grid(&key(1, 0.0), 256), 2);

        // NORMAL-MAP source on flat low-poly geometry — the owner's reference case. A procedural
        // "cracked stone" normal map over a coarse cube: the mesh contributes block edges, the
        // normal map contributes the cracks, and the map should show BOTH.
        let side = 256u32;
        let mut px = vec![0u8; (side * side * 4) as usize];
        let enc = |v: f32| ((v + 1.0) * 127.5).clamp(0.0, 255.0) as u8;
        let h = |u: f32, v: f32| -> f32 {
            // Sum of a few sines + a ridged term: crack-like creases, deterministic.
            let a = (u * 21.0).sin() * (v * 17.0).cos();
            let b = (u * 47.0 + v * 31.0).sin();
            -(a.abs() * 0.6 + b.abs() * 0.25)
        };
        let e = 1.0 / side as f32;
        for y in 0..side {
            for x in 0..side {
                let (u, v) = (x as f32 * e, y as f32 * e);
                let hu = (h(u + e, v) - h(u - e, v)) / (2.0 * e);
                let hv = (h(u, v + e) - h(u, v - e)) / (2.0 * e);
                let n = Vec3::new(-hu, -hv, 60.0).normalize();
                let i = ((y * side + x) * 4) as usize;
                px[i] = enc(n.x);
                px[i + 1] = enc(n.y);
                px[i + 2] = enc(n.z);
                px[i + 3] = 255;
            }
        }
        let mut lp = unwrapped_cube(2); // deliberately LOW poly: no geometric detail at all
        lp.normal_map = Some(NormalMapSrc {
            side,
            px,
            green_dx: false,
        });
        write("lowpoly_normalmap", &lp.build_grid(&key(1, 0.0), 256), 2);

        // Saturation profile: how much of the 0..1 range does a real angle sweep actually use?
        println!("--- shaping profile (region=convex, contrast=1) ---");
        for k in [0.02f32, 0.05, 0.1, 0.2, 0.3, 0.4, 0.5, 0.577] {
            let (_, _, conv, _) =
                shape_curvature_channels(k, POLY_DISPLAY_SCALE, 1, 1.0, 1.0);
            println!("  kappa {k:.3} -> convex {conv:.3}");
        }
    }

    /// DIAGNOSTIC (not a gate) — measure and RENDER what `smart_uv_unwrap` actually produces.
    ///
    /// Owner 2026-08-10: *"THE UNWRAP … DOES NOT PRODUCE ANY GOOD RESULT EVEN ON BASIC CUBE"*.
    /// Poly curvature depends on UVs, so the unwrap's quality is now on this path's critical
    /// line. Reports the four numbers that decide whether an unwrap is usable — island count,
    /// packing efficiency, OVERLAP (two surface points sharing a texel = unusable for any baked
    /// map), and area distortion — and dumps the layout.
    ///
    /// `cargo test -p app --bin app dump_uv_unwrap -- --ignored --nocapture`
    #[test]
    #[ignore = "diagnostic: writes PNGs, run explicitly"]
    fn dump_uv_unwrap() {
        use core_math::uv_unwrap::{smart_uv_unwrap, UvUnwrapParams};
        let dir = std::env::var("AZRA_CURV_DUMP").unwrap_or_else(|_| ".".into());

        let cases: Vec<(&str, core_math::half_edge::HalfEdgeMesh)> = vec![
            ("cube1", plain_cube()),
            ("cube4", subdiv_cube_mesh(4)),
            ("cylinder", cylinder_mesh(16, 4)),
        ];
        for (name, mesh) in cases {
            for angle in [66.0f32, 30.0] {
                let r = smart_uv_unwrap(
                    &mesh,
                    &UvUnwrapParams {
                        method: core_math::uv_unwrap::UvUnwrapMethod::Angle,
                        angle_limit_deg: angle,
                        margin: 0.02,
                        max_cone_deg: 75.0,
                    },
                );
                // Rasterise every UV triangle, counting coverage per texel.
                const R: usize = 512;
                let mut cover = vec![0u16; R * R];
                let mut uv_area = 0.0f64;
                let mut ratios: Vec<f32> = Vec::new();
                let uv_of: std::collections::HashMap<u32, Vec2> =
                    r.uvs.iter().map(|(h, uv)| (h.0, *uv)).collect();
                for fi in 0..mesh.faces.len() {
                    let f = core_math::half_edge::FaceId(fi as u32);
                    if !mesh.is_face_live(f) {
                        continue;
                    }
                    let hes = mesh.face_half_edges(f);
                    if hes.len() < 3 {
                        continue;
                    }
                    let uvs: Vec<Vec2> = hes
                        .iter()
                        .map(|h| uv_of.get(&h.0).copied().unwrap_or(Vec2::ZERO))
                        .collect();
                    let pts: Vec<Vec3> = hes
                        .iter()
                        .map(|h| mesh.vert(mesh.he(*h).vertex).position)
                        .collect();
                    for k in 1..uvs.len() - 1 {
                        let (a, b, c) = (uvs[0], uvs[k], uvs[k + 1]);
                        let ar = ((b.x - a.x) * (c.y - a.y) - (b.y - a.y) * (c.x - a.x)).abs() * 0.5;
                        uv_area += ar as f64;
                        let a3 = (pts[k] - pts[0]).cross(pts[k + 1] - pts[0]).length() * 0.5;
                        if a3 > 1e-9 && ar > 1e-12 {
                            ratios.push(ar / a3);
                        }
                        // Fill this triangle into the coverage counter.
                        let (lo, hi) = (a.min(b).min(c), a.max(b).max(c));
                        let x0 = ((lo.x * R as f32).floor() as i64).max(0);
                        let x1 = ((hi.x * R as f32).ceil() as i64).min(R as i64 - 1);
                        let y0 = ((lo.y * R as f32).floor() as i64).max(0);
                        let y1 = ((hi.y * R as f32).ceil() as i64).min(R as i64 - 1);
                        let den = (b.x - a.x) * (c.y - a.y) - (b.y - a.y) * (c.x - a.x);
                        if den.abs() < 1e-12 {
                            continue;
                        }
                        for py in y0..=y1 {
                            for px in x0..=x1 {
                                let p = Vec2::new(
                                    (px as f32 + 0.5) / R as f32,
                                    (py as f32 + 0.5) / R as f32,
                                );
                                let w0 = ((b.x - p.x) * (c.y - p.y) - (b.y - p.y) * (c.x - p.x))
                                    / den;
                                let w1 = ((c.x - p.x) * (a.y - p.y) - (c.y - p.y) * (a.x - p.x))
                                    / den;
                                let w2 = 1.0 - w0 - w1;
                                if w0 >= 0.0 && w1 >= 0.0 && w2 >= 0.0 {
                                    cover[py as usize * R + px as usize] += 1;
                                }
                            }
                        }
                    }
                }
                let filled = cover.iter().filter(|c| **c > 0).count();
                let overlapped = cover.iter().filter(|c| **c > 1).count();
                let (rmin, rmax) = ratios.iter().fold((f32::MAX, 0.0f32), |(lo, hi), r| {
                    (lo.min(*r), hi.max(*r))
                });
                println!(
                    "{name} @{angle}°: islands={} coverage={:.1}% OVERLAP={:.2}% \
                     distortion(min/max area ratio)={:.3}..{:.3} ({:.1}x spread)",
                    r.island_count,
                    filled as f32 / (R * R) as f32 * 100.0,
                    overlapped as f32 / filled.max(1) as f32 * 100.0,
                    rmin,
                    rmax,
                    if rmin > 0.0 { rmax / rmin } else { f32::NAN }
                );
                let px: Vec<u8> = cover
                    .iter()
                    .map(|c| match c {
                        0 => 20u8,            // empty
                        1 => 160,             // good
                        _ => 255,             // OVERLAP — two surface points on one texel
                    })
                    .collect();
                image::save_buffer(
                    format!("{dir}/uv_{name}_{}.png", angle as i32),
                    &px,
                    R as u32,
                    R as u32,
                    image::ExtendedColorType::L8,
                )
                .expect("png");
            }
        }
    }

    /// DIAGNOSTIC — inspect a REAL `.fl4` and report every precondition poly curvature depends on.
    ///
    /// Owner supplied `curvature.fl4` after three rounds of "still nothing". Reading the actual
    /// saved scene answers in one pass what inference has repeatedly got wrong: is the body
    /// polygonal, does its UV sidecar survive save/load, what projection is the material on, and
    /// is there a normal map.
    ///
    /// `AZRA_FL4=<path> cargo test -p app --bin app inspect_fl4 -- --ignored --nocapture`
    #[test]
    #[ignore = "diagnostic: needs AZRA_FL4=<path>"]
    fn inspect_fl4() {
        use core_math::scene_graph::NodePayload;
        let Ok(path) = std::env::var("AZRA_FL4") else {
            println!("set AZRA_FL4=<path to .fl4>");
            return;
        };
        let p = std::path::PathBuf::from(&path);
        assert!(p.exists(), "no such file: {path}");
        let mut loaded = crate::project::load_binary(&p).expect("load_binary");
        println!("── {path} ──");
        println!("poly layer records (UV sidecar on disk): {}", loaded.poly_layers.len());
        for rec in &loaded.poly_layers {
            println!(
                "   node {} : uv corners = {}, seams = {}",
                rec.node_id,
                rec.uv.len(),
                rec.seam.len()
            );
        }
        let sg = loaded.scene_graph.take().expect("scene_graph");
        for (nid, node) in sg.all_nodes() {
            match &node.payload {
                NodePayload::PolygonalBody { name, mesh, modifiers, .. } => {
                    println!(
                        "POLY  {nid:?} '{name}': {} faces, {} verts, {} modifiers",
                        mesh.face_count(),
                        mesh.vertices.len(),
                        modifiers.len()
                    );
                }
                NodePayload::SdfBody { name, primitives, .. } => {
                    println!("SDF   {nid:?} '{name}': {} prims", primitives.len());
                }
                NodePayload::ImportedMesh { name, .. } => {
                    println!("IMPORT {nid:?} '{name}'  (NOT editable poly — convert it)");
                }
                _ => {}
            }
        }
        for (_, id, name, m) in sg.materials() {
            println!(
                "MAT   id={} albedo.projection={} uv_scale={} | normal_map tex={} | \
                 roughness tex={}",
                id.0,
                if m.albedo_map.projection == 1 { "UV" } else { "Box" },
                m.albedo_map.uv_scale,
                m.normal_map.tex_id,
                m.roughness_map.tex_id,
            );
        }
        println!("material_channels (FL4MATX2 records): {}", loaded.material_channels.len());

        // ── END-TO-END on the REAL asset: build the exact snapshot the bake builds, then bake ──
        let dir = std::env::var("AZRA_CURV_DUMP").unwrap_or_else(|_| ".".into());
        for (nid, node) in sg.all_nodes() {
            let NodePayload::PolygonalBody { name, mesh, .. } = &node.payload else {
                continue;
            };
            let rings = RingMesh::from_half_edge(mesh);
            let rec = loaded
                .poly_layers
                .iter()
                .find(|r| r.node_id == nid.0 as u64);
            // Rebuild corner UVs the way `snapshot_poly_curv_source` does: walk each live face's
            // half-edges in `face_half_edges` order (== `face_vertices` order) and look each up.
            let uv_map: std::collections::HashMap<u32, Vec2> = rec
                .map(|r| r.uv.iter().map(|(k, v)| (*k, *v)).collect())
                .unwrap_or_default();
            let mut corner_uv = Vec::with_capacity(rings.face_ring.len());
            let mut hes = Vec::new();
            let mut missing = 0usize;
            for fi in 0..rings.face_count() {
                let (lo, hi) = (
                    rings.face_start[fi] as usize,
                    rings.face_start[fi + 1] as usize,
                );
                if hi == lo {
                    continue;
                }
                mesh.face_half_edges_into(core_math::half_edge::FaceId(fi as u32), &mut hes);
                for &he in &hes {
                    match uv_map.get(&he.0) {
                        Some(uv) => corner_uv.push(*uv),
                        None => {
                            missing += 1;
                            corner_uv.push(Vec2::ZERO);
                        }
                    }
                }
            }
            println!(
                "\n=== '{name}' ===\nmesh corners={}  uv records={}  rebuilt corner_uv={}  \
                 MISSING uv for {missing} corners",
                rings.face_ring.len(),
                uv_map.len(),
                corner_uv.len(),
            );
            let src = PolyCurvSource {
                rings,
                corner_uv,
                space: PolySampleSpace {
                    projection: 1,
                    tiling: 1.0,
                    blend: 4.0,
                    rot: [0, 0, 0],
                },
                baked_curvature: None,
                normal_map: None,
            };
            println!(
                "uses_uv_writer = {}   (needs projection==UV AND corner_uv.len()==face_ring.len())",
                src.uses_uv_writer()
            );
            let (g, cov) = src.build_grid_probe(&key(1, 0.0), 512);
            let conv: Vec<f32> = (0..(g.res * g.res) as usize)
                .map(|i| g.data[i * 4 + 2])
                .collect();
            let peak = conv.iter().cloned().fold(0.0f32, f32::max);
            let hot = conv.iter().filter(|v| **v > 0.5).count();
            println!(
                "BAKED: written coverage={:.1}%  peak convex={:.3}  hot texels={} ({:.1}%)",
                cov * 100.0,
                peak,
                hot,
                hot as f32 / conv.len() as f32 * 100.0
            );
            let px: Vec<u8> = conv
                .iter()
                .map(|v| (v.clamp(0.0, 1.0) * 255.0) as u8)
                .collect();
            let out = format!("{dir}/real_{name}.png");
            image::save_buffer(&out, &px, g.res, g.res, image::ExtendedColorType::L8)
                .expect("png");
            println!("wrote {out}");

            // The MESH MAP the Bake button writes to a texture slot: signed curvature, grey
            // centred (0.5 = flat). Encoded exactly as `bake_curvature_pixels` does, so this is
            // the byte-for-byte asset the user would get.
            let mm = src.build_grid(
                &CurvKey {
                    eps: 0.0,
                    prim_uid: 0,
                    region: 0,
                    grow: 0.0,
                    band: 0.2,
                    contrast: 1.0,
                    strength: 1.0,
                },
                512,
            );
            let mmn = (mm.res * mm.res) as usize;
            let mut gray = vec![0u8; mmn];
            // Mirror `bake_curvature_pixels`' auto-normalisation exactly.
            let mut peak = 0.0f32;
            for i in 0..mmn {
                peak = peak.max(mm.data[i * 4 + 1].abs());
            }
            let gain = if peak > 1.0e-3 { 0.5 / peak } else { 0.0 };
            let mut lo = 1.0f32;
            let mut hi = 0.0f32;
            for i in 0..mmn {
                let f = (mm.data[i * 4 + 1] * gain).clamp(-0.5, 0.5) + 0.5;
                lo = lo.min(f);
                hi = hi.max(f);
                gray[i] = (f * 255.0).round().clamp(0.0, 255.0) as u8;
            }
            println!(
                "MESH MAP (signed, grey-centred, auto-normalised): raw peak {peak:.4},                  gain {gain:.1}x, range {lo:.3}..{hi:.3} (0.5 = flat)"
            );
            let out2 = format!("{dir}/meshmap_{name}.png");
            image::save_buffer(&out2, &gray, mm.res, mm.res, image::ExtendedColorType::L8)
                .expect("png");
            println!("wrote {out2}");

            // ── AO + Thickness on the real asset (the #1 deliverable) ──────────────────────
            use core_math::bvh::{blas_any_hit, build_tri_blas};
            use core_math::mesh_curvature::triangulate_rings;
            let positions: Vec<[f32; 3]> = src
                .rings
                .positions
                .iter()
                .map(|p| [p.x, p.y, p.z])
                .collect();
            let indices = triangulate_rings(&src.rings);
            let blas = build_tri_blas(&positions, &indices);
            let (blo, bhi) = src.rings.aabb();
            let diag = (bhi - blo).length().max(1e-4);
            let (_, normals) =
                core_math::mesh_curvature::vertex_curvature_and_normals_rings(&src.rings, 0);
            let rays = 64usize;
            let mut ao_sum = 0.0f64;
            let mut ao_min = 1.0f32;
            let mut ao_max = 0.0f32;
            let mut counted = 0usize;
            for (vi, nrm) in normals.iter().enumerate() {
                if *nrm == Vec3::ZERO {
                    continue;
                }
                let o = src.rings.positions[vi] + *nrm * (diag * 1e-3);
                let up = if nrm.z.abs() < 0.9 { Vec3::Z } else { Vec3::X };
                let t1 = nrm.cross(up).normalize_or_zero();
                let t2 = nrm.cross(t1);
                let mut open = 0;
                for i in 0..rays {
                    let t = (i as f32 + 0.5) / rays as f32;
                    let s = t.sqrt();
                    let c = (1.0 - s * s).max(0.0).sqrt();
                    let phi = i as f32 * 2.399_963_2;
                    let d = t1 * (s * phi.cos()) + t2 * (s * phi.sin()) + *nrm * c;
                    if !blas_any_hit(
                        &blas, &positions, &indices,
                        [o.x, o.y, o.z], [d.x, d.y, d.z], diag * 0.75,
                    ) {
                        open += 1;
                    }
                }
                let ao = open as f32 / rays as f32;
                ao_sum += ao as f64;
                ao_min = ao_min.min(ao);
                ao_max = ao_max.max(ao);
                counted += 1;
            }
            println!(
                "AO ({} tris, {counted} verts, {rays} rays): min {ao_min:.3} mean {:.3} max \
                 {ao_max:.3}  {}",
                indices.len() / 3,
                ao_sum / counted.max(1) as f64,
                if ao_max - ao_min > 0.05 {
                    "-> occluded pockets found"
                } else {
                    // Uniform 1.0 is CORRECT on a convex body: every hemisphere ray leaves the
                    // surface and hits nothing. Only uniform 0.0 would indicate an inert query
                    // (or missing self-intersection bias).
                    "-> convex everywhere (uniform is correct here)"
                }
            );
        }
    }

    fn plain_cube() -> core_math::half_edge::HalfEdgeMesh {
        let h = 0.5f32;
        let p: Vec<Vec3> = [
            [-h, -h, -h], [h, -h, -h], [h, h, -h], [-h, h, -h],
            [-h, -h, h], [h, -h, h], [h, h, h], [-h, h, h],
        ]
        .iter()
        .map(|c| Vec3::new(c[0], c[1], c[2]))
        .collect();
        core_math::half_edge::HalfEdgeMesh::from_polygons(
            &p,
            &[
                vec![0usize, 3, 2, 1], vec![4, 5, 6, 7], vec![0, 1, 5, 4],
                vec![2, 3, 7, 6], vec![1, 2, 6, 5], vec![0, 4, 7, 3],
            ],
        )
    }

    fn subdiv_cube_mesh(n: usize) -> core_math::half_edge::HalfEdgeMesh {
        let src = unwrapped_cube(n);
        let mut faces: Vec<Vec<usize>> = Vec::new();
        for fi in 0..src.rings.face_count() {
            let r = src.rings.face(fi);
            if r.len() >= 3 {
                faces.push(r.iter().map(|v| *v as usize).collect());
            }
        }
        core_math::half_edge::HalfEdgeMesh::from_polygons(&src.rings.positions, &faces)
    }

    /// Closed cylinder — the case a per-chart PLANAR projection cannot represent.
    fn cylinder_mesh(segs: usize, rings: usize) -> core_math::half_edge::HalfEdgeMesh {
        let mut pos: Vec<Vec3> = Vec::new();
        for r in 0..=rings {
            let y = -0.5 + r as f32 / rings as f32;
            for s in 0..segs {
                let a = std::f32::consts::TAU * s as f32 / segs as f32;
                pos.push(Vec3::new(0.5 * a.cos(), y, 0.5 * a.sin()));
            }
        }
        let mut faces: Vec<Vec<usize>> = Vec::new();
        for r in 0..rings {
            for s in 0..segs {
                let a = r * segs + s;
                let b = r * segs + (s + 1) % segs;
                faces.push(vec![a, b, b + segs, a + segs]);
            }
        }
        faces.push((0..segs).rev().collect());
        faces.push((rings * segs..rings * segs + segs).collect());
        core_math::half_edge::HalfEdgeMesh::from_polygons(&pos, &faces)
    }

    /// GATE (P3) — dilation bleeds outward but never invents coverage where nothing was measured
    /// at all.
    #[test]
    fn p3_dilate_grows_coverage_from_a_seed_only() {
        let mut w = Writer::new(16);
        assert_eq!(w.coverage(), 0.0);
        w.dilate(4);
        assert_eq!(w.coverage(), 0.0, "nothing to grow from → still nothing");

        w.add(8, 8, 1.0, 1.0);
        w.finish_normalized();
        let before = w.coverage();
        w.dilate(2);
        assert!(w.coverage() > before, "a seed must bleed outward");
    }

    /// CORNER DIAGNOSTIC on an owner-supplied `.fl4` (BEVEL.md §0.1 — measure, never infer).
    ///
    /// `AZRA_FL4=<path> cargo test -p app --bin app inspect_corner -- --ignored --nocapture`
    ///
    /// Answers "which operation produced this corner" from the FILE rather than from inference:
    /// the modifier stack (is there a Bevel at all, and with what params), the face-arity
    /// histogram (a bevel band is quads; a centre-fan cap is triangles), the highest-aspect faces
    /// (the visible pinch/dart), and the highest-valence vertices (a centre-fan cap leaves one hub).
    #[test]
    #[ignore = "diagnostic: needs AZRA_FL4=<path>"]
    fn inspect_corner() {
        use core_math::half_edge::{FaceId, VertexId};
        use core_math::scene_graph::NodePayload;
        use glam::Vec3;

        let Ok(path) = std::env::var("AZRA_FL4") else {
            println!("set AZRA_FL4=<path to .fl4>");
            return;
        };
        let p = std::path::PathBuf::from(&path);
        assert!(p.exists(), "no such file: {path}");
        let mut loaded = crate::project::load_binary(&p).expect("load_binary");
        let sg = loaded.scene_graph.take().expect("scene_graph");

        for (nid, node) in sg.all_nodes() {
            let NodePayload::PolygonalBody { mesh, name, modifiers, .. } = &node.payload else {
                continue;
            };
            println!("\n======== POLY {nid:?} '{name}' ========");
            println!(
                "nv={} nf={} manifold={:?} validate={:?}",
                mesh.vertex_count(),
                mesh.face_count(),
                mesh.is_manifold().is_ok(),
                mesh.validate().is_ok()
            );
            println!("modifiers: {}", modifiers.len());
            for (i, m) in modifiers.iter().enumerate() {
                println!(
                    "  [{i}] {:?} enabled={} | bevel w={} seg={} shape={} clamp={} \
                     limit_angle={} angle={} smart={} consume={} var={} wmin={} merge={}",
                    m.mod_type, m.enabled, m.bevel_width, m.bevel_segments, m.bevel_shape,
                    m.bevel_clamp, m.bevel_limit_angle, m.bevel_angle_deg, m.bevel_smart_select,
                    m.bevel_consume, m.bevel_var_width, m.bevel_width_min, m.bevel_auto_merge,
                );
            }

            let mut arity: std::collections::BTreeMap<usize, usize> = Default::default();
            let mut worst: Vec<(f32, u32, usize, f32, f32)> = Vec::new();
            for f in 0..mesh.face_count() as u32 {
                let fid = FaceId(f);
                if !mesh.is_face_live(fid) {
                    continue;
                }
                let vs = mesh.face_vertices(fid);
                *arity.entry(vs.len()).or_default() += 1;
                if vs.len() < 3 {
                    continue;
                }
                let pts: Vec<Vec3> = vs.iter().map(|v| mesh.vert(*v).position).collect();
                // Newell area + edge-length extremes → aspect ratio (the visual dart).
                let mut n = Vec3::ZERO;
                let mut lmin = f32::INFINITY;
                let mut lmax: f32 = 0.0;
                for i in 0..pts.len() {
                    let a = pts[i];
                    let b = pts[(i + 1) % pts.len()];
                    n += a.cross(b);
                    let l = (b - a).length();
                    lmin = lmin.min(l);
                    lmax = lmax.max(l);
                }
                let aspect = if lmin > 1e-9 { lmax / lmin } else { f32::INFINITY };
                worst.push((aspect, f, pts.len(), 0.5 * n.length(), lmin));
            }
            println!("face arity: {arity:?}");
            worst.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
            println!("-- 8 highest-aspect faces (the pinch, if any) --");
            for (aspect, f, n, area, lmin) in worst.iter().take(8) {
                println!(
                    "  f{f}: arity={n} aspect={aspect:.1} area={area:.6} shortest_edge={lmin:.6}"
                );
            }

            let mut hubs: Vec<(usize, u32)> = (0..mesh.vertex_count() as u32)
                .filter(|&v| mesh.is_vert_live(VertexId(v)))
                .map(|v| (mesh.vertex_fan(VertexId(v)).len(), v))
                .collect();
            hubs.sort_by(|a, b| b.0.cmp(&a.0));
            println!("-- 6 highest-valence vertices --");
            for (deg, v) in hubs.iter().take(6) {
                println!("  v{v}: valence={deg} pos={:?}", mesh.vert(VertexId(*v)).position);
            }

            // OBJ dump — hands the EXACT base mesh to the Blender oracle (§7), so the reference is
            // generated on the owner's geometry instead of a synthetic stand-in.
            if let Ok(dir) = std::env::var("AZRA_OBJ_OUT") {
                let mut s = String::new();
                // Live-vertex compaction: OBJ is 1-based and dense, the half-edge mesh is neither.
                let mut remap = vec![0u32; mesh.vertex_count()];
                let mut n = 0u32;
                for v in 0..mesh.vertex_count() as u32 {
                    if !mesh.is_vert_live(VertexId(v)) {
                        continue;
                    }
                    let p = mesh.vert(VertexId(v)).position;
                    s.push_str(&format!("v {:.7} {:.7} {:.7}\n", p.x, p.y, p.z));
                    n += 1;
                    remap[v as usize] = n; // 1-based
                }
                for f in 0..mesh.face_count() as u32 {
                    let fid = FaceId(f);
                    if !mesh.is_face_live(fid) {
                        continue;
                    }
                    s.push('f');
                    for v in mesh.face_vertices(fid) {
                        s.push_str(&format!(" {}", remap[v.0 as usize]));
                    }
                    s.push('\n');
                }
                let out = std::path::Path::new(&dir).join(format!("{name}.obj"));
                std::fs::write(&out, s).expect("write obj");
                println!("wrote {} ({n} verts)", out.display());
            }
        }
    }
}

// ============================================================================
// BAKED-CURVATURE CONSUMPTION — owner 2026-08-15: the bake succeeds, then the generator and
// mask render black. Root cause: the live-triplanar guard rejected the SOURCE before the baked
// map was ever sampled, so a finished bake was thrown away by its own consumer.
// ============================================================================
#[cfg(test)]
mod baked_consumption_tests {
    use super::*;

    /// A grey-centred 8x8 map with a bright band — stands in for a real bake.
    fn fake_baked_map(side: u32) -> Vec<u8> {
        let n = (side * side) as usize;
        let mut px = vec![128u8; n * 4];
        for i in 0..n {
            px[i * 4 + 3] = 255;
            if (i / side as usize) == (side as usize / 2) {
                px[i * 4] = 255;
                px[i * 4 + 1] = 255;
                px[i * 4 + 2] = 255;
            }
        }
        px
    }

    /// BAKED mode must decode the map WITHOUT running a writer — so it cannot be blocked by the
    /// triplanar guard, and it must not be re-smoothed (radius was baked in).
    #[test]
    fn baked_source_yields_a_grid_without_the_uv_writer() {
        let side = 16u32;
        let src = PolyCurvSource {
            rings: RingMesh::default(),
            corner_uv: Vec::new(),          // no UVs resolved on the DISPLAY mesh
            space: PolySampleSpace::default(), // projection 0 = Box → uses_uv_writer() == false
            baked_curvature: Some((side, fake_baked_map(side))),
            normal_map: None,
        };
        assert!(
            !src.uses_uv_writer(),
            "fixture must be in the exact state the guard used to reject"
        );
        let key = CurvKey { prim_uid: 0, eps: 0.02, region: 0, grow: 0.0, band: 0.2, contrast: 1.0, strength: 1.0 };
        let g = src.build_grid(&key, 32);
        assert_eq!(g.res, 32);
        assert!(
            g.data.iter().any(|v| v.abs() > 1.0e-4),
            "a baked map with a bright band must decode to non-zero curvature, not black"
        );
    }
}
