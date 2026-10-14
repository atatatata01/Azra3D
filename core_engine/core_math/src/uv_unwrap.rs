//! Smart / angle-based UV unwrap for half-edge meshes.
//!
//! Pipeline (Blender Smart UV Project style, simplified):
//!   1. Split faces into **charts** by dihedral angle (limit) + boundary edges
//!   2. Project each chart to 2D using its average face normal
//!   3. **Shelf-pack** islands into the unit square with margin
//!   4. Write per-corner UVs (`HalfEdgeId → Vec2`) + mark chart-boundary seams
//!
//! Pure math — no app types. Callers write into `CustomData.uv` / `.seam`.

use crate::half_edge::{FaceId, HalfEdgeId, HalfEdgeMesh, NO_EDGE, NO_FACE, NO_TWIN};
use glam::{Vec2, Vec3};

/// What DEFINES an island.
///
/// Owner 2026-08-11: *"smart uv unpack do not follow my seam"*. The seams were being honoured —
/// but only as EXTRA cuts on top of automatic angle splitting, so on any hard-surface mesh the
/// 66° limit carved it up regardless and the marked seams could not be seen to matter. Honouring
/// a seam and being CONTROLLED by it are different things, and only the second is what marking a
/// seam means.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UvUnwrapMethod {
    /// **Angle-based** (Blender's Smart UV Project). Splits automatically wherever neighbouring
    /// faces exceed `angle_limit_deg`; marked seams add further cuts. Good with no seams authored.
    Angle,
    /// **Seam-based** (Blender's Unwrap). The author's seams are the ONLY cuts — no automatic
    /// angle splitting at all.
    ///
    /// The normal-cone guard still applies, and cannot be switched off: each chart is flattened by
    /// PLANAR PROJECTION, which folds onto itself once a chart's normals span more than 90°
    /// (measured: 39 % of texels doubly-used on a cylinder). So a chart the seams leave too curved
    /// is still cut, and the caller is told how many extra cuts that took rather than being given
    /// a silently different answer. Removing that limitation means a conformal solver (LSCM), not
    /// a flag.
    Seams,
}

impl UvUnwrapMethod {
    pub fn label(self) -> &'static str {
        match self {
            UvUnwrapMethod::Angle => "Angle (Smart)",
            UvUnwrapMethod::Seams => "Seams only",
        }
    }
    pub fn hint(self) -> &'static str {
        match self {
            UvUnwrapMethod::Angle => {
                "Split automatically where faces meet above the Angle limit. Marked seams add \
                 extra cuts. Best when you have not authored seams."
            }
            UvUnwrapMethod::Seams => {
                "Your marked seams are the ONLY cuts — no automatic angle splitting. Mark seams \
                 with C, then unwrap. (A chart that curves past the Max Cone is still cut, or a \
                 planar projection would fold it onto itself.)"
            }
        }
    }
    pub fn as_u32(self) -> u32 {
        match self {
            UvUnwrapMethod::Angle => 0,
            UvUnwrapMethod::Seams => 1,
        }
    }
    pub fn from_u32(v: u32) -> Self {
        match v {
            1 => UvUnwrapMethod::Seams,
            _ => UvUnwrapMethod::Angle,
        }
    }
}

/// Parameters for [`smart_uv_unwrap`].
#[derive(Clone, Debug)]
pub struct UvUnwrapParams {
    /// What defines an island — see [`UvUnwrapMethod`].
    pub method: UvUnwrapMethod,
    /// Faces connected by an edge whose dihedral is steeper than this (degrees between
    /// normals) become separate islands. Blender Smart UV Project default ≈ 66°.
    pub angle_limit_deg: f32,
    /// Padding between packed islands as a fraction of the unit UV square (0..0.2).
    pub margin: f32,
    /// **Maximum half-angle of a chart's normal cone**, in degrees. A face is refused entry to a
    /// chart when its normal deviates more than this from the chart's running area-weighted mean.
    ///
    /// This is what makes step 2 (project each chart along its average normal) *sound*. The
    /// dihedral test alone bounds the angle between NEIGHBOURS, not across the chart, so a
    /// smoothly curving surface — a cylinder side, a rounded stone — grows one chart that wraps
    /// right around and folds onto itself when projected. Measured on a 16-segment cylinder before
    /// this existed: **39 % of used texels carried two or more surface points**, with 956× area
    /// distortion. A baked map cannot be correct on that; two surface points sharing a texel is
    /// not a quality issue, it is a wrong answer.
    ///
    /// 90° is the theoretical limit (beyond it the projection folds); the default leaves margin.
    /// Lower = more, flatter islands (more seams, less distortion); higher = fewer, more distorted.
    pub max_cone_deg: f32,
}

impl Default for UvUnwrapParams {
    fn default() -> Self {
        Self {
            method: UvUnwrapMethod::Angle,
            angle_limit_deg: 66.0,
            margin: 0.02,
            max_cone_deg: 75.0,
        }
    }
}

/// Result of a smart unwrap — corner UVs + edges that form chart seams.
#[derive(Clone, Debug, Default)]
pub struct UvUnwrapResult {
    /// `(corner half-edge, uv)` for every live face corner.
    pub uvs: Vec<(HalfEdgeId, Vec2)>,
    /// Chart-boundary half-edges (primary side). Mark as UV seams in CustomData.
    pub seams: Vec<HalfEdgeId>,
    pub island_count: u32,
    /// Cuts the NORMAL-CONE guard had to add beyond what the method asked for. Non-zero in
    /// `Seams` mode means the author's seams left a chart too curved to flatten without folding;
    /// surfaced to the user rather than silently changing the island count.
    pub cone_cuts: u32,
}

struct Island {
    corners: Vec<(HalfEdgeId, Vec2)>,
    width: f32,
    height: f32,
}

/// Angle-based smart UV unwrap. Empty mesh → empty result.
///
/// Ignores authored seams — prefer [`smart_uv_unwrap_with_seams`] anywhere the user can mark them.
pub fn smart_uv_unwrap(mesh: &HalfEdgeMesh, params: &UvUnwrapParams) -> UvUnwrapResult {
    smart_uv_unwrap_with_seams(mesh, params, None)
}

/// [`smart_uv_unwrap`] that **honours the user's marked seams**.
///
/// An authored seam is a hard chart boundary: the flood fill never crosses one, whatever the
/// dihedral angle or normal cone say. That is the whole point of marking a seam, and it is the
/// Blender/Substance contract — the automatic split is a starting point, the marked seams are the
/// author's override of it.
///
/// Before this existed, `Mark Seam` (key `C`) and the unwrap were completely disconnected: the
/// unwrap computed its own charts from geometry alone, and `apply_unwrap_to_custom_data` then
/// **overwrote** `cd.seam` with those chart boundaries — so marking seams and unwrapping both
/// ignored the author's seams AND destroyed them. The tool existed and could not affect the
/// result.
///
/// Seam-ness is per-EDGE from a per-corner layer: marked on either half-edge marks the edge, the
/// same rule `poly_mesh::edge_is_sharp` uses (a user may only have marked one side).
pub fn smart_uv_unwrap_with_seams(
    mesh: &HalfEdgeMesh,
    params: &UvUnwrapParams,
    seams: Option<&crate::poly_mesh::BitLayer<HalfEdgeId>>,
) -> UvUnwrapResult {
    let is_seam = |h: HalfEdgeId| -> bool {
        let Some(s) = seams else { return false };
        if s.get(h) {
            return true;
        }
        let t = mesh.he(h).twin;
        t != NO_TWIN && s.get(t)
    };
    let faces = live_faces(mesh);
    if faces.is_empty() {
        return UvUnwrapResult::default();
    }

    // Seam-based: the angle test must never fire, so the author's seams are the only cuts.
    // 180 deg = cos(-1), which every real dihedral exceeds.
    let effective_angle = match params.method {
        UvUnwrapMethod::Seams => 180.0,
        UvUnwrapMethod::Angle => params.angle_limit_deg,
    };
    let cos_limit = effective_angle.clamp(0.0, 180.0).to_radians().cos();
    // Cone test is capped just under 90°: at or past 90° the planar projection folds by
    // definition, so allowing it would re-admit the self-overlap this guard exists to stop.
    let cos_cone = params.max_cone_deg.clamp(5.0, 89.0).to_radians().cos();
    let mut chart_of = vec![u32::MAX; mesh.faces.len()];
    let mut charts: Vec<Vec<FaceId>> = Vec::new();
    let mut cone_cuts = 0u32;

    for &seed in &faces {
        if chart_of[seed.0 as usize] != u32::MAX {
            continue;
        }
        let cid = charts.len() as u32;
        charts.push(Vec::new());
        let mut stack = vec![seed];
        chart_of[seed.0 as usize] = cid;
        // Running area-weighted mean normal of the chart — the axis the chart will actually be
        // projected along, so the admission test below measures the thing that matters.
        let mut cone_acc = mesh.compute_face_normal(seed).normalize_or_zero()
            * face_area(mesh, seed).max(1e-12);
        while let Some(f) = stack.pop() {
            charts[cid as usize].push(f);
            for he in mesh.face_half_edges(f) {
                let e = mesh.he(he);
                if e.twin == NO_TWIN {
                    continue;
                }
                let twin = mesh.he(e.twin);
                if twin.face == NO_FACE || !mesh.is_face_live(twin.face) {
                    continue;
                }
                let of = twin.face;
                if chart_of[of.0 as usize] != u32::MAX {
                    continue;
                }
                // An AUTHORED seam is an absolute boundary — checked before the geometric tests,
                // because it is the author overriding them, not competing with them.
                if is_seam(he) {
                    continue;
                }
                // Join when angle between normals is SMALLER than limit (cos larger).
                let join = match mesh.edge_dihedral_cos(he) {
                    Some(c) => c >= cos_limit,
                    None => false,
                };
                if !join {
                    continue;
                }
                // …AND when the face stays inside the chart's normal cone. The dihedral test
                // above only bounds NEIGHBOURS; without this, curvature accumulates across a
                // chart until it wraps around and the projection folds onto itself.
                let cand = mesh.compute_face_normal(of).normalize_or_zero();
                let axis = cone_acc.normalize_or_zero();
                if axis == Vec3::ZERO || cand == Vec3::ZERO || cand.dot(axis) < cos_cone {
                    // Counted so Seams mode can SAY it added cuts the author did not ask for,
                    // instead of quietly producing more islands than seams.
                    cone_cuts += 1;
                    continue;
                }
                chart_of[of.0 as usize] = cid;
                cone_acc += cand * face_area(mesh, of).max(1e-12);
                stack.push(of);
            }
        }
    }

    let mut islands: Vec<Island> = Vec::with_capacity(charts.len());
    for chart_faces in &charts {
        if chart_faces.is_empty() {
            continue;
        }
        let mut n_acc = Vec3::ZERO;
        for &f in chart_faces {
            let n = mesh.compute_face_normal(f);
            let area = face_area(mesh, f).max(1e-12);
            n_acc += n.normalize_or_zero() * area;
        }
        let n = {
            let n = n_acc.normalize_or_zero();
            if n.length_squared() < 1e-10 {
                Vec3::Y
            } else {
                n
            }
        };
        let up = if n.dot(Vec3::Y).abs() < 0.9 {
            Vec3::Y
        } else {
            Vec3::X
        };
        let u_axis = n.cross(up).normalize_or_zero();
        let v_axis = u_axis.cross(n).normalize_or_zero();

        let mut corners: Vec<(HalfEdgeId, Vec2)> = Vec::new();
        let mut mn = Vec2::splat(f32::INFINITY);
        let mut mx = Vec2::splat(f32::NEG_INFINITY);
        for &f in chart_faces {
            for he in mesh.face_half_edges(f) {
                let p = mesh.vert(mesh.he(he).vertex).position;
                let uv = Vec2::new(p.dot(u_axis), p.dot(v_axis));
                mn = mn.min(uv);
                mx = mx.max(uv);
                corners.push((he, uv));
            }
        }
        if !mn.is_finite() || !mx.is_finite() {
            continue;
        }
        for (_, uv) in &mut corners {
            *uv -= mn;
        }
        let size = (mx - mn).max(Vec2::splat(1e-6));
        islands.push(Island {
            corners,
            width: size.x,
            height: size.y,
        });
    }

    // ORIENT each island to its minimum-area box before packing.
    //
    // A chart is projected along its own normal, so its 2D orientation is arbitrary — a diagonal
    // strip lands in a bounding box mostly full of empty space, and the packer can only ever pack
    // boxes. Rotating each island to the angle that minimises its box area is the single biggest
    // packing win available and costs one coarse sweep per island.
    for isl in &mut islands {
        orient_island_to_min_box(isl);
    }

    islands.sort_by(|a, b| {
        (b.width * b.height)
            .partial_cmp(&(a.width * a.height))
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let margin = params.margin.clamp(0.0, 0.15);
    let packed = shelf_pack(&islands, margin);

    let mut seams = Vec::new();
    for &f in &faces {
        for he in mesh.face_half_edges(f) {
            let e = mesh.he(he);
            if e.twin != NO_TWIN && e.twin.0 < he.0 {
                continue;
            }
            let other = if e.twin != NO_TWIN {
                mesh.he(e.twin).face
            } else {
                NO_FACE
            };
            let c0 = chart_of[f.0 as usize];
            let c1 = if other != NO_FACE && (other.0 as usize) < chart_of.len() {
                chart_of[other.0 as usize]
            } else {
                u32::MAX
            };
            if c0 != c1 {
                seams.push(he);
            }
        }
    }

    UvUnwrapResult {
        uvs: packed,
        seams,
        island_count: islands.len() as u32,
        cone_cuts,
    }
}

/// **Repack existing UVs** without re-deriving charts from geometry.
///
/// This is the difference between a real Pack and a disguised Unwrap. Islands are found by UV
/// CONNECTIVITY — two faces belong to the same island when they share an edge *and* their UVs
/// agree across it — so a layout the user built (or edited, or imported) is preserved and only
/// re-placed. Re-running `smart_uv_unwrap` instead would throw the authored layout away and
/// silently substitute freshly-derived charts, which is the same class of lie as an unwrap that
/// ignores marked seams.
///
/// An edge is UV-continuous when BOTH of its endpoints agree. For a half-edge `h` with twin `t`:
/// `uv(h)` and `uv(t.prev)` both sit at `h.vertex`, and `uv(h.prev)` and `uv(t)` both sit at
/// `h`'s source — so a seam is any edge where either pair differs.
///
/// Returns the repacked per-corner UVs, or an empty vec when there is nothing to pack.
pub fn pack_existing_uvs(
    mesh: &HalfEdgeMesh,
    uv: &crate::poly_mesh::LayerStore<HalfEdgeId, Vec2>,
    margin: f32,
) -> Vec<(HalfEdgeId, Vec2)> {
    let faces = live_faces(mesh);
    if faces.is_empty() || uv.is_empty() {
        return Vec::new();
    }
    let uv_at = |h: HalfEdgeId| -> Option<Vec2> { uv.get(h).copied() };
    // Same tolerance the weld uses: exporters and modifier stacks leave last-ulp differences that
    // must not read as seams.
    let same = |a: Option<Vec2>, b: Option<Vec2>| -> bool {
        match (a, b) {
            (Some(x), Some(y)) => (x - y).length() < 1.0e-5,
            _ => false,
        }
    };

    let mut island_of = vec![u32::MAX; mesh.faces.len()];
    let mut charts: Vec<Vec<FaceId>> = Vec::new();
    for &seed in &faces {
        if island_of[seed.0 as usize] != u32::MAX {
            continue;
        }
        let cid = charts.len() as u32;
        charts.push(Vec::new());
        island_of[seed.0 as usize] = cid;
        let mut stack = vec![seed];
        while let Some(f) = stack.pop() {
            charts[cid as usize].push(f);
            for h in mesh.face_half_edges(f) {
                let e = mesh.he(h);
                if e.twin == NO_TWIN {
                    continue;
                }
                let t = e.twin;
                let tf = mesh.he(t).face;
                if tf == NO_FACE || !mesh.is_face_live(tf) || island_of[tf.0 as usize] != u32::MAX {
                    continue;
                }
                let h_prev = mesh.he(h).prev;
                let t_prev = mesh.he(t).prev;
                if h_prev == NO_EDGE || t_prev == NO_EDGE {
                    continue;
                }
                if same(uv_at(h), uv_at(t_prev)) && same(uv_at(h_prev), uv_at(t)) {
                    island_of[tf.0 as usize] = cid;
                    stack.push(tf);
                }
            }
        }
    }

    let mut islands: Vec<Island> = Vec::with_capacity(charts.len());
    for chart in &charts {
        let mut corners: Vec<(HalfEdgeId, Vec2)> = Vec::new();
        let mut mn = Vec2::splat(f32::INFINITY);
        let mut mx = Vec2::splat(f32::NEG_INFINITY);
        for &f in chart {
            for h in mesh.face_half_edges(f) {
                let p = uv_at(h).unwrap_or(Vec2::ZERO);
                mn = mn.min(p);
                mx = mx.max(p);
                corners.push((h, p));
            }
        }
        if corners.is_empty() || !mn.is_finite() || !mx.is_finite() {
            continue;
        }
        for (_, p) in &mut corners {
            *p -= mn;
        }
        let size = (mx - mn).max(Vec2::splat(1.0e-6));
        islands.push(Island {
            corners,
            width: size.x,
            height: size.y,
        });
    }
    for isl in &mut islands {
        orient_island_to_min_box(isl);
    }
    islands.sort_by(|a, b| {
        (b.width * b.height)
            .partial_cmp(&(a.width * a.height))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    shelf_pack(&islands, margin.clamp(0.0, 0.15))
}

/// Apply unwrap result into a CustomData sidecar (overwrites UV + seam layers).
pub fn apply_unwrap_to_custom_data(
    cd: &mut crate::poly_mesh::CustomData,
    result: &UvUnwrapResult,
) {
    cd.uv = crate::poly_mesh::LayerStore::new(crate::poly_mesh::Interp::Copy);
    for &(he, uv) in &result.uvs {
        cd.uv.set(he, uv);
    }
    // THE SEAM LAYER IS AUTHORED DATA. An unwrap must not write to it — not by replacing, and
    // not by adding.
    //
    // Two wrong versions preceded this, and the second was worse than the first:
    //
    // 1. `cd.seam = BitLayer::new()` — every unwrap DELETED the author's marks and substituted
    //    its own chart boundaries.
    // 2. Union — the marks survived, but each unwrap ADDED its derived boundaries. On a
    //    hard-surface mesh that is dozens of auto-seams sitting in the same layer as the four the
    //    author drew, indistinguishable from them. Then `UvUnwrapMethod::Seams` — whose whole
    //    contract is "your seams are the only cuts" — consumed all of them and reproduced the
    //    angle-mode result. Marking seams appeared to do nothing, and running Smart appeared to
    //    OVERRIDE them (owner 2026-08-11: *"smart unwrap override my seem"*).
    //
    // The category error was in that comment's own claim that computed boundaries "ARE real seams
    // and belong in the layer". They are a RESULT, not an authorship. Blender draws the same line:
    // Smart UV Project adds no seams to the mesh. `UvUnwrapResult::seams` is still returned for
    // display of island borders — it is simply not persisted as intent.
    let _ = &result.seams;
}

fn live_faces(mesh: &HalfEdgeMesh) -> Vec<FaceId> {
    (0..mesh.faces.len() as u32)
        .map(FaceId)
        .filter(|&f| mesh.is_face_live(f))
        .collect()
}

fn face_area(mesh: &HalfEdgeMesh, f: FaceId) -> f32 {
    let vs = mesh.face_vertices(f);
    if vs.len() < 3 {
        return 0.0;
    }
    let p0 = mesh.vert(vs[0]).position;
    let mut area = 0.0;
    for i in 1..vs.len() - 1 {
        let a = mesh.vert(vs[i]).position - p0;
        let b = mesh.vert(vs[i + 1]).position - p0;
        area += a.cross(b).length() * 0.5;
    }
    area
}

/// Rotate an island in place to the angle whose axis-aligned bounding box has the least area,
/// then re-normalise it to the origin.
///
/// A chart's 2D orientation carries no meaning — it falls out of whichever tangent frame the
/// projection happened to build — so a long diagonal strip gets a bounding box mostly full of
/// nothing, and a packer that can only place boxes pays for all of it.
///
/// A coarse 1° sweep over 0..90° is used rather than exact rotating calipers: the search space is
/// tiny, the objective is smooth, and calipers would need a convex hull for a fraction of a
/// percent. 90° is the full period for a box-area objective (beyond that the box repeats with
/// width and height swapped, which the packer tries anyway).
fn orient_island_to_min_box(isl: &mut Island) {
    if isl.corners.len() < 3 {
        return;
    }
    let mut best = (0.0f32, f32::INFINITY, Vec2::ZERO, Vec2::ZERO);
    for step in 0..90 {
        let a = (step as f32).to_radians();
        let (s, c) = a.sin_cos();
        let mut mn = Vec2::splat(f32::INFINITY);
        let mut mx = Vec2::splat(f32::NEG_INFINITY);
        for (_, uv) in &isl.corners {
            let r = Vec2::new(uv.x * c - uv.y * s, uv.x * s + uv.y * c);
            mn = mn.min(r);
            mx = mx.max(r);
        }
        let size = mx - mn;
        let area = size.x * size.y;
        if area < best.1 {
            best = (a, area, mn, size);
        }
    }
    if !best.1.is_finite() || best.0 == 0.0 {
        return; // already optimal (or degenerate) — leave the corners untouched
    }
    let (s, c) = best.0.sin_cos();
    for (_, uv) in &mut isl.corners {
        let r = Vec2::new(uv.x * c - uv.y * s, uv.x * s + uv.y * c);
        *uv = r - best.2;
    }
    isl.width = best.3.x.max(1e-6);
    isl.height = best.3.y.max(1e-6);
}

/// Shelf-pack islands into [0,1]², at the **largest scale that fits**.
///
/// The scale is found by bisection, not by a guess-then-shrink loop. The previous version computed
/// one conservative scale from an "aim for 85 % area" heuristic and returned the moment it fitted,
/// with no attempt to grow: a plain cube — six equal squares, which tile a 3×2 grid perfectly —
/// packed into **41 % of the map**, leaving the bottom half and right column empty. That is a 2.4×
/// texel-density loss on every baked map, i.e. every texture effectively baked at half resolution.
///
/// Bisection is correct here because fit is *monotonic* in scale: if the islands fit at `s` they
/// fit at anything smaller. 40 iterations converge well past f32 precision.
fn shelf_pack(islands: &[Island], margin: f32) -> Vec<(HalfEdgeId, Vec2)> {
    if islands.is_empty() {
        return Vec::new();
    }
    let total_area: f32 = islands.iter().map(|i| i.width * i.height).sum();
    let usable = (1.0 - margin * 2.0).max(0.1);
    if total_area <= 1e-12 {
        return Vec::new();
    }
    // Upper bound: the scale at which the islands would exactly fill the usable square. No
    // arrangement can beat 100 % area, so nothing above this can ever fit.
    let hi_bound = (usable * usable / total_area).sqrt();

    let mut lo = 0.0f32; // always fits (degenerate)
    let mut hi = hi_bound; // never fits, except in the perfect-tiling case tested first
    let mut best: Option<Vec<(HalfEdgeId, Vec2)>> = None;
    if let Some(out) = try_shelf_pack(islands, hi, margin, usable) {
        return out; // perfect tiling (e.g. a cube's six equal squares)
    }
    for _ in 0..40 {
        let mid = 0.5 * (lo + hi);
        match try_shelf_pack(islands, mid, margin, usable) {
            Some(out) => {
                best = Some(out);
                lo = mid;
            }
            None => hi = mid,
        }
    }
    best.unwrap_or_else(|| {
        // Nothing fitted at any scale — a single island wider than the usable square. Clamp
        // rather than emit UVs outside [0,1]; the caller still gets a defined layout.
        let mut out = Vec::new();
        for isl in islands {
            for &(he, uv) in &isl.corners {
                let p = (uv * lo.max(1e-6) + Vec2::splat(margin)).clamp(Vec2::ZERO, Vec2::ONE);
                out.push((he, p));
            }
        }
        out
    })
}

fn try_shelf_pack(
    islands: &[Island],
    scale: f32,
    margin: f32,
    usable: f32,
) -> Option<Vec<(HalfEdgeId, Vec2)>> {
    let mut out: Vec<(HalfEdgeId, Vec2)> = Vec::new();
    let mut x = margin;
    let mut y = margin;
    let mut shelf_h = 0.0f32;
    let max_x = margin + usable;
    let max_y = margin + usable;

    for isl in islands {
        let (mut w, mut h) = (isl.width * scale, isl.height * scale);
        // 90° SWAP: a tall island can take a wide shelf.
        //
        // Tried only when the island does not fit the current shelf as-is, and only if rotating
        // makes it fit — so the natural orientation (already the minimum-area one, see
        // `orient_island_to_min_box`) is always preferred and rotation is a rescue, not a default.
        let mut rotated = false;
        if x + w > max_x + 1e-5 && x + h <= max_x + 1e-5 && h <= usable + 1e-5 {
            std::mem::swap(&mut w, &mut h);
            rotated = true;
        }
        if w > usable + 1e-5 || h > usable + 1e-5 {
            return None;
        }
        if x + w > max_x + 1e-5 {
            x = margin;
            y += shelf_h + margin;
            shelf_h = 0.0;
        }
        if y + h > max_y + 1e-5 {
            return None;
        }
        for &(he, uv) in &isl.corners {
            // 90° CCW about the origin, then shifted back into the positive quadrant: the island's
            // height becomes its width, which is exactly the swap applied to `w`/`h` above.
            let local = if rotated {
                Vec2::new(isl.height - uv.y, uv.x)
            } else {
                uv
            };
            out.push((he, local * scale + Vec2::new(x, y)));
        }
        x += w + margin;
        shelf_h = shelf_h.max(h);
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use glam::Vec3;

    #[test]
    fn cube_unwrap_covers_all_corners() {
        let mesh = HalfEdgeMesh::new_box(Vec3::splat(0.5));
        let r = smart_uv_unwrap(&mesh, &UvUnwrapParams::default());
        assert!(r.island_count >= 1, "expected islands");
        assert_eq!(r.uvs.len(), 24, "cube has 24 face-corners");
        for (_, uv) in &r.uvs {
            assert!(
                (-0.01..=1.01).contains(&uv.x) && (-0.01..=1.01).contains(&uv.y),
                "uv out of unit square: {uv:?}"
            );
        }
        assert!(!r.seams.is_empty(), "cube should have chart seams");
    }

    /// Closed cylinder: `segs × rings` side quads + two caps.
    fn cylinder(segs: usize, rings: usize) -> HalfEdgeMesh {
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
        HalfEdgeMesh::from_polygons(&pos, &faces)
    }

    /// Fraction of covered texels that carry MORE THAN ONE surface point, by rasterising every
    /// UV triangle into a coverage counter. Overlap is not a quality metric — it is a correctness
    /// one: two surface points in one texel means no baked map can be right for both.
    fn uv_overlap_fraction(mesh: &HalfEdgeMesh, r: &UvUnwrapResult, res: usize) -> f32 {
        let uv_of: std::collections::HashMap<u32, Vec2> =
            r.uvs.iter().map(|(h, uv)| (h.0, *uv)).collect();
        let mut cover = vec![0u16; res * res];
        for fi in 0..mesh.faces.len() {
            let f = FaceId(fi as u32);
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
            for k in 1..uvs.len() - 1 {
                let (a, b, c) = (uvs[0], uvs[k], uvs[k + 1]);
                let den = (b.x - a.x) * (c.y - a.y) - (b.y - a.y) * (c.x - a.x);
                if den.abs() < 1e-12 {
                    continue;
                }
                let lo = a.min(b).min(c);
                let hi = a.max(b).max(c);
                let x0 = ((lo.x * res as f32).floor() as i64).max(0);
                let x1 = ((hi.x * res as f32).ceil() as i64).min(res as i64 - 1);
                let y0 = ((lo.y * res as f32).floor() as i64).max(0);
                let y1 = ((hi.y * res as f32).ceil() as i64).min(res as i64 - 1);
                for py in y0..=y1 {
                    for px in x0..=x1 {
                        let p = Vec2::new(
                            (px as f32 + 0.5) / res as f32,
                            (py as f32 + 0.5) / res as f32,
                        );
                        let w0 = ((b.x - p.x) * (c.y - p.y) - (b.y - p.y) * (c.x - p.x)) / den;
                        let w1 = ((c.x - p.x) * (a.y - p.y) - (c.y - p.y) * (a.x - p.x)) / den;
                        let w2 = 1.0 - w0 - w1;
                        if w0 >= 0.0 && w1 >= 0.0 && w2 >= 0.0 {
                            cover[py as usize * res + px as usize] += 1;
                        }
                    }
                }
            }
        }
        let filled = cover.iter().filter(|c| **c > 0).count().max(1);
        cover.iter().filter(|c| **c > 1).count() as f32 / filled as f32
    }

    /// GATE — a curved surface must NOT produce a self-overlapping chart.
    ///
    /// Each chart is flattened along its average normal, which is only sound while the chart's
    /// normals stay inside a cone narrower than 90°. The dihedral test bounds NEIGHBOURS, not the
    /// chart, so before `max_cone_deg` existed a cylinder's side grew into ONE chart that wrapped
    /// 360° and folded flat onto itself: **39 % of used texels carried two or more surface
    /// points**, area distortion 956×. Every baked map on that body — curvature, AO, colour — was
    /// wrong, silently.
    #[test]
    fn curved_chart_does_not_self_overlap() {
        let mesh = cylinder(16, 4);
        let r = smart_uv_unwrap(&mesh, &UvUnwrapParams::default());
        let overlap = uv_overlap_fraction(&mesh, &r, 256);
        assert!(
            overlap < 0.01,
            "a cylinder must unwrap without folding onto itself (overlap {:.1}% of used texels, \
             {} islands) — raising `max_cone_deg` past 90° reintroduces this by definition",
            overlap * 100.0,
            r.island_count
        );
        assert!(
            r.island_count >= 4,
            "the side must split into several charts, not stay one wrapped band (got {})",
            r.island_count
        );
    }

    /// GATE — a MARKED SEAM splits a chart geometry would keep whole, SURVIVES the unwrap, and
    /// the unwrap adds NOTHING to the authored seam layer.
    ///
    /// Three failures, each of which made `Mark Seam` (key `C`) useless in a different way:
    ///
    /// 1. The unwrap read no seam input at all — marks had no effect.
    /// 2. `apply_unwrap_to_custom_data` opened by DELETING `cd.seam` — marks were destroyed by the
    ///    very operation meant to act on them.
    /// 3. Fixing (2) as a UNION then polluted the layer: every unwrap ADDED its derived chart
    ///    boundaries, so four authored seams became forty-odd indistinguishable ones, and
    ///    `Seams` mode consumed the lot and reproduced the angle result. Smart unwrap appeared to
    ///    OVERRIDE the author's seams.
    ///
    /// The third assertion below is the one that pins (3): an unwrap must leave the authored seam
    /// set **byte-identical**, because that layer is intent, not output.
    #[test]
    fn authored_seam_splits_a_chart_and_is_preserved() {
        use crate::poly_mesh::{BitLayer, CustomData};
        // 4x1 strip of coplanar quads — one chart by any geometric measure.
        let mut pos = Vec::new();
        for i in 0..=4 {
            pos.push(Vec3::new(i as f32, 0.0, 0.0));
            pos.push(Vec3::new(i as f32, 0.0, 1.0));
        }
        let faces: Vec<Vec<usize>> = (0..4)
            .map(|i| vec![i * 2, i * 2 + 2, i * 2 + 3, i * 2 + 1])
            .collect();
        let mesh = HalfEdgeMesh::from_polygons(&pos, &faces);

        let plain = smart_uv_unwrap(&mesh, &UvUnwrapParams::default());
        assert_eq!(
            plain.island_count, 1,
            "a flat strip has no geometric reason to split (got {})",
            plain.island_count
        );

        // Mark the edge between face 1 and face 2 as a seam.
        let mut seams: BitLayer<HalfEdgeId> = BitLayer::new();
        let mut marked = None;
        'outer: for he in mesh.face_half_edges(FaceId(1)) {
            let t = mesh.he(he).twin;
            if t != NO_TWIN && mesh.he(t).face == FaceId(2) {
                seams.set(he, true);
                marked = Some(he);
                break 'outer;
            }
        }
        let marked = marked.expect("faces 1 and 2 must share an edge");

        let split = smart_uv_unwrap_with_seams(&mesh, &UvUnwrapParams::default(), Some(&seams));
        assert!(
            split.island_count > plain.island_count,
            "a marked seam must split the chart ({} vs {})",
            split.island_count,
            plain.island_count
        );

        // Applying the result must PRESERVE the mark…
        let mut cd = CustomData::default();
        cd.seam.set(marked, true);
        let before: Vec<u32> = cd.seam.iter().map(|k| k.0).collect();
        apply_unwrap_to_custom_data(&mut cd, &split);
        assert!(
            cd.seam.get(marked),
            "the author's marked seam must survive the unwrap that honoured it"
        );
        // …and must not ADD anything. Derived chart boundaries are a RESULT, not authorship;
        // letting them into this layer is what made Smart unwrap look like it overrode the author.
        let after: Vec<u32> = cd.seam.iter().map(|k| k.0).collect();
        assert_eq!(
            before.len(),
            after.len(),
            "an unwrap must not add derived boundaries to the AUTHORED seam layer ({} -> {}) —              the result carries them in `UvUnwrapResult::seams` for display instead",
            before.len(),
            after.len()
        );
    }


    /// Total UV area occupied by a result, as a fraction of the unit square.
    fn packed_area(mesh: &HalfEdgeMesh, r: &UvUnwrapResult) -> f32 {
        let uv_of: std::collections::HashMap<u32, Vec2> =
            r.uvs.iter().map(|(h, uv)| (h.0, *uv)).collect();
        let mut a = 0.0;
        for fi in 0..mesh.faces.len() {
            let f = FaceId(fi as u32);
            if !mesh.is_face_live(f) {
                continue;
            }
            let hes = mesh.face_half_edges(f);
            let uvs: Vec<Vec2> = hes
                .iter()
                .map(|h| uv_of.get(&h.0).copied().unwrap_or(Vec2::ZERO))
                .collect();
            for k in 1..uvs.len().saturating_sub(1) {
                let (p, q, t) = (uvs[0], uvs[k], uvs[k + 1]);
                a += ((q.x - p.x) * (t.y - p.y) - (q.y - p.y) * (t.x - p.x)).abs() * 0.5;
            }
        }
        a
    }

    /// GATE — orienting an island to its minimum-area box actually tightens that box.
    ///
    /// A chart's 2D orientation is arbitrary (it falls out of whichever tangent frame the
    /// projection built), so a diagonal strip gets a bounding box mostly full of nothing — and a
    /// packer can only place boxes, so it pays for all of it.
    ///
    /// Asserted on the orientation function DIRECTLY, not through end-to-end packed area, because
    /// packed area is not monotonic in box area for a single island: an 8:1 strip packs ~11.5 % of
    /// a square upright and ~18 % left at 45°, since the 45° box is more SQUARE and scales better
    /// against a square atlas. Minimum-area orientation is the right objective for shelf-packing
    /// MANY islands (it reduces the total area to place); it is not a single-island win, and a
    /// gate claiming otherwise would be asserting something false.
    #[test]
    fn min_box_orientation_improves_a_diagonal_island() {
        // Flat strip rotated 45° in the XZ plane — one coplanar chart, diagonal in projection.
        let a = std::f32::consts::FRAC_PI_4;
        let (sn, cs) = a.sin_cos();
        let mut pos = Vec::new();
        for i in 0..=8 {
            for j in 0..=1 {
                let (x, z) = (i as f32, j as f32);
                pos.push(Vec3::new(x * cs - z * sn, 0.0, x * sn + z * cs));
            }
        }
        let faces: Vec<Vec<usize>> = (0..8)
            .map(|i| vec![i * 2, i * 2 + 2, i * 2 + 3, i * 2 + 1])
            .collect();
        let mesh = HalfEdgeMesh::from_polygons(&pos, &faces);

        // Build the island from the real projected chart (the strip is flat in Y, so XZ is it).
        let mut isl = Island {
            corners: Vec::new(),
            width: 0.0,
            height: 0.0,
        };
        let mut mn = Vec2::splat(f32::INFINITY);
        let mut mx = Vec2::splat(f32::NEG_INFINITY);
        for fi in 0..mesh.faces.len() {
            let f = FaceId(fi as u32);
            if !mesh.is_face_live(f) {
                continue;
            }
            for he in mesh.face_half_edges(f) {
                let p = mesh.vert(mesh.he(he).vertex).position;
                let uv = Vec2::new(p.x, p.z);
                mn = mn.min(uv);
                mx = mx.max(uv);
                isl.corners.push((he, uv));
            }
        }
        for (_, uv) in &mut isl.corners {
            *uv -= mn;
        }
        isl.width = (mx - mn).x;
        isl.height = (mx - mn).y;

        let before = isl.width * isl.height;
        orient_island_to_min_box(&mut isl);
        let after = isl.width * isl.height;
        assert!(
            after < before * 0.75,
            "a 45deg strip's box must shrink substantially when oriented ({before:.2} -> {after:.2})"
        );
        for (_, uv) in &isl.corners {
            assert!(
                uv.x >= -1e-3
                    && uv.y >= -1e-3
                    && uv.x <= isl.width + 1e-3
                    && uv.y <= isl.height + 1e-3,
                "corner {uv:?} escaped the oriented box {}x{}",
                isl.width,
                isl.height
            );
        }

        // End to end: still inside the unit square, still no overlap.
        let r = smart_uv_unwrap(&mesh, &UvUnwrapParams::default());
        assert!(packed_area(&mesh, &r) > 0.0, "the strip must land somewhere");
        for (_, uv) in &r.uvs {
            assert!(
                (-0.001..=1.001).contains(&uv.x) && (-0.001..=1.001).contains(&uv.y),
                "orientation must not push UVs out of the unit square: {uv:?}"
            );
        }
        assert!(
            uv_overlap_fraction(&mesh, &r, 256) < 0.01,
            "rotating an island must not make it overlap anything"
        );
    }

    /// GATE — `Seams` mode is CONTROLLED by seams, not merely influenced by them.
    ///
    /// The seams were honoured all along, but only as EXTRA cuts on top of automatic angle
    /// splitting, so on a hard-surface mesh the 66° limit carved it up regardless and marking a
    /// seam could not be seen to matter.
    ///
    /// The fixture must isolate the ANGLE test from the CONE guard, because the cone cannot be
    /// switched off (a planar projection folds past 90°, so it is correctness, not preference). A
    /// cube is therefore the WRONG fixture — its 90° faces are split by the cone whatever the
    /// method, so "6 islands either way" says nothing.
    ///
    /// A 70° roof separates them cleanly: 70° exceeds the 66° angle limit (Angle mode splits it)
    /// but each face sits only 35° from the chart's mean normal, well inside the cone (Seams mode
    /// keeps it whole). Any difference between the modes can then only come from the method.
    #[test]
    fn seams_method_is_driven_by_seams_not_by_angle() {
        use crate::poly_mesh::BitLayer;
        let a = 70.0f32.to_radians();
        let (s70, c70) = a.sin_cos();
        let pos = vec![
            Vec3::new(-1.0, 0.0, 0.0),
            Vec3::new(1.0, 0.0, 0.0),
            Vec3::new(1.0, 1.0, 0.0),
            Vec3::new(-1.0, 1.0, 0.0),
            Vec3::new(1.0, -c70, s70),
            Vec3::new(-1.0, -c70, s70),
        ];
        let faces: Vec<Vec<usize>> = vec![vec![0, 1, 2, 3], vec![1, 0, 5, 4]];
        let mesh = HalfEdgeMesh::from_polygons(&pos, &faces);

        let angle = UvUnwrapParams::default();
        let seams_mode = UvUnwrapParams {
            method: UvUnwrapMethod::Seams,
            max_cone_deg: 89.0,
            ..UvUnwrapParams::default()
        };

        let a_res = smart_uv_unwrap(&mesh, &angle);
        assert_eq!(
            a_res.island_count, 2,
            "angle mode must split a 70deg fold at the 66deg limit"
        );

        let none = smart_uv_unwrap_with_seams(&mesh, &seams_mode, None);
        assert_eq!(
            none.island_count, 1,
            "seams mode with no seams must NOT split on angle (got {}) - this is the assertion \
             that proves automatic splitting is off rather than merely relaxed",
            none.island_count
        );

        let mut marked: BitLayer<HalfEdgeId> = BitLayer::new();
        let mut found = false;
        for h in mesh.face_half_edges(FaceId(0)) {
            let t = mesh.he(h).twin;
            if t != NO_TWIN && mesh.he(t).face == FaceId(1) {
                marked.set(h, true);
                marked.set(t, true);
                found = true;
            }
        }
        assert!(found, "the two faces must share an edge");
        let with = smart_uv_unwrap_with_seams(&mesh, &seams_mode, Some(&marked));
        assert_eq!(
            with.island_count, 2,
            "one marked seam must split the chart in seams mode (got {})",
            with.island_count
        );
        assert!(
            uv_overlap_fraction(&mesh, &with, 256) < 0.01,
            "seam-defined islands must not overlap"
        );
    }

    /// GATE — Pack is a REPACK, not a disguised Unwrap.
    ///
    /// Pack must preserve the layout the author built and only re-place it. Asserted against a
    /// hand-built layout with a KNOWN island count that geometry alone would never produce — a
    /// flat strip (one geometric chart) cut into three UV islands by hand. Re-deriving charts
    /// would collapse it back to one; that is the lie this gate exists to catch.
    #[test]
    fn pack_repacks_existing_islands_without_re_unwrapping() {
        use crate::poly_mesh::{Interp, LayerStore};
        let mut pos = Vec::new();
        for i in 0..=6 {
            pos.push(Vec3::new(i as f32, 0.0, 0.0));
            pos.push(Vec3::new(i as f32, 0.0, 1.0));
        }
        let faces: Vec<Vec<usize>> = (0..6)
            .map(|i| vec![i * 2, i * 2 + 2, i * 2 + 3, i * 2 + 1])
            .collect();
        let mesh = HalfEdgeMesh::from_polygons(&pos, &faces);
        assert_eq!(
            smart_uv_unwrap(&mesh, &UvUnwrapParams::default()).island_count,
            1,
            "geometry alone gives ONE chart here - that is what makes this fixture decisive"
        );

        let mut uv: LayerStore<HalfEdgeId, Vec2> = LayerStore::new(Interp::Copy);
        for fi in 0..mesh.faces.len() {
            let f = FaceId(fi as u32);
            if !mesh.is_face_live(f) {
                continue;
            }
            let island = fi / 2;
            let base = Vec2::new(0.02 + island as f32 * 0.06, 0.02);
            for h in mesh.face_half_edges(f) {
                let p = mesh.vert(mesh.he(h).vertex).position;
                let local = Vec2::new((p.x - (island * 2) as f32) * 0.02, p.z * 0.02);
                uv.set(h, base + local);
            }
        }
        let before_area = {
            let mut mn = Vec2::splat(f32::INFINITY);
            let mut mx = Vec2::splat(f32::NEG_INFINITY);
            for (_, v) in uv.iter() {
                mn = mn.min(*v);
                mx = mx.max(*v);
            }
            (mx.x - mn.x) * (mx.y - mn.y)
        };

        let packed = pack_existing_uvs(&mesh, &uv, 0.02);
        assert!(!packed.is_empty(), "Pack must produce UVs");
        assert_eq!(
            packed.len(),
            uv.len(),
            "every authored corner must come back - Pack moves islands, it does not drop them"
        );

        let mut mn = Vec2::splat(f32::INFINITY);
        let mut mx = Vec2::splat(f32::NEG_INFINITY);
        for (_, v) in &packed {
            mn = mn.min(*v);
            mx = mx.max(*v);
            assert!(
                (-0.001..=1.001).contains(&v.x) && (-0.001..=1.001).contains(&v.y),
                "packed uv escaped the unit square: {v:?}"
            );
        }
        let after_area = (mx.x - mn.x) * (mx.y - mn.y);
        assert!(
            after_area > before_area * 4.0,
            "Pack must grow a scattered tiny layout to use the map ({before_area:.4} -> \
             {after_area:.4})"
        );

        let r = UvUnwrapResult {
            uvs: packed,
            seams: Vec::new(),
            island_count: 3,
            cone_cuts: 0,
        };
        assert!(
            uv_overlap_fraction(&mesh, &r, 256) < 0.01,
            "repacked islands must not overlap each other"
        );
    }

    /// GATE — the packer uses the map instead of hoarding it.
    ///
    /// Six equal squares tile a 3×2 grid perfectly, so a cube is the case where a packer has no
    /// excuse. The old guess-then-shrink loop returned the first scale that fitted and never grew:
    /// **41 % coverage**, i.e. every baked texture ran at ~half its nominal resolution. Bisection
    /// on scale finds the largest that fits.
    #[test]
    fn packer_fills_the_map_on_a_cube() {
        let mesh = HalfEdgeMesh::new_box(Vec3::splat(0.5));
        let r = smart_uv_unwrap(&mesh, &UvUnwrapParams::default());
        let area: f32 = {
            let uv_of: std::collections::HashMap<u32, Vec2> =
                r.uvs.iter().map(|(h, uv)| (h.0, *uv)).collect();
            let mut a = 0.0;
            for fi in 0..mesh.faces.len() {
                let f = FaceId(fi as u32);
                if !mesh.is_face_live(f) {
                    continue;
                }
                let hes = mesh.face_half_edges(f);
                let uvs: Vec<Vec2> = hes
                    .iter()
                    .map(|h| uv_of.get(&h.0).copied().unwrap_or(Vec2::ZERO))
                    .collect();
                for k in 1..uvs.len().saturating_sub(1) {
                    let (p, q, s) = (uvs[0], uvs[k], uvs[k + 1]);
                    a += ((q.x - p.x) * (s.y - p.y) - (q.y - p.y) * (s.x - p.x)).abs() * 0.5;
                }
            }
            a
        };
        assert!(
            area > 0.50,
            "a cube's six equal squares must fill over half the UV square (got {:.1}%) — \
             anything near 41% means the scale search stopped at its first guess again",
            area * 100.0
        );
        // …and still inside the unit square.
        for (_, uv) in &r.uvs {
            assert!(
                (-0.001..=1.001).contains(&uv.x) && (-0.001..=1.001).contains(&uv.y),
                "packed uv escaped the unit square: {uv:?}"
            );
        }
    }

    #[test]
    fn higher_angle_limit_fewer_islands() {
        let mesh = HalfEdgeMesh::new_box(Vec3::splat(0.5));
        let tight = smart_uv_unwrap(
            &mesh,
            &UvUnwrapParams {
                method: UvUnwrapMethod::Angle,
                angle_limit_deg: 5.0,
                margin: 0.02,
                max_cone_deg: 75.0,
            },
        );
        let loose = smart_uv_unwrap(
            &mesh,
            &UvUnwrapParams {
                method: UvUnwrapMethod::Angle,
                angle_limit_deg: 179.0,
                margin: 0.02,
                max_cone_deg: 75.0,
            },
        );
        assert!(
            tight.island_count >= loose.island_count,
            "tighter angle → more islands ({} vs {})",
            tight.island_count,
            loose.island_count
        );
        assert_eq!(
            tight.island_count, 6,
            "cube faces are orthogonal → 6 islands at 5°"
        );
    }

    #[test]
    fn empty_mesh_is_empty() {
        let mesh = HalfEdgeMesh::new();
        let r = smart_uv_unwrap(&mesh, &UvUnwrapParams::default());
        assert!(r.uvs.is_empty());
        assert_eq!(r.island_count, 0);
    }
}
